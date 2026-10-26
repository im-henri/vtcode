use std::time::{Duration, Instant};

use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Style},
    widgets::Paragraph,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::App;

/// Keep the duration visible even when a long filename needs an ellipsis.
pub(crate) fn clip_middle(text: &str, width: usize) -> String {
    let clean: String = text
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    if UnicodeWidthStr::width(clean.as_str()) <= width {
        return clean;
    }
    if width == 0 {
        return String::new();
    }
    let left_budget = (width - 1).div_ceil(2);
    let right_budget = (width - 1) / 2;
    let mut left = String::new();
    let mut used = 0;
    for grapheme in clean.graphemes(true) {
        let cells = UnicodeWidthStr::width(grapheme);
        if used + cells > left_budget {
            break;
        }
        left.push_str(grapheme);
        used += cells;
    }
    let mut right = Vec::new();
    used = 0;
    for grapheme in clean.graphemes(true).rev() {
        let cells = UnicodeWidthStr::width(grapheme);
        if used + cells > right_budget {
            break;
        }
        right.push(grapheme);
        used += cells;
    }
    format!("{left}…{}", right.into_iter().rev().collect::<String>())
}

pub(crate) struct StatusBadge {
    pub width: u16,
    icon: &'static str,
    accent: Color,
}

fn activity_symbol(elapsed: Duration) -> &'static str {
    ["◐", "◓", "◑", "◒"][elapsed.as_secs() as usize % 4]
}

/// A single colored dot keeps the bar quiet. The busy shape changes on the
/// existing activity tick; all numbers and task context stay in the details.
pub(crate) fn status_badge(app: &App, width: usize, now: Instant) -> StatusBadge {
    let monitor = &app.background.activity;
    let elapsed = monitor
        .busy_since(now)
        .map(|start| now.saturating_duration_since(start));
    let timing = &monitor.render;
    let accent = if timing.has_samples() && timing.average_ms >= 50.0 {
        app.colors.fg_error
    } else if (timing.has_samples() && timing.average_ms >= 16.7)
        || elapsed.is_some_and(|elapsed| elapsed >= Duration::from_secs(10))
    {
        app.colors.fg_warning
    } else if timing.has_samples() {
        app.colors.fg_success
    } else {
        app.colors.fg_statusbar
    };
    StatusBadge {
        width: width.min(3) as u16,
        icon: elapsed.map_or("●", activity_symbol),
        accent,
    }
}

pub(crate) fn draw_badge(f: &mut Frame, app: &mut App, area: Rect, badge: StatusBadge) {
    if area.is_empty() {
        return;
    }
    let bg = app.colors.bg_statusbar;
    let hovered = app
        .status_bar_items
        .iter()
        .enumerate()
        .any(|(index, &(_, _, action))| {
            action == "performance" && app.hovered_status_item == Some(index)
        })
        || app.background.activity.details_open;
    let bg = if hovered {
        if app.colors.bg_statusbar_item == bg && !app.colors.ui.preserve_theme_colors {
            crate::app_theme::lerp_color(bg, app.colors.fg_statusbar, 0.12)
        } else {
            app.colors.bg_statusbar_item
        }
    } else {
        bg
    };
    let accent = app.colors.ui.readable_foreground(badge.accent, bg, 4.5);
    f.render_widget(
        Paragraph::new(badge.icon)
            .alignment(Alignment::Center)
            .style(Style::default().fg(accent).bg(bg)),
        area,
    );
}

#[cfg(test)]
use crate::ui_performance_details::close_area;
pub(crate) use crate::ui_performance_details::draw;

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };
    use ratatui::{Terminal, backend::TestBackend};

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn displayed(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn performance_dot_uses_latency_colors_without_showing_numbers() {
        let mut app = App::test_new();
        for (ms, expected) in [
            (2.4, app.colors.fg_success),
            (20.0, app.colors.fg_warning),
            (75.0, app.colors.fg_error),
        ] {
            app.background.activity.render.reset();
            app.background
                .activity
                .render
                .record(Duration::from_secs_f64(ms / 1_000.0));
            let badge = status_badge(&app, 100, Instant::now());
            assert_eq!(badge.width, 3);
            assert_eq!(badge.accent, expected);
            let mut terminal = Terminal::new(TestBackend::new(3, 1)).unwrap();
            terminal
                .draw(|f| draw_badge(f, &mut app, Rect::new(0, 0, 3, 1), badge))
                .unwrap();
            assert_eq!(displayed(&terminal).trim(), "●");
        }
        app.background.activity.render.reset();
        app.background
            .activity
            .render
            .record(Duration::from_micros(2_400));
        let _ticket = app.background.activity.start("Opening", None);
        let start = app.background.activity.active[0].started;
        app.background.refresh_activity(start);
        let busy = status_badge(&app, 100, start + Duration::from_secs(1));
        assert_ne!(busy.icon, "●");
        assert_eq!(busy.accent, app.colors.fg_success);
        let long = status_badge(&app, 100, start + Duration::from_secs(12));
        assert_ne!(long.icon, "●");
        assert_eq!(long.accent, app.colors.fg_warning);
    }

    #[test]
    fn performance_unicode_labels_preserve_the_duration_and_never_overflow() {
        let text = " ◐ Highlighting very-long-文件名-e\u{301}-👩‍💻.rs · 12s +2 ";
        for width in 0..80 {
            let clipped = clip_middle(text, width);
            assert!(UnicodeWidthStr::width(clipped.as_str()) <= width);
            assert!(!clipped.contains('\n'));
            if width >= 24 {
                assert!(clipped.contains("12s +2"));
            }
        }
        assert_eq!(clip_middle("hello\nworld", 20), "hello world");
    }

    #[test]
    fn performance_status_chip_is_clickable_at_narrow_and_wide_sizes() {
        let mut app = App::test_new();
        let _ticket = app.background.activity.start(
            "Highlighting",
            Some(std::path::Path::new("very-long-文件名.rs")),
        );
        app.background.activity.active[0].started -= Duration::from_secs(12);
        app.background.refresh_activity(Instant::now());
        for width in [1, 4, 12, 24, 50, 80, 200] {
            let area = Rect::new(0, 1, width, 1);
            app.status_bar_area = Some(area);
            let mut terminal = Terminal::new(TestBackend::new(width, 2)).unwrap();
            terminal
                .draw(|f| crate::ui_panels::draw_status_bar(f, &mut app, area))
                .unwrap();
            let &(x, cells, action) = app.status_bar_items.last().unwrap();
            assert_eq!(action, "performance");
            assert_eq!(x + cells, width);
            assert!(cells > 0);
            for &(other_x, other_width, other_action) in &app.status_bar_items {
                if other_action != "performance" && other_width > 0 {
                    assert!(other_x + other_width <= x);
                }
            }
            assert_eq!(cells, width.min(3));
            let indicator: String = (x..x + cells)
                .map(|x| terminal.backend().buffer()[(x, area.y)].symbol())
                .collect();
            assert_eq!(indicator.trim().chars().count(), 1);
            assert!(!indicator.chars().any(|ch| ch.is_ascii_alphanumeric()));
            crate::mouse::handle_mouse(
                &mut app,
                mouse(MouseEventKind::Down(MouseButton::Left), x, area.y),
            );
            assert!(
                app.background.activity.details_open,
                "chip click at {width} columns"
            );
            app.background.activity.details_open = false;
        }
    }

    #[test]
    fn performance_details_show_real_task_context_and_dismiss_without_editing() {
        let mut app = App::test_new();
        app.tabs = vec![crate::app::Tab::new_file(
            crate::editor::Editor::test_with_text("original text\n"),
        )];
        app.active_tab = 0;
        app.show_panel = false;
        let path = app.explorer.root.join("src").join("parser.rs");
        let _ticket = app.background.activity.start("Highlighting", Some(&path)).running();
        app.background.activity.active[0].started -= Duration::from_secs(7);
        app.background.refresh_activity(Instant::now());
        app.background
            .activity
            .render
            .record(Duration::from_micros(2_400));
        app.execute_palette_command("performance");
        let mut terminal = Terminal::new(TestBackend::new(110, 28)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let text = displayed(&terminal);
        for expected in [
            "Performance & activity",
            "Highlighting",
            "parser.rs",
            "7s",
            "2.4ms",
            "Background work",
        ] {
            assert!(text.contains(expected), "missing {expected}");
        }
        let area = app.performance_area.unwrap();
        crate::mouse::handle_mouse(
            &mut app,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                area.x + 3,
                area.y + 2,
            ),
        );
        assert!(app.background.activity.details_open);
        assert_eq!(app.editor().unwrap().rope.to_string(), "original text\n");
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(!app.background.activity.details_open);
        assert!(app.performance_area.is_none());
        app.execute_palette_command("performance");
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let area = app.performance_area.unwrap();
        let close = close_area(area);
        crate::mouse::handle_mouse(
            &mut app,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                close.x + 1,
                close.y,
            ),
        );
        assert!(!app.background.activity.details_open);
        assert_eq!(app.editor().unwrap().rope.to_string(), "original text\n");
    }

    #[test]
    fn performance_popover_stays_inside_small_terminals() {
        let mut app = App::test_new();
        app.background.activity.details_open = true;
        app.background
            .activity
            .render
            .record(Duration::from_micros(2_400));
        for (width, height) in [(1, 1), (8, 4), (20, 8), (40, 12), (120, 40)] {
            app.performance_area = None;
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            if let Some(area) = app.performance_area {
                assert_eq!(area.intersection(Rect::new(0, 0, width, height)), area);
            }
            if width >= 20 && height >= 8 {
                assert!(displayed(&terminal).contains("2.4ms"));
            }
        }
    }

    #[test]
    fn performance_busy_chip_keeps_its_width_when_the_clock_grows() {
        let mut app = App::test_new();
        let _ticket = app.background.activity.start(
            "Highlighting",
            Some(std::path::Path::new("文件名-e\u{301}-👩‍💻.rs")),
        );
        let start = app.background.activity.active[0].started;
        app.background.refresh_activity(start);
        for width in [0, 1, 2, 3, 12, 24, 40, 80, 160] {
            let mut previous_width = None;
            for seconds in [1, 9, 10, 59, 60, 599, 600, 3600] {
                let badge = status_badge(&app, width, start + Duration::from_secs(seconds));
                if let Some(previous_width) = previous_width {
                    assert_eq!(badge.width, previous_width);
                }
                previous_width = Some(badge.width);
                assert_eq!(badge.width, width.min(3) as u16);
            }
        }
    }

    #[test]
    fn performance_compact_chip_aggregates_work_without_names_or_paths() {
        let mut app = App::test_new();
        let file = std::path::Path::new("src/very-long-文件名-e\u{301}-👩‍💻.rs");
        let _first = app.background.activity.start("Highlighting", Some(file));
        let _second = app
            .background
            .activity
            .start("Saving", Some(std::path::Path::new("src/main.rs")));
        let _third = app.background.activity.start("Refreshing Git", None);
        app.background.activity.active[0].started -= Duration::from_secs(12);
        app.background.refresh_activity(Instant::now());
        app.background
            .activity
            .render
            .record(Duration::from_micros(2_400));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let area = Rect::new(0, 23, 80, 1);
        terminal
            .draw(|f| crate::ui_panels::draw_status_bar(f, &mut app, area))
            .unwrap();
        let &(x, width, action) = app.status_bar_items.last().unwrap();
        assert_eq!(action, "performance");
        assert_eq!(width, 3);
        let text: String = (x..x + width)
            .map(|x| terminal.backend().buffer()[(x, 23)].symbol())
            .collect();
        assert_eq!(text.trim().chars().count(), 1);
        assert!(!text.chars().any(|ch| ch.is_ascii_alphanumeric()));
        for absent in ["Highlighting", "Saving", "Git", "src", ".rs", "Ready"] {
            assert!(!text.contains(absent));
        }
        app.execute_palette_command("performance");
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        for expected in [
            "3 tasks active",
            "2 file tasks",
            "longest 12s",
            "Highlighting",
            "2.4ms",
        ] {
            assert!(
                displayed(&terminal).contains(expected),
                "missing {expected} in details"
            );
        }
        crate::ui_performance_details::handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        );
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        for expected in ["Highlighting", "Saving", "Refreshing Git", "Queued"] {
            assert!(
                displayed(&terminal).contains(expected),
                "missing {expected} in running tasks"
            );
        }
        crate::ui_control_visibility_tests::snapshot(
            terminal.backend().buffer(),
            "Compact activity - concurrent details",
        );
        app.background.activity.details_open = false;
        terminal
            .draw(|f| crate::ui_panels::draw_status_bar(f, &mut app, area))
            .unwrap();
        crate::ui_control_visibility_tests::snapshot(
            terminal.backend().buffer(),
            "Compact activity - concurrent bar",
        );
        // Counts and timings never expand the indicator, even during long jobs.
        let start = app.background.activity.active[0].started;
        app.background.activity.refresh(start, 1_000);
        app.background
            .activity
            .render
            .record(Duration::from_secs(1));
        for width in 0..=100 {
            let badge = status_badge(&app, width, start + Duration::from_secs(3_600));
            let mut tiny = Terminal::new(TestBackend::new(width as u16, 1)).unwrap();
            tiny.draw(|f| draw_badge(f, &mut app, Rect::new(0, 0, badge.width, 1), badge))
                .unwrap();
            assert!(width == 0 || displayed(&tiny).trim() == "◐");
        }
    }

    #[test]
    fn performance_hover_only_redraws_when_a_control_changes() {
        let mut app = App::test_new();
        app.execute_palette_command("performance");
        let mut terminal = Terminal::new(TestBackend::new(110, 28)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let area = app.performance_area.unwrap();
        app.hovered_status_item = Some(0);
        crate::mouse::handle_mouse(
            &mut app,
            mouse(MouseEventKind::Moved, area.x + 3, area.y + 3),
        );
        assert!(app.mouse_hover_changed);
        assert!(app.hovered_status_item.is_none());
        crate::mouse::handle_mouse(
            &mut app,
            mouse(MouseEventKind::Moved, area.x + 4, area.y + 3),
        );
        assert!(!app.mouse_hover_changed);
        let close = close_area(area);
        crate::mouse::handle_mouse(&mut app, mouse(MouseEventKind::Moved, close.x, close.y));
        assert!(app.mouse_hover_changed);
        crate::mouse::handle_mouse(&mut app, mouse(MouseEventKind::Moved, close.x + 1, close.y));
        assert!(!app.mouse_hover_changed);
        crate::mouse::handle_mouse(
            &mut app,
            mouse(MouseEventKind::Moved, area.x + 4, area.y + 3),
        );
        assert!(app.mouse_hover_changed);
    }

    #[test]
    fn performance_open_card_stays_put_when_background_work_changes() {
        let mut app = App::test_new();
        app.execute_palette_command("performance");
        let mut terminal = Terminal::new(TestBackend::new(110, 28)).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let initial = app.performance_area.unwrap();
        let _ticket = app
            .background
            .activity
            .start("Highlighting", Some(std::path::Path::new("parser.rs")));
        app.background.activity.active[0].started -= Duration::from_secs(7);
        app.background.refresh_activity(Instant::now());
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(app.performance_area, Some(initial));
        assert!(displayed(&terminal).contains("parser.rs"));
        app.background.activity.active[0].finish_for_test(Duration::from_secs(7));
        app.background.refresh_activity(Instant::now());
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(app.performance_area, Some(initial));
        assert!(displayed(&terminal).contains("All caught up"));
        assert!(displayed(&terminal).contains("parser.rs"));
    }

    #[test]
    fn performance_surfaces_follow_the_theme_and_keep_text_readable() {
        use crate::ui_control_visibility_tests::{
            assert_readable, audit_themes, check_region, snapshot,
        };
        let mut app = App::test_new();
        app.tabs = vec![crate::app::Tab::new_file(
            crate::editor::Editor::test_with_text(
                "// Lightweight activity, right where you need it.\nfn main() {\n    let message = \"Ready when you are\";\n    println!(\"{message}\");\n}\n",
            ),
        )];
        app.active_tab = 0;
        app.show_panel = false;
        app.show_explorer = false;
        app.show_minimap = false;
        app.editor_mut().unwrap().file_path = Some(app.explorer.root.join("src/main.rs"));
        app.background
            .activity
            .render
            .record(Duration::from_micros(2_400));
        let mut failures = Vec::new();
        for theme in audit_themes() {
            app.colors = theme.to_theme_colors();
            app.background.activity.details_open = false;
            app.performance_area = None;
            let mut terminal = Terminal::new(TestBackend::new(110, 28)).unwrap();
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            let &(x, width, _) = app.status_bar_items.last().unwrap();
            let badge_area = Rect::new(x, 27, width, 1);
            check_region(
                terminal.backend().buffer(),
                badge_area,
                &format!("{}: idle activity", theme.name),
                &mut failures,
                &app.colors.ui,
            );
            snapshot(
                terminal.backend().buffer(),
                &format!("{} - performance idle", theme.name),
            );
            let path = app.explorer.root.join("src/parser.rs");
            let _ticket = app.background.activity.start("Highlighting", Some(&path));
            app.background.activity.active[0].started -= Duration::from_secs(7);
            app.background.refresh_activity(Instant::now());
            for ms in [
                2.4, 2.1, 3.2, 4.8, 2.7, 1.9, 2.2, 6.5, 3.1, 2.6, 2.3, 2.7, 2.4, 2.1, 1.8, 2.4,
            ] {
                app.background
                    .activity
                    .render
                    .record(Duration::from_secs_f64(ms / 1_000.0));
            }
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            let &(x, width, _) = app.status_bar_items.last().unwrap();
            check_region(
                terminal.backend().buffer(),
                Rect::new(x, 27, width, 1),
                &format!("{}: busy activity", theme.name),
                &mut failures,
                &app.colors.ui,
            );
            snapshot(
                terminal.backend().buffer(),
                &format!("{} - performance busy", theme.name),
            );
            app.execute_palette_command("performance");
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            let area = app.performance_area.unwrap();
            let &(x, width, _) = app.status_bar_items.last().unwrap();
            check_region(
                terminal.backend().buffer(),
                Rect::new(x, 27, width, 1),
                &format!("{}: active indicator", theme.name),
                &mut failures,
                &app.colors.ui,
            );
            let interior = Rect::new(area.x + 1, area.y + 1, area.width - 2, area.height - 2);
            check_region(
                terminal.backend().buffer(),
                interior,
                &format!("{}: activity details", theme.name),
                &mut failures,
                &app.colors.ui,
            );
            snapshot(
                terminal.backend().buffer(),
                &format!("{} - performance details", theme.name),
            );
            let close = close_area(area);
            crate::mouse::handle_mouse(&mut app, mouse(MouseEventKind::Moved, close.x, close.y));
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            check_region(
                terminal.backend().buffer(),
                close,
                &format!("{}: activity close hovered", theme.name),
                &mut failures,
                &app.colors.ui,
            );
            app.background.activity.active[0].finish_for_test(Duration::from_secs(7));
            app.background.refresh_activity(Instant::now());
            terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
            snapshot(
                terminal.backend().buffer(),
                &format!("{} - performance completed", theme.name),
            );
            app.background.activity.recent.clear();
            app.hovered_status_item = None;
            app.mouse_col = u16::MAX;
            app.mouse_row = u16::MAX;
        }
        assert_readable(failures);
    }
}
