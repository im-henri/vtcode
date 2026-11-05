//! All writable editor state belongs to the executable's portable directory.
use std::{
    io,
    path::{Component, Path, PathBuf},
};

fn launched_base_dir() -> PathBuf {
    std::env::current_exe()
        .expect("cannot locate the VTCode executable")
        .parent()
        .expect("VTCode executable has no parent directory")
        .to_path_buf()
}

pub(crate) fn base_dir() -> PathBuf {
    #[cfg(windows)]
    {
        static BASE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        BASE.get_or_init(|| {
            let launched = launched_base_dir();
            launched
                .canonicalize()
                .map_or(launched, |path| filesystem_spelling(&path))
        })
        .clone()
    }
    #[cfg(not(windows))]
    launched_base_dir()
}

/// Keep the on-disk spelling but retain ordinary Windows paths where they
/// resolve to the same directory. Existing portable keys use that namespace.
pub(crate) fn filesystem_spelling(canonical: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::{ffi::OsString, path::Prefix};
        let mut components = canonical.components();
        if let Some(Component::Prefix(prefix)) = components.next() {
            let mut ordinary = match prefix.kind() {
                Prefix::VerbatimDisk(drive) => PathBuf::from(format!("{}:\\", char::from(drive))),
                Prefix::VerbatimUNC(server, share) => {
                    let mut unc = OsString::from(r"\\");
                    unc.push(server);
                    unc.push("\\");
                    unc.push(share);
                    unc.push("\\");
                    PathBuf::from(unc)
                }
                _ => return canonical.to_path_buf(),
            };
            for component in components.filter(|component| *component != Component::RootDir) {
                ordinary.push(component.as_os_str());
            }
            // Verbatim names with trailing dots, spaces or device names can
            // resolve differently in the ordinary Win32 namespace.
            if ordinary.canonicalize().is_ok_and(|path| path == canonical) {
                return ordinary;
            }
        }
    }
    canonical.to_path_buf()
}

pub(crate) fn data_dir() -> PathBuf {
    base_dir().join(".vtcode")
}

pub(crate) fn theme_extensions_dir() -> PathBuf {
    data_dir().join("theme-extensions")
}

pub(crate) fn workspace_dir(project_root: &Path) -> PathBuf {
    workspace_dir_in(&base_dir(), project_root)
}

fn normalized_path(path: &Path) -> PathBuf {
    // Path::parent() returns an empty path for a bare filename argument.
    let path = if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    };
    let absolute = std::path::absolute(path).expect("cannot resolve workspace path");
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn workspace_dir_in(base: &Path, project_root: &Path) -> PathBuf {
    if project_root.as_os_str().is_empty() {
        return normalized_path(base).join(".vtcode").join("empty-window");
    }
    // Do not depend on existence: recovery for a deleted/offline workspace
    // must retain the same key. Lexical normalization also handles '.'/'..'.
    let remote = crate::remote::workspace::is_view_key(project_root);
    let workspace = if remote { project_root.to_path_buf() } else { normalized_path(project_root) };
    let base = normalized_path(base);
    // Workspaces carried inside the portable directory keep their identity
    // when the whole directory is moved to another drive or machine.
    let (scope, identity) = if remote { (b"remote".as_slice(), workspace.as_path()) } else { match workspace.strip_prefix(&base) {
        Ok(relative) => (b"relative".as_slice(), relative),
        Err(_) => (b"absolute".as_slice(), workspace.as_path()),
    }};
    // Fixed FNV-1a 128-bit hash: unlike DefaultHasher, this persisted key is
    // stable across Rust releases, with no unsafe path components from input.
    let mut hash = 0x6c62272e07bb014262b821756295c58du128;
    for byte in scope.iter().chain(identity.as_os_str().as_encoded_bytes()) {
        hash ^= u128::from(*byte);
        hash = hash.wrapping_mul(0x0000000001000000000000000000013b);
    }
    base.join(".vtcode/workspaces").join(format!("{hash:032x}"))
}

/// Adopt the state of a known older spelling only if the resolved workspace
/// has no state yet. An existing workspace directory always wins.
pub(crate) fn recover_workspace_alias(requested: &Path, resolved: &Path) -> io::Result<()> {
    let legacy = workspace_dir_in(&launched_base_dir(), requested);
    let destination = workspace_dir(resolved);
    recover_alias_storage(&legacy, &destination)
}

fn recover_alias_storage(legacy: &Path, destination: &Path) -> io::Result<()> {
    if legacy == destination {
        return Ok(());
    }
    match destination.symlink_metadata() {
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    match legacy.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => return Err(io::Error::other("Older workspace state is not a directory")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    match crate::fs_ops::rename_no_replace(legacy, destination) {
        Ok(()) => Ok(()),
        Err(_) if destination.is_dir() => Ok(()),
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!("Cannot recover workspace state from {}: {error}", legacy.display()),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_is_beside_executable() {
        let executable = std::env::current_exe().unwrap();
        assert_eq!(base_dir().canonicalize().unwrap(), executable.parent().unwrap().canonicalize().unwrap());
        assert!(workspace_dir(Path::new(".")).starts_with(data_dir()));
        assert_eq!(theme_extensions_dir(), data_dir().join("theme-extensions"));
        assert_eq!(workspace_dir(Path::new("")), data_dir().join("empty-window"));
        assert_ne!(workspace_dir(Path::new("")), workspace_dir(Path::new(".")));
    }

    #[test]
    fn workspace_keys_are_isolated_normalized_and_read_only() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("portable");
        let first = temp.path().join("one/project");
        let second = temp.path().join("two/project");
        let path = workspace_dir_in(&base, &first);
        assert_ne!(path, workspace_dir_in(&base, &second));
        assert_eq!(path, workspace_dir_in(&base, &first.join("child/..")));
        std::fs::create_dir_all(&first).unwrap();
        assert_eq!(path, workspace_dir_in(&base, &first));
        assert!(!base.exists());
        assert!(!first.join(".vtcode").exists());
    }

    #[test]
    fn workspace_spelling_follows_the_filesystem_instead_of_folding_case() {
        let temp = tempfile::tempdir().unwrap();
        let mixed = temp.path().join("MixedCase");
        let lower = temp.path().join("mixedcase");
        std::fs::create_dir(&mixed).unwrap();
        let first = crate::startup::LocalFolder::resolve(&mixed).unwrap();
        match std::fs::create_dir(&lower) {
            Ok(()) => {
                let second = crate::startup::LocalFolder::resolve(&lower).unwrap();
                assert_ne!(first.canonical, second.canonical);
                assert_ne!(workspace_dir(&first.root), workspace_dir(&second.root));
            }
            #[cfg(windows)]
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let second = crate::startup::LocalFolder::resolve(&lower).unwrap();
                assert_eq!(first.root, second.root);
                assert_eq!(workspace_dir(&first.root), workspace_dir(&second.root));
                assert_eq!(first.root.file_name().unwrap(), "MixedCase");
            }
            #[cfg(not(windows))]
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => panic!("could not create test directory: {error}"),
        }
    }

    #[cfg(any(windows, target_os = "linux"))]
    #[test]
    fn recovery_adopts_older_alias_state_only_when_the_resolved_key_is_empty() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("portable");
        let old = workspace_dir_in(&base, &temp.path().join("WORKSPACE"));
        let resolved = workspace_dir_in(&base, &temp.path().join("Workspace"));
        assert_ne!(old, resolved);
        std::fs::create_dir_all(old.join("unsaved")).unwrap();
        std::fs::write(old.join("unsaved/draft"), "recovery data").unwrap();
        recover_alias_storage(&old, &resolved).unwrap();
        assert_eq!(std::fs::read_to_string(resolved.join("unsaved/draft")).unwrap(), "recovery data");
        assert!(!old.exists());

        std::fs::create_dir(&old).unwrap();
        std::fs::write(old.join("session.json"), "other session").unwrap();
        recover_alias_storage(&old, &resolved).unwrap();
        assert_eq!(std::fs::read_to_string(old.join("session.json")).unwrap(), "other session");
        assert_eq!(std::fs::read_to_string(resolved.join("unsaved/draft")).unwrap(), "recovery data");
    }

    #[test]
    fn moving_portable_workspaces_preserves_their_key() {
        let temp = tempfile::tempdir().unwrap();
        let before = temp.path().join("before");
        let after = temp.path().join("after");
        let old = workspace_dir_in(&before, &before.join("projects/example"));
        let new = workspace_dir_in(&after, &after.join("projects/example"));
        assert_eq!(
            old.strip_prefix(before).unwrap(),
            new.strip_prefix(after).unwrap()
        );
    }

    #[test]
    fn workspace_settings_sessions_and_extension_settings_stay_portable() {
        let workspace = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let state = workspace_dir(workspace.path());
        let settings_path = crate::settings::project_settings_path(workspace.path());
        assert_eq!(settings_path, state.join("settings.json"));
        let settings = crate::settings::Settings {
            tab_size: Some(8),
            ..Default::default()
        };
        crate::settings::save_to_file(&settings_path, &settings).unwrap();
        assert_eq!(crate::settings::load_from_file(&settings_path), settings);
        assert_eq!(
            crate::settings::load_from_file(&crate::settings::project_settings_path(other.path())),
            crate::settings::Settings::default()
        );

        let session = crate::session::Session {
            show_panel: true,
            ..Default::default()
        };
        crate::session::save_session(workspace.path(), &session).unwrap();
        assert!(
            crate::session::load_session(workspace.path())
                .unwrap()
                .show_panel
        );
        assert!(crate::session::load_session(other.path()).is_none());
        assert_eq!(
            crate::session::unsaved_dir(workspace.path()),
            state.join("unsaved")
        );

        let manager = crate::extensions::ExtensionManager::new(workspace.path());
        assert_eq!(manager.settings_file, state.join("extension-settings.json"));
        assert_eq!(
            crate::extension_wasm::save_extension_setting(
                &manager.settings_file,
                "test",
                "key",
                "value"
            ),
            0
        );
        assert_eq!(
            crate::extension_wasm::load_extension_setting(&manager.settings_file, "test", "key"),
            Some("value".into())
        );
        assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
        assert_eq!(std::fs::read_dir(other.path()).unwrap().count(), 0);
        assert!(state.starts_with(data_dir().join("workspaces")));
        std::fs::remove_dir_all(state).unwrap();
    }
}
