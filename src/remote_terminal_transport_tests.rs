use super::*;
use std::io::Read;
use std::sync::mpsc;

// Model propagation delay, not a sleep on every write: successive packets can
// travel concurrently, just as they do on an SSH connection.
struct LinkWriter {
    sender: mpsc::SyncSender<(Instant, Vec<u8>)>,
    buffered: Vec<u8>,
    delay: Duration,
    messages: Arc<AtomicUsize>,
}
impl Write for LinkWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buffered.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if !self.buffered.is_empty() {
            self.sender
                .send((
                    Instant::now() + self.delay,
                    std::mem::take(&mut self.buffered),
                ))
                .map_err(io::Error::other)?;
            self.messages.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    }
}
struct LinkReader {
    receiver: mpsc::Receiver<(Instant, Vec<u8>)>,
    buffered: io::Cursor<Vec<u8>>,
}
impl Read for LinkReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.buffered.position() as usize == self.buffered.get_ref().len() {
            match self.receiver.recv_timeout(Duration::from_secs(10)) {
                Ok((ready, bytes)) => {
                    std::thread::sleep(ready.saturating_duration_since(Instant::now()));
                    self.buffered = io::Cursor::new(bytes);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(0),
                Err(error) => return Err(io::Error::new(io::ErrorKind::TimedOut, error)),
            }
        }
        self.buffered.read(out)
    }
}
fn link(delay: Duration, messages: Arc<AtomicUsize>) -> (LinkReader, LinkWriter) {
    let (sender, receiver) = mpsc::sync_channel(256);
    (
        LinkReader {
            receiver,
            buffered: io::Cursor::new(Vec::new()),
        },
        LinkWriter {
            sender,
            buffered: Vec::new(),
            delay,
            messages,
        },
    )
}
fn delayed_connection(root: &Path, one_way: Duration) -> (client::Client, Arc<AtomicUsize>) {
    let messages = Arc::new(AtomicUsize::new(0));
    let (server_input, client_output) = link(one_way, messages.clone());
    let (client_input, server_output) = link(one_way, messages.clone());
    let root = root.to_owned();
    std::thread::spawn(move || {
        let _ = service::serve(&root, server_input, server_output);
    });
    (
        client::Client::connect(client_input, client_output).unwrap(),
        messages,
    )
}

struct ReturnGate(Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);
impl ReturnGate {
    fn pause(&self) { *self.0.0.lock().unwrap() = true; }
    fn release(&self) {
        *self.0.0.lock().unwrap() = false;
        self.0.1.notify_all();
    }
}
impl Drop for ReturnGate {
    fn drop(&mut self) { self.release(); }
}
struct GatedReader<R> {
    inner: R,
    gate: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}
impl<R: Read> Read for GatedReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.inner.read(bytes)?;
        let mut paused = self.gate.0.lock().unwrap();
        while *paused { paused = self.gate.1.wait(paused).unwrap(); }
        Ok(count)
    }
}
fn gated_connection(root: &Path) -> (client::Client, ReturnGate) {
    let (input, client_output) = link(Duration::ZERO, Arc::default());
    let (client_input, output) = link(Duration::ZERO, Arc::default());
    let gate = ReturnGate(Arc::default());
    let reader = GatedReader { inner: client_input, gate: gate.0.clone() };
    let root = root.to_owned();
    std::thread::spawn(move || { let _ = service::serve(&root, input, output); });
    (client::Client::connect(reader, client_output).unwrap(), gate)
}

#[test]
fn pending_remote_input_holds_a_steady_cursor_and_acknowledgements_resume_it_without_echo() {
    use crate::{app::{ActivePane, App, Tab, TabKind}, app_terminal_selection::TerminalSurface,
        editor::Editor, terminal::TerminalEmulator};
    use ratatui::{Terminal, backend::{Backend, TestBackend}, layout::{Position, Rect}};
    let directory = tempfile::tempdir().unwrap();
    let (client, gate) = gated_connection(directory.path());
    let (program, args) = fixture_args("terminal_transport::quiet_input_child");
    let wake = crate::background::UiWake::default();
    let process = client.spawn(program, args, RemotePath::default(), None, wake.clone()).unwrap();
    let mut pty = TerminalEmulator::from_remote(process, 80, 24, wake.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        pty.process();
        let text: String = pty.grid.cells.iter().flat_map(|row| row.iter().map(|cell| cell.ch)).collect();
        if text.contains("VT-QUIET-READY") && !pty.has_pending_output() { break; }
        assert!(Instant::now() < deadline, "quiet terminal did not start");
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut app = App::test_new();
    let mut tab = Tab::new_file(Editor::new());
    tab.kind = TabKind::Terminal;
    tab.terminal_idx = Some(0);
    app.tabs = vec![tab];
    app.active_pane = ActivePane::Editor;
    app.cursor_blinking = true;
    app.last_blink = Instant::now() - Duration::from_secs(5);
    app.terminals[0].terminal_pty = Some(pty);
    let area = Rect::new(2, 1, 80, 24);
    app.terminal_surfaces = vec![TerminalSurface { index: 0, area, group: Some(0) }];
    assert_eq!(app.cursor_blink_start(), Some(app.last_blink));
    gate.pause();
    app.terminals[0].terminal_pty.as_ref().unwrap().write(b"x");
    assert!(app.terminals[0].terminal_pty.as_ref().unwrap().remote_input_progress().pending);
    assert!(app.process_terminals());
    assert_eq!(app.cursor_blink_start(), None);
    assert!(!app.terminals[0].terminal_pty.as_ref().unwrap().has_pending_output());
    let paint = |app: &App, blink| {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        // Omitting the native cursor request leaves it hidden at this sentinel.
        terminal.backend_mut().set_cursor_position((99, 29)).unwrap();
        terminal.draw(|frame| crate::ui_helpers::draw_terminal_emulator(frame,
            app.terminals[0].terminal_pty.as_ref().unwrap(), area, &app.colors.ui, blink)).unwrap();
        let position = terminal.backend_mut().get_cursor_position().unwrap();
        (terminal.backend().buffer().clone(), position)
    };
    for shape in 0..=6 {
        app.terminals[0].terminal_pty.as_mut().unwrap().grid.cursor_shape = shape;
        let off = paint(&app, false);
        let on = paint(&app, true);
        assert_eq!(off, on, "pending cursor must not change with the blink phase");
        assert_eq!(on.1, Position::new(99, 29), "the host must not blink a second cursor");
        let pty = app.terminals[0].terminal_pty.as_ref().unwrap();
        let x = area.x + pty.grid.cursor_col as u16;
        let y = area.y + pty.grid.cursor_row as u16;
        assert_ne!(on.0[(x, y)], on.0[(x + 1, y)], "the steady cursor must remain visible");
    }
    // A waiting terminal behind another input must not freeze that input.
    app.palette.active = true;
    assert_eq!(app.cursor_blink_start(), Some(app.last_blink));
    app.palette.active = false;
    app.tabs.push(Tab::new_file(Editor::test_with_text("other editor")));
    app.active_tab = 1;
    assert_eq!(app.cursor_blink_start(), Some(app.last_blink));
    app.active_tab = 0;
    app.terminals[0].terminal_pty.as_mut().unwrap().grid.cursor_shape = 0;
    let generation = wake.generation();
    gate.release();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut redrawn = false;
    loop {
        redrawn |= app.process_terminals();
        if !app.terminals[0].terminal_pty.as_ref().unwrap().remote_input_progress().pending { break; }
        assert!(Instant::now() < deadline, "remote input was never acknowledged");
        std::thread::sleep(Duration::from_millis(1));
    }
    redrawn |= app.process_terminals();
    assert!(redrawn, "a silent acknowledgement must repaint the visible terminal");
    assert_ne!(wake.generation(), generation);
    let pty = app.terminals[0].terminal_pty.as_ref().unwrap();
    assert!(!pty.has_pending_output(), "the fixture deliberately does not echo input");
    let acknowledged = pty.remote_input_progress().acknowledged.unwrap();
    assert_eq!(app.cursor_blink_start(), Some(acknowledged));
    assert!(acknowledged > app.last_blink);
    let off = paint(&app, false);
    let on = paint(&app, true);
    assert_ne!(off.0, on.0, "normal blinking resumes after delivery");
    assert_ne!(on.1, Position::new(99, 29));
    drop(app);
    client.call(Command::Shutdown).unwrap();
}

#[test]
fn rejected_remote_input_never_becomes_pending() {
    let directory = tempfile::tempdir().unwrap();
    let (client, gate) = gated_connection(directory.path());
    let (program, args) = fixture_args("terminal_transport::quiet_input_child");
    let (reader, writer, killer) = client.spawn(program, args, RemotePath::default(), None, Default::default()).unwrap().split();
    let mut reader = io::BufReader::new(reader);
    prelude(&mut reader, "VT-QUIET-READY");
    let mut writer = process::QueuedWriter::new(writer).unwrap();
    gate.pause();
    assert_eq!(writer.write(&vec![b'x'; 4 * 1024 * 1024 + 1]).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    assert!(!reader.get_ref().input_progress().pending);
    writer.write_all(b"x").unwrap();
    assert!(reader.get_ref().input_progress().pending);
    gate.release();
    let deadline = Instant::now() + Duration::from_secs(5);
    while reader.get_ref().input_progress().pending {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(reader.get_ref().input_progress().acknowledged.is_some());
    killer.kill();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !killer.ended() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(writer.write(b"after close").unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    assert!(!reader.get_ref().input_progress().pending);
    client.call(Command::Shutdown).unwrap();
}

#[test]
#[ignore = "subprocess fixture with deliberately silent input"]
fn quiet_input_child() {
    println!("\nVT-QUIET-READY");
    io::stdout().flush().unwrap();
    let mut byte = [0];
    io::stdin().read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"x");
    // Keep the terminal alive after accepting the byte, with no echo/output.
    let _ = io::stdin().read(&mut byte);
}

#[test]
fn remote_terminal_forwards_full_screen_wheel_and_native_mouse_over_the_connection() {
    use crate::{app::{ActivePane, App, Tab}, app_terminal_selection::TerminalSurface, editor::Editor, terminal::TerminalEmulator};
    use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
    fn wait_for(app: &mut App, marker: &str) {
        let end = Instant::now() + Duration::from_secs(8);
        loop {
            let pty = app.terminals[0].terminal_pty.as_mut().unwrap();
            pty.process();
            let text: String = pty.grid.cells.iter().flat_map(|row| row.iter().map(|cell| cell.ch)).collect();
            if text.contains(marker) { return; }
            assert!(Instant::now() < end, "remote terminal did not receive {marker}: {text}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let (client, _) = delayed_connection(directory.path(), Duration::from_millis(2));
    let (program, args) = fixture_args("terminal_transport::remote_terminal_wheel_child");
    // A pipe keeps the byte-level assertion portable; these are the same
    // process channels and queued terminal writer used by SSH PTY sessions.
    let process = client.spawn(program, args, RemotePath::default(), None, Default::default()).unwrap();
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::new())];
    app.active_pane = ActivePane::Editor;
    app.show_terminal_sidebar = true;
    app.terminal_panel_focused = true;
    app.scroll_speed = 3;
    app.terminals[0].terminal_pty = Some(TerminalEmulator::from_remote(process, 80, 24, Default::default()).unwrap());
    app.terminal_surfaces = vec![TerminalSurface { index: 0, area: ratatui::layout::Rect::new(5, 6, 24, 4), group: None }];
    wait_for(&mut app, "VT-WHEEL-READY");
    for marker in ["VT-WHEEL-MOUSE-READY", "VT-WHEEL-OK"] {
        for kind in [MouseEventKind::ScrollUp, MouseEventKind::ScrollDown] {
            crate::mouse::handle_mouse(&mut app, MouseEvent { kind, column: 8, row: 7, modifiers: KeyModifiers::NONE });
        }
        wait_for(&mut app, marker);
    }
    drop(app);
    client.call(Command::Shutdown).unwrap();
}

#[test]
#[ignore = "subprocess fixture for remote terminal wheel input"]
fn remote_terminal_wheel_child() {
    let mut output = io::stdout().lock();
    let mut input = io::stdin().lock();
    output.write_all(b"\x1b[?1049;1h\r\nVT-WHEEL-READY\r\n").unwrap();
    output.flush().unwrap();
    let keys = [b"\x1bOA".repeat(3), b"\x1bOB".repeat(3)].concat();
    let mut received = vec![0; keys.len()];
    input.read_exact(&mut received).unwrap();
    assert_eq!(received, keys);
    output.write_all(b"\x1b[?1002;1006h\r\nVT-WHEEL-MOUSE-READY\r\n").unwrap();
    output.flush().unwrap();
    let reports = b"\x1b[<64;4;2M\x1b[<65;4;2M";
    let mut received = vec![0; reports.len()];
    input.read_exact(&mut received).unwrap();
    assert_eq!(received, reports);
    output.write_all(b"\x1b[?1002;1006;1049l\r\nVT-WHEEL-OK\r\n").unwrap();
    output.flush().unwrap();
}

#[test]
#[ignore = "Remote terminal throughput and echo benchmark; run explicitly"]
fn benchmark_remote_terminal_transport() {
    for one_way in [Duration::ZERO, Duration::from_millis(20)] {
        let dir = tempfile::tempdir().unwrap();
        let (client, messages) = delayed_connection(dir.path(), one_way);
        let (program, args) = fixture_args("process_flood_fixture");
        let (reader, _writer, killer) = client
            .spawn(
                program,
                args,
                RemotePath::default(),
                None,
                Default::default(),
            )
            .unwrap()
            .split();
        let mut reader = io::BufReader::new(reader);
        prelude(&mut reader, "VT-FLOOD-READY");
        let start_messages = messages.load(Ordering::Relaxed);
        let start = Instant::now();
        let mut bytes = vec![0; 4 * 1024 * 1024];
        reader.read_exact(&mut bytes[..1]).unwrap();
        let first = start.elapsed();
        reader.read_exact(&mut bytes[1..]).unwrap();
        assert!(bytes.iter().all(|byte| *byte == b'x'));
        println!(
            "TERMINAL_BENCH rtt_ms={} bulk_ms={:.2} first_ms={:.2} messages={}",
            one_way.as_millis() * 2,
            start.elapsed().as_secs_f64() * 1000.0,
            first.as_secs_f64() * 1000.0,
            messages.load(Ordering::Relaxed) - start_messages
        );
        drop(killer);
        let (program, args) = fixture_args("process_echo_fixture");
        let (reader, writer, killer) = client
            .spawn(
                program,
                args,
                RemotePath::default(),
                None,
                Default::default(),
            )
            .unwrap()
            .split();
        let mut reader = io::BufReader::new(reader);
        let mut writer = process::QueuedWriter::new(writer).unwrap();
        prelude(&mut reader, "VT-ECHO-READY");
        let start = Instant::now();
        for byte in 0..20 {
            writer.write_all(&[byte]).unwrap();
            let mut echoed = [0];
            reader.read_exact(&mut echoed).unwrap();
            assert_eq!(echoed, [byte]);
        }
        println!(
            "TERMINAL_BENCH rtt_ms={} echo_ms={:.2}",
            one_way.as_millis() * 2,
            start.elapsed().as_secs_f64() * 1000.0 / 20.0
        );
        drop(killer);
        client.call(Command::Shutdown).unwrap();
    }
}

#[test]
#[ignore = "Private console terminal echo benchmark; run explicitly"]
fn benchmark_remote_terminal_console_echo() {
    let (program, args) = fixture_args("headless_service_fixture");
    let setup = raw_launch(std::iter::once(program.into()).chain(args.into_iter().map(Into::into)))
        .start(ratatui::layout::Size::new(80, 24), Default::default())
        .unwrap();
    let (client, _owned) = setup.endpoint_for_test().unwrap();
    let (program, args) = fixture_args("process_echo_fixture");
    let (reader, writer, killer) = client
        .spawn(
            program,
            args,
            RemotePath::default(),
            None,
            Default::default(),
        )
        .unwrap()
        .split();
    let mut reader = io::BufReader::new(reader);
    let mut writer = process::QueuedWriter::new(writer).unwrap();
    prelude(&mut reader, "VT-ECHO-READY");
    let mut elapsed = Vec::new();
    for byte in 0..100 {
        let start = Instant::now();
        writer.write_all(&[byte]).unwrap();
        let mut echoed = [0];
        reader.read_exact(&mut echoed).unwrap();
        assert_eq!(echoed, [byte]);
        elapsed.push(start.elapsed());
    }
    elapsed.sort_unstable();
    println!(
        "TERMINAL_CONSOLE_ECHO median_ms={:.2} p95_ms={:.2}",
        elapsed[50].as_secs_f64() * 1000.0,
        elapsed[95].as_secs_f64() * 1000.0
    );
    drop(killer);
    client.call(Command::Shutdown).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "Internal process scheduling fixture"]
fn process_priority_fixture() {
    println!("\nVT-PRIORITY:{}", unsafe {
        libc::getpriority(libc::PRIO_PROCESS, 0)
    });
    io::stdout().flush().unwrap();
    let _ = io::stdin().read(&mut [0]);
}

#[cfg(target_os = "linux")]
#[test]
fn remote_shells_and_tools_inherit_the_service_priority() {
    let expected = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
    let dir = tempfile::tempdir().unwrap();
    let (client, _) = delayed_connection(dir.path(), Duration::ZERO);
    for terminal in [None, Some((80, 24))] {
        let program = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let args = vec![
            "--ignored".into(),
            "--exact".into(),
            "remote::tests::terminal_transport::process_priority_fixture".into(),
            "--nocapture".into(),
        ];
        let (reader, _writer, _killer) = client
            .spawn(
                program,
                args,
                RemotePath::default(),
                terminal,
                Default::default(),
            )
            .unwrap()
            .split();
        let mut reader = io::BufReader::new(reader);
        let mut text = String::new();
        let priority: i32 = loop {
            text.clear();
            assert_ne!(std::io::BufRead::read_line(&mut reader, &mut text).unwrap(), 0);
            if let Some(priority) = text.trim().strip_prefix("VT-PRIORITY:") {
                break priority.parse().unwrap();
            }
        };
        assert_eq!(priority, expected, "terminal={terminal:?}: {text}");
    }
    client.call(Command::Shutdown).unwrap();
}
