use super::*;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn remote() -> (tempfile::TempDir, workspace::Workspace) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("agent.txt"), "disk one\ndisk two\n").unwrap();
    let workspace = workspace::Workspace::new(super::super::tests::connection(
        root.path(),
        Arc::default(),
        Arc::default(),
    ));
    (root, workspace)
}
pub(crate) fn config() -> acp::AgentConfig {
    let mut config = acp::AgentConfig::new(
        "Raw ACP peer",
        std::env::var("VTCODE_TEST_PYTHON").unwrap_or_else(|_| {
            if cfg!(windows) {
                "python".into()
            } else {
                "python3".into()
            }
        }),
    );
    config.args = vec![
        "-u".into(),
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/remote_agent_peer.py")
            .to_string_lossy()
            .into_owned(),
    ];
    config.env.insert("PYTHONUTF8".into(), "1".into());
    config
}
fn until(client: &acp::Client, predicate: impl Fn(&acp::Event) -> bool) -> Vec<acp::Event> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut events = Vec::new();
    loop {
        assert!(Instant::now() < deadline, "No agent progress: {events:?}");
        let event = client
            .recv_timeout(Duration::from_secs(15))
            .expect("agent timed out");
        let done = predicate(&event);
        if !done {
            assert!(
                !matches!(
                    &event,
                    acp::Event::Disconnected(_) | acp::Event::Error { .. }
                ),
                "{event:?}"
            );
        }
        events.push(event);
        if done {
            return events;
        }
    }
}
fn open(client: &acp::Client) -> p::SessionId {
    until(client, |event| matches!(event, acp::Event::Initialized(_)));
    client.send(acp::Command::NewSession).unwrap();
    let events = until(client, |event| {
        matches!(event, acp::Event::SessionOpened { .. })
    });
    let acp::Event::SessionOpened { session_id, .. } = events.last().unwrap() else {
        unreachable!()
    };
    session_id.clone()
}
fn prompt(client: &acp::Client, session: &p::SessionId, text: &str) {
    client
        .send(acp::Command::Prompt {
            session_id: session.clone(),
            content: vec![p::ContentBlock::Text(p::TextContent::new(text))],
        })
        .unwrap();
}
fn messages(events: &[acp::Event]) -> String {
    let mut session = acp::Session::new(p::SessionId::new("test"));
    for event in events {
        if let acp::Event::Update(update) = event {
            session.apply(update.update.clone());
        }
    }
    session
        .entries
        .iter()
        .filter_map(|entry| {
            if let acp::ChatEntry::Agent(text) = entry {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect()
}

#[cfg(windows)]
pub(crate) fn ssh_roundtrip(workspace: &workspace::Workspace) {
    let mut config = acp::AgentConfig::new("Node test peer", "python3");
    config.args = vec!["-u".into(), "agent-peer.py".into()];
    config
        .env
        .insert("VTCODE_TEST_EMBEDDED_CONTEXT".into(), "1".into());
    let client = connect(workspace.clone(), config, Default::default()).unwrap();
    let session = open(&client);
    prompt(&client, &session, "read");
    let event = until(&client, |event| {
        matches!(event, acp::Event::FileRequest { .. })
    })
    .pop()
    .unwrap();
    let acp::Event::FileRequest {
        id,
        ticket,
        request: acp::FileRequest::Read(request),
    } = event
    else {
        unreachable!()
    };
    assert_eq!(request.path, workspace.target_key("agent.txt").unwrap());
    let response = client
        .workspace()
        .for_request(id.clone(), ticket)
        .read(&request, Some("local one\r\nlocal unsaved λ\r\n"))
        .unwrap();
    client
        .send(acp::Command::FileResponse {
            id,
            ticket,
            result: Ok(serde_json::to_value(response).unwrap()),
            edit: None,
        })
        .unwrap();
    let events = until(&client, |event| {
        matches!(event, acp::Event::PromptFinished { .. })
    });
    assert!(messages(&events).contains("local unsaved"));
    prompt(&client, &session, "permission");
    let event = until(&client, |event| {
        matches!(event, acp::Event::Permission { .. })
    })
    .pop()
    .unwrap();
    let acp::Event::Permission {
        id,
        ticket,
        request,
    } = event
    else {
        unreachable!()
    };
    assert_eq!(
        request.tool_call.fields.locations.as_ref().unwrap()[0].path,
        workspace.target_key("agent.txt").unwrap()
    );
    client
        .send(acp::Command::Permission {
            id,
            ticket,
            option: Some(p::PermissionOptionId::new("deny")),
        })
        .unwrap();
    let events = until(&client, |event| {
        matches!(event, acp::Event::PromptFinished { .. })
    });
    assert!(messages(&events).contains("deny"));
    for embedded in [false, true] {
        let key = workspace.target_key("agent.txt").unwrap();
        let (_, source) = workspace
            .file_context(
                &[(key.clone(), key)],
                true,
                &crate::background::CancellationToken::new(),
            )
            .unwrap();
        let source = source.unwrap();
        client
            .send(acp::Command::Prompt {
                session_id: session.clone(),
                content: vec![
                    p::ContentBlock::Text(p::TextContent::new("inspect-context")),
                    source.reference(0, embedded, "agent.txt".into()),
                ],
            })
            .unwrap();
        drop(source);
        let events = until(&client, |event| {
            matches!(event, acp::Event::PromptFinished { .. })
        });
        let text = messages(&events);
        assert!(text.contains("disk one"), "{text}");
        assert!(
            !text.contains("vtcode-context:") && !text.contains("vtcode-remote-"),
            "{text}"
        );
        if embedded {
            assert!(text.contains("file://"), "{text}");
        }
    }
    prompt(&client, &session, "terminal");
    let events = until(&client, |event| {
        matches!(event, acp::Event::PromptFinished { .. })
    });
    let history = events
        .iter()
        .rev()
        .find_map(|event| match event {
            acp::Event::Terminal(terminal) => terminal.history.clone(),
            _ => None,
        })
        .unwrap();
    drop(events);
    drop(client);
    assert!(
        history
            .page(None, 0, 80, 10)
            .unwrap()
            .lines
            .iter()
            .any(|line| line.contains("history-0399 λ"))
    );
}

#[test]
fn rejected_agent_start_delivers_a_failure_and_leaves_the_workspace_connected() {
    let (_root, workspace) = remote();
    let client = connect(
        workspace.clone(),
        acp::AgentConfig::new("Missing", "vtcode-missing-remote-peer-37"),
        Default::default(),
    )
    .unwrap();
    let events = until(&client, |event| {
        matches!(event, acp::Event::Disconnected(_))
    });
    assert!(
        matches!(&events[0], acp::Event::Disconnected(message) if message.contains("vtcode-missing-remote-peer-37")),
        "{events:?}"
    );
    drop(client);
    workspace.client.call(super::super::Command::Ping).unwrap();
    workspace
        .client
        .call(super::super::Command::Shutdown)
        .unwrap();
}

#[test]
#[ignore = "raw ACP process integration; requires Python 3 (VTCODE_TEST_PYTHON)"]
fn headless_agent_prompts_stream_with_local_credits_and_large_input() {
    let (_root, workspace) = remote();
    let client = connect(workspace.clone(), config(), Default::default()).unwrap();
    let session = open(&client);
    let input = "λ line\n".repeat(100_000);
    prompt(&client, &session, &input);
    let events = until(&client, |event| {
        matches!(event, acp::Event::PromptFinished { .. })
    });
    assert!(messages(&events).starts_with(&format!("echo:{}:", input.chars().count())));
    prompt(&client, &session, "burst");
    // Stop draining temporarily. Credits bound memory; ordinary workspace work
    // and local input must not depend on the chat consumer making progress.
    std::thread::sleep(Duration::from_millis(80));
    workspace.client.call(super::super::Command::Ping).unwrap();
    let events = until(&client, |event| {
        matches!(event, acp::Event::PromptFinished { .. })
    });
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, acp::Event::Update(_)))
            .count(),
        400
    );
    drop(client);
    workspace
        .client
        .call(super::super::Command::Shutdown)
        .unwrap();
}

#[test]
#[ignore = "raw ACP process integration; requires Python 3 (VTCODE_TEST_PYTHON)"]
fn frozen_prompt_context_stays_on_node_and_survives_deleted_file_until_agent_accepts_it() {
    for embedded in [false, true] {
        let (root, workspace) = remote();
        let key = workspace.target_key("agent.txt").unwrap();
        let (_, source) = workspace
            .file_context(
                &[(key.clone(), key)],
                true,
                &crate::background::CancellationToken::new(),
            )
            .unwrap();
        let source = source.unwrap();
        let mut configuration = config();
        if embedded {
            configuration
                .env
                .insert("VTCODE_TEST_EMBEDDED_CONTEXT".into(), "1".into());
        }
        let client = connect(workspace.clone(), configuration, Default::default()).unwrap();
        let session = open(&client);
        std::fs::remove_file(root.path().join("agent.txt")).unwrap();
        client
            .send(acp::Command::Prompt {
                session_id: session,
                content: vec![
                    p::ContentBlock::Text(p::TextContent::new("inspect-context")),
                    source.reference(0, embedded, "agent.txt".into()),
                ],
            })
            .unwrap();
        drop(source);
        let events = until(&client, |event| {
            matches!(event, acp::Event::PromptFinished { .. })
        });
        let contents = messages(&events);
        assert!(contents.contains("disk one"), "{contents}");
        assert!(
            !contents.contains("vtcode-context:"),
            "References must never reach the agent: {contents}"
        );
        assert!(!contents.contains("vtcode-remote-"));
        if embedded {
            assert!(contents.contains("file://"));
        }
    }
}

#[test]
#[ignore = "raw ACP process integration; requires Python 3 (VTCODE_TEST_PYTHON)"]
fn cancelling_or_disconnecting_before_agent_start_releases_frozen_context_without_waiting_for_ssh()
{
    use std::sync::atomic::{AtomicBool, Ordering};
    for disconnect in [false, true] {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file"), "context").unwrap();
        let hold = Arc::new(AtomicBool::new(false));
        struct Release(Arc<AtomicBool>);
        impl Drop for Release {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _release = Release(hold.clone());
        let blocked = Arc::new(AtomicBool::new(false));
        let workspace = workspace::Workspace::new(super::super::tests::connection_with_status(
            root.path(),
            hold.clone(),
            Arc::default(),
            blocked.clone(),
        ));
        let key = workspace.target_key("file").unwrap();
        let (_, source) = workspace
            .file_context(
                &[(key.clone(), key)],
                true,
                &crate::background::CancellationToken::new(),
            )
            .unwrap();
        let source = source.unwrap();
        let weak = Arc::downgrade(&source);
        hold.store(true, Ordering::Release);
        let client = connect(workspace, config(), Default::default()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !blocked.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(blocked.load(Ordering::Acquire));
        let session = p::SessionId::new("not-started");
        client
            .send(acp::Command::Prompt {
                session_id: session.clone(),
                content: vec![source.reference(0, false, "file".into())],
            })
            .unwrap();
        drop(source);
        assert!(weak.upgrade().is_some());
        if disconnect {
            client.disconnect();
        } else {
            client.send(acp::Command::Cancel(session)).unwrap();
        }
        assert!(
            weak.upgrade().is_none(),
            "A cancelled queued prompt must release its frozen context immediately"
        );
        hold.store(false, Ordering::Release);
    }
}

fn reused_tickets(client: &acp::Client) {
    let session = open(client);
    prompt(client, &session, "permission");
    let first = until(client, |event| {
        matches!(event, acp::Event::Permission { .. })
    })
    .pop()
    .unwrap();
    let acp::Event::Permission {
        id: old_id,
        ticket: old_ticket,
        ..
    } = first
    else {
        unreachable!()
    };
    client.send(acp::Command::Cancel(session.clone())).unwrap();
    until(client, |event| {
        matches!(event, acp::Event::PromptFinished { .. })
    });
    prompt(client, &session, "permission");
    let next = until(client, |event| {
        matches!(event, acp::Event::Permission { .. })
    })
    .pop()
    .unwrap();
    let acp::Event::Permission { id, ticket, .. } = next else {
        unreachable!()
    };
    assert_eq!(id, old_id);
    assert!(ticket > old_ticket);
    client
        .send(acp::Command::Permission {
            id: old_id,
            ticket: old_ticket,
            option: Some(p::PermissionOptionId::new("allow")),
        })
        .unwrap();
    client
        .send(acp::Command::Permission {
            id,
            ticket,
            option: Some(p::PermissionOptionId::new("deny")),
        })
        .unwrap();
    let events = until(client, |event| {
        matches!(event, acp::Event::PromptFinished { .. })
    });
    assert!(messages(&events).contains("deny"), "{events:?}");
    assert!(!messages(&events).contains("allow"));
    prompt(client, &session, "wait-read");
    let first = until(client, |event| {
        matches!(event, acp::Event::FileRequest { .. })
    })
    .pop()
    .unwrap();
    let acp::Event::FileRequest {
        id: old_id,
        ticket: old_ticket,
        ..
    } = first
    else {
        unreachable!()
    };
    client.send(acp::Command::Cancel(session.clone())).unwrap();
    until(client, |event| {
        matches!(event, acp::Event::PromptFinished { .. })
    });
    prompt(client, &session, "wait-read");
    let next = until(client, |event| {
        matches!(event, acp::Event::FileRequest { .. })
    })
    .pop()
    .unwrap();
    let acp::Event::FileRequest { id, ticket, .. } = next else {
        unreachable!()
    };
    assert_eq!(id, old_id);
    assert!(ticket > old_ticket);
    client
        .send(acp::Command::FileResponse {
            id: old_id,
            ticket: old_ticket,
            result: Ok(serde_json::json!({"content":"stale"})),
            edit: None,
        })
        .unwrap();
    client
        .send(acp::Command::FileResponse {
            id,
            ticket,
            result: Ok(serde_json::json!({"content":"current"})),
            edit: None,
        })
        .unwrap();
    let events = until(client, |event| {
        matches!(event, acp::Event::PromptFinished { .. })
    });
    assert!(messages(&events).contains("current"), "{events:?}");
    assert!(!messages(&events).contains("stale"));
}
#[test]
#[ignore = "raw ACP process integration; requires Python 3 (VTCODE_TEST_PYTHON)"]
fn headless_agent_cancelled_file_and_permission_replies_cannot_answer_reused_ids() {
    let (_root, workspace) = remote();
    let client = connect(workspace.clone(), config(), Default::default()).unwrap();
    reused_tickets(&client);
    drop(client);
    workspace
        .client
        .call(super::super::Command::Shutdown)
        .unwrap();
}
#[test]
#[ignore = "raw ACP process integration; requires Python 3 (VTCODE_TEST_PYTHON)"]
fn native_agent_cancelled_file_and_permission_replies_cannot_answer_reused_ids() {
    let root = tempfile::tempdir().unwrap();
    let client = acp::Client::connect(config(), root.path(), || {}).unwrap();
    reused_tickets(&client);
}

#[test]
#[ignore = "raw ACP process integration; requires Python 3 (VTCODE_TEST_PYTHON)"]
fn headless_agent_file_requests_keep_disk_io_on_node_and_read_unsaved_local_text() {
    let (root, workspace) = remote();
    let client = connect(workspace.clone(), config(), Default::default()).unwrap();
    let session = open(&client);
    for buffer in [None, Some("unsaved one\r\nunsaved λ\r\n")] {
        prompt(&client, &session, "read");
        let event = until(&client, |event| {
            matches!(event, acp::Event::FileRequest { .. })
        })
        .pop()
        .unwrap();
        let acp::Event::FileRequest {
            id,
            ticket,
            request: acp::FileRequest::Read(request),
        } = event
        else {
            unreachable!()
        };
        assert_eq!(request.path, workspace.target_key("agent.txt").unwrap());
        let response = client
            .workspace()
            .for_request(id.clone(), ticket)
            .read(&request, buffer)
            .unwrap();
        if buffer.is_none() {
            assert!(response.content.is_empty(), "Disk text remains on the node");
        }
        client
            .send(acp::Command::FileResponse {
                id,
                ticket,
                result: Ok(serde_json::to_value(response).unwrap()),
                edit: None,
            })
            .unwrap();
        let events = until(&client, |event| {
            matches!(event, acp::Event::PromptFinished { .. })
        });
        assert!(
            messages(&events).contains(if buffer.is_some() {
                "unsaved"
            } else {
                "disk two"
            }),
            "{events:?}"
        );
    }
    prompt(&client, &session, "write");
    let event = until(&client, |event| {
        matches!(event, acp::Event::FileRequest { .. })
    })
    .pop()
    .unwrap();
    let acp::Event::FileRequest {
        id,
        ticket,
        request: acp::FileRequest::Write(request),
    } = event
    else {
        unreachable!()
    };
    assert!(
        request.content.is_empty(),
        "Agent write content must not make a round trip"
    );
    let edit = client
        .workspace()
        .for_request(id.clone(), ticket)
        .write(&request)
        .unwrap();
    client
        .send(acp::Command::FileResponse {
            id,
            ticket,
            result: Ok(serde_json::json!({})),
            edit: Some(edit),
        })
        .unwrap();
    let events = until(&client, |event| {
        matches!(event, acp::Event::PromptFinished { .. })
    });
    assert!(events.iter().any(|event| matches!(event, acp::Event::FileEdited(edit) if edit.before.contains("disk two") && edit.after == "node edit λ\nsecond\n")), "{events:?}");
    assert_eq!(
        std::fs::read_to_string(root.path().join("agent.txt")).unwrap(),
        "node edit λ\nsecond\n"
    );
    drop(client);
    workspace
        .client
        .call(super::super::Command::Shutdown)
        .unwrap();
}

#[test]
#[ignore = "raw ACP process integration; requires Python 3 (VTCODE_TEST_PYTHON)"]
fn headless_agent_terminal_history_survives_agent_disconnect_and_is_paged() {
    let (_root, workspace) = remote();
    let client = connect(workspace.clone(), config(), Default::default()).unwrap();
    let session = open(&client);
    prompt(&client, &session, "terminal");
    let events = until(&client, |event| {
        matches!(event, acp::Event::PromptFinished { .. })
    });
    let terminal = events
        .iter()
        .rev()
        .find_map(|event| {
            if let acp::Event::Terminal(terminal) = event {
                Some(terminal)
            } else {
                None
            }
        })
        .expect("terminal event");
    assert!(terminal.released);
    let history = terminal.history.as_ref().unwrap().clone();
    let page = history.page(Some(0), 0, 80, 10).unwrap();
    assert!(format!("{page:?}").contains("history-0000"), "{page:?}");
    drop(events);
    drop(client);
    let page = history.page(None, 0, 80, 10).unwrap();
    assert!(format!("{page:?}").contains("history-0399"), "{page:?}");
    drop(history);
    workspace
        .client
        .call(super::super::Command::Shutdown)
        .unwrap();
}

#[test]
#[ignore = "raw ACP process integration; requires Python 3 (VTCODE_TEST_PYTHON)"]
fn headless_agent_disconnect_unblocks_full_windows_without_draining_the_inbox() {
    let (_root, workspace) = remote();
    let mut disconnected = Vec::new();
    for _ in 0..12 {
        let client = connect(workspace.clone(), config(), Default::default()).unwrap();
        let session = open(&client);
        prompt(&client, &session, "burst");
        std::thread::sleep(Duration::from_millis(30));
        let start = Instant::now();
        client.disconnect();
        assert!(
            start.elapsed() < Duration::from_millis(50),
            "Disconnect waited on the remote event consumer"
        );
        disconnected.push(client);
        workspace.client.call(super::super::Command::Ping).unwrap();
    }
    let client = connect(workspace.clone(), config(), Default::default()).unwrap();
    let session = open(&client);
    prompt(&client, &session, "works after disconnect");
    let events = until(&client, |event| {
        matches!(event, acp::Event::PromptFinished { .. })
    });
    assert!(messages(&events).contains("works after disconnect"));
    drop(client);
    drop(disconnected);
    workspace
        .client
        .call(super::super::Command::Shutdown)
        .unwrap();
}

#[test]
#[ignore = "raw ACP process integration; requires Python 3 (VTCODE_TEST_PYTHON)"]
fn headless_agent_large_events_wait_for_shared_transfer_capacity_without_disconnect() {
    let (root, workspace) = remote();
    std::fs::write(root.path().join("bulk.txt"), "bulk λ\n".repeat(100_000)).unwrap();
    let client = connect(workspace.clone(), config(), Default::default()).unwrap();
    let session = open(&client);
    let mut transfers = Vec::new();
    for _ in 0..8 {
        let reply = workspace
            .client
            .call_file(super::super::Command::Open {
                path: RemotePath::new(b"bulk.txt".to_vec()).unwrap(),
            })
            .unwrap();
        let super::super::Reply::Text(text::Reply::Opened { transfer, .. }) = reply else {
            panic!("expected streamed open")
        };
        transfers.push(transfer);
    }
    prompt(&client, &session, "large-event");
    std::thread::sleep(Duration::from_millis(60));
    workspace.client.call(super::super::Command::Ping).unwrap();
    for transfer in transfers {
        workspace.client.close_text(transfer);
    }
    let events = until(&client, |event| {
        matches!(event, acp::Event::PromptFinished { .. })
    });
    let characters: usize = events
        .iter()
        .filter_map(|event| match event {
            acp::Event::Update(update) => match &update.update {
                p::SessionUpdate::AgentMessageChunk(chunk) => {
                    Some(acp::content_text(&chunk.content).chars().count())
                }
                _ => None,
            },
            _ => None,
        })
        .sum();
    assert_eq!(
        characters, 600_006,
        "Test the relay contents independently of chat-view truncation"
    );
    drop(client);
    workspace
        .client
        .call(super::super::Command::Shutdown)
        .unwrap();
}
