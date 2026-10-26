use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

fn render(app: &mut App, terminal: &mut Terminal<TestBackend>) -> String {
    app.text_input_regions.clear();
    terminal.draw(|f| draw(f, app, f.area())).unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect()
}

fn choice_area(app: &App, index: usize) -> Rect {
    app.chat
        .hits
        .iter()
        .find(|h| h.action == ChatAction::Pick(index))
        .unwrap()
        .area
}

fn hover(app: &mut App, area: Rect) {
    app.mouse_hover_changed = false;
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: area.x + 3,
            row: area.y,
            modifiers: KeyModifiers::empty(),
        },
    );
    assert!(app.mouse_hover_changed);
}

#[test]
fn mode_hover_previews_description_without_applying_or_moving_the_menu() {
    for width in [36, 100] {
        for theme in ["dark", "light", "high-contrast"] {
            let mut app = tests::seeded();
            app.set_theme_by_key(theme);
            let mut terminal = Terminal::new(TestBackend::new(width, 36)).unwrap();
            app.chat_action(ChatAction::Picker(Picker::Modes));
            assert!(!render(&mut app, &mut terminal).contains("Make changes"));
            let explore = choice_area(&app, 0);
            let build = choice_area(&app, 1);

            hover(&mut app, build);
            assert!(render(&mut app, &mut terminal).contains("Make changes"));
            assert_eq!(app.chat.picker_selected, 1);
            assert_eq!(choice_area(&app, 0), explore);
            assert_eq!(choice_area(&app, 1), build);
            assert_eq!(app.chat.mode_name(), Some("Explore"));
            assert_eq!(app.chat.choices[0].label, "✓ Explore");
            assert_eq!(app.chat.picker, Some(Picker::Modes));
            assert!(!app.chat.control_busy);
            assert!(app.chat.error.is_none());
            presentation_tests::capture(&terminal, &format!("mode-hover-{theme}-{width}"));

            // Keyboard navigation takes over without the stationary pointer
            // leaving two highlighted rows or restoring its old description.
            app.handle_chat_key(KeyEvent::new(KeyCode::Up, KeyModifiers::empty()));
            assert!(!render(&mut app, &mut terminal).contains("Make changes"));
            assert_eq!(app.chat.picker_selected, 0);
            assert_eq!(choice_area(&app, 1), build);
            let buffer = terminal.backend().buffer();
            assert_eq!(
                buffer[(explore.x, explore.y)].bg,
                app.colors.bg_menu_selected
            );
            assert_eq!(buffer[(build.x, build.y)].bg, app.colors.bg_menu);

            app.handle_chat_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty()));
            assert!(render(&mut app, &mut terminal).contains("Make changes"));
            app.handle_chat_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()));
            app.chat_action(ChatAction::Picker(Picker::Modes));
            assert_eq!(app.chat.picker_selected, 0);
            assert_eq!(app.chat.mode_name(), Some("Explore"));
        }
    }
}

#[test]
fn config_mode_hover_shows_wrapped_multiline_descriptions() {
    for width in [36, 100] {
        let mut app = tests::seeded();
        app.chat.session_mut().unwrap().config_options.push(serde_json::from_value(serde_json::json!({
            "id":"behavior","name":"Mode","category":"mode","type":"select","currentValue":"plan",
            "options":[
                {"value":"plan","name":"Plan","description":"Review the request."},
                {"value":"build","name":"Build","description":"Edit files and run commands to implement the requested changes.\nAsks before actions that need permission."}
            ]
        })).unwrap());
        let mut terminal = Terminal::new(TestBackend::new(width, 36)).unwrap();
        app.chat_action(ChatAction::Picker(Picker::Modes));
        assert!(render(&mut app, &mut terminal).contains("Review the request."));
        let build = choice_area(&app, 1);
        hover(&mut app, build);
        let screen = render(&mut app, &mut terminal);
        assert!(screen.contains("Edit files and run commands"));
        assert!(
            screen.contains("permission."),
            "description was cut off at width {width}"
        );
        assert!(!screen.contains("Review the request."));
        assert_eq!(choice_area(&app, 1), build);
        assert_eq!(app.chat.mode_name(), Some("Plan"));
        assert_eq!(app.chat.picker, Some(Picker::Modes));
        assert!(!app.chat.control_busy);
        assert!(app.chat.error.is_none());
        presentation_tests::capture(&terminal, &format!("mode-hover-wrapped-{width}"));
    }
}

#[test]
fn mode_hover_uses_visible_choices_after_scrolling() {
    let mut app = tests::seeded();
    let modes: Vec<_> = (0..12)
        .map(|index| {
            serde_json::json!({
                "id":format!("mode-{index}"), "name":format!("Mode {index}"),
                "description":format!("Description for mode {index}")
            })
        })
        .collect();
    app.chat.session_mut().unwrap().modes = Some(
        serde_json::from_value(serde_json::json!({
            "currentModeId":"mode-0", "availableModes":modes
        }))
        .unwrap(),
    );
    let mut terminal = Terminal::new(TestBackend::new(80, 36)).unwrap();
    app.chat_action(ChatAction::Picker(Picker::Modes));
    for _ in 0..11 {
        app.handle_chat_key(KeyEvent::new(KeyCode::Down, KeyModifiers::empty()));
    }
    assert!(render(&mut app, &mut terminal).contains("Description for mode 11"));
    assert!(app.chat.picker_scroll > 0);
    let first = app.chat.picker_scroll;
    let area = choice_area(&app, first);
    hover(&mut app, area);
    assert!(render(&mut app, &mut terminal).contains(&format!("Description for mode {first}")));
    assert_eq!(app.chat.picker_selected, first);
    assert_eq!(choice_area(&app, first), area);
    assert_eq!(app.chat.mode_name(), Some("Mode 0"));
    assert!(!app.chat.control_busy);
}
