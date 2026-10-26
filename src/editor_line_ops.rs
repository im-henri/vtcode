use crate::editor::Editor;

/// A prefix insertion/removal in original rope character coordinates.
pub(crate) struct LinePrefixEdit<'a> {
    pub line: usize,
    pub index: usize,
    pub col: usize,
    pub removed: usize,
    pub insert: &'a str,
}

/// Split exactly one line ending, retaining any preceding blank lines.
pub(crate) fn split_line_ending(text: &str) -> (&str, &'static str) {
    for ending in ["\r\n", "\n", "\r"] {
        if let Some(content) = text.strip_suffix(ending) {
            return (content, ending);
        }
    }
    (text, "")
}

fn rope_line_ending_len(rope: ropey::RopeSlice<'_>) -> usize {
    let len = rope.len_chars();
    match len.checked_sub(1).map(|index| rope.char(index)) {
        Some('\n') if len > 1 && rope.char(len - 2) == '\r' => 2,
        Some('\n' | '\r') => 1,
        _ => 0,
    }
}

/// Borrow ordinary lines directly from the source. Only lines crossing Rope
/// chunks need a copy; retained records stay compact for large line counts.
pub(crate) fn split_line_text(line: ropey::RopeSlice<'_>)
    -> (std::borrow::Cow<'_, str>, &'static str)
{
    if let Some(text) = line.as_str() {
        let (body, ending) = split_line_ending(text);
        return (std::borrow::Cow::Borrowed(body), ending);
    }
    let mut text = line.to_string();
    let (body, ending) = split_line_ending(&text);
    let len = body.len();
    text.truncate(len);
    (std::borrow::Cow::Owned(text), ending)
}

enum CopiedText {
    Small(String),
    Shared(ropey::Rope),
}

impl CopiedText {
    fn new(slice: ropey::RopeSlice<'_>, prefix: &str, suffix: &str) -> Self {
        // Prepare from the caller's existing slice so short lines need no
        // additional tree seek or intermediate Rope allocation.
        if slice.len_bytes() <= 1024 {
            let mut text = String::with_capacity(prefix.len() + slice.len_bytes() + suffix.len());
            text.push_str(prefix);
            for chunk in slice.chunks() { text.push_str(chunk); }
            text.push_str(suffix);
            Self::Small(text)
        } else {
            let mut text: ropey::Rope = slice.into();
            if !prefix.is_empty() { text.insert(0, prefix); }
            if !suffix.is_empty() { text.insert(text.len_chars(), suffix); }
            Self::Shared(text)
        }
    }
}

impl Editor {
    fn insert_copied_text(&mut self, index: usize, text: CopiedText) {
        crate::editor_document::edit!(self, {
        match text {
            CopiedText::Small(text) => self.rope.insert(index, &text),
            CopiedText::Shared(text) => {
                let tail = self.rope.split_off(index);
                self.rope.append(text);
                self.rope.append(tail);
            }
        }
            })
    }

    pub(crate) fn duplicate_line(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        self.save_undo();
        if let Some((start, end, _, _)) = self.clamped_selection_indices() {
            let text = CopiedText::new(self.rope.slice(start..end), "", "");
            self.insert_copied_text(end, text);
            self.modified = true;
            let new_idx = end + (end - start);
            self.cursor_line = self.rope.char_to_line(new_idx);
            self.cursor_col = new_idx - self.rope.line_to_char(self.cursor_line);
            self.clear_selection();
        } else {
            // Collect all unique lines from primary + extra cursors
            let mut lines: Vec<usize> = vec![self.cursor_line];
            for &(l, _) in &self.extra_cursors {
                lines.push(l);
            }
            lines.sort_unstable();
            lines.dedup();

            // Duplicate lines bottom-to-top to keep indices stable for earlier inserts
            for &line_idx in lines.iter().rev() {
                let line = self.rope.line(line_idx);
                let start = self.rope.line_to_char(line_idx);
                let end = start + line.len_chars();
                let prefix = if rope_line_ending_len(line) > 0 { "" }
                    else if self.cached_line_ending == "CRLF" { "\r\n" } else { "\n" };
                let text = CopiedText::new(line, prefix, "");
                self.insert_copied_text(end, text);
            }
            // Move all cursors down by the number of duplicated lines
            self.cursor_line += lines.partition_point(|&line| line <= self.cursor_line);
            if let Some((ref mut al, _)) = self.selection_anchor {
                *al += lines.partition_point(|&line| line <= *al);
            }
            for ec in &mut self.extra_cursors {
                ec.0 += lines.partition_point(|&line| line <= ec.0);
            }
            for (al, _) in self.extra_cursor_anchors.iter_mut().flatten() {
                *al += lines.partition_point(|&line| line <= *al);
            }
            self.modified = true;
        }
        self.clear_stale_folds();
        self.ensure_visible();
            })
    }

    pub(crate) fn duplicate_line_up(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        self.save_undo();
        if let Some((start, end, _, _)) = self.clamped_selection_indices() {
            // Rectangular selections can extend past the end of a short line.
            // Retain direction using the same clamped offsets as the copy.
            let forward = self.selection_anchor.unwrap() <= (self.cursor_line, self.cursor_col);
            let (cursor, anchor) = if forward { (end, start) } else { (start, end) };
            let text = CopiedText::new(self.rope.slice(start..end), "", "");
            self.insert_copied_text(start, text);
            let inserted = end - start;
            self.cursor_line = self.rope.char_to_line(cursor + inserted);
            self.cursor_col = cursor + inserted - self.rope.line_to_char(self.cursor_line);
            let line = self.rope.char_to_line(anchor + inserted);
            self.selection_anchor = Some((line, anchor + inserted - self.rope.line_to_char(line)));
        } else {
            // Collect all unique lines from primary + extra cursors
            let mut lines: Vec<usize> = vec![self.cursor_line];
            for &(l, _) in &self.extra_cursors {
                lines.push(l);
            }
            lines.sort_unstable();
            lines.dedup();

            // Duplicate lines top-to-bottom, each insert shifts lines below
            for (i, &line_idx) in lines.iter().enumerate() {
                let shifted_idx = line_idx + i;
                let line = self.rope.line(shifted_idx);
                let start = self.rope.line_to_char(shifted_idx);
                let suffix = if rope_line_ending_len(line) > 0 { "" }
                    else if self.cached_line_ending == "CRLF" { "\r\n" } else { "\n" };
                let text = CopiedText::new(line, "", suffix);
                self.insert_copied_text(start, text);
            }
            self.cursor_line += lines.partition_point(|&line| line < self.cursor_line);
            for cursor in &mut self.extra_cursors {
                cursor.0 += lines.partition_point(|&line| line < cursor.0);
            }
            for (line, _) in self.extra_cursor_anchors.iter_mut().flatten() {
                *line += lines.partition_point(|&edited| edited < *line);
            }
            if let Some((line, _)) = self.selection_anchor.as_mut() {
                *line += lines.partition_point(|&edited| edited < *line);
            }
        }
        self.modified = true;
        self.clear_stale_folds();
        self.ensure_visible();
            })
    }

    /// Visit the union of selected lines, not the span between separate carets.
    /// Merge ranges before reading so overlapping selections edit a line once;
    /// seek once per range and never scan the untouched gaps between them.
    pub(crate) fn line_prefix_edit_lines(&self) -> impl Iterator<Item = (usize, usize, ropey::RopeSlice<'_>)> + Clone {
        let max_line = self.line_count().saturating_sub(1);
        let mut ranges = Vec::with_capacity(self.extra_cursors.len() + 1);
        let mut add = |cursor: (usize, usize), anchor: Option<(usize, usize)>| {
            let anchor = anchor.unwrap_or(cursor);
            let (start, end) = (cursor.min(anchor), cursor.max(anchor));
            let last = if end.1 == 0 && end.0 > start.0 { end.0 - 1 } else { end.0 };
            ranges.push((start.0.min(max_line), last.min(max_line)));
        };
        add((self.cursor_line, self.cursor_col), self.selection_anchor);
        for (index, &cursor) in self.extra_cursors.iter().enumerate() {
            add(cursor, self.extra_cursor_anchors.get(index).copied().flatten());
        }
        ranges.sort_unstable();
        let mut count = 0;
        for index in 0..ranges.len() {
            let (first, last) = ranges[index];
            if count > 0 && first <= ranges[count - 1].1.saturating_add(1) {
                ranges[count - 1].1 = ranges[count - 1].1.max(last);
            } else {
                ranges[count] = (first, last);
                count += 1;
            }
        }
        ranges.truncate(count);
        ranges.into_iter().flat_map(move |(first, last)| {
            let mut index = self.rope.line_to_char(first);
            self.rope.lines_at(first).take(last - first + 1).enumerate().map(move |(offset, line)| {
                let entry = (first + offset, index, line);
                index += line.len_chars();
                entry
            })
        })
    }

    /// Apply prefix edits once, then visit each cursor and anchor once. The old
    /// indentation/comment loops revisited every cursor for every edited line.
    pub(crate) fn apply_line_prefix_edits(&mut self, edits: &[LinePrefixEdit<'_>]) {
        crate::editor_document::edit!(self, {
        if edits.is_empty() { return; }
        self.save_undo();
        self.mark_syntax_dirty_range(edits[0].line, edits.last().unwrap().line);
        if let Some((start, end, text)) = crate::editor_text_ops::dense_edit_text(
            &self.rope, edits.iter().map(|edit| (edit.index, edit.index + edit.removed, edit.insert)),
        ) {
            if start < end { self.rope.remove(start..end); }
            self.rope.insert(start, &text);
        } else {
            for edit in edits.iter().rev() {
                if edit.removed > 0 { self.rope.remove(edit.index..edit.index + edit.removed); }
                if !edit.insert.is_empty() { self.rope.insert(edit.index, edit.insert); }
            }
        }
        let adjust = |(line, col): &mut (usize, usize)| {
            if let Ok(index) = edits.binary_search_by_key(line, |edit| edit.line) {
                let edit = &edits[index];
                if *col >= edit.col {
                    *col = edit.col + (*col - edit.col).saturating_sub(edit.removed)
                        + edit.insert.chars().count();
                }
            }
        };
        let mut main = (self.cursor_line, self.cursor_col);
        adjust(&mut main);
        (self.cursor_line, self.cursor_col) = main;
        if let Some(anchor) = self.selection_anchor.as_mut() { adjust(anchor); }
        for cursor in &mut self.extra_cursors { adjust(cursor); }
        for anchor in self.extra_cursor_anchors.iter_mut().flatten() { adjust(anchor); }
        self.modified = true;
        self.ensure_visible();
            })
    }

    pub(crate) fn dedent_line(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        let mut edits = Vec::new();
        for (line_number, index, line) in self.line_prefix_edit_lines() {
            let mut chars = line.chars();
            let first_char = chars.next();
            let removed = if first_char == Some('\t') { 1 }
                else if first_char == Some(' ') && self.tab_size > 0 {
                    1 + chars.take(self.tab_size - 1).take_while(|&c| c == ' ').count()
                } else { 0 };
            if removed > 0 { edits.push(LinePrefixEdit { line: line_number, index, col: 0, removed, insert: "" }); }
        }
        self.apply_line_prefix_edits(&edits);
            })
    }

    pub(crate) fn indent_lines(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        let lines = self.line_prefix_edit_lines();
        let first_char = lines.clone().next().and_then(|(_, _, line)| line.chars().next());
        let insert = if first_char == Some('\t') || (first_char != Some(' ') && self.use_tabs) {
            std::borrow::Cow::Borrowed("\t")
        } else {
            crate::helpers::spaces(self.tab_size)
        };
        if insert.is_empty() { return; }
        let edits: Vec<_> = lines.map(|(line, index, _)| {
            LinePrefixEdit { line, index, col: 0, removed: 0, insert: &insert }
        }).collect();
        self.apply_line_prefix_edits(&edits);
            })
    }

    pub(crate) fn join_lines(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        let total = self.line_count();
        // Check multi-line selection first before checking cursor position
        let (start, end) = if let Some((sl, _, el, ec)) = self.selection_range().filter(|_| self.has_selection()) {
            let effective_end = if ec == 0 && el > sl { el - 1 } else { el };
            (sl, effective_end)
        } else {
            if self.cursor_line + 1 >= total { return; }
            (self.cursor_line, self.cursor_line + 1)
        };
        if start + 1 > end.min(total - 1) { return; }
        self.save_undo();
        self.mark_syntax_dirty_range(start, end.min(total - 1));

        // Build the joined region in one forward pass. Repeatedly joining
        // from the bottom recopied the growing suffix for every selected line.
        let join_end = end.min(total - 1);
        let region_start = self.rope.line_to_char(start);
        let region_end = if join_end + 1 < total { self.rope.line_to_char(join_end + 1) } else { self.rope.len_chars() };
        let mut joined = String::with_capacity(self.rope.char_to_byte(region_end) - self.rope.char_to_byte(region_start));
        let mut first_content_len = 0;
        let mut has_tail_content = false;
        for (offset, line) in self.rope.lines_at(start).take(join_end - start + 1).enumerate() {
            let mut content_end = line.len_chars();
            let mut tail = line.chars_at(content_end).reversed();
            if tail.next() == Some('\n') { content_end -= 1; }
            if line.get_char(content_end.saturating_sub(1)) == Some('\r') && content_end > 0 { content_end -= 1; }
            let content_start = if offset == 0 { 0 }
                else { line.slice(..content_end).chars().take_while(|c| c.is_whitespace()).count() };
            if offset == 0 { first_content_len = content_end; }
            if content_start < content_end {
                if offset > 0 {
                    has_tail_content = true;
                    if !joined.is_empty() { joined.push(' '); }
                }
                for chunk in line.slice(content_start..content_end).chunks() { joined.push_str(chunk); }
            }
            // Even an empty final line owns the boundary to unselected text.
            if offset + start == join_end {
                for chunk in line.slice(content_end..).chunks() { joined.push_str(chunk); }
            }
        }
        let old_lines = self.line_count();
        self.rope.remove(region_start..region_end);
        self.rope.insert(region_start, &joined);
        self.shift_bookmarks(start + 1, -((old_lines - self.line_count()) as isize));
        self.cursor_line = start;
        self.cursor_col = first_content_len + usize::from(first_content_len > 0 && has_tail_content);
        self.clear_selection();
        self.extra_cursors.clear();
        self.extra_cursor_anchors.clear();
        self.modified = true;
        self.clear_stale_folds();
        self.ensure_visible();
            })
    }

    /// Swap adjacent blocks, transferring just their separating line ending
    /// when the second block is the unterminated final line(s).
    fn swap_adjacent_line_blocks(&mut self, first: usize, middle: usize, last: usize) {
        crate::editor_document::edit!(self, {
        self.mark_syntax_dirty_range(first, last);
        let start = self.rope.line_to_char(first);
        let split = self.rope.line_to_char(middle);
        let end = if last + 1 < self.line_count() {
            self.rope.line_to_char(last + 1)
        } else {
            self.rope.len_chars()
        };
        let chars = end - start;
        // UTF-8 needs at most four bytes per scalar. Ordinary short moves can
        // choose the direct path without two extra character-to-byte seeks.
        if chars <= 256 || (chars <= 1024 && self.rope.slice(start..end).len_bytes() <= 1024) {
            let left = self.rope.slice(start..split).to_string();
            let mut right = self.rope.slice(split..end).to_string();
            let left = if split_line_ending(&right).1.is_empty() {
                let (content, ending) = split_line_ending(&left);
                right.push_str(ending);
                content
            } else { &left };
            right.push_str(left);
            self.rope.remove(start..end);
            self.rope.insert(start, &right);
            return;
        }
        // Split/join shares interior Rope nodes, including undo snapshots.
        // Moving a large selection must not flatten and re-decode its text.
        let tail = self.rope.split_off(end);
        let mut right = self.rope.split_off(split);
        let mut left = self.rope.split_off(start);
        if rope_line_ending_len(right.slice(..)) == 0 {
            let ending = left.split_off(left.len_chars() - rope_line_ending_len(left.slice(..)));
            right.append(ending);
        }
        self.rope.append(right);
        self.rope.append(left);
        self.rope.append(tail);
            })
    }

    pub(crate) fn move_line_up(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        // Determine range of lines to move
        let (first_line, last_line) = if let Some((sl, _, el, ec)) = self.selection_range() {
            let effective_end = if ec == 0 && el > sl { el - 1 } else { el };
            (sl, effective_end)
        } else {
            (self.cursor_line, self.cursor_line)
        };
        if first_line == 0 { return; }
        self.save_undo();
        self.swap_adjacent_line_blocks(first_line - 1, first_line, last_line);

        // Adjust cursor and selection
        self.cursor_line -= 1;
        let displaced_to = last_line;
        if let Some((line, _)) = self.selection_anchor.as_mut() {
            // The exclusive, column-zero end belongs to the selection too.
            *line -= 1;
        }
        for ec in &mut self.extra_cursors {
            if ec.0 >= first_line && ec.0 <= last_line {
                ec.0 -= 1;
            } else if ec.0 == first_line - 1 {
                // Cursor on the displaced line moves to where the block was
                ec.0 = displaced_to;
            }
        }
        for a in self.extra_cursor_anchors.iter_mut().flatten() {
            if a.0 >= first_line && a.0 <= last_line {
                a.0 -= 1;
            } else if a.0 == first_line - 1 {
                a.0 = displaced_to;
            }
        }
        // Update bookmarks: lines in the moved block shift -1, displaced line shifts to block end
        let new_bookmarks: std::collections::BTreeSet<usize> = self.bookmarks.iter().map(|&b| {
            if b >= first_line && b <= last_line {
                b - 1
            } else if b == first_line - 1 {
                displaced_to
            } else {
                b
            }
        }).collect();
        self.bookmarks = new_bookmarks;
        self.modified = true;
        self.clear_stale_folds();
        self.ensure_visible();
            })
    }

    pub(crate) fn move_line_down(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        // Determine range of lines to move
        let (first_line, last_line) = if let Some((sl, _, el, ec)) = self.selection_range() {
            let effective_end = if ec == 0 && el > sl { el - 1 } else { el };
            (sl, effective_end)
        } else {
            (self.cursor_line, self.cursor_line)
        };
        if last_line + 1 >= self.line_count() { return; }
        // Don't move down if the line below is just the empty trailing line (from trailing newline)
        let below_line = last_line + 1;
        if below_line == self.rope.len_lines() - 1 && self.rope.line(below_line).len_chars() == 0 { return; }
        self.save_undo();
        self.swap_adjacent_line_blocks(first_line, below_line, below_line);

        // Adjust cursor and selection
        self.cursor_line += 1;
        if self.cursor_line >= self.line_count() {
            self.cursor_line = self.line_count() - 1;
            self.cursor_col = self.line_len(self.cursor_line);
        }
        let displaced_to = first_line;
        if let Some((al, ac)) = self.selection_anchor {
            let line = (al + 1).min(self.line_count() - 1);
            let col = if al + 1 >= self.line_count() { self.line_len(line) } else { ac };
            self.selection_anchor = Some((line, col));
        }
        for ec in &mut self.extra_cursors {
            if ec.0 >= first_line && ec.0 <= last_line {
                ec.0 += 1;
            } else if ec.0 == last_line + 1 {
                // Cursor on the displaced line moves to where the block was
                ec.0 = displaced_to;
            }
        }
        for a in self.extra_cursor_anchors.iter_mut().flatten() {
            if a.0 >= first_line && a.0 <= last_line {
                a.0 += 1;
            } else if a.0 == last_line + 1 {
                a.0 = displaced_to;
            }
        }
        // Update bookmarks: lines in the moved block shift +1, displaced line moves to block start
        let new_bookmarks: std::collections::BTreeSet<usize> = self.bookmarks.iter().map(|&b| {
            if b >= first_line && b <= last_line {
                b + 1
            } else if b == last_line + 1 {
                displaced_to
            } else {
                b
            }
        }).collect();
        self.bookmarks = new_bookmarks;
        self.modified = true;
        self.clear_stale_folds();
        self.ensure_visible();
            })
    }

    pub(crate) fn delete_line(&mut self) {
        crate::editor_document::edit!(self, {
        self.desired_col = None;
        self.save_undo();

        // Work in intervals, not one entry/tree edit per selected line.
        let total = self.line_count();
        let mut ranges = Vec::with_capacity(self.extra_cursors.len() + 1);
        let mut add = |cursor: (usize, usize), anchor: Option<(usize, usize)>| {
            let anchor = anchor.unwrap_or(cursor);
            let (start, end) = if anchor <= cursor { (anchor, cursor) } else { (cursor, anchor) };
            let last = if end.1 == 0 && end.0 > start.0 { end.0 - 1 } else { end.0 };
            if start.0 < total {
                ranges.push((start.0, last.saturating_add(1).min(total)));
            }
        };
        add((self.cursor_line, self.cursor_col), self.selection_anchor);
        for (i, &cursor) in self.extra_cursors.iter().enumerate() {
            add(cursor, self.extra_cursor_anchors.get(i).copied().flatten());
        }
        self.clear_selection();
        self.extra_cursors.clear();
        self.extra_cursor_anchors.clear();
        ranges.sort_unstable();
        let mut merged = 0;
        for i in 0..ranges.len() {
            let (start, end) = ranges[i];
            if merged > 0 && start <= ranges[merged - 1].1 {
                ranges[merged - 1].1 = ranges[merged - 1].1.max(end);
            } else {
                ranges[merged] = (start, end);
                merged += 1;
            }
        }
        ranges.truncate(merged);
        for &(first, end) in ranges.iter().rev() {
            let start = if end == total && first > 0 {
                // Deleting the EOF line(s) also removes the preceding separator.
                self.rope.line_to_char(first - 1) + self.line_len(first - 1)
            } else { self.rope.line_to_char(first) };
            let end = if end < self.rope.len_lines() {
                self.rope.line_to_char(end)
            } else { self.rope.len_chars() };
            self.rope.remove(start..end);
        }
        self.cursor_line = ranges.first().map_or(0, |range| range.0)
            .min(self.line_count().saturating_sub(1));
        self.clamp_cursor_col();
        self.modified = true;
        // Both collections are sorted: remap every bookmark just once.
        if !self.bookmarks.is_empty() {
            let mut range = 0;
            let mut removed = 0;
            self.bookmarks = self.bookmarks.iter().filter_map(|&line| {
                while range < ranges.len() && ranges[range].1 <= line {
                    removed += ranges[range].1 - ranges[range].0;
                    range += 1;
                }
                if range < ranges.len() && ranges[range].0 <= line { None }
                else { Some(line - removed) }
            }).collect();
        }
        self.clear_stale_folds();
        self.ensure_visible();
            })
    }

    pub(crate) fn delete_to_line_start(&mut self) {
        crate::editor_document::edit!(self, {
        self.multi_cursor_delete_to_line_boundary(false);
            })
    }

    /// Delete from every cursor to its line end, merging overlapping suffixes.
    pub(crate) fn delete_to_line_end(&mut self) {
        crate::editor_document::edit!(self, {
        self.multi_cursor_delete_to_line_boundary(true);
            })
    }
}

#[cfg(test)]
#[path = "editor_bulk_edit_tests.rs"]
mod tests;
