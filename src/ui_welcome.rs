use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::{app::App, app_theme::lerp_color};

const LOGO_LARGE: &[&str] = &[
    "███           ███",
    " ███         ███",
    "  ███       ███",
    "   ███     ███",
    "    ███   ███",
    "     ███ ███",
    "      █████",
];

const LOGO_MEDIUM: &[&str] = &[
    "██       ██",
    " ██     ██",
    "  ██   ██",
    "   ██ ██",
    "    ███",
];

const LOGO_COMPACT: &[&str] = &["█   █", " █ █", "  █"];
const LOGO_TINY: &[&str] = &["◆"];

fn logo_width(logo: &[&str]) -> usize {
    logo.iter()
        .map(|line| unicode_width::UnicodeWidthStr::width(*line))
        .max()
        .unwrap_or(0)
}

fn logo_for(area: Rect) -> &'static [&'static str] {
    [LOGO_LARGE, LOGO_MEDIUM, LOGO_COMPACT, LOGO_TINY]
        .into_iter()
        .find(|logo| {
            logo_width(logo) + 4 <= usize::from(area.width)
                && logo.len() + 2 <= usize::from(area.height)
        })
        .unwrap_or(&[])
}

fn draw_logo(f: &mut Frame, area: Rect, mark_color: Color) {
    let logo = logo_for(area);
    if logo.is_empty() {
        return;
    }

    let lines: Vec<Line> = logo
        .iter()
        .map(|line| Line::from(Span::styled(*line, Style::default().fg(mark_color))))
        .collect();
    let height = logo.len() as u16;
    let logo_area = Rect::new(
        area.x,
        area.y + area.height.saturating_sub(height) / 2,
        area.width,
        height.min(area.height),
    );

    f.render_widget(
        Paragraph::new(lines).alignment(Alignment::Center),
        logo_area,
    );
}

/// Use the resolved startup palette so the logo and workbench share a background.
#[cfg(test)]
pub(crate) fn draw_startup_screen(f: &mut Frame, area: Rect, colors: &crate::app_theme::ThemeColors) {
    let background = colors.bg_editor;
    f.render_widget(Block::default().style(Style::default().bg(background)), area);
    draw_logo(f, area, lerp_color(background, colors.fg_accent_blue, 0.38));
}

/// First paint needs no App, settings, disk access, or parsed theme. Keep the
/// terminal's own background until the configured workbench palette is ready.
pub(crate) fn draw_bootstrap_screen(f: &mut Frame) {
    let area = f.area();
    f.render_widget(Block::default().style(Style::default().bg(Color::Reset)), area);
    draw_logo(f, area, Color::Reset);
}

/// Render the intentionally minimal empty-editor state.
pub(crate) fn draw_welcome_screen(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    apply_gradients: impl FnOnce(&mut Frame, Rect),
) {
    let block = Block::default()
        .borders(Borders::NONE)
        .style(Style::default().bg(app.colors.bg_editor));
    let inner = block.inner(area);
    f.render_widget(block, area);
    apply_gradients(f, area);

    app.code_area = None;

    // Keep the mark quiet like a watermark while preserving enough contrast
    // across both bundled and imported light/dark themes.
    let mark_color = lerp_color(app.colors.bg_editor, app.colors.fg_accent_blue, 0.38);
    draw_logo(f, inner, mark_color);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn welcome_is_a_centered_logo_without_copy_or_recent_files() {
        let width = 80;
        let height = 24;
        let mut app = App::test_new();
        app.recent_files = vec!["should-not-render.rs".into()];
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");

        terminal
            .draw(|frame| {
                let area = frame.area();
                draw_welcome_screen(frame, &mut app, area, |_, _| {});
            })
            .expect("welcome draw");

        let buffer = terminal.backend().buffer();
        let mut visible = Vec::new();
        let mut rendered = String::new();
        for y in 0..height {
            for x in 0..width {
                let symbol = buffer.cell((x, y)).expect("cell").symbol();
                rendered.push_str(symbol);
                if symbol != " " {
                    visible.push((x, y, symbol.to_string()));
                }
            }
            rendered.push('\n');
        }

        assert!(!visible.is_empty());
        assert!(visible.iter().all(|(_, _, symbol)| symbol == "█"));
        for removed_copy in [
            "VTCode",
            "Terminal-Powered",
            "Recent Files",
            "Quick Start",
            "should-not-render.rs",
        ] {
            assert!(!rendered.contains(removed_copy));
        }

        let min_x = visible.iter().map(|(x, _, _)| *x).min().unwrap();
        let max_x = visible.iter().map(|(x, _, _)| *x).max().unwrap();
        let min_y = visible.iter().map(|(_, y, _)| *y).min().unwrap();
        let max_y = visible.iter().map(|(_, y, _)| *y).max().unwrap();
        assert!((i32::from(min_x + max_x) - i32::from(width - 1)).abs() <= 1);
        assert!((i32::from(min_y + max_y) - i32::from(height - 1)).abs() <= 1);
    }

    #[test]
    fn welcome_logo_has_responsive_marks_for_small_editor_areas() {
        assert_eq!(logo_for(Rect::new(0, 0, 80, 24)), LOGO_LARGE);
        assert_eq!(logo_for(Rect::new(0, 0, 15, 8)), LOGO_MEDIUM);
        assert_eq!(logo_for(Rect::new(0, 0, 9, 5)), LOGO_COMPACT);
        assert_eq!(logo_for(Rect::new(0, 0, 5, 3)), LOGO_TINY);
        assert!(logo_for(Rect::new(0, 0, 4, 2)).is_empty());
    }

    #[test]
    fn startup_frame_is_a_logo_without_loading_copy() {
        let width = 80;
        let height = 24;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        terminal
            .draw(|frame| draw_startup_screen(frame, frame.area(), &App::test_new().colors))
            .expect("startup draw");

        let buffer = terminal.backend().buffer();
        let rendered = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .map(|position| buffer.cell(position).expect("cell").symbol())
            .collect::<String>();
        assert!(rendered.contains('█'));
        assert!(!rendered.contains("Loading"));
        assert!(!rendered.contains("VTCode"));
    }
}
