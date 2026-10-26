use super::*;
use ratatui::{Terminal, backend::TestBackend};
use vtcode::acp::{Event, Operation};

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

fn choose(app: &mut App, action: ChatAction) {
    let index = app
        .chat
        .choices
        .iter()
        .position(|c| c.action == action)
        .unwrap();
    app.chat_action(ChatAction::Pick(index));
}

fn pending(app: &mut App) -> p::SessionId {
    let id = app.chat.selected.clone().unwrap();
    app.chat.model_change = Some(id.clone());
    app.chat.control_busy = true;
    app.chat_action(ChatAction::Picker(Picker::Thinking));
    id
}

#[test]
fn model_and_thinking_share_a_responsive_control_with_direct_effort_access() {
    for theme in ["dark", "light", "high-contrast"] {
        for width in [36, 66, 100] {
            let mut app = tests::seeded();
            app.set_theme_by_key(theme);
            let mut terminal = Terminal::new(TestBackend::new(width, 40)).unwrap();
            let screen = render(&mut app, &mut terminal);
            assert!(screen.contains("Workspace Large * Balanced"));
            assert!(
                app.chat
                    .hits
                    .iter()
                    .any(|h| h.action == ChatAction::Picker(Picker::Models))
            );
            assert!(
                !app.chat
                    .hits
                    .iter()
                    .any(|h| h.action == ChatAction::Picker(Picker::Values("effort-29".into())))
            );
            assert!(app.chat.hits.iter().all(|h| h.area.right() <= width));
            presentation_tests::capture(&terminal, &format!("model-thinking-{theme}-{width}"));
            app.chat_action(ChatAction::Picker(Picker::Models));
            choose(&mut app, ChatAction::Picker(Picker::Thinking));
            assert_eq!(app.chat.picker, Some(Picker::Thinking));
            assert!(app.chat.choices.iter().any(|c| c.label == "✓ Balanced"));
            assert!(app.chat.choices.iter().any(|c| c.label == "Deep"));
            render(&mut app, &mut terminal);
            assert!(app.chat.hits.iter().all(|h| h.area.right() <= width));
            presentation_tests::capture(&terminal, &format!("model-thinking-menu-{theme}-{width}"));
            choose(&mut app, ChatAction::Picker(Picker::Models));
            assert_eq!(app.chat.picker, Some(Picker::Models));
            // The current model needs no connection just to choose its effort.
            app.chat_action(ChatAction::Pick(0));
            assert_eq!(app.chat.picker, Some(Picker::Thinking));
            assert!(!app.chat.control_busy);
            assert!(app.chat.error.is_none());
        }
    }
}

#[test]
fn thinking_menu_waits_for_confirmed_model_options_and_preserves_dismissal() {
    let mut app = tests::seeded();
    let mut terminal = Terminal::new(TestBackend::new(90, 40)).unwrap();
    let id = pending(&mut app);
    assert!(app.chat.choices.is_empty());
    assert!(render(&mut app, &mut terminal).contains("Changing model…"));
    let options: Vec<p::SessionConfigOption> = serde_json::from_value(serde_json::json!([
        {"id":"engine-b47","name":"Model","category":"model","type":"select","currentValue":"new","options":[{"value":"new","name":"New model"}]},
        {"id":"new-effort","name":"Effort","category":"thought_level","type":"select","currentValue":"maximum","options":[{"value":"low","name":"Low"},{"value":"maximum","name":"Maximum"}]}
    ])).unwrap();
    // A notification can precede the set-config response. Don't offer stale
    // choices or allow another request while the first is still pending.
    app.apply_acp_event(Event::Update(p::SessionNotification::new(
        id.clone(),
        p::SessionUpdate::ConfigOptionUpdate(p::ConfigOptionUpdate::new(options.clone())),
    )));
    assert!(app.chat.choices.is_empty());
    app.apply_acp_event(Event::ConfigChanged {
        session_id: id.clone(),
        options: options.clone(),
    });
    assert_eq!(app.chat.picker, Some(Picker::Thinking));
    assert!(!app.chat.thinking_loading());
    assert_eq!(app.chat.choices[1].label, "✓ Maximum");
    assert_eq!(app.chat.picker_selected, 1);
    assert!(
        matches!(&app.chat.choices[0].action, ChatAction::Config(key, _) if key.to_string() == "new-effort")
    );
    render(&mut app, &mut terminal);
    presentation_tests::capture(&terminal, "model-thinking-confirmed");

    app.chat_action(ChatAction::DismissPicker);
    pending(&mut app);
    app.chat_action(ChatAction::DismissPicker);
    app.apply_acp_event(Event::ConfigChanged {
        session_id: id,
        options,
    });
    assert!(app.chat.picker.is_none());
}

#[test]
fn model_failure_or_session_switch_cannot_open_effort_for_another_chat() {
    let mut app = tests::seeded();
    let id = pending(&mut app);
    app.apply_acp_event(Event::Error {
        operation: Operation::SetConfig,
        session_id: Some(id.clone()),
        error: p::Error::new(-32000, "Model unavailable"),
    });
    assert!(app.chat.picker.is_none());
    assert!(app.chat.model_change.is_none());
    assert_eq!(app.chat.model_name(), Some("Workspace Large"));
    assert!(
        app.chat
            .error
            .as_deref()
            .unwrap()
            .contains("Model unavailable")
    );

    pending(&mut app);
    let options = app.chat.session().unwrap().config_options.clone();
    let other = p::SessionId::new("other");
    app.chat
        .sessions
        .insert(other.clone(), Session::new(other.clone()));
    app.chat.open_tabs.push(other.clone());
    app.chat_action(ChatAction::Switch(other.clone()));
    app.apply_acp_event(Event::ConfigChanged {
        session_id: id,
        options,
    });
    assert_eq!(app.chat.selected, Some(other));
    assert!(app.chat.picker.is_none());
    assert!(app.chat.model_change.is_none());
}

#[test]
fn combined_model_control_keeps_fixed_and_unknown_effort_explanations() {
    for fixed in [false, true] {
        let mut app = tests::seeded();
        app.chat.session_mut().unwrap().config_options.retain(|o| {
            crate::app_acp::config_category(o) != Some(p::SessionConfigOptionCategory::ThoughtLevel)
        });
        if fixed {
            app.chat.session_mut().unwrap().config_options.push(serde_json::from_value(serde_json::json!({
                "id":"fixed-effort","name":"Effort","category":"thought_level","type":"select","currentValue":"medium","options":[{"value":"medium","name":"Medium"}]
            })).unwrap());
        }
        let mut terminal = Terminal::new(TestBackend::new(92, 40)).unwrap();
        let screen = render(&mut app, &mut terminal);
        assert!(screen.contains(if fixed {
            "Workspace Large * Medium ⊘"
        } else {
            "Workspace Large * ? ⊘"
        }));
        app.chat_action(ChatAction::Picker(Picker::Models));
        app.chat_action(ChatAction::Pick(0));
        assert_eq!(app.chat.picker, Some(Picker::Thinking));
        assert!(render(&mut app, &mut terminal).contains("cannot be changed here"));
        assert!(
            !app.chat
                .choices
                .iter()
                .any(|c| matches!(c.action, ChatAction::Config(..)))
        );
        presentation_tests::capture(
            &terminal,
            if fixed {
                "model-thinking-fixed"
            } else {
                "model-thinking-unknown"
            },
        );
    }
}
