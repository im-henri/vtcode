use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::{
    layout::Rect,
    style::Style,
    text::{Line, Span},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// Keep endpoints in the source so scrolling and soft wraps don't affect copying.
#[derive(Default)]
pub(crate) struct Selection {
    anchor: usize,
    end: usize,
    pub(crate) dragging: bool,
}

impl Selection {
    pub(crate) fn start(&mut self, offset: usize) {
        *self = Self {
            anchor: offset,
            end: offset,
            dragging: true,
        };
    }

    pub(crate) fn extend(&mut self, offset: usize) {
        self.end = offset;
    }

    pub(crate) fn range(&self) -> Option<Range<usize>> {
        let (a, b) = (self.anchor, self.end);
        (a != b).then_some(a.min(b)..a.max(b))
    }

    pub(crate) fn text<'a>(&self, source: &'a str) -> Option<&'a str> {
        source.get(self.range()?)
    }

    pub(crate) fn line<'a>(&self, source: &'a str, row: &Range<usize>, style: Style) -> Line<'a> {
        let Some(selection) = self.range() else {
            return Line::raw(&source[row.clone()]);
        };
        let start = selection.start.max(row.start);
        let end = selection.end.min(row.end);
        if start >= end {
            return Line::raw(&source[row.clone()]);
        }
        Line::from(vec![
            Span::raw(&source[row.start..start]),
            Span::styled(&source[start..end], style),
            Span::raw(&source[end..row.end]),
        ])
    }
}

/// Resolve a pointer against its rendered row, clamping a drag past the viewport.
pub(crate) fn byte_at(source: &str, row: &Range<usize>, area: Rect, mouse: MouseEvent) -> usize {
    if mouse.row < area.y {
        return row.start;
    }
    if mouse.row >= area.bottom() {
        return row.end;
    }
    let column = usize::from(mouse.column.saturating_sub(area.x));
    let mut x = 0;
    for (offset, glyph) in source[row.clone()].grapheme_indices(true) {
        x += glyph.width();
        if column < x {
            return row.start + offset;
        }
    }
    row.end
}

pub(crate) fn is_copy_key(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('c' | 'C'))
        && key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
        && !key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT)
}
