use super::*;
use crate::{
    app::{ActivePane, Tab},
    editor::Editor,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
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
    app.execute_palette_command("open_file");
    assert!(app.file_browser_active());
    assert!(app.paste_into_focused_text_input(&format!("\"{}\"", path.display())));
    enter(&mut app);
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|editor| {
            editor.file_path.as_deref() == Some(path.as_path()) && !editor.bg_loading
        })
    });
    assert!(!app.file_browser_active());
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
fn missing_paths_leave_the_picker_open_and_a_typed_directory_navigates() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path());
    app.begin_open_file_browser();
    assert!(app.paste_into_focused_text_input("missing.txt"));
    enter(&mut app);
    app.wait_for_background(Duration::from_secs(10), |app| app.folder_browser.error.is_some());
    assert!(app.file_browser_active());
    assert!(!app.file_browser_open_pending());
    assert!(app.folder_browser.error.as_deref().unwrap().contains("does not exist"));

    crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Char('l'), KeyModifiers::CONTROL));
    assert!(app.paste_into_focused_text_input(&outside.path().display().to_string()));
    enter(&mut app);
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.folder_browser_path == outside.path() && !app.folder_browser_loading
    });
    assert!(app.file_browser_active());
    assert!(app.folder_browser.error.is_none());
    assert_eq!(app.explorer.root, root.path());
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.editor().unwrap().rope.to_string(), "origin\n");
    crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.file_browser_active());
}

#[test]
fn parent_relative_paths_can_open_external_files_without_revealing_them_in_the_tree() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(root.path().join("external.log"), "outside\n").unwrap();
    let mut app = fixture(&workspace);
    app.begin_open_file_browser();
    app.paste_into_focused_text_input("../external.log");
    enter(&mut app);
    app.wait_for_background(Duration::from_secs(10), |app| !app.file_browser_active());
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
    app.begin_open_file_browser();
    app.paste_into_focused_text_input(url::Url::from_file_path(&path).unwrap().as_str());
    enter(&mut app);
    app.wait_for_background(Duration::from_secs(10), |app| !app.file_browser_active());
    assert_eq!(app.editor().unwrap().rope.to_string(), "url content\n");
    assert_eq!(app.explorer.root, root.path());
}

#[test]
fn open_file_is_the_single_file_menu_entry_and_renders_a_path_field() {
    let root = tempfile::tempdir().unwrap();
    let mut app = fixture(root.path());
    let entries = app.menu_entries(0);
    let file_commands: Vec<_> = entries.iter().filter(|entry| entry.label.starts_with("Open File")).collect();
    assert_eq!(file_commands.len(), 1);
    assert_eq!(file_commands[0].label, "Open File…");
    app.execute_palette_command(file_commands[0].command);
    assert!(app.file_browser_active());
    for (width, height) in [(30, 12), (100, 24)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Open File") && text.contains("File name or path"));
        assert_eq!(app.focused_text_input_id(), Some(TextInputId::FolderName));
        assert!(app.folder_browser_area.is_some());
    }
}

#[test]
fn browsing_a_folder_and_clicking_open_loads_the_selected_file() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("docs");
    std::fs::create_dir(&folder).unwrap();
    let path = folder.join("notes.txt");
    std::fs::write(&path, "notes\n").unwrap();
    std::fs::write(root.path().join("aaa.txt"), "root file\n").unwrap();
    let mut app = fixture(root.path());
    app.execute_palette_command("open_file");
    app.wait_for_background(Duration::from_secs(10), |app| !app.folder_browser_loading);
    assert_eq!(app.folder_browser_entries, [folder.clone(), root.path().join("aaa.txt")]);

    let click = |app: &mut App, column, row| crate::mouse::handle_mouse(app, MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left), column, row, modifiers: KeyModifiers::NONE,
    });
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    let list = app.folder_browser_area.unwrap();
    click(&mut app, list.x + 3, list.y + 1);
    click(&mut app, list.x + 3, list.y + 1);
    app.wait_for_background(Duration::from_secs(10), |app| !app.folder_browser_loading);
    assert_eq!(app.folder_browser_path, folder);
    assert_eq!(app.folder_browser_entries, [path.clone()]);
    for (key, expected) in [(KeyCode::Left, root.path()), (KeyCode::Right, folder.as_path())] {
        crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(key, KeyModifiers::ALT));
        app.wait_for_background(Duration::from_secs(10), |app| !app.folder_browser_loading);
        assert_eq!(app.folder_browser_path, expected);
    }
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    let list = app.folder_browser_area.unwrap();
    click(&mut app, list.x + 3, list.y + 1);
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    let (x, y, _) = app.folder_browser_buttons[0];
    click(&mut app, x + 1, y);
    app.wait_for_background(Duration::from_secs(10), |app| !app.file_browser_active());
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&path));
    assert_eq!(app.editor().unwrap().rope.to_string(), "notes\n");
    assert_eq!(app.explorer.root, root.path());
}
