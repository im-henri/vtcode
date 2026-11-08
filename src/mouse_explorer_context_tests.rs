use super::*;
use crate::{
    app::{ActivePane, Tab},
    editor::Editor,
    explorer::ExplorerSortMode,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn fixture(workspace_menu: bool, x: u16, y: u16) -> (tempfile::TempDir, App) {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("main.rs"), "source\n").unwrap();
    let mut app = App::new(directory.path().to_path_buf());
    app.tabs = vec![Tab::new_file(Editor::test_with_text(
        &"background text\n".repeat(100),
    ))];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    if workspace_menu {
        app.open_explorer_workspace_context_menu(x, y);
    } else {
        app.explorer.selected = app
            .explorer
            .entries
            .iter()
            .position(|entry| entry.name == "main.rs")
            .unwrap();
        app.open_explorer_context_menu(x, y);
    }
    (directory, app)
}

fn draw(app: &mut App, width: u16, height: u16) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    app.viewport_area = area;
    app.code_area = Some(area);
    app.editor_area = Some(area);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::ui_dialogs::draw_explorer_context_menu(frame, app))
        .unwrap()
        .buffer
        .clone()
}

fn mouse(app: &mut App, kind: MouseEventKind, column: u16, row: u16) {
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
}

fn key(app: &mut App, code: KeyCode) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, KeyModifiers::NONE));
}

fn label_position(buffer: &Buffer, label: &str) -> (u16, u16) {
    for row in buffer.area.y..buffer.area.bottom() {
        for column in buffer.area.x..=buffer.area.right().saturating_sub(label.len() as u16) {
            let text: String = (column..column + label.len() as u16)
                .map(|x| buffer[(x, row)].symbol())
                .collect();
            if text == label {
                return (column, row);
            }
        }
    }
    panic!("{label} is not visible");
}

#[test]
fn explorer_context_wheel_scrolls_visible_rows_and_scrolled_actions_work() {
    for workspace_menu in [false, true] {
        for (width, height, x, y) in [(80, 18, 4, 7), (44, 12, 40, 8), (24, 8, u16::MAX, u16::MAX)]
        {
            let (_directory, mut app) = fixture(workspace_menu, x, y);
            draw(&mut app, width, height);
            let inner = app.explorer_context_inner.unwrap();
            let count = app.explorer_context_menu_items().len();
            assert!(count > inner.height as usize);
            let editor_scroll = app.editor().unwrap().scroll_offset;
            let tree_scroll = app.explorer.scroll_offset;
            let selected_file = app.explorer.selected;
            mouse(&mut app, MouseEventKind::ScrollDown, inner.x + 1, inner.y);
            assert_eq!(app.explorer_context_scroll, 3);
            draw(&mut app, width, height);
            assert_eq!(
                app.explorer_context_scroll, 3,
                "drawing must preserve manual scrolling"
            );
            for _ in 0..count {
                mouse(
                    &mut app,
                    MouseEventKind::ScrollDown,
                    inner.right() - 1,
                    inner.y,
                );
            }
            assert_eq!(app.explorer_context_scroll, count - inner.height as usize);
            let buffer = draw(&mut app, width, height);
            let (column, row) = label_position(&buffer, "Sort by Type");
            let action = app
                .explorer_context_menu_items()
                .iter()
                .position(|&label| label == "Sort by Type")
                .unwrap();
            mouse(&mut app, MouseEventKind::Moved, column, row);
            assert_eq!(app.explorer_context_selected, action);
            assert_eq!(app.editor().unwrap().scroll_offset, editor_scroll);
            assert_eq!(app.explorer.scroll_offset, tree_scroll);
            assert_eq!(app.explorer.selected, selected_file);
            assert!(!app.editor().unwrap().has_selection());
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                column,
                row,
            );
            assert!(!app.explorer_context_active);
            assert_eq!(app.explorer.sort_mode, ExplorerSortMode::Type);
        }
    }
}

#[test]
fn explorer_context_track_clicks_and_thumb_drags_keep_the_menu_open() {
    let (_directory, mut app) = fixture(false, 4, 7);
    draw(&mut app, 80, 18);
    let inner = app.explorer_context_inner.unwrap();
    let count = app.explorer_context_menu_items().len();
    let bar = DropdownScrollbar::new(inner, count, 0).unwrap();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        bar.area.x,
        bar.area.bottom() - 1,
    );
    assert_eq!(app.explorer_context_scroll, inner.height as usize);
    assert!(app.explorer_context_active);
    assert_eq!(app.explorer.sort_mode, ExplorerSortMode::Name);
    assert!(!app.creating_new_file && !app.creating_new_folder);
    draw(&mut app, 80, 18);
    let bar = DropdownScrollbar::new(inner, count, app.explorer_context_scroll).unwrap();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        bar.area.x,
        bar.area.y,
    );
    assert_eq!(
        app.explorer_context_scroll, 0,
        "clicking the track above the thumb pages up"
    );
    mouse(&mut app, MouseEventKind::ScrollDown, inner.x, inner.y);
    draw(&mut app, 80, 18);
    let bar = DropdownScrollbar::new(inner, count, app.explorer_context_scroll).unwrap();
    let grab = bar.thumb.bottom() - 1;
    let initial = app.explorer_context_scroll;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        bar.area.x + 1,
        grab,
    );
    assert!(app.explorer_context_scrollbar_drag.is_some());
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 0, grab);
    assert_eq!(
        app.explorer_context_scroll, initial,
        "grabbing or moving horizontally must not jump"
    );
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 79, 1000);
    assert_eq!(app.explorer_context_scroll, count - inner.height as usize);
    let buffer = draw(&mut app, 80, 18);
    label_position(&buffer, "Sort by Type");
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 0, 0);
    assert_eq!(app.explorer_context_scroll, 0);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 0, 0);
    assert!(app.explorer_context_active);
    assert!(app.explorer_context_scrollbar_drag.is_none());
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        bar.area.x,
        1000,
    );
    assert_eq!(
        app.explorer_context_scroll, 0,
        "release ends capture outside the menu"
    );
    assert!(!app.editor().unwrap().has_selection());
    assert!(!app.resizing_explorer && app.dragging_explorer_path.is_none());
}

#[test]
fn explorer_context_keyboard_resize_reopening_and_separators_use_current_geometry() {
    let (_directory, mut app) = fixture(false, 75, 17);
    draw(&mut app, 80, 24);
    let count = app.explorer_context_menu_items().len();
    for _ in 0..count {
        key(&mut app, KeyCode::Down);
    }
    let buffer = draw(&mut app, 80, 24);
    label_position(&buffer, "Sort by Type");
    assert_eq!(app.explorer_context_selected, count - 1);
    assert!(app.explorer_context_scroll > 0);
    draw(&mut app, 44, 12);
    let inner = app.explorer_context_inner.unwrap();
    assert_eq!(app.explorer_context_scroll, count - inner.height as usize);
    let items = app.explorer_context_menu_items();
    let separator = (app.explorer_context_scroll..count)
        .find(|&index| items[index] == "─")
        .unwrap();
    let row = inner.y + (separator - app.explorer_context_scroll) as u16;
    let selected = app.explorer_context_selected;
    mouse(&mut app, MouseEventKind::Moved, inner.x + 1, row);
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        inner.x + 1,
        row,
    );
    assert_eq!(app.explorer_context_selected, selected);
    assert!(app.explorer_context_active);
    let bar = DropdownScrollbar::new(inner, count, app.explorer_context_scroll).unwrap();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        bar.area.x,
        bar.thumb.y,
    );
    key(&mut app, KeyCode::Esc);
    assert!(app.explorer_context_scrollbar_drag.is_none());
    app.open_explorer_context_menu(4, 3);
    assert_eq!(app.explorer_context_scroll, 0);
    assert!(app.explorer_context_inner.is_none());
    draw(&mut app, 80, 12);
    let bar = DropdownScrollbar::new(app.explorer_context_inner.unwrap(), count, 0).unwrap();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        bar.area.x,
        bar.thumb.y,
    );
    assert!(app.explorer_context_scrollbar_drag.is_some());
    draw(&mut app, 2, 2);
    assert!(app.explorer_context_inner.is_none());
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 1, 1);
    assert!(app.explorer_context_scrollbar_drag.is_none());
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 1, 1);
    assert!(!app.explorer_context_active);
}

#[test]
fn explorer_context_owns_wheel_outside_the_menu_and_dismissal_does_not_click_through() {
    let (_directory, mut app) = fixture(false, 20, 5);
    draw(&mut app, 80, 18);
    mouse(&mut app, MouseEventKind::ScrollDown, 0, 0);
    assert_eq!(app.explorer_context_scroll, 0);
    assert_eq!(app.editor().unwrap().scroll_offset, 0);
    assert!(app.explorer_context_active);
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), 0, 0);
    assert!(!app.explorer_context_active);
    assert!(!app.editor().unwrap().has_selection());
    assert!(app.drag_start_pos.is_none());
    assert_eq!(app.editor().unwrap().cursor_line, 0);
    assert_eq!(app.editor().unwrap().cursor_col, 0);
}

#[test]
fn explorer_context_scrollbar_takes_priority_over_the_find_input_behind_it() {
    let (_directory, mut app) = fixture(false, 4, 3);
    app.explorer_context_active = false;
    app.show_panel = false;
    app.explorer_width = 64;
    app.open_explorer_find();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let input = crate::ui_explorer_find::layout(&app, app.explorer_area.unwrap()).input;
    app.open_explorer_context_menu(input.x, input.y.saturating_sub(1));
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let inner = app.explorer_context_inner.unwrap();
    let count = app.explorer_context_menu_items().len();
    let bar = DropdownScrollbar::new(inner, count, app.explorer_context_scroll).unwrap();
    assert!(input.contains((bar.area.x, bar.thumb.y).into()));
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        bar.area.x,
        bar.thumb.y,
    );
    assert!(app.explorer_context_scrollbar_drag.is_some());
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        bar.area.x,
        1000,
    );
    assert_eq!(app.explorer_context_scroll, count - inner.height as usize);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 0, 0);
    assert!(app.explorer_context_active);
    assert!(app.explorer_context_scrollbar_drag.is_none());
    assert!(app.explorer.find.query.is_empty());
}
