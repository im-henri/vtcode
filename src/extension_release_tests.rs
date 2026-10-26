use super::*;
use crate::extension_versions::{ReleaseVersion, parse_releases};
use sha2::{Digest, Sha256};
use std::io::Cursor;

const LINK: &str = "https://github.com/owner/example";
const WASM: &[u8] = b"\0asm\x01\0\0\0\x01\x04\x01\x60\0\0\x03\x02\x01\0\x07\x13\x01\x0fvtcode_activate\0\0\x0a\x04\x01\x02\0\x0b";

fn archive(version: &str, extra: &[(&str, &[u8])]) -> Vec<u8> {
    let manifest = serde_json::to_vec(
        &serde_json::json!({"id":"example", "name":"Example", "version": version,
        "repository": LINK, "apiVersion":"2", "main":"example.wasm"}),
    )
    .unwrap();
    archive_with_manifest(&manifest, WASM, extra)
}

fn archive_with_manifest(manifest: &[u8], wasm: &[u8], extra: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for (name, bytes) in [("extension.json", manifest), ("example.wasm", wasm)]
        .into_iter()
        .chain(extra.iter().copied())
    {
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn release(tag: &str, bytes: &[u8]) -> ReleaseVersion {
    ReleaseVersion {
        name: tag.into(),
        release_id: 10,
        asset_id: 20,
        size: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(bytes)),
        prerelease: false,
    }
}

fn install(root: &Path, tag: &str, bytes: &[u8]) -> Result<Vec<InstalledExtension>, String> {
    let source = ExtensionSource::parse(LINK).unwrap();
    let version = release(tag, bytes);
    install_with_download(&source, &version, root, |path| {
        save_download(Cursor::new(bytes), path, &version)
    })
}

fn metadata(tag: &str, asset: bool) -> serde_json::Value {
    serde_json::json!({"id":10, "tag_name":tag, "draft":false, "prerelease":false,
        "assets": if asset { vec![serde_json::json!({"id":20,"name":"vtcode-extension.zip","state":"uploaded", "size":123,
            "digest":format!("sha256:{}", "a".repeat(64))})] } else { Vec::new() }})
}

#[test]
fn github_sources_accept_repository_release_and_tag_links_but_never_branches() {
    for link in [
        LINK.to_string(),
        format!("{LINK}.git"),
        "git@github.com:owner/example.git".into(),
    ] {
        assert_eq!(ExtensionSource::parse(&link).unwrap().repository, LINK);
    }
    for link in [
        format!("{LINK}/releases/tag/v1.0.0"),
        format!("{LINK}#tag=v1.0.0"),
    ] {
        assert_eq!(
            ExtensionSource::parse(&link).unwrap().reference.as_deref(),
            Some("v1.0.0")
        );
    }
    for link in [
        "https://github.com/owner/example/tree/main",
        "https://github.com/owner/example#ref=main",
        "https://github.com/owner/example#path=hex",
        "https://github.com/owner/example#tag=main",
        "https://github.com/owner/example?token=secret",
        "https://other.example/owner/example",
        "http://github.com/owner/example",
        "https://password@github.com/owner/example",
        "https://github.com/owner/example/releases/tag/../main",
    ] {
        assert!(ExtensionSource::parse(link).is_err(), "{link}");
    }
}

#[test]
fn only_published_semantic_tags_with_verified_packages_are_installable() {
    let mut draft = metadata("v9.0.0", true);
    draft["draft"] = true.into();
    let list = vec![
        metadata("v1.9.0", true),
        metadata("v1.10.0", true),
        metadata("v2.0.0-rc.1", true),
        metadata("main", true),
        metadata("v3.0.0", false),
        draft,
    ];
    let catalog = parse_releases(LINK, &serde_json::to_vec(&list).unwrap()).unwrap();
    assert_eq!(
        catalog
            .versions
            .iter()
            .map(|v| v.name.as_str())
            .collect::<Vec<_>>(),
        ["v1.10.0", "v1.9.0", "v2.0.0-rc.1"]
    );
    assert_eq!(catalog.latest_stable().unwrap().name, "v1.10.0");
    for list in [
        vec![],
        vec![metadata("main", true)],
        vec![metadata("v1.0.0", false)],
    ] {
        assert!(
            parse_releases(LINK, &serde_json::to_vec(&list).unwrap())
                .unwrap_err()
                .contains("no installable")
        );
    }
    for value in [serde_json::Value::Null, "sha256:bad".into()] {
        let mut invalid = metadata("v1.0.0", true);
        invalid["assets"][0]["digest"] = value;
        assert!(parse_releases(LINK, &serde_json::to_vec(&[invalid]).unwrap()).is_err());
    }
    let bytes = serde_json::to_vec(&list).unwrap();
    assert!(parse_releases(&format!("{LINK}#tag=v7.0.0"), &bytes).is_err());
    assert!(parse_releases(LINK, b"not json").is_err());
}

#[test]
fn runtime_only_packages_install_upgrade_and_downgrade_without_git_or_source_copies() {
    let root = tempfile::tempdir().unwrap();
    let first = archive(
        "1.0.0",
        &[("README.md", b"first"), ("assets/data.json", b"{}")],
    );
    let installed = install(root.path(), "v1.0.0", &first).unwrap();
    assert_eq!(installed[0].tag, "v1.0.0");
    let target = root.path().join("example");
    assert!(
        !target.join(".git").exists()
            && !target.join("src").exists()
            && !target.join("Cargo.toml").exists()
    );
    assert_eq!(
        std::fs::read(target.join("assets/data.json")).unwrap(),
        b"{}"
    );
    let second = archive("1.1.0", &[("README.md", b"second")]);
    install(root.path(), "v1.1.0", &second).unwrap();
    assert_eq!(std::fs::read(target.join("README.md")).unwrap(), b"second");
    assert!(
        !target.join("assets/data.json").exists(),
        "upgrades replace the package; they do not leave obsolete files"
    );
    install(root.path(), "v1.0.0", &first).unwrap();
    assert_eq!(std::fs::read(target.join("README.md")).unwrap(), b"first");
    let info: InstalledExtension =
        serde_json::from_slice(&std::fs::read(target.join(".vtcode-source.json")).unwrap())
            .unwrap();
    assert_eq!(info.sha256, installed[0].sha256);
    assert!(release("v1.0.0", &first).is_installed(&[info]));
}

#[test]
fn changed_tags_invalid_packages_and_failed_downloads_preserve_installed_files() {
    let root = tempfile::tempdir().unwrap();
    let original = archive("1.0.0", &[("README.md", b"keep this")]);
    install(root.path(), "v1.0.0", &original).unwrap();
    let target = root.path().join("example");
    let saved = std::fs::read(target.join(".vtcode-source.json")).unwrap();
    for (tag, package) in [
        ("v1.0.0", archive("1.0.0", &[("README.md", b"reused tag")])),
        ("v2.0.0", archive("1.0.0", &[])),
        ("v2.0.0", b"broken zip".to_vec()),
        ("v2.0.0", archive("2.0.0", &[("../escape", b"bad")])),
        ("v2.0.0", archive("2.0.0", &[(".git/config", b"bad")])),
        (
            "v2.0.0",
            archive("2.0.0", &[(".vtcode-source.json", b"spoofed")]),
        ),
        (
            "v2.0.0",
            archive("2.0.0", &[("other/extension.json", b"{}")]),
        ),
        (
            "v2.0.0",
            archive("2.0.0", &[("README", b"one"), ("readme", b"two")]),
        ),
    ] {
        assert!(
            install(root.path(), tag, &package).is_err(),
            "accepted invalid package for {tag}"
        );
        assert_eq!(
            std::fs::read(target.join("README.md")).unwrap(),
            b"keep this"
        );
        assert_eq!(
            std::fs::read(target.join(".vtcode-source.json")).unwrap(),
            saved
        );
    }
    let source = ExtensionSource::parse(LINK).unwrap();
    let version = release("v2.0.0", &original);
    for bytes in [
        &original[..original.len() - 1],
        b"wrong checksum".as_slice(),
    ] {
        assert!(
            install_with_download(&source, &version, root.path(), |path| save_download(
                Cursor::new(bytes),
                path,
                &version
            ))
            .is_err()
        );
    }
    assert!(!root.path().join("escape").exists());
    assert!(std::fs::read_dir(root.path()).unwrap().all(|entry| {
        let entry = entry.unwrap();
        !entry.file_type().unwrap().is_dir()
            || !entry.file_name().to_string_lossy().starts_with(".install-")
    }));
}

#[test]
fn links_in_zip_files_and_installation_lock_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .add_symlink(
            "link",
            "../outside",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
    let bytes = writer.finish().unwrap().into_inner();
    assert!(
        install(root.path(), "v1.0.0", &bytes)
            .unwrap_err()
            .contains("links")
    );
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.path().join(".install-lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let package = archive("1.0.0", &[]);
    assert!(
        install(root.path(), "v1.0.0", &package)
            .unwrap_err()
            .contains("Another extension installation")
    );
    lock.unlock().unwrap();
    install(root.path(), "v1.0.0", &package).unwrap();
}

#[test]
fn invalid_manifest_runtime_and_repository_identity_preserve_the_installed_release() {
    let root = tempfile::tempdir().unwrap();
    install(
        root.path(),
        "v1.0.0",
        &archive("1.0.0", &[("README.md", b"original")]),
    )
    .unwrap();
    let base = serde_json::json!({"id":"example", "name":"Example", "version":"2.0.0",
        "repository":LINK, "apiVersion":"2", "main":"example.wasm"});
    for (field, value) in [
        ("id", "../escape"),
        ("main", "../escape"),
        ("main", "missing.wasm"),
        ("apiVersion", "999"),
        ("repository", "https://github.com/another/extension"),
    ] {
        let mut manifest = base.clone();
        manifest[field] = value.into();
        let package = archive_with_manifest(&serde_json::to_vec(&manifest).unwrap(), WASM, &[]);
        assert!(
            install(root.path(), "v2.0.0", &package).is_err(),
            "accepted invalid {field}"
        );
    }
    for wasm in [
        b"invalid wasm".as_slice(),
        b"(module (func (export \"vtcode_activate\")))".as_slice(),
    ] {
        let package = archive_with_manifest(&serde_json::to_vec(&base).unwrap(), wasm, &[]);
        assert!(install(root.path(), "v2.0.0", &package).is_err());
    }
    let large = archive_with_manifest(&vec![b' '; 1024 * 1024 + 1], WASM, &[]);
    assert!(
        install(root.path(), "v2.0.0", &large)
            .unwrap_err()
            .contains("1 MiB")
    );
    assert_eq!(
        std::fs::read(root.path().join("example/README.md")).unwrap(),
        b"original"
    );
}

#[test]
#[ignore = "Build the three standalone extension dist/vtcode-extension.zip packages first"]
fn locally_built_extension_release_packages_validate_before_publication() {
    for project in [
        "vtcode-ext-hex-editor",
        "vtcode-ext-tabular-workbench",
        "vtcode-ext-waveform-viewer",
    ] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join("vtcode_extensions")
            .join(project)
            .join("dist/vtcode-extension.zip");
        let bytes = std::fs::read(&path).unwrap();
        let source =
            ExtensionSource::parse(&format!("https://github.com/im-henri/{project}")).unwrap();
        let version = release("v0.2.0", &bytes);
        let root = tempfile::tempdir().unwrap();
        let info = install_with_download(&source, &version, root.path(), |path| {
            save_download(Cursor::new(&bytes), path, &version)
        })
        .unwrap();
        let target = root.path().join(&info[0].id);
        assert_eq!(info[0].version, "0.2.0");
        for excluded in [".git", "src", "vendor", "Cargo.toml", "Cargo.lock"] {
            assert!(!target.join(excluded).exists());
        }
        eprintln!(
            "Validated {} {} ({} compressed bytes)",
            info[0].name,
            info[0].version,
            bytes.len()
        );
    }
}

#[test]
#[ignore = "Public GitHub: run after publishing the three tagged runtime packages"]
fn published_extension_release_packages_install_from_real_github_without_repository_files() {
    for project in [
        "vtcode-ext-hex-editor",
        "vtcode-ext-tabular-workbench",
        "vtcode-ext-waveform-viewer",
    ] {
        let root = tempfile::tempdir().unwrap();
        let link = format!("https://github.com/im-henri/{project}#tag=v0.2.0");
        let catalog = crate::extension_versions::discover(&link, root.path()).unwrap();
        let info = install_version(
            &catalog.source,
            &catalog.versions[catalog.preferred_index()],
            root.path(),
        )
        .unwrap();
        let installed = root.path().join(&info[0].id);
        assert_eq!(info[0].version, "0.2.0");
        for path in [
            ".git",
            "src",
            "vendor",
            "Cargo.toml",
            "Cargo.lock",
            "package.py",
        ] {
            assert!(!installed.join(path).exists(), "{project}: {path}");
        }
        assert!(installed.join("extension.json").is_file());
        eprintln!(
            "Installed {} {} from {} bytes",
            info[0].name, info[0].version, catalog.versions[0].size
        );
    }
}
