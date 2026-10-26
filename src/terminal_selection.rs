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
pub(super) struct Selection {
    anchor: Position,
    head: Position,
    active: bool,
}

impl Selection {
    fn bounds(self) -> (Position, Position) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
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
        self.selection = self
            .selection_position(row, column)
            .map(|position| Selection {
                anchor: position,
                head: position,
                active: false,
            });
    }

    pub(crate) fn extend_selection(&mut self, row: usize, column: usize) {
        let Some(position) = self.selection_position(row, column) else {
            return;
        };
        if let Some(selection) = &mut self.selection {
            selection.head = position;
            selection.active |= position != selection.anchor;
        }
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
            let (cells, wrapped): (&[TermCell], bool) = if index < self.scrollback.len() {
                (
                    self.scrollback.get(index)?,
                    *self.scrollback_wrapped.get(index)?,
                )
            } else {
                let row = index - self.scrollback.len();
                (self.cells.get(row)?, *self.wrapped.get(row)?)
            };
            let left = if index == start.row { start.column } else { 0 };
            let right = if index == end.row {
                end.column.saturating_add(1).min(cells.len())
            } else {
                cells.len()
            };
            let selected = cells.get(left..right)?;
            let content_end = if wrapped { selected.len() } else {
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
                selection.anchor.row -= removed;
                selection.head.row -= removed;
            }
        }
    }
}
