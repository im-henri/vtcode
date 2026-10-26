//! Serializable, theme-aware terminal surfaces contributed by extensions.
use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ViewFrame {
    #[serde(default)]
    pub width: u16,
    #[serde(default)]
    pub height: u16,
    #[serde(default)]
    pub lines: Vec<Vec<ViewSpan>>,
    pub cursor: Option<(u16, u16)>,
    pub document: Option<u32>,
    pub title: Option<String>,
    #[serde(default)]
    pub dirty: bool,
    pub path: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ViewSpan {
    pub text: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub background: String,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub underline: bool,
}

fn surface_colors(
    colors: &crate::app_theme::ThemeColors,
    span: &ViewSpan,
) -> (ratatui::style::Color, ratatui::style::Color) {
    use ratatui::style::Color;
    let fg = match span.role.as_str() {
        "chart1" => Color::Rgb(106, 203, 145),
        "chart2" => Color::Rgb(106, 184, 255),
        "chart3" => Color::Rgb(232, 196, 104),
        "chart4" => Color::Rgb(194, 151, 235),
        "chart5" => Color::Rgb(239, 145, 181),
        "chart6" => Color::Rgb(104, 204, 200),
        "chart7" => Color::Rgb(234, 170, 121),
        "chart8" => Color::Rgb(184, 197, 125),
        "muted" => colors.fg_dim,
        "accent" | "offset" => colors.fg_accent,
        "changed" | "warning" => colors.fg_warning,
        "error" => colors.fg_error,
        "success" => colors.fg_success,
        "cursor" => colors.bg_editor,
        _ => colors.fg_text,
    };
    let bg = match span.background.as_str() {
        "panel" | "header" => colors.bg_sidebar,
        "selection" => colors.bg_selection,
        "cursor" => colors.fg_accent,
        "match" => colors.bg_search_match,
        _ => colors.bg_editor,
    };
    (fg, bg)
}

/// Structured terminal surfaces are already laid out. Paint their spans without
/// constructing Paragraph/Line trees or remeasuring every plot character.
pub(crate) fn render_frame(
    buffer: &mut ratatui::buffer::Buffer,
    area: ratatui::layout::Rect,
    frame: &ViewFrame,
    colors: &crate::app_theme::ThemeColors,
) {
    use ratatui::style::{Modifier, Style};
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    let area = area.intersection(buffer.area);
    if area.is_empty() {
        return;
    }
    buffer.set_style(area, Style::default().bg(colors.bg_editor));
    let mut styles = rustc_hash::FxHashMap::default();
    for (row, line) in frame
        .lines
        .iter()
        .take(usize::from(area.height))
        .enumerate()
    {
        let y = area.y + row as u16;
        let mut x = area.x;
        'spans: for span in line {
            if x >= area.right() {
                break;
            }
            let (fg, bg) = surface_colors(colors, span);
            let style = *styles
                .entry((fg, bg, span.bold, span.underline))
                .or_insert_with(|| {
                    let mut style = Style::default()
                        .fg(colors.ui.readable_foreground(fg, bg, 4.5))
                        .bg(bg);
                    if span.bold {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    if span.underline {
                        style = style.add_modifier(Modifier::UNDERLINED);
                    }
                    style
                });
            let remaining = usize::from(area.right() - x);
            // ASCII and box/plot glyphs have exactly one column per character.
            // General text follows Paragraph's grapheme and clipping rules.
            if span.text.is_ascii()
                || span
                    .text
                    .chars()
                    .all(|ch| ch.is_ascii() || ('\u{2500}'..='\u{259f}').contains(&ch))
            {
                let start = buffer.index_of(x, y);
                let cells = &mut buffer.content[start..start + remaining];
                for (cell, ch) in cells.iter_mut().zip(span.text.chars()) {
                    cell.set_char(ch).set_style(style);
                    x += 1;
                }
            } else {
                for symbol in span.text.graphemes(true) {
                    let width = symbol.width();
                    if width == 0 || width > usize::from(area.width) {
                        continue;
                    }
                    if width > usize::from(area.right() - x) {
                        break 'spans;
                    }
                    buffer[(x, y)].set_symbol(symbol).set_style(style);
                    x += width as u16;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_surface_paint_matches_paragraph_for_offsets_styles_unicode_and_clipping() {
        use ratatui::{
            buffer::Buffer,
            layout::Rect,
            style::{Modifier, Style},
            text::{Line, Span},
            widgets::{Paragraph, Widget},
        };
        let root = tempfile::tempdir().unwrap();
        let colors = crate::app::App::new(root.path().to_path_buf()).colors;
        let frame = ViewFrame {
            lines: vec![
                vec![
                    ViewSpan {
                        text: "clock ──╱──▁▁   ".into(),
                        role: "chart1".into(),
                        ..ViewSpan::default()
                    },
                    ViewSpan {
                        text: "ABCD".into(),
                        role: "warning".into(),
                        background: "selection".into(),
                        bold: true,
                        underline: true,
                    },
                ],
                vec![ViewSpan {
                    text: "核 e\u{301} 👩‍💻 αβ".into(),
                    role: "accent".into(),
                    background: "header".into(),
                    ..ViewSpan::default()
                }],
                vec![
                    ViewSpan {
                        text: "\u{301}".into(),
                        ..ViewSpan::default()
                    },
                    ViewSpan {
                        text: "界a界b".into(),
                        role: "chart8".into(),
                        ..ViewSpan::default()
                    },
                ],
            ],
            ..ViewFrame::default()
        };
        for width in 1..=30 {
            let area = Rect::new(3, 2, width, 4);
            let mut actual = Buffer::empty(Rect::new(0, 0, 40, 8));
            let mut expected = actual.clone();
            render_frame(&mut actual, area, &frame, &colors);
            let lines: Vec<Line<'_>> = frame
                .lines
                .iter()
                .map(|line| {
                    Line::from(
                        line.iter()
                            .map(|span| {
                                let (fg, bg) = surface_colors(&colors, span);
                                let mut style = Style::default()
                                    .fg(colors.ui.readable_foreground(fg, bg, 4.5))
                                    .bg(bg);
                                if span.bold {
                                    style = style.add_modifier(Modifier::BOLD);
                                }
                                if span.underline {
                                    style = style.add_modifier(Modifier::UNDERLINED);
                                }
                                Span::styled(span.text.as_str(), style)
                            })
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
            Paragraph::new(lines)
                .style(Style::default().bg(colors.bg_editor))
                .render(area, &mut expected);
            assert_eq!(actual, expected, "{width} columns");
        }
    }
}
