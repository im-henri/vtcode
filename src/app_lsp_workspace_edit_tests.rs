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
