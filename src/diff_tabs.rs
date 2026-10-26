//! Independent Source Control diff tabs. The existing diff renderer consumes
//! active-view fields; move those fields between tabs instead of rebuilding
//! their potentially large caches whenever focus changes.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::app::{
    ActivePane, App, DiffBaseRevision, DiffLabels, DiffMode, DiffRenderCache, Tab, TabKind,
};

#[cfg(test)]
#[path = "diff_refresh_tests.rs"]
mod refresh_tests;

// Ordinary diff tabs allocate IDs starting at one. The single commit browsing
// tab owns zero so it shares the same cache, input and split-group lifecycle.
pub(crate) const COMMIT_DIFF_ID: u64 = 0;

fn scroll_original_diff_at(
    split_area: Option<ratatui::layout::Rect>,
    left_code_area: Option<ratatui::layout::Rect>,
    split_ratio: f32,
    text_width: usize,
    scroll: &mut usize,
    point: ratatui::layout::Position,
    delta: isize,
) -> bool {
    let (Some(area), Some(code)) = (split_area, left_code_area) else {
        return false;
    };
    let layout = crate::ui_diff_display::side_by_side_diff_layout(area, split_ratio);
    if !layout.left.contains(point) && !layout.left_header.contains(point) {
        return false;
    }
    // The original pane is unwrapped and has its own width and scrollbar,
    // independent of the target editor's caret, word wrap and horizontal offset.
    *scroll = scroll
        .saturating_add_signed(delta)
        .min(text_width.saturating_sub(code.width as usize));
    true
}

#[derive(Default)]
pub(crate) struct DiffTabs {
    pub owner: Option<u64>,
    next_id: u64,
    views: rustc_hash::FxHashMap<u64, DiffViewState>,
    // Keep comparison identity even while a tab's view is installed or pending.
    source_control: rustc_hash::FxHashMap<u64, SourceControlIdentity>,
    blame_revisions: rustc_hash::FxHashMap<u64, (Option<Arc<crate::git_blame::Revision>>, Arc<crate::git_blame::Revision>)>,
    refresh_generation: u64,
    pub(crate) refresh_running: bool,
    dirty: rustc_hash::FxHashSet<u64>,
}

struct SourceControlIdentity {
    staged: bool,
    repository: Option<PathBuf>,
    revisions: Option<crate::git_diff_sources::Revisions>,
    epoch: u64,
    dirty: Dirty,
    inflight: Dirty,
}

#[derive(Clone, Copy, Default)]
struct Dirty { git: bool, worktree: bool }
impl Dirty { const ALL: Self = Self { git: true, worktree: true }; }

struct DiffRefreshRequest {
    id: u64,
    epoch: u64,
    path: PathBuf,
    repository: Option<PathBuf>,
    staged: bool,
    base: Arc<str>,
    target: crate::editor::LoadedTextFile,
    missing: bool,
    revisions: Option<crate::git_diff_sources::Revisions>,
    dirty: Dirty,
    cache_current: bool,
}

pub(crate) struct SourceControlDiffUpdate {
    id: u64,
    epoch: u64,
    result: Result<SourceControlDiffContent, String>,
}

struct SourceControlDiffContent {
    revisions: crate::git_diff_sources::Revisions,
    target: crate::editor::LoadedTextFile,
    target_changed: bool,
    missing: bool,
    diff: Option<crate::background::PreparedDiffRefresh>,
}

fn prepare_refresh(
    repo: Option<&git2::Repository>,
    request: DiffRefreshRequest,
    size_limit: u64,
) -> Result<SourceControlDiffContent, String> {
    let sources = if request.dirty.git || request.revisions.is_none() {
        let repo = repo.expect("repository for dirty Git revisions");
        let workdir = repo.workdir().ok_or("Bare repositories are not supported")?;
        let relative = crate::app_git::repository_relative_path(workdir, &request.path)
            .ok_or_else(|| format!("{} is not inside the repository worktree", request.path.display()))?;
        Some(crate::git_diff_sources::read(repo, &relative, request.staged, request.revisions)?)
    } else { None };
    let (base, mut target, revisions, mut target_read) = refresh_sources(&request, sources);
    let mut missing = request.missing;
    if !request.staged && request.dirty.worktree {
        if std::fs::metadata(&request.path).is_ok_and(|meta| meta.len() > size_limit) {
            return Err(
                "Diff is paused for this large file; adjust the large-file threshold to enable it"
                    .into(),
            );
        }
        target = match crate::editor::load_text_file(&request.path) {
            Ok(loaded) => { missing = false; loaded },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing = true;
                crate::editor::LoadedTextFile {
            remote_checkpoint: None,
                    rope: ropey::Rope::new(),
                    encoding: "UTF-8",
                    file_size: 0,
                    last_disk_modified: None,
                }
            }
            Err(error) => return Err(format!("Cannot read {}: {error}", request.path.display())),
        };
        target_read = true;
    }
    finish_refresh(request, base, target, missing, size_limit, revisions, target_read)
}

fn prepare_remote_refresh(remote: &crate::remote::workspace::Workspace, request: DiffRefreshRequest,
    size_limit: u64, cancel: &crate::background::CancellationToken) -> Result<SourceControlDiffContent, String> {
    let sources = if request.dirty.git || request.revisions.is_none() {
        Some(remote.git_diff_sources(&request.path, request.repository.as_deref(), request.staged, request.revisions, cancel).map_err(|error| error.to_string())?)
    } else { None };
    let (base, mut target, revisions, mut target_read) = refresh_sources(&request, sources);
    let mut missing = request.missing;
    if !request.staged && request.dirty.worktree {
        target = match remote.load(&request.path) {
            Ok(loaded) => { missing = false; loaded },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing = true;
                crate::editor::LoadedTextFile { remote_checkpoint: None, rope: ropey::Rope::new(), encoding: "UTF-8", file_size: 0, last_disk_modified: None }
            }
            Err(error) => return Err(error.to_string()),
        };
        target_read = true;
    }
    finish_refresh(request, base, target, missing, size_limit, revisions, target_read)
}

fn refresh_sources(request: &DiffRefreshRequest, sources: Option<crate::git_diff_sources::Sources>)
    -> (Arc<str>, crate::editor::LoadedTextFile, crate::git_diff_sources::Revisions, bool) {
    let mut target = request.target.clone();
    let Some(sources) = sources else {
        return (request.base.clone(), target, request.revisions.expect("known Git sources"), false);
    };
    let base = if request.revisions.is_some_and(|known| known.base == sources.revisions.base) {
        request.base.clone()
    } else { sources.base.unwrap_or_default() };
    let target_read = request.staged && request.revisions.is_none_or(|known| known.index != sources.revisions.index);
    if target_read {
        let index = if sources.revisions.index == sources.revisions.base { base.clone() } else { sources.index.unwrap_or_default() };
        target = crate::editor::LoadedTextFile { remote_checkpoint: None, rope: ropey::Rope::from_str(&index), encoding: "UTF-8", file_size: index.len() as u64, last_disk_modified: None };
    }
    (base, target, sources.revisions, target_read)
}

fn finish_refresh(request: DiffRefreshRequest, base: Arc<str>, target: crate::editor::LoadedTextFile,
    missing: bool, size_limit: u64, revisions: crate::git_diff_sources::Revisions,
    target_read: bool) -> Result<SourceControlDiffContent, String> {
    if target.encoding == "Binary" {
        return Err(crate::app_ui_state::BINARY_DIFF_MESSAGE.into());
    }
    if target.file_size > size_limit {
        return Err(
            "Diff is paused for this large file; adjust the large-file threshold to enable it"
                .into(),
        );
    }
    let target_changed = target_read && target.rope != request.target.rope;
    if request.cache_current && (Arc::ptr_eq(&base, &request.base) || base == request.base) && !target_changed {
        return Ok(SourceControlDiffContent {
            revisions,
            target,
            target_changed,
            missing,
            diff: None,
        });
    }
    let diff_lines = crate::app::compute_file_diff(&base, &target.rope.to_string());
    let render_cache = DiffRenderCache::new(&diff_lines);
    let target_line_markers = crate::app_git_display::target_line_markers_from_compare_diff(
        &diff_lines,
        target.rope.len_lines(),
    );
    Ok(SourceControlDiffContent {
        revisions,
        target,
        target_changed,
        missing,
        diff: Some(crate::background::PreparedDiffRefresh {
            base_content: base,
            diff_lines,
            render_cache,
            target_line_markers,
        }),
    })
}

impl DiffTabs {
    pub(crate) fn blame_files_for_tab(&self, tab: &Tab) -> Option<&(Option<Arc<crate::git_blame::Revision>>, Arc<crate::git_blame::Revision>)> {
        let TabKind::Diff(id) = tab.kind else { return None; };
        self.blame_revisions.get(&id)
    }
    pub(crate) fn staged_for_tab(&self, tab: &Tab) -> Option<bool> {
        match tab.kind {
            TabKind::Diff(id) => self.source_control.get(&id).map(|origin| origin.staged),
            _ => None,
        }
    }
    pub(crate) fn repository_for_tab(&self, tab: &Tab) -> Option<&Path> {
        let TabKind::Diff(id) = tab.kind else { return None; };
        self.source_control.get(&id)?.repository.as_deref()
    }
}

pub(crate) struct DiffViewState {
    mode: DiffMode,
    base_path: Option<std::path::PathBuf>,
    base: Option<Arc<str>>,
    base_rope: Option<ropey::Rope>,
    lines: Vec<(char, String)>,
    cache: Arc<DiffRenderCache>,
    target_version: Option<u64>,
    base_revision: Option<DiffBaseRevision>,
    label: Option<String>,
    labels: Option<DiffLabels>,
    target: Option<std::path::PathBuf>,
    left_scroll: usize,
    split_area: Option<ratatui::layout::Rect>,
    left_code_area: Option<ratatui::layout::Rect>,
    inline_scroll: crate::inline_diff::InlineDiffScroll,
    selection: crate::diff_selection::DiffSelection,
    scope_source: crate::diff_scope_source::DiffScopeSource,
    split_ratio: f32,
    pending: bool,
}

/// A suspended card in the commit review. All large fields are moved; the
/// active card and this cache never own independent copies of its revisions.
pub(crate) struct CommitFileView {
    editor: crate::editor::Editor,
    view: DiffViewState,
    full: Arc<DiffRenderCache>,
    revealed: Option<usize>,
    highlight_key: Option<crate::app::VisibleHighlightKey>,
    highlight_cancel: Option<crate::background::CancellationToken>,
    pub(crate) bytes: usize,
    pub(crate) last_used: u64,
}

impl CommitFileView {
    pub(crate) fn pan(&mut self, col: u16, row: u16, delta: isize) {
        self.view.pan(&mut self.editor, col, row, delta);
    }
    pub(crate) fn review_height(&mut self, context: crate::commit_details::DiffContext, mode: DiffMode) -> usize {
        if self.view.cache.context_lines != context.changes_only.then_some(context.lines) {
            self.view.cache = if context.changes_only { Arc::new(self.full.with_context(context.lines)) }
                else { self.full.clone() };
            self.view.inline_scroll = Default::default();
            self.revealed = None;
        }
        crate::commit_review::rows(&self.view.cache, mode).max(1).saturating_add(4)
    }

    pub(crate) fn discard(self, animations: &mut crate::animations::AnimationState) {
        if let Some(cancel) = self.highlight_cancel { cancel.cancel(); }
        self.view.scope_source.forget_original_view(animations);
    }
}

impl DiffViewState {
    fn pan(&mut self, editor: &mut crate::editor::Editor, col: u16, row: u16, delta: isize) {
        if self.mode == DiffMode::SideBySide && scroll_original_diff_at(
            self.split_area, self.left_code_area, self.split_ratio,
            self.cache.max_left_text_width, &mut self.left_scroll, (col, row).into(), delta)
        { return; }
        self.inline_scroll.scroll(editor, true, delta);
    }
    fn take(app: &mut App) -> Self {
        Self {
            mode: std::mem::replace(&mut app.diff_mode, DiffMode::Off),
            base_path: app.compare_base_path.take(),
            base: app.compare_base_content.take(),
            base_rope: app.pending_compare_base_rope.take(),
            lines: std::mem::take(&mut app.compare_diff_lines),
            cache: std::mem::take(&mut app.diff_render_cache),
            target_version: app.compare_diff_target_version.take(),
            base_revision: app.compare_diff_base_revision.take(),
            label: app.diff_tab_label.take(),
            labels: app.diff_labels.take(),
            target: app.diff_target_path.take(),
            left_scroll: std::mem::take(&mut app.diff_left_horizontal_scroll),
            split_area: app.diff_split_area.take(),
            left_code_area: app.diff_left_code_area.take(),
            inline_scroll: std::mem::take(&mut app.diff_inline_scroll),
            selection: std::mem::take(&mut app.diff_selection),
            scope_source: std::mem::take(&mut app.diff_scope_source),
            split_ratio: app.diff_split_ratio,
            pending: app.pending_git_compare_path.is_some(),
        }
    }

    fn install(self, app: &mut App) {
        app.diff_mode = self.mode;
        app.compare_base_path = self.base_path;
        app.compare_base_content = self.base;
        app.pending_compare_base_rope = self.base_rope;
        app.compare_diff_lines = self.lines;
        app.diff_render_cache = self.cache;
        app.compare_diff_target_version = self.target_version;
        app.compare_diff_base_revision = self.base_revision;
        app.diff_tab_label = self.label;
        app.diff_labels = self.labels;
        app.diff_target_path = self.target;
        app.diff_left_horizontal_scroll = self.left_scroll;
        app.diff_split_area = self.split_area;
        app.diff_left_code_area = self.left_code_area;
        app.diff_inline_scroll = self.inline_scroll;
        app.diff_selection = self.selection;
        app.diff_scope_source.forget_original_view(&mut app.animations);
        app.diff_scope_source = self.scope_source;
        app.diff_split_ratio = self.split_ratio;
    }
}

impl App {
    pub(crate) fn pan_commit_review_file(&mut self, file: usize, col: u16, row: u16, delta: isize) {
        if let Some(view) = self.commit_details.review.cached.get_mut(&file) {
            view.pan(col, row, delta);
        } else if self.commit_details.diff_file == Some(file) {
            if self.is_commit_details_tab() { self.scroll_diff_at(col, row, true, delta); }
            else if let Some(view) = self.diff_tabs.views.get_mut(&COMMIT_DIFF_ID)
                && let Some(tab) = self.tabs.iter_mut()
                    .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
                    .find(|tab| tab.kind == TabKind::CommitDetails)
            { view.pan(&mut tab.editor, col, row, delta); }
        }
    }
    /// Route wheel input using the rendered pane without changing keyboard focus.
    pub(crate) fn scroll_diff_at(
        &mut self,
        col: u16,
        row: u16,
        horizontal: bool,
        delta: isize,
    ) -> bool {
        let point = (col, row).into();
        if horizontal
            && self.diff_mode == DiffMode::SideBySide
            && self.should_render_diff_for_active_editor()
            && scroll_original_diff_at(
                self.diff_split_area,
                self.diff_left_code_area,
                self.diff_split_ratio,
                self.diff_render_cache.max_left_text_width,
                &mut self.diff_left_horizontal_scroll,
                point,
                delta,
            )
        {
            return true;
        }
        if self.row_diff_active() && self.diff_inline_scroll.area.contains(point) {
            let mut scroll = std::mem::take(&mut self.diff_inline_scroll);
            if let Some(editor) = self.editor_mut() {
                scroll.scroll(editor, horizontal, delta);
            }
            self.diff_inline_scroll = scroll;
            return true;
        }
        for (index, group) in self.groups.iter_mut().enumerate() {
            if index == self.active_group { continue; }
            let Some(tab) = group.tabs.get_mut(group.active_tab) else { continue };
            let id = match tab.kind {
                TabKind::Diff(id) => id,
                TabKind::CommitDetails => COMMIT_DIFF_ID,
                _ => continue,
            };
            let Some(view) = self.diff_tabs.views.get_mut(&id) else { continue };
            if horizontal
                && view.mode == DiffMode::SideBySide
                && scroll_original_diff_at(
                    view.split_area,
                    view.left_code_area,
                    view.split_ratio,
                    view.cache.max_left_text_width,
                    &mut view.left_scroll,
                    point,
                    delta,
                )
            {
                return true;
            }
            if view.mode != DiffMode::Off && view.inline_scroll.area.contains(point) {
                view.inline_scroll.scroll(&mut tab.editor, horizontal, delta);
                return true;
            }
        }
        false
    }

    pub(crate) fn source_control_diff_disk_changed(
        &self,
        change: &crate::background::WorkspacePathChange,
    ) -> bool {
        self.tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .any(|tab| {
                self.diff_tabs.staged_for_tab(tab) == Some(false)
                    && tab.editor.file_path.as_ref() == Some(&change.path)
                    && !tab.editor.bg_loading
                    && (tab.editor.disk_unavailable == change.exists
                        || (change.exists
                            && (tab.editor.last_disk_modified != change.modified
                                || change.len.is_some_and(|len| len != tab.editor.file_size))))
            })
    }

    pub(crate) fn cancel_source_control_diff_refresh(&mut self) {
        self.diff_tabs.refresh_generation = self.diff_tabs.refresh_generation.wrapping_add(1);
        self.diff_tabs.refresh_running = false;
        for (&id, origin) in &mut self.diff_tabs.source_control {
            origin.epoch = origin.epoch.wrapping_add(1);
            origin.dirty = Dirty::ALL;
            origin.inflight = Dirty::default();
            self.diff_tabs.dirty.insert(id);
        }
    }

    pub(crate) fn set_source_control_diff_revisions(&mut self, revisions: Option<crate::git_diff_sources::Revisions>) {
        if let Some(origin) = self.active_diff_tab_id().and_then(|id| self.diff_tabs.source_control.get_mut(&id)) {
            origin.revisions = revisions;
        }
    }

    /// Explicit/metadata refresh: revalidate visible views and invalidate hidden
    /// ones. Status letters cannot detect restaging an already modified file.
    #[cfg(test)]
    pub(crate) fn refresh_source_control_diffs(&mut self) {
        self.refresh_source_control_diffs_for_paths(None);
    }

    pub(crate) fn refresh_source_control_diffs_for_paths(&mut self, paths: Option<&rustc_hash::FxHashSet<PathBuf>>) {
        self.invalidate_source_control_diffs(paths, true);
        self.refresh_dirty_source_control_diffs();
    }

    pub(crate) fn refresh_source_control_diff_git_changed(&mut self, paths: &rustc_hash::FxHashSet<PathBuf>) {
        self.invalidate_source_control_diffs(None, false);
        self.invalidate_source_control_diffs(Some(paths), true);
        self.refresh_dirty_source_control_diffs();
    }

    fn invalidate_source_control_diffs(&mut self, paths: Option<&rustc_hash::FxHashSet<PathBuf>>, worktree: bool) {
        if !self.source_control_enabled { return; }
        // Normalize each event once, then hash prefixes. Large watcher bursts
        // must not compare every dirty path with every open tab.
        let relative = |path: &Path| crate::native_paths::strip_prefix(path, &self.explorer.root)
            .or_else(|| self.repositories.canonical_workspace.as_deref().and_then(|root| crate::native_paths::strip_prefix(path, root)))
            .unwrap_or(path).to_owned();
        let scopes = paths.and_then(|paths| {
            let mut normalized = Vec::with_capacity(paths.len());
            for path in paths {
                if !path.is_absolute() || path.to_str().is_none() || path.components().any(|part| {
                    part == std::path::Component::ParentDir || part.as_os_str().to_str().is_some_and(|name| {
                        if cfg!(windows) { name.eq_ignore_ascii_case(".git") } else { name == ".git" }
                    })
                }) { return None; }
                let path = relative(path);
                if path.as_os_str().is_empty() { return None; }
                normalized.push(path);
            }
            let mut scopes = crate::app_git::refresh::WorktreePaths::new(normalized);
            scopes.set_ignore_case(cfg!(windows));
            Some(scopes)
        });
        for tab in self.tabs.iter().chain(self.groups.iter().flat_map(|group| &group.tabs)) {
            let TabKind::Diff(id) = tab.kind else { continue; };
            let Some(origin) = self.diff_tabs.source_control.get_mut(&id) else { continue; };
            if let Some(scopes) = &scopes {
                if origin.staged || !tab.editor.file_path.as_deref().is_some_and(|path| scopes.contains(&relative(path).to_string_lossy())) { continue; }
            }
            origin.epoch = origin.epoch.wrapping_add(1);
            origin.dirty.git |= scopes.is_none();
            origin.dirty.worktree |= worktree && !origin.staged;
            self.diff_tabs.dirty.insert(id);
        }
    }

    /// Only displayed panes may perform I/O; hidden tabs keep their dirty bits.
    pub(crate) fn refresh_dirty_source_control_diffs(&mut self) {
        if self.rendering_inactive_editor || !self.source_control_enabled || self.diff_tabs.refresh_running || self.diff_tabs.dirty.is_empty() { return; }
        let mut requests = Vec::new();
        let mut add = |tab: Option<&Tab>| {
            if let Some(tab) = tab {
                let TabKind::Diff(id) = tab.kind else { return; };
                if !self.diff_tabs.dirty.contains(&id) || tab.editor.bg_loading { return; }
                let (base, version, pending) = if self.diff_tabs.owner == Some(id) {
                    (
                        self.compare_base_content.as_ref(),
                        self.compare_diff_target_version,
                        self.pending_git_compare_path.is_some(),
                    )
                } else {
                    let Some(view) = self.diff_tabs.views.get(&id) else { return; };
                    (view.base.as_ref(), view.target_version, view.pending)
                };
                if pending { return; }
                let (Some(base), Some(path), Some(origin)) = (base, &tab.editor.file_path, self.diff_tabs.source_control.get(&id)) else { return; };
                requests.push(DiffRefreshRequest {
                    id,
                    epoch: origin.epoch,
                    path: path.clone(),
                    repository: self.diff_tabs.repository_for_tab(tab).map(Path::to_owned)
                        .or_else(|| self.repository_hint_for_path(path)),
                    staged: origin.staged,
                    base: base.clone(),
                    target: crate::editor::LoadedTextFile { remote_checkpoint: None, rope: tab.editor.rope.clone(), encoding: tab.editor.encoding, file_size: tab.editor.file_size, last_disk_modified: tab.editor.last_disk_modified },
                    missing: tab.editor.disk_unavailable,
                    revisions: origin.revisions,
                    dirty: origin.dirty,
                    cache_current: version == Some(tab.editor.content_version),
                });
            }
        };
        add(self.tabs.get(self.active_tab));
        if let Some(tree) = &self.split_tree {
            tree.visit_leaves(&mut |group| {
                if group != self.active_group && let Some(group) = self.groups.get(group) { add(group.tabs.get(group.active_tab)); }
            });
        } else if self.split_active() {
            for (index, group) in self.groups.iter().enumerate() {
                if index != self.active_group && self.group_in_current_desk(index) { add(group.tabs.get(group.active_tab)); }
            }
        }
        if requests.is_empty() {
            return;
        }
        for request in &requests {
            self.diff_tabs.dirty.remove(&request.id);
            if let Some(origin) = self.diff_tabs.source_control.get_mut(&request.id) {
                origin.inflight = origin.dirty;
                origin.dirty = Dirty::default();
            }
        }
        self.diff_tabs.refresh_running = true;
        self.diff_tabs.refresh_generation = self.diff_tabs.refresh_generation.wrapping_add(1);
        let generation = self.diff_tabs.refresh_generation;
        let root = self.explorer.root.clone();
        let size_limit = self.large_file_threshold_mb.saturating_mul(1024 * 1024);
        let sender = self.source_control_sender();
        let remote = self.remote_workspace.clone();
        let log = self.git_output();
        self.background.spawn("source-control-diffs", move || {
            if sender.is_cancelled() { return; }
            let mut repositories = rustc_hash::FxHashMap::default();
            let updates = requests
                .into_iter()
                .take_while(|_| !sender.is_cancelled())
                .map(|request| {
                    if let Some(remote) = &remote {
                        return SourceControlDiffUpdate { id: request.id, epoch: request.epoch, result: prepare_remote_refresh(remote, request, size_limit, sender.cancellation()) };
                    }
                    let id = request.id;
                    let epoch = request.epoch;
                    let operation = crate::git_output::native(&log, "Read diff", &request.path);
                    if !request.dirty.git && request.revisions.is_some() {
                        let result = prepare_refresh(None, request, size_limit);
                        operation.result(&result, |_| "Worktree diff prepared; Git objects reused".into());
                        return SourceControlDiffUpdate { id, epoch, result };
                    }
                    let key = request.repository.clone().unwrap_or_else(|| request.path.parent().unwrap_or(&request.path).to_owned());
                    let repo = repositories.entry(key).or_insert_with(|| crate::git_repositories::open_file_repository(&request.path, request.repository.as_deref()));
                    let result = match repo {
                            Ok(repo) => prepare_refresh(Some(repo), request, size_limit),
                            Err(error) => Err(error.to_string()),
                        };
                    operation.result(&result, |_| "Diff prepared".into());
                    SourceControlDiffUpdate { id, epoch, result }
                })
                .collect();
            let _ = sender.send(crate::background::BackgroundResult::SourceControlDiffs {
                root,
                generation,
                updates,
            });
        });
    }

    pub(crate) fn finish_source_control_diff_refresh(
        &mut self,
        root: PathBuf,
        generation: u64,
        updates: Vec<SourceControlDiffUpdate>,
    ) -> bool {
        if root != self.explorer.root || generation != self.diff_tabs.refresh_generation {
            return false;
        }
        self.diff_tabs.refresh_running = false;
        let mut changed = false;
        for update in updates {
            let id = update.id;
            let Some(origin) = self.diff_tabs.source_control.get_mut(&id) else { continue; };
            let inflight = std::mem::take(&mut origin.inflight);
            if origin.epoch != update.epoch {
                // The old result may contain a newer index revision as well
                // as worktree bytes. Retrying only the last event loses that.
                origin.dirty.git |= inflight.git;
                origin.dirty.worktree |= inflight.worktree;
                self.diff_tabs.dirty.insert(id);
                continue;
            }
            let active = self.diff_tabs.owner == Some(id);
            // A newly requested initial compare owns this tab until it completes.
            let initial_compare_pending = if active {
                self.pending_git_compare_path.is_some() || self.compare_base_content.is_none()
            } else {
                self.diff_tabs
                    .views
                    .get(&id)
                    .is_none_or(|view| view.pending)
            };
            if initial_compare_pending {
                let origin = self.diff_tabs.source_control.get_mut(&id).unwrap();
                origin.dirty.git |= inflight.git;
                origin.dirty.worktree |= inflight.worktree;
                self.diff_tabs.dirty.insert(id);
                continue;
            }
            match update.result {
                Ok(prepared) => {
                    self.diff_tabs.source_control.get_mut(&id).unwrap().revisions = Some(prepared.revisions);
                    let Some(tab) = self
                        .tabs
                        .iter_mut()
                        .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
                        .find(|tab| tab.kind == TabKind::Diff(id))
                    else {
                        continue;
                    };
                    let editor = &mut tab.editor;
                    if prepared.target_changed {
                        let (line, col, scroll, horizontal, version) = (
                            editor.cursor_line,
                            editor.cursor_col,
                            editor.scroll_offset,
                            editor.horizontal_scroll,
                            editor.content_version,
                        );
                        editor.apply_bg_loaded(
                            prepared.target.rope,
                            prepared.target.encoding,
                            prepared.target.file_size,
                            self.large_file_threshold_mb.saturating_mul(1024 * 1024),
                        );
                        editor.content_version = version.wrapping_add(1);
                        editor.cursor_line = line.min(editor.line_count().saturating_sub(1));
                        editor.cursor_col = col.min(editor.line_len(editor.cursor_line));
                        editor.scroll_offset = scroll.min(editor.line_count().saturating_sub(1));
                        editor.horizontal_scroll = horizontal;
                    }
                    editor.last_disk_modified = prepared.target.last_disk_modified;
                    editor.encoding = prepared.target.encoding;
                    changed |= editor.disk_unavailable != prepared.missing;
                    editor.disk_unavailable = prepared.missing;
                    editor.file_size = prepared.target.file_size;
                    let Some(diff) = prepared.diff else {
                        continue;
                    };
                    editor.set_git_line_status(diff.target_line_markers);
                    let version = Some(editor.content_version);
                    let cache = Arc::new(diff.render_cache);
                    if active {
                        // Supersede any older generic refresh of this view.
                        self.diff_refresh_generation = self.diff_refresh_generation.wrapping_add(1);
                        self.bg_diff_refresh_running = false;
                        self.compare_base_content = Some(diff.base_content);
                        self.compare_diff_lines = diff.diff_lines;
                        self.diff_render_cache = cache;
                        self.compare_diff_target_version = version;
                        self.compare_diff_base_revision = Some(DiffBaseRevision::Snapshot);
                    } else if let Some(view) = self.diff_tabs.views.get_mut(&id) {
                        view.scope_source.forget_original_view(&mut self.animations);
                        view.scope_source = Default::default();
                        view.base = Some(diff.base_content);
                        view.lines = diff.diff_lines;
                        view.cache = cache;
                        view.target_version = version;
                        view.base_revision = Some(DiffBaseRevision::Snapshot);
                    }
                    changed = true;
                }
                Err(error) => {
                    if active {
                        self.clear_diff_state();
                        self.notify_error(error);
                    } else {
                        self.forget_cached_diff_view(id);
                    }
                    changed = true;
                }
            }
        }
        self.refresh_dirty_source_control_diffs();
        changed
    }

    pub(crate) fn clear_commit_diff_view(&mut self) {
        self.commit_details.diff_full_cache = None;
        if self.diff_tabs.owner == Some(COMMIT_DIFF_ID) {
            self.clear_diff_state();
            self.diff_tabs.owner = None;
        }
        self.forget_diff_tab(COMMIT_DIFF_ID);
    }

    pub(crate) fn install_commit_diff(&mut self, snapshot: crate::commit_details::CommitDiff) {
        // Results may arrive with another file or diff focused. Cache them for
        // the browsing tab without touching that editor or activating a tab.
        self.sync_diff_tab();
        let view = self.make_commit_file_view(snapshot);
        self.put_commit_file_view(view);
    }

    pub(crate) fn make_commit_file_view(&self, snapshot: crate::commit_details::CommitDiff) -> CommitFileView {
        let mut editor = self.new_editor();
        editor.language_override = self.language_override_for_path(&snapshot.path);
        editor.content_version = self.tabs.iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .find(|tab| tab.kind == TabKind::CommitDetails)
            .map_or(1, |tab| tab.editor.content_version.wrapping_add(1));
        editor.file_path = Some(snapshot.path.clone());
        editor.file_size = snapshot.target.len_bytes() as u64;
        editor.rope = snapshot.target;
        editor.saved_rope = Some(editor.rope.clone());
        editor.read_only = true;
        editor.syntax_dirty_from = Some(0);
        editor.syntax_dirty_through = Some(editor.rope.len_lines());
        editor.detect_indentation();
        let first = snapshot.prepared.render_cache.hunks.first()
            .map_or(0, |h| h.target_start);
        editor.set_cursor(first.min(editor.line_count().saturating_sub(1)), 0);
        editor.scroll_offset = first.saturating_sub(5);
        editor.set_git_line_status(snapshot.prepared.target_line_markers);
        let full_cache = Arc::new(snapshot.prepared.render_cache);
        let cache = if self.commit_details.diff_context.changes_only {
            Arc::new(full_cache.with_context(self.commit_details.diff_context.lines))
        } else { Arc::clone(&full_cache) };
        let version = editor.content_version;
        // Approximate residency conservatively without scanning all diff rows
        // during a frame. This is a cache budget, not a file-size limit.
        let bytes = editor.rope.len_bytes().saturating_add(snapshot.prepared.base_content.len())
            .saturating_mul(6);
        let view = DiffViewState {
            mode: self.preferred_diff_mode,
            base_path: None,
            base: Some(snapshot.prepared.base_content),
            base_rope: None,
            lines: snapshot.prepared.diff_lines,
            cache,
            target_version: Some(version),
            base_revision: Some(DiffBaseRevision::Snapshot),
            label: None,
            labels: Some(snapshot.labels),
            target: Some(snapshot.path),
            left_scroll: 0,
            split_area: None,
            left_code_area: None,
            inline_scroll: Default::default(),
            selection: Default::default(),
            scope_source: Default::default(),
            split_ratio: self.diff_split_ratio,
            pending: false,
        };
        CommitFileView { editor, view, full: full_cache, revealed: None, highlight_key: None,
            highlight_cancel: None, bytes, last_used: 0 }
    }

    pub(crate) fn put_commit_file_view(&mut self, file: CommitFileView) {
        let CommitFileView { editor, view, full, revealed, highlight_key, highlight_cancel, .. } = file;
        let Some(tab) = self.tabs.iter_mut()
            .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
            .find(|tab| tab.kind == TabKind::CommitDetails)
        else { return; };
        tab.editor = editor;
        self.commit_details.diff_full_cache = Some(full);
        self.commit_details.context_revealed_line = revealed;
        if self.is_commit_details_tab() {
            view.install(self);
            self.visible_highlight_key = highlight_key;
            self.visible_highlight_cancel_token = highlight_cancel;
        } else {
            self.cache_diff_view(COMMIT_DIFF_ID, view);
        }
    }

    pub(crate) fn take_commit_file_view(&mut self) -> Option<CommitFileView> {
        let full = self.commit_details.diff_full_cache.take()?;
        let active = self.is_commit_details_tab();
        let view = if active { DiffViewState::take(self) }
            else { self.diff_tabs.views.remove(&COMMIT_DIFF_ID)? };
        let tab = self.tabs.iter_mut()
            .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
            .find(|tab| tab.kind == TabKind::CommitDetails)?;
        let editor = std::mem::replace(&mut tab.editor, crate::editor::Editor::new());
        let bytes = editor.rope.len_bytes().saturating_add(view.base.as_ref().map_or(0, |base| base.len()))
            .saturating_mul(6);
        Some(CommitFileView { editor, view, full,
            revealed: self.commit_details.context_revealed_line.take(),
            highlight_key: if active { self.visible_highlight_key.take() } else { None },
            highlight_cancel: if active { self.visible_highlight_cancel_token.take() } else { None },
            bytes, last_used: 0 })
    }

    fn cache_diff_view(&mut self, id: u64, view: DiffViewState) {
        if let Some(previous) = self.diff_tabs.views.insert(id, view) {
            previous.scope_source.forget_original_view(&mut self.animations);
        }
    }

    fn forget_cached_diff_view(&mut self, id: u64) {
        if let Some(view) = self.diff_tabs.views.remove(&id) {
            view.scope_source.forget_original_view(&mut self.animations);
        }
    }

    pub(crate) fn forget_diff_tab(&mut self, id: u64) {
        self.forget_cached_diff_view(id);
        self.diff_tabs.source_control.remove(&id);
        self.diff_tabs.dirty.remove(&id);
        self.diff_tabs.blame_revisions.remove(&id);
    }

    pub(crate) fn active_diff_tab_id(&self) -> Option<u64> {
        match self.tabs.get(self.active_tab)?.kind {
            TabKind::Diff(id) => Some(id),
            TabKind::CommitDetails => Some(COMMIT_DIFF_ID),
            _ => None,
        }
    }

    pub(crate) fn suspend_diff_tab(&mut self) {
        if let Some(id) = self.diff_tabs.owner.take() {
            let view = DiffViewState::take(self);
            self.cache_diff_view(id, view);
            // Invalidate unfinished workers so they cannot install a result
            // into a regular editor for the same path. Completed caches stay.
            self.clear_diff_state();
        }
    }

    pub(crate) fn sync_diff_tab(&mut self) {
        let target = self.active_diff_tab_id();
        if target == self.diff_tabs.owner {
            self.refresh_dirty_source_control_diffs();
            return;
        }
        self.suspend_diff_tab();
        let live: std::collections::HashSet<_> = self
            .tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .filter_map(|tab| match tab.kind {
                TabKind::Diff(id) => Some(id),
                TabKind::CommitDetails => Some(COMMIT_DIFF_ID),
                _ => None,
            })
            .collect();
        let animations = &mut self.animations;
        self.diff_tabs.views.retain(|id, view| {
            if live.contains(id) {
                true
            } else {
                view.scope_source.forget_original_view(animations);
                false
            }
        });
        self.diff_tabs.source_control.retain(|id, _| live.contains(id));
        self.diff_tabs.dirty.retain(|id| live.contains(id));
        self.diff_tabs.blame_revisions.retain(|id, _| live.contains(id));
        if let Some(id) = target {
            self.clear_diff_state();
            self.diff_tabs.owner = Some(id);
            let restart = if let Some(view) = self.diff_tabs.views.remove(&id) {
                let pending = view.pending;
                view.install(self);
                pending
            } else {
                true
            };
            if restart && id != COMMIT_DIFF_ID {
                if self.source_control_enabled {
                    self.request_compare_with_git_async();
                } else {
                    self.pending_git_compare_path = self.editor().and_then(|editor| editor.file_path.clone());
                }
            } else if !self.workspace_watcher_active && let Some(origin) = self.diff_tabs.source_control.get_mut(&id) {
                // Without a watcher, returning to a cached view is a full
                // validation boundary, even if no event identified the file.
                origin.epoch = origin.epoch.wrapping_add(1);
                origin.dirty = Dirty::ALL;
                self.diff_tabs.dirty.insert(id);
            }
        }
        self.refresh_dirty_source_control_diffs();
    }

    /// Start a native diff tab for immutable revisions. The caller installs
    /// both snapshots before returning to the event loop.
    pub(crate) fn new_snapshot_diff_tab(&mut self) {
        self.auto_save_on_focus_change();
        self.create_snapshot_diff_tab();
    }

    /// Create the view without attempting another save of its source document.
    pub(crate) fn create_snapshot_diff_tab(&mut self) {
        self.push_nav_history();
        self.suspend_diff_tab();
        self.clear_diff_state();
        self.clear_transient_editor_state();
        self.diff_tabs.next_id += 1;
        let id = self.diff_tabs.next_id;
        let mut tab = Tab::new_file(self.new_editor());
        tab.kind = TabKind::Diff(id);
        tab.editor.read_only = true;
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.diff_tabs.owner = Some(id);
        self.active_pane = ActivePane::Editor;
        self.terminal_panel_focused = false;
        self.settings_editor_active = false;
        self.touch_tab_mru(self.active_tab);
        self.ensure_active_tab_visible();
    }

    #[cfg(test)]
    pub(crate) fn open_source_control_diff_tab(
        &mut self,
        path: &Path,
        deleted: bool,
        staged: bool,
    ) {
        let repository = self.repository_hint_for_path(path).or_else(|| {
            crate::git_repositories::open_file_repository(path, None).ok()
                .and_then(|repo| repo.workdir().map(Path::to_owned))
        });
        self.open_source_control_diff_in_repository(path, deleted, staged, repository);
    }

    pub(crate) fn open_source_control_diff_in_repository(
        &mut self, path: &Path, deleted: bool, staged: bool, repository: Option<PathBuf>,
    ) {
        self.auto_save_on_focus_change();
        self.push_nav_history();
        let path = Self::open_target_path(path);
        let matches = |tab: &Tab| {
            matches!(tab.kind, TabKind::Diff(_))
                && self.diff_tabs.staged_for_tab(tab) == Some(staged)
                && match (self.diff_tabs.repository_for_tab(tab), repository.as_deref()) {
                    (Some(existing), Some(requested)) => Self::same_open_path(existing, requested),
                    (None, None) => true,
                    _ => false,
                }
                && tab
                    .editor
                    .file_path
                    .as_ref()
                    .is_some_and(|other| Self::same_open_path(other, &path))
        };
        if let Some(index) = self.tabs.iter().position(matches) {
            self.active_tab = index;
        } else if let Some((group, index)) =
            self.groups.iter().enumerate().find_map(|(group, state)| {
                state
                    .tabs
                    .iter()
                    .position(matches)
                    .map(|index| (group, index))
            })
        {
            self.switch_group(group);
            self.active_tab = index;
        } else {
            let mut editor = if deleted || staged {
                let mut editor = self.new_editor();
                editor.file_path = Some(path.clone());
                editor.disk_unavailable = deleted;
                editor.saved_rope = Some(editor.rope.clone());
                editor
            } else {
                crate::editor::Editor::new_loading(&path, 0)
            };
            editor.read_only = true;
            editor.language_override = self.language_override_for_path(&path);
            editor.language_associations = self.language_associations.clone();
            editor.tab_size = self.default_tab_size;
            self.diff_tabs.next_id += 1;
            self.diff_tabs.source_control.insert(self.diff_tabs.next_id, SourceControlIdentity {
                staged, repository, revisions: None, epoch: 0, dirty: Dirty::default(), inflight: Dirty::default(),
            });
            let mut tab = Tab::new_file(editor);
            tab.kind = TabKind::Diff(self.diff_tabs.next_id);
            self.tabs.push(tab);
            self.active_tab = self.tabs.len() - 1;
            if !deleted && !staged {
                self.queue_background_file_load(path, true);
            }
        }
        self.active_pane = ActivePane::Editor;
        self.touch_tab_mru(self.active_tab);
        self.sync_diff_tab();
    }

    /// Compare a blamed commit with its parent without changing the working file.
    pub(crate) fn open_blame_history(&mut self, history: crate::git_blame::History) {
        self.push_nav_history();
        if let Some(index) = self.tabs.iter().position(|tab| match tab.kind {
            TabKind::Diff(id) => self.diff_tabs.blame_revisions.get(&id).is_some_and(|(_, revision)| revision.path == history.revision.path
                && revision.workdir == history.revision.workdir && revision.hash == history.hash),
            _ => false,
        }) {
            self.active_tab = index;
            self.sync_diff_tab();
            if let Some(editor) = self.editor_mut() { editor.set_cursor(history.line, 0); }
            return;
        }
        self.suspend_diff_tab();
        let mut editor = self.new_editor();
        editor.language_override = self.language_override_for_path(&history.file_path);
        editor.file_path = Some(history.file_path.clone());
        editor.file_size = history.contents.len_bytes() as u64;
        editor.rope = history.contents;
        editor.saved_rope = Some(editor.rope.clone());
        editor.read_only = true;
        editor.modified = false;
        editor.syntax_dirty_from = Some(0);
        editor.syntax_dirty_through = Some(editor.rope.len_lines());
        editor.set_cursor(history.line, 0);
        self.diff_tabs.next_id += 1;
        let id = self.diff_tabs.next_id;
        let mut tab = Tab::new_file(editor);
        tab.kind = TabKind::Diff(id);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.clear_diff_state();
        self.diff_tabs.owner = Some(id);
        self.diff_tabs.blame_revisions.insert(id, (history.previous_revision, history.revision));
        self.compare_base_content = Some(history.diff.base_content);
        self.diff_labels = Some(DiffLabels::new(history.before_label, history.after_label));
        self.diff_target_path = Some(history.file_path.clone());
        self.diff_tab_label = Some(format!("{} ({} · {})", history.file_path.file_name().unwrap_or_default().to_string_lossy(),
            history.hash.chars().take(7).collect::<String>(), history.summary));
        self.diff_mode = self.preferred_diff_mode;
        self.apply_prepared_compare_diff(history.diff.diff_lines, history.diff.render_cache, history.diff.target_line_markers);
        self.active_pane = ActivePane::Editor;
        self.terminal_panel_focused = false;
        self.touch_tab_mru(self.active_tab);
    }

    /// An immutable agent-provided snapshot uses the native diff renderer but
    /// has no Git identity, so repository refreshes cannot replace its base.
    pub(crate) fn open_acp_diff(&mut self, diff: &vtcode::acp::protocol::Diff) {
        self.suspend_diff_tab();
        let mut editor = self.new_editor();
        editor.language_override = self.language_override_for_path(&diff.path);
        editor.file_path = Some(diff.path.clone());
        editor.rope = ropey::Rope::from_str(&diff.new_text);
        editor.saved_rope = Some(editor.rope.clone());
        editor.read_only = true;
        editor.modified = false;
        self.diff_tabs.next_id += 1;
        let id = self.diff_tabs.next_id;
        let mut tab = Tab::new_file(editor);
        tab.kind = TabKind::Diff(id);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.clear_diff_state();
        self.diff_tabs.owner = Some(id);
        self.compare_base_content = Some(diff.old_text.clone().unwrap_or_default().into());
        self.set_compare_diff_lines(crate::app::compute_file_diff(diff.old_text.as_deref().unwrap_or_default(), &diff.new_text));
        self.compare_diff_target_version = self.editor().map(|e| e.content_version);
        self.compare_diff_base_revision = Some(DiffBaseRevision::Snapshot);
        self.diff_labels = Some(DiffLabels::new("Before agent edit", "Agent result"));
        self.diff_target_path = Some(diff.path.clone());
        self.diff_tab_label = Some(format!("{} (Agent diff)", diff.path.file_name().unwrap_or_default().to_string_lossy()));
        self.diff_mode = self.preferred_diff_mode;
        self.active_pane = ActivePane::Editor;
        self.terminal_panel_focused = false;
        self.touch_tab_mru(self.active_tab);
    }

    // Split rendering temporarily swaps the visible tab list. Project its
    // cached view without cancelling or starting any active-tab workers.
    pub(crate) fn begin_inactive_diff_view(&mut self) -> Option<DiffViewState> {
        let id = self.active_diff_tab_id();
        if id.is_none() && self.diff_tabs.owner.is_none() {
            return None;
        }
        let saved = DiffViewState::take(self);
        if let Some(view) = id.and_then(|id| self.diff_tabs.views.remove(&id)) {
            view.install(self);
        }
        Some(saved)
    }

    pub(crate) fn end_inactive_diff_view(&mut self, saved: Option<DiffViewState>) {
        if let Some(saved) = saved {
            if let Some(id) = self.active_diff_tab_id() {
                let mut view = DiffViewState::take(self);
                // A pending active compare belongs to the focused tab.
                view.pending = view.mode == DiffMode::Off && view.base.is_none();
                self.cache_diff_view(id, view);
            }
            saved.install(self);
        }
    }
}
