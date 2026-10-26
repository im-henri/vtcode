use super::*;
use crate::{app::Tab, editor::Editor};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use std::{path::PathBuf, time::Duration};

fn fixture() -> (tempfile::TempDir, App) {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("folder")).unwrap();
    for name in [
        "first.txt",
        "second.txt",
        "third.txt",
        "keep.txt",
        "folder/nested.txt",
    ] {
        std::fs::write(root.path().join(name), name).unwrap();
    }
    let mut app = App::new_deferred(root.path().to_path_buf());
    app.source_control_enabled = false;
    app.explorer = crate::explorer::FileExplorer::new(root.path().to_path_buf());
    app.show_explorer = true;
    app.sidebar_view = SidebarView::Explorer;
    app.active_pane = ActivePane::Explorer;
    let mut editor = Editor::test_with_text("keep my unsaved edits");
    editor.file_path = Some(root.path().join("keep.txt"));
    editor.insert_char('!');
    app.tabs = vec![Tab::new_file(editor)];
    (root, app)
}

fn index(app: &App, name: &str) -> usize {
    app.explorer
        .find_path(&app.explorer.root.join(name))
        .unwrap()
}

fn select(app: &mut App, names: &[&str]) -> Vec<PathBuf> {
    app.set_explorer_single_selection(index(app, names[0]));
    for name in &names[1..] {
        app.toggle_explorer_selection(index(app, name));
    }
    app.explorer_multi_selected.to_vec()
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    assert!(!crate::input_keyboard::handle_keyboard_event(
        app,
        KeyEvent::new(code, modifiers)
    ));
}

fn render(app: &mut App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui_modal::draw_active(frame, app))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn click_button(app: &mut App, index: usize) {
    let rect = app
        .modal
        .buttons
        .iter()
        .find(|(_, i)| *i == index)
        .unwrap()
        .0;
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: rect.x,
            row: rect.y,
            modifiers: KeyModifiers::NONE,
        },
    );
}

fn wait(app: &mut App) {
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.bg_filesystem_action_running
            && !app.bg_explorer_refresh_running
            && !app.bg_explorer_refresh_pending
    });
}

#[test]
fn delete_key_confirms_and_removes_the_entire_selection_without_editing_code() {
    for mouse_confirm in [false, true] {
        let (root, mut app) = fixture();
        let text = app.editor().unwrap().rope.clone();
        let version = app.editor().unwrap().content_version;
        let paths = select(&mut app, &["first.txt", "second.txt", "third.txt"]);
        key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(app.confirm_delete_targets, paths);
        assert!(!app.bg_filesystem_action_running);
        assert!(
            paths.iter().all(|path| path.exists()),
            "confirmation must come before any deletion"
        );
        let dialog = render(&mut app);
        assert!(dialog.contains("Delete these 3 files or folders?"));
        for name in ["first.txt", "second.txt", "third.txt"] {
            assert!(dialog.contains(name));
        }
        // Confirmation owns its captured targets, even if a refresh or another
        // action updates the live Explorer selection before the user responds.
        app.set_explorer_single_selection(index(&app, "keep.txt"));
        if mouse_confirm {
            click_button(&mut app, 0);
        } else {
            key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        }
        wait(&mut app);
        assert!(paths.iter().all(|path| !path.exists()));
        assert!(root.path().join("keep.txt").exists());
        assert!(root.path().join("folder/nested.txt").exists());
        assert!(app.editor().unwrap().rope.is_instance(&text));
        assert_eq!(app.editor().unwrap().content_version, version);
        assert!(app.editor().unwrap().modified);
    }
}

#[test]
fn deleting_after_ctrl_deselect_never_deletes_the_unselected_focused_row() {
    let (root, mut app) = fixture();
    select(&mut app, &["first.txt", "second.txt", "third.txt"]);
    app.toggle_explorer_selection(index(&app, "third.txt"));
    assert_eq!(app.explorer.selected, index(&app, "third.txt"));
    key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    assert_eq!(
        app.confirm_delete_targets,
        [
            root.path().join("first.txt"),
            root.path().join("second.txt")
        ]
    );
    key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
    wait(&mut app);
    assert!(!root.path().join("first.txt").exists());
    assert!(!root.path().join("second.txt").exists());
    assert!(root.path().join("third.txt").exists());
}

#[test]
fn range_selection_and_select_all_delete_every_selected_file_and_directory() {
    for all in [false, true] {
        let (root, mut app) = fixture();
        if all {
            key(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
        } else {
            app.set_explorer_single_selection(index(&app, "first.txt"));
            app.range_select_explorer(index(&app, "third.txt"), false);
        }
        let paths = app.explorer_multi_selected.to_vec();
        assert!(paths.len() >= 3);
        key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
        assert_eq!(app.confirm_delete_targets, paths);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        wait(&mut app);
        assert!(paths.iter().all(|path| !path.exists()));
        assert!(root.path().exists(), "the workspace itself must remain");
        assert_eq!(root.path().join("folder/nested.txt").exists(), !all);
    }
}

#[test]
fn cancelling_multi_delete_preserves_all_files_and_the_selection() {
    for mouse_cancel in [false, true] {
        let (_root, mut app) = fixture();
        let paths = select(&mut app, &["first.txt", "second.txt"]);
        key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
        render(&mut app);
        if mouse_cancel {
            click_button(&mut app, 1);
        } else {
            key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        }
        assert!(!app.confirm_delete_active && !app.bg_filesystem_action_running);
        assert!(app.confirm_delete_targets.is_empty() && app.confirm_delete_target.is_none());
        assert_eq!(app.explorer_multi_selected, paths);
        assert!(paths.iter().all(|path| path.exists()));
    }
}

#[test]
fn context_menu_deletes_selected_items_but_an_unselected_target_stays_independent() {
    for selected_target in [false, true] {
        let (root, mut app) = fixture();
        let paths = select(&mut app, &["first.txt", "second.txt"]);
        app.explorer.selected = index(
            &app,
            if selected_target {
                "first.txt"
            } else {
                "third.txt"
            },
        );
        app.open_explorer_context_menu(4, 4);
        let action = app
            .explorer_context_menu_items()
            .iter()
            .position(|item| *item == "Delete")
            .unwrap();
        app.execute_explorer_context(action);
        let expected = if selected_target {
            paths
        } else {
            vec![root.path().join("third.txt")]
        };
        assert_eq!(app.confirm_delete_targets, expected);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        wait(&mut app);
        assert!(expected.iter().all(|path| !path.exists()));
        assert_eq!(root.path().join("third.txt").exists(), selected_target);
        assert_eq!(root.path().join("first.txt").exists(), !selected_target);
    }
}

#[test]
fn delete_without_explicit_multi_selection_keeps_the_single_file_behavior() {
    let (root, mut app) = fixture();
    app.explorer.selected = index(&app, "first.txt");
    app.clear_explorer_selection();
    key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    assert_eq!(app.confirm_delete_targets, [root.path().join("first.txt")]);
    assert!(render(&mut app).contains("Delete this item?"));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
    assert!(root.path().join("first.txt").exists());
}

#[test]
fn delete_cannot_act_on_the_file_selection_behind_other_or_hidden_sidebars() {
    for sidebar in [
        SidebarView::Outline,
        SidebarView::Settings,
        SidebarView::Explorer,
    ] {
        let (_root, mut app) = fixture();
        let paths = select(&mut app, &["first.txt", "second.txt"]);
        app.sidebar_view = sidebar;
        app.show_explorer = sidebar != SidebarView::Explorer;
        key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
        assert!(!app.confirm_delete_active && !app.bg_filesystem_action_running);
        assert!(paths.iter().all(|path| path.exists()));
    }
}

#[test]
fn large_delete_requests_deduplicate_without_reordering_the_confirmation() {
    let (_root, mut app) = fixture();
    let expected: Vec<_> = (0..10_000)
        .map(|i| app.explorer.root.join(format!("file-{i}.txt")))
        .collect();
    let targets = expected
        .iter()
        .flat_map(|path| [path.clone(), path.clone()])
        .collect();
    app.request_explorer_delete_targets(targets);
    assert_eq!(app.confirm_delete_targets, expected);
    assert!(!app.bg_filesystem_action_running);
    assert!(render(&mut app).contains("Delete these 10000 files or folders?"));
    key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
}
