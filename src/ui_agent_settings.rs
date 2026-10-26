//! Scoped agent management with structured fields and the workbench input controller.
use crate::{
    app::{ActivePane, App},
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
use std::{collections::BTreeMap, io::Write};
use unicode_width::UnicodeWidthStr;
use vtcode::acp::AgentConfig;

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
    AddEnvironment,
    RemoveEnvironment(usize),
    Save,
    Cancel,
    Close,
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
    // Field identities never move when an argument or variable is removed.
    arguments: Vec<usize>,
    environment: Vec<(usize, usize)>,
}

impl Draft {
    fn new(index: Option<usize>, config: AgentConfig) -> Self {
        let mut draft = Self {
            index,
            fields: vec![
                Field::new(config.name),
                Field::new(config.command),
                Field::new(config.request_timeout_secs.to_string()),
            ],
            arguments: Vec::new(),
            environment: Vec::new(),
        };
        for argument in config.args {
            let id = draft.push(argument);
            draft.arguments.push(id);
        }
        for (name, value) in config.env {
            let name = draft.push(name);
            let value = draft.push(value);
            draft.environment.push((name, value));
        }
        draft
    }

    fn push(&mut self, text: String) -> usize {
        let id = self.fields.len();
        self.fields.push(Field::new(text));
        id
    }

    fn config(&self, agents: &[AgentConfig]) -> Result<AgentConfig, String> {
        let name = self.fields[0].text.trim();
        let command = self.fields[1].text.trim();
        if name.is_empty() {
            return Err("Enter a name for this agent.".into());
        }
        if command.is_empty() {
            return Err("Enter the agent executable or interpreter path.".into());
        }
        if agents
            .iter()
            .enumerate()
            .any(|(i, agent)| Some(i) != self.index && agent.name.eq_ignore_ascii_case(name))
        {
            return Err("An agent with this name already exists. Choose a different name.".into());
        }
        let timeout = self.fields[2]
            .text
            .trim()
            .parse::<u64>()
            .ok()
            .filter(|value| (1..=3600).contains(value))
            .ok_or("Enter a request timeout between 1 and 3600 seconds.")?;
        let mut env = BTreeMap::new();
        for &(key, value) in &self.environment {
            let key = self.fields[key].text.trim();
            let value = &self.fields[value].text;
            if key.is_empty() || key.contains(['=', '\0']) {
                return Err("Each environment variable needs a name without '=' or NUL.".into());
            }
            if env.keys().any(|existing: &String| {
                if cfg!(windows) {
                    existing.eq_ignore_ascii_case(key)
                } else {
                    existing == key
                }
            }) {
                return Err(format!(
                    "Environment variable '{key}' is listed more than once."
                ));
            }
            env.insert(key.to_owned(), value.clone());
        }
        let args: Vec<_> = self
            .arguments
            .iter()
            .map(|&id| self.fields[id].text.clone())
            .collect();
        if [name, command]
            .into_iter()
            .chain(args.iter().map(String::as_str))
            .chain(env.values().map(String::as_str))
            .any(|value| value.contains('\0'))
        {
            return Err(
                "Names, commands, arguments, and environment values cannot contain NUL.".into(),
            );
        }
        Ok(AgentConfig {
            name: name.into(),
            command: command.into(),
            args,
            env,
            request_timeout_secs: timeout,
        })
    }
}

#[derive(Default)]
pub(crate) struct AgentSettingsState {
    pub active: bool,
    scope: SettingsScope,
    agents: Vec<AgentConfig>,
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
}

impl AgentSettingsState {
    pub fn focused_input(&self) -> Option<TextInputId> {
        if let Action::Field(id) = self.focus {
            self.draft.as_ref()?.fields.get(id)?;
            Some(TextInputId::AgentSetting(id))
        } else {
            None
        }
    }
}

impl App {
    pub(crate) fn open_agent_settings_page(&mut self) {
        self.open_settings_tab();
        self.active_pane = ActivePane::Editor;
        self.settings_scope = if self.settings_workspace.acp_agents.is_some() {
            SettingsScope::Workspace
        } else {
            SettingsScope::Global
        };
        self.settings_category_selected = 6;
        self.settings_editor_search.clear();
        self.settings_editor_search_cursor = 0;
        self.settings_editor_search_selection_anchor = None;
        self.settings_editor_scroll = 0;
        self.settings_focus_categories = false;
        self.settings_focus_control = true;
        self.settings_selected = self
            .settings_items()
            .iter()
            .position(|(_, _, id)| id == "acp_agents")
            .unwrap_or(0);
        self.open_agent_settings();
    }

    pub(crate) fn open_agent_settings(&mut self) {
        self.finish_numeric_setting_edit();
        self.close_settings_dropdown();
        self.settings_kb_editing = false;
        self.menu_active = false;
        self.menu_open = false;
        self.terminal_panel_focused = false;
        clear_inputs(self);
        self.agent_settings = AgentSettingsState {
            active: true,
            scope: self.settings_scope,
            agents: self
                .selected_scope_settings()
                .acp_agents
                .unwrap_or_default(),
            inherited: self.settings_scope == SettingsScope::Workspace
                && self.settings_workspace.acp_agents.is_none(),
            width: 76,
            ..Default::default()
        };
    }
}

fn clear_inputs(app: &mut App) {
    app.text_input_histories
        .retain(|id, _| !matches!(id, TextInputId::AgentSetting(_)));
    app.remove_text_input_regions(|id| matches!(id, TextInputId::AgentSetting(_)));
    app.text_input_drag = None;
    app.text_input_click_id = None;
    app.text_input_drag_unit = None;
}

/// Patch only this collection, retaining other settings (including unknown keys).
/// Persist before changing runtime state, so an I/O error leaves the draft usable.
fn persist(app: &mut App, agents: Vec<AgentConfig>) -> Result<(), String> {
    let scope = app.agent_settings.scope;
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
            object.insert("acpAgents".into(), serde_json::to_value(&agents)?);
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
        settings = write().map_err(|error| format!("Could not save agents: {error}"))?;
    }
    settings.acp_agents = Some(agents.clone());
    match scope {
        SettingsScope::Global => app.settings_global = settings,
        SettingsScope::Workspace => app.settings_workspace = settings,
    }
    app.apply_effective_settings();
    app.agent_settings.agents = agents;
    app.agent_settings.inherited = false;
    app.notify("Agent settings saved. Changes apply when you next connect.");
    Ok(())
}

pub(crate) fn activate(app: &mut App, action: Action) {
    app.agent_settings.error = None;
    app.agent_settings.reveal_focus = true;
    match action {
        Action::Add | Action::Edit(_) => {
            let index = if let Action::Edit(index) = action {
                Some(index)
            } else {
                None
            };
            let config = index
                .and_then(|i| app.agent_settings.agents.get(i))
                .cloned()
                .unwrap_or_else(|| AgentConfig::new("", ""));
            clear_inputs(app);
            app.agent_settings.draft = Some(Draft::new(index, config));
            app.agent_settings.focus = Action::Field(0);
            app.agent_settings.scroll = 0;
            app.agent_settings.remove = None;
        }
        Action::Cancel => {
            clear_inputs(app);
            app.agent_settings.draft = None;
            app.agent_settings.focus = Action::Add;
            app.agent_settings.scroll = 0;
        }
        Action::Close => {
            clear_inputs(app);
            app.agent_settings = AgentSettingsState::default();
        }
        Action::Remove(index) => {
            app.agent_settings.remove = Some(index);
            app.agent_settings.focus = Action::Keep;
        }
        Action::Keep => {
            app.agent_settings.remove = None;
            app.agent_settings.focus = Action::Add;
        }
        Action::ConfirmRemove(index) => {
            let mut agents = app.agent_settings.agents.clone();
            if index < agents.len() {
                agents.remove(index);
                match persist(app, agents) {
                    Ok(()) => {
                        app.agent_settings.remove = None;
                        app.agent_settings.focus = Action::Add;
                    }
                    Err(error) => app.agent_settings.error = Some(error),
                }
            }
        }
        Action::Save => {
            let Some(draft) = &app.agent_settings.draft else {
                return;
            };
            match draft.config(&app.agent_settings.agents) {
                Ok(config) => {
                    let mut agents = app.agent_settings.agents.clone();
                    if let Some(index) = draft.index {
                        agents[index] = config;
                    } else {
                        agents.push(config);
                    }
                    match persist(app, agents) {
                        Ok(()) => activate(app, Action::Cancel),
                        Err(error) => app.agent_settings.error = Some(error),
                    }
                }
                Err(error) => app.agent_settings.error = Some(error),
            }
        }
        Action::AddArgument => {
            if let Some(draft) = &mut app.agent_settings.draft {
                let id = draft.push(String::new());
                draft.arguments.push(id);
                app.agent_settings.focus = Action::Field(id);
            }
        }
        Action::AddEnvironment => {
            if let Some(draft) = &mut app.agent_settings.draft {
                let key = draft.push(String::new());
                let value = draft.push(String::new());
                draft.environment.push((key, value));
                app.agent_settings.focus = Action::Field(key);
            }
        }
        Action::RemoveArgument(id) => {
            if let Some(draft) = &mut app.agent_settings.draft {
                draft.arguments.retain(|i| *i != id);
            }
            app.agent_settings.focus = Action::AddArgument;
        }
        Action::RemoveEnvironment(key) => {
            if let Some(draft) = &mut app.agent_settings.draft {
                draft.environment.retain(|(id, _)| *id != key);
            }
            app.agent_settings.focus = Action::AddEnvironment;
        }
        Action::Field(_) => app.agent_settings.focus = action,
    }
    app.agent_settings.hits.clear();
    app.remove_text_input_regions(|id| matches!(id, TextInputId::AgentSetting(_)));
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

fn rows(state: &AgentSettingsState, width: usize) -> Vec<Row> {
    let mut rows = Vec::new();
    if let Some(draft) = &state.draft {
        for (id, label, placeholder) in [
            (0, "Name", "A name for this agent"),
            (1, "Executable", "Executable on PATH, or an absolute path"),
        ] {
            rows.push(Row::Text(label.into(), true));
            rows.push(Row::Field(id, placeholder, None));
            rows.push(Row::Gap);
        }
        note(
            &mut rows,
            "Use a full path if the command is not on PATH. Windows .cmd and .bat launchers are supported. For other scripts, use the interpreter and add the script path as an argument. Paths need no surrounding quotes.",
            width,
        );
        rows.push(Row::Gap);
        rows.push(Row::Text("Arguments".into(), true));
        note(
            &mut rows,
            "One argument per field, in order. Spaces stay inside that argument.",
            width,
        );
        for (number, &id) in draft.arguments.iter().enumerate() {
            rows.push(Row::Text(format!("Argument {}", number + 1), false));
            rows.push(Row::Field(
                id,
                "Argument (an empty value is allowed)",
                Some(Action::RemoveArgument(id)),
            ));
        }
        buttons(
            &mut rows,
            vec![("+ Add Argument".into(), Action::AddArgument)],
            width,
        );
        rows.push(Row::Gap);
        rows.push(Row::Text("Environment variables".into(), true));
        note(
            &mut rows,
            "Optional overrides for the agent process. Other variables are inherited.",
            width,
        );
        for &(key, value) in &draft.environment {
            rows.push(Row::Text("Variable name".into(), false));
            rows.push(Row::Field(
                key,
                "NAME",
                Some(Action::RemoveEnvironment(key)),
            ));
            rows.push(Row::Text("Value".into(), false));
            rows.push(Row::Field(value, "Value (may be empty)", None));
        }
        buttons(
            &mut rows,
            vec![("+ Add Variable".into(), Action::AddEnvironment)],
            width,
        );
        rows.push(Row::Gap);
        rows.push(Row::Text("Request timeout (seconds)".into(), true));
        rows.push(Row::Field(2, "60", None));
        note(
            &mut rows,
            "1–3600 seconds for connection and control requests. Prompts can keep running until completion or cancellation.",
            width,
        );
    } else {
        if state.inherited {
            note(
                &mut rows,
                "Inherited from User settings. Saving here creates a Workspace agent list; User settings stay unchanged.",
                width,
            );
        } else {
            note(
                &mut rows,
                "Connect any local agent that supports ACP over stdio. Changes apply on the next connection.",
                width,
            );
        }
        rows.push(Row::Gap);
        buttons(&mut rows, vec![("+ Add Agent".into(), Action::Add)], width);
        rows.push(Row::Gap);
        if state.agents.is_empty() {
            rows.push(Row::Text("No agents configured yet".into(), true));
            note(
                &mut rows,
                "Add an agent, then select it in Agent Chat to connect.",
                width,
            );
        }
        for (index, agent) in state.agents.iter().enumerate() {
            rows.push(Row::Text(agent.name.clone(), true));
            note(&mut rows, &agent.command, width);
            if state.remove == Some(index) {
                note(&mut rows, "Remove this agent from these settings?", width);
                buttons(
                    &mut rows,
                    vec![
                        ("Remove Agent".into(), Action::ConfirmRemove(index)),
                        ("Keep Agent".into(), Action::Keep),
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

fn footer(state: &AgentSettingsState, width: usize) -> Vec<Row> {
    let mut rows = Vec::new();
    let choices = if state.draft.is_some() {
        vec![
            ("Save Agent".into(), Action::Save),
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
                && app.agent_settings.draft.is_some() =>
        {
            activate(app, Action::Save);
        }
        KeyCode::Esc => {
            let action = if app.agent_settings.draft.is_some() {
                Action::Cancel
            } else if app.agent_settings.remove.is_some() {
                Action::Keep
            } else {
                Action::Close
            };
            activate(app, action);
        }
        KeyCode::Tab | KeyCode::BackTab => {
            let state = &app.agent_settings;
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
                app.agent_settings.focus =
                    all[(index + if reverse { all.len() - 1 } else { 1 }) % all.len()];
                app.agent_settings.reveal_focus = true;
            }
        }
        KeyCode::PageUp | KeyCode::PageDown => {
            let state = &mut app.agent_settings;
            let page = usize::from(state.body.height).max(1);
            state.scroll = if key.code == KeyCode::PageUp {
                state.scroll.saturating_sub(page)
            } else {
                (state.scroll + page).min(state.max_scroll)
            };
            state.reveal_focus = false;
        }
        KeyCode::Enter | KeyCode::Char(' ')
            if !matches!(app.agent_settings.focus, Action::Field(_)) =>
        {
            activate(app, app.agent_settings.focus);
        }
        _ => {
            if let Some(id) = app.agent_settings.focused_input()
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
    let hit = app.agent_settings.hits.iter().any(|(area, action)| {
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
                .agent_settings
                .hits
                .iter()
                .find(|(area, _)| area.contains((mouse.column, mouse.row).into()))
                .copied()
            {
                activate(app, action);
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let state = &mut app.agent_settings;
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
    let focused = action == app.agent_settings.focus;
    let hovered = area.contains((app.mouse_col, app.mouse_row).into());
    let bg = crate::ui_widgets::surface_background(f, area, app.colors.ui.bg_widget);
    let mut style = app.colors.ui.button_style(bg,
        matches!(action, Action::Save | Action::Add), hovered, true).add_modifier(Modifier::BOLD);
    if focused && let Some(border) = app.colors.ui.focus_border.border(style.bg.unwrap_or(bg)) {
        style = style.add_modifier(Modifier::UNDERLINED).underline_color(border);
    }
    f.render_widget(Paragraph::new(format!(" {label} ")).style(style), area);
    app.agent_settings.hits.push((area, action));
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
            let focused = app.agent_settings.focus == Action::Field(*id);
            let field = &app.agent_settings.draft.as_ref().unwrap().fields[*id];
            let input_style = crate::ui_settings::settings_control_style(
                &app.colors, "text", bg, focused,
            );
            let input_bg = input_style.bg.unwrap_or(bg);
            let input = InputField::new(&field.text)
                .theme_control(&app.colors, app.colors.ui.settings_control("text"), bg)
                .prefix(" ", app.colors.ui.description_color(input_bg))
                .bg(input_bg)
                .placeholder_text(placeholder)
                .cursor_marker(if focused { "█" } else { "" })
                .cursor_pos(field.cursor)
                .selection_range(
                    App::inline_selection_range(field.cursor, field.anchor),
                );
            let hit = input.render(f, input_area);
            crate::ui_settings::underline_field(f, input_area, input_style);
            app.register_text_input(TextInputId::AgentSetting(*id), hit);
            app.agent_settings
                .hits
                .push((input_area, Action::Field(*id)));
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
    app.agent_settings.hits.clear();
    app.remove_text_input_regions(|id| matches!(id, TextInputId::AgentSetting(_)));
    let viewport = f.area();
    let width = viewport
        .width
        .saturating_sub(u16::from(viewport.width >= 50) * 4)
        .min(88);
    let content_width = width.saturating_sub(5) as usize;
    let desired_height = rows(&app.agent_settings, content_width).len()
        + footer(&app.agent_settings, content_width).len()
        + 6
        + App::wrap_setting_text_for_width(
            app.agent_settings
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
            .style(
                Style::default()
                    .fg(fg)
                    .bg(bg),
            ),
        area,
    );
    if width < 12 || height < 8 {
        return;
    }
    let inner = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    let scope = if app.agent_settings.scope == SettingsScope::Global {
        "User"
    } else {
        "Workspace"
    };
    let title = if let Some(draft) = &app.agent_settings.draft {
        if draft.index.is_some() {
            "Edit Agent"
        } else {
            "Add Agent"
        }
    } else {
        "Agent Chat"
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
    let footer_rows = footer(&app.agent_settings, inner.width as usize);
    let notices = App::wrap_setting_text_for_width(
        app.agent_settings
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
    let content = rows(&app.agent_settings, body.width as usize);
    let state = &mut app.agent_settings;
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
    if app.agent_settings.max_scroll > 0 && body.height > 0 {
        let thumb = body.y
            + (usize::from(body.height - 1) * app.agent_settings.scroll
                / app.agent_settings.max_scroll) as u16;
        let (track, slider) = app.colors.ui.scrollbar_colors(bg, false, false);
        f.render_widget(
            Paragraph::new("▐").style(Style::default().fg(slider).bg(track)),
            Rect::new(inner.right() - 1, thumb, 1, 1),
        );
    }
    for (i, line) in notices.iter().take(notice_height as usize).enumerate() {
        f.render_widget(
            Paragraph::new(line.as_str()).style(Style::default().fg(
                if app.agent_settings.error.is_some() {
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
    use ratatui::style::Color;
    use ratatui::{Terminal, backend::TestBackend};
    use std::{
        path::Path,
        time::{Duration, Instant},
    };

    fn render(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
        terminal
    }

    #[test]
    fn agent_controls_use_button_roles_instead_of_list_or_link_colors() {
        let mut app = App::test_new();
        app.colors = crate::ui_control_visibility_tests::theme_with_popup_and_control_roles().to_theme_colors();
        let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
        let area = Rect::new(2, 2, 20, 1);
        for (action, label, normal_bg, hover_bg, foreground) in [
            (Action::Add, "Add agent", Color::Rgb(32, 59, 112), Color::Rgb(50, 75, 128), Color::Rgb(255, 244, 187)),
            (Action::Cancel, "Cancel", Color::Rgb(48, 53, 32), Color::Rgb(72, 73, 42), Color::Rgb(221, 238, 255)),
        ] {
            for (focused, hovered) in [(false, false), (true, false), (false, true)] {
                app.agent_settings.focus = if focused { action } else { Action::Close };
                app.mouse_col = if hovered { area.x + 1 } else { 0 };
                app.mouse_row = if hovered { area.y } else { 0 };
                terminal.draw(|frame| button(frame, &mut app, area, label, action)).unwrap();
                let cell = &terminal.backend().buffer()[(area.x + 1, area.y)];
                assert_eq!(cell.bg, if hovered { hover_bg } else { normal_bg }, "{label}, focused={focused}");
                assert_eq!(cell.fg, foreground);
            }
        }
    }

    fn mouse(app: &mut App, column: u16, row: u16) {
        crate::mouse::handle_mouse(
            app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
        );
    }

    fn click(app: &mut App, action: Action) {
        app.agent_settings.focus = action;
        app.agent_settings.reveal_focus = true;
        render(app, 120, 42);
        let area = app
            .agent_settings
            .hits
            .iter()
            .find(|(_, candidate)| *candidate == action)
            .unwrap()
            .0;
        mouse(app, area.x + 1, area.y);
    }

    fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
    }

    fn fill(app: &mut App, id: usize, text: &str) {
        click(app, Action::Field(id));
        key(app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        assert!(app.paste_into_focused_text_input(text));
        assert_eq!(
            app.agent_settings.draft.as_ref().unwrap().fields[id].text,
            text
        );
    }

    fn argument(app: &mut App, text: &str) {
        click(app, Action::AddArgument);
        let id = *app
            .agent_settings
            .draft
            .as_ref()
            .unwrap()
            .arguments
            .last()
            .unwrap();
        fill(app, id, text);
    }

    fn environment(app: &mut App, name: &str, value: &str) {
        click(app, Action::AddEnvironment);
        let (key, id) = *app
            .agent_settings
            .draft
            .as_ref()
            .unwrap()
            .environment
            .last()
            .unwrap();
        fill(app, key, name);
        fill(app, id, value);
    }

    fn app_at(root: &Path) -> App {
        let mut app = App::test_new();
        app.explorer.root = root.to_owned();
        app.settings_global_path = root.join("user.json");
        app.settings_workspace_path = root.join("workspace.json");
        app.settings_persistence_enabled = true;
        app.settings_global = crate::settings::load_from_file(&app.settings_global_path);
        app.settings_workspace = crate::settings::load_from_file(&app.settings_workspace_path);
        app.apply_effective_settings();
        app
    }

    fn capture(app: &mut App, name: &str, width: u16, height: u16) {
        let Some(folder) = std::env::var_os("VTCODE_AGENT_SETTINGS_CAPTURE") else {
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
            folder.join(format!("chat-agent-settings-{name}.json")),
            serde_json::to_vec(&serde_json::json!({"width":width,"height":height,"cells":cells}))
                .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn agent_settings_form_round_trip_preserves_scope_and_literal_arguments() {
        let root = tempfile::tempdir().unwrap();
        let global = serde_json::json!({"tabSize":7,"future.setting":{"enabled":true},"acpAgents":[AgentConfig::new("Original agent", "original")]});
        let original = serde_json::to_string(&global).unwrap();
        std::fs::write(root.path().join("user.json"), &original).unwrap();
        std::fs::write(
            root.path().join("workspace.json"),
            r#"{"wordWrap":true,"future.workspace":42}"#,
        )
        .unwrap();
        let mut app = app_at(root.path());
        app.execute_palette_command("settings");
        app.settings_scope = SettingsScope::Workspace;
        app.settings_category_selected = 6;
        app.ensure_settings_selection_visible();
        let terminal = render(&mut app, 120, 42);
        let buffer = terminal.backend().buffer();
        let position = (0..42)
            .find_map(|y| {
                (0..100).find_map(|x| {
                    let text: String = (x..x + 13)
                        .map(|col| buffer.cell((col, y)).unwrap().symbol())
                        .collect();
                    (text == "Manage Agents").then_some((x, y))
                })
            })
            .expect("Discoverable configuration button in Settings");
        capture(&mut app, "page", 120, 42);
        mouse(&mut app, position.0, position.1);
        assert!(app.agent_settings.active && app.agent_settings.inherited);
        capture(&mut app, "list", 120, 42);
        click(&mut app, Action::Add);
        fill(&mut app, 0, "Local helper");
        fill(&mut app, 1, r"C:\Program Files\Agent\agent.exe");
        argument(&mut app, "--stdio");
        argument(&mut app, "folder with spaces/世界.py");
        argument(&mut app, "");
        environment(&mut app, "GREETING", "  Hello 🌍  ");
        fill(&mut app, 2, "120");
        app.agent_settings.scroll = 0;
        app.agent_settings.focus = Action::Field(0);
        capture(&mut app, "form-dark", 120, 42);
        app.set_theme_by_key("light");
        capture(&mut app, "form-light", 120, 42);
        app.set_theme_by_key("dark");
        click(&mut app, Action::Save);
        assert!(
            app.agent_settings.draft.is_none(),
            "{:?}",
            app.agent_settings.error
        );
        let saved = crate::settings::load_from_file(&app.settings_workspace_path);
        let agents = saved.acp_agents.unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[1].command, r"C:\Program Files\Agent\agent.exe");
        assert_eq!(
            agents[1].args,
            ["--stdio", "folder with spaces/世界.py", ""]
        );
        assert_eq!(agents[1].env["GREETING"], "  Hello 🌍  ");
        assert_eq!(agents[1].request_timeout_secs, 120);
        assert_eq!(
            std::fs::read_to_string(&app.settings_global_path).unwrap(),
            original
        );
        let document: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&app.settings_workspace_path).unwrap())
                .unwrap();
        assert_eq!(document["future.workspace"], 42);
        assert_eq!(document["wordWrap"], true);
        assert_eq!(app.settings_effective.acp_agents.as_ref().unwrap(), &agents);

        click(&mut app, Action::Edit(1));
        fill(&mut app, 0, "Cancelled change");
        click(&mut app, Action::Cancel);
        assert_eq!(app.agent_settings.agents, agents);
        click(&mut app, Action::Edit(1));
        fill(&mut app, 0, "Renamed helper");
        click(&mut app, Action::Save);
        assert_eq!(app.agent_settings.agents[1].name, "Renamed helper");
        assert_eq!(app.agent_settings.agents[1].args, agents[1].args);
        click(&mut app, Action::Remove(0));
        click(&mut app, Action::Keep);
        assert_eq!(app.agent_settings.agents.len(), 2);
        for _ in 0..2 {
            click(&mut app, Action::Remove(0));
            click(&mut app, Action::ConfirmRemove(0));
        }
        assert_eq!(
            app.settings_effective.acp_agents,
            Some(Vec::new()),
            "An empty Workspace list must override User agents"
        );
        click(&mut app, Action::Close);
        assert!(app.reset_setting_in_selected_scope("acp_agents"));
        assert_eq!(
            app.settings_effective.acp_agents.as_ref().unwrap()[0].name,
            "Original agent"
        );
    }

    #[test]
    fn agent_settings_validation_and_failed_save_preserve_drafts() {
        let root = tempfile::tempdir().unwrap();
        let mut app = app_at(root.path());
        app.open_agent_settings_page();
        click(&mut app, Action::Add);
        click(&mut app, Action::Save);
        assert!(app.agent_settings.error.as_ref().unwrap().contains("name"));
        fill(&mut app, 0, "Test agent");
        click(&mut app, Action::Save);
        assert!(
            app.agent_settings
                .error
                .as_ref()
                .unwrap()
                .contains("executable")
        );
        fill(&mut app, 1, "agent");
        fill(&mut app, 2, "0");
        click(&mut app, Action::Save);
        assert!(
            app.agent_settings
                .error
                .as_ref()
                .unwrap()
                .contains("timeout")
        );
        fill(&mut app, 2, "60");
        environment(&mut app, "NAME", "one");
        environment(&mut app, "NAME", "two");
        click(&mut app, Action::Save);
        assert!(
            app.agent_settings
                .error
                .as_ref()
                .unwrap()
                .contains("more than once")
        );
        let last = app.agent_settings.draft.as_ref().unwrap().environment[1].0;
        click(&mut app, Action::RemoveEnvironment(last));
        std::fs::write(&app.settings_global_path, "{invalid JSON").unwrap();
        click(&mut app, Action::Save);
        assert!(
            app.agent_settings
                .error
                .as_ref()
                .unwrap()
                .contains("Could not save")
        );
        assert!(app.agent_settings.draft.is_some());
        assert_eq!(app.agent_settings.agents.len(), 0);
        assert_eq!(
            std::fs::read_to_string(&app.settings_global_path).unwrap(),
            "{invalid JSON"
        );
        std::fs::write(&app.settings_global_path, "{}").unwrap();
        click(&mut app, Action::Save);
        assert!(app.agent_settings.draft.is_none());
        click(&mut app, Action::Add);
        fill(&mut app, 0, "Test agent");
        fill(&mut app, 1, "second-command");
        click(&mut app, Action::Save);
        assert!(
            app.agent_settings
                .error
                .as_ref()
                .unwrap()
                .contains("already exists")
        );
    }

    #[test]
    fn agent_settings_keyboard_focus_and_narrow_layouts() {
        let mut app = App::test_new();
        app.execute_palette_command("acp_configure");
        assert!(app.settings_editor_active && app.agent_settings.active);
        click(&mut app, Action::Add);
        assert_eq!(app.agent_settings.focus, Action::Field(0));
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.agent_settings.focus, Action::Field(1));
        key(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(app.agent_settings.focus, Action::Field(0));
        fill(&mut app, 0, "An agent");
        fill(&mut app, 1, "agent");
        for i in 0..5 {
            argument(&mut app, &format!("argument {i}"));
        }
        environment(&mut app, "NAME", "value");
        for (width, height) in [(28, 14), (40, 20), (80, 24), (140, 44)] {
            app.agent_settings.focus = Action::Field(0);
            app.agent_settings.reveal_focus = true;
            for _ in 0..30 {
                let terminal = render(&mut app, width, height);
                let bounds = terminal.backend().buffer().area;
                for (area, _) in &app.agent_settings.hits {
                    assert_eq!(area.intersection(bounds), *area);
                }
                if matches!(app.agent_settings.focus, Action::Field(_)) {
                    assert!(
                        app.agent_settings
                            .hits
                            .iter()
                            .any(|(_, action)| *action == app.agent_settings.focus),
                        "Tab must reveal its focused field"
                    );
                }
                if app.agent_settings.focus == Action::Save {
                    break;
                }
                key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            }
            assert_eq!(app.agent_settings.focus, Action::Save);
        }
        capture(&mut app, "narrow", 40, 20);
        let field_count = app.agent_settings.draft.as_ref().unwrap().fields.len();
        click(&mut app, Action::RemoveArgument(3));
        click(&mut app, Action::AddArgument);
        assert_eq!(app.agent_settings.focus, Action::Field(field_count));
        fill(&mut app, field_count, "new argument");
        key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert!(
            app.agent_settings.draft.as_ref().unwrap().fields[field_count]
                .text
                .is_empty(),
            "Undo must not resurrect a removed row"
        );
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.agent_settings.active && app.agent_settings.draft.is_none());
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.agent_settings.active);
    }

    #[test]
    #[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
    fn agent_settings_official_echo_connects_from_saved_form() {
        use crate::app_acp::ChatAction;
        let root = tempfile::tempdir().unwrap();
        let mut app = app_at(root.path());
        app.open_agent_settings_page();
        click(&mut app, Action::Add);
        let project = Path::new(env!("CARGO_MANIFEST_DIR"));
        let python = std::env::var_os("VTCODE_ACP_TEST_PYTHON")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                project.join(if cfg!(windows) {
                    "target/acp-python/Scripts/python.exe"
                } else {
                    "target/acp-python/bin/python"
                })
            });
        assert!(
            python.is_file(),
            "Install tests/fixtures/requirements.txt and set VTCODE_ACP_TEST_PYTHON"
        );
        fill(&mut app, 0, "Official SDK Echo");
        fill(&mut app, 1, &python.to_string_lossy());
        argument(&mut app, "-u");
        argument(
            &mut app,
            &project
                .join("tests/fixtures/acp-python-sdk/echo_agent.py")
                .to_string_lossy(),
        );
        environment(&mut app, "PYTHONUTF8", "1");
        environment(&mut app, "PYTHONIOENCODING", "utf-8");
        click(&mut app, Action::Save);
        assert!(
            app.agent_settings.error.is_none(),
            "{:?}",
            app.agent_settings.error
        );
        click(&mut app, Action::Close);
        app.settings_global = crate::settings::load_from_file(&app.settings_global_path);
        app.apply_effective_settings();
        app.chat_action(ChatAction::Connect(0));
        let deadline = Instant::now() + Duration::from_secs(15);
        while app.chat.session().is_none() {
            app.poll_acp();
            assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        app.chat.input = "Configured using the form · 世界 👋".into();
        app.chat.cursor = app.chat.input.len();
        app.chat_action(ChatAction::Send);
        while app.chat.busy() {
            app.poll_acp();
            assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(app.chat.session().unwrap().entries.iter().any(|entry| matches!(entry, vtcode::acp::ChatEntry::Agent(text) if text == "Configured using the form · 世界 👋")));
        app.chat_action(ChatAction::Disconnect);
    }
}
