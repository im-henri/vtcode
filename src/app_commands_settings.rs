use crate::animation_config::clamp_animation_duration_ms;
use crate::app::App;

impl App {
    pub(crate) fn toggle_setting(&mut self, id: &str) {
        if self.change_completion_setting(id) {
            return;
        }
        self.flush_live_settings();
        if id == "source_control_enabled" {
            let enabled = self.selected_scope_settings().source_control_enabled.unwrap_or(true);
            self.save_source_control_enabled(self.settings_scope, !enabled);
            return;
        }
        if id == "source_control_show_untracked" {
            let show = self.selected_scope_settings().source_control_show_untracked.unwrap_or(true);
            self.save_source_control_show_untracked(self.settings_scope, !show);
            return;
        }
        if id == "animation_scope_bubble_languages" {
            self.open_scope_bubble_settings();
            return;
        }
        if id == "language_associations" {
            self.open_file_associations();
            return;
        }
        if id == "formatters" {
            self.open_formatter_settings(None);
            return;
        }
        if id == "lsp_servers" {
            self.open_lsp_settings();
            return;
        }
        if id == "acp_agents" {
            self.open_agent_settings();
            return;
        }
        let scoped_before = Self::setting_json_key(id)
            .filter(|key| *key != "keybindings")
            .map(|_| self.selected_scope_settings());
        if let Some(before) = &scoped_before {
            // Controls operate on the value visible in the selected scope. The
            // final workspace-effective state is restored after persistence.
            self.apply_setting_value(id, before);
        }
        match id {
            "lsp_enabled" => self.lsp_enabled = !self.lsp_enabled,
            "animation_copy" => {
                self.copy_animation.enabled = !self.copy_animation.enabled;
                if !self.copy_animation.enabled { self.copy_animation.clear(); }
            }
            "animation_scope_bubble_tight" => {
                self.animation_settings.scope_tight = !self.animation_settings.scope_tight;
            }
            "animation_scope_bubble_fill" => {
                self.animation_settings.scope_fill = !self.animation_settings.scope_fill;
            }
            id if crate::animation_config::AnimationSettings::effect_for_id(id).is_some() => {
                if let Some(effect) = crate::animation_config::AnimationSettings::effect_for_id(id) {
                    let enabled = !self.animation_settings.enabled(effect);
                    self.animation_settings.set(effect, enabled);
                }
            }
            "tab_size" => {
                let new_size = match self.default_tab_size {
                    2 => 4,
                    4 => 8,
                    _ => 2,
                };
                self.default_tab_size = new_size;
                for tab in &mut self.tabs {
                    if tab.kind == crate::app::TabKind::File {
                        tab.editor.tab_size = new_size;
                    }
                }
                for group in &mut self.groups {
                    for tab in &mut group.tabs {
                        if tab.kind == crate::app::TabKind::File {
                            tab.editor.tab_size = new_size;
                        }
                    }
                }
                self.notify(format!("Tab size: {new_size}"));
            }
            "use_tabs" => {
                self.default_use_tabs = !self.default_use_tabs;
                for tab in &mut self.tabs {
                    if tab.kind == crate::app::TabKind::File {
                        tab.editor.use_tabs = self.default_use_tabs;
                    }
                }
                for group in &mut self.groups {
                    for tab in &mut group.tabs {
                        if tab.kind == crate::app::TabKind::File {
                            tab.editor.use_tabs = self.default_use_tabs;
                        }
                    }
                }
            }
            "word_wrap" => {
                self.default_word_wrap = !self.default_word_wrap;
                for tab in &mut self.tabs {
                    if tab.kind == crate::app::TabKind::File {
                        tab.editor.word_wrap = self.default_word_wrap;
                        if tab.editor.word_wrap {
                            tab.editor.horizontal_scroll = 0;
                        }
                    }
                }
                for group in &mut self.groups {
                    for tab in &mut group.tabs {
                        if tab.kind == crate::app::TabKind::File {
                            tab.editor.word_wrap = self.default_word_wrap;
                            if tab.editor.word_wrap {
                                tab.editor.horizontal_scroll = 0;
                            }
                        }
                    }
                }
                let state = if self.default_word_wrap { "On" } else { "Off" };
                self.notify(format!("Word Wrap: {state}"));
            }
            "word_wrap_column" => {
                // Cycle through: 0 (viewport) -> 80 -> 120 -> 0
                let new_col = match self.word_wrap_column {
                    0 => 80,
                    80 => 120,
                    _ => 0,
                };
                self.word_wrap_column = new_col;
                for tab in &mut self.tabs {
                    tab.editor.word_wrap_column = new_col;
                }
                for group in &mut self.groups {
                    for tab in &mut group.tabs {
                        tab.editor.word_wrap_column = new_col;
                    }
                }
                let label = if new_col == 0 {
                    "viewport".to_string()
                } else {
                    new_col.to_string()
                };
                self.notify(format!("Word Wrap Column: {label}"));
            }
            "whitespace" => {
                let next_mode = self.default_whitespace_mode.next();
                self.default_whitespace_mode = next_mode;
                for tab in &mut self.tabs {
                    if tab.kind == crate::app::TabKind::File {
                        tab.editor.whitespace_mode = next_mode;
                    }
                }
                for group in &mut self.groups {
                    for tab in &mut group.tabs {
                        if tab.kind == crate::app::TabKind::File {
                            tab.editor.whitespace_mode = next_mode;
                        }
                    }
                }
                self.notify(format!("Render Whitespace: {}", next_mode.name()));
            }
            "read_only" => {
                self.default_read_only = !self.default_read_only;
                for tab in &mut self.tabs {
                    if tab.kind == crate::app::TabKind::File {
                        tab.editor.set_read_only(self.default_read_only);
                    }
                }
                for group in &mut self.groups {
                    for tab in &mut group.tabs {
                        if tab.kind == crate::app::TabKind::File {
                            tab.editor.set_read_only(self.default_read_only);
                        }
                    }
                }
                let state = if self.default_read_only { "On" } else { "Off" };
                self.notify(format!("Read Only: {state}"));
            }
            "quick_suggestions" => {
                self.quick_suggestions = !self.quick_suggestions;
                if !self.quick_suggestions {
                    self.dismiss_vtcomplete();
                    self.lsp_completion_active = false;
                }
            }
            "hover_enabled" => {
                self.hover_enabled = !self.hover_enabled;
                if !self.hover_enabled {
                    self.dismiss_lsp_hover();
                }
            }
            "format_on_type" => {
                self.execute_view_command("toggle_on_type_formatting");
            }
            "theme" => {
                self.next_theme();
                self.notify(format!("Theme: {}", self.theme_name));
            }
            "explorer" => {
                self.show_explorer = !self.show_explorer;
            }
            "show_open_editors" => {
                self.show_open_editors = !self.show_open_editors;
                self.notify(if self.show_open_editors {
                    "Open Editors: On"
                } else {
                    "Open Editors: Off"
                });
            }
            "collapse_explorer" => {
                self.collapse_all_explorer_async();
            }
            "minimap" => {
                self.show_minimap = !self.show_minimap;
            }
            "minimap_width" => {
                // Cycle: 8 -> 10 -> 12 -> 15 -> 20 -> 8
                self.minimap_width = match self.minimap_width {
                    w if w < 10 => 10,
                    10 => 12,
                    12 => 15,
                    15 => 20,
                    _ => 8,
                };
            }
            "line_numbers" => {
                self.show_line_numbers = !self.show_line_numbers;
            }
            "relative_lines" => {
                self.relative_line_numbers = !self.relative_line_numbers;
            }
            "panel" => {
                self.show_panel = !self.show_panel;
                if !self.show_panel {
                    self.terminal_panel_focused = false;
                    crate::ui_panels::close_panel(self);
                }
            }
            "panel_height" => {
                // Cycle: 6 -> 10 -> 15 -> 20 -> 6
                self.panel_height = match self.panel_height {
                    h if h < 10 => 10,
                    10 => 15,
                    15 => 20,
                    _ => 6,
                };
                self.notify(format!("Panel height: {}", self.panel_height));
            }
            "sidebar_pos" => {
                self.sidebar_right = !self.sidebar_right;
            }
            "zen" => {
                self.execute_view_command("toggle_zen");
            }
            "indent_rainbow" => {
                self.indent_rainbow = !self.indent_rainbow;
            }
            "color_preview" => {
                self.show_color_preview = !self.show_color_preview;
            }
            "bracket_pair_colorization" => {
                self.bracket_pair_colorization = !self.bracket_pair_colorization;
            }
            "error_lens" => {
                self.error_lens = !self.error_lens;
            }
            "inlay_hints" => {
                self.execute_view_command("toggle_inlay_hints");
            }
            "code_lens" => {
                self.execute_view_command("toggle_code_lens");
            }
            "semantic_highlighting" => {
                self.execute_view_command("toggle_semantic_tokens");
            }
            "focus_mode" => {
                self.focus_mode = !self.focus_mode;
            }
            "sticky_scroll" => {
                self.sticky_scroll = !self.sticky_scroll;
            }
            "sticky_scroll_max" => {
                self.sticky_scroll_max = match self.sticky_scroll_max {
                    1 => 2,
                    2 => 3,
                    3 => 4,
                    4 => 5,
                    _ => 1,
                };
            }
            "scroll_past_end" => {
                self.scroll_past_end = !self.scroll_past_end;
            }
            "word_count" => {
                self.show_word_count = !self.show_word_count;
            }
            "screencast_mode" => {
                self.execute_view_command("toggle_screencast_mode");
            }
            "max_global_search_results" => {
                self.max_global_search_results = match self.max_global_search_results {
                    v if v < 1000 => 1000,
                    1000 => 2000,
                    2000 => 5000,
                    5000 => 10000,
                    10000 => 20000,
                    _ => 1000,
                };
                self.notify(format!(
                    "Search view max results: {}",
                    self.max_global_search_results
                ));
            }
            "large_file_threshold_mb" => {
                self.large_file_threshold_mb = match self.large_file_threshold_mb {
                    value if value < 25 => 25,
                    25 => 50,
                    50 => 100,
                    100 => 250,
                    250 => 500,
                    _ => 10,
                };
                let threshold_bytes = self.large_file_threshold_mb.saturating_mul(1024 * 1024);
                for tab in &mut self.tabs {
                    tab.editor.update_large_file_mode(threshold_bytes);
                    if !tab.editor.large_file_mode {
                        tab.editor.word_wrap = self.default_word_wrap;
                    }
                }
                for group in &mut self.groups {
                    for tab in &mut group.tabs {
                        tab.editor.update_large_file_mode(threshold_bytes);
                        if !tab.editor.large_file_mode {
                            tab.editor.word_wrap = self.default_word_wrap;
                        }
                    }
                }
                self.notify(format!(
                    "Large file threshold: {} MB",
                    self.large_file_threshold_mb
                ));
            }
            "large_file_analysis_timeout_ms" => {
                self.large_file_analysis_timeout_ms = match self.large_file_analysis_timeout_ms {
                    value if value < 500 => 500,
                    500 => 1_000,
                    1_000 => 2_000,
                    2_000 => 5_000,
                    5_000 => 10_000,
                    _ => 250,
                };
                // A timeout change is an explicit request to try rich features
                // again under the new budget. Size-based large-file mode still
                // remains in force independently.
                let threshold_bytes = self.large_file_threshold_mb.saturating_mul(1024 * 1024);
                for tab in &mut self.tabs {
                    tab.editor.rich_features_timed_out = false;
                    tab.editor.update_large_file_mode(threshold_bytes);
                    if !tab.editor.large_file_mode {
                        tab.editor.word_wrap = self.default_word_wrap;
                    }
                }
                for group in &mut self.groups {
                    for tab in &mut group.tabs {
                        tab.editor.rich_features_timed_out = false;
                        tab.editor.update_large_file_mode(threshold_bytes);
                        if !tab.editor.large_file_mode {
                            tab.editor.word_wrap = self.default_word_wrap;
                        }
                    }
                }
                self.notify(format!(
                    "Rich analysis timeout: {} ms",
                    self.large_file_analysis_timeout_ms
                ));
            }
            "rulers" => {
                // Cycle: None -> 80 -> 80,120 -> None
                if self.rulers.is_empty() {
                    self.rulers = vec![80];
                } else if self.rulers.len() == 1 {
                    self.rulers = vec![80, 120];
                } else {
                    self.rulers.clear();
                }
            }
            "trim_trailing" => {
                self.trim_trailing_on_save = !self.trim_trailing_on_save;
                let state = if self.trim_trailing_on_save {
                    "On"
                } else {
                    "Off"
                };
                self.notify(format!("Trim trailing whitespace: {state}"));
            }
            "format_on_save" => {
                self.format_on_save = !self.format_on_save;
                let state = if self.format_on_save { "On" } else { "Off" };
                self.notify(format!("Format on save: {state}"));
            }
            "final_newline" => {
                self.insert_final_newline = !self.insert_final_newline;
                let state = if self.insert_final_newline {
                    "On"
                } else {
                    "Off"
                };
                self.notify(format!("Insert final newline: {state}"));
            }
            "indent_guides" => {
                self.show_indent_guides = !self.show_indent_guides;
            }
            "activity_bar" => {
                if self.zen_mode {
                    let mut state = self
                        .zen_pre_state
                        .unwrap_or((true, true, false, true, true, true));
                    state.5 = !state.5;
                    self.zen_pre_state = Some(state);
                    self.show_activity_bar = false;
                } else {
                    self.show_activity_bar = !self.show_activity_bar;
                }
            }
            "breadcrumbs" => {
                self.show_breadcrumbs = !self.show_breadcrumbs;
            }
            "status_bar" => {
                self.show_status_bar = !self.show_status_bar;
            }
            "tab_bar" => {
                self.show_tab_bar = !self.show_tab_bar;
            }
            "preview_tabs" => {
                self.set_preview_tabs_enabled(!self.enable_preview_tabs);
            }
            "mouse_back_forward" => {
                self.mouse_back_forward_to_navigate = !self.mouse_back_forward_to_navigate;
            }
            "menu_bar" => {
                self.show_menu_bar = !self.show_menu_bar;
            }
            "cursor_blinking" => {
                self.cursor_blinking = !self.cursor_blinking;
            }
            "cursor_surrounding_lines" => {
                self.default_cursor_surrounding_lines = match self.default_cursor_surrounding_lines
                {
                    0 => 3,
                    3 => 5,
                    5 => 10,
                    _ => 0,
                };
                let value = self.default_cursor_surrounding_lines;
                for tab in &mut self.tabs {
                    tab.editor.cursor_surrounding_lines = value;
                }
                for group in &mut self.groups {
                    for tab in &mut group.tabs {
                        tab.editor.cursor_surrounding_lines = value;
                    }
                }
            }
            "cursor_style" => {
                self.cursor_style = match self.cursor_style {
                    crate::app::CursorStyle::Line => crate::app::CursorStyle::Block,
                    crate::app::CursorStyle::Block => crate::app::CursorStyle::Underline,
                    crate::app::CursorStyle::Underline => crate::app::CursorStyle::Line,
                };
            }
            "auto_save_mode" => {
                use crate::app::AutoSaveMode;
                self.auto_save = match self.auto_save {
                    AutoSaveMode::Off => AutoSaveMode::AfterDelay,
                    AutoSaveMode::AfterDelay => AutoSaveMode::OnFocusChange,
                    AutoSaveMode::OnFocusChange => AutoSaveMode::OnWindowChange,
                    AutoSaveMode::OnWindowChange => AutoSaveMode::Off,
                };
            }
            "auto_save_delay" => {
                self.auto_save_delay = match self.auto_save_delay {
                    d if d < 500 => 500,
                    500 => 1000,
                    1000 => 2000,
                    2000 => 5000,
                    _ => 100,
                };
                self.notify(format!("Auto-save delay: {}ms", self.auto_save_delay));
            }
            "search_case_sensitive" => {
                self.search_case_sensitive = !self.search_case_sensitive;
            }
            "search_whole_word" => {
                self.search_whole_word = !self.search_whole_word;
            }
            "search_regex" => {
                self.search_regex = !self.search_regex;
            }
            "search_preserve_case" => {
                self.search_preserve_case = !self.search_preserve_case;
            }
            _ => {
                // Headings are not interactive
                if id.starts_with("heading_") {
                    return;
                }
                // Handle keybinding editing from settings
                if id.starts_with("kb_") {
                    self.settings_kb_editing = true;
                    self.notify("Press new key combination (Esc to cancel)");
                    return;
                }
                if id.starts_with("shortcut_ro_") {
                    self.notify("This shortcut is currently read-only");
                    return;
                }
                // Handle clipboard history paste
                if let Some(idx_str) = id.strip_prefix("clipboard_") {
                    if let Ok(idx) = idx_str.parse::<usize>() {
                        let history_len = self.clipboard_history.len();
                        // Index is reversed (0 = most recent)
                        let real_idx = history_len.saturating_sub(1).saturating_sub(idx);
                        if let Some(text) = self.clipboard_history.get(real_idx).cloned() {
                            self.paste_editor_text(text, true);
                        }
                    }
                }
                // Handle diff compare selection
                if let Some(idx_str) = id.strip_prefix("diff_compare_") {
                    if let Ok(tab_idx) = idx_str.parse::<usize>() {
                        if tab_idx < self.tabs.len() {
                            let base_name = self.tabs[tab_idx].editor.file_name().to_string();
                            let target_name = self
                                .editor()
                                .map(|e| e.file_name().to_string())
                                .unwrap_or_default();
                            if self.begin_tab_compare(tab_idx) {
                                self.notify(format!("Preparing diff: {base_name} ↔ {target_name}"));
                            }
                        }
                    }
                }
            }
        }
        if let Some(before) = scoped_before {
            self.persist_scoped_setting_change(id, &before);
        }
    }

    pub(crate) fn setting_dropdown_options(&self, id: &str) -> Option<Vec<String>> {
        if let Some(options) = self.completion_dropdown_options(id) {
            return Some(
                options.iter()
                    .map(|value| (*value).to_owned())
                    .collect(),
            );
        }
        match id {
            "theme" => Some(self.loaded_themes.iter().map(|t| t.name.clone()).collect()),
            "whitespace" => Some(
                [
                    crate::app::WhitespaceMode::None,
                    crate::app::WhitespaceMode::Boundary,
                    crate::app::WhitespaceMode::Selection,
                    crate::app::WhitespaceMode::Trailing,
                    crate::app::WhitespaceMode::All,
                ]
                .iter()
                .map(|mode| mode.name().to_string())
                .collect(),
            ),
            "comment_whitespace" => Some(
                ["Inherit", "None", "Boundary", "Selection", "Trailing", "All"]
                    .into_iter().map(str::to_string).collect(),
            ),
            "sidebar_pos" => Some(vec!["Left".to_string(), "Right".to_string()]),
            "rulers" => Some(vec![
                "None".to_string(),
                "80".to_string(),
                "80, 120".to_string(),
            ]),
            "cursor_style" => Some(vec![
                "Line".to_string(),
                "Block".to_string(),
                "Underline".to_string(),
            ]),
            "auto_save_mode" => Some(vec![
                "Off".to_string(),
                "After Delay".to_string(),
                "On Focus Change".to_string(),
                "On Window Change".to_string(),
            ]),
            _ => None,
        }
    }

    pub(crate) fn begin_numeric_setting_edit(&mut self, id: &str, current: &str) {
        if !matches!(Self::setting_field_kind(id), "number" | "language-list") {
            return;
        }
        self.text_input_histories.remove(&crate::text_input::TextInputId::SettingsValue);
        self.settings_value_editing = true;
        self.settings_value_setting_id = Some(id.to_string());
        self.settings_value_buffer = current.trim().to_string();
        self.settings_value_cursor = self.settings_value_buffer.len();
        self.settings_value_selection_anchor = Some(0).filter(|_| !self.settings_value_buffer.is_empty());
        self.settings_dropdown_open = false;
    }

    pub(crate) fn cancel_numeric_setting_edit(&mut self) {
        self.settings_value_editing = false;
        self.settings_value_setting_id = None;
        self.settings_value_buffer.clear();
        self.settings_value_cursor = 0;
        self.settings_value_selection_anchor = None;
    }

    pub(crate) fn finish_numeric_setting_edit(&mut self) {
        if self.settings_value_editing && !self.commit_numeric_setting_edit() {
            // Losing focus with an empty or overflowing value behaves like
            // cancelling the edit; Enter keeps the field focused for retry.
            self.cancel_numeric_setting_edit();
        }
    }

    pub(crate) fn commit_numeric_setting_edit(&mut self) -> bool {
        self.flush_live_settings();
        let Some(id) = self.settings_value_setting_id.clone() else {
            self.cancel_numeric_setting_edit();
            return false;
        };
        if id == "animation_scope_bubble_disabled_languages" {
            let value = crate::settings::Settings {
                animation_scope_bubble_disabled_languages: Some(
                    self.settings_value_buffer.split(',').map(str::trim)
                        .filter(|language| !language.is_empty()).map(str::to_owned).collect(),
                ),
                ..Default::default()
            };
            let scoped_before = self.selected_scope_settings();
            self.apply_setting_value(&id, &value);
            self.cancel_numeric_setting_edit();
            self.persist_scoped_setting_change(&id, &scoped_before);
            return true;
        }
        let Ok(raw) = self.settings_value_buffer.trim().parse::<u64>() else {
            self.notify("Enter a whole numeric value");
            return false;
        };
        if let Some(error) = crate::app_vtcomplete_settings::number_error(&id, raw) {
            self.notify(error);
            return false;
        }
        if self.set_completion_number(&id, raw) {
            self.cancel_numeric_setting_edit();
            return true;
        }

        let mut value = crate::settings::Settings::default();
        match id.as_str() {
            "animation_copy_duration_ms" => value.animation_copy_duration_ms = Some(clamp_animation_duration_ms(raw)),
            id if crate::animation_config::AnimationSettings::effect_for_duration_id(id)
                .is_some() =>
            {
                let effect =
                    crate::animation_config::AnimationSettings::effect_for_duration_id(id).unwrap();
                value.set_animation_effect_duration(effect, clamp_animation_duration_ms(raw));
            }
            id if crate::animation_config::AnimationSettings::effect_for_intensity_id(id)
                .is_some() =>
            {
                let effect =
                    crate::animation_config::AnimationSettings::effect_for_intensity_id(id)
                        .unwrap();
                value.set_animation_effect_intensity(effect, raw.clamp(5, 100) as u8);
            }
            "tab_size" => value.tab_size = Some(raw.clamp(1, 8) as u8),
            "word_wrap_column" => {
                value.word_wrap_column = Some(if raw == 0 { 0 } else { raw.clamp(40, 500) as usize })
            }
            "minimap_width" => value.minimap_width = Some(raw.clamp(5, 40) as u16),
            "panel_height" => value.panel_height = Some(raw.clamp(3, u16::MAX as u64) as u16),
            "sticky_scroll_max" => value.sticky_scroll_max = Some(raw.clamp(1, 5) as usize),
            "max_global_search_results" => {
                value.max_global_search_results = Some(raw.max(1) as usize)
            }
            "large_file_threshold_mb" => {
                value.large_file_threshold_mb = Some(raw.clamp(1, 16_384))
            }
            "large_file_analysis_timeout_ms" => {
                value.large_file_analysis_timeout_ms = Some(raw.clamp(100, 60_000))
            }
            "cursor_surrounding_lines" => {
                value.cursor_surrounding_lines = Some(raw.min(50) as usize)
            }
            "scroll_speed" => value.scroll_speed = Some(raw.clamp(1, 10) as usize),
            "auto_save_delay" => value.auto_save_delay = Some(raw.clamp(100, 3_600_000)),
            "lsp_diagnostic_delay_ms" => value.lsp_diagnostic_delay_ms = Some(raw.min(crate::lsp::diagnostics::MAX_DELAY_MS)),
            "animation_duration_ms" => {
                value.animation_duration_ms = Some(clamp_animation_duration_ms(raw))
            }
            "animation_fps" => value.animation_fps = Some(raw.clamp(15, 120)),
            "animation_intensity" => value.animation_intensity = Some(raw.clamp(5, 100) as u8),
            _ => return false,
        }

        let scoped_before = self.selected_scope_settings();
        self.apply_setting_value(&id, &value);
        self.cancel_numeric_setting_edit();
        self.persist_scoped_setting_change(&id, &scoped_before);
        true
    }

    pub(crate) fn open_settings_dropdown(&mut self, id: &str, current: &str) {
        let Some(options) = self.setting_dropdown_options(id) else {
            return;
        };
        let mut selected = options
            .iter()
            .position(|opt| opt.eq_ignore_ascii_case(current.trim()))
            .unwrap_or(0);
        self.theme_management.delete_hits.clear();
        self.theme_management.picker_removable = if id == "theme" {
            self.loaded_themes.iter().map(|theme| !theme.builtin).collect()
        } else { Vec::new() };
        self.theme_management.picker_keys = if id == "theme" {
            selected = self.loaded_themes.iter().position(|theme| theme.config_key == self.theme_key
                && theme.name.eq_ignore_ascii_case(current.trim())).unwrap_or(selected);
            self.loaded_themes.iter().map(|theme| theme.config_key.clone()).collect()
        } else { Vec::new() };
        self.settings_dropdown_open = true;
        self.settings_dropdown_setting_id = Some(id.to_string());
        self.settings_dropdown_options = options;
        self.settings_dropdown_selected = selected;
        // Center scroll so selected item is visible
        self.settings_dropdown_scroll = selected.saturating_sub(5);
        self.settings_dropdown_area = None;
        self.settings_dropdown_theme_original = if id == "theme" {
            Some(self.theme_key.clone())
        } else {
            None
        };
        self.settings_dropdown_committed = false;
    }

    pub(crate) fn open_settings_category_dropdown(&mut self) {
        self.close_settings_dropdown();
        self.settings_focus_categories = true;
        self.settings_focus_control = false;
        self.settings_dropdown_open = true;
        let pages = Self::settings_page_options();
        self.settings_dropdown_selected = pages.iter().position(|(category, section, _)| {
            *category == self.settings_category_selected && *section == self.settings_selected_section(*category)
        }).unwrap_or(0);
        self.settings_dropdown_options = pages.into_iter().map(|(_, _, label)| label).collect();
    }

    pub(crate) fn select_settings_dropdown_option(&mut self, option_idx: usize) {
        if let Some(value) = self.settings_dropdown_options.get(option_idx).cloned() {
            self.settings_dropdown_selected = option_idx;
            if let Some(id) = self.settings_dropdown_setting_id.clone() {
                self.apply_settings_dropdown_value(&id, &value);
            } else if let Some(&(category, section, _)) = Self::settings_page_options().get(option_idx) {
                self.settings_category_collapsed[category] = false;
                self.select_settings_page(category, section);
            }
        }
        self.close_settings_dropdown();
    }

    pub(crate) fn close_settings_dropdown(&mut self) {
        if !self.settings_dropdown_committed
            && self.settings_dropdown_setting_id.as_deref() == Some("theme")
        {
            if let Some(original_key) = self.settings_dropdown_theme_original.take() {
                self.set_theme_by_key(&original_key);
            }
        }
        self.settings_dropdown_open = false;
        self.settings_dropdown_setting_id = None;
        self.settings_dropdown_options.clear();
        self.settings_dropdown_selected = 0;
        self.settings_dropdown_scroll = 0;
        self.settings_dropdown_area = None;
        self.settings_dropdown_theme_original = None;
        self.settings_dropdown_committed = false;
        self.theme_management.picker_keys.clear();
        self.theme_management.picker_removable.clear();
        self.theme_management.delete_hits.clear();
    }

    pub(crate) fn preview_settings_dropdown_value(&mut self, id: &str, value: &str) {
        if id == "theme" {
            if let Some(key) = self.settings_theme_key(value) {
                self.set_theme_by_key(&key);
            }
        }
    }

    pub(crate) fn apply_settings_dropdown_value(&mut self, id: &str, value: &str) {
        if self.set_completion_dropdown(id, value) {
            self.settings_dropdown_committed = true;
            return;
        }
        self.flush_live_settings();
        self.settings_dropdown_committed = true;
        let scoped_before = self.selected_scope_settings();
        self.apply_setting_value(id, &scoped_before);
        match id {
            "comment_whitespace" => {
                self.comment_whitespace_mode = match value {
                    "None" => Some(crate::app::WhitespaceMode::None),
                    "Boundary" => Some(crate::app::WhitespaceMode::Boundary),
                    "Selection" => Some(crate::app::WhitespaceMode::Selection),
                    "Trailing" => Some(crate::app::WhitespaceMode::Trailing),
                    "All" => Some(crate::app::WhitespaceMode::All),
                    _ => None,
                };
            }
            "whitespace" => {
                self.default_whitespace_mode = match value {
                    "Boundary" => crate::app::WhitespaceMode::Boundary,
                    "Selection" => crate::app::WhitespaceMode::Selection,
                    "Trailing" => crate::app::WhitespaceMode::Trailing,
                    "All" => crate::app::WhitespaceMode::All,
                    _ => crate::app::WhitespaceMode::None,
                };
            }
            "theme" => {
                if let Some(key) = self.settings_theme_key(value) {
                    self.set_theme_by_key(&key);
                    self.notify(format!("Theme: {}", self.theme_name));
                }
            }
            "sidebar_pos" => {
                self.sidebar_right = value.eq_ignore_ascii_case("right");
                self.notify(format!(
                    "Sidebar Position: {}",
                    if self.sidebar_right { "Right" } else { "Left" }
                ));
            }
            "rulers" => {
                self.rulers = match value {
                    "None" => Vec::new(),
                    "80" => vec![80],
                    _ => vec![80, 120],
                };
                self.notify(format!(
                    "Column Rulers: {}",
                    if self.rulers.is_empty() {
                        "None".to_string()
                    } else {
                        self.rulers
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(", ")
                    }
                ));
            }
            "cursor_style" => {
                self.cursor_style = match value {
                    "Block" => crate::app::CursorStyle::Block,
                    "Underline" => crate::app::CursorStyle::Underline,
                    _ => crate::app::CursorStyle::Line,
                };
                self.notify(format!("Cursor Style: {value}"));
            }
            "auto_save_mode" => {
                self.auto_save = match value {
                    "After Delay" => crate::app::AutoSaveMode::AfterDelay,
                    "On Focus Change" => crate::app::AutoSaveMode::OnFocusChange,
                    "On Window Change" => crate::app::AutoSaveMode::OnWindowChange,
                    _ => crate::app::AutoSaveMode::Off,
                };
                self.notify(format!("Auto Save: {value}"));
            }
            _ => {}
        }
        self.persist_scoped_setting_change(id, &scoped_before);
    }
}

#[cfg(test)]
mod tests {
    use crate::app::App;
    use crate::app::Tab;
    use crate::editor::Editor;
    use std::path::PathBuf;

    fn make_app() -> App {
        let tmp = std::env::temp_dir().join("vtcode_settings_test");
        let _ = std::fs::create_dir_all(&tmp);
        let mut app = App::new(tmp);
        let mut ed = Editor::new();
        ed.file_path = Some(PathBuf::from("test.rs"));
        app.tabs.push(Tab::new_file(ed));
        app.active_tab = 0;
        app
    }

    // --- toggle_setting: boolean toggles ---

    #[test]
    fn scope_language_list_edits_persist_replace_and_reset_by_scope() {
        let mut app = App::test_new();
        let directory = tempfile::tempdir().unwrap();
        app.settings_workspace_path = directory.path().join("workspace.json");
        app.settings_persistence_enabled = true;
        let id = "animation_scope_bubble_disabled_languages";
        app.settings_global.animation_scope_bubble_disabled_languages = Some(vec!["python".into()]);
        app.apply_effective_settings();
        app.settings_scope = crate::settings::SettingsScope::Workspace;
        assert_eq!(app.setting_source_label(id), "Inherited · Global");
        app.begin_numeric_setting_edit(id, "python");
        app.settings_value_buffer = "JavaScript, C++, javascript".into();
        assert!(app.commit_numeric_setting_edit());
        assert_eq!(app.animation_settings.scope_disabled_languages, ["cpp", "javascript"]);
        assert_eq!(app.settings_global.animation_scope_bubble_disabled_languages, Some(vec!["python".into()]));
        let persisted = crate::settings::load_from_file(&app.settings_workspace_path);
        assert_eq!(persisted.animation_scope_bubble_disabled_languages, Some(vec!["cpp".into(), "javascript".into()]));
        app.begin_numeric_setting_edit(id, "cpp, javascript");
        app.settings_value_buffer.clear();
        assert!(app.commit_numeric_setting_edit());
        assert!(app.animation_settings.scope_disabled_languages.is_empty());
        assert_eq!(app.settings_workspace.animation_scope_bubble_disabled_languages, Some(vec![]));
        assert!(app.reset_setting_in_selected_scope(id));
        assert_eq!(app.animation_settings.scope_disabled_languages, ["python"]);
    }

    #[test]
    fn every_animation_can_be_toggled_and_reset_independently() {
        use crate::animation_config::AnimationEffect;
        let mut app = App::test_new();
        for effect in AnimationEffect::ALL {
            assert!(!app.animation_settings.any_enabled());
            app.toggle_setting(effect.id());
            for other in AnimationEffect::ALL {
                assert_eq!(app.animation_settings.enabled(other), other == effect);
            }
            assert_eq!(app.setting_source_label(effect.id()), "Global");
            assert!(app.reset_setting_in_selected_scope(effect.id()));
            assert!(!app.animation_settings.any_enabled());
            assert_eq!(app.setting_source_label(effect.id()), "Default");
        }
    }

    #[test]
    fn animation_workspace_override_can_disable_and_reset_to_inherited_effect() {
        use crate::animation_config::AnimationEffect;
        let mut app = App::test_new();
        app.settings_global.animation_cursor_smear = Some(true);
        app.apply_effective_settings();
        app.settings_scope = crate::settings::SettingsScope::Workspace;
        app.toggle_setting("animation_cursor_smear");
        assert_eq!(app.settings_global.animation_cursor_smear, Some(true));
        assert_eq!(app.settings_workspace.animation_cursor_smear, Some(false));
        assert!(!app.animation_settings.enabled(AnimationEffect::CursorSmear));
        assert!(app.reset_setting_in_selected_scope("animation_cursor_smear"));
        assert!(app.animation_settings.enabled(AnimationEffect::CursorSmear));
        assert_eq!(app.setting_source_label("animation_cursor_smear"), "Inherited · Global");
    }

    #[test]
    fn fire_hazard_workspace_override_preserves_cursor_smear_and_resets_to_global() {
        use crate::animation_config::AnimationEffect;
        let mut app = App::test_new();
        app.settings_global.animation_cursor_fire = Some(true);
        app.settings_global.animation_cursor_smear = Some(true);
        app.apply_effective_settings();
        app.settings_scope = crate::settings::SettingsScope::Workspace;
        app.toggle_setting("animation_cursor_fire");
        assert_eq!(app.settings_global.animation_cursor_fire, Some(true));
        assert_eq!(app.settings_workspace.animation_cursor_fire, Some(false));
        assert!(!app.animation_settings.enabled(AnimationEffect::CursorFire));
        assert!(app.animation_settings.enabled(AnimationEffect::CursorSmear));
        let persisted = app.current_settings();
        assert_eq!(persisted.animation_cursor_fire, Some(false));
        assert_eq!(persisted.animation_cursor_smear, Some(true));
        assert!(app.reset_setting_in_selected_scope("animation_cursor_fire"));
        assert!(app.animation_settings.enabled(AnimationEffect::CursorFire));
        assert_eq!(app.settings_workspace.animation_cursor_fire, None);
        assert_eq!(app.setting_source_label("animation_cursor_fire"), "Inherited · Global");
        assert!(app.animation_settings.enabled(AnimationEffect::CursorSmear));
    }

    #[test]
    fn animation_numeric_controls_clamp_persist_and_reset() {
        let mut app = App::test_new();
        for (id, input, expected, default) in [
            (
                "animation_scope_connector_duration_ms",
                "99999",
                "5000",
                "240",
            ),
            ("animation_cursor_smear_duration_ms", "99999", "5000", "300"),
            ("animation_fps", "0", "15", "90"),
            ("animation_scope_connector_intensity", "99999", "100", "55"),
        ] {
            app.begin_numeric_setting_edit(id, default);
            app.settings_value_buffer = input.into();
            assert!(app.commit_numeric_setting_edit());
            let value = app.settings_items().into_iter()
                .find(|(_, _, setting_id)| setting_id == id).unwrap().1;
            assert_eq!(value, expected);
            assert!(app.reset_setting_in_selected_scope(id));
            let value = app.settings_items().into_iter()
                .find(|(_, _, setting_id)| setting_id == id).unwrap().1;
            assert_eq!(value, default);
            assert!(!app.animation_settings.any_enabled());
        }
    }

    #[test]
    fn every_animation_duration_control_persists_the_new_bounds() {
        use crate::animation_config::AnimationEffect;
        let mut ids = AnimationEffect::ALL
            .into_iter()
            .map(AnimationEffect::duration_id)
            .collect::<Vec<_>>();
        ids.extend(["animation_duration_ms"]);
        for id in ids {
            for (input, expected) in [("0", 1), ("1", 1), ("5000", 5000), ("99999", 5000)] {
                let mut app = App::test_new();
                app.settings_scope = crate::settings::SettingsScope::Workspace;
                app.begin_numeric_setting_edit(id, "240");
                app.settings_value_buffer = input.into();
                assert!(app.commit_numeric_setting_edit(), "{id} = {input}");
                let key = App::setting_json_key(id).unwrap();
                assert_eq!(
                    serde_json::to_value(&app.settings_workspace).unwrap()[key].as_u64(),
                    Some(expected)
                );
                if id == "animation_duration_ms" {
                    for effect in AnimationEffect::ALL {
                        assert_eq!(app.animation_settings.duration(effect), expected);
                    }
                } else {
                    let effect =
                        crate::animation_config::AnimationSettings::effect_for_duration_id(id)
                            .unwrap();
                    assert_eq!(app.animation_settings.duration(effect), expected);
                }
            }
        }
    }

    #[test]
    fn animation_effect_controls_are_independent_and_restore_inherited_values() {
        use crate::animation_config::AnimationEffect;
        let mut app = App::test_new();
        app.settings_global.animation_duration_ms = Some(380);
        app.settings_global.animation_intensity = Some(65);
        app.apply_effective_settings();
        app.settings_scope = crate::settings::SettingsScope::Workspace;
        for (index, effect) in AnimationEffect::ALL.into_iter().enumerate() {
            assert_eq!(app.animation_settings.duration(effect), 380);
            assert_eq!(app.animation_settings.intensity(effect), 65);
            let duration = 410 + index as u64 * 20;
            let intensity = 70 + index as u8;
            for (id, input) in [
                (effect.duration_id(), duration.to_string()),
                (effect.intensity_id(), intensity.to_string()),
            ] {
                app.begin_numeric_setting_edit(id, "0");
                app.settings_value_buffer = input;
                assert!(app.commit_numeric_setting_edit());
                assert_eq!(app.setting_source_label(id), "Workspace");
            }
            assert_eq!(app.animation_settings.duration(effect), duration);
            assert_eq!(app.animation_settings.intensity(effect), intensity);
            for other in AnimationEffect::ALL.into_iter().skip(index + 1) {
                assert_eq!(app.animation_settings.duration(other), 380);
                assert_eq!(app.animation_settings.intensity(other), 65);
            }
        }
        for effect in AnimationEffect::ALL {
            assert!(app.reset_setting_in_selected_scope(effect.duration_id()));
            assert!(app.reset_setting_in_selected_scope(effect.intensity_id()));
            assert_eq!(app.animation_settings.duration(effect), 380);
            assert_eq!(app.animation_settings.intensity(effect), 65);
        }
    }

    #[test]
    fn scope_bubble_options_toggle_persist_and_reset_independently() {
        let mut app = App::test_new();
        app.settings_global.animation_scope_bubble_tight = Some(false);
        app.apply_effective_settings();
        app.settings_scope = crate::settings::SettingsScope::Workspace;
        assert!(!app.animation_settings.scope_tight);
        app.toggle_setting("animation_scope_bubble_tight");
        assert!(app.animation_settings.scope_tight);
        let persisted = app.current_settings();
        assert_eq!(persisted.animation_scope_bubble_tight, Some(true));
        assert!(app.reset_setting_in_selected_scope("animation_scope_bubble_tight"));
        assert!(!app.animation_settings.scope_tight);
        assert!(!app.animation_settings.scope_fill);
        app.toggle_setting("animation_scope_bubble_fill");
        assert!(app.animation_settings.scope_fill);
        assert!(!app.animation_settings.scope_tight);
        assert_eq!(app.current_settings().animation_scope_bubble_fill, Some(true));
        assert_eq!(app.settings_workspace.animation_scope_bubble_fill, Some(true));
        assert!(app.reset_setting_in_selected_scope("animation_scope_bubble_fill"));
        assert!(!app.animation_settings.scope_fill);
        app.settings_global.animation_scope_bubble_fill = Some(true);
        app.apply_effective_settings();
        assert!(app.animation_settings.scope_fill);
        app.toggle_setting("animation_scope_bubble_fill");
        assert!(!app.animation_settings.scope_fill);
        assert!(app.reset_setting_in_selected_scope("animation_scope_bubble_fill"));
        assert!(app.animation_settings.scope_fill);
    }

    #[test]
    fn toggle_minimap() {
        let mut app = make_app();
        let before = app.show_minimap;
        app.toggle_setting("minimap");
        assert_ne!(app.show_minimap, before);
        app.toggle_setting("minimap");
        assert_eq!(app.show_minimap, before);
    }

    #[test]
    fn toggle_line_numbers() {
        let mut app = make_app();
        let before = app.show_line_numbers;
        app.toggle_setting("line_numbers");
        assert_ne!(app.show_line_numbers, before);
    }

    #[test]
    fn toggle_panel() {
        let mut app = make_app();
        let before = app.show_panel;
        app.toggle_setting("panel");
        assert_ne!(app.show_panel, before);
    }

    #[test]
    fn toggle_explorer() {
        let mut app = make_app();
        let before = app.show_explorer;
        app.toggle_setting("explorer");
        assert_ne!(app.show_explorer, before);
    }

    #[test]
    fn toggle_indent_rainbow() {
        let mut app = make_app();
        let before = app.indent_rainbow;
        app.toggle_setting("indent_rainbow");
        assert_ne!(app.indent_rainbow, before);
    }

    #[test]
    fn toggle_sticky_scroll() {
        let mut app = make_app();
        let before = app.sticky_scroll;
        app.toggle_setting("sticky_scroll");
        assert_ne!(app.sticky_scroll, before);
    }

    #[test]
    fn toggle_scroll_past_end() {
        let mut app = make_app();
        let before = app.scroll_past_end;
        app.toggle_setting("scroll_past_end");
        assert_ne!(app.scroll_past_end, before);
    }

    #[test]
    fn toggle_activity_bar_changes_visibility() {
        let mut app = make_app();
        app.show_activity_bar = true;
        app.toggle_setting("activity_bar");
        assert!(!app.show_activity_bar);
    }

    #[test]
    fn toggle_language_features_clear_stale_results() {
        let mut app = make_app();
        app.lsp_inlay_hints.push(crate::app::InlayHintDisplay::new(0, 0, "hint", None));
        app.lsp_code_lenses.push((0, "1 reference".into()));
        app.lsp_semantic_tokens
            .push(crate::lsp::DecodedSemanticToken {
                line: 0,
                start_char: 0,
                length: 1,
                token_type: 0,
                _token_modifiers: 0,
            });

        app.toggle_setting("inlay_hints");
        app.toggle_setting("code_lens");
        app.toggle_setting("semantic_highlighting");

        assert!(!app.show_inlay_hints);
        assert!(!app.show_code_lenses);
        assert!(!app.show_semantic_tokens);
        assert!(app.lsp_inlay_hints.is_empty());
        assert!(app.lsp_code_lenses.is_empty());
        assert!(app.lsp_semantic_tokens.is_empty());
    }

    #[test]
    fn toggle_suggestion_and_hover_settings_close_active_ui() {
        let mut app = make_app();
        app.vtcomplete_active = true;
        app.lsp_completion_active = true;
        app.lsp_hover_active = true;
        app.lsp_hover_pending_pos = Some((1, 2));
        app.lsp_manager.hover_content = Some("hover".into());

        app.toggle_setting("quick_suggestions");
        app.toggle_setting("hover_enabled");

        assert!(!app.quick_suggestions);
        assert!(!app.vtcomplete_active);
        assert!(!app.lsp_completion_active);
        assert!(!app.hover_enabled);
        assert!(!app.lsp_hover_active);
        assert!(app.lsp_hover_pending_pos.is_none());
        assert!(app.lsp_manager.hover_content.is_none());
    }

    #[test]
    fn toggle_search_defaults_are_functional_flags() {
        let mut app = make_app();
        app.toggle_setting("search_case_sensitive");
        app.toggle_setting("search_whole_word");
        app.toggle_setting("search_regex");
        app.toggle_setting("search_preserve_case");
        assert!(app.search_case_sensitive);
        assert!(app.search_whole_word);
        assert!(app.search_regex);
        assert!(app.search_preserve_case);
    }

    // --- toggle_setting: cycling values ---

    #[test]
    fn toggle_tab_size_cycles() {
        let mut app = make_app();
        app.default_tab_size = 2;
        app.settings_global.tab_size = Some(2);
        if let Some(ed) = app.editor_mut() {
            ed.tab_size = 2;
        }
        app.toggle_setting("tab_size");
        assert_eq!(app.default_tab_size, 4);
        assert_eq!(app.editor().unwrap().tab_size, 4);
        app.toggle_setting("tab_size");
        assert_eq!(app.default_tab_size, 8);
        assert_eq!(app.editor().unwrap().tab_size, 8);
        app.toggle_setting("tab_size");
        assert_eq!(app.default_tab_size, 2);
        assert_eq!(app.editor().unwrap().tab_size, 2);
    }

    #[test]
    fn toggle_whitespace_updates_default_and_editor() {
        let mut app = make_app();
        assert_eq!(
            app.default_whitespace_mode,
            crate::app::WhitespaceMode::Boundary
        );
        app.toggle_setting("whitespace");
        assert_eq!(
            app.default_whitespace_mode,
            crate::app::WhitespaceMode::Selection
        );
        assert_eq!(
            app.editor().unwrap().whitespace_mode,
            crate::app::WhitespaceMode::Selection
        );
    }

    #[test]
    fn toggle_word_wrap_updates_default_and_editor() {
        let mut app = make_app();
        assert!(!app.default_word_wrap);
        app.toggle_setting("word_wrap");
        assert!(app.default_word_wrap);
        assert!(app.editor().unwrap().word_wrap);
    }

    #[test]
    fn toggle_read_only_updates_default_and_editor() {
        let mut app = make_app();
        assert!(!app.default_read_only);
        app.toggle_setting("read_only");
        assert!(app.default_read_only);
        assert!(app.editor().unwrap().read_only);
    }

    #[test]
    fn toggle_minimap_width_cycles() {
        let mut app = make_app();
        app.minimap_width = 8;
        app.settings_global.minimap_width = Some(8);
        app.toggle_setting("minimap_width");
        assert_eq!(app.minimap_width, 10);
        app.toggle_setting("minimap_width");
        assert_eq!(app.minimap_width, 12);
        app.toggle_setting("minimap_width");
        assert_eq!(app.minimap_width, 15);
        app.toggle_setting("minimap_width");
        assert_eq!(app.minimap_width, 20);
        app.toggle_setting("minimap_width");
        assert_eq!(app.minimap_width, 8);
    }

    #[test]
    fn toggle_panel_height_cycles() {
        let mut app = make_app();
        app.panel_height = 6;
        app.settings_global.panel_height = Some(6);
        app.toggle_setting("panel_height");
        assert_eq!(app.panel_height, 10);
        app.toggle_setting("panel_height");
        assert_eq!(app.panel_height, 15);
        app.toggle_setting("panel_height");
        assert_eq!(app.panel_height, 20);
        app.toggle_setting("panel_height");
        assert_eq!(app.panel_height, 6);
    }

    #[test]
    fn toggle_sticky_scroll_max_cycles() {
        let mut app = make_app();
        app.sticky_scroll_max = 1;
        app.settings_global.sticky_scroll_max = Some(1);
        app.toggle_setting("sticky_scroll_max");
        assert_eq!(app.sticky_scroll_max, 2);
        app.toggle_setting("sticky_scroll_max");
        assert_eq!(app.sticky_scroll_max, 3);
    }

    #[test]
    fn toggle_cursor_surrounding_lines_updates_default_and_editors() {
        let mut app = make_app();
        app.default_cursor_surrounding_lines = 5;
        app.toggle_setting("cursor_surrounding_lines");
        assert_eq!(app.default_cursor_surrounding_lines, 10);
        assert_eq!(app.editor().unwrap().cursor_surrounding_lines, 10);
        assert_eq!(app.new_editor().cursor_surrounding_lines, 10);
    }

    #[test]
    fn toggle_rulers_cycles() {
        let mut app = make_app();
        app.rulers.clear();
        app.settings_global.rulers = Some(Vec::new());
        app.toggle_setting("rulers");
        assert_eq!(app.rulers, vec![80]);
        app.toggle_setting("rulers");
        assert_eq!(app.rulers, vec![80, 120]);
        app.toggle_setting("rulers");
        assert!(app.rulers.is_empty());
    }

    // --- setting_dropdown_options ---

    #[test]
    fn dropdown_options_sidebar_pos() {
        let app = make_app();
        let opts = app.setting_dropdown_options("sidebar_pos").unwrap();
        assert_eq!(opts, vec!["Left", "Right"]);
    }

    #[test]
    fn dropdown_options_rulers() {
        let app = make_app();
        let opts = app.setting_dropdown_options("rulers").unwrap();
        assert_eq!(opts.len(), 3);
        assert_eq!(opts[0], "None");
    }

    #[test]
    fn dropdown_options_cursor_style_and_auto_save() {
        let app = make_app();
        assert_eq!(
            app.setting_dropdown_options("cursor_style").unwrap(),
            vec!["Line", "Block", "Underline"]
        );
        assert_eq!(
            app.setting_dropdown_options("auto_save_mode").unwrap(),
            vec!["Off", "After Delay", "On Focus Change", "On Window Change"]
        );
    }

    #[test]
    fn dropdown_options_unknown() {
        let app = make_app();
        assert!(app.setting_dropdown_options("nonexistent").is_none());
    }

    // --- open/close_settings_dropdown ---

    #[test]
    fn open_settings_dropdown_sets_state() {
        let mut app = make_app();
        app.open_settings_dropdown("sidebar_pos", "Left");
        assert!(app.settings_dropdown_open);
        assert_eq!(
            app.settings_dropdown_setting_id.as_deref(),
            Some("sidebar_pos")
        );
        assert_eq!(app.settings_dropdown_options.len(), 2);
        assert_eq!(app.settings_dropdown_selected, 0);
    }

    #[test]
    fn open_settings_dropdown_selects_current() {
        let mut app = make_app();
        app.open_settings_dropdown("sidebar_pos", "Right");
        assert_eq!(app.settings_dropdown_selected, 1);
    }

    #[test]
    fn close_settings_dropdown_clears_state() {
        let mut app = make_app();
        app.open_settings_dropdown("sidebar_pos", "Left");
        app.settings_dropdown_committed = true;
        app.close_settings_dropdown();
        assert!(!app.settings_dropdown_open);
        assert!(app.settings_dropdown_setting_id.is_none());
        assert!(app.settings_dropdown_options.is_empty());
    }

    // --- apply_settings_dropdown_value ---

    #[test]
    fn comment_whitespace_dropdown_persists_overrides_and_inherit_by_scope() {
        let mut app = make_app();
        assert_eq!(App::settings_category_for_id("comment_whitespace"), 1);
        assert_eq!(App::setting_field_kind("comment_whitespace"), "dropdown");
        app.settings_global.comment_whitespace_mode = Some("none".into());
        app.settings_scope = crate::settings::SettingsScope::Workspace;
        app.apply_effective_settings();
        let options = app.setting_dropdown_options("comment_whitespace").unwrap();
        assert_eq!(options, ["Inherit", "None", "Boundary", "Selection", "Trailing", "All"]);
        for value in options {
            app.open_settings_dropdown("comment_whitespace", &value);
            assert_eq!(app.settings_dropdown_options[app.settings_dropdown_selected], value);
            app.apply_settings_dropdown_value("comment_whitespace", &value);
            app.close_settings_dropdown();
            assert_eq!(app.comment_whitespace_mode.map_or("Inherit", |mode| mode.name()), value);
            assert_eq!(app.current_settings().comment_whitespace_mode, Some(value.to_lowercase()));
            assert_eq!(app.settings_workspace.comment_whitespace_mode, Some(value.to_lowercase()));
            assert_eq!(app.default_whitespace_mode, crate::app::WhitespaceMode::Boundary);
            assert_eq!(app.settings_global.comment_whitespace_mode.as_deref(), Some("none"));
        }
        assert!(app.reset_setting_in_selected_scope("comment_whitespace"));
        assert_eq!(app.comment_whitespace_mode, Some(crate::app::WhitespaceMode::None));
        app.settings_scope = crate::settings::SettingsScope::Global;
        assert!(app.reset_setting_in_selected_scope("comment_whitespace"));
        assert_eq!(app.comment_whitespace_mode, Some(crate::app::WhitespaceMode::All));
    }

    #[test]
    fn whitespace_dropdown_selects_each_mode_and_updates_editors() {
        let mut app = make_app();
        app.settings_scope = crate::settings::SettingsScope::Workspace;
        app.settings_workspace.whitespace_mode = Some("all".into());
        app.apply_effective_settings();
        let options = app.setting_dropdown_options("whitespace").unwrap();
        assert_eq!(options, ["None", "Boundary", "Selection", "Trailing", "All"]);
        for value in options {
            app.open_settings_dropdown("whitespace", &value);
            assert_eq!(app.settings_dropdown_options[app.settings_dropdown_selected], value);
            app.apply_settings_dropdown_value("whitespace", &value);
            app.close_settings_dropdown();
            assert_eq!(app.default_whitespace_mode.name(), value);
            assert_eq!(app.tabs[0].editor.whitespace_mode.name(), value);
            assert_eq!(app.settings_workspace.whitespace_mode, Some(value.to_lowercase()));
        }
    }

    #[test]
    fn apply_sidebar_pos_right() {
        let mut app = make_app();
        app.settings_dropdown_committed = false;
        app.apply_settings_dropdown_value("sidebar_pos", "Right");
        assert!(app.sidebar_right);
        assert!(app.settings_dropdown_committed);
    }

    #[test]
    fn apply_rulers_80() {
        let mut app = make_app();
        app.apply_settings_dropdown_value("rulers", "80");
        assert_eq!(app.rulers, vec![80]);
    }

    #[test]
    fn apply_rulers_none() {
        let mut app = make_app();
        app.rulers = vec![80, 120];
        app.apply_settings_dropdown_value("rulers", "None");
        assert!(app.rulers.is_empty());
    }

    #[test]
    fn apply_cursor_style_and_auto_save_dropdowns() {
        let mut app = make_app();
        app.apply_settings_dropdown_value("cursor_style", "Underline");
        assert_eq!(app.cursor_style, crate::app::CursorStyle::Underline);

        app.apply_settings_dropdown_value("auto_save_mode", "On Window Change");
        assert_eq!(app.auto_save, crate::app::AutoSaveMode::OnWindowChange);
    }

    #[test]
    fn numeric_setting_accepts_an_arbitrary_typed_value() {
        let mut app = make_app();
        app.begin_numeric_setting_edit("tab_size", "4");
        app.settings_value_buffer = "7".to_string();
        app.settings_value_cursor = 1;

        assert!(app.commit_numeric_setting_edit());
        assert_eq!(app.settings_global.tab_size, Some(7));
        assert_eq!(app.default_tab_size, 7);
        assert!(!app.settings_value_editing);
    }

    #[test]
    fn editing_global_does_not_replace_a_workspace_override() {
        let mut app = make_app();
        app.settings_global.tab_size = Some(2);
        app.settings_workspace.tab_size = Some(8);
        app.apply_effective_settings();
        assert_eq!(app.default_tab_size, 8);

        app.settings_scope = crate::settings::SettingsScope::Global;
        app.begin_numeric_setting_edit("tab_size", "2");
        app.settings_value_buffer = "6".to_string();
        app.settings_value_cursor = 1;
        assert!(app.commit_numeric_setting_edit());

        assert_eq!(app.settings_global.tab_size, Some(6));
        assert_eq!(app.settings_workspace.tab_size, Some(8));
        assert_eq!(app.default_tab_size, 8);
        let global_value = app
            .settings_items()
            .into_iter()
            .find(|(_, _, id)| id == "tab_size")
            .map(|(_, value, _)| value)
            .unwrap();
        assert_eq!(global_value, "6");
    }

    #[test]
    fn workspace_numeric_edit_wins_and_reset_reveals_global_value() {
        let mut app = make_app();
        app.settings_global.tab_size = Some(3);
        app.settings_scope = crate::settings::SettingsScope::Workspace;
        app.begin_numeric_setting_edit("tab_size", "3");
        app.settings_value_buffer = "5".to_string();
        app.settings_value_cursor = 1;
        assert!(app.commit_numeric_setting_edit());
        assert_eq!(app.settings_workspace.tab_size, Some(5));
        assert_eq!(app.default_tab_size, 5);
        assert_eq!(app.setting_source_label("tab_size"), "Workspace");

        assert!(app.reset_setting_in_selected_scope("tab_size"));
        assert_eq!(app.settings_workspace.tab_size, None);
        assert_eq!(app.default_tab_size, 3);
        assert_eq!(app.setting_source_label("tab_size"), "Inherited · Global");
    }

    #[test]
    fn numeric_editor_clamps_to_the_documented_range() {
        let mut app = make_app();
        app.begin_numeric_setting_edit("scroll_speed", "3");
        app.settings_value_buffer = "999".to_string();
        app.settings_value_cursor = 3;
        assert!(app.commit_numeric_setting_edit());
        assert_eq!(app.settings_global.scroll_speed, Some(10));
        assert_eq!(app.scroll_speed, 10);
    }

    #[test]
    fn invalid_numeric_value_reverts_when_the_field_loses_focus() {
        let mut app = make_app();
        app.begin_numeric_setting_edit("tab_size", "4");
        app.settings_value_buffer.clear();
        app.settings_value_cursor = 0;

        app.finish_numeric_setting_edit();

        assert!(!app.settings_value_editing);
        assert_eq!(app.settings_global.tab_size, None);
        assert_eq!(app.default_tab_size, 4);
    }

    #[test]
    fn scoped_edits_write_only_the_selected_override_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = make_app();
        app.settings_persistence_enabled = true;
        app.settings_global_path = dir.path().join("base").join("settings.json");
        app.settings_workspace_path = dir
            .path()
            .join("workspace")
            .join(".vtcode")
            .join("settings.json");
        app.settings_workspace.tab_size = Some(8);
        app.settings_scope = crate::settings::SettingsScope::Global;

        app.begin_numeric_setting_edit("tab_size", "4");
        app.settings_value_buffer = "6".to_string();
        app.settings_value_cursor = 1;
        assert!(app.commit_numeric_setting_edit());

        let global = crate::settings::load_from_file(&app.settings_global_path);
        assert_eq!(global.tab_size, Some(6));
        assert!(!app.settings_workspace_path.exists());
        assert_eq!(app.settings_workspace.tab_size, Some(8));
        assert_eq!(app.default_tab_size, 8);
    }

    #[test]
    fn keybinding_scope_display_and_write_do_not_flatten_workspace_settings() {
        let mut app = make_app();
        app.settings_workspace.word_wrap = Some(true);
        app.settings_workspace.keybindings = Some(vec![crate::settings::KeybindingSetting {
            command: Some("save".into()),
            key: Some("Alt+S".into()),
            ..Default::default()
        }]);
        app.apply_effective_settings();

        app.settings_scope = crate::settings::SettingsScope::Global;
        let global_save = app
            .settings_items()
            .into_iter()
            .find(|(_, _, id)| id == "kb_0")
            .map(|(_, value, _)| value)
            .unwrap();
        assert_eq!(global_save, "Ctrl+S");

        let before = app.selected_scope_settings();
        app.apply_resolved_settings(&before);
        app.keybindings[0].modifiers = crossterm::event::KeyModifiers::SHIFT;
        app.keybindings[0].code = crossterm::event::KeyCode::F(6);
        app.persist_scoped_setting_change("kb_0", &before);

        assert_eq!(app.settings_global.word_wrap, None);
        assert_eq!(app.settings_workspace.word_wrap, Some(true));
        assert_eq!(app.settings_global.keybindings.as_ref().unwrap()[0].key.as_deref(), Some("Shift+F6"));
        assert!(app.default_word_wrap);

        app.settings_scope = crate::settings::SettingsScope::Workspace;
        let workspace_save = app
            .settings_items()
            .into_iter()
            .find(|(_, _, id)| id == "kb_0")
            .map(|(_, value, _)| value)
            .unwrap();
        assert_eq!(workspace_save, "Alt+S");
    }

    #[test]
    fn clearing_the_last_keybinding_override_restores_builtin_bindings() {
        let mut app = make_app();
        app.settings_global.keybindings = Some(vec![crate::settings::KeybindingSetting {
            command: Some("save".into()),
            key: Some("Alt+S".into()),
            ..Default::default()
        }]);
        app.apply_effective_settings();
        assert_eq!(app.keybindings[0].display(), "Alt+S");

        app.settings_global.keybindings = None;
        app.apply_effective_settings();
        assert_eq!(app.keybindings[0].display(), "Ctrl+S");
    }

    #[test]
    fn workspace_keybinding_edits_store_only_the_workspace_delta() {
        let mut app = make_app();
        app.settings_global.keybindings = Some(vec![crate::settings::KeybindingSetting {
            command: Some("save".into()),
            key: Some("Alt+S".into()),
            ..Default::default()
        }]);
        app.settings_scope = crate::settings::SettingsScope::Workspace;
        app.apply_effective_settings();
        let copy_index = app
            .keybindings
            .iter()
            .position(|binding| binding.id == "copy")
            .unwrap();
        let before = app.selected_scope_settings();
        app.apply_resolved_settings(&before);
        app.keybindings[copy_index].modifiers = crossterm::event::KeyModifiers::SHIFT;
        app.keybindings[copy_index].code = crossterm::event::KeyCode::F(7);

        app.persist_scoped_setting_change(&format!("kb_{copy_index}"), &before);

        let workspace = app.settings_workspace.keybindings.as_ref().unwrap();
        assert_eq!(workspace.len(), 1);
        assert_eq!(workspace[0].command.as_deref(), Some("copy"));
        assert_eq!(workspace[0].key.as_deref(), Some("Shift+F7"));
        assert_eq!(app.keybindings[0].display(), "Alt+S");
        assert_eq!(app.keybindings[copy_index].display(), "Shift+F7");
        assert_eq!(app.setting_source_label("kb_0"), "Inherited · Global");
        assert_eq!(
            app.setting_source_label(&format!("kb_{copy_index}")),
            "Workspace"
        );
    }

    #[test]
    fn resetting_a_workspace_keybinding_reveals_the_global_binding() {
        let mut app = make_app();
        app.settings_global.keybindings = Some(vec![crate::settings::KeybindingSetting {
            command: Some("save".into()),
            key: Some("Alt+S".into()),
            ..Default::default()
        }]);
        app.settings_workspace.keybindings = Some(vec![crate::settings::KeybindingSetting {
            command: Some("save".into()),
            key: Some("Shift+F6".into()),
            ..Default::default()
        }]);
        app.settings_scope = crate::settings::SettingsScope::Workspace;
        app.apply_effective_settings();
        assert_eq!(app.keybindings[0].display(), "Shift+F6");

        assert!(app.reset_keybinding_in_selected_scope(0));

        assert_eq!(app.settings_workspace.keybindings, None);
        assert_eq!(app.keybindings[0].display(), "Alt+S");
        assert_eq!(app.setting_source_label("kb_0"), "Inherited · Global");
    }

    // --- heading and unknown IDs ---

    #[test]
    fn toggle_heading_is_noop() {
        let mut app = make_app();
        let before_minimap = app.show_minimap;
        app.toggle_setting("heading_appearance");
        assert_eq!(app.show_minimap, before_minimap);
    }
}
