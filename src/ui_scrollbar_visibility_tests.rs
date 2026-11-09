use super::*;
use crate::app_theme::{EditorTheme, ThemeColors};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn params<'a>(width: u16, changes: &'a ChangeRows, colors: ThemeColors) -> ScrollbarParams<'a> {
    ScrollbarParams {
        area: Rect::new(0, 0, width, 12), total_lines: 1200, visible_lines: 300,
        scroll: 400, cursor_line: 550, search_matches: &[], diagnostic_lines: &[],
        diagnostic_index: None, change_rows: changes, colors, hovered: false, active: false,
    }
}

fn draw(params: ScrollbarParams) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(params.area.width, params.area.height)).unwrap();
    terminal.draw(|frame| draw_scrollbar(frame, params)).unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn dense_changes_keep_the_thumb_and_caret_visible_in_both_scrollbar_widths() {
    for (name, theme) in [("dark", EditorTheme::Dark), ("light", EditorTheme::Light)] {
        let colors = theme.colors();
        for status in ['U', 'N', 'D', 'S'] {
            let changes = ChangeRows::from_lines(12, 1200, (0..1200).map(|line| (line, status)));
            let marker = crate::ui_colors::overview_marker_color(status, &colors).unwrap();
            for (hovered, active) in [(false, false), (true, false), (false, true)] {
                let (track, thumb) = colors.ui.scrollbar_colors(colors.ui.bg_overview, hovered, active);
                for width in [2, 1] {
                    let buffer = draw(ScrollbarParams { hovered, active, ..params(width, &changes, colors) });
                    if status == 'U' && !hovered && !active {
                        crate::ui_control_visibility_tests::snapshot(&buffer, &format!("dense scrollbar {name} width {width}"));
                    }
                    // The viewport occupies rows 4..7; the caret is on row 5.
                    for y in [3, 4, 6, 7] {
                        let surface = if (4..7).contains(&y) { thumb } else { track };
                        if width == 2 {
                            let navigation = &buffer[(0, y)];
                            assert_eq!(navigation.bg, surface);
                            assert_eq!(navigation.fg, surface, "solid changes must not cover the thumb lane");
                            assert_eq!(buffer[(1, y)].fg, marker);
                            assert_eq!(buffer[(1, y)].symbol(), "█");
                        } else {
                            let cell = &buffer[(0, y)];
                            assert_eq!(cell.symbol(), "▐", "one-column changes must leave room for the thumb");
                            assert_eq!((cell.fg, cell.bg), (marker, surface));
                        }
                    }
                    if width == 2 {
                        assert_eq!(buffer[(0, 5)].symbol(), "━");
                        assert_eq!(buffer[(0, 5)].bg, thumb);
                        assert_eq!(buffer[(1, 5)].fg, marker, "the caret must not erase the change marker");
                    } else {
                        assert_eq!(buffer[(0, 5)].fg, marker);
                        assert_ne!(buffer[(0, 5)].bg, marker, "the caret must not erase the change marker");
                        assert_ne!(buffer[(0, 5)].bg, thumb, "the caret must be visible within the thumb");
                    }
                }
            }
        }
    }
}

#[test]
fn overlapping_search_and_diagnostics_do_not_hide_navigation() {
    let colors = EditorTheme::Dark.colors();
    let changes = ChangeRows::from_lines(12, 1200, (0..1200).map(|line| (line, 'U')));
    for width in [1, 2] {
        let buffer = draw(ScrollbarParams {
            search_matches: &[(400, 0, 1)], diagnostic_lines: &[(550, 1)],
            ..params(width, &changes, colors)
        });
        let (_, thumb) = colors.ui.scrollbar_colors(colors.ui.bg_overview, false, false);
        let x = width - 1;
        assert_eq!(buffer[(x, 4)].fg, colors.ui.fg_overview_find);
        assert_eq!(buffer[(x, 5)].fg, colors.ui.fg_overview_error);
        if width == 2 {
            assert_eq!(buffer[(0, 4)].bg, thumb);
            assert_eq!(buffer[(0, 4)].fg, thumb);
            assert_eq!(buffer[(0, 5)].symbol(), "━");
        } else {
            assert_eq!(buffer[(0, 4)].symbol(), "▐");
            assert_eq!(buffer[(0, 4)].bg, thumb);
            assert_ne!(buffer[(0, 5)].fg, buffer[(0, 5)].bg);
        }
    }
}

#[test]
fn narrow_scrollbars_keep_half_row_change_positions_beside_the_thumb() {
    let colors = EditorTheme::Dark.colors();
    let changes = ChangeRows::from_lines(12, 1200, [(400, 'U'), (650, 'D')]);
    let (_, thumb) = colors.ui.scrollbar_colors(colors.ui.bg_overview, false, false);
    for width in [1, 2] {
        let buffer = draw(ScrollbarParams { cursor_line: 0, ..params(width, &changes, colors) });
        let x = width - 1;
        assert_eq!(buffer[(x, 4)].symbol(), if width == 1 { "▝" } else { "▀" });
        assert_eq!(buffer[(x, 6)].symbol(), if width == 1 { "▗" } else { "▄" });
        assert_eq!(buffer[(x, 4)].fg, colors.ui.fg_overview_modified);
        assert_eq!(buffer[(x, 6)].fg, colors.ui.fg_overview_deleted);
        if width == 1 {
            assert_eq!(buffer[(0, 4)].bg, thumb);
            assert_eq!(buffer[(0, 6)].bg, thumb);
        } else {
            assert_eq!(buffer[(0, 4)].fg, thumb);
            assert_eq!(buffer[(0, 6)].fg, thumb);
        }
    }
}

#[test]
fn dense_diff_views_keep_the_navigation_lane_and_change_markers_together() {
    use crate::{app::{App, DiffMode, Tab}, editor::Editor};

    let base = "original line\n".repeat(400);
    let target = "modified line\n".repeat(400);
    for mode in [DiffMode::Inline, DiffMode::SideBySide] {
        for minimap in [false, true] {
            let mut app = App::test_new();
            let mut editor = Editor::test_with_text(&target);
            editor.word_wrap = false;
            editor.cursor_line = 200;
            editor.scroll_offset = 190;
            app.tabs = vec![Tab::new_file(editor)];
            app.active_tab = 0;
            app.show_minimap = minimap;
            app.sticky_scroll = false;
            app.diff_mode = mode;
            app.compare_base_content = Some(base.clone().into());
            app.set_compare_diff_lines(crate::app::compute_file_diff(&base, &target));
            // Each side-by-side pane must be wide enough to show its minimap.
            let mut terminal = Terminal::new(TestBackend::new(240, 24)).unwrap();
            terminal.draw(|frame| {
                let area = Rect::new(0, 0, 240, 24);
                if mode == DiffMode::Inline { crate::ui_diff_display::draw_inline_diff(frame, &mut app, area); }
                else { crate::ui_diff_display::draw_side_by_side_diff(frame, &mut app, area); }
            }).unwrap();
            let area = app.scrollbar_area.unwrap();
            assert_eq!(area.width, if minimap { 1 } else { 2 });
            let buffer = terminal.backend().buffer();
            assert!((area.y..area.bottom()).any(|y| {
                let marker = &buffer[(area.right() - 1, y)];
                ['N', 'U', 'D'].iter().any(|&status| crate::ui_colors::overview_marker_color(status, &app.colors) == Some(marker.fg))
            }));
            if !minimap {
                assert!((area.y..area.bottom()).any(|y| buffer[(area.x, y)].symbol() == "━"),
                    "a diff must not repaint the navigation lane over the caret tick");
            }
        }
    }
}
