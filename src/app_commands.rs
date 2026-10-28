use crate::app::{
    ActivePane, App, CallHierarchyMode, PaletteCommand, TabKind,
    TypeHierarchyMode,
};
use crate::app_theme::config_key_from_command_id;
use crate::editor::Editor;
#[cfg(test)]
use crate::app::Tab;

fn about_information(headless: Option<(&str, u32)>) -> String {
    use std::fmt::Write;

    let mut info = format!(
        "VTCode {}\nNative terminal code editor\n\nPlatform: {} ({})\nBuild: {}\nLicense: {}\nSSH protocol: {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        if cfg!(debug_assertions) { "Debug" } else { "Release" },
        env!("CARGO_PKG_LICENSE"),
        crate::remote::VERSION,
    );
    if let Some((version, protocol)) = headless {
        let version = if version.is_empty() { "Unknown" } else { version };
        let _ = write!(info, "\nHeadless: VTCode {version} (protocol {protocol})");
    }
    info.push_str("\n\nhttps://github.com/im-henri/vtcode");
    info
}

impl App {
    pub(crate) fn show_about(&mut self) {
        let headless = self.remote_workspace.as_ref().map(|workspace| {
            let hello = workspace.client.hello();
            (hello.app_version.as_str(), hello.version)
        });
        // Use compile-time metadata and the existing handshake, without
        // filesystem probes, subprocesses, or an SSH round trip.
        self.about_info = Some(about_information(headless));
    }

    pub(crate) fn copy_diff_selection(&mut self) -> bool {
        self.copy_diff_selection_with(|text| crate::clipboard::set_text(text).is_ok())
    }

    pub(crate) fn copy_diff_selection_with(&mut self, copy: impl FnOnce(&str) -> bool) -> bool {
        if self.active_pane != ActivePane::Editor || self.focused_terminal_index().is_some() {
            return false;
        }
        let Some(text) = self.diff_selected_text() else {
            return false;
        };
        if text.is_empty() {
            return true;
        }
        let is_line = !self.has_diff_selection();
        let copied = copy(&text);
        crate::clipboard::remember_line_copy(&text, is_line);
        if let Some(editor) = self.editor_mut() {
            editor.clipboard.clone_from(&text);
            editor.clipboard_is_line = is_line;
            editor.clipboard_failed = !copied;
        }
        self.push_clipboard_history(&text);
        if !copied {
            self.notify("System clipboard unavailable — copied to internal clipboard only");
        }
        self.start_copy_animation(std::time::Instant::now());
        true
    }

    pub(crate) fn copy_editor_selection(&mut self) {
        if let Some(editor) = self.editor_mut() {
            editor.copy();
            if editor.clipboard_failed {
                self.notify_copy_failure();
            }
        }
        self.start_copy_animation(std::time::Instant::now());
    }

    pub(crate) fn execute_palette_command(&mut self, id: &str) -> bool {
        self.copy_animation.clear();
        self.record_navigation();
        if matches!(id, "goto_last_edit" | "next_bookmark" | "prev_bookmark" | "next_diagnostic" | "prev_diagnostic" | "goto_bracket" | "next_diff_hunk" | "prev_diff_hunk") {
            self.push_nav_history();
        }
        let handled = self.execute_palette_command_inner(id);
        self.finish_navigation();
        // Settings reads the saved layers, so preferences changed through a
        // command must update them too. Keep text/navigation commands off this
        // serialization and disk-write path.
        if config_key_from_command_id(id).is_some()
            || matches!(id,
                "toggle_indent_rainbow" | "toggle_color_preview" | "toggle_bracket_pair_colorization"
                | "toggle_error_lens" | "toggle_inlay_hints" | "toggle_code_lens"
                | "toggle_on_type_formatting" | "toggle_semantic_tokens" | "toggle_sticky_scroll"
                | "toggle_screencast_mode" | "toggle_minimap" | "toggle_line_numbers"
                | "toggle_indent_guides" | "toggle_breadcrumbs" | "toggle_activity_bar"
                | "toggle_status_bar" | "toggle_tab_bar" | "toggle_menu_bar" | "toggle_relative_lines"
                | "toggle_word_wrap" | "toggle_whitespace" | "toggle_cursor_style"
                | "toggle_cursor_blinking" | "toggle_focus_mode" | "toggle_sidebar_position"
                | "toggle_zen" | "toggle_maximized" | "toggle_explorer" | "toggle_sidebar"
                | "toggle_panel" | "ruler_80" | "ruler_120" | "ruler_clear")
        {
            self.flush_live_settings();
        }
        handled
    }

    fn execute_palette_command_inner(&mut self, id: &str) -> bool {
        if crate::ui_logs::command(self, id) { return true; }
        if self.route_explorer_edit_command(id) { return true; }
        match id {
            "about" => { self.show_about(); return true; }
            "remote_connection_status" => {
                if self.remote_workspace.is_some() {
                    if self.remote_connection.phase == crate::app_remote_connection::Phase::Connected {
                        self.remote_connection.message = "Connected. Editing and undo run locally.".into();
                    }
                    self.publish_remote_connection_notice(true);
                }
                return true;
            }
            "show_output" => { self.show_output(); return true; }
            "remote_connection_diagnostics" => { self.show_remote_diagnostics(); return true; }
            "remote_connection_sign_in" => {
                if self.remote_connection.phase == crate::app_remote_connection::Phase::Connecting {
                    self.remote_connection.details = true;
                }
                return true;
            }
            "remote_connection_diagnostics_copy" => { self.copy_remote_diagnostics(); return true; }
            "remote_capture_cpu" => { self.capture_remote_cpu(); return true; }
            "remote_reconnect" => { self.retry_remote_connection(); return true; }
            "remote_reconnect_stop" => { self.stop_remote_reconnect(); return true; }
            _ => {}
        }
        if crate::ui_desks::command(self, id) { return true; }
        if self.diff_selection_owns_cursor()
            && matches!(id, "undo" | "redo" | "toggle_comment" | "toggle_block_comment"
                | "move_line_up" | "move_line_down" | "transpose_chars" | "transpose_words"
                | "eol_lf" | "eol_crlf")
        {
            return true;
        }
        if self.dispatch_extension_editor_command(id) { return true; }
        if matches!(id, "format_document" | "format_selection")
            && self.tabs.get(self.active_tab).is_some_and(|tab| tab.kind == TabKind::GitRevision) {
            self.show_formatting_error("Historical versions are read-only. Open the working file to format it.");
            return true;
        }
        if self.execute_acp_command(id) { return true; }
        let search_edit = match id {
            "undo" | "redo" => self.file_search_history_snapshot(id == "redo"),
            _ => self.file_search_edit_snapshot(),
        };
        // Handle theme commands: theme_dark, theme_one_dark_pro, etc.
        if let Some(config_key) = config_key_from_command_id(id) {
            if self.set_theme_by_key(&config_key) {
                self.notify(format!("Theme: {}", self.theme_name));
                return true;
            }
        }
        // Dispatch to sub-modules for editor and view commands
        if self.execute_editor_command(id) {
            return true;
        }
        if self.execute_view_command(id) {
            return true;
        }
        if self.execute_git_command(id) {
            return true;
        }
        match id {
            "reload_syntaxes" => self.reload_syntaxes(),
            "install_color_theme" => self.open_theme_install_prompt(),
            "install_syntax" => self.open_syntax_install_prompt(),
            "extensions_install" => self.open_extension_install_prompt(),
            "reload_color_themes" => {
                self.request_external_themes_async();
                self.notify("Reloading color themes...");
            }
            "cancel_file_deletion" => self.cancel_file_deletion(),
            "save" => self.save_current_file(),
            "save_all" => {
                if self.paste_in_progress() {
                    self.notify_warn("Paste is still in progress; Save All after it finishes or cancel it first");
                    return true;
                }
                let mut saved = self.queue_custom_document_actions(0);
                let mut untitled = 0;
                let trim = self.trim_trailing_on_save;
                let newline = self.insert_final_newline;
                let mut bg_jobs: Vec<crate::editor::BackgroundSaveRequest> = Vec::new();
                for tab in &self.tabs {
                    if tab.kind != TabKind::File || !tab.editor.modified {
                        continue;
                    }
                    if tab.editor.file_path.is_none() {
                        untitled += 1;
                        continue;
                    }
                    if let Some(job) = tab.editor.background_save_request(trim, newline, false) {
                        bg_jobs.push(job);
                        saved += 1;
                    }
                }
                for group in &self.groups {
                    for tab in &group.tabs {
                        if tab.kind != TabKind::File || !tab.editor.modified {
                            continue;
                        }
                        if tab.editor.file_path.is_none() {
                            untitled += 1;
                            continue;
                        }
                        if let Some(job) = tab.editor.background_save_request(trim, newline, false)
                        {
                            bg_jobs.push(job);
                            saved += 1;
                        }
                    }
                }
                for job in bg_jobs {
                    self.queue_background_save_job(job);
                }
                if untitled > 0 {
                    self.notify_error(format!(
                        "Saving {saved}; {untitled} untitled file(s) need Save As"
                    ));
                } else {
                    self.notify(format!("Saving {saved} file(s)..."));
                }
            }
            "tab_close_all" => self.close_all_tabs(),
            "save_as" => {
                if self.editor().is_some_and(|editor| self.pending_paste_document() == Some(editor.save_identity)) {
                    self.notify_warn("Paste is still being prepared; Save As after it finishes or cancel it first");
                    return true;
                }
                if self.tabs.get(self.active_tab).is_some_and(|tab| tab.kind == TabKind::GitRevision) {
                    self.notify("Historical versions are read-only. Open the working file to edit.");
                    return true;
                }
                self.modal = crate::ui_modal::ModalState::default();
                self.save_as_active = true;
                self.save_as_query = self
                    .editor()
                    .and_then(|e| e.file_path.as_ref())
                    .map_or_else(
                        || {
                            self.explorer
                                .root
                                .join(self.editor().and_then(|e| e.suggested_file_name.as_deref()).unwrap_or("untitled.txt"))
                                .to_string_lossy()
                                .into_owned()
                        },
                        |p| p.to_string_lossy().into_owned(),
                    );
                if let Some(remote) = &self.remote_workspace {
                    self.save_as_query = self.editor().and_then(|editor| editor.file_path.as_deref())
                        .and_then(|key| remote.display_path(key).ok())
                        .unwrap_or_else(|| remote.initial_save_path(self.editor().and_then(|editor| editor.suggested_file_name.as_deref())));
                }
                self.save_as_cursor = self.save_as_query.len();
                self.save_as_selection_anchor = None;
            }
            "new_file" => self.new_file(),
            "close_tab" | "close_tab_menu" => self.close_tab(self.active_tab),
            "close_saved" => self.close_saved_tabs(),
            "close_others" => {
                let active = self.active_tab;
                self.close_saved_tabs_matching(|i| i != active);
                self.notify("Closed other tabs");
            }
            "close_right" => {
                let active = self.active_tab;
                self.close_saved_tabs_matching(|i| i > active);
                self.notify("Closed tabs to the right");
            }
            "close_left" => {
                let active = self.active_tab;
                self.close_saved_tabs_matching(|i| i < active);
                self.notify("Closed tabs to the left");
            }
            "duplicate_tab" => self.duplicate_tab(self.active_tab),
            "switch_header_source" => {
                if let Some(path) = self.editor().and_then(|e| e.file_path.clone()) {
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    let counterpart_ext = match ext {
                        "h" => Some("c"),
                        "c" => Some("h"),
                        "hpp" | "hxx" => Some("cpp"),
                        "cpp" | "cxx" | "cc" => Some("hpp"),
                        _ => None,
                    };
                    if let Some(new_ext) = counterpart_ext {
                        let new_path = path.with_extension(new_ext);
                        if self.remote_workspace.is_some() && crate::remote::workspace::is_view_key(&new_path) {
                            self.open_remote_location(new_path, None, crate::app_remote_navigation::Source::FileCommand);
                        } else if new_path.exists() {
                            self.open_file(&new_path);
                        } else {
                            self.notify(format!("{} not found", new_path.display()));
                        }
                    } else {
                        self.notify("Not a C/C++ header/source file");
                    }
                }
            }
            "undo" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.undo();
                }
            }
            "redo" => {
                if let Some(ed) = self.editor_mut_writable() {
                    ed.redo();
                }
            }
            "cursor_undo" => {
                if let Some(ed) = self.editor_mut() {
                    ed.cursor_undo();
                }
            }
            "copy" => {
                if let Some(index) = self.focused_terminal_index() {
                    self.copy_terminal_selection(index);
                    return true;
                }
                if self.copy_diff_selection() { return true; }
                self.copy_editor_selection();
            }
            "cut" => {
                if self.focused_terminal_index().is_some() { return true; }
                if (self.editor().is_some_and(|editor| editor.read_only)
                    || self.diff_selection_owns_cursor()) && self.copy_diff_selection() {
                    return true;
                }
                if let Some(ed) = self.editor_mut() {
                    if ed.read_only {
                        ed.copy();
                    } else {
                        ed.cut();
                    }
                    if ed.clipboard_failed {
                        self.notify(
                            "System clipboard unavailable — copied to internal clipboard only",
                        );
                    }
                }
            }
            "paste" => {
                if let Some(index) = self.focused_terminal_index() {
                    self.paste_terminal_clipboard(index);
                    return true;
                }
                if self.diff_selection_owns_cursor() { return true; }
                self.paste_editor_clipboard(true);
            }
            "paste_plain" => {
                if let Some(index) = self.focused_terminal_index() {
                    self.paste_terminal_clipboard(index);
                    return true;
                }
                if self.diff_selection_owns_cursor() { return true; }
                self.paste_editor_clipboard(false);
            }
            "select_all" => {
                if self.active_pane == ActivePane::Explorer && self.sidebar_view == crate::app::SidebarView::SourceControl
                    && !self.source_control_input_mode && !self.sc_graph_focused {
                    self.select_all_source_control_files();
                    return true;
                }
                if self.diff_selection_key(crossterm::event::KeyCode::Char('a'), crossterm::event::KeyModifiers::CONTROL) {
                    return true;
                }
                if let Some(ed) = self.editor_mut() {
                    ed.select_all();
                }
            }
            "select_line" => {
                if let Some(ed) = self.editor_mut() {
                    ed.select_line();
                }
            }
            "select_next_occurrence" | "add_selection_next" => {
                if let Some(ed) = self.editor_mut() {
                    ed.select_next_occurrence();
                }
            }
            "select_all_occurrences" => {
                if let Some(ed) = self.editor_mut() {
                    ed.select_all_occurrences();
                }
            }
            "add_cursors_line_ends" => {
                if let Some(ed) = self.editor_mut() {
                    ed.add_cursors_to_line_ends();
                }
            }
            "add_cursor_above" => {
                if let Some(ed) = self.editor_mut() {
                    ed.add_cursor_above();
                    let count = ed.extra_cursors.len() + 1;
                    self.notify(format!("{count} cursors"));
                }
            }
            "add_cursor_below" => {
                if let Some(ed) = self.editor_mut() {
                    ed.add_cursor_below();
                    let count = ed.extra_cursors.len() + 1;
                    self.notify(format!("{count} cursors"));
                }
            }
            "macro_start" => {
                self.macro_recording = true;
                self.macro_buffer.clear();
                self.notify("🔴 Macro recording started");
            }
            "macro_stop" => {
                if self.macro_recording {
                    self.macro_recording = false;
                    self.macro_saved = self.macro_buffer.clone();
                    let count = self.macro_saved.len();
                    self.notify(format!("Macro recorded ({count} keys)"));
                }
            }
            "macro_replay" => {
                if self.macro_saved.is_empty() {
                    self.notify("No macro recorded");
                } else {
                    self.macro_replay_queue = self.macro_saved.iter().copied().collect();
                    self.notify(format!(
                        "Replaying macro ({} keys)",
                        self.macro_replay_queue.len()
                    ));
                }
            }
            "change_dir" => {
                self.open_folder_browser();
            }
            "open_parent" => {
                if let Some(parent) = self.explorer.root.parent() {
                    let parent = parent.to_path_buf();
                    self.change_workspace(&parent);
                }
            }
            "source_control" => {
                // Toggle source control sidebar
                if self.sidebar_view == crate::app::SidebarView::SourceControl && self.show_explorer
                {
                    self.show_explorer = false;
                } else {
                    self.sidebar_view = crate::app::SidebarView::SourceControl;
                    self.show_explorer = true;
                    self.activity_bar_selected = 2;
                    self.refresh_source_control_async();
                    self.refresh_git_log_async();
                    self.source_control_input_mode = false;
                }
            }
            "extensions" => {
                if self.sidebar_view == crate::app::SidebarView::Extensions && self.show_explorer {
                    self.show_explorer = false;
                } else {
                    self.open_extensions_sidebar();
                }
            }
            "extensions_rescan" => {
                self.extensions_rescan();
            }
            "extensions_reload_all" => {
                self.extensions_reload_all();
            }
            "extensions_reload_selected" => {
                self.extensions_reload_selected();
            }
            "extensions_unload_selected" => {
                self.extensions_unload_selected();
            }
            "extensions_load_selected" => {
                self.extensions_load_selected();
            }

            "new_terminal" => {
                self.new_terminal_tab();
                self.terminal_sidebar_revealed = true;
                self.show_terminal_sidebar = true;
                self.terminal_panel_focused = true;
                self.active_pane = ActivePane::Editor;
            }
            "new_terminal_in_editor_tab" => {
                self.new_terminal_in_editor_tab();
            }
            "close_terminal" => {
                self.close_terminal_tab();
            }
            "toggle_terminal" => {
                self.toggle_terminal_sidebar();
            }
            "move_terminal_to_right" => {
                if self.terminal_is_open(self.active_terminal) {
                    self.move_terminal_to_dock(self.active_terminal, crate::app::TerminalDock::Right);
                }
            }
            "detach_terminal_to_tab" => {
                if self.terminal_is_open(self.active_terminal) {
                    self.detach_terminal_to_tab();
                }
            }
            "run_task" => {
                self.open_task_picker();
            }
            "rerun_last_task" => {
                if let Some(cmd) = self.last_task.clone() {
                    self.run_task(&cmd, "Last Task");
                } else {
                    self.open_task_picker();
                }
            }

            "keybindings" => {
                if self.tabs.get(self.active_tab).map(|t| t.kind)
                    == Some(TabKind::KeyboardShortcuts)
                {
                    self.close_tab(self.active_tab);
                } else {
                    self.open_keybindings_tab();
                }
                self.keybindings_selected = 0;
                self.keybindings_editing = false;
            }
            "settings" => {
                self.open_settings_tab();
                self.settings_selected = 0;
                self.settings_category_selected = 0;
                self.settings_section_selected = None;
                self.settings_category_scroll = 0;
                self.settings_category_collapsed.fill(true);
                self.settings_category_area = None;
                self.settings_focus_categories = false;
                self.settings_editor_scroll = 0;
                self.settings_editor_search.clear();
                self.settings_editor_search_cursor = 0;
                self.settings_editor_search_selection_anchor = None;
                self.ensure_settings_selection_visible();
            }
            "language_servers" => self.open_lsp_settings_page(),
            "completion_index" => crate::ui_vtcomplete_index::open(self),
            "explorer_find" => self.open_explorer_find(),
            "restart_language_servers" => {
                self.refresh_language_servers(true);
                self.notify("Language servers restarted for open files");
            }
            "open_settings_json" | "open_global_settings_json" | "open_workspace_settings_json" => {
                let path = match id {
                    "open_global_settings_json" => self.settings_global_path.clone(),
                    "open_workspace_settings_json" => self.settings_workspace_path.clone(),
                    _ => match self.settings_scope {
                        crate::settings::SettingsScope::Global => self.settings_global_path.clone(),
                        crate::settings::SettingsScope::Workspace => {
                            self.settings_workspace_path.clone()
                        }
                    },
                };
                if !path.exists() {
                    if let Err(error) =
                        crate::settings::save_to_file(&path, &crate::settings::Settings::default())
                    {
                        self.notify(format!(
                            "Could not create settings file {}: {error}",
                            path.display()
                        ));
                        return false;
                    }
                }
                self.open_file(&path);
            }
            "find" => {
                if self.editor().is_none() { return false; }
                self.push_search_history();
                if !self.search_active {
                    self.search_pre_cursor = self
                        .editor()
                        .map(|e| (e.cursor_line, e.cursor_col, e.scroll_offset));
                }
                // Pre-fill search query with single-line selection
                if let Some(ed) = self.editor() {
                    if let Some(sel) = ed.selected_search_text() {
                        self.search_query = sel;
                    }
                }
                self.search_cursor = self.search_query.len();
                self.search_selection_anchor = (!self.search_query.is_empty()).then_some(0);
                self.search_active = true;
                self.search_input_focused = true;
                self.replace_mode = false;
                self.search_focus_replace = false;
                if !self.search_query.is_empty() {
                    self.update_search();
                }
            }
            "find_replace" => {
                if self.editor().is_none() { return false; }
                self.push_search_history();
                if !self.search_active {
                    self.search_pre_cursor = self
                        .editor()
                        .map(|e| (e.cursor_line, e.cursor_col, e.scroll_offset));
                }
                // Pre-fill search query with single-line selection
                if let Some(ed) = self.editor() {
                    if let Some(sel) = ed.selected_search_text() {
                        self.search_query = sel;
                    }
                }
                self.search_cursor = self.search_query.len();
                self.search_selection_anchor = (!self.search_query.is_empty()).then_some(0);
                self.replace_cursor = self.replace_query.len();
                self.replace_selection_anchor = None;
                self.search_active = true;
                self.search_input_focused = true;
                self.replace_mode = true;
                self.search_focus_replace = false;
                if !self.search_query.is_empty() {
                    self.update_search();
                }
            }
            "find_next" => {
                self.push_search_history();
                self.search_next();
            }
            "find_prev" => {
                self.push_search_history();
                self.search_prev();
            }
            "global_search" => {
                self.push_global_search_history();
                self.global_search_query.clear();
                self.update_global_search();
                self.global_search_replace_query.clear();
                self.global_search_results.clear();
                self.global_search_truncated = false;
                self.global_search_stopped = false;
                self.global_search_selected = 0;
                self.global_search_scroll = 0;
                self.global_search_field_cursors = [
                    self.global_search_query.len(),
                    self.global_search_replace_query.len(),
                    self.search_include_pattern.len(),
                    self.search_exclude_pattern.len(),
                ];
                self.global_search_field_selection_anchors = [None; 4];
                self.focus_global_search();
            }
            "replace_all_in_files" => {
                self.replace_all_in_files();
            }
            "goto_line" => {
                self.open_goto_line();
            }
            "goto_symbol" => {
                self.symbol_list_workspace = false;
                self.symbol_list_active = true;
                self.symbol_list_query.clear();
                self.symbol_list_cursor = 0;
                self.symbol_list_selection_anchor = None;
                self.build_symbol_list();
            }
            "goto_workspace_symbol" => {
                self.symbol_list_workspace = true;
                self.symbol_list_active = true;
                self.symbol_list_query.clear();
                self.symbol_list_cursor = 0;
                self.symbol_list_selection_anchor = None;
                self.build_symbol_list();
            }
            "quick_open" => {
                self.quick_open_active = true;
                self.quick_open_query.clear();
                self.quick_open_cursor = 0;
                self.quick_open_selection_anchor = None;
                self.update_quick_open();
            }
            "palette_menu" | "command_palette" => {
                self.open_palette();
            }
            "quit_menu" | "quit" => {
                self.exit_confirm_active = true;
            }
            "show_welcome" => {
                // Close all tabs to show the welcome screen
                while !self.tabs.is_empty() {
                    let idx = self.tabs.len() - 1;
                    if self.tabs[idx].editor.modified {
                        break; // Don't close unsaved tabs
                    }
                    self.tabs.remove(idx);
                }
                self.tab_mru_order.retain(|&i| i < self.tabs.len());
                if self.tabs.is_empty() {
                    self.active_tab = 0;
                }
            }
            "new_folder" => {
                if self.explorer.find.open { self.close_explorer_find(); }
                if self.remote_workspace.is_some() {
                    self.begin_remote_creation(self.explorer.root.clone(), true);
                    return false;
                }
                let dir = std::env::current_dir().unwrap_or_default();
                let placeholder_path = Self::next_available_child_name(&dir, "new folder");
                match std::fs::create_dir(&placeholder_path) {
                    Ok(()) => {
                        self.refresh_explorer_after_edit(Some(&placeholder_path));
                        let initial = placeholder_path
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("new folder")
                            .to_string();
                        self.creating_new_file = false;
                        self.creating_new_folder = true;
                        self.begin_rename(placeholder_path, initial, true, true);
                    }
                    Err(e) => self.notify(format!("Create folder failed: {e}")),
                }
            }
            "open_recent_menu" => {
                if self.recent_files.is_empty() {
                    self.notify("No recent files");
                } else {
                    // Open quick-open which shows recent files
                    self.quick_open_active = true;
                    self.quick_open_query.clear();
                    self.quick_open_cursor = 0;
                    self.quick_open_selection_anchor = None;
                    self.update_quick_open();
                }
            }
            "navigate_back" => {
                self.navigate_back();
            }
            "navigate_forward" => {
                self.navigate_forward();
            }
            "go_to_definition" => {
                if let Some(ed) = self.editor() {
                    if let Some(fp) = ed.file_path.as_ref() {
                        let fp = fp.clone();
                        let line = ed.cursor_line as u32;
                        let col = crate::lsp_positions::to_utf16(&ed.rope, ed.cursor_line, ed.cursor_col);
                        if !self.lsp_manager.request_definition(&fp, line, col) {
                            self.notify("No LSP definition provider available");
                        } else {
                            self.lsp_goto_pending = true;
                        }
                    }
                }
            }
            "go_to_type_definition" => {
                if let Some(ed) = self.editor() {
                    if let Some(fp) = ed.file_path.as_ref() {
                        let fp = fp.clone();
                        let line = ed.cursor_line as u32;
                        let col = crate::lsp_positions::to_utf16(&ed.rope, ed.cursor_line, ed.cursor_col);
                        if !self.lsp_manager.request_type_definition(&fp, line, col) {
                            self.notify("No LSP type definition provider available");
                        } else {
                            self.lsp_goto_pending = true;
                        }
                    }
                }
            }
            "go_to_implementation" => {
                if let Some(ed) = self.editor() {
                    if let Some(fp) = ed.file_path.as_ref() {
                        let fp = fp.clone();
                        let line = ed.cursor_line as u32;
                        let col = crate::lsp_positions::to_utf16(&ed.rope, ed.cursor_line, ed.cursor_col);
                        if !self.lsp_manager.request_implementation(&fp, line, col) {
                            self.notify("No LSP implementation provider available");
                        } else {
                            self.lsp_goto_pending = true;
                        }
                    }
                }
            }
            "go_to_declaration" => {
                if let Some(ed) = self.editor() {
                    if let Some(fp) = ed.file_path.as_ref() {
                        let fp = fp.clone();
                        let line = ed.cursor_line as u32;
                        let col = crate::lsp_positions::to_utf16(&ed.rope, ed.cursor_line, ed.cursor_col);
                        if !self.lsp_manager.request_declaration(&fp, line, col) {
                            self.notify("No LSP declaration provider available");
                        } else {
                            self.lsp_goto_pending = true;
                        }
                    }
                }
            }
            "peek_definition" => {
                self.open_peek_definition();
            }
            "find_references" => {
                if let Some(ed) = self.editor() {
                    if let Some(fp) = ed.file_path.as_ref() {
                        let fp = fp.clone();
                        let line = ed.cursor_line as u32;
                        let col = crate::lsp_positions::to_utf16(&ed.rope, ed.cursor_line, ed.cursor_col);
                        if self.lsp_manager.request_references(&fp, line, col) {
                            self.lsp_references_pending = true;
                            self.lsp_references_active = false;
                            self.lsp_references_selected = 0;
                        } else {
                            self.notify("No LSP references provider available");
                        }
                    }
                }
            }
            "format_document" => { self.request_format(false, false); }
            "format_selection" => { self.request_format(true, false); }
            "configure_formatter" => { self.open_formatter_settings(None); }
            "goto_last_edit" => {
                if let Some((tab_idx, line, col)) = self.last_edit_position {
                    if tab_idx < self.tabs.len() {
                        self.active_tab = tab_idx;
                        if let Some(ed) = self.editor_mut() {
                            ed.clear_selection();
                            ed.set_cursor(line, col);
                        }
                        self.notify("Jumped to last edit location");
                    }
                } else {
                    self.notify("No edit history");
                }
            }
            "compare_tabs" => {
                if self.tabs.len() >= 2 {
                    // Compare active tab with the next tab using split view
                    let next = (self.active_tab + 1) % self.tabs.len();
                    if let (Some(name1), Some(name2)) = (
                        self.tabs
                            .get(self.active_tab)
                            .map(|tab| tab.editor.file_name().to_string()),
                        self.tabs
                            .get(next)
                            .map(|tab| tab.editor.file_name().to_string()),
                    ) {
                        self.create_split_group(next);
                        self.notify(format!("Comparing: {name1} ↔ {name2}"));
                    } else {
                        self.notify("Need at least 2 tabs to compare");
                    }
                } else {
                    self.notify("Need at least 2 tabs to compare");
                }
            }
            "reopen_closed" => {
                self.reopen_closed_tab();
            }
            "show_shortcuts" => {
                self.shortcuts_active = true;
            }
            "toggle_bookmark" => {
                if let Some(ed) = self.editor_mut() {
                    ed.toggle_bookmark();
                }
                let count = self.editor().map_or(0, |e| e.bookmarks.len());
                self.notify(format!("Bookmarks: {count}"));
            }
            "next_bookmark" => {
                if let Some(ed) = self.editor_mut() {
                    ed.next_bookmark();
                }
            }
            "prev_bookmark" => {
                if let Some(ed) = self.editor_mut() {
                    ed.prev_bookmark();
                }
            }
            "next_diagnostic" => {
                self.navigate_diagnostic(true);
            }
            "prev_diagnostic" => {
                self.navigate_diagnostic(false);
            }
            "clear_bookmarks" => {
                if let Some(ed) = self.editor_mut() {
                    ed.clear_bookmarks();
                }
                self.notify("All bookmarks cleared");
            }
            "rename_symbol" => {
                // Try prepareRename first if LSP is available
                let pos_info = self.editor().and_then(|ed| {
                    ed.file_path
                        .as_ref()
                        .map(|fp| (fp.clone(), ed.cursor_line as u32, crate::lsp_positions::to_utf16(&ed.rope, ed.cursor_line, ed.cursor_col)))
                });
                if let Some((fp, line, col)) = pos_info {
                    if self.lsp_manager.request_prepare_rename(&fp, line, col) {
                        self.lsp_prepare_rename_pending = true;
                    } else if let Some(word) = self.editor().and_then(Editor::word_at_cursor) {
                        // No LSP - fall back to direct dialog
                        self.rename.symbol_active = true;
                        self.rename.symbol_old = word.clone();
                        self.rename.symbol_new = word;
                        self.rename.symbol_cursor = self.rename.symbol_new.len();
                        self.rename.symbol_selection_anchor = Some(0);
                    }
                } else if let Some(word) = self.editor().and_then(Editor::word_at_cursor) {
                    self.rename.symbol_active = true;
                    self.rename.symbol_old = word.clone();
                    self.rename.symbol_new = word;
                    self.rename.symbol_cursor = self.rename.symbol_new.len();
                    self.rename.symbol_selection_anchor = Some(0);
                }
            }
            "code_action" => {
                if !self.lsp_manager.clients.is_empty() {
                    if let Some(ed) = self.editor() {
                        {
                            if let Some(fp) = ed.file_path.as_ref() {
                                let fp = fp.clone();
                                let line = ed.cursor_line as u32;
                                let col = crate::lsp_positions::to_utf16(&ed.rope, ed.cursor_line, ed.cursor_col);
                                let diags = self.lsp_manager.code_action_diagnostics(&fp, line);
                                self.lsp_code_action_active = self.lsp_manager.request_code_action(
                                    &crate::lsp::CodeActionParams {
                                        path: &fp,
                                        start_line: line,
                                        start_col: col,
                                        end_line: line,
                                        end_col: col,
                                        diagnostics: diags,
                                    },
                                );
                                if self.lsp_code_action_active {
                                    self.lsp_code_action_selected = 0;
                                }
                            }
                        }
                    }
                }
            }
            "call_hierarchy_incoming" | "call_hierarchy_outgoing" => {
                let is_incoming = id == "call_hierarchy_incoming";
                if !self.lsp_manager.clients.is_empty() {
                    if let Some(ed) = self.editor() {
                        {
                            if let Some(fp) = ed.file_path.as_ref() {
                                let fp = fp.clone();
                                let line = ed.cursor_line as u32;
                                let col = crate::lsp_positions::to_utf16(&ed.rope, ed.cursor_line, ed.cursor_col);
                                if self
                                    .lsp_manager
                                    .request_prepare_call_hierarchy(&fp, line, col)
                                {
                                    self.call_hierarchy_mode = if is_incoming {
                                        CallHierarchyMode::Incoming
                                    } else {
                                        CallHierarchyMode::Outgoing
                                    };
                                    self.call_hierarchy_results.clear();
                                    self.call_hierarchy_selected = 0;
                                    self.call_hierarchy_root_name.clear();
                                }
                            }
                        }
                    }
                }
            }
            "type_hierarchy_supertypes" | "type_hierarchy_subtypes" => {
                let is_supertypes = id == "type_hierarchy_supertypes";
                if !self.lsp_manager.clients.is_empty() {
                    if let Some(ed) = self.editor() {
                        {
                            if let Some(fp) = ed.file_path.as_ref() {
                                let fp = fp.clone();
                                let line = ed.cursor_line as u32;
                                let col = crate::lsp_positions::to_utf16(&ed.rope, ed.cursor_line, ed.cursor_col);
                                if self
                                    .lsp_manager
                                    .request_prepare_type_hierarchy(&fp, line, col)
                                {
                                    self.type_hierarchy_mode = if is_supertypes {
                                        TypeHierarchyMode::Supertypes
                                    } else {
                                        TypeHierarchyMode::Subtypes
                                    };
                                    self.type_hierarchy_results.clear();
                                    self.type_hierarchy_selected = 0;
                                    self.type_hierarchy_root_name.clear();
                                }
                            }
                        }
                    }
                }
            }
            "toggle_pin_tab" => {
                if let Some(tab) = self.tabs.get_mut(self.active_tab) {
                    tab.pinned = !tab.pinned;
                    let state = if tab.pinned { "pinned" } else { "unpinned" };
                    self.notify(format!("Tab {state}"));
                }
            }
            "move_tab_left" => {
                if self.active_tab > 0 {
                    self.swap_tabs(self.active_tab, self.active_tab - 1);
                    self.active_tab -= 1;
                    self.notify("Tab moved left");
                }
            }
            "move_tab_right" => {
                if self.active_tab + 1 < self.tabs.len() {
                    self.swap_tabs(self.active_tab, self.active_tab + 1);
                    self.active_tab += 1;
                    self.notify("Tab moved right");
                }
            }
            "open_folder" => {
                if let Some(parent) = self.editor().and_then(|e| e.file_path.as_ref()).and_then(|p| p.parent()).map(|p| p.to_path_buf()) {
                    self.open_external(crate::external_open::Request::SystemApp(parent));
                }
            }
            "open_in_system_app" => {
                if let Some(path) = self.editor().and_then(|e| e.file_path.clone()) {
                    self.open_external(crate::external_open::Request::SystemApp(path));
                }
            }
            "copy_filename" => {
                if let Some(ed) = self.editor() {
                    let name = ed.file_name().to_string();
                    let _ = crate::clipboard::set_text(&name); // Error OK: clipboard may be unavailable
                    self.notify(format!("Copied: {name}"));
                }
            }
            "copy_filepath" => {
                if let Some(ed) = self.editor() {
                    if let Some(path) = &ed.file_path {
                        let path_str = path.display().to_string();
                        let _ = crate::clipboard::set_text(&path_str); // Error OK: clipboard may be unavailable
                        self.notify(format!("Copied: {path_str}"));
                    } else {
                        self.notify("No file path to copy");
                    }
                }
            }
            "copy_relative_path" => {
                if let Some(ed) = self.editor() {
                    if let Some(path) = &ed.file_path {
                        let root = &self.explorer.root;
                        let relative = path.strip_prefix(root).map_or_else(
                            |_| path.display().to_string(),
                            |p| p.display().to_string(),
                        );
                        let _ = crate::clipboard::set_text(&relative); // Error OK: clipboard may be unavailable
                        self.notify(format!("Copied: {relative}"));
                    } else {
                        self.notify("No file path to copy");
                    }
                }
            }
            "clipboard_history" => {
                if self.clipboard_history.is_empty() {
                    self.notify("Clipboard history is empty");
                } else {
                    // Show clipboard history in palette
                    self.palette.active = true;
                    self.palette.query.clear();
                    self.palette.cursor = 0;
                    self.palette.selection_anchor = None;
                    self.palette.selected = 0;
                    self.palette.commands = self
                        .clipboard_history
                        .iter()
                        .rev()
                        .enumerate()
                        .map(|(i, text)| {
                            let preview: String = text.chars().take(60).collect();
                            let preview = preview.replace('\n', "↵");
                            PaletteCommand {
                                name: format!("{}: {preview}", i + 1),
                                shortcut: String::new(),
                                id: std::borrow::Cow::Owned(format!("clipboard_{i}")),
                            }
                        })
                        .collect();
                    self.palette.filtered = (0..self.palette.commands.len()).collect();
                }
            }
            "copy_path" => {
                if let Some(path) = self.editor().and_then(|e| e.file_path.as_ref()) {
                    let path_str = path.display().to_string();
                    let _ = crate::clipboard::set_text(&path_str); // Error OK: clipboard may be unavailable
                    self.notify(format!("Copied: {path_str}"));
                }
            }
            "reveal_in_explorer" => {
                if let Some(path) = self.editor().and_then(|e| e.file_path.as_ref()) {
                    // Select the file in the file explorer
                    let target = path.clone();
                    self.show_explorer = true;
                    self.explorer.select_path(&target);
                    self.active_pane = ActivePane::Explorer;
                    self.notify("Revealed in explorer");
                }
            }
            id if id.starts_with("ext_cmd:") => {
                self.execute_extension_command_id(id);
            }
            _ => {}
        }
        if let Some(previous) = search_edit.as_ref() {
            if self.refresh_document_feedback_after_edit(previous) {
                // Palette commands include paste, undo, formatting, and whole-file
                // transforms whose mutation range is not necessarily the caret.
                if matches!(id, "undo" | "redo") {
                    self.refresh_search_after_edit(previous);
                } else {
                    self.defer_search_after_unbounded_edit();
                }
            }
        }
        false
    }

    fn navigate_diagnostic(&mut self, forward: bool) {
        let Some((path, cursor_line, cursor_col)) = self.editor().and_then(|editor| {
            editor
                .file_path
                .clone()
                .map(|path| (path, editor.cursor_line, editor.cursor_col))
        }) else {
            self.notify("No diagnostics in current file");
            return;
        };

        let Some(diagnostics) = self.lsp_manager.get_diagnostics(&path) else {
            self.notify("No diagnostics in current file");
            return;
        };
        if diagnostics.is_empty() {
            self.notify("No diagnostics in current file");
            return;
        }

        let mut entries: Vec<(usize, usize, lsp_types::DiagnosticSeverity, String)> = diagnostics
            .iter()
            .map(|diag| (diag.line, self.editor().map_or(0, |ed| crate::lsp_positions::from_utf16(&ed.rope, diag.line, diag.col as u32)), diag.severity, diag.message.clone()))
            .collect();
        entries.sort_by_key(|&(line, col, _, _)| (line, col));

        let current_pos = (cursor_line, cursor_col);
        let target = if forward {
            entries
                .iter()
                .find(|&&(line, col, _, _)| (line, col) > current_pos)
                .or_else(|| entries.first())
        } else {
            entries
                .iter()
                .rev()
                .find(|&&(line, col, _, _)| (line, col) < current_pos)
                .or_else(|| entries.last())
        };

        if let Some((line, col, severity, message)) = target {
            if let Some(editor) = self.editor_mut() {
                editor.clear_selection();
                editor.set_cursor(*line, *col);
                editor.center_cursor();
            }
            self.notify(format!(
                "{} {}",
                crate::lsp::severity_icon(*severity),
                message
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::LspDiagnostic;
    use lsp_types::DiagnosticSeverity;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn about_information_distinguishes_client_and_cached_headless_versions() {
        let local = about_information(None);
        assert!(local.contains(env!("CARGO_PKG_VERSION")));
        assert!(local.contains(std::env::consts::OS));
        assert!(local.contains(std::env::consts::ARCH));
        assert!(local.contains(env!("CARGO_PKG_LICENSE")));
        assert!(!local.contains("Headless:"));

        let remote = about_information(Some(("0.0.test", 42)));
        assert!(remote.contains("Headless: VTCode 0.0.test (protocol 42)"));
        assert!(remote.contains(&format!("SSH protocol: {}", crate::remote::VERSION)));
        assert!(about_information(Some(("", 42))).contains("Headless: VTCode Unknown"));
    }

    fn make_file_tab(name: &str) -> Tab {
        let mut editor = Editor::new();
        editor.file_path = Some(PathBuf::from(name));
        editor.rope = ropey::Rope::from_str("line0\nline1\nline2\nline3\nline4\n");
        Tab::new_file(editor)
    }

    fn temp_repo_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("vtcode-{label}-{unique}"))
    }

    fn run_git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("git command");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn palette_text_edit_keeps_find_highlights_until_debounced_refresh() {
        let mut app = App::test_new();
        app.tabs = vec![Tab::new_file(Editor::test_with_text(
            "needle\nkeep needle\n",
        ))];
        app.active_tab = 0;
        app.search_active = true;
        app.search_query = "needle".into();
        app.update_search();
        assert_eq!(app.search_matches, vec![(0, 0, 6), (1, 5, 11)]);

        app.execute_palette_command("delete_line");

        assert_eq!(app.search_matches, vec![(0, 0, 6), (1, 5, 11)]);
        assert!(app.file_search_refresh_pending);

        app.last_edit_time = std::time::Instant::now() - std::time::Duration::from_millis(100);
        assert!(app.refresh_pending_file_search());
        assert_eq!(app.search_matches, vec![(0, 5, 11)]);
    }

    #[test]
    fn undo_redo_find_rescans_the_edit_range_after_distant_navigation() {
        let mut editor = Editor::test_with_text(&"x\n".repeat(100_000));
        editor
            .rope
            .insert(editor.rope.line_to_char(50_000), "needle ");
        editor.cursor_line = 50_000;
        editor.cursor_col = 3;
        editor.insert_char('x');
        editor.cursor_line = 90_000;
        let mut app = App::test_new();
        app.active_pane = ActivePane::Editor;
        app.tabs = vec![Tab::new_file(editor)];
        app.search_active = true;
        app.search_query = "needle".into();
        app.update_search();
        assert!(app.search_matches.is_empty());

        app.execute_palette_command("undo");
        assert_eq!(app.search_matches, vec![(50_000, 0, 6)]);
        assert!(!app.file_search_refresh_pending);
        let generation = app.file_search_generation;
        app.update_search();
        assert_eq!(
            app.file_search_generation, generation,
            "incremental results must be reusable"
        );

        app.execute_palette_command("redo");
        assert!(app.search_matches.is_empty());
        assert!(!app.file_search_refresh_pending);
    }

    #[test]
    fn undo_redo_find_shifts_matches_when_restoring_a_split_line() {
        let mut editor = Editor::test_with_text("prefix\nneedle\nsuffix needle\n");
        editor.cursor_line = 1;
        editor.cursor_col = 3;
        editor.insert_newline();
        let mut app = App::test_new();
        app.active_pane = ActivePane::Editor;
        app.tabs = vec![Tab::new_file(editor)];
        app.search_active = true;
        app.search_query = "needle".into();
        app.update_search();
        let edited_matches = app.search_matches.clone();
        assert_eq!(edited_matches, vec![(3, 7, 13)]);

        app.execute_palette_command("undo");
        assert_eq!(app.search_matches, vec![(1, 0, 6), (2, 7, 13)]);
        assert!(!app.file_search_refresh_pending);
        app.execute_palette_command("redo");
        assert_eq!(app.search_matches, edited_matches);
        assert!(!app.file_search_refresh_pending);
    }

    #[test]
    fn palette_undo_keeps_lsp_decorations_visible_while_refreshing_feedback() {
        let mut app = App::test_new();
        app.active_pane = ActivePane::Editor;
        let path = std::path::PathBuf::from("undo.cpp");
        let mut editor = Editor::test_with_text("int first;\nint second;\nint value;\n");
        editor.file_path = Some(path.clone());
        editor.cursor_line = 2;
        editor.cursor_col = 3;
        editor.insert_char('x');
        editor.syntax_dirty_from = None;
        editor.syntax_dirty_through = None;
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;
        app.lsp_manager.diagnostics.insert(path.clone(), Vec::new());
        app.lsp_document_highlights.push((0, 0, 0, 3));
        app.lsp_inlay_hints.push(crate::app::InlayHintDisplay::new(0, 3, ": i32", None));

        app.execute_palette_command("undo");

        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            "int first;\nint second;\nint value;\n"
        );
        assert_eq!(app.lsp_document_highlights, vec![(0, 0, 0, 3)]);
        assert_eq!(app.lsp_inlay_hints, vec![crate::app::InlayHintDisplay::new(0, 3, ": i32", None)]);
        assert!(app.lsp_manager.diagnostic_dirty_documents.contains(&path));
        assert_eq!(app.editor().unwrap().syntax_dirty_from, Some(2));
    }

    #[test]
    fn compare_tabs_does_not_use_stale_index_after_split() {
        let mut app = App::test_new();
        app.tabs = vec![make_file_tab("first.txt"), make_file_tab("second.txt")];
        app.active_tab = 0;

        app.execute_palette_command("compare_tabs");

        assert!(app.split_active());
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].editor.file_name(), "second.txt");
        assert!(app
            .groups
            .iter()
            .any(|g| g.tabs.iter().any(|t| t.editor.file_name() == "first.txt")));
        let last_msg = app
            .notification_history
            .last()
            .map(|(msg, _, _)| msg.as_str())
            .unwrap_or("");
        assert!(last_msg.contains("first.txt"));
        assert!(last_msg.contains("second.txt"));
    }

    #[test]
    fn compare_tabs_wraps_to_first_tab() {
        let mut app = App::test_new();
        app.tabs = vec![make_file_tab("first.txt"), make_file_tab("second.txt")];
        app.active_tab = 1;

        app.execute_palette_command("compare_tabs");

        assert!(app.split_active());
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.tabs[0].editor.file_name(), "first.txt");
        let last_msg = app
            .notification_history
            .last()
            .map(|(msg, _, _)| msg.as_str())
            .unwrap_or("");
        assert!(last_msg.contains("second.txt"));
        assert!(last_msg.contains("first.txt"));
    }

    #[test]
    fn toggle_diff_view_populates_compare_state_immediately() {
        let repo_dir = temp_repo_dir("toggle-diff-view");
        std::fs::create_dir_all(&repo_dir).expect("repo dir");
        run_git(&repo_dir, &["init"]);
        run_git(&repo_dir, &["config", "core.autocrlf", "false"]);
        run_git(&repo_dir, &["config", "user.email", "test@example.com"]);
        run_git(&repo_dir, &["config", "user.name", "VTCode Test"]);

        let file_path = repo_dir.join("file.txt");
        std::fs::write(&file_path, "before\n").expect("initial file");
        run_git(&repo_dir, &["add", "file.txt"]);
        run_git(&repo_dir, &["commit", "-m", "initial"]);

        let mut app = App::new(repo_dir.clone());
        let mut editor = Editor::new();
        editor.file_path = Some(file_path);
        editor.rope = ropey::Rope::from_str("after\n");
        editor.modified = true;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;

        app.execute_palette_command("toggle_diff_view");
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            matches!(app.diff_mode, crate::app::DiffMode::SideBySide)
        });

        assert!(matches!(app.diff_mode, crate::app::DiffMode::SideBySide));
        assert_eq!(app.compare_base_content.as_deref(), Some("before\n"));
        assert!(!app.compare_diff_lines.is_empty());
        assert!(app.should_render_diff_for_active_editor());

        app.execute_palette_command("toggle_diff_view");
        assert!(app.diff_mode == crate::app::DiffMode::Inline);
        app.execute_palette_command("toggle_diff_view");
        assert!(app.diff_mode == crate::app::DiffMode::Off);
        app.execute_palette_command("toggle_diff_view");
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            app.should_render_diff_for_active_editor()
        });
        assert!(app.diff_mode == crate::app::DiffMode::Inline);

        let _ = std::fs::remove_dir_all(repo_dir); // Error OK: dir may not exist
    }

    #[test]
    fn toggle_setting_format_on_save_updates_flag() {
        let mut app = App::test_new();

        assert!(!app.format_on_save);

        app.toggle_setting("format_on_save");
        assert!(app.format_on_save);

        app.toggle_setting("format_on_save");
        assert!(!app.format_on_save);
    }

    #[test]
    fn next_diagnostic_wraps_to_first_diagnostic_in_current_file() {
        let mut app = App::test_new();
        let path = PathBuf::from("test.rs");
        app.tabs = vec![make_file_tab("test.rs")];
        app.active_tab = 0;
        app.lsp_manager.diagnostics.insert(
            path,
            vec![
                LspDiagnostic {
                    line: 1,
                    col: 2,
                    end_line: 1,
                    end_col: 4,
                    message: "warning".to_string(),
                    severity: DiagnosticSeverity::WARNING,
                },
                LspDiagnostic {
                    line: 3,
                    col: 1,
                    end_line: 3,
                    end_col: 3,
                    message: "error".to_string(),
                    severity: DiagnosticSeverity::ERROR,
                },
            ],
        );
        if let Some(editor) = app.editor_mut() {
            editor.set_cursor(4, 0);
        }

        app.execute_palette_command("next_diagnostic");

        let editor = app.editor().expect("active editor");
        assert_eq!((editor.cursor_line, editor.cursor_col), (1, 2));
    }

    #[test]
    fn prev_diagnostic_wraps_to_last_diagnostic_in_current_file() {
        let mut app = App::test_new();
        let path = PathBuf::from("test.rs");
        app.tabs = vec![make_file_tab("test.rs")];
        app.active_tab = 0;
        app.lsp_manager.diagnostics.insert(
            path,
            vec![
                LspDiagnostic {
                    line: 1,
                    col: 2,
                    end_line: 1,
                    end_col: 4,
                    message: "warning".to_string(),
                    severity: DiagnosticSeverity::WARNING,
                },
                LspDiagnostic {
                    line: 3,
                    col: 1,
                    end_line: 3,
                    end_col: 3,
                    message: "error".to_string(),
                    severity: DiagnosticSeverity::ERROR,
                },
            ],
        );
        if let Some(editor) = app.editor_mut() {
            editor.set_cursor(0, 0);
        }

        app.execute_palette_command("prev_diagnostic");

        let editor = app.editor().expect("active editor");
        assert_eq!((editor.cursor_line, editor.cursor_col), (3, 1));
    }
}
