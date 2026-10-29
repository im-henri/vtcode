use crossterm::event::{MouseButton, MouseEventKind};
use crate::app::App;
use crate::helpers::len_as_u16;
use unicode_width::UnicodeWidthStr;

/// Scroll a selection index up or down within bounds.
fn scroll_index(current: &mut usize, up: bool, max: usize) {
    if up {
        *current = current.saturating_sub(1);
    } else if *current < max {
        *current += 1;
    }
}

/// Handle mouse events for dialogs, popups, and modal overlays.
///
/// Returns `true` if the event was consumed (caller should return early),
/// `false` to continue with editor/chrome mouse handling.
pub(crate) fn handle_mouse_dialogs(
    app: &mut App,
    mouse: &crossterm::event::MouseEvent,
    col: u16,
    row: u16,
) -> bool {
    if crate::ui_quick_picker::handle_mouse(app, *mouse) {
        return true;
    }
    if app.hunk_popup_active && (app.hunk_peek.is_some() || app.hunk_preview_feedback.is_some()) {
        crate::ui_hunk_peek::handle_mouse(app, *mouse);
        return true;
    }
    // The folder browser replaces the workbench, so it gets modal priority
    // over stale editor, popup, notification, and extension hit regions.
    if app.change_dir_active {
        if app.handle_overlay_text_input_mouse(mouse.kind, col, row, mouse.modifiers) {
            return true;
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => app.folder_browser_click(col, row),
            MouseEventKind::ScrollUp => app.scroll_folder_browser(col, row, true),
            MouseEventKind::ScrollDown => app.scroll_folder_browser(col, row, false),
            _ => {}
        }
        return true;
    }

    // Use the actual rendered rows: dropdowns can shift left, flip upward,
    // or be clipped after resizing. Borders never select a command.
    if app.tab_context_active {
        let items = App::tab_context_items();
        let hit = app.tab_context_inner
            .filter(|inner| inner.contains((col, row).into()))
            .map(|inner| (row - inner.y) as usize)
            .filter(|&index| items.get(index).is_some_and(|item| {
                !item.1.is_empty() && !app.tab_context_command_disabled(item.1)
            }));
        match mouse.kind {
            MouseEventKind::Moved => {
                app.reset_hover_states();
                if let Some(index) = hit {
                    app.tab_context_selected = index;
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(index) = hit {
                    app.tab_context_selected = index;
                    crate::input_dialogs::handle_tab_context_key(app, crossterm::event::KeyEvent::new(
                        crossterm::event::KeyCode::Enter,
                        crossterm::event::KeyModifiers::NONE,
                    ));
                } else {
                    let in_menu = app.tab_context_inner.is_some_and(|inner| {
                        ratatui::layout::Rect::new(
                            inner.x.saturating_sub(1), inner.y.saturating_sub(1),
                            inner.width.saturating_add(2), inner.height.saturating_add(2),
                        ).contains((col, row).into())
                    });
                    if !in_menu {
                        app.tab_context_active = false;
                    }
                }
            }
            MouseEventKind::Down(_) => app.tab_context_active = false,
            _ => {}
        }
        // Dismissal, hovering and scrolling must not reach controls underneath.
        return true;
    }

    // Handle mouse clicks on popup panels
    if mouse.kind == MouseEventKind::Down(MouseButton::Left)
        && crate::mouse_overlay::handle_popup_click(app, col, row)
    {
        return true;
    }

    // Scroll wheel on popups
    if app.settings_active || app.shortcuts_active {
        match mouse.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = mouse.kind == MouseEventKind::ScrollUp;
                if app.settings_active {
                    let max = app.settings_items().len().saturating_sub(1);
                    scroll_index(&mut app.settings_selected, up, max);
                } else if app.shortcuts_active {
                    if up {
                        app.shortcuts_scroll = app.shortcuts_scroll.saturating_sub(1);
                    } else {
                        app.shortcuts_scroll += 1;
                    }
                }
                return true;
            }
            _ => {}
        }
    }

    // Mouse hover on popup list items
    if mouse.kind == MouseEventKind::Moved {
        // Only an obscuring popup clears the workbench underneath it. Clearing
        // these fields during ordinary movement loses the old hover used for
        // repaint detection and restarts tab tooltips on every mouse sample.
        if app.explorer_context_active || app.sc_context_active || app.hunk_popup_active
            || (app.menu_active && app.menu_open)
            || (app.settings_editor_active && app.settings_dropdown_open)
        {
            app.reset_hover_states();
        }
        let (term_w, term_h) = (app.viewport_area.width, app.viewport_area.height);

        // Explorer context menu hover
        if app.explorer_context_active {
            let items = app.explorer_context_menu_items();
            if let Some(inner) = app.explorer_context_inner {
                if inner.contains((col, row).into()) {
                    let idx = (row - inner.y) as usize;
                    if items.get(idx).is_some_and(|&i| i != "─") {
                        app.explorer_context_selected = idx;
                    }
                }
            }
        }

        // Source control context menu hover
        if app.sc_context_active {
            let items = app.source_control_context_menu_items();
            if let Some(inner) = app.sc_context_inner {
                if inner.contains((col, row).into()) {
                    let idx = (row - inner.y) as usize;
                    if idx < items.len() {
                        app.sc_context_selected = idx;
                    }
                }
            }
        }

        if app.hunk_popup_active {
            let popup_area = app.hunk_popup_rect(term_w, term_h);
            let inner_y = popup_area.y + 1;
            let items = app.hunk_popup_items();
            if col >= popup_area.x
                && col < popup_area.x + popup_area.width
                && row >= inner_y
                && row < inner_y + len_as_u16(items.len())
            {
                app.hunk_popup_selected = (row - inner_y) as usize;
            }
        }

        // Menu bar hover: highlight labels and switch dropdown
        if app.menu_active && app.menu_open {
            if let Some(menu_area) = app.menu_bar_area {
                if row == menu_area.y {
                    let labels = App::menu_labels();
                    let mut x = menu_area.x + 3;
                    for (i, label) in labels.iter()
                        .take(crate::ui_chrome::visible_menu_label_count(menu_area.width)).enumerate() {
                        let w = UnicodeWidthStr::width(*label) as u16 + 2;
                        if col >= x && col < x + w {
                            app.menu_selected = i;
                            app.menu_item_selected = 0;
                            return true;
                        }
                        x += w;
                    }
                }
            }
            // Hover over dropdown items
            let labels = App::menu_labels();
            let items = app.menu_items(app.menu_selected);
            let menu_x = 3u16 + labels.iter().take(app.menu_selected).map(|l| UnicodeWidthStr::width(*l) as u16 + 2).sum::<u16>();
            let menu_y = if let Some(bar) = app.menu_bar_area { bar.y + 1 } else { 1 };
            let max_w = items.iter().map(|(l, _)| UnicodeWidthStr::width(l.as_str())).max().unwrap_or(20) as u16 + 4;
            if row > menu_y && row < menu_y + 1 + len_as_u16(items.len())
                && col >= menu_x && col < menu_x + max_w
            {
                let idx = (row - menu_y - 1) as usize;
                if let Some((label, _)) = items.get(idx) {
                    if label != "─" {
                        app.menu_item_selected = idx;
                    }
                }
            }
            return true;
        }

        if app.settings_editor_active && app.settings_dropdown_open {
            if let Some(dd_area) = app.settings_dropdown_area {
                if col >= dd_area.x
                    && col < dd_area.x + dd_area.width
                    && row >= dd_area.y
                    && row < dd_area.y + dd_area.height
                {
                    let option_idx = app.settings_dropdown_scroll + (row - dd_area.y) as usize;
                    if option_idx < app.settings_dropdown_options.len()
                        && option_idx != app.settings_dropdown_selected
                    {
                        app.settings_dropdown_selected = option_idx;
                        if let Some(id) = app.settings_dropdown_setting_id.clone() {
                            if let Some(value) =
                                app.settings_dropdown_options.get(option_idx).cloned()
                            {
                                app.preview_settings_dropdown_value(&id, &value);
                            }
                        }
                    }
                    return true;
                }
            }
        }

        // Hover uses the same rectangles as the rendered controls and clicks.
        if app.file_search_bar_at(col, row).is_some() {
            use crate::ui_search_controls::{SearchControl, SearchScope, control_at};
            app.hovered_search_toggle = control_at(app, SearchScope::File, col, row).map(|button| {
                match button.control {
                    SearchControl::MatchCase => 0,
                    SearchControl::WholeWord => 1,
                    SearchControl::Regex => 2,
                    SearchControl::InSelection => 3,
                    SearchControl::PreserveCase => 4,
                    SearchControl::Close => 99,
                    _ => 5,
                }
            });
            return true;
        }

        // Global search close button hover
        if app.global_search_active
            && !(app.show_explorer && app.sidebar_view == crate::app::SidebarView::Search)
        {
            let width = 70u16.min(term_w.saturating_sub(4));
            let px = (term_w.saturating_sub(width)) / 2;
            let py = 2u16;
            let close_x = px + width - 4;
            app.hovered_global_search_close = row == py && col >= close_x && col < px + width;
        } else {
            app.hovered_global_search_close = false;
        }

        // Block hover events when any modal popup is open
        if app.goto_line_active || app.binary_open_prompt_active || app.confirm_close_active || app.confirm_delete_active || app.confirm_move_active || app.sc_discard_confirm_active || app.exit_confirm_active
            || app.symbol_list_active
            || (app.global_search_active
                && !(app.show_explorer && app.sidebar_view == crate::app::SidebarView::Search))
        {
            return true;
        }

        // Not consumed by dialog hover handling – fall through to chrome hovers
        return false;
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{MouseEvent, MouseEventKind, MouseButton, KeyModifiers};
    use std::path::PathBuf;

    fn make_mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn scroll_index_up() {
        let mut idx = 3;
        scroll_index(&mut idx, true, 10);
        assert_eq!(idx, 2);
    }

    #[test]
    fn scroll_index_up_at_zero() {
        let mut idx = 0;
        scroll_index(&mut idx, true, 10);
        assert_eq!(idx, 0);
    }

    #[test]
    fn scroll_index_down() {
        let mut idx = 3;
        scroll_index(&mut idx, false, 10);
        assert_eq!(idx, 4);
    }

    #[test]
    fn scroll_index_down_at_max() {
        let mut idx = 10;
        scroll_index(&mut idx, false, 10);
        assert_eq!(idx, 10);
    }

    #[test]
    fn handle_mouse_dialogs_no_dialog_active() {
        let mut app = App::new(PathBuf::from("."));
        let mouse = make_mouse(MouseEventKind::Down(MouseButton::Left), 5, 5);
        let consumed = handle_mouse_dialogs(&mut app, &mouse, 5, 5);
        assert!(!consumed);
    }

    #[test]
    fn handle_mouse_dialogs_confirm_close_esc_area() {
        let mut app = App::new(PathBuf::from("."));
        app.confirm_close_active = true;
        // Click outside dialog area — should dismiss
        let mouse = make_mouse(MouseEventKind::Down(MouseButton::Left), 0, 0);
        let consumed = handle_mouse_dialogs(&mut app, &mouse, 0, 0);
        // Should consume the event (dialog was active)
        assert!(consumed);
    }

    #[test]
    fn handle_mouse_dialogs_scroll_in_palette() {
        let mut app = App::new(PathBuf::from("."));
        app.palette.active = true;
        app.palette.selected = 0;
        // Scroll down in palette area
        let mouse = make_mouse(MouseEventKind::ScrollDown, 40, 5);
        let consumed = handle_mouse_dialogs(&mut app, &mouse, 40, 5);
        assert!(consumed);
    }

    #[test]
    fn handle_mouse_dialogs_no_dialog_returns_false() {
        let mut app = App::new(PathBuf::from("."));
        // No dialogs active — clicking anywhere should not be consumed
        let mouse = make_mouse(MouseEventKind::Down(MouseButton::Left), 50, 10);
        assert!(!handle_mouse_dialogs(&mut app, &mouse, 50, 10));
    }
}
