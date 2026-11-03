//! Compact workspace switcher; management lives in the saved-sets tab.
use crate::{app::App, app_workspaces::Request, ui_workspace_picker as picker};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{Frame, layout::Rect, widgets::Paragraph};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub(crate) fn escaped(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.chars().any(char::is_control) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut escaped = String::new();
    for ch in text.chars() {
        if ch.is_control() {
            escaped.extend(ch.escape_default());
        } else {
            escaped.push(ch);
        }
    }
    std::borrow::Cow::Owned(escaped)
}
pub(crate) fn clipped(text: &str, width: usize) -> String {
    let safe = escaped(text);
    if safe.width() <= width {
        return safe.into_owned();
    }
    let mut remaining = width.saturating_sub(1);
    let mut result = String::new();
    for grapheme in safe.graphemes(true) {
        if grapheme.width() > remaining {
            break;
        }
        result.push_str(grapheme);
        remaining -= grapheme.width();
    }
    if width > 0 {
        result.push('…');
    }
    result
}
#[derive(Default)]
pub(crate) struct Popup {
    pub sets: crate::ui_workspace_sets::Popup,
    pub open: bool,
    pub close: bool,
    pub selected: usize,
    pub scroll: usize,
    pub area: Rect,
    pub tabs: [Rect; 2],
    pub dismiss: Rect,
    pub search: picker::Search,
    hits: Vec<(Rect, usize)>,
}
pub(crate) fn open(app: &mut App) {
    if app.paste_receiving() {
        app.notify_warn("Finish or cancel the incoming paste before switching workspaces");
        return;
    }
    if crate::ui_modal::active(app).is_some()
        || app.change_dir_active
        || app.workspaces.exit.is_some()
    {
        return;
    }
    app.clear_drag_states();
    app.clear_tab_tooltip();
    app.menu_active = false;
    app.menu_open = false;
    app.palette.active = false;
    app.desks.popup = Default::default();
    let selected = app
        .workspace_entries()
        .iter()
        .position(|entry| entry.active)
        .unwrap_or(0);
    app.workspaces.popup = Popup {
        open: true,
        selected,
        ..Default::default()
    };
}
pub(crate) fn dismiss(app: &mut App) {
    if matches!(
        app.workspaces.popup.sets.mode,
        Some(crate::ui_workspace_sets::Mode::Confirm)
    ) {
        app.cancel_workspace_set_switch();
    }
    app.workspaces.popup = Default::default();
}
pub(crate) fn add(app: &mut App) {
    if app.paste_receiving() {
        app.notify_warn("Finish or cancel the incoming paste before adding a workspace");
        return;
    }
    if crate::ui_modal::active(app).is_some() || app.workspaces.exit.is_some() {
        return;
    }
    dismiss(app);
    app.open_folder_browser();
    app.folder_browser.add_workspace = true;
}
pub(crate) fn close(app: &mut App) {
    open(app);
    if app.workspaces.popup.open {
        app.workspaces.popup.close = true;
        app.workspaces.popup.selected = 1;
    }
}
fn entries(app: &App) -> Vec<crate::app_workspaces::Entry> {
    let query = app.workspaces.popup.search.text.trim();
    app.workspace_entries()
        .into_iter()
        .filter(|entry| {
            query.is_empty()
                || crate::app::contains_ignore_case(&entry.name, query)
                || crate::app::contains_ignore_case(&entry.path, query)
        })
        .collect()
}
fn activate(app: &mut App, index: usize) {
    if app.workspaces.popup.close {
        if index == 0 {
            dismiss(app);
            app.workspaces.request = Some(Request::Close);
        } else {
            open(app);
        }
        return;
    }
    let entries = entries(app);
    let choices = entries.len().max(1);
    if let Some(entry) = entries.get(index) {
        let id = entry.id;
        dismiss(app);
        app.workspaces.request = Some(Request::Switch(id));
    } else if index == choices {
        add(app);
    } else if index == choices + 1 {
        crate::ui_workspace_sets::save(app);
    } else if index == choices + 2 {
        close(app);
    }
}
pub(crate) fn filter_key(app: &mut App, key: KeyEvent) -> bool {
    if matches!(
        key.code,
        KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Left | KeyCode::Right
    ) || matches!(key.code, KeyCode::Delete | KeyCode::Home | KeyCode::End)
        && !app.workspaces.popup.search.text.is_empty()
    {
        if app.workspaces.popup.search.edit(key, None) {
            app.workspaces.popup.selected = 0;
            app.workspaces.popup.scroll = 0;
        }
        return true;
    }
    false
}
pub(crate) fn paste_filter(app: &mut App, text: &str) {
    let text: String = text
        .chars()
        .filter(|ch| !ch.is_control())
        .take(256)
        .collect();
    if app.workspaces.popup.search.edit(
        KeyEvent::new(KeyCode::Null, KeyModifiers::NONE),
        Some(&text),
    ) {
        app.workspaces.popup.selected = 0;
        app.workspaces.popup.scroll = 0;
    }
}
pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if !app.workspaces.popup.open {
        return false;
    }
    if crate::ui_modal::active(app).is_some() {
        dismiss(app);
        return false;
    }
    if key.kind == KeyEventKind::Release
        || key.kind == KeyEventKind::Repeat && key.code == KeyCode::Enter
    {
        return true;
    }
    let list = !app.workspaces.popup.close
        && (app.workspaces.popup.sets.mode.is_none()
            || matches!(
                app.workspaces.popup.sets.mode,
                Some(crate::ui_workspace_sets::Mode::List)
            ));
    if list
        && matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
        && key.modifiers.contains(KeyModifiers::CONTROL)
    {
        if app.workspaces.popup.sets.mode.is_some() {
            open(app);
        } else {
            crate::ui_workspace_sets::open(app);
        }
        return true;
    }
    if list && key.code == KeyCode::Esc && !app.workspaces.popup.search.text.is_empty() {
        app.workspaces.popup.search = Default::default();
        app.workspaces.popup.selected = 0;
        app.workspaces.popup.scroll = 0;
        return true;
    }
    if crate::ui_workspace_sets::handle_key(app, key) {
        return true;
    }
    if !app.workspaces.popup.close && filter_key(app, key) {
        return true;
    }
    let count = if app.workspaces.popup.close {
        2
    } else {
        entries(app).len().max(1) + 3
    };
    match key.code {
        KeyCode::Esc => dismiss(app),
        KeyCode::Up | KeyCode::BackTab => {
            app.workspaces.popup.selected = (app.workspaces.popup.selected + count - 1) % count
        }
        KeyCode::Down | KeyCode::Tab => {
            app.workspaces.popup.selected = (app.workspaces.popup.selected + 1) % count
        }
        KeyCode::Home => app.workspaces.popup.selected = 0,
        KeyCode::End => app.workspaces.popup.selected = count - 1,
        KeyCode::Enter => activate(app, app.workspaces.popup.selected),
        _ => {}
    }
    true
}
pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if !app.workspaces.popup.open {
        return false;
    }
    if crate::ui_modal::active(app).is_some() {
        dismiss(app);
        return false;
    }
    let point = (mouse.column, mouse.row).into();
    app.mouse_hover_changed |= (app.mouse_col, app.mouse_row) != (mouse.column, mouse.row);
    app.mouse_col = mouse.column;
    app.mouse_row = mouse.row;
    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        let saved = app.workspaces.popup.tabs[1].contains(point);
        let workspaces = app.workspaces.popup.tabs[0].contains(point);
        let sets_button = crate::ui_chrome::saved_workspace_sets_area(app)
            .is_some_and(|rect| rect.contains(point));
        let controls = crate::ui_chrome::workspace_controls(app);
        let directory_button = controls.is_some_and(|(rect, _)| rect.contains(point));
        if app.workspaces.popup.dismiss.contains(point) {
            dismiss(app);
            return true;
        }
        if sets_button || saved {
            if sets_button
                && matches!(
                    app.workspaces.popup.sets.mode,
                    Some(crate::ui_workspace_sets::Mode::List)
                )
            {
                dismiss(app);
            } else {
                dismiss(app);
                crate::ui_workspace_sets::open(app);
            }
            return true;
        }
        if directory_button || workspaces {
            if directory_button
                && app.workspaces.popup.sets.mode.is_none()
                && !app.workspaces.popup.close
            {
                dismiss(app);
            } else {
                dismiss(app);
                open(app);
            }
            return true;
        }
        if controls.is_some_and(|(_, rect)| rect.contains(point)) {
            add(app);
            return true;
        }
    }
    let list = !app.workspaces.popup.close
        && (app.workspaces.popup.sets.mode.is_none()
            || matches!(
                app.workspaces.popup.sets.mode,
                Some(crate::ui_workspace_sets::Mode::List)
            ));
    if list && app.workspaces.popup.search.mouse(mouse) {
        return true;
    }
    if crate::ui_workspace_sets::handle_mouse(app, mouse) {
        return true;
    }
    let hit = app
        .workspaces
        .popup
        .hits
        .iter()
        .find(|(rect, _)| rect.contains(point))
        .map(|(_, index)| *index);
    let count = if app.workspaces.popup.close {
        2
    } else {
        entries(app).len().max(1) + 3
    };
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(index) = hit {
                activate(app, index);
            } else if !app.workspaces.popup.area.contains(point) {
                dismiss(app);
            }
        }
        MouseEventKind::Down(_) if !app.workspaces.popup.area.contains(point) => dismiss(app),
        MouseEventKind::Moved => {
            if let Some(index) = hit {
                app.workspaces.popup.selected = index;
            }
        }
        MouseEventKind::ScrollUp => {
            app.workspaces.popup.selected = app.workspaces.popup.selected.saturating_sub(1)
        }
        MouseEventKind::ScrollDown => {
            app.workspaces.popup.selected = (app.workspaces.popup.selected + 1).min(count - 1)
        }
        _ => {}
    }
    true
}
pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    if !app.workspaces.popup.open {
        return;
    }
    if app.workspaces.popup.sets.mode.is_some() {
        crate::ui_workspace_sets::draw(f, app);
        return;
    }
    app.workspaces.popup.hits.clear();
    if app.workspaces.popup.close {
        draw_close(f, app);
        return;
    }
    let entries = entries(app);
    let count = entries.len().max(1);
    app.workspaces.popup.selected = app.workspaces.popup.selected.min(count + 2);
    let Some(layout) = picker::list(f, app, false, count, 3) else {
        return;
    };
    let selected = app.workspaces.popup.selected;
    let start = picker::scroll(
        &mut app.workspaces.popup.scroll,
        selected,
        count,
        layout.choices.height,
    );
    for (offset, index) in (start..count)
        .take(layout.choices.height as usize)
        .enumerate()
    {
        let row = Rect::new(
            layout.choices.x,
            layout.choices.y + offset as u16,
            layout.choices.width,
            1,
        );
        if let Some(entry) = entries.get(index) {
            let marker = if entry.active { "✓" } else { " " };
            let badge = if entry.attention {
                "! Attention".into()
            } else if entry.unread > 0 {
                format!("● {} new", entry.unread)
            } else if entry.running {
                "● Running".into()
            } else {
                String::new()
            };
            let duplicate = entries
                .iter()
                .filter(|other| other.name == entry.name)
                .count()
                > 1;
            let label = if duplicate {
                let room = row.width as usize;
                let path = crate::ui_performance::clip_middle(
                    &entry.path,
                    room.saturating_sub(entry.name.width() + badge.width() + 9),
                );
                format!("{marker} {} · {path}", entry.name)
            } else {
                format!("{marker} {}", entry.name)
            };
            picker::choice(f, app, row, &label, &badge, selected == index, true);
            app.workspaces.popup.hits.push((row, index));
        } else {
            picker::choice(f, app, row, "No matching workspaces", "", false, false);
        }
    }
    let action_start = selected
        .saturating_sub(count)
        .saturating_sub(layout.actions.height.saturating_sub(1) as usize);
    for (offset, (index, label)) in ["+ Add workspace…", "Save as set…", "Close workspace…"]
        .iter()
        .enumerate()
        .skip(action_start)
        .take(layout.actions.height as usize)
        .enumerate()
    {
        let row = Rect::new(
            layout.actions.x,
            layout.actions.y + offset as u16,
            layout.actions.width,
            1,
        );
        picker::choice(f, app, row, label, "", selected == count + index, true);
        app.workspaces.popup.hits.push((row, count + index));
    }
    if let Some(entry) = entries.get(selected) {
        picker::detail(
            f,
            app,
            Rect::new(
                layout.detail.x,
                layout.detail.y,
                layout.detail.width,
                layout.detail.height.min(1),
            ),
            &entry.path,
        );
        if layout.detail.height > 1 {
            picker::detail(
                f,
                app,
                Rect::new(layout.detail.x, layout.detail.y + 1, layout.detail.width, 1),
                &entry.last,
            );
        }
    }
}
fn draw_close(f: &mut Frame, app: &mut App) {
    let name = app
        .workspace_entries()
        .into_iter()
        .find(|entry| entry.active)
        .map_or("workspace".into(), |entry| entry.name);
    let quitting = app.workspaces.parked.is_empty();
    let jobs = app.chat.any_agent_running()
        || app
            .terminals
            .iter()
            .any(|terminal| terminal.terminal_pty.is_some());
    let mut lines = vec![format!("Close “{name}”?")];
    if jobs {
        lines.push("Its terminals and agents will stop.".into());
    }
    if app.unsaved_file_count() > 0 {
        lines.push("Unsaved edits will be kept.".into());
    }
    let Some(inner) = picker::surface(
        f,
        app,
        if quitting {
            "Quit VTCode"
        } else {
            "Close workspace"
        },
        lines.len() + 4,
    ) else {
        return;
    };
    f.render_widget(
        Paragraph::new(lines.join("\n")).style(picker::base(app)),
        Rect::new(
            inner.x + 1,
            inner.y,
            inner.width.saturating_sub(2),
            inner.height.saturating_sub(1),
        ),
    );
    let selected = app.workspaces.popup.selected;
    let hits = picker::buttons(
        f,
        app,
        Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        if quitting { "Quit" } else { "Close" },
        selected == 0,
        selected == 1,
        true,
    );
    app.workspaces.popup.hits.extend(
        hits.into_iter()
            .enumerate()
            .map(|(index, rect)| (rect, index)),
    );
}
