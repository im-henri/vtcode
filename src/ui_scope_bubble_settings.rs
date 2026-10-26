//! Per-language scope profiles. Drafts stay local until Save succeeds.
use crate::{
    app::App,
    scope_bubble_config::{self as config, Config, Delimiter, Syntax},
    settings::SettingsScope,
    text_input::TextInputId,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Action {
    #[default]
    New,
    Close,
    Cancel,
    Save,
    Reset,
    Scope(SettingsScope),
    Rule(usize),
    Language,
    ChooseLanguage,
    ChooseSyntax,
    PickLanguage(usize),
    PickSyntax(Syntax),
    Pair(Delimiter),
    Blocks,
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
    pub field: Field,
    pub focus: Action,
    pub reveal_focus: bool,
    pub error: Option<String>,
    scope: SettingsScope,
    original: Option<String>,
    draft: Config,
    rules: Vec<(String, Config, bool)>,
    languages: Vec<String>,
    menu: Option<Action>,
    menu_index: usize,
    popup: Rect,
    hits: Vec<(Rect, Action)>,
    body: Rect,
    scroll: usize,
    max_scroll: usize,
}
impl State {
    pub(crate) fn focused_input(&self) -> Option<TextInputId> {
        (self.editing && self.focus == Action::Language && self.menu != Some(Action::ChooseSyntax))
            .then_some(TextInputId::ScopeBubbleLanguage)
    }
    fn can_reset(&self) -> bool {
        self.rules
            .iter()
            .any(|(name, _, local)| *local && Some(name) == self.original.as_ref())
    }
    fn syntax(&self) -> Syntax {
        if self.draft.syntax == Syntax::Automatic {
            Syntax::for_language(&self.field.text).unwrap_or(Syntax::Automatic)
        } else {
            self.draft.syntax
        }
    }
    fn pairs(&self) -> Vec<Delimiter> {
        self.draft.delimiters.clone().unwrap_or_else(|| {
            if self.draft.syntax == Syntax::Python
                || (self.draft.syntax == Syntax::Automatic
                    && config::language_key(&self.field.text) == "python")
            {
                vec![]
            } else {
                vec![Delimiter::Braces]
            }
        })
    }
    fn choices(&self) -> Vec<Action> {
        match self.menu {
            Some(Action::ChooseSyntax) => Syntax::ALL.into_iter().map(Action::PickSyntax).collect(),
            Some(Action::ChooseLanguage) => {
                let query = self.field.text.trim().to_ascii_lowercase();
                self.languages
                    .iter()
                    .enumerate()
                    .filter_map(|(index, name)| {
                        (name.to_ascii_lowercase().contains(&query)
                            || config::language_key(name) == query)
                            .then_some(Action::PickLanguage(index))
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }
}

fn clear_inputs(app: &mut App) {
    app.text_input_histories
        .remove(&TextInputId::ScopeBubbleLanguage);
    app.remove_text_input_regions(|id| id == TextInputId::ScopeBubbleLanguage);
    app.text_input_drag = None;
    app.text_input_click_id = None;
    app.text_input_drag_unit = None;
}
fn reload(app: &mut App) {
    let scope = app.scope_bubble_settings.scope;
    let layer = match scope {
        SettingsScope::Global => &app.settings_global,
        SettingsScope::Workspace => &app.settings_workspace,
    };
    let local = config::merge(&None, &layer.animation_scope_bubble_languages).unwrap_or_default();
    app.scope_bubble_settings.rules = config::merge(
        &None,
        &app.settings_for_scope(scope)
            .animation_scope_bubble_languages,
    )
    .unwrap_or_default()
    .into_iter()
    .map(|(name, rule)| {
        let owned = local.contains_key(&name);
        (name, rule, owned)
    })
    .collect();
}
fn finish_edit(app: &mut App) {
    clear_inputs(app);
    reload(app);
    let state = &mut app.scope_bubble_settings;
    state.editing = false;
    state.menu = None;
    state.error = None;
    state.focus = state
        .rules
        .iter()
        .position(|(name, _, _)| Some(name) == state.original.as_ref())
        .map(Action::Rule)
        .unwrap_or(Action::New);
    state.reveal_focus = true;
}
fn edit(app: &mut App, language: String) {
    clear_inputs(app);
    let key = config::language_key(&language);
    let state = &mut app.scope_bubble_settings;
    let existing = state.rules.iter().find(|(name, _, _)| *name == key);
    state.original = existing.map(|rule| rule.0.clone());
    state.draft = existing.map(|rule| rule.1.clone()).unwrap_or_default();
    state.field = Field::new(language);
    state.field.anchor = Some(0);
    state.editing = true;
    state.focus = Action::Language;
    state.menu = None;
    state.error = None;
    state.scroll = 0;
    state.reveal_focus = true;
}
impl App {
    pub(crate) fn open_scope_bubble_settings(&mut self) {
        self.flush_live_settings();
        self.finish_numeric_setting_edit();
        self.close_settings_dropdown();
        self.close_palette();
        self.clear_drag_states();
        self.menu_active = false;
        self.menu_open = false;
        self.terminal_panel_focused = false;
        self.dismiss_lsp_hover();
        clear_inputs(self);
        self.scope_bubble_settings = State {
            active: true,
            scope: if self.settings_editor_active {
                self.settings_scope
            } else {
                SettingsScope::Global
            },
            languages: self.highlighter.syntax_names(),
            ..Default::default()
        };
        reload(self);
    }

    pub(crate) fn write_scope_bubble_rule(
        &mut self,
        scope: SettingsScope,
        original: Option<&str>,
        language: &str,
        value: Option<Config>,
    ) -> Result<(), String> {
        let key = config::language_key(language);
        if key.is_empty() {
            return Err("Enter a language name or ID.".into());
        }
        self.patch_settings_scope(scope, "animationScopeBubbleLanguages", |settings| {
            let mut map = config::merge(&None, &settings.animation_scope_bubble_languages)
                .unwrap_or_default();
            if let Some(original) = original {
                map.remove(&config::language_key(original));
            }
            if let Some(value) = &value {
                map.insert(key.clone(), value.clone());
            } else {
                map.remove(&key);
            }
            settings.animation_scope_bubble_languages = (!map.is_empty()).then_some(map);
        })
    }
}
pub(crate) fn input_changed(app: &mut App) {
    let state = &mut app.scope_bubble_settings;
    state.error = None;
    state.menu_index = 0;
    state.reveal_focus = true;
    state.hits.clear();
}
fn report_error(app: &mut App, error: String) {
    app.scope_bubble_settings.error = Some(error);
    app.scope_bubble_settings.reveal_focus = true;
    app.scope_bubble_settings.hits.clear();
}
pub(crate) fn activate(app: &mut App, action: Action) {
    match action {
        Action::Close => {
            clear_inputs(app);
            app.scope_bubble_settings = State::default();
        }
        Action::Cancel => finish_edit(app),
        Action::Scope(scope) if !app.scope_bubble_settings.editing => {
            app.scope_bubble_settings.scope = scope;
            finish_edit(app);
            app.scope_bubble_settings.focus = action;
        }
        Action::New => {
            let language = app
                .editor()
                .filter(|_| !app.settings_editor_active)
                .map(|editor| config::language(editor).to_owned())
                .unwrap_or_default();
            edit(app, language);
        }
        Action::Rule(index) => {
            if let Some(rule) = app.scope_bubble_settings.rules.get(index) {
                edit(app, rule.0.clone());
            }
        }
        Action::Save if app.scope_bubble_settings.editing => {
            let state = &app.scope_bubble_settings;
            let key = config::language_key(&state.field.text);
            if key.is_empty() {
                report_error(app, "Enter a language name or ID.".into());
                return;
            }
            if state.original.as_deref() != Some(&key)
                && state.rules.iter().any(|rule| rule.0 == key)
            {
                report_error(
                    app,
                    "This language already has a rule. Edit its row instead.".into(),
                );
                return;
            }
            if state.syntax() == Syntax::Automatic {
                report_error(app, "Choose a syntax style for this language, for example C++ for a C-like language.".into());
                return;
            }
            let scope = state.scope;
            let original = state.can_reset().then(|| state.original.clone()).flatten();
            let draft = state.draft.clone();
            if let Err(error) =
                app.write_scope_bubble_rule(scope, original.as_deref(), &key, Some(draft))
            {
                report_error(app, error);
                return;
            }
            app.scope_bubble_settings.original = Some(key);
            finish_edit(app);
        }
        Action::Reset if app.scope_bubble_settings.can_reset() => {
            let state = &app.scope_bubble_settings;
            let name = state.original.clone().unwrap();
            if let Err(error) = app.write_scope_bubble_rule(state.scope, None, &name, None) {
                report_error(app, error);
                return;
            }
            finish_edit(app);
        }
        Action::ChooseLanguage | Action::ChooseSyntax => {
            let state = &mut app.scope_bubble_settings;
            state.focus = if action == Action::ChooseLanguage {
                Action::Language
            } else {
                action
            };
            state.menu = if state.menu == Some(action) {
                None
            } else {
                Some(action)
            };
            state.menu_index = if action == Action::ChooseSyntax {
                Syntax::ALL
                    .iter()
                    .position(|s| *s == state.draft.syntax)
                    .unwrap_or(0)
            } else {
                0
            };
        }
        Action::PickLanguage(index) => {
            if let Some(language) = app.scope_bubble_settings.languages.get(index).cloned() {
                // Choosing an existing language opens its effective rule instead of creating a duplicate.
                edit(app, language);
                app.scope_bubble_settings.focus = Action::ChooseSyntax;
            }
        }
        Action::PickSyntax(syntax) => {
            let state = &mut app.scope_bubble_settings;
            state.error = None;
            let had_delimiters = state.syntax().has_delimiters();
            state.draft.syntax = syntax;
            if !state.syntax().has_delimiters() || !had_delimiters {
                state.draft.delimiters = None;
            }
            state.menu = None;
            state.focus = Action::ChooseSyntax;
        }
        Action::Pair(pair) if app.scope_bubble_settings.draft.syntax != Syntax::Disabled => {
            let state = &mut app.scope_bubble_settings;
            state.error = None;
            let mut pairs = state.pairs();
            if pairs.contains(&pair) {
                pairs.retain(|p| *p != pair);
            } else {
                pairs.push(pair);
            }
            state.draft.delimiters = Some(pairs);
            state.focus = action;
        }
        Action::Blocks if app.scope_bubble_settings.draft.syntax != Syntax::Disabled => {
            app.scope_bubble_settings.error = None;
            app.scope_bubble_settings.draft.language_blocks ^= true;
            app.scope_bubble_settings.focus = action;
        }
        Action::Language => app.scope_bubble_settings.focus = action,
        _ => {}
    }
    app.scope_bubble_settings.hits.clear();
    app.scope_bubble_settings.reveal_focus = true;
}
fn focus_order(state: &State) -> Vec<Action> {
    let mut actions = if state.editing {
        let mut actions = vec![
            Action::Language,
            Action::ChooseLanguage,
            Action::ChooseSyntax,
        ];
        if state.draft.syntax != Syntax::Disabled {
            if state.syntax().has_delimiters() {
                actions.extend(Delimiter::ALL.map(Action::Pair));
            }
            actions.push(Action::Blocks);
        }
        actions.extend([Action::Save, Action::Cancel]);
        if state.can_reset() {
            actions.push(Action::Reset);
        }
        actions
    } else {
        let mut actions = vec![
            Action::Scope(SettingsScope::Global),
            Action::Scope(SettingsScope::Workspace),
        ];
        actions.extend((0..state.rules.len()).map(Action::Rule));
        actions.push(Action::New);
        actions
    };
    actions.push(Action::Close);
    actions
}
pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    if app.scope_bubble_settings.menu.is_some() {
        let choices = app.scope_bubble_settings.choices();
        match key.code {
            KeyCode::Esc => {
                app.scope_bubble_settings.menu = None;
                return;
            }
            KeyCode::Up | KeyCode::Down => {
                let state = &mut app.scope_bubble_settings;
                state.menu_index = if key.code == KeyCode::Up {
                    state.menu_index.saturating_sub(1)
                } else {
                    (state.menu_index + 1).min(choices.len().saturating_sub(1))
                };
                return;
            }
            KeyCode::Enter => {
                if let Some(action) = choices.get(app.scope_bubble_settings.menu_index) {
                    activate(app, *action);
                }
                return;
            }
            KeyCode::Tab | KeyCode::BackTab => app.scope_bubble_settings.menu = None,
            _ if app.scope_bubble_settings.menu == Some(Action::ChooseSyntax) => return,
            _ => {}
        }
    }
    match key.code {
        KeyCode::Esc => activate(
            app,
            if app.scope_bubble_settings.editing {
                Action::Cancel
            } else {
                Action::Close
            },
        ),
        KeyCode::Down if app.scope_bubble_settings.focus == Action::Language => {
            activate(app, Action::ChooseLanguage)
        }
        KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
            let state = &mut app.scope_bubble_settings;
            let order = focus_order(state);
            let i = order
                .iter()
                .position(|action| *action == state.focus)
                .unwrap_or(0);
            let reverse = matches!(key.code, KeyCode::BackTab | KeyCode::Up)
                || key.modifiers.contains(KeyModifiers::SHIFT);
            state.focus = order[(i + if reverse { order.len() - 1 } else { 1 }) % order.len()];
            state.reveal_focus = true;
        }
        KeyCode::Enter => activate(
            app,
            if app.scope_bubble_settings.focus == Action::Language {
                Action::ChooseSyntax
            } else {
                app.scope_bubble_settings.focus
            },
        ),
        KeyCode::Char(' ') if app.scope_bubble_settings.focused_input().is_none() => {
            activate(app, app.scope_bubble_settings.focus)
        }
        _ => {
            if let Some(id) = app.scope_bubble_settings.focused_input() {
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
    let action = app
        .scope_bubble_settings
        .hits
        .iter()
        .find(|(rect, _)| rect.contains((mouse.column, mouse.row).into()))
        .map(|(_, a)| *a);
    if (action == Some(Action::Language)
        || matches!(mouse.kind, MouseEventKind::Drag(_) | MouseEventKind::Up(_)))
        && app.handle_overlay_text_input_mouse(mouse.kind, mouse.column, mouse.row, mouse.modifiers)
    {
        return;
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if app.scope_bubble_settings.menu.is_some()
                && !matches!(
                    action,
                    Some(Action::PickSyntax(_) | Action::PickLanguage(_) | Action::Language)
                )
            {
                app.scope_bubble_settings.menu = None;
                app.scope_bubble_settings.hits.clear();
                return;
            }
            if let Some(action) = action {
                activate(app, action);
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let up = mouse.kind == MouseEventKind::ScrollUp;
            let state = &mut app.scope_bubble_settings;
            if state.menu.is_some() {
                state.menu_index = if up {
                    state.menu_index.saturating_sub(3)
                } else {
                    (state.menu_index + 3).min(state.choices().len().saturating_sub(1))
                };
            } else {
                state.scroll = if up {
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

#[path = "ui_scope_bubble_settings_view.rs"]
mod view;
pub(crate) use view::draw;

#[cfg(test)]
#[path = "ui_scope_bubble_settings_tests.rs"]
mod tests;
