use crate::text_input::TextInputId;
use crossterm::event::{KeyCode, KeyModifiers};
use crate::app::App;

pub(crate) fn handle_quick_open_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        KeyCode::Esc => {
            app.quick_open_active = false;
        }
        KeyCode::Enter => {
            if let Some(path) = app.quick_open_results.get(app.quick_open_selected).cloned() {
                app.open_file(&path);
            }
            app.quick_open_active = false;
        }
        KeyCode::Up => {
            if app.quick_open_selected > 0 {
                app.quick_open_selected -= 1;
            }
        }
        KeyCode::Down => {
            if app.quick_open_selected + 1 < app.quick_open_results.len() {
                app.quick_open_selected += 1;
            }
        }
        KeyCode::PageUp => {
            app.quick_open_selected = app.quick_open_selected.saturating_sub(10);
        }
        KeyCode::PageDown => {
            let max = app.quick_open_results.len().saturating_sub(1);
            app.quick_open_selected = (app.quick_open_selected + 10).min(max);
        }
        _ => {
            if app.edit_text_input(
                TextInputId::QuickOpen,
                code,
                modifiers,
                None,
            ).changed {
                app.update_quick_open();
            }
        }
    }
}

pub(crate) fn handle_git_branch_picker_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::ui_git_branches::handle_key(app, code, modifiers);
}

pub(crate) fn handle_goto_line_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        KeyCode::Esc => {
            app.cancel_goto_line();
        }
        KeyCode::Enter => {
            app.goto_line();
        }
        _ => {
            let result = app.edit_text_input(
                TextInputId::GotoLine,
                code,
                modifiers,
                None,
            );
            if result.changed {
                if app.goto_line_query.is_empty() {
                    // Restore original position when query is cleared.
                    app.restore_goto_line_origin();
                } else {
                    app.preview_goto_line();
                }
            }
        }
    }
}

pub(crate) fn handle_symbol_list_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    match code {
        KeyCode::Esc => {
            app.symbol_list_active = false;
            app.source_symbol_work.cancel_picker();
        }
        KeyCode::Enter => {
            if let Some(&idx) = app.symbol_list_filtered.get(app.symbol_list_selected) {
                if let Some(item) = app.symbol_list_items.get(idx).cloned() {
                    app.push_nav_history();
                    app.symbol_list_active = false;
                    app.source_symbol_work.cancel_picker();
                        if let Some(tab_idx) = item.tab_index {
                            if tab_idx < app.tabs.len() {
                                app.sync_duplicate_file_tabs();
                                app.active_tab = tab_idx;
                                app.validate_diff_state_for_active_tab();
                            }
                        } else if let Some(ref fp) = item.file_path {
                            app.open_file(fp);
                    }
                    if let Some(ed) = app.editor_mut() {
                        ed.clear_selection();
                        ed.set_cursor(item.line, 0);
                    }
                }
            }
        }
        KeyCode::Up => {
            if app.symbol_list_selected > 0 {
                app.symbol_list_selected -= 1;
            }
        }
        KeyCode::Down => {
            if app.symbol_list_selected + 1 < app.symbol_list_filtered.len() {
                app.symbol_list_selected += 1;
            }
        }
        KeyCode::PageUp => {
            app.symbol_list_selected = app.symbol_list_selected.saturating_sub(10);
        }
        KeyCode::PageDown => {
            let max = app.symbol_list_filtered.len().saturating_sub(1);
            app.symbol_list_selected = (app.symbol_list_selected + 10).min(max);
        }
        _ => {
            if app.edit_text_input(
                TextInputId::SymbolList,
                code,
                modifiers,
                None,
            ).changed {
                app.update_symbol_filter();
            }
        }
    }
}

pub(crate) fn handle_palette_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) -> bool {
    if app.theme_install_prompt_visible() {
        match code {
            KeyCode::Esc => app.close_palette(),
            KeyCode::Enter => app.start_theme_install(),
            _ if !app.install_prompt_running() => {
                if app.edit_text_input(TextInputId::CommandPalette, code, modifiers, None).changed {
                    app.palette.theme_install_error = None;
                }
            }
            _ => {}
        }
        return false;
    }

    match code {
        KeyCode::Esc => {
            app.close_palette();
        }
        KeyCode::Enter => {
            if let Some(&idx) = app.palette.filtered.get(app.palette.selected) {
                let recent_id = app.palette_recent_key_for_index(idx);
                // Record as recently used
                app.palette.recent.retain(|r| r != &recent_id);
                app.palette.recent.insert(0, recent_id);
                if app.palette.recent.len() > 10 {
                    app.palette.recent.truncate(10);
                }
                if app.palette_selected_theme_key().is_some() {
                    app.palette.theme_preview_original = None;
                }
                app.close_palette();
                if app.execute_extension_palette_index(idx) {
                    return false;
                }
                let Some(cmd) = app.palette.commands.get(idx) else { return false; };
                let id = cmd.id.to_string();
                // `execute_palette_command` reports whether a sub-dispatcher
                // handled the command; it is not an application-quit signal.
                app.execute_palette_command(&id);
                return false;
            }
        }
        KeyCode::Up => {
            if app.palette.selected > 0 {
                app.palette.selected -= 1;
            }
        }
        KeyCode::Down => {
            if app.palette.selected + 1 < app.palette.filtered.len() {
                app.palette.selected += 1;
            }
        }
        KeyCode::PageUp => {
            app.palette.selected = app.palette.selected.saturating_sub(10);
        }
        KeyCode::PageDown => {
            let max = app.palette.filtered.len().saturating_sub(1);
            app.palette.selected = (app.palette.selected + 10).min(max);
        }
        _ => {
            if app.edit_text_input(
                TextInputId::CommandPalette,
                code,
                modifiers,
                None,
            ).changed {
                app.update_palette_filter();
            }
        }
    }
    // Keep scroll offset in sync so the selected item is always visible
    let visible = crate::ui_quick_picker::visible_rows(app, crate::ui_quick_picker::Kind::Commands);
    if app.palette.selected < app.palette.scroll_offset {
        app.palette.scroll_offset = app.palette.selected;
    } else if app.palette.selected >= app.palette.scroll_offset + visible {
        app.palette.scroll_offset = app.palette.selected.saturating_sub(visible - 1);
    }
    app.preview_palette_theme();
    false
}

pub(crate) fn handle_context_menu_key(app: &mut App, code: KeyCode) -> bool {
    app.context_menu_button_drag = None;
    match code {
        KeyCode::Home => app.context_menu_selected = 0,
        KeyCode::End => app.context_menu_selected = app.context_menu_items.len().saturating_sub(1),
        KeyCode::Up => {
            if app.context_menu_selected > 0 {
                app.context_menu_selected -= 1;
            }
        }
        KeyCode::Down => {
            if app.context_menu_selected + 1 < app.context_menu_items.len() {
                app.context_menu_selected += 1;
            }
        }
        KeyCode::Enter | KeyCode::Char(' ') if code == KeyCode::Enter || app.source_control_filter_menu_open() || app.source_control_action_menu_open() => {
            let Some(item) = app.context_menu_items.get(app.context_menu_selected) else { return false; };
            let id = item.id;
            if app.source_control_menu_command_disabled(id) { return false; }
            app.context_menu_active = false;
            if id == "command_palette" {
                app.open_palette();
            } else {
                app.execute_palette_command(id);
            }
        }
        _ => {
            app.context_menu_active = false;
        }
    }
    false
}

pub(crate) fn handle_confirm_close_key(app: &mut App, code: KeyCode) {
    let idx = app.confirm_close_tab;
    if matches!(code, KeyCode::Char('y' | 'Y' | 'n' | 'N'))
        && app.tabs.get(idx).is_some_and(|tab| app.tab_operation_pending(tab))
    {
        app.notify("Wait for the current save or extension operation before closing this tab");
        return;
    }
    match code {
        KeyCode::Char('y' | 'Y') => {
            // Save and close
            if app.queue_extension_document_close(idx, true) { return; }
            let trim = app.trim_trailing_on_save;
            let newline = app.insert_final_newline;
            if idx < app.tabs.len() {
                if let Err(error) = app.tabs[idx].editor.save_file_with_options(trim, newline) {
                    if let Some(path) = app.tabs[idx].editor.file_path.clone() {
                        app.show_file_save_error(&path, app.tabs[idx].editor.save_identity, app.workspace_generation, error);
                    } else { app.show_save_error(format!("Could not save file: {error}")); }
                    return;
                }
                app.force_close_tab(idx);
            }
        }
        KeyCode::Char('n' | 'N') => {
            // Close without saving
            if app.queue_extension_document_close(idx, false) { return; }
            app.force_close_tab(idx);
        }
        KeyCode::Esc => {
            app.confirm_close_active = false;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use std::path::PathBuf;
    use crate::app::{App, Tab, ContextMenuItem};
    use crate::editor::Editor;

    fn make_app() -> App {
        let mut app = App::new(PathBuf::from("."));
        app.tabs = vec![Tab::new_file(Editor::new())];
        app.active_tab = 0;
        app
    }

    // --- Quick Open ---

    #[test]
    fn quick_open_esc_deactivates() {
        let mut app = make_app();
        app.quick_open_active = true;
        handle_quick_open_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.quick_open_active);
    }

    #[test]
    fn quick_open_up_decrements_selected() {
        let mut app = make_app();
        app.quick_open_active = true;
        app.quick_open_results = vec![PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")];
        app.quick_open_selected = 2;
        handle_quick_open_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.quick_open_selected, 1);
    }

    #[test]
    fn quick_open_down_increments_selected() {
        let mut app = make_app();
        app.quick_open_active = true;
        app.quick_open_results = vec![PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")];
        app.quick_open_selected = 0;
        handle_quick_open_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.quick_open_selected, 1);
    }

    #[test]
    fn quick_open_down_clamped_at_end() {
        let mut app = make_app();
        app.quick_open_active = true;
        app.quick_open_results = vec![PathBuf::from("a")];
        app.quick_open_selected = 0;
        handle_quick_open_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.quick_open_selected, 0);
    }

    #[test]
    fn quick_open_page_up_jumps_10() {
        let mut app = make_app();
        app.quick_open_selected = 15;
        handle_quick_open_key(&mut app, KeyCode::PageUp, KeyModifiers::NONE);
        assert_eq!(app.quick_open_selected, 5);
    }

    #[test]
    fn quick_open_page_down_jumps_10() {
        let mut app = make_app();
        app.quick_open_results = (0..20).map(|i| PathBuf::from(format!("{i}"))).collect();
        app.quick_open_selected = 5;
        handle_quick_open_key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        assert_eq!(app.quick_open_selected, 15);
    }

    #[test]
    fn quick_open_enter_deactivates() {
        let mut app = make_app();
        app.quick_open_active = true;
        handle_quick_open_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(!app.quick_open_active);
    }

    // --- Goto Line ---

    #[test]
    fn goto_line_esc_cancels() {
        let mut app = make_app();
        app.goto_line_active = true;
        handle_goto_line_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.goto_line_active);
    }

    #[test]
    fn goto_line_digit_inserts_char() {
        let mut app = make_app();
        app.goto_line_active = true;
        app.goto_line_query.clear();
        app.goto_line_cursor = 0;
        handle_goto_line_key(&mut app, KeyCode::Char('5'), KeyModifiers::NONE);
        assert_eq!(app.goto_line_query, "5");
    }

    #[test]
    fn goto_line_colon_inserts_char() {
        let mut app = make_app();
        app.goto_line_active = true;
        app.goto_line_query = "10".to_string();
        app.goto_line_cursor = 2;
        handle_goto_line_key(&mut app, KeyCode::Char(':'), KeyModifiers::NONE);
        assert_eq!(app.goto_line_query, "10:");
    }

    #[test]
    fn goto_line_backspace_removes_char() {
        let mut app = make_app();
        app.goto_line_query = "42".to_string();
        app.goto_line_cursor = 2;
        handle_goto_line_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(app.goto_line_query, "4");
    }

    // --- Symbol List ---

    #[test]
    fn symbol_list_esc_deactivates() {
        let mut app = make_app();
        app.symbol_list_active = true;
        handle_symbol_list_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.symbol_list_active);
    }

    #[test]
    fn symbol_list_up_decrements() {
        let mut app = make_app();
        app.symbol_list_filtered = vec![0, 1, 2];
        app.symbol_list_selected = 2;
        handle_symbol_list_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.symbol_list_selected, 1);
    }

    #[test]
    fn symbol_list_down_increments() {
        let mut app = make_app();
        app.symbol_list_filtered = vec![0, 1, 2];
        app.symbol_list_selected = 0;
        handle_symbol_list_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.symbol_list_selected, 1);
    }

    #[test]
    fn symbol_list_home_moves_text_cursor_to_start() {
        let mut app = make_app();
        app.symbol_list_filtered = vec![0, 1, 2];
        app.symbol_list_selected = 2;
        app.symbol_list_query = "abc".to_owned();
        app.symbol_list_cursor = 2;
        handle_symbol_list_key(&mut app, KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(app.symbol_list_cursor, 0);
        assert_eq!(app.symbol_list_selected, 2);
    }

    #[test]
    fn symbol_list_end_moves_text_cursor_to_end() {
        let mut app = make_app();
        app.symbol_list_filtered = vec![0, 1, 2];
        app.symbol_list_selected = 0;
        app.symbol_list_query = "abc".to_owned();
        app.symbol_list_cursor = 1;
        handle_symbol_list_key(&mut app, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(app.symbol_list_cursor, 3);
        assert_eq!(app.symbol_list_selected, 0);
    }

    // --- Palette ---

    #[test]
    fn palette_esc_deactivates() {
        let mut app = make_app();
        app.palette.active = true;
        handle_palette_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.palette.active);
    }

    #[test]
    fn palette_up_decrements() {
        let mut app = make_app();
        app.palette.filtered = vec![0, 1, 2];
        app.palette.selected = 2;
        handle_palette_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.palette.selected, 1);
    }

    #[test]
    fn palette_down_increments() {
        let mut app = make_app();
        app.palette.filtered = vec![0, 1, 2];
        app.palette.selected = 0;
        handle_palette_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.palette.selected, 1);
    }

    #[test]
    fn palette_home_moves_text_cursor_to_start() {
        let mut app = make_app();
        app.palette.filtered = vec![0, 1, 2];
        app.palette.selected = 2;
        app.palette.query = "abc".to_owned();
        app.palette.cursor = 2;
        handle_palette_key(&mut app, KeyCode::Home, KeyModifiers::NONE);
        assert_eq!(app.palette.cursor, 0);
        assert_eq!(app.palette.selected, 2);
    }

    #[test]
    fn palette_end_moves_text_cursor_to_end() {
        let mut app = make_app();
        app.palette.filtered = vec![0, 1, 2];
        app.palette.selected = 0;
        app.palette.query = "abc".to_owned();
        app.palette.cursor = 1;
        handle_palette_key(&mut app, KeyCode::End, KeyModifiers::NONE);
        assert_eq!(app.palette.cursor, 3);
        assert_eq!(app.palette.selected, 0);
    }

    #[test]
    fn palette_scroll_offset_tracks_selected() {
        let mut app = make_app();
        app.palette.filtered = (0..30).collect();
        app.palette.selected = 0;
        app.palette.scroll_offset = 0;
        // Move down 20 times
        for _ in 0..20 {
            handle_palette_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        }
        assert!(app.palette.scroll_offset > 0);
    }

    #[test]
    fn applying_theme_from_palette_does_not_request_application_exit() {
        let mut app = make_app();
        let Some((index, command)) = app
            .palette
            .commands
            .iter()
            .enumerate()
            .find(|(_, command)| command.id.starts_with("theme_"))
        else {
            return;
        };
        let command_id = command.id.to_string();
        app.palette.active = true;
        app.palette.filtered = vec![index];
        app.palette.selected = 0;

        let quit = handle_palette_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

        assert!(!quit, "palette commands must not be interpreted as quit requests");
        assert!(!app.palette.active);
        assert_eq!(
            crate::app_theme::config_key_from_command_id(&command_id).as_deref(),
            Some(app.theme_key.as_str())
        );
    }

    // --- Context Menu ---

    #[test]
    fn context_menu_up_decrements() {
        let mut app = make_app();
        app.context_menu_items = vec![
            ContextMenuItem { id: "a", label: "A".into(), shortcut: String::new(), separator_after: false },
            ContextMenuItem { id: "b", label: "B".into(), shortcut: String::new(), separator_after: false },
        ];
        app.context_menu_selected = 1;
        handle_context_menu_key(&mut app, KeyCode::Up);
        assert_eq!(app.context_menu_selected, 0);
    }

    #[test]
    fn context_menu_down_increments() {
        let mut app = make_app();
        app.context_menu_items = vec![
            ContextMenuItem { id: "a", label: "A".into(), shortcut: String::new(), separator_after: false },
            ContextMenuItem { id: "b", label: "B".into(), shortcut: String::new(), separator_after: false },
        ];
        app.context_menu_selected = 0;
        handle_context_menu_key(&mut app, KeyCode::Down);
        assert_eq!(app.context_menu_selected, 1);
    }

    #[test]
    fn context_menu_other_key_deactivates() {
        let mut app = make_app();
        app.context_menu_active = true;
        handle_context_menu_key(&mut app, KeyCode::Char('q'));
        assert!(!app.context_menu_active);
    }

    // --- Confirm Close ---

    #[test]
    fn confirm_close_esc_deactivates() {
        let mut app = make_app();
        app.confirm_close_active = true;
        handle_confirm_close_key(&mut app, KeyCode::Esc);
        assert!(!app.confirm_close_active);
    }

    #[test]
    fn confirm_close_n_closes_tab() {
        let mut app = make_app();
        app.tabs = vec![Tab::new_file(Editor::new()), Tab::new_file(Editor::new())];
        app.confirm_close_active = true;
        app.confirm_close_tab = 0;
        let count_before = app.tabs.len();
        handle_confirm_close_key(&mut app, KeyCode::Char('n'));
        assert!(app.tabs.len() < count_before);
    }

    #[test]
    fn confirm_close_save_failure_keeps_tab_open() {
        let mut app = make_app();
        app.tabs = vec![Tab::new_file(Editor::new())];
        app.tabs[0].editor.modified = true;
        app.confirm_close_active = true;
        app.confirm_close_tab = 0;

        handle_confirm_close_key(&mut app, KeyCode::Char('y'));

        assert_eq!(app.tabs.len(), 1);
        assert!(app.confirm_close_active);
        assert!(!app.notification_history.is_empty());
    }
}
