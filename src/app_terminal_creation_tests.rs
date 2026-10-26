use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn label_at(buffer: &Buffer, label: &str, rows: std::ops::Range<u16>) -> (u16, u16) {
    for row in rows {
        for col in 0..=buffer.area.width.saturating_sub(label.len() as u16) {
            if label.chars().enumerate().all(|(i, ch)| {
                buffer[(col + i as u16, row)].symbol() == ch.to_string()
            }) {
                return (col, row);
            }
        }
    }
    panic!("menu label not rendered: {label}");
}

fn click(app: &mut App, (column, row): (u16, u16)) {
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        crate::mouse::handle_mouse(app, MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE });
    }
}

#[test]
fn terminal_creation_menu_creates_one_session_at_startup_and_after_last_close() {
    for command in ["new_terminal", "new_terminal_in_editor_tab"] {
        for closed_dock in [None, Some(TerminalDock::Right)] {
            for keyboard in [false, true] {
                let mut app = App::test_new();
                app.open_settings_tab();
                app.settings_editor_search = "theme".into();
                if let Some(dock) = closed_dock {
                    app.toggle_terminal_sidebar();
                    app.move_terminal_to_dock(0, dock);
                    app.close_terminal_tab();
                }
                assert!(!app.terminal_is_open(0));
                let tabs = app.tabs.len();
                let mut terminal = Terminal::new(TestBackend::new(140, 35)).unwrap();
                terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
                let menu = label_at(terminal.backend().buffer(), "Terminal", 0..1);
                click(&mut app, menu);
                let items = app.menu_items(app.menu_selected);
                let index = items.iter().position(|(_, id)| *id == command).expect("creation action in menu");
                assert!(!app.is_menu_cmd_disabled(command));
                app.menu_item_selected = index;
                terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
                if keyboard {
                    crate::input_keyboard::handle_keyboard_event(&mut app,
                        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                } else {
                    let action = label_at(terminal.backend().buffer(), items[index].0.trim(), 1..12);
                    click(&mut app, action);
                }
                assert!(!app.menu_active && !app.menu_open);
                assert_eq!(app.terminals.len(), 1, "{command}, previous dock {closed_dock:?}");
                assert_eq!(app.active_terminal, 0);
                assert!(app.terminal_is_open(0));
                assert_eq!(app.active_pane, ActivePane::Editor);
                assert_eq!(app.settings_editor_search, "theme");
                if command == "new_terminal" {
                    assert!(app.show_terminal_sidebar && app.docked_terminal_has_focus());
                    assert_eq!(app.tabs.len(), tabs);
                    assert_eq!(app.tabs[app.active_tab].kind, TabKind::Settings);
                } else {
                    assert!(!app.show_terminal_sidebar && !app.show_panel);
                    assert!(!app.terminal_panel_focused);
                    assert_eq!(app.tabs.len(), tabs + 1);
                    assert_eq!(app.tabs[app.active_tab].kind, TabKind::Terminal);
                    assert_eq!(app.tabs[app.active_tab].terminal_idx, Some(0));
                }

                // The resulting view must accept input without spawning a host shell.
                let mut pty = crate::terminal::TerminalEmulator::test_with_output(40, 20, "");
                let input = pty.test_capture_input();
                app.terminals[0].terminal_pty = Some(pty);
                terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
                crate::input_keyboard::handle_keyboard_event(&mut app,
                    KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
                assert_eq!(&*input.lock().unwrap(), b"x", "{command}");
                assert_eq!(app.settings_editor_search, "theme");
            }
        }
    }
}

#[test]
fn new_terminal_in_editor_tab_uses_focused_split_and_preserves_dock_session() {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::new()), Tab::new_file(Editor::new())];
    app.create_split_group(1);
    let group = app.active_group;
    let other = app.groups[0].tabs.len();
    app.open_settings_tab();
    app.execute_palette_command("new_terminal");
    app.terminals[0].pending_command = Some("existing command".into());
    assert!(app.docked_terminal_has_focus());

    app.execute_palette_command("new_terminal_in_editor_tab");

    assert_eq!(app.active_group, group);
    assert_eq!(app.groups[0].tabs.len(), other);
    assert_eq!(app.terminals.len(), 2);
    assert_eq!(app.terminals[0].pending_command.as_deref(), Some("existing command"));
    assert_eq!(app.tabs[app.active_tab].kind, TabKind::Terminal);
    assert_eq!(app.tabs[app.active_tab].terminal_idx, Some(1));
    assert_eq!(app.focused_terminal_index(), Some(1));
    assert!(app.show_terminal_sidebar, "existing dock remains visible");
    assert!(!app.terminal_panel_focused, "new editor terminal takes focus");
    app.close_terminal_tab();
    assert_eq!(app.terminals.len(), 1);
    assert!(app.tabs.iter().all(|tab| tab.kind != TabKind::Terminal));
    assert_eq!(app.terminals[0].pending_command.as_deref(), Some("existing command"));
}

#[test]
fn terminal_menu_actions_work_over_an_editor_terminal() {
    for command in ["toggle_terminal", "new_terminal", "new_terminal_in_editor_tab", "move_terminal_to_right"] {
        for split in [false, true] {
            for mouse_tracking in [false, true] {
                for keyboard in [false, true] {
                    let context = format!("{command}, split={split}, mouse={mouse_tracking}, keyboard={keyboard}");
                    let mut app = App::test_new();
                    app.show_explorer = false;
                    if split {
                        app.tabs = vec![Tab::new_file(Editor::new()), Tab::new_file(Editor::new())];
                        app.create_split_group(1);
                        app.switch_group(0);
                    }
                    let group = app.active_group;
                    app.new_terminal_in_editor_tab();
                    let output = if mouse_tracking { "\x1b[?1049;1003;1006h" } else { "shell output" };
                    let mut pty = crate::terminal::TerminalEmulator::test_with_output(140, 30, output);
                    let input = pty.test_capture_input();
                    app.terminals[0].terminal_pty = Some(pty);
                    let tabs = app.tabs.len();
                    let mut terminal = Terminal::new(TestBackend::new(140, 35)).unwrap();
                    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
                    click(&mut app, label_at(terminal.backend().buffer(), "Terminal", 0..1));
                    assert!(app.menu_active && app.menu_open, "{context}");
                    let items = app.menu_items(app.menu_selected);
                    let index = items.iter().position(|(_, id)| *id == command).unwrap();
                    app.menu_item_selected = index;
                    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
                    let action = label_at(terminal.backend().buffer(), items[index].0.trim(), 1..12);
                    assert!(app.terminal_surfaces.iter().any(|surface| surface.area.contains(action.into())),
                        "the menu must overlap the terminal: {context}");
                    if keyboard {
                        crate::input_keyboard::handle_keyboard_event(&mut app,
                            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                    } else {
                        click(&mut app, action);
                    }
                    assert!(!app.menu_active && !app.menu_open, "{context}");
                    assert!(app.terminal_mouse_capture.is_none(), "{context}");
                    assert!(app.terminal_selection_drag.is_none(), "{context}");
                    assert!(!app.terminals[0].terminal_pty.as_ref().unwrap().grid.has_selection(), "{context}");
                    assert!(input.lock().unwrap().is_empty(), "menu input reached the shell: {context}");
                    assert_eq!(app.active_group, group, "{context}");
                    match command {
                        "toggle_terminal" | "new_terminal" => {
                            assert_eq!(app.terminals.len(), 2, "{context}");
                            assert_eq!(app.tabs.len(), tabs, "{context}");
                            assert!(app.terminal_is_in_editor(0), "{context}");
                            assert!(app.show_terminal_sidebar && app.docked_terminal_has_focus(), "{context}");
                            assert_eq!(app.active_terminal, 1, "{context}");
                        }
                        "new_terminal_in_editor_tab" => {
                            assert_eq!(app.terminals.len(), 2, "{context}");
                            assert_eq!(app.tabs.len(), tabs + 1, "{context}");
                            assert!(app.terminal_is_in_editor(0) && app.terminal_is_in_editor(1), "{context}");
                            assert!(!app.show_terminal_sidebar && !app.terminal_panel_focused, "{context}");
                            assert_eq!(app.focused_terminal_index(), Some(1), "{context}");
                        }
                        "move_terminal_to_right" => {
                            assert_eq!(app.terminals.len(), 1, "{context}");
                            assert_eq!(app.tabs.len(), tabs - 1, "{context}");
                            assert!(!app.terminal_is_in_editor(0), "{context}");
                            assert!(app.show_terminal_sidebar && app.docked_terminal_has_focus(), "{context}");
                            assert_eq!(app.active_terminal, 0, "{context}");
                        }
                        _ => unreachable!(),
                    }
                    let index = app.focused_terminal_index().unwrap();
                    let mut pty = crate::terminal::TerminalEmulator::test_with_output(40, 20, "");
                    let focused_input = pty.test_capture_input();
                    app.terminals[index].terminal_pty = Some(pty);
                    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
                    crate::input_keyboard::handle_keyboard_event(&mut app,
                        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
                    assert_eq!(&*focused_input.lock().unwrap(), b"x", "{context}");
                }
            }
        }
    }
}

#[test]
fn disabled_detach_menu_action_does_not_select_or_send_mouse_to_editor_terminal() {
    for mouse_tracking in [false, true] {
        let mut app = App::test_new();
        app.show_explorer = false;
        app.new_terminal_in_editor_tab();
        let output = if mouse_tracking { "\x1b[?1003;1006h" } else { "shell output" };
        let mut pty = crate::terminal::TerminalEmulator::test_with_output(140, 30, output);
        let input = pty.test_capture_input();
        app.terminals[0].terminal_pty = Some(pty);
        let mut terminal = Terminal::new(TestBackend::new(140, 35)).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        click(&mut app, label_at(terminal.backend().buffer(), "Terminal", 0..1));
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        assert!(app.is_menu_cmd_disabled("detach_terminal_to_tab"));
        click(&mut app, label_at(terminal.backend().buffer(), "Detach to Editor Tab", 1..12));
        assert!(app.menu_active && app.menu_open);
        assert!(app.terminal_mouse_capture.is_none());
        assert!(app.terminal_selection_drag.is_none());
        assert!(input.lock().unwrap().is_empty());
        assert_eq!(app.terminals.len(), 1);
        assert!(app.terminal_is_in_editor(0));
        assert!(!app.show_terminal_sidebar);
    }
}
