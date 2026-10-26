//! Prepare a large paste against a shared Rope snapshot. No editor caches,
//! language assets, saved file, clipboard copies, or undo stacks are cloned.
use crate::{background::CancellationToken, editor::Editor};
use ropey::Rope;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, PartialEq, Eq)]
struct View {
    cursor: (usize, usize),
    anchor: Option<(usize, usize)>,
    cursors: Vec<(usize, usize)>,
    anchors: Vec<Option<(usize, usize)>>,
    bookmarks: BTreeSet<usize>,
    folds: BTreeMap<usize, usize>,
}

impl View {
    fn capture(editor: &Editor) -> Self {
        Self {
            cursor: (editor.cursor_line, editor.cursor_col),
            anchor: editor.selection_anchor,
            cursors: editor.extra_cursors.clone(),
            anchors: editor.extra_cursor_anchors.clone(),
            bookmarks: editor.bookmarks.clone(),
            folds: editor.folded_ranges.clone(),
        }
    }
    fn matches(&self, editor: &Editor) -> bool {
        self.cursor == (editor.cursor_line, editor.cursor_col)
            && self.anchor == editor.selection_anchor
            && self.cursors == editor.extra_cursors
            && self.anchors == editor.extra_cursor_anchors
            && self.bookmarks == editor.bookmarks
            && self.folds == editor.folded_ranges
    }
    fn install(self, editor: &mut Editor) {
        (editor.cursor_line, editor.cursor_col) = self.cursor;
        editor.selection_anchor = self.anchor;
        editor.extra_cursors = self.cursors;
        editor.extra_cursor_anchors = self.anchors;
        editor.bookmarks = self.bookmarks;
        editor.folded_ranges = self.folds;
    }
}

pub(crate) struct Snapshot {
    source: Rope,
    document: u64,
    version: u64,
    view: View,
    pub navigation: u64,
    line_copy: bool,
}

pub(crate) struct Prepared {
    snapshot: Snapshot,
    rope: Rope,
    view: View,
    text: String,
    first: usize,
    last: usize,
}

impl Snapshot {
    pub(crate) fn capture(editor: &Editor, text: &str) -> Option<Self> {
        if editor.read_only || editor.bg_loading || editor.markdown_preview_scroll.is_some() {
            return None;
        }
        Some(Self {
            source: editor.rope.clone(),
            document: editor.save_identity,
            version: editor.content_version,
            navigation: editor.navigation_id,
            view: View::capture(editor),
            line_copy: crate::clipboard::line_copy_status(text)
                .unwrap_or_else(|| editor.clipboard_is_line && text == editor.clipboard),
        })
    }

    pub(crate) fn prepare(
        self,
        text: String,
        smart: bool,
        cancel: &CancellationToken,
    ) -> Result<Prepared, String> {
        if cancel.is_cancelled() {
            return Err("Paste cancelled; the document was not changed".into());
        }
        let mut editor = Editor::new();
        editor.rope = self.source.clone();
        self.view.clone().install(&mut editor);
        let first = editor.earliest_edit_line();
        // The scratch view never wraps or renders the million-line payload.
        editor.paste_snapshot_text(&text, smart, Some(self.line_copy));
        if cancel.is_cancelled() {
            return Err("Paste cancelled; the document was not changed".into());
        }
        let last = editor.latest_edit_line();
        let view = View::capture(&editor);
        Ok(Prepared {
            snapshot: self,
            rope: editor.rope,
            view,
            text,
            first,
            last,
        })
    }
}

impl Prepared {
    pub(crate) fn apply(self, editor: &mut Editor, threshold: u64) -> Result<usize, String> {
        crate::editor_document::edit!(editor, {
            if editor.read_only
                || editor.bg_loading
                || editor.save_content_error().is_some()
                || editor.markdown_preview_scroll.is_some()
                || self.snapshot.navigation != editor.navigation_id
                || self.snapshot.document != editor.save_identity
                || self.snapshot.version != editor.content_version
                || !self.snapshot.source.is_instance(&editor.rope)
                || !self.snapshot.view.matches(editor)
            {
                return Err("Paste cancelled because the document or selection changed; no pasted text was inserted".into());
            }
            editor.break_undo_group();
            editor.save_undo_for_range(self.first, self.last);
            editor.rope = self.rope;
            self.view.install(editor);
            editor.modified = true;
            editor.desired_col = None;
            editor.cached_visible_line_count = None;
            editor.cached_wrapped_total = None;
            editor.cached_row_offsets = None;
            editor.fold_buttons.clear();
            editor.snippet_cancel();
            editor.mark_syntax_dirty_range(self.first, self.last);
            // Disable whole-document wrapping before ensuring the new caret
            // is visible, rather than discovering large-file mode next frame.
            editor.update_large_file_mode(threshold);
            editor.ensure_visible();
            let bytes = self.text.len();
            editor.clipboard = self.text;
            editor.clipboard_is_line = self.snapshot.line_copy;
            Ok(bytes)
        })
    }
}
