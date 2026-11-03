use super::*;
use crate::workspace_sets::{self, Location, Mutation, Scope};

fn tick(app: &mut App) {
    app.poll_workspace_requests();
    services(app);
    if app.workspace_set_preserving() {
        crate::app_event_loop::queue_background_workspace_auto_saves(app);
    }
    if app.exit_keep_unsaved_quit {
        if !app.custom_exit_started {
            app.start_custom_exit(true);
        }
        if app.custom_exit_remaining == 0
            && app.pending_file_saves.is_empty()
            && app.deferred_file_saves.is_empty()
        {
            if app.exit_recovery.saved {
                app.shutdown_anim_active = true;
            } else if !app.exit_recovery.pending() {
                app.start_exit_recovery();
            }
        }
    }
    app.advance_workspace_exit();
}
fn catalog(app: &mut App, path: &Path) {
    app.workspaces.sets.path = path.into();
    app.load_workspace_sets(true);
    until(|| {
        tick(app);
        !app.workspace_sets_busy()
    });
}
fn save(app: &mut App, name: &str) -> u64 {
    app.save_workspace_set(name.into(), None);
    until(|| {
        tick(app);
        !app.workspace_sets_busy()
    });
    assert!(
        app.workspaces.sets.error.is_none(),
        "{:?}",
        app.workspaces.sets.error
    );
    app.saved_workspace_sets()
        .iter()
        .find(|set| set.name == name)
        .unwrap()
        .id
}
fn start(app: &mut App, id: u64) {
    app.workspaces.popup = Default::default();
    app.workspaces.request = Some(Request::OpenSet(id));
    app.poll_workspace_requests();
}

#[test]
fn sets_switch_exact_membership_reuse_dirty_documents_and_keep_closed_recovery() {
    let a = Project::new();
    let b = Project::new();
    let c = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.workspaces.sets.path = a.path().join("sets.json");
    app.insert_workspace(b.app(wake.clone()));
    let identity = app.editor().unwrap().save_identity;
    app.editor_mut().unwrap().insert_char('B');
    let id = save(&mut app, "Engineering");
    let disk = app.workspaces.sets.path.clone();
    app.insert_workspace(c.app(wake));
    app.editor_mut().unwrap().insert_char('C');
    start(&mut app, id);
    until(|| {
        tick(&mut app);
        app.workspace_set_confirmation().is_some()
    });
    let (_, count, close) = app.workspace_set_confirmation().unwrap();
    assert_eq!(count, 2);
    assert_eq!(close.len(), 1);
    assert_eq!(close[0].id, 2);
    crate::ui_control_visibility_tests::snapshot(
        &draw(&mut app, 120, 28),
        "Workspace set switch confirmation",
    );
    assert_eq!(
        app.workspace_entries().len(),
        3,
        "no folder closes before confirmation"
    );
    app.confirm_workspace_set();
    until(|| {
        tick(&mut app);
        !app.workspace_set_switching()
    });
    assert_eq!(app.workspaces.active, 1);
    assert_eq!(app.editor().unwrap().save_identity, identity);
    assert!(app.editor().unwrap().rope.to_string().starts_with('B'));
    assert_eq!(app.workspace_entries().len(), 2);
    let mut recovered = crate::session::load_session(c.path()).unwrap();
    crate::session::hydrate_saved_tab(c.path(), &mut recovered.tabs[0]);
    assert!(
        recovered.tabs[0]
            .pending_unsaved_content
            .as_ref()
            .unwrap()
            .to_string()
            .starts_with('C')
    );
    let mut restarted = a.app(Default::default());
    catalog(&mut restarted, &disk);
    assert_eq!(restarted.saved_workspace_sets()[0].name, "Engineering");
}

#[test]
fn opening_missing_folders_is_atomic_and_cancel_leaves_jobs_and_documents_open() {
    let a = Project::new();
    let b = Project::new();
    let mut app = a.app(Default::default());
    let disk = a.path().join("sets.json");
    let target = Mutation::Save {
        name: "Other".into(),
        scope: Scope::Local,
        roots: vec![Location::local(b.path(), &crate::portable::base_dir())],
        active: 0,
        replace: None,
    };
    workspace_sets::mutate(&disk, target).unwrap();
    catalog(&mut app, &disk);
    let id = app.saved_workspace_sets()[0].id;
    let identity = app.editor().unwrap().save_identity;
    start(&mut app, id);
    until(|| {
        tick(&mut app);
        app.workspace_set_confirmation().is_some()
    });
    crate::ui_workspaces::handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.workspace_set_switching());
    assert_eq!(app.editor().unwrap().save_identity, identity);
    app.workspaces.popup = Default::default();
    std::fs::remove_dir(b.path()).unwrap();
    start(&mut app, id);
    until(|| {
        tick(&mut app);
        !app.workspace_set_switching()
    });
    assert_eq!(app.editor().unwrap().save_identity, identity);
    assert_eq!(app.workspace_entries().len(), 1);
}

#[test]
fn new_target_is_loaded_before_last_old_workspace_closes_and_changes_are_revalidated() {
    let a = Project::new();
    let b = Project::new();
    let mut app = a.app(Default::default());
    let disk = a.path().join("sets.json");
    workspace_sets::mutate(
        &disk,
        Mutation::Save {
            name: "Solo".into(),
            scope: Scope::Local,
            roots: vec![Location::local(b.path(), &crate::portable::base_dir())],
            active: 0,
            replace: None,
        },
    )
    .unwrap();
    catalog(&mut app, &disk);
    let id = app.saved_workspace_sets()[0].id;
    start(&mut app, id);
    until(|| {
        tick(&mut app);
        app.workspace_set_confirmation().is_some()
    });
    std::fs::remove_dir(b.path()).unwrap();
    app.confirm_workspace_set();
    until(|| {
        tick(&mut app);
        !app.workspace_set_switching()
    });
    assert_eq!(app.explorer.root, a.path());
    std::fs::create_dir(b.path()).unwrap();
    start(&mut app, id);
    until(|| {
        tick(&mut app);
        app.workspace_set_confirmation().is_some()
    });
    app.confirm_workspace_set();
    until(|| {
        tick(&mut app);
        !app.workspace_set_switching()
    });
    assert_eq!(app.explorer.root, b.path());
    assert!(app.workspaces.parked.is_empty());
    assert!(!app.shutdown_anim_active);
}

#[test]
fn remote_sets_are_connection_scoped_and_reuse_live_workspaces_without_rpc() {
    let a = Project::new();
    let b = Project::new();
    let c = Project::new();
    let first = crate::remote::workspace::Workspace::new(headless_connection(a.path()));
    let second = first
        .open_workspace(
            &first.view_key(&crate::remote::git::from_native(b.path(), a.path()).unwrap()),
        )
        .unwrap();
    assert_eq!(first.workspace_sets_scope(), second.workspace_sets_scope());
    let other = crate::remote::workspace::Workspace::new(headless_connection(c.path()));
    assert_ne!(first.workspace_sets_scope(), other.workspace_sets_scope());
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.explorer.root = first.view_root.clone();
    app.remote_workspace = Some(first.clone());
    let mut next = b.app(wake);
    next.explorer.root = second.view_root.clone();
    next.remote_workspace = Some(second);
    app.insert_workspace(next);
    app.workspaces.sets.path = a.path().join("sets.json");
    let id = save(&mut app, "Remote work");
    let identity = app.editor().unwrap().save_identity;
    first.disconnect();
    app.switch_workspace(0);
    start(&mut app, id);
    until(|| {
        tick(&mut app);
        !app.workspace_set_switching()
    });
    assert_eq!(app.workspace_entries().len(), 2);
    assert_eq!(app.workspaces.active, 1);
    assert_eq!(app.editor().unwrap().save_identity, identity);
    app.remote_workspace = Some(other.clone());
    assert!(app.saved_workspace_sets().is_empty());
    other.disconnect();
}

#[test]
fn title_button_directory_menu_name_input_and_small_frames_share_hit_geometry() {
    let project = Project::new();
    let mut app = project.app(Default::default());
    app.workspaces.sets.path = project.path().join("sets.json");
    for width in [48, 80, 140] {
        draw(&mut app, width, 28);
        let (directory, _) = crate::ui_chrome::workspace_controls(&app).unwrap();
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: directory.x,
                row: directory.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        draw(&mut app, width, 28);
        let saved_tab = app.workspaces.popup.tabs[1];
        crate::mouse::handle_mouse(&mut app, MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left), column: saved_tab.x, row: saved_tab.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(
            app.workspaces.popup.sets.mode,
            Some(crate::ui_workspace_sets::Mode::List)
        ));
        until(|| {
            tick(&mut app);
            !app.workspace_sets_busy()
        });
        app.workspaces.popup = Default::default();
    }
    crate::ui_workspaces::open(&mut app);
    let buffer = draw(&mut app, 120, 28);
    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    assert!(text.contains("Saved workspaces") && text.contains("Save workspace"));
    crate::ui_control_visibility_tests::snapshot(&buffer, "Workspace sets directory menu");
    crate::ui_workspace_sets::save(&mut app);
    crate::app_event_loop::handle_paste_input(&mut app, "Engineering λ🦀");
    crate::ui_control_visibility_tests::snapshot(
        &draw(&mut app, 120, 28),
        "Save workspace set name",
    );
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    until(|| {
        tick(&mut app);
        !app.workspace_sets_busy()
    });
    assert_eq!(app.saved_workspace_sets()[0].name, "Engineering λ🦀");
    crate::ui_control_visibility_tests::snapshot(
        &draw(&mut app, 120, 28),
        "Saved workspace sets list",
    );
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE),
    );
    crate::app_event_loop::handle_paste_input(&mut app, "Renamed set");
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    until(|| {
        tick(&mut app);
        !app.workspace_sets_busy()
    });
    assert_eq!(app.saved_workspace_sets()[0].name, "Renamed set");
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
    );
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert_eq!(app.saved_workspace_sets().len(), 1);
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE),
    );
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
    );
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    until(|| {
        tick(&mut app);
        !app.workspace_sets_busy()
    });
    assert!(app.saved_workspace_sets().is_empty());
    for (width, height) in [(1, 1), (4, 4), (20, 6), (48, 12)] {
        crate::ui_workspace_sets::open(&mut app);
        draw(&mut app, width, height);
        app.workspaces.popup = Default::default();
    }
}

#[test]
fn remote_sets_load_missing_workspaces_and_close_extras_on_the_shared_transport() {
    let a = Project::new();
    let b = Project::new();
    let c = Project::new();
    let first = crate::remote::workspace::Workspace::new(headless_connection(a.path()));
    let second = first
        .open_workspace(
            &first.view_key(&crate::remote::git::from_native(b.path(), a.path()).unwrap()),
        )
        .unwrap();
    let third = first
        .open_workspace(
            &first.view_key(&crate::remote::git::from_native(c.path(), a.path()).unwrap()),
        )
        .unwrap();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.explorer.root = first.view_root.clone();
    app.remote_workspace = Some(first.clone());
    let kept = app.editor().unwrap().save_identity;
    let mut next = b.app(wake);
    next.explorer.root = second.view_root.clone();
    next.remote_workspace = Some(second);
    app.insert_workspace(next);
    let disk = a.path().join("sets.json");
    workspace_sets::mutate(
        &disk,
        Mutation::Save {
            name: "Remote pair".into(),
            scope: app.workspace_sets_scope(),
            roots: vec![
                Location::Remote {
                    uri: first.client.hello().root_uri.clone(),
                },
                Location::Remote {
                    uri: third.client.hello().root_uri.clone(),
                },
            ],
            active: 1,
            replace: None,
        },
    )
    .unwrap();
    catalog(&mut app, &disk);
    let id = app.saved_workspace_sets()[0].id;
    start(&mut app, id);
    until(|| {
        tick(&mut app);
        app.workspace_set_confirmation().is_some()
    });
    app.confirm_workspace_set();
    until(|| {
        tick(&mut app);
        !app.workspace_set_switching()
    });
    assert_eq!(app.explorer.root, third.view_root);
    assert_eq!(app.workspace_entries().len(), 2);
    assert!(first.client.connected());
    assert!(
        app.remote_workspace
            .as_ref()
            .unwrap()
            .client
            .same_transport(&first.client)
    );
    assert_eq!(
        app.workspaces.parked[0].app.editor().unwrap().save_identity,
        kept
    );
    app.remote_workspace
        .as_ref()
        .unwrap()
        .client
        .call(crate::remote::Command::List {
            path: Default::default(),
        })
        .unwrap();
    first.disconnect();
}

#[test]
fn pending_catalog_save_blocks_quit_and_completion_does_not_steal_editor_focus() {
    let a = Project::new();
    let mut app = a.app(Default::default());
    app.workspaces.sets.path = a.path().join("sets.json");
    app.save_workspace_set("Saved".into(), None);
    app.exit_confirm_active = true;
    assert_eq!(
        crate::input_dialogs::handle_exit_confirm_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)
        ),
        Some(false)
    );
    assert!(!app.exit_keep_unsaved_quit);
    app.exit_confirm_active = false;
    until(|| {
        tick(&mut app);
        !app.workspace_sets_busy()
    });
    assert!(!app.workspaces.popup.open);
    assert_eq!(app.saved_workspace_sets().len(), 1);
}

#[test]
fn keep_and_switch_does_not_autosave_any_workspace_that_will_close() {
    for mode in [
        crate::app::AutoSaveMode::OnFocusChange,
        crate::app::AutoSaveMode::AfterDelay,
    ] {
        let projects: Vec<_> = (0..3).map(|_| Project::new()).collect();
        let wake = crate::background::UiWake::default();
        let mut app = projects[0].app(wake.clone());
        for project in &projects[1..] {
            app.insert_workspace(project.app(wake.clone()));
        }
        app.switch_workspace(0);
        for id in 0..2 {
            app.switch_workspace(id);
            let path = projects[id as usize].path().join("file.txt");
            std::fs::write(&path, "disk").unwrap();
            app.editor_mut().unwrap().open_file(&path).unwrap();
            app.editor_mut()
                .unwrap()
                .insert_char(char::from(b'A' + id as u8));
        }
        app.switch_workspace(0);
        let disk = projects[0].path().join("sets.json");
        workspace_sets::mutate(
            &disk,
            Mutation::Save {
                name: "Only kept folder".into(),
                scope: Scope::Local,
                roots: vec![Location::local(
                    projects[2].path(),
                    &crate::portable::base_dir(),
                )],
                active: 0,
                replace: None,
            },
        )
        .unwrap();
        catalog(&mut app, &disk);
        let id = app.saved_workspace_sets()[0].id;
        start(&mut app, id);
        until(|| {
            tick(&mut app);
            app.workspace_set_confirmation().is_some()
        });
        app.auto_save = mode;
        app.auto_save_delay = 0;
        app.last_edit_time = Instant::now() - Duration::from_secs(10);
        for parked in &mut app.workspaces.parked {
            parked.app.auto_save = mode;
            parked.app.auto_save_delay = 0;
            parked.app.last_edit_time = app.last_edit_time;
        }
        app.confirm_workspace_set();
        until(|| {
            tick(&mut app);
            !app.workspace_set_switching()
        });
        assert_eq!(app.explorer.root, projects[2].path());
        for project in &projects[..2] {
            assert_eq!(
                std::fs::read_to_string(project.path().join("file.txt")).unwrap(),
                "disk"
            );
        }
    }
}

#[test]
fn cancelling_loading_set_ignores_late_worker_replies() {
    let a = Project::new();
    let b = Project::new();
    let mut app = a.app(Default::default());
    let disk = a.path().join("sets.json");
    workspace_sets::mutate(
        &disk,
        Mutation::Save {
            name: "Other folder".into(),
            scope: Scope::Local,
            roots: vec![Location::local(b.path(), &crate::portable::base_dir())],
            active: 0,
            replace: None,
        },
    )
    .unwrap();
    catalog(&mut app, &disk);
    let id = app.saved_workspace_sets()[0].id;
    let identity = app.editor().unwrap().save_identity;
    start(&mut app, id);
    app.cancel_workspace_set_switch();
    until(|| {
        tick(&mut app);
        app.background.active_job_count() == 0
    });
    assert_eq!(app.workspace_entries().len(), 1);
    assert_eq!(app.editor().unwrap().save_identity, identity);
    assert!(!app.workspace_set_switching());
}

#[test]
fn hovering_a_set_redraws_its_row_and_management_targets_that_set() {
    let a = Project::new();
    let mut app = a.app(Default::default());
    let disk = a.path().join("sets.json");
    for name in ["Alpha", "Beta"] {
        workspace_sets::mutate(
            &disk,
            Mutation::Save {
                name: name.into(),
                scope: Scope::Local,
                roots: vec![Location::local(a.path(), &crate::portable::base_dir())],
                active: 0,
                replace: None,
            },
        )
        .unwrap();
    }
    catalog(&mut app, &disk);
    crate::ui_workspace_sets::open(&mut app);
    let buffer = draw(&mut app, 120, 28);
    let area = app.workspaces.popup.area;
    let row = (area.y..area.bottom()).find(|&y| {
        (area.x..area.right()).map(|x| buffer[(x, y)].symbol()).collect::<String>().contains("Beta")
    }).unwrap();
    app.mouse_hover_changed = false;
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Moved,
            column: area.x + 2,
            row,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert!(app.mouse_hover_changed);
    let buffer = draw(&mut app, 120, 28);
    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    assert!(!text.contains("Update from open folders"));
    crate::mouse::handle_mouse(&mut app, MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left), column: area.right() - 3, row,
        modifiers: KeyModifiers::NONE,
    });
    let buffer = draw(&mut app, 120, 28);
    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    assert!(text.contains("Beta") && text.contains("Update from open folders"));
}
