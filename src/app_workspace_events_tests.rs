use super::*;
use crate::{app::Tab, editor::Editor};
use std::time::SystemTime;

fn editor(path: PathBuf, text: &str) -> Tab {
    let mut editor = Editor::test_with_text(text);
    editor.file_path = Some(path);
    editor.saved_rope = Some(editor.rope.clone());
    editor.file_size = text.len() as u64;
    editor.last_disk_modified = Some(SystemTime::UNIX_EPOCH);
    editor.publish_document();
    Tab::new_file(editor)
}

#[test]
fn workspace_bursts_refresh_active_git_diff_only_for_relevant_changes() {
    for case in ["unrelated", "active", "ancestor", "git", "overflow"] {
        let root = tempfile::tempdir().unwrap();
        let active = root.path().join("src/open.rs");
        let mut app = App::new_deferred(root.path().to_owned());
        app.source_control_enabled = true;
        app.tabs = vec![editor(active.clone(), "saved")];
        app.bg_git_diff_running = true;
        let path = match case {
            "active" => active,
            "ancestor" => root.path().join("src"),
            _ => root.path().join("build/generated.rs"),
        };
        let changes = if matches!(case, "git" | "overflow") { Vec::new() } else {
            vec![WorkspacePathChange {
                path, observed_at: std::time::Instant::now(), exists: true,
                modified: None, len: None,
            }]
        };
        app.apply_workspace_file_changes(root.path().to_owned(), changes,
            case == "ancestor", case == "overflow", case == "git");
        assert_eq!(app.bg_git_diff_pending, case != "unrelated", "{case}");
    }
}

#[test]
fn a_deleted_tab_cannot_shift_metadata_updates_onto_another_document() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first.txt");
    let second = root.path().join("second.txt");
    std::fs::write(&first, "first").unwrap();
    std::fs::write(&second, "second").unwrap();
    let mut app = App::new_deferred(root.path().to_owned());
    app.source_control_enabled = false;
    app.tabs = vec![editor(first.clone(), "first"), editor(second.clone(), "second")];
    app.tabs[1].editor.insert_char('!');
    let unsaved = app.tabs[1].editor.rope.to_string();
    std::fs::remove_file(&first).unwrap();
    std::fs::write(&second, "external replacement").unwrap();
    let changes = [first, second.clone()].into_iter().map(crate::background::inspect_workspace_path).collect();
    app.apply_workspace_file_changes(root.path().to_owned(), changes, true, false, false);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.tabs[0].editor.file_path.as_ref(), Some(&second));
    assert_eq!(app.tabs[0].editor.rope.to_string(), unsaved);
    assert!(app.tabs[0].editor.modified);
    assert_eq!(app.tabs[0].editor.file_size, "external replacement".len() as u64);
    assert_eq!(std::fs::read_to_string(second).unwrap(), "external replacement");
}

#[test]
fn workspace_event_application_keeps_language_server_notifications_batched() {
    let root = tempfile::tempdir().unwrap();
    let mut app = App::new_deferred(root.path().to_owned());
    app.source_control_enabled = false;
    let (client, server) = crate::lsp::LspClient::test_pair("rust", &["*.rs"]);
    app.lsp_manager.clients.push(client);
    let changes = (0..128).map(|index| WorkspacePathChange {
        path: root.path().join(format!("build/source{index}.rs")),
        observed_at: std::time::Instant::now(), exists: true, modified: None, len: None,
    }).collect();
    app.apply_workspace_file_changes(root.path().to_owned(), changes, false, false, false);
    let messages = server.take_messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["method"], "workspace/didChangeWatchedFiles");
    assert_eq!(messages[0]["params"]["changes"].as_array().unwrap().len(), 128);
}
