use crate::app::{App, SplitDirection, Tab};
use crate::editor::Editor;
use crate::lsp::{LspClient, LspManager, TestLspServer};
use std::path::{Path, PathBuf};

fn tab(path: &Path) -> Tab {
    let mut editor = Editor::test_with_text("value = 1\n");
    editor.file_path = Some(path.to_path_buf());
    Tab::new_file(editor)
}

fn two_views() -> (App, TestLspServer, tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.py");
    let mut app = App::test_new();
    app.tabs = vec![tab(&path), tab(&path)];
    app.active_tab = 0;
    app.lsp_manager = LspManager::new(dir.path().to_path_buf());
    let (client, server) = LspClient::test_pair("python", &["*.py"]);
    app.lsp_manager.clients.push(client);
    app.lsp_manager
        .sync_document_owned(&path, 1, "value = 1\n".into());
    server.take_messages();
    (app, server, dir, path)
}

fn close_count(server: &TestLspServer) -> usize {
    server
        .take_messages()
        .iter()
        .filter(|message| message["method"] == "textDocument/didClose")
        .count()
}

const BULK_CLOSE: [fn(&mut App); 2] = [App::close_saved_tabs, |app| {
    app.execute_palette_command("tab_close_all");
}];

#[test]
fn closing_one_split_keeps_the_shared_lsp_document_open() {
    let (mut app, server, _dir, path) = two_views();
    app.create_split_group_dir(1, SplitDirection::Horizontal);
    app.force_close_tab(0);
    assert_eq!(close_count(&server), 0);
    assert!(app.lsp_manager.clients[0].is_file_open(&path));
    app.force_close_tab(0);
    assert_eq!(close_count(&server), 1);
    assert!(!app.lsp_manager.clients[0].is_file_open(&path));
}

#[test]
fn closing_a_duplicate_in_the_same_group_keeps_lsp_open() {
    let (mut app, server, _dir, path) = two_views();
    app.force_close_tab(0);
    assert_eq!(close_count(&server), 0);
    assert!(app.lsp_manager.clients[0].is_file_open(&path));
    app.force_close_tab(0);
    assert_eq!(close_count(&server), 1);
}

#[test]
fn bulk_close_keeps_lsp_for_pinned_or_modified_split_views() {
    for close in BULK_CLOSE {
        for kept_in_active_group in [false, true] {
            for pinned in [false, true] {
                let (mut app, server, _dir, path) = two_views();
                // A retained tab in each group exercises both closing loops.
                let mut retained = tab(&path.with_file_name("retained.py"));
                retained.pinned = true;
                app.tabs.push(retained);
                let kept = usize::from(kept_in_active_group);
                app.tabs[kept].pinned = pinned;
                app.tabs[kept].editor.modified = !pinned;
                app.create_split_group_dir(1, SplitDirection::Horizontal);
                if !kept_in_active_group {
                    // Keep this group alive while its duplicate is removed.
                    let mut retained = tab(&path.with_file_name("other.py"));
                    retained.pinned = true;
                    app.tabs.push(retained);
                }
                close(&mut app);
                assert_eq!(
                    close_count(&server),
                    0,
                    "active={kept_in_active_group}, pinned={pinned}"
                );
                assert!(app.lsp_manager.clients[0].is_file_open(&path));
            }
        }
    }
}

#[test]
fn bulk_close_preserves_the_selected_tab_in_inactive_groups() {
    for close in BULK_CLOSE {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::test_new();
        app.tabs = ["saved.py", "selected.py", "pinned.py", "other.py"]
            .iter()
            .map(|name| tab(&dir.path().join(name)))
            .collect();
        for index in [1, 2, 3] {
            app.tabs[index].pinned = true;
        }
        app.active_tab = 1;
        app.create_split_group_dir(3, SplitDirection::Horizontal);
        close(&mut app);
        app.switch_group(0);
        assert_eq!(app.editor().unwrap().file_name(), "selected.py");
    }
}

#[test]
fn bulk_close_removes_empty_inactive_panes() {
    for close in BULK_CLOSE {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::test_new();
        app.tabs = vec![
            tab(&dir.path().join("saved.py")),
            tab(&dir.path().join("kept.py")),
        ];
        app.tabs[1].pinned = true;
        app.create_split_group_dir(1, SplitDirection::Horizontal);
        close(&mut app);
        assert!(!app.split_active());
        assert_eq!(app.editor().unwrap().file_name(), "kept.py");
    }
}

#[test]
fn bulk_close_sends_one_notification_when_the_last_view_closes() {
    for close in BULK_CLOSE {
        let (mut app, server, _dir, path) = two_views();
        app.create_split_group_dir(1, SplitDirection::Horizontal);
        close(&mut app);
        assert_eq!(close_count(&server), 1);
        assert!(!app.lsp_manager.clients[0].is_file_open(&path));
        assert!(app.tabs.is_empty());
        assert!(!app.split_active());
    }
}

#[test]
fn batch_closing_distinct_files_keeps_shared_pinned_documents_open() {
    for close in BULK_CLOSE {
        let (mut app, server, dir, kept) = two_views();
        app.tabs[0].pinned = true;
        app.create_split_group_dir(1, SplitDirection::Horizontal);
        let mut paths = vec![kept.clone()];
        for i in 0..8 {
            let path = dir.path().join(format!("closed-{i}.py"));
            app.lsp_manager.sync_document_owned(&path, 1, "value = 1\n".into());
            app.tabs.push(tab(&path));
            app.groups[0].tabs.push(tab(&path));
            paths.push(path);
        }
        server.take_messages();
        close(&mut app);
        assert_eq!(close_count(&server), 8);
        for path in paths {
            assert_eq!(app.lsp_manager.clients[0].is_file_open(&path), path == kept);
        }
        assert!(!app.split_active());
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&kept));
    }
}

#[test]
fn queued_lsp_edits_still_reach_the_surviving_view() {
    let (mut app, server, _dir, path) = two_views();
    app.tabs[1].editor.content_version = 2;
    let edited = ropey::Rope::from_str("value = 2\n");
    app.tabs[1].editor.rope = edited.clone();
    app.queue_lsp_document_sync(path.clone(), 2, edited);
    app.force_close_tab(0);
    assert_eq!(close_count(&server), 0);
    app.flush_lsp_document_syncs();
    let messages = server.take_messages();
    assert!(
        messages
            .iter()
            .any(|message| message["method"] == "textDocument/didChange")
    );
    assert!(
        !messages
            .iter()
            .any(|message| message["method"] == "textDocument/didOpen")
    );
    app.force_close_tab(0);
    assert_eq!(close_count(&server), 1);
}
