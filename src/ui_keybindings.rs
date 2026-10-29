//! Keyboard Shortcuts is a workbench tab. Input follows its rendered pane,
//! while the focused tab alone can capture a shortcut assignment.
use crate::app::{ActivePane, App, TabKind};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy)]
struct View {
    editor: u64,
    area: Rect,
    list: Rect,
    first: usize,
}

#[derive(Default)]
pub(crate) struct State {
    views: Vec<View>,
    pressed: Option<MouseButton>,
}

impl State {
    pub(crate) fn begin_frame(&mut self) {
        self.views.clear();
    }
    pub(crate) fn cancel_drag(&mut self) {
        self.pressed = None;
    }
}

fn first_row(app: &App, list: Rect) -> usize {
    app.keybindings_selected
        .saturating_sub((list.height as usize).saturating_sub(1))
}

fn focused(app: &App) -> bool {
    app.active_pane == ActivePane::Editor
        && !app.docked_terminal_has_focus()
        && app
            .tabs
            .get(app.active_tab)
            .is_some_and(|tab| tab.kind == TabKind::KeyboardShortcuts)
        && !app.context_menu_active
        && !app.shortcuts_active
        && !app.settings_active
        && !app.hunk_popup_active
        && !app.lsp_code_action_active
        && !app.call_hierarchy_active
        && !app.type_hierarchy_active
        && !app.lsp_references_active
        && crate::ui_quick_picker::active(app).is_none()
        && !(app.search_active && app.search_input_focused)
}

pub(crate) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if !focused(app) {
        return false;
    }
    if app.keybindings_editing {
        if !(key.code == KeyCode::Esc && key.modifiers.is_empty()) {
            if let Some(kb) = app.keybindings.get_mut(app.keybindings_selected) {
                let message = if key.modifiers.is_empty()
                    && matches!(key.code, KeyCode::Delete | KeyCode::Backspace)
                {
                    kb.modifiers = KeyModifiers::empty();
                    kb.code = KeyCode::Null;
                    format!("Unassigned {}", kb.label)
                } else {
                    kb.modifiers = key.modifiers;
                    kb.code = key.code;
                    format!("Set {} to {}", kb.label, kb.display())
                };
                app.notify(message);
                app.rebuild_keybinding_settings_cache();
                app.save_settings();
            }
        }
        app.keybindings_editing = false;
        return true;
    }
    match key.code {
        KeyCode::Esc => app.close_tab(app.active_tab),
        KeyCode::Up => app.keybindings_selected = app.keybindings_selected.saturating_sub(1),
        KeyCode::Down => {
            app.keybindings_selected = app
                .keybindings_selected
                .saturating_add(1)
                .min(app.keybindings.len().saturating_sub(1))
        }
        KeyCode::Enter => app.keybindings_editing = !app.keybindings.is_empty(),
        _ => {}
    }
    true
}

pub(crate) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    if matches!(mouse.kind, MouseEventKind::Down(_)) {
        app.keybindings_ui.pressed = None;
    }
    if let MouseEventKind::Drag(button) | MouseEventKind::Up(button) = mouse.kind {
        if app.keybindings_ui.pressed != Some(button) {
            return false;
        }
        if matches!(mouse.kind, MouseEventKind::Up(_)) {
            app.keybindings_ui.pressed = None;
        }
        return true;
    }
    let Some(view) = app
        .keybindings_ui
        .views
        .iter()
        .find(|view| view.area.contains((mouse.column, mouse.row).into()))
        .copied()
    else {
        return false;
    };
    let Some(group) = app.visible_tab_group(view.editor, TabKind::KeyboardShortcuts) else {
        return false;
    };
    match mouse.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            app.keybindings_editing = false;
            app.keybindings_selected = if mouse.kind == MouseEventKind::ScrollUp {
                app.keybindings_selected.saturating_sub(app.scroll_speed)
            } else {
                app.keybindings_selected
                    .saturating_add(app.scroll_speed)
                    .min(app.keybindings.len().saturating_sub(1))
            };
        }
        MouseEventKind::Down(button) => {
            app.auto_save_on_focus_change();
            app.switch_group(group);
            app.active_pane = ActivePane::Editor;
            app.terminal_panel_focused = false;
            app.search_input_focused = false;
            app.clear_drag_states();
            app.drag_start_pos = None;
            app.drag_has_moved = false;
            app.keybindings_ui.pressed = Some(button);
            if button == MouseButton::Left
                && view.list.contains((mouse.column, mouse.row).into())
                && first_row(app, view.list) == view.first
            {
                let index = view.first + (mouse.row - view.list.y) as usize;
                if index < app.keybindings.len() {
                    app.keybindings_selected = index;
                    app.keybindings_editing = true;
                }
            }
        }
        MouseEventKind::Moved => {
            app.reset_hover_states();
            app.clear_transient_editor_state();
            app.mouse_hover_changed = true;
        }
        _ => {}
    }
    true
}

fn keybinding_rows(app: &App, width: u16, visible: usize, surface: Color) -> Vec<Line<'static>> {
    let selected = app.keybindings_selected;
    let scroll = selected.saturating_sub(visible.saturating_sub(1));
    app.keybindings
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .map(|(i, kb)| {
            let is_selected = i == selected;
            let editing = is_selected && app.keybindings_editing;
            let row_bg = if i % 2 == 1 {
                app.colors
                    .ui
                    .keybinding_table_row
                    .over(surface)
                    .unwrap_or(surface)
            } else {
                surface
            };
            let normal = if editing {
                app.colors
                    .ui
                    .list_focus_style(app.colors.ui.workbench_foreground, row_bg)
            } else {
                app.colors.ui.list_row_style(
                    app.colors.ui.workbench_foreground,
                    row_bg,
                    is_selected,
                    app.active_pane == crate::app::ActivePane::Editor,
                    false,
                )
            };
            let bg = normal.bg.unwrap_or(row_bg);
            let shortcut = kb.display();
            let padding = 20usize.saturating_sub(UnicodeWidthStr::width(shortcut.as_str()));
            let mut line = Line::from(vec![
                Span::styled(format!(" {:<30}", kb.label), normal),
                Span::styled(shortcut, app.colors.ui.keybinding_label_style(bg)),
                Span::styled(crate::helpers::spaces(padding), normal),
                Span::styled(
                    if editing {
                        "⌨ waiting..."
                    } else {
                        "Enter=edit"
                    },
                    normal.fg(app.colors.ui.description_color(bg)),
                ),
            ]);
            let fill = (width as usize).saturating_sub(line.width());
            line.spans
                .push(Span::styled(crate::helpers::spaces(fill), normal));
            line
        })
        .collect()
}

fn keybinding_header(app: &App, surface: Color, width: u16) -> Line<'static> {
    let bg = app
        .colors
        .ui
        .keybinding_table_header
        .over(surface)
        .unwrap_or(surface);
    let normal = Style::default()
        .fg(app.colors.ui.workbench_text_color(bg))
        .bg(bg);
    let mut line = Line::from(vec![
        Span::styled(
            format!(" {:<30}", "Command"),
            normal.add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:<20}", "Shortcut"),
            normal.add_modifier(Modifier::BOLD),
        ),
        Span::styled("Action", normal.fg(app.colors.ui.description_color(bg))),
    ]);
    let fill = (width as usize).saturating_sub(line.width());
    line.spans
        .push(Span::styled(crate::helpers::spaces(fill), normal));
    line
}

/// Draw the tab and retain only the hit areas for its visible rows.
pub(crate) fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    app.code_area = None;
    app.gutter_area = None;
    app.minimap_area = None;
    app.scrollbar_area = None;
    app.h_scrollbar_area = None;
    let bg = app.colors.bg_editor;
    let block = Block::default()
        .borders(Borders::NONE)
        .style(Style::default().bg(bg));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let list = Rect::new(
        inner.x,
        inner.y.saturating_add(3).min(inner.bottom()),
        inner.width,
        inner.height.saturating_sub(4),
    );
    if let Some(editor) = app
        .tabs
        .get(app.active_tab)
        .map(|tab| tab.editor.navigation_id)
    {
        let view = View {
            editor,
            area,
            list,
            first: first_row(app, list),
        };
        app.keybindings_ui.views.retain(|old| old.editor != editor);
        app.keybindings_ui.views.push(view);
    }
    if inner.height < 4 {
        return;
    }

    // Title
    let title_area = Rect::new(inner.x, inner.y, inner.width, 1);
    let title = if app.keybindings_editing {
        " Keyboard Shortcuts — Press new key combo... "
    } else {
        " Keyboard Shortcuts "
    };
    f.render_widget(
        Paragraph::new(Span::styled(
            title,
            Style::default()
                .fg(app.colors.ui.settings_header_color(bg))
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        )),
        title_area,
    );

    // Header
    let header_area = Rect::new(inner.x, inner.y + 1, inner.width, 1);
    let header = keybinding_header(app, bg, header_area.width);
    f.render_widget(Paragraph::new(header), header_area);

    // Separator
    let sep_area = Rect::new(inner.x, inner.y + 2, inner.width, 1);
    f.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(inner.width as usize),
            Style::default().fg(app.colors.border_color),
        )),
        sep_area,
    );

    // List
    let list_area = Rect::new(
        inner.x,
        inner.y + 3,
        inner.width,
        inner.height.saturating_sub(4),
    );
    let visible = list_area.height as usize;
    let lines = keybinding_rows(app, list_area.width, visible, bg);
    f.render_widget(
        Paragraph::new(lines).style(Style::default().bg(bg)),
        list_area,
    );

    // Hint
    let hint_area = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
    let hint = if app.keybindings_editing {
        "Press any key combo to set  │  Esc = cancel"
    } else {
        "↑↓ navigate  │  Enter = edit shortcut  │  Esc = close"
    };
    f.render_widget(
        Paragraph::new(Span::styled(
            hint,
            Style::default()
                .fg(app.colors.ui.description_color(bg))
                .bg(bg),
        )),
        hint_area,
    );
}
