use super::*;
use crate::app::App;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
use std::io::{self, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

#[test]
#[ignore = "32-second idle transport soak; run explicitly"]
fn idle_headless_connection_sends_no_periodic_traffic_and_resumes_on_demand() {
    let root = tempfile::tempdir().unwrap();
    let outgoing = Arc::new(AtomicUsize::new(0));
    let incoming = Arc::new(AtomicUsize::new(0));
    let client = super::tests::connection_with_traffic(
        root.path(), Arc::default(), outgoing.clone(), Arc::default(), incoming.clone(),
    );
    client.call(Command::Ping).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let before = (outgoing.load(Ordering::Relaxed), incoming.load(Ordering::Relaxed));
    // Cross the former 30-second idle-probe interval using the real transport.
    std::thread::sleep(Duration::from_secs(32));
    let after = (outgoing.load(Ordering::Relaxed), incoming.load(Ordering::Relaxed));
    let connected = client.connected();
    let resumed = client.call_timeout(Command::Ping, Duration::from_secs(5));
    let _ = client.call_timeout(Command::Shutdown, Duration::from_secs(5));
    assert_eq!(after, before, "An idle connection sent or received protocol traffic");
    assert!(connected, "Silence must not expire a healthy transport");
    assert!(matches!(resumed, Ok(Reply::Done)), "{resumed:?}");
}

struct Saving {
    app: App,
    root: tempfile::TempDir,
    hold: Arc<AtomicBool>,
    socket: TcpStream,
    service: Option<std::thread::JoinHandle<io::Result<()>>>,
}
impl Drop for Saving {
    fn drop(&mut self) {
        self.hold.store(false, Ordering::Release);
        let _ = self.socket.shutdown(std::net::Shutdown::Both);
        if let Some(service) = self.service.take() {
            let _ = service.join();
        }
    }
}
fn stalled_save() -> Saving {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("file.txt"), "disk\n").unwrap();
    let hold = Arc::new(AtomicBool::new(false));
    let blocked = Arc::new(AtomicBool::new(false));
    struct Paused {
        socket: TcpStream,
        hold: Arc<AtomicBool>,
        blocked: Arc<AtomicBool>,
    }
    impl Write for Paused {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.hold.load(Ordering::Acquire) {
                self.blocked.store(true, Ordering::Release);
                while self.hold.load(Ordering::Acquire) {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            self.socket.write(bytes)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.socket.flush()
        }
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    socket.set_nodelay(true).unwrap();
    let (server, _) = listener.accept().unwrap();
    server.set_nodelay(true).unwrap();
    let writer = Paused {
        socket: server.try_clone().unwrap(),
        hold: hold.clone(),
        blocked: blocked.clone(),
    };
    let service_root = root.path().to_owned();
    let service = std::thread::spawn(move || service::serve(&service_root, server, writer));
    let client =
        client::Client::connect(socket.try_clone().unwrap(), socket.try_clone().unwrap()).unwrap();
    let workspace = workspace::Workspace::new(client);
    let key = workspace.view_key(&RemotePath::new(b"file.txt".to_vec()).unwrap());
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.source_control_enabled = false;
    app.remote_workspace = Some(workspace);
    app.open_file(&key);
    app.wait_for_background(Duration::from_secs(20), |app| {
        app.editor().is_some_and(|editor| !editor.bg_loading)
    });
    app.editor_mut().unwrap().set_cursor(0, 0);
    app.editor_mut().unwrap().insert_char('x');
    app.editor_mut().unwrap().break_undo_group();
    let mut fixture = Saving {
        app,
        root,
        hold,
        socket,
        service: Some(service),
    };
    fixture.hold.store(true, Ordering::Release);
    let app = &mut fixture.app;
    app.save_current_file();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !blocked.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "save reply did not reach the gate"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    // The write completed but its ACK is unavailable: never assume cancellation
    // means that disk still contains the previous saved version.
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
        "xdisk\n"
    );
    assert!(!app.pending_file_saves.is_empty());
    fixture
}
fn click(app: &mut App, index: usize) {
    let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    let area = app
        .modal
        .buttons
        .iter()
        .find(|(_, button)| *button == index)
        .unwrap()
        .0;
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + area.width / 2,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        },
    );
}

#[test]
fn idle_remote_save_cannot_trap_mouse_or_keyboard_keep_unsaved_quit() {
    for mouse in [false, true] {
        let mut fixture = stalled_save();
        let app = &mut fixture.app;
        app.exit_confirm_active = true;
        // Save is still pending; the dialog must explain that and remain
        // cancellable rather than disappearing into a never-ending quit wait.
        click(app, 0);
        assert!(app.exit_confirm_active);
        assert!(!app.exit_save_and_quit);
        assert!(app.modal.error.as_ref().unwrap().contains("pending"));
        click(app, 3);
        assert!(!app.exit_confirm_active);
        app.exit_confirm_active = true;
        if mouse {
            click(app, 2);
        } else {
            assert!(!crate::input_keyboard::handle_keyboard_event(
                app,
                KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE)
            ));
        }
        assert!(app.exit_keep_unsaved_quit);
        assert!(!app.exit_confirm_active);
        app.wait_for_background(Duration::from_secs(5), |app| {
            app.pending_file_saves.is_empty()
        });
        assert!(!app.remote_workspace.as_ref().unwrap().client.connected());
        assert_eq!(app.editor().unwrap().rope.to_string(), "xdisk\n");
        assert!(app.editor().unwrap().modified);
        app.editor_mut().unwrap().undo();
        assert_eq!(app.editor().unwrap().rope.to_string(), "disk\n");
        app.editor_mut().unwrap().redo();
        let session = app.build_session();
        assert_eq!(
            session.tabs[0]
                .pending_unsaved_content
                .as_ref()
                .unwrap()
                .to_string(),
            "xdisk\n"
        );
        assert!(!session.tabs[0].undo_entries.as_ref().unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(fixture.root.path().join("file.txt")).unwrap(),
            "xdisk\n"
        );
    }
}

#[test]
fn failed_save_and_quit_reopens_confirmation_without_retrying_the_write() {
    let mut fixture = stalled_save();
    let app = &mut fixture.app;
    app.exit_save_and_quit = true;
    app.custom_exit_started = true;
    app.remote_workspace.as_ref().unwrap().disconnect();
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.pending_file_saves.is_empty()
    });
    assert!(!app.exit_save_and_quit);
    assert!(!app.custom_exit_started);
    assert!(app.exit_confirm_active);
    assert!(app.editor().unwrap().modified);
    assert!(crate::input_dialogs::save_text_documents_before_exit(app).is_err());
    assert!(
        app.pending_file_saves.is_empty(),
        "A failed exit must not restart the save"
    );
}
