//! Paint fixed-width gutter cells without allocating a Line and owned number
//! for each row. Numbers and indicators never wrap or span multiple graphemes.
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    text::Span,
};

#[cfg(test)]
thread_local! {
    pub(crate) static REFERENCE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, style::Modifier};

    fn render(width: u16, direct: bool) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(24, 8)).unwrap();
        terminal.draw(|frame| {
            let area = Rect::new(3, 2, width, 4);
            for cell in &mut frame.buffer_mut().content {
                cell.set_char('x').set_style(Style::default().fg(Color::Cyan)
                    .add_modifier(Modifier::ITALIC));
            }
            let mut rows = Rows::new(frame, area, Color::Blue);
            for (index, text) in [" 12345 ", "12e\u{301}45", "  界 🦀", "\t1\r2\0"].iter().enumerate() {
                let spans = [Span::styled(*text, Style::default().fg(Color::Yellow)),
                    Span::styled(" ›", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))];
                if direct { rows.push(frame, spans); }
                else {
                    let mut x = area.x;
                    for span in spans {
                        if x >= area.right() { break; }
                        x = frame.buffer_mut().set_stringn(x, area.y + index as u16,
                            span.content, usize::from(area.right() - x), span.style).0;
                    }
                }
            }
        }).unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn gutter_ascii_cells_match_original_numbers_unicode_controls_and_clipping() {
        for width in [0, 1, 2, 3, 6, 12] {
            assert_eq!(render(width, true), render(width, false), "width={width}");
        }
    }
}

pub(super) struct Rows {
    area: Rect,
    row: usize,
    #[cfg(test)]
    background: Color,
    #[cfg(test)]
    reference: Option<Vec<ratatui::text::Line<'static>>>,
}

impl Rows {
    pub(super) fn new(frame: &mut Frame, area: Rect, background: Color) -> Self {
        frame
            .buffer_mut()
            .set_style(area, Style::default().bg(background));
        Self {
            area,
            row: 0,
            #[cfg(test)]
            background,
            #[cfg(test)]
            reference: REFERENCE.get().then(Vec::new),
        }
    }

    pub(super) fn len(&self) -> usize {
        self.row
    }

    pub(super) fn push<'a>(
        &mut self,
        frame: &mut Frame,
        spans: impl IntoIterator<Item = Span<'a>>,
    ) {
        let row = self.row;
        self.row += 1;
        #[cfg(test)]
        if let Some(lines) = &mut self.reference {
            lines.push(ratatui::text::Line::from(
                spans
                    .into_iter()
                    .map(|span| Span::styled(span.content.into_owned(), span.style))
                    .collect::<Vec<_>>(),
            ));
            return;
        }
        if row >= usize::from(self.area.height) {
            return;
        }
        let y = self.area.y + row as u16;
        let mut x = self.area.x;
        let buffer = frame.buffer_mut();
        for span in spans {
            if x >= self.area.right() {
                break;
            }
            let remaining = usize::from(self.area.right() - x);
            let bytes = span.content.as_bytes();
            // Numbers and gutter padding need no grapheme segmentation. One
            // byte of lookahead preserves combining marks at the clip edge.
            if bytes[..bytes.len().min(remaining + 1)].iter()
                .all(|&byte| (b' '..=b'~').contains(&byte))
            {
                let count = bytes.len().min(remaining);
                let start = buffer.index_of(x, y);
                for (cell, &byte) in buffer.content[start..start + count].iter_mut().zip(bytes) {
                    cell.set_char(char::from(byte)).set_style(span.style);
                }
                x += count as u16;
                continue;
            }
            x = buffer
                .set_stringn(
                    x,
                    y,
                    &span.content,
                    usize::from(self.area.right() - x),
                    span.style,
                )
                .0;
        }
    }

    pub(super) fn finish(self, _frame: &mut Frame) {
        #[cfg(test)]
        if let Some(lines) = self.reference {
            _frame.render_widget(
                ratatui::widgets::Paragraph::new(lines).style(Style::default().bg(self.background)),
                self.area,
            );
        }
    }
}
