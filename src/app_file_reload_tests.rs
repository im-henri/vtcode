use super::*;
use crate::{editor::Editor, remote};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

struct Release(Arc<AtomicBool>);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < deadline, "reload did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn tab(path: &Path) -> Tab {
    let mut editor = Editor::test_with_text("before\n");
    editor.file_path = Some(path.into());
    editor.saved_rope = Some(editor.rope.clone());
    Tab::new_file(editor)
}

#[test]
fn remote_reload_skips_closed_dirty_loading_and_immutable_views_without_io() {
    let root = tempfile::tempdir().unwrap();
    let traffic = Arc::new(AtomicUsize::new(0));
    let workspace = remote::workspace::Workspace::new(remote::tests::connection(
        root.path(),
        Arc::default(),
        traffic.clone(),
    ));
    let path = workspace.target_key("file.txt").unwrap();
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    app.source_control_enabled = false;
    for state in 0..6 {
        let mut view = tab(&path);
        match state {
            0 => view.editor.insert_char('!'),
            1 => view.editor.bg_loading = true,
            2 => view.kind = TabKind::Diff(1),
            3 => view.kind = TabKind::GitRevision,
            4 => view.kind = TabKind::CommitDetails,
            _ => (),
        }
        app.tabs = if state == 5 { vec![] } else { vec![view] };
        let jobs = app.background.active_job_count();
        let bytes = traffic.load(Ordering::Relaxed);
        for _ in 0..100 {
            app.queue_file_reload(path.clone());
        }
        assert!(app.pending_file_reloads.is_empty(), "state={state}");
        assert!(app.pending_file_reload_again.is_empty(), "state={state}");
        assert_eq!(app.background.active_job_count(), jobs, "state={state}");
        assert_eq!(traffic.load(Ordering::Relaxed), bytes, "state={state}");
    }
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

fn stale_shared_view(typing_during_read: bool) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("file.txt"), "external change\n").unwrap();
    let hold = Arc::new(AtomicBool::new(false));
    let _release = Release(hold.clone());
    let blocked = Arc::new(AtomicBool::new(false));
    let workspace = remote::workspace::Workspace::new(remote::tests::connection_with_status(
        root.path(),
        hold.clone(),
        Arc::default(),
        blocked.clone(),
    ));
    let path = workspace.target_key("file.txt").unwrap();
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    app.source_control_enabled = false;
    app.tabs = vec![tab(&path), tab(&path)];
    app.sync_duplicate_file_tabs();
    app.create_split_group_dir(1, SplitDirection::Horizontal);
    if typing_during_read {
        hold.store(true, Ordering::Release);
        app.queue_file_reload(path.clone());
        until(|| blocked.load(Ordering::Acquire));
    }
    app.editor_mut().unwrap().insert_char('!');
    assert!(
        app.groups
            .iter()
            .flat_map(|group| &group.tabs)
            .any(|view| !view.editor.modified),
        "fixture requires a stale clean snapshot in another pane"
    );
    if !typing_during_read {
        app.queue_file_reload(path);
        assert!(
            app.pending_file_reloads.is_empty(),
            "a stale view must not cause a read of a dirty shared document"
        );
    }
    hold.store(false, Ordering::Release);
    until(|| {
        app.poll_background_results();
        app.pending_file_reloads.is_empty() && !app.background.has_active_jobs()
    });
    for view in app
        .tabs
        .iter_mut()
        .chain(app.groups.iter_mut().flat_map(|group| &mut group.tabs))
    {
        view.editor.refresh_document();
        assert_eq!(
            view.editor.rope, "!before\n",
            "a delayed reload must never overwrite a shared unsaved document"
        );
        assert!(view.editor.modified);
    }
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, "before\n");
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().redo();
    assert_eq!(app.editor().unwrap().rope, "!before\n");
    assert!(app.editor().unwrap().modified);
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn remote_reload_checks_shared_dirty_state_before_scheduling_a_read() {
    stale_shared_view(false);
}

#[test]
fn remote_reload_checks_shared_dirty_state_after_typing_during_a_delayed_read() {
    stale_shared_view(true);
}

#[test]
fn one_agent_edit_reloads_each_split_file_once_but_distinct_events_stay_ordered() {
    let root = tempfile::tempdir().unwrap();
    let native = root.path().join("file.txt");
    std::fs::write(&native, "first agent edit\n").unwrap();
    let hold = Arc::new(AtomicBool::new(false));
    let _release = Release(hold.clone());
    let blocked = Arc::new(AtomicBool::new(false));
    let workspace = remote::workspace::Workspace::new(remote::tests::connection_with_status(
        root.path(),
        hold.clone(),
        Arc::default(),
        blocked.clone(),
    ));
    let path = workspace.target_key("file.txt").unwrap();
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    app.source_control_enabled = false;
    // Keep Explorer outside this file-reload fixture. Its independent refresh
    // is not a second document change and must not influence the assertion.
    app.bg_explorer_refresh_running = true;
    app.tabs = vec![tab(&path), tab(&path)];
    app.sync_duplicate_file_tabs();
    app.create_split_group_dir(1, SplitDirection::Horizontal);
    let event = |after: &str| {
        vtcode::acp::Event::FileEdited(vtcode::acp::FileEdit {
            session_id: vtcode::acp::protocol::SessionId::new("edit"),
            path: path.clone(),
            before: "before\n".into(),
            after: after.into(),
        })
    };
    hold.store(true, Ordering::Release);
    app.apply_acp_event(event("first agent edit\n"));
    until(|| blocked.load(Ordering::Acquire));
    assert!(app.pending_file_reloads.contains(&path));
    assert!(
        app.pending_file_reload_again.is_empty(),
        "another view of the same edit must not force a second disk read"
    );
    // A genuinely later change still invalidates the in-flight first reply.
    // Event.after is deliberately older than the latest disk content: blindly
    // applying it would lose a shell/tool write that followed the agent write.
    std::fs::write(&native, "latest shell edit\n").unwrap();
    app.apply_acp_event(event("second agent edit\n"));
    assert!(app.pending_file_reload_again.contains(&path));
    hold.store(false, Ordering::Release);
    until(|| {
        app.poll_background_results();
        app.pending_file_reloads.is_empty() && !app.background.has_active_jobs()
    });
    for view in app
        .tabs
        .iter()
        .chain(app.groups.iter().flat_map(|group| &group.tabs))
    {
        assert_eq!(view.editor.rope, "latest shell edit\n");
        assert!(!view.editor.modified);
    }
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, "before\n");
    assert!(app.editor().unwrap().modified);
    app.editor_mut().unwrap().redo();
    assert_eq!(app.editor().unwrap().rope, "latest shell edit\n");
    assert!(!app.editor().unwrap().modified);
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn agent_content_edits_preserve_indexes_but_new_files_rules_and_metadata_sort_refresh() {
    let root = tempfile::tempdir().unwrap();
    let workspace = remote::workspace::Workspace::new(remote::tests::connection(
        root.path(),
        Arc::default(),
        Arc::default(),
    ));
    for hosted in [false, true] {
        let mut app = App::new_deferred(if hosted {
            workspace.view_root.clone()
        } else {
            root.path().into()
        });
        if hosted {
            app.remote_workspace = Some(workspace.clone());
        }
        app.workspace_watcher_active = true;
        app.bg_explorer_refresh_running = true;
        app.bg_git_status_running = true;
        for (name, before, sort, refresh, structural) in [
            (
                "code.rs",
                "old",
                crate::explorer::ExplorerSortMode::Name,
                false,
                false,
            ),
            (
                "code.rs",
                "old",
                crate::explorer::ExplorerSortMode::Modified,
                true,
                false,
            ),
            (
                "code.rs",
                "old",
                crate::explorer::ExplorerSortMode::Size,
                true,
                false,
            ),
            (
                "nested/.gitignore",
                "old",
                crate::explorer::ExplorerSortMode::Name,
                true,
                false,
            ),
            (
                "new/file.rs",
                "",
                crate::explorer::ExplorerSortMode::Name,
                true,
                true,
            ),
        ] {
            app.bg_explorer_refresh_pending = false;
            app.bg_git_status_pending = false;
            app.pending_git_status_paths = None;
            app.explorer.sort_mode = sort;
            let generation = app.workspace_file_index_generation;
            let path = if hosted {
                workspace.target_key(name).unwrap()
            } else {
                root.path().join(name)
            };
            app.apply_acp_event(vtcode::acp::Event::FileEdited(vtcode::acp::FileEdit {
                session_id: vtcode::acp::protocol::SessionId::new("edit"),
                path: path.clone(),
                before: before.into(),
                after: "new".into(),
            }));
            assert_eq!(
                app.bg_explorer_refresh_pending, refresh,
                "hosted={hosted}, name={name}, sort={sort:?}"
            );
            assert_eq!(
                app.workspace_file_index_generation,
                generation + u64::from(structural),
                "hosted={hosted}, name={name}"
            );
            assert!(app.bg_git_status_pending);
            if hosted {
                assert_eq!(
                    app.pending_git_status_paths.as_ref().unwrap(),
                    &std::iter::once(path).collect()
                );
            }
        }
    }
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
#[ignore = "Measures actual TCP bytes and reload latency for repeated 256 KiB agent edits"]
fn benchmark_remote_agent_reload_traffic() {
    let mut state = 0x78563412u32;
    let mut text = String::with_capacity(256 * 1024);
    for index in 0..256 * 1024 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        text.push(if index % 96 == 95 {
            '\n'
        } else {
            char::from(b'!' + (state % 90) as u8)
        });
    }
    for dirty in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let native = root.path().join("file.txt");
        std::fs::write(&native, "before\n").unwrap();
        let sent = Arc::new(AtomicUsize::new(0));
        let received = Arc::new(AtomicUsize::new(0));
        let workspace = remote::workspace::Workspace::new(remote::tests::connection_with_traffic(
            root.path(),
            Arc::default(),
            sent.clone(),
            Arc::default(),
            received.clone(),
        ));
        let path = workspace.target_key("file.txt").unwrap();
        let mut app = App::new_deferred(workspace.view_root.clone());
        app.remote_workspace = Some(workspace.clone());
        app.source_control_enabled = false;
        app.bg_explorer_refresh_running = true;
        app.tabs = vec![tab(&path), tab(&path)];
        if dirty {
            app.tabs[0].editor.insert_char('!');
        }
        app.sync_duplicate_file_tabs();
        app.create_split_group_dir(1, SplitDirection::Horizontal);
        let start_sent = sent.load(Ordering::Relaxed);
        let start_received = received.load(Ordering::Relaxed);
        let start = Instant::now();
        let mut previous = String::from("before\n");
        for index in 0..12 {
            text.replace_range(0..1, if index % 2 == 0 { "A" } else { "B" });
            std::fs::write(&native, &text).unwrap();
            app.apply_acp_event(vtcode::acp::Event::FileEdited(vtcode::acp::FileEdit {
                session_id: vtcode::acp::protocol::SessionId::new("edit"),
                path: path.clone(),
                before: previous,
                after: text.clone(),
            }));
            app.wait_for_background(Duration::from_secs(10), |app| {
                app.pending_file_reloads.is_empty()
            });
            let expected = if dirty { "!before\n" } else { text.as_str() };
            assert_eq!(app.editor().unwrap().rope, expected);
            assert_eq!(app.editor().unwrap().modified, dirty);
            previous = text.clone();
        }
        // Drain asynchronous transfer-release notifications before reading the
        // counters. The final ping is identical in both benchmark versions.
        workspace.client.call(remote::Command::Ping).unwrap();
        println!(
            "VTCODE_RELOAD {}",
            serde_json::json!({
                "dirty":dirty, "views":2, "edits":12, "document_bytes":text.len(),
                "elapsed_ms":start.elapsed().as_secs_f64()*1000.,
                "request_bytes":sent.load(Ordering::Relaxed)-start_sent,
                "response_bytes":received.load(Ordering::Relaxed)-start_received,
            })
        );
        workspace.client.call(remote::Command::Shutdown).unwrap();
    }
}
