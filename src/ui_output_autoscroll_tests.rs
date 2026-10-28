use super::*;

fn offset(app: &App) -> usize {
    let reader = app.logs.reader.as_ref().unwrap();
    if reader.wrap {
        reader.editor.wrap_scroll_offset
    } else {
        reader.editor.scroll_offset
    }
}

fn wheel(app: &mut App, kind: MouseEventKind) {
    let text = app.logs.reader.as_ref().unwrap().text;
    pointer(app, kind, text.x + 2, text.y, KeyModifiers::NONE);
}

#[test]
fn downward_overscroll_resumes_each_output_channel_with_and_without_wrapping() {
    for source in Source::ALL {
        for wrap in [false, true] {
            let ssh = Log::default();
            let mut app = app(ssh.clone());
            let log = if source == Source::Git {
                app.git_output()
            } else {
                ssh
            };
            app.logs.select_source(source);
            app.logs.reader().wrap = wrap;
            app.scroll_speed = 3;
            for index in 0..40 {
                log.event("Test", &format!("row {index} {}", "text ".repeat(40)));
            }
            draw(&mut app, 100, 30);
            let bottom = offset(&app);
            wheel(&mut app, MouseEventKind::ScrollUp);
            assert_eq!(offset(&app), bottom - 3);
            assert!(!app.logs.reader().view.autoscroll);
            wheel(&mut app, MouseEventKind::ScrollDown);
            assert_eq!(offset(&app), bottom);
            assert!(
                !app.logs.reader().view.autoscroll,
                "reaching the end alone stays paused"
            );
            // Burst wheel events must stay at the bottom even before another frame.
            for _ in 0..2 {
                wheel(&mut app, MouseEventKind::ScrollDown);
                assert!(app.logs.reader().view.autoscroll);
                assert_eq!(offset(&app), bottom);
            }
            log.event("Test", "latest output after resuming");
            assert!(draw(&mut app, 100, 30).contains("latest output after resuming"));
            assert!(app.logs.reader().view.autoscroll);
            assert!(offset(&app) > bottom);

            app.scroll_speed = 1;
            wheel(&mut app, MouseEventKind::ScrollUp);
            assert!(!app.logs.reader().view.autoscroll);
            app.scroll_speed = 3;
            wheel(&mut app, MouseEventKind::ScrollDown);
            assert!(
                app.logs.reader().view.autoscroll,
                "overshooting from above the bottom resumes too"
            );
            assert_eq!(app.editor().unwrap().rope, "λ");
        }
    }
}

#[test]
fn autoscroll_labels_fit_toolbar_and_match_the_overflow_menu() {
    let log = Log::default();
    log.event("Test", "short output");
    let mut app = app(log);
    for width in [80, 100, 140] {
        let screen = draw(&mut app, width, 30);
        assert!(screen.contains("✓ Autoscroll"));
        assert!(!screen.contains("Follow"));
        assert!(control(&app, Control::Autoscroll).width >= 12);
        assert!(menu_actions(&app, Control::More).contains(&("✓ Autoscroll", Control::Autoscroll)));
    }
    let button = control(&app, Control::Autoscroll);
    click(&mut app, button);
    assert!(!app.logs.reader().view.autoscroll);
    assert!(menu_actions(&app, Control::More).contains(&("Autoscroll", Control::Autoscroll)));
    // A log that fits entirely in the viewport still accepts downward overscroll.
    wheel(&mut app, MouseEventKind::ScrollDown);
    assert!(app.logs.reader().view.autoscroll);
    app.mouse_col = button.x;
    app.mouse_row = button.y;
    let tooltip = hint(&app).unwrap();
    assert!(tooltip.contains("Autoscroll") && tooltip.contains("resume"));
    assert!(!tooltip.contains("Follow"));
}

#[test]
fn downward_keyboard_scroll_resumes_autoscroll_and_shift_selection_stays_paused() {
    for wrap in [false, true] {
        let log = Log::default();
        for _ in 0..40 {
            log.event("Test", &"text ".repeat(40));
        }
        let mut app = app(log.clone());
        app.logs.reader().wrap = wrap;
        draw(&mut app, 100, 30);
        for (code, modifiers) in [
            (KeyCode::Down, KeyModifiers::CONTROL),
            (KeyCode::PageDown, KeyModifiers::NONE),
            (KeyCode::Down, KeyModifiers::NONE),
        ] {
            action(&mut app, Control::Autoscroll);
            assert!(!app.logs.reader().view.autoscroll);
            input(&mut app, code, modifiers);
            assert!(app.logs.reader().view.autoscroll, "{code:?} {modifiers:?}");
            log.event("Test", "keyboard resumed output");
            assert!(draw(&mut app, 100, 30).contains("keyboard resumed output"));
        }
        input(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        let selected = app.logs.selected_text();
        for code in [KeyCode::Down, KeyCode::PageDown] {
            input(&mut app, code, KeyModifiers::SHIFT);
            assert!(!app.logs.reader().view.autoscroll);
            assert_eq!(app.logs.selected_text(), selected);
        }
    }
}

#[test]
fn dragging_past_the_vertical_scrollbar_resumes_but_selecting_text_does_not() {
    for wrap in [false, true] {
        let log = Log::default();
        for _ in 0..40 {
            log.event("Test", &"text ".repeat(40));
        }
        let mut app = app(log.clone());
        app.logs.reader().wrap = wrap;
        draw(&mut app, 100, 30);
        let text = app.logs.reader().text;
        pointer(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            text.right(),
            text.y,
            KeyModifiers::NONE,
        );
        assert!(!app.logs.reader().view.autoscroll);
        for kind in [
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            pointer(
                &mut app,
                kind,
                text.right(),
                text.bottom() + 2,
                KeyModifiers::NONE,
            );
            assert!(app.logs.reader().view.autoscroll);
        }
        assert!(!app.logs.reader().capturing());
        log.event("Test", "scrollbar resumed output");
        assert!(draw(&mut app, 100, 30).contains("scrollbar resumed output"));
        let text = app.logs.reader().text;
        pointer(
            &mut app,
            MouseEventKind::Down(MouseButton::Left),
            text.x + 2,
            text.y + 1,
            KeyModifiers::NONE,
        );
        for kind in [
            MouseEventKind::Drag(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            pointer(
                &mut app,
                kind,
                text.x + 5,
                text.bottom() + 2,
                KeyModifiers::NONE,
            );
        }
        assert!(!app.logs.reader().view.autoscroll);
        assert!(app.logs.selected_text().is_some());
    }
}
