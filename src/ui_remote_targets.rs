//! Remote Explorer: saved SSH targets, accessible forms and inline SSH login.
use crate::{
    app::{ActivePane, App, SidebarView},
    app_remote_targets::Action,
    text_input::TextInputId,
    ui_widgets::{InputField, draw_dialog_button_with_hover},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    widgets::{Block, Clear, Paragraph, Wrap},
};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;

fn line(f: &mut Frame, area: Rect, y: u16, value: impl Into<String>, style: Style) {
    if y < area.bottom() && area.width > 0 {
        f.render_widget(
            Paragraph::new(crate::ui_pickers::truncate_to_width(
                &value.into(),
                area.width as usize,
            ))
            .style(style),
            Rect::new(area.x, y, area.width, 1),
        );
    }
}

fn button(
    f: &mut Frame,
    app: &mut App,
    area: Rect,
    label: &str,
    action: Action,
    primary: bool,
    focused: bool,
    modal: bool,
) {
    if area.is_empty() {
        return;
    }
    let hovered = area.contains((app.mouse_col, app.mouse_row).into());
    draw_dialog_button_with_hover(
        f,
        area,
        label,
        &app.colors,
        primary,
        focused,
        hovered,
        action == Action::ConfirmRemove,
        enabled(app, action),
    );
    if modal {
        app.remote_targets.dialog_hits.push((area, action));
    } else {
        app.remote_targets.hits.push((area, action));
    }
}

fn enabled(app: &App, action: Action) -> bool {
    match action {
        Action::Add
        | Action::Edit(_)
        | Action::Duplicate(_)
        | Action::Remove(_)
        | Action::SaveCurrent => app.remote_targets.can_edit(),
        Action::Connect(_) => !app.workspace_opening() && app.workspaces.exit.is_none(),
        Action::Save(_) => !app
            .remote_targets
            .form
            .as_ref()
            .is_some_and(|form| form.conflict),
        _ => true,
    }
}

fn input(f: &mut Frame, app: &mut App, area: Rect, id: TextInputId, placeholder: &str) {
    if area.is_empty() {
        return;
    }
    let focused = app.focused_text_input_id() == Some(id);
    let field = match id {
        TextInputId::RemoteFilter => &app.remote_targets.filter,
        TextInputId::RemoteTarget(index) => {
            &app.remote_targets.form.as_ref().unwrap().fields[index]
        }
        _ => unreachable!(),
    };
    let surface = if matches!(id, TextInputId::RemoteTarget(_)) {
        app.colors.ui.bg_widget
    } else {
        app.colors.bg_sidebar
    };
    let control = crate::ui_settings::settings_control_style(&app.colors, "text", surface, focused);
    let mut widget = InputField::new(&field.text)
        .viewport(app.text_input_viewport(id))
        .theme_control(&app.colors, app.colors.ui.settings_control("text"), surface)
        .bg(control.bg.unwrap_or(surface))
        .prefix(" ", app.colors.fg_dim)
        .placeholder_text(placeholder)
        .cursor_marker(if focused && app.cursor_visible {
            "│"
        } else {
            ""
        });
    if focused {
        widget = widget.cursor_pos(field.cursor).selection_range(
            field
                .anchor
                .map(|anchor| (anchor.min(field.cursor), anchor.max(field.cursor))),
        );
    }
    let hit = widget.render(f, area);
    crate::ui_settings::underline_field(f, area, control);
    app.register_text_input(id, hit);
}

#[path = "ui_remote_targets_panel.rs"]
mod panel;
pub(crate) use panel::draw_sidebar;

pub(crate) fn activate(app: &mut App, action: Action) {
    if !enabled(app, action) {
        return;
    }
    if !matches!(action, Action::Menu(_)) {
        app.remote_targets.menu = None;
    }
    match action {
        Action::Add => app.edit_remote_target(None, false),
        Action::Refresh => app.reload_remote_targets(),
        Action::OpenConfig => app.open_remote_targets_config(),
        Action::ClearFilter => app.clear_remote_target_filter(),
        Action::ReloadForm => app.reload_remote_target_form(),
        Action::SaveCopy => app.save_remote_target_copy(),
        Action::SaveCurrent => app.save_current_remote_target(),
        Action::Status => {
            app.execute_palette_command("remote_connection_status");
        }
        Action::Disconnect => {
            app.execute_palette_command("remote_disconnect");
        }
        Action::Filter => {
            app.focus_text_input(TextInputId::RemoteFilter);
        }
        Action::Select(position) => {
            app.active_pane = ActivePane::Explorer;
            app.terminal_panel_focused = false;
            app.remote_targets.filter_focused = false;
            app.remote_targets.selected = position;
            app.remote_targets.clamp();
        }
        Action::ScrollTo(offset) => {
            app.remote_targets.scroll = offset.min(
                app.remote_targets
                    .indices()
                    .len()
                    .saturating_sub(app.remote_targets.rows.max(1)),
            );
            app.remote_targets.last_click = None;
        }
        Action::Connect(index) => {
            if let Some(target) = app.remote_targets.target(index) {
                app.connect_remote_target(target);
            }
        }
        Action::Edit(index) => app.edit_remote_target(Some(index), false),
        Action::Duplicate(index) => app.edit_remote_target(Some(index), true),
        Action::Remove(index) => {
            app.remote_targets.remove_error = None;
            app.remote_targets.remove = app.remote_targets.target(index);
            app.modal.focus = 1;
        }
        Action::Menu(index) => {
            if let Some(position) = app
                .remote_targets
                .indices()
                .iter()
                .position(|&entry| entry == index)
            {
                app.remote_targets.selected = position;
            }
            app.remote_targets.filter_focused = false;
            app.remote_targets.last_click = None;
            app.remote_targets.dialog_hits.clear();
            app.active_pane = ActivePane::Explorer;
            app.remote_targets.menu = Some((index, 0, (app.mouse_col, app.mouse_row)));
        }
        Action::Focus(index) => {
            if let Some(form) = &mut app.remote_targets.form {
                form.focus = index;
                form.reveal = true;
            }
        }
        Action::Advanced => {
            if let Some(form) = &mut app.remote_targets.form {
                form.advanced = !form.advanced;
                form.focus = 5;
                form.reveal = true;
            }
            app.remote_targets.dialog_hits.clear();
            app.remove_text_input_regions(|id| matches!(id, TextInputId::RemoteTarget(_)));
        }
        Action::Save(connect) => app.save_remote_target(connect),
        Action::Cancel => {
            if app.remote_targets.modal() {
                app.cancel_remote_target_dialog();
            } else {
                app.cancel_remote_target_connection();
            }
        }
        Action::ConfirmRemove => app.remove_remote_target(),
        Action::Retry => {
            let target = app
                .remote_targets
                .attempt
                .as_ref()
                .map(|attempt| attempt.target.clone());
            app.cancel_remote_target_connection();
            if let Some(target) = target {
                app.connect_remote_target(target);
            }
        }
        Action::EditAttempt => {
            let target = app
                .remote_targets
                .attempt
                .as_ref()
                .map(|attempt| attempt.target.clone());
            app.cancel_remote_target_connection();
            if let Some(target) = target {
                let original = app
                    .remote_targets
                    .store
                    .as_ref()
                    .and_then(|store| store.find(&target.name).ok())
                    .cloned();
                app.begin_remote_target_form(target, original);
            }
        }
        Action::CopyLog => {
            let report = app.remote_targets.attempt.as_ref().map(|attempt| {
                attempt
                    .setup
                    .as_ref()
                    .map(|setup| setup.diagnostics().report())
                    .unwrap_or_else(|| attempt.error.clone().unwrap_or_default())
            });
            if let Some(report) = report {
                match crate::clipboard::set_text(report) {
                    Ok(()) => app.notify("Remote connection log copied"),
                    Err(error) => {
                        app.notify_warn(format!("Could not copy connection log: {error}"))
                    }
                }
            }
        }
    }
}

pub(crate) fn sidebar_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    let index = app.remote_targets.selected_index();
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);
    match code {
        KeyCode::Esc => {
            if app.remote_targets.filter_focused || !app.remote_targets.filter.text.is_empty() {
                app.clear_remote_target_filter();
            } else {
                app.active_pane = ActivePane::Editor;
            }
        }
        KeyCode::Down | KeyCode::Up | KeyCode::PageDown | KeyCode::PageUp => {
            app.remote_targets.filter_focused = false;
            let step = if matches!(code, KeyCode::PageDown | KeyCode::PageUp) {
                app.remote_targets.rows.max(1)
            } else {
                1
            };
            if matches!(code, KeyCode::Down | KeyCode::PageDown) {
                app.remote_targets.selected = app.remote_targets.selected.saturating_add(step);
            } else {
                app.remote_targets.selected = app.remote_targets.selected.saturating_sub(step);
            }
            app.remote_targets.clamp();
        }
        KeyCode::Enter if index.is_some() => activate(app, Action::Connect(index.unwrap())),
        KeyCode::F(2) if index.is_some() => activate(app, Action::Edit(index.unwrap())),
        KeyCode::Delete if !app.remote_targets.filter_focused && index.is_some() => {
            activate(app, Action::Remove(index.unwrap()))
        }
        KeyCode::Char('n' | 'N') if !app.remote_targets.filter_focused && !ctrl => {
            activate(app, Action::Add)
        }
        KeyCode::Char('d' | 'D') if ctrl && index.is_some() => {
            activate(app, Action::Duplicate(index.unwrap()))
        }
        KeyCode::F(5) => activate(app, Action::Refresh),
        KeyCode::Tab | KeyCode::BackTab | KeyCode::Char('/')
            if !app.remote_targets.filter_focused =>
        {
            activate(app, Action::Filter)
        }
        KeyCode::Tab | KeyCode::BackTab => app.remote_targets.filter_focused = false,
        KeyCode::Home | KeyCode::End if !app.remote_targets.filter_focused => {
            app.remote_targets.selected = if code == KeyCode::Home {
                0
            } else {
                app.remote_targets.indices().len().saturating_sub(1)
            };
            app.remote_targets.clamp();
        }
        _ => {
            if !app.remote_targets.filter_focused {
                app.focus_text_input(TextInputId::RemoteFilter);
            }
            if app
                .edit_text_input(TextInputId::RemoteFilter, code, modifiers, None)
                .changed
            {
                app.text_input_changed(TextInputId::RemoteFilter);
            }
        }
    }
}

pub(crate) fn handle_shortcut(app: &mut App, key: KeyEvent) -> bool {
    if !app.sidebar_is_visible()
        || app.sidebar_view != SidebarView::Remote
        || app.active_pane != ActivePane::Explorer
        || app.palette.active
        || app.quick_open_active
        || app.menu_active
        || app.tab_context_active
        || crate::ui_modal::active(app).is_some()
    {
        return false;
    }
    if key.kind == crossterm::event::KeyEventKind::Repeat && key.code == KeyCode::Enter {
        return true;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('f' | 'F'))
    {
        activate(app, Action::Filter);
        return true;
    }
    if !app.remote_targets.filter_focused
        && let Some(index) = app.remote_targets.selected_index()
    {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('d' | 'D'))
        {
            activate(app, Action::Duplicate(index));
            return true;
        }
        if key.code == KeyCode::F(10) && key.modifiers.contains(KeyModifiers::SHIFT) {
            let position = app.remote_targets.selected;
            let anchor = app
                .remote_targets
                .hits
                .iter()
                .find(|(_, action)| *action == Action::Select(position))
                .map(|(area, _)| (area.x + 2, area.y + 1))
                .unwrap_or((
                    app.remote_targets.list_area.x,
                    app.remote_targets.list_area.y,
                ));
            activate(app, Action::Menu(index));
            if let Some((_, _, point)) = &mut app.remote_targets.menu {
                *point = anchor;
            }
            return true;
        }
    }
    false
}

pub(crate) fn hover_action(app: &App) -> Option<Action> {
    if !app.sidebar_is_visible() || app.sidebar_view != SidebarView::Remote {
        return None;
    }
    let point = (app.mouse_col, app.mouse_row).into();
    app.explorer_area.filter(|area| area.contains(point))?;
    app.remote_targets
        .hits
        .iter()
        .rev()
        .find(|(area, action)| !matches!(action, Action::ScrollTo(_)) && area.contains(point))
        .map(|(_, action)| *action)
}

pub(crate) fn hover_text(app: &App, action: Action) -> String {
    let target = |index| app.remote_targets.target(index);
    match action {
        Action::Add => {
            if app.remote_targets.can_edit() {
                "Add SSH target · N".into()
            } else {
                "Repair or refresh the configuration before adding targets.".into()
            }
        }
        Action::Refresh => "Reload saved targets from disk · F5".into(),
        Action::OpenConfig => app
            .remote_targets
            .error
            .clone()
            .unwrap_or_else(|| "Open the local remote targets configuration file".into()),
        Action::ClearFilter => "Clear target filter · Esc".into(),
        Action::Menu(_) => "Target actions · Shift+F10".into(),
        Action::Edit(_) => "Edit target · F2".into(),
        Action::Connect(index) => target(index)
            .map(|target| {
                format!(
                    "{} {} · Enter",
                    app.remote_target_connection(&target)
                        .map_or("Connect to", |connection| connection.status.action()),
                    target.name
                )
            })
            .unwrap_or_default(),
        Action::Select(position) => app
            .remote_targets
            .indices()
            .get(position)
            .and_then(|&index| target(index))
            .map(|target| {
                let folder = if !target.remote_command.trim().is_empty() {
                    "Selected by a custom command"
                } else if target.folder.is_empty() {
                    "Remote working directory"
                } else {
                    &target.folder
                };
                format!(
                    "{}\nSSH: {}\nFolder: {}\nEnter Open · F2 Edit · Shift+F10 Actions",
                    target.name, target.host, folder
                )
            })
            .unwrap_or_default(),
        Action::Disconnect => {
            "Close this remote workspace; other open workspaces stay available".into()
        }
        Action::Status => "Show SSH connection status and diagnostics".into(),
        Action::SaveCurrent => "Save the current SSH connection as a reusable target".into(),
        _ => String::new(),
    }
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if !app.sidebar_is_visible()
        || app.sidebar_view != SidebarView::Remote
        || crate::ui_modal::active(app).is_some()
        || app.palette.active
        || app.menu_active
        || app.quick_open_active
    {
        return false;
    }
    let point = (mouse.column, mouse.row).into();
    let Some(area) = app.explorer_area.filter(|area| area.contains(point)) else {
        return false;
    };
    if crate::mouse::explorer_resize_hit(area, app.sidebar_right, mouse.column, mouse.row) {
        return false;
    }
    if matches!(
        mouse.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) {
        let max = app
            .remote_targets
            .indices()
            .len()
            .saturating_sub(app.remote_targets.rows.max(1));
        app.remote_targets.scroll = if mouse.kind == MouseEventKind::ScrollUp {
            app.remote_targets.scroll.saturating_sub(3)
        } else {
            app.remote_targets.scroll.saturating_add(3).min(max)
        };
        return true;
    }
    if mouse.kind == MouseEventKind::Moved {
        app.mouse_hover_changed = true;
        return true;
    }
    if let MouseEventKind::Down(button) = mouse.kind {
        let action = app
            .remote_targets
            .hits
            .iter()
            .rev()
            .find(|(area, _)| area.contains(point))
            .map(|(_, action)| *action);
        crate::ui_panels::capture_pointer(app, button);
        if let Some(action) = action {
            if let Action::Select(position) = action {
                let double = button == MouseButton::Left
                    && app.remote_targets.last_click.is_some_and(|(last, when)| {
                        last == position && when.elapsed() < Duration::from_millis(450)
                    });
                app.remote_targets.last_click =
                    (button == MouseButton::Left).then_some((position, Instant::now()));
                if !matches!(button, MouseButton::Left | MouseButton::Right) {
                    return true;
                }
                activate(app, action);
                if let Some(index) = app.remote_targets.selected_index() {
                    if button == MouseButton::Right {
                        activate(app, Action::Menu(index));
                    } else if button == MouseButton::Left && double {
                        activate(app, Action::Connect(index));
                    }
                }
            } else if button == MouseButton::Left {
                activate(app, action);
            }
        }
    }
    true
}

fn menu_actions(app: &App, index: usize) -> [(Action, &'static str); 4] {
    let label = app
        .remote_targets
        .target(index)
        .as_ref()
        .and_then(|target| app.remote_target_connection(target))
        .map_or("Connect to Target", |connection| match connection.status {
            crate::app_remote_targets::ConnectionStatus::Current => "Open Workspace",
            crate::app_remote_targets::ConnectionStatus::Connected => "Switch to Workspace",
            crate::app_remote_targets::ConnectionStatus::Reconnecting => "Show Connection",
            crate::app_remote_targets::ConnectionStatus::SigningIn => "Show SSH Sign-in",
            crate::app_remote_targets::ConnectionStatus::Failed => "Show Connection Error",
            crate::app_remote_targets::ConnectionStatus::Disconnected => "Reconnect to Target",
        });
    [
        (Action::Connect(index), label),
        (Action::Edit(index), "Edit Target…"),
        (Action::Duplicate(index), "Duplicate Target…"),
        (Action::Remove(index), "Remove Target…"),
    ]
}

pub(crate) fn connection_has_focus(app: &App) -> bool {
    app.active_pane == ActivePane::Editor
        && !app.rendering_inactive_editor
        && matches!(
            app.tabs.get(app.active_tab).map(|tab| tab.kind),
            Some(crate::app::TabKind::RemoteConnection(_))
        )
        && crate::ui_modal::active(app).is_none()
        && !app.palette.active
        && !app.quick_open_active
        && !app.menu_open
        && !app.tab_context_active
        && !app.context_menu_active
        && !app.change_dir_active
        && !app.workspaces.popup.open
        && app.desks.popup.mode.is_none()
}

pub(crate) fn connection_key(app: &mut App, key: KeyEvent) -> bool {
    if !connection_has_focus(app) {
        return false;
    }
    if key.kind == crossterm::event::KeyEventKind::Repeat && key.code == KeyCode::Esc {
        return true;
    }
    if key.code == KeyCode::Esc {
        if app.tabs[app.active_tab].kind == crate::app::TabKind::RemoteConnection(0) {
            app.close_remote_connection_tabs(0);
        } else {
            app.cancel_remote_target_connection();
        }
        return true;
    }
    if app.tabs[app.active_tab].kind == crate::app::TabKind::RemoteConnection(0) {
        return crate::ui_remote_connection::handle_key(app, key);
    }
    if app
        .remote_targets
        .attempt
        .as_ref()
        .is_some_and(|attempt| attempt.error.is_none())
        && ((key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('v' | 'V')))
            || (key.modifiers.contains(KeyModifiers::SHIFT) && key.code == KeyCode::Insert))
    {
        match crate::clipboard::get_text() {
            Ok(text) => {
                app.paste_remote_target_login(&text);
            }
            Err(error) => app.notify_warn(format!("Could not paste into SSH: {error}")),
        }
        return true;
    }
    if let Some(attempt) = &mut app.remote_targets.attempt {
        if attempt.error.is_some() {
            if key.kind == crossterm::event::KeyEventKind::Repeat && key.code == KeyCode::Enter {
                return true;
            }
            if matches!(
                key.code,
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right
            ) {
                let reverse = key.code == KeyCode::BackTab
                    || key.code == KeyCode::Left
                    || key.modifiers.contains(KeyModifiers::SHIFT);
                attempt.focus = (attempt.focus.min(3) + if reverse { 3 } else { 1 }) % 4;
            } else if key.code == KeyCode::Enter {
                let action = [
                    Action::Retry,
                    Action::EditAttempt,
                    Action::CopyLog,
                    Action::Cancel,
                ][attempt.focus.min(3)];
                activate(app, action);
            } else if key.code == KeyCode::F(2) {
                activate(app, Action::EditAttempt);
            }
        } else if let Some(setup) = &attempt.setup {
            match setup.send_key(key, &attempt.grid) {
                Ok(()) => {
                    attempt.input_sent = true;
                    attempt.input_error = None;
                }
                Err(error) => attempt.input_error = Some(format!("Could not send input: {error}")),
            }
        }
        return true;
    }
    true
}

pub(crate) fn connection_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if crate::ui_modal::active(app).is_some() || app.palette.active || app.quick_open_active {
        return false;
    }
    let area = app.remote_targets.connection_area;
    if !area.contains((mouse.column, mouse.row).into()) {
        return false;
    }
    if !matches!(
        app.tabs.get(app.active_tab).map(|tab| tab.kind),
        Some(crate::app::TabKind::RemoteConnection(_))
    ) {
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
                .remote_targets
                .connection_hits
                .iter()
                .find(|(area, _)| area.contains((mouse.column, mouse.row).into()))
                .map(|(_, action)| *action);
            if let Some(action) = action {
                activate(app, action);
            }
        }
    }
    true
}

pub(crate) fn modal_key(app: &mut App, key: KeyEvent) {
    if key.kind == crossterm::event::KeyEventKind::Repeat
        && matches!(key.code, KeyCode::Enter | KeyCode::Esc)
    {
        return;
    }
    if key.code == KeyCode::Esc {
        app.cancel_remote_target_dialog();
        return;
    }
    if let Some((index, selected, anchor)) = app.remote_targets.menu {
        let actions = menu_actions(app, index);
        let choices: Vec<_> = actions
            .iter()
            .enumerate()
            .filter(|(_, (action, _))| enabled(app, *action))
            .map(|(index, _)| index)
            .collect();
        let position = choices
            .iter()
            .position(|&choice| choice == selected)
            .unwrap_or(0);
        let mut next = selected;
        match key.code {
            KeyCode::Up | KeyCode::BackTab => {
                if !choices.is_empty() {
                    next = choices[(position + choices.len() - 1) % choices.len()];
                }
            }
            KeyCode::Down | KeyCode::Tab => {
                if !choices.is_empty() {
                    next = choices[(position
                        + if key.modifiers.contains(KeyModifiers::SHIFT) {
                            choices.len() - 1
                        } else {
                            1
                        })
                        % choices.len()];
                }
            }
            KeyCode::Home => next = choices.first().copied().unwrap_or(0),
            KeyCode::End => next = choices.last().copied().unwrap_or(0),
            KeyCode::Enter => {
                let action = actions[selected].0;
                activate(app, action);
                return;
            }
            _ => {}
        }
        app.remote_targets.menu = Some((index, next, anchor));
        return;
    }
    if app.remote_targets.remove.is_some() {
        match key.code {
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right => {
                app.modal.focus = 1 - app.modal.focus.min(1)
            }
            KeyCode::Enter if app.modal.focus == 0 => app.remove_remote_target(),
            KeyCode::Enter => app.cancel_remote_target_dialog(),
            _ => {}
        }
        return;
    }
    let Some(form) = &mut app.remote_targets.form else {
        return;
    };
    match key.code {
        KeyCode::PageUp | KeyCode::PageDown => {
            let step = app.remote_targets.dialog_body.height.max(1) as usize;
            form.reveal = false;
            form.scroll = if key.code == KeyCode::PageUp {
                form.scroll.saturating_sub(step)
            } else {
                (form.scroll + step).min(app.remote_targets.dialog_max_scroll)
            };
        }
        KeyCode::Tab | KeyCode::BackTab => {
            let order = form.order();
            let position = order
                .iter()
                .position(|&focus| focus == form.focus)
                .unwrap_or(0);
            let reverse =
                key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
            form.focus =
                order[(position + if reverse { order.len() - 1 } else { 1 }) % order.len()];
            form.reveal = true;
        }
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) && !form.conflict => {
            app.save_remote_target(true)
        }
        KeyCode::Enter => {
            let action = match form.focus {
                5 => Action::Advanced,
                7 => Action::Save(true),
                8 => Action::Cancel,
                9 => Action::ReloadForm,
                10 => Action::SaveCopy,
                _ => Action::Save(false),
            };
            activate(app, action);
        }
        KeyCode::Char('s' | 'S')
            if key.modifiers.contains(KeyModifiers::CONTROL) && !form.conflict =>
        {
            app.save_remote_target(false)
        }
        KeyCode::Char(' ') if form.focus == 5 => activate(app, Action::Advanced),
        _ if form.focus < 5 => {
            let id = TextInputId::RemoteTarget(form.focus);
            if app
                .edit_text_input(id, key.code, key.modifiers, None)
                .changed
            {
                app.text_input_changed(id);
            }
        }
        _ => {}
    }
}

pub(crate) fn modal_mouse(app: &mut App, mouse: MouseEvent) {
    if app.handle_overlay_text_input_mouse(mouse.kind, mouse.column, mouse.row, mouse.modifiers) {
        return;
    }
    let point = (mouse.column, mouse.row).into();
    if mouse.kind == MouseEventKind::Moved {
        app.mouse_hover_changed = true;
        if let Some((index, _, anchor)) = app.remote_targets.menu {
            let action = app
                .remote_targets
                .dialog_hits
                .iter()
                .find(|(area, _)| area.contains(point))
                .map(|(_, action)| *action);
            if let Some(selected) = menu_actions(app, index)
                .iter()
                .position(|(candidate, _)| Some(*candidate) == action && enabled(app, *candidate))
            {
                app.remote_targets.menu = Some((index, selected, anchor));
            }
        }
    }
    if matches!(
        mouse.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) {
        if let Some(form) = &mut app.remote_targets.form {
            form.reveal = false;
            form.scroll = if mouse.kind == MouseEventKind::ScrollUp {
                form.scroll.saturating_sub(3)
            } else {
                (form.scroll + 3).min(app.remote_targets.dialog_max_scroll)
            };
        }
    }
    if let MouseEventKind::Down(button) = mouse.kind {
        crate::ui_panels::capture_pointer(app, button);
        if button == MouseButton::Left {
            if let Some(action) = app
                .remote_targets
                .dialog_hits
                .iter()
                .rev()
                .find(|(area, _)| area.contains(point))
                .map(|(_, action)| *action)
            {
                activate(app, action);
            } else if app.remote_targets.menu.is_some() {
                app.cancel_remote_target_dialog();
            }
        }
    }
}

pub(crate) fn draw_dialog(f: &mut Frame, app: &mut App) {
    app.remote_targets.dialog_hits.clear();
    app.remove_text_input_regions(|id| matches!(id, TextInputId::RemoteTarget(_)));
    let screen = f.area();
    if let Some((index, selected, (x, y))) = app.remote_targets.menu {
        let width = screen.width.min(27);
        let height = screen.height.min(6);
        let area = Rect::new(
            x.min(screen.right().saturating_sub(width)),
            y.min(screen.bottom().saturating_sub(height)),
            width,
            height,
        );
        let style = Style::default()
            .fg(app.colors.fg_text)
            .bg(app.colors.ui.bg_widget);
        let block = Block::bordered().style(style);
        let inner = block.inner(area);
        f.render_widget(Clear, area);
        f.render_widget(block, area);
        for (row, (action, label)) in menu_actions(app, index)
            .into_iter()
            .enumerate()
            .take(inner.height as usize)
        {
            button(
                f,
                app,
                Rect::new(inner.x, inner.y + row as u16, inner.width, 1),
                label,
                action,
                false,
                row == selected,
                true,
            );
        }
        return;
    }
    let removing = app.remote_targets.remove.is_some();
    let desired_height = if removing {
        12
    } else {
        form::preferred_height(app, screen.width.saturating_sub(4).min(84))
    };
    let width = screen
        .width
        .saturating_sub(4)
        .min(84)
        .max(screen.width.min(4));
    let height = screen
        .height
        .saturating_sub(2)
        .min(desired_height)
        .max(screen.height.min(2));
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    app.modal.area = area;
    let title = if removing {
        "Remove Remote Target"
    } else if app
        .remote_targets
        .form
        .as_ref()
        .is_some_and(|form| form.original.is_some())
    {
        "Edit Remote Target"
    } else {
        "Add Remote Target"
    };
    let bg = app.colors.ui.bg_widget;
    let style = Style::default().fg(app.colors.fg_text).bg(bg);
    let title = crate::ui_pickers::truncate_to_width(title, width.saturating_sub(8) as usize);
    let block = Block::bordered().title(format!(" {title} ")).style(style);
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    if area.width >= 8 {
        let close = Rect::new(area.right() - 4, area.y, 3, 1);
        app.modal.close = close;
        button(f, app, close, "×", Action::Cancel, false, false, true);
    }
    if inner.is_empty() {
        return;
    }
    let content = Rect::new(
        inner.x + u16::from(inner.width > 2),
        inner.y,
        inner.width.saturating_sub(2),
        inner.height,
    );
    if content.is_empty() {
        return;
    }
    if removing {
        draw_remove(f, app, content, style);
    } else {
        draw_form(f, app, content, style);
    }
}

#[path = "ui_remote_targets_form.rs"]
mod form;
use form::draw_form;

fn draw_remove(f: &mut Frame, app: &mut App, content: Rect, style: Style) {
    let target = app.remote_targets.remove.as_ref().unwrap();
    line(
        f,
        content,
        content.y + 1,
        format!("Remove '{}' from saved targets?", target.name),
        style.add_modifier(Modifier::BOLD),
    );
    if content.height > 5 {
        f.render_widget(
            Paragraph::new("Open workspaces stay connected. You can add this target again later.")
                .style(style)
                .wrap(Wrap { trim: true }),
            Rect::new(content.x, content.y + 3, content.width, 2),
        );
    }
    if let Some(error) = &app.remote_targets.remove_error {
        line(
            f,
            content,
            content.bottom().saturating_sub(3),
            error,
            style.fg(app.colors.ui.error_text_color(style.bg.unwrap())),
        );
    }
    let y = content.bottom().saturating_sub(1);
    let width = content.width.min(12);
    button(
        f,
        app,
        Rect::new(content.x, y, width, 1),
        "Remove",
        Action::ConfirmRemove,
        false,
        app.modal.focus == 0,
        true,
    );
    if content.width > width + 1 {
        button(
            f,
            app,
            Rect::new(
                content.x + width + 1,
                y,
                (content.width - width - 1).min(12),
                1,
            ),
            "Cancel",
            Action::Cancel,
            false,
            app.modal.focus != 0,
            true,
        );
    }
}

pub(crate) fn draw_connection_tab(f: &mut Frame, app: &mut App, area: Rect, id: u64) {
    app.code_area = None;
    app.gutter_area = None;
    app.minimap_area = None;
    app.scrollbar_area = None;
    app.h_scrollbar_area = None;
    let interactive = !app.rendering_inactive_editor;
    if interactive {
        app.remote_targets.connection_area = area;
        app.remote_targets.connection_hits.clear();
    }
    if id == 0 {
        crate::ui_remote_connection::draw_in_area(f, app, area);
        return;
    }
    let style = Style::default()
        .fg(app.colors.fg_text)
        .bg(app.colors.bg_editor);
    f.render_widget(Paragraph::new("").style(style), area);
    if !app
        .remote_targets
        .attempt
        .as_ref()
        .is_some_and(|attempt| attempt.id == id)
    {
        return;
    }
    let content = area.inner(ratatui::layout::Margin::new(
        u16::from(area.width > 4) * 2,
        u16::from(area.height > 10),
    ));
    let dialog_hits = std::mem::take(&mut app.remote_targets.dialog_hits);
    draw_connection(
        f,
        app,
        content,
        style,
        interactive && connection_has_focus(app),
    );
    let hits = std::mem::replace(&mut app.remote_targets.dialog_hits, dialog_hits);
    if interactive {
        app.remote_targets.connection_hits = hits;
    }
}

fn draw_connection(f: &mut Frame, app: &mut App, content: Rect, style: Style, focused: bool) {
    let attempt = app.remote_targets.attempt.as_ref().unwrap();
    let failed = attempt.error.is_some();
    let actions: &[(&str, Action)] = if failed {
        &[
            ("Retry", Action::Retry),
            ("Edit Target", Action::EditAttempt),
            ("Copy Log", Action::CopyLog),
            ("Close", Action::Cancel),
        ]
    } else {
        &[("Cancel", Action::Cancel), ("Copy Log", Action::CopyLog)]
    };
    let needed: usize = actions.iter().map(|(label, _)| label.width() + 5).sum();
    let stacked = needed.saturating_sub(1) > content.width as usize;
    let footer_rows = if stacked { actions.len() as u16 } else { 1 };
    let footer_y = content.bottom().saturating_sub(footer_rows).max(content.y);
    line(
        f,
        content,
        content.y,
        format!("{} · {}", attempt.target.name, attempt.target.host),
        style.add_modifier(Modifier::BOLD),
    );
    let message = if failed {
        attempt.error.clone().unwrap_or_default()
    } else if let Some(error) = &attempt.input_error {
        error.clone()
    } else if attempt.input_sent {
        "Input sent to SSH. Password characters stay hidden.".into()
    } else if attempt.setup.is_some() && !focused {
        "Waiting for SSH sign-in · select this tab to type".into()
    } else if attempt.setup.is_some() {
        "SSH sign-in · keyboard input is active".into()
    } else {
        "Starting SSH…".into()
    };
    let error_style = style.fg(app.colors.ui.error_text_color(style.bg.unwrap()));
    if content.height > 3 {
        let message_rows = if failed { 3 } else { 1 };
        f.render_widget(
            Paragraph::new(message)
                .style(if failed || attempt.input_error.is_some() {
                    error_style
                } else {
                    style
                })
                .wrap(Wrap { trim: false }),
            Rect::new(
                content.x,
                content.y + 1,
                content.width,
                message_rows.min(content.height - 2),
            ),
        );
    }
    let mut terminal_y = content.y + if failed { 5 } else { 3 };
    if !failed && terminal_y < footer_y {
        let help = "Passwords and passphrases are hidden by SSH. Type, then Enter. Ctrl+V pastes; Esc cancels.";
        let lines = App::wrap_setting_text_for_width(help, content.width.max(1) as usize);
        let available = footer_y.saturating_sub(terminal_y + 3).max(1) as usize;
        for hint in lines.iter().take(available) {
            line(
                f,
                content,
                terminal_y,
                hint,
                style.fg(app.colors.ui.description_color(style.bg.unwrap())),
            );
            terminal_y += 1;
        }
        terminal_y += 1;
    }
    if footer_y > terminal_y {
        let area = Rect::new(content.x, terminal_y, content.width, footer_y - terminal_y);
        let terminal_bg = app.colors.ui.bg_terminal;
        let border = app
            .colors
            .ui
            .focus_border
            .border(terminal_bg)
            .unwrap_or(app.colors.fg_accent_blue);
        let block = Block::bordered()
            .title(if failed {
                " SSH output "
            } else if focused {
                " SSH · type here "
            } else {
                " SSH sign-in "
            })
            .style(
                Style::default()
                    .fg(app.colors.ui.fg_terminal)
                    .bg(terminal_bg),
            )
            .border_style(
                Style::default()
                    .fg(if failed || !focused {
                        app.colors.fg_dim
                    } else {
                        border
                    })
                    .bg(terminal_bg),
            );
        let viewport = block.inner(area);
        f.render_widget(block, area);
        let attempt = app.remote_targets.attempt.as_mut().unwrap();
        if !app.rendering_inactive_editor
            && !viewport.is_empty()
            && (attempt.grid.width != viewport.width.max(2) as usize
                || attempt.grid.height != viewport.height.max(2) as usize)
        {
            if let Some(setup) = &attempt.setup {
                let _ = setup.resize(viewport.width, viewport.height);
            }
            attempt.grid.resize(
                viewport.width.max(2) as usize,
                viewport.height.max(2) as usize,
            );
        }
        let first_row = (attempt.grid.cursor_row + 1).saturating_sub(viewport.height as usize);
        let first_column = (attempt.grid.cursor_col + 1).saturating_sub(viewport.width as usize);
        attempt.rows.paint_viewport(
            f.buffer_mut(),
            &attempt.grid,
            viewport,
            &app.colors.ui,
            first_row,
            first_column,
        );
        if focused && !failed && attempt.grid.cursor_visible && !viewport.is_empty() {
            f.set_cursor_position((
                viewport.x
                    + (attempt.grid.cursor_col - first_column).min(viewport.width as usize - 1)
                        as u16,
                viewport.y
                    + (attempt.grid.cursor_row - first_row).min(viewport.height as usize - 1)
                        as u16,
            ));
        }
    }
    let mut x = content.x;
    let mut y = footer_y;
    for (index, &(label, action)) in actions.iter().enumerate() {
        let width = (label.width() as u16 + 4).min(content.width);
        if y < content.bottom() {
            button(
                f,
                app,
                Rect::new(x, y, width, 1),
                label,
                action,
                action == Action::Retry,
                focused && failed && app.remote_targets.attempt.as_ref().unwrap().focus == index,
                true,
            );
        }
        if stacked {
            y = y.saturating_add(1);
        } else {
            x = x.saturating_add(width + 1);
        }
    }
}
