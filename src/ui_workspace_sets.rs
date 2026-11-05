//! Saved-set browsing, directory previews and explicit set actions.
use crate::{
    app::App,
    app_workspaces::Request,
    text_input::{TextInput, TextInputHistory, TextInputHitMap},
    ui_workspace_picker as picker,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Debug)]
pub(crate) enum Mode {
    List,
    Manage(u64),
    Save(Option<u64>),
    Rename(u64),
    Delete(u64),
    Confirm,
}
#[derive(Clone)]
enum Action {
    None,
    Open(u64),
    Manage(u64),
    New,
    Update(u64),
    Rename(u64),
    Delete(u64),
    ConfirmDelete(u64),
    Submit,
    Confirm,
    Cancel,
    Refresh,
    CancelOpening,
}
struct Row {
    text: String,
    badge: String,
    action: Action,
}
impl Row {
    fn new(text: impl Into<String>, action: Action) -> Self {
        Self {
            text: text.into(),
            badge: String::new(),
            action,
        }
    }
}
#[derive(Default)]
pub(crate) struct Popup {
    pub mode: Option<Mode>,
    pub text: String,
    cursor: usize,
    anchor: Option<usize>,
    history: TextInputHistory,
    input: Option<TextInputHitMap>,
    dragging: bool,
    field_focus: bool,
    selected_set: Option<u64>,
    back_to_set: Option<u64>,
    back_selection: usize,
    back_to_workspaces: bool,
    contents: Rect,
    contents_scroll: usize,
    contents_len: usize,
    hits: Vec<(Rect, Action, usize)>,
}
impl Popup {
    pub(crate) fn input_state(&mut self) -> (&mut String, &mut usize, &mut Option<usize>) {
        (&mut self.text, &mut self.cursor, &mut self.anchor)
    }
    pub(crate) fn focus_input(&mut self) {
        self.field_focus = true;
    }
    pub(crate) fn name_focused(&self) -> bool {
        self.field_focus && matches!(self.mode, Some(Mode::Save(_) | Mode::Rename(_)))
    }
}

pub(crate) fn edit_input(
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
    paste: Option<&str>,
) -> crate::text_input::TextInputResult {
    if app.workspace_sets_saving() {
        return crate::text_input::TextInputResult {
            consumed: true,
            changed: false,
        };
    }
    let popup = &mut app.workspaces.popup.sets;
    let result = popup.history.edit(
        TextInput::new(&mut popup.text, &mut popup.cursor, &mut popup.anchor),
        code,
        modifiers,
        paste,
    );
    if result.changed {
        app.workspaces.sets.error = None;
    }
    result
}
pub(crate) fn open(app: &mut App) {
    crate::ui_workspaces::open(app);
    if !app.workspaces.popup.open {
        return;
    }
    show_list(app);
    app.load_workspace_sets(false);
}
pub(crate) fn show_list(app: &mut App) {
    if crate::ui_modal::active(app).is_some() || app.change_dir_active {
        return;
    }
    if !app.workspaces.popup.open {
        crate::ui_workspaces::open(app);
    }
    if !app.workspaces.popup.open {
        return;
    }
    let previous = &app.workspaces.popup.sets;
    let sets = app.saved_workspace_sets();
    let preferred = if matches!(previous.mode, Some(Mode::Save(None))) {
        sets.iter()
            .find(|set| previous.text.trim() == set.name)
            .map(|set| set.id)
    } else {
        previous.selected_set
    };
    if sets
        .iter()
        .find(|set| Some(set.id) == preferred)
        .is_some_and(|set| {
            !crate::app::contains_ignore_case(&set.name, app.workspaces.popup.search.text.trim())
        })
    {
        app.workspaces.popup.search = Default::default();
    }
    let query = app.workspaces.popup.search.text.trim();
    let selected = sets
        .iter()
        .filter(|set| query.is_empty() || crate::app::contains_ignore_case(&set.name, query))
        .position(|set| Some(set.id) == preferred)
        .unwrap_or(0);
    app.workspaces.popup.close = false;
    app.workspaces.popup.selected = selected;
    app.workspaces.popup.scroll = 0;
    app.workspaces.popup.sets = Popup {
        mode: Some(Mode::List),
        selected_set: preferred,
        ..Default::default()
    };
}
pub(crate) fn save(app: &mut App) {
    if app.open_folder_count() == 0 {
        app.notify_warn("Open a folder before saving a workspace");
        return;
    }
    let back_to_workspaces = app.workspaces.popup.open && app.workspaces.popup.sets.mode.is_none();
    open(app);
    if app.workspaces.popup.open {
        let name = app
            .workspace_entries()
            .iter()
            .filter(|entry| !entry.path.is_empty())
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>()
            .join(" + ");
        form(app, Mode::Save(None), name.chars().take(80).collect());
        app.workspaces.popup.sets.back_to_workspaces = back_to_workspaces;
    }
}
fn form(app: &mut App, mode: Mode, text: String) {
    let previous = &app.workspaces.popup.sets;
    let back_to_set = match previous.mode {
        Some(Mode::Manage(id)) => Some(id),
        _ => previous.back_to_set,
    };
    let contents_scroll = previous.contents_scroll;
    let back_selection = if matches!(previous.mode, Some(Mode::Manage(_))) {
        app.workspaces.popup.selected
    } else {
        previous.back_selection
    };
    let selected_set = match mode {
        Mode::Manage(id) | Mode::Rename(id) | Mode::Delete(id) | Mode::Save(Some(id)) => Some(id),
        _ => app.workspaces.popup.sets.selected_set,
    };
    let back_to_set = if matches!(mode, Mode::Manage(_)) {
        None
    } else {
        back_to_set
    };
    app.workspaces.sets.error = None;
    app.workspaces.popup.selected = 0;
    app.workspaces.popup.scroll = 0;
    app.workspaces.popup.sets = Popup {
        mode: Some(mode),
        cursor: text.len(),
        anchor: (!text.is_empty()).then_some(0),
        text,
        field_focus: true,
        selected_set,
        back_to_set,
        back_selection,
        contents_scroll,
        ..Default::default()
    };
}
pub(crate) fn show_confirmation(app: &mut App) {
    crate::ui_workspaces::open(app);
    if !app.workspaces.popup.open {
        return;
    }
    app.workspaces.popup.sets.mode = Some(Mode::Confirm);
    app.workspaces.popup.selected = rows(app).len().saturating_sub(1);
}
fn is_form(app: &App) -> bool {
    matches!(
        app.workspaces.popup.sets.mode,
        Some(Mode::Save(_) | Mode::Rename(_))
    )
}
fn root_paths(set: &crate::workspace_sets::SavedSet) -> Vec<String> {
    let base = crate::portable::base_dir();
    set.roots
        .iter()
        .map(|root| match root {
            crate::workspace_sets::Location::Local { path, .. } => {
                root.local_path(&base).map_or_else(
                    |_| match path {
                        crate::workspace_sets::NativePath::Utf8(text) => text.clone(),
                        crate::workspace_sets::NativePath::Unix(bytes) => {
                            String::from_utf8_lossy(bytes).into_owned()
                        }
                        crate::workspace_sets::NativePath::Windows(units) => {
                            String::from_utf16_lossy(units)
                        }
                    },
                    |path| path.display().to_string(),
                )
            }
            crate::workspace_sets::Location::Remote { uri } => uri.clone(),
        })
        .collect::<Vec<_>>()
}
fn rows(app: &App) -> Vec<Row> {
    let sets = app.saved_workspace_sets();
    match &app.workspaces.popup.sets.mode {
        Some(Mode::List) => {
            let query = app.workspaces.popup.search.text.trim();
            let mut rows: Vec<_> = sets
                .iter()
                .filter(|set| {
                    query.is_empty() || crate::app::contains_ignore_case(&set.name, query)
                })
                .map(|set| Row {
                    text: set.name.clone(),
                    badge: format!(
                        "{} folder{}",
                        set.roots.len(),
                        if set.roots.len() == 1 { "" } else { "s" }
                    ),
                    action: Action::Manage(set.id),
                })
                .collect();
            if rows.is_empty() {
                rows.push(Row::new(
                    if app.workspace_sets_busy() {
                        "Loading workspaces…"
                    } else if !query.is_empty() {
                        "No matching saved workspaces"
                    } else {
                        "No saved workspaces yet"
                    },
                    Action::None,
                ));
            }
            if app.workspaces.sets.error.is_some() {
                rows.push(Row::new("Retry loading workspaces", Action::Refresh));
            }
            if app.open_folder_count() > 0 {
                rows.push(Row::new("+ Save workspace…", Action::New));
            }
            if app.workspace_set_switching() {
                rows.push(Row::new("Cancel opening workspace", Action::CancelOpening));
            }
            rows
        }
        Some(Mode::Manage(id)) => {
            let mut rows = Vec::new();
            if sets.iter().any(|set| set.id == *id) {
                rows.extend([
                    Row::new("Open workspace", Action::Open(*id)),
                    Row::new("Update from open folders…", Action::Update(*id)),
                    Row::new("Rename workspace…", Action::Rename(*id)),
                    Row::new("Delete workspace…", Action::Delete(*id)),
                ]);
            }
            rows.push(Row::new("‹ Saved workspaces", Action::Cancel));
            rows
        }
        Some(Mode::Save(_) | Mode::Rename(_)) => vec![
            Row::new("Save", Action::Submit),
            Row::new("Cancel", Action::Cancel),
        ],
        Some(Mode::Delete(id)) => vec![
            Row::new(
                format!(
                    "Delete “{}”?",
                    sets.iter()
                        .find(|set| set.id == *id)
                        .map_or("workspace", |set| &set.name)
                ),
                Action::None,
            ),
            Row::new("Your open folders stay open.", Action::None),
            Row::new("Delete workspace", Action::ConfirmDelete(*id)),
            Row::new("Cancel", Action::Cancel),
        ],
        Some(Mode::Confirm) => {
            let Some((name, count, close)) = app.workspace_set_confirmation() else {
                return vec![Row::new("Preparing workspace…", Action::None)];
            };
            if app.workspace_folder_replacement() {
                let mut rows = vec![
                    Row::new(format!("Open “{name}”?"), Action::None),
                    Row::new(
                        "The folders below and their open files will close.",
                        Action::None,
                    ),
                    Row::new("Their terminals and agents will stop.", Action::None),
                    Row::new("Unsaved edits will be kept for next time.", Action::None),
                ];
                rows.extend(close.into_iter().map(|entry| {
                    Row::new(
                        if entry.path.is_empty() {
                            entry.name
                        } else {
                            entry.path
                        },
                        Action::None,
                    )
                }));
                rows.push(Row::new("Open folder", Action::Confirm));
                rows.push(Row::new("Cancel", Action::Cancel));
                return rows;
            }
            let mut rows = vec![
                Row::new(
                    format!(
                        "Open “{name}” · {count} folder{}",
                        if count == 1 { "" } else { "s" }
                    ),
                    Action::None,
                ),
                Row::new(
                    format!(
                        "{} open folder{} will close:",
                        close.len(),
                        if close.len() == 1 { "" } else { "s" }
                    ),
                    Action::None,
                ),
            ];
            rows.push(Row::new(
                "Edits are kept. Jobs in closed folders stop.",
                Action::None,
            ));
            rows.extend(
                close
                    .into_iter()
                    .map(|entry| Row::new(entry.path, Action::None)),
            );
            rows.push(Row::new("Open workspace", Action::Confirm));
            rows.push(Row::new("Cancel", Action::Cancel));
            rows
        }
        None => vec![],
    }
}
fn activate(app: &mut App, action: Action) {
    if app.workspace_sets_busy()
        && !matches!(
            action,
            Action::Cancel | Action::CancelOpening | Action::Manage(_) | Action::None
        )
    {
        return;
    }
    let set = |id| {
        app.saved_workspace_sets()
            .into_iter()
            .find(|set| set.id == id)
    };
    match action {
        Action::None => {}
        Action::Open(id) => {
            crate::ui_workspaces::dismiss(app);
            app.workspaces.request = Some(Request::OpenSet(id));
        }
        Action::Manage(id) => {
            let same_set = app.workspaces.popup.sets.selected_set == Some(id);
            form(app, Mode::Manage(id), String::new());
            if !same_set {
                app.workspaces.popup.sets.contents_scroll = 0;
            }
        }
        Action::New => {
            let name = app
                .workspace_entries()
                .iter()
                .filter(|entry| !entry.path.is_empty())
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>()
                .join(" + ");
            form(app, Mode::Save(None), name.chars().take(80).collect());
        }
        Action::Update(id) => {
            if let Some(set) = set(id) {
                form(app, Mode::Save(Some(id)), set.name);
            }
        }
        Action::Rename(id) => {
            if let Some(set) = set(id) {
                form(app, Mode::Rename(id), set.name);
            }
        }
        Action::Delete(id) => {
            form(app, Mode::Delete(id), String::new());
            app.workspaces.popup.selected = rows(app).len().saturating_sub(1);
        }
        Action::ConfirmDelete(id) => app.delete_workspace_set(id),
        Action::Submit => {
            let text = app.workspaces.popup.sets.text.clone();
            if text.trim().is_empty() {
                app.workspaces.sets.error = Some("Enter a workspace name".into());
                return;
            }
            match app.workspaces.popup.sets.mode {
                Some(Mode::Save(replace)) => app.save_workspace_set(text, replace),
                Some(Mode::Rename(id)) => app.rename_workspace_set(id, text),
                _ => {}
            }
        }
        Action::Confirm => app.confirm_workspace_set(),
        Action::Cancel => {
            if matches!(app.workspaces.popup.sets.mode, Some(Mode::Confirm)) {
                app.cancel_workspace_set_switch();
            }
            if matches!(app.workspaces.popup.sets.mode, Some(Mode::List)) {
                crate::ui_workspaces::dismiss(app);
            } else if let Some(id) = app.workspaces.popup.sets.back_to_set {
                let selected = app.workspaces.popup.sets.back_selection;
                form(app, Mode::Manage(id), String::new());
                app.workspaces.popup.selected = selected.min(rows(app).len().saturating_sub(1));
            } else if app.workspaces.popup.sets.back_to_workspaces {
                crate::ui_workspaces::open(app);
                app.workspaces.popup.selected = app.workspace_entries().len().max(1) + 1;
            } else {
                show_list(app);
            }
        }
        Action::Refresh => app.load_workspace_sets(true),
        Action::CancelOpening => {
            app.cancel_workspace_set_switch();
            show_list(app);
        }
    }
}
pub(crate) fn paste(app: &mut App, text: &str) {
    if !is_form(app) {
        if !app.workspaces.popup.close
            && matches!(app.workspaces.popup.sets.mode, None | Some(Mode::List))
        {
            crate::ui_workspaces::paste_filter(app, text);
        }
        return;
    }
    if app.workspace_sets_saving() {
        return;
    }
    if text.chars().take(81).count() > 80 {
        app.workspaces.sets.error = Some("Use a name of up to 80 characters".into());
        return;
    }
    let p = &mut app.workspaces.popup.sets;
    p.history.edit(
        TextInput::new(&mut p.text, &mut p.cursor, &mut p.anchor),
        KeyCode::Null,
        KeyModifiers::NONE,
        Some(text),
    );
    app.workspaces.sets.error = None;
}
pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.workspaces.popup.sets.mode.is_none() {
        return false;
    }
    if key.code == KeyCode::Esc {
        activate(app, Action::Cancel);
        return true;
    }
    let form = is_form(app);
    let list_mode = matches!(app.workspaces.popup.sets.mode, Some(Mode::List));
    let list = rows(app);
    if matches!(app.workspaces.popup.sets.mode, Some(Mode::Manage(_))) {
        match key.code {
            KeyCode::Left => {
                activate(app, Action::Cancel);
                return true;
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                scroll_contents(app, key.code == KeyCode::PageDown, true);
                return true;
            }
            _ => {}
        }
    }
    if let Some(Mode::Manage(id)) = app.workspaces.popup.sets.mode
        && matches!(key.code, KeyCode::F(2) | KeyCode::Delete)
        && matches!(
            list.first(),
            Some(Row {
                action: Action::Open(_),
                ..
            })
        )
    {
        activate(
            app,
            if key.code == KeyCode::F(2) {
                Action::Rename(id)
            } else {
                Action::Delete(id)
            },
        );
        return true;
    }
    if list_mode && key.code == KeyCode::F(5) {
        activate(app, Action::Refresh);
        return true;
    }
    if list_mode
        && (key.code == KeyCode::Menu
            || key.code == KeyCode::F(10) && key.modifiers.contains(KeyModifiers::SHIFT))
    {
        if let Some(Row {
            action: Action::Manage(id),
            ..
        }) = list.get(app.workspaces.popup.selected)
        {
            activate(app, Action::Manage(*id));
        }
        return true;
    }
    if list_mode && key.code == KeyCode::Right && app.workspaces.popup.search.text.is_empty() {
        if let Some(Row {
            action: Action::Manage(id),
            ..
        }) = list.get(app.workspaces.popup.selected)
        {
            activate(app, Action::Manage(*id));
        }
        return true;
    }
    if list_mode && crate::ui_workspaces::filter_key(app, key) {
        return true;
    }
    match key.code {
        KeyCode::Enter => activate(
            app,
            if form && app.workspaces.popup.sets.field_focus {
                Action::Submit
            } else {
                list.get(app.workspaces.popup.selected)
                    .map_or(Action::None, |row| row.action.clone())
            },
        ),
        KeyCode::F(2) | KeyCode::Delete if list_mode => {
            if let Some(Row {
                action: Action::Manage(id),
                ..
            }) = list.get(app.workspaces.popup.selected)
            {
                activate(
                    app,
                    if key.code == KeyCode::F(2) {
                        Action::Rename(*id)
                    } else {
                        Action::Delete(*id)
                    },
                );
            }
        }
        KeyCode::Tab | KeyCode::BackTab if form => {
            let p = &mut app.workspaces.popup;
            if p.sets.field_focus {
                p.sets.field_focus = false;
                p.selected = if key.code == KeyCode::Tab { 0 } else { 1 };
            } else if (p.selected == 1 && key.code == KeyCode::Tab)
                || (p.selected == 0 && key.code == KeyCode::BackTab)
            {
                p.sets.field_focus = true;
            } else {
                p.selected = 1 - p.selected.min(1);
            }
        }
        KeyCode::Up | KeyCode::BackTab if !form || !app.workspaces.popup.sets.field_focus => {
            app.workspaces.popup.selected =
                (app.workspaces.popup.selected + list.len().saturating_sub(1)) % list.len().max(1)
        }
        KeyCode::Down | KeyCode::Tab if !form || !app.workspaces.popup.sets.field_focus => {
            app.workspaces.popup.selected = (app.workspaces.popup.selected + 1) % list.len().max(1)
        }
        KeyCode::Home if !form => app.workspaces.popup.selected = 0,
        KeyCode::End if !form => app.workspaces.popup.selected = list.len().saturating_sub(1),
        _ if form && app.workspaces.popup.sets.field_focus && !app.workspace_sets_saving() => {
            let p = &mut app.workspaces.popup.sets;
            if p.history
                .edit(
                    TextInput::new(&mut p.text, &mut p.cursor, &mut p.anchor),
                    key.code,
                    key.modifiers,
                    None,
                )
                .changed
            {
                app.workspaces.sets.error = None;
            }
        }
        _ => {}
    }
    if let Some(Row {
        action: Action::Manage(id),
        ..
    }) = rows(app).get(app.workspaces.popup.selected)
    {
        app.workspaces.popup.sets.selected_set = Some(*id);
    }
    true
}
pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if app.workspaces.popup.sets.mode.is_none() {
        return false;
    }
    let point = (mouse.column, mouse.row).into();
    if app.workspaces.popup.sets.contents.contains(point)
        && matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        )
    {
        scroll_contents(app, mouse.kind == MouseEventKind::ScrollDown, false);
        return true;
    }
    let hit = app
        .workspaces
        .popup
        .sets
        .hits
        .iter()
        .find(|(rect, _, _)| rect.contains(point))
        .cloned();
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(byte) = app
                .workspaces
                .popup
                .sets
                .input
                .as_ref()
                .filter(|map| map.contains(mouse.column, mouse.row))
                .map(|map| map.byte_at(mouse.column, mouse.row))
            {
                let p = &mut app.workspaces.popup.sets;
                if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                    p.anchor.get_or_insert(p.cursor);
                } else {
                    p.anchor = None;
                }
                p.cursor = byte;
                p.dragging = true;
                p.field_focus = true;
            } else if let Some((_, action, index)) = hit {
                app.workspaces.popup.selected = index;
                activate(app, action);
            } else if !app.workspaces.popup.area.contains(point) {
                crate::ui_workspaces::dismiss(app);
            }
        }
        MouseEventKind::Down(MouseButton::Right)
            if matches!(app.workspaces.popup.sets.mode, Some(Mode::List)) =>
        {
            if let Some((_, Action::Open(id) | Action::Manage(id), _)) = hit {
                activate(app, Action::Manage(id));
            }
        }
        MouseEventKind::Drag(MouseButton::Left) if app.workspaces.popup.sets.dragging => {
            let p = &mut app.workspaces.popup.sets;
            if let Some(map) = &p.input {
                p.anchor.get_or_insert(p.cursor);
                p.cursor = map.byte_at(mouse.column, mouse.row);
            }
        }
        MouseEventKind::Up(MouseButton::Left) => app.workspaces.popup.sets.dragging = false,
        MouseEventKind::ScrollUp => {
            app.workspaces.popup.selected = app.workspaces.popup.selected.saturating_sub(1)
        }
        MouseEventKind::ScrollDown => {
            app.workspaces.popup.selected =
                (app.workspaces.popup.selected + 1).min(rows(app).len().saturating_sub(1))
        }
        MouseEventKind::Moved => {
            if let Some((_, action, index)) = hit {
                app.workspaces.popup.selected = index;
                if let Action::Open(id) | Action::Manage(id) = action {
                    app.workspaces.popup.sets.selected_set = Some(id);
                }
            }
        }
        _ => {}
    }
    true
}
fn scroll_contents(app: &mut App, down: bool, page: bool) {
    let popup = &mut app.workspaces.popup.sets;
    let step = if page {
        popup.contents.height.max(1) as usize
    } else {
        1
    };
    let end = popup
        .contents_len
        .saturating_sub(popup.contents.height as usize);
    popup.contents_scroll = if down {
        popup.contents_scroll.saturating_add(step).min(end)
    } else {
        popup.contents_scroll.saturating_sub(step)
    };
}
pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    let Some(mode) = app.workspaces.popup.sets.mode.clone() else {
        return;
    };
    app.workspaces.popup.sets.hits.clear();
    app.workspaces.popup.sets.input = None;
    app.workspaces.popup.sets.contents = Rect::default();
    let rows = rows(app);
    app.workspaces.popup.selected = app
        .workspaces
        .popup
        .selected
        .min(rows.len().saturating_sub(1));
    if matches!(mode, Mode::List) {
        draw_list(f, app, &rows);
        return;
    }
    if let Mode::Manage(id) = mode {
        draw_contents(f, app, id, &rows);
        return;
    }
    draw_dialog(f, app, mode, &rows);
}
fn draw_contents(f: &mut Frame, app: &mut App, id: u64, rows: &[Row]) {
    let set = app
        .saved_workspace_sets()
        .into_iter()
        .find(|set| set.id == id);
    let paths = set.as_ref().map(root_paths).unwrap_or_default();
    let name = set
        .as_ref()
        .map_or("Saved workspace", |set| set.name.as_str());
    let Some(inner) = picker::surface(
        f,
        app,
        "Workspace details",
        (paths.len().clamp(1, 8) + 12).max(16),
    ) else {
        return;
    };
    let selected = app.workspaces.popup.selected;
    let back_index = rows.len().saturating_sub(1);
    let room = inner.height;
    let header = if room >= 9 { 4 } else { 0 };
    let footer = if room >= 5 { 3 } else { room.min(2) };
    if header > 0 {
        let active = app.current_workspace_set().filter(|(set, _)| set.id == id);
        let badge = match active {
            Some((_, true)) => "Active · Unsaved changes",
            Some(_) => "Active workspace",
            None => "Saved workspace",
        };
        picker::choice(
            f,
            app,
            Rect::new(inner.x, inner.y, inner.width, 1),
            name,
            badge,
            false,
            true,
        );
        picker::detail(
            f,
            app,
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
            &format!(
                "{} folder{} · Open this collection together",
                paths.len(),
                if paths.len() == 1 { "" } else { "s" }
            ),
        );
        picker::detail(
            f,
            app,
            Rect::new(inner.x, inner.y + 3, inner.width, 1),
            "FOLDERS IN THIS WORKSPACE",
        );
    }
    let contents = Rect::new(
        inner.x,
        inner.y + header,
        inner.width,
        room.saturating_sub(header + footer),
    );
    let popup = &mut app.workspaces.popup.sets;
    popup.contents = contents;
    popup.contents_len = paths.len();
    popup.contents_scroll = popup
        .contents_scroll
        .min(paths.len().saturating_sub(contents.height as usize));
    let start = popup.contents_scroll;
    for (offset, (index, path)) in paths
        .iter()
        .enumerate()
        .skip(start)
        .take(contents.height as usize)
        .enumerate()
    {
        let first = set.as_ref().is_some_and(|set| set.active == index);
        let badge = if first && contents.width >= 30 {
            "Start folder"
        } else {
            ""
        };
        let path = crate::ui_performance::clip_middle(
            &crate::ui_workspaces::escaped(path),
            contents.width.saturating_sub(badge.width() as u16 + 6) as usize,
        );
        picker::choice(
            f,
            app,
            Rect::new(contents.x, contents.y + offset as u16, contents.width, 1),
            &format!("{} {path}", if first { "▰" } else { "▱" }),
            badge,
            false,
            true,
        );
    }
    if set.is_none() {
        picker::detail(f, app, contents, "Workspace is no longer available");
    }
    if footer == 0 {
        return;
    }
    let action_y = inner.bottom() - footer;
    if footer == 1 && selected == back_index {
        for (rect, index) in picker::action_buttons(
            f,
            app,
            Rect::new(inner.x, action_y, inner.width, 1),
            &["‹ Saved workspaces"],
            selected,
            back_index,
        ) {
            app.workspaces
                .popup
                .sets
                .hits
                .push((rect, Action::Cancel, index));
        }
        return;
    }
    let action_count = rows.len().saturating_sub(1);
    let labels: Vec<_> = rows
        .iter()
        .take(action_count)
        .map(|row| row.text.as_str())
        .collect();
    for (rect, index) in picker::action_buttons(
        f,
        app,
        Rect::new(inner.x, action_y, inner.width, 1),
        &labels,
        selected,
        0,
    ) {
        app.workspaces
            .popup
            .sets
            .hits
            .push((rect, rows[index].action.clone(), index));
    }
    if footer > 1 {
        let row = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
        let hits =
            picker::action_buttons(f, app, row, &["‹ Saved workspaces"], selected, back_index);
        app.workspaces.popup.sets.hits.extend(
            hits.into_iter()
                .map(|(rect, index)| (rect, Action::Cancel, index)),
        );
        if row.width >= 62 {
            picker::detail(
                f,
                app,
                Rect::new(row.x + 25, row.y, row.width - 25, 1),
                "F2 Rename · Delete Remove · PgUp/PgDn Folders",
            );
        }
    }
}
fn draw_list(f: &mut Frame, app: &mut App, rows: &[Row]) {
    let actions =
        usize::from(app.open_folder_count() > 0) + usize::from(app.workspace_set_switching());
    let count = rows.len().saturating_sub(actions);
    let Some(layout) = picker::list(f, app, true, count, actions, 2) else {
        return;
    };
    let selected = app.workspaces.popup.selected;
    let active = app.current_workspace_set();
    let start = picker::scroll(
        &mut app.workspaces.popup.scroll,
        selected,
        count,
        layout.choices.height / layout.row_height,
    );
    let sets = app.saved_workspace_sets();
    for (offset, (index, row)) in rows
        .iter()
        .enumerate()
        .take(count)
        .skip(start)
        .take((layout.choices.height / layout.row_height) as usize)
        .enumerate()
    {
        let rect = Rect::new(
            layout.choices.x,
            layout.choices.y + offset as u16 * layout.row_height,
            layout.choices.width,
            1,
        );
        let set = match row.action {
            Action::Manage(id) => sets.iter().find(|set| set.id == id),
            _ => None,
        };
        let current =
            set.and_then(|set| active.as_ref().filter(|(current, _)| current.id == set.id));
        let badge = match current {
            Some((_, true)) => format!("Active · Unsaved  {}  ›", row.badge),
            Some(_) => format!("Active  {}  ›", row.badge),
            None if set.is_some() => format!("{}  ›", row.badge),
            None => row.badge.clone(),
        };
        picker::choice(
            f,
            app,
            rect,
            &format!("{} {}", if selected == index { "›" } else { "▦" }, row.text),
            &badge,
            selected == index,
            !matches!(row.action, Action::None),
        );
        if layout.row_height > 1 {
            let description = set
                .map(|set| {
                    root_paths(set)
                        .iter()
                        .map(|path| {
                            path.rsplit(['/', '\\'])
                                .find(|part| !part.is_empty())
                                .unwrap_or(path)
                                .to_owned()
                        })
                        .collect::<Vec<_>>()
                        .join("  ·  ")
                })
                .unwrap_or_else(|| "Save a workspace to keep a named folder collection.".into());
            picker::subtitle(
                f,
                app,
                Rect::new(rect.x, rect.y + 1, rect.width, 1),
                &crate::ui_workspaces::clipped(&description, rect.width.saturating_sub(5) as usize),
                selected == index,
            );
        }
        app.workspaces.popup.sets.hits.push((
            Rect {
                height: layout.row_height,
                ..rect
            },
            row.action.clone(),
            index,
        ));
    }
    let labels: Vec<_> = rows
        .iter()
        .skip(count)
        .map(|row| row.text.as_str())
        .collect();
    for (rect, index) in picker::action_buttons(f, app, layout.actions, &labels, selected, count) {
        app.workspaces
            .popup
            .sets
            .hits
            .push((rect, rows[index].action.clone(), index));
    }
    let detail = app.workspaces.sets.error.clone().unwrap_or_else(|| {
        rows.get(selected)
            .and_then(|row| match row.action {
                Action::Manage(id) => sets.iter().find(|set| set.id == id),
                _ => None,
            })
            .map(|set| {
                format!(
                    "{} folders · Start folder: {}",
                    set.roots.len(),
                    root_paths(set).get(set.active).cloned().unwrap_or_default()
                )
            })
            .unwrap_or_default()
    });
    picker::detail(
        f,
        app,
        Rect {
            height: layout.detail.height.min(1),
            ..layout.detail
        },
        &detail,
    );
}
fn draw_dialog(f: &mut Frame, app: &mut App, mode: Mode, rows: &[Row]) {
    let form = is_form(app);
    let title = match mode {
        Mode::Save(Some(_)) => "Update workspace",
        Mode::Save(None) => "Save workspace",
        Mode::Rename(_) => "Rename workspace",
        Mode::Delete(_) => "Delete workspace",
        Mode::Confirm if app.workspace_folder_replacement() => "Open folder",
        _ => "Open workspace",
    };
    let error = app.workspaces.sets.error.clone();
    let Some(inner) = picker::surface(
        f,
        app,
        title,
        if form {
            (if matches!(mode, Mode::Rename(_)) {
                6
            } else {
                7
            }) + usize::from(error.is_some())
        } else {
            rows.len() + 2
        },
    ) else {
        return;
    };
    let mut y = inner.y;
    let text_width = inner.width.saturating_sub(2);
    let text_x = inner.x + 1;
    if form {
        if inner.height >= 4 {
            picker::detail(
                f,
                app,
                Rect::new(inner.x, y, inner.width, 1),
                "Workspace name",
            );
            y += 1;
        }
        if inner.height > 1 || app.workspaces.popup.sets.field_focus {
            let viewport = app.text_input_viewport(crate::text_input::TextInputId::WorkspaceName);
            let p = &mut app.workspaces.popup.sets;
            p.input = Some(
                crate::ui_widgets::InputField::new(&p.text)
                    .viewport(viewport)
                    .prefix("", app.colors.fg_input)
                    .cursor_pos(p.cursor)
                    .selection_range(App::inline_selection_range(p.cursor, p.anchor))
                    .cursor_marker(if p.field_focus { "│" } else { "" })
                    .placeholder_text("Workspace name")
                    .theme_colors(&app.colors)
                    .render(f, Rect::new(text_x, y, text_width, 1)),
            );
            let hit = p.input.clone().unwrap();
            app.register_text_input(crate::text_input::TextInputId::WorkspaceName, hit);
            y += 1;
        }
        if inner.height >= 5 && !matches!(mode, Mode::Rename(_)) {
            let entries: Vec<_> = app
                .workspace_entries()
                .into_iter()
                .filter(|entry| !entry.path.is_empty())
                .collect();
            picker::detail(
                f,
                app,
                Rect::new(inner.x, y, inner.width, 1),
                &format!(
                    "{} folder{} · {}",
                    entries.len(),
                    if entries.len() == 1 { "" } else { "s" },
                    entries
                        .iter()
                        .map(|entry| entry.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
            y += 1;
        }
    } else {
        let count = rows.len().saturating_sub(2);
        let height = inner.height.saturating_sub(1);
        let start = picker::scroll(
            &mut app.workspaces.popup.scroll,
            app.workspaces.popup.selected.min(count),
            count,
            height,
        );
        for row in rows.iter().take(count).skip(start).take(height as usize) {
            picker::detail(f, app, Rect::new(inner.x, y, inner.width, 1), &row.text);
            y += 1;
        }
    }
    if let Some(error) = error
        && y < inner.bottom().saturating_sub(1)
    {
        picker::detail(f, app, Rect::new(inner.x, y, inner.width, 1), &error);
    }
    if inner.height > 1 || !form || !app.workspaces.popup.sets.field_focus {
        let primary_index = rows.len().saturating_sub(2);
        let cancel_index = rows.len().saturating_sub(1);
        let saving = app.workspace_sets_busy();
        let primary = if saving {
            if app.workspace_sets_saving() {
                "Saving…"
            } else {
                "Loading…"
            }
        } else if let Mode::Save(Some(_)) = mode {
            "Update workspace"
        } else if form {
            if matches!(mode, Mode::Rename(_)) {
                "Rename"
            } else {
                "Save workspace"
            }
        } else {
            rows.get(primary_index)
                .map_or("Continue", |row| row.text.as_str())
        };
        let selected = app.workspaces.popup.selected;
        let focused = !(form && app.workspaces.popup.sets.field_focus);
        let hits = picker::buttons(
            f,
            app,
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
            primary,
            focused && selected == primary_index,
            focused && selected == cancel_index,
            !saving,
        );
        for (rect, index) in hits.into_iter().zip([primary_index, cancel_index]) {
            if let Some(row) = rows.get(index) {
                app.workspaces
                    .popup
                    .sets
                    .hits
                    .push((rect, row.action.clone(), index));
            }
        }
    }
}
