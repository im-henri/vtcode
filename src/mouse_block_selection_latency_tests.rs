use super::{TerminalEventPump, handle_mouse_input};
use crate::app::App;
use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};

fn fixture(unicode: bool, wrapped: bool, alt_shift: bool) -> (App, Terminal<TestBackend>) {
    let (mut app, mut terminal) = super::mouse_selection_latency_tests::fixture(unicode);
    app.editor_mut().unwrap().word_wrap = wrapped;
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut app))
        .unwrap();
    let area = app.code_area.unwrap();
    app.last_click_time = std::time::Instant::now() - std::time::Duration::from_secs(1);
    let (button, modifiers) = gesture(alt_shift);
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(button),
            column: area.x + 12,
            row: area.y + 10,
            modifiers,
        },
    );
    assert!(app.middle_drag_start.is_some());
    (app, terminal)
}

fn gesture(alt_shift: bool) -> (MouseButton, KeyModifiers) {
    if alt_shift {
        (MouseButton::Left, KeyModifiers::ALT | KeyModifiers::SHIFT)
    } else {
        (MouseButton::Middle, KeyModifiers::NONE)
    }
}

fn sample(area: Rect, index: usize, alt_shift: bool) -> MouseEvent {
    let (button, modifiers) = gesture(alt_shift);
    MouseEvent {
        kind: MouseEventKind::Drag(button),
        // Deterministic direction reversals across both sides of the anchor.
        column: area.x + (index * 37 % usize::from(area.width - 2)) as u16,
        row: area.y + (index * 17 % usize::from(area.height - 2)) as u16,
        modifiers,
    }
}

fn assert_selection_equal(actual: &App, expected: &App) {
    let actual_editor = actual.editor().unwrap();
    let expected_editor = expected.editor().unwrap();
    assert_eq!(
        super::input_performance_tests::selection_state(actual),
        super::input_performance_tests::selection_state(expected)
    );
    assert_eq!(
        actual_editor.wrap_scroll_offset,
        expected_editor.wrap_scroll_offset
    );
    assert_eq!(actual_editor.extra_cursors, expected_editor.extra_cursors);
    assert_eq!(
        actual_editor.extra_cursor_anchors,
        expected_editor.extra_cursor_anchors
    );
    assert_eq!(
        (actual.mouse_col, actual.mouse_row),
        (expected.mouse_col, expected.mouse_row)
    );
}

fn dispatch(pump: &mut TerminalEventPump, app: &mut App) -> bool {
    let Some(Event::Mouse(mouse)) = pump.try_recv().unwrap() else {
        panic!("drag");
    };
    handle_mouse_input(app, pump, mouse)
}

#[test]
fn rectangular_selection_batches_redraws_without_changing_cursors_or_navigation() {
    for unicode in [false, true] {
        for wrapped in [false, true] {
            for alt_shift in [false, true] {
                let (mut actual, mut actual_terminal) = fixture(unicode, wrapped, alt_shift);
                let (mut expected, mut expected_terminal) = fixture(unicode, wrapped, alt_shift);
                let area = actual.code_area.unwrap();
                let (sender, receiver) = std::sync::mpsc::channel();
                let mut pump = TerminalEventPump::from_receiver(receiver);
                for index in 0..512 {
                    let mouse = sample(area, index, alt_shift);
                    crate::mouse::handle_mouse(&mut expected, mouse);
                    sender.send(Ok(Event::Mouse(mouse))).unwrap();
                }
                let mut frames = 0;
                while pump.has_pending_event() {
                    assert!(dispatch(&mut pump, &mut actual));
                    frames += 1;
                }
                assert!(frames < 512, "queued middle drags must share redraws");
                assert_selection_equal(&actual, &expected);
                actual_terminal
                    .draw(|frame| crate::ui::draw(frame, &mut actual))
                    .unwrap();
                expected_terminal
                    .draw(|frame| crate::ui::draw(frame, &mut expected))
                    .unwrap();
                assert_eq!(
                    actual_terminal.backend().buffer(),
                    expected_terminal.backend().buffer()
                );
                for app in [&mut actual, &mut expected] {
                    let original = app.editor().unwrap().rope.clone();
                    let editor = app.editor_mut().unwrap();
                    editor.multi_cursor_insert_char('λ');
                    let replaced = editor.rope.clone();
                    editor.undo();
                    assert_eq!(editor.rope, original);
                    editor.redo();
                    assert_eq!(editor.rope, replaced);
                }
                assert_selection_equal(&actual, &expected);
                for app in [&mut actual, &mut expected] {
                    let editor = app.editor_mut().unwrap();
                    editor.multi_cursor_paste("first λ\nnext 界");
                    let pasted = editor.rope.clone();
                    editor.undo();
                    editor.redo();
                    assert_eq!(editor.rope, pasted);
                }
                assert_selection_equal(&actual, &expected);
                assert_eq!(
                    actual.editor().unwrap().rope,
                    expected.editor().unwrap().rope
                );
            }
        }
    }
}

#[test]
fn rectangular_batches_preserve_input_edges_modifiers_errors_and_startup_order() {
    use crossterm::event::{KeyCode, KeyEvent};
    let area = Rect::new(10, 5, 100, 20);
    for alt_shift in [false, true] {
        let drag = sample(area, 1, alt_shift);
        let later = sample(area, 3, alt_shift);
        let (button, _) = gesture(alt_shift);
        let other_button = if alt_shift {
            MouseButton::Middle
        } else {
            MouseButton::Left
        };
        for boundary in [
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Up(button),
                ..drag
            }),
            Event::Mouse(MouseEvent {
                modifiers: KeyModifiers::CONTROL,
                ..drag
            }),
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Drag(other_button),
                ..drag
            }),
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                ..drag
            }),
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                ..drag
            }),
            Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            Event::Paste("paste λ".into()),
            Event::Resize(90, 25),
            Event::FocusLost,
        ] {
            let (sender, receiver) = std::sync::mpsc::channel();
            let mut pump = TerminalEventPump::from_receiver(receiver);
            sender.send(Ok(Event::Mouse(later))).unwrap();
            sender.send(Ok(boundary.clone())).unwrap();
            sender.send(Ok(Event::Mouse(drag))).unwrap();
            assert_eq!(pump.next_matching_drag(drag), Some(later));
            assert_eq!(pump.next_matching_drag(later), None);
            assert_eq!(pump.try_recv().unwrap(), Some(boundary));
            assert_eq!(pump.try_recv().unwrap(), Some(Event::Mouse(drag)));
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        sender
            .send(Err(std::io::Error::other("block input error")))
            .unwrap();
        assert_eq!(pump.next_matching_drag(drag), None);
        assert_eq!(
            pump.try_recv().unwrap_err().to_string(),
            "block input error"
        );

        let (mut app, _) = fixture(true, false, alt_shift);
        let area = app.code_area.unwrap();
        let drag = sample(area, 1, alt_shift);
        let later = sample(area, 3, alt_shift);
        let key = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        sender.send(Ok(Event::Mouse(later))).unwrap();
        assert!(pump.has_pending_event());
        pump.replay_startup_input([Event::Mouse(drag), key.clone()].into());
        assert!(dispatch(&mut pump, &mut app));
        assert_eq!((app.mouse_col, app.mouse_row), (drag.column, drag.row));
        assert_eq!(pump.try_recv().unwrap(), Some(key));
        assert_eq!(pump.try_recv().unwrap(), Some(Event::Mouse(later)));
    }
}

#[test]
fn rectangular_selection_work_is_bounded_and_keeps_the_release() {
    for alt_shift in [false, true] {
        let (mut app, _) = fixture(false, false, alt_shift);
        let area = app.code_area.unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        let count = super::MAX_SCROLL_BATCH;
        let total = count * 3 + 17;
        for index in 0..total {
            sender
                .send(Ok(Event::Mouse(sample(area, index, alt_shift))))
                .unwrap();
        }
        let last = sample(area, total - 1, alt_shift);
        let release = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Up(gesture(alt_shift).0),
            ..last
        });
        sender.send(Ok(release.clone())).unwrap();
        assert!(dispatch(&mut pump, &mut app));
        assert!(
            (0..count).any(|index| {
                let endpoint = sample(area, index, alt_shift);
                (app.mouse_col, app.mouse_row) == (endpoint.column, endpoint.row)
            }),
            "one input turn must not exceed its dispatch limit"
        );
        assert!(
            pump.has_pending_event(),
            "a continuous stream must yield to drawing and background work"
        );
        let mut released = false;
        while let Some(Event::Mouse(mouse)) = pump.try_recv().unwrap() {
            if Event::Mouse(mouse) == release {
                assert!(
                    app.middle_drag_start.is_some(),
                    "release must not be swallowed by batching"
                );
                handle_mouse_input(&mut app, &mut pump, mouse);
                released = true;
                break;
            }
            handle_mouse_input(&mut app, &mut pump, mouse);
        }
        assert!(released);
        assert!(app.middle_drag_start.is_none());
        assert!(app.block_drag_cache.is_none());
        assert_eq!((app.mouse_col, app.mouse_row), (last.column, last.row));
        assert!(!pump.has_pending_event());
    }
}

#[test]
fn rectangular_selection_keeps_every_outside_autoscroll_step() {
    for wrapped in [false, true] {
        for alt_shift in [false, true] {
            let (mut actual, _) = fixture(true, wrapped, alt_shift);
            let (mut expected, _) = fixture(true, wrapped, alt_shift);
            let area = actual.code_area.unwrap();
            let (sender, receiver) = std::sync::mpsc::channel();
            let mut pump = TerminalEventPump::from_receiver(receiver);
            for index in 0..128 {
                let mut mouse = sample(area, index, alt_shift);
                if index % 7 == 0 {
                    mouse.row = area.bottom() + 2;
                } else if index % 13 == 0 {
                    mouse.row = area.y - 1;
                }
                crate::mouse::handle_mouse(&mut expected, mouse);
                sender.send(Ok(Event::Mouse(mouse))).unwrap();
            }
            while pump.has_pending_event() {
                dispatch(&mut pump, &mut actual);
            }
            assert_selection_equal(&actual, &expected);
            let editor = actual.editor().unwrap();
            assert!(if wrapped {
                editor.wrap_scroll_offset > 0
            } else {
                editor.scroll_offset > 0
            });
        }
    }
}

#[test]
fn rectangular_selection_preserves_empty_rows_folds_and_horizontal_pan() {
    let text = "\n\t界αβ abcdefghijklmnopqrstuvwxyz\n\nx\nabcdef\n".repeat(400);
    for wrapped in [false, true] {
        let (mut actual, mut actual_terminal) = fixture(true, wrapped, false);
        let (mut expected, mut expected_terminal) = fixture(true, wrapped, false);
        for (app, terminal) in [
            (&mut actual, &mut actual_terminal),
            (&mut expected, &mut expected_terminal),
        ] {
            let mut editor = crate::editor::Editor::test_with_text(&text);
            editor.word_wrap = wrapped;
            editor.folded_ranges.extend([(2, 7), (15, 22)]);
            if !wrapped {
                editor.horizontal_scroll = 3;
            }
            *app.editor_mut().unwrap() = editor;
            terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
            let area = app.code_area.unwrap();
            crate::mouse::handle_mouse(
                app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Middle),
                    column: area.x + 2,
                    row: area.y + 2,
                    modifiers: KeyModifiers::NONE,
                },
            );
        }
        let area = actual.code_area.unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        // Render after each burst to validate both cached and cold projections.
        for last in [0, 3, 10, 100, 511] {
            for index in 0..=last {
                let mouse = sample(area, index, false);
                crate::mouse::handle_mouse(&mut expected, mouse);
                sender.send(Ok(Event::Mouse(mouse))).unwrap();
            }
            while pump.has_pending_event() {
                dispatch(&mut pump, &mut actual);
            }
            assert_selection_equal(&actual, &expected);
            actual_terminal
                .draw(|frame| crate::ui::draw(frame, &mut actual))
                .unwrap();
            expected_terminal
                .draw(|frame| crate::ui::draw(frame, &mut expected))
                .unwrap();
            assert_eq!(
                actual_terminal.backend().buffer(),
                expected_terminal.backend().buffer()
            );
        }
    }
}

#[test]
fn rectangular_drags_do_not_coalesce_through_popups_or_panel_capture() {
    for alt_shift in [false, true] {
        for captured in [false, true] {
            let (mut actual, _) = fixture(true, false, alt_shift);
            let (mut expected, _) = fixture(true, false, alt_shift);
            let area = actual.code_area.unwrap();
            for app in [&mut actual, &mut expected] {
                if captured {
                    crate::ui_panels::capture_pointer(app, gesture(alt_shift).0);
                } else {
                    app.publish_remote_connection_notice(true);
                    app.side_notice.area = Some(Rect::new(area.x + 20, area.y + 5, 30, 12));
                }
            }
            let (sender, receiver) = std::sync::mpsc::channel();
            let mut pump = TerminalEventPump::from_receiver(receiver);
            for index in 0..128 {
                let mouse = sample(area, index, alt_shift);
                crate::mouse::handle_mouse(&mut expected, mouse);
                sender.send(Ok(Event::Mouse(mouse))).unwrap();
            }
            dispatch(&mut pump, &mut actual);
            assert!(
                pump.has_pending_event(),
                "another surface must retain its input stream"
            );
            while pump.has_pending_event() {
                dispatch(&mut pump, &mut actual);
            }
            assert_selection_equal(&actual, &expected);
        }
    }
}

#[test]
#[ignore = "manual optimized benchmark: rectangular selection with queued two-dimensional pointer movement"]
fn benchmark_rectangular_mouse_selection() {
    use std::time::{Duration, Instant};
    for unicode in [false, true] {
        for output_delay_ms in [0, 2] {
            let mut samples = Vec::new();
            for _ in 0..7 {
                let (mut app, mut terminal) = fixture(unicode, false, false);
                let (mut expected, _) = fixture(unicode, false, false);
                let area = app.code_area.unwrap();
                let (sender, receiver) = std::sync::mpsc::channel();
                let mut pump = TerminalEventPump::from_receiver(receiver);
                for index in 0..512 {
                    let mouse = sample(area, index, false);
                    crate::mouse::handle_mouse(&mut expected, mouse);
                    sender.send(Ok(Event::Mouse(mouse))).unwrap();
                }
                let mut frames = 0;
                let mut dispatch = Duration::ZERO;
                let mut render = Duration::ZERO;
                let mut first_endpoint = None;
                let started = Instant::now();
                while let Some(Event::Mouse(mouse)) = pump.try_recv().unwrap() {
                    let turn = Instant::now();
                    let redraw = handle_mouse_input(&mut app, &mut pump, mouse);
                    dispatch += turn.elapsed();
                    first_endpoint.get_or_insert((app.mouse_col, app.mouse_row));
                    if redraw {
                        let drawing = Instant::now();
                        terminal
                            .draw(|frame| crate::ui::draw(frame, &mut app))
                            .unwrap();
                        render += drawing.elapsed();
                        if output_delay_ms != 0 {
                            std::thread::sleep(Duration::from_millis(output_delay_ms));
                        }
                        frames += 1;
                    }
                }
                let elapsed = started.elapsed();
                assert_selection_equal(&app, &expected);
                samples.push((elapsed, dispatch, render, frames, first_endpoint.unwrap()));
            }
            samples.sort_unstable();
            let (elapsed, dispatch, render, frames, first_endpoint) = samples[3];
            eprintln!(
                "rectangular selection unicode={unicode} output_delay_ms={output_delay_ms} events=512 frames={frames} elapsed_us={} dispatch_us={} render_us={} first_frame_endpoint={first_endpoint:?}",
                elapsed.as_micros(),
                dispatch.as_micros(),
                render.as_micros()
            );
        }
    }
}
