//! First-use formatter choice and the same editable form in Settings.
use crate::{
    app::App,
    app_formatters::Request,
    formatters::{CommandMode, Config, Provider},
    settings::SettingsScope,
    text_input::TextInputId,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Action {
    #[default]
    Save,
    Close,
    Reset,
    Scope(SettingsScope),
    ChooseLanguage,
    ChooseProvider,
    ChooseMode,
    Language(usize),
    Provider(Provider),
    Mode(CommandMode),
    Arguments,
    Field(usize),
    AddArgument,
    RemoveArgument(usize),
}

#[cfg(test)]
#[path = "ui_formatter_settings_tests.rs"]
mod tests;

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

#[derive(Default)]
pub(crate) struct State {
    pub active: bool,
    pub fields: Vec<Field>,
    pub focus: Action,
    pub error: Option<String>,
    pub reveal_focus: bool,
    pub provider: Provider,
    mode: CommandMode,
    scope: SettingsScope,
    request: Option<Request>,
    arguments: Vec<usize>,
    known: Vec<String>,
    hits: Vec<(Rect, Action)>,
    scroll: usize,
    max_scroll: usize,
    body: Rect,
    popup: Rect,
    menu: Option<Menu>,
    menu_index: usize,
    arguments_open: bool,
    loaded_language: String,
    drafts: Vec<(SettingsScope, String, Config)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Menu {
    Language,
    Provider,
    Mode,
}
impl State {
    pub(crate) fn focused_input(&self) -> Option<TextInputId> {
        if let Action::Field(id) = self.focus
            && self.field_visible(id)
        {
            self.fields
                .get(id)
                .map(|_| TextInputId::FormatterSetting(id))
        } else {
            None
        }
    }
    fn field_visible(&self, id: usize) -> bool {
        if id == 0 {
            self.request.is_none()
        } else {
            self.provider == Provider::Command
                && (id == 1 || (self.arguments_open && self.arguments.contains(&id)))
        }
    }
    fn language(&self) -> String {
        crate::file_associations::language_id(&self.fields[0].text)
    }
    fn config(&self) -> Config {
        let external = self.provider == Provider::Command;
        Config {
            provider: self.provider,
            mode: if external { self.mode } else { CommandMode::default() },
            command: if external {
                self.fields[1].text.trim().into()
            } else {
                String::new()
            },
            args: if external {
                self.arguments
                    .iter()
                    .map(|&id| self.fields[id].text.clone())
                    .collect()
            } else {
                Vec::new()
            },
        }
    }
    fn load(&mut self, language: String, config: Config) {
        self.provider = config.provider;
        self.mode = config.mode;
        self.loaded_language = language.clone();
        self.arguments_open = !config.args.is_empty();
        self.menu = None;
        self.fields = vec![Field::new(language), Field::new(config.command)];
        self.arguments.clear();
        for arg in config.args {
            self.arguments.push(self.fields.len());
            self.fields.push(Field::new(arg));
        }
        self.focus = Action::ChooseProvider;
        self.scroll = 0;
        self.reveal_focus = true;
    }

    fn remember_draft(&mut self) {
        let config = self.config();
        self.drafts.retain(|(scope, language, draft)| {
            *scope != self.scope
                || *language != self.loaded_language
                || draft.provider != self.provider
        });
        self.drafts
            .push((self.scope, self.loaded_language.clone(), config));
    }
}

fn load_language(app: &mut App, language: String) {
    let state = &app.formatter_settings;
    let config = state
        .drafts
        .iter()
        .rev()
        .find(|(scope, name, _)| *scope == state.scope && *name == language)
        .map(|(_, _, config)| config.clone())
        .or_else(|| {
            app.settings_for_scope(state.scope)
                .formatters?
                .get(&language)
                .cloned()
        })
        .unwrap_or_default();
    clear_inputs(app);
    app.formatter_settings.load(language, config);
}

// Commit the language field before using the rest of the form: a typed language
// must not accidentally inherit the previous language's command or arguments.
fn sync_language(app: &mut App) -> bool {
    let language = app.formatter_settings.language();
    if let Err(error) = crate::formatters::validate(&language, &Config::default()) {
        app.formatter_settings.error = Some(error);
        app.formatter_settings.focus = Action::Field(0);
        app.formatter_settings.reveal_focus = true;
        return false;
    }
    if language != app.formatter_settings.loaded_language {
        app.formatter_settings.remember_draft();
        load_language(app, language);
    }
    true
}

fn local_config(app: &App) -> Option<&Config> {
    let state = &app.formatter_settings;
    let settings = match state.scope {
        SettingsScope::Global => &app.settings_global,
        SettingsScope::Workspace => &app.settings_workspace,
    };
    settings.formatters.as_ref()?.get(&state.language())
}

fn clear_inputs(app: &mut App) {
    app.text_input_histories
        .retain(|id, _| !matches!(id, TextInputId::FormatterSetting(_)));
    app.remove_text_input_regions(|id| matches!(id, TextInputId::FormatterSetting(_)));
    app.text_input_drag = None;
    app.text_input_click_id = None;
    app.text_input_drag_unit = None;
}

impl App {
    pub(crate) fn open_formatter_settings(&mut self, request: Option<Request>) {
        self.flush_live_settings();
        self.finish_numeric_setting_edit();
        self.close_settings_dropdown();
        self.clear_drag_states();
        self.menu_active = false;
        self.menu_open = false;
        self.palette.active = false;
        self.terminal_panel_focused = false;
        self.formatting.error = None;
        self.dismiss_lsp_hover();
        let language = self
            .editor()
            .filter(|_| !self.settings_editor_active)
            .map(crate::formatters::language)
            .unwrap_or_else(|| "python".into());
        let scope = if self.settings_editor_active {
            self.settings_scope
        } else if self
            .settings_workspace
            .formatters
            .as_ref()
            .is_some_and(|map| map.contains_key(&language))
        {
            SettingsScope::Workspace
        } else {
            SettingsScope::Global
        };
        let config = self
            .settings_for_scope(scope)
            .formatters
            .unwrap_or_default()
            .get(&language)
            .cloned()
            .unwrap_or_default();
        let mut known: Vec<_> = self
            .highlighter
            .syntax_names()
            .iter()
            .map(|name| crate::file_associations::language_id(name))
            .collect();
        known.extend(self.formatters.keys().cloned());
        known.push(language.clone());
        known.sort();
        known.dedup();
        clear_inputs(self);
        self.formatter_settings = State {
            active: true,
            scope,
            request,
            known,
            ..Default::default()
        };
        self.formatter_settings.load(language, config);
    }
}

pub(crate) fn activate(app: &mut App, action: Action) {
    app.formatter_settings.error = None;
    let language_changed =
        app.formatter_settings.language() != app.formatter_settings.loaded_language;
    if !matches!(
        action,
        Action::Close | Action::Field(0) | Action::ChooseLanguage | Action::Language(_)
    ) && !sync_language(app)
    {
        return;
    }
    if language_changed
        && matches!(
            action,
            Action::Field(1..)
                | Action::Arguments
                | Action::AddArgument
                | Action::RemoveArgument(_)
                | Action::ChooseMode
                | Action::Mode(_)
        )
    {
        // This action belonged to the old language's controls; the newly loaded
        // language may have another provider or a different argument list.
        app.formatter_settings.focus = Action::ChooseProvider;
        return;
    }
    app.formatter_settings.reveal_focus = true;
    if !matches!(action, Action::ChooseLanguage | Action::ChooseProvider | Action::ChooseMode) {
        app.formatter_settings.menu = None;
    }
    match action {
        Action::Save | Action::Reset => {
            let state = &app.formatter_settings;
            let language = state.language();
            let config = (action == Action::Save).then(|| state.config());
            if let Err(error) = app.save_formatter(state.scope, &language, config) {
                app.formatter_settings.error = Some(error);
                if app.formatter_settings.provider == Provider::Command
                    && app.formatter_settings.fields[1].text.trim().is_empty()
                {
                    app.formatter_settings.focus = Action::Field(1);
                } else if app.formatter_settings.provider == Provider::Command {
                    app.formatter_settings.arguments_open = true;
                    app.formatter_settings.focus = Action::Arguments;
                }
                return;
            }
            let request = app.formatter_settings.request.take();
            clear_inputs(app);
            app.formatter_settings = State::default();
            app.modal = Default::default();
            if let Some(request) = request {
                if app.editor().is_some_and(|editor| {
                    editor.navigation_id == request.document
                        && editor.content_version == request.version
                        && editor.file_path.as_ref() == Some(&request.path)
                }) {
                    if !app.request_format(request.selection, request.save_after)
                        && request.save_after
                    {
                        if let Err(error) = app.save_open_file_by_path(&request.path) {
                            app.show_formatting_error(error.to_string());
                        }
                    }
                } else {
                    app.show_formatting_error("Formatter saved. The document changed while choosing it; run Format Document again.");
                }
            } else {
                app.notify("Formatter settings saved");
            }
        }
        Action::Close => {
            let request = app.formatter_settings.request.take();
            clear_inputs(app);
            app.formatter_settings = State::default();
            app.modal = Default::default();
            if let Some(request) = request.filter(|request| request.save_after) {
                if let Err(error) = app.save_open_file_by_path(&request.path) {
                    app.show_formatting_error(error.to_string());
                }
            }
        }
        Action::Scope(scope) => {
            if app.formatter_settings.scope != scope {
                let language = app.formatter_settings.language();
                app.formatter_settings.remember_draft();
                app.formatter_settings.scope = scope;
                load_language(app, language);
            }
            app.formatter_settings.focus = action;
        }
        Action::ChooseLanguage | Action::ChooseProvider | Action::ChooseMode => {
            let state = &mut app.formatter_settings;
            let menu = match action {
                Action::ChooseLanguage => Menu::Language,
                Action::ChooseMode => Menu::Mode,
                _ => Menu::Provider,
            };
            if menu == Menu::Language && state.request.is_some() {
                return;
            }
            state.menu = if state.menu == Some(menu) {
                None
            } else {
                Some(menu)
            };
            state.menu_index = match menu {
                Menu::Language => language_matches(state)
                    .iter()
                    .position(|&index| state.known[index] == state.loaded_language)
                    .unwrap_or(0),
                Menu::Provider => providers()
                    .iter()
                    .position(|&p| p == state.provider)
                    .unwrap_or(0),
                Menu::Mode => modes().iter().position(|&mode| mode == state.mode).unwrap_or(0),
            };
            state.focus = if menu == Menu::Language {
                Action::Field(0)
            } else {
                action
            };
            if menu == Menu::Language {
                state.fields[0].anchor = Some(0);
                state.fields[0].cursor = state.fields[0].text.len();
            }
        }
        Action::Language(index) => {
            if app.formatter_settings.request.is_none()
                && let Some(language) = app.formatter_settings.known.get(index).cloned()
            {
                app.formatter_settings.remember_draft();
                load_language(app, language);
            }
        }
        Action::Provider(provider) => {
            if provider != app.formatter_settings.provider {
                let language = app.formatter_settings.language();
                app.formatter_settings.remember_draft();
                let state = &app.formatter_settings;
                let config = state
                    .drafts
                    .iter()
                    .rev()
                    .find(|(scope, name, config)| {
                        *scope == state.scope && *name == language && config.provider == provider
                    })
                    .map(|(_, _, config)| config.clone())
                    .unwrap_or(Config {
                        provider,
                        args: if provider == Provider::Command { vec!["{file}".into()] } else { Vec::new() },
                        ..Default::default()
                    });
                clear_inputs(app);
                app.formatter_settings.load(language, config);
            }
            app.formatter_settings.focus = Action::ChooseProvider;
        }
        Action::Mode(mode) => {
            // Arguments belong to the command. Changing how its output is read
            // must not silently remove flags or append undocumented arguments.
            app.formatter_settings.mode = mode;
            app.formatter_settings.focus = Action::ChooseMode;
        }
        Action::Arguments => {
            app.formatter_settings.arguments_open = !app.formatter_settings.arguments_open;
            app.formatter_settings.focus = action;
        }
        Action::Field(id) => {
            app.formatter_settings.focus = if app.formatter_settings.field_visible(id) {
                action
            } else {
                Action::ChooseProvider
            };
        }
        Action::AddArgument => {
            let state = &mut app.formatter_settings;
            let id = state.fields.len();
            state.fields.push(Field::new(String::new()));
            state.arguments.push(id);
            state.arguments_open = true;
            state.focus = Action::Field(id);
            state.reveal_focus = true;
        }
        Action::RemoveArgument(id) => {
            app.formatter_settings
                .arguments
                .retain(|&value| value != id);
            app.formatter_settings.focus = Action::AddArgument;
            clear_inputs(app);
        }
    }
    app.formatter_settings.hits.clear();
}

fn providers() -> [Provider; 3] {
    [Provider::Lsp, Provider::Command, Provider::Disabled]
}

fn modes() -> [CommandMode; 3] {
    [CommandMode::File, CommandMode::Stdin, CommandMode::FileStdout]
}

fn language_matches(state: &State) -> Vec<usize> {
    let query = state.language();
    state
        .known
        .iter()
        .enumerate()
        .filter_map(|(index, language)| {
            (query == state.loaded_language || language.contains(&query)).then_some(index)
        })
        .collect()
}

fn menu_actions(state: &State) -> Vec<Action> {
    match state.menu {
        Some(Menu::Language) => language_matches(state)
            .into_iter()
            .map(Action::Language)
            .collect(),
        Some(Menu::Provider) => providers().into_iter().map(Action::Provider).collect(),
        Some(Menu::Mode) => modes().into_iter().map(Action::Mode).collect(),
        None => Vec::new(),
    }
}

fn focus_order(app: &App) -> Vec<Action> {
    let state = &app.formatter_settings;
    let mut actions = vec![
        Action::Scope(SettingsScope::Global),
        Action::Scope(SettingsScope::Workspace),
    ];
    if state.request.is_none() {
        actions.extend([Action::Field(0), Action::ChooseLanguage]);
    }
    actions.push(Action::ChooseProvider);
    if state.provider == Provider::Command {
        actions.extend([Action::Field(1), Action::ChooseMode, Action::Arguments]);
        if state.arguments_open {
            for &id in &state.arguments {
                actions.extend([Action::Field(id), Action::RemoveArgument(id)]);
            }
            actions.push(Action::AddArgument);
        }
    }
    if state.request.is_none() && local_config(app).is_some() {
        actions.push(Action::Reset);
    }
    actions.extend([Action::Save, Action::Close]);
    actions
}

pub(crate) fn input_changed(app: &mut App) {
    let state = &mut app.formatter_settings;
    state.error = None;
    state.reveal_focus = true;
    if state.focus == Action::Field(0) {
        state.menu_index = 0;
    }
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    if app.formatter_settings.menu.is_some() {
        let choices = menu_actions(&app.formatter_settings);
        match key.code {
            KeyCode::Esc => {
                app.formatter_settings.menu = None;
                return;
            }
            KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End => {
                let state = &mut app.formatter_settings;
                let last = choices.len().saturating_sub(1);
                state.menu_index = match key.code {
                    KeyCode::Home => 0,
                    KeyCode::End => last,
                    KeyCode::Up => state.menu_index.saturating_sub(1).min(last),
                    _ => (state.menu_index + 1).min(last),
                };
                return;
            }
            KeyCode::Enter => {
                if let Some(&action) = choices.get(app.formatter_settings.menu_index) {
                    activate(app, action);
                    return;
                }
                app.formatter_settings.menu = None;
            }
            KeyCode::Tab | KeyCode::BackTab => app.formatter_settings.menu = None,
            _ => {}
        }
    } else if matches!(key.code, KeyCode::Up | KeyCode::Down)
        && matches!(
            app.formatter_settings.focus,
            Action::ChooseProvider | Action::ChooseMode | Action::ChooseLanguage | Action::Field(0)
        )
    {
        let action = match app.formatter_settings.focus {
            Action::ChooseProvider => Action::ChooseProvider,
            Action::ChooseMode => Action::ChooseMode,
            _ => Action::ChooseLanguage,
        };
        activate(app, action);
        return;
    }
    match key.code {
        KeyCode::Esc => activate(app, Action::Close),
        KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
            let focus = app.formatter_settings.focus;
            if focus == Action::Field(0) && !sync_language(app) {
                return;
            }
            let all = focus_order(app);
            let current = all.iter().position(|&action| action == focus).unwrap_or(0);
            let reverse = matches!(key.code, KeyCode::BackTab | KeyCode::Up)
                || key.modifiers.contains(KeyModifiers::SHIFT);
            app.formatter_settings.focus =
                all[(current + if reverse { all.len() - 1 } else { 1 }) % all.len()];
            app.formatter_settings.reveal_focus = true;
        }
        KeyCode::Enter if app.formatter_settings.focus == Action::Field(0) => {
            if sync_language(app) {
                app.formatter_settings.focus = Action::ChooseProvider;
            }
        }
        KeyCode::Enter => {
            let action = app.formatter_settings.focus;
            activate(
                app,
                if matches!(action, Action::Field(_)) {
                    Action::Save
                } else {
                    action
                },
            );
        }
        KeyCode::Char(' ') if app.formatter_settings.focused_input().is_none() => {
            activate(app, app.formatter_settings.focus)
        }
        _ => {
            if let Some(id) = app.formatter_settings.focused_input()
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
    if app.formatter_settings.menu.is_some() {
        if let MouseEventKind::ScrollUp | MouseEventKind::ScrollDown = mouse.kind {
            let count = menu_actions(&app.formatter_settings).len();
            let state = &mut app.formatter_settings;
            state.menu_index = if mouse.kind == MouseEventKind::ScrollUp {
                state.menu_index.saturating_sub(1)
            } else {
                (state.menu_index + 1).min(count.saturating_sub(1))
            };
            return;
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            let action = app
                .formatter_settings
                .hits
                .iter()
                .find(|(area, _)| area.contains((mouse.column, mouse.row).into()))
                .map(|(_, action)| *action);
            match action {
                Some(action @ (Action::Language(_) | Action::Provider(_) | Action::Mode(_))) => {
                    activate(app, action);
                    return;
                }
                Some(Action::Field(0)) if app.formatter_settings.menu == Some(Menu::Language) => {}
                _ => {
                    app.formatter_settings.menu = None;
                    return;
                }
            }
        }
    }
    let field = app
        .formatter_settings
        .hits
        .iter()
        .find_map(|(area, action)| {
            if let Action::Field(id) = action
                && area.contains((mouse.column, mouse.row).into())
            {
                Some(*id)
            } else {
                None
            }
        });
    if field.is_some_and(|id| id != 0) && mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        let changed = app.formatter_settings.language() != app.formatter_settings.loaded_language;
        if !sync_language(app) || changed {
            return;
        }
    }
    if (field.is_some() || matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_)))
        && app.handle_overlay_text_input_mouse(mouse.kind, mouse.column, mouse.row, mouse.modifiers)
    {
        return;
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some((_, action)) = app
                .formatter_settings
                .hits
                .iter()
                .find(|(area, _)| area.contains((mouse.column, mouse.row).into()))
                .copied()
            {
                activate(app, action);
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let state = &mut app.formatter_settings;
            if state.body.contains((mouse.column, mouse.row).into()) {
                state.scroll = if mouse.kind == MouseEventKind::ScrollUp {
                    state.scroll.saturating_sub(3)
                } else {
                    (state.scroll + 3).min(state.max_scroll)
                };
                state.reveal_focus = false;
            }
        }
        _ => {}
    }
}

#[path = "ui_formatter_settings_view.rs"]
mod view;

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    view::draw(f, app);
}
