//! Paint rows whose wrapping and horizontal scrolling are already resolved.
//! Unlike Paragraph, this needs neither a second layout pass nor an allocated
//! list of styled graphemes. Keep Paragraph's clipping and span boundaries.

#[cfg(test)]
use ratatui::layout::Alignment;
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::Style,
    text::{Line, Span},
    widgets::{Paragraph, Widget},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[path = "ui_editor_rows.rs"]
mod rows;
pub(super) use rows::Rows;

#[cfg(test)]
thread_local! {
    pub(super) static REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(super) fn paint(buffer: &mut Buffer, area: Rect, lines: Vec<Line<'_>>, style: Style) {
    #[cfg(test)]
    if REFERENCE.get() {
        Paragraph::new(lines).style(style).render(area, buffer);
        return;
    }
    // Prepared editor rows are left aligned. Keep an exact fallback if a new
    // caller supplies centered/right-aligned content.
    if lines
        .iter()
        .any(|line| line.alignment.is_some_and(|a| a != Alignment::Left))
    {
        Paragraph::new(lines).style(style).render(area, buffer);
        return;
    }
    paint_rows(buffer, area, &lines, style);
}

#[cfg(test)]
fn paint_rows(buffer: &mut Buffer, area: Rect, lines: &[Line<'_>], style: Style) {
    let area = area.intersection(buffer.area);
    if area.is_empty() {
        return;
    }
    for (line, y) in lines.iter().zip(area.y..area.bottom()) {
        paint_row(
            buffer,
            Rect::new(area.x, y, area.width, 1),
            &line.spans,
            line.style,
            style,
        );
    }
    let used = lines.len().min(usize::from(area.height)) as u16;
    buffer.set_style(
        Rect::new(area.x, area.y + used, area.width, area.height - used),
        style,
    );
}

fn paint_row(
    buffer: &mut Buffer,
    area: Rect,
    spans: &[Span<'_>],
    line_style: Style,
    base_style: Style,
) {
    if area.is_empty() {
        return;
    }
    let start = buffer.index_of(area.x, area.y);
    let row = &mut buffer.content[start..start + usize::from(area.width)];
    // Patch the row background into each span once, instead of styling every
    // cell once for the background and again for its text. Unpainted cells and
    // wide-glyph continuations still inherit only the original base style.
    let line_style = base_style.patch(line_style);
    let mut x = 0;
    'spans: for span in spans {
        if x == row.len() {
            break;
        }
        let style = line_style.patch(span.style);
        let text = span.content.as_ref();
        // Inspect only the visible prefix plus one byte of lookahead. The
        // lookahead rules out a combining mark extending its final ASCII
        // character; long offscreen annotations need no full-string scan.
        let visible = row.len() - x;
        let prefix = &text.as_bytes()[..text.len().min(visible + 1)];
        if prefix.iter().all(|&byte| (b' '..=b'~').contains(&byte)) {
            if super::plain_runs::enabled() {
                let count = text.len().min(visible);
                // The clipped run is known up front: no per-cell index lookup
                // or end-of-row branch while painting ordinary text/padding.
                for (cell, &byte) in row[x..x + count].iter_mut().zip(text.as_bytes()) {
                    cell.set_char(char::from(byte)).set_style(style);
                }
                x += count;
            } else {
                for &byte in text.as_bytes().iter().take(visible) {
                    row[x].set_char(char::from(byte)).set_style(style);
                    x += 1;
                    if x == row.len() { break 'spans; }
                }
            }
        } else if let Some(ch) = text.chars().next().filter(|ch| ch.len_utf8() == text.len()) {
            // Whitespace dots, guides, arrows, and individually styled Unicode
            // scalars are already whole graphemes. Avoid the segmentation state
            // machine, keeping the same string-width and clipping rules.
            if ch.is_control() {
                continue;
            }
            let width = text.width();
            if width == 0 || width > row.len() {
                continue;
            }
            if width > row.len() - x {
                break 'spans;
            }
            row[x].set_symbol(text).set_style(style);
            for cell in &mut row[x + 1..x + width] {
                cell.set_style(base_style);
            }
            x += width;
        } else {
            for symbol in text.graphemes(true) {
                if symbol.contains(char::is_control) {
                    continue;
                }
                let width = symbol.width();
                if width == 0 || width > row.len() {
                    continue;
                }
                if width > row.len() - x {
                    break 'spans;
                }
                row[x].set_symbol(symbol).set_style(style);
                for cell in &mut row[x + 1..x + width] {
                    cell.set_style(base_style);
                }
                x += width;
                if x == row.len() {
                    break 'spans;
                }
            }
        }
    }
    for cell in &mut row[x..] {
        cell.set_style(base_style);
    }
}

#[cfg(test)]
#[path = "ui_editor_text_tests.rs"]
mod tests;
