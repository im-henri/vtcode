// Frozen comparison algorithms from 5da4a00; test-only.
use super::*;
impl TermGrid {
    pub(super) fn reference_scroll_up(&mut self, n: usize) {
        // Only rows leaving the top of the primary screen enter history.
        // A fixed footer is allowed; a region below a header must not be saved.
        let save_history = self.alt_saved.is_none() && self.scroll_top == 0;
        let full_screen = save_history && self.scroll_bottom == self.height.saturating_sub(1);
        if !full_screen {
            if save_history {
                self.clear_selection();
            } else {
                self.invalidate_selection_rows(self.scroll_top, self.scroll_bottom);
            }
        }
        let n = n.min(self.scroll_bottom.saturating_sub(self.scroll_top) + 1);
        for _ in 0..n {
            if self.scroll_top <= self.scroll_bottom && self.scroll_top < self.cells.len() {
                let row = self.cells.remove(self.scroll_top);
                let wrapped = self.wrapped.remove(self.scroll_top);
                if save_history {
                    self.scrollback.push_back(row);
                    self.scrollback_wrapped.push_back(wrapped);
                    if let Some(ConptyResize::Repainting { scrolled_rows, .. }) =
                        &mut self.conpty_resize
                    {
                        *scrolled_rows += 1;
                    }
                    if full_screen && (self.scroll_view > 0 || self.has_selection()) {
                        self.scroll_view = (self.scroll_view + 1).min(self.scrollback.len());
                    }
                    self.trim_scrollback();
                }
                let pos = self.scroll_bottom.min(self.cells.len());
                self.cells.insert(pos, self.new_row());
                self.wrapped.insert(pos, false);
            }
        }
    }

    pub(super) fn reference_scroll_down(&mut self, n: usize) {
        self.invalidate_selection_rows(self.scroll_top, self.scroll_bottom);
        let n = n.min(self.scroll_bottom.saturating_sub(self.scroll_top) + 1);
        for _ in 0..n {
            if self.scroll_top <= self.scroll_bottom && self.scroll_bottom < self.cells.len() {
                self.cells.remove(self.scroll_bottom);
                self.wrapped.remove(self.scroll_bottom);
                self.cells.insert(self.scroll_top, self.new_row());
                self.wrapped.insert(self.scroll_top, false);
            }
        }
    }

    pub(super) fn reference_erase_in_display(&mut self, mode: u16) {
        if mode == 2 || mode == 3 {
            self.conpty_resize = None;
        }
        // ED 3 erases saved lines, not the live viewport (ED 2). Rebase a
        // live-screen selection and discard one that intersects old history.
        if mode == 3 {
            self.trim_selection(self.scrollback.len());
            self.scrollback.clear();
            self.scrollback_wrapped.clear();
            self.scroll_view = 0;
            return;
        }
        match mode {
            0 => self.invalidate_selection_rows(self.cursor_row, self.height.saturating_sub(1)),
            1 => self.invalidate_selection_rows(0, self.cursor_row),
            2 => self.invalidate_selection_rows(0, self.height.saturating_sub(1)),
            _ => {}
        }
        match mode {
            0 => {
                self.reference_erase_in_line(0);
                for row in (self.cursor_row + 1)..self.height {
                    if row < self.cells.len() {
                        self.cells[row] = self.new_row();
                        self.wrapped[row] = false;
                    }
                }
            }
            1 => {
                for row in 0..self.cursor_row {
                    if row < self.cells.len() {
                        self.cells[row] = self.new_row();
                        self.wrapped[row] = false;
                    }
                }
                self.reference_erase_in_line(1);
            }
            2 => {
                for row in 0..self.height {
                    if row < self.cells.len() {
                        self.cells[row] = self.new_row();
                        self.wrapped[row] = false;
                    }
                }
            }
            _ => {}
        }
    }

    pub(super) fn reference_erase_in_line(&mut self, mode: u16) {
        if self.cursor_row >= self.cells.len() {
            return;
        }
        if mode <= 2 {
            self.invalidate_selection_rows(self.cursor_row, self.cursor_row);
        }
        match mode {
            0 => {
                self.wrapped[self.cursor_row] = false;
                for col in self.cursor_col..self.width {
                    if col < self.cells[self.cursor_row].len() {
                        self.cells[self.cursor_row][col] = TermCell::default();
                    }
                }
            }
            1 => {
                for col in 0..=self.cursor_col.min(self.width.saturating_sub(1)) {
                    if col < self.cells[self.cursor_row].len() {
                        self.cells[self.cursor_row][col] = TermCell::default();
                    }
                }
            }
            2 => {
                self.cells[self.cursor_row] = self.new_row();
                self.wrapped[self.cursor_row] = false;
            }
            _ => {}
        }
    }
}
