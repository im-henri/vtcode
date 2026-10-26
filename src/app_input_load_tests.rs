use super::*;
use crate::{app::Tab, editor::Editor};
use crossterm::event::{KeyEvent, MouseEvent, MouseEventKind};
use ratatui::{backend::TestBackend, layout::Rect};

fn fixture() -> App {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::test_with_text(
        &"fn calculate(value) { return value + other_value; }\n".repeat(2_000),
    ))];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.show_explorer = false;
    app.show_minimap = false;
    app.sticky_scroll = false;
    app.cursor_blinking = false;
    for effect in crate::animation_config::AnimationEffect::ALL {
        app.animation_settings.set(effect, false);
    }
    app
}

fn key(code: KeyCode) -> Event { Event::Key(KeyEvent::new(code, KeyModifiers::NONE)) }

fn dispatch(app: &mut App, pump: &mut TerminalEventPump, event: Event) {
    match event {
        Event::Key(key) => assert!(!handle_keyboard_input(app, pump, key)),
        Event::Mouse(mouse) => { handle_mouse_input(app, pump, mouse); }
        _ => panic!("unexpected event"),
    }
}

#[test]
fn changing_navigation_direction_preserves_selection_and_repeat_events() {
    for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT, KeyModifiers::CONTROL | KeyModifiers::SHIFT] {
        let keys: Vec<_> = [KeyCode::Down, KeyCode::Right, KeyCode::End, KeyCode::Up,
            KeyCode::Left, KeyCode::Home, KeyCode::PageDown, KeyCode::PageUp]
            .into_iter().cycle().take(180).enumerate().map(|(i, code)| {
                KeyEvent::new_with_kind(code, modifiers, if i % 2 == 0 { KeyEventKind::Press } else { KeyEventKind::Repeat })
            }).collect();
        let mut actual = fixture();
        let mut expected = fixture();
        actual.macro_recording = true;
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        for &key in &keys {
            sender.send(Ok(Event::Key(key))).unwrap();
            input_keyboard::handle_keyboard_event(&mut expected, key);
        }
        let mut batches = 0;
        while let Some(event) = pump.try_recv().unwrap() {
            dispatch(&mut actual, &mut pump, event);
            batches += 1;
        }
        assert!(batches >= keys.len().div_ceil(MAX_NAVIGATION_BATCH));
        assert!(batches < keys.len());
        assert_eq!(actual.macro_buffer, keys);
        assert_eq!(input_performance_tests::selection_state(&actual), input_performance_tests::selection_state(&expected));
        assert_eq!(actual.editor().unwrap().desired_col, expected.editor().unwrap().desired_col);
    }
}

#[test]
fn navigation_batch_yields_to_overlays_modifiers_release_and_errors() {
    for boundary in [key(KeyCode::Char('x')), Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::SHIFT)),
        Event::Key(KeyEvent::new_with_kind(KeyCode::Right, KeyModifiers::NONE, KeyEventKind::Release)),
        Event::FocusLost, Event::Resize(30, 10)] {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        sender.send(Ok(boundary.clone())).unwrap();
        assert_eq!(pump.next_navigation_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)), None);
        assert_eq!(pump.try_recv().unwrap(), Some(boundary));
    }
    let mut app = fixture();
    let binding = app.keybindings.iter_mut().find(|b| b.id == "toggle_explorer").unwrap();
    binding.id = "quick_open";
    binding.code = KeyCode::Down;
    binding.modifiers = KeyModifiers::NONE;
    let (sender, receiver) = std::sync::mpsc::channel();
    let mut pump = TerminalEventPump::from_receiver(receiver);
    sender.send(Ok(key(KeyCode::Right))).unwrap();
    dispatch(&mut app, &mut pump, key(KeyCode::Down));
    assert!(app.quick_open_active);
    assert_eq!(pump.try_recv().unwrap(), Some(key(KeyCode::Right)));
    sender.send(Err(io::Error::other("input error"))).unwrap();
    assert_eq!(pump.next_navigation_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)), None);
    assert_eq!(pump.try_recv().unwrap_err().to_string(), "input error");
}

#[test]
fn multiline_typing_keeps_normal_autoindent_pairing_undo_and_completion() {
    for multicursor in [false, true] {
        let mut actual = fixture();
        let mut expected = fixture();
        for app in [&mut actual, &mut expected] {
            app.macro_recording = true;
            if multicursor { app.editor_mut().unwrap().extra_cursors = vec![(3, 0), (7, 0)]; }
        }
        let events: Vec<_> = [KeyCode::Char('{'), KeyCode::Enter, KeyCode::Tab,
            KeyCode::Char('界'), KeyCode::Char('e'), KeyCode::Char('\u{301}'), KeyCode::Backspace,
            KeyCode::Char('}'), KeyCode::Enter, KeyCode::Char('v'), KeyCode::Char('a'), KeyCode::Char('l')]
            .into_iter().map(key).collect();
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        let (_empty, receiver) = std::sync::mpsc::channel();
        let mut empty = TerminalEventPump::from_receiver(receiver);
        for event in &events {
            dispatch(&mut expected, &mut empty, event.clone());
            sender.send(Ok(event.clone())).unwrap();
        }
        let mut batches = 0;
        while let Some(event) = pump.try_recv().unwrap() {
            let before = actual.vtcomplete_input_batch.submissions;
            dispatch(&mut actual, &mut pump, event);
            assert!(actual.vtcomplete_input_batch.submissions - before <= 1);
            batches += 1;
        }
        assert!(batches < events.len());
        assert_eq!(actual.macro_buffer, expected.macro_buffer);
        if multicursor {
            // Completion commits target one caret. A hidden suggestion must
            // never intercept typing or Tab in a multicursor editing session.
            assert_eq!(actual.vtcomplete_input_batch.submissions, 0);
            assert!(!actual.vtcomplete_active);
        } else {
            assert!(actual.vtcomplete_input_batch.submissions > 0);
        actual.wait_for_background(Duration::from_secs(3), |app| app.vtcomplete_active);
        assert!(actual.vtcomplete_suggestions.iter().any(|word| word == "value"));
        }
        for _ in 0..15 {
            let (a, b) = (actual.editor().unwrap(), expected.editor().unwrap());
            assert_eq!(a.rope, b.rope);
            assert_eq!(a.extra_cursors, b.extra_cursors);
            assert_eq!(a.extra_cursor_anchors, b.extra_cursor_anchors);
            assert_eq!(input_performance_tests::selection_state(&actual), input_performance_tests::selection_state(&expected));
            actual.editor_mut().unwrap().undo();
            expected.editor_mut().unwrap().undo();
        }
    }
}

#[test]
fn deferred_completion_cannot_revive_after_caret_context_or_explicit_dismissal_changes() {
    for change in 0..5 {
        let mut app = fixture();
        app.editor_mut().unwrap().cursor_col = 5;
        app.begin_vtcomplete_batch();
        app.update_vtcomplete();
        assert_eq!(app.vtcomplete_input_batch.submissions, 0);
        match change {
            0 => app.dismiss_vtcomplete(),
            1 => app.editor_mut().unwrap().move_right(),
            2 => app.editor_mut().unwrap().insert_char(' '),
            3 => { app.tabs[0] = Tab::new_file(Editor::test_with_text("other")); }
            _ => { app.active_group += 1; }
        }
        app.finish_vtcomplete_batch();
        assert_eq!(app.vtcomplete_input_batch.submissions, 0);
        assert!(!app.vtcomplete_active);
    }
}

#[test]
fn moving_wheel_pointer_preserves_every_delta_and_stops_at_editor_boundary() {
    for wrapped in [false, true] {
        let mut actual = fixture();
        let mut expected = fixture();
        let area = Rect::new(10, 5, 80, 20);
        for app in [&mut actual, &mut expected] {
            app.code_area = Some(area);
            let editor = app.editor_mut().unwrap();
            editor.viewport_height = 20;
            editor.viewport_width = 80;
            editor.word_wrap = wrapped;
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = TerminalEventPump::from_receiver(receiver);
        for index in 0..180 {
            let mouse = MouseEvent { kind: MouseEventKind::ScrollDown, column: area.x + index % 30,
                row: area.y + index % 15, modifiers: KeyModifiers::NONE };
            sender.send(Ok(Event::Mouse(mouse))).unwrap();
            mouse::handle_mouse(&mut expected, mouse);
        }
        let boundary = Event::Mouse(MouseEvent { kind: MouseEventKind::ScrollDown,
            column: area.right(), row: area.y, modifiers: KeyModifiers::NONE });
        sender.send(Ok(boundary.clone())).unwrap();
        let mut batches = 0;
        loop {
            let next = pump.try_recv().unwrap().unwrap();
            if next == boundary { break; }
            dispatch(&mut actual, &mut pump, next);
            batches += 1;
        }
        assert!(batches >= 180usize.div_ceil(MAX_SCROLL_BATCH));
        assert!(batches < 180);
        assert_eq!(input_performance_tests::selection_state(&actual), input_performance_tests::selection_state(&expected));
        assert_eq!(actual.editor().unwrap().wrap_scroll_offset, expected.editor().unwrap().wrap_scroll_offset);
        assert_eq!((actual.mouse_col, actual.mouse_row), (expected.mouse_col, expected.mouse_row));
    }
}

#[test]
#[ignore = "manual optimized-profile comparison, also run with constrained CPU affinity and CPU contention"]
fn benchmark_contended_editor_input() {
    use std::time::Instant;
    for case in ["cursor", "selection", "scroll", "typing"] {
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..7 {
            let mut states = Vec::new();
            for reference in [round % 2 == 0, round % 2 != 0] {
                REFERENCE_INPUT_BATCHES.set(reference);
                let mut app = fixture();
                let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
                terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
                let area = app.code_area.unwrap();
                let (sender, receiver) = std::sync::mpsc::channel();
                let mut pump = TerminalEventPump::from_receiver(receiver);
                for index in 0..240 {
                    let event = match case {
                        "scroll" => Event::Mouse(MouseEvent { kind: MouseEventKind::ScrollDown,
                            column: area.x + (index % 25) as u16, row: area.y + 3 + (index % 5) as u16,
                            modifiers: KeyModifiers::NONE }),
                        "typing" => key(match index % 8 { 0 => KeyCode::Enter, 1 => KeyCode::Tab,
                            _ => KeyCode::Char(['v', 'a', 'l', 'u', 'e', 's'][index % 6]) }),
                        _ => Event::Key(KeyEvent::new(if index % 2 == 0 { KeyCode::Down } else { KeyCode::Right },
                            if case == "selection" { KeyModifiers::SHIFT } else { KeyModifiers::NONE })),
                    };
                    sender.send(Ok(event)).unwrap();
                }
                let start = Instant::now();
                let mut frames = 0;
                while let Some(event) = pump.try_recv().unwrap() {
                    dispatch(&mut app, &mut pump, event);
                    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
                    frames += 1;
                }
                let elapsed = start.elapsed();
                samples[usize::from(reference)].push(elapsed);
                states.push((app.editor().unwrap().rope.clone(), input_performance_tests::selection_state(&app),
                    terminal.backend().buffer().clone()));
                if round == 6 { eprintln!("{case} reference={reference}: frames={frames}, completion jobs={}, total={elapsed:?}",
                    app.vtcomplete_input_batch.submissions); }
            }
            assert_eq!(states[0], states[1], "case={case}");
        }
        REFERENCE_INPUT_BATCHES.set(false);
        for sample in &mut samples { sample.sort(); }
        eprintln!("{case}: median old={:?} new={:?} speedup={:.2}x", samples[1][3], samples[0][3],
            samples[1][3].as_secs_f64() / samples[0][3].as_secs_f64());
    }
}
