use super::*;
use crate::{
    app_remote_connection::Phase,
    remote::{self, Command},
};

#[test]
fn queued_workspace_switch_waits_for_an_intervening_save_dialog() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.insert_workspace(b.app(wake));
    app.workspaces.request = Some(Request::Switch(0));
    app.show_save_error("A save failed before the queued switch");
    assert!(!app.poll_workspace_requests());
    assert_eq!(app.workspaces.active, 1);
    assert!(app.save_error.is_some());
    app.dismiss_save_error();
    assert!(app.poll_workspace_requests());
    assert_eq!(app.workspaces.active, 0);
}

#[test]
fn queued_workspace_close_waits_for_an_intervening_save_dialog() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.insert_workspace(b.app(wake));
    app.workspaces.request = Some(Request::Close);
    app.show_save_error("A save failed before the queued close");
    assert!(!app.poll_workspace_requests());
    assert!(app.workspaces.exit.is_none());
    assert!(!app.exit_keep_unsaved_quit);
    app.dismiss_save_error();
    app.poll_workspace_requests();
    assert!(app.workspaces.exit.is_some() && app.exit_keep_unsaved_quit);
    app.reset_workspace_exit();
}

#[test]
fn close_confirmation_keeps_actions_reachable_in_short_terminals() {
    let project = Project::new();
    let mut app = project.app(Default::default());
    let other = Project::new();
    app.insert_workspace(other.app(Default::default()));
    for height in 4..=9 {
        crate::ui_workspaces::close(&mut app);
        let buffer = draw(&mut app, 48, height);
        let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("Cancel"), "Cancel missing at height {height}");
        crate::ui_workspaces::handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        let buffer = draw(&mut app, 48, height);
        let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
        assert!(
            text.contains("[ Close folder ]"),
            "Close folder action missing at height {height}"
        );
    }
}

#[test]
fn deleting_the_reconnect_owner_does_not_strand_healthy_remote_workspaces() {
    exercise_missing_reconnect_owner(false, false, false);
}

#[test]
fn recreated_reconnect_owner_rejoins_the_existing_transport_on_retry() {
    exercise_missing_reconnect_owner(true, false, false);
}

#[test]
fn closing_the_failed_owner_during_transport_handoff_preserves_its_sibling() {
    exercise_missing_reconnect_owner(false, true, false);
}

#[test]
fn disconnecting_after_coordination_still_reserves_one_reconnect_owner() {
    exercise_missing_reconnect_owner(false, false, true);
}

fn close_active_workspace(app: &mut App) {
    app.workspaces.request = Some(Request::Close);
    app.poll_workspace_requests();
    app.start_exit_recovery();
    until(|| {
        services(app);
        app.exit_recovery.saved
    });
    app.shutdown_anim_active = true;
    assert!(app.advance_workspace_exit());
    assert!(app.workspaces.parked.is_empty());
}

fn exercise_missing_reconnect_owner(recreate: bool, close_during_handoff: bool, between_polls: bool) {
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
    std::fs::remove_dir(b.path()).unwrap();
    if between_polls { app.coordinate_workspace_connections(); }
    first.client.call(Command::Shutdown).unwrap();
    if between_polls {
        until(|| !first.client.connected());
        // Force loss after the family snapshot, before the next coordination
        // turn. Both workspaces see it, but only the reserved owner may launch.
        app.poll_background_results();
        app.workspaces.parked[0].app.poll_background_results();
        assert_eq!(app.remote_connection.attempt + app.workspaces.parked[0].app.remote_connection.attempt, 1);
    }
    until(|| {
        services(&mut app);
        (close_during_handoff && app.remote_connection.phase == Phase::Stopped)
            || (app.workspaces.parked[0].app.remote_connection.phase == Phase::Restored
                && app.workspaces.parked[0]
                    .app
                    .remote_workspace
                    .as_ref()
                    .unwrap()
                    .client
                    .connected())
            || app.remote_connection.attempt >= 2
    });
    if close_during_handoff {
        assert!(app.remote_connection.phase == Phase::Stopped);
        close_active_workspace(&mut app);
        until(|| {
            services(&mut app);
            app.remote_connection.phase == Phase::Restored
        });
        let restored = app.remote_workspace.as_ref().unwrap();
        assert!(restored.client.connected());
        restored
            .client
            .call(Command::List {
                path: Default::default(),
            })
            .unwrap();
        restored.disconnect();
        return;
    }
    let restored = app.workspaces.parked[0]
        .app
        .remote_workspace
        .as_ref()
        .unwrap()
        .clone();
    assert!(
        restored.client.connected(),
        "a missing reconnect-owner folder must not strand its healthy sibling"
    );
    assert!(app.remote_connection.phase == Phase::Stopped);
    assert!(app.workspaces.parked[0].app.remote_connection.phase == Phase::Restored);
    assert_eq!(
        app.remote_connection.attempt + app.workspaces.parked[0].app.remote_connection.attempt,
        1
    );
    let serial = app.remote_connection.serial;
    for _ in 0..4 {
        services(&mut app);
    }
    assert_eq!(
        app.remote_connection.serial, serial,
        "the missing folder must not retry forever on a healthy transport"
    );
    if recreate {
        std::fs::create_dir(b.path()).unwrap();
        app.retry_remote_connection();
        until(|| {
            services(&mut app);
            app.remote_connection.phase == Phase::Restored
        });
        assert!(
            app.remote_workspace
                .as_ref()
                .unwrap()
                .client
                .same_transport(&restored.client)
        );
        assert_eq!(
            app.remote_connection.attempt + app.workspaces.parked[0].app.remote_connection.attempt,
            1
        );
    }
    close_active_workspace(&mut app);
    assert!(restored.client.connected());
    restored
        .client
        .call(Command::List {
            path: Default::default(),
        })
        .unwrap();
    restored.disconnect();
}
