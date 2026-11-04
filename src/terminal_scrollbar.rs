//! Terminal chrome follows the emulator's retained rows, including ED 3 and
//! alternate-screen swaps. The gutter never belongs to the child PTY.
use crate::{app::App, app_terminal_selection::TerminalSurface, terminal::TermGrid};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect, style::Style};

#[derive(Default)]
pub(crate) struct State {
    pub areas: Vec<Surface>,
    drag: Option<Drag>,
}

impl State {
    pub fn cancel_drag(&mut self) {
        self.drag = None;
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Surface {
    terminal: TerminalSurface,
    area: Rect,
}

#[derive(Clone, Copy)]
struct Drag {
    surface: Surface,
    row: u16,
    last_row: u16,
    offset: usize,
}

struct Metrics {
    area: Rect,
    max: usize,
    offset: usize,
    thumb_top: u16,
    thumb_height: u16,
}

impl Metrics {
    fn new(grid: &TermGrid, area: Rect) -> Self {
        let max = grid.scrollback.len();
        let offset = max.saturating_sub(grid.scroll_view);
        let total = max.saturating_add(grid.height).max(1);
        let thumb_height = (usize::from(area.height).saturating_mul(grid.height) / total)
            .max(1)
            .min(usize::from(area.height)) as u16;
        let travel = area.height.saturating_sub(thumb_height);
        let thumb_top = if max == 0 {
            0
        } else {
            (offset as f64 / max as f64 * f64::from(travel)).round() as u16
        };
        Self {
            area,
            max,
            offset,
            thumb_top,
            thumb_height,
        }
    }

    fn offset_at(&self, row: u16) -> usize {
        let travel = self.area.height.saturating_sub(self.thumb_height);
        if travel == 0 {
            return 0;
        }
        let top = row
            .saturating_sub(self.area.y.saturating_add(self.thumb_height / 2))
            .min(travel);
        (f64::from(top) / f64::from(travel) * self.max as f64).round() as usize
    }

    fn drag_offset(&self, row: u16, start_row: u16, start_offset: usize) -> usize {
        let travel = self.area.height.saturating_sub(self.thumb_height);
        if travel == 0 {
            return start_offset.min(self.max);
        }
        // Keep the exact starting row even when a single thumb cell represents
        // thousands of history lines. A stationary drag must never jump.
        let delta = (f64::from(row) - f64::from(start_row)) / f64::from(travel) * self.max as f64;
        (start_offset as f64 + delta)
            .round()
            .clamp(0.0, self.max as f64) as usize
    }
}

/// Keep the gutter reserved even with no history, avoiding a PTY resize and
/// reflow whenever output starts scrolling or the shell clears its history.
pub(crate) fn content_area(area: Rect) -> Rect {
    Rect {
        width: area.width.saturating_sub(u16::from(area.width > 1)),
        ..area
    }
}

pub(crate) fn draw(f: &mut Frame, app: &mut App, terminal: TerminalSurface, outer: Rect) {
    if outer.width <= terminal.area.width || outer.height == 0 {
        return;
    }
    let surface = Surface {
        terminal,
        area: Rect::new(terminal.area.right(), outer.y, 1, outer.height),
    };
    let Some(pty) = app
        .terminals
        .get(terminal.index)
        .and_then(|tab| tab.terminal_pty.as_ref())
    else {
        return;
    };
    let metrics = Metrics::new(&pty.grid, surface.area);
    app.terminal_scrollbar.areas.push(surface);
    let hovered = surface.area.contains((app.mouse_col, app.mouse_row).into());
    let active = app
        .terminal_scrollbar
        .drag
        .is_some_and(|drag| drag.surface == surface);
    let (track, thumb) = app
        .colors
        .ui
        .scrollbar_colors(app.colors.ui.bg_terminal, hovered, active);
    for row in 0..surface.area.height {
        let color = if (metrics.thumb_top..metrics.thumb_top + metrics.thumb_height).contains(&row)
        {
            thumb
        } else {
            track
        };
        if let Some(cell) = f
            .buffer_mut()
            .cell_mut((surface.area.x, surface.area.y + row))
        {
            cell.set_char('▐')
                .set_style(Style::default().fg(color).bg(track));
        }
    }
}

fn metrics(app: &App, surface: Surface) -> Option<Metrics> {
    if !app.terminal_scrollbar.areas.contains(&surface)
        || !app.terminal_surfaces.contains(&surface.terminal)
    {
        return None;
    }
    app.terminals
        .get(surface.terminal.index)
        .and_then(|tab| tab.terminal_pty.as_ref())
        .map(|pty| Metrics::new(&pty.grid, surface.area))
}

fn scroll_to(app: &mut App, surface: Surface, metrics: &Metrics, offset: usize) {
    if let Some(pty) = app
        .terminals
        .get_mut(surface.terminal.index)
        .and_then(|tab| tab.terminal_pty.as_mut())
    {
        pty.scroll_to_view(metrics.max.saturating_sub(offset));
    }
}

/// The scrollbar owns its entire drag, including release over another pane or
/// a newly opened popup. Disappearing or resized surfaces cannot reuse it.
pub(crate) fn captured_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    let Some(drag) = app.terminal_scrollbar.drag else {
        return false;
    };
    if matches!(mouse.kind, MouseEventKind::Down(_)) {
        app.terminal_scrollbar.cancel_drag();
        return false;
    }
    if !matches!(
        mouse.kind,
        MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
    ) {
        return false;
    }
    if mouse.row != drag.last_row
        && let Some(metrics) = metrics(app, drag.surface)
    {
        let offset = metrics.drag_offset(mouse.row, drag.row, drag.offset);
        scroll_to(app, drag.surface, &metrics, offset);
    }
    if let Some(active) = app.terminal_scrollbar.drag.as_mut() {
        active.last_row = mouse.row;
    }
    app.mouse_col = mouse.column;
    app.mouse_row = mouse.row;
    app.mouse_hover_changed = true;
    if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
        app.terminal_scrollbar.cancel_drag();
    }
    true
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent, previous: (u16, u16)) -> bool {
    // Captured scrollbar drags are handled before hit testing. A different
    // gesture must keep its drag/release when it crosses this gutter.
    if matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_)) {
        return false;
    }
    let hit = app
        .terminal_scrollbar
        .areas
        .iter()
        .rev()
        .copied()
        .find(|surface| surface.area.contains((mouse.column, mouse.row).into()));
    if mouse.kind == MouseEventKind::Moved {
        app.mouse_hover_changed |= hit.is_some()
            || app
                .terminal_scrollbar
                .areas
                .iter()
                .any(|surface| surface.area.contains(previous.into()));
    }
    let Some(surface) = hit else {
        return false;
    };
    let Some(metrics) = metrics(app, surface) else {
        return false;
    };
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.clear_drag_states();
            app.drag_start_pos = None;
            app.drag_has_moved = false;
            let row = mouse.row.saturating_sub(surface.area.y);
            let offset =
                if (metrics.thumb_top..metrics.thumb_top + metrics.thumb_height).contains(&row) {
                    metrics.offset
                } else {
                    metrics.offset_at(mouse.row)
                };
            scroll_to(app, surface, &metrics, offset);
            if metrics.max > 0 && metrics.thumb_height < surface.area.height {
                app.terminal_scrollbar.drag = Some(Drag {
                    surface,
                    row: mouse.row,
                    last_row: mouse.row,
                    offset,
                });
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            if let Some(pty) = app.terminals[surface.terminal.index].terminal_pty.as_mut() {
                if mouse.kind == MouseEventKind::ScrollUp {
                    pty.scroll_up(app.scroll_speed);
                } else {
                    pty.scroll_down(app.scroll_speed);
                }
            }
        }
        _ => {}
    }
    true
}

#[cfg(test)]
#[path = "terminal_scrollbar_tests.rs"]
mod tests;
