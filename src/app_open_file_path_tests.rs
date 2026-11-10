use super::*;
use crate::{
    app::{ActivePane, Tab},
    editor::Editor,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend};
use std::{path::Path, time::Duration};

fn fixture(root: &Path) -> App {
    let mut app = App::test_new();
    app.explorer = crate::explorer::FileExplorer::new_deferred(root.into());
    app.source_control_enabled = false;
    app.tabs = vec![Tab::new_file(Editor::test_with_text("origin\n"))];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app
}

fn enter(app: &mut App) {
    crate::input_keyboard::handle_keyboard_event(
        app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
}

#[test]
fn opening_an_external_local_file_by_path_keeps_the_workspace_and_saves_the_original_file() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let path = outside.path().join("資料 log.txt");
    std::fs::write(&path, "local log\n").unwrap();
    let mut app = fixture(workspace.path());
    app.execute_palette_command("open_file_path");
    assert_eq!(
        crate::ui_modal::active(&app),
        Some(crate::ui_modal::Kind::OpenFilePath)
    );
    assert!(app.paste_into_focused_text_input(&format!("\"{}\"", path.display())));
    enter(&mut app);
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|editor| {
            editor.file_path.as_deref() == Some(path.as_path()) && !editor.bg_loading
        })
    });
    assert!(!app.file_open.active);
    assert_eq!(app.editor().unwrap().rope.to_string(), "local log\n");
    assert_eq!(app.explorer.root, workspace.path());
    assert!(!app.explorer_contains_file_path(&path));
    assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 0);
    app.editor_mut().unwrap().insert_char('X');
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.pending_file_saves.is_empty()
    });
    assert_eq!(std::fs::read_to_string(path).unwrap(), "Xlocal log\n");
    assert_eq!(app.explorer.root, workspace.path());
}

#[test]
fn missing_paths_and_directories_leave_the_prompt_open_with_a_reason() {
    let root = tempfile::tempdir().unwrap();
    for path in [root.path().join("missing.txt"), root.path().to_owned()] {
        let mut app = fixture(root.path());
        app.begin_open_file_path();
        assert!(app.paste_into_focused_text_input(&path.display().to_string()));
        enter(&mut app);
        app.wait_for_background(Duration::from_secs(10), |app| app.modal.error.is_some());
        assert!(app.file_open.active);
        assert!(!app.file_open.pending);
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.editor().unwrap().rope.to_string(), "origin\n");
        let error = app.modal.error.as_deref().unwrap();
        assert!(
            error.contains("does not exist") || error.contains("folder"),
            "{error}"
        );
        crate::ui_modal::handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.file_open.active);
    }
}

#[test]
fn parent_relative_paths_can_open_external_files_without_revealing_them_in_the_tree() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(root.path().join("external.log"), "outside\n").unwrap();
    let mut app = fixture(&workspace);
    app.begin_open_file_path();
    app.paste_into_focused_text_input("../external.log");
    enter(&mut app);
    app.wait_for_background(Duration::from_secs(10), |app| !app.file_open.active);
    assert_eq!(app.editor().unwrap().rope.to_string(), "outside\n");
    let path = app.editor().unwrap().file_path.as_deref().unwrap();
    assert!(!app.explorer_contains_file_path(path));
    assert_eq!(app.explorer.root, workspace);
}

#[test]
fn file_url_input_opens_a_file_without_changing_the_workspace() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let path = outside.path().join("log #1.txt");
    std::fs::write(&path, "url content\n").unwrap();
    let mut app = fixture(root.path());
    app.begin_open_file_path();
    app.paste_into_focused_text_input(url::Url::from_file_path(&path).unwrap().as_str());
    enter(&mut app);
    app.wait_for_background(Duration::from_secs(10), |app| !app.file_open.active);
    assert_eq!(app.editor().unwrap().rope.to_string(), "url content\n");
    assert_eq!(app.explorer.root, root.path());
}

#[test]
fn path_dialog_is_discoverable_and_renders_the_local_source_and_standard_input() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path());
    assert!(
        app.menu_entries(0)
            .iter()
            .any(|entry| entry.command == "open_file_path")
    );
    app.begin_open_file_path();
    for (width, height) in [(30, 12), (100, 24)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::ui_modal::draw_active(frame, &mut app))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Open File by Path"));
        if width == 100 {
            assert!(text.contains("this device"));
        }
        assert_eq!(app.focused_text_input_id(), Some(TextInputId::OpenFilePath));
        for (rect, _) in &app.modal.buttons {
            assert_eq!(rect.intersection(app.modal.area), *rect);
        }
    }
}
