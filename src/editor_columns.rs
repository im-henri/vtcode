//! Translate painted cells to source columns without treating tabs as characters.
use crate::editor::Editor;
use ropey::RopeSlice;
use unicode_width::UnicodeWidthChar;

/// A preceding tab ends at a tab stop. Only the source columns since that tab
/// matter; seek backwards through rope chunks instead of allocating the line.
pub(crate) fn tab_width_at(line: RopeSlice<'_>, column: usize, tab_size: usize) -> usize {
    let tab_size = tab_size.max(1);
    let prefix = line.slice(..column.min(line.len_chars()));
    let (mut chunks, _, _, _) = prefix.chunks_at_byte(prefix.len_bytes());
    let mut after = 0;
    while let Some(chunk) = chunks.prev() {
        if let Some(tab) = memchr::memrchr(b'\t', chunk.as_bytes()) {
            after += chunk[tab + 1..].chars().count();
            return tab_size - after % tab_size;
        }
        after += chunk.chars().count();
    }
    tab_size - after % tab_size
}

impl Editor {
    /// Horizontal scrolling stores source columns, but the viewport holds
    /// cells. The ordinary reveal first bounds the source span; correct for
    /// tabs/wide glyphs using only that span, never a long hidden prefix.
    pub(crate) fn reveal_cursor_cells(&mut self) {
        let width = self.viewport_width.max(1);
        let column = self.cursor_col.min(self.line_len(self.cursor_line));
        if width <= 2 {
            self.horizontal_scroll = column;
            return;
        }
        let start = self.horizontal_scroll.min(column);
        let cells = self.cell_at_source_column(self.cursor_line, start, column);
        let margin = 4.min((width - 1) / 2);
        let target = width - margin - 1;
        if cells > target {
            let (mut start, _) = self.source_column_at_cell(
                self.cursor_line, start, column, cells - target,
            );
            // The requested cell can lie inside a tab or wide glyph. Move
            // past that glyph so the caret actually fits, including at EOL.
            if self.cell_at_source_column(self.cursor_line, start, column) > target {
                start = (start + 1).min(column);
            }
            self.horizontal_scroll = start;
        }
    }

    /// Painted cell offset of a source column within a visual row. Reuse the
    /// render cache when available, and otherwise visit only the needed prefix.
    pub(crate) fn cell_at_source_column(&self, line: usize, start: usize, col: usize) -> usize {
        if let Some(glyphs) = self.render_line_cache.borrow().pointer_glyphs(
            line, (self.content_version, self.wrap_revision, self.tab_size), &self.rope,
        ) {
            let end = col.min(glyphs.line_chars);
            let start = start.min(end);
            if end <= glyphs.chars.len() {
                return glyphs.cell_column(end) - glyphs.cell_column(start);
            }
        }
        let Some(text) = self.rope.get_line(line) else { return 0; };
        let end = col.min(Self::line_content_len(text));
        let start = start.min(end);
        if start == end { return 0; }

        let mut cells = 0;
        let mut column = start;
        let mut after_tab = None;
        let tab_size = self.tab_size.max(1);
        for chunk in text.slice(start..end).chunks() {
            if chunk.bytes().all(|byte| (b' '..=b'~').contains(&byte)) {
                cells += chunk.len();
                column += chunk.len();
                if let Some(after) = &mut after_tab { *after += chunk.len(); }
                continue;
            }
            for ch in chunk.chars() {
                cells += if ch == '\t' {
                    let width = after_tab.map_or_else(
                        || tab_width_at(text, column, tab_size),
                        |after: usize| tab_size - after % tab_size,
                    );
                    after_tab = Some(0);
                    width
                } else {
                    if let Some(after) = &mut after_tab { *after += 1; }
                    ch.width().unwrap_or(0)
                };
                column += 1;
            }
        }
        cells
    }

    pub(crate) fn column_on_wrapped_row(&self, row: &crate::editor::WrappedRowInfo, cell: usize) -> usize {
        // A continuation boundary belongs to the next row, not this one.
        let end = if row.end_col < row.line_len { row.end_col.saturating_sub(1) } else { row.end_col };
        self.source_column_at_cell(row.line_idx, row.start_col, end, cell).0
    }

    /// `start`/`end` are source columns, while `cell` is relative to the painted
    /// row. The boolean distinguishes source glyphs from the blank area at EOL.
    pub(crate) fn source_column_at_cell(
        &self,
        line: usize,
        start: usize,
        end: usize,
        cell: usize,
    ) -> (usize, bool) {
        if let Some(glyphs) = self.render_line_cache.borrow().pointer_glyphs(
            line, (self.content_version, self.wrap_revision, self.tab_size), &self.rope,
        ) {
            let end = end.min(glyphs.line_chars);
            let start = start.min(end);
            let known_end = end.min(glyphs.chars.len());
            if start <= known_end {
                let result = glyphs.source_at_cell(start, known_end, cell);
                if result.1 || known_end == end { return result; }
            }
        }

        let Some(text) = self.rope.get_line(line) else { return (0, false); };
        let end = end.min(Self::line_content_len(text));
        let start = start.min(end);

        // Before the first paint, after an edit, and outside the visible cache,
        // inspect only as far as the requested cell. Plain text never needs a
        // scan of the hidden prefix, even when horizontally scrolled far right.
        let mut x = 0;
        let mut column = start;
        let mut tab_column = None;
        let tab_size = self.tab_size.max(1);
        for chunk in text.slice(start..end).chunks() {
            // Vertical movement also uses this path before a row is painted.
            // Skip plain chunks without decoding every character on long lines.
            let checked = cell.saturating_sub(x).saturating_add(1).min(chunk.len());
            if chunk.as_bytes()[..checked].iter().all(|byte| (b' '..=b'~').contains(byte)) {
                if cell < x + chunk.len() { return (column + (cell - x), true); }
                x += chunk.len();
                column += chunk.len();
                if let Some(previous) = &mut tab_column { *previous += chunk.len(); }
                continue;
            }
            for ch in chunk.chars() {
                let width = if ch == '\t' {
                    let width = tab_column.map_or_else(
                        || tab_width_at(text, column, tab_size),
                        |previous: usize| tab_size - previous % tab_size,
                    );
                    tab_column = Some(0);
                    width
                } else {
                    if let Some(previous) = &mut tab_column { *previous += 1; }
                    ch.width().unwrap_or(0)
                };
                if cell < x + width { return (column, true); }
                x += width;
                column += 1;
            }
        }
        (end, false)
    }
}
