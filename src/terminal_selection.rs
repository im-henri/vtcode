//! Selection positions use the scrollback + live grid, rather than viewport rows.
use super::{TermCell, TermGrid};

#[cfg(test)]
#[path = "terminal_selection_reference.rs"]
mod reference;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct Position {
    row: usize,
    column: usize,
}

#[derive(Clone, Copy)]
enum Unit {
    Character,
    Word,
    Line,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Extent {
    start: Position,
    end: Position,
}

#[derive(Clone, Copy)]
pub(super) struct Selection {
    anchor: Extent,
    head: Extent,
    unit: Unit,
    active: bool,
}

impl Selection {
    fn bounds(self) -> (Position, Position) {
        (self.anchor.start.min(self.head.start), self.anchor.end.max(self.head.end))
    }
}

impl TermGrid {
    fn selection_position(&self, row: usize, column: usize) -> Option<Position> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        Some(Position {
            row: self.scrollback.len().saturating_sub(self.scroll_view) + row.min(self.height - 1),
            column: column.min(self.width - 1),
        })
    }

    pub(crate) fn begin_selection(&mut self, row: usize, column: usize) {
        self.begin_selection_unit(row, column, Unit::Character);
    }

    pub(crate) fn begin_word_selection(&mut self, row: usize, column: usize) {
        self.begin_selection_unit(row, column, Unit::Word);
    }

    pub(crate) fn begin_line_selection(&mut self, row: usize, column: usize) {
        self.begin_selection_unit(row, column, Unit::Line);
    }

    fn begin_selection_unit(&mut self, row: usize, column: usize, unit: Unit) {
        self.selection = self
            .selection_position(row, column)
            .and_then(|position| self.selection_extent(position, unit))
            .map(|extent| Selection {
                anchor: extent,
                head: extent,
                unit,
                active: !matches!(unit, Unit::Character),
            });
    }

    pub(crate) fn extend_selection(&mut self, row: usize, column: usize) {
        let Some(unit) = self.selection.map(|selection| selection.unit) else { return; };
        let Some(position) = self.selection_position(row, column) else {
            return;
        };
        let Some(extent) = self.selection_extent(position, unit) else { return; };
        if let Some(selection) = &mut self.selection {
            selection.head = extent;
            selection.active |= extent != selection.anchor;
        }
    }

    fn selection_row(&self, index: usize) -> Option<(&[TermCell], bool)> {
        if index < self.scrollback.len() {
            Some((self.scrollback.get(index)?, *self.scrollback_wrapped.get(index)?))
        } else {
            let row = index - self.scrollback.len();
            Some((self.cells.get(row)?, *self.wrapped.get(row)?))
        }
    }

    fn selection_neighbor(&self, position: Position, forward: bool) -> Option<Position> {
        let (cells, wrapped) = self.selection_row(position.row)?;
        if forward {
            if position.column + 1 < cells.len().min(self.width) {
                Some(Position { column: position.column + 1, ..position })
            } else if wrapped && !self.selection_row(position.row + 1)?.0.is_empty() {
                Some(Position { row: position.row + 1, column: 0 })
            } else { None }
        } else if position.column > 0 {
            Some(Position { column: position.column - 1, ..position })
        } else if position.row > 0 {
            let (previous, wrapped) = self.selection_row(position.row - 1)?;
            (wrapped && !previous.is_empty()).then(|| Position {
                row: position.row - 1, column: previous.len().min(self.width) - 1,
            })
        } else { None }
    }

    fn selection_extent(&self, position: Position, unit: Unit) -> Option<Extent> {
        let mut extent = Extent { start: position, end: position };
        match unit {
            Unit::Character => {}
            Unit::Word => {
                // Terminal separators leave identifiers, paths and URLs intact.
                let class = |ch: char| if ch == '\0' || ch.is_whitespace() { 0 }
                    else if " ()[]{}',\"`".contains(ch) { 1 } else { 2 };
                let cell_class = |at: Position| self.selection_row(at.row)?.0.get(at.column).map(|cell| class(cell.ch));
                let kind = cell_class(position)?;
                if kind != 1 {
                    while let Some(previous) = self.selection_neighbor(extent.start, false) {
                        if cell_class(previous) != Some(kind) { break; }
                        extent.start = previous;
                    }
                    while let Some(next) = self.selection_neighbor(extent.end, true) {
                        if cell_class(next) != Some(kind) { break; }
                        extent.end = next;
                    }
                }
            }
            Unit::Line => {
                while extent.start.row > 0 && self.selection_row(extent.start.row - 1)?.1 {
                    extent.start.row -= 1;
                }
                while self.selection_row(extent.end.row)?.1
                    && self.selection_row(extent.end.row + 1).is_some() {
                    extent.end.row += 1;
                }
                extent.start.column = 0;
                extent.end.column = self.selection_row(extent.end.row)?.0.len().min(self.width).saturating_sub(1);
            }
        }
        Some(extent)
    }

    pub(crate) fn finish_selection(&mut self) {
        if !self.has_selection() {
            self.clear_selection();
        }
    }

    pub(crate) fn clear_selection(&mut self) {
        self.selection = None;
    }

    pub(crate) fn has_selection(&self) -> bool {
        self.selection.is_some_and(|selection| selection.active)
    }

    #[cfg(test)]
    pub(crate) fn selection_contains(&self, row: usize, column: usize) -> bool {
        let Some(selection) = self.selection.filter(|selection| selection.active) else {
            return false;
        };
        let Some(position) = self.selection_position(row, column) else {
            return false;
        };
        let (start, end) = selection.bounds();
        position >= start && position <= end
    }

    /// Resolve a row once for rendering instead of rebasing and ordering the
    /// selection for every cell. The returned columns are half-open.
    pub(crate) fn selection_columns(&self, row: usize) -> std::ops::Range<usize> {
        let Some(selection) = self.selection.filter(|selection| selection.active) else {
            return 0..0;
        };
        let Some(position) = self.selection_position(row, 0) else {
            return 0..0;
        };
        let (start, end) = selection.bounds();
        if position.row < start.row || position.row > end.row {
            return 0..0;
        }
        let left = if position.row == start.row { start.column } else { 0 };
        let right = if position.row == end.row { end.column + 1 } else { self.width };
        left..right.min(self.width)
    }

    /// Copy plain text; soft wrapping never inserts an artificial newline.
    pub(crate) fn selected_text(&self) -> Option<String> {
        let selection = self.selection.filter(|selection| selection.active)?;
        let (start, end) = selection.bounds();
        let mut result = String::new();
        for index in start.row..=end.row {
            let (cells, wrapped) = self.selection_row(index)?;
            let left = if index == start.row { start.column } else { 0 };
            let right = if index == end.row {
                end.column.saturating_add(1).min(cells.len())
            } else {
                cells.len()
            };
            let selected = cells.get(left..right)?;
            let preserve_spaces = matches!(selection.unit, Unit::Word)
                && cells[right..].iter().any(|cell| !matches!(cell.ch, ' ' | '\0'));
            let content_end = if wrapped || preserve_spaces { selected.len() } else {
                selected.iter().rposition(|cell| !matches!(cell.ch, ' ' | '\0'))
                    .map_or(0, |last| last + 1)
            };
            result.extend(selected[..content_end].iter()
                .map(|cell| if cell.ch == '\0' { ' ' } else { cell.ch }));
            if !wrapped && index < end.row {
                result.push('\n');
            }
        }
        Some(result)
    }

    /// Mutable screen rows must never silently retarget a selection after a redraw.
    pub(super) fn invalidate_selection_rows(&mut self, first: usize, last: usize) {
        let Some(selection) = self.selection else {
            return;
        };
        let (start, end) = selection.bounds();
        let first = self.scrollback.len() + first;
        let last = self.scrollback.len() + last;
        if start.row <= last && end.row >= first {
            self.clear_selection();
        }
    }

    pub(super) fn trim_selection(&mut self, removed: usize) {
        if removed == 0 {
            return;
        }
        if let Some(selection) = &mut self.selection {
            if selection.bounds().0.row < removed {
                self.clear_selection();
            } else {
                selection.anchor.start.row -= removed;
                selection.anchor.end.row -= removed;
                selection.head.start.row -= removed;
                selection.head.end.row -= removed;
            }
        }
    }
}
