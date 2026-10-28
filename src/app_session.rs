use crate::app::{
    ActivePane, App, AutoSaveMode, CursorStyle, EditorGroup, SidebarView, Tab, TabKind,
};
use crate::editor::Editor;
use crate::settings::{KeybindingSetting, Settings};
use crossterm::event::{KeyCode, KeyModifiers};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SESSION_RESTORE_ID: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
#[path = "app_settings_persistence_tests.rs"]
mod persistence_tests;

impl App {
    pub(crate) fn keycode_from_settings_token(token: &str) -> Option<KeyCode> {
        if let Some(raw) = token.strip_prefix("char:") {
            let codepoint = raw.parse::<u32>().ok()?;
            return char::from_u32(codepoint).map(KeyCode::Char);
        }
        if let Some(raw) = token.strip_prefix("f:") {
            return raw.parse::<u8>().ok().map(KeyCode::F);
        }
        match token {
            "backspace" => Some(KeyCode::Backspace),
            "enter" => Some(KeyCode::Enter),
            "left" => Some(KeyCode::Left),
            "right" => Some(KeyCode::Right),
            "up" => Some(KeyCode::Up),
            "down" => Some(KeyCode::Down),
            "home" => Some(KeyCode::Home),
            "end" => Some(KeyCode::End),
            "page_up" => Some(KeyCode::PageUp),
            "page_down" => Some(KeyCode::PageDown),
            "tab" => Some(KeyCode::Tab),
            "back_tab" => Some(KeyCode::BackTab),
            "delete" => Some(KeyCode::Delete),
            "insert" => Some(KeyCode::Insert),
            "esc" => Some(KeyCode::Esc),
            "null" => Some(KeyCode::Null),
            _ => None,
        }
    }

    pub(crate) fn keybinding_from_settings_text(value: &str) -> Option<(KeyModifiers, KeyCode)> {
        let text = value.trim();
        if text.is_empty() {
            return None;
        }
        if text.eq_ignore_ascii_case("unassigned") {
            return Some((KeyModifiers::empty(), KeyCode::Null));
        }

        let parts: Vec<&str> = text
            .split('+')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .collect();
        if parts.is_empty() {
            return None;
        }

        let mut modifiers = KeyModifiers::empty();
        for part in &parts[..parts.len().saturating_sub(1)] {
            if part.eq_ignore_ascii_case("ctrl") || part.eq_ignore_ascii_case("control") {
                modifiers |= KeyModifiers::CONTROL;
            } else if part.eq_ignore_ascii_case("alt") {
                modifiers |= KeyModifiers::ALT;
            } else if part.eq_ignore_ascii_case("shift") {
                modifiers |= KeyModifiers::SHIFT;
            } else {
                return None;
            }
        }

        let key_part = parts[parts.len() - 1];
        let lower = key_part.to_ascii_lowercase();
        let code = match lower.as_str() {
            "backspace" => KeyCode::Backspace,
            "enter" => KeyCode::Enter,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "page_up" => KeyCode::PageUp,
            "pagedown" | "page_down" => KeyCode::PageDown,
            "tab" => KeyCode::Tab,
            "backtab" | "back_tab" => KeyCode::BackTab,
            "delete" => KeyCode::Delete,
            "insert" => KeyCode::Insert,
            "esc" | "escape" => KeyCode::Esc,
            _ => {
                if let Some(num) = lower.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                    KeyCode::F(num)
                } else if key_part.chars().count() == 1 {
                    let mut chars = key_part.chars();
                    let ch = chars.next()?;
                    KeyCode::Char(ch.to_ascii_lowercase())
                } else {
                    return None;
                }
            }
        };
        Some((modifiers, code))
    }

    // Settings UI display methods moved to app_settings_ui.rs

    /// Build a Settings struct from current app state.
    /// When zen mode is active, saves the pre-zen visibility state
    /// so launching again won't persist zen mode's hidden UI.
    pub(crate) fn current_settings(&self) -> Settings {
        // Use pre-zen state for visibility flags to avoid persisting zen mode overrides
        let (
            show_explorer,
            show_minimap,
            show_panel,
            show_tab_bar,
            show_status_bar,
            show_activity_bar,
        ) = if self.zen_mode {
            let (
                show_explorer,
                show_minimap,
                show_panel,
                show_tab_bar,
                show_status_bar,
                show_activity_bar,
            ) = self
                .zen_pre_state
                .unwrap_or((true, true, false, true, true, true));
            (
                show_explorer,
                show_minimap,
                show_panel,
                show_tab_bar,
                show_status_bar,
                show_activity_bar,
            )
        } else {
            (
                self.show_explorer,
                self.show_minimap,
                self.show_panel,
                self.show_tab_bar,
                self.show_status_bar,
                self.show_activity_bar,
            )
        };
        let tab_size = self.default_tab_size as u8;
        let use_tabs = self.default_use_tabs;
        let word_wrap = self.default_word_wrap;
        let whitespace_mode_str = self.default_whitespace_mode.name().to_lowercase();
        let theme_name = self.committed_theme_key().to_string();
        let keybindings = {
            let overrides: Vec<KeybindingSetting> = self
                .keybindings
                .iter()
                .filter_map(|kb| {
                    if !kb.is_modified() {
                        return None;
                    }
                    Some(KeybindingSetting {
                        command: Some(kb.id.to_string()),
                        key: Some(kb.display()),
                        index: None,
                        modifiers: None,
                        key_code: None,
                    })
                })
                .collect();
            if overrides.is_empty() {
                None
            } else {
                Some(overrides)
            }
        };
        Settings {
            acp_agents: self.settings_effective.acp_agents.clone(),
            lsp_enabled: Some(self.lsp_enabled),
            source_control_enabled: Some(self.source_control_enabled),
            source_control_show_untracked: Some(self.sc_visibility.untracked),
            lsp_diagnostic_delay_ms: Some(self.lsp_manager.diagnostic_publication.delay_ms()),
            lsp_servers: self.settings_effective.lsp_servers.clone(),
            chat_output_detail: Some(self.chat.output_detail.label().to_lowercase()),
            chat_thinking_expanded: Some(self.chat.thinking_expanded),
            tab_size: Some(tab_size),
            use_tabs: Some(use_tabs),
            word_wrap: Some(word_wrap),
            word_wrap_column: Some(self.word_wrap_column),
            whitespace_mode: Some(whitespace_mode_str),
            comment_whitespace_mode: Some(self.comment_whitespace_mode.map_or("inherit", |mode| mode.name()).to_lowercase()),
            read_only: Some(self.default_read_only),
            diff_side_by_side: Some(self.preferred_diff_mode == crate::app::DiffMode::SideBySide),
            theme: Some(theme_name),
            workbench_color_customizations: self.settings_effective.workbench_color_customizations.clone(),
            show_explorer: Some(show_explorer),
            show_open_editors: Some(self.show_open_editors),
            show_minimap: Some(show_minimap),
            minimap_width: Some(self.minimap_width),
            show_line_numbers: Some(self.show_line_numbers),
            relative_line_numbers: Some(self.relative_line_numbers),
            show_indent_guides: Some(self.show_indent_guides),
            show_panel: Some(show_panel),
            panel_height: Some(self.panel_height),
            sidebar_right: Some(self.sidebar_right),
            explorer_width: Some(self.explorer_width),
            show_activity_bar: Some(show_activity_bar),
            show_breadcrumbs: Some(self.show_breadcrumbs),
            show_status_bar: Some(show_status_bar),
            status_bar_items: self.status_bar_pins.clone(),
            show_tab_bar: Some(show_tab_bar),
            enable_preview_tabs: Some(self.enable_preview_tabs),
            mouse_back_forward_to_navigate: Some(self.mouse_back_forward_to_navigate),
            show_menu_bar: Some(self.show_menu_bar),
            zen_mode: Some(self.zen_mode),
            indent_rainbow: Some(self.indent_rainbow),
            color_preview: Some(self.show_color_preview),
            bracket_pair_colorization: Some(self.bracket_pair_colorization),
            error_lens: Some(self.error_lens),
            hover_enabled: Some(self.hover_enabled),
            quick_suggestions: Some(self.quick_suggestions),
            inlay_hints: Some(self.show_inlay_hints),
            code_lens: Some(self.show_code_lenses),
            semantic_highlighting: Some(self.show_semantic_tokens),
            format_on_type: Some(self.on_type_formatting_enabled),
            sticky_scroll: Some(self.sticky_scroll),
            sticky_scroll_max: Some(self.sticky_scroll_max),
            scroll_past_end: Some(self.scroll_past_end),
            focus_mode: Some(self.focus_mode),
            show_word_count: Some(self.show_word_count),
            screencast_mode: Some(self.screencast_mode),
            max_global_search_results: Some(self.max_global_search_results),
            large_file_threshold_mb: Some(self.large_file_threshold_mb),
            large_file_analysis_timeout_ms: Some(self.large_file_analysis_timeout_ms),
            rulers: Some(self.rulers.clone()),
            trim_trailing_on_save: Some(self.trim_trailing_on_save),
            format_on_save: Some(self.format_on_save),
            formatters: Some(self.formatters.clone()),
            insert_final_newline: Some(self.insert_final_newline),
            cursor_style: Some(match self.cursor_style {
                CursorStyle::Line => "line".to_string(),
                CursorStyle::Block => "block".to_string(),
                CursorStyle::Underline => "underline".to_string(),
            }),
            cursor_blinking: Some(self.cursor_blinking),
            auto_save: Some(match self.auto_save {
                AutoSaveMode::Off => "off".to_string(),
                AutoSaveMode::AfterDelay => "afterDelay".to_string(),
                AutoSaveMode::OnFocusChange => "onFocusChange".to_string(),
                AutoSaveMode::OnWindowChange => "onWindowChange".to_string(),
            }),
            auto_save_delay: Some(self.auto_save_delay),
            animation_cursor_smear: Some(self.animation_settings.enabled(crate::animation_config::AnimationEffect::CursorSmear)),
            animation_copy: Some(self.copy_animation.enabled),
            animation_copy_duration_ms: Some(self.copy_animation.duration_ms),
            animation_cursor_fire: Some(self.animation_settings.enabled(crate::animation_config::AnimationEffect::CursorFire)),
            animation_whitespace_reveal: Some(self.animation_settings.enabled(crate::animation_config::AnimationEffect::WhitespaceReveal)),
            animation_scope_connector: Some(self.animation_settings.enabled(crate::animation_config::AnimationEffect::ScopeConnector)),
            animation_tab_slide: Some(self.animation_settings.enabled(crate::animation_config::AnimationEffect::TabSlide)),
            animation_duration_ms: Some(self.animation_settings.duration_ms),
            animation_fps: Some(self.animation_settings.fps),
            animation_intensity: Some(self.animation_settings.intensity),
            animation_cursor_smear_duration_ms: Some(
                self.animation_settings
                    .duration(crate::animation_config::AnimationEffect::CursorSmear),
            ),
            animation_cursor_smear_intensity: Some(
                self.animation_settings
                    .intensity(crate::animation_config::AnimationEffect::CursorSmear),
            ),
            animation_cursor_fire_duration_ms: Some(
                self.animation_settings
                    .duration(crate::animation_config::AnimationEffect::CursorFire),
            ),
            animation_cursor_fire_intensity: Some(
                self.animation_settings
                    .intensity(crate::animation_config::AnimationEffect::CursorFire),
            ),
            animation_whitespace_reveal_duration_ms: Some(
                self.animation_settings
                    .duration(crate::animation_config::AnimationEffect::WhitespaceReveal),
            ),
            animation_whitespace_reveal_intensity: Some(
                self.animation_settings
                    .intensity(crate::animation_config::AnimationEffect::WhitespaceReveal),
            ),
            animation_scope_bubble_duration_ms: Some(
                self.animation_settings
                    .duration(crate::animation_config::AnimationEffect::ScopeConnector),
            ),
            animation_scope_bubble_intensity: Some(
                self.animation_settings
                    .intensity(crate::animation_config::AnimationEffect::ScopeConnector),
            ),
            animation_tab_slide_duration_ms: Some(
                self.animation_settings
                    .duration(crate::animation_config::AnimationEffect::TabSlide),
            ),
            animation_tab_slide_intensity: Some(
                self.animation_settings
                    .intensity(crate::animation_config::AnimationEffect::TabSlide),
            ),
            animation_scope_bubble_tight: Some(self.animation_settings.scope_tight),
            animation_scope_bubble_fill: Some(self.animation_settings.scope_fill),
            animation_scope_bubble_languages: Some(self.animation_settings.scope_languages.configs.clone()),
            animation_scope_bubble_disabled_languages: Some(
                self.animation_settings.scope_disabled_languages.clone(),
            ),
            animation_scope_bubble_glow: None,
            animation_scope_bubble_glow_duration_ms: None,
            scroll_speed: Some(self.scroll_speed),
            cursor_surrounding_lines: Some(self.default_cursor_surrounding_lines),
            keybindings,
            search_case_sensitive: Some(self.search_case_sensitive),
            search_regex: Some(self.search_regex),
            search_whole_word: Some(self.search_whole_word),
            search_preserve_case: Some(self.search_preserve_case),
            search_debounce_ms: Some(self.search_debounce_ms),
            search_use_ignore_files: Some(self.settings_effective.search_use_ignore_files.unwrap_or(true)),
            search_use_global_ignore_files: Some(self.settings_effective.search_use_global_ignore_files.unwrap_or(false)),
            search_use_parent_ignore_files: Some(self.settings_effective.search_use_parent_ignore_files.unwrap_or(false)),
            search_follow_symlinks: Some(self.settings_effective.search_follow_symlinks.unwrap_or(true)),
            files_exclude: self.settings_effective.files_exclude.clone(),
            search_exclude: self.settings_effective.search_exclude.clone(),
            language_associations: if self.language_associations.is_empty() {
                None
            } else {
                Some(
                    self.language_associations
                        .iter()
                        .map(|(k, v)| (k.clone(), v.clone()))
                        .collect(),
                )
            },
        }
    }

    /// Apply a Settings struct to the app
    pub(crate) fn apply_settings(&mut self, s: &Settings) {
        if let Some(side_by_side) = s.diff_side_by_side {
            self.preferred_diff_mode = if side_by_side { crate::app::DiffMode::SideBySide } else { crate::app::DiffMode::Inline };
        }
        if let Some(enabled) = s.source_control_enabled {
            self.set_source_control_enabled(enabled);
        }
        if let Some(untracked) = s.source_control_show_untracked {
            self.set_source_control_visibility(crate::source_control_rows::Visibility { untracked });
        }
        if let Some(enabled) = s.lsp_enabled { self.lsp_enabled = enabled; }
        if let Some(delay) = s.lsp_diagnostic_delay_ms {
            self.lsp_manager.diagnostic_publication.set_delay_ms(delay);
        }
        self.animation_settings.apply(s);
        if let Some(enabled) = s.animation_copy {
            self.copy_animation.enabled = enabled;
            if !enabled { self.copy_animation.clear(); }
        }
        if let Some(duration) = s.animation_copy_duration_ms {
            self.copy_animation.duration_ms = crate::animation_config::clamp_animation_duration_ms(duration);
        }
        // Share compiled profiles across all views/desks. Preserve identity on
        // unrelated settings changes so cached scope analysis remains reusable.
        let rules = self.animation_settings.scope_languages.clone();
        for tab in self.tabs.iter_mut().chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs)) {
            if tab.editor.scope_bubble_rules != rules { tab.editor.scope_bubble_rules = rules.clone(); }
        }
        if let Some(detail) = &s.chat_output_detail {
            self.chat.output_detail = crate::app_acp::OutputDetail::parse(detail);
        }
        if let Some(expanded) = s.chat_thinking_expanded {
            self.chat.thinking_expanded = expanded;
        }
        self.settings_effective.workbench_color_customizations = s.workbench_color_customizations.clone();
        let requested_zen_mode = s.zen_mode;
        if requested_zen_mode == Some(false) && self.zen_mode {
            self.execute_view_command("toggle_zen");
        }
        if let Some(v) = s.tab_size {
            let v = v as usize;
            self.default_tab_size = v;
            for tab in &mut self.tabs {
                if tab.kind == TabKind::File {
                    tab.editor.tab_size = v;
                }
            }
            for group in &mut self.groups {
                for tab in &mut group.tabs {
                    if tab.kind == TabKind::File {
                        tab.editor.tab_size = v;
                    }
                }
            }
        }
        if let Some(v) = s.use_tabs {
            self.default_use_tabs = v;
            for tab in &mut self.tabs {
                if tab.kind == TabKind::File {
                    tab.editor.use_tabs = v;
                }
            }
            for group in &mut self.groups {
                for tab in &mut group.tabs {
                    if tab.kind == TabKind::File {
                        tab.editor.use_tabs = v;
                    }
                }
            }
        }
        if let Some(v) = s.word_wrap {
            self.default_word_wrap = v;
            for tab in &mut self.tabs {
                if tab.kind == TabKind::File {
                    tab.editor.word_wrap = v;
                }
            }
            for group in &mut self.groups {
                for tab in &mut group.tabs {
                    if tab.kind == TabKind::File {
                        tab.editor.word_wrap = v;
                    }
                }
            }
        }
        if let Some(v) = s.word_wrap_column {
            self.word_wrap_column = v;
            for tab in &mut self.tabs {
                if tab.kind == TabKind::File {
                    tab.editor.word_wrap_column = v;
                }
            }
            for group in &mut self.groups {
                for tab in &mut group.tabs {
                    if tab.kind == TabKind::File {
                        tab.editor.word_wrap_column = v;
                    }
                }
            }
        }
        if let Some(ref v) = s.whitespace_mode {
            let mode = match v.as_str() {
                "boundary" => crate::app::WhitespaceMode::Boundary,
                "selection" => crate::app::WhitespaceMode::Selection,
                "trailing" => crate::app::WhitespaceMode::Trailing,
                "all" => crate::app::WhitespaceMode::All,
                _ => crate::app::WhitespaceMode::None,
            };
            self.default_whitespace_mode = mode;
            for tab in &mut self.tabs {
                if tab.kind == TabKind::File {
                    tab.editor.whitespace_mode = mode;
                }
            }
            for group in &mut self.groups {
                for tab in &mut group.tabs {
                    if tab.kind == TabKind::File {
                        tab.editor.whitespace_mode = mode;
                    }
                }
            }
        }
        if let Some(ref v) = s.comment_whitespace_mode {
            self.comment_whitespace_mode = match v.as_str() {
                "none" => Some(crate::app::WhitespaceMode::None),
                "boundary" => Some(crate::app::WhitespaceMode::Boundary),
                "selection" => Some(crate::app::WhitespaceMode::Selection),
                "trailing" => Some(crate::app::WhitespaceMode::Trailing),
                "all" => Some(crate::app::WhitespaceMode::All),
                _ => None,
            };
        }
        if let Some(v) = s.read_only {
            self.default_read_only = v;
            for tab in &mut self.tabs {
                if tab.kind == TabKind::File {
                    tab.editor.set_read_only(v);
                }
            }
            for group in &mut self.groups {
                for tab in &mut group.tabs {
                    if tab.kind == TabKind::File {
                        tab.editor.set_read_only(v);
                    }
                }
            }
        }
        {
            let name = s.theme.as_deref().unwrap_or(&self.theme_key).to_string();
            if !self.set_theme_by_key(&name) {
                // Fallback to Dark if theme not found
                self.set_theme_by_key("dark");
            }
        }
        if let Some(v) = s.show_explorer {
            self.show_explorer = v;
        }
        if let Some(v) = s.show_open_editors {
            self.show_open_editors = v;
        }
        if let Some(v) = s.show_minimap {
            self.show_minimap = v;
        }
        if let Some(v) = s.minimap_width {
            self.minimap_width = v.max(5);
        }
        if let Some(v) = s.show_line_numbers {
            self.show_line_numbers = v;
        }
        if let Some(v) = s.relative_line_numbers {
            self.relative_line_numbers = v;
        }
        if let Some(v) = s.show_indent_guides {
            self.show_indent_guides = v;
        }
        if let Some(v) = s.show_panel {
            self.show_panel = v;
        }
        if let Some(v) = s.panel_height {
            self.panel_height = v;
        }
        if let Some(v) = s.sidebar_right {
            self.sidebar_right = v;
        }
        if let Some(v) = s.explorer_width {
            self.explorer_width = v;
        }
        if let Some(v) = s.show_activity_bar {
            if self.zen_mode {
                let mut state = self
                    .zen_pre_state
                    .unwrap_or((true, true, false, true, true, true));
                state.5 = v;
                self.zen_pre_state = Some(state);
                self.show_activity_bar = false;
            } else {
                self.show_activity_bar = v;
            }
        }
        if let Some(v) = s.show_breadcrumbs {
            self.show_breadcrumbs = v;
        }
        if let Some(v) = s.show_status_bar {
            self.show_status_bar = v;
        }
        self.status_bar_pins = s.status_bar_items.clone();
        if let Some(v) = s.show_tab_bar {
            self.show_tab_bar = v;
        }
        if let Some(v) = s.enable_preview_tabs {
            self.set_preview_tabs_enabled(v);
        }
        if let Some(v) = s.mouse_back_forward_to_navigate {
            self.mouse_back_forward_to_navigate = v;
        }
        if let Some(v) = s.show_menu_bar {
            self.show_menu_bar = v;
        }
        if let Some(v) = s.indent_rainbow {
            self.indent_rainbow = v;
        }
        if let Some(v) = s.color_preview {
            self.show_color_preview = v;
        }
        if let Some(v) = s.bracket_pair_colorization {
            self.bracket_pair_colorization = v;
        }
        if let Some(v) = s.error_lens {
            self.error_lens = v;
        }
        if let Some(v) = s.hover_enabled {
            self.hover_enabled = v;
            if !v {
                self.dismiss_lsp_hover();
            }
        }
        if let Some(v) = s.quick_suggestions {
            self.quick_suggestions = v;
            if !v {
                self.dismiss_vtcomplete();
                self.lsp_completion_active = false;
            }
        }
        if let Some(v) = s.inlay_hints {
            self.show_inlay_hints = v;
            if !v {
                self.lsp_inlay_hints.clear();
            }
            self.lsp_inlay_hint_last_scroll = usize::MAX;
        }
        if let Some(v) = s.code_lens {
            self.show_code_lenses = v;
            if !v {
                self.lsp_code_lenses.clear();
            }
            self.lsp_code_lens_last_version = u64::MAX;
        }
        if let Some(v) = s.semantic_highlighting {
            self.show_semantic_tokens = v;
            if !v {
                self.lsp_semantic_tokens.clear();
            }
            self.lsp_semantic_tokens_last_version = u64::MAX;
        }
        if let Some(v) = s.format_on_type {
            self.on_type_formatting_enabled = v;
        }
        if let Some(v) = s.sticky_scroll {
            self.sticky_scroll = v;
        }
        if let Some(v) = s.sticky_scroll_max {
            self.sticky_scroll_max = v.clamp(1, 5);
        }
        if let Some(v) = s.scroll_past_end {
            self.scroll_past_end = v;
        }
        if let Some(v) = s.focus_mode {
            self.focus_mode = v;
        }
        if let Some(v) = s.show_word_count {
            self.show_word_count = v;
        }
        if let Some(&pinned) = s.status_bar_items.get("word_count") {
            self.show_word_count = pinned;
        }
        if let Some(v) = s.screencast_mode {
            self.screencast_mode = v;
            if !v {
                self.screencast_keys.clear();
            }
        }
        if let Some(v) = s.max_global_search_results {
            self.max_global_search_results = v.max(1);
        }
        if let Some(v) = s.large_file_threshold_mb {
            self.large_file_threshold_mb = v.clamp(1, 16_384);
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
        }
        if let Some(v) = s.large_file_analysis_timeout_ms {
            self.large_file_analysis_timeout_ms = v.clamp(100, 60_000);
        }
        if let Some(ref v) = s.rulers {
            self.rulers = v.clone();
        }
        if let Some(v) = s.trim_trailing_on_save {
            self.trim_trailing_on_save = v;
        }
        if let Some(v) = s.format_on_save {
            self.format_on_save = v;
        }
        if let Some(configs) = &s.formatters {
            self.formatters = configs.clone();
        }
        if let Some(v) = s.insert_final_newline {
            self.insert_final_newline = v;
        }
        if let Some(ref v) = s.cursor_style {
            self.cursor_style = match v.as_str() {
                "block" => CursorStyle::Block,
                "underline" => CursorStyle::Underline,
                _ => CursorStyle::Line,
            };
        }
        if let Some(v) = s.cursor_blinking {
            self.cursor_blinking = v;
        }
        if let Some(ref v) = s.auto_save {
            self.auto_save = match v.as_str() {
                "afterDelay" => AutoSaveMode::AfterDelay,
                "onFocusChange" => AutoSaveMode::OnFocusChange,
                "onWindowChange" => AutoSaveMode::OnWindowChange,
                _ => AutoSaveMode::Off,
            };
        }
        if let Some(v) = s.auto_save_delay {
            self.auto_save_delay = v.max(100);
        }
        if let Some(v) = s.scroll_speed {
            self.scroll_speed = v.clamp(1, 10);
        }
        if let Some(ref v) = s.language_associations {
            let fx: rustc_hash::FxHashMap<String, String> =
                v.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
            self.language_associations = fx.clone();
            // Propagate to all open editors
            for tab in &mut self.tabs {
                tab.editor.language_associations = fx.clone();
            }
            for group in &mut self.groups {
                for tab in &mut group.tabs {
                    tab.editor.language_associations = fx.clone();
                }
            }
        }
        if let Some(v) = s.cursor_surrounding_lines {
            self.default_cursor_surrounding_lines = v;
            for tab in &mut self.tabs {
                tab.editor.cursor_surrounding_lines = v;
            }
            for group in &mut self.groups {
                for tab in &mut group.tabs {
                    tab.editor.cursor_surrounding_lines = v;
                }
            }
        }
        if let Some(custom_keybindings) = &s.keybindings {
            for kb in &mut self.keybindings {
                kb.reset();
            }
            for binding in custom_keybindings {
                if let (Some(command), Some(key)) =
                    (binding.command.as_deref(), binding.key.as_deref())
                {
                    if let Some((modifiers, code)) = Self::keybinding_from_settings_text(key) {
                        if let Some(kb) = self.keybindings.iter_mut().find(|kb| kb.id == command) {
                            kb.modifiers = modifiers;
                            kb.code = code;
                            continue;
                        }
                    }
                }
                if let (Some(index), Some(modifiers), Some(key_code)) = (
                    binding.index,
                    binding.modifiers,
                    binding.key_code.as_deref(),
                ) {
                    if let Some(code) = Self::keycode_from_settings_token(key_code) {
                        if let Some(kb) = self.keybindings.get_mut(index) {
                            kb.modifiers = KeyModifiers::from_bits_truncate(modifiers);
                            kb.code = code;
                        }
                    }
                }
            }
        }
        if let Some(v) = s.search_case_sensitive {
            self.search_case_sensitive = v;
        }
        if let Some(v) = s.search_regex {
            self.search_regex = v;
        }
        if let Some(v) = s.search_whole_word {
            self.search_whole_word = v;
        }
        if let Some(v) = s.search_preserve_case {
            self.search_preserve_case = v;
        }
        if let Some(v) = s.search_debounce_ms {
            self.search_debounce_ms = v;
        }
        if self.zen_mode {
            let mut state = self.zen_pre_state.unwrap_or((true, true, false, true, true, true));
            if let Some(value) = s.show_explorer { state.0 = value; }
            if let Some(value) = s.show_minimap { state.1 = value; }
            if let Some(value) = s.show_panel { state.2 = value; }
            if let Some(value) = s.show_tab_bar { state.3 = value; }
            if let Some(value) = s.show_status_bar { state.4 = value; }
            if let Some(value) = s.show_activity_bar { state.5 = value; }
            self.zen_pre_state = Some(state);
            self.show_explorer = false;
            self.show_minimap = false;
            self.show_panel = false;
            self.show_tab_bar = false;
            self.show_status_bar = false;
            self.show_activity_bar = false;
        }
        if requested_zen_mode == Some(true) && !self.zen_mode {
            self.execute_view_command("toggle_zen");
        }
        self.rebuild_keybinding_settings_cache();
    }

    /// Load the global and workspace override files, then apply the complete
    /// `defaults < global < workspace` stack. Missing files stay missing until
    /// the user actually changes that scope, keeping startup read-only and
    /// avoiding a blocking workspace write merely to show the first frame.
    pub(crate) fn initialize_settings(&mut self) {
        self.settings_global_path = crate::settings::global_settings_path();
        self.settings_workspace_path =
            crate::settings::project_settings_path(&self.explorer.root);

        if !self.settings_persistence_enabled {
            self.apply_effective_settings();
            return;
        }

        self.settings_global = crate::settings::load_from_file(&self.settings_global_path);
        self.settings_workspace = crate::settings::load_from_file(&self.settings_workspace_path);
        self.settings_global.validate();
        self.settings_workspace.validate();
        self.apply_effective_settings();
    }

    pub(crate) fn settings_for_scope(&self, scope: crate::settings::SettingsScope) -> Settings {
        // Explicit legacy timing still provides the per-effect fallback.
        // Otherwise retain Cursor Smear's separate built-in duration.
        let mut defaults = self.settings_defaults.clone();
        if self.settings_global.animation_duration_ms.is_some()
            || (scope == crate::settings::SettingsScope::Workspace
                && self.settings_workspace.animation_duration_ms.is_some())
        {
            defaults.animation_cursor_smear_duration_ms = None;
        }
        defaults.animation_cursor_smear_intensity = None;
        defaults.animation_cursor_fire_duration_ms = None;
        defaults.animation_cursor_fire_intensity = None;
        defaults.animation_whitespace_reveal_duration_ms = None;
        defaults.animation_whitespace_reveal_intensity = None;
        defaults.animation_scope_bubble_duration_ms = None;
        defaults.animation_scope_bubble_intensity = None;
        defaults.animation_tab_slide_duration_ms = None;
        defaults.animation_tab_slide_intensity = None;
        let global = defaults.merge(&self.settings_global);
        match scope {
            crate::settings::SettingsScope::Global => global,
            crate::settings::SettingsScope::Workspace => global.merge(&self.settings_workspace),
        }
    }

    pub(crate) fn selected_scope_settings(&self) -> Settings {
        self.settings_for_scope(self.settings_scope)
    }

    /// Read/edit one control in its selected scope without temporarily applying
    /// that scope's unrelated values over workspace or document preferences.
    pub(crate) fn apply_setting_value(&mut self, id: &str, settings: &Settings) {
        let Some(key) = Self::setting_json_key(id) else { return; };
        if key == "keybindings" {
            self.keybindings = Self::keybindings_for_settings(settings);
            self.rebuild_keybinding_settings_cache();
            return;
        }
        let mut patch = Settings {
            workbench_color_customizations: self.settings_effective.workbench_color_customizations.clone(),
            status_bar_items: self.status_bar_pins.clone(),
            ..Default::default()
        };
        if Self::copy_json_setting_field(&mut patch, settings, key) {
            self.apply_settings(&patch);
        }
    }

    pub(crate) fn apply_effective_settings(&mut self) {
        let mut effective = self.settings_for_scope(crate::settings::SettingsScope::Workspace);
        effective.validate();
        let files_changed = effective.files_exclude != self.settings_effective.files_exclude;
        let search_changed = files_changed || effective.search_exclude != self.settings_effective.search_exclude;
        self.apply_resolved_settings(&effective);
        self.settings_effective = effective;
        if files_changed {
            self.refresh_explorer_tree_async();
        }
        if search_changed {
            self.workspace_file_index_generation = self.workspace_file_index_generation.wrapping_add(1);
            if self.quick_open_active { self.update_quick_open(); }
            if !self.global_search_query.is_empty() { self.update_global_search(); }
        }
        self.refresh_language_servers(false);
        self.preview_palette_theme();
    }

    pub(crate) fn apply_resolved_settings(&mut self, settings: &Settings) {
        // A full stack replay should only replace document-specific preferences
        // when their default actually changes. Controls may have already updated
        // the live default, so compare with both it and the previous stack.
        let current = self.current_settings();
        let mut applied = settings.clone();
        macro_rules! preserve_document_overrides {
            ($($field:ident),+ $(,)?) => {
                $(if settings.$field == self.settings_effective.$field
                    && settings.$field == current.$field
                {
                    applied.$field = None;
                })+
            };
        }
        preserve_document_overrides!(tab_size, use_tabs, word_wrap, word_wrap_column,
            whitespace_mode, read_only, cursor_surrounding_lines, large_file_threshold_mb);
        self.animation_settings = crate::animation_config::AnimationSettings::default();
        self.copy_animation.enabled = settings.animation_copy.unwrap_or(true);
        self.copy_animation.duration_ms = settings.animation_copy_duration_ms
            .unwrap_or(crate::copy_animation::DEFAULT_DURATION_MS);
        if !self.copy_animation.enabled { self.copy_animation.clear(); }
        self.chat.output_detail = settings.chat_output_detail.as_deref()
            .map(crate::app_acp::OutputDetail::parse).unwrap_or_default();
        self.chat.thinking_expanded = settings.chat_thinking_expanded.unwrap_or(false);
        // Optional collection fields use `None` to mean "inherit/default".
        // Applying a complete stack must actively clear a previously applied
        // override instead of leaving stale runtime state behind.
        if settings.keybindings.is_none() {
            for keybinding in &mut self.keybindings {
                keybinding.reset();
            }
        }
        if settings.language_associations.is_none() {
            self.language_associations.clear();
            for tab in &mut self.tabs {
                tab.editor.language_associations.clear();
            }
            for group in &mut self.groups {
                for tab in &mut group.tabs {
                    tab.editor.language_associations.clear();
                }
            }
        }
        if settings.formatters.is_none() {
            self.formatters.clear();
        }
        self.apply_settings(&applied);
    }

    /// Atomically patch one preference while preserving unknown keys and external edits.
    /// Failure leaves both the disk file and live settings unchanged.
    pub(crate) fn patch_settings_scope(
        &mut self,
        scope: crate::settings::SettingsScope,
        key: &str,
        patch: impl Fn(&mut Settings),
    ) -> Result<(), String> {
        use crate::settings::SettingsScope;
        let (path, layer) = match scope {
            SettingsScope::Global => (&self.settings_global_path, &self.settings_global),
            SettingsScope::Workspace => (&self.settings_workspace_path, &self.settings_workspace),
        };
        let mut layer = layer.clone();
        if self.settings_persistence_enabled {
            if self
                .tabs
                .iter()
                .chain(self.groups.iter().flat_map(|group| &group.tabs))
                .any(|tab| {
                    tab.editor.modified
                        && tab
                            .editor
                            .file_path
                            .as_ref()
                            .is_some_and(|p| Self::same_open_path(p, path))
                })
            {
                return Err("The settings file has unsaved edits. Save or close those edits, then try again.".into());
            }
            let write = || -> Result<Settings, Box<dyn std::error::Error>> {
                use std::io::Write;
                let mut document: serde_json::Value = match std::fs::read_to_string(path) {
                    Ok(text) => serde_json::from_str(&text)?,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        serde_json::json!({})
                    }
                    Err(error) => return Err(error.into()),
                };
                let mut updated: Settings = serde_json::from_value(document.clone())?;
                patch(&mut updated);
                document
                    .as_object_mut()
                    .ok_or("Settings must be an object")?
                    .insert(
                        key.into(),
                        if matches!(key, "files.exclude" | "search.exclude") {
                            serde_json::to_value(&updated)?[key].as_object().cloned().map(serde_json::Value::Object).unwrap_or_else(|| serde_json::json!({}))
                        } else { serde_json::to_value(&updated)?[key].clone() },
                    );
                let parent = path.parent().ok_or("Settings path has no parent")?;
                std::fs::create_dir_all(parent)?;
                let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
                if let Ok(metadata) = path.metadata() {
                    if metadata.permissions().readonly() {
                        return Err("Settings file is read-only".into());
                    }
                    temporary
                        .as_file()
                        .set_permissions(metadata.permissions())?;
                }
                serde_json::to_writer_pretty(&mut temporary, &document)?;
                temporary.write_all(b"\n")?;
                temporary.as_file().sync_all()?;
                temporary.persist(path)?;
                Ok(updated)
            };
            layer = write().map_err(|error| error.to_string())?;
        } else {
            patch(&mut layer);
        }
        match scope {
            SettingsScope::Global => self.settings_global = layer,
            SettingsScope::Workspace => self.settings_workspace = layer,
        }
        self.apply_effective_settings();
        Ok(())
    }

    fn write_settings_scope(
        &mut self,
        scope: crate::settings::SettingsScope,
        mut settings: Settings,
    ) {
        settings.validate();
        let path = match scope {
            crate::settings::SettingsScope::Global => self.settings_global_path.clone(),
            crate::settings::SettingsScope::Workspace => self.settings_workspace_path.clone(),
        };

        if self.settings_persistence_enabled {
            if let Err(error) = crate::settings::save_to_file(&path, &settings) {
                log_error!("failed to save {} settings: {error}", scope.label());
                self.notify(format!("Could not save {} settings: {error}", scope.label()));
            }
        }

        match scope {
            crate::settings::SettingsScope::Global => self.settings_global = settings,
            crate::settings::SettingsScope::Workspace => self.settings_workspace = settings,
        }
    }

    pub(crate) fn set_status_bar_item_pinned(&mut self, item: &str, pinned: bool) {
        // Taskbar preferences normally follow the user across workspaces. If
        // this item already has a workspace override, edit that effective
        // choice so the visible bar always responds to the user's action.
        let scope = if self.settings_workspace.status_bar_items.contains_key(item)
            || (item == "word_count" && self.settings_workspace.show_word_count.is_some())
        {
            crate::settings::SettingsScope::Workspace
        } else {
            crate::settings::SettingsScope::Global
        };
        let mut target = match scope {
            crate::settings::SettingsScope::Global => self.settings_global.clone(),
            crate::settings::SettingsScope::Workspace => self.settings_workspace.clone(),
        };
        let changed = target.status_bar_items.insert(item.to_string(), pinned) != Some(pinned);
        let legacy_changed = item == "word_count" && target.show_word_count != Some(pinned);
        if item == "word_count" {
            target.show_word_count = Some(pinned);
        }
        if changed || legacy_changed {
            self.write_settings_scope(scope, target);
        }
        // Moving a status item must not reapply unrelated editor preferences
        // (such as a document's temporary word-wrap or read-only mode).
        let effective = self.settings_for_scope(crate::settings::SettingsScope::Workspace);
        if item == "word_count" {
            self.show_word_count = effective.show_word_count.unwrap_or(pinned);
            self.settings_effective.show_word_count = effective.show_word_count;
        }
        self.status_bar_pins = effective.status_bar_items.clone();
        self.settings_effective.status_bar_items = effective.status_bar_items;
    }

    fn copy_json_setting_field(target: &mut Settings, source: &Settings, key: &str) -> bool {
        let Ok(mut target_value) = serde_json::to_value(&*target) else {
            return false;
        };
        let Ok(source_value) = serde_json::to_value(source) else {
            return false;
        };
        let (Some(target_object), Some(source_object)) =
            (target_value.as_object_mut(), source_value.as_object())
        else {
            return false;
        };
        let Some(value) = source_object.get(key) else {
            return false;
        };
        if value.is_null() {
            target_object.remove(key);
        } else {
            target_object.insert(key.to_string(), value.clone());
        }
        match serde_json::from_value(target_value) {
            Ok(updated) => {
                *target = updated;
                true
            }
            Err(error) => {
                log_error!("failed to update setting field {key}: {error}");
                false
            }
        }
    }

    fn keybinding_overrides_for_scope(
        &self,
        scope: crate::settings::SettingsScope,
        existing_layer: &Settings,
    ) -> Option<Vec<KeybindingSetting>> {
        let parent = match scope {
            crate::settings::SettingsScope::Global => self.settings_defaults.clone(),
            crate::settings::SettingsScope::Workspace => {
                self.settings_defaults.merge(&self.settings_global)
            }
        };
        let parent_bindings = Self::keybindings_for_settings(&parent);
        let mut overrides = Vec::new();
        for (binding, inherited) in self.keybindings.iter().zip(parent_bindings.iter()) {
            if binding.modifiers != inherited.modifiers || binding.code != inherited.code {
                overrides.push(KeybindingSetting {
                    command: Some(binding.id.to_string()),
                    key: Some(binding.display()),
                    ..Default::default()
                });
            }
        }

        // Preserve entries for commands supplied by a newer build or plugin;
        // editing a built-in shortcut must never discard unknown JSON data.
        if let Some(existing) = &existing_layer.keybindings {
            overrides.extend(existing.iter().filter(|entry| {
                let known_command = entry.command.as_deref().is_some_and(|command| {
                    self.keybindings.iter().any(|binding| binding.id == command)
                });
                let known_index = entry.command.is_none()
                    && entry.index.is_some_and(|index| index < self.keybindings.len());
                !known_command && !known_index
            }).cloned());
        }

        (!overrides.is_empty()).then_some(overrides)
    }

    pub(crate) fn persist_scoped_setting_change(&mut self, id: &str, before: &Settings) {
        let Some(key) = Self::setting_json_key(id) else {
            self.apply_effective_settings();
            return;
        };
        if key == "keybindings" {
            let mut target = match self.settings_scope {
                crate::settings::SettingsScope::Global => self.settings_global.clone(),
                crate::settings::SettingsScope::Workspace => self.settings_workspace.clone(),
            };
            let overrides =
                self.keybinding_overrides_for_scope(self.settings_scope, &target);
            if target.keybindings != overrides {
                target.keybindings = overrides;
                self.write_settings_scope(self.settings_scope, target);
            }
            self.apply_effective_settings();
            return;
        }
        let after = self.current_settings();
        let before_value = serde_json::to_value(before).ok();
        let after_value = serde_json::to_value(&after).ok();
        if before_value
            .as_ref()
            .and_then(|value| value.get(key))
            == after_value.as_ref().and_then(|value| value.get(key))
        {
            self.apply_effective_settings();
            return;
        }

        let mut target = match self.settings_scope {
            crate::settings::SettingsScope::Global => self.settings_global.clone(),
            crate::settings::SettingsScope::Workspace => self.settings_workspace.clone(),
        };
        if Self::copy_json_setting_field(&mut target, &after, key) {
            if id == "word_count" {
                target.status_bar_items
                    .insert("word_count".into(), self.show_word_count);
            }
            self.write_settings_scope(self.settings_scope, target);
        }
        self.apply_effective_settings();
    }

    pub(crate) fn reset_setting_in_selected_scope(&mut self, id: &str) -> bool {
        if crate::app_vtcomplete_settings::field(id).is_some() {
            return self.reset_completion_setting(id);
        }
        self.flush_live_settings();
        if let Some(index) = id
            .strip_prefix("kb_")
            .and_then(|value| value.parse::<usize>().ok())
        {
            return self.reset_keybinding_in_selected_scope(index);
        }
        let Some(key) = Self::setting_json_key(id) else {
            return false;
        };
        let mut target_value = match self.settings_scope {
            crate::settings::SettingsScope::Global => serde_json::to_value(&self.settings_global),
            crate::settings::SettingsScope::Workspace => {
                serde_json::to_value(&self.settings_workspace)
            }
        }
        .unwrap_or_default();
        let Some(target_object) = target_value.as_object_mut() else {
            return false;
        };
        let has_word_count_pin = id == "word_count"
            && target_object.get("statusBarItems")
                .and_then(serde_json::Value::as_object)
                .is_some_and(|items| items.contains_key("word_count"));
        if target_object.get(key).is_none_or(serde_json::Value::is_null) && !has_word_count_pin {
            return false;
        }
        target_object.remove(key);
        if id == "word_count"
            && let Some(items) = target_object.get_mut("statusBarItems")
                .and_then(serde_json::Value::as_object_mut)
        {
            items.remove("word_count");
        }
        let Ok(target) = serde_json::from_value(target_value) else {
            return false;
        };
        self.write_settings_scope(self.settings_scope, target);
        self.apply_effective_settings();
        self.notify(format!(
            "Reset {id} in {} settings",
            self.settings_scope.label()
        ));
        true
    }

    pub(crate) fn reset_keybinding_in_selected_scope(&mut self, index: usize) -> bool {
        self.flush_live_settings();
        let Some(binding) = self.keybindings.get(index) else {
            return false;
        };
        let command = binding.id;
        let label = binding.label;
        let mut target = match self.settings_scope {
            crate::settings::SettingsScope::Global => self.settings_global.clone(),
            crate::settings::SettingsScope::Workspace => self.settings_workspace.clone(),
        };
        let Some(entries) = target.keybindings.as_mut() else {
            return false;
        };
        let previous_len = entries.len();
        entries.retain(|entry| {
            entry.command.as_deref() != Some(command)
                && !(entry.command.is_none() && entry.index == Some(index))
        });
        if entries.len() == previous_len {
            return false;
        }
        if entries.is_empty() {
            target.keybindings = None;
        }
        self.write_settings_scope(self.settings_scope, target);
        self.apply_effective_settings();
        self.notify(format!(
            "Reset {label} in {} settings",
            self.settings_scope.label()
        ));
        true
    }

    /// Persist any live setting changes that were not made through a specific
    /// Settings control (for example a keybinding capture), without flattening
    /// the effective workspace values into the selected scope.
    pub(crate) fn save_settings(&mut self) {
        if self.persist_runtime_settings(Some(self.settings_scope)) {
            self.apply_effective_settings();
        }
    }

    /// Commit UI preferences before Settings reads or reapplies the layer stack.
    /// Follow an existing workspace override; otherwise update User settings.
    /// Do not replay settings here: document overrides and previews stay live.
    pub(crate) fn flush_live_settings(&mut self) {
        self.persist_runtime_settings(None);
    }

    fn persist_runtime_settings(&mut self, scope: Option<crate::settings::SettingsScope>) -> bool {
        use crate::settings::SettingsScope::{Global, Workspace};

        let after = self.current_settings();
        // The resolved runtime snapshot writes every effect's value explicitly,
        // while the layer stack leaves inherited per-effect values absent.
        // Compare their effective values so saving an unrelated preference does
        // not turn animation defaults or legacy fallbacks into local overrides.
        let mut before = self.settings_effective.clone();
        before.animation_copy = Some(before.animation_copy.unwrap_or(true));
        before.animation_copy_duration_ms = Some(before.animation_copy_duration_ms
            .unwrap_or(crate::copy_animation::DEFAULT_DURATION_MS));
        let mut animations = crate::animation_config::AnimationSettings::default();
        animations.apply(&before);
        for effect in crate::animation_config::AnimationEffect::ALL {
            before.set_animation_effect_duration(effect, animations.duration(effect));
            before.set_animation_effect_intensity(effect, animations.intensity(effect));
        }
        let Ok(before_value) = serde_json::to_value(&before) else {
            return false;
        };
        let Ok(after_value) = serde_json::to_value(&after) else {
            return false;
        };
        let (Some(before_object), Some(after_object)) =
            (before_value.as_object(), after_value.as_object())
        else {
            return false;
        };

        let workspace = serde_json::to_value(&self.settings_workspace).unwrap_or_default();
        let mut targets = [self.settings_global.clone(), self.settings_workspace.clone()];
        let mut changed = [false; 2];
        for (key, after_field) in after_object {
            if before_object.get(key) != Some(after_field) {
                let scope = scope.unwrap_or_else(|| {
                    if workspace.get(key).is_some_and(|value| !value.is_null()) {
                        Workspace
                    } else {
                        Global
                    }
                });
                let index = usize::from(scope == Workspace);
                let target = &mut targets[index];
                if key == "keybindings" {
                    let overrides = self.keybinding_overrides_for_scope(scope, target);
                    if target.keybindings != overrides {
                        target.keybindings = overrides;
                        changed[index] = true;
                    }
                } else {
                    changed[index] |= Self::copy_json_setting_field(target, &after, key);
                }
                Self::copy_json_setting_field(&mut self.settings_effective, &after, key);
            }
        }
        for (index, target) in targets.into_iter().enumerate() {
            if changed[index] {
                self.write_settings_scope(if index == 0 { Global } else { Workspace }, target);
            }
        }
        changed.into_iter().any(|changed| changed)
    }

    /// Apply explicit preferences over a restored layout without replacing its
    /// saved view state with the defaults for a newly opened workspace.
    pub(crate) fn apply_session_settings(&mut self) {
        let settings = self.settings_global.merge(&self.settings_workspace);
        self.apply_settings(&settings);
        // Restoring a directory's layout is not a preference edit. Keep the
        // runtime baseline in sync so quitting cannot promote those saved
        // dimensions or visibility flags into global/workspace overrides.
        self.settings_effective = self.current_settings();
        self.refresh_language_servers(false);
    }

    /// Build session state from current app state
    pub(crate) fn build_session(&self) -> crate::session::Session {
        let saved_active_tab = |tabs: &[Tab], active_tab: usize| {
            let preceding_files = tabs
                .iter()
                .take(active_tab)
                .filter(|tab| tab.kind == TabKind::File)
                .count();
            let file_count = tabs.iter().filter(|tab| tab.kind == TabKind::File).count();
            preceding_files.min(file_count.saturating_sub(1))
        };
        let visibility = if self.zen_mode {
            self.zen_pre_state.unwrap_or((true, true, false, true, true, true))
        } else {
            (
                self.show_explorer,
                self.show_minimap,
                self.show_panel,
                self.show_tab_bar,
                self.show_status_bar,
                self.show_activity_bar,
            )
        };
        let save_tab = |tab: &Tab| -> crate::session::SavedTab {
            // Preserve the on-disk manifest verbatim while its background
            // session load is still pending. An immediate quit must never
            // discard recovery sidecars that have not been hydrated yet.
            if tab.editor.bg_loading {
                if let Some(restore) = &tab.editor.pending_session_restore {
                    return restore.tab.clone();
                }
            }
            let unsaved = tab.editor.modified.then(|| tab.editor.rope.clone());
            let (undo_entries, redo_entries) = if tab.editor.modified {
                (
                    Some(tab.editor.export_undo_stack()),
                    Some(tab.editor.export_redo_stack()),
                )
            } else {
                (None, None)
            };
            crate::session::SavedTab {
                file_path: tab
                    .editor
                    .file_path
                    .as_ref()
                    .map(|p| p.display().to_string()),
                cursor_line: tab.editor.cursor_line,
                cursor_col: tab.editor.cursor_col,
                scroll_offset: tab.editor.scroll_offset,
                horizontal_scroll: tab.editor.horizontal_scroll,
                pinned: tab.pinned,
                preview: tab.preview && !tab.pinned && !tab.has_unsaved_changes(),
                pending_unsaved_content: unsaved,
                unsaved_file: None,
                undo_history_file: None,
                language_override: tab.editor.language_override.clone(),
                undo_entries,
                redo_entries,
                bookmarks: tab.editor.bookmarks.iter().copied().collect(),
                folded_ranges: tab
                    .editor
                    .folded_ranges
                    .iter()
                    .map(|(&k, &v)| (k, v))
                    .collect(),
            }
        };
        let tabs: Vec<crate::session::SavedTab> = self
            .tabs
            .iter()
            .filter(|t| t.kind == TabKind::File)
            .map(&save_tab)
            .collect();
        // Save non-active groups
        let saved_groups: Vec<crate::session::SavedGroup> = self
            .groups
            .iter()
            .map(|g| crate::session::SavedGroup {
                tabs: g
                    .tabs
                    .iter()
                    .filter(|t| t.kind == TabKind::File)
                    .map(&save_tab)
                    .collect(),
                active_tab: saved_active_tab(&g.tabs, g.active_tab),
            })
            .collect();
        // Serialize theme name
        let theme_str = self.committed_theme_key().to_string();
        let sidebar_str = match self.sidebar_view {
            SidebarView::Explorer => "explorer",
            SidebarView::Search => "search",
            SidebarView::SourceControl => "source-control",
            SidebarView::Settings => "settings",
            SidebarView::Extensions => "extensions",
            SidebarView::Outline => "outline",
        };
        crate::session::Session {
            remote_checkpoints: self.remote_workspace.as_ref().map(|_| std::sync::Arc::new(self.tabs.iter()
                .chain(self.groups.iter().flat_map(|group| &group.tabs))
                .filter_map(|tab| Some((tab.editor.file_path.as_ref()?.to_string_lossy().into_owned(), tab.editor.remote_checkpoint.clone()?)))
                .collect())),
            desks: (self.desks.items.len()>1 || !self.desks.membership.is_empty()).then(||self.save_desks()),
            tabs,
            active_tab: saved_active_tab(&self.tabs, self.active_tab),
            explorer_width: self.explorer_width,
            show_explorer: visibility.0,
            show_minimap: visibility.1,
            show_panel: visibility.2,
            panel_height: self.panel_height,
            panel_tab: self.panel_tab,
            show_line_numbers: Some(self.show_line_numbers),
            relative_line_numbers: self.relative_line_numbers,
            show_indent_guides: Some(self.show_indent_guides),
            show_open_editors: Some(self.show_open_editors),
            minimap_width: Some(self.minimap_width),
            sidebar_right: Some(self.sidebar_right),
            theme: Some(theme_str),
            sidebar_view: Some(sidebar_str.to_string()),
            sc_graph_collapsed: self.sc_graph_collapsed,
            sc_visibility: self.sc_visibility,
            repositories: self.saved_repositories(),
            sc_graph_height: self.sc_graph_height,
            outline_scroll: self.outline_scroll,
            search_history: self.search_history.saved(&self.search_query),
            global_search_history: self.global_search_history.saved(&self.global_search_query),
            global_search_replace_history: self.global_search_replace_history.saved(&self.global_search_replace_query),
            search_include_history: self.search_include_history.saved(&self.search_include_pattern),
            search_exclude_history: self.search_exclude_history.saved(&self.search_exclude_pattern),
            global_search_query: self.global_search_query.clone(),
            global_search_replace_query: self.global_search_replace_query.clone(),
            search_include_pattern: self.search_include_pattern.clone(),
            search_exclude_pattern: self.search_exclude_pattern.clone(),
            search_focus_field: self.search_focus_field,
            split_active: self.split_active(),
            split_tab: if self.split_active() {
                self.active_group
            } else {
                0
            },
            split_vertical: self.split_vertical,
            split_ratio: self.split_ratio,
            split_tree: self.split_tree.clone(),
            groups: saved_groups,
            active_group: self.active_group,
            zen_mode: self.zen_mode,
            show_activity_bar: visibility.5,
            show_tab_bar: visibility.3,
            show_status_bar: visibility.4,
        }
    }

    /// Restore session state
    pub(crate) fn restore_session(&mut self, session: &crate::session::Session) {
        self.restore_search_histories(session);
        if let Some(remote) = &self.remote_workspace { remote.set_recovery(session.remote_checkpoints.clone()); }
        let lang_assoc = self.language_associations.clone();
        let scope_rules = self.animation_settings.scope_languages.clone();
        let default_word_wrap = self.default_word_wrap;
        let word_wrap_column = self.word_wrap_column;
        let default_whitespace_mode = self.default_whitespace_mode;
        let default_tab_size = self.default_tab_size;
        let default_use_tabs = self.default_use_tabs;
        let default_cursor_surrounding_lines = self.default_cursor_surrounding_lines;
        let enable_preview_tabs = self.enable_preview_tabs;
        let mut restore_jobs: rustc_hash::FxHashMap<
            std::path::PathBuf,
            Vec<crate::session::SessionRestore>,
        > = rustc_hash::FxHashMap::default();
        let mut untitled_restore_jobs = Vec::new();
        let mut restore_tab = |saved_tab: &crate::session::SavedTab| -> Tab {
            let restore = crate::session::SessionRestore {
                // Old workers may finish after a workspace switch. Their
                // results must never match a new placeholder for the same path.
                id: NEXT_SESSION_RESTORE_ID.fetch_add(1, Ordering::Relaxed),
                tab: saved_tab.clone(),
            };
            let mut editor = if let Some(path) = &saved_tab.file_path {
                Editor::new_loading(std::path::Path::new(path), 0)
            } else {
                let mut editor = Editor::new();
                editor.bg_loading = true;
                editor
            };
            editor.language_associations = lang_assoc.clone();
            editor.scope_bubble_rules = scope_rules.clone();
            editor.read_only = true;
            editor.word_wrap = default_word_wrap;
            editor.word_wrap_column = word_wrap_column;
            editor.whitespace_mode = default_whitespace_mode;
            editor.tab_size = default_tab_size;
            editor.use_tabs = default_use_tabs;
            editor.cursor_surrounding_lines = default_cursor_surrounding_lines;
            editor.pending_session_restore = Some(restore.clone());
            if let Some(path) = &saved_tab.file_path {
                let path = std::path::PathBuf::from(path);
                restore_jobs.entry(path).or_default().push(restore);
            } else {
                untitled_restore_jobs.push(restore);
            }

            Tab {
                editor,
                pinned: saved_tab.pinned,
                preview: enable_preview_tabs && saved_tab.preview && !saved_tab.pinned
                    && saved_tab.unsaved_file.is_none()
                    && saved_tab.pending_unsaved_content.is_none(),
                kind: TabKind::File,
                extension_view: None,
                file_search: Default::default(),
                terminal_idx: None,
            }
        };

        for saved_tab in &session.tabs {
            self.tabs.push(restore_tab(saved_tab));
        }
        if !self.tabs.is_empty() {
            self.active_tab = session.active_tab.min(self.tabs.len().saturating_sub(1));
            self.active_pane = ActivePane::Editor;
            // Rebuild MRU order for restored tabs
            self.tab_mru_order.clear();
            self.tab_mru_order.push(self.active_tab);
            for i in 0..self.tabs.len() {
                if i != self.active_tab {
                    self.tab_mru_order.push(i);
                }
            }
        }
        self.explorer_width = session.explorer_width.max(15);
        self.show_explorer = session.show_explorer;
        self.show_minimap = session.show_minimap;
        self.show_panel = session.show_panel;
        self.panel_height = session.panel_height.max(3);
        self.panel_tab = Self::normalize_panel_tab(session.panel_tab);
        if let Some(v) = session.show_line_numbers {
            self.show_line_numbers = v;
        }
        self.relative_line_numbers = session.relative_line_numbers;
        if let Some(v) = session.show_indent_guides {
            self.show_indent_guides = v;
        }
        if let Some(v) = session.show_open_editors {
            self.show_open_editors = v;
        }
        if let Some(v) = session.minimap_width {
            self.minimap_width = v.max(5);
        }
        if let Some(v) = session.sidebar_right {
            self.sidebar_right = v;
        }
        if let Some(ref name) = session.theme {
            if !self.set_theme_by_key(name) {
                self.set_theme_by_key("dark");
            }
        }
        if let Some(ref sv) = session.sidebar_view {
            self.sidebar_view = match sv.as_str() {
                "search" => SidebarView::Search,
                "source-control" => SidebarView::SourceControl,
                "settings" => SidebarView::Settings,
                "extensions" => SidebarView::Extensions,
                "outline" => SidebarView::Outline,
                _ => SidebarView::Explorer,
            };
        }
        self.sc_graph_collapsed = session.sc_graph_collapsed;
        self.set_source_control_visibility(session.sc_visibility);
        self.restore_repositories(session.repositories.clone());
        self.sc_graph_height = session.sc_graph_height.max(1);
        self.outline_scroll = session.outline_scroll;
        self.global_search_query = session.global_search_query.clone();
        self.global_search_replace_query = session.global_search_replace_query.clone();
        self.search_include_pattern = session.search_include_pattern.clone();
        self.search_exclude_pattern = session.search_exclude_pattern.clone();
        self.search_focus_field = session.search_focus_field.min(3);
        self.sync_activity_bar_selection();
        if self.sidebar_view == SidebarView::Outline {
            // Session files are restored asynchronously. Preserve the saved
            // scroll position until the active file arrives; clamp the outline
            // when the corresponding symbols have finished loading.
            if self
                .editor()
                .is_some_and(|editor| !editor.bg_loading && !editor.large_file_mode)
            {
                self.update_outline();
            }
        }
        self.global_search_active = self.show_explorer && self.sidebar_view == SidebarView::Search;
        self.global_search_field_cursors = [
            self.global_search_query.len(),
            self.global_search_replace_query.len(),
            self.search_include_pattern.len(),
            self.search_exclude_pattern.len(),
        ];
        self.global_search_field_selection_anchors = [None; 4];
        if self.show_explorer && self.sidebar_view == SidebarView::SourceControl {
            self.refresh_source_control_async();
            self.refresh_git_log_async();
        }
        self.zen_mode = session.zen_mode;
        self.zen_pre_state = session.zen_mode.then_some((
            session.show_explorer,
            session.show_minimap,
            session.show_panel,
            session.show_tab_bar,
            session.show_status_bar,
            session.show_activity_bar,
        ));
        self.show_activity_bar = session.show_activity_bar && !self.zen_mode;
        self.show_tab_bar = session.show_tab_bar && !self.zen_mode;
        self.show_status_bar = session.show_status_bar && !self.zen_mode;
        if self.zen_mode {
            self.show_explorer = false;
            self.show_minimap = false;
            self.show_panel = false;
            self.global_search_active = false;
        }
        // Restore split pane groups
        if !session.groups.is_empty() {
            self.split_vertical = session.split_vertical;
            self.split_ratio = session.split_ratio;
            for saved_group in &session.groups {
                let mut group = EditorGroup::new();
                for saved_tab in &saved_group.tabs {
                    group.tabs.push(restore_tab(saved_tab));
                }
                group.active_tab = saved_group
                    .active_tab
                    .min(group.tabs.len().saturating_sub(1));
                if !group.tabs.is_empty() { group.tab_mru_order.push(group.active_tab); }
                // The active group's tabs live in self.tabs, so its empty
                // placeholder is necessary to preserve every pane's index.
                self.groups.push(group);
            }
            if !self.groups.is_empty() {
                self.active_group = session
                    .active_group
                    .min(self.groups.len().saturating_sub(1));
                // Older manifests only stored flat groups. Preserve the full
                // split arrangement when available and valid for these groups.
                if self.groups.len() > 1 {
                    let dir = if self.split_vertical {
                        crate::app::SplitDirection::Horizontal
                    } else {
                        crate::app::SplitDirection::Vertical
                    };
                    let n = self.groups.len();
                    self.split_tree = session
                        .split_tree
                        .as_ref()
                        .filter(|tree| Self::valid_session_split_tree(tree, n))
                        .cloned()
                        .or_else(|| {
                            let ratios = if n == 2 && self.split_ratio.is_finite() {
                                let ratio = self.split_ratio.clamp(0.1, 0.9);
                                vec![ratio, 1.0 - ratio]
                            } else {
                                vec![1.0 / n as f32; n]
                            };
                            Some(crate::app::SplitNode::Split {
                                direction: dir,
                                children: (0..n).map(crate::app::SplitNode::Leaf).collect(),
                                ratios,
                            })
                        });
                }
            }
        } else if session.split_active {
            self.split_vertical = session.split_vertical;
            self.split_ratio = session.split_ratio;
        }
        drop(restore_tab);
        if let Some(desks) = &session.desks { self.restore_desks(desks); }
        let active_restore = self.editor().and_then(|editor| editor.pending_session_restore.as_ref())
            .map(|restore| restore.id);
        let mut visible_restores = rustc_hash::FxHashSet::default();
        if let Some(tree) = &self.split_tree {
            tree.visit_leaves(&mut |index| {
                if index != self.active_group
                    && let Some(group) = self.groups.get(index)
                    && let Some(tab) = group.tabs.get(group.active_tab)
                    && let Some(restore) = &tab.editor.pending_session_restore
                { visible_restores.insert(restore.id); }
            });
        }
        let priority = |restore: &crate::session::SessionRestore| {
            if Some(restore.id) == active_restore { 0 }
            else if visible_restores.contains(&restore.id) { 1 }
            else { 2 }
        };
        enum RestoreJob {
            File(std::path::PathBuf, Vec<crate::session::SessionRestore>),
            Untitled(crate::session::SessionRestore),
        }
        // Active buffer first (including untitled recovery), then visible split
        // panes, then hidden tabs/desks. Recovery disk reads never occupy the
        // CPU pool needed for highlighting and immediate editor feedback.
        let mut jobs: Vec<_> = restore_jobs.into_iter().map(|(path, restores)| {
            (restores.iter().map(&priority).min().unwrap_or(2), RestoreJob::File(path, restores))
        }).chain(untitled_restore_jobs.into_iter().map(|restore| {
            (priority(&restore), RestoreJob::Untitled(restore))
        })).collect();
        jobs.sort_by_key(|(priority, _)| *priority);
        for (priority, job) in jobs {
            match job {
                RestoreJob::File(path, restores) => {
                    self.queue_background_session_file_load(path, restores, priority < 2);
                }
                RestoreJob::Untitled(mut restore) => {
                    let project_root = self.explorer.root.clone();
                    let activity_path = project_root.clone();
                    let sender = self.background.sender.clone();
                    self.background.read_for_file("session-recovery", &activity_path, priority < 2, move || {
                        crate::session::hydrate_saved_tab(&project_root, &mut restore.tab);
                        let _ = sender.send(crate::background::BackgroundResult::SessionUntitledLoaded { restore });
                    });
                }
            }
        }
        // Restored editors bypass `open_file`, so request their Git gutter
        // markers explicitly instead of leaving the active file undecorated
        // until the periodic refresh runs.
        self.refresh_git_line_status_async();
    }

    fn valid_session_split_tree(tree: &crate::app::SplitNode, group_count: usize) -> bool {
        fn valid_splits(tree: &crate::app::SplitNode) -> bool {
            match tree {
                crate::app::SplitNode::Leaf(_) => true,
                crate::app::SplitNode::Split { children, ratios, .. } => {
                    children.len() >= 2
                        && ratios.len() == children.len()
                        && ratios.iter().all(|ratio| ratio.is_finite() && *ratio > 0.0)
                        && children.iter().all(valid_splits)
                }
            }
        }

        let mut leaves = tree.leaves();
        leaves.sort_unstable();
        leaves == (0..group_count).collect::<Vec<_>>() && valid_splits(tree)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for_session_restore(app: &mut App) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app
            .tabs
            .iter()
            .chain(app.groups.iter().flat_map(|group| group.tabs.iter()))
            .any(|tab| tab.editor.bg_loading)
            && std::time::Instant::now() < deadline
        {
            app.poll_background_results();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        app.poll_background_results();
    }

    // --- wrap_setting_text_for_width tests ---

    #[test]
    fn wrap_text_basic() {
        let result = App::wrap_setting_text_for_width("hello world foo bar", 11);
        assert_eq!(result, vec!["hello world", "foo bar"]);
    }

    #[test]
    fn wrap_text_empty() {
        let result = App::wrap_setting_text_for_width("", 10);
        assert_eq!(result, vec![""]);
    }

    #[test]
    fn wrap_text_long_word() {
        let result = App::wrap_setting_text_for_width("abcdefghij", 5);
        assert_eq!(result, vec!["abcde", "fghij"]);
    }

    #[test]
    fn wrap_text_single_word_fits() {
        let result = App::wrap_setting_text_for_width("hello", 10);
        assert_eq!(result, vec!["hello"]);
    }

    // --- settings_category_for_id tests ---

    #[test]
    fn category_editor() {
        assert_eq!(App::settings_category_for_id("tab_size"), 1);
    }

    #[test]
    fn category_appearance() {
        assert_eq!(App::settings_category_for_id("theme"), 2);
    }

    #[test]
    fn category_shortcuts() {
        assert_eq!(App::settings_category_for_id("kb_copy"), 5);
    }

    #[test]
    fn category_unknown() {
        assert_eq!(App::settings_category_for_id("unknown_setting"), 2);
    }

    // --- setting_field_kind tests ---

    #[test]
    fn field_kind_heading() {
        assert_eq!(App::setting_field_kind("heading_editor"), "heading");
    }

    #[test]
    fn field_kind_number() {
        assert_eq!(App::setting_field_kind("tab_size"), "number");
    }

    #[test]
    fn field_kind_checkbox() {
        assert_eq!(App::setting_field_kind("word_wrap"), "checkbox");
    }

    #[test]
    fn field_kind_dropdown() {
        assert_eq!(App::setting_field_kind("theme"), "dropdown");
    }

    // --- setting_description tests ---

    #[test]
    fn description_known_id() {
        let desc = App::setting_description("tab_size");
        assert!(!desc.is_empty());
    }

    #[test]
    fn description_unknown_id() {
        let desc = App::setting_description("nonexistent_setting");
        assert!(desc.is_empty());
    }

    #[test]
    fn description_keyboard_shortcut() {
        let desc = App::setting_description("kb_copy");
        assert!(desc.is_empty());
    }

    #[test]
    fn settings_categories_nonempty() {
        let cats = App::settings_categories();
        assert!(!cats.is_empty());
        assert!(cats.contains(&"Text Editor"));
        assert!(cats.contains(&"Workbench"));
    }

    #[test]
    fn setting_description_lines_basic() {
        let lines = App::setting_description_lines("Tab Size", "tab_size", 80);
        assert!(!lines.is_empty());
        assert!(lines[0].contains("Tab Size"));
    }

    #[test]
    fn setting_description_lines_no_desc() {
        let lines = App::setting_description_lines("Heading", "heading_editor", 80);
        assert!(!lines.is_empty());
        assert_eq!(lines[0], "Heading");
    }

    #[test]
    fn setting_visual_height_at_least_two() {
        // visual height = description lines + 1 (for the control row)
        let h = App::setting_visual_height("Tab Size", "tab_size", 80);
        assert!(h >= 2);
    }

    #[test]
    fn setting_visual_height_narrow_wraps() {
        let narrow = App::setting_visual_height("Tab Size", "tab_size", 20);
        let lines = App::setting_description_lines("Tab Size", "tab_size", 20);
        assert!(lines.len() > 2);
        assert_eq!(narrow, lines.len() + 2);
    }

    // --- build_session tests ---

    #[test]
    fn build_session_shares_unsaved_text_and_retains_snapshot_across_edits() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(&"unsaved source\n".repeat(10_000));
        editor.modified = true;
        let expected = editor.rope.clone();
        app.tabs.push(Tab::new_file(editor));
        let session = app.build_session();
        let captured = session.tabs[0].pending_unsaved_content.as_ref().unwrap();
        assert!(captured.is_instance(&expected));
        app.tabs[0].editor.rope.insert(0, "later edit\n");
        assert_eq!(captured, &expected);
        let cloned = session.clone();
        assert!(cloned.tabs[0].pending_unsaved_content.as_ref().unwrap().is_instance(captured));
    }

    #[test]
    fn build_session_empty_app() {
        let app = App::test_new();
        let session = app.build_session();
        // No tabs open by default, so tabs should be empty
        assert!(session.tabs.is_empty());
        assert_eq!(session.active_tab, 0);
        assert_eq!(session.explorer_width, 30);
        assert!(session.show_explorer);
        assert!(session.show_minimap);
        assert!(!session.show_panel);
        assert_eq!(session.panel_height, 10);
        assert_eq!(session.panel_tab, 1);
        assert!(!session.zen_mode);
    }

    #[test]
    fn build_session_preserves_layout_flags() {
        let app = App::test_new();
        let session = app.build_session();
        assert_eq!(session.show_line_numbers, Some(true));
        assert!(!session.relative_line_numbers);
        assert_eq!(session.show_indent_guides, Some(true));
        assert_eq!(session.show_open_editors, Some(true));
        assert_eq!(session.minimap_width, Some(10));
        assert_eq!(session.sidebar_right, Some(false));
        assert!(session.theme.is_some());
        assert!(session.sidebar_view.is_some());
    }

    #[test]
    fn build_session_split_state_default() {
        let app = App::test_new();
        let session = app.build_session();
        assert!(!session.split_active);
        assert!(session.split_vertical);
        assert!((session.split_ratio - 0.5).abs() < f32::EPSILON);
        assert!(session.groups.is_empty());
        assert_eq!(session.active_group, 0);
    }

    // --- restore_session tests ---

    #[test]
    fn restore_session_empty_roundtrip() {
        let app = App::test_new();
        let session = app.build_session();
        let mut app2 = App::test_new();
        app2.restore_session(&session);
        assert_eq!(app2.explorer_width, app.explorer_width);
        assert_eq!(app2.show_explorer, app.show_explorer);
        assert_eq!(app2.show_minimap, app.show_minimap);
        assert_eq!(app2.show_panel, app.show_panel);
        assert_eq!(app2.panel_height, app.panel_height);
        assert_eq!(app2.panel_tab, app.panel_tab);
        assert_eq!(app2.zen_mode, app.zen_mode);
    }

    #[test]
    fn restore_session_applies_layout_overrides() {
        let app = App::test_new();
        let mut session = app.build_session();
        session.show_explorer = false;
        session.show_minimap = false;
        session.show_panel = true;
        session.explorer_width = 50;
        session.panel_height = 20;
        session.panel_tab = 1;
        session.zen_mode = true;
        session.relative_line_numbers = true;
        session.show_line_numbers = Some(false);
        session.show_indent_guides = Some(false);
        session.show_open_editors = Some(false);
        session.sidebar_right = Some(true);
        session.minimap_width = Some(20);

        let mut app2 = App::test_new();
        app2.restore_session(&session);
        assert!(!app2.show_explorer);
        assert!(!app2.show_minimap);
        assert!(!app2.show_panel);
        assert!(app2.zen_pre_state.unwrap().2);
        assert_eq!(app2.explorer_width, 50);
        assert_eq!(app2.panel_height, 20);
        assert_eq!(app2.panel_tab, 1);
        assert!(app2.zen_mode);
        assert!(app2.relative_line_numbers);
        assert!(!app2.show_line_numbers);
        assert!(!app2.show_indent_guides);
        assert!(!app2.show_open_editors);
        assert!(app2.sidebar_right);
        assert_eq!(app2.minimap_width, 20);
    }

    #[test]
    fn restore_session_clamps_panel_tab() {
        let app = App::test_new();
        let mut session = app.build_session();
        session.panel_tab = 255;
        let mut app2 = App::test_new();
        app2.restore_session(&session);
        assert_eq!(app2.panel_tab, 1);
    }

    #[test]
    fn restore_session_preserves_notifications_panel_tab() {
        let app = App::test_new();
        let mut session = app.build_session();
        session.show_panel = true;
        session.panel_tab = 3;

        let mut app2 = App::test_new();
        app2.restore_session(&session);

        assert!(app2.show_panel);
        assert_eq!(app2.panel_tab, 3);
    }

    #[test]
    fn restore_session_clamps_explorer_width_min() {
        let app = App::test_new();
        let mut session = app.build_session();
        session.explorer_width = 5;
        let mut app2 = App::test_new();
        app2.restore_session(&session);
        assert_eq!(app2.explorer_width, 15);
    }

    #[test]
    fn restore_session_clamps_panel_height_min() {
        let app = App::test_new();
        let mut session = app.build_session();
        session.panel_height = 1;
        let mut app2 = App::test_new();
        app2.restore_session(&session);
        assert_eq!(app2.panel_height, 3);
    }

    #[test]
    fn restore_session_sidebar_view_variants() {
        let app = App::test_new();
        let mut session = app.build_session();
        for (sv_str, expected, expected_activity) in [
            ("search", SidebarView::Search, 1),
            ("source-control", SidebarView::SourceControl, 2),
            ("settings", SidebarView::Settings, usize::MAX),
            ("extensions", SidebarView::Extensions, 4),
            ("outline", SidebarView::Outline, 5),
            ("explorer", SidebarView::Explorer, 0),
            ("unknown", SidebarView::Explorer, 0),
        ] {
            session.sidebar_view = Some(sv_str.to_string());
            let mut app2 = App::test_new();
            app2.restore_session(&session);
            assert_eq!(
                app2.sidebar_view, expected,
                "sidebar_view mismatch for {sv_str:?}"
            );
            assert_eq!(
                app2.activity_bar_selected, expected_activity,
                "activity_bar_selected mismatch for {sv_str:?}"
            );
        }
    }

    #[test]
    fn restore_session_rebuilds_outline_items_for_outline_view() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("outline_restore.rs");
        std::fs::write(
            &file_path,
            "fn restored_symbol() {}\nstruct RestoredStruct;\n",
        )
        .expect("write outline fixture");

        let mut app = App::test_new();
        app.open_file(&file_path);
        app.sidebar_view = SidebarView::Outline;
        let session = app.build_session();

        let mut restored = App::test_new();
        restored.restore_session(&session);
        wait_for_session_restore(&mut restored);

        assert_eq!(restored.sidebar_view, SidebarView::Outline);
        assert!(
            restored
                .outline_items
                .iter()
                .any(|item| item.name == "restored_symbol"),
            "expected outline items to be rebuilt for restored outline view"
        );
        assert!(
            restored
                .outline_items
                .iter()
                .any(|item| item.name == "RestoredStruct"),
            "expected multiple outline symbols to be available after restore"
        );
    }

    #[test]
    fn restore_session_preserves_outline_scroll_for_outline_view() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("outline_scroll_restore.rs");
        let mut content = String::new();
        for idx in 0..40 {
            content.push_str(&format!("fn outline_symbol_{idx}() {{}}\n"));
        }
        std::fs::write(&file_path, content).expect("write outline scroll fixture");

        let mut app = App::test_new();
        app.open_file(&file_path);
        app.sidebar_view = SidebarView::Outline;
        app.update_outline();
        app.outline_scroll = 25;
        let session = app.build_session();

        let mut restored = App::test_new();
        restored.restore_session(&session);
        wait_for_session_restore(&mut restored);

        assert_eq!(restored.sidebar_view, SidebarView::Outline);
        assert_eq!(restored.outline_scroll, 25);
    }

    #[test]
    fn restore_session_preserves_global_search_sidebar_inputs_and_focus() {
        let mut app = App::test_new();
        app.show_explorer = true;
        app.sidebar_view = SidebarView::Search;
        app.global_search_active = true;
        app.global_search_query = "TODO".to_string();
        app.global_search_replace_query = "DONE".to_string();
        app.search_include_pattern = "src\\**\\*.rs".to_string();
        app.search_exclude_pattern = "target".to_string();
        app.search_focus_field = 3;

        let session = app.build_session();

        let mut restored = App::test_new();
        restored.restore_session(&session);

        assert_eq!(restored.sidebar_view, SidebarView::Search);
        assert!(restored.global_search_active);
        assert_eq!(restored.global_search_query, "TODO");
        assert_eq!(restored.global_search_replace_query, "DONE");
        assert_eq!(restored.search_include_pattern, "src\\**\\*.rs");
        assert_eq!(restored.search_exclude_pattern, "target");
        assert_eq!(restored.search_focus_field, 3);
        assert_eq!(
            restored.global_search_field_cursors,
            [4, 4, "src\\**\\*.rs".len(), "target".len()]
        );
        assert_eq!(restored.global_search_field_selection_anchors, [None; 4]);
    }

    #[test]
    fn restore_session_refreshes_source_control_for_visible_source_control_view() {
        let app = App::test_new();
        let mut session = app.build_session();
        session.show_explorer = true;
        session.sidebar_view = Some("source-control".to_string());

        let mut restored = App::test_new();
        restored.restore_session(&session);

        assert_eq!(restored.sidebar_view, SidebarView::SourceControl);
        assert!(restored.bg_git_status_running);
        assert!(restored.bg_git_log_running);
    }

    #[test]
    fn restore_session_preserves_source_control_graph_state() {
        let app = App::test_new();
        let mut session = app.build_session();
        session.show_explorer = true;
        session.sidebar_view = Some("source-control".to_string());
        session.sc_graph_collapsed = true;
        session.sc_graph_height = 7;

        let mut restored = App::test_new();
        restored.restore_session(&session);

        assert_eq!(restored.sidebar_view, SidebarView::SourceControl);
        assert!(restored.sc_graph_collapsed);
        assert_eq!(restored.sc_graph_height, 7);
    }

    #[test]
    fn restore_session_missing_file_skips_tab() {
        let app = App::test_new();
        let mut session = app.build_session();
        session.tabs.push(crate::session::SavedTab {
            file_path: Some("/nonexistent/path/file.txt".to_string()),
            cursor_line: 0,
            cursor_col: 0,
            scroll_offset: 0,
            horizontal_scroll: 0,
            pinned: false,
            preview: false,
            pending_unsaved_content: None,
            unsaved_file: None,
            undo_history_file: None,
            language_override: None,
            undo_entries: None,
            redo_entries: None,
            bookmarks: Vec::new(),
            folded_ranges: Vec::new(),
        });
        let mut app2 = App::test_new();
        app2.restore_session(&session);
        wait_for_session_restore(&mut app2);
        // Nonexistent file tab is skipped
        assert!(app2.tabs.is_empty());
    }

    #[test]
    fn restore_session_hydrates_unsaved_content_off_thread() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("restored.txt");
        std::fs::write(&file_path, "disk baseline\n").unwrap();
        let session = crate::session::Session {
            tabs: vec![crate::session::SavedTab {
                file_path: Some(file_path.display().to_string()),
                cursor_line: 0,
                cursor_col: 7,
                scroll_offset: 0,
                horizontal_scroll: 0,
                pinned: true,
                preview: false,
                pending_unsaved_content: Some("unsaved buffer\n".into()),
                unsaved_file: None,
                undo_history_file: None,
                language_override: None,
                undo_entries: None,
                redo_entries: None,
                bookmarks: Vec::new(),
                folded_ranges: Vec::new(),
            }],
            ..crate::session::Session::default()
        };
        crate::session::save_session(dir.path(), &session).unwrap();
        let manifest = crate::session::load_session(dir.path()).unwrap();
        assert!(manifest.tabs[0].pending_unsaved_content.is_none());

        let mut restored = App::new(dir.path().to_path_buf());
        restored.restore_session(&manifest);
        assert!(restored.tabs[0].editor.bg_loading);
        wait_for_session_restore(&mut restored);

        let editor = &restored.tabs[0].editor;
        assert_eq!(editor.rope.to_string(), "unsaved buffer\n");
        assert!(editor.modified);
        assert_eq!(editor.cursor_col, 7);
    }

    #[test]
    fn restore_session_recovers_unsaved_content_when_disk_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let missing_path = dir.path().join("missing.txt");
        let session = crate::session::Session {
            tabs: vec![crate::session::SavedTab {
                file_path: Some(missing_path.display().to_string()),
                cursor_line: 0,
                cursor_col: 4,
                scroll_offset: 0,
                horizontal_scroll: 0,
                pinned: false,
                preview: false,
                pending_unsaved_content: Some("kept unsaved\n".into()),
                unsaved_file: None,
                undo_history_file: None,
                language_override: None,
                undo_entries: None,
                redo_entries: None,
                bookmarks: Vec::new(),
                folded_ranges: Vec::new(),
            }],
            ..crate::session::Session::default()
        };
        crate::session::save_session(dir.path(), &session).unwrap();
        let manifest = crate::session::load_session(dir.path()).unwrap();

        let mut restored = App::new(dir.path().to_path_buf());
        restored.restore_session(&manifest);
        // Before hydration completes, an immediate quit must preserve the
        // sidecar reference even though App supplies its own layout defaults.
        assert_eq!(restored.build_session().tabs, manifest.tabs);
        wait_for_session_restore(&mut restored);

        assert_eq!(restored.tabs.len(), 1);
        let editor = &restored.tabs[0].editor;
        assert_eq!(editor.rope.to_string(), "kept unsaved\n");
        assert!(editor.modified);
        assert!(editor.disk_unavailable);
        assert_eq!(editor.cursor_col, 4);
    }

    #[test]
    fn restore_session_recovers_untitled_buffers_history_and_blank_tabs() {
        let dir = tempfile::tempdir().unwrap();
        let mut source = App::new(dir.path().to_path_buf());
        let mut editor = Editor::new();
        editor.save_undo();
        editor.rope = ropey::Rope::from_str("draft text\nsecond line\n");
        editor.modified = true;
        editor.cursor_line = 1;
        editor.cursor_col = 4;
        editor.scroll_offset = 1;
        editor.horizontal_scroll = 2;
        editor.language_override = Some("rust".into());
        editor.bookmarks.insert(1);
        let undo = editor.export_undo_stack();
        source.tabs.push(Tab::new_file(editor));
        source.tabs[0].pinned = true;
        source.tabs.push(Tab::new_file(Editor::new()));
        source.active_tab = 1;
        crate::session::save_session(dir.path(), &source.build_session()).unwrap();
        let manifest = crate::session::load_session(dir.path()).unwrap();
        assert!(manifest.tabs[0].unsaved_file.is_some());
        assert!(manifest.tabs[0].undo_history_file.is_some());

        let mut restored = App::new(dir.path().to_path_buf());
        restored.restore_session(&manifest);
        assert_eq!(restored.build_session().tabs, manifest.tabs);
        wait_for_session_restore(&mut restored);

        assert_eq!(restored.tabs.len(), 2);
        assert_eq!(restored.active_tab, 1);
        assert!(restored.tabs[0].pinned);
        let editor = &restored.tabs[0].editor;
        assert!(editor.file_path.is_none());
        assert_eq!(editor.rope.to_string(), "draft text\nsecond line\n");
        assert!(editor.modified);
        assert!(!editor.read_only);
        assert!(!editor.bg_loading);
        assert_eq!((editor.cursor_line, editor.cursor_col), (1, 4));
        assert_eq!(editor.scroll_offset, 1);
        assert_eq!(editor.horizontal_scroll, 2);
        assert_eq!(editor.language_override.as_deref(), Some("rust"));
        assert!(editor.bookmarks.contains(&1));
        assert_eq!(editor.export_undo_stack(), undo);
        assert!(restored.tabs[1].editor.file_path.is_none());
        assert_eq!(restored.tabs[1].editor.rope.len_chars(), 0);
        assert!(!restored.tabs[1].editor.modified);
        assert!(!restored.tabs[1].editor.bg_loading);
    }

    #[test]
    fn build_session_maps_active_tabs_after_filtering_non_file_tabs() {
        let mut app = App::test_new();
        let settings_tab = || {
            let mut tab = Tab::new_file(Editor::new());
            tab.kind = TabKind::Settings;
            tab
        };
        app.tabs = vec![
            settings_tab(),
            Tab::new_file(Editor::new()),
            Tab::new_file(Editor::new()),
        ];
        app.active_tab = 1;
        let mut group = EditorGroup::new();
        group.tabs = vec![settings_tab(), Tab::new_file(Editor::new())];
        group.active_tab = 1;
        app.groups.push(group);

        let session = app.build_session();

        assert_eq!(session.tabs.len(), 2);
        assert_eq!(session.active_tab, 0);
        assert_eq!(session.groups[0].tabs.len(), 1);
        assert_eq!(session.groups[0].active_tab, 0);
    }

    #[test]
    fn restore_session_preserves_empty_group_slots_and_nested_split_layout() {
        use crate::app::{SplitDirection, SplitNode};

        let mut source = App::test_new();
        source.tabs.push(Tab::new_file(Editor::new()));
        source.groups = vec![EditorGroup::new(), EditorGroup::new(), EditorGroup::new()];
        source.groups[0].tabs.push(Tab::new_file(Editor::new()));
        source.active_group = 1;
        source.split_tree = Some(SplitNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![
                SplitNode::Leaf(0),
                SplitNode::Split {
                    direction: SplitDirection::Vertical,
                    children: vec![SplitNode::Leaf(1), SplitNode::Leaf(2)],
                    ratios: vec![0.4, 0.6],
                },
            ],
            ratios: vec![0.3, 0.7],
        });
        let session: crate::session::Session =
            serde_json::from_str(&serde_json::to_string(&source.build_session()).unwrap()).unwrap();
        let mut restored = App::test_new();

        restored.restore_session(&session);
        wait_for_session_restore(&mut restored);

        assert_eq!(restored.groups.len(), 3);
        assert_eq!(restored.active_group, 1);
        assert_eq!(restored.tabs.len(), 1);
        assert_eq!(restored.groups[0].tabs.len(), 1);
        assert!(restored.groups[1].tabs.is_empty());
        assert!(restored.groups[2].tabs.is_empty());
        assert_eq!(restored.split_tree, source.split_tree);
        restored.switch_group(2);
        assert!(restored.tabs.is_empty());
        assert_eq!(restored.groups[1].tabs.len(), 1);
        restored.switch_group(1);
        assert_eq!(restored.tabs.len(), 1);
    }

    #[test]
    fn restore_session_rebuilds_invalid_or_legacy_split_tree() {
        use crate::app::{SplitDirection, SplitNode};

        let mut source = App::test_new();
        source.groups = vec![EditorGroup::new(), EditorGroup::new()];
        source.split_ratio = 0.3;
        let mut session = source.build_session();
        for tree in [None, Some(SplitNode::Leaf(99))] {
            session.split_tree = tree;
            let mut restored = App::test_new();
            restored.restore_session(&session);
            assert_eq!(
                restored.split_tree,
                Some(SplitNode::Split {
                    direction: SplitDirection::Horizontal,
                    children: vec![SplitNode::Leaf(0), SplitNode::Leaf(1)],
                    ratios: vec![0.3, 0.7],
                })
            );
        }
    }

    #[test]
    fn restore_session_ids_remain_unique_across_workspace_visits() {
        let mut source = App::test_new();
        source.tabs.push(Tab::new_file(Editor::new()));
        let session = source.build_session();
        let mut restored = App::test_new();
        restored.restore_session(&session);
        let first_id = restored.tabs[0].editor.pending_session_restore.as_ref().unwrap().id;
        restored.tabs.clear();

        restored.restore_session(&session);

        let second_id = restored.tabs[0].editor.pending_session_restore.as_ref().unwrap().id;
        assert_ne!(first_id, second_id);
        wait_for_session_restore(&mut restored);
        assert_eq!(restored.tabs.len(), 1);
        assert!(!restored.tabs[0].editor.bg_loading);
    }

    #[test]
    fn explicit_session_settings_preserve_layout_without_overrides() {
        let mut source = App::test_new();
        source.show_explorer = false;
        source.explorer_width = 45;
        source.show_minimap = false;
        source.show_panel = true;
        source.panel_height = 18;
        let session = source.build_session();
        let mut restored = App::test_new();
        restored.settings_global = Settings::default();
        restored.settings_workspace = Settings::default();
        restored.restore_session(&session);

        restored.apply_session_settings();

        assert!(!restored.show_explorer);
        assert_eq!(restored.explorer_width, 45);
        assert!(!restored.show_minimap);
        assert!(restored.show_panel);
        assert_eq!(restored.panel_height, 18);

        restored.settings_global.show_minimap = Some(true);
        restored.settings_global.show_panel = Some(false);
        restored.settings_workspace.show_panel = Some(true);
        restored.apply_session_settings();

        assert!(restored.show_minimap);
        assert!(restored.show_panel);
        assert_eq!(restored.explorer_width, 45);
        assert_eq!(restored.panel_height, 18);
    }

    #[test]
    fn explicit_session_settings_keep_pending_restores_read_only() {
        let mut source = App::test_new();
        source.tabs.push(Tab::new_file(Editor::new()));
        source.groups = vec![EditorGroup::new(), EditorGroup::new()];
        source.groups[1].tabs.push(Tab::new_file(Editor::new()));
        let mut restored = App::test_new();
        restored.settings_global = Settings::default();
        restored.settings_workspace = Settings {
            read_only: Some(false),
            ..Settings::default()
        };
        restored.restore_session(&source.build_session());

        restored.apply_session_settings();

        assert!(!restored.default_read_only);
        assert!(restored.tabs[0].editor.read_only);
        assert!(restored.groups[1].tabs[0].editor.read_only);
        wait_for_session_restore(&mut restored);
        assert!(!restored.tabs[0].editor.read_only);
        assert!(!restored.groups[1].tabs[0].editor.read_only);
    }

    #[test]
    fn saving_restored_layout_does_not_create_settings_overrides() {
        let mut source = App::test_new();
        source.show_explorer = false;
        source.explorer_width = 48;
        source.show_panel = true;
        source.panel_height = 19;
        assert!(source.set_theme_by_key("light"));
        let session = source.build_session();

        for scope in [
            crate::settings::SettingsScope::Global,
            crate::settings::SettingsScope::Workspace,
        ] {
            let mut restored = App::test_new();
            restored.settings_global = Settings {
                tab_size: Some(8),
                ..Settings::default()
            };
            restored.settings_workspace = Settings {
                use_tabs: Some(true),
                ..Settings::default()
            };
            restored.initialize_settings();
            restored.settings_scope = scope;
            let global = restored.settings_global.clone();
            let workspace = restored.settings_workspace.clone();
            restored.restore_session(&session);
            restored.apply_session_settings();

            restored.save_settings();

            assert_eq!(restored.settings_global, global);
            assert_eq!(restored.settings_workspace, workspace);
            assert_eq!(restored.settings_effective.theme.as_deref(), Some("light"));
            assert!(!restored.show_explorer);
            assert_eq!(restored.explorer_width, 48);
            assert!(restored.show_panel);
            assert_eq!(restored.panel_height, 19);
        }
    }

    #[test]
    fn restore_session_preserves_zen_mode_and_layout_to_restore_on_exit() {
        let mut source = App::test_new();
        source.show_explorer = false;
        source.show_minimap = true;
        source.show_panel = true;
        source.show_activity_bar = false;
        source.execute_palette_command("toggle_zen");
        let session = source.build_session();
        assert!(!session.show_explorer);
        assert!(session.show_minimap);
        assert!(session.show_panel);
        let mut restored = App::test_new();
        restored.settings_global = Settings::default();
        restored.settings_workspace = Settings {
            show_explorer: Some(true),
            show_minimap: Some(false),
            ..Settings::default()
        };
        restored.restore_session(&session);

        restored.apply_session_settings();

        assert!(restored.zen_mode);
        assert!(!restored.show_explorer);
        assert!(!restored.show_minimap);
        assert!(!restored.show_panel);
        assert!(!restored.show_tab_bar);
        assert!(!restored.show_status_bar);
        assert!(!restored.show_activity_bar);
        restored.execute_palette_command("toggle_zen");
        assert!(restored.show_explorer);
        assert!(!restored.show_minimap);
        assert!(restored.show_panel);
        assert!(restored.show_tab_bar);
        assert!(restored.show_status_bar);
        assert!(!restored.show_activity_bar);
    }

    // --- current_settings tests ---

    #[test]
    fn unrelated_settings_save_preserves_animation_inheritance_and_legacy_fallbacks() {
        use crate::animation_config::AnimationEffect;
        for scope in [
            crate::settings::SettingsScope::Global,
            crate::settings::SettingsScope::Workspace,
        ] {
            let mut app = App::test_new();
            app.settings_global.animation_duration_ms = Some(430);
            app.settings_global.animation_intensity = Some(74);
            app.settings_workspace.animation_scope_bubble_duration_ms = Some(730);
            app.apply_effective_settings();
            app.settings_scope = scope;
            app.cursor_blinking = !app.cursor_blinking;
            app.save_settings();
            for effect in AnimationEffect::ALL {
                assert_eq!(app.settings_global.animation_effect_duration(effect), None);
                assert_eq!(app.settings_global.animation_effect_intensity(effect), None);
                assert_eq!(
                    app.settings_workspace.animation_effect_duration(effect),
                    if effect == AnimationEffect::ScopeConnector {
                        Some(730)
                    } else {
                        None
                    }
                );
                assert_eq!(
                    app.settings_workspace.animation_effect_intensity(effect),
                    None
                );
            }
            assert_eq!(app.settings_global.animation_duration_ms, Some(430));
            assert_eq!(app.settings_global.animation_intensity, Some(74));
            app.settings_global.animation_duration_ms = Some(560);
            app.settings_global.animation_intensity = Some(82);
            app.apply_effective_settings();
            for effect in AnimationEffect::ALL {
                assert_eq!(
                    app.animation_settings.duration(effect),
                    if effect == AnimationEffect::ScopeConnector {
                        730
                    } else {
                        560
                    }
                );
                assert_eq!(app.animation_settings.intensity(effect), 82);
            }
        }
    }

    #[test]
    fn live_effect_setting_save_persists_only_changed_effect_fields() {
        use crate::animation_config::AnimationEffect;
        let mut app = App::test_new();
        app.apply_effective_settings();
        app.settings_scope = crate::settings::SettingsScope::Workspace;
        app.animation_settings
            .set_duration(AnimationEffect::CursorFire, 680);
        app.animation_settings
            .set_intensity(AnimationEffect::ScopeConnector, 91);
        app.save_settings();
        for effect in AnimationEffect::ALL {
            assert_eq!(
                app.settings_workspace.animation_effect_duration(effect),
                if effect == AnimationEffect::CursorFire {
                    Some(680)
                } else {
                    None
                }
            );
            assert_eq!(
                app.settings_workspace.animation_effect_intensity(effect),
                if effect == AnimationEffect::ScopeConnector {
                    Some(91)
                } else {
                    None
                }
            );
            assert_eq!(app.settings_global.animation_effect_duration(effect), None);
            assert_eq!(app.settings_global.animation_effect_intensity(effect), None);
        }
        assert_eq!(app.settings_workspace.animation_duration_ms, None);
        assert_eq!(app.settings_workspace.animation_intensity, None);
    }

    #[test]
    fn current_settings_defaults() {
        let app = App::test_new();
        let s = app.current_settings();
        // No editor open, so tab_size defaults to 4
        assert_eq!(s.tab_size, Some(4));
        assert_eq!(s.use_tabs, Some(false));
        assert_eq!(s.word_wrap, Some(false));
        assert_eq!(s.whitespace_mode, Some("boundary".to_string()));
        assert_eq!(s.comment_whitespace_mode.as_deref(), Some("all"));
        assert_eq!(s.show_explorer, Some(true));
        assert_eq!(s.show_minimap, Some(true));
        assert_eq!(s.show_line_numbers, Some(true));
        assert_eq!(s.relative_line_numbers, Some(false));
        assert_eq!(s.show_indent_guides, Some(true));
        assert_eq!(s.show_panel, Some(false));
        assert_eq!(s.panel_height, Some(10));
        assert_eq!(s.sidebar_right, Some(false));
        assert_eq!(s.explorer_width, Some(30));
        assert_eq!(s.show_activity_bar, Some(true));
        assert_eq!(s.show_breadcrumbs, Some(true));
        assert_eq!(s.show_status_bar, Some(true));
        assert_eq!(s.show_tab_bar, Some(true));
        assert_eq!(s.show_menu_bar, Some(true));
    }

    #[test]
    fn current_settings_feature_defaults() {
        let app = App::test_new();
        let s = app.current_settings();
        assert_eq!(s.indent_rainbow, Some(false));
        assert_eq!(s.color_preview, Some(false));
        assert_eq!(s.bracket_pair_colorization, Some(true));
        assert_eq!(s.error_lens, Some(false));
        assert_eq!(s.hover_enabled, Some(true));
        assert_eq!(s.quick_suggestions, Some(true));
        assert_eq!(s.inlay_hints, Some(true));
        assert_eq!(s.code_lens, Some(true));
        assert_eq!(s.semantic_highlighting, Some(true));
        assert_eq!(s.format_on_type, Some(false));
        assert_eq!(s.sticky_scroll, Some(true));
        assert_eq!(s.sticky_scroll_max, Some(3));
        assert_eq!(s.scroll_past_end, Some(true));
        assert_eq!(s.focus_mode, Some(false));
        assert_eq!(s.show_word_count, Some(false));
        assert_eq!(s.screencast_mode, Some(false));
        assert_eq!(s.max_global_search_results, Some(20_000));
    }

    #[test]
    fn current_settings_save_cursor_defaults() {
        let app = App::test_new();
        let s = app.current_settings();
        assert_eq!(s.trim_trailing_on_save, Some(true));
        assert_eq!(s.format_on_save, Some(false));
        assert_eq!(s.insert_final_newline, Some(false));
        assert_eq!(s.cursor_style, Some("line".to_string()));
        assert_eq!(s.cursor_blinking, Some(true));
        assert_eq!(s.cursor_surrounding_lines, Some(5));
        assert_eq!(s.auto_save, Some("off".to_string()));
        assert_eq!(s.auto_save_delay, Some(1000));
        assert_eq!(s.scroll_speed, Some(3));
        assert_eq!(s.search_case_sensitive, Some(false));
        assert_eq!(s.search_whole_word, Some(false));
        assert_eq!(s.search_regex, Some(false));
        assert_eq!(s.search_preserve_case, Some(false));
    }

    #[test]
    fn current_settings_no_keybindings_when_unmodified() {
        let app = App::test_new();
        let s = app.current_settings();
        assert!(s.keybindings.is_none());
    }

    #[test]
    fn current_settings_no_language_associations_when_empty() {
        let app = App::test_new();
        let s = app.current_settings();
        assert!(s.language_associations.is_none());
    }

    #[test]
    fn current_settings_include_settings_view_defaults_without_editor() {
        let mut app = App::test_new();
        app.tabs.clear();
        app.default_tab_size = 8;
        app.default_use_tabs = true;
        app.default_word_wrap = true;
        app.default_whitespace_mode = crate::app::WhitespaceMode::All;
        app.default_read_only = true;
        app.show_open_editors = false;
        app.minimap_width = 15;

        let s = app.current_settings();

        assert_eq!(s.tab_size, Some(8));
        assert_eq!(s.use_tabs, Some(true));
        assert_eq!(s.word_wrap, Some(true));
        assert_eq!(s.whitespace_mode.as_deref(), Some("all"));
        assert_eq!(s.read_only, Some(true));
        assert_eq!(s.show_open_editors, Some(false));
        assert_eq!(s.minimap_width, Some(15));
    }

    // --- apply_settings tests ---

    #[test]
    fn apply_settings_toggles_booleans() {
        let mut app = App::test_new();
        let mut s = app.current_settings();
        s.show_explorer = Some(false);
        s.show_minimap = Some(false);
        s.show_line_numbers = Some(false);
        s.show_indent_guides = Some(false);
        s.show_panel = Some(true);
        s.show_activity_bar = Some(false);
        s.sidebar_right = Some(true);
        s.show_breadcrumbs = Some(false);
        s.show_status_bar = Some(false);
        s.show_tab_bar = Some(false);
        s.show_menu_bar = Some(false);
        app.apply_settings(&s);

        assert!(!app.show_explorer);
        assert!(!app.show_minimap);
        assert!(!app.show_line_numbers);
        assert!(!app.show_indent_guides);
        assert!(app.show_panel);
        assert!(app.sidebar_right);
        assert!(!app.show_activity_bar);
        assert!(!app.show_breadcrumbs);
        assert!(!app.show_status_bar);
        assert!(!app.show_tab_bar);
        assert!(!app.show_menu_bar);
    }

    #[test]
    fn apply_settings_feature_flags() {
        let mut app = App::test_new();
        let mut s = app.current_settings();
        s.indent_rainbow = Some(true);
        s.color_preview = Some(false);
        s.bracket_pair_colorization = Some(false);
        s.error_lens = Some(false);
        s.hover_enabled = Some(false);
        s.quick_suggestions = Some(false);
        s.inlay_hints = Some(false);
        s.code_lens = Some(false);
        s.semantic_highlighting = Some(false);
        s.format_on_type = Some(true);
        s.sticky_scroll = Some(false);
        s.scroll_past_end = Some(true);
        s.focus_mode = Some(true);
        s.show_word_count = Some(true);
        s.screencast_mode = Some(true);
        app.apply_settings(&s);

        assert!(app.indent_rainbow);
        assert!(!app.show_color_preview);
        assert!(!app.bracket_pair_colorization);
        assert!(!app.error_lens);
        assert!(!app.hover_enabled);
        assert!(!app.quick_suggestions);
        assert!(!app.show_inlay_hints);
        assert!(!app.show_code_lenses);
        assert!(!app.show_semantic_tokens);
        assert!(app.on_type_formatting_enabled);
        assert!(!app.sticky_scroll);
        assert!(app.scroll_past_end);
        assert!(app.focus_mode);
        assert!(app.show_word_count);
        assert!(app.screencast_mode);
    }

    #[test]
    fn apply_settings_numeric_fields() {
        let mut app = App::test_new();
        let mut s = app.current_settings();
        s.panel_height = Some(25);
        s.minimap_width = Some(18);
        s.explorer_width = Some(45);
        s.word_wrap_column = Some(120);
        s.sticky_scroll_max = Some(5);
        s.auto_save_delay = Some(2000);
        s.scroll_speed = Some(7);
        app.apply_settings(&s);

        assert_eq!(app.panel_height, 25);
        assert_eq!(app.minimap_width, 18);
        assert_eq!(app.explorer_width, 45);
        assert_eq!(app.word_wrap_column, 120);
        assert_eq!(app.sticky_scroll_max, 5);
        assert_eq!(app.auto_save_delay, 2000);
        assert_eq!(app.scroll_speed, 7);
    }

    #[test]
    fn apply_settings_editor_defaults_propagate_to_existing_and_new_editors() {
        let mut app = App::test_new();
        app.tabs.push(crate::app::Tab::new_file(Editor::new()));
        app.active_tab = 0;
        let settings = Settings {
            tab_size: Some(8),
            use_tabs: Some(true),
            word_wrap: Some(true),
            word_wrap_column: Some(120),
            whitespace_mode: Some("all".to_string()),
            read_only: Some(true),
            ..Default::default()
        };

        app.apply_settings(&settings);

        let active = app.editor().unwrap();
        assert_eq!(app.default_tab_size, 8);
        assert!(app.default_use_tabs);
        assert!(app.default_word_wrap);
        assert_eq!(app.word_wrap_column, 120);
        assert_eq!(app.default_whitespace_mode, crate::app::WhitespaceMode::All);
        assert!(app.default_read_only);
        assert_eq!(active.tab_size, 8);
        assert!(active.use_tabs);
        assert!(active.word_wrap);
        assert_eq!(active.word_wrap_column, 120);
        assert_eq!(active.whitespace_mode, crate::app::WhitespaceMode::All);
        assert!(active.read_only);

        let new_editor = app.new_editor();
        assert_eq!(new_editor.tab_size, 8);
        assert!(new_editor.use_tabs);
        assert!(new_editor.word_wrap);
        assert_eq!(new_editor.word_wrap_column, 120);
        assert_eq!(new_editor.whitespace_mode, crate::app::WhitespaceMode::All);
        assert!(new_editor.read_only);
    }

    #[test]
    fn reapplying_settings_after_session_restore_wins_over_session_ui_state() {
        let mut app = App::test_new();
        let settings = Settings {
            show_explorer: Some(false),
            show_open_editors: Some(false),
            show_minimap: Some(false),
            minimap_width: Some(20),
            show_line_numbers: Some(false),
            show_panel: Some(true),
            panel_height: Some(20),
            sidebar_right: Some(true),
            show_status_bar: Some(false),
            show_tab_bar: Some(false),
            zen_mode: Some(false),
            ..Default::default()
        };
        app.apply_settings(&settings);

        let mut session_app = App::test_new();
        session_app.show_explorer = true;
        session_app.show_open_editors = true;
        session_app.show_minimap = true;
        session_app.minimap_width = 8;
        session_app.show_line_numbers = true;
        session_app.show_panel = false;
        session_app.panel_height = 10;
        session_app.sidebar_right = false;
        session_app.show_activity_bar = true;
        session_app.show_status_bar = true;
        session_app.show_tab_bar = true;
        session_app.zen_mode = true;
        let session = session_app.build_session();

        app.restore_session(&session);
        app.apply_settings(&settings);

        assert!(!app.show_explorer);
        assert!(!app.show_open_editors);
        assert!(!app.show_minimap);
        assert_eq!(app.minimap_width, 20);
        assert!(!app.show_line_numbers);
        assert!(app.show_panel);
        assert_eq!(app.panel_height, 20);
        assert!(app.sidebar_right);
        assert!(app.show_activity_bar);
        assert!(!app.show_status_bar);
        assert!(!app.show_tab_bar);
        assert!(!app.zen_mode);
    }

    #[test]
    fn settings_view_roundtrip_preserves_saved_values() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        let settings = Settings {
            tab_size: Some(8),
            use_tabs: Some(true),
            word_wrap: Some(true),
            word_wrap_column: Some(0),
            whitespace_mode: Some("all".to_string()),
            comment_whitespace_mode: Some("inherit".to_string()),
            read_only: Some(true),
            theme: Some("dark".to_string()),
            show_explorer: Some(false),
            show_open_editors: Some(false),
            show_minimap: Some(false),
            minimap_width: Some(20),
            show_line_numbers: Some(false),
            relative_line_numbers: Some(true),
            show_indent_guides: Some(false),
            show_panel: Some(true),
            panel_height: Some(20),
            sidebar_right: Some(true),
            explorer_width: Some(45),
            show_activity_bar: Some(false),
            show_breadcrumbs: Some(false),
            show_status_bar: Some(false),
            show_tab_bar: Some(false),
            show_menu_bar: Some(false),
            zen_mode: Some(false),
            indent_rainbow: Some(true),
            color_preview: Some(false),
            bracket_pair_colorization: Some(false),
            error_lens: Some(false),
            hover_enabled: Some(false),
            quick_suggestions: Some(false),
            inlay_hints: Some(false),
            code_lens: Some(false),
            semantic_highlighting: Some(false),
            format_on_type: Some(true),
            sticky_scroll: Some(false),
            sticky_scroll_max: Some(5),
            scroll_past_end: Some(true),
            focus_mode: Some(true),
            show_word_count: Some(true),
            screencast_mode: Some(true),
            max_global_search_results: Some(5000),
            rulers: Some(vec![80, 120]),
            trim_trailing_on_save: Some(false),
            format_on_save: Some(true),
            insert_final_newline: Some(false),
            cursor_style: Some("underline".to_string()),
            cursor_blinking: Some(false),
            cursor_surrounding_lines: Some(10),
            auto_save: Some("onFocusChange".to_string()),
            auto_save_delay: Some(5000),
            scroll_speed: Some(7),
            search_case_sensitive: Some(true),
            search_regex: Some(true),
            search_whole_word: Some(true),
            search_preserve_case: Some(true),
            ..Default::default()
        };
        crate::settings::save_to_file(&path, &settings).expect("save settings");
        let mut loaded = crate::settings::load_from_file(&path);
        loaded.validate();

        let mut app = App::test_new();
        app.apply_settings(&loaded);
        let after = app.current_settings();

        assert_eq!(after.tab_size, Some(8));
        assert_eq!(after.use_tabs, Some(true));
        assert_eq!(after.word_wrap, Some(true));
        assert_eq!(after.word_wrap_column, Some(0));
        assert_eq!(after.whitespace_mode.as_deref(), Some("all"));
        assert_eq!(after.comment_whitespace_mode.as_deref(), Some("inherit"));
        assert_eq!(after.read_only, Some(true));
        assert_eq!(after.theme.as_deref(), Some("dark"));
        assert_eq!(after.show_explorer, Some(false));
        assert_eq!(after.show_open_editors, Some(false));
        assert_eq!(after.show_minimap, Some(false));
        assert_eq!(after.minimap_width, Some(20));
        assert_eq!(after.show_line_numbers, Some(false));
        assert_eq!(after.relative_line_numbers, Some(true));
        assert_eq!(after.show_indent_guides, Some(false));
        assert_eq!(after.show_panel, Some(true));
        assert_eq!(after.panel_height, Some(20));
        assert_eq!(after.sidebar_right, Some(true));
        assert_eq!(after.explorer_width, Some(45));
        assert_eq!(after.show_activity_bar, Some(false));
        assert_eq!(after.show_breadcrumbs, Some(false));
        assert_eq!(after.show_status_bar, Some(false));
        assert_eq!(after.show_tab_bar, Some(false));
        assert_eq!(after.show_menu_bar, Some(false));
        assert_eq!(after.zen_mode, Some(false));
        assert_eq!(after.indent_rainbow, Some(true));
        assert_eq!(after.color_preview, Some(false));
        assert_eq!(after.bracket_pair_colorization, Some(false));
        assert_eq!(after.error_lens, Some(false));
        assert_eq!(after.hover_enabled, Some(false));
        assert_eq!(after.quick_suggestions, Some(false));
        assert_eq!(after.inlay_hints, Some(false));
        assert_eq!(after.code_lens, Some(false));
        assert_eq!(after.semantic_highlighting, Some(false));
        assert_eq!(after.format_on_type, Some(true));
        assert_eq!(after.sticky_scroll, Some(false));
        assert_eq!(after.sticky_scroll_max, Some(5));
        assert_eq!(after.scroll_past_end, Some(true));
        assert_eq!(after.focus_mode, Some(true));
        assert_eq!(after.show_word_count, Some(true));
        assert_eq!(after.screencast_mode, Some(true));
        assert_eq!(after.max_global_search_results, Some(5000));
        assert_eq!(after.rulers, Some(vec![80, 120]));
        assert_eq!(after.trim_trailing_on_save, Some(false));
        assert_eq!(after.format_on_save, Some(true));
        assert_eq!(after.insert_final_newline, Some(false));
        assert_eq!(after.cursor_surrounding_lines, Some(10));
        assert_eq!(after.search_case_sensitive, Some(true));
        assert_eq!(after.search_regex, Some(true));
        assert_eq!(after.search_whole_word, Some(true));
        assert_eq!(after.search_preserve_case, Some(true));
        assert_eq!(after.cursor_style.as_deref(), Some("underline"));
        assert_eq!(after.cursor_blinking, Some(false));
        assert_eq!(after.auto_save.as_deref(), Some("onFocusChange"));
        assert_eq!(after.auto_save_delay, Some(5000));
        assert_eq!(after.scroll_speed, Some(7));
    }

    #[test]
    fn apply_settings_clamps_sticky_scroll_max() {
        let mut app = App::test_new();
        let mut s = app.current_settings();
        s.sticky_scroll_max = Some(100);
        app.apply_settings(&s);
        assert_eq!(app.sticky_scroll_max, 5);

        s.sticky_scroll_max = Some(0);
        app.apply_settings(&s);
        assert_eq!(app.sticky_scroll_max, 1);
    }

    #[test]
    fn apply_settings_clamps_scroll_speed() {
        let mut app = App::test_new();
        let mut s = app.current_settings();
        s.scroll_speed = Some(100);
        app.apply_settings(&s);
        assert_eq!(app.scroll_speed, 10);

        s.scroll_speed = Some(0);
        app.apply_settings(&s);
        assert_eq!(app.scroll_speed, 1);
    }

    #[test]
    fn apply_settings_clamps_auto_save_delay() {
        let mut app = App::test_new();
        let mut s = app.current_settings();
        s.auto_save_delay = Some(10);
        app.apply_settings(&s);
        assert_eq!(app.auto_save_delay, 100);
    }

    #[test]
    fn apply_settings_preserves_max_global_search_results() {
        let mut app = App::test_new();
        let mut s = app.current_settings();
        s.max_global_search_results = Some(50);
        app.apply_settings(&s);
        assert_eq!(app.max_global_search_results, 50);

        s.max_global_search_results = Some(50_000);
        app.apply_settings(&s);
        assert_eq!(app.max_global_search_results, 50_000);
    }

    #[test]
    fn apply_settings_cursor_style_variants() {
        let mut app = App::test_new();
        let mut s = app.current_settings();

        s.cursor_style = Some("block".to_string());
        app.apply_settings(&s);
        assert_eq!(app.cursor_style, CursorStyle::Block);

        s.cursor_style = Some("underline".to_string());
        app.apply_settings(&s);
        assert_eq!(app.cursor_style, CursorStyle::Underline);

        s.cursor_style = Some("line".to_string());
        app.apply_settings(&s);
        assert_eq!(app.cursor_style, CursorStyle::Line);

        s.cursor_style = Some("unknown".to_string());
        app.apply_settings(&s);
        assert_eq!(app.cursor_style, CursorStyle::Line);
    }

    #[test]
    fn apply_settings_auto_save_variants() {
        let mut app = App::test_new();
        let mut s = app.current_settings();

        s.auto_save = Some("afterDelay".to_string());
        app.apply_settings(&s);
        assert_eq!(app.auto_save, AutoSaveMode::AfterDelay);

        s.auto_save = Some("onFocusChange".to_string());
        app.apply_settings(&s);
        assert_eq!(app.auto_save, AutoSaveMode::OnFocusChange);

        s.auto_save = Some("onWindowChange".to_string());
        app.apply_settings(&s);
        assert_eq!(app.auto_save, AutoSaveMode::OnWindowChange);

        s.auto_save = Some("off".to_string());
        app.apply_settings(&s);
        assert_eq!(app.auto_save, AutoSaveMode::Off);

        s.auto_save = Some("unknown".to_string());
        app.apply_settings(&s);
        assert_eq!(app.auto_save, AutoSaveMode::Off);
    }

    #[test]
    fn apply_settings_roundtrip() {
        let mut app = App::test_new();
        let original = app.current_settings();
        let mut modified = original.clone();
        modified.show_minimap = Some(false);
        modified.sidebar_right = Some(true);
        modified.panel_height = Some(20);
        modified.indent_rainbow = Some(true);
        app.apply_settings(&modified);

        let after = app.current_settings();
        assert_eq!(after.show_minimap, Some(false));
        assert_eq!(after.sidebar_right, Some(true));
        assert_eq!(after.panel_height, Some(20));
        assert_eq!(after.indent_rainbow, Some(true));
    }

    #[test]
    fn apply_settings_rulers() {
        let mut app = App::test_new();
        let mut s = app.current_settings();
        s.rulers = Some(vec![80, 120]);
        app.apply_settings(&s);
        assert_eq!(app.rulers, vec![80, 120]);
    }

    #[test]
    fn apply_settings_language_associations() {
        let mut app = App::test_new();
        let mut s = app.current_settings();
        let mut assoc = std::collections::HashMap::new();
        assoc.insert(".vtc".to_string(), "rust".to_string());
        s.language_associations = Some(assoc.clone());
        app.apply_settings(&s);
        let expected: rustc_hash::FxHashMap<String, String> = assoc.into_iter().collect();
        assert_eq!(app.language_associations, expected);
    }

    // --- settings_filtered_indices_for_category tests ---

    #[test]
    fn filtered_indices_out_of_bounds_category() {
        let app = App::test_new();
        let result = app.settings_filtered_indices_for_category(999);
        assert!(result.is_empty());
    }

    #[test]
    fn collapsed_navigation_branch_keeps_category_settings_visible() {
        let mut app = App::test_new();
        app.settings_category_collapsed[0] = true;
        let result = app.settings_filtered_indices_for_category(0);
        assert!(!result.is_empty());
    }

    #[test]
    fn filtered_indices_expanded_category_nonempty() {
        let app = App::test_new();
        // Category 0 (Commonly Used) should have items when not collapsed
        let result = app.settings_filtered_indices_for_category(0);
        assert!(!result.is_empty());
    }

    #[test]
    fn filtered_indices_common_category_excludes_headings() {
        let app = App::test_new();
        let result = app.settings_filtered_indices_for_category(0);
        assert!(!result.is_empty());
        let items = app.settings_items();
        assert!(result
            .iter()
            .all(|&idx| !items[idx].2.starts_with("heading_")));
    }

    #[test]
    fn filtered_indices_search_narrows_results() {
        let mut app = App::test_new();
        // Search spans every category, so compare it with the full editable
        // settings set rather than the smaller Commonly Used category.
        let items = app.settings_items();
        let all_editable = items
            .iter()
            .filter(|(_, _, id)| !id.starts_with("heading_"))
            .count();
        app.settings_editor_search = "tab".to_string();
        let filtered = app.settings_filtered_indices_for_category(0);
        assert!(filtered.len() < all_editable);
        assert!(!filtered.is_empty());
        let ids = filtered
            .iter()
            .map(|&index| items[index].2.as_str())
            .collect::<Vec<_>>();
        assert!(ids.contains(&"tab_size"));
        assert!(ids.contains(&"animation_tab_slide_duration_ms"));
        assert!(!ids.contains(&"animation_fps"));
    }
}
