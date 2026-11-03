use super::*;
use crate::remote::{self, Command, RemotePath};
use std::sync::{Arc, atomic::AtomicBool};

#[test]
fn ten_workspaces_retain_coverage_without_scheduling_passive_work_while_hidden() {
    let projects: Vec<_> = (0..10).map(|_| Project::new()).collect();
    let wake = crate::background::UiWake::default();
    let mut app = projects[0].app(wake.clone());
    app.start_workspace_watcher();
    until(|| {
        services(&mut app);
        app.workspace_watcher_active
    });
    for project in &projects[1..] {
        let mut next = project.app(wake.clone());
        next.start_workspace_watcher();
        app.insert_workspace(next);
    }
    until(|| {
        services(&mut app);
        app.workspace_watcher_active
            && !app.workspaces_have_pending_results()
            && app.background.active_job_count() == 0
            && app
                .workspaces
                .parked
                .iter()
                .all(|parked| parked.app.background.active_job_count() == 0)
    });
    // Let any startup burst finish before measuring hidden filesystem work.
    std::thread::sleep(Duration::from_millis(350));
    until(|| {
        services(&mut app);
        !app.workspaces_have_pending_results()
            && app.background.active_job_count() == 0
            && app
                .workspaces
                .parked
                .iter()
                .all(|parked| parked.app.background.active_job_count() == 0)
    });
    let observed = wake.generation();
    for (index, project) in projects[..9].iter().enumerate() {
        std::fs::write(
            project.path().join("background-build.txt"),
            format!("build {index}"),
        )
        .unwrap();
    }
    std::thread::sleep(Duration::from_millis(650));
    assert_eq!(
        wake.generation(),
        observed,
        "hidden filesystem activity must not wake the UI"
    );
    assert!(!app.workspaces_have_pending_results());
    for parked in &mut app.workspaces.parked {
        let hidden = &mut parked.app;
        assert!(hidden.workspace_watcher_active);
        hidden.source_control_enabled = true;
        let generation = hidden.explorer_refresh_generation;
        hidden.refresh_explorer_async();
        hidden.refresh_source_control_async();
        hidden.refresh_git_branch_async();
        hidden.refresh_git_line_status_async();
        hidden.refresh_git_log_async();
        hidden.refresh_source_control_from_watcher(Default::default(), true);
        hidden.global_search_query = "background".into();
        hidden.schedule_global_search();
        hidden.last_edit_time = Instant::now();
        assert!(!hidden.poll_completion_index());
        assert!(!hidden.poll_git_watcher_refresh(Instant::now() + Duration::from_secs(5)));
        assert!(!hidden.poll_pending_global_search(Instant::now() + Duration::from_secs(5)));
        assert_eq!(hidden.explorer_refresh_generation, generation);
        assert_eq!(hidden.background.active_job_count(), 0);
        assert!(
            !hidden.bg_git_status_running
                && !hidden.bg_git_branch_running
                && !hidden.bg_explorer_refresh_running
        );
        assert_eq!(
            crate::app_event_loop::background_workspace_wait(hidden, false),
            None
        );
    }
    assert_eq!(app.background_workspace_wait(), None);
}

#[test]
fn returning_to_a_workspace_refreshes_disk_git_and_deletions_without_losing_edits() {
    let a = Project::new();
    let b = Project::new();
    git2::Repository::init(a.path()).unwrap();
    let clean = a.path().join("clean.txt");
    let dirty = a.path().join("dirty.txt");
    let deleted = a.path().join("deleted.txt");
    for path in [&clean, &dirty, &deleted] {
        std::fs::write(path, "original").unwrap();
    }
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.tabs.clear();
    for path in [&clean, &dirty, &deleted] {
        let mut editor = Editor::new();
        editor.open_file(path).unwrap();
        if path == &dirty {
            editor.insert_char('!');
        }
        app.tabs.push(Tab::new_file(editor));
    }
    app.source_control_enabled = true;
    app.start_workspace_watcher();
    until(|| {
        services(&mut app);
        app.workspace_watcher_active
    });
    app.insert_workspace(b.app(wake));
    std::fs::write(&clean, "externally updated clean file").unwrap();
    std::fs::write(&dirty, "external dirty-file replacement").unwrap();
    std::fs::remove_file(&deleted).unwrap();
    assert!(app.switch_workspace(0));
    until(|| {
        services(&mut app);
        app.poll_git_watcher_refresh(Instant::now());
        app.tabs.iter().any(|tab| {
            tab.editor.file_path.as_ref() == Some(&clean)
                && tab.editor.rope.to_string() == "externally updated clean file"
        }) && !app
            .tabs
            .iter()
            .any(|tab| tab.editor.file_path.as_ref() == Some(&deleted))
            && app
                .source_control_files
                .iter()
                .any(|file| file.path == "clean.txt")
    });
    let editor = &app
        .tabs
        .iter()
        .find(|tab| tab.editor.file_path.as_ref() == Some(&dirty))
        .unwrap()
        .editor;
    assert!(editor.modified);
    assert!(editor.rope.to_string().starts_with('!'));
    assert!(!editor.rope.to_string().contains("external"));
}

#[test]
fn cancelled_watcher_replies_cannot_disable_the_resumed_subscription() {
    let a = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake);
    app.start_workspace_watcher();
    until(|| {
        services(&mut app);
        app.workspace_watcher_active
    });
    app.background
        .sender
        .send(crate::background::BackgroundResult::FileWatcherEvent {
            cancelled: Arc::new(AtomicBool::new(true)),
            result: Box::new(crate::background::BackgroundResult::FileWatcherState {
                root: a.path().into(),
                active: false,
            }),
        })
        .unwrap();
    services(&mut app);
    assert!(app.workspace_watcher_active);
}

#[test]
fn headless_workspace_watchers_pause_and_resume_on_the_shared_transport() {
    let projects: Vec<_> = (0..10).map(|_| Project::new()).collect();
    for project in &projects {
        std::fs::write(project.path().join("file.txt"), "original").unwrap();
    }
    let first = remote::workspace::Workspace::new(headless_connection(projects[0].path()));
    let wake = crate::background::UiWake::default();
    first.client.set_wake(wake.clone());
    let mut remotes = vec![first.clone()];
    for project in &projects[1..] {
        remotes.push(
            first
                .open_workspace(&first.view_key(
                    &remote::git::from_native(project.path(), projects[0].path()).unwrap(),
                ))
                .unwrap(),
        );
    }
    let make_app = |remote: &remote::workspace::Workspace| {
        let mut app = App::new_startup(remote.view_root.clone(), wake.clone());
        app.remote_workspace = Some(remote.clone());
        app.lsp_manager.remote_workspace = Some(remote.clone());
        app.lsp_manager.configs.clear();
        app.source_control_enabled = false;
        app.start_workspace_watcher();
        app
    };
    let mut app = make_app(&first);
    let key = first.view_key(&RemotePath::new(b"file.txt".to_vec()).unwrap());
    app.open_file(&key);
    until(|| {
        services(&mut app);
        app.workspace_watcher_active && app.editor().is_some_and(|ed| !ed.bg_loading)
    });
    for remote in &remotes[1..] {
        app.insert_workspace(make_app(remote));
    }
    // The observer command stream is ordered: this barrier follows all Unwatch requests.
    remotes
        .last()
        .unwrap()
        .client
        .call(Command::Unwatch)
        .unwrap();
    remotes.last().unwrap().client.call(Command::Watch).unwrap();
    until(|| {
        services(&mut app);
        app.workspace_watcher_active
    });
    std::thread::sleep(Duration::from_millis(350));
    until(|| {
        services(&mut app);
        !app.workspaces_have_pending_results()
            && app.background.active_job_count() == 0
            && app
                .workspaces
                .parked
                .iter()
                .all(|parked| parked.app.background.active_job_count() == 0)
    });
    let observed = wake.generation();
    for project in &projects[..9] {
        std::fs::write(project.path().join("file.txt"), "changed while hidden").unwrap();
    }
    std::thread::sleep(Duration::from_millis(650));
    assert_eq!(
        wake.generation(),
        observed,
        "headless inactive roots must not stream watcher events"
    );
    for remote in &remotes[..9] {
        assert!(remote.client.take_workspace_changes().is_none());
    }
    assert!(app.switch_workspace(0));
    until(|| {
        services(&mut app);
        app.workspace_watcher_active
            && app
                .editor()
                .is_some_and(|ed| ed.rope.to_string() == "changed while hidden")
    });
    for _ in 0..12 {
        assert!(app.switch_workspace(9));
        assert!(app.switch_workspace(0));
    }
    until(|| {
        services(&mut app);
        assert!(
            first.client.connected(),
            "rapid switching disconnected the headless transport: {:?}",
            first.client.disconnect_reason()
        );
        app.workspace_watcher_active && !app.bg_explorer_refresh_running
    });
    std::fs::write(projects[0].path().join("file.txt"), "after rapid switching").unwrap();
    until(|| {
        services(&mut app);
        app.editor()
            .is_some_and(|ed| ed.rope.to_string() == "after rapid switching")
    });
    assert!(remotes.iter().all(|remote| remote.client.connected()));
    first.disconnect();
}

#[test]
fn hidden_language_servers_drain_replies_without_requesting_ui_feedback() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    let path = a.path().join("file.rs");
    app.editor_mut().unwrap().file_path = Some(path.clone());
    app.editor_mut().unwrap().content_version = 1;
    let (client, server) = crate::lsp::LspClient::test_pair("rust", &["*.rs"]);
    app.lsp_manager.clients.push(client);
    app.lsp_manager
        .sync_document_owned(&path, 1, "original".into());
    server.take_messages();
    app.last_edit_time = Instant::now() - Duration::from_secs(2);
    app.insert_workspace(b.app(wake));
    for _ in 0..4 {
        services(&mut app);
    }
    let messages = server.take_messages();
    assert!(
        !messages.iter().any(|message| message.get("id").is_some()),
        "hidden UI requests: {messages:?}"
    );
    assert!(app.switch_workspace(0));
    services(&mut app);
    assert!(
        server
            .take_messages()
            .iter()
            .any(|message| message["method"] == "textDocument/documentSymbol")
    );
}

#[test]
fn restoring_hidden_documents_defers_starting_a_language_server_until_visited() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    let path = a.path().join("file.c");
    app.editor_mut().unwrap().file_path = Some(path.clone());
    app.editor_mut().unwrap().content_version = 1;
    app.lsp_manager
        .register_config(crate::lsp::LspServerConfig {
            language: "c".into(),
            command: "vtcode-missing-test-server".into(),
            args: Vec::new(),
            file_patterns: vec!["*.c".into()],
            settings: None,
            init_options: None,
        });
    app.insert_workspace(b.app(wake));
    let hidden = &mut app.workspaces.parked[0].app;
    hidden.queue_lsp_document_sync(path, 1, ropey::Rope::from_str("original"));
    for _ in 0..4 {
        services(&mut app);
    }
    let hidden = &app.workspaces.parked[0].app;
    assert!(hidden.lsp_manager.clients.is_empty());
    assert!(
        hidden.lsp_manager.startup_errors.is_empty(),
        "hidden restoration must not try to spawn the server"
    );
    assert!(app.switch_workspace(0));
    services(&mut app);
    assert!(
        app.lsp_manager.startup_errors.contains_key("c"),
        "visiting the document must resume its deferred startup"
    );
}

#[test]
fn headless_default_root_notifications_and_requests_share_the_same_watch_scope() {
    let project = Project::new();
    let workspace = remote::workspace::Workspace::new(headless_connection(project.path()));
    workspace.client.notify(Command::Watch).unwrap();
    until(|| {
        workspace
            .client
            .take_workspace_changes()
            .is_some_and(|batch| batch.active == Some(true) && batch.rescan)
    });
    workspace.client.call(Command::Unwatch).unwrap();
    std::thread::sleep(Duration::from_millis(350));
    workspace.client.take_workspace_changes();
    std::fs::write(project.path().join("unwatched.txt"), "no notifications").unwrap();
    std::thread::sleep(Duration::from_millis(650));
    assert!(
        workspace.client.take_workspace_changes().is_none(),
        "default-root Unwatch must release a subscription started by notify"
    );
    assert!(workspace.client.connected());
    workspace.disconnect();
}
