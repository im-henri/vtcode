use super::*;

#[test]
fn slow_idle_workspace_sweeps_finish_without_fabricating_producer_wakes() {
    let projects: Vec<_> = (0..10).map(|_| Project::new()).collect();
    let wake = crate::background::UiWake::default();
    let mut app = projects[0].app(wake.clone());
    for project in &projects[1..] {
        app.insert_workspace(project.app(wake.clone()));
    }
    let observed = wake.generation();
    let mut scheduler = crate::ui_work::Scheduler::default();
    let mut slices = 0;
    loop {
        assert_eq!(scheduler.observe_wake(observed), observed);
        let now = Instant::now();
        let mut turn = crate::ui_work::Turn::new(now);
        turn.deadline = now; // Model descheduling before every workspace slice.
        let service = scheduler.next(&mut turn, now, false).unwrap();
        app.service_workspaces_until(service, now, || true);
        if app.workspace_service_pending(service) {
            scheduler.defer_last_service(&mut turn, observed);
        }
        assert_eq!(scheduler.next(&mut turn, Instant::now(), false), None);
        slices += 1;
        if !turn.deferred() {
            break;
        }
        assert!(
            slices < projects.len() * 5,
            "an idle registry keeps restarting instead of finishing its sweep"
        );
    }
    assert_eq!(slices, projects.len() * 5);
    assert_eq!(
        wake.generation(),
        observed,
        "a descheduled idle registry must eventually sleep"
    );
}

#[test]
fn disabled_git_and_exhausted_history_do_not_invalidate_a_hidden_workspace() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = warm_app(&a, wake.clone());
    app.source_control_enabled = false;
    let before = snapshot(&app);
    app.insert_workspace(warm_app(&b, wake));
    let hidden = &mut app.workspaces.parked[0].app;
    hidden.refresh_source_control_changed_paths([a.path().join("file.txt")]);
    hidden.load_more_git_history();
    app.switch_workspace(0);
    settle(&mut app);
    assert_eq!(snapshot(&app), before);
}

#[test]
fn hidden_explorer_refresh_does_not_revalidate_documents_or_restart_git_history() {
    let a = Project::new();
    let b = Project::new();
    git2::Repository::init(a.path()).unwrap();
    let path = a.path().join("file.txt");
    std::fs::write(&path, "original").unwrap();
    let wake = crate::background::UiWake::default();
    let mut app = warm_app(&a, wake.clone());
    app.editor_mut().unwrap().open_file(&path).unwrap();
    settle(&mut app);
    let history = app.git_history.generation;
    let document = app.editor().unwrap().content_version;
    let last_job = app
        .background
        .activity
        .recent
        .iter()
        .map(|job| job.id)
        .max()
        .unwrap_or(0);
    app.insert_workspace(warm_app(&b, wake));
    app.workspaces.parked[0].app.refresh_explorer_async();
    app.switch_workspace(0);
    settle(&mut app);
    assert_eq!(app.git_history.generation, history);
    assert_eq!(app.editor().unwrap().content_version, document);
    assert!(
        !app.background
            .activity
            .recent
            .iter()
            .any(|job| job.id > last_job && job.path.as_deref() == Some(path.as_path())),
        "only the Explorer tree needed work"
    );
}

#[test]
fn deferred_search_and_git_reads_do_not_force_an_explorer_rescan() {
    let a = Project::new();
    let b = Project::new();
    git2::Repository::init(a.path()).unwrap();
    let wake = crate::background::UiWake::default();
    let mut app = warm_app(&a, wake.clone());
    let explorer = app.explorer_refresh_generation;
    let history = app.git_history.generation;
    app.insert_workspace(warm_app(&b, wake));
    let hidden = &mut app.workspaces.parked[0].app;
    hidden.refresh_git_branch_async();
    hidden.refresh_git_line_status_async();
    hidden.update_global_search(); // An empty query is a no-op, even while hidden.
    app.switch_workspace(0);
    settle(&mut app);
    assert_eq!(app.explorer_refresh_generation, explorer);
    assert_eq!(app.git_history.generation, history);
}

#[test]
fn hidden_agent_edits_revalidate_only_the_edited_document_even_without_a_watcher() {
    for dirty in [false, true] {
        let a = Project::new();
        let b = Project::new();
        let touched = a.path().join("touched.txt");
        let untouched = a.path().join("untouched.txt");
        for path in [&touched, &untouched] {
            std::fs::write(path, "original").unwrap();
        }
        let wake = crate::background::UiWake::default();
        let mut app = a.app(wake.clone());
        app.tabs.clear();
        for path in [&touched, &untouched] {
            let mut editor = Editor::new();
            editor.open_file(path).unwrap();
            app.tabs.push(Tab::new_file(editor));
        }
        if dirty {
            app.editor_mut().unwrap().insert_char('!');
        }
        let version = app.tabs[1].editor.content_version;
        app.insert_workspace(b.app(wake));
        std::fs::write(&touched, "agent edit while hidden").unwrap();
        app.workspaces.parked[0]
            .app
            .apply_acp_event(vtcode::acp::Event::FileEdited(vtcode::acp::FileEdit {
                session_id: vtcode::acp::protocol::SessionId::new("edit"),
                path: touched,
                before: "original".into(),
                after: "agent edit while hidden".into(),
            }));
        assert!(!app.workspaces.parked[0].app.background.has_active_jobs());
        app.switch_workspace(0);
        until(|| {
            services(&mut app);
            if dirty {
                app.status_message.contains("unsaved edits are preserved")
            } else {
                app.editor().unwrap().rope.to_string() == "agent edit while hidden"
            }
        });
        settle(&mut app);
        if dirty {
            assert_eq!(app.editor().unwrap().rope.to_string(), "!original");
        }
        assert_eq!(app.tabs[1].editor.content_version, version);
        assert!(
            !app.background
                .activity
                .recent
                .iter()
                .any(|job| job.path.as_deref() == Some(untouched.as_path()))
        );
    }
}

#[test]
fn background_formatting_does_not_start_unrelated_language_servers() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    let c = a.path().join("file.c");
    let python = a.path().join("file.py");
    for (language, pattern) in [("c", "*.c"), ("python", "*.py")] {
        app.lsp_manager
            .register_config(crate::lsp::LspServerConfig {
                language: language.into(),
                command: "vtcode-missing-test-server".into(),
                args: Vec::new(),
                file_patterns: vec![pattern.into()],
                settings: None,
                init_options: None,
            });
    }
    app.tabs.clear();
    for path in [&c, &python] {
        let mut editor = Editor::test_with_text("original");
        editor.file_path = Some(path.clone());
        editor.content_version = 1;
        app.tabs.push(Tab::new_file(editor));
    }
    let (client, _server) = crate::lsp::LspClient::test_pair("c", &["*.c"]);
    app.lsp_manager.clients.push(client);
    app.insert_workspace(b.app(wake));
    let hidden = &mut app.workspaces.parked[0].app;
    hidden.lsp_format_pending = true;
    hidden.lsp_format_target_path = Some(c);
    hidden.queue_lsp_document_sync(python.clone(), 1, ropey::Rope::from_str("original"));
    hidden.flush_lsp_document_syncs();
    assert!(!hidden.lsp_manager.startup_errors.contains_key("python"));
    app.switch_workspace(0);
    app.flush_lsp_document_syncs();
    assert!(app.lsp_manager.startup_errors.contains_key("python"));
}

#[test]
fn passive_hidden_sync_does_not_restart_a_dead_language_server() {
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
    let (mut client, dead_server) = crate::lsp::LspClient::test_pair("c", &["*.c"]);
    client.server_dead = true;
    app.lsp_manager.clients.push(client);
    app.insert_workspace(b.app(wake));
    let hidden = &mut app.workspaces.parked[0].app;
    hidden.queue_lsp_document_sync(path.clone(), 1, ropey::Rope::from_str("original"));
    hidden.flush_lsp_document_syncs();
    assert!(hidden.lsp_manager.startup_errors.is_empty());
    assert!(dead_server.take_messages().is_empty());
    hidden.lsp_manager.clients.clear();
    let (client, server) = crate::lsp::LspClient::test_pair("c", &["*.c"]);
    hidden.lsp_manager.clients.push(client);
    app.switch_workspace(0);
    app.flush_lsp_document_syncs();
    until(|| {
        server
            .take_messages()
            .iter()
            .any(|message| message["method"] == "textDocument/didOpen")
    });
}

#[test]
fn document_check_overflow_does_not_invalidate_git_or_explorer() {
    let a = Project::new();
    let b = Project::new();
    git2::Repository::init(a.path()).unwrap();
    let path = a.path().join("file.txt");
    std::fs::write(&path, "original").unwrap();
    let wake = crate::background::UiWake::default();
    let mut app = warm_app(&a, wake.clone());
    app.editor_mut().unwrap().open_file(&path).unwrap();
    settle(&mut app);
    let explorer = app.explorer_refresh_generation;
    let history = app.git_history.generation;
    let git = app.source_control_last_refresh;
    app.insert_workspace(warm_app(&b, wake));
    let hidden = &mut app.workspaces.parked[0].app;
    hidden.defer_workspace_file_check(&path);
    for index in 0..1100 {
        hidden.defer_workspace_file_check(&a.path().join(format!("closed-{index}.txt")));
    }
    app.switch_workspace(0);
    settle(&mut app);
    assert_eq!(app.explorer_refresh_generation, explorer);
    assert_eq!(app.git_history.generation, history);
    assert_eq!(app.source_control_last_refresh, git);
}
