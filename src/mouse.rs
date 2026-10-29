use crossterm::event;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;
use crate::app::{ActivePane, App, TerminalDock};
use crate::helpers::len_as_u16;
use unicode_width::UnicodeWidthStr;
use crate::ui_chrome_tabs::TabBarHits;
#[cfg(test)]
use crate::ui_chrome_tabs::tab_display_name;
#[cfg(test)]
pub(crate) use crate::ui_chrome_tabs::tab_render_width;

const ACTIVITY_SETTINGS_INDEX: usize = 6;

pub(crate) fn explorer_resize_hit(
    area: ratatui::layout::Rect,
    sidebar_right: bool,
    col: u16,
    row: u16,
) -> bool {
    if row < area.y || row >= area.y.saturating_add(area.height) {
        return false;
    }
    let border_col = if sidebar_right {
        area.x
    } else {
        area.x.saturating_add(area.width.saturating_sub(1))
    };
    col == border_col
}

fn activity_item_at(area: ratatui::layout::Rect, row: u16) -> Option<usize> {
    crate::ui_chrome::activity_item_regions(area).iter()
        .find(|(item, _)| row >= item.y && row < item.bottom()).map(|(_, index)| *index)
}
fn open_editor_close_hit(area: ratatui::layout::Rect, col: u16) -> bool {
    col >= area.x.saturating_add(area.width).saturating_sub(3)
        && col < area.x.saturating_add(area.width)
}

pub(crate) fn handle_mouse(app: &mut App, mouse: event::MouseEvent) {
    if matches!(mouse.kind, MouseEventKind::Down(_)) {
        app.chord_pending = false;
        app.terminal_clicks.press();
        app.context_menu_button_drag = None;
    }
    if matches!(mouse.kind, MouseEventKind::Down(_) | MouseEventKind::Up(MouseButton::Left)) {
        app.editor_selection_drag = None;
    }
    if crate::ui_vtcomplete_index::handle_mouse(app, mouse) {
        return;
    }
    if matches!(mouse.kind, MouseEventKind::Down(_) | MouseEventKind::Drag(_)) {
        app.copy_animation.clear();
    }
    if crate::ui_panels::captured_mouse(app, mouse) { return; }
    if crate::ui_side_notice::handle_mouse(app, mouse) { return; }
    if crate::ui_logs::mouse(app, mouse) { return; }
    if crate::ui_remote_connection::handle_mouse(app, mouse) { return; }
    if app.custom_exit_remaining > 0 { return; }
    let deletion_hover_changed = mouse.kind == MouseEventKind::Moved
        && app.file_deletion.as_ref().and_then(|deletion| deletion.area).is_some_and(|area|
            area.contains((mouse.column, mouse.row).into()) || area.contains((app.mouse_col, app.mouse_row).into()));
    if crate::ui_file_deletion::handle_mouse(app, mouse) { app.mouse_hover_changed |= deletion_hover_changed; return; }
    if handle_navigation_mouse(app, mouse) {
        return;
    }
    app.record_navigation();
    handle_mouse_inner(app, mouse);
    app.mouse_hover_changed |= deletion_hover_changed;
    app.finish_navigation();
}

/// Side buttons navigate the workbench unless a child requests mouse input.
/// Consume releases and drags too, so one gesture navigates exactly once.
pub(crate) fn handle_navigation_mouse(app: &mut App, mouse: event::MouseEvent) -> bool {
    if crate::ui_modal::active(app).is_none() && app.terminal_wants_mouse(mouse) {
        return false;
    }
    match mouse.kind {
        MouseEventKind::Down(button @ (MouseButton::Back | MouseButton::Forward)) => {
            if app.mouse_back_forward_to_navigate && crate::ui_modal::active(app).is_none() {
                if button == MouseButton::Back {
                    app.navigate_back();
                } else {
                    app.navigate_forward();
                }
            }
            true
        }
        MouseEventKind::Up(MouseButton::Back | MouseButton::Forward)
        | MouseEventKind::Drag(MouseButton::Back | MouseButton::Forward) => true,
        _ => false,
    }
}

fn handle_mouse_inner(app: &mut App, mouse: event::MouseEvent) {
    if !matches!(mouse.kind, MouseEventKind::Moved | MouseEventKind::Up(_)) {
        app.dismiss_lsp_hover();
    }
    app.sync_text_input_sessions();
    let col = mouse.column;
    let row = mouse.row;
    let prev_mouse_col = app.mouse_col;
    let prev_mouse_row = app.mouse_row;
    let was_over_performance = app.background.activity.details_open && app.performance_area
        .is_some_and(|area| area.contains((prev_mouse_col, prev_mouse_row).into()));
    let was_over_blame = crate::editor_blame::hovered(app, prev_mouse_col, prev_mouse_row);
    let was_over_search_control = app.search_control_regions.iter()
        .any(|button| button.area.contains((prev_mouse_col, prev_mouse_row).into()));
    let was_over_zen_exit = app.zen_mode && app.zen_exit_area
        .is_some_and(|area| area.contains((prev_mouse_col, prev_mouse_row).into()));
    let was_over_agent_chat = app.menu_bar_area
        .and_then(crate::ui_chrome::agent_chat_button_area)
        .is_some_and(|area| area.contains((prev_mouse_col, prev_mouse_row).into()));
    let was_over_navigation = crate::ui_chrome::navigation_button_at(app, prev_mouse_col, prev_mouse_row).is_some();
    let was_over_source_control = app.show_explorer
        && app.sidebar_view == crate::app::SidebarView::SourceControl
        && (app.source_control_detail_area().is_some_and(|area| area.contains((prev_mouse_col, prev_mouse_row).into()))
            || [app.source_control_toggle_area, app.source_control_commit_area, app.source_control_amend_area,
            app.source_control_stage_all_area, app.source_control_unstage_all_area, app.source_control_discard_all_area,
            app.source_control_untracked_stage_area, app.source_control_untracked_discard_area,
            app.source_control_actions_area, app.repositories.reopen_button]
            .into_iter()
            .flatten()
            .any(|area| area.contains((prev_mouse_col, prev_mouse_row).into())));

    if crate::ui_desks::handle_mouse(app, mouse) { return; }
    // Track mouse position for hover effects during rendering
    app.mouse_row = row;
    app.mouse_col = col;
    crate::ui_inline_blame::leave(app, col, row);
    let modifiers_changed = app.update_link_hover_modifiers(mouse.modifiers);
    if crate::ui_search_dialog::handle_search_bar_resize(app, mouse) { return; }
    // Deliver leave before chrome/modal hover handlers can consume the move.
    if mouse.kind == MouseEventKind::Moved
        && app.code_area.is_some_and(|area| area.contains((prev_mouse_col, prev_mouse_row).into())
            && !area.contains((col, row).into())) {
        app.handle_active_extension_view_hover(col, row, (prev_mouse_col, prev_mouse_row), mouse.modifiers);
    }

    // Alt+click is an edge-triggered cursor toggle. Some terminals emit a
    // left-drag event as part of an otherwise stationary click; never let the
    // rest of that gesture move the primary caret or toggle another cursor.
    if app.alt_cursor_mouse_down_active {
        match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) => return,
            MouseEventKind::Up(MouseButton::Left) => {
                app.alt_cursor_mouse_down_active = false;
                app.drag_start_pos = None;
                app.drag_has_moved = false;
                return;
            }
            _ => {}
        }
    }
    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        app.alt_cursor_mouse_down_active = false;
        app.explorer_scrollbar_drag = None;
    }

    // A gesture that began in a terminal keeps its drag/release even if a
    // popup appears mid-gesture. A fresh press always ends the old capture.
    if matches!(mouse.kind, MouseEventKind::Down(_)) {
        app.terminal_mouse_capture = None;
        if let Some(drag) = app.terminal_selection_drag.take() {
            if let Some(pty) = app.terminals.get_mut(drag.index)
                .and_then(|terminal| terminal.terminal_pty.as_mut()) {
                pty.grid.finish_selection();
            }
        }
    } else if (app.terminal_selection_drag.is_some() || app.terminal_mouse_capture.is_some())
        && matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_))
        && app.handle_terminal_selection_mouse(mouse) {
        return;
    }

    if app.handle_extension_pointer_capture(mouse) { return; }

    // A save error or recovery prompt can appear over the folder browser.
    // Its visible controls must own input before the full-workbench browser.
    if crate::ui_modal::handle_mouse(app, mouse) {
        return;
    }

    if app.change_dir_active {
        if mouse.kind == MouseEventKind::Moved && (col, row) != (prev_mouse_col, prev_mouse_row) {
            app.hover_folder_browser_button(col, row);
        }
        crate::mouse_dialogs::handle_mouse_dialogs(app, &mouse, col, row);
        return;
    }

    if crate::ui_quick_picker::handle_mouse(app, mouse) {
        return;
    }
    // A scrollbar gesture keeps its pointer when it crosses the resize seam,
    // editor, or other workbench surfaces. Release must end that capture there.
    if app.commit_input.scrollbar_drag.is_some()
        && app.handle_commit_input_scrollbar_mouse(mouse.kind, col, row)
    {
        return;
    }
    if app.explorer_scrollbar_drag.is_some() || app.settings_scrollbar_drag.is_some() {
        match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                crate::mouse_drag::handle_left_drag(app, col, row, prev_mouse_col, prev_mouse_row, mouse.modifiers);
                return;
            }
            MouseEventKind::Up(MouseButton::Left) => {
                crate::mouse_drag::handle_left_up(app, col, row, mouse.modifiers);
                return;
            }
            _ => {}
        }
    }
    if crate::ui_status::handle_mouse(app, mouse) { return; }
    if crate::ui_performance_details::handle_mouse(app, mouse, (prev_mouse_col, prev_mouse_row)) { return; }

    // Tab menus render above Find and other workbench input fields. Only
    // extension overlays are drawn above them and get first refusal here.
    if app.tab_context_active {
        if !crate::mouse_overlay::handle_overlay_mouse(app, mouse.kind, col, row) {
            crate::mouse_dialogs::handle_mouse_dialogs(app, &mouse, col, row);
        }
        return;
    }

    // Shared fields get first refusal so modal handlers cannot swallow clicks
    // in their input rows before caret placement and drag selection run.
    if app.handle_overlay_text_input_mouse(mouse.kind, col, row, mouse.modifiers) {
        return;
    }

    // Extension overlays get first priority (they render on top of everything)
    if crate::mouse_overlay::handle_overlay_mouse(app, mouse.kind, col, row) {
        return;
    }

    // Dialog/popup mouse handling (modals, context menus, notifications, picker scrolling, hover)
    if crate::mouse_dialogs::handle_mouse_dialogs(app, &mouse, col, row) {
        return;
    }
    if crate::ui_panels::scroll_input(app, mouse) { return; }
    if mouse.kind == MouseEventKind::Moved
        && (crate::ui_search_dialog::search_bar_resize_hit(app, col, row)
            || crate::ui_search_dialog::search_bar_resize_hit(app, prev_mouse_col, prev_mouse_row))
    {
        app.mouse_hover_changed = (col, row) != (prev_mouse_col, prev_mouse_row);
        return;
    }
    if crate::ui_keybindings::handle_mouse(app, mouse) { return; }
    if crate::ui_commit_details::handle_mouse(app, mouse) { return; }
    if crate::ui_extension_info::handle_mouse(app, mouse) { return; }
    if crate::ui_lsp_completion::handle_mouse(app, mouse) {
        return;
    }
    if crate::ui_search_dialog::handle_vtcomplete_mouse(app, mouse) { return; }
    if crate::ui_inline_blame::handle_mouse(app, mouse) { return; }
    if app.handle_terminal_selection_mouse(mouse) { return; }
    if app.handle_chat_mouse(mouse) { return; }
    if mouse.kind == MouseEventKind::Moved
        && (was_over_blame || crate::editor_blame::hovered(app, col, row)) {
        app.mouse_hover_changed = (col, row) != (prev_mouse_col, prev_mouse_row);
        app.hovered_gutter_line = None;
        app.hovered_editor_pos = None;
        return;
    }
    if crate::editor_blame::handle_mouse(app, mouse) { return; }
    if app.handle_editor_review_mouse(mouse) { return; }

    if app.zen_mode && app.zen_exit_area.is_some_and(|area| area.contains((col, row).into())) {
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            app.execute_palette_command("toggle_zen");
        }
        app.mouse_hover_changed = (col, row) != (prev_mouse_col, prev_mouse_row);
        return;
    }

    // Settings dropdowns can flip above their control in short panes, covering
    // the search field and scope buttons. Resolve their clicks first.
    if app.settings_editor_active && app.settings_dropdown_open
        && mouse.kind == MouseEventKind::Down(MouseButton::Left)
    {
        if let Some((_, row)) = app.theme_management.delete_hits.iter().find(|(area, _)| area.contains((col, row).into())) {
            let row = *row;
            app.request_theme_removal(row);
            return;
        }
        if let Some(area) = app.settings_dropdown_area.filter(|area| area.contains((col, row).into())) {
            app.active_pane = ActivePane::Editor;
            app.terminal_panel_focused = false;
            let option_idx = app.settings_dropdown_scroll + (row - area.y) as usize;
            app.select_settings_dropdown_option(option_idx);
        } else {
            app.close_settings_dropdown();
        }
        return;
    }

    if app.settings_editor_active && app.settings_dropdown_open
        && app.settings_dropdown_setting_id.as_deref() == Some("theme")
    {
        if mouse.kind == MouseEventKind::Moved {
            app.mouse_hover_changed |= app.settings_dropdown_area.is_some_and(|area|
                area.contains((col, row).into()) || area.contains((prev_mouse_col, prev_mouse_row).into()));
            return;
        }
        if matches!(mouse.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown)
            && let Some(area) = app.settings_dropdown_area.filter(|area| area.contains((col, row).into()))
        {
            let max = app.settings_dropdown_options.len().saturating_sub(area.height as usize);
            app.settings_dropdown_scroll = if mouse.kind == MouseEventKind::ScrollUp {
                app.settings_dropdown_scroll.saturating_sub(3)
            } else { app.settings_dropdown_scroll.saturating_add(3).min(max) };
            let last = (app.settings_dropdown_scroll + area.height as usize).saturating_sub(1)
                .min(app.settings_dropdown_options.len().saturating_sub(1));
            app.settings_dropdown_selected = app.settings_dropdown_selected.clamp(app.settings_dropdown_scroll, last);
            app.theme_management.delete_hits.clear();
            return;
        }
    }

    // Non-modal fields run after overlay/dialog hit testing so an obscured
    // Settings/Search/Explorer field can never steal a click from a popup.
    if app.handle_background_text_input_mouse(mouse.kind, col, row, mouse.modifiers) {
        return;
    }
    if crate::ui_explorer_find::handle_mouse(app, mouse) { return; }

    // Focus is owned by the terminal only after an explicit click in its panel.
    // Clicking the editor, explorer, status bar, or another panel tab releases it.
    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        app.terminal_panel_focused = false;
        if app.search_active {
            app.search_input_focused = false;
            // Moving focus into the workbench accepts the current search location.
            app.search_pre_cursor = None;
        }
    }

    if mouse.kind == MouseEventKind::Moved {
        let old_explorer = app.hovered_explorer;
        let extension_hover = app.code_area.is_some_and(|area| area.contains((col, row).into()))
            && app.handle_active_extension_view_hover(col, row, (prev_mouse_col, prev_mouse_row), mouse.modifiers);
        // Fast path: if mouse is in the editor code area, skip chrome hover checks
        // and only update code-area-specific hover state.
        let in_code_area = app.code_area.is_some_and(|code| {
            col >= code.x && col < code.x + code.width
                && row >= code.y && row < code.y + code.height
        });
        let in_gutter = app.gutter_area.is_some_and(|ga| {
            col >= ga.x && col < ga.x + ga.width
                && row >= ga.y && row < ga.y + ga.height
        });
        if in_code_area || in_gutter {
            let fold_hover = crate::ui_editor_folds::at(app, col, row);
            let old_fold_hover = crate::ui_editor_folds::at(app, prev_mouse_col, prev_mouse_row);
            // Save old state to detect actual changes
            let old_gutter = app.hovered_gutter_line;
            let old_editor = app.hovered_editor_pos;
            let had_chrome_hover = app.hovered_tab.is_some()
                || app.hovered_tab_close_btn
                || app.hovered_tab_scroll_left || app.hovered_tab_scroll_right
                || app.hovered_new_tab_button
                || app.hovered_activity.is_some()
                || app.hovered_menu.is_some()
                || app.hovered_close_button
                || app.hovered_status_item.is_some()
                || app.hovered_panel_close
                || app.hovered_panel_tab.is_some()
                || app.hovered_terminal_plus
                || app.hovered_terminal_tab.is_some()
                || app.hovered_terminal_close.is_some()
                || app.hovered_terminal_sidebar_toggle || app.outline_hovered.is_some()
                || app.hovered_problem_row.is_some()
                || app.hovered_scrollbar
                || app.hovered_minimap
                || app.hovered_h_scrollbar
                || app.hovered_explorer.is_some();
            // Clear all chrome hover states in one batch
            app.reset_hover_states();
            app.hovered_problem_row = None;
            // Handle gutter hover
            if extension_hover {
                app.hovered_gutter_line = None;
                app.hovered_editor_pos = None;
            } else if in_gutter {
                if let (Some(ga), Some(editor)) = (app.gutter_area, app.editor()) {
                    let rel_row = (row - ga.y) as usize;
                    let line = editor.scroll_offset + rel_row;
                    app.hovered_gutter_line = if line < editor.rope.len_lines() { Some(line) } else { None };
                }
                app.hovered_editor_pos = None;
            } else {
                app.hovered_gutter_line = None;
                // Editor code area hover
                app.hovered_editor_pos = if fold_hover.is_some() { None }
                    else { crate::app_lsp_links::editor_position_at(app, col, row) };
                if (col, row) != (prev_mouse_col, prev_mouse_row) {
                    // Ignore stationary reports (including modifier changes), but
                    // allow another hover even within the previously hovered word.
                    app.lsp_hover_suppressed = false;
                }
            }
            // The pointer position also drives delayed LSP requests, but plain
            // source text has no hover paint. Keep tracking it without forcing
            // the entire workbench through preparation, rendering and output.
            let editor_tooltip = crate::ui_editor_highlight::has_diagnostic_marker_tooltip(app, old_editor)
                || crate::ui_editor_highlight::has_diagnostic_marker_tooltip(app, app.hovered_editor_pos);
            let sticky_hover = app.sticky_scroll && !app.cached_sticky_lines.is_empty()
                && app.code_area.is_some_and(|area| {
                    let bottom = area.y.saturating_add(app.cached_sticky_lines.len().min(area.height as usize) as u16);
                    row < bottom || (prev_mouse_row >= area.y && prev_mouse_row < bottom)
                });
            // Only redraw if hover state actually changed or a painted tooltip
            // follows the pointer. Modifier/decorated/custom surfaces retain
            // their existing route, including removal of a previous hover.
            let changed = modifiers_changed || had_chrome_hover || was_over_search_control || was_over_zen_exit || was_over_source_control
                || was_over_agent_chat || was_over_navigation
                || was_over_performance
                || extension_hover || app.lsp_hover_active
                || mouse.modifiers.contains(KeyModifiers::CONTROL)
                || editor_tooltip || sticky_hover
                || app.chat.area.is_some_and(|area| area.contains((prev_mouse_col, prev_mouse_row).into()))
                || app.hovered_gutter_line != old_gutter
                || fold_hover != old_fold_hover
                || ((app.vtcomplete_active || app.lsp_completion_active)
                    && (col != prev_mouse_col || row != prev_mouse_row));
            app.mouse_hover_changed = changed;
            return;
        }

        // UI chrome hover tracking (always active, not just for popups)
        // Tab bar hover
        if let Some(tab_area) = app.tab_bar_area {
            if row >= tab_area.y && row < tab_area.y + tab_area.height {
                let hits = TabBarHits::new(app, tab_area);
                let hovered = hits.tab_at(col);
                app.hovered_tab = hovered.map(|(index, _, _)| index);
                // Commit details uses its editor path for the selected diff,
                // not the tab itself; it must not expose that path as a tooltip.
                let tooltip_tab = app.hovered_tab.filter(|&index| {
                    app.tabs[index].kind != crate::app::TabKind::CommitDetails
                        && app.tabs[index].editor.file_path.is_some()
                });
                if tooltip_tab != app.tab_tooltip_tab {
                    app.tab_tooltip_tab = tooltip_tab;
                    app.tab_tooltip_hover_start = tooltip_tab.map(|_| std::time::Instant::now());
                    app.tab_tooltip_shown = false;
                }
                // Detect if hovering over the close button area of a tab
                app.hovered_tab_close_btn = hovered
                    .is_some_and(|(_, _, close)| TabBarHits::contains(close, col));
                // Check if hovering over "+" new tab button (right after last visible tab)
                app.hovered_new_tab_button = TabBarHits::contains(hits.new_tab, col);
                // Check if hovering scroll arrows
                app.hovered_tab_scroll_left = TabBarHits::contains(hits.left, col);
                app.hovered_tab_scroll_right = TabBarHits::contains(hits.right, col);
            } else {
                app.hovered_tab = None;
                app.hovered_tab_close_btn = false;
                app.hovered_new_tab_button = false;
                app.hovered_tab_scroll_left = false;
                app.hovered_tab_scroll_right = false;
                app.clear_tab_tooltip();
            }
        }
        // Activity bar hover
        if let Some(ab_area) = app.activity_bar_area {
            if col >= ab_area.x && col < ab_area.x + ab_area.width
                && row >= ab_area.y && row < ab_area.y + ab_area.height
            {
                app.hovered_activity = activity_item_at(ab_area, row);
            } else {
                app.hovered_activity = None;
            }
        }
        // Menu bar hover
        if let Some(menu_area) = app.menu_bar_area {
            if row == menu_area.y {
                let labels = App::menu_labels();
                let visible_count =
                    crate::ui_chrome::visible_menu_label_count(menu_area.width);
                let mut x = menu_area.x + 3;
                let mut found = false;
                for (i, label) in labels
                    .iter()
                    .take(visible_count)
                    .enumerate()
                {
                    let w = UnicodeWidthStr::width(*label) as u16 + 2;
                    if col >= x && col < x + w {
                        app.hovered_menu = Some(i);
                        found = true;
                        break;
                    }
                    x += w;
                }
                if !found && crate::ui_chrome::menu_overflow_visible(menu_area.width) {
                    let width = UnicodeWidthStr::width(crate::ui_chrome::MENU_OVERFLOW_LABEL)
                        as u16;
                    if col >= x && col < x.saturating_add(width) {
                        app.hovered_menu = Some(visible_count);
                        found = true;
                    }
                }
                if !found { app.hovered_menu = None; }
                // Close button hover (rightmost 3 chars of title bar)
                let right_edge = menu_area.x + menu_area.width;
                app.hovered_close_button = col >= right_edge.saturating_sub(3);
            } else {
                app.hovered_menu = None;
                app.hovered_close_button = false;
            }
        }
        // Status bar hover - use stored item positions from rendering
        if let Some(sb_area) = app.status_bar_area {
            if row == sb_area.y && col >= sb_area.x && col < sb_area.x + sb_area.width {
                app.hovered_status_item = None;
                for (idx, &(ix, iw, _action)) in app.status_bar_items.iter().enumerate() {
                    if col >= ix && col < ix + iw {
                        app.hovered_status_item = Some(idx);
                        break;
                    }
                }
            } else {
                app.hovered_status_item = None;
            }
        }
        // Panel close button hover
        if let Some(panel_area) = app.panel_area {
            app.hovered_panel_close = row == panel_area.y && col >= panel_area.x + panel_area.width.saturating_sub(3) && col < panel_area.x + panel_area.width;
            // Panel tab hover (OUTPUT/PROBLEMS/TERMINAL/NOTIFICATIONS)
            app.hovered_panel_tab = None;
            if app.show_panel && row == panel_area.y {
                app.hovered_panel_tab = app.panel_tab_areas.iter().find(|(area, _)| area.contains((col, row).into())).map(|(_, index)| *index);
                app.mouse_hover_changed |= app.panel_chrome.maximize.contains((col, row).into()) || app.panel_chrome.maximize.contains((prev_mouse_col, prev_mouse_row).into());
            }

            // Problem row hover
            if app.show_panel && app.panel_tab == 1 && row > panel_area.y && row < panel_area.y + panel_area.height {
                app.hovered_problem_row = crate::ui_panels::hovered_problem(app, col, row);
            } else {
                app.hovered_problem_row = None;
            }

        } else {
            app.hovered_panel_close = false;
            app.hovered_panel_tab = None;
            app.hovered_problem_row = None;
        }
        let side_plus_hovered = app.terminal_sidebar_plus_area.is_some_and(|area| {
            col >= area.x
                && col < area.x + area.width
                && row >= area.y
                && row < area.y + area.height
        });
        app.hovered_terminal_plus = side_plus_hovered;
        app.hovered_terminal_sidebar_toggle =
            app.terminal_sidebar_toggle_area.is_some_and(|area| {
                col >= area.x
                    && col < area.x + area.width
                    && row >= area.y
                    && row < area.y + area.height
            });

        app.hovered_terminal_tab = None;
        app.hovered_terminal_close = None;
        for &(area, terminal_index) in &app.terminal_sidebar_close_areas {
            if col >= area.x
                && col < area.x + area.width
                && row >= area.y
                && row < area.y + area.height
            {
                app.hovered_terminal_close = Some(terminal_index);
                break;
            }
        }
        if app.hovered_terminal_close.is_none() {
            for &(area, terminal_index) in &app.terminal_sidebar_tab_areas {
                if col >= area.x
                    && col < area.x + area.width
                    && row >= area.y
                    && row < area.y + area.height
                {
                    app.hovered_terminal_tab = Some(terminal_index);
                    break;
                }
            }
        }
        // Scrollbar hover
        app.hovered_scrollbar = app.scrollbar_area.is_some_and(|area| {
            col >= area.x && col < area.x + area.width
                && row >= area.y && row < area.y + area.height
        });
        // Minimap hover
        app.hovered_minimap = app.minimap_area.is_some_and(|area| {
            col >= area.x && col < area.x + area.width
                && row >= area.y && row < area.y + area.height
        });
        app.hovered_h_scrollbar = app.h_scrollbar_area.is_some_and(|area| {
            col >= area.x && col < area.x + area.width
                && row >= area.y && row < area.y + area.height
        });
        // Explorer hover
        if let Some(exp_area) = app.explorer_area {
            if col >= exp_area.x && col < exp_area.x + exp_area.width
                && row >= exp_area.y && row < exp_area.y + exp_area.height
                && app.sidebar_view == crate::app::SidebarView::Explorer
            {
                let tree_area = crate::ui_chrome::explorer_tree_content_area(app, exp_area);
                let tree_start = tree_area.y;
                if tree_area.contains((col, row).into()) {
                    let rel_row = (row - tree_start) as usize;
                    let visible_height = tree_area.height as usize;
                    app.hovered_explorer = app.explorer.entry_at_row(rel_row, visible_height);
                } else {
                    app.hovered_explorer = None;
                }
            } else {
                app.hovered_explorer = None;
            }
        }
        // Outline hover
        if let Some(exp_area) = app.explorer_area {
            if app.sidebar_view == crate::app::SidebarView::Outline
                && col >= exp_area.x && col < exp_area.x + exp_area.width
                && row > exp_area.y && row < exp_area.y + exp_area.height
            {
                let rel_row = (row - exp_area.y - 1) as usize;
                let idx = app.outline_scroll + rel_row;
                if idx < app.outline_items.len() {
                    app.outline_hovered = Some(idx);
                } else {
                    app.outline_hovered = None;
                }
            } else if app.sidebar_view == crate::app::SidebarView::Outline {
                app.outline_hovered = None;
            }
        }
        // Gutter hover - detect line for blame tooltip
        if let Some(gutter) = app.gutter_area {
            if col >= gutter.x && col < gutter.x + gutter.width
                && row >= gutter.y && row < gutter.y + gutter.height
            {
                let rel_row = (row - gutter.y) as usize;
                if let Some(editor) = app.editor() {
                    let line = editor.scroll_offset + rel_row;
                    if line < editor.rope.len_lines() {
                        app.hovered_gutter_line = Some(line);
                    } else {
                        app.hovered_gutter_line = None;
                    }
                }
            } else {
                app.hovered_gutter_line = None;
            }
        } else {
            app.hovered_gutter_line = None;
        }
        // Editor code area hover - detect line/col for diagnostic tooltip
        app.hovered_editor_pos = crate::app_lsp_links::editor_position_at(app, col, row);
        // Before metadata is available there is no following tooltip to move.
        // Horizontal motion within the same Explorer row leaves its paint
        // unchanged and must not repaint the editor behind it.
        if let Some(index) = app.hovered_explorer.filter(|index| Some(*index) == old_explorer)
            && app.explorer.entries.get(index).is_some_and(|entry| !app.explorer_hover_info_cache.contains_key(&entry.path))
            && app.explorer_area.is_some_and(|area| {
                let tree = crate::ui_chrome::explorer_tree_content_area(app, area);
                tree.contains((col, row).into()) && tree.contains((prev_mouse_col, prev_mouse_row).into())
            })
        {
            app.mouse_hover_changed = modifiers_changed;
        }
        return;
    }

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.dragging_minimap = false;
            app.minimap_drag_start = None;
            app.dragging_terminal_tab = None;
            app.drag_start_pos = Some((col, row));
            app.drag_has_moved = false;

            // Click on menu bar?
            if let Some(menu_area) = app.menu_bar_area {
                if row == menu_area.y {
                    if let Some((_, forward)) = crate::ui_chrome::navigation_button_at(app, col, row) {
                        app.menu_active = false;
                        app.menu_open = false;
                        app.hovered_menu = None;
                        app.clear_drag_states();
                        app.drag_start_pos = None;
                        if app.navigation.can_navigate(forward) {
                            if forward { app.navigate_forward(); } else { app.navigate_back(); }
                        }
                        return;
                    }
                    if crate::ui_chrome::agent_chat_button_area(menu_area)
                        .is_some_and(|area| area.contains((col, row).into()))
                    {
                        app.menu_active = false;
                        app.menu_open = false;
                        app.hovered_menu = None;
                        app.execute_palette_command("acp_chat");
                        return;
                    }
                    let labels = App::menu_labels();
                    let visible_count =
                        crate::ui_chrome::visible_menu_label_count(menu_area.width);
                    let mut x = menu_area.x + 3; // skip icon
                    for (i, label) in labels
                        .iter()
                        .take(visible_count)
                        .enumerate()
                    {
                        let w = UnicodeWidthStr::width(*label) as u16 + 2;
                        if col >= x && col < x + w {
                            if app.menu_active && app.menu_selected == i && app.menu_open {
                                app.menu_active = false;
                                app.menu_open = false;
                            } else {
                                app.menu_active = true;
                                app.menu_selected = i;
                                app.menu_open = true;
                                app.menu_item_selected = 0;
                            }
                            return;
                        }
                        x += w;
                    }
                    if crate::ui_chrome::menu_overflow_visible(menu_area.width) {
                        let width = UnicodeWidthStr::width(
                            crate::ui_chrome::MENU_OVERFLOW_LABEL,
                        ) as u16;
                        if col >= x && col < x.saturating_add(width) {
                            if app.menu_active
                                && app.menu_selected == visible_count
                                && app.menu_open
                            {
                                app.menu_active = false;
                                app.menu_open = false;
                            } else {
                                app.menu_active = true;
                                app.menu_selected = visible_count;
                                app.menu_open = true;
                                app.menu_item_selected = 0;
                            }
                            return;
                        }
                    }
                    // Close button (✕) at right edge: " ✕ " = 3 chars
                    let right_edge = menu_area.x + menu_area.width;
                    if col >= right_edge.saturating_sub(3) {
                        // ✕ close button
                        app.exit_confirm_active = true;
                        return;
                    }
                    // Clicked on menu bar but not on any label -> close
                    app.menu_active = false;
                    app.menu_open = false;
                    return;
                }
            }

            // Click on menu dropdown item?
            if app.menu_active && app.menu_open {
                let items = app.menu_items(app.menu_selected);
                let menu_x = crate::ui_chrome::menu_dropdown_anchor_x(app);
                let menu_y = if let Some(bar) = app.menu_bar_area { bar.y + 1 } else { 1 };
                let max_w = items.iter().map(|(l, _)| UnicodeWidthStr::width(l.as_str())).max().unwrap_or(20) as u16 + 4;

                if row > menu_y && row < menu_y + 1 + len_as_u16(items.len())
                    && col >= menu_x && col < menu_x + max_w
                {
                    let item_idx = (row - menu_y - 1) as usize;
                    if let Some((label, cmd_id)) = items.get(item_idx) {
                        if *label != "─" && !app.is_menu_cmd_disabled(cmd_id) {
                            let cmd = cmd_id.to_string();
                            app.menu_active = false;
                            app.menu_open = false;
                            match cmd.as_str() {
                                "quit_menu" => { app.exit_confirm_active = true; }
                                "close_tab_menu" => { app.close_tab(app.active_tab); }
                                "palette_menu" => { app.open_palette(); }
                                "shortcuts_ref" => { app.shortcuts_active = !app.shortcuts_active; }
                                other => { app.execute_palette_command(other); }
                            }
                        }
                    }
                    return;
                }
                // Click outside dropdown -> close menu
                app.menu_active = false;
                app.menu_open = false;
            }

            // Click on activity bar?
            if let Some(ab_area) = app.activity_bar_area {
                if col >= ab_area.x
                    && col < ab_area.x + ab_area.width
                    && row >= ab_area.y
                    && row < ab_area.y + ab_area.height
                {
                    if let Some(icon_idx) = activity_item_at(ab_area, row) {
                        if icon_idx == ACTIVITY_SETTINGS_INDEX {
                            app.execute_palette_command("settings");
                            return;
                        }
                        app.activity_bar_selected = icon_idx;
                        if icon_idx != 1 {
                            app.global_search_active = false;
                        }
                        match icon_idx {
                            0 => {
                                // Explorer: toggle sidebar or switch to explorer view
                                if app.show_explorer && app.sidebar_view == crate::app::SidebarView::Explorer {
                                    app.show_explorer = false;
                                } else {
                                    app.show_explorer = true;
                                    app.sidebar_view = crate::app::SidebarView::Explorer;
                                }
                            }
                            1 => {
                                // A visible Search sidebar may not own keyboard focus.
                                // Refocus it before treating a repeated click as a toggle.
                                if app.show_explorer
                                    && app.sidebar_view == crate::app::SidebarView::Search
                                    && app.global_search_active
                                    && app.active_pane == ActivePane::Explorer
                                    && app.search_focus_field == 0
                                {
                                    app.show_explorer = false;
                                    app.global_search_active = false;
                                    app.active_pane = ActivePane::Editor;
                                } else {
                                    app.focus_global_search();
                                }
                            }
                            2 => {
                                // Source Control: toggle sidebar view
                                if app.show_explorer && app.sidebar_view == crate::app::SidebarView::SourceControl {
                                    app.show_explorer = false;
                                } else {
                                    app.show_explorer = true;
                                    app.sidebar_view = crate::app::SidebarView::SourceControl;
                                    app.refresh_source_control_async();
                                    app.refresh_git_log_async();
                                }
                            }
                            4 => {
                                // Extensions: toggle sidebar view
                                if app.show_explorer && app.sidebar_view == crate::app::SidebarView::Extensions {
                                    app.show_explorer = false;
                                } else {
                                    app.open_extensions_sidebar();
                                }
                            }
                            5 => {
                                // Outline: toggle sidebar view
                                if app.show_explorer && app.sidebar_view == crate::app::SidebarView::Outline {
                                    app.show_explorer = false;
                                } else {
                                    app.show_explorer = true;
                                    app.sidebar_view = crate::app::SidebarView::Outline;
                                    app.update_outline();
                                }
                            }
                            _ => {}
                        }
                    }
                    return;
                }
            }

            // Click in the collapsible right-side terminal dock.
            if let Some(side_area) = app.terminal_sidebar_area {
                if col >= side_area.x
                    && col < side_area.x + side_area.width
                    && row >= side_area.y
                    && row < side_area.y + side_area.height
                {
                    let in_rect = |area: Rect| {
                        col >= area.x
                            && col < area.x + area.width
                            && row >= area.y
                            && row < area.y + area.height
                    };
                    if app.terminal_sidebar_toggle_area.is_some_and(in_rect) {
                        app.toggle_terminal_sidebar();
                        return;
                    }
                    if !app.show_terminal_sidebar {
                        app.toggle_terminal_sidebar();
                        return;
                    }

                    let close_target = app
                        .terminal_sidebar_close_areas
                        .iter()
                        .find(|&&(area, _)| in_rect(area))
                        .map(|&(_, index)| index);
                    let tab_target = app
                        .terminal_sidebar_tab_areas
                        .iter()
                        .find(|&&(area, _)| in_rect(area))
                        .map(|&(_, index)| index);
                    if let Some(index) = close_target {
                        app.close_terminal_tab_at(index);
                        app.terminal_panel_focused = app
                            .active_terminal_in_dock(TerminalDock::Right)
                            .is_some();
                    } else if let Some(index) = tab_target {
                        app.active_terminal = index;
                        app.active_pane = ActivePane::Editor;
                        app.terminal_panel_focused = true;
                        app.dragging_terminal_tab = Some(index);
                    } else if app.terminal_sidebar_plus_area.is_some_and(in_rect) {
                        app.new_terminal_tab_in(TerminalDock::Right);
                        app.terminal_sidebar_revealed = true;
                        app.show_terminal_sidebar = true;
                        app.active_pane = ActivePane::Editor;
                        app.terminal_panel_focused = true;
                    } else if let Some(index) =
                        app.active_terminal_in_dock(TerminalDock::Right)
                    {
                        app.active_terminal = index;
                        app.active_pane = ActivePane::Editor;
                        app.terminal_panel_focused = true;
                    }
                    return;
                }
            }

            // Click on panel top border to start resize?
            if app.show_panel {
                if let Some(panel_area) = app.panel_area {
                    let border_y = panel_area.y.saturating_sub(1);
                    if row == border_y
                        && col >= panel_area.x
                        && col < panel_area.x + panel_area.width
                    {
                        let over_sc_graph_header = if app.sidebar_view == crate::app::SidebarView::SourceControl {
                            if let Some(exp_area) = app.source_control_detail_area() {
                                if col >= exp_area.x
                                    && col < exp_area.x + exp_area.width
                                    && row >= exp_area.y
                                    && row < exp_area.y + exp_area.height
                                {
                                    let rel_y = row.saturating_sub(exp_area.y);
                                    let (changes_h, graph_h) = crate::ui_source_control::source_control_split_heights(
                                        exp_area.height,
                                        app.sc_graph_collapsed,
                                        app.sc_graph_height,
                                    );
                                    graph_h > 0 && rel_y == changes_h
                                } else {
                                    false
                                }
                            } else {
                                false
                            }
                        } else {
                            false
                        };
                        if over_sc_graph_header {
                            // Let source-control graph header clicks win over panel-border resize.
                        } else {
                        app.resizing_panel = true;
                        return;
                        }
                    }
                }
            }

            // Click on panel?
            if app.show_panel && let Some(panel_area) = app.panel_area {
                if row >= panel_area.y && row < panel_area.y + panel_area.height
                    && col >= panel_area.x && col < panel_area.x + panel_area.width
                {
                    if crate::ui_panels::scrollbar_press(app, col, row) { return; }
                    crate::ui_panels::capture_pointer(app, MouseButton::Left);
                    if row == panel_area.y {
                        if app.panel_chrome.close.contains((col, row).into()) {
                            crate::ui_panels::close_panel(app);
                        } else if app.panel_chrome.maximize.contains((col, row).into()) {
                            app.panel_chrome.maximized = !app.panel_chrome.maximized;
                        } else if let Some((_, index)) = app.panel_tab_areas.iter().find(|(area, _)| area.contains((col, row).into())).copied() {
                            crate::ui_panels::select_tab(app, index);
                        }
                    } else if app.panel_tab == 1 {
                        crate::ui_panels::activate_problem(app, col, row);
                    }
                    return;
                }
            }

            // Click on status bar?
            if let Some(sb_area) = app.status_bar_area {
                if row >= sb_area.y && row < sb_area.y + sb_area.height {
                    // Find which status bar item was clicked
                    let mut clicked_action = None;
                    for &(ix, iw, action) in &app.status_bar_items {
                        if col >= ix && col < ix + iw {
                            clicked_action = Some(action);
                            break;
                        }
                    }
                    match clicked_action {
                        Some("performance") => {
                            crate::ui_performance_details::toggle(app);
                        }
                        Some(action) => crate::ui_status::activate(app, action),
                        _ => {}
                    }
                    return;
                }
            }

            // In split mode, switch focus first so tab drag/click works in non-focused panes.
            crate::mouse_selection::switch_focus_if_split(app, col, row, true);
            if app.handle_active_extension_view_mouse_down(col, row, mouse.modifiers) {
                app.active_pane = ActivePane::Editor;
                return;
            }

            // Click on tab bar?
            if let Some(tab_area) = app.tab_bar_area {
                if col >= tab_area.x
                    && col < tab_area.x + tab_area.width
                    && row >= tab_area.y
                    && row < tab_area.y + tab_area.height
                {
                    let hits = TabBarHits::new(app, tab_area);
                    let clicked_tab = hits.tab_at(col).map(|(index, _, _)| index);
                    if let Some(idx) = clicked_tab {
                        app.dragging_tab = Some(idx);
                        app.dragging_tab_source_group = Some(app.active_group);
                        app.dragging_explorer_path = None;
                        app.tab_drop_target_group = None;
                    }
                    let handled = handle_tab_hit(app, col, tab_area, &hits);
                    if clicked_tab.is_none() && !handled {
                        // Check if clicked "+" button
                        if TabBarHits::contains(hits.new_tab, col) {
                            app.new_file();
                            return;
                        }
                        // Double-click on empty tab bar space: new file
                        let now = std::time::Instant::now();
                        if now.duration_since(app.last_click_time).as_millis() < 400
                            && app.last_click_pos.1 == row
                        {
                            app.new_file();
                        }
                    }
                    app.last_click_time = std::time::Instant::now();
                    app.last_click_pos = (col, row);
                    return;
                }
            }

            // Click on settings sidebar/editor item
            if app.settings_editor_active {
                if app.settings_value_editing {
                    app.finish_numeric_setting_edit();
                }
                if app.settings_category_picker_area.is_some_and(|area| area.contains((col, row).into())) {
                    app.active_pane = ActivePane::Editor;
                    app.open_settings_category_dropdown();
                    return;
                }
                if app.settings_clear_search_area.is_some_and(|area| area.contains((col, row).into())) {
                    app.settings_editor_search.clear();
                    app.settings_editor_search_cursor = 0;
                    app.settings_editor_search_selection_anchor = None;
                    app.settings_editor_scroll = 0;
                    app.settings_focus_categories = false;
                    app.settings_focus_control = false;
                    app.active_pane = ActivePane::Editor;
                    app.ensure_settings_selection_visible();
                    return;
                }
                if app.settings_json_area.is_some_and(|area| area.contains((col, row).into())) {
                    app.execute_palette_command("open_settings_json");
                    return;
                }
                for (scope_idx, scope_area) in app.settings_scope_areas.iter().enumerate() {
                    if scope_area.is_some_and(|scope_area| {
                        col >= scope_area.x
                            && col < scope_area.x + scope_area.width
                            && row >= scope_area.y
                            && row < scope_area.y + scope_area.height
                    }) {
                        app.settings_scope = if scope_idx == 0 {
                            crate::settings::SettingsScope::Global
                        } else {
                            crate::settings::SettingsScope::Workspace
                        };
                        app.settings_editor_scroll = 0;
                        app.ensure_settings_selection_visible();
                        app.notify(format!("Editing {} settings", app.settings_scope.label()));
                        return;
                    }
                }
                if let Some(category_area) = app.settings_category_area {
                    if col >= category_area.x
                        && col < category_area.x + category_area.width
                        && row >= category_area.y
                        && row < category_area.y + category_area.height
                    {
                        app.active_pane = ActivePane::Editor;
                        if col == category_area.right().saturating_sub(1)
                            && app.settings_category_scrollbar().is_some()
                        {
                            app.dragging_settings_categories = true;
                            app.drag_settings_category_scrollbar(row);
                            return;
                        }
                        let visual_idx = app.settings_category_scroll + (row - category_area.y) as usize;
                        let category_rows = app.settings_category_visual_rows();
                        if let Some(&(idx, is_child, label)) = category_rows.get(visual_idx) {
                            if !is_child && !App::settings_sections(idx).is_empty()
                                && col <= category_area.x + 2
                            {
                                // A disclosure arrow changes the tree, not the
                                // current page or an in-progress global search.
                                app.settings_category_collapsed[idx] = !app.settings_category_collapsed[idx];
                                app.settings_category_scroll = app.settings_category_scroll.min(
                                    app.settings_category_visual_rows().len().saturating_sub(category_area.height as usize));
                                if idx == app.settings_category_selected {
                                    app.settings_focus_categories = true;
                                    app.settings_focus_control = false;
                                    app.reveal_settings_category();
                                }
                                return;
                            }
                            app.settings_focus_categories = true;
                            app.settings_focus_control = false;
                            if !is_child && !App::settings_sections(idx).is_empty() {
                                app.settings_category_collapsed[idx] = false;
                            }
                            app.select_settings_page(idx, is_child.then_some(label));
                        }
                        return;
                    }
                }
                if let Some(settings_area) = app.settings_sidebar_area {
                    if col >= settings_area.x && col < settings_area.x + settings_area.width
                        && row >= settings_area.y && row < settings_area.y + settings_area.height
                    {
                        app.active_pane = ActivePane::Editor;
                        if let Some((track_x, track_h, thumb_h, max_scroll)) =
                            settings_scrollbar_metrics(app, settings_area)
                        {
                            if col == track_x {
                                let travel = track_h.saturating_sub(thumb_h);
                                let offset = app.settings_editor_scroll.min(max_scroll);
                                let top = if travel == 0 { 0 } else { offset * travel / max_scroll };
                                let rel_y = row.saturating_sub(settings_area.y) as usize;
                                let offset = if (top..top + thumb_h).contains(&rel_y) { offset }
                                    else if travel == 0 { 0 } else {
                                        (rel_y.saturating_sub(thumb_h / 2).min(travel) as f64
                                            * max_scroll as f64 / travel as f64).round() as usize
                                    };
                                app.clear_drag_states();
                                app.settings_editor_scroll = offset;
                                app.settings_scrollbar_drag = Some(crate::settings_layout::Drag { row, offset });
                                return;
                            }
                        }
                        app.settings_focus_categories = false;
                        let layout = app.settings_line_layout(settings_area.width.saturating_sub(3) as usize);
                        let scroll = app.settings_editor_scroll.min(layout.max_scroll(settings_area.height as usize));
                        let visual_row = scroll + (row - settings_area.y) as usize;
                        let items = app.settings_items();
                        if let Some(item) = layout.row_at(visual_row) {
                            let orig_idx = item.index;
                            let (_, _, id) = &items[orig_idx];
                            let desc_lines = &item.lines;
                            let item_height = item.height;
                            let running_rows = item.start;
                            let kind = App::setting_field_kind(id);
                            let relative_row = visual_row - running_rows;
                            let gap = if kind == "heading" { 0 } else {
                                App::setting_row_gap(settings_area.width.saturating_sub(3) as usize)
                            };
                            // Spacing and section headings are not controls.
                            if kind == "heading" || relative_row >= item_height.saturating_sub(gap) {
                                return;
                            }
                            app.settings_selected = orig_idx;
                            app.settings_focus_control = true;
                            if relative_row == 0 && !id.starts_with("kb_") && settings_area.width >= 32
                                && col >= settings_area.right().saturating_sub(8)
                                && col < settings_area.right().saturating_sub(1)
                                && matches!(app.setting_source_label(id), "Global" | "Workspace")
                            {
                                app.reset_setting_in_selected_scope(id);
                                return;
                            }
                            if id.starts_with("kb_") {
                                let control_row = running_rows + item.control;
                                if visual_row == control_row {
                                    let reset_label = "[Reset]";
                                    let unassign_label = "[Unassign]";
                                    let reset_w = UnicodeWidthStr::width(reset_label) as u16;
                                    let unassign_w = UnicodeWidthStr::width(unassign_label) as u16;
                                    let buttons_w = reset_w + 1 + unassign_w;
                                    if settings_area.width > buttons_w + 1 {
                                        let buttons_x = settings_area
                                            .x
                                            + settings_area.width.saturating_sub(buttons_w);
                                        let unassign_x = buttons_x + reset_w + 1;
                                        if col >= buttons_x && col < buttons_x + reset_w {
                                            if let Some(kb_idx) = id
                                                .strip_prefix("kb_")
                                                .and_then(|v| v.parse::<usize>().ok())
                                            {
                                                app.reset_keybinding_in_selected_scope(kb_idx);
                                            }
                                            app.ensure_settings_selection_visible();
                                            return;
                                        }
                                        if col >= unassign_x && col < unassign_x + unassign_w {
                                            if let Some(kb_idx) = id
                                                .strip_prefix("kb_")
                                                .and_then(|v| v.parse::<usize>().ok())
                                            {
                                                app.flush_live_settings();
                                                let scoped_before = app.selected_scope_settings();
                                                app.apply_setting_value(id, &scoped_before);
                                                let mut unassign_msg: Option<String> = None;
                                                if let Some(kb) = app.keybindings.get_mut(kb_idx) {
                                                    if kb.code != KeyCode::Null
                                                        || !kb.modifiers.is_empty()
                                                    {
                                                        kb.modifiers = KeyModifiers::empty();
                                                        kb.code = KeyCode::Null;
                                                        unassign_msg =
                                                            Some(format!("Unassigned {}", kb.label));
                                                    }
                                                }
                                                if let Some(msg) = unassign_msg {
                                                    app.notify(msg);
                                                    app.rebuild_keybinding_settings_cache();
                                                    app.persist_scoped_setting_change(id, &scoped_before);
                                                } else {
                                                    app.apply_effective_settings();
                                                }
                                            }
                                            app.ensure_settings_selection_visible();
                                            return;
                                        }
                                    }
                                }
                            }
                            if kind == "exclusions" {
                                let offset = relative_row.saturating_sub(item.control);
                                if relative_row >= item.control {
                                    let patterns = serde_json::from_str::<crate::settings::ExcludePatterns>(&items[orig_idx].1).unwrap_or_default();
                                    if offset <= patterns.len() {
                                        let pattern = patterns.keys().nth(offset).cloned();
                                        app.open_exclusion_settings(id, pattern, offset == patterns.len());
                                    }
                                }
                                return;
                            }
                            let control_width = match kind {
                                "number" => 20,
                                "language-list" => 60,
                                "dropdown" => 40,
                                "action" => UnicodeWidthStr::width(items[orig_idx].1.trim()) as u16 + 2,
                                _ => settings_area.width.saturating_sub(3),
                            }.min(settings_area.width.saturating_sub(3));
                            let on_control = if kind == "checkbox" {
                                relative_row > 0 && relative_row < desc_lines.len()
                                    && col >= settings_area.x + 2
                                    && col < settings_area.x + 6 + UnicodeWidthStr::width(desc_lines[relative_row].as_str()) as u16
                            } else {
                                relative_row == item.control
                                    && col >= settings_area.x + 2
                                    && col < settings_area.x + 2 + control_width
                            };
                            if !on_control {
                                return;
                            }
                            if matches!(kind, "number" | "language-list") {
                                let current = items
                                    .get(orig_idx)
                                    .map(|(_, value, _)| value.clone())
                                    .unwrap_or_default();
                                app.begin_numeric_setting_edit(id, &current);
                                // The first click activates the field; keep it in
                                // the shared gesture so a second click selects its value.
                                app.text_input_click_id = Some(crate::text_input::TextInputId::SettingsValue);
                                app.click_count = 1;
                                app.last_click_time = std::time::Instant::now();
                                app.last_click_pos = (col, row);
                            } else if kind == "dropdown" {
                                let current = items
                                    .get(orig_idx)
                                    .map(|(_, value, _)| value.clone())
                                    .unwrap_or_default();
                                app.open_settings_dropdown(id, &current);
                            } else {
                                app.toggle_setting(id);
                            }
                            app.ensure_settings_selection_visible();
                        }
                        return;
                    }
                }
            } else if app.sidebar_view == crate::app::SidebarView::Settings {
                if let Some(settings_area) = app.settings_sidebar_area {
                    if col >= settings_area.x && col < settings_area.x + settings_area.width
                        && row >= settings_area.y && row < settings_area.y + settings_area.height
                    {
                        let items = app.settings_items();
                        let visible_items = settings_area.height as usize;
                        let scroll_offset = if app.settings_selected >= visible_items {
                            app.settings_selected - visible_items + 1
                        } else {
                            0
                        };
                        let clicked_idx = scroll_offset + (row - settings_area.y) as usize;
                        if let Some((_, _, id)) = items.get(clicked_idx) {
                            app.settings_selected = clicked_idx;
                            app.toggle_setting(id);
                        }
                        return;
                    }
                }
            }

            // Resolve the painted scrollbar before the resize seam and file rows.
            if app.show_explorer && app.sidebar_view == crate::app::SidebarView::Explorer
                && let Some(area) = app.explorer_area
                && let Some(bar) = [crate::ui_chrome::ExplorerSection::OpenEditors, crate::ui_chrome::ExplorerSection::Files,
                    crate::ui_chrome::ExplorerSection::FilesHorizontal]
                    .into_iter()
                    .filter_map(|section| crate::ui_chrome::explorer_scrollbar(app, area, section))
                    .find(|bar| bar.area.contains((col, row).into()))
            {
                app.clear_drag_states();
                app.drag_start_pos = None;
                app.active_pane = ActivePane::Explorer;
                app.hovered_explorer = None;
                let position = bar.coordinate(col, row);
                let relative_row = position - bar.coordinate(bar.area.x, bar.area.y);
                let on_thumb = (bar.thumb_top..bar.thumb_top + bar.thumb_height).contains(&relative_row);
                let grab = if on_thumb { relative_row - bar.thumb_top } else { bar.thumb_height / 2 };
                let offset = if on_thumb { bar.offset } else { bar.offset_at(position, grab).unwrap_or(bar.offset) };
                bar.section.scroll_to(app, offset);
                app.explorer_scrollbar_drag = Some((bar.section, position, offset));
                return;
            }

            // Keep Source Control header actions ahead of the resize
            // seam, including a sidebar reduced to a single terminal cell.
            if app.show_explorer && app.sidebar_view == crate::app::SidebarView::SourceControl
            {
                if let Some(button) = app.repositories.reopen_button.filter(|area| area.contains((col, row).into())) {
                    app.active_pane = ActivePane::Explorer;
                    app.source_control_input_mode = false;
                    app.context_menu_pos = (button.x, button.bottom());
                    app.open_repository_picker();
                    crate::mouse_overlay::capture_context_menu_button(app, button);
                    return;
                }
                if let Some(button) = app.source_control_actions_area.filter(|area| area.contains((col, row).into())) {
                    app.active_pane = ActivePane::Explorer;
                    app.source_control_input_mode = false;
                    app.open_source_control_action_menu(button.x, button.bottom());
                    crate::mouse_overlay::capture_context_menu_button(app, button);
                    return;
                }
            }

            // Click on explorer resize border?
            if app.show_explorer {
                if let Some(exp_area) = app.explorer_area {
                    if explorer_resize_hit(exp_area, app.sidebar_right, col, row) {
                        app.resizing_explorer = true;
                        return;
                    }
                }
            }

            // Internal side-by-side diff divider. It is independent from the
            // editor-group split tree and resizes only the two diff panes.
            if let Some(divider) = app.diff_divider_area {
                let hit = col >= divider.x.saturating_sub(1)
                    && col <= divider.x.saturating_add(divider.width)
                    && row >= divider.y
                    && row < divider.bottom();
                if hit {
                    let now = std::time::Instant::now();
                    let is_double_click = now
                        .duration_since(app.last_click_time)
                        .as_millis()
                        < 400
                        && app.last_click_pos == (col, row);
                    if is_double_click {
                        app.diff_split_ratio = 0.5;
                    } else {
                        app.resizing_diff_split = true;
                    }
                    app.last_click_time = now;
                    app.last_click_pos = (col, row);
                    return;
                }
            }

            // Click on split divider to start resize?
            if app.split_active() && !app.split_dividers.is_empty() {
                let over_scrollbar = app.scrollbar_area.is_some_and(|area| {
                    col >= area.x
                        && col < area.x + area.width
                        && row >= area.y
                        && row < area.y + area.height
                }) || app.h_scrollbar_area.is_some_and(|area| {
                    col >= area.x
                        && col < area.x + area.width
                        && row >= area.y
                        && row < area.y + area.height
                }) || app.groups.iter().any(|group| {
                    group.scrollbar_area.is_some_and(|area| {
                        col >= area.x
                            && col < area.x + area.width
                            && row >= area.y
                            && row < area.y + area.height
                    }) || group.h_scrollbar_area.is_some_and(|area| {
                        col >= area.x
                            && col < area.x + area.width
                            && row >= area.y
                            && row < area.y + area.height
                    })
                });
                if !over_scrollbar {
                    for (div_rect, path, child_idx, direction, parent_area) in &app.split_dividers {
                        // Expand hit area by 1 pixel on each side for easier grabbing
                        let hit = match direction {
                            crate::app::SplitDirection::Horizontal => {
                                col >= div_rect.x.saturating_sub(1) && col <= div_rect.x + div_rect.width
                                && row >= div_rect.y && row < div_rect.y + div_rect.height
                            }
                            crate::app::SplitDirection::Vertical => {
                                row >= div_rect.y.saturating_sub(1) && row <= div_rect.y + div_rect.height
                                && col >= div_rect.x && col < div_rect.x + div_rect.width
                            }
                        };
                        if hit {
                            let now = std::time::Instant::now();
                            let is_double_click = now.duration_since(app.last_click_time).as_millis() < 400
                                && app.last_click_pos == (col, row);
                            if is_double_click {
                                // Double-click on divider: reset ratios to equal
                                if let Some(ref mut tree) = app.split_tree {
                                    tree.reset_ratios_at(path);
                                }
                                app.last_click_time = std::time::Instant::now();
                                app.last_click_pos = (col, row);
                                return;
                            }
                            let total_size = match direction {
                                crate::app::SplitDirection::Horizontal => parent_area.width,
                                crate::app::SplitDirection::Vertical => parent_area.height,
                            };
                            app.dragging_divider = Some((path.clone(), *child_idx, *direction, total_size));
                            app.resizing_split = true;
                            app.last_click_time = std::time::Instant::now();
                            app.last_click_pos = (col, row);
                            return;
                        }
                    }
                }
            }

            // Click on sidebar area?
            if let Some(exp_area) = app.explorer_area {
                if col >= exp_area.x
                    && col < exp_area.x + exp_area.width
                    && row >= exp_area.y
                    && row < exp_area.y + exp_area.height
                {
                    // Handle search panel clicks
                    if app.sidebar_view == crate::app::SidebarView::Search {
                        app.active_pane = ActivePane::Explorer;
                        let inner_y = exp_area.y;
                        let inner_x = crate::ui_search_panel::search_form_left(exp_area);
                        macro_rules! focus_search_field {
                            ($field:expr, $label:expr) => {{
                                app.global_search_active = true;
                                app.search_focus_field = $field;
                                let label_width = UnicodeWidthStr::width($label) as u16;
                                let text_start = inner_x.saturating_add(label_width);
                                app.global_search_set_field_cursor_char_index($field, col.saturating_sub(text_start) as usize);
                            }};
                        }
                        if crate::ui_search_controls::handle_click(
                            app, crate::ui_search_controls::SearchScope::Workspace, col, row,
                        ) {
                            return;
                        }
                        let hidden_rows = u16::from(!app.global_search_replace_visible);
                        if row == inner_y + 1 {
                            focus_search_field!(0, " ");
                            return;
                        }
                        if app.global_search_replace_visible && row == inner_y + 2 {
                            focus_search_field!(1, " ");
                            return;
                        }
                        if row == inner_y + 4 - hidden_rows {
                            focus_search_field!(2, " ");
                            return;
                        }
                        if row == inner_y + 6 - hidden_rows {
                            focus_search_field!(3, " ");
                            return;
                        }
                        let results_start =
                            crate::ui_search_panel::search_results_start_y(app, exp_area);
                        if row >= results_start && !app.global_search_results.is_empty() {
                            let rel_row = (row - results_start) as usize;
                            let max_visible = exp_area
                                .y
                                .saturating_add(exp_area.height)
                                .saturating_sub(results_start)
                                as usize;
                            if max_visible == 0 {
                                return;
                            }

                            use crate::search_results::SearchRow;
                            let max_scroll = app.global_search_results.row_count().saturating_sub(max_visible);
                            let scroll = app.global_search_scroll.min(max_scroll);
                            let clicked_row = scroll + rel_row;
                            if let Some(hit) = app.global_search_results.row_at(clicked_row) {
                                match hit {
                                    SearchRow::Header(idx) => {
                                        let path = app.global_search_results[idx].file_path.clone();
                                        app.toggle_global_search_file_collapsed(&path);
                                    }
                                    SearchRow::Result(idx) => {
                                        app.global_search_selected = idx;
                                        app.open_global_search_result();
                                    }
                                }
                            }
                        }
                        return;
                    }

                    // Handle source control panel clicks
                    if app.sidebar_view == crate::app::SidebarView::SourceControl {
                        app.active_pane = ActivePane::Explorer;
                        if app.source_control_toggle_area.is_some_and(|area| area.contains((col, row).into())) {
                            app.toggle_workspace_source_control();
                            return;
                        }
                        if app.repositories.parent_button.is_some_and(|area| area.contains((col, row).into())) {
                            if app.repositories.parent.is_some() { app.open_parent_repository(); }
                            else { app.reopen_source_control_repositories(); }
                            return;
                        }
                        if let Some((_, root)) = app.repositories.closes.iter().find(|(area, _)| area.contains((col, row).into())).cloned() {
                            app.close_source_control_repository(&root);
                            return;
                        }
                        if let Some((_, root)) = app.repositories.headers.iter().find(|(area, _)| area.contains((col, row).into())).cloned() {
                            app.select_source_control_repository(&root);
                            return;
                        }
                        if app.source_control_repository_state
                            != crate::app::SourceControlRepositoryState::Available
                        {
                            if let Some((_, action)) = app.repositories.recovery_buttons.iter()
                                .find(|(area, _)| area.contains((col, row).into())).copied()
                            {
                                action.run(app);
                                return;
                            }
                            if app.source_control_repository_state
                                == crate::app::SourceControlRepositoryState::Missing
                                && app.source_control_initialize_area.is_some_and(|area| {
                                    col >= area.x
                                        && col < area.x.saturating_add(area.width)
                                        && row >= area.y
                                        && row < area.y.saturating_add(area.height)
                                })
                            {
                                app.git_initialize_repository();
                            }
                            return;
                        }
                        if app.repositories.discovered && !app.repositories.expanded { return; }
                        let hits = |area: Rect| {
                            col >= area.x
                                && col < area.x.saturating_add(area.width)
                                && row >= area.y
                                && row < area.y.saturating_add(area.height)
                        };
                        let commit = app.source_control_commit_area.is_some_and(hits);
                        let amend = app.source_control_amend_area.is_some_and(hits);
                        if commit || amend {
                            if app.bg_git_action_running || app.bg_git_remote_running {
                                return;
                            }
                            app.sc_amend_mode = amend;
                            if amend {
                                if app.source_control_commit_msg.is_empty() {
                                    if let Some(last) = app.git_log.first() {
                                        app.source_control_commit_msg = last.message.clone();
                                        app.source_control_commit_cursor = app.source_control_commit_msg.len();
                                        app.source_control_commit_selection_anchor = None;
                                    }
                                }
                                app.git_amend_commit();
                            } else {
                                app.git_commit();
                            }
                            return;
                        }
                        if app.source_control_stage_all_area.is_some_and(hits) || app.source_control_untracked_stage_area.is_some_and(hits) {
                            let group = if app.source_control_untracked_stage_area.is_some_and(hits) {
                                crate::source_control_rows::Group::Untracked
                            } else { crate::source_control_rows::Group::Unstaged };
                            app.git_stage_changes(app.source_control_group_changes(group));
                            return;
                        }
                        if app.source_control_unstage_all_area.is_some_and(hits) {
                            app.git_unstage_changes(app.source_control_group_changes(crate::source_control_rows::Group::Staged));
                            return;
                        }
                        if app.source_control_discard_all_area.is_some_and(hits) || app.source_control_untracked_discard_area.is_some_and(hits) {
                            let group = if app.source_control_untracked_discard_area.is_some_and(hits) {
                                crate::source_control_rows::Group::Untracked
                            } else { crate::source_control_rows::Group::Unstaged };
                            app.request_source_control_discard(app.source_control_group_changes(group), false);
                            return;
                        }
                        if crate::ui_source_control::handle_file_action_click(app, col, row, mouse.modifiers) { return; }
                        if let Some(exp_area) = app.source_control_detail_area() {
                            let rel_y = row.saturating_sub(exp_area.y);
                            let total_h = exp_area.height;
                            let (changes_h, graph_h) = crate::ui_source_control::source_control_split_heights(
                                total_h,
                                app.sc_graph_collapsed,
                                app.sc_graph_height,
                            );

                            if graph_h > 0
                                && !app.sc_graph_collapsed
                                && rel_y == changes_h
                                && col > exp_area.x.saturating_add(3)
                            {
                                app.sc_graph_click_start = Some((col, row));
                                return;
                            }

                            if rel_y < changes_h {
                                // Inside CHANGES section
                                // Row 0: header action buttons
                                // Rows 1-4: commit message input box
                                // Row 5: Commit button
                                // Rows 6+: file list
                                if rel_y == 0 {
                                    return;
                                } else if (1..=4).contains(&rel_y) {
                                    app.sc_graph_focused = false;
                                    app.source_control_input_mode = true;
                                    let visible_lines = 2usize;
                                    app.prepare_commit_input(exp_area.width.saturating_sub(2) as usize, visible_lines);
                                    let scroll_line = app.commit_input.scroll;
                                    let view_line = if rel_y <= 1 {
                                        0usize
                                    } else if rel_y >= 4 {
                                        visible_lines.saturating_sub(1)
                                    } else {
                                        (rel_y - 2) as usize
                                    };
                                    let line = scroll_line + view_line;
                                    let col_in_line = col.saturating_sub(exp_area.x.saturating_add(1)) as usize;
                                    app.source_control_commit_cursor = app.commit_input.byte_at_column(line, col_in_line);
                                    app.source_control_commit_selection_anchor = None;
                                    app.break_commit_input_group();
                                } else if rel_y == 5 {
                                    // The gap between actions is not a button.
                                    return;
                                } else if rel_y >= 6 {
                                    let list_row = (rel_y - 6) as usize;
                                    let layout = app.source_control_layout();

                                    // Read-only pass: determine what was clicked
                                    enum ClickAction {
                                        ToggleGroup(crate::source_control_rows::Group),
                                        SelectAndDiff(usize),
                                    }
                                    let action = layout.rows(app.source_control_scroll).nth(list_row).map(|row| {
                                        match row {
                                            crate::source_control_rows::Row::Section(group) => ClickAction::ToggleGroup(group),
                                            crate::source_control_rows::Row::File(i) => {
                                                ClickAction::SelectAndDiff(i)
                                            }
                                        }
                                    });

                                    // Apply the action
                                    app.sc_graph_focused = false;
                                    let ctrl = mouse.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
                                    let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
                                    match action {
                                        Some(ClickAction::ToggleGroup(group)) => app.set_source_control_group_collapsed(group, !app.source_control_group_collapsed(group)),
                                        Some(ClickAction::SelectAndDiff(i))
                                            if ctrl || shift => { app.select_source_control_file(i, ctrl, shift); }
                                        Some(ClickAction::SelectAndDiff(i)) => {
                                            app.select_source_control_file(i, false, false);
                                            app.open_source_control_diff(i);
                                        }
                                        None => {}
                                    }
                                }
                            } else if graph_h > 0 {
                                // Inside GRAPH section
                                let graph_rel_y = rel_y - changes_h;
                                if graph_rel_y == 0 {
                                    // GRAPH header click: collapse/expand on mouse release.
                                    app.sc_graph_click_start = Some((col, row));
                                } else if !app.sc_graph_collapsed {
                                    let commit_idx = app.git_log_scroll + (graph_rel_y - 1) as usize;
                                    if commit_idx == app.git_log.len() && app.git_history.error.is_some() {
                                        app.refresh_git_log_async();
                                        return;
                                    }
                                    if commit_idx < app.git_log.len() {
                                        app.git_log_selected = commit_idx;
                                        app.open_source_control_commit_details(commit_idx);
                                    }
                                }
                            }
                        }
                        return;
                    }

                    // Handle extensions sidebar clicks
                    if app.sidebar_view == crate::app::SidebarView::Extensions {
                        crate::ui_extensions::handle_click(app, col, row);
                        return;
                    }

                    // Handle outline sidebar clicks
                    if app.sidebar_view == crate::app::SidebarView::Outline {
                        app.active_pane = ActivePane::Explorer;
                        if row > exp_area.y {
                            let clicked_idx = app.outline_scroll + (row - exp_area.y - 1) as usize;
                            if clicked_idx < app.outline_items.len() {
                                app.outline_selected = clicked_idx;
                                let line = app.outline_items[clicked_idx].line;
                                app.push_nav_history();
                                if let Some(ed) = app.editor_mut() {
                                    ed.clear_selection();
                                    ed.set_cursor(line, 0);
                                    ed.desired_col = None;
                                    ed.selection_anchor = None;
                                    ed.ensure_visible();
                                }
                            }
                        }
                        return;
                    }

                    // Explorer view clicks
                    app.active_pane = ActivePane::Explorer;

                    // Refresh button in explorer header
                    if row == exp_area.y {
                        let header_label_w = UnicodeWidthStr::width(" EXPLORER") as u16;
                        let collapse_w = UnicodeWidthStr::width(" ⤒ ") as u16;
                        let refresh_w = UnicodeWidthStr::width(" ↻ ") as u16;
                        let fill_w = exp_area
                            .width
                            .saturating_sub(header_label_w.saturating_add(collapse_w).saturating_add(refresh_w));
                        let collapse_x = exp_area.x.saturating_add(header_label_w).saturating_add(fill_w);
                        let refresh_x = collapse_x.saturating_add(collapse_w);
                        if col >= refresh_x && col < refresh_x.saturating_add(refresh_w) {
                            app.refresh_explorer_async();
                            app.notify("Explorer refreshed");
                            return;
                        }
                        if col >= collapse_x && col < collapse_x.saturating_add(collapse_w) {
                            app.collapse_all_explorer_async();
                            if app.explorer.entries.is_empty() {
                                app.explorer_multi_selected.clear();
                                app.explorer_selection_anchor = None;
                            } else {
                                app.set_explorer_single_selection(app.explorer.selected);
                            }
                            app.notify("Explorer collapsed");
                            return;
                        }
                    }

                    let (oe_area, _) = crate::ui_chrome::explorer_sections(app, exp_area);
                    let oe_count = oe_area.height.saturating_sub(1) as usize;

                    // Click on Open Editors header -> toggle
                    if !oe_area.is_empty() && row == oe_area.y {
                        app.show_open_editors = !app.show_open_editors;
                        app.flush_live_settings();
                        return;
                    }

                    // Click on Open Editors entry -> switch tab
                    let oe_list_area = crate::ui_chrome::open_editors_list_area(app, exp_area);
                    if app.show_open_editors && oe_list_area.contains((col, row).into()) {
                        let tab_idx = crate::ui_chrome::open_editors_offset(app, oe_count)
                            + (row - oe_area.y - 1) as usize;
                        if tab_idx < app.tabs.len() {
                            // VS Code places the close affordance at the row's right edge.
                            if open_editor_close_hit(oe_list_area, col) {
                                app.close_tab(tab_idx);
                            } else {
                                app.sync_duplicate_file_tabs();
                                app.active_tab = tab_idx;
                                app.touch_tab_mru(tab_idx);
                                app.refresh_git_line_status_async();
                            }
                        }
                        return;
                    }

                    let tree_area = crate::ui_chrome::explorer_tree_content_area(app, exp_area);
                    let tree_start = tree_area.y;
                    if !tree_area.contains((col, row).into()) {
                        return;
                    }

                    let inner_row = (row - tree_start) as usize;
                    let visible_height = tree_area.height as usize;
                    let Some(clicked_idx) = app.explorer.entry_at_row(inner_row, visible_height) else {
                        return;
                    };

                    // Multi-click detection for explorer
                    let now = std::time::Instant::now();
                    if now.duration_since(app.last_click_time).as_millis() < 400
                        && app.last_click_pos == (col, row)
                    {
                        app.click_count = (app.click_count + 1).min(3);
                    } else {
                        app.click_count = 1;
                    }
                    app.last_click_time = now;
                    app.last_click_pos = (col, row);

                    if clicked_idx < app.explorer.entries.len() {
                        let ctrl = mouse.modifiers.contains(KeyModifiers::CONTROL);
                        let shift = mouse.modifiers.contains(KeyModifiers::SHIFT);
                        if shift {
                            app.range_select_explorer(clicked_idx, ctrl);
                            app.dragging_explorer_path = Some(app.explorer.entries[clicked_idx].path.clone());
                            return;
                        }
                        if ctrl {
                            app.toggle_explorer_selection(clicked_idx);
                            app.dragging_explorer_path = app.explorer_multi_selected.contains(&app.explorer.entries[clicked_idx].path)
                                .then(|| app.explorer.entries[clicked_idx].path.clone());
                            return;
                        }

                        // Retain the selected file set when starting a drag.
                        // Chat accepts the entire selection as file context.
                        if app.explorer_multi_selected.contains(&app.explorer.entries[clicked_idx].path) {
                            app.explorer.selected = clicked_idx;
                        } else {
                            app.set_explorer_single_selection(clicked_idx);
                        }
                        app.dragging_explorer_path = None;
                        let is_dir = app.explorer.entries[clicked_idx].is_dir;
                        if is_dir {
                            // Directories expand/collapse on single click
                            let _ = app.toggle_or_open_explorer_async();
                            app.dragging_explorer_path =
                                Some(app.explorer.entries[clicked_idx].path.clone());
                        } else if app.click_count >= 2 {
                            // Double click: open file normally (pins the tab)
                            if let Some(path) = app.toggle_or_open_explorer_async() {
                                app.open_file(&path);
                                app.dragging_explorer_path = Some(path);
                            }
                        } else {
                            // Browse in one preview tab; clicking it again keeps it open.
                            let path = app.explorer.entries[clicked_idx].path.clone();
                            app.open_file_preview(&path);
                            // Previewing a selection keeps Delete, F2, and arrow
                            // keys in the tree until the user focuses the editor.
                            app.active_pane = ActivePane::Explorer;
                            app.dragging_explorer_path =
                                Some(app.explorer.entries[clicked_idx].path.clone());
                        }
                    } else {
                        app.clear_explorer_selection();
                        app.dragging_explorer_path = None;
                    }
                    return;
                }
            }

            // Editor area: gutter, minimap, code, scrollbars, merge-conflict buttons
            crate::mouse_selection::handle_editor_area_left_click(app, col, row, mouse.modifiers);
        }
        MouseEventKind::Down(MouseButton::Middle) => {
            if app.show_panel && app.panel_area.is_some_and(|area| area.contains((col, row).into())) {
                crate::ui_panels::capture_pointer(app, MouseButton::Middle);
                return;
            }
            crate::mouse_selection::switch_focus_if_split(app, col, row, false);
            if app.handle_active_extension_view_middle_button("down", col, row, mouse.modifiers) {
                app.active_pane = ActivePane::Editor;
                return;
            }
            crate::mouse_selection::handle_middle_button_down(app, col, row);
        }
        MouseEventKind::Down(MouseButton::Right) => {
            if app.show_panel && app.panel_area.is_some_and(|area| area.contains((col, row).into())) {
                crate::ui_panels::capture_pointer(app, MouseButton::Right);
                return;
            }
            crate::mouse_selection::switch_focus_if_split(app, col, row, false);
            if app.handle_active_extension_view_context_menu(col, row, mouse.modifiers) {
                app.active_pane = ActivePane::Editor;
                return;
            }
            crate::mouse_selection::handle_right_button_down(app, col, row);
        }
        MouseEventKind::ScrollUp => {
            crate::mouse_scroll::handle_scroll_up(app, col, row, mouse.modifiers);
        }
        MouseEventKind::ScrollDown => {
            crate::mouse_scroll::handle_scroll_down(app, col, row, mouse.modifiers);
        }
        MouseEventKind::ScrollLeft => {
            crate::mouse_scroll::handle_scroll_left(app, col, row, mouse.modifiers);
        }
        MouseEventKind::ScrollRight => {
            crate::mouse_scroll::handle_scroll_right(app, col, row, mouse.modifiers);
        }
        // No double-click in this crossterm version
        MouseEventKind::Drag(MouseButton::Left) => {
            crate::mouse_drag::handle_left_drag(app, col, row, prev_mouse_col, prev_mouse_row, mouse.modifiers);
        }
        MouseEventKind::Drag(MouseButton::Middle) => {
            if app.handle_active_extension_view_middle_button("drag", col, row, mouse.modifiers) { return; }
            // Middle drag: column/block selection
            crate::mouse_drag::handle_block_drag(app, col, row);
        }
        MouseEventKind::Up(MouseButton::Middle) => {
            if app.handle_active_extension_view_middle_button("up", col, row, mouse.modifiers) { return; }
                app.block_drag_cache = None;
            app.middle_drag_start = None;
        }
        MouseEventKind::Up(MouseButton::Left) => {
            crate::mouse_drag::handle_left_up(app, col, row, mouse.modifiers);
        }
        _ => {}
    }
}


pub(crate) fn settings_scrollbar_metrics(
    app: &App,
    settings_area: ratatui::layout::Rect,
) -> Option<(u16, usize, usize, usize)> {
    if settings_area.is_empty() { return None; }
    app.settings_line_layout(settings_area.width.saturating_sub(3) as usize).scrollbar(settings_area)
}

#[cfg(test)]
fn tab_bar_right_indicator_start(app: &App, tab_area: ratatui::layout::Rect) -> Option<u16> {
    let right = TabBarHits::new(app, tab_area).right;
    (!right.is_empty()).then_some(right.x)
}

pub(crate) fn ensure_group_active_tab_visible(group: &mut crate::app::EditorGroup) {
    if let Some(scroll) = group.tab_strip_scroll {
        if group.tabs.get(group.active_tab).is_some_and(|tab| tab.editor.navigation_id == scroll.anchor) { return; }
        group.tab_strip_scroll = None;
    }
    if group.tabs.is_empty() {
        group.tab_scroll_offset = 0;
        return;
    }
    if group.active_tab < group.tab_scroll_offset {
        group.tab_scroll_offset = group.active_tab;
    }
    let area_width = group.tab_bar_area.map_or(0, |a| a.width as usize);
    let approx_tab_width = 14usize;
    let visible_tabs = if area_width > 0 {
        (area_width.saturating_sub(if group.tab_scroll_offset > 0 { 2 } else { 0 }) / approx_tab_width)
            .max(1)
    } else {
        8
    };
    if group.active_tab >= group.tab_scroll_offset + visible_tabs {
        group.tab_scroll_offset = group.active_tab.saturating_sub(visible_tabs - 1);
    }
}

fn reconcile_dragging_tab_after_close(app: &mut App, closed_idx: usize) {
    if let Some(drag_idx) = app.dragging_tab {
        if drag_idx == closed_idx {
            app.dragging_tab = None;
            app.dragging_tab_source_group = None;
            app.tab_drop_zone = None;
            app.tab_drop_target_group = None;
        } else if drag_idx > closed_idx {
            app.dragging_tab = Some(drag_idx - 1);
        }
    }
}

#[cfg(test)]
pub(crate) fn handle_tab_click(app: &mut App, col: u16, tab_area: ratatui::layout::Rect) -> bool {
    let hits = TabBarHits::new(app, tab_area);
    handle_tab_hit(app, col, tab_area, &hits)
}

fn handle_tab_hit(app: &mut App, col: u16, tab_area: ratatui::layout::Rect, hits: &TabBarHits) -> bool {
    if crate::ui_chrome_tabs::strip_scroll_position(app, app.active_group).is_some() {
        let direction = if TabBarHits::contains(hits.left, col) { Some(-1) }
            else if TabBarHits::contains(hits.right, col) { Some(1) } else { None };
        if let Some(direction) = direction {
            crate::ui_chrome_tabs::scroll_strip_by(app, tab_area, app.active_group, direction * 3);
            return true;
        }
    }
    if TabBarHits::contains(hits.left, col) {
        app.tab_scroll_offset = app.tab_scroll_offset.saturating_sub(1);
        app.tab_scroll_manual_anchor = Some(app.active_tab);
        return true;
    }
    if TabBarHits::contains(hits.right, col) {
        app.tab_scroll_offset = (app.tab_scroll_offset + 1).min(app.max_tab_scroll_offset());
        app.tab_scroll_manual_anchor = Some(app.active_tab);
        return true;
    }
    let Some((index, rect, close)) = hits.tab_at(col) else { return false; };
    app.tab_scroll_manual_anchor = None;
    app.tab_strip_scroll = None;
    if TabBarHits::contains(close, col) {
        reconcile_dragging_tab_after_close(app, index);
        app.close_tab(index);
        app.ensure_active_tab_visible();
    } else {
        let double_click = std::time::Instant::now()
            .duration_since(app.last_click_time).as_millis() < 400
            && app.last_click_pos.1 == tab_area.y
            && TabBarHits::contains(rect, app.last_click_pos.0)
            && !TabBarHits::contains(close, app.last_click_pos.0)
            && app.active_tab == index;
        app.sync_duplicate_file_tabs();
        app.clear_transient_editor_state();
        app.active_tab = index;
        app.touch_tab_mru(index);
        if double_click {
            app.pin_preview_tab();
        }
        app.active_pane = ActivePane::Editor;
        app.validate_diff_state_for_active_tab();
        app.ensure_active_tab_visible();
        app.refresh_git_line_status_async();
    }
    true
}

pub(crate) fn find_tab_at_col(app: &App, col: u16, tab_area: ratatui::layout::Rect) -> Option<usize> {
    TabBarHits::new(app, tab_area).tab_at(col).map(|(index, _, _)| index)
}

#[cfg(test)]
#[path = "mouse_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "mouse_context_geometry_tests.rs"]
mod context_geometry_tests;

#[cfg(test)]
#[path = "mouse_explorer_scrollbar_tests.rs"]
mod explorer_scrollbar_tests;

#[cfg(test)]
#[path = "mouse_tab_context_tests.rs"]
mod tab_context_tests;
