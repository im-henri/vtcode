//! Find searches the local retained grid, including output received over SSH.
//! Opening the widget is explicit: shell/application shortcuts stay with the PTY.
use crate::{
    app::{ActivePane, App},
    terminal::find::Index,
    text_input::TextInputId,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) struct Find {
    pub id: u64,
    pub open: bool,
    pub query: String,
    pub cursor: usize,
    pub anchor: Option<usize>,
    case_sensitive: bool,
    index: Index,
    history: crate::search_history::History,
}
impl Default for Find {
    fn default() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            open: false,
            query: String::new(),
            cursor: 0,
            anchor: None,
            case_sensitive: false,
            index: Index::default(),
            history: Default::default(),
        }
    }
}

#[derive(Clone, Copy)]
enum Action {
    Open,
    Previous,
    Next,
    Case,
    Close,
}
#[derive(Default)]
pub(crate) struct Controller {
    pub focused: Option<u64>,
    controls: Vec<(Rect, u64, Action)>,
    areas: Vec<Rect>,
    captured: bool,
}
impl Controller {
    pub fn begin_frame(&mut self) {
        self.controls.clear();
        self.areas.clear();
    }
}

impl App {
    pub(crate) fn terminal_find_index(&self, id: u64) -> Option<usize> {
        self.terminals
            .iter()
            .position(|terminal| terminal.find.id == id)
    }

    pub(crate) fn forget_terminal_find(&mut self, index: usize) {
        let Some(terminal) = self.terminals.get(index) else {
            return;
        };
        let id = terminal.find.id;
        if self.terminal_find.focused == Some(id) {
            self.terminal_find.focused = None;
        }
        self.remove_text_input_regions(|field| field == TextInputId::TerminalFind(id));
    }

    pub(crate) fn focused_terminal_find(&self) -> Option<u64> {
        let id = self.terminal_find.focused?;
        let index = self.terminal_find_index(id)?;
        (self.terminals[index].find.open && self.focused_terminal_index() == Some(index))
            .then_some(id)
    }

    pub(crate) fn focus_terminal_find(&mut self, id: u64) {
        let Some(index) = self.terminal_find_index(id) else {
            return;
        };
        // Context menus may target a terminal in another editor group.
        if self.terminal_is_in_editor(index) {
            if !self.tabs.iter().any(|tab| tab.terminal_idx == Some(index)) {
                if let Some(group) = self.groups.iter().enumerate().position(|(group, state)| {
                    self.group_in_current_desk(group)
                        && state.tabs.iter().any(|tab| tab.terminal_idx == Some(index))
                }) {
                    self.switch_group(group);
                }
            }
            if let Some(tab) = self
                .tabs
                .iter()
                .position(|tab| tab.terminal_idx == Some(index))
            {
                self.active_tab = tab;
                self.ensure_active_tab_visible();
            }
            self.terminal_panel_focused = false;
        } else {
            self.show_terminal_sidebar = true;
            self.terminal_sidebar_revealed = true;
            self.terminal_panel_focused = true;
        }
        self.active_terminal = index;
        self.active_pane = ActivePane::Editor;
        self.search_input_focused = false;
        self.search_pre_cursor = None;
        self.terminal_find.focused = Some(id);
    }

    pub(crate) fn begin_terminal_find(&mut self, index: usize) {
        let Some(terminal) = self.terminals.get_mut(index) else {
            return;
        };
        let find = &mut terminal.find;
        find.open = true;
        find.cursor = find.query.len();
        find.anchor = (!find.query.is_empty()).then_some(0);
        let id = find.id;
        self.focus_text_input(TextInputId::TerminalFind(id));
        self.update_terminal_find(id, true);
    }

    pub(crate) fn find_context_terminal(&mut self) {
        if let Some(index) = self.terminal_rename.context_target.take() {
            self.begin_terminal_find(index);
        }
    }

    pub(crate) fn update_terminal_find(&mut self, id: u64, reveal: bool) {
        let Some(index) = self.terminal_find_index(id) else {
            return;
        };
        let terminal = &mut self.terminals[index];
        let Some(pty) = &mut terminal.terminal_pty else {
            return;
        };
        let find = &mut terminal.find;
        find.index
            .update(&pty.grid, &find.query, find.case_sensitive);
        if reveal {
            find.index.reveal(&mut pty.grid);
        }
    }

    pub(crate) fn terminal_find_edited(&mut self, id: u64) {
        if let Some(index) = self.terminal_find_index(id) {
            self.terminals[index].find.history.edited();
        }
        self.update_terminal_find(id, true);
    }

    fn terminal_find_action(&mut self, id: u64, action: Action) {
        let Some(index) = self.terminal_find_index(id) else {
            return;
        };
        if matches!(action, Action::Open) {
            self.begin_terminal_find(index);
            return;
        }
        if matches!(action, Action::Close) {
            let find = &mut self.terminals[index].find;
            find.history.record(&find.query);
            find.open = false;
            find.index.clear();
            if self.terminal_find.focused == Some(id) {
                self.terminal_find.focused = None;
            }
            self.remove_text_input_regions(|field| field == TextInputId::TerminalFind(id));
            return;
        }
        self.focus_text_input(TextInputId::TerminalFind(id));
        if matches!(action, Action::Case) {
            self.terminals[index].find.case_sensitive ^= true;
            self.update_terminal_find(id, true);
        } else {
            self.update_terminal_find(id, false);
            let terminal = &mut self.terminals[index];
            terminal.find.history.record(&terminal.find.query);
            terminal
                .find
                .index
                .navigate(matches!(action, Action::Previous));
            if let Some(pty) = &mut terminal.terminal_pty {
                terminal.find.index.reveal(&mut pty.grid);
            }
        }
    }
}

/// Only a focused Find field owns these keys; nothing is intercepted in the PTY.
pub(crate) fn key(app: &mut App, key: KeyEvent) -> bool {
    let Some(TextInputId::TerminalFind(id)) = app.focused_text_input_id() else {
        return false;
    };
    if app.context_menu_active || app.tab_context_active || app.menu_active {
        return false;
    }
    if key.kind == crossterm::event::KeyEventKind::Release {
        return true;
    }
    match key.code {
        KeyCode::Esc => app.terminal_find_action(id, Action::Close),
        KeyCode::Enter | KeyCode::F(3) => app.terminal_find_action(
            id,
            if key.modifiers.contains(KeyModifiers::SHIFT) {
                Action::Previous
            } else {
                Action::Next
            },
        ),
        KeyCode::Up | KeyCode::Down if key.modifiers.is_empty() => {
            let index = app.terminal_find_index(id).unwrap();
            let find = &mut app.terminals[index].find;
            let query = if key.code == KeyCode::Up {
                find.history.previous(&find.query)
            } else {
                find.history.next()
            };
            if let Some(query) = query {
                find.query = query;
                find.cursor = find.query.len();
                find.anchor = None;
                app.update_terminal_find(id, true);
            }
        }
        _ => {
            let result =
                app.edit_text_input(TextInputId::TerminalFind(id), key.code, key.modifiers, None);
            if result.changed {
                app.text_input_changed(TextInputId::TerminalFind(id));
            }
        }
    }
    true
}

pub(crate) fn mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if app.terminal_find.captured
        && matches!(mouse.kind, MouseEventKind::Up(_) | MouseEventKind::Drag(_))
    {
        if matches!(mouse.kind, MouseEventKind::Up(_)) {
            app.terminal_find.captured = false;
        }
        return true;
    }
    let point = (mouse.column, mouse.row).into();
    let control = app
        .terminal_find
        .controls
        .iter()
        .rev()
        .find(|(area, _, _)| area.contains(point))
        .copied();
    let over_widget = app
        .terminal_find
        .areas
        .iter()
        .any(|area| area.contains(point));
    if let Some((_, id, action)) = control {
        if mouse.kind == MouseEventKind::Moved {
            app.mouse_hover_changed = true;
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            app.terminal_find.captured = true;
            app.terminal_find_action(id, action);
        }
        return true;
    }
    if over_widget {
        return true;
    }
    if matches!(mouse.kind, MouseEventKind::Down(_)) {
        app.terminal_find.focused = None;
    }
    false
}

pub(crate) fn draw_button(f: &mut Frame, app: &mut App, area: Rect, index: usize) {
    let Some(terminal) = app.terminals.get(index) else {
        return;
    };
    let id = terminal.find.id;
    let hovered = area.contains((app.mouse_col, app.mouse_row).into());
    f.render_widget(
        Paragraph::new(if area.width >= 8 {
            " ⌕ Find "
        } else {
            " ⌕ "
        })
        .style(app.colors.ui.toolbar_style(
            app.colors.ui.fg_panel_title,
            app.colors.ui.bg_panel,
            hovered,
            terminal.find.open,
            false,
        )),
        area,
    );
    app.terminal_find.controls.push((area, id, Action::Open));
}

pub(crate) fn draw_tooltip(f: &mut Frame, app: &App) {
    if app.context_menu_active
        || app.tab_context_active
        || app.menu_active
        || app.change_dir_active
        || crate::ui_modal::active(app).is_some()
        || crate::ui_quick_picker::active(app).is_some()
        || app.workspaces.popup.open
    {
        return;
    }
    let Some(&(area, _, action)) = app
        .terminal_find
        .controls
        .iter()
        .rev()
        .find(|(area, _, _)| area.contains((app.mouse_col, app.mouse_row).into()))
    else {
        return;
    };
    let hint = match action {
        Action::Open => "Find in Terminal",
        Action::Previous => "Previous Match (Shift+Enter)",
        Action::Next => "Next Match (Enter)",
        Action::Case => "Match Case",
        Action::Close => "Close Find (Esc)",
    };
    crate::ui_widgets::Tooltip::hover(vec![ratatui::text::Line::raw(hint)], &app.colors).render(
        f,
        area.x,
        area.bottom(),
    );
}

/// Called after terminal cells are painted. The floating widget never resizes the PTY.
pub(crate) fn draw(f: &mut Frame, app: &mut App, index: usize, area: Rect) {
    let Some(terminal) = app.terminals.get(index) else {
        return;
    };
    if !terminal.find.open || area.is_empty() {
        return;
    }
    let id = terminal.find.id;
    app.update_terminal_find(id, false);
    let terminal = &app.terminals[index];
    if let Some(pty) = &terminal.terminal_pty {
        let grid = &pty.grid;
        let top = grid.scrollback.len().saturating_sub(grid.scroll_view);
        let bottom = top + area.height as usize;
        let hits = &terminal.find.index.matches;
        let first = hits.partition_point(|hit| hit.end.row < top);
        for (offset, hit) in hits[first..]
            .iter()
            .enumerate()
            .take_while(|(_, hit)| hit.start.row < bottom)
        {
            for row in hit.start.row.max(top)..=hit.end.row.min(bottom.saturating_sub(1)) {
                let start = if row == hit.start.row {
                    hit.start.column
                } else {
                    0
                };
                let end = if row == hit.end.row {
                    hit.end.column
                } else {
                    grid.width
                };
                let selected = grid.selection_columns(row - top);
                for column in start..end.min(area.width as usize) {
                    if selected.contains(&column) {
                        continue;
                    }
                    if let Some(cell) = f
                        .buffer_mut()
                        .cell_mut((area.x + column as u16, area.y + (row - top) as u16))
                    {
                        let other = app
                            .colors
                            .ui
                            .search_match_style(cell.style())
                            .bg
                            .unwrap_or(cell.bg);
                        cell.set_style(app.colors.ui.find_style(
                            cell.style(),
                            terminal.find.index.selected == Some(first + offset),
                            other,
                        ));
                    }
                }
            }
        }
    }
    let widget = Rect::new(
        area.right().saturating_sub(area.width.min(66)),
        area.y,
        area.width.min(66),
        area.height.min(3),
    );
    let bg = app.colors.ui.bg_panel;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .title(" Find in Terminal ")
        .border_style(Style::default().fg(app.colors.fg_dim))
        .style(Style::default().bg(bg));
    let inner = block.inner(widget);
    f.render_widget(Clear, widget);
    f.render_widget(block, widget);
    app.terminal_find.areas.push(widget);
    if inner.is_empty() {
        return;
    }
    let wide = inner.width >= 38;
    let controls_width = if wide { 12 } else { 3 };
    let count_width = if wide { 12 } else { 0 };
    let input = Rect::new(
        inner.x,
        inner.y,
        inner.width.saturating_sub(controls_width + count_width),
        1,
    );
    let field_id = TextInputId::TerminalFind(id);
    let find = &app.terminals[index].find;
    let focused = app.focused_text_input_id() == Some(field_id) && !app.rendering_inactive_editor;
    let field = crate::ui_widgets::InputField::new(&find.query)
        .theme_control(&app.colors, app.colors.ui.input_colors, bg)
        .prefix("", app.colors.fg_input)
        .placeholder_text("Search terminal output…")
        .cursor_marker(if focused && app.cursor_visible {
            "│"
        } else {
            ""
        })
        .cursor_pos(find.cursor)
        .selection_range(App::inline_selection_range(find.cursor, find.anchor))
        .viewport(app.text_input_viewport(field_id))
        .render(f, input);
    let count = if find.index.error.is_some() {
        "Too long".to_owned()
    } else if find.query.is_empty() {
        String::new()
    } else if find.index.matches.is_empty() {
        "No results".to_owned()
    } else {
        format!(
            "{}/{}{}",
            find.index.selected.map_or(0, |index| index + 1),
            find.index.matches.len(),
            if find.index.limited { "+" } else { "" }
        )
    };
    let case_sensitive = find.case_sensitive;
    let no_matches = find.index.matches.is_empty();
    app.register_text_input(field_id, field);
    if wide {
        let count_area = Rect::new(input.right(), inner.y, count_width, 1);
        f.render_widget(
            Paragraph::new(count).style(
                Style::default()
                    .fg(if no_matches {
                        app.colors.fg_error
                    } else {
                        app.colors.fg_dim
                    })
                    .bg(bg),
            ),
            count_area,
        );
    }
    let start = inner.right().saturating_sub(controls_width);
    let controls: &[(Action, &str)] = if wide {
        &[
            (Action::Case, "Aa "),
            (Action::Previous, " ↑ "),
            (Action::Next, " ↓ "),
            (Action::Close, " × "),
        ]
    } else {
        &[(Action::Close, " × ")]
    };
    for (offset, &(action, label)) in controls.iter().enumerate() {
        let target = Rect::new(start + offset as u16 * 3, inner.y, 3.min(inner.width), 1);
        let hovered = target.contains((app.mouse_col, app.mouse_row).into());
        let active = matches!(action, Action::Case) && case_sensitive;
        f.render_widget(
            Paragraph::new(label).style(app.colors.ui.toolbar_style(
                app.colors.ui.fg_panel_title,
                bg,
                hovered,
                active,
                false,
            )),
            target,
        );
        app.terminal_find.controls.push((target, id, action));
    }
}

#[cfg(test)]
#[path = "app_terminal_find_tests.rs"]
mod tests;
