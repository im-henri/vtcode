use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};

use super::{TerminalEventPump, handle_keyboard_input, handle_mouse_input};
use crate::app::{ActivePane, App, Tab};
use crate::editor::Editor;

fn editor_app() -> App {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::test_with_text(
        &"fn sample() { let value = 123; }\n".repeat(2_000),
    ))];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.show_explorer = false;
    app.show_minimap = false;
    app
}

#[test]
fn input_backlog_is_handled_before_ready_background_results() {
    use crate::ui_work::{Scheduler, Turn};
    let mut app = editor_app();
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    for index in 0..100 {
        app.background.sender.send(crate::background::BackgroundResult::GitBranch(index.to_string())).unwrap();
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    sender.send(Ok(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT)))).unwrap();
    let now = std::time::Instant::now();
    assert!(Scheduler::default().next(&mut Turn::new(now), now, pump.has_pending_event()).is_none());
    let Some(Event::Key(key)) = pump.try_recv().unwrap() else { panic!("lost input") };
    assert!(!handle_keyboard_input(&mut app, &mut pump, key));
    assert_eq!(app.editor().unwrap().cursor_line, 1);
    assert!(app.editor().unwrap().selection_anchor.is_some());
    assert!(app.background.has_pending_results());
    for _ in 0..100 { app.poll_background_results(); }
    assert_eq!(app.cached_git_branch, "99");
    assert!(!app.background.has_pending_results());
}

#[test]
#[ignore = "manual release benchmark; synthetic five-service CPU burst with real editor input/rendering"]
fn benchmark_input_latency_with_busy_services() {
    use crate::ui_work::{Scheduler, Service, Turn};
    use std::time::{Duration, Instant};
    const SERVICES: [Service; 5] = [Service::LanguageServers, Service::Workers, Service::Terminals, Service::Agents, Service::Extensions];
    let mut latencies = [Vec::new(), Vec::new()];
    let mut final_states = Vec::new();
    for scheduled in [false, true] {
        let mut app = editor_app();
        let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        for index in 0..100 {
            sender.send(Ok(Event::Key(KeyEvent::new(
                if index % 2 == 0 { KeyCode::Down } else { KeyCode::Right }, KeyModifiers::SHIFT,
            )))).unwrap();
        }
        let mut remaining = [100; 5];
        let mut scheduler = Scheduler::default();
        while pump.has_pending_event() || remaining.iter().any(|count| *count > 0) {
            let input_pending = pump.has_pending_event();
            let started = Instant::now();
            let mut work = |service: Service| {
                let index = SERVICES.iter().position(|candidate| *candidate == service).unwrap();
                if remaining[index] == 0 { return; }
                remaining[index] -= 1;
                let end = Instant::now() + Duration::from_millis(1);
                while Instant::now() < end { std::hint::spin_loop(); }
            };
            if scheduled {
                let mut turn = Turn::new(started);
                while let Some(service) = scheduler.next(&mut turn, Instant::now(), input_pending) {
                    work(service);
                }
            } else {
                for service in SERVICES { work(service); }
            }
            if !input_pending { continue; }
            let Some(Event::Key(key)) = pump.try_recv().unwrap() else { panic!("lost input") };
            latencies[usize::from(scheduled)].push(started.elapsed());
            assert!(!handle_keyboard_input(&mut app, &mut pump, key));
            terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        }
        assert_eq!(remaining, [0; 5], "all background work eventually completes");
        final_states.push((selection_state(&app), terminal.backend().buffer().clone()));
    }
    assert_eq!(final_states[0], final_states[1]);
    for (index, values) in latencies.iter_mut().enumerate() {
        values.sort();
        eprintln!("mixed service burst scheduled={}: dispatch median={:?}, p95={:?}, max={:?}, events={}",
            index == 1, values[values.len() / 2], values[values.len() * 95 / 100], values.last().unwrap(), values.len());
    }
}

fn drag(column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind: MouseEventKind::Drag(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

#[test]
fn terminal_resize_bursts_keep_latest_geometry_and_input_boundaries() {
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    for width in 80..200 {
        sender.send(Ok(Event::Resize(width, 50))).unwrap();
    }
    let key = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    sender.send(Ok(key.clone())).unwrap();
    sender.send(Ok(Event::Resize(240, 70))).unwrap();
    sender
        .send(Err(std::io::Error::other("resize error")))
        .unwrap();
    assert_eq!(pump.try_recv().unwrap(), Some(Event::Resize(199, 50)));
    assert_eq!(pump.try_recv().unwrap(), Some(key));
    assert_eq!(pump.try_recv().unwrap(), Some(Event::Resize(240, 70)));
    assert_eq!(pump.try_recv().unwrap_err().to_string(), "resize error");
}

pub(super) fn pane_resize_app(kind: usize) -> App {
    let mut app = editor_app();
    app.explorer_area = Some(Rect::new(0, 1, 30, 48));
    app.panel_area = Some(Rect::new(0, 30, 160, 19));
    app.diff_split_area = Some(Rect::new(0, 2, 160, 45));
    match kind {
        0 => {
            app.resizing_explorer = true;
            app.show_explorer = true;
        }
        1 => app.resizing_panel = true,
        2 => app.resizing_diff_split = true,
        3 => {
            app.resizing_sc_graph = true;
            app.sidebar_view = crate::app::SidebarView::SourceControl;
        }
        _ => unreachable!(),
    }
    app
}

fn pane_resize_state(app: &App) -> (u16, u16, u32, u16) {
    (
        app.explorer_width,
        app.panel_height,
        app.diff_split_ratio.to_bits(),
        app.sc_graph_height,
    )
}

#[test]
fn pane_resize_bursts_apply_latest_geometry_once_and_preserve_release() {
    for kind in 0..4 {
        let mut reference = pane_resize_app(kind);
        let mut coalesced = pane_resize_app(kind);
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        for index in 0..120 {
            let mouse = drag(15 + (index % 100), 5 + (index % 40));
            crate::mouse::handle_mouse(&mut reference, mouse);
            sender.send(Ok(Event::Mouse(mouse))).unwrap();
        }
        let release = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            ..drag(34, 44)
        });
        sender.send(Ok(release.clone())).unwrap();
        let Some(Event::Mouse(first)) = pump.try_recv().unwrap() else {
            panic!("drag");
        };
        assert!(handle_mouse_input(&mut coalesced, &mut pump, first));
        assert_eq!(
            pane_resize_state(&coalesced),
            pane_resize_state(&reference),
            "pane {kind}"
        );
        assert_eq!(pump.try_recv().unwrap(), Some(release));
        assert!(pump.try_recv().unwrap().is_none());
    }
}

#[test]
#[ignore = "manual release pane resize benchmark"]
fn benchmark_pane_resize_backlog() {
    let mut before = Vec::new();
    let mut after = Vec::new();
    for sample in 0..7 {
        let mut buffers = Vec::new();
        for coalesced in [sample % 2 == 0, sample % 2 != 0] {
            let mut app = pane_resize_app(0);
            app.resizing_explorer = false;
            app.editor_mut().unwrap().word_wrap = true;
            let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
            terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .unwrap();
            app.resizing_explorer = true;
            let (sender, receiver) = std::sync::mpsc::channel();
            let mut pump = TerminalEventPump::from_receiver(receiver);
            for index in 0..120 {
                sender
                    .send(Ok(Event::Mouse(drag(20 + index % 55, 20))))
                    .unwrap();
            }
            let started = std::time::Instant::now();
            let mut frames = 0;
            while let Some(Event::Mouse(mouse)) = pump.try_recv().unwrap() {
                if coalesced {
                    handle_mouse_input(&mut app, &mut pump, mouse);
                } else {
                    crate::mouse::handle_mouse(&mut app, mouse);
                }
                terminal
                    .draw(|frame| crate::ui::draw(frame, &mut app))
                    .unwrap();
                frames += 1;
            }
            let elapsed = started.elapsed();
            if coalesced {
                assert_eq!(frames, 1);
                after.push(elapsed);
            } else {
                assert_eq!(frames, 120);
                before.push(elapsed);
            }
            buffers.push(terminal.backend().buffer().clone());
        }
        assert_eq!(buffers[0], buffers[1]);
    }
    before.sort();
    after.sort();
    eprintln!(
        "120 explorer resize samples with wrapped code: per-sample={:?} coalesced={:?}",
        before[3], after[3]
    );
}

#[test]
fn navigation_backlog_preserves_selection_macro_and_screencast_and_yields() {
    let mut app = editor_app();
    app.macro_recording = true;
    app.screencast_mode = true;
    let key = KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT);
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    for _ in 0..120 {
        sender.send(Ok(Event::Key(key))).unwrap();
    }
    let mut frames = 0;
    while let Some(Event::Key(next)) = pump.try_recv().unwrap() {
        assert!(!handle_keyboard_input(&mut app, &mut pump, next));
        frames += 1;
        assert!(app.editor().unwrap().cursor_line <= frames * super::MAX_NAVIGATION_BATCH);
    }
    assert!(frames >= 2);
    assert_eq!(
        app.editor().unwrap().selection_range(),
        Some((0, 0, 120, 0))
    );
    assert_eq!(app.macro_buffer, vec![key; 120]);
    assert_eq!(app.screencast_keys.len(), 5);
    assert!(app.cursor_visible);
}

#[test]
fn reported_key_repeats_move_and_extend_selection() {
    let mut app = editor_app();
    let key = KeyEvent::new_with_kind(KeyCode::Down, KeyModifiers::SHIFT, KeyEventKind::Repeat);
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    for _ in 0..10 {
        sender.send(Ok(Event::Key(key))).unwrap();
    }
    while let Some(Event::Key(key)) = pump.try_recv().unwrap() {
        assert!(!handle_keyboard_input(&mut app, &mut pump, key));
    }
    assert_eq!(app.editor().unwrap().selection_range(), Some((0, 0, 10, 0)));
}

#[test]
fn navigation_batch_keeps_all_event_and_error_boundaries_in_order() {
    let key = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
    for boundary in [
        Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
        Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT)),
        Event::Key(KeyEvent::new_with_kind(
            KeyCode::Right,
            KeyModifiers::NONE,
            KeyEventKind::Release,
        )),
        Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
        Event::Mouse(drag(20, 10)),
        Event::Paste("pasted".into()),
        Event::Resize(100, 30),
        Event::FocusLost,
    ] {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        sender.send(Ok(Event::Key(key))).unwrap();
        sender.send(Ok(boundary.clone())).unwrap();
        sender.send(Ok(Event::Key(key))).unwrap();
        assert!(pump.next_matching_key(key));
        assert!(!pump.next_matching_key(key));
        assert_eq!(pump.try_recv().unwrap(), Some(boundary));
        assert_eq!(pump.try_recv().unwrap(), Some(Event::Key(key)));
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    sender
        .send(Err(std::io::Error::other("input error")))
        .unwrap();
    assert!(!pump.next_matching_key(key));
    assert_eq!(pump.try_recv().unwrap_err().to_string(), "input error");
    drop(sender);
    assert!(!pump.next_matching_key(key));
    assert_eq!(
        pump.try_recv().unwrap_err().kind(),
        std::io::ErrorKind::UnexpectedEof
    );
}

#[test]
fn navigation_batch_honors_custom_bindings_and_editing_yields_to_replay() {
    let mut app = editor_app();
    let binding = app
        .keybindings
        .iter_mut()
        .find(|b| b.id == "toggle_explorer")
        .unwrap();
    binding.code = KeyCode::Right;
    binding.modifiers = KeyModifiers::NONE;
    let key = KeyEvent::new(KeyCode::Right, KeyModifiers::NONE);
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    for _ in 0..3 {
        sender.send(Ok(Event::Key(key))).unwrap();
    }
    let Some(Event::Key(next)) = pump.try_recv().unwrap() else {
        panic!("queued key");
    };
    assert!(!handle_keyboard_input(&mut app, &mut pump, next));
    assert!(app.show_explorer);
    assert_eq!(app.editor().unwrap().cursor_col, 0);
    assert_eq!(pump.try_recv().unwrap(), None);

    let typed = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
    sender.send(Ok(Event::Key(typed))).unwrap();
    assert!(!handle_keyboard_input(&mut app, &mut pump, typed));
    assert_eq!(pump.try_recv().unwrap(), None);

    app.macro_replay_queue.push_back(typed);
    sender.send(Ok(Event::Key(key))).unwrap();
    assert!(!handle_keyboard_input(&mut app, &mut pump, key));
    assert_eq!(pump.try_recv().unwrap(), Some(Event::Key(key)));
    assert_eq!(app.macro_replay_queue.pop_front(), Some(typed));
}

#[test]
fn queued_mixed_editor_edits_preserve_text_carets_undo_and_every_macro_key() {
    let mut reference = editor_app();
    let mut batched = editor_app();
    for app in [&mut reference, &mut batched] {
        app.macro_recording = true;
        app.screencast_mode = true;
        let editor = app.editor_mut().unwrap();
        editor.selection_anchor = Some((0, 0));
        editor.cursor_col = 2;
        editor.extra_cursors = vec![(2, 2), (4, 2)];
    }
    let keys: Vec<_> = (0..180)
        .map(|index| {
            KeyEvent::new(
                match index % 9 {
                    0 => KeyCode::Char('('),
                    1 => KeyCode::Char('界'),
                    2 => KeyCode::Char('e'),
                    3 => KeyCode::Char('\u{301}'),
                    4 => KeyCode::Char(' '),
                    5 => KeyCode::Char('x'),
                    6 => KeyCode::Backspace,
                    7 => KeyCode::Delete,
                    _ => KeyCode::Char(')'),
                },
                KeyModifiers::NONE,
            )
        })
        .collect();
    let (_empty_sender, empty_receiver) = std::sync::mpsc::channel();
    let mut empty = TerminalEventPump::from_receiver(empty_receiver);
    for &key in &keys {
        assert!(!handle_keyboard_input(&mut reference, &mut empty, key));
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    for &key in &keys {
        sender.send(Ok(Event::Key(key))).unwrap();
    }
    let mut frames = 0;
    while let Some(Event::Key(key)) = pump.try_recv().unwrap() {
        assert!(!handle_keyboard_input(&mut batched, &mut pump, key));
        frames += 1;
    }
    assert!(frames >= keys.len().div_ceil(super::MAX_NAVIGATION_BATCH));
    assert!(frames < keys.len(), "queued editing should share frames");
    assert_eq!(batched.macro_buffer, keys);
    assert_eq!(batched.macro_buffer, reference.macro_buffer);
    assert_eq!(
        batched
            .screencast_keys
            .iter()
            .map(|(key, _)| *key)
            .collect::<Vec<_>>(),
        keys[keys.len() - 5..]
    );
    for _ in 0..12 {
        let a = reference.editor().unwrap();
        let b = batched.editor().unwrap();
        assert_eq!(a.rope, b.rope);
        assert_eq!(selection_state(&reference), selection_state(&batched));
        assert_eq!(a.extra_cursors, b.extra_cursors);
        assert_eq!(a.extra_cursor_anchors, b.extra_cursor_anchors);
        reference.editor_mut().unwrap().undo();
        batched.editor_mut().unwrap().undo();
    }
}

#[test]
fn editing_batch_keeps_nonediting_events_modifiers_errors_and_context_changes() {
    let typed = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
    for boundary in [
        Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
        Event::Key(KeyEvent::new(KeyCode::Char('B'), KeyModifiers::SHIFT)),
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        )),
        Event::Paste("pasted".into()),
        Event::FocusLost,
        Event::Resize(100, 30),
        Event::Mouse(drag(20, 10)),
    ] {
        let mut app = editor_app();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        sender
            .send(Ok(Event::Key(KeyEvent::new(
                KeyCode::Char('b'),
                KeyModifiers::NONE,
            ))))
            .unwrap();
        sender.send(Ok(boundary.clone())).unwrap();
        sender.send(Ok(Event::Key(typed))).unwrap();
        assert!(!handle_keyboard_input(&mut app, &mut pump, typed));
        assert_eq!(pump.try_recv().unwrap(), Some(boundary));
        assert_eq!(pump.try_recv().unwrap(), Some(Event::Key(typed)));
        assert!(app.editor().unwrap().rope.to_string().starts_with("ab"));
    }
    let mut app = editor_app();
    let binding = app
        .keybindings
        .iter_mut()
        .find(|binding| binding.id == "toggle_explorer")
        .unwrap();
    binding.id = "quick_open";
    binding.code = KeyCode::Char('a');
    binding.modifiers = KeyModifiers::NONE;
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    sender.send(Ok(Event::Key(typed))).unwrap();
    assert!(!handle_keyboard_input(&mut app, &mut pump, typed));
    assert!(app.quick_open_active);
    assert_eq!(pump.try_recv().unwrap(), Some(Event::Key(typed)));

    app.quick_open_active = false;
    sender
        .send(Err(std::io::Error::other("typing input failed")))
        .unwrap();
    assert_eq!(pump.next_editing_key(typed), None);
    assert_eq!(
        pump.try_recv().unwrap_err().to_string(),
        "typing input failed"
    );
}

#[test]
#[ignore = "manual release queued typing and deletion workbench benchmark"]
fn benchmark_typing_and_deletion_backlogs() {
    for deleting in [false, true] {
        let mut ordinary = Vec::new();
        let mut batched_samples = Vec::new();
        for sample in 0..7 {
            let mut outputs = Vec::new();
            for batched in [sample % 2 == 0, sample % 2 != 0] {
                let mut app = editor_app();
                if deleting {
                    let editor = app.editor_mut().unwrap();
                    editor.rope.insert(0, &"x ".repeat(120));
                    editor.cursor_col = 240;
                }
                let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
                terminal
                    .draw(|frame| crate::ui::draw(frame, &mut app))
                    .unwrap();
                let (sender, receiver) = std::sync::mpsc::channel();
                let mut pump = TerminalEventPump::from_receiver(receiver);
                for ch in "let value = (界 + x); ".chars().cycle().take(240) {
                    let code = if deleting {
                        KeyCode::Backspace
                    } else {
                        KeyCode::Char(ch)
                    };
                    sender
                        .send(Ok(Event::Key(KeyEvent::new(code, KeyModifiers::NONE))))
                        .unwrap();
                }
                let started = std::time::Instant::now();
                let mut frames = 0;
                let mut next = pump.try_recv().unwrap();
                let final_buffer = loop {
                    let Some(Event::Key(key)) = next else {
                        panic!("expected editing backlog");
                    };
                    if batched {
                        handle_keyboard_input(&mut app, &mut pump, key);
                    } else {
                        crate::input_keyboard::handle_keyboard_event(&mut app, key);
                    }
                    next = pump.try_recv().unwrap();
                    let frame = terminal
                        .draw(|frame| crate::ui::draw(frame, &mut app))
                        .unwrap();
                    frames += 1;
                    // TestBackend retains historical symbols in the hidden
                    // continuation cells of wide glyphs. Compare the complete
                    // rendered scene rather than those nondisplayed cells.
                    if next.is_none() {
                        break frame.buffer.clone();
                    }
                };
                let elapsed = started.elapsed();
                outputs.push((
                    app.editor().unwrap().rope.clone(),
                    selection_state(&app),
                    final_buffer,
                ));
                if batched {
                    batched_samples.push(elapsed);
                } else {
                    ordinary.push(elapsed);
                }
                eprintln!(
                    "editing backlog deleting={deleting} batched={batched} frames={frames} elapsed={elapsed:?}"
                );
            }
            assert_eq!(outputs[0], outputs[1]);
        }
        ordinary.sort_unstable();
        batched_samples.sort_unstable();
        eprintln!(
            "editing backlog deleting={deleting}: ordinary {:?}, batched {:?}, {:.2}x",
            ordinary[3],
            batched_samples[3],
            ordinary[3].as_secs_f64() / batched_samples[3].as_secs_f64()
        );
    }
}

fn selection_app() -> App {
    let mut app = editor_app();
    let area = Rect::new(10, 5, 40, 10);
    app.code_area = Some(area);
    let editor = app.editor_mut().unwrap();
    editor.viewport_height = area.height as usize;
    editor.viewport_width = area.width as usize;
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            ..drag(12, 7)
        },
    );
    app
}

pub(super) fn selection_state(app: &App) -> (usize, usize, usize, usize, Option<(usize, usize)>) {
    let ed = app.editor().unwrap();
    (
        ed.cursor_line,
        ed.cursor_col,
        ed.scroll_offset,
        ed.horizontal_scroll,
        ed.selection_anchor,
    )
}

#[test]
fn selection_backlog_preserves_every_auto_scroll_step_and_gesture_boundary() {
    let mut reference = selection_app();
    let mut batched = selection_app();
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    for i in 0..120 {
        // Include direction reversals and samples above/below the viewport.
        let row = match i % 4 {
            0 => 4,
            1 => 17,
            _ => 10,
        };
        let mouse = drag(12 + (i % 20) as u16, row);
        crate::mouse::handle_mouse(&mut reference, mouse);
        sender.send(Ok(Event::Mouse(mouse))).unwrap();
    }
    let release = MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        ..drag(15, 10)
    };
    sender.send(Ok(Event::Mouse(release))).unwrap();
    let mut frames = 0;
    while let Some(Event::Mouse(mouse)) = pump.try_recv().unwrap() {
        if mouse.kind == release.kind {
            break;
        }
        handle_mouse_input(&mut batched, &mut pump, mouse);
        frames += 1;
    }
    assert!(frames >= 2);
    assert_eq!(selection_state(&batched), selection_state(&reference));
    assert_eq!(
        batched.editor().unwrap().selected_text(),
        reference.editor().unwrap().selected_text()
    );
}

#[test]
fn selection_batch_keeps_modifiers_buttons_input_and_errors_in_order() {
    let mouse = drag(20, 10);
    for boundary in [
        Event::Mouse(MouseEvent {
            modifiers: KeyModifiers::SHIFT,
            ..mouse
        }),
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Middle),
            ..mouse
        }),
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            ..mouse
        }),
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            ..mouse
        }),
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        Event::Resize(100, 30),
        Event::FocusLost,
    ] {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        sender.send(Ok(boundary.clone())).unwrap();
        assert_eq!(pump.next_matching_drag(mouse), None);
        assert_eq!(pump.try_recv().unwrap(), Some(boundary));
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    sender
        .send(Err(std::io::Error::other("drag error")))
        .unwrap();
    assert_eq!(pump.next_matching_drag(mouse), None);
    assert_eq!(pump.try_recv().unwrap_err().to_string(), "drag error");
}

#[test]
#[ignore = "manual release benchmark"]
fn benchmark_navigation_and_selection_backlogs() {
    for selection_drag in [false, true] {
        let mut before = Vec::new();
        let mut after = Vec::new();
        for sample in 0..7 {
            let mut outputs = Vec::new();
            let mut states = Vec::new();
            for batched in [sample % 2 == 0, sample % 2 != 0] {
                let mut app = editor_app();
                let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
                terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                let area = app.code_area.unwrap();
                if selection_drag {
                    crate::mouse::handle_mouse(
                        &mut app,
                        MouseEvent {
                            kind: MouseEventKind::Down(MouseButton::Left),
                            ..drag(area.x + 2, area.y + 2)
                        },
                    );
                }
                let (sender, receiver) = std::sync::mpsc::channel();
                let mut pump = TerminalEventPump::from_receiver(receiver);
                for i in 0..120 {
                    let event = if selection_drag {
                        Event::Mouse(drag(area.x + 2 + (i % 20), area.y + 2 + (i % 10)))
                    } else {
                        Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT))
                    };
                    sender.send(Ok(event)).unwrap();
                }
                let mut frames = 0;
                let started = std::time::Instant::now();
                while let Some(event) = pump.try_recv().unwrap() {
                    match event {
                        Event::Mouse(mouse) if batched => {
                            handle_mouse_input(&mut app, &mut pump, mouse);
                        }
                        Event::Mouse(mouse) => crate::mouse::handle_mouse(&mut app, mouse),
                        Event::Key(key) if batched => {
                            handle_keyboard_input(&mut app, &mut pump, key);
                        }
                        Event::Key(key) => {
                            crate::input_keyboard::handle_keyboard_event(&mut app, key);
                        }
                        _ => unreachable!(),
                    }
                    terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                    frames += 1;
                }
                let elapsed = started.elapsed();
                outputs.push(terminal.backend().buffer().clone());
                states.push(selection_state(&app));
                if batched {
                    after.push(elapsed);
                } else {
                    before.push(elapsed);
                }
                eprintln!(
                    "input backlog drag={selection_drag} batched={batched} frames={frames} elapsed={elapsed:?}"
                );
            }
            assert_eq!(states[0], states[1]);
            assert_eq!(outputs[0], outputs[1]);
        }
        before.sort();
        after.sort();
        eprintln!(
            "input backlog drag={selection_drag} median before={:?} after={:?}",
            before[3], after[3]
        );
    }
}
