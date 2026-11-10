use super::tests::{app_with_peek, mouse};
use super::*;
use crate::{
    app::{ActivePane, Tab},
    editor::Editor,
};
use ratatui::{Terminal, backend::TestBackend};

fn fixture() -> App {
    let mut app = app_with_peek();
    app.tabs = vec![Tab::new_file(Editor::test_with_text("underlying editor"))];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app
}

fn render(app: &mut App) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
    terminal.draw(|frame| draw(frame, app)).unwrap();
    terminal
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}

fn right_click(app: &mut App, column: u16, row: u16) {
    mouse(
        app,
        MouseEventKind::Down(MouseButton::Right),
        column,
        row,
        KeyModifiers::NONE,
    );
    mouse(
        app,
        MouseEventKind::Up(MouseButton::Right),
        column,
        row,
        KeyModifiers::NONE,
    );
}

fn menu_item(app: &mut App, label: &str) -> (u16, u16) {
    let terminal = render(app);
    let inner = app
        .hunk_peek
        .as_ref()
        .unwrap()
        .context_menu
        .as_ref()
        .unwrap()
        .inner;
    for row in inner.y..inner.bottom() {
        let text: String = (inner.x..inner.right())
            .map(|column| terminal.backend().buffer()[(column, row)].symbol())
            .collect();
        if text
            .trim_start()
            .strip_prefix(label)
            .is_some_and(|tail| tail.trim().is_empty() || tail.starts_with("  "))
        {
            return (inner.x + 1, row);
        }
    }
    panic!("menu item was not visible: {label}");
}

fn choose(app: &mut App, label: &str) {
    let (column, row) = menu_item(app, label);
    mouse(
        app,
        MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        KeyModifiers::NONE,
    );
    mouse(
        app,
        MouseEventKind::Up(MouseButton::Left),
        column,
        row,
        KeyModifiers::NONE,
    );
}

#[test]
fn nearby_copy_buttons_target_their_own_hunks_and_leave_the_editor_unchanged() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for status in ['M', 'S', 'N'] {
        let mut app = fixture();
        app.hunk_popup_status = status;
        if status == 'S' {
            app.hunk_peek.as_mut().unwrap().hunks[1].action_line = None;
        }
        let terminal = render(&mut app);
        let peek = app.hunk_peek.as_ref().unwrap();
        let layout = peek.layout.unwrap();
        let copy = buttons(&app)
            .iter()
            .position(|(_, label)| *label == "Copy Old Lines")
            .unwrap();
        assert!(
            button_rect(&app, layout.panel, copy, buttons(&app)).is_empty(),
            "copy should move out of the far-right toolbar"
        );
        let actions: Vec<_> = quick_actions(peek, layout, status != 'M').collect();
        let copies: Vec<_> = actions
            .iter()
            .filter(|action| action.button == copy)
            .copied()
            .collect();
        assert_eq!(copies.len(), 2);
        for action in &copies {
            let previous = actions
                .iter()
                .filter(|other| other.hunk == action.hunk && other.area.x < action.area.x)
                .last()
                .unwrap();
            assert_eq!(previous.area.right(), action.area.x);
            assert_eq!(previous.area.y, action.area.y);
            assert_eq!(
                terminal.backend().buffer()[(action.area.x + 1, action.area.y)].symbol(),
                "⧉"
            );
        }
        for (action, expected) in [
            (copies[1], "last_old();\n"),
            (copies[0], "let old = 1;\nremoved();\n"),
        ] {
            mouse(
                &mut app,
                MouseEventKind::Moved,
                action.area.x + 1,
                action.area.y,
                KeyModifiers::NONE,
            );
            let hovered = render(&mut app);
            let title: String = (layout.panel.x..layout.panel.right())
                .map(|x| hovered.backend().buffer()[(x, layout.panel.y + 1)].symbol())
                .collect();
            assert!(title.contains("Copy Old Lines"));
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                action.area.x + 1,
                action.area.y,
                KeyModifiers::NONE,
            );
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                action.area.x + 1,
                action.area.y,
                KeyModifiers::NONE,
            );
            assert_eq!(crate::clipboard::get_text().unwrap(), expected);
            assert!(app.hunk_popup_active);
            assert_eq!(app.editor().unwrap().rope.to_string(), "underlying editor");
        }
    }
}

#[test]
fn nearby_copy_uses_the_clicked_hunks_availability_even_when_an_addition_is_selected() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let mut app = fixture();
    app.hunk_popup_status = 'N';
    app.hunk_peek = Some(
        HunkPeek::from_texts(
            "one\ntwo\nthree\nfour\nfive\nold\n",
            "added\none\ntwo\nthree\nfour\nfive\nnew\n",
            0,
            4,
        )
        .unwrap(),
    );
    render(&mut app);
    let peek = app.hunk_peek.as_ref().unwrap();
    assert_eq!(peek.hunks.len(), 2);
    assert!(!peek.has_old_lines());
    let copy = buttons(&app)
        .iter()
        .position(|(_, label)| *label == "Copy Old Lines")
        .unwrap();
    let actions: Vec<_> = quick_actions(peek, peek.layout.unwrap(), true).collect();
    let addition = actions
        .iter()
        .find(|action| action.hunk == 0 && action.button == copy)
        .unwrap();
    let replacement = actions
        .iter()
        .find(|action| action.hunk == 1 && action.button == copy)
        .unwrap();
    crate::clipboard::set_text("keep").unwrap();
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        addition.area.x + 1,
        addition.area.y,
        KeyModifiers::NONE,
    );
    assert_eq!(crate::clipboard::get_text().unwrap(), "keep");
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        replacement.area.x + 1,
        replacement.area.y,
        KeyModifiers::NONE,
    );
    assert_eq!(crate::clipboard::get_text().unwrap(), "old\n");
    assert_eq!(app.hunk_peek.as_ref().unwrap().selected, 1);
}

#[test]
fn right_click_menu_copies_the_clicked_old_block_and_single_old_line() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let mut app = fixture();
    render(&mut app);
    let layout = input_layout(&app);
    let first = app.hunk_peek.as_ref().unwrap().hunks[0].start_row;
    let second = app.hunk_peek.as_ref().unwrap().hunks[1].start_row;
    right_click(&mut app, layout.code.x + 1, layout.body.y + second as u16);
    assert_eq!(app.hunk_peek.as_ref().unwrap().selected, 1);
    assert_eq!(app.hunk_popup_line, 6);
    choose(&mut app, "Copy Old Lines");
    assert_eq!(crate::clipboard::get_text().unwrap(), "last_old();\n");
    assert!(app.hunk_popup_active);
    assert!(app.hunk_peek.as_ref().unwrap().context_menu.is_none());
    right_click(
        &mut app,
        layout.code.x + 1,
        layout.body.y + first as u16 + 1,
    );
    choose(&mut app, "Copy Old Line (3)");
    assert_eq!(crate::clipboard::get_text().unwrap(), "removed();");
    assert!(app.hunk_popup_active);
    assert_eq!(app.editor().unwrap().rope.to_string(), "underlying editor");
}

#[test]
fn context_menu_copy_preserves_selection_and_escape_only_dismisses_the_menu() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let mut app = fixture();
    render(&mut app);
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    let selected = app.hunk_peek.as_ref().unwrap().selected_text().unwrap();
    let layout = input_layout(&app);
    right_click(&mut app, layout.code.x, layout.body.y + 1);
    assert_eq!(
        app.hunk_peek.as_ref().unwrap().selected_text().unwrap(),
        selected
    );
    choose(&mut app, "Copy");
    assert_eq!(crate::clipboard::get_text().unwrap(), selected);
    right_click(&mut app, layout.quick_actions.x, layout.body.y + 1);
    render(&mut app);
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.hunk_popup_active);
    assert!(app.hunk_peek.as_ref().unwrap().context_menu.is_none());
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!app.hunk_popup_active);
}

#[test]
fn context_menu_disabled_copy_and_outside_click_cannot_trigger_the_editor() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let mut app = fixture();
    app.hunk_popup_status = 'N';
    app.hunk_peek = Some(HunkPeek::from_texts("", "added\n", 0, 4).unwrap());
    render(&mut app);
    let layout = input_layout(&app);
    crate::clipboard::set_text("keep").unwrap();
    right_click(&mut app, layout.code.x, layout.body.y);
    choose(&mut app, "Copy Old Lines");
    assert_eq!(crate::clipboard::get_text().unwrap(), "keep");
    assert!(app.hunk_peek.as_ref().unwrap().context_menu.is_some());
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        0,
        0,
        KeyModifiers::NONE,
    );
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        0,
        0,
        KeyModifiers::NONE,
    );
    assert!(app.hunk_popup_active);
    assert!(app.hunk_peek.as_ref().unwrap().context_menu.is_none());
    assert_eq!(app.editor().unwrap().rope.to_string(), "underlying editor");
}

#[test]
fn keyboard_context_menu_can_copy_and_select_all_without_changing_the_document() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let mut app = fixture();
    render(&mut app);
    key(&mut app, KeyCode::F(10), KeyModifiers::SHIFT);
    assert!(app.hunk_peek.as_ref().unwrap().context_menu.is_some());
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(
        crate::clipboard::get_text().unwrap(),
        "let old = 1;\nremoved();\n"
    );
    key(&mut app, KeyCode::F(10), KeyModifiers::SHIFT);
    key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    assert!(app.hunk_peek.as_ref().unwrap().selected_text().is_some());
    key(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert!(
        crate::clipboard::get_text()
            .unwrap()
            .contains("last_old();")
    );
    assert!(app.hunk_popup_active);
    assert_eq!(app.editor().unwrap().rope.to_string(), "underlying editor");
}

#[test]
fn context_menu_revert_uses_the_pointed_at_unsaved_hunk() {
    let mut app = fixture();
    app.tabs[0].editor =
        Editor::test_with_text("first\nold\nthird\nfourth\nfifth\nsixth\nlast old\nend\n");
    app.tabs[0].editor.saved_rope = Some(app.tabs[0].editor.rope.clone());
    app.tabs[0].editor.file_path = Some("example.rs".into());
    app.tabs[0].editor.rope =
        ropey::Rope::from_str("first\nnew\nthird\nfourth\nfifth\nsixth\nlast new\nend\n");
    app.tabs[0].editor.modified = true;
    app.open_hunk_popup(10, 4, Some(std::path::PathBuf::from("example.rs")), 1, 'N');
    app.wait_for_hunk_preview();
    render(&mut app);
    let layout = input_layout(&app);
    let peek = app.hunk_peek.as_ref().unwrap();
    let row = peek.hunks[1].start_row - peek.scroll;
    right_click(&mut app, layout.code.x, layout.body.y + row as u16);
    choose(&mut app, "Revert Unsaved Hunk");
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "first\nnew\nthird\nfourth\nfifth\nsixth\nlast old\nend\n"
    );
}

#[test]
fn single_old_line_copy_preserves_unicode_tabs_and_empty_line_endings() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let mut app = fixture();
    app.hunk_popup_status = 'N';
    app.hunk_peek =
        Some(HunkPeek::from_texts("α\t界\r\n\r\nlast\r\n", "replacement\r\n", 0, 4).unwrap());
    render(&mut app);
    let layout = input_layout(&app);
    for (row, label, expected) in [
        (0, "Copy Old Line (1)", "α\t界"),
        (1, "Copy Old Line (2)", "\r\n"),
    ] {
        right_click(&mut app, layout.code.x, layout.body.y + row);
        choose(&mut app, label);
        assert_eq!(crate::clipboard::get_text().unwrap(), expected);
    }
}

#[test]
fn context_menu_stays_within_the_viewport_after_resize() {
    for (width, height) in [(110, 32), (32, 12), (18, 8), (12, 4), (1, 1)] {
        let mut app = fixture();
        render(&mut app);
        let layout = input_layout(&app);
        right_click(&mut app, layout.code.right() - 1, layout.body.bottom() - 1);
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        if let Some(menu) = app.hunk_peek.as_ref().unwrap().context_menu.as_ref() {
            assert!(menu.inner.right() <= width && menu.inner.bottom() <= height);
        }
    }
}
