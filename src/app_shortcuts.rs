//! Shared shortcut definitions, persistence and the assignment dialog.
use crate::app::{App, KeyBinding};
use crate::shortcut_context::{Context, When};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum EditAction {
    Save,
    Cancel,
    Record,
    Chord,
    Type,
    Reset,
    Unassign,
    When,
}

const FOCUS_ACTIONS: [EditAction; 8] = [
    EditAction::Record,
    EditAction::Chord,
    EditAction::Type,
    EditAction::When,
    EditAction::Save,
    EditAction::Cancel,
    EditAction::Reset,
    EditAction::Unassign,
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditField {
    Shortcut,
    When,
}

pub(crate) struct EditorState {
    index: usize,
    settings: bool,
    scope: crate::settings::SettingsScope,
    current: String,
    text: String,
    draft_when: Option<String>,
    recording: bool,
    initial_recording: bool,
    chord_mode: bool,
    first: Option<Stroke>,
    typing: Option<EditField>,
    cursor: usize,
    selection: Option<usize>,
    field_hit: Option<crate::text_input::TextInputHitMap>,
    focus: usize,
    error: Option<String>,
    areas: Vec<(Rect, EditAction)>,
    owner: Option<u64>,
    area: Option<Rect>,
}

#[path = "shortcut_defaults.rs"]
mod defaults;

pub(crate) type Stroke = (KeyModifiers, KeyCode);

/// Index exact and factory strokes once per edit to the shortcut table.
/// Mutating through the ordinary vector API always invalidates both indexes.
#[derive(Default)]
#[cfg_attr(test, derive(Clone))]
pub(crate) struct KeyBindings {
    bindings: Vec<KeyBinding>,
    index: std::cell::OnceCell<StrokeIndex>,
}

#[derive(Default)]
#[cfg_attr(test, derive(Clone))]
struct StrokeIndex {
    current: rustc_hash::FxHashMap<Stroke, Vec<usize>>,
    defaults: rustc_hash::FxHashMap<Stroke, Vec<usize>>,
}

impl From<Vec<KeyBinding>> for KeyBindings {
    fn from(bindings: Vec<KeyBinding>) -> Self {
        Self { bindings, index: Default::default() }
    }
}

impl std::ops::Deref for KeyBindings {
    type Target = Vec<KeyBinding>;

    fn deref(&self) -> &Self::Target {
        &self.bindings
    }
}

impl std::ops::DerefMut for KeyBindings {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.index.take();
        &mut self.bindings
    }
}

impl KeyBindings {
    fn index(&self) -> &StrokeIndex {
        self.index.get_or_init(|| {
            let mut index = StrokeIndex::default();
            for (position, binding) in self.bindings.iter().enumerate() {
                if binding.code != KeyCode::Null {
                    index.current.entry(normalize((binding.modifiers, binding.code)))
                        .or_default().push(position);
                }
                index.defaults.entry(normalize((binding.default_modifiers, binding.default_code)))
                    .or_default().push(position);
            }
            index
        })
    }
}

fn blocked_by_popup(app: &App) -> bool {
    app.menu_active
        || app.context_menu_active
        || app.shortcuts_active
        || app.hunk_popup_active
        || app.lsp_code_action_active
        || app.call_hierarchy_active
        || app.type_hierarchy_active
        || app.lsp_references_active
        || app.quick_open_active
        || app.goto_line_active
        || app.symbol_list_active
        || crate::ui_modal::active(app).is_some()
        || crate::ui_quick_picker::active(app).is_some()
}

pub(crate) fn normalize((modifiers, code): Stroke) -> Stroke {
    match code {
        KeyCode::BackTab => (modifiers | KeyModifiers::SHIFT, KeyCode::Tab),
        KeyCode::Char(c) if c.is_ascii_alphabetic() => {
            (modifiers, KeyCode::Char(c.to_ascii_lowercase()))
        }
        _ => (modifiers, code),
    }
}

pub(crate) fn display_stroke(modifiers: KeyModifiers, code: KeyCode) -> String {
    KeyBinding {
        id: "",
        label: "",
        modifiers,
        code,
        default_modifiers: modifiers,
        default_code: code,
        chord: None,
        default_chord: None,
        built_in_handler: false,
        editor_only: false,
        when: None,
        priority: 0,
    }
    .display()
}

pub(crate) fn parse_shortcut(text: &str) -> Option<(Stroke, Option<Stroke>)> {
    let parts: Vec<_> = text.split_whitespace().collect();
    let (first, second) = if parts.len() == 2 {
        (
            parts[0].strip_suffix(',').unwrap_or(parts[0]),
            Some(parts[1]),
        )
    } else if let Some((first, second)) = text
        .split_once(',')
        .filter(|(_, second)| !second.trim().is_empty())
    {
        (first.trim(), Some(second.trim()))
    } else {
        (text.trim(), None)
    };
    let parse = |part: &str| App::keybinding_from_settings_text(part);
    let first = parse(first)?;
    let second = match second {
        Some(second) => Some(parse(second)?),
        None => None,
    };
    if (first.1 == KeyCode::Null && second.is_some())
        || second.is_some_and(|stroke| stroke.1 == KeyCode::Null)
    {
        return None;
    }
    Some((first, second))
}

pub(crate) fn append_builtins(bindings: &mut Vec<KeyBinding>) {
    for &(command, label, key) in defaults::BUILTINS {
        let id = crate::app_menu::keybinding_id_for_command(command);
        let Some(((modifiers, code), chord)) = parse_shortcut(key) else {
            continue;
        };
        if bindings.iter().any(|binding| {
            binding.id == id
                && normalize((binding.default_modifiers, binding.default_code))
                    == normalize((modifiers, code))
                && binding.default_chord.map(normalize) == chord.map(normalize)
        }) {
            continue;
        }
        bindings.push(KeyBinding {
            id,
            label,
            modifiers,
            code,
            default_modifiers: modifiers,
            default_code: code,
            chord,
            default_chord: chord,
            built_in_handler: !matches!(
                id,
                "reopen_closed" | "toggle_zen" | "settings" | "show_explorer" | "quit"
            ),
            editor_only: ["Edit:", "Selection:", "Fold:", "Bookmarks:", "Macro:"]
                .iter()
                .any(|prefix| label.starts_with(prefix))
                || matches!(
                    id,
                    "format_selection" | "goto_bracket" | "markdown_preview" | "code_action"
                ),
            when: None,
            priority: 0,
        });
    }
}

impl App {
    pub(crate) fn shortcut_entry_index(
        bindings: &[KeyBinding],
        entry: &crate::settings::KeybindingSetting,
    ) -> Option<usize> {
        if let Some(command) = entry.command.as_deref() {
            let command = crate::app_menu::keybinding_id_for_command(command);
            entry
                .index
                .filter(|&index| {
                    bindings
                        .get(index)
                        .is_some_and(|binding| binding.id == command)
                })
                .or_else(|| bindings.iter().position(|binding| binding.id == command))
        } else {
            entry.index.filter(|&index| index < bindings.len())
        }
    }
    pub(crate) fn begin_shortcut_edit(&mut self, index: usize, settings: bool) {
        self.flush_live_settings();
        self.clear_drag_states();
        self.dismiss_lsp_hover();
        self.chord_pending = false;
        self.shortcut_chord_prefix = None;
        let bindings = Self::keybindings_for_settings(&self.selected_scope_settings());
        let Some(binding) = bindings.get(index) else {
            return;
        };
        self.shortcut_editor = Some(EditorState {
            index,
            settings,
            scope: self.settings_scope,
            current: binding.display(),
            text: String::new(),
            draft_when: None,
            recording: true,
            initial_recording: true,
            chord_mode: binding.chord.is_some(),
            first: None,
            typing: None,
            cursor: 0,
            selection: None,
            field_hit: None,
            focus: 0,
            error: None,
            areas: Vec::new(),
            owner: self
                .tabs
                .get(self.active_tab)
                .map(|tab| tab.editor.navigation_id),
            area: None,
        });
        self.settings_kb_editing = settings;
        self.keybindings_editing = !settings;
    }

    pub(crate) fn shortcut_conflicts(
        bindings: &[KeyBinding],
        index: usize,
        text: &str,
    ) -> Vec<usize> {
        let Some((first, second)) = parse_shortcut(text) else {
            return Vec::new();
        };
        if first.1 == KeyCode::Null {
            return Vec::new();
        }
        let Some(target) = bindings.get(index) else {
            return Vec::new();
        };
        if target.when.is_some()
            || normalize((target.modifiers, target.code)) == normalize(first)
                && target.chord.map(normalize) == second.map(normalize)
        {
            return Vec::new();
        }
        bindings
            .iter()
            .enumerate()
            .filter_map(|(other_index, other)| {
                if other_index == index
                    || other.when.is_some()
                    || (other.id == target.id
                        && other.chord.map(normalize) == second.map(normalize))
                    || other.code == KeyCode::Null
                    || matches!(
                        (target.id, other.id),
                        ("macro_start", "macro_stop") | ("macro_stop", "macro_start")
                    )
                {
                    return None;
                }
                (normalize((other.modifiers, other.code)) == normalize(first)
                    && (other.chord.is_none()
                        || second.is_none()
                        || other.chord.map(normalize) == second.map(normalize)))
                .then_some(other_index)
            })
            .collect()
    }

    fn finish_shortcut_edit(&mut self, edit: EditorState, save: bool) {
        if save {
            let Some(((modifiers, code), chord)) = parse_shortcut(&edit.text) else {
                self.shortcut_editor = Some(EditorState {
                    error: Some(
                        "Enter a valid shortcut, for example Ctrl+Shift+S or Ctrl+K, S.".into(),
                    ),
                    ..edit
                });
                return;
            };
            let when = match edit.draft_when.as_deref().map(When::parse).transpose() {
                Ok(when) => when,
                Err(error) => {
                    self.shortcut_editor = Some(EditorState {
                        error: Some(error),
                        ..edit
                    });
                    return;
                }
            };
            let mut bindings = Self::keybindings_for_settings(&self.settings_for_scope(edit.scope));
            if let Some(original) = bindings.get(edit.index) {
                let mut binding = original.clone();
                binding.assign(modifiers, code, chord);
                if let Some(when) = when {
                    binding.when = Some(when);
                }
                if !binding.same_keys(original) || binding.when().text() != original.when().text() {
                    let mut edited = edit.conflicts(&bindings);
                    for &index in &edited {
                        bindings[index].assign(KeyModifiers::NONE, KeyCode::Null, None);
                    }
                    bindings[edit.index] = binding;
                    edited.push(edit.index);
                    self.save_shortcut_edits(edit.scope, &bindings, &edited);
                }
            }
        }
        self.settings_kb_editing = false;
        self.keybindings_editing = false;
    }

    fn shortcut_editor_action(&mut self, mut edit: EditorState, action: EditAction) {
        match action {
            EditAction::Save => {
                self.finish_shortcut_edit(edit, true);
                return;
            }
            EditAction::Cancel => {
                self.finish_shortcut_edit(edit, false);
                return;
            }
            EditAction::Reset => {
                let previous = self.settings_scope;
                self.settings_scope = edit.scope;
                self.reset_keybinding_in_selected_scope(edit.index);
                self.settings_scope = previous;
                self.finish_shortcut_edit(edit, false);
                return;
            }
            EditAction::Record | EditAction::Chord => {
                edit.chord_mode = action == EditAction::Chord;
                edit.recording = true;
                edit.typing = None;
                edit.first = None;
                edit.text.clear();
            }
            EditAction::Type | EditAction::When => {
                edit.recording = false;
                if edit.text.is_empty() || edit.first.take().is_some() {
                    edit.text = edit.current.clone();
                }
                edit.typing = Some(if action == EditAction::When {
                    let bindings =
                        Self::keybindings_for_settings(&self.settings_for_scope(edit.scope));
                    edit.draft_when
                        .get_or_insert_with(|| bindings[edit.index].when().text().to_owned());
                    EditField::When
                } else {
                    EditField::Shortcut
                });
                edit.cursor = edit.typed_text().len();
                edit.selection = Some(0);
            }
            EditAction::Unassign => {
                edit.text = "Unassigned".into();
                edit.recording = false;
                edit.typing = None;
                edit.first = None;
            }
        }
        edit.initial_recording = false;
        edit.focus = 0;
        edit.error = None;
        self.shortcut_editor = Some(edit);
    }

    fn shortcut_editor_has_focus(&self, edit: &EditorState) -> bool {
        let focused = if edit.settings {
            self.settings_editor_has_focus() || self.settings_active
        } else {
            crate::ui_keybindings::focused(self)
        };
        focused
            && edit.owner
                == self
                    .tabs
                    .get(self.active_tab)
                    .map(|tab| tab.editor.navigation_id)
    }

    pub(crate) fn handle_shortcut_editor_key(&mut self, key: KeyEvent) -> bool {
        if blocked_by_popup(self) {
            return false;
        }
        if self.shortcut_editor.is_none()
            && ((self.settings_kb_editing
                && (self.settings_editor_has_focus() || self.settings_active))
                || (self.keybindings_editing && crate::ui_keybindings::focused(self)))
        {
            let index = if self.settings_kb_editing {
                self.settings_items()
                    .get(self.settings_selected)
                    .and_then(|(_, _, id)| id.strip_prefix("kb_")?.parse().ok())
            } else {
                Some(self.keybindings_selected)
            };
            if let Some(index) = index {
                self.begin_shortcut_edit(index, self.settings_kb_editing);
            }
        }
        let Some(mut edit) = self.shortcut_editor.take() else {
            return false;
        };
        if !self.shortcut_editor_has_focus(&edit) {
            self.finish_shortcut_edit(edit, false);
            return false;
        }
        if key.kind == crossterm::event::KeyEventKind::Release {
            self.shortcut_editor = Some(edit);
            return true;
        }
        if edit.recording && key.kind == crossterm::event::KeyEventKind::Repeat {
            self.shortcut_editor = Some(edit);
            return true;
        }
        if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            self.finish_shortcut_edit(edit, false);
            return true;
        }
        let actions = FOCUS_ACTIONS;
        if (!edit.recording || edit.initial_recording)
            && matches!(key.code, KeyCode::Tab | KeyCode::BackTab)
        {
            edit.initial_recording = false;
            edit.recording = false;
            if edit.text.is_empty() {
                edit.text = edit.current.clone();
            }
            let backwards =
                key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
            edit.focus =
                (edit.focus + if backwards { actions.len() } else { 1 }) % (actions.len() + 1);
        } else if key.code == KeyCode::Enter && key.modifiers.is_empty() && !edit.recording {
            let action = edit
                .focus
                .checked_sub(1)
                .map_or(EditAction::Save, |focus| actions[focus]);
            self.shortcut_editor_action(edit, action);
            return true;
        } else if edit.focus != 0 && key.code == KeyCode::Char(' ') {
            let action = actions[edit.focus - 1];
            self.shortcut_editor_action(edit, action);
            return true;
        } else if edit.recording || edit.typing.is_none() {
            if !matches!(key.code, KeyCode::Modifier(_) | KeyCode::Null) {
                edit.initial_recording = false;
                if edit.chord_mode && edit.first.is_none() {
                    edit.recording = true;
                    edit.first = Some((key.modifiers, key.code));
                    edit.text = format!("{}, …", display_stroke(key.modifiers, key.code));
                } else {
                    let second = display_stroke(key.modifiers, key.code);
                    edit.text = edit.first.take().map_or(second.clone(), |first| {
                        format!("{}, {second}", display_stroke(first.0, first.1))
                    });
                    edit.recording = false;
                }
            }
        } else {
            edit.edit_text(key.code, key.modifiers, None);
        }
        edit.error = None;
        self.shortcut_editor = Some(edit);
        true
    }

    pub(crate) fn paste_shortcut_text(&mut self, value: &str) -> bool {
        let Some(mut edit) = self.shortcut_editor.take() else {
            return false;
        };
        let focused = !blocked_by_popup(self) && self.shortcut_editor_has_focus(&edit);
        if focused && edit.typing.is_some() {
            edit.edit_text(KeyCode::Null, KeyModifiers::NONE, Some(value));
            edit.error = None;
        }
        self.shortcut_editor = Some(edit);
        focused
    }
    pub(crate) fn apply_shortcut_overrides(
        bindings: &mut [KeyBinding],
        entries: &[crate::settings::KeybindingSetting],
    ) {
        for (order, entry) in entries.iter().enumerate() {
            let Some(index) = Self::shortcut_entry_index(bindings, entry) else {
                continue;
            };
            let binding = &mut bindings[index];
            let when = match entry
                .when
                .as_deref()
                .map(crate::shortcut_context::When::parse)
                .transpose()
            {
                Ok(when) => when,
                Err(_) => continue,
            };
            if let Some(key) = entry.key.as_deref() {
                let Some(((modifiers, code), chord)) = parse_shortcut(key) else {
                    continue;
                };
                binding.assign(modifiers, code, chord);
            } else if let (Some(modifiers), Some(code)) =
                (entry.modifiers, entry.key_code.as_deref())
            {
                let Some(code) = Self::keycode_from_settings_token(code) else {
                    continue;
                };
                binding.assign(KeyModifiers::from_bits_truncate(modifiers), code, None);
            } else if when.is_none() {
                continue;
            }
            if let Some(when) = when {
                binding.when = Some(when);
            }
            binding.priority = order + 1;
        }
    }

    /// Prefer configured rules in settings order and preserve the order of defaults.
    pub(crate) fn resolve_shortcut(
        &self,
        first: Stroke,
        second: Option<Stroke>,
    ) -> Option<&KeyBinding> {
        let first = normalize(first);
        let context = std::cell::OnceCell::new();
        let index = self.keybindings.index();
        let find = |stroke: Stroke, shifted: bool| {
            index.current.get(&stroke)?
                .iter()
                .map(|&position| (position, &self.keybindings[position]))
                .filter(|(_, binding)| {
                    second
                            .is_none_or(|key| binding.chord.map(normalize) == Some(normalize(key)))
                        && (!shifted
                            || (!binding.uses_native_handler()
                                && binding.when.is_none()
                                && binding.chord.is_none()))
                        && binding
                            .when()
                            .matches(context.get_or_init(|| Context::new(self)))
                })
                .max_by_key(|(index, binding)| {
                    (
                        binding.priority,
                        binding.is_modified(),
                        std::cmp::Reverse(*index),
                    )
                })
                .map(|(_, binding)| binding)
        };
        find(first, false).or_else(|| {
            let (modifiers, code) = first;
            // Preserve legacy aliases where no exact binding owns this stroke.
            if second.is_none()
                && modifiers.contains(KeyModifiers::SHIFT)
                && matches!(code, KeyCode::Char(c) if c.is_ascii_alphabetic())
                && !(modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(code, KeyCode::Char('s' | 'z' | 'v')))
                && !index.current.contains_key(&first)
                && !index.defaults.contains_key(&first)
            {
                find((modifiers - KeyModifiers::SHIFT, code), true)
            } else {
                None
            }
        })
    }

    pub(crate) fn shortcut_starts_chord(&self, stroke: Stroke) -> bool {
        self.resolve_shortcut(stroke, None)
            .is_some_and(|binding| binding.chord.is_some())
    }

    pub(crate) fn lookup_keybinding(
        &self,
        modifiers: KeyModifiers,
        code: KeyCode,
    ) -> Option<&'static str> {
        self.resolve_shortcut((modifiers, code), None)
            .filter(|binding| binding.chord.is_none() && !binding.uses_native_handler())
            .map(|binding| binding.id)
    }

    pub(crate) fn shortcut_default_overridden(&self, stroke: Stroke) -> bool {
        let stroke = normalize(stroke);
        let Some(bindings) = self.keybindings.index().defaults.get(&stroke) else { return false; };
        let context = std::cell::OnceCell::new();
        bindings.iter().any(|&position| {
            let binding = &self.keybindings[position];
            binding.default_chord.is_none()
                && binding.is_modified()
                && binding
                    .default_when()
                    .matches(context.get_or_init(|| Context::new(self)))
        })
    }
}

impl EditorState {
    fn edit_text(&mut self, code: KeyCode, modifiers: KeyModifiers, paste: Option<&str>) {
        let text = match self.typing {
            Some(EditField::When) => self.draft_when.as_mut().unwrap(),
            _ => &mut self.text,
        };
        let mut input =
            crate::text_input::TextInput::new(text, &mut self.cursor, &mut self.selection);
        if let Some(value) = paste {
            input.insert_text(value);
        } else {
            input.handle_key(code, modifiers);
        }
    }

    fn typed_text(&self) -> &str {
        if self.typing == Some(EditField::When) {
            self.draft_when.as_deref().unwrap_or_default()
        } else {
            &self.text
        }
    }

    fn conflicts(&self, bindings: &[KeyBinding]) -> Vec<usize> {
        if self.draft_when.is_some() {
            Vec::new()
        } else {
            App::shortcut_conflicts(bindings, self.index, &self.text)
        }
    }
}

pub(crate) fn handle_mouse(app: &mut App, mouse: crossterm::event::MouseEvent) -> bool {
    use crossterm::event::{MouseButton, MouseEventKind};
    if blocked_by_popup(app) {
        return false;
    }
    let Some(mut edit) = app.shortcut_editor.take() else {
        return false;
    };
    if !app.shortcut_editor_has_focus(&edit) {
        app.finish_shortcut_edit(edit, false);
        return false;
    }
    app.mouse_hover_changed |= (app.mouse_col, app.mouse_row) != (mouse.column, mouse.row);
    app.mouse_col = mouse.column;
    app.mouse_row = mouse.row;
    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        if let Some(field) = &edit.field_hit
            && field.contains(mouse.column, mouse.row)
        {
            edit.cursor = field.byte_at(mouse.column, mouse.row);
            edit.selection = None;
            edit.focus = 0;
            app.shortcut_editor = Some(edit);
            return true;
        }
        if let Some(action) = edit.areas.iter().find_map(|&(area, action)| {
            area.contains((mouse.column, mouse.row).into())
                .then_some(action)
        }) {
            app.shortcut_editor_action(edit, action);
            return true;
        }
        if edit
            .area
            .is_none_or(|area| !area.contains((mouse.column, mouse.row).into()))
        {
            app.finish_shortcut_edit(edit, false);
            return false;
        }
    }
    app.shortcut_editor = Some(edit);
    true
}

#[cfg(test)]
#[path = "shortcut_editor_tests.rs"]
mod tests;

pub(crate) fn draw_editor(frame: &mut ratatui::Frame, app: &mut App) {
    use ratatui::{
        style::{Modifier, Style},
        text::{Line, Span},
        widgets::{Block, Borders, Clear, Paragraph, Wrap},
    };
    if blocked_by_popup(app) {
        return;
    }
    let Some(mut edit) = app.shortcut_editor.take() else {
        return;
    };
    if !app.shortcut_editor_has_focus(&edit) {
        app.finish_shortcut_edit(edit, false);
        return;
    }
    let bindings = App::keybindings_for_settings(&app.settings_for_scope(edit.scope));
    let Some(binding) = bindings.get(edit.index) else {
        app.finish_shortcut_edit(edit, false);
        return;
    };
    let conflicts = edit.conflicts(&bindings);
    let parsed = parse_shortcut(&edit.text);
    let valid = parsed.is_some() && !edit.recording;
    let full = frame.area();
    let width = full.width.saturating_sub(4).min(82).max(full.width.min(8));
    let compact = width < 54;
    let button_sets = [
        vec![
            (
                EditAction::Record,
                if compact { "Record" } else { "Record keys" },
            ),
            (
                EditAction::Chord,
                if compact { "Chord" } else { "Record chord" },
            ),
            (
                EditAction::Type,
                if compact { "Type" } else { "Type shortcut" },
            ),
            (EditAction::When, "When"),
        ],
        vec![
            (
                EditAction::Save,
                if conflicts.is_empty() {
                    "Save"
                } else {
                    "Replace"
                },
            ),
            (EditAction::Cancel, "Cancel"),
            (EditAction::Reset, "Reset"),
            (EditAction::Unassign, "Unassign"),
        ],
    ];
    let mut button_rows: Vec<Vec<(EditAction, String)>> = Vec::new();
    for buttons in button_sets {
        let mut row = Vec::new();
        let mut occupied = 0;
        for (action, label) in buttons {
            let label = format!("[{label}]");
            if !row.is_empty() && occupied + label.len() + 1 > width.saturating_sub(4) as usize {
                button_rows.push(std::mem::take(&mut row));
                occupied = 0;
            }
            occupied += label.len() + 1;
            row.push((action, label));
        }
        if !row.is_empty() {
            button_rows.push(row);
        }
    }
    let mut messages = vec![
        Line::from(Span::styled(
            binding.label,
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::raw(format!(
            "{} settings · Current: {}",
            if edit.scope == crate::settings::SettingsScope::Global {
                "User"
            } else {
                crate::settings::SettingsScope::Workspace.label()
            },
            edit.current
        )),
        Line::raw(""),
    ];
    let hint = if edit.recording {
        if edit.first.is_some() {
            "Press the second key combination…"
        } else if edit.chord_mode {
            "Press the first of two key combinations…"
        } else {
            "Press the new key combination…"
        }
    } else if edit.typing == Some(EditField::When) {
        "Enter a condition, e.g. terminalFocus && !terminalAltScreen"
    } else if edit.typing == Some(EditField::Shortcut) {
        "Type a shortcut, e.g. Ctrl+Shift+S or Ctrl+K, S"
    } else if !conflicts.is_empty() {
        "Enter replaces · Esc cancels · Tab moves between actions"
    } else {
        "Enter saves · Esc cancels · Tab moves between actions"
    };
    messages.push(Line::from(Span::styled(
        hint,
        Style::default().fg(app.colors.fg_dim),
    )));
    let draft = if edit.text.is_empty() {
        "Waiting for keys…".to_string()
    } else {
        parsed.map_or_else(
            || edit.text.clone(),
            |(first, second)| {
                let first = display_stroke(first.0, first.1);
                second.map_or(first.clone(), |second| {
                    format!("{first}, {}", display_stroke(second.0, second.1))
                })
            },
        )
    };
    if edit.typing != Some(EditField::Shortcut) {
        messages.push(Line::from(Span::styled(
            format!("  {draft}"),
            Style::default()
                .fg(app.colors.fg_accent_blue)
                .add_modifier(Modifier::BOLD),
        )));
    }
    if edit.typing != Some(EditField::When) {
        let when = edit
            .draft_when
            .as_deref()
            .unwrap_or_else(|| binding.when().text());
        messages.push(Line::from(Span::styled(
            format!("  When  {}", if when.is_empty() { "Always" } else { when }),
            Style::default().fg(app.colors.fg_accent_blue),
        )));
    }
    if !conflicts.is_empty() {
        messages.push(Line::raw(""));
        messages.push(Line::from(Span::styled(
            "Already assigned to:",
            Style::default().fg(app.colors.fg_warning),
        )));
        for &index in conflicts.iter().take(3) {
            messages.push(Line::raw(format!("  {}", bindings[index].label)));
        }
        if conflicts.len() > 3 {
            messages.push(Line::raw(format!("  and {} more", conflicts.len() - 3)));
        }
        messages.push(Line::raw(
            "Replace removes those assignments in this settings scope.",
        ));
    }
    if let Some(error) = &edit.error {
        messages.push(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(app.colors.fg_error),
        )));
    }
    let text_height = messages
        .iter()
        .map(|line| {
            App::wrap_setting_text_for_width(
                &line.to_string(),
                usize::from(width.saturating_sub(4)).max(1),
            )
            .len()
            .max(1)
        })
        .sum::<usize>();
    let field_height = u16::from(edit.typing.is_some());
    let height = (text_height + button_rows.len() + usize::from(field_height) + 4)
        .min(full.height.saturating_sub(2) as usize) as u16;
    let area = Rect::new(
        full.x + full.width.saturating_sub(width) / 2,
        full.y + full.height.saturating_sub(height) / 2,
        width,
        height,
    );
    edit.area = Some(area);
    frame.render_widget(Clear, area);
    let bg = app.colors.bg_sidebar;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(if edit.settings {
            " Edit keyboard shortcut "
        } else {
            " Assign keyboard shortcut "
        })
        .border_style(Style::default().fg(app.colors.fg_accent))
        .style(Style::default().fg(app.colors.fg_text).bg(bg));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let actions_height = button_rows.len().min(inner.height as usize) as u16;
    let text = Rect::new(
        inner.x + u16::from(inner.width > 2),
        inner.y,
        inner.width.saturating_sub(2),
        inner
            .height
            .saturating_sub(actions_height + field_height + 1),
    );
    frame.render_widget(
        Paragraph::new(messages)
            .style(Style::default().fg(app.colors.fg_text).bg(bg))
            .wrap(Wrap { trim: true }),
        text,
    );
    edit.field_hit = None;
    if let Some(field) = edit.typing
        && inner.height > actions_height
    {
        let input_area = Rect::new(text.x, text.bottom(), text.width, 1);
        let selection = edit
            .selection
            .map(|anchor| (anchor.min(edit.cursor), anchor.max(edit.cursor)));
        let input = crate::ui_widgets::InputField::new(edit.typed_text())
            .theme_colors(&app.colors)
            .prefix(
                if field == EditField::When {
                    "When  "
                } else {
                    "Key  "
                },
                app.colors.fg_input,
            )
            .placeholder_text(if field == EditField::When {
                "Always"
            } else {
                ""
            })
            .cursor_pos(edit.cursor)
            .selection_range(selection);
        edit.field_hit = Some(input.render(frame, input_area));
    }
    edit.areas.clear();
    let order = FOCUS_ACTIONS;
    for (row, buttons) in button_rows
        .into_iter()
        .enumerate()
        .take(actions_height as usize)
    {
        let mut x = inner.x + u16::from(inner.width > 2);
        let y = inner.bottom().saturating_sub(actions_height) + row as u16;
        for (action, label) in buttons {
            let w = label.len().min(inner.right().saturating_sub(x) as usize) as u16;
            if w == 0 {
                continue;
            }
            let button = Rect::new(x, y, w, 1);
            let layer = match edit.scope {
                crate::settings::SettingsScope::Global => &app.settings_global,
                crate::settings::SettingsScope::Workspace => &app.settings_workspace,
            };
            let enabled = match action {
                EditAction::Save => valid,
                EditAction::Reset => {
                    app.setting_is_defined_in_layer(&format!("kb_{}", edit.index), layer)
                }
                _ => true,
            };
            let focused =
                order.get(edit.focus.saturating_sub(1)) == Some(&action) && edit.focus != 0;
            let style = app.colors.ui.button_style(
                bg,
                focused || (action == EditAction::Save && valid),
                button.contains((app.mouse_col, app.mouse_row).into()),
                enabled,
            );
            frame.render_widget(Paragraph::new(label).style(style), button);
            if enabled {
                edit.areas.push((button, action));
            }
            x += w + 1;
        }
    }
    app.shortcut_editor = Some(edit);
}
