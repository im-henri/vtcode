//! Lightweight view positions retained when a file is closed in this workspace.
use crate::editor::Editor;

#[derive(Clone, Copy)]
pub(crate) struct ViewState {
    cursor_line: usize,
    cursor_col: usize,
    scroll: usize,
    horizontal: usize,
    wrap_scroll: usize,
    wrap_col: usize,
}

impl ViewState {
    pub(crate) fn capture(editor: &mut Editor) -> Self {
        if editor.bg_loading {
            if let Some(saved) = editor.pending_reopen_view {
                return saved;
            }
            if let Some(saved) = &editor.pending_session_restore {
                return Self {
                    cursor_line: saved.tab.cursor_line,
                    cursor_col: saved.tab.cursor_col,
                    scroll: saved.tab.scroll_offset,
                    horizontal: saved.tab.horizontal_scroll,
                    wrap_scroll: 0,
                    wrap_col: 0,
                };
            }
        }
        let mut saved = Self {
            cursor_line: editor.cursor_line,
            cursor_col: editor.cursor_col,
            scroll: editor.scroll_offset,
            horizontal: editor.horizontal_scroll,
            wrap_scroll: editor.wrap_scroll_offset,
            wrap_col: 0,
        };
        if editor.word_wrap && !editor.bg_loading {
            let width = editor.effective_wrap_width();
            if let Some(row) = editor
                .wrapped_rows(editor.wrap_scroll_offset, 1, width)
                .first()
            {
                saved.scroll = row.line_idx;
                saved.wrap_col = row.start_col;
            }
        }
        saved
    }
}

impl Editor {
    pub(crate) fn restore_reopen_view(&mut self) {
        if self.pending_cursor_position.is_some() || self.pending_navigation_selection.is_some() {
            self.pending_reopen_view = None;
            return;
        }
        if self.bg_loading {
            return;
        }
        let Some(saved) = self.pending_reopen_view else {
            return;
        };
        self.cursor_line = saved.cursor_line.min(self.line_count().saturating_sub(1));
        self.cursor_col = saved.cursor_col.min(self.line_len(self.cursor_line));
        self.scroll_offset = saved.scroll.min(self.line_count().saturating_sub(1));
        self.horizontal_scroll = saved.horizontal;
        self.wrap_scroll_offset = saved.wrap_scroll;
        if !self.word_wrap {
            self.pending_reopen_view = None;
        }
    }

    /// Resolve the saved wrapped row against the reopened pane's actual width.
    pub(crate) fn restore_reopen_wrap_scroll(&mut self) {
        if self.bg_loading {
            return;
        }
        let Some(saved) = self.pending_reopen_view else {
            return;
        };
        if self.word_wrap {
            let width = self.effective_wrap_width();
            let line = saved.scroll.min(self.line_count().saturating_sub(1));
            let col = saved.wrap_col.min(self.line_len(line));
            let row = self.wrapped_row_for_position(line, col, width);
            self.set_wrap_scroll_offset(row);
            if !self.wrap_layout_ready(width) {
                return;
            }
        }
        self.pending_reopen_view = None;
    }
}
