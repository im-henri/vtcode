//! Workspace repository discovery and the independent state of each Git project.
//! Filesystem traversal and status collection run on workers, never while drawing.
use crate::app::{App, GitFileChange, SourceControlRepositoryState};
use crate::background::CancellationToken;
use crate::source_control_rows::{Group, Visibility};
use ratatui::layout::Rect;
use rustc_hash::FxHashSet;
use std::path::{Path, PathBuf};

#[cfg(test)]
#[path = "git_repositories_tests.rs"]
mod tests;

#[derive(Default)]
pub(crate) struct Repositories {
    pub workspace: Option<PathBuf>,
    pub canonical_workspace: Option<PathBuf>,
    pub entries: Vec<Repository>,
    pub active: Option<PathBuf>,
    pub preferred: Option<PathBuf>,
    pub parent: Option<PathBuf>,
    pub parent_open: bool,
    pub closed: FxHashSet<PathBuf>,
    pub discovered: bool,
    pub rescan: bool,
    pub expanded: bool,
    pub header_scroll: usize,
    pub headers: Vec<(Rect, PathBuf)>,
    pub closes: Vec<(Rect, PathBuf)>,
    pub parent_button: Option<Rect>,
    pub reopen_button: Option<Rect>,
    pub detail_area: Option<Rect>,
    pub view_generation: u64,
    pub membership_generation: u64,
    pub reopen_candidates: Vec<PathBuf>,
}

impl Repositories {
    /// Known, closed projects only: no filesystem discovery while drawing or
    /// opening a menu. Keep a closed parent reachable after restoring a session.
    pub(crate) fn closed_roots(&self) -> impl Iterator<Item = &PathBuf> {
        self.entries.iter().map(|repo| &repo.root)
            .chain(self.parent.iter().filter(move |parent| {
                !self.entries.iter().any(|repo| &repo.root == *parent)
            }))
            .filter(move |root| self.closed.contains(*root))
    }
}

pub(crate) struct Repository {
    pub root: PathBuf,
    // The active repository's files and input live in App while it is selected.
    // Switching moves them, including input undo history; it never clones files.
    pub files: Vec<GitFileChange>,
    pub branch: String,
    pub counts: [usize; Group::ALL.len()],
    pub view: View,
}

impl Repository {
    pub(crate) fn change_count(&self, visibility: Visibility) -> usize {
        Group::ALL.into_iter().filter(|&group| visibility.shows(group))
            .map(|group| self.counts[group as usize]).sum()
    }
}

#[derive(Default)]
pub(crate) struct View {
    message: String,
    cursor: usize,
    anchor: Option<usize>,
    input: crate::commit_input::CommitInputState,
    selection: crate::app_source_control::SourceControlSelection,
    selected: usize,
    scroll: usize,
    collapsed: [bool; Group::ALL.len()],
    amend: bool,
}

#[derive(Clone)]
pub(crate) struct Scan {
    pub root: PathBuf,
    pub files: Vec<GitFileChange>,
    pub branch: String,
    /// Ordinary watcher events update only unstaged records within these
    /// prefixes. None is a complete repository snapshot (including the index).
    pub worktree_paths: Option<crate::app_git::refresh::WorktreePaths>,
    /// False snapshots cannot satisfy a later request to display untracked files.
    pub include_untracked: bool,
}

pub(crate) struct Discovery {
    pub roots: Vec<PathBuf>,
    pub parent: Option<PathBuf>,
    pub workspace: PathBuf,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub(crate) struct SavedRepositories {
    active: Option<PathBuf>,
    closed: Vec<PathBuf>,
    parent_open: bool,
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

pub(crate) fn git_command_for(repo: &git2::Repository) -> Result<std::process::Command, String> {
    let workdir = repo
        .workdir()
        .ok_or("Bare repositories are not supported")?;
    let mut command = std::process::Command::new("git");
    // Pin subprocesses too: if a nested .git disappears, ordinary Git discovery
    // must never fall back to running an action against the parent repository.
    command
        .arg("--git-dir")
        .arg(repo.path())
        .arg("--work-tree")
        .arg(workdir)
        .current_dir(workdir);
    Ok(command)
}

pub(crate) fn git_command(root: &Path) -> Result<std::process::Command, String> {
    let repo = git2::Repository::open(root).map_err(|error| error.to_string())?;
    git_command_for(&repo)
}

pub(crate) fn open_file_repository(
    file: &Path,
    hint: Option<&Path>,
) -> Result<git2::Repository, String> {
    if let Some(root) = hint {
        return git2::Repository::open(root).map_err(|error| error.to_string());
    }
    let absolute = if file.is_absolute() {
        file.to_owned()
    } else {
        std::env::current_dir()
            .map_err(|error| error.to_string())?
            .join(file)
    };
    let mut parent = absolute.parent();
    // A deleted file can also have deleted parents. Discover from its nearest
    // existing directory, while known repositories above are always pinned.
    while let Some(directory) = parent {
        match std::fs::metadata(directory) {
            Ok(_) => {
                return git2::Repository::discover(directory).map_err(|error| error.to_string());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                parent = directory.parent()
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("Git repository not found".into())
}

/// Inspect directories only, skip Git object databases and symlink loops, and
/// prune ignored trees. Check for a repository *before* pruning, so an ignored
/// submodule or embedded project is still discoverable without scanning its
/// build/dependency output. This traversal is cached until a structural change.
pub(crate) fn discover(workspace: &Path, cancel: &CancellationToken) -> Discovery {
    if cancel.is_cancelled() {
        return Discovery {
            roots: Vec::new(),
            parent: None,
            workspace: workspace.to_owned(),
        };
    }
    let workspace = canonical(workspace);
    let containing = git2::Repository::discover(&workspace).ok();
    if containing.as_ref().is_some_and(|repo| repo.is_bare()) {
        return Discovery {
            roots: Vec::new(),
            parent: None,
            workspace,
        };
    }
    let parent = containing
        .as_ref()
        .and_then(|repo| repo.workdir())
        .map(canonical)
        .filter(|root| root != &workspace);
    let mut contexts = containing
        .into_iter()
        .filter_map(|repo| {
            let workdir = repo.workdir().map(canonical)?;
            Some((repo, workdir))
        })
        .collect::<Vec<_>>();
    let mut stack = vec![(workspace.clone(), (!contexts.is_empty()).then_some(0))];
    let mut roots = Vec::new();
    while let Some((directory, mut context)) = stack.pop() {
        if cancel.is_cancelled() {
            break;
        }
        let marker = directory.join(".git");
        if directory == workspace {
            if contexts.first().is_some_and(|(_, root)| root == &workspace) {
                roots.push(workspace.clone());
            }
        } else if marker.try_exists().unwrap_or(false)
            && let Ok(repo) = git2::Repository::open(&directory)
            && !repo.is_bare()
            && let Some(root) = repo.workdir().map(canonical)
            && root == directory
        {
            roots.push(root.clone());
            context = Some(contexts.len());
            contexts.push((repo, root));
        } else if directory != workspace
            && (!crate::background::workspace_path_is_watched(&workspace, &directory)
                || context
                    .and_then(|index| contexts.get(index))
                    .is_some_and(|(repo, root)| {
                        directory.strip_prefix(root).ok().is_some_and(|relative| {
                            repo.status_should_ignore(relative).unwrap_or(false)
                        })
                    }))
        {
            continue;
        }
        let Ok(children) = std::fs::read_dir(&directory) else {
            continue;
        };
        for child in children.flatten() {
            if cancel.is_cancelled() {
                break;
            }
            if !child
                .file_type()
                .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
            {
                continue;
            }
            let name = child.file_name();
            if name == ".git"
                || name
                    .to_string_lossy()
                    .starts_with(crate::formatters::TEMP_DIR_PREFIX)
            {
                continue;
            }
            stack.push((child.path(), context));
        }
    }
    roots.sort_unstable();
    roots.dedup();
    Discovery {
        roots,
        parent,
        workspace,
    }
}

impl App {
    pub(crate) fn workspace_file_path(&self, path: &Path) -> PathBuf {
        if self.repositories.workspace.as_ref() == Some(&self.explorer.root)
            && let Some(root) = &self.repositories.canonical_workspace
            && let Ok(relative) = path.strip_prefix(root)
        {
            // Preserve the workspace's spelling, including a symlink/junction,
            // when opening an editor. Repository operations keep their own root.
            return self.explorer.root.join(relative);
        }
        path.to_owned()
    }

    pub(crate) fn repository_hint_for_path(&self, path: &Path) -> Option<PathBuf> {
        // Let the worker resolve parent traversals and symlinks. A lexical
        // prefix like child/../file must not pin the file to child's repository.
        if path
            .components()
            .any(|part| part == std::path::Component::ParentDir)
        {
            return None;
        }
        let resolved = self
            .repositories
            .canonical_workspace
            .as_ref()
            .filter(|_| self.repositories.workspace.as_ref() == Some(&self.explorer.root))
            .and_then(|root| {
                path.strip_prefix(&self.explorer.root)
                    .ok()
                    .map(|relative| root.join(relative))
            });
        let path = resolved.as_deref().unwrap_or(path);
        self.repositories
            .entries
            .iter()
            .map(|repo| &repo.root)
            .chain(self.repositories.parent.iter())
            .filter(|root| path.starts_with(root))
            .max_by_key(|root| root.components().count())
            .cloned()
    }
    pub(crate) fn saved_repositories(&self) -> SavedRepositories {
        let mut closed: Vec<_> = self.repositories.closed.iter().cloned().collect();
        closed.sort_unstable();
        SavedRepositories {
            active: self.repositories.active.clone(),
            closed,
            parent_open: self.repositories.parent_open,
        }
    }

    pub(crate) fn restore_repositories(&mut self, saved: SavedRepositories) {
        self.repositories.membership_generation =
            self.repositories.membership_generation.wrapping_add(1);
        self.repositories.workspace = Some(self.explorer.root.clone());
        self.repositories.preferred = saved.active;
        self.repositories.closed = saved.closed.into_iter().collect();
        self.repositories.parent_open = saved.parent_open;
        self.repositories.rescan = true;
    }

    pub(crate) fn source_control_root(&self) -> &Path {
        if self.repositories.workspace.as_ref() == Some(&self.explorer.root) {
            self.repositories
                .active
                .as_deref()
                .unwrap_or(&self.explorer.root)
        } else {
            &self.explorer.root
        }
    }

    pub(crate) fn current_repository_matches(&self, root: &Path) -> bool {
        if self.repositories.discovered && self.repositories.active.is_none() {
            return false;
        }
        root == self.source_control_root()
            || (self.repositories.workspace.as_ref() == Some(&self.explorer.root)
                && self.repositories.active == self.repositories.canonical_workspace
                && Self::same_open_path(root, &self.explorer.root))
    }

    pub(crate) fn source_control_detail_area(&self) -> Option<Rect> {
        if self.repositories.discovered && !self.repositories.expanded {
            return None;
        }
        self.repositories.detail_area.or(self.explorer_area)
    }

    pub(crate) fn source_control_change_count(&self) -> usize {
        if !self.source_control_enabled {
            return 0;
        }
        if !self.repositories.discovered {
            return self.source_control_layout().file_count();
        }
        self.repositories
            .entries
            .iter()
            .filter(|repo| !self.repositories.closed.contains(&repo.root))
            .map(|repo| repo.change_count(self.sc_visibility))
            .sum()
    }

    pub(crate) fn clear_untracked_source_control_cache(&mut self) {
        self.source_control_selected = self.source_control_selection.retain_visible(
            &mut self.source_control_files, self.source_control_selected, self.sc_visibility);
        for repo in &mut self.repositories.entries {
            repo.view.selected = repo.view.selection.retain_visible(
                &mut repo.files, repo.view.selected, self.sc_visibility);
            repo.view.scroll = 0;
            repo.counts[Group::Untracked as usize] = 0;
        }
    }

    fn swap_repository_view(&mut self, index: usize) {
        let repo = &mut self.repositories.entries[index];
        let view = &mut repo.view;
        use std::mem::swap;
        swap(&mut self.source_control_files, &mut repo.files);
        swap(&mut self.source_control_commit_msg, &mut view.message);
        swap(&mut self.source_control_commit_cursor, &mut view.cursor);
        swap(
            &mut self.source_control_commit_selection_anchor,
            &mut view.anchor,
        );
        swap(&mut self.commit_input, &mut view.input);
        swap(&mut self.source_control_selection, &mut view.selection);
        swap(&mut self.source_control_selected, &mut view.selected);
        swap(&mut self.source_control_scroll, &mut view.scroll);
        swap(&mut self.sc_merge_collapsed, &mut view.collapsed[Group::Merge as usize]);
        swap(&mut self.sc_staged_collapsed, &mut view.collapsed[Group::Staged as usize]);
        swap(&mut self.sc_changes_collapsed, &mut view.collapsed[Group::Unstaged as usize]);
        swap(&mut self.sc_untracked_collapsed, &mut view.collapsed[Group::Untracked as usize]);
        swap(&mut self.sc_amend_mode, &mut view.amend);
    }

    pub(crate) fn select_source_control_repository(&mut self, root: &Path) {
        let Some(next) = self
            .repositories
            .entries
            .iter()
            .position(|repo| repo.root == root)
        else {
            return;
        };
        if self.repositories.closed.contains(root) {
            return;
        }
        if self.repositories.active.as_deref() == Some(root) {
            self.repositories.expanded = !self.repositories.expanded;
            self.source_control_input_mode = false;
            return;
        }
        self.activate_source_control_repository(next);
    }

    fn activate_source_control_repository(&mut self, next: usize) {
        let changing = self.repositories.active.is_some();
        // A draft can already exist when the first status reply arrives (or
        // while Source Control is being re-enabled). Discovery must not erase it.
        let initial_draft = (!changing
            && Some(&self.repositories.entries[next].root)
                == self.repositories.canonical_workspace.as_ref()
            && (!self.source_control_commit_msg.is_empty() || !self.commit_input.text.is_empty()))
        .then(|| {
            (
                std::mem::take(&mut self.source_control_commit_msg),
                self.source_control_commit_cursor,
                self.source_control_commit_selection_anchor,
                std::mem::take(&mut self.commit_input),
            )
        });
        self.repositories.view_generation = self.repositories.view_generation.wrapping_add(1);
        if let Some(previous) = self
            .repositories
            .entries
            .iter()
            .position(|repo| Some(&repo.root) == self.repositories.active.as_ref())
        {
            self.swap_repository_view(previous);
        } else {
            self.source_control_files.clear();
            self.source_control_commit_msg.clear();
            self.source_control_commit_cursor = 0;
            self.source_control_commit_selection_anchor = None;
            self.commit_input = Default::default();
            self.source_control_selection = Default::default();
            self.source_control_selected = 0;
            self.source_control_scroll = 0;
        }
        self.swap_repository_view(next);
        if let Some((message, cursor, anchor, input)) = initial_draft {
            self.source_control_commit_msg = message;
            self.source_control_commit_cursor = cursor;
            self.source_control_commit_selection_anchor = anchor;
            self.commit_input = input;
        }
        self.repositories.active = Some(self.repositories.entries[next].root.clone());
        let row = self.repositories.entries[..next].iter()
            .filter(|repo| !self.repositories.closed.contains(&repo.root)).count();
        let visible = self.repositories.headers.len().max(1);
        self.repositories.header_scroll = self.repositories.header_scroll.min(row);
        if row >= self.repositories.header_scroll + visible {
            self.repositories.header_scroll = row + 1 - visible;
        }
        self.repositories.expanded = true;
        self.source_control_repository_state = SourceControlRepositoryState::Available;
        self.source_control_input_mode = false;
        self.sc_context_active = false;
        self.sc_context_target = None;
        self.sc_context_targets.clear();
        self.resizing_sc_graph = false;
        self.sc_graph_click_start = None;
        self.context_menu_active = false;
        self.git_branch_picker_active = false;
        self.bg_git_branch_picker_running = false;
        self.bg_git_branch_running = false;
        self.bg_git_branch_pending = false;
        self.cached_git_branch = self.repositories.entries[next].branch.clone();
        self.clear_git_history();
        if changing {
            self.commit_details.generation = self.commit_details.generation.wrapping_add(1);
            self.commit_details.loading = false;
            self.commit_details.diff_loading = false;
            self.close_commit_details_tab();
        }
        self.refresh_git_log_async();
        self.refresh_git_branch_async();
    }

    pub(crate) fn close_source_control_repository(&mut self, root: &Path) {
        if !self.repositories.closed.insert(root.to_owned()) {
            return;
        }
        self.repositories.membership_generation =
            self.repositories.membership_generation.wrapping_add(1);
        if self.repositories.active.as_deref() == Some(root) {
            if let Some(next) = self
                .repositories
                .entries
                .iter()
                .position(|repo| !self.repositories.closed.contains(&repo.root))
            {
                self.activate_source_control_repository(next);
            } else {
                if let Some(previous) = self
                    .repositories
                    .entries
                    .iter()
                    .position(|repo| repo.root == root)
                {
                    self.swap_repository_view(previous);
                }
                self.repositories.active = None;
                self.repositories.view_generation =
                    self.repositories.view_generation.wrapping_add(1);
                self.repositories.expanded = false;
                self.source_control_repository_state = SourceControlRepositoryState::Missing;
                self.clear_git_history();
                self.cached_git_branch.clear();
                self.source_control_input_mode = false;
                self.bg_git_branch_running = false;
                self.bg_git_branch_pending = false;
                self.bg_git_branch_picker_running = false;
                self.git_branch_picker_active = false;
                self.close_commit_details_tab();
            }
        }
        if let Some(repo) = self
            .repositories
            .entries
            .iter_mut()
            .find(|repo| repo.root == root)
        {
            let files = std::mem::take(&mut repo.files);
            let selection = std::mem::take(&mut repo.view.selection);
            repo.view.selected = 0;
            repo.view.scroll = 0;
            if !files.is_empty() {
                self.background
                    .spawn("closed-repository-drop", move || drop((files, selection)));
            }
        }
        self.refresh_source_control_async();
    }

    pub(crate) fn open_parent_repository(&mut self) {
        self.repositories.membership_generation =
            self.repositories.membership_generation.wrapping_add(1);
        let Some(parent) = self.repositories.parent.clone() else {
            return;
        };
        self.repositories.closed.remove(&parent);
        self.repositories.parent_open = true;
        self.repositories.preferred = Some(parent);
        self.source_control_repository_state = SourceControlRepositoryState::Loading;
        self.refresh_source_control_async();
    }

    pub(crate) fn reopen_source_control_repositories(&mut self) {
        self.repositories.membership_generation =
            self.repositories.membership_generation.wrapping_add(1);
        self.repositories.closed.clear();
        self.repositories.rescan = true;
        self.refresh_source_control_async();
    }

    pub(crate) fn open_repository_picker(&mut self) {
        self.context_menu_button_drag = None;
        if !self.source_control_enabled { return; }
        let mut roots: Vec<_> = self.repositories.closed_roots().cloned().collect();
        if roots.is_empty() && !self.repositories.parent_open
            && let Some(parent) = &self.repositories.parent
        {
            roots.push(parent.clone());
        }
        roots.sort_unstable();
        roots.dedup();
        if roots.is_empty() {
            self.repositories.rescan = true;
            self.refresh_source_control_async();
            return;
        }
        self.context_menu_items = roots
            .iter()
            .map(|root| {
                let path = self.display_path(root);
                let name = path.rsplit(['/', '\\']).find(|part| !part.is_empty()).unwrap_or(&path);
                crate::app::ContextMenuItem {
                    label: format!("{name} — {path}"),
                    shortcut: String::new(),
                    id: "git_reopen_repository",
                    separator_after: false,
                }
            })
            .collect();
        self.repositories.reopen_candidates = roots;
        self.context_menu_inner = None;
        self.context_menu_selected = 0;
        self.context_menu_scroll = 0;
        self.context_menu_active = true;
    }

    pub(crate) fn reopen_selected_repository(&mut self) {
        if !self.source_control_enabled { return; }
        let Some(root) = self
            .repositories
            .reopen_candidates
            .get(self.context_menu_selected)
            .cloned()
        else {
            return;
        };
        if !self.repositories.closed_roots().any(|candidate| candidate == &root)
            && !(self.repositories.parent.as_ref() == Some(&root) && !self.repositories.parent_open)
        {
            return;
        }
        self.repositories.membership_generation =
            self.repositories.membership_generation.wrapping_add(1);
        self.context_menu_active = false;
        self.repositories.closed.remove(&root);
        if self.repositories.parent.as_ref() == Some(&root) {
            self.repositories.parent_open = true;
        }
        self.repositories.preferred = Some(root);
        self.refresh_source_control_async();
    }

    pub(crate) fn apply_repository_scan(
        &mut self,
        root: PathBuf,
        generation: u64,
        discovery: Discovery,
        scans: Vec<Scan>,
    ) -> bool {
        if root != self.explorer.root || generation != self.workspace_generation {
            return false;
        }
        self.source_control_last_refresh = Some(std::time::Instant::now());
        self.repositories.discovered = true;
        self.repositories.canonical_workspace = Some(discovery.workspace);
        self.repositories.parent = discovery.parent;
        let parent = self
            .repositories
            .parent
            .clone()
            .filter(|_| self.repositories.parent_open);
        let live = |path: &PathBuf| discovery.roots.contains(path) || parent.as_ref() == Some(path);
        if self.hunk_popup_status != 'N'
            && self
                .hunk_popup_file
                .as_ref()
                .and_then(|file| self.repository_hint_for_path(file))
                .is_some_and(|root| {
                    self.repositories
                        .entries
                        .iter()
                        .any(|repo| repo.root == root)
                        && !live(&root)
                })
        {
            self.close_hunk_popup();
        }
        if self
            .repositories
            .active
            .as_ref()
            .is_some_and(|path| !live(path))
        {
            if let Some(index) = self
                .repositories
                .entries
                .iter()
                .position(|repo| Some(&repo.root) == self.repositories.active.as_ref())
            {
                self.swap_repository_view(index);
            }
            self.repositories.active = None;
            self.close_commit_details_tab();
        }
        self.repositories.entries.retain(|repo| {
            discovery.roots.contains(&repo.root) || parent.as_ref() == Some(&repo.root)
        });
        for workdir in discovery
            .roots
            .iter()
            .chain(scans.iter().map(|scan| &scan.root))
        {
            if !self
                .repositories
                .entries
                .iter()
                .any(|repo| &repo.root == workdir)
            {
                self.repositories.entries.push(Repository {
                    root: workdir.clone(),
                    files: Vec::new(),
                    branch: String::new(),
                    counts: [0; Group::ALL.len()],
                    view: Default::default(),
                });
            }
        }
        for mut scan in scans {
            // A scan started while untracked files were visible can finish
            // after they are hidden. Reuse its tracked rows without another scan.
            if !self.sc_visibility.untracked {
                scan.files.retain(|file| file.staged || file.status != '?');
            }
            let Some(index) = self
                .repositories
                .entries
                .iter()
                .position(|repo| repo.root == scan.root)
            else {
                continue;
            };
            // Ahead/behind information is refreshed on Git metadata changes,
            // not recomputed by every ordinary file-status scan.
            if self.repositories.entries[index]
                .branch
                .split_whitespace()
                .next()
                .unwrap_or("")
                != scan.branch
            {
                self.repositories.entries[index].branch = scan.branch;
            }
            let active = self.repositories.active.as_ref() == Some(&scan.root);
            let (files, selection, selected) = if active {
                (&mut self.source_control_files, &mut self.source_control_selection, &mut self.source_control_selected)
            } else {
                let repo = &mut self.repositories.entries[index];
                (&mut repo.files, &mut repo.view.selection, &mut repo.view.selected)
            };
            if let Some(paths) = scan.worktree_paths {
                *selected = selection.update_worktree(files, *selected, scan.files, &paths, self.sc_visibility);
            } else {
                *selected = selection.reconcile(files, *selected, &scan.files, self.sc_visibility);
                *files = scan.files;
            }
            let mut counts = [0; Group::ALL.len()];
            for file in files.iter() {
                counts[crate::source_control_rows::Group::of(file) as usize] += 1;
            }
            self.repositories.entries[index].counts = counts;
            if active {
                self.cached_git_branch = self.repositories.entries[index].branch.clone();
            }
        }
        // A selective refresh omits unrelated repositories. Their retained
        // snapshots must still contribute Explorer badges and ancestor colors.
        let active = self.repositories.active.as_ref();
        let active_files = &self.source_control_files;
        let closed = &self.repositories.closed;
        self.explorer.apply_git_repository_changes(self.repositories.entries.iter()
            .filter(|repo| !closed.contains(&repo.root))
            .flat_map(|repo| {
                let files = if active == Some(&repo.root) { active_files } else { &repo.files };
                files.iter().map(move |file| (repo.root.as_path(), file.path.as_str(), file.status))
            }));
        if let Some(preferred) = self.repositories.preferred.take()
            && Some(&preferred) != self.repositories.active.as_ref()
            && let Some(next) = self.repositories.entries.iter().position(|repo| {
                repo.root == preferred && !self.repositories.closed.contains(&repo.root)
            })
        {
            self.activate_source_control_repository(next);
        }
        if self.repositories.active.is_none() {
            let next = self
                .repositories
                .entries
                .iter()
                .position(|repo| !self.repositories.closed.contains(&repo.root));
            if let Some(next) = next {
                self.activate_source_control_repository(next);
            } else {
                self.source_control_repository_state = SourceControlRepositoryState::Missing;
                self.source_control_files.clear();
                self.clear_git_history();
                self.cached_git_branch.clear();
                self.repositories.view_generation =
                    self.repositories.view_generation.wrapping_add(1);
                self.bg_git_branch_running = false;
                self.bg_git_branch_pending = false;
                self.source_control_input_mode = false;
                self.sc_context_active = false;
                self.sc_context_target = None;
                self.sc_context_targets.clear();
                self.sc_discard_confirm_active = false;
                self.sc_discard_request = None;
                self.sc_stage_all_commit_request = None;
                self.git_branch_picker_active = false;
                self.close_commit_details_tab();
            }
        }
        true
    }

    pub(crate) fn finish_repository_commit(
        &mut self,
        request: &crate::app_git::GitCommitRequest,
        amended: bool,
    ) {
        if self.current_repository_matches(&request.root) {
            if request.message == self.source_control_commit_msg {
                self.source_control_commit_msg.clear();
                self.source_control_commit_cursor = 0;
                self.source_control_commit_selection_anchor = None;
            }
            if amended {
                self.sc_amend_mode = false;
            }
        } else if let Some(repo) = self.repositories.entries.iter_mut().find(|repo| {
            repo.root == request.root
                || (Some(&repo.root) == self.repositories.canonical_workspace.as_ref()
                    && self.repositories.workspace.as_ref() == Some(&self.explorer.root)
                    && Self::same_open_path(&request.root, &self.explorer.root))
        }) {
            if repo.view.message == request.message {
                repo.view.message.clear();
                repo.view.cursor = 0;
                repo.view.anchor = None;
            }
            if amended {
                repo.view.amend = false;
            }
        }
    }
}
