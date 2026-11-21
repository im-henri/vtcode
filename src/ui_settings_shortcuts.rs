//! Settings › Keyboard Shortcuts as a one-line-per-command table.
//!
//! Shortcut rows (`kb_<index>`) and their group headings stay ordinary
//! Settings items, so search, scrolling and keyboard navigation are shared
//! with every other page. Only geometry, painting and pointer hits live here.
use crate::app::App;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

/// The Settings category that shows the shortcut table.
pub(crate) const CATEGORY: usize = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Edit,
    Reset,
    Remove,
}

/// Column rectangles for one table line. `x` positions are absolute.
struct Columns {
    command: Rect,
    shortcut: Rect,
    when: Rect,
}

fn columns(row: Rect) -> Columns {
    // Leave the first two cells for the modified/focus marker.
    let x = row.x.saturating_add(2);
    let width = row.width.saturating_sub(2);
    let shortcut = (width * 24 / 100).clamp(12.min(width), 24);
    let (command, when) = if width >= 56 {
        let command = (width * 38 / 100).clamp(18, 44);
        (command, width.saturating_sub(command + shortcut + 2))
    } else {
        (width.saturating_sub(shortcut + 1), 0)
    };
    let command_rect = Rect::new(x, row.y, command, 1);
    let shortcut_rect = Rect::new(command_rect.right() + 1, row.y, shortcut, 1);
    let when_rect = Rect::new(shortcut_rect.right() + 1, row.y, when, 1);
    Columns {
        command: command_rect,
        shortcut: shortcut_rect,
        when: when_rect,
    }
}

pub(crate) fn binding_index(id: &str) -> Option<usize> {
    id.strip_prefix("kb_")?.parse().ok()
}

pub(crate) fn is_table_item(id: &str) -> bool {
    id.starts_with("kb_") || id.starts_with("heading_kb_")
}

/// Whether the current Settings page is the shortcut table (not search results).
pub(crate) fn page_active(app: &App) -> bool {
    app.settings_category_selected == CATEGORY && app.settings_editor_search.trim().is_empty()
}

fn command_label(label: &str) -> &str {
    label.strip_prefix("Shortcut: ").unwrap_or(label)
}

fn modified(app: &App, id: &str) -> bool {
    app.setting_is_defined_in_layer(id, selected_layer(app))
}

fn selected_layer(app: &App) -> &crate::settings::Settings {
    match app.settings_scope {
        crate::settings::SettingsScope::Global => &app.settings_global,
        crate::settings::SettingsScope::Workspace => &app.settings_workspace,
    }
}

/// Hover actions, right-aligned on the row. Disabled actions are omitted so
/// every visible button does something.
fn actions(app: &App, id: &str, value: &str, row: Rect) -> Vec<(Rect, Action, &'static str)> {
    let mut wanted = vec![(Action::Edit, " Change ")];
    if modified(app, id) {
        wanted.push((Action::Reset, " Reset "));
    }
    if value != "Unassigned" {
        wanted.push((Action::Remove, " Remove "));
    }
    let total: u16 = wanted.iter().map(|(_, label)| label.len() as u16 + 1).sum();
    // Never cover the command name or the shortcut itself.
    let min_x = columns(row).shortcut.right().saturating_add(1);
    let mut x = row.right().saturating_sub(total.saturating_sub(1));
    if x < min_x {
        // Narrow rows keep only the primary action.
        wanted.truncate(1);
        x = row.right().saturating_sub(wanted[0].1.len() as u16);
        if x < min_x {
            return Vec::new();
        }
    }
    wanted
        .into_iter()
        .map(|(action, label)| {
            let rect = Rect::new(x, row.y, label.len() as u16, 1);
            x = rect.right() + 1;
            (rect, action, label)
        })
        .collect()
}

fn cell(text: &str, width: u16) -> String {
    let text = crate::ui_pickers::truncate_to_width(text, width as usize);
    let pad = (width as usize).saturating_sub(UnicodeWidthStr::width(text.as_str()));
    format!("{text}{}", crate::helpers::spaces(pad))
}

/// The column header shown above the table on the Keyboard Shortcuts page.
pub(crate) fn draw_header(f: &mut Frame, app: &App, row: Rect) {
    let bg = app.colors.bg_editor;
    let header_bg = app.colors.ui.keybinding_table_header.over(bg).unwrap_or(bg);
    let style = Style::default()
        .fg(app.colors.ui.description_color(header_bg))
        .bg(header_bg)
        .add_modifier(Modifier::BOLD);
    f.render_widget(Paragraph::new("").style(style), row);
    let cols = columns(row);
    for (rect, text) in [
        (cols.command, "Command"),
        (cols.shortcut, "Shortcut"),
        (cols.when, "When"),
    ] {
        if rect.width > 0 {
            f.render_widget(Paragraph::new(cell(text, rect.width)).style(style), rect);
        }
    }
}

/// A group heading: a short bold label followed by a faint rule.
pub(crate) fn draw_heading(f: &mut Frame, app: &App, row: Rect, label: &str) {
    let bg = app.colors.bg_editor;
    let text = label.trim_matches(['─', ' ']);
    let rule = crate::app_theme::lerp_color(bg, app.colors.border_color, 0.45);
    let used = UnicodeWidthStr::width(text) + 3;
    let line = Line::from(vec![
        Span::styled("  ", Style::default().bg(bg)),
        Span::styled(
            text.to_owned(),
            Style::default()
                .fg(app.colors.ui.settings_header_color(bg))
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {}", "─".repeat((row.width as usize).saturating_sub(used))),
            Style::default().fg(rule).bg(bg),
        ),
    ]);
    f.render_widget(Paragraph::new(line), row);
}

/// One shortcut row. `focused` means keyboard focus is on this row.
pub(crate) fn draw_row(
    f: &mut Frame,
    app: &App,
    row: Rect,
    label: &str,
    value: &str,
    id: &str,
    focused: bool,
    hovered: bool,
) {
    let bg = app.colors.bg_editor;
    let editing = focused && app.settings_kb_editing;
    let striped = binding_index(id).is_some_and(|index| index % 2 == 1);
    let base = if striped {
        app.colors.ui.keybinding_table_row.over(bg).unwrap_or(bg)
    } else {
        bg
    };
    let row_bg = if focused {
        app.colors.ui.bg_settings_focus
    } else if hovered {
        app.colors.ui.bg_settings_hover
    } else {
        base
    };
    let fg =
        app.colors
            .ui
            .readable_foreground(app.colors.ui.workbench_text_color(row_bg), row_bg, 4.5);
    let dim = app.colors.ui.description_color(row_bg);
    let is_modified = modified(app, id);
    f.render_widget(Paragraph::new("").style(Style::default().bg(row_bg)), row);
    if focused || is_modified {
        let color = if focused {
            app.colors.fg_accent
        } else {
            app.colors.fg_warning
        };
        f.render_widget(
            Paragraph::new("▏").style(Style::default().fg(color).bg(row_bg)),
            Rect::new(row.x, row.y, 1, 1),
        );
    }
    let cols = columns(row);
    // "File: Save All" → dim "File: " so the command name leads.
    let name = command_label(label);
    let (prefix, name) = match name.split_once(": ") {
        Some((prefix, rest)) if !prefix.contains(' ') => (format!("{prefix}: "), rest),
        _ => (String::new(), name),
    };
    let prefix = crate::ui_pickers::truncate_to_width(&prefix, cols.command.width as usize);
    let prefix_width = UnicodeWidthStr::width(prefix.as_str()) as u16;
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(prefix, Style::default().fg(dim).bg(row_bg)),
            Span::styled(
                cell(name, cols.command.width.saturating_sub(prefix_width)),
                Style::default().fg(fg).bg(row_bg),
            ),
        ])),
        cols.command,
    );

    // The shortcut is drawn as a key cap so it reads as the clickable value.
    let keys = if editing { "Press keys…" } else { value };
    let key_style = if value == "Unassigned" && !editing {
        Style::default()
            .fg(dim)
            .bg(row_bg)
            .add_modifier(Modifier::ITALIC)
    } else {
        app.colors.ui.keybinding_label_style(row_bg)
    };
    let key_text = if value == "Unassigned" && !editing {
        "—".to_owned()
    } else {
        format!(" {keys} ")
    };
    let key_text = crate::ui_pickers::truncate_to_width(&key_text, cols.shortcut.width as usize);
    let key_width = UnicodeWidthStr::width(key_text.as_str()) as u16;
    f.render_widget(
        Paragraph::new(key_text).style(key_style),
        Rect::new(
            cols.shortcut.x,
            row.y,
            key_width.min(cols.shortcut.width),
            1,
        ),
    );

    let buttons = if hovered || focused {
        actions(app, id, value, row)
    } else {
        Vec::new()
    };
    let mut when_end = buttons
        .first()
        .map_or(row.right(), |(rect, _, _)| rect.x.saturating_sub(1));
    if buttons.is_empty() && is_modified && row.width >= 70 {
        let note = "Modified";
        let note_x = row.right().saturating_sub(note.len() as u16 + 1);
        f.render_widget(
            Paragraph::new(note).style(Style::default().fg(dim).bg(row_bg)),
            Rect::new(note_x, row.y, note.len() as u16, 1),
        );
        when_end = note_x.saturating_sub(1);
    }
    let when_width = when_end.saturating_sub(cols.when.x).min(cols.when.width);
    if when_width > 0 {
        let when = binding_index(id)
            .and_then(|index| app.keybindings.get(index))
            .map(|binding| binding.when().text().to_owned())
            .unwrap_or_default();
        f.render_widget(
            Paragraph::new(cell(&when, when_width)).style(Style::default().fg(dim).bg(row_bg)),
            Rect::new(cols.when.x, row.y, when_width, 1),
        );
    }
    for (rect, _, text) in buttons {
        let hot = rect.contains((app.mouse_col, app.mouse_row).into());
        let style = app.colors.ui.button_style(row_bg, false, hot, true);
        f.render_widget(Paragraph::new(text).style(style), rect);
    }
}

/// What a left click at `col` on a shortcut row should do.
pub(crate) fn hit(app: &App, id: &str, value: &str, row: Rect, col: u16) -> Option<Action> {
    if let Some((_, action, _)) = actions(app, id, value, row)
        .into_iter()
        .find(|(rect, _, _)| rect.contains((col, row.y).into()))
    {
        return Some(action);
    }
    let cols = columns(row);
    (col >= cols.shortcut.x && col < cols.shortcut.right()).then_some(Action::Edit)
}

impl App {
    pub(crate) fn run_shortcut_action(&mut self, index: usize, action: Action) {
        match action {
            Action::Edit => self.begin_shortcut_edit(index, true),
            Action::Reset => {
                self.reset_keybinding_in_selected_scope(index);
            }
            Action::Remove => self.unassign_keybinding_in_selected_scope(index),
        }
        self.ensure_settings_selection_visible();
    }

    /// Unassign a binding and persist that in the selected settings scope.
    pub(crate) fn unassign_keybinding_in_selected_scope(&mut self, index: usize) {
        let id = format!("kb_{index}");
        self.flush_live_settings();
        let scoped_before = self.selected_scope_settings();
        self.apply_setting_value(&id, &scoped_before);
        let mut message = None;
        if let Some(binding) = self.keybindings.get_mut(index)
            && (binding.code != KeyCode::Null || !binding.modifiers.is_empty())
        {
            binding.assign(KeyModifiers::empty(), KeyCode::Null, None);
            message = Some(format!("Removed shortcut for {}", binding.label));
        }
        if let Some(message) = message {
            self.notify(message);
            self.rebuild_keybinding_settings_cache();
            self.persist_scoped_setting_change(&id, &scoped_before);
        } else {
            self.apply_effective_settings();
        }
    }

    /// Show Settings › Keyboard Shortcuts (File › Preferences, command palette).
    pub(crate) fn open_keyboard_shortcuts_settings(&mut self) {
        self.terminal_panel_focused = false;
        self.open_settings_tab();
        self.settings_category_area = None;
        self.select_settings_page(CATEGORY, None);
        self.settings_focus_categories = false;
        self.settings_focus_control = true;
        self.ensure_settings_selection_visible();
    }
}
