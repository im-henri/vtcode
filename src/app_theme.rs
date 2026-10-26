use ratatui::style::Color;

#[cfg(test)]
pub(crate) use crate::app_theme_loader::hex_to_color;
#[cfg(test)]
pub(crate) use crate::app_theme_loader::load_all_themes;
pub(crate) use crate::app_theme_loader::{
    ThemeFile, config_key_from_command_id, load_builtin_themes, load_external_themes, merge_themes,
    theme_command_id,
};

/// JSON-serializable color as hex string "#RRGGBB"
#[cfg(test)]
fn color_to_hex(c: Color) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{:02X}{:02X}{:02X}", r, g, b),
        Color::White => "#FFFFFF".into(),
        Color::Black => "#000000".into(),
        _ => "#000000".into(),
    }
}

#[cfg(test)]
pub(crate) fn offset_color(color: Color, delta: i16) -> Color {
    match color {
        Color::Rgb(r, g, b) => {
            let adj = |v: u8| -> u8 { (v as i16 + delta).clamp(0, 255) as u8 };
            Color::Rgb(adj(r), adj(g), adj(b))
        }
        _ => color,
    }
}

/// Interpolate between two RGB colors. `t` is 0.0 (start) to 1.0 (end).
pub(crate) fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    match (a, b) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let mix = |a: u8, b: u8| -> u8 {
                (f32::from(b) - f32::from(a))
                    .mul_add(t, f32::from(a))
                    .round() as u8
            };
            Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
        }
        _ => {
            if t < 0.5 {
                a
            } else {
                b
            }
        }
    }
}

/// Pre-computed gradient parameters for a given (width, height, angle).
/// Avoids redundant trig per cell — compute once, then use `t_at()`.
pub(crate) struct GradientPrecomputed {
    inv_w: f32,
    inv_h: f32,
    vx: f32,
    vy: f32,
    min_p: f32,
    inv_range: f32, // 1.0 / (max_p - min_p), 0.0 if degenerate
}

impl GradientPrecomputed {
    pub(crate) fn new(width: u16, height: u16, angle_deg: f32) -> Self {
        let inv_w = if width > 1 {
            1.0 / f32::from(width - 1)
        } else {
            0.0
        };
        let inv_h = if height > 1 {
            1.0 / f32::from(height - 1)
        } else {
            0.0
        };
        let span_x = if width > 1 { 1.0 } else { 0.0 };
        let span_y = if height > 1 { 1.0 } else { 0.0 };
        let radians = angle_deg.to_radians();
        let vx = radians.cos();
        let vy = radians.sin();
        let p10 = span_x * vx;
        let p01 = span_y * vy;
        let p11 = p10 + p01;
        let min_p = 0.0f32.min(p10.min(p01.min(p11)));
        let max_p = 0.0f32.max(p10.max(p01.max(p11)));
        let range = max_p - min_p;
        let inv_range = if range.abs() <= f32::EPSILON {
            0.0
        } else {
            1.0 / range
        };
        Self {
            inv_w,
            inv_h,
            vx,
            vy,
            min_p,
            inv_range,
        }
    }

    #[inline(always)]
    pub(crate) fn t_at(&self, local_x: u16, local_y: u16) -> f32 {
        if self.inv_range == 0.0 {
            return 0.0;
        }
        let tx = f32::from(local_x) * self.inv_w;
        let ty = f32::from(local_y) * self.inv_h;
        (tx.mul_add(self.vx, ty * self.vy - self.min_p) * self.inv_range).clamp(0.0, 1.0)
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ThemeGradientSpec {
    pub start: Color,
    pub end: Color,
    pub angle_deg: f32,
}

#[derive(Clone, Copy)]
#[allow(dead_code)] // some fields are reserved for future per-element gradient rendering
pub(crate) struct ThemeGradients {
    pub bg_dark: ThemeGradientSpec,
    pub bg_sidebar: ThemeGradientSpec,
    pub bg_editor: ThemeGradientSpec,
    pub bg_tabbar: ThemeGradientSpec,
    pub bg_tab_active: ThemeGradientSpec,
    pub bg_tab_inactive: ThemeGradientSpec,
    pub bg_statusbar: ThemeGradientSpec,
    pub bg_statusbar_item: ThemeGradientSpec,
    pub fg_statusbar: ThemeGradientSpec,
    pub bg_line_highlight: ThemeGradientSpec,
    pub fg_text: ThemeGradientSpec,
    pub fg_dim: ThemeGradientSpec,
    pub fg_accent: ThemeGradientSpec,
    pub bg_selected: ThemeGradientSpec,
    pub fg_selected: ThemeGradientSpec,
}

/// Pre-computed color values for the current theme.
/// Stored on App and updated when theme changes, avoiding
/// repeated match dispatch on every widget render.
#[derive(Clone, Copy)]
pub(crate) struct ThemeColors {
    pub ui: crate::theme_workbench::WorkbenchColors,
    pub fg_workbench: Color,
    pub bg_dark: Color,
    pub bg_sidebar: Color,
    pub bg_editor: Color,
    pub bg_tabbar: Color,
    pub bg_tab_active: Color,
    pub bg_tab_inactive: Color,
    pub bg_statusbar: Color,
    pub bg_statusbar_item: Color,
    pub fg_statusbar: Color,
    pub bg_line_highlight: Color,
    pub fg_text: Color,
    pub fg_dim: Color,
    pub fg_accent: Color,
    pub bg_selected: Color,
    pub fg_selected: Color,
    pub bg_diff_added: Color,
    pub bg_diff_removed: Color,
    pub bg_diff_added_highlight: Color,
    pub bg_diff_removed_highlight: Color,
    pub bg_selection: Color,
    pub bg_bracket_match: Color,
    pub bg_word_highlight: Color,
    pub bg_search_range: Color,
    pub fg_bookmark: Color,
    pub bg_fold_indicator: Color,
    pub bg_sticky_header: Color,
    pub fg_todo: Color,
    pub fg_fixme: Color,
    pub fg_tilde: Color,
    pub fg_overwrite_cursor: Color,
    pub bg_cursor: Color,
    pub fg_cursor: Color,
    pub fg_git_modified: Color,
    pub fg_git_added: Color,
    pub fg_git_deleted: Color,
    pub fg_modified_indicator: Color,
    pub bg_menu: Color,
    pub bg_activitybar: Color,
    pub fg_menu: Color,
    pub bg_menu_selected: Color,
    pub border_color: Color,
    pub fg_accent_blue: Color,
    pub bg_hover: Color,
    pub fg_folder_icon: Color,
    pub bg_explorer_selected: Color,
    pub fg_info_label: Color,
    pub fg_success: Color,
    pub fg_error: Color,
    pub fg_warning: Color,
    pub fg_hack: Color,
    pub fg_md_code: Color,
    pub bg_md_code: Color,
    pub fg_md_heading: Color,
    pub fg_md_link: Color,
    pub fg_diff_added: Color,
    pub fg_diff_removed: Color,
    pub fg_md_blockquote: Color,
    pub bg_sc_button: Color,
    pub bg_sc_button_secondary: Color,
    pub bg_input: Color,
    pub fg_input: Color,
    pub fg_input_placeholder: Color,
    pub bg_search_match: Color,
    pub gradients: ThemeGradients,
}

// Compatibility helper used by rendering tests. Runtime selection is registry based.
// Compatibility helper used by rendering tests. Runtime selection is registry based.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum EditorTheme {
    Dark,
    #[cfg(test)]
    Light,
}
impl EditorTheme {
    #[cfg(test)]
    pub(crate) fn syntax_theme_key(self) -> &'static str {
        if self == Self::Dark {
            "dark-plus"
        } else {
            "InspiredGitHub"
        }
    }
    pub(crate) fn colors(self) -> ThemeColors {
        crate::app_theme_loader::builtin_theme(if self == Self::Dark { "dark" } else { "light" })
            .expect("bundled VS Code theme")
            .to_theme_colors()
    }
}

#[cfg(test)]
#[path = "app_theme_tests.rs"]
mod tests;
