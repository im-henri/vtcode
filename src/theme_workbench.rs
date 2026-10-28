//! Independently resolved VS Code workbench colors used by vtcode renderers.
use crate::app_theme::ThemeColors;
use ratatui::style::{Color, Modifier, Style};
use serde_json::{Map, Value};

/// Preserve alpha until the owning surface is known. Controls appear on sidebar,
/// editor, input and terminal surfaces. This cache avoids parsing or allocating
/// while drawing, and prevents blending every control on the sidebar.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ColorLayer(Option<crate::textmate::Color>);

impl ColorLayer {
    pub(crate) fn resolve(colors: &Map<String, Value>, id: &str, fallback: Self) -> Self {
        if colors.get(id).is_some_and(Value::is_null) {
            return Self::default();
        }
        colors
            .get(id)
            .and_then(Value::as_str)
            .and_then(crate::vscode_theme::parse_syntax_color)
            .map(|color| Self(Some(color)))
            .unwrap_or(fallback)
    }

    pub(crate) fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self(Some(crate::textmate::Color { r, g, b, a }))
    }

    pub(crate) fn opaque(color: Color) -> Self {
        match color {
            Color::Rgb(r, g, b) => Self::rgba(r, g, b, 255),
            _ => Self::default(),
        }
    }

    fn with_alpha(mut self, alpha: u8) -> Self {
        if let Some(color) = self.0.as_mut() {
            color.a = alpha;
        }
        self
    }

    fn with_opacity(mut self, opacity: f32) -> Self {
        if let Some(color) = self.0.as_mut() {
            color.a = (f32::from(color.a) * opacity.clamp(0.0, 1.0)).round() as u8;
        }
        self
    }

    fn with_lightness(mut self, factor: f32) -> Self {
        if let Some(color) = self.0.as_mut()
            && let Color::Rgb(r, g, b) = adjust_lightness(Color::Rgb(color.r, color.g, color.b), factor) {
            (color.r, color.g, color.b) = (r, g, b);
        }
        self
    }

    pub(crate) fn over(self, surface: Color) -> Option<Color> {
        let color = self.0?;
        if color.a == 0 {
            return Some(surface);
        }
        if color.a == 255 {
            return Some(Color::Rgb(color.r, color.g, color.b));
        }
        let Color::Rgb(r, g, b) = surface else {
            return None;
        };
        let alpha = u16::from(color.a);
        let blend = |front: u8, back: u8| {
            ((u16::from(front) * alpha + u16::from(back) * (255 - alpha) + 127) / 255) as u8
        };
        Some(Color::Rgb(
            blend(color.r, r),
            blend(color.g, g),
            blend(color.b, b),
        ))
    }

    pub(crate) fn border(self, surface: Color) -> Option<Color> {
        self.0.filter(|color| color.a != 0)?;
        self.over(surface)
    }

    pub(crate) fn is_transparent(self) -> bool {
        self.0.is_some_and(|color| color.a == 0)
    }

    pub(crate) fn or(self, fallback: Self) -> Self {
        Self(self.0.or(fallback.0))
    }
}

/// A control's fill, text and outline are layered on the actual row behind it.
/// Inherited input/dropdown roles retain their alpha just like explicit roles.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ControlColors {
    pub background: ColorLayer,
    pub foreground: ColorLayer,
    pub border: ColorLayer,
}

/// VS Code's hover defaults change HSL lightness, retaining the color's hue
/// and saturation rather than mixing the fill with an unrelated text color.
fn adjust_lightness(color: Color, factor: f32) -> Color {
    let Color::Rgb(r, g, b) = color else {
        return color;
    };
    let (r, g, b) = (
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    );
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let lightness = (max + min) / 2.0;
    let delta = max - min;
    let (hue, saturation) = if delta <= f32::EPSILON {
        (0.0, 0.0)
    } else {
        let hue = if max == r {
            ((g - b) / delta).rem_euclid(6.0)
        } else if max == g {
            (b - r) / delta + 2.0
        } else {
            (r - g) / delta + 4.0
        };
        (hue * 60.0, delta / (1.0 - (2.0 * lightness - 1.0).abs()))
    };
    let (r, g, b) = crate::ui::hsl_to_rgb(hue, saturation, (lightness * factor).clamp(0.0, 1.0));
    Color::Rgb(r, g, b)
}

const SYMBOL_COLOR_NAMES: [&str; 33] = [
    "array", "boolean", "class", "color", "constant", "constructor",
    "enumerator", "enumeratorMember", "event", "field", "file", "folder",
    "function", "interface", "key", "keyword", "method", "module",
    "namespace", "null", "number", "object", "operator", "package",
    "property", "reference", "snippet", "string", "struct", "text",
    "typeParameter", "unit", "variable",
];

#[derive(Clone, Copy)]
pub(crate) struct WorkbenchColors {
    /// Imported themes already specify their appearance. Do not substitute
    /// palette colors to meet vtcode's legacy contrast thresholds.
    pub preserve_theme_colors: bool,
    /// Neutral text and surface colors from the loaded theme, in fallback order.
    pub contrast_palette: [Color; 14],
    pub workbench_foreground: ColorLayer,
    pub description_foreground: ColorLayer,
    pub editor_foreground: ColorLayer,
    pub ruler_foreground: ColorLayer,
    pub codelens_foreground: ColorLayer,
    pub inlay_colors: [ControlColors; 3],
    pub sticky_background: ColorLayer,
    pub sticky_hover_background: ColorLayer,
    pub sticky_border: ColorLayer,
    pub sidebar_foreground: ColorLayer,
    pub sidebar_title_foreground: ColorLayer,
    pub git_decorations: [ColorLayer; 6],
    pub git_conflict: ColorLayer,
    pub section_foreground: ColorLayer,
    pub sidebar_sticky_background: ColorLayer,
    pub graph_lane_foreground: ColorLayer,
    pub graph_references: [ColorLayer; 2],
    pub graph_reference_foreground: ColorLayer,
    pub widget_background: ColorLayer,
    pub widget_foreground: ColorLayer,
    pub widget_border: ColorLayer,
    pub hover_colors: ControlColors,
    pub suggest_colors: ControlColors,
    pub suggest_selection: ControlColors,
    pub merge_headers: [ColorLayer; 3],
    pub merge_contents: [ColorLayer; 3],
    pub merge_border: ColorLayer,
    pub sidebar_border: ColorLayer,
    pub activity_border: ColorLayer,
    pub sash_hover_border: ColorLayer,
    pub panel_border: ColorLayer,
    pub editor_split_borders: [ColorLayer; 2],
    pub hover_highlight: ColorLayer,
    pub line_number_foreground: ColorLayer,
    pub error_foreground: ColorLayer,
    pub warning_foreground: ColorLayer,
    pub disabled_foreground: ColorLayer,
    pub primary_button: ControlColors,
    pub secondary_button: ControlColors,
    pub primary_button_hover: ColorLayer,
    pub secondary_button_hover: ColorLayer,
    pub keybinding_label_background: ColorLayer,
    pub keybinding_label_foreground: ColorLayer,
    pub keybinding_label_border: ColorLayer,
    pub keybinding_table_header: ColorLayer,
    pub keybinding_table_row: ColorLayer,
    pub notification_icons: [ColorLayer; 3],
    pub output_timestamp: ColorLayer,
    pub badge_background: ColorLayer,
    pub badge_foreground: ColorLayer,
    pub icon_foreground: ColorLayer,
    pub folder_icon_foreground: ColorLayer,
    pub list_active_icon: ColorLayer,
    pub list_inactive_icon: ColorLayer,
    pub list_match_foreground: ColorLayer,
    pub list_focus_match_foreground: ColorLayer,
    pub search_match_background: ColorLayer,
    pub search_match_border: ColorLayer,
    pub symbol_icons: [ColorLayer; SYMBOL_COLOR_NAMES.len()],
    pub scrollbar_background: ColorLayer,
    pub scrollbar_slider: ColorLayer,
    pub scrollbar_slider_hover: ColorLayer,
    pub scrollbar_slider_active: ColorLayer,
    pub suggest_highlight: ColorLayer,
    pub suggest_focus_highlight: ColorLayer,
    pub suggest_selected_icon: ColorLayer,
    pub input_colors: ControlColors,
    pub dropdown_colors: ControlColors,
    pub menu_colors: ControlColors,
    pub menu_selection_colors: ControlColors,
    pub menu_separator: ColorLayer,
    pub input_placeholder: ColorLayer,
    pub input_selection_background: ColorLayer,
    pub settings_controls: [ControlColors; 4],
    pub settings_row_border: ColorLayer,
    pub settings_header_foreground: ColorLayer,
    pub settings_dropdown_list: ControlColors,
    pub list_active_background: ColorLayer,
    pub list_active_foreground: ColorLayer,
    pub list_inactive_background: ColorLayer,
    pub list_inactive_foreground: ColorLayer,
    pub list_hover_background: ColorLayer,
    pub list_hover_foreground: ColorLayer,
    pub list_drop_background: ColorLayer,
    pub list_focus_background: ColorLayer,
    pub list_focus_foreground: ColorLayer,
    pub list_focused_outline: ColorLayer,
    pub list_error_foreground: ColorLayer,
    pub list_focus_outline: ColorLayer,
    pub list_inactive_focus_outline: ColorLayer,
    pub action_list_background: ColorLayer,
    pub action_list_foreground: ColorLayer,
    pub action_list_focus_background: ColorLayer,
    pub action_list_focus_foreground: ColorLayer,
    pub quick_foreground: ColorLayer,
    pub quick_background: ColorLayer,
    pub quick_focus_background: ColorLayer,
    pub quick_focus_foreground: ColorLayer,
    pub quick_focus_highlight: ColorLayer,
    pub quick_focus_icon: ColorLayer,
    pub quick_title_background: ColorLayer,
    pub peek_border: ColorLayer,
    pub peek_title_background: ColorLayer,
    pub peek_title_foreground: ColorLayer,
    pub peek_description_foreground: ColorLayer,
    pub peek_editor_background: ColorLayer,
    pub peek_gutter_background: ColorLayer,
    pub command_center_background: ColorLayer,
    pub command_center_active_background: ColorLayer,
    pub command_center_foreground: ColorLayer,
    pub command_center_active_foreground: ColorLayer,
    pub command_center_inactive_foreground: ColorLayer,
    pub command_center_border: ColorLayer,
    pub command_center_active_border: ColorLayer,
    pub command_center_inactive_border: ColorLayer,
    pub menubar_background: ColorLayer,
    pub menubar_foreground: ColorLayer,
    pub menubar_border: ColorLayer,
    pub md_inline_background: ColorLayer,
    pub md_inline_foreground: ColorLayer,
    pub md_code_background: ColorLayer,
    pub md_quote_background: ColorLayer,
    pub md_quote_border: ColorLayer,
    pub chat_avatar: ControlColors,
    pub chat_request_hover_background: ColorLayer,
    pub focus_border: ColorLayer,
    pub contrast_border: ColorLayer,
    pub active_contrast_border: ColorLayer,
    pub toolbar_hover: ColorLayer,
    pub toolbar_active: ColorLayer,
    pub toolbar_outline: ColorLayer,
    pub input_option_background: ColorLayer,
    pub input_option_foreground: ColorLayer,
    pub input_option_border: ColorLayer,
    pub input_option_hover: ColorLayer,
    pub bg_unfocused_tab_inactive: Color,
    pub bg_unfocused_tab_hover: Color,
    pub fg_unfocused_tab_hover: Color,
    pub border_tab: Option<Color>,
    pub border_tab_active: Option<Color>,
    pub border_tab_unfocused: Option<Color>,
    pub border_tab_hover: Option<Color>,
    pub bg_activity_active: ColorLayer,
    pub border_activity_active: ColorLayer,
    pub border_panel_active: Color,
    pub border_terminal_tab: Color,
    pub border_sidebar: Option<Color>,
    pub border_section: Option<Color>,
    pub border_notification: Option<Color>,
    pub border_diff: Option<Color>,
    pub border_diff_added: Option<Color>,
    pub border_diff_removed: Option<Color>,
    pub fg_diff_diagonal: Color,
    pub bg_md_inline: Color,
    pub fg_md_inline: Color,
    pub fg_folding: Color,
    pub fg_group_border: Color,
    pub bg_editor_drop: Color,
    pub bg_settings_hover: Color,
    pub bg_settings_focus: Color,
    pub fg_overview_error: Color,
    pub fg_overview_warning: Color,
    pub fg_overview_info: Color,
    pub fg_overview_find: Color,
    pub fg_overview_added: Color,
    pub fg_overview_modified: Color,
    pub fg_overview_deleted: Color,
    pub bg_overview: Color,
    pub fg_minimap_error: Color,
    pub fg_minimap_warning: Color,
    pub fg_minimap_info: Color,
    pub fg_minimap_find: Color,
    pub bg_minimap_selection: Color,
    pub fg_minimap_added: Color,
    pub fg_minimap_modified: Color,
    pub fg_minimap_deleted: Color,
    pub bg_minimap_slider: Color,
    pub bg_minimap_slider_hover: Color,
    pub bg_minimap_slider_active: Color,
    pub bg_unfocused_tab: Color,
    pub fg_unfocused_tab: Color,
    pub fg_unfocused_tab_inactive: Color,
    pub indent_guides: [Color; 6],
    pub indent_guides_active: [Color; 6],
    indent_guide_counts: [u8; 2],
    pub bracket_guides: [Color; 6],
    pub bracket_guides_active: [Color; 6],

    pub fg_sidebar: Color,
    pub fg_sidebar_title: Color,
    pub fg_section: Color,
    pub bg_section: Color,
    pub fg_tab_active: Color,
    pub fg_tab_inactive: Color,
    pub fg_tab_hover: Color,
    pub bg_tab_hover: Color,
    pub fg_activity: Color,
    pub fg_activity_inactive: Color,
    pub bg_activity_badge: Color,
    pub fg_activity_badge: Color,
    pub bg_panel: Color,
    pub fg_panel_title: Color,
    pub fg_panel_inactive: Color,
    pub fg_statusbar_hover: Option<Color>,
    pub fg_editor_selection: Option<Color>,
    pub fg_line_number: Color,
    pub fg_line_number_active: Color,
    pub fg_whitespace: Color,
    pub bg_find_current: Color,
    pub fg_find: Option<Color>,
    pub fg_find_other: Option<Color>,
    pub border_find: Option<Color>,
    pub border_find_other: Option<Color>,
    pub bg_widget: Color,
    pub fg_widget: Color,
    pub fg_diagnostic_error: Color,
    pub fg_diagnostic_warning: Color,
    pub fg_diagnostic_info: Color,
    pub fg_diagnostic_hint: Color,
    pub fg_gutter_added: Color,
    pub fg_gutter_modified: Color,
    pub fg_gutter_deleted: Color,
    pub fg_list_match: Color,
    pub fg_menu_selected: Color,
    pub fg_menu_disabled: Color,
    pub fg_menu_separator: Color,
    pub border_menu: Color,
    pub menu_selection_border: ColorLayer,
    pub bg_quick_input: Color,
    pub fg_quick_input: Color,
    pub bg_quick_selected: Color,
    pub fg_quick_selected: Color,
    pub fg_quick_match: Color,
    pub bg_chat_request: Color,
    pub fg_chat_edited_file: Color,
    pub fg_chat_added: Color,
    pub fg_chat_removed: Color,
    pub bg_breadcrumb: Color,
    pub fg_breadcrumb: Color,
    pub fg_breadcrumb_focus: Color,
    pub bg_title: Color,
    pub fg_title: Color,
    pub bg_title_inactive: Color,
    pub fg_title_inactive: Color,
    pub border_bracket: Option<Color>,
    pub bg_selection_highlight: Color,
    pub border_selection_highlight: Option<Color>,
    pub border_word_highlight: Option<Color>,
    pub fg_overview_border: Color,
    pub fg_scrollbar: Color,
    pub bg_minimap: Color,
    pub bg_terminal: Color,
    pub fg_terminal: Color,
    pub bg_terminal_selection: Color,
    pub fg_terminal_selection: Option<Color>,
    pub bg_terminal_cursor: Color,
    pub fg_terminal_cursor: Color,
    pub bg_notification: Color,
    pub fg_notification: Color,
    pub brackets: [Option<Color>; 6],
    pub ansi: [Color; 16],
}
/// A theme commonly defines only color 1; the registry's transparent defaults
/// for colors 2–6 are unused palette slots, not invisible indentation levels.
fn indent_guide_palette(colors: &Map<String, Value>, prefix: &str, background: Color, fallback: Color) -> ([Color; 6], u8) {
    let mut palette = [fallback; 6];
    let mut count = 0;
    for level in 1..=6 {
        if let Some(color) = ColorLayer::resolve(colors, &format!("{prefix}{level}"), ColorLayer::default()).border(background)
            && color != background
        {
            palette[count] = color;
            count += 1;
        }
    }
    let count = count.max(1);
    for index in count..6 { palette[index] = palette[index % count]; }
    (palette, count as u8)
}

impl WorkbenchColors {
    pub(crate) fn resolve(colors: &Map<String, Value>, base: &ThemeColors, light: bool, high_contrast: bool) -> Self {
        let get = |id: &str, fallback: Color, background: Color| {
            let bg = match background {
                Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
                _ => "#000000".into(),
            };
            crate::vscode_theme::resolved_color(colors, id, &bg)
                .map(|c| crate::app_theme_loader::hex_to_color(&c))
                .unwrap_or(fallback)
        };
        let surface = |id: &str, fallback: Color| get(id, fallback, fallback);
        let (indent_guides, indent_count) = indent_guide_palette(colors, "editorIndentGuide.background", base.bg_editor, base.fg_dim);
        let (indent_guides_active, active_indent_count) = indent_guide_palette(colors, "editorIndentGuide.activeBackground", base.bg_editor, base.fg_accent);
        let panel = surface("panel.background", base.bg_editor);
        let terminal = surface("terminal.background", panel);
        let widget_default = base.bg_sidebar;
        let widget = get("editorWidget.background", widget_default, base.bg_editor);
        let widget_fg = get("editorWidget.foreground", base.fg_workbench, widget);
        let widget_border = get("editorWidget.border",
            crate::app_theme::lerp_color(widget, widget_fg, 0.2), widget);
        let quick = get("quickInput.background", widget, base.bg_editor);
        let sidebar_fg = get("sideBar.foreground", base.fg_workbench, base.bg_sidebar);
        let chat_request = get("chat.requestBubbleBackground",
            get("chat.requestBackground", base.bg_sidebar, base.bg_sidebar), base.bg_sidebar);
        let disabled = |background| {
            let fallback = crate::app_theme::lerp_color(background, base.fg_workbench, 0.5);
            get("disabledForeground", fallback, background)
        };
        let optional = |id: &str, background: Color| {
            // A transparent border must not turn into an editor-colored underline.
            colors.get(id).and_then(Value::as_str)
                .and_then(crate::vscode_theme::parse_syntax_color)
                .filter(|color| color.a != 0)
                .map(|_| get(id, background, background))
        };
        let find_border_default = high_contrast.then(|| get("contrastActiveBorder", base.fg_accent, base.bg_editor));
        let notification = get("notifications.background", base.bg_menu, base.bg_editor);
        let minimap = surface("minimap.background", base.bg_editor);
        let gutter_added = get(
            "editorGutter.addedBackground",
            base.fg_git_added,
            base.bg_dark,
        );
        let gutter_modified = get(
            "editorGutter.modifiedBackground",
            base.fg_git_modified,
            base.bg_dark,
        );
        let gutter_deleted = get(
            "editorGutter.deletedBackground",
            base.fg_git_deleted,
            base.bg_dark,
        );
        let section = surface("sideBarSectionHeader.background", base.bg_sidebar);
        let overview = get("editorOverviewRuler.background", base.bg_editor, base.bg_editor);
        let unfocused_tab = get("tab.unfocusedActiveBackground", base.bg_tab_active, base.bg_tabbar);
        let unfocused_inactive = get("tab.unfocusedInactiveBackground", base.bg_tab_inactive, base.bg_tabbar);
        let tab_hover = get("tab.hoverBackground", base.bg_tab_inactive, base.bg_tab_inactive);
        let unfocused_hover = get("tab.unfocusedHoverBackground", tab_hover, unfocused_inactive);
        let badge = get("activityBarBadge.background", base.fg_accent, base.bg_activitybar);
        let inlay = ControlColors {
            background: ColorLayer::resolve(colors, "editorInlayHint.background",
                if high_contrast && !light { ColorLayer::rgba(255, 255, 255, 26) }
                else { ColorLayer::resolve(colors, "badge.background", ColorLayer::opaque(base.bg_input)).with_opacity(0.1) }),
            foreground: ColorLayer::resolve(colors, "editorInlayHint.foreground", if high_contrast {
                if light { ColorLayer::rgba(0, 0, 0, 255) } else { ColorLayer::rgba(255, 255, 255, 255) }
            } else { ColorLayer::rgba(150, 150, 150, 255) }),
            border: ColorLayer::default(),
        };
        let find = get("editor.findMatchBackground", if high_contrast { base.bg_editor }
            else if light { Color::Rgb(168, 172, 148) } else { Color::Rgb(81, 92, 106) }, base.bg_editor);
        let quick_selected = get("quickInputList.focusBackground",
            get("quickInput.list.focusBackground",
                get("list.activeSelectionBackground", base.bg_selected, quick), quick), quick);
        let breadcrumb = surface("breadcrumb.background", base.bg_editor);
        let title = get("titleBar.activeBackground", base.bg_tabbar, base.bg_editor);
        let title_inactive = get("titleBar.inactiveBackground", title, base.bg_editor);
        let terminal_selection = get("terminal.selectionBackground", base.bg_selection, terminal);
        let selection_highlight = get("editor.selectionHighlightBackground", if high_contrast { base.bg_editor }
            else { crate::app_theme::lerp_color(base.bg_editor, base.bg_selection, 0.3) }, base.bg_editor);
        let contrast = ColorLayer::resolve(colors, "contrastBorder", if high_contrast {
            ColorLayer::opaque(base.fg_accent)
        } else { ColorLayer::default() });
        let focus = ColorLayer::resolve(colors, "focusBorder", ColorLayer::opaque(base.fg_accent));
        let border = |id: &str, background: Color| {
            if colors.contains_key(id) { optional(id, background) } else { contrast.border(background) }
        };
        let panel_border = get("panel.border", contrast.border(panel).unwrap_or(base.border_color), panel);
        let group_border = ColorLayer::resolve(colors, "editorGroup.border", ColorLayer::opaque(base.border_color));
        let panel_active_border = get("panelTitle.activeBorder", base.fg_workbench, panel);
        let inline_code = get("textPreformat.background",
            if colors.get("textPreformat.background").is_some_and(Value::is_null) { base.bg_editor } else { base.bg_md_code }, base.bg_editor);
        let ansi_defaults = terminal_ansi_defaults(light, high_contrast);
        let title_foreground = ColorLayer::resolve(colors, "titleBar.activeForeground", ColorLayer::opaque(base.fg_workbench));
        let title_inactive_foreground = ColorLayer::resolve(colors, "titleBar.inactiveForeground",
            ColorLayer::opaque(get("titleBar.inactiveForeground", base.fg_dim, title_inactive)));
        let command_center_fill = title_foreground.with_alpha(13);
        let command_center_active_fill = title_foreground.with_alpha(20);
        let icon_foreground = ColorLayer::resolve(colors, "icon.foreground", ColorLayer::opaque(base.fg_workbench));
        let list_match_foreground = ColorLayer::resolve(colors, "list.highlightForeground", ColorLayer::opaque(base.fg_accent));
        // Registered VS Code defaults (colors/miscColors.ts). Each state has
        // its own default; an omitted hover role does not inherit the idle role.
        let contrast_border = ColorLayer::resolve(colors, "contrastBorder", if light {
            ColorLayer::rgba(15, 74, 133, 255)
        } else {
            ColorLayer::rgba(111, 195, 223, 255)
        });
        let scrollbar_slider = ColorLayer::resolve(colors, "scrollbarSlider.background", if high_contrast {
            contrast_border.with_opacity(if light { 0.4 } else { 0.6 })
        } else if light {
            ColorLayer::rgba(100, 100, 100, 102)
        } else {
            ColorLayer::rgba(121, 121, 121, 102)
        });
        let scrollbar_slider_hover = ColorLayer::resolve(colors, "scrollbarSlider.hoverBackground", if high_contrast {
            contrast_border.with_opacity(0.8)
        } else {
            ColorLayer::rgba(100, 100, 100, 179)
        });
        let scrollbar_slider_active = ColorLayer::resolve(colors, "scrollbarSlider.activeBackground", if high_contrast {
            contrast_border
        } else if light {
            ColorLayer::rgba(0, 0, 0, 153)
        } else {
            ColorLayer::rgba(191, 191, 191, 102)
        });
        // VS Code derives missing minimap sliders from the matching scrollbar
        // state at half opacity (colors/minimapColors.ts). Retain alpha until
        // compositing on the minimap, which may have a different background.
        let minimap_slider = |id: &str, fallback: ColorLayer| {
            ColorLayer::resolve(colors, id, fallback.with_opacity(0.5))
                .over(minimap).unwrap_or(minimap)
        };
        let suggest_highlight = ColorLayer::resolve(colors, "editorSuggestWidget.highlightForeground",
            ColorLayer::resolve(colors, "list.highlightForeground", ColorLayer::opaque(base.fg_accent)));
        let input_colors = ControlColors {
            background: ColorLayer::resolve(colors, "input.background", ColorLayer::opaque(base.bg_input)),
            foreground: ColorLayer::resolve(colors, "input.foreground", ColorLayer::opaque(base.fg_input)),
            border: ColorLayer::resolve(colors, "input.border", contrast),
        };
        let dropdown_colors = ControlColors {
            background: ColorLayer::resolve(colors, "dropdown.background", input_colors.background),
            foreground: ColorLayer::resolve(colors, "dropdown.foreground", input_colors.foreground),
            border: ColorLayer::resolve(colors, "dropdown.border", if high_contrast { contrast }
                else { input_colors.foreground.with_alpha(51) }),
        };
        let checkbox_colors = ControlColors {
            background: ColorLayer::resolve(colors, "checkbox.background", dropdown_colors.background),
            foreground: ColorLayer::resolve(colors, "checkbox.foreground", dropdown_colors.foreground),
            border: ColorLayer::resolve(colors, "checkbox.border", dropdown_colors.border),
        };
        let settings_controls = [
            ("checkbox", checkbox_colors), ("dropdown", dropdown_colors),
            ("numberInput", input_colors), ("textInput", input_colors),
        ].map(|(name, fallback)| ControlColors {
            background: ColorLayer::resolve(colors, &format!("settings.{name}Background"), fallback.background),
            foreground: ColorLayer::resolve(colors, &format!("settings.{name}Foreground"), fallback.foreground),
            border: ColorLayer::resolve(colors, &format!("settings.{name}Border"), fallback.border),
        });
        let workbench_foreground = ColorLayer::resolve(colors, "foreground", ColorLayer::opaque(base.fg_workbench));
        let sidebar_foreground = ColorLayer::resolve(colors, "sideBar.foreground", workbench_foreground);
        let widget_foreground = ColorLayer::resolve(colors, "editorWidget.foreground", workbench_foreground);
        let widget_background = ColorLayer::resolve(colors, "editorWidget.background", ColorLayer::opaque(widget_default));
        let widget_border_layer = ColorLayer::resolve(colors, "editorWidget.border", ColorLayer::opaque(widget_border));
        let quick_foreground = ColorLayer::resolve(colors, "quickInput.foreground", widget_foreground);
        let list_active_background = ColorLayer::resolve(colors, "list.activeSelectionBackground", ColorLayer::opaque(base.bg_selected));
        let list_active_foreground = ColorLayer::resolve(colors, "list.activeSelectionForeground", ColorLayer::opaque(base.fg_selected));
        let quick_focus_background = ColorLayer::resolve(colors, "quickInputList.focusBackground",
            if high_contrast { ColorLayer::default() }
            else { ColorLayer::resolve(colors, "quickInput.list.focusBackground", list_active_background) });
        let quick_focus_foreground = ColorLayer::resolve(colors, "quickInputList.focusForeground",
            ColorLayer::resolve(colors, "list.activeSelectionForeground",
                if high_contrast { ColorLayer::default() } else { ColorLayer::opaque(base.fg_selected) }));
        let quick_focus_icon = ColorLayer::resolve(colors, "quickInputList.focusIconForeground",
            ColorLayer::resolve(colors, "list.activeSelectionIconForeground", ColorLayer::default()));
        let suggest_colors = ControlColors {
            background: ColorLayer::resolve(colors, "editorSuggestWidget.background", widget_background),
            foreground: ColorLayer::resolve(colors, "editorSuggestWidget.foreground",
                ColorLayer::resolve(colors, "editor.foreground", ColorLayer::opaque(base.fg_text))),
            border: ColorLayer::resolve(colors, "editorSuggestWidget.border", widget_border_layer),
        };
        let suggest_selection = ControlColors {
            background: ColorLayer::resolve(colors, "editorSuggestWidget.selectedBackground",
                if high_contrast { suggest_colors.foreground } else { quick_focus_background }),
            foreground: ColorLayer::resolve(colors, "editorSuggestWidget.selectedForeground",
                if high_contrast { suggest_colors.background } else { quick_focus_foreground }),
            border: ColorLayer::resolve(colors, "editorSuggestWidget.focusOutline",
                ColorLayer::resolve(colors, "contrastActiveBorder",
                    if high_contrast { focus } else { ColorLayer::default() })),
        };
        let primary_button = ControlColors {
            background: ColorLayer::resolve(colors, "button.background", ColorLayer::opaque(base.bg_sc_button)),
            foreground: ColorLayer::resolve(colors, "button.foreground", ColorLayer::opaque(base.fg_text)),
            border: ColorLayer::resolve(colors, "button.border", contrast),
        };
        let secondary_button = ControlColors {
            background: ColorLayer::resolve(colors, "button.secondaryBackground", ColorLayer::opaque(base.bg_sc_button_secondary)),
            foreground: ColorLayer::resolve(colors, "button.secondaryForeground", workbench_foreground),
            border: ColorLayer::resolve(colors, "button.secondaryBorder", if high_contrast { contrast }
                else { workbench_foreground.with_alpha(38) }),
        };
        Self {
            preserve_theme_colors: false,
            contrast_palette: [
                get("foreground", base.fg_workbench, base.bg_editor),
                sidebar_fg,
                base.fg_text,
                get("input.foreground", base.fg_input, base.bg_input),
                title_foreground.over(title).unwrap_or(base.fg_workbench),
                get("button.foreground", base.fg_text, base.bg_sc_button),
                get("button.secondaryForeground", base.fg_workbench, base.bg_sc_button_secondary),
                get("terminal.foreground", base.fg_text, terminal),
                get("list.activeSelectionForeground", base.fg_selected, base.bg_selected),
                base.bg_editor,
                base.bg_sidebar,
                base.bg_input,
                title,
                base.bg_selected,
            ],
            workbench_foreground,
            description_foreground: ColorLayer::resolve(colors, "descriptionForeground", ColorLayer::opaque(base.fg_dim)),
            editor_foreground: ColorLayer::resolve(colors, "editor.foreground", ColorLayer::opaque(base.fg_text)),
            ruler_foreground: ColorLayer::resolve(colors, "editorRuler.foreground", ColorLayer::opaque(match (light, high_contrast) {
                (false, false) => Color::Rgb(90, 90, 90), (true, false) => Color::Rgb(211, 211, 211),
                (false, true) => Color::Rgb(255, 255, 255), (true, true) => Color::Rgb(41, 41, 41),
            })),
            codelens_foreground: ColorLayer::resolve(colors, "editorCodeLens.foreground", ColorLayer::opaque(
                if light { if high_contrast { Color::Rgb(41, 41, 41) } else { Color::Rgb(145, 145, 145) } }
                else { Color::Rgb(153, 153, 153) })),
            inlay_colors: [inlay,
                ControlColors { foreground: ColorLayer::resolve(colors, "editorInlayHint.typeForeground", inlay.foreground),
                    background: ColorLayer::resolve(colors, "editorInlayHint.typeBackground", inlay.background), ..inlay },
                ControlColors { foreground: ColorLayer::resolve(colors, "editorInlayHint.parameterForeground", inlay.foreground),
                    background: ColorLayer::resolve(colors, "editorInlayHint.parameterBackground", inlay.background), ..inlay },
            ],
            sticky_background: ColorLayer::resolve(colors, "editorStickyScroll.background", ColorLayer::opaque(base.bg_sticky_header)),
            sticky_hover_background: ColorLayer::resolve(colors, "editorStickyScrollHover.background", ColorLayer::default()),
            sticky_border: ColorLayer::resolve(colors, "editorStickyScroll.border", ColorLayer::resolve(colors, "contrastBorder",
                if high_contrast { ColorLayer::opaque(base.border_color) } else { ColorLayer::default() })),
            sidebar_foreground,
            sidebar_title_foreground: ColorLayer::resolve(colors, "sideBarTitle.foreground", sidebar_foreground),
            git_decorations: [
                ("added", base.fg_git_added), ("deleted", base.fg_git_deleted),
                ("modified", base.fg_git_modified), ("untracked", base.fg_git_added),
                ("renamed", base.fg_git_modified), ("ignored", base.fg_dim),
            ].map(|(name, fallback)| ColorLayer::resolve(colors, &format!("gitDecoration.{name}ResourceForeground"), ColorLayer::opaque(fallback))),
            git_conflict: ColorLayer::resolve(colors, "gitDecoration.conflictingResourceForeground", ColorLayer::opaque(Color::Rgb(231, 122, 0))),
            section_foreground: ColorLayer::resolve(colors, "sideBarSectionHeader.foreground", sidebar_foreground),
            sidebar_sticky_background: ColorLayer::resolve(colors, "sideBarStickyScroll.background", ColorLayer::default()),
            graph_lane_foreground: ColorLayer::resolve(colors, "scmGraph.foreground1", ColorLayer::opaque(base.fg_accent)),
            graph_references: [
                ColorLayer::resolve(colors, "scmGraph.historyItemRefColor",
                    ColorLayer::resolve(colors, "charts.blue", ColorLayer::opaque(base.fg_accent_blue))),
                ColorLayer::resolve(colors, "scmGraph.historyItemRemoteRefColor",
                    ColorLayer::resolve(colors, "charts.purple", ColorLayer::opaque(base.fg_accent))),
            ],
            graph_reference_foreground: ColorLayer::resolve(colors, "scmGraph.historyItemHoverLabelForeground", ColorLayer::opaque(panel)),
            widget_background,
            widget_foreground,
            widget_border: widget_border_layer,
            hover_colors: ControlColors {
                background: ColorLayer::resolve(colors, "editorHoverWidget.background", widget_background),
                foreground: ColorLayer::resolve(colors, "editorHoverWidget.foreground", widget_foreground),
                border: ColorLayer::resolve(colors, "editorHoverWidget.border", widget_border_layer),
            },
            suggest_colors,
            suggest_selection,
            // VS Code's registered merge defaults, independent of unrelated
            // list colors and of the weaker merge content tints.
            merge_headers: std::array::from_fn(|index| ColorLayer::resolve(colors,
                ["merge.currentHeaderBackground", "merge.incomingHeaderBackground", "merge.commonHeaderBackground"][index],
                if high_contrast { ColorLayer::default() } else {
                    [ColorLayer::rgba(64, 200, 174, 128), ColorLayer::rgba(64, 166, 255, 128), ColorLayer::rgba(96, 96, 96, 102)][index]
                })),
            merge_contents: std::array::from_fn(|index| ColorLayer::resolve(colors,
                ["merge.currentContentBackground", "merge.incomingContentBackground", "merge.commonContentBackground"][index],
                if high_contrast { ColorLayer::default() } else {
                    [ColorLayer::rgba(64, 200, 174, 51), ColorLayer::rgba(64, 166, 255, 51), ColorLayer::rgba(96, 96, 96, 26)][index]
                })),
            merge_border: ColorLayer::resolve(colors, "merge.border", if high_contrast {
                ColorLayer::opaque(if light { Color::Rgb(0, 122, 204) } else { Color::Rgb(195, 223, 111) })
            } else { ColorLayer::default() }),
            sidebar_border: ColorLayer::resolve(colors, "sideBar.border", ColorLayer::default()),
            activity_border: ColorLayer::resolve(colors, "activityBar.border", contrast),
            sash_hover_border: ColorLayer::resolve(colors, "sash.hoverBorder", focus),
            panel_border: ColorLayer::resolve(colors, "panel.border", ColorLayer::opaque(panel_border)),
            editor_split_borders: [
                ColorLayer::resolve(colors, "sideBySideEditor.verticalBorder", group_border),
                ColorLayer::resolve(colors, "sideBySideEditor.horizontalBorder", group_border),
            ],
            hover_highlight: ColorLayer::resolve(colors, "editorHoverWidget.highlightForeground",
                ColorLayer::resolve(colors, "list.highlightForeground", ColorLayer::opaque(base.fg_accent_blue))),
            line_number_foreground: ColorLayer::resolve(colors, "editorLineNumber.foreground", ColorLayer::opaque(base.fg_dim)),
            error_foreground: ColorLayer::resolve(colors, "errorForeground", ColorLayer::opaque(base.fg_error)),
            warning_foreground: ColorLayer::resolve(colors, "editorWarning.foreground", ColorLayer::opaque(base.fg_warning)),
            disabled_foreground: ColorLayer::resolve(colors, "disabledForeground", workbench_foreground.with_alpha(128)),
            primary_button,
            secondary_button,
            primary_button_hover: ColorLayer::resolve(colors, "button.hoverBackground", if high_contrast { primary_button.background }
                else { primary_button.background.with_lightness(if light { 0.8 } else { 1.2 }) }),
            secondary_button_hover: ColorLayer::resolve(colors, "button.secondaryHoverBackground", if high_contrast { secondary_button.background }
                else { secondary_button.background.with_lightness(1.2) }),
            keybinding_label_background: ColorLayer::resolve(colors, "keybindingLabel.background", ColorLayer::default()),
            keybinding_label_foreground: ColorLayer::resolve(colors, "keybindingLabel.foreground", ColorLayer::opaque(base.fg_workbench)),
            keybinding_label_border: ColorLayer::resolve(colors, "keybindingLabel.bottomBorder",
                ColorLayer::resolve(colors, "keybindingLabel.border", ColorLayer::default())),
            keybinding_table_header: ColorLayer::resolve(colors, "keybindingTable.headerBackground", ColorLayer::default()),
            keybinding_table_row: ColorLayer::resolve(colors, "keybindingTable.rowsBackground", ColorLayer::default()),
            notification_icons: std::array::from_fn(|i| ColorLayer::resolve(colors,
                ["notificationsInfoIcon.foreground", "notificationsWarningIcon.foreground", "notificationsErrorIcon.foreground"][i],
                ColorLayer::opaque([base.fg_accent_blue, base.fg_warning, base.fg_error][i]))),
            output_timestamp: ColorLayer::opaque(base.fg_dim),
            badge_background: ColorLayer::resolve(colors, "badge.background", ColorLayer::opaque(base.bg_input)),
            badge_foreground: ColorLayer::resolve(colors, "badge.foreground", ColorLayer::opaque(base.fg_workbench)),
            icon_foreground,
            folder_icon_foreground: ColorLayer::resolve(colors, "icon.foreground", ColorLayer::opaque(base.fg_folder_icon)),
            list_active_icon: ColorLayer::resolve(colors, "list.activeSelectionIconForeground", ColorLayer::default()),
            list_inactive_icon: ColorLayer::resolve(colors, "list.inactiveSelectionIconForeground", ColorLayer::default()),
            list_match_foreground,
            list_focus_match_foreground: ColorLayer::resolve(colors, "list.focusHighlightForeground", list_match_foreground),
            // Search snippets use the editor's find decoration, not the
            // foreground used to filter filenames in lists and Quick Open.
            // Keep alpha until the selected/hovered result row is known.
            search_match_background: if high_contrast { ColorLayer::default() } else {
                ColorLayer::resolve(colors, "editor.findMatchHighlightBackground", ColorLayer::opaque(base.bg_search_match))
            },
            search_match_border: ColorLayer::resolve(colors, "editor.findMatchHighlightBorder",
                if high_contrast { ColorLayer::resolve(colors, "contrastActiveBorder", focus) }
                else { ColorLayer::default() }),
            symbol_icons: std::array::from_fn(|i| ColorLayer::resolve(colors,
                &format!("symbolIcon.{}Foreground", SYMBOL_COLOR_NAMES[i]), icon_foreground)),
            scrollbar_background: ColorLayer::resolve(colors, "scrollbar.background", ColorLayer::default()),
            scrollbar_slider,
            scrollbar_slider_hover,
            scrollbar_slider_active,
            suggest_highlight,
            suggest_focus_highlight: ColorLayer::resolve(colors, "editorSuggestWidget.focusHighlightForeground",
                if high_contrast { suggest_selection.foreground }
                else { ColorLayer::resolve(colors, "list.focusHighlightForeground", list_match_foreground) }),
            suggest_selected_icon: ColorLayer::resolve(colors, "editorSuggestWidget.selectedIconForeground",
                if high_contrast { suggest_colors.background } else { quick_focus_icon }),
            input_colors,
            dropdown_colors,
            menu_colors: ControlColors {
                background: ColorLayer::resolve(colors, "menu.background",
                    ColorLayer::resolve(colors, "dropdown.background", ColorLayer::opaque(base.bg_menu))),
                foreground: ColorLayer::resolve(colors, "menu.foreground",
                    ColorLayer::resolve(colors, "dropdown.foreground", ColorLayer::opaque(base.fg_menu))),
                border: ColorLayer::resolve(colors, "menu.border", contrast),
            },
            menu_selection_colors: ControlColors {
                background: ColorLayer::resolve(colors, "menu.selectionBackground", list_active_background),
                foreground: ColorLayer::resolve(colors, "menu.selectionForeground", list_active_foreground),
                border: ColorLayer::resolve(colors, "menu.selectionBorder", ColorLayer::default()),
            },
            menu_separator: ColorLayer::resolve(colors, "menu.separatorBackground", workbench_foreground.with_opacity(0.2)),
            input_placeholder: ColorLayer::resolve(colors, "input.placeholderForeground", ColorLayer::opaque(base.fg_input_placeholder)),
            input_selection_background: ColorLayer::resolve(colors, "selection.background", ColorLayer::opaque(base.bg_selection))
                .or(ColorLayer::opaque(base.bg_selection)),
            settings_controls,
            settings_row_border: ColorLayer::resolve(colors, "settings.focusedRowBorder", focus),
            settings_header_foreground: ColorLayer::resolve(colors, "settings.headerForeground", ColorLayer::opaque(base.fg_text)),
            settings_dropdown_list: ControlColors {
                // SelectBoxCustom.styleList uses selectBackground when the
                // optional list background CSS variable is unset.
                background: ColorLayer::resolve(colors, "dropdown.listBackground", ColorLayer::default())
                    .or(settings_controls[1].background),
                foreground: settings_controls[1].foreground,
                border: ColorLayer::resolve(colors, "settings.dropdownListBorder",
                    ColorLayer::resolve(colors, "editorWidget.border", ColorLayer::opaque(widget_border))),
            },
            list_active_background,
            list_active_foreground,
            list_inactive_background: ColorLayer::resolve(colors, "list.inactiveSelectionBackground", ColorLayer::opaque(base.bg_explorer_selected)),
            list_inactive_foreground: ColorLayer::resolve(colors, "list.inactiveSelectionForeground", ColorLayer::default()),
            list_hover_background: ColorLayer::resolve(colors, "list.hoverBackground", ColorLayer::opaque(base.bg_hover)),
            list_hover_foreground: ColorLayer::resolve(colors, "list.hoverForeground", ColorLayer::default()),
            list_drop_background: ColorLayer::resolve(colors, "list.dropBackground", list_active_background.with_alpha(128)),
            list_focus_background: ColorLayer::resolve(colors, "list.focusBackground", list_active_background),
            list_focus_foreground: ColorLayer::resolve(colors, "list.focusForeground", ColorLayer::default()),
            list_focused_outline: ColorLayer::resolve(colors, "list.focusOutline", focus),
            list_error_foreground: ColorLayer::resolve(colors, "list.errorForeground",
                ColorLayer::resolve(colors, "errorForeground", ColorLayer::opaque(base.fg_error))),
            list_focus_outline: ColorLayer::resolve(colors, "list.focusAndSelectionOutline", ColorLayer::default())
                .or(ColorLayer::resolve(colors, "list.focusOutline", contrast)),
            list_inactive_focus_outline: ColorLayer::resolve(colors, "list.inactiveFocusOutline", contrast),
            action_list_background: ColorLayer::resolve(colors, "editorActionList.background", widget_background),
            action_list_foreground: ColorLayer::resolve(colors, "editorActionList.foreground", widget_foreground),
            action_list_focus_background: ColorLayer::resolve(colors, "editorActionList.focusBackground", list_active_background),
            action_list_focus_foreground: ColorLayer::resolve(colors, "editorActionList.focusForeground", list_active_foreground),
            quick_foreground,
            quick_background: ColorLayer::resolve(colors, "quickInput.background", widget_background),
            quick_focus_background,
            quick_focus_foreground,
            quick_focus_highlight: ColorLayer::resolve(colors, "quickInputList.focusHighlightForeground",
                ColorLayer::resolve(colors, "list.focusHighlightForeground", list_match_foreground)),
            quick_focus_icon,
            // VS Code's registered title defaults are independent of text
            // colors. Theme kind comes from metadata, never the display name.
            quick_title_background: ColorLayer::resolve(colors, "quickInputTitle.background",
                match (light, high_contrast) {
                    (false, false) => ColorLayer::rgba(255, 255, 255, 27),
                    (true, false) => ColorLayer::rgba(0, 0, 0, 15),
                    (false, true) => ColorLayer::rgba(0, 0, 0, 255),
                    (true, true) => ColorLayer::rgba(255, 255, 255, 255),
                }),
            peek_border: ColorLayer::resolve(colors, "peekView.border", focus),
            peek_title_background: ColorLayer::resolve(colors, "peekViewTitle.background", ColorLayer::opaque(base.bg_dark)),
            peek_title_foreground: ColorLayer::resolve(colors, "peekViewTitleLabel.foreground", ColorLayer::opaque(base.fg_text)),
            peek_description_foreground: ColorLayer::resolve(colors, "peekViewTitleDescription.foreground",
                ColorLayer::resolve(colors, "descriptionForeground", ColorLayer::opaque(base.fg_dim))),
            peek_editor_background: ColorLayer::resolve(colors, "peekViewEditor.background", ColorLayer::opaque(base.bg_editor)),
            peek_gutter_background: ColorLayer::resolve(colors, "peekViewEditorGutter.background", ColorLayer::opaque(base.bg_dark)),
            command_center_background: ColorLayer::resolve(colors, "commandCenter.background", if high_contrast { ColorLayer::default() } else { command_center_fill }),
            command_center_active_background: ColorLayer::resolve(colors, "commandCenter.activeBackground", if high_contrast { ColorLayer::resolve(colors, "menubar.selectionBackground", ColorLayer::default()) } else { command_center_active_fill }),
            command_center_foreground: ColorLayer::resolve(colors, "commandCenter.foreground", title_foreground),
            command_center_active_foreground: ColorLayer::resolve(colors, "commandCenter.activeForeground", ColorLayer::resolve(colors, "menubar.selectionForeground", title_foreground)),
            command_center_inactive_foreground: ColorLayer::resolve(colors, "commandCenter.inactiveForeground", title_inactive_foreground),
            command_center_border: ColorLayer::resolve(colors, "commandCenter.border", if high_contrast { contrast } else { title_foreground.with_alpha(51) }),
            command_center_active_border: ColorLayer::resolve(colors, "commandCenter.activeBorder", if high_contrast { title_foreground } else { title_foreground.with_alpha(77) }),
            command_center_inactive_border: ColorLayer::resolve(colors, "commandCenter.inactiveBorder", title_inactive_foreground.with_alpha(64)),
            menubar_background: ColorLayer::resolve(colors, "menubar.selectionBackground", title_foreground.with_alpha(32)),
            menubar_foreground: ColorLayer::resolve(colors, "menubar.selectionForeground", ColorLayer::default()),
            menubar_border: ColorLayer::resolve(colors, "menubar.selectionBorder", contrast),
            md_code_background: ColorLayer::resolve(colors, "textCodeBlock.background", ColorLayer::opaque(base.bg_md_code)),
            md_quote_background: ColorLayer::resolve(colors, "textBlockQuote.background", ColorLayer::default()),
            md_quote_border: ColorLayer::resolve(colors, "textBlockQuote.border", ColorLayer::opaque(base.fg_md_blockquote)),
            md_inline_background: ColorLayer::resolve(colors, "textPreformat.background", ColorLayer::resolve(colors, "textCodeBlock.background", ColorLayer::opaque(base.bg_md_code))),
            md_inline_foreground: ColorLayer::resolve(colors, "textPreformat.foreground", ColorLayer::opaque(base.fg_md_code)),
            chat_avatar: ControlColors {
                background: ColorLayer::resolve(colors, "chat.avatarBackground", ColorLayer::opaque(base.bg_sidebar)),
                foreground: ColorLayer::resolve(colors, "chat.avatarForeground", ColorLayer::opaque(base.fg_accent)),
                border: ColorLayer::default(),
            },
            chat_request_hover_background: ColorLayer::resolve(colors, "chat.requestBubbleHoverBackground", ColorLayer::opaque(base.bg_hover)),
            focus_border: focus,
            contrast_border: contrast,
            active_contrast_border: ColorLayer::resolve(colors, "contrastActiveBorder", if high_contrast { focus } else { ColorLayer::default() }),
            toolbar_hover: ColorLayer::resolve(colors, "toolbar.hoverBackground", ColorLayer::opaque(base.fg_workbench).with_alpha(20)),
            toolbar_active: ColorLayer::resolve(colors, "toolbar.activeBackground", ColorLayer::opaque(base.fg_workbench).with_alpha(32)),
            toolbar_outline: ColorLayer::resolve(colors, "toolbar.hoverOutline", contrast),
            input_option_background: ColorLayer::resolve(colors, "inputOption.activeBackground",
                focus.with_alpha(if high_contrast { 0 } else if light { 51 } else { 102 })),
            input_option_foreground: ColorLayer::resolve(colors, "inputOption.activeForeground",
                ColorLayer::opaque(base.fg_input)),
            input_option_border: ColorLayer::resolve(colors, "inputOption.activeBorder", if high_contrast { contrast } else { focus }),
            input_option_hover: ColorLayer::resolve(colors, "inputOption.hoverBackground", if high_contrast { ColorLayer::default() }
                else { ColorLayer::opaque(base.fg_input).with_alpha(32) }),
            bg_unfocused_tab_inactive: unfocused_inactive,
            bg_unfocused_tab_hover: unfocused_hover,
            fg_unfocused_tab_hover: get("tab.unfocusedHoverForeground", get("tab.unfocusedInactiveForeground", base.fg_dim, unfocused_hover), unfocused_hover),
            border_tab: border("tab.border", base.bg_tabbar),
            border_tab_active: if colors.get("tab.activeBorderTop").is_some_and(Value::is_string) { optional("tab.activeBorderTop", base.bg_tab_active) }
                else { border("tab.activeBorder", base.bg_tab_active) },
            border_tab_unfocused: if colors.get("tab.unfocusedActiveBorderTop").is_some_and(Value::is_string) { optional("tab.unfocusedActiveBorderTop", unfocused_tab) }
                else { border("tab.unfocusedActiveBorder", unfocused_tab) },
            border_tab_hover: border("tab.hoverBorder", tab_hover),
            bg_activity_active: ColorLayer::resolve(colors, "activityBar.activeBackground", ColorLayer::default()),
            border_activity_active: ColorLayer::resolve(colors, "activityBar.activeBorder",
                ColorLayer::resolve(colors, "activityBar.foreground", ColorLayer::opaque(base.fg_workbench))),
            border_panel_active: panel_active_border,
            border_terminal_tab: get("terminal.tab.activeBorder", optional("tab.activeBorder", panel).unwrap_or(panel_active_border), panel),
            border_sidebar: border("sideBar.border", base.bg_sidebar),
            border_section: border("sideBarSectionHeader.border", section),
            border_notification: border("notifications.border", notification),
            border_diff: if colors.contains_key("diffEditor.border") { optional("diffEditor.border", base.bg_editor) }
                else { Some(contrast.border(base.bg_editor).unwrap_or(base.border_color)) },
            border_diff_added: if colors.contains_key("diffEditor.insertedTextBorder") { optional("diffEditor.insertedTextBorder", base.bg_diff_added_highlight) } else { high_contrast.then_some(if light { Color::Rgb(55, 78, 6) } else { Color::Rgb(51, 255, 46) }) },
            border_diff_removed: if colors.contains_key("diffEditor.removedTextBorder") { optional("diffEditor.removedTextBorder", base.bg_diff_removed_highlight) } else { high_contrast.then_some(if light { Color::Rgb(173, 7, 7) } else { Color::Rgb(255, 0, 143) }) },
            fg_diff_diagonal: get("diffEditor.diagonalFill", base.fg_dim, base.bg_editor),
            bg_md_inline: inline_code,
            fg_md_inline: get("textPreformat.foreground", base.fg_md_code, inline_code),
            fg_folding: get(
                "editorGutter.foldingControlForeground",
                base.fg_dim,
                base.bg_dark,
            ),
            fg_group_border: get("editorGroup.border", base.border_color, base.bg_editor),
            bg_editor_drop: get(
                "editorGroup.dropBackground",
                base.bg_selection,
                base.bg_editor,
            ),
            bg_settings_hover: get("settings.rowHoverBackground", base.bg_editor, base.bg_editor),
            bg_settings_focus: get(
                "settings.focusedRowBackground",
                base.bg_editor,
                base.bg_editor,
            ),
            fg_overview_error: get(
                "editorOverviewRuler.errorForeground",
                base.fg_error,
                overview,
            ),
            fg_overview_warning: get(
                "editorOverviewRuler.warningForeground",
                base.fg_warning,
                overview,
            ),
            fg_overview_info: get(
                "editorOverviewRuler.infoForeground",
                base.fg_info_label,
                overview,
            ),
            fg_overview_find: get(
                "editorOverviewRuler.findMatchForeground",
                base.bg_search_range,
                overview,
            ),
            fg_overview_added: get(
                "editorOverviewRuler.addedForeground",
                gutter_added,
                overview,
            ),
            fg_overview_modified: get(
                "editorOverviewRuler.modifiedForeground",
                gutter_modified,
                overview,
            ),
            fg_overview_deleted: get(
                "editorOverviewRuler.deletedForeground",
                gutter_deleted,
                overview,
            ),
            bg_overview: overview,
            fg_minimap_error: get("minimap.errorHighlight", base.fg_error, minimap),
            fg_minimap_warning: get("minimap.warningHighlight", base.fg_warning, minimap),
            fg_minimap_info: get("minimap.infoHighlight", base.fg_info_label, minimap),
            fg_minimap_find: get(
                "minimap.findMatchHighlight",
                base.bg_search_range,
                minimap,
            ),
            bg_minimap_selection: get(
                "minimap.selectionHighlight",
                base.bg_selection,
                minimap,
            ),
            fg_minimap_added: get(
                "minimapGutter.addedBackground",
                gutter_added,
                minimap,
            ),
            fg_minimap_modified: get(
                "minimapGutter.modifiedBackground",
                gutter_modified,
                minimap,
            ),
            fg_minimap_deleted: get(
                "minimapGutter.deletedBackground",
                gutter_deleted,
                minimap,
            ),
            bg_minimap_slider: minimap_slider(
                "minimapSlider.background",
                scrollbar_slider,
            ),
            bg_minimap_slider_hover: minimap_slider(
                "minimapSlider.hoverBackground",
                scrollbar_slider_hover,
            ),
            bg_minimap_slider_active: minimap_slider(
                "minimapSlider.activeBackground",
                scrollbar_slider_active,
            ),
            bg_unfocused_tab: unfocused_tab,
            fg_unfocused_tab: get("tab.unfocusedActiveForeground", base.fg_dim, unfocused_tab),
            fg_unfocused_tab_inactive: get("tab.unfocusedInactiveForeground", base.fg_dim, unfocused_inactive),
            indent_guides,
            indent_guides_active,
            indent_guide_counts: [indent_count, active_indent_count],
            bracket_guides: std::array::from_fn(|i| {
                get(
                    &format!("editorBracketPairGuide.background{}", i + 1),
                    get(
                        &format!("editorBracketHighlight.foreground{}", i + 1),
                        [
                            Color::Rgb(255, 215, 0),
                            Color::Rgb(218, 112, 214),
                            Color::Rgb(23, 159, 255),
                        ][i % 3],
                        base.bg_editor,
                    ),
                    base.bg_editor,
                )
            }),
            bracket_guides_active: std::array::from_fn(|i| {
                get(
                    &format!("editorBracketPairGuide.activeBackground{}", i + 1),
                    get(
                        &format!("editorBracketPairGuide.background{}", i + 1),
                        base.fg_accent,
                        base.bg_editor,
                    ),
                    base.bg_editor,
                )
            }),

            fg_sidebar: sidebar_fg,
            fg_sidebar_title: get("sideBarTitle.foreground", sidebar_fg, base.bg_sidebar),
            fg_section: get("sideBarSectionHeader.foreground", sidebar_fg, section),
            bg_section: section,
            fg_tab_active: get("tab.activeForeground", base.fg_text, base.bg_tab_active),
            fg_tab_inactive: get("tab.inactiveForeground", base.fg_dim, base.bg_tab_inactive),
            fg_tab_hover: get("tab.hoverForeground", get("tab.inactiveForeground", base.fg_dim, tab_hover), tab_hover),
            bg_tab_hover: tab_hover,
            fg_activity: get("activityBar.foreground", base.fg_text, base.bg_activitybar),
            fg_activity_inactive: get(
                "activityBar.inactiveForeground",
                base.fg_dim,
                base.bg_activitybar,
            ),
            bg_activity_badge: badge,
            fg_activity_badge: get("activityBarBadge.foreground", Color::Rgb(255, 255, 255), badge),
            bg_panel: panel,
            fg_panel_title: get("panelTitle.activeForeground", base.fg_text, panel),
            fg_panel_inactive: get("panelTitle.inactiveForeground", base.fg_dim, panel),
            fg_statusbar_hover: optional("statusBarItem.hoverForeground", base.bg_statusbar_item),
            fg_editor_selection: optional("editor.selectionForeground", base.bg_selection),
            fg_line_number: get("editorLineNumber.foreground", base.fg_dim, base.bg_dark),
            fg_line_number_active: get("editorLineNumber.activeForeground", base.fg_text, base.bg_dark),
            fg_whitespace: get("editorWhitespace.foreground", base.fg_dim, base.bg_editor),
            bg_find_current: find,
            fg_find: optional("editor.findMatchForeground", find),
            fg_find_other: optional("editor.findMatchHighlightForeground", base.bg_search_match),
            border_find: if colors.contains_key("editor.findMatchBorder") { optional("editor.findMatchBorder", base.bg_editor) } else { find_border_default },
            border_find_other: if colors.contains_key("editor.findMatchHighlightBorder") { optional("editor.findMatchHighlightBorder", base.bg_editor) } else { find_border_default },
            bg_widget: widget,
            fg_widget: widget_fg,
            fg_diagnostic_error: get("editorError.foreground", base.fg_error, base.bg_editor),
            fg_diagnostic_warning: get("editorWarning.foreground", base.fg_warning, base.bg_editor),
            fg_diagnostic_info: get("editorInfo.foreground", base.fg_info_label, base.bg_editor),
            fg_diagnostic_hint: get("editorHint.foreground", base.fg_dim, base.bg_editor),
            fg_gutter_added: gutter_added,
            fg_gutter_modified: gutter_modified,
            fg_gutter_deleted: gutter_deleted,
            fg_list_match: get("list.highlightForeground", base.fg_accent, base.bg_sidebar),
            fg_menu_selected: get(
                "menu.selectionForeground",
                get(
                    "list.activeSelectionForeground",
                    if high_contrast { base.fg_menu } else { Color::Rgb(255, 255, 255) },
                    base.bg_menu_selected,
                ),
                base.bg_menu_selected,
            ),
            fg_menu_disabled: disabled(base.bg_menu),
            fg_menu_separator: get(
                "menu.separatorBackground",
                if high_contrast { base.border_color } else {
                    crate::app_theme::lerp_color(base.bg_menu, base.fg_menu, 0.2)
                },
                base.bg_menu,
            ),
            border_menu: ColorLayer::resolve(colors, "menu.border", ColorLayer::default())
                .or(widget_border_layer).border(base.bg_menu).unwrap_or(base.bg_menu),
            menu_selection_border: ColorLayer::resolve(colors, "menu.selectionBorder", ColorLayer::default()),
            bg_quick_input: quick,
            fg_quick_input: get("quickInput.foreground", widget_fg, quick),
            bg_quick_selected: quick_focus_background.over(quick).unwrap_or(quick),
            fg_quick_selected: quick_focus_foreground.or(quick_foreground)
                .over(quick_focus_background.over(quick).unwrap_or(quick)).unwrap_or(widget_fg),
            fg_quick_match: get("quickInputList.focusHighlightForeground",
                get("list.focusHighlightForeground", get("list.highlightForeground", base.fg_accent, quick_selected), quick_selected), quick_selected),
            bg_chat_request: chat_request,
            fg_chat_edited_file: get("chat.editedFileForeground", base.fg_md_link, base.bg_sidebar),
            fg_chat_added: get("chat.linesAddedForeground", gutter_added, base.bg_sidebar),
            fg_chat_removed: get("chat.linesRemovedForeground", gutter_deleted, base.bg_sidebar),
            bg_breadcrumb: breadcrumb,
            fg_breadcrumb: get("breadcrumb.foreground", base.fg_dim, breadcrumb),
            fg_breadcrumb_focus: get("breadcrumb.focusForeground", base.fg_workbench, breadcrumb),
            bg_title: title,
            fg_title: get("titleBar.activeForeground", base.fg_workbench, title),
            bg_title_inactive: title_inactive,
            fg_title_inactive: get("titleBar.inactiveForeground", base.fg_dim, title_inactive),
            border_bracket: border("editorBracketMatch.border", base.bg_bracket_match),
            bg_selection_highlight: selection_highlight,
            border_selection_highlight: border("editor.selectionHighlightBorder", selection_highlight),
            border_word_highlight: border("editor.wordHighlightBorder", base.bg_word_highlight),
            fg_overview_border: get("editorOverviewRuler.border", base.fg_dim, base.bg_editor),
            fg_scrollbar: scrollbar_slider.over(base.bg_editor).unwrap_or(base.fg_dim),
            bg_minimap: minimap,
            bg_terminal: terminal,
            fg_terminal: get("terminal.foreground", base.fg_text, terminal),
            bg_terminal_selection: terminal_selection,
            fg_terminal_selection: if colors.contains_key("terminal.selectionForeground") { optional("terminal.selectionForeground", terminal_selection) } else if high_contrast { Some(if light { Color::Rgb(255, 255, 255) } else { Color::Rgb(0, 0, 0) }) } else { None },
            bg_terminal_cursor: get("terminalCursor.foreground", get("terminal.foreground", base.fg_text, terminal), terminal),
            fg_terminal_cursor: get("terminalCursor.background", terminal, get("terminalCursor.foreground", get("terminal.foreground", base.fg_text, terminal), terminal)),
            bg_notification: notification,
            fg_notification: get("notifications.foreground", base.fg_workbench, notification),
            brackets: std::array::from_fn(|i| {
                ColorLayer::resolve(
                    colors,
                    &format!("editorBracketHighlight.foreground{}", i + 1),
                    ColorLayer::opaque([
                        Color::Rgb(255, 215, 0),
                        Color::Rgb(218, 112, 214),
                        Color::Rgb(23, 159, 255),
                    ][i % 3]),
                ).border(base.bg_editor)
            }),
            ansi: std::array::from_fn(|i| {
                get(
                    [
                        "terminal.ansiBlack",
                        "terminal.ansiRed",
                        "terminal.ansiGreen",
                        "terminal.ansiYellow",
                        "terminal.ansiBlue",
                        "terminal.ansiMagenta",
                        "terminal.ansiCyan",
                        "terminal.ansiWhite",
                        "terminal.ansiBrightBlack",
                        "terminal.ansiBrightRed",
                        "terminal.ansiBrightGreen",
                        "terminal.ansiBrightYellow",
                        "terminal.ansiBrightBlue",
                        "terminal.ansiBrightMagenta",
                        "terminal.ansiBrightCyan",
                        "terminal.ansiBrightWhite",
                    ][i],
                    ansi_defaults[i],
                    terminal,
                )
            }),
        }
    }
    /// Imported themes retain exactly the specified role. Native palettes keep
    /// their existing contrast fallback behavior.
    pub(crate) fn readable_foreground(&self, color: Color, background: Color, minimum: f32) -> Color {
        crate::ui_colors::ensure_contrast(color, background, minimum, self.contrast_colors())
    }

    pub(crate) fn contrast_colors(&self) -> &[Color] {
        if self.preserve_theme_colors { &[] } else { &self.contrast_palette }
    }

    pub(crate) fn contrast_fallbacks(&self) -> Option<[Color; 14]> {
        (!self.preserve_theme_colors).then_some(self.contrast_palette)
    }

    pub(crate) fn workbench_text_color(&self, surface: Color) -> Color {
        self.readable_foreground(self.workbench_foreground.over(surface).unwrap_or(self.fg_sidebar), surface, 4.5)
    }

    pub(crate) fn description_color(&self, surface: Color) -> Color {
        self.readable_foreground(self.description_foreground.over(surface).unwrap_or(self.fg_sidebar), surface, 4.5)
    }

    pub(crate) fn sidebar_text_color(&self, surface: Color) -> Color {
        self.readable_foreground(self.sidebar_foreground.over(surface).unwrap_or(self.fg_sidebar), surface, 4.5)
    }

    pub(crate) fn git_decoration(&self, status: char) -> ColorLayer {
        if status == '!' { return self.git_conflict; }
        self.git_decorations[match status { 'A' | 'C' => 0, 'D' => 1, '?' | 'U' => 3, 'R' => 4, 'I' => 5, _ => 2 }]
    }

    pub(crate) fn git_status_color(&self, status: char, surface: Color) -> Color {
        self.readable_foreground(self.git_decoration(status).over(surface)
            .unwrap_or_else(|| self.description_color(surface)), surface, 4.5)
    }

    pub(crate) fn graph_reference_style(&self, remote: bool, surface: Color) -> Style {
        let background = self.graph_references[usize::from(remote)].over(surface).unwrap_or(surface);
        let foreground = self.graph_reference_foreground.over(background)
            .unwrap_or_else(|| self.workbench_text_color(background));
        Style::default().bg(background).fg(self.readable_foreground(foreground, background, 4.5))
    }

    pub(crate) fn keybinding_label_style(&self, surface: Color) -> Style {
        let background = self.keybinding_label_background.over(surface).unwrap_or(surface);
        let foreground = self.keybinding_label_foreground.over(background)
            .unwrap_or_else(|| self.workbench_text_color(background));
        let mut style = Style::default().fg(self.readable_foreground(foreground, background, 4.5)).bg(background);
        if let Some(border) = self.keybinding_label_border.border(background) {
            style = style.add_modifier(Modifier::UNDERLINED).underline_color(border);
        }
        style
    }

    pub(crate) fn inlay_hint_style(&self, kind: usize, surface: Color) -> Style {
        let colors = self.inlay_colors.get(kind).unwrap_or(&self.inlay_colors[0]);
        let background = colors.background.over(surface).unwrap_or(surface);
        let foreground = colors.foreground.over(background).unwrap_or_else(|| self.editor_text_color(background));
        Style::default().bg(background).fg(self.readable_foreground(foreground, background, 4.5)).add_modifier(Modifier::ITALIC)
    }

    pub(crate) fn codelens_style(&self, surface: Color) -> Style {
        let foreground = self.codelens_foreground.over(surface).unwrap_or_else(|| self.editor_text_color(surface));
        Style::default().bg(surface).fg(self.readable_foreground(foreground, surface, 4.5)).add_modifier(Modifier::ITALIC)
    }

    pub(crate) fn sticky_scroll_style(&self, surface: Color, hovered: bool) -> Style {
        let background = self.sticky_background.over(surface).unwrap_or(surface);
        let background = if hovered { self.sticky_hover_background.over(background).unwrap_or(background) } else { background };
        Style::default().bg(background).fg(self.editor_text_color(background))
    }

    /// Inspect only a marker's seven-character prefix, including bounded rope
    /// slices. Header tints retain alpha on current-line and editor surfaces.
    pub(crate) fn merge_marker_style(&self, mut prefix: impl Iterator<Item = char>, surface: Color) -> Option<Style> {
        let marker = [(); 7].map(|_| prefix.next().unwrap_or('\0'));
        let header = match marker {
            ['<', '<', '<', '<', '<', '<', '<'] => Some(0),
            ['>', '>', '>', '>', '>', '>', '>'] => Some(1),
            ['|', '|', '|', '|', '|', '|', '|'] => Some(2),
            ['=', '=', '=', '=', '=', '=', '='] => None,
            _ => return None,
        };
        let background = header.and_then(|index| self.merge_headers[index].over(surface)).unwrap_or(surface);
        let mut style = Style::default().bg(background).fg(self.editor_text_color(background));
        if let Some(border) = self.merge_border.border(background) {
            style = style.add_modifier(Modifier::UNDERLINED).underline_color(border);
        }
        Some(style)
    }

    pub(crate) fn notification_icon_color(&self, level: usize, surface: Color) -> Color {
        let foreground = self.notification_icons[level.min(2)].over(surface)
            .unwrap_or_else(|| self.icon_color(surface));
        self.readable_foreground(foreground, surface, 3.0)
    }

    pub(crate) fn badge_style(&self, surface: Color) -> Style {
        let background = self.badge_background.over(surface).unwrap_or(surface);
        let foreground = self.badge_foreground.over(background).unwrap_or(self.fg_sidebar);
        Style::default().fg(self.readable_foreground(foreground, background, 4.5)).bg(background)
    }

    pub(crate) fn icon_color(&self, surface: Color) -> Color {
        self.readable_foreground(self.icon_foreground.over(surface).unwrap_or(self.fg_sidebar), surface, 3.0)
    }

    pub(crate) fn folder_icon_color(&self, surface: Color) -> Color {
        self.readable_foreground(self.folder_icon_foreground.over(surface).unwrap_or(self.fg_sidebar), surface, 3.0)
    }

    pub(crate) fn list_icon_color(&self, foreground: Color, surface: Color, selected: bool, active: bool) -> Color {
        let layer = if !selected { ColorLayer::default() }
            else if active { self.list_active_icon } else { self.list_inactive_icon };
        self.readable_foreground(layer.over(surface).unwrap_or(foreground), surface, 3.0)
    }

    pub(crate) fn list_match_color(&self, surface: Color, focused: bool) -> Color {
        let layer = if focused { self.list_focus_match_foreground } else { self.list_match_foreground };
        self.readable_foreground(layer.over(surface).unwrap_or(self.fg_list_match), surface, 4.5)
    }

    /// Workspace search matches inherit the row text, with a find fill and
    /// outline layered on that row. A terminal underline represents the outline.
    pub(crate) fn search_match_style(&self, row: Style) -> Style {
        let surface = row.bg.unwrap_or(Color::Reset);
        let background = self.search_match_background.over(surface).unwrap_or(surface);
        let foreground = row.fg.unwrap_or(self.fg_sidebar);
        let mut style = row.bg(background)
            .fg(self.readable_foreground(foreground, background, 4.5));
        if let Some(border) = self.search_match_border.border(background) {
            style = style.add_modifier(Modifier::UNDERLINED).underline_color(border);
        }
        style
    }

    /// Resolve every row state on its owning list, including translucent text
    /// and fills. A widget's ordinary text remains the fallback for row labels.
    pub(crate) fn list_row_style(&self, foreground: ColorLayer, surface: Color, selected: bool, active: bool, hovered: bool) -> Style {
        let (fill, text) = if selected && active { (self.list_active_background, self.list_active_foreground) }
            else if selected { (self.list_inactive_background, self.list_inactive_foreground) }
            else if hovered { (self.list_hover_background, self.list_hover_foreground) }
            else { (ColorLayer::default(), foreground) };
        let background = fill.over(surface).unwrap_or(surface);
        let foreground = text.over(background).or_else(|| foreground.over(background))
            .unwrap_or_else(|| self.workbench_text_color(background));
        let mut style = Style::default().bg(background).fg(self.readable_foreground(foreground, background, 4.5));
        if selected && let Some(border) = (if active { self.list_focus_outline } else { self.list_inactive_focus_outline }).border(background) {
            style = style.add_modifier(Modifier::UNDERLINED).underline_color(border);
        }
        style
    }

    pub(crate) fn list_focus_style(&self, foreground: ColorLayer, surface: Color) -> Style {
        let background = self.list_focus_background.over(surface).unwrap_or(surface);
        let foreground = self.list_focus_foreground.over(background).or_else(|| foreground.over(background))
            .unwrap_or_else(|| self.workbench_text_color(background));
        let mut style = Style::default().bg(background).fg(self.readable_foreground(foreground, background, 4.5));
        if let Some(border) = self.list_focused_outline.border(background) {
            style = style.add_modifier(Modifier::UNDERLINED).underline_color(border);
        }
        style
    }

    pub(crate) fn list_error_color(&self, surface: Color) -> Color {
        self.readable_foreground(self.list_error_foreground.over(surface)
            .unwrap_or_else(|| self.error_text_color(surface)), surface, 4.5)
    }

    pub(crate) fn editor_text_color(&self, surface: Color) -> Color {
        self.readable_foreground(self.editor_foreground.over(surface).unwrap_or(self.fg_quick_input), surface, 4.5)
    }

    pub(crate) fn error_text_color(&self, surface: Color) -> Color {
        self.readable_foreground(self.error_foreground.over(surface).unwrap_or(self.fg_quick_input), surface, 4.5)
    }

    pub(crate) fn warning_text_color(&self, surface: Color) -> Color {
        self.readable_foreground(self.warning_foreground.over(surface).unwrap_or(self.fg_quick_input), surface, 4.5)
    }

    pub(crate) fn button_style(&self, surface: Color, primary: bool, hovered: bool, enabled: bool) -> Style {
        let control = if primary && (enabled || self.preserve_theme_colors) { self.primary_button } else { self.secondary_button };
        let fill = if !enabled || !hovered { control.background }
            else if primary { self.primary_button_hover.or(control.background) }
            else { self.secondary_button_hover.or(control.background) };
        let mut background = fill.over(surface).unwrap_or(surface);
        let foreground = control.foreground.over(background).unwrap_or_else(|| self.widget_text_color(background));
        let mut foreground = self.readable_foreground(foreground, background, 4.5);
        let mut border = control.border.border(background);
        if self.preserve_theme_colors && !enabled {
            // VS Code's button.css applies opacity 0.4 to the entire disabled
            // button, retaining its primary/secondary colors and outline.
            let dim = |color| ColorLayer::opaque(color).with_opacity(0.4).over(surface).unwrap_or(color);
            background = dim(background);
            foreground = dim(foreground);
            border = border.map(dim);
        }
        let mut style = Style::default().bg(background).fg(if enabled || self.preserve_theme_colors { foreground }
            else { crate::ui_colors::disabled_text_color(self.disabled_foreground.over(background).unwrap_or(foreground), foreground, background, self.contrast_colors()) });
        if (enabled || self.preserve_theme_colors) && let Some(border) = border {
            style = style.add_modifier(Modifier::UNDERLINED).underline_color(border);
        }
        style
    }

    pub(crate) fn widget_background_color(&self, surface: Color) -> Color {
        self.widget_background.over(surface).unwrap_or(self.bg_widget)
    }

    pub(crate) fn chat_avatar_style(&self, surface: Color) -> Style {
        let background = self.chat_avatar.background.over(surface).unwrap_or(surface);
        let foreground = self.chat_avatar.foreground.over(background).unwrap_or_else(|| self.sidebar_text_color(background));
        Style::default().bg(background).fg(self.readable_foreground(foreground, background, 4.5))
    }

    pub(crate) fn widget_text_color(&self, surface: Color) -> Color {
        self.readable_foreground(self.widget_foreground.over(surface).unwrap_or(self.fg_widget), surface, 4.5)
    }

    pub(crate) fn hover_style(&self, surface: Color) -> Style {
        let background = self.hover_colors.background.over(surface).unwrap_or(surface);
        let foreground = self.hover_colors.foreground.over(background).unwrap_or_else(|| self.widget_text_color(background));
        Style::default().bg(background).fg(self.readable_foreground(foreground, background, 4.5))
    }

    /// The popup fill has already been applied. Selection and pointer hover
    /// retain their opacity on that fill; text is resolved on the resulting row.
    pub(crate) fn suggest_row_style(&self, surface: Color, selected: bool, hovered: bool) -> Style {
        let fill = if selected { self.suggest_selection.background }
            else if hovered { self.list_hover_background } else { ColorLayer::default() };
        let background = fill.over(surface).unwrap_or(surface);
        let text = if selected { self.suggest_selection.foreground.or(self.suggest_colors.foreground) }
            else { self.suggest_colors.foreground };
        let foreground = text
            .over(background).unwrap_or_else(|| self.widget_text_color(background));
        let mut style = Style::default().bg(background).fg(self.readable_foreground(foreground, background, 4.5));
        if selected && let Some(border) = self.suggest_selection.border.border(background) {
            style = style.add_modifier(Modifier::UNDERLINED).underline_color(border);
        }
        style
    }

    pub(crate) fn input_style(&self, surface: Color) -> Style {
        let background = self.input_colors.background.over(surface).unwrap_or(surface);
        let foreground = self.input_colors.foreground.over(background).unwrap_or(self.fg_quick_input);
        Style::default().bg(background).fg(self.readable_foreground(foreground, background, 4.5))
    }

    pub(crate) fn input_placeholder_color(&self, surface: Color) -> Color {
        self.readable_foreground(self.input_placeholder.over(surface).unwrap_or(self.fg_quick_input), surface, 4.5)
    }

    pub(crate) fn input_selection_style(&self, surface: Color) -> Style {
        crate::ui_colors::input_selection_from_layers(self.input_colors.foreground, self.input_selection_background,
            surface, self.fg_quick_input, self.contrast_colors())
    }

    pub(crate) fn input_caret_style(&self, surface: Color) -> Style {
        let foreground = self.input_colors.foreground.over(surface).unwrap_or(self.fg_quick_input);
        crate::ui_colors::block_cursor_style(foreground, surface, surface, self.contrast_colors())
    }

    pub(crate) fn settings_control(&self, kind: &str) -> ControlColors {
        self.settings_controls[match kind { "checkbox" => 0, "dropdown" => 1, "number" => 2, _ => 3 }]
    }

    pub(crate) fn settings_header_color(&self, surface: Color) -> Color {
        self.readable_foreground(self.settings_header_foreground.over(surface).unwrap_or(self.fg_quick_input), surface, 4.5)
    }

    pub(crate) fn quick_text_color(&self, surface: Color, focused: bool) -> Color {
        let (layer, fallback) = if focused { (self.quick_focus_foreground.or(self.quick_foreground), self.fg_quick_selected) }
            else { (self.quick_foreground, self.fg_quick_input) };
        self.readable_foreground(layer.over(surface).unwrap_or(fallback), surface, 4.5)
    }

    pub(crate) fn quick_match_color(&self, surface: Color, focused: bool) -> Color {
        let layer = if focused { self.quick_focus_highlight } else { self.list_match_foreground };
        self.readable_foreground(layer.over(surface).unwrap_or(self.fg_quick_match), surface, 4.5)
    }

    pub(crate) fn quick_icon_color(&self, foreground: Color, surface: Color, focused: bool) -> Color {
        let color = if focused { self.quick_focus_icon.over(surface).unwrap_or(foreground) } else { foreground };
        self.readable_foreground(color, surface, 3.0)
    }

    pub(crate) fn peek_title_colors(&self, surface: Color) -> (Color, Color, Color) {
        let background = self.peek_title_background.over(surface).unwrap_or(surface);
        let foreground = self.peek_title_foreground.over(background).unwrap_or(self.fg_quick_input);
        let description = self.peek_description_foreground.over(background).unwrap_or(foreground);
        (background, self.readable_foreground(foreground, background, 4.5),
            self.readable_foreground(description, background, 4.5))
    }

    pub(crate) fn symbol_color(&self, kind: &str, surface: Color) -> Color {
        let kind = match kind {
            "trait" => "interface",
            "enum" => "enumerator",
            "enum_member" => "enumeratorMember",
            "static" => "constant",
            "impl" | "type" => "typeParameter",
            other => other,
        };
        let foreground = SYMBOL_COLOR_NAMES.iter().position(|&name| name == kind)
            .and_then(|index| self.symbol_icons[index].over(surface))
            .unwrap_or_else(|| self.icon_color(surface));
        self.readable_foreground(foreground, surface, 3.0)
    }

    /// Resolve translucent sliders on the view that owns them, including the
    /// optional scrollbar track, rather than baking in the editor background.
    pub(crate) fn scrollbar_colors(&self, surface: Color, hovered: bool, active: bool) -> (Color, Color) {
        let track = self.scrollbar_background.over(surface).unwrap_or(surface);
        let layer = if active { self.scrollbar_slider_active }
            else if hovered { self.scrollbar_slider_hover } else { self.scrollbar_slider };
        let thumb = layer.over(track).unwrap_or(self.fg_scrollbar);
        // Slider fills are intentionally subtle, unlike text or caret colors.
        // Contrast correction can turn a translucent gray into a bright block.
        (track, thumb)
    }

    pub(crate) fn suggest_match_color(&self, surface: Color, focused: bool) -> Color {
        let layer = if focused { self.suggest_focus_highlight } else { self.suggest_highlight };
        let foreground = layer.or(self.suggest_colors.foreground).over(surface)
            .unwrap_or_else(|| self.editor_text_color(surface));
        self.readable_foreground(foreground, surface, 4.5)
    }

    pub(crate) fn completion_icon_color(&self, kind: Option<lsp_types::CompletionItemKind>, surface: Color, focused: bool) -> Color {
        use lsp_types::CompletionItemKind as Kind;
        let kind = match kind {
            Some(Kind::METHOD) => "method",
            Some(Kind::FUNCTION) => "function",
            Some(Kind::CONSTRUCTOR) => "constructor",
            Some(Kind::FIELD) => "field",
            Some(Kind::VARIABLE) => "variable",
            Some(Kind::CLASS) => "class",
            Some(Kind::INTERFACE) => "interface",
            Some(Kind::MODULE) => "module",
            Some(Kind::PROPERTY) => "property",
            Some(Kind::UNIT) => "unit",
            Some(Kind::VALUE) => "object",
            Some(Kind::ENUM) => "enumerator",
            Some(Kind::KEYWORD) => "keyword",
            Some(Kind::SNIPPET) => "snippet",
            Some(Kind::COLOR) => "color",
            Some(Kind::FILE) => "file",
            Some(Kind::REFERENCE) => "reference",
            Some(Kind::FOLDER) => "folder",
            Some(Kind::ENUM_MEMBER) => "enumeratorMember",
            Some(Kind::CONSTANT) => "constant",
            Some(Kind::STRUCT) => "struct",
            Some(Kind::EVENT) => "event",
            Some(Kind::OPERATOR) => "operator",
            Some(Kind::TYPE_PARAMETER) => "typeParameter",
            _ => "text",
        };
        let foreground = if focused { self.suggest_selected_icon.over(surface) } else { None }
            .unwrap_or_else(|| self.symbol_color(kind, surface));
        self.readable_foreground(foreground, surface, 3.0)
    }

    pub(crate) fn command_center_style(
        &self,
        surface: Color,
        window_active: bool,
        hovered: bool,
    ) -> Style {
        let background = if hovered {
            self.command_center_active_background
        } else {
            self.command_center_background
        }
        .over(surface)
        .unwrap_or(surface);
        let foreground = if !window_active {
            self.command_center_inactive_foreground
        } else if hovered {
            self.command_center_active_foreground
        } else {
            self.command_center_foreground
        };
        let foreground = foreground.over(background).unwrap_or(if window_active {
            self.fg_title
        } else {
            self.fg_title_inactive
        });
        let border = if !window_active {
            self.command_center_inactive_border
        } else if hovered {
            self.command_center_active_border
        } else {
            self.command_center_border
        };
        let mut style = Style::default()
            .bg(background)
            .fg(self.readable_foreground(foreground, background, 4.5));
        if let Some(border) = border.border(background) {
            style = style
                .add_modifier(Modifier::UNDERLINED)
                .underline_color(border);
        }
        style
    }

    pub(crate) fn toolbar_style(
        &self,
        foreground: Color,
        surface: Color,
        hovered: bool,
        focused: bool,
        active: bool,
    ) -> Style {
        let style = self.toolbar_surface_style(surface, hovered, focused, active);
        style.fg(self.readable_foreground(foreground, style.bg.unwrap_or(surface), 4.5))
    }

    /// Toolbar labels retain their alpha on the final hover/active fill.
    pub(crate) fn toolbar_layer_style(
        &self, foreground: ColorLayer, surface: Color, hovered: bool, focused: bool, active: bool,
    ) -> Style {
        let style = self.toolbar_surface_style(surface, hovered, focused, active);
        let background = style.bg.unwrap_or(surface);
        style
            .fg(self.readable_foreground(foreground.over(background)
                .unwrap_or(self.fg_widget), background, 4.5))
    }

    fn toolbar_background(&self, surface: Color, hovered: bool, active: bool) -> Color {
        if active {
            self.toolbar_active.over(surface)
        } else if hovered {
            self.toolbar_hover.over(surface)
        } else {
            None
        }
        .unwrap_or(surface)
    }

    fn toolbar_surface_style(&self, surface: Color, hovered: bool, focused: bool, active: bool) -> Style {
        let background = self.toolbar_background(surface, hovered, active);
        let mut style = Style::default().bg(background);
        let outline = if focused {
            self.focus_border.border(background)
        } else if hovered {
            self.toolbar_outline.border(background)
        } else {
            self.contrast_border.border(background)
        };
        if let Some(outline) = outline {
            style = style
                .add_modifier(Modifier::UNDERLINED)
                .underline_color(outline);
        }
        style
    }

    pub(crate) fn input_option_style(
        &self,
        foreground: Color,
        surface: Color,
        active: bool,
        hovered: bool,
    ) -> Style {
        let background = if active {
            self.input_option_background.over(surface)
        } else if hovered {
            self.input_option_hover.over(surface)
        } else {
            None
        }
        .unwrap_or(surface);
        let foreground = if active {
            self.input_option_foreground
                .over(background)
                .unwrap_or(foreground)
        } else {
            foreground
        };
        let mut style = Style::default()
            .bg(background)
            .fg(self.readable_foreground(foreground, background, 4.5));
        if active && let Some(border) = self.input_option_border.border(background) {
            style = style
                .add_modifier(Modifier::UNDERLINED)
                .underline_color(border);
        }
        style
    }

    /// Transparent and null bracket roles disable palette slots. Cycle only
    /// the enabled colors; an empty palette leaves syntax coloring intact.
    pub(crate) fn bracket_color(&self, depth: usize) -> Option<Color> {
        let count = self.brackets.iter().flatten().count();
        self.brackets.iter().flatten().nth(depth.checked_rem(count)?).copied()
    }

    pub(crate) fn indent_guide_color(&self, depth: usize, active: bool) -> Color {
        let slot = usize::from(active);
        let index = depth % usize::from(self.indent_guide_counts[slot]).max(1);
        if active { self.indent_guides_active[index] } else { self.indent_guides[index] }
    }

    pub(crate) fn bracket_style(&self, style: Style, background: Color) -> Style {
        let mut style = style.bg(background);
        if let Some(border) = self.border_bracket {
            style = style
                .add_modifier(Modifier::UNDERLINED)
                .underline_color(border);
        }
        style
    }

    pub(crate) fn occurrence_style(
        &self,
        style: Style,
        word_background: Color,
        selection: bool,
    ) -> Style {
        let mut style = style.bg(if selection {
            self.bg_selection_highlight
        } else {
            word_background
        });
        if let Some(border) = if selection {
            self.border_selection_highlight
        } else {
            self.border_word_highlight
        } {
            style = style
                .add_modifier(Modifier::UNDERLINED)
                .underline_color(border);
        }
        style
    }

    /// Terminals cannot stroke a rectangle over text cells. Use the theme's
    /// outline color as an underline, preserving the token foreground.
    pub(crate) fn find_style(&self, style: Style, current: bool, other_bg: Color) -> Style {
        let (bg, fg, border) = if current {
            (self.bg_find_current, self.fg_find, self.border_find)
        } else {
            (other_bg, self.fg_find_other, self.border_find_other)
        };
        let mut style = style.bg(bg);
        if let Some(fg) = fg { style = style.fg(fg); }
        if let Some(border) = border {
            style = style.add_modifier(Modifier::UNDERLINED).underline_color(border);
        }
        style
    }

    pub(crate) fn diagnostic(&self, severity: lsp_types::DiagnosticSeverity) -> Color {
        match severity {
            lsp_types::DiagnosticSeverity::ERROR => self.fg_diagnostic_error,
            lsp_types::DiagnosticSeverity::WARNING => self.fg_diagnostic_warning,
            lsp_types::DiagnosticSeverity::INFORMATION => self.fg_diagnostic_info,
            _ => self.fg_diagnostic_hint,
        }
    }
    pub(crate) fn terminal_color(&self, color: Color, foreground: bool) -> Color {
        match color {
            Color::Reset => {
                if foreground {
                    self.fg_terminal
                } else {
                    self.bg_terminal
                }
            }
            Color::Indexed(i) if i < 16 => self.ansi[i as usize],
            Color::Black => self.ansi[0],
            Color::Red => self.ansi[1],
            Color::Green => self.ansi[2],
            Color::Yellow => self.ansi[3],
            Color::Blue => self.ansi[4],
            Color::Magenta => self.ansi[5],
            Color::Cyan => self.ansi[6],
            Color::Gray => self.ansi[7],
            Color::DarkGray => self.ansi[8],
            Color::LightRed => self.ansi[9],
            Color::LightGreen => self.ansi[10],
            Color::LightYellow => self.ansi[11],
            Color::LightBlue => self.ansi[12],
            Color::LightMagenta => self.ansi[13],
            Color::LightCyan => self.ansi[14],
            Color::White => self.ansi[15],
            other => other,
        }
    }
}
/// VS Code terminal defaults (terminalColorRegistry.ts). Explicit theme ANSI
/// values win; truecolor/256-color application output is preserved.
fn terminal_ansi_defaults(light: bool, high_contrast: bool) -> [Color; 16] {
    let values: [u32; 16] = match (light, high_contrast) {
        (false, false) => [0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5,
            0x666666, 0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xe5e5e5],
        (true, false) => [0x000000, 0xcd3131, 0x107c10, 0x949800, 0x0451a5, 0xbc05bc, 0x0598bc, 0x555555,
            0x666666, 0xf14c4c, 0x14ce14, 0xb5ba00, 0x3b8eea, 0xd670d6, 0x29b8db, 0xa5a5a5],
        (false, true) => [0x000000, 0xcd0000, 0x00cd00, 0xcdcd00, 0x0000ee, 0xcd00cd, 0x00cdcd, 0xe5e5e5,
            0x7f7f7f, 0xff0000, 0x00ff00, 0xffff00, 0x5c5cff, 0xff00ff, 0x00ffff, 0xffffff],
        (true, true) => [0x292929, 0xcd3131, 0x136c13, 0x949800, 0x0451a5, 0xbc05bc, 0x0598bc, 0x555555,
            0x666666, 0xcd3131, 0x00bc00, 0xb5ba00, 0x0451a5, 0xbc05bc, 0x0598bc, 0xa5a5a5],
    };
    values.map(|rgb| Color::Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8))
}

// Filled with the resolved cache before the palette reaches any renderer.
impl Default for WorkbenchColors {
    fn default() -> Self {
        Self {
            preserve_theme_colors: false,
            contrast_palette: [Color::Reset; 14],
            workbench_foreground: ColorLayer::default(),
            description_foreground: ColorLayer::default(),
            editor_foreground: ColorLayer::default(),
            ruler_foreground: ColorLayer::default(),
            codelens_foreground: ColorLayer::default(),
            inlay_colors: [ControlColors::default(); 3],
            sticky_background: ColorLayer::default(),
            sticky_hover_background: ColorLayer::default(),
            sticky_border: ColorLayer::default(),
            sidebar_foreground: ColorLayer::default(),
            sidebar_title_foreground: ColorLayer::default(),
            git_decorations: [ColorLayer::default(); 6],
            git_conflict: ColorLayer::default(),
            section_foreground: ColorLayer::default(),
            sidebar_sticky_background: ColorLayer::default(),
            graph_lane_foreground: ColorLayer::default(),
            graph_references: [ColorLayer::default(); 2],
            graph_reference_foreground: ColorLayer::default(),
            widget_background: ColorLayer::default(),
            widget_foreground: ColorLayer::default(),
            widget_border: ColorLayer::default(),
            hover_colors: ControlColors::default(),
            suggest_colors: ControlColors::default(),
            suggest_selection: ControlColors::default(),
            merge_headers: [ColorLayer::default(); 3],
            merge_contents: [ColorLayer::default(); 3],
            merge_border: ColorLayer::default(),
            sidebar_border: ColorLayer::default(),
            activity_border: ColorLayer::default(),
            sash_hover_border: ColorLayer::default(),
            panel_border: ColorLayer::default(),
            editor_split_borders: [ColorLayer::default(); 2],
            hover_highlight: ColorLayer::default(),
            line_number_foreground: ColorLayer::default(),
            error_foreground: ColorLayer::default(),
            warning_foreground: ColorLayer::default(),
            disabled_foreground: ColorLayer::default(),
            primary_button: ControlColors::default(),
            secondary_button: ControlColors::default(),
            primary_button_hover: ColorLayer::default(),
            secondary_button_hover: ColorLayer::default(),
            keybinding_label_background: ColorLayer::default(),
            keybinding_label_foreground: ColorLayer::default(),
            keybinding_label_border: ColorLayer::default(),
            keybinding_table_header: ColorLayer::default(),
            keybinding_table_row: ColorLayer::default(),
            notification_icons: [ColorLayer::default(); 3],
            output_timestamp: ColorLayer::default(),
            badge_background: ColorLayer::default(),
            badge_foreground: ColorLayer::default(),
            icon_foreground: ColorLayer::default(),
            folder_icon_foreground: ColorLayer::default(),
            list_active_icon: ColorLayer::default(),
            list_inactive_icon: ColorLayer::default(),
            list_match_foreground: ColorLayer::default(),
            list_focus_match_foreground: ColorLayer::default(),
            search_match_background: ColorLayer::default(),
            search_match_border: ColorLayer::default(),
            symbol_icons: [ColorLayer::default(); SYMBOL_COLOR_NAMES.len()],
            scrollbar_background: ColorLayer::default(),
            scrollbar_slider: ColorLayer::default(),
            scrollbar_slider_hover: ColorLayer::default(),
            scrollbar_slider_active: ColorLayer::default(),
            suggest_highlight: ColorLayer::default(),
            suggest_focus_highlight: ColorLayer::default(),
            suggest_selected_icon: ColorLayer::default(),
            input_colors: ControlColors::default(),
            dropdown_colors: ControlColors::default(),
            menu_colors: ControlColors::default(),
            menu_selection_colors: ControlColors::default(),
            menu_separator: ColorLayer::default(),
            input_placeholder: ColorLayer::default(),
            input_selection_background: ColorLayer::default(),
            settings_controls: [ControlColors::default(); 4],
            settings_row_border: ColorLayer::default(),
            settings_header_foreground: ColorLayer::default(),
            settings_dropdown_list: ControlColors::default(),
            list_active_background: ColorLayer::default(),
            list_active_foreground: ColorLayer::default(),
            list_inactive_background: ColorLayer::default(),
            list_inactive_foreground: ColorLayer::default(),
            list_hover_background: ColorLayer::default(),
            list_hover_foreground: ColorLayer::default(),
            list_drop_background: ColorLayer::default(),
            list_focus_background: ColorLayer::default(),
            list_focus_foreground: ColorLayer::default(),
            list_focused_outline: ColorLayer::default(),
            list_error_foreground: ColorLayer::default(),
            list_focus_outline: ColorLayer::default(),
            list_inactive_focus_outline: ColorLayer::default(),
            action_list_background: ColorLayer::default(),
            action_list_foreground: ColorLayer::default(),
            action_list_focus_background: ColorLayer::default(),
            action_list_focus_foreground: ColorLayer::default(),
            quick_foreground: ColorLayer::default(),
            quick_background: ColorLayer::default(),
            quick_focus_background: ColorLayer::default(),
            quick_focus_foreground: ColorLayer::default(),
            quick_focus_highlight: ColorLayer::default(),
            quick_focus_icon: ColorLayer::default(),
            quick_title_background: ColorLayer::default(),
            peek_border: ColorLayer::default(),
            peek_title_background: ColorLayer::default(),
            peek_title_foreground: ColorLayer::default(),
            peek_description_foreground: ColorLayer::default(),
            peek_editor_background: ColorLayer::default(),
            peek_gutter_background: ColorLayer::default(),
            command_center_background: ColorLayer::default(),
            command_center_active_background: ColorLayer::default(),
            command_center_foreground: ColorLayer::default(),
            command_center_active_foreground: ColorLayer::default(),
            command_center_inactive_foreground: ColorLayer::default(),
            command_center_border: ColorLayer::default(),
            command_center_active_border: ColorLayer::default(),
            command_center_inactive_border: ColorLayer::default(),
            menubar_background: ColorLayer::default(),
            menubar_foreground: ColorLayer::default(),
            menubar_border: ColorLayer::default(),
            md_inline_background: ColorLayer::default(),
            md_inline_foreground: ColorLayer::default(),
            md_code_background: ColorLayer::default(),
            md_quote_background: ColorLayer::default(),
            md_quote_border: ColorLayer::default(),
            chat_avatar: ControlColors::default(),
            chat_request_hover_background: ColorLayer::default(),
            focus_border: ColorLayer::default(),
            contrast_border: ColorLayer::default(),
            active_contrast_border: ColorLayer::default(),
            toolbar_hover: ColorLayer::default(),
            toolbar_active: ColorLayer::default(),
            toolbar_outline: ColorLayer::default(),
            input_option_background: ColorLayer::default(),
            input_option_foreground: ColorLayer::default(),
            input_option_border: ColorLayer::default(),
            input_option_hover: ColorLayer::default(),
            bg_unfocused_tab_inactive: Color::Reset,
            bg_unfocused_tab_hover: Color::Reset,
            fg_unfocused_tab_hover: Color::Reset,
            border_tab: None,
            border_tab_active: None,
            border_tab_unfocused: None,
            border_tab_hover: None,
            bg_activity_active: ColorLayer::default(),
            border_activity_active: ColorLayer::default(),
            border_panel_active: Color::Reset,
            border_terminal_tab: Color::Reset,
            border_sidebar: None,
            border_section: None,
            border_notification: None,
            border_diff: None,
            border_diff_added: None,
            border_diff_removed: None,
            fg_diff_diagonal: Color::Reset,
            bg_md_inline: Color::Reset,
            fg_md_inline: Color::Reset,
            fg_folding: Color::Reset,
            fg_group_border: Color::Reset,
            bg_editor_drop: Color::Reset,
            bg_settings_hover: Color::Reset,
            bg_settings_focus: Color::Reset,
            fg_overview_error: Color::Reset,
            fg_overview_warning: Color::Reset,
            fg_overview_info: Color::Reset,
            fg_overview_find: Color::Reset,
            fg_overview_added: Color::Reset,
            fg_overview_modified: Color::Reset,
            fg_overview_deleted: Color::Reset,
            bg_overview: Color::Reset,
            fg_minimap_error: Color::Reset,
            fg_minimap_warning: Color::Reset,
            fg_minimap_info: Color::Reset,
            fg_minimap_find: Color::Reset,
            bg_minimap_selection: Color::Reset,
            fg_minimap_added: Color::Reset,
            fg_minimap_modified: Color::Reset,
            fg_minimap_deleted: Color::Reset,
            bg_minimap_slider: Color::Reset,
            bg_minimap_slider_hover: Color::Reset,
            bg_minimap_slider_active: Color::Reset,
            bg_unfocused_tab: Color::Reset,
            fg_unfocused_tab: Color::Reset,
            fg_unfocused_tab_inactive: Color::Reset,
            indent_guides: [Color::Reset; 6],
            indent_guides_active: [Color::Reset; 6],
            indent_guide_counts: [1; 2],
            bracket_guides: [Color::Reset; 6],
            bracket_guides_active: [Color::Reset; 6],

            fg_sidebar: Color::Reset,
            fg_sidebar_title: Color::Reset,
            fg_section: Color::Reset,
            bg_section: Color::Reset,
            fg_tab_active: Color::Reset,
            fg_tab_inactive: Color::Reset,
            fg_tab_hover: Color::Reset,
            bg_tab_hover: Color::Reset,
            fg_activity: Color::Reset,
            fg_activity_inactive: Color::Reset,
            bg_activity_badge: Color::Reset,
            fg_activity_badge: Color::Reset,
            bg_panel: Color::Reset,
            fg_panel_title: Color::Reset,
            fg_panel_inactive: Color::Reset,
            fg_statusbar_hover: None,
            fg_editor_selection: None,
            fg_line_number: Color::Reset,
            fg_line_number_active: Color::Reset,
            fg_whitespace: Color::Reset,
            bg_find_current: Color::Reset,
            fg_find: None,
            fg_find_other: None,
            border_find: None,
            border_find_other: None,
            bg_widget: Color::Reset,
            fg_widget: Color::Reset,
            fg_diagnostic_error: Color::Reset,
            fg_diagnostic_warning: Color::Reset,
            fg_diagnostic_info: Color::Reset,
            fg_diagnostic_hint: Color::Reset,
            fg_gutter_added: Color::Reset,
            fg_gutter_modified: Color::Reset,
            fg_gutter_deleted: Color::Reset,
            fg_list_match: Color::Reset,
            fg_menu_selected: Color::Reset,
            fg_menu_disabled: Color::Reset,
            fg_menu_separator: Color::Reset,
            border_menu: Color::Reset,
            menu_selection_border: ColorLayer::default(),
            bg_quick_input: Color::Reset,
            fg_quick_input: Color::Reset,
            bg_quick_selected: Color::Reset,
            fg_quick_selected: Color::Reset,
            fg_quick_match: Color::Reset,
            bg_chat_request: Color::Reset,
            fg_chat_edited_file: Color::Reset,
            fg_chat_added: Color::Reset,
            fg_chat_removed: Color::Reset,
            bg_breadcrumb: Color::Reset,
            fg_breadcrumb: Color::Reset,
            fg_breadcrumb_focus: Color::Reset,
            bg_title: Color::Reset,
            fg_title: Color::Reset,
            bg_title_inactive: Color::Reset,
            fg_title_inactive: Color::Reset,
            border_bracket: None,
            bg_selection_highlight: Color::Reset,
            border_selection_highlight: None,
            border_word_highlight: None,
            fg_overview_border: Color::Reset,
            fg_scrollbar: Color::Reset,
            bg_minimap: Color::Reset,
            bg_terminal: Color::Reset,
            fg_terminal: Color::Reset,
            bg_terminal_selection: Color::Reset,
            fg_terminal_selection: None,
            bg_terminal_cursor: Color::Reset,
            fg_terminal_cursor: Color::Reset,
            bg_notification: Color::Reset,
            fg_notification: Color::Reset,
            brackets: [None; 6],
            ansi: [Color::Reset; 16],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracket_palette_skips_disabled_slots_before_blending_alpha() {
        let mut base = crate::app_theme::EditorTheme::Dark.colors();
        base.bg_editor = Color::Rgb(16, 32, 48);
        let values = serde_json::json!({
            "editorBracketHighlight.foreground1": "#ff000080",
            "editorBracketHighlight.foreground2": null,
            "editorBracketHighlight.foreground3": "#ffffff00",
            "editorBracketHighlight.foreground4": "#112233",
            "editorBracketHighlight.foreground5": "#00000000",
            "editorBracketHighlight.foreground6": "#102030"
        });
        let colors = WorkbenchColors::resolve(values.as_object().unwrap(), &base, false, false);
        let expected = [Color::Rgb(136, 16, 24), Color::Rgb(17, 34, 51), base.bg_editor];
        assert_eq!(colors.brackets[1], None);
        assert_eq!(colors.brackets[2], None);
        assert_eq!(colors.brackets[4], None);
        // Even an opaque color matching the background remains an authored
        // palette entry. Only alpha-zero and null roles are disabled.
        for depth in 0..12 {
            assert_eq!(colors.bracket_color(depth), Some(expected[depth % 3]));
        }
        assert_eq!(colors.bracket_color(usize::MAX), Some(expected[usize::MAX % 3]));
    }

    #[test]
    fn bracket_palette_handles_all_disabled_roles_and_native_fallbacks() {
        let base = crate::app_theme::EditorTheme::Dark.colors();
        let values = serde_json::json!({
            "editorBracketHighlight.foreground1": null,
            "editorBracketHighlight.foreground2": "#00000000",
            "editorBracketHighlight.foreground3": null,
            "editorBracketHighlight.foreground4": "#ffffff00",
            "editorBracketHighlight.foreground5": null,
            "editorBracketHighlight.foreground6": "#12345600"
        });
        let colors = WorkbenchColors::resolve(values.as_object().unwrap(), &base, false, false);
        assert_eq!(colors.bracket_color(0), None);
        assert_eq!(colors.bracket_color(usize::MAX), None);
        let native = WorkbenchColors::resolve(&Map::new(), &base, false, false);
        assert!(native.brackets.iter().all(Option::is_some));
        assert_eq!(native.bracket_color(0), Some(Color::Rgb(255, 215, 0)));
        assert_eq!(native.bracket_color(3), native.bracket_color(0));
        assert_eq!(native.bracket_color(6), native.bracket_color(0));
    }

    #[test]
    fn minimap_slider_resolves_alpha_on_its_own_surface_and_inherits_scrollbar() {
        let base = crate::app_theme::EditorTheme::Dark.colors();
        let values = serde_json::json!({
            "minimap.background": "#203040",
            "minimapSlider.background": "#ffffff80",
            "scrollbarSlider.hoverBackground": "#ffffff80"
        });
        let colors = WorkbenchColors::resolve(values.as_object().unwrap(), &base, false, false);
        assert_eq!(colors.bg_minimap, Color::Rgb(32, 48, 64));
        assert_eq!(colors.bg_minimap_slider, Color::Rgb(144, 152, 160));
        assert_eq!(colors.bg_minimap_slider_hover, Color::Rgb(88, 100, 112));
    }

    #[test]
    fn minimap_explicit_slider_colors_take_precedence_over_scrollbar_colors() {
        let base = crate::app_theme::EditorTheme::Dark.colors();
        let values = serde_json::json!({
            "minimapSlider.background": "#123456",
            "minimapSlider.hoverBackground": "#abcdef",
            "minimapSlider.activeBackground": "#654321",
            "scrollbarSlider.background": "#ff0000",
            "scrollbarSlider.hoverBackground": "#00ff00",
            "scrollbarSlider.activeBackground": "#0000ff"
        });
        let colors = WorkbenchColors::resolve(values.as_object().unwrap(), &base, false, false);
        assert_eq!(colors.bg_minimap_slider, Color::Rgb(18, 52, 86));
        assert_eq!(colors.bg_minimap_slider_hover, Color::Rgb(171, 205, 239));
        assert_eq!(colors.bg_minimap_slider_active, Color::Rgb(101, 67, 33));
    }

    #[test]
    fn scrollbar_and_minimap_defaults_match_vscode_theme_kinds() {
        let mut base = crate::app_theme::EditorTheme::Dark.colors();
        base.bg_editor = Color::Rgb(0, 0, 0);
        // These are the registered VS Code defaults composited on black.
        // A contrasting editor foreground must not influence slider defaults.
        for (light, high_contrast, scrollbar, minimap) in [
            (false, false, [(48, 48, 48), (70, 70, 70), (76, 76, 76)],
                [(24, 24, 24), (35, 35, 35), (38, 38, 38)]),
            (true, false, [(40, 40, 40), (70, 70, 70), (0, 0, 0)],
                [(20, 20, 20), (35, 35, 35), (0, 0, 0)]),
            (false, true, [(67, 117, 134), (89, 156, 178), (111, 195, 223)],
                [(34, 59, 67), (44, 78, 89), (56, 98, 112)]),
            (true, true, [(6, 30, 53), (12, 59, 106), (15, 74, 133)],
                [(3, 15, 27), (6, 30, 53), (8, 37, 67)]),
        ] {
            let colors = WorkbenchColors::resolve(&Map::new(), &base, light, high_contrast);
            for (i, (hovered, active)) in [(false, false), (true, false), (true, true)].into_iter().enumerate() {
                let (r, g, b) = scrollbar[i];
                assert_eq!(colors.scrollbar_colors(base.bg_editor, hovered, active).1, Color::Rgb(r, g, b));
            }
            for (actual, (r, g, b)) in [colors.bg_minimap_slider, colors.bg_minimap_slider_hover, colors.bg_minimap_slider_active]
                .into_iter().zip(minimap) {
                assert_eq!(actual, Color::Rgb(r, g, b));
            }
        }
    }

    #[test]
    fn minimap_and_scrollbar_preserve_transparent_roles_and_independent_state_defaults() {
        let base = crate::app_theme::EditorTheme::Dark.colors();
        let values = serde_json::json!({
            "minimap.background": "#203040",
            "minimapSlider.background": "#ffffff00",
            "minimapSlider.hoverBackground": "#ffffff00",
            "minimapSlider.activeBackground": "#ffffff00",
            "scrollbarSlider.background": "#ff000000",
            "scrollbarSlider.hoverBackground": "#00ff0000",
            "scrollbarSlider.activeBackground": "#0000ff00"
        });
        let colors = WorkbenchColors::resolve(values.as_object().unwrap(), &base, false, false);
        for (hovered, active) in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(colors.scrollbar_colors(base.bg_editor, hovered, active), (base.bg_editor, base.bg_editor));
        }
        assert_eq!(colors.bg_minimap_slider, colors.bg_minimap);
        assert_eq!(colors.bg_minimap_slider_hover, colors.bg_minimap);
        assert_eq!(colors.bg_minimap_slider_active, colors.bg_minimap);

        let values = serde_json::json!({
            "minimap.background": "#000000",
            "scrollbarSlider.background": "#ffffff"
        });
        let colors = WorkbenchColors::resolve(values.as_object().unwrap(), &base, false, false);
        assert_eq!(colors.bg_minimap_slider, Color::Rgb(128, 128, 128));
        assert_eq!(colors.bg_minimap_slider_hover, Color::Rgb(35, 35, 35));
        assert_eq!(colors.bg_minimap_slider_active, Color::Rgb(38, 38, 38));
    }
}
