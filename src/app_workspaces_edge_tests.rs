use super::*;
use crate::{
    app::AutoSaveMode,
    app_remote_connection::Phase,
    remote::{self, Command, RemotePath},
};

fn ready_open(app: &mut App, root: &Path) {
    let (sender, receiver) = mpsc::channel();
    sender
        .send(Ok(Prepared {
            startup: crate::startup::Prepared {
                target: crate::startup::Target {
                    root: root.into(),
                    file: None,
                    line: None,
                    column: None,
                },
                global: Default::default(),
                workspace: Default::default(),
                session: None,
            },
            remote: None,
            equivalent_roots: Vec::new(),
        }))
        .unwrap();
    app.workspaces.opening = Some(Opening {
        receiver,
        foreground: true,
        close_remote: None,
    });
}

#[test]
fn ready_folder_load_waits_for_modal_and_respects_a_new_workspace_selection() {
    let a = Project::new();
    let b = Project::new();
    let c = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.insert_workspace(b.app(wake));
    ready_open(&mut app, c.path());
    app.show_save_error("Keep this save error visible");
    assert!(!app.poll_workspace_requests());
    assert_eq!(app.workspaces.active, 1);
    assert!(app.workspace_opening());
    app.dismiss_save_error();
    app.workspaces.request = Some(Request::Switch(0));
    app.poll_workspace_requests();
    assert_eq!(app.workspaces.active, 0);
    assert_eq!(app.workspaces.parked.len(), 2);
    assert!(
        app.workspaces
            .parked
            .iter()
            .any(|parked| parked.app.explorer.root == c.path())
    );
    assert!(!app.workspace_opening());
}

#[test]
fn incoming_paste_cannot_be_redirected_to_another_workspace() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.insert_workspace(b.app(wake));
    app.terminal_paste_status(crossterm::event::PasteStatus::Receiving { bytes: 100 });
    app.workspaces.request = Some(Request::Switch(0));
    app.poll_workspace_requests();
    assert_eq!(app.workspaces.active, 1);
    crate::app_event_loop::handle_paste_input(&mut app, "correct workspace");
    assert!(
        app.editor()
            .unwrap()
            .rope
            .to_string()
            .contains("correct workspace")
    );
    assert_eq!(
        app.workspaces.parked[0]
            .app
            .editor()
            .unwrap()
            .rope
            .to_string(),
        "original"
    );
}

#[test]
fn retrying_save_after_pending_io_still_saves_every_workspace() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    for project in [&a, &b] {
        std::fs::write(project.path().join("file.txt"), "disk").unwrap();
    }
    app.editor_mut()
        .unwrap()
        .open_file(&a.path().join("file.txt"))
        .unwrap();
    app.editor_mut().unwrap().insert_char('A');
    let mut next = b.app(wake);
    next.editor_mut()
        .unwrap()
        .open_file(&b.path().join("file.txt"))
        .unwrap();
    next.editor_mut().unwrap().insert_char('B');
    app.insert_workspace(next);
    app.pending_file_saves
        .insert(b.path().join("other.txt"), (0, 1.into()));
    app.workspaces.request = Some(Request::Exit('y'));
    app.poll_workspace_requests();
    assert!(app.workspaces.exit.is_some() && app.exit_confirm_active);
    assert!(!app.shutdown_anim_active);
    app.pending_file_saves.clear();
    assert_eq!(
        crate::input_dialogs::handle_exit_confirm_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)
        ),
        Some(false)
    );
    assert!(app.advance_workspace_exit());
    assert_eq!(app.workspaces.active, 0);
    assert!(!app.advance_workspace_exit());
    for (project, prefix) in [(&a, 'A'), (&b, 'B')] {
        assert!(
            std::fs::read_to_string(project.path().join("file.txt"))
                .unwrap()
                .starts_with(prefix)
        );
    }
}

#[test]
fn late_edits_after_one_workspace_is_backed_up_cancel_quit_without_losing_them() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.insert_workspace(b.app(wake));
    app.editor_mut().unwrap().insert_char('B');
    app.workspaces.request = Some(Request::Exit('k'));
    app.poll_workspace_requests();
    app.start_exit_recovery();
    until(|| {
        services(&mut app);
        app.exit_recovery.saved
    });
    app.shutdown_anim_active = true;
    assert!(app.advance_workspace_exit());
    assert_eq!(app.workspaces.active, 0);
    app.workspaces
        .parked
        .iter_mut()
        .find(|parked| parked.id == 1)
        .unwrap()
        .app
        .editor_mut()
        .unwrap()
        .insert_char('!');
    app.start_exit_recovery();
    until(|| {
        services(&mut app);
        app.exit_recovery.saved
    });
    app.shutdown_anim_active = true;
    assert!(app.advance_workspace_exit());
    assert_eq!(app.workspaces.active, 1);
    assert!(app.workspaces.exit.is_none() && !app.shutdown_anim_active);
    assert!(app.editor().unwrap().rope.to_string().contains('!'));
    assert!(
        app.save_error
            .as_ref()
            .unwrap()
            .contains("changed while closing")
    );
}

#[test]
fn failed_hidden_saves_produce_attention_and_counts_cannot_overflow() {
    let projects: Vec<_> = (0..3).map(|_| Project::new()).collect();
    let wake = crate::background::UiWake::default();
    let mut app = projects[0].app(wake.clone());
    app.insert_workspace(projects[1].app(wake.clone()));
    app.insert_workspace(projects[2].app(wake));
    for parked in &mut app.workspaces.parked {
        parked.app.show_save_error("Save failed while hidden");
    }
    assert_eq!(app.workspace_unread(), 2);
    assert!(app.workspace_needs_attention());
    for parked in &mut app.workspaces.parked {
        parked.app.workspace_activity.unread = usize::MAX;
    }
    assert_eq!(app.workspace_unread(), usize::MAX);
}

#[test]
fn hidden_due_auto_save_does_not_override_keep_or_discard() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    let path = a.path().join("file.txt");
    std::fs::write(&path, "disk").unwrap();
    app.editor_mut().unwrap().open_file(&path).unwrap();
    app.editor_mut().unwrap().insert_char('!');
    app.insert_workspace(b.app(wake));
    let parked = &mut app.workspaces.parked[0].app;
    parked.auto_save = AutoSaveMode::AfterDelay;
    parked.auto_save_delay = 0;
    parked.last_edit_time = Instant::now() - Duration::from_secs(10);
    parked.exit_keep_unsaved_quit = true;
    crate::app_event_loop::queue_background_workspace_auto_saves(parked);
    assert!(parked.pending_file_saves.is_empty());
    parked.exit_keep_unsaved_quit = false;
    parked.exit_discard_quit = true;
    crate::app_event_loop::queue_background_workspace_auto_saves(parked);
    assert!(parked.pending_file_saves.is_empty());
    assert_eq!(
        crate::app_event_loop::background_workspace_wait(parked, false),
        None
    );
    assert_eq!(std::fs::read_to_string(path).unwrap(), "disk");
}

#[test]
fn changed_user_settings_reach_hidden_workspaces_before_autosave_without_erasing_overrides() {
    let a = Project::new();
    let b = Project::new();
    let c = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    let path = a.path().join("file.txt");
    std::fs::write(&path, "disk").unwrap();
    app.editor_mut().unwrap().open_file(&path).unwrap();
    app.editor_mut().unwrap().insert_char('A');
    app.settings_global.auto_save = Some("afterDelay".into());
    app.settings_global.auto_save_delay = Some(0);
    app.apply_effective_settings();
    app.last_edit_time = Instant::now() - Duration::from_secs(10);
    let mut next = b.app(wake.clone());
    next.settings_workspace.auto_save = Some("onFocusChange".into());
    app.insert_workspace(next);
    app.insert_workspace(c.app(wake));
    app.settings_global.auto_save = Some("off".into());
    app.settings_global.tab_size = Some(7);
    app.apply_effective_settings();
    services(&mut app);
    let inherited = &app
        .workspaces
        .parked
        .iter()
        .find(|parked| parked.id == 0)
        .unwrap()
        .app;
    let overridden = &app
        .workspaces
        .parked
        .iter()
        .find(|parked| parked.id == 1)
        .unwrap()
        .app;
    assert!(matches!(inherited.auto_save, AutoSaveMode::Off));
    assert!(matches!(overridden.auto_save, AutoSaveMode::OnFocusChange));
    assert_eq!(inherited.default_tab_size, 7);
    assert_eq!(overridden.default_tab_size, 7);
    assert!(inherited.pending_file_saves.is_empty());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "disk");
    assert!(app.switch_workspace(0));
    assert!(app.editor().unwrap().rope.to_string().starts_with('A'));
}

#[test]
fn two_remote_watches_remain_active_and_unwatching_one_preserves_the_other() {
    let a = Project::new();
    let b = Project::new();
    let first = remote::workspace::Workspace::new(headless_connection(a.path()));
    let second = first
        .open_workspace(&first.view_key(&remote::git::from_native(b.path(), a.path()).unwrap()))
        .unwrap();
    for remote in [&first, &second] {
        remote.client.call(Command::Watch).unwrap();
        until(|| {
            remote
                .client
                .take_workspace_changes()
                .is_some_and(|batch| batch.active == Some(true))
        });
    }
    std::fs::write(a.path().join("from-a.txt"), "a").unwrap();
    std::fs::write(b.path().join("from-b.txt"), "b").unwrap();
    for (remote, file) in [
        (&first, b"from-a.txt".as_slice()),
        (&second, b"from-b.txt".as_slice()),
    ] {
        until(|| {
            remote.client.take_workspace_changes().is_some_and(|batch| {
                batch
                    .states
                    .iter()
                    .any(|state| state.path == RemotePath::new(file.to_vec()).unwrap())
            })
        });
    }
    second.client.call(Command::Unwatch).unwrap();
    std::fs::write(a.path().join("still-watched.txt"), "a").unwrap();
    until(|| {
        first.client.take_workspace_changes().is_some_and(|batch| {
            batch
                .states
                .iter()
                .any(|state| state.path == RemotePath::new(b"still-watched.txt".to_vec()).unwrap())
        })
    });
    assert!(first.client.connected() && second.client.connected());
    first.disconnect();
}

#[test]
fn remote_git_logs_are_owned_by_the_requesting_workspace() {
    let a = Project::new();
    let b = Project::new();
    git2::Repository::init(a.path()).unwrap();
    git2::Repository::init(b.path()).unwrap();
    let first = remote::workspace::Workspace::new(headless_connection(a.path()));
    let second = first
        .open_workspace(&first.view_key(&remote::git::from_native(b.path(), a.path()).unwrap()))
        .unwrap();
    let log_a = remote::diagnostics::Log::default();
    let log_b = remote::diagnostics::Log::default();
    first.client.set_git_output(&log_a);
    second.client.set_git_output(&log_b);
    second
        .client
        .call(Command::GitBranches {
            repo: RemotePath::default(),
        })
        .unwrap();
    assert!(log_a.report().lines().count() < log_b.report().lines().count());
    let b_before = log_b.version();
    first
        .client
        .call(Command::GitBranches {
            repo: RemotePath::default(),
        })
        .unwrap();
    assert_eq!(log_b.version(), b_before);
    assert!(log_a.version() > 0);
    first.client.release_git_output();
    second
        .client
        .call(Command::GitBranches {
            repo: RemotePath::default(),
        })
        .unwrap();
    assert!(log_b.version() > b_before);
    first.disconnect();
}

#[test]
fn shared_remote_outage_reconnects_once_and_restores_every_folder() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let launch = remote::launch::Launch::new(
        std::env::current_exe().unwrap().into_os_string(),
        [
            "--exact",
            "remote::tests::headless_service_fixture",
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ]
        .into_iter()
        .map(Into::into)
        .collect(),
    );
    let first = launch
        .start_in(ratatui::layout::Size::new(80, 24), wake.clone(), a.path())
        .unwrap()
        .workspace_for_test()
        .unwrap();
    let second = first
        .open_workspace(&first.view_key(&remote::git::from_native(b.path(), a.path()).unwrap()))
        .unwrap();
    let mut app = App::new_startup(first.view_root.clone(), wake.clone());
    app.remote_workspace = Some(first.clone());
    app.source_control_enabled = false;
    let mut next = App::new_startup(second.view_root.clone(), wake);
    next.remote_workspace = Some(second);
    next.source_control_enabled = false;
    app.insert_workspace(next);
    first.client.call(Command::Shutdown).unwrap();
    until(|| {
        services(&mut app);
        app.remote_connection.phase == Phase::Restored
            && app.workspaces.parked[0].app.remote_connection.phase == Phase::Restored
    });
    let active = app.remote_workspace.as_ref().unwrap();
    let parked = app.workspaces.parked[0]
        .app
        .remote_workspace
        .as_ref()
        .unwrap();
    assert!(active.client.connected() && parked.client.connected());
    assert!(active.client.same_transport(&parked.client));
    assert!(!active.client.same_transport(&first.client));
    assert_eq!(
        app.remote_connection.attempt + app.workspaces.parked[0].app.remote_connection.attempt,
        1
    );
    assert_eq!(parked.view_root, first.view_root);
    assert!(
        app.lsp_manager
            .remote_workspace
            .as_ref()
            .unwrap()
            .client
            .same_transport(&active.client)
    );
    assert!(
        app.workspaces.parked[0]
            .app
            .lsp_manager
            .remote_workspace
            .as_ref()
            .unwrap()
            .client
            .same_transport(&active.client)
    );
    active.disconnect();
    assert!(
        !parked.reconnect_allowed(),
        "explicit disconnect applies to the shared transport"
    );
}

#[test]
fn batched_terminal_completions_preserve_count_and_earlier_failures() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    let mut pty = TerminalEmulator::test_with_output(50, 8, "");
    let _keep_running = pty.test_keep_running();
    app.terminals[0].terminal_pty = Some(pty);
    app.insert_workspace(b.app(wake));
    app.workspaces.parked[0].app.terminals[0].terminal_pty.as_ref().unwrap().test_queue_output(
        b"\x1b]633;C\x07\x1b]633;D;1\x07\x1b]633;C\x07\x1b]633;D;0\x07\x1b]633;C\x07\x1b]633;D;0\x07");
    services(&mut app);
    assert_eq!(app.workspace_unread(), 3);
    assert!(app.workspace_needs_attention());
    assert!(
        app.workspaces.parked[0]
            .app
            .workspace_activity
            .last
            .contains("3 commands finished (1 failed)")
    );
}

#[test]
fn picker_paste_events_clear_receiving_state_without_touching_documents() {
    let project = Project::new();
    let mut app = project.app(Default::default());
    crate::ui_workspaces::open(&mut app);
    app.terminal_paste_status(crossterm::event::PasteStatus::Receiving { bytes: 128 });
    crate::app_event_loop::handle_paste_input(&mut app, "discard in picker");
    assert!(!app.paste_in_progress());
    assert_eq!(app.editor().unwrap().rope.to_string(), "original");
    app.workspaces.popup = Default::default();
    app.terminal_paste_status(crossterm::event::PasteStatus::Receiving { bytes: 128 });
    crate::ui_workspaces::open(&mut app);
    assert!(
        !app.workspaces.popup.open,
        "an incoming paste keeps its owner"
    );
    crate::app_event_loop::handle_paste_input(&mut app, "keep this paste");
    assert!(
        app.editor()
            .unwrap()
            .rope
            .to_string()
            .contains("keep this paste")
    );
}

#[test]
fn workspace_labels_escape_terminal_control_characters() {
    let project = Project::new();
    let mut app = project.app(Default::default());
    app.explorer.root = PathBuf::from("project-\n\x1b[31mname");
    let entry = app.workspace_entries().pop().unwrap();
    assert!(!entry.name.chars().any(char::is_control));
    assert!(!entry.path.chars().any(char::is_control));
    assert!(entry.name.contains("\\n"));
    assert!(
        !crate::ui_workspaces::clipped("event\n\x1b[31m", 50)
            .chars()
            .any(char::is_control)
    );
}

#[test]
fn stale_shared_reconnect_result_does_not_disconnect_its_siblings() {
    let a = Project::new();
    let b = Project::new();
    let first = remote::workspace::Workspace::new(headless_connection(a.path()));
    let second = first
        .open_workspace(&first.view_key(&remote::git::from_native(b.path(), a.path()).unwrap()))
        .unwrap();
    let mut app = a.app(Default::default());
    app.remote_workspace = Some(first.clone());
    app.remote_connection.phase = Phase::Stopped;
    app.finish_shared_remote_reconnect(999, 999, Ok(second.clone()));
    assert!(first.client.connected() && second.client.connected());
    first.disconnect();
}

#[test]
fn deleted_remote_folder_does_not_loop_or_break_the_replacement_connection() {
    let a = Project::new();
    let parent = Project::new();
    let b = parent.path().join("deleted");
    std::fs::create_dir(&b).unwrap();
    let first = remote::workspace::Workspace::new(headless_connection(a.path()));
    let second = first
        .open_workspace(&first.view_key(&remote::git::from_native(&b, a.path()).unwrap()))
        .unwrap();
    let wake = crate::background::UiWake::default();
    let mut app = App::new_startup(first.view_root.clone(), wake.clone());
    app.remote_workspace = Some(first.clone());
    app.source_control_enabled = false;
    let mut next = App::new_startup(second.view_root.clone(), wake);
    next.remote_workspace = Some(second);
    next.source_control_enabled = false;
    app.insert_workspace(next);
    first.disconnect();
    std::fs::remove_dir(&b).unwrap();
    let replacement = first
        .resume_on(remote::workspace::Workspace::new(headless_connection(
            a.path(),
        )))
        .unwrap();
    let parked = &mut app.workspaces.parked[0].app;
    parked.workspace_generation += 1;
    parked.resume_remote_services(replacement.clone());
    until(|| {
        services(&mut app);
        app.remote_connection.phase == Phase::Stopped
            && app.remote_connection.shared_attempt.is_some()
    });
    let serial = app.remote_connection.serial;
    for _ in 0..10 {
        services(&mut app);
    }
    assert_eq!(
        app.remote_connection.serial, serial,
        "failed folder resolution is attempted once per transport"
    );
    assert!(replacement.client.connected());
    assert!(
        app.remote_connection
            .message
            .contains("could not be reopened")
    );
    replacement.disconnect();
}

#[test]
fn completed_open_on_a_dead_remote_transport_is_rejected() {
    let a = Project::new();
    let b = Project::new();
    let first = remote::workspace::Workspace::new(headless_connection(a.path()));
    let second = first
        .open_workspace(&first.view_key(&remote::git::from_native(b.path(), a.path()).unwrap()))
        .unwrap();
    let mut app = a.app(Default::default());
    let original = app.editor().unwrap().save_identity;
    let (sender, receiver) = mpsc::channel();
    let mut startup = crate::startup::Prepared::load_remote(second.view_root.clone());
    startup.session = None;
    sender
        .send(Ok(Prepared {
            startup,
            remote: Some(second),
            equivalent_roots: vec![],
        }))
        .unwrap();
    app.workspaces.opening = Some(Opening {
        receiver,
        foreground: true,
        close_remote: None,
    });
    first.disconnect();
    app.poll_workspace_requests();
    assert_eq!(app.editor().unwrap().save_identity, original);
    assert!(app.workspaces.parked.is_empty() && !app.workspace_opening());
}

#[test]
fn custom_recovery_must_finish_before_claiming_save_and_quit() {
    let project = Project::new();
    let path = project.path().join("document.bin");
    std::fs::write(&path, [1, 2, 3]).unwrap();
    let mut app = project.app(Default::default());
    app.pending_custom_recoveries
        .push(crate::extension_recovery::RecoveryRecord {
            extension_id: "example".into(),
            command_id: "open".into(),
            original_path: path.clone(),
            stamp: crate::extension_documents::DiskStamp {
                len: 3,
                modified: std::fs::metadata(path).unwrap().modified().ok(),
            },
            record_path: PathBuf::new(),
        });
    assert!(
        crate::input_dialogs::save_text_documents_before_exit(&mut app)
            .unwrap_err()
            .contains("custom editor recovery")
    );
    assert_eq!(app.pending_custom_recoveries.len(), 1);
}

#[test]
fn many_switches_and_closes_preserve_ownership_and_unique_ids() {
    let projects: Vec<_> = (0..12).map(|_| Project::new()).collect();
    let wake = crate::background::UiWake::default();
    let mut app = projects[0].app(wake.clone());
    for project in &projects[1..] {
        app.insert_workspace(project.app(wake.clone()));
    }
    let mut expected = std::collections::HashMap::new();
    let mut seed = 719u64;
    for turn in 0..160 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let id = seed % 12;
        app.switch_workspace(id);
        app.editor_mut()
            .unwrap()
            .insert_char(char::from(b'a' + (turn % 26) as u8));
        expected.insert(
            id,
            (
                app.editor().unwrap().save_identity,
                app.editor().unwrap().rope.to_string(),
            ),
        );
        assert!(app.workspace_visible);
        for parked in &app.workspaces.parked {
            assert!(!parked.app.workspace_visible && parked.app.workspaces.parked.is_empty());
        }
    }
    for (id, (identity, text)) in expected {
        app.switch_workspace(id);
        assert_eq!(app.editor().unwrap().save_identity, identity);
        assert_eq!(app.editor().unwrap().rope.to_string(), text);
    }
    let ids: std::collections::HashSet<_> = app
        .workspace_entries()
        .iter()
        .map(|entry| entry.id)
        .collect();
    assert_eq!(ids.len(), 12);
}

#[test]
fn stopping_reconnect_on_a_follower_is_not_undone_by_a_connected_sibling() {
    let a = Project::new();
    let b = Project::new();
    let first = remote::workspace::Workspace::new(headless_connection(a.path()));
    let second = first
        .open_workspace(&first.view_key(&remote::git::from_native(b.path(), a.path()).unwrap()))
        .unwrap();
    let wake = crate::background::UiWake::default();
    let mut app = App::new_startup(first.view_root.clone(), wake.clone());
    app.remote_workspace = Some(first.clone());
    app.source_control_enabled = false;
    let mut next = App::new_startup(second.view_root.clone(), wake);
    next.remote_workspace = Some(second.clone());
    next.source_control_enabled = false;
    app.insert_workspace(next);
    first.disconnect();
    let replacement = first
        .resume_on(remote::workspace::Workspace::new(headless_connection(
            a.path(),
        )))
        .unwrap();
    app.workspaces.parked[0].app.workspace_generation += 1;
    app.workspaces.parked[0]
        .app
        .resume_remote_services(replacement.clone());
    app.stop_remote_reconnect();
    let serial = app.remote_connection.serial;
    for _ in 0..4 {
        app.coordinate_workspace_connections();
    }
    assert!(app.remote_connection.phase == Phase::Stopped);
    assert_eq!(app.remote_connection.serial, serial);
    assert!(replacement.client.connected());
    replacement.disconnect();
}

#[test]
fn cancelled_quit_clears_all_workspace_exit_flags_and_backup_approvals() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.insert_workspace(b.app(wake));
    app.workspaces.request = Some(Request::Exit('k'));
    app.poll_workspace_requests();
    app.exit_confirm_active = true;
    app.workspaces.parked[0].app.exit_discard_quit = true;
    app.workspaces.parked[0].app.exit_recovery.saved = true;
    crate::input_dialogs::handle_exit_confirm_key(
        &mut app,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
    assert!(app.workspaces.exit.is_none());
    assert!(!app.exit_confirm_active && !app.exit_keep_unsaved_quit && !app.shutdown_anim_active);
    assert!(
        !app.workspaces.parked[0].app.exit_discard_quit
            && !app.workspaces.parked[0].app.exit_recovery.saved
    );
}

#[test]
fn connection_sign_in_targets_the_workspace_that_owns_the_reconnect() {
    let a = Project::new();
    let b = Project::new();
    let first = remote::workspace::Workspace::new(headless_connection(a.path()));
    let second = first
        .open_workspace(&first.view_key(&remote::git::from_native(b.path(), a.path()).unwrap()))
        .unwrap();
    let wake = crate::background::UiWake::default();
    let mut app = App::new_startup(first.view_root.clone(), wake.clone());
    app.remote_workspace = Some(first.clone());
    app.source_control_enabled = false;
    let mut next = App::new_startup(second.view_root.clone(), wake);
    next.remote_workspace = Some(second);
    next.source_control_enabled = false;
    app.insert_workspace(next);
    first.client.disconnect();
    app.workspaces.parked[0].app.remote_connection.phase = Phase::Connecting;
    app.show_workspace_connection_sign_in();
    app.poll_workspace_requests();
    assert_eq!(app.workspaces.active, 0);
    assert!(app.remote_connection.details);
    assert!(!app.workspaces.parked[0].app.remote_connection.details);
}

#[test]
fn keep_and_discard_are_not_changed_by_focus_or_delay_autosave_during_exit() {
    for mode in [AutoSaveMode::OnFocusChange, AutoSaveMode::AfterDelay] {
        for choice in ['k', 'd'] {
            let a = Project::new();
            let b = Project::new();
            let wake = crate::background::UiWake::default();
            let mut app = a.app(wake.clone());
            for project in [&a, &b] {
                std::fs::write(project.path().join("file.txt"), "disk").unwrap();
            }
            app.editor_mut()
                .unwrap()
                .open_file(&a.path().join("file.txt"))
                .unwrap();
            app.editor_mut().unwrap().insert_char('A');
            let mut next = b.app(wake.clone());
            next.editor_mut()
                .unwrap()
                .open_file(&b.path().join("file.txt"))
                .unwrap();
            next.editor_mut().unwrap().insert_char('B');
            app.insert_workspace(next);
            app.auto_save = mode;
            app.auto_save_delay = 0;
            app.last_edit_time = Instant::now() - Duration::from_secs(10);
            app.workspaces.parked[0].app.auto_save = mode;
            app.workspaces.parked[0].app.auto_save_delay = 0;
            app.workspaces.parked[0].app.last_edit_time = app.last_edit_time;
            app.workspaces.request = Some(Request::Exit(choice));
            let (sender, receiver) = mpsc::channel();
            let (done, completed) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = completed.recv_timeout(Duration::from_secs(15));
                drop(sender);
                wake.notify();
            });
            let pump = crate::app_event_loop::TerminalEventPump::from_receiver(receiver);
            let mut terminal = Terminal::with_options(
                ratatui::backend::CrosstermBackend::new(Vec::<u8>::new()),
                ratatui::TerminalOptions {
                    viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 120, 28)),
                },
            )
            .unwrap();
            crate::app_event_loop::run_app(&mut terminal, &mut app, pump, None)
                .expect("quit must finish without autosaving kept/discarded data");
            let _ = done.send(());
            app.finish_workspaces_on_shutdown();
            for project in [&a, &b] {
                assert_eq!(
                    std::fs::read_to_string(project.path().join("file.txt")).unwrap(),
                    "disk"
                );
                let mut session = crate::session::load_session(project.path()).unwrap();
                crate::session::hydrate_saved_tab(project.path(), &mut session.tabs[0]);
                assert_eq!(
                    session.tabs[0].pending_unsaved_content.is_some(),
                    choice == 'k'
                );
            }
        }
    }
}

#[test]
fn closing_one_remote_workspace_keeps_the_other_connection_live() {
    let a = Project::new();
    let b = Project::new();
    let first = remote::workspace::Workspace::new(headless_connection(a.path()));
    let second = first
        .open_workspace(&first.view_key(&remote::git::from_native(b.path(), a.path()).unwrap()))
        .unwrap();
    let wake = crate::background::UiWake::default();
    let mut app = App::new_startup(first.view_root.clone(), wake.clone());
    app.remote_workspace = Some(first.clone());
    app.source_control_enabled = false;
    let mut next = App::new_startup(second.view_root.clone(), wake);
    next.remote_workspace = Some(second.clone());
    next.source_control_enabled = false;
    app.insert_workspace(next);
    app.workspaces.request = Some(Request::Close);
    app.poll_workspace_requests();
    app.start_exit_recovery();
    until(|| {
        services(&mut app);
        app.exit_recovery.saved
    });
    app.shutdown_anim_active = true;
    assert!(app.advance_workspace_exit());
    assert_eq!(app.explorer.root, first.view_root);
    assert!(app.workspaces.parked.is_empty());
    assert!(first.client.connected());
    first
        .client
        .call(Command::List {
            path: RemotePath::default(),
        })
        .unwrap();
    first.disconnect();
}

#[cfg(unix)]
#[test]
fn folder_symlink_returns_to_the_existing_live_workspace() {
    let a = Project::new();
    let b = Project::new();
    let alias = b.path().join("alias");
    std::os::unix::fs::symlink(a.path(), &alias).unwrap();
    let mut app = a.app(Default::default());
    let identity = app.editor().unwrap().save_identity;
    app.workspaces.request = Some(Request::Open(alias));
    app.poll_workspace_requests();
    until(|| {
        services(&mut app);
        app.poll_workspace_requests();
        !app.workspace_opening()
    });
    assert!(app.workspaces.parked.is_empty());
    assert_eq!(app.editor().unwrap().save_identity, identity);
}
