//! Selection belongs to the immutable comparison, including original/deleted
//! text that has no corresponding position in the target editor.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{Frame, layout::Rect};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::{ActivePane, AlignedDiffRow, App, DiffMode, DiffRenderCache};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Domain {
    Original,
    Target,
    Inline,
}

#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct Position {
    row: usize,
    column: usize,
}

#[derive(Clone, Copy, Default)]
struct Geometry {
    area: Rect,
    first: usize,
    pan: usize,
}

#[derive(Clone, Copy, Default)]
enum SelectionUnit {
    #[default]
    Character,
    Word {
        start: Position,
        end: Position,
    },
    Line {
        start: Position,
        end: Position,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct NativeCursor {
    position: (usize, usize),
    anchor: Option<(usize, usize)>,
    extra_count: usize,
}

struct SourceLine<'a> {
    row: Option<&'a AlignedDiffRow>,
    text: &'a str,
    len: usize,
    original: bool,
    source: Option<usize>,
}

impl<'a> SourceLine<'a> {
    fn chars(&self, start: usize, count: usize) -> &'a str {
        let Some(row) = self.row else {
            return "";
        };
        if self.original {
            row.left_text_window(start, count)
        } else {
            row.right_text_window(start, count)
        }
    }

    fn byte_at(&self, column: usize) -> usize {
        if column >= self.len {
            return self.text.len();
        }
        self.chars(column, 1).as_ptr() as usize - self.text.as_ptr() as usize
    }

    fn cell_column(&self, column: usize) -> usize {
        self.row.map_or(0, |row| {
            if self.original {
                row.left_cell_column(column)
            } else {
                row.right_cell_column(column)
            }
        })
    }

    fn column_at_cell(&self, cell: usize) -> usize {
        self.row.map_or(0, |row| {
            if self.original { row.left_column_at_cell(cell) }
            else { row.right_column_at_cell(cell) }
        })
    }

    fn window(&self, domain: Domain, geometry: Geometry) -> (&'a str, usize, usize) {
        if domain == Domain::Inline {
            self.row.map_or(("", 0, 0), |row| {
                if self.original {
                    row.left_inline_text_window(geometry.pan, geometry.area.width as usize)
                } else {
                    row.right_inline_text_window(geometry.pan, geometry.area.width as usize)
                }
            })
        } else {
            (
                self.chars(geometry.pan, geometry.area.width as usize),
                geometry.pan,
                0,
            )
        }
    }
}

#[derive(Default)]
pub(crate) struct DiffSelection {
    cache: Weak<DiffRenderCache>,
    source: Option<ropey::Rope>,
    navigation_id: u64,
    content_version: u64,
    native_cursor: Option<NativeCursor>,
    mode: Option<DiffMode>,
    original: Geometry,
    target: Geometry,
    inline: Geometry,
    original_terminal: bool,
    target_terminal: bool,
    focus: Option<Domain>,
    anchor: Position,
    cursor: Position,
    desired: Option<usize>,
    dragging: bool,
    unit: SelectionUnit,
    last_click: Option<(Instant, u16, u16, Domain, u8)>,
}

impl DiffSelection {
    pub(crate) fn copy_animation_key(&self) -> Option<(usize, u8, usize, usize, usize, usize)> {
        let focus = self.focus?;
        if self.anchor == self.cursor { return None; }
        Some((self.cache.as_ptr() as usize, focus as u8,
            self.anchor.row, self.anchor.column, self.cursor.row, self.cursor.column))
    }

    pub(crate) fn cancel_drag(&mut self) {
        self.dragging = false;
    }

    fn fresh(&self, app: &App) -> bool {
        self.mode == Some(app.diff_mode)
            && self.cache.as_ptr() == Arc::as_ptr(&app.diff_render_cache)
            && app.should_render_diff_for_active_editor()
            && app.editor().is_some_and(|editor| {
                self.navigation_id == editor.navigation_id
                    && self.content_version == editor.content_version
                    && self
                        .source
                        .as_ref()
                        .is_some_and(|source| source.is_instance(&editor.rope))
                    && (self.focus.is_none() || self.native_cursor == Self::editor_cursor(app))
            })
    }

    fn editor_cursor(app: &App) -> Option<NativeCursor> {
        app.editor().map(|editor| NativeCursor {
            position: (editor.cursor_line, editor.cursor_col),
            anchor: editor.selection_anchor,
            extra_count: editor.extra_cursors.len(),
        })
    }

    fn refresh(&mut self, app: &App, aligned: bool) {
        if !self.fresh(app) {
            *self = Self::default();
            let Some(editor) = app.editor() else {
                return;
            };
            self.cache = Arc::downgrade(&app.diff_render_cache);
            self.source = Some(editor.rope.clone());
            self.navigation_id = editor.navigation_id;
            self.content_version = editor.content_version;
            self.mode = Some(app.diff_mode);
        }
        let Some(editor) = app.editor() else {
            return;
        };
        let cache = &app.diff_render_cache;
        let first = if aligned {
            app.aligned_diff_scroll_row(editor.scroll_offset)
        } else {
            app.diff_inline_scroll.row
        };
        let code = app.code_area.unwrap_or_default();
        let target_inset = if aligned && !app.compare_diff_lines.is_empty() {
            2
        } else {
            0
        };
        self.target = Geometry {
            area: Rect::new(
                code.x.saturating_add(target_inset).min(code.right()),
                code.y,
                code.width.saturating_sub(target_inset),
                code.height,
            ),
            first,
            pan: editor.horizontal_scroll,
        };
        self.original = Geometry {
            area: app.diff_left_code_area.unwrap_or_default(),
            first,
            pan: app.diff_left_horizontal_scroll,
        };
        self.inline = Geometry {
            area: code,
            first,
            pan: app.diff_inline_scroll.column,
        };
        let last = editor.line_count().saturating_sub(1);
        self.target_terminal = cache.context_lines.is_none()
            && last == cache.target_content_lines()
            && editor.rope.line(last).len_chars() == 0;
        self.original_terminal = cache.context_lines.is_none()
            && app
                .compare_base_content
                .as_ref()
                .is_some_and(|base| base.is_empty() || base.ends_with(['\n', '\r']));
        self.native_cursor = Self::editor_cursor(app);
    }

    fn geometry(&self, domain: Domain) -> Geometry {
        match domain {
            Domain::Original => self.original,
            Domain::Target => self.target,
            Domain::Inline => self.inline,
        }
    }

    fn terminal(&self, domain: Domain) -> bool {
        if domain == Domain::Original {
            self.original_terminal
        } else {
            self.target_terminal
        }
    }

    fn content_rows(cache: &DiffRenderCache, domain: Domain) -> usize {
        if domain == Domain::Inline {
            cache.inline_rows.len()
        } else {
            cache.rows.len()
        }
    }

    fn line<'a>(
        &self,
        cache: &'a DiffRenderCache,
        domain: Domain,
        index: usize,
    ) -> Option<SourceLine<'a>> {
        if index == Self::content_rows(cache, domain) && self.terminal(domain) {
            return Some(SourceLine {
                row: None,
                text: "",
                len: 0,
                original: domain == Domain::Original,
                source: (domain != Domain::Original).then_some(cache.target_content_lines()),
            });
        }
        let (row, original) = if domain == Domain::Inline {
            let entry = cache.inline_rows.get(index)?;
            (cache.rows.get(entry.aligned_row)?, entry.kind == '-')
        } else {
            (cache.rows.get(index)?, domain == Domain::Original)
        };
        if row.hidden_lines > 0 {
            return None;
        }
        let source = if original {
            row.left_line
        } else {
            row.right_line
        }?;
        Some(SourceLine {
            row: Some(row),
            original,
            source: Some(source),
            text: if original {
                &row.left_text
            } else {
                &row.right_text
            },
            len: if original {
                row.left_char_len()
            } else {
                row.right_char_len()
            },
        })
    }

    fn hit(
        &self,
        cache: &DiffRenderCache,
        domain: Domain,
        col: u16,
        row: u16,
        clamp: bool,
    ) -> Option<Position> {
        let geometry = self.geometry(domain);
        if geometry.area.width == 0 || geometry.area.height == 0 {
            return None;
        }
        if !clamp && !geometry.area.contains((col, row).into()) {
            return None;
        }
        let screen_row = row
            .saturating_sub(geometry.area.y)
            .min(geometry.area.height - 1) as usize;
        let clicked = geometry.first.saturating_add(screen_row);
        // Resolve alignment holes within the visible viewport, never by walking
        // a potentially enormous deleted/added run on every pointer sample.
        let index = if self.line(cache, domain, clicked).is_some() {
            clicked
        } else {
            (0..geometry.area.height as usize)
                .map(|offset| geometry.first.saturating_add(offset))
                .filter(|index| self.line(cache, domain, *index).is_some())
                .min_by_key(|index| index.abs_diff(clicked))?
        };
        let line = self.line(cache, domain, index)?;
        let cell = col.saturating_sub(geometry.area.x).min(geometry.area.width) as usize;
        let (text, mut source, padding) = line.window(domain, geometry);
        let mut x = padding;
        for grapheme in text.graphemes(true) {
            let width = rendered_width(grapheme);
            if width > 0 && cell < x.saturating_add(width) {
                return Some(Position {
                    row: index,
                    column: source.min(line.len),
                });
            }
            source += grapheme.chars().count();
            x += width;
        }
        Some(Position {
            row: index,
            column: source.min(line.len),
        })
    }

    fn sync_target(&mut self, app: &mut App) {
        let Some(domain) = self.focus else {
            return;
        };
        let cache = &app.diff_render_cache;
        let Some(cursor) = self.line(cache, domain, self.cursor.row) else {
            return;
        };
        if cursor.original {
            self.native_cursor = Self::editor_cursor(app);
            return;
        }
        let Some(line) = cursor.source else {
            return;
        };
        let anchor = (domain == Domain::Target)
            .then(|| self.line(cache, domain, self.anchor.row))
            .flatten()
            .and_then(|anchor| anchor.source.map(|line| (line, self.anchor.column)));
        if let Some(editor) = app.editor_mut() {
            editor.cursor_line = line.min(editor.line_count().saturating_sub(1));
            editor.cursor_col = self.cursor.column.min(editor.line_len(editor.cursor_line));
            editor.desired_col = None;
            editor.selection_anchor =
                anchor.filter(|anchor| *anchor != (editor.cursor_line, editor.cursor_col));
        }
        self.native_cursor = Self::editor_cursor(app);
    }

    fn source_position(
        &self,
        cache: &DiffRenderCache,
        domain: Domain,
        line: usize,
        column: usize,
    ) -> Option<Position> {
        if domain == Domain::Original {
            return None;
        }
        if self.target_terminal && line == cache.target_content_lines() {
            return Some(Position {
                row: Self::content_rows(cache, domain),
                column: 0,
            });
        }
        let aligned = cache.aligned_row_for_target_line(line);
        let row = if domain == Domain::Target {
            aligned
        } else {
            let first = cache
                .inline_rows
                .partition_point(|entry| entry.aligned_row < aligned);
            first + usize::from(cache.inline_rows.get(first)?.kind == '-')
        };
        let text = self.line(cache, domain, row)?;
        (!text.original && text.source == Some(line)).then_some(Position {
            row,
            column: column.min(text.len),
        })
    }

    fn range(&self) -> (Position, Position) {
        if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        }
    }

    fn boundary(&self, cache: &DiffRenderCache, domain: Domain, last: bool) -> Option<Position> {
        let terminal = self.terminal(domain).then_some(Position {
            row: Self::content_rows(cache, domain),
            column: 0,
        });
        if last && terminal.is_some() {
            return terminal;
        }
        let rows = cache.selectable_rows(domain != Domain::Inline, domain == Domain::Original);
        let row = if last { rows.last() } else { rows.first() };
        row.and_then(|&row| {
            self.line(cache, domain, row).map(|line| Position {
                row,
                column: if last { line.len } else { 0 },
            })
        })
        .or(terminal)
    }

    fn adjacent(
        &self,
        cache: &DiffRenderCache,
        domain: Domain,
        position: Position,
        down: bool,
    ) -> Option<Position> {
        let rows = cache.selectable_rows(domain != Domain::Inline, domain == Domain::Original);
        let next = if down {
            rows.get(rows.partition_point(|row| *row <= position.row))
                .copied()
                .or_else(|| {
                    (self.terminal(domain) && position.row < Self::content_rows(cache, domain))
                        .then_some(Self::content_rows(cache, domain))
                })
        } else {
            rows.partition_point(|row| *row < position.row)
                .checked_sub(1)
                .and_then(|index| rows.get(index))
                .copied()
        }?;
        self.line(cache, domain, next).map(|line| Position {
            row: next,
            column: position.column.min(line.len),
        })
    }

    fn paragraph(
        &self,
        cache: &DiffRenderCache,
        domain: Domain,
        position: Position,
        down: bool,
    ) -> Position {
        let Some(boundary) = self.boundary(cache, domain, down) else {
            return position;
        };
        if position.row == boundary.row {
            return position;
        }
        let rows = cache.selectable_rows(domain != Domain::Inline, domain == Domain::Original);
        let index = rows.partition_point(|row| {
            if down {
                *row <= position.row
            } else {
                *row < position.row
            }
        });
        let blank = |row: &usize| {
            (
                *row,
                self.line(cache, domain, *row)
                    .is_none_or(|line| line.text.trim().is_empty()),
            )
        };
        // Walk the cached selectable rows once; spacers and hidden context are
        // never cursor destinations, and no document copy is needed.
        let row = if down {
            paragraph_boundary(rows[index..].iter().map(blank), boundary.row)
        } else {
            paragraph_boundary(rows[..index].iter().rev().map(blank), boundary.row)
        };
        Position { row, column: 0 }
    }

    fn word_span(
        &self,
        cache: &DiffRenderCache,
        domain: Domain,
        position: Position,
    ) -> (Position, Position) {
        let Some(line) = self.line(cache, domain, position.row) else {
            return (position, position);
        };
        if line.len == 0 {
            return (position, position);
        }
        let column = position.column.min(line.len - 1);
        let byte = line.byte_at(column);
        let ch = line.text[byte..].chars().next().unwrap();
        let class = |ch: char| {
            if ch.is_alphanumeric() || ch == '_' {
                0
            } else if ch.is_whitespace() {
                1
            } else {
                2
            }
        };
        let target = class(ch);
        let before = line.text[..byte]
            .chars()
            .rev()
            .take_while(|ch| class(*ch) == target)
            .count();
        let after = line.text[byte..]
            .chars()
            .take_while(|ch| class(*ch) == target)
            .count();
        (
            Position {
                row: position.row,
                column: column - before,
            },
            Position {
                row: position.row,
                column: column + after,
            },
        )
    }

    fn line_span(
        &self,
        cache: &DiffRenderCache,
        domain: Domain,
        position: Position,
    ) -> (Position, Position) {
        let start = Position {
            row: position.row,
            column: 0,
        };
        let end = self
            .adjacent(cache, domain, position, true)
            .map(|next| Position {
                row: next.row,
                column: 0,
            })
            .unwrap_or(Position {
                row: position.row,
                column: self
                    .line(cache, domain, position.row)
                    .map_or(0, |line| line.len),
            });
        (start, end)
    }

    fn drag_position(&mut self, cache: &DiffRenderCache, domain: Domain, position: Position) {
        let (start, end, unit) = match self.unit {
            SelectionUnit::Character => {
                self.cursor = position;
                return;
            }
            SelectionUnit::Word { start, end } => (start, end, false),
            SelectionUnit::Line { start, end } => (start, end, true),
        };
        if position < start {
            self.anchor = end;
            self.cursor = if unit {
                self.line_span(cache, domain, position).0
            } else {
                self.word_span(cache, domain, position).0
            };
        } else if position > end || (unit && position == end) {
            self.anchor = start;
            self.cursor = if unit {
                self.line_span(cache, domain, position).1
            } else {
                self.word_span(cache, domain, position).1
            };
        } else {
            self.anchor = start;
            self.cursor = end;
        }
    }

    fn reveal(&mut self, app: &mut App) {
        let Some(domain) = self.focus else {
            return;
        };
        let geometry = self.geometry(domain);
        let height = geometry.area.height.max(1) as usize;
        let first = if self.cursor.row < geometry.first {
            self.cursor.row
        } else if self.cursor.row >= geometry.first.saturating_add(height) {
            self.cursor.row.saturating_add(1).saturating_sub(height)
        } else {
            geometry.first
        };
        if first != geometry.first {
            if app.row_diff_active() {
                let mut scroll = std::mem::take(&mut app.diff_inline_scroll);
                scroll.row = first.min(scroll.max_row());
                if let Some(editor) = app.editor_mut() {
                    scroll.sync_editor(editor);
                }
                app.diff_inline_scroll = scroll;
            } else {
                let source = app.diff_render_cache.target_line_for_aligned_row(first);
                if let Some(editor) = app.editor_mut() {
                    editor.scroll_offset = source.min(editor.line_count().saturating_sub(1));
                }
            }
        }
        let cache = &app.diff_render_cache;
        if let Some(line) = self.line(cache, domain, self.cursor.row)
            && caret_column(&line, domain, geometry, self.cursor.column).is_none()
        {
            let pan = if domain == Domain::Inline {
                line.cell_column(self.cursor.column)
                    .saturating_sub(geometry.area.width.saturating_sub(1) as usize)
            } else if self.cursor.column < geometry.pan {
                self.cursor.column
            } else {
                self.cursor
                    .column
                    .saturating_sub(geometry.area.width.saturating_sub(1) as usize)
            };
            if domain == Domain::Original {
                app.diff_left_horizontal_scroll = pan;
            } else if let Some(editor) = app.editor_mut() {
                editor.horizontal_scroll = pan;
            }
        }
        self.refresh(app, domain != Domain::Inline);
    }
}

fn word_boundary(line: &SourceLine<'_>, column: usize, right: bool) -> usize {
    let column = column.min(line.len);
    let byte = line.byte_at(column);
    let word = |c: char| c.is_alphanumeric() || c == '_';
    if right {
        let mut chars = line.text[byte..].chars().peekable();
        let Some(&first) = chars.peek() else {
            return column;
        };
        let mut end = column;
        let mut previous = None;
        for ch in chars {
            let same = if first.is_whitespace() {
                ch.is_whitespace()
            } else if word(first) {
                word(ch)
                    && !(ch.is_uppercase()
                        && previous.is_some_and(|p: char| p.is_lowercase() || p.is_ascii_digit()))
            } else {
                !word(ch) && !ch.is_whitespace()
            };
            if !same {
                break;
            }
            previous = Some(ch);
            end += 1;
        }
        end
    } else {
        let mut chars = line.text[..byte].chars().rev().peekable();
        let mut start = column;
        while chars.peek().is_some_and(|ch| ch.is_whitespace()) {
            chars.next();
            start -= 1;
        }
        let Some(&first) = chars.peek() else {
            return start;
        };
        let mut previous = line.text[byte..].chars().next();
        let mut consumed = false;
        for ch in chars {
            if if word(first) {
                !word(ch)
                    || (consumed && ch.is_lowercase() && previous.is_some_and(char::is_uppercase))
            } else {
                word(ch) || ch.is_whitespace()
            } {
                break;
            }
            start -= 1;
            previous = Some(ch);
            consumed = true;
        }
        start
    }
}

/// Match editor paragraph movement: cross blank lines, then stop at text.
pub(crate) fn paragraph_boundary(
    rows: impl Iterator<Item = (usize, bool)>,
    boundary: usize,
) -> usize {
    let mut crossed_blank = false;
    for (row, blank) in rows {
        if blank {
            crossed_blank = true;
        } else if crossed_blank {
            return row;
        }
    }
    boundary
}

fn caret_column(
    line: &SourceLine<'_>,
    domain: Domain,
    geometry: Geometry,
    cursor: usize,
) -> Option<(usize, usize)> {
    if line.row.is_none() && geometry.pan > 0 {
        return None;
    }
    if domain == Domain::Inline
        && line.row.is_some_and(|row| {
            geometry.pan
                > if line.original {
                    row.left_cell_len()
                } else {
                    row.right_cell_len()
                }
        })
    {
        return None;
    }
    let (text, mut source, mut column) = line.window(domain, geometry);
    if cursor < source {
        return None;
    }
    for grapheme in text.graphemes(true) {
        let end = source + grapheme.chars().count();
        let cells = rendered_width(grapheme);
        if cells == 0 {
            source = end;
            continue;
        }
        if cursor < end {
            return (column + cells <= geometry.area.width as usize).then_some((column, cells));
        }
        source = end;
        column += cells;
    }
    (cursor <= source && source == line.len && column < geometry.area.width as usize)
        .then_some((column, 1))
}

fn rendered_width(grapheme: &str) -> usize {
    if grapheme.chars().any(char::is_control) {
        0
    } else {
        grapheme.width()
    }
}

impl App {
    /// Copy the selection coordinates that affect cosmetic geometry. The
    /// comparison's immutable text and source identity live in the cache key;
    /// no selected text or viewport cells need to be allocated or hashed.
    pub(crate) fn diff_animation_selection_key(
        &self,
    ) -> (
        Option<(u8, usize, usize, usize, usize)>,
        Option<(usize, usize, usize, usize)>,
    ) {
        let state = &self.diff_selection;
        let comparison = if state.fresh(self) {
            state.focus.map(|domain| {
                let (first, last) = state.range();
                let domain = match domain {
                    Domain::Original => 0,
                    Domain::Target => 1,
                    Domain::Inline => 2,
                };
                (domain, first.row, first.column, last.row, last.column)
            })
        } else {
            None
        };
        (
            comparison,
            self.editor().and_then(|editor| editor.selection_range()),
        )
    }

    /// Scope lookup follows the revision under the diff caret, not the target
    /// editor's unrelated cursor when original/deleted text has focus.
    pub(crate) fn diff_scope_cursor(&self) -> Option<(bool, (usize, usize))> {
        if !self.should_render_diff_for_active_editor() {
            return None;
        }
        let state = &self.diff_selection;
        if state.fresh(self)
            && let Some(domain) = state.focus
            && let Some(line) = state.line(&self.diff_render_cache, domain, state.cursor.row)
        {
            let source = line.source.unwrap_or_else(|| {
                if line.original && self.compare_base_content.as_deref() != Some("") {
                    self.diff_render_cache.max_left_line
                } else {
                    0
                }
            });
            return Some((line.original, (source, state.cursor.column.min(line.len))));
        }
        self.editor()
            .map(|editor| (false, (editor.cursor_line, editor.cursor_col)))
    }

    /// Return a visible row's selected source columns without allocating text.
    pub(crate) fn diff_scope_selection_columns(
        &self,
        original: bool,
        row: usize,
        source_line: usize,
    ) -> Option<(usize, usize)> {
        let state = &self.diff_selection;
        if state.fresh(self)
            && let Some(domain) = state.focus
        {
            if domain != Domain::Inline && (domain == Domain::Original) != original {
                return None;
            }
            let (start, end) = state.range();
            return (start != end && row >= start.row && row <= end.row).then_some((
                if row == start.row { start.column } else { 0 },
                if row == end.row {
                    end.column
                } else {
                    usize::MAX
                },
            ));
        }
        if original {
            return None;
        }
        self.editor()
            .and_then(|editor| editor.selection_range())
            .filter(|&(start, _, end, _)| source_line >= start && source_line <= end)
            .map(|(start, from, end, to)| {
                (
                    if source_line == start { from } else { 0 },
                    if source_line == end { to } else { usize::MAX },
                )
            })
    }

    pub(crate) fn clear_diff_selection(&mut self) {
        self.diff_selection = DiffSelection::default();
    }

    pub(crate) fn has_diff_selection(&self) -> bool {
        self.diff_selection.fresh(self)
            && self.diff_selection.focus.is_some()
            && self.diff_selection.anchor != self.diff_selection.cursor
    }

    pub(crate) fn diff_selection_owns_cursor(&self) -> bool {
        self.diff_selection.fresh(self)
            && matches!(
                self.diff_selection.focus,
                Some(Domain::Original | Domain::Inline)
            )
    }

    pub(crate) fn diff_selected_text(&self) -> Option<String> {
        let state = &self.diff_selection;
        if !state.fresh(self) {
            return None;
        }
        let domain = state.focus?;
        let (start, end) = state.range();
        if start == end {
            if domain == Domain::Target {
                return None;
            }
            let line = state.line(&self.diff_render_cache, domain, start.row)?;
            let mut text = line.text.to_owned();
            if line.row.is_some() {
                text.push('\n');
            }
            return Some(text);
        }
        let mut text = String::new();
        let mut first = true;
        let rows = self
            .diff_render_cache
            .selectable_rows(domain != Domain::Inline, domain == Domain::Original);
        let first_row = rows.partition_point(|row| *row < start.row);
        let after_last = rows.partition_point(|row| *row <= end.row);
        let terminal = DiffSelection::content_rows(&self.diff_render_cache, domain);
        let terminal = (state.terminal(domain) && terminal >= start.row && terminal <= end.row)
            .then_some(terminal);
        for index in rows[first_row..after_last].iter().copied().chain(terminal) {
            let Some(line) = state.line(&self.diff_render_cache, domain, index) else {
                continue;
            };
            let from = if index == start.row {
                start.column.min(line.len)
            } else {
                0
            };
            let to = if index == end.row {
                end.column.min(line.len)
            } else {
                line.len
            };
            if !first {
                text.push('\n');
            }
            first = false;
            text.push_str(line.chars(from, to.saturating_sub(from)));
        }
        Some(text)
    }

    pub(crate) fn diff_selection_mouse_down(
        &mut self,
        col: u16,
        row: u16,
        modifiers: KeyModifiers,
    ) -> bool {
        let mut state = std::mem::take(&mut self.diff_selection);
        if !state.fresh(self)
            || modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL | KeyModifiers::SUPER)
        {
            self.diff_selection = state;
            return false;
        }
        let domain = if self.diff_mode == DiffMode::Inline {
            Domain::Inline
        } else if state.original.area.contains((col, row).into()) {
            Domain::Original
        } else {
            Domain::Target
        };
        let Some(position) = state.hit(&self.diff_render_cache, domain, col, row, false) else {
            let consumed = state.geometry(domain).area.contains((col, row).into());
            if consumed {
                // A spacer/omitted-context-only viewport has no source
                // endpoint. An explicit click still relinquishes the old pane
                // selection instead of falling through to an unrelated caret.
                state.focus = None;
                state.dragging = false;
                state.unit = SelectionUnit::Character;
                state.last_click = None;
                if let Some(editor) = self.editor_mut() {
                    editor.clear_selection();
                }
                self.active_pane = ActivePane::Editor;
                self.cursor_visible = true;
            }
            self.diff_selection = state;
            return consumed;
        };
        let shift = modifiers.contains(KeyModifiers::SHIFT) && state.focus == Some(domain);
        let native_anchor = if modifiers.contains(KeyModifiers::SHIFT) && state.focus.is_none() {
            self.editor().and_then(|editor| {
                let (line, column) = editor
                    .selection_anchor
                    .unwrap_or((editor.cursor_line, editor.cursor_col));
                state.source_position(&self.diff_render_cache, domain, line, column)
            })
        } else {
            None
        };
        let now = Instant::now();
        let clicks = state
            .last_click
            .filter(|(when, x, y, old_domain, _)| {
                !shift
                    && *x == col
                    && *y == row
                    && *old_domain == domain
                    && now.duration_since(*when) < Duration::from_millis(400)
            })
            .map_or(1, |(_, _, _, _, count)| (count + 1).min(3));
        state.last_click = Some((now, col, row, domain, clicks));
        if !shift {
            state.anchor = native_anchor.unwrap_or(position);
        }
        state.cursor = position;
        state.focus = Some(domain);
        state.dragging = true;
        state.unit = SelectionUnit::Character;
        state.desired = None;
        if clicks == 2 {
            let (start, end) = state.word_span(&self.diff_render_cache, domain, position);
            state.anchor = start;
            state.cursor = end;
            state.unit = SelectionUnit::Word { start, end };
        } else if clicks >= 3 {
            let (start, end) = state.line_span(&self.diff_render_cache, domain, position);
            state.anchor = start;
            state.cursor = end;
            state.unit = SelectionUnit::Line { start, end };
        }
        if let Some(editor) = self.editor_mut() {
            editor.clear_selection();
            editor.extra_cursors.clear();
            editor.extra_cursor_anchors.clear();
        }
        self.dragging_text = false;
        self.dragging_text_start = None;
        self.multi_click_anchor = None;
        self.active_pane = ActivePane::Editor;
        self.cursor_visible = true;
        state.sync_target(self);
        self.diff_selection = state;
        true
    }

    pub(crate) fn diff_selection_mouse_drag(&mut self, col: u16, row: u16) -> bool {
        let mut state = std::mem::take(&mut self.diff_selection);
        if !state.fresh(self) || !state.dragging {
            self.diff_selection = state;
            return false;
        }
        let Some(domain) = state.focus else {
            self.diff_selection = state;
            return false;
        };
        let geometry = state.geometry(domain);
        let delta = if row < geometry.area.y {
            -1
        } else if row >= geometry.area.bottom() {
            1
        } else {
            0
        };
        if delta != 0 {
            if self.row_diff_active() {
                let mut scroll = std::mem::take(&mut self.diff_inline_scroll);
                if let Some(editor) = self.editor_mut() {
                    scroll.scroll(editor, false, delta);
                }
                self.diff_inline_scroll = scroll;
            } else if let Some(editor) = self.editor_mut() {
                editor.scroll_offset = editor
                    .scroll_offset
                    .saturating_add_signed(delta)
                    .min(editor.line_count().saturating_sub(1));
            }
            state.refresh(self, domain != Domain::Inline);
        }
        if let Some(position) = state.hit(&self.diff_render_cache, domain, col, row, true) {
            state.drag_position(&self.diff_render_cache, domain, position);
            state.sync_target(self);
        }
        self.cursor_visible = true;
        self.diff_selection = state;
        true
    }

    pub(crate) fn diff_selection_mouse_up(&mut self, col: u16, row: u16) -> bool {
        if !self.diff_selection.fresh(self) || !self.diff_selection.dragging {
            return false;
        }
        self.diff_selection_mouse_drag(col, row);
        self.diff_selection.dragging = false;
        true
    }

    pub(crate) fn diff_selection_key(&mut self, key: KeyCode, modifiers: KeyModifiers) -> bool {
        let mut state = std::mem::take(&mut self.diff_selection);
        if !state.fresh(self) || modifiers.contains(KeyModifiers::ALT) {
            self.diff_selection = state;
            return false;
        }
        let ctrl = modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
        let shift = modifiers.contains(KeyModifiers::SHIFT);
        if matches!(state.focus, Some(Domain::Original | Domain::Inline))
            && (matches!(
                key,
                KeyCode::Enter
                    | KeyCode::Backspace
                    | KeyCode::Delete
                    | KeyCode::Tab
                    | KeyCode::BackTab
            ) || (!ctrl && matches!(key, KeyCode::Char(_))))
        {
            state.dragging = false;
            self.diff_selection = state;
            self.notify_warn("Comparison text is read-only");
            return true;
        }
        if key == KeyCode::Esc && state.focus.is_some() {
            state.focus = None;
            state.dragging = false;
            if let Some(editor) = self.editor_mut() {
                editor.clear_selection();
            }
            self.diff_selection = state;
            return true;
        }
        if ctrl && matches!(key, KeyCode::Char('a' | 'A')) {
            let domain = state
                .focus
                .unwrap_or(if self.diff_mode == DiffMode::Inline {
                    Domain::Inline
                } else {
                    Domain::Target
                });
            if let (Some(first), Some(last)) = (
                state.boundary(&self.diff_render_cache, domain, false),
                state.boundary(&self.diff_render_cache, domain, true),
            ) {
                state.focus = Some(domain);
                state.anchor = first;
                state.cursor = last;
                state.dragging = false;
                state.sync_target(self);
                state.reveal(self);
            }
            self.diff_selection = state;
            return true;
        }
        let movement = matches!(
            key,
            KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageUp
                | KeyCode::PageDown
        );
        let Some(domain) = state.focus.filter(|_| movement) else {
            self.diff_selection = state;
            return false;
        };
        if domain == Domain::Target {
            state.focus = None;
            state.dragging = false;
            self.diff_selection = state;
            return false; // Native target movement retains its existing semantics.
        }
        let cache = &self.diff_render_cache;
        let Some(line) = state.line(cache, domain, state.cursor.row) else {
            self.diff_selection = state;
            return false;
        };
        let mut cursor = state.cursor;
        match key {
            KeyCode::Home if ctrl => {
                cursor = state.boundary(cache, domain, false).unwrap_or(cursor)
            }
            KeyCode::End if ctrl => cursor = state.boundary(cache, domain, true).unwrap_or(cursor),
            KeyCode::Home => {
                let indent = line
                    .text
                    .chars()
                    .take_while(|ch| ch.is_whitespace())
                    .count();
                cursor.column = if cursor.column == indent || indent >= line.len {
                    0
                } else {
                    indent
                };
            }
            KeyCode::End => cursor.column = line.len,
            KeyCode::Up | KeyCode::Down if ctrl => {
                cursor = state.paragraph(cache, domain, cursor, key == KeyCode::Down);
            }
            KeyCode::Left if cursor.column > 0 => {
                cursor.column = if ctrl {
                    word_boundary(&line, cursor.column, false)
                } else {
                    cursor.column - 1
                }
            }
            KeyCode::Right if cursor.column < line.len => {
                cursor.column = if ctrl {
                    word_boundary(&line, cursor.column, true)
                } else {
                    cursor.column + 1
                }
            }
            KeyCode::Left => {
                if let Some(previous) = state.adjacent(cache, domain, cursor, false) {
                    cursor = previous;
                    cursor.column = state
                        .line(cache, domain, cursor.row)
                        .map_or(0, |line| line.len);
                }
            }
            KeyCode::Right => {
                if let Some(next) = state.adjacent(cache, domain, cursor, true) {
                    cursor = Position {
                        row: next.row,
                        column: 0,
                    };
                }
            }
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown => {
                let desired = state.desired.unwrap_or_else(|| state
                    .line(cache, domain, cursor.row).map_or(0, |line| line.cell_column(cursor.column)));
                let count = if matches!(key, KeyCode::PageUp | KeyCode::PageDown) {
                    state.geometry(domain).area.height.max(1) as usize
                } else {
                    1
                };
                let down = matches!(key, KeyCode::Down | KeyCode::PageDown);
                for _ in 0..count {
                    let Some(next) = state.adjacent(cache, domain, cursor, down) else {
                        break;
                    };
                    cursor = next;
                    cursor.column = state.line(cache, domain, cursor.row)
                        .map_or(0, |line| line.column_at_cell(desired));
                }
                state.desired = Some(desired);
            }
            _ => {}
        }
        if ctrl
            || !matches!(
                key,
                KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown
            )
        {
            state.desired = None;
        }
        if !shift {
            state.anchor = cursor;
        }
        state.cursor = cursor;
        state.dragging = false;
        state.sync_target(self);
        state.reveal(self);
        self.cursor_visible = true;
        self.diff_selection = state;
        true
    }
}

pub(crate) fn draw(
    frame: &mut Frame,
    app: &mut App,
    aligned: bool,
    show_cursor: bool,
) -> Option<crate::ui_editor_animations::CursorUnderlay> {
    let mut primary = None;
    let mut selection = app.copy_animation_selection_style();
    let mut state = std::mem::take(&mut app.diff_selection);
    state.refresh(app, aligned);
    let native = app.editor().and_then(|editor| editor.selection_range());
    let domains: &[Domain] = if aligned {
        &[Domain::Original, Domain::Target]
    } else {
        &[Domain::Inline]
    };
    for &domain in domains {
        let geometry = state.geometry(domain);
        for offset in 0..geometry.area.height as usize {
            let index = geometry.first.saturating_add(offset);
            let Some(line) = state.line(&app.diff_render_cache, domain, index) else {
                continue;
            };
            let range = if state.focus == Some(domain) {
                let (start, end) = state.range();
                (start != end && index >= start.row && index <= end.row).then_some((
                    if index == start.row { start.column } else { 0 },
                    if index == end.row {
                        end.column
                    } else {
                        line.len.saturating_add(1)
                    },
                ))
            } else if state.focus.is_none() && !line.original {
                native.and_then(|(sl, sc, el, ec)| {
                    line.source
                        .filter(|source| *source >= sl && *source <= el)
                        .map(|source| {
                            (
                                if source == sl { sc } else { 0 },
                                if source == el {
                                    ec
                                } else {
                                    line.len.saturating_add(1)
                                },
                            )
                        })
                })
            } else {
                None
            };
            let Some((from, to)) = range.filter(|(from, to)| from < to) else {
                continue;
            };
            let (text, mut source, mut x) = line.window(domain, geometry);
            let y = geometry.area.y + offset as u16;
            for grapheme in text.graphemes(true) {
                let end = source + grapheme.chars().count();
                let cells = rendered_width(grapheme);
                if source < to && end > from {
                    for column in x..x.saturating_add(cells).min(geometry.area.width as usize) {
                        let cell = &mut frame.buffer_mut()[(geometry.area.x + column as u16, y)];
                        cell.set_style(selection.apply(cell.style()));
                    }
                }
                source = end;
                x += cells;
            }
            if to > line.len && from <= line.len && source == line.len {
                for column in x..geometry.area.width as usize {
                    let cell = &mut frame.buffer_mut()[(geometry.area.x + column as u16, y)];
                    cell.set_style(selection.apply(cell.style()));
                }
            }
        }
    }
    if matches!(state.focus, Some(Domain::Original | Domain::Inline))
        && show_cursor
        && !app.rendering_inactive_editor
        && !app.status_details.open
    {
        let domain = state.focus.unwrap();
        let geometry = state.geometry(domain);
        if let Some(offset) = state
            .cursor
            .row
            .checked_sub(geometry.first)
            .filter(|offset| *offset < geometry.area.height as usize)
            && let Some(line) = state.line(&app.diff_render_cache, domain, state.cursor.row)
            && let Some((column, cells)) =
                caret_column(&line, domain, geometry, state.cursor.column)
        {
            let x = geometry.area.x + column as u16;
            let y = geometry.area.y + offset as u16;
            primary = Some(crate::ui_diff_display::paint_diff_cursor(
                frame, app, x, y, cells,
            ));
        }
    }
    app.diff_selection = state;
    app.copy_animation.observe(&selection);
    primary
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache() -> Arc<DiffRenderCache> {
        Arc::new(DiffRenderCache::new(&crate::app::compute_file_diff(
            "alpha\nremoved\n界e\u{301} old\nlast\n",
            "alpha\n界e\u{301} new\nlast\n",
        )))
    }

    #[test]
    fn original_inline_and_target_rows_have_distinct_ownership() {
        let cache = cache();
        let state = DiffSelection::default();
        assert!(cache.inline_rows.iter().enumerate().any(|(index, entry)| {
            entry.kind == '-'
                && state
                    .line(&cache, Domain::Inline, index)
                    .is_some_and(|line| line.original)
        }));
        for (index, row) in cache.rows.iter().enumerate() {
            assert_eq!(
                state.line(&cache, Domain::Original, index).is_some(),
                row.left_line.is_some()
            );
            assert_eq!(
                state.line(&cache, Domain::Target, index).is_some(),
                row.right_line.is_some()
            );
        }
    }

    #[test]
    fn indexed_character_slices_and_caret_projection_preserve_unicode() {
        let cache = cache();
        let state = DiffSelection::default();
        let row = cache
            .rows
            .iter()
            .position(|row| row.right_text.starts_with('界'))
            .unwrap();
        let line = state.line(&cache, Domain::Target, row).unwrap();
        assert_eq!(line.chars(1, 2), "e\u{301}");
        assert_eq!(&line.text[..line.byte_at(3)], "界e\u{301}");
        let geometry = Geometry {
            area: Rect::new(0, 0, 20, 1),
            first: row,
            pan: 0,
        };
        assert_eq!(
            caret_column(&line, Domain::Target, geometry, 0),
            Some((0, 2))
        );
        assert_eq!(
            caret_column(&line, Domain::Target, geometry, 2),
            Some((2, 1))
        );
        let geometry = Geometry { pan: 1, ..geometry };
        assert_eq!(
            caret_column(&line, Domain::Target, geometry, 3),
            Some((1, 1))
        );
    }

    #[test]
    fn pointer_projection_skips_alignment_holes_and_uses_glyph_cells() {
        let cache = cache();
        let row = cache
            .rows
            .iter()
            .position(|row| row.right_text.starts_with('界'))
            .unwrap();
        let state = DiffSelection {
            target: Geometry {
                area: Rect::new(8, 2, 20, 8),
                first: 0,
                pan: 0,
            },
            ..Default::default()
        };
        assert_eq!(
            state.hit(&cache, Domain::Target, 10, 2 + row as u16, false),
            Some(Position { row, column: 1 })
        );
        assert_eq!(
            state.hit(&cache, Domain::Target, 11, 2 + row as u16, false),
            Some(Position { row, column: 3 })
        );
        assert!(state.hit(&cache, Domain::Target, 7, 2, false).is_none());
    }

    #[test]
    fn compact_markers_are_not_source_text() {
        let base = (0..30)
            .map(|line| format!("line_{line}\n"))
            .collect::<String>();
        let target = base.replace("line_15", "changed_15");
        let full = DiffRenderCache::new(&crate::app::compute_file_diff(&base, &target));
        let compact = full.with_context(2);
        let state = DiffSelection::default();
        for (index, row) in compact
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.hidden_lines > 0)
        {
            assert!(state.line(&compact, Domain::Original, index).is_none());
            assert!(state.line(&compact, Domain::Target, index).is_none());
            assert!(!row.left_text.is_empty());
        }
    }

    #[test]
    fn omitted_control_characters_advance_source_without_claiming_glyph_cells() {
        for control in ['\t', '\u{7}'] {
            let base = format!("a{control}b old\n");
            let target = format!("a{control}b new\n");
            let cache = DiffRenderCache::new(&crate::app::compute_file_diff(&base, &target));
            let geometry = Geometry {
                area: Rect::new(0, 0, 20, 8),
                first: 0,
                pan: 0,
            };
            let state = DiffSelection {
                original: geometry,
                target: geometry,
                inline: geometry,
                ..Default::default()
            };
            for domain in [Domain::Original, Domain::Target, Domain::Inline] {
                for row in 0..DiffSelection::content_rows(&cache, domain) {
                    let Some(line) = state.line(&cache, domain, row) else {
                        continue;
                    };
                    assert_eq!(
                        state.hit(&cache, domain, 1, row as u16, false),
                        Some(Position { row, column: 2 })
                    );
                    assert_eq!(
                        state.hit(&cache, domain, 2, row as u16, false),
                        Some(Position { row, column: 3 })
                    );
                    assert_eq!(line.chars(2, 1), "b");
                    assert_eq!(caret_column(&line, domain, geometry, 2), Some((1, 1)));
                }
            }
            let only_control =
                DiffRenderCache::new(&crate::app::compute_file_diff("", &format!("{control}")));
            let line = state.line(&only_control, Domain::Target, 0).unwrap();
            assert_eq!(
                caret_column(&line, Domain::Target, geometry, 0),
                Some((0, 1))
            );
            assert_eq!(
                caret_column(&line, Domain::Target, geometry, 1),
                Some((0, 1))
            );
        }
    }

    #[test]
    fn word_and_line_drag_keep_expanded_units_on_release_and_direction_change() {
        let cache = cache();
        let mut state = DiffSelection::default();
        let row = cache
            .rows
            .iter()
            .position(|row| row.left_text == "removed")
            .unwrap();
        let position = Position { row, column: 3 };
        let (start, end) = state.word_span(&cache, Domain::Original, position);
        state.unit = SelectionUnit::Word { start, end };
        state.drag_position(&cache, Domain::Original, position);
        assert_eq!(
            state.range(),
            (Position { row, column: 0 }, Position { row, column: 7 })
        );
        state.drag_position(&cache, Domain::Original, Position { row: 0, column: 2 });
        assert_eq!(state.range(), (Position { row: 0, column: 0 }, end));
        state.drag_position(&cache, Domain::Original, position);
        assert_eq!(state.range(), (start, end));

        let (start, end) = state.line_span(&cache, Domain::Original, position);
        state.unit = SelectionUnit::Line { start, end };
        state.drag_position(&cache, Domain::Original, position);
        assert_eq!(state.range(), (start, end));
        state.drag_position(&cache, Domain::Original, end);
        assert!(
            state.cursor > end,
            "line drag into the next row selects that entire row"
        );
        state.drag_position(&cache, Domain::Original, Position { row: 0, column: 1 });
        assert_eq!(state.anchor, end);
        assert_eq!(state.cursor, Position { row: 0, column: 0 });
    }

    #[test]
    fn indexed_navigation_matches_source_row_scan_including_terminal_empty_row() {
        let cache = cache();
        for terminal in [false, true] {
            let state = DiffSelection {
                original_terminal: terminal,
                target_terminal: terminal,
                ..Default::default()
            };
            for domain in [Domain::Original, Domain::Target, Domain::Inline] {
                let count = DiffSelection::content_rows(&cache, domain) + usize::from(terminal);
                let available = (0..count)
                    .filter_map(|row| {
                        state.line(&cache, domain, row).map(|line| Position {
                            row,
                            column: line.len,
                        })
                    })
                    .collect::<Vec<_>>();
                assert_eq!(
                    state.boundary(&cache, domain, false),
                    available.first().map(|position| Position {
                        row: position.row,
                        column: 0
                    })
                );
                assert_eq!(
                    state.boundary(&cache, domain, true),
                    available.last().copied()
                );
                for row in 0..count {
                    let position = Position { row, column: 4 };
                    let previous = available
                        .iter()
                        .rev()
                        .find(|candidate| candidate.row < row)
                        .map(|candidate| Position {
                            row: candidate.row,
                            column: 4.min(candidate.column),
                        });
                    let next =
                        available
                            .iter()
                            .find(|candidate| candidate.row > row)
                            .map(|candidate| Position {
                                row: candidate.row,
                                column: 4.min(candidate.column),
                            });
                    assert_eq!(state.adjacent(&cache, domain, position, false), previous);
                    assert_eq!(state.adjacent(&cache, domain, position, true), next);
                }
            }
        }
    }

    #[test]
    fn indexed_cell_columns_match_long_unicode_control_prefixes() {
        let text = format!("{}end", "界e\u{301}\tz\u{7}👩‍💻 ".repeat(180));
        let cache = DiffRenderCache::new(&[(' ', text)]);
        let row = &cache.rows[0];
        let mut source = 0;
        let mut cells = 0;
        for grapheme in row.left_text.graphemes(true) {
            // Every scalar within a combining/ZWJ cluster occupies its
            // grapheme's starting cell, even across sparse checkpoints.
            for inside in 0..grapheme.chars().count() {
                assert_eq!(row.left_cell_column(source + inside), cells);
                assert_eq!(row.right_cell_column(source + inside), cells);
            }
            source += grapheme.chars().count();
            if !grapheme.chars().any(char::is_control) {
                cells += grapheme.width();
            }
            assert_eq!(row.left_cell_column(source), cells);
            assert_eq!(row.right_cell_column(source), cells);
        }
        assert!(source > 1024, "exercise several sparse checkpoints");
        assert_eq!(row.left_cell_len(), cells);
        assert_eq!(row.right_cell_len(), cells);
        assert_eq!(row.left_cell_column(usize::MAX), cells);
        assert_eq!(row.right_cell_column(usize::MAX), cells);
    }

    #[test]
    fn indexed_cell_to_source_columns_keep_wide_and_combining_graphemes_intact() {
        for text in ["".to_owned(), "ascii".repeat(300), "\t\u{7}".repeat(300),
            "界e\u{301}\tz\u{7}👩‍💻 ".repeat(180)] {
            let cache = DiffRenderCache::new(&[(' ', text.clone())]);
            let row = &cache.rows[0];
            let mut source = 0;
            let mut cells = 0;
            for grapheme in text.graphemes(true) {
                let width = rendered_width(grapheme);
                for cell in cells..cells + width {
                    assert_eq!(row.left_column_at_cell(cell), source);
                    assert_eq!(row.right_column_at_cell(cell), source);
                }
                source += grapheme.chars().count();
                cells += width;
            }
            for cell in [cells, cells + 1, usize::MAX] {
                assert_eq!(row.left_column_at_cell(cell), source);
                assert_eq!(row.right_column_at_cell(cell), source);
            }
        }
    }
}
