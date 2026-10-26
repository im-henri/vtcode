use super::*;
use crate::{app::Tab, editor::Editor};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::time::Duration;

fn fixture(selected_text: bool) -> (tempfile::TempDir, App) {
    let directory = tempfile::tempdir().unwrap();
    for name in ["alpha.rs", "second.txt", "folder/nested.txt"] {
        let path = directory.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("Saved {name}\n")).unwrap();
    }
    std::fs::create_dir(directory.path().join("destination")).unwrap();
    let mut app = App::new(directory.path().into());
    app.source_control_enabled = false;
    app.explorer = crate::explorer::FileExplorer::new(directory.path().into());
    app.show_explorer = true;
    app.sidebar_view = SidebarView::Explorer;
    app.active_pane = ActivePane::Explorer;
    let mut editor = Editor::test_with_text("alpha\nbeta\n");
    editor.file_path = Some(directory.path().join("alpha.rs"));
    editor.insert_char('!');
    editor.set_cursor(1, 2);
    editor.selection_anchor = selected_text.then_some((0, 1));
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    select(&mut app, "alpha.rs");
    (directory, app)
}

fn select(app: &mut App, relative: &str) {
    let path = app.explorer.root.join(relative);
    let index = app.explorer.find_path(&path).unwrap();
    app.set_explorer_single_selection(index);
}
fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    assert!(!crate::input_keyboard::handle_keyboard_event(
        app,
        KeyEvent::new(code, modifiers)
    ));
}
fn ctrl(app: &mut App, letter: char) {
    key(app, KeyCode::Char(letter), KeyModifiers::CONTROL);
}
fn wait(app: &mut App) {
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.bg_filesystem_action_running && !app.bg_explorer_refresh_running
    });
}
fn editor_state(app: &App) -> (ropey::Rope, usize, usize, Option<(usize, usize)>, u64, bool) {
    let editor = app.editor().unwrap();
    (
        editor.rope.clone(),
        editor.cursor_line,
        editor.cursor_col,
        editor.selection_anchor,
        editor.content_version,
        editor.modified,
    )
}

#[test]
fn explorer_copy_and_cut_own_shortcuts_and_edit_menu_commands_without_changing_code() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for selected_text in [false, true] {
        for (letter, command) in [('c', "copy"), ('x', "cut")] {
            for uppercase in [false, true] {
                for menu in [false, true] {
                    let (directory, mut app) = fixture(selected_text);
                    // A previous context menu must not choose a different file.
                    app.explorer_context_target = Some(directory.path().join("second.txt"));
                    let before = editor_state(&app);
                    if menu {
                        app.execute_palette_command(command);
                    } else {
                        ctrl(
                            &mut app,
                            if uppercase {
                                letter.to_ascii_uppercase()
                            } else {
                                letter
                            },
                        );
                    }
                    let clipboard = app.explorer_clipboard.as_ref().unwrap();
                    assert_eq!(clipboard.paths, [directory.path().join("alpha.rs")]);
                    assert_eq!(clipboard.cut, command == "cut");
                    assert_eq!(editor_state(&app), before);
                    assert!(
                        directory.path().join("alpha.rs").exists(),
                        "cut is deferred until paste"
                    );
                }
            }
        }
    }
}

#[test]
fn explorer_undo_redo_never_changes_the_document_or_its_history() {
    let (_directory, mut app) = fixture(true);
    let before = editor_state(&app);
    for letter in ['z', 'y', 'Z', 'Y'] {
        ctrl(&mut app, letter);
        assert_eq!(editor_state(&app), before);
    }
    for command in ["undo", "redo"] {
        app.execute_palette_command(command);
        assert_eq!(editor_state(&app), before);
    }
    app.active_pane = ActivePane::Editor;
    ctrl(&mut app, 'z');
    assert_eq!(app.editor().unwrap().rope.to_string(), "alpha\nbeta\n");
    ctrl(&mut app, 'y');
    assert_eq!(app.editor().unwrap().rope, before.0);
    app.execute_palette_command("undo");
    assert_eq!(app.editor().unwrap().rope.to_string(), "alpha\nbeta\n");
    app.execute_palette_command("redo");
    assert_eq!(app.editor().unwrap().rope, before.0);
}

#[test]
fn file_paste_uses_the_selected_directory_and_matching_terminal_paste_payload() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let (directory, mut app) = fixture(true);
    let before = editor_state(&app);
    ctrl(&mut app, 'c');
    let payload = app
        .explorer_clipboard
        .as_ref()
        .unwrap()
        .terminal_text
        .clone();
    select(&mut app, "destination");
    crate::app_event_loop::handle_paste_input(&mut app, "unrelated text from another program");
    assert!(!app.bg_filesystem_action_running);
    assert!(!directory.path().join("destination/alpha.rs").exists());
    crate::app_event_loop::handle_paste_input(&mut app, &payload);
    assert!(app.bg_filesystem_action_running);
    wait(&mut app);
    assert_eq!(
        std::fs::read_to_string(directory.path().join("destination/alpha.rs")).unwrap(),
        "Saved alpha.rs\n"
    );
    assert_eq!(editor_state(&app), before);
    assert!(
        app.explorer_clipboard.is_some(),
        "copy remains available for repeated pastes"
    );
    assert!(directory.path().join("alpha.rs").exists());
}

#[test]
fn terminal_multi_file_paste_accepts_normalized_line_endings_without_parsing_external_paths() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for ending in ["\n", "\r\n", "\r"] {
        let (directory, mut app) = fixture(true);
        app.explorer_multi_selected = vec![
            directory.path().join("alpha.rs"),
            directory.path().join("second.txt"),
        ]
        .into();
        let before = editor_state(&app);
        ctrl(&mut app, 'c');
        let payload = app
            .explorer_clipboard
            .as_ref()
            .unwrap()
            .terminal_text
            .replace('\n', ending);
        select(&mut app, "destination");
        crate::app_event_loop::handle_paste_input(&mut app, &payload);
        wait(&mut app);
        assert!(directory.path().join("destination/alpha.rs").exists());
        assert!(directory.path().join("destination/second.txt").exists());
        assert_eq!(editor_state(&app), before);
    }
}

#[test]
fn copying_into_the_original_folder_chooses_new_names_and_preserves_the_source() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let (directory, mut app) = fixture(false);
    let before = editor_state(&app);
    ctrl(&mut app, 'c');
    for suffix in [1, 2] {
        select(&mut app, "alpha.rs");
        ctrl(&mut app, 'v');
        wait(&mut app);
        assert_eq!(
            std::fs::read_to_string(directory.path().join(format!("alpha ({suffix}).rs"))).unwrap(),
            "Saved alpha.rs\n"
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("alpha.rs")).unwrap(),
            "Saved alpha.rs\n"
        );
    }
    assert_eq!(editor_state(&app), before);
}

#[test]
fn cutting_and_pasting_moves_the_file_but_preserves_unsaved_buffer_and_undo_history() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let (directory, mut app) = fixture(true);
    let before = editor_state(&app);
    ctrl(&mut app, 'x');
    select(&mut app, "destination");
    ctrl(&mut app, 'v');
    wait(&mut app);
    assert!(!directory.path().join("alpha.rs").exists());
    assert!(directory.path().join("destination/alpha.rs").exists());
    assert_eq!(
        app.editor().unwrap().file_path.as_ref(),
        Some(&directory.path().join("destination/alpha.rs"))
    );
    assert_eq!(editor_state(&app), before);
    assert!(app.explorer_clipboard.is_none());
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "alpha\nbeta\n");
}

#[test]
fn copy_and_cut_multiselections_process_each_selected_tree_only_once() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for cut in [false, true] {
        let (directory, mut app) = fixture(true);
        select(&mut app, "folder");
        app.explorer_multi_selected = vec![
            directory.path().join("folder"),
            directory.path().join("folder/nested.txt"),
            directory.path().join("alpha.rs"),
        ]
        .into();
        let before = editor_state(&app);
        ctrl(&mut app, if cut { 'x' } else { 'c' });
        assert_eq!(app.explorer_clipboard.as_ref().unwrap().paths.len(), 2);
        select(&mut app, "destination");
        ctrl(&mut app, 'v');
        wait(&mut app);
        assert!(
            directory
                .path()
                .join("destination/folder/nested.txt")
                .exists()
        );
        assert!(directory.path().join("destination/alpha.rs").exists());
        assert!(!directory.path().join("destination/nested.txt").exists());
        assert_eq!(directory.path().join("folder/nested.txt").exists(), !cut);
        assert_eq!(editor_state(&app), before);
    }
}

#[test]
fn select_all_and_empty_explorer_clipboard_actions_never_touch_editor_selection() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let (_directory, mut app) = fixture(true);
    let before = editor_state(&app);
    ctrl(&mut app, 'a');
    assert_eq!(
        app.explorer_multi_selected.len(),
        app.explorer.entries.len()
    );
    assert_eq!(editor_state(&app), before);
    app.explorer.entries.clear();
    app.clear_explorer_selection();
    for letter in ['a', 'c', 'x', 'v'] {
        ctrl(&mut app, letter);
    }
    assert!(app.explorer_clipboard.is_none());
    assert_eq!(editor_state(&app), before);
}

#[test]
fn rebound_commands_and_insert_aliases_use_file_clipboard_with_explorer_focus() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let (directory, mut app) = fixture(true);
    let before = editor_state(&app);
    let binding = app
        .keybindings
        .iter_mut()
        .find(|binding| binding.id == "copy")
        .unwrap();
    binding.code = KeyCode::F(6);
    binding.modifiers = KeyModifiers::NONE;
    key(&mut app, KeyCode::F(6), KeyModifiers::NONE);
    assert_eq!(
        app.explorer_clipboard.as_ref().unwrap().paths,
        [directory.path().join("alpha.rs")]
    );
    select(&mut app, "destination");
    key(&mut app, KeyCode::Insert, KeyModifiers::SHIFT);
    wait(&mut app);
    select(&mut app, "second.txt");
    key(&mut app, KeyCode::Insert, KeyModifiers::CONTROL);
    assert_eq!(
        app.explorer_clipboard.as_ref().unwrap().paths,
        [directory.path().join("second.txt")]
    );
    assert_eq!(editor_state(&app), before);
}

#[test]
fn clipboard_keys_in_other_sidebars_and_explorer_controls_cannot_edit_hidden_code() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for sidebar in [
        SidebarView::Search,
        SidebarView::SourceControl,
        SidebarView::Extensions,
        SidebarView::Outline,
        SidebarView::Settings,
    ] {
        let (_directory, mut app) = fixture(true);
        app.sidebar_view = sidebar;
        app.source_control_input_mode = false;
        app.ext_sidebar.search_focused = false;
        let before = editor_state(&app);
        for letter in ['c', 'x', 'v'] {
            ctrl(&mut app, letter);
        }
        assert!(app.explorer_clipboard.is_none());
        assert_eq!(editor_state(&app), before, "{sidebar:?}");
    }
    let (_directory, mut app) = fixture(true);
    let before = editor_state(&app);
    app.explorer.find.open = true;
    app.explorer.find.focus = crate::explorer_find::Focus::Filter;
    for id in [
        "copy",
        "cut",
        "paste",
        "paste_plain",
        "clipboard_history",
        "select_all",
    ] {
        app.execute_palette_command(id);
    }
    assert!(app.explorer_clipboard.is_none());
    assert_eq!(editor_state(&app), before);
}

#[test]
fn rename_and_explorer_find_fields_keep_text_clipboard_ownership() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for rename in [false, true] {
        let (directory, mut app) = fixture(true);
        let before = editor_state(&app);
        if rename {
            app.begin_rename(
                directory.path().join("alpha.rs"),
                "alpha.rs".into(),
                true,
                false,
            );
        } else {
            app.open_explorer_find();
            app.explorer.find.query = "alpha.rs".into();
            app.explorer.find.cursor = "alpha.rs".len();
        }
        ctrl(&mut app, 'a');
        app.execute_palette_command("cut");
        let value = if rename {
            &app.rename.query
        } else {
            &app.explorer.find.query
        };
        assert!(value.is_empty());
        crate::app_event_loop::handle_paste_input(&mut app, "renamed.rs");
        let value = if rename {
            &app.rename.query
        } else {
            &app.explorer.find.query
        };
        assert_eq!(value, "renamed.rs");
        assert!(app.explorer_clipboard.is_none());
        assert_eq!(editor_state(&app), before);
        assert!(directory.path().join("alpha.rs").exists());
    }
}

#[test]
fn release_and_repeated_paste_events_do_not_cut_code_or_start_duplicate_operations() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let (directory, mut app) = fixture(true);
    let before = editor_state(&app);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL,
            KeyEventKind::Release,
        ),
    );
    assert!(app.explorer_clipboard.is_none());
    ctrl(&mut app, 'c');
    select(&mut app, "destination");
    for _ in 0..3 {
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new_with_kind(
                KeyCode::Char('v'),
                KeyModifiers::CONTROL,
                KeyEventKind::Repeat,
            ),
        );
    }
    wait(&mut app);
    assert!(directory.path().join("destination/alpha.rs").exists());
    assert!(!directory.path().join("destination/alpha (1).rs").exists());
    assert_eq!(editor_state(&app), before);
}

#[test]
fn cutting_back_into_the_same_folder_is_a_noop_and_copying_a_folder_into_itself_is_rejected() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let (directory, mut app) = fixture(true);
    let before = editor_state(&app);
    ctrl(&mut app, 'x');
    ctrl(&mut app, 'v');
    wait(&mut app);
    assert!(directory.path().join("alpha.rs").exists());
    assert!(!directory.path().join("alpha (1).rs").exists());
    assert_eq!(editor_state(&app), before);
    select(&mut app, "folder");
    ctrl(&mut app, 'c');
    ctrl(&mut app, 'v');
    wait(&mut app);
    assert!(!directory.path().join("folder/folder").exists());
    assert_eq!(
        std::fs::read_to_string(directory.path().join("folder/nested.txt")).unwrap(),
        "Saved folder/nested.txt\n"
    );
}
