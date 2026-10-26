use crate::editor::Editor;

/// Occurrence endpoints arrive in source order and often share a line. Reuse
/// its character bounds instead of seeking the rope tree for every endpoint.
fn occurrence_position(
    rope: &ropey::Rope,
    index: usize,
    cached: &mut Option<(usize, usize, usize)>,
) -> (usize, usize) {
    if let Some((line, start, end)) = *cached {
        if index >= start && (index < end || (index == end && line + 1 == rope.len_lines())) {
            return (line, index - start);
        }
    }
    let line = rope.char_to_line(index);
    let start = rope.line_to_char(line);
    *cached = Some((line, start, start + rope.line(line).len_chars()));
    (line, index - start)
}

impl Editor {
    fn vertical_goal_column(&mut self, row_start: usize) -> usize {
        let column = self.desired_col.unwrap_or_else(||
            self.cell_at_source_column(self.cursor_line, row_start, self.cursor_col));
        self.desired_col = Some(column);
        column
    }

    /// Center the viewport on the current cursor line.
    pub(crate) fn center_cursor(&mut self) {
        if self.viewport_height > 0 {
            let center = self.viewport_height / 2;
            if self.word_wrap {
                let wrap_width = self.effective_wrap_width();
                let cursor_row =
                    self.wrapped_row_for_position(self.cursor_line, self.cursor_col, wrap_width);
                self.wrap_scroll_offset = cursor_row.saturating_sub(center);
                self.sync_line_scroll_from_wrap(wrap_width);
            } else {
                let mut top = self.cursor_line.saturating_sub(center);
                if !self.folded_ranges.is_empty() {
                    // Count screen rows, jumping over hidden bodies instead of
                    // walking their source lines (which may number millions).
                    top = self.cursor_line;
                    for _ in 0..center.min(self.cursor_line) {
                        top = top.saturating_sub(1);
                        while let Some((start, _)) = self.folded_range_containing(top) {
                            top = start;
                        }
                        if top == 0 { break; }
                    }
                }
                self.scroll_offset = top;
                self.wrap_scroll_offset = top;
            }
        }
    }

    pub(crate) fn page_up(&mut self) {
        let page = self.viewport_height.saturating_sub(2).max(1);
        if self.word_wrap {
            let wrap_width = self.effective_wrap_width();
            let cursor_row =
                self.wrapped_row_for_position(self.cursor_line, self.cursor_col, wrap_width);
            let start = self.wrapped_row_at(cursor_row, wrap_width).map_or(0, |row| row.start_col);
            let desired = self.vertical_goal_column(start);
            let target_row = cursor_row.saturating_sub(page);
            if let Some(info) = self.wrapped_row_at(target_row, wrap_width) {
                self.cursor_line = info.line_idx;
                self.cursor_col = self.column_on_wrapped_row(&info, desired);
            } else {
                self.cursor_line = 0;
                self.cursor_col = self.source_column_at_cell(0, 0, self.line_len(0), desired).0;
            }
            self.wrap_scroll_offset = self.wrap_scroll_offset.saturating_sub(page);
            self.sync_line_scroll_from_wrap(wrap_width);
        } else {
            let desired = self.vertical_goal_column(0);
            self.cursor_line = self.cursor_line.saturating_sub(page);
            // Skip out of folded regions
            while let Some((fold_start, _)) = self.folded_range_containing(self.cursor_line) {
                self.cursor_line = fold_start;
            }
            self.scroll_offset = self.scroll_offset.saturating_sub(page);
            let max_col = self.current_line_len();
            self.cursor_col = self.source_column_at_cell(self.cursor_line, 0, max_col, desired).0;
        }
    }

    pub(crate) fn page_down(&mut self) {
        let page = self.viewport_height.saturating_sub(2).max(1);
        let max_line = self.line_count().saturating_sub(1);
        if self.word_wrap {
            let wrap_width = self.effective_wrap_width();
            let cursor_row =
                self.wrapped_row_for_position(self.cursor_line, self.cursor_col, wrap_width);
            let start = self.wrapped_row_at(cursor_row, wrap_width).map_or(0, |row| row.start_col);
            let desired = self.vertical_goal_column(start);
            let total_rows = self.wrapped_total_rows(wrap_width);
            let target_row = (cursor_row + page).min(total_rows.saturating_sub(1));
            if let Some(info) = self.wrapped_row_at(target_row, wrap_width) {
                self.cursor_line = info.line_idx;
                self.cursor_col = self.column_on_wrapped_row(&info, desired);
            } else {
                self.cursor_line = max_line;
                self.cursor_col = self.source_column_at_cell(max_line, 0, self.line_len(max_line), desired).0;
            }
            self.wrap_scroll_offset =
                (self.wrap_scroll_offset + page).min(total_rows.saturating_sub(1));
            self.sync_line_scroll_from_wrap(wrap_width);
        } else {
            let desired = self.vertical_goal_column(0);
            self.cursor_line = (self.cursor_line + page).min(max_line);
            // Skip out of folded regions
            while let Some((fold_start, fold_end)) = self.folded_range_containing(self.cursor_line)
            {
                if fold_end > max_line {
                    self.cursor_line = fold_start;
                    break;
                }
                self.cursor_line = fold_end;
            }
            let scroll_max = if self.scroll_past_end {
                max_line
            } else {
                max_line.saturating_sub(self.viewport_height.saturating_sub(1))
            };
            self.scroll_offset = (self.scroll_offset + page).min(scroll_max);
            let max_col = self.current_line_len();
            self.cursor_col = self.source_column_at_cell(self.cursor_line, 0, max_col, desired).0;
        }
    }

    pub(crate) fn move_up(&mut self) {
        if self.word_wrap {
            let wrap_width = self.effective_wrap_width();
            let current_row =
                self.wrapped_row_for_position(self.cursor_line, self.cursor_col, wrap_width);
            if current_row > 0 {
                if let (Some(current), Some(target)) = (
                    self.wrapped_row_at(current_row, wrap_width),
                    self.wrapped_row_at(current_row - 1, wrap_width),
                ) {
                    let desired = self.vertical_goal_column(current.start_col);
                    self.cursor_line = target.line_idx;
                    self.cursor_col = self.column_on_wrapped_row(&target, desired);
                    self.ensure_visible();
                }
            } else if self.cursor_col > 0 {
                self.cursor_col = 0;
                self.desired_col = Some(self.cursor_col);
                self.ensure_visible();
            }
            return;
        }
        if self.cursor_line > 0 {
            let desired = self.vertical_goal_column(0);
            let mut target = self.cursor_line - 1;
            // Skip folded regions when moving up
            while let Some((fold_start, _)) = self.folded_range_containing(target) {
                target = fold_start;
            }
            self.cursor_line = target;
            let max_col = self.current_line_len();
            self.cursor_col = self.source_column_at_cell(self.cursor_line, 0, max_col, desired).0;
            self.ensure_visible();
        } else if self.cursor_col > 0 {
            self.cursor_col = 0;
            self.desired_col = Some(self.cursor_col);
            self.ensure_visible();
        }
    }

    pub(crate) fn move_down(&mut self) {
        if self.word_wrap {
            let wrap_width = self.effective_wrap_width();
            let current_row =
                self.wrapped_row_for_position(self.cursor_line, self.cursor_col, wrap_width);
            let total_rows = self.wrapped_total_rows(wrap_width);
            if current_row + 1 < total_rows {
                if let (Some(current), Some(target)) = (
                    self.wrapped_row_at(current_row, wrap_width),
                    self.wrapped_row_at(current_row + 1, wrap_width),
                ) {
                    let desired = self.vertical_goal_column(current.start_col);
                    self.cursor_line = target.line_idx;
                    self.cursor_col = self.column_on_wrapped_row(&target, desired);
                    self.ensure_visible();
                }
            } else {
                let last_line = self.line_count().saturating_sub(1);
                let last_col = self.line_len(last_line);
                if self.cursor_line != last_line || self.cursor_col != last_col {
                    self.cursor_line = last_line;
                    self.cursor_col = last_col;
                    self.desired_col = None;
                    self.ensure_visible();
                }
            }
            return;
        }
        if self.cursor_line + 1 < self.line_count() {
            let desired = self.vertical_goal_column(0);
            let mut target = self.cursor_line + 1;
            // Skip folded regions when moving down
            if let Some(&fold_end) = self.folded_ranges.get(&self.cursor_line) {
                target = fold_end;
            }
            while let Some((_, fold_end)) = self.folded_range_containing(target) {
                target = fold_end;
            }
            if target < self.line_count() {
                self.cursor_line = target;
                let max_col = self.current_line_len();
                self.cursor_col = self.source_column_at_cell(self.cursor_line, 0, max_col, desired).0;
                self.ensure_visible();
            }
        } else {
            let max_col = self.current_line_len();
            if self.cursor_col != max_col {
                self.cursor_col = max_col;
                self.desired_col = None;
                self.ensure_visible();
            }
        }
    }

    /// Move cursor to previous blank line (paragraph boundary)
    pub(crate) fn move_paragraph_up(&mut self) {
        if self.cursor_line == 0 {
            return;
        }
        let mut target = 0;
        let mut crossed_blank = false;
        // One line iterator traverses rope leaves without seeking the tree for
        // every line, and each line is classified only once.
        for (offset, line) in self.rope.lines_at(self.cursor_line).reversed().enumerate() {
            let index = self.cursor_line - offset - 1;
            if index == 0 {
                break;
            }
            if line.chunks().all(|chunk| chunk.trim().is_empty()) {
                crossed_blank = true;
            } else if crossed_blank {
                target = index;
                break;
            }
        }
        self.cursor_line = target;
        self.cursor_col = 0;
        self.ensure_visible();
    }

    /// Move cursor to next blank line (paragraph boundary)
    pub(crate) fn move_paragraph_down(&mut self) {
        let max = self.line_count().saturating_sub(1);
        if self.cursor_line >= max {
            return;
        }
        let mut target = max;
        let mut crossed_blank = false;
        for (offset, line) in self.rope.lines_at(self.cursor_line + 1).enumerate() {
            let index = self.cursor_line + offset + 1;
            if index == max {
                break;
            }
            if line.chunks().all(|chunk| chunk.trim().is_empty()) {
                crossed_blank = true;
            } else if crossed_blank {
                target = index;
                break;
            }
        }
        self.cursor_line = target;
        self.cursor_col = 0;
        self.ensure_visible();
    }

    /// Select the current paragraph (text block delimited by blank lines).
    pub(crate) fn select_paragraph(&mut self) {
        self.desired_col = None;
        let mut start = self.cursor_line;
        for line in self.rope.lines_at(self.cursor_line).reversed() {
            if !line.chunks().all(|chunk| chunk.trim().is_empty()) {
                start -= 1;
            } else {
                break;
            }
        }
        let mut end = self.cursor_line;
        for line in self.rope.lines_at(self.cursor_line + 1) {
            if !line.chunks().all(|chunk| chunk.trim().is_empty()) {
                end += 1;
            } else {
                break;
            }
        }
        self.selection_anchor = Some((start, 0));
        self.cursor_line = end;
        self.cursor_col = self.line_len(end);
        self.ensure_visible();
    }

    pub(crate) fn move_left(&mut self) {
        self.desired_col = None;
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
            self.ensure_visible();
        } else if self.cursor_line > 0 {
            // Wrap to end of previous line
            self.cursor_line -= 1;
            self.cursor_col = self.current_line_len();
            self.ensure_visible();
        }
    }

    pub(crate) fn move_right(&mut self) {
        self.desired_col = None;
        let max_col = self.current_line_len();
        if self.cursor_col < max_col {
            self.cursor_col += 1;
            self.ensure_visible();
        } else if self.cursor_line + 1 < self.line_count() {
            // Wrap to start of next line
            self.cursor_line += 1;
            self.cursor_col = 0;
            self.ensure_visible();
        }
    }

    pub(crate) fn move_word_left(&mut self) {
        self.desired_col = None;
        (self.cursor_line, self.cursor_col) =
            self.word_left_position(self.cursor_line, self.cursor_col);
        self.ensure_visible();
    }

    /// Shared by ordinary and multiple-cursor navigation/selection. Each scan
    /// seeks into the rope once and then walks chunks without indexed lookups.
    pub(crate) fn word_left_position(&self, line_idx: usize, col: usize) -> (usize, usize) {
        let line_idx = line_idx.min(self.line_count().saturating_sub(1));
        let mut col = col.min(self.line_len(line_idx));
        if col == 0 && line_idx > 0 {
            return (line_idx - 1, self.line_len(line_idx - 1));
        }
        let line = self.rope.line(line_idx);
        let mut chars = line.chars_at(col).reversed().peekable();
        // Skip whitespace backwards
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
            col -= 1;
        }
        if let Some(&c) = chars.peek() {
            if c.is_alphanumeric() || c == '_' {
                // Seek once into the rope, then traverse its chunks. Repeated
                // indexed character access makes long tokens O(n log n).
                let mut right = line.get_char(col);
                let mut consumed_word = false;
                while let Some(&c) = chars.peek() {
                    if (!c.is_alphanumeric() && c != '_')
                        || (consumed_word
                            && (c.is_lowercase() || c.is_ascii_digit())
                            && right.is_some_and(char::is_uppercase))
                    {
                        break;
                    }
                    chars.next();
                    col -= 1;
                    right = Some(c);
                    // Starting on a CamelCase boundary must move past it.
                    consumed_word = true;
                }
            } else {
                // Skip punctuation group backwards.
                while chars
                    .peek()
                    .is_some_and(|c| !c.is_alphanumeric() && *c != '_' && !c.is_whitespace())
                {
                    chars.next();
                    col -= 1;
                }
            }
        }
        (line_idx, col)
    }

    pub(crate) fn move_word_right(&mut self) {
        self.desired_col = None;
        (self.cursor_line, self.cursor_col) =
            self.word_right_position(self.cursor_line, self.cursor_col);
        self.ensure_visible();
    }

    pub(crate) fn word_right_position(&self, line_idx: usize, col: usize) -> (usize, usize) {
        let line_idx = line_idx.min(self.line_count().saturating_sub(1));
        let line_len = self.line_len(line_idx);
        let mut col = col.min(line_len);
        if col >= line_len {
            return if line_idx + 1 < self.line_count() {
                (line_idx + 1, 0)
            } else {
                (line_idx, line_len)
            };
        }
        let line = self.rope.line(line_idx);
        let mut chars = line.chars_at(col).take(line_len - col).peekable();
        if let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                // From whitespace, jump to next token start.
                while chars.peek().is_some_and(|c| c.is_whitespace()) {
                    chars.next();
                    col += 1;
                }
            } else if c.is_alphanumeric() || c == '_' {
                // From a word, stop at word end or CamelCase boundary.
                let mut prev: Option<char> = None;
                for c in chars {
                    if (!c.is_alphanumeric() && c != '_')
                        || (c.is_uppercase()
                            && prev.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit()))
                    {
                        break;
                    }
                    prev = Some(c);
                    col += 1;
                }
            } else {
                // From punctuation, stop at punctuation block end.
                while chars
                    .peek()
                    .is_some_and(|c| !c.is_alphanumeric() && *c != '_' && !c.is_whitespace())
                {
                    chars.next();
                    col += 1;
                }
            }
        }
        (line_idx, col)
    }

    pub(crate) fn move_to_line_start(&mut self) {
        self.desired_col = None;
        if self.word_wrap {
            // In word-wrap mode, Home goes to start of visual (wrapped) line
            let wrap_width = self.effective_wrap_width();
            let ranges = self.wrap_ranges_for_line(self.cursor_line, wrap_width);
            // Find which wrapped segment the cursor is in
            let segment = ranges.partition_point(|&(_, end)| end < self.cursor_col);
            let visual_start = ranges.get(segment).map_or(0, |&(start, _)| start);
            if visual_start == 0 {
                // First segment: use smart home (toggle between first non-ws and col 0)
                let first_non_ws = self.line_leading_whitespace();
                if self.cursor_col == first_non_ws || first_non_ws >= self.current_line_len() {
                    self.cursor_col = 0;
                } else {
                    self.cursor_col = first_non_ws;
                }
            } else if self.cursor_col == visual_start {
                // Already at visual line start, go to actual line start (smart toggle)
                self.cursor_col = 0;
            } else {
                self.cursor_col = visual_start;
            }
        } else {
            // Smart home: first press goes to first non-whitespace char, second press to col 0
            let first_non_ws = self.line_leading_whitespace();
            if self.cursor_col == first_non_ws || first_non_ws >= self.current_line_len() {
                self.cursor_col = 0;
            } else {
                self.cursor_col = first_non_ws;
            }
        }
        self.ensure_visible();
    }

    fn line_leading_whitespace(&self) -> usize {
        self.rope
            .line(self.cursor_line)
            .chars()
            .take_while(|c| c.is_whitespace() && *c != '\n' && *c != '\r')
            .count()
    }

    pub(crate) fn move_to_line_end(&mut self) {
        self.desired_col = None;
        if self.word_wrap {
            // In word-wrap mode, End goes to end of visual (wrapped) line
            let wrap_width = self.effective_wrap_width();
            let ranges = self.wrap_ranges_for_line(self.cursor_line, wrap_width);
            let actual_end = self.current_line_len();
            // Find which wrapped segment the cursor is in
            let segment = ranges.partition_point(|&(_, end)| end < self.cursor_col);
            if let Some(&(_, end_col)) = ranges.get(segment) {
                if self.cursor_col == end_col && end_col < actual_end {
                    // Already at visual end, go to actual line end
                    self.cursor_col = actual_end;
                } else {
                    self.cursor_col = end_col;
                }
                self.ensure_visible();
                return;
            }
            self.cursor_col = actual_end;
        } else {
            self.cursor_col = self.current_line_len();
        }
        self.ensure_visible();
    }

    pub(crate) fn move_to_top(&mut self) {
        self.cursor_line = 0;
        self.cursor_col = 0;
        self.desired_col = None;
        self.ensure_visible();
    }

    pub(crate) fn move_to_bottom(&mut self) {
        self.cursor_line = self.line_count().saturating_sub(1);
        self.cursor_col = self.current_line_len();
        self.desired_col = None;
        self.ensure_visible();
    }

    pub(crate) fn scroll_up(&mut self, lines: usize) {
        if self.word_wrap {
            self.wrap_scroll_offset = self.wrap_scroll_offset.saturating_sub(lines);
            self.sync_line_scroll_from_wrap(self.effective_wrap_width());
            self.horizontal_scroll = 0;
        } else {
            self.scroll_offset = self.scroll_offset.saturating_sub(lines);
        }
    }

    pub(crate) fn scroll_down(&mut self, lines: usize) {
        if self.word_wrap {
            let wrap_width = self.effective_wrap_width();
            let max = self.wrapped_total_rows(wrap_width).saturating_sub(1);
            self.wrap_scroll_offset = (self.wrap_scroll_offset + lines).min(max);
            self.sync_line_scroll_from_wrap(wrap_width);
            self.horizontal_scroll = 0;
        } else {
            let max = if self.scroll_past_end {
                self.line_count().saturating_sub(1)
            } else {
                self.line_count()
                    .saturating_sub(self.viewport_height.max(1))
            };
            self.scroll_offset = (self.scroll_offset + lines).min(max);
        }
    }

    // Selection support

    pub(crate) fn select_line(&mut self) {
        self.desired_col = None;
        // If already selecting full lines, expand by one more line (VS Code behavior)
        if let Some((anchor_line, 0)) = self.selection_anchor {
            if self.cursor_col == 0 && self.cursor_line > anchor_line {
                // Already have a line selection; extend one more line down
                if self.cursor_line + 1 < self.line_count() {
                    self.cursor_line += 1;
                    return;
                } else {
                    self.cursor_col = self.line_len(self.cursor_line);
                    return;
                }
            }
        }
        self.selection_anchor = Some((self.cursor_line, 0));
        if self.cursor_line + 1 < self.line_count() {
            self.cursor_line += 1;
            self.cursor_col = 0;
        } else {
            self.cursor_col = self.line_len(self.cursor_line);
        }
    }

    /// Whitespace run for mouse selection, excluding the line ending. Clicking
    /// past the text can still select trailing whitespace on that line.
    pub(crate) fn whitespace_boundaries_at(&self, line: usize, col: usize) -> Option<(usize, usize)> {
        let len = self.line_len(line);
        if len == 0 {
            return None;
        }
        let rope_line = self.rope.line(line);
        let col = col.min(len - 1);
        if !rope_line.char(col).is_whitespace() {
            return None;
        }
        let start = col - rope_line.chars_at(col).reversed()
            .take_while(|ch| ch.is_whitespace()).count();
        let end = col + rope_line.chars_at(col).take(len - col)
            .take_while(|ch| ch.is_whitespace()).count();
        Some((start, end))
    }

    pub(crate) fn select_word_at_cursor(&mut self) {
        self.desired_col = None;
        let rope_line = self.rope.line(self.cursor_line);
        let line_len = rope_line.len_chars();
        if line_len == 0 {
            return;
        }

        let is_word_char = |c: char| c.is_alphanumeric() || c == '_';

        // If cursor is on a word char, select that word
        // If cursor is past content or on a non-word char, try the word to the left
        let col = if self.cursor_col < line_len && is_word_char(rope_line.char(self.cursor_col)) {
            self.cursor_col
        } else if self.cursor_col > 0 {
            let left = self.cursor_col.min(line_len) - 1;
            if is_word_char(rope_line.char(left)) {
                left
            } else {
                return;
            }
        } else {
            return;
        };

        let (start, end) = self.word_boundaries_at(self.cursor_line, col);
        self.selection_anchor = Some((self.cursor_line, start));
        self.cursor_col = end;
    }

    /// Get the word at cursor position without selecting it
    pub(crate) fn word_at_cursor(&self) -> Option<String> {
        let (start, end) = self.word_range_at_cursor()?;
        Some(self.rope.line(self.cursor_line).slice(start..end).to_string())
    }

    pub(crate) fn word_range_at_cursor(&self) -> Option<(usize, usize)> {
        if self.cursor_line >= self.rope.len_lines() {
            return None;
        }
        let line = self.rope.line(self.cursor_line);
        let line_char_count = line.len_chars();
        // Exclude trailing newline
        let content_len = if line_char_count > 0 && line.char(line_char_count - 1) == '\n' {
            if line_char_count > 1 && line.char(line_char_count - 2) == '\r' {
                line_char_count - 2
            } else {
                line_char_count - 1
            }
        } else {
            line_char_count
        };
        if self.cursor_col >= content_len {
            return None;
        }

        let is_word_char = |c: char| c.is_alphanumeric() || c == '_';
        let ch = line.char(self.cursor_col);
        if !is_word_char(ch) {
            return None;
        }

        let (start, end) = self.word_boundaries_at(self.cursor_line, self.cursor_col);
        if end == start {
            return None;
        }
        Some((start, end))
    }

    pub(crate) fn start_selection(&mut self) {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some((self.cursor_line, self.cursor_col));
        }
    }

    pub(crate) fn clear_selection(&mut self) {
        if let Some(view) = &mut self.markdown_preview_scroll {
            view.clear_selection();
        }
        self.selection_anchor = None;
    }

    pub(crate) fn has_selection(&self) -> bool {
        if let Some(view) = &self.markdown_preview_scroll {
            return view.has_selection();
        }
        self.selection_anchor.is_some_and(|anchor| anchor != (self.cursor_line, self.cursor_col))
    }

    /// Returns (start_line, start_col, end_line, end_col) normalized so start <= end
    pub(crate) fn selection_range(&self) -> Option<(usize, usize, usize, usize)> {
        let (anchor_line, anchor_col) = self.selection_anchor?;
        let (sl, sc, el, ec) = if (anchor_line, anchor_col) <= (self.cursor_line, self.cursor_col) {
            (anchor_line, anchor_col, self.cursor_line, self.cursor_col)
        } else {
            (self.cursor_line, self.cursor_col, anchor_line, anchor_col)
        };
        Some((sl, sc, el, ec))
    }

    /// Compute clamped char indices for the current selection.
    pub(crate) fn clamped_selection_indices(&self) -> Option<(usize, usize, usize, usize)> {
        let (sl, sc, el, ec) = self.selection_range()?;
        let max_line = self.line_count().saturating_sub(1);
        let sl = sl.min(max_line);
        let el = el.min(max_line);
        let start_col = sc.min(self.line_len(sl));
        let end_col = ec.min(self.line_len(el));
        let start_idx = self.rope.line_to_char(sl) + start_col;
        let end_idx = self.rope.line_to_char(el) + end_col;
        if start_idx >= end_idx || end_idx > self.rope.len_chars() {
            return None;
        }
        Some((start_idx, end_idx, sl, start_col))
    }

    /// Get selected text
    pub(crate) fn selected_text(&self) -> Option<String> {
        if let Some(view) = &self.markdown_preview_scroll {
            return view.selected_text();
        }
        let (start_idx, end_idx, _, _) = self.clamped_selection_indices()?;
        Some(self.rope.slice(start_idx..end_idx).to_string())
    }

    /// Find/Replace accepts no LF in its initial query. Inspect borrowed chunks
    /// first so a rejected multiline selection is never copied in full.
    pub(crate) fn selected_search_text(&self) -> Option<String> {
        if self.markdown_preview_scroll.is_some() {
            return self.selected_text().filter(|text| !text.is_empty() && !text.contains('\n'));
        }
        let (start, end, _, _) = self.clamped_selection_indices()?;
        let selection = self.rope.slice(start..end);
        if selection.chunks().any(|chunk| chunk.contains('\n')) { return None; }
        Some(selection.to_string())
    }

    /// Check if a (line, col) position is within the current selection
    pub(crate) fn selection_contains(&self, line: usize, col: usize) -> bool {
        if let Some((sl, sc, el, ec)) = self.selection_range() {
            if sl == el {
                line == sl && col >= sc && col < ec
            } else if line == sl {
                col >= sc
            } else if line == el {
                col < ec
            } else {
                line > sl && line < el
            }
        } else {
            false
        }
    }

    /// Ctrl+D: select word under cursor, or find next occurrence of selection
    pub(crate) fn select_next_occurrence(&mut self) {
        self.desired_col = None;
        if let Some(text) = self.selected_text() {
            self.select_next_matching_occurrence(&text, true);
        } else {
            self.select_word_at_cursor();
        }
    }

    /// Search each side once, sweeping selected ranges alongside the matches.
    /// Checking full ranges avoids duplicates in reverse selections as well
    /// as partial overlaps that would later merge into a larger edit.
    fn select_next_matching_occurrence(&mut self, text: &str, keep_current: bool) {
        if text.is_empty() {
            return;
        }
        let search = crate::editor::RopeSearch::new(text);
        let needle_len = search.char_len();
        let total = self.rope.len_chars();
        let Some((start, cursor, _, _)) = self.clamped_selection_indices() else { return; };
        let mut existing = Vec::with_capacity(self.extra_cursors.len() + 1);
        existing.push((start, cursor));
        for (index, &(line, col)) in self.extra_cursors.iter().enumerate() {
            let state = crate::editor_multicursor::MultiCursor {
                line, col, anchor: self.extra_cursor_anchors.get(index).copied().flatten(),
            };
            if let Some(span) = self.selection_span_for_cursor(state) {
                existing.push((span.start_idx, span.end_idx));
            }
        }
        existing.sort_unstable();
        let find = |from, through| {
            let mut covered = 0;
            search.matches(&self.rope, from, through).find(|&start| {
                while covered < existing.len() && existing[covered].1 <= start {
                    covered += 1;
                }
                covered == existing.len() || existing[covered].0 >= start + needle_len
            })
        };
        let found = find(cursor, total).or_else(|| find(0, cursor));
        if let Some(start) = found {
            let end = start + needle_len;
            let line = self.rope.char_to_line(start);
            let col = start - self.rope.line_to_char(line);
            let end_line = self.rope.char_to_line(end);
            let end_col = end - self.rope.line_to_char(end_line);
            if keep_current {
                self.extra_cursor_anchors.resize(self.extra_cursors.len(), None);
                self.extra_cursors.push((self.cursor_line, self.cursor_col));
                self.extra_cursor_anchors.push(self.selection_anchor);
            }
            self.selection_anchor = Some((line, col));
            self.cursor_line = end_line;
            self.cursor_col = end_col;
            self.ensure_visible();
        }
    }

    /// Ctrl+K Ctrl+D: skip current occurrence and select next
    pub(crate) fn skip_occurrence_select_next(&mut self) {
        self.desired_col = None;
        if let Some(text) = self.selected_text() {
            self.select_next_matching_occurrence(&text, false);
        }
    }

    /// Ctrl+Shift+L: select all occurrences of current selection (or word under cursor)
    pub(crate) fn select_all_occurrences(&mut self) {
        self.desired_col = None;
        if self.clamped_selection_indices().is_none() {
            self.select_word_at_cursor();
        }

        let text = match self.selected_text() {
            Some(t) if !t.is_empty() => t,
            _ => return,
        };
        let search = crate::editor::RopeSearch::new(&text);
        let needle_len = search.char_len();
        let mut positions = search.matches(&self.rope, 0, self.rope.len_chars());
        let Some(first) = positions.next() else { return; };

        self.extra_cursors.clear();
        self.extra_cursor_anchors.clear();

        let mut start_line = None;
        let mut end_line = None;
        self.selection_anchor = Some(occurrence_position(&self.rope, first, &mut start_line));
        (self.cursor_line, self.cursor_col) =
            occurrence_position(&self.rope, first + needle_len, &mut end_line);

        for pos in positions {
            self.extra_cursor_anchors.push(Some(occurrence_position(&self.rope, pos, &mut start_line)));
            self.extra_cursors.push(occurrence_position(&self.rope, pos + needle_len, &mut end_line));
        }

        self.ensure_visible();
    }

    pub(crate) fn select_all(&mut self) {
        if let Some(view) = &mut self.markdown_preview_scroll {
            view.select_all();
            return;
        }
        self.desired_col = None;
        self.extra_cursors.clear();
        self.extra_cursor_anchors.clear();
        self.selection_anchor = Some((0, 0));
        let last_line = self.line_count().saturating_sub(1);
        let last_col = self.line_len(last_line);
        self.cursor_line = last_line;
        self.cursor_col = last_col;
    }

    /// A selection history belongs to exact text, not just a view. Edits,
    /// reloads and changes in another pane must not resurrect stale positions.
    pub(crate) fn remember_selection(&mut self) {
        if !self.selection_history_source.as_ref().is_some_and(|source| source.is_instance(&self.rope)) {
            self.selection_history.clear();
            self.selection_history_source = Some(self.rope.clone());
        }
        let current_state = (self.selection_anchor, (self.cursor_line, self.cursor_col));
        self.selection_history.push_back(current_state);
        if self.selection_history.len() > 50 {
            self.selection_history.pop_front();
        }
    }

    /// Expand selection: word → brackets → line → all
    pub(crate) fn expand_selection(&mut self) {
        self.desired_col = None;
        self.remember_selection();

        if !self.has_selection() {
            // First: select current word
            let rope_line = self.rope.line(self.cursor_line);
            let line_len = rope_line.len_chars();
            let is_word = |c: char| c.is_alphanumeric() || c == '_';
            if self.cursor_col < line_len && is_word(rope_line.char(self.cursor_col)) {
                let (start, end) = self.word_boundaries_at(self.cursor_line, self.cursor_col);
                self.selection_anchor = Some((self.cursor_line, start));
                self.cursor_col = end;
            } else {
                // Select entire line
                self.selection_anchor = Some((self.cursor_line, 0));
                self.cursor_col = self.line_len(self.cursor_line);
            }
            return;
        }

        // Already have selection — try to expand to enclosing brackets
        let Some((start_char, end_char, _, _)) = self.clamped_selection_indices() else {
            return;
        };
        if let Some((open, close)) = self.enclosing_bracket_range(start_char, end_char) {
            // Repeating expansion must progress when the inner contents are
            // already selected; include the delimiters in that case.
            let (new_start, new_end) = if open + 1 == start_char && close == end_char {
                (open, close + 1)
            } else {
                (open + 1, close)
            };
            let new_sl = self.rope.char_to_line(new_start);
            let new_sc = new_start - self.rope.line_to_char(new_sl);
            let new_el = self.rope.char_to_line(new_end);
            let new_ec = new_end - self.rope.line_to_char(new_el);
            self.selection_anchor = Some((new_sl, new_sc));
            self.cursor_line = new_el;
            self.cursor_col = new_ec;
            return;
        }

        // Fallback: select all
        self.select_all();
    }

    /// Match actual enclosing pairs, skipping completed siblings and any
    /// pairs consumed by the selection. Three bounded counters keep the scan
    /// linear even for malformed text containing long runs of unmatched opens.
    fn enclosing_bracket_range(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        let openers = ['(', '[', '{'];
        let closers = [')', ']', '}'];
        let mut before = [0usize; 3];
        let mut after = [0usize; 3];
        for ch in self.rope.slice(start..end).chars() {
            if let Some(kind) = openers.iter().position(|&open| ch == open) {
                after[kind] += 1;
            } else if let Some(kind) = closers.iter().position(|&close| ch == close) {
                if after[kind] == 0 { before[kind] += 1; }
                else { after[kind] -= 1; }
            }
        }
        let mut matches: [Option<Option<usize>>; 3] = [None; 3];
        for (offset, ch) in self.rope.chars_at(start).reversed().enumerate() {
            if let Some(kind) = closers.iter().position(|&close| ch == close) {
                before[kind] += 1;
            } else if let Some(kind) = openers.iter().position(|&open| ch == open) {
                if before[kind] > 0 {
                    before[kind] -= 1;
                    continue;
                }
                // Usually the closest opener succeeds immediately. Cache a
                // failed suffix too, so malformed opens cannot rescan it.
                let close = *matches[kind].get_or_insert_with(|| {
                    let mut depth = after[kind] + 1;
                    for (offset, ch) in self.rope.chars_at(end).enumerate() {
                        if ch == openers[kind] { depth += 1; }
                        else if ch == closers[kind] {
                            depth -= 1;
                            if depth == 0 { return Some(end + offset); }
                        }
                    }
                    None
                });
                if let Some(close) = close {
                    return Some((start - offset - 1, close));
                }
            }
        }
        None
    }

    pub(crate) fn shrink_selection(&mut self) {
        self.desired_col = None;
        if !self.selection_history_source.as_ref().is_some_and(|source| source.is_instance(&self.rope)) {
            self.selection_history.clear();
            self.selection_history_source = None;
            return;
        }
        if let Some((anchor, (line, column))) = self.selection_history.pop_back() {
            self.selection_anchor = anchor;
            self.cursor_line = line;
            self.cursor_col = column;
            self.ensure_visible();
        }
        if self.selection_history.is_empty() {
            self.selection_history_source = None;
        }
    }

    /// Check if a position (line, col) is within the selection
    #[cfg(test)]
    pub(crate) fn is_in_selection(&self, line: usize, col: usize) -> bool {
        if let Some((sl, sc, el, ec)) = self.selection_range() {
            if line < sl || line > el {
                return false;
            }
            if line == sl && line == el {
                return col >= sc && col < ec;
            }
            if line == sl {
                return col >= sc;
            }
            if line == el {
                return col < ec;
            }
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
#[path = "editor_navigation_performance_tests.rs"]
mod navigation_performance_tests;
