use super::*;

fn edge_until(app: &mut App, ready: impl Fn(&App) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        app.poll_background_results();
        app.poll_acp();
        if ready(app) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Agent Chat edge case timed out: {} {:?}",
            app.chat.status,
            app.chat.error
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
#[ignore = "requires official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn acp_chat_sdk_round_trip_background_error_during_history_load_preserves_gate_and_saved_chat() {
    let root = tempfile::tempdir().unwrap();
    let gates = tempfile::tempdir().unwrap();
    git2::Repository::init(root.path()).unwrap();
    let mut config = AgentConfig::new(
        "Control edge case",
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
        "ACP_TEST_CONTROL_GATE_DIR".into(),
        gates.path().to_string_lossy().into_owned(),
    );
    config
        .env
        .insert("ACP_TEST_EMPTY_REPLAY".into(), "1".into());
    config.env.insert(
        "ACP_TEST_EDGE_LOG".into(),
        gates
            .path()
            .join("events.jsonl")
            .to_string_lossy()
            .into_owned(),
    );
    let mut app = App::test_new();
    app.explorer.root = root.path().into();
    app.settings_effective.acp_agents = Some(vec![config]);
    app.chat_action(ChatAction::Connect(0));
    edge_until(&mut app, |app| {
        app.chat.selected.is_some()
            && !app.chat.control_busy
            && app.chat.rollback.initialized
            && !app.chat.rollback.busy()
    });
    let running = app.chat.selected.clone().unwrap();
    app.chat.input = "background-error-gated".into();
    app.chat_action(ChatAction::Send);
    edge_until(&mut app, |_| gates.path().join("prompt-ready").exists());
    let saved = p::SessionId::new("history-a");
    app.chat_action(ChatAction::Switch(saved.clone()));
    edge_until(&mut app, |_| gates.path().join("load-ready").exists());
    assert!(app.chat.control_busy);
    std::fs::write(gates.path().join("fail-prompt"), "go").unwrap();
    edge_until(&mut app, |app| {
        !app.chat.sessions[&running].busy
            && !app
                .chat
                .rollback
                .conversation_busy(app.chat.active_agent, Some(&running))
    });
    assert!(app.chat.control_busy);
    assert_eq!(app.chat.loading_session, Some(saved.clone()));
    app.chat.next_cursor = Some("page-two".into());
    app.chat_action(ChatAction::MoreSessions);
    app.chat_action(ChatAction::New);
    assert!(app.chat.control_busy);
    std::fs::write(gates.path().join("finish-load"), "go").unwrap();
    edge_until(&mut app, |app| {
        app.chat.selected.as_ref() == Some(&saved) && !app.chat.control_busy
    });
    assert!(app.chat.session().unwrap().has_conversation);
    assert!(app.chat.session().unwrap().entries.is_empty());
    app.chat_action(ChatAction::CloseTab(saved.clone()));
    assert!(app.chat.sessions.contains_key(&saved));
    let log = std::fs::read_to_string(gates.path().join("events.jsonl")).unwrap();
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("new-session"))
            .count(),
        1,
        "stale actions must not create another chat"
    );
    app.chat_action(ChatAction::Disconnect);
}

#[test]
fn background_failures_and_finished_prompts_do_not_release_another_control_request() {
    for waiting in [
        Operation::NewSession,
        Operation::LoadSession,
        Operation::SetConfig,
    ] {
        for failed in [
            Operation::Prompt,
            Operation::Terminal,
            Operation::DeleteSession,
        ] {
            let mut app = App::test_new();
            let running = p::SessionId::new("background-response");
            let awaited = p::SessionId::new("awaited-history-or-settings");
            let target = if waiting == Operation::NewSession {
                None
            } else {
                Some(awaited.clone())
            };
            let mut session = Session::new(running.clone());
            session.begin_prompt("background request".into());
            app.chat.sessions.insert(running.clone(), session);
            app.chat.control_busy = true;
            app.chat.control_request = Some((waiting.clone(), target.clone()));
            app.chat.status = "Waiting for the foreground request".into();
            app.chat.opening_group = Some(0);
            if waiting == Operation::LoadSession {
                app.chat.loading_session = Some(awaited);
            }
            app.apply_acp_event(Event::Error {
                operation: failed,
                session_id: Some(running.clone()),
                error: p::Error::new(-32000, "Background request failed"),
            });
            app.apply_acp_event(Event::PromptFinished {
                session_id: running,
                response: p::PromptResponse::new(p::StopReason::EndTurn),
            });
            app.chat_action(ChatAction::New);
            assert!(app.chat.control_busy);
            assert_eq!(
                app.chat.control_request,
                Some((waiting.clone(), target.clone()))
            );
            assert_eq!(app.chat.status, "Waiting for the foreground request");
            assert_eq!(app.chat.opening_group, Some(0));
            app.apply_acp_event(Event::Error {
                operation: waiting.clone(),
                session_id: target,
                error: p::Error::new(-32000, "Foreground request failed"),
            });
            assert!(!app.chat.control_busy);
            assert!(app.chat.control_request.is_none());
        }
    }
}

#[test]
fn late_control_replies_are_bound_to_the_operation_and_session_that_started_them() {
    let mut app = App::test_new();
    let awaited = p::SessionId::new("model-change");
    let other = p::SessionId::new("other-chat");
    app.chat.control_busy = true;
    app.chat.control_request = Some((Operation::SetConfig, Some(awaited.clone())));
    app.chat.status = "Changing model…".into();
    app.apply_acp_event(Event::ConfigChanged {
        session_id: other,
        options: vec![],
    });
    app.apply_acp_event(Event::SessionsListed(p::ListSessionsResponse::new(vec![])));
    app.apply_acp_event(Event::Error {
        operation: Operation::SetMode,
        session_id: Some(awaited.clone()),
        error: p::Error::new(-32000, "A different request failed"),
    });
    assert!(app.chat.control_busy);
    assert_eq!(app.chat.status, "Changing model…");
    app.apply_acp_event(Event::ConfigChanged {
        session_id: awaited,
        options: vec![],
    });
    assert!(!app.chat.control_busy);
    assert!(app.chat.control_request.is_none());
    assert_eq!(app.chat.status, "Ready");
}

#[test]
fn late_session_replies_cannot_select_a_chat_or_consume_another_requests_destination() {
    let mut app = App::test_new();
    let current = p::SessionId::new("current-chat");
    let awaited = p::SessionId::new("awaited-chat");
    app.chat
        .sessions
        .insert(current.clone(), Session::new(current.clone()));
    app.chat.open_tab(current.clone());
    app.chat.control_busy = true;
    app.chat.control_request = Some((Operation::LoadSession, Some(awaited.clone())));
    app.chat.loading_session = Some(awaited.clone());
    app.chat.opening_group = Some(0);
    for (id, loaded) in [("obsolete-new-chat", false), ("obsolete-saved-chat", true)] {
        let id = p::SessionId::new(id);
        app.apply_acp_event(Event::SessionOpened {
            session_id: id.clone(),
            modes: None,
            config_options: vec![],
            loaded,
        });
        assert!(!app.chat.sessions.contains_key(&id));
        assert_eq!(app.chat.selected, Some(current.clone()));
        assert_eq!(app.chat.opening_group, Some(0));
        assert_eq!(app.chat.loading_session, Some(awaited.clone()));
        assert!(app.chat.control_busy);
    }
    app.chat.opening_group = None;
    app.apply_acp_event(Event::SessionOpened {
        session_id: awaited.clone(),
        modes: None,
        config_options: vec![],
        loaded: true,
    });
    assert_eq!(app.chat.selected, Some(awaited));
    assert!(app.chat.loading_session.is_none());
    assert!(!app.chat.control_busy);
}

#[test]
fn loading_view_tracks_the_request_even_when_a_background_action_finishes() {
    let mut app = App::test_new();
    let current = p::SessionId::new("current-chat");
    app.chat.selected = Some(current.clone());
    app.chat.control_busy = true;
    for operation in [
        Operation::NewSession,
        Operation::LoadSession,
        Operation::ResumeSession,
    ] {
        app.chat.control_request = Some((operation, Some(current.clone())));
        app.chat.status = "Unrelated status update".into();
        assert!(app.chat.loading_conversation());
        app.apply_acp_event(Event::SessionDeleted(p::SessionId::new(
            "obsolete-empty-chat",
        )));
        assert!(app.chat.loading_conversation());
        assert!(app.chat.control_busy);
        assert_eq!(app.chat.status, "Unrelated status update");
    }
    app.chat.control_request = Some((Operation::SetConfig, Some(current)));
    app.chat.status = "Loading conversation…".into();
    assert!(!app.chat.loading_conversation());
}

#[test]
fn stale_history_pagination_controls_cannot_start_duplicate_or_unrelated_requests() {
    let mut app = App::test_new();
    let id = p::SessionId::new("loading");
    app.chat.control_busy = true;
    app.chat.control_request = Some((Operation::LoadSession, Some(id.clone())));
    app.chat.next_cursor = Some("page-two".into());
    app.chat_action(ChatAction::MoreSessions);
    assert!(app.chat.control_busy);
    assert_eq!(
        app.chat.control_request,
        Some((Operation::LoadSession, Some(id)))
    );
    assert!(app.chat.error.is_none());
    app.chat.control_busy = false;
    app.chat.control_request = None;
    app.chat.next_cursor = None;
    app.chat_action(ChatAction::MoreSessions);
    assert!(app.chat.error.is_none(), "no next page must be a no-op");
}

#[test]
fn loaded_chats_with_empty_or_delayed_replay_are_preserved_when_the_tab_closes() {
    for supports_load in [false, true] {
        let mut app = App::test_new();
        let id = p::SessionId::new("saved-chat-without-replayed-messages");
        app.chat.initialized = Some(
            serde_json::from_value(serde_json::json!({
                "protocolVersion": 1,
                "agentCapabilities": {"loadSession": supports_load}
            }))
            .unwrap(),
        );
        app.apply_acp_event(Event::SessionOpened {
            session_id: id.clone(),
            modes: None,
            config_options: vec![],
            loaded: true,
        });
        assert!(app.chat.session().unwrap().has_conversation);
        assert!(!app.chat.empty_conversation(&id));
        assert!(!app.chat.history_filter.contains(&id));
        app.chat_action(ChatAction::CloseTab(id.clone()));
        assert!(app.chat.sessions.contains_key(&id));
        assert!(!app.chat.removed_sessions.contains(&id));
        app.chat_action(ChatAction::Switch(id.clone()));
        assert_eq!(app.chat.selected, Some(id));
    }
}

#[cfg(any(unix, windows))]
#[test]
fn new_buffers_and_pending_saves_through_directory_aliases_block_agent_writes() {
    let root = tempfile::tempdir().unwrap();
    let holder = tempfile::tempdir().unwrap();
    let link = holder.path().join("workspace-alias");
    #[cfg(windows)]
    assert!(
        std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(&link)
            .arg(root.path())
            .output()
            .unwrap()
            .status
            .success()
    );
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.path(), &link).unwrap();
    let workspace = vtcode::acp::Workspace::new(root.path()).unwrap();
    let requested = workspace.resolve(&root.path().join("new.rs")).unwrap();
    let mut app = App::test_new();
    let mut editor = crate::editor::Editor::test_with_text("unsaved text");
    editor.file_path = Some(link.join("new.rs"));
    editor.modified = true;
    app.tabs.push(crate::app::Tab::new_file(editor));
    let editors = app.acp_file_editors(&requested);
    let new_buffer_found = editors.len() == 1;
    let new_buffer_blocked = app.acp_write_blocked(&requested, &editors);
    app.tabs.clear();
    let existing = root.path().join("existing.rs");
    std::fs::write(&existing, "disk baseline").unwrap();
    for name in ["existing.rs", "another-new.rs"] {
        let target = workspace.resolve(&root.path().join(name)).unwrap();
        app.pending_file_saves.insert(link.join(name), (app.workspace_generation, 42.into()));
        let blocked = app.acp_write_blocked(&target, &app.acp_file_editors(&target));
        app.pending_file_saves.clear();
        assert!(blocked, "pending alias save for {name} was missed");
    }
    #[cfg(windows)]
    std::fs::remove_dir(&link).unwrap();
    #[cfg(unix)]
    std::fs::remove_file(&link).unwrap();
    assert!(new_buffer_found && new_buffer_blocked);
    assert!(!app.acp_write_blocked(&requested, &[]));
    assert_eq!(std::fs::read_to_string(existing).unwrap(), "disk baseline");
}
