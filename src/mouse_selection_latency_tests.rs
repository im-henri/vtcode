use super::{TerminalEventPump, handle_mouse_input};
use crate::{
    app::{ActivePane, App, Tab},
    editor::Editor,
};
use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

fn pointer(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

pub(super) fn fixture(unicode: bool) -> (App, Terminal<TestBackend>) {
    let line = if unicode {
        "\tlet α界 = \"e\u{301}🦀\";\t // selection through tabs and Unicode cells; ".repeat(8)
    } else {
        "let sample_value = some_function(first_argument, second_argument); ".repeat(8)
    };
    let mut editor = Editor::test_with_text(&(line + "\n").repeat(2_000));
    editor.word_wrap = false;
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.show_explorer = false;
    app.show_minimap = false;
    app.sticky_scroll = false;
    app.cursor_blinking = false;
    for effect in crate::animation_config::AnimationEffect::ALL {
        app.animation_settings.set(effect, false);
    }
    let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
    for _ in 0..3 {
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
    }
    let area = app.code_area.unwrap();
    crate::mouse::handle_mouse(
        &mut app,
        pointer(
            MouseEventKind::Down(MouseButton::Left),
            area.x + 4,
            area.y + 3,
        ),
    );
    (app, terminal)
}

fn send_drag(pump: &mut TerminalEventPump, app: &mut App) -> bool {
    let Some(Event::Mouse(mouse)) = pump.try_recv().unwrap() else {
        panic!("missing drag");
    };
    handle_mouse_input(app, pump, mouse)
}

fn assert_selection_equal(actual: &App, expected: &App) {
    assert_eq!(
        super::input_performance_tests::selection_state(actual),
        super::input_performance_tests::selection_state(expected)
    );
    assert_eq!(
        actual.editor().unwrap().selected_text(),
        expected.editor().unwrap().selected_text()
    );
    assert_eq!(
        (actual.mouse_col, actual.mouse_row),
        (expected.mouse_col, expected.mouse_row)
    );
}

#[test]
fn horizontal_selection_reaches_the_latest_queued_endpoint_in_one_frame() {
    for unicode in [false, true] {
        for wrapped in [false, true] {
            let (mut actual, mut actual_terminal) = fixture(unicode);
            let (mut expected, mut expected_terminal) = fixture(unicode);
            if wrapped {
                for (app, terminal) in [
                    (&mut actual, &mut actual_terminal),
                    (&mut expected, &mut expected_terminal),
                ] {
                    app.editor_mut().unwrap().word_wrap = true;
                    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
                }
            }
            let area = actual.code_area.unwrap();
            let (sender, receiver) = std::sync::mpsc::channel();
            let mut pump = TerminalEventPump::from_receiver(receiver);
            for index in 0..512 {
                let drag = pointer(
                    MouseEventKind::Drag(MouseButton::Left),
                    area.x + (index * 37 % usize::from(area.width - 2)) as u16,
                    area.y + 3,
                );
                crate::mouse::handle_mouse(&mut expected, drag);
                sender.send(Ok(Event::Mouse(drag))).unwrap();
            }
            assert!(send_drag(&mut pump, &mut actual));
            assert!(
                !pump.has_pending_event(),
                "the first frame must use the current endpoint"
            );
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
                let selection = app.editor().unwrap().selected_text();
                let editor = app.editor_mut().unwrap();
                editor.paste_plain_with_text("replacement λ\nnext row");
                let replaced = editor.rope.clone();
                editor.undo();
                assert_eq!(editor.rope, original);
                assert_eq!(editor.selected_text(), selection);
                editor.redo();
                assert_eq!(editor.rope, replaced);
            }
            assert_selection_equal(&actual, &expected);
        }
    }
}

#[test]
fn repeated_selection_cells_do_not_redraw_and_stale_carets_are_still_corrected() {
    let (mut app, _) = fixture(true);
    let area = app.code_area.unwrap();
    let drag = pointer(
        MouseEventKind::Drag(MouseButton::Left),
        area.x + 20,
        area.y + 3,
    );
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    sender.send(Ok(Event::Mouse(drag))).unwrap();
    assert!(send_drag(&mut pump, &mut app));
    let expected = super::input_performance_tests::selection_state(&app);
    for _ in 0..128 {
        sender.send(Ok(Event::Mouse(drag))).unwrap();
        assert!(!send_drag(&mut pump, &mut app));
        assert_eq!(
            super::input_performance_tests::selection_state(&app),
            expected
        );
    }
    app.lsp_link_hover.modifiers = KeyModifiers::CONTROL;
    sender.send(Ok(Event::Mouse(drag))).unwrap();
    assert!(
        send_drag(&mut pump, &mut app),
        "removing link-hover decoration still needs a frame"
    );
    // A blink, asynchronous navigation or another command can change the
    // current caret without changing the last reported pointer cell.
    app.cursor_visible = false;
    sender.send(Ok(Event::Mouse(drag))).unwrap();
    assert!(send_drag(&mut pump, &mut app));
    assert!(app.cursor_visible);
    app.editor_mut().unwrap().cursor_col = 0;
    sender.send(Ok(Event::Mouse(drag))).unwrap();
    assert!(send_drag(&mut pump, &mut app));
    assert_eq!(
        super::input_performance_tests::selection_state(&app),
        expected
    );
    app.editor_mut().unwrap().scroll_offset = 100;
    sender.send(Ok(Event::Mouse(drag))).unwrap();
    assert!(send_drag(&mut pump, &mut app));
    assert_eq!(app.editor().unwrap().cursor_line, 103);
}

#[test]
fn drag_endpoint_coalescing_preserves_every_input_and_geometry_boundary() {
    use crossterm::event::{KeyCode, KeyEvent};
    let area = ratatui::layout::Rect::new(10, 5, 100, 20);
    let drag = pointer(MouseEventKind::Drag(MouseButton::Left), 30, 8);
    for boundary in [
        Event::Mouse(pointer(MouseEventKind::Up(MouseButton::Left), 50, 8)),
        Event::Mouse(MouseEvent {
            modifiers: KeyModifiers::SHIFT,
            ..drag
        }),
        Event::Mouse(pointer(MouseEventKind::Drag(MouseButton::Right), 50, 8)),
        Event::Mouse(pointer(MouseEventKind::Drag(MouseButton::Left), 50, area.bottom())),
        Event::Mouse(pointer(MouseEventKind::Drag(MouseButton::Left), 9, 8)),
        Event::Mouse(pointer(MouseEventKind::Drag(MouseButton::Left), 110, 8)),
        Event::Mouse(pointer(MouseEventKind::ScrollDown, 50, 8)),
        Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
        Event::Paste("paste".into()),
        Event::Resize(90, 25),
        Event::FocusLost,
    ] {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        sender
            .send(Ok(Event::Mouse(MouseEvent { column: 70, ..drag })))
            .unwrap();
        sender.send(Ok(boundary.clone())).unwrap();
        sender.send(Ok(Event::Mouse(drag))).unwrap();
        assert_eq!(
            pump.latest_selection_drag(drag, area, 1_024),
            (MouseEvent { column: 70, ..drag }, 2)
        );
        assert_eq!(pump.try_recv().unwrap(), Some(boundary));
        assert_eq!(pump.try_recv().unwrap(), Some(Event::Mouse(drag)));
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    sender
        .send(Err(std::io::Error::other("selection input error")))
        .unwrap();
    assert_eq!(pump.latest_selection_drag(drag, area, 1_024), (drag, 1));
    assert_eq!(
        pump.try_recv().unwrap_err().to_string(),
        "selection input error"
    );
}

#[test]
fn horizontal_selection_does_not_cross_startup_replay_boundaries_or_skip_pending_input() {
    use crossterm::event::{KeyCode, KeyEvent};
    let (mut app, _) = fixture(false);
    let area = app.code_area.unwrap();
    let drag = pointer(
        MouseEventKind::Drag(MouseButton::Left),
        area.x + 10,
        area.y + 3,
    );
    let key = Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
    let later = Event::Mouse(MouseEvent {
        column: area.x + 30,
        ..drag
    });
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    sender.send(Ok(later.clone())).unwrap();
    assert!(pump.has_pending_event()); // There is already a terminal lookahead.
    pump.replay_startup_input([Event::Mouse(drag), key.clone()].into());
    assert!(send_drag(&mut pump, &mut app));
    assert_eq!(app.mouse_col, drag.column);
    assert_eq!(pump.try_recv().unwrap(), Some(key));
    assert_eq!(pump.try_recv().unwrap(), Some(later));
}

#[test]
fn horizontal_selection_work_is_bounded_with_a_large_pointer_backlog() {
    let (mut app, _) = fixture(false);
    let area = app.code_area.unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    let samples: Vec<_> = (0..TerminalEventPump::MAX_MOVE_BATCH + 7)
        .map(|index| {
            pointer(
                MouseEventKind::Drag(MouseButton::Left),
                area.x + (index * 37 % usize::from(area.width - 2)) as u16,
                area.y + 3,
            )
        })
        .collect();
    for &mouse in &samples {
        sender.send(Ok(Event::Mouse(mouse))).unwrap();
    }
    assert!(send_drag(&mut pump, &mut app));
    assert_eq!(
        app.mouse_col,
        samples[TerminalEventPump::MAX_MOVE_BATCH - 1].column
    );
    assert!(pump.has_pending_event());
    assert!(send_drag(&mut pump, &mut app));
    assert_eq!(app.mouse_col, samples.last().unwrap().column);
    assert!(!pump.has_pending_event());
}

#[test]
fn panel_captured_drags_never_change_the_editor_or_use_endpoint_coalescing() {
    let (mut app, _) = fixture(false);
    let area = app.code_area.unwrap();
    crate::ui_panels::capture_pointer(&mut app, MouseButton::Left);
    let expected = super::input_performance_tests::selection_state(&app);
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    for index in 0..100 {
        sender
            .send(Ok(Event::Mouse(pointer(
                MouseEventKind::Drag(MouseButton::Left),
                area.x + (index % 30) as u16,
                area.y + 3,
            ))))
            .unwrap();
    }
    assert!(send_drag(&mut pump, &mut app));
    assert!(
        pump.has_pending_event(),
        "the editor must not coalesce another surface's gesture"
    );
    assert_eq!(
        super::input_performance_tests::selection_state(&app),
        expected
    );
}

#[test]
fn word_line_and_column_gestures_keep_their_ordered_selection_samples() {
    for mode in 0..3 {
        let (mut actual, _) = fixture(true);
        let (mut expected, _) = fixture(true);
        for app in [&mut actual, &mut expected] {
            if mode == 2 {
                app.middle_drag_start = Some((3, 4, 4));
            } else {
                app.multi_click_anchor = Some((3, 1, 3, 5));
                app.click_count = 2 + mode;
            }
        }
        let area = actual.code_area.unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        let samples: Vec<_> = (0..128)
            .map(|index| {
                pointer(
                    MouseEventKind::Drag(MouseButton::Left),
                    area.x + (index * 37 % usize::from(area.width - 2)) as u16,
                    area.y + 3,
                )
            })
            .collect();
        for &mouse in &samples {
            sender.send(Ok(Event::Mouse(mouse))).unwrap();
        }
        assert!(send_drag(&mut pump, &mut actual));
        assert!(
            pump.has_pending_event(),
            "stateful selection retains the existing batch limit"
        );
        while pump.has_pending_event() {
            send_drag(&mut pump, &mut actual);
        }
        for mouse in samples {
            crate::mouse::handle_mouse(&mut expected, mouse);
        }
        assert_selection_equal(&actual, &expected);
        assert_eq!(
            actual.editor().unwrap().extra_cursors,
            expected.editor().unwrap().extra_cursors
        );
        assert_eq!(
            actual.editor().unwrap().extra_cursor_anchors,
            expected.editor().unwrap().extra_cursor_anchors
        );
    }
}

#[test]
fn a_modeless_popup_keeps_the_last_unobscured_selection_endpoint() {
    let (mut actual, _) = fixture(false);
    let (mut expected, _) = fixture(false);
    let area = actual.code_area.unwrap();
    let row = area.y + 3;
    for app in [&mut actual, &mut expected] {
        app.publish_remote_connection_notice(true);
        app.side_notice.area = Some(ratatui::layout::Rect::new(area.x + 20, row, 20, 3));
    }
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    for index in 0..100 {
        let drag = pointer(
            MouseEventKind::Drag(MouseButton::Left),
            area.x + 10 + index % 20,
            row,
        );
        crate::mouse::handle_mouse(&mut expected, drag);
        sender.send(Ok(Event::Mouse(drag))).unwrap();
    }
    while pump.has_pending_event() {
        send_drag(&mut pump, &mut actual);
    }
    assert_selection_equal(&actual, &expected);
    assert_eq!(actual.editor().unwrap().cursor_col, 19);
}

#[test]
fn late_selection_refresh_never_crosses_input_boundaries() {
    use crossterm::event::{KeyCode, KeyEvent};
    for case in 0..10 {
        let (mut app, _) = fixture(true);
        let area = app.code_area.unwrap();
        let old = pointer(MouseEventKind::Drag(MouseButton::Left), area.x + 10, area.y + 3);
        let fresh = pointer(MouseEventKind::Drag(MouseButton::Left), area.x + 30, area.y + 5);
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        handle_mouse_input(&mut app, &mut pump, old);
        let before = super::input_performance_tests::selection_state(&app);
        let mut drag = super::editor_selection_drag_state(&app, old).map(|state| (old, state));
        assert!(drag.is_some());
        let boundary = match case {
            0 => Event::Key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL)),
            1 => Event::Mouse(pointer(MouseEventKind::Up(MouseButton::Left), fresh.column, fresh.row)),
            2 => Event::Mouse(pointer(MouseEventKind::Down(MouseButton::Left), fresh.column, fresh.row)),
            3 => Event::Paste("replacement λ\nnext line".into()),
            4 => Event::Resize(180, 60),
            5 => Event::FocusLost,
            6 => Event::Mouse(MouseEvent { modifiers: KeyModifiers::SHIFT, ..fresh }),
            7 => Event::Mouse(pointer(MouseEventKind::Moved, fresh.column, fresh.row)),
            8 => Event::Mouse(pointer(MouseEventKind::Drag(MouseButton::Left), area.x, area.y + area.height)),
            _ => Event::Mouse(pointer(MouseEventKind::Drag(MouseButton::Middle), fresh.column, fresh.row)),
        };
        sender.send(Ok(boundary.clone())).unwrap();
        sender.send(Ok(Event::Mouse(fresh))).unwrap();
        assert!(!super::refresh_selection_before_paint(&mut app, &mut pump, &mut drag));
        assert_eq!(super::input_performance_tests::selection_state(&app), before);
        assert_eq!(pump.try_recv().unwrap(), Some(boundary));
        assert_eq!(pump.try_recv().unwrap(), Some(Event::Mouse(fresh)));
        sender.send(Err(std::io::Error::other("input failed"))).unwrap();
        assert!(!super::refresh_selection_before_paint(&mut app, &mut pump, &mut drag));
        assert_eq!(pump.try_recv().unwrap_err().to_string(), "input failed");
    }
}

#[test]
fn late_selection_refresh_rejects_changed_editor_or_gesture_state() {
    for case in 0..6 {
        let (mut app, _) = fixture(false);
        let area = app.code_area.unwrap();
        let old = pointer(MouseEventKind::Drag(MouseButton::Left), area.x + 10, area.y + 3);
        let fresh = pointer(MouseEventKind::Drag(MouseButton::Left), area.x + 30, area.y + 5);
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        handle_mouse_input(&mut app, &mut pump, old);
        let mut drag = super::editor_selection_drag_state(&app, old).map(|state| (old, state));
        assert!(drag.is_some());
        match case {
            0 => app.active_pane = ActivePane::Explorer,
            1 => app.editor_mut().unwrap().selection_anchor = None,
            2 => app.editor_mut().unwrap().content_version += 1,
            3 => app.editor_mut().unwrap().scroll_offset += 1,
            4 => app.drag_start_pos = None,
            _ => app.hunk_popup_active = true,
        }
        sender.send(Ok(Event::Mouse(fresh))).unwrap();
        assert!(!super::refresh_selection_before_paint(&mut app, &mut pump, &mut drag));
        assert!(drag.is_none());
        assert_eq!(pump.try_recv().unwrap(), Some(Event::Mouse(fresh)));
    }
}

#[test]
#[ignore = "manual optimized benchmark: queued horizontal selection and simulated slow terminal output"]
fn benchmark_horizontal_mouse_selection() {
    use std::time::{Duration, Instant};
    for unicode in [false, true] {
        for output_delay_ms in [0, 2] {
            let mut samples = Vec::new();
            for _ in 0..7 {
                let (mut app, mut terminal) = fixture(unicode);
                let area = app.code_area.unwrap();
                let row = area.y + 3;
                let (sender, receiver) = std::sync::mpsc::channel();
                let mut pump = TerminalEventPump::from_receiver(receiver);
                let mut last = pointer(MouseEventKind::Drag(MouseButton::Left), area.x, row);
                for index in 0..512 {
                    // Reverse direction repeatedly, including crossing the original anchor.
                    last.column = area.x + (index * 37 % usize::from(area.width - 2)) as u16;
                    sender.send(Ok(Event::Mouse(last))).unwrap();
                }
                let mut frames = 0;
                let mut dispatch = Duration::ZERO;
                let mut render = Duration::ZERO;
                let mut first_col = None;
                let started = Instant::now();
                while let Some(Event::Mouse(mouse)) = pump.try_recv().unwrap() {
                    let turn = Instant::now();
                    assert!(handle_mouse_input(&mut app, &mut pump, mouse));
                    dispatch += turn.elapsed();
                    first_col.get_or_insert(app.mouse_col);
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
                let elapsed = started.elapsed();
                let editor = app.editor_mut().unwrap();
                let expected =
                    crate::mouse_drag::code_area_to_buffer_pos(editor, area, last.column, row);
                assert_eq!((editor.cursor_line, editor.cursor_col), expected);
                assert_eq!(app.mouse_col, last.column);
                samples.push((elapsed, dispatch, render, frames, first_col.unwrap()));
            }
            samples.sort_unstable();
            let (elapsed, dispatch, render, frames, first_col) = samples[3];
            eprintln!(
                "horizontal selection unicode={unicode} output_delay_ms={output_delay_ms} events=512 frames={frames} elapsed_us={} dispatch_us={} render_us={} first_frame_column={first_col}",
                elapsed.as_micros(),
                dispatch.as_micros(),
                render.as_micros()
            );
        }
    }
}
