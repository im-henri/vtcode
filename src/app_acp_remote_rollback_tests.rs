use super::*;
use crate::remote::{self, workspace::Workspace};

fn remote() -> (tempfile::TempDir, Workspace, App) {
    let root = tempfile::tempdir().unwrap();
    git2::Repository::init(root.path()).unwrap();
    std::fs::write(root.path().join("a.txt"), "first\nkeep\nlast\n").unwrap();
    let workspace = Workspace::new(remote::tests::connection(
        root.path(),
        Arc::default(),
        Arc::default(),
    ));
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    app.source_control_enabled = false;
    workspace.client.set_wake(app.background.wake_handle());
    let id = p::SessionId::new("chat");
    app.chat
        .sessions
        .insert(id.clone(), vtcode::acp::Session::new(id.clone()));
    app.chat.selected = Some(id.clone());
    app.chat.open_tabs.push(id);
    app.initialize_chat_rollback(&AgentConfig::new("agent", "unused"));
    settle(&mut app);
    assert!(
        app.chat.rollback.enabled,
        "{:?}",
        app.chat.rollback.initialization_error()
    );
    (root, workspace, app)
}
fn settle(app: &mut App) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        app.poll_chat_rollback();
        app.poll_background_results();
        if !app.chat.rollback.busy() && !app.background.has_active_jobs() {
            break;
        }
        assert!(Instant::now() < deadline, "Checkpoint work did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn wait_agent(app: &mut App, condition: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        app.poll_acp();
        app.poll_background_results();
        if condition(app) { return; }
        assert!(Instant::now() < deadline, "No progress: status={}, error={:?}", app.chat.status, app.chat.error);
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn connect_agent(app: &mut App) {
    app.settings_effective.acp_agents = Some(vec![remote::agent::test_config()]);
    app.focus_chat();
    app.chat_action(ChatAction::Connect(0));
    wait_agent(app, |app| app.chat.selected.is_some() && !app.chat.control_busy && !app.chat.rollback.busy());
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
}
fn send_agent(app: &mut App, text: &str) {
    app.chat.input = text.into();
    app.chat.cursor = text.len();
    app.chat_action(ChatAction::Send);
    wait_agent(app, |app| !app.chat.busy() && !app.chat.rollback.busy() && !app.background.has_active_jobs());
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
}

#[test]
#[ignore = "headless App/ACP integration; requires Python 3 (VTCODE_TEST_PYTHON)"]
fn remote_agent_uses_unsaved_buffers_in_hidden_desks_and_writes_reload_with_undo() {
    let (root, workspace, mut app) = remote();
    std::fs::write(root.path().join("agent.txt"), "disk one\ndisk two\n").unwrap();
    let path = workspace.target_key("agent.txt").unwrap();
    app.open_file(&path);
    app.wait_for_background(Duration::from_secs(10), |app| app.editor().is_some_and(|editor| !editor.bg_loading));
    app.editor_mut().unwrap().cursor_line = 1;
    app.editor_mut().unwrap().cursor_col = 0;
    app.editor_mut().unwrap().paste_with_text("unsaved ");
    let dirty = app.editor().unwrap().rope.to_string();
    let main = app.desks.active;
    app.new_desk("Other", false);
    connect_agent(&mut app);
    send_agent(&mut app, "read");
    assert!(app.chat.session().unwrap().entries.iter().any(|entry| matches!(entry, ChatEntry::Agent(text) if text.contains("unsaved disk two"))));
    send_agent(&mut app, "write");
    assert!(app.chat.session().unwrap().entries.iter().any(|entry| matches!(entry, ChatEntry::Agent(text) if text.contains("write-denied"))));
    send_agent(&mut app, "write-burst");
    assert!(app.chat.session().unwrap().entries.iter().any(|entry| matches!(entry, ChatEntry::Agent(text) if text.contains("all-64-replies-received"))));
    assert!(!app.chat.session().unwrap().entries.iter().any(|entry| matches!(entry, ChatEntry::Agent(text) if text.contains("unexpected-write"))));
    assert_eq!(std::fs::read_to_string(root.path().join("agent.txt")).unwrap(), "disk one\ndisk two\n");
    let session = app.chat.selected.clone();
    app.switch_desk(main);
    app.chat.selected = session;
    assert_eq!(app.editor().unwrap().rope.to_string(), dirty);
    app.editor_mut().unwrap().undo();
    assert!(!app.editor().unwrap().modified);
    send_agent(&mut app, "write");
    wait_agent(&mut app, |app| app.editor().is_some_and(|editor| editor.rope.to_string() == "node edit λ\nsecond\n"));
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "disk one\ndisk two\n");
    assert!(app.editor().unwrap().modified);
    app.editor_mut().unwrap().redo();
    assert!(!app.editor().unwrap().modified);
    drop(app);
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
#[cfg(unix)]
#[ignore = "headless App/ACP symlink integration; requires Python 3 (VTCODE_TEST_PYTHON)"]
fn remote_agent_revalidates_retargeted_aliases_before_reading_or_writing_buffers() {
    let (root, workspace, mut app) = remote();
    std::fs::write(root.path().join("agent.txt"), "disk one\ndisk two\n").unwrap();
    std::fs::write(root.path().join("other.txt"), "other one\nother two\n").unwrap();
    std::os::unix::fs::symlink("other.txt", root.path().join("alias.txt")).unwrap();
    let path = workspace.target_key("alias.txt").unwrap();
    app.open_file(&path);
    app.wait_for_background(Duration::from_secs(10), |app| app.editor().is_some_and(|editor| !editor.bg_loading));
    app.editor_mut().unwrap().cursor_line = 1;
    app.editor_mut().unwrap().paste_with_text("unsaved ");
    std::fs::remove_file(root.path().join("alias.txt")).unwrap();
    std::os::unix::fs::symlink("agent.txt", root.path().join("alias.txt")).unwrap();
    connect_agent(&mut app);
    send_agent(&mut app, "read");
    assert!(app.chat.session().unwrap().entries.iter().any(|entry| matches!(entry, ChatEntry::Agent(text) if text.contains("unsaved other two"))));
    send_agent(&mut app, "write");
    assert_eq!(std::fs::read_to_string(root.path().join("agent.txt")).unwrap(), "disk one\ndisk two\n");
    assert!(app.chat.session().unwrap().entries.iter().any(|entry| matches!(entry, ChatEntry::Agent(text) if text.contains("write-denied"))));
    drop(app);
    workspace.client.call(remote::Command::Shutdown).unwrap();
}
fn capture(root: &Path, app: &mut App) -> u64 {
    let mut journal = app.chat.rollback.journal.as_ref().unwrap().lock().unwrap();
    let checkpoint = journal
        .begin("", "chat", "change this", BTreeSet::new())
        .unwrap();
    std::fs::write(root.join("a.txt"), "FIRST\nkeep\nLAST\n").unwrap();
    journal
        .finish_checkpoint(checkpoint, BTreeSet::new())
        .unwrap();
    app.chat.rollback.inline_reviews = Arc::new(journal.inline_reviews());
    drop(journal);
    app.refresh_chat_rollback();
    checkpoint
}

#[test]
fn remote_checkpoint_reject_and_undo_reload_the_shared_buffer_with_local_undo() {
    let (root, workspace, mut app) = remote();
    capture(root.path(), &mut app);
    let path = workspace.target_key("a.txt").unwrap();
    app.open_file(&path);
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "FIRST\nkeep\nLAST\n"
    );
    let review = app.editor_file_review(&path).unwrap();
    assert_eq!(review.chunks.len(), 2);
    let selected = (review.chunks[0].edit, review.chunks[0].index);
    app.resolve_editor_changes(&path, Some(selected), false);
    settle(&mut app);
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    assert_eq!(
        std::fs::read_to_string(root.path().join("a.txt")).unwrap(),
        "first\nkeep\nLAST\n"
    );
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "first\nkeep\nLAST\n"
    );
    app.editor_mut().unwrap().undo();
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "FIRST\nkeep\nLAST\n"
    );
    assert!(app.editor().unwrap().modified);
    app.undo_chat_rollback();
    settle(&mut app);
    assert!(
        app.chat
            .error
            .as_deref()
            .is_some_and(|e| e.contains("unsaved"))
    );
    app.editor_mut().unwrap().redo();
    app.chat.error = None;
    app.undo_chat_rollback();
    settle(&mut app);
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    assert_eq!(
        app.editor().unwrap().rope.to_string(),
        "FIRST\nkeep\nLAST\n"
    );
    drop(app);
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn remote_checkpoint_plans_cannot_overwrite_later_changes_or_another_workspace() {
    let (root, workspace, mut app) = remote();
    capture(root.path(), &mut app);
    let reference = app.chat.rollback.journal.clone().unwrap();
    let mut journal = reference.lock().unwrap();
    let plan = journal.plan("", "chat", &Selection::All).unwrap();
    let foreign_plan = journal.plan("", "chat", &Selection::All).unwrap();
    std::fs::write(root.path().join("a.txt"), "changed externally\n").unwrap();
    assert!(journal.apply(plan).is_err());
    assert_eq!(
        std::fs::read_to_string(root.path().join("a.txt")).unwrap(),
        "changed externally\n"
    );
    let nested = root.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    git2::Repository::init(&nested).unwrap();
    let second = workspace
        .open_workspace(&workspace.target_key("nested").unwrap())
        .unwrap();
    let mut other = Journal::open_remote(second).unwrap().unwrap();
    assert!(
        other
            .apply(foreign_plan)
            .unwrap_err()
            .contains("another workspace")
    );
    drop(other);
    drop(journal);
    drop(reference);
    drop(app);
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn remote_checkpoint_previews_are_cached_and_preserve_case_and_created_file_deletion() {
    let (root, workspace, mut app) = remote();
    let path = workspace.target_key("Created.txt").unwrap();
    let lower = workspace.target_key("created.txt").unwrap();
    let reference = app.chat.rollback.journal.clone().unwrap();
    let mut journal = reference.lock().unwrap();
    let checkpoint = journal
        .begin("", "chat", "create file", BTreeSet::new())
        .unwrap();
    std::fs::write(root.path().join("Created.txt"), "new file\n").unwrap();
    journal
        .finish_checkpoint(checkpoint, BTreeSet::new())
        .unwrap();
    let summary = journal.summary("", "chat");
    assert!(summary.pending_file(&path).is_some());
    assert!(
        summary.pending_file(&lower).is_none(),
        "remote names must not be case folded by Windows"
    );
    app.chat.rollback.inline_reviews = Arc::new(journal.inline_reviews());
    drop(journal);
    app.refresh_chat_rollback();
    app.open_file(&path);
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    app.resolve_editor_changes(&path, None, false);
    settle(&mut app);
    assert!(app.chat.error.is_none(), "{:?}", app.chat.error);
    assert!(!root.path().join("Created.txt").exists());
    assert!(
        app.tabs
            .iter()
            .all(|t| t.editor.file_path.as_ref() != Some(&path))
    );
    assert!(!app.pending_agent_change(&path));
    drop(reference);
    drop(app);
    workspace.client.call(remote::Command::Shutdown).unwrap();
}
