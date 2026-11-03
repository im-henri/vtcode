use super::*;
use crate::{
    app::{SplitDirection, Tab, TerminalTab},
    editor::Editor,
    terminal::TerminalEmulator,
};
use std::sync::{Arc, Mutex};

fn fixture(output: &str, surface_kind: usize) -> (App, Arc<Mutex<Vec<u8>>>) {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::test_with_text(
        &"line of editor text\n".repeat(40),
    ))];
    app.active_pane = ActivePane::Editor;
    app.active_tab = 0;
    app.scroll_speed = 3;
    app.show_terminal_sidebar = true;
    app.terminal_panel_focused = true;
    let mut pty = TerminalEmulator::test_with_output(24, 4, output);
    let input = pty.test_capture_input();
    app.terminals[0].terminal_pty = Some(pty);
    let group = match surface_kind {
        1 => {
            app.terminal_panel_focused = false;
            app.active_pane = ActivePane::Explorer;
            None
        }
        2 | 3 => {
            assert!(app.detach_terminal_to_group(0, 0));
            if surface_kind == 3 {
                let selected = app.active_tab;
                app.create_split_group_dir(selected, SplitDirection::Horizontal);
                let terminal_group = app.active_group;
                app.switch_group(0);
                Some(terminal_group)
            } else {
                Some(app.active_group)
            }
        }
        _ => None,
    };
    app.terminal_surfaces = vec![TerminalSurface {
        index: 0,
        area: Rect::new(5, 6, 24, 4),
        group,
    }];
    (app, input)
}

fn event(kind: MouseEventKind, modifiers: KeyModifiers) -> MouseEvent {
    MouseEvent {
        kind,
        column: 8,
        row: 7,
        modifiers,
    }
}

fn wheel(app: &mut App, kind: MouseEventKind, modifiers: KeyModifiers) {
    crate::mouse::handle_mouse(app, event(kind, modifiers));
}

fn output(app: &mut App, bytes: &[u8]) {
    let pty = app.terminals[0].terminal_pty.as_mut().unwrap();
    pty.test_queue_output(bytes);
    pty.process();
}

#[test]
fn full_screen_wheel_reaches_child_without_mouse_reporting_in_every_terminal_surface() {
    for mode in [47, 1047, 1049] {
        for application in [false, true] {
            for surface_kind in 0..4 {
                let modes = format!(
                    "\x1b[?{mode}h{}Vim",
                    if application { "\x1b[?1h" } else { "" }
                );
                let (mut app, input) = fixture(&modes, surface_kind);
                let focus = (
                    app.active_group,
                    app.active_tab,
                    app.active_terminal,
                    app.terminal_panel_focused,
                );
                assert!(
                    app.terminal_wants_mouse(event(MouseEventKind::ScrollUp, KeyModifiers::NONE))
                );
                for kind in [MouseEventKind::ScrollUp, MouseEventKind::ScrollDown] {
                    wheel(&mut app, kind, KeyModifiers::NONE);
                }
                let up: &[u8] = if application { b"\x1bOA" } else { b"\x1b[A" };
                let down: &[u8] = if application { b"\x1bOB" } else { b"\x1b[B" };
                assert_eq!(
                    *input.lock().unwrap(),
                    [up.repeat(3), down.repeat(3)].concat()
                );
                assert_eq!(
                    app.terminals[0]
                        .terminal_pty
                        .as_ref()
                        .unwrap()
                        .grid
                        .scroll_view,
                    0
                );
                assert_eq!(
                    (
                        app.active_group,
                        app.active_tab,
                        app.active_terminal,
                        app.terminal_panel_focused
                    ),
                    focus,
                    "wheel input must not move keyboard focus"
                );
            }
        }
    }
}

#[test]
fn native_wheel_reports_take_priority_over_alternate_scroll_and_are_not_multiplied() {
    for tracking in [1000, 1002, 1003] {
        for sgr in [false, true] {
            let (mut app, input) = fixture(
                &format!(
                    "\x1b[?1049;1;{tracking}h{}",
                    if sgr { "\x1b[?1006h" } else { "" }
                ),
                0,
            );
            for kind in [
                MouseEventKind::ScrollUp,
                MouseEventKind::ScrollDown,
                MouseEventKind::ScrollLeft,
                MouseEventKind::ScrollRight,
            ] {
                wheel(&mut app, kind, KeyModifiers::CONTROL);
            }
            let expected = if sgr {
                b"\x1b[<80;4;2M\x1b[<81;4;2M\x1b[<82;4;2M\x1b[<83;4;2M".to_vec()
            } else {
                [80u8, 81, 82, 83]
                    .into_iter()
                    .flat_map(|code| [0x1b, b'[', b'M', code + 32, 36, 34])
                    .collect()
            };
            assert_eq!(*input.lock().unwrap(), expected);
        }
    }
}

#[test]
fn alternate_scroll_opt_out_reenable_reset_and_return_to_shell_are_honored() {
    let history = (0..12)
        .map(|index| format!("line {index}\r\n"))
        .collect::<String>();
    let (mut app, input) = fixture(&history, 0);
    app.terminals[0].terminal_pty.as_mut().unwrap().scroll_up(2);
    output(&mut app, b"\x1b[?1049h");
    wheel(&mut app, MouseEventKind::ScrollUp, KeyModifiers::NONE);
    assert_eq!(*input.lock().unwrap(), b"\x1b[A".repeat(3));
    input.lock().unwrap().clear();
    output(&mut app, b"\x1b[?1007l");
    wheel(&mut app, MouseEventKind::ScrollDown, KeyModifiers::NONE);
    assert!(input.lock().unwrap().is_empty());
    output(&mut app, b"\x1b[?1007h");
    wheel(&mut app, MouseEventKind::ScrollDown, KeyModifiers::NONE);
    assert_eq!(*input.lock().unwrap(), b"\x1b[B".repeat(3));
    input.lock().unwrap().clear();
    output(&mut app, b"\x1b[?1049l");
    let before = app.terminals[0]
        .terminal_pty
        .as_ref()
        .unwrap()
        .grid
        .scroll_view;
    wheel(&mut app, MouseEventKind::ScrollUp, KeyModifiers::NONE);
    let pty = app.terminals[0].terminal_pty.as_ref().unwrap();
    assert_eq!(
        pty.grid.scroll_view,
        (before + 3).min(pty.grid.scrollback.len())
    );
    assert!(input.lock().unwrap().is_empty());
    output(&mut app, b"\x1b[?1007l\x1bc\x1b[?1049h");
    wheel(&mut app, MouseEventKind::ScrollUp, KeyModifiers::NONE);
    assert_eq!(
        *input.lock().unwrap(),
        b"\x1b[A".repeat(3),
        "terminal reset restores the default"
    );
}

#[test]
fn shift_override_and_active_local_selections_keep_wheel_events_out_of_the_child() {
    let (mut app, input) = fixture("\x1b[?1049hselectable text", 0);
    wheel(&mut app, MouseEventKind::ScrollUp, KeyModifiers::SHIFT);
    assert!(input.lock().unwrap().is_empty());
    crate::mouse::handle_mouse(
        &mut app,
        event(MouseEventKind::Down(MouseButton::Left), KeyModifiers::SHIFT),
    );
    wheel(&mut app, MouseEventKind::ScrollDown, KeyModifiers::NONE);
    assert!(input.lock().unwrap().is_empty());
    crate::mouse::handle_mouse(
        &mut app,
        event(MouseEventKind::Up(MouseButton::Left), KeyModifiers::SHIFT),
    );
    wheel(&mut app, MouseEventKind::ScrollDown, KeyModifiers::NONE);
    assert_eq!(*input.lock().unwrap(), b"\x1b[B".repeat(3));
}

#[test]
fn alternate_scroll_uses_win32_key_press_and_release_records_when_requested() {
    let (mut app, input) = fixture("\x1b[?1049;9001hVim", 0);
    app.scroll_speed = 2;
    wheel(&mut app, MouseEventKind::ScrollUp, KeyModifiers::NONE);
    wheel(&mut app, MouseEventKind::ScrollDown, KeyModifiers::CONTROL);
    let up = b"\x1b[38;0;0;1;0;1_\x1b[38;0;0;0;0;1_".repeat(2);
    let down = b"\x1b[40;0;0;1;8;1_\x1b[40;0;0;0;8;1_".repeat(2);
    assert_eq!(*input.lock().unwrap(), [up, down].concat());
}

#[test]
fn alternate_scroll_preserves_cursor_key_modifiers_and_respects_scroll_speed() {
    let (mut app, input) = fixture("\x1b[?1049;1hVim", 0);
    app.scroll_speed = 1;
    wheel(&mut app, MouseEventKind::ScrollUp, KeyModifiers::CONTROL);
    wheel(&mut app, MouseEventKind::ScrollDown, KeyModifiers::ALT);
    assert_eq!(input.lock().unwrap().as_slice(), b"\x1b[1;5A\x1b[1;3B");
}

#[test]
fn unreported_horizontal_wheel_events_stay_in_terminal_instead_of_scrolling_the_editor() {
    for alternate in [false, true] {
        let (mut app, input) = fixture(if alternate { "\x1b[?1049hVim" } else { "shell" }, 0);
        app.editor_mut().unwrap().horizontal_scroll = 5;
        for kind in [MouseEventKind::ScrollLeft, MouseEventKind::ScrollRight] {
            wheel(&mut app, kind, KeyModifiers::NONE);
            assert_eq!(app.editor().unwrap().horizontal_scroll, 5);
        }
        assert!(input.lock().unwrap().is_empty());
    }
}

#[test]
fn wheel_targets_hovered_full_screen_session_without_stealing_focus() {
    let (mut app, other_input) = fixture("shell", 0);
    let mut terminal = TerminalTab::new("Remote Vim".into(), app.explorer.root.clone());
    let mut pty = TerminalEmulator::test_with_output(24, 4, "\x1b[?1049;1hVim");
    let input = pty.test_capture_input();
    terminal.terminal_pty = Some(pty);
    app.terminals.push(terminal);
    app.terminal_surfaces[0].index = 1;
    app.active_terminal = 0;
    app.terminal_panel_focused = true;
    wheel(&mut app, MouseEventKind::ScrollDown, KeyModifiers::NONE);
    assert_eq!(*input.lock().unwrap(), b"\x1bOB".repeat(3));
    assert!(other_input.lock().unwrap().is_empty());
    assert_eq!(app.focused_terminal_index(), Some(0));
}

#[test]
#[ignore = "requires a local Vim executable and a real PTY; VTCODE_TEST_VIM overrides the executable"]
fn real_vim_receives_wheel_with_and_without_mouse_reporting() {
    use crossterm::event::KeyCode;
    use std::time::{Duration, Instant};
    fn keys(app: &mut App, command: &str) {
        for code in std::iter::once(KeyCode::Esc)
            .chain(command.chars().map(KeyCode::Char))
            .chain(std::iter::once(KeyCode::Enter))
        {
            crate::input_keyboard::handle_keyboard_event(
                app,
                KeyEvent::new(code, KeyModifiers::NONE),
            );
        }
    }
    let program = std::env::var("VTCODE_TEST_VIM").unwrap_or_else(|_| {
        if cfg!(windows) {
            "C:/Program Files/Git/usr/bin/vim.exe".into()
        } else {
            "vim".into()
        }
    });
    for reporting in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.txt");
        std::fs::write(
            &source,
            (1..=200)
                .map(|line| format!("line {line:03}\n"))
                .collect::<String>(),
        )
        .unwrap();
        let args = vec![
            "-Nu".into(),
            "NONE".into(),
            "-i".into(),
            "NONE".into(),
            "-n".into(),
            "-N".into(),
            "--noplugin".into(),
            "-T".into(),
            "xterm-256color".into(),
            "-c".into(),
            format!(
                "set laststatus=2 statusline=VT-VIM-READY ttymouse=sgr mouse={}",
                if reporting { "a" } else { "" }
            ),
            source.to_string_lossy().replace('\\', "/"),
        ];
        let (mut app, _) = fixture("", 0);
        app.terminal_surfaces[0].area = Rect::new(0, 0, 80, 24);
        app.terminals[0].terminal_pty = Some(
            TerminalEmulator::spawn_command_in_dir(&program, &args, 80, 24, directory.path())
                .expect("launch isolated Vim without user config, swap, or history"),
        );
        let ready = Instant::now() + Duration::from_secs(10);
        loop {
            let pty = app.terminals[0].terminal_pty.as_mut().unwrap();
            pty.process();
            let screen: String = pty
                .grid
                .cells
                .iter()
                .flat_map(|row| row.iter().map(|cell| cell.ch))
                .collect();
            if screen.contains("VT-VIM-READY") {
                break;
            }
            assert!(Instant::now() < ready, "Vim did not become ready: {screen}");
            std::thread::sleep(Duration::from_millis(5));
        }
        let grid = &app.terminals[0].terminal_pty.as_ref().unwrap().grid;
        assert!(grid.is_alt_screen());
        assert_eq!(grid.mouse_tracking != 0, reporting);
        for _ in 0..5 {
            crate::mouse::handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    column: 20,
                    row: 10,
                    modifiers: KeyModifiers::NONE,
                },
            );
        }
        keys(
            &mut app,
            ":call writefile([string(line('.')), string(line('w0'))], 'position.txt')",
        );
        let position = directory.path().join("position.txt");
        let ready = Instant::now() + Duration::from_secs(8);
        let positions = loop {
            if let Ok(text) = std::fs::read_to_string(&position)
                && text.ends_with('\n')
                && let Ok(positions) = text
                    .lines()
                    .map(str::parse::<usize>)
                    .collect::<Result<Vec<_>, _>>()
                && positions.len() == 2
            {
                break positions;
            }
            app.terminals[0].terminal_pty.as_mut().unwrap().process();
            assert!(
                Instant::now() < ready,
                "Vim did not acknowledge the wheel test"
            );
            std::thread::sleep(Duration::from_millis(5));
        };
        assert!(
            positions[0] > 1,
            "wheel input must reach Vim: {positions:?}"
        );
        if reporting {
            assert!(
                positions[1] > 1,
                "Vim's native wheel event must scroll its viewport"
            );
        } else {
            assert_eq!(
                positions[0], 16,
                "five wheel ticks generate fifteen cursor steps"
            );
        }
        keys(&mut app, ":q!");
    }
}
