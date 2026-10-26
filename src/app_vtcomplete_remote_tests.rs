//! Real headless transport, loaded buffers, and the production completion bridge.
use super::*;
use crate::{
    app::Tab,
    editor::Editor,
    remote::{self, workspace::Workspace},
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct Fixture {
    _root: tempfile::TempDir,
    app: App,
    remote: Workspace,
    sent: Arc<AtomicUsize>,
    received: Arc<AtomicUsize>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        for (name, text) in [
            ("definitions.rs", "remoteSharedSymbol remoteSharedSymbol"),
            ("other.rs", "remoteOtherSymbol remoteOtherSymbol"),
            ("query.rs", "remoteSha"),
            ("unopened.rs", "remoteUnopenedSecret remoteUnopenedSecret"),
        ] {
            std::fs::write(root.path().join(name), text).unwrap();
        }
        let sent = Arc::new(AtomicUsize::new(0));
        let received = Arc::new(AtomicUsize::new(0));
        let client = remote::tests::connection_with_traffic(
            root.path(),
            Arc::default(),
            sent.clone(),
            Arc::default(),
            received.clone(),
        );
        let remote = Workspace::new(client);
        let mut app = App::new_deferred(remote.view_root.clone());
        app.remote_workspace = Some(remote.clone());
        app.source_control_enabled = false;
        // Exercise the same workspace initialization path as the real app.
        app.settings_persistence_enabled = true;
        for name in ["definitions.rs", "other.rs", "query.rs"] {
            let key = remote.target_key(name).unwrap();
            let loaded = remote.load(&key).unwrap();
            let mut editor = Editor::test_with_text(&loaded.rope.to_string());
            editor.saved_rope = Some(editor.rope.clone());
            editor.file_path = Some(key);
            editor.language_override = Some("Rust".into());
            app.tabs.push(Tab::new_file(editor));
        }
        app.active_tab = 2;
        app.tabs[2].editor.cursor_col = 9;
        app.last_edit_time = Instant::now() - Duration::from_secs(1);
        remote.client.call(remote::Command::Ping).unwrap();
        Self {
            _root: root,
            app,
            remote,
            sent,
            received,
        }
    }

    fn traffic(&self) -> (usize, usize) {
        (
            self.sent.load(Ordering::Relaxed),
            self.received.load(Ordering::Relaxed),
        )
    }

    fn wait(&mut self, ready: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            self.app.poll_completion_index();
            assert!(self.app.completion_index.status().error.is_none());
            if ready(&self.app) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "remote completion did not settle: {:?}",
                self.app.completion_index.status()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn settle(&mut self, count: usize) {
        self.wait(|app| {
            let status = app.completion_index.status();
            status.indexed_files == count
                && status.pending_files == 0
                && status.phase == crate::completion_index::IndexPhase::Idle
        });
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.remote.client.call(remote::Command::Shutdown);
    }
}

fn has(app: &App, word: &str) -> bool {
    app.completion_index
        .query(&word[..word.len() - 1], "")
        .iter()
        .any(|entry| entry.word == word)
}

#[test]
fn remote_completion_uses_other_opened_files_without_reading_unopened_files_or_sending_traffic() {
    let mut f = Fixture::new();
    let traffic = f.traffic();
    f.settle(3);
    assert!(f.app.completion_index.buffer_only());
    assert!(!f.app.completion_index.settings().project_wide);
    assert!(has(&f.app, "remoteSharedSymbol"));
    assert!(has(&f.app, "remoteOtherSymbol"));
    assert!(!has(&f.app, "remoteUnopenedSecret"));
    f.app.update_vtcomplete_explicit();
    f.app.wait_for_background(Duration::from_secs(5), |app| {
        app.vtcomplete_suggestions
            .iter()
            .any(|word| word == "remoteSharedSymbol")
    });
    assert!(
        !f.app
            .editor()
            .unwrap()
            .rope
            .to_string()
            .contains("remoteSharedSymbol")
    );
    for _ in 0..20 {
        f.app.poll_completion_index();
    }
    assert!(f.app.completion_index_next_wait().is_none());
    assert_eq!(
        f.traffic(),
        traffic,
        "indexing, prediction and idle must not add SSH traffic"
    );
}

#[test]
fn remote_completion_updates_inactive_buffers_and_respects_pause_off_refresh_and_close() {
    let mut f = Fixture::new();
    let traffic = f.traffic();
    f.settle(3);
    f.app
        .set_completion_dropdown("vtcomplete_learning_mode", "Paused");
    f.app.poll_completion_index();
    let editor = &mut f.app.tabs[0].editor;
    editor.rope = ropey::Rope::from_str("remoteUpdatedSymbol remoteUpdatedSymbol");
    editor.content_version += 1;
    editor.modified = true;
    f.app.last_edit_time = Instant::now() - Duration::from_secs(1);
    f.app.poll_completion_index();
    assert!(has(&f.app, "remoteSharedSymbol"));
    assert!(!has(&f.app, "remoteUpdatedSymbol"));
    f.app
        .set_completion_dropdown("vtcomplete_learning_mode", "On");
    f.wait(|app| has(app, "remoteUpdatedSymbol") && !has(app, "remoteSharedSymbol"));
    f.settle(3);
    f.app
        .set_completion_dropdown("vtcomplete_learning_mode", "Off");
    f.app.poll_completion_index();
    assert!(!has(&f.app, "remoteUpdatedSymbol"));
    f.app
        .set_completion_dropdown("vtcomplete_learning_mode", "On");
    f.app.poll_completion_index();
    f.settle(3);
    let path = f.app.tabs[0].editor.file_path.clone().unwrap();
    assert!(f.app.completion_index.remove_file(&path));
    f.settle(2);
    assert!(!has(&f.app, "remoteUpdatedSymbol"));
    assert!(f.app.refresh_completion_files());
    f.wait(|app| has(app, "remoteUpdatedSymbol"));
    f.settle(3);
    f.app.tabs.remove(0);
    f.app.active_tab = 1;
    f.settle(2);
    assert!(
        !has(&f.app, "remoteUpdatedSymbol"),
        "discarded dirty text must be retired"
    );
    f.app.tabs.remove(0);
    f.app.active_tab = 0;
    f.settle(2);
    assert!(
        has(&f.app, "remoteOtherSymbol"),
        "clean previously opened files stay learned"
    );
    assert_eq!(f.traffic(), traffic);
}

#[test]
fn remote_completion_relaunch_restores_the_matching_local_cache_without_ssh_reads() {
    let mut f = Fixture::new();
    f.settle(3);
    let workspace = crate::portable::workspace_dir(&f.remote.view_root);
    let cache = crate::portable::data_dir()
        .join("remote")
        .join(workspace.file_name().unwrap())
        .join("autocomplete");
    let data = std::fs::read_to_string(cache.join("completion-index.jsonl")).unwrap();
    let header: serde_json::Value = serde_json::from_str(data.lines().next().unwrap()).unwrap();
    assert_eq!(header["remote_root_uri"], f.remote.client.hello().root_uri);
    let traffic = f.traffic();
    f.app = App::new_deferred(f.remote.view_root.clone());
    f.app.remote_workspace = Some(f.remote.clone());
    f.app.settings_persistence_enabled = true;
    f.app.source_control_enabled = false;
    f.settle(3);
    assert!(
        f.app.tabs.is_empty(),
        "the cached files have not been reopened"
    );
    assert!(has(&f.app, "remoteSharedSymbol"));
    assert!(has(&f.app, "remoteOtherSymbol"));
    assert!(!has(&f.app, "remoteUnopenedSecret"));
    assert_eq!(
        f.traffic(),
        traffic,
        "cache restoration must not read remote source files"
    );
    let other =
        Workspace::with_connection_identity(f.remote.client.clone(), b"different-ssh-target");
    let other_index = crate::completion_index::CompletionIndex::new_remote(&other);
    let deadline = Instant::now() + Duration::from_secs(5);
    while other_index.status().phase != crate::completion_index::IndexPhase::Idle {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(other_index.file_page(0, 32).total, 0);
    assert_eq!(f.traffic(), traffic);
}

#[test]
fn remote_completion_workspace_switch_drops_the_previous_catalog() {
    let mut first = Fixture::new();
    first.settle(3);
    let mut second = Fixture::new();
    let key = second.app.tabs[0].editor.file_path.clone().unwrap();
    second.app.tabs[0].editor.rope = ropey::Rope::from_str("newWorkspaceOnlyIdentifier");
    second.app.tabs[0].editor.content_version += 1;
    second.app.tabs.truncate(1);
    first.app.dismiss_vtcomplete();
    first.app.tabs = std::mem::take(&mut second.app.tabs);
    first.app.active_tab = 0;
    first.app.explorer.root = second.remote.view_root.clone();
    first.app.remote_workspace = Some(second.remote.clone());
    first.settle(1);
    assert!(has(&first.app, "newWorkspaceOnlyIdentifier"));
    assert!(!has(&first.app, "remoteSharedSymbol"));
    assert_eq!(first.app.completion_index.file_page(0, 32).paths, [key]);
}
