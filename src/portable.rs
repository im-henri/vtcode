//! All writable editor state belongs to the executable's portable directory.
use std::path::{Component, Path, PathBuf};

pub(crate) fn base_dir() -> PathBuf {
    std::env::current_exe()
        .expect("cannot locate the VTCode executable")
        .parent()
        .expect("VTCode executable has no parent directory")
        .to_path_buf()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_is_beside_executable() {
        let executable = std::env::current_exe().unwrap();
        assert_eq!(base_dir(), executable.parent().unwrap());
        assert!(workspace_dir(Path::new(".")).starts_with(data_dir()));
        assert_eq!(theme_extensions_dir(), data_dir().join("theme-extensions"));
        assert_eq!(workspace_dir(Path::new("")), workspace_dir(Path::new(".")));
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
