use super::*;
use ratatui::{Terminal, backend::TestBackend};
use vtcode::acp::Event;

fn setup() -> App {
    let mut app = tests::seeded();
    app.chat.agent_order = vec![app.chat.active_agent];
    app.chat.dock_selected = app.chat.selected.clone();
    app.show_explorer = false;
    app.show_panel = false;
    app.terminal_sidebar_revealed = false;
    app
}

fn paint(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
}

fn marker(app: &App, terminal: &Terminal<TestBackend>, action: ChatAction) -> String {
    let hit = app.chat.hits.iter().find(|h| h.action == action).unwrap();
    terminal.backend().buffer()[(hit.area.x + 1, hit.area.y)]
        .symbol()
        .into()
}

fn tick(app: &mut App) {
    app.chat
        .activity
        .tick
        .set(Some(Instant::now() - ACTIVITY_INTERVAL));
    assert!(advance_activity_frame(app));
}

fn finish(app: &mut App, id: &p::SessionId) {
    app.apply_acp_event(Event::PromptFinished {
        session_id: id.clone(),
        response: serde_json::from_value(serde_json::json!({"stopReason":"end_turn"})).unwrap(),
    });
}

#[test]
fn acp_chat_status_spinners_animate_tabs_without_rebuilding_transcript() {
    for theme in ["dark", "light", "high-contrast"] {
        let mut app = setup();
        app.set_theme_by_key(theme);
        let id = app.chat.selected.clone().unwrap();
        let agent = app.chat.active_agent;
        let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
        paint(&mut app, &mut terminal);
        let idle_agent = marker(&app, &terminal, ChatAction::SwitchAgent(agent));
        assert_eq!(marker(&app, &terminal, ChatAction::Switch(id.clone())), "◇");
        assert_eq!(next_activity_frame(&app), None);

        app.chat
            .session_mut()
            .unwrap()
            .begin_prompt("Keep working".into());
        paint(&mut app, &mut terminal);
        assert_eq!(marker(&app, &terminal, ChatAction::SwitchAgent(agent)), "⠋");
        assert_eq!(marker(&app, &terminal, ChatAction::Switch(id.clone())), "⠋");
        let cached = app.chat.cache.rows[0].clone();
        tick(&mut app);
        paint(&mut app, &mut terminal);
        assert_eq!(marker(&app, &terminal, ChatAction::SwitchAgent(agent)), "⠙");
        assert_eq!(marker(&app, &terminal, ChatAction::Switch(id.clone())), "⠙");
        assert!(Arc::ptr_eq(&cached, &app.chat.cache.rows[0]));

        // The tab animation survives scrolling away from the Working row.
        app.chat.follow = false;
        app.chat.scroll = 0;
        paint(&mut app, &mut terminal);
        tick(&mut app);
        paint(&mut app, &mut terminal);
        assert_eq!(marker(&app, &terminal, ChatAction::Switch(id.clone())), "⠹");
        app.chat.follow = true;
        finish(&mut app, &id);
        paint(&mut app, &mut terminal);
        assert_eq!(
            marker(&app, &terminal, ChatAction::SwitchAgent(agent)),
            idle_agent
        );
        assert_eq!(marker(&app, &terminal, ChatAction::Switch(id)), "◇");
        assert_eq!(next_activity_frame(&app), None);

        app.chat
            .session_mut()
            .unwrap()
            .begin_prompt("Hidden work".into());
        app.chat_action(ChatAction::Hide);
        paint(&mut app, &mut terminal);
        assert!(app.chat.any_agent_running());
        assert_eq!(next_activity_frame(&app), None);
    }
}

#[test]
fn acp_chat_status_unseen_completion_survives_background_redraw_and_scrollback() {
    let mut app = setup();
    let id = app.chat.selected.clone().unwrap();
    let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
    app.chat
        .session_mut()
        .unwrap()
        .begin_prompt("Background response".into());
    paint(&mut app, &mut terminal);
    app.terminal_has_focus = false;
    tick(&mut app);
    paint(&mut app, &mut terminal);
    finish(&mut app, &id);
    for _ in 0..3 {
        paint(&mut app, &mut terminal);
        assert!(app.chat.unread_tabs.contains(&id));
        assert_eq!(marker(&app, &terminal, ChatAction::Switch(id.clone())), "●");
        assert_eq!(marker(&app, &terminal, ChatAction::SwitchAgent(0)), "●");
        assert_eq!(next_activity_frame(&app), None);
    }
    app.terminal_has_focus = true;
    paint(&mut app, &mut terminal);
    assert!(!app.chat.unread_tabs.contains(&id));
    assert_eq!(marker(&app, &terminal, ChatAction::Switch(id.clone())), "◇");

    app.chat
        .session_mut()
        .unwrap()
        .begin_prompt("More work".into());
    app.chat.follow = false;
    app.chat.scroll = 0;
    paint(&mut app, &mut terminal);
    finish(&mut app, &id);
    paint(&mut app, &mut terminal);
    assert_eq!(marker(&app, &terminal, ChatAction::Switch(id.clone())), "●");
    app.chat_action(ChatAction::Switch(id.clone()));
    paint(&mut app, &mut terminal);
    assert!(app.chat.unread_tabs.contains(&id));
    app.chat_action(ChatAction::Follow);
    paint(&mut app, &mut terminal);
    assert!(!app.chat.unread_tabs.contains(&id));
    assert_eq!(marker(&app, &terminal, ChatAction::Switch(id)), "◇");
}

#[test]
fn acp_chat_status_another_conversation_runs_and_finishes_without_stealing_focus() {
    let mut app = setup();
    let first = app.chat.selected.clone().unwrap();
    app.chat
        .session_mut()
        .unwrap()
        .begin_prompt("First chat task".into());
    let second = p::SessionId::new("second");
    app.apply_acp_event(Event::SessionOpened {
        session_id: second.clone(),
        modes: None,
        config_options: vec![],
        loaded: false,
    });
    app.chat.input = "Unsent second chat draft".into();
    let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
    paint(&mut app, &mut terminal);
    assert!(!app.chat.busy());
    assert_eq!(
        marker(&app, &terminal, ChatAction::Switch(first.clone())),
        "⠋"
    );
    tick(&mut app);
    paint(&mut app, &mut terminal);
    assert_eq!(
        marker(&app, &terminal, ChatAction::Switch(first.clone())),
        "⠙"
    );
    finish(&mut app, &first);
    paint(&mut app, &mut terminal);
    assert_eq!(
        marker(&app, &terminal, ChatAction::Switch(first.clone())),
        "●"
    );
    assert_eq!(app.chat.selected.as_ref(), Some(&second));
    assert_eq!(app.chat.input, "Unsent second chat draft");
    app.chat_action(ChatAction::Switch(first.clone()));
    paint(&mut app, &mut terminal);
    assert_eq!(marker(&app, &terminal, ChatAction::Switch(first)), "◇");
}

#[test]
fn acp_chat_status_editor_tab_works_with_the_dock_hidden_and_keeps_unseen_results() {
    let mut app = setup();
    let id = app.chat.selected.clone().unwrap();
    app.chat
        .session_mut()
        .unwrap()
        .begin_prompt("Editor chat task".into());
    app.place_chat_in_editor(id.clone(), app.active_group, None);
    let key = app.chat.layout.active.unwrap();
    let kind = crate::app::TabKind::Chat(key);
    let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
    paint(&mut app, &mut terminal);
    app.new_file();
    app.chat.visible = false;
    paint(&mut app, &mut terminal);
    assert!(app.chat.layout.surfaces.is_empty());
    assert_eq!(
        crate::ui_chrome_tabs::chat_tab_marker(&app, kind)
            .unwrap()
            .0,
        "⠋ "
    );
    tick(&mut app);
    paint(&mut app, &mut terminal);
    assert_eq!(
        crate::ui_chrome_tabs::chat_tab_marker(&app, kind)
            .unwrap()
            .0,
        "⠙ "
    );
    // Completion after switching tabs but before the next paint must not be
    // acknowledged using the old editor surface's geometry.
    assert!(app.activate_editor_chat(&id));
    paint(&mut app, &mut terminal);
    app.new_file();
    finish(&mut app, &id);
    paint(&mut app, &mut terminal);
    assert_eq!(
        crate::ui_chrome_tabs::chat_tab_marker(&app, kind)
            .unwrap()
            .0,
        "● "
    );
    assert!(app.chat.unread_tabs.contains(&id));
    assert_eq!(next_activity_frame(&app), None);
    assert!(app.activate_editor_chat(&id));
    paint(&mut app, &mut terminal);
    assert_eq!(
        crate::ui_chrome_tabs::chat_tab_marker(&app, kind)
            .unwrap()
            .0,
        "◇ "
    );
    assert!(!app.chat.unread_tabs.contains(&id));
}
