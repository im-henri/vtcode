//! Scoped glob maps shared by Explorer and workspace searches.
use crate::{
    app::App,
    settings::{ExcludePatterns, SettingsScope},
    text_input::TextInputId,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Margin, Rect},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Clear, Paragraph},
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Action {
    #[default]
    New,
    Rule(usize),
    Field(usize),
    Enabled,
    Save,
    Cancel,
    Reset,
    Close,
}

#[derive(Default)]
pub(crate) struct Field {
    pub text: String,
    pub cursor: usize,
    pub anchor: Option<usize>,
}

#[derive(Default)]
pub(crate) struct State {
    pub active: bool,
    pub editing: bool,
    pub fields: [Field; 2],
    pub focus: Action,
    pub error: Option<String>,
    key: &'static str,
    scope: SettingsScope,
    rules: Vec<(String, serde_json::Value)>,
    original: Option<String>,
    enabled: bool,
    scroll: usize,
    max_scroll: usize,
    reveal_focus: bool,
    hits: Vec<(Rect, Action)>,
}

impl State {
    pub(crate) fn focused_input(&self) -> Option<TextInputId> {
        if self.editing
            && let Action::Field(i) = self.focus
        {
            Some(TextInputId::Exclusion(i))
        } else {
            None
        }
    }
}

fn patterns<'a>(settings: &'a crate::settings::Settings, key: &str) -> &'a ExcludePatterns {
    if key == "files.exclude" {
        &settings.files_exclude
    } else {
        &settings.search_exclude
    }
}

fn clear_inputs(app: &mut App) {
    app.text_input_histories
        .retain(|id, _| !matches!(id, TextInputId::Exclusion(_)));
    app.remove_text_input_regions(|id| matches!(id, TextInputId::Exclusion(_)));
    app.text_input_drag = None;
    app.text_input_click_id = None;
    app.text_input_drag_unit = None;
}

fn reload(app: &mut App) {
    clear_inputs(app);
    let state = &app.exclusion_settings;
    let settings = app.settings_for_scope(state.scope);
    app.exclusion_settings.rules = patterns(&settings, state.key)
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let state = &mut app.exclusion_settings;
    state.editing = false;
    state.original = None;
    state.fields = Default::default();
    state.focus = Action::New;
    state.reveal_focus = true;
    state.error = None;
}

impl App {
    pub(crate) fn open_exclusion_settings(&mut self, id: &str, pattern: Option<String>, new: bool) {
        self.flush_live_settings();
        self.finish_numeric_setting_edit();
        self.close_settings_dropdown();
        self.clear_drag_states();
        self.dismiss_lsp_hover();
        self.exclusion_settings = State {
            active: true,
            scope: self.settings_scope,
            key: if id == "files_exclude" {
                "files.exclude"
            } else {
                "search.exclude"
            },
            ..Default::default()
        };
        reload(self);
        if new {
            activate(self, Action::New);
        } else if let Some(index) = pattern.and_then(|p| {
            self.exclusion_settings
                .rules
                .iter()
                .position(|(key, _)| *key == p)
        }) {
            activate(self, Action::Rule(index));
        }
    }

    pub(crate) fn save_exclusion_pattern(
        &mut self,
        scope: SettingsScope,
        key: &str,
        original: Option<&str>,
        pattern: &str,
        value: Option<serde_json::Value>,
    ) -> Result<(), String> {
        if value.is_some() && (pattern.is_empty() || pattern.chars().any(char::is_control)) {
            return Err("Enter a glob pattern, such as **/generated or **/*.tmp.".into());
        }
        let inherited = match scope {
            SettingsScope::Global => self.settings_defaults.clone(),
            SettingsScope::Workspace => self.settings_for_scope(SettingsScope::Global),
        };
        self.patch_settings_scope(scope, key, |settings| {
            let map = if key == "files.exclude" {
                &mut settings.files_exclude
            } else {
                &mut settings.search_exclude
            };
            if let Some(original) = original.filter(|old| *old != pattern) {
                if patterns(&inherited, key).contains_key(original) {
                    map.insert(original.into(), serde_json::Value::Bool(false));
                } else {
                    map.remove(original);
                }
            }
            if let Some(value) = &value {
                map.insert(pattern.into(), value.clone());
            } else {
                map.remove(pattern);
            }
        })
    }
}

pub(crate) fn activate(app: &mut App, action: Action) {
    app.exclusion_settings.error = None;
    match action {
        Action::Close => {
            clear_inputs(app);
            app.exclusion_settings = Default::default();
            app.modal = Default::default();
        }
        Action::Cancel => reload(app),
        Action::New | Action::Rule(_) => {
            clear_inputs(app);
            let existing = if let Action::Rule(i) = action {
                app.exclusion_settings.rules.get(i).cloned()
            } else {
                None
            };
            let state = &mut app.exclusion_settings;
            state.editing = true;
            state.scroll = 0;
            state.reveal_focus = true;
            state.original = existing.as_ref().map(|(pattern, _)| pattern.clone());
            state.enabled = existing
                .as_ref()
                .is_none_or(|(_, value)| value != &serde_json::Value::Bool(false));
            let when = existing
                .as_ref()
                .and_then(|(_, value)| value.get("when")?.as_str())
                .unwrap_or("")
                .to_owned();
            state.fields[0].text = existing.map_or_else(String::new, |(pattern, _)| pattern);
            state.fields[1].text = when;
            for field in &mut state.fields {
                field.cursor = field.text.len();
            }
            state.focus = Action::Field(0);
        }
        Action::Enabled => app.exclusion_settings.enabled = !app.exclusion_settings.enabled,
        Action::Save | Action::Reset => {
            let state = &app.exclusion_settings;
            let pattern = state.fields[0].text.trim().to_owned();
            let when = state.fields[1].text.trim();
            let value = if action == Action::Reset {
                None
            } else if !state.enabled {
                Some(serde_json::Value::Bool(false))
            } else if when.is_empty() {
                Some(serde_json::Value::Bool(true))
            } else {
                Some(serde_json::json!({"when": when}))
            };
            if action == Action::Save
                && state.original.as_deref() != Some(&pattern)
                && state.rules.iter().any(|(p, _)| *p == pattern)
            {
                app.exclusion_settings.error =
                    Some("That pattern already exists. Select its row to edit it.".into());
                return;
            }
            if action == Action::Reset && state.original.is_none() {
                return;
            }
            let original = state.original.clone();
            let target = if action == Action::Reset {
                original.as_deref().unwrap()
            } else {
                &pattern
            };
            match app.save_exclusion_pattern(
                state.scope,
                state.key,
                if action == Action::Reset {
                    None
                } else {
                    original.as_deref()
                },
                target,
                value,
            ) {
                Ok(()) => reload(app),
                Err(error) => app.exclusion_settings.error = Some(error),
            }
        }
        Action::Field(i) => app.focus_text_input(TextInputId::Exclusion(i)),
    }
}

fn actions(state: &State) -> Vec<Action> {
    if state.editing {
        let mut actions = vec![
            Action::Field(0),
            Action::Field(1),
            Action::Enabled,
            Action::Save,
            Action::Cancel,
        ];
        if state.original.is_some() {
            actions.push(Action::Reset);
        }
        actions.push(Action::Close);
        actions
    } else {
        (0..state.rules.len())
            .map(Action::Rule)
            .chain([Action::New, Action::Close])
            .collect()
    }
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => activate(
            app,
            if app.exclusion_settings.editing {
                Action::Cancel
            } else {
                Action::Close
            },
        ),
        KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
            let actions = actions(&app.exclusion_settings);
            let index = actions
                .iter()
                .position(|a| *a == app.exclusion_settings.focus)
                .unwrap_or(0);
            let reverse = matches!(key.code, KeyCode::BackTab | KeyCode::Up)
                || key.modifiers.contains(KeyModifiers::SHIFT);
            app.exclusion_settings.focus =
                actions[(index + if reverse { actions.len() - 1 } else { 1 }) % actions.len()];
            app.exclusion_settings.reveal_focus = true;
        }
        KeyCode::Enter => activate(
            app,
            match app.exclusion_settings.focus {
                Action::Field(0) => Action::Field(1),
                Action::Field(1) => Action::Save,
                action => action,
            },
        ),
        KeyCode::Char(' ') if app.exclusion_settings.focused_input().is_none() => {
            activate(app, app.exclusion_settings.focus)
        }
        _ => {
            if let Some(id) = app.exclusion_settings.focused_input()
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
    if app.handle_overlay_text_input_mouse(mouse.kind, mouse.column, mouse.row, mouse.modifiers) {
        return;
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some((_, action)) = app
                .exclusion_settings
                .hits
                .iter()
                .find(|(rect, _)| rect.contains((mouse.column, mouse.row).into()))
            {
                let action = *action;
                app.exclusion_settings.focus = action;
                activate(app, action);
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let state = &mut app.exclusion_settings;
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

fn rule_label(pattern: &str, value: &serde_json::Value) -> String {
    if value == &serde_json::Value::Bool(false) {
        format!("[ ] {pattern}  (disabled)")
    } else if let Some(when) = value.get("when").and_then(serde_json::Value::as_str) {
        format!("[x] {pattern}  when: {when}")
    } else {
        format!("[x] {pattern}")
    }
}

fn button(f: &mut Frame, app: &mut App, area: Rect, label: &str, action: Action, bg: Color) {
    let focused = app.exclusion_settings.focus == action;
    let mut style = app.colors.ui.button_style(
        bg,
        action == Action::Save || action == Action::New,
        area.contains((app.mouse_col, app.mouse_row).into()),
        true,
    );
    if focused {
        style = style.add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
    }
    f.render_widget(Paragraph::new(format!(" {label} ")).style(style), area);
    app.exclusion_settings.hits.push((area, action));
}

pub(crate) fn draw(f: &mut Frame, app: &mut App) {
    app.exclusion_settings.hits.clear();
    app.remove_text_input_regions(|id| matches!(id, TextInputId::Exclusion(_)));
    let viewport = f.area();
    let width = viewport.width.saturating_sub(4).min(90);
    let height = viewport
        .height
        .saturating_sub(2)
        .min(if app.exclusion_settings.editing {
            16
        } else {
            12u16.saturating_add(app.exclusion_settings.rules.len().min(u16::MAX as usize) as u16)
        });
    let area = Rect::new(
        viewport.x + (viewport.width - width) / 2,
        viewport.y + (viewport.height - height) / 2,
        width,
        height,
    );
    let bg = app.colors.ui.bg_widget;
    let fg = app.colors.ui.widget_text_color(bg);
    let normal = Style::default().fg(fg).bg(bg);
    f.render_widget(Clear, area);
    let title = format!(
        " {} · {} ",
        app.exclusion_settings.key,
        app.exclusion_settings.scope.label()
    );
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .style(normal),
        area,
    );
    app.modal.area = area;
    app.modal.close = Rect::new(area.right().saturating_sub(4), area.y, 3.min(width), 1);
    button(f, app, app.modal.close, "×", Action::Close, bg);
    if width < 18 || height < 7 {
        return;
    }
    let inner = area.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });
    if app.exclusion_settings.editing {
        let rows = inner.height.saturating_sub(3) as usize;
        let state = &mut app.exclusion_settings;
        state.max_scroll = 7usize.saturating_sub(rows);
        if state.reveal_focus {
            let target = match state.focus {
                Action::Field(i) => Some(i * 3 + 1),
                Action::Enabled => Some(6),
                _ => None,
            };
            if let Some(target) = target {
                if target < state.scroll {
                    state.scroll = target.saturating_sub(1);
                } else if target >= state.scroll + rows {
                    state.scroll = target.saturating_sub(rows.saturating_sub(1));
                }
            }
        }
        state.scroll = state.scroll.min(state.max_scroll);
        state.reveal_focus = false;
        let first_y = i64::from(inner.y) - state.scroll as i64;
        let end_y = i64::from(inner.y) + rows as i64;
        for (i, label) in [
            "Glob pattern",
            "When sibling exists (optional, e.g. $(basename).ts)",
        ]
        .iter()
        .enumerate()
        {
            let y = first_y + i as i64 * 3;
            if y >= i64::from(inner.y) && y < end_y {
                f.render_widget(
                    Paragraph::new(*label).style(normal),
                    Rect::new(inner.x, y as u16, inner.width, 1),
                );
            }
            if y + 1 < i64::from(inner.y) || y + 1 >= end_y {
                continue;
            }
            let field = &app.exclusion_settings.fields[i];
            let focused = app.exclusion_settings.focus == Action::Field(i);
            let input = Rect::new(inner.x, (y + 1) as u16, inner.width, 1);
            let hit = crate::ui_widgets::InputField::new(&field.text)
                .theme_control(&app.colors, app.colors.ui.settings_control("text"), bg)
                .prefix("", Color::Reset)
                .cursor_pos(field.cursor)
                .cursor_marker(if focused && app.cursor_visible {
                    "│"
                } else {
                    ""
                })
                .selection_range(if focused {
                    App::inline_selection_range(field.cursor, field.anchor)
                } else {
                    None
                })
                .render(f, input);
            app.register_text_input(TextInputId::Exclusion(i), hit);
            app.exclusion_settings.hits.push((input, Action::Field(i)));
        }
        let y = first_y + 6;
        let label = if app.exclusion_settings.enabled {
            "[x] Exclude matching paths"
        } else {
            "[ ] Exclude matching paths"
        };
        if y >= i64::from(inner.y) && y < end_y {
            button(
                f,
                app,
                Rect::new(inner.x, y as u16, inner.width, 1),
                label,
                Action::Enabled,
                bg,
            );
        }
        let mut x = inner.x;
        for (label, action) in [
            ("Save", Action::Save),
            ("Cancel", Action::Cancel),
            ("Reset Override", Action::Reset),
        ] {
            if action == Action::Reset && app.exclusion_settings.original.is_none() {
                continue;
            }
            let w = (label.len() as u16 + 2).min(inner.right().saturating_sub(x));
            button(
                f,
                app,
                Rect::new(x, inner.bottom() - 1, w, 1),
                label,
                action,
                bg,
            );
            x += w + 1;
        }
    } else {
        let description = if app.exclusion_settings.key == "search.exclude" {
            "Also inherits Files: Exclude. Select a pattern to edit or disable."
        } else {
            "Hidden in Explorer; inherited by workspace search and Quick Open."
        };
        f.render_widget(
            Paragraph::new(description).style(normal),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        let rows = inner.height.saturating_sub(4) as usize;
        let state = &mut app.exclusion_settings;
        state.max_scroll = state.rules.len().saturating_sub(rows);
        if state.reveal_focus
            && let Action::Rule(index) = state.focus
        {
            if index < state.scroll {
                state.scroll = index;
            } else if index >= state.scroll + rows {
                state.scroll = index.saturating_sub(rows.saturating_sub(1));
            }
        }
        state.scroll = state.scroll.min(state.max_scroll);
        state.reveal_focus = false;
        let rules = state
            .rules
            .iter()
            .enumerate()
            .skip(state.scroll)
            .take(rows)
            .map(|(i, (p, v))| (i, rule_label(p, v)))
            .collect::<Vec<_>>();
        for (offset, (index, label)) in rules.into_iter().enumerate() {
            button(
                f,
                app,
                Rect::new(inner.x, inner.y + 2 + offset as u16, inner.width, 1),
                &label,
                Action::Rule(index),
                bg,
            );
        }
        button(
            f,
            app,
            Rect::new(inner.x, inner.bottom() - 1, 15.min(inner.width), 1),
            "Add Pattern",
            Action::New,
            bg,
        );
    }
    if let Some(error) = &app.exclusion_settings.error {
        f.render_widget(
            Paragraph::new(error.as_str()).style(normal.fg(app.colors.fg_error)),
            Rect::new(inner.x, inner.bottom().saturating_sub(2), inner.width, 1),
        );
    }
}

pub(crate) fn draw_setting(
    f: &mut Frame,
    app: &App,
    value: &str,
    area: Rect,
    start_y: i64,
    selected: bool,
    bg: Color,
) {
    let rules = serde_json::from_str::<ExcludePatterns>(value).unwrap_or_default();
    let labels = rules
        .iter()
        .map(|(p, v)| rule_label(p, v))
        .chain(std::iter::once(" Add Pattern ".into()));
    let count = rules.len();
    for (index, label) in labels.enumerate() {
        let y = start_y + index as i64;
        if y < i64::from(area.y) || y >= i64::from(area.bottom()) {
            continue;
        }
        let row = Rect::new(area.x + 2, y as u16, area.width.saturating_sub(3), 1);
        let style = if index == count {
            app.colors.ui.button_style(
                bg,
                false,
                row.contains((app.mouse_col, app.mouse_row).into()),
                true,
            )
        } else {
            crate::ui_settings::settings_control_style(&app.colors, "text", bg, selected)
        };
        f.render_widget(
            Paragraph::new(crate::ui_pickers::truncate_to_width(
                &label,
                row.width as usize,
            ))
            .style(style),
            row,
        );
    }
}

#[cfg(test)]
#[path = "ui_exclusion_settings_tests.rs"]
mod tests;
