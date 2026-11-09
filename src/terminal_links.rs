//! File links are discovered only in the hovered logical line. Neither output
//! ingestion nor idle terminal frames scan history or access the filesystem.
use super::{TermGrid, TermRow};
use crate::terminal::find::Point;
use std::{ops::Range, sync::OnceLock};

const MAX_ROWS: usize = 128;
const MAX_CELLS: usize = 8192;
const MAX_LINKS: usize = 64;
const MAX_PATH_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Location {
    pub path: String,
    // Editor coordinates are zero based; terminal diagnostics are one based.
    pub line: Option<u32>,
    pub column: u32,
}

#[derive(Clone, Debug)]
pub(crate) struct Link {
    pub location: Location,
    pub start: Point,
    pub end: Point,
}

struct Parsed {
    range: Range<usize>,
    location: Location,
}

fn location_suffix(text: &str) -> Option<(u32, u32, usize)> {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| regex::Regex::new(concat!(
        r"(?i)^\s*(?:,\s*)?(?:",
        r"line\s+(?P<line>[0-9]{1,10})(?:\s*,?\s*(?:column|col)\s+(?P<column>[0-9]{1,10}))?",
        r"|:\s*(?P<colon_line>[0-9]{1,10})(?:\s*:\s*(?P<colon_column>[0-9]{1,10}))?",
        r"|\(\s*(?P<paren_line>[0-9]{1,10})(?:\s*,\s*(?P<paren_column>[0-9]{1,10}))?\s*\))"
    )).unwrap());
    let capture = pattern.captures(text)?;
    let number = |names: &[&str]| names.iter().find_map(|name| capture.name(name))
        .and_then(|value| value.as_str().parse::<u32>().ok())?.checked_sub(1);
    let line = number(&["line", "colon_line", "paren_line"])?;
    let column = if ["column", "colon_column", "paren_column"].iter().any(|name| capture.name(name).is_some()) {
        number(&["column", "colon_column", "paren_column"])?
    } else { 0 };
    Some((line, column, capture.get(0)?.end()))
}

fn file_shape(path: &str) -> bool {
    if path.is_empty() || path.len() > MAX_PATH_BYTES || path.contains("://")
        || path.chars().any(char::is_control) || path.ends_with(['/', '\\'])
        || matches!(path, "." | ".." | "/" | "\\")
    { return false; }
    if path.contains(['/', '\\']) { return true; }
    if matches!(path, "Makefile" | "Dockerfile" | "CMakeLists.txt" | "BUILD" | "WORKSPACE") { return true; }
    path.rsplit_once('.').is_some_and(|(name, extension)| (!name.is_empty() || path.starts_with('.'))
        && extension.len() <= 24
        && extension.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"_+-".contains(&byte)))
}

fn parse_candidate(text: &str, path: Range<usize>, range: Range<usize>) -> Option<Parsed> {
    let raw = &text[path.clone()];
    if raw.contains("://") { return None; }
    // A location can be inside the quotes/token, including Windows drive paths.
    for (offset, ch) in raw.char_indices() {
        if matches!(ch, ':' | '(') && file_shape(&raw[..offset])
            && let Some((line, column, length)) = location_suffix(&text[path.start + offset..])
        {
            let end = path.start + offset + length;
            if end < path.end && !text[end..path.end].starts_with([':', ')', ']', '}', ',', ';']) { continue; }
            return Some(Parsed { range: range.start..(path.start + offset + length).max(range.end),
                location: Location { path: raw[..offset].into(), line: Some(line), column } });
        }
    }
    if !file_shape(raw) { return None; }
    let location = location_suffix(&text[range.end..]);
    Some(Parsed { range: range.start..range.end + location.map_or(0, |(_, _, length)| length),
        location: Location { path: raw.into(), line: location.map(|(line, _, _)| line),
            column: location.map_or(0, |(_, column, _)| column) } })
}

fn parse(text: &str) -> Vec<Parsed> {
    static QUOTED: OnceLock<regex::Regex> = OnceLock::new();
    static TOKENS: OnceLock<regex::Regex> = OnceLock::new();
    let quoted = QUOTED.get_or_init(|| regex::Regex::new(r#""(?P<double>[^"\r\n]+)"|'(?P<single>[^'\r\n]+)'"#).unwrap());
    let tokens = TOKENS.get_or_init(|| regex::Regex::new(r#"[^\s"']+"#).unwrap());
    let mut links = Vec::new();
    let mut excluded = Vec::new();
    for capture in quoted.captures_iter(text).take(MAX_LINKS) {
        let whole = capture.get(0).unwrap().range();
        let path = capture.name("double").or_else(|| capture.name("single")).unwrap().range();
        if let Some(link) = parse_candidate(text, path, whole.clone()) { links.push(link); }
        excluded.push(whole);
    }
    let mut quote = 0;
    for token in tokens.find_iter(text) {
        if links.len() == MAX_LINKS { break; }
        while quote < excluded.len() && excluded[quote].end <= token.start() { quote += 1; }
        if excluded.get(quote).is_some_and(|range| range.start < token.end()) { continue; }
        let raw = token.as_str();
        let left = raw.trim_start_matches(['(', '[', '{', '<']);
        let start = token.start() + raw.len() - left.len();
        let value = left.trim_end_matches([':', ',', ';', '.', ']', '}', '>']);
        let end = start + value.len();
        if let Some(link) = parse_candidate(text, start..end, start..end) { links.push(link); }
        else {
            let value = value.trim_end_matches(')');
            let end = start + value.len();
            if let Some(link) = parse_candidate(text, start..end, start..end) { links.push(link); }
        }
    }
    links.sort_unstable_by_key(|link| link.range.start);
    links
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
