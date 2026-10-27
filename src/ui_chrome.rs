use rustc_hash::FxHashSet;
use std::borrow::Cow;
use std::fmt::Write as FmtWrite;
use std::path::PathBuf;

use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::App;
use crate::explorer::{GitStatus, GitStatusSummary};
use crate::text_input::TextInputId;
use crate::ui_dialogs::file_icon_colored;
use crate::ui_widgets::{DropdownItem, DropdownMenu, InputField, themed_dropdown_colors};
use crate::ui_editor::draw_editor;
use crate::ui_helpers::{
    draw_inline_diff, draw_side_by_side_diff,
    draw_split_editor_for_group, draw_terminal_in_tab,
};

#[cfg(test)]
thread_local! {
    static REFERENCE_EXPLORER_STYLES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn reuse_explorer_styles() -> bool {
    #[cfg(test)]
    { return !REFERENCE_EXPLORER_STYLES.get(); }
    #[cfg(not(test))]
    { true }
}

#[path = "ui_explorer_scroll.rs"]
mod explorer_scroll;
pub(crate) use explorer_scroll::scroll_horizontal_at as scroll_explorer_horizontal_at;

#[cfg(test)]
#[path = "ui_explorer_horizontal_tests.rs"]
mod explorer_horizontal_tests;

/// Shared section geometry for rendering and all Explorer pointer actions.
/// Each section includes its header; reserve half the available entry rows
/// for the file tree when the open-editor list overflows.
pub(crate) fn explorer_sections(app: &App, area: Rect) -> (Rect, Rect) {
    let inner_y = area.y.saturating_add(area.height.min(1));
    let inner_height = area.height.saturating_sub(1);
    let entries = if app.show_open_editors {
        app.tabs.len().min(inner_height.saturating_sub(2).div_ceil(2) as usize)
    } else {
        0
    };
    let open_height = (entries as u16 + 1).min(inner_height);
    (
        Rect::new(area.x, inner_y, area.width, open_height),
        Rect::new(area.x, inner_y + open_height, area.width, inner_height - open_height),
    )
}

fn explorer_section_area(app: &App, area: Rect) -> Rect {
    // The workbench paints the resize seam after the Explorer. Keep both
    // lists and their scrollbars inside that seam on either sidebar side.
    Rect::new(
        area.x.saturating_add(u16::from(app.sidebar_right).min(area.width)),
        area.y,
        area.width.saturating_sub(1),
        area.height,
    )
}

pub(crate) fn explorer_find_area(app: &App, area: Rect) -> Rect {
    let (_, tree) = explorer_sections(app, area);
    let tree = explorer_section_area(app, tree);
    Rect::new(tree.x, tree.y.saturating_add(tree.height.min(1)), tree.width,
        if app.explorer.find.open {
            tree.height.saturating_sub(1).min(5 + u16::from(app.explorer.find.results.error.is_some()))
        } else { 0 })
}

fn explorer_tree_raw_area(app: &App, area: Rect) -> Rect {
    let (_, tree) = explorer_sections(app, area);
    let tree = explorer_section_area(app, tree);
    let headers = (1 + explorer_find_area(app, area).height).min(tree.height);
    Rect::new(tree.x, tree.y.saturating_add(headers), tree.width, tree.height - headers)
}

pub(crate) fn explorer_tree_content_area(app: &App, area: Rect) -> Rect {
    explorer_scroll::tree_layout(app, area).content
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExplorerSection {
    OpenEditors,
    Files,
    FilesHorizontal,
}

impl ExplorerSection {
    pub(crate) fn scroll_to(self, app: &mut App, offset: usize) {
        match self {
            Self::OpenEditors => {
                app.open_editors_scroll_offset = offset;
                app.open_editors_scroll_anchor = open_editors_anchor(app);
            }
            Self::Files => app.explorer.scroll_offset = offset,
            Self::FilesHorizontal => app.explorer.horizontal.offset = offset,
        }
    }
}

pub(crate) struct ExplorerScrollbar {
    pub section: ExplorerSection,
    pub area: Rect,
    pub offset: usize,
    pub thumb_top: u16,
    pub thumb_height: u16,
    max_scroll: usize,
}

impl ExplorerScrollbar {
    pub(crate) fn coordinate(&self, col: u16, row: u16) -> u16 {
        if self.section == ExplorerSection::FilesHorizontal { col } else { row }
    }

    fn length(&self) -> u16 { self.coordinate(self.area.width, self.area.height) }

    fn draw(&self, f: &mut Frame, app: &App) {
        if self.area.is_empty() { return; }
        let sidebar_bg = app.colors.bg_sidebar;
        let hovered = self.area.contains((app.mouse_col, app.mouse_row).into());
        let dragging = app.explorer_scrollbar_drag.is_some_and(|(section, _, _)| section == self.section);
        let (track_color, thumb_color) = app.colors.ui.scrollbar_colors(sidebar_bg, hovered, dragging);
        let buffer = f.buffer_mut();
        let horizontal = self.section == ExplorerSection::FilesHorizontal;
        for i in 0..self.length() {
            let in_thumb = (self.thumb_top..self.thumb_top + self.thumb_height).contains(&i);
            let color = if in_thumb { thumb_color } else { track_color };
            let pos = if horizontal { (self.area.x + i, self.area.y) } else { (self.area.x, self.area.y + i) };
            if let Some(cell) = buffer.cell_mut(pos) {
                cell.set_symbol(if horizontal { "▄" } else { "▐" }).set_style(Style::default().fg(color).bg(track_color));
            }
        }
    }

    pub(crate) fn offset_at(&self, row: u16, grab: u16) -> Option<usize> {
        let travel = self.length().saturating_sub(self.thumb_height);
        if travel == 0 { return None; }
        let origin = self.coordinate(self.area.x, self.area.y);
        let top = row.saturating_sub(origin.saturating_add(grab)).min(travel);
        Some((f64::from(top) / f64::from(travel) * self.max_scroll as f64).round() as usize)
    }

    pub(crate) fn drag_offset(&self, row: u16, start_row: u16, start_offset: usize) -> usize {
        let travel = self.length().saturating_sub(self.thumb_height);
        if travel == 0 { return start_offset.min(self.max_scroll); }
        // Preserve the exact starting offset: a cell can represent many files,
        // so remapping the rounded thumb position would jump on the first drag.
        let delta = (f64::from(row) - f64::from(start_row)) / f64::from(travel) * self.max_scroll as f64;
        (start_offset as f64 + delta).round().clamp(0.0, self.max_scroll as f64) as usize
    }
}

/// Use exactly the same track and thumb for painting, clicking, and dragging.
pub(crate) fn explorer_scrollbar(app: &App, area: Rect, section: ExplorerSection) -> Option<ExplorerScrollbar> {
    if section == ExplorerSection::FilesHorizontal {
        return explorer_scroll::tree_layout(app, area).horizontal;
    }
    let (open, _) = explorer_sections(app, area);
    let (content, count) = match section {
        ExplorerSection::OpenEditors if app.show_open_editors => {
            let open = explorer_section_area(app, open);
            (Rect::new(open.x, open.y + open.height.min(1), open.width, open.height.saturating_sub(1)), app.tabs.len())
        }
        ExplorerSection::OpenEditors => return None,
        ExplorerSection::Files => (explorer_tree_content_area(app, area), app.explorer.entries.len()),
        ExplorerSection::FilesHorizontal => unreachable!(),
    };
    let height = content.height;
    if height == 0 || content.width <= 5 || count <= height as usize {
        return None;
    }
    let thumb_height = ((f64::from(height) / count as f64) * f64::from(height)).max(1.0) as u16;
    let max_scroll = count - height as usize;
    let offset = match section {
        ExplorerSection::OpenEditors => open_editors_offset(app, height as usize),
        ExplorerSection::Files => app.explorer.viewport_offset(height as usize),
        ExplorerSection::FilesHorizontal => unreachable!(),
    };
    let thumb_top = (offset as f64 / max_scroll as f64 * f64::from(height - thumb_height)) as u16;
    Some(ExplorerScrollbar {
        section,
        area: Rect::new(content.right() - 1, content.y, 1, height),
        offset,
        thumb_top,
        thumb_height,
        max_scroll,
    })
}

pub(crate) fn open_editors_list_area(app: &App, area: Rect) -> Rect {
    let (open, _) = explorer_sections(app, area);
    let mut list = explorer_section_area(app, open);
    list.y = list.y.saturating_add(list.height.min(1));
    list.height = list.height.saturating_sub(1);
    if explorer_scrollbar(app, area, ExplorerSection::OpenEditors).is_some() {
        list.width = list.width.saturating_sub(1);
    }
    list
}

fn open_editors_anchor(app: &App) -> Option<(usize, u64)> {
    app.tabs.get(app.active_tab).map(|tab| (app.active_group, tab.editor.navigation_id))
}

pub(crate) fn open_editors_offset(app: &App, visible: usize) -> usize {
    if visible == 0 { return 0; }
    let mut offset = app.open_editors_scroll_offset;
    if app.open_editors_scroll_anchor.is_none()
        || app.open_editors_scroll_anchor != open_editors_anchor(app)
    {
        let active = app.active_tab.min(app.tabs.len().saturating_sub(1));
        if active < offset {
            offset = active;
        } else if active >= offset.saturating_add(visible) {
            offset = active + 1 - visible;
        }
    }
    offset.min(app.tabs.len().saturating_sub(visible))
}

pub(crate) fn scroll_open_editors(app: &mut App, area: Rect, row: u16, down: bool) -> bool {
    if app.sidebar_view != crate::app::SidebarView::Explorer || !app.show_open_editors {
        return false;
    }
    let (open, _) = explorer_sections(app, area);
    if row < open.y || row >= open.bottom() { return false; }
    let visible = open.height.saturating_sub(1) as usize;
    if visible == 0 { return true; }
    let offset = open_editors_offset(app, visible);
    let offset = if down {
        offset.saturating_add(app.scroll_speed).min(app.tabs.len().saturating_sub(visible))
    } else {
        offset.saturating_sub(app.scroll_speed)
    };
    ExplorerSection::OpenEditors.scroll_to(app, offset);
    true
}

#[cfg(test)]
#[path = "ui_explorer_performance_tests.rs"]
mod explorer_performance_tests;
#[cfg(test)]
#[path = "ui_explorer_open_editors_tests.rs"]
mod explorer_open_editors_tests;
/// Compact angular mark used in place of VS Code's icon-font logo.
const TITLE_BAR_LOGO: &str = "◈  ";
/// Width of the window close-button region (" ✕ ") in the title bar.
const TITLE_BAR_CLOSE_BTN_WIDTH: usize = 3;
/// Minimize, maximize, and close regions are each three cells wide.
const TITLE_BAR_WINDOW_CONTROLS_WIDTH: usize = TITLE_BAR_CLOSE_BTN_WIDTH * 3;
const TITLE_BAR_CHAT_LABEL: &str = " ◇ Agent Chat ";
const TITLE_BAR_CHAT_COMPACT_LABEL: &str = " ◇ ";
const TITLE_BAR_NAVIGATION_WIDTH: usize = 6;
pub(crate) const MENU_OVERFLOW_LABEL: &str = " … ";

fn agent_chat_button_label(area_width: u16) -> Option<&'static str> {
    // Keep the menu overflow and window controls usable even in narrow windows.
    let available = (area_width as usize).saturating_sub(
        UnicodeWidthStr::width(TITLE_BAR_LOGO)
            + TITLE_BAR_WINDOW_CONTROLS_WIDTH
            + UnicodeWidthStr::width(MENU_OVERFLOW_LABEL)
            + 1,
    );
    [TITLE_BAR_CHAT_LABEL, TITLE_BAR_CHAT_COMPACT_LABEL]
        .into_iter()
        .find(|label| UnicodeWidthStr::width(*label) <= available)
}

fn agent_chat_reserved_width(area_width: u16) -> usize {
    agent_chat_button_label(area_width).map_or(0, |label| UnicodeWidthStr::width(label) + 1)
}

fn navigation_reserved_width(area_width: u16) -> usize {
    let minimum = UnicodeWidthStr::width(TITLE_BAR_LOGO)
        + TITLE_BAR_WINDOW_CONTROLS_WIDTH + UnicodeWidthStr::width(TITLE_BAR_CHAT_LABEL) + 1
        + UnicodeWidthStr::width(MENU_OVERFLOW_LABEL) + TITLE_BAR_NAVIGATION_WIDTH;
    if area_width as usize >= minimum { TITLE_BAR_NAVIGATION_WIDTH } else { 0 }
}

pub(crate) fn agent_chat_button_area(area: Rect) -> Option<Rect> {
    if area.height == 0 { return None; }
    let width = UnicodeWidthStr::width(agent_chat_button_label(area.width)?) as u16;
    Some(Rect::new(
        area.right() - TITLE_BAR_WINDOW_CONTROLS_WIDTH as u16 - 1 - width,
        area.y,
        width,
        1,
    ))
}

pub(crate) fn visible_menu_label_count(area_width: u16) -> usize {
    let labels = App::menu_labels();
    let fixed = UnicodeWidthStr::width(TITLE_BAR_LOGO) + TITLE_BAR_WINDOW_CONTROLS_WIDTH
        + agent_chat_reserved_width(area_width) + navigation_reserved_width(area_width);
    let all_width = labels
        .iter()
        .map(|label| UnicodeWidthStr::width(*label) + 2)
        .sum::<usize>();
    if fixed + all_width <= area_width as usize {
        return labels.len();
    }

    let available = (area_width as usize)
        .saturating_sub(fixed + UnicodeWidthStr::width(MENU_OVERFLOW_LABEL));
    let mut used = 0usize;
    labels
        .iter()
        .take_while(|label| {
            let width = UnicodeWidthStr::width(**label) + 2;
            if used + width > available {
                false
            } else {
                used += width;
                true
            }
        })
        .count()
}

pub(crate) fn menu_overflow_visible(area_width: u16) -> bool {
    let fixed = UnicodeWidthStr::width(TITLE_BAR_LOGO) + TITLE_BAR_WINDOW_CONTROLS_WIDTH
        + agent_chat_reserved_width(area_width) + navigation_reserved_width(area_width);
    visible_menu_label_count(area_width) < App::menu_labels().len()
        && area_width as usize >= fixed + UnicodeWidthStr::width(MENU_OVERFLOW_LABEL)
}

struct TitleBarCenterLayout {
    area: Rect,
    navigation: Option<[Rect; 2]>,
    workspace: Option<Rect>,
}

fn workspace_name(app: &App) -> &str {
    app.explorer.root.file_name().and_then(|name| name.to_str()).unwrap_or("VTCode")
}

fn title_bar_center_layout(app: &App, area: Rect) -> TitleBarCenterLayout {
    let used = UnicodeWidthStr::width(TITLE_BAR_LOGO)
        + App::menu_labels().iter().take(visible_menu_label_count(area.width))
            .map(|label| UnicodeWidthStr::width(*label) + 2).sum::<usize>()
        + usize::from(menu_overflow_visible(area.width)) * UnicodeWidthStr::width(MENU_OVERFLOW_LABEL);
    let remaining = (area.width as usize).saturating_sub(
        used + TITLE_BAR_WINDOW_CONTROLS_WIDTH + agent_chat_reserved_width(area.width));
    let center = Rect::new(area.x.saturating_add(used as u16), area.y, remaining as u16, area.height.min(1));
    let nav_width = navigation_reserved_width(area.width);
    let workspace_width = UnicodeWidthStr::width(workspace_name(app)) + 2;
    let show_workspace = nav_width + workspace_width <= remaining && area.height > 0;
    let content_width = nav_width + if show_workspace { workspace_width } else { 0 };
    let x = center.x + remaining.saturating_sub(content_width) as u16 / 2;
    TitleBarCenterLayout {
        area: center,
        navigation: (nav_width > 0 && area.height > 0).then(|| [
            Rect::new(x, area.y, 3, 1), Rect::new(x + 3, area.y, 3, 1),
        ]),
        workspace: show_workspace.then(|| Rect::new(x + nav_width as u16, area.y, workspace_width as u16, 1)),
    }
}

pub(crate) fn navigation_button_at(app: &App, col: u16, row: u16) -> Option<(Rect, bool)> {
    let buttons = title_bar_center_layout(app, app.menu_bar_area?).navigation?;
    buttons.into_iter().enumerate()
        .find(|(_, area)| area.contains((col, row).into()))
        .map(|(index, area)| (area, index == 1))
}

pub(crate) fn draw_navigation_tooltip(f: &mut Frame, app: &App) {
    if app.palette.active || app.menu_active || app.change_dir_active
        || app.context_menu_active || app.explorer_context_active || app.sc_context_active
        || crate::ui_modal::active(app).is_some() { return; }
    let Some((area, forward)) = navigation_button_at(app, app.mouse_col, app.mouse_row) else { return; };
    let (label, command) = if forward { ("Go Forward", "navigate_forward") } else { ("Go Back", "navigate_back") };
    let shortcut = app.shortcut_for_command(command);
    let text = if shortcut.is_empty() { label.to_string() } else { format!("{label} ({shortcut})") };
    crate::ui_widgets::Tooltip::hover(vec![Line::raw(text)], &app.colors)
        .prefer_below().render(f, area.x, area.y);
}

#[cfg(test)]
#[path = "ui_navigation_tests.rs"]
mod navigation_tests;

pub(crate) fn menu_dropdown_anchor_x(app: &App) -> u16 {
    let labels = App::menu_labels();
    let bar_x = app.menu_bar_area.map_or(0, |bar| bar.x);
    let desired = bar_x
        + UnicodeWidthStr::width(TITLE_BAR_LOGO) as u16
        + labels
            .iter()
            .take(app.menu_selected)
            .map(|label| UnicodeWidthStr::width(*label) as u16 + 2)
            .sum::<u16>();
    app.menu_bar_area.map_or(desired, |bar| {
        desired.min(bar.right().saturating_sub(16).max(bar.x))
    })
}

#[derive(Clone, Copy)]
struct ActivityLabel([&'static str; 3]);

const ACTIVITY_LABEL_EXPLORER: ActivityLabel = ActivityLabel(["EXPL", "ORER", ""]);
const ACTIVITY_LABEL_SEARCH: ActivityLabel = ActivityLabel(["SEAR", "CH", ""]);
const ACTIVITY_LABEL_SOURCE_CONTROL: ActivityLabel = ActivityLabel(["SRC", "CTRL", ""]);
const ACTIVITY_LABEL_EXTENSIONS: ActivityLabel = ActivityLabel(["", "EXTS", ""]);
const ACTIVITY_LABEL_OUTLINE: ActivityLabel = ActivityLabel(["OUTL", "INE", ""]);
const ACTIVITY_LABEL_SETTINGS: ActivityLabel = ActivityLabel(["SETT", "INGS", ""]);

fn git_status_color(status: GitStatus, colors: &crate::app_theme::ThemeColors, surface: Color) -> Color {
    let badge = match status {
        GitStatus::Modified => 'M', GitStatus::Renamed => 'R', GitStatus::Added => 'A',
        GitStatus::Deleted => 'D', GitStatus::Untracked => 'U', GitStatus::Ignored => 'I',
        GitStatus::Unmodified => return colors.ui.sidebar_text_color(surface),
    };
    colors.ui.git_status_color(badge, surface)
}

fn folder_icon_color(status: GitStatus, colors: &crate::app_theme::ThemeColors, surface: Color) -> Color {
    match status {
        GitStatus::Unmodified => colors.ui.folder_icon_color(surface),
        _ => git_status_color(status, colors, surface),
    }
}

fn git_status_badge(
    status: GitStatus,
    colors: &crate::app_theme::ThemeColors,
    surface: Color,
) -> Option<(&'static str, Color)> {
    match status {
        GitStatus::Modified => Some(("M", git_status_color(status, colors, surface))),
        GitStatus::Renamed => Some(("R", git_status_color(status, colors, surface))),
        GitStatus::Added => Some(("A", git_status_color(status, colors, surface))),
        GitStatus::Untracked => Some(("U", git_status_color(status, colors, surface))),
        GitStatus::Deleted => Some(("D", git_status_color(status, colors, surface))),
        GitStatus::Unmodified | GitStatus::Ignored => None,
    }
}

fn git_status_badge_spans(
    summary: GitStatusSummary,
    visibility: crate::explorer::GitBadgeVisibility,
    colors: &crate::app_theme::ThemeColors,
    bg: Color,
) -> Vec<Span<'static>> {
    let mut badges = Vec::new();
    for status in summary.statuses().filter(|status| visibility.shows(*status)) {
        if let Some((badge, badge_color)) = git_status_badge(status, colors, bg) {
            if badges.is_empty() {
                badges.reserve(9);
            } else {
                badges.push(Span::styled(" ", Style::default().bg(bg)));
            }
            badges.push(Span::styled(
                badge,
                Style::default().fg(badge_color).bg(bg),
            ));
        }
    }
    badges
}

fn split_constraints(child_count: usize, ratios: &[f32]) -> Vec<Constraint> {
    if child_count == 0 {
        return Vec::new();
    }
    let mut constraints = Vec::with_capacity(child_count.saturating_mul(2).saturating_sub(1));
    let fallback = 1.0 / child_count as f32;
    for index in 0..child_count {
        if index > 0 {
            constraints.push(Constraint::Length(1));
        }
        let ratio = ratios
            .get(index)
            .copied()
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(fallback);
        let weight = (ratio * 1_000.0).round().clamp(1.0, f32::from(u16::MAX)) as u16;
        constraints.push(Constraint::Fill(weight));
    }
    constraints
}

/// Query the membership index only for visible rows. The projection keeps
/// borrowed paths and reuses the index while the selection is unchanged.
fn visible_explorer_selection<'a>(
    app: &'a App,
    rows: &[usize],
) -> Option<rustc_hash::FxHashMap<&'a PathBuf, bool>> {
    if app.explorer_multi_selected.len() <= 32 || rows.is_empty() {
        return None;
    }
    Some(rows.iter().map(|&index| {
        let path = &app.explorer.entries[index].path;
        (path, app.explorer_multi_selected.contains(path))
    }).collect())
}

pub(crate) fn draw_explorer(f: &mut Frame, app: &mut App, area: Rect) {
    draw_explorer_impl(f, app, area, true);
}

fn draw_explorer_impl(f: &mut Frame, app: &mut App, area: Rect, indexed_selection: bool) {
    app.explorer.visible_height = 0;
    if area.is_empty() { return; }
    let sidebar_bg = app.colors.bg_sidebar;
    let explorer_focused = app.active_pane == crate::app::ActivePane::Explorer
        && app.sidebar_view == crate::app::SidebarView::Explorer;

    // VS Code uses borderless flat style for the sidebar
    f.render_widget(Paragraph::new("").style(Style::default().bg(sidebar_bg)), area);

    // Header: "EXPLORER" with collapse-all and refresh buttons
    let header_area = Rect::new(area.x, area.y, area.width, 1);
    let header_label = if app.remote_workspace.as_ref().is_some_and(|remote| !remote.client.connected()) {
        " Explorer · offline"
    } else if app.bg_filesystem_action_running {
        " Explorer · working…"
    } else if app.bg_explorer_refresh_running {
        if app.explorer.find.loading { " Explorer · searching…" } else { " Explorer · loading…" }
    } else if app.remote_workspace.is_some() && app.remote_explorer_error.is_some() {
        " Explorer · error"
    } else { " Explorer" };
    let header_label = crate::ui_pickers::truncate_to_width(header_label, area.width.saturating_sub(6) as usize);
    let header_label = header_label.as_str();
    let collapse_btn = " ⤒ ";
    let refresh_btn = " ↻ ";
    let header_label_w = UnicodeWidthStr::width(header_label);
    let collapse_w = UnicodeWidthStr::width(collapse_btn) as u16;
    let refresh_w = UnicodeWidthStr::width(refresh_btn) as u16;
    let fill_w = (area.width as usize)
        .saturating_sub(header_label_w + collapse_w as usize + refresh_w as usize);
    let collapse_x = area
        .x
        .saturating_add(header_label_w as u16)
        .saturating_add(fill_w as u16);
    let refresh_x = collapse_x.saturating_add(collapse_w);
    let hover_collapse = app.sidebar_view == crate::app::SidebarView::Explorer
        && app.mouse_row == header_area.y
        && app.mouse_col >= collapse_x
        && app.mouse_col < collapse_x.saturating_add(collapse_w);
    let hover_refresh = app.sidebar_view == crate::app::SidebarView::Explorer
        && app.mouse_row == header_area.y
        && app.mouse_col >= refresh_x
        && app.mouse_col < refresh_x.saturating_add(refresh_w);
    let button_style = |hovered: bool| app.colors.ui.toolbar_layer_style(
        app.colors.ui.sidebar_foreground, sidebar_bg, hovered, false, false,
    );
    let header_label_hovered = app.sidebar_view == crate::app::SidebarView::Explorer
        && app.mouse_row == header_area.y
        && app.mouse_col >= header_area.x
        && app.mouse_col < collapse_x;
    let header_label_style = app.colors.ui.toolbar_layer_style(
        app.colors.ui.sidebar_title_foreground, sidebar_bg, header_label_hovered, false, false,
    ).add_modifier(Modifier::BOLD);
    let header_label_bg = header_label_style.bg.unwrap_or(sidebar_bg);
    let header_line = Line::from(vec![
        Span::styled(
            header_label,
            header_label_style,
        ),
        Span::styled(crate::helpers::spaces(fill_w), Style::default().bg(header_label_bg)),
        Span::styled(collapse_btn, button_style(hover_collapse)),
        Span::styled(refresh_btn, button_style(hover_refresh)),
    ]);
    f.render_widget(Paragraph::new(header_line), header_area);

    // Split explorer into: Open Editors + File Tree
    let (oe_area, raw_tree_area) = explorer_sections(app, area);
    if oe_area.is_empty() { return; }
    let open_editors_count = oe_area.height.saturating_sub(1) as usize;
    let open_editors_offset = open_editors_offset(app, open_editors_count);
    if app.show_open_editors && open_editors_count > 0 {
        app.open_editors_scroll_offset = open_editors_offset;
        if app.open_editors_scroll_anchor != open_editors_anchor(app) {
            app.open_editors_scroll_anchor = None;
        }
    }

    // OPEN EDITORS section header
    let header_hovered = app.mouse_row == oe_area.y
        && app.mouse_col >= oe_area.x && app.mouse_col < oe_area.x + oe_area.width;
    let header_bg = app.colors.ui.bg_section;
    let oe_header_text = if app.show_open_editors && open_editors_count > 0 && app.tabs.len() > open_editors_count {
        Cow::Owned(format!("⌄ Open Editors {}–{}/{}", open_editors_offset + 1,
            open_editors_offset + open_editors_count, app.tabs.len()))
    } else {
        Cow::Borrowed(if app.show_open_editors { "⌄ Open Editors" } else { "› Open Editors" })
    };
    let oe_header = Line::from(Span::styled(
        oe_header_text,
        {
            let mut style = app.colors.ui.toolbar_layer_style(app.colors.ui.section_foreground, header_bg, header_hovered, false, false)
                .add_modifier(Modifier::BOLD);
            if let Some(border) = app.colors.ui.border_section {
                style = style.add_modifier(Modifier::UNDERLINED).underline_color(border);
            }
            style
        },
    ));
    f.render_widget(Paragraph::new(oe_header).style(Style::default().bg(header_bg)), Rect::new(oe_area.x, oe_area.y, oe_area.width, 1));

    // Open editor entries
    if app.show_open_editors && oe_area.height > 1 {
        let oe_list_area = open_editors_list_area(app, area);
        let mut oe_lines: Vec<Line> = Vec::with_capacity(open_editors_count);
        let mut oe_buf = String::with_capacity(64);
        for (i, tab) in app.tabs.iter().enumerate().skip(open_editors_offset).take(open_editors_count) {
            let name = match tab.kind {
                crate::app::TabKind::CommitDetails => Cow::Borrowed("Commit details"),
                crate::app::TabKind::File => tab.editor.display_name(),
                _ => Cow::Borrowed(tab.editor.file_name()),
            };
            let deleted_prefix = if tab.kind == crate::app::TabKind::File && tab.editor.disk_unavailable {
                "~"
            } else {
                ""
            };
            let (icon, icon_color) = if tab.kind == crate::app::TabKind::File {
                file_icon_colored(tab.editor.file_name())
            } else {
                (crate::ui_dialogs::tab_icon(tab.kind, tab.editor.file_name()), app.colors.ui.fg_sidebar)
            };
            let modified = if tab.has_unsaved_changes() { "● " } else { "  " };
            let is_active = i == app.active_tab;
            let close_row = oe_area.y + 1 + (i - open_editors_offset) as u16;
            let row_hovered = app.mouse_row == close_row
                && app.mouse_col >= oe_list_area.x
                && app.mouse_col < oe_list_area.right();
            let row_style = app.colors.ui.list_row_style(app.colors.ui.sidebar_foreground,
                sidebar_bg, is_active, explorer_focused, row_hovered);
            let bg = row_style.bg.unwrap_or(sidebar_bg);
            let icon_color = if tab.kind == crate::app::TabKind::File {
                icon_color
            } else {
                let color = app.colors.ui.list_icon_color(icon_color, bg, is_active, explorer_focused);
                app.colors.ui.readable_foreground(color, bg, if icon.is_ascii() { 4.5 } else { 3.0 })
            };
            let fg = row_style.fg.unwrap_or_else(|| app.colors.ui.sidebar_text_color(bg));
            let close_hovered = app.mouse_row == close_row
                && app.mouse_col >= oe_list_area.right().saturating_sub(3)
                && app.mouse_col < oe_list_area.right();
            let show_close = is_active || row_hovered;
            let close_bg = if close_hovered { app.colors.ui.toolbar_hover.over(bg).unwrap_or(bg) } else { bg };
            let close_fg = app.colors.ui.readable_foreground(fg, close_bg, 4.5);
            oe_buf.clear();
            let _ = write!(oe_buf, "{deleted_prefix}{name}"); // Error OK: write to String buffer is infallible in practice
            let controls_width = 2
                + UnicodeWidthStr::width(modified)
                + UnicodeWidthStr::width(icon)
                + 3;
            let name_width = (oe_list_area.width as usize).saturating_sub(controls_width);
            oe_buf = crate::ui_pickers::truncate_to_width(&oe_buf, name_width);
            let content_width = controls_width + UnicodeWidthStr::width(oe_buf.as_str());
            let fill = (oe_list_area.width as usize).saturating_sub(content_width);
            oe_lines.push(Line::from(vec![
                Span::styled("  ", Style::default().bg(bg)),
                Span::styled(modified, row_style.fg(app.colors.ui.readable_foreground(app.colors.fg_modified_indicator, bg, 3.0))),
                Span::styled(icon, Style::default().fg(icon_color).bg(bg)),
                Span::styled(std::mem::take(&mut oe_buf), row_style),
                Span::styled(crate::helpers::spaces(fill), row_style),
                Span::styled(
                    if show_close { " × " } else { "   " },
                    Style::default().fg(close_fg).bg(close_bg),
                ),
            ]));
        }
        if !oe_lines.is_empty() {
            f.render_widget(Paragraph::new(oe_lines).style(Style::default().bg(sidebar_bg)), oe_list_area);
        }
        if let Some(bar) = explorer_scrollbar(app, area, ExplorerSection::OpenEditors) {
            bar.draw(f, app);
        }
    }

    // File tree section. The workbench paints its sidebar/editor separator
    // after the Explorer, so the seam column is not drawable content. Keep
    // badges one cell inside it even when the collapsed tree has no scrollbar.
    let tree_area = explorer_section_area(app, raw_tree_area);
    if tree_area.is_empty() {
        return;
    }

    // Folder name header
    let folder_name = app.explorer.root.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Workspace");
    let ws_hovered = app.mouse_row == tree_area.y
        && app.mouse_col >= tree_area.x && app.mouse_col < tree_area.x + tree_area.width;
    let badge_visibility = app.explorer_git_badge_visibility();
    let root_git_summary = app.explorer.root_git_status_summary();
    let root_git_status = root_git_summary.primary_visible(badge_visibility);
    let mut ws_style = app.colors.ui.toolbar_layer_style(app.colors.ui.section_foreground,
        app.colors.ui.bg_section, ws_hovered, false, false).add_modifier(Modifier::BOLD);
    let ws_bg = ws_style.bg.unwrap_or(sidebar_bg);
    if root_git_status != GitStatus::Unmodified {
        ws_style = ws_style.fg(git_status_color(root_git_status, &app.colors, ws_bg));
    }
    let mut folder_title = String::with_capacity(folder_name.len() + 2);
    let _ = write!(folder_title, "⌄ {folder_name}");
    let root_badges = git_status_badge_spans(root_git_summary, badge_visibility, &app.colors, ws_bg);
    let badge_width = root_badges
        .iter()
        .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
        .sum::<usize>();
    let badge_gap = usize::from(!root_badges.is_empty());
    let title_room = (tree_area.width as usize).saturating_sub(badge_width + badge_gap);
    let folder_title = crate::ui_pickers::truncate_to_width(&folder_title, title_room);
    let title_width = UnicodeWidthStr::width(folder_title.as_str());
    let fill = (tree_area.width as usize)
        .saturating_sub(title_width.saturating_add(badge_width).saturating_add(badge_gap));
    let mut folder_header_spans = vec![Span::styled(
        folder_title,
        ws_style,
    )];
    folder_header_spans.push(Span::styled(crate::helpers::spaces(fill), Style::default().bg(ws_bg)));
    if !root_badges.is_empty() {
        folder_header_spans.push(Span::styled(" ", Style::default().bg(ws_bg)));
        folder_header_spans.extend(root_badges);
    }
    let folder_header = Line::from(folder_header_spans);
    f.render_widget(Paragraph::new(folder_header).style(Style::default().bg(ws_bg)), Rect::new(tree_area.x, tree_area.y, tree_area.width, 1));

    crate::ui_explorer_find::draw(f, app, area);
    let tree_layout = explorer_scroll::tree_layout(app, area);
    let tree_content = tree_layout.content;
    let file_area = tree_layout.files;
    app.explorer.horizontal.offset = app.explorer.horizontal.offset.min(tree_layout.max_scroll);
    app.explorer.set_viewport_height(tree_content.height as usize);
    if app.explorer.entries.is_empty() {
        if (app.bg_explorer_refresh_running || app.explorer.find.filtering()) && tree_content.height > 0
            && app.explorer.find.results.error.is_none() {
            f.render_widget(
                Paragraph::new(if app.explorer.find.loading { "  Searching…" }
                    else if app.bg_explorer_refresh_running { "  Loading…" }
                    else { "  No matches" }).style(
                    Style::default()
                        .fg(app.colors.ui.description_color(sidebar_bg))
                        .bg(sidebar_bg)
                        .add_modifier(Modifier::ITALIC),
                ),
                tree_content,
            );
        }
        return;
    }

    // Reserve 1 column for scrollbar if content overflows
    let scrollbar = explorer_scrollbar(app, area, ExplorerSection::Files);

    // Use explorer scroll_offset, ensured visible by input handlers
    let visible_height = file_area.height as usize;
    let offset = app.explorer.viewport_offset(visible_height);
    app.explorer.scroll_offset = offset;
    let rows = app.explorer.viewport_indices(visible_height);
    if app.rename.active && file_area.width > 0
        && let Some(entry) = rows.iter().map(|&index| &app.explorer.entries[index])
            .find(|entry| app.rename.target.as_deref() == Some(entry.path.as_path()))
    {
        // F2 must expose a usable field even when indentation alone exceeds
        // the viewport. The input widget handles scrolling inside its text.
        let prefix = entry.prefix_width();
        app.explorer.horizontal.offset = app.explorer.horizontal.offset
            .max(prefix.saturating_sub(usize::from(file_area.width / 2)))
            .min(prefix).min(tree_layout.max_scroll);
    }
    let horizontal_offset = app.explorer.horizontal.offset;
    let selected_paths = indexed_selection.then(|| visible_explorer_selection(app, &rows)).flatten();

    // Pre-compute set of modified file paths for O(1) lookup in explorer entries
    let modified_paths: FxHashSet<&PathBuf> = app.tabs.iter()
        .chain(app.groups.iter().flat_map(|g| g.tabs.iter()))
        .filter(|t| t.has_unsaved_changes())
        .filter_map(|t| t.editor.file_path.as_ref())
        .collect();

    // Identify the currently active file for highlighting in the tree
    let active_file_path: Option<&PathBuf> = app.tabs.get(app.active_tab)
        .and_then(|t| t.editor.file_path.as_ref());

    // Pre-build only enough indentation for the rendered slice. Scanning the
    // complete expanded tree here made every editor cursor frame proportional
    // to the workspace size.
    let max_depth = rows
        .iter()
        .map(|&i| app.explorer.entries[i].depth)
        .max()
        .unwrap_or(0);
    let mut indent_pool = String::with_capacity(1 + max_depth * 2);
    indent_pool.push(' ');
    for _ in 0..max_depth {
        indent_pool.push_str("  ");
    }

    let mut rename_input_area = None;
    // Visible rows share only a handful of states. Resolve alpha/contrast and
    // Git text colors once per state, with a frame-local cache so theme and
    // focus changes cannot reuse stale styles.
    let mut row_styles = rustc_hash::FxHashMap::default();
    let items: Vec<_> = rows
        .iter()
        .enumerate()
        .map(|(row, &i)| {
            let entry = &app.explorer.entries[i];
            let row_bg = if i < offset + row {
                app.colors.ui.sidebar_sticky_background.over(sidebar_bg).unwrap_or(sidebar_bg)
            } else {
                sidebar_bg
            };
            let indent_len = 1 + entry.depth * 2;
            let indent = &indent_pool[..indent_len.min(indent_pool.len())];
            let file_icon = (!entry.is_dir).then(|| file_icon_colored(&entry.name));
            let icon = if entry.is_dir {
                if app.explorer_expanding.contains(&entry.path) {
                    "◌ "
                } else if entry.expanded {
                    "▾ "
                } else {
                    "▸ "
                }
            } else {
                file_icon.unwrap().0
            };

            // Check if this file is open and modified
            let is_modified = !entry.is_dir && modified_paths.contains(&entry.path);
            let is_active_file = !entry.is_dir && active_file_path == Some(&entry.path);
            let is_selected = selected_paths.as_ref().map_or_else(
                || app.explorer_multi_selected.iter().any(|p| p == &entry.path),
                |selected| selected.get(&entry.path).copied().unwrap_or(false),
            )
                || (app.explorer_selection_anchor.is_some() && i == app.explorer.selected);

            let is_drop_target = app.dragging_explorer_path.is_some()
                && app.explorer_drop_target_idx == Some(i);

            let hovered = app.hovered_explorer == Some(i) && !app.explorer_context_active;
            let visible_git_status = if entry.is_dir {
                entry.git_status_summary.primary_visible(badge_visibility)
            } else {
                badge_visibility.color_status(entry.git_status)
            };
            let resolve_style = || {
                let mut style = app.colors.ui.list_row_style(app.colors.ui.sidebar_foreground,
                    row_bg, is_selected || is_active_file, explorer_focused, hovered);
                if is_drop_target {
                    let background = app.colors.ui.list_drop_background.over(row_bg).unwrap_or(row_bg);
                    style = Style::default().bg(background).fg(app.colors.ui.sidebar_text_color(background));
                }
                if !is_selected && !hovered {
                    style = style.fg(git_status_color(visible_git_status, &app.colors, style.bg.unwrap_or(row_bg)));
                }
                style
            };
            let style = if reuse_explorer_styles() {
                *row_styles.entry((row_bg, visible_git_status as u8, is_selected, is_active_file, hovered, is_drop_target))
                    .or_insert_with(resolve_style)
            } else { resolve_style() };
            let bg = style.bg.unwrap_or(row_bg);
            let icon_color = if visible_git_status == GitStatus::Ignored {
                // Keep ignored file icons quiet too; selection/hover retain
                // the row's readable foreground instead of a colored icon.
                style.fg.unwrap_or_else(|| git_status_color(GitStatus::Ignored, &app.colors, bg))
            } else if entry.is_dir {
                app.colors.ui.list_icon_color(folder_icon_color(visible_git_status, &app.colors, bg), bg,
                    is_selected || is_active_file, explorer_focused)
            } else {
                file_icon.unwrap().1
            };
            let icon_style = Style::default().fg(icon_color).bg(bg);
            // Show inline rename input for the selected entry
            let is_rename_target = app.rename.active
                && app.rename.target.as_deref() == Some(entry.path.as_path());

            let mut trailing = if entry.is_dir && !is_rename_target {
                git_status_badge_spans(entry.git_status_summary, badge_visibility, &app.colors, bg)
            } else {
                Vec::new()
            };
            let find_match = app.explorer.find.results.rows.get(&entry.path);
            if !is_rename_target && entry.is_dir && !app.explorer.find.filter
                && let Some(matched) = find_match.filter(|matched| matched.descendants > 0)
            {
                if !trailing.is_empty() { trailing.push(Span::styled(" ", style)); }
                trailing.push(Span::styled(matched.descendants.to_string(),
                    style.fg(app.colors.ui.list_match_color(bg, is_selected && explorer_focused))));
            }
            if !is_rename_target {
                if !entry.is_dir && badge_visibility.shows(entry.git_status) {
                    let status = entry.git_status;
                    if let Some((badge, badge_color)) = git_status_badge(status, &app.colors, bg) {
                        if !trailing.is_empty() {
                            trailing.push(Span::styled(" ", Style::default().bg(bg)));
                        }
                        trailing.push(Span::styled(
                            badge,
                            Style::default().fg(badge_color).bg(bg),
                        ));
                    }
                }
                if is_modified {
                    if !trailing.is_empty() {
                        trailing.push(Span::styled(" ", Style::default().bg(bg)));
                    }
                    trailing.push(Span::styled(
                        "●",
                        Style::default().fg(app.colors.ui.readable_foreground(app.colors.fg_modified_indicator, bg, 3.0)).bg(bg),
                    ));
                }
            }

            let prefix_width = UnicodeWidthStr::width(indent) + UnicodeWidthStr::width(icon);
            let trailing_width = trailing
                .iter()
                .map(|span| UnicodeWidthStr::width(span.content.as_ref()))
                .sum::<usize>();

            let mut spans = vec![
                Span::styled(indent, style),
                Span::styled(icon, icon_style),
            ];
            if is_rename_target {
                let input_start = prefix_width.saturating_sub(horizontal_offset).min(file_area.width.saturating_sub(1) as usize);
                let input_width = (file_area.width as usize).saturating_sub(input_start);
                let base_style = Style::default().fg(app.colors.ui.fg_sidebar).bg(app.colors.bg_dark);
                spans.push(Span::styled(crate::helpers::spaces(input_width), base_style));
                rename_input_area = Some((Rect::new(
                    file_area.x.saturating_add(input_start as u16),
                    file_area.y.saturating_add(row as u16),
                    input_width as u16,
                    1,
                ), bg));
            } else {
                if let Some(matched) = find_match.filter(|matched| !matched.indices.is_empty()) {
                    let highlight = style.fg(app.colors.ui.list_match_color(bg, is_selected && explorer_focused))
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
                    spans.extend(crate::ui_explorer_find::highlighted_name(
                        &entry.name, &matched.indices, style, highlight));
                } else {
                    spans.push(Span::styled(entry.name.as_str(), style));
                }
            }

            (Line::from(spans), style, trailing, trailing_width)
        })
        .collect();

    for (row, (line, style, trailing, trailing_width)) in items.into_iter().enumerate() {
        let row_area = Rect::new(file_area.x, file_area.y + row as u16, file_area.width, 1);
        f.buffer_mut().set_style(row_area, style);
        let badge_width = trailing_width.min(file_area.width as usize) as u16;
        let reserved = badge_width.saturating_add(u16::from(badge_width > 0)).min(file_area.width);
        f.render_widget(Paragraph::new(line).style(style).scroll((0, horizontal_offset as u16)),
            Rect::new(row_area.x, row_area.y, row_area.width - reserved, 1));
        if badge_width > 0 {
            // Git/find/unsaved indicators stay readable while the name scrolls.
            f.render_widget(Paragraph::new(Line::from(trailing)).style(style),
                Rect::new(row_area.right() - badge_width, row_area.y, badge_width, 1));
        }
    }

    if let Some((input_area, surface)) = rename_input_area {
        let cursor = if app.cursor_visible { "│" } else { "" };
        let hit_map = InputField::new(&app.rename.query)
            .theme_control(&app.colors, app.colors.ui.input_colors, surface)
            .prefix("", Color::Reset)
            .cursor_marker(cursor)
            .cursor_pos(app.rename.cursor)
            .selection_range(
                App::inline_selection_range(app.rename.cursor, app.rename.selection_anchor),
            )
            .render(f, input_area);
        app.register_text_input(TextInputId::ExplorerRename, hit_map);
    }

    // Draw scrollbar if content overflows
    if let Some(bar) = scrollbar {
        bar.draw(f, app);
    }
    if let Some(bar) = explorer_scroll::tree_layout(app, area).horizontal {
        bar.draw(f, app);
    }
}

/// Draw divider cells directly; a vertical sash needs no per-row widgets.
fn draw_divider(f: &mut Frame, area: Rect, direction: &Direction, style: Style) {
    let symbol = if *direction == Direction::Horizontal { "│" } else { "─" };
    let buffer = f.buffer_mut();
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            if let Some(cell) = buffer.cell_mut((x, y)) {
                cell.set_symbol(symbol).set_style(style);
            }
        }
    }
}

/// Render a group pane (tab bar + breadcrumb + editor) for the active group.
fn render_group_pane(f: &mut Frame, app: &mut App, area: Rect, group_id: usize, show_chrome: bool) {
    if group_id == app.active_group {
        let current_tab_kind = app.tabs.get(app.active_tab).map(|tab| tab.kind);
        let show_tabs = show_chrome && app.show_tab_bar;
        let show_breadcrumbs = show_chrome
            && app.show_breadcrumbs
            && matches!(
                current_tab_kind,
                Some(crate::app::TabKind::File | crate::app::TabKind::GitRevision | crate::app::TabKind::Diff(_))
            );
        let pane_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(u16::from(show_tabs)),
                Constraint::Length(u16::from(show_breadcrumbs)),
                Constraint::Min(1),
            ])
            .split(area);
        if show_tabs {
            app.tab_bar_area = Some(pane_layout[0]);
            crate::ui_chrome_tabs::draw_tab_bar(f, app, pane_layout[0]);
            // In split mode, highlight active group with a colored accent on the active tab
            if app.split_active() {
                let accent = app.colors.fg_accent_blue;
                let buf = f.buffer_mut();
                // Paint a colored underline effect on the first column of the tab bar
                if let Some(cell) = buf.cell_mut((pane_layout[0].x, pane_layout[0].y)) {
                    cell.set_fg(accent);
                    cell.set_symbol("▍");
                }
            }
        } else {
            app.tab_bar_area = None;
        }
        if show_breadcrumbs {
            crate::ui_chrome_tabs::draw_breadcrumb(f, app, pane_layout[1]);
        }
        crate::ui_loading::draw_editor_header(f, app, pane_layout[0], pane_layout[1]);
        app.editor_area = Some(area);
        if let Some(crate::app::TabKind::Chat(key)) = current_tab_kind {
            app.settings_editor_active = false;
            crate::ui_chat::queue_editor(app, key, group_id, pane_layout[2]);
        } else if current_tab_kind == Some(crate::app::TabKind::CommitDetails) {
            app.settings_editor_active = false;
            crate::ui_commit_details::draw_in_area(f, app, pane_layout[2]);
        } else if current_tab_kind == Some(crate::app::TabKind::Settings) {
            app.settings_editor_active = true;
            crate::ui_settings::draw_settings_editor(f, app, pane_layout[2]);
        } else if current_tab_kind == Some(crate::app::TabKind::KeyboardShortcuts) {
            app.settings_editor_active = false;
            crate::ui_keybindings::draw(f, app, pane_layout[2]);
        } else if current_tab_kind == Some(crate::app::TabKind::ExtensionInfo) {
            app.settings_editor_active = false;
            crate::ui_extension_info::draw_extension_info(f, app, pane_layout[2]);
        } else if current_tab_kind == Some(crate::app::TabKind::MarkdownPreview) {
            app.settings_editor_active = false;
            app.code_area = None;
            app.gutter_area = None;
            app.minimap_area = None;
            app.scrollbar_area = None;
            app.h_scrollbar_area = None;
            crate::ui_helpers::draw_markdown_preview(f, app, pane_layout[2]);
        } else if current_tab_kind == Some(crate::app::TabKind::Terminal) {
            app.settings_editor_active = false;
            app.code_area = None;
            app.gutter_area = None;
            app.minimap_area = None;
            app.scrollbar_area = None;
            app.h_scrollbar_area = None;
            draw_terminal_in_tab(f, app, pane_layout[2]);
        } else {
            app.settings_editor_active = false;
            if app.should_render_diff_for_active_editor() {
                app.refresh_compare_diff();
            }
            if app.diff_mode == crate::app::DiffMode::SideBySide
                && app.should_render_diff_for_active_editor()
            {
                draw_side_by_side_diff(f, app, pane_layout[2]);
            } else if app.diff_mode == crate::app::DiffMode::Inline
                && app.should_render_diff_for_active_editor()
            {
                app.editor_area = Some(pane_layout[2]);
                draw_inline_diff(f, app, pane_layout[2]);
            } else {
                draw_editor(f, app, pane_layout[2]);
            }
        }
    } else {
        app.groups[group_id].editor_area = Some(area);
        draw_split_editor_for_group(f, app, area, group_id);
    }
}

type SplitDivider = (Rect, Vec<usize>, usize, crate::app::SplitDirection, Rect);

/// Layout changes only with the split tree or available rectangle. Mouse moves,
/// edits and animation ticks reuse pane rectangles and divider paths in place.
#[derive(Default)]
pub(crate) struct SplitPaneLayout {
    tree: Option<crate::app::SplitNode>,
    area: Rect,
    panes: Vec<(usize, Rect)>,
    #[cfg(test)]
    builds: usize,
}

impl SplitPaneLayout {
    fn update(&mut self, tree: &crate::app::SplitNode, area: Rect, dividers: &mut Vec<SplitDivider>) {
        if self.area == area && self.tree.as_ref().is_some_and(|previous| previous == tree) {
            return;
        }
        self.area = area;
        self.tree = Some(tree.clone());
        self.panes.clear();
        dividers.clear();
        self.collect(tree, area, &mut Vec::new(), dividers);
        #[cfg(test)]
        { self.builds += 1; }
    }

    fn collect(&mut self, node: &crate::app::SplitNode, area: Rect, path: &mut Vec<usize>, dividers: &mut Vec<SplitDivider>) {
        match node {
            crate::app::SplitNode::Leaf(group) => self.panes.push((*group, area)),
            crate::app::SplitNode::Split { direction, children, ratios } => {
                if children.is_empty() { return; }
                let dir = match direction {
                    crate::app::SplitDirection::Horizontal => Direction::Horizontal,
                    crate::app::SplitDirection::Vertical => Direction::Vertical,
                };
                let rects = Layout::default().direction(dir)
                    .constraints(split_constraints(children.len(), ratios)).split(area);
                for (index, child) in children.iter().enumerate() {
                    path.push(index);
                    self.collect(child, rects[index * 2], path, dividers);
                    path.pop();
                    if index > 0 {
                        dividers.push((rects[index * 2 - 1], path.clone(), index - 1, *direction, area));
                    }
                }
            }
        }
    }
}

fn render_split_tree(f: &mut Frame, app: &mut App, node: &crate::app::SplitNode, area: Rect, show_chrome: bool) {
    let mut layout = std::mem::take(&mut app.split_layout_cache);
    layout.update(node, area, &mut app.split_dividers);
    for &(group, pane) in &layout.panes {
        render_group_pane(f, app, pane, group, show_chrome);
    }
    let background = app.colors.bg_editor;
    for (rect, path, child, direction, _) in &app.split_dividers {
        let dir = match direction {
            crate::app::SplitDirection::Horizontal => Direction::Horizontal,
            crate::app::SplitDirection::Vertical => Direction::Vertical,
        };
        let hovered = rect.contains((app.mouse_col, app.mouse_row).into());
        let dragging = app.resizing_split && matches!(
            app.dragging_divider.as_ref(),
            Some((drag_path, drag_child, drag_direction, _))
                if drag_path == path && drag_child == child && drag_direction == direction
        );
        let layer = if dragging || hovered { app.colors.ui.sash_hover_border }
            else { app.colors.ui.editor_split_borders[usize::from(dir == Direction::Vertical)] };
        let style = Style::default().bg(background).fg(layer.border(background).unwrap_or(background));
        draw_divider(f, *rect, &dir, style);
    }
    app.split_layout_cache = layout;
}

pub(crate) fn draw_editor_section(f: &mut Frame, app: &mut App, area: Rect) {
    app.keybindings_ui.begin_frame();
    crate::ui_extension_info::begin_frame(app);
    app.sync_diff_tab();
    app.commit_details.visible = false;
    // Safety: if split mode is active but current group has no tabs, recover to a non-empty group.
    if app.split_active() && app.tabs.is_empty() {
        app.close_empty_groups();
    }

    app.desks.host = app.split_tree.as_ref().and_then(|tree| tree.leaves().first().copied()).unwrap_or(app.active_group);
    let show_chrome = !app.zen_mode;
    let is_split = app.split_active();
    let current_tab_kind = app.tabs.get(app.active_tab).map(|tab| tab.kind);
    if !is_split || app.split_tree.is_none() {
        app.split_dividers.clear();
        app.split_layout_cache = Default::default();
    }
    app.diff_split_area = None;
    app.diff_divider_area = None;
    app.diff_left_code_area = None;
    app.diff_left_h_scrollbar_area = None;
    app.breadcrumb_area = None;
    app.breadcrumb_segments.clear();
    app.breadcrumb_markdown_preview_area = None;
    app.diff_nav_prev_area = None;
    app.diff_nav_next_area = None;
    app.diff_layout_toggle_area = None;

    // In split mode, each pane draws its own tab bar — skip top-level chrome
    let show_tabs = show_chrome && !is_split && app.show_tab_bar;
    let tab_height = u16::from(show_tabs);
    let show_breadcrumbs = show_chrome
        && !is_split
        && app.show_breadcrumbs
        && matches!(
            current_tab_kind,
            Some(crate::app::TabKind::File | crate::app::TabKind::GitRevision | crate::app::TabKind::Diff(_))
        );
    let breadcrumb_height = u16::from(show_breadcrumbs);

    let layout = if is_split { [area; 3] } else {
        let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(tab_height),
            Constraint::Length(breadcrumb_height),
            Constraint::Min(1),
        ])
        .split(area);
        [layout[0], layout[1], layout[2]]
    };

    if show_tabs {
        app.tab_bar_area = Some(layout[0]);
        crate::ui_chrome_tabs::draw_tab_bar(f, app, layout[0]);
    } else if !is_split {
        app.tab_bar_area = None;
    }
    if show_breadcrumbs {
        crate::ui_chrome_tabs::draw_breadcrumb(f, app, layout[1]);
    }
    if !is_split {
        crate::ui_loading::draw_editor_header(f, app, layout[0], layout[1]);
    }

    // Settings/Keyboard Shortcuts tab view
    if !is_split && let Some(crate::app::TabKind::Chat(key)) = current_tab_kind {
        app.settings_editor_active = false;
        app.editor_area = Some(layout[2]);
        crate::ui_chat::queue_editor(app, key, app.active_group, layout[2]);
        return;
    } else if !is_split && current_tab_kind == Some(crate::app::TabKind::CommitDetails) {
        app.settings_editor_active = false;
        app.editor_area = Some(layout[2]);
        crate::ui_commit_details::draw_in_area(f, app, layout[2]);
        return;
    } else if !is_split && current_tab_kind == Some(crate::app::TabKind::Settings) {
        app.settings_editor_active = true;
        app.editor_area = Some(layout[2]);
        crate::ui_settings::draw_settings_editor(f, app, layout[2]);
        return;
    } else if !is_split && current_tab_kind == Some(crate::app::TabKind::KeyboardShortcuts) {
        app.settings_editor_active = false;
        app.editor_area = Some(layout[2]);
        crate::ui_keybindings::draw(f, app, layout[2]);
        return;
    } else if !is_split && current_tab_kind == Some(crate::app::TabKind::ExtensionInfo) {
        app.settings_editor_active = false;
        app.editor_area = Some(layout[2]);
        crate::ui_extension_info::draw_extension_info(f, app, layout[2]);
        return;
    } else if !is_split && current_tab_kind == Some(crate::app::TabKind::Terminal) {
        app.settings_editor_active = false;
        app.editor_area = Some(layout[2]);
        app.code_area = None;
        app.gutter_area = None;
        app.minimap_area = None;
        app.scrollbar_area = None;
        app.h_scrollbar_area = None;
        draw_terminal_in_tab(f, app, layout[2]);
        return;
    } else if !is_split && current_tab_kind == Some(crate::app::TabKind::MarkdownPreview) {
        app.settings_editor_active = false;
        app.editor_area = Some(layout[2]);
        app.code_area = None;
        app.gutter_area = None;
        app.minimap_area = None;
        app.scrollbar_area = None;
        app.h_scrollbar_area = None;
        crate::ui_helpers::draw_markdown_preview(f, app, layout[2]);
        return;
    }
    // Active tab is not Settings/KeyboardShortcuts, clear flags
    app.settings_editor_active = false;

    if app.should_render_diff_for_active_editor() {
        app.refresh_compare_diff();
    }

    if is_split {
        // Tree-based split rendering
        if let Some(tree) = app.split_tree.take() {
            render_split_tree(f, app, &tree, area, show_chrome);
            app.split_tree = Some(tree);
        } else {
            // Fallback: flat rendering for legacy splits without a tree
            let non_active_count = app.groups.iter().enumerate()
                .filter(|(gi, g)| *gi != app.active_group && !g.tabs.is_empty())
                .count();
            let total_panes = 1 + non_active_count;
            let direction = if app.split_vertical { Direction::Horizontal } else { Direction::Vertical };

            let mut constraints = Vec::with_capacity(total_panes * 2 - 1);
            let pane_pct = 100u16 / total_panes as u16;
            for i in 0..total_panes {
                if i > 0 { constraints.push(Constraint::Length(1)); }
                if i == total_panes - 1 {
                    constraints.push(Constraint::Min(1));
                } else {
                    constraints.push(Constraint::Percentage(pane_pct));
                }
            }
            let split = Layout::default().direction(direction).constraints(constraints).split(area);

            let mut pane_index = 0;
            let mut pane_assignments: Vec<(usize, usize)> = Vec::with_capacity(app.groups.len());
            for (gi, g) in app.groups.iter().enumerate() {
                if gi == app.active_group || !g.tabs.is_empty() {
                    pane_assignments.push((gi, pane_index * 2));
                    pane_index += 1;
                }
            }
            if let Some(&(_, active_rect_idx)) = pane_assignments.iter().find(|(gi, _)| *gi == app.active_group) {
                render_group_pane(f, app, split[active_rect_idx], app.active_group, show_chrome);
            }
            let divider_bg = app.colors.bg_editor;
            let divider_style = Style::default().bg(divider_bg).fg(app.colors.ui.editor_split_borders[
                usize::from(direction == Direction::Vertical)
            ].border(divider_bg).unwrap_or(divider_bg));
            for i in 1..total_panes {
                let div_idx = i * 2 - 1;
                if div_idx < split.len() {
                    draw_divider(f, split[div_idx], &direction, divider_style);
                }
            }
            for &(gi, rect_idx) in &pane_assignments {
                if gi != app.active_group {
                    app.groups[gi].editor_area = Some(split[rect_idx]);
                    draw_split_editor_for_group(f, app, split[rect_idx], gi);
                }
            }
        }
    } else if app.diff_mode == crate::app::DiffMode::SideBySide
        && app.should_render_diff_for_active_editor()
    {
        draw_side_by_side_diff(f, app, layout[2]);
    } else if app.diff_mode == crate::app::DiffMode::Inline
        && app.should_render_diff_for_active_editor()
    {
        app.editor_area = Some(layout[2]);
        draw_inline_diff(f, app, layout[2]);
    } else {
        // If diff mode is on but no base content, clear it
        if app.diff_mode != crate::app::DiffMode::Off && app.compare_base_content.is_none() {
            app.clear_diff_state();
        }
        app.editor_area = Some(layout[2]);
        draw_editor(f, app, layout[2]);
    }
}

pub(crate) fn draw_zen_exit_button(f: &mut Frame, app: &mut App, area: Rect) {
    if area.is_empty() {
        return;
    }
    f.render_widget(Paragraph::new("").style(Style::default().bg(app.colors.bg_editor)), area);
    // Keep an explicit exit action even in narrow terminals. Its own row
    // prevents it from covering code, terminal output, or split controls.
    let label = ["[ Exit Zen Mode ]", "[ Exit Zen ]", "[ Exit ]", "Exit"]
        .into_iter()
        .find(|label| label.len() <= area.width as usize)
        .unwrap_or("Exit");
    let width = (label.len() as u16).min(area.width);
    let button = Rect::new(area.right().saturating_sub(width), area.y, width, 1);
    app.zen_exit_area = Some(button);
    let hovered = button.contains((app.mouse_col, app.mouse_row).into());
    let style = crate::ui_widgets::action_button_style(&app.colors, app.colors.bg_editor, false, hovered, true);
    f.render_widget(Paragraph::new(label).style(style), button);
}

pub(crate) fn draw_menu_bar(f: &mut Frame, app: &App, area: Rect) {
    let (bg, fg) = if app.terminal_has_focus {
        (app.colors.ui.bg_title, app.colors.ui.fg_title)
    } else {
        (app.colors.ui.bg_title_inactive, app.colors.ui.fg_title_inactive)
    };
    let fg = app.colors.ui.readable_foreground(fg, bg, 4.5);
    let bg_active = app.colors.ui.menubar_background.over(bg).unwrap_or(bg);
    let fg_active = app.colors.ui.readable_foreground(app.colors.ui.menubar_foreground.over(bg_active).unwrap_or(fg), bg_active, 4.5);
    let close_style = app.colors.ui.toolbar_style(fg, bg, app.hovered_close_button, false, false);
    let close_bg = close_style.bg.unwrap_or(bg);
    let close_fg = close_style.fg.unwrap_or(fg);

    let fixed_width = UnicodeWidthStr::width(TITLE_BAR_LOGO) + TITLE_BAR_WINDOW_CONTROLS_WIDTH;
    if (area.width as usize) < fixed_width {
        let width = area.width as usize;
        let mut compact = Vec::with_capacity(3);
        if width >= TITLE_BAR_WINDOW_CONTROLS_WIDTH {
            compact.push(Span::styled(
                crate::helpers::spaces(width - TITLE_BAR_WINDOW_CONTROLS_WIDTH),
                Style::default().bg(bg),
            ));
            compact.push(Span::styled(
                " ─  □ ",
                Style::default().fg(fg).bg(bg),
            ));
            compact.push(Span::styled(
                " × ",
                Style::default().fg(close_fg).bg(close_bg),
            ));
        } else if width >= TITLE_BAR_CLOSE_BTN_WIDTH {
            compact.push(Span::styled(
                crate::helpers::spaces(width - TITLE_BAR_CLOSE_BTN_WIDTH),
                Style::default().bg(bg),
            ));
            compact.push(Span::styled(
                " × ",
                Style::default().fg(close_fg).bg(close_bg),
            ));
        } else if width > 0 {
            compact.push(Span::styled(
                crate::helpers::spaces(width - 1),
                Style::default().bg(bg),
            ));
            compact.push(Span::styled(
                "×",
                Style::default().fg(close_fg).bg(close_bg),
            ));
        }
        f.render_widget(Paragraph::new(Line::from(compact)), area);
        return;
    }

    let labels = App::menu_labels();
    let mut spans = Vec::with_capacity(labels.len() + 6);
    let visible_menu_count = visible_menu_label_count(area.width);
    let show_overflow = menu_overflow_visible(area.width);
    // App icon
    spans.push(Span::styled(
        TITLE_BAR_LOGO,
        Style::default()
            .fg(app.colors.ui.readable_foreground(app.colors.fg_accent_blue, bg, 3.0))
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    ));

    for (i, label) in labels.iter().take(visible_menu_count).enumerate() {
        let (fg_c, bg_c) = if app.menu_active && app.menu_selected == i {
            (fg_active, bg_active)
        } else if app.hovered_menu == Some(i) {
            (fg_active, bg_active)
        } else {
            (fg, bg)
        };
        let mut menu_buf = String::with_capacity(label.len() + 2);
        let _ = write!(menu_buf, " {label} ");
        let mut style = Style::default().fg(fg_c).bg(bg_c);
        if (app.menu_active && app.menu_selected == i || app.hovered_menu == Some(i))
            && let Some(border) = app.colors.ui.menubar_border.border(bg_c) {
            style = style.add_modifier(Modifier::UNDERLINED).underline_color(border);
        }
        spans.push(Span::styled(menu_buf, style));
    }
    if show_overflow {
        let overflow_active = (app.menu_active && app.menu_selected >= visible_menu_count)
            || app
                .hovered_menu
                .is_some_and(|hovered| hovered >= visible_menu_count);
        spans.push(Span::styled(
            MENU_OVERFLOW_LABEL,
            if overflow_active {
                Style::default().fg(fg_active).bg(bg_active)
            } else {
                Style::default().fg(fg).bg(bg)
            },
        ));
    }

    // Render from the same layout used by mouse hit testing and tooltips.
    let center = title_bar_center_layout(app, area);
    let content_start = center.navigation.map(|buttons| buttons[0].x)
        .or(center.workspace.map(|workspace| workspace.x)).unwrap_or(center.area.x);
    spans.push(Span::styled(crate::helpers::spaces((content_start - center.area.x) as usize), Style::default().bg(bg)));
    let mut content_end = content_start;
    if let Some(buttons) = center.navigation {
        for (index, button) in buttons.into_iter().enumerate() {
            let enabled = app.navigation.can_navigate(index == 1);
            let hovered = enabled && button.contains((app.mouse_col, app.mouse_row).into());
            let mut style = app.colors.ui.toolbar_style(fg, bg, hovered, false, false);
            if !enabled {
                let background = style.bg.unwrap_or(bg);
                style = style.fg(crate::ui_colors::disabled_text_color(
                    app.colors.ui.disabled_foreground.over(background).unwrap_or(app.colors.fg_dim),
                    style.fg.unwrap_or(fg), background, app.colors.ui.contrast_colors()));
            }
            spans.push(Span::styled(if index == 0 { " ← " } else { " → " }, style));
            content_end = button.right();
        }
    }
    if let Some(workspace) = center.workspace {
        spans.push(Span::styled(
            format!(" {} ", workspace_name(app)),
            app.colors.ui.command_center_style(bg, app.terminal_has_focus,
                workspace.contains((app.mouse_col, app.mouse_row).into()) || app.palette.active),
        ));
        content_end = workspace.right();
    }
    spans.push(Span::styled(crate::helpers::spaces(center.area.right().saturating_sub(content_end) as usize), Style::default().bg(bg)));
    if let Some(button) = agent_chat_button_area(area) {
        let hovered = button.contains((app.mouse_col, app.mouse_row).into());
        let mut style = crate::ui_widgets::action_button_style(
            &app.colors, bg, app.chat.visible && app.chat.layout.dock_visible, hovered, true,
        ).add_modifier(Modifier::BOLD);
        if hovered { style = style.add_modifier(Modifier::UNDERLINED); }
        spans.push(Span::styled(agent_chat_button_label(area.width).unwrap_or_default(), style));
        spans.push(Span::styled(" ", Style::default().bg(bg)));
    }
    spans.push(Span::styled(
        " ─  □ ",
        Style::default().fg(fg).bg(bg),
    ));
    spans.push(Span::styled(" × ", Style::default().fg(close_fg).bg(close_bg)));

    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub(crate) fn draw_menu_dropdown(f: &mut Frame, app: &App) {
    let raw_items = app.menu_items(app.menu_selected);
    if raw_items.is_empty() { return; }

    let menu_x = menu_dropdown_anchor_x(app);
    let menu_y: u16 = if let Some(bar) = app.menu_bar_area { bar.y + 1 } else { 1 };

    let items: Vec<DropdownItem> = raw_items.iter()
        .map(|(label, cmd)| {
            let disabled = !cmd.is_empty() && label != "─" && app.is_menu_cmd_disabled(cmd);
            DropdownItem::new(label).disabled(disabled)
        })
        .collect();
    let colors = themed_dropdown_colors(&app.colors);
    DropdownMenu::new(&items, colors)
        .anchor(menu_x, menu_y)
        .min_width(16)
        .selected(app.menu_item_selected)
        .mouse(app.mouse_row, app.mouse_col)
        .render(f);
}

pub(crate) fn draw_outline(f: &mut Frame, app: &App, area: Rect) {
    let sidebar_bg = app.colors.bg_sidebar;
    let focused = app.active_pane == crate::app::ActivePane::Explorer
        && app.sidebar_view == crate::app::SidebarView::Outline;
    f.render_widget(Paragraph::new("").style(Style::default().bg(sidebar_bg)), area);

    let header_area = Rect::new(area.x, area.y, area.width, 1);
    let outline_header_hovered = app.mouse_row == header_area.y
        && app.mouse_col >= header_area.x && app.mouse_col < header_area.x + header_area.width;
    let header_style = app.colors.ui.toolbar_layer_style(app.colors.ui.section_foreground, app.colors.ui.bg_section,
        outline_header_hovered, false, false).add_modifier(Modifier::BOLD);
    f.render_widget(
        Paragraph::new(Span::styled(
            " Outline",
            header_style,
        )).style(header_style),
        header_area,
    );

    if area.height < 2 { return; }
    let list_area = Rect::new(area.x, area.y + 1, area.width, area.height - 1);
    let items = &app.outline_items;

    if items.is_empty() {
        f.render_widget(
            Paragraph::new(Span::styled(
                if app.source_symbol_work.outline_pending() {
                    " Reading source symbols…"
                } else {
                    " No symbols found"
                },
                Style::default().fg(app.colors.ui.description_color(sidebar_bg)).bg(sidebar_bg),
            )),
            Rect::new(list_area.x, list_area.y, list_area.width, 1),
        );
        return;
    }

    // Reserve 1 column for scrollbar if content overflows
    let total_items = items.len();
    let needs_scrollbar = total_items > list_area.height as usize && list_area.width > 5;
    let (entry_area, sb_area) = if needs_scrollbar {
        (
            Rect::new(list_area.x, list_area.y, list_area.width.saturating_sub(1), list_area.height),
            Some(Rect::new(list_area.x + list_area.width.saturating_sub(1), list_area.y, 1, list_area.height)),
        )
    } else {
        (list_area, None)
    };
    let visible = entry_area.height as usize;
    let render_width = entry_area.width;

    let scroll = app.outline_scroll;
    let mut styles = rustc_hash::FxHashMap::default();
    let mut line_info_buf = String::with_capacity(8);
    for (i, item) in items.iter().enumerate().skip(scroll).take(visible) {
        let y = entry_area.y.saturating_add((i - scroll) as u16);
        let is_selected = i == app.outline_selected;

        let kind_icon = match item.kind {
            "function" => " ƒ ",
            "struct" | "class" => " S ",
            "enum" => " E ",
            "trait" | "interface" => " I ",
            "impl" => " ⊕ ",
            "module" => " M ",
            "constant" | "static" => " C ",
            "type" => " T ",
            _ => " · ",
        };
        let hovered = app.outline_hovered == Some(i);
        // Contrast calculations depend on kind and interaction state, not on
        // the symbol name. Resolve each distinct visible style once per frame.
        let (row_style, icon_style, detail_style) = *styles.entry((item.kind, is_selected, hovered))
            .or_insert_with(|| {
                let row = app.colors.ui.list_row_style(app.colors.ui.sidebar_foreground,
                    sidebar_bg, is_selected, focused, hovered);
                let bg = row.bg.unwrap_or(sidebar_bg);
                let color = app.colors.ui.list_icon_color(app.colors.ui.symbol_color(item.kind, bg),
                    bg, is_selected, focused);
                let color = app.colors.ui.readable_foreground(color, bg, if kind_icon.is_ascii() { 4.5 } else { 3.0 });
                (row, Style::default().fg(color).bg(bg),
                    Style::default().fg(app.colors.ui.description_color(bg)).bg(bg))
            });

        let max_name = (render_width as usize).saturating_sub(4);
        let name_display = if UnicodeWidthStr::width(item.name.as_str()) > max_name {
            let mut truncated = String::with_capacity(max_name);
            let mut w = 0;
            for ch in item.name.chars() {
                let cw = ch.width().unwrap_or(0);
                if w + cw + 1 > max_name { break; }
                truncated.push(ch);
                w += cw;
            }
            truncated.push('…');
            Cow::Owned(truncated)
        } else {
            Cow::Borrowed(item.name.as_str())
        };
        line_info_buf.clear();
        let _ = write!(line_info_buf, ":{}", item.line + 1); // Error OK: write to String buffer is infallible in practice

        let text_width = (UnicodeWidthStr::width(kind_icon)
            + UnicodeWidthStr::width(name_display.as_ref())
            + line_info_buf.len()) as u16;
        let mut spans = vec![
            Span::styled(kind_icon, icon_style),
            Span::styled(name_display, row_style),
            Span::styled(line_info_buf.as_str(), detail_style),
        ];
        let fill = render_width.saturating_sub(text_width);
        if fill > 0 {
            spans.push(Span::styled(crate::helpers::spaces(fill as usize), row_style));
        }
        f.render_widget(Paragraph::new(Line::from(spans)).style(row_style), Rect::new(entry_area.x, y, render_width, 1));
    }

    // Draw scrollbar if content overflows
    if let Some(sb) = sb_area {
        let height = sb.height as usize;
        let thumb_size = ((visible as f64 / total_items as f64) * height as f64).max(1.0) as usize;
        let thumb_size = thumb_size.min(height);
        let max_scroll = total_items.saturating_sub(visible);
        let thumb_pos = if max_scroll > 0 {
            ((scroll as f64 / max_scroll as f64) * (height - thumb_size) as f64) as usize
        } else {
            0
        };
        let hovered = sb.contains((app.mouse_col, app.mouse_row).into());
        let (track_color, thumb_color) = app.colors.ui.scrollbar_colors(sidebar_bg, hovered, false);
        for i in 0..height {
            let in_thumb = i >= thumb_pos && i < thumb_pos + thumb_size;
            let color = if in_thumb { thumb_color } else { track_color };
            if let Some(cell) = f.buffer_mut().cell_mut((sb.x, sb.y + i as u16)) {
                cell.set_symbol("▐").set_style(Style::default().fg(color).bg(track_color));
            }
        }
    }
}

pub(crate) fn activity_item_regions(area: Rect) -> Vec<(Rect, usize)> {
    if area.is_empty() { return Vec::new(); }
    let primary: &[usize] = &[0, 1, 2, 4, 5];
    let height = (area.height / (primary.len() as u16 + 1)).clamp(1, 3);
    let visible = primary.len().min(area.height.saturating_sub(height) as usize / height as usize);
    let mut regions: Vec<_> = primary.iter().take(visible).enumerate().map(|(position, &index)|
        (Rect::new(area.x, area.y + position as u16 * height, area.width, height), index)).collect();
    regions.push((Rect::new(area.x, area.bottom() - height, area.width, height), 6));
    regions
}

pub(crate) fn draw_activity_bar(f: &mut Frame, app: &App, area: Rect) {
    let bg = app.colors.bg_activitybar;
    f.render_widget(Paragraph::new("").style(Style::default().bg(bg)), area);
    for (item_area, index) in activity_item_regions(area) {
        let label = match index {
            0 => ACTIVITY_LABEL_EXPLORER, 1 => ACTIVITY_LABEL_SEARCH,
            2 => ACTIVITY_LABEL_SOURCE_CONTROL, 4 => ACTIVITY_LABEL_EXTENSIONS,
            5 => ACTIVITY_LABEL_OUTLINE, 6 => ACTIVITY_LABEL_SETTINGS,
            _ => continue,
        };
        let active = if index == 6 { app.settings_editor_active || app.settings_active }
            else { app.show_explorer && app.activity_bar_selected == index };
        let hovered = app.hovered_activity == Some(index);
        let cell_bg = if active { app.colors.ui.bg_activity_active.over(bg).unwrap_or(bg) } else { bg };
        let fg = app.colors.ui.readable_foreground(if active || hovered { app.colors.ui.fg_activity }
            else { app.colors.ui.fg_activity_inactive }, cell_bg, 4.5);
        f.render_widget(Paragraph::new("").style(Style::default().bg(cell_bg)), item_area);
        if active {
            let border = app.colors.ui.border_activity_active.border(cell_bg).unwrap_or(fg);
            let border = app.colors.ui.readable_foreground(border, cell_bg, 3.0);
            for y in item_area.y..item_area.bottom() {
                f.render_widget(Paragraph::new("▎").style(Style::default().fg(border).bg(cell_bg)),
                    Rect::new(item_area.x, y, 1, 1));
            }
        }
        let content_width = item_area.width.saturating_sub(2);
        let count = match index {
            2 => app.source_control_change_count(), 4 => app.extension_manager.error_count(),
            _ => 0,
        };
        let unknown_count = index == 2 && matches!(app.source_control_repository_state,
            crate::app::SourceControlRepositoryState::Checking | crate::app::SourceControlRepositoryState::Loading);
        let show_badge = (unknown_count || count > 0) && item_area.height >= 3 && content_width > 0;
        let text_height = item_area.height - u16::from(show_badge);
        let line_count = label.0.iter().filter(|line| !line.is_empty()).count().min(text_height as usize);
        let text_y = item_area.y + text_height.saturating_sub(line_count as u16) / 2;
        for (offset, text) in label.0.iter().filter(|line| !line.is_empty()).take(line_count).enumerate() {
            let text_width = UnicodeWidthStr::width(*text) as u16;
            if text_width <= content_width {
                let text_x = item_area.x + 1 + content_width.saturating_sub(text_width) / 2;
                f.render_widget(Paragraph::new(*text).style(Style::default().fg(fg).bg(cell_bg)),
                    Rect::new(text_x, text_y + offset as u16, text_width, 1));
            }
        }
        if show_badge {
            let badge = if unknown_count { "?".into() }
                else if count > 99 { "99+".into() } else { count.to_string() };
            let width = (UnicodeWidthStr::width(badge.as_str()) as u16).min(content_width);
            let badge_bg = app.colors.ui.bg_activity_badge;
            let badge_fg = app.colors.ui.readable_foreground(app.colors.ui.fg_activity_badge, badge_bg, 4.5);
            let x = item_area.x + 1 + content_width.saturating_sub(width) / 2;
            let y = item_area.bottom() - 1;
            f.render_widget(Paragraph::new(badge).style(Style::default().fg(badge_fg).bg(badge_bg).add_modifier(Modifier::BOLD)),
                Rect::new(x, y, width, 1));
        } else if unknown_count && item_area.width >= 2 {
            // Short activity bars have no spare row; keep the unknown count
            // in the right padding without displacing SRC / CTRL or its border.
            let badge_bg = app.colors.ui.bg_activity_badge;
            let badge_fg = app.colors.ui.readable_foreground(app.colors.ui.fg_activity_badge, badge_bg, 4.5);
            f.render_widget(Paragraph::new("?").style(Style::default().fg(badge_fg).bg(badge_bg).add_modifier(Modifier::BOLD)),
                Rect::new(item_area.right() - 1, item_area.bottom() - 1, 1, 1));
        }
    }
}
pub(crate) fn draw_tab_context_menu(f: &mut Frame, app: &mut App) {
    let raw_items = App::tab_context_items();
    let (x, y) = app.tab_context_pos;
    let items: Vec<DropdownItem> = raw_items.iter()
        .map(|(label, command, shortcut)| {
            DropdownItem::new(label)
                .shortcut(shortcut)
                .disabled(app.tab_context_command_disabled(command))
        })
        .collect();
    let colors = themed_dropdown_colors(&app.colors);
    let inner = DropdownMenu::new(&items, colors)
        .anchor(x, y)
        .min_width(16)
        .selected(app.tab_context_selected)
        .mouse(app.mouse_row, app.mouse_col)
        .render(f);
    app.tab_context_inner = Some(inner);
}


#[cfg(test)]
#[path = "ui_pane_performance_tests.rs"]
mod pane_performance_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn diff_tab_navigation_arrows_render_hover_and_navigate_in_each_layout() {
        use crate::app::{DiffMode, SplitDirection, Tab, TabKind};
        use crate::editor::Editor;

        for split in [false, true] {
            for mode in [DiffMode::SideBySide, DiffMode::Inline] {
                let mut app = App::test_new();
                app.show_minimap = false;
                let mut editor = Editor::new();
                editor.file_path = Some("changes.txt".into());
                editor.rope = ropey::Rope::from_str("first\nnew one\nmiddle\nnew two\nlast\n");
                editor.read_only = true;
                let mut tab = Tab::new_file(editor);
                tab.kind = TabKind::Diff(1);
                app.tabs = vec![Tab::new_file(Editor::new()), tab];
                app.active_tab = 1;
                app.diff_tabs.owner = Some(1);
                app.diff_mode = mode;
                app.compare_base_content = Some("first\nold one\nmiddle\nold two\nlast\n".into());
                app.set_compare_diff_lines(crate::app::compute_file_diff(
                    "first\nold one\nmiddle\nold two\nlast\n",
                    "first\nnew one\nmiddle\nnew two\nlast\n",
                ));
                app.editor_mut().unwrap().clear_git_line_status();
                if split {
                    app.create_split_group_dir(1, SplitDirection::Horizontal);
                }

                let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
                terminal
                    .draw(|frame| {
                        draw_editor_section(frame, &mut app, Rect::new(0, 0, 120, 20));
                    })
                    .unwrap();
                let previous = app.diff_nav_prev_area.expect("previous change button");
                let next = app.diff_nav_next_area.expect("next change button");
                let buffer = terminal.backend().buffer();
                assert_eq!(
                    buffer.cell((previous.x + 1, previous.y)).unwrap().symbol(),
                    "▲"
                );
                assert_eq!(buffer.cell((next.x + 1, next.y)).unwrap().symbol(), "▼");
                assert!(app.code_area.unwrap().y > next.y);

                app.mouse_col = next.x + 1;
                app.mouse_row = next.y;
                terminal
                    .draw(|frame| {
                        draw_editor_section(frame, &mut app, Rect::new(0, 0, 120, 20));
                    })
                    .unwrap();
                assert_eq!(
                    terminal
                        .backend()
                        .buffer()
                        .cell((next.x + 1, next.y))
                        .unwrap()
                        .bg,
                    app.colors.ui.toolbar_hover.over(app.colors.ui.bg_breadcrumb).unwrap_or(app.colors.ui.bg_breadcrumb)
                );
                let click = |app: &mut App, area: Rect| {
                    crate::mouse::handle_mouse(
                        app,
                        crossterm::event::MouseEvent {
                            kind: crossterm::event::MouseEventKind::Down(
                                crossterm::event::MouseButton::Left,
                            ),
                            column: area.x + 1,
                            row: area.y,
                            modifiers: crossterm::event::KeyModifiers::NONE,
                        },
                    );
                };
                click(&mut app, next);
                assert_eq!(app.editor().unwrap().cursor_line, 1);
                click(&mut app, next);
                assert_eq!(app.editor().unwrap().cursor_line, 3);
                click(&mut app, previous);
                assert_eq!(app.editor().unwrap().cursor_line, 1);

                let toggle = app.diff_layout_toggle_area.expect("diff layout button");
                let cache = std::sync::Arc::clone(&app.diff_render_cache);
                for _ in 0..2 {
                    let previous_mode = app.diff_mode;
                    let editor = app.editor().unwrap();
                    let position = (editor.cursor_line, editor.cursor_col, editor.scroll_offset);
                    app.mouse_col = toggle.x + 1;
                    app.mouse_row = toggle.y;
                    terminal
                        .draw(|frame| {
                            draw_editor_section(frame, &mut app, Rect::new(0, 0, 120, 20));
                        })
                        .unwrap();
                    let button_text: String = (toggle.x..toggle.right())
                        .map(|x| {
                            terminal.backend().buffer().cell((x, toggle.y)).unwrap().symbol()
                        })
                        .collect();
                    assert_eq!(
                        button_text.trim(),
                        if previous_mode == DiffMode::SideBySide {
                            "Inline"
                        } else {
                            "Side by Side"
                        }
                    );
                    assert_eq!(
                        terminal.backend().buffer().cell((toggle.x, toggle.y)).unwrap().bg,
                        app.colors.ui.toolbar_hover.over(app.colors.ui.bg_breadcrumb).unwrap_or(app.colors.ui.bg_breadcrumb)
                    );

                    click(&mut app, toggle);
                    assert!(app.diff_mode != previous_mode);
                    assert!(app.diff_mode != DiffMode::Off);
                    assert!(std::sync::Arc::ptr_eq(&cache, &app.diff_render_cache));
                    let editor = app.editor().unwrap();
                    assert_eq!(
                        (editor.cursor_line, editor.cursor_col, editor.scroll_offset),
                        position
                    );
                    assert!(app.should_render_diff_for_active_editor());
                    terminal
                        .draw(|frame| {
                            draw_editor_section(frame, &mut app, Rect::new(0, 0, 120, 20));
                        })
                        .unwrap();
                    assert_eq!(app.diff_layout_toggle_area, Some(toggle));
                    assert_eq!(
                        app.diff_divider_area.is_some(),
                        app.diff_mode == DiffMode::SideBySide
                    );
                }
                assert!(app.diff_mode == mode);

                // Hidden chrome must not leave invisible clickable arrows behind.
                app.show_breadcrumbs = false;
                terminal
                    .draw(|frame| {
                        draw_editor_section(frame, &mut app, Rect::new(0, 0, 120, 20));
                    })
                    .unwrap();
                assert!(app.diff_nav_prev_area.is_none());
                assert!(app.diff_nav_next_area.is_none());
                assert!(app.diff_layout_toggle_area.is_none());
                assert!(app.breadcrumb_area.is_none());
            }
        }
    }

    fn test_colors() -> crate::app_theme::ThemeColors {
        let mut theme = crate::ui_control_visibility_tests::theme_with_popup_and_control_roles();
        for (key, value) in [("sideBar.foreground", "#bbccdd"),
            ("gitDecoration.modifiedResourceForeground", "#ffee99"),
            ("gitDecoration.addedResourceForeground", "#bbff99"),
            ("gitDecoration.untrackedResourceForeground", "#bbddff"),
            ("gitDecoration.deletedResourceForeground", "#ffbbdd"),
            ("gitDecoration.ignoredResourceForeground", "#c0ccde")] {
            theme.workbench_colors.insert(key.into(), value.into());
        }
        theme.to_theme_colors()
    }

    #[test]
    fn git_status_colors_use_each_resource_role_and_the_clean_sidebar_foreground() {
        let colors = test_colors();
        for (status, expected) in [
            (GitStatus::Modified, Color::Rgb(255, 238, 153)),
            (GitStatus::Added, Color::Rgb(187, 255, 153)),
            (GitStatus::Untracked, Color::Rgb(187, 221, 255)),
            (GitStatus::Deleted, Color::Rgb(255, 187, 221)),
            (GitStatus::Unmodified, Color::Rgb(187, 204, 221)),
            (GitStatus::Ignored, Color::Rgb(192, 204, 222)),
        ] {
            assert_eq!(
                git_status_color(status, &colors, colors.bg_sidebar),
                expected,
                "{status:?}"
            );
        }
    }

    #[test]
    fn changed_folder_icon_uses_git_status_color() {
        let colors = test_colors();
        assert_eq!(
            folder_icon_color(GitStatus::Modified, &colors, colors.bg_sidebar),
            Color::Rgb(255, 238, 153)
        );
        assert_eq!(
            folder_icon_color(GitStatus::Added, &colors, colors.bg_sidebar),
            Color::Rgb(187, 255, 153)
        );
        assert_eq!(
            folder_icon_color(GitStatus::Deleted, &colors, colors.bg_sidebar),
            Color::Rgb(255, 187, 221)
        );
    }

    #[test]
    fn clean_folder_icon_keeps_folder_color() {
        let colors = test_colors();
        assert_eq!(
            folder_icon_color(GitStatus::Unmodified, &colors, colors.bg_sidebar),
            colors.ui.folder_icon_color(colors.bg_sidebar)
        );
    }

    #[test]
    fn ignored_folder_icon_uses_the_ignored_resource_color() {
        let colors = test_colors();
        assert_eq!(
            folder_icon_color(GitStatus::Ignored, &colors, colors.bg_sidebar),
            git_status_color(GitStatus::Ignored, &colors, colors.bg_sidebar),
        );
        assert!(git_status_badge(GitStatus::Ignored, &colors, colors.bg_sidebar).is_none());
    }

    #[test]
    fn status_badges_distinguish_every_git_state() {
        let colors = test_colors();
        assert_eq!(
            git_status_badge(GitStatus::Modified, &colors, colors.bg_sidebar).unwrap().0,
            "M"
        );
        assert_eq!(
            git_status_badge(GitStatus::Added, &colors, colors.bg_sidebar).unwrap().0,
            "A"
        );
        assert_eq!(
            git_status_badge(GitStatus::Untracked, &colors, colors.bg_sidebar).unwrap().0,
            "U"
        );
        assert_eq!(
            git_status_badge(GitStatus::Deleted, &colors, colors.bg_sidebar).unwrap().0,
            "D"
        );
        assert!(git_status_badge(GitStatus::Unmodified, &colors, colors.bg_sidebar).is_none());
    }

    #[test]
    fn explorer_sticky_rows_render_and_receive_pointer_actions() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

        for sidebar_right in [false, true] {
            for show_open_editors in [false, true] {
                let mut app = App::test_new();
                app.sidebar_right = sidebar_right;
                app.show_open_editors = show_open_editors;
                app.sidebar_view = crate::app::SidebarView::Explorer;
                app.explorer_multi_selected.clear();
                app.explorer_selection_anchor = None;
                let root = app.explorer.root.clone();
                app.explorer.entries = (0..14).map(|i| {
                    let (path, name, depth, is_dir) = match i {
                        0 => (root.join("src"), "src".to_string(), 0, true),
                        1 => (root.join("src/nested"), "nested".to_string(), 1, true),
                        _ => (root.join(format!("src/nested/file{i}.rs")), format!("file{i}.rs"), 2, false),
                    };
                    crate::explorer::FileEntry {
                        path, name, depth, is_dir, expanded: is_dir,
                        git_status: GitStatus::Unmodified,
                        git_status_summary: GitStatusSummary::default(),
                    }
                }).collect();
                app.explorer.scroll_offset = 4;
                let area = Rect::new(0, 0, 32, 10);
                app.explorer_area = Some(area);
                let mut terminal = Terminal::new(TestBackend::new(32, 10)).unwrap();
                terminal.draw(|frame| draw_explorer(frame, &mut app, area)).unwrap();
                let tree_y = area.height - app.explorer.visible_height as u16;
                let buffer = terminal.backend().buffer();
                let row_text = |y| (0..32).map(|x| buffer.cell((x, y)).unwrap().symbol()).collect::<String>();
                assert!(row_text(tree_y).contains("src"));
                assert!(row_text(tree_y + 1).contains("nested"));
                assert!(row_text(tree_y + 2).contains("file6.rs"));
                assert_eq!(buffer.cell((10, tree_y)).unwrap().bg, app.colors.bg_sidebar);

                let event = |kind, row| MouseEvent {
                    kind, column: 10, row, modifiers: KeyModifiers::NONE,
                };
                crate::mouse::handle_mouse(&mut app, event(MouseEventKind::Moved, tree_y + 1));
                assert_eq!(app.hovered_explorer, Some(1));
                crate::mouse::handle_mouse(&mut app, event(MouseEventKind::Down(MouseButton::Right), tree_y));
                assert_eq!(app.explorer_context_target, Some(root.join("src")));
                app.explorer_context_active = false;
                app.dragging_explorer_path = Some(root.join("src/nested/file6.rs"));
                crate::mouse_drag::handle_left_drag(
                    &mut app, 10, tree_y + 1, 10, tree_y + 2, KeyModifiers::NONE,
                );
                assert_eq!(app.explorer_drop_target_idx, Some(1));
                app.dragging_explorer_path = None;
                crate::mouse::handle_mouse(&mut app, event(MouseEventKind::Down(MouseButton::Left), tree_y + 1));
                assert_eq!(app.explorer.selected, 1);
                assert!(!app.explorer.entries[1].expanded);
            }
        }
    }

    #[test]
    fn explorer_renders_all_mixed_directory_statuses() {
        let mut app = App::test_new();
        app.show_open_editors = false;
        app.explorer.entries = vec![crate::explorer::FileEntry {
            path: app.explorer.root.join("src"),
            name: "very-long-source-directory".to_string(),
            is_dir: true,
            depth: 0,
            expanded: false,
            git_status: GitStatus::Unmodified,
            git_status_summary: crate::explorer::GitStatusSummary::default(),
        }];
        app.explorer.apply_git_status_changes(
            None,
            [("src/changed.rs", 'M'), ("src/new.rs", 'A'), ("src/old.rs", 'D')],
        );
        app.set_explorer_single_selection(0);

        let mut terminal = Terminal::new(TestBackend::new(16, 5)).expect("test terminal");
        terminal
            .draw(|frame| draw_explorer(frame, &mut app, Rect::new(0, 0, 16, 5)))
            .expect("explorer draw");

        let buffer = terminal.backend().buffer();
        let root_row = (0..16)
            .filter_map(|x| buffer.cell((x, 2)))
            .map(|cell| cell.symbol())
            .collect::<String>();
        let row = (0..16)
            .filter_map(|x| buffer.cell((x, 3)))
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(
            root_row.contains("M A D"),
            "workspace root row: {root_row:?}"
        );
        assert!(row.contains("M A D"), "mixed directory row: {row:?}");
    }

    #[test]
    fn narrow_menu_bar_preserves_window_controls() {
        let app = App::test_new();
        assert_eq!(visible_menu_label_count(120), App::menu_labels().len());
        assert!(visible_menu_label_count(40) < App::menu_labels().len());

        let width = 20;
        let mut terminal = Terminal::new(TestBackend::new(width, 1)).expect("test terminal");
        terminal
            .draw(|frame| draw_menu_bar(frame, &app, Rect::new(0, 0, width, 1)))
            .expect("narrow menu draw");

        let row: String = (0..width)
            .filter_map(|x| terminal.backend().buffer().cell((x, 0)))
            .map(|cell| cell.symbol())
            .collect();
        assert!(row.ends_with(" × "), "menu row: {row:?}");
        assert!(row.contains('…'), "hidden menus expose an overflow affordance");
        assert!(row.contains('◇'), "compact chat button stays accessible");

        for compact_width in [12, 8, 2] {
            let mut compact = Terminal::new(TestBackend::new(compact_width, 1))
                .expect("compact terminal");
            compact
                .draw(|frame| {
                    draw_menu_bar(frame, &app, Rect::new(0, 0, compact_width, 1));
                })
                .expect("compact menu draw");
            assert!(
                compact
                    .backend()
                    .buffer()
                    .cell((compact_width - 1, 0))
                    .is_some_and(|cell| cell.symbol() == " " || cell.symbol() == "×")
            );
            let compact_row: String = (0..compact_width)
                .filter_map(|x| compact.backend().buffer().cell((x, 0)))
                .map(|cell| cell.symbol())
                .collect();
            assert!(compact_row.contains('×'), "compact menu row: {compact_row:?}");
        }
    }

    #[test]
    fn vanilla_activity_bar_uses_readable_text_labels_without_extensions() {
        let mut app = App::test_new();
        app.activity_bar_selected = 0;
        assert!(app.extension_manager.is_empty());
        let mut terminal = Terminal::new(TestBackend::new(6, 21)).expect("test terminal");
        terminal
            .draw(|frame| draw_activity_bar(frame, &app, Rect::new(0, 0, 6, 21)))
            .expect("activity bar draw");

        let buffer = terminal.backend().buffer();
        let row = |y| {
            (0..6)
                .filter_map(|x| buffer.cell((x, y)))
                .map(|cell| cell.symbol())
                .collect::<String>()
        };

        assert!(row(0).starts_with('▎'));
        assert!(row(0).contains("EXPL"));
        assert!(row(1).contains("ORER"));
        assert!(row(3).starts_with(' '), "inactive items have no active marker");
        assert!(row(3).contains("SEAR"));
        assert!(row(4).contains("CH"));
        assert!(row(6).contains("SRC"));
        assert!(row(7).contains("CTRL"));
        assert!(row(10).contains("EXTS"));
        assert!(row(12).contains("OUTL"));
        assert!(row(13).contains("INE"));
        assert!((0..18).all(|y| !row(y).contains("RUN")));
        assert!(row(18).contains("SETT"));
        assert!(row(19).contains("INGS"), "settings action stays at the bottom");
    }

    #[test]
    fn vanilla_activity_bar_uses_available_compact_height_for_settings() {
        let app = App::test_new();
        let mut terminal = Terminal::new(TestBackend::new(6, 18)).expect("test terminal");
        terminal
            .draw(|frame| draw_activity_bar(frame, &app, Rect::new(0, 0, 6, 18)))
            .expect("compact activity bar draw");

        let row = |y| {
            (0..6)
                .filter_map(|x| terminal.backend().buffer().cell((x, y)))
                .map(|cell| cell.symbol())
                .collect::<String>()
        };
        assert!(row(10).contains("EXTS"));
        assert!(row(12).contains("OUTL"));
        assert!(row(13).contains("INE"));
        assert!((0..18).all(|y| !row(y).contains("CHAT")));
        assert!(row(15).contains("SETT"));
        assert!(row(16).contains("INGS"));
    }

    #[test]
    fn activity_bar_labels_use_at_most_four_cells_per_row() {
        for label in [
            ACTIVITY_LABEL_EXPLORER,
            ACTIVITY_LABEL_SEARCH,
            ACTIVITY_LABEL_SOURCE_CONTROL,
            ACTIVITY_LABEL_EXTENSIONS,
            ACTIVITY_LABEL_OUTLINE,
            ACTIVITY_LABEL_SETTINGS,
        ] {
            assert!(label.0.iter().all(|line| UnicodeWidthStr::width(*line) <= 4));
        }
    }

    #[test]
    fn menu_bar_renders_command_center_and_window_controls() {
        let app = App::test_new();
        let mut terminal = Terminal::new(TestBackend::new(140, 1)).expect("test terminal");
        terminal
            .draw(|frame| draw_menu_bar(frame, &app, Rect::new(0, 0, 140, 1)))
            .expect("menu bar draw");

        let buffer = terminal.backend().buffer();
        let row = (0..140)
            .filter_map(|x| buffer.cell((x, 0)))
            .map(|cell| cell.symbol())
            .collect::<String>();
        let workspace_name = app
            .explorer
            .root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("VTCode");

        assert!(row.starts_with(TITLE_BAR_LOGO));
        assert!(!row.contains("‹ ›"));
        assert!(row.contains(workspace_name));
        assert!(row.contains("◇ Agent Chat"));
        assert!(row.ends_with(" ─  □  × "));

        let workspace_chars: Vec<char> = workspace_name.chars().collect();
        let command_x = (0..140u16.saturating_sub(workspace_chars.len() as u16))
            .find(|&start| {
                workspace_chars.iter().enumerate().all(|(offset, expected)| {
                    buffer
                        .cell((start + offset as u16, 0))
                        .is_some_and(|cell| cell.symbol() == expected.to_string())
                })
            })
            .expect("workspace in command center");
        assert_eq!(
            buffer.cell((command_x, 0)).map(|cell| cell.bg),
            app.colors.ui.command_center_style(app.colors.ui.bg_title, true, false).bg
        );
    }

    #[test]
    fn split_constraints_cover_every_child_and_divider() {
        let constraints = split_constraints(3, &[0.2, 0.3, 0.5]);
        assert_eq!(constraints.len(), 5);
        assert!(matches!(constraints[1], Constraint::Length(1)));
        assert!(matches!(constraints[3], Constraint::Length(1)));
    }

    #[test]
    fn split_constraints_recover_from_missing_or_invalid_ratios() {
        let constraints = split_constraints(3, &[f32::NAN, -1.0]);
        assert_eq!(constraints.len(), 5);
        assert!(matches!(constraints[0], Constraint::Fill(weight) if weight > 0));
        assert!(matches!(constraints[2], Constraint::Fill(weight) if weight > 0));
        assert!(matches!(constraints[4], Constraint::Fill(weight) if weight > 0));
    }
}

#[cfg(test)]
#[path = "ui_outline_performance_tests.rs"]
mod outline_performance_tests;
