use super::*;

fn setup() -> (App, p::SessionId) {
    let mut app = App::test_new();
    let id = p::SessionId::new("queued-session");
    app.chat.selected = Some(id.clone());
    app.chat
        .sessions
        .insert(id.clone(), Session::new(id.clone()));
    app.chat.session_order.push(id.clone());
    app.chat.queues.insert(
        id.clone(),
        PromptQueue {
            items: (1..=3).map(prompt).collect(),
            ..Default::default()
        },
    );
    (app, id)
}
fn prompt(id: u64) -> QueuedPrompt {
    QueuedPrompt {
        id,
        text: format!("message {id}"),
        files: vec![PathBuf::from(format!("file-{id}.rs"))],
        paste: None,
    }
}

#[test]
fn checkpoint_failure_retains_unsent_text_and_attachments_and_pauses_queue() {
    let (mut app, id) = setup();
    let queue = app.chat.queues.get_mut(&id).unwrap();
    queue.inflight = queue.items.pop_front();
    app.queued_checkpoint_finished(&id, false, false);
    let queue = &app.chat.queues[&id];
    assert!(queue.paused);
    assert_eq!(queue.items.len(), 3);
    assert_eq!(queue.items[0].text, "message 1");
    assert_eq!(queue.items[0].files, vec![PathBuf::from("file-1.rs")]);
    assert!(queue.inflight.is_none());
}

#[test]
fn cancelled_checkpoint_keeps_forced_successor_first_and_retains_unsent_prompt() {
    let (mut app, id) = setup();
    let queue = app.chat.queues.get_mut(&id).unwrap();
    queue.inflight = queue.items.pop_front();
    queue.priority = Some(2);
    app.queued_checkpoint_finished(&id, false, true);
    let queue = &app.chat.queues[&id];
    assert!(!queue.paused);
    assert_eq!(
        queue.items.iter().map(|p| p.id).collect::<Vec<_>>(),
        vec![2, 1, 3]
    );
    assert_eq!(queue.priority, Some(2));
    let queue = app.chat.queues.get_mut(&id).unwrap();
    queue.inflight = queue.items.pop_front();
    app.queued_checkpoint_finished(&id, true, false);
    assert!(
        app.chat.queues[&id].paused,
        "The cancelled, unsent prompt requires an explicit resume"
    );
    assert_eq!(app.chat.queues[&id].items[0].id, 1);
}

#[test]
fn queue_edit_cannot_replace_draft_or_terminal_input_and_keeps_other_sessions_untouched() {
    let (mut app, id) = setup();
    let other = p::SessionId::new("other");
    app.chat.queues.insert(
        other.clone(),
        PromptQueue {
            items: vec![prompt(10)].into(),
            ..Default::default()
        },
    );
    app.chat.input = "My unsent draft".into();
    app.chat_action(ChatAction::EditQueued(2));
    assert_eq!(app.chat.input, "My unsent draft");
    assert_eq!(app.chat.queues[&id].items.len(), 3);
    app.chat.input.clear();
    app.chat.terminal_input = Some("terminal".into());
    app.chat_action(ChatAction::EditQueued(2));
    assert!(app.chat.input.is_empty());
    app.chat.terminal_input = None;
    app.chat_action(ChatAction::EditQueued(2));
    assert_eq!(app.chat.input, "message 2");
    assert_eq!(app.chat.files.attached, vec![PathBuf::from("file-2.rs")]);
    assert_eq!(app.chat.queues[&id].items.len(), 2);
    assert_eq!(app.chat.queues[&other].items[0].id, 10);
    assert!(
        app.chat
            .history_delete_unavailable(&id)
            .unwrap()
            .contains("queued")
    );
}

#[test]
fn stop_and_disconnect_pause_queues_without_discarding_them() {
    let (mut app, id) = setup();
    app.chat_action(ChatAction::Stop);
    assert!(app.chat.queues[&id].paused);
    assert_eq!(app.chat.queues[&id].items.len(), 3);
    app.chat.queues.get_mut(&id).unwrap().paused = false;
    app.apply_acp_event(Event::Disconnected("Test disconnect".into()));
    assert!(app.chat.queues[&id].paused);
    assert_eq!(app.chat.queues[&id].items.len(), 3);
    assert!(!app.chat.empty_conversation(&id));
}

#[test]
fn queue_priority_is_cleared_only_after_actual_send_and_removal_cannot_send_another_item() {
    let (mut app, id) = setup();
    let queue = app.chat.queues.get_mut(&id).unwrap();
    queue.priority = Some(1);
    queue.inflight = queue.items.pop_front();
    app.queued_checkpoint_finished(&id, true, false);
    assert_eq!(app.chat.queues[&id].priority, None);
    assert_eq!(app.chat.queues[&id].items.len(), 2);
    app.chat.queues.get_mut(&id).unwrap().priority = Some(2);
    app.chat_action(ChatAction::RemoveQueued(2));
    assert!(app.chat.queues[&id].paused);
    assert_eq!(app.chat.queues[&id].items[0].id, 3);
    assert_eq!(app.chat.queues[&id].priority, None);
}

fn wait(app: &mut App, ready: impl Fn(&App) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        app.poll_acp();
        assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
        if ready(app) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Timed out: {}",
            app.chat.status
        );
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn acp_chat_sdk_round_trip_queue_cancellation_fifo_and_message_checkpoints() {
    use vtcode::acp::rollback::Selection;
    for git in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        if git {
            git2::Repository::init(root.path()).unwrap();
        }
        let log = data.path().join("queue.log");
        let mut app = App::test_new();
        app.explorer.root = root.path().into();
        let python = std::env::var("VTCODE_ACP_TEST_PYTHON").expect("set VTCODE_ACP_TEST_PYTHON; see tests/fixtures/acp-python-sdk/README.md");
        let mut config = AgentConfig::new("Queue SDK", python);
        config.args = vec![
            "-u".into(),
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/scenario_agent.py")
                .to_string_lossy()
                .into_owned(),
        ];
        config.env.insert(
            "ACP_TEST_QUEUE_LOG".into(),
            log.to_string_lossy().into_owned(),
        );
        app.settings_effective.acp_agents = Some(vec![config]);
        app.chat_action(ChatAction::Connect(0));
        wait(&mut app, |a| {
            a.chat.selected.is_some() && !a.chat.rollback.busy()
        });
        let id = app.chat.selected.clone().unwrap();
        app.chat.input = "silent".into();
        app.chat_action(ChatAction::Send);
        wait(&mut app, |a| {
            a.chat
                .diagnostics
                .iter()
                .any(|line| line.contains("Silent prompt received"))
        });
        for value in ["B", "A"] {
            app.chat.input = format!("queue-write:{value}");
            app.chat_action(ChatAction::Send);
            assert!(app.chat.input.is_empty());
        }
        assert_eq!(app.chat.queues[&id].items.len(), 2);
        let forced = app.chat.queues[&id].items[1].id;
        app.chat.input = "Keep composing during queued turns".into();
        app.chat_action(ChatAction::SendQueued(forced));
        assert!(app.chat.session().unwrap().cancelling);
        assert_eq!(
            app.chat
                .session()
                .unwrap()
                .entries
                .iter()
                .filter(|e| matches!(e, ChatEntry::User(_)))
                .count(),
            1
        );
        // A different visible chat's composer must not be used by dispatch.
        let other = p::SessionId::new("background-isolation");
        app.chat
            .sessions
            .insert(other.clone(), Session::new(other.clone()));
        app.chat.open_tab(other.clone());
        app.chat.input = "Another conversation draft".into();
        wait(&mut app, |a| {
            !a.chat.sessions[&id].busy
                && a.chat.queues[&id].is_empty()
                && !a.chat.rollback.busy()
                && !a.chat.rollback.active
        });
        assert_eq!(app.chat.selected.as_ref(), Some(&other));
        assert_eq!(app.chat.input, "Another conversation draft");
        app.chat.open_tab(id.clone());
        assert_eq!(app.chat.input, "Keep composing during queued turns");
        assert_eq!(
            std::fs::read_to_string(root.path().join("queue.txt")).unwrap(),
            "A\nB\n"
        );
        let events = std::fs::read_to_string(&log).unwrap();
        assert!(
            events.find("CANCEL_FINISHED").unwrap() < events.find("START:queue-write:A").unwrap()
        );
        assert!(
            events.find("START:queue-write:A").unwrap()
                < events.find("START:queue-write:B").unwrap()
        );
        let messages: Vec<_> = app
            .chat
            .session()
            .unwrap()
            .entries
            .iter()
            .filter_map(|e| {
                if let ChatEntry::User(text) = e {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(messages, vec!["silent", "queue-write:A", "queue-write:B"]);
        assert_eq!(app.chat.rollback.enabled, git);
        if git {
            let checkpoints = &app.chat.session().unwrap().rollback.checkpoints;
            assert_eq!(checkpoints.len(), 3);
            let cp = checkpoints[2].id;
            app.chat_action(ChatAction::Resolve(Selection::Checkpoint(cp), false));
            wait(&mut app, |a| !a.chat.rollback.busy());
            assert_eq!(
                std::fs::read_to_string(root.path().join("queue.txt")).unwrap(),
                "A\n"
            );
            assert_eq!(
                std::fs::read_to_string(root.path().join("cancel-finished.txt")).unwrap(),
                "finished"
            );
        }
        app.chat_action(ChatAction::Disconnect);
    }
}

#[test]
#[ignore = "requires the official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn acp_chat_sdk_round_trip_queue_stop_attachments_and_error_recovery() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("context.txt");
    std::fs::write(&path, "old context").unwrap();
    let mut app = App::test_new();
    app.explorer.root = root.path().into();
    let python = std::env::var("VTCODE_ACP_TEST_PYTHON").expect("set VTCODE_ACP_TEST_PYTHON; see tests/fixtures/acp-python-sdk/README.md");
    let mut config = AgentConfig::new("Queue SDK", python);
    config.args = vec![
        "-u".into(),
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/scenario_agent.py")
            .to_string_lossy()
            .into_owned(),
    ];
    config
        .env
        .insert("ACP_TEST_EMBEDDED_CONTEXT".into(), "1".into());
    app.settings_effective.acp_agents = Some(vec![config]);
    app.chat_action(ChatAction::Connect(0));
    wait(&mut app, |a| {
        a.chat.selected.is_some() && !a.chat.rollback.busy()
    });
    let id = app.chat.selected.clone().unwrap();
    app.chat.input = "silent".into();
    app.chat_action(ChatAction::Send);
    wait(&mut app, |a| {
        a.chat
            .diagnostics
            .iter()
            .any(|line| line.contains("Silent prompt received"))
    });
    app.chat.input = "inspect attachments".into();
    app.chat.files.attached.push(path.clone());
    app.chat_action(ChatAction::Send);
    std::fs::write(&path, "current context at dispatch").unwrap();
    app.chat_action(ChatAction::Stop);
    wait(&mut app, |a| !a.chat.busy());
    assert_eq!(app.chat.queues[&id].items.len(), 1);
    assert!(app.chat.queues[&id].paused);
    app.chat_action(ChatAction::PauseQueue);
    wait(&mut app, |a| {
        !a.chat.busy() && a.chat.queues[&id].is_empty()
    });
    assert!(app.chat.session().unwrap().entries.iter().any(
        |e| matches!(e, ChatEntry::Agent(text) if text.contains("current context at dispatch"))
    ));
    app.chat.input = "error".into();
    app.chat_action(ChatAction::Send);
    app.chat.input = "Keep this after the error".into();
    app.chat_action(ChatAction::Send);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.chat.busy() {
        app.poll_acp();
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(3));
    }
    assert!(app.chat.error.is_some());
    assert!(app.chat.queues[&id].paused);
    assert_eq!(app.chat.queues[&id].items.len(), 1);
    app.chat_action(ChatAction::Reconnect);
    assert!(app.chat.error.as_ref().unwrap().contains("queued"));
    assert_eq!(app.chat.selected.as_ref(), Some(&id));
    let item = app.chat.queues[&id].items[0].id;
    app.chat_action(ChatAction::EditQueued(item));
    assert_eq!(app.chat.input, "Keep this after the error");
    assert!(app.chat.queues[&id].is_empty());
    app.chat_action(ChatAction::Disconnect);
}
