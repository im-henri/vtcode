use crate::text_input::TextInputId;
use crate::app::{ActivePane, App};
use crossterm::event::{KeyCode, KeyModifiers};

fn finish_editor_edit(
    app: &mut App,
    previous: Option<crate::app_search_algo::FileSearchEditSnapshot>,
) {
    let Some(previous) = previous else {
        return;
    };
    if !app.refresh_document_feedback_after_edit(&previous) {
        return;
    }
    if let Some(editor) = app.editor_mut() {
        let start = editor.earliest_edit_line();
        let end = editor.latest_edit_line();
        editor.mark_syntax_dirty_range(start, end);
    }

    app.refresh_search_after_edit(&previous);
}

fn should_request_on_type_formatting(app: &App, trigger: &str) -> bool {
    app.on_type_formatting_enabled
        && app.uses_lsp_formatter()
        && trigger != "\n"
        && app
            .lsp_manager
            .on_type_trigger_chars
            .iter()
            .any(|candidate| candidate == trigger)
}

fn request_on_type_formatting(app: &mut App, trigger: &str) {
    if !should_request_on_type_formatting(app, trigger) {
        return;
    }
    let request = app.editor().and_then(|editor| {
        Some((
            editor.file_path.clone()?,
            editor.cursor_line as u32,
            crate::lsp_positions::to_utf16(&editor.rope, editor.cursor_line, editor.cursor_col),
            editor.tab_size as u32,
            !editor.use_tabs,
            editor.content_version,
            editor.rope.clone(),
        ))
    });
    let Some((path, line, col, tab_size, insert_spaces, content_version, rope)) = request else {
        return;
    };

    if app.lsp_manager.has_config_for_file(&path) {
        app.pending_lsp_on_type_format = Some((
            path.clone(),
            line,
            col,
            trigger.to_string(),
            tab_size,
            insert_spaces,
            content_version,
        ));
        app.queue_lsp_document_sync(path, content_version, rope);
    }
}

/// Handle common inline text editing keys (char insert, backspace, delete,
/// cursor movement, selection, ctrl+a, ctrl+backspace, ctrl+delete).
/// Returns true if the key was consumed by inline editing.
#[cfg(test)]
pub(crate) fn handle_inline_edit_key(
    text: &mut String,
    cursor: &mut usize,
    selection_anchor: &mut Option<usize>,
    code: KeyCode,
    modifiers: KeyModifiers,
) -> crate::text_input::TextInputResult {
    crate::text_input::TextInput::new(text, cursor, selection_anchor).handle_key(code, modifiers)
}

pub(crate) fn handle_global_search_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let alt = modifiers.contains(KeyModifiers::ALT);
    let replace_hidden = app.show_explorer
        && app.sidebar_view == crate::app::SidebarView::Search
        && !app.global_search_replace_visible;
    match code {
        KeyCode::Esc => {
            app.push_global_search_history();
            app.global_search_active = false;
        }
        KeyCode::Enter => {
            app.push_global_search_history();
            if ctrl && alt {
                app.replace_all_in_files();
            } else if app.open_global_search_result() {
                app.global_search_active = false;
            }
        }
        KeyCode::Tab => {
            app.search_focus_field = (app.search_focus_field + 1) % 4;
            if replace_hidden && app.search_focus_field == 1 { app.search_focus_field = 2; }
        }
        KeyCode::BackTab => {
            app.search_focus_field = if app.search_focus_field == 0 {
                3
            } else {
                app.search_focus_field - 1
            };
            if replace_hidden && app.search_focus_field == 1 { app.search_focus_field = 0; }
        }
        KeyCode::Up if app.search_focus_field == 0 && modifiers.is_empty() => {
            app.global_search_history_prev();
        }
        KeyCode::Down if app.search_focus_field == 0 && modifiers.is_empty() => {
            app.global_search_history_next();
        }
        KeyCode::Up => {
            app.global_search_select_prev_visible();
            app.reveal_global_search_selection();
        }
        KeyCode::Down => {
            app.global_search_select_next_visible();
            app.reveal_global_search_selection();
        }
        KeyCode::Char('c' | 'C' | 'w' | 'W' | 'r' | 'R' | 'p' | 'P') if alt && !ctrl => {
            use crate::ui_search_controls::{SearchControl, SearchScope, activate};
            let control = match code {
                KeyCode::Char('c' | 'C') => SearchControl::MatchCase,
                KeyCode::Char('w' | 'W') => SearchControl::WholeWord,
                KeyCode::Char('r' | 'R') => SearchControl::Regex,
                _ => SearchControl::PreserveCase,
            };
            activate(app, SearchScope::Workspace, control);
        }
        _ => {
            let field = app.search_focus_field;
            let result = app.edit_text_input(
                TextInputId::GlobalSearch(field),
                code,
                modifiers,
                None,
            );
            if result.changed && field != 1 {
                if field == 0 {
                    app.global_search_history.edited();
                }
                app.update_global_search();
            }
        }
    }
}

pub(crate) fn handle_search_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        KeyCode::Esc => {
            // Save non-empty query to history before closing
            app.push_search_history();
            // Restore pre-search cursor position if no replacement was made
            if let Some((line, col, scroll)) = app.search_pre_cursor.take() {
                if let Some(ed) = app.editor_mut() {
                    ed.cursor_line = line.min(ed.line_count().saturating_sub(1));
                    ed.cursor_col = col;
                    ed.scroll_offset = scroll;
                    ed.clear_selection();
                    ed.ensure_visible();
                }
            }
            app.search_active = false;
            app.search_input_focused = false;
            app.clear_file_search_results();
            app.replace_mode = false;
            app.search_focus_replace = false;
            app.search_in_selection = false;
            app.search_selection_range = None;
        }
        KeyCode::Tab | KeyCode::BackTab => {
            if app.replace_mode {
                app.search_focus_replace = !app.search_focus_replace;
            }
        }
        KeyCode::Char('h' | 'H') if modifiers == KeyModifiers::CONTROL => {
            app.replace_mode = true;
            app.search_focus_replace = true;
        }
        KeyCode::Enter => {
            if modifiers.contains(KeyModifiers::CONTROL)
                && modifiers.contains(KeyModifiers::ALT)
                && app.replace_mode
            {
                app.search_pre_cursor = None;
                app.push_search_history();
                app.replace_all();
            } else if modifiers.contains(KeyModifiers::ALT) {
                // Alt+Enter: select all search matches and close search
                app.push_search_history();
                app.search_pre_cursor = None;
                if let Some(ed) = app.editor_mut() {
                    ed.select_all_occurrences();
                }
                app.search_active = false;
                app.search_input_focused = false;
                app.clear_file_search_results();
                app.replace_mode = false;
                app.search_focus_replace = false;
                app.search_in_selection = false;
                app.search_selection_range = None;
            } else if app.search_focus_replace && app.replace_mode {
                app.search_pre_cursor = None; // Replacement made, don't restore
                app.push_search_history();
                app.replace_current();
            } else if modifiers.contains(KeyModifiers::SHIFT) {
                app.push_search_history();
                app.search_prev();
            } else {
                // Accept search result, clear pre-cursor
                app.search_pre_cursor = None;
                app.push_search_history();
                app.search_next();
            }
        }
        KeyCode::Char('c' | 'C' | 'r' | 'R' | 'w' | 'W' | 'l' | 'L' | 'p' | 'P')
            if modifiers == KeyModifiers::ALT =>
        {
            use crate::ui_search_controls::{SearchControl, SearchScope, activate};
            let control = match code {
                KeyCode::Char('c' | 'C') => SearchControl::MatchCase,
                KeyCode::Char('w' | 'W') => SearchControl::WholeWord,
                KeyCode::Char('r' | 'R') => SearchControl::Regex,
                KeyCode::Char('l' | 'L') => SearchControl::InSelection,
                _ => SearchControl::PreserveCase,
            };
            activate(app, SearchScope::File, control);
        }
        KeyCode::Up if !app.search_focus_replace => {
            app.search_history_prev();
        }
        KeyCode::Down if !app.search_focus_replace => {
            app.search_history_next();
        }
        _ => {
            let result = if app.search_focus_replace {
                app.edit_text_input(
                    TextInputId::SearchReplace,
                    code,
                    modifiers,
                    None,
                )
            } else {
                app.edit_text_input(
                    TextInputId::SearchFind,
                    code,
                    modifiers,
                    None,
                )
            };
            if result.changed && !app.search_focus_replace {
                app.search_history.edited();
                app.update_search();
            }
        }
    }
}

pub(crate) fn handle_explorer_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    if code == KeyCode::Insert {
        let command = match modifiers {
            KeyModifiers::CONTROL => Some("copy"),
            KeyModifiers::SHIFT => Some("paste"),
            _ => None,
        };
        if let Some(command) = command {
            app.execute_palette_command(command);
            return;
        }
    }
    // Extensions sidebar keyboard handling
    if app.sidebar_view == crate::app::SidebarView::Extensions {
        crate::ui_extensions::handle_key(app, code, modifiers);
        return;
    }

    // When source control sidebar is active, handle SC-specific keys
    if app.sidebar_view == crate::app::SidebarView::SourceControl {
        app.source_control_input_mode = false;
        crate::ui_source_control::handle_key(app, code, modifiers);
        return;
    }

    match code {
        KeyCode::Esc => {
            if app.dragging_explorer_path.is_some() || app.explorer_drop_target_idx.is_some() {
                app.dragging_explorer_path = None;
                app.explorer_drop_target_idx = None;
                app.notify("Explorer drag cancelled");
            }
        }
        KeyCode::Up => {
            app.explorer.move_up();
            app.set_explorer_single_selection(app.explorer.selected);
        }
        KeyCode::Down => {
            app.explorer.move_down();
            app.set_explorer_single_selection(app.explorer.selected);
        }
        KeyCode::Enter | KeyCode::Right => {
            if let Some(path) = app.toggle_or_open_explorer_async() {
                // Only non-directory entries are returned, so opening can be
                // queued without a blocking metadata probe here.
                app.open_file(&path);
            }
            app.set_explorer_single_selection(app.explorer.selected);
        }
        KeyCode::Left => {
            app.collapse_explorer_entry_async();
            app.set_explorer_single_selection(app.explorer.selected);
        }
        KeyCode::F(2) if modifiers.is_empty()
            && app.sidebar_view == crate::app::SidebarView::Explorer => {
            if let Some(entry) = app.explorer.entries.get(app.explorer.selected) {
                let initial = entry
                    .path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                app.begin_rename(entry.path.clone(), initial, true, false);
            }
        }
        KeyCode::Delete if app.show_explorer
            && app.sidebar_view == crate::app::SidebarView::Explorer => {
            app.request_explorer_selection_delete();
        }
        _ => {}
    }
}

pub(crate) fn handle_editor_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    let shift = modifiers.contains(KeyModifiers::SHIFT);
    let alt = modifiers.contains(KeyModifiers::ALT);

    // End clipboard cycle on any key that isn't Ctrl+Shift+V
    if app.clipboard_cycle_active {
        let is_cycle_key = ctrl && shift && matches!(code, KeyCode::Char('V' | 'v'));
        if !is_cycle_key {
            app.clipboard_cycle_active = false;
        }
    }

    // Dispatch keyboard events to extension overlays with key handlers
    if app.handle_overlay_key_event(code, modifiers) {
        return;
    }

    if app.active_extension_view().is_some() {
        if app.handle_active_extension_view_key(code, modifiers) {
            return;
        }
        if !ctrl
            && !alt
            && matches!(
                code,
                KeyCode::Char(_)
                    | KeyCode::Enter
                    | KeyCode::Tab
                    | KeyCode::BackTab
                    | KeyCode::Backspace
                    | KeyCode::Delete
                    | KeyCode::Insert
            )
        {
            return;
        }
    }

    if app.markdown_preview_selection_key(code, modifiers) {
        return;
    }
    if app.diff_selection_key(code, modifiers) { return; }
    // Original/deleted comparison text never sends edits or extra carets to
    // the target editor's unrelated native cursor.
    if app.diff_selection_owns_cursor()
        && ((ctrl && shift && matches!(code, KeyCode::Char('V' | 'v')))
            || (alt && matches!(code, KeyCode::Up | KeyCode::Down)))
    {
        return;
    }
    if app.context_diff_navigation(code, modifiers) {
        return;
    }
    if modifiers.is_empty() && app.inline_diff_navigation(code) {
        return;
    }
    if modifiers.is_empty() && app.editor_review_navigation(code) {
        return;
    }
    // Ctrl+Alt+Up/Down for column/multi-cursor
    if ctrl && alt {
        match code {
            KeyCode::Up => {
                if let Some(ed) = app.editor_mut() {
                    ed.add_cursor_above();
                }
                return;
            }
            KeyCode::Down => {
                if let Some(ed) = app.editor_mut() {
                    ed.add_cursor_below();
                }
                return;
            }
            _ => {}
        }
    }

    if !alt && !shift && (!ctrl || matches!(code, KeyCode::Home | KeyCode::End))
        && app.markdown_preview_navigation(code) {
        return;
    }
    // Ctrl+Up/Down for paragraph movement
    if ctrl && !alt {
        match code {
            KeyCode::Up => {
                if let Some(ed) = app.editor_mut() {
                    if shift {
                        ed.start_selection();
                    } else {
                        ed.clear_selection();
                    }
                    ed.desired_col = None;
                    ed.move_paragraph_up();
                }
                return;
            }
            KeyCode::Down => {
                if let Some(ed) = app.editor_mut() {
                    if shift {
                        ed.start_selection();
                    } else {
                        ed.clear_selection();
                    }
                    ed.desired_col = None;
                    ed.move_paragraph_down();
                }
                return;
            }
            _ => {}
        }
    }

    // Alt+Arrow for line movement, Alt+Z for word wrap, Alt+key for common actions
    if alt {
        let is_read_only = app.editor().is_some_and(|e| e.read_only)
            || app.diff_selection_owns_cursor();
        match code {
            KeyCode::Up => {
                if is_read_only {
                    return;
                }
                if shift {
                    if let Some(ed) = app.editor_mut() {
                        ed.duplicate_line_up();
                    }
                } else if let Some(ed) = app.editor_mut() {
                    ed.move_line_up();
                }
                return;
            }
            KeyCode::Down => {
                if is_read_only {
                    return;
                }
                if shift {
                    if let Some(ed) = app.editor_mut() {
                        ed.duplicate_line();
                    }
                } else if let Some(ed) = app.editor_mut() {
                    ed.move_line_down();
                }
                return;
            }
            KeyCode::Char('z') => {
                app.execute_palette_command("toggle_word_wrap");
                return;
            }
            KeyCode::Char(c @ '1'..='9') => {
                let tab_idx = (c as usize) - ('1' as usize);
                if tab_idx < app.tabs.len() {
                    app.active_tab = tab_idx;
                    app.active_pane = ActivePane::Editor;
                    app.validate_diff_state_for_active_tab();
                }
                return;
            }
            KeyCode::Left => {
                if shift {
                    if let Some(ed) = app.editor_mut() {
                        ed.shrink_selection();
                    }
                }
                return;
            }
            KeyCode::Right => {
                if shift {
                    if let Some(ed) = app.editor_mut() {
                        ed.expand_selection();
                    }
                }
                return;
            }
            KeyCode::Char('d' | 'D') => {
                if !is_read_only {
                    if let Some(ed) = app.editor_mut() {
                        ed.delete_line();
                    }
                }
                return;
            }
            KeyCode::Char('t' | 'T') => {
                app.reopen_closed_tab();
                return;
            }
            KeyCode::Enter => {
                if !is_read_only {
                    if let Some(ed) = app.editor_mut() {
                        ed.insert_line_above();
                    }
                }
                return;
            }
            KeyCode::Char('\\') => {
                app.execute_palette_command("goto_bracket");
                return;
            }
            KeyCode::Char('i' | 'I') => {
                if shift {
                    if let Some(ed) = app.editor_mut() {
                        ed.add_cursors_to_line_ends();
                    }
                }
                return;
            }
            KeyCode::Char('/') => {
                if !is_read_only {
                    if let Some(ed) = app.editor_mut() {
                        ed.toggle_block_comment();
                    }
                }
                return;
            }
            KeyCode::Char('[') => {
                if let Some(ed) = app.editor_mut() {
                    let line = ed.cursor_line;
                    if ed.find_fold_end(line).is_some() && !ed.is_fold_start(line) {
                        ed.toggle_fold(line);
                    }
                }
                return;
            }
            KeyCode::Char(']') => {
                if let Some(ed) = app.editor_mut() {
                    let line = ed.cursor_line;
                    if ed.is_fold_start(line) {
                        ed.toggle_fold(line);
                    }
                }
                return;
            }
            KeyCode::Char('f' | 'F') => {
                app.push_global_search_history();
                app.global_search_active = true;
                app.global_search_query.clear();
                app.global_search_replace_query.clear();
                app.global_search_results.clear();
                app.global_search_truncated = false;
                app.global_search_selected = 0;
                app.global_search_scroll = 0;
                app.search_focus_field = 0;
                app.global_search_field_cursors = [
                    app.global_search_query.len(),
                    app.global_search_replace_query.len(),
                    app.search_include_pattern.len(),
                    app.search_exclude_pattern.len(),
                ];
                app.global_search_field_selection_anchors = [None; 4];
                return;
            }
            KeyCode::Char('h' | 'H') => {
                // Ctrl+Shift+H: global search with replace focused
                app.push_global_search_history();
                app.global_search_active = true;
                app.global_search_replace_visible = true;
                app.show_explorer = true;
                app.sidebar_view = crate::app::SidebarView::Search;
                app.active_pane = ActivePane::Explorer;
                app.search_input_focused = false;
                app.global_search_query.clear();
                app.global_search_replace_query.clear();
                app.global_search_results.clear();
                app.global_search_truncated = false;
                app.global_search_selected = 0;
                app.global_search_scroll = 0;
                app.search_focus_field = 1;
                app.global_search_field_cursors = [
                    app.global_search_query.len(),
                    app.global_search_replace_query.len(),
                    app.search_include_pattern.len(),
                    app.search_exclude_pattern.len(),
                ];
                app.global_search_field_selection_anchors = [None; 4];
                return;
            }
            KeyCode::Char('e' | 'E') => {
                app.show_explorer = true;
                app.active_pane = ActivePane::Explorer;
                return;
            }
            KeyCode::Char('o' | 'O') => {
                app.execute_palette_command("switch_header_source");
                return;
            }
            KeyCode::Char('l' | 'L') => {
                // Select all occurrences with proper multi-cursor selection
                if let Some(ed) = app.editor_mut() {
                    ed.select_all_occurrences();
                    let count = 1 + ed.extra_cursors.len();
                    app.notify(format!("{count} occurrences selected"));
                }
                return;
            }
            KeyCode::Char('u' | 'U') => {
                if let Some(ed) = app.editor_mut() {
                    ed.toggle_case();
                }
                return;
            }
            KeyCode::Char('m' | 'M') => {
                crate::ui_panels::toggle_problems(app);
                return;
            }
            KeyCode::Char('v' | 'V') => {
                app.toggle_markdown_preview_tab();
                return;
            }
            KeyCode::Char('r' | 'R') => {
                if app.macro_recording {
                    app.execute_palette_command("macro_stop");
                } else {
                    app.execute_palette_command("macro_start");
                }
                return;
            }
            _ => {}
        }
    }

    // Ctrl+Shift combos (kept for compatibility, Alt+key alternatives exist)
    if ctrl && shift {
        match code {
            KeyCode::Char('S' | 's') => {
                app.execute_palette_command("save_as");
                return;
            }
            KeyCode::Char('T' | 't') => {
                // Ctrl+Shift+T: reopen closed tab
                app.reopen_closed_tab();
                return;
            }
            KeyCode::Char('`') => {
                // Ctrl+Shift+`: create a terminal in the default right dock.
                app.execute_palette_command("new_terminal");
                return;
            }
            KeyCode::Char(' ') => {
                // Ctrl+Shift+Space: show parameter hints.
                app.request_lsp_suggestions(true);
                return;
            }
            KeyCode::Char('V' | 'v') => {
                // Ctrl+Shift+V: cycle paste from clipboard history
                if app.editor().is_none_or(|ed| ed.read_only) {
                    return;
                }
                if app.clipboard_history.is_empty() {
                    app.notify("Clipboard history is empty");
                    return;
                }
                if app.paste_in_progress() { app.notify_warn("Wait for the current paste or cancel it first"); return; }
                let history_len = app.clipboard_history.len();
                if app.clipboard_cycle_active {
                    // Undo previous cycle paste, then paste next older entry
                    if let Some(ed) = app.editor_mut() {
                        ed.undo();
                    }
                    app.clipboard_cycle_index = (app.clipboard_cycle_index + 1) % history_len;
                } else {
                    // Start new cycle from most recent entry
                    app.clipboard_cycle_active = true;
                    app.clipboard_cycle_index = 0;
                }
                let real_idx = history_len - 1 - app.clipboard_cycle_index;
                if let Some(text) = app.clipboard_history.get(real_idx).cloned() {
                    app.paste_editor_text(text, true);
                }
                return;
            }
            KeyCode::Enter => {
                if let Some(ed) = app.editor_mut() {
                    ed.insert_line_above();
                }
                return;
            }
            KeyCode::Char('\\') => {
                app.execute_palette_command("goto_bracket");
                return;
            }
            KeyCode::Char('[') => {
                app.execute_palette_command("fold");
                return;
            }
            KeyCode::Char(']') => {
                app.execute_palette_command("unfold");
                return;
            }
            KeyCode::Backspace => {
                if let Some(ed) = app.editor_mut() {
                    if !ed.read_only {
                        ed.delete_to_line_start();
                    }
                }
                return;
            }
            KeyCode::Delete => {
                if let Some(ed) = app.editor_mut() {
                    if !ed.read_only {
                        ed.delete_to_line_end();
                    }
                }
                return;
            }
            KeyCode::PageUp => {
                // Ctrl+Shift+PageUp = move tab left
                if app.active_tab > 0 {
                    app.swap_tabs(app.active_tab, app.active_tab - 1);
                    app.active_tab -= 1;
                    app.ensure_active_tab_visible();
                }
                return;
            }
            KeyCode::PageDown => {
                // Ctrl+Shift+PageDown = move tab right
                if app.active_tab + 1 < app.tabs.len() {
                    app.swap_tabs(app.active_tab, app.active_tab + 1);
                    app.active_tab += 1;
                    app.ensure_active_tab_visible();
                }
                return;
            }
            _ => {}
        }
    }

    // Ctrl+key combinations
    if ctrl {
        match code {
            KeyCode::Enter => {
                if let Some(ed) = app.editor_mut() {
                    ed.insert_line_below();
                }
                return;
            }
            KeyCode::Char('c') => {
                if app.copy_diff_selection() { return; }
                app.copy_editor_selection();
                let clip = app.editor().map(|ed| ed.clipboard.clone());
                if let Some(text) = clip { app.push_clipboard_history(&text); }
                return;
            }
            KeyCode::Char('x') => {
                if (app.editor().is_some_and(|editor| editor.read_only)
                    || app.diff_selection_owns_cursor()) && app.copy_diff_selection() {
                    return;
                }
                if let Some(ed) = app.editor_mut() {
                    if ed.read_only {
                        ed.copy();
                    } else {
                        ed.cut();
                    }
                    if ed.clipboard_failed {
                        app.notify(
                            "System clipboard unavailable — copied to internal clipboard only",
                        );
                    }
                }
                let clip = app.editor().map(|ed| ed.clipboard.clone());
                if let Some(text) = clip {
                    app.push_clipboard_history(&text);
                }
                return;
            }
            KeyCode::Char('v') => {
                if app.diff_selection_owns_cursor() { return; }
                app.paste_editor_clipboard(true);
                return;
            }
            KeyCode::Char('a') => {
                if let Some(ed) = app.editor_mut() {
                    ed.select_all();
                }
                return;
            }
            KeyCode::Left => {
                if let Some(ed) = app.editor_mut() {
                    if ed.extra_cursors.is_empty() {
                        if shift {
                            ed.start_selection();
                        } else {
                            ed.clear_selection();
                        }
                        ed.move_word_left();
                    } else if shift {
                        ed.multi_cursor_select_move_word_left();
                    } else {
                        ed.multi_cursor_move_word_left();
                    }
                }
                return;
            }
            KeyCode::Right => {
                if let Some(ed) = app.editor_mut() {
                    if ed.extra_cursors.is_empty() {
                        if shift {
                            ed.start_selection();
                        } else {
                            ed.clear_selection();
                        }
                        ed.move_word_right();
                    } else if shift {
                        ed.multi_cursor_select_move_word_right();
                    } else {
                        ed.multi_cursor_move_word_right();
                    }
                }
                return;
            }
            KeyCode::Backspace => {
                if let Some(ed) = app.editor_mut() {
                    if !ed.read_only {
                        if ed.extra_cursors.is_empty() {
                            ed.delete_word_left();
                        } else {
                            ed.multi_cursor_delete_word_left();
                        }
                    }
                }
                return;
            }
            KeyCode::Delete => {
                if let Some(ed) = app.editor_mut() {
                    if !ed.read_only {
                        if ed.extra_cursors.is_empty() {
                            ed.delete_word_right();
                        } else {
                            ed.multi_cursor_delete_word_right();
                        }
                    }
                }
                return;
            }
            KeyCode::Home => {
                if let Some(ed) = app.editor_mut() {
                    if shift {
                        ed.start_selection();
                    } else {
                        ed.clear_selection();
                    }
                    ed.move_to_top();
                }
                return;
            }
            KeyCode::End => {
                if let Some(ed) = app.editor_mut() {
                    if shift {
                        ed.start_selection();
                    } else {
                        ed.clear_selection();
                    }
                    ed.move_to_bottom();
                }
                return;
            }
            KeyCode::Char('K' | 'k') if shift => {
                // Ctrl+Shift+K: delete line (VS Code standard)
                app.execute_palette_command("delete_line");
                return;
            }
            KeyCode::Char('k') => {
                // Start Ctrl+K chord (only without Shift)
                app.chord_pending = true;
                app.chord_start = std::time::Instant::now();
                app.notify("Ctrl+K ...");
                return;
            }
            KeyCode::Up => {
                // Ctrl+Up: scroll view up one line, keep cursor visible
                if let Some(ed) = app.editor_mut() {
                    ed.scroll_up(1);
                    if !ed.word_wrap {
                        let visible_lines = ed.viewport_height.max(1);
                        if ed.cursor_line >= ed.scroll_offset + visible_lines {
                            ed.cursor_line = ed.scroll_offset + visible_lines - 1;
                            let line_len =
                                ed.rope.line(ed.cursor_line).len_chars().saturating_sub(1);
                            if ed.cursor_col > line_len {
                                ed.cursor_col = line_len;
                            }
                        }
                    }
                }
                return;
            }
            KeyCode::Down => {
                // Ctrl+Down: scroll view down one line, keep cursor visible
                if let Some(ed) = app.editor_mut() {
                    ed.scroll_down(1);
                    if !ed.word_wrap && ed.cursor_line < ed.scroll_offset {
                        ed.cursor_line = ed.scroll_offset;
                        let line_len = ed.rope.line(ed.cursor_line).len_chars().saturating_sub(1);
                        if ed.cursor_col > line_len {
                            ed.cursor_col = line_len;
                        }
                    }
                }
                return;
            }
            KeyCode::Char('t') => {
                // Ctrl+T: workspace symbol search
                app.symbol_list_workspace = true;
                app.symbol_list_active = true;
                app.symbol_list_query.clear();
                app.symbol_list_cursor = 0;
                app.symbol_list_selection_anchor = None;
                app.build_symbol_list();
                return;
            }
            KeyCode::Char(' ') => {
                // Ctrl+Space: request the language server even with automatic suggestions disabled.
                app.update_vtcomplete_explicit();
                if app.request_lsp_suggestions(false) {
                    app.vtcomplete_active = false;
                    app.completion_ui.explicit = true;
                }
                return;
            }
            _ => {
                return;
            }
        }
    }

    // Shift+Arrow for selection
    if shift {
        match code {
            KeyCode::Up => {
                if let Some(ed) = app.editor_mut() {
                    if ed.extra_cursors.is_empty() {
                        ed.start_selection();
                        ed.move_up();
                    } else {
                        ed.multi_cursor_select_move_up();
                    }
                }
                return;
            }
            KeyCode::Down => {
                if let Some(ed) = app.editor_mut() {
                    if ed.extra_cursors.is_empty() {
                        ed.start_selection();
                        ed.move_down();
                    } else {
                        ed.multi_cursor_select_move_down();
                    }
                }
                return;
            }
            KeyCode::Left => {
                if let Some(ed) = app.editor_mut() {
                    if ed.extra_cursors.is_empty() {
                        ed.start_selection();
                        ed.move_left();
                    } else {
                        ed.multi_cursor_select_move_left();
                    }
                }
                return;
            }
            KeyCode::Right => {
                if let Some(ed) = app.editor_mut() {
                    if ed.extra_cursors.is_empty() {
                        ed.start_selection();
                        ed.move_right();
                    } else {
                        ed.multi_cursor_select_move_right();
                    }
                }
                return;
            }
            KeyCode::Home => {
                if let Some(ed) = app.editor_mut() {
                    if ed.extra_cursors.is_empty() {
                        ed.start_selection();
                        ed.move_to_line_start();
                    } else {
                        ed.multi_cursor_select_move_to_line_start();
                    }
                }
                return;
            }
            KeyCode::End => {
                if let Some(ed) = app.editor_mut() {
                    if ed.extra_cursors.is_empty() {
                        ed.start_selection();
                        ed.move_to_line_end();
                    } else {
                        ed.multi_cursor_select_move_to_line_end();
                    }
                }
                return;
            }
            _ => {}
        }
    }

    // Regular keys — clear selection on movement, replace selection on typing
    // Read-only guard for editing operations
    let is_read_only = app.editor().is_some_and(|e| e.read_only)
        || app.diff_selection_owns_cursor();
    match code {
        KeyCode::Char(c) => {
            if is_read_only {
                app.notify_warn("File is read-only");
                return;
            }
            let previous_version = app.file_search_edit_snapshot();
            app.pin_preview_tab();
            app.record_edit_position();
            if let Some(ed) = app.editor_mut() {
                if !ed.extra_cursors.is_empty() {
                    ed.multi_cursor_insert_char(c);
                } else if ed.has_selection() && matches!(c, '(' | '[' | '{' | '"' | '\'' | '`') {
                    let closing = match c {
                        '(' => ')',
                        '[' => ']',
                        '{' => '}',
                        _ => c,
                    };
                    ed.surround_selection(&c.to_string(), &closing.to_string());
                } else {
                    ed.insert_char_replacing_selection(c);
                }
            }
            finish_editor_edit(app, previous_version);
            // Dismiss ghost text and restart debounce timer on typing
            if (c.is_alphanumeric() || c == '_' || c == '$') && (app.quick_suggestions || app.vtcomplete_explicit || app.lsp_completion_active) {
                app.update_vtcomplete();
                let lsp_completion_requested = app.request_lsp_suggestions(false);
                if lsp_completion_requested {
                    app.vtcomplete_active = false;
                }
            } else if c == '.' || c == ':' || c == '<' {
                app.request_lsp_suggestions(false);
                app.update_vtcomplete();
            } else if c == '(' || c == ',' {
                app.request_lsp_suggestions(true);
                app.update_vtcomplete();
                app.lsp_completion_active = false;
            } else if c == ')' {
                app.lsp_signature_active = false;
                app.dismiss_vtcomplete();
                app.lsp_completion_active = false;
            } else {
                if matches!(c, '&' | '*' | '=' | '>') {
                    app.update_vtcomplete();
            } else {
                    app.dismiss_vtcomplete();
                }
                app.lsp_completion_active = false;
            }
            let mut trigger = [0; 4];
            request_on_type_formatting(app, c.encode_utf8(&mut trigger));
        }
        KeyCode::Enter => {
            if is_read_only {
                return;
            }
            let previous_version = app.file_search_edit_snapshot();
            app.pin_preview_tab();
            app.record_edit_position();
            app.dismiss_vtcomplete();
            if let Some(ed) = app.editor_mut() {
                if ed.extra_cursors.is_empty() {
                    ed.insert_newline_replacing_selection();
                } else {
                    ed.multi_cursor_insert_char('\n');
                }
            }
            finish_editor_edit(app, previous_version);
        }
        KeyCode::Backspace => {
            if is_read_only {
                return;
            }
            let previous_version = app.file_search_edit_snapshot();
            app.record_edit_position();
            if let Some(ed) = app.editor_mut() {
                if ed.extra_cursors.is_empty() {
                    ed.backspace_replacing_selection();
                } else {
                    ed.multi_cursor_backspace();
                }
            }
            finish_editor_edit(app, previous_version);
            app.update_vtcomplete();
            if (app.quick_suggestions || app.lsp_completion_active) && app.editor().is_some_and(|editor| editor.has_word_prefix_at_cursor()) {
                if app.request_lsp_suggestions(false) { app.vtcomplete_active = false; }
            } else {
                app.lsp_completion_active = false;
            }
        }
        KeyCode::Delete => {
            if is_read_only {
                return;
            }
            let previous_version = app.file_search_edit_snapshot();
            if let Some(ed) = app.editor_mut() {
                if ed.extra_cursors.is_empty() {
                    ed.delete_replacing_selection();
                } else {
                    ed.multi_cursor_delete();
                }
            }
            finish_editor_edit(app, previous_version);
        }
        KeyCode::Tab => {
            if is_read_only {
                return;
            }
            let previous_version = app.file_search_edit_snapshot();
            if let Some(ed) = app.editor_mut() {
                if ed.snippet_active && ed.snippet_next_tab_stop() {
                    return;
                }
                if !ed.extra_cursors.is_empty() {
                    ed.multi_cursor_insert_tab();
                } else if shift {
                    ed.dedent_line();
                } else if ed.has_selection() {
                    ed.indent_lines();
                } else {
                    ed.insert_tab_replacing_selection();
                }
            }
            finish_editor_edit(app, previous_version);
        }
        KeyCode::BackTab => {
            if is_read_only {
                return;
            }
            let previous_version = app.file_search_edit_snapshot();
            if let Some(ed) = app.editor_mut() {
                if ed.snippet_active && ed.snippet_prev_tab_stop() {
                    return;
                }
                ed.dedent_line();
            }
            finish_editor_edit(app, previous_version);
        }
        KeyCode::Insert if modifiers.is_empty() => {
            if let Some(ed) = app.editor_mut() {
                ed.overwrite_mode = !ed.overwrite_mode;
            }
        }
        KeyCode::Up => {
            if let Some(ed) = app.editor_mut() {
                if ed.extra_cursors.is_empty() {
                    ed.clear_selection();
                    ed.move_up();
                } else {
                    ed.multi_cursor_move_up();
                }
            }
        }
        KeyCode::Down => {
            if let Some(ed) = app.editor_mut() {
                if ed.extra_cursors.is_empty() {
                    ed.clear_selection();
                    ed.move_down();
                } else {
                    ed.multi_cursor_move_down();
                }
            }
        }
        KeyCode::Left => {
            if let Some(ed) = app.editor_mut() {
                if ed.extra_cursors.is_empty() {
                    ed.clear_selection();
                    ed.move_left();
                } else {
                    ed.multi_cursor_move_left();
                }
            }
        }
        KeyCode::Right => {
            if let Some(ed) = app.editor_mut() {
                if ed.extra_cursors.is_empty() {
                    ed.clear_selection();
                    ed.move_right();
                } else {
                    ed.multi_cursor_move_right();
                }
            }
        }
        KeyCode::Home => {
            if let Some(ed) = app.editor_mut() {
                if ed.extra_cursors.is_empty() {
                    if shift {
                        ed.start_selection();
                    } else {
                        ed.clear_selection();
                    }
                    ed.move_to_line_start();
                } else {
                    ed.multi_cursor_move_to_line_start();
                }
            }
        }
        KeyCode::End => {
            if let Some(ed) = app.editor_mut() {
                if ed.extra_cursors.is_empty() {
                    if shift {
                        ed.start_selection();
                    } else {
                        ed.clear_selection();
                    }
                    ed.move_to_line_end();
                } else {
                    ed.multi_cursor_move_to_line_end();
                }
            }
        }
        KeyCode::PageUp => {
            if let Some(ed) = app.editor_mut() {
                if shift {
                    ed.start_selection();
                } else {
                    ed.clear_selection();
                }
                ed.page_up();
            }
        }
        KeyCode::PageDown => {
            if let Some(ed) = app.editor_mut() {
                if shift {
                    ed.start_selection();
                } else {
                    ed.clear_selection();
                }
                ed.page_down();
            }
        }
        KeyCode::F(5) => {
            if alt && shift {
                // Alt+Shift+F5: Go to previous change
                if let Some(ed) = app.editor_mut() {
                    ed.prev_change();
                }
            } else if alt {
                // Alt+F5: Go to next change
                if let Some(ed) = app.editor_mut() {
                    ed.next_change();
                }
            }
        }
        KeyCode::F(12) => {
            if alt {
                // Alt+F12: Peek definition
                app.open_peek_definition();
            }
        }
        KeyCode::F(2) => {
            if ctrl {
                if let Some(ed) = app.editor_mut() {
                    ed.toggle_bookmark();
                }
                let count = app.editor().map_or(0, |e| e.bookmarks.len());
                app.notify(format!("Bookmarks: {count}"));
            } else if shift {
                app.execute_palette_command("prev_bookmark");
            }
        }
        KeyCode::Esc => {
            // Close peek, then search, then snippet mode, then multi-cursors, then selection
            if app.peek_definition.is_some() {
                app.peek_definition = None;
            } else if app.search_active {
                app.search_active = false;
                app.search_input_focused = false;
                app.clear_file_search_results();
            } else if let Some(ed) = app.editor_mut() {
                if ed.snippet_active {
                    ed.snippet_cancel();
                } else if !ed.extra_cursors.is_empty() {
                    ed.extra_cursors.clear();
                    ed.extra_cursor_anchors.clear();
                } else if ed.has_selection() {
                    ed.clear_selection();
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, Tab};
    use crate::editor::Editor;

    #[test]
    fn enter_never_requests_on_type_formatting() {
        let mut app = App::test_new();
        app.on_type_formatting_enabled = true;
        app.lsp_manager.on_type_trigger_chars.push("\n".to_string());

        assert!(!should_request_on_type_formatting(&app, "\n"));
    }

    #[test]
    fn enter_keeps_allman_header_statement_and_closing_brace_indentation() {
        let source = "    if (something)\n    {\n        work();\n    }";
        for (line, expected, caret) in [
            (
                0,
                "    if (something)\n    \n    {\n        work();\n    }",
                (1, 4),
            ),
            (
                2,
                "    if (something)\n    {\n        work();\n        \n    }",
                (3, 8),
            ),
            (
                3,
                "    if (something)\n    {\n        work();\n    }\n    ",
                (4, 4),
            ),
        ] {
            let mut app = App::test_new();
            let mut editor = Editor::test_with_text(source);
            editor.language_override = Some("C".into());
            editor.cursor_line = line;
            editor.cursor_col = editor.line_len(line);
            app.tabs.push(Tab::new_file(editor));
            app.active_tab = 0;
            app.active_pane = ActivePane::Editor;
            handle_editor_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let editor = app.editor_mut().unwrap();
            assert_eq!(editor.rope.to_string(), expected, "line {line}");
            assert_eq!((editor.cursor_line, editor.cursor_col), caret);
            editor.undo();
            assert_eq!(editor.rope.to_string(), source);
        }
    }

    #[test]
    fn editing_with_visible_unfocused_find_updates_matches_without_a_full_rescan() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("needle\nkeep needle");
        editor.cursor_line = 0;
        editor.cursor_col = 6;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.active_pane = ActivePane::Editor;
        app.search_active = true;
        app.search_input_focused = false;
        app.search_query = "needle".into();
        app.update_search();
        assert_eq!(app.search_matches, vec![(0, 0, 6), (1, 5, 11)]);

        handle_editor_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);

        assert_eq!(app.search_matches, vec![(1, 5, 11)]);
        assert!(!app.file_search_refresh_pending);
    }

    #[test]
    fn newline_edit_shifts_untouched_find_matches_incrementally() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("needle\none\ntwo\nthree\nfour\nbottom needle");
        editor.cursor_line = 2;
        editor.cursor_col = 0;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.active_pane = ActivePane::Editor;
        app.search_active = true;
        app.search_input_focused = false;
        app.search_query = "needle".into();
        app.update_search();
        assert_eq!(app.search_matches, vec![(0, 0, 6), (5, 7, 13)]);

        handle_editor_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

        assert_eq!(app.search_matches, vec![(0, 0, 6), (6, 7, 13)]);
        assert!(!app.file_search_refresh_pending);
    }

    #[test]
    fn pathological_line_defers_find_rescan_without_clearing_highlights() {
        let text = format!("needle{}", "x".repeat(300_000));
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(&text);
        editor.cursor_line = 0;
        editor.cursor_col = 6;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.active_pane = ActivePane::Editor;
        app.search_active = true;
        app.search_input_focused = false;
        app.search_query = "needle".into();
        app.update_search();
        assert_eq!(app.search_matches, vec![(0, 0, 6)]);

        handle_editor_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);

        // Keep the last complete result visible until the bounded debounce
        // expires instead of blanking every highlight between keypresses.
        assert_eq!(app.search_matches, vec![(0, 0, 6)]);
        assert!(app.file_search_refresh_pending);

        app.last_edit_time = std::time::Instant::now() - std::time::Duration::from_millis(100);
        assert!(app.refresh_pending_file_search());
        assert!(app.search_matches.is_empty());
        assert!(!app.file_search_refresh_pending);
    }

    #[test]
    fn configured_printable_trigger_can_request_opt_in_formatting() {
        let mut app = App::test_new();
        app.tabs = vec![Tab::new_file(Editor::test_with_text(""))];
        app.active_tab = 0;
        let language = crate::formatters::language(app.editor().unwrap());
        app.formatters.insert(language.clone(), Default::default());
        app.on_type_formatting_enabled = true;
        app.lsp_manager.on_type_trigger_chars.push("}".to_string());

        assert!(should_request_on_type_formatting(&app, "}"));
        app.formatters.get_mut(&language).unwrap().provider = crate::formatters::Provider::Command;
        assert!(!should_request_on_type_formatting(&app, "}"));
        app.formatters.clear();
        assert!(!should_request_on_type_formatting(&app, "}"));
    }

    #[test]
    fn typing_invalidates_feedback_requests_but_keeps_visual_caches_until_refresh() {
        let mut app = App::test_new();
        let path = std::path::PathBuf::from("sample.cpp");
        let mut editor = Editor::test_with_text("int value;\n");
        editor.file_path = Some(path.clone());
        editor.cursor_col = 3;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.lsp_manager.diagnostics.insert(path.clone(), Vec::new());
        app.lsp_semantic_tokens
            .push(crate::lsp::DecodedSemanticToken {
                line: 0,
                start_char: 0,
                length: 3,
                token_type: 0,
                _token_modifiers: 0,
            });
        app.lsp_document_highlights.push((0, 0, 0, 3));
        app.lsp_inlay_hints.push(crate::app::InlayHintDisplay::new(0, 3, ": i32", None));

        handle_editor_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);

        assert!(app.lsp_manager.get_diagnostics(&path).is_some());
        assert!(app.lsp_manager.diagnostic_dirty_documents.contains(&path));
        assert_eq!(app.lsp_semantic_tokens.len(), 1);
        assert_eq!(app.lsp_document_highlights, vec![(0, 0, 0, 3)]);
        assert_eq!(app.lsp_inlay_hints, vec![crate::app::InlayHintDisplay::new(0, 3, ": i32", None)]);
        assert_eq!(app.lsp_semantic_tokens_last_version, u64::MAX);
    }

    #[test]
    fn typing_identifier_keeps_builtin_vtcomplete_visible_without_lsp() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("pri print\n");
        editor.cursor_line = 0;
        editor.cursor_col = 3;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;

        handle_editor_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
        app.wait_for_background(std::time::Duration::from_secs(2), |app| {
            app.vtcomplete_active
        });

        assert!(app.vtcomplete_active);
        assert!(app
            .vtcomplete_suggestions
            .iter()
            .any(|suggestion| suggestion == "print"));
        assert_eq!(app.tabs[0].editor.rope.to_string(), "prin print\n");
    }

    #[test]
    fn references_and_single_character_prefixes_trigger_context_completions() {
        for suffix in ["&", "&s", "&mut s"] {
            let mut app = App::test_new();
            let text = "let source_value = 1;\nlet previous = &source_value;\nlet borrowed = ";
            let mut editor = Editor::test_with_text(text);
            editor.language_override = Some("Rust".into());
            editor.cursor_line = 2;
            editor.cursor_col = "let borrowed = ".len();
            app.tabs.push(Tab::new_file(editor));
            app.active_tab = 0;
            for ch in suffix.chars() {
                handle_editor_key(&mut app, KeyCode::Char(ch), KeyModifiers::NONE);
            }
            app.wait_for_background(std::time::Duration::from_secs(3), |app| {
                app.vtcomplete_active
            });
            assert!(
                app.vtcomplete_suggestions
                    .iter()
                    .any(|value| value == "source_value"),
                "{suffix}: {:?}",
                app.vtcomplete_suggestions
            );
            assert!(!app.vtcomplete_explicit);
            app.vtcomplete_selected = app.vtcomplete_suggestions.iter()
                .position(|value| value == "source_value").unwrap();
            crate::input_keyboard::handle_keyboard_event(
                &mut app,
                crossterm::event::KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            );
            let expected = if suffix == "&mut s" {
                "let borrowed = &mut source_value"
            } else {
                "let borrowed = &source_value"
            };
            assert!(app.editor().unwrap().rope.to_string().ends_with(expected),
                "{suffix}: {:?}", app.editor().unwrap().rope.to_string());
        }
    }

    #[test]
    fn structural_triggers_are_configurable_and_stay_quiet_in_comments() {
        for (text, enabled, expected) in [
            (
                "let source_value = 1;\nlet previous = &source_value;\nlet value = ",
                false,
                false,
            ),
            ("let source_value = 1;\n// ", true, false),
            (
                "let source_value = 1;\nlet previous = &source_value;\nlet value = ",
                true,
                true,
            ),
        ] {
            let mut app = App::test_new();
            let mut preferences = app.completion_index.settings();
            preferences.context_triggers = enabled;
            app.completion_index.configure(preferences);
            let mut editor = Editor::test_with_text(text);
            editor.language_override = Some("Rust".into());
            editor.cursor_line = text.lines().count() - 1;
            editor.cursor_col = editor.rope.line(editor.cursor_line).len_chars();
            app.tabs.push(Tab::new_file(editor));
            app.active_tab = 0;
            handle_editor_key(&mut app, KeyCode::Char('&'), KeyModifiers::NONE);
            app.wait_for_background(std::time::Duration::from_secs(2), |app| {
                app.vtcomplete_cancel_token.is_none()
            });
            assert_eq!(app.vtcomplete_active, expected, "{text}");
        }
    }

    #[test]
    fn bounded_builtin_completion_remains_available_in_large_file_mode() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(
            "let source_value = 1;\nlet prior = &source_value;\nlet value = ",
        );
        editor.large_file_mode = true;
        editor.cursor_line = 2;
        editor.cursor_col = "let value = ".len();
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        handle_editor_key(&mut app, KeyCode::Char('&'), KeyModifiers::NONE);
        app.wait_for_background(std::time::Duration::from_secs(2), |app| {
            app.vtcomplete_active
        });
        assert!(
            app.vtcomplete_suggestions
                .iter()
                .any(|value| value == "source_value")
        );
    }

    #[test]
    fn typing_identifier_does_not_open_automatic_suggestions_when_disabled() {
        let mut app = App::test_new();
        app.quick_suggestions = false;
        let mut editor = Editor::test_with_text("pri print\n");
        editor.cursor_line = 0;
        editor.cursor_col = 3;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;

        handle_editor_key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);

        assert!(!app.vtcomplete_active);
        assert!(!app.lsp_completion_active);
        assert_eq!(app.tabs[0].editor.rope.to_string(), "prin print\n");
    }

    #[test]
    fn inline_edit_char_insert_at_cursor() {
        let mut text = String::from("hello");
        let mut cursor = 2;
        let mut anchor = None;
        let result = handle_inline_edit_key(
            &mut text,
            &mut cursor,
            &mut anchor,
            KeyCode::Char('x'),
            KeyModifiers::NONE,
        );
        assert!(result.consumed);
        assert_eq!(text, "hexllo");
        assert_eq!(cursor, 3);
    }

    #[test]
    fn inline_edit_char_insert_at_end() {
        let mut text = String::from("abc");
        let mut cursor = 3;
        handle_inline_edit_key(
            &mut text,
            &mut cursor,
            &mut None,
            KeyCode::Char('d'),
            KeyModifiers::NONE,
        );
        assert_eq!(text, "abcd");
        assert_eq!(cursor, 4);
    }

    #[test]
    fn inline_edit_backspace_at_beginning_is_noop() {
        let mut text = String::from("hello");
        let mut cursor = 0;
        let result = handle_inline_edit_key(
            &mut text,
            &mut cursor,
            &mut None,
            KeyCode::Backspace,
            KeyModifiers::NONE,
        );
        assert!(result.consumed);
        assert_eq!(text, "hello");
        assert_eq!(cursor, 0);
    }

    #[test]
    fn inline_edit_backspace_in_middle() {
        let mut text = String::from("hello");
        let mut cursor = 3;
        handle_inline_edit_key(
            &mut text,
            &mut cursor,
            &mut None,
            KeyCode::Backspace,
            KeyModifiers::NONE,
        );
        assert_eq!(text, "helo");
        assert_eq!(cursor, 2);
    }

    #[test]
    fn inline_edit_backspace_at_end() {
        let mut text = String::from("hello");
        let mut cursor = 5;
        handle_inline_edit_key(
            &mut text,
            &mut cursor,
            &mut None,
            KeyCode::Backspace,
            KeyModifiers::NONE,
        );
        assert_eq!(text, "hell");
        assert_eq!(cursor, 4);
    }

    #[test]
    fn inline_edit_delete_at_end_is_noop() {
        let mut text = String::from("hello");
        let mut cursor = 5;
        let result = handle_inline_edit_key(
            &mut text,
            &mut cursor,
            &mut None,
            KeyCode::Delete,
            KeyModifiers::NONE,
        );
        assert!(result.consumed);
        assert_eq!(text, "hello");
        assert_eq!(cursor, 5);
    }

    #[test]
    fn inline_edit_delete_at_beginning() {
        let mut text = String::from("hello");
        let mut cursor = 0;
        handle_inline_edit_key(
            &mut text,
            &mut cursor,
            &mut None,
            KeyCode::Delete,
            KeyModifiers::NONE,
        );
        assert_eq!(text, "ello");
        assert_eq!(cursor, 0);
    }

    #[test]
    fn inline_edit_left_movement() {
        let mut text = String::from("hello");
        let mut cursor = 3;
        let result = handle_inline_edit_key(
            &mut text,
            &mut cursor,
            &mut None,
            KeyCode::Left,
            KeyModifiers::NONE,
        );
        assert!(result.consumed);
        assert_eq!(cursor, 2);
        assert_eq!(text, "hello");
    }

    #[test]
    fn inline_edit_right_movement() {
        let mut text = String::from("hello");
        let mut cursor = 2;
        let result = handle_inline_edit_key(
            &mut text,
            &mut cursor,
            &mut None,
            KeyCode::Right,
            KeyModifiers::NONE,
        );
        assert!(result.consumed);
        assert_eq!(cursor, 3);
    }

    #[test]
    fn inline_edit_ctrl_a_select_all() {
        let mut text = String::from("hello");
        let mut cursor = 2;
        let mut anchor = None;
        let result = handle_inline_edit_key(
            &mut text,
            &mut cursor,
            &mut anchor,
            KeyCode::Char('a'),
            KeyModifiers::CONTROL,
        );
        assert!(result.consumed);
        assert_eq!(cursor, 5);
        assert_eq!(anchor, Some(0));
    }

    #[test]
    fn inline_edit_unhandled_key_returns_false() {
        let mut text = String::from("hello");
        let mut cursor = 0;
        let result = handle_inline_edit_key(
            &mut text,
            &mut cursor,
            &mut None,
            KeyCode::F(1),
            KeyModifiers::NONE,
        );
        assert!(!result.consumed);
        assert_eq!(text, "hello");
        assert_eq!(cursor, 0);
    }
}
