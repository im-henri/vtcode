//! Search the retained terminal grid, joining soft wraps but not hard newlines.
//! Row identities/generations avoid rescanning unchanged output on each frame.
use super::{TermGrid, TermRow};
use rustc_hash::{FxHashMap, FxHashSet};

const MAX_MATCHES: usize = 20_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Point {
    pub row: usize,
    pub column: usize,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Hit {
    pub start: Point,
    pub end: Point,
    anchor: (usize, usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Stamp {
    id: usize,
    generation: u64,
    width: usize,
    wrapped: bool,
}

struct Line {
    rows: Vec<Stamp>,
    matches: Vec<(Point, Point)>,
}

#[derive(Default)]
pub(crate) struct Index {
    query: String,
    case_sensitive: bool,
    text_revision: Option<u64>,
    regex: Option<regex::Regex>,
    rows: Vec<Stamp>,
    lines: FxHashMap<usize, Line>,
    pub matches: Vec<Hit>,
    pub selected: Option<usize>,
    pub limited: bool,
    pub error: Option<String>,
    #[cfg(test)]
    pub rescanned: usize,
    #[cfg(test)]
    pub stamp_builds: usize,
}

fn row(grid: &TermGrid, index: usize) -> Option<(&TermRow, bool)> {
    if index < grid.scrollback.len() {
        Some((
            grid.scrollback.get(index)?,
            grid.scrollback_wrapped.get(index).copied().unwrap_or(false),
        ))
    } else {
        let index = index - grid.scrollback.len();
        Some((
            grid.cells.get(index)?,
            grid.wrapped.get(index).copied().unwrap_or(false),
        ))
    }
}

fn scan(grid: &TermGrid, start: usize, stamps: &[Stamp], regex: &regex::Regex) -> Line {
    let mut text = String::new();
    let mut positions = Vec::new();
    for (relative, stamp) in stamps.iter().enumerate() {
        let Some((cells, _)) = row(grid, start + relative) else {
            continue;
        };
        let width = if stamp.wrapped {
            stamp.width
        } else {
            cells[..stamp.width]
                .iter()
                .rposition(|cell| cell.ch != ' ' && cell.ch != '\0')
                .map_or(0, |column| column + 1)
        };
        for (column, cell) in cells[..width].iter().enumerate() {
            if cell.ch == '\0' {
                continue;
            }
            // Coordinates follow grid cells, rather than UTF-8 bytes or the
            // glyph's Unicode width. Include any explicit continuation cells.
            let mut end = column + 1;
            while end < stamp.width && cells[end].ch == '\0' {
                end += 1;
            }
            positions.push((
                text.len(),
                Point {
                    row: relative,
                    column,
                },
                Point {
                    row: relative,
                    column: end,
                },
            ));
            text.push(cell.ch);
        }
    }
    let mut matches = std::collections::VecDeque::new();
    for hit in regex.find_iter(&text) {
        let Some(start) = positions
            .partition_point(|(byte, _, _)| *byte <= hit.start())
            .checked_sub(1)
        else {
            continue;
        };
        let Some(end) = positions
            .partition_point(|(byte, _, _)| *byte < hit.end())
            .checked_sub(1)
        else {
            continue;
        };
        if matches.len() == MAX_MATCHES + 1 {
            matches.pop_front();
        }
        matches.push_back((positions[start].1, positions[end].2));
    }
    Line {
        rows: stamps.to_vec(),
        matches: matches.into(),
    }
}

impl Index {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn update(&mut self, grid: &TermGrid, query: &str, case_sensitive: bool) -> bool {
        if query.is_empty() {
            if self.query.is_empty() {
                return false;
            }
            self.clear();
            return true;
        }
        let changed_query = self.query != query || self.case_sensitive != case_sensitive;
        if !changed_query && self.text_revision == Some(grid.text_revision) {
            return false;
        }
        self.text_revision = Some(grid.text_revision);
        if changed_query {
            self.query = query.to_owned();
            self.case_sensitive = case_sensitive;
            self.lines.clear();
            self.rows.clear();
            let regex = regex::RegexBuilder::new(&regex::escape(query))
                .case_insensitive(!case_sensitive)
                .build();
            self.error = regex
                .as_ref()
                .err()
                .map(|_| "Search text is too long".to_owned());
            self.regex = regex.ok();
        }
        #[cfg(test)]
        {
            self.stamp_builds += 1;
        }
        let rows: Vec<_> = (0..grid.scrollback.len() + grid.cells.len())
            .filter_map(|index| {
                let (row, wrapped) = row(grid, index)?;
                Some(Stamp {
                    id: row.identity(),
                    generation: row.generation(),
                    width: row.len().min(grid.width),
                    wrapped,
                })
            })
            .collect();
        if !changed_query && rows == self.rows {
            return false;
        }
        let anchor = self
            .selected
            .and_then(|index| self.matches.get(index))
            .map(|hit| hit.anchor);
        self.matches.clear();
        self.limited = false;
        let mut groups = Vec::new();
        let mut first = 0;
        for (index, stamp) in rows.iter().enumerate() {
            if !stamp.wrapped || index + 1 == rows.len() {
                groups.push(first..index + 1);
                first = index + 1;
            }
        }
        let mut used = FxHashSet::default();
        if let Some(regex) = &self.regex {
            'groups: for group in groups.into_iter().rev() {
                let stamps = &rows[group.clone()];
                let id = stamps[0].id;
                used.insert(id);
                if self.lines.get(&id).is_none_or(|line| line.rows != stamps) {
                    self.lines
                        .insert(id, scan(grid, group.start, stamps, regex));
                    #[cfg(test)]
                    {
                        self.rescanned += 1;
                    }
                }
                for &(start, end) in self.lines[&id].matches.iter().rev() {
                    if self.matches.len() == MAX_MATCHES {
                        self.limited = true;
                        break 'groups;
                    }
                    self.matches.push(Hit {
                        start: Point {
                            row: group.start + start.row,
                            column: start.column,
                        },
                        end: Point {
                            row: group.start + end.row,
                            column: end.column,
                        },
                        anchor: (stamps[start.row].id, start.column),
                    });
                }
            }
        }
        self.lines.retain(|id, _| used.contains(id));
        self.matches.reverse();
        self.selected = anchor
            .and_then(|anchor| self.matches.iter().position(|hit| hit.anchor == anchor))
            .or_else(|| {
                self.matches.iter().position(|hit| {
                    hit.start.row >= grid.scrollback.len().saturating_sub(grid.scroll_view)
                })
            })
            .or_else(|| self.matches.len().checked_sub(1));
        self.rows = rows;
        true
    }

    pub fn navigate(&mut self, previous: bool) {
        let count = self.matches.len();
        if count == 0 {
            self.selected = None;
            return;
        }
        let index = self.selected.unwrap_or(0).min(count - 1);
        self.selected = Some(if previous {
            (index + count - 1) % count
        } else {
            (index + 1) % count
        });
    }

    pub fn reveal(&self, grid: &mut TermGrid) {
        if let Some(hit) = self.selected.and_then(|index| self.matches.get(index)) {
            grid.clear_selection();
            grid.scroll_view = grid
                .scrollback
                .len()
                .saturating_sub(hit.start.row.saturating_sub(grid.height / 2));
        }
    }
}

#[cfg(test)]
#[path = "terminal_find_tests.rs"]
mod tests;
