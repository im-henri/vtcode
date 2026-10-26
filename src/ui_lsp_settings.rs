//! Language-server setup using the workbench's modal input controls.
use crate::{
    app::{ActivePane, App},
    lsp::LspServerConfig,
    settings::{Settings, SettingsScope},
    text_input::TextInputId,
    ui_widgets::InputField,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Margin, Rect},
    style::{Modifier, Style},
    widgets::{Block, Borders, Clear, Paragraph},
};
use std::io::Write;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Action {
    #[default]
    Add,
    Edit(usize),
    Remove(usize),
    ConfirmRemove(usize),
    Keep,
    Field(usize),
    AddArgument,
    RemoveArgument(usize),
    Save,
    Cancel,
    Close,
    Restart,
}

pub(crate) struct Field {
    pub text: String,
    pub cursor: usize,
    pub anchor: Option<usize>,
}
impl Field {
    fn new(text: String) -> Self {
        Self {
            cursor: text.len(),
            text,
            anchor: None,
        }
    }
}

pub(crate) struct Draft {
    index: Option<usize>,
    pub fields: Vec<Field>,
    arguments: Vec<usize>,
}
impl Draft {
    fn new(index: Option<usize>, config: LspServerConfig) -> Self {
        let mut draft = Self {
            index,
            fields: vec![
                Field::new(config.language),
                Field::new(config.command),
                Field::new(config.file_patterns.join(" ")),
                Field::new(
                    config
                        .init_options
                        .map(|value| value.to_string())
                        .unwrap_or_default(),
                ),
                Field::new(config.settings.map(|value| value.to_string()).unwrap_or_default()),
            ],
            arguments: Vec::new(),
        };
        for arg in config.args {
            let id = draft.push(arg);
            draft.arguments.push(id);
        }
        draft
    }
    fn push(&mut self, text: String) -> usize {
        let id = self.fields.len();
        self.fields.push(Field::new(text));
        id
    }
    fn config(&self, servers: &[LspServerConfig]) -> Result<LspServerConfig, String> {
        let language = self.fields[0].text.trim();
        if language.is_empty() || language.chars().any(char::is_whitespace) {
            return Err("Enter a language ID without spaces, such as rust or python.".into());
        }
        if servers
            .iter()
            .enumerate()
            .any(|(i, server)| Some(i) != self.index && server.language == language)
        {
            return Err("This language already has a server. Edit its existing entry.".into());
        }
        let command = self.fields[1].text.trim();
        if command.is_empty() {
            return Err("Enter the server executable or interpreter path.".into());
        }
        let patterns: Vec<String> = self.fields[2]
            .text
            .split_whitespace()
            .map(str::to_string)
            .collect();
        if patterns.is_empty()
            || patterns.iter().any(|p| {
                let suffix = p.strip_prefix("*.").unwrap_or(p);
                suffix.is_empty() || suffix.contains(['*', '?', '/', '\\'])
            })
        {
            return Err(
                "Enter file extensions such as *.rs, or exact filenames, separated by spaces."
                    .into(),
            );
        }
        let args: Vec<_> = self
            .arguments
            .iter()
            .map(|&i| self.fields[i].text.clone())
            .collect();
        if [language, command]
            .into_iter()
            .chain(patterns.iter().map(String::as_str))
            .chain(args.iter().map(String::as_str))
            .any(|value| value.contains('\0'))
        {
            return Err("Language, executable, patterns and arguments cannot contain NUL.".into());
        }
        let options = self.fields[3].text.trim();
        let init_options = if options.is_empty() {
            None
        } else {
            Some(
                serde_json::from_str(options)
                    .map_err(|e| format!("Initialization options must be valid JSON: {e}"))?,
            )
        };
        let settings = self.fields[4].text.trim();
        let settings = if settings.is_empty() { None } else {
            let value: serde_json::Value = serde_json::from_str(settings)
                .map_err(|error| format!("Server settings must be valid JSON: {error}"))?;
            if !value.is_object() { return Err("Server settings must be a JSON object.".into()); }
            Some(value)
        };
        Ok(LspServerConfig {
            language: language.into(),
            command: command.into(),
            args,
            file_patterns: patterns,
            init_options,
            settings,
        })
    }
}

#[derive(Default)]
pub(crate) struct LspSettingsState {
    pub active: bool,
    scope: SettingsScope,
    servers: Vec<LspServerConfig>,
    inherited: bool,
    pub draft: Option<Draft>,
    pub focus: Action,
    pub error: Option<String>,
    remove: Option<usize>,
    scroll: usize,
    max_scroll: usize,
    pub reveal_focus: bool,
    hits: Vec<(Rect, Action)>,
    body: Rect,
    width: usize,
    runtime: Vec<(String, String)>,
}

impl LspSettingsState {
    pub fn focused_input(&self) -> Option<TextInputId> {
        if let Action::Field(id) = self.focus {
            self.draft.as_ref()?.fields.get(id)?;
            Some(TextInputId::LspSetting(id))
        } else {
            None
        }
    }
}

impl App {
    pub(crate) fn open_lsp_settings_page(&mut self) {
        self.open_settings_tab();
        self.active_pane = ActivePane::Editor;
        self.settings_scope = if self.settings_workspace.lsp_servers.is_some()
            || self.settings_workspace.lsp_enabled.is_some()
        {
            SettingsScope::Workspace
        } else {
            SettingsScope::Global
        };
        self.settings_category_selected = 8;
        self.settings_category_collapsed[8] = false;
        self.settings_editor_search.clear();
        self.settings_editor_search_cursor = 0;
        self.settings_editor_search_selection_anchor = None;
        self.settings_editor_scroll = 0;
        self.settings_focus_categories = false;
        self.settings_focus_control = true;
        self.settings_selected = self
            .settings_items()
            .iter()
            .position(|(_, _, id)| id == "lsp_servers")
            .unwrap_or(0);
        self.open_lsp_settings();
    }

    pub(crate) fn open_lsp_settings(&mut self) {
        self.finish_numeric_setting_edit();
        self.close_settings_dropdown();
        self.settings_kb_editing = false;
        self.menu_active = false;
        self.menu_open = false;
        self.terminal_panel_focused = false;
        clear_inputs(self);
        self.lsp_settings = LspSettingsState {
            active: true,
            scope: self.settings_scope,
            servers: self
                .selected_scope_settings()
                .lsp_servers
                .unwrap_or_default(),
            inherited: self.settings_scope == SettingsScope::Workspace
                && self.settings_workspace.lsp_servers.is_none(),
            width: 76,
            ..Default::default()
        };
    }
}

fn clear_inputs(app: &mut App) {
    app.text_input_histories
        .retain(|id, _| !matches!(id, TextInputId::LspSetting(_)));
    app.remove_text_input_regions(|id| matches!(id, TextInputId::LspSetting(_)));
    app.text_input_drag = None;
    app.text_input_click_id = None;
    app.text_input_drag_unit = None;
}

/// Patch only this collection, retaining other settings (including unknown keys).
/// Persist before changing runtime state, so an I/O error leaves the draft usable.
fn persist(app: &mut App, servers: Vec<LspServerConfig>) -> Result<(), String> {
    let scope = app.lsp_settings.scope;
    let (path, layer) = match scope {
        SettingsScope::Global => (&app.settings_global_path, &app.settings_global),
        SettingsScope::Workspace => (&app.settings_workspace_path, &app.settings_workspace),
    };
    let mut settings = layer.clone();
    if app.settings_persistence_enabled {
        let same_path = |candidate: &std::path::Path| {
            candidate == path
                || candidate
                    .canonicalize()
                    .ok()
                    .zip(path.canonicalize().ok())
                    .is_some_and(|(a, b)| a == b)
        };
        if app
            .tabs
            .iter()
            .chain(app.groups.iter().flat_map(|group| &group.tabs))
            .any(|tab| {
                tab.editor.modified && tab.editor.file_path.as_ref().is_some_and(|p| same_path(p))
            })
        {
            return Err(
                "The settings file has unsaved edits. Save or close those edits, then try again."
                    .into(),
            );
        }
        let write = || -> Result<Settings, Box<dyn std::error::Error>> {
            let mut document: serde_json::Value = match std::fs::read_to_string(path) {
                Ok(text) => serde_json::from_str(&text)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
                Err(error) => return Err(error.into()),
            };
            let object = document
                .as_object_mut()
                .ok_or("Settings must be a JSON object")?;
            object.insert("lspServers".into(), serde_json::to_value(&servers)?);
            let updated = serde_json::from_value(document.clone())?;
            let parent = path.parent().ok_or("Settings path has no parent")?;
            std::fs::create_dir_all(parent)?;
            let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
            if let Ok(metadata) = path.metadata() {
                if metadata.permissions().readonly() {
                    return Err("Settings file is read-only".into());
                }
                temporary
                    .as_file()
                    .set_permissions(metadata.permissions())?;
            }
            serde_json::to_writer_pretty(&mut temporary, &document)?;
            temporary.write_all(b"\n")?;
            temporary.as_file().sync_all()?;
            temporary.persist(path)?;
            Ok(updated)
        };
        settings = write().map_err(|error| format!("Could not save servers: {error}"))?;
    }
    settings.lsp_servers = Some(servers.clone());
    match scope {
        SettingsScope::Global => app.settings_global = settings,
        SettingsScope::Workspace => app.settings_workspace = settings,
    }
    app.apply_effective_settings();
    app.lsp_settings.servers = servers;
    app.lsp_settings.inherited = false;
    app.notify("Language server settings saved and applied.");
    Ok(())
}

pub(crate) fn activate(app: &mut App, action: Action) {
    app.lsp_settings.error = None;
    app.lsp_settings.reveal_focus = true;
    match action {
        Action::Add | Action::Edit(_) => {
            let index = if let Action::Edit(index) = action {
                Some(index)
            } else {
                None
            };
            let config = index
                .and_then(|i| app.lsp_settings.servers.get(i))
                .cloned()
                .unwrap_or(LspServerConfig {
                    language: String::new(),
                    command: String::new(),
                    args: Vec::new(),
                    file_patterns: Vec::new(),
                    settings: None, init_options: None,
                });
            clear_inputs(app);
            app.lsp_settings.draft = Some(Draft::new(index, config));
            app.lsp_settings.focus = Action::Field(0);
            app.lsp_settings.scroll = 0;
            app.lsp_settings.remove = None;
        }
        Action::Cancel => {
            clear_inputs(app);
            app.lsp_settings.draft = None;
            app.lsp_settings.focus = Action::Add;
            app.lsp_settings.scroll = 0;
        }
        Action::Close => {
            clear_inputs(app);
            app.lsp_settings = LspSettingsState::default();
        }
        Action::Remove(index) => {
            app.lsp_settings.remove = Some(index);
            app.lsp_settings.focus = Action::Keep;
        }
        Action::Keep => {
            app.lsp_settings.remove = None;
            app.lsp_settings.focus = Action::Add;
        }
        Action::ConfirmRemove(index) => {
            let mut servers = app.lsp_settings.servers.clone();
            if index < servers.len() {
                servers.remove(index);
                match persist(app, servers) {
                    Ok(()) => {
                        app.lsp_settings.remove = None;
                        app.lsp_settings.focus = Action::Add;
                    }
                    Err(error) => app.lsp_settings.error = Some(error),
                }
            }
        }
        Action::Save => {
            let Some(draft) = &app.lsp_settings.draft else {
                return;
            };
            match draft.config(&app.lsp_settings.servers) {
                Ok(config) => {
                    let mut servers = app.lsp_settings.servers.clone();
                    if let Some(index) = draft.index {
                        servers[index] = config;
                    } else {
                        servers.push(config);
                    }
                    match persist(app, servers) {
                        Ok(()) => activate(app, Action::Cancel),
                        Err(error) => app.lsp_settings.error = Some(error),
                    }
                }
                Err(error) => app.lsp_settings.error = Some(error),
            }
        }
        Action::AddArgument => {
            if let Some(draft) = &mut app.lsp_settings.draft {
                let id = draft.push(String::new());
                draft.arguments.push(id);
                app.lsp_settings.focus = Action::Field(id);
            }
        }
        Action::RemoveArgument(id) => {
            if let Some(draft) = &mut app.lsp_settings.draft {
                draft.arguments.retain(|i| *i != id);
            }
            app.lsp_settings.focus = Action::AddArgument;
        }
        Action::Restart => {
            app.refresh_language_servers(true);
        }
        Action::Field(_) => app.lsp_settings.focus = action,
    }
    app.lsp_settings.hits.clear();
    app.remove_text_input_regions(|id| matches!(id, TextInputId::LspSetting(_)));
}

enum Row {
    Text(String, bool),
    Field(usize, &'static str, Option<Action>),
    Buttons(Vec<(String, Action)>),
    Gap,
}

fn note(rows: &mut Vec<Row>, text: &str, width: usize) {
    rows.extend(
        App::wrap_setting_text_for_width(text, width.max(1))
            .into_iter()
            .map(|text| Row::Text(text, false)),
    );
}

fn buttons(rows: &mut Vec<Row>, choices: Vec<(String, Action)>, width: usize) {
    let mut line = Vec::new();
    let mut used = 0;
    for (label, action) in choices {
        let w = (label.width() + 2).min(width);
        if !line.is_empty() && used + w > width {
            rows.push(Row::Buttons(std::mem::take(&mut line)));
            used = 0;
        }
        used += w + 1;
        line.push((label, action));
    }
    if !line.is_empty() {
        rows.push(Row::Buttons(line));
    }
}

fn rows(state: &LspSettingsState, width: usize) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(draft) = &state.draft {
        for (id, label, placeholder) in [
            (0, "Language ID", "e.g. rust, python, typescript"),
            (1, "Executable", "Executable on PATH, or an absolute path"),
            (
                2,
                "File patterns",
                "e.g. *.rs or *.ts *.tsx (separate with spaces)",
            ),
        ] {
            rows.push(Row::Text(label.into(), true));
            rows.push(Row::Field(id, placeholder, None));
            rows.push(Row::Gap);
        }
        note(
            &mut rows,
            "Install the server separately. It must support LSP over stdin/stdout. Paths need no surrounding quotes. Windows .cmd and .bat launchers are supported.",
            width,
        );
        rows.push(Row::Gap);
        rows.push(Row::Text("Arguments".into(), true));
        note(
            &mut rows,
            "One argument per field, in order. Spaces stay inside that argument. Add --stdio here if your server requires it.",
            width,
        );
        for (number, &id) in draft.arguments.iter().enumerate() {
            rows.push(Row::Text(format!("Argument {}", number + 1), false));
            rows.push(Row::Field(
                id,
                "Argument (may be empty)",
                Some(Action::RemoveArgument(id)),
            ));
        }
        buttons(
            &mut rows,
            vec![("+ Add Argument".into(), Action::AddArgument)],
            width,
        );
        rows.push(Row::Gap);
        rows.push(Row::Text(
            "Initialization options (optional JSON)".into(),
            true,
        ));
        rows.push(Row::Field(
            3,
            "Leave empty to use the server defaults",
            None,
        ));
        rows.push(Row::Gap);
        rows.push(Row::Text("Server settings (optional JSON)".into(), true));
        rows.push(Row::Field(4, r#"Example: {"python":{"analysis":{"typeCheckingMode":"basic"}}}"#, None));
    } else {
        note(
            &mut rows,
            "Install a language server, then add its executable and the files it handles. Example: language rust, executable rust-analyzer, file pattern *.rs.",
            width,
        );
        if state.inherited {
            note(
                &mut rows,
                "Inherited from User settings. Saving here creates a Workspace server list.",
                width,
            );
        }
        note(
            &mut rows,
            "Saved entries override extension servers with the same language ID. Use Enable Language Servers on the settings page to turn LSP on or off.",
            width,
        );
        rows.push(Row::Gap);
        buttons(
            &mut rows,
            vec![
                ("+ Add Server".into(), Action::Add),
                ("Restart Servers".into(), Action::Restart),
            ],
            width,
        );
        rows.push(Row::Gap);
        if !state.runtime.is_empty() {
            rows.push(Row::Text(
                "Current servers (including extensions)".into(),
                true,
            ));
            for (language, status) in &state.runtime {
                note(&mut rows, &format!("{language}: {status}"), width);
            }
            rows.push(Row::Gap);
        }
        if state.servers.is_empty() {
            rows.push(Row::Text("No custom servers configured".into(), true));
        }
        for (index, server) in state.servers.iter().enumerate() {
            rows.push(Row::Text(server.language.clone(), true));
            note(&mut rows, &server.command, width);
            note(&mut rows, &server.file_patterns.join(" "), width);
            if state.remove == Some(index) {
                note(&mut rows, "Remove this server from these settings?", width);
                buttons(
                    &mut rows,
                    vec![
                        ("Remove Server".into(), Action::ConfirmRemove(index)),
                        ("Keep Server".into(), Action::Keep),
                    ],
                    width,
                );
            } else {
                buttons(
                    &mut rows,
                    vec![
                        ("Edit".into(), Action::Edit(index)),
                        ("Remove".into(), Action::Remove(index)),
                    ],
                    width,
                );
            }
            rows.push(Row::Gap);
        }
    }
    rows
}

fn footer(state: &LspSettingsState, width: usize) -> Vec<Row> {
    let mut rows = Vec::new();
    let choices = if state.draft.is_some() {
        vec![
            ("Save Server".into(), Action::Save),
            ("Cancel".into(), Action::Cancel),
        ]
    } else {
        vec![("Done".into(), Action::Close)]
    };
    buttons(&mut rows, choices, width);
    rows
}

fn actions(rows: &[Row]) -> Vec<(usize, Action)> {
    let mut actions = Vec::new();
    for (y, row) in rows.iter().enumerate() {
        match row {
            Row::Field(id, _, remove) => {
                actions.push((y, Action::Field(*id)));
                if let Some(action) = remove {
                    actions.push((y, *action));
                }
            }
            Row::Buttons(buttons) => actions.extend(buttons.iter().map(|(_, action)| (y, *action))),
            _ => {}
        }
    }
    actions
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('s' | 'S')
            if key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
                && !key.modifiers.contains(KeyModifiers::ALT)
                && app.lsp_settings.draft.is_some() =>
        {
            activate(app, Action::Save);
        }
        KeyCode::Esc => {
            let action = if app.lsp_settings.draft.is_some() {
                Action::Cancel
            } else if app.lsp_settings.remove.is_some() {
                Action::Keep
            } else {
                Action::Close
            };
            activate(app, action);
        }
        KeyCode::Tab | KeyCode::BackTab => {
            let state = &app.lsp_settings;
            let all: Vec<_> = actions(&rows(state, state.width))
                .into_iter()
                .chain(actions(&footer(state, state.width)))
                .map(|(_, action)| action)
                .collect();
            let reverse =
                key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
            if !all.is_empty() {
                let index = all
                    .iter()
                    .position(|action| *action == state.focus)
                    .unwrap_or(0);
                app.lsp_settings.focus =
                    all[(index + if reverse { all.len() - 1 } else { 1 }) % all.len()];
                app.lsp_settings.reveal_focus = true;
            }
        }
        KeyCode::PageUp | KeyCode::PageDown => {
            let state = &mut app.lsp_settings;
            let page = usize::from(state.body.height).max(1);
            state.scroll = if key.code == KeyCode::PageUp {
                state.scroll.saturating_sub(page)
            } else {
                (state.scroll + page).min(state.max_scroll)
            };
            state.reveal_focus = false;
        }
        KeyCode::Enter | KeyCode::Char(' ')
            if !matches!(app.lsp_settings.focus, Action::Field(_)) =>
        {
            activate(app, app.lsp_settings.focus);
        }
        _ => {
            if let Some(id) = app.lsp_settings.focused_input()
                && app
                    .edit_text_input(id, key.code, key.modifiers, None)
                    .changed
            {
                app.text_input_changed(id);
            }
        }
    }
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    // Only this dialog's fields may receive pointer input through the modal.
    let hit = app.lsp_settings.hits.iter().any(|(area, action)| {
        matches!(action, Action::Field(_)) && area.contains((mouse.column, mouse.row).into())
    });
    if (hit || matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_)))
        && app.handle_overlay_text_input_mouse(mouse.kind, mouse.column, mouse.row, mouse.modifiers)
    {
        return;
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some((_, action)) = app
                .lsp_settings
                .hits
                .iter()
                .find(|(area, _)| area.contains((mouse.column, mouse.row).into()))
                .copied()
            {
                activate(app, action);
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let state = &mut app.lsp_settings;
            state.scroll = if mouse.kind == MouseEventKind::ScrollUp {
                state.scroll.saturating_sub(3)
            } else {
                (state.scroll + 3).min(state.max_scroll)
            };
            state.reveal_focus = false;
        }
        _ => {}
    }
}

fn button(f: &mut Frame, app: &mut App, area: Rect, label: &str, action: Action) {
    if area.is_empty() {
        return;
    }
    let focused = action == app.lsp_settings.focus;
    let hovered = area.contains((app.mouse_col, app.mouse_row).into());
    let bg = crate::ui_widgets::surface_background(f, area, app.colors.ui.bg_widget);
    let mut style = app
        .colors
        .ui
        .button_style(
            bg,
            matches!(action, Action::Save | Action::Add),
            hovered,
            true,
        )
        .add_modifier(Modifier::BOLD);
    if focused && let Some(border) = app.colors.ui.focus_border.border(style.bg.unwrap_or(bg)) {
        style = style
            .add_modifier(Modifier::UNDERLINED)
            .underline_color(border);
    }
    f.render_widget(Paragraph::new(format!(" {label} ")).style(style), area);
    app.lsp_settings.hits.push((area, action));
}

fn draw_row(f: &mut Frame, app: &mut App, row: &Row, area: Rect) {
    let bg = crate::ui_widgets::surface_background(f, area, app.colors.ui.bg_widget);
    match row {
        Row::Text(text, heading) => {
            let style = Style::default().fg(if *heading {
                app.colors.ui.widget_text_color(bg)
            } else {
                app.colors.ui.description_color(bg)
            });
            f.render_widget(
                Paragraph::new(text.as_str()).style(if *heading {
                    style.add_modifier(Modifier::BOLD)
                } else {
                    style
                }),
                area,
            );
        }
        Row::Field(id, placeholder, remove) => {
            let field_width = area
                .width
                .saturating_sub(if remove.is_some() { 4 } else { 0 });
            let input_area = Rect::new(area.x, area.y, field_width, 1);
            let focused = app.lsp_settings.focus == Action::Field(*id);
            let field = &app.lsp_settings.draft.as_ref().unwrap().fields[*id];
            let input_style =
                crate::ui_settings::settings_control_style(&app.colors, "text", bg, focused);
            let input_bg = input_style.bg.unwrap_or(bg);
            let input = InputField::new(&field.text)
                .theme_control(&app.colors, app.colors.ui.settings_control("text"), bg)
                .prefix(" ", app.colors.ui.description_color(input_bg))
                .bg(input_bg)
                .placeholder_text(placeholder)
                .cursor_marker(if focused { "█" } else { "" })
                .cursor_pos(field.cursor)
                .selection_range(App::inline_selection_range(field.cursor, field.anchor));
            let hit = input.render(f, input_area);
            crate::ui_settings::underline_field(f, input_area, input_style);
            app.register_text_input(TextInputId::LspSetting(*id), hit);
            app.lsp_settings.hits.push((input_area, Action::Field(*id)));
            if let Some(action) = remove {
                button(
                    f,
                    app,
                    Rect::new(area.right().saturating_sub(3), area.y, 3, 1),
                    "×",
                    *action,
                );
            }
        }
        Row::Buttons(choices) => {
            let mut x = area.x;
            for (label, action) in choices {
                let width = (label.width() as u16 + 2).min(area.right().saturating_sub(x));
                button(f, app, Rect::new(x, area.y, width, 1), label, *action);
                x = x.saturating_add(width + 1);
            }
        }
        Row::Gap => {}
    }
}

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    app.lsp_settings.runtime = if app.lsp_enabled {
        app.lsp_manager
            .configs
            .iter()
            .map(|config| {
                (
                    config.language.clone(),
                    app.lsp_manager.server_status(&config.language),
                )
            })
            .collect()
    } else {
        vec![("LSP".into(), "Disabled in settings".into())]
    };
    app.lsp_settings.hits.clear();
    app.remove_text_input_regions(|id| matches!(id, TextInputId::LspSetting(_)));
    let viewport = f.area();
    let width = viewport
        .width
        .saturating_sub(u16::from(viewport.width >= 50) * 4)
        .min(88);
    let content_width = width.saturating_sub(5) as usize;
    let desired_height = rows(&app.lsp_settings, content_width).len()
        + footer(&app.lsp_settings, content_width).len()
        + 6
        + App::wrap_setting_text_for_width(
            app.lsp_settings
                .error
                .as_deref()
                .unwrap_or("Tab / Shift+Tab to navigate · Scroll for more"),
            width.saturating_sub(4) as usize,
        )
        .len()
        .min(3);
    let height = viewport
        .height
        .saturating_sub(u16::from(viewport.height >= 24) * 2)
        .min(desired_height.min(u16::MAX as usize) as u16)
        .min(36);
    let area = Rect::new(
        viewport.x + (viewport.width - width) / 2,
        viewport.y + (viewport.height - height) / 2,
        width,
        height,
    );
    for cell in &mut f.buffer_mut().content {
        cell.modifier.insert(Modifier::DIM);
    }
    let bg = crate::ui_widgets::widget_background(f, &app.colors, area);
    let fg = app.colors.ui.widget_text_color(bg);
    f.render_widget(Clear, area);
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(app.colors.ui.widget_border.border(bg).unwrap_or(bg)))
            .style(Style::default().fg(fg).bg(bg)),
        area,
    );
    if width < 12 || height < 8 {
        return;
    }
    let inner = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    let scope = if app.lsp_settings.scope == SettingsScope::Global {
        "User"
    } else {
        "Workspace"
    };
    let title = if let Some(draft) = &app.lsp_settings.draft {
        if draft.index.is_some() {
            "Edit Server"
        } else {
            "Add Server"
        }
    } else {
        "Language Servers"
    };
    f.render_widget(
        Paragraph::new(format!("{title} · {scope}"))
            .style(Style::default().add_modifier(Modifier::BOLD)),
        Rect::new(inner.x, inner.y, inner.width.saturating_sub(4), 1),
    );
    button(
        f,
        app,
        Rect::new(inner.right() - 3, inner.y, 3, 1),
        "×",
        Action::Close,
    );
    let footer_rows = footer(&app.lsp_settings, inner.width as usize);
    let notices = App::wrap_setting_text_for_width(
        app.lsp_settings
            .error
            .as_deref()
            .unwrap_or("Tab / Shift+Tab to navigate · Scroll for more"),
        inner.width as usize,
    );
    let notice_height = notices
        .len()
        .min(3)
        .min(inner.height.saturating_sub(5) as usize) as u16;
    let footer_y = inner.bottom().saturating_sub(footer_rows.len() as u16);
    let notice_y = footer_y.saturating_sub(notice_height + 1);
    let body = Rect::new(
        inner.x,
        inner.y + 2,
        inner.width.saturating_sub(1),
        notice_y.saturating_sub(inner.y + 3),
    );
    let content = rows(&app.lsp_settings, body.width as usize);
    let state = &mut app.lsp_settings;
    state.width = body.width as usize;
    state.body = body;
    state.max_scroll = content.len().saturating_sub(body.height as usize);
    if state.reveal_focus {
        if let Some((y, _)) = actions(&content)
            .into_iter()
            .find(|(_, action)| *action == state.focus)
        {
            if y < state.scroll {
                state.scroll = y.saturating_sub(1);
            }
            if y >= state.scroll + body.height as usize {
                state.scroll = (y + 1).saturating_sub(body.height as usize);
            }
        }
        state.reveal_focus = false;
    }
    state.scroll = state.scroll.min(state.max_scroll);
    for (index, row) in content
        .iter()
        .skip(state.scroll)
        .take(body.height as usize)
        .enumerate()
    {
        draw_row(
            f,
            app,
            row,
            Rect::new(body.x, body.y + index as u16, body.width, 1),
        );
    }
    if app.lsp_settings.max_scroll > 0 && body.height > 0 {
        let thumb = body.y
            + (usize::from(body.height - 1) * app.lsp_settings.scroll / app.lsp_settings.max_scroll)
                as u16;
        let (track, slider) = app.colors.ui.scrollbar_colors(bg, false, false);
        f.render_widget(
            Paragraph::new("▐").style(Style::default().fg(slider).bg(track)),
            Rect::new(inner.right() - 1, thumb, 1, 1),
        );
    }
    for (i, line) in notices.iter().take(notice_height as usize).enumerate() {
        f.render_widget(
            Paragraph::new(line.as_str()).style(Style::default().fg(
                if app.lsp_settings.error.is_some() {
                    app.colors.ui.error_text_color(bg)
                } else {
                    app.colors.ui.description_color(bg)
                },
            )),
            Rect::new(inner.x, notice_y + i as u16, inner.width, 1),
        );
    }
    for (i, row) in footer_rows.iter().enumerate() {
        draw_row(
            f,
            app,
            row,
            Rect::new(inner.x, footer_y + i as u16, inner.width, 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn render(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
        terminal
    }

    fn capture(app: &mut App, name: &str, width: u16, height: u16) {
        let Some(folder) = std::env::var_os("VTCODE_LSP_SETTINGS_CAPTURE") else {
            return;
        };
        let terminal = render(app, width, height);
        let cells: Vec<_> = terminal.backend().buffer().content.iter().map(|cell| serde_json::json!({
            "text":cell.symbol(), "fg":format!("{:?}",cell.fg), "bg":format!("{:?}",cell.bg),
            "bold":cell.modifier.contains(Modifier::BOLD)
        })).collect();
        let folder = std::path::PathBuf::from(folder);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(
            folder.join(format!("lsp-{name}.json")),
            serde_json::to_vec(&serde_json::json!({"width":width,"height":height,"cells":cells}))
                .unwrap(),
        )
        .unwrap();
    }

    fn click(app: &mut App, action: Action) {
        app.lsp_settings.focus = action;
        app.lsp_settings.reveal_focus = true;
        render(app, 100, 30);
        let area = app
            .lsp_settings
            .hits
            .iter()
            .find(|(_, a)| *a == action)
            .unwrap()
            .0;
        crate::mouse::handle_mouse(
            app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x + 1,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            },
        );
    }

    fn fill(app: &mut App, id: usize, value: &str) {
        click(app, Action::Field(id));
        crate::input_keyboard::handle_keyboard_event(
            app,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
        );
        assert!(app.paste_into_focused_text_input(value));
        assert_eq!(
            app.lsp_settings.draft.as_ref().unwrap().fields[id].text,
            value
        );
    }

    fn app_at(root: &std::path::Path) -> App {
        let mut app = App::test_new();
        app.explorer.root = root.into();
        app.settings_global_path = root.join("user.json");
        app.settings_workspace_path = root.join("workspace.json");
        app.settings_persistence_enabled = true;
        app.settings_global = crate::settings::load_from_file(&app.settings_global_path);
        app.settings_workspace = crate::settings::load_from_file(&app.settings_workspace_path);
        app.apply_effective_settings();
        app
    }

    fn fill_server(app: &mut App) {
        fill(app, 0, "rust");
        fill(app, 1, "server with spaces.exe");
        fill(app, 2, "*.rs *.test.rs Cargo.toml");
        fill(app, 3, r#"{"check":{"command":"clippy"}}"#);
        fill(app, 4, r#"{"rust-analyzer":{"check":{"command":"clippy"}}}"#);
        click(app, Action::AddArgument);
        let id = *app
            .lsp_settings
            .draft
            .as_ref()
            .unwrap()
            .arguments
            .last()
            .unwrap();
        fill(app, id, "an argument with spaces/世界");
    }

    #[test]
    fn setup_form_persists_scopes_literal_arguments_and_unknown_settings() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("user.json"),
            r#"{"tabSize":7,"future.setting":42}"#,
        )
        .unwrap();
        let mut app = app_at(root.path());
        app.open_lsp_settings_page();
        assert_eq!(app.settings_category_selected, 8);
        click(&mut app, Action::Add);
        fill_server(&mut app);
        click(&mut app, Action::Save);
        assert!(
            app.lsp_settings.draft.is_none(),
            "{:?}",
            app.lsp_settings.error
        );
        let global_text = std::fs::read_to_string(&app.settings_global_path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&global_text).unwrap();
        assert_eq!(json["future.setting"], 42);
        assert_eq!(json["tabSize"], 7);
        let saved = crate::settings::load_from_file(&app.settings_global_path)
            .lsp_servers
            .unwrap();
        assert_eq!(saved[0].args, ["an argument with spaces/世界"]);
        assert_eq!(saved[0].settings.as_ref().unwrap()["rust-analyzer"]["check"]["command"], "clippy");
        assert_eq!(saved[0].file_patterns, ["*.rs", "*.test.rs", "Cargo.toml"]);
        assert_eq!(app.lsp_manager.configs, saved);
        assert!(app.lsp_manager.clients.is_empty());
        click(&mut app, Action::Close);

        app.settings_scope = SettingsScope::Workspace;
        app.toggle_setting("lsp_servers");
        assert!(app.lsp_settings.inherited);
        click(&mut app, Action::Edit(0));
        fill(&mut app, 1, "workspace-server.exe");
        click(&mut app, Action::Save);
        assert_eq!(app.lsp_manager.configs[0].command, "workspace-server.exe");
        assert_eq!(
            std::fs::read_to_string(&app.settings_global_path).unwrap(),
            global_text
        );
        drop(app);
        let mut app = app_at(root.path());
        assert_eq!(app.lsp_manager.configs[0].command, "workspace-server.exe");
        app.open_lsp_settings_page();
        assert_eq!(app.settings_scope, SettingsScope::Workspace);
        click(&mut app, Action::Remove(0));
        click(&mut app, Action::ConfirmRemove(0));
        assert_eq!(app.settings_workspace.lsp_servers, Some(vec![]));
        assert!(app.lsp_manager.configs.is_empty());
        assert_eq!(
            std::fs::read_to_string(&app.settings_global_path).unwrap(),
            global_text
        );
    }

    #[test]
    fn invalid_or_unsaved_settings_keep_the_form_and_runtime_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let mut app = app_at(root.path());
        app.open_lsp_settings_page();
        click(&mut app, Action::Add);
        fill_server(&mut app);
        fill(&mut app, 3, "not json");
        click(&mut app, Action::Save);
        assert!(app.lsp_settings.error.as_deref().unwrap().contains("JSON"));
        assert!(app.lsp_manager.configs.is_empty());
        fill(&mut app, 3, "{}");
        fill(&mut app, 4, "[]");
        click(&mut app, Action::Save);
        assert!(app.lsp_settings.error.as_deref().unwrap().contains("JSON object"));
        fill(&mut app, 4, "{}");
        fill(&mut app, 2, "src/**/*.rs");
        click(&mut app, Action::Save);
        assert!(
            app.lsp_settings
                .error
                .as_deref()
                .unwrap()
                .contains("file extensions")
        );
        fill(&mut app, 2, "*.rs");
        std::fs::write(&app.settings_global_path, "invalid settings").unwrap();
        click(&mut app, Action::Save);
        assert!(
            app.lsp_settings
                .error
                .as_deref()
                .unwrap()
                .contains("Could not save")
        );
        assert!(app.lsp_settings.draft.is_some());
        assert_eq!(
            std::fs::read_to_string(&app.settings_global_path).unwrap(),
            "invalid settings"
        );
        std::fs::write(&app.settings_global_path, "{}").unwrap();
        let mut editor = crate::editor::Editor::test_with_text("unsaved settings");
        editor.file_path = Some(app.settings_global_path.clone());
        editor.modified = true;
        app.tabs.push(crate::app::Tab::new_file(editor));
        click(&mut app, Action::Save);
        assert!(
            app.lsp_settings
                .error
                .as_deref()
                .unwrap()
                .contains("unsaved edits")
        );
        assert!(app.lsp_manager.configs.is_empty());
    }

    #[test]
    fn setup_is_searchable_and_controls_fit_short_and_narrow_views() {
        let mut app = App::test_new();
        app.execute_palette_command("settings");
        app.settings_editor_search = "lsp".into();
        let items = app.settings_items();
        assert!(
            app.settings_filtered_indices_for_category(0)
                .iter()
                .any(|&i| items[i].2 == "lsp_servers")
        );
        app.settings_editor_search.clear();
        app.settings_category_selected = 8;
        app.ensure_settings_selection_visible();
        let terminal = render(&mut app, 120, 40);
        let buffer = terminal.backend().buffer();
        let button = (0..40)
            .find_map(|y| {
                (0..105).find_map(|x| {
                    let text: String = (x..x + 14).map(|col| buffer[(col, y)].symbol()).collect();
                    (text == "Manage Servers").then_some((x, y))
                })
            })
            .expect("Settings shows a visible Manage Servers button");
        capture(&mut app, "page", 120, 40);
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: button.0,
                row: button.1,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(app.lsp_settings.active);
        capture(&mut app, "list", 100, 30);
        click(&mut app, Action::Add);
        capture(&mut app, "form", 100, 30);
        for (width, height) in [(120, 40), (80, 24), (40, 14)] {
            for action in [
                Action::Field(0),
                Action::Field(1),
                Action::Field(2),
                Action::Field(3),
                Action::AddArgument,
                Action::Save,
                Action::Cancel,
            ] {
                app.lsp_settings.focus = action;
                app.lsp_settings.reveal_focus = true;
                let terminal = render(&mut app, width, height);
                let area = app
                    .lsp_settings
                    .hits
                    .iter()
                    .find(|(_, a)| *a == action)
                    .unwrap()
                    .0;
                assert!(!area.is_empty());
                assert!(
                    terminal
                        .backend()
                        .buffer()
                        .area
                        .contains((area.x, area.y).into())
                );
                assert!(
                    app.lsp_settings
                        .hits
                        .iter()
                        .any(|(_, a)| *a == Action::Save)
                );
            }
        }
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(app.lsp_settings.draft.is_none());
        assert!(app.lsp_settings.active);
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(!app.lsp_settings.active);
    }
}
