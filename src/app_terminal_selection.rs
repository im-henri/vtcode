//! Mouse capture and clipboard routing for regular workbench terminals.
use crate::app::{ActivePane, App, ContextMenuItem, TabKind};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TerminalSurface {
    pub index: usize,
    pub area: Rect,
    pub group: Option<usize>,
}

#[derive(Default)]
pub(crate) struct TerminalClicks {
    serial: u64,
    last: Option<TerminalClick>,
}

struct TerminalClick {
    surface: TerminalSurface,
    position: (u16, u16),
    time: std::time::Instant,
    serial: u64,
    count: u8,
}

impl TerminalClicks {
    /// Count every workbench press, including clicks intercepted by overlays.
    pub(crate) fn press(&mut self) { self.serial = self.serial.wrapping_add(1); }

    fn count(&mut self, surface: TerminalSurface, mouse: MouseEvent) -> u8 {
        let time = std::time::Instant::now();
        let position = (mouse.column, mouse.row);
        let count = self.last.as_ref().filter(|last| last.surface == surface
            && last.position == position && last.serial.wrapping_add(1) == self.serial
            && time.duration_since(last.time) < std::time::Duration::from_millis(400))
            .map_or(1, |last| (last.count + 1).min(3));
        self.last = Some(TerminalClick { surface, position, time, serial: self.serial, count });
        count
    }
}

impl App {
    pub(crate) fn focused_terminal_index(&self) -> Option<usize> {
        let index = if self.docked_terminal_has_focus() {
            Some(self.active_terminal)
        } else if self.active_pane == ActivePane::Editor {
            self.tabs
                .get(self.active_tab)
                .filter(|tab| tab.kind == TabKind::Terminal)
                .and_then(|tab| tab.terminal_idx)
        } else {
            None
        }?;
        (index < self.terminals.len()).then_some(index)
    }

    pub(crate) fn focused_terminal_has_selection(&self) -> bool {
        self.focused_terminal_index()
            .and_then(|index| self.terminals[index].terminal_pty.as_ref())
            .is_some_and(|pty| pty.grid.has_selection())
    }

    pub(crate) fn forward_terminal_focus(&self, focused: bool) {
        if let Some(pty) = self
            .focused_terminal_index()
            .and_then(|index| self.terminals[index].terminal_pty.as_ref())
            && pty.grid.focus_reporting
        {
            pty.write(if focused { b"\x1b[I" } else { b"\x1b[O" });
        }
    }

    pub(crate) fn terminal_wants_mouse(&self, mouse: MouseEvent) -> bool {
        (self.terminal_mouse_capture.is_some()
            && matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_)))
            || (!mouse.modifiers.contains(KeyModifiers::SHIFT)
                && self
                    .terminal_surfaces
                    .iter()
                    .rev()
                    .find(|surface| surface.area.contains((mouse.column, mouse.row).into()))
                    .and_then(|surface| self.terminals.get(surface.index))
                    .and_then(|terminal| terminal.terminal_pty.as_ref())
                    .is_some_and(|pty| pty.grid.mouse_tracking != 0))
    }

    fn forward_terminal_mouse_at(&mut self, surface: TerminalSurface, mouse: MouseEvent) {
        if let Some(pty) = self
            .terminals
            .get_mut(surface.index)
            .and_then(|terminal| terminal.terminal_pty.as_mut())
        {
            let local = MouseEvent {
                column: mouse
                    .column
                    .saturating_sub(surface.area.x)
                    .min(surface.area.width.saturating_sub(1)),
                row: mouse
                    .row
                    .saturating_sub(surface.area.y)
                    .min(surface.area.height.saturating_sub(1)),
                ..mouse
            };
            if let Some(bytes) = pty.grid.mouse_sequence(local) {
                pty.grid.scroll_view = 0;
                pty.write(&bytes);
            }
        }
    }

    fn focus_terminal_surface(&mut self, surface: TerminalSurface) {
        if let Some(group) = surface.group {
            self.switch_group(group);
            if let Some(tab) = self
                .tabs
                .iter()
                .position(|tab| tab.terminal_idx == Some(surface.index))
            {
                self.active_tab = tab;
            }
        }
        self.active_terminal = surface.index;
        self.active_pane = ActivePane::Editor;
        self.terminal_panel_focused = surface.group.is_none();
        self.search_input_focused = false;
        self.search_pre_cursor = None;
        self.drag_start_pos = None;
        self.drag_has_moved = false;
    }

    /// Capture a selection until release, even when the pointer leaves its pane.
    pub(crate) fn handle_terminal_selection_mouse(&mut self, mouse: MouseEvent) -> bool {
        if matches!(mouse.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown)
            || (matches!(mouse.kind, MouseEventKind::Drag(_))
                && self.terminal_clicks.last.as_ref().is_some_and(|last| last.position != (mouse.column, mouse.row))) {
            self.terminal_clicks.last = None;
        }
        if let Some(capture) = self.terminal_mouse_capture {
            if matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_)) {
                if self.terminal_surfaces.contains(&capture) {
                    self.forward_terminal_mouse_at(capture, mouse);
                }
                if matches!(mouse.kind, MouseEventKind::Up(_))
                    || !self.terminal_surfaces.contains(&capture)
                {
                    self.terminal_mouse_capture = None;
                }
                return true;
            }
            if matches!(mouse.kind, MouseEventKind::Down(_)) {
                self.terminal_mouse_capture = None;
            }
        }
        if let Some(drag) = self.terminal_selection_drag {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left)
                | MouseEventKind::Up(MouseButton::Left)
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown => {
                    // A resize, tab move, or closed terminal invalidates the old geometry.
                    if self.terminal_surfaces.contains(&drag) {
                        if let Some(pty) = self
                            .terminals
                            .get_mut(drag.index)
                            .and_then(|terminal| terminal.terminal_pty.as_mut())
                        {
                            match mouse.kind {
                                MouseEventKind::ScrollUp => pty.scroll_up(self.scroll_speed),
                                MouseEventKind::ScrollDown => pty.scroll_down(self.scroll_speed),
                                MouseEventKind::Drag(_) if mouse.row < drag.area.y => {
                                    pty.scroll_up(1)
                                }
                                MouseEventKind::Drag(_) if mouse.row >= drag.area.bottom() => {
                                    pty.scroll_down(1)
                                }
                                _ => {}
                            }
                            let row = mouse
                                .row
                                .saturating_sub(drag.area.y)
                                .min(drag.area.height.saturating_sub(1));
                            let column = mouse
                                .column
                                .saturating_sub(drag.area.x)
                                .min(drag.area.width.saturating_sub(1));
                            pty.grid.extend_selection(row as usize, column as usize);
                            if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
                                pty.grid.finish_selection();
                            }
                        }
                    }
                    if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
                        self.terminal_selection_drag = None;
                    }
                    return true;
                }
                MouseEventKind::Down(_) => {
                    self.terminal_selection_drag = None;
                }
                _ => {}
            }
        }
        // Menus render above terminal surfaces. Existing captures above still
        // own their release, but a new gesture belongs to the open menu.
        if self.menu_active && self.menu_open {
            return false;
        }
        // A press handled by workbench chrome must not send its later drag or
        // release to a child that never received the press.
        if matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_)) {
            return false;
        }
        let Some(surface) = self
            .terminal_surfaces
            .iter()
            .rev()
            .copied()
            .find(|surface| surface.area.contains((mouse.column, mouse.row).into()))
        else {
            return false;
        };
        if surface.index >= self.terminals.len() {
            return false;
        }
        if self.terminal_wants_mouse(mouse) && self.terminal_selection_drag.is_none() {
            if matches!(mouse.kind, MouseEventKind::Down(_)) {
                self.focus_terminal_surface(surface);
                self.terminal_mouse_capture = Some(surface);
            }
            self.forward_terminal_mouse_at(surface, mouse);
            return true;
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.focus_terminal_surface(surface);
                let clicks = self.terminal_clicks.count(surface, mouse);
                if let Some(pty) = self.terminals[surface.index].terminal_pty.as_mut() {
                    let row = (mouse.row - surface.area.y) as usize;
                    let column = (mouse.column - surface.area.x) as usize;
                    match clicks {
                        1 => pty.grid.begin_selection(row, column),
                        2 => pty.grid.begin_word_selection(row, column),
                        _ => pty.grid.begin_line_selection(row, column),
                    }
                    self.terminal_selection_drag = Some(surface);
                }
                true
            }
            MouseEventKind::Down(MouseButton::Right) => {
                self.focus_terminal_surface(surface);
                self.context_menu_items = vec![
                    ContextMenuItem {
                        label: "Copy".into(),
                        shortcut: "Ctrl+Shift+C".into(),
                        id: "copy",
                        separator_after: false,
                    },
                    ContextMenuItem {
                        label: "Paste".into(),
                        shortcut: "Ctrl+Shift+V".into(),
                        id: "paste",
                        separator_after: false,
                    },
                ];
                self.context_menu_pos = (mouse.column, mouse.row);
                self.context_menu_inner = None;
                self.context_menu_selected = 0;
                self.context_menu_active = true;
                true
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                if let Some(pty) = self.terminals[surface.index].terminal_pty.as_mut() {
                    if mouse.kind == MouseEventKind::ScrollUp {
                        pty.scroll_up(self.scroll_speed);
                    } else {
                        pty.scroll_down(self.scroll_speed);
                    }
                }
                true
            }
            _ => false,
        }
    }

    pub(crate) fn copy_terminal_selection(&mut self, index: usize) {
        self.copy_terminal_selection_with(index, |text| crate::clipboard::set_text(text).is_ok());
    }

    pub(crate) fn copy_terminal_selection_with(
        &mut self,
        index: usize,
        copy: impl FnOnce(&str) -> bool,
    ) {
        let Some(text) = self
            .terminals
            .get(index)
            .and_then(|terminal| terminal.terminal_pty.as_ref())
            .and_then(|pty| pty.grid.selected_text())
            .filter(|text| !text.is_empty())
        else {
            return;
        };
        let copied = copy(&text);
        self.push_clipboard_history(&text);
        if !copied {
            self.notify("System clipboard unavailable — copied to clipboard history only");
        }
    }

    pub(crate) fn paste_terminal_clipboard(&mut self, index: usize) {
        match crate::clipboard::get_text() {
            Ok(text) => self.paste_terminal_text(index, &text),
            Err(_) => self.notify("System clipboard unavailable"),
        }
    }

    pub(crate) fn paste_terminal_text(&mut self, index: usize, text: &str) {
        if index >= self.terminals.len() || text.is_empty() {
            return;
        }
        self.ensure_terminal_pty_at(index, 80, 24);
        if let Some(pty) = self.terminals[index].terminal_pty.as_mut() {
            pty.grid.clear_selection();
            pty.grid.scroll_view = 0;
            if pty.grid.bracketed_paste {
                pty.write(b"\x1b[200~");
            }
            pty.write(text.as_bytes());
            if pty.grid.bracketed_paste {
                pty.write(b"\x1b[201~");
            }
        }
    }
}

#[cfg(test)]
#[path = "app_terminal_selection_tests.rs"]
mod tests;
