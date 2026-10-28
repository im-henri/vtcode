use crate::app::{App, AutoSaveMode, GitFileChange};
use crate::app_ui_state::BINARY_DIFF_MESSAGE;
use crate::app_git_display::{
    compute_line_diff, map_worktree_line_to_index_line, parse_git_file_patch, select_patch_hunk,
};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

#[path = "app_git_changes.rs"]
pub(crate) mod changes;

#[path = "app_git_refresh.rs"]
pub(crate) mod refresh;

pub(crate) struct SourceControlStatusSnapshot<P = String> {
    pub files: Vec<GitFileChange<P>>,
    pub workdir: Option<PathBuf>,
    pub repository_found: bool,
    pub branch: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct GitCommitRequest {
    pub root: PathBuf,
    pub message: String,
}

fn ignore_formatter_working_copies(repo: &git2::Repository) -> Result<(), git2::Error> {
    // Only this repository handle is affected; never edit the user's ignores.
    repo.add_ignore_rule(&format!("{}*/", crate::formatters::TEMP_DIR_PREFIX))
}

pub(crate) fn commit_repository(
    root: &Path,
    message: &str,
    stage_all: bool,
) -> Result<crate::background::GitActionOutcome, String> {
    let repo = git2::Repository::open(root).map_err(|error| error.to_string())?;
    let sig = repo
        .signature()
        .or_else(|_| git2::Signature::now("VTCode User", "user@vtcode.dev"))
        .map_err(|error| error.to_string())?;
    let parent = match repo.head() {
        Ok(head) => Some(head.peel_to_commit().map_err(|error| error.to_string())?),
        Err(error)
            if matches!(
                error.code(),
                git2::ErrorCode::NotFound | git2::ErrorCode::UnbornBranch
            ) =>
        {
            None
        }
        Err(error) => return Err(error.to_string()),
    };
    let mut index = repo.index().map_err(|error| error.to_string())?;
    if index.has_conflicts() {
        return Err("Resolve merge conflicts before committing".into());
    }
    if stage_all {
        ignore_formatter_working_copies(&repo).map_err(|error| error.to_string())?;
        index
            .add_all(std::iter::once("*"), git2::IndexAddOption::DEFAULT, None)
            .map_err(|error| error.to_string())?;
    }
    let tree_oid = index.write_tree().map_err(|error| error.to_string())?;
    let tree = repo.find_tree(tree_oid).map_err(|error| error.to_string())?;
    // Recheck the actual index: files may have been reverted while the
    // confirmation was open or the displayed status may be out of date.
    if parent
        .as_ref()
        .map_or(tree.is_empty(), |parent| parent.tree_id() == tree_oid)
    {
        if stage_all {
            return Err("No changes to commit".into());
        }
        return Ok(crate::background::GitActionOutcome {
            stage_all_commit_request: Some(GitCommitRequest {
                root: root.to_path_buf(),
                message: message.to_owned(),
            }),
            ..Default::default()
        });
    }
    if stage_all {
        index.write().map_err(|error| error.to_string())?;
    }
    let parents: Vec<&git2::Commit> = parent.iter().collect();
    repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &parents)
        .map_err(|error| error.to_string())?;
    Ok(crate::background::GitActionOutcome {
        message: format!("Committed: {message}"),
        refresh_log: true,
        refresh_line_status: stage_all,
        clear_commit_message: Some(GitCommitRequest {
            root: root.to_path_buf(),
            message: message.to_owned(),
        }),
        ..Default::default()
    })
}

pub(crate) fn diff_blob_content(blob: &git2::Blob<'_>) -> Result<String, String> {
    if blob.is_binary() {
        return Err(BINARY_DIFF_MESSAGE.into());
    }
    Ok(String::from_utf8_lossy(blob.content()).into_owned())
}

// Preserve a missing Git entry separately from a tracked file with empty contents.
pub(crate) fn head_file_content(
    repo: &git2::Repository,
    relative: &Path,
    staged: bool,
) -> Result<Option<std::sync::Arc<str>>, String> {
    blob_file_content(repo, head_file_id(repo, relative, staged)?)
}

pub(crate) fn head_file_id(
    repo: &git2::Repository,
    relative: &Path,
    staged: bool,
) -> Result<Option<git2::Oid>, String> {
    let tree = match repo.head().and_then(|head| head.peel_to_tree()) {
        Ok(tree) => tree,
        Err(error)
            if matches!(
                error.code(),
                git2::ErrorCode::NotFound | git2::ErrorCode::UnbornBranch
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(format!("Cannot read HEAD: {error}")),
    };
    let original_path = if staged {
        let mut diff = repo.diff_tree_to_index(Some(&tree), None, None)
            .map_err(|error| format!("Cannot compare HEAD and index: {error}"))?;
        diff.find_similar(Some(git2::DiffFindOptions::new().renames(true)))
            .map_err(|error| format!("Cannot detect staged moves: {error}"))?;
        diff.deltas().find_map(|delta| {
            (delta.status() == git2::Delta::Renamed && delta.new_file().path() == Some(relative))
                .then(|| delta.old_file().path().map(Path::to_path_buf)).flatten()
        })
    } else {
        None
    };
    let entry = match tree.get_path(original_path.as_deref().unwrap_or(relative)) {
        Ok(entry) => entry,
        Err(error) if error.code() == git2::ErrorCode::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(format!("Cannot read Git tree entry: {error}")),
    };
    Ok(Some(entry.id()))
}

pub(crate) fn index_file_content(
    repo: &git2::Repository,
    relative: &Path,
) -> Result<Option<std::sync::Arc<str>>, String> {
    blob_file_content(repo, index_file_id(repo, relative)?)
}

pub(crate) fn index_file_id(
    repo: &git2::Repository,
    relative: &Path,
) -> Result<Option<git2::Oid>, String> {
    let index = repo
        .index()
        .map_err(|error| format!("Cannot read Git index: {error}"))?;
    let Some(entry) = index.get_path(relative, 0) else {
        return Ok(None);
    };
    // `git add -N` records a placeholder, not a staged version of the new file.
    if entry.flags_extended & git2::IndexEntryExtendedFlag::INTENT_TO_ADD.bits() != 0 {
        return Ok(None);
    }
    Ok(Some(entry.id))
}

pub(crate) fn blob_file_content(
    repo: &git2::Repository,
    id: Option<git2::Oid>,
) -> Result<Option<std::sync::Arc<str>>, String> {
    let Some(id) = id else { return Ok(None); };
    let blob = repo.find_blob(id).map_err(|error| format!("Cannot read Git blob: {error}"))?;
    diff_blob_content(&blob).map(|content| Some(content.into()))
}

pub(crate) fn repository_relative_path(workdir: &Path, file_path: &Path) -> Option<PathBuf> {
    if let Some(relative) = crate::native_paths::strip_prefix(file_path, workdir) {
        // On Unix the prefix can match even when the remaining path contains
        // `..`. Git index lookups need the resolved repository-relative path.
        if !relative.components().any(|part| part == std::path::Component::ParentDir) {
            return Some(relative.to_path_buf());
        }
    }

    // A workspace opened as `.` (or through a path containing `..`) does not
    // have the same lexical spelling as libgit2's worktree path. Compare their
    // canonical forms so tracked files still receive gutter decorations.
    let canonical_workdir = std::fs::canonicalize(workdir).ok()?;
    if let Ok(relative) = file_path.strip_prefix(&canonical_workdir)
        && !relative.components().any(|part| part == std::path::Component::ParentDir)
    {
        return Some(relative.to_owned());
    }
    let canonical_file = std::fs::canonicalize(file_path).ok().or_else(|| {
        // Whole directories can be deleted too. Resolve the nearest surviving
        // ancestor, then normalize only the missing suffix before checking that
        // it still belongs to the worktree. No missing directory must be opened.
        let absolute = if file_path.is_absolute() { file_path.to_owned() }
            else { std::env::current_dir().ok()?.join(file_path) };
        let mut ancestor = absolute.as_path();
        let mut suffix = Vec::new();
        loop {
            match std::fs::canonicalize(ancestor) {
                Ok(mut resolved) => {
                    for part in suffix.iter().rev() {
                        if part == std::ffi::OsStr::new("..") { if !resolved.pop() { return None; } }
                        else if part != std::ffi::OsStr::new(".") { resolved.push(part); }
                    }
                    return Some(resolved);
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return None,
            }
            let part = ancestor.components().next_back()?;
            if !matches!(part, std::path::Component::Normal(_) | std::path::Component::ParentDir | std::path::Component::CurDir) { return None; }
            suffix.push(part.as_os_str().to_os_string());
            ancestor = ancestor.parent()?;
        }
    })?;
    canonical_file
        .strip_prefix(canonical_workdir)
        .ok()
        .map(Path::to_path_buf)
}

#[cfg(test)]
fn collect_source_control_status(root: PathBuf) -> SourceControlStatusSnapshot {
    collect_source_control_status_with_progress(root, || {})
}

#[cfg(test)]
fn collect_source_control_status_with_progress(
    root: PathBuf,
    repository_found_callback: impl FnOnce(),
) -> SourceControlStatusSnapshot {
    collect_source_control_status_cancellable(root, repository_found_callback, &crate::background::CancellationToken::new())
}

#[cfg(test)]
pub(crate) fn collect_source_control_status_cancellable(
    root: PathBuf,
    repository_found_callback: impl FnOnce(),
    cancel: &crate::background::CancellationToken,
) -> SourceControlStatusSnapshot {
    collect_source_control_status_with_paths(root, repository_found_callback, cancel, true, |path| path.to_string_lossy().into_owned())
}

#[cfg(test)]
pub(crate) fn collect_source_control_status_with_paths<P: Ord>(
    root: PathBuf,
    repository_found_callback: impl FnOnce(),
    cancel: &crate::background::CancellationToken,
    include_untracked: bool,
    format_path: impl Fn(&Path) -> P,
) -> SourceControlStatusSnapshot<P> {
    collect_source_control_status_impl(root, repository_found_callback, cancel, format_path, None, include_untracked)
}

fn collect_source_control_status_impl<P: Ord>(
    root: PathBuf,
    repository_found_callback: impl FnOnce(),
    cancel: &crate::background::CancellationToken,
    format_path: impl Fn(&Path) -> P,
    mut changed: Option<&mut refresh::WorktreePaths>,
    include_untracked: bool,
) -> SourceControlStatusSnapshot<P> {
    if cancel.is_cancelled() {
        return SourceControlStatusSnapshot { files: Vec::new(), workdir: None, repository_found: false, branch: String::new() };
    }
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    let mut files = Vec::new();
    let mut workdir = None;
    let mut repository_found = false;
    let mut branch = String::new();

    if let Ok(repo) = git2::Repository::discover(&root) {
        repository_found = true;
        // Report discovery before traversing the worktree. Large repositories
        // can spend seconds here even though their Git directory was found.
        repository_found_callback();
        workdir = repo
            .workdir()
            .map(|path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()));
        if cancel.is_cancelled() {
            return SourceControlStatusSnapshot { files, workdir, repository_found, branch };
        }
        branch = repo.head().ok().and_then(|head| head.shorthand().map(str::to_owned)).unwrap_or_default();

        // When requested, include every untracked descendant within the scope. Its compact
        // untracked-directory form is not enough to decorate every collapsed
        // Explorer ancestor reliably, and truncating the list makes the
        // visible states depend on status iteration order.
        let _ = ignore_formatter_working_copies(&repo);
        if let Some(changed) = changed.as_deref_mut() {
            // Use Git's policy, including case-sensitive Windows worktrees,
            // when merging cached paths after this read.
            changed.set_ignore_case(repo.config().and_then(|config| config.get_bool("core.ignorecase"))
                .unwrap_or(cfg!(windows)));
        }
        let mut options = git2::StatusOptions::new();
        options
            .show(if changed.is_some() { git2::StatusShow::Workdir } else { git2::StatusShow::IndexAndWorkdir })
            .include_untracked(include_untracked)
            .recurse_untracked_dirs(include_untracked)
            .renames_head_to_index(changed.is_none());
        if let Some(changed) = changed {
            options.disable_pathspec_match(true);
            for path in &changed.paths { options.pathspec(path); }
        }

        if let Ok(statuses) = repo.statuses(Some(&mut options)) {
            for entry in statuses.iter() {
                if cancel.is_cancelled() { break; }
                let status = entry.status();
                if status.contains(git2::Status::CONFLICTED) {
                    let path = entry.head_to_index().or_else(|| entry.index_to_workdir())
                        .and_then(|delta| delta.new_file().path().or_else(|| delta.old_file().path()).map(&format_path));
                    if let Some(path) = path {
                        files.push(GitFileChange { path,
                            original_path: None, status: '!', staged: false });
                    }
                    continue;
                }
                for staged in [true, false] {
                    let (delta, flags): (_, &[(git2::Status, char)]) = if staged {
                        (entry.head_to_index(), &[
                            (git2::Status::INDEX_RENAMED, 'R'),
                            (git2::Status::INDEX_NEW, 'A'),
                            (git2::Status::INDEX_DELETED, 'D'),
                            (git2::Status::INDEX_MODIFIED, 'M'),
                        ])
                    } else {
                        (entry.index_to_workdir(), &[
                            (git2::Status::WT_NEW, '?'),
                            (git2::Status::WT_DELETED, 'D'),
                            (git2::Status::WT_MODIFIED, 'M'),
                        ])
                    };
                    let Some((_, marker)) = flags.iter().find(|(flag, _)| status.contains(*flag)) else { continue; };
                    let Some(delta) = delta else { continue; };
                    let old_file = delta.old_file();
                    let new_file = delta.new_file();
                    let Some(path) = new_file.path().or_else(|| old_file.path()) else { continue; };
                    files.push(GitFileChange {
                        path: format_path(path),
                        original_path: if *marker == 'R' {
                            old_file.path().map(&format_path)
                        } else { None },
                        status: *marker,
                        staged,
                    });
                }
            }
        }
    }

    let group = |file: &GitFileChange<P>| if file.status == '!' { 0 } else if file.staged { 1 } else if file.status == '?' { 3 } else { 2 };
    files.sort_unstable_by(|a, b| group(a).cmp(&group(b)).then(a.path.cmp(&b.path)));
    SourceControlStatusSnapshot {
        files,
        workdir,
        repository_found,
        branch,
    }
}

impl App {
    /// Discover the git repository for the current explorer root.
    /// On failure, sends a notification and returns `None`.
    pub(crate) fn discover_repo(&mut self) -> Option<git2::Repository> {
        if !self.source_control_enabled { return None; }
        match git2::Repository::discover(self.source_control_root()) {
            Ok(r) => Some(r),
            Err(e) => {
                self.notify_error(format!("Git error: {e}"));
                None
            }
        }
    }

    pub(crate) fn run_git_action_async(
        &mut self,
        worker_name: &'static str,
        action: &'static str,
        job: impl FnOnce(PathBuf) -> Result<crate::background::GitActionOutcome, String>
            + Send
            + 'static,
    ) {
        if !self.source_control_enabled { return; }
        if action != "initialize repository" && self.repositories.discovered && self.repositories.active.is_none()
        {
            self.notify("Open a Git repository in Source Control first");
            return;
        }
        if self.bg_git_action_running || self.bg_git_remote_running {
            self.notify("Another git operation is already running");
            return;
        }
        self.bg_git_action_running = true;
        self.status_message = format!("Git {action}...");
        let root = self.source_control_root().to_owned();
        let log = self.git_output();
        let operation = self.remote_workspace.is_none().then(||
            log.operation("native Git", action, &root.to_string_lossy()));
        let sender = self.background.sender.clone();
        self.background.spawn(worker_name, move || {
            // Typed jobs open/validate their repository themselves. Opening
            // it here as well adds another metadata round trip on shared disks.
            if let Some(operation) = &operation { operation.stage("Running"); }
            let result = job(root);
            if let Some(operation) = &operation {
                operation.result(&result, |_| "Repository operation finished".into());
            }
            // Completion must release the operation gate even after a folder
            // switch. Commit requests carry their original repository identity
            // so they cannot clear the new workspace's draft.
            let _ = sender.send(crate::background::BackgroundResult::GitActionDone { action, result });
        });
    }

    /// Rapid navigation must not repeatedly walk the whole working tree.
    /// Watcher events, saves, Git actions and explicit refresh bypass this gate.
    pub(crate) fn refresh_source_control_for_navigation(&mut self) {
        if self.bg_git_status_running
            || self.source_control_last_refresh.is_some_and(|time| time.elapsed() < std::time::Duration::from_secs(2))
        {
            self.refresh_dirty_source_control_diffs();
            return;
        }
        if self.workspace_watcher_active {
            self.refresh_dirty_source_control_diffs();
            // Navigation does not invalidate remote Git state. Reuse fresh
            // snapshots; their full-read deadline still catches missed events.
            let paths = self.remote_workspace.as_ref()
                .filter(|_| self.source_control_repository_state == crate::app::SourceControlRepositoryState::Available)
                .map(|_| Default::default());
            self.start_source_control_status_refresh(paths);
        } else {
            self.refresh_source_control_async();
        }
    }

    /// Reuse discovered roots between invalidations; one worker serializes status
    /// scans instead of competing readers walking the same disks concurrently.
    pub(crate) fn refresh_source_control_async(&mut self) {
        self.refresh_source_control_for_paths(None);
    }

    pub(crate) fn refresh_source_control_changed_paths(&mut self, paths: impl IntoIterator<Item = std::path::PathBuf>) {
        // These paths are authoritative for diff invalidation even when the
        // status collector needs its unwatched-workspace full-scan fallback.
        self.refresh_source_control_for_paths(Some(paths.into_iter().collect()));
    }

    pub(crate) fn refresh_source_control_for_paths(&mut self, paths: Option<rustc_hash::FxHashSet<std::path::PathBuf>>) {
        self.refresh_source_control_diffs_for_paths(paths.as_ref());
        self.start_source_control_status_refresh(paths);
    }

    pub(crate) fn refresh_source_control_git_changed_paths(&mut self, paths: impl IntoIterator<Item = std::path::PathBuf>) {
        self.refresh_source_control_diff_git_changed(&paths.into_iter().collect());
        self.start_source_control_status_refresh(None);
    }

    pub(crate) fn start_source_control_status_refresh(&mut self, mut paths: Option<rustc_hash::FxHashSet<std::path::PathBuf>>) {
        if !self.source_control_enabled { return; }
        if self.repositories.workspace.as_ref().is_some_and(|root| root != &self.explorer.root) {
            self.repositories = Default::default();
        }
        self.repositories.workspace = Some(self.explorer.root.clone());
        if self.bg_git_status_running {
            if self.bg_git_status_pending {
                match (&mut self.pending_git_status_paths, paths) {
                    (Some(pending), Some(paths)) => pending.extend(paths),
                    (pending, _) => *pending = None,
                }
            } else {
                self.pending_git_status_paths = paths;
            }
            self.bg_git_status_pending = true;
            return;
        }
        if self.repositories.rescan || !self.repositories.discovered || !self.workspace_watcher_active {
            paths = None;
        }
        self.bg_git_status_running = true;
        let report_discovery = self.source_control_repository_state
            != crate::app::SourceControlRepositoryState::Available;
        let root = self.explorer.root.clone();
        let workspace_generation = self.workspace_generation;
        let known = (self.repositories.discovered && !self.repositories.rescan).then(|| {
            crate::git_repositories::Discovery {
                roots: self.repositories.entries.iter()
                    .filter(|repo| Some(&repo.root) != self.repositories.parent.as_ref())
                    .map(|repo| repo.root.clone()).collect(),
                parent: self.repositories.parent.clone(),
                workspace: self.repositories.canonical_workspace.clone().unwrap_or_else(|| self.explorer.root.clone()),
            }
        });
        self.repositories.rescan = false;
        let parent_open = self.repositories.parent_open;
        let repository_generation = self.repositories.membership_generation;
        let include_untracked = self.sc_visibility.untracked;
        let closed = self.repositories.closed.clone();
        let sender = self.source_control_sender();
        let log = self.git_output();
        if let Some(remote) = self.remote_workspace.clone() {
            let paths = paths.and_then(|paths| paths.iter().map(|path| remote.remote_path(path)).collect::<Result<Vec<_>,_>>().ok());
            let known = known.and_then(|known| {
                Some(crate::remote::git::Discovery {
                    roots: known.roots.iter().map(|path| remote.remote_path(path)).collect::<Result<_,_>>().ok()?,
                    parent: known.parent.as_ref().map(|path| remote.remote_path(path)).transpose().ok()?,
                })
            });
            let closed = closed.iter().filter_map(|path| remote.remote_path(path).ok()).collect();
            self.background.read_for_file("remote-git-status", &root.clone(), false, move || {
                match remote.git_snapshot(include_untracked, known, parent_open, closed, paths, sender.cancellation()) {
                    Ok((discovery, scans)) => { let _ = sender.send(crate::background::BackgroundResult::GitRepositories { root, workspace_generation, repository_generation, discovery, scans }); }
                    Err(error) => { let _ = sender.send(crate::background::BackgroundResult::RemoteWorkspaceError { operation: "Git status", error: error.to_string() }); }
                }
            });
            return;
        }
        self.background.spawn("git-status", move || {
            if sender.is_cancelled() { return; }
            let operation = crate::git_output::native(&log, "Refresh status", &root);
            let announce = || {
                if report_discovery {
                    let _ = sender.send(crate::background::BackgroundResult::GitRepositoryFound {
                        root: root.clone(), workspace_generation, repository_generation,
                    });
                }
            };
            let exact = report_discovery.then(|| git2::Repository::open(&root).ok()
                .filter(|repo| !repo.is_bare())
                .and_then(|repo| repo.workdir().map(|path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned()))))
                .flatten();
            let announced = exact.as_ref().is_some_and(|root| !closed.contains(root));
            if announced { announce(); }
            let mut discovery = known.unwrap_or_else(|| crate::git_repositories::discover(&root, sender.cancellation()));
            // A previously missing workspace can gain its own .git between
            // refreshes, even when the watcher has not delivered that event yet.
            if let Some(workdir) = exact {
                if !discovery.roots.contains(&workdir) { discovery.roots.insert(0, workdir); }
                if !parent_open { discovery.parent = None; }
            }
            let mut roots = discovery.roots.clone();
            if parent_open && let Some(parent) = &discovery.parent { roots.push(parent.clone()); }
            roots.retain(|root| !closed.contains(root));
            if !announced && !roots.is_empty() { announce(); }
            let scans = refresh::collect(&mut discovery, roots, paths, &root, sender.cancellation(), include_untracked);
            if !sender.is_cancelled() {
                operation.completed(format!("{} repositories found; {} snapshots updated; {} status entries; untracked={include_untracked}",
                    discovery.roots.len(), scans.len(), scans.iter().map(|scan| scan.files.len()).sum::<usize>()));
            }
            let _ = sender.send(crate::background::BackgroundResult::GitRepositories {
                root, workspace_generation, repository_generation, discovery, scans,
            });
        });
    }

    pub(crate) fn git_initialize_repository(&mut self) {
        if self.source_control_repository_state
            != crate::app::SourceControlRepositoryState::Missing
        {
            return;
        }
        self.queue_git_task("git-init", "initialize repository", crate::git_tasks::Task::Init);
    }

    #[cfg(test)]
    pub(crate) fn git_stage_file(&mut self, idx: usize) {
        let changes = self.source_control_files.get(idx).cloned().into_iter().collect();
        self.git_stage_changes(changes);
    }

    pub(crate) fn git_stage_changes(&mut self, changes: Vec<GitFileChange>) {
        let changes: Vec<_> = changes.into_iter().filter(|change| !change.staged).collect();
        if changes.is_empty() { return; }
        self.queue_git_task("git-stage", "stage", crate::git_tasks::Task::Stage(changes));
    }

    #[cfg(test)]
    pub(crate) fn git_unstage_file(&mut self, idx: usize) {
        let changes = self.source_control_files.get(idx).cloned().into_iter().collect();
        self.git_unstage_changes(changes);
    }

    pub(crate) fn git_unstage_changes(&mut self, changes: Vec<GitFileChange>) {
        let changes: Vec<_> = changes.into_iter().filter(|change| change.staged).collect();
        if changes.is_empty() { return; }
        self.queue_git_task("git-unstage", "unstage", crate::git_tasks::Task::Unstage(changes));
    }

    pub(crate) fn git_stage_all(&mut self) {
        self.queue_git_task("git-stage-all", "stage all", crate::git_tasks::Task::StageAll);
    }

    pub(crate) fn git_unstage_all(&mut self) {
        self.queue_git_task("git-unstage-all", "unstage all", crate::git_tasks::Task::UnstageAll);
    }

    pub(crate) fn git_stage_hunk(&mut self, file: &Path, line: usize) {
        if self.remote_workspace.is_some() { self.remote_hunk_action(file, line, crate::remote::hunk::Kind::Stage); return; }
        let operation = crate::git_output::native(&self.git_output(), "git apply --cached (stage hunk)", file);
        let label = Self::git_hunk_label(file);
        if let Err(err) = self.ensure_hunk_actions_ready(file, false) {
            operation.failed(&err);
            self.notify_error(err);
            return;
        }
        let result =
            self.selected_unstaged_hunk_patch(file, line)
                .and_then(|(repo_root, patch)| {
                    Self::run_git_apply(
                        &repo_root,
                        &[
                            "apply".to_string(),
                            "--cached".to_string(),
                            "--whitespace=nowarn".to_string(),
                            "--unidiff-zero".to_string(),
                        ],
                        &patch,
                    )
                });
        operation.result(&result, |_| "Hunk staged".into());
        match result {
            Ok(()) => {
                self.refresh_source_control_async();
                self.refresh_git_line_status_async();
                self.notify(format!("Staged hunk in {label}"));
            }
            Err(err) => self.notify_error(format!("Stage hunk failed: {err}")),
        }
    }

    pub(crate) fn git_unstage_hunk(&mut self, file: &Path, line: usize) {
        if self.remote_workspace.is_some() { self.remote_hunk_action(file, line, crate::remote::hunk::Kind::Unstage); return; }
        let operation = crate::git_output::native(&self.git_output(), "git apply --cached --reverse (unstage hunk)", file);
        let label = Self::git_hunk_label(file);
        if let Err(err) = self.ensure_hunk_actions_ready(file, false) {
            operation.failed(&err);
            self.notify_error(err);
            return;
        }
        let result = self
            .selected_staged_hunk_patch(file, line)
            .and_then(|(repo_root, patch)| {
                Self::run_git_apply(
                    &repo_root,
                    &[
                        "apply".to_string(),
                        "--cached".to_string(),
                        "--reverse".to_string(),
                        "--whitespace=nowarn".to_string(),
                        "--unidiff-zero".to_string(),
                    ],
                    &patch,
                )
            });
        operation.result(&result, |_| "Hunk unstaged".into());
        match result {
            Ok(()) => {
                self.refresh_source_control_async();
                self.refresh_git_line_status_async();
                self.notify(format!("Reverted hunk in {label}"));
            }
            Err(err) => self.notify_error(format!("Revert hunk failed: {err}")),
        }
    }

    pub(crate) fn git_discard_hunk(&mut self, file: &Path, line: usize) {
        if self.remote_workspace.is_some() { self.remote_hunk_action(file, line, crate::remote::hunk::Kind::Discard); return; }
        let operation = crate::git_output::native(&self.git_output(), "git apply --reverse (discard hunk)", file);
        let label = Self::git_hunk_label(file);
        if let Err(err) = self.ensure_hunk_actions_ready(file, true) {
            operation.failed(&err);
            self.notify_error(err);
            return;
        }
        let result =
            self.selected_unstaged_hunk_patch(file, line)
                .and_then(|(repo_root, patch)| {
                    Self::run_git_apply(
                        &repo_root,
                        &[
                            "apply".to_string(),
                            "--reverse".to_string(),
                            "--whitespace=nowarn".to_string(),
                            "--unidiff-zero".to_string(),
                        ],
                        &patch,
                    )
                });
        operation.result(&result, |_| "Hunk discarded".into());
        match result {
            Ok(()) => {
                // Install the result and its undo step before another input
                // can use the old text/line numbers or undo a previous edit.
                let reload = self.reload_clean_editor_from_disk(file);
                self.refresh_source_control_async();
                self.refresh_git_line_status_async();
                match reload {
                    Ok(()) => self.notify(format!("Discarded hunk in {label}")),
                    Err(err) => {
                        self.queue_file_reload(file.to_path_buf());
                        self.notify_error(format!("Discarded hunk in {label}, but could not reload it: {err}"));
                    }
                }
            }
            Err(err) => self.notify_error(format!("Discard hunk failed: {err}")),
        }
    }

    pub(crate) fn git_discard_changes(&mut self, mut changes: Vec<GitFileChange>) {
        // A file can be selected in both sections. Discard it once, with the
        // staged selection explicitly authorizing restoration to HEAD.
        changes.sort_by(|a, b| a.path.cmp(&b.path).then(b.staged.cmp(&a.staged)));
        changes.dedup_by(|a, b| a.path == b.path);
        if changes.is_empty() { return; }
        self.queue_git_task("git-discard", "discard", crate::git_tasks::Task::Discard(changes));
    }

    pub(crate) fn open_source_control_diff(&mut self, idx: usize) {
        let change = match self.source_control_files.get(idx) {
            Some(c) => c.clone(),
            None => return,
        };
        let file_path = self.workspace_file_path(&self.source_control_root().join(&change.path));
        if change.status == '!' {
            self.open_file(&file_path);
            self.active_pane = crate::app::ActivePane::Editor;
            return;
        }
        self.open_source_control_diff_in_repository(&file_path, change.status == 'D', change.staged,
            Some(self.source_control_root().to_owned()));
    }

    /// Request the active Source Control comparison (or HEAD for a regular editor)
    /// without reading blobs, flattening ropes,
    /// or running the diff algorithm on the terminal event loop.
    pub(crate) fn request_compare_with_git_async(&mut self) -> bool {
        if !self.source_control_enabled { return false; }
        let Some(editor) = self.editor() else {
            return false;
        };
        if editor.encoding == "Binary" {
            self.reject_binary_diff();
            return false;
        }
        let Some(file_path) = editor.file_path.clone() else {
            return false;
        };
        let file_name = editor.file_name().to_string();
        let still_loading = editor.bg_loading;
        if editor.large_file_mode {
            self.notify("Diff is paused for this large file; adjust the large-file threshold to enable it");
            return false;
        }

        self.clear_diff_state();
        self.git_compare_generation = self.git_compare_generation.wrapping_add(1);
        self.pending_git_compare_path = Some(file_path.clone());
        self.bg_git_compare_running = false;
        self.status_message = if still_loading {
            format!("Loading file for diff: {file_name}...")
        } else {
            format!("Preparing diff: {file_name}...")
        };
        self.start_pending_git_compare_for_path(&file_path);
        true
    }

    /// Start a pending compare once the target editor has real file contents.
    /// `FileLoaded` calls this again after replacing its placeholder rope.
    pub(crate) fn start_pending_git_compare_for_path(&mut self, file_path: &Path) {
        if !self.source_control_enabled { return; }
        if self.bg_git_compare_running
            || self.pending_git_compare_path.as_deref() != Some(file_path)
        {
            return;
        }

        let Some(editor) = self.editor() else {
            return;
        };
        if editor.bg_loading
            || editor.large_file_mode
            || editor.file_path.as_deref() != Some(file_path)
        {
            return;
        }
        if editor.encoding == "Binary" {
            self.reject_binary_diff();
            return;
        }

        let target = crate::background::GitCompareTarget {
            generation: self.git_compare_generation,
            file_path: file_path.to_path_buf(),
            content_version: editor.content_version,
            len_bytes: editor.rope.len_bytes(),
            line_count: editor.rope.len_lines(),
            last_disk_modified: editor.last_disk_modified,
        };
        let target_rope = editor.rope.clone();
        let staged = self.tabs.get(self.active_tab)
            .and_then(|tab| self.diff_tabs.staged_for_tab(tab));
        let file_name = editor.file_name().to_string();
        let hint = self.tabs.get(self.active_tab).and_then(|tab| self.diff_tabs.repository_for_tab(tab))
            .map(Path::to_owned).or_else(|| self.repository_hint_for_path(&target.file_path));
        let sender = self.source_control_sender();
        self.bg_git_compare_running = true;
        self.status_message = format!("Preparing diff: {file_name}...");
        if let Some(remote) = self.remote_workspace.clone() {
            self.background.spawn("remote-git-compare", move || {
                let result = (|| -> Result<crate::background::PreparedGitCompare, String> {
                    let (base, target_snapshot, source_revisions) = if let Some(staged) = staged {
                        let sources = remote.git_diff_sources(&target.file_path, hint.as_deref(), staged, None, sender.cancellation()).map_err(|error| error.to_string())?;
                        let snapshot = staged.then(|| ropey::Rope::from_str(sources.index_content()));
                        (sources.base, snapshot, Some(sources.revisions))
                    } else {
                        let sources = remote.git_sources(&target.file_path, hint.as_deref(), false).map_err(|error| error.to_string())?;
                        (sources.head.clone(), None, None)
                    };
                    if sender.is_cancelled() { return Err("Git comparison cancelled".into()); }
                    let is_new_file = staged.is_some() && base.is_none();
                    let base_content = base.unwrap_or_default();
                    let comparison_target = target_snapshot.as_ref().unwrap_or(&target_rope);
                    let diff_lines = crate::app::compute_file_diff(&base_content, &comparison_target.to_string());
                    let render_cache = crate::app::DiffRenderCache::new(&diff_lines);
                    let target_line_markers = crate::app_git_display::target_line_markers_from_compare_diff(&diff_lines, comparison_target.len_lines());
                    Ok(crate::background::PreparedGitCompare { source_revisions, target_snapshot, is_new_file, base_content, file_name, diff_lines, render_cache, target_line_markers })
                })();
                let _ = sender.send(crate::background::BackgroundResult::GitCompare { target, result });
            });
            return;
        }

        self.background.spawn("git-compare", move || {
            if sender.is_cancelled() { return; }
            let result = (|| -> Result<crate::background::PreparedGitCompare, String> {
                let repo = crate::git_repositories::open_file_repository(&target.file_path, hint.as_deref())
                    .map_err(|error| format!("Git repository not found: {error}"))?;
                if sender.is_cancelled() { return Err("Git comparison cancelled".into()); }
                let workdir = repo
                    .workdir()
                    .ok_or_else(|| "Bare repositories are not supported".to_string())?;
                let relative =
                    repository_relative_path(workdir, &target.file_path).ok_or_else(|| {
                        format!(
                            "{} is not inside the repository worktree",
                            target.file_path.display()
                        )
                    })?;
                let (base_content, target_snapshot, source_revisions) = if let Some(staged) = staged {
                    let sources = crate::git_diff_sources::read(&repo, &relative, staged, None)?;
                    let snapshot = staged.then(|| ropey::Rope::from_str(sources.index_content()));
                    (sources.base, snapshot, Some(sources.revisions))
                } else {
                    (head_file_content(&repo, &relative, false)?, None, None)
                };
                let is_new_file = staged.is_some() && base_content.is_none();
                let base_content = base_content.unwrap_or_default();
                let comparison_target = target_snapshot.as_ref().unwrap_or(&target_rope);
                if sender.is_cancelled() { return Err("Git comparison cancelled".into()); }
                let target_content = comparison_target.to_string();
                let diff_lines =
                    crate::app::compute_file_diff(base_content.as_ref(), &target_content);
                let render_cache = crate::app::DiffRenderCache::new(&diff_lines);
                let target_line_markers =
                    crate::app_git_display::target_line_markers_from_compare_diff(
                        &diff_lines,
                        comparison_target.len_lines(),
                    );
                let file_name = relative
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| target.file_path.display().to_string());

                Ok(crate::background::PreparedGitCompare {
                    source_revisions,
                    target_snapshot,
                    is_new_file,
                    base_content,
                    file_name,
                    diff_lines,
                    render_cache,
                    target_line_markers,
                })
            })();

            // Error OK: receiver dropped means the application has exited.
            let _ = sender.send(crate::background::BackgroundResult::GitCompare { target, result });
        });
    }

    pub(crate) fn git_commit(&mut self) {
        if !self.source_control_enabled { return; }
        if self.bg_git_action_running || self.bg_git_remote_running {
            self.notify("Another git operation is already running");
            return;
        }
        if self.source_control_commit_msg.trim().is_empty() {
            self.notify("Commit message cannot be empty");
            return;
        }
        if !self.source_control_files.iter().any(|f| f.staged) {
            self.sc_stage_all_commit_request = Some(GitCommitRequest {
                root: self.source_control_root().to_owned(),
                message: self.source_control_commit_msg.clone(),
            });
            self.modal = crate::ui_modal::ModalState::default();
            return;
        }
        let message = self.source_control_commit_msg.clone();
        self.queue_git_task("git-commit", "commit", crate::git_tasks::Task::Commit { message, stage_all: false });
    }

    pub(crate) fn confirm_stage_all_commit(&mut self) {
        let Some(request) = self.sc_stage_all_commit_request.take() else {
            return;
        };
        if !self.current_repository_matches(&request.root) {
            self.notify("Workspace changed; please try Commit again");
            return;
        }
        self.queue_git_task("git-stage-and-commit", "stage and commit", crate::git_tasks::Task::Commit { message: request.message, stage_all: true });
    }

    /// Auto-save the current file if OnFocusChange mode is active.
    /// Call this before switching away from the current tab.
    pub(crate) fn auto_save_on_focus_change(&mut self) {
        if !matches!(self.auto_save, AutoSaveMode::OnFocusChange) {
            return;
        }
        self.queue_auto_save_for_modified_tabs();
    }

    /// Auto-save when the terminal window loses focus. OnFocusChange also applies
    /// because the active editor necessarily loses focus with its window.
    pub(crate) fn auto_save_on_window_change(&mut self) {
        if !matches!(
            self.auto_save,
            AutoSaveMode::OnFocusChange | AutoSaveMode::OnWindowChange
        ) {
            return;
        }
        self.queue_auto_save_for_modified_tabs();
    }

    fn queue_auto_save_for_modified_tabs(&mut self) {
        let trim = self.trim_trailing_on_save;
        let newline = self.insert_final_newline;
        let paste_document = self.pending_paste_document();
        let mut bg_jobs: Vec<crate::editor::BackgroundSaveRequest> = Vec::new();
        for tab in &self.tabs {
            if tab.editor.modified && tab.editor.file_path.is_some() && paste_document != Some(tab.editor.save_identity) {
                // Always use background save to avoid blocking UI on focus change
                if let Some(job) = tab.editor.background_save_request(trim, newline, false) {
                    bg_jobs.push(job);
                }
            }
        }
        for group in &self.groups {
            for tab in &group.tabs {
                if tab.editor.modified && tab.editor.file_path.is_some() && paste_document != Some(tab.editor.save_identity) {
                    if let Some(job) = tab.editor.background_save_request(trim, newline, false) {
                        bg_jobs.push(job);
                    }
                }
            }
        }
        for job in bg_jobs {
            self.queue_background_save_job(job);
        }
    }

    pub(crate) fn find_editor_mut_by_path(&mut self, path: &Path) -> Option<&mut crate::editor::Editor> {
        let active_matches = self.editor().and_then(|editor| editor.file_path.as_deref())
            .is_some_and(|other| Self::same_open_path(other, path));
        if active_matches {
            return self.editor_mut();
        }

        if let Some(tab) = self.tabs.iter_mut().find(|tab| {
            tab.kind == crate::app::TabKind::File && tab.editor.file_path.as_deref().is_some_and(|other| Self::same_open_path(other, path))
        }) {
            return Some(&mut tab.editor);
        }

        for group in &mut self.groups {
            if let Some(tab) = group.tabs.iter_mut().find(|tab| {
                tab.kind == crate::app::TabKind::File
                    && tab.editor.file_path.as_deref().is_some_and(|other| Self::same_open_path(other, path))
            }) {
                return Some(&mut tab.editor);
            }
        }

        None
    }

    fn reload_clean_editor_from_disk(&mut self, path: &Path) -> Result<(), String> {
        let matches = |tab: &crate::app::Tab| {
            tab.kind == crate::app::TabKind::File
                && tab.editor.file_path.as_deref() == Some(path)
                && !tab.editor.modified && !tab.editor.bg_loading
        };
        if !self.tabs.iter().chain(self.groups.iter().flat_map(|group| &group.tabs)).any(matches) {
            return Ok(());
        }
        // Read once and share the Rope across all open copies. A queued
        // watcher read may predate the discard, so retire that result too.
        let loaded = crate::editor::load_text_file(path).map_err(|error| error.to_string())?;
        for tab in self.tabs.iter_mut().chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs)) {
            if matches(tab) {
                tab.editor.apply_reloaded_file(loaded.clone());
            }
        }
        self.refresh_pending_file_reload(path.to_path_buf());
        Ok(())
    }

    pub(crate) fn ensure_hunk_actions_ready(&mut self, file: &Path, discard: bool) -> Result<(), String> {
        if !self.source_control_enabled { return Err("Source Control is disabled".into()); }
        if self.bg_git_action_running || self.bg_git_remote_running {
            return Err("Another git operation is already running".into());
        }
        if self.pending_file_saves.contains_key(file) || self.deferred_file_saves.contains_key(file)
            || self.chat.rollback.file_busy(file)
        {
            return Err("Wait for the file operation to finish before using git hunk actions".into());
        }
        if self.should_render_diff_for_active_editor()
            && self.editor().and_then(|editor| editor.file_path.as_deref()).is_some_and(|other| Self::same_open_path(other, file))
        {
            return Err("Hunk actions are unavailable in compare view".to_string());
        }
        for tab in self.tabs.iter().chain(self.groups.iter().flat_map(|group| &group.tabs))
            .filter(|tab| tab.kind == crate::app::TabKind::File && tab.editor.file_path.as_deref().is_some_and(|other| Self::same_open_path(other, file)))
        {
            let editor = &tab.editor;
            if editor.modified {
                return Err("Save the file before using git hunk actions".to_string());
            }
            if editor.bg_loading {
                return Err("Wait for the file to finish loading before using git hunk actions".into());
            }
            if discard && editor.read_only {
                return Err("Cannot discard a hunk while the file is read-only".into());
            }
        }
        if self.remote_workspace.is_none() && !self.git_file_is_trackable(file)? {
            return Err("Hunk actions are unavailable for untracked files".to_string());
        }
        Ok(())
    }

    fn git_hunk_label(file: &Path) -> String {
        file.file_name()
            .and_then(|name| name.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| file.display().to_string())
    }

    fn git_repo_root_and_rel_path(
        &mut self,
        file: &Path,
    ) -> Result<(std::path::PathBuf, std::path::PathBuf), String> {
        let hint = self.repository_hint_for_path(file);
        let repo = crate::git_repositories::open_file_repository(file, hint.as_deref())?;
        let workdir = repo
            .workdir()
            .ok_or_else(|| "Bare repositories are not supported".to_string())?
            .to_path_buf();
        let rel_path = repository_relative_path(&workdir, file)
            .ok_or_else(|| format!("{} is not inside the repository worktree", file.display()))?;
        Ok((workdir, rel_path))
    }

    fn git_file_is_trackable(&mut self, file: &Path) -> Result<bool, String> {
        let hint = self.repository_hint_for_path(file);
        let repo = crate::git_repositories::open_file_repository(file, hint.as_deref())?;
        let workdir = repo
            .workdir()
            .ok_or_else(|| "Bare repositories are not supported".to_string())?
            .to_path_buf();
        let rel_path = repository_relative_path(&workdir, file)
            .ok_or_else(|| format!("{} is not inside the repository worktree", file.display()))?;
        let in_index = repo
            .index()
            .ok()
            .and_then(|index| index.get_path(&rel_path, 0))
            .is_some();
        let in_head = repo
            .head()
            .ok()
            .and_then(|head| head.peel_to_tree().ok())
            .and_then(|tree| tree.get_path(&rel_path).ok())
            .is_some();
        Ok(in_index || in_head)
    }

    pub(crate) fn run_git_capture(repo_root: &Path, args: &[String]) -> Result<String, String> {
        let output = crate::git_repositories::git_command(repo_root)?
            .args(args)
            .output()
            .map_err(|err| format!("Failed to run git: {err}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Err(if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                "git command failed".to_string()
            })
        }
    }

    pub(crate) fn run_git_apply(repo_root: &Path, args: &[String], patch: &str) -> Result<(), String> {
        let mut child = crate::git_repositories::git_command(repo_root)?
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("Failed to run git apply: {err}"))?;
        let write_result = if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(patch.as_bytes())
                .map_err(|err| format!("Failed to write git patch: {err}"))
        } else {
            Ok(())
        };
        let output = child
            .wait_with_output()
            .map_err(|err| format!("Failed to wait for git apply: {err}"))?;
        write_result?;
        if output.status.success() {
            Ok(())
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            Err(if !stderr.is_empty() {
                stderr
            } else if !stdout.is_empty() {
                stdout
            } else {
                "git apply failed".to_string()
            })
        }
    }

    fn git_diff_patch_text(
        &mut self,
        file: &Path,
        cached: bool,
    ) -> Result<(std::path::PathBuf, String), String> {
        let (repo_root, rel_path) = self.git_repo_root_and_rel_path(file)?;
        let rel_arg = rel_path.to_string_lossy().replace('\\', "/");
        let mut args = vec![
            "--literal-pathspecs".to_string(),
            "diff".to_string(),
            "--no-ext-diff".to_string(),
            "--no-textconv".to_string(),
            "--no-color".to_string(),
            "--src-prefix=a/".to_string(),
            "--dst-prefix=b/".to_string(),
            "--inter-hunk-context=0".to_string(),
            "--unified=0".to_string(),
        ];
        if cached {
            args.push("--cached".to_string());
        }
        args.push("--".to_string());
        args.push(rel_arg);
        let patch = Self::run_git_capture(&repo_root, &args)?;
        Ok((repo_root, patch))
    }

    pub(crate) fn load_hunk_peek(&mut self) -> Result<crate::ui_hunk_peek::HunkPeek, String> {
        if self.hunk_popup_status == 'N' {
            // Unsaved markers compare the live buffer with its saved snapshot.
            // This path needs neither Git nor a disk read, including if the
            // file was removed/changed externally after it was opened.
            let editor = self.editor()
                .filter(|editor| editor.file_path == self.hunk_popup_file)
                .ok_or("The document is no longer open")?;
            let saved = editor.saved_rope.as_ref().ok_or("Saved version is unavailable")?;
            let mut peek = crate::ui_hunk_peek::HunkPeek::from_texts(
                &saved.to_string(), &editor.rope.to_string(), self.hunk_popup_line, editor.tab_size,
            )?;
            peek.unsaved = Some(crate::ui_hunk_peek::UnsavedHunkSnapshot {
                document: editor.navigation_id,
                version: editor.content_version,
                saved: saved.clone(),
                current: editor.rope.clone(),
            });
            return Ok(peek);
        }
        let file = self.hunk_popup_file.clone().ok_or("No file selected")?;
        let staged = self.hunk_popup_status == 'S';
        let live = self.find_editor_mut_by_path(&file)
            .filter(|editor| editor.modified).map(|editor| editor.rope.clone());
        let (repo_root, patch_text) = if !staged && live.is_some() {
            // The disk diff is obsolete when the editor has unsaved text.
            (self.git_repo_root_and_rel_path(&file)?.0, String::new())
        } else {
            self.git_diff_patch_text(&file, staged)?
        };
        let repo = git2::Repository::open(&repo_root).map_err(|err| err.to_string())?;
        let relative = repository_relative_path(&repo_root, &file)
            .ok_or("File is outside the repository worktree")?;
        if repo.status_file(&relative).map_err(|err| err.to_string())?.is_ignored() {
            return Err("No changes found".to_string());
        }
        let (index, worktree) = self.read_index_and_worktree_text(&file, live.as_ref())?;
        let tab_size = self.find_editor_mut_by_path(&file).map_or(4, |editor| editor.tab_size);
        if !staged && (live.is_some() || (patch_text.is_empty() && !self.git_file_is_trackable(&file)?)) {
            return crate::ui_hunk_peek::HunkPeek::from_texts(
                &index, &worktree, self.hunk_popup_line, tab_size,
            );
        }
        let patch = parse_git_file_patch(&patch_text).ok_or("No changes found")?;
        let target = if staged { &index } else { &worktree };
        // Compute the index/worktree correspondence once, rather than diffing
        // the complete file again for every staged hunk's navigation anchor.
        let equal_ranges: Vec<_> = if staged {
            similar::TextDiff::from_lines(&index, &worktree)
                .ops()
                .iter()
                .filter_map(|op| match *op {
                    similar::DiffOp::Equal { old_index, new_index, len } =>
                        Some((old_index, new_index, len)),
                    _ => None,
                })
                .collect()
        } else {
            Vec::new()
        };
        let line = if staged {
            equal_ranges.iter().find_map(|&(old, new, len)| {
                (self.hunk_popup_line >= new && self.hunk_popup_line < new + len)
                    .then(|| old + self.hunk_popup_line - new)
            })
                .unwrap_or(self.hunk_popup_line)
        } else {
            self.hunk_popup_line
        };
        let mut peek = crate::ui_hunk_peek::HunkPeek::for_line(&patch, target, line, tab_size)?;
        for hunk in &mut peek.hunks {
            if staged {
                hunk.action_line = hunk.action_line.and_then(|line| {
                    let range = equal_ranges.partition_point(|&(old, _, len)| old + len <= line);
                    equal_ranges.get(range).and_then(|&(old, new, _)|
                        (line >= old).then(|| new + line - old))
                });
            }
        }
        // Keep the exact clicked worktree line for the initially selected action.
        peek.hunks[peek.selected].action_line = Some(self.hunk_popup_line);
        Ok(peek)
    }

    /// Restore only the previewed saved-buffer hunk. Snapshot identity makes
    /// validation constant-time, and the edit copies only the restored lines:
    /// no disk/index writes, Git process, or second whole-document diff.
    pub(crate) fn revert_unsaved_hunk(&mut self) -> Result<(), String> {
        let stale = "The document changed since this preview. Reopen the hunk and try again.";
        let peek = self.hunk_peek.as_ref()
            .filter(|_| self.hunk_popup_active && self.hunk_popup_status == 'N')
            .ok_or("No unsaved change selected")?;
        let snapshot = peek.unsaved.as_ref().ok_or(stale)?;
        let hunk = peek.source.hunks.get(peek.selected).ok_or(stale)?;
        let editor = self.editor().ok_or(stale)?;
        if editor.navigation_id != snapshot.document
            || editor.file_path != self.hunk_popup_file
            || editor.content_version != snapshot.version
            || !editor.rope.is_instance(&snapshot.current)
            || !editor.saved_rope.as_ref().is_some_and(|saved| saved.is_instance(&snapshot.saved))
        {
            return Err(stale.into());
        }
        if editor.read_only || editor.bg_loading {
            return Err("The document is read-only or still loading; the hunk was not reverted.".into());
        }
        let range = |rope: &ropey::Rope, start: usize, count: usize| {
            // Zero-line hunks refer to an insertion boundary, not the line
            // before it. Rope ranges also retain CRLF and missing final LF.
            let start = if count == 0 { start } else { start.saturating_sub(1) };
            let end = start.checked_add(count).ok_or(stale)?;
            Ok::<_, &str>(rope.try_line_to_char(start).map_err(|_| stale)?
                ..rope.try_line_to_char(end).map_err(|_| stale)?)
        };
        let original = range(&snapshot.saved, hunk.old_start, hunk.old_count)?;
        let current = range(&snapshot.current, hunk.new_start, hunk.new_count)?;
        let replacement = snapshot.saved.slice(original).to_string();
        let inserted = replacement.chars().count();
        let previous_lines = editor.line_count();
        let dirty_from = editor.rope.char_to_line(current.start);
        let dirty_through = editor.rope.char_to_line(current.end);
        let offset = |(line, col): (usize, usize)| {
            let line = line.min(editor.line_count() - 1);
            let offset = editor.rope.line_to_char(line) + col.min(editor.line_len(line));
            if offset <= current.start { offset }
            else if offset < current.end { current.start }
            else { current.start + inserted + (offset - current.end) }
        };
        let cursor = offset((editor.cursor_line, editor.cursor_col));
        let extra: Vec<_> = editor.extra_cursors.iter().copied().map(offset).collect();
        let editor = self.editor_mut().ok_or(stale)?;
        editor.save_undo_public();
        editor.mark_syntax_dirty_range(dirty_from, dirty_through);
        editor.clear_selection();
        editor.snippet_cancel();
        editor.extra_cursor_anchors.clear();
        editor.desired_col = None;
        editor.rope.remove(current.clone());
        editor.rope.insert(current.start, &replacement);
        if editor.line_count() != previous_lines {
            editor.clear_stale_folds();
        }
        editor.modified = true;
        editor.mark_syntax_dirty_range(dirty_from,
            dirty_through.max(editor.rope.char_to_line(current.start + inserted)));
        // Reverting the last change is clean without advancing the disk
        // checkpoint; pending encoding changes still require saving.
        editor.refresh_modified_from_saved();
        let position = |offset| {
            let line = editor.rope.char_to_line(offset);
            (line, offset - editor.rope.line_to_char(line))
        };
        (editor.cursor_line, editor.cursor_col) = position(cursor);
        editor.extra_cursors = extra.into_iter().map(position).collect();
        editor.clamp_cursor();
        editor.ensure_visible();
        self.sync_duplicate_file_tabs();
        Ok(())
    }

    fn selected_unstaged_hunk_patch(
        &mut self,
        file: &Path,
        line: usize,
    ) -> Result<(std::path::PathBuf, String), String> {
        let (repo_root, patch_text) = self.git_diff_patch_text(file, false)?;
        let patch = parse_git_file_patch(&patch_text)
            .ok_or_else(|| format!("No unstaged hunks found in {}", file.display()))?;
        if let Some(selected) = self.previewed_hunk_patch(file, &patch)? {
            return Ok((repo_root, selected));
        }
        let line_number = line.saturating_add(1);
        let selected = select_patch_hunk(&patch, line_number, true)
            .ok_or_else(|| format!("No unstaged hunk found at line {line_number}"))?;
        Ok((repo_root, selected))
    }

    fn previewed_hunk_patch(
        &self,
        file: &Path,
        patch: &crate::app_git_display::GitFilePatch,
    ) -> Result<Option<String>, String> {
        let Some(peek) = self.hunk_peek.as_ref().filter(|_| {
            self.hunk_popup_active && self.hunk_popup_file.as_deref() == Some(file)
        }) else { return Ok(None); };
        let expected = &peek.source.hunks[peek.selected];
        if patch.header != peek.source.header || !patch.hunks.contains(expected) {
            return Err("The file or Git index changed since this preview. Reopen the hunk and try again.".into());
        }
        // Use the selected preview hunk, not an ambiguous deletion boundary
        // or a staged line that may no longer map into the worktree.
        Ok(Some(format!("{}{}", patch.header, expected.text)))
    }

    fn read_index_and_worktree_text(
        &mut self,
        file: &Path,
        live: Option<&ropey::Rope>,
    ) -> Result<(String, String), String> {
        let hint = self.repository_hint_for_path(file);
        let repo = crate::git_repositories::open_file_repository(file, hint.as_deref())?;
        let workdir = repo
            .workdir()
            .ok_or_else(|| "Bare repositories are not supported".to_string())?
            .to_path_buf();
        let rel_path = repository_relative_path(&workdir, file)
            .ok_or_else(|| format!("{} is not inside the repository worktree", file.display()))?;

        let index_text = if let Ok(index) = repo.index() {
            if let Some(entry) = index.get_path(&rel_path, 0) {
                if let Ok(blob) = repo.find_blob(entry.id) {
                    String::from_utf8_lossy(blob.content()).into_owned()
                } else {
                    String::new()
                }
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        let worktree_text = match live {
            Some(rope) => rope.to_string(),
            None => std::fs::read_to_string(file)
                .map_err(|err| format!("Failed to read {}: {err}", file.display()))?,
        };
        // Git's index commonly uses LF while a Windows buffer uses CRLF.
        // Match the gutter's line comparison without treating every line as
        // changed or losing the index/buffer mapping for staged hunks.
        let normalize = |text: String| {
            if text.contains("\r\n") { text.replace("\r\n", "\n") } else { text }
        };
        Ok((normalize(index_text), normalize(worktree_text)))
    }

    fn selected_staged_hunk_patch(
        &mut self,
        file: &Path,
        line: usize,
    ) -> Result<(std::path::PathBuf, String), String> {
        let (repo_root, patch_text) = self.git_diff_patch_text(file, true)?;
        let patch = parse_git_file_patch(&patch_text)
            .ok_or_else(|| format!("No staged hunks found in {}", file.display()))?;
        if let Some(selected) = self.previewed_hunk_patch(file, &patch)? {
            return Ok((repo_root, selected));
        }
        let (index_text, worktree_text) = self.read_index_and_worktree_text(file, None)?;
        let worktree_line_number = line.saturating_add(1);
        let mapped_line_number = map_worktree_line_to_index_line(&index_text, &worktree_text, line)
            .map(|mapped| mapped.saturating_add(1))
            .unwrap_or(worktree_line_number);
        let selected = select_patch_hunk(&patch, mapped_line_number, true)
            .or_else(|| select_patch_hunk(&patch, worktree_line_number, true))
            .ok_or_else(|| format!("No staged hunk found at line {worktree_line_number}"))?;
        Ok((repo_root, selected))
    }

    pub(crate) fn save_open_file_by_path(&mut self, path: &Path) -> Result<(), String> {
        let trim = self.trim_trailing_on_save;
        let newline = self.insert_final_newline;
        let paste_document = self.pending_paste_document();
        let editor = self
            .find_editor_mut_by_path(path)
            .ok_or_else(|| format!("No open editor for {}", path.display()))?;
        if paste_document == Some(editor.save_identity) {
            return Err("Paste is still in progress; save after it finishes or cancel it first".into());
        }
        if let Some(error) = editor.save_content_error() { return Err(error.into()); }
        let request = editor.background_save_request(trim, newline, false)
            .ok_or_else(|| format!("No file to save: {}", path.display()))?;
        if !self.queue_background_save_job(request) {
            return Err("The save could not start. Your edits remain unsaved; wait for the current file operation to finish.".into());
        }
        self.notify("Saving...");
        self.status_message = String::from("Saving...");
        self.saving_flash = Some(std::time::Instant::now());
        Ok(())
    }

    pub(crate) fn save_current_file(&mut self) {
        if self.editor().is_some_and(|editor| self.pending_paste_document() == Some(editor.save_identity)) {
            self.notify_warn("Paste is still being prepared; save after it finishes or cancel it first");
            return;
        }
        if self.dispatch_extension_editor_command("save") { return; }
        // A different pane may have saved/edited this shared document since
        // this view last received input.
        let _ = self.editor_mut();
        if self
            .tabs
            .get(self.active_tab)
            .is_some_and(|tab| tab.kind != crate::app::TabKind::File)
        {
            self.notify("This view cannot be saved");
            return;
        }
        if self.editor().is_some_and(|editor| self.pending_save_as.contains_key(&editor.save_identity)) {
            self.notify("Save As is still in progress. New edits remain unsaved until you save again.");
            return;
        }
        if self.editor().and_then(|e| e.file_path.as_ref()).is_none() {
            self.save_as_active = true;
            self.save_as_query = self
                .explorer
                .root
                .join(self.editor().and_then(|e| e.suggested_file_name.as_deref()).unwrap_or("untitled.txt"))
                .to_string_lossy()
                .to_string();
            if let Some(remote) = &self.remote_workspace {
                self.save_as_query = remote.initial_save_path(self.editor().and_then(|editor| editor.suggested_file_name.as_deref()));
            }
            self.save_as_cursor = self.save_as_query.len();
            self.save_as_selection_anchor = None;
            self.notify("Choose a file path to save");
            return;
        }
        // The common clean Ctrl+S path needs no request, worker or even a path
        // allocation. Cleanup options are cached against the saved checkpoint.
        if self.editor().is_some_and(|editor| editor.file_path.as_deref().is_some_and(|path| {
            crate::remote::workspace::is_view_key(path) && !editor.modified
                && editor.save_content_error().is_none() && !editor.disk_unavailable
                && editor.remote_checkpoint.as_ref().is_some_and(|checkpoint| !checkpoint.conflicted)
                && editor.saved_rope.is_some()
                && editor.saved_save_options == Some(editor.save_options(self.trim_trailing_on_save, self.insert_final_newline))
                && !self.pending_file_saves.contains_key(path) && !self.deferred_file_saves.contains_key(path)
                && !self.remote_change_warnings.contains(path)
        })) { return; }
        let path = self
            .editor()
            .and_then(|editor| editor.file_path.clone())
            .expect("active editor must have a file_path: None case returns early above");

        if crate::remote::workspace::is_view_key(&path) {
            if self.pending_file_saves.contains_key(&path)
                && let Some(request) = self.editor().and_then(|editor| editor.background_save_request(self.trim_trailing_on_save, self.insert_final_newline, false))
                && self.coalesce_identical_save(&request, self.workspace_generation) { return; }
        }

        // Formatting is work on edited text. A clean remote document can still
        // need newly enabled whitespace/newline cleanup, prepared locally below.
        if self.format_on_save && (!crate::remote::workspace::is_view_key(&path) || self.editor().is_some_and(|editor| editor.modified)) && self.request_format(false, true) {
            return;
        }

        if let Err(err) = self.save_open_file_by_path(&path) {
            self.notify_error(format!("Error: {err}"));
            self.status_message = format!("Error saving: {err}");
        }
    }

    pub(crate) fn queue_background_save_job(
        &mut self,
        request: crate::editor::BackgroundSaveRequest,
    ) -> bool {
        self.queue_background_save_job_for_workspace(request, self.workspace_generation)
    }

    pub(crate) fn queue_background_save_job_for_workspace(
        &mut self,
        request: crate::editor::BackgroundSaveRequest,
        workspace_generation: u64,
    ) -> bool {
        if workspace_generation == self.workspace_generation && self.pending_paste_document() == Some(request.source_document_id) {
            return false;
        }
        self.queue_background_save_intent(request, workspace_generation, false)
    }

    pub(crate) fn queue_background_overwrite(&mut self, request: crate::editor::BackgroundSaveRequest) -> bool {
        self.queue_background_save_intent(request, self.workspace_generation, true)
    }

    fn coalesce_identical_save(&mut self, request: &crate::editor::BackgroundSaveRequest, generation: u64) -> bool {
        if self.pending_file_saves.get(&request.path).is_some_and(|(current, intent)| *current == generation && intent.matches(request)) {
            // Latest intent wins even if the user changed options and then
            // changed them back while the original write was still pending.
            self.deferred_file_saves.remove(&request.path);
            return true;
        }
        self.deferred_file_saves.get(&request.path).is_some_and(|(current, previous)| *current == generation && previous.same_intent(request))
    }

    fn queue_background_save_intent(
        &mut self,
        request: crate::editor::BackgroundSaveRequest,
        workspace_generation: u64,
        overwrite: bool,
    ) -> bool {
        if overwrite && (self.pending_file_saves.contains_key(&request.path) || self.deferred_file_saves.contains_key(&request.path)) {
            self.notify("Wait for this file's pending save before choosing Overwrite.");
            return false;
        }
        if crate::remote::workspace::is_view_key(&request.path) {
            if self.remote_workspace.as_ref().is_none_or(|remote| !remote.client.connected()) {
                self.notify_warn("Remote connection is unavailable. Your edits remain local; save again after reconnecting.");
                return false;
            }
            if self.remote_connection.checking.contains(&request.path) {
                self.notify("Checking this file after reconnecting. Your edits remain local; save again when the check finishes.");
                return false;
            }
        }
        if self.pending_save_as.contains_key(&request.source_document_id)
            || self.pending_save_as.values().any(|(_, target)| Self::same_open_path(target, &request.path))
        {
            self.notify("Save As is still in progress. New edits remain unsaved until you save again.");
            return false;
        }
        if self.chat.rollback.mutating {
            self.notify_error("Wait for checkpoint capture or rollback before saving. Your edits remain in the buffer.");
            return false;
        }
        if self.chat.rollback.file_writes.load(std::sync::atomic::Ordering::Acquire) != 0 {
            self.notify_error("Wait for the agent file write before saving. Your edits remain in the buffer.");
            return false;
        }
        if !overwrite && self.coalesce_identical_save(&request, workspace_generation) { return true; }
        self.chat.rollback.note_user_save(&request.path);
        // A reload started before this save must never restore its older disk
        // snapshot after the save makes the editor clean.
        self.refresh_pending_file_reload(request.path.clone());
        if workspace_generation == self.workspace_generation {
            // Typing after a save request needs its own undo group even when
            // the write is slow or queued behind another save. This retains
            // the exact snapshot that will become the new disk checkpoint.
            for tab in self.tabs.iter_mut().chain(self.groups.iter_mut().flat_map(|group| group.tabs.iter_mut())) {
                if tab.editor.file_path.as_ref() == Some(&request.path)
                    && tab.editor.content_version == request.source_content_version
                    && tab.editor.rope.is_instance(&request.rope)
                {
                    tab.editor.break_undo_group();
                }
            }
        }
        let path = request.path.clone();
        if self.pending_file_saves.contains_key(&path) {
            // A later explicit Save supersedes the queued request even when
            // text versions match: encoding and save options may have changed,
            // or the document may have been closed and reopened in between.
            self.deferred_file_saves.insert(path, (workspace_generation, request));
            return true;
        }
        let request = std::sync::Arc::new(request);
        self.pending_file_saves
            .insert(path.clone(), (workspace_generation, crate::editor::SaveIntent::new(request.clone())));
        let sender = self.background.sender.clone();
        let activity_path = path.clone();
        let remote = self.remote_workspace.clone().filter(|_| crate::remote::workspace::is_view_key(&path));
        let mut known_metadata = None;
        let allow_noop = !overwrite && !self.remote_save_needs_validation(&path);
        let mut checkpoint = remote.as_ref().and_then(|_| self.tabs.iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .find(|tab| tab.editor.save_identity == request.source_document_id)
            .and_then(|tab| {
                if allow_noop && !tab.editor.disk_unavailable { known_metadata = Some((tab.editor.file_size, tab.editor.last_disk_modified)); }
                Some((tab.editor.remote_checkpoint.clone()?, tab.editor.saved_rope.clone()?, tab.editor.saved_encoding))
            }));
        self.background.save_for_file(&activity_path, move || {
            let mut request = request;
            // Rebase deferred saves against the editor's previous checkpoint,
            // separately from the fresh disk snapshot an overwrite validates.
            let previous_disk = request.expected_disk.clone();
            let source_content_version = request.source_content_version;
            let mut remote_size = None;
            let mut remote_checkpoint = None;
            let mut remote_modified = None;
            let result = (|| {
                if overwrite { checkpoint = crate::app_save_conflict::prepare_overwrite(std::sync::Arc::make_mut(&mut request), remote.as_ref())?; }
                match remote {
                    Some(remote) => remote.save_progress(&request, checkpoint, known_metadata,
                        crate::remote::text::progress(sender.clone(), workspace_generation, path.clone(), true)).map(|saved| {
                        remote_size = Some(saved.size);
                        remote_checkpoint = Some(saved.checkpoint);
                        remote_modified = saved.modified;
                        crate::editor::save::SavedFile { rope: saved.rope, metadata: None, transformed_lines: saved.transformed_lines, disk_written: saved.disk_written }
                    }),
                    None => crate::editor::write_background_save(&request),
                }
            })();
            let transformed_lines = result.as_ref().ok().and_then(|saved| saved.transformed_lines);
            let disk_written = result.as_ref().is_ok_and(|saved| saved.disk_written);
            let metadata = result.as_ref().ok().and_then(|saved| saved.metadata.as_ref());
            let last_disk_modified = metadata
                .and_then(|metadata| metadata.modified().ok()).or(remote_modified);
            let file_size = remote_size.or_else(|| metadata.map(std::fs::Metadata::len));
            let (saved_rope, error) = match result {
                Ok(saved) => (Some(saved.rope), None),
                Err(error) => (None, Some(error)),
            };
            let result = crate::background::BackgroundResult::FileSaved {
                source_document_id: request.source_document_id,
                workspace_generation,
                path,
                error,
                last_disk_modified,
                file_size,
                source_content_version,
                source_rope: request.rope.clone(),
                previous_disk,
                saved_encoding: request.encoding,
                transformed_lines,
                disk_written,
                saved_rope,
            };
            let result = match remote_checkpoint {
                Some(checkpoint) => crate::background::BackgroundResult::RemoteFileResult { checkpoint, result: Box::new(result) },
                None => result,
            };
            let _ = sender.send(result); // Receiver may have closed.
        });
        true
    }

    /// Refresh branch/ahead-behind state only after an explicit repository
    /// invalidation (startup, folder switch, Git action, or `.git` watcher
    /// event). Stable workspaces never run a branch timer.
    pub(crate) fn refresh_git_branch_async(&mut self) {
        if self.remote_workspace.is_some() { return; }
        if !self.source_control_enabled { return; }
        if self.repositories.discovered && self.repositories.active.is_none() { return; }
        if self.bg_git_branch_running {
            self.bg_git_branch_pending = true;
            return;
        }
        self.bg_git_branch_running = true;
        let root = self.source_control_root().to_owned();
        let sender = self.repository_sender();
        self.background.spawn("git-branch", move || {
            if sender.is_cancelled() { return; }
            let result = git2::Repository::open(&root)
                .ok()
                .and_then(|repo| {
                    if sender.is_cancelled() { return None; }
                    let head = repo.head().ok()?;
                    let branch_name = head.shorthand().map(String::from)?;
                    let local_oid = head.target()?;
                    let ahead_behind = repo
                        .find_branch(&branch_name, git2::BranchType::Local)
                        .ok()
                        .and_then(|branch| branch.upstream().ok())
                        .and_then(|upstream| upstream.get().target())
                        .and_then(|remote_oid| repo.graph_ahead_behind(local_oid, remote_oid).ok());
                    let sync_info = match ahead_behind {
                        Some((ahead, behind)) if ahead > 0 && behind > 0 => {
                            format!(" ↑{ahead} ↓{behind}")
                        }
                        Some((ahead, 0)) if ahead > 0 => format!(" ↑{ahead}"),
                        Some((0, behind)) if behind > 0 => format!(" ↓{behind}"),
                        _ => String::new(),
                    };
                    Some(format!("{branch_name}{sync_info}"))
                })
                .unwrap_or_default();
            let _ = sender.send(crate::background::BackgroundResult::GitBranch(result));
        });
    }

    /// Async version: compute git line diff in background thread.
    pub(crate) fn refresh_git_line_status_async(&mut self) {
        if !self.source_control_enabled { return; }
        if self.active_diff_uses_snapshot_target() {
            return;
        }
        let Some(editor) = self.editor() else {
            return;
        };
        if editor.bg_loading || editor.large_file_mode {
            return;
        }
        let Some(file_path) = editor.file_path.clone() else {
            return;
        };
        let line_count = editor.rope.len_lines();
        let saved_snapshot = editor.saved_rope.clone().unwrap_or_else(|| editor.rope.clone());
        if self.bg_git_diff_running {
            self.bg_git_diff_pending = true;
            return;
        }

        self.bg_git_diff_running = true;
        let hint = self.repository_hint_for_path(&file_path);
        let sender = self.source_control_sender();
        let log = self.git_output();
        if let Some(remote) = self.remote_workspace.clone() {
            self.background.spawn("remote-git-line-diff", move || {
                let result = remote.git_sources(&file_path, hint.as_deref(), false).ok().filter(|sources| !sources.ignored).map(|sources| {
                    let head = sources.head.as_deref().unwrap_or_default().lines().collect::<Vec<_>>();
                    let index = sources.index.as_deref().unwrap_or_default().lines().collect::<Vec<_>>();
                    let saved = saved_snapshot.to_string();
                    let working = saved.lines().collect::<Vec<_>>();
                    (compute_line_diff(&head, &index), compute_line_diff(&index, &working))
                });
                let (staged, unstaged) = result.unwrap_or_default();
                let status = crate::editor::PreparedGitLineStatus::from_diffs(line_count, &staged, &unstaged);
                let _ = sender.send(crate::background::BackgroundResult::GitLineDiff { file_path, status });
            });
            return;
        }
        self.background.spawn("git-line-diff", move || {
            if sender.is_cancelled() { return; }
            let operation = crate::git_output::native(&log, "Read line diff", &file_path);
            let result = (|| -> Option<(Vec<char>, Vec<char>)> {
                let repo = crate::git_repositories::open_file_repository(&file_path, hint.as_deref()).ok()?;
                if sender.is_cancelled() { return None; }
                let workdir = repo.workdir()?.to_path_buf();
                let rel_path = repository_relative_path(&workdir, &file_path)?;

                // Missing HEAD/index content also occurs for ignored files.
                // Only genuine untracked files should diff against an empty base.
                // Git status keeps tracked files visible even if an ignore rule matches.
                if repo.status_file(&rel_path).ok()?.is_ignored() {
                    return None;
                }
                if sender.is_cancelled() { return None; }

                let head_content = (|| -> Option<String> {
                    let head = repo.head().ok()?;
                    let tree = head.peel_to_tree().ok()?;
                    let entry = tree.get_path(&rel_path).ok()?;
                    let blob = repo.find_blob(entry.id()).ok()?;
                    Some(String::from_utf8_lossy(blob.content()).into_owned())
                })();

                let index_content = (|| -> Option<String> {
                    let index = repo.index().ok()?;
                    let entry = index.get_path(&rel_path, 0)?;
                    let blob = repo.find_blob(entry.id).ok()?;
                    Some(String::from_utf8_lossy(blob.content()).into_owned())
                })();

                if sender.is_cancelled() { return None; }
                let worktree_content = std::fs::read_to_string(&file_path).ok()?;
                if sender.is_cancelled() { return None; }

                let head_text = head_content.as_deref().unwrap_or("");
                let index_text = index_content.as_deref().unwrap_or("");
                let head_lines: Vec<&str> = head_text.lines().collect();
                let index_lines: Vec<&str> = index_text.lines().collect();
                let worktree_lines: Vec<&str> = worktree_content.lines().collect();

                let staged = compute_line_diff(&head_lines, &index_lines);
                let unstaged = compute_line_diff(&index_lines, &worktree_lines);
                Some((staged, unstaged))
            })();

            if !sender.is_cancelled() {
                operation.completed(if result.is_some() { "Line changes prepared" } else { "No Git line changes available" }.into());
            }
            if let Some((staged, unstaged)) = result {
                let status = crate::editor::PreparedGitLineStatus::from_diffs(
                    line_count,
                    &staged,
                    &unstaged,
                );
                // Error OK: receiver dropped means no listener
                let _ = sender.send(crate::background::BackgroundResult::GitLineDiff {
                    file_path,
                    status,
                });
            } else {
                // Send an empty prepared status to clear the running flag and
                // any markers left by the previous repository state.
                let status = crate::editor::PreparedGitLineStatus::from_diffs(
                    line_count,
                    &[],
                    &[],
                );
                // Error OK: receiver dropped means no listener
                let _ = sender.send(crate::background::BackgroundResult::GitLineDiff {
                    file_path,
                    status,
                });
            }
        });
    }

    fn commit_tree_file_content(
        repo: &git2::Repository,
        tree: Option<&git2::Tree<'_>>,
        rel_path: &std::path::Path,
    ) -> Result<String, String> {
        let Some(tree) = tree else {
            return Ok(String::new());
        };
        let Ok(entry) = tree.get_path(rel_path) else {
            return Ok(String::new());
        };
        let Ok(blob) = repo.find_blob(entry.id()) else {
            return Ok(String::new());
        };
        diff_blob_content(&blob)
    }

    pub(crate) fn open_source_control_commit_details_file_diff(&mut self, file_idx: usize) {
        let Some(file) = self.commit_details.data.files.get(file_idx).cloned() else {
            return;
        };
        let rel_path_str = file.path;
        let commit_ref = self.commit_details.data.hash.clone();
        let Some(repo) = self.discover_repo() else {
            return;
        };
        let obj = match repo.revparse_single(&commit_ref) {
            Ok(o) => o,
            Err(e) => {
                self.notify_error(format!("Commit lookup failed: {e}"));
                return;
            }
        };
        let commit = match obj.peel_to_commit() {
            Ok(c) => c,
            Err(e) => {
                self.notify_error(format!("Commit read failed: {e}"));
                return;
            }
        };
        let commit_tree = match commit.tree() {
            Ok(t) => t,
            Err(e) => {
                self.notify_error(format!("Commit tree error: {e}"));
                return;
            }
        };
        let parent = commit.parent(0).ok();
        let parent_tree = parent.as_ref().and_then(|p| p.tree().ok());
        let rel_path = std::path::Path::new(&rel_path_str);
        let contents = Self::commit_tree_file_content(&repo, parent_tree.as_ref(), std::path::Path::new(file.old_path.as_deref().unwrap_or(&rel_path_str)))
            .and_then(|parent_content| {
                Self::commit_tree_file_content(&repo, Some(&commit_tree), rel_path)
                    .map(|commit_content| (parent_content, commit_content))
            });
        let (parent_content, commit_content) = match contents {
            Ok(contents) => contents,
            Err(message) => {
                self.status_message = message.clone();
                self.notify(message);
                return;
            }
        };
        let parent_short = parent.as_ref().map_or_else(
            || "∅".to_string(),
            |p| p.id().to_string().chars().take(7).collect::<String>(),
        );
        let syntax_path = repo.workdir().unwrap_or(&self.explorer.root).join(&rel_path_str);
        let language = self.language_override_for_path(&syntax_path);

        self.new_snapshot_diff_tab();
        let large_file_threshold_bytes =
            self.large_file_threshold_mb.saturating_mul(1024 * 1024);
        if let Some(ed) = self.editor_mut() {
            ed.rope = ropey::Rope::from_str(&commit_content);
            ed.file_path = Some(syntax_path.clone());
            ed.language_override = language;
            ed.cursor_line = 0;
            ed.cursor_col = 0;
            ed.scroll_offset = 0;
            ed.wrap_scroll_offset = 0;
            ed.horizontal_scroll = 0;
            ed.selection_anchor = None;
            ed.extra_cursors.clear();
            ed.extra_cursor_anchors.clear();
            ed.read_only = true;
            ed.modified = false;
            ed.file_size = commit_content.len() as u64;
            ed.update_large_file_mode(large_file_threshold_bytes);
            ed.content_version = 0;
            ed.syntax_dirty_from = Some(0);
            ed.syntax_dirty_through = Some(ed.rope.len_lines());
            ed.cached_visible_line_count = None;
            ed.saved_rope = Some(ed.rope.clone());
            ed.detect_indentation();
        }
        self.compare_base_path = None;
        self.compare_base_content = Some(parent_content.clone().into());
        let compare_diff_lines = crate::app::compute_file_diff(&parent_content, &commit_content);
        self.set_compare_diff_lines(compare_diff_lines);
        self.diff_tab_label = Some(format!("{rel_path_str} ({parent_short} ↔ {})", commit.id().to_string().chars().take(7).collect::<String>()));
        self.diff_labels = Some(crate::app::DiffLabels::new(
            if parent.is_some() {
                format!("{parent_short} (parent)")
            } else {
                "Empty tree (no parent)".to_string()
            },
            format!(
                "{} (commit)",
                commit.id().to_string().chars().take(7).collect::<String>()
            ),
        ));
        self.diff_target_path = Some(syntax_path);
        self.diff_mode = self.preferred_diff_mode;
        self.scroll_to_first_diff_change();
        self.notify(format!("Opened commit diff: {rel_path_str}"));
    }

    pub(crate) fn git_amend_commit(&mut self) {
        if self.source_control_commit_msg.trim().is_empty() {
            self.notify("Commit message cannot be empty");
            return;
        }
        if !self.source_control_files.iter().any(|file| file.staged) {
            self.notify("No staged changes to commit");
            return;
        }
        let message = self.source_control_commit_msg.clone();
        self.queue_git_task("git-amend", "amend", crate::git_tasks::Task::Amend { message });
    }

    pub(crate) fn git_stash_save(&mut self) {
        let entered_message = self.source_control_commit_msg.trim().to_string();
        let stash_message = if entered_message.is_empty() {
            "WIP on vtcode".to_string()
        } else {
            entered_message.clone()
        };
        let label = if entered_message.is_empty() {
            "WIP".to_string()
        } else {
            entered_message
        };
        self.queue_git_task("git-stash", "stash", crate::git_tasks::Task::Stash { stash_message, label });
    }

    pub(crate) fn git_stash_pop(&mut self) {
        self.queue_git_task("git-stash-pop", "stash pop", crate::git_tasks::Task::StashPop);
    }

    // format_time_ago moved to app_git_display.rs
}

// Diff display functions moved to app_git_display.rs

#[cfg(test)]
#[path = "app_git_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "app_git_commit_tests.rs"]
mod commit_tests;

#[cfg(test)]
#[path = "source_control_loading_tests.rs"]
mod loading_tests;
