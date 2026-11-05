//! A small, short-lived cache of comparisons the user actually opened.
//! Restored views are painted immediately, then revalidated on a worker.
use super::*;
use std::{collections::VecDeque, time::{Duration, Instant}};

pub(super) const MAX_ENTRIES: usize = 8;
pub(super) const MAX_BYTES: usize = 16 * 1024 * 1024;
pub(super) const LIFETIME: Duration = Duration::from_secs(60);

pub(super) struct Entry {
    pub path: PathBuf,
    pub repository: Option<PathBuf>,
    pub staged: bool,
    pub missing: bool,
    pub revisions: crate::git_diff_sources::Revisions,
    pub target: crate::editor::LoadedTextFile,
    pub view: DiffViewState,
    markers: crate::editor::PreparedGitLineStatus,
    position: (usize, usize, usize, usize, usize),
    version: u64,
    pub closed: Instant,
}

impl Entry {
    pub fn memory_bytes(&self) -> usize {
        // Conservatively include duplicated strings, row maps and per-line
        // rendering data without walking or flattening the file on close.
        self.target.rope.len_bytes().saturating_add(self.view.base.as_ref().map_or(0, |base| base.len()))
            .saturating_mul(8).saturating_add(self.view.cache.rows.len()
                .saturating_add(self.view.cache.inline_rows.len()).saturating_mul(512))
    }

    fn matches(&self, path: &Path, repository: Option<&Path>, staged: bool) -> bool {
        self.staged == staged && App::same_open_path(&self.path, path)
            && match (self.repository.as_deref(), repository) {
                (Some(a), Some(b)) => App::same_open_path(a, b),
                (None, None) => true,
                _ => false,
            }
    }

    pub fn editor(&self, mut editor: crate::editor::Editor, limit: u64) -> crate::editor::Editor {
        editor.apply_bg_loaded(self.target.rope.clone(), self.target.encoding, self.target.file_size, limit);
        editor.file_path = Some(self.path.clone());
        editor.last_disk_modified = self.target.last_disk_modified;
        editor.remote_checkpoint = self.target.remote_checkpoint.clone();
        editor.disk_unavailable = self.missing;
        editor.content_version = self.version;
        editor.read_only = true;
        editor.set_prepared_git_line_status(self.markers.clone());
        (editor.cursor_line, editor.cursor_col, editor.scroll_offset,
            editor.horizontal_scroll, editor.wrap_scroll_offset) = self.position;
        editor
    }
}

#[derive(Default)]
pub(super) struct Cache {
    pub entries: VecDeque<Entry>,
}

impl Cache {
    pub fn take(&mut self, path: &Path, repository: Option<&Path>, staged: bool, missing: bool,
        limit: u64, now: Instant) -> Option<Entry> {
        self.entries.retain(|entry| now.saturating_duration_since(entry.closed) < LIFETIME);
        let index = self.entries.iter().position(|entry| entry.matches(path, repository, staged))?;
        let entry = self.entries.remove(index)?;
        (entry.missing == missing && entry.target.file_size <= limit).then_some(entry)
    }

    pub fn insert(&mut self, entry: Entry) {
        let now = Instant::now();
        self.entries.retain(|old| now.saturating_duration_since(old.closed) < LIFETIME
            && !old.matches(&entry.path, entry.repository.as_deref(), entry.staged));
        let bytes = entry.memory_bytes();
        if bytes > MAX_BYTES || now.saturating_duration_since(entry.closed) >= LIFETIME { return; }
        while self.entries.len() >= MAX_ENTRIES
            || self.entries.iter().map(Entry::memory_bytes).sum::<usize>().saturating_add(bytes) > MAX_BYTES {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }
}

impl App {
    pub(crate) fn remember_closed_source_control_diffs(&mut self, plan: &[(usize, Vec<usize>)]) {
        let candidates: Vec<_> = plan.iter().flat_map(|(group, indices)| indices.iter().map(move |index| (*group, *index)))
            .filter(|(group, index)| {
                let tab = if *group == self.active_group { self.tabs.get(*index) }
                    else { self.groups.get(*group).and_then(|group| group.tabs.get(*index)) };
                tab.is_some_and(|tab| self.diff_tabs.staged_for_tab(tab).is_some())
            }).take(MAX_ENTRIES).collect();
        for (group, index) in candidates { self.remember_closed_source_control_diff(group, index); }
    }

    /// Move completed render data out before normal close handling releases its
    /// tab identity. No document is saved or retained as an editable buffer.
    fn remember_closed_source_control_diff(&mut self, group: usize, index: usize) {
        let tab = if group == self.active_group { self.tabs.get(index) }
            else { self.groups.get(group).and_then(|group| group.tabs.get(index)) };
        let Some(tab) = tab else { return; };
        let TabKind::Diff(id) = tab.kind else { return; };
        // A diff shared by desks/groups still belongs to its remaining view.
        if self.tabs.iter().chain(self.groups.iter().flat_map(|group| &group.tabs))
            .filter(|tab| tab.kind == TabKind::Diff(id)).take(2).count() > 1 { return; }
        let Some(origin) = self.diff_tabs.source_control.get(&id) else { return; };
        let Some(revisions) = origin.revisions else { return; };
        // A re-opened snapshot may close again before its validation finishes.
        // Keep its original expiry while no newer event invalidated the view.
        let cached_at = origin.validating_cached.filter(|_| origin.epoch == 0);
        if tab.editor.bg_loading || tab.editor.load_failed || tab.editor.large_file_mode
            || (cached_at.is_none() && (origin.dirty.git || origin.dirty.worktree
                || origin.inflight.git || origin.inflight.worktree)) {
            return;
        }
        let Some(path) = tab.editor.file_path.clone() else { return; };
        let staged = origin.staged;
        let repository = origin.repository.clone();
        let editor = &tab.editor;
        let target = crate::editor::LoadedTextFile {
            remote_checkpoint: editor.remote_checkpoint.clone(), rope: editor.rope.clone(),
            encoding: editor.encoding, file_size: editor.file_size, last_disk_modified: editor.last_disk_modified,
        };
        let markers = editor.prepared_git_line_status();
        let missing = editor.disk_unavailable;
        let version = editor.content_version;
        let position = (editor.cursor_line, editor.cursor_col, editor.scroll_offset,
            editor.horizontal_scroll, editor.wrap_scroll_offset);
        if self.diff_tabs.owner == Some(id) { self.suspend_diff_tab(); }
        let Some(mut view) = self.diff_tabs.views.remove(&id) else { return; };
        if view.pending || view.base.is_none() || view.target_version != Some(version) {
            view.scope_source.forget_original_view(&mut self.animations);
            return;
        }
        view.scope_source.forget_original_view(&mut self.animations);
        view.scope_source = Default::default();
        view.selection = Default::default();
        view.split_area = None;
        view.left_code_area = None;
        self.diff_tabs.recent.insert(Entry { path, repository, staged, missing, revisions,
            target, view, markers, position, version, closed: cached_at.unwrap_or_else(Instant::now) });
    }
}
