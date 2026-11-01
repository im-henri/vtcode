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
            vec![WorkspacePathChange { is_dir: false,
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
    let changes = (0..128).map(|index| WorkspacePathChange { is_dir: false,
        path: root.path().join(format!("build/source{index}.rs")),
        observed_at: std::time::Instant::now(), exists: true, modified: None, len: None,
    }).collect();
    app.apply_workspace_file_changes(root.path().to_owned(), changes, false, false, false);
    let messages = server.take_messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["method"], "workspace/didChangeWatchedFiles");
    assert_eq!(messages[0]["params"]["changes"].as_array().unwrap().len(), 128);
}

#[test]
fn directory_invalidations_keep_open_generated_files_fresh_without_rebuilding_a_closed_folder() {
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("build");
    std::fs::create_dir(&output).unwrap();
    let path = output.join("generated.txt");
    std::fs::write(&path, "old").unwrap();
    let mut app = App::new_deferred(root.path().to_path_buf());
    app.source_control_enabled = false;
    app.explorer = crate::explorer::FileExplorer::new(root.path().to_path_buf());
    app.tabs = vec![editor(path.clone(), "old")];
    let generation = app.explorer_refresh_generation;
    std::fs::write(&path, "new generated contents").unwrap();
    app.apply_workspace_file_changes(root.path().to_path_buf(),
        vec![crate::background::inspect_workspace_path(output)], true, false, false);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.editor().is_some_and(|editor| editor.rope == "new generated contents" && !editor.bg_loading)
    });
    assert_eq!(app.explorer_refresh_generation, generation);
    assert!(!app.bg_explorer_refresh_running);
}

#[test]
fn replacing_a_closed_directory_with_a_file_still_updates_the_tree() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("output");
    std::fs::create_dir(&path).unwrap();
    let mut app = App::new_deferred(root.path().to_path_buf());
    app.source_control_enabled = false;
    app.explorer = crate::explorer::FileExplorer::new(root.path().to_path_buf());
    assert!(app.explorer.entries[0].is_dir);
    std::fs::remove_dir(&path).unwrap();
    std::fs::write(&path, "replacement").unwrap();
    let change = crate::background::inspect_workspace_path(path.clone());
    assert!(!change.is_dir);
    app.apply_workspace_file_changes(root.path().to_path_buf(), vec![change], false, false, false);
    assert!(app.bg_explorer_refresh_running);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| !app.bg_explorer_refresh_running);
    let entry = app.explorer.entries.iter().find(|entry| entry.path == path).unwrap();
    assert!(!entry.is_dir);
}

#[test]
fn windows_generic_modifications_reload_open_files_without_rebuilding_the_name_sorted_tree() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("open.txt");
    std::fs::write(&path, "old").unwrap();
    let mut app = App::new_deferred(root.path().to_path_buf());
    app.source_control_enabled = false;
    app.explorer = crate::explorer::FileExplorer::new(root.path().to_path_buf());
    app.tabs = vec![editor(path.clone(), "old")];
    let generation = app.explorer_refresh_generation;
    std::fs::write(&path, "new source contents").unwrap();
    let mut burst = crate::workspace_watch::Burst::default();
    burst.push(root.path(), Ok(notify::Event::new(notify::EventKind::Modify(notify::event::ModifyKind::Any)).add_path(path)));
    assert!(!burst.event.structure_changed && !burst.event.rescan);
    let event = burst.event;
    let changes = event.paths.into_iter().map(crate::background::inspect_workspace_path).collect();
    app.apply_workspace_file_changes(root.path().to_path_buf(), changes, event.structure_changed, event.rescan, event.git_metadata_changed);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        app.editor().is_some_and(|editor| editor.rope == "new source contents" && !editor.bg_loading)
    });
    assert_eq!(app.explorer_refresh_generation, generation);
    assert!(!app.bg_explorer_refresh_running);
}

#[test]
fn a_file_only_notification_can_reveal_a_new_ancestor_directory() {
    let root = tempfile::tempdir().unwrap();
    let mut app = App::new_deferred(root.path().to_path_buf());
    app.source_control_enabled = false;
    app.explorer = crate::explorer::FileExplorer::new(root.path().to_path_buf());
    let path = root.path().join("new/deep/file.rs");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "new source").unwrap();
    app.apply_workspace_file_changes(root.path().to_path_buf(),
        vec![crate::background::inspect_workspace_path(path)], true, false, false);
    app.wait_for_background(std::time::Duration::from_secs(5), |app| !app.bg_explorer_refresh_running);
    assert!(app.explorer.entries.iter().any(|entry| entry.path == root.path().join("new") && entry.is_dir));
}
