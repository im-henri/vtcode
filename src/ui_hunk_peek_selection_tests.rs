use super::tests::{app_with_peek, long_peek, mouse};
use super::*;
use crate::{
    app::{ActivePane, Tab},
    editor::Editor,
};
use ratatui::{Terminal, backend::TestBackend};

fn keyboard(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}

fn render(app: &mut App) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
    terminal.draw(|frame| draw(frame, app)).unwrap();
    terminal
}

fn action(app: &mut App, label: &str) {
    let index = buttons(app)
        .iter()
        .position(|(_, action)| *action == label)
        .unwrap();
    let area = button_rect(input_layout(app).panel, index, buttons(app));
    mouse(
        app,
        MouseEventKind::Down(MouseButton::Left),
        area.x + 1,
        area.y,
        KeyModifiers::NONE,
    );
    mouse(
        app,
        MouseEventKind::Up(MouseButton::Left),
        area.x + 1,
        area.y,
        KeyModifiers::NONE,
    );
}

fn gesture(app: &mut App, start: (u16, u16), end: (u16, u16)) {
    mouse(
        app,
        MouseEventKind::Down(MouseButton::Left),
        start.0,
        start.1,
        KeyModifiers::NONE,
    );
    mouse(
        app,
        MouseEventKind::Drag(MouseButton::Left),
        end.0,
        end.1,
        KeyModifiers::NONE,
    );
    mouse(
        app,
        MouseEventKind::Up(MouseButton::Left),
        end.0,
        end.1,
        KeyModifiers::NONE,
    );
}

fn preview(old: &str, new: &str) -> App {
    let mut app = app_with_peek();
    app.hunk_peek = Some(HunkPeek::from_texts(old, new, 0, 4).unwrap());
    app.hunk_popup_status = 'N';
    app.tabs = vec![Tab::new_file(Editor::test_with_text("underlying editor"))];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app
}

#[test]
fn copy_old_lines_button_uses_active_hunk_and_keeps_preview_open() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for status in ['M', 'S', 'N'] {
        let mut app = app_with_peek();
        app.hunk_popup_status = status;
        render(&mut app);
        action(&mut app, "Copy Old Lines");
        assert_eq!(
            crate::clipboard::get_text().unwrap(),
            "let old = 1;\nremoved();\n"
        );
        assert!(app.hunk_popup_active);
        action(&mut app, "Next Change");
        render(&mut app);
        action(&mut app, "Copy Old Lines");
        assert_eq!(crate::clipboard::get_text().unwrap(), "last_old();\n");
        assert!(app.hunk_popup_active);
    }
}

#[test]
fn copying_old_lines_retains_tabs_unicode_blank_lines_line_endings_and_eof() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for (old, new) in [
        ("\t界e\u{301}👩‍💻\r\n\r\nsecond\r\n", "replacement\r\n"),
        ("first\rsecond\r", "replacement\r"),
        ("no final newline", "replacement"),
        ("with final newline\n", "replacement"),
        ("\n\n", ""),
        ("deleted\n", ""),
    ] {
        let mut app = preview(old, new);
        render(&mut app);
        action(&mut app, "Copy Old Lines");
        assert_eq!(crate::clipboard::get_text().unwrap(), old, "{old:?}");
        assert_eq!(app.editor().unwrap().rope.to_string(), "underlying editor");
    }

    for (patch, target, old) in [
        (
            "@@ -1,2 +1 @@\n-first\r\n-second\r\n+new\r\n",
            "new\r\n",
            "first\r\nsecond\r\n",
        ),
        (
            "@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n",
            "new\n",
            "old",
        ),
        (
            "@@ -1 +1 @@\n-old\n+new\n\\ No newline at end of file\n",
            "new",
            "old\n",
        ),
    ] {
        let mut app = app_with_peek();
        app.hunk_peek = Some(HunkPeek::from_patch(
            &crate::app_git_display::parse_git_file_patch(patch).unwrap(),
            target,
            0,
        ));
        render(&mut app);
        action(&mut app, "Copy Old Lines");
        assert_eq!(crate::clipboard::get_text().unwrap(), old);
    }
}

#[test]
fn mouse_selection_copies_plain_text_in_both_directions_without_editing_the_document() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for reverse in [false, true] {
        let mut app = app_with_peek();
        let mut editor = Editor::test_with_text("underlying selection");
        editor.cursor_col = 10;
        editor.selection_anchor = Some((0, 0));
        app.tabs = vec![Tab::new_file(editor)];
        let terminal = render(&mut app);
        let layout = input_layout(&app);
        let start = (layout.code.x + 4, layout.code.y + 1);
        let end = (layout.code.x + 7, layout.code.y + 2);
        let original_bg = terminal.backend().buffer()[(start.0, start.1)].bg;
        gesture(
            &mut app,
            if reverse { end } else { start },
            if reverse { start } else { end },
        );
        let terminal = render(&mut app);
        crate::ui_control_visibility_tests::snapshot(
            terminal.backend().buffer(),
            if reverse {
                "Hunk Peek Reversed Selection"
            } else {
                "Hunk Peek Text Selection"
            },
        );
        assert_ne!(
            terminal.backend().buffer()[(start.0, start.1)].bg,
            original_bg
        );
        keyboard(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(crate::clipboard::get_text().unwrap(), "old = 1;\nremoved");
        assert!(app.hunk_popup_active);
        let editor = app.editor().unwrap();
        assert_eq!(editor.rope.to_string(), "underlying selection");
        assert_eq!(
            (
                editor.cursor_line,
                editor.cursor_col,
                editor.selection_anchor
            ),
            (0, 10, Some((0, 0)))
        );
        assert!(!editor.modified);
    }
}

#[test]
fn unicode_selection_uses_rendered_grapheme_cells_even_after_horizontal_scrolling() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for pan in [0, 5] {
        let mut app = preview(
            "\t界e\u{301}👩‍💻old\n",
            &format!("changed{}\n", "x".repeat(300)),
        );
        app.hunk_peek.as_mut().unwrap().horizontal_scroll = pan;
        render(&mut app);
        let code = input_layout(&app).code;
        let start = (code.x + 4usize.saturating_sub(pan) as u16, code.y);
        let end = (code.x + (9 - pan) as u16, code.y);
        gesture(&mut app, start, end);
        keyboard(&mut app, KeyCode::Insert, KeyModifiers::CONTROL);
        assert_eq!(crate::clipboard::get_text().unwrap(), "界e\u{301}👩‍💻");
        assert_eq!(text_width("\t界e\u{301}👩‍💻old", 4), 12);
        let peek = app.hunk_peek.as_ref().unwrap();
        let rendered = row_spans(
            &peek.rows[0],
            pan,
            20,
            Style::default(),
            Color::Red,
            4,
            &app.colors.ui,
        )
        .into_iter()
        .map(|span| span.content.into_owned())
        .collect::<String>();
        assert!(rendered.contains("e\u{301}👩‍💻old"));
    }
}

#[test]
fn word_line_shift_click_and_select_all_copy_only_preview_content() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let mut app = preview("old word\n", "new word\n");
    render(&mut app);
    let code = input_layout(&app).code;
    for click in 1..=3 {
        mouse(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            code.x + 1,
            code.y,
            KeyModifiers::NONE,
        );
        mouse(
            &mut app,
            MouseEventKind::Up(MouseButton::Left),
            code.x + 1,
            code.y,
            KeyModifiers::NONE,
        );
        if click == 2 {
            keyboard(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
            assert_eq!(crate::clipboard::get_text().unwrap(), "old");
        }
    }
    keyboard(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(crate::clipboard::get_text().unwrap(), "old word\n");
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        code.x + 3,
        code.y + 1,
        KeyModifiers::SHIFT,
    );
    mouse(
        &mut app,
        MouseEventKind::Up(MouseButton::Left),
        code.x + 3,
        code.y + 1,
        KeyModifiers::SHIFT,
    );
    keyboard(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(crate::clipboard::get_text().unwrap(), "old word\nnew");
    keyboard(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
    keyboard(&mut app, KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(
        crate::clipboard::get_text().unwrap(),
        "old word\nnew word\n"
    );
    assert_eq!(app.editor().unwrap().rope.to_string(), "underlying editor");
}

#[test]
fn pure_additions_disable_old_copy_and_empty_selection_never_copies_underlying_editor() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let mut app = preview("", "addition\n");
    render(&mut app);
    crate::clipboard::set_text("keep clipboard").unwrap();
    let index = buttons(&app)
        .iter()
        .position(|(_, action)| *action == "Copy Old Lines")
        .unwrap();
    assert!(!button_enabled(&app, index));
    action(&mut app, "Copy Old Lines");
    for code in [KeyCode::Char('c'), KeyCode::Char('x')] {
        keyboard(&mut app, code, KeyModifiers::CONTROL);
    }
    keyboard(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    keyboard(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
    assert_eq!(crate::clipboard::get_text().unwrap(), "keep clipboard");
    assert_eq!(app.editor().unwrap().rope.to_string(), "underlying editor");
    app.hunk_peek.as_mut().unwrap().focused_button = Some(index - 1);
    keyboard(&mut app, KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(
        app.hunk_peek.as_ref().unwrap().focused_button,
        Some(index + 1)
    );
    assert!(app.hunk_popup_active);
}

#[test]
fn selection_scrolls_with_its_rows_and_cancels_drag_on_focus_loss_or_navigation() {
    let (mut app, mut terminal) = long_peek();
    let layout = input_layout(&app);
    let original_scroll = app.hunk_peek.as_ref().unwrap().scroll;
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        layout.code.x,
        layout.code.y,
        KeyModifiers::NONE,
    );
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        layout.code.x + 4,
        layout.code.bottom() + 2,
        KeyModifiers::NONE,
    );
    assert!(app.hunk_peek.as_ref().unwrap().scroll > original_scroll);
    assert!(
        app.hunk_peek
            .as_ref()
            .unwrap()
            .selected_text()
            .unwrap()
            .starts_with("line 007\n")
    );
    let selected = app.hunk_peek.as_ref().unwrap().selected_text();
    app.clear_drag_states();
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        layout.code.x + 6,
        layout.code.bottom() + 3,
        KeyModifiers::NONE,
    );
    assert_eq!(app.hunk_peek.as_ref().unwrap().selected_text(), selected);
    terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    action(&mut app, "Next Change");
    assert!(app.hunk_peek.as_ref().unwrap().selected_text().is_none());
    keyboard(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(app.hunk_peek.is_none());
}

#[test]
fn copy_button_stays_within_narrow_toolbars_and_is_keyboard_accessible() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for width in [32, 60, 90] {
        let mut app = preview("old\n", "new\n");
        app.code_area.as_mut().unwrap().width = width - 6;
        app.gutter_area.as_mut().unwrap().width = 6;
        let terminal = render(&mut app);
        let layout = input_layout(&app);
        let actions = buttons(&app);
        let mut previous_right = layout.panel.x;
        for index in 0..actions.len() {
            let area = button_rect(layout.panel, index, actions);
            assert_eq!(area.intersection(layout.panel), area);
            assert!(area.x >= previous_right);
            previous_right = area.right();
        }
        let title = (layout.panel.x..layout.panel.right())
            .map(|x| terminal.backend().buffer()[(x, layout.panel.y + 1)].symbol())
            .collect::<String>();
        assert!(title.contains("Copy"));
        let count = actions.len();
        for _ in 0..count - 1 {
            keyboard(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        }
        keyboard(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(crate::clipboard::get_text().unwrap(), "old\n");
        assert!(app.hunk_popup_active);
    }
}
