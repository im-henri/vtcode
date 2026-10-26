use rustc_hash::FxHashMap;

use crate::editor::Editor;

#[derive(Clone, Copy)]
struct PreparedCursor {
    index: usize,
    line: usize,
    col: usize,
    line_start: usize,
    line_len: usize,
    position: usize,
    start: usize,
    end: usize,
    virtual_column: bool,
}

#[derive(Clone, Copy)]
struct CursorEdit<'a> {
    index: usize,
    /// Original cursor line, its char start and its content length.
    line: (usize, usize, usize),
    start: usize,
    end: usize,
    text: &'a str,
    text_chars: usize,
    caret_offset: usize,
}

impl Editor {
    /// Cache metadata for each touched line before mutating the rope. A line
    /// containing thousands of cursors needs one pair of rope seeks, rather
    /// than repeatedly finding and measuring it during every edit.
    fn prepare_multi_cursor_edits(&self) -> Vec<PreparedCursor> {
        let cursors = self.collect_multi_cursors();
        let max_line = self.line_count().saturating_sub(1);
        let mut lines = FxHashMap::default();
        let simple_order = cursors.windows(2).all(|pair| pair[0].line <= pair[1].line)
            || cursors.windows(2).all(|pair| pair[0].line >= pair[1].line);
        let simple_order = simple_order
            && cursors.iter().all(|cursor| {
                cursor
                    .anchor
                    .is_none_or(|(line, _)| line.min(max_line) == cursor.line.min(max_line))
            });
        let mut previous = None;
        let mut line_info = |line: usize| {
            let line = line.min(max_line);
            let (start, len) = match previous {
                Some((cached, start, len)) if cached == line => (start, len),
                _ if simple_order => (self.rope.line_to_char(line), self.line_len(line)),
                _ => *lines
                    .entry(line)
                    .or_insert_with(|| (self.rope.line_to_char(line), self.line_len(line))),
            };
            previous = Some((line, start, len));
            (line, start, len)
        };
        cursors
            .into_iter()
            .enumerate()
            .map(|(index, cursor)| {
                let (line, line_start, line_len) = line_info(cursor.line);
                let col = cursor.col.min(line_len);
                let position = line_start + col;
                let anchor = cursor
                    .anchor
                    .map(|(line, col)| {
                        let (_, start, len) = line_info(line);
                        start + col.min(len)
                    })
                    .unwrap_or(position);
                PreparedCursor {
                    index,
                    line,
                    col,
                    line_start,
                    line_len,
                    position,
                    start: position.min(anchor),
                    end: position.max(anchor),
                    virtual_column: cursor.col > line_len,
                }
            })
            .collect()
    }

    /// Commit edits in original rope coordinates. A sorted sweep both merges
    /// overlapping selections and computes final cursor positions, avoiding
    /// mutable per-line offset maps and rescans after each previous mutation.
    fn apply_multi_cursor_edits(&mut self, mut edits: Vec<CursorEdit<'_>>) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        if edits.is_empty() {
            return;
        }
        let count = edits.len();
        let same_lines = edits.iter().all(|edit| {
            let (_, start, len) = edit.line;
            edit.start >= start && edit.end <= start + len && !edit.text.contains(['\r', '\n'])
        });
        edits.sort_unstable_by(|a, b| {
            a.start
                .cmp(&b.start)
                .then_with(|| b.end.cmp(&a.end))
                .then_with(|| a.index.cmp(&b.index))
        });
        let mut destinations = Vec::with_capacity(count);
        let mut positions = vec![(0, 0); count];
        let mut merged = Vec::with_capacity(count);
        let mut removed = 0;
        let mut inserted = 0;
        let mut index = 0;
        let mut line_delta = (usize::MAX, 0, 0);
        while index < count {
            let first = index;
            let mut edit = edits[index];
            index += 1;
            while index < count
                && (edits[index].start < edit.end || edits[index].start == edit.start)
            {
                edit.end = edit.end.max(edits[index].end);
                if edit.text.is_empty() && !edits[index].text.is_empty() {
                    edit.text = edits[index].text;
                    edit.text_chars = edits[index].text_chars;
                }
                index += 1;
            }
            let destination = edit.start - removed + inserted;
            if line_delta.0 != edit.line.0 {
                line_delta = (edit.line.0, 0, 0);
            }
            for original in &edits[first..index] {
                let caret = if edit.start == edit.end && edit.text.is_empty() {
                    // A caret may skip an existing closer without editing it.
                    original.caret_offset
                } else { original.caret_offset.min(edit.text_chars) };
                if same_lines {
                    positions[original.index] = (
                        edit.line.0,
                        edit.start - edit.line.1 - line_delta.1 + line_delta.2 + caret,
                    );
                } else {
                    destinations.push((original.index, destination + caret));
                }
            }
            line_delta.1 += edit.end - edit.start;
            line_delta.2 += edit.text_chars;
            removed += edit.end - edit.start;
            inserted += edit.text_chars;
            if edit.start != edit.end || !edit.text.is_empty() {
                merged.push(edit);
            }
        }
        if !merged.is_empty() {
            let first_line = self.rope.char_to_line(merged[0].start);
            let last_line = self.rope.char_to_line(merged.last().unwrap().end);
            self.save_undo();
            self.mark_syntax_dirty_range(first_line, last_line);
            let mut changed_lines = false;
            let repeated = (merged.len() > 1 && merged[0].text.len() >= 4_096
                && merged.iter().all(|edit| std::ptr::eq(edit.text, merged[0].text)))
                .then(|| ropey::Rope::from_str(merged[0].text));
            let repeated_lines = repeated.as_ref().map(|rope| rope.len_lines() - 1);
            let dense = if repeated.is_some() { None } else {
                crate::editor_text_ops::dense_edit_text(&self.rope, merged.iter().map(|edit| (edit.start, edit.end, edit.text)))
            };
            for edit in merged.iter().rev() {
                let inserted_lines = repeated_lines.unwrap_or_else(|| edit.text.bytes().filter(|&c| c == b'\n').count());
                let (line, removed_lines) =
                    if !same_lines && (edit.start != edit.end || inserted_lines > 0) {
                        let line = self.rope.char_to_line(edit.start);
                        (line, self.rope.char_to_line(edit.end) - line)
                    } else {
                        (0, 0)
                    };
                if dense.is_none() {
                    if edit.start != edit.end {
                        self.rope.remove(edit.start..edit.end);
                    }
                    if !edit.text.is_empty() {
                        if let Some(repeated) = &repeated {
                            let suffix = self.rope.split_off(edit.start);
                            self.rope.append(repeated.clone());
                            self.rope.append(suffix);
                        } else {
                            self.rope.insert(edit.start, edit.text);
                        }
                    }
                }
                if inserted_lines != removed_lines {
                    self.shift_bookmarks(
                        line + 1,
                        inserted_lines as isize - removed_lines as isize,
                    );
                }
                changed_lines |= inserted_lines > 0 || removed_lines > 0;
            }
            if let Some((first, last, text)) = dense {
                if first != last {
                    self.rope.remove(first..last);
                }
                if !text.is_empty() {
                    self.rope.insert(first, &text);
                }
            }
            self.modified = true;
            if changed_lines {
                self.clear_stale_folds();
            }
        }
        if same_lines {
            self.set_valid_multi_cursor_positions(positions);
            return;
        }
        let mut cached_line = None;
        for (index, position) in destinations {
            let (line, start, end) = match cached_line {
                Some((line, start, end)) if position >= start && position < end => {
                    (line, start, end)
                }
                _ => {
                    let line = self.rope.char_to_line(position);
                    let start = self.rope.line_to_char(line);
                    let end = if line + 1 < self.line_count() {
                        self.rope.line_to_char(line + 1)
                    } else {
                        self.rope.len_chars().saturating_add(1)
                    };
                    (line, start, end)
                }
            };
            cached_line = Some((line, start, end));
            positions[index] = (line, position - start);
        }
        self.set_multi_cursor_positions(positions);
            })
    }

    pub(crate) fn multi_cursor_insert_char(&mut self, c: char) {
        crate::editor_document::edit!(self, {
        let cursors = self.prepare_multi_cursor_edits();
        let has_selection = cursors.iter().any(|cursor| cursor.start != cursor.end);
        if !has_selection
            && matches!(c, ')' | ']' | '}' | '"' | '\'' | '`')
            && cursors
                .iter()
                .all(|cursor| self.rope.get_char(cursor.position) == Some(c))
        {
            self.desired_col = None;
            self.break_undo_group();
            self.set_multi_cursor_positions(
                cursors
                    .iter()
                    .map(|cursor| (cursor.line, cursor.col + 1))
                    .collect(),
            );
            return;
        }
        let closing = if has_selection || c == '\n' {
            None
        } else {
            match c {
                '(' => Some(')'),
                '[' => Some(']'),
                '{' => Some('}'),
                '"' => Some('"'),
                '\'' => Some('\''),
                '`' => Some('`'),
                _ => None,
            }
        };
        let mut encoded = [0; 4];
        let text = c.encode_utf8(&mut encoded);
        let pair = match closing {
            Some(')') => Some("()"),
            Some(']') => Some("[]"),
            Some('}') => Some("{}"),
            Some('"') => Some("\"\""),
            Some('\'') => Some("''"),
            Some('`') => Some("``"),
            _ => None,
        };
        let edits = cursors
            .iter()
            .map(|cursor| {
                if cursor.start == cursor.end && matches!(c, ')' | ']' | '}' | '"' | '\'' | '`')
                    && self.rope.get_char(cursor.position) == Some(c)
                {
                    return CursorEdit {
                        index: cursor.index, line: (cursor.line, cursor.line_start, cursor.line_len),
                        start: cursor.position, end: cursor.position,
                        text: "", text_chars: 0, caret_offset: 1,
                    };
                }
                let should_close = pair.is_some()
                    && !self
                        .rope
                        .get_char(cursor.position)
                        .is_some_and(|c| c.is_alphanumeric() || c == '_')
                    && !(matches!(c, '"' | '\'' | '`') && cursor.col > 0
                        && self.rope.get_char(cursor.position - 1).is_some_and(|ch| ch.is_alphanumeric() || ch == '_'));
                let overwrite = !has_selection
                    && c != '\n'
                    && closing.is_none()
                    && self.overwrite_mode
                    && cursor.col < cursor.line_len;
                CursorEdit {
                    index: cursor.index,
                    line: (cursor.line, cursor.line_start, cursor.line_len),
                    start: cursor.start,
                    end: cursor.end + usize::from(overwrite),
                    text: if should_close {
                        pair.unwrap_or(text)
                    } else {
                        text
                    },
                    text_chars: if should_close { 2 } else { 1 },
                    caret_offset: 1,
                }
            })
            .collect();
        self.apply_multi_cursor_edits(edits);
            })
    }

    /// Normalize overlap before deciding whether clipboard lines distribute
    /// across carets. Keep original cursor indices so the primary caret stays
    /// primary even when it is below the others.
    pub(crate) fn multi_cursor_paste(&mut self, text: &str) {
        crate::editor_document::edit!(self, {
        let mut cursors = self.prepare_multi_cursor_edits();
        cursors.sort_unstable_by_key(|cursor| (cursor.start, std::cmp::Reverse(cursor.end), cursor.index));
        let mut groups = Vec::new();
        let mut index = 0;
        while index < cursors.len() {
            let first = index;
            let start = cursors[index].start;
            let mut end = cursors[index].end;
            index += 1;
            while index < cursors.len() && (cursors[index].start < end || cursors[index].start == start) {
                end = end.max(cursors[index].end);
                index += 1;
            }
            groups.push(first..index);
        }
        let distributed: Vec<_> = if text.split('\n').count() == groups.len() {
            text.split('\n').map(|line| (line, line.chars().count())).collect()
        } else { Vec::new() };
        let repeated_chars = if distributed.is_empty() { text.chars().count() } else { 0 };
        let mut edits = Vec::with_capacity(cursors.len());
        for (index, group) in groups.into_iter().enumerate() {
            let (text, chars) = distributed.get(index).copied().unwrap_or((text, repeated_chars));
            for cursor in &cursors[group] {
                edits.push(CursorEdit {
                    index: cursor.index, line: (cursor.line, cursor.line_start, cursor.line_len),
                    start: cursor.start, end: cursor.end, text, text_chars: chars, caret_offset: chars,
                });
            }
        }
        self.apply_multi_cursor_edits(edits);
            })
    }

    pub(crate) fn multi_cursor_insert_tab(&mut self) {
        crate::editor_document::edit!(self, {
        let mut cursors = self.prepare_multi_cursor_edits();
        cursors.sort_unstable_by_key(|cursor| (cursor.line, cursor.col, cursor.index));
        let mut previous_line = None;
        let mut previous_col = 0;
        let mut cells = 0;
        let mut chars = self.rope.chars();
        let tab_size = self.tab_size.max(1);
        let spaces = crate::helpers::spaces(if self.use_tabs { 0 } else { tab_size });
        let edits = cursors
            .iter()
            .map(|cursor| {
                if previous_line != Some(cursor.line) {
                    previous_line = Some(cursor.line);
                    previous_col = 0;
                    cells = 0;
                    chars = self.rope.chars_at(cursor.line_start);
                }
                if !self.use_tabs {
                    // Walk each source prefix once. Earlier inserted spaces
                    // also affect any hard tab between this caret and the last.
                    for ch in chars.by_ref().take(cursor.col - previous_col) {
                        cells += if ch == '\t' { tab_size - cells % tab_size }
                            else { unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) };
                    }
                }
                previous_col = cursor.col;
                let cols = if self.use_tabs {
                    1
                } else {
                    tab_size - cells % tab_size
                };
                cells += cols;
                CursorEdit {
                    index: cursor.index,
                    line: (cursor.line, cursor.line_start, cursor.line_len),
                    start: cursor.position,
                    end: cursor.position,
                    text: if self.use_tabs {
                        "\t"
                    } else {
                        &spaces[..cols]
                    },
                    text_chars: cols,
                    caret_offset: cols,
                }
            })
            .collect();
        self.apply_multi_cursor_edits(edits);
            })
    }

    pub(crate) fn multi_cursor_backspace(&mut self) {
        crate::editor_document::edit!(self, {
        let cursors = self.prepare_multi_cursor_edits();
        let edits = cursors
            .iter()
            .map(|cursor| {
                let (mut start, end) = (cursor.start, cursor.end);
                if start == end && !cursor.virtual_column && start > 0 {
                    start -= if cursor.col == 0
                        && start >= 2
                        && self.rope.char(start - 2) == '\r'
                        && self.rope.char(start - 1) == '\n'
                    {
                        2
                    } else {
                        1
                    };
                }
                CursorEdit {
                    index: cursor.index,
                    line: (cursor.line, cursor.line_start, cursor.line_len),
                    start,
                    end,
                    text: "",
                    text_chars: 0,
                    caret_offset: 0,
                }
            })
            .collect();
        self.apply_multi_cursor_edits(edits);
            })
    }

    pub(crate) fn multi_cursor_delete(&mut self) {
        crate::editor_document::edit!(self, {
        let cursors = self.prepare_multi_cursor_edits();
        let edits = cursors
            .iter()
            .map(|cursor| {
                let (start, mut end) = (cursor.start, cursor.end);
                if start == end && !cursor.virtual_column && end < self.rope.len_chars() {
                    end += if self.rope.char(end) == '\r'
                        && self.rope.get_char(end + 1) == Some('\n')
                    {
                        2
                    } else {
                        1
                    };
                }
                CursorEdit {
                    index: cursor.index,
                    line: (cursor.line, cursor.line_start, cursor.line_len),
                    start,
                    end,
                    text: "",
                    text_chars: 0,
                    caret_offset: 0,
                }
            })
            .collect();
        self.apply_multi_cursor_edits(edits);
            })
    }

    pub(crate) fn multi_cursor_delete_word_left(&mut self) {
        crate::editor_document::edit!(self, {
        let cursors = self.prepare_multi_cursor_edits();
        let edits = cursors
            .iter()
            .map(|cursor| {
                let (mut start, end) = (cursor.start, cursor.end);
                if start == end {
                    let (line, col) = self.word_left_position(cursor.line, cursor.col);
                    start = if line == cursor.line {
                        cursor.line_start + col
                    } else {
                        self.rope.line_to_char(line) + col
                    };
                }
                CursorEdit {
                    index: cursor.index,
                    line: (cursor.line, cursor.line_start, cursor.line_len),
                    start,
                    end,
                    text: "",
                    text_chars: 0,
                    caret_offset: 0,
                }
            })
            .collect();
        self.apply_multi_cursor_edits(edits);
            })
    }

    pub(crate) fn multi_cursor_delete_word_right(&mut self) {
        crate::editor_document::edit!(self, {
        let cursors = self.prepare_multi_cursor_edits();
        let edits = cursors
            .iter()
            .map(|cursor| {
                let (start, mut end) = (cursor.start, cursor.end);
                if start == end {
                    if cursor.col == cursor.line_len {
                        end += if self.rope.get_char(end) == Some('\r')
                            && self.rope.get_char(end + 1) == Some('\n')
                        {
                            2
                        } else {
                            usize::from(end < self.rope.len_chars())
                        };
                    } else {
                        let (_, col) = self.word_right_position(cursor.line, cursor.col);
                        let col = col
                            + self
                                .rope
                                .line(cursor.line)
                                .chars_at(col)
                                .take(cursor.line_len - col)
                                .take_while(|c| c.is_whitespace())
                                .count();
                        end = cursor.line_start + col;
                    }
                }
                CursorEdit {
                    index: cursor.index,
                    line: (cursor.line, cursor.line_start, cursor.line_len),
                    start,
                    end,
                    text: "",
                    text_chars: 0,
                    caret_offset: 0,
                }
            })
            .collect();
        self.apply_multi_cursor_edits(edits);
            })
    }

    /// Transpose the two characters around the cursor
    pub(crate) fn transpose_chars(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        let line_len = self.line_len(self.cursor_line);
        if self.cursor_col == 0 || line_len < 2 {
            return;
        }
        self.save_undo();

        let col = self.cursor_col.min(line_len);
        let (a_col, b_col) = if col >= line_len {
            (col - 2, col - 1)
        } else {
            (col - 1, col)
        };

        let line_start = self.rope.line_to_char(self.cursor_line);
        let a_idx = line_start + a_col;
        let b_idx = line_start + b_col;

        if b_idx >= self.rope.len_chars() {
            return;
        }

        let a = self.rope.char(a_idx);
        let b = self.rope.char(b_idx);

        self.rope.remove(a_idx..=b_idx);
        let swapped = format!("{b}{a}");
        self.rope.insert(a_idx, &swapped);
        self.cursor_col = b_col + 1;
        self.clamp_cursor_col();
        self.modified = true;
            })
    }

    /// Delete prefixes/suffixes for every cursor through the same overlap-safe
    /// mapping used by typing and word deletion.
    pub(crate) fn multi_cursor_delete_to_line_boundary(&mut self, to_end: bool) {
        crate::editor_document::edit!(self, {
        let cursors = self.prepare_multi_cursor_edits();
        let edits = cursors
            .iter()
            .map(|cursor| CursorEdit {
                index: cursor.index,
                line: (cursor.line, cursor.line_start, cursor.line_len),
                start: if to_end {
                    cursor.position
                } else {
                    cursor.line_start
                },
                end: if to_end {
                    cursor.line_start + cursor.line_len
                } else {
                    cursor.position
                },
                text: "",
                text_chars: 0,
                caret_offset: 0,
            })
            .collect();
        self.apply_multi_cursor_edits(edits);
            })
    }

    /// Transpose adjacent words by reading only those words and their separator.
    pub(crate) fn transpose_words(&mut self) {
        crate::editor_document::edit!(self, {
        let line = self.rope.line(self.cursor_line);
        let col = self.cursor_col.min(self.line_len(self.cursor_line));
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let word_end = col + line.chars_at(col).take_while(|&c| is_word(c)).count();
        let word_start = col
            - line
                .chars_at(col)
                .reversed()
                .take_while(|&c| is_word(c))
                .count();
        if word_start == word_end {
            return;
        }
        let prev_end = word_start
            - line
                .chars_at(word_start)
                .reversed()
                .take_while(|&c| !is_word(c))
                .count();
        if prev_end == 0 {
            return;
        }
        let prev_start = prev_end
            - line
                .chars_at(prev_end)
                .reversed()
                .take_while(|&c| is_word(c))
                .count();
        let mut replacement = String::new();
        for slice in [
            line.slice(word_start..word_end),
            line.slice(prev_end..word_start),
            line.slice(prev_start..prev_end),
        ] {
            for chunk in slice.chunks() {
                replacement.push_str(chunk);
            }
        }
        self.desired_col = None;
        self.save_undo();
        let start = self.rope.line_to_char(self.cursor_line);
        self.rope.remove(start + prev_start..start + word_end);
        self.rope.insert(start + prev_start, &replacement);
        self.cursor_col = word_end;
        self.modified = true;
            })
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::Editor;

    fn text_of(ed: &Editor) -> String {
        ed.rope.to_string()
    }

    // ── multi_cursor_insert_char ─────────────────────────────────────

    #[test]
    fn insert_char_single_cursor() {
        let mut ed = Editor::test_with_text("hello");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.multi_cursor_insert_char('!');
        assert_eq!(text_of(&ed), "hello!");
        assert_eq!(ed.cursor_col, 6);
    }

    #[test]
    fn insert_char_at_beginning() {
        let mut ed = Editor::test_with_text("world");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.multi_cursor_insert_char('H');
        assert_eq!(text_of(&ed), "Hworld");
        assert_eq!(ed.cursor_col, 1);
    }

    #[test]
    fn insert_char_multiple_cursors() {
        let mut ed = Editor::test_with_text("ab\ncd\n");
        ed.cursor_line = 0;
        ed.cursor_col = 1;
        ed.extra_cursors.push((1, 1));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_insert_char('X');
        let t = text_of(&ed);
        assert!(t.contains("aXb"), "got: {t}");
        assert!(t.contains("cXd"), "got: {t}");
    }

    #[test]
    fn insert_char_replaces_selection() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.selection_anchor = Some((0, 0));
        ed.multi_cursor_insert_char('X');
        let t = text_of(&ed);
        assert!(t.starts_with("X world"), "got: {t}");
    }

    #[test]
    fn insert_auto_close_paren() {
        let mut ed = Editor::test_with_text("");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.multi_cursor_insert_char('(');
        let t = text_of(&ed);
        assert_eq!(t, "()");
        assert_eq!(ed.cursor_col, 1);
    }

    #[test]
    fn insert_auto_close_bracket() {
        let mut ed = Editor::test_with_text("");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.multi_cursor_insert_char('[');
        let t = text_of(&ed);
        assert_eq!(t, "[]");
    }

    #[test]
    fn insert_auto_close_brace() {
        let mut ed = Editor::test_with_text("");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.multi_cursor_insert_char('{');
        let t = text_of(&ed);
        assert_eq!(t, "{}");
    }

    // ── multi_cursor_insert_tab ──────────────────────────────────────

    #[test]
    fn insert_tab_default_spaces() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.tab_size = 4;
        ed.use_tabs = false;
        ed.multi_cursor_insert_tab();
        let t = text_of(&ed);
        assert!(t.starts_with("    hello"), "got: {t}");
    }

    #[test]
    fn insert_tab_with_tabs() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.use_tabs = true;
        ed.multi_cursor_insert_tab();
        let t = text_of(&ed);
        assert!(t.starts_with("\thello"), "got: {t}");
    }

    // ── multi_cursor_backspace ───────────────────────────────────────

    #[test]
    fn backspace_single_char() {
        let mut ed = Editor::test_with_text("ab\n");
        ed.cursor_line = 0;
        ed.cursor_col = 1;
        ed.multi_cursor_backspace();
        assert_eq!(text_of(&ed), "b\n");
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn backspace_at_line_start_joins_lines() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.multi_cursor_backspace();
        let t = text_of(&ed);
        assert!(t.starts_with("helloworld"), "got: {t}");
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 5);
    }

    #[test]
    fn backspace_deletes_selection() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.selection_anchor = Some((0, 0));
        ed.multi_cursor_backspace();
        let t = text_of(&ed);
        assert!(t.starts_with(" world"), "got: {t}");
    }

    #[test]
    fn backspace_at_start_does_nothing() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.multi_cursor_backspace();
        assert_eq!(text_of(&ed), "hello\n");
    }

    #[test]
    fn backspace_multi_cursor() {
        let mut ed = Editor::test_with_text("abc\nxyz\n");
        ed.cursor_line = 0;
        ed.cursor_col = 2;
        ed.extra_cursors.push((1, 2));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_backspace();
        let t = text_of(&ed);
        assert!(t.contains("ac"), "got: {t}");
        assert!(t.contains("xz"), "got: {t}");
    }

    // ── multi_cursor_delete ──────────────────────────────────────────

    #[test]
    fn delete_single_char() {
        let mut ed = Editor::test_with_text("ab\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.multi_cursor_delete();
        assert_eq!(text_of(&ed), "b\n");
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn delete_at_end_joins_lines() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.multi_cursor_delete();
        let t = text_of(&ed);
        assert!(t.starts_with("helloworld"), "got: {t}");
    }

    #[test]
    fn delete_deletes_selection() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.selection_anchor = Some((0, 0));
        ed.multi_cursor_delete();
        let t = text_of(&ed);
        assert!(t.starts_with(" world"), "got: {t}");
    }

    // ── multi_cursor_delete_word_left ────────────────────────────────

    #[test]
    fn delete_word_left_basic() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.multi_cursor_delete_word_left();
        let t = text_of(&ed);
        assert!(t.starts_with(" world"), "got: {t}");
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn delete_word_left_at_start_does_nothing() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.multi_cursor_delete_word_left();
        assert_eq!(text_of(&ed), "hello\n");
    }

    // ── multi_cursor_delete_word_right ───────────────────────────────

    #[test]
    fn delete_word_right_basic() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.multi_cursor_delete_word_right();
        let t = text_of(&ed);
        // delete_word_right eats the word + trailing whitespace
        assert!(t.starts_with("world"), "got: {t}");
    }

    // ── transpose_chars ──────────────────────────────────────────────

    #[test]
    fn transpose_chars_middle() {
        let mut ed = Editor::test_with_text("abc\n");
        ed.cursor_line = 0;
        ed.cursor_col = 1;
        ed.transpose_chars();
        assert_eq!(text_of(&ed), "bac\n");
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn transpose_chars_at_start_does_nothing() {
        let mut ed = Editor::test_with_text("abc\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.transpose_chars();
        assert_eq!(text_of(&ed), "abc\n");
    }

    // ── transpose_words ──────────────────────────────────────────────

    #[test]
    fn transpose_words_basic() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 6;
        ed.transpose_words();
        let t = text_of(&ed);
        assert_eq!(t, "world hello\n");
    }

    // ── CRLF multi-cursor backspace/delete ────────────────────────

    #[test]
    fn multi_cursor_backspace_crlf_line_join() {
        let mut ed = Editor::test_with_text("hello\r\nworld\r\n");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.multi_cursor_backspace();
        let t = text_of(&ed);
        assert_eq!(
            t, "helloworld\r\n",
            "Should remove both \\r and \\n: got {t:?}"
        );
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 5);
    }

    #[test]
    fn multi_cursor_delete_crlf_line_join() {
        let mut ed = Editor::test_with_text("hello\r\nworld\r\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.multi_cursor_delete();
        let t = text_of(&ed);
        assert_eq!(
            t, "helloworld\r\n",
            "Should remove both \\r and \\n: got {t:?}"
        );
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 5);
    }
}
