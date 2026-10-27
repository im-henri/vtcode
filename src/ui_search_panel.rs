use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, List, ListItem, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{ActivePane, App, GlobalSearchResult, SidebarView};
use crate::text_input::TextInputId;
use crate::ui_dialogs::file_icon_colored;
use crate::ui_widgets::{InputField, VerticalScrollbar};
use crate::ui_search_controls::{SearchControl, SearchScope, draw_button};

const SEARCH_TOGGLE_WIDTH: u16 = 3;
const SEARCH_TOGGLE_COUNT: u16 = 3;
const SEARCH_DIVIDER_GAP: u16 = 1;
const SEARCH_RESULTS_OFFSET: u16 = 8;
const SEARCH_FORM_INSET: u16 = 3;

pub(crate) fn search_form_left(panel_area: Rect) -> u16 {
    panel_area.x.saturating_add(SEARCH_FORM_INSET)
}

pub(crate) fn search_results_start_y(app: &App, panel_area: Rect) -> u16 {
    panel_area.y.saturating_add(SEARCH_RESULTS_OFFSET - u16::from(!app.global_search_replace_visible))
}

/// Click targets for Match Case, Whole Word, and Regex. The final cell before
/// the sidebar divider is deliberately left empty so option buttons never
/// compete with the pane-resize handle.
pub(crate) fn search_toggle_areas(panel_area: Rect) -> [Rect; 3] {
    let divider_x = panel_area
        .x
        .saturating_add(panel_area.width.saturating_sub(1));
    let content_right = divider_x.saturating_sub(SEARCH_DIVIDER_GAP);
    let cluster_width = SEARCH_TOGGLE_WIDTH * SEARCH_TOGGLE_COUNT;
    let start = content_right
        .saturating_sub(cluster_width)
        .max(panel_area.x.saturating_add(1));
    [
        Rect::new(
            start,
            panel_area.y.saturating_add(1),
            SEARCH_TOGGLE_WIDTH,
            1,
        ),
        Rect::new(
            start.saturating_add(SEARCH_TOGGLE_WIDTH),
            panel_area.y.saturating_add(1),
            SEARCH_TOGGLE_WIDTH,
            1,
        ),
        Rect::new(
            start.saturating_add(SEARCH_TOGGLE_WIDTH * 2),
            panel_area.y.saturating_add(1),
            SEARCH_TOGGLE_WIDTH,
            1,
        ),
    ]
}

/// Refresh/stop, clear, and collapse-all actions in the Search header. Keeping the geometry
/// shared with mouse handling prevents the compact icon row from developing
/// invisible or overlapping hit targets.
pub(crate) fn search_header_action_areas(panel_area: Rect) -> [Rect; 3] {
    let right = panel_area
        .x
        .saturating_add(panel_area.width.saturating_sub(2));
    // Single-cell icons need odd-width buttons for equal padding on both
    // sides. Two-cell backgrounds appear shifted relative to their labels.
    let width = if panel_area.width >= 18 { 3 } else { 1 };
    let start = right.saturating_sub(width * 3);
    [
        Rect::new(start, panel_area.y, width, 1),
        Rect::new(start.saturating_add(width), panel_area.y, width, 1),
        Rect::new(start.saturating_add(width * 2), panel_area.y, width, 1),
    ]
}

pub(crate) fn draw_search_panel(f: &mut Frame, app: &mut App, area: Rect) {
    app.search_control_regions.retain(|button| button.scope != SearchScope::Workspace);
    let bg = app.colors.bg_sidebar;
    let fg = app.colors.ui.sidebar_text_color(bg);
    let dim = app.colors.ui.description_color(bg);
    f.render_widget(Block::default().style(Style::default().bg(bg)), area);
    if area.height < 5 || area.width < 12 {
        return;
    }

    let row = |offset: u16| Rect::new(area.x, area.y.saturating_add(offset), area.width, 1);
    let results_y = search_results_start_y(app, area).min(area.bottom());
    let results_area = Rect::new(
        area.x,
        results_y,
        area.width,
        area.y.saturating_add(area.height).saturating_sub(results_y),
    );

    f.render_widget(
        Paragraph::new(Span::styled(
            " Search",
            Style::default().fg(fg).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(bg)),
        row(0),
    );
    let header_actions = search_header_action_areas(area);
    let search_action = if app.bg_global_search_running {
        (SearchControl::Stop, "■", false)
    } else {
        (SearchControl::Refresh, "↻", false)
    };
    let actions = [
        search_action,
        (SearchControl::Clear, "×", app.global_search_query.is_empty() && app.global_search_results.is_empty()),
        (SearchControl::CollapseResults, app.global_search_all_files_toggle_label(), app.global_search_results.is_empty()),
    ];
    for (area, (control, label, disabled)) in header_actions.into_iter().zip(actions) {
        draw_button(f, app, SearchScope::Workspace, control, area, label, false, disabled);
    }

    let inset = |r: Rect| {
        Rect::new(
            search_form_left(r),
            r.y,
            r.width.saturating_sub(SEARCH_FORM_INSET.saturating_add(2)),
            1,
        )
    };
    let replace_toggle_label = if app.global_search_replace_visible { "▾" } else { "▸" };
    draw_button(f, app, SearchScope::Workspace, SearchControl::ToggleReplace,
        Rect::new(area.x.saturating_add(1), row(1).y, 2, 1), replace_toggle_label, false, false);
    let search_area = inset(row(1));
    let toggle_areas = search_toggle_areas(area);
    let toggle_start = toggle_areas[0].x.max(search_area.x);
    let search_field_area = Rect::new(
        search_area.x,
        search_area.y,
        toggle_start.saturating_sub(search_area.x),
        1,
    );
    draw_field(f, app, search_field_area, 0, "Search");
    let toggles = [
        (SearchControl::MatchCase, "Aa", app.search_case_sensitive),
        (SearchControl::WholeWord, "ab", app.search_whole_word),
        (SearchControl::Regex, ".*", app.search_regex),
    ];
    for (area, (control, label, active)) in toggle_areas.into_iter().zip(toggles) {
        draw_button(f, app, SearchScope::Workspace, control, area, label, active, false);
    }

    if app.global_search_replace_visible {
        let replace_area = inset(row(2));
        let button_width = replace_area.width.min(4);
        let preserve_width = replace_area.width.saturating_sub(button_width).min(3);
        let field_area = Rect::new(
            replace_area.x,
            replace_area.y,
            replace_area.width.saturating_sub(button_width + preserve_width),
            1,
        );
        let button_area = Rect::new(
            field_area.right() + preserve_width,
            replace_area.y,
            button_width,
            1,
        );
        draw_field(f, app, field_area, 1, "Replace");
        let disabled = app.global_search_results.is_empty()
            || app.bg_global_search_running || app.bg_global_replace_running;
        let preserve_case = app.search_preserve_case;
        draw_button(f, app, SearchScope::Workspace, SearchControl::PreserveCase,
            Rect::new(field_area.right(), replace_area.y, preserve_width, 1), "AB", preserve_case, false);
        draw_button(f, app, SearchScope::Workspace, SearchControl::ReplaceAll,
            button_area, "⇆", false, disabled);
    }

    let hidden_rows = u16::from(!app.global_search_replace_visible);
    f.render_widget(
        Paragraph::new("files to include").style(Style::default().fg(fg).bg(bg)),
        inset(row(3 - hidden_rows)),
    );
    draw_field(f, app, inset(row(4 - hidden_rows)), 2, "e.g. *.rs, src/**");
    f.render_widget(
        Paragraph::new("files to exclude").style(Style::default().fg(fg).bg(bg)),
        inset(row(5 - hidden_rows)),
    );
    draw_field(f, app, inset(row(6 - hidden_rows)), 3, "e.g. target, **/*.lock");

    let count = app.global_search_results.len();
    let files = app.global_search_results.file_count();
    let summary = if let Some(error) = &app.global_search_error {
        format!(" {error}")
    } else if app.global_search_query.is_empty() {
        String::new()
    } else if app.bg_global_search_running && count == 0 {
        " Searching…".to_string()
    } else if app.global_search_stopped && count == 0 {
        " Search stopped".to_string()
    } else if count == 0 {
        " No results found".to_string()
    } else {
        format!(
            " {count} result{} in {files} file{}{}",
            if count == 1 { "" } else { "s" },
            if files == 1 { "" } else { "s" },
            if app.global_search_stopped {
                " (stopped)"
            } else if app.global_search_truncated {
                " (truncated)"
            } else {
                ""
            }
        )
    };
    f.render_widget(
        Paragraph::new(Span::styled(summary, Style::default().fg(
            if app.global_search_error.is_some() { app.colors.fg_error } else { dim })))
            .style(Style::default().bg(bg)),
        row(7 - hidden_rows),
    );
    draw_results(f, app, results_area);
}

fn draw_field(f: &mut Frame, app: &mut App, area: Rect, field: u8, placeholder: &str) {
    let focused = app.global_search_active && app.search_focus_field == field;
    let input_bg = app.colors.ui.input_style(app.colors.bg_sidebar).bg.unwrap_or(app.colors.bg_sidebar);
    let edge = (if focused { app.colors.ui.focus_border } else { app.colors.ui.input_colors.border })
        .border(input_bg).unwrap_or(input_bg);
    let prefix = if focused { "▏" } else { " " };
    let index = field.min(3) as usize;
    let text = match index {
        0 => &app.global_search_query,
        1 => &app.global_search_replace_query,
        2 => &app.search_include_pattern,
        _ => &app.search_exclude_pattern,
    };
    let cursor = app.global_search_field_cursors[index];
    let anchor = app.global_search_field_selection_anchors[index];
    let cursor_glyph = if focused && app.cursor_visible {
        "│"
    } else {
        ""
    };
    let hit_map = InputField::new(text)
        .theme_colors(&app.colors)
        .prefix_decoration(prefix, edge)
        .placeholder_text(placeholder)
        .cursor_marker(cursor_glyph)
        .cursor_pos(cursor)
        .selection_range(
            if focused {
                App::inline_selection_range(cursor, anchor)
            } else {
                None
            },
        )
        .render(f, area);
    app.register_text_input(TextInputId::GlobalSearch(field), hit_map);
}

fn draw_results(f: &mut Frame, app: &mut App, area: Rect) {
    if area.height == 0 || app.global_search_results.is_empty() {
        return;
    }
    let bg = app.colors.bg_sidebar;
    let active = app.active_pane == ActivePane::Explorer && app.sidebar_view == SidebarView::Search;
    let total_rows = app.global_search_results.row_count();
    let visible = area.height as usize;
    let max_scroll = total_rows.saturating_sub(visible);
    app.global_search_scroll = app.global_search_scroll.min(max_scroll);
    let scroll = app.global_search_scroll;
    let viewport = scroll..scroll.saturating_add(visible);
    let show_scrollbar = total_rows > visible && area.width > 2;
    let seam_width = u16::from(!app.sidebar_right);
    let leading_seam_width = u16::from(app.sidebar_right);
    let result_area = Rect::new(
        area.x.saturating_add(leading_seam_width),
        area.y,
        area.width
            .saturating_sub(leading_seam_width)
            .saturating_sub(seam_width)
            .saturating_sub(u16::from(show_scrollbar)),
        area.height,
    );
    let mut items = Vec::with_capacity(visible);
    for (visual_row, hit) in app.global_search_results.visible_rows(viewport) {
        let (index, header) = match hit {
            crate::search_results::SearchRow::Header(index) => (index, true),
            crate::search_results::SearchRow::Result(index) => (index, false),
        };
        let result = &app.global_search_results[index];
        if header {
            let row_area = Rect::new(result_area.x,
                result_area.y + (visual_row - scroll) as u16, result_area.width, 1);
            let hovered = row_area.contains((app.mouse_col, app.mouse_row).into());
            let row_style = app.colors.ui.list_row_style(app.colors.ui.sidebar_foreground,
                bg, false, active, hovered);
            let row_bg = row_style.bg.unwrap_or(bg);
            let dim = app.colors.ui.description_color(row_bg);
            let collapsed = app.is_global_search_file_collapsed(&result.file_path);
            let name = result
                .file_path
                .file_name()
                .and_then(|v| v.to_str())
                .unwrap_or("?");
            let dir = result
                .file_path
                .parent()
                .and_then(|p| p.strip_prefix(&app.explorer.root).ok())
                .map(|p| p.to_string_lossy())
                .unwrap_or_default();
            let count = app.global_search_results.file_matches(&result.file_path);
            let (icon, icon_color) = file_icon_colored(name);
            items.push(
                ListItem::new(Line::from(vec![
                    Span::styled(
                        if collapsed { "▶ " } else { "▼ " },
                        Style::default().fg(dim),
                    ),
                    Span::styled(icon, Style::default().fg(icon_color)),
                    Span::styled(name.to_string(), row_style),
                    Span::raw("  "),
                    Span::styled(
                        format!(" {count} "),
                        app.colors.ui.badge_style(row_bg),
                    ),
                    Span::styled(
                        if dir.is_empty() {
                            String::new()
                        } else {
                            format!("  {dir}")
                        },
                        Style::default().fg(dim),
                    ),
                ]))
                .style(row_style),
            );
            continue;
        }
        let selected = index == app.global_search_selected;
        let row_area = Rect::new(result_area.x,
            result_area.y + (visual_row - scroll) as u16, result_area.width, 1);
        let hovered = row_area.contains((app.mouse_col, app.mouse_row).into());
        let row_style = app.colors.ui.list_row_style(app.colors.ui.sidebar_foreground,
            bg, selected, active, hovered);
        let (before, matched, after) = preview_parts(result, result_area.width.saturating_sub(3) as usize);
        items.push(
            ListItem::new(Line::from(vec![
                Span::raw("   "),
                Span::styled(before, row_style),
                Span::styled(
                    matched,
                    app.colors.ui.search_match_style(row_style),
                ),
                Span::styled(after, row_style),
            ]))
            .style(row_style),
        );
    }
    f.render_widget(List::new(items).style(Style::default().bg(bg)), result_area);
    if show_scrollbar {
        let hovered = app.mouse_col == result_area.right() && (area.y..area.bottom()).contains(&app.mouse_row);
        let (track, thumb) = app.colors.ui.scrollbar_colors(bg, hovered, false);
        VerticalScrollbar::new(result_area.right(), area.y, area.height, total_rows, scroll)
            .symbols('▐', '▐')
            .colors(thumb, track, track)
            .render(f);
    }
}

fn preview_parts(result: &GlobalSearchResult, max_width: usize) -> (String, String, String) {
    let leading = result
        .line_text
        .chars()
        .take_while(|ch| ch.is_whitespace())
        .count();
    let chars: Vec<char> = result.line_text.chars().skip(leading).collect();
    if max_width == 0 {
        return (String::new(), String::new(), String::new());
    }
    let start = result.col_start.saturating_sub(leading).min(chars.len());
    let end = result
        .col_end
        .saturating_sub(leading)
        .max(start)
        .min(chars.len());
    let before_budget = max_width.saturating_sub(end - start).min(max_width / 3);
    let window_start = start.saturating_sub(before_budget);
    let prefix = window_start > 0;
    let used = end - window_start + usize::from(prefix) + usize::from(start == end);
    let window_end = (end + max_width.saturating_sub(used)).min(chars.len());
    let mut before: String = chars[window_start..start].iter().collect();
    if prefix {
        before.insert(0, '…');
    }
    let matched = if start == end { "│".into() } else { chars[start..end].iter().collect() };
    let mut after: String = chars[end..window_end].iter().collect();
    if window_end < chars.len() && UnicodeWidthStr::width(after.as_str()) + used < max_width {
        after.push('…');
    }
    (before, matched, after)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};
    use std::path::PathBuf;

    #[test]
    fn scrolled_search_rows_match_full_render_with_collapsed_groups() {
        let mut app = App::test_new();
        // Compare document rows without a pointer hovering a different row in
        // each viewport. Hover is tied to screen coordinates, not file rows.
        app.mouse_col = u16::MAX;
        app.mouse_row = u16::MAX;
        app.global_search_results = (0..30)
            .map(|index| GlobalSearchResult {
                file_path: PathBuf::from(format!("src/file-{}.rs", index / 10)),
                line_num: index + 1,
                line_text: format!("row {index} needle").into(),
                col_start: 7,
                col_end: 13,
                context_before: None,
                context_after: None,
            })
            .collect();
        app.global_search_results.set_collapsed(std::path::Path::new("src/file-1.rs"), true);
        let mut full = Terminal::new(TestBackend::new(60, 40)).unwrap();
        full.draw(|f| draw_results(f, &mut app, Rect::new(0, 0, 60, 40)))
            .unwrap();
        for requested_scroll in [0, 7, 10, 15, usize::MAX] {
            app.global_search_scroll = requested_scroll;
            let mut cropped = Terminal::new(TestBackend::new(60, 8)).unwrap();
            cropped
                .draw(|f| draw_results(f, &mut app, Rect::new(0, 0, 60, 8)))
                .unwrap();
            assert_eq!(app.global_search_scroll, requested_scroll.min(15));
            for y in 0..8 {
                for x in 0..57 {
                    assert_eq!(
                        cropped.backend().buffer().cell((x, y)),
                        full.backend()
                            .buffer()
                            .cell((x, y + app.global_search_scroll as u16)),
                        "scroll={requested_scroll} cell=({x}, {y})"
                    );
                }
            }
        }
    }

    #[test]
    fn scrolled_search_hover_uses_visible_rows_and_excludes_sidebar_seams() {
        use crate::theme_workbench::ColorLayer;
        for sidebar_right in [false, true] {
            let mut app = App::test_new();
            app.colors = crate::ui_control_visibility_tests::theme_with_popup_and_control_roles().to_theme_colors();
            app.sidebar_right = sidebar_right;
            app.sidebar_view = SidebarView::Search;
            app.active_pane = ActivePane::Explorer;
            app.global_search_results = (0..30).map(|index| GlobalSearchResult {
                file_path: PathBuf::from("src/example.rs"), line_num: index + 1,
                line_text: format!("row{index} needle").into(), col_start: 5, col_end: 11,
                context_before: None, context_after: None,
            }).collect();
            app.global_search_scroll = 7;
            app.mouse_col = u16::MAX;
            app.mouse_row = u16::MAX;
            let area = Rect::new(10, 5, 60, 8);
            let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
            terminal.draw(|frame| draw_results(frame, &mut app, area)).unwrap();
            let position = crate::ui_control_visibility_tests::find_cell(terminal.backend().buffer(), "row6");
            assert_eq!(position.1, area.y);
            (app.mouse_col, app.mouse_row) = position;
            terminal.draw(|frame| draw_results(frame, &mut app, area)).unwrap();
            let cell = &terminal.backend().buffer()[position];
            assert_eq!(cell.bg, ColorLayer::rgba(80, 56, 72, 192).over(app.colors.bg_sidebar).unwrap());
            assert_eq!(cell.fg, ColorLayer::rgba(221, 255, 204, 224).over(cell.bg).unwrap());
            app.mouse_col = if sidebar_right { area.x } else { area.right() - 1 };
            terminal.draw(|frame| draw_results(frame, &mut app, area)).unwrap();
            assert_eq!(terminal.backend().buffer()[position].bg, app.colors.bg_sidebar);
        }
    }

    #[test]
    #[ignore = "manual release benchmark"]
    fn benchmark_search_panel_render() {
        let mut app = App::test_new();
        app.global_search_query = "needle".into();
        app.global_search_results = (0..10_000)
            .map(|index| GlobalSearchResult {
                file_path: PathBuf::from(format!("src/file-{}.rs", index / 100)),
                line_num: index + 1,
                line_text: format!("{} needle suffix", "source ".repeat(100)).into(),
                col_start: 701,
                col_end: 707,
                context_before: None,
                context_after: None,
            })
            .collect();
        app.global_search_scroll = 5_000;
        let mut terminal = Terminal::new(TestBackend::new(60, 30)).unwrap();
        let mut samples = Vec::new();
        for _ in 0..7 {
            app.text_input_regions.clear();
            let started = std::time::Instant::now();
            terminal
                .draw(|frame| draw_search_panel(frame, &mut app, Rect::new(0, 0, 60, 30)))
                .unwrap();
            samples.push(started.elapsed());
            assert_eq!(app.global_search_scroll, 5_000);
            std::hint::black_box(terminal.backend().buffer());
        }
        samples.sort();
        eprintln!("search_panel_render results=10000 median={:?}", samples[3]);
    }

    #[test]
    fn search_toggles_leave_a_safe_gap_before_the_divider() {
        let panel = Rect::new(10, 5, 30, 20);
        let toggles = search_toggle_areas(panel);
        let regex = toggles[2];
        let divider_x = panel.x + panel.width - 1;

        assert!(toggles.iter().all(|area| area.width == 3));
        assert_eq!(regex.x + regex.width, divider_x - 1);
    }

    #[test]
    fn renders_compact_vscode_search_structure() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};

        let mut app = App::test_new();
        app.global_search_active = true;
        app.sidebar_view = crate::app::SidebarView::Search;
        app.global_search_query = "needle".into();
        app.global_search_field_cursors[0] = app.global_search_query.len();
        app.global_search_results = vec![GlobalSearchResult {
            file_path: PathBuf::from("src/main.rs"),
            line_num: 20,
            line_text: "    let needle = value;".into(),
            col_start: 8,
            col_end: 14,
            context_before: None,
            context_after: None,
        }].into();
        let mut terminal = Terminal::new(TestBackend::new(48, 14)).unwrap();
        terminal
            .draw(|frame| draw_search_panel(frame, &mut app, Rect::new(0, 0, 48, 14)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row = |y| {
            (0..48)
                .filter_map(|x| buffer.cell((x, y)))
                .map(|c| c.symbol())
                .collect::<String>()
        };
        assert!(row(0).contains("Search"));
        assert!(row(0).contains('↻'));
        assert!(row(0).contains('×'));
        assert!(row(0).contains('⊟'));
        assert!(row(3).contains("files to include"));
        assert!(row(4).contains("e.g. *.rs, src/**"));
        assert!(row(5).contains("files to exclude"));
        assert!(row(6).contains("e.g. target, **/*.lock"));
        assert!(row(7).contains("1 result in 1 file"));
        assert!(row(8).contains("main.rs"));
        assert!(row(9).contains("needle"));
        assert!(!row(0).contains('┌'));
        assert_eq!(buffer.cell((3, 1)).unwrap().symbol(), "▏");
        assert_eq!(buffer.cell((3, 1)).unwrap().fg, app.colors.ui.focus_border.over(app.colors.bg_input).unwrap());
        assert_eq!(buffer.cell((20, 1)).unwrap().bg, app.colors.bg_input);
        assert_eq!(buffer.cell((3, 3)).unwrap().fg, app.colors.ui.fg_sidebar);
        assert_eq!(buffer.cell((20, 4)).unwrap().bg, app.colors.bg_input);
        assert_eq!(app.text_input_regions.len(), 4);
        assert!(app.handle_background_text_input_mouse(
            MouseEventKind::Down(MouseButton::Left),
            6,
            1,
            KeyModifiers::NONE,
        ));
        assert_eq!(app.global_search_field_cursors[0], 2);
        assert!(app.handle_background_text_input_mouse(
            MouseEventKind::Down(MouseButton::Left),
            8,
            4,
            KeyModifiers::NONE,
        ));
        assert_eq!(app.search_focus_field, 2);
    }

    #[test]
    fn advanced_filters_are_always_visible() {
        let app = App::test_new();
        let panel = Rect::new(3, 7, 30, 20);
        assert_eq!(search_results_start_y(&app, panel), 15);
    }

    #[test]
    fn overflowing_results_draw_a_sleek_scrollbar_inside_the_seam() {
        let mut app = App::test_new();
        app.sidebar_view = crate::app::SidebarView::Search;
        app.global_search_query = "match".into();
        app.global_search_results = (0..20)
            .map(|index| GlobalSearchResult {
                file_path: PathBuf::from("src/main.rs"),
                line_num: index + 1,
                line_text: format!("match {index}").into(),
                col_start: 0,
                col_end: 5,
                context_before: None,
                context_after: None,
            })
            .collect();
        app.global_search_scroll = 3;
        let width = 48;
        let height = 14;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();

        terminal
            .draw(|frame| draw_search_panel(frame, &mut app, Rect::new(0, 0, width, height)))
            .unwrap();

        let scrollbar_x = width - 2;
        let buffer = terminal.backend().buffer();
        assert!((SEARCH_RESULTS_OFFSET..height).all(|y| {
            buffer
                .cell((scrollbar_x, y))
                .is_some_and(|cell| cell.symbol() == "▐")
        }));
    }
}
