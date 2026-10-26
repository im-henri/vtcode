//! Persistent file types, reachable from the language selector and Settings.
use crate::{app::App, settings::SettingsScope, text_input::TextInputId};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Frame, layout::Rect, style::Modifier, widgets::Paragraph};

#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) enum Action {
    #[default]
    Save,
    Close,
    Cancel,
    Remove,
    Scope(SettingsScope),
    New,
    Suggestion(usize),
    Rule(usize),
    Delete(usize),
    Language(usize),
    Field(usize),
}

#[derive(Default)]
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
    pub editing: bool,
    pub fields: [Field; 2],
    pub focus: Action,
    pub error: Option<String>,
    pub reveal_focus: bool,
    scope: SettingsScope,
    languages: Vec<String>,
    suggestions: Vec<String>,
    rules: Vec<(String, String, bool)>,
    original_pattern: Option<String>,
    pub(crate) hits: Vec<(Rect, Action)>,
    scroll: usize,
    max_scroll: usize,
}
impl State {
    pub(crate) fn focused_input(&self) -> Option<TextInputId> {
        if self.editing
            && let Action::Field(index) = self.focus
        {
            Some(TextInputId::FileAssociation(index))
        } else {
            None
        }
    }

    fn can_remove(&self) -> bool {
        self.rules
            .iter()
            .any(|(pattern, _, local)| *local && Some(pattern) == self.original_pattern.as_ref())
    }
}

fn clear_inputs(app: &mut App) {
    app.text_input_histories
        .retain(|id, _| !matches!(id, TextInputId::FileAssociation(_)));
    app.remove_text_input_regions(|id| matches!(id, TextInputId::FileAssociation(_)));
    app.text_input_drag = None;
    app.text_input_click_id = None;
    app.text_input_drag_unit = None;
}

fn reload_rules(app: &mut App) {
    let scope = app.file_associations.scope;
    let layer = match scope {
        SettingsScope::Global => &app.settings_global,
        SettingsScope::Workspace => &app.settings_workspace,
    };
    let mut rules: Vec<_> = app
        .settings_for_scope(scope)
        .language_associations
        .unwrap_or_default()
        .into_iter()
        .map(|(pattern, language)| {
            let local = layer
                .language_associations
                .as_ref()
                .is_some_and(|map| map.contains_key(&pattern));
            (pattern, language, local)
        })
        .collect();
    rules.sort_by(|a, b| a.0.cmp(&b.0));
    app.file_associations.rules = rules;
}

fn load_pattern(app: &mut App, pattern: String) {
    clear_inputs(app);
    app.file_associations.original_pattern = None;
    if let Some((_, language, _)) = app
        .file_associations
        .rules
        .iter()
        .find(|rule| rule.0 == pattern)
    {
        app.file_associations.fields[1] = Field::new(language.clone());
        app.file_associations.original_pattern = Some(pattern.clone());
    }
    app.file_associations.editing = true;
    app.file_associations.fields[0] = Field::new(pattern);
    app.file_associations.fields[1].anchor = Some(0);
    app.file_associations.focus = Action::Field(1);
    app.file_associations.reveal_focus = true;
}

impl App {
    pub(crate) fn open_file_associations(&mut self) {
        self.flush_live_settings();
        self.finish_numeric_setting_edit();
        self.close_settings_dropdown();
        self.close_palette();
        self.clear_drag_states();
        self.menu_active = false;
        self.menu_open = false;
        self.terminal_panel_focused = false;
        self.dismiss_lsp_hover();
        let editor = self.editor().filter(|_| !self.settings_editor_active);
        let path = editor.and_then(|editor| editor.syntax_file_path());
        let mut suggestions = Vec::new();
        if let Some(path) = path {
            if let Some(ext) = path
                .extension()
                .and_then(|ext| ext.to_str())
                .filter(|ext| !ext.is_empty())
            {
                suggestions.push(format!("*.{ext}"));
            }
            if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                suggestions.push(name.to_owned());
            }
        }
        let pattern = path
            .and_then(|path| crate::file_associations::resolve(&self.language_associations, path))
            .map(|(pattern, _)| pattern.to_owned())
            .or_else(|| suggestions.first().cloned())
            .unwrap_or_default();
        let language = editor
            .map(|editor| editor.language_name().to_owned())
            .unwrap_or_default();
        let scope = if self.settings_editor_active {
            self.settings_scope
        } else if self
            .settings_workspace
            .language_associations
            .as_ref()
            .is_some_and(|map| map.contains_key(&pattern))
        {
            SettingsScope::Workspace
        } else {
            SettingsScope::Global
        };
        clear_inputs(self);
        self.file_associations = State {
            active: true,
            scope,
            suggestions,
            fields: [Field::new(pattern), Field::new(language)],
            languages: self.highlighter.syntax_names(),
            focus: Action::New,
            reveal_focus: true,
            ..Default::default()
        };
        reload_rules(self);
        if let Some(index) = self
            .file_associations
            .rules
            .iter()
            .position(|rule| rule.0 == self.file_associations.fields[0].text)
        {
            self.file_associations.focus = Action::Rule(index);
        }
    }

    pub(crate) fn save_file_association(
        &mut self,
        scope: SettingsScope,
        pattern: &str,
        language: Option<&str>,
    ) -> Result<(), String> {
        self.write_file_association(scope, None, pattern, language)
    }

    fn write_file_association(
        &mut self,
        scope: SettingsScope,
        original: Option<&str>,
        pattern: &str,
        language: Option<&str>,
    ) -> Result<(), String> {
        if language.is_some() {
            crate::file_associations::validate_pattern(pattern)?;
        }
        let language = language
            .map(|language| {
                let id = crate::file_associations::language_id(language);
                self.highlighter
                    .syntax_names()
                    .into_iter()
                    .find(|name| {
                        name.eq_ignore_ascii_case(language)
                            || crate::file_associations::language_id(name) == id
                    })
                    .ok_or_else(|| {
                        "Choose a file type from the list, or enter its full name.".to_owned()
                    })
            })
            .transpose()?;
        self.patch_settings_scope(scope, "languageAssociations", |settings| {
            let map = settings
                .language_associations
                .get_or_insert_with(Default::default);
            if let Some(language) = &language {
                if let Some(original) = original.filter(|original| *original != pattern) {
                    map.remove(original);
                }
                map.insert(pattern.into(), language.clone());
            } else {
                map.remove(pattern);
            }
        })
        .map_err(|error| format!("Could not save file association: {error}"))?;
        // Choosing a persistent type replaces this file's temporary mode in
        // its editor and diff views. Other files keep their explicit modes.
        let rule = [(pattern.to_owned(), String::new())].into_iter().collect();
        if self.editor().is_some_and(|editor| {
            editor.syntax_file_path()
                .is_some_and(|path| crate::file_associations::resolve(&rule, path).is_some())
        }) {
            self.set_editor_language(None);
        }
        self.visible_highlight_key = None;
        Ok(())
    }
}

fn finish_edit(app: &mut App, pattern: Option<&str>) {
    clear_inputs(app);
    reload_rules(app);
    let state = &mut app.file_associations;
    state.editing = false;
    state.original_pattern = None;
    state.fields = Default::default();
    state.error = None;
    state.focus = pattern
        .and_then(|pattern| state.rules.iter().position(|rule| rule.0 == pattern))
        .map(Action::Rule)
        .unwrap_or(Action::New);
    state.reveal_focus = true;
}

pub(crate) fn activate(app: &mut App, action: Action) {
    app.file_associations.error = None;
    match action {
        Action::Close => {
            clear_inputs(app);
            app.file_associations = State::default();
            app.modal = Default::default();
        }
        Action::Cancel => {
            let pattern = app.file_associations.original_pattern.clone();
            finish_edit(app, pattern.as_deref());
        }
        Action::Save if app.file_associations.editing => {
            let state = &app.file_associations;
            let scope = state.scope;
            let pattern = state.fields[0].text.trim().to_owned();
            let language = state.fields[1].text.trim().to_owned();
            if state.original_pattern.as_deref() != Some(pattern.as_str())
                && state.rules.iter().any(|rule| rule.0 == pattern)
            {
                app.file_associations.error = Some(format!(
                    "An association for {pattern} already exists. Edit that row instead."
                ));
                return;
            }
            // Renaming a local key replaces it in the same settings write.
            // Overriding an inherited key never removes its User definition.
            let original = state
                .can_remove()
                .then(|| state.original_pattern.clone())
                .flatten();
            if let Err(error) = app.write_file_association(
                scope,
                original.as_deref(),
                &pattern,
                Some(language.as_str()),
            ) {
                app.file_associations.error = Some(error);
                return;
            }
            finish_edit(app, Some(&pattern));
        }
        Action::Remove if app.file_associations.can_remove() => {
            let state = &app.file_associations;
            let pattern = state.original_pattern.clone().unwrap();
            if let Err(error) = app.save_file_association(state.scope, &pattern, None) {
                app.file_associations.error = Some(error);
                return;
            }
            finish_edit(app, Some(&pattern));
        }
        Action::Delete(index) if !app.file_associations.editing => {
            if let Some((pattern, _, true)) = app.file_associations.rules.get(index).cloned() {
                if let Err(error) =
                    app.save_file_association(app.file_associations.scope, &pattern, None)
                {
                    app.file_associations.error = Some(error);
                    return;
                }
                finish_edit(app, Some(&pattern));
            }
        }
        Action::Scope(scope) if !app.file_associations.editing => {
            app.file_associations.scope = scope;
            finish_edit(app, None);
            app.file_associations.focus = action;
            app.file_associations.scroll = 0;
        }
        Action::New if !app.file_associations.editing => {
            clear_inputs(app);
            let language = app
                .editor()
                .filter(|_| !app.settings_editor_active)
                .map(|editor| editor.language_name().to_owned())
                .unwrap_or_default();
            app.file_associations.fields = [Field::default(), Field::new(language)];
            app.file_associations.fields[1].anchor = Some(0);
            app.file_associations.original_pattern = None;
            app.file_associations.editing = true;
            app.file_associations.focus = Action::Field(0);
            app.file_associations.reveal_focus = true;
        }
        Action::Suggestion(index) if app.file_associations.editing => {
            if let Some(pattern) = app.file_associations.suggestions.get(index).cloned() {
                load_pattern(app, pattern);
            }
        }
        Action::Rule(index) if !app.file_associations.editing => {
            if let Some((pattern, _, _)) = app.file_associations.rules.get(index).cloned() {
                load_pattern(app, pattern);
            }
        }
        Action::Language(index) if app.file_associations.editing => {
            if let Some(language) = app.file_associations.languages.get(index).cloned() {
                clear_inputs(app);
                app.file_associations.fields[1] = Field::new(language);
                app.file_associations.focus = Action::Save;
            }
        }
        Action::Field(_) if app.file_associations.editing => app.file_associations.focus = action,
        _ => return,
    }
    app.file_associations.hits.clear();
}

enum Row {
    Rule(usize),
    Edit,
    Suggestions,
    Language(usize),
    LanguageHint(&'static str),
    Empty,
}

fn language_matches(state: &State) -> Vec<usize> {
    let query = state.fields[1].text.trim().to_lowercase();
    let id = crate::file_associations::language_id(&query);
    state
        .languages
        .iter()
        .enumerate()
        .filter_map(|(index, language)| {
            (language.to_lowercase().contains(&query)
                || crate::file_associations::language_id(language) == id)
                .then_some(index)
        })
        .collect()
}

fn edit_rows(state: &State, rows: &mut Vec<Row>) {
    rows.push(Row::Edit);
    if state.original_pattern.is_none()
        && !state.suggestions.is_empty()
        && matches!(state.focus, Action::Field(0) | Action::Suggestion(_))
    {
        rows.push(Row::Suggestions);
    }
    if matches!(state.focus, Action::Field(1) | Action::Language(_)) {
        let matches = language_matches(state);
        if matches.is_empty() {
            rows.push(Row::LanguageHint("No languages found"));
        } else {
            // Keep every language reachable without growing the popup to the
            // size of the syntax inventory. Arrow keys move this small window.
            let selected = matches
                .iter()
                .position(|&i| state.focus == Action::Language(i))
                .unwrap_or(0);
            let start = selected.saturating_sub(5);
            rows.extend(
                matches
                    .iter()
                    .skip(start)
                    .take(6)
                    .map(|&i| Row::Language(i)),
            );
            if matches.len() > 6 {
                rows.push(Row::LanguageHint("Type to narrow · ↑↓ to browse"));
            }
        }
    }
}

fn rows(state: &State) -> Vec<Row> {
    let mut rows = Vec::new();
    for (index, (pattern, _, _)) in state.rules.iter().enumerate() {
        if state.editing && state.original_pattern.as_ref() == Some(pattern) {
            edit_rows(state, &mut rows);
        } else {
            rows.push(Row::Rule(index));
        }
    }
    if state.editing && state.original_pattern.is_none() {
        edit_rows(state, &mut rows);
    } else if state.rules.is_empty() {
        rows.push(Row::Empty);
    }
    rows
}

fn row_has_focus(row: &Row, focus: Action) -> bool {
    match (row, focus) {
        (Row::Rule(i), Action::Rule(j) | Action::Delete(j)) => *i == j,
        (Row::Edit, Action::Field(_)) | (Row::Suggestions, Action::Suggestion(_)) => true,
        (Row::Language(i), Action::Language(j)) => *i == j,
        _ => false,
    }
}

fn focus_order(state: &State) -> Vec<Action> {
    if state.editing {
        let mut actions = vec![
            Action::Field(0),
            Action::Field(1),
            Action::Save,
            Action::Cancel,
        ];
        if state.can_remove() {
            actions.push(Action::Remove);
        }
        if state.original_pattern.is_none() {
            actions.extend((0..state.suggestions.len()).map(Action::Suggestion));
        }
        actions.push(Action::Close);
        actions
    } else {
        let mut actions = vec![
            Action::Scope(SettingsScope::Global),
            Action::Scope(SettingsScope::Workspace),
        ];
        for (index, (_, _, local)) in state.rules.iter().enumerate() {
            actions.push(Action::Rule(index));
            if *local {
                actions.push(Action::Delete(index));
            }
        }
        actions.extend([Action::New, Action::Close]);
        actions
    }
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    if app.file_associations.editing
        && matches!(app.file_associations.focus, Action::Language(_))
        && matches!(
            key.code,
            KeyCode::Char(_)
                | KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
        )
    {
        app.file_associations.focus = Action::Field(1);
    }
    match key.code {
        KeyCode::Tab
            if !key.modifiers.contains(KeyModifiers::SHIFT)
                && matches!(app.file_associations.focus, Action::Language(_)) =>
        {
            activate(app, app.file_associations.focus);
        }
        KeyCode::Esc => activate(
            app,
            if app.file_associations.editing {
                Action::Cancel
            } else {
                Action::Close
            },
        ),
        KeyCode::Up | KeyCode::Down
            if app.file_associations.editing
                && matches!(
                    app.file_associations.focus,
                    Action::Field(1) | Action::Language(_)
                ) =>
        {
            let matches = language_matches(&app.file_associations);
            if !matches.is_empty() {
                let current = matches
                    .iter()
                    .position(|&i| app.file_associations.focus == Action::Language(i));
                let index = match (current, key.code == KeyCode::Up) {
                    (Some(i), true) => i.saturating_sub(1),
                    (Some(i), false) => (i + 1).min(matches.len() - 1),
                    (None, true) => matches.len() - 1,
                    (None, false) => 0,
                };
                app.file_associations.focus = Action::Language(matches[index]);
                app.file_associations.reveal_focus = true;
            }
        }
        KeyCode::Up | KeyCode::Down
            if !app.file_associations.editing && !app.file_associations.rules.is_empty() =>
        {
            let last = app.file_associations.rules.len() - 1;
            let index = match (app.file_associations.focus, key.code == KeyCode::Up) {
                (Action::Rule(i) | Action::Delete(i), true) => i.saturating_sub(1),
                (Action::Rule(i) | Action::Delete(i), false) => (i + 1).min(last),
                (_, true) => last,
                (_, false) => 0,
            };
            app.file_associations.focus = Action::Rule(index);
            app.file_associations.reveal_focus = true;
        }
        KeyCode::Delete if !app.file_associations.editing => {
            if let Action::Rule(index) | Action::Delete(index) = app.file_associations.focus {
                activate(app, Action::Delete(index));
            }
        }
        KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
            let all = focus_order(&app.file_associations);
            let current = all
                .iter()
                .position(|action| *action == app.file_associations.focus)
                .unwrap_or(1);
            let reverse = matches!(key.code, KeyCode::BackTab | KeyCode::Up)
                || key.modifiers.contains(KeyModifiers::SHIFT);
            app.file_associations.focus =
                all[(current + if reverse { all.len() - 1 } else { 1 }) % all.len()];
            app.file_associations.reveal_focus = true;
        }
        KeyCode::Enter => {
            let action = app.file_associations.focus;
            activate(
                app,
                if action == Action::Field(0) {
                    Action::Field(1)
                } else if action == Action::Field(1) {
                    Action::Save
                } else {
                    action
                },
            );
        }
        KeyCode::Char(' ') if app.file_associations.focused_input().is_none() => {
            activate(app, app.file_associations.focus)
        }
        _ => {
            if let Some(id) = app.file_associations.focused_input() {
                if app
                    .edit_text_input(id, key.code, key.modifiers, None)
                    .changed
                {
                    app.text_input_changed(id);
                }
            }
        }
    }
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    let hit = app.file_associations.hits.iter().any(|(area, action)| {
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
                .file_associations
                .hits
                .iter()
                .find(|(area, _)| area.contains((mouse.column, mouse.row).into()))
                .copied()
            {
                activate(app, action);
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let over_languages = app.file_associations.hits.iter().any(|(rect, action)| {
                matches!(action, Action::Language(_))
                    && rect.contains((mouse.column, mouse.row).into())
            });
            if over_languages {
                let matches = language_matches(&app.file_associations);
                if !matches.is_empty() {
                    let current = matches
                        .iter()
                        .position(|&i| app.file_associations.focus == Action::Language(i))
                        .unwrap_or(0);
                    let index = if mouse.kind == MouseEventKind::ScrollUp {
                        current.saturating_sub(3)
                    } else {
                        (current + 3).min(matches.len() - 1)
                    };
                    app.file_associations.focus = Action::Language(matches[index]);
                    app.file_associations.reveal_focus = true;
                }
                return;
            }
            let state = &mut app.file_associations;
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
    let bg = crate::ui_widgets::surface_background(f, area, app.colors.ui.bg_widget);
    let mut style = app.colors.ui.button_style(
        bg,
        matches!(action, Action::Save | Action::New),
        area.contains((app.mouse_col, app.mouse_row).into()),
        true,
    );
    if app.file_associations.focus == action {
        style = style.add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
    }
    f.render_widget(Paragraph::new(format!(" {label} ")).style(style), area);
    app.file_associations.hits.push((area, action));
}

#[path = "ui_file_associations_table.rs"]
mod table;

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    table::draw(f, app);
}

#[cfg(test)]
#[path = "ui_file_associations_tests.rs"]
mod tests;
