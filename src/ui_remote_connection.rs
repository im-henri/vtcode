//! Reconnect details and authentication live in an ordinary editor tab.
use crate::{
    app::{ActivePane, App, TabKind},
    app_remote_connection::Phase,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Margin, Rect},
    style::{Modifier, Style},
    widgets::{Block, Paragraph, Wrap},
};
use unicode_width::UnicodeWidthStr;

fn actions(app: &App) -> Vec<(&'static str, &'static str)> {
    let mut actions = vec![("Connection Log", "diagnostics")];
    match app.remote_connection.phase {
        Phase::Connected | Phase::Restored | Phase::Restoring | Phase::Resolving => {}
        Phase::Stopped => actions.push(("Reconnect", "retry")),
        Phase::Waiting | Phase::Starting | Phase::Connecting => {
            actions.push(("Retry Now", "retry"));
            actions.push(("Stop Reconnecting", "stop"));
        }
    }
    actions.push(("Close Tab", "dismiss"));
    actions
}

pub(crate) fn draw_in_area(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused = crate::ui_remote_targets::connection_has_focus(app);
    let interactive = !app.rendering_inactive_editor;
    if interactive {
        app.remote_connection.area = Some(area);
        app.remote_connection.buttons.clear();
    }
    let style = Style::default()
        .fg(app.colors.fg_text)
        .bg(app.colors.bg_editor);
    frame.render_widget(Paragraph::new("").style(style), area);
    let content = area.inner(Margin::new(
        u16::from(area.width > 4) * 2,
        u16::from(area.height > 10),
    ));
    if content.is_empty() {
        return;
    }
    let choices = actions(app);
    let mut controls = Vec::new();
    let (mut x, mut y) = (0u16, 0u16);
    for &(label, id) in &choices {
        let width = (label.width() as u16 + 4).min(content.width);
        if x > 0 && x + width > content.width {
            x = 0;
            y += 1;
        }
        controls.push((Rect::new(content.x + x, y, width, 1), label, id));
        x += width + 1;
    }
    let footer = content.bottom().saturating_sub(y + 1).max(content.y);
    let title = match app.remote_connection.phase {
        Phase::Connected => "SSH connected",
        Phase::Restored => "SSH connection restored",
        Phase::Stopped => "SSH disconnected",
        Phase::Restoring | Phase::Resolving => "Restoring remote workspace",
        _ => "Reconnecting to SSH",
    };
    frame.render_widget(
        Paragraph::new(title).style(style.add_modifier(Modifier::BOLD)),
        Rect::new(content.x, content.y, content.width, 1),
    );
    let authenticating = app.remote_connection.phase == Phase::Connecting;
    let message = if let Some(error) = app
        .remote_connection
        .login
        .as_ref()
        .and_then(|login| login.input_error.as_ref())
    {
        error.clone()
    } else if authenticating {
        if app
            .remote_connection
            .login
            .as_ref()
            .is_some_and(|login| login.input_sent)
        {
            "Input sent to SSH. Password characters stay hidden.".into()
        } else {
            "SSH sign-in · click the terminal and type".into()
        }
    } else {
        app.remote_connection.message.clone()
    };
    let help = if authenticating {
        "Passwords and passphrases are hidden by SSH. Type, then Enter. Ctrl+V pastes; Esc closes this tab."
    } else {
        "Local files and unsaved edits stay open while the connection recovers."
    };
    let mut top = content.y + 2;
    let mut paragraphs = vec![(message, style)];
    if !app.remote_connection.error.is_empty() {
        paragraphs.push((
            app.remote_connection.error.clone(),
            style.fg(app.colors.ui.error_text_color(app.colors.bg_editor)),
        ));
    }
    paragraphs.push((
        help.into(),
        style.fg(app.colors.ui.description_color(app.colors.bg_editor)),
    ));
    for (text, text_style) in paragraphs {
        let rows =
            App::wrap_setting_text_for_width(&text, content.width.max(1) as usize).len() as u16;
        let height = rows.min(footer.saturating_sub(top + 4));
        if height > 0 {
            frame.render_widget(
                Paragraph::new(text)
                    .style(text_style)
                    .wrap(Wrap { trim: false }),
                Rect::new(content.x, top, content.width, height),
            );
            top += height + 1;
        }
    }
    if footer > top {
        let terminal = Rect::new(content.x, top, content.width, footer - top);
        let bg = app.colors.ui.bg_terminal;
        let block = Block::bordered()
            .title(if authenticating && focused {
                " SSH · type here "
            } else {
                " SSH output "
            })
            .style(Style::default().fg(app.colors.ui.fg_terminal).bg(bg))
            .border_style(
                Style::default()
                    .fg(if authenticating && focused {
                        app.colors.fg_accent_blue
                    } else {
                        app.colors.fg_dim
                    })
                    .bg(bg),
            );
        let viewport = block.inner(terminal);
        frame.render_widget(block, terminal);
        if interactive {
            app.remote_connection
                .resize_login(viewport.width, viewport.height);
        }
        if let Some(login) = &mut app.remote_connection.login {
            let first_row = (login.grid.cursor_row + 1).saturating_sub(viewport.height as usize);
            let first_col = (login.grid.cursor_col + 1).saturating_sub(viewport.width as usize);
            login.rows.paint_viewport(
                frame.buffer_mut(),
                &login.grid,
                viewport,
                &app.colors.ui,
                first_row,
                first_col,
            );
            if focused && authenticating && login.grid.cursor_visible && !viewport.is_empty() {
                frame.set_cursor_position((
                    viewport.x
                        + (login.grid.cursor_col - first_col).min(viewport.width as usize - 1)
                            as u16,
                    viewport.y
                        + (login.grid.cursor_row - first_row).min(viewport.height as usize - 1)
                            as u16,
                ));
            }
        }
    }
    for (index, (mut button, label, id)) in controls.into_iter().enumerate() {
        button.y = footer.saturating_add(button.y);
        if button.y >= content.bottom() {
            continue;
        }
        crate::ui_widgets::draw_dialog_button_with_hover(
            frame,
            button,
            label,
            &app.colors,
            id == "retry",
            focused && !authenticating && app.remote_connection.focus == index,
            button.contains((app.mouse_col, app.mouse_row).into()),
            false,
            true,
        );
        if interactive {
            app.remote_connection.buttons.push((button, id));
        }
    }
}

fn activate(app: &mut App, id: &str) {
    match id {
        "diagnostics" => app.show_remote_diagnostics(),
        "retry" => {
            app.retry_remote_connection();
            app.remote_connection.details = true;
        }
        "stop" => app.stop_remote_reconnect(),
        "dismiss" => app.close_remote_connection_tabs(0),
        _ => {}
    }
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if crate::ui_modal::active(app).is_some() || app.palette.active || app.quick_open_active {
        return false;
    }
    if app.tabs.get(app.active_tab).map(|tab| tab.kind) != Some(TabKind::RemoteConnection(0))
        || !app
            .remote_connection
            .area
            .is_some_and(|area| area.contains((mouse.column, mouse.row).into()))
    {
        return false;
    }
    if mouse.kind == MouseEventKind::Moved {
        app.mouse_hover_changed = true;
    }
    if let MouseEventKind::Down(button) = mouse.kind {
        app.active_pane = ActivePane::Editor;
        app.terminal_panel_focused = false;
        app.remote_targets.filter_focused = false;
        crate::ui_panels::capture_pointer(app, button);
        if button == MouseButton::Left {
            let action = app
                .remote_connection
                .buttons
                .iter()
                .find(|(area, _)| area.contains((mouse.column, mouse.row).into()))
                .map(|(_, id)| *id);
            if let Some(id) = action {
                activate(app, id);
            }
        }
    }
    true
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if !crate::ui_remote_targets::connection_has_focus(app)
        || app.tabs[app.active_tab].kind != TabKind::RemoteConnection(0)
    {
        return false;
    }
    if app.remote_connection.phase == Phase::Connecting {
        if (key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('v' | 'V')))
            || (key.modifiers.contains(KeyModifiers::SHIFT) && key.code == KeyCode::Insert)
        {
            match crate::clipboard::get_text() {
                Ok(text) => app.remote_connection.send_paste(&text),
                Err(error) => app.notify_warn(format!("Could not paste into SSH: {error}")),
            }
        } else {
            app.remote_connection.send_key(key);
        }
    } else {
        let choices = actions(app);
        let count = choices.len();
        match key.code {
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                let reverse = matches!(key.code, KeyCode::BackTab | KeyCode::Left)
                    || key.modifiers.contains(KeyModifiers::SHIFT);
                app.remote_connection.focus = (app.remote_connection.focus.min(count - 1)
                    + if reverse { count - 1 } else { 1 })
                    % count;
            }
            KeyCode::Enter if key.kind != crossterm::event::KeyEventKind::Repeat => {
                activate(app, choices[app.remote_connection.focus.min(count - 1)].1)
            }
            _ => {}
        }
    }
    true
}

pub(crate) fn handle_paste(app: &mut App, text: &str) -> bool {
    if !crate::ui_remote_targets::connection_has_focus(app)
        || app.tabs[app.active_tab].kind != TabKind::RemoteConnection(0)
    {
        return false;
    }
    app.remote_connection.send_paste(text);
    true
}
