use crossterm::event::{KeyCode, KeyModifiers};

use crate::app::{ActivePane, App};
use crate::input;
use crate::input_dialogs;
use crate::input_keyboard_dialogs;
use crate::input_pickers;

#[cfg(test)]
#[path = "tab_navigation_tests.rs"]
mod tab_navigation_tests;

fn terminal_has_focus(app: &App) -> bool {
    (app.active_pane == ActivePane::Editor
        && app
            .tabs
            .get(app.active_tab)
            .is_some_and(|tab| tab.kind == crate::app::TabKind::Terminal))
        || app.docked_terminal_has_focus()
}

/// Workbench commands that VS Code keeps even when the integrated terminal is
/// focused. All other keys should reach the shell before editor keybindings.
fn terminal_workbench_shortcut(app: &App, key: crossterm::event::KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    if ctrl && key.code == KeyCode::Char('`') {
        return true;
    }
    // A fullscreen terminal application owns its editor shortcuts. Keep the
    // terminal toggle available so the user can return to this workbench.
    if app.focused_terminal_is_fullscreen() {
        return false;
    }
    if key.code == KeyCode::F(1) { return true; }
    if ctrl
        && matches!(
            key.code,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::PageUp | KeyCode::PageDown
        )
    {
        return true;
    }
    ctrl && shift
        && matches!(
            key.code,
            KeyCode::Char(
                'P' | 'p' | 'G' | 'g' | 'X' | 'x' | 'E' | 'e' | 'F' | 'f' | 'M' | 'm' | 'B' | 'b' | 'I' | 'i'
            )
        )
}

/// Handle a keyboard event. Returns `true` when the application should exit.
pub(crate) fn handle_keyboard_event(app: &mut App, key: crossterm::event::KeyEvent) -> bool {
    app.update_link_hover_key(key);
    if matches!(key.code, KeyCode::Modifier(_)) || key.kind == crossterm::event::KeyEventKind::Release {
        return false;
    }
    if app.custom_exit_remaining > 0 { return false; }
    if key.code == KeyCode::Esc && app.paste_in_progress() {
        app.cancel_pending_paste(); return false;
    }
    app.copy_animation.clear();
    app.dismiss_lsp_hover();
    app.inline_blame_hover = None;
    app.search_bar_layout.resizing = false;
    app.cancel_extension_pointer_capture();
    app.record_navigation();
    let quit = handle_keyboard_event_inner(app, key);
    app.finish_navigation();
    quit
}

fn handle_keyboard_event_inner(app: &mut App, key: crossterm::event::KeyEvent) -> bool {
    if crate::ui_vtcomplete_index::handle_key(app, key) {
        return false;
    }
    if crate::ui_logs::key(app, key) { return false; }
    if crate::ui_remote_connection::handle_key(app, key) { return false; }
    if crate::ui_desks::handle_key(app, key) { return false; }
    app.sync_text_input_sessions();
    if let Some(quit) = crate::ui_modal::handle_key(app, key) {
        return quit;
    }
    if app.sc_context_active {
        let count = app.source_control_context_menu_items().len();
        match key.code {
            KeyCode::Esc => { app.sc_context_active = false; app.sc_context_targets.clear(); }
            KeyCode::Up => app.sc_context_selected = app.sc_context_selected.saturating_sub(1),
            KeyCode::Down => app.sc_context_selected = (app.sc_context_selected + 1).min(count.saturating_sub(1)),
            KeyCode::Enter => app.execute_source_control_context(app.sc_context_selected),
            _ => {}
        }
        return false;
    }
    if crate::ui_status::handle_key(app, key) { return false; }
    if crate::ui_performance_details::handle_key(app, key) { return false; }
    if let Some(quit) = input_keyboard_dialogs::handle_commit_details_key(app, key) {
        return quit;
    }
    if crate::ui_quick_picker::active(app) == Some(crate::ui_quick_picker::Kind::OpenWith) {
        return input_keyboard_dialogs::handle_binary_open_prompt_key(app, key).unwrap_or(false);
    }
    // A fullscreen terminal application owns its navigation shortcuts too.
    if !app.focused_terminal_is_fullscreen()
        && let Some(id @ ("navigate_back" | "navigate_forward")) = app.lookup_keybinding(key.modifiers, key.code) {
        app.execute_palette_command(id);
        return false;
    }
    // Exit confirmation dialog
    if let Some(quit) = input_dialogs::handle_exit_confirm_key(app, key) {
        return quit;
    }

    // Change directory dialog
    if let Some(quit) = input_dialogs::handle_change_dir_key(app, key) {
        return quit;
    }

    // Tab context menu
    if let Some(quit) = input_dialogs::handle_tab_context_key(app, key) {
        return quit;
    }

    // Menu bar navigation
    if let Some(quit) = input_dialogs::handle_menu_key(app, key) {
        return quit;
    }

    // Explicit assignment captures shortcuts before workbench dispatch. Normal
    // tab navigation follows the popup, field, and terminal handlers below.
    if app.keybindings_editing && crate::ui_keybindings::handle_key(app, key) {
        return false;
    }

    // Source control commit message input (sidebar mode)
    if let Some(quit) = input_keyboard_dialogs::handle_commit_input_key(app, key) {
        return quit;
    }

    if app.settings_editor_has_focus() && app.settings_kb_editing {
        if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            app.settings_kb_editing = false;
            app.apply_effective_settings();
            app.notify("Keybinding edit cancelled");
        } else {
            let items = app.settings_items();
            if let Some((_, _, setting_id)) = items.get(app.settings_selected) {
                let scoped_before = app.selected_scope_settings();
                app.apply_setting_value(setting_id, &scoped_before);
                if let Some(kb_idx) = setting_id
                    .strip_prefix("kb_")
                    .and_then(|v| v.parse::<usize>().ok())
                {
                    if let Some(kb) = app.keybindings.get_mut(kb_idx) {
                        if key.modifiers.is_empty()
                            && matches!(key.code, KeyCode::Delete | KeyCode::Backspace)
                        {
                            kb.modifiers = KeyModifiers::empty();
                            kb.code = KeyCode::Null;
                            let msg = format!("Unassigned {}", kb.label);
                            app.notify(msg);
                        } else {
                            kb.modifiers = key.modifiers;
                            kb.code = key.code;
                            let msg = format!("Set {} to {}", kb.label, kb.display());
                            app.notify(msg);
                        }
                        app.rebuild_keybinding_settings_cache();
                        app.persist_scoped_setting_change(setting_id, &scoped_before);
                    }
                }
            }
            app.settings_kb_editing = false;
        }
        return false;
    }

    // A focused numeric Settings field owns every key, including unmodified
    // digits that users may have assigned as workbench shortcuts.
    if app.settings_editor_has_focus() && app.settings_value_editing {
        return input_dialogs::handle_settings_editor_key(app, key).unwrap_or(false);
    }

    // Resolve a pending chord before its second key can be consumed as a
    // standalone editing or workbench shortcut (for example Ctrl+C or Ctrl+W).
    if app.chord_pending {
        app.chord_pending = false;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('d' | 'D') if !ctrl => { crate::ui_desks::open(app, crate::ui_desks::Mode::Switch); }
            KeyCode::Char('g' | 'G') if !ctrl => { crate::ui_desks::open(app, crate::ui_desks::Mode::AddGroup); }
            KeyCode::Char('c' | 'C') if ctrl => {
                // Ctrl+K, Ctrl+C: Add line comment
                if let Some(ed) = app.editor_mut_writable() {
                    ed.add_line_comment();
                }
            }
            KeyCode::Char('u' | 'U') if ctrl => {
                // Ctrl+K, Ctrl+U: Remove line comment
                if let Some(ed) = app.editor_mut_writable() {
                    ed.remove_line_comment();
                }
            }
            KeyCode::Char('0') => {
                // Ctrl+K, 0: Fold all
                app.execute_palette_command("fold_all");
            }
            KeyCode::Char(c @ '1'..='9') => {
                // Ctrl+K, 1-9: Fold to level N
                let level = (c as usize) - ('0' as usize);
                if let Some(ed) = app.editor_mut() {
                    ed.fold_at_level(level);
                }
                app.notify(format!("Folded to level {level}"));
            }
            KeyCode::Char('j' | 'J') if ctrl => {
                // Ctrl+K, Ctrl+J: Unfold all
                app.execute_palette_command("unfold_all");
            }
            KeyCode::Char('x' | 'X') if ctrl => {
                // Ctrl+K, Ctrl+X: Trim trailing whitespace
                app.execute_palette_command("trim_trailing");
            }
            KeyCode::Char('s' | 'S') if ctrl => {
                // Ctrl+K, Ctrl+S: Keyboard shortcuts
                app.shortcuts_active = true;
            }
            KeyCode::Char('s') if !ctrl => {
                // Ctrl+K, S: Save all
                app.execute_palette_command("save_all");
            }
            KeyCode::Char('u') if !ctrl => {
                // Ctrl+K, U: Close all saved tabs
                app.close_saved_tabs();
            }
            KeyCode::Char('w') if !ctrl => {
                // Ctrl+K, W: Close all tabs
                app.execute_palette_command("tab_close_all");
            }
            KeyCode::Char('w') if ctrl => {
                // Ctrl+K, Ctrl+W: Close all editors in group
                app.close_tabs_matching(|_| true);
            }
            KeyCode::Char('q' | 'Q') if ctrl => {
                // Ctrl+K, Ctrl+Q: Go to last edit location
                app.execute_palette_command("goto_last_edit");
            }
            KeyCode::Char('d' | 'D') if ctrl => {
                // Ctrl+K, Ctrl+D: Skip occurrence, select next
                if let Some(ed) = app.editor_mut() {
                    ed.skip_occurrence_select_next();
                }
            }
            _ => {
                app.notify("Chord cancelled");
            }
        }
        return false;
    }

    // Focused text fields own editing shortcuts. This must precede the
    // configurable workbench dispatcher, which otherwise sends Ctrl+X/C/V/A
    // to the code editor even when a search or settings field has focus.
    if app.handle_chat_key(key) { return false; }
    if crate::ui_explorer_find::handle_key(app, key) { return false; }
    if app.handle_focused_text_input_shortcut(key.code, key.modifiers) {
        return false;
    }

    if !terminal_has_focus(app) || terminal_workbench_shortcut(app, key)
        || (app.zen_mode && !app.focused_terminal_is_fullscreen()
            && key.code == KeyCode::F(11) && key.modifiers.is_empty())
    {
        // AltGr handling (Ctrl+Alt on Windows = AltGr)
        // When both Ctrl and Alt are pressed with a printable char, treat as text input
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::ALT)
            && !key.modifiers.contains(KeyModifiers::SHIFT)
        {
            if let KeyCode::Char(c) = key.code {
                if app.active_pane == ActivePane::Editor {
                    // Strip the synthetic Ctrl+Alt modifiers and use the normal
                    // editor text path so undo, highlighting, LSP, and an open
                    // (unfocused) Find widget all observe the edit.
                    input::handle_editor_key(app, KeyCode::Char(c), KeyModifiers::empty());
                }
                return false;
            }
        }

        // Configurable keybinding dispatch
        if app.route_custom_editor_shortcut(key.code, key.modifiers) { return false; }
        if let Some(cmd_id) = app.lookup_keybinding(key.modifiers, key.code) {
            if app.route_explorer_edit_command(cmd_id) { return false; }
            // End clipboard cycle on any keybinding that isn't clipboard_history
            if app.clipboard_cycle_active && cmd_id != "clipboard_history" {
                app.clipboard_cycle_active = false;
            }
            match cmd_id {
                "quit" => {
                    app.exit_confirm_active = true;
                    return false;
                }
                "save" => {
                    app.save_current_file();
                    return false;
                }
                "toggle_explorer" => {
                    app.toggle_explorer();
                    return false;
                }
                "toggle_terminal" => {
                    app.toggle_terminal_sidebar();
                    return false;
                }
                "close_tab" => {
                    app.close_tab(app.active_tab);
                    return false;
                }
                "palette" => {
                    app.open_palette();
                    return false;
                }
                "source_control" => {
                    app.execute_palette_command("source_control");
                    return false;
                }
                "extensions" => {
                    app.execute_palette_command("extensions");
                    return false;
                }
                "go_to_line" => {
                    app.execute_palette_command("goto_line");
                    return false;
                }
                "global_search" => {
                    app.execute_palette_command("global_search");
                    return false;
                }
                "delete_line" => {
                    app.execute_palette_command("delete_line");
                    return false;
                }
                "toggle_comment" => {
                    app.execute_palette_command("toggle_comment");
                    return false;
                }
                "show_shortcuts" => {
                    app.shortcuts_active = !app.shortcuts_active;
                    app.shortcuts_scroll = 0;
                    return false;
                }
                "rename_symbol" => {
                    // Explorer owns F2 for file/folder renaming. Leave it for
                    // the focused view after menus and text inputs are handled.
                    if app.active_pane == ActivePane::Editor {
                        app.execute_palette_command("rename_symbol");
                        return false;
                    }
                }
                "find_next" => {
                    if !app.search_query.is_empty() {
                        app.search_next();
                    }
                    return false;
                }
                "find_prev" => {
                    if !app.search_query.is_empty() {
                        app.search_prev();
                    }
                    return false;
                }
                "toggle_menu_bar" => {
                    app.execute_palette_command("toggle_menu_bar");
                    return false;
                }
                "go_to_definition" => {
                    if app.active_pane == ActivePane::Editor {
                        app.execute_palette_command("go_to_definition");
                        return false;
                    }
                }
                "go_to_type_definition" => {
                    if app.active_pane == ActivePane::Editor {
                        app.execute_palette_command("go_to_type_definition");
                        return false;
                    }
                }
                "go_to_implementation" => {
                    if app.active_pane == ActivePane::Editor {
                        app.execute_palette_command("go_to_implementation");
                        return false;
                    }
                }
                "go_to_declaration" => {
                    if app.active_pane == ActivePane::Editor {
                        app.execute_palette_command("go_to_declaration");
                        return false;
                    }
                }
                "find_references" => {
                    if app.active_pane == ActivePane::Editor {
                        app.execute_palette_command("find_references");
                        return false;
                    }
                }
                "format_document" => {
                    app.execute_palette_command("format_document");
                    return false;
                }
                "toggle_problems" => {
                    crate::ui_panels::toggle_problems(app);
                    return false;
                }
                "next_tab" | "prev_tab" | "next_tab_page" | "prev_tab_page" => {
                    if !app.tabs.is_empty() {
                        app.sync_duplicate_file_tabs();
                        app.clear_transient_editor_state();
                        if app.tabs.len() > 1 { app.auto_save_on_focus_change(); }
                        app.active_tab = if matches!(cmd_id, "next_tab" | "next_tab_page") {
                            (app.active_tab + 1) % app.tabs.len()
                        } else {
                            app.active_tab.checked_sub(1).unwrap_or(app.tabs.len() - 1)
                        };
                        app.validate_diff_state_for_active_tab();
                        app.touch_tab_mru(app.active_tab);
                        app.refresh_git_line_status_async();
                        app.active_pane = ActivePane::Editor;
                        app.terminal_panel_focused = false;
                    }
                    return false;
                }
                "clipboard_history" => {
                    // Ctrl+Shift+V: cycle paste from clipboard history
                    if app.diff_selection_owns_cursor() || app.editor().is_none_or(|ed| ed.read_only) {
                        return false;
                    }
                    if app.clipboard_history.is_empty() {
                        app.notify("Clipboard history is empty");
                    } else {
                        if app.paste_in_progress() { app.notify_warn("Wait for the current paste or cancel it first"); return false; }
                        let history_len = app.clipboard_history.len();
                        if app.clipboard_cycle_active {
                            if let Some(ed) = app.editor_mut() {
                                ed.undo();
                            }
                            app.clipboard_cycle_index =
                                (app.clipboard_cycle_index + 1) % history_len;
                        } else {
                            app.clipboard_cycle_active = true;
                            app.clipboard_cycle_index = 0;
                        }
                        let real_idx = history_len - 1 - app.clipboard_cycle_index;
                        if let Some(text) = app.clipboard_history.get(real_idx).cloned() {
                            app.paste_editor_text(text, true);
                        }
                    }
                    return false;
                }
                _ => {
                    // Try to dispatch via palette command system
                    app.execute_palette_command(cmd_id);
                    return false;
                }
            }
        }

        // Global keybindings (Ctrl+*)
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::SHIFT)
            && (key.code == KeyCode::Char('P') || key.code == KeyCode::Char('p'))
        {
            app.open_palette();
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::SHIFT)
            && (key.code == KeyCode::Char('G') || key.code == KeyCode::Char('g'))
        {
            app.execute_palette_command("source_control");
            app.terminal_panel_focused = false;
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::SHIFT)
            && (key.code == KeyCode::Char('X') || key.code == KeyCode::Char('x'))
        {
            app.execute_palette_command("extensions");
            app.terminal_panel_focused = false;
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::SHIFT)
            && (key.code == KeyCode::Char('E') || key.code == KeyCode::Char('e'))
        {
            // Ctrl+Shift+E: toggle/focus explorer
            if !app.show_explorer {
                app.show_explorer = true;
            }
            app.active_pane = crate::app::ActivePane::Explorer;
            app.terminal_panel_focused = false;
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::SHIFT)
            && (key.code == KeyCode::Char('F') || key.code == KeyCode::Char('f'))
        {
            // Ctrl+Shift+F: focus global search
            app.focus_global_search();
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::SHIFT)
            && (key.code == KeyCode::Char('M') || key.code == KeyCode::Char('m'))
        {
            // Ctrl+Shift+M: toggle Problems panel
            crate::ui_panels::toggle_problems(app);
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::SHIFT)
            && (key.code == KeyCode::Char('S') || key.code == KeyCode::Char('s'))
        {
            // Match VS Code: Ctrl+Shift+S opens Save As. Save All is Ctrl+K, S.
            app.execute_palette_command("save_as");
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::SHIFT)
            && (key.code == KeyCode::Char('B') || key.code == KeyCode::Char('b'))
        {
            // Ctrl+Shift+B: rerun last task or open task picker
            app.execute_palette_command("rerun_last_task");
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::SHIFT)
            && (key.code == KeyCode::Char('O') || key.code == KeyCode::Char('o'))
        {
            // Ctrl+Shift+O: go to symbol in file
            app.symbol_list_workspace = false;
            app.symbol_list_active = true;
            app.symbol_list_query.clear();
            app.symbol_list_cursor = 0;
            app.symbol_list_selection_anchor = None;
            app.build_symbol_list();
            return false;
        }
        // Shift+Alt+F: format document
        if key.modifiers.contains(KeyModifiers::SHIFT)
            && key.modifiers.contains(KeyModifiers::ALT)
            && !key.modifiers.contains(KeyModifiers::CONTROL)
            && (key.code == KeyCode::Char('F') || key.code == KeyCode::Char('f'))
        {
            app.execute_palette_command("format_document");
            return false;
        }
        // F2: Rename symbol
        if app.active_pane == ActivePane::Editor
            && key.code == KeyCode::F(2) && key.modifiers.is_empty() {
            app.execute_palette_command("rename_symbol");
            return false;
        }
        // F3/Shift+F3: Find next/previous
        if key.code == KeyCode::F(3) {
            if !app.search_query.is_empty() {
                if key.modifiers.contains(KeyModifiers::SHIFT) {
                    app.search_prev();
                } else {
                    app.search_next();
                }
            }
            return false;
        }
        // F11: Toggle zen mode
        if key.code == KeyCode::F(11) && key.modifiers.is_empty() {
            app.execute_palette_command("toggle_zen");
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('q') => {
                    app.exit_confirm_active = true;
                    return false;
                }
                KeyCode::Char('s') => {
                    app.save_current_file();
                    return false;
                }
                KeyCode::Char('b') => {
                    app.toggle_explorer();
                    return false;
                }
                KeyCode::Char('`') => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        app.execute_palette_command("new_terminal");
                    } else {
                        app.toggle_terminal_sidebar();
                    }
                    return false;
                }
                KeyCode::Char('w') => {
                    app.close_tab(app.active_tab);
                    return false;
                }
                KeyCode::Char('f') => {
                    if app.editor().is_none() { return false; }
                    if !app.search_active {
                        app.search_pre_cursor = app
                            .editor()
                            .map(|e| (e.cursor_line, e.cursor_col, e.scroll_offset));
                    }
                    app.search_active = true;
                    app.search_input_focused = true;
                    app.replace_mode = false;
                    app.search_focus_replace = false;
                    if let Some(ed) = app.editor() {
                        if let Some((sl, sc, el, ec)) = ed.selection_range() {
                            if sl != el {
                                // Multi-line selection: enable search-in-selection
                                app.search_in_selection = true;
                                app.search_selection_range = Some((sl, sc, el, ec));
                            } else if let Some(sel) = ed.selected_text() {
                                // Single-line selection: pre-fill search query
                                if !sel.is_empty() {
                                    app.search_query = sel;
                                    app.update_search();
                                }
                            }
                        }
                    }
                    app.search_cursor = app.search_query.len();
                    app.search_selection_anchor = (!app.search_query.is_empty()).then_some(0);
                    return false;
                }
                KeyCode::Char('h') => {
                    if app.editor().is_none() { return false; }
                    if !app.search_active {
                        app.search_pre_cursor = app
                            .editor()
                            .map(|e| (e.cursor_line, e.cursor_col, e.scroll_offset));
                    }
                    // Pre-fill search query with single-line selection
                    if let Some(ed) = app.editor() {
                        if let Some(sel) = ed.selected_search_text() {
                            app.search_query = sel;
                        }
                    }
                    app.search_active = true;
                    app.search_input_focused = true;
                    app.replace_mode = true;
                    app.search_focus_replace = false;
                    app.search_cursor = app.search_query.len();
                    app.search_selection_anchor = (!app.search_query.is_empty()).then_some(0);
                    app.replace_cursor = app.replace_query.len();
                    app.replace_selection_anchor = None;
                    if !app.search_query.is_empty() {
                        app.update_search();
                    }
                    return false;
                }
                KeyCode::Char('p') => {
                    app.clear_drag_states();
                    app.quick_open_active = true;
                    app.quick_open_query.clear();
                    app.quick_open_cursor = 0;
                    app.quick_open_selection_anchor = None;
                    app.update_quick_open();
                    return false;
                }
                KeyCode::Char('g') => {
                    app.open_goto_line();
                    return false;
                }
                KeyCode::Char('z' | 'Z') => {
                    let redo_requested = key.modifiers.contains(KeyModifiers::SHIFT)
                        || key.code == KeyCode::Char('Z');
                    if redo_requested {
                        if let Some(ed) = app.editor_mut_writable() {
                            ed.redo();
                        }
                        if !app.search_query.is_empty() {
                            app.update_search();
                        }
                    } else {
                        if let Some(ed) = app.editor_mut_writable() {
                            ed.undo();
                        }
                        if !app.search_query.is_empty() {
                            app.update_search();
                        }
                    }
                    return false;
                }
                KeyCode::Char('u') => {
                    // Ctrl+U: Cursor undo (undo last cursor position change)
                    if let Some(ed) = app.editor_mut() {
                        ed.cursor_undo();
                    }
                    return false;
                }
                KeyCode::Char('y') => {
                    if let Some(ed) = app.editor_mut_writable() {
                        ed.redo();
                    }
                    if !app.search_query.is_empty() {
                        app.update_search();
                    }
                    return false;
                }
                KeyCode::Char('d' | 'D') => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        // Ctrl+Shift+D: duplicate line
                        if let Some(ed) = app.editor_mut_writable() {
                            ed.duplicate_line();
                        }
                    } else {
                        // Ctrl+D: select next occurrence
                        if let Some(ed) = app.editor_mut() {
                            ed.select_next_occurrence();
                        }
                    }
                    return false;
                }
                KeyCode::Char('/') => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        // Ctrl+Shift+/: Toggle block comment
                        if let Some(ed) = app.editor_mut_writable() {
                            ed.toggle_block_comment();
                        }
                    } else if let Some(ed) = app.editor_mut_writable() {
                        ed.toggle_line_comment();
                    }
                    return false;
                }
                KeyCode::Char(']') => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        app.execute_palette_command("unfold");
                    } else if let Some(ed) = app.editor_mut_writable() {
                        ed.indent_lines();
                    }
                    return false;
                }
                KeyCode::Char('[') => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        app.execute_palette_command("fold");
                    } else if let Some(ed) = app.editor_mut_writable() {
                        ed.dedent_line();
                    }
                    return false;
                }
                KeyCode::Char('l' | 'L') => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        if let Some(ed) = app.editor_mut() {
                            ed.select_all_occurrences();
                        }
                    } else if let Some(ed) = app.editor_mut() {
                        ed.select_line();
                    }
                    return false;
                }
                KeyCode::Char('j') => {
                    if let Some(ed) = app.editor_mut_writable() {
                        ed.join_lines();
                    }
                    return false;
                }
                KeyCode::Char('t' | 'T') => {
                    if let Some(ed) = app.editor_mut_writable() {
                        ed.transpose_chars();
                    }
                    return false;
                }
                KeyCode::Char('k' | 'K') => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        // Ctrl+Shift+K: delete line
                        if let Some(ed) = app.editor_mut_writable() {
                            ed.delete_line();
                        }
                    } else {
                        // Ctrl+K: enter chord mode
                        app.chord_pending = true;
                        app.chord_start = std::time::Instant::now();
                        app.notify("Ctrl+K ...");
                    }
                    return false;
                }
                KeyCode::Char('\\') => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        // Ctrl+Shift+\: select to matching bracket
                        app.execute_palette_command("select_to_bracket");
                    } else {
                        // Ctrl+\: go to matching bracket
                        app.execute_palette_command("goto_bracket");
                    }
                    return false;
                }
                KeyCode::Char('.') => {
                    // Ctrl+.: code actions / quick fixes
                    if !app.lsp_manager.clients.is_empty() {
                        if let Some(ed) = app.editor() {
                            {
                                if let Some(fp) = ed.file_path.as_ref() {
                                    let fp = fp.clone();
                                    let line = ed.cursor_line as u32;
                                    let col = crate::lsp_positions::to_utf16(&ed.rope, ed.cursor_line, ed.cursor_col);
                                    // Collect diagnostics at cursor position for context
                                    let diags = app.lsp_manager.code_action_diagnostics(&fp, line);
                                    app.lsp_code_action_active = app
                                        .lsp_manager
                                        .request_code_action(&crate::lsp::CodeActionParams {
                                            path: &fp,
                                            start_line: line,
                                            start_col: col,
                                            end_line: line,
                                            end_col: col,
                                            diagnostics: diags,
                                        });
                                    if app.lsp_code_action_active {
                                        app.lsp_code_action_selected = 0;
                                    }
                                }
                            }
                        }
                    }
                    return false;
                }
                KeyCode::Char('n') => {
                    app.new_file();
                    return false;
                }
                KeyCode::Char('o') => {
                    // Ctrl+O: open folder / change directory
                    app.open_folder_browser();
                    return false;
                }
                KeyCode::Char(',') => {
                    // Toggle settings tab
                    if app.tabs.get(app.active_tab).map(|t| t.kind)
                        == Some(crate::app::TabKind::Settings)
                    {
                        app.close_tab(app.active_tab);
                        app.settings_editor_active = false;
                    } else {
                        app.open_settings_tab();
                        app.settings_selected = 0;
                        app.settings_category_selected = 0;
                        app.settings_section_selected = None;
                        app.settings_category_scroll = 0;
                        app.settings_category_collapsed.fill(true);
                        app.settings_category_area = None;
                        app.settings_focus_categories = false;
                        app.settings_editor_scroll = 0;
                        app.settings_editor_search.clear();
                        app.settings_editor_search_cursor = 0;
                        app.settings_editor_search_selection_anchor = None;
                        app.ensure_settings_selection_visible();
                    }
                    return false;
                }
                _ => {}
            }
        }
    }

    // Explorer context menu
    if app.explorer_context_active {
        match key.code {
            KeyCode::Esc => {
                app.explorer_context_active = false;
            }
            KeyCode::Up => {
                if app.explorer_context_selected > 0 {
                    app.explorer_context_selected -= 1;
                }
            }
            KeyCode::Down => {
                let count = app.explorer_context_menu_items().len();
                if app.explorer_context_selected + 1 < count {
                    app.explorer_context_selected += 1;
                }
            }
            KeyCode::Enter => {
                let idx = app.explorer_context_selected;
                app.execute_explorer_context(idx);
            }
            _ => {}
        }
        return false;
    }

    if app.hunk_popup_active {
        if app.hunk_peek.is_some() || app.hunk_preview_feedback.is_some() {
            crate::ui_hunk_peek::handle_key(app, key);
            return false;
        }
        match key.code {
            KeyCode::Esc => app.close_hunk_popup(),
            KeyCode::Up => {
                if app.hunk_popup_selected > 0 {
                    app.hunk_popup_selected -= 1;
                }
            }
            KeyCode::Down => {
                let count = app.hunk_popup_items().len();
                if app.hunk_popup_selected + 1 < count {
                    app.hunk_popup_selected += 1;
                }
            }
            KeyCode::Enter => {
                let idx = app.hunk_popup_selected;
                app.execute_hunk_popup_action(idx);
            }
            _ => {}
        }
        return false;
    }

    // Rename dialog
    if let Some(quit) = input_keyboard_dialogs::handle_rename_key(app, key) {
        return quit;
    }

    // Save As dialog
    if let Some(quit) = input_keyboard_dialogs::handle_save_as_key(app, key) {
        return quit;
    }

    // Rename symbol dialog
    if let Some(quit) = input_keyboard_dialogs::handle_rename_symbol_key(app, key) {
        return quit;
    }

    // Context menu
    if app.context_menu_active {
        if input_pickers::handle_context_menu_key(app, key.code) {
            return true;
        }
        return false;
    }

    // Binary open prompt dialog
    if let Some(quit) = input_keyboard_dialogs::handle_binary_open_prompt_key(app, key) {
        return quit;
    }

    // Confirm close dialog
    if app.confirm_close_active {
        input_pickers::handle_confirm_close_key(app, key.code);
        return false;
    }

    // Explorer delete confirmation dialog
    if let Some(quit) = input_keyboard_dialogs::handle_confirm_delete_key(app, key) {
        return quit;
    }

    // Explorer drag-and-drop move confirmation dialog
    if let Some(quit) = input_keyboard_dialogs::handle_confirm_move_key(app, key) {
        return quit;
    }

    // Command palette
    if app.palette.active {
        if input_pickers::handle_palette_key(app, key.code, key.modifiers) {
            return true;
        }
        return false;
    }

    // Task picker
    if let Some(quit) = input_dialogs::handle_task_picker_key(app, key) {
        return quit;
    }

    if key.code == KeyCode::Esc && (app.vtcomplete_active
        || app.vtcomplete_explicit
            || app.vtcomplete_cancel_token.is_some()
        || app.lsp_completion_active || app.lsp_signature_active)
    {
        app.dismiss_vtcomplete();
        app.lsp_completion_active = false;
        app.lsp_signature_active = false;
        app.dispatch_lsp_suggestions();
        return false;
    }

    if app.lsp_signature_active && key.modifiers == KeyModifiers::ALT
        && matches!(key.code, KeyCode::Char('[') | KeyCode::Char(']'))
    {
        if let Some(help) = app.lsp_manager.signature_help.as_mut().filter(|help| !help.signatures.is_empty()) {
            let count = help.signatures.len();
            help.active_signature = if key.code == KeyCode::Char('[') {
                (help.active_signature.min(count - 1) + count - 1) % count
            } else { (help.active_signature.min(count - 1) + 1) % count };
        }
        return false;
    }

    // LSP suggestions are displayed above local results, which can arrive later.
    // Route keys to the visible list first, retaining local results as a fallback.
    if crate::ui_lsp_completion::handle_key(app, key) { return false; }
    if crate::ui_lsp_completion::visible(app) {
        // A hidden local list must never accept a key intended for this popup.
        app.dismiss_vtcomplete();
    }
    if app.lsp_completion_active && !matches!(key.code, KeyCode::Char(_) | KeyCode::Backspace) {
        app.lsp_completion_active = false;
    }

    if !app.vtcomplete_active
        && app.vtcomplete_explicit
        && !matches!(key.code, KeyCode::Char(_) | KeyCode::Backspace)
    {
        app.dismiss_vtcomplete();
    }

    // VTComplete popup
    if app.vtcomplete_active {
        match key.code {
            KeyCode::Up if key.modifiers.is_empty() => {
                if app.vtcomplete_selected > 0 {
                    app.vtcomplete_selected -= 1;
                }
                app.vtcomplete_navigated = true;
                return false;
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                if app.vtcomplete_selected + 1 < app.vtcomplete_suggestions.len() {
                    app.vtcomplete_selected += 1;
                }
                app.vtcomplete_navigated = true;
                return false;
            }
            KeyCode::Tab if key.modifiers.is_empty() => {
                app.apply_vtcomplete();
                return false;
            }
            KeyCode::Enter
                if key.modifiers.is_empty()
                    && (app.vtcomplete_explicit || app.vtcomplete_navigated) =>
            {
                app.apply_vtcomplete();
                return false;
            }
            KeyCode::Char(c)
                if (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
                    && (c.is_alphanumeric() || c == '_' || c == '$') => {}
            KeyCode::Backspace if key.modifiers.is_empty() => {}
            _ => {
                // Close vtcomplete and let the key be handled normally.
                app.dismiss_vtcomplete();
            }
        }
    }

    // LSP code action menu
    if app.lsp_code_action_active && !app.lsp_manager.code_actions.is_empty() {
        match key.code {
            KeyCode::Up if key.modifiers.is_empty() => {
                if app.lsp_code_action_selected > 0 {
                    app.lsp_code_action_selected -= 1;
                }
                return false;
            }
            KeyCode::Down if key.modifiers.is_empty() => {
                let max = app.lsp_manager.code_actions.len().saturating_sub(1);
                if app.lsp_code_action_selected < max {
                    app.lsp_code_action_selected += 1;
                }
                return false;
            }
            KeyCode::Enter => {
                app.apply_lsp_code_action();
                return false;
            }
            KeyCode::Esc => {
                app.lsp_code_action_active = false;
                app.lsp_manager.code_actions.clear();
                return false;
            }
            _ => {
                app.lsp_code_action_active = false;
                app.lsp_manager.code_actions.clear();
            }
        }
    }

    // Call hierarchy dialog
    if app.call_hierarchy_active {
        match key.code {
            KeyCode::Up => {
                if app.call_hierarchy_selected > 0 {
                    app.call_hierarchy_selected -= 1;
                }
                return false;
            }
            KeyCode::Down => {
                let max = app.call_hierarchy_results.len().saturating_sub(1);
                if app.call_hierarchy_selected < max {
                    app.call_hierarchy_selected += 1;
                }
                return false;
            }
            KeyCode::Enter => {
                if let Some(entry) = app
                    .call_hierarchy_results
                    .get(app.call_hierarchy_selected)
                    .cloned()
                {
                    app.call_hierarchy_active = false;
                    app.push_nav_history();
                    app.open_file(&entry.file_path);
                    if let Some(ed) = app.editor_mut() {
                        ed.clear_selection();
                        ed.set_lsp_cursor(entry.line, entry.col as u32);
                        ed.center_cursor();
                    }
                }
                return false;
            }
            KeyCode::Esc => {
                app.call_hierarchy_active = false;
                return false;
            }
            _ => {
                app.call_hierarchy_active = false;
            }
        }
    }

    // Type hierarchy popup
    if app.type_hierarchy_active {
        match key.code {
            KeyCode::Up => {
                if app.type_hierarchy_selected > 0 {
                    app.type_hierarchy_selected -= 1;
                }
                return false;
            }
            KeyCode::Down => {
                let max = app.type_hierarchy_results.len().saturating_sub(1);
                if app.type_hierarchy_selected < max {
                    app.type_hierarchy_selected += 1;
                }
                return false;
            }
            KeyCode::Enter => {
                if let Some(entry) = app
                    .type_hierarchy_results
                    .get(app.type_hierarchy_selected)
                    .cloned()
                {
                    app.type_hierarchy_active = false;
                    app.push_nav_history();
                    app.open_file(&entry.file_path);
                    if let Some(ed) = app.editor_mut() {
                        ed.clear_selection();
                        ed.set_lsp_cursor(entry.line, entry.col as u32);
                        ed.center_cursor();
                    }
                }
                return false;
            }
            KeyCode::Esc => {
                app.type_hierarchy_active = false;
                return false;
            }
            _ => {
                app.type_hierarchy_active = false;
            }
        }
    }

    // LSP references popup
    if app.lsp_references_active && !app.lsp_manager.reference_locations.is_empty() {
        match key.code {
            KeyCode::Up => {
                if app.lsp_references_selected > 0 {
                    app.lsp_references_selected -= 1;
                }
                return false;
            }
            KeyCode::Down => {
                let max = app.lsp_manager.reference_locations.len().saturating_sub(1);
                if app.lsp_references_selected < max {
                    app.lsp_references_selected += 1;
                }
                return false;
            }
            KeyCode::Enter => {
                let loc = app.lsp_manager.reference_locations[app.lsp_references_selected].clone();
                app.lsp_references_active = false;
                if let Ok(path) = loc.uri.to_file_path() {
                    app.push_nav_history();
                    app.open_file(&path);
                    if let Some(ed) = app.editor_mut() {
                        ed.clear_selection();
                        ed.set_lsp_cursor(
                            loc.range.start.line as usize,
                            loc.range.start.character,
                        );
                        ed.center_cursor();
                    }
                }
                return false;
            }
            KeyCode::Esc => {
                app.lsp_references_active = false;
                app.lsp_manager.reference_locations.clear();
                return false;
            }
            _ => {
                app.lsp_references_active = false;
                app.lsp_manager.reference_locations.clear();
            }
        }
    }

    // Shortcuts reference
    if app.shortcuts_active {
        if key.code == KeyCode::Esc {
            app.shortcuts_active = false;
        }
        return false;
    }

    // Settings editor view
    if let Some(quit) = input_dialogs::handle_settings_editor_key(app, key) {
        return quit;
    }

    // Settings panel
    if let Some(quit) = input_dialogs::handle_settings_panel_key(app, key) {
        return quit;
    }

    // Search mode input
    if app.file_search_visible() && app.search_input_focused {
        input::handle_search_key(app, key.code, key.modifiers);
        return false;
    }

    // Quick open mode
    if app.quick_open_active {
        input_pickers::handle_quick_open_key(app, key.code, key.modifiers);
        return false;
    }

    if app.git_branch_picker_active {
        input_pickers::handle_git_branch_picker_key(app, key.code, key.modifiers);
        return false;
    }

    // Go to line mode
    if app.goto_line_active {
        input_pickers::handle_goto_line_key(app, key.code, key.modifiers);
        return false;
    }

    // Global search mode
    let sidebar_search_active =
        app.show_explorer && app.sidebar_view == crate::app::SidebarView::Search;
    if app.global_search_active
        && (!sidebar_search_active || app.active_pane == ActivePane::Explorer)
    {
        input::handle_global_search_key(app, key.code, key.modifiers);
        return false;
    }

    // Symbol list mode
    if app.symbol_list_active {
        input_pickers::handle_symbol_list_key(app, key.code, key.modifiers);
        return false;
    }

    // Terminal keys are deliberately routed after modal inputs but before
    // editor input. The global-keybinding block above is skipped for shell
    // keys, so Ctrl+C/Ctrl+R/Ctrl+L/etc. behave like a real terminal.
    if let Some(result) = app.handle_terminal_tab_key(key) {
        return result;
    }
    if let Some(result) = app.handle_panel_terminal_key(key) {
        return result;
    }

    if crate::editor_blame::handle_key(app, key) {
        return false;
    }

    if crate::ui_keybindings::handle_key(app, key) { return false; }
    if crate::ui_extension_info::handle_key(app, key) { return false; }

    match app.active_pane {
        ActivePane::Explorer => input::handle_explorer_key(app, key.code, key.modifiers),
        ActivePane::Editor => input::handle_editor_key(app, key.code, key.modifiers),
        ActivePane::Chat => {},
        ActivePane::Panel => {},
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn completion_app() -> App {
        let mut app = App::test_new();
        let mut editor = crate::editor::Editor::test_with_text("mea");
        editor.set_cursor(0, 3);
        app.tabs = vec![crate::app::Tab::new_file(editor)];
        app.active_tab = 0;
        app.active_pane = ActivePane::Editor;
        app.vtcomplete_active = true;
        app.vtcomplete_suggestions = vec!["meadow".into(), "meanwhile".into()];
        app.lsp_completion_active = true;
        app.lsp_manager.completion_items = ["measurements", "meaning"].into_iter().map(|label|
            crate::lsp::LspCompletionItem {
                label: label.into(), insert_text: label.into(), detail: None,
                kind: Some(lsp_types::CompletionItemKind::VARIABLE), text_edit_range: None,
                additional_text_edits: vec![], is_snippet: false, original_item: None, documentation: None,
            }).collect();
        app
    }

    #[test]
    fn lsp_completion_keyboard_targets_the_visible_list_when_local_results_arrive() {
        for accept in [KeyCode::Enter, KeyCode::Tab] {
            let mut app = completion_app();
            handle_keyboard_event(&mut app, key(KeyCode::Down));
            assert_eq!(app.lsp_completion_selected, 1);
            assert_eq!(app.vtcomplete_selected, 0);
            assert_eq!((app.editor().unwrap().cursor_line, app.editor().unwrap().cursor_col), (0, 3));
            let mut repeated = key(KeyCode::Down);
            repeated.kind = KeyEventKind::Repeat;
            handle_keyboard_event(&mut app, repeated);
            assert_eq!(app.lsp_completion_selected, 1, "selection stops at the last suggestion");
            handle_keyboard_event(&mut app, key(KeyCode::Up));
            handle_keyboard_event(&mut app, key(KeyCode::Up));
            assert_eq!(app.lsp_completion_selected, 0);
            handle_keyboard_event(&mut app, key(KeyCode::Down));
            handle_keyboard_event(&mut app, key(accept));
            assert_eq!(app.editor().unwrap().rope.to_string(), "meaning");
            assert!(!app.lsp_completion_active);
            assert!(!app.vtcomplete_active);
        }
    }

    #[test]
    fn lsp_completion_navigation_scrolls_to_keep_the_selected_row_visible() {
        let mut app = completion_app();
        app.code_area = Some(ratatui::layout::Rect::new(0, 0, 100, 30));
        let item = app.lsp_manager.completion_items[0].clone();
        app.lsp_manager.completion_items = vec![item; 15];
        for _ in 0..12 { handle_keyboard_event(&mut app, key(KeyCode::Down)); }
        assert_eq!(app.lsp_completion_selected, 12);
        let geometry = crate::ui_search_dialog::lsp_completion_popup_geometry(
            &mut app, ratatui::layout::Rect::new(0, 0, 100, 30)).unwrap();
        assert_eq!(geometry.scroll, 3);
        assert_eq!(app.editor().unwrap().cursor_col, 3);
    }

    #[test]
    fn completion_escape_dismisses_both_providers_and_rejects_late_local_results() {
        let mut app = completion_app();
        let editor = app.editor().unwrap();
        let late_result = crate::background::BackgroundResult::VtComplete {
            generation: app.vtcomplete_generation,
            active_group: app.active_group, active_tab: app.active_tab,
            document_id: editor.navigation_id, cursor_line: editor.cursor_line, cursor_col: editor.cursor_col,
            content_version: editor.content_version, source_rope: editor.rope.clone(),
                source_identity: crate::completion_source::Source::new(editor.file_path.as_deref(), editor.language_name()),
            language: editor.language_name().into(), suggestions: vec!["meadow".into()],
            details: Vec::new(),
            work_outcome: crate::app_vtcomplete::VtCompleteWorkOutcome::Exact, deferred_candidates: 0,
        };
        handle_keyboard_event(&mut app, key(KeyCode::Esc));
        assert!(!app.lsp_completion_active);
        assert!(!app.vtcomplete_active);
        app.background.sender.send(late_result).unwrap();
        app.poll_background_results();
        assert!(!app.vtcomplete_active, "dismissed suggestions must not reopen on a late reply");
    }

    #[test]
    fn local_completion_keeps_keyboard_control_while_lsp_has_no_results() {
        let mut app = completion_app();
        app.lsp_manager.completion_items.clear();
        handle_keyboard_event(&mut app, key(KeyCode::Down));
        assert_eq!(app.vtcomplete_selected, 1);
        handle_keyboard_event(&mut app, key(KeyCode::Enter));
        assert_eq!(app.editor().unwrap().rope.to_string(), "meanwhile");
    }

    #[test]
    fn automatic_local_suggestions_leave_enter_available_for_newlines() {
        let mut app = completion_app();
        app.lsp_completion_active = false;
        handle_keyboard_event(&mut app, key(KeyCode::Enter));
        assert_eq!(app.editor().unwrap().rope.to_string(), "mea\n");
        assert!(!app.vtcomplete_active);
    }

    #[test]
    fn tab_and_explicit_enter_accept_local_suggestions() {
        for code in [KeyCode::Tab, KeyCode::Enter] {
            let mut app = completion_app();
            app.lsp_completion_active = false;
            app.vtcomplete_explicit = code == KeyCode::Enter;
            handle_keyboard_event(&mut app, key(code));
            assert_eq!(app.editor().unwrap().rope.to_string(), "meadow");
            assert!(!app.vtcomplete_active);
        }
    }

    #[test]
    fn indexing_dialog_captures_typing_and_paste_without_modifying_editor() {
        let mut app = completion_app();
        crate::ui_vtcomplete_index::open(&mut app);
        handle_keyboard_event(&mut app, key(KeyCode::Char('z')));
        crate::app_event_loop::handle_paste_input(&mut app, "pasted text");
        assert_eq!(app.editor().unwrap().rope.to_string(), "mea");
        assert!(app.completion_index_ui.open);
        handle_keyboard_event(&mut app, key(KeyCode::Esc));
        assert!(!app.completion_index_ui.open);
    }

    #[test]
    fn empty_explicit_results_exit_on_escape_or_cursor_navigation() {
        for code in [KeyCode::Esc, KeyCode::Left] {
            let mut app = completion_app();
            app.lsp_completion_active = false;
            app.vtcomplete_active = false;
            app.vtcomplete_suggestions.clear();
            app.vtcomplete_explicit = true;
            handle_keyboard_event(&mut app, key(code));
            assert!(!app.vtcomplete_explicit);
            assert!(app.vtcomplete_cancel_token.is_none());
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    fn key_mod(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    #[test]
    fn visible_terminal_panel_does_not_steal_focus() {
        let mut app = App::test_new();
        app.terminals[0].dock = crate::app::TerminalDock::Right;
        app.show_terminal_sidebar = true;
        app.show_panel = true;
        app.panel_tab = 1;
        app.terminal_panel_focused = false;
        assert!(!terminal_has_focus(&app));

        app.terminal_panel_focused = true;
        assert!(terminal_has_focus(&app));
    }

    #[test]
    fn focused_terminal_owns_typing_and_shortcuts_while_settings_is_open() {
        use crate::app::TerminalDock;

        for dock in [TerminalDock::Right] {
            for field in ["search", "number", "keybinding"] {
                let mut app = App::test_new();
                app.open_settings_tab();
                app.settings_editor_search = "theme".into();
                app.settings_editor_search_cursor = 1;
                app.settings_editor_search_selection_anchor = Some(4);
                if field == "number" {
                    app.begin_numeric_setting_edit("tab_size", "4");
                } else if field == "keybinding" {
                    app.settings_kb_editing = true;
                }
                let mut pty = crate::terminal::TerminalEmulator::test_with_output(40, 20, "");
                let input = pty.test_capture_input();
                app.terminals[0].terminal_pty = Some(pty);
                app.terminals[0].dock = dock;
                app.show_terminal_sidebar = dock == TerminalDock::Right;
                app.show_panel = false;
                app.panel_tab = 1;
                app.terminal_panel_focused = true;

                assert!(terminal_has_focus(&app));
                assert!(app.focused_text_input_id().is_none());
                handle_keyboard_event(&mut app, key(KeyCode::Char('7')));
                handle_keyboard_event(&mut app, key_mod(KeyCode::Char('a'), KeyModifiers::CONTROL));

                assert_eq!(&*input.lock().unwrap(), b"7\x01", "{dock:?}: {field}");
                assert_eq!(app.settings_editor_search, "theme");
                assert_eq!(app.settings_editor_search_cursor, 1);
                assert_eq!(app.settings_editor_search_selection_anchor, Some(4));
                assert_eq!(app.settings_kb_editing, field == "keybinding");
                if field == "number" {
                    assert!(app.settings_value_editing);
                    assert_eq!(app.settings_value_buffer, "4");
                }

                // Returning focus to Settings restores its normal input route.
                app.terminal_panel_focused = false;
                assert!(app.settings_editor_has_focus());
                if field != "keybinding" {
                    assert!(app.focused_text_input_id().is_some());
                    handle_keyboard_event(&mut app, key_mod(KeyCode::Char('a'), KeyModifiers::CONTROL));
                    handle_keyboard_event(&mut app, key(KeyCode::Char('8')));
                    let value = if field == "number" {
                        &app.settings_value_buffer
                    } else {
                        &app.settings_editor_search
                    };
                    assert_eq!(value, "8");
                    assert_eq!(&*input.lock().unwrap(), b"7\x01");
                }
            }
        }
    }

    #[test]
    fn f11_exits_zen_mode_with_editor_or_terminal_focus() {
        for terminal in [false, true] {
            let mut app = App::test_new();
            let mut tab = crate::app::Tab::new_file(crate::editor::Editor::new());
            if terminal {
                tab.kind = crate::app::TabKind::Terminal;
                tab.terminal_idx = Some(0);
            }
            app.tabs = vec![tab];
            app.active_tab = 0;
            app.active_pane = ActivePane::Editor;
            app.execute_palette_command("toggle_zen");
            assert!(!handle_keyboard_event(&mut app, key(KeyCode::F(11))));
            assert!(!app.zen_mode);
            assert!(app.show_tab_bar);
        }
    }

    #[test]
    fn ctrl_backtick_toggles_right_terminal_with_one_press() {
        let mut app = App::test_new();
        let shortcut = key_mod(KeyCode::Char('`'), KeyModifiers::CONTROL);

        assert!(!app.show_terminal_sidebar);
        assert!(!handle_keyboard_event(&mut app, shortcut));
        assert!(app.show_terminal_sidebar);
        assert!(app.docked_terminal_has_focus());

        assert!(!handle_keyboard_event(&mut app, shortcut));
        assert!(!app.show_terminal_sidebar);
    }

    // ── Exit confirmation dialog ──────────────────────────────────────

    #[test]
    fn ctrl_q_always_opens_exit_confirmation() {
        for modified in [false, true] {
            let mut app = App::test_new();
            let mut editor = crate::editor::Editor::new();
            editor.modified = modified;
            app.tabs = vec![crate::app::Tab::new_file(editor)];
            app.active_tab = 0;
            app.active_pane = ActivePane::Editor;
            app.keybindings.clear();

            assert!(!handle_keyboard_event(
                &mut app,
                key_mod(KeyCode::Char('q'), KeyModifiers::CONTROL),
            ));
            assert!(app.exit_confirm_active);
            assert!(!app.exit_keep_unsaved_quit);
            assert_eq!(app.tabs.len(), 1);
            assert_eq!(app.tabs[0].editor.modified, modified);

            assert!(!handle_keyboard_event(&mut app, key(KeyCode::Esc)));
            assert!(!app.exit_confirm_active);
        }
    }

    #[test]
    fn exit_confirm_y_returns_true() {
        let mut app = App::test_new();
        app.exit_confirm_active = true;
        assert!(handle_keyboard_event(&mut app, key(KeyCode::Char('y'))));
    }

    #[test]
    fn exit_confirm_d_discards_and_exits() {
        let mut app = App::test_new();
        app.exit_confirm_active = true;
        assert!(handle_keyboard_event(&mut app, key(KeyCode::Char('d'))));
        assert!(app.exit_discard_quit, "discard must also suppress unsaved session recovery");
    }

    #[test]
    fn exit_confirm_n_cancels() {
        let mut app = App::test_new();
        app.exit_confirm_active = true;
        let exited = handle_keyboard_event(&mut app, key(KeyCode::Char('n')));
        assert!(!exited);
        assert!(!app.exit_confirm_active);
    }

    #[test]
    fn exit_confirm_esc_cancels() {
        let mut app = App::test_new();
        app.exit_confirm_active = true;
        let exited = handle_keyboard_event(&mut app, key(KeyCode::Esc));
        assert!(!exited);
        assert!(!app.exit_confirm_active);
    }

    #[test]
    fn exit_confirm_k_keeps_unsaved() {
        let mut app = App::test_new();
        app.exit_confirm_active = true;
        let exited = handle_keyboard_event(&mut app, key(KeyCode::Char('k')));
        assert!(!exited);
        assert!(!app.exit_confirm_active);
        assert!(app.exit_keep_unsaved_quit);
    }

    #[test]
    fn exit_confirm_unknown_key_stays_active() {
        let mut app = App::test_new();
        app.exit_confirm_active = true;
        let exited = handle_keyboard_event(&mut app, key(KeyCode::Char('z')));
        assert!(!exited);
        assert!(app.exit_confirm_active);
    }

    #[test]
    fn ctrl_a_is_owned_by_focused_search_field_before_editor_keybindings() {
        let mut app = App::test_new();
        app.search_active = true;
        app.search_input_focused = true;
        app.search_query = "needle".into();
        app.search_cursor = app.search_query.len();
        app.search_selection_anchor = None;

        handle_keyboard_event(&mut app, key_mod(KeyCode::Char('a'), KeyModifiers::CONTROL));

        assert_eq!(app.search_cursor, app.search_query.len());
        assert_eq!(app.search_selection_anchor, Some(0));
    }

    #[test]
    fn ctrl_a_is_owned_by_settings_search_field_before_editor_keybindings() {
        let mut app = App::test_new();
        app.settings_editor_active = true;
        app.settings_focus_categories = false;
        app.active_pane = ActivePane::Editor;
        app.settings_editor_search = "font size".into();
        app.settings_editor_search_cursor = app.settings_editor_search.len();
        app.settings_editor_search_selection_anchor = None;

        handle_keyboard_event(&mut app, key_mod(KeyCode::Char('a'), KeyModifiers::CONTROL));

        assert_eq!(
            app.settings_editor_search_cursor,
            app.settings_editor_search.len()
        );
        assert_eq!(app.settings_editor_search_selection_anchor, Some(0));
    }

    #[test]
    fn numeric_settings_input_precedes_custom_digit_keybindings() {
        let mut app = App::test_new();
        app.settings_editor_active = true;
        app.active_pane = ActivePane::Editor;
        let binding = app
            .keybindings
            .iter_mut()
            .find(|binding| binding.id == "toggle_explorer")
            .expect("toggle explorer keybinding");
        binding.modifiers = KeyModifiers::empty();
        binding.code = KeyCode::Char('7');
        let explorer_was_visible = app.show_explorer;
        app.begin_numeric_setting_edit("tab_size", "4");

        handle_keyboard_event(&mut app, key(KeyCode::Char('7')));

        assert_eq!(app.settings_value_buffer, "7");
        assert_eq!(app.show_explorer, explorer_was_visible);
    }

    #[test]
    fn ctrl_x_does_not_fall_through_to_editor_when_search_field_is_focused() {
        let mut app = App::test_new();
        let mut editor = crate::editor::Editor::test_with_text("editor text");
        editor.cursor_line = 0;
        editor.cursor_col = 6;
        editor.selection_anchor = Some((0, 0));
        app.tabs.push(crate::app::Tab::new_file(editor));
        app.active_tab = 0;
        app.search_active = true;
        app.search_input_focused = true;
        app.search_query = "needle".into();
        app.search_cursor = app.search_query.len();
        app.search_selection_anchor = None;

        handle_keyboard_event(&mut app, key_mod(KeyCode::Char('x'), KeyModifiers::CONTROL));

        assert_eq!(
            app.editor().map(|editor| editor.rope.to_string()),
            Some("editor text".to_string())
        );
        assert_eq!(app.search_query, "needle");
    }

    #[test]
    fn altgr_characters_are_owned_by_the_focused_search_field() {
        let mut app = App::test_new();
        app.active_pane = ActivePane::Editor;
        app.tabs.push(crate::app::Tab::new_file(
            crate::editor::Editor::test_with_text("editor"),
        ));
        app.active_tab = 0;
        app.search_active = true;
        app.search_input_focused = true;

        for character in ['\\', '}'] {
            handle_keyboard_event(
                &mut app,
                key_mod(
                    KeyCode::Char(character),
                    KeyModifiers::CONTROL | KeyModifiers::ALT,
                ),
            );
        }

        assert_eq!(app.search_query, "\\}");
        assert_eq!(app.editor().expect("editor").rope.to_string(), "editor");
    }

    #[test]
    fn visible_unfocused_search_does_not_capture_editor_typing() {
        let mut app = App::test_new();
        app.active_pane = ActivePane::Editor;
        app.tabs.push(crate::app::Tab::new_file(
            crate::editor::Editor::test_with_text("editor"),
        ));
        app.active_tab = 0;
        app.search_active = true;
        app.search_input_focused = false;
        app.search_query = "needle".into();

        handle_keyboard_event(&mut app, key(KeyCode::Char('x')));
        handle_keyboard_event(
            &mut app,
            key_mod(
                KeyCode::Char('\\'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            ),
        );

        assert_eq!(app.search_query, "needle");
        assert_eq!(app.editor().expect("editor").rope.to_string(), "x\\editor");
    }

    // ── Menu bar ──────────────────────────────────────────────────────

    #[test]
    fn menu_esc_closes() {
        let mut app = App::test_new();
        app.menu_active = true;
        app.menu_open = true;
        handle_keyboard_event(&mut app, key(KeyCode::Esc));
        assert!(!app.menu_active);
        assert!(!app.menu_open);
    }

    #[test]
    fn menu_left_right_cycles_selection() {
        let mut app = App::test_new();
        app.menu_active = true;
        app.menu_open = true;
        app.menu_selected = 0;
        app.menu_item_selected = 2;

        handle_keyboard_event(&mut app, key(KeyCode::Right));
        assert_eq!(app.menu_selected, 1);
        assert_eq!(app.menu_item_selected, 0);

        // Wrap around with Left from 0 -> last
        app.menu_selected = 0;
        handle_keyboard_event(&mut app, key(KeyCode::Left));
        let count = App::menu_labels().len();
        assert_eq!(app.menu_selected, count - 1);
    }

    #[test]
    fn menu_down_opens_closed_menu() {
        let mut app = App::test_new();
        app.menu_active = true;
        app.menu_open = false;
        handle_keyboard_event(&mut app, key(KeyCode::Down));
        assert!(app.menu_open);
        assert_eq!(app.menu_item_selected, 0);
    }

    // ── Change directory dialog ───────────────────────────────────────

    #[test]
    fn change_dir_esc_closes() {
        let mut app = App::test_new();
        app.change_dir_active = true;
        let exited = handle_keyboard_event(&mut app, key(KeyCode::Esc));
        assert!(!exited);
        assert!(!app.change_dir_active);
    }

    // ── Tab context menu ──────────────────────────────────────────────

    #[test]
    fn tab_context_esc_closes() {
        let mut app = App::test_new();
        app.tab_context_active = true;
        handle_keyboard_event(&mut app, key(KeyCode::Esc));
        assert!(!app.tab_context_active);
    }

    #[test]
    fn tab_context_up_down_navigates() {
        let mut app = App::test_new();
        app.tab_context_active = true;
        app.tab_context_selected = 0;

        handle_keyboard_event(&mut app, key(KeyCode::Down));
        assert!(app.tab_context_selected >= 1);

        let saved = app.tab_context_selected;
        handle_keyboard_event(&mut app, key(KeyCode::Up));
        assert!(app.tab_context_selected < saved);
    }

    // ── Shortcuts reference ───────────────────────────────────────────

    #[test]
    fn shortcuts_esc_dismisses() {
        let mut app = App::test_new();
        app.shortcuts_active = true;
        app.active_pane = ActivePane::Editor;
        let exited = handle_keyboard_event(&mut app, key(KeyCode::Esc));
        assert!(!exited);
        assert!(!app.shortcuts_active);
    }

    // ── Ctrl+F activates search ───────────────────────────────────────

    #[test]
    fn ctrl_f_activates_search() {
        let mut app = App::test_new();
        app.tabs.push(crate::app::Tab::new_file(crate::editor::Editor::new()));
        app.active_pane = ActivePane::Editor;
        app.search_active = false;
        let exited =
            handle_keyboard_event(&mut app, key_mod(KeyCode::Char('f'), KeyModifiers::CONTROL));
        assert!(!exited);
        assert!(app.search_active);
        assert!(app.search_input_focused);
        assert!(!app.replace_mode);
    }

    // ── Ctrl+H activates search & replace ─────────────────────────────

    #[test]
    fn ctrl_h_activates_replace() {
        let mut app = App::test_new();
        app.tabs.push(crate::app::Tab::new_file(crate::editor::Editor::new()));
        app.active_pane = ActivePane::Editor;
        app.search_active = false;
        let exited =
            handle_keyboard_event(&mut app, key_mod(KeyCode::Char('h'), KeyModifiers::CONTROL));
        assert!(!exited);
        assert!(app.search_active);
        assert!(app.search_input_focused);
        assert!(app.replace_mode);
    }

    // ── Ctrl+P activates quick open ───────────────────────────────────

    #[test]
    fn ctrl_p_activates_quick_open() {
        let mut app = App::test_new();
        app.active_pane = ActivePane::Editor;
        app.quick_open_active = false;
        let exited =
            handle_keyboard_event(&mut app, key_mod(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert!(!exited);
        assert!(app.quick_open_active);
    }
}
