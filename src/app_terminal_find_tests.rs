use super::*;
use crate::{
    app::{Tab, TerminalTab},
    editor::Editor,
    terminal::TerminalEmulator,
};
use ratatui::{Terminal, backend::TestBackend};

fn fixture(editor: bool) -> App {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::test_with_text("unchanged document"))];
    app.active_tab = 0;
    app.show_explorer = false;
    app.active_pane = ActivePane::Editor;
    app.show_terminal_sidebar = true;
    app.terminal_sidebar_revealed = true;
    app.terminal_panel_focused = true;
    app.terminals[0].is_placeholder = false;
    app.terminals[0].terminal_pty = Some(TerminalEmulator::test_with_output(
        60,
        8,
        "match\r\nsecond match\r\nlast MATCH",
    ));
    if editor {
        assert!(app.detach_terminal_to_group(0, app.active_group));
    }
    app
}

fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}
fn click(app: &mut App, area: Rect) {
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::mouse::handle_mouse(
            app,
            MouseEvent {
                kind,
                column: area.x,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            },
        );
    }
}
fn draw(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    terminal
}

#[test]
fn terminal_shortcuts_are_forwarded_even_with_find_open_but_unfocused() {
    for editor in [false, true] {
        for fullscreen in [false, true] {
            let mut app = fixture(editor);
            if fullscreen {
                app.terminals[0].terminal_pty = Some(TerminalEmulator::test_with_output(
                    60,
                    8,
                    "\x1b[?1049hfullscreen app",
                ));
            }
            let captured = app.terminals[0]
                .terminal_pty
                .as_mut()
                .unwrap()
                .test_capture_input();
            for open in [false, true] {
                if open {
                    app.begin_terminal_find(0);
                    app.terminal_find.focused = None;
                }
                for code in [KeyCode::Char('f'), KeyCode::Char('r'), KeyCode::Char('a')] {
                    press(&mut app, code, KeyModifiers::CONTROL);
                }
                assert_eq!(
                    &*captured.lock().unwrap(),
                    if open {
                        b"\x06\x12\x01\x06\x12\x01".as_slice()
                    } else {
                        b"\x06\x12\x01".as_slice()
                    }
                );
                assert!(app.focused_text_input_id().is_none());
            }
        }
    }
}

#[test]
fn focused_find_owns_typing_paste_navigation_and_escape_in_both_locations() {
    for editor in [false, true] {
        let mut app = fixture(editor);
        let captured = app.terminals[0]
            .terminal_pty
            .as_mut()
            .unwrap()
            .test_capture_input();
        app.begin_terminal_find(0);
        let id = app.terminals[0].find.id;
        press(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
        crate::app_event_loop::handle_paste_input(&mut app, "atch");
        assert_eq!(app.terminals[0].find.query, "match");
        assert_eq!(app.terminals[0].find.index.matches.len(), 3);
        assert_eq!(
            app.focused_text_input_id(),
            Some(TextInputId::TerminalFind(id))
        );
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.terminals[0].find.index.selected, Some(1));
        press(&mut app, KeyCode::F(3), KeyModifiers::SHIFT);
        assert_eq!(app.terminals[0].find.index.selected, Some(0));
        press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        crate::app_event_loop::handle_paste_input(&mut app, "absent");
        assert!(app.terminals[0].find.index.matches.is_empty());
        press(&mut app, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(app.terminals[0].find.query, "match");
        assert!(
            captured.lock().unwrap().is_empty(),
            "Find input must never reach the child"
        );
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!app.terminals[0].find.open);
        assert!(app.focused_text_input_id().is_none());
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(&*captured.lock().unwrap(), b"x");
    }
}

#[test]
fn find_button_and_widget_controls_capture_mouse_without_resizing_terminal() {
    let mut app = fixture(false);
    draw(&mut app, 180, 36);
    let grid = &app.terminals[0].terminal_pty.as_ref().unwrap().grid;
    let size = (grid.width, grid.height);
    let captured = app.terminals[0]
        .terminal_pty
        .as_mut()
        .unwrap()
        .test_capture_input();
    let button = app
        .terminal_find
        .controls
        .iter()
        .find(|(_, _, action)| matches!(action, Action::Open))
        .unwrap()
        .0;
    click(&mut app, button);
    crate::app_event_loop::handle_paste_input(&mut app, "match");
    let terminal = draw(&mut app, 180, 36);
    assert!(
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .contains("Find in Terminal")
    );
    let grid = &app.terminals[0].terminal_pty.as_ref().unwrap().grid;
    assert_eq!((grid.width, grid.height), size);
    for (action, expected) in [(Action::Next, 1), (Action::Previous, 0), (Action::Case, 0)] {
        let target = app
            .terminal_find
            .controls
            .iter()
            .find(|(_, _, candidate)| {
                std::mem::discriminant(candidate) == std::mem::discriminant(&action)
            })
            .unwrap()
            .0;
        click(&mut app, target);
        assert_eq!(app.terminals[0].find.index.selected, Some(expected));
    }
    assert!(app.terminals[0].find.case_sensitive);
    assert_eq!(app.terminals[0].find.index.matches.len(), 2);
    let close = app
        .terminal_find
        .controls
        .iter()
        .find(|(_, _, action)| matches!(action, Action::Close))
        .unwrap()
        .0;
    click(&mut app, close);
    assert!(!app.terminals[0].find.open);
    assert!(captured.lock().unwrap().is_empty());
}

#[test]
fn search_field_uses_shared_context_menu_and_stable_terminal_identity() {
    let mut app = fixture(false);
    app.terminals
        .push(TerminalTab::new("other".into(), app.explorer.root.clone()));
    app.terminals[1].terminal_pty = Some(TerminalEmulator::test_with_output(60, 8, "other output"));
    app.begin_terminal_find(1);
    let id = app.terminals[1].find.id;
    draw(&mut app, 180, 36);
    let area = app.text_input_area(TextInputId::TerminalFind(id)).unwrap();
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert!(app.text_input_context_menu.is_some());
    assert_eq!(
        app.focused_text_input_id(),
        Some(TextInputId::TerminalFind(id))
    );
    app.text_input_context_menu = None;
    app.close_terminal_tab_at(0);
    assert_eq!(app.terminals[0].find.id, id);
    crate::app_event_loop::handle_paste_input(&mut app, "other");
    assert_eq!(app.terminals[0].find.query, "other");
    assert_eq!(app.terminals[0].find.index.matches.len(), 1);
}

#[test]
fn widget_renders_in_small_terminals_without_panicking() {
    let mut app = fixture(true);
    app.begin_terminal_find(0);
    for width in [1, 2, 8, 20, 38, 66] {
        for height in [1, 2, 3, 8] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| super::draw(frame, &mut app, 0, frame.area()))
                .unwrap();
        }
    }
}

#[test]
fn closing_the_search_owner_removes_stale_input_regions() {
    let mut app = fixture(false);
    app.begin_terminal_find(0);
    let id = TextInputId::TerminalFind(app.terminals[0].find.id);
    draw(&mut app, 180, 36);
    assert!(app.text_input_area(id).is_some());
    app.close_terminal_tab_at(0);
    assert!(app.text_input_area(id).is_none());
    assert!(app.focused_text_input_id().is_none());
    assert!(app.terminal_find.focused.is_none());
}

#[test]
fn long_search_fields_scroll_without_sending_wheel_input_to_the_terminal() {
    let mut app = fixture(false);
    let captured = app.terminals[0]
        .terminal_pty
        .as_mut()
        .unwrap()
        .test_capture_input();
    app.begin_terminal_find(0);
    crate::app_event_loop::handle_paste_input(&mut app, &"0123456789".repeat(20));
    let terminal = draw(&mut app, 180, 36);
    let id = TextInputId::TerminalFind(app.terminals[0].find.id);
    let area = app.text_input_area(id).unwrap();
    let visible_text = |terminal: &Terminal<TestBackend>| {
        (area.x..area.right())
            .map(|x| terminal.backend().buffer()[(x, area.y)].symbol())
            .collect::<String>()
    };
    let before = visible_text(&terminal);
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::ScrollLeft,
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        },
    );
    let terminal = draw(&mut app, 180, 36);
    assert_ne!(visible_text(&terminal), before);
    assert!(app.text_input_viewport(id).is_some());
    assert_eq!(app.terminals[0].find.query, "0123456789".repeat(20));
    assert_eq!(app.focused_text_input_id(), Some(id));
    assert!(captured.lock().unwrap().is_empty());
}

#[test]
fn editor_find_button_and_context_menu_open_the_target_terminal() {
    for context_menu in [false, true] {
        let mut app = fixture(true);
        draw(&mut app, 180, 36);
        if context_menu {
            app.tab_context_tab = app.active_tab;
            app.tab_context_active = true;
            app.tab_context_selected = App::tab_context_items()
                .iter()
                .position(|(_, command, _)| *command == "tab_find_terminal")
                .unwrap();
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        } else {
            let button = app
                .terminal_find
                .controls
                .iter()
                .find(|(_, _, action)| matches!(action, Action::Open))
                .unwrap()
                .0;
            click(&mut app, button);
        }
        assert_eq!(
            app.focused_text_input_id(),
            Some(TextInputId::TerminalFind(app.terminals[0].find.id))
        );
        assert!(!app.terminal_panel_focused);
    }
}

#[test]
fn matches_highlight_the_visible_scrollback_and_manual_selection_takes_precedence() {
    let mut app = fixture(false);
    let text = (0..80)
        .map(|index| format!("line {index} match\r\n"))
        .collect::<String>();
    app.terminals[0].terminal_pty = Some(TerminalEmulator::test_with_output(80, 18, &text));
    let mut terminal = Terminal::new(TestBackend::new(80, 18)).unwrap();
    app.begin_terminal_find(0);
    crate::app_event_loop::handle_paste_input(&mut app, "match");
    // Search from the top of history, well away from the floating widget.
    app.terminals[0].find.index.selected = Some(0);
    let session = &mut app.terminals[0];
    session
        .find
        .index
        .reveal(&mut session.terminal_pty.as_mut().unwrap().grid);
    let paint = |frame: &mut Frame, app: &mut App| {
        crate::ui_helpers::draw_terminal_emulator_with_cursor(
            frame,
            app.terminals[0].terminal_pty.as_ref().unwrap(),
            frame.area(),
            &app.colors.ui,
            false,
            false,
            &app.terminals[0].input_activity,
        );
        super::draw(frame, app, 0, frame.area());
    };
    terminal.draw(|frame| paint(frame, &mut app)).unwrap();
    let hit = app.terminals[0].find.index.matches[0];
    assert!(
        app.terminals[0]
            .terminal_pty
            .as_ref()
            .unwrap()
            .grid
            .scroll_view
            > 0
    );
    assert_eq!(
        terminal.backend().buffer()[(hit.start.column as u16, 0)].bg,
        app.colors.ui.bg_find_current
    );
    let grid = &mut app.terminals[0].terminal_pty.as_mut().unwrap().grid;
    grid.begin_selection(0, hit.start.column);
    grid.extend_selection(0, hit.end.column - 1);
    grid.finish_selection();
    terminal.draw(|frame| paint(frame, &mut app)).unwrap();
    assert_ne!(
        terminal.backend().buffer()[(hit.start.column as u16, 0)].bg,
        app.colors.ui.bg_find_current
    );
    let pty = app.terminals[0].terminal_pty.as_mut().unwrap();
    pty.test_queue_output(b"\x1b[H\x1b[2J\x1b[3J");
    assert!(pty.process_while(|| true));
    terminal.draw(|frame| paint(frame, &mut app)).unwrap();
    assert!(app.terminals[0].find.index.matches.is_empty());
}

#[test]
fn unchanged_find_frames_skip_the_history_walk_until_the_pty_changes() {
    let mut app = fixture(false);
    app.terminals[0].terminal_pty = Some(TerminalEmulator::test_with_output(
        80, 24, &"find match\r\n".repeat(3000),
    ));
    app.begin_terminal_find(0);
    crate::app_event_loop::handle_paste_input(&mut app, "match");
    assert_eq!(app.terminals[0].find.index.matches.len(), 3000);
    let builds = app.terminals[0].find.index.stamp_builds;
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let redraw = |terminal: &mut Terminal<TestBackend>, app: &mut App| {
        terminal.draw(|frame| super::draw(frame, app, 0, frame.area())).unwrap();
    };
    for _ in 0..10 {
        redraw(&mut terminal, &mut app);
    }
    assert_eq!(app.terminals[0].find.index.stamp_builds, builds);

    let pty = app.terminals[0].terminal_pty.as_mut().unwrap();
    pty.resize(78, 24);
    redraw(&mut terminal, &mut app);
    assert_eq!(app.terminals[0].find.index.stamp_builds, builds + 1);
    assert_eq!(app.terminals[0].find.index.matches.len(), 3000);

    app.terminals[0].terminal_pty = Some(TerminalEmulator::test_with_output(78, 24, "new match"));
    redraw(&mut terminal, &mut app);
    assert_eq!(app.terminals[0].find.index.stamp_builds, builds + 2);
    assert_eq!(app.terminals[0].find.index.matches.len(), 1);
}
