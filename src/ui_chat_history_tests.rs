use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use vtcode::acp::{Event, Operation};

fn open(app: &mut App, id: &str, loaded: bool) -> p::SessionId {
    let id = p::SessionId::new(id.to_owned());
    app.apply_acp_event(Event::SessionOpened {
        session_id: id.clone(),
        modes: None,
        config_options: vec![],
        loaded,
    });
    id
}

fn listed(app: &App, id: &p::SessionId) -> bool {
    app.chat
        .choices
        .iter()
        .any(|c| c.action == ChatAction::Switch(id.clone()))
}

#[test]
fn acp_chat_history_failed_replay_keeps_saved_chat_visible_and_retryable() {
    for operation in [Operation::LoadSession, Operation::ResumeSession] {
        let mut app = tests::seeded();
        let current = app.chat.selected.clone().unwrap();
        let saved = p::SessionId::new("saved-chat");
        app.chat.remote_sessions.push(
            p::SessionInfo::new(saved.clone(), app.explorer.root.clone()).title("Saved chat"),
        );
        app.paste_into_focused_text_input("Keep my current draft");
        app.chat.control_busy = true;
        app.chat.loading_session = Some(saved.clone());
        app.apply_acp_event(Event::Update(p::SessionNotification::new(
            saved.clone(),
            p::SessionUpdate::AgentMessageChunk(p::ContentChunk::new(p::ContentBlock::Text(
                p::TextContent::new("Incomplete replay"),
            ))),
        )));
        assert!(app.chat.sessions.contains_key(&saved));
        assert!(!app.chat.session_order.contains(&saved));
        // Clicking the same entry while replay is in flight must not expose
        // an unestablished session as a usable conversation.
        app.chat_action(ChatAction::Switch(saved.clone()));
        assert_eq!(app.chat.selected.as_ref(), Some(&current));
        assert!(!app.chat.open_tabs.contains(&saved));

        app.apply_acp_event(Event::Error {
            operation,
            session_id: Some(saved.clone()),
            error: p::Error::new(-32000, "Temporary history load failure; try again"),
        });
        assert!(!app.chat.control_busy);
        assert!(app.chat.loading_session.is_none());
        assert!(!app.chat.sessions.contains_key(&saved));
        assert!(!app.chat.unread_tabs.contains(&saved));
        assert_eq!(app.chat.selected.as_ref(), Some(&current));
        assert_eq!(app.chat.input, "Keep my current draft");
        assert!(app.chat.error.as_deref().unwrap().contains("try again"));
        app.chat_action(ChatAction::Picker(Picker::Sessions));
        assert!(
            listed(&app, &saved),
            "a failed replay must remain available for retry"
        );
        assert!(listed(&app, &current));
    }
}

#[test]
fn acp_chat_history_failed_duplicate_load_preserves_an_established_conversation() {
    let mut app = tests::seeded();
    let current = app.chat.selected.clone().unwrap();
    let entries = app.chat.session().unwrap().entries.len();
    app.apply_acp_event(Event::Error {
        operation: Operation::LoadSession,
        session_id: Some(current.clone()),
        error: p::Error::new(-32000, "This session is already open"),
    });
    assert!(app.chat.sessions.contains_key(&current));
    assert!(app.chat.open_tabs.contains(&current));
    assert_eq!(app.chat.session().unwrap().entries.len(), entries + 1);
    app.chat_action(ChatAction::Switch(current.clone()));
    assert_eq!(app.chat.selected.as_ref(), Some(&current));
}

fn render(app: &mut App, terminal: &mut Terminal<TestBackend>) {
    app.text_input_regions.clear();
    terminal.draw(|f| draw(f, app, f.area())).unwrap();
}

fn click(app: &mut App, terminal: &mut Terminal<TestBackend>, action: ChatAction) {
    render(app, terminal);
    let hit = app
        .chat
        .hits
        .iter()
        .find(|h| h.action == action)
        .unwrap_or_else(|| panic!("missing {action:?}"))
        .area;
    app.handle_chat_mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: hit.x,
        row: hit.y,
        modifiers: KeyModifiers::empty(),
    });
}

#[test]
fn acp_chat_history_omits_empty_drafts_and_preserves_unknown_remote_history() {
    let mut app = tests::seeded();
    let conversation = app.chat.selected.clone().unwrap();
    let empty = open(&mut app, "empty", false);
    app.paste_into_focused_text_input("Unsent draft");
    app.chat.files.attached.push("context.rs".into());
    app.chat
        .session_mut()
        .unwrap()
        .push(ChatEntry::Notice("Startup information".into()));
    app.chat
        .session_mut()
        .unwrap()
        .push(ChatEntry::Error("Configuration error".into()));
    app.chat.session_mut().unwrap().apply(
        serde_json::from_value(
            serde_json::json!({"sessionUpdate":"session_info_update","title":"Named but empty"}),
        )
        .unwrap(),
    );
    let remote = p::SessionId::new("remote-without-title");
    app.apply_acp_event(Event::SessionsListed(p::ListSessionsResponse::new(vec![
        p::SessionInfo::new(empty.clone(), app.explorer.root.clone()),
        p::SessionInfo::new(remote.clone(), app.explorer.root.clone()),
    ])));
    app.chat_action(ChatAction::Picker(Picker::Sessions));
    assert!(listed(&app, &conversation));
    assert!(
        listed(&app, &remote),
        "an absent title is not proof of an empty chat"
    );
    assert!(!listed(&app, &empty));
    app.chat_action(ChatAction::CloseTab(empty.clone()));
    assert!(!app.chat.sessions.contains_key(&empty));
    assert!(!app.chat.open_tabs.contains(&empty));
    app.apply_acp_event(Event::SessionsListed(p::ListSessionsResponse::new(vec![
        p::SessionInfo::new(empty.clone(), app.explorer.root.clone()),
    ])));
    app.chat_action(ChatAction::DismissPicker);
    app.chat_action(ChatAction::Picker(Picker::Sessions));
    assert!(
        !listed(&app, &empty),
        "a stale list must not resurrect a discarded empty tab"
    );

    app.chat
        .initialized
        .as_mut()
        .unwrap()
        .agent_capabilities
        .load_session = false;
    let resumed = open(&mut app, "resume-without-replay", true);
    app.chat_action(ChatAction::Picker(Picker::Sessions));
    assert!(listed(&app, &resumed));
    app.chat_action(ChatAction::CloseTab(resumed.clone()));
    assert!(app.chat.sessions.contains_key(&resumed));
}

#[test]
fn acp_chat_history_delete_updates_open_tabs_and_rejects_stale_lists() {
    let mut app = tests::seeded();
    let first = app.chat.selected.clone().unwrap();
    let other = open(&mut app, "other", false);
    app.chat
        .session_mut()
        .unwrap()
        .begin_prompt("Keep this conversation".into());
    app.chat
        .session_mut()
        .unwrap()
        .finish(Some(p::StopReason::EndTurn));
    app.paste_into_focused_text_input("Keep this draft");
    app.chat_action(ChatAction::Picker(Picker::Sessions));
    app.apply_acp_event(Event::Error {
        operation: Operation::DeleteSession,
        session_id: Some(first.clone()),
        error: p::Error::new(-32000, "Deletion failed"),
    });
    assert!(app.chat.sessions.contains_key(&first));
    assert!(listed(&app, &first));
    assert!(
        app.chat
            .error
            .as_deref()
            .unwrap()
            .contains("Deletion failed")
    );
    app.apply_acp_event(Event::SessionDeleted(first.clone()));
    assert_eq!(app.chat.selected.as_ref(), Some(&other));
    assert_eq!(app.chat.input, "Keep this draft");
    assert!(!listed(&app, &first));
    app.apply_acp_event(Event::SessionsListed(p::ListSessionsResponse::new(vec![
        p::SessionInfo::new(first.clone(), app.explorer.root.clone()),
    ])));
    assert!(!listed(&app, &first));
    app.apply_acp_event(Event::SessionDeleted(other));
    assert!(app.chat.open_tabs.is_empty());
    assert!(app.chat.selected.is_none());
    assert!(app.chat.input.is_empty());
}

#[test]
fn acp_chat_history_delete_controls_follow_capabilities_busy_state_and_theme() {
    for theme in ["dark", "light", "high-contrast", "monokai"] {
        for width in [28, 58, 100] {
            let mut app = tests::seeded();
            app.set_theme_by_key(theme);
            let id = app.chat.selected.clone().unwrap();
            app.chat
                .initialized
                .as_mut()
                .unwrap()
                .agent_capabilities
                .session_capabilities
                .delete = Some(p::SessionDeleteCapabilities::new());
            app.chat_action(ChatAction::Picker(Picker::Sessions));
            let mut terminal = Terminal::new(TestBackend::new(width, 32)).unwrap();
            render(&mut app, &mut terminal);
            let delete = app
                .chat
                .hits
                .iter()
                .find(|h| h.action == ChatAction::DeleteSession(id.clone()))
                .unwrap()
                .area;
            for hit in &app.chat.hits {
                assert!(hit.area.right() <= width && hit.area.bottom() <= 32);
                if matches!(hit.action, ChatAction::Pick(_)) {
                    assert!(!hit.area.contains((delete.x, delete.y).into()));
                }
            }
            let normal = terminal
                .backend()
                .buffer()
                .cell((delete.x, delete.y))
                .unwrap()
                .style();
            app.mouse_col = delete.x;
            app.mouse_row = delete.y;
            render(&mut app, &mut terminal);
            assert_ne!(
                normal,
                terminal
                    .backend()
                    .buffer()
                    .cell((delete.x, delete.y))
                    .unwrap()
                    .style(),
                "hover may use the high contrast outline instead of a fill"
            );
            if let Some(folder) = std::env::var_os("VTCODE_CHAT_CAPTURE") {
                let buffer = terminal.backend().buffer();
                let cells: Vec<_> = buffer.content.iter().map(|c| serde_json::json!({"text":c.symbol(),"fg":format!("{:?}",c.fg),"bg":format!("{:?}",c.bg),"bold":c.modifier.contains(Modifier::BOLD)})).collect();
                std::fs::write(
                    std::path::Path::new(&folder)
                        .join(format!("chat-history-{theme}-{width}.json")),
                    serde_json::to_vec(
                        &serde_json::json!({"width":width,"height":32,"cells":cells}),
                    )
                    .unwrap(),
                )
                .unwrap();
            }
            app.chat.session_mut().unwrap().busy = true;
            render(&mut app, &mut terminal);
            assert!(
                !app.chat
                    .hits
                    .iter()
                    .any(|h| matches!(h.action, ChatAction::DeleteSession(_)))
            );
            app.chat.session_mut().unwrap().busy = false;
            app.chat
                .initialized
                .as_mut()
                .unwrap()
                .agent_capabilities
                .session_capabilities
                .delete = None;
            render(&mut app, &mut terminal);
            assert!(
                app.chat
                    .hits
                    .iter()
                    .any(|h| matches!(h.action, ChatAction::DeleteSession(_)))
            );
            app.chat.picker_selected = app
                .chat
                .choices
                .iter()
                .position(|c| c.action == ChatAction::Switch(id.clone()))
                .unwrap();
            app.handle_chat_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::empty()));
            assert!(!app.chat.sessions.contains_key(&id));
            assert!(app.chat.error.is_none());
        }
    }
}

fn history_until(app: &mut App, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done(app) {
        assert!(
            Instant::now() < deadline,
            "history timed out: {:?}",
            app.chat.error
        );
        app.poll_acp();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn history_agent(root: &std::path::Path) -> vtcode::acp::AgentConfig {
    let mut config = vtcode::acp::AgentConfig::new(
        "Persistent SDK history peer",
        std::env::var("VTCODE_ACP_TEST_PYTHON").expect("set VTCODE_ACP_TEST_PYTHON; see tests/fixtures/acp-python-sdk/README.md"),
    );
    config.args = vec![
        "-u".into(),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/history_agent.py")
            .to_string_lossy()
            .into_owned(),
    ];
    for (key, value) in [
        (
            "ACP_TEST_HISTORY_FILE",
            root.join("history.json").to_string_lossy().into_owned(),
        ),
        (
            "ACP_TEST_HISTORY_LOG",
            root.join("events.jsonl").to_string_lossy().into_owned(),
        ),
        ("PYTHONUTF8", "1".into()),
        ("PYTHONIOENCODING", "utf-8".into()),
    ] {
        config.env.insert(key.into(), value);
    }
    config
}

fn connect_history_app(root: &std::path::Path, config: vtcode::acp::AgentConfig) -> App {
    let mut app = App::test_new();
    app.explorer.root = root.into();
    app.settings_effective.acp_agents = Some(vec![config]);
    app.focus_chat();
    app.chat_action(ChatAction::Connect(0));
    history_until(&mut app, |a| {
        a.chat.selected.is_some() && !a.chat.control_busy && a.chat.rollback.initialized
    });
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    app
}

fn reopen_history(app: &mut App, terminal: &mut Terminal<TestBackend>, id: &p::SessionId) {
    click(app, terminal, ChatAction::Picker(Picker::Sessions));
    history_until(app, |a| !a.chat.control_busy);
    let choice = app
        .chat
        .choices
        .iter()
        .position(|c| c.action == ChatAction::Switch(id.clone()))
        .expect("the saved conversation must appear in History");
    click(app, terminal, ChatAction::Pick(choice));
    history_until(app, |a| !a.chat.control_busy);
    assert_eq!(app.chat.selected.as_ref(), Some(id));
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
}

const HISTORY_MARKER: &str = "Remember pineapple 🥝";

fn continue_history(app: &mut App, id: &p::SessionId, text: &str, turn: usize) {
    app.paste_into_focused_text_input(text);
    app.handle_chat_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
    assert!(
        app.chat.busy(),
        "the follow-up must actually be sent: {:?}",
        app.chat.error
    );
    history_until(app, |a| !a.chat.busy());
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    assert_eq!(app.chat.selected.as_ref(), Some(id));
    let expected = format!("Turn {turn}; first message: {HISTORY_MARKER}; session: {id}");
    assert!(
        app.chat
            .session()
            .unwrap()
            .entries
            .iter()
            .any(|e| matches!(e, ChatEntry::Agent(text) if text == &expected)),
        "the agent must continue the same conversation with its original context"
    );
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn acp_chat_sdk_round_trip_history_continues_after_tab_close_reconnect_and_restart() {
    for resume_only in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut config = history_agent(root.path());
        if resume_only {
            config.env.insert("ACP_TEST_RESUME_ONLY".into(), "1".into());
        }
        let mut app = connect_history_app(root.path(), config.clone());
        let id = app.chat.selected.clone().unwrap();
        let mut terminal = Terminal::new(TestBackend::new(92, 32)).unwrap();
        continue_history(&mut app, &id, HISTORY_MARKER, 1);
        click(&mut app, &mut terminal, ChatAction::CloseTab(id.clone()));
        assert!(app.chat.selected.is_none());
        reopen_history(&mut app, &mut terminal, &id);
        continue_history(&mut app, &id, "Continue the closed chat", 2);

        app.chat_action(ChatAction::Reconnect);
        history_until(&mut app, |a| {
            a.chat.selected.is_some() && !a.chat.control_busy && a.chat.rollback.initialized
        });
        assert_ne!(app.chat.selected.as_ref(), Some(&id));
        reopen_history(&mut app, &mut terminal, &id);
        assert!(app.chat.session().unwrap().has_conversation);
        assert_eq!(app.chat.session().unwrap().entries.is_empty(), resume_only);
        continue_history(&mut app, &id, "Continue after reconnecting", 3);
        app.chat_action(ChatAction::Disconnect);
        drop(app);

        // A new App has no in-memory transcript or session handle. The peer
        // must recover its own persisted context before receiving the prompt.
        let mut restarted = connect_history_app(root.path(), config);
        reopen_history(&mut restarted, &mut terminal, &id);
        assert_eq!(
            restarted.chat.session().unwrap().entries.is_empty(),
            resume_only
        );
        continue_history(&mut restarted, &id, "Continue after restarting", 4);
        restarted.chat_action(ChatAction::Disconnect);
        let log = std::fs::read_to_string(root.path().join("events.jsonl")).unwrap();
        let events: Vec<serde_json::Value> = log
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        let prompts: Vec<_> = events.iter().filter(|e| e["method"] == "prompt").collect();
        assert_eq!(prompts.len(), 4);
        for (index, prompt) in prompts.iter().enumerate() {
            assert_eq!(prompt["sessionId"], id.to_string());
            assert_eq!(prompt["turn"], index + 1);
            assert_eq!(prompt["first"], HISTORY_MARKER);
        }
        let restore = if resume_only { "resume" } else { "load" };
        assert_eq!(events.iter().filter(|e| e["method"] == restore).count(), 2);
        assert!(!events.iter().any(|e| e["method"] == "unknown-prompt"));
    }
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn acp_chat_sdk_round_trip_history_partial_load_failure_can_retry_and_continue() {
    let root = tempfile::tempdir().unwrap();
    let mut config = history_agent(root.path());
    config
        .env
        .insert("ACP_TEST_HISTORY_FAIL_LOAD_ONCE".into(), "1".into());
    let mut app = connect_history_app(root.path(), config);
    let id = app.chat.selected.clone().unwrap();
    continue_history(&mut app, &id, HISTORY_MARKER, 1);
    app.chat_action(ChatAction::Reconnect);
    history_until(&mut app, |a| {
        a.chat.selected.is_some() && !a.chat.control_busy && a.chat.rollback.initialized
    });
    let current = app.chat.selected.clone().unwrap();
    let mut terminal = Terminal::new(TestBackend::new(92, 32)).unwrap();
    click(
        &mut app,
        &mut terminal,
        ChatAction::Picker(Picker::Sessions),
    );
    history_until(&mut app, |a| !a.chat.control_busy);
    let choice = app
        .chat
        .choices
        .iter()
        .position(|c| c.action == ChatAction::Switch(id.clone()))
        .unwrap();
    click(&mut app, &mut terminal, ChatAction::Pick(choice));
    history_until(&mut app, |a| !a.chat.control_busy);
    assert_eq!(app.chat.selected.as_ref(), Some(&current));
    assert!(!app.chat.sessions.contains_key(&id));
    assert!(app.chat.error.as_deref().unwrap().contains("try again"));
    reopen_history(&mut app, &mut terminal, &id);
    assert!(
        !app.chat
            .session()
            .unwrap()
            .entries
            .iter()
            .any(|e| matches!(e, ChatEntry::Agent(text) if text.contains("Incomplete replay")))
    );
    continue_history(&mut app, &id, "Continue after retrying the load", 2);
    app.chat_action(ChatAction::Disconnect);
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn acp_chat_sdk_round_trip_history_deletion_and_empty_cleanup() {
    fn until(app: &mut App, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !done(app) {
            assert!(
                Instant::now() < deadline,
                "agent timed out: {:?}",
                app.chat.error
            );
            app.poll_acp();
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("deleted.txt");
    let mut app = App::test_new();
    app.explorer.root = root.path().into();
    let mut config = vtcode::acp::AgentConfig::new(
        "Official SDK scenario agent",
        std::env::var("VTCODE_ACP_TEST_PYTHON").expect("set VTCODE_ACP_TEST_PYTHON; see tests/fixtures/acp-python-sdk/README.md"),
    );
    config.args = vec![
        "-u".into(),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/scenario_agent.py")
            .to_string_lossy()
            .into_owned(),
    ];
    config.env.insert(
        "ACP_TEST_DELETE_LOG".into(),
        log.to_string_lossy().into_owned(),
    );
    config
        .env
        .insert("ACP_TEST_LIVE_HISTORY".into(), "1".into());
    app.settings_effective.acp_agents = Some(vec![config]);
    app.focus_chat();
    app.chat_action(ChatAction::Connect(0));
    until(&mut app, |a| a.chat.selected.is_some());
    let first = app.chat.selected.clone().unwrap();
    app.chat_action(ChatAction::Picker(Picker::Sessions));
    until(&mut app, |a| !a.chat.control_busy);
    assert!(!listed(&app, &first));
    app.chat_action(ChatAction::DismissPicker);
    app.chat_action(ChatAction::New);
    until(&mut app, |a| a.chat.selected.as_ref() != Some(&first));
    let empty = app.chat.selected.clone().unwrap();
    app.chat_action(ChatAction::CloseTab(empty.clone()));
    until(&mut app, |a| !a.chat.deleting_sessions.contains(&empty));
    assert!(!app.chat.sessions.contains_key(&empty));
    app.paste_into_focused_text_input("A real conversation");
    app.chat_action(ChatAction::Send);
    until(&mut app, |a| !a.chat.busy());
    app.chat_action(ChatAction::Picker(Picker::Sessions));
    until(&mut app, |a| !a.chat.control_busy);
    assert!(listed(&app, &first));
    let mut terminal = Terminal::new(TestBackend::new(82, 32)).unwrap();
    click(
        &mut app,
        &mut terminal,
        ChatAction::DeleteSession(first.clone()),
    );
    until(&mut app, |a| !a.chat.sessions.contains_key(&first));
    assert!(!listed(&app, &first));
    // Delete an agent-owned history entry without loading it first.
    let remote = p::SessionId::new("history-a");
    click(
        &mut app,
        &mut terminal,
        ChatAction::DeleteSession(remote.clone()),
    );
    until(&mut app, |a| !a.chat.deleting_sessions.contains(&remote));
    app.chat_action(ChatAction::DismissPicker);
    app.chat_action(ChatAction::Picker(Picker::Sessions));
    until(&mut app, |a| !a.chat.control_busy);
    assert!(!listed(&app, &remote));
    assert!(!listed(&app, &first));
    app.chat_action(ChatAction::DismissPicker);
    app.chat_action(ChatAction::Switch(p::SessionId::new("delete-fails")));
    until(&mut app, |a| !a.chat.control_busy);
    app.chat_action(ChatAction::Picker(Picker::Sessions));
    until(&mut app, |a| !a.chat.control_busy);
    click(
        &mut app,
        &mut terminal,
        ChatAction::DeleteSession(p::SessionId::new("delete-fails")),
    );
    until(&mut app, |a| a.chat.deleting_sessions.is_empty());
    assert!(listed(&app, &p::SessionId::new("delete-fails")));
    assert!(
        app.chat
            .error
            .as_deref()
            .unwrap()
            .contains("Test deletion failed")
    );
    app.chat_action(ChatAction::DismissPicker);
    app.chat_action(ChatAction::New);
    until(&mut app, |a| !a.chat.control_busy);
    let abandoned = app.chat.selected.clone().unwrap();
    app.chat_action(ChatAction::Disconnect);
    let deleted = std::fs::read_to_string(log).unwrap();
    for id in [first, empty, remote, abandoned] {
        assert!(
            deleted.lines().any(|line| line == id.to_string()),
            "{id} was not removed at the agent"
        );
    }
    assert!(!deleted.contains("delete-fails"));
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn acp_chat_sdk_round_trip_history_removal_without_delete_capability_survives_reconnect() {
    fn until(app: &mut App, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !done(app) {
            assert!(
                Instant::now() < deadline,
                "agent timed out: {:?}",
                app.chat.error
            );
            app.poll_acp();
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("deleted.txt");
    let mut config = vtcode::acp::AgentConfig::new(
        "Official SDK scenario agent",
        std::env::var("VTCODE_ACP_TEST_PYTHON").expect("set VTCODE_ACP_TEST_PYTHON; see tests/fixtures/acp-python-sdk/README.md"),
    );
    config.args = vec![
        "-u".into(),
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/scenario_agent.py")
            .to_string_lossy()
            .into_owned(),
    ];
    config.env.insert("ACP_TEST_NO_DELETE".into(), "1".into());
    config.env.insert(
        "ACP_TEST_DELETE_LOG".into(),
        log.to_string_lossy().into_owned(),
    );
    let remote = p::SessionId::new("history-a");
    for attempt in 0..2 {
        let mut app = App::test_new();
        app.explorer.root = root.path().into();
        app.settings_effective.acp_agents = Some(vec![config.clone()]);
        app.focus_chat();
        app.chat_action(ChatAction::Connect(0));
        until(&mut app, |a| a.chat.selected.is_some());
        app.chat_action(ChatAction::Picker(Picker::Sessions));
        until(&mut app, |a| !a.chat.control_busy);
        if attempt == 0 {
            assert!(listed(&app, &remote));
            let mut terminal = Terminal::new(TestBackend::new(82, 32)).unwrap();
            click(
                &mut app,
                &mut terminal,
                ChatAction::DeleteSession(remote.clone()),
            );
        }
        assert!(!listed(&app, &remote));
        assert!(app.chat.error.is_none());
        app.chat_action(ChatAction::Disconnect);
        assert!(
            !log.exists(),
            "the agent must not receive unsupported deletion requests"
        );
    }
}
