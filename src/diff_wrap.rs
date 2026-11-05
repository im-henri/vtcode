use std::sync::{Arc, Weak};

use unicode_width::UnicodeWidthStr;

use crate::app::{AlignedDiffRow, DiffRenderCache};
use crate::editor::Editor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Widths {
    pub left: usize,
    pub right: usize,
}

impl Widths {
    pub fn new(editor: &Editor, left: usize, right: usize) -> Option<Self> {
        let limit = |width: usize| {
            if editor.word_wrap_column > 0 {
                width.min(editor.word_wrap_column).max(1)
            } else {
                width.max(1)
            }
        };
        (editor.word_wrap && !editor.large_file_mode).then(|| Self {
            left: limit(left),
            right: limit(right),
        })
    }
}

type Columns = (usize, usize);

struct Line {
    logical: usize,
    first: usize,
    original: Vec<Columns>,
    target: Vec<Columns>,
}

impl Line {
    fn rows(&self) -> usize {
        self.original.len().max(self.target.len())
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) struct Row {
    pub logical: usize,
    pub continuation: usize,
    original: Option<Columns>,
    target: Option<Columns>,
}

impl Row {
    pub fn columns(self, original: bool) -> Option<Columns> {
        if original { self.original } else { self.target }
    }

    pub fn padding(self, original: bool) -> bool {
        self.continuation > 0 && self.columns(original).is_none()
    }

    pub fn target(self, cache: &DiffRenderCache, aligned: bool) -> Option<&AlignedDiffRow> {
        if self.padding(false) {
            return None;
        }
        if aligned {
            cache.rows.get(self.logical)
        } else {
            let entry = cache
                .inline_rows
                .get(self.logical)
                .filter(|entry| entry.kind != '-')?;
            cache.rows.get(entry.aligned_row)
        }
    }

    pub fn cell_column(self, row: &AlignedDiffRow, original: bool, column: usize) -> usize {
        let start = self.columns(original).map_or(0, |(start, _)| start);
        let cell = |column| {
            if original {
                row.left_cell_column(column)
            } else {
                row.right_cell_column(column)
            }
        };
        cell(column).saturating_sub(cell(start))
    }

    pub fn column_at_cell(self, row: &AlignedDiffRow, original: bool, desired: usize) -> usize {
        let length = if original {
            row.left_char_len()
        } else {
            row.right_char_len()
        };
        let (start, end) = self.columns(original).unwrap_or((0, length));
        let cell = |column| {
            if original {
                row.left_cell_column(column)
            } else {
                row.right_cell_column(column)
            }
        };
        let last_cell = cell(end).saturating_sub(usize::from(end < length));
        let goal = cell(start).saturating_add(desired).min(last_cell);
        if original {
            row.left_column_at_cell(goal)
        } else {
            row.right_column_at_cell(goal)
        }
    }

    pub fn window(
        self,
        row: &AlignedDiffRow,
        original: bool,
        aligned: bool,
        pan: usize,
        width: usize,
    ) -> (&str, usize, usize) {
        if let Some((start, end)) = self.columns(original) {
            let text = if original {
                row.left_text_window(start, end - start)
            } else {
                row.right_text_window(start, end - start)
            };
            (text, start, 0)
        } else if aligned {
            let text = if original {
                row.left_text_window(pan, width)
            } else {
                row.right_text_window(pan, width)
            };
            (text, pan, 0)
        } else if original {
            row.left_inline_text_window(pan, width)
        } else {
            row.right_inline_text_window(pan, width)
        }
    }
}

/// Keep entries only for rows with continuations. All other diff rows keep
/// their original text indexes and need no additional storage.
#[derive(Default)]
pub(crate) struct Layout {
    widths: Option<Widths>,
    aligned: bool,
    cache: Weak<DiffRenderCache>,
    lines: Vec<Line>,
}

impl Layout {
    pub fn enabled(&self) -> bool {
        self.widths.is_some()
    }

    pub fn widths(&self) -> Option<Widths> {
        self.widths
    }

    pub fn update(
        &mut self,
        cache: &Arc<DiffRenderCache>,
        aligned: bool,
        widths: Option<Widths>,
    ) -> bool {
        if self.cache.as_ptr() == Arc::as_ptr(cache)
            && self.aligned == aligned
            && self.widths == widths
        {
            return false;
        }
        self.cache = Arc::downgrade(cache);
        self.aligned = aligned;
        self.widths = widths;
        self.lines.clear();
        let Some(widths) = widths else {
            return true;
        };
        if cache.max_inline_text_width <= widths.right && cache.max_left_text_width <= widths.left {
            return true;
        }
        let mut extra = 0;
        let count = if aligned {
            cache.rows.len()
        } else {
            cache.inline_rows.len()
        };
        for logical in 0..count {
            let (row, original, target) = if aligned {
                (&cache.rows[logical], true, true)
            } else {
                let entry = &cache.inline_rows[logical];
                (
                    &cache.rows[entry.aligned_row],
                    entry.kind == '-',
                    entry.kind != '-',
                )
            };
            if row.hidden_lines > 0 {
                continue;
            }
            let original = if original && row.left_cell_len() > widths.left {
                columns(&row.left_text, row.left_cell_len(), widths.left)
            } else {
                Vec::new()
            };
            let target = if target && row.right_cell_len() > widths.right {
                columns(&row.right_text, row.right_cell_len(), widths.right)
            } else {
                Vec::new()
            };
            let line = Line {
                logical,
                first: logical + extra,
                original,
                target,
            };
            if line.rows() > 1 {
                extra += line.rows() - 1;
                self.lines.push(line);
            }
        }
        true
    }

    pub fn first_row(&self, logical: usize) -> usize {
        let index = self.lines.partition_point(|line| line.logical < logical);
        logical
            + self
                .lines
                .get(index.wrapping_sub(1))
                .map_or(0, |line| line.first + line.rows() - line.logical - 1)
    }

    pub fn row_for_column(&self, logical: usize, original: bool, column: usize) -> usize {
        let first = self.first_row(logical);
        let Ok(index) = self
            .lines
            .binary_search_by_key(&logical, |line| line.logical)
        else {
            return first;
        };
        let line = &self.lines[index];
        let columns = if original {
            &line.original
        } else {
            &line.target
        };
        first
            + columns
                .partition_point(|&(_, end)| end <= column)
                .min(columns.len().saturating_sub(1))
    }

    pub fn row_at(&self, visual: usize) -> Row {
        let index = self.lines.partition_point(|line| line.first <= visual);
        if let Some(line) = self.lines.get(index.wrapping_sub(1)) {
            let offset = visual - line.first;
            if offset < line.rows() {
                return Row {
                    logical: line.logical,
                    continuation: offset,
                    original: line.original.get(offset).copied(),
                    target: line.target.get(offset).copied(),
                };
            }
            return Row {
                logical: line.logical + offset + 1 - line.rows(),
                continuation: 0,
                original: None,
                target: None,
            };
        }
        Row {
            logical: visual,
            continuation: 0,
            original: None,
            target: None,
        }
    }

    /// First logical row whose start lies at or below this visual boundary.
    pub fn logical_boundary(&self, visual: usize) -> usize {
        let row = self.row_at(visual);
        row.logical + usize::from(row.continuation > 0)
    }
}

fn columns(text: &str, cells: usize, width: usize) -> Vec<Columns> {
    if text.len() == cells && text.is_ascii() {
        // Use the editor's fast word-wrap path for ordinary source code.
        Editor::wrap_ranges_for_line_text_tabbed(text, width, 1)
    } else {
        crate::text_wrap::segments(text, width, |glyph, _| {
            if glyph.contains(char::is_control) {
                0
            } else {
                glyph.width()
            }
        })
        .map(|segment| (segment.start, segment.end))
        .collect()
    }
}
