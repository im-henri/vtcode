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
    pub pointer_buttons: Vec<String>,
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
    let bg = match span.background.as_str() {
        "panel" | "header" => colors.bg_sidebar,
        "selection" => colors.bg_selection,
        "cursor" => colors.bg_cursor,
        "match" => colors.bg_search_match,
        _ => colors.bg_editor,
    };
    let selection_foreground = (span.background == "selection" || span.role == "selection")
        .then(|| colors.ui.editor_selection_foreground.over(bg)).flatten();
    let fg = if span.background == "cursor" || span.role == "cursor" {
        colors.fg_cursor
    } else if let Some(foreground) = selection_foreground {
        foreground
    } else if span.background == "match" && colors.ui.fg_find_other.is_some() {
        colors.ui.fg_find_other.unwrap()
    } else {
        match span.role.as_str() {
            "chart1" => colors.ui.chart_color(0, bg),
            "chart2" => colors.ui.chart_color(1, bg),
            "chart3" => colors.ui.chart_color(2, bg),
            "chart4" => colors.ui.chart_color(3, bg),
            "chart5" => colors.ui.chart_color(4, bg),
            "chart6" => colors.ui.chart_color(5, bg),
            "chart7" => colors.ui.chart_color(6, bg),
            "chart8" => colors.ui.chart_color(7, bg),
            "muted" => colors.ui.description_color(bg),
            "accent" | "offset" => colors.ui.focus_border.over(bg).unwrap_or(colors.fg_accent),
            "changed" | "warning" => colors.ui.warning_text_color(bg),
            "error" => colors.ui.error_text_color(bg),
            "success" => colors.fg_success,
            _ => colors.ui.editor_text_color(bg),
        }
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
    buffer.set_style(area, Style::default().fg(colors.fg_text).bg(colors.bg_editor));
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
                .style(Style::default().fg(colors.fg_text).bg(colors.bg_editor))
                .render(area, &mut expected);
            assert_eq!(actual, expected, "{width} columns");
        }
    }
}
