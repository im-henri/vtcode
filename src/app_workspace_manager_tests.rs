use super::sets_tests::tick;
use super::*;
use crate::workspace_sets::{Location, SavedSet, Scope};

#[test]
fn add_folder_keeps_the_current_editor_terminal_and_existing_folder_focus() {
    let a = Project::new();
    let b = Project::new();
    let mut app = a.app(Default::default());
    app.editor_mut().unwrap().insert_char('!');
    let identity = app.editor().unwrap().save_identity;
    let mut terminal = TerminalEmulator::test_with_output(40, 8, "running build");
    let running = terminal.test_keep_running();
    app.terminals[0].terminal_pty = Some(terminal);
    let active = app.workspaces.active;
    app.workspaces.request = Some(Request::Add(b.path().to_owned()));
    until(|| {
        tick(&mut app);
        assert_eq!(app.workspaces.active, active);
        assert_eq!(app.explorer.root, a.path());
        assert_eq!(app.editor().unwrap().save_identity, identity);
        app.open_folder_count() == 2 && !app.workspace_opening()
    });
    assert!(running.send(()).is_ok());
    app.workspaces.request = Some(Request::Add(b.path().to_owned()));
    tick(&mut app);
    assert_eq!(app.workspaces.active, active);
    assert_eq!(app.open_folder_count(), 2);
}

#[test]
fn named_workspace_status_tracks_membership_but_not_folder_focus() {
    let a = Project::new();
    let b = Project::new();
    let c = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.park_new_workspace(b.app(wake.clone()));
    app.workspaces.sets.loaded = true;
    app.workspaces.sets.active = Some(7);
    app.workspaces.sets.catalog.sets.push(SavedSet {
        id: 7,
        name: "Development".into(),
        scope: Scope::Local,
        roots: [a.path(), b.path()]
            .into_iter()
            .map(|root| Location::local(root, &crate::portable::base_dir()))
            .collect(),
        active: 0,
    });
    assert_eq!(
        app.current_workspace_set()
            .map(|(set, modified)| (set.name, modified)),
        Some(("Development".into(), false))
    );
    let other = app.workspaces.parked[0].id;
    app.switch_workspace(other);
    assert!(!app.current_workspace_set().unwrap().1);
    app.park_new_workspace(c.app(wake));
    assert!(app.current_workspace_set().unwrap().1);
    app.workspaces.parked.pop();
    assert!(!app.current_workspace_set().unwrap().1);
    let mut duplicate = app.saved_workspace_sets()[0].clone();
    duplicate.id = 8;
    duplicate.name = "Another collection".into();
    app.workspaces.sets.catalog.sets.push(duplicate);
    app.workspaces.sets.catalog.sets.retain(|set| set.id != 7);
    assert!(
        app.current_workspace_set().is_none(),
        "deleting the active workspace must not activate a different name with identical folders"
    );
}

#[test]
fn closing_an_inactive_folder_stops_only_its_session_and_returns_to_the_current_folder() {
    let a = Project::new();
    let b = Project::new();
    let c = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.park_new_workspace(b.app(wake.clone()));
    app.park_new_workspace(c.app(wake));
    let current = app.workspaces.active;
    let target = app.workspaces.parked[0].id;
    let mut terminal = TerminalEmulator::test_with_output(40, 8, "target shell");
    let running = terminal.test_keep_running();
    app.workspaces.parked[0].app.terminals[0].terminal_pty = Some(terminal);
    app.workspaces.parked[0]
        .app
        .editor_mut()
        .unwrap()
        .insert_char('!');
    app.workspaces.request = Some(Request::CloseFolder(target));
    until(|| {
        tick(&mut app);
        !app.workspace_entries()
            .iter()
            .any(|entry| entry.id == target)
    });
    assert_eq!(app.workspaces.active, current);
    assert_eq!(app.explorer.root, a.path());
    assert_eq!(app.open_folder_count(), 2);
    assert!(running.send(()).is_err());
    assert!(!app.shutdown_anim_active);
    assert!(crate::session::load_session(b.path()).is_some());
}

#[test]
fn closing_the_last_folder_keeps_vtcode_open_in_an_empty_window() {
    let a = Project::new();
    let mut app = a.app(Default::default());
    app.workspaces.sets.active = Some(42);
    app.editor_mut().unwrap().insert_char('!');
    let text = app.editor().unwrap().rope.to_string();
    app.workspaces.request = Some(Request::CloseFolder(app.workspaces.active));
    until(|| {
        tick(&mut app);
        !app.has_open_folder() && !app.workspace_opening() && app.workspaces.exit.is_none()
    });
    assert!(!app.shutdown_anim_active);
    assert_eq!(app.open_folder_count(), 0);
    assert!(app.workspaces.sets.active.is_none());
    let mut session = crate::session::load_session(a.path()).unwrap();
    crate::session::hydrate_saved_tab(a.path(), &mut session.tabs[0]);
    assert_eq!(
        session.tabs[0]
            .pending_unsaved_content
            .as_ref()
            .unwrap()
            .to_string(),
        text
    );
}

#[test]
fn added_folder_appears_while_the_workspace_manager_stays_open() {
    let a = Project::new();
    let b = Project::new();
    let mut app = a.app(Default::default());
    app.workspaces.sets.loaded = true;
    app.workspaces.request = Some(Request::Add(b.path().to_owned()));
    app.poll_workspace_requests();
    crate::ui_workspaces::open(&mut app);
    until(|| {
        tick(&mut app);
        assert!(app.workspaces.popup.open);
        assert_eq!(app.explorer.root, a.path());
        app.open_folder_count() == 2 && !app.workspace_opening()
    });
}

#[test]
fn saving_and_updating_a_workspace_marks_the_named_collection_active() {
    let a = Project::new();
    let b = Project::new();
    let mut app = a.app(Default::default());
    app.workspaces.sets.path = a.path().join("sets.json");
    app.save_workspace_set("Development".into(), None);
    until(|| {
        tick(&mut app);
        !app.workspace_sets_busy()
    });
    let (saved, modified) = app.current_workspace_set().unwrap();
    assert!(!modified);
    assert_eq!(app.workspaces.sets.active, Some(saved.id));
    app.park_new_workspace(b.app(Default::default()));
    assert!(app.current_workspace_set().unwrap().1);
    app.save_workspace_set(saved.name, Some(saved.id));
    until(|| {
        tick(&mut app);
        !app.workspace_sets_busy()
    });
    assert_eq!(app.current_workspace_set().unwrap().0.id, saved.id);
    assert!(!app.current_workspace_set().unwrap().1);
}
