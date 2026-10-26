//! Install VS Code theme data from local files or gallery links.
//! Extension code and build scripts never run.
use crate::vscode_theme::{parse_jsonc, slug};
use serde_json::Value;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub(crate) const MAX_PACKAGE_SIZE: u64 = 100 * 1024 * 1024;
const MAX_EXPANDED: u64 = 256 * 1024 * 1024;

pub(crate) fn install(
    source: &str,
    root: &Path,
) -> Result<Vec<crate::app_theme::ThemeFile>, String> {
    let source = source.trim().trim_matches('"');
    let removed = crate::theme_catalog::removed(root)?;
    let themes = if source.contains("://") {
        install_from_link(source, root, crate::theme_download::download)
    } else {
        install_local(Path::new(source), root)
    }?;
    let restored = themes.iter().filter(|theme| removed.contains(&theme.config_key))
        .map(|theme| theme.config_key.clone()).collect();
    crate::theme_catalog::restore(root, &restored)?;
    Ok(themes)
}

fn install_from_link(
    source: &str,
    root: &Path,
    fetch: impl FnOnce(&str, &Path) -> Result<(), String>,
) -> Result<Vec<crate::app_theme::ThemeFile>, String> {
    let url = crate::theme_download::package_url(source)?;
    let temporary_root = root
        .parent()
        .ok_or("Invalid theme installation directory")?
        .join("tmp");
    std::fs::create_dir_all(&temporary_root).map_err(err)?;
    let temporary = tempfile::Builder::new()
        .prefix("theme-install-")
        .tempdir_in(&temporary_root)
        .map_err(err)?;
    let package = temporary.path().join("theme.vsix");
    let result = fetch(&url, &package).and_then(|()| install_local(&package, root));
    // Close explicitly to report cleanup failures instead of silently leaving a
    // downloaded or partially downloaded VSIX behind. Local sources are untouched.
    let cleanup = temporary.close();
    match (result, cleanup) {
        (result, Ok(())) => result,
        (Ok(_), Err(error)) => Err(format!(
            "Theme installed, but temporary download cleanup failed: {error}"
        )),
        (Err(error), Err(cleanup)) => Err(format!(
            "{error}; temporary download cleanup failed: {cleanup}"
        )),
    }
}

fn install_local(local: &Path, root: &Path) -> Result<Vec<crate::app_theme::ThemeFile>, String> {
    if local.as_os_str().is_empty() || !(local.is_file() || local.is_dir()) {
        return Err(
            "Use a vscodethemes.com theme link or an existing local VSIX, ZIP, extension directory, or JSON/JSONC theme file."
                .into(),
        );
    }
    std::fs::create_dir_all(root).map_err(err)?;
    let staging = tempfile::Builder::new()
        .prefix(".install-")
        .tempdir_in(root)
        .map_err(err)?;
    let unpacked = staging.path().join("unpacked");
    let package_root = if local.is_dir() {
        local.to_path_buf()
    } else if local
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json") || e.eq_ignore_ascii_case("jsonc"))
    {
        return install_single(local, root, staging.path());
    } else {
        let file = std::fs::File::open(local).map_err(err)?;
        if file.metadata().map_err(err)?.len() > MAX_PACKAGE_SIZE {
            return Err("Theme package exceeds 100 MiB".into());
        }
        extract(file, &unpacked)?;
        find_package(&unpacked)?
    };
    install_package(&package_root, root, staging.path())
}

fn err(error: impl std::fmt::Display) -> String {
    error.to_string()
}
fn read_json(path: &Path) -> Result<Value, String> {
    parse_jsonc(&std::fs::read_to_string(path).map_err(err)?)
}
fn contributions(package: &Value) -> Result<&Vec<Value>, String> {
    package
        .pointer("/contributes/themes")
        .and_then(Value::as_array)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| "Extension has no color theme contributions".into())
}
fn manifest_id<'a>(package: &'a Value, key: &str) -> Result<&'a str, String> {
    package
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        })
        .ok_or_else(|| format!("Invalid extension {key}"))
}
fn extract<R: Read + std::io::Seek>(reader: R, root: &Path) -> Result<(), String> {
    let mut archive = zip::ZipArchive::new(reader).map_err(err)?;
    if archive.len() > 20_000 {
        return Err("Too many files in theme package".into());
    }
    let mut remaining = MAX_EXPANDED;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(err)?;
        let relative = file.enclosed_name().ok_or("Unsafe path in theme package")?;
        if file.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
            return Err("Symlinks are not supported in theme packages".into());
        }
        // Reject Windows drive/stream paths on every platform.
        if relative.to_string_lossy().contains(':')
            || relative.components().any(|c| {
                matches!(
                    c,
                    Component::Prefix(_) | Component::RootDir | Component::ParentDir
                )
            })
        {
            return Err("Unsafe path in theme package".into());
        }
        let output = root.join(relative);
        if file.is_dir() {
            std::fs::create_dir_all(output).map_err(err)?;
            continue;
        }
        remaining = remaining
            .checked_sub(file.size())
            .ok_or("Theme package exceeds 256 MiB expanded")?;
        std::fs::create_dir_all(output.parent().unwrap()).map_err(err)?;
        let mut writer = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(output)
            .map_err(err)?;
        let expected = file.size();
        let copied =
            std::io::copy(&mut (&mut file).take(expected + 1), &mut writer).map_err(err)?;
        if copied != expected {
            return Err("Invalid theme package file size".into());
        }
    }
    Ok(())
}
fn find_package(root: &Path) -> Result<PathBuf, String> {
    for candidate in [root.to_path_buf(), root.join("extension")] {
        if candidate.join("package.json").is_file() {
            return Ok(candidate);
        }
    }
    for entry in std::fs::read_dir(root).map_err(err)? {
        let path = entry.map_err(err)?.path();
        if path.join("package.json").is_file() {
            return Ok(path);
        }
    }
    Err("No VS Code extension package.json found".into())
}

fn install_package(
    source: &Path,
    root: &Path,
    staging: &Path,
) -> Result<Vec<crate::app_theme::ThemeFile>, String> {
    let package = read_json(&source.join("package.json"))?;
    let publisher = manifest_id(&package, "publisher")?;
    let name = manifest_id(&package, "name")?;
    let items = contributions(&package)?;
    let prepared = staging.join("prepared");
    let extension = prepared.join(format!("{publisher}.{name}"));
    std::fs::create_dir_all(&extension).map_err(err)?;
    let safe_manifest = serde_json::json!({ "name": name, "publisher": publisher, "version": package.get("version"), "contributes": {"themes": items} });
    std::fs::write(
        extension.join("package.json"),
        serde_json::to_vec_pretty(&safe_manifest).map_err(err)?,
    )
    .map_err(err)?;
    let source = source.canonicalize().map_err(err)?;
    let mut copied = std::collections::HashSet::new();
    for item in items {
        let path = item
            .get("path")
            .and_then(Value::as_str)
            .ok_or("Theme contribution has no path")?;
        copy_theme_data(&source, &source.join(path), &extension, &mut copied, 0)?;
    }
    for name in ["package.nls.json", "LICENSE", "LICENSE.txt", "LICENSE.md"] {
        if source.join(name).is_file() {
            copy_theme_data(&source, &source.join(name), &extension, &mut copied, 0)?;
        }
    }
    let themes = crate::vscode_theme::load_extension_themes(&prepared);
    if themes.len() != items.len() {
        return Err(
            "One or more contributed themes could not be loaded; installation cancelled".into(),
        );
    }
    publish(
        &extension,
        &root.join(format!("{publisher}.{name}")),
        staging,
    )?;
    Ok(themes)
}

fn copy_theme_data(
    root: &Path,
    path: &Path,
    destination: &Path,
    copied: &mut std::collections::HashSet<PathBuf>,
    depth: usize,
) -> Result<(), String> {
    if depth > 32 {
        return Err("Theme dependency nesting is too deep".into());
    }
    let path = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let relative = path
        .strip_prefix(root)
        .map_err(|_| "Theme reference escapes extension directory")?;
    if !copied.insert(path.clone()) {
        return Ok(());
    }
    let data = std::fs::read(&path).map_err(err)?;
    if data.len() > 8 * 1024 * 1024 {
        return Err("Theme data file exceeds 8 MiB".into());
    }
    let output = destination.join(relative);
    std::fs::create_dir_all(output.parent().unwrap()).map_err(err)?;
    std::fs::write(output, &data).map_err(err)?;
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json") || e.eq_ignore_ascii_case("jsonc"))
    {
        let value = parse_jsonc(std::str::from_utf8(&data).map_err(err)?)?;
        for key in ["include", "tokenColors"] {
            if let Some(reference) = value.get(key).and_then(Value::as_str) {
                copy_theme_data(
                    root,
                    &path.parent().unwrap().join(reference),
                    destination,
                    copied,
                    depth + 1,
                )?;
            }
        }
    }
    Ok(())
}
fn publish(prepared: &Path, destination: &Path, staging: &Path) -> Result<(), String> {
    let backup = staging.join("previous");
    let existed = destination.exists();
    if existed {
        std::fs::rename(destination, &backup).map_err(err)?;
    }
    if let Err(error) = std::fs::rename(prepared, destination) {
        if existed {
            std::fs::rename(&backup, destination).map_err(|restore| format!("Install failed: {error}; restore failed: {restore}; previous installation at {}", backup.display()))?;
        }
        return Err(err(error));
    }
    Ok(())
}
fn install_single(
    path: &Path,
    root: &Path,
    staging: &Path,
) -> Result<Vec<crate::app_theme::ThemeFile>, String> {
    let path = path.canonicalize().map_err(err)?;
    let theme = crate::vscode_theme::load_theme_file(&path, None, None)?;
    let source = staging.join("single");
    std::fs::create_dir_all(&source).map_err(err)?;
    copy_theme_data(
        path.parent().unwrap(),
        &path,
        &source,
        &mut Default::default(),
        0,
    )?;
    let manifest = serde_json::json!({"name": slug(&theme.name), "publisher": "local", "version": "1", "contributes": {"themes": [{"label": theme.name, "path": path.file_name().unwrap().to_string_lossy()}]}});
    std::fs::write(
        source.join("package.json"),
        serde_json::to_vec(&manifest).map_err(err)?,
    )
    .map_err(err)?;
    install_package(&source, root, staging)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn vsix(path: &Path, files: &[(&str, &str)]) {
        let mut writer = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        for (name, data) in files {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(data.as_bytes()).unwrap();
        }
        writer.finish().unwrap();
    }

    #[test]
    fn vsix_installs_all_variants_and_preserves_the_previous_install_on_failure() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("theme.vsix");
        let root = temp.path().join("installed");
        vsix(
            &archive,
            &[
                (
                    "extension/package.json",
                    r#"{"publisher":"example","name":"pack","contributes":{"themes":[{"label":"Dark","path":"themes/dark.json","uiTheme":"vs-dark"},{"label":"Light","path":"themes/light.json","uiTheme":"vs"}]}}"#,
                ),
                (
                    "extension/themes/dark.json",
                    r##"{"colors":{"editor.background":"#102030"},"tokenColors":[{"scope":"keyword","settings":{"foreground":"#abcdef"}}]}"##,
                ),
                (
                    "extension/themes/light.json",
                    r##"{"include":"dark.json","colors":{"editor.background":"#ffffff"}}"##,
                ),
                ("extension/extension.js", "doNotExecute()"),
                ("extension/LICENSE", "Example theme license"),
            ],
        );
        let themes = install(archive.to_str().unwrap(), &root).unwrap();
        assert_eq!(themes.len(), 2);
        assert!(root.join("example.pack/LICENSE").exists());
        assert!(!root.join("example.pack/extension.js").exists());
        assert_eq!(crate::vscode_theme::load_extension_themes(&root).len(), 2);
        vsix(&archive, &[("../escape.json", "{}")]);
        assert!(install(archive.to_str().unwrap(), &root).is_err());
        assert_eq!(crate::vscode_theme::load_extension_themes(&root).len(), 2);
        assert!(!temp.path().join("escape.json").exists());
    }
    #[test]
    fn rejects_unsupported_urls_before_creating_installation_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("installed");
        for source in [
            "https://vscodethemes.com/",
            "https://vscodethemes.com/e/not-an-extension/dark",
            "https://github.com/example/theme",
            "https://github.com/example/theme/releases/download/v1/theme.vsix",
            "https://marketplace.visualstudio.com/_apis/public/gallery/publishers/example/vsextensions/theme/latest/vspackage",
            "https://example.com/theme.vsix",
            "http://example.com/theme.json",
            "HTTPS://example.com/theme.vsix",
            "file:///theme.json",
            "  \"https://example.com/theme.vsix\"  ",
        ] {
            let error = install(source, &root).err().expect("URLs must be rejected");
            assert!(error.contains("Use a theme link"), "{source}: {error}");
            assert!(!root.exists());
        }
    }

    #[test]
    fn gallery_install_uses_a_temporary_vsix_and_cleans_it_up_on_all_outcomes() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join(".vtcode/theme-extensions");
        let temporary_root = temp.path().join(".vtcode/tmp");
        let link = "https://vscodethemes.com/e/example.pack/dark?language=py";
        let themes = install_from_link(link, &root, |url, path| {
            assert_eq!(url, "https://marketplace.visualstudio.com/_apis/public/gallery/publishers/example/vsextensions/pack/latest/vspackage");
            assert!(path.starts_with(&temporary_root));
            vsix(path, &[
                ("extension/package.json", r#"{"publisher":"example","name":"pack","contributes":{"themes":[{"label":"Dark","path":"theme.json"}]}}"#),
                ("extension/theme.json", r##"{"colors":{"editor.background":"#102030"}}"##),
            ]);
            Ok(())
        }).unwrap();
        assert_eq!(themes.len(), 1);
        assert!(root.join("example.pack/theme.json").is_file());
        assert_eq!(std::fs::read_dir(&temporary_root).unwrap().count(), 0);
        let original = std::fs::read(root.join("example.pack/theme.json")).unwrap();
        for download_failed in [true, false] {
            let result = install_from_link(link, &root, |_, path| {
                std::fs::write(path, "not a complete VSIX").unwrap();
                if download_failed {
                    Err("Connection lost".into())
                } else {
                    Ok(())
                }
            });
            assert!(result.is_err());
            assert_eq!(std::fs::read_dir(&temporary_root).unwrap().count(), 0);
            assert_eq!(
                std::fs::read(root.join("example.pack/theme.json")).unwrap(),
                original
            );
            assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
        }
    }

    #[test]
    #[ignore = "requires access to the public Marketplace"]
    fn gallery_install_live_onedark_package() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join(".vtcode/theme-extensions");
        let themes = install(
            "https://vscodethemes.com/e/akamud.vscode-theme-onedark/atom-one-dark?language=py",
            &root,
        )
        .unwrap();
        assert!(themes.iter().any(|theme| theme.name.contains("One Dark")));
        assert!(
            root.join("akamud.vscode-theme-onedark/package.json")
                .is_file()
        );
        assert_eq!(
            std::fs::read_dir(temp.path().join(".vtcode/tmp"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            crate::vscode_theme::load_extension_themes(&root).len(),
            themes.len()
        );
    }

    #[test]
    fn installs_quoted_local_json_and_jsonc_files_with_dependencies() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("local themes");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(
            source.join("base.json"),
            r##"{"colors":{"editor.background":"#102030"}}"##,
        )
        .unwrap();
        for extension in ["json", "JSONC"] {
            let path = source.join(format!("custom theme.{extension}"));
            std::fs::write(
                &path,
                r##"{"name":"Local Theme","include":"base.json","colors":{"editor.foreground":"#abcdef"},}"##,
            )
            .unwrap();
            let root = temp.path().join(format!("installed-{extension}"));
            let themes = install(&format!("  \"{}\"  ", path.display()), &root).unwrap();
            assert_eq!(themes.len(), 1);
            assert_eq!(themes[0].name, "Local Theme");
            assert!(root.join("local.local-theme/base.json").is_file());
            assert_eq!(crate::vscode_theme::load_extension_themes(&root).len(), 1);
        }
    }

    #[test]
    fn missing_local_theme_does_not_create_installation_files() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("installed");
        let missing = temp.path().join("missing.vsix");
        for source in ["", "   ", missing.to_str().unwrap()] {
            let error = install(source, &root)
                .err()
                .expect("Missing files must fail");
            assert!(error.contains("existing local"));
            assert!(!root.exists());
        }
    }

    #[test]
    fn installs_includes_and_updates_without_executable_code() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("package.json"), r##"{"publisher":"test","name":"theme","main":"evil.js","contributes":{"themes":[{"label":"Example","path":"theme.json","uiTheme":"vs-dark"}]}}"##).unwrap();
        std::fs::write(
            source.join("theme.json"),
            r##"{"include":"base.json","colors":{"tab.activeForeground":"#123456"}}"##,
        )
        .unwrap();
        std::fs::write(
            source.join("base.json"),
            r##"{"colors":{"editor.background":"#102030"}}"##,
        )
        .unwrap();
        std::fs::write(source.join("evil.js"), "throw new Error()").unwrap();
        let root = temp.path().join("installed");
        for _ in 0..2 {
            assert_eq!(install(source.to_str().unwrap(), &root).unwrap().len(), 1);
        }
        assert!(root.join("test.theme/base.json").is_file());
        assert!(!root.join("test.theme/evil.js").exists());
        std::fs::write(
            source.join("theme.json"),
            r##"{"include":"../outside.json"}"##,
        )
        .unwrap();
        std::fs::write(temp.path().join("outside.json"), r##"{"colors":{}}"##).unwrap();
        assert!(install(source.to_str().unwrap(), &root).is_err());
        assert_eq!(crate::vscode_theme::load_extension_themes(&root).len(), 1);
    }
}
