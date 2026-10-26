use crate::editor::{is_markup_language, Editor};

fn matching_line_text<'a>(line: ropey::RopeSlice<'a>, cancelled: &impl Fn() -> bool) -> Option<std::borrow::Cow<'a, str>> {
    if cancelled() { return None; }
    if let Some(text) = line.as_str() { return Some(std::borrow::Cow::Borrowed(text)); }
    let mut text = String::with_capacity(line.len_bytes());
    for chunk in line.chunks() {
        if cancelled() { return None; }
        text.push_str(chunk);
    }
    Some(std::borrow::Cow::Owned(text))
}

/// Check cancellation even inside huge minified markup lines. ASCII matches
/// are UTF-8 boundaries, including when a search block splits a character.
fn tag_delimiter(text: &str, delimiter: u8, reverse: bool, cancelled: &impl Fn() -> bool) -> Option<usize> {
    if reverse {
        let mut end = text.len();
        for block in text.as_bytes().rchunks(4_096) {
            if cancelled() { return None; }
            end -= block.len();
            if let Some(offset) = memchr::memrchr(delimiter, block) { return Some(end + offset); }
        }
    } else {
        for (block_index, block) in text.as_bytes().chunks(4_096).enumerate() {
            if cancelled() { return None; }
            if let Some(offset) = memchr::memchr(delimiter, block) { return Some(block_index * 4_096 + offset); }
        }
    }
    None
}

impl Editor {
    /// Prove common negative cases without starting a worker. The markup
    /// prefix search is bounded even on a multi-megabyte minified line.
    pub(crate) fn may_have_cursor_pair(&self) -> bool {
        let Some(line) = self.rope.get_line(self.cursor_line) else { return false; };
        let bracket = |ch| matches!(ch, '(' | ')' | '[' | ']' | '{' | '}');
        if line.get_char(self.cursor_col).is_some_and(bracket)
            || self.cursor_col.checked_sub(1).and_then(|col| line.get_char(col)).is_some_and(bracket)
        { return true; }
        if !is_markup_language(self.language_name()) { return false; }
        let mut cursor = self.cursor_col.min(line.len_chars());
        // Matching includes the position immediately after the closing `>`.
        if cursor > 0 && line.char(cursor - 1) == '>' { cursor -= 1; }
        let before = line.slice(..cursor);
        let mut budget = 2_048usize;
        for chunk in before.chunks_at_byte(before.len_bytes()).0.reversed() {
            let bytes = chunk.as_bytes();
            let len = bytes.len().min(budget);
            if let Some(byte) = memchr::memrchr2(b'<', b'>', &bytes[bytes.len() - len..]) {
                return bytes[bytes.len() - len + byte] == b'<';
            }
            budget -= len;
            if budget == 0 { return true; } // Uncertain: let the worker decide.
        }
        false
    }

    /// Find the matching bracket position for the cursor, returns (line, col)
    pub(crate) fn find_matching_bracket(&self) -> Option<(usize, usize)> {
        self.find_matching_bracket_cancellable(&|| false)
    }

    pub(crate) fn find_matching_bracket_cancellable(
        &self, cancelled: &impl Fn() -> bool,
    ) -> Option<(usize, usize)> {
        if cancelled() || self.cursor_line >= self.rope.len_lines() {
            return None;
        }
        let line_slice = self.rope.line(self.cursor_line);

        // Try character at cursor first
        if self.cursor_col < line_slice.len_chars() {
            let ch = line_slice.char(self.cursor_col);
            if let Some(result) =
                self.find_matching_bracket_for_cancellable(ch, self.cursor_line, self.cursor_col, cancelled)
            {
                return Some(result);
            }
        }
        // Fall back to character before cursor (VS Code behavior)
        if self.cursor_col > 0 && self.cursor_col - 1 < line_slice.len_chars() {
            let ch = line_slice.char(self.cursor_col - 1);
            if let Some(result) =
                self.find_matching_bracket_for_cancellable(ch, self.cursor_line, self.cursor_col - 1, cancelled)
            {
                return Some(result);
            }
        }
        None
    }

    #[cfg(test)]
    pub(crate) fn find_matching_bracket_for(
        &self,
        ch: char,
        start_line: usize,
        start_col: usize,
    ) -> Option<(usize, usize)> {
        self.find_matching_bracket_for_cancellable(ch, start_line, start_col, &|| false)
    }

    fn find_matching_bracket_for_cancellable(
        &self, ch: char, start_line: usize, start_col: usize, cancelled: &impl Fn() -> bool,
    ) -> Option<(usize, usize)> {
        if cancelled() { return None; }
        let (target, forward) = match ch {
            '(' => (')', true), '[' => (']', true), '{' => ('}', true),
            ')' => ('(', false), ']' => ('[', false), '}' => ('{', false),
            _ => return None,
        };
        if start_line >= self.line_count() {
            return None;
        }
        let mut depth = 0i32;
        let mut block_comment = false;
        let mut candidates = Vec::new();
        let line_count = if forward { self.line_count() - start_line } else { start_line + 1 };
        for offset in 0..line_count {
            if cancelled() { return None; }
            let line = if forward { start_line + offset } else { start_line - offset };
            let slice = self.rope.line(line);
            let start = if forward && line == start_line { start_col.min(slice.len_chars()) } else { 0 };
            let end = if !forward && line == start_line { start_col } else { slice.len_chars() };
            let mut chars = slice.chars_at(start).enumerate().peekable();
            let mut single_quote = false;
            let mut double_quote = false;
            let mut backtick = false;
            let mut bc = block_comment;
            let mut next_cancel_check = 0;
            candidates.clear();
            // Seek once per line and advance the rope iterator, including
            // lookahead. Indexed RopeSlice::char inside this loop made long
            // lines pay for a tree traversal at every character.
            while let Some((offset, c)) = chars.next() {
                if offset >= next_cancel_check {
                    if cancelled() { return None; }
                    next_cancel_check = offset + 1_024;
                }
                let col = start + offset;
                let next = chars.peek().map(|&(_, c)| c);
                if (single_quote || double_quote || backtick) && c == '\\' {
                    chars.next();
                    continue;
                }
                if !bc {
                    if c == '\'' && !double_quote && !backtick {
                        single_quote = !single_quote;
                        continue;
                    }
                    if c == '"' && !single_quote && !backtick {
                        double_quote = !double_quote;
                        continue;
                    }
                    if c == '`' && !single_quote && !double_quote {
                        backtick = !backtick;
                        continue;
                    }
                }
                if !single_quote && !double_quote && !backtick && !bc {
                    if c == '/' && next == Some('/') {
                        // No later character on this logical line can affect
                        // bracket depth or the block-comment state.
                        break;
                    }
                    if c == '/' && next == Some('*') {
                        bc = true;
                        chars.next();
                        continue;
                    }
                }
                if bc && c == '*' && next == Some('/') {
                    bc = false;
                    chars.next();
                    continue;
                }
                if !single_quote && !double_quote && !backtick && !bc {
                    if forward {
                        if c == ch { depth += 1; }
                        if c == target { depth -= 1; }
                        if depth == 0 { return Some((line, col)); }
                    } else if col <= end && (c == ch || c == target) {
                        // Backward matching only needs bracket candidates,
                        // rather than a per-character context bitmap followed
                        // by a second complete indexed scan of the line.
                        candidates.push((col, c));
                    }
                }
            }
            if forward || line == start_line {
                block_comment = bc;
            }
            if !forward {
                for (index, &(col, c)) in candidates.iter().rev().enumerate() {
                    if index.is_multiple_of(1_024) && cancelled() { return None; }
                    if c == ch { depth += 1; }
                    if c == target { depth -= 1; }
                    if depth == 0 { return Some((line, col)); }
                }
            }
        }
        None
    }

    /// Check if cursor is inside an HTML/XML tag name and return info for linked editing.
    /// Returns (tag_name, name_start_col, name_end_col, is_closing, cursor_offset_in_name)
    #[cfg(test)]
    pub(crate) fn cursor_in_tag_name(&self) -> Option<(String, usize, usize, bool)> {
        let (start, end, closing) = self.cursor_in_tag_name_range()?;
        Some((
            self.rope
                .line(self.cursor_line)
                .slice(start..end)
                .to_string(),
            start,
            end,
            closing,
        ))
    }

    /// Resolve linked-editing coordinates without copying the surrounding
    /// line or the name. Most markup keystrokes can reject a preceding `>`
    /// immediately; long tag-free prefixes are searched by UTF-8 chunk bytes.
    pub(crate) fn cursor_in_tag_name_range(&self) -> Option<(usize, usize, bool)> {
        let lang = self.language_name();
        if !is_markup_language(lang) {
            return None;
        }
        let line = self.rope.get_line(self.cursor_line)?;
        let cursor = self.cursor_col.min(line.len_chars());
        let before = line.slice(..cursor);
        let mut end_byte = before.len_bytes();
        let mut opening = None;
        for chunk in before.chunks_at_byte(end_byte).0.reversed() {
            end_byte -= chunk.len();
            if let Some(byte) = memchr::memrchr2(b'<', b'>', chunk.as_bytes()) {
                if chunk.as_bytes()[byte] == b'>' {
                    return None;
                }
                opening = Some(before.byte_to_char(end_byte + byte));
                break;
            }
        }
        let opening = opening?;
        let closing = line.get_char(opening + 1) == Some('/');
        let name_start = opening + if closing { 2 } else { 1 };
        let after = line.slice(opening + 1..);
        let mut byte_offset = 0;
        let mut tag_end = None;
        for chunk in after.chunks() {
            if let Some(byte) = memchr::memchr(b'>', chunk.as_bytes()) {
                tag_end = Some(opening + 1 + after.byte_to_char(byte_offset + byte));
                break;
            }
            byte_offset += chunk.len();
        }
        let tag_end = tag_end?;
        if tag_end == opening + 1 {
            return None;
        }
        let name_end = name_start
            + line
                .slice(name_start..tag_end)
                .chars()
                .take_while(|ch| !ch.is_whitespace() && *ch != '/')
                .count();
        if cursor < name_start || cursor > name_end {
            return None;
        }
        Some((name_start, name_end, closing))
    }

    /// Find the matching HTML/XML tag for the tag at cursor position.
    /// Returns two ranges: (tag1_line, tag1_col_start, tag1_col_end, tag2_line, tag2_col_start, tag2_col_end)
    pub(crate) fn find_matching_tag(&self) -> Option<(usize, usize, usize, usize, usize, usize)> {
        self.find_matching_tag_cancellable(&|| false)
    }

    pub(crate) fn find_matching_tag_cancellable(
        &self, cancelled: &impl Fn() -> bool,
    ) -> Option<(usize, usize, usize, usize, usize, usize)> {
        if cancelled() || !is_markup_language(self.language_name()) {
            return None;
        }
        if self.cursor_line >= self.rope.len_lines() {
            return None;
        }
        let rope_line = self.rope.line(self.cursor_line);
        let line_text = matching_line_text(rope_line, cancelled)?;
        // Convert char-based cursor_col to byte position for string operations
        let cursor_byte = rope_line.char_to_byte(self.cursor_col.min(rope_line.len_chars()));
        // Check if cursor is inside a tag: find '<' before cursor and '>' after cursor
        let before = &line_text[..cursor_byte];
        let lt_pos = tag_delimiter(before, b'<', true, cancelled)?;
        let after_lt = &line_text[lt_pos..];
        let gt_rel = tag_delimiter(after_lt, b'>', false, cancelled)?;
        let tag_text = &line_text[lt_pos..=lt_pos + gt_rel];
        let tag_col_start = lt_pos;
        let tag_col_end = lt_pos + gt_rel + 1;
        // Check cursor is within this tag
        if cursor_byte > tag_col_end {
            return None;
        }
        let inside = tag_text.trim_start_matches('<').trim_end_matches('>');
        if inside.is_empty() {
            return None;
        }
        let is_closing = inside.starts_with('/');
        let tag_name = if is_closing {
            inside[1..].split(|c: char| c.is_whitespace()).next()?
        } else {
            if inside.ends_with('/') {
                return None;
            } // self-closing
            inside.split(|c: char| c.is_whitespace()).next()?
        };
        if tag_name.is_empty() {
            return None;
        }
        let total_lines = self.rope.len_lines();
        let max_scan = total_lines;
        if is_closing {
            // Search backward for matching opening tag
            let mut depth = 0i32;
            let start_line = self.cursor_line.saturating_sub(max_scan);
            for line in (start_line..=self.cursor_line).rev() {
                if cancelled() { return None; }
                let scan_rope = self.rope.line(line);
                let scan_line = matching_line_text(scan_rope, cancelled)?;
                let text = &*scan_line;
                // Find all tags in this line in reverse
                let mut positions: Vec<(usize, usize, bool)> = Vec::new(); // (col_start, col_end, is_close)
                let mut search_from = 0;
                while let Some(lt) = tag_delimiter(&text[search_from..], b'<', false, cancelled) {
                    if cancelled() { return None; }
                    let open_pos = search_from + lt;
                    if let Some(gt) = tag_delimiter(&text[open_pos..], b'>', false, cancelled) {
                        let close_pos = open_pos + gt + 1;
                        let tag_inner = &text[open_pos + 1..close_pos - 1];
                        let is_close = tag_inner.starts_with('/');
                        let name = if is_close {
                            tag_inner[1..]
                                .split(|c: char| c.is_whitespace())
                                .next()
                                .unwrap_or("")
                        } else {
                            tag_inner
                                .split(|c: char| c.is_whitespace())
                                .next()
                                .unwrap_or("")
                        };
                        if name.eq_ignore_ascii_case(tag_name) && !tag_inner.ends_with('/') {
                            let skip = line == self.cursor_line && open_pos == tag_col_start;
                            if !skip {
                                positions.push((open_pos, close_pos, is_close));
                            }
                        }
                        search_from = close_pos;
                    } else {
                        break;
                    }
                }
                for (index, &(cs, ce, ic)) in positions.iter().rev().enumerate() {
                    if index.is_multiple_of(1_024) && cancelled() { return None; }
                    if ic {
                        depth += 1;
                    } else {
                        depth -= 1;
                    }
                    if depth < 0 {
                        return Some((
                            line,
                            scan_rope.byte_to_char(cs),
                            scan_rope.byte_to_char(ce),
                            self.cursor_line,
                            rope_line.byte_to_char(tag_col_start),
                            rope_line.byte_to_char(tag_col_end),
                        ));
                    }
                }
            }
        } else {
            // Search forward for matching closing tag
            let mut depth = 0i32;
            let end_line = (self.cursor_line + max_scan).min(total_lines);
            for line in self.cursor_line..end_line {
                if cancelled() { return None; }
                let scan_rope = self.rope.line(line);
                let scan_line = matching_line_text(scan_rope, cancelled)?;
                let text = &*scan_line;
                let mut search_from = 0;
                while let Some(lt) = tag_delimiter(&text[search_from..], b'<', false, cancelled) {
                    if cancelled() { return None; }
                    let open_pos = search_from + lt;
                    if let Some(gt) = tag_delimiter(&text[open_pos..], b'>', false, cancelled) {
                        let close_pos = open_pos + gt + 1;
                        let tag_inner = &text[open_pos + 1..close_pos - 1];
                        let is_close = tag_inner.starts_with('/');
                        let name = if is_close {
                            tag_inner[1..]
                                .split(|c: char| c.is_whitespace())
                                .next()
                                .unwrap_or("")
                        } else {
                            tag_inner
                                .split(|c: char| c.is_whitespace())
                                .next()
                                .unwrap_or("")
                        };
                        if name.eq_ignore_ascii_case(tag_name) && !tag_inner.ends_with('/') {
                            let skip = line == self.cursor_line && open_pos == tag_col_start;
                            if !skip {
                                if is_close {
                                    depth -= 1;
                                } else {
                                    depth += 1;
                                }
                                if depth < 0 {
                                    return Some((
                                        self.cursor_line,
                                        rope_line.byte_to_char(tag_col_start),
                                        rope_line.byte_to_char(tag_col_end),
                                        line,
                                        scan_rope.byte_to_char(open_pos),
                                        scan_rope.byte_to_char(close_pos),
                                    ));
                                }
                            }
                        }
                        search_from = close_pos;
                    } else {
                        break;
                    }
                }
            }
        }
        None
    }

    pub(crate) fn go_to_matching_bracket(&mut self) {
        self.desired_col = None;
        if let Some((line, col)) = self.find_matching_bracket() {
            self.clear_selection();
            self.cursor_line = line;
            self.cursor_col = col;
            self.ensure_visible();
        } else {
            // Search forward on current line for nearest bracket, then try matching
            let rope_line = self.rope.line(self.cursor_line);
            let line_len = rope_line.len_chars();
            for (offset, ch) in rope_line.chars_at((self.cursor_col + 1).min(line_len)).enumerate() {
                let i = self.cursor_col + 1 + offset;
                if matches!(ch, '(' | ')' | '[' | ']' | '{' | '}') {
                    let saved_col = self.cursor_col;
                    self.cursor_col = i;
                    if let Some((ml, mc)) = self.find_matching_bracket() {
                        self.clear_selection();
                        self.cursor_line = ml;
                        self.cursor_col = mc;
                        self.ensure_visible();
                        return;
                    }
                    self.cursor_col = saved_col;
                }
            }
        }
    }

    pub(crate) fn select_to_matching_bracket(&mut self) {
        self.desired_col = None;
        if let Some((line, col)) = self.find_matching_bracket() {
            if self.selection_anchor.is_none() {
                self.selection_anchor = Some((self.cursor_line, self.cursor_col));
            }
            self.cursor_line = line;
            self.cursor_col = col;
            self.ensure_visible();
        }
    }

    pub(crate) fn select_inside_brackets(&mut self) {
        // Find innermost bracket pair that contains cursor
        let openers = ['(', '[', '{', '<'];
        let closers = [')', ']', '}', '>'];

        let mut depth = [0i32; 4]; // depth for each bracket type
        // Virtual-space selections can leave the caret beyond EOF. The old
        // indexed scan skipped those positions; seek only the existing text.
        let cursor_char_idx = (self.rope.line_to_char(self.cursor_line) + self.cursor_col)
            .min(self.rope.len_chars());

        // Search backward
        let mut open_pos = None;
        let mut bracket_type = 0;
        for (offset, c) in self.rope.chars_at(cursor_char_idx).reversed().enumerate() {
            let i = cursor_char_idx - offset - 1;
            for (bi, (o, cl)) in openers.iter().zip(closers.iter()).enumerate() {
                if c == *cl {
                    depth[bi] += 1;
                }
                if c == *o {
                    if depth[bi] == 0 {
                        open_pos = Some(i);
                        bracket_type = bi;
                        break;
                    }
                    depth[bi] -= 1;
                }
            }
            if open_pos.is_some() {
                break;
            }
        }

        if let Some(open) = open_pos {
            // Search forward for matching close
            let mut depth = 0i32;
            let opener = openers[bracket_type];
            let closer = closers[bracket_type];
            for (offset, c) in self.rope.chars_at(open + 1).enumerate() {
                let i = open + 1 + offset;
                if c == opener {
                    depth += 1;
                }
                if c == closer {
                    if depth == 0 {
                        // Select from open+1 to i
                        let start_line = self.rope.char_to_line(open + 1);
                        let start_col = (open + 1) - self.rope.line_to_char(start_line);
                        let end_line = self.rope.char_to_line(i);
                        let end_col = i - self.rope.line_to_char(end_line);
                        self.selection_anchor = Some((start_line, start_col));
                        self.cursor_line = end_line;
                        self.cursor_col = end_col;
                        self.ensure_visible();
                        return;
                    }
                    depth -= 1;
                }
            }
        }
    }

    /// Select text inside the nearest quote pair (single, double, or backtick).
    pub(crate) fn select_inside_quotes(&mut self) {
        let rope_line = self.rope.line(self.cursor_line);
        let col = self.cursor_col.min(rope_line.len_chars());
        let quotes = ['"', '\'', '`'];
        // Seek around the caret instead of collecting every character of the
        // line. A nearby quote pair now leaves a megabyte prefix/tail unread.
        // Quote-kind precedence and caret-on-quote skipping remain unchanged.
        for quote in quotes {
            let mut before = rope_line.chars_at(col).reversed().enumerate().peekable();
            let mut opening = None;
            while let Some((offset, ch)) = before.next() {
                if ch != quote { continue; }
                let mut escaped = false;
                while before.peek().is_some_and(|&(_, ch)| ch == '\\') {
                    before.next();
                    escaped = !escaped;
                }
                if !escaped {
                    opening = Some(col - offset - 1);
                    break;
                }
            }
            let at_cursor = rope_line.get_char(col);
            if opening.is_none() && at_cursor == Some(quote) {
                let escaped = rope_line.chars_at(col).reversed()
                    .take_while(|&ch| ch == '\\').count() % 2 != 0;
                if !escaped { opening = Some(col); }
            }
            let Some(opening) = opening else { continue; };
            let start = col + usize::from(at_cursor == Some(quote));
            let mut escaped = rope_line.chars_at(start).reversed()
                .take_while(|&ch| ch == '\\').count() % 2 != 0;
            for (offset, ch) in rope_line.chars_at(start).enumerate() {
                if ch == quote && !escaped {
                    self.selection_anchor = Some((self.cursor_line, opening + 1));
                    self.cursor_col = start + offset;
                    self.ensure_visible();
                    return;
                }
                escaped = ch == '\\' && !escaped;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::Editor;

    fn reference_tag_name(text: &str, cursor: usize) -> Option<(String, usize, usize, bool)> {
        let byte = text
            .char_indices()
            .nth(cursor)
            .map_or(text.len(), |(byte, _)| byte);
        let before = &text[..byte];
        let opening = before.rfind('<')?;
        if before[opening..].contains('>') {
            return None;
        }
        let end = opening + text[opening..].find('>')?;
        let inner = &text[opening + 1..end];
        if inner.is_empty() {
            return None;
        }
        let closing = inner.starts_with('/');
        let start = opening + if closing { 2 } else { 1 };
        let end = start
            + text[start..end]
                .find(|ch: char| ch.is_whitespace() || ch == '/')
                .unwrap_or(end - start);
        if byte < start || byte > end {
            return None;
        }
        Some((
            text[start..end].to_owned(),
            text[..start].chars().count(),
            text[..end].chars().count(),
            closing,
        ))
    }

    #[test]
    fn tag_name_lookup_matches_string_reference_across_unicode_and_rope_chunks() {
        for prefix in [String::new(), "x".repeat(4_093), "界é".repeat(2_048)] {
            for tag in [
                "<div title='x'>",
                "</span>",
                "<é界 attr='x'>",
                "< />",
                "</>",
                "<>",
                "<unfinished",
                "<a>text<b>",
            ] {
                let text = format!("{prefix}{tag} suffix");
                let mut editor = Editor::test_with_text(&text);
                editor.file_path = Some("test.html".into());
                for cursor in prefix.chars().count().saturating_sub(2)..=text.chars().count() {
                    editor.cursor_col = cursor;
                    assert_eq!(
                        editor.cursor_in_tag_name(),
                        reference_tag_name(&text, cursor),
                        "tag={tag:?} cursor={cursor}"
                    );
                }
            }
        }
    }

    #[test]
    #[ignore = "manual release benchmark of linked-editing lookups while typing/backspacing markup"]
    fn benchmark_markup_tag_lookup_avoids_long_line_materialization() {
        use std::{hint::black_box, time::Instant};
        for (label, text, cursor) in [
            (
                "text",
                format!("<div>{}</div>", "x".repeat(2 * 1024 * 1024)),
                1_000_000,
            ),
            (
                "tag",
                format!("{}<span title='x'>", "x".repeat(2 * 1024 * 1024)),
                2 * 1024 * 1024 + 3,
            ),
        ] {
            let mut editor = Editor::test_with_text(&text);
            editor.file_path = Some("test.html".into());
            editor.cursor_col = cursor;
            let mut before = Vec::new();
            let mut after = Vec::new();
            for sample in 0..7 {
                for fast in [sample % 2 == 0, sample % 2 != 0] {
                    let start = Instant::now();
                    for _ in 0..32 {
                        if fast {
                            black_box(editor.cursor_in_tag_name_range());
                        } else {
                            let owned = editor.rope.line(0).to_string();
                            black_box(reference_tag_name(&owned, cursor));
                        }
                    }
                    if fast {
                        after.push(start.elapsed() / 32);
                    } else {
                        before.push(start.elapsed() / 32);
                    }
                }
            }
            before.sort();
            after.sort();
            eprintln!(
                "2MiB markup {label} lookup: materialized {:?}, local {:?}",
                before[3], after[3]
            );
        }
    }

    #[test]
    fn find_matching_bracket_paren_forward() {
        let mut ed = Editor::test_with_text("foo(bar)");
        ed.cursor_line = 0;
        ed.cursor_col = 3; // on '('
        assert_eq!(ed.find_matching_bracket(), Some((0, 7)));
    }

    #[test]
    fn find_matching_bracket_paren_backward() {
        let mut ed = Editor::test_with_text("foo(bar)");
        ed.cursor_line = 0;
        ed.cursor_col = 7; // on ')'
        assert_eq!(ed.find_matching_bracket(), Some((0, 3)));
    }

    #[test]
    fn find_matching_bracket_curly() {
        let mut ed = Editor::test_with_text("fn main() {\n}\n");
        ed.cursor_line = 0;
        ed.cursor_col = 11; // on '{'
        assert_eq!(ed.find_matching_bracket(), Some((1, 0)));
    }

    #[test]
    fn find_matching_bracket_square() {
        let mut ed = Editor::test_with_text("[1, 2, 3]");
        ed.cursor_line = 0;
        ed.cursor_col = 0; // on '['
        assert_eq!(ed.find_matching_bracket(), Some((0, 8)));
    }

    #[test]
    fn find_matching_bracket_nested() {
        let mut ed = Editor::test_with_text("((()))");
        ed.cursor_line = 0;
        ed.cursor_col = 0; // outer '('
        assert_eq!(ed.find_matching_bracket(), Some((0, 5)));
    }

    #[test]
    fn find_matching_bracket_none() {
        let mut ed = Editor::test_with_text("hello world");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        assert_eq!(ed.find_matching_bracket(), None);
    }

    #[test]
    fn find_matching_bracket_in_string_ignored() {
        let mut ed = Editor::test_with_text("let s = \"(\"; foo()");
        ed.cursor_line = 0;
        ed.cursor_col = 16; // on '('  in foo()
        assert_eq!(ed.find_matching_bracket(), Some((0, 17)));
    }

    #[test]
    fn find_matching_bracket_before_cursor() {
        // VS Code behavior: if char AT cursor isn't a bracket, try char BEFORE cursor
        let mut ed = Editor::test_with_text("(x)");
        ed.cursor_line = 0;
        ed.cursor_col = 1; // on 'x', but '(' is before cursor
        assert_eq!(ed.find_matching_bracket(), Some((0, 2)));
    }

    #[test]
    fn select_inside_brackets_basic() {
        let mut ed = Editor::test_with_text("(hello)");
        ed.cursor_line = 0;
        ed.cursor_col = 3; // inside brackets
        ed.select_inside_brackets();
        assert_eq!(ed.selection_anchor, Some((0, 1)));
        assert_eq!(ed.cursor_col, 6);
    }

    #[test]
    fn select_inside_quotes_double() {
        let mut ed = Editor::test_with_text("let s = \"hello world\";");
        ed.cursor_line = 0;
        ed.cursor_col = 12; // inside quotes
        ed.select_inside_quotes();
        assert_eq!(ed.selection_anchor, Some((0, 9)));
        assert_eq!(ed.cursor_col, 20);
    }

    #[test]
    fn select_inside_quotes_single() {
        let mut ed = Editor::test_with_text("let c = 'x';");
        ed.cursor_line = 0;
        ed.cursor_col = 9; // on 'x'
        ed.select_inside_quotes();
        assert_eq!(ed.selection_anchor, Some((0, 9)));
        assert_eq!(ed.cursor_col, 10);
    }

    #[test]
    fn go_to_matching_bracket_moves_cursor() {
        let mut ed = Editor::test_with_text("fn main() {\n    body\n}\n");
        ed.cursor_line = 0;
        ed.cursor_col = 11; // on '{'
        ed.viewport_height = 50;
        ed.go_to_matching_bracket();
        assert_eq!(ed.cursor_line, 2);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn select_to_matching_bracket_sets_anchor() {
        let mut ed = Editor::test_with_text("(abc)");
        ed.cursor_line = 0;
        ed.cursor_col = 0; // on '('
        ed.viewport_height = 50;
        ed.select_to_matching_bracket();
        assert_eq!(ed.selection_anchor, Some((0, 0)));
        assert_eq!(ed.cursor_col, 4);
    }

    #[test]
    fn cursor_in_tag_name_html() {
        let mut ed = Editor::test_with_text("<div class=\"foo\">hello</div>");
        ed.cursor_line = 0;
        ed.cursor_col = 2; // in 'div'
        ed.file_path = Some(std::path::PathBuf::from("test.html"));
        let result = ed.cursor_in_tag_name();
        assert!(result.is_some());
        let (name, _start, _end, is_closing) = result.unwrap();
        assert_eq!(name, "div");
        assert!(!is_closing);
    }

    #[test]
    fn cursor_in_tag_name_not_markup() {
        let mut ed = Editor::test_with_text("<div>hello</div>");
        ed.cursor_line = 0;
        ed.cursor_col = 2;
        ed.file_path = Some(std::path::PathBuf::from("test.rs"));
        assert!(ed.cursor_in_tag_name().is_none());
    }

    #[test]
    fn find_matching_bracket_for_non_bracket_returns_none() {
        let ed = Editor::test_with_text("hello");
        assert_eq!(ed.find_matching_bracket_for('a', 0, 0), None);
    }
}
