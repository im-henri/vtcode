use crossterm::event::KeyModifiers;
use crate::app::{ActivePane, App};

/// Switch focus to a non-active split group if the click lands inside its area.
///
/// When `check_scrollbars` is true, clicks on a group's scrollbar areas also
/// trigger the focus switch (used by left-click). When false, only the
/// editor/tab area is checked (used by middle-click).
pub(crate) fn switch_focus_if_split(app: &mut App, col: u16, row: u16, check_scrollbars: bool) {
    if !app.split_active() {
        return;
    }
    for (gi, group) in app.groups.iter().enumerate() {
        if gi == app.active_group {
            continue;
        }
        let in_editor_or_tab = group.editor_area.is_some_and(|split_area| {
            let tab_top = group.tab_bar_area.map_or(split_area.y, |a| a.y);
            col >= split_area.x
                && col < split_area.x + split_area.width
                && row >= tab_top
                && row < split_area.y + split_area.height
        });
        let in_scrollbar = check_scrollbars
            && (group.scrollbar_area.is_some_and(|area| {
                col >= area.x
                    && col < area.x + area.width
                    && row >= area.y
                    && row < area.y + area.height
            }) || group.h_scrollbar_area.is_some_and(|area| {
                col >= area.x
                    && col < area.x + area.width
                    && row >= area.y
                    && row < area.y + area.height
            }));
        if in_editor_or_tab || in_scrollbar {
            app.switch_group(gi);
            break;
        }
    }
}

/// Handle left-clicks in the editor area: gutter, minimap, merge-conflict
/// buttons, code area (cursor / selection), scrollbars, and editor focus.
pub(crate) fn handle_editor_area_left_click(
    app: &mut App,
    col: u16,
    row: u16,
    modifiers: KeyModifiers,
) {
    if app.tabs.get(app.active_tab).is_some_and(|tab| tab.kind == crate::app::TabKind::MarkdownPreview) {
        if app.scrollbar_area.is_some_and(|area| area.contains(ratatui::layout::Position::new(col, row))) {
            app.dragging_v_scrollbar = true;
            app.markdown_preview_scrollbar_input(row, true);
        } else {
            let opened = modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::SHIFT)
                && app.markdown_preview_link_click(col, row);
            if !opened { app.markdown_preview_selection_click(col, row, modifiers.contains(KeyModifiers::SHIFT)); }
        }
        app.active_pane = ActivePane::Editor;
        return;
    }
    // The original (left) diff pane owns an independent horizontal scrollbar.
    if let Some(h_area) = app.diff_left_h_scrollbar_area {
        if col >= h_area.x
            && col < h_area.right()
            && row >= h_area.y
            && row < h_area.bottom()
        {
            app.dragging_diff_left_h_scrollbar = true;
            let total_width = app.diff_render_cache.max_left_text_width;
            let visible_width = h_area.width as usize;
            if total_width > visible_width {
                let relative = col.saturating_sub(h_area.x) as usize;
                let max_scroll = total_width.saturating_sub(visible_width);
                app.diff_left_horizontal_scroll =
                    relative.saturating_mul(max_scroll) / visible_width.max(1);
            }
            return;
        }
    }

    if app.diff_selection_mouse_down(col, row, modifiers) { return; }

    // Click on gutter (line numbers) — select entire line
    if let Some(gutter_area) = app.gutter_area {
        if col >= gutter_area.x
            && col < gutter_area.x + gutter_area.width
            && row >= gutter_area.y
            && row < gutter_area.y + gutter_area.height
        {
            app.active_pane = ActivePane::Editor;
            app.clear_diff_selection();
            let is_diff_col = col == gutter_area.x;
            let is_fold_col = col == gutter_area.x + gutter_area.width - 1;
            let screen_row = (row - gutter_area.y) as usize;
            let (clicked_row, file_path, diff_status) = {
                let Some(ed) = app.editor_mut() else {
                    return;
                };
                let clicked_row = if let Some(rows) = &ed.diff_display_rows {
                    rows.get(screen_row).copied().flatten().map(|line_idx| crate::editor::WrappedRowInfo {
                        line_idx, start_col: 0, end_col: ed.line_len(line_idx), line_len: ed.line_len(line_idx), is_first: true,
                    })
                } else if ed.ai_review.active {
                    ed.ai_review.source_rows.get(screen_row).copied().flatten()
                } else if ed.word_wrap {
                    if screen_row >= ed.viewport_height.max(1) {
                        None
                    } else if ed.ensure_row_offsets(ed.effective_wrap_width()) {
                        ed.wrapped_row_at(
                            ed.wrap_scroll_offset.saturating_add(screen_row),
                            ed.effective_wrap_width(),
                        )
                    } else {
                        // The pending-layout view paints consecutive visible
                        // logical lines, rather than independent raw row IDs.
                        ed.visible_line_at(ed.wrap_scroll_offset, screen_row).map(|line_idx| {
                            let line_len = ed.line_len(line_idx);
                            crate::editor::WrappedRowInfo {
                                line_idx, start_col: 0, end_col: line_len, line_len, is_first: true,
                            }
                        })
                    }
                } else {
                    (screen_row < ed.viewport_height)
                        .then(|| ed.visible_line_at(ed.scroll_offset, screen_row))
                        .flatten()
                        .map(|line_idx| {
                            let line_len = ed.line_len(line_idx);
                            crate::editor::WrappedRowInfo {
                            line_idx,
                            start_col: 0,
                            end_col: line_len,
                            line_len,
                            is_first: true,
                            }
                        })
                };
                let diff_status = clicked_row
                    .as_ref()
                    .map_or(' ', |row| ed.line_diff_status(row.line_idx));
                (clicked_row, ed.file_path.clone(), diff_status)
            };
            if let Some(clicked_row) = clicked_row {
                let clicked_line = clicked_row.line_idx;
                let compare_view = app.should_render_diff_for_active_editor();
                if is_diff_col {
                    if compare_view {
                        app.notify("Hunk actions are unavailable in compare view");
                    } else if matches!(diff_status, 'N' | 'U' | 'A' | 'M' | 'D' | 'S' | '?') {
                        app.open_hunk_popup(col, row, file_path, clicked_line, diff_status);
                    }
                    return;
                }
                if is_fold_col {
                    if clicked_row.is_first {
                        if let Some(ed) = app.editor_mut() {
                            ed.toggle_fold(clicked_line);
                        }
                    }
                } else if let Some(ed) = app.editor_mut() {
                    ed.cursor_line = clicked_line;
                    ed.cursor_col = 0;
                    ed.desired_col = None;
                    ed.select_line();
                    app.dragging_gutter = true;
                    app.dragging_gutter_start_line = Some(clicked_line);
                }
            }
            return;
        }
    }

    // Click on minimap — scroll only (never move cursor)
    if let Some(minimap_area) = app.minimap_area {
        if col >= minimap_area.x
            && col < minimap_area.x + minimap_area.width
            && row >= minimap_area.y
            && row < minimap_area.y + minimap_area.height
        {
            app.active_pane = ActivePane::Editor;
            app.dragging_minimap = true;
            app.minimap_drag_start = None;
            let mut drag_start = None;
            if let Some(ed) = app.editor_mut() {
                let total_lines = ed.line_count();
                let minimap_height = minimap_area.height as usize;
                if minimap_height > 0 && total_lines > 0 {
                    let click_row = (row - minimap_area.y) as usize;
                    let layout = crate::ui_editor_minimap::MinimapLayout::new(
                        total_lines,
                        ed.scroll_offset,
                        ed.viewport_height,
                        minimap_height,
                        ed.scroll_past_end,
                    );
                    ed.scroll_offset = layout.click_scroll(click_row);
                    drag_start = Some((click_row, ed.scroll_offset));
                    if ed.word_wrap {
                        let wrap_row = ed.wrapped_row_for_position(
                            ed.scroll_offset,
                            0,
                            ed.effective_wrap_width(),
                        );
                        ed.set_wrap_scroll_offset(wrap_row);
                    }
                } else {
                    // Still consume the click to prevent click-through.
                    ed.scroll_offset = 0;
                    if ed.word_wrap {
                        ed.set_wrap_scroll_offset(0);
                    }
                }
            }
            app.minimap_drag_start = drag_start;
            return;
        }
    }

    if crate::ui_editor_folds::click(app, col, row) { return; }

    // Click on editor code area?
    if let Some(code_area) = app.code_area {
        if col >= code_area.x
            && col < code_area.x + code_area.width
            && row >= code_area.y
            && row < code_area.y + code_area.height
        {
            app.active_pane = ActivePane::Editor;
            app.editor_selection_drag = app.editor().map(|editor| editor.navigation_id);

            // Modified clicks start their own gesture; a previous word/line
            // selection must not turn Shift-drag into a word/line drag.
            if !modifiers.is_empty() {
                app.multi_click_anchor = None;
                app.dragging_text = false;
                app.dragging_text_start = None;
            }
            // Multi-click detection (double/triple)
            let now = std::time::Instant::now();
            if modifiers.is_empty()
                && now.duration_since(app.last_click_time).as_millis() < 400
                && app.last_click_pos == (col, row)
            {
                app.click_count = (app.click_count + 1).min(3);
            } else {
                app.click_count = if modifiers.is_empty() { 1 } else { 0 };
            }
            app.last_click_time = now;
            app.last_click_pos = (col, row);

            // Alt+Click toggles an extra cursor; Ctrl+Click opens definitions.
            let ctrl_held = modifiers.contains(KeyModifiers::CONTROL);
            let alt_held = modifiers.contains(KeyModifiers::ALT);
            let shift_held = modifiers.contains(KeyModifiers::SHIFT);
            let ctrl_position = (ctrl_held && !alt_held && !shift_held)
                .then(|| crate::app_lsp_links::editor_position_at(app, col, row)).flatten();

            // Only the press edge owns plain Alt+click. Ignore the terminal's
            // subsequent drag/release sequence until the button comes up.
            app.alt_cursor_mouse_down_active = alt_held && !shift_held && !ctrl_held;

            let click_count = app.click_count;
            if alt_held && shift_held {
                app.block_drag_cache = None;
            }
            let mut new_mc_anchor: Option<Option<(usize, usize, usize, usize)>> = None;
            if let Some(ed) = app.editor_mut() {
                let (clicked_line, clicked_col) = ctrl_position.unwrap_or_else(||
                    crate::mouse_drag::code_area_to_buffer_pos(ed, code_area, col, row));

                if alt_held && shift_held {
                    // Alt+Shift+Click: start column/block drag selection
                    ed.clear_selection();
                    ed.extra_cursors.clear();
                    ed.extra_cursor_anchors.clear();
                    let clamped_col = clicked_col.min(ed.line_len(clicked_line));
                    ed.cursor_line = clicked_line;
                    ed.cursor_col = clamped_col;
                    ed.desired_col = None;
                    let start_visual_col = if ed.word_wrap {
                        col.saturating_sub(code_area.x) as usize
                    } else {
                        ed.horizontal_scroll
                            .saturating_add(col.saturating_sub(code_area.x) as usize)
                    };
                    app.middle_drag_start =
                        Some((clicked_line, clicked_col, start_visual_col));
                } else if shift_held {
                    // Shift+Click: extend selection to clicked position
                    ed.extra_cursors.clear();
                    ed.extra_cursor_anchors.clear();
                    if ed.selection_anchor.is_none() {
                        ed.selection_anchor = Some((ed.cursor_line, ed.cursor_col));
                    }
                    let clamped_col = clicked_col.min(ed.line_len(clicked_line));
                    ed.cursor_line = clicked_line;
                    ed.cursor_col = clamped_col;
                    ed.desired_col = None;
                } else if ctrl_held {
                    // Ctrl+Click: go to definition (like VS Code)
                    ed.extra_cursors.clear();
                    ed.extra_cursor_anchors.clear();
                    ed.clear_selection();
                    ed.set_cursor_without_scrolling(clicked_line, clicked_col);
                } else if alt_held {
                    // Alt+Click: add a new cursor or remove the one already here.
                    ed.toggle_extra_cursor((clicked_line, clicked_col));
                } else {
                    match click_count {
                        3 => {
                            ed.extra_cursors.clear();
                            ed.extra_cursor_anchors.clear();
                            ed.clear_selection();
                            ed.set_cursor_without_scrolling(clicked_line, clicked_col);
                            ed.select_line();
                            let line_end = ed.line_len(clicked_line);
                            new_mc_anchor =
                                Some(Some((clicked_line, 0, clicked_line, line_end)));
                        }
                        2 => {
                            ed.extra_cursors.clear();
                            ed.extra_cursor_anchors.clear();
                            ed.clear_selection();
                            ed.set_cursor_without_scrolling(clicked_line, clicked_col);
                            if let Some((start, end)) = ed.whitespace_boundaries_at(clicked_line, clicked_col) {
                                ed.selection_anchor = Some((clicked_line, start));
                                ed.cursor_col = end;
                            } else {
                                ed.select_word_at_cursor();
                            }
                            new_mc_anchor = Some(Some(ed.selection_range().unwrap_or(
                                (clicked_line, clicked_col, clicked_line, clicked_col),
                            )));
                        }
                        _ => {
                            new_mc_anchor = Some(None);
                            // Check if clicking within existing selection (text drag-and-drop)
                            if ed.has_selection()
                                && ed.selection_contains(clicked_line, clicked_col)
                            {
                                app.dragging_text = true;
                                app.dragging_text_start = Some((col, row));
                            } else {
                                ed.extra_cursors.clear();
                                ed.extra_cursor_anchors.clear();
                                ed.clear_selection();
                                ed.set_cursor_without_scrolling(clicked_line, clicked_col);
                                ed.selection_anchor =
                                    Some((ed.cursor_line, ed.cursor_col));
                            }
                        }
                    }
                }
            }
            if let Some(anchor) = new_mc_anchor {
                app.multi_click_anchor = anchor;
            }
            // Ctrl+Click: check for document link first, then go-to-definition
            if ctrl_held && !shift_held {
                let mut opened_link = false;
                if let Some(ed) = app.editor() {
                    let click_line = ed.cursor_line;
                    let click_col = ed.cursor_col;
                    if let Some(link) = app.lsp_document_links.iter().find(|&&(l, sc, ec, _)| {
                        l == click_line && click_col >= sc && click_col < ec
                    }) {
                        let target = link.3.clone();
                        if target.starts_with("file://") {
                            if let Ok(url) = lsp_types::Url::parse(&target) {
                                if let Ok(path) = url.to_file_path() {
                                    let frag_line = url
                                        .fragment()
                                        .and_then(|f| f.strip_prefix('L'))
                                        .and_then(|l| l.parse::<usize>().ok());
                                    app.push_nav_history();
                                    app.open_file(&path);
                                    if let Some(line) = frag_line {
                                        if let Some(ed) = app.editor_mut() {
                                            ed.clear_selection();
                                            ed.set_cursor(line.saturating_sub(1), 0);
                                            ed.center_cursor();
                                        }
                                    }
                                    opened_link = true;
                                }
                            }
                        } else if target.starts_with("http://") || target.starts_with("https://") {
                            app.open_external(crate::external_open::Request::Url(target));
                            opened_link = true;
                        }
                    }
                }
                if !opened_link {
                    app.execute_palette_command("go_to_definition");
                }
            }
            return;
        }
    }

    // Click on scrollbar — scroll to position and start drag
    if let Some(scrollbar_area) = app.scrollbar_area {
        if col >= scrollbar_area.x
            && col < scrollbar_area.x + scrollbar_area.width
            && row >= scrollbar_area.y
            && row < scrollbar_area.y + scrollbar_area.height
        {
            app.dragging_v_scrollbar = true;
            if app.inline_diff_scrollbar_input(row, false, true) {
                return;
            }
            if let Some(ed) = app.editor_mut() {
                let sb_height = scrollbar_area.height as usize;
                if ed.word_wrap {
                    let total_rows = ed.wrapped_total_rows(ed.effective_wrap_width());
                    if sb_height > 0 && total_rows > ed.viewport_height {
                        let click_row = row.saturating_sub(scrollbar_area.y) as usize;
                        let max_scroll =
                            total_rows.saturating_sub(ed.viewport_height.max(1));
                        let target = (click_row * max_scroll) / sb_height;
                        ed.set_wrap_scroll_offset(target.min(max_scroll));
                    }
                } else {
                    let total_lines = ed.line_count();
                    if sb_height > 0 && total_lines > ed.viewport_height {
                        let click_row = row.saturating_sub(scrollbar_area.y) as usize;
                        let max_scroll = total_lines.saturating_sub(ed.viewport_height);
                        ed.scroll_offset = (click_row * max_scroll) / sb_height;
                        ed.scroll_offset = ed.scroll_offset.min(max_scroll);
                    }
                }
            }
            return;
        }
    }

    // Click on horizontal scrollbar — scroll to position and start drag
    if let Some(h_area) = app.h_scrollbar_area {
        if col >= h_area.x
            && col < h_area.x + h_area.width
            && row >= h_area.y
            && row < h_area.y + h_area.height
        {
            app.dragging_h_scrollbar = true;
            if app.inline_diff_scrollbar_input(col, true, true) {
                return;
            }
            let max_line_width = app.cached_max_line_width;
            if let Some(code_area) = app.code_area {
                let visible_width = code_area.width as usize;
                if max_line_width > visible_width {
                    let bar_width = h_area.width as usize;
                    let click_col = (col - h_area.x) as usize;
                    let max_scroll = max_line_width.saturating_sub(visible_width);
                    if let Some(ed) = app.editor_mut() {
                        if !ed.word_wrap {
                            ed.horizontal_scroll =
                                (click_col * max_scroll) / bar_width.max(1);
                            ed.horizontal_scroll = ed.horizontal_scroll.min(max_scroll);
                        }
                    }
                }
            }
            return;
        }
    }

    // Click anywhere in editor area
    if let Some(ed_area) = app.editor_area {
        if col >= ed_area.x
            && col < ed_area.x + ed_area.width
            && row >= ed_area.y
            && row < ed_area.y + ed_area.height
        {
            app.active_pane = ActivePane::Editor;
        }
    }
}

/// Handle middle mouse button down events (split focus, tab close, block select).
pub(crate) fn handle_middle_button_down(app: &mut App, col: u16, row: u16) {
    app.block_drag_cache = None;
    // In split mode, switch focus first so middle-click tab close works in non-focused panes.
    switch_focus_if_split(app, col, row, false);

    // Middle click on tab to close it
    if let Some(tab_area) = app.tab_bar_area {
        if row >= tab_area.y && row < tab_area.y + tab_area.height {
            if let Some(idx) = crate::mouse::find_tab_at_col(app, col, tab_area) {
                app.close_tab(idx);
                return;
            }
        }
    }
    // Middle click in editor: start column/block selection
    if let Some(code_area) = app.code_area {
        if col >= code_area.x
            && col < code_area.x + code_area.width
            && row >= code_area.y
            && row < code_area.y + code_area.height
        {
            if let Some(ed) = app.editor_mut() {
                let (line, actual_col) =
                    crate::mouse_drag::code_area_to_buffer_pos(ed, code_area, col, row);
                let clamped_col = actual_col.min(ed.line_len(line));
                ed.cursor_line = line;
                ed.cursor_col = clamped_col;
                ed.desired_col = None;
                ed.clear_selection();
                ed.extra_cursors.clear();
                ed.extra_cursor_anchors.clear();
                let start_visual_col = if ed.word_wrap {
                    col.saturating_sub(code_area.x) as usize
                } else {
                    ed.horizontal_scroll
                        .saturating_add(col.saturating_sub(code_area.x) as usize)
                };
                app.middle_drag_start = Some((line, actual_col, start_visual_col));
            }
        }
    }
}

/// Handle right mouse button down events (context menus for tabs, source
/// control, explorer, and editor).
pub(crate) fn handle_right_button_down(app: &mut App, col: u16, row: u16) {
    // Right-click on tab bar opens tab context menu
    if let Some(tab_area) = app.tab_bar_area {
        if tab_area.contains((col, row).into()) {
            if let Some(tab_idx) = crate::mouse::find_tab_at_col(app, col, tab_area) {
                app.tab_context_active = true;
                app.tab_context_pos = (col, row + 1);
                app.tab_context_inner = None;
                app.tab_context_tab = tab_idx;
                app.tab_context_selected = 0;
            }
            return;
        }
    }
    // Right-click on source-control changed file opens source-control context menu
    if let Some(exp_area) = app.source_control_detail_area() {
        if app.sidebar_view == crate::app::SidebarView::SourceControl
            && app.source_control_repository_state
                == crate::app::SourceControlRepositoryState::Available
            && col >= exp_area.x
            && col < exp_area.x + exp_area.width
            && row >= exp_area.y
            && row < exp_area.y + exp_area.height
        {
            let rel_y = row.saturating_sub(exp_area.y);
            let (changes_h, _) = crate::ui_source_control::source_control_split_heights(
                exp_area.height,
                app.sc_graph_collapsed,
                app.sc_graph_height,
            );
            if rel_y >= 6 && rel_y < changes_h {
                let list_row = (rel_y - 6) as usize;
                let layout = app.source_control_layout();
                if let Some(crate::source_control_rows::Row::File(file_idx)) =
                    layout.rows(app.source_control_scroll).nth(list_row)
                {
                    app.preserve_source_control_selection(file_idx);
                    if let Some(change) = app.source_control_files.get(file_idx).cloned() {
                        app.open_source_control_context_menu(col, row, change);
                    }
                    return;
                }
            }
        }
    }
    // Right-click in explorer opens explorer context menu
    if let Some(exp_area) = app.explorer_area {
        if app.sidebar_view == crate::app::SidebarView::Explorer
            && col >= exp_area.x
            && col < exp_area.x + exp_area.width
            && row >= exp_area.y
            && row < exp_area.y + exp_area.height
        {
            let (_, tree_area) = crate::ui_chrome::explorer_sections(app, exp_area);
            if tree_area.is_empty() { return; }
            let content = crate::ui_chrome::explorer_tree_content_area(app, exp_area);
            let tree_start = content.y;
            if !content.contains((col, row).into()) {
                app.open_explorer_workspace_context_menu(col, row);
                return;
            }
            let inner_row = (row - tree_start) as usize;
            let visible_height = content.height as usize;
            if let Some(clicked_idx) = app.explorer.entry_at_row(inner_row, visible_height) {
                let clicked_path = app.explorer.entries[clicked_idx].path.clone();
                let keep_multi_selection = app.explorer_multi_selected.len() > 1
                    && app.explorer_multi_selected.iter().any(|p| p == &clicked_path);
                if keep_multi_selection {
                    app.explorer.selected = clicked_idx;
                } else {
                    app.set_explorer_single_selection(clicked_idx);
                }
                app.open_explorer_context_menu(col, row);
            } else {
                app.open_explorer_workspace_context_menu(col, row);
            }
            return;
        }
    }
    if app.should_render_diff_for_active_editor()
        && [app.code_area, app.diff_left_code_area].into_iter().flatten()
            .any(|area| area.contains((col, row).into()))
    {
        app.active_pane = ActivePane::Editor;
        if !app.has_diff_selection() && app.editor().is_none_or(|editor| !editor.has_selection()) {
            app.diff_selection_mouse_down(col, row, KeyModifiers::NONE);
            app.diff_selection_mouse_up(col, row);
        }
        app.context_menu_active = true;
        app.context_menu_items = App::build_context_menu();
        app.refresh_shortcut_hints();
        app.context_menu_pos = (col, row);
        app.context_menu_inner = None;
        app.context_menu_selected = 0;
        return;
    }

    // Right-click opens context menu in editor area (only for file editor tabs)
    let current_kind = app.tabs.get(app.active_tab).map(|t| t.kind);
    let is_file_tab = current_kind == Some(crate::app::TabKind::File);
    if is_file_tab {
        if let Some(code_area) = app.code_area {
        if col >= code_area.x
            && col < code_area.x + code_area.width
            && row >= code_area.y
            && row < code_area.y + code_area.height
        {
            app.active_pane = ActivePane::Editor;
            // Move cursor to right-click position
            if let Some(ed) = app.editor_mut() {
                let (clicked_line, clicked_col) =
                    crate::mouse_drag::code_area_to_buffer_pos(ed, code_area, col, row);
                ed.set_cursor_without_scrolling(clicked_line, clicked_col);
            }
            app.context_menu_active = true;
            app.context_menu_items = App::build_context_menu();
            app.refresh_shortcut_hints();
            app.context_menu_pos = (col, row);
            app.context_menu_inner = None;
            app.context_menu_selected = 0;
        }
        }
    }
}
