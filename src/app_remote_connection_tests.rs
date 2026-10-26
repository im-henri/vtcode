use super::*;
use crate::{
    app::Tab,
    editor::Editor,
    remote::{self, Command},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use std::sync::atomic::{AtomicBool, AtomicUsize};

struct Fixture {
    root: tempfile::TempDir,
    app: App,
    original: workspace::Workspace,
    path: PathBuf,
}
struct ReleaseOnDrop(Arc<AtomicBool>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file.txt"), "before\n").unwrap();
        let original = workspace::Workspace::new(remote::tests::connection(
            root.path(),
            Arc::default(),
            Arc::default(),
        ));
        let path = original.target_key("file.txt").unwrap();
        let mut app = App::new_deferred(original.view_root.clone());
        app.remote_workspace = Some(original.clone());
        app.source_control_enabled = false;
        app.trim_trailing_on_save = false;
        app.insert_final_newline = false;
        app.open_file(&path);
        app.wait_for_background(Duration::from_secs(10), |app| {
            app.editor().is_some_and(|e| !e.bg_loading)
        });
        Self {
            root,
            app,
            original,
            path,
        }
    }
    fn replacement(
        &mut self,
        hold: Arc<AtomicBool>,
        traffic: Arc<AtomicUsize>,
    ) -> workspace::Workspace {
        self.original.client.call(Command::Shutdown).unwrap();
        // Simulate the generation boundary at transport loss, before callbacks
        // from its replacement may be delivered. Automatic launch is exercised
        // separately by the real SSH tests.
        self.app.workspace_generation += 1;
        self.original
            .resume_on(workspace::Workspace::new(remote::tests::connection(
                self.root.path(),
                hold,
                traffic,
            )))
            .unwrap()
    }
    fn resume(&mut self) {
        let next = self.replacement(Arc::default(), Arc::default());
        self.app.resume_remote_services(next);
        self.wait();
    }
    fn wait(&mut self) {
        self.app
            .wait_for_background(Duration::from_secs(10), |app| {
                app.remote_connection.phase == Phase::Restored
            });
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(remote) = &self.app.remote_workspace
            && remote.client.connected()
        {
            let _ = remote.client.call(Command::Shutdown);
        }
    }
}

#[test]
fn remote_reconnect_preserves_dirty_shared_buffer_views_and_history() {
    let mut f = Fixture::new();
    let mut other = Editor::test_with_text("before\n");
    other.file_path = Some(f.path.clone());
    f.app.tabs.push(Tab::new_file(other));
    f.app.sync_duplicate_file_tabs();
    f.app.editor_mut().unwrap().insert_char('λ');
    f.app.editor_mut().unwrap().break_undo_group();
    let identity = f.app.editor().unwrap().save_identity;
    f.resume();
    assert_eq!(f.app.remote_connection.conflicts, 0);
    for tab in &mut f.app.tabs {
        tab.editor.refresh_document();
        assert_eq!(tab.editor.save_identity, identity);
        assert_eq!(tab.editor.rope, "λbefore\n");
        assert!(tab.editor.modified);
    }
    f.app.editor_mut().unwrap().undo();
    assert_eq!(f.app.editor().unwrap().rope, "before\n");
    assert!(!f.app.editor().unwrap().modified);
    f.app.editor_mut().unwrap().redo();
    assert_eq!(f.app.editor().unwrap().rope, "λbefore\n");
    f.app.save_current_file();
    f.app.wait_for_background(Duration::from_secs(10), |app| {
        app.pending_file_saves.is_empty()
    });
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("file.txt")).unwrap(),
        "λbefore\n"
    );
    assert!(!f.app.editor().unwrap().modified);
}

#[test]
fn remote_reconnect_recognizes_lost_save_ack_without_replaying_a_write() {
    let mut f = Fixture::new();
    f.app.editor_mut().unwrap().insert_char('!');
    f.app.editor_mut().unwrap().break_undo_group();
    // The remote write completed before its acknowledgement was lost.
    std::fs::write(f.root.path().join("file.txt"), "!before\n").unwrap();
    f.resume();
    assert_eq!(f.app.remote_connection.conflicts, 0);
    assert!(!f.app.editor().unwrap().modified);
    assert!(f.app.pending_file_saves.is_empty());
    f.app.editor_mut().unwrap().undo();
    assert_eq!(f.app.editor().unwrap().rope, "before\n");
    assert!(f.app.editor().unwrap().modified);
    f.app.editor_mut().unwrap().redo();
    assert!(!f.app.editor().unwrap().modified);
}

#[test]
fn remote_reconnect_check_keeps_typing_after_snapshot_and_blocks_racing_save() {
    let mut f = Fixture::new();
    f.app.editor_mut().unwrap().insert_char('!');
    f.app.editor_mut().unwrap().break_undo_group();
    std::fs::write(f.root.path().join("file.txt"), "!before\n").unwrap();
    let hold = Arc::<AtomicBool>::default();
    let _release = ReleaseOnDrop(hold.clone());
    let next = f.replacement(hold.clone(), Arc::default());
    hold.store(true, std::sync::atomic::Ordering::Release);
    f.app.resume_remote_services(next);
    f.app.editor_mut().unwrap().insert_char('x');
    f.app.editor_mut().unwrap().break_undo_group();
    f.app.save_current_file();
    assert!(f.app.pending_file_saves.is_empty());
    assert!(f.app.remote_connection.checking.contains(&f.path));
    hold.store(false, std::sync::atomic::Ordering::Release);
    f.wait();
    assert_eq!(f.app.editor().unwrap().rope, "!xbefore\n");
    assert!(f.app.editor().unwrap().modified);
    assert_eq!(
        f.app.editor().unwrap().saved_rope.as_ref().unwrap(),
        "!before\n"
    );
    f.app.editor_mut().unwrap().undo();
    assert!(!f.app.editor().unwrap().modified);
    f.app.editor_mut().unwrap().redo();
    assert!(f.app.editor().unwrap().modified);
}

#[test]
fn remote_reconnect_reloads_clean_shared_document_only_once_and_is_undoable() {
    let mut f = Fixture::new();
    let mut other = Editor::test_with_text("before\n");
    other.file_path = Some(f.path.clone());
    f.app.tabs.push(Tab::new_file(other));
    f.app.sync_duplicate_file_tabs();
    std::fs::write(f.root.path().join("file.txt"), "external\n").unwrap();
    f.resume();
    assert_eq!(f.app.remote_connection.conflicts, 0);
    for tab in &mut f.app.tabs {
        tab.editor.refresh_document();
        assert_eq!(tab.editor.rope, "external\n");
        assert!(!tab.editor.modified);
    }
    f.app.editor_mut().unwrap().undo();
    assert_eq!(f.app.editor().unwrap().rope, "before\n");
    assert!(f.app.editor().unwrap().modified);
    f.app.editor_mut().unwrap().redo();
    assert!(!f.app.editor().unwrap().modified);
}

#[test]
fn remote_reconnect_conflict_retains_local_changes_and_refuses_overwrite() {
    let mut f = Fixture::new();
    f.app.editor_mut().unwrap().insert_char('!');
    f.app.editor_mut().unwrap().break_undo_group();
    std::fs::write(f.root.path().join("file.txt"), "external\n").unwrap();
    f.resume();
    assert_eq!(f.app.remote_connection.conflicts, 1);
    assert!(f.app.side_notice.visible() && f.app.remote_connection.expires.is_none());
    assert_eq!(f.app.editor().unwrap().rope, "!before\n");
    f.app.save_current_file();
    f.app.wait_for_background(Duration::from_secs(10), |app| {
        app.pending_file_saves.is_empty()
    });
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("file.txt")).unwrap(),
        "external\n"
    );
    assert!(f.app.editor().unwrap().modified);
    f.app.editor_mut().unwrap().undo();
    assert_eq!(f.app.editor().unwrap().rope, "before\n");
}

#[test]
fn remote_reconnect_missing_file_retains_buffer_and_warns_until_dismissed() {
    let mut f = Fixture::new();
    f.app.editor_mut().unwrap().insert_char('!');
    std::fs::remove_file(f.root.path().join("file.txt")).unwrap();
    f.resume();
    assert_eq!(f.app.remote_connection.conflicts, 1);
    assert!(f.app.remote_connection.expires.is_none());
    assert_eq!(f.app.editor().unwrap().rope, "!before\n");
    assert_eq!(f.app.editor().unwrap().file_path.as_ref(), Some(&f.path));
    assert!(f.app.editor().unwrap().modified);
}

#[test]
fn remote_reconnect_unchanged_file_checks_without_uploading_or_replacing_document() {
    let mut f = Fixture::new();
    let rope = f.app.editor().unwrap().rope.clone();
    let traffic = Arc::<AtomicUsize>::default();
    let next = f.replacement(Arc::default(), traffic.clone());
    f.app.resume_remote_services(next);
    f.wait();
    assert!(f.app.editor().unwrap().rope.is_instance(&rope));
    assert!(traffic.load(std::sync::atomic::Ordering::Relaxed) < 16 * 1024);
    assert!(f.app.pending_file_saves.is_empty());
}

#[test]
fn remote_reconnect_stale_save_cannot_clear_new_pending_save_or_mark_clean() {
    let mut f = Fixture::new();
    let old_generation = f.app.workspace_generation;
    f.app.editor_mut().unwrap().insert_char('!');
    let document = f.app.editor().unwrap().save_identity;
    let version = f.app.editor().unwrap().content_version;
    let source = f.app.editor().unwrap().rope.clone();
    f.resume();
    f.app
        .pending_file_saves
        .insert(f.path.clone(), (f.app.workspace_generation, version.into()));
    f.app
        .background
        .sender
        .send(BackgroundResult::FileSaved {
            source_document_id: document,
            workspace_generation: old_generation,
            path: f.path.clone(),
            error: Some(std::io::Error::other("old SSH connection ended")),
            last_disk_modified: None,
            file_size: None,
            source_content_version: version,
            source_rope: source,
            previous_disk: None,
            saved_encoding: "UTF-8",
            transformed_lines: None,
            disk_written: false,
            saved_rope: None,
        })
        .unwrap();
    f.app.poll_background_results();
    assert!(f.app.pending_file_saves.contains_key(&f.path));
    assert!(f.app.editor().unwrap().modified);
    f.app.pending_file_saves.clear();
}

#[test]
fn remote_reconnect_backoff_is_bounded_stoppable_and_version_mismatch_is_terminal() {
    let mut state = State::default();
    for (attempt, seconds) in [(1, 1), (2, 2), (3, 4), (6, 30), (1000, 30)] {
        state.attempt = attempt;
        state.failed("SSH failed".into());
        let wait = state.next_wait().unwrap();
        assert!(
            wait <= Duration::from_secs(seconds)
                && wait > Duration::from_millis(seconds * 1000 - 500)
        );
        assert!(state.phase == Phase::Waiting);
    }
    state.failed("VTCode version mismatch: local 1, headless 2".into());
    assert!(state.phase == Phase::Stopped);
    assert!(state.next_wait().is_none());
    state.cancel_attempt();
    assert!(state.next_wait().is_none());
}

#[test]
fn remote_reconnect_side_popup_is_modeless_and_quit_confirmation_keeps_priority() {
    let mut f = Fixture::new();
    f.app.remote_connection.phase = Phase::Waiting;
    f.app.remote_connection.attempt = 2;
    f.app.remote_connection.message = "Connection lost. Reconnecting…".into();
    f.app.publish_remote_connection_notice(true);
    let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
    terminal
        .draw(|frame| crate::ui::draw(frame, &mut f.app))
        .unwrap();
    let rect = f.app.side_notice.area.unwrap();
    assert_eq!(rect.x, 1);
    assert_eq!(rect.height, 5);
    assert!(rect.right() < 110);
    let screen: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(screen.contains("SSH connection lost"));
    assert!(screen.contains("attempt 3 next"));
    assert!(!crate::ui_remote_connection::handle_key(
        &mut f.app,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)
    ));
    crate::input_keyboard::handle_keyboard_event(
        &mut f.app,
        KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
    );
    assert_eq!(f.app.editor().unwrap().rope, "xbefore\n");
    f.app.remote_connection.details = true;
    assert!(crate::ui_remote_connection::handle_key(
        &mut f.app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)
    ));
    assert!(!f.app.remote_connection.details);
    f.app.exit_confirm_active = true;
    f.app.remote_connection.details = true;
    assert!(!crate::ui_remote_connection::handle_key(
        &mut f.app,
        KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)
    ));
    for (width, height) in [(1, 1), (3, 3), (18, 7)] {
        Terminal::new(TestBackend::new(width, height))
            .unwrap()
            .draw(|frame| crate::ui_remote_connection::draw(frame, &mut f.app))
            .unwrap();
    }
}

#[test]
fn dismissing_a_connection_notice_lasts_through_failed_retries_and_recovery() {
    let mut f = Fixture::new();
    f.app.remote_connection.phase = Phase::Waiting;
    f.app.publish_remote_connection_notice(true);
    f.app.side_notice.dismiss(NOTICE_ID);
    for attempt in 1..=3 {
        f.app.remote_connection.attempt = attempt;
        f.app.remote_reconnect_failed("SSH fixture failed".into());
        assert!(!f.app.side_notice.visible());
    }
    f.resume();
    assert!(!f.app.side_notice.visible(), "recovery must not reopen a dismissed notice");
    f.app.begin_remote_disconnect();
    assert!(f.app.side_notice.visible(), "a new outage deserves its own notice");
    f.app.side_notice.dismiss(NOTICE_ID);
    f.app.execute_palette_command("remote_connection_status");
    assert!(f.app.side_notice.visible(), "status can be reopened explicitly");
}

#[test]
fn remote_reconnect_resumes_current_subfolder_and_rejects_other_identity() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("sub")).unwrap();
    let initial = workspace::Workspace::with_connection_identity(
        remote::tests::connection(root.path(), Arc::default(), Arc::default()),
        b"original SSH/job command",
    );
    let current = initial
        .open_workspace(&initial.target_key("sub").unwrap())
        .unwrap();
    let next = workspace::Workspace::with_connection_identity(
        remote::tests::connection(root.path(), Arc::default(), Arc::default()),
        b"original SSH/job command",
    );
    let resumed = current.resume_on(next).unwrap();
    assert_eq!(resumed.view_root, current.view_root);
    resumed.client.call(Command::Shutdown).unwrap();
    let other = workspace::Workspace::with_connection_identity(
        remote::tests::connection(root.path(), Arc::default(), Arc::default()),
        b"different command",
    );
    assert!(current.resume_on(other.clone()).is_err());
    initial.client.call(Command::Shutdown).unwrap();
    other.client.call(Command::Shutdown).unwrap();
}
