use crate::text_input::TextInputId;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::App;
#[cfg(test)]
use crate::app::ActivePane;

/// Save one current snapshot per path and retain all documents on failure.
/// Both keyboard and mouse exit paths use this result before allowing exit.
pub(crate) fn save_text_documents_before_exit(app: &mut App) -> Result<(), String> {
    if app.paste_in_progress() {
        return Err("Wait for the paste to finish or cancel it before Save and Quit".into());
    }
    if !app.pending_file_saves.is_empty() || !app.deferred_file_saves.is_empty() {
        return Err("Wait for pending file saves before quitting".into());
    }
    app.sync_duplicate_file_tabs();
    let mut jobs = rustc_hash::FxHashMap::default();
    for tab in app.tabs.iter().chain(app.groups.iter().flat_map(|group| &group.tabs)) {
        if tab.kind != crate::app::TabKind::File || !tab.has_unsaved_changes() { continue; }
        let editor = &tab.editor;
        if editor.read_only || editor.bg_loading {
            return Err(format!("Cannot save {} while it is read-only or loading", editor.file_name()));
        }
        let request = editor.background_save_request(app.trim_trailing_on_save, app.insert_final_newline, false)
            .ok_or("Use Save As for untitled documents before Save and Quit, or choose Keep Unsaved")?;
        if jobs.get(&request.path).is_none_or(|previous: &crate::editor::BackgroundSaveRequest|
            request.source_content_version > previous.source_content_version)
        {
            jobs.insert(request.path.clone(), request);
        }
    }
    let threshold = app.large_file_threshold_mb.saturating_mul(1024 * 1024);
    if app.remote_workspace.is_some() && !jobs.is_empty() {
        if !app.remote_workspace.as_ref().unwrap().client.connected() {
            return Err("Remote connection is unavailable. Choose Keep unsaved & quit to preserve your local edits.".into());
        }
        for (_, request) in jobs {
            if !app.queue_background_save_job(request) { return Err("Remote save could not start. Your edits remain open.".into()); }
        }
        return Err("Saving remote files…".into());
    }
    for (_, request) in jobs {
        let saved = match crate::editor::write_background_save(&request) {
            Ok(saved) => saved,
            Err(error) => {
                let message = format!("Could not save {}: {error}. Your edits remain open.", request.path.display());
                app.show_file_save_error(&request.path, request.source_document_id, app.workspace_generation, error);
                return Err(message);
            }
        };
        let modified = saved.metadata.as_ref().and_then(|metadata| metadata.modified().ok());
        let size = saved.metadata.as_ref().map(std::fs::Metadata::len);
        for tab in app.tabs.iter_mut().chain(app.groups.iter_mut().flat_map(|group| &mut group.tabs)) {
            if tab.kind == crate::app::TabKind::File && tab.editor.file_path.as_ref() == Some(&request.path) {
                tab.editor.apply_background_save(request.source_content_version, &request.rope,
                    saved.rope.clone(), request.encoding, modified, size, saved.transformed_lines, threshold);
            }
        }
    }
    Ok(())
}

/// Handle exit confirmation dialog.
/// Returns `Some(should_quit)` when consumed, `None` when dialog is not active.
pub(crate) fn handle_exit_confirm_key(app: &mut App, key: KeyEvent) -> Option<bool> {
    handle_exit_confirm_key_inner(app, key, false)
}

pub(crate) fn confirm_large_backup(app: &mut App) -> bool {
    let Some(previous) = app.exit_backup_warning.take() else { return false; };
    let current = app.large_backup_warning();
    if current.as_ref().is_some_and(|current| current != &previous) {
        app.exit_backup_warning = current;
        app.modal = crate::ui_modal::ModalState { kind: Some(crate::ui_modal::Kind::ExitBackup),
            focus: 1, error: Some("Files changed. Review the updated backup sizes before quitting.".into()),
            ..Default::default() };
        return false;
    }
    app.exit_recovery.approved = current;
    handle_exit_confirm_key_inner(app, KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE), true)
        .unwrap_or(false)
}

fn handle_exit_confirm_key_inner(app: &mut App, key: KeyEvent, backup_confirmed: bool) -> Option<bool> {
    if !app.exit_confirm_active {
        return None;
    }
    if !backup_confirmed {
        if app.exit_backup_warning.is_some() {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('n' | 'N')) {
                app.exit_backup_warning = None;
            }
            return Some(false);
        }
        if matches!(key.code, KeyCode::Char('k' | 'K')) {
            app.exit_recovery.approved = None;
            app.exit_backup_warning = app.large_backup_warning();
            if app.exit_backup_warning.is_some() {
                // Do not disconnect remote saves or start recovery work until
                // the user approves the additional disk usage.
                app.modal = Default::default();
                return Some(false);
            }
        }
    }
    if matches!(key.code, KeyCode::Enter | KeyCode::Char('y' | 'Y' | 'd' | 'D' | 'k' | 'K'))
        && (!app.pending_file_saves.is_empty() || !app.deferred_file_saves.is_empty())
    {
        let keep_remote = matches!(key.code, KeyCode::Char('k' | 'K'))
            && app.remote_workspace.is_some()
            && app.pending_file_saves.keys().chain(app.deferred_file_saves.keys())
                .all(|path| crate::remote::workspace::is_view_key(path));
        if keep_remote {
            // A save may have reached disk without its acknowledgement. Stop
            // waiting, retain the local buffer/checkpoint for recovery, and
            // let the worker publish failure rather than mark it as saved.
            app.remote_workspace.as_ref().unwrap().disconnect();
        } else {
            let message = if app.remote_workspace.is_some() {
                "A remote save is pending. Choose Keep unsaved & quit to preserve local edits without waiting for the connection."
            } else { "Wait for pending file saves before quitting" };
            app.modal.error = Some(message.into());
            app.notify(message);
            return Some(false);
        }
    }
    match key.code {
        KeyCode::Char('y' | 'Y') | KeyCode::Enter => {
            // Save all modified files, then quit
            if app.start_custom_exit(false) {
                app.exit_save_and_quit = true;
                app.exit_confirm_active = false;
                return Some(false);
            }
            if let Err(error) = save_text_documents_before_exit(app) {
                if app.remote_workspace.is_some() && !app.pending_file_saves.is_empty() {
                    app.exit_save_and_quit = true;
                    app.exit_confirm_active = false;
                    return Some(false);
                }
                app.custom_exit_started = false;
                if app.save_error.is_none() { app.show_save_error(error); }
                return Some(false);
            }
            return Some(true);
        }
        KeyCode::Char('d' | 'D') => {
            // Discard and quit without saving
            app.exit_discard_quit = true;
            if app.start_custom_discard_exit() {
                app.exit_confirm_active = false;
                return Some(false);
            }
            return Some(true);
        }
        KeyCode::Char('k' | 'K') => {
            // Keep unsaved content in session and quit
            // Session auto-saves on exit which preserves unsaved content
            app.exit_keep_unsaved_quit = true;
            app.exit_confirm_active = false;
        }
        KeyCode::Char('n' | 'N') | KeyCode::Esc => {
            app.exit_confirm_active = false;
        }
        _ => {}
    }
    Some(false)
}

/// Handle change-directory folder browser dialog.
pub(crate) fn handle_change_dir_key(app: &mut App, key: KeyEvent) -> Option<bool> {
    if !app.change_dir_active {
        return None;
    }
    app.folder_browser_key(key);
    Some(false)
}

/// Handle tab context menu keyboard navigation.
pub(crate) fn handle_tab_context_key(app: &mut App, key: KeyEvent) -> Option<bool> {
    if !app.tab_context_active {
        return None;
    }
    match key.code {
        KeyCode::Esc => {
            app.tab_context_active = false;
        }
        KeyCode::Up => {
            let items = App::tab_context_items();
            if app.tab_context_selected > 0 {
                app.tab_context_selected -= 1;
                while app.tab_context_selected > 0
                    && (items[app.tab_context_selected].0 == "─"
                        || app.tab_context_command_disabled(items[app.tab_context_selected].1))
                {
                    app.tab_context_selected -= 1;
                }
            }
        }
        KeyCode::Down => {
            let items = App::tab_context_items();
            let max = items.len().saturating_sub(1);
            if app.tab_context_selected < max {
                app.tab_context_selected += 1;
                while app.tab_context_selected < max
                    && (items[app.tab_context_selected].0 == "─"
                        || app.tab_context_command_disabled(items[app.tab_context_selected].1))
                {
                    app.tab_context_selected += 1;
                }
            }
        }
        KeyCode::Enter => {
            let items = App::tab_context_items();
            if let Some((_, cmd, _)) = items.get(app.tab_context_selected) {
                if cmd.is_empty() || app.tab_context_command_disabled(cmd) {
                    return Some(false);
                }
                let tab = app.tab_context_tab;
                app.tab_context_active = false;
                if cmd.starts_with("tab_group_") || matches!(*cmd, "tab_move_desk" | "tab_copy_desk") {
                    app.sync_duplicate_file_tabs();
                    app.active_tab = tab;
                    crate::ui_desks::command(app, cmd);
                    return Some(false);
                }
                match *cmd {
                    "tab_close" => {
                        app.close_tab(tab);
                    }
                    "tab_close_others" => {
                        app.close_tabs_matching(|i| i != tab);
                    }
                    "tab_close_all" => {
                        app.close_tabs_matching(|_| true);
                    }
                    "tab_close_saved" => {
                        app.close_saved_tabs();
                    }
                    "tab_close_right" => {
                        app.close_tabs_matching(|i| i > tab);
                    }
                    "tab_close_left" => {
                        app.close_tabs_matching(|i| i < tab);
                    }
                    "tab_copy_path" => {
                        if let Some(path) =
                            app.tabs.get(tab).and_then(|t| t.editor.file_path.as_ref())
                        {
                            let path_str = path.display().to_string();
                            if crate::clipboard::set_text(&path_str).is_ok() {
                                app.notify(format!("Copied: {path_str}"));
                            }
                        }
                    }
                    "tab_copy_rel_path" => {
                        if let Some(path) =
                            app.tabs.get(tab).and_then(|t| t.editor.file_path.as_ref())
                        {
                            let rel = path.strip_prefix(&app.explorer.root).unwrap_or(path);
                            let path_str = rel.display().to_string();
                            if crate::clipboard::set_text(&path_str).is_ok() {
                                app.notify(format!("Copied: {path_str}"));
                            }
                        }
                    }
                    "tab_pin" => {
                        if let Some(t) = app.tabs.get_mut(tab) {
                            t.pinned = !t.pinned;
                        }
                    }
                    "tab_split_right" => {
                        app.create_split_group_dir(tab, crate::app::SplitDirection::Horizontal);
                        app.notify("Split Right");
                    }
                    "tab_split_down" => {
                        app.create_split_group_dir(tab, crate::app::SplitDirection::Vertical);
                        app.notify("Split Down");
                    }
                    "tab_close_split" => {
                        app.close_all_splits();
                        app.notify("Split closed");
                    }
                    "tab_reveal" => {
                        if let Some(path) =
                            app.tabs.get(tab).and_then(|t| t.editor.file_path.clone())
                        {
                            app.explorer.select_path(&path);
                            app.show_explorer = true;
                        }
                    }
                    "tab_open_folder" => {
                        if let Some(parent) = app.tabs.get(tab).and_then(|t| t.editor.file_path.as_ref()).and_then(|p| p.parent()).map(|p| p.to_path_buf()) {
                            app.open_external(crate::external_open::Request::SystemApp(parent));
                        }
                    }
                    "tab_open_system" => {
                        if let Some(path) = app.tabs.get(tab).and_then(|t| t.editor.file_path.clone()) {
                            app.open_external(crate::external_open::Request::SystemApp(path));
                        }
                    }
                    "tab_select_compare" | "tab_compare_selected" => {
                        app.execute_tab_compare_command(tab, cmd);
                    }
                    "duplicate_tab" => app.duplicate_tab(tab),
                    _ => {}
                }
            }
        }
        _ => {}
    }
    Some(false)
}

/// Handle menu bar keyboard navigation.
pub(crate) fn handle_menu_key(app: &mut App, key: KeyEvent) -> Option<bool> {
    if !app.menu_active {
        return None;
    }
    match key.code {
        KeyCode::Esc => {
            app.menu_active = false;
            app.menu_open = false;
        }
        KeyCode::Left => {
            let count = App::menu_labels().len();
            app.menu_selected = if app.menu_selected == 0 {
                count - 1
            } else {
                app.menu_selected - 1
            };
            app.menu_item_selected = 0;
        }
        KeyCode::Right => {
            let count = App::menu_labels().len();
            app.menu_selected = (app.menu_selected + 1) % count;
            app.menu_item_selected = 0;
        }
        KeyCode::Down => {
            if app.menu_open {
                let items = app.menu_items(app.menu_selected);
                let max = items.len().saturating_sub(1);
                app.menu_item_selected = (app.menu_item_selected + 1).min(max);
                // Skip separator items
                while app.menu_item_selected < max {
                    if items[app.menu_item_selected].0 == "─" {
                        app.menu_item_selected += 1;
                    } else {
                        break;
                    }
                }
            } else {
                app.menu_open = true;
                app.menu_item_selected = 0;
            }
        }
        KeyCode::Up => {
            if app.menu_open && app.menu_item_selected > 0 {
                app.menu_item_selected -= 1;
                let items = app.menu_items(app.menu_selected);
                // Skip separator items
                while app.menu_item_selected > 0 && items[app.menu_item_selected].0 == "─" {
                    app.menu_item_selected -= 1;
                }
            }
        }
        KeyCode::Enter => {
            if app.menu_open {
                let items = app.menu_items(app.menu_selected);
                if let Some((label, cmd_id)) = items.get(app.menu_item_selected) {
                    if label == "─" || app.is_menu_cmd_disabled(cmd_id) {
                        return Some(false);
                    }
                    let cmd = cmd_id.to_string();
                    app.menu_active = false;
                    app.menu_open = false;
                    match cmd.as_str() {
                        "quit_menu" => {
                            app.exit_confirm_active = true;
                        }
                        "close_tab_menu" => {
                            app.close_tab(app.active_tab);
                        }
                        "palette_menu" => {
                            app.open_palette();
                        }
                        "shortcuts_ref" => {
                            app.shortcuts_active = !app.shortcuts_active;
                        }
                        other => {
                            app.execute_palette_command(other);
                        }
                    }
                }
            } else {
                app.menu_open = true;
                app.menu_item_selected = 0;
            }
        }
        _ => {}
    }
    Some(false)
}

/// Handle task picker dialog keyboard events.
pub(crate) fn handle_task_picker_key(app: &mut App, key: KeyEvent) -> Option<bool> {
    if !app.task_picker_active {
        return None;
    }
    match key.code {
        KeyCode::Esc => {
            app.task_picker_active = false;
        }
        KeyCode::Enter => {
            let filtered: Vec<usize> = if app.task_picker_query.is_empty() {
                (0..app.task_list.len()).collect()
            } else {
                app.task_list
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| {
                        crate::app::contains_ignore_case(&t.label, &app.task_picker_query)
                    })
                    .map(|(i, _)| i)
                    .collect()
            };
            if let Some(&task_idx) = filtered.get(app.task_picker_selected) {
                let cmd = app.task_list[task_idx].command.clone();
                let label = app.task_list[task_idx].label.clone();
                app.task_picker_active = false;
                app.run_task(&cmd, &label);
            }
        }
        KeyCode::Up => {
            if app.task_picker_selected > 0 {
                app.task_picker_selected -= 1;
            }
        }
        KeyCode::Down => {
            let filtered_len = if app.task_picker_query.is_empty() {
                app.task_list.len()
            } else {
                app.task_list
                    .iter()
                    .filter(|t| crate::app::contains_ignore_case(&t.label, &app.task_picker_query))
                    .count()
            };
            if app.task_picker_selected + 1 < filtered_len {
                app.task_picker_selected += 1;
            }
        }
        _ => {
            let result = app.edit_text_input(
                TextInputId::TaskPicker,
                key.code,
                key.modifiers,
                None,
            );
            if result.changed {
                app.task_picker_selected = 0;
            }
        }
    }
    Some(false)
}

/// Move among controls while keeping decorative section rows in the visual list.
fn move_settings_selection(app: &mut App, filtered: &[usize], down: bool) {
    let items = app.settings_items();
    let selectable = |index: usize| {
        items
            .get(index)
            .is_some_and(|(_, _, id)| App::setting_field_kind(id) != "heading")
    };
    let position = filtered
        .iter()
        .position(|&index| index == app.settings_selected);
    let next = position.and_then(|position| {
        if down {
            filtered[position + 1..]
                .iter()
                .copied()
                .find(|&index| selectable(index))
        } else {
            filtered[..position]
                .iter()
                .rev()
                .copied()
                .find(|&index| selectable(index))
        }
    });
    let next = next.or_else(|| {
        if position.is_some() && selectable(app.settings_selected) {
            Some(app.settings_selected)
        } else {
            filtered.iter().copied().find(|&index| selectable(index))
        }
    });
    if let Some(next) = next {
        app.settings_selected = next;
    }
}

/// Handle settings editor view keyboard events.
pub(crate) fn handle_settings_editor_key(app: &mut App, key: KeyEvent) -> Option<bool> {
    if !app.settings_editor_has_focus() {
        return None;
    }
    // Keybinding capture mode
    if app.settings_kb_editing {
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
        return Some(false);
    }
    if app.settings_value_editing {
        match key.code {
            KeyCode::Esc => app.cancel_numeric_setting_edit(),
            KeyCode::Enter | KeyCode::Tab => {
                let _ = app.commit_numeric_setting_edit();
            }
            _ => {
                app.edit_text_input(
                    TextInputId::SettingsValue,
                    key.code,
                    key.modifiers,
                    None,
                );
            }
        }
        return Some(false);
    }
    if app.settings_dropdown_open {
        match key.code {
            KeyCode::Delete if app.settings_dropdown_setting_id.as_deref() == Some("theme") => {
                app.request_theme_removal(app.settings_dropdown_selected);
            }
            KeyCode::Esc => {
                app.close_settings_dropdown();
            }
            KeyCode::Up => {
                app.settings_dropdown_selected = app.settings_dropdown_selected.saturating_sub(1);
                // Keep scroll so selected is visible
                if app.settings_dropdown_selected < app.settings_dropdown_scroll {
                    app.settings_dropdown_scroll = app.settings_dropdown_selected;
                }
                if let Some(id) = app.settings_dropdown_setting_id.clone() {
                    if let Some(value) = app
                        .settings_dropdown_options
                        .get(app.settings_dropdown_selected)
                        .cloned()
                    {
                        app.preview_settings_dropdown_value(&id, &value);
                    }
                }
            }
            KeyCode::Down => {
                let max = app.settings_dropdown_options.len().saturating_sub(1);
                if app.settings_dropdown_selected < max {
                    app.settings_dropdown_selected += 1;
                }
                // Keep scroll so selected is visible
                if let Some(dd_area) = app.settings_dropdown_area {
                    let visible = dd_area.height as usize;
                    if app.settings_dropdown_selected >= app.settings_dropdown_scroll + visible {
                        app.settings_dropdown_scroll = app
                            .settings_dropdown_selected
                            .saturating_sub(visible.saturating_sub(1));
                    }
                }
                if let Some(id) = app.settings_dropdown_setting_id.clone() {
                    if let Some(value) = app
                        .settings_dropdown_options
                        .get(app.settings_dropdown_selected)
                        .cloned()
                    {
                        app.preview_settings_dropdown_value(&id, &value);
                    }
                }
            }
            KeyCode::Enter => {
                app.select_settings_dropdown_option(app.settings_dropdown_selected);
            }
            KeyCode::Tab | KeyCode::BackTab if app.settings_dropdown_setting_id.is_none() => {
                app.close_settings_dropdown();
                app.settings_focus_categories = false;
                app.settings_focus_control = key.code == KeyCode::Tab
                    && !key.modifiers.contains(KeyModifiers::SHIFT);
                if app.settings_focus_control {
                    app.ensure_settings_selection_visible();
                }
            }
            _ => {}
        }
        return Some(false);
    }
    if key.modifiers.contains(KeyModifiers::ALT)
        && matches!(key.code, KeyCode::Left | KeyCode::Right)
    {
        app.settings_scope = match key.code {
            KeyCode::Left => crate::settings::SettingsScope::Global,
            KeyCode::Right => crate::settings::SettingsScope::Workspace,
            _ => app.settings_scope,
        };
        app.settings_editor_scroll = 0;
        app.ensure_settings_selection_visible();
        app.notify(format!("Editing {} settings", app.settings_scope.label()));
        return Some(false);
    }
    if app.settings_focus_categories
        && app.settings_category_picker_area.is_some()
        && matches!(key.code, KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Up | KeyCode::Down)
    {
        app.open_settings_category_dropdown();
        return Some(false);
    }
    match key.code {
        KeyCode::Esc => {
            // Close the settings tab
            if let Some(idx) = app
                .tabs
                .iter()
                .position(|t| t.kind == crate::app::TabKind::Settings)
            {
                app.close_tab(idx);
            }
            app.settings_editor_active = false;
        }
        KeyCode::Tab | KeyCode::BackTab => {
            let current = if app.settings_focus_categories { 1 }
                else if app.settings_focus_control { 2 } else { 0 };
            let reverse = key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
            let mut next = (current + if reverse { 2 } else { 1 }) % 3;
            if next == 1 && app.settings_category_area.is_none()
                && app.settings_category_picker_area.is_none()
            {
                next = if reverse { 0 } else { 2 };
            }
            app.settings_focus_categories = next == 1;
            app.settings_focus_control = next == 2;
            if app.settings_focus_categories {
                app.reveal_settings_category();
            }
            if app.settings_focus_control {
                app.ensure_settings_selection_visible();
            }
        }
        KeyCode::Up => {
            app.settings_focus_control = !app.settings_focus_categories;
            if app.settings_focus_categories {
                app.move_settings_category(false);
            } else {
                let filtered =
                    app.settings_filtered_indices_for_category(app.settings_category_selected);
                move_settings_selection(app, &filtered, false);
            }
            app.ensure_settings_selection_visible();
        }
        KeyCode::Down => {
            app.settings_focus_control = !app.settings_focus_categories;
            if app.settings_focus_categories {
                app.move_settings_category(true);
            } else {
                let filtered =
                    app.settings_filtered_indices_for_category(app.settings_category_selected);
                move_settings_selection(app, &filtered, true);
            }
            app.ensure_settings_selection_visible();
        }
        KeyCode::Left | KeyCode::Right if app.settings_focus_categories => {
            app.expand_settings_category(key.code == KeyCode::Right);
        }
        KeyCode::Home | KeyCode::End if app.settings_focus_categories => {
            let rows = app.settings_category_visual_rows();
            let row = if key.code == KeyCode::Home { rows.first() } else { rows.last() };
            if let Some(&(category, child, label)) = row {
                app.select_settings_page(category, child.then_some(label));
            }
        }
        KeyCode::Enter => {
            if app.settings_focus_categories {
                let idx = app
                    .settings_category_selected
                    .min(App::settings_categories().len().saturating_sub(1));
                if app.settings_selected_section(idx).is_some() || App::settings_sections(idx).is_empty() {
                    app.settings_focus_categories = false;
                    app.settings_focus_control = true;
                } else {
                    app.settings_category_collapsed[idx] = !app.settings_category_collapsed[idx];
                }
                app.reveal_settings_category();
            } else {
                let items = app.settings_items();
                app.settings_focus_control = true;
                if let Some((_, _, id)) = items.get(app.settings_selected) {
                    if matches!(App::setting_field_kind(id), "number" | "language-list") {
                        let current = items
                            .get(app.settings_selected)
                            .map(|(_, value, _)| value.clone())
                            .unwrap_or_default();
                        app.begin_numeric_setting_edit(id, &current);
                    } else if App::setting_field_kind(id) == "dropdown" {
                        let current = items
                            .get(app.settings_selected)
                            .map(|(_, value, _)| value.clone())
                            .unwrap_or_default();
                        app.open_settings_dropdown(id, &current);
                    } else {
                        app.toggle_setting(id);
                    }
                }
            }
            app.ensure_settings_selection_visible();
        }
        KeyCode::Delete if key.modifiers.contains(KeyModifiers::ALT) => {
            // Alt+Delete removes the override from the selected scope. Plain
            // Delete belongs to the focused settings search text field.
            let items = app.settings_items();
            if let Some((_, _, id)) = items.get(app.settings_selected) {
                app.reset_setting_in_selected_scope(id);
            }
        }
        KeyCode::Char(' ') if app.settings_focus_control => {
            let items = app.settings_items();
            if let Some((_, _, id)) = items.get(app.settings_selected) {
                if App::setting_field_kind(id) == "checkbox" {
                    app.toggle_setting(id);
                }
            }
        }
        _ => {
            let result = app.edit_text_input(
                TextInputId::SettingsSearch,
                key.code,
                key.modifiers,
                None,
            );
            if result.consumed {
                app.settings_focus_categories = false;
                app.settings_focus_control = false;
            }
            if result.changed {
                app.settings_editor_scroll = 0;
                app.ensure_settings_selection_visible();
            }
        }
    }
    Some(false)
}

/// Handle settings panel keyboard events.
pub(crate) fn handle_settings_panel_key(app: &mut App, key: KeyEvent) -> Option<bool> {
    if !app.settings_active {
        return None;
    }
    // Keybinding capture mode
    if app.settings_kb_editing {
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
        return Some(false);
    }
    match key.code {
        KeyCode::Esc => app.settings_active = false,
        KeyCode::Up => {
            let filtered: Vec<_> = (0..app.settings_items().len()).collect();
            move_settings_selection(app, &filtered, false);
        }
        KeyCode::Down => {
            let filtered: Vec<_> = (0..app.settings_items().len()).collect();
            move_settings_selection(app, &filtered, true);
        }
        KeyCode::Enter => {
            let items = app.settings_items();
            if let Some((_, _, id)) = items.get(app.settings_selected) {
                app.toggle_setting(id);
            }
        }
        _ => {}
    }
    Some(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn test_app() -> App {
        App::new(std::env::current_dir().unwrap())
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::empty(),
            kind: KeyEventKind::Press,
            state: KeyEventState::empty(),
        }
    }

    // --- handle_exit_confirm_key tests ---

    #[test]
    fn exit_save_skips_settings_and_saves_real_files_in_other_panes() {
        let directory = tempfile::tempdir().unwrap();
        let settings_path = directory.path().join("settings-view-must-not-be-saved");
        let file_path = directory.path().join("edited.txt");
        let mut app = test_app();
        app.open_settings_tab();
        let settings = app.editor_mut().unwrap();
        settings.file_path = Some(settings_path.clone());
        settings.insert_char('x');

        let mut editor = crate::editor::Editor::new();
        editor.file_path = Some(file_path.clone());
        editor.insert_char('y');
        let mut group = crate::app::EditorGroup::new();
        group.tabs.push(crate::app::Tab::new_file(editor));
        app.groups.push(group);
        app.exit_confirm_active = true;

        assert_eq!(handle_exit_confirm_key(&mut app, key(KeyCode::Char('y'))), Some(true));
        assert!(!settings_path.exists());
        assert_eq!(std::fs::read_to_string(file_path).unwrap().trim_end(), "y");
    }

    #[test]
    fn exit_confirm_inactive_returns_none() {
        let mut app = test_app();
        app.exit_confirm_active = false;
        assert!(handle_exit_confirm_key(&mut app, key(KeyCode::Char('y'))).is_none());
    }

    #[test]
    fn exit_confirm_esc_closes() {
        let mut app = test_app();
        app.exit_confirm_active = true;
        let result = handle_exit_confirm_key(&mut app, key(KeyCode::Esc));
        assert_eq!(result, Some(false));
        assert!(!app.exit_confirm_active);
    }

    #[test]
    fn exit_confirm_n_closes() {
        let mut app = test_app();
        app.exit_confirm_active = true;
        let result = handle_exit_confirm_key(&mut app, key(KeyCode::Char('n')));
        assert_eq!(result, Some(false));
        assert!(!app.exit_confirm_active);
    }

    #[test]
    fn exit_confirm_d_returns_quit() {
        let mut app = test_app();
        app.exit_confirm_active = true;
        let result = handle_exit_confirm_key(&mut app, key(KeyCode::Char('d')));
        assert_eq!(result, Some(true));
    }

    #[test]
    fn exit_confirm_k_sets_keep_flag() {
        let mut app = test_app();
        app.exit_confirm_active = true;
        let result = handle_exit_confirm_key(&mut app, key(KeyCode::Char('k')));
        assert_eq!(result, Some(false));
        assert!(app.exit_keep_unsaved_quit);
        assert!(!app.exit_confirm_active);
    }

    // --- handle_change_dir_key tests ---

    #[test]
    fn change_dir_inactive_returns_none() {
        let mut app = test_app();
        app.change_dir_active = false;
        assert!(handle_change_dir_key(&mut app, key(KeyCode::Up)).is_none());
    }

    #[test]
    fn change_dir_esc_closes() {
        let mut app = test_app();
        app.change_dir_active = true;
        handle_change_dir_key(&mut app, key(KeyCode::Esc));
        assert!(!app.change_dir_active);
    }

    #[test]
    fn change_dir_up_saturates_at_zero() {
        let mut app = test_app();
        app.change_dir_active = true;
        app.folder_browser_selected = 0;
        handle_change_dir_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.folder_browser_selected, 0);
    }

    // --- handle_settings_panel_key tests ---

    #[test]
    fn settings_inactive_returns_none() {
        let mut app = test_app();
        app.settings_active = false;
        assert!(handle_settings_panel_key(&mut app, key(KeyCode::Up)).is_none());
    }

    #[test]
    fn settings_esc_closes() {
        let mut app = test_app();
        app.settings_active = true;
        handle_settings_panel_key(&mut app, key(KeyCode::Esc));
        assert!(!app.settings_active);
    }

    #[test]
    fn settings_up_decrements() {
        let mut app = test_app();
        app.settings_active = true;
        app.settings_selected = 5;
        handle_settings_panel_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.settings_selected, 4);
    }

    #[test]
    fn settings_up_from_heading_finds_first_control() {
        let mut app = test_app();
        app.settings_active = true;
        app.settings_selected = 0;
        let first_control = app
            .settings_items()
            .iter()
            .position(|(_, _, id)| App::setting_field_kind(id) != "heading")
            .unwrap();
        handle_settings_panel_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.settings_selected, first_control);
        handle_settings_panel_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.settings_selected, first_control);
    }

    #[test]
    fn animations_keyboard_navigation_skips_sections_and_preserves_visual_rows() {
        let mut app = App::test_new();
        app.settings_editor_active = true;
        app.active_pane = ActivePane::Editor;
        app.settings_category_selected = 7;
        app.settings_focus_categories = false;
        app.settings_focus_control = true;
        let items = app.settings_items();
        let filtered = app.settings_filtered_indices_for_category(7);
        let controls: Vec<_> = filtered
            .iter()
            .copied()
            .filter(|&index| App::setting_field_kind(&items[index].2) != "heading")
            .collect();
        assert!(
            filtered.len() > controls.len() + 1,
            "animation groups must remain visible"
        );
        app.settings_selected = controls[0];
        for &expected in controls.iter().skip(1) {
            handle_settings_editor_key(&mut app, key(KeyCode::Down));
            assert_eq!(app.settings_selected, expected);
        }
        handle_settings_editor_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.settings_selected, *controls.last().unwrap());
        for &expected in controls.iter().rev().skip(1) {
            handle_settings_editor_key(&mut app, key(KeyCode::Up));
            assert_eq!(app.settings_selected, expected);
        }
        handle_settings_editor_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.settings_selected, controls[0]);
        assert_eq!(app.settings_filtered_indices_for_category(7), filtered);

        app.settings_editor_search = "animations".into();
        let results = app.settings_filtered_indices_for_category(7);
        assert!(!results.is_empty());
        assert!(results
            .iter()
            .all(|&index| App::setting_field_kind(&items[index].2) != "heading"));
        app.settings_selected = results[0];
        handle_settings_editor_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.settings_selected, results[1]);
    }

    #[test]
    fn legacy_settings_navigation_skips_section_headings_in_both_directions() {
        let mut app = App::test_new();
        app.settings_active = true;
        let items = app.settings_items();
        let heading = (1..items.len() - 1)
            .find(|&index| {
                App::setting_field_kind(&items[index].2) == "heading"
                    && App::setting_field_kind(&items[index - 1].2) != "heading"
                    && App::setting_field_kind(&items[index + 1].2) != "heading"
            })
            .unwrap();
        app.settings_selected = heading - 1;
        handle_settings_panel_key(&mut app, key(KeyCode::Down));
        assert_eq!(app.settings_selected, heading + 1);
        handle_settings_panel_key(&mut app, key(KeyCode::Up));
        assert_eq!(app.settings_selected, heading - 1);
    }
}
