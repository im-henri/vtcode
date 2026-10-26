use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

fn setup() -> App {
    let mut app = tests::seeded();
    let id = app.chat.selected.clone().unwrap();
    *app.chat.session_mut().unwrap() = Session::new(id);
    app.chat
        .session_mut()
        .unwrap()
        .begin_prompt("Inspect the workspace".into());
    app
}

fn thought(app: &mut App, text: &str) {
    app.apply_acp_event(vtcode::acp::Event::Update(p::SessionNotification::new(
        app.chat.selected.clone().unwrap(),
        p::SessionUpdate::AgentThoughtChunk(p::ContentChunk::new(p::ContentBlock::Text(
            p::TextContent::new(text),
        ))),
    )));
}

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

fn expand(app: &mut App, terminal: &mut Terminal<TestBackend>, index: usize) {
    render(app, terminal);
    let area = app
        .chat
        .hits
        .iter()
        .find(|h| h.action == ChatAction::ToggleEntry(index))
        .unwrap()
        .area;
    app.handle_chat_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x,
        row: area.y,
        modifiers: KeyModifiers::empty(),
    });
}

#[test]
fn expanded_thinking_streams_before_answer_and_past_the_old_preview_limit() {
    let mut app = setup();
    let mut terminal = Terminal::new(TestBackend::new(80, 32)).unwrap();
    thought(&mut app, "Inspecting");
    assert!(!render(&mut app, &mut terminal).contains("Inspecting"));
    expand(&mut app, &mut terminal, 1);
    assert!(render(&mut app, &mut terminal).contains("Inspecting"));
    let cached_user = app.chat.cache.entries[0].2[0].clone();
    thought(&mut app, " the **stream**");
    assert!(render(&mut app, &mut terminal).contains("Inspecting the stream"));
    assert!(Arc::ptr_eq(&cached_user, &app.chat.cache.entries[0].2[0]));

    // Long running reasoning must not freeze at the first 300 rendered rows.
    let long = (0..320)
        .map(|i| format!("\n\nConsidering step {i:03}."))
        .collect::<String>();
    thought(&mut app, &long);
    assert!(render(&mut app, &mut terminal).contains("Considering step 319."));
    thought(&mut app, "\n\nNewest thought still arriving");
    assert!(render(&mut app, &mut terminal).contains("Newest thought still arriving"));
    assert!(app.chat.busy());
    assert!(
        !app.chat
            .session()
            .unwrap()
            .entries
            .iter()
            .any(|e| matches!(e, ChatEntry::Agent(_)))
    );
    assert_eq!(app.chat.scroll, app.chat.max_scroll);
    presentation_tests::capture(&terminal, "thinking-live-tail");

    app.handle_chat_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::empty()));
    let scroll = app.chat.scroll;
    let before = render(&mut app, &mut terminal);
    thought(&mut app, "\n\nA later thought while reading earlier lines");
    assert_eq!(render(&mut app, &mut terminal), before);
    assert_eq!(app.chat.scroll, scroll);
    assert!(!app.chat.follow);

    app.chat_action(ChatAction::Follow);
    assert!(
        render(&mut app, &mut terminal).contains("A later thought while reading earlier lines")
    );
    app.chat
        .session_mut()
        .unwrap()
        .finish(Some(p::StopReason::EndTurn));
    assert!(
        render(&mut app, &mut terminal).contains("A later thought while reading earlier lines")
    );
}

#[test]
fn expanding_current_thinking_at_live_end_resumes_stream_following() {
    let mut app = setup();
    let mut terminal = Terminal::new(TestBackend::new(70, 26)).unwrap();
    thought(&mut app, "First partial thought");
    render(&mut app, &mut terminal);
    // Inspecting terminal cards can pause following without leaving the end.
    app.chat.follow = false;
    assert_eq!(app.chat.scroll, app.chat.max_scroll);
    expand(&mut app, &mut terminal, 1);
    assert!(app.chat.follow);
    let more = (0..35)
        .map(|i| format!("\n\nLive step {i:02}"))
        .collect::<String>();
    thought(&mut app, &more);
    assert!(render(&mut app, &mut terminal).contains("Live step 34"));
    assert!(app.chat.busy());
}

#[test]
fn expanding_historical_thinking_does_not_jump_to_the_live_end() {
    let mut app = setup();
    let mut terminal = Terminal::new(TestBackend::new(70, 26)).unwrap();
    thought(&mut app, "Older reasoning");
    for i in 0..20 {
        app.chat
            .session_mut()
            .unwrap()
            .push(ChatEntry::Agent(format!("Earlier answer {i}")));
    }
    thought(&mut app, "Current reasoning");
    render(&mut app, &mut terminal);
    app.chat.follow = false;
    app.chat.scroll = 0;
    expand(&mut app, &mut terminal, 1);
    assert!(render(&mut app, &mut terminal).contains("Older reasoning"));
    thought(&mut app, " is still arriving");
    render(&mut app, &mut terminal);
    assert_eq!(app.chat.scroll, 0);
    assert!(!app.chat.follow);
}

#[test]
fn thinking_default_streams_new_cards_and_is_independent_of_tool_visibility() {
    let root = tempfile::tempdir().unwrap();
    let mut app = setup();
    app.settings_global_path = root.path().join("settings.json");
    app.settings_workspace_path = root.path().join("workspace-settings.json");
    app.settings_scope = crate::settings::SettingsScope::Global;
    app.settings_persistence_enabled = true;
    let mut terminal = Terminal::new(TestBackend::new(90, 38)).unwrap();
    thought(&mut app, "Working through the request");
    assert!(!render(&mut app, &mut terminal).contains("Working through the request"));
    assert!(
        app.chat
            .hits
            .iter()
            .any(|h| h.action == ChatAction::Picker(Picker::Display))
    );
    app.chat_action(ChatAction::Picker(Picker::Display));
    let expanded = app
        .chat
        .choices
        .iter()
        .position(|choice| choice.action == ChatAction::SetThinkingDetail(true))
        .unwrap();
    app.chat_action(ChatAction::Pick(expanded));
    assert_eq!(app.chat.picker, Some(Picker::Display));
    app.chat_action(ChatAction::DismissPicker);
    assert!(render(&mut app, &mut terminal).contains("Working through the request"));
    thought(&mut app, " while the response is still running");
    assert!(render(&mut app, &mut terminal).contains("response is still running"));
    let saved: crate::settings::Settings =
        serde_json::from_slice(&std::fs::read(&app.settings_global_path).unwrap()).unwrap();
    assert_eq!(saved.chat_thinking_expanded, Some(true));
    app.chat_action(ChatAction::ToggleEntry(1));
    app.chat_action(ChatAction::OutputDetail);
    assert!(
        !app.chat.entry_expanded(1),
        "tool preference must preserve the manually folded thought"
    );
    app.chat
        .session_mut()
        .unwrap()
        .push(ChatEntry::Tool(Box::new(p::ToolCall::new(
            "search", "grep",
        ))));
    let tool_index = app.chat.session().unwrap().entries.len() - 1;
    app.chat_action(ChatAction::ToggleEntry(tool_index));
    thought(&mut app, "A new thought is visible automatically");
    assert!(render(&mut app, &mut terminal).contains("A new thought is visible automatically"));
    app.chat_action(ChatAction::ThinkingDetail);
    assert!(
        !app.chat.entry_expanded(tool_index),
        "thinking preference must preserve the manually folded tool"
    );
    assert!(
        !app.chat
            .entry_expanded(app.chat.session().unwrap().entries.len() - 1)
    );
    app.apply_resolved_settings(&saved);
    assert!(app.chat.thinking_expanded);
    app.apply_resolved_settings(&crate::settings::Settings::default());
    assert!(!app.chat.thinking_expanded);
}

#[test]
fn thinking_default_updates_other_conversations_without_losing_their_tool_overrides() {
    let mut app = setup();
    let first = app.chat.selected.clone().unwrap();
    thought(&mut app, "First conversation thought");
    app.chat_action(ChatAction::ToggleEntry(1));
    app.chat
        .session_mut()
        .unwrap()
        .push(ChatEntry::Tool(Box::new(p::ToolCall::new(
            "first-tool",
            "grep",
        ))));
    app.chat_action(ChatAction::ToggleEntry(2));
    let second = p::SessionId::new("second");
    let mut session = Session::new(second.clone());
    session.push(ChatEntry::Thought("Second conversation thought".into()));
    app.chat.sessions.insert(second.clone(), session);
    app.chat.select_tab(Some(second));
    app.chat_action(ChatAction::ThinkingDetail);
    app.chat_action(ChatAction::ThinkingDetail);
    app.chat.select_tab(Some(first));
    assert!(!app.chat.entry_expanded(1));
    assert!(app.chat.entry_expanded(2));
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn acp_chat_sdk_round_trip_expanded_thinking_while_prompt_is_pending() {
    fn wait(app: &mut App, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            app.poll_acp();
            assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
            if done(app) {
                return;
            }
            assert!(Instant::now() < deadline, "Timed out: {}", app.chat.status);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let root = tempfile::tempdir().unwrap();
    let mut app = App::test_new();
    app.explorer.root = root.path().to_path_buf();
    let python = std::env::var("VTCODE_ACP_TEST_PYTHON").expect("set VTCODE_ACP_TEST_PYTHON; see tests/fixtures/acp-python-sdk/README.md");
    let mut config = vtcode::acp::AgentConfig::new("Official SDK scenario agent", python);
    config.args = vec![
        "-u".into(),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/scenario_agent.py")
            .to_string_lossy()
            .into_owned(),
    ];
    app.settings_effective.acp_agents = Some(vec![config]);
    app.focus_chat();
    app.chat_action(ChatAction::Connect(0));
    wait(&mut app, |app| app.chat.selected.is_some());
    app.paste_into_focused_text_input("thinking-stream");
    app.chat_action(ChatAction::Send);
    wait(&mut app, |app| {
        app.chat
            .session()
            .is_some_and(|s| s.entries.iter().any(|e| matches!(e, ChatEntry::Thought(_))))
    });
    let mut terminal = Terminal::new(TestBackend::new(86, 30)).unwrap();
    assert!(!render(&mut app, &mut terminal).contains("First partial thought"));
    expand(&mut app, &mut terminal, 1);
    assert!(render(&mut app, &mut terminal).contains("First partial thought"));
    assert!(app.chat.busy());

    std::fs::write(root.path().join("continue-thinking"), "").unwrap();
    wait(&mut app, |app| {
        app.chat
            .session()
            .unwrap()
            .entries
            .iter()
            .any(|e| matches!(e, ChatEntry::Thought(s) if s.contains("second partial thought")))
    });
    let screen = render(&mut app, &mut terminal);
    assert!(screen.contains("First partial thought; second partial thought"));
    assert!(!screen.contains("Final answer after thinking"));
    assert!(app.chat.busy());
    assert!(app.chat.session().unwrap().stop_reason.is_none());
    presentation_tests::capture(&terminal, "thinking-stream-pending");

    std::fs::write(root.path().join("finish-thinking"), "").unwrap();
    wait(&mut app, |app| !app.chat.busy());
    let screen = render(&mut app, &mut terminal);
    assert!(screen.contains("First partial thought; second partial thought"));
    assert!(screen.contains("Final answer after thinking"));
    app.chat_action(ChatAction::Disconnect);
}
