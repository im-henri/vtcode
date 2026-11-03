//! The right terminal dock owns its divider gesture independently of terminal
//! input, so dragging across a terminal or editor cannot become a selection.
use crate::app::App;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect};

const MIN_WIDTH: u16 = 12;
const WORKBENCH_RESERVE: u16 = 24;

#[derive(Default)]
pub(crate) struct State {
    /// None retains the responsive default until the user resizes the dock.
    pub width: Option<u16>,
    pub handle: Option<Rect>,
    last_column: Option<u16>,
}

impl State {
    pub fn dragging(&self) -> bool {
        self.last_column.is_some()
    }
    pub fn cancel_drag(&mut self) {
        self.last_column = None;
    }
    // Closing or hiding the dock must still consume the gesture's release.
    pub fn hide(&mut self) {
        self.handle = None;
    }
    pub fn restore_width(&mut self, width: Option<u16>) {
        self.width = width.map(|width| width.max(MIN_WIDTH));
    }
}

pub(crate) fn width(available: u16, expanded: bool, requested: Option<u16>) -> u16 {
    if available <= 1 {
        return 0;
    }
    if !expanded {
        return 3.min(available - 1);
    }
    let Some(requested) = requested else {
        return available.div_ceil(3).min(56).min(available - 1);
    };
    // Explicit resizing can use more room than the default one-third dock.
    // Small windows still share space proportionally with the workbench.
    let reserve = (available.saturating_mul(2) / 3).min(WORKBENCH_RESERVE);
    let maximum = available.saturating_sub(reserve).max(1);
    requested.clamp(MIN_WIDTH.min(maximum), maximum)
}

/// Reserve the divider outside terminal cells, header buttons and tab targets.
pub(crate) fn content_area(frame: &mut Frame, app: &mut App, area: Rect) -> Rect {
    app.terminal_sidebar.handle = None;
    if !app.show_terminal_sidebar || area.width < 2 || area.height == 0 {
        return area;
    }
    let handle = Rect::new(area.x, area.y, 1, area.height);
    app.terminal_sidebar.handle = Some(handle);
    let hovered = handle.contains((app.mouse_col, app.mouse_row).into());
    let layer = if hovered || app.terminal_sidebar.dragging() {
        app.colors.ui.sash_hover_border
    } else {
        app.colors.ui.panel_border
    };
    let background = app.colors.ui.bg_panel;
    let color = layer.border(background);
    for y in handle.y..handle.bottom() {
        if let Some(cell) = frame.buffer_mut().cell_mut((handle.x, y)) {
            cell.set_char(if color.is_some() { '│' } else { ' ' });
            cell.set_fg(color.unwrap_or(background)).set_bg(background);
        }
    }
    Rect::new(area.x + 1, area.y, area.width - 1, area.height)
}

pub(crate) fn begin_resize(app: &mut App, mouse: MouseEvent) -> bool {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left)
        || !app.show_terminal_sidebar
        || !app
            .terminal_sidebar
            .handle
            .is_some_and(|area| area.contains((mouse.column, mouse.row).into()))
    {
        return false;
    }
    app.clear_drag_states();
    app.drag_start_pos = Some((mouse.column, mouse.row));
    app.terminal_sidebar.last_column = Some(mouse.column);
    true
}

/// Retain capture through release, even outside the dock or over a mouse-aware
/// application. No terminal selection, input or tab drag sees these samples.
pub(crate) fn captured_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    let Some(previous) = app.terminal_sidebar.last_column else {
        return false;
    };
    if matches!(mouse.kind, MouseEventKind::Down(_)) {
        app.clear_drag_states();
        app.drag_start_pos = None;
        return false;
    }
    if !matches!(
        mouse.kind,
        MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
    ) {
        return false;
    }
    if app.show_terminal_sidebar
        && app.terminal_sidebar.handle.is_some()
        && mouse.column != previous
        && let Some(body) = app.workbench_body_area
    {
        app.terminal_sidebar.width = Some(width(
            body.width,
            true,
            Some(body.right().saturating_sub(mouse.column)),
        ));
        app.terminal_sidebar.last_column = Some(mouse.column);
    }
    app.mouse_col = mouse.column;
    app.mouse_row = mouse.row;
    app.mouse_hover_changed = true;
    if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
        app.clear_drag_states();
        app.drag_start_pos = None;
        app.update_sidebar_auto_hide(mouse);
    }
    true
}

#[cfg(test)]
#[path = "terminal_sidebar_tests.rs"]
mod tests;
