//! Cell-aware log wrapping. Retain one row prefix per logical line, not a copy
//! of the text or a table per glyph. Appends and retention update only the tail
//! and expired prefix; only a width change lays out the retained buffer again.
use super::*;
use unicode_width::UnicodeWidthStr;

pub(super) use crate::text_wrap::Segment;

pub(super) fn glyph_width(glyph: &str, cell: usize) -> usize {
    if glyph == "\t" {
        4 - cell % 4
    } else {
        glyph.width()
    }
}

pub(super) fn segments(text: &str, width: usize) -> impl Iterator<Item = Segment> {
    crate::text_wrap::segments(text, width, glyph_width)
}

#[derive(Default)]
pub(super) struct Layout {
    pub width: usize,
    starts: VecDeque<usize>,
    end: usize,
    first_line: u64,
    generation: u64,
    version: Option<u64>,
    #[cfg(test)]
    pub scanned_lines: usize,
}

impl Layout {
    pub fn sync(&mut self, editor: &Editor, view: &View, width: usize) {
        let width = width.max(1);
        let reset = self.width != width
            || self.generation != view.generation
            || view.first_line < self.first_line;
        if !reset && self.version == Some(editor.content_version) {
            return;
        }
        if reset {
            self.starts.clear();
            self.end = 0;
            self.first_line = view.first_line;
            self.width = width;
            self.generation = view.generation;
        } else {
            while self.first_line < view.first_line && !self.starts.is_empty() {
                self.starts.pop_front();
                self.first_line += 1;
            }
            self.first_line = view.first_line;
            // Each log batch replaces the previously empty trailing line.
            if let Some(last) = self.starts.pop_back() {
                self.end = last;
            }
        }
        for line in editor.rope.lines_at(self.starts.len()) {
            let body = line.slice(..Editor::line_content_len(line));
            let text = body
                .as_str()
                .map(std::borrow::Cow::Borrowed)
                .unwrap_or_else(|| std::borrow::Cow::Owned(body.to_string()));
            self.starts.push_back(self.end);
            self.end += segments(&text, width).count();
            #[cfg(test)]
            {
                self.scanned_lines += 1;
            }
        }
        self.version = Some(editor.content_version);
    }

    pub fn total(&self) -> usize {
        self.starts.front().map_or(0, |first| self.end - first)
    }

    pub fn line_at(&self, row: usize) -> Option<(usize, usize)> {
        let absolute = self.starts.front()? + row;
        if absolute >= self.end {
            return None;
        }
        let line = self
            .starts
            .partition_point(|&start| start <= absolute)
            .saturating_sub(1);
        Some((line, absolute - self.starts[line]))
    }

    pub fn row_for_line(&self, line: usize) -> usize {
        self.starts
            .get(line)
            .map_or(self.total(), |start| start - self.starts[0])
    }

    pub fn segment_at(&self, editor: &Editor, row: usize) -> Option<(usize, Segment)> {
        let (line, offset) = self.line_at(row)?;
        let body = editor.rope.line(line);
        let body = body.slice(..Editor::line_content_len(body));
        let text = body
            .as_str()
            .map(std::borrow::Cow::Borrowed)
            .unwrap_or_else(|| std::borrow::Cow::Owned(body.to_string()));
        segments(&text, self.width)
            .nth(offset)
            .map(|segment| (line, segment))
    }

    pub fn row_for_position(&self, editor: &Editor, line: usize, column: usize) -> usize {
        let Some(body) = editor.rope.get_line(line) else {
            return self.total().saturating_sub(1);
        };
        let length = Editor::line_content_len(body);
        let body = body.slice(..length);
        let text = body
            .as_str()
            .map(std::borrow::Cow::Borrowed)
            .unwrap_or_else(|| std::borrow::Cow::Owned(body.to_string()));
        let mut offset = 0;
        for (index, segment) in segments(&text, self.width).enumerate() {
            offset = index;
            if column < segment.end || segment.end == length {
                break;
            }
        }
        self.row_for_line(line) + offset
    }
}
