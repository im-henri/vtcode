use ropey::Rope;

use crate::editor::{CaretState, CharClass, Editor, UndoSnapshot, UndoState};

fn next_snapshot_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl Editor {
    pub(crate) fn set_encoding(&mut self, encoding: &'static str) {
        crate::editor_document::edit!(self, {
            if self.encoding == encoding || self.save_content_error().is_some() {
                return;
            }
            self.encoding = encoding;
            self.modified = true;
            // Save/autosave versions identify the bytes to write, not just text.
            self.content_version = self.content_version.wrapping_add(1);
            self.break_undo_group();
            self.refresh_modified_from_saved();
        })
    }

    /// Check a claimed clean state before capturing another edit. Dirty states
    /// need no comparison on the typing path; history navigation and hunk
    /// reverts reconcile both directions after restoring their content.
    fn verify_clean_snapshot(&mut self) {
        if self.modified {
            return;
        }
        self.refresh_modified_from_saved();
    }

    /// History flags are hints, not the saved state: distinct snapshots can
    /// contain exactly the same text. Reuse the saved Rope after equality is
    /// verified so subsequent checks use constant-time identity comparisons.
    pub(crate) fn refresh_modified_from_saved(&mut self) {
        let Some(saved) = &self.saved_rope else {
            return;
        };
        if crate::rope_changes::equal(&self.rope, saved) {
            self.rope = saved.clone();
            self.modified = self.encoding != self.saved_encoding;
            self.reset_unsaved_diff_cache();
        } else {
            self.modified = true;
            self.refresh_git_index_match();
        }
    }

    /// Advance the disk checkpoint without comparing every history entry's
    /// text. Rebase their hints by Rope identity, then verify the live buffer;
    /// other entries are checked exactly when Undo/Redo restores them.
    pub(crate) fn record_saved_snapshot(&mut self, saved: Rope) {
        crate::editor_document::edit!(self, {
            self.record_saved_snapshot_with_encoding(saved, self.encoding);
        })
    }

    pub(crate) fn record_saved_snapshot_with_encoding(
        &mut self,
        saved: Rope,
        encoding: &'static str,
    ) {
        crate::editor_document::edit!(self, {
            let same_baseline = self
                .saved_rope
                .as_ref()
                .is_some_and(|old| old.is_instance(&saved));
            for state in self
                .undo_stack
                .write()
                .iter_mut()
                .chain(self.redo_stack.write().iter_mut())
            {
                state.modified =
                    !(state.rope.is_instance(&saved) || (same_baseline && !state.modified));
            }
            self.saved_rope = Some(saved);
            self.saved_encoding = encoding;
            self.saved_save_options = None;
            self.refresh_modified_from_saved();
            if self
                .saved_rope
                .as_ref()
                .is_some_and(|saved| self.rope.is_instance(saved))
            {
                self.insertion_anchors = self
                    .insertion_anchors
                    .take()
                    .and_then(|anchors| anchors.rebase(&self.rope));
                self.saved_insertion_anchors = self.insertion_anchors.clone();
            } else if !same_baseline {
                self.saved_insertion_anchors = None;
            }
        })
    }

    /// Push current cursor position onto the cursor undo stack.
    /// Called before significant cursor movements (jumps, go-to-line, etc.)
    pub(crate) fn push_cursor_position(&mut self) {
        let pos = (self.cursor_line, self.cursor_col);
        // Don't push if same as top of stack
        if self.cursor_position_stack.back() == Some(&pos) {
            return;
        }
        self.cursor_position_stack.push_back(pos);
        // Limit stack size
        if self.cursor_position_stack.len() > 100 {
            self.cursor_position_stack.pop_front();
        }
    }

    /// Pop and restore previous cursor position (Ctrl+U cursor undo).
    pub(crate) fn cursor_undo(&mut self) {
        // If extra cursors exist, remove the last one (undo last cursor addition)
        if !self.extra_cursors.is_empty() {
            self.extra_cursors.pop();
            self.extra_cursor_anchors.pop();
            return;
        }
        // Otherwise, undo last cursor position change
        while let Some((line, col)) = self.cursor_position_stack.pop_back() {
            if line != self.cursor_line || col != self.cursor_col {
                self.cursor_line = line;
                self.cursor_col = col;
                self.clamp_cursor();
                self.selection_anchor = None;
                self.ensure_visible();
                return;
            }
        }
    }

    pub(crate) fn save_undo(&mut self) {
        self.save_undo_for_range(self.earliest_edit_line(), self.latest_edit_line());
    }

    /// Operations such as save cleanup know their affected range and do not
    /// edit at the caret. Keep their history and cache invalidation local too.
    pub(crate) fn save_undo_for_range(&mut self, dirty_from: usize, dirty_through: usize) {
        self.verify_clean_snapshot();
        // Explicit edits (paste, cut, Enter, transforms) end the previous typing
        // group. Only maybe_save_undo[_char] opts into continued grouping.
        self.break_undo_group();
        // Stop attributing marks to the previous snapshot before starting a
        // new edit group.
        self.active_undo_change_id = None;
        self.mark_syntax_dirty_range(dirty_from, dirty_through);
        self.content_version += 1;
        self.cached_wrapped_total = None;
        let change_id = self.next_undo_change_id;
        self.next_undo_change_id = self.next_undo_change_id.wrapping_add(1);
        self.undo_stack.push_back(UndoState {
            snapshot_id: next_snapshot_id(),
            rope: self.rope.clone(),
            insertion_anchors: self.insertion_anchors.clone(),
            line_ending: self.cached_line_ending,
            cursor_line: self.cursor_line,
            cursor_col: self.cursor_col,
            extra_cursors: self.extra_cursors.clone(),
            extra_cursor_anchors: self.extra_cursor_anchors.clone(),
            selection_anchor: self.selection_anchor,
            modified: self.modified,
            dirty_from,
            dirty_through,
            change_id,
            other_caret: None,
        });
        self.active_undo_change_id = Some(change_id);
        self.redo_stack.clear();
        // Rope snapshots share their backing tree, so keep consistent undo semantics.
        let max_undo = 500;
        if self.undo_stack.len() > max_undo {
            self.undo_stack.pop_front();
        }
    }

    pub(crate) fn save_undo_public(&mut self) {
        self.save_undo();
    }

    pub(crate) fn maybe_save_undo(&mut self) {
        // Always save an undo snapshot on first change of a new group
        if !self.undo_group_active {
            self.save_undo();
            self.undo_group_active = true;
            self.last_undo_time = std::time::Instant::now();
        } else {
            self.invalidate_grouped_edit();
        }
    }

    /// Snapshot creation already performs this work. Continuing a typing group
    /// needs the same invalidation once, without cloning another undo state.
    fn invalidate_grouped_edit(&mut self) {
        self.mark_syntax_dirty_range(self.earliest_edit_line(), self.latest_edit_line());
        self.content_version += 1;
        self.cached_wrapped_total = None;
    }

    /// Save undo snapshot based on character class grouping (word boundary detection).
    /// This gives VS Code-like undo behavior: typing a word undoes as one step.
    pub(crate) fn maybe_save_undo_char(&mut self, c: char) {
        let class = CharClass::of(c);
        let elapsed = self.last_undo_time.elapsed().as_millis();

        // Start a new undo group when there is a pause, character class change,
        // or newline. This is independent of document size.
        let need_snapshot = !self.undo_group_active
            || elapsed > 500
            || class != self.last_undo_char_class
            || class == CharClass::Newline;

        if need_snapshot {
            self.save_undo();
            self.undo_group_active = true;
        } else {
            self.invalidate_grouped_edit();
        }

        self.last_undo_time = std::time::Instant::now();
        self.last_undo_char_class = class;
    }

    /// Call this to break the current undo group (e.g. on cursor movement, selection change)
    pub(crate) fn break_undo_group(&mut self) {
        self.active_undo_change_id = None;
        if self.undo_group_active {
            self.undo_group_active = false;
            self.last_undo_char_class = CharClass::None;
        }
    }

    pub(crate) fn caret_state(&self) -> CaretState {
        CaretState {
            line: self.cursor_line,
            col: self.cursor_col,
            extra_cursors: self.extra_cursors.clone(),
            extra_cursor_anchors: self.extra_cursor_anchors.clone(),
            selection_anchor: self.selection_anchor,
        }
    }

    fn restore_caret(&mut self, caret: CaretState) {
        self.cursor_line = caret.line;
        self.cursor_col = caret.col;
        self.extra_cursors = caret.extra_cursors;
        self.extra_cursor_anchors = caret.extra_cursor_anchors;
        self.selection_anchor = caret.selection_anchor;
    }

    /// VS Code replaces an edit group's `afterCursorState` after every edit
    /// appended to it. Called when each top-level edit transaction ends, so
    /// Redo returns to where the caret was right after the change, not to
    /// wherever it was when Undo was pressed.
    pub(crate) fn record_undo_after_caret(&mut self) {
        let Some(active_id) = self.active_undo_change_id else {
            return;
        };
        let caret = self.caret_state();
        if let Some(mut state) = self
            .undo_stack
            .back_mut()
            .filter(|state| state.change_id == active_id)
        {
            if state.other_caret.as_ref() != Some(&caret) {
                state.other_caret = Some(caret);
            }
        }
    }

    /// History without a recorded caret (restored sessions, edits made
    /// outside a transaction) still lands on the change itself: the start or
    /// end of the text that differs from `other`.
    fn caret_at_change(rope: &Rope, other: &Rope, at_end: bool) -> CaretState {
        let (_, changed) = crate::rope_changes::changed_chars(other, rope);
        let idx = if at_end { changed.end } else { changed.start }.min(rope.len_chars());
        let line = rope.char_to_line(idx);
        CaretState {
            line,
            col: idx - rope.line_to_char(line),
            ..CaretState::default()
        }
    }

    pub(crate) fn undo(&mut self) {
        crate::editor_document::edit!(self, {
            self.desired_col = None;
            // A still-open typing group has the post-edit caret right now.
            self.record_undo_after_caret();
            self.break_undo_group();
            if let Some(state) = self.undo_stack.pop_back() {
                self.snippet_cancel();
                self.animation_undo_revision = self.animation_undo_revision.wrapping_add(1);
                let old_line_count = self.rope.len_lines();
                // VS Code: Undo restores the carets from before the edit and
                // Redo the carets from right after it.
                let before = CaretState {
                    line: state.cursor_line,
                    col: state.cursor_col,
                    extra_cursors: state.extra_cursors,
                    extra_cursor_anchors: state.extra_cursor_anchors,
                    selection_anchor: state.selection_anchor,
                };
                let after = state
                    .other_caret
                    .unwrap_or_else(|| Self::caret_at_change(&self.rope, &state.rope, true));
                self.redo_stack.push_back(UndoState {
                    snapshot_id: next_snapshot_id(),
                    rope: self.rope.clone(),
                    insertion_anchors: self.insertion_anchors.clone(),
                    line_ending: self.cached_line_ending,
                    cursor_line: after.line,
                    cursor_col: after.col,
                    extra_cursors: after.extra_cursors,
                    extra_cursor_anchors: after.extra_cursor_anchors,
                    selection_anchor: after.selection_anchor,
                    modified: self.modified,
                    dirty_from: state.dirty_from,
                    dirty_through: state.dirty_through,
                    change_id: state.change_id,
                    other_caret: Some(before.clone()),
                });
                // Cap redo stack to same limit as undo
                let max_redo = 500;
                if self.redo_stack.len() > max_redo {
                    self.redo_stack.pop_front();
                }
                self.rope = state.rope;
                self.insertion_anchors = state.insertion_anchors;
                self.cached_line_ending = state.line_ending;
                self.restore_caret(before);
                let line_delta = old_line_count.abs_diff(self.rope.len_lines());
                self.mark_syntax_dirty_range(
                    state.dirty_from,
                    state
                        .dirty_through
                        .max(state.dirty_from.saturating_add(line_delta)),
                );
                self.content_version += 1;
                self.cached_wrapped_total = None;
                self.clear_stale_folds();
                self.modified = state.modified;
                self.refresh_modified_from_saved();
                self.ensure_visible();
            }
        })
    }

    pub(crate) fn redo(&mut self) {
        crate::editor_document::edit!(self, {
            self.desired_col = None;
            self.break_undo_group();
            if let Some(state) = self.redo_stack.pop_back() {
                self.snippet_cancel();
                self.animation_undo_revision = self.animation_undo_revision.wrapping_add(1);
                let old_line_count = self.rope.len_lines();
                let after = CaretState {
                    line: state.cursor_line,
                    col: state.cursor_col,
                    extra_cursors: state.extra_cursors,
                    extra_cursor_anchors: state.extra_cursor_anchors,
                    selection_anchor: state.selection_anchor,
                };
                let before = state
                    .other_caret
                    .unwrap_or_else(|| Self::caret_at_change(&self.rope, &state.rope, false));
                self.undo_stack.push_back(UndoState {
                    snapshot_id: next_snapshot_id(),
                    rope: self.rope.clone(),
                    insertion_anchors: self.insertion_anchors.clone(),
                    line_ending: self.cached_line_ending,
                    cursor_line: before.line,
                    cursor_col: before.col,
                    extra_cursors: before.extra_cursors,
                    extra_cursor_anchors: before.extra_cursor_anchors,
                    selection_anchor: before.selection_anchor,
                    modified: self.modified,
                    dirty_from: state.dirty_from,
                    dirty_through: state.dirty_through,
                    change_id: state.change_id,
                    other_caret: Some(after.clone()),
                });
                // Cap undo stack (same limit as in undo path)
                let max_undo = 500;
                if self.undo_stack.len() > max_undo {
                    self.undo_stack.pop_front();
                }
                self.rope = state.rope;
                self.insertion_anchors = state.insertion_anchors;
                self.cached_line_ending = state.line_ending;
                self.restore_caret(after);
                let line_delta = old_line_count.abs_diff(self.rope.len_lines());
                self.mark_syntax_dirty_range(
                    state.dirty_from,
                    state
                        .dirty_through
                        .max(state.dirty_from.saturating_add(line_delta)),
                );
                self.content_version += 1;
                self.cached_wrapped_total = None;
                self.clear_stale_folds();
                self.modified = state.modified;
                self.refresh_modified_from_saved();
                self.ensure_visible();
            }
        })
    }

    /// Keep only the history that recovery persists, sharing immutable text
    /// with the editor instead of expanding every snapshot into a String.
    pub(crate) fn export_undo_stack(&self) -> Vec<UndoSnapshot> {
        self.undo_stack
            .read()
            .iter()
            .rev()
            .take(crate::session::MAX_UNDO_ENTRIES)
            .rev()
            .map(|s| {
                (
                    s.rope.clone(),
                    s.cursor_line,
                    s.cursor_col,
                    s.extra_cursors.clone(),
                    s.extra_cursor_anchors.clone(),
                    s.selection_anchor,
                    s.modified,
                    s.other_caret.clone(),
                )
            })
            .collect()
    }

    pub(crate) fn can_undo(&self) -> bool {
        !self.undo_stack.is_empty()
    }

    pub(crate) fn can_redo(&self) -> bool {
        !self.redo_stack.is_empty()
    }

    pub(crate) fn undo_history_key(&self) -> (u64, usize, Option<u64>, usize, Option<u64>) {
        (
            self.next_undo_change_id,
            self.undo_stack.len(),
            self.undo_stack.back().map(|s| s.snapshot_id),
            self.redo_stack.len(),
            self.redo_stack.back().map(|s| s.snapshot_id),
        )
    }

    /// Share the actual history queues; no per-view history copies exist.
    pub(crate) fn sync_undo_history_from(&mut self, source: &Self) {
        self.break_undo_group();
        self.undo_stack = source.undo_stack.clone();
        self.redo_stack = source.redo_stack.clone();
        self.next_undo_change_id = source.next_undo_change_id;
    }

    /// Export redo stack for session recovery.
    pub(crate) fn export_redo_stack(&self) -> Vec<UndoSnapshot> {
        self.redo_stack
            .read()
            .iter()
            .rev()
            .take(crate::session::MAX_UNDO_ENTRIES)
            .rev()
            .map(|s| {
                (
                    s.rope.clone(),
                    s.cursor_line,
                    s.cursor_col,
                    s.extra_cursors.clone(),
                    s.extra_cursor_anchors.clone(),
                    s.selection_anchor,
                    s.modified,
                    s.other_caret.clone(),
                )
            })
            .collect()
    }

    /// Import undo/redo stacks from saved entries.
    pub(crate) fn import_undo_redo(
        &mut self,
        undo_entries: Vec<UndoSnapshot>,
        redo_entries: Vec<UndoSnapshot>,
    ) {
        crate::editor_document::edit!(self, {
            self.break_undo_group();
            // Helper to clamp a (line, col) pair to valid bounds for a rope
            let clamp_pos = |rope: &Rope, line: usize, col: usize| -> (usize, usize) {
                let max_line = rope.len_lines().saturating_sub(1);
                let clamped_line = line.min(max_line);
                let line_slice = rope.line(clamped_line);
                let line_len = line_slice.len_chars().saturating_sub(
                    if line_slice.len_chars() > 1
                        && line_slice.char(line_slice.len_chars() - 2) == '\r'
                        && line_slice.char(line_slice.len_chars() - 1) == '\n'
                    {
                        2
                    } else if line_slice.len_chars() > 0
                        && matches!(line_slice.char(line_slice.len_chars() - 1), '\n' | '\r')
                    {
                        1
                    } else {
                        0
                    },
                );
                (clamped_line, col.min(line_len))
            };

            self.undo_stack.clear();
            for (
                content,
                cl,
                cc,
                extra_cursors,
                extra_cursor_anchors,
                selection_anchor,
                modified,
                other_caret,
            ) in undo_entries
            {
                let rope = content;
                let dirty_through = rope.len_lines().saturating_sub(1);
                let (cl, cc) = clamp_pos(&rope, cl, cc);
                let extra_cursors: Vec<(usize, usize)> = extra_cursors
                    .into_iter()
                    .map(|(l, c)| clamp_pos(&rope, l, c))
                    .collect();
                let extra_cursor_anchors: Vec<Option<(usize, usize)>> = extra_cursor_anchors
                    .into_iter()
                    .map(|opt| opt.map(|(l, c)| clamp_pos(&rope, l, c)))
                    .collect();
                let selection_anchor = selection_anchor.map(|(l, c)| clamp_pos(&rope, l, c));
                let other_caret = other_caret.map(|caret| {
                    let (line, col) = clamp_pos(&rope, caret.line, caret.col);
                    CaretState { line, col, ..caret }
                });
                let change_id = self.next_undo_change_id;
                self.next_undo_change_id = self.next_undo_change_id.wrapping_add(1);
                self.undo_stack.push_back(UndoState {
                    snapshot_id: next_snapshot_id(),
                    line_ending: crate::editor::rope_line_ending(&rope),
                    insertion_anchors: None,
                    rope,
                    cursor_line: cl,
                    cursor_col: cc,
                    extra_cursors,
                    extra_cursor_anchors,
                    selection_anchor,
                    modified,
                    dirty_from: 0,
                    dirty_through,
                    change_id,
                    other_caret,
                });
            }
            self.redo_stack.clear();
            for (
                content,
                cl,
                cc,
                extra_cursors,
                extra_cursor_anchors,
                selection_anchor,
                modified,
                other_caret,
            ) in redo_entries
            {
                let rope = content;
                let dirty_through = rope.len_lines().saturating_sub(1);
                let (cl, cc) = clamp_pos(&rope, cl, cc);
                let extra_cursors: Vec<(usize, usize)> = extra_cursors
                    .into_iter()
                    .map(|(l, c)| clamp_pos(&rope, l, c))
                    .collect();
                let extra_cursor_anchors: Vec<Option<(usize, usize)>> = extra_cursor_anchors
                    .into_iter()
                    .map(|opt| opt.map(|(l, c)| clamp_pos(&rope, l, c)))
                    .collect();
                let selection_anchor = selection_anchor.map(|(l, c)| clamp_pos(&rope, l, c));
                let other_caret = other_caret.map(|caret| {
                    let (line, col) = clamp_pos(&rope, caret.line, caret.col);
                    CaretState { line, col, ..caret }
                });
                let change_id = self.next_undo_change_id;
                self.next_undo_change_id = self.next_undo_change_id.wrapping_add(1);
                self.redo_stack.push_back(UndoState {
                    snapshot_id: next_snapshot_id(),
                    line_ending: crate::editor::rope_line_ending(&rope),
                    insertion_anchors: None,
                    rope,
                    cursor_line: cl,
                    cursor_col: cc,
                    extra_cursors,
                    extra_cursor_anchors,
                    selection_anchor,
                    modified,
                    dirty_from: 0,
                    dirty_through,
                    change_id,
                    other_caret,
                });
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    #[test]
    fn recovery_export_limits_history_before_copying_and_keeps_snapshot_identity() {
        let mut editor = Editor::test_with_text(&"line\n".repeat(2_000));
        for _ in 0..120 {
            editor.save_undo();
            editor.rope.insert(0, "é");
        }
        let undo = editor.export_undo_stack();
        assert_eq!(undo.len(), crate::session::MAX_UNDO_ENTRIES);
        for (saved, original) in undo.iter().zip(editor.undo_stack.read().iter().skip(70)) {
            assert!(saved.0.is_instance(&original.rope));
        }
        let snapshot = undo.last().unwrap().0.to_string();
        editor.rope.insert(0, "later edit");
        assert_eq!(undo.last().unwrap().0, snapshot);
        for _ in 0..80 {
            editor.undo();
        }
        let redo = editor.export_redo_stack();
        assert_eq!(redo.len(), crate::session::MAX_UNDO_ENTRIES);
        for (saved, original) in redo.iter().zip(editor.redo_stack.read().iter().skip(30)) {
            assert!(saved.0.is_instance(&original.rope));
        }
        let shared = redo.last().unwrap().0.clone();
        let mut restored = Editor::test_with_text("current");
        restored.import_undo_redo(undo, redo);
        assert!(
            restored
                .redo_stack
                .back()
                .unwrap()
                .rope
                .is_instance(&shared)
        );
        restored.redo();
        assert!(restored.rope.is_instance(&shared));
    }

    #[test]
    #[ignore = "manual release benchmark"]
    fn benchmark_recovery_snapshot_export() {
        let mut editor = Editor::test_with_text(&"source line\n".repeat(48_000));
        for _ in 0..500 {
            editor.save_undo();
            editor.rope.insert(0, "x");
        }
        for optimized in [false, true] {
            let mut samples = Vec::new();
            for _ in 0..7 {
                let started = std::time::Instant::now();
                if optimized {
                    std::hint::black_box(editor.export_undo_stack());
                } else {
                    // Previous export expanded all 500 snapshots even though
                    // the persistence layer retained only the final 50.
                    std::hint::black_box(
                        editor
                            .undo_stack
                            .read()
                            .iter()
                            .map(|state| {
                                (
                                    state.rope.to_string(),
                                    state.cursor_line,
                                    state.cursor_col,
                                    state.extra_cursors.clone(),
                                    state.extra_cursor_anchors.clone(),
                                    state.selection_anchor,
                                    state.modified,
                                )
                            })
                            .collect::<Vec<_>>(),
                    );
                }
                samples.push(started.elapsed());
            }
            samples.sort();
            eprintln!(
                "recovery_export optimized={optimized} document_bytes={} snapshots=500 median={:?}",
                editor.rope.len_bytes(),
                samples[3]
            );
        }
    }

    #[test]
    fn test_save_undo_creates_snapshot() {
        let mut ed = Editor::test_with_text("hello");
        assert!(!ed.can_undo());
        ed.save_undo();
        assert!(ed.can_undo());
    }

    #[test]
    fn test_undo_restores_content() {
        let mut ed = Editor::test_with_text("original");
        ed.save_undo();
        ed.rope = Rope::from_str("modified");
        ed.undo();
        assert_eq!(ed.rope.to_string(), "original");
    }

    #[test]
    fn test_redo_after_undo() {
        let mut ed = Editor::test_with_text("original");
        ed.save_undo();
        ed.rope = Rope::from_str("modified");
        ed.undo();
        assert_eq!(ed.rope.to_string(), "original");
        ed.redo();
        assert_eq!(ed.rope.to_string(), "modified");
    }

    #[test]
    fn live_undo_redo_restores_saved_state_metadata() {
        let mut ed = Editor::test_with_text(&"unchanged line\n".repeat(50_000));
        ed.saved_rope = Some(ed.rope.clone());
        ed.cursor_line = 49_999;
        ed.cursor_col = 0;

        ed.insert_char('x');
        assert!(ed.modified);
        assert!(!ed.undo_stack.back().expect("undo snapshot").modified);

        ed.undo();
        assert!(!ed.modified);
        ed.redo();
        assert!(ed.modified);
    }

    #[test]
    fn imported_undo_snapshot_restores_saved_state_metadata() {
        let mut ed = Editor::test_with_text("saved\n");
        ed.saved_rope = Some(ed.rope.clone());
        ed.rope = Rope::from_str("changed\n");
        ed.modified = true;
        ed.import_undo_redo(
            vec![("saved\n".into(), 0, 0, vec![], vec![], None, false, None)],
            vec![],
        );

        ed.undo();
        assert!(!ed.modified);
    }

    #[test]
    fn undo_and_redo_keep_syntax_invalidation_local_to_the_edit() {
        let mut ed = Editor::test_with_text("zero\none\ntwo\nthree\nfour\n");
        ed.cursor_line = 2;
        ed.cursor_col = 3;
        ed.insert_newline();
        // This is the range supplied by the editor input post-edit hook.
        ed.mark_syntax_dirty_range(2, 3);

        // Simulate the worker converging before the user invokes undo.
        ed.syntax_dirty_from = None;
        ed.syntax_dirty_through = None;
        ed.undo();
        assert_eq!(ed.syntax_dirty_from, Some(2));
        assert_eq!(ed.syntax_dirty_through, Some(3));

        ed.syntax_dirty_from = None;
        ed.syntax_dirty_through = None;
        ed.redo();
        assert_eq!(ed.syntax_dirty_from, Some(2));
        assert_eq!(ed.syntax_dirty_through, Some(3));
    }

    #[test]
    fn test_undo_restores_cursor_position() {
        let mut ed = Editor::test_with_text("hello world\nsecond line\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.save_undo();
        ed.cursor_line = 1;
        ed.cursor_col = 5;
        ed.undo();
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 3);
    }

    fn type_at(ed: &mut Editor, line: usize, col: usize, text: &str) {
        // Moving the caret ends the previous undo group, as a click would.
        ed.break_undo_group();
        ed.cursor_line = line;
        ed.cursor_col = col;
        for c in text.chars() {
            ed.insert_char(c);
        }
    }

    fn caret(ed: &Editor) -> (usize, usize) {
        (ed.cursor_line, ed.cursor_col)
    }

    #[test]
    fn redo_returns_to_where_the_edit_ended_not_where_undo_was_pressed() {
        let mut ed = Editor::test_with_text("first line\nsecond line\nthird line\n");
        type_at(&mut ed, 0, 5, "AAA");
        type_at(&mut ed, 2, 0, "BBB");
        // Wander off before undoing, as people do.
        ed.break_undo_group();
        ed.cursor_line = 1;
        ed.cursor_col = 2;

        ed.undo();
        assert_eq!(caret(&ed), (2, 0), "undo goes to before BBB");
        ed.undo();
        assert_eq!(caret(&ed), (0, 5), "undo goes to before AAA");
        ed.redo();
        assert_eq!(
            ed.rope.to_string(),
            "firstAAA line\nsecond line\nthird line\n"
        );
        assert_eq!(caret(&ed), (0, 8), "redo lands after AAA, not on line 2");
        ed.redo();
        assert_eq!(caret(&ed), (2, 3), "redo lands after BBB, not on line 1");

        // Round trips keep both ends of every change.
        ed.undo();
        assert_eq!(caret(&ed), (2, 0));
        ed.undo();
        assert_eq!(caret(&ed), (0, 5));
        ed.redo();
        ed.redo();
        assert_eq!(caret(&ed), (2, 3));
    }

    #[test]
    fn redo_of_a_typing_group_ends_after_its_last_keystroke() {
        let mut ed = Editor::test_with_text("x\n");
        type_at(&mut ed, 0, 1, "abc");
        // Moving away after typing must not change where redo returns.
        ed.break_undo_group();
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.undo();
        assert_eq!(ed.rope.to_string(), "x\n");
        assert_eq!(caret(&ed), (0, 1));
        ed.redo();
        assert_eq!(ed.rope.to_string(), "xabc\n");
        assert_eq!(caret(&ed), (0, 4));
    }

    #[test]
    fn undo_and_redo_restore_selection_and_extra_cursors() {
        let mut ed = Editor::test_with_text("one two\none two\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.extra_cursors = vec![(1, 3)];
        ed.extra_cursor_anchors = vec![None];
        ed.multi_cursor_insert_char('!');
        let after = ed.caret_state();
        assert_eq!(ed.rope.to_string(), "one! two\none! two\n");
        ed.break_undo_group();
        ed.extra_cursors.clear();
        ed.extra_cursor_anchors.clear();
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.undo();
        assert_eq!(caret(&ed), (0, 3));
        assert_eq!(ed.extra_cursors, vec![(1, 3)]);
        ed.redo();
        assert_eq!(ed.caret_state(), after);

        // Typing over a selection: undo brings the replaced selection back.
        let mut ed = Editor::test_with_text("hello world\n");
        ed.selection_anchor = Some((0, 6));
        ed.cursor_line = 0;
        ed.cursor_col = 11;
        ed.insert_char('X');
        assert_eq!(ed.rope.to_string(), "hello X\n");
        ed.undo();
        assert_eq!(ed.selection_anchor, Some((0, 6)));
        assert_eq!(caret(&ed), (0, 11));
        ed.redo();
        assert_eq!(ed.selection_anchor, None);
        assert_eq!(caret(&ed), (0, 7));
    }

    #[test]
    fn imported_history_redo_lands_on_the_change() {
        let mut ed = Editor::test_with_text("alpha\nbeta\ngamma\n");
        ed.import_undo_redo(
            vec![(
                "alpha\nbeta\n".into(),
                0,
                0,
                vec![],
                vec![],
                None,
                true,
                None,
            )],
            vec![],
        );
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.undo();
        assert_eq!(ed.rope.to_string(), "alpha\nbeta\n");
        ed.redo();
        // No recorded caret: end of the re-applied text.
        assert_eq!(caret(&ed), (3, 0));
    }

    #[test]
    fn test_cursor_position_stack_limit() {
        let mut ed = Editor::test_with_text(&"x\n".repeat(200));
        for i in 0..150 {
            ed.cursor_line = i;
            ed.cursor_col = 0;
            ed.push_cursor_position();
        }
        assert!(ed.cursor_position_stack.len() <= 100);
    }

    #[test]
    fn test_cursor_undo_restores_position() {
        let mut ed = Editor::test_with_text("line1\nline2\nline3\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.push_cursor_position();
        ed.cursor_line = 2;
        ed.cursor_col = 3;
        ed.push_cursor_position();
        ed.cursor_line = 1;
        ed.cursor_col = 1;
        ed.cursor_undo();
        assert_eq!(ed.cursor_line, 2);
        assert_eq!(ed.cursor_col, 3);
    }

    #[test]
    fn test_undo_stack_limit_normal_mode() {
        let mut ed = Editor::test_with_text("a");
        for _ in 0..600 {
            ed.save_undo();
        }
        assert!(ed.undo_stack.len() <= 500);
    }

    #[test]
    fn test_can_undo_empty_stack() {
        let ed = Editor::new();
        assert!(!ed.can_undo());
        assert!(!ed.can_redo());
    }

    #[test]
    fn test_multiple_undo_redo_cycle() {
        let mut ed = Editor::test_with_text("state0");

        ed.save_undo();
        ed.rope = Rope::from_str("state1");

        ed.save_undo();
        ed.rope = Rope::from_str("state2");

        ed.save_undo();
        ed.rope = Rope::from_str("state3");

        // Undo back through all states
        ed.undo();
        assert_eq!(ed.rope.to_string(), "state2");
        ed.undo();
        assert_eq!(ed.rope.to_string(), "state1");
        ed.undo();
        assert_eq!(ed.rope.to_string(), "state0");

        // Redo forward
        ed.redo();
        assert_eq!(ed.rope.to_string(), "state1");
        ed.redo();
        assert_eq!(ed.rope.to_string(), "state2");
        ed.redo();
        assert_eq!(ed.rope.to_string(), "state3");
    }
}
