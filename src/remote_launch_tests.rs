use super::*;
use ratatui::{Terminal, backend::TestBackend};

#[derive(Debug)]
struct NoChild;

impl portable_pty::ChildKiller for NoChild {
    fn kill(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
        Box::new(Self)
    }
}

/// A burst queued before the first poll must drain without requiring a new
/// keystroke to wake the connection screen. Esc is deliberately past the input
/// budget; the watchdog only makes a regression fail instead of hanging CI.
#[test]
fn queued_login_input_does_not_wait_for_another_notification() {
    let wake = UiWake::default();
    let pair = portable_pty::native_pty_system()
        .openpty(portable_pty::PtySize::default())
        .unwrap();
    let (_output, incoming) = mpsc::sync_channel(32);
    let (_ready, endpoint) = mpsc::sync_channel(1);
    let (keys, _outgoing) = mpsc::sync_channel(32);
    let setup = Setup {
        owned: Arc::new(Process {
            killer: Mutex::new(Box::new(NoChild)),
            master: Mutex::new(pair.master),
            trace: super::super::diagnostics::Log::default().begin("test transport".as_ref()),
            termination: Arc::new(AtomicBool::new(false)),
            #[cfg(windows)]
            job: None,
        }),
        incoming,
        endpoint,
        keys,
        wake: wake.clone(),
        identity: Vec::new(),
        recipe: None,
    };
    let (sender, receiver) = mpsc::channel();
    for _ in 0..64 {
        sender.send(Ok(Event::FocusGained)).unwrap();
    }
    sender
        .send(Ok(Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Esc,
            KeyModifiers::NONE,
        ))))
        .unwrap();
    let (done, finished) = mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        let expired = finished
            .recv_timeout(std::time::Duration::from_secs(3))
            .is_err();
        if expired {
            wake.notify();
        }
        expired
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut events = TerminalEventPump::from_receiver(receiver);
    let result = setup.wait(&mut terminal, &mut events);
    let _ = done.send(());
    assert!(
        !watchdog.join().unwrap(),
        "Queued input required a new wake"
    );
    assert!(result.unwrap().is_none());
}

fn screen(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn connection_screen_explains_waiting_and_preserves_authentication_prompts() {
    let mut terminal = Terminal::new(TestBackend::new(100, 12)).unwrap();
    let mut grid = crate::terminal::TermGrid::new(100, 10);
    terminal
        .draw(|frame| draw_setup(frame, &grid, false))
        .unwrap();
    let waiting = screen(&terminal);
    assert!(waiting.contains("Waiting for connection output"));
    assert!(waiting.contains("no second terminal is needed"));
    assert!(!waiting.contains("On the allocated node"));

    let mut parser = vte::Parser::new();
    for &byte in b"Enter passphrase for key: " {
        parser.advance(&mut grid, byte);
    }
    terminal
        .draw(|frame| draw_setup(frame, &grid, true))
        .unwrap();
    let prompt = screen(&terminal);
    assert!(prompt.contains("Enter passphrase for key:"));
    assert!(!prompt.contains("Waiting for connection output"));
    assert!(prompt.contains("Use this window for SSH authentication"));
}

#[test]
fn connection_screen_fits_small_terminals() {
    for width in [0, 1, 20, 80] {
        for height in [0, 1, 2, 3, 8] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let grid = crate::terminal::TermGrid::new(width.max(2) as usize, 2);
            terminal
                .draw(|frame| draw_setup(frame, &grid, false))
                .unwrap();
        }
    }
}

#[test]
fn reconnect_launch_failures_back_off_stop_retry_and_never_restart_after_quit() {
    use crate::app_remote_connection::Phase;
    let root = tempfile::tempdir().unwrap();
    let mut remote = workspace::Workspace::new(super::super::tests::connection(
        root.path(),
        Arc::default(),
        Arc::default(),
    ));
    remote.recipe = Some(Arc::new(Recipe {
        launch: Launch::new(
            root.path()
                .join("nonexistent-vtcode-test-command")
                .into_os_string(),
            Vec::new(),
        ),
        cwd: root.path().to_owned(),
        diagnostics: Default::default(),
    }));
    let mut app = crate::app::App::new_deferred(remote.view_root.clone());
    app.remote_workspace = Some(remote.clone());
    app.source_control_enabled = false;
    app.new_file();
    app.editor_mut().unwrap().insert_char('x');
    remote.client.call(Command::Shutdown).unwrap();
    // The in-process TCP fixture retains its test reader after Shutdown;
    // explicitly deliver the transport failure that a real SSH exit produces.
    remote.client.disconnect();
    app.wait_for_background(std::time::Duration::from_secs(10), |app| {
        app.remote_connection.phase == Phase::Waiting && app.remote_connection.attempt == 1
    });
    assert!(app.side_notice.visible());
    assert!(app.remote_connection.next_wait().unwrap() > std::time::Duration::ZERO);
    assert_eq!(app.editor().unwrap().rope, "x");
    app.stop_remote_reconnect();
    let stopped_attempt = app.remote_connection.attempt;
    for _ in 0..8 {
        app.poll_background_results();
    }
    assert!(app.remote_connection.phase == Phase::Stopped);
    assert_eq!(app.remote_connection.attempt, stopped_attempt);
    assert!(app.remote_connection.next_wait().is_none());
    app.retry_remote_connection();
    app.wait_for_background(std::time::Duration::from_secs(10), |app| {
        app.remote_connection.phase == Phase::Waiting && app.remote_connection.attempt == 2
    });
    app.exit_keep_unsaved_quit = true;
    app.poll_background_results();
    assert!(app.remote_connection.phase == Phase::Stopped);
    assert!(app.remote_connection.next_wait().is_none());
    assert_eq!(app.editor().unwrap().rope, "x");
}

#[test]
fn connection_diagnostics_explain_initial_transport_failure_and_preserve_exit_status() {
    #[cfg(windows)]
    let launch = Launch::new("cmd.exe".into(), vec!["/d".into(), "/c".into(), "echo ssh: diagnostic fixture connection refused 1>&2 & exit /b 42".into()]);
    #[cfg(not(windows))]
    let launch = Launch::new("sh".into(), vec!["-c".into(), "printf 'ssh: diagnostic fixture connection refused\\n' >&2; exit 42".into()]);
    let setup = launch.start(ratatui::layout::Size::new(100, 24), UiWake::default()).unwrap();
    let log = setup.owned.trace.log.clone();
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    let (_sender, receiver) = mpsc::channel();
    let mut events = TerminalEventPump::from_receiver(receiver);
    let error = match setup.wait(&mut terminal, &mut events) { Err(error) => error.to_string(), Ok(_) => panic!("Failed transport started an editor") };
    assert!(error.contains("diagnostic fixture connection refused"), "{error}");
    assert!(error.contains("Connection diagnostics started"), "{error}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let report = log.report();
        if report.contains("code 42") { break; }
        assert!(std::time::Instant::now() < deadline, "Exit status was lost: {report}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn simplified_remote_cli_preserves_quoted_commands_through_the_transport_and_relaunch() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("SSH launch λ🦀 with spaces");
    std::fs::create_dir(&root).unwrap();
    let executable = std::env::current_exe().unwrap();
    let prefix = shell_words::join([
        executable.to_str().unwrap(), "--ignored", "--exact",
        "remote::launch::tests::ssh_command_prefix_fixture", "--nocapture", "--",
    ]);
    for command in [None, Some("submitcmd --queue short '/opt/VT Code/vtcode' --headless '/project λ🦀' ; literal $HOME")] {
        let mut args = vec!["--remote".into(), "test-alias".into(), "--ssh-command".into(), prefix.clone().into()];
        if let Some(command) = command { args.extend(["--remote-command".into(), command.into()]); }
        let setup = Launch::parse(args).unwrap().unwrap()
            .start_in(ratatui::layout::Size::new(100, 24), UiWake::default(), &root).unwrap();
        let recipe = setup.recipe.clone().unwrap();
        let (client, owner) = setup.endpoint_for_test().unwrap();
        assert!(matches!(client.call(Command::Ping).unwrap(), Reply::Done));
        let delivered: Vec<String> = serde_json::from_slice(&std::fs::read(root.join("ssh-argv.json")).unwrap()).unwrap();
        assert_eq!(delivered, ["test-alias", command.unwrap_or("vtcode --headless")]);
        client.call(Command::Shutdown).unwrap();
        owner.terminate();
        let (recovered, next_owner) = recipe.start(ratatui::layout::Size::new(100, 24), UiWake::default()).unwrap().endpoint_for_test().unwrap();
        let delivered_again: Vec<String> = serde_json::from_slice(&std::fs::read(root.join("ssh-argv.json")).unwrap()).unwrap();
        assert_eq!(delivered_again, delivered);
        assert!(matches!(recovered.call(Command::Ping).unwrap(), Reply::Done));
        recovered.call(Command::Shutdown).unwrap();
        next_owner.terminate();
    }
}

#[test]
#[ignore = "Internal SSH command argv fixture, launched only by the CLI transport test"]
fn ssh_command_prefix_fixture() {
    let args: Vec<_> = std::env::args().collect();
    let delivered = &args[args.len() - 2..];
    let root = std::env::current_dir().unwrap();
    std::fs::write(root.join("ssh-argv.json"), serde_json::to_vec(delivered).unwrap()).unwrap();
    super::super::service::run(&root).unwrap();
}
