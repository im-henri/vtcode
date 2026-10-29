//! Repository access errors and explicit, repository-scoped Git trust.
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RecoveryAction { Retry, Trust }

impl RecoveryAction {
    pub(crate) fn run(self, app: &mut crate::app::App) {
        if app.bg_git_action_running || app.bg_git_remote_running || app.bg_git_status_running { return; }
        match self {
            Self::Retry => app.refresh_source_control_async(),
            Self::Trust => {
                let Some(root) = app.repositories.recovery_root.clone() else { return; };
                if app.repositories.failure().is_some_and(|error| error.root == root) {
                    app.trust_source_control_repository();
                }
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum FailureKind {
    Ownership,
    Repository,
    Discovery,
    Workspace,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Failure<P = PathBuf> {
    pub root: P,
    pub message: String,
    pub kind: FailureKind,
}

impl<P> Failure<P> {
    pub(crate) fn try_map<Q, E>(self, convert: impl FnOnce(P) -> Result<Q, E>) -> Result<Failure<Q>, E> {
        Ok(Failure { root: convert(self.root)?, message: self.message, kind: self.kind })
    }
}

fn metadata_root(path: &Path) -> Option<PathBuf> {
    let discover = |path: &Path| git2::Repository::discover_path(path, std::iter::empty::<&std::ffi::OsStr>())
        .ok().map(|path| std::fs::canonicalize(&path).unwrap_or(path));
    if let Some(metadata) = discover(path) {
        for directory in path.ancestors() {
            let marker = directory.join(".git");
            let matches = if marker.is_dir() {
                std::fs::canonicalize(&marker).is_ok_and(|path| path == metadata)
            } else {
                marker.is_file() && discover(directory).as_ref() == Some(&metadata)
            };
            if matches { return Some(directory.to_owned()); }
        }
        return Some(metadata); // A bare repository has no worktree marker.
    }
    // An existing but damaged Git marker must be reported as a failure, not
    // offered initialization as though the user's Git data did not exist.
    path.ancestors().find(|directory| directory.join(".git").try_exists().unwrap_or(false))
        .map(Path::to_path_buf)
}

impl Failure {
    pub(crate) fn opening(path: &Path, error: &git2::Error) -> Option<Self> {
        let repository = metadata_root(path);
        if error.code() == git2::ErrorCode::NotFound && repository.is_none() {
            return None;
        }
        Some(Self {
            root: std::fs::canonicalize(repository.as_deref().unwrap_or(path))
                .unwrap_or_else(|_| repository.unwrap_or_else(|| path.to_owned())),
            message: error.to_string(),
            kind: if error.code() == git2::ErrorCode::Owner {
                FailureKind::Ownership
            } else {
                FailureKind::Repository
            },
        })
    }

    pub(crate) fn reading(root: &Path, error: impl std::fmt::Display) -> Self {
        Self { root: root.to_owned(), message: error.to_string(), kind: FailureKind::Repository }
    }

    pub(crate) fn discovery(root: &Path, error: impl std::fmt::Display) -> Self {
        Self { root: root.to_owned(), message: error.to_string(), kind: FailureKind::Discovery }
    }
}

pub(crate) fn safe_directory_value(root: &Path) -> Result<std::ffi::OsString, String> {
    if !root.is_absolute() {
        return Err("Repository trust requires an absolute directory path.".into());
    }
    let normalized: PathBuf = root.components().collect();
    let root = normalized.as_path();
    if root.as_os_str().as_encoded_bytes().ends_with(b"/*") {
        return Err("Git would interpret this repository name as a trust wildcard; it cannot be trusted as a single directory.".into());
    }
    #[cfg(windows)]
    {
        let path = root.to_str().ok_or("The repository path cannot be represented in Git's configuration")?.replace('\\', "/");
        let path = path.strip_prefix("//?/UNC/").map(|tail| format!("//{tail}"))
            .unwrap_or_else(|| path.strip_prefix("//?/").unwrap_or(&path).to_owned());
        Ok(if path.starts_with("//") {
            format!("%(prefix)/{}", path.trim_end_matches('/'))
        } else { path }.into())
    }
    #[cfg(not(windows))]
    { Ok(root.as_os_str().to_owned()) }
}

pub(crate) fn trust_repository(root: &Path) -> Result<(), String> {
    let root = std::fs::canonicalize(root).map_err(|error| error.to_string())?;
    if metadata_root(&root).as_ref() != Some(&root) {
        return Err("The repository moved or its Git metadata was removed. Refresh Source Control and try again.".into());
    }
    match git2::Repository::open(&root) {
        Ok(_) => return Ok(()),
        Err(error) if error.code() == git2::ErrorCode::Owner => {}
        Err(error) => return Err(format!("The repository cannot be opened: {error}")),
    }
    let output = std::process::Command::new("git")
        .args(["config", "--global", "--add", "safe.directory"])
        .arg(safe_directory_value(&root)?)
        .current_dir(&root)
        .output().map_err(|error| format!("Could not save repository trust: {error}"))?;
    if !output.status.success() {
        return Err(format!("Could not save repository trust: {}", String::from_utf8_lossy(&output.stderr).trim()));
    }
    git2::Repository::open(&root)
        .map_err(|error| format!("Git trust was saved, but the repository still cannot be opened: {error}"))?;
    Ok(())
}

#[cfg(test)]
#[path = "git_ownership_tests.rs"]
mod tests;
