//! Opt-in tests for a real SSH connection. The argument file must launch a
//! headless service in a disposable workspace, never a user's project.
use super::*;
use crossterm::event::{KeyCode, KeyModifiers, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;
use std::time::{Duration, Instant};

fn connect() -> (workspace::Workspace, std::sync::Arc<launch::Process>, f64) {
    let arguments = std::env::var_os("VTCODE_HEADLESS_SSH_ARGS").expect("SSH argument JSON");
    let ssh: Vec<String> = serde_json::from_slice(&std::fs::read(arguments).unwrap()).unwrap();
    let launch = super::tests::raw_launch(ssh.into_iter().map(Into::into));
    let attaching = Instant::now();
    let (client, owner) = launch
        .start(ratatui::layout::Size::new(120, 40), Default::default())
        .unwrap()
        .endpoint_for_test()
        .unwrap();
    (
        workspace::Workspace::new(client),
        owner,
        attaching.elapsed().as_secs_f64() * 1000.,
    )
}

#[test]
#[ignore = "Real SSH: VTCODE_HEADLESS_SSH_ARGS must launch a fresh disposable workspace"]
fn windows_ssh_git_explorer_and_editor_combination_keeps_hunks_saves_and_undo_consistent() {
    let (workspace, owner, _) = connect();
    println!(
        "VTCODE_SSH_CORE_WORKFLOW {}",
        super::core_workflow_tests::run(&workspace)
    );
    super::core_workflow_tests::raw_git_paths(&workspace);
    workspace.client.notify(Command::Close { path: RemotePath::new("core-workflow/file[1] λ.rs".as_bytes().to_vec()).unwrap() }).unwrap();
    workspace.client.call(Command::Ping).unwrap();
    let log = workspace.diagnostics().unwrap().report();
    for operation in ["Read file", "Save document", "Close file", "Refresh Explorer", "Git stage", "Stage hunk"] {
        assert!(log.contains(operation), "Missing operation diagnostics: {operation}\n{log}");
    }
    assert!(log.contains("Completed") && log.contains("Awaiting response"));
    assert!(!log.contains("Disposable core workflow baseline"), "Commit messages are not diagnostic metadata");
    if let Some(path) = std::env::var_os("VTCODE_SSH_TRACE_REPORT") {
        std::fs::write(path, log).unwrap();
    }
    shutdown(&workspace);
    drop((workspace, owner));
}

#[test]
#[ignore = "Real SSH: matching headless build and a fresh disposable workspace"]
fn windows_ssh_scoped_git_status_keeps_full_state_and_native_filename_identity() {
    let (workspace, owner, _) = connect();
    let workflow = super::core_workflow_tests::run(&workspace);
    super::core_workflow_tests::raw_git_paths(&workspace);
    let cancel = crate::background::CancellationToken::new();
    let (initial, _) = workspace.git_snapshot(true, None, false, Vec::new(), None, &cancel).unwrap();
    let known = super::git::Discovery {
        roots: initial.roots.iter().map(|root| workspace.remote_path(root).unwrap()).collect(),
        parent: None,
    };
    let file = RemotePath::new("core-workflow/file[1] λ.rs".as_bytes().to_vec()).unwrap();
    let Reply::GitStatus { scans, .. } = workspace.client.call(Command::GitStatus {
        known: Some(known.clone()), closed: Vec::new(), parent_open: false, include_untracked: true,
        changed: Some(vec![file.clone()]),
    }).unwrap() else { panic!(); };
    assert_eq!(scans.len(), 1);
    assert_eq!(scans[0].worktree_paths.as_ref().unwrap().paths,
        [RemotePath::new("file[1] λ.rs".as_bytes().to_vec()).unwrap()]);
    assert!(!scans[0].worktree_paths.as_ref().unwrap().ignore_case);
    tool_line(&workspace, "python3", vec!["-c".into(),
        "from pathlib import Path; Path('core-workflow/file[1] λ.rs').write_text('scoped status edit\\n'); print('written')".into()]);
    let (_, scoped) = workspace.git_snapshot(true, Some(known.clone()), false, Vec::new(), Some(vec![file]), &cancel).unwrap();
    let (_, complete) = workspace.git_snapshot(true, Some(known.clone()), false, Vec::new(), None, &cancel).unwrap();
    let keys = |scans: Vec<crate::git_repositories::Scan>| scans.into_iter().map(|scan| (scan.root,
        scan.files.into_iter().map(|file| (file.path, file.original_path, file.status, file.staged)).collect::<Vec<_>>()))
        .collect::<Vec<_>>();
    assert_eq!(keys(scoped), keys(complete));
    let Reply::GitStatus { scans, .. } = workspace.client.call(Command::GitStatus {
        known: Some(known), closed: Vec::new(), parent_open: false, include_untracked: true,
        changed: Some(vec![RemotePath::new(b"git-identities/byte-\xff.rs".to_vec()).unwrap()]),
    }).unwrap() else { panic!(); };
    assert_eq!(scans.len(), 1);
    assert!(scans[0].worktree_paths.is_none(), "Non-UTF-8 paths require a lossless complete snapshot");
    println!("VTCODE_SSH_SCOPED_GIT {}", json!({"workflow":workflow,"scoped_equals_full":true,"raw_names":true}));
    shutdown(&workspace);
    drop((workspace, owner));
}

#[test]
#[ignore = "Requires an incompatible headless build in VTCODE_HEADLESS_MISMATCH_SSH_ARGS"]
fn ssh_version_mismatch_explains_both_endpoints_and_stops_attachment() {
    let arguments =
        std::env::var_os("VTCODE_HEADLESS_MISMATCH_SSH_ARGS").expect("SSH argument JSON");
    let ssh: Vec<String> = serde_json::from_slice(&std::fs::read(arguments).unwrap()).unwrap();
    let setup = super::tests::raw_launch(ssh.into_iter().map(Into::into))
    .start(ratatui::layout::Size::new(120, 40), Default::default())
    .unwrap();
    let error = match setup.endpoint_for_test() {
        Err(error) => error,
        Ok(_) => panic!("Incompatible headless build opened a workspace"),
    };
    let message = error.to_string();
    assert!(message.starts_with("VTCode version mismatch:"), "{message}");
    assert!(
        message.contains("local ") && message.contains("headless "),
        "{message}"
    );
    assert!(
        message.contains("Install matching VTCode builds on both machines"),
        "{message}"
    );
    println!("{message}");
}

fn test_app(root: PathBuf) -> crate::app::App {
    let mut app = crate::app::App::new_deferred(root);
    app.source_control_enabled = false;
    app.trim_trailing_on_save = false;
    app.insert_final_newline = false;
    app
}

#[test]
#[ignore = "Real SSH: matching headless in VTCODE_HEADLESS_SSH_ARGS and a disposable workspace"]
fn ssh_repeated_clean_saves_send_no_requests_and_rapid_dirty_saves_keep_one_pending_intent() {
    let (workspace, owner, _) = connect();
    let path = RemotePath::new(b"save-traffic.txt".to_vec()).unwrap();
    workspace.client.call(Command::Save {
        path: path.clone(), base: None, encoding: "UTF-8".into(),
        change: Change { start: 0, end: 0, text: "original\n".into() },
    }).unwrap();
    let mut app = test_app(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    app.bg_explorer_refresh_running = true;
    app.format_on_save = true;
    open(&mut app, &workspace.view_key(&path));
    let before = workspace.client.next_request_id_for_test();
    for _ in 0..128 { app.save_current_file(); }
    app.wait_for_background(Duration::from_secs(30), |app| app.pending_file_saves.is_empty());
    assert_eq!(workspace.client.next_request_id_for_test(), before);
    assert!(!app.formatter_settings.active);
    app.format_on_save = false;
    app.editor_mut().unwrap().insert_char('A');
    for _ in 0..128 { app.save_current_file(); }
    assert_eq!(app.pending_file_saves.len(), 1);
    assert!(app.deferred_file_saves.is_empty());
    app.wait_for_background(Duration::from_secs(30), |app| app.pending_file_saves.is_empty() && app.background.active_job_count() == 0);
    assert!(!app.editor().unwrap().modified);
    let before = workspace.client.next_request_id_for_test();
    let jobs = app.background.activity.job_count();
    for _ in 0..1024 { app.save_current_file(); }
    assert_eq!(workspace.client.next_request_id_for_test(), before);
    assert_eq!(app.background.activity.job_count(), jobs);
    assert_eq!(workspace.load(&workspace.view_key(&path)).unwrap().rope, "Aoriginal\n");
    println!("VTCODE_SSH_SAVE_TRAFFIC: 128 initial clean saves and 1024 repeated clean saves sent 0 requests; 128 dirty saves retained one write");
    shutdown(&workspace);
    drop((workspace, owner));
}

fn reconnecting_workspace() -> workspace::Workspace {
    let arguments = std::env::var_os("VTCODE_HEADLESS_SSH_ARGS").expect("SSH argument JSON");
    let ssh: Vec<String> = serde_json::from_slice(&std::fs::read(arguments).unwrap()).unwrap();
    super::tests::raw_launch(ssh.into_iter().map(Into::into))
        .start(ratatui::layout::Size::new(120, 40), Default::default()).unwrap().workspace_for_test().unwrap()
}

#[test]
#[ignore = "Real SSH: VTCODE_HEADLESS_SSH_ARGS launches matching headless in a disposable workspace"]
fn ssh_logs_are_available_while_connected_and_after_transport_disconnect() {
    let workspace = reconnecting_workspace();
    let mut app = test_app(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    app.new_file();
    app.editor_mut().unwrap().insert_char('λ');
    app.execute_palette_command("show_output");
    assert!(workspace.client.connected());
    assert!(app.show_panel && app.panel_tab == 2);
    assert!(!app.side_notice.visible());
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    let screen: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
    assert!(screen.contains("Remote SSH Connection") && screen.contains("Handshake"));
    assert!(!workspace.client.capturing_cpu(), "opening Logs never samples the node");
    workspace.client.call(Command::Watch).unwrap();
    workspace.client.call(Command::GitStatus { include_untracked: true, known: None, parent_open: false, closed: Vec::new(), changed: None }).unwrap();
    app.execute_palette_command("remote_capture_cpu");
    assert!(workspace.client.capturing_cpu());
    app.wait_for_background(Duration::from_secs(30), |_| !workspace.client.capturing_cpu());
    let capture = workspace.diagnostics().unwrap().report();
    assert!(capture.contains("Capture complete") && capture.contains("100% = one logical CPU"), "{capture}");
    assert!(capture.contains("Thread ") || capture.contains("partial/unavailable"), "native thread counters or their availability are reported: {capture}");
    if let Some(path) = std::env::var_os("VTCODE_SSH_TRACE_REPORT") {
        std::fs::write(path, &capture).unwrap();
    }
    // Optional quiet window lets an external /proc probe verify that sampling
    // really stops after an explicit capture. No timer is added to VTCode.
    if let Ok(seconds) = std::env::var("VTCODE_SSH_CPU_IDLE_SECONDS") {
        std::thread::sleep(Duration::from_secs(seconds.parse().unwrap()));
    }
    workspace.disconnect();
    app.wait_for_background(Duration::from_secs(15), |app|
        app.remote_connection.phase == crate::app_remote_connection::Phase::Stopped);
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    let screen: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
    assert!(app.show_panel && app.panel_tab == 2);
    assert!(screen.contains("Disconnected"));
    crate::ui_panels::close_panel(&mut app);
    app.execute_palette_command("show_output");
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    let screen: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
    assert!(screen.contains("Disconnected"));
    crate::ui_panels::close_panel(&mut app);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, "");
    app.editor_mut().unwrap().redo();
    assert_eq!(app.editor().unwrap().rope, "λ");
}

#[test]
#[ignore = "Real SSH: matching protocol-17 service in VTCODE_HEADLESS_SSH_ARGS and disposable workspace with file.txt"]
fn ssh_save_conflict_popup_compares_latest_disk_and_overwrites_without_losing_undo() {
    let workspace = reconnecting_workspace();
    let key = workspace.target_key("file.txt").unwrap();
    let mut app = test_app(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    app.format_on_save = false;
    open(&mut app, &key);
    app.editor_mut().unwrap().insert_char('X');
    tool_line(&workspace, "python3", vec!["-c".into(), "from pathlib import Path; Path('file.txt').write_text('external\\n'); print('changed')".into()]);
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(30), |app| app.pending_file_saves.is_empty());
    assert!(app.save_conflict.is_some());
    let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    let text: String = terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect();
    for label in ["File changed outside VTCode", "Overwrite", "View Diff"] { assert!(text.contains(label), "{text}"); }
    app.view_save_conflict();
    app.wait_for_background(Duration::from_secs(30), |app| app.save_error.is_none());
    assert_eq!(app.compare_base_content.as_deref(), Some("external\n"));
    assert_eq!(app.editor().unwrap().rope, "Xoriginal\n");
    assert!(app.editor().unwrap().read_only);
    app.active_tab = 0;
    app.sync_diff_tab();
    tool_line(&workspace, "python3", vec!["-c".into(), "from pathlib import Path; Path('file.txt').write_text('another external edit\\n'); print('changed again')".into()]);
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(30), |app| app.pending_file_saves.is_empty());
    assert!(app.save_conflict.is_some());
    app.overwrite_save_conflict();
    app.editor_mut().unwrap().insert_char('Y');
    app.save_current_file();
    let expected = app.editor().unwrap().rope.clone();
    app.wait_for_background(Duration::from_secs(30), |app| app.pending_file_saves.is_empty() && app.deferred_file_saves.is_empty());
    assert!(app.save_error.is_none(), "{:?}", app.save_error);
    assert_eq!(workspace.load(&key).unwrap().rope, expected);
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, "Xoriginal\n");
    assert!(app.editor().unwrap().modified);
    app.editor_mut().unwrap().redo();
    assert!(!app.editor().unwrap().modified);
    shutdown(&workspace);
    workspace.disconnect();
}

#[test]
#[ignore = "Real SSH: VTCODE_HEADLESS_CLI_ARGS supplies --remote/--remote-command/--ssh-command for a disposable workspace with file.txt"]
fn ssh_simplified_remote_cli_opens_saves_and_relaunches_with_the_same_overrides() {
    let path = std::env::var_os("VTCODE_HEADLESS_CLI_ARGS").expect("Simplified CLI argument JSON");
    let args: Vec<String> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let workspace = launch::Launch::parse(args.into_iter().map(Into::into)).unwrap().unwrap()
        .start(ratatui::layout::Size::new(100, 30), Default::default()).unwrap().workspace_for_test().unwrap();
    let recipe = workspace.recipe.clone().unwrap();
    let key = workspace.target_key("file.txt").unwrap();
    let mut app = test_app(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    open(&mut app, &key);
    app.editor_mut().unwrap().paste_plain_with_text("simplified CLI λ🦀\n");
    let expected = app.editor().unwrap().rope.to_string();
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(30), |app| app.pending_file_saves.is_empty());
    assert!(!app.editor().unwrap().modified);
    shutdown(&workspace);
    workspace.disconnect();
    let recovered = recipe.start(ratatui::layout::Size::new(100, 30), Default::default()).unwrap().workspace_for_test().unwrap();
    assert_eq!(recovered.view_root, workspace.view_root);
    let next_key = recovered.target_key("file.txt").unwrap();
    let mut next_app = test_app(recovered.view_root.clone());
    next_app.remote_workspace = Some(recovered.clone());
    open(&mut next_app, &next_key);
    assert_eq!(next_app.editor().unwrap().rope.to_string(), expected);
    shutdown(&recovered);
    recovered.disconnect();
}

#[test]
#[ignore = "Real SSH: stable disposable fixture fails the second launch with diagnostic text and exit 42, then recovers; owned-process VTCODE_HEADLESS_KILL_ARGS required"]
fn ssh_diagnostics_retain_failed_reconnect_reason_after_success_without_buffer_text() {
    use crate::app_remote_connection::Phase;
    let workspace = reconnecting_workspace();
    let log = workspace.diagnostics().unwrap();
    let path = workspace.target_key("file.txt").unwrap();
    let mut app = test_app(workspace.view_root.clone()); app.remote_workspace = Some(workspace);
    open(&mut app, &path);
    app.editor_mut().unwrap().paste_plain_with_text("private unsaved buffer λ🦀\n");
    app.editor_mut().unwrap().break_undo_group();
    let expected = app.editor().unwrap().rope.to_string();
    let arguments = std::env::var_os("VTCODE_HEADLESS_KILL_ARGS").expect("Owned-process failure command JSON");
    let kill: Vec<String> = serde_json::from_slice(&std::fs::read(arguments).unwrap()).unwrap();
    let output = std::process::Command::new(&kill[0]).args(&kill[1..]).arg("headless").output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    app.wait_for_background(Duration::from_secs(60), |app| app.remote_connection.phase == Phase::Restored);
    let report = log.report();
    assert!(report.contains("diagnostic fixture submission temporarily unavailable"), "{report}");
    assert!(report.contains("code 42") && report.contains("Connection attempt: 3 · Starting"), "{report}");
    assert!(report.contains("Disconnected:") && report.contains("Restored:"), "{report}");
    assert!(!report.contains("private unsaved buffer"), "Protocol contents entered the log");
    assert_eq!(app.editor().unwrap().rope.to_string(), expected);
    app.show_remote_diagnostics();
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(30), |app| app.pending_file_saves.is_empty());
    assert!(!app.editor().unwrap().modified);
    assert!(!log.report().contains("private unsaved buffer"));
    println!("VTCODE_SSH_DIAGNOSTICS\n{report}");
    shutdown(app.remote_workspace.as_ref().unwrap());
}

#[test]
#[ignore = "Real SSH: stable disposable workspace and VTCODE_HEADLESS_KILL_ARGS for its owned headless/sshd session only"]
fn ssh_reconnect_after_service_and_transport_death_keeps_edits_undo_and_file_operations() {
    use crate::app_remote_connection::Phase;
    let arguments = std::env::var_os("VTCODE_HEADLESS_SSH_ARGS").expect("SSH argument JSON");
    let ssh: Vec<String> = serde_json::from_slice(&std::fs::read(arguments).unwrap()).unwrap();
    let workspace = super::tests::raw_launch(ssh.into_iter().map(Into::into))
        .start(ratatui::layout::Size::new(120, 40), Default::default()).unwrap().workspace_for_test().unwrap();
    let kill_arguments = std::env::var_os("VTCODE_HEADLESS_KILL_ARGS").expect("Owned-process failure command JSON");
    let kill: Vec<String> = serde_json::from_slice(&std::fs::read(kill_arguments).unwrap()).unwrap();
    let path = workspace.target_key("file.txt").unwrap();
    let mut app = test_app(workspace.view_root.clone());
    app.remote_workspace = Some(workspace);
    open(&mut app, &path);
    app.source_control_enabled = true;
    app.refresh_source_control_async();
    app.refresh_git_log_async();
    app.wait_for_background(Duration::from_secs(30), |app| !app.bg_git_status_running && !app.git_log.is_empty());
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let mut failures = Vec::new();
    for (kind, marker) in [("headless", 'h'), ("ssh", 's')] {
        let old = app.remote_workspace.as_ref().unwrap().clone();
        let previous = app.editor().unwrap().rope.to_string();
        app.editor_mut().unwrap().set_cursor(0, 0);
        app.editor_mut().unwrap().insert_char(marker);
        app.editor_mut().unwrap().break_undo_group();
        let expected = format!("{marker}{previous}");
        let killed = std::process::Command::new(&kill[0]).args(&kill[1..]).arg(kind).output().unwrap();
        assert!(killed.status.success(), "{}", String::from_utf8_lossy(&killed.stderr));
        let start = Instant::now();
        let mut samples = Vec::new();
        let mut saw_popup = false;
        while start.elapsed() < Duration::from_secs(60) {
            let sample = Instant::now();
            app.poll_background_results();
            saw_popup |= app.side_notice.visible() && app.remote_connection.phase != Phase::Connected;
            // These editor actions never need the SSH host to answer.
            let editor = app.editor_mut().unwrap();
            editor.insert_char('λ'); editor.break_undo_group(); editor.undo();
            editor.scroll_down(3); editor.scroll_up(3);
            terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
            samples.push(sample.elapsed().as_secs_f64() * 1000.);
            if app.remote_connection.phase == Phase::Restored { break; }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(saw_popup, "Transport loss did not show the side popup");
        assert!(app.remote_connection.phase == Phase::Restored, "{}: {}", app.remote_connection.message, app.remote_connection.error);
        assert!(!old.client.connected());
        assert_ne!(app.remote_workspace.as_ref().unwrap().client.hello().session, old.client.hello().session);
        assert_eq!(app.editor().unwrap().rope.to_string(), expected);
        assert!(app.editor().unwrap().modified);
        app.editor_mut().unwrap().undo();
        assert_eq!(app.editor().unwrap().rope.to_string(), previous);
        app.editor_mut().unwrap().redo();
        assert_eq!(app.editor().unwrap().rope.to_string(), expected);
        app.save_current_file();
        app.wait_for_background(Duration::from_secs(30), |app| app.pending_file_saves.is_empty());
        assert!(!app.editor().unwrap().modified);
        let remote = app.remote_workspace.as_ref().unwrap();
        assert_eq!(remote.load(&path).unwrap().rope.to_string(), expected);
        assert!(remote.client.call(Command::Ping).is_ok());
        app.wait_for_background(Duration::from_secs(30), |app| !app.bg_git_status_running && !app.bg_git_log_running);
        assert!(!app.git_log.is_empty(), "Git history did not resume after reconnecting");
        samples.sort_by(f64::total_cmp);
        failures.push(json!({"failure":kind,"reconnect_ms":start.elapsed().as_secs_f64()*1000.,
            "samples":samples.len(),"local_ops_p95_ms":samples[samples.len()*95/100]}));
    }
    app.force_close_tab(app.active_tab);
    open(&mut app, &path);
    assert!(!app.editor().unwrap().bg_loading);
    shutdown(app.remote_workspace.as_ref().unwrap());
    println!("VTCODE_SSH_RECONNECT {}", json!(failures));
}

#[test]
#[ignore = "Real SSH: five-minute idle soak in a fresh disposable workspace"]
fn windows_ssh_five_minute_idle_resumes_editing_file_operations_and_quit() {
    let (workspace, owner, _) = connect();
    let relative = RemotePath::new(b"idle-soak.py".to_vec()).unwrap();
    let text = format!("value = '{}'\n", "x".repeat(160)).repeat(100);
    workspace.client.call(Command::Save {
        path: relative.clone(), base: None, encoding: "UTF-8".into(),
        change: Change { start: 0, end: 0, text: text.clone() },
    }).unwrap();
    let mut app = test_app(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    open(&mut app, &workspace.view_key(&relative));
    println!("VTCODE_SSH_IDLE starting five minutes without user input");
    std::thread::sleep(Duration::from_secs(300));
    assert!(workspace.client.connected(), "A healthy idle connection disconnected");
    let metrics = workload(&mut app, &text);
    app.force_close_tab(app.active_tab);
    app.exit_confirm_active = true;
    assert!(crate::input_keyboard::handle_keyboard_event(&mut app,
        crossterm::event::KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)));
    println!("VTCODE_SSH_IDLE {}", json!({"idle_seconds":300,"resumed":metrics}));
    shutdown(&workspace);
    drop((app, workspace, owner));
}

#[test]
#[ignore = "Real SSH: disposable bridge_lab fixture stalls both directions after 6 seconds"]
fn windows_ssh_stalled_link_can_disconnect_and_keep_quit_usable() {
    let (mut workspace, owner, _) = connect();
    workspace.connection = Some(owner.clone());
    let relative = RemotePath::new(b"silent-link.py".to_vec()).unwrap();
    let text = "value = 42\n".repeat(100);
    workspace.client.call(Command::Save {
        path: relative.clone(), base: None, encoding: "UTF-8".into(),
        change: Change { start: 0, end: 0, text: text.clone() },
    }).unwrap();
    let mut app = test_app(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    open(&mut app, &workspace.view_key(&relative));
    // The fault fixture retains SSH and its pipes, but stops delivering data.
    std::thread::sleep(Duration::from_secs(8));
    app.editor_mut().unwrap().insert_char('x');
    app.editor_mut().unwrap().break_undo_group();
    app.save_current_file();
    let expected = format!("x{text}");
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let mut samples = Vec::new();
    for _ in 0..256 {
        let start = Instant::now();
        for key in [KeyCode::Right, KeyCode::Left, KeyCode::Down, KeyCode::Up] {
            crate::input::handle_editor_key(&mut app, key, KeyModifiers::NONE);
        }
        let editor = app.editor_mut().unwrap();
        editor.insert_char('λ');
        editor.undo();
        editor.paste_plain_with_text("\t🦀\r\n");
        editor.undo();
        editor.scroll_down(3);
        editor.scroll_up(3);
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        app.poll_background_results();
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    assert_eq!(app.editor().unwrap().rope.to_string(), expected);
    assert!(!app.pending_file_saves.is_empty(), "Save must still be awaiting the stalled peer");
    assert!(workspace.client.connected(), "Silence alone must not disconnect the transport");
    workspace.client.disconnect();
    app.wait_for_background(Duration::from_secs(5), |app| app.pending_file_saves.is_empty());
    assert_eq!(app.editor().unwrap().rope.to_string(), expected);
    assert!(app.editor().unwrap().modified);
    crate::input_keyboard::handle_keyboard_event(&mut app,
        crossterm::event::KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    app.exit_confirm_active = true;
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    let button = app.modal.buttons.iter().find(|(_, index)| *index == 2).unwrap().0;
    crate::mouse::handle_mouse(&mut app, MouseEvent {
        kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: button.x + button.width / 2, row: button.y, modifiers: KeyModifiers::NONE,
    });
    assert!(app.exit_keep_unsaved_quit);
    let session = app.build_session();
    assert_eq!(session.tabs[0].pending_unsaved_content.as_ref().unwrap().to_string(), expected);
    assert!(!session.tabs[0].undo_entries.as_ref().unwrap().is_empty());
    samples.sort_by(f64::total_cmp);
    println!("VTCODE_SSH_SILENT {}", json!({
        "explicit_disconnect":true,
        "samples":samples.len(), "local_ops_p95_ms":samples[samples.len() * 95 / 100],
        "pending_saves":app.pending_file_saves.len(), "kept_local_edits":true,
    }));
    drop((app, workspace, owner));
}

fn open(app: &mut crate::app::App, path: &Path) -> f64 {
    let start = Instant::now();
    app.open_file(path);
    app.wait_for_background(Duration::from_secs(90), |app| {
        app.editor().is_some_and(|editor| !editor.bg_loading)
    });
    start.elapsed().as_secs_f64() * 1000.
}

fn shutdown(workspace: &workspace::Workspace) {
    workspace.client.call(Command::Shutdown).unwrap();
    // The disposable SSH command must finish before its owner is dropped;
    // otherwise killing SSH can interrupt the fault harness's final metrics.
    let end = Instant::now() + Duration::from_secs(10);
    while workspace.client.connected() && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        !workspace.client.connected(),
        "Disposable SSH command did not finish after Shutdown"
    );
}

fn workload(app: &mut crate::app::App, text: &str) -> serde_json::Value {
    // The real event loop seeds this cache from the current line before its
    // optional full-document analysis; this focused workload has no event loop.
    app.cached_max_line_width = app.editor().unwrap().current_line_len();
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    for _ in 0..8 {
        terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    }
    // Save A, then immediately type B. The acknowledgement must retain B and
    // leave the document dirty. Navigation/rendering never waits for that ACK.
    app.editor_mut().unwrap().insert_char('A');
    app.editor_mut().unwrap().break_undo_group();
    let save = Instant::now();
    app.save_current_file();
    app.editor_mut().unwrap().insert_char('B');
    app.editor_mut().unwrap().break_undo_group();
    let mut save_ms = None;
    let mut samples = Vec::new();
    for _ in 0..256 {
        let start = Instant::now();
        for (key, modifiers) in [
            (KeyCode::Right, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            (KeyCode::Left, KeyModifiers::CONTROL),
            (KeyCode::Down, KeyModifiers::NONE),
            (KeyCode::Up, KeyModifiers::NONE),
        ] {
            crate::input::handle_editor_key(app, key, modifiers);
        }
        let editor = app.editor_mut().unwrap();
        editor.insert_char('λ');
        editor.undo();
        editor.redo();
        editor.undo();
        editor.paste_plain_with_text("\tλ 🦀\r\n  mixed\n");
        editor.undo();
        editor.scroll_down(3);
        editor.scroll_up(3);
        let area = app.editor_area.unwrap();
        let old_vertical = app.editor().unwrap().scroll_offset;
        for kind in [MouseEventKind::ScrollDown, MouseEventKind::ScrollUp] {
            crate::mouse::handle_mouse(
                app,
                MouseEvent {
                    kind,
                    column: area.x + area.width / 2,
                    row: area.y + area.height / 2,
                    modifiers: KeyModifiers::SHIFT,
                },
            );
            if kind == MouseEventKind::ScrollDown {
                assert!(app.editor().unwrap().horizontal_scroll > 0);
            }
        }
        assert_eq!(app.editor().unwrap().scroll_offset, old_vertical);
        terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
        app.poll_background_results();
        if save_ms.is_none() && app.pending_file_saves.is_empty() {
            save_ms = Some(save.elapsed().as_secs_f64() * 1000.);
        }
        samples.push(start.elapsed().as_secs_f64() * 1000.);
    }
    app.wait_for_background(Duration::from_secs(90), |app| {
        app.pending_file_saves.is_empty()
    });
    let save_ms = save_ms.unwrap_or_else(|| save.elapsed().as_secs_f64() * 1000.);
    assert_eq!(app.editor().unwrap().rope.to_string(), format!("AB{text}"));
    assert!(app.editor().unwrap().modified);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), format!("A{text}"));
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().redo();
    assert!(app.editor().unwrap().modified);
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(90), |app| {
        app.pending_file_saves.is_empty()
    });
    assert!(!app.editor().unwrap().modified);
    samples.sort_by(f64::total_cmp);
    json!({"samples":samples.len(), "save_ack_ms":save_ms,
        "input_draw_p50_ms":samples[samples.len()/2],
        "input_draw_p95_ms":samples[samples.len()*95/100],
        "input_draw_max_ms":samples[samples.len()-1]})
}

#[test]
#[ignore = "Real SSH: VTCODE_HEADLESS_SSH_ARGS must name an argument JSON launching a disposable workspace"]
fn windows_ssh_editor_latency_and_save_undo_workflow() {
    let (workspace, owner, attach_ms) = connect();
    // Long first line makes horizontal-wheel behavior observable; the rest
    // covers UTF-8, tabs, mixed indentation, CRLF, and multiple viewports.
    let mut text = format!("// {}\r\n", "wide λ 🦀 ".repeat(80));
    for index in 0..8000 {
        text.push_str(&format!(
            "fn example_{index}() {{\r\n\tlet café_{index} = \"🦀{index:08x}\";\r\n}}\r\n"
        ));
    }
    let remote = RemotePath::new(b"ssh-workload.rs".to_vec()).unwrap();
    workspace
        .client
        .call_file(Command::Save {
            path: remote.clone(),
            base: None,
            encoding: "UTF-8".into(),
            change: Change {
                start: 0,
                end: 0,
                text: text.clone(),
            },
        })
        .unwrap();
    let temporary = tempfile::tempdir().unwrap();
    let native_path = temporary.path().join("ssh-workload.rs");
    std::fs::write(&native_path, &text).unwrap();
    let mut native = test_app(temporary.path().to_owned());
    let native_open_ms = open(&mut native, &native_path);
    let native_result = workload(&mut native, &text);
    assert_eq!(
        std::fs::read_to_string(native_path).unwrap(),
        format!("AB{text}")
    );
    drop(native);

    let mut app = test_app(workspace.view_root.clone());
    workspace.client.set_wake(app.background.wake_handle());
    app.remote_workspace = Some(workspace.clone());
    let key = workspace.view_key(&remote);
    let remote_open_ms = open(&mut app, &key);
    assert_eq!(app.editor().unwrap().rope.to_string(), text);
    let remote_result = workload(&mut app, &text);
    assert_eq!(
        workspace.load(&key).unwrap().rope.to_string(),
        format!("AB{text}")
    );
    println!(
        "VTCODE_SSH_LAB {}",
        json!({"bytes":text.len(), "attach_ms":attach_ms,
        "native_open_ms":native_open_ms, "remote_open_ms":remote_open_ms,
        "native":native_result, "remote":remote_result})
    );
    shutdown(&workspace);
    drop(app);
    drop(workspace);
    drop(owner);
}

#[test]
#[ignore = "Real SSH: VTCODE_HEADLESS_SSH_ARGS must launch a disposable workspace"]
fn windows_ssh_remote_locations_keep_buffers_navigation_and_save_conflicts_correct() {
    let (workspace, owner, _) = connect();
    for name in ["links folder", "unlisted", "unlisted/nested"] {
        workspace
            .client
            .call(Command::CreateDirectory {
                path: RemotePath::new(name.as_bytes().to_vec()).unwrap(),
            })
            .unwrap();
    }
    let path = RemotePath::new("links folder/猫.txt".as_bytes().to_vec()).unwrap();
    let text = "first\r\n\tλ 🦀\r\nlast\r\n".repeat(30);
    workspace
        .client
        .call_file(Command::Save {
            path: path.clone(),
            base: None,
            encoding: "UTF-8".into(),
            change: Change {
                start: 0,
                end: 0,
                text: text.clone(),
            },
        })
        .unwrap();
    let key = workspace.view_key(&path);
    let mut app = test_app(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    workspace.client.set_wake(app.background.wake_handle());
    app.open_chat_location(&key, Some(1));
    app.wait_for_background(Duration::from_secs(90), |app| {
        app.editor()
            .is_some_and(|editor| editor.file_path.as_ref() == Some(&key) && !editor.bg_loading)
    });
    assert_eq!(app.editor().unwrap().rope.to_string(), text);
    assert_eq!(app.editor().unwrap().cursor_line, 1);
    app.editor_mut().unwrap().insert_char('!');
    let dirty = app.editor().unwrap().rope.to_string();
    let navigation_id = app.editor().unwrap().navigation_id;
    let tabs = app.tabs.len();
    let directory = workspace.view_key(&RemotePath::new(b"unlisted/nested".to_vec()).unwrap());
    app.open_chat_location(&directory, None);
    app.wait_for_background(Duration::from_secs(90), |app| {
        app.active_pane == crate::app::ActivePane::Explorer
            && app
                .explorer
                .entries
                .iter()
                .any(|entry| entry.path == directory)
    });
    assert_eq!(app.tabs.len(), tabs);
    app.open_chat_location(&key, Some(1));
    assert_eq!(app.editor().unwrap().navigation_id, navigation_id);
    assert_eq!(app.editor().unwrap().rope.to_string(), dirty);
    assert!(app.editor().unwrap().modified);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), text);
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().redo();
    assert_eq!(app.editor().unwrap().rope.to_string(), dirty);
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(90), |app| {
        app.pending_file_saves.is_empty()
    });
    assert!(!app.editor().unwrap().modified);
    let saved = workspace.load(&key).unwrap();
    assert_eq!(saved.rope.to_string(), dirty);
    // A second writer changing the node file must still conflict with a local
    // pending edit, including after the prefetched link-open populated caches.
    app.editor_mut().unwrap().insert_char('X');
    let local = app.editor().unwrap().rope.to_string();
    workspace
        .client
        .call_file(Command::Save {
            path,
            base: Some(saved.remote_checkpoint.unwrap().revision.clone()),
            encoding: "UTF-8".into(),
            change: Change {
                start: 0,
                end: saved.rope.len_chars(),
                text: "external writer\r\n".into(),
            },
        })
        .unwrap();
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(90), |app| {
        app.pending_file_saves.is_empty()
    });
    assert_eq!(app.editor().unwrap().rope.to_string(), local);
    assert!(app.editor().unwrap().modified);
    assert_eq!(
        workspace.load(&key).unwrap().rope.to_string(),
        "external writer\r\n"
    );
    println!(
        "VTCODE_SSH_LOCATIONS: file, directory, live dirty buffer, undo/redo, save and external conflict passed"
    );
    shutdown(&workspace);
    drop((app, workspace, owner));
}

#[test]
#[ignore = "Real SSH: VTCODE_HEADLESS_SSH_ARGS must launch a fresh disposable workspace"]
fn windows_ssh_agent_writes_keep_split_edits_and_reload_undo() {
    let (workspace, owner, _) = connect();
    let path = RemotePath::new(b"agent-reload.rs".to_vec()).unwrap();
    let original = "fn original() {}\n";
    let node_edit = "fn node_edit() { /* λ */ }\n";
    workspace
        .client
        .call_file(Command::Save {
            path: path.clone(),
            base: None,
            encoding: "UTF-8".into(),
            change: Change {
                start: 0,
                end: 0,
                text: original.into(),
            },
        })
        .unwrap();
    let mut app = test_app(workspace.view_root.clone());
    workspace.client.set_wake(app.background.wake_handle());
    app.remote_workspace = Some(workspace.clone());
    let key = workspace.view_key(&path);
    open(&mut app, &key);
    let mut duplicate = crate::editor::Editor::new();
    duplicate.file_path = Some(key.clone());
    duplicate.copy_document_from(app.editor().unwrap());
    app.tabs.push(crate::app::Tab::new_file(duplicate));
    app.create_split_group_dir(1, crate::app::SplitDirection::Horizontal);
    let replace = |app: &crate::app::App, before: &str, after: &str| {
        workspace
            .client
            .call_file(Command::Save {
                path: path.clone(),
                base: Some(
                    app.editor()
                        .unwrap()
                        .remote_checkpoint
                        .as_ref()
                        .unwrap()
                        .revision
                        .clone(),
                ),
                encoding: "UTF-8".into(),
                change: Change {
                    start: 0,
                    end: before.chars().count(),
                    text: after.into(),
                },
            })
            .unwrap();
    };
    let event = |before: &str, after: &str| {
        vtcode::acp::Event::FileEdited(vtcode::acp::FileEdit {
            session_id: vtcode::acp::protocol::SessionId::new("node-edit"),
            path: key.clone(),
            before: before.into(),
            after: after.into(),
        })
    };
    let settle = |app: &mut crate::app::App| {
        app.wait_for_background(Duration::from_secs(90), |app| {
            !app.background.has_active_jobs() && !app.background.has_pending_results()
        });
    };
    replace(&app, original, node_edit);
    app.apply_acp_event(event(original, node_edit));
    // The background read may finish, but no UI completion is processed until
    // after this local edit. The other pane still has its old clean snapshot.
    app.editor_mut().unwrap().insert_char('!');
    settle(&mut app);
    let local = format!("!{original}");
    for tab in app
        .tabs
        .iter_mut()
        .chain(app.groups.iter_mut().flat_map(|group| &mut group.tabs))
    {
        tab.editor.refresh_document();
        assert_eq!(tab.editor.rope, local.as_str());
        assert!(tab.editor.modified);
    }
    app.save_current_file();
    settle(&mut app);
    assert!(
        app.save_error.is_some(),
        "the newer node write must still conflict"
    );
    assert_eq!(app.editor().unwrap().rope, local.as_str());
    assert!(app.editor().unwrap().modified);
    assert_eq!(workspace.load(&key).unwrap().rope, node_edit);
    app.save_error = None;
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, original);
    assert!(!app.editor().unwrap().modified);
    app.queue_file_reload(key.clone());
    settle(&mut app);
    assert_eq!(app.editor().unwrap().rope, node_edit);
    assert!(!app.editor().unwrap().modified);
    let next = "fn newer_node_edit() {}\n";
    replace(&app, node_edit, next);
    app.apply_acp_event(event(node_edit, next));
    settle(&mut app);
    app.switch_group(0);
    assert_eq!(app.editor().unwrap().rope, next);
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, node_edit);
    assert!(app.editor().unwrap().modified);
    app.switch_group(1);
    app.editor_mut().unwrap().redo();
    assert_eq!(app.editor().unwrap().rope, next);
    assert!(!app.editor().unwrap().modified);
    assert_eq!(workspace.load(&key).unwrap().rope, next);
    println!(
        "VTCODE_SSH_AGENT_RELOAD: split typing during reload, save conflict, clean reload and shared undo/redo passed"
    );
    shutdown(&workspace);
    drop((app, workspace, owner));
}

#[test]
#[ignore = "Real SSH: VTCODE_HEADLESS_SSH_ARGS must launch a fresh disposable workspace"]
fn windows_ssh_file_commands_and_save_as_preserve_filename_bytes() {
    let (workspace, owner, _) = connect();
    let directory = RemotePath::new(b"chosen".to_vec()).unwrap();
    workspace
        .client
        .call(Command::CreateDirectory {
            path: directory.clone(),
        })
        .unwrap();
    let mut app = test_app(workspace.view_root.clone());
    workspace.client.set_wake(app.background.wake_handle());
    app.remote_workspace = Some(workspace.clone());
    for name in [
        br"literal\name.txt".as_slice(),
        b"trailing ",
        b"non-\xff.txt",
        b"main.cpp",
        b"main.hpp",
    ] {
        let path = RemotePath::new(name.to_vec()).unwrap();
        workspace
            .client
            .call(Command::Save {
                path: path.clone(),
                base: None,
                encoding: "UTF-8".into(),
                change: Change {
                    start: 0,
                    end: 0,
                    text: "original\n".into(),
                },
            })
            .unwrap();
        if name == b"main.cpp" || name == b"main.hpp" {
            continue;
        }
        let key = workspace.view_key(&path);
        open(&mut app, &key);
        app.editor_mut().unwrap().insert_char('X');
        app.execute_palette_command("save_as");
        assert_eq!(workspace.target_key(&app.save_as_query).unwrap(), key);
        app.execute_save_as();
        assert!(!app.save_as_active, "{:?}", app.modal.error);
        app.wait_for_background(Duration::from_secs(30), |app| {
            app.pending_file_saves.is_empty()
        });
        assert!(!app.editor().unwrap().modified);
        assert_eq!(
            workspace.load(&key).unwrap().rope.to_string(),
            "Xoriginal\n"
        );
        app.editor_mut().unwrap().undo();
        assert!(app.editor().unwrap().modified);
        app.editor_mut().unwrap().redo();
        assert!(!app.editor().unwrap().modified);
        app.execute_palette_command("save_as");
        let folder = workspace.view_key(&directory);
        app.folder_browser_path = folder.clone();
        app.folder_browser.folder = workspace.display_path(&folder).unwrap();
        app.folder_browser.save_as_return = true;
        app.confirm_folder_browser();
        let target = workspace.view_key(&directory.child(name).unwrap());
        assert_eq!(workspace.target_key(&app.save_as_query).unwrap(), target);
        app.execute_save_as();
        app.wait_for_background(Duration::from_secs(30), |app| {
            app.pending_file_saves.is_empty() && app.pending_save_as.is_empty()
        });
        assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&target));
        assert_eq!(
            workspace.load(&target).unwrap().rope.to_string(),
            "Xoriginal\n"
        );
        assert_eq!(
            workspace.load(&key).unwrap().rope.to_string(),
            "Xoriginal\n"
        );
    }
    let cpp = workspace.target_key("main.cpp").unwrap();
    let hpp = workspace.target_key("main.hpp").unwrap();
    open(&mut app, &cpp);
    app.execute_palette_command("switch_header_source");
    app.wait_for_background(Duration::from_secs(30), |app| {
        app.editor()
            .is_some_and(|editor| editor.file_path.as_ref() == Some(&hpp) && !editor.bg_loading)
    });
    app.editor_mut().unwrap().insert_char('H');
    app.execute_palette_command("switch_header_source");
    app.wait_for_background(Duration::from_secs(30), |app| {
        app.editor()
            .is_some_and(|editor| editor.file_path.as_ref() == Some(&cpp))
    });
    app.execute_palette_command("switch_header_source");
    assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&hpp));
    assert_eq!(app.editor().unwrap().rope.to_string(), "Horiginal\n");
    println!(
        "VTCODE_SSH_FILE_COMMANDS: raw filenames, Save/Save As/browse, undo/redo, header/source and dirty buffers passed"
    );
    shutdown(&workspace);
    drop(app);
    drop(workspace);
    drop(owner);
}

fn tool_line(workspace: &workspace::Workspace, program: &str, args: Vec<String>) -> String {
    use std::io::BufRead;
    let process = workspace
        .client
        .spawn(
            program.into(),
            args,
            RemotePath::default(),
            None,
            Default::default(),
        )
        .unwrap();
    let (reader, _writer, _killer) = process.split();
    let mut line = String::new();
    std::io::BufReader::new(reader)
        .read_line(&mut line)
        .unwrap();
    line.trim_end().into()
}

#[test]
#[ignore = "Real SSH: fresh disposable workspace and Python 3 on the node"]
fn windows_ssh_workspace_replace_keeps_local_undo_and_unrelated_saves_responsive() {
    use crate::{app::Tab, editor::Editor};
    let (workspace, owner, _) = connect();
    let created = tool_line(&workspace, "python3", vec!["-c".into(),
        "from pathlib import Path; import json; p=Path('ssh-replace'); p.mkdir(); (p/'open.txt').write_bytes(b'foo'); data=b'foo\\r\\n'+('unchanged λ\\n'*8192).encode(); [(p/f'closed-{i}.txt').write_bytes(data) for i in range(32)]; print(json.dumps({'files':32,'bytes':len(data)*32}))".into()]);
    let created: serde_json::Value = serde_json::from_str(&created).unwrap();
    let key = workspace.view_key(&RemotePath::new(b"ssh-replace/open.txt".to_vec()).unwrap());
    let mut app = test_app(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    open(&mut app, &key);
    let mut shared = Editor::new();
    shared.copy_document_from(app.editor().unwrap());
    app.new_desk("replacement", false);
    app.tabs.push(Tab::new_file(shared));
    app.active_tab = 0;
    app.show_explorer = false;
    app.global_search_query = "foo".into();
    app.global_search_replace_query = "bar".into();
    app.update_global_search();
    app.wait_for_background(Duration::from_secs(90), |app| !app.bg_global_search_running);
    assert_eq!(app.global_search_results.len(), 33);
    app.replace_all_in_files();
    app.wait_for_background(Duration::from_secs(30), |app| {
        app.editor()
            .is_some_and(|editor| editor.rope.to_string() == "bar")
    });
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    let mut samples = Vec::new();
    for _ in 0..128 {
        let started = Instant::now();
        let editor = app.editor_mut().unwrap();
        editor.selection_anchor = None;
        editor.set_cursor(0, 3);
        editor.move_left();
        editor.move_right();
        editor.selection_anchor = Some((0, 0));
        assert_eq!(editor.selected_text().unwrap(), "bar");
        editor.selection_anchor = None;
        editor.insert_char('!');
        editor.undo();
        editor.redo();
        editor.undo();
        assert_eq!(editor.rope.to_string(), "bar");
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        samples.push(started.elapsed().as_secs_f64() * 1000.);
        app.poll_background_results();
    }
    let saving = Instant::now();
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(30), |app| {
        app.editor().is_some_and(|editor| !editor.modified)
    });
    let save_ms = saving.elapsed().as_secs_f64() * 1000.;
    let save_during_batch = app.bg_global_replace_running;
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "foo");
    assert!(app.editor().unwrap().modified);
    app.editor_mut().unwrap().redo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "bar");
    assert!(!app.editor().unwrap().modified);
    app.wait_for_background(Duration::from_secs(120), |app| {
        !app.bg_global_replace_running && !app.bg_global_search_running
    });
    let verification = tool_line(&workspace, "python3", vec!["-c".into(),
        "from pathlib import Path; import json,hashlib; p=Path('ssh-replace'); expected=b'bar\\r\\n'+('unchanged λ\\n'*8192).encode(); files=list(p.glob('closed-*.txt')); assert len(files)==32; assert all(x.read_bytes()==expected for x in files); assert (p/'open.txt').read_bytes()==b'bar'; print(json.dumps({'verified':len(files),'sha256':hashlib.sha256(expected).hexdigest()}))".into()]);
    let verification: serde_json::Value = serde_json::from_str(&verification).unwrap();
    let arguments: Vec<String> = serde_json::from_slice(
        &std::fs::read(std::env::var_os("VTCODE_HEADLESS_SSH_ARGS").unwrap()).unwrap(),
    )
    .unwrap();
    if arguments
        .iter()
        .any(|argument| argument.contains("--disk-delay-ms"))
    {
        assert!(
            save_during_batch,
            "an unrelated save must not queue behind the closed-file batch"
        );
    }
    samples.sort_by(f64::total_cmp);
    println!(
        "VTCODE_SSH_REPLACE {}",
        json!({"closed_files":created["files"],"closed_bytes":created["bytes"],"verified":verification,
        "input_draw_p95_ms":samples[121],"input_draw_max_ms":samples[127],"save_ms":save_ms,"save_finished_during_batch":save_during_batch})
    );
    shutdown(&workspace);
    drop((app, workspace, owner));
}

#[test]
#[ignore = "Real SSH: fresh disposable workspace and Python 3 on the Linux node"]
fn windows_ssh_filtered_search_keeps_native_names_drafts_and_undo_responsive() {
    let (workspace, owner, _) = connect();
    let created = tool_line(&workspace, "python3", vec!["-c".into(),
        "from pathlib import Path; import json; p=Path('ssh-search'); p.mkdir(); f=p/'100%real:λ.txt'; f.write_bytes('disk needle 🦀\\r\\n'.encode()); print(json.dumps({'absolute':str(f.resolve())}))".into()]);
    let created: serde_json::Value = serde_json::from_str(&created).unwrap();
    let key = workspace.view_key(&RemotePath::new("ssh-search/100%real:λ.txt".as_bytes().to_vec()).unwrap());
    let mut app = test_app(workspace.view_root.clone());
    app.remote_workspace = Some(workspace.clone());
    open(&mut app, &key);
    app.editor_mut().unwrap().set_cursor(0, 0);
    app.editor_mut().unwrap().paste_plain_with_text("draft ");
    app.search_include_pattern = "100%real:λ.txt".into();
    app.global_search_query = "needle".into();
    let mut queries = 0;
    let search = |app: &mut crate::app::App| {
        app.update_global_search();
        app.wait_for_background(Duration::from_secs(90), |app| !app.bg_global_search_running);
        assert!(app.global_search_error.is_none(), "{:?}", app.global_search_error);
    };
    search(&mut app);
    queries += 1;
    assert_eq!(app.global_search_results.len(), 1);
    assert!(app.global_search_results[0].line_text.starts_with("draft "));
    app.search_include_pattern = created["absolute"].as_str().unwrap().into();
    search(&mut app);
    queries += 1;
    assert_eq!(app.global_search_results.len(), 1, "the Windows client must use Linux absolute path semantics");
    app.global_search_query = "draft".into();
    search(&mut app);
    queries += 1;
    assert_eq!(app.global_search_results.len(), 1);
    app.editor_mut().unwrap().undo();
    search(&mut app);
    queries += 1;
    assert!(app.global_search_results.is_empty(), "scope caching cannot retain undone content");
    app.editor_mut().unwrap().redo();
    app.search_include_pattern.clear();
    app.search_exclude_pattern = "100%real:λ.txt".into();
    search(&mut app);
    queries += 1;
    assert!(app.global_search_results.is_empty());
    app.search_exclude_pattern.clear();
    app.search_include_pattern = "100%real:λ.txt".into();
    app.search_regex = true;
    let mut setup = Vec::new();
    for index in 0..24 {
        app.global_search_query = if index % 2 == 0 { "(".into() } else {
            format!("(?:needle|{})", (0..400).map(|part| format!("name_{part}_{index}")).collect::<Vec<_>>().join("|"))
        };
        let started = Instant::now();
        app.update_global_search();
        setup.push(started.elapsed().as_secs_f64() * 1000.);
    }
    queries += 24;
    let expected = app.editor().unwrap().rope.to_string();
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let mut samples = Vec::new();
    for _ in 0..128 {
        let started = Instant::now();
        let editor = app.editor_mut().unwrap();
        editor.move_right();
        editor.move_left();
        editor.insert_char('λ');
        editor.undo();
        editor.redo();
        editor.undo();
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        samples.push(started.elapsed().as_secs_f64() * 1000.);
        app.poll_background_results();
    }
    app.wait_for_background(Duration::from_secs(90), |app| !app.bg_global_search_running);
    assert!(app.global_search_error.is_none(), "an obsolete invalid query cannot fail the final valid query");
    assert_eq!(app.global_search_results.len(), 1);
    assert_eq!(app.editor().unwrap().rope.to_string(), expected);
    assert!(app.editor().unwrap().modified);
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(90), |app| app.pending_file_saves.is_empty());
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().undo();
    assert!(app.editor().unwrap().modified);
    app.editor_mut().unwrap().redo();
    assert!(!app.editor().unwrap().modified);
    let verified = tool_line(&workspace, "python3", vec!["-c".into(),
        "from pathlib import Path; data=(Path('ssh-search')/'100%real:λ.txt').read_bytes(); assert data=='draft disk needle 🦀\\r\\n'.encode(), repr(data); print('verified')".into()]);
    assert_eq!(verified, "verified");
    samples.sort_by(f64::total_cmp);
    println!("VTCODE_SSH_SEARCH {}", json!({"queries":queries,
        "query_setup_max_ms":setup.iter().copied().fold(0., f64::max),
        "input_draw_p95_ms":samples[121],"input_draw_max_ms":samples[127],"saved_bytes_verified":true}));
    shutdown(&workspace);
    drop((app, workspace, owner));
}

#[test]
#[ignore = "Real SSH: fresh disposable Linux workspace; checks actual filesystem notifications"]
fn windows_ssh_external_changes_preserve_shared_edits_and_confirm_disk_state() {
    use crate::{app::{Tab, TabKind, SplitDirection}, editor::Editor};
    let (workspace, owner, _) = connect();
    tool_line(&workspace, "python3", vec!["-c".into(),
        "from pathlib import Path; p=Path('ssh-external'); p.mkdir(); [(p/n).write_bytes(t.encode()) for n,t in [('edit.txt','before\\n'),('case.rs','lower\\n'),('CASE.rs','UPPER\\n')]]; print('created')".into()]);
    let mut app = test_app(workspace.view_root.clone());
    workspace.client.set_wake(app.background.wake_handle());
    app.remote_workspace = Some(workspace.clone());
    app.bg_explorer_refresh_running = true;
    workspace.client.call(Command::Watch).unwrap();
    app.wait_for_background(Duration::from_secs(30), |app| app.workspace_watcher_active);
    let key = workspace.target_key("ssh-external/edit.txt").unwrap();
    open(&mut app, &key);
    let version = app.editor().unwrap().content_version;
    let history = app.editor().unwrap().undo_stack.len();
    let modified = app.editor().unwrap().last_disk_modified;
    app.editor_mut().unwrap().selection_anchor = Some((0, 0));
    tool_line(&workspace, "python3", vec!["-c".into(), "from pathlib import Path; import os; p=Path('ssh-external/edit.txt'); s=p.stat(); os.utime(p,ns=(s.st_atime_ns,s.st_mtime_ns+2000000000)); print('touched')".into()]);
    app.wait_for_background(Duration::from_secs(30), |app| app.editor().unwrap().last_disk_modified != modified && !app.background.has_active_jobs());
    assert_eq!(app.editor().unwrap().content_version, version);
    assert_eq!(app.editor().unwrap().undo_stack.len(), history);
    assert_eq!(app.editor().unwrap().selection_anchor, Some((0, 0)));
    app.editor_mut().unwrap().selection_anchor = None;
    tool_line(&workspace, "python3", vec!["-c".into(), "from pathlib import Path; import os; p=Path('ssh-external/edit.txt'); s=p.stat(); p.write_bytes(b'AFTER!\\n'); os.utime(p,ns=(s.st_atime_ns,s.st_mtime_ns)); print('replaced')".into()]);
    app.wait_for_background(Duration::from_secs(30), |app| app.editor().unwrap().rope == "AFTER!\n");
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, "before\n");
    assert!(app.editor().unwrap().modified);
    app.editor_mut().unwrap().redo();
    assert!(!app.editor().unwrap().modified);
    app.editor_mut().unwrap().set_cursor(0, 0);
    app.editor_mut().unwrap().insert_char('!');
    tool_line(&workspace, "python3", vec!["-c".into(), "from pathlib import Path; import os; p=Path('ssh-external/edit.txt'); s=p.stat(); p.write_bytes(b'SECOND\\n'); os.utime(p,ns=(s.st_atime_ns,s.st_mtime_ns)); print('changed while dirty')".into()]);
    app.wait_for_background(Duration::from_secs(30), |app| app.notification_history.iter().any(|entry| entry.0.contains("changed on disk")));
    assert_eq!(app.editor().unwrap().rope, "!AFTER!\n");
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(30), |app| app.pending_file_saves.is_empty());
    assert!(app.save_error.is_some());
    app.save_error = None;
    app.editor_mut().unwrap().undo();
    app.queue_file_reload(key.clone());
    app.wait_for_background(Duration::from_secs(30), |app| app.editor().unwrap().rope == "SECOND\n");
    // Replace the file with a directory, then restore it. A read error must not
    // generate an endless check/reload chain or prevent a later valid reload.
    tool_line(&workspace, "python3", vec!["-c".into(), "from pathlib import Path; p=Path('ssh-external/edit.txt'); p.unlink(); p.mkdir(); print('directory')".into()]);
    app.wait_for_background(Duration::from_secs(30), |app| app.notification_history.iter().any(|entry| entry.0.contains("Could not reload")) && !app.background.has_active_jobs());
    assert_eq!(app.editor().unwrap().rope, "SECOND\n");
    tool_line(&workspace, "python3", vec!["-c".into(), "from pathlib import Path; p=Path('ssh-external/edit.txt'); p.rmdir(); p.write_bytes(b'final\\n'); print('restored')".into()]);
    app.wait_for_background(Duration::from_secs(30), |app| app.editor().unwrap().rope == "final\n");
    tool_line(&workspace, "python3", vec!["-c".into(), "from pathlib import Path; p=Path('ssh-external/tree'); p.mkdir(); (p/'child.txt').write_bytes(b'before\\n'); print('tree created')".into()]);
    let child = workspace.target_key("ssh-external/tree/child.txt").unwrap();
    open(&mut app, &child);
    tool_line(&workspace, "python3", vec!["-c".into(), "from pathlib import Path; import os; p=Path('ssh-external/tree'); s=(p/'child.txt').stat(); p.rename(p.with_name('former-tree')); p.mkdir(); f=p/'child.txt'; f.write_bytes(b'AFTER!\\n'); os.utime(f,ns=(s.st_atime_ns,s.st_mtime_ns)); print('tree replaced')".into()]);
    app.wait_for_background(Duration::from_secs(30), |app| app.editor().unwrap().rope == "AFTER!\n");
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, "before\n");
    app.editor_mut().unwrap().redo();
    let upper = workspace.target_key("ssh-external/CASE.rs").unwrap();
    let lower = workspace.target_key("ssh-external/case.rs").unwrap();
    open(&mut app, &upper);
    open(&mut app, &lower);
    let mut shared = Editor::new();
    shared.copy_document_from(app.editor().unwrap());
    app.tabs.push(Tab::new_file(shared));
    app.create_split_group_dir(app.tabs.len()-1, SplitDirection::Horizontal);
    app.editor_mut().unwrap().insert_char('!');
    tool_line(&workspace, "python3", vec!["-c".into(), "from pathlib import Path; (Path('ssh-external')/'case.rs').unlink(); print('deleted lower case file')".into()]);
    app.wait_for_background(Duration::from_secs(30), |app| app.editor().is_some_and(|e| e.file_path.is_none()));
    let views = app.tabs.iter().chain(app.groups.iter().flat_map(|group| &group.tabs));
    assert_eq!(views.clone().filter(|tab| tab.kind == TabKind::File && tab.editor.file_path.is_none()).count(), 2);
    assert!(views.clone().any(|tab| tab.editor.file_path.as_ref() == Some(&upper)));
    app.editor_mut().unwrap().undo();
    for tab in app.tabs.iter_mut().chain(app.groups.iter_mut().flat_map(|group| &mut group.tabs)) {
        if tab.kind != TabKind::File { continue; }
        tab.editor.refresh_document();
        if tab.editor.file_path.is_none() { assert_eq!(tab.editor.rope, "lower\n"); assert!(tab.editor.modified); }
    }
    app.save_as_active = true;
    app.save_as_query = "ssh-external/recovered.txt".into();
    app.execute_save_as();
    let recovered = workspace.target_key("ssh-external/recovered.txt").unwrap();
    app.wait_for_background(Duration::from_secs(30), |app| app.editor().is_some_and(|e| e.file_path.as_ref()==Some(&recovered) && !e.modified) && app.pending_file_saves.is_empty());
    app.editor_mut().unwrap().redo();
    assert_eq!(app.editor().unwrap().rope, "!lower\n");
    app.save_current_file();
    app.wait_for_background(Duration::from_secs(30), |app| app.pending_file_saves.is_empty());
    assert!(app.save_error.is_none());
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let mut samples = Vec::new();
    for _ in 0..128 {
        let started = Instant::now();
        let editor = app.editor_mut().unwrap();
        editor.move_right(); editor.move_left(); editor.insert_char('λ'); editor.undo(); editor.redo(); editor.undo();
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        samples.push(started.elapsed().as_secs_f64()*1000.);
        app.poll_background_results();
    }
    assert_eq!(app.editor().unwrap().rope, "!lower\n");
    assert!(!app.editor().unwrap().modified);
    let verified = tool_line(&workspace, "python3", vec!["-c".into(), "from pathlib import Path; p=Path('ssh-external'); assert not (p/'case.rs').exists(); assert (p/'CASE.rs').read_bytes()==b'UPPER\\n'; assert (p/'recovered.txt').read_bytes()==b'!lower\\n'; assert (p/'edit.txt').read_bytes()==b'final\\n'; print('verified')".into()]);
    assert_eq!(verified,"verified");
    samples.sort_by(f64::total_cmp);
    println!("VTCODE_SSH_EXTERNAL {}", json!({"watcher":true,"preserved_metadata":true,"dirty_save_conflict":true,
        "case_sensitive_delete":true,"shared_recovery_save_as":true,"parent_replacement":true,"input_draw_p95_ms":samples[121],"input_draw_max_ms":samples[127]}));
    shutdown(&workspace);
    drop((app,workspace,owner));
}

#[test]
#[ignore = "Real SSH and >64 MiB: VTCODE_HEADLESS_SSH_ARGS must launch a disposable workspace"]
fn windows_ssh_large_unicode_crlf_file_has_no_transfer_size_cap() {
    use sha2::{Digest, Sha256};
    let (workspace, owner, _) = connect();
    let chunk = "\"quoted\"\\path\t🦀 café\r\n".repeat(2048);
    let blocks = (64 * 1024 * 1024) / chunk.len() + 3;
    let script = format!(
        "import json; block=json.loads({:?}).encode('utf-8'); f=open('ssh-large.txt','wb'); \
         [f.write(block) for _ in range({blocks})]; f.close(); print('created',flush=True)",
        serde_json::to_string(&chunk).unwrap()
    );
    assert_eq!(
        tool_line(&workspace, "python3", vec!["-c".into(), script]),
        "created"
    );
    let key = workspace.target_key("ssh-large.txt").unwrap();
    let opening = Instant::now();
    let mut progress = (0, 0);
    let loaded = workspace
        .load_progress(&key, |done, total| {
            assert!(done >= progress.0 && done <= total);
            progress = (done, total);
        })
        .unwrap();
    let open_ms = opening.elapsed().as_secs_f64() * 1000.;
    assert!(loaded.rope.len_bytes() > 64 * 1024 * 1024);
    assert_eq!(
        progress,
        (
            loaded.rope.len_bytes() as u64,
            loaded.rope.len_bytes() as u64
        )
    );
    assert_eq!(
        loaded.rope.slice(..chunk.chars().count()).to_string(),
        chunk
    );
    let mut builder = ropey::RopeBuilder::new();
    let replacement = "\"changed\"\\file\t文 🦀\r\n".repeat(2048);
    for _ in 0..blocks + 100 {
        builder.append(&replacement);
    }
    let replacement = builder.finish();
    assert!(replacement.len_bytes() > 64 * 1024 * 1024);
    let checkpoint = loaded.remote_checkpoint.clone().unwrap();
    let request = crate::editor::BackgroundSaveRequest {
        prepared_options: None,
        source_document_id: 1,
        path: key,
        rope: replacement.clone(),
        encoding: loaded.encoding,
        line_ending: "CRLF",
        trim_trailing: false,
        insert_final_newline: false,
        source_content_version: 1,
        fail_if_exists: false,
        expected_disk: Some(crate::editor::DiskSaveBaseline {
            rope: loaded.rope.clone(),
            encoding: loaded.encoding,
        }),
    };
    let saving = Instant::now();
    progress = (0, 0);
    let saved = workspace
        .save_progress(
            &request,
            Some((checkpoint, loaded.rope, loaded.encoding)),
            None,
            |done, total| {
                assert!(done >= progress.0 && done <= total);
                progress = (done, total);
            },
        )
        .unwrap();
    let save_ms = saving.elapsed().as_secs_f64() * 1000.;
    assert_eq!(saved.rope, replacement);
    // The unchanged quote/CRLF prefix and suffix need not be transferred.
    assert!(progress.0 > 64 * 1024 * 1024);
    assert_eq!(progress.0, progress.1);
    let mut expected = Sha256::new();
    for chunk in replacement.chunks() {
        expected.update(chunk.as_bytes());
    }
    let expected = format!("{:x}", expected.finalize());
    let disk = tool_line(&workspace, "sha256sum", vec!["ssh-large.txt".into()]);
    assert!(
        disk.starts_with(&expected),
        "Node disk SHA-256 must match every saved byte: {disk}"
    );
    println!(
        "VTCODE_SSH_LARGE {}",
        json!({"bytes":replacement.len_bytes(),"open_ms":open_ms,"save_ms":save_ms,"sha256":expected})
    );
    shutdown(&workspace);
    drop(workspace);
    drop(owner);
}

#[test]
#[ignore = "Real SSH: argument JSON must end with the command launching a disposable workspace"]
fn windows_ssh_disconnect_stops_owned_background_processes() {
    use std::io::BufRead;
    let (workspace, owner, _) = connect();
    let mut owned = Vec::new();
    let mut pids = Vec::new();
    for terminal in [None, Some((80, 24))] {
        let process = workspace
            .client
            .spawn(
                "/bin/sh".into(),
                vec![
                    "-c".into(),
                    "sleep 120 & child=$!; printf '%s %s\\n' \"$$\" \"$child\"; wait".into(),
                ],
                RemotePath::default(),
                terminal,
                Default::default(),
            )
            .unwrap();
        let (reader, writer, killer) = process.split();
        let mut reader = std::io::BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let children: Vec<u32> = line
            .split_whitespace()
            .map(|s| s.parse().unwrap())
            .collect();
        assert_eq!(children.len(), 2);
        pids.extend(children);
        owned.push((reader, writer, killer));
    }
    let formatting_workspace = workspace.clone();
    let formatter = std::thread::spawn(move || {
        formatting_workspace.format_document(&formatting_workspace.target_key("remote.rs").unwrap(),
            crate::formatters::Config { provider: crate::formatters::Provider::Command,
                mode: crate::formatters::CommandMode::Stdin, command: "/bin/sh".into(),
                args: vec!["-c".into(), "sleep 120 & child=$!; printf '%s %s\\n' \"$$\" \"$child\" > ssh-formatter-pids; wait".into()] },
            &ropey::Rope::from_str("source\n"), None, &crate::background::CancellationToken::new())
    });
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(loaded) = workspace.load(&workspace.target_key("ssh-formatter-pids").unwrap()) {
            let text = loaded.rope.to_string();
            let children: Vec<u32> = text
                .split_whitespace()
                .map(|s| s.parse().unwrap())
                .collect();
            if children.len() == 2 {
                pids.extend(children);
                break;
            }
        }
        assert!(Instant::now() < deadline, "Formatter did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(pids.iter().all(|pid| *pid > 1));
    // Keep the tool's handles alive: killing SSH must clean up on the node
    // without a later tool-close message or a cooperative Shutdown request.
    drop(owner);
    let code = format!(
        "from pathlib import Path\nimport sys,time\npids={pids:?}\n\
        def live(pid):\n try:\n  return Path('/proc/%s/stat'%pid).read_text().rsplit(')',1)[1].split()[0]!='Z'\n except FileNotFoundError:\n  return False\n\
        end=time.monotonic()+5\nwhile any(map(live,pids)) and time.monotonic()<end: time.sleep(.05)\n\
        alive=list(filter(live,pids))\nprint('Remaining owned process IDs:',alive)\nsys.exit(bool(alive))"
    );
    let check = format!("python3 -c '{}'", code.replace('\'', "'\\''"));
    let arguments = std::env::var_os("VTCODE_HEADLESS_SSH_ARGS").unwrap();
    let ssh: Vec<String> = serde_json::from_slice(&std::fs::read(arguments).unwrap()).unwrap();
    let mut command = std::process::Command::new(&ssh[0]);
    use std::os::windows::process::CommandExt;
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    let result = command
        .args(&ssh[1..ssh.len() - 1])
        .arg(check)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "Disconnect orphaned an owned process: {} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        formatter.join().unwrap().is_err(),
        "Disconnect interrupts the formatter caller"
    );
    drop((owned, workspace));
}
