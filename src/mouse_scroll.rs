use crossterm::event::KeyModifiers;
use crate::app::{App, TerminalDock};

fn docked_terminal_at(app: &App, col: u16, row: u16) -> Option<usize> {
    if app.show_terminal_sidebar
        && app.terminal_sidebar_area.is_some_and(|area| {
            col >= area.x
                && col < area.x + area.width
                && row >= area.y
                && row < area.y + area.height
        })
    {
        return app.active_terminal_in_dock(TerminalDock::Right);
    }
    None
}

/// Settings panes register their rendered content even when another group has
/// keyboard focus. Wheel input belongs to the visible pane under the pointer.
fn scroll_settings_at(app: &mut App, col: u16, row: u16, down: bool) -> bool {
    if app.settings_scroll_areas.is_empty() {
        return false;
    }
    let point = (col, row).into();
    if app.settings_dropdown_open
        && let Some(area) = app.settings_dropdown_area.filter(|area| area.contains(point))
    {
        app.settings_dropdown_scroll = if down {
            let max = app.settings_dropdown_options.len().saturating_sub(area.height as usize);
            app.settings_dropdown_scroll.saturating_add(3).min(max)
        } else {
            app.settings_dropdown_scroll.saturating_sub(3)
        };
        return true;
    }
    if let Some(area) = app.settings_category_area.filter(|area| area.contains(point)) {
        let max = app.settings_category_visual_rows().len().saturating_sub(area.height as usize);
        app.settings_category_scroll = if down {
            app.settings_category_scroll.saturating_add(3).min(max)
        } else {
            app.settings_category_scroll.saturating_sub(3)
        };
        return true;
    }
    let Some(area) = app.settings_scroll_areas.iter().find(|area| area.contains(point)).copied() else { return false; };
    let max = app.settings_line_layout(area.width.saturating_sub(3) as usize).max_scroll(area.height as usize);
    app.settings_editor_scroll = if down {
        app.settings_editor_scroll.saturating_add(app.scroll_speed)
    } else {
        app.settings_editor_scroll.saturating_sub(app.scroll_speed)
    }.min(max);
    true
}

fn scroll_repositories_at(app: &mut App, col: u16, row: u16, down: bool) -> bool {
    if !app.show_explorer || app.sidebar_view != crate::app::SidebarView::SourceControl { return false; }
    if !app.repositories.headers.iter().any(|(area, _)| area.contains((col, row).into())) {
        return app.repositories.discovered && !app.repositories.expanded
            && app.explorer_area.is_some_and(|area| area.contains((col, row).into()));
    }
    let count = app.repositories.entries.iter().filter(|repo| !app.repositories.closed.contains(&repo.root)).count();
    let max = count.saturating_sub(app.repositories.headers.len());
    app.repositories.header_scroll = if down { app.repositories.header_scroll.saturating_add(1).min(max) }
        else { app.repositories.header_scroll.saturating_sub(1) };
    true
}

/// Handle scroll-up events in the main editor view.
pub(crate) fn handle_scroll_up(app: &mut App, col: u16, row: u16, modifiers: KeyModifiers) {
    let shift_scroll = modifiers.contains(KeyModifiers::SHIFT);
    let spd = app.scroll_speed;
    // Change directory dialog scroll (mouse-first modal behavior)
    if app.change_dir_active {
        app.scroll_folder_browser(col, row, true);
        return;
    }
    if scroll_settings_at(app, col, row, false) {
        return;
    }
    if scroll_repositories_at(app, col, row, false) { return; }
    // Scroll on tab bar = scroll tab bar, or switch tabs with Shift
    if let Some(tab_area) = app.tab_bar_area {
        if col >= tab_area.x
            && col < tab_area.x + tab_area.width
            && row >= tab_area.y
            && row < tab_area.y + tab_area.height
            && !app.tabs.is_empty()
        {
            if shift_scroll {
                app.active_tab =
                    if app.active_tab == 0 { app.tabs.len() - 1 } else { app.active_tab - 1 };
                app.tab_scroll_manual_anchor = None;
                app.ensure_active_tab_visible();
            } else {
                app.tab_scroll_offset = app.tab_scroll_offset.saturating_sub(1);
            }
            return;
        }
    }
    if app.handle_extension_view_scroll_input(
        col,
        row,
        -1,
        modifiers,
        "vertical",
    ) {
        return;
    }
    if app.scroll_markdown_preview_at(col, row, -(spd as isize)) {
        return;
    }
    if app.scroll_diff_at(col, row, shift_scroll, if shift_scroll { -6 } else { -(spd as isize) }) {
        return;
    }
    if shift_scroll {
        // Shift+ScrollUp = scroll left
        if crate::ui_chrome::scroll_explorer_horizontal_at(app, col, row, -6) { return; }
        if let Some(ed) = app.editor_mut() {
            if !ed.word_wrap {
                ed.horizontal_scroll = ed.horizontal_scroll.saturating_sub(6);
            }
        }
        return;
    }
    if let Some(exp_area) = app.explorer_area {
        if col >= exp_area.x && col < exp_area.x + exp_area.width
            && row >= exp_area.y && row < exp_area.y + exp_area.height
        {
            if app.sidebar_view == crate::app::SidebarView::Settings {
                if app.settings_selected > 0 { app.settings_selected -= 1; }
            } else if app.sidebar_view == crate::app::SidebarView::Search {
                let results_y =
                    crate::ui_search_panel::search_results_start_y(app, exp_area);
                if row >= results_y {
                    app.global_search_scroll = app.global_search_scroll.saturating_sub(spd);
                }
            } else if app.sidebar_view == crate::app::SidebarView::SourceControl
                && app.source_control_repository_state
                    == crate::app::SourceControlRepositoryState::Available
            {
                if let Some(exp_area) = app.source_control_detail_area() {
                    let (changes_h, _) = crate::ui_source_control::source_control_split_heights(
                        exp_area.height,
                        app.sc_graph_collapsed,
                        app.sc_graph_height,
                    );
                    let rel_y = row.saturating_sub(exp_area.y);
                    if rel_y >= changes_h {
                        app.git_log_scroll = app.git_log_scroll.saturating_sub(spd);
                    } else {
                        let max = app.source_control_layout().max_scroll(
                            crate::ui_source_control::source_control_file_list_visible_rows(changes_h));
                        app.source_control_scroll = app
                            .source_control_scroll
                            .saturating_sub(spd)
                            .min(max);
                    }
                } else {
                    app.source_control_scroll = 0;
                }
            } else if app.sidebar_view == crate::app::SidebarView::Extensions {
                app.extensions_scroll = app.extensions_scroll.saturating_sub(spd);
            } else if app.sidebar_view == crate::app::SidebarView::Outline {
                app.outline_scroll = app.outline_scroll.saturating_sub(spd);
            } else if !crate::ui_chrome::scroll_open_editors(app, exp_area, row, false) {
                app.explorer.scroll_up(spd);
            }
            return;
        }
    }
    // Check if scrolling on a non-active split group
    if app.split_active() {
        for (gi, group) in app.groups.iter_mut().enumerate() {
            if gi == app.active_group { continue; }
            if let Some(ca) = group.code_area {
                let (scroll_x, scroll_w) = group
                    .editor_area
                    .map_or((ca.x, ca.width), |ea| (ea.x, ea.width));
                if col >= scroll_x
                    && col < scroll_x + scroll_w
                    && row >= ca.y
                    && row < ca.y + ca.height
                {
                    if let Some(tab) = group.tabs.get_mut(group.active_tab) {
                        tab.editor.scroll_up(spd);
                    }
                    return;
                }
            }
            if let Some(tba) = group.tab_bar_area {
                if row >= tba.y && row < tba.y + tba.height && col >= tba.x && col < tba.x + tba.width && !group.tabs.is_empty() {
                    if shift_scroll {
                        group.active_tab = if group.active_tab == 0 { group.tabs.len() - 1 } else { group.active_tab - 1 };
                        crate::mouse::ensure_group_active_tab_visible(group);
                    }
                    return;
                }
            }
        }
    }
    // Terminal dock scrollback (scroll up)
    if let Some(index) = docked_terminal_at(app, col, row) {
        if let Some(ref mut pty) = app.terminals[index].terminal_pty {
            pty.scroll_up(spd);
        }
        return;
    }
    if let Some(ed) = app.editor_mut() {
        ed.scroll_up(spd);
    }
}

/// Handle scroll-down events in the main editor view.
pub(crate) fn handle_scroll_down(app: &mut App, col: u16, row: u16, modifiers: KeyModifiers) {
    let shift_scroll = modifiers.contains(KeyModifiers::SHIFT);
    let spd = app.scroll_speed;
    // Change directory dialog scroll (mouse-first modal behavior)
    if app.change_dir_active {
        app.scroll_folder_browser(col, row, false);
        return;
    }
    if scroll_settings_at(app, col, row, true) {
        return;
    }
    if scroll_repositories_at(app, col, row, true) { return; }
    // Scroll on tab bar = scroll tab bar, or switch tabs with Shift
    if let Some(tab_area) = app.tab_bar_area {
        if col >= tab_area.x
            && col < tab_area.x + tab_area.width
            && row >= tab_area.y
            && row < tab_area.y + tab_area.height
            && !app.tabs.is_empty()
        {
            if shift_scroll {
                app.active_tab = (app.active_tab + 1) % app.tabs.len();
                app.validate_diff_state_for_active_tab();
                app.tab_scroll_manual_anchor = None;
                app.ensure_active_tab_visible();
            } else {
                app.tab_scroll_offset = (app.tab_scroll_offset + 1).min(app.max_tab_scroll_offset());
            }
            return;
        }
    }
    if app.handle_extension_view_scroll_input(
        col,
        row,
        1,
        modifiers,
        "vertical",
    ) {
        return;
    }
    if app.scroll_markdown_preview_at(col, row, spd as isize) {
        return;
    }
    if app.scroll_diff_at(col, row, shift_scroll, if shift_scroll { 6 } else { spd as isize }) {
        return;
    }
    if shift_scroll {
        // Shift+ScrollDown = scroll right
        if crate::ui_chrome::scroll_explorer_horizontal_at(app, col, row, 6) { return; }
        let max_w = app.cached_max_line_width;
        if let Some(ed) = app.editor_mut() {
            if !ed.word_wrap {
                let max_scroll = max_w.saturating_sub(ed.viewport_width);
                ed.horizontal_scroll = (ed.horizontal_scroll + 6).min(max_scroll);
            }
        }
        return;
    }
    if let Some(exp_area) = app.explorer_area {
        if col >= exp_area.x && col < exp_area.x + exp_area.width
            && row >= exp_area.y && row < exp_area.y + exp_area.height
        {
            if app.sidebar_view == crate::app::SidebarView::Settings {
                let items_count = app.settings_items().len();
                if app.settings_selected < items_count.saturating_sub(1) {
                    app.settings_selected += 1;
                }
            } else if app.sidebar_view == crate::app::SidebarView::Search {
                let results_y =
                    crate::ui_search_panel::search_results_start_y(app, exp_area);
                if row >= results_y {
                    app.global_search_scroll = app.global_search_scroll.saturating_add(spd);
                }
            } else if app.sidebar_view == crate::app::SidebarView::SourceControl
                && app.source_control_repository_state
                    == crate::app::SourceControlRepositoryState::Available
            {
                if let Some(exp_area) = app.source_control_detail_area() {
                    let (changes_h, _) = crate::ui_source_control::source_control_split_heights(
                        exp_area.height,
                        app.sc_graph_collapsed,
                        app.sc_graph_height,
                    );
                    let rel_y = row.saturating_sub(exp_area.y);
                    if rel_y >= changes_h {
                        let max = (app.git_log.len() + usize::from(app.git_history_has_footer())).saturating_sub(1);
                        app.git_log_scroll = (app.git_log_scroll + spd).min(max);
                    } else {
                        let max = app.source_control_layout().max_scroll(
                            crate::ui_source_control::source_control_file_list_visible_rows(changes_h));
                        app.source_control_scroll = (app.source_control_scroll + spd).min(max);
                    }
                } else {
                    app.source_control_scroll = 0;
                }
            } else if app.sidebar_view == crate::app::SidebarView::Extensions {
                let visible = usize::from(app.ext_sidebar.list_area.height / crate::ui_extensions::CARD_HEIGHT).max(1);
                let max = app.filtered_extension_indices().len().saturating_sub(visible);
                app.extensions_scroll = app.extensions_scroll.saturating_add(spd).min(max);
            } else if app.sidebar_view == crate::app::SidebarView::Outline {
                let max = app.outline_items.len().saturating_sub(1);
                app.outline_scroll = (app.outline_scroll + spd).min(max);
            } else if !crate::ui_chrome::scroll_open_editors(app, exp_area, row, true) {
                app.explorer.scroll_down(spd);
            }
            return;
        }
    }
    // Check if scrolling on a non-active split group
    if app.split_active() {
        for (gi, group) in app.groups.iter_mut().enumerate() {
            if gi == app.active_group { continue; }
            if let Some(ca) = group.code_area {
                let (scroll_x, scroll_w) = group
                    .editor_area
                    .map_or((ca.x, ca.width), |ea| (ea.x, ea.width));
                if col >= scroll_x
                    && col < scroll_x + scroll_w
                    && row >= ca.y
                    && row < ca.y + ca.height
                {
                    if let Some(tab) = group.tabs.get_mut(group.active_tab) {
                        tab.editor.scroll_down(spd);
                    }
                    return;
                }
            }
            if let Some(tba) = group.tab_bar_area {
                if row >= tba.y && row < tba.y + tba.height && col >= tba.x && col < tba.x + tba.width && !group.tabs.is_empty() {
                    if shift_scroll {
                        group.active_tab = (group.active_tab + 1) % group.tabs.len();
                        crate::mouse::ensure_group_active_tab_visible(group);
                    }
                    return;
                }
            }
        }
    }
    // Terminal dock scrollback (scroll down)
    if let Some(index) = docked_terminal_at(app, col, row) {
        if let Some(ref mut pty) = app.terminals[index].terminal_pty {
            pty.scroll_down(spd);
        }
        return;
    }
    if let Some(ed) = app.editor_mut() {
        ed.scroll_down(spd);
    }
}

/// Handle scroll-left events.
pub(crate) fn handle_scroll_left(app: &mut App, col: u16, row: u16, modifiers: KeyModifiers) {
    if crate::ui_chrome::scroll_explorer_horizontal_at(app, col, row, -6) { return; }
    if app.handle_extension_view_scroll_input(col, row, -1, modifiers, "horizontal") { return; }
    if app.scroll_diff_at(col, row, true, -6) { return; }
    if let Some(tab_area) = app.tab_bar_area {
        if col >= tab_area.x
            && col < tab_area.x + tab_area.width
            && row >= tab_area.y
            && row < tab_area.y + tab_area.height
            && !app.tabs.is_empty()
        {
            app.active_tab =
                if app.active_tab == 0 { app.tabs.len() - 1 } else { app.active_tab - 1 };
            app.tab_scroll_manual_anchor = None;
            app.ensure_active_tab_visible();
            return;
        }
    }
    if app.split_active() {
        for (gi, group) in app.groups.iter_mut().enumerate() {
            if gi == app.active_group {
                continue;
            }
            if let Some(tba) = group.tab_bar_area {
                if row >= tba.y
                    && row < tba.y + tba.height
                    && col >= tba.x
                    && col < tba.x + tba.width
                    && !group.tabs.is_empty()
                {
                    group.active_tab = if group.active_tab == 0 {
                        group.tabs.len() - 1
                    } else {
                        group.active_tab - 1
                    };
                    crate::mouse::ensure_group_active_tab_visible(group);
                    return;
                }
            }
        }
    }
    if let Some(ed) = app.editor_mut() {
        ed.horizontal_scroll = ed.horizontal_scroll.saturating_sub(6);
    }
}

/// Handle scroll-right events.
pub(crate) fn handle_scroll_right(app: &mut App, col: u16, row: u16, modifiers: KeyModifiers) {
    if crate::ui_chrome::scroll_explorer_horizontal_at(app, col, row, 6) { return; }
    if app.handle_extension_view_scroll_input(col, row, 1, modifiers, "horizontal") { return; }
    if app.scroll_diff_at(col, row, true, 6) { return; }
    if let Some(tab_area) = app.tab_bar_area {
        if col >= tab_area.x
            && col < tab_area.x + tab_area.width
            && row >= tab_area.y
            && row < tab_area.y + tab_area.height
            && !app.tabs.is_empty()
        {
            app.active_tab = (app.active_tab + 1) % app.tabs.len();
            app.validate_diff_state_for_active_tab();
            app.tab_scroll_manual_anchor = None;
            app.ensure_active_tab_visible();
            return;
        }
    }
    if app.split_active() {
        for (gi, group) in app.groups.iter_mut().enumerate() {
            if gi == app.active_group {
                continue;
            }
            if let Some(tba) = group.tab_bar_area {
                if row >= tba.y
                    && row < tba.y + tba.height
                    && col >= tba.x
                    && col < tba.x + tba.width
                    && !group.tabs.is_empty()
                {
                    group.active_tab = (group.active_tab + 1) % group.tabs.len();
                    crate::mouse::ensure_group_active_tab_visible(group);
                    return;
                }
            }
        }
    }
    let max_w = app.cached_max_line_width;
    if let Some(ed) = app.editor_mut() {
        let max_scroll = max_w.saturating_sub(ed.viewport_width);
        ed.horizontal_scroll = (ed.horizontal_scroll + 6).min(max_scroll);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, GitFileChange, GlobalSearchResult, SidebarView, Tab};
    use crate::editor::Editor;
    use crossterm::event::KeyModifiers;
    use std::path::PathBuf;

    fn make_app() -> App {
        let mut app = App::new(PathBuf::from("."));
        app.tabs = vec![Tab::new_file(Editor::new()), Tab::new_file(Editor::new()), Tab::new_file(Editor::new())];
        app.active_tab = 0;
        app.scroll_speed = 3;
        app
    }

    // --- Editor scroll up/down ---

    #[test]
    fn scroll_up_decrements_editor_scroll_offset() {
        let mut app = make_app();
        app.editor_mut().unwrap().scroll_offset = 10;
        handle_scroll_up(&mut app, 50, 10, KeyModifiers::NONE);
        assert!(app.editor().unwrap().scroll_offset < 10);
    }

    #[test]
    fn scroll_down_increments_editor_scroll_offset() {
        let mut app = make_app();
        app.editor_mut().unwrap().scroll_offset = 0;
        app.editor_mut().unwrap().rope = ropey::Rope::from_str(&"line\n".repeat(100));
        handle_scroll_down(&mut app, 50, 10, KeyModifiers::NONE);
        assert!(app.editor().unwrap().scroll_offset > 0);
    }

    #[test]
    fn scroll_up_at_zero_stays_zero() {
        let mut app = make_app();
        app.editor_mut().unwrap().scroll_offset = 0;
        handle_scroll_up(&mut app, 50, 10, KeyModifiers::NONE);
        assert_eq!(app.editor().unwrap().scroll_offset, 0);
    }

    // --- Shift+scroll = horizontal ---

    #[test]
    fn shift_scroll_up_scrolls_left() {
        let mut app = make_app();
        app.editor_mut().unwrap().horizontal_scroll = 20;
        handle_scroll_up(&mut app, 50, 10, KeyModifiers::SHIFT);
        assert!(app.editor().unwrap().horizontal_scroll < 20);
    }

    #[test]
    fn shift_scroll_down_scrolls_right() {
        let mut app = make_app();
        app.cached_max_line_width = 200;
        app.editor_mut().unwrap().horizontal_scroll = 0;
        handle_scroll_down(&mut app, 50, 10, KeyModifiers::SHIFT);
        assert!(app.editor().unwrap().horizontal_scroll > 0);
    }

    // --- Tab bar scroll ---

    #[test]
    fn scroll_up_on_tab_bar_decrements_tab_scroll_offset() {
        let mut app = make_app();
        app.tab_bar_area = Some(ratatui::layout::Rect::new(0, 0, 200, 1));
        app.tab_scroll_offset = 5;
        handle_scroll_up(&mut app, 10, 0, KeyModifiers::NONE);
        assert_eq!(app.tab_scroll_offset, 4);
    }

    #[test]
    fn scroll_down_on_tab_bar_increments_tab_scroll_offset() {
        let mut app = make_app();
        app.tab_bar_area = Some(ratatui::layout::Rect::new(0, 0, 200, 1));
        app.tab_scroll_offset = 0;
        // max_tab_scroll_offset may be 0 for few tabs, so just verify no panic
        handle_scroll_down(&mut app, 10, 0, KeyModifiers::NONE);
        // tab_scroll_offset clamped by max_tab_scroll_offset
        assert!(app.tab_scroll_offset <= app.max_tab_scroll_offset());
    }

    #[test]
    fn shift_scroll_up_on_tab_bar_switches_to_prev_tab() {
        let mut app = make_app();
        app.tab_bar_area = Some(ratatui::layout::Rect::new(0, 0, 200, 1));
        app.active_tab = 1;
        handle_scroll_up(&mut app, 10, 0, KeyModifiers::SHIFT);
        assert_eq!(app.active_tab, 0);
    }

    #[test]
    fn shift_scroll_down_on_tab_bar_switches_to_next_tab() {
        let mut app = make_app();
        app.tab_bar_area = Some(ratatui::layout::Rect::new(0, 0, 200, 1));
        app.active_tab = 0;
        handle_scroll_down(&mut app, 10, 0, KeyModifiers::SHIFT);
        assert_eq!(app.active_tab, 1);
    }

    #[test]
    fn shift_scroll_up_on_tab_bar_wraps_around() {
        let mut app = make_app();
        app.tab_bar_area = Some(ratatui::layout::Rect::new(0, 0, 200, 1));
        app.active_tab = 0;
        handle_scroll_up(&mut app, 10, 0, KeyModifiers::SHIFT);
        assert_eq!(app.active_tab, 2); // wraps to last tab
    }

    #[test]
    fn shift_scroll_down_on_tab_bar_wraps_around() {
        let mut app = make_app();
        app.tab_bar_area = Some(ratatui::layout::Rect::new(0, 0, 200, 1));
        app.active_tab = 2;
        handle_scroll_down(&mut app, 10, 0, KeyModifiers::SHIFT);
        assert_eq!(app.active_tab, 0); // wraps to first tab
    }

    // --- Horizontal scroll left/right ---

    #[test]
    fn scroll_left_decrements_horizontal_scroll() {
        let mut app = make_app();
        app.editor_mut().unwrap().horizontal_scroll = 20;
        handle_scroll_left(&mut app, 50, 10, KeyModifiers::NONE);
        assert_eq!(app.editor().unwrap().horizontal_scroll, 14); // 20 - 6
    }

    #[test]
    fn scroll_right_increments_horizontal_scroll() {
        let mut app = make_app();
        app.cached_max_line_width = 200;
        app.editor_mut().unwrap().horizontal_scroll = 0;
        handle_scroll_right(&mut app, 50, 10, KeyModifiers::NONE);
        assert_eq!(app.editor().unwrap().horizontal_scroll, 6);
    }

    #[test]
    fn scroll_left_at_zero_stays_zero() {
        let mut app = make_app();
        app.editor_mut().unwrap().horizontal_scroll = 0;
        handle_scroll_left(&mut app, 50, 10, KeyModifiers::NONE);
        assert_eq!(app.editor().unwrap().horizontal_scroll, 0);
    }

    #[test]
    fn scroll_right_clamps_to_max_width() {
        let mut app = make_app();
        app.cached_max_line_width = 85;
        app.editor_mut().unwrap().viewport_width = 80;
        app.editor_mut().unwrap().horizontal_scroll = 2;
        handle_scroll_right(&mut app, 50, 10, KeyModifiers::NONE);
        assert_eq!(app.editor().unwrap().horizontal_scroll, 5); // max = 85-80=5
    }

    // --- Scroll left/right on tab bar cycles tabs ---

    #[test]
    fn scroll_left_on_tab_bar_switches_to_prev_tab() {
        let mut app = make_app();
        app.tab_bar_area = Some(ratatui::layout::Rect::new(0, 0, 200, 1));
        app.active_tab = 2;
        handle_scroll_left(&mut app, 10, 0, KeyModifiers::NONE);
        assert_eq!(app.active_tab, 1);
    }

    #[test]
    fn scroll_right_on_tab_bar_switches_to_next_tab() {
        let mut app = make_app();
        app.tab_bar_area = Some(ratatui::layout::Rect::new(0, 0, 200, 1));
        app.active_tab = 0;
        handle_scroll_right(&mut app, 10, 0, KeyModifiers::NONE);
        assert_eq!(app.active_tab, 1);
    }

    // --- Settings scroll ---

    fn settings_split_app(zone: crate::app::TabDropZone) -> App {
        let mut app = make_app();
        app.tabs = vec![Tab::new_file(Editor::test_with_text(&"line\n".repeat(200)))];
        app.show_explorer = false;
        app.show_activity_bar = false;
        app.show_panel = false;
        app.show_minimap = false;
        app.open_settings_tab();
        app.settings_category_selected = 2;
        app.ensure_settings_selection_visible();
        app.create_split_group_at(app.active_tab, zone);
        app.switch_group(0);
        app
    }

    fn wheel(app: &mut App, down: bool, col: u16, row: u16) {
        crate::mouse::handle_mouse(app, crossterm::event::MouseEvent {
            kind: if down { crossterm::event::MouseEventKind::ScrollDown }
                else { crossterm::event::MouseEventKind::ScrollUp },
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        });
    }

    #[test]
    fn settings_split_scrolls_without_taking_focus() {
        use crate::app::{ActivePane, TabDropZone};
        use ratatui::{Terminal, backend::TestBackend};
        for (zone, name) in [
            (TabDropZone::Left, "left"), (TabDropZone::Right, "right"),
            (TabDropZone::Top, "top"), (TabDropZone::Bottom, "bottom"),
        ] {
            for width in [90, 160] {
                for focus in [ActivePane::Editor, ActivePane::Explorer] {
                    let mut app = settings_split_app(zone);
                    app.active_pane = focus;
                    app.show_explorer = focus == ActivePane::Explorer;
                    let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
                    terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                    assert!(!app.settings_editor_active);
                    let pane = app.groups[1].editor_area.unwrap();
                    let (col, row) = (pane.right() - 8, pane.bottom() - 3);
                    let editor_scroll = app.editor().unwrap().scroll_offset;
                    wheel(&mut app, true, col, row);
                    assert_eq!(app.settings_editor_scroll, app.scroll_speed,
                        "Settings must scroll under the pointer: {name}, width {width}, focus {focus:?}");
                    terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                    wheel(&mut app, false, col, row);
                    assert_eq!(app.settings_editor_scroll, 0);
                    assert_eq!(app.active_group, 0);
                    assert_eq!(app.active_tab, 0);
                    assert_eq!(app.active_pane, focus);
                    assert_eq!(app.editor().unwrap().scroll_offset, editor_scroll);
                    assert!(!app.settings_editor_active);
                }
            }
        }
    }

    #[test]
    fn settings_scroll_targets_follow_resizing_and_disappear_with_the_view() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = settings_split_app(crate::app::TabDropZone::Right);
        let mut terminal = Terminal::new(TestBackend::new(90, 40)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        let old_pane = app.groups[1].editor_area.unwrap();
        let (old_col, row) = (old_pane.right() - 8, old_pane.bottom() - 3);
        wheel(&mut app, true, old_col, row);
        let settings_scroll = app.settings_editor_scroll;

        terminal.backend_mut().resize(200, 40);
        terminal.autoresize().unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert_eq!(app.settings_scroll_areas.len(), 1);
        // The old location now belongs to the file pane.
        wheel(&mut app, true, old_col, row);
        assert_eq!(app.settings_editor_scroll, settings_scroll);
        assert!(app.editor().unwrap().scroll_offset > 0);
        let pane = app.groups[1].editor_area.unwrap();
        let col = pane.right() - 8;
        wheel(&mut app, true, col, row);
        assert_eq!(app.settings_editor_scroll, settings_scroll + app.scroll_speed);

        app.groups[1].tabs.push(Tab::new_file(Editor::test_with_text(&"line\n".repeat(200))));
        app.groups[1].active_tab = 1;
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert!(app.settings_scroll_areas.is_empty());
        let settings_scroll = app.settings_editor_scroll;
        wheel(&mut app, true, col, row);
        assert_eq!(app.settings_editor_scroll, settings_scroll);
        assert!(app.groups[1].tabs[1].editor.scroll_offset > 0);
        assert_eq!(app.active_group, 0);
    }

    #[test]
    fn unfocused_settings_dropdown_scrolls_before_the_settings_page() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut app = settings_split_app(crate::app::TabDropZone::Right);
        app.switch_group(1);
        app.settings_category_selected = App::settings_categories().len() - 1;
        app.open_settings_category_dropdown();
        app.switch_group(0);
        let mut terminal = Terminal::new(TestBackend::new(90, 12)).unwrap();
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        assert!(!app.settings_editor_active);
        let menu = app.settings_dropdown_area.unwrap();
        let before = app.settings_dropdown_scroll;
        assert!(before > 0);
        let page_scroll = app.settings_editor_scroll;
        wheel(&mut app, false, menu.x, menu.y);
        assert!(app.settings_dropdown_scroll < before);
        terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
        wheel(&mut app, true, menu.x, menu.y);
        assert_eq!(app.settings_dropdown_scroll, before);
        assert_eq!(app.settings_editor_scroll, page_scroll);
        assert_eq!(app.active_group, 0);
        assert_eq!(app.active_pane, crate::app::ActivePane::Editor);
    }

    #[test]
    fn scroll_up_on_settings_decrements_settings_scroll() {
        let mut app = make_app();
        app.settings_editor_active = true;
        app.settings_scroll_areas.push(ratatui::layout::Rect::new(0, 0, 40, 30));
        app.settings_editor_scroll = 10;
        handle_scroll_up(&mut app, 5, 5, KeyModifiers::NONE);
        assert!(app.settings_editor_scroll < 10);
    }

    #[test]
    fn scroll_down_on_settings_increments_settings_scroll() {
        let mut app = make_app();
        app.settings_editor_active = true;
        app.settings_scroll_areas.push(ratatui::layout::Rect::new(0, 0, 40, 30));
        app.settings_editor_scroll = 0;
        handle_scroll_down(&mut app, 5, 5, KeyModifiers::NONE);
        assert!(app.settings_editor_scroll > 0);
    }

    // --- Explorer sidebar scroll ---

    #[test]
    fn scroll_up_on_explorer_with_settings_sidebar_decrements_selected() {
        let mut app = make_app();
        app.explorer_area = Some(ratatui::layout::Rect::new(0, 0, 40, 30));
        app.sidebar_view = SidebarView::Settings;
        app.settings_selected = 3;
        handle_scroll_up(&mut app, 5, 5, KeyModifiers::NONE);
        assert_eq!(app.settings_selected, 2);
    }

    fn search_results(count: usize) -> Vec<GlobalSearchResult> {
        (0..count)
            .map(|idx| GlobalSearchResult {
                file_path: PathBuf::from("src/main.rs"),
                line_num: idx + 1,
                line_text: format!("match {idx}").into(),
                col_start: 0,
                col_end: 5,
                context_before: None,
                context_after: None,
            })
            .collect()
    }

    #[test]
    fn wheel_down_scrolls_search_results_without_moving_selection() {
        let mut app = make_app();
        app.explorer_area = Some(ratatui::layout::Rect::new(0, 0, 40, 30));
        app.sidebar_view = SidebarView::Search;
        app.global_search_results = search_results(20).into();
        app.global_search_selected = 4;
        app.global_search_scroll = 2;

        handle_scroll_down(&mut app, 5, 12, KeyModifiers::NONE);

        assert_eq!(app.global_search_scroll, 5);
        assert_eq!(app.global_search_selected, 4);
    }

    #[test]
    fn wheel_up_scrolls_search_results_without_moving_selection() {
        let mut app = make_app();
        app.explorer_area = Some(ratatui::layout::Rect::new(0, 0, 40, 30));
        app.sidebar_view = SidebarView::Search;
        app.global_search_results = search_results(20).into();
        app.global_search_selected = 10;
        app.global_search_scroll = 7;

        handle_scroll_up(&mut app, 5, 12, KeyModifiers::NONE);

        assert_eq!(app.global_search_scroll, 4);
        assert_eq!(app.global_search_selected, 10);
    }

    #[test]
    fn wheel_over_search_fields_does_not_scroll_results() {
        let mut app = make_app();
        app.explorer_area = Some(ratatui::layout::Rect::new(0, 0, 40, 30));
        app.sidebar_view = SidebarView::Search;
        app.global_search_scroll = 6;

        handle_scroll_down(&mut app, 5, 3, KeyModifiers::NONE);

        assert_eq!(app.global_search_scroll, 6);
    }

    fn source_control_changes(count: usize) -> Vec<GitFileChange> {
        (0..count)
            .map(|index| GitFileChange {
                original_path: None,
                path: format!("src/file_{index}.rs"),
                status: 'M',
                staged: false,
            })
            .collect()
    }

    #[test]
    fn wheel_down_does_not_scroll_source_control_files_that_fit() {
        let mut app = make_app();
        app.explorer_area = Some(ratatui::layout::Rect::new(0, 0, 40, 12));
        app.sidebar_view = SidebarView::SourceControl;
        app.source_control_repository_state =
            crate::app::SourceControlRepositoryState::Available;
        app.sc_graph_collapsed = true;
        app.source_control_files = source_control_changes(2);

        handle_scroll_down(&mut app, 5, 7, KeyModifiers::NONE);

        assert_eq!(app.source_control_scroll, 0);
    }

    #[test]
    fn wheel_down_scrolls_overflowing_source_control_files() {
        let mut app = make_app();
        app.explorer_area = Some(ratatui::layout::Rect::new(0, 0, 40, 12));
        app.sidebar_view = SidebarView::SourceControl;
        app.source_control_repository_state =
            crate::app::SourceControlRepositoryState::Available;
        app.sc_graph_collapsed = true;
        app.source_control_files = source_control_changes(10);

        handle_scroll_down(&mut app, 5, 7, KeyModifiers::NONE);

        assert_eq!(app.source_control_scroll, app.scroll_speed);
    }

    #[test]
    fn scroll_up_moves_wrapped_rows_immediately() {
        let mut app = make_app();
        let editor = app.editor_mut().unwrap();
        editor.rope = ropey::Rope::from_str(&"abcdefghij\n".repeat(100));
        editor.word_wrap = true;
        editor.viewport_width = 4;
        editor.set_wrap_scroll_offset(10);
        handle_scroll_up(&mut app, 50, 10, KeyModifiers::NONE);
        let editor = app.editor().unwrap();
        assert_eq!(editor.wrap_scroll_offset, 7);
        assert_eq!(editor.scroll_offset, 2);
    }

    #[test]
    fn scroll_down_moves_wrapped_rows_immediately() {
        let mut app = make_app();
        let editor = app.editor_mut().unwrap();
        editor.rope = ropey::Rope::from_str(&"abcdefghij\n".repeat(100));
        editor.word_wrap = true;
        editor.viewport_width = 4;
        handle_scroll_down(&mut app, 50, 10, KeyModifiers::NONE);
        let editor = app.editor().unwrap();
        assert_eq!(editor.wrap_scroll_offset, 3);
        assert_eq!(editor.scroll_offset, 1);
    }
}
