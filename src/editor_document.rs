//! A document is shared by every view, including views on hidden desks.
//! Editors retain cheap immutable Rope snapshots for rendering; transactions
//! read and publish one authoritative document and share the actual undo queues.
use crate::editor::{Editor, UndoState};
use ropey::Rope;
use std::{
    cell::Cell,
    collections::VecDeque,
    ops::{Deref, DerefMut},
    sync::{Arc, RwLock, RwLockReadGuard, RwLockWriteGuard},
};

#[derive(Clone, Default)]
pub(crate) struct HistoryStack(Arc<RwLock<VecDeque<UndoState>>>);
pub(crate) struct HistoryEnd<'a>(RwLockReadGuard<'a, VecDeque<UndoState>>, bool);
impl Deref for HistoryEnd<'_> {
    type Target = UndoState;
    fn deref(&self) -> &UndoState {
        if self.1 {
            self.0.back().unwrap()
        } else {
            self.0.front().unwrap()
        }
    }
}
pub(crate) struct HistoryEndMut<'a>(RwLockWriteGuard<'a, VecDeque<UndoState>>);
impl Deref for HistoryEndMut<'_> {
    type Target = UndoState;
    fn deref(&self) -> &UndoState {
        self.0.back().unwrap()
    }
}
impl DerefMut for HistoryEndMut<'_> {
    fn deref_mut(&mut self) -> &mut UndoState {
        self.0.back_mut().unwrap()
    }
}
impl HistoryStack {
    pub(crate) fn read(&self) -> RwLockReadGuard<'_, VecDeque<UndoState>> {
        self.0.read().unwrap()
    }
    pub(crate) fn write(&self) -> RwLockWriteGuard<'_, VecDeque<UndoState>> {
        self.0.write().unwrap()
    }
    pub(crate) fn len(&self) -> usize {
        self.read().len()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.read().is_empty()
    }
    pub(crate) fn clear(&self) {
        self.write().clear();
    }
    pub(crate) fn push_back(&self, state: UndoState) {
        self.write().push_back(state);
    }
    pub(crate) fn pop_back(&self) -> Option<UndoState> {
        self.write().pop_back()
    }
    pub(crate) fn pop_front(&self) -> Option<UndoState> {
        self.write().pop_front()
    }
    pub(crate) fn back(&self) -> Option<HistoryEnd<'_>> {
        let r = self.read();
        if r.is_empty() {
            None
        } else {
            Some(HistoryEnd(r, true))
        }
    }
    #[cfg(test)]
    pub(crate) fn front(&self) -> Option<HistoryEnd<'_>> {
        let r = self.read();
        if r.is_empty() {
            None
        } else {
            Some(HistoryEnd(r, false))
        }
    }
    pub(crate) fn back_mut(&self) -> Option<HistoryEndMut<'_>> {
        let r = self.write();
        if r.is_empty() {
            None
        } else {
            Some(HistoryEndMut(r))
        }
    }
    #[cfg(test)]
    pub(crate) fn shares(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[derive(Default)]
pub(crate) struct DocumentView {
    shared: Arc<SharedDocument>,
    seen: Cell<u64>,
    depth: usize,
    pub(crate) restore_view_only: bool,
}
#[derive(Default)]
struct SharedDocument {
    state: RwLock<Option<Document>>,
}
struct Document {
    revision: u64,
    rope: Rope,
    saved: Option<Rope>,
    modified: bool,
    version: u64,
    identity: u64,
    path: Option<std::path::PathBuf>,
    suggested: Option<String>,
    encoding: &'static str,
    saved_encoding: &'static str,
    saved_save_options: Option<crate::editor::SaveOptions>,
    remote_checkpoint: Option<Arc<crate::remote::Checkpoint>>,
    line_ending: &'static str,
    read_only: bool,
    disk_unavailable: bool,
    load_failed: bool,
    disk_modified: Option<std::time::SystemTime>,
    file_size: u64,
    large_file: bool,
    rich_features_timed_out: bool,
    loading: bool,
    language: Option<String>,
    tab_size: usize,
    use_tabs: bool,
    trim_trailing: Option<bool>,
    final_newline: Option<bool>,
    next_change: u64,
}
impl Document {
    fn capture(e: &Editor, revision: u64) -> Self {
        Self {
            revision,
            rope: e.rope.clone(),
            saved: e.saved_rope.clone(),
            modified: e.modified,
            version: e.content_version,
            identity: e.save_identity,
            path: e.file_path.clone(),
            suggested: e.suggested_file_name.clone(),
            encoding: e.encoding,
            saved_encoding: e.saved_encoding,
            saved_save_options: e.saved_save_options,
            remote_checkpoint: e.remote_checkpoint.clone(),
            line_ending: e.cached_line_ending,
            read_only: e.read_only,
            disk_unavailable: e.disk_unavailable,
            load_failed: e.load_failed,
            disk_modified: e.last_disk_modified,
            file_size: e.file_size,
            large_file: e.large_file_mode,
            rich_features_timed_out: e.rich_features_timed_out,
            loading: e.bg_loading,
            language: e.language_override.clone(),
            tab_size: e.tab_size,
            use_tabs: e.use_tabs,
            trim_trailing: e.ec_trim_trailing,
            final_newline: e.ec_insert_final_newline,
            next_change: e.next_undo_change_id,
        }
    }
    fn matches(&self, e: &Editor) -> bool {
        self.rope.is_instance(&e.rope)
            && self.modified == e.modified
            && self.version == e.content_version
            && self.identity == e.save_identity
            && self.path == e.file_path
            && self.suggested == e.suggested_file_name
            && self.encoding == e.encoding
            && self.saved_encoding == e.saved_encoding
            && self.saved_save_options == e.saved_save_options
            && self.remote_checkpoint == e.remote_checkpoint
            && self.line_ending == e.cached_line_ending
            && self.read_only == e.read_only
            && self.disk_unavailable == e.disk_unavailable
            && self.load_failed == e.load_failed
            && self.disk_modified == e.last_disk_modified
            && self.file_size == e.file_size
            && self.large_file == e.large_file_mode
            && self.rich_features_timed_out == e.rich_features_timed_out
            && self.loading == e.bg_loading
            && self.language == e.language_override
            && self.tab_size == e.tab_size
            && self.use_tabs == e.use_tabs
            && self.trim_trailing == e.ec_trim_trailing
            && self.final_newline == e.ec_insert_final_newline
            && self.next_change == e.next_undo_change_id
            && match (&self.saved, &e.saved_rope) {
                (Some(a), Some(b)) => a.is_instance(b),
                (None, None) => true,
                _ => false,
            }
    }
}

// Nested operations form one transaction: selection replacement, auto-pairs,
// multi-cursor editing and undo never publish intermediate document states.
macro_rules! edit {
    ($editor:ident, $body:block) => {{
        $editor.begin_document_edit();
        let result = (|| $body)();
        $editor.end_document_edit();
        result
    }};
}
pub(crate) use edit;

impl Editor {
    pub(crate) fn extension_buffer_snapshot(&self) -> crate::extension_buffer::Buffer {
        crate::extension_buffer::Buffer::from_rope(&self.rope)
    }

    pub(crate) fn set_remote_checkpoint(&mut self, checkpoint: Option<Arc<crate::remote::Checkpoint>>) {
        crate::editor_document::edit!(self, { self.remote_checkpoint = checkpoint; });
    }
    pub(crate) fn document_revision(&self) -> u64 {
        self.document.seen.get()
    }
    pub(crate) fn begin_document_edit(&mut self) {
        if self.document.depth == 0 {
            self.refresh_document();
        }
        self.document.depth += 1;
    }
    pub(crate) fn end_document_edit(&mut self) {
        self.document.depth -= 1;
        if self.document.depth == 0 {
            self.publish_document();
        }
    }
    pub(crate) fn publish_document(&self) {
        // Single-view files need neither a second snapshot nor a lock on typing.
        if Arc::strong_count(&self.document.shared) == 1 {
            return;
        }
        let mut shared = self.document.shared.state.write().unwrap();
        if let Some(doc) = shared.as_ref() {
            // A stale view can never overwrite a newer edit from another view.
            if self.document.seen.get() != doc.revision || doc.matches(self) {
                return;
            }
        }
        let revision = shared.as_ref().map_or(1, |d| d.revision.wrapping_add(1));
        *shared = Some(Document::capture(self, revision));
        self.document.seen.set(revision);
    }
    pub(crate) fn refresh_document(&mut self) {
        if Arc::strong_count(&self.document.shared) == 1 && self.document.seen.get() == 0 {
            return;
        }
        let handle = self.document.shared.clone();
        let shared = handle.state.read().unwrap();
        let Some(doc) = shared
            .as_ref()
            .filter(|d| d.revision != self.document.seen.get())
        else {
            return;
        };
        let text_changed = !self.rope.is_instance(&doc.rope);
        let saved_changed = match (&self.saved_rope, &doc.saved) {
            (Some(old), Some(new)) => !old.is_instance(new),
            (None, None) => false,
            _ => true,
        };
        let changed_lines = text_changed.then(|| {
            if self.large_file_mode || doc.large_file {
                // These views retain bounded viewport caches, not full-file
                // rich analysis. Invalidate those directly instead of walking
                // every shared leaf in a gigabyte to discover a tiny edit.
                // This also covers missed edits, replacements and undo/redo.
                return (0, self.rope.len_lines().max(doc.rope.len_lines()).saturating_sub(1), true);
            }
            let (old, new) = crate::rope_changes::changed_chars(&self.rope, &doc.rope);
            let first = self.rope.char_to_line(old.start).min(doc.rope.char_to_line(new.start));
            let last = self.rope.char_to_line(old.end).max(doc.rope.char_to_line(new.end));
            let multiline = self.rope.slice(old).len_lines() > 1
                || doc.rope.slice(new).len_lines() > 1;
            (first, last, multiline)
        });
        if text_changed {
            self.snippet_cancel();
            if !self.folded_ranges.is_empty() && changed_lines.unwrap().2 {
                self.clear_stale_folds();
            }
        }
        self.rope = doc.rope.clone();
        self.saved_rope = doc.saved.clone();
        self.modified = doc.modified;
        self.content_version = doc.version;
        self.save_identity = doc.identity;
        self.file_path.clone_from(&doc.path);
        self.suggested_file_name.clone_from(&doc.suggested);
        self.encoding = doc.encoding;
        self.saved_encoding = doc.saved_encoding;
        self.saved_save_options = doc.saved_save_options;
        self.remote_checkpoint = doc.remote_checkpoint.clone();
        self.cached_line_ending = doc.line_ending;
        self.read_only = doc.read_only;
        self.disk_unavailable = doc.disk_unavailable;
        self.load_failed = doc.load_failed;
        self.last_disk_modified = doc.disk_modified;
        self.file_size = doc.file_size;
        self.large_file_mode = doc.large_file;
        self.rich_features_timed_out = doc.rich_features_timed_out;
        if self.large_file_mode { self.pause_large_file_features(); }
        self.bg_loading = doc.loading;
        self.language_override.clone_from(&doc.language);
        let indentation_changed = self.tab_size != doc.tab_size;
        self.tab_size = doc.tab_size;
        self.use_tabs = doc.use_tabs;
        self.ec_trim_trailing = doc.trim_trailing;
        self.ec_insert_final_newline = doc.final_newline;
        self.next_undo_change_id = doc.next_change;
        self.document.seen.set(doc.revision);
        self.break_undo_group();
        if let Some((first, last, _)) = changed_lines {
            // Split panes and hidden desks share text, but retain their own
            // layout caches. Ordinary files retain precise invalidation;
            // large files discard only their bounded viewport caches.
            self.mark_syntax_dirty_range(first, last);
            self.invalidate_unsaved_diff_version();
        }
        if text_changed || indentation_changed {
            self.cached_visible_line_count = None;
            self.cached_wrapped_total = None;
            if indentation_changed { self.cached_row_offsets = None; }
        }
        if saved_changed {
            self.invalidate_unsaved_diff_cache();
        }
        if !text_changed { return; }
        self.clamp_cursor();
        let rope = &self.rope;
        let clamp = |(line, col): (usize, usize)| {
            let line = line.min(rope.len_lines() - 1);
            (line, col.min(Editor::line_content_len(rope.line(line))))
        };
        self.selection_anchor = self.selection_anchor.map(clamp);
        for pos in &mut self.extra_cursors {
            *pos = clamp(*pos);
        }
        for pos in self.extra_cursor_anchors.iter_mut().flatten() {
            *pos = clamp(*pos);
        }
    }
    pub(crate) fn copy_document_from(&mut self, source: &Self) {
        if Arc::ptr_eq(&self.document.shared, &source.document.shared) {
            self.refresh_document();
            self.set_prepared_git_line_status(source.prepared_git_line_status());
            return;
        }
        source.publish_document();
        self.document.shared = source.document.shared.clone();
        {
            let mut shared = self.document.shared.state.write().unwrap();
            if shared.is_none()
                || Arc::strong_count(&self.document.shared) == 2
                    && source.document.seen.get() == shared.as_ref().map_or(0, |d| d.revision)
            {
                let revision = shared.as_ref().map_or(1, |d| d.revision.wrapping_add(1));
                *shared = Some(Document::capture(source, revision));
                source.document.seen.set(revision);
            }
        }
        self.document.seen.set(0);
        self.sync_undo_history_from(source);
        self.refresh_document();
        self.set_prepared_git_line_status(source.prepared_git_line_status());
    }
    #[cfg(test)]
    pub(crate) fn shares_document(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.document.shared, &other.document.shared)
            && self.undo_stack.shares(&other.undo_stack)
            && self.redo_stack.shares(&other.redo_stack)
    }
}
