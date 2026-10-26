use crate::editor::{Editor, extract_opening_tag, is_markup_language};

#[cfg(test)]
#[path = "editor_batch_input_tests.rs"]
mod batch_input_tests;

pub(crate) const DENSE_EDIT_MAX_SPAN: usize = 65_536;
pub(crate) const DENSE_EDIT_MAX_BYTES: usize = 1024 * 1024;

/// Dense editing (column carets, occurrence selections, indentation) is much
/// cheaper as one local rope replacement than hundreds of separate tree
/// mutations. Sparse edits retain the shared tree between distant carets.
pub(crate) fn dense_edit_text<'a>(rope: &ropey::Rope, edits: impl ExactSizeIterator<Item = (usize, usize, &'a str)> + Clone) -> Option<(usize, usize, String)> {
    if edits.len() < 16 {
        return None;
    }
    let first = edits.clone().next()?.0;
    let last = edits.clone().last()?.1;
    if last.saturating_sub(first) > DENSE_EDIT_MAX_SPAN {
        return None;
    }
    let source = rope.slice(first..last);
    let inserted_bytes: usize = edits.clone().map(|(_, _, text)| text.len()).sum();
    if inserted_bytes > DENSE_EDIT_MAX_BYTES {
        return None;
    }
    let mut result = String::with_capacity(source.len_bytes().saturating_add(inserted_bytes));
    let mut chunks = source.chunks();
    let mut chunk = "";
    let mut chunk_chars = 0;
    // Consume a single chunk stream, counting each UTF-8 scalar only once.
    // Rope slicing per caret would replace mutation seeks with read seeks.
    let mut consume = |mut count: usize, output: Option<&mut String>| {
        let mut output = output;
        while count > 0 {
            if chunk_chars == 0 {
                chunk = chunks.next().expect("dense edit source covers every range");
                chunk_chars = chunk.chars().count();
            }
            let consumed = count.min(chunk_chars);
            let bytes = if consumed == chunk_chars {
                chunk.len()
            } else {
                chunk
                    .char_indices()
                    .nth(consumed)
                    .map_or(chunk.len(), |(byte, _)| byte)
            };
            if let Some(output) = output.as_deref_mut() {
                output.push_str(&chunk[..bytes]);
            }
            chunk = &chunk[bytes..];
            chunk_chars -= consumed;
            count -= consumed;
        }
    };
    let mut offset = first;
    for (start, end, text) in edits {
        consume(start - offset, Some(&mut result));
        consume(end - start, None);
        result.push_str(text);
        offset = end;
    }
    Some((first, last, result))
}

impl Editor {
    /// Perform linked editing: mirror an insert/delete at the matching tag.
    /// `offset_in_name` is where within the tag name the edit happened.
    /// `edit_kind`: 'i' for insert with char, 'd' for delete (backspace).
    pub(crate) fn apply_linked_tag_edit(
        &mut self,
        offset_in_name: usize,
        edit_kind: char,
        ch: char,
    ) {
        crate::editor_document::edit!(self, {
        // Save current tag info before modifying
        let Some((_name_start, _name_end, _is_closing)) = self.cursor_in_tag_name_range() else {
            return;
        };
        // We need the new tag name after the edit was already applied to find the matching tag
        // The current tag already has the edit applied, so find_matching_tag should work with the current state
        let matching = self.find_matching_tag();
        if let Some((t1l, t1cs, _t1ce, t2l, t2cs, _t2ce)) = matching {
            // Determine which tag is the "other" one (not the one we just edited)
            let (other_line, other_tag_start) =
                if t1l == self.cursor_line && t1cs <= self.cursor_col {
                    (t2l, t2cs)
                } else {
                    (t1l, t1cs)
                };
            // Find the other tag's name start
            let other_line_slice = self.rope.line(other_line);
            let other_inner_start = if other_line_slice.get_char(other_tag_start) == Some('<')
                && other_line_slice.get_char(other_tag_start + 1) == Some('/')
            {
                other_tag_start + 2
            } else {
                other_tag_start + 1
            };
            let target_col = other_inner_start + offset_in_name;
            let other_line_start = self.rope.line_to_char(other_line);
            match edit_kind {
                'i' => {
                    // Insert the same character
                    let idx = other_line_start + target_col;
                    if idx <= self.rope.len_chars() {
                        self.rope.insert_char(idx, ch);
                        self.mark_syntax_dirty_range(other_line, other_line);
                    }
                }
                'd' => {
                    // Delete one character
                    if target_col > 0 {
                        let idx = other_line_start + target_col - 1;
                        if idx < self.rope.len_chars() {
                            self.rope.remove(idx..=idx);
                            self.mark_syntax_dirty_range(other_line, other_line);
                        }
                    }
                }
                _ => {}
            }
        }
            })
    }

    pub(crate) fn insert_char(&mut self, c: char) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;

        // Surround selection with bracket/quote pairs (VS Code behavior)
        if let Some(closing) = match c {
            '(' => Some(')'),
            '[' => Some(']'),
            '{' => Some('}'),
            '"' => Some('"'),
            '\'' => Some('\''),
            '`' => Some('`'),
            _ => None,
        } {
            if let Some((start_idx, end_idx, _, _)) = self.clamped_selection_indices() {
                self.save_undo();
                self.rope.insert_char(end_idx, closing);
                self.rope.insert_char(start_idx, c);
                // Move cursor past the closing char; selection now wraps the pair
                let new_end = end_idx + 1; // +1 for the inserted opening char
                let new_line = self.rope.char_to_line(new_end);
                let new_col = new_end - self.rope.line_to_char(new_line);
                self.cursor_line = new_line;
                self.cursor_col = new_col;
                self.clear_selection();
                self.modified = true;
                self.ensure_visible();
                return;
            }
        }

        // Clamp cursor_col to line length to avoid inserting past line endings
        let max_col = self.line_len(self.cursor_line);
        if self.cursor_col > max_col {
            self.cursor_col = max_col;
        }
        let line_start = self.rope.line_to_char(self.cursor_line);
        let idx = line_start + self.cursor_col;
        let replacing = self.clamped_selection_indices().is_some();

        // Skip over auto-inserted closing char if the next char matches
        if !replacing && matches!(c, ')' | ']' | '}' | '"' | '\'' | '`')
            && idx < self.rope.len_chars()
            && self.rope.char(idx) == c
        {
            // Auto-dedent: if typing closing bracket on whitespace-only line, decrease indent
            let mut remove_count = 0;
            if matches!(c, ')' | ']' | '}') {
                let ts = self.tab_size.max(1);
                let before_len = self.cursor_col;
                let all_ws = before_len > 0
                    && self
                        .rope
                        .slice(line_start..line_start + before_len)
                        .chunks()
                        .all(|chunk| chunk.bytes().all(|ch| matches!(ch, b' ' | b'\t')));
                if all_ws {
                    // One indentation level may contain a tab, spaces, or both.
                    let mut cells = 0;
                    for (index, ch) in self.rope.chars_at(line_start).take(before_len).enumerate() {
                        cells += if ch == '\t' { ts - cells % ts } else { 1 };
                        if cells == ts {
                            remove_count = index + 1;
                            break;
                        }
                    }
                }
            }
            if remove_count > 0 {
                self.save_undo();
                self.rope.remove(line_start..line_start + remove_count);
                self.cursor_col -= remove_count;
                self.modified = true;
            } else {
                // Moving over an existing delimiter must not clear Redo or
                // create an empty edit/version change.
                self.break_undo_group();
            }
            self.cursor_col += 1;
            self.clear_selection();
            self.ensure_visible();
            return;
        }

        let (max_col, line_start, idx) = if replacing {
            self.save_undo();
            self.delete_selection_for_replacement();
            self.undo_group_active = true;
            self.last_undo_time = std::time::Instant::now();
            self.last_undo_char_class = crate::editor::CharClass::of(c);
            let line_start = self.rope.line_to_char(self.cursor_line);
            (self.line_len(self.cursor_line), line_start, line_start + self.cursor_col)
        } else {
            self.maybe_save_undo_char(c);
            self.clear_selection();
            (max_col, line_start, idx)
        };

        // Capture linked editing info before the edit for word chars in tag names.
        let linked_info = if c.is_alphanumeric() || c == '_' || c == '-' {
            self.cursor_in_tag_name_range()
                .map(|(name_start, _name_end, _is_closing)| self.cursor_col - name_start)
        } else {
            None
        };

        // Auto-close brackets/quotes
        let close = match c {
            '(' => Some(')'),
            '[' => Some(']'),
            '{' => Some('}'),
            '"' => Some('"'),
            '\'' => Some('\''),
            '`' => Some('`'),
            _ => None,
        };

        // Overwrite mode: replace character at cursor instead of inserting
        if !replacing && self.overwrite_mode && close.is_none() && self.cursor_col < max_col {
            self.rope.remove(idx..=idx);
        }

        // Decide from the original neighbors and insert an automatic pair in
        // one rope mutation. Looking up neighbors after the first insertion
        // and mutating the same leaf twice duplicated ordinary typing work.
        let auto_close = close.filter(|_| {
            let next_char = self.rope.get_char(idx);
            let mut should_close =
                !matches!(next_char, Some(nc) if nc.is_alphanumeric() || nc == '_');
            // For quotes, also suppress if preceding char is alphanumeric (e.g. don't)
            if should_close && matches!(c, '"' | '\'' | '`') {
                if self.cursor_col >= 1 {
                    let prev_char = self.rope.char(idx - 1);
                    if prev_char.is_alphanumeric() || prev_char == '_' {
                        should_close = false;
                    }
                }
            }
            should_close
        });
        if let Some(closing) = auto_close {
            let pair = [c as u8, closing as u8]; // All automatic delimiters are ASCII.
            self.rope.insert(idx, std::str::from_utf8(&pair).unwrap());
        } else {
            self.rope.insert_char(idx, c);
        }
        self.cursor_col += 1;

        // Auto-close HTML/XML tags: typing '>' after <tagname inserts </tagname>
        if c == '>' && is_markup_language(self.language_name()) {
            let line = self.rope.line(self.cursor_line);
            if let Some((offset, _)) = line
                .chars_at(self.cursor_col)
                .reversed()
                .enumerate()
                .find(|&(_, ch)| ch == '<')
            {
                let start = self.cursor_col - offset - 1;
                let tag = line.slice(start..self.cursor_col);
                let candidate: std::borrow::Cow<str> = tag
                    .as_str()
                    .map(std::borrow::Cow::Borrowed)
                    .unwrap_or_else(|| std::borrow::Cow::Owned(tag.to_string()));
                if let Some(tag_name) = extract_opening_tag(&candidate) {
                    let close_tag = format!("</{tag_name}>");
                    let insert_pos = line_start + self.cursor_col;
                    self.rope.insert(insert_pos, &close_tag);
                }
            }
        }

        // Linked editing: mirror insert to matching HTML/XML tag
        if let Some(offset) = linked_info {
            self.apply_linked_tag_edit(offset, 'i', c);
        }

        self.modified = true;
        self.ensure_visible();
            })
    }

    pub(crate) fn insert_newline(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        self.save_undo();
        self.delete_selection_for_replacement();
        self.cursor_col = self.cursor_col.min(self.line_len(self.cursor_line));
        let idx = self.rope.line_to_char(self.cursor_line) + self.cursor_col;
        if self.cursor_col == 0 {
            // No indentation belongs before a split at the start of a line.
            // In particular, do not read/copy a generated line's entire indent.
            self.rope.insert(idx, "\n");
            self.cursor_line += 1;
            self.modified = true;
            self.shift_bookmarks(self.cursor_line, 1);
            self.clear_stale_folds();
            self.ensure_visible();
            return;
        }
        let current_line = self.rope.line(self.cursor_line);

        let char_before = self
            .cursor_col
            .checked_sub(1)
            .and_then(|col| current_line.get_char(col));
        let char_after = current_line.get_char(self.cursor_col);
        let between_brackets = matches!(
            (char_before, char_after),
            (Some('{'), Some('}')) | (Some('('), Some(')')) | (Some('['), Some(']'))
        );
        let mut tail = current_line.chars_at(current_line.len_chars()).reversed();
        let mut trimmed_len = current_line.len_chars();
        let last = loop {
            match tail.next() {
                Some('\n' | '\r' | ' ') => trimmed_len -= 1,
                other => break other,
            }
        };
        let ends_with_open = matches!(last, Some('{' | '(' | '['));
        let lang = self.language_name();
        let ends_with_colon = matches!(lang, "Python" | "YAML") && last == Some(':');
        let ends_with_arrow = matches!(
            lang,
            "Rust" | "JavaScript" | "TypeScript" | "JavaScript React" | "TypeScript React"
        ) && last == Some('>')
            && tail.next() == Some('=');
        let open_bracket_before_cursor = ends_with_open
            && !between_brackets
            && char_before
                .is_some_and(|c| matches!(c, '{' | '(' | '[') || self.cursor_col >= trimmed_len);
        let lang_indent_trigger =
            (ends_with_colon || ends_with_arrow) && self.cursor_col >= trimmed_len;
        let extra_indent = between_brackets || open_bracket_before_cursor || lang_indent_trigger;
        // Splits inside indentation carry the unused suffix to the next line.
        // Only inspect the prefix we actually need, without a temporary String.
        let limit = if extra_indent { current_line.len_chars() } else { self.cursor_col };
        // A closing delimiter does not imply another dedent: its line is
        // already aligned, and `)` may simply end an if header or a call.
        let indent_chars = current_line.chars().take(limit)
            .take_while(|&c| c.is_whitespace() && c != '\n' && c != '\r').count();
        let indent = current_line.slice(..indent_chars);
        let one_indent = if self.use_tabs {
            "\t"
        } else {
            &crate::helpers::spaces(self.tab_size)
        };

        // Construct the complete insertion before editing the rope. A bracket
        // expansion used to perform four separate tree edits and line seeks.
        let mut insertion = String::with_capacity(2 + indent.len_bytes() * 2 + one_indent.len());
        insertion.push('\n');
        for chunk in indent.chunks() { insertion.push_str(chunk); }
        let (lines_inserted, new_col) =
            if extra_indent {
                insertion.push_str(one_indent);
                if between_brackets {
                    insertion.push('\n');
                    for chunk in indent.chunks() { insertion.push_str(chunk); }
                }
                (
                    if between_brackets { 2 } else { 1 },
                    indent_chars + one_indent.len(),
                )
            } else {
                (1, indent_chars)
            };
        self.rope.insert(idx, &insertion);
        self.cursor_line += 1;
        self.cursor_col = new_col;
        self.modified = true;
        self.shift_bookmarks(self.cursor_line, lines_inserted);
        self.clear_stale_folds();
        self.ensure_visible();
            })
    }

    pub(crate) fn insert_tab(&mut self) {
        crate::editor_document::edit!(self, {
        if self.use_tabs {
            self.insert_char('\t');
            return;
        }
        // A soft tab is one logical edit. Repeating insert_char invalidated
        // syntax/wrap state and adjusted the viewport for every single space.
        self.desired_col = None;
        let replacing = self.clamped_selection_indices().is_some();
        if replacing {
            self.save_undo();
            self.delete_selection_for_replacement();
        } else {
            self.maybe_save_undo_char(' ');
            self.clear_selection();
        }
        let size = self.tab_size.max(1);
        let spaces = size - (self.cell_at_source_column(self.cursor_line, 0, self.cursor_col) % size);
        let len = self.line_len(self.cursor_line);
        self.cursor_col = self.cursor_col.min(len);
        let index = self.rope.line_to_char(self.cursor_line) + self.cursor_col;
        if self.overwrite_mode && !replacing {
            let removed = spaces.min(len - self.cursor_col);
            if removed > 0 { self.rope.remove(index..index + removed); }
        }
        let text = crate::helpers::spaces(spaces);
        self.rope.insert(index, &text);
        self.cursor_col += spaces;
        self.modified = true;
        self.ensure_visible();
            })
    }

    pub(crate) fn insert_line_above(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        self.save_undo();
        let current_line = self.rope.line(self.cursor_line);
        let mut chars = current_line.chars().peekable();
        let mut insertion = String::new();
        let mut indent_chars = 0;
        while chars
            .peek()
            .is_some_and(|&c| c.is_whitespace() && c != '\n' && c != '\r')
        {
            insertion.push(chars.next().unwrap());
            indent_chars += 1;
        }
        // The new line belongs inside a block when inserted before its closer.
        if matches!(chars.find(|c| !c.is_whitespace()), Some('}' | ')' | ']')) {
            let one_indent = if self.use_tabs {
                "\t"
            } else {
                &crate::helpers::spaces(self.tab_size)
            };
            insertion.push_str(one_indent);
            indent_chars += one_indent.len();
        }
        insertion.push('\n');
        let line_start = self.rope.line_to_char(self.cursor_line);
        self.rope.insert(line_start, &insertion);
        self.cursor_col = indent_chars;
        self.modified = true;
        self.shift_bookmarks(self.cursor_line, 1);
        self.clear_stale_folds();
        self.ensure_visible();
            })
    }

    pub(crate) fn insert_line_below(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        self.save_undo();
        let current_line = self.rope.line(self.cursor_line);
        let mut insertion = String::new();
        let mut indent_chars = 0;
        for ch in current_line
            .chars()
            .take_while(|&c| c.is_whitespace() && c != '\n' && c != '\r')
        {
            insertion.push(ch);
            indent_chars += 1;
        }
        let last = current_line
            .chars_at(current_line.len_chars())
            .reversed()
            .find(|&c| !matches!(c, '\n' | '\r' | ' '));
        if matches!(last, Some('{' | '(' | '[')) {
            let one_indent = if self.use_tabs {
                "\t"
            } else {
                &crate::helpers::spaces(self.tab_size)
            };
            insertion.push_str(one_indent);
            indent_chars += one_indent.len();
        }
        let line_end = self.rope.line_to_char(self.cursor_line) + current_line.len_chars();
        if current_line.get_char(current_line.len_chars().saturating_sub(1)) == Some('\n') {
            insertion.push('\n');
        } else {
            insertion.insert(0, '\n');
        }
        self.rope.insert(line_end, &insertion);
        self.cursor_line += 1;
        self.cursor_col = indent_chars;
        self.modified = true;
        self.shift_bookmarks(self.cursor_line, 1);
        self.clear_stale_folds();
        self.ensure_visible();
            })
    }

    pub(crate) fn backspace(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        let length = self.line_len(self.cursor_line);
        if self.cursor_col > length {
            self.cursor_col = length;
            self.ensure_visible();
            return;
        }
        if self.cursor_line == 0 && self.cursor_col == 0 {
            self.ensure_visible();
            return;
        }
        self.maybe_save_undo();
        // Capture linked editing info before the edit.
        let linked_info = if self.cursor_col > 0 {
            self.cursor_in_tag_name_range()
                .map(|(name_start, _name_end, _is_closing)| self.cursor_col - name_start)
        } else {
            None
        };
        if self.cursor_col > 0 {
            let line_start = self.rope.line_to_char(self.cursor_line);

            // Smart backspace: if only spaces before cursor, remove to previous tab stop
            // Most backspaces are within code. Check the preceding character
            // first, so ordinary typing does not rescan leading indentation.
            let ch_before = self.rope.char(line_start + self.cursor_col - 1);
            let all_spaces = ch_before == ' '
                && self
                    .rope
                    .slice(line_start..line_start + self.cursor_col)
                    .chunks()
                    .all(|chunk| chunk.bytes().all(|ch| ch == b' '));
            if all_spaces {
                let ts = self.tab_size.max(1);
                let spaces_to_remove = if self.cursor_col.is_multiple_of(ts) {
                    ts
                } else {
                    self.cursor_col % ts
                };
                let remove_start = line_start + self.cursor_col - spaces_to_remove;
                let remove_end = line_start + self.cursor_col;
                self.rope.remove(remove_start..remove_end);
                self.cursor_col -= spaces_to_remove;
            } else {
                // Check for auto-close bracket pair deletion
                let closing = match ch_before {
                    '(' => Some(')'), '[' => Some(']'), '{' => Some('}'),
                    '"' | '\'' | '`' => Some(ch_before), _ => None,
                };
                let is_pair = closing.is_some_and(|closing|
                    self.rope.get_char(line_start + self.cursor_col) == Some(closing));
                let idx = line_start + self.cursor_col - 1;
                if is_pair {
                    // Delete both characters
                    self.rope.remove(idx..idx + 2);
                } else {
                    self.rope.remove(idx..=idx);
                }
                self.cursor_col -= 1;
                // Linked editing: mirror backspace to matching HTML/XML tag
                if let Some(offset) = linked_info {
                    if offset > 0 {
                        self.apply_linked_tag_edit(offset, 'd', '\0');
                    }
                }
            }
            self.modified = true;
        } else if self.cursor_line > 0 {
            let prev_line_len = self.line_len(self.cursor_line - 1);
            let line_start = self.rope.line_to_char(self.cursor_line);
            // Remove line ending: handle both \r\n (CRLF) and \n (LF)
            let remove_start = if line_start >= 2
                && self.rope.char(line_start - 2) == '\r'
                && self.rope.char(line_start - 1) == '\n'
            {
                line_start - 2 // CRLF: remove both \r and \n
            } else {
                line_start - 1 // LF: remove just \n
            };
            self.rope.remove(remove_start..line_start);
            self.shift_bookmarks(self.cursor_line, -1);
            self.cursor_line -= 1;
            self.cursor_col = prev_line_len;
            self.modified = true;
            self.clear_stale_folds();
        }
        self.ensure_visible();
            })
    }

    pub(crate) fn delete_word_left(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        if self.clamped_selection_indices().is_some() {
            self.discard_selection();
            return;
        }
        self.clear_selection();
        let length = self.line_len(self.cursor_line);
        if self.cursor_col > length {
            self.cursor_col = length;
            self.ensure_visible();
            return;
        }
        self.break_undo_group();
        if self.cursor_col == 0 {
            self.backspace();
            self.break_undo_group();
            return;
        }
        self.save_undo();
        let line_start = self.rope.line_to_char(self.cursor_line);
        let start_col = self.cursor_col;
        let (_, end_col) = self.word_left_position(self.cursor_line, self.cursor_col);
        let from = line_start + end_col;
        let to = line_start + start_col;
        if from < to {
            self.rope.remove(from..to);
            self.cursor_col = end_col;
            self.modified = true;
        }
        self.ensure_visible();
            })
    }

    pub(crate) fn delete_word_right(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        if self.clamped_selection_indices().is_some() {
            self.discard_selection();
            return;
        }
        self.clear_selection();
        self.break_undo_group();
        let line_len = self.current_line_len();
        if self.cursor_col >= line_len {
            self.delete_char();
            self.break_undo_group();
            return;
        }
        self.save_undo();
        let line_start = self.rope.line_to_char(self.cursor_line);
        let start_col = self.cursor_col;
        let (_, col) = self.word_right_position(self.cursor_line, start_col);
        let col = col
            + self
                .rope
                .line(self.cursor_line)
                .chars_at(col)
                .take(line_len - col)
                .take_while(|c| c.is_whitespace())
                .count();
        let from = line_start + start_col;
        let to = line_start + col;
        if from < to {
            self.rope.remove(from..to);
            self.modified = true;
        }
        self.ensure_visible();
            })
    }

    pub(crate) fn delete_char(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        let length = self.line_len(self.cursor_line);
        if self.cursor_col > length {
            self.cursor_col = length;
            self.ensure_visible();
            return;
        }
        let line_start = self.rope.line_to_char(self.cursor_line);
        let idx = line_start + self.cursor_col;
        if idx >= self.rope.len_chars() {
            self.ensure_visible();
            return;
        }
        self.maybe_save_undo();
        if idx < self.rope.len_chars() {
            let ch = self.rope.char(idx);
            let is_newline = ch == '\n' || ch == '\r';
            // Handle CRLF: remove both \r and \n together
            let remove_end =
                if ch == '\r' && idx + 1 < self.rope.len_chars() && self.rope.char(idx + 1) == '\n'
                {
                    idx + 2 // CRLF: remove both
                } else {
                    idx + 1 // single char
                };
            self.rope.remove(idx..remove_end);
            self.modified = true;
            if is_newline {
                self.shift_bookmarks(self.cursor_line + 1, -1);
                self.clear_stale_folds();
            }
            let line_len = self.line_len(self.cursor_line);
            if self.cursor_col > line_len {
                self.cursor_col = line_len;
            }
        }
        self.ensure_visible();
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    #[test]
    fn linked_tag_mirror_invalidates_a_distant_warm_wrap_line() {
        let source = format!("<span>\n{}</span>", "text\n".repeat(80));
        let mut editor = Editor::test_with_text(&source);
        editor.language_override = Some("HTML".into());
        editor.word_wrap = true;
        editor.viewport_width = 7;
        editor.cursor_col = 1;
        let last = editor.line_count() - 1;
        assert!(editor.ensure_row_offsets(7));
        let previous = editor.wrap_ranges_for_line(last, 7);
        editor.save_undo();
        editor.apply_linked_tag_edit(0, 'i', 'x');
        assert_eq!(editor.rope.line(last).to_string(), "</xspan>");
        let fresh = Editor::build_wrap_row_offsets(&editor.rope, 7, editor.tab_size, &[], || false)
            .unwrap();
        assert_eq!(editor.wrapped_total_rows(7), *fresh.last().unwrap());
        let current = editor.wrap_ranges_for_line(last, 7);
        assert!(!std::rc::Rc::ptr_eq(&previous, &current));
        assert_eq!(current.len(), 2);
    }

    // ── insert_char ──────────────────────────────────────────────

    #[test]
    fn insert_char_at_start() {
        let mut ed = Editor::test_with_text("hello");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.insert_char('X');
        assert_eq!(ed.rope.to_string(), "Xhello");
        assert_eq!(ed.cursor_col, 1);
        assert_eq!(ed.cursor_line, 0);
    }

    #[test]
    fn insert_char_mid_line() {
        let mut ed = Editor::test_with_text("hllo");
        ed.cursor_line = 0;
        ed.cursor_col = 1;
        ed.insert_char('e');
        assert_eq!(ed.rope.to_string(), "hello");
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn insert_char_at_end() {
        let mut ed = Editor::test_with_text("hi");
        ed.cursor_line = 0;
        ed.cursor_col = 2;
        ed.insert_char('!');
        assert_eq!(ed.rope.to_string(), "hi!");
        assert_eq!(ed.cursor_col, 3);
    }

    // ── insert_newline ───────────────────────────────────────────

    #[test]
    fn insert_newline_splits_line() {
        let mut ed = Editor::test_with_text("hello world");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.insert_newline();
        assert_eq!(ed.rope.line(0).to_string(), "hello\n");
        assert_eq!(ed.rope.line(1).to_string(), " world");
        assert_eq!(ed.cursor_line, 1);
    }

    #[test]
    fn insert_newline_preserves_indent() {
        let mut ed = Editor::test_with_text("    code");
        ed.cursor_line = 0;
        ed.cursor_col = 8;
        ed.insert_newline();
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 4);
        let line1 = ed.rope.line(1).to_string();
        assert!(
            line1.starts_with("    "),
            "expected 4-space indent, got: {:?}",
            line1
        );
    }

    #[test]
    fn insert_newline_at_line_start() {
        let mut ed = Editor::test_with_text("hello");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.insert_newline();
        assert_eq!(ed.rope.line(0).to_string(), "\n");
        assert_eq!(ed.rope.line(1).to_string(), "hello");
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn insert_newline_before_close_brace_keeps_indent() {
        // Bug fix: cursor right before `}` at indent boundary should NOT lose indent
        let mut ed = Editor::test_with_text("    }\n");
        ed.cursor_line = 0;
        ed.cursor_col = 4; // right before `}`
        ed.insert_newline();
        // The `}` should keep its indent on the new line
        let line1 = ed.rope.line(1).to_string();
        assert!(
            line1.starts_with("    }"),
            "closing brace must keep indent, got: {:?}",
            line1
        );
    }

    #[test]
    fn insert_newline_after_close_brace_keeps_existing_indent() {
        // The closing brace is already aligned with its enclosing block.
        let mut ed = Editor::test_with_text("        }\n");
        ed.tab_size = 4;
        ed.cursor_line = 0;
        ed.cursor_col = 9; // after 8-space indent + `}`
        ed.insert_newline();
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 8);
        assert_eq!(ed.rope.to_string(), "        }\n        \n");
    }

    #[test]
    fn insert_newline_preserves_indent_after_closed_expressions() {
        for statement in ["if (something)", "work()", "values[index]", "}"] {
            for indent in ["    ", "        ", "\t\t\t\t", "\t  "] {
                let source = format!("{indent}{statement}");
                let mut ed = Editor::test_with_text(&source);
                ed.cursor_col = source.chars().count();
                ed.insert_newline();
                let expected = format!("{source}\n{indent}");
                assert_eq!(ed.rope.to_string(), expected, "{source:?}");
                assert_eq!(
                    (ed.cursor_line, ed.cursor_col),
                    (1, indent.chars().count())
                );
                ed.undo();
                assert_eq!(ed.rope.to_string(), source);
                ed.redo();
                assert_eq!(ed.rope.to_string(), expected);
            }
        }
    }

    #[test]
    fn allman_block_enter_keeps_spaces_tabs_and_indent_widths() {
        for (use_tabs, tab_size) in [
            (false, 2),
            (false, 3),
            (false, 4),
            (false, 8),
            (true, 2),
            (true, 4),
            (true, 8),
        ] {
            let unit = if use_tabs {
                "\t".into()
            } else {
                " ".repeat(tab_size)
            };
            let base = unit.repeat(4);
            let body = format!("{base}{unit}");
            let header = format!("{base}if (something)");
            let mut ed = Editor::test_with_text(&header);
            ed.language_override = Some("C".into());
            ed.use_tabs = use_tabs;
            ed.tab_size = tab_size;
            ed.cursor_col = header.chars().count();

            ed.insert_newline();
            assert_eq!(ed.rope.to_string(), format!("{header}\n{base}"));
            assert_eq!(
                (ed.cursor_line, ed.cursor_col),
                (1, base.chars().count())
            );
            ed.insert_char('{');
            ed.insert_newline();
            assert_eq!(
                ed.rope.to_string(),
                format!("{header}\n{base}{{\n{body}\n{base}}}")
            );
            assert_eq!(
                (ed.cursor_line, ed.cursor_col),
                (2, body.chars().count())
            );
            for ch in "work();".chars() {
                ed.insert_char(ch);
            }
            ed.insert_newline();
            assert_eq!(
                ed.rope.to_string(),
                format!("{header}\n{base}{{\n{body}work();\n{body}\n{base}}}")
            );
            assert_eq!(
                (ed.cursor_line, ed.cursor_col),
                (3, body.chars().count())
            );

            ed.cursor_line = 4;
            ed.cursor_col = ed.line_len(4);
            ed.insert_newline();
            assert_eq!(
                ed.rope.to_string(),
                format!("{header}\n{base}{{\n{body}work();\n{body}\n{base}}}\n{base}")
            );
            assert_eq!(
                (ed.cursor_line, ed.cursor_col),
                (5, base.chars().count())
            );
        }
    }

    #[test]
    fn insert_newline_code_then_close_brace_keeps_indent() {
        // `code();}` — splitting before `}` should keep same indent level
        let mut ed = Editor::test_with_text("        code();}\n");
        ed.tab_size = 4;
        ed.cursor_line = 0;
        ed.cursor_col = 15; // after `code();`, before `}`
        ed.insert_newline();
        let line1 = ed.rope.line(1).to_string();
        assert!(
            line1.starts_with("        }"),
            "closing brace should keep same indent, got: {:?}",
            line1
        );
    }

    #[test]
    fn insert_newline_between_brackets_expands() {
        let mut ed = Editor::test_with_text("    {}\n");
        ed.tab_size = 4;
        ed.cursor_line = 0;
        ed.cursor_col = 5; // between { and }
        ed.insert_newline();
        // Should create 3 lines: `    {`, `        ` (cursor here), `    }`
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 8); // extra indent
        let line2 = ed.rope.line(2).to_string();
        assert!(
            line2.starts_with("    }"),
            "closing brace on own line with base indent, got: {:?}",
            line2
        );
    }

    #[test]
    fn insert_newline_before_close_brace_col0_no_indent() {
        // Line `}` with no indent, cursor at col 0
        let mut ed = Editor::test_with_text("}\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.insert_newline();
        // `}` should stay as-is on line 1
        let line1 = ed.rope.line(1).to_string();
        assert!(
            line1.starts_with("}"),
            "brace should remain, got: {:?}",
            line1
        );
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn insert_newline_at_end_of_open_brace() {
        let mut ed = Editor::test_with_text("    if (true) {\n");
        ed.tab_size = 4;
        ed.cursor_line = 0;
        ed.cursor_col = 15; // after `{`
        ed.insert_newline();
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 8); // extra indent
    }

    // ── insert_tab ───────────────────────────────────────────────

    #[test]
    fn insert_tab_at_col_zero() {
        let mut ed = Editor::test_with_text("x");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.insert_tab();
        assert_eq!(ed.rope.to_string(), "    x");
        assert_eq!(ed.cursor_col, 4);
    }

    #[test]
    fn insert_tab_alignment() {
        let mut ed = Editor::test_with_text(" x");
        ed.cursor_line = 0;
        ed.cursor_col = 1;
        ed.insert_tab();
        // col 1 → next tab stop is col 4 → insert 3 spaces
        assert_eq!(ed.rope.to_string(), "    x");
        assert_eq!(ed.cursor_col, 4);
    }

    // ── insert_line_above / insert_line_below ────────────────────

    #[test]
    fn insert_line_above() {
        let mut ed = Editor::test_with_text("first\nsecond\n");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.insert_line_above();
        assert_eq!(ed.rope.len_lines(), 4);
        assert_eq!(ed.rope.line(1).to_string(), "\n");
        assert_eq!(ed.cursor_line, 1);
    }

    #[test]
    fn insert_line_below() {
        let mut ed = Editor::test_with_text("first\nsecond\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.insert_line_below();
        assert_eq!(ed.rope.len_lines(), 4);
        assert_eq!(ed.cursor_line, 1);
    }

    #[test]
    fn insert_line_below_after_open_brace() {
        let mut ed = Editor::test_with_text("    if (true) {\n    }\n");
        ed.cursor_line = 0;
        ed.cursor_col = 15;
        ed.insert_line_below();
        let new_line = ed.rope.line(1).to_string();
        assert!(
            new_line.starts_with("        "),
            "Should add extra indent after {{: {:?}",
            new_line
        );
        assert_eq!(ed.cursor_col, 8);
    }

    #[test]
    fn insert_line_above_before_close_brace() {
        let mut ed = Editor::test_with_text("    }\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.insert_line_above();
        let new_line = ed.rope.line(0).to_string();
        assert!(
            new_line.starts_with("        "),
            "Should add extra indent above }}: {:?}",
            new_line
        );
        assert_eq!(ed.cursor_col, 8);
    }

    // ── backspace ────────────────────────────────────────────────

    #[test]
    fn backspace_mid_line() {
        let mut ed = Editor::test_with_text("abc");
        ed.cursor_line = 0;
        ed.cursor_col = 2;
        ed.backspace();
        assert_eq!(ed.rope.to_string(), "ac");
        assert_eq!(ed.cursor_col, 1);
    }

    #[test]
    fn backspace_at_start_of_first_line() {
        let mut ed = Editor::test_with_text("abc");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.backspace();
        assert_eq!(ed.rope.to_string(), "abc");
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn backspace_joins_lines() {
        let mut ed = Editor::test_with_text("hello\nworld");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.backspace();
        assert_eq!(ed.rope.to_string(), "helloworld");
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 5);
    }

    // ── delete_char ──────────────────────────────────────────────

    #[test]
    fn delete_char_mid_line() {
        let mut ed = Editor::test_with_text("abc");
        ed.cursor_line = 0;
        ed.cursor_col = 1;
        ed.delete_char();
        assert_eq!(ed.rope.to_string(), "ac");
        assert_eq!(ed.cursor_col, 1);
    }

    #[test]
    fn delete_char_at_end_joins_next_line() {
        let mut ed = Editor::test_with_text("ab\ncd");
        ed.cursor_line = 0;
        ed.cursor_col = 2;
        ed.delete_char();
        assert_eq!(ed.rope.to_string(), "abcd");
        assert_eq!(ed.cursor_line, 0);
    }

    // ── delete_word_left / delete_word_right ─────────────────────

    #[test]
    fn delete_word_left_single_word() {
        let mut ed = Editor::test_with_text("hello world");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.delete_word_left();
        assert_eq!(ed.rope.to_string(), " world");
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn delete_word_right_single_word() {
        let mut ed = Editor::test_with_text("hello world");
        ed.cursor_line = 0;
        ed.cursor_col = 6;
        ed.delete_word_right();
        assert_eq!(ed.rope.to_string(), "hello ");
        assert_eq!(ed.cursor_col, 6);
    }

    // ── duplicate_line / duplicate_line_up ────────────────────────

    #[test]
    fn duplicate_line_down() {
        let mut ed = Editor::test_with_text("aaa\nbbb\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.duplicate_line();
        assert_eq!(ed.rope.line(0).to_string(), "aaa\n");
        assert_eq!(ed.rope.line(1).to_string(), "aaa\n");
        assert_eq!(ed.rope.line(2).to_string(), "bbb\n");
        assert_eq!(ed.cursor_line, 1);
    }

    #[test]
    fn duplicate_line_up() {
        let mut ed = Editor::test_with_text("aaa\nbbb\n");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.duplicate_line_up();
        // bbb is duplicated above: original should stay, copy inserted above
        assert_eq!(ed.rope.line(1).to_string(), "bbb\n");
        assert_eq!(ed.cursor_line, 1);
    }

    // ── dedent_line / indent_lines ───────────────────────────────

    #[test]
    fn dedent_removes_spaces() {
        let mut ed = Editor::test_with_text("        code");
        ed.cursor_line = 0;
        ed.cursor_col = 8;
        ed.dedent_line();
        assert_eq!(ed.rope.to_string(), "    code");
        assert_eq!(ed.cursor_col, 4);
    }

    #[test]
    fn dedent_partial_indent() {
        let mut ed = Editor::test_with_text("  code");
        ed.cursor_line = 0;
        ed.cursor_col = 2;
        ed.dedent_line();
        assert_eq!(ed.rope.to_string(), "code");
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn indent_adds_spaces() {
        let mut ed = Editor::test_with_text("code");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.indent_lines();
        assert_eq!(ed.rope.to_string(), "    code");
        assert_eq!(ed.cursor_col, 4);
    }

    // ── join_lines ───────────────────────────────────────────────

    #[test]
    fn join_lines_adds_space() {
        let mut ed = Editor::test_with_text("hello\n    world");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.join_lines();
        assert_eq!(ed.rope.to_string(), "hello world");
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 6);
    }

    #[test]
    fn join_lines_empty_next_preserves_the_final_line_ending() {
        let mut ed = Editor::test_with_text("hello\n\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.join_lines();
        let result = ed.rope.to_string();
        // Joining two lines removes one separator, not both.
        assert_eq!(result, "hello\n");
    }

    // ── move_line_up / move_line_down ────────────────────────────

    #[test]
    fn move_line_up() {
        let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.move_line_up();
        assert_eq!(ed.rope.line(0).to_string(), "bbb\n");
        assert_eq!(ed.rope.line(1).to_string(), "aaa\n");
        assert_eq!(ed.cursor_line, 0);
    }

    #[test]
    fn move_line_up_already_at_top() {
        let mut ed = Editor::test_with_text("aaa\nbbb\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.move_line_up();
        assert_eq!(ed.rope.line(0).to_string(), "aaa\n");
        assert_eq!(ed.cursor_line, 0);
    }

    #[test]
    fn move_line_down() {
        let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.move_line_down();
        assert_eq!(ed.rope.line(0).to_string(), "bbb\n");
        assert_eq!(ed.rope.line(1).to_string(), "aaa\n");
        assert_eq!(ed.cursor_line, 1);
    }

    // ── delete_line ──────────────────────────────────────────────

    #[test]
    fn delete_line_middle() {
        let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.delete_line();
        assert_eq!(ed.rope.to_string(), "aaa\nccc\n");
        assert_eq!(ed.cursor_line, 1);
    }

    #[test]
    fn delete_line_only_line() {
        let mut ed = Editor::test_with_text("only");
        ed.cursor_line = 0;
        ed.cursor_col = 2;
        ed.delete_line();
        let result = ed.rope.to_string();
        assert!(result.is_empty() || result == "\n", "got: {:?}", result);
    }

    // ── delete_to_line_start / delete_to_line_end ────────────────

    #[test]
    fn delete_to_line_start() {
        let mut ed = Editor::test_with_text("hello world");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.delete_to_line_start();
        assert_eq!(ed.rope.to_string(), " world");
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn delete_to_line_end() {
        let mut ed = Editor::test_with_text("hello world");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.delete_to_line_end();
        assert_eq!(ed.rope.to_string(), "hello");
        assert_eq!(ed.cursor_col, 5);
    }

    // ── delete_selection ─────────────────────────────────────────

    #[test]
    fn delete_selection_returns_deleted_text() {
        let mut ed = Editor::test_with_text("hello world");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.selection_anchor = Some((0, 0));
        let deleted = ed.delete_selection();
        assert_eq!(deleted, Some("hello".to_string()));
        assert_eq!(ed.rope.to_string(), " world");
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn delete_selection_none_when_no_selection() {
        let mut ed = Editor::test_with_text("hello");
        ed.cursor_line = 0;
        ed.cursor_col = 2;
        let deleted = ed.delete_selection();
        assert_eq!(deleted, None);
        assert_eq!(ed.rope.to_string(), "hello");
    }

    // ── wrap_selection ───────────────────────────────────────────

    #[test]
    fn wrap_selection_with_parens() {
        let mut ed = Editor::test_with_text("hello");
        ed.selection_anchor = Some((0, 0));
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.wrap_selection('(', ')');
        assert_eq!(ed.rope.to_string(), "(hello)");
    }

    #[test]
    fn wrap_selection_with_brackets() {
        let mut ed = Editor::test_with_text("abc");
        ed.selection_anchor = Some((0, 1));
        ed.cursor_line = 0;
        ed.cursor_col = 2;
        ed.wrap_selection('[', ']');
        assert_eq!(ed.rope.to_string(), "a[b]c");
    }

    // ── paste_with_text ──────────────────────────────────────────

    #[test]
    fn paste_text_at_cursor() {
        let mut ed = Editor::test_with_text("hd");
        ed.cursor_line = 0;
        ed.cursor_col = 1;
        ed.paste_with_text("ello worl");
        assert_eq!(ed.rope.to_string(), "hello world");
        assert_eq!(ed.cursor_col, 10);
    }

    #[test]
    fn paste_multiline_text() {
        let mut ed = Editor::test_with_text("AB");
        ed.cursor_line = 0;
        ed.cursor_col = 1;
        ed.paste_with_text("x\ny");
        // Should insert "x\ny" between A and B
        assert_eq!(ed.rope.line(0).to_string(), "Ax\n");
        assert!(ed.rope.to_string().contains("yB"));
    }

    // ── adjust_char_idx_for_deletions ────────────────────────────

    #[test]
    fn adjust_idx_no_deletions() {
        let result = Editor::adjust_char_idx_for_deletions(10, &[]);
        assert_eq!(result, 10);
    }

    #[test]
    fn adjust_idx_with_prior_deletion() {
        // Deletion of 3 chars (indices 2..5) before our index 10
        let ranges = vec![(2, 5, 0, 0, 0)];
        let result = Editor::adjust_char_idx_for_deletions(10, &ranges);
        assert_eq!(result, 7);
    }

    #[test]
    fn adjust_idx_deletion_after_is_noop() {
        // Deletion at index 15..20, our index is 10 → no change
        let ranges = vec![(15, 20, 0, 0, 0)];
        let result = Editor::adjust_char_idx_for_deletions(10, &ranges);
        assert_eq!(result, 10);
    }

    // ── Additional edge cases ────────────────────────────────────

    #[test]
    fn backspace_smart_tab_unindent() {
        let mut ed = Editor::test_with_text("        x");
        ed.cursor_line = 0;
        ed.cursor_col = 8;
        ed.backspace();
        // smart backspace removes to previous tab stop (8→4)
        assert_eq!(ed.rope.to_string(), "    x");
        assert_eq!(ed.cursor_col, 4);
    }

    #[test]
    fn insert_tab_with_real_tabs() {
        let mut ed = Editor::new();
        ed.use_tabs = true;
        ed.rope = Rope::from_str("x");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.insert_tab();
        assert_eq!(ed.rope.to_string(), "\tx");
        assert_eq!(ed.cursor_col, 1);
    }

    #[test]
    fn delete_to_line_start_at_col_zero() {
        let mut ed = Editor::test_with_text("hello");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.delete_to_line_start();
        assert_eq!(ed.rope.to_string(), "hello");
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn delete_to_line_end_at_end() {
        let mut ed = Editor::test_with_text("hello");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.delete_to_line_end();
        assert_eq!(ed.rope.to_string(), "hello");
    }

    #[test]
    fn move_line_down_at_bottom_is_noop() {
        let mut ed = Editor::test_with_text("only\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.move_line_down();
        // Should be no-op since next line is the trailing empty line
        assert_eq!(ed.rope.line(0).to_string(), "only\n");
        assert_eq!(ed.cursor_line, 0);
    }
}
