//! Selection uses the immutable preview's rows, never the editor underneath.
use super::{App, HunkPeek, KeyModifiers, PeekLayout};
use ratatui::Frame;
use std::time::{Duration, Instant};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Position {
    row: usize,
    byte: usize,
}

#[derive(Clone, Copy)]
enum Unit {
    Word,
    Line,
}

#[derive(Default)]
pub(super) struct Selection {
    anchor: Option<Position>,
    cursor: Position,
    dragging: bool,
    unit: Option<(Unit, Position, Position)>,
    last_click: Option<(Instant, Position, u8)>,
}

impl Selection {
    fn range(&self) -> Option<(Position, Position)> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then_some((anchor.min(self.cursor), anchor.max(self.cursor)))
    }

    pub(super) fn has_text(&self) -> bool {
        self.range().is_some()
    }
    pub(super) fn dragging(&self) -> bool {
        self.dragging
    }
    pub(super) fn cancel_drag(&mut self) {
        self.dragging = false;
    }
}

fn position(peek: &HunkPeek, layout: PeekLayout, column: u16, row: u16) -> Position {
    let index = peek.scroll + row.saturating_sub(layout.code.y) as usize;
    if index >= peek.rows.len() {
        return Position {
            row: peek.rows.len(),
            byte: 0,
        };
    }
    let text = &peek.rows[index].text;
    let target = peek.horizontal_scroll + column.saturating_sub(layout.code.x) as usize;
    let mut cell = 0;
    for (byte, glyph) in text.grapheme_indices(true) {
        let width = if glyph == "\t" {
            peek.tab_size - cell % peek.tab_size
        } else {
            glyph.width()
        };
        if target < cell + width {
            return Position { row: index, byte };
        }
        cell += width;
    }
    Position {
        row: index,
        byte: text.len(),
    }
}

fn extent(peek: &HunkPeek, position: Position, unit: Unit) -> (Position, Position) {
    let Some(row) = peek.rows.get(position.row) else {
        return (position, position);
    };
    if matches!(unit, Unit::Line) {
        return (
            Position {
                row: position.row,
                byte: 0,
            },
            Position {
                row: position.row + 1,
                byte: 0,
            },
        );
    }
    let hit = position.byte.min(row.text.len().saturating_sub(1));
    let (byte, text) = row
        .text
        .split_word_bound_indices()
        .find(|(byte, text)| *byte <= hit && hit < *byte + text.len())
        .unwrap_or((0, ""));
    (
        Position {
            row: position.row,
            byte,
        },
        Position {
            row: position.row,
            byte: byte + text.len(),
        },
    )
}

impl HunkPeek {
    pub(crate) fn cancel_drag(&mut self) {
        self.drag = None;
        self.selection.cancel_drag();
    }

    pub(super) fn has_old_lines(&self) -> bool {
        let hunk = &self.hunks[self.selected];
        self.rows[hunk.start_row..hunk.end_row]
            .iter()
            .any(|row| row.kind == '-')
    }

    fn old_text(&self) -> Option<String> {
        if !self.has_old_lines() {
            return None;
        }
        let hunk = &self.hunks[self.selected];
        let mut text = String::new();
        for row in self.rows[hunk.start_row..hunk.end_row]
            .iter()
            .filter(|row| row.kind == '-')
        {
            text.push_str(&row.text);
            text.push_str(row.line_ending);
        }
        Some(text)
    }

    pub(super) fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection.range()?;
        let mut text = String::new();
        for index in start.row..=end.row.min(self.rows.len().saturating_sub(1)) {
            let row = &self.rows[index];
            let from = if index == start.row { start.byte } else { 0 };
            let to = if index == end.row {
                end.byte
            } else {
                row.text.len()
            };
            text.push_str(&row.text[from..to]);
            if index < end.row {
                // Adjacent old/new rows remain separate even if the original
                // file lacked a newline. Do not invent a final EOF newline.
                text.push_str(
                    if row.line_ending.is_empty() && index + 1 < self.rows.len() {
                        "\n"
                    } else {
                        row.line_ending
                    },
                );
            }
        }
        Some(text)
    }

    pub(super) fn select_all(&mut self) {
        self.selection = Selection {
            anchor: Some(Position::default()),
            cursor: Position {
                row: self.rows.len(),
                byte: 0,
            },
            ..Selection::default()
        };
        self.focused_button = None;
    }

    pub(super) fn begin_selection(
        &mut self,
        layout: PeekLayout,
        column: u16,
        row: u16,
        modifiers: KeyModifiers,
    ) {
        let point = position(self, layout, column, row);
        self.focused_button = None;
        self.drag = None;
        if modifiers.contains(KeyModifiers::SHIFT) && self.selection.anchor.is_some() {
            self.selection.cursor = point;
            self.selection.unit = None;
            self.selection.last_click = None;
        } else {
            let now = Instant::now();
            let count = self
                .selection
                .last_click
                .filter(|(time, previous, _)| {
                    *previous == point
                        && now.saturating_duration_since(*time) < Duration::from_millis(500)
                })
                .map_or(1, |(_, _, count)| count % 3 + 1);
            let unit = match count {
                2 => Some(Unit::Word),
                3 => Some(Unit::Line),
                _ => None,
            };
            let (start, end) = unit.map_or((point, point), |unit| extent(self, point, unit));
            self.selection = Selection {
                anchor: Some(start),
                cursor: end,
                unit: unit.map(|unit| (unit, start, end)),
                last_click: Some((now, point, count)),
                ..Selection::default()
            };
        }
        self.selection.dragging = true;
    }

    pub(super) fn extend_selection(&mut self, layout: PeekLayout, column: u16, row: u16) {
        if !self.selection.dragging || layout.code.is_empty() {
            return;
        }
        let point = position(
            self,
            layout,
            column.max(layout.code.x).min(layout.code.right()),
            row.max(layout.code.y)
                .min(layout.code.bottom().saturating_sub(1)),
        );
        if let Some((unit, start, end)) = self.selection.unit {
            let (from, to) = extent(self, point, unit);
            if point < start {
                self.selection.anchor = Some(end);
                self.selection.cursor = from;
            } else {
                self.selection.anchor = Some(start);
                self.selection.cursor = to;
            }
        } else {
            self.selection.cursor = point;
        }
    }
}

pub(super) fn drag(app: &mut App, layout: PeekLayout, column: u16, row: u16) {
    if layout.code.is_empty() {
        return;
    }
    let amount = app.scroll_speed.max(1);
    if row < layout.code.y {
        super::scroll(app, false, amount, false);
    } else if row >= layout.code.bottom() {
        super::scroll(app, true, amount, false);
    }
    if column < layout.code.x {
        super::scroll(app, false, amount, true);
    } else if column >= layout.code.right() {
        super::scroll(app, true, amount, true);
    }
    if let Some(peek) = app.hunk_peek.as_mut() {
        peek.extend_selection(layout, column, row);
    }
}

pub(super) fn copy(app: &mut App, old: bool) {
    let Some(peek) = app.hunk_peek.as_ref() else {
        return;
    };
    let Some(text) = (if old {
        peek.old_text()
    } else {
        peek.selected_text()
    }) else {
        return;
    };
    match crate::clipboard::set_text(&text) {
        Ok(()) => app.notify(if old {
            "Copied old lines"
        } else {
            "Copied selected text"
        }),
        Err(error) => app.notify_error(format!("Unable to copy: {error}")),
    }
}

pub(super) fn draw(
    frame: &mut Frame,
    peek: &HunkPeek,
    layout: PeekLayout,
    colors: &crate::app_theme::ThemeColors,
) {
    let Some((start, end)) = peek.selection.range() else {
        return;
    };
    let mut style = crate::ui_colors::EditorSelectionStyle::new(colors);
    for (index, row) in peek
        .rows
        .iter()
        .enumerate()
        .skip(peek.scroll)
        .take(layout.code.height as usize)
    {
        if index < start.row || index > end.row {
            continue;
        }
        let from = if index == start.row { start.byte } else { 0 };
        let to = if index == end.row {
            end.byte
        } else {
            row.text.len() + 1
        };
        let mut column = 0;
        for (byte, glyph) in row.text.grapheme_indices(true) {
            if column >= peek.horizontal_scroll + layout.code.width as usize {
                break;
            }
            let width = if glyph == "\t" {
                peek.tab_size - column % peek.tab_size
            } else {
                glyph.width()
            };
            if byte < to && byte + glyph.len() > from {
                for x in column.max(peek.horizontal_scroll)
                    ..(column + width).min(peek.horizontal_scroll + layout.code.width as usize)
                {
                    let cell = &mut frame.buffer_mut()[(
                        layout.code.x + (x - peek.horizontal_scroll) as u16,
                        layout.code.y + (index - peek.scroll) as u16,
                    )];
                    cell.set_style(style.apply(cell.style()));
                }
            }
            column += width;
        }
        if to > row.text.len() && from <= row.text.len() {
            if column >= peek.horizontal_scroll
                && column < peek.horizontal_scroll + layout.code.width as usize
            {
                let cell = &mut frame.buffer_mut()[(
                    layout.code.x + (column - peek.horizontal_scroll) as u16,
                    layout.code.y + (index - peek.scroll) as u16,
                )];
                cell.set_style(style.apply(cell.style()));
            }
        }
    }
}
