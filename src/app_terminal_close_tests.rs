use super::*;
use crate::terminal::TerminalEmulator;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use std::sync::mpsc::Sender;

fn running_terminal(app: &mut App, index: usize) -> Sender<()> {
    let mut terminal = TerminalEmulator::test_with_output(40, 10, "still running");
    let running = terminal.test_keep_running();
    app.terminals[index].terminal_pty = Some(terminal);
    running
}

#[test]
fn editor_terminal_close_button_stops_session_without_reopening_the_dock() {
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

    assert!(running.send(()).is_ok());
    assert!(app.terminal_is_in_editor(0));
    app.tabs[app.active_tab].pinned = false;
    app.close_tab(app.active_tab);
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
