use super::*;
use crate::{
    app::SplitDirection,
    background::{BackgroundResult, FilesystemActionOutcome, WorkspacePathChange},
    editor::Editor,
};
use std::time::{Duration, Instant, UNIX_EPOCH};

fn loaded(path: &Path) -> Tab {
    let mut editor = Editor::test_with_text("original\n");
    editor.file_path = Some(path.into());
    editor.saved_rope = Some(editor.rope.clone());
    editor.last_disk_modified = Some(UNIX_EPOCH + Duration::from_secs(1));
    editor.file_size = 9;
    Tab::new_file(editor)
}

fn missing(path: &Path) -> WorkspacePathChange {
    WorkspacePathChange {
        path: path.into(),
        observed_at: Instant::now(),
        exists: false,
        modified: None,
        len: None,
    }
}

fn send(app: &mut App, result: BackgroundResult) {
    app.background.sender.send(result).unwrap();
    app.poll_background_results();
}

#[test]
fn watcher_closes_pinned_duplicate_files_and_compacts_split_views() {
    let root = tempfile::tempdir().unwrap();
    let deleted = root.path().join("deleted.txt");
    let keep = root.path().join("keep.txt");
    let mut app = App::test_new();
    app.explorer.root = root.path().into();
    app.tabs = vec![loaded(&deleted), loaded(&keep), loaded(&deleted)];
    app.tabs[0].pinned = true;
    app.tabs[2].pinned = true;
    let keep_id = app.tabs[1].editor.navigation_id;
    app.create_split_group_dir(2, SplitDirection::Horizontal);
    assert!(app.split_active());
    let change = missing(&deleted);
    send(
        &mut app,
        BackgroundResult::FilesChanged {
            root: root.path().into(),
            changes: vec![change],
            structure_changed: true,
            rescan: false,
            git_metadata_changed: false,
        },
    );
    assert!(!app.split_active());
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].editor.navigation_id, keep_id);
    assert_eq!(app.active_tab, 0);
    assert!(!app.confirm_close_active);
    assert!(
        !app.closed_tab_paths
            .iter()
            .any(|(path, _, _)| path == &deleted)
    );
}

#[test]
fn explorer_directory_delete_removes_children_and_keeps_active_neighbour_and_snapshot_tabs() {
    let root = tempfile::tempdir().unwrap();
    let deleted = root.path().join("deleted");
    let keep = root.path().join("deleted-neighbour/keep.txt");
    let mut app = App::test_new();
    app.tabs = vec![
        loaded(&deleted.join("one.txt")),
        loaded(&deleted.join("nested/two.txt")),
        loaded(&keep),
    ];
    let mut snapshot = loaded(&deleted.join("one.txt"));
    snapshot.kind = TabKind::Diff(77);
    app.tabs.push(snapshot);
    app.active_tab = 2;
    app.tab_mru_order = vec![2, 0, 1];
    let keep_id = app.tabs[2].editor.navigation_id;
    send(
        &mut app,
        BackgroundResult::FilesystemActionDone {
            action: "Delete".into(),
            result: Ok(FilesystemActionOutcome {
                deleted: vec![deleted],
                message: "Deleted folder".into(),
                ..Default::default()
            }),
        },
    );
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.tabs[app.active_tab].editor.navigation_id, keep_id);
    assert!(matches!(app.tabs[1].kind, TabKind::Diff(77)));
    assert_eq!(app.tab_mru_order, vec![0]);
}

#[test]
fn externally_deleted_dirty_file_becomes_untitled_and_retains_text_cursor_and_undo() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("deleted.txt");
    let mut app = App::test_new();
    let mut tab = loaded(&path);
    tab.preview = true;
    tab.editor.insert_char('!');
    let text = tab.editor.rope.to_string();
    let cursor = (tab.editor.cursor_line, tab.editor.cursor_col);
    let id = tab.editor.navigation_id;
    app.tabs.push(tab);
    send(&mut app, BackgroundResult::FileDiskState(missing(&path)));
    assert_eq!(app.tabs.len(), 1);
    let editor = &mut app.tabs[0].editor;
    assert!(editor.file_path.is_none());
    assert_eq!(editor.navigation_id, id);
    assert_eq!(editor.rope.to_string(), text);
    assert_eq!((editor.cursor_line, editor.cursor_col), cursor);
    assert!(editor.modified);
    assert!(!editor.disk_unavailable);
    editor.undo();
    assert_eq!(editor.rope.to_string(), "original\n");
    assert!(
        editor.modified,
        "the recovered original is still unsaved in Untitled"
    );
    assert!(!app.tabs[0].preview);
}

#[test]
fn delayed_load_and_reload_results_cannot_reopen_deleted_tabs() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("deleted-while-loading.txt");
    let mut app = App::test_new();
    app.tabs.push(Tab::new_file(Editor::new_loading(&path, 9)));
    app.save_as_active = true;
    send(&mut app, BackgroundResult::FileDiskState(missing(&path)));
    assert!(app.tabs.is_empty());
    assert!(!app.save_as_active);
    send(
        &mut app,
        BackgroundResult::FileLoaded {
            workspace_generation: 0,
            path: path.clone(),
            rope: ropey::Rope::from_str("old read\n"),
            encoding: "UTF-8",
            file_size: 9,
            last_disk_modified: Some(UNIX_EPOCH),
            session_restores: vec![],
            skip_open_prompt: true,
        },
    );
    send(
        &mut app,
        BackgroundResult::FileReloaded {
            targets: Vec::new(),
            workspace_generation: 0,
            path,
            result: Ok(crate::editor::LoadedTextFile {
            remote_checkpoint: None,
                rope: ropey::Rope::from_str("old read\n"),
                encoding: "UTF-8",
                file_size: 9,
                last_disk_modified: Some(UNIX_EPOCH),
            }),
        },
    );
    assert!(app.tabs.is_empty());
}

#[test]
fn a_missing_probe_during_a_save_waits_and_a_failed_save_rechecks_deletion() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("deleted-during-save.txt");
    let mut app = App::test_new();
    app.tabs.push(loaded(&path));
    app.pending_file_saves.insert(path.clone(), (app.workspace_generation, 0.into()));
    send(&mut app, BackgroundResult::FileDiskState(missing(&path)));
    assert_eq!(app.tabs.len(), 1);
    send(
        &mut app,
        BackgroundResult::FileSaved {
            source_document_id: 0,
            previous_disk: None,
            workspace_generation: 0,
            path,
            error: Some(std::io::Error::other("failed save")),
            last_disk_modified: None,
            file_size: None,
            source_content_version: 0,
            source_rope: ropey::Rope::from_str("original\n"),
            saved_encoding: "UTF-8",
            transformed_lines: None,
            disk_written: false,
            saved_rope: None,
        },
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !app.tabs.is_empty() {
        app.poll_background_results();
        assert!(
            Instant::now() < deadline,
            "missing file was not checked after the failed save"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_stale_missing_probe_after_successful_save_does_not_close_the_file() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("saved.txt");
    std::fs::write(&path, "original\n").unwrap();
    let mut app = App::test_new();
    app.tabs.push(loaded(&path));
    let stale = missing(&path);
    send(
        &mut app,
        BackgroundResult::FileSaved {
            source_document_id: 0,
            previous_disk: None,
            workspace_generation: 0,
            path: path.clone(),
            error: None,
            last_disk_modified: Some(UNIX_EPOCH + Duration::from_secs(1)),
            file_size: Some(9),
            source_content_version: 0,
            source_rope: ropey::Rope::from_str("original\n"),
            saved_encoding: "UTF-8",
            transformed_lines: None,
            disk_written: true,
            saved_rope: Some(ropey::Rope::from_str("original\n")),
        },
    );
    send(&mut app, BackgroundResult::FileDiskState(stale));
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].editor.file_path.as_ref(), Some(&path));
    assert!(!app.tabs[0].editor.modified);
}

#[test]
fn never_saved_named_draft_is_not_an_external_deletion() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("new-file.txt");
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text("new unsaved work");
    editor.file_path = Some(path.clone());
    editor.modified = true;
    app.tabs.push(Tab::new_file(editor));
    send(&mut app, BackgroundResult::FileDiskState(missing(&path)));
    assert_eq!(app.tabs[0].editor.file_path.as_ref(), Some(&path));
    assert!(app.notification_history.is_empty());
}

#[test]
fn absolute_deletion_events_close_tabs_opened_with_relative_paths() {
    let relative = Path::new("./vtcode-deleted-relative-test.txt");
    let absolute = std::path::absolute(relative).unwrap();
    let mut app = App::test_new();
    app.tabs.push(loaded(relative));
    send(
        &mut app,
        BackgroundResult::FileDiskState(missing(&absolute)),
    );
    assert!(app.tabs.is_empty());
}

#[cfg(windows)]
#[test]
fn deletion_events_close_tabs_with_verbatim_or_differently_cased_windows_paths() {
    let root = tempfile::tempdir().unwrap();
    // Keep the same directory spelling: this test varies case and verbatim prefixes,
    // not the short-path aliases that Windows may use for its temporary directory.
    let canonical_root = root.path().canonicalize().unwrap();
    let ordinary_root = PathBuf::from(
        canonical_root
            .to_string_lossy()
            .replace(r"\\?\UNC\", r"\\")
            .trim_start_matches(r"\\?\"),
    );
    let deleted = ordinary_root.join("deleted.txt");
    let alias = PathBuf::from(
        canonical_root
            .join("DELETED.TXT")
            .to_string_lossy()
            .to_uppercase(),
    );
    let mut app = App::test_new();
    app.tabs.push(loaded(&alias));
    send(&mut app, BackgroundResult::FileDiskState(missing(&deleted)));
    assert!(app.tabs.is_empty());
}
