//! Install only the runtime files in verified, tagged GitHub release packages.
pub(crate) use crate::extension_release_source::ExtensionSource;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Component, Path};
pub(crate) const MAX_PACKAGE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_WASM_BYTES: u64 = 32 * 1024 * 1024;

pub(crate) fn validate_relative(path: &Path) -> Result<(), String> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || path.to_string_lossy().contains(['\\', ':'])
        || path.to_string_lossy().chars().any(char::is_control)
    {
        return Err("Extension package paths must stay inside their directory".into());
    }
    Ok(())
}

pub(crate) fn validate_manifest(
    manifest: &crate::extension_manifest::ExtensionManifest,
) -> Result<(), String> {
    let id = manifest.id.as_str();
    if id.is_empty()
        || id.len() > 100
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        || !id.as_bytes()[0].is_ascii_alphanumeric()
        || id.ends_with('.')
        || matches!(
            id.split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_uppercase()
                .as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        )
    {
        return Err("Extension id must be a portable name containing letters, numbers, dots, dashes or underscores".into());
    }
    if manifest.name.trim().is_empty() || manifest.version.trim().is_empty() {
        return Err("Extension name and version are required".into());
    }
    let source = ExtensionSource::parse(&manifest.repository).map_err(|_| {
        "Every extension must declare a GitHub `repository` containing its source code".to_string()
    })?;
    if source.reference.is_some() {
        return Err("The manifest's repository must link to the GitHub repository root".into());
    }
    if !manifest.main.is_empty() {
        validate_relative(Path::new(&manifest.main))?;
    }
    if manifest.main.is_empty() && manifest.lsp.is_none() {
        return Err("The extension must declare a Wasm main or an LSP configuration".into());
    }
    if let Some(bundle) = &manifest.bundle {
        for platform in bundle.platforms.values() {
            for path in [&platform.command, &platform.node_command]
                .into_iter()
                .flatten()
            {
                validate_relative(Path::new(path))?;
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstalledExtension {
    pub id: String,
    pub name: String,
    pub version: String,
    pub repository: String,
    pub tag: String,
    pub asset_id: u64,
    pub sha256: String,
}

struct InstallLock(std::fs::File);
impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub(crate) fn install_version(
    source: &ExtensionSource,
    version: &crate::extension_versions::ReleaseVersion,
    root: &Path,
) -> Result<Vec<InstalledExtension>, String> {
    install_with_download(source, version, root, |path| {
        let agent = crate::extension_release_source::GitHubClient::new();
        let mut response = agent.get(
            &source.api(&format!("releases/assets/{}", version.asset_id)),
            "application/octet-stream",
        )?;
        save_download(response.body_mut().as_reader(), path, version)
    })
}

fn save_download(
    mut reader: impl Read,
    path: &Path,
    version: &crate::extension_versions::ReleaseVersion,
) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let mut output = std::fs::File::create(path).map_err(|error| error.to_string())?;
    let mut hasher = Sha256::new();
    let mut bytes = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = reader
            .read(&mut bytes)
            .map_err(|error| format!("Extension download interrupted: {error}"))?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or("Package size overflow")?;
        if total > version.size || total > MAX_PACKAGE_BYTES {
            return Err("The downloaded package exceeds the selected release's size; nothing was installed.".into());
        }
        hasher.update(&bytes[..read]);
        output
            .write_all(&bytes[..read])
            .map_err(|error| error.to_string())?;
    }
    if total != version.size || format!("{:x}", hasher.finalize()) != version.sha256 {
        return Err("The package size or SHA-256 checksum does not match the selected release. Refresh the release list; the installed extension was preserved.".into());
    }
    output.flush().map_err(|error| error.to_string())
}

fn install_with_download(
    source: &ExtensionSource,
    version: &crate::extension_versions::ReleaseVersion,
    root: &Path,
    download: impl FnOnce(&Path) -> Result<(), String>,
) -> Result<Vec<InstalledExtension>, String> {
    if version.semantic_version().is_none()
        || version.asset_id == 0
        || version.size == 0
        || version.size > MAX_PACKAGE_BYTES
        || version.sha256.len() != 64
        || !version.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    {
        return Err("Invalid release package selection. Refresh the release list.".into());
    }
    std::fs::create_dir_all(root)
        .map_err(|error| format!("Cannot create the extensions folder: {error}"))?;
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root.join(".install-lock"))
        .map_err(|error| error.to_string())?;
    lock_file.try_lock().map_err(
        |_| "Another extension installation is running. Wait for it to finish, then retry.",
    )?;
    let _lock = InstallLock(lock_file);
    let staging = tempfile::Builder::new()
        .prefix(".install-")
        .tempdir_in(root)
        .map_err(|error| error.to_string())?;
    let archive = staging.path().join("release.zip");
    download(&archive)?;
    let package = staging.path().join("package");
    std::fs::create_dir(&package).map_err(|error| error.to_string())?;
    extract_package(&archive, &package)?;
    let json = read_small_file(&package.join("extension.json"))
        .map_err(|_| "The release package must contain extension.json at its root.")?;
    if json.len() > 1024 * 1024 {
        return Err("Extension manifest exceeds 1 MiB".into());
    }
    let manifest: crate::extension_manifest::ExtensionManifest = serde_json::from_slice(&json)
        .map_err(|error| format!("Invalid extension.json: {error}"))?;
    validate_manifest(&manifest)?;
    if semver::Version::parse(&manifest.version).ok() != version.semantic_version() {
        return Err(format!(
            "Release tag {} contains extension version {}. The author must publish a matching manifest and tag.",
            version.name, manifest.version
        ));
    }
    if !crate::extension_manifest::supports_api(&manifest.api_version) {
        return Err(format!(
            "Unsupported extension API version: {}",
            manifest.api_version
        ));
    }
    if ExtensionSource::parse(&manifest.repository)?.repository_key() != source.repository_key() {
        return Err(
            "The package declares a different source repository; nothing was installed.".into(),
        );
    }
    validate_runtime(&package, &manifest)?;
    let info = InstalledExtension {
        id: manifest.id.clone(),
        name: manifest.name,
        version: manifest.version,
        repository: source.repository.clone(),
        tag: version.name.clone(),
        asset_id: version.asset_id,
        sha256: version.sha256.clone(),
    };
    let target = root.join(&info.id);
    let existed = target.try_exists().map_err(|error| error.to_string())?;
    if existed {
        if std::fs::symlink_metadata(&target)
            .map_err(|error| error.to_string())?
            .file_type()
            .is_symlink()
        {
            return Err("The installed extension directory cannot be a symlink.".into());
        }
        let previous = read_small_file(&target.join(".vtcode-source.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<InstalledExtension>(&bytes).ok());
        let Some(previous) = previous.filter(|previous| {
            ExtensionSource::parse(&previous.repository)
                .is_ok_and(|previous| previous.repository_key() == source.repository_key())
        }) else {
            return Err(format!(
                "Extension id {} is already used by another or unmanaged installation. Remove that installation before installing this release.",
                info.id
            ));
        };
        if previous.tag == info.tag && previous.sha256 != info.sha256 {
            return Err("This release tag's package changed. The author must publish a new version; the installed extension was preserved.".into());
        }
    }
    std::fs::write(
        package.join(".vtcode-source.json"),
        serde_json::to_vec_pretty(&info).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let backup = staging.path().join("previous");
    if existed {
        std::fs::rename(&target, &backup).map_err(|error| {
            format!(
                "Could not move the installed extension; close files using it and retry: {error}"
            )
        })?;
    }
    if let Err(error) = std::fs::rename(&package, &target) {
        if existed && std::fs::rename(&backup, &target).is_err() {
            let recovery = staging.keep();
            return Err(format!(
                "Could not install extension: {error}. Recovery copies remain at {}",
                recovery.display()
            ));
        }
        return Err(format!("Could not install extension: {error}"));
    }
    Ok(vec![info])
}

fn read_small_file(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Extension metadata exceeds 1 MiB",
        ));
    }
    Ok(bytes)
}

fn extract_package(archive: &Path, package: &Path) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|error| error.to_string())?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|error| format!("Invalid extension ZIP package: {error}"))?;
    if archive.len() > 4096 {
        return Err("Extension package contains more than 4096 entries.".into());
    }
    let mut seen = rustc_hash::FxHashSet::default();
    let mut total = 0u64;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(|error| error.to_string())?;
        let relative = entry
            .enclosed_name()
            .ok_or("Extension package path escapes its directory")?;
        validate_relative(&relative)?;
        if relative == Path::new("extension.json") && entry.size() > 1024 * 1024 {
            return Err("Extension manifest exceeds 1 MiB".into());
        }
        if relative.components().any(|part| {
            part.as_os_str()
                .to_string_lossy()
                .eq_ignore_ascii_case(".git")
        }) || relative
            .to_string_lossy()
            .eq_ignore_ascii_case(".vtcode-source.json")
            || relative.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .eq_ignore_ascii_case("extension.json")
            }) && relative != Path::new("extension.json")
        {
            return Err("Release packages must contain one root extension.json and no Git or installation metadata.".into());
        }
        let mode = entry.unix_mode().unwrap_or(0) & 0o170000;
        if !matches!(mode, 0 | 0o100000 | 0o040000) || mode == 0o040000 && !entry.is_dir() {
            return Err("Extension packages cannot contain links or special files.".into());
        }
        if !seen.insert(relative.to_string_lossy().to_lowercase()) {
            return Err("Extension package contains duplicate or case-colliding paths.".into());
        }
        let destination = package.join(&relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&destination).map_err(|error| error.to_string())?;
            continue;
        }
        if entry.size() > MAX_PACKAGE_BYTES - total {
            return Err("Unpacked extension package exceeds 256 MiB".into());
        }
        std::fs::create_dir_all(destination.parent().ok_or("Invalid package path")?)
            .map_err(|error| error.to_string())?;
        let mut output = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&destination)
            .map_err(|error| error.to_string())?;
        let copied = std::io::copy(
            &mut entry.by_ref().take(MAX_PACKAGE_BYTES - total + 1),
            &mut output,
        )
        .map_err(|error| format!("Cannot extract extension package: {error}"))?;
        total += copied;
        if total > MAX_PACKAGE_BYTES || copied != entry.size() {
            return Err("Unpacked extension package size is invalid or exceeds 256 MiB".into());
        }
        #[cfg(unix)]
        if entry.unix_mode().is_some_and(|mode| mode & 0o111 != 0) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o755))
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn validate_runtime(
    package: &Path,
    manifest: &crate::extension_manifest::ExtensionManifest,
) -> Result<(), String> {
    if manifest.main.is_empty() {
        return Ok(());
    }
    let wasm = package.join(&manifest.main);
    let metadata = std::fs::metadata(&wasm).map_err(|_| {
        format!(
            "{} is missing its prebuilt {} in the release package.",
            manifest.name, manifest.main
        )
    })?;
    if !metadata.is_file() || metadata.len() > MAX_WASM_BYTES {
        return Err("Extension Wasm must be a regular file of at most 32 MiB".into());
    }
    let bytes = std::fs::read(wasm).map_err(|error| error.to_string())?;
    if !bytes.starts_with(b"\0asm") {
        return Err("The release must contain a prebuilt WebAssembly binary.".into());
    }
    #[cfg(not(test))]
    let engine = wasmtime::Engine::default();
    #[cfg(test)]
    let engine = crate::extensions::test_extension_engine().clone();
    #[cfg(not(test))]
    let module = wasmtime::Module::new(&engine, &bytes);
    #[cfg(test)]
    let module = crate::extensions::test_extension_module(&engine, &bytes);
    let module = module.map_err(|error| format!("Invalid extension Wasm: {error}"))?;
    if module.imports().any(|import| import.module() != "vtcode") {
        return Err("Extension uses unsupported imports; build for wasm32-unknown-unknown with the vtcode SDK".into());
    }
    if !module
        .exports()
        .any(|export| export.name() == "vtcode_activate")
    {
        return Err("Extension is missing vtcode_activate".into());
    }
    let mut linker = wasmtime::Linker::<crate::extension_wasm::HostState>::new(&engine);
    crate::extension_host::define_host_functions(&mut linker)?;
    linker
        .instantiate_pre(&module)
        .map_err(|error| format!("Extension requires an incompatible host API: {error}"))?;
    Ok(())
}

#[cfg(test)]
#[path = "extension_release_tests.rs"]
mod tests;
