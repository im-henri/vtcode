use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::{app::App, ui_widgets::DropdownScrollbar};

#[cfg(test)]
#[path = "mouse_explorer_context_tests.rs"]
mod tests;

fn scroll_to(app: &mut App, inner: Rect, offset: usize) {
    let items = app.explorer_context_menu_items();
    let offset = offset.min(items.len().saturating_sub(inner.height as usize));
    let end = (offset + inner.height as usize).min(items.len());
    if offset < end {
        // Keep keyboard focus in the new viewport so drawing cannot snap a
        // manual scroll back to the previously selected command.
        let selected = app.explorer_context_selected.clamp(offset, end - 1);
        let selected = (selected..end)
            .chain((offset..selected).rev())
            .find(|&index| items[index] != "─")
            .unwrap_or(selected);
        app.explorer_context_selected = selected;
    }
    app.explorer_context_scroll = offset;
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    let point = (mouse.column, mouse.row).into();
    if matches!(
        mouse.kind,
        MouseEventKind::Down(_) | MouseEventKind::Up(MouseButton::Left)
    ) {
        app.explorer_context_scrollbar_drag = None;
    }
    let Some(inner) = app.explorer_context_inner else {
        if matches!(mouse.kind, MouseEventKind::Down(_)) {
            app.explorer_context_active = false;
        }
        return;
    };
    let count = app.explorer_context_menu_items().len();
    let bar = DropdownScrollbar::new(inner, count, app.explorer_context_scroll);
    if let Some((start_row, start_offset)) = app.explorer_context_scrollbar_drag
        && mouse.kind == MouseEventKind::Drag(MouseButton::Left)
    {
        if let Some(bar) = &bar {
            scroll_to(
                app,
                inner,
                bar.drag_offset(start_row, start_offset, mouse.row),
            );
        }
        return;
    }
    let item_area = Rect::new(
        inner.x,
        inner.y,
        inner.width - bar.as_ref().map_or(0, |bar| bar.area.width),
        inner.height,
    );
    let hit = item_area
        .contains(point)
        .then(|| app.explorer_context_scroll + (mouse.row - inner.y) as usize)
        .filter(|&index| {
            app.explorer_context_menu_items()
                .get(index)
                .is_some_and(|&label| label != "─")
        });
    let in_menu = Rect::new(
        inner.x.saturating_sub(1),
        inner.y.saturating_sub(1),
        inner.width.saturating_add(2),
        inner.height.saturating_add(2),
    )
    .contains(point);
    match mouse.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if in_menu => {
            let offset = if mouse.kind == MouseEventKind::ScrollUp {
                app.explorer_context_scroll.saturating_sub(3)
            } else {
                app.explorer_context_scroll.saturating_add(3)
            };
            scroll_to(app, inner, offset);
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(bar) = bar.filter(|bar| bar.area.contains(point)) {
                if bar.thumb.contains(point) {
                    app.explorer_context_scrollbar_drag =
                        Some((mouse.row, app.explorer_context_scroll));
                } else {
                    let offset = if mouse.row < bar.thumb.y {
                        app.explorer_context_scroll
                            .saturating_sub(inner.height as usize)
                    } else {
                        app.explorer_context_scroll
                            .saturating_add(inner.height as usize)
                    };
                    scroll_to(app, inner, offset);
                }
            } else if let Some(index) = hit {
                app.explorer_context_selected = index;
                app.execute_explorer_context(index);
            } else if !in_menu {
                app.explorer_context_active = false;
                app.explorer_context_inner = None;
            }
        }
        MouseEventKind::Moved => {
            app.reset_hover_states();
            if let Some(index) = hit {
                app.explorer_context_selected = index;
            }
        }
        MouseEventKind::Down(_) => {
            app.explorer_context_active = false;
            app.explorer_context_inner = None;
        }
        _ => {}
    }
}
