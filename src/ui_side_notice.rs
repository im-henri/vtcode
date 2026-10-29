//! A compact, modeless notice for exceptional workbench events. Producers own
//! their lifecycle; this component has no timer, worker, or logging subscription.
use crate::app::{App, NotifyLevel};
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Block, BorderType, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

pub(crate) struct Action {
    pub label: &'static str,
    pub command: &'static str,
}

pub(crate) struct Notice {
    pub title: &'static str,
    pub message: String,
    pub level: NotifyLevel,
    pub actions: &'static [Action],
}

struct Entry {
    id: &'static str,
    notice: Notice,
    dismissed: bool,
}

/// One exceptional notice at a time. Updating the same producer's notice keeps
/// dismissal intact; an explicit `show` starts a new incident or reveals it.
/// Ordinary notifications continue to use the status bar and history.
#[derive(Default)]
pub(crate) struct State {
    entry: Option<Entry>,
    pub area: Option<Rect>,
    close: Rect,
    actions: Vec<(Rect, &'static str)>,
}

impl State {
    pub(crate) fn visible(&self) -> bool {
        self.entry.as_ref().is_some_and(|entry| !entry.dismissed)
    }

    pub(crate) fn publish(&mut self, id: &'static str, notice: Notice) {
        // Until the next paint, the old card still covers this footprint.
        // Disable its stale actions without letting clicks reach the editor.
        let occupied = self.area.filter(|_| self.visible());
        let dismissed = self
            .entry
            .as_ref()
            .is_some_and(|entry| entry.id == id && entry.dismissed);
        self.entry = Some(Entry {
            id,
            notice,
            dismissed,
        });
        self.invalidate_layout();
        self.area = occupied;
    }

    pub(crate) fn show(&mut self, id: &'static str, notice: Notice) {
        self.publish(id, notice);
        self.entry.as_mut().unwrap().dismissed = false;
    }

    pub(crate) fn dismiss(&mut self, id: &'static str) {
        if let Some(entry) = &mut self.entry
            && entry.id == id
        {
            entry.dismissed = true;
            self.invalidate_layout();
        }
    }

    pub(crate) fn remove(&mut self, id: &'static str) {
        if self.entry.as_ref().is_some_and(|entry| entry.id == id) {
            self.entry = None;
            self.invalidate_layout();
        }
    }

    fn invalidate_layout(&mut self) {
        self.area = None;
        self.close = Rect::default();
        self.actions.clear();
    }
}

fn obscured(app: &App) -> bool {
    crate::ui_modal::active(app).is_some()
        || app.remote_connection.details
        || app.palette.active
        || app.quick_open_active
        || app.change_dir_active
        || app.menu_open
        || app.context_menu_active
        || app.tab_context_active
        || app.explorer_context_active
        || app.sc_context_active
        || app.desks.popup.mode.is_some()
        || crate::ui_logs::dropdown_open(app)
        || crate::ui_quick_picker::active(app).is_some()
}

pub(crate) fn draw(frame: &mut Frame, app: &mut App) {
    app.side_notice.invalidate_layout();
    if !app.side_notice.visible() || obscured(app) {
        return;
    }
    let screen = frame.area();
    let bottom = app
        .status_bar_area
        .map_or(screen.bottom().saturating_sub(1), |area| {
            area.y.min(screen.bottom())
        });
    if screen.width < 16 || bottom.saturating_sub(screen.y) < 6 {
        return;
    }
    let area = Rect::new(
        screen.x + 1,
        bottom - 5,
        screen.width.saturating_sub(2).min(52),
        5,
    );
    let notice = &app.side_notice.entry.as_ref().unwrap().notice;
    let bg = app.colors.ui.bg_notification;
    let fg = app
        .colors
        .ui
        .readable_foreground(app.colors.ui.fg_notification, bg, 4.5);
    let dim = app.colors.ui.description_color(bg);
    let style = Style::default().bg(bg).fg(fg);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(app.colors.ui.border_notification.unwrap_or(bg)))
        .style(style);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let level = match notice.level {
        NotifyLevel::Info => 0,
        NotifyLevel::Warning => 1,
        NotifyLevel::Error => 2,
    };
    let icon = match notice.level {
        NotifyLevel::Info => "i",
        NotifyLevel::Warning => "!",
        NotifyLevel::Error => "!",
    };
    frame.render_widget(
        Paragraph::new(icon).style(style.fg(app.colors.ui.notification_icon_color(level, bg))),
        Rect::new(inner.x, inner.y, 1, 1),
    );
    frame.render_widget(
        Paragraph::new(crate::ui_pickers::truncate_to_width(
            notice.title,
            inner.width.saturating_sub(5) as usize,
        ))
        .style(style.add_modifier(Modifier::BOLD)),
        Rect::new(inner.x + 2, inner.y, inner.width.saturating_sub(5), 1),
    );
    let close = Rect::new(inner.right() - 2, inner.y, 2, 1);
    let point = (app.mouse_col, app.mouse_row).into();
    let hover = app.colors.ui.toolbar_hover.over(bg).unwrap_or(bg);
    frame.render_widget(
        Paragraph::new("× ").style(style.fg(dim).bg(if close.contains(point) {
            hover
        } else {
            bg
        })),
        close,
    );
    frame.render_widget(
        Paragraph::new(crate::ui_pickers::truncate_to_width(
            &notice.message,
            inner.width.saturating_sub(2) as usize,
        ))
        .style(style.fg(dim)),
        Rect::new(inner.x + 2, inner.y + 1, inner.width.saturating_sub(2), 1),
    );
    let link = app
        .colors
        .ui
        .readable_foreground(app.colors.fg_accent_blue, bg, 4.5);
    let mut x = inner.x + 2;
    for action in notice.actions {
        let width = action.label.width().min(u16::MAX as usize) as u16;
        if x.saturating_add(width) > inner.right() {
            break;
        }
        let hit = Rect::new(x, inner.y + 2, width, 1);
        frame.render_widget(
            Paragraph::new(action.label).style(style.fg(link).bg(if hit.contains(point) {
                hover
            } else {
                bg
            })),
            hit,
        );
        app.side_notice.actions.push((hit, action.command));
        x = x.saturating_add(width + 2);
    }
    app.side_notice.area = Some(area);
    app.side_notice.close = close;
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if !app.side_notice.visible() || obscured(app) {
        return false;
    }
    let Some(area) = app.side_notice.area else {
        return false;
    };
    let point = (mouse.column, mouse.row).into();
    let inside = area.contains(point);
    if mouse.kind == MouseEventKind::Moved {
        app.mouse_hover_changed |= inside || area.contains((app.mouse_col, app.mouse_row).into());
    }
    if !inside {
        return false;
    }
    app.mouse_col = mouse.column;
    app.mouse_row = mouse.row;
    if let MouseEventKind::Down(button) = mouse.kind {
        // Own the entire gesture, even if closing the notice or opening Output
        // changes the layout before its drag/release arrives.
        crate::ui_panels::capture_pointer(app, button);
        if button == MouseButton::Left {
            if app.side_notice.close.contains(point) {
                let id = app.side_notice.entry.as_ref().unwrap().id;
                app.side_notice.dismiss(id);
            } else if let Some(command) = app
                .side_notice
                .actions
                .iter()
                .find(|(hit, _)| hit.contains(point))
                .map(|(_, command)| *command)
            {
                app.execute_palette_command(command);
            }
        }
    }
    true
}

#[cfg(test)]
#[path = "ui_side_notice_tests.rs"]
mod tests;
