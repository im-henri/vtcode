use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    widgets::{Clear, Paragraph},
    Frame,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, TerminalDock};
use crate::ui_animation_chrome::{draw as draw_static_chrome, Region as ChromeRegion};
use crate::{
    ui_chrome, ui_dialogs, ui_helpers, ui_panels, ui_pickers, ui_search_dialog, ui_settings,
};

#[cfg(test)]
#[path = "ui_workbench_gradient_tests.rs"]
mod workbench_gradient_tests;

#[cfg(test)]
thread_local! {
    static REFERENCE_WORKBENCH_GRADIENT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Height of the explorer tooltip (file path + details).
const EXPLORER_TOOLTIP_HEIGHT: u16 = 2;
/// Comfortable editor width to preserve when a sidebar is visible. On smaller
/// terminals the editor keeps roughly two thirds of the available workbench.
const COMFORTABLE_EDITOR_WIDTH: u16 = 44;
/// Smallest useful bottom panel. Below this height the panel is temporarily
/// omitted instead of reducing the editor to a one-row sliver.
const MIN_BOTTOM_PANEL_HEIGHT: u16 = 3;
/// Rows kept for editing while the bottom panel is visible.
const MIN_EDITOR_HEIGHT_WITH_PANEL: u16 = 4;

fn responsive_sidebar_width(available: u16, requested: u16) -> u16 {
    if available <= 1 {
        return 0;
    }

    let editor_reserve = available
        .saturating_mul(2)
        .checked_div(3)
        .unwrap_or(0)
        .clamp(1, COMFORTABLE_EDITOR_WIDTH);
    let max_sidebar = available.saturating_sub(editor_reserve);
    if max_sidebar == 0 {
        return 0;
    }

    requested.clamp(12.min(max_sidebar), max_sidebar)
}

fn responsive_right_dock_width(available: u16, expanded: bool) -> u16 {
    if available <= 1 {
        return 0;
    }
    if !expanded {
        return 3.min(available.saturating_sub(1));
    }

    // Keep approximately two thirds of the workbench for the editor and its
    // sidebar. The old 32-column hard minimum overwhelmed ordinary 80-column
    // terminals whenever Explorer and the right terminal were both open.
    available
        .div_ceil(3)
        .min(56)
        .min(available.saturating_sub(1))
}

pub(crate) fn responsive_panel_height(available: u16, requested: u16) -> u16 {
    let editor_reserve = available
        .saturating_mul(2)
        .checked_div(3)
        .unwrap_or(0)
        .clamp(MIN_EDITOR_HEIGHT_WITH_PANEL, 8)
        .min(available);
    let max_panel = available.saturating_sub(editor_reserve);
    if max_panel < MIN_BOTTOM_PANEL_HEIGHT {
        0
    } else {
        requested.clamp(MIN_BOTTOM_PANEL_HEIGHT, max_panel)
    }
}

/// Find color codes in a line, returns (col_start, col_len, r, g, b) tuples.
/// Supports: #RRGGBB, #RGB, rgb(r,g,b), rgba(r,g,b,a), hsl(h,s%,l%)
#[cfg(test)]
pub(crate) fn find_hex_colors(line: &str) -> Vec<(usize, usize, u8, u8, u8)> {
    let mut results = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let len = chars.len();
    let mut i = 0;
    while i < len {
        // #RRGGBB or #RRGGBBAA
        if chars[i] == '#' && i + 6 < len {
            let all_hex = chars[i + 1..i + 7].iter().all(|c| c.is_ascii_hexdigit());
            if all_hex {
                // Check it's not an 8-digit hex or longer identifier
                let next = i + 7;
                let is_8digit = next < len
                    && chars[next].is_ascii_hexdigit()
                    && next + 1 < len
                    && chars[next + 1].is_ascii_hexdigit()
                    && (next + 2 >= len || !chars[next + 2].is_ascii_hexdigit());
                let token_len = if is_8digit { 9 } else { 7 };
                if let (Some(rh), Some(rl), Some(gh), Some(gl), Some(bh), Some(bl)) = (
                    chars[i + 1].to_digit(16),
                    chars[i + 2].to_digit(16),
                    chars[i + 3].to_digit(16),
                    chars[i + 4].to_digit(16),
                    chars[i + 5].to_digit(16),
                    chars[i + 6].to_digit(16),
                ) {
                    let r = (rh * 16 + rl) as u8;
                    let g = (gh * 16 + gl) as u8;
                    let b = (bh * 16 + bl) as u8;
                    results.push((i, token_len, r, g, b));
                    i += token_len;
                    continue;
                }
            }
        }
        // #RGB (3-digit shorthand) — checked separately so short lines still match
        if chars[i] == '#' && i + 3 < len {
            let all_hex = chars[i + 1..i + 4].iter().all(|c| c.is_ascii_hexdigit());
            if all_hex && (i + 4 >= len || !chars[i + 4].is_ascii_hexdigit()) {
                if let (Some(r), Some(g), Some(b)) = (
                    chars[i + 1].to_digit(16),
                    chars[i + 2].to_digit(16),
                    chars[i + 3].to_digit(16),
                ) {
                    results.push((i, 4, (r * 17) as u8, (g * 17) as u8, (b * 17) as u8));
                    i += 4;
                    continue;
                }
            }
        }
        // rgb(r, g, b) or rgba(r, g, b, a)
        if i + 4 < len && (line_substr_at(&chars, i, "rgb(") || line_substr_at(&chars, i, "rgba("))
        {
            let start = i;
            let prefix_len = if line_substr_at(&chars, i, "rgba(") {
                5
            } else {
                4
            };
            if let Some((end, r, g, b)) = parse_rgb_values(&chars, i + prefix_len) {
                let token_len = end - start;
                results.push((start, token_len, r, g, b));
                i = end;
                continue;
            }
        }
        // hsl(h, s%, l%)
        if i + 4 < len && (line_substr_at(&chars, i, "hsl(") || line_substr_at(&chars, i, "hsla("))
        {
            let start = i;
            let prefix_len = if line_substr_at(&chars, i, "hsla(") {
                5
            } else {
                4
            };
            if let Some((end, r, g, b)) = parse_hsl_values(&chars, i + prefix_len) {
                let token_len = end - start;
                results.push((start, token_len, r, g, b));
                i = end;
                continue;
            }
        }
        i += 1;
    }
    results
}

#[cfg(test)]
fn line_substr_at(chars: &[char], pos: usize, pat: &str) -> bool {
    if pos + pat.len() > chars.len() {
        return false;
    }
    for (j, pc) in pat.bytes().enumerate() {
        if (chars[pos + j] as u32) > 127 || chars[pos + j].to_ascii_lowercase() as u8 != pc {
            return false;
        }
    }
    true
}

/// Parse "r, g, b)" or "r, g, b, a)" after the opening paren
#[cfg(test)]
fn parse_rgb_values(chars: &[char], start: usize) -> Option<(usize, u8, u8, u8)> {
    let len = chars.len();
    let mut i = start;
    let mut nums = [0u8; 4];
    let mut count = 0;
    while i < len && count < 4 {
        // skip whitespace
        while i < len && chars[i] == ' ' {
            i += 1;
        }
        if i >= len {
            return None;
        }
        if chars[i] == ')' {
            i += 1;
            break;
        }
        // parse number
        let num_start = i;
        while i < len && (chars[i].is_ascii_digit() || chars[i] == '.') {
            i += 1;
        }
        if i == num_start {
            return None;
        }
        let num_str: String = chars[num_start..i].iter().collect();
        // Handle percentage
        if i < len && chars[i] == '%' {
            i += 1;
            let pct: f32 = num_str.parse().ok()?;
            nums[count] = (pct * 2.55).round() as u8;
        } else {
            let val: f32 = num_str.parse().ok()?;
            nums[count] = val.min(255.0) as u8;
        }
        count += 1;
        // skip whitespace + comma or closing paren
        while i < len && chars[i] == ' ' {
            i += 1;
        }
        if i < len && chars[i] == ',' {
            i += 1;
        }
        if i < len && chars[i] == '/' {
            i += 1;
        } // CSS4 alpha separator
    }
    // Handle closing paren
    while i < len && chars[i] == ' ' {
        i += 1;
    }
    if i < len && chars[i] == ')' {
        i += 1;
    }
    if count >= 3 {
        Some((i, nums[0], nums[1], nums[2]))
    } else {
        None
    }
}

/// Parse "h, s%, l%)" or "h, s%, l%, a)" after the opening paren
#[cfg(test)]
fn parse_hsl_values(chars: &[char], start: usize) -> Option<(usize, u8, u8, u8)> {
    let len = chars.len();
    let mut i = start;
    let mut vals = [0.0f32; 4];
    let mut count = 0;
    while i < len && count < 4 {
        while i < len && chars[i] == ' ' {
            i += 1;
        }
        if i >= len {
            return None;
        }
        if chars[i] == ')' {
            i += 1;
            break;
        }
        let num_start = i;
        while i < len && (chars[i].is_ascii_digit() || chars[i] == '.') {
            i += 1;
        }
        if i == num_start {
            return None;
        }
        let num_str: String = chars[num_start..i].iter().collect();
        let val: f32 = num_str.parse().ok()?;
        // skip deg/% suffix
        if i < len && chars[i] == '%' {
            i += 1;
        }
        if i + 2 < len && line_substr_at(chars, i, "deg") {
            i += 3;
        }
        vals[count] = val;
        count += 1;
        while i < len && chars[i] == ' ' {
            i += 1;
        }
        if i < len && chars[i] == ',' {
            i += 1;
        }
        if i < len && chars[i] == '/' {
            i += 1;
        }
    }
    while i < len && chars[i] == ' ' {
        i += 1;
    }
    if i < len && chars[i] == ')' {
        i += 1;
    }
    if count >= 3 {
        let (r, g, b) = hsl_to_rgb(vals[0], vals[1] / 100.0, vals[2] / 100.0);
        Some((i, r, g, b))
    } else {
        None
    }
}

pub(crate) fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let h = ((h % 360.0) + 360.0) % 360.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r1, g1, b1) = match h as u32 {
        0..=59 => (c, x, 0.0),
        60..=119 => (x, c, 0.0),
        120..=179 => (0.0, c, x),
        180..=239 => (0.0, x, c),
        240..=299 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    )
}

/// Colors for indent guides, cycling by nesting depth (subtle/muted variants of bracket colors)

fn apply_area_gradient(
    f: &mut Frame,
    area: Rect,
    target_bg: Color,
    gradient: crate::app_theme::ThemeGradientSpec,
) {
    #[cfg(test)]
    if REFERENCE_WORKBENCH_GRADIENT.get() {
        return apply_area_gradient_reference(f, area, target_bg, gradient);
    }
    // Share the editor's bounded exact-color tiles across workbench surfaces.
    // Resizing/theme changes select new tiles; overlays keep their own colors.
    crate::ui_editor_highlight::apply_editor_gradients(
        f, area, target_bg, target_bg, gradient,
        crate::app_theme::ThemeGradientSpec { start: Color::Reset, end: Color::Reset, angle_deg: 0.0 },
    );
}

#[cfg(test)]
fn apply_area_gradient_reference(
    f: &mut Frame,
    area: Rect,
    target_bg: Color,
    gradient: crate::app_theme::ThemeGradientSpec,
) {
    if area.width == 0 || area.height == 0 || gradient.start == gradient.end {
        return;
    }
    let buf = f.buffer_mut();
    let g = crate::app_theme::GradientPrecomputed::new(area.width, area.height, gradient.angle_deg);
    for y in area.y..area.y + area.height {
        let ly = y - area.y;
        for x in area.x..area.x + area.width {
            let t = g.t_at(x - area.x, ly);
            if let Some(cell) = buf.cell_mut((x, y)) {
                if cell.bg == target_bg {
                    cell.set_bg(crate::app_theme::lerp_color(
                        gradient.start,
                        gradient.end,
                        t,
                    ));
                }
            }
        }
    }
}

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    app.viewport_area = f.area();
    app.begin_copy_animation_frame(std::time::Instant::now());
    app.loading.begin_frame();
    crate::ui_logs::begin_frame(app);
    crate::ui_inline_blame::begin_frame(app);
    app.animation_frame_cache.begin_frame(
        app.rendering_animation_frame,
        app.animation_settings.any_enabled() || app.loading.active(),
    );
    app.editor_gradient_area = None;
    app.animations.begin_frame();
    let animation_view = crate::diff_scope_source::animation_view_id(app);
    app.animations.set_active_view(animation_view);
    app.chat.activity.begin_frame();
    app.acknowledge_visible_chat_activity();
    crate::ui_quick_picker::begin_frame(app);
    // Input geometry must never outlive the frame that produced it.  Widgets
    // below register hit maps using their exact rendered viewport.
    app.text_input_regions.clear();
    app.settings_scroll_areas.clear();
    app.settings_category_area = None;
    app.terminal_surfaces.clear();
    app.search_control_regions.clear();
    app.search_bar_area = None;
    crate::file_search::begin_frame(app);
    app.zen_exit_area = None;
    app.chat.area = None;
    app.chat.layout.placements.clear();
    // Cosmetic frames have no input or worker changes. Tab/session topology
    // was already reconciled by the semantic frame that captured the scene.
    if !app.rendering_animation_frame {
        app.prune_editor_chats();
    }
    app.chat.tab_area = None;
    app.chat.agent_tab_area = None;
    app.chat.context_usage_area = None;
    app.chat.hits.clear();

    // Expire old screencast keys (remove those older than 3 seconds)
    if app.screencast_mode {
        let now = std::time::Instant::now();
        app.screencast_keys
            .retain(|(_, t)| now.duration_since(*t).as_millis() < 3000);
    }

    // The folder browser is a full-workbench view. Avoid rendering the editor
    // and chrome underneath it: none of that content is visible or interactive
    // until the browser closes.
    if app.change_dir_active {
        ui_pickers::draw_change_dir(f, app);
        crate::ui_remote_connection::draw(f, app);
        crate::ui_side_notice::draw(f, app);
        app.logs.finish_frame(app.background.wake_handle());
        crate::ui_modal::draw_active(f, app);
        crate::ui_loading::finish_frame(app, std::time::Instant::now());
        return;
    }
    app.folder_browser_area = None;
    app.folder_browser_buttons.clear();

    // Top-level layout: menu bar + middle + status bar
    let menu_bar_height: u16 = u16::from(app.zen_mode || app.show_menu_bar);
    let status_height: u16 = u16::from(!app.zen_mode && app.show_status_bar);
    let top_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(menu_bar_height),
            Constraint::Min(u16::from(!app.zen_mode)),
            Constraint::Length(status_height),
        ])
        .split(f.area());

    let menu_bar_area = top_layout[0];
    let middle_area = top_layout[1];
    let status_area = top_layout[2];

    if !app.zen_mode && app.show_menu_bar {
        app.menu_bar_area = Some(menu_bar_area);
        draw_static_chrome(f, app, menu_bar_area, ChromeRegion::Menu);
    } else {
        app.menu_bar_area = None;
        if app.zen_mode {
            ui_chrome::draw_zen_exit_button(f, app, menu_bar_area);
        }
    }

    // Activity bar + main content
    // Four-character text labels, an active marker, and the sidebar separator.
    let activity_width: u16 = if app.zen_mode || !app.show_activity_bar {
        0
    } else {
        6
    };
    let activity_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(activity_width), Constraint::Min(1)])
        .split(middle_area);

    let activity_area = activity_layout[0];
    let main_area = crate::ui_performance_details::reserve_dock(app, activity_layout[1]);

    if !app.zen_mode && app.show_activity_bar {
        app.activity_bar_area = Some(activity_area);
        draw_static_chrome(f, app, activity_area, ChromeRegion::Activity);
        if activity_area.width > 0 {
            let sep_x = activity_area.x + activity_area.width.saturating_sub(1);
            let buf = f.buffer_mut();
            for y in activity_area.y..activity_area.y + activity_area.height {
                if let Some(cell) = buf.cell_mut((sep_x, y)) {
                    let border = app.colors.ui.activity_border.border(cell.bg);
                    cell.set_char(if border.is_some() { '│' } else { ' ' });
                    cell.set_fg(border.unwrap_or(cell.bg));
                }
            }
        }
    } else {
        app.activity_bar_area = None;
    }

    // Main layout: body + optional panel
    let panel_height = if app.show_panel {
        if app.panel_chrome.maximized { main_area.height.saturating_sub(2) } else { responsive_panel_height(main_area.height, app.panel_height) }
    } else {
        0
    };
    let panel_visible = panel_height > 0;
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(u16::from(panel_visible)),
            Constraint::Length(panel_height),
        ])
        .split(main_area);

    let body_area = outer[0];
    let (body_area, chat_area) = if app.chat.visible && app.chat.layout.dock_visible && !app.zen_mode
        && (body_area.width >= 90 || app.active_pane == crate::app::ActivePane::Chat) {
        let width = if body_area.width < 90 { body_area.width } else { app.chat.width.min(body_area.width.saturating_sub(36)).max(36) };
        let split = Layout::default().direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(width)]).split(body_area);
        (split[0], Some(split[1]))
    } else { (body_area, None) };
    let panel_separator = outer[1];
    let panel_area = outer[2];

    app.workbench_body_area = Some(body_area);
    let terminal_sidebar_available = !app.zen_mode
        && app.terminal_sidebar_revealed
        && app.has_terminal_in_dock(TerminalDock::Right)
        && body_area.width > 1;
    let (workbench_area, terminal_sidebar_area) = if terminal_sidebar_available {
        let side_width =
            responsive_right_dock_width(body_area.width, app.show_terminal_sidebar);
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(side_width)])
            .split(body_area);
        (body[0], Some(body[1]))
    } else {
        (body_area, None)
    };

    // Body: sidebar + editor section
    let sidebar_width = responsive_sidebar_width(workbench_area.width, app.explorer_width);
    if app.show_explorer && sidebar_width > 0 {
        let (sidebar_area, editor_area) = if app.sidebar_right {
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(1), Constraint::Length(sidebar_width)])
                .split(workbench_area);
            (body[1], body[0])
        } else {
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(sidebar_width), Constraint::Min(1)])
                .split(workbench_area);
            (body[0], body[1])
        };

        app.explorer_area = Some(sidebar_area);
        match app.sidebar_view {
            crate::app::SidebarView::Explorer => draw_static_chrome(f, app, sidebar_area, ChromeRegion::Explorer),
            crate::app::SidebarView::Search => ui_dialogs::draw_search_panel(f, app, sidebar_area),
            crate::app::SidebarView::SourceControl => {
                crate::ui_source_control::draw_source_control_sidebar(f, app, sidebar_area);
            }
            crate::app::SidebarView::Settings => {
                ui_settings::draw_settings_sidebar(f, app, sidebar_area);
            }
            crate::app::SidebarView::Extensions => {
                ui_helpers::draw_extensions_sidebar(f, app, sidebar_area);
            }
            crate::app::SidebarView::Outline => {
                draw_static_chrome(f, app, sidebar_area, ChromeRegion::Outline);
            }
        }
        crate::ui_loading::draw_sidebar(f, app, sidebar_area);
        app.editor_gradient_area = Some(editor_area);
        ui_chrome::draw_editor_section(f, app, editor_area);
        app.editor_gradient_area = None;
        apply_area_gradient(
            f,
            sidebar_area,
            app.colors.bg_sidebar,
            app.colors.gradients.bg_sidebar,
        );
        apply_area_gradient(
            f,
            editor_area,
            app.colors.bg_editor,
            app.colors.gradients.bg_editor,
        );

        // Reserve a seam outside scrollbar content. Preserve transparent
        // borders and resolve the sash color on each final gradient cell.
        let border_col = if app.sidebar_right {
            sidebar_area.x
        } else {
            sidebar_area.x + sidebar_area.width.saturating_sub(1)
        };
        let is_hovering_border = crate::mouse::explorer_resize_hit(
            sidebar_area,
            app.sidebar_right,
            app.mouse_col,
            app.mouse_row,
        );
        let separator_layer = if app.resizing_explorer || is_hovering_border {
            app.colors.ui.sash_hover_border
        } else {
            app.colors.ui.sidebar_border.or(app.colors.ui.border_sidebar
                .map(crate::theme_workbench::ColorLayer::opaque).unwrap_or_default())
        };
        let buf = f.buffer_mut();
        for y in sidebar_area.y..sidebar_area.y + sidebar_area.height {
            if let Some(cell) = buf.cell_mut((border_col, y)) {
                let separator_color = separator_layer.border(cell.bg);
                cell.set_char(if separator_color.is_some() { '│' } else { ' ' });
                cell.set_fg(separator_color.unwrap_or(cell.bg));
            }
        }
    } else {
        app.explorer_area = None;
        app.editor_gradient_area = Some(workbench_area);
        ui_chrome::draw_editor_section(f, app, workbench_area);
        app.editor_gradient_area = None;
        apply_area_gradient(
            f,
            workbench_area,
            app.colors.bg_editor,
            app.colors.gradients.bg_editor,
        );
    }

    if let Some(sidebar_area) = terminal_sidebar_area {
        app.terminal_sidebar_area = Some(sidebar_area);
        ui_panels::draw_terminal_sidebar(f, app, sidebar_area);
    } else {
        app.terminal_sidebar_area = None;
        app.terminal_sidebar_toggle_area = None;
        app.terminal_sidebar_plus_area = None;
        app.terminal_sidebar_tab_areas.clear();
        app.terminal_sidebar_close_areas.clear();
    }

    crate::ui_chat::draw_surfaces(f, app, chat_area);

    // Bottom panel (file statistics)
    if panel_visible {
        app.panel_area = Some(panel_area);
        ui_panels::draw_bottom_panel(f, app, panel_area);
        if panel_separator.height > 0 {
            let border_y = panel_separator.y;
            let is_hovering_panel_border = app.mouse_row >= border_y
                && app.mouse_row <= panel_area.y
                && app.mouse_col >= panel_area.x
                && app.mouse_col < panel_area.x + panel_area.width;
            let border = if app.resizing_panel || is_hovering_panel_border {
                app.colors.ui.sash_hover_border
            } else {
                app.colors.ui.panel_border
            };
            let buf = f.buffer_mut();
            for x in panel_area.x..panel_area.right() {
                if let Some(cell) = buf.cell_mut((x, border_y)) {
                    cell.set_bg(app.colors.ui.bg_panel);
                    let color = border.border(cell.bg);
                    cell.set_char(if color.is_some() { '─' } else { ' ' });
                    cell.set_fg(color.unwrap_or(cell.bg));
                }
            }
        }
    } else {
        crate::ui_panels::invalidate_layout(app);
    }

    if !app.zen_mode && app.show_status_bar {
        app.status_bar_area = Some(status_area);
        ui_panels::draw_status_bar(f, app, status_area);
    } else {
        app.status_bar_area = None;
        app.status_bar_items.clear();
    }

    crate::ui_performance_details::draw_docked(f, app);
    crate::ui_file_deletion::draw(f, app);

    if app.show_explorer && app.sidebar_view == crate::app::SidebarView::Search {
        crate::ui_search_controls::draw_tooltip(f, app, crate::ui_search_controls::SearchScope::Workspace);
    }

    crate::ui_extensions::draw_tooltip(f, app);
    crate::ui_explorer_find::draw_tooltip(f, app);
    ui_chrome::draw_navigation_tooltip(f, app);
    crate::ui_source_control::draw_action_tooltip(f, app);
    crate::ui_panels::draw_tooltip(f, app);

    crate::ui_inline_blame::draw(f, app);

    // Search bar overlay (rendered last, on top)
    ui_search_dialog::draw_file_search_bars(f, app);

    // Context menu
    if app.context_menu_active {
        ui_dialogs::draw_context_menu(f, app);
    }

    // Explorer context menu
    if app.explorer_context_active {
        ui_dialogs::draw_explorer_context_menu(f, app);
    }

    // Source control file context menu
    if app.sc_context_active {
        ui_dialogs::draw_source_control_context_menu(f, app);
    }

    if app.hunk_popup_active {
        ui_dialogs::draw_hunk_popup(f, app);
    }

    // Rename is now shown inline in the explorer (no popup needed)

    // Peek definition
    if app.peek_definition.is_some() {
        ui_dialogs::draw_peek_definition(f, app);
    }

    // Global search popup (hidden when sidebar search is active)
    if app.global_search_active
        && !(app.show_explorer && app.sidebar_view == crate::app::SidebarView::Search)
    {
        ui_search_dialog::draw_global_search(f, app);
    }

    // VTComplete popup
    if app.vtcomplete_active && !app.vtcomplete_suggestions.is_empty()
        && !crate::ui_lsp_completion::visible(app)
    {
        ui_search_dialog::draw_vtcomplete(f, app);
    }

    // LSP completion popup
    ui_search_dialog::draw_lsp_completion(f, app);

    // LSP hover tooltip
    ui_dialogs::draw_lsp_hover(f, app);

    // LSP signature help tooltip
    ui_dialogs::draw_lsp_signature_help(f, app);

    // LSP code action menu
    ui_dialogs::draw_lsp_code_actions(f, app);

    // Call hierarchy dialog
    ui_dialogs::draw_call_hierarchy(f, app);

    // Type hierarchy dialog
    ui_dialogs::draw_type_hierarchy(f, app);

    // LSP references popup
    ui_dialogs::draw_lsp_references(f, app);

    // Keyboard shortcuts reference
    if app.shortcuts_active {
        ui_dialogs::draw_shortcuts(f, app);
    }

    // Settings panel
    if app.settings_active {
        ui_settings::draw_settings(f, app);
    }

    // Tab switcher overlay (Ctrl+Tab)
    if app.tab_switcher_active {
        ui_pickers::draw_tab_switcher(f, app);
    }

    // Menu dropdown overlay (must be on top of everything)
    if app.menu_active && app.menu_open {
        ui_chrome::draw_menu_dropdown(f, app);
    }

    // Tab context menu overlay
    if app.tab_context_active {
        ui_chrome::draw_tab_context_menu(f, app);
    }

    // Screencast mode overlay (show recent key presses)
    if app.screencast_mode {
        ui_dialogs::draw_screencast_overlay(f, app);
    }

    // Extension overlays (floating panels from extensions)
    ui_dialogs::draw_extension_overlays(f, app);

    // Tab path tooltip (shows full path after hovering ~500ms)
    if app.tab_tooltip_tab.is_some_and(|index| {
        app.tabs.get(index).is_none_or(|tab| {
            tab.kind == crate::app::TabKind::CommitDetails || tab.editor.file_path.is_none()
        })
    }) {
        app.clear_tab_tooltip();
    }
    if !app.tab_context_active && !app.menu_active {
        if let Some(tab_idx) = app.tab_tooltip_tab {
            if let Some(start) = app.tab_tooltip_hover_start {
                if start.elapsed().as_millis() >= 500 && tab_idx < app.tabs.len() {
                    app.tab_tooltip_shown = true;
                    if let Some(ref path) = app.tabs[tab_idx].editor.file_path {
                        let path_str = path.display().to_string();
                        if let Some(tab_area) = app.tab_bar_area {
                            let hits = crate::ui_chrome_tabs::TabBarHits::new(app, tab_area);
                            let tx = hits.tabs.iter().find(|(index, _, _)| *index == tab_idx)
                                .map_or(tab_area.x, |(_, rect, _)| rect.x);
                            let anchor_y = tab_area.y + tab_area.height;
                            let line = ratatui::text::Line::raw(path_str);
                            crate::ui_widgets::Tooltip::hover(vec![line], &app.colors)
                                .max_width(80)
                                .render(f, tx, anchor_y);
                        }
                    }
                }
            }
        }
    }

    // Explorer hover tooltip (file size + modified date). Filesystem metadata
    // and line counting are worker-backed; rendering only consults snapshots.
    if app.show_explorer && app.sidebar_view == crate::app::SidebarView::Explorer
        && !app.explorer_context_active
    {
        if let Some((entry_path, entry_is_dir)) = app
            .hovered_explorer
            .and_then(|index| app.explorer.entries.get(index))
            .map(|entry| (entry.path.clone(), entry.is_dir))
        {
            let open_lines = (!entry_is_dir)
                .then(|| {
                    app.tabs
                        .iter()
                        .chain(app.groups.iter().flat_map(|group| group.tabs.iter()))
                        .find_map(|tab| {
                            (tab.editor.file_path.as_deref() == Some(entry_path.as_path()))
                                .then(|| tab.editor.rope.len_lines())
                        })
                })
                .flatten();
            if let Some(info) = app.explorer_hover_info_cache.get(&entry_path).cloned() {
                let size = info.size;
                let size_str = if size < 1024 {
                    format!("{size} B")
                } else if size < 1024 * 1024 {
                    format!("{:.1} KB", size as f64 / 1024.0)
                } else {
                    format!("{:.1} MB", size as f64 / (1024.0 * 1024.0))
                };
                let modified_str = info
                    .modified
                    .map(|t| {
                        let elapsed = t.elapsed().unwrap_or_default();
                        let secs = elapsed.as_secs();
                        if secs < 60 {
                            "just now".to_string()
                        } else if secs < 3600 {
                            format!("{} min ago", secs / 60)
                        } else if secs < 86400 {
                            format!("{} hours ago", secs / 3600)
                        } else {
                            format!("{} days ago", secs / 86400)
                        }
                    })
                    .unwrap_or_default();
                let line_count = open_lines
                    .or(info.line_count)
                    .or_else(|| app.explorer_line_count_cache.get(&entry_path).copied());
                let line_count_str = (!info.is_dir)
                    .then(|| line_count.map(|lines| format!("{lines} lines")))
                    .flatten();
                let tooltip_header = if info.is_dir {
                    format!(" 📁 {modified_str} ")
                } else if let Some(ref lc) = line_count_str {
                    format!(" {size_str}, {lc}, {modified_str} ")
                } else {
                    format!(" {size_str}, {modified_str} ")
                };
                let path_str = format!(" {} ", entry_path.display());

                let tooltip_height = EXPLORER_TOOLTIP_HEIGHT;
                let header_w = UnicodeWidthStr::width(tooltip_header.as_str());
                let path_w = UnicodeWidthStr::width(path_str.as_str());
                let max_width = header_w
                    .max(path_w)
                    .min((f.area().width as usize).saturating_sub(2))
                    + 1;
                let tooltip_width = max_width as u16;
                let tooltip_x =
                    (app.mouse_col + 2).min(f.area().width.saturating_sub(tooltip_width));
                let tooltip_y = app.mouse_row + 1;
                let tooltip_bg = app.colors.bg_sidebar;
                if tooltip_y + tooltip_height < f.area().height && tooltip_width > 2 {
                    // Styling a Paragraph does not erase characters in its
                    // unused cells. Clear the whole card, including padding.
                    f.render_widget(Clear, Rect::new(tooltip_x, tooltip_y, tooltip_width, tooltip_height));
                    // Path line (dim)
                    let path_area = Rect::new(tooltip_x, tooltip_y, tooltip_width, 1);
                    let path_text: String = path_str.chars().take(max_width).collect();
                    f.render_widget(
                        Paragraph::new(path_text)
                            .style(Style::default().fg(app.colors.fg_dim).bg(tooltip_bg)),
                        path_area,
                    );
                    // Info line
                    let header_area = Rect::new(tooltip_x, tooltip_y + 1, tooltip_width, 1);
                    let header_text: String = tooltip_header.chars().take(max_width).collect();
                    f.render_widget(
                        Paragraph::new(header_text)
                            .style(Style::default().fg(app.colors.fg_text).bg(tooltip_bg)),
                        header_area,
                    );
                }
            }
        }
    }

    // Drag drop zone overlay (tabs + explorer file drags)
    if app.drag_has_moved
        && (app.dragging_tab.is_some()
            || app.chat.layout.dragging.is_some()
            || app.dragging_terminal_tab.is_some()
            || app.dragging_explorer_path.is_some())
    {
        let drop_target_area = app
            .tab_drop_target_group
            .and_then(|group_idx| {
                if group_idx == app.active_group {
                    app.editor_area
                } else {
                    app.groups.get(group_idx).and_then(|g| g.editor_area)
                }
            })
            .or(app.editor_area);
        let dock_highlight = app.terminal_dock_drop_target.and_then(|dock| {
            let body = app.workbench_body_area?;
            match dock {
                TerminalDock::Right => {
                    let width = responsive_right_dock_width(body.width, true);
                    if width == 0 {
                        return None;
                    }
                    Some(Rect::new(
                        body.x.saturating_add(body.width.saturating_sub(width)),
                        body.y,
                        width,
                        body.height,
                    ))
                }
            }
        });
        let tab_highlight = if let (Some(zone), Some(ed_area)) =
            (app.tab_drop_zone, drop_target_area)
        {
            Some(match zone {
                crate::app::TabDropZone::Left => {
                    Rect::new(ed_area.x, ed_area.y, ed_area.width / 2, ed_area.height)
                }
                crate::app::TabDropZone::Right => {
                    let half = ed_area.width / 2;
                    Rect::new(
                        ed_area.x + half,
                        ed_area.y,
                        ed_area.width - half,
                        ed_area.height,
                    )
                }
                crate::app::TabDropZone::Top => {
                    Rect::new(ed_area.x, ed_area.y, ed_area.width, ed_area.height / 2)
                }
                crate::app::TabDropZone::Bottom => {
                    let half = ed_area.height / 2;
                    Rect::new(
                        ed_area.x,
                        ed_area.y + half,
                        ed_area.width,
                        ed_area.height - half,
                    )
                }
            })
        } else {
            None
        };
        if let Some(highlight_area) = dock_highlight.or(tab_highlight) {
            // Semi-transparent blue overlay
            for y in highlight_area.y..highlight_area.y + highlight_area.height {
                for x in highlight_area.x..highlight_area.x + highlight_area.width {
                    if let Some(cell) = f
                        .buffer_mut()
                        .cell_mut(ratatui::layout::Position::new(x, y))
                    {
                        cell.set_bg(app.colors.ui.bg_editor_drop);
                    }
                }
            }
        }

        let drag_name = if let Some(drag_idx) = app.dragging_tab {
            let source_group = app.dragging_tab_source_group.unwrap_or(app.active_group);
            if source_group == app.active_group {
                app.tabs
                    .get(drag_idx)
                    .map(|t| t.editor.display_name().into_owned())
            } else {
                app.groups
                    .get(source_group)
                    .and_then(|g| g.tabs.get(drag_idx))
                    .map(|t| t.editor.display_name().into_owned())
            }
        } else if let Some(id) = &app.chat.layout.dragging {
            app.chat.sessions.get(id).map(|session| session.title.clone())
        } else if let Some(terminal_index) = app.dragging_terminal_tab {
            app.terminals
                .get(terminal_index)
                .map(|terminal| terminal.name.clone())
        } else {
            app.dragging_explorer_path
                .as_ref()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
                .map(ToString::to_string)
        };
        if let Some(name) = drag_name {
            let badge_text = format!(" ⇄ {name} ");
            let max_width = f.area().width.saturating_sub(1) as usize;
            let clipped: String = {
                let mut w = 0;
                badge_text
                    .chars()
                    .take_while(|c| {
                        w += UnicodeWidthChar::width(*c).unwrap_or(0);
                        w <= max_width
                    })
                    .collect()
            };
            let badge_w = UnicodeWidthStr::width(&*clipped) as u16;
            if badge_w > 0 {
                let x = app
                    .mouse_col
                    .saturating_add(2)
                    .min(f.area().width.saturating_sub(badge_w));
                let y = app
                    .mouse_row
                    .saturating_sub(1)
                    .min(f.area().height.saturating_sub(1));
                f.render_widget(
                    Paragraph::new(clipped).style(
                        Style::default()
                            .fg(app.colors.fg_selected)
                            .bg(app.colors.fg_accent_blue),
                    ),
                    Rect::new(x, y, badge_w, 1),
                );
            }
        }
    }
    crate::ui_performance::draw(f, app);
    crate::ui_remote_connection::draw(f, app);
    crate::ui_side_notice::draw(f, app);
    app.logs.finish_frame(app.background.wake_handle());
    crate::ui_logs::draw_dropdown(f, app);
    crate::ui_status::draw_overlays(f, app);
    crate::ui_quick_picker::draw_active(f, app);
    crate::ui_desks::draw(f, app);
    crate::ui_vtcomplete_index::draw(f, app);
    crate::ui_modal::draw_active(f, app);
    crate::ui_loading::finish_frame(app, std::time::Instant::now());
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn zen_mode_has_a_visible_exit_button() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let mut app = App::test_new();
        app.show_menu_bar = false;
        app.tabs = vec![crate::app::Tab::new_file(crate::editor::Editor::test_with_text("original text\n"))];
        app.active_tab = 0;
        app.editor_mut().unwrap().cursor_col = 3;
        let before = (app.show_explorer, app.show_minimap, app.show_panel,
            app.show_tab_bar, app.show_status_bar, app.show_activity_bar);
        app.execute_palette_command("toggle_zen");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let displayed: String = terminal.backend().buffer().content.iter()
            .map(|cell| cell.symbol()).collect();
        assert!(displayed.contains("Exit Zen Mode"));
        let button = app.zen_exit_area.expect("persistent exit button");
        assert_eq!(button.right(), 80);
        assert_eq!(button.y, 0);
        assert!(app.editor_area.unwrap().y >= button.bottom());
        assert!(app.menu_bar_area.is_none());

        let mouse = |kind, column, row| MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE };
        crate::mouse::handle_mouse(&mut app, mouse(MouseEventKind::Moved, button.x + 2, button.y));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let hovered = terminal.backend().buffer()[(button.x + 2, button.y)].clone();
        assert_eq!(hovered.bg, app.colors.ui.secondary_button_hover.over(app.colors.bg_editor).unwrap());
        let code = app.code_area.unwrap();
        app.mouse_hover_changed = false;
        crate::mouse::handle_mouse(&mut app, mouse(MouseEventKind::Moved, code.x, code.y));
        assert!(app.mouse_hover_changed, "leaving the exit button must repaint its hover state");
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let resting = &terminal.backend().buffer()[(button.x + 2, button.y)];
        assert_eq!(resting.bg, app.colors.ui.secondary_button.background.over(app.colors.bg_editor).unwrap());
        assert_ne!(resting.bg, hovered.bg);

        crate::mouse::handle_mouse(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), button.x + 2, button.y));
        assert!(!app.zen_mode);
        assert_eq!((app.show_explorer, app.show_minimap, app.show_panel,
            app.show_tab_bar, app.show_status_bar, app.show_activity_bar), before);
        assert_eq!(app.editor().unwrap().rope.to_string(), "original text\n");
        assert_eq!(app.editor().unwrap().cursor_col, 3);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(app.zen_exit_area.is_none());
        let displayed: String = terminal.backend().buffer().content.iter()
            .map(|cell| cell.symbol()).collect();
        assert!(!displayed.contains("Exit Zen Mode"));
    }

    #[test]
    fn zen_exit_button_survives_narrow_windows_and_terminal_views() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        for (width, height, label) in [(80, 24, "Exit Zen Mode"), (17, 8, "Exit Zen Mode"),
            (12, 6, "Exit Zen"), (8, 4, "Exit"), (4, 2, "Exit")]
        {
            let mut app = App::test_new();
            // Restored sessions can start directly in Zen mode with a terminal active.
            app.zen_mode = true;
            app.show_menu_bar = false;
            app.show_panel = false;
            app.show_explorer = false;
            let mut tab = crate::app::Tab::new_file(crate::editor::Editor::new());
            tab.kind = crate::app::TabKind::Terminal;
            tab.terminal_idx = Some(0);
            app.tabs = vec![tab];
            app.active_tab = 0;
            app.terminals[0].terminal_pty = Some(crate::terminal::TerminalEmulator::test_with_output(80, 24, "shell output\r\n"));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut app)).unwrap();
            let displayed: String = terminal.backend().buffer().content.iter()
                .map(|cell| cell.symbol()).collect();
            assert!(displayed.contains(label), "missing exit label at {width}x{height}");
            let button = app.zen_exit_area.unwrap();
            assert_eq!(button.right(), width);
            crate::mouse::handle_mouse(&mut app, MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left), column: button.x,
                row: button.y, modifiers: KeyModifiers::NONE,
            });
            assert!(!app.zen_mode);
        }
    }

    #[test]
    fn notifications_stay_in_history_without_covering_the_editor() {
        let mut app = App::test_new();
        app.show_panel = false;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let before = terminal.backend().buffer().clone();

        app.notify("Saving...");
        app.notify("File saved");
        app.notify_warn("Example warning");
        app.notify_error("Example error");
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        assert_eq!(terminal.backend().buffer(), &before);
        assert_eq!(app.notification_history.len(), 4);
        terminal.draw(|frame| {
            crate::ui_panels::draw_panel_notifications(frame, &app, frame.area());
        }).unwrap();
        let messages: String = terminal.backend().buffer().content.iter()
            .map(|cell| cell.symbol()).collect();
        for expected in ["Saving...", "File saved", "Example warning", "Example error"] {
            assert!(messages.contains(expected));
        }
    }

    #[test]
    fn responsive_workbench_sizes_preserve_editing_space() {
        assert_eq!(responsive_sidebar_width(114, 34), 34);
        assert_eq!(responsive_sidebar_width(74, 34), 30);
        assert_eq!(responsive_sidebar_width(42, 34), 14);
        assert_eq!(responsive_sidebar_width(1, 34), 0);

        assert_eq!(responsive_right_dock_width(74, true), 25);
        assert_eq!(responsive_right_dock_width(100, true), 34);
        assert_eq!(responsive_right_dock_width(200, true), 56);
        assert_eq!(responsive_right_dock_width(74, false), 3);

        assert_eq!(responsive_panel_height(22, 10), 10);
        assert_eq!(responsive_panel_height(10, 10), 4);
        assert_eq!(responsive_panel_height(6, 10), 0);
    }

    #[test]
    fn eighty_column_layout_keeps_a_comfortable_editor() {
        let mut app = App::test_new();
        app.show_panel = false;
        app.show_explorer = true;
        app.explorer_width = 34;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");

        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("responsive workbench draw");

        assert_eq!(app.explorer_width, 34, "the saved preference is preserved");
        assert_eq!(app.activity_bar_area.expect("text navigation").width, 6);
        assert_eq!(app.explorer_area.expect("sidebar").width, 30);
        assert!(app.editor_area.expect("editor").width >= COMFORTABLE_EDITOR_WIDTH);
    }

    #[test]
    fn tiny_height_temporarily_omits_an_unusable_panel() {
        let mut app = App::test_new();
        app.show_panel = true;
        app.panel_height = 10;
        let mut terminal = Terminal::new(TestBackend::new(60, 8)).expect("test terminal");

        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("tiny workbench draw");

        assert!(app.show_panel, "the user's preference is preserved");
        assert!(app.panel_area.is_none());
        assert!(app.workbench_body_area.is_some_and(|area| area.height >= 4));
    }

    #[test]
    fn source_control_scrollbar_survives_workbench_separator() {
        let mut app = App::test_new();
        app.sidebar_view = crate::app::SidebarView::SourceControl;
        app.source_control_repository_state =
            crate::app::SourceControlRepositoryState::Available;
        app.explorer_width = 30;
        app.show_panel = false;
        app.sc_graph_collapsed = true;
        app.source_control_files = (0..50)
            .map(|index| crate::app::GitFileChange {
                original_path: None,
                path: format!("src/file_{index}.rs"),
                status: 'M',
                staged: false,
            })
            .collect();
        app.source_control_scroll = 4;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");

        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("workbench draw");

        let sidebar = app.explorer_area.expect("source control sidebar");
        let seam_x = sidebar.right() - 1;
        let scrollbar_x = seam_x - 1;
        let buffer = terminal.backend().buffer();
        let thumb = app.colors.ui.scrollbar_colors(app.colors.bg_sidebar, false, false).1;
        assert!((sidebar.top()..sidebar.bottom()).any(|y| {
            buffer
                .cell((scrollbar_x, y))
                .is_some_and(|cell| {
                    cell.symbol() == "▐" && cell.fg == thumb
                })
        }));
        assert!((sidebar.top()..sidebar.bottom()).all(|y| {
            buffer
                .cell((seam_x, y))
                .is_some_and(|cell| cell.symbol() == if app.colors.ui.border_sidebar.is_some() { "│" } else { " " })
        }));
    }

    #[test]
    fn search_scrollbar_survives_workbench_separator() {
        let mut app = App::test_new();
        app.sidebar_view = crate::app::SidebarView::Search;
        app.explorer_width = 30;
        app.show_panel = false;
        app.global_search_query = "match".into();
        app.global_search_results = (0..50)
            .map(|index| crate::app::GlobalSearchResult {
                file_path: std::path::PathBuf::from("src/main.rs"),
                line_num: index + 1,
                line_text: format!("match {index}").into(),
                col_start: 0,
                col_end: 5,
                context_before: None,
                context_after: None,
            })
            .collect();
        app.global_search_scroll = 4;
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("test terminal");

        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("workbench draw");

        let sidebar = app.explorer_area.expect("search sidebar");
        let seam_x = sidebar.right() - 1;
        let scrollbar_x = seam_x - 1;
        let results_y = crate::ui_search_panel::search_results_start_y(&app, sidebar);
        let buffer = terminal.backend().buffer();
        assert!((results_y..sidebar.bottom()).all(|y| {
            buffer
                .cell((scrollbar_x, y))
                .is_some_and(|cell| cell.symbol() == "▐")
        }));
        assert!((sidebar.top()..sidebar.bottom()).all(|y| {
            buffer
                .cell((seam_x, y))
                .is_some_and(|cell| cell.symbol() == if app.colors.ui.border_sidebar.is_some() { "│" } else { " " })
        }));
    }

    #[test]
    fn collapsed_explorer_badge_survives_workbench_separator() {
        let mut app = App::test_new();
        app.show_open_editors = false;
        app.show_panel = false;
        app.explorer_width = 16;
        app.explorer.entries = vec![crate::explorer::FileEntry {
            path: app.explorer.root.join("src"),
            name: "src".to_string(),
            is_dir: true,
            depth: 0,
            expanded: false,
            git_status: crate::explorer::GitStatus::Unmodified,
            git_status_summary: crate::explorer::GitStatusSummary::default(),
        }];
        app.explorer
            .apply_git_status_changes(None, [("src/app.rs", 'M')]);
        app.set_explorer_single_selection(0);

        let mut terminal = Terminal::new(TestBackend::new(60, 16)).expect("test terminal");
        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("workbench draw");

        let buffer = terminal.backend().buffer();
        let sidebar_x = 6;
        let sidebar_end = sidebar_x + app.explorer_width;
        let src_row = (0..16)
            .map(|y| {
                (sidebar_x..sidebar_end)
                    .filter_map(|x| buffer.cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            })
            .find(|row| row.contains("src"))
            .expect("src row");
        assert!(src_row.contains('M'), "collapsed src row: {src_row:?}");
    }

    #[test]
    fn right_terminal_dock_collapses_to_a_three_cell_tab() {
        let mut app = App::test_new();
        app.show_explorer = false;
        app.terminal_sidebar_revealed = true;
        app.show_terminal_sidebar = true;
        app.terminals[0].terminal_launch_error = Some("test".into());
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).expect("test terminal");

        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("expanded terminal draw");
        let expanded = app.terminal_sidebar_area.expect("expanded right dock");
        assert!(expanded.width > 3);
        assert_eq!(expanded.x + expanded.width, 100);

        app.toggle_terminal_sidebar();
        terminal
            .draw(|frame| draw(frame, &mut app))
            .expect("collapsed terminal draw");
        let collapsed = app.terminal_sidebar_area.expect("collapsed right tab");
        assert_eq!(collapsed.width, 3);
        assert_eq!(collapsed.x + collapsed.width, 100);
    }

    #[test]
    fn hsl_pure_red() {
        let (r, g, b) = hsl_to_rgb(0.0, 1.0, 0.5);
        assert_eq!(r, 255);
        assert_eq!(g, 0);
        assert_eq!(b, 0);
    }

    #[test]
    fn hsl_pure_green() {
        let (r, g, b) = hsl_to_rgb(120.0, 1.0, 0.5);
        assert_eq!(r, 0);
        assert_eq!(g, 255);
        assert_eq!(b, 0);
    }

    #[test]
    fn hsl_black() {
        let (r, g, b) = hsl_to_rgb(0.0, 0.0, 0.0);
        assert_eq!(r, 0);
        assert_eq!(g, 0);
        assert_eq!(b, 0);
    }

    #[test]
    fn hsl_white() {
        let (r, g, b) = hsl_to_rgb(0.0, 0.0, 1.0);
        assert_eq!(r, 255);
        assert_eq!(g, 255);
        assert_eq!(b, 255);
    }

    #[test]
    fn find_hex_colors_hex6() {
        let result = find_hex_colors("#FF0000");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], (0, 7, 255, 0, 0));
    }

    #[test]
    fn find_hex_colors_hex3() {
        let result = find_hex_colors("#F00");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], (0, 4, 255, 0, 0));
    }

    #[test]
    fn find_hex_colors_rgb() {
        let result = find_hex_colors("rgb(255, 128, 0)");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].2, 255);
        assert_eq!(result[0].3, 128);
        assert_eq!(result[0].4, 0);
    }

    #[test]
    fn find_hex_colors_hsl() {
        let result = find_hex_colors("hsl(0, 100%, 50%)");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].2, 255);
        assert_eq!(result[0].3, 0);
        assert_eq!(result[0].4, 0);
    }

    #[test]
    fn find_hex_colors_none() {
        let result = find_hex_colors("no colors here");
        assert!(result.is_empty());
    }

    #[test]
    fn find_hex_colors_rgba() {
        let result = find_hex_colors("rgba(100, 200, 50, 0.5)");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].2, 100);
        assert_eq!(result[0].3, 200);
        assert_eq!(result[0].4, 50);
    }

    #[test]
    fn find_hex_colors_rgb_percent() {
        let result = find_hex_colors("rgb(100%, 0%, 50%)");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].2, 255);
        assert_eq!(result[0].3, 0);
    }

    #[test]
    fn find_hex_colors_hsla() {
        let result = find_hex_colors("hsla(120, 100%, 50%, 0.8)");
        assert_eq!(result.len(), 1);
        // HSL(120, 100%, 50%) = pure green
        assert_eq!(result[0].2, 0);
        assert_eq!(result[0].3, 255);
        assert_eq!(result[0].4, 0);
    }
}
