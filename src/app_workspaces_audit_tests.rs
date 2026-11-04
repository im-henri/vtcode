use super::*;

#[test]
fn returning_detects_native_replacements_that_preserve_size_and_timestamp() {
    for dirty in [false, true] {
        let a = Project::new();
        let b = Project::new();
        let path = a.path().join("file.txt");
        std::fs::write(&path, "original").unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let wake = crate::background::UiWake::default();
        let mut app = a.app(wake.clone());
        app.editor_mut().unwrap().open_file(&path).unwrap();
        if dirty {
            app.editor_mut().unwrap().insert_char('!');
        }
        app.start_workspace_watcher();
        until(|| {
            services(&mut app);
            app.workspace_watcher_active
                && !app.background.has_active_jobs()
                && !app.workspaces_have_pending_results()
        });
        app.insert_workspace(b.app(wake));
        std::fs::write(&path, "CHANGED!").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
        assert!(app.switch_workspace(0));
        until(|| {
            services(&mut app);
            if dirty {
                app.status_message.contains("unsaved edits are preserved")
            } else {
                app.editor().unwrap().rope.to_string() == "CHANGED!"
            }
        });
        assert_eq!(app.editor().unwrap().modified, dirty);
        if dirty {
            assert_eq!(app.editor().unwrap().rope.to_string(), "!original");
        }
    }
}

#[test]
fn saved_set_additions_are_revalidated_even_when_no_workspace_needs_closing() {
    use crate::workspace_sets::{self, Location, Mutation, Scope};
    let a = Project::new();
    let b = Project::new();
    let mut app = a.app(Default::default());
    app.workspaces.sets.catalog = workspace_sets::mutate(
        &a.path().join("sets.json"),
        Mutation::Save {
            name: "Add folder".into(),
            scope: Scope::Local,
            roots: vec![
                Location::local(a.path(), &crate::portable::base_dir()),
                Location::local(b.path(), &crate::portable::base_dir()),
            ],
            active: 0,
            replace: None,
        },
    )
    .unwrap();
    let id = app.workspaces.sets.catalog.sets[0].id;
    app.prepare_workspace_set(id);
    app.show_save_error("Hold workspace topology while the set is prepared");
    until(|| {
        app.poll_workspace_requests();
        services(&mut app);
        matches!(app.workspaces.sets.plan, Some(sets::Plan::Ready(_)))
    });
    std::fs::remove_dir(b.path()).unwrap();
    app.dismiss_save_error();
    until(|| {
        app.poll_workspace_requests();
        services(&mut app);
        !app.workspace_set_switching()
    });
    assert_eq!(
        app.workspace_entries().len(),
        1,
        "a disappeared folder must not be added from a stale prepared snapshot"
    );
    assert_eq!(app.explorer.root, a.path());
}

#[test]
fn completed_workspaces_do_not_leave_expired_background_deadlines() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.insert_workspace(b.app(wake));
    let hidden = &mut app.workspaces.parked[0].app;
    hidden.formatting.feedback = Some((
        "Formatting complete".into(),
        Instant::now() - Duration::from_secs(1),
    ));
    let stamp = hidden.workspace_exit_stamp().unwrap();
    app.workspaces.exit = Some(Exit {
        remaining: vec![],
        choice: 'k',
        close: false,
        completed: vec![(0, stamp)],
        ready: false,
        return_to: None,
    });
    assert_eq!(
        app.background_workspace_wait(),
        None,
        "a frozen workspace cannot consume its expired timer; it must not spin the UI while other workspaces close"
    );
}

#[test]
fn queued_language_server_and_extension_edits_block_a_completed_exit_snapshot() {
    let project = Project::new();
    let mut app = project.app(Default::default());
    app.editor_mut().unwrap().file_path = Some(project.path().join("file.txt"));
    app.queue_lsp_workspace_edit(
        lsp_types::WorkspaceEdit::default(),
        "Edit".into(),
        None,
        None,
    );
    assert!(app.workspace_exit_stamp().is_none());
    app.poll_lsp_workspace_edits();
    app.extension_manager.sync_discovered(vec![crate::extension_manifest::DiscoveredExtension {
        manifest: serde_json::from_value(serde_json::json!({"id":"pending-edit","name":"Pending edit","version":"1","enabled":false})).unwrap(),
        manifest_path: project.path().join("extension.json"), wasm_path: project.path().join("extension.wasm"),
        manifest_mtime: None, wasm_mtime: None,
    }], Vec::new());
    app.extension_manager.records[0].text_edit_requests.push(
        crate::extensions::TextEditRequest::InsertText {
            line: 0,
            col: 0,
            text: "late ".into(),
        },
    );
    assert!(app.workspace_exit_stamp().is_none());
    assert!(app.apply_extension_text_edits());
    assert_eq!(app.editor().unwrap().rope.to_string(), "late original");
    assert!(app.workspace_exit_stamp().is_some());
}

#[test]
fn cancelled_native_validation_is_requeued_after_a_fast_workspace_return() {
    let a = Project::new();
    let b = Project::new();
    let path = a.path().join("file.txt");
    std::fs::write(&path, "original").unwrap();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.editor_mut().unwrap().open_file(&path).unwrap();
    let baseline = crate::editor::DiskSaveBaseline {
        rope: app.editor().unwrap().saved_rope.clone().unwrap(),
        encoding: app.editor().unwrap().saved_encoding,
    };
    let cancel = app.workspace_file_check_token();
    let document = app.editor().unwrap().save_identity;
    app.pending_file_reloads.insert(path.clone());
    app.insert_workspace(b.app(wake));
    assert!(cancel.is_cancelled());
    std::fs::write(&path, "new contents").unwrap();
    app.switch_workspace(0);
    // A recheck requested before the old cancellation acknowledgement must
    // survive its retirement, without treating cancellation as a disk error.
    app.queue_workspace_file_check(path.clone());
    app.background
        .sender
        .send(crate::background::BackgroundResult::NativeFileValidated {
            generation: app.workspace_generation,
            path,
            document,
            baseline,
            cancel,
            result: Err("cancelled".into()),
        })
        .unwrap();
    until(|| {
        services(&mut app);
        app.editor().unwrap().rope.to_string() == "new contents"
    });
    assert!(app.save_error.is_none());
}

#[test]
fn reusing_a_live_headless_watcher_acknowledges_readiness_without_rescanning() {
    use crate::remote::{self, Command};
    let project = Project::new();
    let remote = remote::workspace::Workspace::new(headless_connection(project.path()));
    remote.client.call(Command::Watch).unwrap();
    until(|| {
        remote
            .client
            .take_workspace_changes()
            .is_some_and(|batch| batch.active == Some(true))
    });
    // This is the wire sequence after a quick hide/show coalesces away PauseWatch.
    remote.client.call(Command::Watch).unwrap();
    until(|| {
        remote
            .client
            .take_workspace_changes()
            .is_some_and(|batch| batch.active == Some(true) && !batch.rescan && batch.states.is_empty() && !batch.git && !batch.structural)
    });
    remote.disconnect();
}

#[test]
fn pending_language_server_commands_block_closing_until_their_reply() {
    let project = Project::new();
    let mut app = project.app(Default::default());
    let (mut client, server) = crate::lsp::LspClient::test_pair("c", &["*.c"]);
    let id = client
        .send_request(
            "workspace/executeCommand",
            Some(serde_json::json!({"command":"editFiles"})),
        )
        .unwrap();
    app.lsp_manager.clients.push(client);
    assert!(app.workspace_exit_stamp().is_none());
    server.respond(serde_json::json!({"id":id,"result":null}));
    services(&mut app);
    assert!(app.workspace_exit_stamp().is_some());
}

#[test]
fn a_forced_native_refresh_without_a_saved_baseline_reads_bytes_after_metadata_was_observed() {
    let project = Project::new();
    let path = project.path().join("file.txt");
    std::fs::write(&path, "new text").unwrap();
    let metadata = std::fs::metadata(&path).unwrap();
    let mut app = project.app(Default::default());
    let editor = app.editor_mut().unwrap();
    editor.file_path = Some(path.clone());
    editor.last_disk_modified = metadata.modified().ok();
    editor.file_size = metadata.len();
    assert!(editor.saved_rope.is_none());
    app.queue_workspace_file_check(path);
    until(|| {
        services(&mut app);
        app.editor().unwrap().rope.to_string() == "new text"
    });
    assert!(!app.editor().unwrap().modified);
}
