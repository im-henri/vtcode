//! Paint completed rows immediately and recycle their temporary text. Nothing
//! retained here describes a document, theme, selection or earlier frame.
use super::*;
use std::{borrow::Cow, cell::RefCell};

const MAX_STRINGS: usize = 256;
const MAX_STRING_BYTES: usize = 4096;
const MAX_SCRATCH_BYTES: usize = 128 * 1024;

#[derive(Default)]
struct Scratch {
    strings: Vec<String>,
    bytes: usize,
}

thread_local! {
    static SCRATCH: RefCell<Scratch> = RefCell::default();
}

pub(in crate::ui_editor) struct Rows {
    area: Rect,
    style: Style,
    count: usize,
    scratch: Scratch,
}

impl Rows {
    pub(in crate::ui_editor) fn new(area: Rect, style: Style) -> Self {
        Self {
            area,
            style,
            count: 0,
            scratch: SCRATCH.with(|slot| slot.take()),
        }
    }

    pub(in crate::ui_editor) fn len(&self) -> usize {
        self.count
    }

    pub(in crate::ui_editor) fn flush<'a>(
        &mut self,
        spans: &mut Vec<Span<'a>>,
        batch: &mut String,
        style: Style,
    ) {
        if batch.is_empty() {
            return;
        }
        let spare = self.scratch.strings.pop().unwrap_or_default();
        self.scratch.bytes -= spare.capacity();
        spans.push(Span::styled(std::mem::replace(batch, spare), style));
    }

    pub(in crate::ui_editor) fn push(
        &mut self,
        buffer: &mut Buffer,
        spans: &mut Vec<Span<'_>>,
        line_style: Style,
    ) {
        // Match the complete renderer's clipping, including an offset buffer.
        let area = self.area.intersection(buffer.area);
        if self.count < usize::from(area.height) && area.width > 0 {
            let row = Rect::new(area.x, area.y + self.count as u16, area.width, 1);
            #[cfg(test)]
            let reference = REFERENCE.get();
            #[cfg(not(test))]
            let reference = false;
            if reference {
                Paragraph::new(Line::from(spans.clone()).style(line_style))
                    .style(self.style)
                    .render(row, buffer);
            } else {
                paint_row(buffer, row, spans, line_style, self.style);
            }
        }
        self.count += 1;
        // Drain retains the span vector; moving strings keeps their buffers.
        for span in spans.drain(..) {
            if let Cow::Owned(mut text) = span.content {
                let bytes = text.capacity();
                if bytes > 0
                    && bytes <= MAX_STRING_BYTES
                    && self.scratch.strings.len() < MAX_STRINGS
                    && self.scratch.bytes + bytes <= MAX_SCRATCH_BYTES
                {
                    text.clear();
                    self.scratch.strings.push(text);
                    self.scratch.bytes += bytes;
                }
            }
        }
    }

    pub(in crate::ui_editor) fn finish(&mut self, buffer: &mut Buffer) {
        #[cfg(test)]
        let reference = REFERENCE.get();
        #[cfg(not(test))]
        let reference = false;
        if !reference {
            let area = self.area.intersection(buffer.area);
            if area.width > 0 {
                for row in self.count..usize::from(area.height) {
                    let start = buffer.index_of(area.x, area.y + row as u16);
                    for cell in &mut buffer.content[start..start + usize::from(area.width)] {
                        cell.set_char(' ').set_style(self.style);
                    }
                }
            }
            self.count = self.count.max(usize::from(area.height));
            return;
        }
        let mut empty = Vec::with_capacity(1);
        while self.count < usize::from(self.area.intersection(buffer.area).height) {
            empty.push(Span::raw(crate::helpers::spaces(usize::from(
                self.area.width,
            ))));
            self.push(buffer, &mut empty, Style::default());
        }
    }
}

impl Drop for Rows {
    fn drop(&mut self) {
        SCRATCH.with(|slot| *slot.borrow_mut() = std::mem::take(&mut self.scratch));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    #[test]
    fn blank_tail_matches_span_rendering_over_styled_cells_and_clipped_views() {
        for area in [Rect::default(), Rect::new(0, 0, 18, 8), Rect::new(5, 3, 12, 5),
            Rect::new(14, 6, 20, 12), Rect::new(30, 20, 8, 5)]
        {
            for count in [0, 1, 3, 12] {
                let mut before = Buffer::empty(Rect::new(3, 2, 20, 10));
                for cell in &mut before.content {
                    cell.set_symbol("界").set_style(Style::default().fg(Color::Yellow)
                        .bg(Color::Red).add_modifier(Modifier::ITALIC | Modifier::DIM));
                }
                let style = Style::default().fg(Color::Cyan).bg(Color::Blue)
                    .remove_modifier(Modifier::DIM);
                let mut expected = before.clone();
                let mut actual = before;
                for (buffer, reference) in [(&mut expected, true), (&mut actual, false)] {
                    REFERENCE.set(reference);
                    let mut rows = Rows::new(area, style);
                    rows.count = count;
                    rows.finish(buffer);
                    assert_eq!(rows.count, count.max(usize::from(area.intersection(buffer.area).height)));
                }
                REFERENCE.set(false);
                assert_eq!(actual, expected, "area={area:?}, count={count}");
            }
        }
    }

    #[test]
    fn completed_rows_match_whole_paragraph_and_reuse_bounded_storage() {
        for width in [0, 1, 2, 7, 80] {
            for origin in [(0, 0), (3, 2)] {
                let area = Rect::new(origin.0, origin.1, width, 8);
                let style = Style::default()
                    .bg(Color::Blue)
                    .remove_modifier(Modifier::DIM);
                let mut expected = Buffer::empty(Rect::new(0, 0, 100, 12));
                let mut actual = expected.clone();
                let mut lines = Vec::new();
                let mut rows = Rows::new(area, style);
                let mut spans = Vec::with_capacity(64);
                let vector = spans.as_ptr();
                let mut batch = String::new();
                for row in 0..8 {
                    for text in ["abc", "界", "e\u{301}", " ", "👩‍💻", "\t\r", "end"] {
                        batch.push_str(text);
                        rows.flush(
                            &mut spans,
                            &mut batch,
                            Style::default().fg(Color::Indexed(row)),
                        );
                    }
                    lines.push(Line::from(spans.clone()));
                    rows.push(&mut actual, &mut spans, Style::default());
                    assert!(spans.is_empty());
                    assert_eq!(spans.as_ptr(), vector);
                }
                rows.finish(&mut actual);
                Paragraph::new(lines)
                    .style(style)
                    .render(area, &mut expected);
                assert_eq!(actual, expected);
                assert!(rows.scratch.strings.iter().all(String::is_empty));
                assert!(rows.scratch.strings.len() <= MAX_STRINGS);
                assert!(rows.scratch.bytes <= MAX_SCRATCH_BYTES);
                assert_eq!(
                    rows.scratch.bytes,
                    rows.scratch
                        .strings
                        .iter()
                        .map(String::capacity)
                        .sum::<usize>()
                );
                spans.extend((0..1000).map(|_| Span::raw("x".repeat(5000))));
                rows.push(&mut actual, &mut spans, Style::default());
                assert!(
                    rows.scratch
                        .strings
                        .iter()
                        .all(|text| text.capacity() <= MAX_STRING_BYTES)
                );
            }
        }
    }

    #[test]
    fn pooled_rows_do_not_reuse_text_or_styles_between_frames_or_nested_draws() {
        let area = Rect::new(0, 0, 15, 4);
        for frame in 0..10 {
            let mut buffer = Buffer::empty(area);
            let mut rows = Rows::new(area, Style::default().bg(Color::Indexed(frame)));
            let mut spans = vec![Span::styled(
                format!("frame {frame}"),
                Style::default().fg(Color::Red),
            )];
            rows.push(&mut buffer, &mut spans, Style::default());
            {
                let _nested = Rows::new(area, Style::default());
            }
            rows.finish(&mut buffer);
            assert_eq!(buffer[(6, 0)].symbol(), frame.to_string());
            assert_eq!(buffer[(6, 0)].fg, Color::Red);
            for y in 1..4 {
                for x in 0..15 {
                    assert_eq!(buffer[(x, y)].symbol(), " ");
                    assert_eq!(buffer[(x, y)].bg, Color::Indexed(frame));
                }
            }
        }
    }
}
