use crate::animation_config::clamp_animation_duration_ms;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SettingsScope {
    #[default]
    Global,
    Workspace,
}

impl SettingsScope {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::Workspace => "Workspace",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct KeybindingSetting {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modifiers: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_code: Option<String>,
}

/// Persistent settings, serialized to/from JSON.
/// Mirrors VS Code's settings.json approach:
///   - Global: <vtcode-base>/settings.json, beside themes/ and syntaxes/
///   - Per-workspace: <vtcode-base>/.vtcode/workspaces/<id>/settings.json
///
/// Workspace settings override global settings (field by field).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_control_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_control_show_untracked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lsp_diagnostic_delay_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) lsp_servers: Option<Vec<crate::lsp::LspServerConfig>>,
    /// User-configured ACP executables. Connections are started explicitly.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub acp_agents: Option<Vec<vtcode::acp::AgentConfig>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_output_detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_thinking_expanded: Option<bool>,
    // Editor
    pub tab_size: Option<u8>,
    pub use_tabs: Option<bool>,
    pub word_wrap: Option<bool>,
    pub word_wrap_column: Option<usize>,
    pub whitespace_mode: Option<String>,
    pub comment_whitespace_mode: Option<String>,
    pub read_only: Option<bool>,

    // Appearance
    pub theme: Option<String>,
    #[serde(rename = "workbench.colorCustomizations", skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub workbench_color_customizations: std::collections::BTreeMap<String, serde_json::Value>,
    pub show_explorer: Option<bool>,
    pub show_open_editors: Option<bool>,
    pub show_minimap: Option<bool>,
    pub minimap_width: Option<u16>,
    pub show_line_numbers: Option<bool>,
    pub relative_line_numbers: Option<bool>,
    pub show_indent_guides: Option<bool>,
    pub show_panel: Option<bool>,
    pub panel_height: Option<u16>,
    pub sidebar_right: Option<bool>,
    pub explorer_width: Option<u16>,
    pub show_activity_bar: Option<bool>,
    pub show_breadcrumbs: Option<bool>,
    pub show_status_bar: Option<bool>,
    /// Per-item placement: true keeps an item on the bar, false puts it in
    /// the overflow menu. Unspecified items use the built-in defaults.
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub status_bar_items: std::collections::BTreeMap<String, bool>,
    pub show_tab_bar: Option<bool>,
    pub enable_preview_tabs: Option<bool>,
    #[serde(rename = "workbench.editor.mouseBackForwardToNavigate", alias = "mouseBackForwardToNavigate")]
    pub mouse_back_forward_to_navigate: Option<bool>,
    pub show_menu_bar: Option<bool>,
    pub zen_mode: Option<bool>,

    // Features
    pub indent_rainbow: Option<bool>,
    pub color_preview: Option<bool>,
    pub bracket_pair_colorization: Option<bool>,
    pub error_lens: Option<bool>,
    pub hover_enabled: Option<bool>,
    pub quick_suggestions: Option<bool>,
    pub inlay_hints: Option<bool>,
    pub code_lens: Option<bool>,
    pub semantic_highlighting: Option<bool>,
    pub format_on_type: Option<bool>,
    pub sticky_scroll: Option<bool>,
    pub sticky_scroll_max: Option<usize>,
    pub scroll_past_end: Option<bool>,
    pub focus_mode: Option<bool>,
    pub show_word_count: Option<bool>,
    pub screencast_mode: Option<bool>,
    #[serde(alias = "search.maxResults")]
    pub max_global_search_results: Option<usize>,
    pub large_file_threshold_mb: Option<u64>,
    pub large_file_analysis_timeout_ms: Option<u64>,
    pub rulers: Option<Vec<u16>>,

    // Save behavior
    pub trim_trailing_on_save: Option<bool>,
    pub format_on_save: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) formatters: Option<std::collections::BTreeMap<String, crate::formatters::Config>>,
    pub insert_final_newline: Option<bool>,

    // Cursor / auto-save
    pub cursor_style: Option<String>,
    pub cursor_blinking: Option<bool>,
    pub cursor_surrounding_lines: Option<usize>,
    pub auto_save: Option<String>,
    pub auto_save_delay: Option<u64>,
    pub scroll_speed: Option<usize>,
    pub keybindings: Option<Vec<KeybindingSetting>>,

    // Copy feedback is on by default; decorative effects are opt-in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_copy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_copy_duration_ms: Option<u64>,
    // Optional animations inherit independently.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_cursor_smear: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_cursor_fire: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_whitespace_reveal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_scope_connector: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_tab_slide: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_fps: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_intensity: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_cursor_smear_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_cursor_smear_intensity: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_cursor_fire_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_cursor_fire_intensity: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_whitespace_reveal_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_whitespace_reveal_intensity: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_scope_bubble_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_scope_bubble_intensity: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_tab_slide_duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_tab_slide_intensity: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_scope_bubble_tight: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_scope_bubble_fill: Option<bool>,
    /// Case-insensitive language names or IDs for which Scope Bubble is disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_scope_bubble_disabled_languages: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_scope_bubble_languages: Option<crate::scope_bubble_config::Configs>,
    // Retired preferences are accepted from existing files, but never applied
    // by AnimationSettings or exposed by the settings editor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_scope_bubble_glow: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation_scope_bubble_glow_duration_ms: Option<u64>,

    // Search preferences
    pub search_case_sensitive: Option<bool>,
    pub search_regex: Option<bool>,
    pub search_whole_word: Option<bool>,
    pub search_preserve_case: Option<bool>,
    #[serde(alias = "search.useIgnoreFiles")]
    pub search_use_ignore_files: Option<bool>,
    #[serde(alias = "search.useGlobalIgnoreFiles")]
    pub search_use_global_ignore_files: Option<bool>,
    #[serde(alias = "search.useParentIgnoreFiles")]
    pub search_use_parent_ignore_files: Option<bool>,
    #[serde(alias = "search.followSymlinks")]
    pub search_follow_symlinks: Option<bool>,
    #[serde(rename = "files.exclude", skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub files_exclude: std::collections::BTreeMap<String, serde_json::Value>,
    #[serde(rename = "search.exclude", skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub search_exclude: std::collections::BTreeMap<String, serde_json::Value>,

    /// Custom file extension → language mappings. Keys are glob patterns like "*.xyz",
    /// "Dockerfile.*", or exact filenames like "Jenkinsfile". Values are language names
    /// like "Python", "JavaScript", etc.
    pub language_associations: Option<HashMap<String, String>>,

}

fn merge_keybinding_layers(
    base: &Option<Vec<KeybindingSetting>>,
    overrides: &Option<Vec<KeybindingSetting>>,
) -> Option<Vec<KeybindingSetting>> {
    let Some(overrides) = overrides else {
        return base.clone();
    };
    let mut merged = base.clone().unwrap_or_default();
    for entry in overrides {
        let matching_index = merged.iter().position(|existing| {
            entry
                .command
                .as_ref()
                .zip(existing.command.as_ref())
                .is_some_and(|(left, right)| left == right)
                || (entry.command.is_none()
                    && existing.command.is_none()
                    && entry.index.is_some()
                    && entry.index == existing.index)
        });
        if let Some(index) = matching_index {
            merged[index] = entry.clone();
        } else {
            merged.push(entry.clone());
        }
    }
    Some(merged)
}

impl Settings {
    pub(crate) fn animation_effect_duration(
        &self,
        effect: crate::animation_config::AnimationEffect,
    ) -> Option<u64> {
        match effect {
            crate::animation_config::AnimationEffect::CursorSmear => {
                self.animation_cursor_smear_duration_ms
            }
            crate::animation_config::AnimationEffect::CursorFire => {
                self.animation_cursor_fire_duration_ms
            }
            crate::animation_config::AnimationEffect::WhitespaceReveal => {
                self.animation_whitespace_reveal_duration_ms
            }
            crate::animation_config::AnimationEffect::ScopeConnector => {
                self.animation_scope_bubble_duration_ms
            }
            crate::animation_config::AnimationEffect::TabSlide => {
                self.animation_tab_slide_duration_ms
            }
        }
    }

    pub(crate) fn set_animation_effect_duration(
        &mut self,
        effect: crate::animation_config::AnimationEffect,
        value: u64,
    ) {
        match effect {
            crate::animation_config::AnimationEffect::CursorSmear => {
                self.animation_cursor_smear_duration_ms = Some(value)
            }
            crate::animation_config::AnimationEffect::CursorFire => {
                self.animation_cursor_fire_duration_ms = Some(value)
            }
            crate::animation_config::AnimationEffect::WhitespaceReveal => {
                self.animation_whitespace_reveal_duration_ms = Some(value)
            }
            crate::animation_config::AnimationEffect::ScopeConnector => {
                self.animation_scope_bubble_duration_ms = Some(value)
            }
            crate::animation_config::AnimationEffect::TabSlide => {
                self.animation_tab_slide_duration_ms = Some(value)
            }
        }
    }

    pub(crate) fn animation_effect_intensity(
        &self,
        effect: crate::animation_config::AnimationEffect,
    ) -> Option<u8> {
        match effect {
            crate::animation_config::AnimationEffect::CursorSmear => {
                self.animation_cursor_smear_intensity
            }
            crate::animation_config::AnimationEffect::CursorFire => {
                self.animation_cursor_fire_intensity
            }
            crate::animation_config::AnimationEffect::WhitespaceReveal => {
                self.animation_whitespace_reveal_intensity
            }
            crate::animation_config::AnimationEffect::ScopeConnector => {
                self.animation_scope_bubble_intensity
            }
            crate::animation_config::AnimationEffect::TabSlide => {
                self.animation_tab_slide_intensity
            }
        }
    }

    pub(crate) fn set_animation_effect_intensity(
        &mut self,
        effect: crate::animation_config::AnimationEffect,
        value: u8,
    ) {
        match effect {
            crate::animation_config::AnimationEffect::CursorSmear => {
                self.animation_cursor_smear_intensity = Some(value)
            }
            crate::animation_config::AnimationEffect::CursorFire => {
                self.animation_cursor_fire_intensity = Some(value)
            }
            crate::animation_config::AnimationEffect::WhitespaceReveal => {
                self.animation_whitespace_reveal_intensity = Some(value)
            }
            crate::animation_config::AnimationEffect::ScopeConnector => {
                self.animation_scope_bubble_intensity = Some(value)
            }
            crate::animation_config::AnimationEffect::TabSlide => {
                self.animation_tab_slide_intensity = Some(value)
            }
        }
    }

    /// Merge another settings on top of self; non-None fields in `other` override self.
    pub(crate) fn merge(&self, other: &Settings) -> Settings {
        Settings {
            lsp_enabled: other.lsp_enabled.or(self.lsp_enabled),
            source_control_enabled: other.source_control_enabled.or(self.source_control_enabled),
            source_control_show_untracked: other.source_control_show_untracked.or(self.source_control_show_untracked),
            lsp_diagnostic_delay_ms: other.lsp_diagnostic_delay_ms.or(self.lsp_diagnostic_delay_ms),
            lsp_servers: other.lsp_servers.clone().or_else(|| self.lsp_servers.clone()),
            acp_agents: other.acp_agents.clone().or_else(|| self.acp_agents.clone()),
            chat_output_detail: other.chat_output_detail.clone().or_else(|| self.chat_output_detail.clone()),
            chat_thinking_expanded: other.chat_thinking_expanded.or(self.chat_thinking_expanded),
            tab_size: other.tab_size.or(self.tab_size),
            use_tabs: other.use_tabs.or(self.use_tabs),
            word_wrap: other.word_wrap.or(self.word_wrap),
            word_wrap_column: other.word_wrap_column.or(self.word_wrap_column),
            whitespace_mode: other.whitespace_mode.clone().or_else(|| self.whitespace_mode.clone()),
            comment_whitespace_mode: other.comment_whitespace_mode.clone().or_else(|| self.comment_whitespace_mode.clone()),
            read_only: other.read_only.or(self.read_only),
            theme: other.theme.clone().or_else(|| self.theme.clone()),
            workbench_color_customizations: crate::theme_customizations::merge(&self.workbench_color_customizations, &other.workbench_color_customizations),
            show_explorer: other.show_explorer.or(self.show_explorer),
            show_open_editors: other.show_open_editors.or(self.show_open_editors),
            show_minimap: other.show_minimap.or(self.show_minimap),
            minimap_width: other.minimap_width.or(self.minimap_width),
            show_line_numbers: other.show_line_numbers.or(self.show_line_numbers),
            relative_line_numbers: other.relative_line_numbers.or(self.relative_line_numbers),
            show_indent_guides: other.show_indent_guides.or(self.show_indent_guides),
            show_panel: other.show_panel.or(self.show_panel),
            panel_height: other.panel_height.or(self.panel_height),
            sidebar_right: other.sidebar_right.or(self.sidebar_right),
            explorer_width: other.explorer_width.or(self.explorer_width),
            show_activity_bar: other.show_activity_bar.or(self.show_activity_bar),
            show_breadcrumbs: other.show_breadcrumbs.or(self.show_breadcrumbs),
            show_status_bar: other.show_status_bar.or(self.show_status_bar),
            status_bar_items: {
                let mut items = self.status_bar_items.clone();
                if let Some(pinned) = other.show_word_count {
                    items.insert("word_count".into(), pinned);
                }
                items.extend(other.status_bar_items.clone());
                items
            },
            show_tab_bar: other.show_tab_bar.or(self.show_tab_bar),
            enable_preview_tabs: other.enable_preview_tabs.or(self.enable_preview_tabs),
            mouse_back_forward_to_navigate: other.mouse_back_forward_to_navigate.or(self.mouse_back_forward_to_navigate),
            show_menu_bar: other.show_menu_bar.or(self.show_menu_bar),
            zen_mode: other.zen_mode.or(self.zen_mode),
            indent_rainbow: other.indent_rainbow.or(self.indent_rainbow),
            color_preview: other.color_preview.or(self.color_preview),
            bracket_pair_colorization: other.bracket_pair_colorization.or(self.bracket_pair_colorization),
            error_lens: other.error_lens.or(self.error_lens),
            hover_enabled: other.hover_enabled.or(self.hover_enabled),
            quick_suggestions: other.quick_suggestions.or(self.quick_suggestions),
            inlay_hints: other.inlay_hints.or(self.inlay_hints),
            code_lens: other.code_lens.or(self.code_lens),
            semantic_highlighting: other.semantic_highlighting.or(self.semantic_highlighting),
            format_on_type: other.format_on_type.or(self.format_on_type),
            sticky_scroll: other.sticky_scroll.or(self.sticky_scroll),
            sticky_scroll_max: other.sticky_scroll_max.or(self.sticky_scroll_max),
            scroll_past_end: other.scroll_past_end.or(self.scroll_past_end),
            focus_mode: other.focus_mode.or(self.focus_mode),
            show_word_count: other.status_bar_items.get("word_count").copied()
                .or(other.show_word_count)
                .or_else(|| self.status_bar_items.get("word_count").copied())
                .or(self.show_word_count),
            screencast_mode: other.screencast_mode.or(self.screencast_mode),
            max_global_search_results: other.max_global_search_results.or(self.max_global_search_results),
            large_file_threshold_mb: other
                .large_file_threshold_mb
                .or(self.large_file_threshold_mb),
            large_file_analysis_timeout_ms: other
                .large_file_analysis_timeout_ms
                .or(self.large_file_analysis_timeout_ms),
            rulers: other.rulers.clone().or_else(|| self.rulers.clone()),
            trim_trailing_on_save: other.trim_trailing_on_save.or(self.trim_trailing_on_save),
            format_on_save: other.format_on_save.or(self.format_on_save),
            formatters: if self.formatters.is_none() && other.formatters.is_none() { None } else {
                let mut map = self.formatters.clone().unwrap_or_default();
                map.extend(other.formatters.clone().unwrap_or_default());
                Some(map)
            },
            insert_final_newline: other.insert_final_newline.or(self.insert_final_newline),
            cursor_style: other.cursor_style.clone().or_else(|| self.cursor_style.clone()),
            cursor_blinking: other.cursor_blinking.or(self.cursor_blinking),
            cursor_surrounding_lines: other.cursor_surrounding_lines.or(self.cursor_surrounding_lines),
            auto_save: other.auto_save.clone().or_else(|| self.auto_save.clone()),
            auto_save_delay: other.auto_save_delay.or(self.auto_save_delay),
            animation_cursor_smear: other.animation_cursor_smear.or(self.animation_cursor_smear),
            animation_copy: other.animation_copy.or(self.animation_copy),
            animation_copy_duration_ms: other.animation_copy_duration_ms.or(self.animation_copy_duration_ms),
            animation_cursor_fire: other.animation_cursor_fire.or(self.animation_cursor_fire),
            animation_whitespace_reveal: other.animation_whitespace_reveal.or(self.animation_whitespace_reveal),
            animation_scope_connector: other.animation_scope_connector.or(self.animation_scope_connector),
            animation_tab_slide: other.animation_tab_slide.or(self.animation_tab_slide),
            animation_duration_ms: other.animation_duration_ms.or(self.animation_duration_ms),
            animation_fps: other.animation_fps.or(self.animation_fps),
            animation_intensity: other.animation_intensity.or(self.animation_intensity),
            animation_cursor_smear_duration_ms: other
                .animation_cursor_smear_duration_ms
                .or(self.animation_cursor_smear_duration_ms),
            animation_cursor_smear_intensity: other
                .animation_cursor_smear_intensity
                .or(self.animation_cursor_smear_intensity),
            animation_cursor_fire_duration_ms: other
                .animation_cursor_fire_duration_ms
                .or(self.animation_cursor_fire_duration_ms),
            animation_cursor_fire_intensity: other
                .animation_cursor_fire_intensity
                .or(self.animation_cursor_fire_intensity),
            animation_whitespace_reveal_duration_ms: other
                .animation_whitespace_reveal_duration_ms
                .or(self.animation_whitespace_reveal_duration_ms),
            animation_whitespace_reveal_intensity: other
                .animation_whitespace_reveal_intensity
                .or(self.animation_whitespace_reveal_intensity),
            animation_scope_bubble_duration_ms: other
                .animation_scope_bubble_duration_ms
                .or(self.animation_scope_bubble_duration_ms),
            animation_scope_bubble_intensity: other
                .animation_scope_bubble_intensity
                .or(self.animation_scope_bubble_intensity),
            animation_tab_slide_duration_ms: other
                .animation_tab_slide_duration_ms
                .or(self.animation_tab_slide_duration_ms),
            animation_tab_slide_intensity: other
                .animation_tab_slide_intensity
                .or(self.animation_tab_slide_intensity),
            animation_scope_bubble_tight: other
                .animation_scope_bubble_tight
                .or(self.animation_scope_bubble_tight),
            animation_scope_bubble_fill: other
                .animation_scope_bubble_fill
                .or(self.animation_scope_bubble_fill),
            animation_scope_bubble_disabled_languages: other
                .animation_scope_bubble_disabled_languages
                .clone()
                .or_else(|| self.animation_scope_bubble_disabled_languages.clone()),
            animation_scope_bubble_languages: crate::scope_bubble_config::merge(
                &self.animation_scope_bubble_languages, &other.animation_scope_bubble_languages),
            animation_scope_bubble_glow: other
                .animation_scope_bubble_glow
                .or(self.animation_scope_bubble_glow),
            animation_scope_bubble_glow_duration_ms: other
                .animation_scope_bubble_glow_duration_ms
                .or(self.animation_scope_bubble_glow_duration_ms),
            scroll_speed: other.scroll_speed.or(self.scroll_speed),
            keybindings: merge_keybinding_layers(&self.keybindings, &other.keybindings),
            search_case_sensitive: other.search_case_sensitive.or(self.search_case_sensitive),
            search_regex: other.search_regex.or(self.search_regex),
            search_whole_word: other.search_whole_word.or(self.search_whole_word),
            search_preserve_case: other.search_preserve_case.or(self.search_preserve_case),
            search_use_ignore_files: other.search_use_ignore_files.or(self.search_use_ignore_files),
            search_use_global_ignore_files: other.search_use_global_ignore_files.or(self.search_use_global_ignore_files),
            search_use_parent_ignore_files: other.search_use_parent_ignore_files.or(self.search_use_parent_ignore_files),
            search_follow_symlinks: other.search_follow_symlinks.or(self.search_follow_symlinks),
            files_exclude: self.files_exclude.iter().chain(&other.files_exclude).map(|(key, value)| (key.clone(), value.clone())).collect(),
            search_exclude: self.search_exclude.iter().chain(&other.search_exclude).map(|(key, value)| (key.clone(), value.clone())).collect(),
            language_associations: match (&other.language_associations, &self.language_associations) {
                (Some(o), Some(s)) => {
                    let mut merged = s.clone();
                    merged.extend(o.iter().map(|(k, v)| (k.clone(), v.clone())));
                    Some(merged)
                }
                (Some(o), None) => Some(o.clone()),
                (None, Some(s)) => Some(s.clone()),
                (None, None) => None,
            },
        }
    }

    /// Clamp numeric settings to valid ranges and validate string enums.
    pub(crate) fn validate(&mut self) {
        if let Some(ref mut v) = self.tab_size {
            *v = (*v).clamp(1, 8);
        }
        if let Some(ref mut v) = self.panel_height {
            if *v < 3 { *v = 3; }
        }
        if let Some(ref mut v) = self.explorer_width {
            if *v < 15 { *v = 15; }
        }
        if let Some(ref mut v) = self.minimap_width {
            *v = (*v).clamp(5, 40);
        }
        if let Some(ref mut v) = self.sticky_scroll_max {
            *v = (*v).clamp(1, 5);
        }
        if let Some(ref mut v) = self.rulers {
            v.retain(|&r| r > 0 && r < 1000);
            if v.len() > 10 {
                v.truncate(10);
            }
        }
        if let Some(ref mut v) = self.max_global_search_results {
            *v = (*v).max(1);
        }
        if let Some(ref mut v) = self.large_file_threshold_mb {
            *v = (*v).clamp(1, 16_384);
        }
        if let Some(ref mut v) = self.large_file_analysis_timeout_ms {
            *v = (*v).clamp(100, 60_000);
        }
        if let Some(ref v) = self.cursor_style {
            if !matches!(v.as_str(), "line" | "block" | "underline") {
                self.cursor_style = None;
            }
        }
        if let Some(ref v) = self.whitespace_mode {
            if !matches!(v.as_str(), "selection" | "all" | "none" | "boundary" | "trailing") {
                self.whitespace_mode = None;
            }
        }
        if let Some(ref v) = self.comment_whitespace_mode {
            if !matches!(v.as_str(), "inherit" | "selection" | "all" | "none" | "boundary" | "trailing") {
                self.comment_whitespace_mode = None;
            }
        }
        if let Some(ref v) = self.auto_save {
            if !matches!(v.as_str(), "off" | "afterDelay" | "onFocusChange" | "onWindowChange") {
                self.auto_save = None;
            }
        }
        if let Some(ref mut v) = self.auto_save_delay {
            *v = (*v).clamp(100, 3_600_000);
        }
        if let Some(ref mut v) = self.lsp_diagnostic_delay_ms {
            *v = (*v).min(crate::lsp::diagnostics::MAX_DELAY_MS);
        }
        if let Some(ref mut v) = self.animation_duration_ms {
            *v = clamp_animation_duration_ms(*v);
        }
        if let Some(ref mut v) = self.animation_copy_duration_ms { *v = clamp_animation_duration_ms(*v); }
        if let Some(ref mut v) = self.animation_fps {
            *v = (*v).clamp(15, 120);
        }
        if let Some(ref mut v) = self.animation_intensity {
            *v = (*v).clamp(5, 100);
        }
        if let Some(ref mut value) = self.animation_cursor_smear_duration_ms {
            *value = clamp_animation_duration_ms(*value);
        }
        if let Some(ref mut value) = self.animation_cursor_smear_intensity {
            *value = (*value).clamp(5, 100);
        }
        if let Some(ref mut value) = self.animation_cursor_fire_duration_ms {
            *value = clamp_animation_duration_ms(*value);
        }
        if let Some(ref mut value) = self.animation_cursor_fire_intensity {
            *value = (*value).clamp(5, 100);
        }
        if let Some(ref mut value) = self.animation_whitespace_reveal_duration_ms {
            *value = clamp_animation_duration_ms(*value);
        }
        if let Some(ref mut value) = self.animation_whitespace_reveal_intensity {
            *value = (*value).clamp(5, 100);
        }
        if let Some(ref mut value) = self.animation_scope_bubble_duration_ms {
            *value = clamp_animation_duration_ms(*value);
        }
        if let Some(ref mut value) = self.animation_scope_bubble_intensity {
            *value = (*value).clamp(5, 100);
        }
        if let Some(ref mut value) = self.animation_tab_slide_duration_ms {
            *value = clamp_animation_duration_ms(*value);
        }
        if let Some(ref mut value) = self.animation_tab_slide_intensity {
            *value = (*value).clamp(5, 100);
        }
        if let Some(ref mut value) = self.animation_scope_bubble_glow_duration_ms {
            *value = clamp_animation_duration_ms(*value);
        }
        if let Some(ref mut v) = self.scroll_speed {
            *v = (*v).clamp(1, 10);
        }
        if let Some(ref mut v) = self.cursor_surrounding_lines {
            *v = (*v).min(50);
        }
        if let Some(ref mut v) = self.word_wrap_column {
            if *v != 0 {
                *v = (*v).clamp(40, 500);
            }
        }
        // Validate language associations: reject oversized keys/values
        if let Some(ref mut assoc) = self.language_associations {
            assoc.retain(|pattern, lang| {
                !pattern.is_empty() && pattern.len() <= 100 && !lang.is_empty() && lang.len() <= 50
            });
        }
    }
}
/// Global preferences always live beside the running executable.
pub(crate) fn global_settings_base_dir() -> PathBuf {
    crate::portable::base_dir()
}

pub(crate) fn global_settings_path() -> PathBuf {
    global_settings_base_dir().join("settings.json")
}

/// Path to the workspace settings file.
pub(crate) fn project_settings_path(project_root: &Path) -> PathBuf {
    crate::portable::workspace_dir(project_root).join("settings.json")
}

/// Load settings from a JSON file. Returns default if file doesn't exist.
pub(crate) fn load_from_file(path: &Path) -> Settings {
    match std::fs::read_to_string(path) {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(settings) => settings,
            Err(e) => {
                log_warn!("failed to parse settings from {}: {e}", path.display());
                Settings::default()
            }
        },
        Err(_) => Settings::default(),
    }
}

/// Save settings to a JSON file, creating parent dirs as needed.
pub(crate) fn save_to_file(path: &Path, settings: &Settings) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Scope files contain overrides only. Omitting unset fields keeps an empty
    // workspace file readable (`{}`) and makes hand-editing pleasant.
    let mut value = serde_json::to_value(settings)?;
    if let Some(object) = value.as_object_mut() {
        object.retain(|_, value| !value.is_null());
    }
    let mut json = serde_json::to_string_pretty(&value)?;
    json.push('\n');
    std::fs::write(path, json)
}

#[cfg(test)]
mod tests {
    use super::Settings;

    #[test]
    fn diagnostic_delay_roundtrips_inherits_and_clamps_without_losing_zero() {
        let global: Settings = serde_json::from_value(serde_json::json!({
            "lspDiagnosticDelayMs": 1_500
        })).unwrap();
        assert_eq!(global.lsp_diagnostic_delay_ms, Some(1_500));
        assert_eq!(serde_json::to_value(&global).unwrap()["lspDiagnosticDelayMs"], 1_500);
        assert_eq!(global.merge(&Settings::default()).lsp_diagnostic_delay_ms, Some(1_500));
        let mut workspace = Settings { lsp_diagnostic_delay_ms: Some(0), ..Default::default() };
        workspace.validate();
        assert_eq!(global.merge(&workspace).lsp_diagnostic_delay_ms, Some(0));
        workspace.lsp_diagnostic_delay_ms = Some(u64::MAX);
        workspace.validate();
        assert_eq!(workspace.lsp_diagnostic_delay_ms, Some(crate::lsp::diagnostics::MAX_DELAY_MS));
        assert!(serde_json::to_value(Settings::default()).unwrap().get("lspDiagnosticDelayMs").is_none());
    }

    #[test]
    fn animation_preferences_round_trip_and_merge_independently() {
        use crate::animation_config::AnimationEffect;
        let mut base_json = serde_json::json!({});
        for effect in AnimationEffect::ALL {
            base_json[effect.json_key()] = serde_json::json!(true);
        }
        base_json["animationDurationMs"] = serde_json::json!(300);
        base_json["animationFps"] = serde_json::json!(30);
        base_json["animationIntensity"] = serde_json::json!(70);
        let base: Settings = serde_json::from_value(base_json.clone()).unwrap();
        let decoded: Settings = serde_json::from_str(&serde_json::to_string(&base).unwrap()).unwrap();
        assert_eq!(decoded, base);
        let empty = serde_json::to_value(Settings::default()).unwrap();
        for effect in AnimationEffect::ALL {
            assert!(empty.get(effect.json_key()).is_none());
            let mut override_json = serde_json::json!({});
            override_json[effect.json_key()] = serde_json::json!(false);
            let overrides: Settings = serde_json::from_value(override_json).unwrap();
            let merged = serde_json::to_value(base.merge(&overrides)).unwrap();
            for other in AnimationEffect::ALL {
                assert_eq!(merged[other.json_key()], serde_json::json!(other != effect));
            }
            assert_eq!(merged["animationDurationMs"], 300);
            assert_eq!(merged["animationFps"], 30);
            assert_eq!(merged["animationIntensity"], 70);
        }
    }

    #[test]
    fn retired_animation_preferences_are_ignored_without_enabling_retained_effects() {
        let old: Settings = serde_json::from_value(serde_json::json!({
            "animationCursorGlow": true,
            "animationTypingParticles": true,
            "animationSaveShimmer": true,
            "animationMinimapPulse": true,
            "animationDurationMs": 310,
            "tabSize": 8,
        })).unwrap();
        let mut effective = crate::animation_config::AnimationSettings::default();
        effective.apply(&old);
        assert!(!effective.any_enabled());
        assert_eq!(effective.duration_ms, 310);
        assert_eq!(old.tab_size, Some(8));
        let saved = serde_json::to_value(old).unwrap();
        for key in ["animationCursorGlow", "animationTypingParticles", "animationSaveShimmer", "animationMinimapPulse"] {
            assert!(saved.get(key).is_none());
        }
    }

    #[test]
    fn animation_timing_validation_clamps_both_ends() {
        for (duration, fps, intensity, expected) in [
            (0, 0, 0, (1, 15, 5)),
            (u64::MAX, u64::MAX, u8::MAX, (5000, 120, 100)),
        ] {
            let mut settings = Settings {
                animation_duration_ms: Some(duration),
                animation_fps: Some(fps),
                animation_intensity: Some(intensity),
                ..Default::default()
            };
            settings.validate();
            assert_eq!(settings.animation_duration_ms, Some(expected.0));
            assert_eq!(settings.animation_fps, Some(expected.1));
            assert_eq!(settings.animation_intensity, Some(expected.2));
        }
    }

    #[test]
    fn every_animation_duration_validates_between_one_and_five_thousand_ms() {
        use crate::animation_config::AnimationEffect;
        for (input, expected) in [(0, 1), (1, 1), (5000, 5000), (u64::MAX, 5000)] {
            let mut settings = Settings {
                animation_duration_ms: Some(input),
                animation_scope_bubble_glow_duration_ms: Some(input),
                ..Default::default()
            };
            for effect in AnimationEffect::ALL {
                settings.set_animation_effect_duration(effect, input);
            }
            settings.validate();
            assert_eq!(settings.animation_duration_ms, Some(expected));
            assert_eq!(
                settings.animation_scope_bubble_glow_duration_ms,
                Some(expected)
            );
            for effect in AnimationEffect::ALL {
                assert_eq!(settings.animation_effect_duration(effect), Some(expected));
            }
        }
    }

    #[test]
    fn per_effect_timing_and_scope_options_roundtrip_merge_and_validate() {
        use crate::animation_config::AnimationEffect;
        let mut base = Settings::default();
        for (index, effect) in AnimationEffect::ALL.into_iter().enumerate() {
            base.set_animation_effect_duration(effect, 300 + index as u64 * 50);
            base.set_animation_effect_intensity(effect, 60 + index as u8 * 5);
        }
        base.animation_scope_bubble_tight = Some(false);
        base.animation_scope_bubble_glow = Some(true);
        base.animation_scope_bubble_glow_duration_ms = Some(2200);
        let json = serde_json::to_value(&base).unwrap();
        let restored: Settings = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(restored, base);
        for (index, effect) in AnimationEffect::ALL.into_iter().enumerate() {
            assert_eq!(json[effect.duration_json_key()], 300 + index as u64 * 50);
            assert_eq!(json[effect.intensity_json_key()], 60 + index as u8 * 5);
            let mut overrides = Settings::default();
            overrides.set_animation_effect_duration(effect, u64::MAX);
            overrides.set_animation_effect_intensity(effect, 0);
            overrides.animation_scope_bubble_glow_duration_ms = Some(u64::MAX);
            let mut merged = base.merge(&overrides);
            merged.validate();
            for other in AnimationEffect::ALL {
                assert_eq!(
                    merged.animation_effect_duration(other),
                    if other == effect {
                        Some(5000)
                    } else {
                        base.animation_effect_duration(other)
                    }
                );
                assert_eq!(
                    merged.animation_effect_intensity(other),
                    if other == effect {
                        Some(5)
                    } else {
                        base.animation_effect_intensity(other)
                    }
                );
            }
            assert_eq!(merged.animation_scope_bubble_tight, Some(false));
            assert_eq!(merged.animation_scope_bubble_glow, Some(true));
            assert_eq!(merged.animation_scope_bubble_glow_duration_ms, Some(5000));
        }
    }

    #[test]
    fn scope_bubble_and_effect_overrides_survive_settings_file_roundtrip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("settings.json");
        let settings = Settings {
            animation_cursor_smear_duration_ms: Some(320),
            animation_cursor_fire_intensity: Some(87),
            animation_scope_bubble_duration_ms: Some(640),
            animation_scope_bubble_intensity: Some(72),
            animation_scope_bubble_tight: Some(false),
            animation_scope_bubble_fill: Some(true),
            animation_scope_bubble_disabled_languages: Some(vec!["Python".into(), "cpp".into()]),
            animation_scope_bubble_glow: Some(true),
            animation_scope_bubble_glow_duration_ms: Some(2100),
            animation_fps: Some(90),
            ..Default::default()
        };
        super::save_to_file(&path, &settings).unwrap();
        assert_eq!(super::load_from_file(&path), settings);
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(json["animationScopeBubbleTight"], false);
        assert_eq!(json["animationScopeBubbleFill"], true);
        assert_eq!(json["animationScopeBubbleDisabledLanguages"], serde_json::json!(["Python", "cpp"]));
        assert_eq!(json["animationScopeBubbleGlow"], true);
        assert_eq!(json["animationScopeBubbleGlowDurationMs"], 2100);
        assert!(json.get("animationDurationMs").is_none());
        assert!(json.get("animationIntensity").is_none());
    }

    #[test]
    fn merge_other_overrides_base() {
        let base = Settings {
            tab_size: Some(4),
            format_on_save: Some(false),
            ..Default::default()
        };
        let other = Settings {
            tab_size: Some(2),
            format_on_save: Some(true),
            ..Default::default()
        };
        let merged = base.merge(&other);
        assert_eq!(merged.tab_size, Some(2));
        assert_eq!(merged.format_on_save, Some(true));
    }

    #[test]
    fn merge_none_does_not_override() {
        let base = Settings { tab_size: Some(4), theme: Some("dark".to_string()), ..Default::default() };
        let other = Settings::default();
        let merged = base.merge(&other);
        assert_eq!(merged.tab_size, Some(4));
        assert_eq!(merged.theme, Some("dark".to_string()));
    }

    #[test]
    fn status_bar_item_overrides_merge_individually_and_roundtrip() {
        let mut global = Settings::default();
        global.status_bar_items.insert("encoding".into(), true);
        global.status_bar_items.insert("language".into(), false);
        let mut workspace = Settings::default();
        workspace.status_bar_items.insert("encoding".into(), false);
        workspace.status_bar_items.insert("line_ending".into(), true);
        let merged = global.merge(&workspace);
        assert_eq!(merged.status_bar_items.get("encoding"), Some(&false));
        assert_eq!(merged.status_bar_items.get("language"), Some(&false));
        assert_eq!(merged.status_bar_items.get("line_ending"), Some(&true));
        let json = serde_json::to_string(&merged).unwrap();
        let restored: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.status_bar_items, merged.status_bar_items);
        assert!(!serde_json::to_value(Settings::default()).unwrap()
            .as_object().unwrap().contains_key("statusBarItems"));
    }

    #[test]
    fn merge_mixed_fields() {
        let base = Settings { tab_size: Some(4), use_tabs: Some(true), ..Default::default() };
        let other = Settings { use_tabs: Some(false), word_wrap: Some(true), ..Default::default() };
        let merged = base.merge(&other);
        assert_eq!(merged.tab_size, Some(4)); // kept from base
        assert_eq!(merged.use_tabs, Some(false)); // overridden by other
        assert_eq!(merged.word_wrap, Some(true)); // new from other
    }

    #[test]
    fn merge_new_editor_and_search_preferences() {
        let base = Settings {
            hover_enabled: Some(true),
            quick_suggestions: Some(true),
            format_on_type: Some(false),
            search_preserve_case: Some(false),
            ..Default::default()
        };
        let other = Settings {
            hover_enabled: Some(false),
            format_on_type: Some(true),
            search_preserve_case: Some(true),
            ..Default::default()
        };

        let merged = base.merge(&other);

        assert_eq!(merged.hover_enabled, Some(false));
        assert_eq!(merged.quick_suggestions, Some(true));
        assert_eq!(merged.format_on_type, Some(true));
        assert_eq!(merged.search_preserve_case, Some(true));
    }

    #[test]
    fn project_settings_path_structure() {
        use std::path::Path;
        let p = super::project_settings_path(Path::new("/my/project"));
        assert!(p.ends_with("settings.json"));
        assert!(p.to_string_lossy().contains(".vtcode"));
    }

    #[test]
    fn global_settings_live_in_the_global_vtcode_base() {
        let base = super::global_settings_base_dir();
        assert_eq!(super::global_settings_path(), base.join("settings.json"));
        assert_eq!(base, std::env::current_exe().unwrap().parent().unwrap());
    }

    #[test]
    fn load_from_file_missing_returns_default() {
        use std::path::Path;
        let settings = super::load_from_file(Path::new("/nonexistent/path/settings.json"));
        assert_eq!(settings.tab_size, None);
        assert_eq!(settings.use_tabs, None);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = std::env::temp_dir().join("vtcode_test_settings_roundtrip");
        let _ = std::fs::create_dir_all(&dir); // Error OK: dir may already exist
        let path = dir.join("settings.json");
        let settings = Settings { tab_size: Some(8), use_tabs: Some(true), ..Default::default() };
        super::save_to_file(&path, &settings).unwrap();
        let loaded = super::load_from_file(&path);
        assert_eq!(loaded.tab_size, Some(8));
        assert_eq!(loaded.use_tabs, Some(true));
        let _ = std::fs::remove_dir_all(&dir); // Error OK: dir may not exist
    }

    #[test]
    fn empty_scope_serializes_as_a_clean_json_object() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        super::save_to_file(&path, &Settings::default()).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{}\n");
    }

    #[test]
    fn loading_missing_scope_is_read_only_until_first_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project").join(".vtcode").join("settings.json");

        let loaded = super::load_from_file(&path);
        assert_eq!(loaded, Settings::default());
        assert!(!path.exists());

        super::save_to_file(&path, &loaded).unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{}\n");
    }

    #[test]
    fn precedence_is_defaults_then_global_then_workspace() {
        let defaults = Settings {
            tab_size: Some(4),
            word_wrap: Some(false),
            theme: Some("dark".into()),
            ..Default::default()
        };
        let global = Settings {
            tab_size: Some(2),
            word_wrap: Some(true),
            ..Default::default()
        };
        let workspace = Settings {
            tab_size: Some(8),
            ..Default::default()
        };

        let effective = defaults.merge(&global).merge(&workspace);
        assert_eq!(effective.tab_size, Some(8));
        assert_eq!(effective.word_wrap, Some(true));
        assert_eq!(effective.theme.as_deref(), Some("dark"));
    }

    #[test]
    fn merge_language_associations() {
        use std::collections::HashMap;
        let mut base_map = HashMap::new();
        base_map.insert("*.xyz".to_string(), "Python".to_string());
        base_map.insert("*.abc".to_string(), "Java".to_string());
        let base = Settings { language_associations: Some(base_map), ..Default::default() };

        let mut other_map = HashMap::new();
        other_map.insert("*.xyz".to_string(), "Ruby".to_string()); // override
        other_map.insert("*.def".to_string(), "Go".to_string());   // new
        let other = Settings { language_associations: Some(other_map), ..Default::default() };

        let merged = base.merge(&other);
        let assoc = merged.language_associations.unwrap();
        assert_eq!(assoc.get("*.xyz").unwrap(), "Ruby");  // overridden
        assert_eq!(assoc.get("*.abc").unwrap(), "Java");  // kept
        assert_eq!(assoc.get("*.def").unwrap(), "Go");    // new
    }

    #[test]
    fn validate_max_global_search_results_preserves_user_limits() {
        let mut settings = Settings {
            max_global_search_results: Some(50_000),
            ..Default::default()
        };
        settings.validate();
        assert_eq!(settings.max_global_search_results, Some(50_000));

        let mut settings = Settings {
            max_global_search_results: Some(10),
            ..Default::default()
        };
        settings.validate();
        assert_eq!(settings.max_global_search_results, Some(10));
    }

    #[test]
    fn vscode_search_settings_load_dotted_keys_and_merge_exclusion_entries() {
        let global: Settings = serde_json::from_value(serde_json::json!({
            "search.useIgnoreFiles": false,
            "search.useParentIgnoreFiles": true,
            "search.useGlobalIgnoreFiles": true,
            "search.followSymlinks": false,
            "search.maxResults": 50_000,
            "files.exclude": { "**/generated": true },
            "search.exclude": { "**/node_modules": false, "**/cache": true }
        })).unwrap();
        let workspace: Settings = serde_json::from_value(serde_json::json!({
            "search.exclude": { "**/cache": false }
        })).unwrap();
        let merged = global.merge(&workspace);
        assert_eq!(merged.search_use_ignore_files, Some(false));
        assert_eq!(merged.search_use_parent_ignore_files, Some(true));
        assert_eq!(merged.search_use_global_ignore_files, Some(true));
        assert_eq!(merged.search_follow_symlinks, Some(false));
        assert_eq!(merged.max_global_search_results, Some(50_000));
        assert_eq!(merged.files_exclude["**/generated"], true);
        assert_eq!(merged.search_exclude["**/node_modules"], false);
        assert_eq!(merged.search_exclude["**/cache"], false);
    }

    #[test]
    fn validate_tab_size_clamps() {
        let mut s = Settings { tab_size: Some(0), ..Default::default() };
        s.validate();
        assert_eq!(s.tab_size, Some(1));

        let mut s = Settings { tab_size: Some(99), ..Default::default() };
        s.validate();
        assert_eq!(s.tab_size, Some(8));
    }

    #[test]
    fn validate_cursor_style_rejects_invalid() {
        for (style, expected) in [
            ("blink-square", None),
            ("blinky", None),
            ("BLOCK", None),
            ("Line", None),
            ("fancy", None),
            ("", None),
            ("block", Some("block")),
        ] {
            let mut settings = Settings {
                cursor_style: Some(style.into()),
                ..Default::default()
            };
            settings.validate();
            assert_eq!(settings.cursor_style.as_deref(), expected, "{style:?}");
        }
    }

    #[test]
    fn validate_whitespace_mode_rejects_invalid() {
        let mut s = Settings { whitespace_mode: Some("invalid".to_string()), ..Default::default() };
        s.validate();
        assert_eq!(s.whitespace_mode, None);

        let mut s = Settings { whitespace_mode: Some("all".to_string()), ..Default::default() };
        s.validate();
        assert_eq!(s.whitespace_mode, Some("all".to_string()));
    }

    #[test]
    fn comment_whitespace_modes_validate_and_inherit_overrides_a_global_choice() {
        for mode in ["inherit", "none", "boundary", "selection", "trailing", "all", "invalid"] {
            let mut settings: Settings = serde_json::from_value(serde_json::json!({
                "commentWhitespaceMode": mode,
            })).unwrap();
            settings.validate();
            assert_eq!(settings.comment_whitespace_mode.as_deref(), (mode != "invalid").then_some(mode));
        }
        let global = Settings { comment_whitespace_mode: Some("all".into()), ..Default::default() };
        let workspace = Settings { comment_whitespace_mode: Some("inherit".into()), ..Default::default() };
        let merged = global.merge(&workspace);
        let saved = serde_json::to_string(&merged).unwrap();
        let loaded: Settings = serde_json::from_str(&saved).unwrap();
        assert_eq!(loaded.comment_whitespace_mode.as_deref(), Some("inherit"));
    }

    #[test]
    fn validate_auto_save_rejects_invalid() {
        let mut s = Settings { auto_save: Some("always".to_string()), ..Default::default() };
        s.validate();
        assert_eq!(s.auto_save, None);

        let mut s = Settings { auto_save: Some("afterDelay".to_string()), ..Default::default() };
        s.validate();
        assert_eq!(s.auto_save, Some("afterDelay".to_string()));
    }

    #[test]
    fn validate_auto_save_delay_clamps() {
        let mut s = Settings { auto_save_delay: Some(10), ..Default::default() };
        s.validate();
        assert_eq!(s.auto_save_delay, Some(100));

        let mut s = Settings { auto_save_delay: Some(99_999_999), ..Default::default() };
        s.validate();
        assert_eq!(s.auto_save_delay, Some(3_600_000));
    }

    #[test]
    fn validate_scroll_speed_clamps() {
        let mut s = Settings { scroll_speed: Some(0), ..Default::default() };
        s.validate();
        assert_eq!(s.scroll_speed, Some(1));

        let mut s = Settings { scroll_speed: Some(100), ..Default::default() };
        s.validate();
        assert_eq!(s.scroll_speed, Some(10));
    }

    #[test]
    fn validate_explorer_width_min() {
        let mut s = Settings { explorer_width: Some(5), ..Default::default() };
        s.validate();
        assert_eq!(s.explorer_width, Some(15));
    }

    #[test]
    fn validate_panel_height_min() {
        for height in [0, 1, 2, 3, 4] {
            let mut settings = Settings {
                panel_height: Some(height),
                ..Default::default()
            };
            settings.validate();
            assert_eq!(settings.panel_height, Some(height.max(3)));
        }
    }

    #[test]
    fn validate_sticky_scroll_max_clamps() {
        let mut s = Settings { sticky_scroll_max: Some(0), ..Default::default() };
        s.validate();
        assert_eq!(s.sticky_scroll_max, Some(1));

        let mut s = Settings { sticky_scroll_max: Some(50), ..Default::default() };
        s.validate();
        assert_eq!(s.sticky_scroll_max, Some(5));
    }

    #[test]
    fn validate_word_wrap_column_clamps() {
        let mut s = Settings { word_wrap_column: Some(10), ..Default::default() };
        s.validate();
        assert_eq!(s.word_wrap_column, Some(40));

        let mut s = Settings { word_wrap_column: Some(1000), ..Default::default() };
        s.validate();
        assert_eq!(s.word_wrap_column, Some(500));
    }

    #[test]
    fn validate_word_wrap_column_preserves_viewport_zero() {
        let mut s = Settings { word_wrap_column: Some(0), ..Default::default() };
        s.validate();
        assert_eq!(s.word_wrap_column, Some(0));
    }

    #[test]
    fn validate_rulers_filters_invalid_and_truncates() {
        let mut s = Settings { rulers: Some(vec![0, 80, 120, 1500]), ..Default::default() };
        s.validate();
        let rulers = s.rulers.unwrap();
        assert_eq!(rulers, vec![80, 120]); // 0 and 1500 filtered out

        let mut s = Settings {
            rulers: Some((1..=15).collect()),
            ..Default::default()
        };
        s.validate();
        assert_eq!(s.rulers.unwrap().len(), 10); // truncated to 10
    }

    #[test]
    fn validate_cursor_surrounding_lines_max() {
        let mut s = Settings { cursor_surrounding_lines: Some(100), ..Default::default() };
        s.validate();
        assert_eq!(s.cursor_surrounding_lines, Some(50));
    }

    #[test]
    fn merge_language_associations_one_side_none() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        map.insert("*.py".to_string(), "Python".to_string());
        let base = Settings { language_associations: Some(map), ..Default::default() };
        let other = Settings::default();
        let merged = base.merge(&other);
        assert_eq!(merged.language_associations.unwrap().get("*.py").unwrap(), "Python");

        let base2 = Settings::default();
        let mut map2 = HashMap::new();
        map2.insert("*.rs".to_string(), "Rust".to_string());
        let other2 = Settings { language_associations: Some(map2), ..Default::default() };
        let merged2 = base2.merge(&other2);
        assert_eq!(merged2.language_associations.unwrap().get("*.rs").unwrap(), "Rust");
    }

    #[test]
    fn settings_serde_round_trip() {
        let s = Settings {
            tab_size: Some(2),
            use_tabs: Some(true),
            theme: Some("monokai".to_string()),
            show_explorer: Some(false),
            ..Default::default()
        };
        let json = serde_json::to_string(&s).unwrap();
        let loaded: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.tab_size, Some(2));
        assert_eq!(loaded.use_tabs, Some(true));
        assert_eq!(loaded.theme, Some("monokai".to_string()));
        assert_eq!(loaded.show_explorer, Some(false));
    }

    #[test]
    fn removed_scroll_preference_does_not_break_existing_settings() {
        let settings: Settings =
            serde_json::from_str(r#"{"smoothScrolling":true,"scrollSpeed":7}"#).unwrap();
        assert_eq!(settings.scroll_speed, Some(7));
        assert!(serde_json::to_value(settings).unwrap().get("smoothScrolling").is_none());
    }

    #[test]
    fn merge_keybindings_layer_by_command() {
        let kb1 = super::KeybindingSetting {
            command: Some("save".to_string()),
            key: Some("ctrl+s".to_string()),
            ..Default::default()
        };
        let kb2 = super::KeybindingSetting {
            command: Some("quit".to_string()),
            key: Some("ctrl+q".to_string()),
            ..Default::default()
        };
        let base = Settings { keybindings: Some(vec![kb1]), ..Default::default() };
        let other = Settings { keybindings: Some(vec![kb2]), ..Default::default() };
        let merged = base.merge(&other);
        let kbs = merged.keybindings.unwrap();
        assert_eq!(kbs.len(), 2);
        assert_eq!(kbs[0].command.as_deref(), Some("save"));
        assert_eq!(kbs[1].command.as_deref(), Some("quit"));
    }

    #[test]
    fn merge_keybindings_workspace_replaces_matching_global_command() {
        let global = Settings {
            keybindings: Some(vec![super::KeybindingSetting {
                command: Some("save".to_string()),
                key: Some("Alt+S".to_string()),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let workspace = Settings {
            keybindings: Some(vec![super::KeybindingSetting {
                command: Some("save".to_string()),
                key: Some("Shift+F6".to_string()),
                ..Default::default()
            }]),
            ..Default::default()
        };

        let merged = global.merge(&workspace);

        let bindings = merged.keybindings.unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].key.as_deref(), Some("Shift+F6"));
    }

    #[test]
    fn validate_language_associations_rejects_empty_keys() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        map.insert(String::new(), "Python".to_string());
        map.insert("*.rs".to_string(), String::new());
        map.insert("*.js".to_string(), "JavaScript".to_string());
        let mut s = Settings { language_associations: Some(map), ..Default::default() };
        s.validate();
        let assoc = s.language_associations.unwrap();
        assert_eq!(assoc.len(), 1);
        assert!(assoc.contains_key("*.js"));
    }

    #[test]
    fn validate_language_associations_rejects_oversized() {
        use std::collections::HashMap;
        let mut map = HashMap::new();
        let long_pattern = "x".repeat(101);
        map.insert(long_pattern, "Python".to_string());
        map.insert("*.rs".to_string(), "Rust".to_string());
        let mut s = Settings { language_associations: Some(map), ..Default::default() };
        s.validate();
        let assoc = s.language_associations.unwrap();
        assert_eq!(assoc.len(), 1);
        assert!(assoc.contains_key("*.rs"));
    }

    #[test]
    fn test_merge_both_sides_none() {
        let base = Settings::default();
        let other = Settings::default();
        let merged = base.merge(&other);
        // All fields should remain None (default)
        assert_eq!(merged.tab_size, None);
        assert_eq!(merged.use_tabs, None);
        assert_eq!(merged.theme, None);
        assert_eq!(merged.cursor_style, None);
        assert_eq!(merged.panel_height, None);
        assert_eq!(merged.word_wrap, None);
        assert_eq!(merged.scroll_speed, None);
        assert_eq!(merged.language_associations, None);
    }
}
