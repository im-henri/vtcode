use super::*;
use crate::terminal::TerminalEmulator;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use std::sync::mpsc::Sender;

fn running_terminal(app: &mut App, index: usize) -> Sender<()> {
    let mut terminal = TerminalEmulator::test_with_output(40, 10, "still running");
    let running = terminal.test_keep_running();
    app.terminals[index].terminal_pty = Some(terminal);
    running
}

fn key(app: &mut App, code: KeyCode) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, KeyModifiers::NONE));
}

fn confirm_close(app: &mut App) {
    assert_eq!(crate::ui_modal::active(app), Some(crate::ui_modal::Kind::CloseTerminal));
    key(app, KeyCode::Left);
    key(app, KeyCode::Enter);
    assert!(app.terminal_close.is_none());
}

#[test]
fn editor_terminal_close_button_asks_before_stopping_session() {
    let mut app = App::test_new();
    app.new_terminal_in_editor_tab();
    let running = running_terminal(&mut app, 0);
    let mut terminal = Terminal::new(TestBackend::new(140, 35)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let area = app.tab_bar_area.unwrap();
    let hits = crate::ui_chrome_tabs::TabBarHits::new(&app, area);
    let (_, _, close) = hits
        .tabs
        .iter()
        .find(|(index, _, _)| *index == app.active_tab)
        .unwrap();
    let (column, row) = (close.x, close.y);

    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind,
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
        );
    }
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();

    assert!(running.send(()).is_ok(), "the job must survive until confirmation");
    assert_eq!(app.modal.focus, 1, "Cancel must be selected by default");
    let screen: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
    assert!(screen.contains("Close Running Terminal"));
    assert!(screen.contains("any running jobs"));
    let close = app.modal.buttons[0].0;
    crate::mouse::handle_mouse(&mut app, MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left), column: close.x, row: close.y,
        modifiers: KeyModifiers::NONE,
    });

    assert!(
        running.send(()).is_err(),
        "closing the tab must drop its terminal session"
    );
    assert!(!app.terminal_is_open(0));
    assert!(!app.show_terminal_sidebar);
    assert!(!app.terminal_sidebar_revealed);
    assert!(!app.terminal_panel_focused);
    assert!(app.tabs.iter().all(|tab| tab.kind != TabKind::Terminal));
}

#[test]
fn closing_editor_terminal_preserves_the_running_dock_session() {
    let mut app = App::test_new();
    app.toggle_terminal_sidebar();
    app.terminals[0].name = "Dock session".into();
    let dock_running = running_terminal(&mut app, 0);
    app.new_terminal_in_editor_tab();
    let editor_running = running_terminal(&mut app, 1);

    app.close_tab(app.active_tab);
    assert!(editor_running.send(()).is_ok());
    confirm_close(&mut app);

    assert!(editor_running.send(()).is_err());
    assert!(dock_running.send(()).is_ok());
    assert_eq!(app.terminals.len(), 1);
    assert_eq!(app.terminals[0].name, "Dock session");
    assert!(app.show_terminal_sidebar);
    assert!(app.terminal_is_in_dock(0, TerminalDock::Right));
    assert!(app.tabs.iter().all(|tab| tab.kind != TabKind::Terminal));
}

#[test]
fn bulk_tab_close_stops_only_the_selected_terminal_sessions() {
    let commands: [fn(&mut App); 3] = [App::close_saved_tabs, App::close_all_tabs, |app| {
        app.close_tabs_matching(|_| true)
    }];
    for (command, close) in commands.into_iter().enumerate() {
        let mut app = App::test_new();
        for _ in 0..4 {
            app.new_terminal_tab();
        }
        let running: Vec<_> = (0..4)
            .map(|index| running_terminal(&mut app, index))
            .collect();
        app.terminals[1].name = "Retained dock session".into();
        for index in [0, 2, 3] {
            assert!(app.detach_terminal_to_group(index, app.active_group));
        }
        // The all-pane commands also close terminals in an inactive split pane.
        if command != 2 {
            app.create_split_group(app.active_tab);
        }

        close(&mut app);
        assert!(running.iter().all(|running| running.send(()).is_ok()));
        confirm_close(&mut app);

        for (index, running) in running.iter().enumerate() {
            assert_eq!(running.send(()).is_ok(), index == 1, "terminal {index}");
        }
        assert_eq!(app.terminals.len(), 1);
        assert_eq!(app.terminals[0].name, "Retained dock session");
    }
}

#[test]
fn closing_editor_terminal_collapses_its_split_and_rebases_the_remaining_terminal() {
    let mut app = App::test_new();
    app.tabs.clear();
    app.new_terminal_in_editor_tab();
    let first = running_terminal(&mut app, 0);
    app.new_terminal_in_editor_tab();
    let second = running_terminal(&mut app, 1);
    app.create_split_group(app.active_tab);
    app.switch_group(0);

    app.close_tab(0);
    confirm_close(&mut app);

    assert!(first.send(()).is_err());
    assert!(second.send(()).is_ok());
    assert!(!app.split_active());
    assert_eq!(app.terminals.len(), 1);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].kind, TabKind::Terminal);
    assert_eq!(app.tabs[0].terminal_idx, Some(0));
    assert!(!app.show_terminal_sidebar);
}

#[test]
fn closing_editor_terminal_removes_views_in_other_desks() {
    let mut app = App::test_new();
    app.new_terminal_in_editor_tab();
    let running = running_terminal(&mut app, 0);
    app.new_desk("Copy", true);
    assert!(app.tabs.iter().any(|tab| tab.kind == TabKind::Terminal));

    app.close_tab(app.active_tab);
    confirm_close(&mut app);

    assert!(running.send(()).is_err());
    assert!(
        app.tabs
            .iter()
            .chain(app.groups.iter().flat_map(|group| &group.tabs))
            .all(|tab| tab.kind != TabKind::Terminal)
    );
    assert!(!app.terminal_is_open(0));
}

#[test]
fn pinned_editor_terminal_stays_running_until_unpinned() {
    let mut app = App::test_new();
    app.new_terminal_in_editor_tab();
    let running = running_terminal(&mut app, 0);
    app.tabs[app.active_tab].pinned = true;

    app.close_tab(app.active_tab);
    app.close_all_tabs();

    assert!(app.terminal_close.is_none());
    assert!(app.terminals[0].terminal_pty.is_some());
    assert!(app.terminal_is_in_editor(0));
    app.tabs[app.active_tab].pinned = false;
    app.close_tab(app.active_tab);
    confirm_close(&mut app);
    assert!(running.send(()).is_err());
}

#[test]
fn explicitly_moving_editor_terminal_to_dock_keeps_it_running() {
    let mut app = App::test_new();
    app.new_terminal_in_editor_tab();
    let running = running_terminal(&mut app, 0);

    assert!(app.move_terminal_to_dock(0, TerminalDock::Right));

    assert!(running.send(()).is_ok());
    assert!(app.terminal_is_in_dock(0, TerminalDock::Right));
    assert!(app.show_terminal_sidebar);
}

#[test]
fn live_terminal_close_can_be_cancelled_without_losing_focus_selection_or_input() {
    for editor_tab in [false, true] {
        for cancel in [KeyCode::Esc, KeyCode::Enter] {
            let mut app = App::test_new();
            if editor_tab { app.new_terminal_in_editor_tab(); } else { app.toggle_terminal_sidebar(); }
            let running = running_terminal(&mut app, 0);
            let input = app.terminals[0].terminal_pty.as_mut().unwrap().test_capture_input();
            let focus = (app.active_pane, app.active_tab, app.active_terminal, app.terminal_panel_focused);
            let count = app.tabs.len();

            app.execute_palette_command("close_terminal");
            assert!(running.send(()).is_ok());
            crate::app_event_loop::handle_paste_input(&mut app, "do not send this to the shell");
            // Repeated input from a held key cannot accept a destructive action.
            crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new_with_kind(
                KeyCode::Left, KeyModifiers::NONE, KeyEventKind::Repeat));
            crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new_with_kind(
                KeyCode::Enter, KeyModifiers::NONE, KeyEventKind::Repeat));
            assert!(app.terminal_close.is_some());
            assert_eq!(app.modal.focus, 1);
            key(&mut app, cancel);

            assert!(app.terminal_close.is_none());
            assert!(running.send(()).is_ok());
            assert_eq!(app.tabs.len(), count);
            assert_eq!((app.active_pane, app.active_tab, app.active_terminal, app.terminal_panel_focused), focus);
            assert!(input.lock().unwrap().is_empty(), "dialog keys must not reach the shell");
        }
    }
}

#[test]
fn empty_failed_and_exited_terminals_close_without_confirmation() {
    for state in 0..3 {
        let mut app = App::test_new();
        app.new_terminal_in_editor_tab();
        match state {
            0 => {},
            1 => app.terminals[0].terminal_launch_error = Some("Shell unavailable".into()),
            _ => app.terminals[0].terminal_pty = Some(TerminalEmulator::test_with_output(40, 10, "finished")),
        }
        app.close_tab(app.active_tab);
        assert!(app.terminal_close.is_none());
        assert!(!app.terminal_is_open(0));
        assert!(app.tabs.iter().all(|tab| tab.kind != TabKind::Terminal));
    }
}

#[test]
fn terminal_exits_during_confirmation_never_close_a_different_session() {
    for exits_target in [false, true] {
        let mut app = App::test_new();
        app.new_terminal_tab();
        app.new_terminal_tab();
        let first = running_terminal(&mut app, 0);
        let second = running_terminal(&mut app, 1);
        app.close_terminal_tab_at(1);
        assert!(app.terminal_close.is_some());

        let (exited, survivor) = if exits_target { (second, first) } else { (first, second) };
        exited.send(()).unwrap();
        app.process_terminals();
        assert!(exited.send(()).is_err());
        assert!(survivor.send(()).is_ok());
        if exits_target {
            assert!(app.terminal_close.is_none());
            key(&mut app, KeyCode::Enter);
            assert!(survivor.send(()).is_ok());
        } else {
            confirm_close(&mut app);
            assert!(survivor.send(()).is_err(), "confirmation must follow the original session");
        }
    }
}

#[test]
fn bulk_close_cancel_preserves_saved_files_and_live_terminal_views() {
    for close in [App::close_saved_tabs as fn(&mut App), App::close_all_tabs, |app| app.close_tabs_matching(|_| true)] {
        let mut app = App::test_new();
        app.tabs = vec![Tab::new_file(Editor::test_with_text("saved"))];
        app.new_terminal_in_editor_tab();
        let running = running_terminal(&mut app, 0);
        let views: Vec<_> = app.tabs.iter().map(|tab| tab.editor.navigation_id).collect();
        close(&mut app);
        assert!(running.send(()).is_ok());
        assert!(app.terminal_close.is_some());
        key(&mut app, KeyCode::Esc);
        assert_eq!(app.tabs.iter().map(|tab| tab.editor.navigation_id).collect::<Vec<_>>(), views);
        assert!(running.send(()).is_ok());
    }
}

#[test]
fn confirmed_bulk_close_preserves_unsaved_prompts_after_terminal_index_changes() {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text("unsaved");
    editor.insert_char('!');
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.new_terminal_in_editor_tab();
    let running = running_terminal(&mut app, 0);
    app.active_tab = 0;
    app.close_all_tabs();
    assert!(app.terminal_close.is_some());
    assert!(!app.confirm_close_active);
    confirm_close(&mut app);
    assert!(running.send(()).is_err());
    assert!(app.confirm_close_active);
    assert_eq!(app.confirm_close_tab, 0);
    assert_eq!(app.editor().unwrap().rope.to_string(), "!unsaved");
}

#[test]
fn confirmed_bulk_close_uses_original_views_after_an_unrelated_terminal_exits() {
    let mut app = App::test_new();
    app.new_terminal_tab();
    app.new_terminal_tab();
    let first = running_terminal(&mut app, 0);
    let second = running_terminal(&mut app, 1);
    assert!(app.detach_terminal_to_group(1, app.active_group));
    app.close_all_tabs();
    assert!(app.terminal_close.is_some());
    first.send(()).unwrap();
    app.process_terminals();
    assert!(second.send(()).is_ok());
    confirm_close(&mut app);
    assert!(second.send(()).is_err());
    assert!(!app.terminal_is_open(0));
}

#[test]
fn dock_close_button_can_cancel_then_close_only_the_named_terminal() {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::test_with_text("keep this file"))];
    app.new_terminal_tab();
    app.new_terminal_tab();
    app.terminals[0].name = "Long build".into();
    app.terminals[1].name = "Other shell".into();
    let first = running_terminal(&mut app, 0);
    let second = running_terminal(&mut app, 1);
    app.show_terminal_sidebar = true;
    app.terminal_sidebar_revealed = true;
    app.terminal_panel_focused = false;
    let mut terminal = Terminal::new(TestBackend::new(160, 40)).unwrap();
    let click = |app: &mut App, area: ratatui::layout::Rect| {
        crate::mouse::handle_mouse(app, MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left), column: area.x, row: area.y,
            modifiers: KeyModifiers::NONE,
        });
    };
    for confirm in [false, true] {
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        let close = app.terminal_sidebar_close_areas.iter().find(|(_, index)| *index == 0).unwrap().0;
        click(&mut app, close);
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        assert_eq!(crate::ui_modal::active(&app), Some(crate::ui_modal::Kind::CloseTerminal));
        let screen: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
        assert!(screen.contains("Long build"));
        let button = app.modal.buttons[usize::from(!confirm)].0;
        click(&mut app, button);
        assert!(app.terminal_close.is_none());
        assert!(!app.terminal_panel_focused);
        if !confirm {
            assert_eq!(app.terminals.len(), 2);
            assert_eq!(app.active_terminal, 1);
            assert!(app.terminals[0].terminal_pty.is_some());
        }
    }
    assert!(first.send(()).is_err());
    assert!(second.send(()).is_ok());
    assert_eq!(app.terminals.len(), 1);
    assert_eq!(app.terminals[0].name, "Other shell");
    assert_eq!(app.editor().unwrap().rope.to_string(), "keep this file");
}
