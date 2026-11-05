use super::*;
use crate::{
    app::Tab,
    editor::Editor,
    lsp::{LspClient, TestLspServer},
};
use serde_json::json;

fn fixture() -> (App, TestLspServer, tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("open.py");
    std::fs::write(&path, "value = 1\n").unwrap();
    let mut app = App::test_new();
    app.explorer = crate::explorer::FileExplorer::new_deferred(temp.path().to_path_buf());
    app.lsp_manager = LspManager::new(temp.path().to_path_buf());
    let (client, server) = LspClient::test_pair("python", &["*.py"]);
    app.lsp_manager.clients.push(client);
    let mut editor = Editor::test_with_text("value = 1\n");
    editor.file_path = Some(path.clone());
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.format_on_save = false;
    (app, server, temp, path)
}

fn workspace(paths: &[PathBuf]) -> WorkspaceEdit {
    WorkspaceEdit {
        changes: Some(
            paths
                .iter()
                .map(|path| {
                    (
                        lsp_types::Url::from_file_path(path).unwrap(),
                        vec![TextEdit::new(
                            lsp_types::Range::new(
                                lsp_types::Position::new(0, 0),
                                lsp_types::Position::new(0, 5),
                            ),
                            "renamed".into(),
                        )],
                    )
                })
                .collect(),
        ),
        ..Default::default()
    }
}

#[test]
fn workspace_edits_wait_for_unopened_files_and_leave_disk_untouched() {
    let (mut app, _server, temp, path) = fixture();
    let closed = temp.path().join("closed.py");
    std::fs::write(&closed, "value = 2\n").unwrap();
    app.queue_lsp_workspace_edit(
        workspace(&[path.clone(), closed.clone()]),
        "Rename".into(),
        None,
        None,
    );
    assert!(!app.poll_lsp_workspace_edits());
    assert_eq!(
        app.find_editor_mut_by_path(&path).unwrap().rope.to_string(),
        "value = 1\n"
    );
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.tabs.iter().all(|tab| !tab.editor.bg_loading)
    });
    assert!(app.poll_lsp_workspace_edits());
    for (path, original) in [(&path, "value = 1\n"), (&closed, "value = 2\n")] {
        let editor = app.find_editor_mut_by_path(path).unwrap();
        assert_eq!(
            editor.rope.to_string(),
            original.replace("value", "renamed")
        );
        editor.undo();
        assert_eq!(editor.rope.to_string(), original);
        assert_eq!(std::fs::read_to_string(path).unwrap(), original);
    }
}

#[test]
fn workspace_edits_reject_changes_during_loading_without_partial_application() {
    let (mut app, _server, temp, path) = fixture();
    let closed = temp.path().join("closed.py");
    std::fs::write(&closed, "value = 2\n").unwrap();
    app.queue_lsp_workspace_edit(
        workspace(&[path.clone(), closed.clone()]),
        "Rename".into(),
        None,
        None,
    );
    let editor = app.find_editor_mut_by_path(&path).unwrap();
    editor.save_undo_public();
    editor.rope.insert(0, "# user edit\n");
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.tabs.iter().all(|tab| !tab.editor.bg_loading)
    });
    assert!(app.poll_lsp_workspace_edits());
    assert_eq!(
        app.find_editor_mut_by_path(&closed)
            .unwrap()
            .rope
            .to_string(),
        "value = 2\n"
    );
    assert!(
        app.find_editor_mut_by_path(&path)
            .unwrap()
            .rope
            .to_string()
            .starts_with("# user edit")
    );
}

#[test]
fn invalid_later_workspace_edit_target_keeps_earlier_documents_unchanged() {
    let (mut app, server, temp, path) = fixture();
    let broken_path = temp.path().join("broken.py");
    let mut broken = Editor::test_with_text("value = 2\n");
    broken.file_path = Some(broken_path.clone());
    broken.load_failed = true;
    broken.publish_document();
    app.tabs.push(Tab::new_file(broken));
    let mut edits = workspace(&[path.clone(), broken_path.clone()])
        .changes
        .unwrap();
    let changes: Vec<_> = [&path, &broken_path]
        .into_iter()
        .map(|path| {
            let uri = lsp_types::Url::from_file_path(path).unwrap();
            let edits = edits.remove(&uri).unwrap();
            json!({"textDocument": {"uri": uri, "version": null}, "edits": edits})
        })
        .collect();
    let edit = serde_json::from_value(json!({"documentChanges": changes})).unwrap();
    let process = app.lsp_manager.clients[0].child.id();
    app.queue_lsp_workspace_edit(edit, "Rename".into(), Some((process, json!(7))), None);
    assert!(app.poll_lsp_workspace_edits());
    assert_eq!(server.take_messages()[0]["result"]["applied"], false);
    assert_eq!(app.tabs[0].editor.rope, "value = 1\n");
    assert!(!app.tabs[0].editor.modified);
}

#[test]
fn workspace_edit_rejects_reopened_document_with_same_content_version() {
    let (mut app, server, _temp, path) = fixture();
    let process = app.lsp_manager.clients[0].child.id();
    app.queue_lsp_workspace_edit(
        workspace(std::slice::from_ref(&path)),
        "Rename".into(),
        Some((process, json!(7))),
        None,
    );
    let version = app.tabs[0].editor.content_version;
    let mut replacement = Editor::test_with_text("value = 9\n");
    replacement.file_path = Some(path);
    replacement.content_version = version;
    replacement.publish_document();
    app.tabs[0] = Tab::new_file(replacement);

    assert!(app.poll_lsp_workspace_edits());
    assert_eq!(server.take_messages()[0]["result"]["applied"], false);
    assert_eq!(app.tabs[0].editor.rope, "value = 9\n");
}

#[cfg(windows)]
#[test]
fn workspace_edit_rejects_duplicate_windows_paths_to_the_same_editor() {
    let (mut app, server, _temp, path) = fixture();
    let alias = path.with_file_name("OPEN.PY");
    let process = app.lsp_manager.clients[0].child.id();
    app.queue_lsp_workspace_edit(
        workspace(&[path, alias]),
        "Rename".into(),
        Some((process, json!(7))),
        None,
    );
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.tabs.iter().all(|tab| !tab.editor.bg_loading)
    });
    assert!(app.poll_lsp_workspace_edits());
    assert_eq!(server.take_messages()[0]["result"]["applied"], false);
    assert_eq!(app.tabs[0].editor.rope, "value = 1\n");
}

#[cfg(windows)]
#[test]
fn workspace_edit_targets_the_exact_case_with_a_different_sibling_active() {
    let (mut app, server, temp, _) = fixture();
    let folder = temp.path().join("case-sensitive");
    std::fs::create_dir(&folder).unwrap();
    if !crate::native_paths::enable_case_sensitive_test_directory(&folder) {
        return;
    }
    let upper = folder.join("Script.py");
    let lower = folder.join("script.py");
    std::fs::write(&upper, "value = 1\n").unwrap();
    std::fs::write(&lower, "value = 2\n").unwrap();
    app.open_file(&upper);
    app.open_file(&lower);
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.tabs.iter().all(|tab| !tab.editor.bg_loading)
    });
    assert_eq!(app.editor().unwrap().file_path.as_deref(), Some(lower.as_path()));
    let process = app.lsp_manager.clients[0].child.id();
    app.queue_lsp_workspace_edit(
        workspace(std::slice::from_ref(&upper)),
        "Rename".into(),
        Some((process, json!(7))),
        None,
    );
    assert!(app.poll_lsp_workspace_edits());
    assert_eq!(server.take_messages().into_iter().find(|message| message["id"] == 7).unwrap()["result"]["applied"], true);
    assert_eq!(app.find_editor_mut_by_path(&upper).unwrap().rope, "renamed = 1\n");
    assert_eq!(app.find_editor_mut_by_path(&lower).unwrap().rope, "value = 2\n");
}

#[test]
fn active_revision_tab_cannot_intercept_an_edit_to_the_worktree_file() {
    let (mut app, _server, _temp, path) = fixture();
    let mut history = Editor::test_with_text("value = 0\n");
    history.file_path = Some(path.clone());
    history.read_only = true;
    let mut tab = Tab::new_file(history);
    tab.kind = crate::app::TabKind::GitRevision;
    app.tabs.push(tab);
    app.active_tab = 1;

    app.queue_lsp_workspace_edit(workspace(&[path]), "Rename".into(), None, None);
    assert!(app.poll_lsp_workspace_edits());
    assert_eq!(app.tabs[0].editor.rope, "renamed = 1\n");
    assert_eq!(app.tabs[1].editor.rope, "value = 0\n");
}

#[test]
fn stalled_workspace_edit_has_a_deadline_even_without_loader_events() {
    let (mut app, server, _temp, path) = fixture();
    app.tabs[0].editor.bg_loading = true;
    let process = app.lsp_manager.clients[0].child.id();
    app.queue_lsp_workspace_edit(
        workspace(&[path]),
        "Rename".into(),
        Some((process, json!(7))),
        None,
    );
    assert!(!app.poll_lsp_workspace_edits());
    assert!(app.lsp_workspace_edit_wait().unwrap() <= EDIT_TIMEOUT);
    app.pending_lsp_workspace_edits[0].queued = Instant::now() - EDIT_TIMEOUT;
    assert_eq!(app.lsp_workspace_edit_wait(), Some(Duration::ZERO));
    assert!(app.poll_lsp_workspace_edits());
    assert_eq!(server.take_messages()[0]["result"]["applied"], false);
    assert!(app.lsp_workspace_edit_wait().is_none());
}

#[test]
fn server_workspace_apply_edit_replies_only_after_validated_application() {
    let (mut app, server, _temp, path) = fixture();
    server.respond(json!({"id":"edit-1","method":"workspace/applyEdit","params":{"edit":workspace(&[path.clone()])}}));
    app.lsp_manager.poll();
    assert!(server.take_messages().is_empty());
    let (process, id, edit) = app.lsp_manager.workspace_edit_requests.remove(0);
    app.queue_lsp_workspace_edit(edit, "Applied".into(), Some((process, id)), None);
    assert!(app.poll_lsp_workspace_edits());
    let response = server.take_messages();
    assert_eq!(response[0]["id"], "edit-1");
    assert_eq!(response[0]["result"]["applied"], true);
    assert_eq!(
        app.find_editor_mut_by_path(&path).unwrap().rope.to_string(),
        "renamed = 1\n"
    );
}

#[test]
fn server_workspace_apply_edit_rejects_stale_versions_and_read_only_files() {
    for read_only in [true, false] {
        let (mut app, server, _temp, path) = fixture();
        app.find_editor_mut_by_path(&path).unwrap().read_only = read_only;
        app.lsp_manager.doc_versions.insert(path.clone(), 2);
        let edit = if read_only {
            workspace(&[path.clone()])
        } else {
            serde_json::from_value(json!({"documentChanges":[{
            "textDocument":{"uri":lsp_types::Url::from_file_path(&path).unwrap(),"version":1},
            "edits":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":5}},"newText":"renamed"}]
        }]})).unwrap()
        };
        let process = app.lsp_manager.clients[0].child.id();
        app.queue_lsp_workspace_edit(edit, "Applied".into(), Some((process, json!(7))), None);
        assert!(app.poll_lsp_workspace_edits());
        let response = server.take_messages();
        assert_eq!(response[0]["result"]["applied"], false);
        assert!(response[0]["result"]["failureReason"].is_string());
        assert_eq!(
            app.find_editor_mut_by_path(&path).unwrap().rope.to_string(),
            "value = 1\n"
        );
    }
}

#[test]
fn workspace_edits_reject_resource_operations_without_deleting_files() {
    let (mut app, server, _temp, path) = fixture();
    let edit = serde_json::from_value(json!({"documentChanges":[{"kind":"delete","uri":lsp_types::Url::from_file_path(&path).unwrap()}]})).unwrap();
    let process = app.lsp_manager.clients[0].child.id();
    app.queue_lsp_workspace_edit(edit, "Applied".into(), Some((process, json!(1))), None);
    assert!(app.pending_lsp_workspace_edits.is_empty());
    assert_eq!(server.take_messages()[0]["result"]["applied"], false);
    assert!(path.exists());
}

#[test]
fn code_actions_execute_the_server_command_after_edits() {
    let (mut app, server, _temp, path) = fixture();
    app.queue_lsp_workspace_edit(
        workspace(&[path.clone()]),
        "Apply".into(),
        None,
        Some((
            path,
            lsp_types::Command {
                title: "Imports".into(),
                command: "pyright.organizeimports".into(),
                arguments: Some(vec![json!("uri")]),
            },
        )),
    );
    assert!(app.poll_lsp_workspace_edits());
    let request = server.take_messages();
    assert_eq!(request[0]["method"], "workspace/executeCommand");
    assert_eq!(request[0]["params"]["arguments"], json!(["uri"]));
}
