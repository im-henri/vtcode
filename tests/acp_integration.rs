//! External-peer integration tests use the official Python SDK.
//! See tests/fixtures/acp-python-sdk/README.md for setup; no credentials or service are required.
use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use vtcode::acp::{
    AgentConfig, ChatEntry, Client, Command, Event, FileRequest, Operation, Session, protocol as p,
};

fn python() -> PathBuf {
    std::env::var_os("VTCODE_ACP_TEST_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join(if cfg!(windows) {
                "target/acp-python/Scripts/python.exe"
            } else {
                "target/acp-python/bin/python"
            })
        })
}
fn config(script: &str) -> AgentConfig {
    assert!(
        python().is_file(),
        "Install the official Python SDK and set VTCODE_ACP_TEST_PYTHON; see tests/fixtures/acp-python-sdk/README.md"
    );
    let mut config = AgentConfig::new("External SDK test peer", python().to_string_lossy());
    config.args = vec![
        "-u".into(),
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(script)
            .to_string_lossy()
            .into_owned(),
    ];
    config.env.insert("PYTHONUTF8".into(), "1".into());
    config.env.insert("PYTHONIOENCODING".into(), "utf-8".into());
    // SDK imports can take several seconds on Windows, especially on a cold
    // filesystem cache. Timeout-specific tests override this startup budget.
    config.request_timeout_secs = 15;
    config
}
fn next(client: &Client) -> Event {
    client
        .recv_timeout(Duration::from_secs(20))
        .expect("ACP peer made no progress")
}
fn until(client: &Client, predicate: impl Fn(&Event) -> bool) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut events = Vec::new();
    loop {
        assert!(Instant::now() < deadline, "Timed out: {events:?}");
        let event = next(client);
        let done = predicate(&event);
        if !done {
            assert!(
                !matches!(&event, Event::Disconnected(_) | Event::Error { .. }),
                "Unexpected event: {event:?}"
            );
        }
        events.push(event);
        if done {
            return events;
        }
    }
}
fn open(client: &Client) -> p::SessionId {
    client.send(Command::NewSession).unwrap();
    let events = until(client, |e| matches!(e, Event::SessionOpened { .. }));
    match events.last().unwrap() {
        Event::SessionOpened { session_id, .. } => session_id.clone(),
        _ => unreachable!(),
    }
}
fn prompt(client: &Client, id: &p::SessionId, text: &str) {
    client
        .send(Command::Prompt {
            session_id: id.clone(),
            content: vec![p::ContentBlock::Text(p::TextContent::new(text))],
        })
        .unwrap();
}
fn connect(config: AgentConfig, root: &Path) -> Client {
    let client = Client::connect(config, root, || {}).unwrap();
    until(&client, |e| matches!(e, Event::Initialized(_)));
    client
}

#[test]
fn missing_agent_reports_the_executable_and_a_settings_fix() {
    let root = tempfile::tempdir().unwrap();
    let client = Client::connect(
        AgentConfig::new("Missing test agent", "vtcode-nonexistent-acp-launcher-37"),
        root.path(),
        || {},
    )
    .unwrap();
    let Event::Disconnected(message) = next(&client) else {
        panic!("expected a startup error")
    };
    assert!(
        message.contains("vtcode-nonexistent-acp-launcher-37"),
        "{message}"
    );
    assert!(message.contains("Settings > Agent Chat"), "{message}");
    assert!(message.contains("PATH"), "{message}");
}

#[test]
#[cfg(windows)]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn official_echo_starts_through_windows_cmd_and_bat_launchers() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("launcher folder");
    std::fs::create_dir(&folder).unwrap();
    // Match npm's real installation layout: its extensionless Unix shim must
    // not shadow the Windows .cmd/.bat launcher for a bare command name.
    std::fs::write(folder.join("sdk-echo"), "#!/bin/sh\nexit 99\n").unwrap();
    std::fs::write(folder.join("sdk-echo.ps1"), "exit 99\n").unwrap();
    for extension in ["cmd", "bat"] {
        let launcher = folder.join(format!("sdk-echo.{extension}"));
        std::fs::write(
            &launcher,
            format!("@echo off\r\n\"{}\" %*\r\n", python().display()),
        )
        .unwrap();
        for name in [
            "sdk-echo".into(),
            format!("launcher folder/sdk-echo.{extension}"),
            launcher.to_string_lossy().into_owned(),
        ] {
            let mut cfg = config("acp-python-sdk/echo_agent.py");
            cfg.command = name;
            cfg.env
                .insert("Path".into(), folder.to_string_lossy().into_owned());
            cfg.env
                .insert("PATHEXT".into(), format!(".{}", extension.to_uppercase()));
            let client = connect(cfg, root.path());
            let id = open(&client);
            prompt(&client, &id, "Windows launcher round trip 世界 👋");
            let events = until(&client, |e| matches!(e, Event::PromptFinished { .. }));
            assert!(events.iter().any(|e| matches!(e, Event::Update(n)
                if matches!(&n.update, p::SessionUpdate::AgentMessageChunk(chunk)
                    if matches!(&chunk.content, p::ContentBlock::Text(text) if text.text == "Windows launcher round trip 世界 👋")))));
        }
    }
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn official_unmodified_echo_streams_unicode_and_metadata_over_stdio() {
    let root = tempfile::tempdir().unwrap();
    let client = connect(config("acp-python-sdk/echo_agent.py"), root.path());
    let id = open(&client);
    client
        .send(Command::Prompt {
            session_id: id.clone(),
            content: ["Hello, ", "世界 👋\n", "second block"]
                .into_iter()
                .map(|text| p::ContentBlock::Text(p::TextContent::new(text)))
                .collect(),
        })
        .unwrap();
    let events = until(&client, |e| matches!(e, Event::PromptFinished { .. }));
    let mut session = Session::new(id);
    let mut chunks = 0;
    for event in events {
        if let Event::Update(notification) = event {
            if let p::SessionUpdate::AgentMessageChunk(chunk) = &notification.update {
                assert_eq!(chunk.meta.as_ref().unwrap()["echo"], true);
                chunks += 1;
            }
            session.apply(notification.update);
        }
    }
    assert_eq!(chunks, 3);
    assert!(
        matches!(&session.entries[0], ChatEntry::Agent(text) if text == "Hello, 世界 👋\nsecond block")
    );
}

#[test]
#[cfg(unix)]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn official_echo_starts_through_unix_path_and_relative_launchers() {
    use std::os::unix::fs::PermissionsExt;

    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("launcher folder");
    std::fs::create_dir(&folder).unwrap();
    let launcher = folder.join("sdk-echo");
    let quoted_python = python().to_string_lossy().replace('\'', "'\\''");
    std::fs::write(
        &launcher,
        format!("#!/bin/sh\nexec '{quoted_python}' \"$@\"\n"),
    )
    .unwrap();
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths(std::iter::once(folder.clone()).chain(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    )))
    .unwrap();
    for name in [
        "sdk-echo".into(),
        "launcher folder/sdk-echo".into(),
        launcher.to_string_lossy().into_owned(),
    ] {
        let mut cfg = config("acp-python-sdk/echo_agent.py");
        cfg.command = name;
        cfg.env
            .insert("PATH".into(), path.to_string_lossy().into_owned());
        let client = connect(cfg, root.path());
        let id = open(&client);
        prompt(&client, &id, "Unix launcher round trip 世界 👋");
        let events = until(&client, |event| {
            matches!(event, Event::PromptFinished { .. })
        });
        assert!(events.iter().any(|event| matches!(event, Event::Update(notification)
            if matches!(&notification.update, p::SessionUpdate::AgentMessageChunk(chunk)
                if matches!(&chunk.content, p::ContentBlock::Text(text) if text.text == "Unix launcher round trip 世界 👋")))));
    }
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn sdk_modes_config_auth_pagination_and_history_are_discovered() {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = config("scenario_agent.py");
    cfg.env
        .insert("ACP_TEST_MODE_A".into(), "custom-explore-23".into());
    cfg.env
        .insert("ACP_TEST_MODE_B".into(), "custom-work-87".into());
    cfg.env.insert("ACP_TEST_REQUIRE_AUTH".into(), "1".into());
    let client = connect(cfg, root.path());
    client.send(Command::NewSession).unwrap();
    until(&client, |e| {
        matches!(
            e,
            Event::Error {
                operation: Operation::NewSession,
                ..
            }
        )
    });
    client
        .send(Command::Authenticate(p::AuthMethodId::new(
            "local-handshake",
        )))
        .unwrap();
    until(&client, |e| matches!(e, Event::Authenticated));
    client.send(Command::NewSession).unwrap();
    let events = until(&client, |e| matches!(e, Event::SessionOpened { .. }));
    let Event::SessionOpened {
        session_id,
        modes,
        config_options,
        ..
    } = events.last().unwrap()
    else {
        unreachable!()
    };
    assert_eq!(
        modes.as_ref().unwrap().available_modes[1].id.to_string(),
        "custom-work-87"
    );
    assert_eq!(config_options[0].id.to_string(), "detail-level");
    client
        .send(Command::SetMode {
            session_id: session_id.clone(),
            mode_id: modes.as_ref().unwrap().available_modes[1].id.clone(),
        })
        .unwrap();
    until(
        &client,
        |e| matches!(e, Event::ModeChanged { mode_id, .. } if mode_id.to_string() == "custom-work-87"),
    );
    client
        .send(Command::SetConfig {
            session_id: session_id.clone(),
            config_id: p::SessionConfigId::new("detail-level"),
            value: p::SessionConfigOptionValue::ValueId {
                value: p::SessionConfigValueId::new("full"),
            },
        })
        .unwrap();
    until(&client, |e| matches!(e, Event::ConfigChanged { .. }));
    client.send(Command::ListSessions { cursor: None }).unwrap();
    let events = until(&client, |e| matches!(e, Event::SessionsListed(_)));
    let Event::SessionsListed(list) = events.last().unwrap() else {
        unreachable!()
    };
    assert_eq!(list.next_cursor.as_deref(), Some("page-2"));
    let history_id = list.sessions[0].session_id.clone();
    client
        .send(Command::ListSessions {
            cursor: list.next_cursor.clone(),
        })
        .unwrap();
    until(
        &client,
        |e| matches!(e, Event::SessionsListed(list) if list.next_cursor.is_none()),
    );
    client
        .send(Command::LoadSession(history_id.clone()))
        .unwrap();
    let events = until(&client, |e| {
        matches!(e, Event::SessionOpened { loaded: true, .. })
    });
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Update(n) if n.session_id == history_id))
    );
    client.send(Command::CloseSession(history_id)).unwrap();
    until(&client, |e| matches!(e, Event::SessionClosed(_)));
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn sdk_history_delete_uses_advertised_capability_without_loading_history() {
    let root = tempfile::tempdir().unwrap();
    let client = connect(config("scenario_agent.py"), root.path());
    let id = p::SessionId::new("history-a");
    for _ in 0..2 {
        client.send(Command::DeleteSession(id.clone())).unwrap();
        until(
            &client,
            |e| matches!(e, Event::SessionDeleted(deleted) if deleted == &id),
        );
    }
    client.send(Command::ListSessions { cursor: None }).unwrap();
    let events = until(&client, |e| matches!(e, Event::SessionsListed(_)));
    let Event::SessionsListed(list) = events.last().unwrap() else {
        unreachable!()
    };
    assert!(list.sessions.iter().all(|s| s.session_id != id));
    drop(client);

    let mut peer_config = config("scenario_agent.py");
    peer_config
        .env
        .insert("ACP_TEST_NO_DELETE".into(), "1".into());
    let client = connect(peer_config, root.path());
    client.send(Command::DeleteSession(id)).unwrap();
    let events = until(&client, |e| {
        matches!(
            e,
            Event::Error {
                operation: Operation::DeleteSession,
                ..
            }
        )
    });
    let Event::Error { error, .. } = events.last().unwrap() else {
        unreachable!()
    };
    assert!(error.message.contains("does not support deleting"));
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn sdk_permissions_edits_plans_tools_and_bounded_terminals() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("context.txt"), "disk one\ndisk two\n").unwrap();
    std::fs::write(root.path().join("generated.txt"), "before\n").unwrap();
    let client = connect(config("scenario_agent.py"), root.path());
    let id = open(&client);
    let mut session = Session::new(id.clone());
    prompt(&client, &id, "exercise");
    let mut permissions = 0;
    let mut edits = 0;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline);
        match next(&client) {
            Event::Permission { id, ticket, request } => {
                permissions += 1;
                assert_eq!(request.options[0].option_id.to_string(), "permit-local-37");
                client
                    .send(Command::Permission {
                        id,
                        ticket,
                        option: Some(request.options[0].option_id.clone()),
                    })
                    .unwrap();
            }
            Event::FileRequest { id, ticket, request } => {
                let (result, edit) = match request {
                    FileRequest::Read(request) => (
                        Ok(serde_json::to_value(
                            client
                                .workspace()
                                .read(&request, Some("unsaved one\r\nunsaved two\r\n"))
                                .unwrap(),
                        )
                        .unwrap()),
                        None,
                    ),
                    FileRequest::Write(request) => {
                        let edit = client.workspace().write(&request).unwrap();
                        (Ok(serde_json::json!({})), Some(edit))
                    }
                };
                client
                    .send(Command::FileResponse { id, ticket, result, edit })
                    .unwrap();
            }
            Event::FileEdited(edit) => {
                edits += 1;
                assert_eq!(edit.before, "before\n");
            }
            Event::Update(notification) => session.apply(notification.update),
            Event::Terminal(snapshot) => session.terminal(snapshot),
            Event::PromptFinished { response, .. } => {
                assert_eq!(response.stop_reason, p::StopReason::EndTurn);
                break;
            }
            Event::Error { error, .. } => panic!("SDK scenario error: {error:?}"),
            Event::Disconnected(reason) => panic!("Peer disconnected: {reason}"),
            _ => {}
        }
    }
    assert_eq!((permissions, edits), (1, 1));
    assert_eq!(
        std::fs::read_to_string(root.path().join("generated.txt")).unwrap(),
        "after\n"
    );
    assert!(
        session
            .entries
            .iter()
            .any(|e| matches!(e, ChatEntry::Agent(text) if text.contains("unsaved two\r\n")))
    );
    assert!(session.entries.iter().any(|e| matches!(e, ChatEntry::Tool(t) if t.tool_call_id.to_string().starts_with("edit-") && t.status == p::ToolCallStatus::Completed && !t.content.is_empty())));
    assert!(session.entries.iter().any(|e| matches!(e, ChatEntry::Plan(plan) if plan.entries.iter().all(|e| e.status == p::PlanEntryStatus::Completed))));
    let terminal = session.terminals.values().next().unwrap();
    assert!(terminal.released && terminal.truncated && terminal.output.len() <= 96);
    assert_eq!(terminal.exit_status.as_ref().unwrap().exit_code, Some(0));
    assert_eq!(session.commands[0].name, "inspect-local");
    assert_eq!(session.usage.as_ref().unwrap().used, 256);
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn sdk_cancellation_rejection_and_errors_do_not_poison_the_connection() {
    let root = tempfile::tempdir().unwrap();
    let client = connect(config("scenario_agent.py"), root.path());
    check_cancellation(&client);
    check_rejection_and_errors(&client, root.path());
}

fn check_cancellation(client: &Client) {
    let id = open(client);
    prompt(client, &id, "permission");
    let events = until(client, |e| matches!(e, Event::Permission { .. }));
    let Event::Permission {
        id: permission_id, ..
    } = events.last().unwrap()
    else {
        unreachable!()
    };
    client.send(Command::Cancel(id.clone())).unwrap();
    let events = until(client, |e| matches!(e, Event::PromptFinished { .. }));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::PermissionResolved(id, _) if id == permission_id))
    );
    assert!(
        matches!(events.last().unwrap(), Event::PromptFinished { response, .. } if response.stop_reason == p::StopReason::Cancelled)
    );
    prompt(client, &id, "cancel");
    until(client, |e| matches!(e, Event::Update(_)));
    client.send(Command::Cancel(id.clone())).unwrap();
    until(
        client,
        |e| matches!(e, Event::PromptFinished { response, .. } if response.stop_reason == p::StopReason::Cancelled),
    );
    prompt(client, &id, "works again");
    until(client, |e| matches!(e, Event::PromptFinished { .. }));
    // A Stop queued immediately after completion must not poison the next turn.
    client.send(Command::Cancel(id.clone())).unwrap();
    prompt(client, &id, "permission");
    let events = until(client, |e| matches!(e, Event::Permission { .. }));
    let Event::Permission { id, ticket, .. } = events.last().unwrap() else {
        unreachable!()
    };
    client
        .send(Command::Permission {
            id: id.clone(),
            ticket: *ticket,
            option: None,
        })
        .unwrap();
    until(client, |e| matches!(e, Event::PromptFinished { .. }));
}

fn check_rejection_and_errors(client: &Client, root: &Path) {
    let id = open(client);
    prompt(client, &id, "permission");
    let events = until(client, |e| matches!(e, Event::Permission { .. }));
    let Event::Permission {
        id: request_id,
        ticket,
        request,
    } = events.last().unwrap()
    else {
        unreachable!()
    };
    client
        .send(Command::Permission {
            id: request_id.clone(),
            ticket: *ticket,
            option: Some(request.options[1].option_id.clone()),
        })
        .unwrap();
    until(client, |e| matches!(e, Event::PromptFinished { .. }));
    prompt(client, &id, "error");
    until(
        client,
        |e| matches!(e, Event::Error { error, .. } if error.code == p::ErrorCode::Other(-32042)),
    );
    prompt(client, &id, "unknown-method");
    let events = until(client, |e| matches!(e, Event::PromptFinished { .. }));
    assert!(events.iter().any(|e| matches!(e, Event::Update(n) if matches!(&n.update, p::SessionUpdate::AgentMessageChunk(c) if matches!(&c.content, p::ContentBlock::Text(t) if t.text.contains("-32601"))))));
    std::fs::write(root.join("context.txt"), "\"".repeat(5 * 1024 * 1024)).unwrap();
    prompt(client, &id, "large-file");
    let events = until(client, |e| matches!(e, Event::FileRequest { .. }));
    let Event::FileRequest {
        id: request_id,
        ticket,
        request: FileRequest::Read(request),
    } = events.last().unwrap()
    else {
        unreachable!()
    };
    let response = client.workspace().read(request, None).unwrap();
    client
        .send(Command::FileResponse {
            id: request_id.clone(),
            ticket: *ticket,
            result: Ok(serde_json::to_value(response).unwrap()),
            edit: None,
        })
        .unwrap();
    until(client, |e| matches!(e, Event::PromptFinished { .. }));
    prompt(client, &id, "still connected");
    until(client, |e| matches!(e, Event::PromptFinished { .. }));
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn sdk_terminal_input_kill_and_release_are_live_during_prompts() {
    let root = tempfile::tempdir().unwrap();
    let client = connect(config("scenario_agent.py"), root.path());
    let id = open(&client);
    prompt(&client, &id, "terminal-input");
    let events = until(
        &client,
        |e| matches!(e, Event::Terminal(t) if t.output.contains("ready")),
    );
    let Event::Terminal(terminal) = events.last().unwrap() else {
        unreachable!()
    };
    client
        .send(Command::TerminalInput {
            session_id: id.clone(),
            terminal_id: terminal.id.clone(),
            text: "from the client\n".into(),
        })
        .unwrap();
    let events = until(&client, |e| matches!(e, Event::PromptFinished { .. }));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Terminal(t) if t.output.contains("input=from the client")))
    );
    for command in [
        "terminal-kill",
        "terminal-release",
        "terminal-descendant",
        "terminal-failure",
    ] {
        prompt(&client, &id, command);
        until(&client, |e| matches!(e, Event::PromptFinished { .. }));
    }
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn sdk_disconnect_bad_frames_and_bad_versions_fail_promptly() {
    let root = tempfile::tempdir().unwrap();
    for text in ["crash", "bad-json", "partial-json"] {
        let client = connect(config("scenario_agent.py"), root.path());
        let id = open(&client);
        prompt(&client, &id, text);
        let mut disconnected = false;
        for _ in 0..8 {
            if matches!(next(&client), Event::Disconnected(_)) {
                disconnected = true;
                break;
            }
        }
        assert!(disconnected, "{text} did not disconnect");
    }
    let mut cfg = config("scenario_agent.py");
    cfg.env.insert("ACP_TEST_BAD_VERSION".into(), "1".into());
    let client = Client::connect(cfg, root.path(), || {}).unwrap();
    assert!(matches!(
        next(&client),
        Event::Error {
            operation: Operation::Initialize,
            ..
        }
    ));
    assert!(matches!(next(&client), Event::Disconnected(_)));
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn sdk_initialization_and_cancel_timeouts_clean_up() {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = config("scenario_agent.py");
    cfg.request_timeout_secs = 1;
    cfg.env.insert("ACP_TEST_INIT_HANG".into(), "1".into());
    let client = Client::connect(cfg, root.path(), || {}).unwrap();
    assert!(matches!(
        next(&client),
        Event::Error {
            operation: Operation::Initialize,
            ..
        }
    ));
    assert!(matches!(next(&client), Event::Disconnected(_)));
    let mut cfg = config("scenario_agent.py");
    // Normal Python SDK startup can take over a second. Keep its usual budget;
    // an ignored cancellation must still disconnect within the five-second cap.
    cfg.env.insert("ACP_TEST_IGNORE_CANCEL".into(), "1".into());
    let client = connect(cfg, root.path());
    let id = open(&client);
    prompt(&client, &id, "cancel");
    until(&client, |e| matches!(e, Event::Update(_)));
    client.send(Command::Cancel(id)).unwrap();
    until(&client, |e| {
        matches!(
            e,
            Event::Error {
                operation: Operation::Prompt,
                ..
            }
        )
    });
    assert!(matches!(next(&client), Event::Disconnected(_)));
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn sdk_early_session_updates_and_resume_without_replay() {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = config("scenario_agent.py");
    cfg.env.insert("ACP_TEST_RESUME_ONLY".into(), "1".into());
    cfg.env.insert("ACP_TEST_EARLY_UPDATES".into(), "1".into());
    let client = connect(cfg, root.path());
    let id = open(&client);
    assert!(matches!(next(&client), Event::Update(n) if n.session_id == id));
    client
        .send(Command::LoadSession(p::SessionId::new("saved")))
        .unwrap();
    until(&client, |e| {
        matches!(
            e,
            Event::Error {
                operation: Operation::LoadSession,
                ..
            }
        )
    });
    client
        .send(Command::ResumeSession(p::SessionId::new("saved")))
        .unwrap();
    until(
        &client,
        |e| matches!(e, Event::SessionOpened { session_id, loaded: true, .. } if session_id.to_string() == "saved"),
    );
    prompt(&client, &p::SessionId::new("saved"), "resumed");
    until(&client, |e| matches!(e, Event::PromptFinished { .. }));
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn disconnect_terminates_process_tree_even_when_event_queue_is_full() {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = config("scenario_agent.py");
    let pid_file = root.path().join("agent.pid");
    let child_file = root.path().join("descendant.pid");
    cfg.env.insert(
        "ACP_TEST_PID_FILE".into(),
        pid_file.to_string_lossy().into_owned(),
    );
    cfg.env.insert(
        "ACP_TEST_CHILD_PID_FILE".into(),
        child_file.to_string_lossy().into_owned(),
    );
    let (wake, enqueued) = std::sync::mpsc::channel();
    let client = Client::connect(cfg, root.path(), move || {
        let _ = wake.send(());
    })
    .unwrap();
    let initialized = until(&client, |event| matches!(event, Event::Initialized(_)));
    let pids: Vec<u32> = [&pid_file, &child_file]
        .into_iter()
        .map(|p| std::fs::read_to_string(p).unwrap().parse().unwrap())
        .collect();
    assert!(pids.iter().all(|&pid| process_alive(pid)));
    client.send(Command::NewSession).unwrap();
    let opened = until(&client, |event| {
        matches!(event, Event::SessionOpened { .. })
    });
    let Event::SessionOpened { session_id: id, .. } = opened.last().unwrap() else {
        unreachable!()
    };
    // Each callback follows one successful UI-event enqueue. Account for the
    // events already consumed, including callbacks racing their reception.
    for _ in 0..initialized.len() + opened.len() {
        enqueued.recv_timeout(Duration::from_secs(20)).unwrap();
    }
    prompt(&client, id, "flood");
    // Client::connect buffers 256 UI events. Stop draining and wait until that
    // queue is actually full; shutdown must not depend on consuming updates.
    let deadline = Instant::now() + Duration::from_secs(20);
    for _ in 0..256 {
        enqueued
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("the SDK peer did not fill the UI event queue");
    }
    client.disconnect();
    let deadline = Instant::now() + Duration::from_secs(5);
    while pids.iter().any(|&pid| process_alive(pid)) {
        assert!(
            Instant::now() < deadline,
            "ACP process tree survived disconnect"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(client);
}

fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::{Foundation::CloseHandle, System::Threading::*};
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let mut code = 0;
        let alive = GetExitCodeProcess(process, &mut code) != 0 && code == 259;
        CloseHandle(process);
        alive
    }
    #[cfg(unix)]
    {
        // A reaped parent can leave a zombie until init adopts it on Linux;
        // zombies no longer execute or retain pipe handles.
        #[cfg(target_os = "linux")]
        if std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| {
            s.rsplit_once(')')
                .is_some_and(|(_, fields)| fields.trim_start().starts_with('Z'))
        }) {
            return false;
        }
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
}
