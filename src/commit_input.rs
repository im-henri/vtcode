use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::{
    app::App,
    text_input::{TextInput, TextInputHistory, TextInputId},
};

/// Cached visual rows shared by painting, keyboard navigation, and hit testing.
#[derive(Default)]
pub(crate) struct CommitInputState {
    pub text: String,
    pub width: usize,
    pub rows: Vec<Range<usize>>,
    pub scroll: usize,
    pub height: usize,
    pub scrollbar_drag: Option<(u16, usize)>,
    revealed_cursor: usize,
    desired_column: Option<usize>,
    history: TextInputHistory,
}

impl CommitInputState {
    pub(crate) fn scrollbar(&self, content: Rect) -> Option<crate::ui_widgets::VerticalScrollbar> {
        if content.is_empty() || self.rows.len() <= content.height as usize {
            return None;
        }
        // Use the right border so showing the scrollbar never changes wrapping
        // or moves the text/caret underneath the pointer.
        Some(crate::ui_widgets::VerticalScrollbar::new(
            content.right(), content.y, content.height, self.rows.len(), self.scroll,
        ))
    }

    pub fn layout(&mut self, text: &str, width: usize) {
        let width = width.max(1);
        if self.text == text && self.width == width && !self.rows.is_empty() {
            return;
        }
        self.text.clear();
        self.text.push_str(text);
        self.width = width;
        self.rows.clear();
        let mut base = 0;
        for line in text.split('\n') {
            let graphemes: Vec<_> = line.grapheme_indices(true).collect();
            let mut start = 0;
            while start < graphemes.len() {
                let mut end = start;
                let mut columns = 0;
                let mut word_break = None;
                while end < graphemes.len() {
                    let g = graphemes[end].1;
                    let size = if g == "\t" { 1 } else { g.width() };
                    if columns + size > width && end > start {
                        break;
                    }
                    columns += size;
                    end += 1;
                    if g.chars().all(char::is_whitespace)
                        || matches!(g, "-" | "/" | "\\" | "," | ";")
                    {
                        word_break = Some(end);
                    }
                }
                if end < graphemes.len() {
                    end = word_break.unwrap_or(end);
                }
                let end_byte = graphemes.get(end).map_or(line.len(), |(byte, _)| *byte);
                self.rows.push(base + graphemes[start].0..base + end_byte);
                start = end;
            }
            // Only EOF needs a spare caret row. An explicit newline already
            // supplies the following row and must not create an extra blank.
            if line.is_empty()
                || (base + line.len() == text.len()
                    && self
                        .rows
                        .last()
                        .is_some_and(|range| text[range.clone()].width() >= width))
            {
                self.rows.push(base + line.len()..base + line.len());
            }
            base += line.len() + 1;
        }
    }

    pub fn cursor_row(&self, byte: usize) -> usize {
        self.rows
            .partition_point(|range| range.start <= byte)
            .saturating_sub(1)
    }

    pub fn reveal(&mut self, cursor: usize) {
        self.revealed_cursor = cursor;
        let row = self.cursor_row(cursor);
        if row < self.scroll {
            self.scroll = row;
        }
        if row >= self.scroll + self.height.max(1) {
            self.scroll = row + 1 - self.height.max(1);
        }
        self.scroll = self
            .scroll
            .min(self.rows.len().saturating_sub(self.height.max(1)));
    }

    pub fn byte_at_column(&self, row: usize, column: usize) -> usize {
        let Some(range) = self.rows.get(row) else {
            return self.text.len();
        };
        let mut x = 0;
        for (offset, g) in self.text[range.clone()].grapheme_indices(true) {
            let width = if g == "\t" { 1 } else { g.width() };
            if x + width > column {
                return range.start + offset;
            }
            x += width;
        }
        range.end
    }
}

impl App {
    pub(crate) fn handle_commit_input_scrollbar_mouse(
        &mut self,
        kind: MouseEventKind,
        column: u16,
        row: u16,
    ) -> bool {
        if !matches!(kind, MouseEventKind::Down(MouseButton::Left)
            | MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown)
        {
            return false;
        }
        let bar = self.text_input_area(TextInputId::SourceControlCommit)
            .and_then(|area| self.commit_input.scrollbar(area));
        if let Some((start_row, start_offset)) = self.commit_input.scrollbar_drag {
            if matches!(kind, MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)) {
                if let Some(bar) = bar {
                    let (_, thumb_height) = bar.thumb();
                    let travel = bar.height.saturating_sub(thumb_height).max(1) as usize;
                    let max_scroll = bar.total_rows.saturating_sub(bar.height as usize);
                    let amount = usize::from(row.abs_diff(start_row)).saturating_mul(max_scroll) / travel;
                    self.commit_input.scroll = if row < start_row {
                        start_offset.saturating_sub(amount)
                    } else {
                        start_offset.saturating_add(amount)
                    }.min(max_scroll);
                } else {
                    self.commit_input.scrollbar_drag = None;
                }
                if kind == MouseEventKind::Up(MouseButton::Left) {
                    self.commit_input.scrollbar_drag = None;
                }
                return true;
            }
        }
        let Some(bar) = bar else { return false; };
        if !Rect::new(bar.x, bar.top_y, 1, bar.height).contains((column, row).into()) {
            return false;
        }
        let max_scroll = bar.total_rows.saturating_sub(bar.height as usize);
        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.clear_drag_states();
                self.drag_start_pos = None;
                self.text_input_drag = None;
                self.text_input_drag_unit = None;
                self.text_input_click_id = None;
                self.focus_text_input(TextInputId::SourceControlCommit);
                self.break_commit_input_group();
                let (thumb_top, thumb_height) = bar.thumb();
                let relative = row - bar.top_y;
                if !(thumb_top..thumb_top + thumb_height).contains(&relative) {
                    let travel = bar.height.saturating_sub(thumb_height).max(1) as usize;
                    let position = usize::from(relative.saturating_sub(thumb_height / 2)).min(travel);
                    self.commit_input.scroll = position.saturating_mul(max_scroll) / travel;
                }
                self.commit_input.scrollbar_drag = Some((row, self.commit_input.scroll));
                true
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                self.commit_input.scroll = if kind == MouseEventKind::ScrollUp {
                    self.commit_input.scroll.saturating_sub(3)
                } else {
                    self.commit_input.scroll.saturating_add(3).min(max_scroll)
                };
                true
            }
            _ => false,
        }
    }

    pub(crate) fn prepare_commit_input(&mut self, width: usize, height: usize) {
        // Git operations may replace/clear the message outside the controller.
        let changed = self.commit_input.text != self.source_control_commit_msg;
        let resized = self.commit_input.width != width.max(1) || self.commit_input.height != height;
        if changed {
            self.commit_input.history = TextInputHistory::default();
            self.commit_input.desired_column = None;
        }
        Self::normalize_inline_cursor(
            &self.source_control_commit_msg,
            &mut self.source_control_commit_cursor,
            &mut self.source_control_commit_selection_anchor,
        );
        self.commit_input
            .layout(&self.source_control_commit_msg, width);
        self.commit_input.height = height;
        if changed
            || resized
            || self.commit_input.revealed_cursor != self.source_control_commit_cursor
        {
            self.commit_input.reveal(self.source_control_commit_cursor);
        }
    }

    pub(crate) fn edit_commit_input(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        paste: Option<&str>,
    ) {
        self.prepare_commit_input(
            self.commit_input.width.max(1),
            self.commit_input.height.max(1),
        );
        let before = (
            self.source_control_commit_msg.clone(),
            self.source_control_commit_cursor,
            self.source_control_commit_selection_anchor,
        );
        let primary = modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
            && !modifiers.contains(KeyModifiers::ALT);
        let shift = modifiers.contains(KeyModifiers::SHIFT);
        if paste.is_none()
            && !primary
            && matches!(
                code,
                KeyCode::Up
                    | KeyCode::Down
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::PageUp
                    | KeyCode::PageDown
            )
        {
            let row = self.commit_input.cursor_row(before.1);
            let range = &self.commit_input.rows[row];
            let col = self.commit_input.text[range.start..before.1.min(range.end)].width();
            let goal = *self.commit_input.desired_column.get_or_insert(col);
            let target = match code {
                KeyCode::Home => range.start,
                KeyCode::End => range.end,
                _ => {
                    let step = if matches!(code, KeyCode::PageUp | KeyCode::PageDown) {
                        self.commit_input.height.max(1)
                    } else {
                        1
                    };
                    let next = if matches!(code, KeyCode::Up | KeyCode::PageUp) {
                        row.saturating_sub(step)
                    } else {
                        (row + step).min(self.commit_input.rows.len() - 1)
                    };
                    self.commit_input.byte_at_column(next, goal)
                }
            };
            self.source_control_commit_selection_anchor = if shift {
                before.2.or(Some(before.1))
            } else {
                None
            };
            self.source_control_commit_cursor = target;
            self.commit_input.history.break_group();
        } else {
            self.commit_input.desired_column = None;
            let input = TextInput::new(
                &mut self.source_control_commit_msg,
                &mut self.source_control_commit_cursor,
                &mut self.source_control_commit_selection_anchor,
            )
            .multiline(true);
            let paste = if code == KeyCode::Tab && !primary && !shift && paste.is_none() {
                Some("\t")
            } else {
                paste
            };
            self.commit_input
                .history
                .edit(input, code, modifiers, paste);
        }
        self.commit_input
            .layout(&self.source_control_commit_msg, self.commit_input.width);
        self.commit_input.reveal(self.source_control_commit_cursor);
    }

    pub(crate) fn break_commit_input_group(&mut self) {
        self.commit_input.history.break_group();
        self.commit_input.desired_column = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_input_wraps_words_wide_graphemes_and_exact_width_carets() {
        let mut state = CommitInputState::default();
        state.layout("alpha beta gamma", 11);
        assert_eq!(
            state
                .rows
                .iter()
                .map(|r| &state.text[r.clone()])
                .collect::<Vec<_>>(),
            ["alpha beta ", "gamma"]
        );
        state.layout("界界e\u{301}🙂z", 4);
        assert_eq!(
            state
                .rows
                .iter()
                .map(|r| &state.text[r.clone()])
                .collect::<Vec<_>>(),
            ["界界", "e\u{301}🙂z", ""]
        );
        assert_eq!(state.byte_at_column(0, 1), 0);
        assert_eq!(state.byte_at_column(0, 2), "界".len());
        assert_eq!(state.cursor_row(state.text.len()), 2);
    }

    #[test]
    fn commit_input_vertical_navigation_preserves_column_and_extends_selection() {
        let mut app = App::test_new();
        app.source_control_commit_msg = "abcdefghij\nx\nabcdefghij".into();
        app.source_control_commit_cursor = 8;
        app.prepare_commit_input(5, 2);
        app.edit_commit_input(KeyCode::Down, KeyModifiers::empty(), None);
        assert_eq!(app.source_control_commit_cursor, 12);
        app.edit_commit_input(KeyCode::Down, KeyModifiers::SHIFT, None);
        assert_eq!(app.source_control_commit_cursor, 16);
        assert_eq!(app.source_control_commit_selection_anchor, Some(12));
        app.edit_commit_input(KeyCode::Up, KeyModifiers::empty(), None);
        app.edit_commit_input(KeyCode::Up, KeyModifiers::empty(), None);
        assert_eq!(app.source_control_commit_cursor, 8);
    }

    #[test]
    fn commit_input_typing_paste_and_replace_have_local_undo_redo() {
        let mut app = App::test_new();
        app.prepare_commit_input(12, 2);
        for c in "hello".chars() {
            app.edit_commit_input(KeyCode::Char(c), KeyModifiers::empty(), None);
        }
        app.edit_commit_input(KeyCode::Null, KeyModifiers::empty(), Some("\nworld"));
        app.edit_commit_input(KeyCode::Char('z'), KeyModifiers::CONTROL, None);
        assert_eq!(app.source_control_commit_msg, "hello");
        app.edit_commit_input(KeyCode::Char('z'), KeyModifiers::CONTROL, None);
        assert_eq!(app.source_control_commit_msg, "");
        app.edit_commit_input(KeyCode::Char('y'), KeyModifiers::CONTROL, None);
        assert_eq!(app.source_control_commit_msg, "hello");
        app.source_control_commit_selection_anchor = Some(0);
        app.edit_commit_input(KeyCode::Char('x'), KeyModifiers::empty(), None);
        assert_eq!(app.source_control_commit_msg, "x");
        app.edit_commit_input(KeyCode::Char('z'), KeyModifiers::CONTROL, None);
        assert_eq!(app.source_control_commit_msg, "hello");
        assert_eq!(app.source_control_commit_selection_anchor, Some(0));
    }

    #[test]
    fn commit_input_cursor_and_delete_keep_combining_graphemes_whole() {
        let mut app = App::test_new();
        app.source_control_commit_msg = "e\u{301}🙂".into();
        app.source_control_commit_cursor = app.source_control_commit_msg.len();
        app.prepare_commit_input(10, 2);
        app.edit_commit_input(KeyCode::Left, KeyModifiers::empty(), None);
        assert_eq!(app.source_control_commit_cursor, "e\u{301}".len());
        app.edit_commit_input(KeyCode::Backspace, KeyModifiers::empty(), None);
        assert_eq!(app.source_control_commit_msg, "🙂");
        app.edit_commit_input(KeyCode::Char('z'), KeyModifiers::CONTROL, None);
        assert_eq!(app.source_control_commit_msg, "e\u{301}🙂");
    }
}
