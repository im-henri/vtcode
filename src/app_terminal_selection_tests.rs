use super::*;
use crate::app::{EditorGroup, Tab, TerminalDock, TerminalTab};
use crate::editor::Editor;
use crate::terminal::TerminalEmulator;
use crossterm::event::KeyModifiers;
use ratatui::{Terminal, backend::TestBackend, style::Color};

fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn fixture() -> App {
    let mut app = App::test_new();
    app.active_pane = ActivePane::Editor;
    app.tabs = vec![Tab::new_file(Editor::new())];
    app.show_terminal_sidebar = true;
    app.terminal_panel_focused = true;
    app.terminals[0].terminal_pty =
        Some(TerminalEmulator::test_with_output(20, 3, "hello\r\nworld"));
    app.terminal_surfaces.push(TerminalSurface {
        index: 0,
        area: Rect::new(2, 4, 20, 3),
        group: None,
    });
    app
}

#[test]
fn terminal_double_click_selects_words_and_triple_click_selects_lines() {
    for detached in [false, true] {
        let mut app = fixture();
        let code = "let foo_bar = run(foo_bar);";
        app.terminals[0].terminal_pty = Some(TerminalEmulator::test_with_output(40, 3, &format!("{code}\r\nnext_line")));
        app.terminal_surfaces[0].area.width = 40;
        if detached {
            assert!(app.detach_terminal_to_group(0, app.active_group));
            app.terminal_surfaces[0].group = Some(app.active_group);
        }
        let selected = |app: &App| app.terminals[0].terminal_pty.as_ref().unwrap().grid.selected_text();
        for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Drag(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
            crate::mouse::handle_mouse(&mut app, mouse(kind, 8, 4));
        }
        assert_eq!(selected(&app), None, "one click only focuses the terminal");
        click(&mut app, 8, 4);
        assert_eq!(selected(&app).as_deref(), Some("foo_bar"));
        assert_eq!(app.terminals[0].terminal_pty.as_ref().unwrap().grid.selection_columns(0), 4..11);
        let mut copied = String::new();
        app.copy_terminal_selection_with(0, |text| { copied = text.into(); true });
        assert_eq!(copied, "foo_bar");
        click(&mut app, 8, 4);
        assert_eq!(selected(&app).as_deref(), Some(code));
        assert!(!app.editor().is_some_and(|editor| editor.has_selection() || editor.modified));
        assert!(app.terminal_selection_drag.is_none());
    }
}

fn click(app: &mut App, column: u16, row: u16) {
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        crate::mouse::handle_mouse(app, mouse(kind, column, row));
    }
}

#[test]
fn terminal_double_click_drag_keeps_whole_words_in_both_directions() {
    let mut app = fixture();
    app.terminals[0].terminal_pty = Some(TerminalEmulator::test_with_output(20, 3, "one two three four"));
    click(&mut app, 7, 4);
    crate::mouse::handle_mouse(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), 7, 4));
    for (column, expected) in [(12, "two three"), (2, "one two"), (7, "two")] {
        crate::mouse::handle_mouse(&mut app, mouse(MouseEventKind::Drag(MouseButton::Left), column, 4));
        assert_eq!(app.terminals[0].terminal_pty.as_ref().unwrap().grid.selected_text().as_deref(), Some(expected));
    }
    crate::mouse::handle_mouse(&mut app, mouse(MouseEventKind::Up(MouseButton::Left), 12, 4));
    assert_eq!(app.terminals[0].terminal_pty.as_ref().unwrap().grid.selected_text().as_deref(), Some("two three"));
    click(&mut app, 7, 4);
    assert!(!app.focused_terminal_has_selection(), "a drag ends the multi-click sequence");
}

#[test]
fn terminal_multi_click_resets_after_timeout_other_clicks_and_terminal_changes() {
    let mut app = fixture();
    click(&mut app, 4, 4);
    app.terminal_clicks.last.as_mut().unwrap().time -= std::time::Duration::from_millis(401);
    click(&mut app, 4, 4);
    assert!(!app.focused_terminal_has_selection());
    // An overlay consumes this press before terminal selection sees it.
    app.change_dir_active = true;
    crate::mouse::handle_mouse(&mut app, mouse(MouseEventKind::Down(MouseButton::Left), 4, 4));
    app.change_dir_active = false;
    click(&mut app, 4, 4);
    assert!(!app.focused_terminal_has_selection());
    app.terminals.push(TerminalTab::new("Other".into(), app.explorer.root.clone()));
    app.terminals[1].terminal_pty = Some(TerminalEmulator::test_with_output(20, 3, "other_word"));
    app.terminal_surfaces[0].index = 1;
    click(&mut app, 4, 4);
    assert!(!app.focused_terminal_has_selection());
    click(&mut app, 4, 4);
    assert_eq!(app.terminals[1].terminal_pty.as_ref().unwrap().grid.selected_text().as_deref(), Some("other_word"));
}

#[test]
fn terminal_double_click_respects_child_mouse_tracking_and_shift_override() {
    let mut app = fixture();
    let mut pty = TerminalEmulator::test_with_output(20, 3, "\x1b[?1003;1006hhello");
    let captured = pty.test_capture_input();
    app.terminals[0].terminal_pty = Some(pty);
    click(&mut app, 4, 4);
    click(&mut app, 4, 4);
    assert!(!app.focused_terminal_has_selection());
    assert_eq!(&*captured.lock().unwrap(), b"\x1b[<0;3;1M\x1b[<0;3;1m\x1b[<0;3;1M\x1b[<0;3;1m");
    let forwarded = captured.lock().unwrap().clone();
    for _ in 0..2 {
        for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
            crate::mouse::handle_mouse(&mut app, MouseEvent { modifiers: KeyModifiers::SHIFT, ..mouse(kind, 4, 4) });
        }
    }
    assert_eq!(app.terminals[0].terminal_pty.as_ref().unwrap().grid.selected_text().as_deref(), Some("hello"));
    assert_eq!(*captured.lock().unwrap(), forwarded, "local selection must not send mouse events to the child");
}

#[test]
fn child_mouse_capture_uses_local_coordinates_and_keeps_drag_until_release() {
    let mut app = fixture();
    let mut pty = TerminalEmulator::test_with_output(20, 3, "\x1b[?1003;1006h");
    let captured = pty.test_capture_input();
    app.terminals[0].terminal_pty = Some(pty);
    for event in [
        mouse(MouseEventKind::Down(MouseButton::Left), 3, 4),
        mouse(MouseEventKind::Drag(MouseButton::Left), 80, 30),
        mouse(MouseEventKind::Up(MouseButton::Left), 80, 30),
        mouse(MouseEventKind::Down(MouseButton::Right), 3, 4),
        mouse(MouseEventKind::Up(MouseButton::Right), 3, 4),
        mouse(MouseEventKind::ScrollUp, 3, 4),
        mouse(MouseEventKind::Down(MouseButton::Back), 3, 4),
        mouse(MouseEventKind::Up(MouseButton::Back), 3, 4),
    ] {
        crate::mouse::handle_mouse(&mut app, event);
    }
    assert_eq!(&*captured.lock().unwrap(),
        b"\x1b[<0;2;1M\x1b[<32;20;3M\x1b[<0;20;3m\x1b[<2;2;1M\x1b[<2;2;1m\x1b[<64;2;1M\x1b[<128;2;1M\x1b[<128;2;1m");
    assert!(!app.context_menu_active);
    assert!(!app.focused_terminal_has_selection());
    assert!(app.terminal_mouse_capture.is_none());

    // Shift still provides the terminal's local selection override.
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                modifiers: KeyModifiers::SHIFT,
                ..mouse(kind, 5, 4)
            },
        );
    }
    assert!(app.terminal_mouse_capture.is_none());
    assert!(app.terminal_selection_drag.is_none());
    assert!(!app.editor().unwrap().has_selection());
}

#[test]
fn child_mouse_capture_does_not_survive_terminal_closure() {
    let mut app = fixture();
    let mut pty = TerminalEmulator::test_with_output(20, 3, "\x1b[?1003;1006h");
    let captured = pty.test_capture_input();
    app.terminals[0].terminal_pty = Some(pty);
    crate::mouse::handle_mouse(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 3, 4),
    );
    assert!(app.terminal_mouse_capture.is_some());
    app.close_terminal_tab_at(0);
    assert!(app.terminal_mouse_capture.is_none());
    assert_eq!(&*captured.lock().unwrap(), b"\x1b[<0;2;1M");
}

#[test]
fn terminal_mouse_ignores_workbench_gestures_but_finishes_its_own_capture_with_a_menu_open() {
    let mut app = fixture();
    let mut pty = TerminalEmulator::test_with_output(20, 3, "\x1b[?1003;1006h");
    let captured = pty.test_capture_input();
    app.terminals[0].terminal_pty = Some(pty);
    for kind in [MouseEventKind::Drag(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        assert!(!app.handle_terminal_selection_mouse(mouse(kind, 3, 4)));
    }
    app.menu_active = true;
    app.menu_open = true;
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Moved,
        MouseEventKind::ScrollUp, MouseEventKind::Up(MouseButton::Left)]
    {
        assert!(!app.handle_terminal_selection_mouse(mouse(kind, 3, 4)));
    }
    assert!(captured.lock().unwrap().is_empty());
    assert!(app.terminal_mouse_capture.is_none());
    assert!(app.terminal_selection_drag.is_none());

    app.menu_active = false;
    app.menu_open = false;
    assert!(app.handle_terminal_selection_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 3, 4)));
    app.menu_active = true;
    app.menu_open = true;
    assert!(app.handle_terminal_selection_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 80, 30)));
    assert!(app.handle_terminal_selection_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 80, 30)));
    assert_eq!(&*captured.lock().unwrap(), b"\x1b[<0;2;1M\x1b[<32;20;3M\x1b[<0;20;3m");
    assert!(app.terminal_mouse_capture.is_none());
    assert!(app.menu_active && app.menu_open);
}

#[test]
fn drag_capture_copies_across_rows_without_selecting_the_editor() {
    let mut app = fixture();
    app.code_area = Some(Rect::new(25, 4, 20, 3));
    for event in [
        mouse(MouseEventKind::Down(MouseButton::Left), 2, 4),
        mouse(MouseEventKind::Drag(MouseButton::Left), 6, 5),
        mouse(MouseEventKind::Up(MouseButton::Left), 6, 5),
    ] {
        crate::mouse::handle_mouse(&mut app, event);
    }
    assert_eq!(
        app.terminals[0]
            .terminal_pty
            .as_ref()
            .unwrap()
            .grid
            .selected_text()
            .as_deref(),
        Some("hello\nworld")
    );
    assert!(!app.editor().unwrap().has_selection());
    assert!(app.terminal_selection_drag.is_none());
    let mut copied = String::new();
    app.copy_terminal_selection_with(0, |text| {
        copied = text.to_owned();
        true
    });
    assert_eq!(copied, "hello\nworld");
    assert_eq!(
        app.clipboard_history.back().map(String::as_str),
        Some("hello\nworld")
    );
    assert!(
        app.terminals[0]
            .terminal_pty
            .as_ref()
            .unwrap()
            .grid
            .has_selection()
    );
}

#[test]
fn drag_outside_terminal_stays_captured_and_clamps_to_its_grid() {
    let mut app = fixture();
    assert!(app.handle_terminal_selection_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        2,
        4
    )));
    assert!(app.handle_terminal_selection_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        50,
        5
    )));
    assert!(app.handle_terminal_selection_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        50,
        5
    )));
    assert_eq!(
        app.terminals[0]
            .terminal_pty
            .as_ref()
            .unwrap()
            .grid
            .selected_text()
            .as_deref(),
        Some("hello\nworld")
    );
    assert!(!app.editor().unwrap().has_selection());
}

#[test]
fn right_click_offers_terminal_copy_and_paste_and_failed_copy_keeps_text() {
    let mut app = fixture();
    let grid = &mut app.terminals[0].terminal_pty.as_mut().unwrap().grid;
    grid.begin_selection(0, 0);
    grid.extend_selection(0, 4);
    app.handle_terminal_selection_mouse(mouse(MouseEventKind::Down(MouseButton::Right), 3, 4));
    assert!(app.context_menu_active);
    assert_eq!(
        app.context_menu_items
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        vec!["copy", "paste"]
    );
    assert!(!app.is_menu_cmd_disabled("copy"));
    assert!(!app.is_menu_cmd_disabled("paste"));
    assert!(app.is_menu_cmd_disabled("cut"));
    app.copy_terminal_selection_with(0, |_| false);
    assert_eq!(
        app.clipboard_history.back().map(String::as_str),
        Some("hello")
    );
    assert!(app.focused_terminal_has_selection());
}

#[test]
fn dock_focus_wins_over_an_editor_terminal_and_paste_does_not_touch_files() {
    let mut app = fixture();
    app.terminals.push(TerminalTab::new(
        "Editor shell".into(),
        app.explorer.root.clone(),
    ));
    app.terminals[1].terminal_pty = Some(TerminalEmulator::test_with_output(20, 3, "other"));
    assert!(app.detach_terminal_to_group(1, app.active_group));
    app.active_terminal = 0;
    app.terminal_panel_focused = true;
    assert_eq!(app.focused_terminal_index(), Some(0));
    let capture = app.terminals[0]
        .terminal_pty
        .as_mut()
        .unwrap()
        .test_capture_input();
    app.terminals[0]
        .terminal_pty
        .as_mut()
        .unwrap()
        .grid
        .bracketed_paste = true;
    app.paste_terminal_text(0, "héllo\nworld");
    assert_eq!(
        &*capture.lock().unwrap(),
        b"\x1b[200~h\xc3\xa9llo\nworld\x1b[201~"
    );
    assert!(!app.tabs[0].editor.modified);
    assert!(!app.tabs[1].editor.modified);
}

#[test]
fn terminal_resize_geometry_and_closure_cannot_select_another_terminal() {
    let mut app = fixture();
    app.handle_terminal_selection_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 4));
    app.terminal_surfaces[0].area = Rect::new(2, 4, 10, 3);
    assert!(app.handle_terminal_selection_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        4,
        4
    )));
    assert!(
        !app.terminals[0]
            .terminal_pty
            .as_ref()
            .unwrap()
            .grid
            .has_selection()
    );
    app.terminals
        .push(TerminalTab::new("Second".into(), app.explorer.root.clone()));
    app.close_terminal_tab_at(0);
    assert!(app.terminal_selection_drag.is_none());
    assert!(app.terminal_surfaces.is_empty());
    assert_eq!(app.terminals[0].name, "Second");
}

#[test]
fn output_selection_uses_terminal_theme_colors_and_preserves_output() {
    let mut pty = TerminalEmulator::test_with_output(20, 3, "\x1b[31mhello\x1b[0m\r\nworld");
    pty.grid.begin_selection(0, 1);
    pty.grid.extend_selection(0, 3);
    let mut app = fixture();
    app.colors.ui.bg_terminal_selection = Color::Blue;
    app.colors.ui.fg_terminal_selection = Some(Color::White);
    let mut terminal = Terminal::new(TestBackend::new(24, 5)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_helpers::draw_terminal_emulator(
                frame,
                &pty,
                Rect::new(2, 1, 20, 3),
                &app.colors.ui,
                true,
            )
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    for column in 3..=5 {
        let cell = buffer.cell((column, 1)).unwrap();
        assert_eq!(cell.bg, Color::Blue);
        assert_eq!(cell.fg, Color::White);
    }
    let unselected = buffer.cell((2, 1)).unwrap();
    assert_eq!(unselected.symbol(), "h");
    assert_eq!(
        unselected.fg,
        app.colors.ui.terminal_color(Color::Red, true)
    );
    assert_eq!(buffer.cell((2, 2)).unwrap().symbol(), "w");
}

#[test]
fn rendered_editor_splits_register_the_owner_for_mouse_focus() {
    let mut app = fixture();
    assert!(app.detach_terminal_to_group(0, app.active_group));
    let term_tab = app.tabs.pop().unwrap();
    app.groups.push(EditorGroup::new());
    let mut group = EditorGroup::new();
    group.tabs.push(term_tab);
    app.groups.push(group);
    app.terminal_surfaces.clear();
    let mut terminal = Terminal::new(TestBackend::new(20, 4)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_helpers::draw_split_editor_for_group(
                frame,
                &mut app,
                Rect::new(0, 0, 20, 4),
                1,
            )
        })
        .unwrap();
    let surface = *app.terminal_surfaces.last().unwrap();
    assert_eq!(surface.group, Some(1));
    assert_eq!(surface.area, Rect::new(0, 1, 20, 3));
    app.handle_terminal_selection_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 0, 1));
    assert_eq!(app.active_group, 1);
    assert_eq!(app.focused_terminal_index(), Some(0));
    assert!(!app.terminal_panel_focused);
}

#[test]
fn right_dock_registers_only_its_output_rectangle() {
    for dock in [TerminalDock::Right] {
        let mut app = fixture();
        app.terminals[0].dock = dock;
        app.panel_tab = 1;
        app.show_panel = true;
        app.terminal_surfaces.clear();
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        terminal
            .draw(|frame| {
                let area = Rect::new(0, 0, 40, 10);
                match dock {
                    TerminalDock::Right => {
                        crate::ui_panels::draw_terminal_sidebar(frame, &mut app, area)
                    }
                }
            })
            .unwrap();
        let surface = *app.terminal_surfaces.last().unwrap();
        assert_eq!(surface.index, 0);
        assert_eq!(surface.group, None);
        assert!(surface.area.y > 0);
        assert!(!surface.area.contains((0, 0).into()));
    }
}

#[test]
fn a_popup_cannot_swallow_the_release_of_a_terminal_selection() {
    let mut app = fixture();
    crate::mouse::handle_mouse(
        &mut app,
        mouse(MouseEventKind::Down(MouseButton::Left), 2, 4),
    );
    crate::mouse::handle_mouse(
        &mut app,
        mouse(MouseEventKind::Drag(MouseButton::Left), 6, 4),
    );
    assert!(app.terminal_selection_drag.is_some());
    app.change_dir_active = true;
    crate::mouse::handle_mouse(&mut app, mouse(MouseEventKind::Up(MouseButton::Left), 6, 4));
    assert!(app.terminal_selection_drag.is_none());
    assert_eq!(
        app.terminals[0]
            .terminal_pty
            .as_ref()
            .unwrap()
            .grid
            .selected_text()
            .as_deref(),
        Some("hello")
    );
    assert!(!app.editor().unwrap().has_selection());
}
