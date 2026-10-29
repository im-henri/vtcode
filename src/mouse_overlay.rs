use crossterm::event;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use crate::app::App;
use crate::helpers::len_as_u16;
use crate::input;
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthStr;

const SETTINGS_PANEL_WIDTH: u16 = 55;

fn handle_folder_browser_click(app: &mut App, col: u16, row: u16) -> bool {
    app.folder_browser_click(col, row);
    true
}

/// Handle mouse events that target extension overlays.
/// Returns `true` if the event was consumed by an overlay.
pub(crate) fn handle_overlay_mouse(app: &mut App, kind: MouseEventKind, col: u16, row: u16) -> bool {
    match kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let overlays = app.extension_manager.overlay_targets();
            for (ext_id, overlay) in overlays.iter().rev() {
                let ox = overlay.x;
                let oy = overlay.y;
                let ow = overlay.width;
                let oh = overlay.height;

                if col >= ox && col < ox + ow && row >= oy && row < oy + oh {
                    if row == oy {
                        // Title bar — check close button or start drag
                        let close_x = ox + ow.saturating_sub(3);
                        if col >= close_x {
                            let ext_id = ext_id.clone();
                            let overlay_id = overlay.id.clone();
                            app.extension_manager.close_overlay(&ext_id, &overlay_id);
                            return true;
                        }
                        app.extension_overlay_drag = Some((
                            ext_id.clone(),
                            overlay.id.clone(),
                            col as i16 - ox as i16,
                            row as i16 - oy as i16,
                        ));
                        return true;
                    }
                    // Content area — dispatch to overlay mouse handler
                    if overlay.mouse_handler {
                        let payload = serde_json::json!({
                            "kind": "down",
                            "column": col,
                            "row": row,
                            "viewColumn": col.saturating_sub(ox + 1),
                            "viewRow": row.saturating_sub(oy + 1),
                            "width": ow.saturating_sub(2),
                            "height": oh.saturating_sub(2),
                        })
                        .to_string();
                        let ext_id = ext_id.clone();
                        let overlay_id = overlay.id.clone();
                        let ctx = app.current_extension_editor_context();
                        // Error OK: extension handler failure is non-fatal
                        let _ = app.extension_manager.invoke_overlay_mouse_handler(
                            &ext_id,
                            &overlay_id,
                            &payload,
                            Some(ctx),
                        );
                    }
                    return true;
                }
            }
            false
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if let Some((ref ext_id, ref overlay_id, offset_x, offset_y)) =
                app.extension_overlay_drag
            {
                let new_x = (col as i16 - offset_x).max(0) as u16;
                let new_y = (row as i16 - offset_y).max(0) as u16;
                let ext_id = ext_id.clone();
                let overlay_id = overlay_id.clone();
                app.extension_manager
                    .update_overlay_position(&ext_id, &overlay_id, new_x, new_y);
                return true;
            }
            false
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if app.extension_overlay_drag.is_some() {
                app.extension_overlay_drag = None;
                return true;
            }
            let overlays = app.extension_manager.overlay_targets();
            for (ext_id, overlay) in overlays.iter().rev() {
                let ox = overlay.x;
                let oy = overlay.y;
                let ow = overlay.width;
                let oh = overlay.height;
                if col >= ox && col < ox + ow && row > oy && row < oy + oh {
                    if overlay.mouse_handler {
                        let payload = serde_json::json!({
                            "kind": "up",
                            "column": col,
                            "row": row,
                            "viewColumn": col.saturating_sub(ox + 1),
                            "viewRow": row.saturating_sub(oy + 1),
                            "width": ow.saturating_sub(2),
                            "height": oh.saturating_sub(2),
                        })
                        .to_string();
                        let ext_id = ext_id.clone();
                        let overlay_id = overlay.id.clone();
                        let ctx = app.current_extension_editor_context();
                        // Error OK: extension handler failure is non-fatal
                        let _ = app.extension_manager.invoke_overlay_mouse_handler(
                            &ext_id,
                            &overlay_id,
                            &payload,
                            Some(ctx),
                        );
                    }
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

fn find_menu_item_at_row(items: &[crate::app::ContextMenuItem], target_row: usize) -> Option<usize> {
    let mut visual_row = 0;
    for (i, item) in items.iter().enumerate() {
        if visual_row == target_row {
            return Some(i);
        }
        visual_row += 1;
        if item.separator_after {
            visual_row += 1;
        }
    }
    None
}

/// A dropdown button owns the opening press through release, including the
/// button and the gap before the rendered menu. Ordinary context menus still
/// activate on a fresh press and ignore the release that opened them.
pub(crate) fn capture_context_menu_button(app: &mut App, button: Rect) {
    if !app.context_menu_active { return; }
    app.clear_drag_states();
    app.drag_start_pos = None;
    app.drag_has_moved = false;
    app.reset_hover_states();
    app.context_menu_button_drag = Some(button);
}

pub(crate) fn handle_context_menu_mouse(app: &mut App, mouse: event::MouseEvent) {
    let col = mouse.column;
    let row = mouse.row;
    app.mouse_col = col;
    app.mouse_row = row;
    if matches!(mouse.kind, MouseEventKind::Down(_)) {
        app.context_menu_button_drag = None;
    }
    // Dropdowns can flip above the pointer or lose rows when the window is
    // resized. Only the rows actually drawn can select a command.
    let hit = app.context_menu_inner
        .filter(|inner| {
            let rows = app.context_menu_items.len() + app.context_menu_items.iter().filter(|item| item.separator_after).count();
            let scrollbar = if rows > inner.height as usize { 2.min(inner.width.saturating_sub(1)) } else { 0 };
            Rect::new(inner.x, inner.y, inner.width - scrollbar, inner.height).contains((col, row).into())
        })
        .and_then(|inner| find_menu_item_at_row(&app.context_menu_items, app.context_menu_scroll + (row - inner.y) as usize));
    let in_menu = app.context_menu_inner.is_some_and(|inner| {
        Rect::new(inner.x.saturating_sub(1), inner.y.saturating_sub(1),
            inner.width.saturating_add(2), inner.height.saturating_add(2))
            .contains((col, row).into())
    });

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(item_idx) = hit {
                app.context_menu_selected = item_idx;
                crate::input_pickers::handle_context_menu_key(app, KeyCode::Enter);
            } else if !in_menu {
                app.context_menu_active = false;
            }
        }
        MouseEventKind::Moved | MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(item_idx) = hit {
                app.context_menu_selected = item_idx;
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if let Some(button) = app.context_menu_button_drag.take()
                && !button.contains((col, row).into())
                && let Some(item_idx) = hit
            {
                app.context_menu_selected = item_idx;
                crate::input_pickers::handle_context_menu_key(app, KeyCode::Enter);
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if in_menu => {
            app.context_menu_selected = if mouse.kind == MouseEventKind::ScrollUp {
                app.context_menu_selected.saturating_sub(1)
            } else {
                (app.context_menu_selected + 1).min(app.context_menu_items.len().saturating_sub(1))
            };
        }
        _ => {
            if !in_menu {
                app.context_menu_button_drag = None;
                app.context_menu_active = false;
            }
        }
    }
}

/// Handle mouse clicks on the settings panel. Returns true if click was consumed.
pub(crate) fn handle_settings_mouse(app: &mut App, col: u16, row: u16, term_w: u16, term_h: u16) -> bool {
    let items = app.settings_items();
    let width = SETTINGS_PANEL_WIDTH.min(term_w.saturating_sub(4));
    let max_visible = (term_h.saturating_sub(6)) as usize;
    let height = (max_visible as u16 + 4).min(term_h.saturating_sub(2));
    let px = (term_w.saturating_sub(width)) / 2;
    let py = (term_h.saturating_sub(height)) / 2;

    if col >= px && col < px + width && row >= py && row < py + height {
        // Inner area starts at px+1, py+1, with 1 row for hints
        let inner_y = py + 2; // border + hint row
        let inner_h = height.saturating_sub(3);
        if row >= inner_y && row < inner_y + inner_h {
            let visible_items = inner_h as usize;
            let scroll_offset = if app.settings_selected >= visible_items {
                app.settings_selected - visible_items + 1
            } else {
                0
            };
            let clicked_idx = scroll_offset + (row - inner_y) as usize;
            if clicked_idx < items.len() {
                app.settings_selected = clicked_idx;
                // Toggle the setting
                let id = items[clicked_idx].2.to_string();
                app.toggle_setting(&id);
            }
        }
        return true;
    }
    // Click outside settings -> close
    app.settings_active = false;
    true
}

/// Handle mouse clicks on popup panels (palette, quick open, dialogs, etc.)
/// Returns `true` if the event was consumed.
fn point_in_rect(area: Rect, col: u16, row: u16) -> bool {
    col >= area.x
        && col < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

fn handle_completion_popup_click(app: &mut App, col: u16, row: u16, viewport: Rect) -> bool {
    if crate::ui_lsp_completion::handle_mouse(app, event::MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left), column: col, row,
        modifiers: KeyModifiers::NONE,
    }) { return true; }
    if app.lsp_completion_active && app.lsp_manager.completion_items.is_empty() {
        app.lsp_completion_active = false;
        app.dispatch_lsp_suggestions();
    }
    // LSP completions render last and therefore have mouse priority.
    if app.lsp_completion_active && !app.lsp_manager.completion_items.is_empty() {
        let item_count = app.lsp_manager.completion_items.len();
        if let Some(geometry) = crate::ui_search_dialog::lsp_completion_popup_geometry(app, viewport) {
            if let Some(index) = crate::ui_search_dialog::completion_index_at(geometry, col, row, item_count) {
                app.lsp_completion_selected = index;
                app.apply_lsp_completion();
                return true;
            }
            if point_in_rect(geometry.popup_area, col, row) {
                return true;
            }
        }
        app.lsp_completion_active = false;
        return false;
    }

    if app.vtcomplete_active && !app.vtcomplete_suggestions.is_empty() {
        let item_count = app.vtcomplete_suggestions.len();
        if let Some(geometry) = crate::ui_search_dialog::vtcomplete_popup_geometry(app, viewport) {
            if let Some(index) = crate::ui_search_dialog::completion_index_at(geometry, col, row, item_count) {
                app.vtcomplete_selected = index;
                app.apply_vtcomplete();
                return true;
            }
            if point_in_rect(geometry.popup_area, col, row) {
                return true;
            }
        }
        app.dismiss_vtcomplete();
    }

    false
}

pub(crate) fn handle_popup_click(app: &mut App, col: u16, row: u16) -> bool {
    if crate::ui_quick_picker::handle_mouse(
        app,
        event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        },
    ) {
        return true;
    }
    if crate::ui_modal::handle_mouse(
        app,
        event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        },
    ) {
        return true;
    }
    let viewport = app.viewport_area;
    let (term_w, term_h) = (viewport.width, viewport.height);

    if app.change_dir_active {
        return handle_folder_browser_click(app, col, row);
    }

    // Global search click
    if app.global_search_active
        && !(app.show_explorer && app.sidebar_view == crate::app::SidebarView::Search)
    {
        let max_results = 15u16; // Must match draw_global_search
        let height = (len_as_u16(app.global_search_results.len()) + 3).min(max_results + 3).max(5);
        let width = 70u16.min(term_w.saturating_sub(4));
        let px = (term_w.saturating_sub(width)) / 2;
        let py = 2u16;
        // Close button click: " × " in top-right border
        let close_x = px + width - 4;
        if row == py && col >= close_x && col < px + width {
            app.global_search_active = false;
            return true;
        }
        let inner_x = px + 1;
        let inner_y = py + 1;
        let inner_h = height.saturating_sub(2);
        let results_area_height = inner_h.saturating_sub(1) as usize;
        // Input row is inner_y, results start at inner_y + 1
        if col >= inner_x && col < inner_x + width.saturating_sub(2)
            && row > inner_y && row < py + height.saturating_sub(1)
        {
            let scroll = app.global_search_selected.saturating_sub(
                results_area_height.saturating_sub(1)
            );
            let clicked = scroll + (row - inner_y - 1) as usize;
            if clicked < app.global_search_results.len() {
                app.global_search_selected = clicked;
                input::handle_global_search_key(app, KeyCode::Enter, KeyModifiers::NONE);
            }
            return true;
        }
        if !(col >= px && col < px + width && row >= py && row < py + height) {
            app.global_search_active = false;
            return true;
        }
        return true;
    }

    if handle_completion_popup_click(app, col, row, viewport) {
        return true;
    }

    // Use the controls recorded by the rendered Find widget.
    if let Some(bar) = app.file_search_bar_at(col, row) {
        app.focus_file_search_owner(bar.owner);
        if crate::ui_search_dialog::search_bar_resize_hit(app, col, row) {
            app.clear_drag_states();
            app.search_bar_layout.resizing = true;
            return true;
        }
        crate::ui_search_controls::handle_click(
            app, crate::ui_search_controls::SearchScope::File, col, row,
        );
        return true;
    }

    // Peek definition click (click outside to dismiss)
    if let Some(peek) = app.peek_definition.as_ref() {
        if let Some(code_area) = app.code_area {
            let width = code_area.width.min(80);
            let height = (len_as_u16(peek.lines.len()) + 2).clamp(5, 15);
            let px = code_area.x + (code_area.width.saturating_sub(width)) / 2;
            let py = code_area.y + code_area.height.saturating_sub(height);
            if col < px || col >= px + width || row < py || row >= py + height {
                app.peek_definition = None;
                return true;
            }
        }
        return true;
    }

    // Diff layout and navigation buttons
    if app.diff_layout_toggle_area.is_some_and(|area| area.contains((col, row).into())) {
        app.execute_palette_command("toggle_diff_layout");
        return true;
    }
    if let Some(prev_area) = app.diff_nav_prev_area {
        if row == prev_area.y && col >= prev_area.x && col < prev_area.x + prev_area.width {
            app.execute_palette_command("prev_diff_hunk");
            return true;
        }
    }
    if let Some(next_area) = app.diff_nav_next_area {
        if row == next_area.y && col >= next_area.x && col < next_area.x + next_area.width {
            app.execute_palette_command("next_diff_hunk");
            return true;
        }
    }

    if let Some(button_area) = app.breadcrumb_markdown_preview_area {
        if row == button_area.y
            && col >= button_area.x
            && col < button_area.x.saturating_add(button_area.width)
        {
            app.toggle_markdown_preview_tab();
            return true;
        }
    }

    // Breadcrumb dropdown click
    if app.breadcrumb_dropdown_active {
        // Check if click is on a dropdown item
        if let Some(bc_area) = app.breadcrumb_area {
            let dropdown_x = app.breadcrumb_segments.get(app.breadcrumb_dropdown_segment)
                .map_or(bc_area.x, |&(x, _, _)| x);
            let dropdown_y = bc_area.y + 1;
            let items_count = app.breadcrumb_dropdown_items.len();
            let max_name_len = app.breadcrumb_dropdown_items.iter()
                .filter_map(|(path, _)| path.file_name().and_then(|n| n.to_str()).map(UnicodeWidthStr::width))
                .max()
                .unwrap_or(10);
            let dw = (max_name_len + 6) as u16;
            let _dh = (items_count as u16 + 2).min(15);
            let inner_y = dropdown_y + 1;
            if col >= dropdown_x && col < dropdown_x + dw
                && row >= inner_y && row < inner_y + items_count as u16
                && (row - inner_y) < items_count as u16
            {
                let idx = (row - inner_y) as usize;
                if let Some((item, is_dir)) = app.breadcrumb_dropdown_items.get(idx).cloned() {
                    app.breadcrumb_dropdown_active = false;
                    if is_dir {
                        // Navigate into directory in explorer
                        app.explorer.select_path(&item);
                    } else {
                        app.open_file(&item);
                    }
                }
                return true;
            }
        }
        // Click outside dropdown dismisses it
        app.breadcrumb_dropdown_active = false;
        return true;
    }

    // Breadcrumb segment click (open dropdown)
    if let Some(bc_area) = app.breadcrumb_area {
        if row == bc_area.y && col >= bc_area.x && col < bc_area.x + bc_area.width {
            // Find matching segment without cloning Vec
            let matched = app.breadcrumb_segments.iter()
                .find(|&&(seg_x, seg_w, _)| col >= seg_x && col < seg_x + seg_w)
                .copied();
            if let Some((_seg_x, _seg_w, seg_take)) = matched {
                    // Find the index of this segment in breadcrumb_segments for dropdown positioning
                    let seg_vec_idx = app.breadcrumb_segments.iter()
                        .position(|s| s.2 == seg_take)
                        .unwrap_or(0);
                    // Build the full path up to this segment
                    if let Some(editor) = app.editor() {
                        if let Some(file_path) = &editor.file_path {
                            let rel = file_path.strip_prefix(&app.explorer.root)
                                .unwrap_or(file_path);
                            let rel_str = rel.to_string_lossy().into_owned();
                            let parts: Vec<&str> = rel_str.split(['/', '\\']).collect();
                            // Build parent directory path for this segment
                            let mut dir = app.explorer.root.clone();
                            for part in parts.iter().take(seg_take) {
                                dir = dir.join(part);
                            }
                            app.breadcrumb_dropdown_generation =
                                app.breadcrumb_dropdown_generation.wrapping_add(1);
                            let generation = app.breadcrumb_dropdown_generation;
                            app.breadcrumb_dropdown_items.clear();
                            app.breadcrumb_dropdown_segment = seg_vec_idx;
                            app.breadcrumb_dropdown_selected = 0;
                            app.breadcrumb_dropdown_loading = true;
                            app.breadcrumb_dropdown_active = true;

                            let sender = app.background.sender.clone();
                            let remote = app.remote_workspace.clone();
                            app.background.spawn("breadcrumb-list", move || {
                                let mut items: Vec<(std::path::PathBuf, bool)> = if let Some(remote) = remote {
                                    remote.remote_path(&dir).and_then(|path| remote.list(&path)).map(|entries| entries.iter()
                                        .filter(|entry| !entry.path.name().starts_with('.')).map(|entry| (remote.view_key(&entry.path),entry.is_dir)).collect()).unwrap_or_default()
                                } else { std::fs::read_dir(&dir)
                                        .into_iter()
                                        .flatten()
                                        .filter_map(Result::ok)
                                        .filter_map(|entry| {
                                            let path = entry.path();
                                            let name = path
                                                .file_name()
                                                .and_then(|name| name.to_str())?;
                                            if name.starts_with('.') {
                                                return None;
                                            }
                                            let is_dir = match entry.file_type() {
                                                Ok(kind) if kind.is_symlink() => path.is_dir(),
                                                Ok(kind) => kind.is_dir(),
                                                Err(_) => path.is_dir(),
                                            };
                                            Some((path, is_dir))
                                        })
                                        .collect() };
                                items.sort_by_cached_key(|(path, is_dir)| {
                                    let name = path
                                        .file_name()
                                        .and_then(|name| name.to_str())
                                        .unwrap_or("")
                                        .to_lowercase();
                                    (!*is_dir, name)
                                });
                                let _ = sender.send(
                                    crate::background::BackgroundResult::BreadcrumbEntries {
                                        generation,
                                        segment: seg_vec_idx,
                                        entries: items,
                                    },
                                );
                            });
                        }
                    }
                    return true;
            }
        }
    }

    // Source control commit details popup click
    if app.hunk_popup_active {
        if app.hunk_peek.is_some() || app.hunk_preview_feedback.is_some() {
            crate::ui_hunk_peek::click(app, col, row, term_w, term_h);
            return true;
        }
        let popup_area = app.hunk_popup_rect(term_w, term_h);
        if col >= popup_area.x
            && col < popup_area.x + popup_area.width
            && row >= popup_area.y
            && row < popup_area.y + popup_area.height
        {
            let inner_y = popup_area.y + 1;
            let items = app.hunk_popup_items();
            if row >= inner_y && row < inner_y + len_as_u16(items.len()) {
                let idx = (row - inner_y) as usize;
                app.hunk_popup_selected = idx;
                app.execute_hunk_popup_action(idx);
            }
            return true;
        }
        app.close_hunk_popup();
        return true;
    }

    // Source control context menu click
    if app.sc_context_active {
        let items = app.source_control_context_menu_items();
        if let Some(inner) = app.sc_context_inner {
            if inner.contains((col, row).into()) {
                let idx = (row - inner.y) as usize;
                if idx < items.len() {
                    app.sc_context_selected = idx;
                    app.execute_source_control_context(idx);
                }
                return true;
            }
        }
        app.sc_context_active = false;
        app.sc_context_inner = None;
        return true;
    }

    // Explorer context menu click
    if app.explorer_context_active {
        let items = app.explorer_context_menu_items();
        if let Some(inner) = app.explorer_context_inner {
            if inner.contains((col, row).into()) {
                let idx = (row - inner.y) as usize;
                if items.get(idx).is_some_and(|&i| i != "─") {
                    app.explorer_context_selected = idx;
                    app.execute_explorer_context(idx);
                }
                return true;
            }
        }
        app.explorer_context_active = false;
        return true;
    }

    // Settings panel click
    if app.settings_active
        && handle_settings_mouse(app, col, row, term_w, term_h) {
            return true;
        }

    // Shortcuts panel click (click outside to dismiss)
    if app.shortcuts_active {
        let width = 70u16.min(term_w.saturating_sub(4));
        let height = 35u16.min(term_h.saturating_sub(4));
        let dx = (term_w.saturating_sub(width)) / 2;
        let dy = (term_h.saturating_sub(height)) / 2;
        if col < dx || col >= dx + width || row < dy || row >= dy + height {
            app.shortcuts_active = false;
        }
        return true;
    }

false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{ContextMenuItem, Tab};
    use crate::editor::Editor;
    use crate::lsp::LspCompletionItem;

    fn make_item(label: &str, sep: bool) -> ContextMenuItem {
        ContextMenuItem {
            label: label.to_string(),
            shortcut: String::new(),
            id: "test",
            separator_after: sep,
        }
    }

    fn completion_app(text: &str) -> App {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(text);
        editor.cursor_line = 0;
        editor.cursor_col = text.chars().count();
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.code_area = Some(Rect::new(5, 5, 70, 15));
        app
    }

    fn lsp_completion(label: &str) -> LspCompletionItem {
        LspCompletionItem {
            label: label.to_string(),
            detail: None,
            insert_text: label.to_string(),
            kind: None,
            text_edit_range: None,
            additional_text_edits: Vec::new(),
            is_snippet: false,
            original_item: None,
            documentation: None,
        }
    }

    #[test]
    fn clicking_builtin_completion_accepts_clicked_row() {
        let viewport = Rect::new(0, 0, 100, 30);
        let mut app = completion_app("pr");
        app.vtcomplete_active = true;
        app.vtcomplete_suggestions = vec!["print".to_string(), "println".to_string()];
        let geometry = crate::ui_search_dialog::vtcomplete_popup_geometry(&mut app, viewport).unwrap();

        assert!(handle_completion_popup_click(
            &mut app,
            geometry.inner_area.x,
            geometry.inner_area.y + 1,
            viewport,
        ));
        assert_eq!(app.editor().unwrap().rope.to_string(), "println");
        assert!(!app.vtcomplete_active);
    }

    #[test]
    fn clicking_lsp_completion_accepts_clicked_row() {
        let viewport = Rect::new(0, 0, 100, 30);
        let mut app = completion_app("pr");
        app.lsp_completion_active = true;
        app.lsp_manager.completion_items = vec![lsp_completion("print"), lsp_completion("println")];
        let geometry = crate::ui_search_dialog::lsp_completion_popup_geometry(&mut app, viewport).unwrap();

        assert!(handle_completion_popup_click(
            &mut app,
            geometry.inner_area.x,
            geometry.inner_area.y + 1,
            viewport,
        ));
        assert_eq!(app.editor().unwrap().rope.to_string(), "println");
        assert!(!app.lsp_completion_active);
    }

    #[test]
    fn clicking_scrolled_completion_uses_visible_row_index() {
        let viewport = Rect::new(0, 0, 100, 25);
        let mut app = completion_app("it");
        app.code_area = Some(Rect::new(5, 20, 70, 4));
        app.vtcomplete_active = true;
        app.vtcomplete_selected = 9;
        app.vtcomplete_suggestions = (0..10).map(|i| format!("item{i}")).collect();
        let geometry = crate::ui_search_dialog::vtcomplete_popup_geometry(&mut app, viewport).unwrap();

        assert_eq!(geometry.scroll, 2);
        assert!(geometry.popup_area.y < app.code_area.unwrap().y);
        assert!(handle_completion_popup_click(
            &mut app,
            geometry.inner_area.x,
            geometry.inner_area.y,
            viewport,
        ));
        assert_eq!(app.editor().unwrap().rope.to_string(), "item2");
    }

    #[test]
    fn folder_browser_cancel_uses_the_rendered_button_area() {
        let mut app = App::test_new();
        app.change_dir_active = true;
        app.folder_browser_buttons = vec![(20, 20, 13), (35, 20, 8)];

        assert!(handle_folder_browser_click(&mut app, 36, 20));
        assert!(!app.change_dir_active);
    }

    #[test]
    fn folder_browser_list_click_uses_the_full_view_scroll_offset() {
        let mut app = App::test_new();
        app.change_dir_active = true;
        app.folder_browser_path = std::path::PathBuf::from("C:/workspace");
        app.folder_browser_entries = (0..10)
            .map(|index| std::path::PathBuf::from(format!("C:/workspace/folder-{index}")))
            .collect();
        app.folder_browser_area = Some(Rect::new(1, 2, 98, 20));
        app.folder_browser_scroll = 3;
        app.folder_browser_selected = 0;

        assert!(handle_folder_browser_click(&mut app, 5, 4));
        assert_eq!(app.folder_browser_selected, 5);
        assert!(app.change_dir_active);
    }

    // --- find_menu_item_at_row tests ---

    #[test]
    fn find_item_empty_list() {
        assert_eq!(find_menu_item_at_row(&[], 0), None);
    }

    #[test]
    fn find_item_first_row() {
        let items = vec![make_item("A", false), make_item("B", false)];
        assert_eq!(find_menu_item_at_row(&items, 0), Some(0));
    }

    #[test]
    fn find_item_second_row() {
        let items = vec![make_item("A", false), make_item("B", false)];
        assert_eq!(find_menu_item_at_row(&items, 1), Some(1));
    }

    #[test]
    fn find_item_past_end() {
        let items = vec![make_item("A", false)];
        assert_eq!(find_menu_item_at_row(&items, 1), None);
    }

    #[test]
    fn find_item_with_separator() {
        // A (row 0) + separator (row 1), B (row 2)
        let items = vec![make_item("A", true), make_item("B", false)];
        assert_eq!(find_menu_item_at_row(&items, 0), Some(0));
        assert_eq!(find_menu_item_at_row(&items, 1), None); // separator row
        assert_eq!(find_menu_item_at_row(&items, 2), Some(1));
    }

    #[test]
    fn find_item_multiple_separators() {
        let items = vec![
            make_item("Cut", true),
            make_item("Copy", true),
            make_item("Paste", false),
        ];
        // Cut=0, sep=1, Copy=2, sep=3, Paste=4
        assert_eq!(find_menu_item_at_row(&items, 0), Some(0));
        assert_eq!(find_menu_item_at_row(&items, 2), Some(1));
        assert_eq!(find_menu_item_at_row(&items, 4), Some(2));
        assert_eq!(find_menu_item_at_row(&items, 5), None);
    }

    // --- handle_settings_mouse tests ---

    fn test_app() -> App {
        App::new(std::env::current_dir().unwrap())
    }

    #[test]
    fn settings_click_outside_closes() {
        let mut app = test_app();
        app.settings_active = true;
        // Click far outside the settings panel
        handle_settings_mouse(&mut app, 0, 0, 120, 40);
        assert!(!app.settings_active);
    }

    #[test]
    fn settings_click_inside_consumed() {
        let mut app = test_app();
        app.settings_active = true;
        // Click in center of terminal (where settings panel would be)
        let result = handle_settings_mouse(&mut app, 60, 20, 120, 40);
        assert!(result);
    }

}
