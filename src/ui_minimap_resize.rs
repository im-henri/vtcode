use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
};

use crate::app::{App, Tab, TabKind};
use crate::settings::{MAX_MINIMAP_WIDTH, MIN_MINIMAP_WIDTH};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Owner {
    editor: u64,
    kind: TabKind,
}

impl Owner {
    fn from_tab(tab: &Tab) -> Self {
        Self {
            editor: tab.editor.navigation_id,
            kind: tab.kind,
        }
    }

    fn visible(self, app: &App) -> bool {
        app.visible_tab_group(self.editor, self.kind).is_some()
            || (app.split_tree.is_none()
                && app.split_active()
                && app.groups.iter().enumerate().any(|(index, group)| {
                    index != app.active_group
                        && group.desk == app.desks.active
                        && group
                            .tabs
                            .get(group.active_tab)
                            .is_some_and(|tab| Self::from_tab(tab) == self)
                }))
    }
}

#[derive(Clone, Copy)]
struct Divider {
    owner: Owner,
    area: Rect,
    button: bool,
}

#[derive(Clone, Copy)]
struct Press {
    owner: Owner,
    start: Position,
    original_width: u16,
    width: u16,
    toggle: bool,
}

impl Press {
    fn valid(self, app: &App) -> bool {
        app.show_minimap
            && app.terminal_has_focus
            && !app.change_dir_active
            && crate::ui_modal::active(app).is_none()
            && app.minimap_width == self.original_width
            && self.owner.visible(app)
    }

    fn resize(&mut self, column: u16) {
        let delta = i32::from(self.start.x) - i32::from(column);
        self.width = (i32::from(self.original_width) + delta)
            .clamp(i32::from(MIN_MINIMAP_WIDTH), i32::from(MAX_MINIMAP_WIDTH))
            as u16;
    }
}

pub(crate) struct State {
    locked: bool,
    press: Option<Press>,
    dividers: Vec<Divider>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            locked: true,
            press: None,
            dividers: Vec::new(),
        }
    }
}

impl State {
    pub(crate) fn cancel(&mut self) -> bool {
        self.press.take().is_some()
    }

    pub(crate) fn dragging(&self) -> bool {
        self.press.is_some() && !self.locked
    }

    fn hit(&self, point: Position) -> Option<Divider> {
        self.dividers
            .iter()
            .find(|divider| divider.area.contains(point))
            .copied()
    }
}

impl App {
    pub(crate) fn minimap_render_width(&self) -> u16 {
        self.minimap_resize
            .press
            .map_or(self.minimap_width, |press| press.width)
    }
}

pub(crate) fn begin_frame(app: &mut App) {
    app.minimap_resize.dividers.clear();
    if app
        .minimap_resize
        .press
        .is_some_and(|press| !press.valid(app))
    {
        app.minimap_resize.cancel();
    }
}

/// Paint on top of the editor gradient and outside retained animation scenes.
/// The minimap and its scrollbar keep their own pointer regions.
pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    if !app.show_minimap || crate::ui_modal::active(app).is_some() {
        return;
    }
    let frame_area = f.area();
    let pointer = Position::new(app.mouse_col, app.mouse_row);
    let make_divider = |tab: &Tab, minimap: Rect| {
        if minimap.is_empty()
            || tab.extension_view.is_some()
            || !matches!(
                tab.kind,
                TabKind::File | TabKind::GitRevision | TabKind::Diff(_) | TabKind::CommitDetails
            )
        {
            return None;
        }
        let mut area = Rect::new(minimap.x.checked_sub(1)?, minimap.y, 1, minimap.height)
            .intersection(frame_area);
        // Find overlays the top of the pane, including the divider.
        for bar in &app.file_search_bars {
            if area.intersects(bar.area) {
                let top = bar.area.bottom().min(area.bottom());
                area = Rect::new(area.x, top, area.width, area.bottom() - top);
            }
        }
        (!area.is_empty()).then_some(Divider {
            owner: Owner::from_tab(tab),
            area,
            button: app.terminal_has_focus && area.contains(pointer),
        })
    };
    let split = app.split_active();
    let dividers = &mut app.minimap_resize.dividers;
    if let Some((tab, area)) = app.tabs.get(app.active_tab).zip(app.minimap_area) {
        dividers.extend(make_divider(tab, area));
    }
    let mut add_group = |index: usize| {
        if index != app.active_group
            && let Some(group) = app.groups.get(index)
            && group.desk == app.desks.active
            && let Some((tab, area)) = group.tabs.get(group.active_tab).zip(group.minimap_area)
        {
            dividers.extend(make_divider(tab, area));
        }
    };
    if let Some(tree) = &app.split_tree {
        tree.visit_leaves(&mut add_group);
    } else if split {
        for index in 0..app.groups.len() {
            add_group(index);
        }
    }
    let state = &app.minimap_resize;
    for divider in &state.dividers {
        let dragging = state.dragging()
            && state
                .press
                .is_some_and(|press| press.owner == divider.owner);
        if !divider.button && !dragging {
            continue;
        }
        let area = divider.area;
        for y in area.y..area.bottom() {
            let cell = &mut f.buffer_mut()[(area.x, y)];
            let accent = app
                .colors
                .ui
                .sash_hover_border
                .border(cell.bg)
                .unwrap_or(app.colors.fg_accent);
            if divider.button && y == area.y {
                let glyph = if state.locked { 'L' } else { '↔' };
                cell.set_char(glyph)
                    .set_bg(accent)
                    .set_fg(app.colors.ui.widget_text_color(accent));
            } else {
                cell.set_fg(accent);
            }
        }
    }
}

pub(crate) fn hover_changed(app: &App, previous: Position) -> bool {
    let state = &app.minimap_resize;
    state.hit(previous).map(|divider| divider.area)
        != state
            .hit(Position::new(app.mouse_col, app.mouse_row))
            .map(|divider| divider.area)
}

pub(crate) fn press(app: &mut App, mouse: MouseEvent) -> bool {
    let point = Position::new(mouse.column, mouse.row);
    let Some(divider) = app.minimap_resize.hit(point) else {
        return false;
    };
    if !app.show_minimap || !divider.owner.visible(app) {
        return false;
    }
    app.clear_drag_states();
    app.drag_start_pos = None;
    app.drag_has_moved = false;
    app.minimap_resize.press = Some(Press {
        owner: divider.owner,
        start: point,
        original_width: app.minimap_width,
        width: app.minimap_width,
        toggle: divider.button && point.y == divider.area.y,
    });
    true
}

pub(crate) fn captured_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    let Some(mut press) = app.minimap_resize.press else {
        return false;
    };
    if matches!(mouse.kind, MouseEventKind::Down(_)) || !press.valid(app) {
        app.minimap_resize.cancel();
        return false;
    }
    let release = mouse.kind == MouseEventKind::Up(MouseButton::Left);
    if !release && mouse.kind != MouseEventKind::Drag(MouseButton::Left) {
        return false;
    }
    app.mouse_col = mouse.column;
    app.mouse_row = mouse.row;
    app.mouse_hover_changed = true;
    if !app.minimap_resize.locked && mouse.column != press.start.x {
        press.resize(mouse.column);
    } else {
        press.width = press.original_width;
    }
    if release {
        app.minimap_resize.cancel();
        if press.toggle
            && press.start == Position::new(mouse.column, mouse.row)
            && app.minimap_resize.hit(press.start).is_some_and(|divider| {
                divider.owner == press.owner && divider.area.y == press.start.y
            })
        {
            app.minimap_resize.locked = !app.minimap_resize.locked;
        } else if press.width != press.original_width {
            app.save_minimap_width(press.width);
        }
        app.update_sidebar_auto_hide(mouse);
    } else {
        // Terminals can report movement within one cell during a click.
        press.toggle &= press.start == Position::new(mouse.column, mouse.row);
        app.minimap_resize.press = Some(press);
    }
    true
}

#[cfg(test)]
#[path = "ui_minimap_resize_tests.rs"]
mod tests;
