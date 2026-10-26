use super::*;
use crate::{app_terminal_selection::TerminalSurface, terminal::TerminalEmulator};
use ratatui::layout::Rect;

fn fixture(count: usize) -> (App, Vec<std::sync::mpsc::Sender<()>>) {
    let mut app = App::test_new();
    app.terminals.clear();
    let mut exits = Vec::new();
    for index in 0..count {
        let mut pty = TerminalEmulator::test_with_output(20, 3, "");
        exits.push(pty.test_keep_running());
        let mut tab = TerminalTab::new(format!("terminal {index}"), "workspace".into());
        tab.terminal_pty = Some(pty);
        app.terminals.push(tab);
    }
    (app, exits)
}

#[test]
fn hidden_output_is_drained_without_redrawing_and_visible_splits_redraw() {
    let (mut app, _exits) = fixture(3);
    app.terminal_surfaces = vec![
        TerminalSurface {
            index: 1,
            area: Rect::new(10, 5, 20, 3),
            group: Some(2),
        },
        TerminalSurface {
            index: 2,
            area: Rect::default(),
            group: None,
        },
    ];
    for (index, expected) in [(0, false), (1, true), (2, false)] {
        app.terminals[index]
            .terminal_pty
            .as_ref()
            .unwrap()
            .test_queue_output(b"x");
        assert_eq!(app.process_terminals(), expected);
        assert_eq!(
            app.terminals[index]
                .terminal_pty
                .as_ref()
                .unwrap()
                .grid
                .cells[0][0]
                .ch,
            'x'
        );
    }
    assert!(!app.process_terminals());
}

#[test]
fn terminal_budget_resumes_fairly_and_wakes_for_unvisited_exits() {
    let (mut app, exits) = fixture(5);
    for terminal in &app.terminals {
        terminal
            .terminal_pty
            .as_ref()
            .unwrap()
            .test_queue_output(b"x");
    }
    let future = Instant::now() + Duration::from_secs(60);
    for index in 0..5 {
        let wake = app.background.wake_handle().generation();
        assert!(!app.process_terminals_until(future, || false));
        assert_eq!(app.next_terminal_to_process, (index + 1) % 5);
        assert_ne!(app.background.wake_handle().generation(), wake);
        for (position, terminal) in app.terminals.iter().enumerate() {
            assert_eq!(
                terminal.terminal_pty.as_ref().unwrap().has_pending_output(),
                position > index
            );
        }
    }
    exits[4].send(()).unwrap();
    for _ in 0..5 {
        app.process_terminals_until(future, || false);
    }
    assert_eq!(app.terminals.len(), 4);
    app.next_terminal_to_process = 3;
    drop(exits);
    assert!(app.process_terminals_until(future, || true));
    assert_eq!(app.terminals.len(), 1);
    assert!(app.terminals[0].terminal_pty.is_none());
}

#[test]
fn empty_terminal_inventory_is_safe_after_a_budgeted_turn() {
    let (mut app, _exits) = fixture(0);
    app.next_terminal_to_process = usize::MAX;
    assert!(!app.process_terminals());
}

#[test]
fn expired_deadline_does_not_keep_quiet_terminals_waking_the_ui() {
    let (mut app, _exits) = fixture(5);
    let wake = app.background.wake_handle().generation();
    assert!(!app.process_terminals_until(Instant::now(), || true));
    assert_eq!(app.background.wake_handle().generation(), wake);
}

#[test]
fn terminals_observe_an_exhausted_shared_deadline_and_keep_the_remainder() {
    let (mut app, _exits) = fixture(3);
    for terminal in &app.terminals {
        terminal.terminal_pty.as_ref().unwrap().test_queue_output(&vec![b'x'; 32 * 1024]);
    }
    let wake = app.background.wake_handle().generation();
    let expired = Instant::now();
    for expected in [1, 2, 0] {
        app.process_terminals_until(expired, || true);
        assert_eq!(app.next_terminal_to_process, expected);
        assert!(app.terminals.iter().all(|terminal| terminal.terminal_pty.as_ref().unwrap().has_pending_output()));
    }
    assert!(app.background.wake_handle().generation() > wake);
    for _ in 0..200 {
        if app.terminals.iter().all(|terminal| !terminal.terminal_pty.as_ref().unwrap().has_pending_output()) { return; }
        app.process_terminals();
    }
    panic!("deferred terminal output never completed");
}

#[test]
fn input_interrupts_a_single_busy_terminal_and_remaining_output_resumes_exactly() {
    let (mut app, _exits) = fixture(1);
    let mut bytes = vec![0; 255];
    bytes.extend_from_slice("é\x1b[38;2;11;22;33m世界\r\n".repeat(300).as_bytes());
    app.terminals[0].terminal_pty.as_ref().unwrap().test_queue_output(&bytes);
    let mut checks = 0;
    assert!(!app.process_terminals_until(Instant::now() + Duration::from_secs(60), || {
        checks += 1;
        false
    }));
    assert_eq!(checks, 1, "a single terminal must observe arriving input");
    let pty = app.terminals[0].terminal_pty.as_ref().unwrap();
    assert!(pty.has_pending_output());
    assert_eq!(pty.grid.cells[0][0].ch, ' ', "yield in the middle of UTF-8");
    pty.test_queue_output(b"newer output");
    for _ in 0..1_000 {
        if !app.terminals[0].terminal_pty.as_ref().unwrap().has_pending_output() { break; }
        app.process_terminals_until(Instant::now() + Duration::from_secs(60), || false);
    }
    bytes.extend_from_slice(b"newer output");
    let expected = TerminalEmulator::test_with_output(20, 3, std::str::from_utf8(&bytes).unwrap());
    let actual = app.terminals[0].terminal_pty.as_ref().unwrap();
    assert!(!actual.has_pending_output());
    assert_eq!(actual.grid.cells, expected.grid.cells);
    assert_eq!(actual.grid.scrollback, expected.grid.scrollback);
    assert_eq!((actual.grid.cursor_row, actual.grid.cursor_col),
        (expected.grid.cursor_row, expected.grid.cursor_col));
}

#[test]
fn mid_batch_input_yields_without_starving_other_terminals_or_exits() {
    let (mut app, exits) = fixture(3);
    let bytes = b"build output\r\n".repeat(2_000);
    for tab in &app.terminals {
        tab.terminal_pty.as_ref().unwrap().test_queue_output(&bytes);
    }
    for expected in [1, 2, 0] {
        assert!(!app.process_terminals_until(Instant::now() + Duration::from_secs(60), || false));
        assert_eq!(app.next_terminal_to_process, expected);
        assert!(app.terminals.iter().all(|tab| tab.terminal_pty.as_ref().unwrap().has_pending_output()));
    }
    exits[1].send(()).unwrap();
    for _ in 0..3 {
        app.process_terminals_until(Instant::now() + Duration::from_secs(60), || false);
    }
    assert_eq!(app.terminals.len(), 2);
    assert_eq!(app.terminals[0].name, "terminal 0");
    assert_eq!(app.terminals[1].name, "terminal 2");
}

#[test]
#[ignore = "manual optimized-profile input handoff during sustained terminal output"]
fn benchmark_build_output_input_handoff() {
    use crate::{app::Tab, editor::Editor};
    use ratatui::{backend::TestBackend, Terminal};
    use std::{hint::black_box, time::Duration};

    let bytes = "\x1b[32mCompiling\x1b[0m project/file.rs:123: checking symbols and dependencies\r\n"
        .repeat(20_000).into_bytes();
    let mut samples = Vec::new();
    let mut frames = Vec::new();
    let mut checks = Vec::new();
    for _ in 0..31 {
        let (mut app, _exits) = fixture(1);
        app.tabs = vec![Tab::new_file(Editor::test_with_text(
            &"fn work(value: usize) { consume(value); }\n".repeat(2_000),
        ))];
        app.active_tab = 0;
        app.show_explorer = false;
        app.show_minimap = false;
        app.show_panel = false;
        app.show_terminal_sidebar = false;
        app.cursor_blinking = false;
        for effect in crate::animation_config::AnimationEffect::ALL {
            app.animation_settings.set(effect, false);
        }
        let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        app.terminals[0].terminal_pty.as_mut().unwrap().resize(120, 30);
        app.terminals[0].terminal_pty.as_ref().unwrap().test_queue_output(&bytes);
        let mut polls = 0;
        let start = Instant::now();
        // Input appears at the first opportunity to interrupt ongoing output.
        app.process_terminals_until(start + Duration::from_millis(4), || {
            polls += 1;
            false
        });
        samples.push(start.elapsed());
        app.editor_mut().unwrap().move_down();
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        frames.push(start.elapsed());
        checks.push(polls);
        assert_eq!(app.editor().unwrap().cursor_line, 1);
        black_box(terminal.backend().buffer());
    }
    samples.sort();
    frames.sort();
    eprintln!("build output input handoff: service median={:?} p95={:?}; with editor move/draw median={:?} p95={:?}; interrupt checks={}",
        samples[15], samples[29], frames[15], frames[29], checks[0]);
}
