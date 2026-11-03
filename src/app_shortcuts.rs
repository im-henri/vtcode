//! Shared shortcut definitions, persistence and the assignment dialog.
use crate::app::{ActivePane, App, KeyBinding, TabKind};
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
}

const FOCUS_ACTIONS: [EditAction; 7] = [
    EditAction::Record,
    EditAction::Chord,
    EditAction::Type,
    EditAction::Save,
    EditAction::Cancel,
    EditAction::Reset,
    EditAction::Unassign,
];

pub(crate) struct EditorState {
    index: usize,
    settings: bool,
    scope: crate::settings::SettingsScope,
    current: String,
    text: String,
    recording: bool,
    chord_mode: bool,
    first: Option<Stroke>,
    typing: bool,
    select_all: bool,
    focus: usize,
    error: Option<String>,
    areas: Vec<(Rect, EditAction)>,
    owner: Option<u64>,
    area: Option<Rect>,
}

#[path = "shortcut_defaults.rs"]
mod defaults;

pub(crate) type Stroke = (KeyModifiers, KeyCode);

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
            built_in_handler: true,
            editor_only: ["Edit:", "Selection:", "Fold:", "Bookmarks:", "Macro:"]
                .iter()
                .any(|prefix| label.starts_with(prefix))
                || matches!(
                    id,
                    "format_selection" | "goto_bracket" | "markdown_preview" | "code_action"
                ),
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
            recording: true,
            chord_mode: binding.chord.is_some(),
            first: None,
            typing: false,
            select_all: false,
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
        if normalize((target.modifiers, target.code)) == normalize(first)
            && target.chord.map(normalize) == second.map(normalize)
        {
            return Vec::new();
        }
        bindings
            .iter()
            .enumerate()
            .filter_map(|(other_index, other)| {
                if other_index == index
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
            let previous_scope = self.settings_scope;
            self.settings_scope = edit.scope;
            let before = self.selected_scope_settings();
            if Self::keybindings_for_settings(&before)
                .get(edit.index)
                .is_some_and(|binding| {
                    normalize((binding.modifiers, binding.code)) == normalize((modifiers, code))
                        && binding.chord.map(normalize) == chord.map(normalize)
                })
            {
                self.settings_scope = previous_scope;
                self.settings_kb_editing = false;
                self.keybindings_editing = false;
                return;
            }
            let setting_id = format!("kb_{}", edit.index);
            self.apply_setting_value(&setting_id, &before);
            let conflicts = Self::shortcut_conflicts(&self.keybindings, edit.index, &edit.text);
            for index in conflicts {
                self.keybindings[index].assign(KeyModifiers::NONE, KeyCode::Null, None);
            }
            if let Some(binding) = self.keybindings.get_mut(edit.index) {
                binding.assign(modifiers, code, chord);
                let message = format!("{}: {}", binding.label, binding.display());
                self.rebuild_keybinding_settings_cache();
                self.persist_scoped_setting_change(&setting_id, &before);
                self.notify(message);
            }
            self.settings_scope = previous_scope;
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
                edit.typing = false;
                edit.first = None;
                edit.text.clear();
            }
            EditAction::Type => {
                edit.recording = false;
                edit.typing = true;
                if edit.text.is_empty() {
                    edit.text = edit.current.clone();
                }
                edit.select_all = true;
            }
            EditAction::Unassign => {
                edit.text = "Unassigned".into();
                edit.recording = false;
                edit.typing = false;
                edit.first = None;
            }
        }
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
        if !edit.recording && matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
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
        } else if edit.recording || !edit.typing {
            if !matches!(key.code, KeyCode::Modifier(_) | KeyCode::Null) {
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
        } else if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('a' | 'A'))
        {
            edit.select_all = true;
        } else if matches!(key.code, KeyCode::Backspace | KeyCode::Delete) {
            if edit.select_all {
                edit.text.clear();
            } else {
                edit.text.pop();
            }
            edit.select_all = false;
        } else if let KeyCode::Char(c) = key.code
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            if edit.select_all {
                edit.text.clear();
                edit.select_all = false;
            }
            if edit.text.len() < 160 {
                edit.text.push(c);
            }
        }
        edit.error = None;
        self.shortcut_editor = Some(edit);
        true
    }
    pub(crate) fn apply_shortcut_overrides(
        bindings: &mut [KeyBinding],
        entries: &[crate::settings::KeybindingSetting],
    ) {
        for entry in entries {
            if let (Some(_), Some(key)) = (entry.command.as_deref(), entry.key.as_deref())
                && let Some(((modifiers, code), chord)) = parse_shortcut(key)
                && let Some(index) = Self::shortcut_entry_index(bindings, entry)
            {
                bindings[index].assign(modifiers, code, chord);
                continue;
            }
            if let (None, Some(index), Some(modifiers), Some(code)) = (
                &entry.command,
                entry.index,
                entry.modifiers,
                entry.key_code.as_deref(),
            ) && let (Some(binding), Some(code)) = (
                bindings.get_mut(index),
                Self::keycode_from_settings_token(code),
            ) {
                binding.assign(KeyModifiers::from_bits_truncate(modifiers), code, None);
            }
        }
    }
    pub(crate) fn shortcut_applies(&self, binding: &KeyBinding) -> bool {
        if binding.id == "macro_start" && self.macro_recording {
            return false;
        }
        if binding.id == "macro_stop" && !self.macro_recording {
            return false;
        }
        !binding.editor_only
            || (self.active_pane == ActivePane::Editor
                && self.tabs.get(self.active_tab).is_some_and(|tab| {
                    matches!(
                        tab.kind,
                        TabKind::File | TabKind::GitRevision | TabKind::Diff(_)
                    )
                }))
    }

    pub(crate) fn shortcut_starts_chord(&self, stroke: Stroke) -> bool {
        self.keybindings.iter().any(|binding| {
            binding.chord.is_some()
                && self.shortcut_applies(binding)
                && normalize((binding.modifiers, binding.code)) == normalize(stroke)
        })
    }

    pub(crate) fn lookup_shortcut_chord(
        &self,
        first: Stroke,
        second: Stroke,
    ) -> Option<&'static str> {
        self.keybindings
            .iter()
            .find(|binding| {
                self.shortcut_applies(binding)
                    && normalize((binding.modifiers, binding.code)) == normalize(first)
                    && binding.chord.map(normalize) == Some(normalize(second))
            })
            .map(|binding| binding.id)
    }

    pub(crate) fn shortcut_default_overridden(
        &self,
        first: Stroke,
        second: Option<Stroke>,
    ) -> bool {
        self.keybindings.iter().any(|binding| {
            binding.is_modified()
                && self.shortcut_applies(binding)
                && normalize((binding.default_modifiers, binding.default_code)) == normalize(first)
                && binding.default_chord.map(normalize) == second.map(normalize)
        })
    }

    pub(crate) fn remaining_default_shortcut(&self, first: Stroke) -> Option<&'static str> {
        self.keybindings
            .iter()
            .find(|binding| {
                !binding.is_modified()
                    && binding.chord.is_none()
                    && binding.built_in_handler
                    && self.shortcut_applies(binding)
                    && normalize((binding.modifiers, binding.code)) == normalize(first)
            })
            .map(|binding| binding.id)
    }
}

pub(crate) fn handle_mouse(app: &mut App, mouse: crossterm::event::MouseEvent) -> bool {
    use crossterm::event::{MouseButton, MouseEventKind};
    if blocked_by_popup(app) {
        return false;
    }
    let Some(edit) = app.shortcut_editor.take() else {
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
    let conflicts = App::shortcut_conflicts(&bindings, edit.index, &edit.text);
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
    } else if edit.typing {
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
    messages.push(Line::from(Span::styled(
        format!("  {draft}{}", if edit.typing { " ▏" } else { "" }),
        Style::default()
            .fg(app.colors.fg_accent_blue)
            .add_modifier(Modifier::BOLD),
    )));
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
    let height =
        (text_height + button_rows.len() + 4).min(full.height.saturating_sub(2) as usize) as u16;
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
        inner.height.saturating_sub(actions_height + 1),
    );
    frame.render_widget(
        Paragraph::new(messages)
            .style(Style::default().fg(app.colors.fg_text).bg(bg))
            .wrap(Wrap { trim: true }),
        text,
    );
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
