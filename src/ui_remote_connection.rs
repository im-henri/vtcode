//! Explicit authentication details during reconnect. The compact workbench
//! notice lives in `ui_side_notice`; only this opt-in view captures typing.
use crate::{app::App, app_remote_connection::Phase};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

pub(crate) fn draw(frame: &mut Frame, app: &mut App) {
    let state = &mut app.remote_connection;
    state.area = None;
    state.buttons.clear();
    if !state.details {
        return;
    }
    let screen = frame.area();
    if screen.width < 5 || screen.height < 5 {
        return;
    }
    let width = if state.details { 76 } else { 56 }.min(screen.width.saturating_sub(2).max(3));
    let height = if state.details {
        24
    } else if state.error.is_empty() {
        8
    } else {
        10
    }
    .min(screen.height.saturating_sub(2).max(3));
    let area = Rect::new(
        screen.right().saturating_sub(width + 1),
        screen.y + 1,
        width,
        height,
    );
    state.area = Some(area);
    let style = Style::default()
        .fg(app.colors.fg_text)
        .bg(app.colors.bg_sidebar);
    let block = Block::bordered().title(" Remote connection ").style(style);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let title = match state.phase {
        Phase::Connected => "Connected",
        Phase::Restored => "Connection restored",
        Phase::Stopped => "Disconnected",
        Phase::Restoring => "Reconnected — checking files",
        _ => "Connection lost — reconnecting",
    };
    frame.render_widget(
        Paragraph::new(title).style(style.add_modifier(Modifier::BOLD)),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    if inner.height < 4 {
        frame.render_widget(
            Paragraph::new(state.message.as_str()).style(style),
            Rect::new(
                inner.x,
                inner.y + 1,
                inner.width,
                inner.height.saturating_sub(1),
            ),
        );
        return;
    }
    let mut text = state.message.clone();
    if !state.error.is_empty() {
        text.push('\n');
        text.push_str(&state.error);
    }
    frame.render_widget(
        Paragraph::new(text).style(style).wrap(Wrap { trim: false }),
        Rect::new(
            inner.x,
            inner.y + 2,
            inner.width,
            if state.details {
                3.min(inner.height.saturating_sub(3))
            } else {
                inner.height.saturating_sub(3)
            },
        ),
    );
    if state.details
        && inner.height > 7
        && let Some(login) = &state.login
    {
        let rows = inner.height.saturating_sub(7) as usize;
        let lines: Vec<_> = login
            .grid
            .cells
            .iter()
            .take(rows)
            .map(|row| {
                Line::from(
                    row.iter()
                        .map(|cell| {
                            Span::styled(
                                cell.ch.to_string(),
                                Style::default().fg(cell.fg).bg(cell.bg),
                            )
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        frame.render_widget(
            Paragraph::new(lines).style(style),
            Rect::new(inner.x, inner.y + 5, inner.width, rows as u16),
        );
    }
    let buttons: &[(&str, &str)] = if matches!(
        state.phase,
        Phase::Connected | Phase::Restored | Phase::Restoring
    ) {
        &[("Diagnostics", "diagnostics"), ("Dismiss", "dismiss")]
    } else if state.phase == Phase::Stopped {
        &[
            ("Diagnostics", "diagnostics"),
            ("Reconnect", "retry"),
            ("Details", "details"),
            ("Dismiss", "dismiss"),
        ]
    } else {
        &[
            ("Diagnostics", "diagnostics"),
            ("Details", "details"),
            ("Retry now", "retry"),
            ("Stop", "stop"),
            ("Dismiss", "dismiss"),
        ]
    };
    let mut x = inner.x;
    for &(label, id) in buttons {
        let label = if id == "details" && state.details {
            "Back to editor"
        } else {
            label
        };
        let width = (label.width() as u16 + 2).min(inner.right().saturating_sub(x));
        if width < 3 {
            break;
        }
        let button = Rect::new(x, inner.bottom().saturating_sub(1), width, 1);
        let hovered = button.contains((app.mouse_col, app.mouse_row).into());
        frame.render_widget(
            Paragraph::new(format!(" {label} ")).style(if hovered {
                style.add_modifier(Modifier::REVERSED)
            } else {
                style
            }),
            button,
        );
        state.buttons.push((button, id));
        x = x.saturating_add(width + 1);
    }
}
pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if crate::ui_modal::active(app).is_some() || app.palette.active || app.quick_open_active {
        return false;
    }
    if !app.remote_connection.details { return false; }
    let state = &mut app.remote_connection;
    if !state.details
        || !state
            .area
            .is_some_and(|area| area.contains((mouse.column, mouse.row).into()))
    {
        return false;
    }
    app.mouse_col = mouse.column;
    app.mouse_row = mouse.row;
    if mouse.kind == MouseEventKind::Moved {
        app.mouse_hover_changed = true;
    }
    if let MouseEventKind::Down(button) = mouse.kind {
        let action = state
            .buttons
            .iter()
            .find(|(area, _)| area.contains((mouse.column, mouse.row).into()))
            .map(|(_, id)| *id);
        crate::ui_panels::capture_pointer(app, button);
        if button != MouseButton::Left { return true; }
        match action {
            Some("diagnostics") => app.show_remote_diagnostics(),
            Some("retry") => app.retry_remote_connection(),
            Some("stop") => app.stop_remote_reconnect(),
            Some("details") => app.remote_connection.details = !app.remote_connection.details,
            Some("dismiss") => {
                app.remote_connection.details = false;
            }
            _ => {}
        }
    }
    true
}
pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if !app.remote_connection.details
        || crate::ui_modal::active(app).is_some()
        || app.palette.active
        || app.quick_open_active
        || key.code == KeyCode::F(1)
        || (key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('q' | 'Q' | 'p' | 'P')))
    {
        return false;
    }
    if key.code == KeyCode::Esc {
        app.remote_connection.details = false;
        return true;
    }
    app.remote_connection.send_key(key);
    true
}
pub(crate) fn handle_paste(app: &mut App, text: &str) -> bool {
    if !app.remote_connection.details
        || crate::ui_modal::active(app).is_some()
        || app.palette.active
        || app.quick_open_active
    {
        return false;
    }
    app.remote_connection.send_paste(text);
    true
}
