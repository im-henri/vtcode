//! Path links are discovered only in the hovered logical line. Neither output
//! ingestion nor idle terminal frames scan history or access the filesystem.
use super::{TermGrid, TermRow};
use crate::path_links::{Location, parse};
use crate::terminal::find::Point;

const MAX_ROWS: usize = 128;
const MAX_CELLS: usize = 8192;

#[derive(Clone, Debug)]
pub(crate) struct Link {
    pub location: Location,
    pub start: Point,
    pub end: Point,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp { id: usize, generation: u64, width: usize, wrapped: bool }

fn row(grid: &TermGrid, index: usize) -> Option<(&TermRow, bool)> {
    if index < grid.scrollback.len() {
        Some((grid.scrollback.get(index)?, grid.scrollback_wrapped.get(index).copied().unwrap_or(false)))
    } else {
        let index = index - grid.scrollback.len();
        Some((grid.cells.get(index)?, grid.wrapped.get(index).copied().unwrap_or(false)))
    }
}

fn stamp(grid: &TermGrid, index: usize) -> Option<Stamp> {
    let (row, wrapped) = row(grid, index)?;
    Some(Stamp { id: row.identity(), generation: row.generation(), width: row.len().min(grid.width), wrapped })
}

#[derive(Default)]
pub(crate) struct Cache {
    rows: Vec<Stamp>,
    text: String,
    positions: Vec<(usize, Point, Point)>,
    pub links: Vec<Link>,
    pub first: usize,
    #[cfg(test)]
    pub scans: usize,
    #[cfg(test)]
    pub inspected_rows: usize,
}

impl Cache {
    /// Only bounded row metadata is revisited on a cache hit. Output elsewhere
    /// in the terminal does not invalidate a stable hovered line.
    pub fn hit(&mut self, grid: &TermGrid, visual_row: usize, column: usize) -> Option<usize> {
        if visual_row >= grid.height || column >= grid.width { return None; }
        let absolute = grid.scrollback.len().saturating_sub(grid.scroll_view) + visual_row;
        let mut first = absolute;
        let mut visited = 0;
        while first > 0 && row(grid, first - 1).is_some_and(|(_, wrapped)| wrapped) {
            visited += 1;
            if visited >= MAX_ROWS { return None; }
            first -= 1;
        }
        let mut end = first;
        let mut cells = 0;
        loop {
            let current = stamp(grid, end)?;
            cells += current.width;
            #[cfg(test)] { self.inspected_rows += 1; }
            if end - first >= MAX_ROWS || cells > MAX_CELLS { return None; }
            end += 1;
            if !current.wrapped { break; }
        }
        let unchanged = self.rows.len() == end - first
            && self.rows.iter().enumerate().all(|(offset, old)| stamp(grid, first + offset) == Some(*old));
        self.first = first;
        if !unchanged {
            #[cfg(test)] { self.scans += 1; }
            self.rows.clear();
            self.text.clear();
            self.positions.clear();
            self.links.clear();
            for index in first..end {
                let (source, wrapped) = row(grid, index)?;
                let current = stamp(grid, index)?;
                self.rows.push(current);
                let width = if wrapped { current.width } else {
                    source[..current.width].iter().rposition(|cell| !matches!(cell.ch, ' ' | '\0')).map_or(0, |col| col + 1)
                };
                for (col, cell) in source[..width].iter().enumerate() {
                    if cell.ch == '\0' { continue; }
                    let mut stop = col + 1;
                    while stop < width && source[stop].ch == '\0' { stop += 1; }
                    self.positions.push((self.text.len(), Point { row: index - first, column: col },
                        Point { row: index - first, column: stop }));
                    self.text.push(cell.ch);
                }
            }
            for parsed in parse(&self.text) {
                let start = self.positions.partition_point(|(byte, _, _)| *byte <= parsed.range.start).checked_sub(1)?;
                let end = self.positions.partition_point(|(byte, _, _)| *byte < parsed.range.end).checked_sub(1)?;
                self.links.push(Link { location: parsed.location, start: self.positions[start].1, end: self.positions[end].2 });
            }
        }
        let point = (absolute - first, column);
        self.links.iter().position(|link| (link.start.row, link.start.column) <= point && point < (link.end.row, link.end.column))
    }
}

#[cfg(test)]
#[path = "terminal_links_tests.rs"]
mod tests;
