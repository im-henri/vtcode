//! One styled row drawn straight into the buffer.
//!
//! `Paragraph::new(line).style(style).scroll((0, x))` allocates a `Text`, a
//! grapheme vector and a line composer for every call. Sidebars and bars draw
//! dozens of single-line rows per frame, so they use this instead; the cells
//! written are identical, including truncation and horizontal scrolling.
use ratatui::{
    buffer::Buffer,
    layout::{Alignment, Rect},
    style::Style,
    text::{Line, StyledGrapheme},
};
use unicode_width::UnicodeWidthStr;

#[cfg(test)]
thread_local! {
    /// Draw through `Paragraph`, to compare whole frames against it.
    pub(crate) static REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Draws `line` like a non-wrapping, left-aligned `Paragraph` styled with
/// `style` and scrolled `scroll_x` columns: the whole area takes `style`, and
/// the line fills its first row.
pub(crate) fn render_line(
    buffer: &mut Buffer,
    area: Rect,
    line: &Line<'_>,
    style: Style,
    scroll_x: u16,
) {
    #[cfg(test)]
    if REFERENCE.get() {
        use ratatui::widgets::Widget;
        ratatui::widgets::Paragraph::new(line.clone())
            .style(style)
            .scroll((0, scroll_x))
            .render(area, buffer);
        return;
    }
    let area = area.intersection(buffer.area);
    buffer.set_style(area, style);
    if area.is_empty() {
        return;
    }
    let alignment = line.alignment.unwrap_or(Alignment::Left);
    // Text scrolls horizontally only when it is left-aligned.
    let scroll = if alignment == Alignment::Left {
        usize::from(scroll_x)
    } else {
        0
    };
    let x = match alignment {
        Alignment::Left => 0,
        _ => {
            let width = visit(line, area.width, scroll, |_, _, _| {});
            if alignment == Alignment::Center {
                (area.width / 2).saturating_sub(width / 2)
            } else {
                area.width.saturating_sub(width)
            }
        }
    };
    let mut column = area.x + x;
    visit(line, area.width, scroll, |symbol, width, style| {
        buffer[(column, area.y)].set_symbol(symbol).set_style(style);
        column += width;
    });
}

/// `Paragraph::new("").style(style)`: styles the area and draws nothing.
pub(crate) fn fill(buffer: &mut Buffer, area: Rect, style: Style) {
    #[cfg(test)]
    if REFERENCE.get() {
        use ratatui::widgets::Widget;
        ratatui::widgets::Paragraph::new("")
            .style(style)
            .render(area, buffer);
        return;
    }
    let area = area.intersection(buffer.area);
    buffer.set_style(area, style);
}

/// A non-wrapping `Paragraph` of several lines, one per row from the top.
pub(crate) fn render_lines(buffer: &mut Buffer, area: Rect, lines: &[Line<'_>], style: Style) {
    #[cfg(test)]
    if REFERENCE.get() {
        use ratatui::widgets::Widget;
        ratatui::widgets::Paragraph::new(lines.to_vec())
            .style(style)
            .render(area, buffer);
        return;
    }
    let area = area.intersection(buffer.area);
    buffer.set_style(area, style);
    for (row, line) in (area.y..area.bottom()).zip(lines) {
        render_line(
            buffer,
            Rect {
                y: row,
                height: 1,
                ..area
            },
            line,
            style,
            0,
        );
    }
}

/// Feeds each drawn grapheme with its width to `draw`, following
/// `LineTruncator`, and returns the width of the drawn line.
fn visit(
    line: &Line<'_>,
    max_width: u16,
    mut scroll: usize,
    mut draw: impl FnMut(&str, u16, Style),
) -> u16 {
    let mut used = 0u16;
    // Returns false once the row is full.
    let mut step = |symbol: &str, width: usize, style: Style| -> bool {
        // A grapheme wider than the whole row is skipped.
        if width > usize::from(max_width) {
            return true;
        }
        if usize::from(used) + width > usize::from(max_width) {
            return false;
        }
        if scroll > 0 {
            if width > scroll {
                // A partly scrolled grapheme is still drawn whole.
                scroll = 0;
            } else {
                scroll -= width;
                return true;
            }
        }
        if width > 0 {
            used += width as u16;
            draw(symbol, width as u16, style);
        }
        true
    };
    let base = Style::default().patch(line.style);
    for span in &line.spans {
        let text = span.content.as_ref();
        if text.is_ascii() {
            // Each printable byte is one single-cell grapheme; control bytes
            // are dropped, as `styled_graphemes` drops them.
            let style = base.patch(span.style);
            for (index, byte) in text.bytes().enumerate() {
                if !byte.is_ascii_control() && !step(&text[index..=index], 1, style) {
                    return used;
                }
            }
        } else {
            for StyledGrapheme { symbol, style } in span.styled_graphemes(base) {
                if !step(symbol, symbol.width(), style) {
                    return used;
                }
            }
        }
    }
    used
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{
        style::{Color, Modifier},
        text::Span,
        widgets::{Paragraph, Widget},
    };

    #[test]
    fn fill_matches_an_empty_paragraph() {
        let style = Style::default()
            .bg(Color::Indexed(4))
            .add_modifier(Modifier::DIM);
        let outer = Rect::new(0, 0, 10, 4);
        for area in [
            Rect::new(1, 1, 5, 2),
            Rect::new(8, 2, 6, 5),
            Rect::default(),
        ] {
            let mut expected = Buffer::filled(outer, ratatui::buffer::Cell::new("x"));
            let mut actual = expected.clone();
            Paragraph::new("").style(style).render(area, &mut expected);
            fill(&mut actual, area, style);
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn rows_match_paragraph_rendering() {
        let pieces = [
            "plain",
            " ",
            "src",
            "界",
            "👁️‍🗨️",
            "e\u{301}",
            "\t",
            "\u{200b}",
            "▸ ",
            "",
            "long_file_name.rs",
            "\r\n",
            "●",
            "ﬁ",
            "🦀",
        ];
        let mut seed = 0x2545_f491_u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as usize
        };
        for case in 0..3_000 {
            let spans: Vec<Span<'_>> = (0..next() % 6)
                .map(|index| {
                    let text: String = (0..1 + next() % 3)
                        .map(|_| pieces[next() % pieces.len()])
                        .collect();
                    let style = Style::default()
                        .fg(Color::Indexed((index * 7) as u8))
                        .add_modifier(if index % 2 == 0 {
                            Modifier::BOLD
                        } else {
                            Modifier::empty()
                        });
                    Span::styled(text, style)
                })
                .collect();
            let mut line = Line::from(spans);
            if next() % 4 == 0 {
                line = line.style(Style::default().bg(Color::Indexed(9)));
            }
            line.alignment = match next() % 5 {
                0 => Some(Alignment::Center),
                1 => Some(Alignment::Right),
                2 => Some(Alignment::Left),
                _ => None,
            };
            let width = (next() % 24) as u16;
            let height = 1 + (next() % 2) as u16;
            let scroll = (next() % 8) as u16 * u16::from(next() % 2 == 0);
            let style = Style::default().bg(Color::Indexed(4)).fg(Color::Indexed(5));
            let area = Rect::new(1, 1, width, height);
            let outer = Rect::new(0, 0, 26, 4);
            let mut expected = Buffer::filled(outer, ratatui::buffer::Cell::new("x"));
            let mut actual = expected.clone();
            Paragraph::new(line.clone())
                .style(style)
                .scroll((0, scroll))
                .render(area, &mut expected);
            render_line(&mut actual, area, &line, style, scroll);
            assert_eq!(
                actual, expected,
                "case {case}: {line:?} width={width} scroll={scroll}"
            );
            if scroll == 0 {
                let lines = [line.clone(), Line::from("second row 界"), line.clone()];
                let mut expected = Buffer::filled(outer, ratatui::buffer::Cell::new("x"));
                let mut actual = expected.clone();
                Paragraph::new(lines.to_vec())
                    .style(style)
                    .render(area, &mut expected);
                render_lines(&mut actual, area, &lines, style);
                assert_eq!(actual, expected, "case {case} as lines");
            }
        }
    }
}
