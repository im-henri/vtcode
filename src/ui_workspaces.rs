//! Switch between open folders and manage saved workspaces.
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
    pub close_target: Option<u64>,
    close_return_selected: usize,
    pub selected: usize,
    pub scroll: usize,
    pub area: Rect,
    pub tabs: [Rect; 2],
    pub dismiss: Rect,
    pub search: picker::Search,
    hits: Vec<(Rect, usize)>,
    pub(crate) close_hits: Vec<(Rect, u64)>,
}
pub(crate) fn open(app: &mut App) {
    if app.paste_receiving() {
        app.notify_warn("Finish or cancel the incoming paste before switching folders");
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
    app.load_workspace_sets(false);
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
pub(crate) fn open_folder(app: &mut App) {
    choose_folder(app, false);
}
pub(crate) fn add(app: &mut App) {
    choose_folder(app, true);
}
fn choose_folder(app: &mut App, add: bool) {
    if app.paste_receiving() {
        app.notify_warn("Finish or cancel the incoming paste before opening a folder");
        return;
    }
    if crate::ui_modal::active(app).is_some() || app.workspaces.exit.is_some() {
        return;
    }
    dismiss(app);
    app.open_folder_browser();
    app.folder_browser.add_workspace = add;
}
pub(crate) fn close(app: &mut App) {
    if !can_close(app) {
        return;
    }
    close_folder(app, app.workspaces.active);
}
pub(crate) fn close_folder(app: &mut App, id: u64) {
    if !app
        .workspace_entries()
        .iter()
        .any(|entry| entry.id == id && !entry.path.is_empty())
    {
        return;
    }
    let selected = app.workspaces.popup.selected;
    let search = std::mem::take(&mut app.workspaces.popup.search);
    open(app);
    if app.workspaces.popup.open {
        app.workspaces.popup.close = true;
        app.workspaces.popup.close_target = Some(id);
        app.workspaces.popup.close_return_selected = selected;
        app.workspaces.popup.search = search;
        app.workspaces.popup.selected = 1;
    }
}
fn cancel_close(app: &mut App) {
    app.workspaces.popup.close = false;
    app.workspaces.popup.close_target = None;
    app.workspaces.popup.selected = app.workspaces.popup.close_return_selected;
}
pub(crate) fn can_close(app: &App) -> bool {
    app.has_open_folder()
}
fn actions(app: &App) -> &'static [&'static str] {
    if app.open_folder_count() == 0 {
        &["+ Open folder…"]
    } else {
        &["+ Add folder…", "Save workspace…"]
    }
}
fn entries(app: &App) -> Vec<crate::app_workspaces::Entry> {
    let query = app.workspaces.popup.search.text.trim();
    app.workspace_entries()
        .into_iter()
        .filter(|entry| !entry.path.is_empty() || app.open_folder_count() > 0)
        .filter(|entry| {
            query.is_empty()
                || crate::app::contains_ignore_case(&entry.name, query)
                || crate::app::contains_ignore_case(&entry.path, query)
        })
        .collect()
}
fn activate(app: &mut App, index: usize) {
    if app.workspaces.popup.close {
        let target = app
            .workspaces
            .popup
            .close_target
            .unwrap_or(app.workspaces.active);
        if index == 0
            && app
                .workspace_entries()
                .iter()
                .any(|entry| entry.id == target && !entry.path.is_empty())
        {
            dismiss(app);
            app.workspaces.request = Some(Request::CloseFolder(target));
        } else {
            cancel_close(app);
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
        if app.open_folder_count() == 0 {
            open_folder(app);
        } else {
            add(app);
        }
    } else if index == choices + 1 {
        crate::ui_workspace_sets::save(app);
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
    if list
        && app.workspaces.popup.sets.mode.is_none()
        && key.code == KeyCode::Delete
        && app.workspaces.popup.search.text.is_empty()
        && let Some(entry) = entries(app).get(app.workspaces.popup.selected)
    {
        close_folder(app, entry.id);
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
        entries(app).len().max(1) + actions(app).len()
    };
    match key.code {
        KeyCode::Esc if app.workspaces.popup.close => cancel_close(app),
        KeyCode::Left | KeyCode::Right if app.workspaces.popup.close => {
            app.workspaces.popup.selected = 1 - app.workspaces.popup.selected.min(1)
        }
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
    if mouse.kind == MouseEventKind::Down(MouseButton::Right)
        && [
            crate::text_input::TextInputId::WorkspaceFilter,
            crate::text_input::TextInputId::WorkspaceName,
        ]
        .into_iter()
        .filter_map(|id| app.text_input_area(id))
        .any(|area| area.contains((mouse.column, mouse.row).into()))
        && app.handle_overlay_text_input_mouse(mouse.kind, mouse.column, mouse.row, mouse.modifiers)
    {
        return true;
    }
    let point = (mouse.column, mouse.row).into();
    app.mouse_hover_changed |= (app.mouse_col, app.mouse_row) != (mouse.column, mouse.row);
    app.mouse_col = mouse.column;
    app.mouse_row = mouse.row;
    if !app.workspaces.popup.close
        && mouse.kind == MouseEventKind::Down(MouseButton::Left)
        && let Some((_, id)) = app
            .workspaces
            .popup
            .close_hits
            .iter()
            .find(|(rect, _)| rect.contains(point))
            .copied()
    {
        close_folder(app, id);
        return true;
    }
    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        let saved = app.workspaces.popup.tabs[1].contains(point);
        let workspaces = app.workspaces.popup.tabs[0].contains(point);
        let directory_button = crate::ui_chrome::workspace_button_area(app)
            .is_some_and(|rect| rect.contains(point));
        if app.workspaces.popup.dismiss.contains(point) {
            dismiss(app);
            return true;
        }
        if saved {
            if app.workspaces.popup.sets.mode.is_none() {
                dismiss(app);
                crate::ui_workspace_sets::open(app);
            }
            return true;
        }
        if directory_button || workspaces {
            if directory_button {
                dismiss(app);
            } else if app.workspaces.popup.sets.mode.is_some() || app.workspaces.popup.close {
                dismiss(app);
                open(app);
            }
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
        entries(app).len().max(1) + actions(app).len()
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

pub(crate) fn focused_input(app: &App) -> Option<crate::text_input::TextInputId> {
    if !app.workspaces.popup.open || app.workspaces.popup.close {
        return None;
    }
    if app.workspaces.popup.sets.name_focused() {
        Some(crate::text_input::TextInputId::WorkspaceName)
    } else if matches!(
        app.workspaces.popup.sets.mode,
        None | Some(crate::ui_workspace_sets::Mode::List)
    ) {
        Some(crate::text_input::TextInputId::WorkspaceFilter)
    } else {
        None
    }
}

pub(crate) fn edit_filter(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
    paste: Option<&str>,
) -> crate::text_input::TextInputResult {
    let changed = app
        .workspaces
        .popup
        .search
        .edit(KeyEvent::new(code, modifiers), paste);
    if changed {
        app.workspaces.popup.selected = 0;
        app.workspaces.popup.scroll = 0;
    }
    crate::text_input::TextInputResult {
        consumed: true,
        changed,
    }
}
pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    if !app.workspaces.popup.open {
        return;
    }
    app.workspaces.popup.close_hits.clear();
    if app.workspaces.popup.sets.mode.is_some() {
        crate::ui_workspace_sets::draw(f, app);
        return;
    }
    app.workspaces.popup.hits.clear();
    if app.workspaces.popup.close
        && !app.workspace_entries().iter().any(|entry| {
            entry.id
                == app
                    .workspaces
                    .popup
                    .close_target
                    .unwrap_or(app.workspaces.active)
                && !entry.path.is_empty()
        })
    {
        cancel_close(app);
    }
    if app.workspaces.popup.close {
        draw_close(f, app);
        return;
    }
    let entries = entries(app);
    let count = entries.len().max(1);
    let actions = actions(app);
    app.workspaces.popup.selected = app.workspaces.popup.selected.min(count + actions.len() - 1);
    let Some(layout) = picker::list(f, app, false, count, actions.len(), 2) else {
        return;
    };
    let selected = app.workspaces.popup.selected;
    let start = picker::scroll(
        &mut app.workspaces.popup.scroll,
        selected,
        count,
        layout.choices.height / layout.row_height,
    );
    for (offset, index) in (start..count)
        .take((layout.choices.height / layout.row_height) as usize)
        .enumerate()
    {
        let row = Rect::new(
            layout.choices.x,
            layout.choices.y + offset as u16 * layout.row_height,
            layout.choices.width,
            1,
        );
        if let Some(entry) = entries.get(index) {
            let close = Rect::new(
                row.right().saturating_sub(4).max(row.x),
                row.y,
                row.width.min(3),
                1,
            );
            let show_close = !entry.path.is_empty() && row.width >= 12;
            let content = Rect {
                width: row.width.saturating_sub(if show_close { 5 } else { 0 }),
                ..row
            };
            let mut badges = Vec::new();
            if entry.active {
                badges.push("Current".to_owned());
            }
            if entry.attention {
                badges.push("! Attention".into());
            } else if entry.unread > 0 {
                badges.push(format!("● {} new", entry.unread));
            } else if entry.running {
                badges.push("● Running".into());
            }
            let name = format!(
                "{} {}",
                if selected == index { "›" } else { "▰" },
                entry.name
            );
            picker::choice(f, app, row, "", "", selected == index, true);
            picker::choice(
                f,
                app,
                content,
                &name,
                &badges.join(" · "),
                selected == index,
                true,
            );
            if show_close {
                let hovered = close.contains((app.mouse_col, app.mouse_row).into());
                let style =
                    app.colors
                        .ui
                        .button_style(picker::base(app).bg.unwrap(), false, hovered, true);
                f.render_widget(Paragraph::new(" × ").style(style), close);
                app.workspaces.popup.close_hits.push((close, entry.id));
            }
            if layout.row_height > 1 {
                let path = crate::ui_performance::clip_middle(
                    if entry.path.is_empty() {
                        "Open files and tools"
                    } else {
                        &entry.path
                    },
                    row.width.saturating_sub(5) as usize,
                );
                picker::subtitle(
                    f,
                    app,
                    Rect::new(row.x, row.y + 1, row.width, 1),
                    &path,
                    selected == index,
                );
            }
            app.workspaces.popup.hits.push((
                Rect {
                    height: layout.row_height,
                    ..row
                },
                index,
            ));
        } else {
            picker::choice(
                f,
                app,
                row,
                if app.workspaces.popup.search.text.is_empty() {
                    "No folder open"
                } else {
                    "No matching folders"
                },
                "",
                false,
                false,
            );
        }
    }
    let hits = picker::action_buttons(f, app, layout.actions, actions, selected, count);
    app.workspaces.popup.hits.extend(hits);
    if let Some(entry) = entries.get(selected) {
        let stats = format!(
            "{} tab{}  ·  {} unsaved  ·  {} terminal{}",
            entry.tabs,
            if entry.tabs == 1 { "" } else { "s" },
            entry.unsaved,
            entry.terminals,
            if entry.terminals == 1 { "" } else { "s" }
        );
        picker::detail(
            f,
            app,
            Rect {
                height: layout.detail.height.min(1),
                ..layout.detail
            },
            &stats,
        );
        if layout.detail.height > 1 {
            let note = if layout.row_height == 1 && !entry.path.is_empty() {
                &entry.path
            } else if !entry.last.is_empty() {
                &entry.last
            } else if entry.running {
                "Commands or agent chats are running in this folder."
            } else if entry.active {
                "This folder is currently shown in the editor."
            } else {
                "Switching folders keeps their files, terminals and chats open."
            };
            picker::detail(
                f,
                app,
                Rect::new(layout.detail.x, layout.detail.y + 1, layout.detail.width, 1),
                note,
            );
        }
    }
}
fn draw_close(f: &mut Frame, app: &mut App) {
    let target = app
        .workspaces
        .popup
        .close_target
        .unwrap_or(app.workspaces.active);
    let Some(entry) = app
        .workspace_entries()
        .into_iter()
        .find(|entry| entry.id == target)
    else {
        cancel_close(app);
        return;
    };
    let mut lines = vec![
        format!("Close “{}”?", entry.name),
        entry.path.clone(),
        String::new(),
        "Its open files will close. Files on disk will stay untouched.".into(),
    ];
    if entry.terminals > 0 || entry.running {
        lines.push("Its terminals and agents will stop.".into());
    }
    if entry.unsaved > 0 {
        lines.push("Unsaved edits will be kept for next time.".into());
    }
    if app.open_folder_count() == 1 {
        lines.push("VTCode will stay open without a folder.".into());
    }
    let Some(inner) = picker::surface(f, app, "Close folder", lines.len() + 5) else {
        return;
    };
    if inner.is_empty() {
        return;
    }
    f.render_widget(
        Paragraph::new(lines.join("\n")).style(picker::base(app)),
        Rect::new(
            inner.x + 1,
            inner.y,
            inner.width.saturating_sub(2),
            inner.height.saturating_sub(2),
        ),
    );
    let hits = picker::buttons(
        f,
        app,
        Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        "Close folder",
        app.workspaces.popup.selected == 0,
        app.workspaces.popup.selected == 1,
        true,
    );
    app.workspaces.popup.hits.extend(
        hits.into_iter()
            .enumerate()
            .map(|(index, rect)| (rect, index)),
    );
}
