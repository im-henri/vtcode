//! Settings UI display methods.
//!
//! Contains settings metadata (categories, field kinds, descriptions),
//! text wrapping, and settings list filtering/scrolling.
//! Extracted from `app_session.rs`.

use crate::app::{App, AutoSaveMode};
use crate::animation_config::{AnimationEffect, AnimationSettings};

pub(crate) const SETTINGS_CATEGORIES: &[&str] = &[
    "Commonly Used", "Text Editor", "Workbench", "Features", "Files",
    "Keyboard Shortcuts", "Agent Chat", "Animations", "Language Servers", "Source Control", "All",
];

pub(crate) const SETTINGS_ALL_CATEGORY: usize = SETTINGS_CATEGORIES.len() - 1;

const ANIMATION_SECTIONS: &[(&str, &str, &[AnimationEffect])] = &[
    (
        "Cursor",
        "heading_animation_cursor",
        &[AnimationEffect::CursorSmear, AnimationEffect::CursorFire],
    ),
    (
        "Editing",
        "heading_animation_editing",
        &[AnimationEffect::WhitespaceReveal],
    ),
    (
        "Code Structure",
        "heading_animation_structure",
        &[AnimationEffect::ScopeConnector],
    ),
    (
        "Navigation",
        "heading_animation_navigation",
        &[AnimationEffect::TabSlide],
    ),
    ("Shared Frame Rate", "heading_animation_timing", &[]),
];

impl App {
    pub(crate) fn settings_categories() -> &'static [&'static str] {
        SETTINGS_CATEGORIES
    }

    pub(crate) fn settings_category_for_id(setting_id: &str) -> usize {
        if crate::app_vtcomplete_settings::field(setting_id).is_some() {
            return 1;
        }
        if setting_id.starts_with("animation_") || setting_id.starts_with("heading_animation") {
            return 7;
        }
        if setting_id.starts_with("kb_")
            || setting_id == "heading_shortcuts"
            || setting_id.starts_with("heading_kb_")
            || setting_id.starts_with("shortcut_ro_")
        {
            return 5;
        }
        match setting_id {
            "source_control_enabled" | "source_control_show_untracked" => 9,
            "lsp_servers" | "lsp_enabled" | "lsp_diagnostic_delay_ms" => 8,
            "acp_agents" => 6,
            "heading_editor" | "tab_size" | "use_tabs" | "word_wrap" | "word_wrap_column" | "whitespace" | "comment_whitespace" | "read_only"
            | "quick_suggestions" | "hover_enabled" | "format_on_type" | "format_on_save" | "formatters"
            | "cursor_style" | "cursor_blinking" | "cursor_surrounding_lines" | "minimap" | "minimap_width" => 1,
            "heading_features" | "indent_rainbow" | "color_preview" | "bracket_pair_colorization" | "error_lens"
            | "inlay_hints" | "code_lens" | "semantic_highlighting" | "sticky_scroll" | "sticky_scroll_max"
            | "scroll_past_end" | "focus_mode" | "word_count" | "screencast_mode" | "rulers"
            | "heading_search" | "search_case_sensitive" | "search_whole_word" | "search_regex"
            | "search_preserve_case" | "search_exclude" | "search_debounce_ms" | "max_global_search_results" | "scroll_speed" => 3,
            "files_exclude" | "heading_save" | "trim_trailing" | "final_newline" | "language_associations"
            | "large_file_threshold_mb" | "large_file_analysis_timeout_ms"
            | "heading_cursor"
            | "auto_save_mode" | "auto_save_delay" => 4,
            _ => 2,
        }
    }

    pub(crate) fn setting_field_kind(setting_id: &str) -> &'static str {
        if let Some(field) = crate::app_vtcomplete_settings::field(setting_id) {
            return field.2;
        }
        if setting_id == "animation_scope_bubble_languages" { return "action"; }
        if AnimationSettings::effect_for_duration_id(setting_id).is_some()
            || AnimationSettings::effect_for_intensity_id(setting_id).is_some()
        {
            return "number";
        }
        if matches!(setting_id, "animation_scope_bubble_tight" | "animation_scope_bubble_fill") {
            return "checkbox";
        }
        if setting_id == "animation_scope_bubble_disabled_languages" {
            return "language-list";
        }
        if AnimationSettings::effect_for_id(setting_id).is_some() {
            return "checkbox";
        }
        if setting_id.starts_with("heading_") {
            return "heading";
        }
        if setting_id.starts_with("kb_") {
            return "text";
        }
        match setting_id {
            "files_exclude" | "search_exclude" => "exclusions",
            "acp_agents" | "lsp_servers" | "formatters" | "language_associations" => "action",
            "animation_copy" => "checkbox",
            "animation_copy_duration_ms" => "number",
            "lsp_enabled" | "source_control_enabled" | "source_control_show_untracked" => "checkbox",
            "animation_duration_ms" | "animation_fps" | "animation_intensity" => "number",
            "tab_size" | "minimap_width" | "panel_height"
            | "auto_save_delay" | "lsp_diagnostic_delay_ms" | "scroll_speed" | "word_wrap_column"
            | "sticky_scroll_max" | "max_global_search_results" | "search_debounce_ms"
            | "cursor_surrounding_lines" | "large_file_threshold_mb"
            | "large_file_analysis_timeout_ms" => "number",
            "theme" | "sidebar_pos" | "rulers" | "cursor_style" | "auto_save_mode" | "whitespace" | "comment_whitespace" => "dropdown",
            "use_tabs" | "word_wrap" | "read_only" | "quick_suggestions" | "hover_enabled" | "format_on_type" | "explorer"
            | "show_open_editors" | "minimap" | "line_numbers" | "relative_lines" | "panel"
            | "zen" | "indent_rainbow" | "color_preview" | "bracket_pair_colorization" | "error_lens" | "sticky_scroll" | "focus_mode"
            | "word_count" | "trim_trailing" | "format_on_save" | "final_newline" | "scroll_past_end"
            | "indent_guides" | "activity_bar" | "breadcrumbs" | "status_bar" | "tab_bar" | "preview_tabs" | "mouse_back_forward" | "menu_bar"
            | "inlay_hints" | "code_lens" | "semantic_highlighting" | "screencast_mode"
            | "search_case_sensitive" | "search_whole_word" | "search_regex" | "search_preserve_case"
            | "cursor_blinking" => "checkbox",
            _ => "text",
        }
    }

    /// Map Settings UI identifiers to their serialized `settings.json` keys.
    /// Keeping this mapping in one place ensures scope writes and reset/source
    /// indicators always address the same field.
    pub(crate) fn setting_json_key(setting_id: &str) -> Option<&'static str> {
        if let Some(effect) = AnimationSettings::effect_for_duration_id(setting_id) {
            return Some(effect.duration_json_key());
        }
        if let Some(effect) = AnimationSettings::effect_for_intensity_id(setting_id) {
            return Some(effect.intensity_json_key());
        }
        if let Some(effect) = AnimationSettings::effect_for_id(setting_id) {
            return Some(effect.json_key());
        }
        if setting_id.starts_with("kb_") {
            return Some("keybindings");
        }
        Some(match setting_id {
            "animation_copy" => "animationCopy",
            "animation_copy_duration_ms" => "animationCopyDurationMs",
            "formatters" => "formatters",
            "language_associations" => "languageAssociations",
            "acp_agents" => "acpAgents",
            "lsp_servers" => "lspServers",
            "lsp_enabled" => "lspEnabled",
            "source_control_enabled" => "sourceControlEnabled",
            "source_control_show_untracked" => "sourceControlShowUntracked",
            "lsp_diagnostic_delay_ms" => "lspDiagnosticDelayMs",
            "animation_duration_ms" => "animationDurationMs",
            "animation_fps" => "animationFps",
            "animation_intensity" => "animationIntensity",
            "animation_scope_bubble_languages" => "animationScopeBubbleLanguages",
            "animation_scope_bubble_tight" => "animationScopeBubbleTight",
            "animation_scope_bubble_fill" => "animationScopeBubbleFill",
            "animation_scope_bubble_disabled_languages" => "animationScopeBubbleDisabledLanguages",
            "tab_size" => "tabSize",
            "use_tabs" => "useTabs",
            "word_wrap" => "wordWrap",
            "word_wrap_column" => "wordWrapColumn",
            "whitespace" => "whitespaceMode",
            "comment_whitespace" => "commentWhitespaceMode",
            "read_only" => "readOnly",
            "quick_suggestions" => "quickSuggestions",
            "hover_enabled" => "hoverEnabled",
            "format_on_type" => "formatOnType",
            "theme" => "theme",
            "explorer" => "showExplorer",
            "show_open_editors" => "showOpenEditors",
            "minimap" => "showMinimap",
            "minimap_width" => "minimapWidth",
            "line_numbers" => "showLineNumbers",
            "relative_lines" => "relativeLineNumbers",
            "indent_guides" => "showIndentGuides",
            "activity_bar" => "showActivityBar",
            "breadcrumbs" => "showBreadcrumbs",
            "status_bar" => "showStatusBar",
            "tab_bar" => "showTabBar",
            "preview_tabs" => "enablePreviewTabs",
            "mouse_back_forward" => "workbench.editor.mouseBackForwardToNavigate",
            "menu_bar" => "showMenuBar",
            "panel" => "showPanel",
            "panel_height" => "panelHeight",
            "sidebar_pos" => "sidebarRight",
            "zen" => "zenMode",
            "indent_rainbow" => "indentRainbow",
            "color_preview" => "colorPreview",
            "bracket_pair_colorization" => "bracketPairColorization",
            "error_lens" => "errorLens",
            "inlay_hints" => "inlayHints",
            "code_lens" => "codeLens",
            "semantic_highlighting" => "semanticHighlighting",
            "sticky_scroll" => "stickyScroll",
            "sticky_scroll_max" => "stickyScrollMax",
            "scroll_past_end" => "scrollPastEnd",
            "focus_mode" => "focusMode",
            "word_count" => "showWordCount",
            "screencast_mode" => "screencastMode",
            "rulers" => "rulers",
            "search_case_sensitive" => "searchCaseSensitive",
            "search_whole_word" => "searchWholeWord",
            "search_regex" => "searchRegex",
            "search_preserve_case" => "searchPreserveCase",
            "search_debounce_ms" => "search.searchOnTypeDebouncePeriod",
            "files_exclude" => "files.exclude",
            "search_exclude" => "search.exclude",
            "max_global_search_results" => "maxGlobalSearchResults",
            "large_file_threshold_mb" => "largeFileThresholdMb",
            "large_file_analysis_timeout_ms" => "largeFileAnalysisTimeoutMs",
            "trim_trailing" => "trimTrailingOnSave",
            "format_on_save" => "formatOnSave",
            "final_newline" => "insertFinalNewline",
            "cursor_style" => "cursorStyle",
            "cursor_blinking" => "cursorBlinking",
            "cursor_surrounding_lines" => "cursorSurroundingLines",
            "scroll_speed" => "scrollSpeed",
            "auto_save_mode" => "autoSave",
            "auto_save_delay" => "autoSaveDelay",
            _ => return None,
        })
    }

    pub(crate) fn setting_is_defined_in_layer(
        &self,
        setting_id: &str,
        settings: &crate::settings::Settings,
    ) -> bool {
        if crate::app_vtcomplete_settings::field(setting_id).is_some() {
            return self.completion_setting_modified(setting_id);
        }
        if setting_id == "word_count" && settings.status_bar_items.contains_key("word_count") {
            return true;
        }
        if let Some(index) = setting_id
            .strip_prefix("kb_")
            .and_then(|value| value.parse::<usize>().ok())
        {
            let command = self.keybindings.get(index).map(|binding| binding.id);
            return settings.keybindings.as_ref().is_some_and(|entries| {
                entries.iter().any(|entry| {
                    command.is_some_and(|command| entry.command.as_deref() == Some(command))
                        || (entry.command.is_none() && entry.index == Some(index))
                })
            });
        }
        let Some(key) = Self::setting_json_key(setting_id) else {
            return false;
        };
        serde_json::to_value(settings)
            .ok()
            .and_then(|value| value.get(key).cloned())
            .is_some_and(|value| !value.is_null())
    }

    pub(crate) fn setting_source_label(&self, setting_id: &str) -> &'static str {
        if crate::app_vtcomplete_settings::field(setting_id).is_some() {
            return if self.completion_setting_modified(setting_id) {
                "Project"
            } else {
                "Project default"
            };
        }
        if Self::setting_json_key(setting_id).is_none() {
            return "";
        }
        match self.settings_scope {
            crate::settings::SettingsScope::Global => {
                if self.setting_is_defined_in_layer(setting_id, &self.settings_global) {
                    "Global"
                } else {
                    "Default"
                }
            }
            crate::settings::SettingsScope::Workspace => {
                if self.setting_is_defined_in_layer(setting_id, &self.settings_workspace) {
                    "Workspace"
                } else if self.setting_is_defined_in_layer(setting_id, &self.settings_global) {
                    "Inherited · Global"
                } else {
                    "Inherited · Default"
                }
            }
        }
    }

    fn format_scoped_setting_value(
        &self,
        setting_id: &str,
        value: &serde_json::Value,
        fallback: &str,
    ) -> String {
        if matches!(setting_id, "files_exclude" | "search_exclude") { return value.to_string(); }
        if setting_id == "animation_scope_bubble_languages" {
            return format!(" Configure… ({}) ", value.as_object().map_or(0, serde_json::Map::len));
        }
        if setting_id == "acp_agents" {
            let count = value.as_array().map_or(0, Vec::len);
            return format!(" Manage Agents… ({count}) ");
        }
        if setting_id == "lsp_servers" {
            return format!(" Manage Servers… ({}) ", value.as_array().map_or(0, Vec::len));
        }
        if setting_id == "language_associations" {
            return format!(" Manage Associations… ({}) ", value.as_object().map_or(0, serde_json::Map::len));
        }
        if setting_id == "animation_scope_bubble_disabled_languages" {
            return value.as_array().map(|languages| {
                languages.iter().filter_map(serde_json::Value::as_str).collect::<Vec<_>>().join(", ")
            }).unwrap_or_else(|| fallback.to_owned());
        }
        if let Some(boolean) = value.as_bool() {
            return if boolean { "On" } else { "Off" }.to_string();
        }
        if value.is_number() {
            return value.to_string();
        }
        if setting_id == "rulers" {
            let Some(values) = value.as_array() else {
                return fallback.to_string();
            };
            if values.is_empty() {
                return "None".to_string();
            }
            return values
                .iter()
                .filter_map(serde_json::Value::as_u64)
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
                .join(", ");
        }
        let Some(text) = value.as_str() else {
            return fallback.to_string();
        };
        match setting_id {
            "theme" => self
                .loaded_themes
                .iter()
                .find(|theme| theme.config_key == text)
                .map(|theme| theme.name.clone())
                .unwrap_or_else(|| text.to_string()),
            "whitespace" | "comment_whitespace" | "cursor_style" => {
                let mut chars = text.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
                    .unwrap_or_default()
            }
            "auto_save_mode" => match text {
                "afterDelay" => "After Delay".to_string(),
                "onFocusChange" => "On Focus Change".to_string(),
                "onWindowChange" => "On Window Change".to_string(),
                _ => "Off".to_string(),
            },
            _ => text.to_string(),
        }
    }

    pub(crate) fn keybindings_for_settings(
        settings: &crate::settings::Settings,
    ) -> Vec<crate::app::KeyBinding> {
        let mut bindings = crate::app::default_keybindings();
        let Some(overrides) = settings.keybindings.as_ref() else {
            return bindings;
        };
        for entry in overrides {
            if let (Some(command), Some(key)) = (entry.command.as_deref(), entry.key.as_deref()) {
                if let Some((modifiers, code)) = Self::keybinding_from_settings_text(key) {
                    if let Some(binding) = bindings.iter_mut().find(|binding| binding.id == command)
                    {
                        binding.modifiers = modifiers;
                        binding.code = code;
                        continue;
                    }
                }
            }
            if let (Some(index), Some(modifiers), Some(key_code)) =
                (entry.index, entry.modifiers, entry.key_code.as_deref())
            {
                if let Some(code) = Self::keycode_from_settings_token(key_code) {
                    if let Some(binding) = bindings.get_mut(index) {
                        binding.modifiers =
                            crossterm::event::KeyModifiers::from_bits_truncate(modifiers);
                        binding.code = code;
                    }
                }
            }
        }
        bindings
    }

    fn scoped_keybinding_value(&self, setting_id: &str, fallback: &str) -> String {
        let Some(index) = setting_id
            .strip_prefix("kb_")
            .and_then(|value| value.parse::<usize>().ok())
        else {
            return fallback.to_string();
        };
        Self::keybindings_for_settings(&self.selected_scope_settings())
            .get(index)
            .map(crate::app::KeyBinding::display)
            .unwrap_or_else(|| fallback.to_string())
    }

    pub(crate) fn setting_description(setting_id: &str) -> &'static str {
        if let Some(field) = crate::app_vtcomplete_settings::field(setting_id) {
            return field.3;
        }
        if setting_id == AnimationEffect::ScopeConnector.duration_id() {
            return "Duration of the scope outline animation in milliseconds (1–5000). When Background Fill is enabled, its animation follows the outline and uses the same duration.";
        }
        if AnimationSettings::effect_for_duration_id(setting_id).is_some() {
            return "Duration of this effect in milliseconds (1–5000). Other animation durations are independent.";
        }
        if AnimationSettings::effect_for_intensity_id(setting_id).is_some() {
            return "Strength of this effect, from subtle (5) to vivid (100). Other animation intensities are independent.";
        }
        if let Some(effect) = AnimationSettings::effect_for_id(setting_id) {
            return effect.description();
        }
        if setting_id.starts_with("kb_") {
            return "";
        }
        if setting_id.starts_with("shortcut_ro_") {
            return "";
        }
        match setting_id {
            "files_exclude" => "Configure glob patterns for files and folders to hide in Explorer. Search inherits these patterns. Settings patterns are relative to the workspace root.",
            "search_exclude" => "Configure glob patterns to exclude from workspace text search and Quick Open. Inherits all patterns from Files: Exclude. Disable a pattern here to override an inherited exclusion.",
            "acp_agents" => "Add, edit, or remove local ACP agents. Configure names, executables, arguments, environment variables, and request timeouts with a form.",
            "lsp_servers" => "Set up language servers (LSP) for completion, diagnostics, navigation, and formatting. Add an executable, language ID, file patterns, arguments, and initialization options. Install the server separately. Saved servers override extension defaults for the same language.",
            "lsp_enabled" => "Enable language servers (LSP) from settings and extensions. Changes apply to open files immediately.",
            "source_control_enabled" => "Enable Git changes, history, blame and file decorations. Turn off to stop background Git work on slow or shared disks. Editing, saving and undo still work.",
            "source_control_show_untracked" => "Show untracked files in Source Control, include them in its change count, and show their Git badges and colors in Explorer. Turn off to skip untracked-file scanning on local and SSH workspaces. Tracked changes stay visible. Remembered across restarts.",
            "lsp_diagnostic_delay_ms" => "Wait this many milliseconds after the last edit before displaying updated LSP problems, underlines, and error messages. Completion stays responsive. Default: 1000. Set to 0 for immediate updates (maximum 10000).",
            "tab_size" => "Number of spaces a tab character visually uses in the editor.",
            "animation_duration_ms" => {
                "Fallback duration for decorative effects in milliseconds (1–5000). Copy Animation has its own duration."
            }
            "animation_copy" => "Briefly fade the selected code highlight out and back after Copy (Ctrl+C, Edit menu, or Command Palette). Enabled by default. Only visible selected cells animate; editing and clipboard contents stay unchanged. Runs locally in SSH mode.",
            "animation_copy_duration_ms" => "Total time in milliseconds for the copy highlight to fade out and return (1–5000). Default: 220. A new copy restarts the animation. Has no effect when Copy Animation is off.",
            "animation_fps" => {
                "Maximum animation frame rate (15–120). Only active effects request extra frames."
            }
            "animation_intensity" => {
                "Strength of animation highlights, from subtle (5) to vivid (100)."
            }
            "animation_scope_bubble_languages" => "Choose parsing styles and delimiter pairs for custom or built-in languages. Workspace rules override the same User language.",
            "animation_scope_bubble_tight" => {
                "Follow indentation and line endings closely. Turn off to keep each side outside the code without entering its internal whitespace."
            }
            "animation_scope_bubble_fill" => {
                "After the full scope outline is drawn, animate a subtle background color inward across the code. Uses Scope Bubble Duration and Intensity. The fill stays visible inside the scope and fades when leaving it. Off by default."
            }
            "animation_scope_bubble_disabled_languages" => {
                "Disable Scope Bubble for comma-separated language names or IDs, such as python, cpp, or JavaScript. Names are case-insensitive; an empty list allows every language. Workspace values replace the Global list."
            }
            "use_tabs" => "Insert tab characters instead of spaces when indenting.",
            "word_wrap" => "Wrap long lines so they stay visible without horizontal scrolling.",
            "word_wrap_column" => "Column at which to wrap lines (0 = viewport width).",
            "whitespace" => "Controls rendering of whitespace: None, Boundary (tabs, indentation, repeated and trailing spaces), Selection (only selected), Trailing, All.",
            "comment_whitespace" => "Render spaces as dots and tabs as arrows inside syntax-recognized comments. Defaults to All. Inherit uses Render Whitespace; other modes override it only inside comments.",
            "read_only" => "Prevent text edits in the active editor tab.",
            "quick_suggestions" => "Show VTComplete and LSP suggestions while typing. Turn off to request suggestions with Ctrl+Space. VTComplete file learning has its own mode.",
            "hover_enabled" => "Show language information when the pointer rests over editor text.",
            "format_on_type" => "When Language Server is the chosen formatter, adjust text after supported trigger characters. New lines are never formatted by this setting. External tools run on Format Document or Format on Save.",
            "theme" => "Select the active color theme for the whole editor.",
            "explorer" => "Show or hide the left sidebar explorer.",
            "show_open_editors" => "Show the Open Editors section above the file tree.",
            "minimap" => "Show or hide the minimap preview in editor panes.",
            "minimap_width" => "Set the minimap width in characters.",
            "line_numbers" => "Show absolute line numbers in the editor gutter.",
            "relative_lines" => "Show relative line numbers for faster motion navigation.",
            "panel" => "Show or hide the bottom panel (Output/Problems/Terminal).",
            "panel_height" => "Set the bottom panel height.",
            "sidebar_pos" => "Choose whether the sidebar is on the left or right side.",
            "zen" => "Toggle zen mode to reduce UI chrome.",
            "activity_bar" => "Show the Activity Bar used to switch between Explorer, Search, Source Control, and Outline.",
            "indent_rainbow" => "Color indentation levels for easier nesting visibility.",
            "color_preview" => "Show inline color swatches for hex, rgb(), and rgba() color values.",
            "bracket_pair_colorization" => "Color matching bracket pairs by nesting depth.",
            "error_lens" => "Show extra messages at the end of lines containing TODO, FIXME, HACK, BUG, or NOTE markers, plus inline language-server diagnostics. Off by default.",
            "inlay_hints" => "Show type, parameter, and other inline hints supplied by the language server.",
            "code_lens" => "Show actionable reference and implementation information supplied by the language server.",
            "semantic_highlighting" => "Use semantic tokens from the language server to refine syntax colors.",
            "sticky_scroll" => "Keep scope headers visible while scrolling large files.",
            "sticky_scroll_max" => "Maximum number of sticky scroll lines (1-5).",
            "scroll_past_end" => "Allow scrolling past the last line of the file.",
            "focus_mode" => "Dim unfocused UI regions to emphasize active content.",
            "word_count" => "Show live word count in the status bar.",
            "screencast_mode" => "Show recent key presses in an overlay for demonstrations and recordings.",
            "max_global_search_results" => "Maximum number of Search view results before truncating the list.",
            "large_file_threshold_mb" => "File size in MiB above which syntax, language services, Git diff, wrapping, and whole-file analysis are paused. Editing, scrolling, saving, search, line numbers, and the bounded minimap remain available.",
            "large_file_analysis_timeout_ms" => "Maximum time automatic rich analysis may run before those features are paused for the current file.",
            "search_case_sensitive" => "Use case-sensitive matching by default in editor and workspace searches.",
            "search_whole_word" => "Match complete words by default in editor and workspace searches.",
            "search_regex" => "Interpret search text as a regular expression by default.",
            "search_preserve_case" => "Preserve the capitalization pattern of each match when replacing text.",
            "search_debounce_ms" => "Delay in milliseconds after typing in workspace search or its include/exclude fields before searching. Set to 0 to search immediately. Find within a file stays instant.",
            "rulers" => "Choose visible column rulers (for example 80 and 120).",
            "trim_trailing" => "Remove trailing whitespace when saving files.",
            "format_on_save" => "Run the selected formatter before saving. Choose a formatter the first time you format each language.",
            "formatters" => "Choose and remember a formatter for each language: Language Server, External Command, or Disabled. Independent of completion and diagnostics. Configure the executable and arguments here.",
            "language_associations" => "Associate a filename or extension with a file type for syntax highlighting, language servers, and formatting. Supports *.py, *.d.ts, *.{c,h}, Dockerfile.*, and exact filenames. Exact filenames take priority. Save globally or for this workspace; remove a rule to restore the inherited or automatic type.",
            "final_newline" => "Ensure a newline exists at the end of saved files.",
            "indent_guides" => "Show vertical guides at indentation levels.",
            "breadcrumbs" => "Show breadcrumbs navigation above the editor.",
            "status_bar" => "Show or hide the status bar at the bottom.",
            "tab_bar" => "Show or hide the tab bar at the top of the editor.",
            "preview_tabs" => "Single-click files in Explorer to reuse an italic preview tab. Click the file again, double-click its tab, or edit it to keep it open.",
            "mouse_back_forward" => "Use the mouse side buttons to go back and forward through editor locations.",
            "menu_bar" => "Show or hide the menu bar.",
            "cursor_style" => "Set the cursor shape: Line, Block, or Underline.",
            "cursor_blinking" => "Enable cursor blinking animation.",
            "cursor_surrounding_lines" => "Keep at least this many visible lines above and below the cursor while scrolling.",
            "scroll_speed" => "Number of lines to scroll per mouse wheel step (1-10).",
            "auto_save_mode" => "Controls when files are automatically saved.",
            "auto_save_delay" => "Delay in milliseconds before auto-saving after the last edit (minimum 100).",
            _ => "",
        }
    }

    pub(crate) fn wrap_setting_text_for_width(text: &str, width: usize) -> Vec<String> {
        let width = width.max(1);
        let mut lines = Vec::new();
        let mut current = String::new();
        let mut current_len = 0usize;

        let push_word_chunks = |word: &str, out: &mut Vec<String>| {
            let mut chunk = String::new();
            let mut chunk_len = 0usize;
            for ch in word.chars() {
                if chunk_len >= width {
                    out.push(std::mem::take(&mut chunk));
                    chunk_len = 0;
                }
                chunk.push(ch);
                chunk_len += 1;
            }
            if !chunk.is_empty() {
                out.push(chunk);
            }
        };

        for word in text.split_whitespace() {
            let word_len = word.chars().count();
            if current_len == 0 {
                if word_len <= width {
                    current.push_str(word);
                    current_len = word_len;
                } else {
                    push_word_chunks(word, &mut lines);
                }
                continue;
            }

            if current_len + 1 + word_len <= width {
                current.push(' ');
                current.push_str(word);
                current_len += 1 + word_len;
            } else {
                lines.push(std::mem::take(&mut current));
                current_len = 0;
                if word_len <= width {
                    current.push_str(word);
                    current_len = word_len;
                } else {
                    push_word_chunks(word, &mut lines);
                }
            }
        }

        if !current.is_empty() {
            lines.push(current);
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        lines
    }

    pub(crate) fn setting_description_lines(label: &str, setting_id: &str, width: usize) -> Vec<String> {
        let desc = Self::setting_description(setting_id);
        let mut lines = vec![label.to_string()];
        if !desc.is_empty() {
            // The checkbox occupies three cells plus a space. Reserve that
            // indent on every description line, including wrapped lines.
            let width = if Self::setting_field_kind(setting_id) == "checkbox" {
                width.saturating_sub(4)
            } else {
                width
            };
            lines.extend(Self::wrap_setting_text_for_width(desc, width));
        }
        lines
    }

    pub(crate) fn setting_row_gap(width: usize) -> usize {
        if width >= 60 { 2 } else { 1 }
    }

    #[cfg(test)]
    pub(crate) fn setting_visual_height(label: &str, setting_id: &str, width: usize) -> usize {
        let lines = Self::setting_description_lines(label, setting_id, width).len();
        crate::settings_layout::visual_height(Self::setting_field_kind(setting_id), lines, width)
    }

    pub(crate) fn animation_setting_section(setting_id: &str) -> Option<&'static str> {
        if matches!(setting_id, "animation_copy" | "animation_copy_duration_ms") { return Some("Editing"); }
        let effect = AnimationSettings::effect_for_id(setting_id)
            .or_else(|| AnimationSettings::effect_for_duration_id(setting_id))
            .or_else(|| AnimationSettings::effect_for_intensity_id(setting_id))
            .or_else(|| {
                setting_id
                    .starts_with("animation_scope_bubble_")
                    .then_some(AnimationEffect::ScopeConnector)
            });
        let control_heading = match setting_id {
            "animation_duration_ms" | "animation_fps" | "animation_intensity" => Some("heading_animation_timing"),
            _ => None,
        };
        ANIMATION_SECTIONS.iter().find_map(|(label, heading_id, effects)| {
            (control_heading == Some(*heading_id)
                || effect.is_some_and(|effect| effects.contains(&effect)))
                .then_some(*label)
        })
    }

    pub(crate) fn settings_filtered_indices_for_category(&self, category_idx: usize) -> Vec<usize> {
        self.settings_filtered_indices_with_items(category_idx, &self.settings_items())
    }

    pub(crate) fn settings_filtered_indices_with_items(&self, category_idx: usize, items: &[(String, String, String)]) -> Vec<usize> {
        let query = self.settings_editor_search.trim();

        // VS Code's Settings search is global: the category tree scopes normal
        // browsing, but it must not hide matches from other pages while a query
        // is present.
        if !query.is_empty() {
            return items
                .iter()
                .enumerate()
                .filter(|(_, (label, _, id))| {
                    if id.starts_with("heading_") {
                        return false;
                    }
                    let category_idx = Self::settings_category_for_id(id);
                    let category = Self::settings_categories()
                        .get(category_idx)
                        .copied()
                        .unwrap_or("");
                    let child = Self::settings_section_for_id(id).unwrap_or("");
                    let searchable_id = id.replace('_', " ");
                    crate::app::contains_ignore_case(label, query)
                        || crate::app::contains_ignore_case(Self::setting_description(id), query)
                        || crate::app::contains_ignore_case(&searchable_id, query)
                        || crate::app::contains_ignore_case(category, query)
                        || crate::app::contains_ignore_case(child, query)
                        || Self::animation_setting_section(id)
                            .is_some_and(|section| crate::app::contains_ignore_case(section, query))
                })
                .map(|(idx, _)| idx)
                .collect();
        }

        if category_idx >= Self::settings_categories().len() {
            return Vec::new();
        }
        if category_idx == SETTINGS_ALL_CATEGORY {
            return (0..items.len()).collect();
        }
        let is_common_category = category_idx == 0;
        const COMMON_SETTING_IDS: &[&str] = &[
            "tab_size",
            "word_wrap",
            "theme",
            "explorer",
            "minimap",
            "breadcrumbs",
            "format_on_save",
            "trim_trailing",
            "auto_save_mode",
        ];
        items
            .iter()
            .enumerate()
            .filter(|(_, (_, _, id))| {
                if is_common_category {
                    if !COMMON_SETTING_IDS.contains(&id.as_str()) {
                        return false;
                    }
                    return true;
                }
                if Self::settings_category_for_id(id) != category_idx {
                    return false;
                }
                if let Some(section) = self.settings_selected_section(category_idx) {
                    return !id.starts_with("heading_") && Self::settings_section_for_id(id) == Some(section);
                }
                !id.starts_with("heading_")
                    || id.starts_with("heading_kb_")
                    || id.starts_with("heading_animation_")
            })
            .map(|(idx, _)| idx)
            .collect()
    }

    pub(crate) fn ensure_settings_selection_visible(&mut self) {
        let items = self.settings_items();
        let filtered = self.settings_filtered_indices_with_items(self.settings_category_selected, &items);
        if filtered.is_empty() {
            self.settings_editor_scroll = 0;
            self.settings_reveal_selected = false;
            return;
        }
        if !filtered.contains(&self.settings_selected)
            || items.get(self.settings_selected).is_none_or(|(_, _, id)| id.starts_with("heading_"))
        {
            if let Some(&first) = filtered.iter().find(|&&idx| !items[idx].2.starts_with("heading_")) {
                self.settings_selected = first;
            }
            self.settings_editor_scroll = 0;
            self.settings_reveal_selected = false;
            return;
        }
        let Some(area) = self.settings_sidebar_area.filter(|area| !area.is_empty()) else {
            // Resolve after the first paint knows the actual wrapping width.
            self.settings_reveal_selected = true;
            return;
        };
        self.settings_reveal_selected = false;
        let layout = self.settings_layout_cache.get(&items, &filtered, area.width.saturating_sub(3) as usize);
        let height = area.height as usize;
        let mut scroll = self.settings_editor_scroll.min(layout.max_scroll(height));
        if let Some(row) = layout.row_for_item(self.settings_selected) {
            let control = row.start + row.control;
            if control < scroll { scroll = row.start.max(control.saturating_add(1).saturating_sub(height)); }
            else if control >= scroll + height { scroll = control + 1 - height; }
        }
        self.settings_editor_scroll = scroll.min(layout.max_scroll(height));
    }

    /// Get settings items: (label, current_value_str, setting_id)
    pub(crate) fn settings_items(&self) -> Vec<(String, String, String)> {
        let mut animation_settings = AnimationSettings::default();
        animation_settings.apply(&self.selected_scope_settings());
        let tab_size = self.default_tab_size;
        let word_wrap = self.default_word_wrap;
        let whitespace = self.default_whitespace_mode;
        let read_only = self.default_read_only;
        let use_tabs = self.default_use_tabs;
        let show_activity_bar = if self.zen_mode {
            self.zen_pre_state.is_none_or(|state| state.5)
        } else {
            self.show_activity_bar
        };
        let on_off = |b: bool| -> String { if b { "On".into() } else { "Off".into() } };
        let theme_name = self.theme_name.as_str();
        let rulers_str = if self.rulers.is_empty() {
            "None".to_string()
        } else {
            self.rulers.iter().enumerate().fold(String::new(), |mut acc, (i, r)| {
                if i > 0 { acc.push_str(", "); }
                acc.push_str(&r.to_string());
                acc
            })
        };
        let mut items: Vec<(String, String, String)> = vec![
            // === Editor ===
            ("── Editor ──".into(), String::new(), "heading_editor".into()),
            ("Tab Size".into(), tab_size.to_string(), "tab_size".into()),
            ("Indent Using Tabs".into(), on_off(use_tabs), "use_tabs".into()),
            ("Word Wrap".into(), on_off(word_wrap), "word_wrap".into()),
            ("Word Wrap Column".into(), if self.word_wrap_column == 0 { "viewport".to_string() } else { self.word_wrap_column.to_string() }, "word_wrap_column".into()),
            ("Render Whitespace".into(), whitespace.name().to_string(), "whitespace".into()),
            ("Whitespace in Comments".into(), self.comment_whitespace_mode.map_or("Inherit", |mode| mode.name()).to_string(), "comment_whitespace".into()),
            ("Read Only".into(), on_off(read_only), "read_only".into()),
            ("Show Suggestions While Typing".into(), on_off(self.quick_suggestions), "quick_suggestions".into()),
            ("Hover".into(), on_off(self.hover_enabled), "hover_enabled".into()),
            ("Format On Type".into(), on_off(self.on_type_formatting_enabled), "format_on_type".into()),
            // === Appearance ===
            ("── Appearance ──".into(), String::new(), "heading_appearance".into()),
            ("Color Theme".into(), theme_name.to_string(), "theme".into()),
            ("Show Explorer".into(), on_off(self.show_explorer), "explorer".into()),
            ("Show Open Editors".into(), on_off(self.show_open_editors), "show_open_editors".into()),
            ("Show Minimap".into(), on_off(self.show_minimap), "minimap".into()),
            ("Minimap Width".into(), self.minimap_width.to_string(), "minimap_width".into()),
            ("Show Line Numbers".into(), on_off(self.show_line_numbers), "line_numbers".into()),
            ("Relative Line Numbers".into(), on_off(self.relative_line_numbers), "relative_lines".into()),
            ("Show Indent Guides".into(), on_off(self.show_indent_guides), "indent_guides".into()),
            ("Show Activity Bar".into(), on_off(show_activity_bar), "activity_bar".into()),
            ("Show Breadcrumbs".into(), on_off(self.show_breadcrumbs), "breadcrumbs".into()),
            ("Show Status Bar".into(), on_off(self.show_status_bar), "status_bar".into()),
            ("Show Tab Bar".into(), on_off(self.show_tab_bar), "tab_bar".into()),
            ("Preview Tabs".into(), on_off(self.enable_preview_tabs), "preview_tabs".into()),
            ("Mouse Back/Forward Navigation".into(), on_off(self.mouse_back_forward_to_navigate), "mouse_back_forward".into()),
            ("Show Menu Bar".into(), on_off(self.show_menu_bar), "menu_bar".into()),
            ("Show Bottom Panel".into(), on_off(self.show_panel), "panel".into()),
            ("Panel Height".into(), self.panel_height.to_string(), "panel_height".into()),
            ("Sidebar Position".into(), if self.sidebar_right { "Right" } else { "Left" }.into(), "sidebar_pos".into()),
            ("Zen Mode".into(), on_off(self.zen_mode), "zen".into()),
            // === Features ===
            ("── Features ──".into(), String::new(), "heading_features".into()),
            ("Indent Rainbow".into(), on_off(self.indent_rainbow), "indent_rainbow".into()),
            ("Color Preview".into(), on_off(self.show_color_preview), "color_preview".into()),
            ("Bracket Pair Colorization".into(), on_off(self.bracket_pair_colorization), "bracket_pair_colorization".into()),
            ("Error Lens".into(), on_off(self.error_lens), "error_lens".into()),
            ("Inlay Hints".into(), on_off(self.show_inlay_hints), "inlay_hints".into()),
            ("Code Lens".into(), on_off(self.show_code_lenses), "code_lens".into()),
            ("Semantic Highlighting".into(), on_off(self.show_semantic_tokens), "semantic_highlighting".into()),
            ("Sticky Scroll".into(), on_off(self.sticky_scroll), "sticky_scroll".into()),
            ("Sticky Scroll Max Lines".into(), self.sticky_scroll_max.to_string(), "sticky_scroll_max".into()),
            ("Scroll Past End".into(), on_off(self.scroll_past_end), "scroll_past_end".into()),
            ("Focus Mode".into(), on_off(self.focus_mode), "focus_mode".into()),
            ("Word Count in Status Bar".into(), on_off(self.show_word_count), "word_count".into()),
            ("Screencast Mode".into(), on_off(self.screencast_mode), "screencast_mode".into()),
            ("Column Rulers".into(), rulers_str, "rulers".into()),
            // === Search ===
            ("── Search ──".into(), String::new(), "heading_search".into()),
            ("Match Case".into(), on_off(self.search_case_sensitive), "search_case_sensitive".into()),
            ("Match Whole Word".into(), on_off(self.search_whole_word), "search_whole_word".into()),
            ("Use Regular Expression".into(), on_off(self.search_regex), "search_regex".into()),
            ("Preserve Case on Replace".into(), on_off(self.search_preserve_case), "search_preserve_case".into()),
            ("Search: Debounce (ms)".into(), self.search_debounce_ms.to_string(), "search_debounce_ms".into()),
            ("Search: Exclude".into(), String::new(), "search_exclude".into()),
            ("Files: Exclude".into(), String::new(), "files_exclude".into()),
            ("Search View Max Results".into(), self.max_global_search_results.to_string(), "max_global_search_results".into()),
            // === Save ===
            ("── Save ──".into(), String::new(), "heading_save".into()),
            ("Large File Threshold (MB)".into(), self.large_file_threshold_mb.to_string(), "large_file_threshold_mb".into()),
            ("Rich Analysis Timeout (ms)".into(), self.large_file_analysis_timeout_ms.to_string(), "large_file_analysis_timeout_ms".into()),
            ("Trim Trailing Whitespace".into(), on_off(self.trim_trailing_on_save), "trim_trailing".into()),
            ("Format On Save".into(), on_off(self.format_on_save), "format_on_save".into()),
            ("Formatters".into(), "Configure…".into(), "formatters".into()),
            ("File Associations".into(), "Manage Associations…".into(), "language_associations".into()),
            ("Insert Final Newline".into(), on_off(self.insert_final_newline), "final_newline".into()),
            // === Cursor & Auto Save ===
            ("── Cursor & Auto Save ──".into(), String::new(), "heading_cursor".into()),
            ("Cursor Style".into(), match self.cursor_style {
                crate::app::CursorStyle::Line => "Line",
                crate::app::CursorStyle::Block => "Block",
                crate::app::CursorStyle::Underline => "Underline",
            }.into(), "cursor_style".into()),
            ("Cursor Blinking".into(), on_off(self.cursor_blinking), "cursor_blinking".into()),
            ("Cursor Surrounding Lines".into(), self.default_cursor_surrounding_lines.to_string(), "cursor_surrounding_lines".into()),
            ("Scroll Speed".into(), self.scroll_speed.to_string(), "scroll_speed".into()),
            ("Auto Save".into(), match self.auto_save {
                AutoSaveMode::Off => "Off",
                AutoSaveMode::AfterDelay => "After Delay",
                AutoSaveMode::OnFocusChange => "On Focus Change",
                AutoSaveMode::OnWindowChange => "On Window Change",
            }.into(), "auto_save_mode".into()),
            ("Auto Save Delay (ms)".into(), self.auto_save_delay.to_string(), "auto_save_delay".into()),
        ];
        items.push(("Configured Agents".into(), " Manage Agents… (0) ".into(), "acp_agents".into()));
        items.push(("Enable Language Servers".into(), on_off(self.lsp_enabled), "lsp_enabled".into()));
        items.push(("Enable Source Control".into(), on_off(self.source_control_enabled), "source_control_enabled".into()));
        items.push(("Show Untracked Files".into(), on_off(self.sc_visibility.untracked), "source_control_show_untracked".into()));
        items.push(("Diagnostic Delay (ms)".into(), self.lsp_manager.diagnostic_publication.delay_ms().to_string(), "lsp_diagnostic_delay_ms".into()));
        items.push(("Language Server Setup".into(), " Manage Servers… (0) ".into(), "lsp_servers".into()));
        for &(label, heading_id, effects) in ANIMATION_SECTIONS {
            items.push((format!("── {label} ──"), String::new(), heading_id.into()));
            if heading_id == "heading_animation_editing" {
                let scoped = self.selected_scope_settings();
                items.push(("Copy Animation".into(), on_off(scoped.animation_copy.unwrap_or(true)), "animation_copy".into()));
                items.push(("Copy Animation Duration (ms)".into(),
                    scoped.animation_copy_duration_ms.unwrap_or(crate::copy_animation::DEFAULT_DURATION_MS).to_string(),
                    "animation_copy_duration_ms".into()));
            }
            for &effect in effects {
                items.push((
                    effect.label().into(),
                    on_off(animation_settings.enabled(effect)),
                    effect.id().into(),
                ));
                items.push((
                    format!("{} Duration (ms)", effect.label()),
                    animation_settings.duration(effect).to_string(),
                    effect.duration_id().into(),
                ));
                items.push((
                    format!("{} Intensity", effect.label()),
                    animation_settings.intensity(effect).to_string(),
                    effect.intensity_id().into(),
                ));
                if effect == AnimationEffect::ScopeConnector {
                    items.extend([(
                        "Scope Bubble: Tight Outline".into(),
                        on_off(animation_settings.scope_tight),
                        "animation_scope_bubble_tight".into(),
                    )]);
                    items.push((
                        "Scope Bubble: Background Fill".into(),
                        on_off(animation_settings.scope_fill),
                        "animation_scope_bubble_fill".into(),
                    ));
                    items.push((
                        "Scope Bubble: Language Rules".into(),
                        "Configure...".into(),
                        "animation_scope_bubble_languages".into(),
                    ));
                    items.push((
                        "Scope Bubble: Disabled Languages".into(),
                        animation_settings.scope_disabled_languages.join(", "),
                        "animation_scope_bubble_disabled_languages".into(),
                    ));
                }
            }
            if heading_id == "heading_animation_timing" {
                items.extend([(
                    "Maximum Frame Rate".into(),
                    animation_settings.fps.to_string(),
                    "animation_fps".into(),
                )]);
            }
        }
        items.extend(self.completion_setting_items());
        // === Keyboard Shortcuts ===
        items.push(("── Keyboard Shortcuts ──".into(), String::new(), "heading_shortcuts".into()));
        for (label, value, setting_id) in &self.keybinding_settings_cache {
            items.push((label.clone(), value.clone(), setting_id.clone()));
        }
        let scoped = serde_json::to_value(self.selected_scope_settings()).ok();
        for (_, value, id) in &mut items {
            if id.starts_with("kb_") {
                *value = self.scoped_keybinding_value(id, value);
                continue;
            }
            let Some(key) = Self::setting_json_key(id) else {
                continue;
            };
            if let Some(scoped_value) = scoped.as_ref().and_then(|settings| settings.get(key)) {
                *value = self.format_scoped_setting_value(id, scoped_value, value);
            }
        }
        items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── settings_categories ──────────────────────────────────────────

    #[test]
    fn categories_count() {
        let cats = App::settings_categories();
        assert_eq!(cats.len(), 11);
    }

    #[test]
    fn all_settings_includes_every_control_and_search_can_be_cleared() {
        let mut app = App::test_new();
        app.rebuild_keybinding_settings_cache();
        app.select_settings_page(1, Some("Cursor"));
        app.select_settings_page(SETTINGS_ALL_CATEGORY, None);
        assert_eq!(App::settings_categories()[SETTINGS_ALL_CATEGORY], "All");
        assert_eq!(app.settings_selected_section(SETTINGS_ALL_CATEGORY), None);

        let items = app.settings_items();
        let all: Vec<_> = (0..items.len()).collect();
        assert_eq!(app.settings_filtered_indices_with_items(SETTINGS_ALL_CATEGORY, &items), all);
        assert!(items.iter().any(|(_, _, id)| id.starts_with("kb_")));
        assert!(all.contains(&app.settings_selected));
        assert!(!items[app.settings_selected].2.starts_with("heading_"));

        app.settings_editor_search = "Tab Size".into();
        let matches = app.settings_filtered_indices_with_items(SETTINGS_ALL_CATEGORY, &items);
        assert!(!matches.is_empty());
        assert!(matches.iter().all(|&index| items[index].2 == "tab_size"));
        app.settings_editor_search.clear();
        assert_eq!(app.settings_filtered_indices_with_items(SETTINGS_ALL_CATEGORY, &items), all);
    }

    #[test]
    fn animations_have_their_own_category_with_copy_on_and_decorative_effects_off() {
        let mut app = App::test_new();
        app.settings_category_selected = 7;
        assert_eq!(App::settings_categories()[7], "Animations");
        let items = app.settings_items();
        let filtered = app.settings_filtered_indices_for_category(7);
        assert_eq!(items.iter().find(|(_, _, id)| id == "animation_copy").unwrap().1, "On");
        assert_eq!(items.iter().find(|(_, _, id)| id == "animation_copy_duration_ms").unwrap().1, "220");
        assert_eq!(
            filtered
                .iter()
                .filter(|&&index| !items[index].2.starts_with("heading_"))
                .count(),
            AnimationEffect::ALL.len() * 3 + 7
        );
        for effect in AnimationEffect::ALL {
            assert_eq!(App::settings_category_for_id(effect.id()), 7);
            assert_eq!(App::setting_field_kind(effect.id()), "checkbox");
            assert_eq!(App::setting_json_key(effect.id()), Some(effect.json_key()));
            assert_eq!(App::setting_description(effect.id()), effect.description());
            let index = items.iter().position(|(_, _, id)| id == effect.id()).unwrap();
            assert!(filtered.contains(&index));
            assert_eq!(items[index].1, "Off");
            for id in [effect.duration_id(), effect.intensity_id()] {
                assert_eq!(App::setting_field_kind(id), "number");
                assert_eq!(App::settings_category_for_id(id), 7);
                assert!(!App::setting_description(id).is_empty());
                assert!(items.iter().any(|(_, _, candidate)| candidate == id));
            }
        }
        for id in ["animation_fps"] {
            assert_eq!(App::setting_field_kind(id), "number");
            assert_eq!(App::settings_category_for_id(id), 7);
        }
        assert!(!items.iter().any(|(_, _, id)| matches!(
            id.as_str(),
            "animation_duration_ms"
                | "animation_intensity"
                | "animation_scope_bubble_glow"
                | "animation_scope_bubble_glow_duration_ms"
        )));
        for (id, expected) in [("animation_scope_bubble_tight", "On"), ("animation_scope_bubble_fill", "Off")] {
            assert_eq!(App::setting_field_kind(id), "checkbox");
            assert_eq!(
                items
                    .iter()
                    .find(|(_, _, candidate)| candidate == id)
                    .unwrap()
                    .1,
                expected
            );
        }
        app.settings_category_collapsed[7] = false;
        assert!(app.settings_category_visual_rows().contains(&(7, true, "Code Structure")));
    }

    #[test]
    fn animations_are_searchable_from_other_categories() {
        let mut app = App::test_new();
        app.settings_editor_search = "endif".into();
        let items = app.settings_items();
        let filtered = app.settings_filtered_indices_for_category(0);
        assert!(filtered.iter().any(|&index| items[index].2 == "animation_scope_connector"));
        app.settings_editor_search = "code structure".into();
        let filtered = app.settings_filtered_indices_for_category(0);
        let ids: Vec<_> = filtered
            .iter()
            .map(|&index| items[index].2.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "animation_scope_connector",
                "animation_scope_connector_duration_ms",
                "animation_scope_connector_intensity",
                "animation_scope_bubble_tight",
                "animation_scope_bubble_fill",
                "animation_scope_bubble_languages",
                "animation_scope_bubble_disabled_languages",
            ]
        );
    }

    #[test]
    fn animation_controls_display_the_selected_scope_and_legacy_fallbacks() {
        let mut app = App::test_new();
        app.settings_global.animation_duration_ms = Some(390);
        app.settings_global.animation_intensity = Some(61);
        app.settings_workspace.animation_scope_bubble_duration_ms = Some(870);
        app.settings_workspace.animation_scope_bubble_intensity = Some(93);
        app.settings_workspace.animation_scope_bubble_tight = Some(false);
        app.apply_effective_settings();
        for (scope, duration, intensity, tight) in [
            (crate::settings::SettingsScope::Global, "390", "61", "On"),
            (
                crate::settings::SettingsScope::Workspace,
                "870",
                "93",
                "Off",
            ),
        ] {
            app.settings_scope = scope;
            let items = app.settings_items();
            for (id, expected) in [
                ("animation_scope_connector_duration_ms", duration),
                ("animation_scope_connector_intensity", intensity),
                ("animation_scope_bubble_tight", tight),
                ("animation_cursor_smear_duration_ms", "390"),
            ] {
                assert_eq!(
                    items
                        .iter()
                        .find(|(_, _, candidate)| candidate == id)
                        .unwrap()
                        .1,
                    expected
                );
            }
        }
    }

    #[test]
    fn categories_first_is_commonly_used() {
        assert_eq!(App::settings_categories()[0], "Commonly Used");
    }

    #[test]
    fn categories_includes_agent_chat_page() {
        let cats = App::settings_categories();
        assert_eq!(cats[6], "Agent Chat");
    }

    #[test]
    fn categories_contains_keyboard_shortcuts() {
        assert!(App::settings_categories().contains(&"Keyboard Shortcuts"));
    }

    #[test]
    fn category_tree_keeps_other_branches_collapsed_until_opened() {
        let mut app = App::test_new();
        app.settings_category_selected = 2;
        app.settings_category_collapsed[2] = false;

        let rows = app.settings_category_visual_rows();
        assert!(rows.contains(&(2, false, "Workbench")));
        assert!(rows.contains(&(2, true, "Appearance")));
        assert!(!rows.iter().any(|&(idx, child, _)| idx == 1 && child));
    }

    // ── settings_category_for_id ─────────────────────────────────────

    #[test]
    fn category_editor_settings() {
        assert_eq!(App::settings_category_for_id("tab_size"), 1);
        assert_eq!(App::settings_category_for_id("use_tabs"), 1);
        assert_eq!(App::settings_category_for_id("word_wrap"), 1);
        assert_eq!(App::settings_category_for_id("whitespace"), 1);
        assert_eq!(App::settings_category_for_id("read_only"), 1);
        assert_eq!(App::settings_category_for_id("quick_suggestions"), 1);
        assert_eq!(App::settings_category_for_id("hover_enabled"), 1);
        assert_eq!(App::settings_category_for_id("format_on_type"), 1);
    }

    #[test]
    fn category_features_settings() {
        assert_eq!(App::settings_category_for_id("indent_rainbow"), 3);
        assert_eq!(App::settings_category_for_id("color_preview"), 3);
        assert_eq!(App::settings_category_for_id("sticky_scroll"), 3);
        assert_eq!(App::settings_category_for_id("rulers"), 3);
        assert_eq!(App::settings_category_for_id("inlay_hints"), 3);
        assert_eq!(App::settings_category_for_id("semantic_highlighting"), 3);
        assert_eq!(App::settings_category_for_id("search_regex"), 3);
    }

    #[test]
    fn category_save_settings() {
        assert_eq!(App::settings_category_for_id("trim_trailing"), 4);
        assert_eq!(App::settings_category_for_id("format_on_save"), 1);
        assert_eq!(App::settings_category_for_id("cursor_style"), 1);
        assert_eq!(App::settings_category_for_id("auto_save_mode"), 4);
        assert_eq!(App::settings_category_for_id("cursor_surrounding_lines"), 1);
    }

    #[test]
    fn category_keyboard_shortcuts() {
        assert_eq!(App::settings_category_for_id("kb_copy"), 5);
        assert_eq!(App::settings_category_for_id("heading_shortcuts"), 5);
        assert_eq!(App::settings_category_for_id("heading_kb_editor"), 5);
        assert_eq!(App::settings_category_for_id("shortcut_ro_ctrl_c"), 5);
    }

    #[test]
    fn category_workbench_is_default() {
        assert_eq!(App::settings_category_for_id("theme"), 2);
        assert_eq!(App::settings_category_for_id("explorer"), 2);
        assert_eq!(App::settings_category_for_id("minimap"), 1);
    }

    #[test]
    fn category_heading_editor_is_editor() {
        assert_eq!(App::settings_category_for_id("heading_editor"), 1);
    }

    #[test]
    fn category_heading_features_is_features() {
        assert_eq!(App::settings_category_for_id("heading_features"), 3);
    }

    // ── setting_field_kind ───────────────────────────────────────────

    #[test]
    fn field_kind_headings() {
        assert_eq!(App::setting_field_kind("heading_editor"), "heading");
        assert_eq!(App::setting_field_kind("heading_features"), "heading");
        assert_eq!(App::setting_field_kind("heading_shortcuts"), "heading");
    }

    #[test]
    fn field_kind_numbers() {
        assert_eq!(App::setting_field_kind("tab_size"), "number");
        assert_eq!(App::setting_field_kind("minimap_width"), "number");
        assert_eq!(App::setting_field_kind("panel_height"), "number");
        assert_eq!(App::setting_field_kind("auto_save_delay"), "number");
        assert_eq!(App::setting_field_kind("scroll_speed"), "number");
        assert_eq!(App::setting_field_kind("sticky_scroll_max"), "number");
        assert_eq!(App::setting_field_kind("max_global_search_results"), "number");
        assert_eq!(App::setting_field_kind("cursor_surrounding_lines"), "number");
        assert_eq!(App::setting_field_kind("large_file_threshold_mb"), "number");
        assert_eq!(App::setting_field_kind("large_file_analysis_timeout_ms"), "number");
    }

    #[test]
    fn field_kind_dropdowns() {
        assert_eq!(App::setting_field_kind("theme"), "dropdown");
        assert_eq!(App::setting_field_kind("sidebar_pos"), "dropdown");
        assert_eq!(App::setting_field_kind("rulers"), "dropdown");
        assert_eq!(App::setting_field_kind("cursor_style"), "dropdown");
        assert_eq!(App::setting_field_kind("auto_save_mode"), "dropdown");
        assert_eq!(App::setting_field_kind("whitespace"), "dropdown");
    }

    #[test]
    fn field_kind_checkboxes() {
        assert_eq!(App::setting_field_kind("use_tabs"), "checkbox");
        assert_eq!(App::setting_field_kind("word_wrap"), "checkbox");
        assert_eq!(App::setting_field_kind("minimap"), "checkbox");
        assert_eq!(App::setting_field_kind("zen"), "checkbox");
        assert_eq!(App::setting_field_kind("quick_suggestions"), "checkbox");
        assert_eq!(App::setting_field_kind("inlay_hints"), "checkbox");
        assert_eq!(App::setting_field_kind("search_regex"), "checkbox");
    }

    #[test]
    fn field_kind_keyboard_bindings_are_text() {
        assert_eq!(App::setting_field_kind("kb_copy"), "text");
        assert_eq!(App::setting_field_kind("kb_paste"), "text");
    }

    #[test]
    fn field_kind_unknown_is_text() {
        assert_eq!(App::setting_field_kind("nonexistent_setting"), "text");
    }

    #[test]
    fn every_editable_setting_has_a_persistence_target() {
        let app = App::test_new();
        for (_, _, id) in app.settings_items() {
            if id.starts_with("heading_") || id.starts_with("shortcut_ro_") {
                continue;
            }
            assert!(
                App::setting_json_key(&id).is_some()
                    || crate::app_vtcomplete_settings::field(&id).is_some(),
                "setting {id} has no general or project-completion persistence target"
            );
        }
    }

    #[test]
    fn source_labels_explain_inheritance_for_each_scope() {
        let mut app = App::test_new();
        assert_eq!(app.setting_source_label("tab_size"), "Default");
        app.settings_global.tab_size = Some(2);
        assert_eq!(app.setting_source_label("tab_size"), "Global");
        app.settings_scope = crate::settings::SettingsScope::Workspace;
        assert_eq!(app.setting_source_label("tab_size"), "Inherited · Global");
        app.settings_workspace.tab_size = Some(8);
        assert_eq!(app.setting_source_label("tab_size"), "Workspace");
    }

    // ── setting_description ──────────────────────────────────────────

    #[test]
    fn description_tab_size_nonempty() {
        let desc = App::setting_description("tab_size");
        assert!(!desc.is_empty());
        assert!(desc.contains("tab"));
    }

    #[test]
    fn description_theme_nonempty() {
        let desc = App::setting_description("theme");
        assert!(!desc.is_empty());
        assert!(desc.contains("theme"));
    }

    #[test]
    fn description_keyboard_binding_is_empty() {
        assert_eq!(App::setting_description("kb_copy"), "");
        assert_eq!(App::setting_description("kb_paste"), "");
    }

    #[test]
    fn description_shortcut_ro_is_empty() {
        assert_eq!(App::setting_description("shortcut_ro_ctrl_c"), "");
    }

    #[test]
    fn description_unknown_is_empty() {
        assert_eq!(App::setting_description("nonexistent"), "");
    }

    #[test]
    fn description_all_known_settings_consistent() {
        let known = ["tab_size", "use_tabs", "word_wrap", "theme", "explorer",
            "minimap", "line_numbers", "panel", "zen", "trim_trailing",
            "cursor_style", "auto_save_mode"];
        for id in &known {
            let desc = App::setting_description(id);
            assert!(!desc.is_empty(), "setting {id} should have a description");
        }
    }

    #[test]
    fn expanded_settings_are_present_and_documented() {
        let app = App::test_new();
        let items = app.settings_items();
        let ids: Vec<&str> = items.iter().map(|(_, _, id)| id.as_str()).collect();
        let expected = [
            "quick_suggestions", "hover_enabled", "format_on_type", "activity_bar",
            "inlay_hints", "code_lens", "semantic_highlighting", "screencast_mode",
            "search_case_sensitive", "search_whole_word", "search_regex",
            "search_preserve_case", "cursor_surrounding_lines",
        ];
        for id in expected {
            assert!(ids.contains(&id), "missing settings item {id}");
            assert!(!App::setting_description(id).is_empty(), "missing description for {id}");
        }
    }

    #[test]
    fn settings_search_ignores_the_selected_category() {
        let mut app = App::test_new();
        app.settings_category_selected = 2; // Workbench
        app.settings_editor_search = "format on save".into();

        let items = app.settings_items();
        let ids = app
            .settings_filtered_indices_for_category(app.settings_category_selected)
            .into_iter()
            .map(|idx| items[idx].2.as_str())
            .collect::<Vec<_>>();

        assert!(ids.contains(&"format_on_save"));
        assert_eq!(App::settings_category_for_id("format_on_save"), 1);
    }

    #[test]
    fn clearing_settings_search_restores_category_filtering() {
        let mut app = App::test_new();
        app.settings_category_selected = 2; // Workbench
        app.settings_editor_search.clear();

        let items = app.settings_items();
        let ids = app
            .settings_filtered_indices_for_category(app.settings_category_selected)
            .into_iter()
            .map(|idx| items[idx].2.as_str())
            .collect::<Vec<_>>();

        assert!(!ids.contains(&"format_on_save"));
        assert!(ids.contains(&"theme"));
    }

    // ── wrap_setting_text_for_width ──────────────────────────────────

    #[test]
    fn wrap_empty_string() {
        let result = App::wrap_setting_text_for_width("", 40);
        assert_eq!(result, vec![""]);
    }

    #[test]
    fn wrap_short_text_fits() {
        let result = App::wrap_setting_text_for_width("hello world", 40);
        assert_eq!(result, vec!["hello world"]);
    }

    #[test]
    fn wrap_exact_width() {
        let result = App::wrap_setting_text_for_width("hello", 5);
        assert_eq!(result, vec!["hello"]);
    }

    #[test]
    fn wrap_splits_at_word_boundary() {
        let result = App::wrap_setting_text_for_width("hello world", 6);
        assert_eq!(result, vec!["hello", "world"]);
    }

    #[test]
    fn wrap_long_word_chunked() {
        let result = App::wrap_setting_text_for_width("abcdefghij", 3);
        assert_eq!(result, vec!["abc", "def", "ghi", "j"]);
    }

    #[test]
    fn wrap_mixed_short_and_long() {
        let result = App::wrap_setting_text_for_width("hi abcdef", 4);
        assert_eq!(result, vec!["hi", "abcd", "ef"]);
    }

    #[test]
    fn wrap_width_one() {
        let result = App::wrap_setting_text_for_width("ab cd", 1);
        assert_eq!(result, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn wrap_multiple_spaces_collapsed() {
        let result = App::wrap_setting_text_for_width("a   b   c", 40);
        assert_eq!(result, vec!["a b c"]);
    }

    #[test]
    fn wrap_realistic_description() {
        let desc = "Number of spaces a tab character visually uses in the editor.";
        let result = App::wrap_setting_text_for_width(desc, 30);
        assert!(result.len() >= 2);
        for line in &result {
            assert!(line.chars().count() <= 30, "line too long: {line}");
        }
    }

    // ── setting_description_lines ────────────────────────────────────

    #[test]
    fn description_lines_with_desc() {
        let lines = App::setting_description_lines("Tab Size", "tab_size", 80);
        assert!(!lines.is_empty());
        let joined = lines.join(" ");
        assert!(joined.contains("Tab Size"));
        assert!(joined.contains("Number of spaces"));
    }

    #[test]
    fn description_lines_without_desc() {
        let lines = App::setting_description_lines("Ctrl+C", "kb_copy", 80);
        assert_eq!(lines, vec!["Ctrl+C"]);
    }

    #[test]
    fn description_lines_wraps_narrow() {
        let lines = App::setting_description_lines("Tab Size", "tab_size", 20);
        assert!(lines.len() > 1, "should wrap at narrow width");
    }

    // ── setting_visual_height ────────────────────────────────────────

    #[test]
    fn visual_height_heading_minimum_one() {
        let h = App::setting_visual_height("Editor", "heading_editor", 80);
        assert!(h >= 1);
    }

    #[test]
    fn visual_height_regular_includes_input_row() {
        let h = App::setting_visual_height("Tab Size", "tab_size", 80);
        let desc_lines = App::setting_description_lines("Tab Size", "tab_size", 80).len();
        assert_eq!(h, desc_lines + 4); // control spacing, input, and two-row gap
    }

    #[test]
    fn visual_height_narrow_keeps_wrapped_description_and_input() {
        let narrow = App::setting_visual_height("Tab Size", "tab_size", 20);
        let lines = App::setting_description_lines("Tab Size", "tab_size", 20);
        assert_eq!(narrow, lines.len() + 2, "input and compact spacing follow the description");
    }
}
