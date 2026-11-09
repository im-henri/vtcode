use super::*;
use crate::{app::TerminalTab, app_terminal_selection::TerminalSurface, terminal::TerminalEmulator};
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};

fn terminal(fixture: &mut Fixture, output: &str) -> TerminalSurface {
    fixture.app.active_pane = ActivePane::Editor;
    fixture.app.show_terminal_sidebar = true;
    fixture.app.terminal_panel_focused = true;
    fixture.app.terminals[0].is_placeholder = false;
    fixture.app.terminals[0].terminal_cwd = fixture.app.explorer.root.clone();
    fixture.app.terminals[0].terminal_pty = Some(TerminalEmulator::test_with_output(200, 5, output));
    let surface = TerminalSurface { index: 0, area: Rect::new(2, 3, 200, 5), group: None };
    fixture.app.terminal_surfaces = vec![surface];
    surface
}

fn click(fixture: &mut Fixture) {
    for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
        crate::mouse::handle_mouse(&mut fixture.app, MouseEvent {
            kind, column: 16, row: 3, modifiers: KeyModifiers::CONTROL,
        });
    }
}

#[test]
fn remote_terminal_hover_sends_no_requests_and_click_loads_exact_diagnostic_location() {
    let mut fixture = setup();
    let path = file(&fixture, "nested/source.c", &("x".repeat(80) + "\n").repeat(5000));
    let surface = terminal(&mut fixture, "Error in \"nested/source.c\", line 4755, column 49: Undeclared variable.");
    fixture.app.mouse_col = 16;
    fixture.app.mouse_row = 3;
    fixture.app.update_link_hover_modifiers(KeyModifiers::CONTROL);
    let requests = fixture.traffic.load(Ordering::Relaxed);
    let mut output = Terminal::new(TestBackend::new(204, 10)).unwrap();
    for _ in 0..100 {
        fixture.app.terminal_links.begin_frame();
        output.draw(|frame| crate::app_terminal_links::draw(frame, &mut fixture.app, surface)).unwrap();
    }
    assert_eq!(fixture.traffic.load(Ordering::Relaxed), requests, "Ctrl-hover must never access the remote filesystem");
    fixture.hold.store(true, Ordering::Release);
    let began = Instant::now();
    click(&mut fixture);
    assert!(began.elapsed() < Duration::from_secs(1), "Ctrl-click must not wait on remote I/O");
    until(|| fixture.blocked.load(Ordering::Acquire));
    assert_eq!(fixture.app.editor().unwrap().rope.to_string(), "origin\n");
    fixture.hold.store(false, Ordering::Release);
    drain(&mut fixture);
    let editor = fixture.app.editor().unwrap();
    assert_eq!(editor.file_path.as_ref(), Some(&path));
    assert_eq!((editor.cursor_line, editor.cursor_col), (4754, 48));
    assert!(crate::remote::workspace::is_view_key(editor.file_path.as_ref().unwrap()));
    assert!(!fixture.app.terminal_panel_focused);
}

#[test]
fn remote_terminal_links_resolve_against_shell_cwd_and_keep_absolute_paths_remote() {
    let mut fixture = setup();
    let path = file(&fixture, "nested/source.c", "first\nsecond\nthird line\n");
    let folder = key(&fixture, "nested");
    let workspace = fixture.app.remote_workspace.as_ref().unwrap().clone();
    let uri = workspace.lsp_uri(url::Url::from_file_path(&folder).unwrap().as_str(), true).unwrap();
    terminal(&mut fixture, &format!("\x1b]7;{uri}\x07Error in \"source.c\", line 3, column 4"));
    let requests = fixture.traffic.load(Ordering::Relaxed);
    let absolute = crate::app_terminal_links::resolve_path(&fixture.app, 0, "/my/work/path/file.c").unwrap();
    assert!(crate::remote::workspace::is_view_key(&absolute));
    assert!(workspace.remote_path(&absolute).unwrap().is_absolute());
    assert_eq!(fixture.traffic.load(Ordering::Relaxed), requests);
    click(&mut fixture);
    drain(&mut fixture);
    let editor = fixture.app.editor().unwrap();
    assert_eq!(editor.file_path.as_ref(), Some(&path));
    assert_eq!((editor.cursor_line, editor.cursor_col), (2, 3));
}

#[test]
fn a_delayed_remote_terminal_link_cannot_steal_focus_from_another_terminal() {
    let mut fixture = setup();
    file(&fixture, "source.c", "source text\n");
    terminal(&mut fixture, "Error in \"source.c\", line 1, column 1");
    fixture.hold.store(true, Ordering::Release);
    click(&mut fixture);
    until(|| fixture.blocked.load(Ordering::Acquire));
    let mut other = TerminalTab::new("other".into(), fixture.app.explorer.root.clone());
    other.terminal_pty = Some(TerminalEmulator::test_with_output(20, 3, "other terminal"));
    fixture.app.terminals.push(other);
    fixture.app.active_terminal = 1;
    fixture.app.cancel_stale_remote_location();
    fixture.hold.store(false, Ordering::Release);
    drain(&mut fixture);
    assert_eq!(fixture.app.active_terminal, 1);
    assert!(fixture.app.terminal_panel_focused);
    assert_eq!(fixture.app.editor().unwrap().rope.to_string(), "origin\n");
}

#[test]
fn further_terminal_input_cancels_a_pending_remote_file_link() {
    for keyboard in [false, true] {
        let mut fixture = setup();
        file(&fixture, "source.c", "source text\n");
        terminal(&mut fixture, "Error in \"source.c\", line 1, column 1");
        fixture.hold.store(true, Ordering::Release);
        click(&mut fixture);
        until(|| fixture.blocked.load(Ordering::Acquire));
        if keyboard {
            crate::input_keyboard::handle_keyboard_event(&mut fixture.app,
                crossterm::event::KeyEvent::new(crossterm::event::KeyCode::Char('x'), KeyModifiers::NONE));
        } else {
            for kind in [MouseEventKind::Down(MouseButton::Left), MouseEventKind::Up(MouseButton::Left)] {
                crate::mouse::handle_mouse(&mut fixture.app, MouseEvent { kind, column: 16, row: 3, modifiers: KeyModifiers::NONE });
            }
        }
        fixture.hold.store(false, Ordering::Release);
        drain(&mut fixture);
        assert!(fixture.app.terminal_panel_focused);
        assert_eq!(fixture.app.editor().unwrap().rope.to_string(), "origin\n");
    }
}
