use super::*;
use crate::{
    app::{ActivePane, Tab, TerminalTab},
    editor::Editor,
    terminal::TerminalEmulator,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};

#[path = "app_workspaces_edge_tests.rs"]
mod edge_tests;

#[path = "app_workspaces_race_tests.rs"]
mod race_tests;

#[path = "app_workspace_sets_tests.rs"]
mod sets_tests;
#[path = "app_open_folder_tests.rs"]
mod open_folder_tests;
#[path = "app_workspaces_idle_tests.rs"]
mod idle_tests;
#[path = "app_workspaces_audit_tests.rs"]
mod audit_tests;
#[path = "app_workspaces_warm_tests.rs"]
mod warm_tests;

#[path = "app_workspace_manager_tests.rs"]
mod manager_tests;

struct Project(tempfile::TempDir);
impl Project {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }
    fn path(&self) -> &Path {
        self.0.path()
    }
    fn app(&self, wake: crate::background::UiWake) -> App {
        let mut app = App::new_startup(self.path().into(), wake);
        app.source_control_enabled = false;
        app.lsp_manager.configs.clear();
        app.tabs = vec![Tab::new_file(Editor::test_with_text("original"))];
        app.active_tab = 0;
        app.active_pane = ActivePane::Editor;
        app
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let state = crate::portable::workspace_dir(self.path());
        assert!(state.starts_with(crate::portable::data_dir().join("workspaces")));
        let _ = std::fs::remove_dir_all(state);
    }
}

fn until(mut ready: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(15);
    while !ready() {
        assert!(Instant::now() < end, "workspace operation stalled");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn services(app: &mut App) {
    for service in [
        Service::Workers,
        Service::Terminals,
        Service::Agents,
        Service::LanguageServers,
        Service::Extensions,
    ] {
        app.service_workspaces_until(service, Instant::now() + Duration::from_millis(20), || true);
    }
}

// Exercise the real framed headless service over stdio-like byte streams.
// No listener, SSH daemon, credentials or network access is needed.
fn headless_connection(root: &Path) -> crate::remote::client::Client {
    use std::io::{Read, Write};
    struct Input {
        receive: mpsc::Receiver<Vec<u8>>,
        buffer: io::Cursor<Vec<u8>>,
    }
    impl Read for Input {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if out.is_empty() {
                return Ok(0);
            }
            while self.buffer.position() as usize == self.buffer.get_ref().len() {
                match self.receive.recv() {
                    Ok(bytes) => self.buffer = io::Cursor::new(bytes),
                    Err(_) => return Ok(0),
                }
            }
            self.buffer.read(out)
        }
    }
    struct Output(mpsc::Sender<Vec<u8>>);
    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.send(bytes.to_vec()).map_err(io::Error::other)?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let (to_server, server_receive) = mpsc::channel();
    let (to_client, client_receive) = mpsc::channel();
    let root = root.to_path_buf();
    std::thread::spawn(move || {
        let _ = crate::remote::service::serve(
            &root,
            Input {
                receive: server_receive,
                buffer: io::Cursor::new(vec![]),
            },
            Output(to_client),
        );
    });
    crate::remote::client::Client::connect(
        Input {
            receive: client_receive,
            buffer: io::Cursor::new(vec![]),
        },
        Output(to_server),
    )
    .unwrap()
}

fn draw(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn switching_preserves_documents_undo_desks_chat_and_terminal_identity() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.editor_mut().unwrap().insert_char('!');
    let document = app.editor().unwrap().save_identity;
    let desk = app.new_desk("Review", true);
    app.chat.input = "unfinished prompt".into();
    let mut terminal = TerminalTab::new("Build".into(), a.path().into());
    terminal.terminal_pty = Some(TerminalEmulator::test_with_output(40, 8, "before switch"));
    let keep_running = terminal.terminal_pty.as_mut().unwrap().test_keep_running();
    app.terminals = vec![terminal];
    app.insert_workspace(b.app(wake));
    assert_eq!(app.explorer.root, b.path());
    assert_eq!(app.workspaces.parked.len(), 1);
    assert!(!app.workspaces.parked[0].app.workspace_visible);
    assert!(app.workspaces.parked[0].app.workspaces.parked.is_empty());
    assert!(app.switch_workspace(0));
    assert_eq!(app.explorer.root, a.path());
    assert_eq!(app.desks.active, desk);
    assert_eq!(app.chat.input, "unfinished prompt");
    assert_eq!(app.editor().unwrap().save_identity, document);
    assert!(app.editor().unwrap().rope.to_string().contains('!'));
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope.to_string(), "original");
    assert!(app.terminals[0].terminal_pty.is_some());
    assert!(app.workspace_visible && app.terminal_has_focus);
    assert!(!app.switch_workspace(0));
    assert!(!app.switch_workspace(999));
    drop(keep_running);
}

#[test]
fn parked_terminal_output_is_drained_and_completion_is_acknowledged_only_on_visit() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    let mut pty = TerminalEmulator::test_with_output(50, 8, "");
    let _keep_running = pty.test_keep_running();
    pty.test_queue_output(b"\x1b]633;C\x07building");
    app.terminals = vec![TerminalTab::new("Build".into(), a.path().into())];
    app.terminals[0].terminal_pty = Some(pty);
    app.process_terminals();
    app.insert_workspace(b.app(wake));
    app.workspaces.parked[0].app.terminals[0]
        .terminal_pty
        .as_ref()
        .unwrap()
        .test_queue_output(b"\r\ndone\x1b]633;D;0\x07");
    assert!(app.service_workspaces_until(
        Service::Terminals,
        Instant::now() + Duration::from_millis(50),
        || true
    ));
    assert_eq!(app.workspace_unread(), 1);
    let entry = app
        .workspace_entries()
        .into_iter()
        .find(|entry| entry.id == 0)
        .unwrap();
    assert!(entry.last.contains("Build: command finished"));
    assert!(!entry.attention);
    for _ in 0..3 {
        services(&mut app);
    }
    assert_eq!(
        app.workspace_unread(),
        1,
        "quiet polls must not duplicate a completion"
    );
    app.switch_workspace(0);
    assert_eq!(app.workspace_activity.unread, 0);
    assert!(app.status_message.contains("Build: command finished"));
    assert!(
        app.terminals[0].completion.unread,
        "the terminal keeps its own unread state until focused"
    );
}

#[test]
fn background_worker_replies_and_agent_notifications_stay_with_their_workspace() {
    use vtcode::acp::{Event, Session, protocol as p};
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    let id = p::SessionId::new("work");
    let mut session = Session::new(id.clone());
    session.begin_prompt("build".into());
    app.chat.sessions.insert(id.clone(), session);
    let mut second = b.app(wake);
    let mut same_id = Session::new(id.clone());
    same_id.begin_prompt("independent task".into());
    second.chat.sessions.insert(id.clone(), same_id);
    app.insert_workspace(second);
    let parked = &mut app.workspaces.parked[0].app;
    parked
        .background
        .sender
        .send(crate::background::BackgroundResult::GitBranch(
            "background-branch".into(),
        ))
        .unwrap();
    parked.apply_acp_event(Event::PromptFinished {
        session_id: id.clone(),
        response: p::PromptResponse::new(p::StopReason::EndTurn),
    });
    assert!(app.workspaces_have_pending_results());
    services(&mut app);
    assert_ne!(app.cached_git_branch, "background-branch");
    let parked = &app.workspaces.parked[0].app;
    assert_eq!(parked.cached_git_branch, "background-branch");
    assert!(!parked.chat.sessions[&id].busy);
    assert_eq!(app.workspace_unread(), 1);
    assert!(
        app.chat.sessions[&id].busy,
        "the visible workspace's session with the same id keeps running"
    );
    let active_history = app.notification_history.len();
    app.switch_workspace(0);
    assert_eq!(app.workspace_activity.unread, 0);
    assert!(app.notification_history.len() > active_history);
}

#[test]
fn opening_is_async_deduplicates_roots_and_keeps_current_workspace_on_failure() {
    let a = Project::new();
    let b = Project::new();
    let mut app = a.app(Default::default());
    let document = app.editor().unwrap().save_identity;
    app.workspaces.request = Some(Request::Open(b.path().into()));
    app.poll_workspace_requests();
    assert!(app.workspace_opening());
    until(|| {
        services(&mut app);
        app.poll_workspace_requests();
        !app.workspace_opening()
    });
    assert_eq!(app.explorer.root, b.path());
    assert_eq!(app.workspaces.parked.len(), 1);
    app.workspaces.request = Some(Request::Open(a.path().join(".")));
    app.poll_workspace_requests();
    app.poll_workspace_requests();
    assert_eq!(app.workspaces.active, 0);
    assert_eq!(app.editor().unwrap().save_identity, document);
    assert_eq!(app.workspaces.parked.len(), 1);
    app.workspaces.request = Some(Request::Open(a.path().join("missing")));
    app.poll_workspace_requests();
    until(|| {
        services(&mut app);
        app.poll_workspace_requests();
        !app.workspace_opening()
    });
    assert_eq!(app.workspaces.active, 0);
    assert_eq!(app.editor().unwrap().save_identity, document);
    assert!(
        app.notification_history
            .last()
            .unwrap()
            .0
            .contains("Could not open folder")
    );
}

#[test]
fn title_picker_mouse_keyboard_and_small_viewports_use_matching_geometry() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.insert_workspace(b.app(wake));
    app.workspaces.parked[0]
        .app
        .record_workspace_event("Build finished", Level::Done);
    for width in [48, 80, 100, 140, 200] {
        let buffer = draw(&mut app, width, 30);
        let picker =
            crate::ui_chrome::workspace_button_area(&app).expect("workspace control fits");
        assert!(picker.right() < width - 3);
        let title: String = (0..width).map(|x| buffer[(x, picker.y)].symbol()).collect();
        assert!(!title.contains('+'), "the title bar only offers the workspace picker");
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: picker.x,
                row: picker.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(app.workspaces.popup.open);
        crate::ui_workspaces::handle_key(&mut app, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        let popup = draw(&mut app, width, 30);
        let text: String = popup.content.iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("Open folders"));
        assert!(text.contains("Build finished"));
        crate::ui_control_visibility_tests::snapshot(&buffer, &format!("Workspaces title {width}"));
        crate::ui_control_visibility_tests::snapshot(&popup, &format!("Workspaces picker {width}"));
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );
        assert!(!app.workspaces.popup.open);
    }
    for (width, height) in [(1, 1), (8, 3), (20, 8), (40, 9)] {
        crate::ui_workspaces::open(&mut app);
        draw(&mut app, width, height);
        app.workspaces.popup = Default::default();
    }
    crate::ui_workspaces::open(&mut app);
    crate::ui_workspaces::handle_key(&mut app, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    crate::ui_workspaces::handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.poll_workspace_requests());
    assert_eq!(app.workspaces.active, 0);
    app.explorer.root = PathBuf::from("工程👩‍💻長いワークスペース名");
    for width in [48, 67, 83, 140] {
        let buffer = draw(&mut app, width, 25);
        let picker = crate::ui_chrome::workspace_button_area(&app).unwrap();
        let label: String = (picker.x..picker.right())
            .map(|x| buffer[(x, picker.y)].symbol()).collect();
        assert!(label.contains('▾'), "wide names must preserve the workspace dropdown");
        assert_eq!(buffer[(width - 2, 0)].symbol(), "×");
    }
}

#[test]
fn remote_workspaces_share_a_headless_connection_with_independent_buffers_and_tools() {
    use crate::remote::{self, RemotePath};
    let a = Project::new();
    let b = Project::new();
    std::fs::write(a.path().join("same.txt"), "workspace A").unwrap();
    std::fs::write(b.path().join("same.txt"), "workspace B").unwrap();
    let client = headless_connection(a.path());
    let first = remote::workspace::Workspace::new(client);
    let target = first.view_key(&remote::git::from_native(b.path(), a.path()).unwrap());
    let wake = crate::background::UiWake::default();
    first.client.set_wake(wake.clone());
    let mut app = App::new_startup(first.view_root.clone(), wake);
    app.remote_workspace = Some(first.clone());
    app.lsp_manager.remote_workspace = Some(first.clone());
    app.source_control_enabled = false;
    let relative = RemotePath::new(b"same.txt".to_vec()).unwrap();
    let a_key = first.view_key(&relative);
    app.open_file(&a_key);
    until(|| {
        services(&mut app);
        app.editor().is_some_and(|editor| !editor.bg_loading)
    });
    assert_eq!(app.editor().unwrap().rope.to_string(), "workspace A");
    app.editor_mut().unwrap().insert_char('!');
    let identity = app.editor().unwrap().save_identity;
    let process = first
        .client
        .spawn(
            std::env::current_exe()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            vec![
                "--exact".into(),
                "app_workspaces::tests::workspace_job_fixture".into(),
                "--ignored".into(),
                "--nocapture".into(),
            ],
            RemotePath::default(),
            None,
            app.background.wake_handle(),
        )
        .unwrap();
    app.terminals = vec![TerminalTab::new(
        "Remote build".into(),
        first.view_root.clone(),
    )];
    app.terminals[0].terminal_pty =
        Some(TerminalEmulator::from_remote(process, 80, 10, app.background.wake_handle()).unwrap());
    until(|| {
        services(&mut app);
        app.terminals[0]
            .terminal_pty
            .as_ref()
            .unwrap()
            .grid
            .commands
            .running()
    });
    app.workspaces.request = Some(Request::Open(target));
    app.poll_workspace_requests();
    until(|| {
        services(&mut app);
        app.poll_workspace_requests();
        !app.workspace_opening()
    });
    let second = app.remote_workspace.as_ref().unwrap().clone();
    assert_ne!(first.view_root, second.view_root);
    assert_eq!(first.client.hello().session, second.client.hello().session);
    let b_key = second.view_key(&relative);
    app.open_file(&b_key);
    until(|| {
        services(&mut app);
        app.editor().is_some_and(|editor| !editor.bg_loading)
    });
    assert_eq!(app.editor().unwrap().rope.to_string(), "workspace B");
    assert_eq!(first.load(&a_key).unwrap().rope.to_string(), "workspace A");
    let running = &app.workspaces.parked[0].app.terminals[0].terminal_pty;
    assert!(running.as_ref().unwrap().grid.commands.running());
    running.as_ref().unwrap().write(b"x");
    until(|| {
        services(&mut app);
        app.workspace_unread() > 0
    });
    assert!(
        app.workspaces.parked[0]
            .app
            .workspace_activity
            .last
            .contains("Remote build: command finished")
    );
    app.switch_workspace(0);
    assert_eq!(app.editor().unwrap().save_identity, identity);
    assert!(app.editor().unwrap().rope.to_string().contains('!'));
    assert!(first.client.connected() && second.client.connected());
    first.disconnect();
}

#[test]
#[ignore = "subprocess fixture for a live remote job across workspace switches"]
fn workspace_job_fixture() {
    use std::io::{Read, Write};
    let mut output = std::io::stdout().lock();
    output
        .write_all(b"\r\n\x1b]633;C\x07WORKSPACE-JOB-READY\r\n")
        .unwrap();
    output.flush().unwrap();
    let mut input = std::io::stdin().lock();
    input.read_exact(&mut [0]).unwrap();
    output
        .write_all(b"\r\nWORKSPACE-JOB-DONE\x1b]633;D;0\x07\r\n")
        .unwrap();
    output.flush().unwrap();
    // Retain the terminal after completion, just like a shell at its prompt.
    let _ = input.read_exact(&mut [0]);
}

#[test]
fn close_preserves_unsaved_session_and_removes_only_its_own_workbench() {
    let a = Project::new();
    let b = Project::new();
    let wake = crate::background::UiWake::default();
    let mut app = a.app(wake.clone());
    app.insert_workspace(b.app(wake));
    app.editor_mut().unwrap().insert_char('!');
    let text = app.editor().unwrap().rope.to_string();
    app.pending_file_saves
        .insert(b.path().join("pending.txt"), (0, 1.into()));
    app.workspaces.request = Some(Request::Close);
    app.poll_workspace_requests();
    assert!(app.workspaces.exit.is_none());
    assert_eq!(app.workspaces.parked.len(), 1);
    app.pending_file_saves.clear();
    app.workspaces.request = Some(Request::Close);
    app.poll_workspace_requests();
    assert!(app.exit_keep_unsaved_quit);
    app.start_exit_recovery();
    until(|| {
        services(&mut app);
        app.exit_recovery.saved
    });
    app.shutdown_anim_active = true;
    assert!(app.advance_workspace_exit());
    assert_eq!(app.explorer.root, a.path());
    assert!(app.workspaces.parked.is_empty());
    assert!(!app.shutdown_anim_active);
    let mut session = crate::session::load_session(b.path()).unwrap();
    crate::session::hydrate_saved_tab(b.path(), &mut session.tabs[0]);
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
fn quit_choice_is_applied_to_all_workspaces_by_the_real_event_loop() {
    use crossterm::event::Event;
    for choice in ['y', 'd', 'k'] {
        let a = Project::new();
        let b = Project::new();
        let wake = crate::background::UiWake::default();
        let mut app = a.app(wake.clone());
        for project in [&a, &b] {
            std::fs::write(project.path().join("file.txt"), "saved").unwrap();
        }
        app.tabs[0]
            .editor
            .open_file(&a.path().join("file.txt"))
            .unwrap();
        app.editor_mut().unwrap().insert_char('!');
        let mut second = b.app(wake);
        second.tabs[0]
            .editor
            .open_file(&b.path().join("file.txt"))
            .unwrap();
        second.editor_mut().unwrap().insert_char('?');
        app.insert_workspace(second);
        let (sender, receiver) = std::sync::mpsc::channel();
        for key in [
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::Char(choice), KeyModifiers::NONE),
        ] {
            sender.send(Ok(Event::Key(key))).unwrap();
        }
        let pump = crate::app_event_loop::TerminalEventPump::from_receiver(receiver);
        let mut terminal = Terminal::with_options(
            ratatui::backend::CrosstermBackend::new(Vec::<u8>::new()),
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Fixed(ratatui::layout::Rect::new(0, 0, 120, 28)),
            },
        )
        .unwrap();
        let (done, completed) = std::sync::mpsc::channel();
        let wake = app.background.wake_handle();
        std::thread::spawn(move || {
            let _ = completed.recv_timeout(Duration::from_secs(20));
            drop(sender);
            wake.notify();
        });
        let result = crate::app_event_loop::run_app(&mut terminal, &mut app, pump, None);
        assert!(
            result.is_ok(),
            "quit {choice}: {result:?}; active={}, plan={}, flags={:?}, confirm={}, custom={:?}, recovery={:?}, save_error={:?}, notifications={:?}",
            app.workspaces.active,
            app.workspaces.exit.is_some(),
            (
                app.exit_save_and_quit,
                app.exit_discard_quit,
                app.exit_keep_unsaved_quit,
                app.shutdown_anim_active
            ),
            app.exit_confirm_active,
            (
                app.custom_exit_started,
                app.custom_exit_remaining,
                app.custom_exit_failed
            ),
            (app.exit_recovery.pending(), app.exit_recovery.saved),
            app.save_error,
            app.notification_history
        );
        done.send(()).unwrap();
        app.finish_workspaces_on_shutdown();
        assert_eq!(app.workspaces.parked.len(), 1);
        for (project, marker) in [(&a, '!'), (&b, '?')] {
            let mut session = crate::session::load_session(project.path()).unwrap();
            let saved = std::fs::read_to_string(project.path().join("file.txt")).unwrap();
            if choice == 'y' {
                assert!(saved.contains(marker));
            } else {
                assert_eq!(saved, "saved");
            }
            crate::session::hydrate_saved_tab(project.path(), &mut session.tabs[0]);
            assert_eq!(
                session.tabs[0].pending_unsaved_content.is_some(),
                choice == 'k'
            );
        }
    }
}

#[test]
fn hidden_idle_workspaces_have_no_cosmetic_deadlines_and_respect_shared_budgets() {
    let projects: Vec<_> = (0..3).map(|_| Project::new()).collect();
    let wake = crate::background::UiWake::default();
    let mut app = projects[0].app(wake.clone());
    for project in &projects[1..] {
        app.insert_workspace(project.app(wake.clone()));
    }
    for parked in &mut app.workspaces.parked {
        parked.app.last_user_activity = Instant::now() - Duration::from_secs(30);
        parked.app.last_edit_time = Instant::now() - Duration::from_secs(30);
    }
    assert_eq!(app.background_workspace_wait(), None);
    // Even an expired budget services one workspace and resumes at the next;
    // it does not restart forever at a noisy visible workbench.
    for parked in &app.workspaces.parked {
        parked
            .app
            .background
            .sender
            .send(crate::background::BackgroundResult::GitBranch(format!(
                "branch-{}",
                parked.id
            )))
            .unwrap();
    }
    for _ in 0..6 {
        app.service_workspaces_until(Service::Workers, Instant::now(), || true);
    }
    for parked in &app.workspaces.parked {
        assert_eq!(
            parked.app.cached_git_branch,
            format!("branch-{}", parked.id)
        );
    }
    assert!(!app.workspaces_have_pending_results());
    assert_eq!(app.background_workspace_wait(), None);
}

#[test]
fn keyboard_cycles_accumulate_before_dispatch_and_picker_captures_paste() {
    let projects: Vec<_> = (0..3).map(|_| Project::new()).collect();
    let wake = crate::background::UiWake::default();
    let mut app = projects[0].app(wake.clone());
    for project in &projects[1..] {
        app.insert_workspace(project.app(wake.clone()));
    }
    for _ in 0..2 {
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::PageDown, KeyModifiers::CONTROL | KeyModifiers::ALT),
        );
    }
    app.poll_workspace_requests();
    assert_eq!(
        app.workspaces.active, 1,
        "two switches queued before a frame must both count"
    );
    let text = app.editor().unwrap().rope.to_string();
    crate::ui_workspaces::open(&mut app);
    assert!(!app.has_text_input_focus());
    crate::app_event_loop::handle_paste_input(&mut app, "must not edit through the picker");
    assert_eq!(app.editor().unwrap().rope.to_string(), text);
    app.show_save_error("A save failed while the picker was open");
    crate::input_keyboard::handle_keyboard_event(
        &mut app,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
    );
    assert!(!app.workspaces.popup.open);
    assert!(
        app.save_error.is_none(),
        "a newly arriving modal must receive the key"
    );
    assert!(app.workspaces.request.is_none());
}
