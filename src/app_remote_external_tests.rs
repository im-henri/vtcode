use super::*;
#[path = "app_remote_save_tests.rs"]
mod saves;
use crate::{
    background::{BackgroundResult, WorkspacePathChange},
    remote,
};
use std::{
    sync::Arc,
    time::{Duration, Instant, UNIX_EPOCH},
};

fn setup(root: &Path) -> (App, remote::workspace::Workspace) {
    let workspace = remote::workspace::Workspace::new(remote::tests::connection(
        root,
        Arc::default(),
        Arc::default(),
    ));
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    app.source_control_enabled = false;
    // These tests exercise document state, not the independent Explorer scan.
    app.bg_explorer_refresh_running = true;
    (app, workspace)
}

fn loaded(path: &Path) -> Tab {
    let mut editor = Editor::test_with_text("before\n");
    editor.file_path = Some(path.into());
    editor.saved_rope = Some(editor.rope.clone());
    editor.last_disk_modified = Some(UNIX_EPOCH);
    Tab::new_file(editor)
}

fn event(app: &mut App, change: WorkspacePathChange) {
    app.background
        .sender
        .send(BackgroundResult::FilesChanged {
            root: app.explorer.root.clone(),
            changes: vec![change],
            structure_changed: false,
            rescan: false,
            git_metadata_changed: false,
        })
        .unwrap();
    app.poll_background_results();
}

#[cfg(windows)]
#[test]
fn ssh_external_delete_keeps_case_distinct_linux_file_and_navigation() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, workspace) = setup(root.path());
    let lower = workspace.target_key("case.rs").unwrap();
    let upper = workspace.target_key("CASE.rs").unwrap();
    app.tabs = vec![loaded(&lower), loaded(&upper)];
    assert!(app.close_deleted_file_tabs(&lower, false));
    assert_eq!(
        app.tabs.len(),
        1,
        "a Linux filename must not be lowercased by a Windows client"
    );
    assert_eq!(app.tabs[0].editor.file_path.as_ref(), Some(&upper));
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn ssh_external_delete_recovers_all_shared_dirty_views_and_shared_undo() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, workspace) = setup(root.path());
    let path = workspace.target_key("file.txt").unwrap();
    app.tabs = vec![loaded(&path), loaded(&path)];
    app.sync_duplicate_file_tabs();
    app.create_split_group_dir(1, SplitDirection::Horizontal);
    app.editor_mut().unwrap().insert_char('!');
    assert!(
        app.groups
            .iter()
            .flat_map(|g| &g.tabs)
            .any(|tab| !tab.editor.modified),
        "fixture requires a stale clean view"
    );
    assert!(app.close_deleted_file_tabs(&path, false));
    assert_eq!(
        app.tabs.len() + app.groups.iter().map(|g| g.tabs.len()).sum::<usize>(),
        2,
        "deletion must not close a stale view of a dirty shared document"
    );
    app.editor_mut().unwrap().undo();
    for tab in app
        .tabs
        .iter_mut()
        .chain(app.groups.iter_mut().flat_map(|g| &mut g.tabs))
    {
        tab.editor.refresh_document();
        assert!(
            tab.editor.file_path.is_none(),
            "a later shared refresh must not resurrect the deleted path"
        );
        assert_eq!(tab.editor.rope, "before\n");
        assert!(
            tab.editor.modified,
            "recovered text is unsaved even at its old disk baseline"
        );
    }
    app.editor_mut().unwrap().redo();
    assert_eq!(app.editor().unwrap().rope, "!before\n");
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn ssh_external_delayed_missing_notification_cannot_close_a_successfully_saved_file() {
    let root = tempfile::tempdir().unwrap();
    let native = root.path().join("file.txt");
    std::fs::write(&native, "before\n").unwrap();
    let (mut app, workspace) = setup(root.path());
    let path = workspace.target_key("file.txt").unwrap();
    app.trim_trailing_on_save = false;
    app.insert_final_newline = false;
    app.open_file(&path);
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    std::fs::remove_file(&native).unwrap();
    let stale = remote::watch::State::read(
        root.path(),
        remote::RemotePath::new(b"file.txt".to_vec()).unwrap(),
    )
    .unwrap();
    assert!(!stale.exists);
    std::fs::write(&native, "before\n").unwrap();
    app.editor_mut().unwrap().insert_char('!');
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.pending_file_saves.is_empty()
    });
    assert_eq!(std::fs::read_to_string(&native).unwrap(), "!before\n");
    event(
        &mut app,
        WorkspacePathChange {
            path: path.clone(),
            observed_at: Instant::now(),
            exists: stale.exists,
            modified: stale.modified,
            len: stale.size,
        },
    );
    assert_eq!(
        app.tabs.len(),
        1,
        "notification receipt time does not prove the file is still missing"
    );
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.pending_disk_checks.is_empty()
    });
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&path));
    assert_eq!(app.editor().unwrap().rope, "!before\n");
    assert!(!app.editor().unwrap().modified);
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn ssh_external_write_with_preserved_mtime_and_size_reloads_clean_text() {
    let root = tempfile::tempdir().unwrap();
    let native = root.path().join("file.txt");
    std::fs::write(&native, "before\n").unwrap();
    let (mut app, workspace) = setup(root.path());
    let path = workspace.target_key("file.txt").unwrap();
    app.open_file(&path);
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    let modified = app.editor().unwrap().last_disk_modified.unwrap();
    std::fs::write(&native, "AFTER!\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&native)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(
        std::fs::metadata(&native).unwrap().modified().unwrap(),
        modified
    );
    event(
        &mut app,
        WorkspacePathChange {
            path,
            observed_at: Instant::now(),
            exists: true,
            modified: Some(modified),
            len: Some(7),
        },
    );
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.editor().unwrap().rope == "AFTER!\n"
    });
    assert_eq!(
        app.editor().unwrap().rope,
        "AFTER!\n",
        "a write event is not disproved by equal metadata"
    );
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, "before\n");
    assert!(app.editor().unwrap().modified);
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn ssh_external_unchanged_hints_preserve_dirty_selection_history_and_do_not_transfer_bodies() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let root = tempfile::tempdir().unwrap();
    let text = "unchanged λ 🦀\r\n".repeat(80_000);
    std::fs::write(root.path().join("file.txt"), &text).unwrap();
    let incoming = Arc::new(AtomicUsize::new(0));
    let workspace = remote::workspace::Workspace::new(remote::tests::connection_with_traffic(
        root.path(),
        Arc::default(),
        Arc::default(),
        Arc::default(),
        incoming.clone(),
    ));
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.source_control_enabled = false;
    app.bg_explorer_refresh_running = true;
    app.remote_workspace = Some(workspace.clone());
    let path = workspace.target_key("file.txt").unwrap();
    app.open_file(&path);
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    let editor = app.editor_mut().unwrap();
    editor.insert_char('!');
    editor.selection_anchor = Some((0, 0));
    let rope = editor.rope.clone();
    let version = editor.content_version;
    let history = editor.undo_stack.len();
    let before = incoming.load(Ordering::Relaxed);
    for _ in 0..32 {
        app.queue_remote_file_change(path.clone());
    }
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.pending_file_reloads.is_empty()
    });
    let editor = app.editor().unwrap();
    assert!(editor.rope.is_instance(&rope));
    assert_eq!(editor.content_version, version);
    assert_eq!(editor.undo_stack.len(), history);
    assert_eq!(editor.selection_anchor, Some((0, 0)));
    assert!(
        incoming.load(Ordering::Relaxed) - before < 4096,
        "unchanged multi-megabyte text must stay on the node"
    );
    assert!(
        !app.notification_history
            .iter()
            .any(|entry| entry.0.contains("changed on disk"))
    );
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, text);
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn ssh_external_same_metadata_dirty_edit_warns_once_and_save_never_overwrites() {
    let root = tempfile::tempdir().unwrap();
    let native = root.path().join("file.txt");
    std::fs::write(&native, "before\n").unwrap();
    let (mut app, workspace) = setup(root.path());
    let path = workspace.target_key("file.txt").unwrap();
    app.open_file(&path);
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    let modified = app.editor().unwrap().last_disk_modified.unwrap();
    app.editor_mut().unwrap().insert_char('!');
    std::fs::write(&native, "AFTER!\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&native)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(modified))
        .unwrap();
    for _ in 0..3 {
        app.queue_remote_file_change(path.clone());
        app.wait_for_background(Duration::from_secs(5), |app| {
            app.pending_file_reloads.is_empty()
        });
    }
    assert_eq!(
        app.notification_history
            .iter()
            .filter(|entry| entry.0.contains("changed on disk"))
            .count(),
        1
    );
    assert_eq!(app.editor().unwrap().rope, "!before\n");
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.pending_file_saves.is_empty()
    });
    assert!(app.save_error.is_some());
    assert_eq!(std::fs::read_to_string(native).unwrap(), "AFTER!\n");
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, "before\n");
    app.editor_mut().unwrap().redo();
    assert_eq!(app.editor().unwrap().rope, "!before\n");
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn ssh_external_replacement_by_directory_reports_error_without_a_reload_loop() {
    let root = tempfile::tempdir().unwrap();
    let native = root.path().join("file.txt");
    std::fs::write(&native, "before\n").unwrap();
    let (mut app, workspace) = setup(root.path());
    let path = workspace.target_key("file.txt").unwrap();
    app.open_file(&path);
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    std::fs::remove_file(&native).unwrap();
    std::fs::create_dir(&native).unwrap();
    app.queue_remote_file_change(path.clone());
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.pending_file_reloads.is_empty() && !app.background.has_active_jobs()
    });
    for _ in 0..20 {
        app.poll_background_results();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(app.pending_file_reloads.is_empty() && app.pending_disk_checks.is_empty());
    assert_eq!(app.editor().unwrap().rope, "before\n");
    assert_eq!(
        app.notification_history
            .iter()
            .filter(|entry| entry.0.contains("Could not reload"))
            .count(),
        1
    );
    std::fs::remove_dir(&native).unwrap();
    std::fs::write(&native, "restored\n").unwrap();
    app.queue_remote_file_change(path);
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.editor().unwrap().rope == "restored\n"
    });
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn ssh_external_late_validation_after_close_and_reopen_cannot_warn_for_new_document() {
    let root = tempfile::tempdir().unwrap();
    let native = root.path().join("file.txt");
    std::fs::write(&native, "before\n").unwrap();
    let (mut app, workspace) = setup(root.path());
    let path = workspace.target_key("file.txt").unwrap();
    app.open_file(&path);
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    let editor = app.editor().unwrap();
    let document = editor.save_identity;
    let baseline = remote::workspace::Baseline {
        checkpoint: editor.remote_checkpoint.clone().unwrap(),
        rope: editor.saved_rope.clone().unwrap(),
        encoding: editor.saved_encoding,
    };
    app.force_close_tab(0);
    app.open_file(&path);
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    let messages = app.notification_history.len();
    app.finish_remote_file_validation(
        app.workspace_generation,
        path,
        document,
        baseline,
        Err("old connection failed".into()),
    );
    assert_eq!(app.notification_history.len(), messages);
    assert!(app.pending_disk_checks.is_empty());
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn ssh_external_hint_during_save_revalidates_the_new_baseline_without_losing_new_typing() {
    use std::sync::atomic::{AtomicBool, Ordering};
    struct Release(Arc<AtomicBool>);
    impl Drop for Release {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let root = tempfile::tempdir().unwrap();
    let native = root.path().join("file.txt");
    std::fs::write(&native, "before\n").unwrap();
    let hold = Arc::new(AtomicBool::new(false));
    let _release = Release(hold.clone());
    let blocked = Arc::new(AtomicBool::new(false));
    let workspace = remote::workspace::Workspace::new(remote::tests::connection_with_status(
        root.path(),
        hold.clone(),
        Arc::default(),
        blocked.clone(),
    ));
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    app.source_control_enabled = false;
    app.bg_explorer_refresh_running = true;
    let path = workspace.target_key("file.txt").unwrap();
    app.open_file(&path);
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.editor().is_some_and(|e| !e.bg_loading)
    });
    app.editor_mut().unwrap().insert_char('A');
    app.editor_mut().unwrap().break_undo_group();
    hold.store(true, Ordering::Release);
    app.queue_remote_file_change(path.clone());
    app.wait_for_background(Duration::from_secs(5), |_| blocked.load(Ordering::Acquire));
    app.save_current_file();
    app.queue_remote_file_change(path.clone());
    app.editor_mut().unwrap().insert_char('B');
    app.editor_mut().unwrap().break_undo_group();
    hold.store(false, Ordering::Release);
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.pending_file_saves.is_empty()
            && app.pending_file_reloads.is_empty()
            && app.remote_changes_deferred.is_empty()
    });
    assert_eq!(app.editor().unwrap().rope, "ABbefore\n");
    assert_eq!(std::fs::read_to_string(native).unwrap(), "Abefore\n");
    assert!(app.editor().unwrap().modified);
    assert!(
        !app.notification_history
            .iter()
            .any(|entry| entry.0.contains("changed on disk"))
    );
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, "Abefore\n");
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, "before\n");
    assert!(app.editor().unwrap().modified);
    app.editor_mut().unwrap().redo();
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().redo();
    assert!(app.editor().unwrap().modified);
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn ssh_external_conditional_reload_preserves_bom_encoding_and_detects_encoding_only_changes() {
    let root = tempfile::tempdir().unwrap();
    let workspace = remote::workspace::Workspace::new(remote::tests::connection(
        root.path(),
        Arc::default(),
        Arc::default(),
    ));
    let text = "λ 🦀\r\nsecond\n";
    for encoding in ["UTF-8", "UTF-8 with BOM", "UTF-16 LE", "UTF-16 BE"] {
        let bytes = match encoding {
            "UTF-8 with BOM" => [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat(),
            "UTF-16 LE" => [
                vec![0xff, 0xfe],
                text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            ]
            .concat(),
            "UTF-16 BE" => [
                vec![0xfe, 0xff],
                text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
            ]
            .concat(),
            _ => text.as_bytes().to_vec(),
        };
        let native = root.path().join("file.txt");
        std::fs::write(&native, &bytes).unwrap();
        let key = workspace.target_key("file.txt").unwrap();
        let old = workspace.load(&key).unwrap();
        let baseline = remote::workspace::Baseline {
            checkpoint: old.remote_checkpoint.clone().unwrap(),
            rope: old.rope.clone(),
            encoding: old.encoding,
        };
        let unchanged = workspace.reload(&key, Some(&baseline)).unwrap();
        assert!(unchanged.rope.is_instance(&old.rope), "{encoding}");
        assert_eq!(unchanged.encoding, encoding);
        if encoding != "UTF-8" {
            std::fs::write(&native, text).unwrap();
            assert!(workspace.validate_saved(&key, &baseline).unwrap().changed);
            let changed = workspace.reload(&key, Some(&baseline)).unwrap();
            assert_eq!(changed.encoding, "UTF-8");
            assert_eq!(changed.rope, text);
        }
    }
    workspace.client.call(remote::Command::Shutdown).unwrap();
}

#[test]
fn ssh_external_parent_replacement_and_watcher_overflow_revalidate_open_children() {
    for overflow in [false, true] {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("nested")).unwrap();
        let native = root.path().join("nested/file.txt");
        std::fs::write(&native, "before\n").unwrap();
        let (mut app, workspace) = setup(root.path());
        let path = workspace.target_key("nested/file.txt").unwrap();
        app.open_file(&path);
        app.wait_for_background(Duration::from_secs(5), |app| {
            app.editor().is_some_and(|e| !e.bg_loading)
        });
        let modified = app.editor().unwrap().last_disk_modified.unwrap();
        std::fs::rename(root.path().join("nested"), root.path().join("old-tree")).unwrap();
        std::fs::create_dir(root.path().join("nested")).unwrap();
        std::fs::write(&native, "AFTER!\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&native)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        let changes = if overflow {
            vec![]
        } else {
            vec![WorkspacePathChange {
                path: workspace.target_key("nested").unwrap(),
                observed_at: Instant::now(),
                exists: true,
                modified: None,
                len: None,
            }]
        };
        app.background
            .sender
            .send(BackgroundResult::FilesChanged {
                root: app.explorer.root.clone(),
                changes,
                structure_changed: true,
                rescan: overflow,
                git_metadata_changed: false,
            })
            .unwrap();
        app.wait_for_background(Duration::from_secs(5), |app| {
            app.editor().unwrap().rope == "AFTER!\n"
        });
        app.editor_mut().unwrap().undo();
        assert_eq!(app.editor().unwrap().rope, "before\n");
        assert!(app.editor().unwrap().modified);
        workspace.client.call(remote::Command::Shutdown).unwrap();
    }
}
