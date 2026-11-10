//! Actions stay attached to the preview and the hunk under the pointer.
use super::*;
use crate::ui_widgets::{DropdownItem, DropdownMenu, themed_dropdown_colors};
use std::borrow::Cow;

pub(super) struct Menu {
    position: (u16, u16),
    pub(super) inner: Rect,
    scroll: usize,
    selected: usize,
    old_row: Option<usize>,
}

#[derive(Clone, Copy)]
enum Action {
    Button(usize),
    CopyLine(usize),
    CopySelection,
    SelectAll,
}

struct Item {
    label: Cow<'static, str>,
    shortcut: &'static str,
    action: Action,
    enabled: bool,
    separator: bool,
}

fn items(app: &App) -> Vec<Item> {
    let Some(peek) = &app.hunk_peek else {
        return Vec::new();
    };
    let actions = buttons(app);
    let Some(copy) = actions
        .iter()
        .position(|(_, label)| *label == "Copy Old Lines")
    else {
        return Vec::new();
    };
    let mut items = vec![Item {
        label: "Copy Old Lines".into(),
        shortcut: "",
        action: Action::Button(copy),
        enabled: peek.has_old_lines(),
        separator: false,
    }];
    if let Some(index) = peek.context_menu.as_ref().and_then(|menu| menu.old_row)
        && let Some(row) = peek.rows.get(index).filter(|row| row.kind == '-')
        && let Some(line) = row.old_line
    {
        let hunk = &peek.hunks[peek.selected];
        if peek.rows[hunk.start_row..hunk.end_row]
            .iter()
            .filter(|row| row.kind == '-')
            .take(2)
            .count()
            > 1
        {
            items.push(Item {
                label: format!("Copy Old Line ({line})").into(),
                shortcut: "",
                action: Action::CopyLine(index),
                enabled: true,
                separator: false,
            });
        }
    }
    items.extend([
        Item {
            label: "Copy".into(),
            shortcut: "Ctrl+C",
            action: Action::CopySelection,
            enabled: peek.selection.has_text(),
            separator: false,
        },
        Item {
            label: "Select All".into(),
            shortcut: "Ctrl+A",
            action: Action::SelectAll,
            enabled: !peek.rows.is_empty(),
            separator: true,
        },
    ]);
    let count = if matches!(app.hunk_popup_status, 'N' | 'S') {
        1
    } else {
        2
    };
    for (index, (_, label)) in actions.iter().enumerate().take(count) {
        items.push(Item {
            label: if app.hunk_popup_status == 'S' {
                "Unstage Hunk"
            } else {
                *label
            }
            .into(),
            shortcut: "",
            action: Action::Button(index),
            separator: false,
            enabled: button_enabled(app, index) && peek.hunks[peek.selected].action_line.is_some(),
        });
    }
    items.push(Item {
        label: "Close Preview".into(),
        shortcut: "",
        action: Action::Button(actions.len() - 1),
        enabled: true,
        separator: false,
    });
    items
}

pub(super) fn open(app: &mut App, column: u16, row: u16) {
    if app.hunk_preview_feedback.is_some() {
        return;
    }
    let layout = input_layout(app);
    if !layout.panel.contains((column, row).into()) {
        return;
    }
    let Some(peek) = app.hunk_peek.as_mut() else {
        return;
    };
    let index = layout
        .body
        .contains((column, row).into())
        .then(|| peek.scroll + usize::from(row - layout.body.y));
    if let Some(index) = index
        && let Some(hunk) = peek
            .hunks
            .iter()
            .position(|hunk| (hunk.start_row..hunk.end_row).contains(&index))
    {
        peek.selected = hunk;
        if let Some(line) = peek.hunks[hunk].action_line {
            app.hunk_popup_line = line;
        }
    }
    peek.drag = None;
    peek.selection.cancel_drag();
    peek.context_menu = Some(Menu {
        position: (column, row),
        inner: Rect::default(),
        scroll: 0,
        selected: 0,
        old_row: index.filter(|&index| peek.rows.get(index).is_some_and(|row| row.kind == '-')),
    });
    let selected = items(app).iter().position(|item| item.enabled).unwrap_or(0);
    app.hunk_peek
        .as_mut()
        .unwrap()
        .context_menu
        .as_mut()
        .unwrap()
        .selected = selected;
    app.mouse_hover_changed = true;
}

fn execute(app: &mut App, index: usize) {
    let Some((action, true)) = items(app)
        .get(index)
        .map(|item| (item.action, item.enabled))
    else {
        return;
    };
    app.hunk_peek.as_mut().unwrap().context_menu = None;
    match action {
        Action::Button(index) => activate(app, index),
        Action::CopyLine(index) => selection::copy_old_line(app, index),
        Action::CopySelection => selection::copy(app, false),
        Action::SelectAll => app.hunk_peek.as_mut().unwrap().select_all(),
    }
}

pub(super) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    let Some(menu) = app
        .hunk_peek
        .as_ref()
        .and_then(|peek| peek.context_menu.as_ref())
    else {
        return false;
    };
    let current = menu.selected;
    match key.code {
        KeyCode::Esc => app.hunk_peek.as_mut().unwrap().context_menu = None,
        KeyCode::Enter | KeyCode::Char(' ') => execute(app, current),
        KeyCode::Char('c' | 'C' | 'a' | 'A')
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            let select = matches!(key.code, KeyCode::Char('a' | 'A'));
            if let Some(index) = items(app).iter().position(|item| {
                matches!(
                    (item.action, select),
                    (Action::SelectAll, true) | (Action::CopySelection, false)
                )
            }) {
                execute(app, index);
            }
        }
        KeyCode::Up
        | KeyCode::Down
        | KeyCode::Home
        | KeyCode::End
        | KeyCode::Tab
        | KeyCode::BackTab => {
            let enabled: Vec<_> = items(app)
                .iter()
                .enumerate()
                .filter(|(_, item)| item.enabled)
                .map(|(i, _)| i)
                .collect();
            if !enabled.is_empty() {
                let position = enabled
                    .iter()
                    .position(|&index| index == current)
                    .unwrap_or(0);
                let previous = key.code == KeyCode::Up
                    || key.code == KeyCode::BackTab
                    || (key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT));
                let next = match key.code {
                    KeyCode::Home => 0,
                    KeyCode::End => enabled.len() - 1,
                    _ if previous => (position + enabled.len() - 1) % enabled.len(),
                    _ => (position + 1) % enabled.len(),
                };
                app.hunk_peek
                    .as_mut()
                    .unwrap()
                    .context_menu
                    .as_mut()
                    .unwrap()
                    .selected = enabled[next];
            }
        }
        _ => {}
    }
    true
}

pub(super) fn handle_mouse(app: &mut App, mouse: MouseEvent) -> bool {
    let Some(menu) = app
        .hunk_peek
        .as_ref()
        .and_then(|peek| peek.context_menu.as_ref())
    else {
        return false;
    };
    let inner = menu.inner;
    let entries = items(app);
    let rows = entries.len() + entries.iter().filter(|item| item.separator).count();
    let scrollbar = crate::ui_widgets::DropdownScrollbar::new(inner, rows, menu.scroll);
    let item_area = Rect {
        width: inner
            .width
            .saturating_sub(scrollbar.map_or(0, |bar| bar.area.width)),
        ..inner
    };
    let mut visual_row = menu.scroll + usize::from(mouse.row.saturating_sub(inner.y));
    let hit = item_area
        .contains((mouse.column, mouse.row).into())
        .then(|| {
            for (index, item) in entries.iter().enumerate() {
                if visual_row == 0 {
                    return Some(index);
                }
                visual_row -= 1;
                if item.separator {
                    if visual_row == 0 {
                        return None;
                    }
                    visual_row -= 1;
                }
            }
            None
        })
        .flatten();
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Right) => {
            app.hunk_peek.as_mut().unwrap().context_menu = None;
            return false; // Reopen at the newly pointed-at change.
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(index) = hit {
                execute(app, index);
            } else if !Rect::new(
                inner.x.saturating_sub(1),
                inner.y.saturating_sub(1),
                inner.width.saturating_add(2),
                inner.height.saturating_add(2),
            )
            .contains((mouse.column, mouse.row).into())
            {
                app.hunk_peek.as_mut().unwrap().context_menu = None;
            }
        }
        MouseEventKind::Moved | MouseEventKind::Drag(MouseButton::Left) => {
            if let Some(index) = hit {
                let menu = app
                    .hunk_peek
                    .as_mut()
                    .unwrap()
                    .context_menu
                    .as_mut()
                    .unwrap();
                app.mouse_hover_changed |= menu.selected != index;
                menu.selected = index;
            }
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let code = if mouse.kind == MouseEventKind::ScrollUp {
                KeyCode::Up
            } else {
                KeyCode::Down
            };
            handle_key(app, KeyEvent::new(code, KeyModifiers::NONE));
        }
        MouseEventKind::Down(_) => app.hunk_peek.as_mut().unwrap().context_menu = None,
        _ => {}
    }
    true
}

pub(super) fn draw(frame: &mut Frame, app: &mut App) {
    if app
        .hunk_peek
        .as_ref()
        .is_none_or(|peek| peek.context_menu.is_none())
    {
        return;
    }
    let items = items(app);
    if items.is_empty() {
        app.hunk_peek.as_mut().unwrap().context_menu = None;
        return;
    }
    let dropdown: Vec<_> = items
        .iter()
        .map(|item| {
            let entry = DropdownItem::new(&item.label)
                .shortcut(item.shortcut)
                .disabled(!item.enabled);
            if item.separator {
                entry.separator()
            } else {
                entry
            }
        })
        .collect();
    let menu = app
        .hunk_peek
        .as_mut()
        .unwrap()
        .context_menu
        .as_mut()
        .unwrap();
    menu.inner = DropdownMenu::new(&dropdown, themed_dropdown_colors(&app.colors))
        .anchor(menu.position.0, menu.position.1)
        .selected(menu.selected)
        .mouse(app.mouse_row, app.mouse_col)
        .min_width(24)
        .render_scrolled(frame, &mut menu.scroll);
}
