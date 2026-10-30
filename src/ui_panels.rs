use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Paragraph},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, TerminalDock};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

#[path = "ui_panel_problems.rs"]
mod problems;

/// Bottom panel chrome is independent of editor tabs and terminal sessions.
#[derive(Default)]
pub(crate) struct Chrome {
    pub maximized: bool,
    pub close: Rect,
    pub maximize: Rect,
    pointer: Option<MouseButton>,
    scrollbar: Option<ListScrollbar>,
    scroll_drag: Option<(u8, crate::settings_layout::Drag)>,
    notification_top: Option<u64>,
}

#[derive(Clone, Copy)]
struct ListScrollbar {
    area: Rect,
    tab: u8,
    maximum: usize,
    offset: usize,
    thumb_top: u16,
    thumb: u16,
}

fn notification_offset(app: &App, height: usize) -> usize {
    let maximum = app.notification_history.len().saturating_sub(height);
    app.panel_chrome.notification_top.map_or(maximum, |top| {
        app.notification_history.partition_point(|(_, _, sequence)| *sequence < top).min(maximum)
    })
}

fn scroll_to(app: &mut App, tab: u8, offset: usize, maximum: usize) {
    let offset = offset.min(maximum);
    if tab == 1 { app.problem_scroll = offset; }
    if tab == 3 {
        app.panel_chrome.notification_top = if offset == maximum { None }
            else { app.notification_history.get(offset).map(|item| item.2) };
    }
}

pub(crate) fn scroll_input(app: &mut App, mouse: MouseEvent) -> bool {
    if !app.show_panel || !app.panel_area.is_some_and(|area| area.contains((mouse.column, mouse.row).into())) {
        return false;
    }
    let down = match mouse.kind {
        MouseEventKind::ScrollUp => false,
        MouseEventKind::ScrollDown => true,
        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => return true,
        _ => return false,
    };
    let panel = app.panel_area.unwrap();
    // Header and horizontal gestures never fall through to a file or terminal.
    if mouse.row == panel.y || mouse.modifiers.contains(crossterm::event::KeyModifiers::SHIFT) { return true; }
    let height = panel.height.saturating_sub(1) as usize;
    let (offset, total) = match app.panel_tab {
        1 => (app.problem_scroll, problems::layout(app).total),
        3 => (notification_offset(app, height), app.notification_history.len()),
        _ => return true, // Output's text view has already handled its own wheel.
    };
    let maximum = total.saturating_sub(height);
    let offset = offset.min(maximum);
    let next = if down { offset.saturating_add(app.scroll_speed) } else { offset.saturating_sub(app.scroll_speed) };
    scroll_to(app, app.panel_tab, next, maximum);
    true
}

pub(crate) fn scrollbar_press(app: &mut App, col: u16, row: u16) -> bool {
    let Some(bar) = app.panel_chrome.scrollbar.filter(|bar| bar.tab == app.panel_tab && bar.area.contains((col, row).into())) else { return false; };
    capture_pointer(app, MouseButton::Left);
    let relative = row - bar.area.y;
    let offset = if (bar.thumb_top..bar.thumb_top + bar.thumb).contains(&relative) { bar.offset }
        else {
            let travel = bar.area.height.saturating_sub(bar.thumb);
            ((relative.saturating_sub(bar.thumb / 2).min(travel) as u128 * bar.maximum as u128)
                / travel.max(1) as u128) as usize
        };
    scroll_to(app, bar.tab, offset, bar.maximum);
    app.panel_chrome.scroll_drag = Some((bar.tab, crate::settings_layout::Drag { row, offset }));
    true
}

fn draw_list_scrollbar(f: &mut Frame, app: &mut App, area: Rect, total: usize, offset: usize, bg: Color) {
    if area.height == 0 || area.width < 2 || total <= area.height as usize { return; }
    let rect = Rect::new(area.right() - 1, area.y, 1, area.height);
    let hovered = rect.contains((app.mouse_col, app.mouse_row).into());
    let (track, thumb) = app.colors.ui.scrollbar_colors(bg, hovered, app.panel_chrome.scroll_drag.is_some());
    let bar = crate::ui_widgets::VerticalScrollbar::new(rect.x, rect.y, rect.height, total, offset).colors(thumb, track, bg);
    let (thumb_top, thumb_size) = bar.thumb();
    app.panel_chrome.scrollbar = Some(ListScrollbar { area: rect, tab: app.panel_tab, maximum: total - area.height as usize, offset, thumb_top, thumb: thumb_size });
    bar.render(f);
}

pub(crate) fn hovered_problem(app: &mut App, col: u16, row: u16) -> Option<usize> {
    match problems::hit(app, col, row) { Some(problems::Row::Marker(index)) => Some(index), _ => None }
}

pub(crate) fn activate_problem(app: &mut App, col: u16, row: u16) { problems::activate(app, col, row); }

pub(crate) fn hide_for_layout(app: &mut App) {
    app.show_panel = false;
    app.logs.hide();
    app.panel_chrome.scrollbar = None;
    app.panel_chrome.scroll_drag = None;
    if app.active_pane == crate::app::ActivePane::Panel { app.active_pane = crate::app::ActivePane::Editor; }
}

pub(crate) fn invalidate_layout(app: &mut App) {
    app.panel_area = None;
    app.panel_tab_areas.clear();
    app.panel_chrome.close = Rect::default();
    app.panel_chrome.maximize = Rect::default();
    app.panel_chrome.scrollbar = None;
    app.panel_chrome.scroll_drag = None;
    app.logs.hide();
    if app.active_pane == crate::app::ActivePane::Panel { app.active_pane = crate::app::ActivePane::Editor; }
}

pub(crate) fn show_problems(app: &mut App) {
    app.show_panel = true;
    select_tab(app, 1);
}

pub(crate) fn toggle_problems(app: &mut App) {
    if app.show_panel && app.panel_tab == 1 { close_panel(app); }
    else { show_problems(app); }
}

/// Chrome and non-selectable panel content own their complete press gesture,
/// even when navigation, closing the panel, or a popup changes the layout.
pub(crate) fn capture_pointer(app: &mut App, button: MouseButton) {
    app.clear_drag_states();
    app.drag_start_pos = None;
    app.drag_has_moved = false;
    app.dragging_gutter = false;
    app.dragging_gutter_start_line = None;
    app.middle_drag_start = None;
    app.block_drag_cache = None;
    app.diff_selection.cancel_drag();
    app.panel_chrome.scroll_drag = None;
    app.panel_chrome.pointer = Some(button);
}

pub(crate) fn captured_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if matches!(mouse.kind, MouseEventKind::Down(_)) {
        app.panel_chrome.pointer = None;
        app.panel_chrome.scroll_drag = None;
        return false;
    }
    match mouse.kind {
        MouseEventKind::Drag(button) | MouseEventKind::Up(button)
            if app.panel_chrome.pointer == Some(button) =>
        {
            if let Some((tab, drag)) = app.panel_chrome.scroll_drag
                && tab == app.panel_tab && app.show_panel
                && let Some(bar) = app.panel_chrome.scrollbar.filter(|bar| bar.tab == tab)
            {
                let offset = drag.offset_at(mouse.row, bar.area.height.saturating_sub(bar.thumb) as usize, bar.maximum);
                scroll_to(app, tab, offset, bar.maximum);
            }
            if matches!(mouse.kind, MouseEventKind::Up(_)) {
                app.panel_chrome.pointer = None;
                app.panel_chrome.scroll_drag = None;
            }
            app.mouse_col = mouse.column;
            app.mouse_row = mouse.row;
            app.mouse_hover_changed = true;
            true
        }
        _ => false,
    }
}

pub(crate) const PANEL_LABELS: [(u8, &str); 3] = [(1, "Problems"), (2, "Output"), (3, "Notifications")];

/// Shorten labels before hiding tabs, and keep the selected tab reachable.
pub(crate) fn panel_tab_layout(area: Rect, active: u8) -> Vec<(Rect, u8, &'static str)> {
    if area.is_empty() { return Vec::new(); }
    let actions = if area.width >= 12 { 6 } else { 3 };
    let more = if active == 2 && area.width >= 15 { 3 } else { 0 };
    let available = area.width.saturating_sub(actions + more) as usize;
    let labels = [PANEL_LABELS, [(1, "Problems"), (2, "Output"), (3, "Alerts")], [(1, "P"), (2, "O"), (3, "N")]]
        .into_iter().find(|labels| labels.iter().map(|(_, label)| UnicodeWidthStr::width(*label) + 2).sum::<usize>() <= available);
    let mut tabs = Vec::with_capacity(3);
    if let Some(labels) = labels {
        let mut x = area.x;
        for (index, label) in labels {
            let width = (UnicodeWidthStr::width(label) + 2) as u16;
            tabs.push((Rect::new(x, area.y, width, 1), index, label));
            x += width;
        }
    } else if available > 0 {
        let active = App::normalize_panel_tab(active);
        let next_width = if available >= 5 { 2 } else { 0 };
        let label = PANEL_LABELS.iter().find(|(index, _)| *index == active).unwrap().1;
        tabs.push((Rect::new(area.x, area.y, (available - next_width) as u16, 1), active, label));
        if next_width > 0 {
            tabs.push((Rect::new(area.x + (available - next_width) as u16, area.y, 2, 1), active % 3 + 1, "›"));
        }
    }
    tabs
}

fn draw_panel_header(f: &mut Frame, app: &mut App, area: Rect) {
    let bg = app.colors.ui.bg_panel;
    f.render_widget(Block::default().style(Style::default().bg(bg)), area);
    app.panel_tab_areas.clear();
    app.panel_chrome.close = Rect::default();
    app.panel_chrome.maximize = Rect::default();
    if area.is_empty() { return; }
    let mut tabs_end = area.x;
    for (tab_area, index, label) in panel_tab_layout(area, app.panel_tab) {
        let active = app.panel_tab == index;
        let hovered = tab_area.contains((app.mouse_col, app.mouse_row).into());
        let fg = if active { app.colors.ui.fg_panel_title } else { app.colors.ui.fg_panel_inactive };
        let mut style = app.colors.ui.toolbar_style(fg, bg, hovered, active, false);
        if active { style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED).underline_color(app.colors.ui.border_panel_active); }
        let label = crate::ui_performance::clip_middle(label, tab_area.width.saturating_sub(2).max(1) as usize);
        f.render_widget(Paragraph::new(if tab_area.width > 2 { format!(" {label} ") } else { label }).style(style), tab_area);
        app.panel_tab_areas.push((tab_area, index));
        tabs_end = tabs_end.max(tab_area.right());
    }
    let width = area.width.min(3);
    let close = Rect::new(area.right() - width, area.y, width, 1);
    app.panel_chrome.close = close;
    let maximize = if area.width >= 12 { Rect::new(close.x - 3, area.y, 3, 1) } else { Rect::default() };
    app.panel_chrome.maximize = maximize;
    for (rect, label) in [(maximize, if app.panel_chrome.maximized { " ▿ " } else { " ▴ " }), (close, if width == 3 { " ✕ " } else { "✕" })] {
        if rect.is_empty() { continue; }
        f.render_widget(Paragraph::new(label).style(app.colors.ui.toolbar_style(app.colors.ui.fg_panel_inactive, bg,
            rect.contains((app.mouse_col, app.mouse_row).into()), false, false)), rect);
    }
    if app.panel_tab == 2 {
        let right = if maximize.is_empty() { close.x } else { maximize.x };
        let start = tabs_end.saturating_add(1).min(right);
        crate::ui_logs::draw_toolbar(f, app, Rect::new(start, area.y, right - start, 1));
    }
}

pub(crate) fn close_panel(app: &mut App) {
    hide_for_layout(app);
    app.panel_chrome.maximized = false;
}

pub(crate) fn draw_tooltip(frame: &mut Frame, app: &App) {
    if !app.show_panel || app.menu_active || app.remote_connection.details
        || app.side_notice.area.is_some_and(|area| area.contains((app.mouse_col, app.mouse_row).into()))
        || crate::ui_modal::active(app).is_some() || crate::ui_quick_picker::active(app).is_some() { return; }
    let point = (app.mouse_col, app.mouse_row).into();
    let hint = if app.panel_chrome.close.contains(point) { Some("Close Panel".into()) }
        else if app.panel_chrome.maximize.contains(point) { Some(if app.panel_chrome.maximized { "Restore Panel Size" } else { "Maximize Panel" }.into()) }
        else { crate::ui_logs::hint(app) };
    if let Some(hint) = hint {
        crate::ui_widgets::Tooltip::hover(vec![Line::raw(hint)], &app.colors)
            .max_width(76).render(frame, app.mouse_col, app.mouse_row);
    }
}

pub(crate) fn select_tab(app: &mut App, index: u8) {
    app.panel_chrome.scrollbar = None;
    app.panel_chrome.scroll_drag = None;
    app.panel_tab = App::normalize_panel_tab(index);
    app.terminal_panel_focused = false;
    if app.panel_tab == 2 { app.show_output(); }
    else {
        app.logs.hide();
        if app.active_pane == crate::app::ActivePane::Panel { app.active_pane = crate::app::ActivePane::Editor; }
    }
}

pub(crate) fn draw_bottom_panel(f: &mut Frame, app: &mut App, area: Rect) {
    app.panel_chrome.scrollbar = None;
    app.panel_tab = App::normalize_panel_tab(app.panel_tab);
    let header = Rect::new(area.x, area.y, area.width, area.height.min(1));
    let content = Rect::new(area.x, header.bottom(), area.width, area.height.saturating_sub(header.height));
    f.render_widget(Block::default().style(Style::default().bg(app.colors.ui.bg_panel)), area);
    if app.panel_tab == 2 { crate::ui_logs::draw_content(f, app, content); }
    draw_panel_header(f, app, header);
    match app.panel_tab {
        1 => draw_panel_problems(f, app, content),
        3 => {
            draw_panel_notifications(f, app, content);
            let offset = notification_offset(app, content.height as usize);
            draw_list_scrollbar(f, app, content, app.notification_history.len(), offset, app.colors.ui.bg_notification);
        }
        _ => {}
    }
}

/// Render the terminal's default home as a collapsible right-side workbench tab.
pub(crate) fn draw_terminal_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    app.terminal_sidebar_toggle_area = None;
    app.terminal_sidebar_plus_area = None;
    app.terminal_sidebar_tab_areas.clear();
    app.terminal_sidebar_close_areas.clear();
    if area.is_empty() {
        return;
    }

    f.render_widget(
        Block::default().style(Style::default().bg(app.colors.ui.bg_panel)),
        area,
    );

    let toggle_width = area.width.min(3);
    let toggle_x = area.x.saturating_add(area.width.saturating_sub(toggle_width));
    let toggle_area = Rect::new(toggle_x, area.y, toggle_width, 1);
    app.terminal_sidebar_toggle_area = Some(toggle_area);

    if !app.show_terminal_sidebar {
        let toggle = crate::ui_pickers::truncate_to_width(" ‹ ", area.width as usize);
        f.render_widget(
            Paragraph::new(toggle).style(
                app.colors.ui.toolbar_style(app.colors.ui.fg_panel_title, app.colors.ui.bg_panel, app.hovered_terminal_sidebar_toggle, false, false),
            ),
            toggle_area,
        );
        for (offset, ch) in "TERM".chars().enumerate() {
            let y = area.y.saturating_add(2 + offset as u16);
            if y >= area.y.saturating_add(area.height) {
                break;
            }
            f.buffer_mut().set_string(
                area.x.saturating_add(area.width.saturating_sub(1) / 2),
                y,
                ch.to_string(),
                Style::default()
                    .fg(app.colors.ui.description_color(app.colors.ui.bg_panel))
                    .bg(app.colors.ui.bg_panel),
            );
        }
        return;
    }

    let header_width = area.width.saturating_sub(6) as usize;
    let title = crate::ui_pickers::truncate_to_width(" TERMINAL  Ctrl+`", header_width);
    f.buffer_mut().set_string(
        area.x,
        area.y,
        title,
        Style::default()
            .fg(app.colors.ui.readable_foreground(app.colors.ui.fg_panel_inactive, app.colors.ui.bg_panel, 4.5))
            .bg(app.colors.ui.bg_panel),
    );

    let plus_x = toggle_x.saturating_sub(3);
    if plus_x >= area.x {
        let plus_area = Rect::new(plus_x, area.y, 3.min(area.width), 1);
        app.terminal_sidebar_plus_area = Some(plus_area);
        f.render_widget(
            Paragraph::new(" + ").style(
                app.colors.ui.toolbar_style(app.colors.ui.fg_panel_title, app.colors.ui.bg_panel, app.hovered_terminal_plus, false, false),
            ),
            plus_area,
        );
    }
    f.render_widget(
        Paragraph::new(" › ").style(
            app.colors.ui.toolbar_style(app.colors.ui.fg_panel_title, app.colors.ui.bg_panel, app.hovered_terminal_sidebar_toggle, false, false),
        ),
        toggle_area,
    );

    if area.height < 3 {
        return;
    }
    let right_indices: Vec<usize> = (0..app.terminals.len())
        .filter(|&index| app.terminal_is_in_dock(index, TerminalDock::Right))
        .collect();
    let selected = app
        .active_terminal_in_dock(TerminalDock::Right)
        .or_else(|| right_indices.first().copied());

    let mut ordered = right_indices;
    if let Some(selected_index) = selected {
        if let Some(position) = ordered.iter().position(|&index| index == selected_index) {
            ordered.swap(0, position);
        }
    }
    let mut x = area.x;
    let tab_right = area.x.saturating_add(area.width);
    let now = std::time::Instant::now();
    for index in ordered {
        let remaining = tab_right.saturating_sub(x) as usize;
        if remaining < 6 {
            break;
        }
        let notice = &app.terminals[index].completion;
        let notified = notice.visible(now);
        // Keep a fixed two-cell prefix so completing/acknowledging a command
        // never moves this tab's close button or its neighbours.
        let max_name = remaining.saturating_sub(5).clamp(1, 20);
        let name = crate::ui_pickers::truncate_to_width(&app.terminals[index].name, max_name);
        let label_width = (name.width() + 3) as u16;
        let tab_area = Rect::new(x, area.y + 1, label_width, 1);
        let is_active = Some(index) == selected;
        let background = crate::ui_chrome_tabs::terminal_completion_background(app, Some(index), app.colors.ui.bg_panel, now);
        let mut style = app.colors.ui.toolbar_style(
            if is_active { app.colors.ui.fg_panel_title } else { app.colors.ui.fg_panel_inactive },
            background, app.hovered_terminal_tab == Some(index), false, false,
        );
        if is_active { style = style.add_modifier(Modifier::UNDERLINED).underline_color(app.colors.ui.border_terminal_tab); }
        let color = if notice.failed() { app.colors.fg_error } else { app.colors.fg_success };
        let label = vec![
            Span::styled(if notified { "● " } else { "  " },
                style.fg(app.colors.ui.readable_foreground(color, background, 3.0))),
            Span::styled(format!("{name} "), style),
        ];
        f.render_widget(Paragraph::new(Line::from(label)).style(style), tab_area);
        app.terminal_sidebar_tab_areas.push((tab_area, index));
        x = x.saturating_add(label_width);

        if x.saturating_add(2) > tab_right {
            break;
        }
        let close_area = Rect::new(x, area.y + 1, 2, 1);
        f.render_widget(
            Paragraph::new("× ").style(
                app.colors.ui.toolbar_style(
                    app.colors.ui.fg_panel_inactive, app.colors.ui.bg_panel,
                    app.hovered_terminal_close == Some(index), false, false,
                ),
            ),
            close_area,
        );
        app.terminal_sidebar_close_areas.push((close_area, index));
        x = x.saturating_add(2);
    }

    let content = Rect::new(
        area.x,
        area.y + 2,
        area.width,
        area.height.saturating_sub(2),
    );
    if let Some(index) = selected {
        draw_docked_terminal(f, app, index, content);
    } else {
        f.render_widget(
            Paragraph::new(" No terminals on the right. Use + to create one.").style(
                Style::default()
                    .fg(app.colors.ui.description_color(app.colors.ui.bg_panel))
                    .bg(app.colors.ui.bg_panel),
            ),
            content,
        );
    }
}

fn problem_row<'a>(spans: Vec<Span<'a>>, background: Color, width: u16) -> Line<'a> {
    let mut line = Line::from(spans);
    let fill = (width as usize).saturating_sub(line.width());
    line.spans.push(Span::styled(crate::helpers::spaces(fill), Style::default().bg(background)));
    line
}

pub(crate) fn draw_panel_problems(f: &mut Frame, app: &mut App, area: Rect) {
    problems::draw(f, app, area);
}

fn draw_docked_terminal(f: &mut Frame, app: &mut App, index: usize, area: Rect) {
    if area.is_empty() || index >= app.terminals.len() {
        return;
    }
    let width = area.width as usize;
    let height = area.height as usize;
    app.ensure_terminal_pty_at(index, width, height);

    if let Some(ref mut pty) = app.terminals[index].terminal_pty {
        pty.resize(width, height);
    }
    if let Some(ref pty) = app.terminals[index].terminal_pty {
        app.terminal_surfaces.push(crate::app_terminal_selection::TerminalSurface {
            index, area, group: None,
        });
        crate::ui_helpers::draw_terminal_emulator(
            f,
            pty,
            area,
            &app.colors.ui,
            app.cursor_visible && app.active_terminal == index && app.terminal_panel_focused,
        );
        return;
    }

    crate::ui_helpers::draw_terminal_launch_error(
        f,
        app.terminals[index].terminal_launch_error.as_deref(),
        area,
        app.colors.fg_text,
        app.colors.fg_dim,
        app.colors.fg_error,
        app.colors.ui.bg_panel,
    );
}

pub(crate) fn draw_panel_notifications(f: &mut Frame, app: &App, area: Rect) {
    use crate::app::NotifyLevel;

    if app.notification_history.is_empty() {
        let p = Paragraph::new(Line::from(Span::styled(
            " No notifications yet",
            Style::default().fg(app.colors.ui.description_color(app.colors.ui.bg_panel)).bg(app.colors.ui.bg_panel),
        )));
        f.render_widget(p, area);
        return;
    }

    let max_visible = area.height as usize;
    let skip = notification_offset(app, max_visible);

    let lines: Vec<Line<'_>> = app
        .notification_history
        .iter()
        .skip(skip)
        .take(max_visible)
        .map(|(msg, level, seq)| {
            let (icon, severity) = match level {
                NotifyLevel::Info => ("ℹ ", 0),
                NotifyLevel::Warning => ("⚠ ", 1),
                NotifyLevel::Error => ("✗ ", 2),
            };
            let icon_color = app.colors.ui.notification_icon_color(severity, app.colors.ui.bg_notification);
            let seq_label = format!(" #{:<4} ", seq + 1);
            Line::from(vec![
                Span::styled(
                    seq_label,
                    Style::default()
                        .fg(app.colors.ui.description_color(app.colors.ui.bg_notification))
                        .bg(app.colors.ui.bg_notification),
                ),
                Span::styled(icon, Style::default().fg(icon_color).bg(app.colors.ui.bg_notification)),
                Span::styled(
                    msg.clone(),
                    Style::default()
                        .fg(app.colors.ui.readable_foreground(app.colors.ui.fg_notification, app.colors.ui.bg_notification, 4.5))
                        .bg(app.colors.ui.bg_notification),
                ),
            ])
        })
        .collect();

    let p = Paragraph::new(lines).style(Style::default().bg(app.colors.ui.bg_notification));
    f.render_widget(p, area);
    if let Some(border) = app.colors.ui.border_notification {
        for y in area.y..area.bottom() {
            if let Some(cell) = f.buffer_mut().cell_mut((area.x, y)) {
                cell.set_symbol("│");
                cell.set_fg(border);
                cell.set_bg(app.colors.ui.bg_notification);
            }
        }
    }
}

pub(crate) fn draw_status_bar(f: &mut Frame, app: &mut App, area: Rect) {
    crate::ui_status::draw_bar(f, app, area);
}

#[cfg(test)]
#[path = "ui_panel_performance_tests.rs"]
mod performance_tests;

#[cfg(test)]
#[path = "ui_panel_mouse_tests.rs"]
mod mouse_tests;

#[cfg(test)]
#[path = "ui_panel_combination_tests.rs"]
mod combination_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Tab;
    use crate::editor::Editor;
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn bottom_panel_offers_output_problems_and_notifications_at_every_width() {
        for width in [1, 3, 4, 7, 10, 18, 32, 80] {
            for selected in [0, 1, 2, 3, 255] {
                let mut app = App::test_new();
                app.panel_tab = selected;
                let mut terminal = Terminal::new(TestBackend::new(width, 6)).unwrap();
                terminal.draw(|frame| draw_bottom_panel(frame, &mut app, frame.area())).unwrap();
                assert_eq!(app.panel_tab, App::normalize_panel_tab(selected));
                for (rect, index) in &app.panel_tab_areas {
                    assert!(matches!(index, 1 | 2 | 3));
                    assert!(rect.right() <= width.saturating_sub(3));
                }
                let header: String = (0..width).map(|x| terminal.backend().buffer()[(x, 0)].symbol()).collect();
                assert!(!header.contains("Terminal"));
                if width >= 48 {
                    assert!(header.contains("Problems") && header.contains("Output") && header.contains("Notifications"));
                }
            }
        }
    }

    #[test]
    fn menus_and_palette_cannot_reopen_removed_panel_surfaces() {
        let app = App::test_new();
        assert!(App::build_command_list().iter().all(|command| command.id != "move_terminal_to_bottom"));
        assert!(app.menu_items(6).iter().all(|(_, id)| *id != "move_terminal_to_bottom"));
    }

    #[test]
    fn status_selection_stats_are_compact_and_restore_after_reselection() {
        let backend = TestBackend::new(200, 1);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("alpha beta\ngamma delta\n");
        editor.selection_anchor = Some((0, 0));
        editor.cursor_line = 1;
        editor.cursor_col = 5;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;

        terminal
            .draw(|frame| draw_status_bar(frame, &mut app, Rect::new(0, 0, 200, 1)))
            .expect("selected status draw");
        let first = app.cached_sel_stats.clone().expect("selection stats");
        assert!(first.contains("selected"));
        assert!(!first.contains("words"));

        app.editor_mut().unwrap().selection_anchor = None;
        terminal
            .draw(|frame| draw_status_bar(frame, &mut app, Rect::new(0, 0, 200, 1)))
            .expect("cleared status draw");
        assert!(app.cached_sel_stats.is_none());

        app.editor_mut().unwrap().selection_anchor = Some((0, 0));
        terminal
            .draw(|frame| draw_status_bar(frame, &mut app, Rect::new(0, 0, 200, 1)))
            .expect("reselected status draw");
        assert_eq!(app.cached_sel_stats.as_deref(), Some(first.as_str()));
    }

    #[test]
    fn right_terminal_header_has_single_press_collapse_control() {
        let backend = TestBackend::new(40, 3);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        let mut app = App::test_new();
        app.show_terminal_sidebar = true;
        app.terminals[0].terminal_launch_error = Some("test".into());

        terminal
            .draw(|frame| draw_terminal_sidebar(frame, &mut app, Rect::new(0, 0, 40, 3)))
            .expect("right terminal draw");

        assert!(app.terminal_sidebar_toggle_area.is_some());
        assert!(app.terminal_sidebar_plus_area.is_some());
        assert_eq!(app.terminal_sidebar_tab_areas.len(), 1);
        assert_eq!(app.terminal_sidebar_close_areas.len(), 1);
    }
}
