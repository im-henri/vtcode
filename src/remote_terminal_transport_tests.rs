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

fn quiet_terminal(client: &client::Client, wake: crate::background::UiWake) -> crate::terminal::TerminalEmulator {
    let (program, args) = fixture_args("terminal_transport::quiet_input_child");
    let process = client.spawn(program, args, RemotePath::default(), None, wake.clone()).unwrap();
    let mut pty = crate::terminal::TerminalEmulator::from_remote(process, 80, 24, wake).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        pty.process();
        let text: String = pty.grid.cells.iter().flat_map(|row| row.iter().map(|cell| cell.ch)).collect();
        if text.contains("VT-QUIET-READY") && !pty.has_pending_output() { return pty; }
        assert!(Instant::now() < deadline, "quiet terminal did not start");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn pending_remote_input_animates_the_cursor_and_a_silent_acknowledgement_restores_it() {
    use crate::{app::{ActivePane, App, Tab, TabKind}, app_terminal_selection::TerminalSurface,
        editor::Editor};
    use ratatui::{Terminal, backend::{Backend, TestBackend}, layout::{Position, Rect}};
    let directory = tempfile::tempdir().unwrap();
    let (client, gate) = gated_connection(directory.path());
    let wake = crate::background::UiWake::default();
    let pty = quiet_terminal(&client, wake.clone());
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
    let progress = app.terminals[0].terminal_pty.as_ref().unwrap().remote_input_progress();
    assert!(progress.pending);
    let started = progress.pending_since.unwrap();
    assert!(app.process_terminals());
    assert_eq!(app.cursor_blink_start(), None);
    assert!(!app.terminals[0].terminal_pty.as_ref().unwrap().has_pending_output());
    let paint = |app: &App, blink| {
        let tab = &app.terminals[0];
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        // Omitting the native cursor request leaves it hidden at this sentinel.
        terminal.backend_mut().set_cursor_position((99, 29)).unwrap();
        terminal.draw(|frame| crate::ui_helpers::draw_terminal_emulator_with_cursor(frame,
            tab.terminal_pty.as_ref().unwrap(), area, &app.colors.ui, blink, true,
            &tab.input_activity)).unwrap();
        let position = terminal.backend_mut().get_cursor_position().unwrap();
        (terminal.backend().buffer().clone(), position)
    };
    app.terminals[0].input_activity.show(started);
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

    let grid = &mut app.terminals[0].terminal_pty.as_mut().unwrap().grid;
    grid.cells[grid.cursor_row][grid.cursor_col].ch = '>';
    let position = (area.x + grid.cursor_col as u16, area.y + grid.cursor_row as u16);
    for (elapsed, symbol) in [(200, "⠋"), (320, "⠙")] {
        let now = started + Duration::from_millis(elapsed);
        let activity = &app.terminals[0].input_activity;
        activity.begin_frame();
        activity.show(now);
        let off = paint(&app, false);
        assert_eq!(off, paint(&app, true));
        assert_eq!(off.0[position].symbol(), symbol);
        assert_eq!(off.1, Position::new(99, 29), "the spinner replaces the host cursor");
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
    assert_eq!(off.0[position].symbol(), ">");
    assert_ne!(off.0, on.0, "normal blinking resumes after delivery");
    assert_eq!(on.1, Position::from(position));
    drop(app);
    client.call(Command::Shutdown).unwrap();
}

#[test]
fn remote_terminal_input_spinner_matches_docked_and_editor_cursors_and_clears_without_echo() {
    use crate::{app::{ActivePane, App, Tab}, editor::Editor};
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};

    fn draw(app: &mut App) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(180, 42)).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
        terminal.backend().buffer().clone()
    }
    fn text(buffer: &Buffer, rect: Rect) -> String {
        (rect.x..rect.right()).map(|x| buffer[(x, rect.y)].symbol()).collect()
    }
    fn cursor_position(app: &App) -> (u16, u16) {
        let area = app.terminal_surfaces.iter().find(|surface| surface.index == 0).unwrap().area;
        let grid = &app.terminals[0].terminal_pty.as_ref().unwrap().grid;
        (area.x + grid.cursor_col as u16, area.y + grid.cursor_row as u16)
    }
    let directory = tempfile::tempdir().unwrap();
    let (client, gate) = gated_connection(directory.path());
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(Editor::test_with_text("document"))];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.show_explorer = false;
    app.show_terminal_sidebar = true;
    app.terminal_sidebar_revealed = true;
    app.terminals[0].name = "Shell".into();
    app.terminals[0].terminal_pty = Some(quiet_terminal(&client, app.background.wake_handle()));
    for effect in crate::animation_config::AnimationEffect::ALL { app.animation_settings.set(effect, false); }
    draw(&mut app);
    let tab_areas = app.terminal_sidebar_tab_areas.clone();
    let close_areas = app.terminal_sidebar_close_areas.clone();

    gate.pause();
    app.terminals[0].terminal_pty.as_ref().unwrap().write(b"x");
    let progress = app.terminals[0].terminal_pty.as_ref().unwrap().remote_input_progress();
    let started = progress.pending_since.unwrap();
    draw(&mut app);
    assert!(app.next_terminal_input_wait(Instant::now()).is_some(), "pending input needs its own timer even with effects disabled");
    std::thread::sleep((started + Duration::from_millis(210)).saturating_duration_since(Instant::now()));
    let buffer = draw(&mut app);
    let area = tab_areas.iter().find(|(_, index)| *index == 0).unwrap().0;
    let spinner = buffer[(area.x, area.y)].symbol();
    assert!("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(spinner));
    assert_eq!(buffer[cursor_position(&app)].symbol(), spinner);
    assert!(text(&buffer, area).ends_with(" Shell "));
    assert_eq!(app.terminal_sidebar_tab_areas, tab_areas);
    assert_eq!(app.terminal_sidebar_close_areas, close_areas);
    app.mouse_col = area.x + 2;
    app.mouse_row = area.y;
    let hovered = draw(&mut app);
    let screen: String = hovered.content.iter().map(|cell| cell.symbol()).collect();
    assert!(screen.contains(crate::terminal_input_activity::HINT));
    app.mouse_col = 0;
    app.mouse_row = 0;
    app.terminal_has_focus = false;
    assert_eq!(app.next_terminal_input_wait(Instant::now()), None);
    app.terminal_has_focus = true;
    app.show_terminal_sidebar = false;
    draw(&mut app);
    assert_eq!(app.next_terminal_input_wait(Instant::now()), None, "hidden indicators must not poll");

    assert!(app.detach_terminal_to_group(0, app.active_group));
    let buffer = draw(&mut app);
    let bar = app.tab_bar_area.unwrap();
    let hits = crate::ui_chrome_tabs::TabBarHits::new(&app, bar);
    let terminal_area = hits.tabs.iter().find(|(index, _, _)| *index == app.active_tab).unwrap().1;
    let spinner = buffer[(terminal_area.x + 1, terminal_area.y)].symbol();
    assert!("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(spinner));
    assert_eq!(buffer[cursor_position(&app)].symbol(), spinner);

    app.show_tab_bar = false;
    let buffer = draw(&mut app);
    assert!("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(buffer[cursor_position(&app)].symbol()));
    assert!(app.next_terminal_input_wait(Instant::now()).is_some(), "the cursor drives the spinner without the tab bar");
    app.terminals[0].terminal_pty.as_mut().unwrap().grid.cursor_visible = false;
    let buffer = draw(&mut app);
    assert!(!"⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(buffer[cursor_position(&app)].symbol()));
    assert_eq!(app.next_terminal_input_wait(Instant::now()), None);
    app.terminals[0].terminal_pty.as_mut().unwrap().grid.cursor_visible = true;

    app.show_tab_bar = true;
    app.active_tab = 0; // Keep only the terminal's tab visible, without its console.
    app.animation_settings.set(crate::animation_config::AnimationEffect::TabSlide, true);
    let buffer = draw(&mut app);
    let bar = app.tab_bar_area.unwrap();
    assert!(text(&buffer, bar).contains("  Shell"));
    let hits = crate::ui_chrome_tabs::TabBarHits::new(&app, bar).tabs;
    let terminal_index = app.tabs.iter().position(|tab| tab.terminal_idx == Some(0)).unwrap();
    let terminal_area = hits.iter().find(|(index, _, _)| *index == terminal_index).unwrap().1;
    assert!("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(buffer[(terminal_area.x + 1, terminal_area.y)].symbol()));
    assert!(app.terminal_surfaces.is_empty());
    app.rendering_animation_frame = true;
    draw(&mut app);
    app.rendering_animation_frame = false;
    assert!(app.next_terminal_input_wait(Instant::now()).is_some(), "cached tab strips must preserve the spinner's timer");
    app.mouse_col = terminal_area.x + 2;
    app.mouse_row = terminal_area.y;
    let screen: String = draw(&mut app).content.iter().map(|cell| cell.symbol()).collect();
    assert!(screen.contains(crate::terminal_input_activity::HINT));
    app.mouse_col = 0;
    app.mouse_row = 0;

    let mut grouped = Terminal::new(TestBackend::new(180, 42)).unwrap();
    app.terminals[0].input_activity.begin_frame();
    grouped.draw(|frame| crate::ui_chrome_tabs::draw_tab_bar_for_group(
        frame, &app, bar, app.active_group, Instant::now())).unwrap();
    assert!("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏".contains(grouped.backend().buffer()[(terminal_area.x + 1, bar.y)].symbol()));
    assert!(app.next_terminal_input_wait(Instant::now()).is_some());
    app.terminals[0].input_activity.begin_frame();
    grouped.draw(|frame| crate::ui_chrome_tabs::draw_tab_bar(
        frame, &app, Rect::new(0, 0, 4, 1))).unwrap();
    assert_eq!(app.next_terminal_input_wait(Instant::now()), None, "tabs clipped out of the strip must not animate");
    draw(&mut app);

    // Do not process terminals while the input is pending: even an ack that
    // overtakes the first service turn must clear the visible, inactive tab.
    gate.release();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.terminals[0].terminal_pty.as_ref().unwrap().remote_input_progress().pending {
        assert!(Instant::now() < deadline, "input was never acknowledged");
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(app.process_terminals(), "a silent ack must clear the inactive tab's spinner");
    let buffer = draw(&mut app);
    assert_eq!(app.next_terminal_input_wait(Instant::now()), None);
    assert!(text(&buffer, bar).contains(">_ Shell"));
    assert_eq!(crate::ui_chrome_tabs::TabBarHits::new(&app, bar).tabs, hits);
    assert_eq!(app.terminals[0].name, "Shell");
    assert!(!app.terminals[0].terminal_pty.as_ref().unwrap().has_pending_output());
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
    assert!(reader.get_ref().input_progress().pending_since.is_none());
    assert_eq!(writer.write(b"").unwrap(), 0);
    assert!(reader.get_ref().input_progress().pending_since.is_none());
    writer.write_all(b"x").unwrap();
    assert!(reader.get_ref().input_progress().pending);
    assert!(reader.get_ref().input_progress().pending_since.is_some());
    gate.release();
    let deadline = Instant::now() + Duration::from_secs(5);
    while reader.get_ref().input_progress().pending {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(reader.get_ref().input_progress().acknowledged.is_some());
    assert!(reader.get_ref().input_progress().pending_since.is_none());
    killer.kill();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !killer.ended() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(writer.write(b"after close").unwrap_err().kind(), io::ErrorKind::BrokenPipe);
    assert!(!reader.get_ref().input_progress().pending);
    assert!(reader.get_ref().input_progress().pending_since.is_none());
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
