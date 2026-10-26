//! A pinned, asynchronous commit inspection session. Late results cannot replace
//! a newer selection or cross a workspace boundary.
use std::path::{Path, PathBuf};

use crate::app::{ActivePane, App, GitCommitReference, Tab, TabKind};
use crate::background::BackgroundResult;

#[cfg(test)]
#[path = "commit_details_tests.rs"]
mod tests;

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct CommitFile {
    pub status: char,
    pub path: String,
    pub old_path: Option<String>,
    pub added: usize,
    pub deleted: usize,
    pub binary: bool,
}

impl CommitFile {
    #[cfg(test)]
    pub fn new(status: char, path: impl Into<String>) -> Self {
        Self {
            status,
            path: path.into(),
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct CommitData {
    pub hash: String,
    pub message: String,
    pub author: String,
    pub email: String,
    pub time_ago: String,
    pub date: String,
    pub parents: Vec<String>,
    pub references: Vec<GitCommitReference>,
    pub files: Vec<CommitFile>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Focus {
    #[default]
    Files,
    Message,
    Filter,
    Diff,
    Action(usize),
}

pub(crate) struct CopyFeedback {
    pub action: usize,
    pub copied: bool,
    pub expires_at: std::time::Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Divider {
    Primary,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PaneSizes {
    pub overview: u16,
}

impl Default for PaneSizes {
    fn default() -> Self {
        Self {
            overview: 260,
        }
    }
}

#[derive(Default)]
pub(crate) struct CommitDetails {
    pub active: bool,
    pub data: CommitData,
    pub selected: usize,
    pub message_scroll: usize,
    pub message_rows: usize,
    pub query: String,
    pub query_cursor: usize,
    pub query_anchor: Option<usize>,
    pub query_hit: Option<crate::text_input::TextInputHitMap>,
    pub focus: Focus,
    pub copy_feedback: Option<CopyFeedback>,
    pub loading: bool,
    pub error: Option<String>,
    pub generation: u64,
    pub layout: Option<crate::ui_commit_details::InspectorLayout>,
    pub area: ratatui::layout::Rect,
    pub diff_error: Option<String>,
    pub diff_file: Option<usize>,
    pub diff_loading: bool,
    pub diff_expanded: bool,
    pub diff_context: DiffContext,
    pub context_draft: Option<String>,
    pub context_revealed_line: Option<usize>,
    pub diff_full_cache: Option<std::sync::Arc<crate::app::DiffRenderCache>>,
    pub scrollbar_drag: Option<(Focus, u16)>,
    pub panes: PaneSizes,
    pub divider_drag: Option<Divider>,
    pub last_divider_click: Option<(Divider, std::time::Instant)>,
    pub visible: bool,
    pub review: crate::commit_review::Review,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DiffContext {
    pub changes_only: bool,
    pub lines: usize,
}

impl Default for DiffContext {
    fn default() -> Self {
        Self {
            changes_only: true,
            lines: 3,
        }
    }
}

impl CommitDetails {
    pub fn expire_copy_feedback(&mut self, now: std::time::Instant) -> bool {
        if self
            .copy_feedback
            .as_ref()
            .is_some_and(|feedback| now >= feedback.expires_at)
        {
            self.copy_feedback = None;
            true
        } else {
            false
        }
    }

    pub fn clear_filter(&mut self) {
        // Keep the same file selected when broadening back to the full list.
        let selected = self.selected_file();
        self.query.clear();
        self.query_cursor = 0;
        self.query_anchor = None;
        self.selected = selected.unwrap_or(0);
        self.review.reveal = true;
        self.focus = Focus::Filter;
    }

    pub fn filtered_files(&self) -> Vec<usize> {
        let query = self.query.trim().to_lowercase();
        self.data
            .files
            .iter()
            .enumerate()
            .filter_map(|(i, file)| {
                (query.is_empty()
                    || file.path.to_lowercase().contains(&query)
                    || file
                        .old_path
                        .as_ref()
                        .is_some_and(|path| path.to_lowercase().contains(&query)))
                .then_some(i)
            })
            .collect()
    }

    pub fn selected_file(&self) -> Option<usize> {
        if self.query.trim().is_empty() {
            return (self.selected < self.data.files.len()).then_some(self.selected);
        }
        self.filtered_files().get(self.selected).copied()
    }

    pub fn edit_filter(
        &mut self,
        code: crossterm::event::KeyCode,
        modifiers: crossterm::event::KeyModifiers,
        paste: Option<&str>,
    ) {
        let mut input = crate::text_input::TextInput::new(
            &mut self.query,
            &mut self.query_cursor,
            &mut self.query_anchor,
        );
        let changed = if let Some(text) = paste {
            input.insert_text(text);
            true
        } else {
            input.handle_key(code, modifiers).changed
        };
        if changed {
            self.selected = 0;
            self.review.scroll = 0;
            self.review.reveal = true;
        }
    }
}

pub(crate) struct DetailsResult {
    pub root: PathBuf,
    pub generation: u64,
    pub result: Result<CommitData, String>,
}

pub(crate) struct DiffResult {
    pub root: PathBuf,
    pub generation: u64,
    pub file: usize,
    pub result: Result<CommitDiff, String>,
}

/// The same complete revisions and prepared cache consumed by editor diff tabs.
pub(crate) struct CommitDiff {
    pub path: PathBuf,
    pub target: ropey::Rope,
    pub labels: crate::app::DiffLabels,
    pub prepared: crate::background::PreparedDiffRefresh,
}

pub(crate) fn load_diff(
    root: &Path,
    hash: &str,
    file: &CommitFile,
    limit: u64,
) -> Result<CommitDiff, String> {
    Ok(prepare_diff(read_diff(root, hash, file, limit)?))
}

/// Repository reads happen beside the files; comparison and render preparation
/// happen on a local worker. Only the two revisions cross the remote link.
pub(crate) struct DiffContents {
    pub path: PathBuf,
    pub base: String,
    pub target: String,
    pub labels: crate::app::DiffLabels,
}

pub(crate) fn read_diff(
    root: &Path,
    hash: &str,
    file: &CommitFile,
    limit: u64,
) -> Result<DiffContents, String> {
    let repo = git2::Repository::discover(root).map_err(|error| error.to_string())?;
    let commit = repo
        .find_commit(git2::Oid::from_str(hash).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    let tree = commit.tree().map_err(|error| error.to_string())?;
    let parent = if commit.parent_count() > 0 {
        Some(commit.parent(0).map_err(|error| error.to_string())?)
    } else {
        None
    };
    let parent_tree = parent
        .as_ref()
        .map(|parent| parent.tree())
        .transpose()
        .map_err(|error| error.to_string())?;
    let contents = |tree: Option<&git2::Tree<'_>>, path: &str| -> Result<String, String> {
        let Some(tree) = tree else {
            return Ok(String::new());
        };
        let entry = match tree.get_path(Path::new(path)) {
            Ok(entry) => entry,
            Err(error) if error.code() == git2::ErrorCode::NotFound => return Ok(String::new()),
            Err(error) => return Err(error.to_string()),
        };
        if entry.kind() != Some(git2::ObjectType::Blob) {
            return Err("Text diff is unavailable for submodule changes".into());
        }
        let blob = repo
            .find_blob(entry.id())
            .map_err(|error| error.to_string())?;
        if blob.size() as u64 > limit {
            return Err(
                "Diff is paused for this large file; adjust the large-file threshold to enable it"
                    .into(),
            );
        }
        crate::app_git::diff_blob_content(&blob)
    };
    let base = contents(
        parent_tree.as_ref(),
        file.old_path.as_deref().unwrap_or(&file.path),
    )?;
    let target = contents(Some(&tree), &file.path)?;
    Ok(DiffContents {
        path: repo.workdir().unwrap_or(root).join(&file.path),
        base,
        target,
        labels: crate::app::DiffLabels::new(
            parent.as_ref().map_or_else(
                || "Empty tree (no parent)".into(),
                |parent| format!("{} (parent)", &parent.id().to_string()[..7]),
            ),
            format!("{} (commit)", &commit.id().to_string()[..7]),
        ),
    })
}

pub(crate) fn prepare_diff(contents: DiffContents) -> CommitDiff {
    let DiffContents { path, base, target, labels } = contents;
    let diff_lines = crate::app::compute_file_diff(&base, &target);
    let target = ropey::Rope::from_str(&target);
    let render_cache = crate::app::DiffRenderCache::new(&diff_lines);
    let target_line_markers = crate::app_git_display::target_line_markers_from_compare_diff(
        &diff_lines,
        target.len_lines(),
    );
    CommitDiff {
        path,
        target,
        labels,
        prepared: crate::background::PreparedDiffRefresh {
            base_content: base.into(),
            diff_lines,
            render_cache,
            target_line_markers,
        },
    }
}

fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .collect()
}

pub(crate) fn load(root: &Path, hash: &str) -> Result<CommitData, String> {
    let read = || -> Result<CommitData, git2::Error> {
        let repo = git2::Repository::discover(root)?;
        let commit = repo.find_commit(git2::Oid::from_str(hash)?)?;
        let tree = commit.tree()?;
        // A missing parent in a shallow clone is an error, not an empty tree.
        let parent = if commit.parent_count() > 0 {
            Some(commit.parent(0)?.tree()?)
        } else {
            None
        };
        let mut diff = repo.diff_tree_to_tree(parent.as_ref(), Some(&tree), None)?;
        diff.find_similar(Some(git2::DiffFindOptions::new().renames(true)))?;
        let mut files = Vec::new();
        for (index, delta) in diff.deltas().enumerate() {
            let status = match delta.status() {
                git2::Delta::Added => 'A',
                git2::Delta::Deleted => 'D',
                git2::Delta::Renamed => 'R',
                git2::Delta::Copied => 'C',
                git2::Delta::Typechange => 'T',
                _ => 'M',
            };
            let path = delta
                .new_file()
                .path()
                .or_else(|| delta.old_file().path())
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default();
            let old_path = delta
                .old_file()
                .path()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .filter(|old| old != &path);
            let patch = git2::Patch::from_diff(&diff, index)?;
            let (_, added, deleted) = patch
                .as_ref()
                .map(|patch| patch.line_stats())
                .transpose()?
                .unwrap_or_default();
            let binary = patch.as_ref().map_or_else(
                || delta.old_file().is_binary() || delta.new_file().is_binary(),
                |patch| {
                    patch.delta().old_file().is_binary() || patch.delta().new_file().is_binary()
                },
            );
            files.push(CommitFile {
                status,
                path,
                old_path,
                added,
                deleted,
                binary,
            });
        }
        files.sort_unstable_by(|a, b| a.path.cmp(&b.path));
        let mut references = Vec::new();
        for reference in repo.references()?.flatten() {
            if (reference.is_branch() || reference.is_remote() || reference.is_tag())
                && reference
                    .peel_to_commit()
                    .is_ok_and(|target| target.id() == commit.id())
            {
                let name = reference.shorthand().unwrap_or("");
                references.push(GitCommitReference {
                    name: if reference.is_tag() {
                        format!("tag: {name}")
                    } else {
                        name.to_owned()
                    },
                    remote: reference.is_remote(),
                });
            }
        }
        references.sort_by(|a, b| (a.remote, &a.name).cmp(&(b.remote, &b.name)));
        let time = commit.time();
        let offset = chrono::FixedOffset::east_opt(time.offset_minutes().saturating_mul(60));
        let date = chrono::DateTime::from_timestamp(time.seconds(), 0)
            .and_then(|date| offset.map(|offset| date.with_timezone(&offset)))
            .map(|date| date.format("%d %b %Y, %H:%M %:z").to_string())
            .unwrap_or_default();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        Ok(CommitData {
            hash: commit.id().to_string(),
            message: clean(commit.message().unwrap_or("")),
            author: clean(commit.author().name().unwrap_or("Unknown author")),
            email: clean(commit.author().email().unwrap_or("")),
            time_ago: App::format_time_ago(now, time.seconds()),
            date,
            parents: commit.parent_ids().map(|id| id.to_string()).collect(),
            references,
            files,
        })
    };
    read().map_err(|error| error.to_string())
}

impl App {
    fn has_commit_details_tab(&self) -> bool {
        self.tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .any(|tab| tab.kind == TabKind::CommitDetails)
    }
    pub(crate) fn is_commit_details_tab(&self) -> bool {
        self.tabs
            .get(self.active_tab)
            .is_some_and(|tab| tab.kind == TabKind::CommitDetails)
    }

    pub(crate) fn commit_details_tab_group(&self) -> Option<usize> {
        if self.is_commit_details_tab() {
            return Some(self.active_group);
        }
        self.groups.iter().enumerate().find_map(|(group, state)| {
            (group != self.active_group
                && state
                    .tabs
                    .get(state.active_tab)
                    .is_some_and(|tab| tab.kind == TabKind::CommitDetails))
            .then_some(group)
        })
    }

    pub(crate) fn reset_commit_details(&mut self) {
        self.clear_commit_review_files();
        self.clear_commit_diff_view();
        self.commit_details = CommitDetails {
            generation: self.commit_details.generation.wrapping_add(1),
            panes: self.commit_details.panes,
            diff_context: self.commit_details.diff_context,
            ..CommitDetails::default()
        };
    }

    pub(crate) fn close_commit_details_tab(&mut self) {
        if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.kind == TabKind::CommitDetails)
        {
            self.force_close_tab(index);
        } else if let Some((group, index)) =
            self.groups.iter().enumerate().find_map(|(group, state)| {
                state
                    .tabs
                    .iter()
                    .position(|tab| tab.kind == TabKind::CommitDetails)
                    .map(|index| (group, index))
            })
        {
            self.switch_group(group);
            self.force_close_tab(index);
        }
        self.reset_commit_details();
    }

    pub(crate) fn open_source_control_commit_details(&mut self, idx: usize) {
        if !self.source_control_enabled { return; }
        let Some(info) = self.git_log.get(idx).cloned() else {
            return;
        };
        let generation = self.commit_details.generation.wrapping_add(1);
        self.git_log_selected = idx;
        self.sc_graph_focused = true;
        self.source_control_input_mode = false;
        self.suspend_diff_tab();
        self.auto_save_on_focus_change();
        self.clear_transient_editor_state();
        // A single browsing tab follows graph selection. Reopening the same
        // commit retains its filter, selection, scroll positions and layout.
        if let Some((group, index)) = self.groups.iter().enumerate().find_map(|(group, state)| {
            state
                .tabs
                .iter()
                .position(|tab| tab.kind == TabKind::CommitDetails)
                .map(|index| (group, index))
        }) {
            self.switch_group(group);
            self.active_tab = index;
        }
        let tab = if let Some(index) = self
            .tabs
            .iter()
            .position(|tab| tab.kind == TabKind::CommitDetails)
        {
            index
        } else {
            let mut editor = self.new_editor();
            editor.read_only = true;
            let mut tab = Tab::new_file(editor);
            tab.kind = TabKind::CommitDetails;
            self.tabs.push(tab);
            self.tabs.len() - 1
        };
        self.active_tab = tab;
        self.active_pane = ActivePane::Editor;
        self.terminal_panel_focused = false;
        self.settings_editor_active = false;
        self.touch_tab_mru(tab);
        self.ensure_active_tab_visible();
        self.sync_diff_tab();
        if self.commit_details.active && self.commit_details.data.hash == info.full_hash {
            return;
        }
        self.clear_commit_review_files();
        self.clear_commit_diff_view();
        self.commit_details = CommitDetails {
            active: true,
            loading: true,
            generation,
            data: CommitData {
                hash: info.full_hash,
                message: info.message,
                author: info.author,
                time_ago: info.time_ago,
                references: info.branches,
                ..CommitData::default()
            },
            // Keep the last rendered geometry until the next frame.
            layout: self.commit_details.layout.take(),
            area: self.commit_details.area,
            panes: self.commit_details.panes,
            diff_context: self.commit_details.diff_context,
            ..CommitDetails::default()
        };
        self.load_commit_details();
    }

    pub(crate) fn load_commit_details(&mut self) {
        if !self.source_control_enabled { return; }
        self.commit_details.review.pending = None;
        self.commit_details.loading = true;
        self.commit_details.error = None;
        self.commit_details.generation = self.commit_details.generation.wrapping_add(1);
        let generation = self.commit_details.generation;
        let root = self.source_control_root().to_owned();
        let hash = self.commit_details.data.hash.clone();
        let sender = self.source_control_sender();
        let remote = self.remote_workspace.clone();
        let log = self.git_output();
        self.background.spawn("commit-details", move || {
            if sender.is_cancelled() { return; }
            let operation = remote.is_none().then(|| crate::git_output::native(&log, "Read commit details", &root));
            let result = match remote {
                Some(remote) => remote.commit_details(&root, hash).map_err(|error| error.to_string()),
                None => load(&root, &hash),
            };
            if let Some(operation) = &operation {
                operation.result(&result, |data| format!("{} changed files", data.files.len()));
            }
            let _ = sender.send(BackgroundResult::CommitDetails(DetailsResult {
                root,
                generation,
                result,
            }));
        });
    }

    pub(crate) fn apply_commit_details(&mut self, result: DetailsResult) -> bool {
        if !self.commit_details.active
            || !self.has_commit_details_tab()
            || !self.current_repository_matches(&result.root)
            || result.generation != self.commit_details.generation
        {
            return false;
        }
        self.commit_details.loading = false;
        match result.result {
            Ok(data) => self.commit_details.data = data,
            Err(error) => self.commit_details.error = Some(error),
        }
        true
    }

    pub(crate) fn adjacent_commit(&self, newer: bool) -> Option<usize> {
        let current = self
            .git_log
            .iter()
            .position(|item| item.full_hash == self.commit_details.data.hash)?;
        if newer {
            current.checked_sub(1)
        } else {
            (current + 1 < self.git_log.len()).then_some(current + 1)
        }
    }

    pub(crate) fn request_commit_diff(&mut self) {
        if !self.source_control_enabled { return; }
        let file = self.commit_details.selected_file();
        if !self.commit_details.active
            || self.commit_details.loading
            || self.commit_details.error.is_some()
            || file.is_some_and(|file| self.commit_details.review.collapsed.contains(&file))
        {
            return;
        }
        if let Some(file) = file
            && self.activate_commit_review_file(file) { return; }
        if self.commit_details.review.pending.is_some() { return; }
        if file == self.commit_details.diff_file
            && (self.commit_details.diff_full_cache.is_some() || self.commit_details.diff_error.is_some())
        { return; }
        self.stash_commit_review_file();
        self.commit_details.diff_file = file;
        self.commit_details.diff_error = None;
        self.commit_details.diff_loading = file.is_some();
        self.clear_commit_diff_view();
        let Some(index) = file else {
            return;
        };
        self.queue_commit_review_file(index);
    }

    pub(crate) fn queue_commit_review_file(&mut self, index: usize) {
        if !self.source_control_enabled || !self.commit_details.active
            || self.commit_details.loading || self.commit_details.error.is_some()
            || self.commit_details.review.pending.is_some()
            || index >= self.commit_details.data.files.len() { return; }
        self.commit_details.review.pending = Some(index);
        if self.commit_details.selected_file() == Some(index) {
            self.commit_details.diff_loading = true;
        }
        let file = self.commit_details.data.files[index].clone();
        let root = self.source_control_root().to_owned();
        let generation = self.commit_details.generation;
        let hash = self.commit_details.data.hash.clone();
        let limit = self.large_file_threshold_mb.saturating_mul(1024 * 1024);
        let sender = self.source_control_sender();
        let remote = self.remote_workspace.clone();
        let log = self.git_output();
        self.background.spawn("commit-diff", move || {
            if sender.is_cancelled() { return; }
            let operation = remote.is_none().then(|| crate::git_output::native(&log, "Read commit diff", &root));
            let result = match remote {
                Some(remote) => remote.commit_diff(&root, hash, file, limit).map_err(|error| error.to_string()),
                None => load_diff(&root, &hash, &file, limit),
            };
            if let Some(operation) = &operation {
                operation.result(&result, |_| "Commit diff prepared".into());
            }
            let _ = sender.send(BackgroundResult::CommitDiff(DiffResult {
                root,
                generation,
                file: index,
                result,
            }));
        });
    }

    pub(crate) fn apply_commit_diff(&mut self, result: DiffResult) -> bool {
        if !self.commit_details.active
            || !self.has_commit_details_tab()
            || !self.current_repository_matches(&result.root)
            || result.generation != self.commit_details.generation
            || (self.commit_details.review.pending != Some(result.file)
                && (self.commit_details.review.pending.is_some() || Some(result.file) != self.commit_details.diff_file))
        {
            return false;
        }
        self.commit_details.review.pending = None;
        let selected = self.commit_details.selected_file() == Some(result.file);
        if selected { self.commit_details.diff_loading = false; }
        match result.result {
            Ok(diff) => {
                self.commit_details.review.errors.remove(&result.file);
                if selected {
                    if self.commit_details.diff_file != Some(result.file) { self.stash_commit_review_file(); }
                    self.commit_details.diff_file = Some(result.file);
                    self.commit_details.diff_error = None;
                    self.install_commit_diff(diff);
                } else {
                    let view = self.make_commit_file_view(diff);
                    self.commit_details.review.cached.insert(result.file, view);
                }
            }
            Err(error) => {
                self.commit_details.review.errors.insert(result.file, error.clone());
                if selected { self.commit_details.diff_error = Some(error); }
            }
        }
        true
    }
}
