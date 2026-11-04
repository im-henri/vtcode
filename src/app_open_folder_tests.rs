use super::*;
use super::sets_tests::tick;

#[path = "app_open_folder_confirmation_tests.rs"]
mod confirmation_tests;

fn choose(app: &mut App, path: &Path) {
    app.execute_palette_command("change_dir");
    assert!(app.change_dir_active && !app.folder_browser.add_workspace);
    app.folder_browser.folder = app.display_path(path);
    app.confirm_folder_browser();
    assert!(!app.change_dir_active);
    assert!(matches!(app.workspaces.request, Some(Request::Replace(_))));
}

fn confirmation(app: &mut App) {
    until(|| {
        tick(app);
        app.workspace_set_confirmation().is_some()
    });
    assert!(app.workspace_folder_replacement());
    assert!(matches!(app.workspaces.popup.sets.mode, Some(crate::ui_workspace_sets::Mode::Confirm)));
}

fn finish(app: &mut App) {
    until(|| {
        tick(app);
        !app.workspace_set_switching() && !app.workspace_opening() && app.workspaces.exit.is_none()
    });
    assert!(!app.shutdown_anim_active);
}

fn finish_without_confirmation(app: &mut App) {
    until(|| {
        tick(app);
        assert!(!matches!(app.workspaces.popup.sets.mode, Some(crate::ui_workspace_sets::Mode::Confirm)),
            "clean workbenches must not show an Open folder confirmation");
        !app.workspace_set_switching() && !app.workspace_opening() && app.workspaces.exit.is_none()
    });
    assert!(!app.shutdown_anim_active);
}

#[test]
fn open_folder_and_ctrl_o_replace_while_add_folder_keeps_existing_workbenches() {
    let a = Project::new();
    let b = Project::new();
    let mut app = a.app(Default::default());
    crate::input_keyboard::handle_keyboard_event(&mut app,
        KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    assert!(app.change_dir_active && !app.folder_browser.add_workspace);
    app.folder_browser_action(crate::folder_browser::Action::Close);

    app.execute_palette_command("workspace_add");
    assert!(app.change_dir_active && app.folder_browser.add_workspace);
    app.folder_browser.folder = b.path().display().to_string();
    app.confirm_folder_browser();
    assert!(matches!(app.workspaces.request, Some(Request::Add(_))));
    until(|| {
        app.poll_workspace_requests();
        services(&mut app);
        app.open_folder_count() == 2 && !app.workspace_opening()
    });
    assert_eq!(app.open_folder_count(), 2);
    assert_eq!(app.explorer.root, a.path());
    assert_eq!(app.workspaces.parked[0].app.explorer.root, b.path());
    assert_eq!(app.editor().unwrap().rope.to_string(), "original");
    assert!(app.workspace_set_confirmation().is_none());
}

#[test]
fn open_folder_warns_before_closing_all_folders_and_keeps_unsaved_edits_for_recovery() {
    let a = Project::new();
    let b = Project::new();
    let c = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.editor_mut().unwrap().insert_char('A');
    let mut terminal = TerminalTab::new("Build".into(), a.path().into());
    terminal.terminal_pty = Some(TerminalEmulator::test_with_output(40, 8, "still running"));
    let running = terminal.terminal_pty.as_mut().unwrap().test_keep_running();
    app.terminals = vec![terminal];
    app.insert_workspace(b.app(wake));
    app.editor_mut().unwrap().insert_char('B');
    let identity = app.editor().unwrap().save_identity;
    choose(&mut app, c.path());
    confirmation(&mut app);
    assert_eq!(app.open_folder_count(), 2);
    assert_eq!(app.explorer.root, b.path());
    assert_eq!(app.workspace_set_confirmation().unwrap().2.len(), 2);
    let buffer = draw(&mut app, 120, 30);
    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    for expected in ["Open folder", "open files will close", "terminals and agents will stop", "Unsaved edits will be kept", "Cancel"] {
        assert!(text.contains(expected), "missing warning: {expected}");
    }
    // The initial action is Cancel, including when Enter is pressed immediately.
    crate::ui_workspaces::handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(!app.workspace_set_switching());
    assert_eq!(app.open_folder_count(), 2);
    assert_eq!(app.editor().unwrap().save_identity, identity);
    assert!(app.workspaces.parked[0].app.terminals[0].terminal_pty.is_some());

    choose(&mut app, c.path());
    confirmation(&mut app);
    let cancel = app.workspaces.popup.selected;
    for height in [4, 6, 9] {
        app.workspaces.popup.selected = cancel;
        crate::ui_workspaces::handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        let buffer = draw(&mut app, 60, height);
        let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("Open folder") && text.contains("Cancel"), "actions missing at height {height}");
    }
    crate::ui_workspaces::handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    finish(&mut app);
    assert_eq!(app.explorer.root, c.path());
    assert_eq!(app.open_folder_count(), 1);
    assert!(app.workspaces.parked.is_empty());
    assert!(running.send(()).is_err(), "the old terminal must close after confirmation");
    for (project, text) in [(&a, "Aoriginal"), (&b, "Boriginal")] {
        let mut session = crate::session::load_session(project.path()).unwrap();
        crate::session::hydrate_saved_tab(project.path(), &mut session.tabs[0]);
        assert_eq!(session.tabs[0].pending_unsaved_content.as_ref().unwrap().to_string(), text);
    }
}

#[test]
fn open_folder_reuses_an_open_destination_and_reopening_the_current_folder_is_a_noop() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    let identity = app.editor().unwrap().save_identity;
    app.editor_mut().unwrap().insert_char('A');
    choose(&mut app, &a.path().join("."));
    finish(&mut app);
    assert_eq!(app.editor().unwrap().save_identity, identity);
    assert_eq!(app.open_folder_count(), 1);
    assert!(app.workspace_set_confirmation().is_none());
    app.insert_workspace(b.app(wake));
    choose(&mut app, a.path());
    finish_without_confirmation(&mut app);
    assert_eq!(app.explorer.root, a.path());
    assert_eq!(app.editor().unwrap().save_identity, identity);
    assert_eq!(app.editor().unwrap().rope.to_string(), "Aoriginal");
    assert!(app.workspaces.parked.is_empty());
}

#[test]
fn open_folder_handles_empty_windows_and_warns_before_closing_loose_editors() {
    for with_file in [false, true] {
        let destination = Project::new();
        let mut app = App::new_startup(PathBuf::new(), Default::default());
        app.source_control_enabled = false;
        app.lsp_manager.configs.clear();
        if with_file {
            let mut editor = Editor::test_with_text("loose draft");
            editor.insert_char('!');
            app.tabs.push(Tab::new_file(editor));
            app.active_pane = ActivePane::Editor;
        }
        choose(&mut app, destination.path());
        if with_file {
            confirmation(&mut app);
            assert!(!app.has_open_folder());
            assert_eq!(app.editor().unwrap().rope.to_string(), "!loose draft");
            assert_eq!(app.workspace_set_confirmation().unwrap().2[0].name, "No folder");
            app.confirm_workspace_set();
        }
        finish(&mut app);
        assert_eq!(app.explorer.root, destination.path());
        assert_eq!(app.open_folder_count(), 1);
        assert!(app.workspaces.parked.is_empty());
    }
}

#[test]
fn open_folder_from_the_empty_folder_picker_uses_replacement_mode() {
    let mut app = App::new_startup(PathBuf::new(), Default::default());
    app.tabs.push(Tab::new_file(Editor::test_with_text("loose file")));
    crate::ui_workspaces::open(&mut app);
    crate::ui_workspaces::handle_key(&mut app, KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
    crate::ui_workspaces::handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.change_dir_active && !app.folder_browser.add_workspace);
    assert_eq!(app.editor().unwrap().rope.to_string(), "loose file");
}

#[test]
fn open_folder_keeps_the_workbench_when_the_destination_disappears() {
    let a = Project::new();
    let b = Project::new();
    let mut app = a.app(Default::default());
    app.editor_mut().unwrap().insert_char('!');
    let identity = app.editor().unwrap().save_identity;
    app.workspaces.request = Some(Request::Replace(a.path().join("missing")));
    finish(&mut app);
    assert_eq!(app.explorer.root, a.path());
    assert_eq!(app.editor().unwrap().save_identity, identity);
    choose(&mut app, b.path());
    confirmation(&mut app);
    std::fs::remove_dir(b.path()).unwrap();
    app.confirm_workspace_set();
    finish(&mut app);
    assert_eq!(app.explorer.root, a.path());
    assert_eq!(app.editor().unwrap().rope.to_string(), "!original");
    assert_eq!(app.editor().unwrap().save_identity, identity);
    assert!(app.workspaces.parked.is_empty());
}

#[test]
fn open_folder_keeps_unsaved_editors_open_when_recovery_cannot_be_saved() {
    let a = Project::new();
    let b = Project::new();
    let mut app = a.app(Default::default());
    app.editor_mut().unwrap().insert_char('!');
    let identity = app.editor().unwrap().save_identity;
    std::fs::create_dir_all(crate::session::session_path(a.path())).unwrap();
    choose(&mut app, b.path());
    confirmation(&mut app);
    app.confirm_workspace_set();
    finish(&mut app);
    assert_eq!(app.explorer.root, a.path());
    assert_eq!(app.editor().unwrap().save_identity, identity);
    assert_eq!(app.editor().unwrap().rope.to_string(), "!original");
    assert!(app.editor().unwrap().modified);
    assert!(app.save_error.is_some());
}

#[test]
fn open_folder_replaces_remote_roots_without_disconnect_or_an_extra_list_entry() {
    let a = Project::new();
    let b = Project::new();
    let workspace = crate::remote::workspace::Workspace::new(headless_connection(a.path()));
    let destination = workspace.view_key(&crate::remote::git::from_native(b.path(), a.path()).unwrap());
    let mut app = a.app(Default::default());
    app.explorer.root = workspace.view_root.clone();
    app.remote_workspace = Some(workspace.clone());
    choose(&mut app, &destination);
    finish_without_confirmation(&mut app);
    assert_eq!(app.open_folder_count(), 1);
    assert!(app.workspaces.parked.is_empty());
    let current = app.remote_workspace.as_ref().unwrap();
    assert!(current.client.connected());
    assert_ne!(current.view_root, workspace.view_root);
    assert_eq!(current.client.hello().root_uri, url::Url::from_directory_path(b.path()).unwrap().as_str());
    current.client.call(crate::remote::Command::Shutdown).unwrap();
}
