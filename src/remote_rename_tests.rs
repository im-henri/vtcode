use super::*;
use crate::app::{ActivePane, SidebarView};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

fn key(app: &mut crate::app::App, code: KeyCode) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, KeyModifiers::NONE));
}

#[test]
fn nested_directory_rename_commits_from_keyboard_context_menu_and_clicking_away() {
    for (remote, context_menu, click_away) in [
        (false, false, false),
        (false, false, true),
        (false, true, false),
        (false, true, true),
        (true, false, false),
        (true, false, true),
        (true, true, false),
        (true, true, true),
    ] {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("parent/original.dir")).unwrap();
        std::fs::write(
            directory.path().join("parent/original.dir/inside.txt"),
            "keep this content\n",
        )
        .unwrap();
        let workspace =
            workspace::Workspace::new(connection(directory.path(), Arc::default(), Arc::default()));
        let mut app = if remote {
            app_for_workspace(&workspace)
        } else {
            crate::app::App::new_deferred(directory.path().into())
        };
        app.source_control_enabled = false;
        app.trim_trailing_on_save = false;
        app.insert_final_newline = false;
        let view_key = |path: &str| {
            if remote {
                workspace.target_key(path).unwrap()
            } else {
                directory.path().join(path)
            }
        };
        app.show_explorer = true;
        app.sidebar_view = SidebarView::Explorer;
        app.active_pane = ActivePane::Explorer;
        app.refresh_explorer_async();
        app.wait_for_background(Duration::from_secs(5), |app| {
            !app.bg_explorer_refresh_running
        });
        let original = view_key("parent/original.dir");
        app.open_file(&view_key("parent/original.dir/inside.txt"));
        app.wait_for_background(Duration::from_secs(5), |app| {
            !app.editor().unwrap().bg_loading
        });
        app.editor_mut().unwrap().insert_char('X');
        app.active_pane = ActivePane::Explorer;
        app.set_explorer_single_selection(app.explorer.find_path(&view_key("parent")).unwrap());
        assert!(app.toggle_or_open_explorer_async().is_none());
        app.wait_for_background(Duration::from_secs(5), |app| {
            !app.bg_explorer_refresh_running
        });
        app.set_explorer_single_selection(app.explorer.find_path(&original).unwrap());
        assert!(app.toggle_or_open_explorer_async().is_none());
        app.wait_for_background(Duration::from_secs(5), |app| {
            !app.bg_explorer_refresh_running
        });
        let index = app.explorer.find_path(&original).unwrap();
        app.set_explorer_single_selection(index);
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        if context_menu {
            app.explorer_context_target = Some(original.clone());
            let rename = app
                .explorer_context_menu_items()
                .iter()
                .position(|&label| label == "Rename")
                .unwrap();
            app.execute_explorer_context(rename);
        } else {
            key(&mut app, KeyCode::F(2));
        }
        assert!(app.rename.active);
        for ch in "renamed.dir".chars() {
            key(&mut app, KeyCode::Char(ch));
        }
        assert_eq!(app.rename.query, "renamed.dir");
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        if click_away {
            let editor = app.editor_area.unwrap();
            crate::mouse::handle_mouse(
                &mut app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: editor.x + editor.width / 2,
                    row: editor.y + editor.height / 2,
                    modifiers: KeyModifiers::NONE,
                },
            );
        } else {
            key(&mut app, KeyCode::Enter);
        }
        app.wait_for_background(Duration::from_secs(5), |app| {
            !app.bg_filesystem_action_running && !app.bg_explorer_refresh_running
        });
        assert!(
            directory.path().join("parent/renamed.dir").is_dir(),
            "remote: {remote}; context menu: {context_menu}; click away: {click_away}; status: {}; notifications: {:?}",
            app.status_message,
            app.notification_history
        );
        assert!(!directory.path().join("parent/original.dir").exists());
        assert_eq!(
            std::fs::read_to_string(directory.path().join("parent/renamed.dir/inside.txt"))
                .unwrap(),
            "keep this content\n"
        );
        assert!(!app.rename.active);
        assert_eq!(
            app.editor().unwrap().file_path.as_ref(),
            Some(&view_key("parent/renamed.dir/inside.txt"))
        );
        assert_eq!(
            app.editor().unwrap().rope.to_string(),
            "Xkeep this content\n"
        );
        assert!(app.editor().unwrap().modified);
        assert!(
            app.explorer
                .find_path(&view_key("parent/renamed.dir"))
                .is_some()
        );
        app.save_current_file();
        app.wait_for_background(Duration::from_secs(5), |app| {
            app.pending_file_saves.is_empty()
        });
        assert_eq!(
            std::fs::read_to_string(directory.path().join("parent/renamed.dir/inside.txt"))
                .unwrap(),
            "Xkeep this content\n"
        );
    }
}

#[test]
fn remote_rename_reports_a_busy_file_operation_and_keeps_the_name_for_retry() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("original")).unwrap();
    let workspace =
        workspace::Workspace::new(connection(directory.path(), Arc::default(), Arc::default()));
    let mut app = app_for_workspace(&workspace);
    app.begin_rename(
        workspace.target_key("original").unwrap(),
        "renamed".into(),
        true,
        false,
    );
    app.bg_filesystem_action_running = true;
    key(&mut app, KeyCode::Enter);
    assert!(app.rename.active);
    assert_eq!(app.rename.query, "renamed");
    assert!(
        app.notification_history
            .iter()
            .any(|(message, _, _)| message.contains("Another file operation"))
    );
    assert!(directory.path().join("original").is_dir());
    assert!(!directory.path().join("renamed").exists());
    app.bg_filesystem_action_running = false;
    key(&mut app, KeyCode::Enter);
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.bg_filesystem_action_running
    });
    assert!(!app.rename.active);
    assert!(directory.path().join("renamed").is_dir());
}
