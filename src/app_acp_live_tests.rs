use super::*;

fn live_seeded() -> (tempfile::TempDir, tempfile::TempDir, App, u64) {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    git2::Repository::init(root.path()).unwrap();
    std::fs::write(root.path().join("a.txt"), "first\nkeep\nlast\n").unwrap();
    let mut journal = Journal::open(root.path(), data.path()).unwrap();
    let cp = journal
        .begin("", "live", "live changes", BTreeSet::new())
        .unwrap();
    let mut app = App::test_new();
    app.explorer.root = root.path().into();
    let id = p::SessionId::new("live");
    let mut session = Session::new(id.clone());
    session.begin_prompt("live changes".into());
    app.chat.sessions.insert(id.clone(), session);
    app.chat.selected = Some(id.clone());
    app.chat.open_tabs.push(id.clone());
    app.chat.rollback = ReviewState {
        enabled: true,
        initialized: true,
        root: root.path().into(),
        journal: Some(Arc::new(Mutex::new(journal))),
        ..Default::default()
    };
    app.chat.rollback.turns.insert(
        (0, id),
        TurnCapture {
            checkpoint: Some(cp),
            next_scan: Some(Instant::now() + Duration::from_secs(2)),
            ..Default::default()
        },
    );
    app.refresh_chat_rollback();
    (root, data, app, cp)
}

fn until(app: &mut App, ready: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        app.poll_background_results();
        app.poll_acp();
        assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
        if ready(app) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "live capture timed out: {:?}",
            app.chat.rollback.turns
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn changed(app: &mut App, root: &Path, path: &Path) {
    app.background
        .sender
        .send(crate::background::BackgroundResult::FilesChanged {
            root: root.into(),
            changes: vec![crate::background::inspect_workspace_path(path.into())],
            structure_changed: false,
            rescan: false,
            git_metadata_changed: false,
        })
        .unwrap();
    app.poll_background_results();
}

#[test]
fn completed_writes_and_shell_watcher_events_refresh_inline_controls_during_a_response() {
    let (root, _data, mut app, cp) = live_seeded();
    let path = root.path().join("a.txt");
    let id = app.chat.selected.clone().unwrap();
    show_review_file(&mut app, &path);
    std::fs::write(&path, "FIRST\nkeep\nlast\n").unwrap();
    app.apply_acp_event(vtcode::acp::Event::FileEdited(vtcode::acp::FileEdit {
        session_id: id.clone(),
        path: path.clone(),
        before: "first\nkeep\nlast\n".into(),
        after: "FIRST\nkeep\nlast\n".into(),
    }));
    until(&mut app, |app| {
        !app.chat.rollback.busy()
            && app.pending_agent_change(&path)
            && app.editor().unwrap().rope.to_string() == "FIRST\nkeep\nlast\n"
    });
    assert!(app.chat.session().unwrap().busy);
    assert_eq!(app.chat.session().unwrap().rollback.active, Some(cp));
    assert_eq!(
        app.chat
            .session()
            .unwrap()
            .rollback
            .pending_chunks(&Selection::All)
            .unwrap()
            .len(),
        1
    );
    assert!(!app.chat.rollback.reviewing());
    let first = app.editor_file_review(&path).unwrap().chunks[0].edit;
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    draw_review_file(&mut app, &mut terminal);
    let frame: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(frame.contains("Accept all") && frame.contains("Reject all"));
    assert!(
        app.editor()
            .unwrap()
            .ai_review
            .hits
            .iter()
            .any(|hit| hit.chunk == Some((first, 0)) && hit.accept)
    );
    press_editor_review(&mut app, Some((first, 0)), true);
    settle(&mut app);
    assert!(app.chat.session().unwrap().busy);
    assert!(!app.pending_agent_change(&path));

    std::fs::write(&path, "FIRST\nkeep\nLAST\n").unwrap();
    // This write bypasses ACP file methods, just like a shell command.
    changed(&mut app, root.path(), &path);
    until(&mut app, |app| {
        !app.chat.rollback.busy()
            && app.pending_agent_change(&path)
            && app.editor().unwrap().rope.to_string() == "FIRST\nkeep\nLAST\n"
    });
    assert!(app.chat.session().unwrap().busy);
    let summary = &app.chat.session().unwrap().rollback;
    assert_eq!(summary.edits.len(), 2);
    assert_eq!(summary.edits[0].id, first);
    assert_eq!(
        summary.edits[0].chunks[0].decision,
        vtcode::acp::rollback::Decision::Accepted
    );
    assert_eq!(app.editor_file_review(&path).unwrap().chunks.len(), 1);
    app.chat
        .session_mut()
        .unwrap()
        .finish(Some(p::StopReason::EndTurn));
    until(&mut app, |app| {
        !app.chat.rollback.active && !app.chat.rollback.busy()
    });
    assert_eq!(app.chat.session().unwrap().rollback.edits.len(), 2);
}

#[test]
fn continuously_changing_chat_does_not_starve_another_active_chat() {
    let (root, _data, mut app, _) = live_seeded();
    let path = root.path().join("a.txt");
    let first = app.chat.selected.clone().unwrap();
    let other = p::SessionId::new("other");
    let cp = app
        .chat
        .rollback
        .journal
        .as_ref()
        .unwrap()
        .lock()
        .unwrap()
        .begin("", "other", "other live changes", BTreeSet::new())
        .unwrap();
    let mut session = Session::new(other.clone());
    session.begin_prompt("other live changes".into());
    app.chat.sessions.insert(other.clone(), session);
    app.chat.rollback.turns.insert(
        (0, other.clone()),
        TurnCapture {
            checkpoint: Some(cp),
            next_scan: Some(Instant::now() + Duration::from_secs(1)),
            ..Default::default()
        },
    );
    std::fs::write(&path, "FIRST\nkeep\nlast\n").unwrap();
    app.chat.rollback.note_workspace_change(&path);
    app.capture_running_chat_changes();
    assert_eq!(
        app.chat.rollback.job.as_ref().unwrap().session,
        Some(other.clone())
    );
    // Further notifications keep arriving for the selected chat while the
    // older background chat's capture is already in flight.
    for _ in 0..5 {
        app.chat.rollback.note_agent_write(0, &first, &path);
    }
    until(&mut app, |app| {
        !app.chat.rollback.busy()
            && app.chat.sessions[&first].rollback.edits.len() == 1
            && app.chat.sessions[&other].rollback.edits.len() == 1
    });
    for id in [first, other] {
        assert!(app.chat.sessions[&id].busy);
        assert!(app.chat.sessions[&id].rollback.active.is_some());
        assert_eq!(
            app.chat.sessions[&id]
                .rollback
                .pending_chunks(&Selection::All)
                .unwrap()
                .len(),
            1
        );
    }
}

#[test]
fn hidden_background_agent_captures_host_and_shell_edits_with_colliding_session_ids() {
    let (root, _data, mut app, cp) = live_seeded();
    let path = root.path().join("a.txt");
    let id = app.chat.selected.clone().unwrap();
    super::background_review_tests::other_agent(&mut app, &id);
    app.select_chat_agent(1);
    show_review_file(&mut app, &path);
    assert!(!app.chat.visible);
    std::fs::write(&path, "FIRST\nkeep\nlast\n").unwrap();
    app.with_agent_context(0, true, |app| {
        app.apply_acp_event(vtcode::acp::Event::FileEdited(vtcode::acp::FileEdit {
            session_id: id.clone(),
            path: path.clone(),
            before: "first\nkeep\nlast\n".into(),
            after: "FIRST\nkeep\nlast\n".into(),
        }));
    });
    until(&mut app, |app| {
        !app.chat.rollback.busy()
            && app.pending_agent_change(&path)
            && app.editor().unwrap().rope.to_string() == "FIRST\nkeep\nlast\n"
    });
    assert_eq!(app.chat.active_agent, 1);
    assert_eq!(
        app.chat.agent_state(0).unwrap().sessions[&id].rollback.active,
        Some(cp),
    );
    assert!(app.chat.session().unwrap().rollback.edits.is_empty());
    super::background_review_tests::assert_file_marked(&mut app, &path, 1);

    std::fs::write(&path, "FIRST\nkeep\nLAST\n").unwrap();
    changed(&mut app, root.path(), &path);
    until(&mut app, |app| {
        !app.chat.rollback.busy()
            && app.editor_file_review(&path).is_some_and(|file| file.chunks.len() == 2)
            && app.editor().unwrap().rope.to_string() == "FIRST\nkeep\nLAST\n"
    });
    super::background_review_tests::assert_file_marked(&mut app, &path, 2);
    app.with_agent_context(0, true, |app| {
        app.chat
            .session_mut()
            .unwrap()
            .finish(Some(p::StopReason::EndTurn));
    });
    until(&mut app, |app| {
        !app.chat.rollback.active && !app.chat.rollback.busy()
    });
    assert_eq!(app.chat.active_agent, 1);
    assert!(app.chat.session().unwrap().rollback.edits.is_empty());
    assert!(!app.chat.visible);
    super::background_review_tests::assert_file_marked(&mut app, &path, 2);
}

#[test]
fn live_capture_coalesces_changes_waits_for_writes_and_retries_without_losing_paths() {
    let (root, _data, mut app, _) = live_seeded();
    let id = app.chat.selected.clone().unwrap();
    let path = root.path().join("a.txt");
    let write = app.chat.rollback.file_request_guard(0, &id, true);
    std::fs::write(&path, "FIRST\nkeep\nlast\n").unwrap();
    for _ in 0..5 {
        app.chat.rollback.note_agent_write(0, &id, &path);
    }
    assert_eq!(
        app.chat.rollback.turns[&(0, id.clone())]
            .changed_paths
            .len(),
        1
    );
    app.poll_acp();
    assert!(!app.chat.rollback.busy());
    assert!(app.chat.session().unwrap().rollback.edits.is_empty());
    assert!(app.chat.rollback.next_capture_deadline().is_none());
    drop(write);
    assert!(app.chat.rollback.next_capture_deadline().is_some());
    // Simulate a file still changing when the first background capture reads it.
    let turn = app.chat.rollback.turns.get_mut(&(0, id.clone())).unwrap();
    turn.capture_paths = std::mem::take(&mut turn.changed_paths);
    turn.capturing = true;
    app.checkpoint_job(JobKind::Capture, Some(id.clone()), false, || {
        Err("File changed during checkpoint capture".into())
    });
    settle(&mut app);
    assert!(app.chat.error.is_none());
    assert!(app.chat.session().unwrap().busy);
    let turn = app.chat.rollback.turns.get_mut(&(0, id.clone())).unwrap();
    assert!(!turn.failed && !turn.capturing);
    assert!(turn.changed_paths.contains(Path::new("a.txt")));
    assert!(turn.retry_at.is_some());
    turn.retry_at = Some(Instant::now());
    until(&mut app, |app| {
        app.pending_agent_change(&path) && !app.chat.rollback.busy()
    });
    assert!(
        app.chat.rollback.turns[&(0, id.clone())]
            .capture_error
            .is_none()
    );
    assert_eq!(app.chat.session().unwrap().rollback.edits.len(), 1);

    // Fallback capture also has a deadline when chat is hidden and no watcher
    // event arrives, rather than depending on the thinking animation to poll.
    app.chat.visible = false;
    std::fs::write(&path, "FIRST\nkeep\nLAST\n").unwrap();
    app.chat.rollback.turns.get_mut(&(0, id)).unwrap().next_scan = Some(Instant::now());
    assert!(app.chat.rollback.next_capture_deadline().unwrap() <= Instant::now());
    until(&mut app, |app| {
        app.chat.session().unwrap().rollback.edits.len() == 2 && !app.chat.rollback.busy()
    });
    assert!(app.chat.session().unwrap().busy);
}

#[test]
#[ignore = "requires official Python SDK; see tests/fixtures/acp-python-sdk/README.md"]
fn acp_chat_sdk_round_trip_live_changes_appear_before_turn_end_for_host_and_shell_writes() {
    for shell in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let gates = tempfile::tempdir().unwrap();
        git2::Repository::init(root.path()).unwrap();
        let path = root.path().join("live.txt");
        std::fs::write(&path, "first\nkeep\nlast\n").unwrap();
        let mut app = App::test_new();
        app.explorer.root = root.path().into();
        app.background.start_file_watcher(root.path().into());
        let mut config = AgentConfig::new(
            "Live SDK",
            std::env::var("VTCODE_ACP_TEST_PYTHON").expect("set VTCODE_ACP_TEST_PYTHON; see tests/fixtures/acp-python-sdk/README.md"),
        );
        config.args = vec![
            "-u".into(),
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/scenario_agent.py")
                .to_string_lossy()
                .into_owned(),
        ];
        config.env.insert(
            "ACP_TEST_LIVE_GATE_DIR".into(),
            gates.path().to_string_lossy().into_owned(),
        );
        app.settings_effective.acp_agents = Some(vec![config]);
        app.chat_action(ChatAction::Connect(0));
        until(&mut app, |app| {
            app.chat.selected.is_some()
                && app.chat.rollback.initialized
                && !app.chat.rollback.busy()
        });
        show_review_file(&mut app, &path);
        app.chat.input = if shell {
            "live-changes-shell"
        } else {
            "live-changes-acp"
        }
        .into();
        app.chat_action(ChatAction::Send);
        until(&mut app, |app| {
            gates.path().join("ready-1").exists()
                && app.pending_agent_change(&path)
                && !app.chat.rollback.busy()
                && app.editor().unwrap().rope.to_string() == "FIRST\nkeep\nlast\n"
        });
        assert!(app.chat.session().unwrap().busy);
        let checkpoint = app.chat.session().unwrap().rollback.active.unwrap();
        let first = app.chat.session().unwrap().rollback.edits[0].id;
        app.chat_action(ChatAction::Resolve(Selection::All, true));
        settle(&mut app);
        assert!(app.chat.session().unwrap().busy);
        assert!(!app.pending_agent_change(&path));
        std::fs::write(gates.path().join("next"), "go").unwrap();
        until(&mut app, |app| {
            gates.path().join("ready-2").exists()
                && app.pending_agent_change(&path)
                && !app.chat.rollback.busy()
        });
        assert!(app.chat.session().unwrap().busy);
        let summary = &app.chat.session().unwrap().rollback;
        assert_eq!(summary.edits.len(), 2);
        assert_eq!(summary.edits[0].id, first);
        assert_eq!(
            summary.edits[0].chunks[0].decision,
            vtcode::acp::rollback::Decision::Accepted
        );
        assert_eq!(summary.pending_chunks(&Selection::All).unwrap().len(), 1);
        assert_eq!(app.editor_file_review(&path).unwrap().chunks.len(), 1);
        std::fs::write(gates.path().join("finish"), "go").unwrap();
        until(&mut app, |app| {
            !app.chat.session().unwrap().busy
                && !app.chat.rollback.active
                && !app.chat.rollback.busy()
        });
        assert_eq!(app.chat.session().unwrap().rollback.edits.len(), 2);
        app.chat_action(ChatAction::Resolve(
            Selection::Checkpoint(checkpoint),
            false,
        ));
        settle(&mut app);
        assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "first\nkeep\nlast\n"
        );
        app.chat_action(ChatAction::Disconnect);
    }
}
