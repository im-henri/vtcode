//! SSH authentication and launch output followed by a local App. Only this
//! connection phase forwards keys. Once attached, the SSH stream carries operations,
//! never editor input or remotely rendered screen updates.
use super::*;
use crate::{app_event_loop::TerminalEventPump, background::UiWake};
use crossterm::event::{Event, KeyCode, KeyModifiers};
use std::{
    ffi::OsString,
    io::{BufReader, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
};
#[cfg(windows)]
#[path = "remote_console.rs"]
mod console;
#[cfg(windows)]
pub(crate) use console::run_host;

trait LoginWriter: Write + Send {
    fn key(
        &mut self,
        key: crossterm::event::KeyEvent,
        app_cursor: bool,
        win32: bool,
    ) -> io::Result<()> {
        let mut result = Ok(());
        crate::input_terminal::write_key(key, app_cursor, win32, |bytes| {
            if result.is_ok() {
                result = self.write_all(bytes);
            }
        });
        result
    }
}
#[cfg(not(windows))]
struct ByteWriter(Box<dyn Write + Send>);
#[cfg(not(windows))]
impl Write for ByteWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}
#[cfg(not(windows))]
impl LoginWriter for ByteWriter {}
struct Transport {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn LoginWriter>,
    #[cfg(windows)]
    job: Option<std::os::windows::io::OwnedHandle>,
    #[cfg(windows)]
    console_output: Option<Box<dyn Read + Send>>,
}
enum Input {
    Text(Vec<u8>),
    Key(crossterm::event::KeyEvent, bool, bool),
}

#[derive(Clone)]
pub(crate) struct Launch {
    program: OsString,
    args: Vec<OsString>,
}
/// Retain the exact opaque SSH/job command and its original working directory.
/// Reconnection must not invent a scheduler command or depend on a node address.
pub(crate) struct Recipe {
    launch: Launch,
    cwd: PathBuf,
    pub(crate) diagnostics: super::diagnostics::Log,
}
impl Recipe {
    pub(crate) fn start(&self, size: ratatui::layout::Size, wake: UiWake) -> io::Result<Setup> {
        self.launch.clone().start_with_log(size, wake, &self.cwd, self.diagnostics.clone())
    }
}
impl Launch {
    pub(crate) fn new(program: OsString, args: Vec<OsString>) -> Self {
        Self { program, args }
    }
    #[cfg(test)]
    pub(crate) fn parse(args: impl IntoIterator<Item = OsString>) -> io::Result<Option<Self>> {
        match crate::cli::parse(args)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?
        {
            crate::cli::Command::Remote { program, args } => Ok(Some(Self::new(program, args))),
            _ => Ok(None),
        }
    }
    pub(crate) fn start(self, size: ratatui::layout::Size, wake: UiWake) -> io::Result<Setup> {
        self.start_in(size, wake, &std::env::current_dir()?)
    }
    pub(crate) fn start_in(
        self,
        size: ratatui::layout::Size,
        wake: UiWake,
        cwd: &Path,
    ) -> io::Result<Setup> {
        self.start_with_log(size, wake, cwd, Default::default())
    }
    fn start_with_log(self, size: ratatui::layout::Size, wake: UiWake, cwd: &Path, log: super::diagnostics::Log) -> io::Result<Setup> {
        let trace = log.begin(&self.program);
        let result = self.start_attempt(size, wake, cwd, trace.clone());
        if let Err(error) = &result { trace.event("Launch failed", &error.to_string()); }
        result
    }
    fn start_attempt(self, size: ratatui::layout::Size, wake: UiWake, cwd: &Path, trace: super::diagnostics::Attempt) -> io::Result<Setup> {
        trace.event("Local directory", &cwd.to_string_lossy());
        let recipe = Arc::new(Recipe { launch: self.clone(), cwd: cwd.to_owned(), diagnostics: trace.log.clone() });
        let mut identity = Vec::new();
        for arg in std::iter::once(&self.program).chain(&self.args) {
            let bytes = arg.as_encoded_bytes();
            identity.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            identity.extend_from_slice(bytes);
        }
        #[cfg(windows)]
        let transport = console::spawn(&self.program, &self.args, size, cwd)?;
        #[cfg(not(windows))]
        let transport = {
            let pair = portable_pty::native_pty_system()
                .openpty(portable_pty::PtySize {
                    rows: size.height.saturating_sub(2).max(2),
                    cols: size.width.max(2),
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(io::Error::other)?;
            let mut command = portable_pty::CommandBuilder::new(self.program);
            command.cwd(cwd);
            for arg in self.args {
                command.arg(arg);
            }
            command.env("TERM", "xterm-256color");
            command.env("TERM_PROGRAM", "vtcode");
            let child = pair
                .slave
                .spawn_command(command)
                .map_err(io::Error::other)?;
            drop(pair.slave);
            let reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
            let writer = Box::new(ByteWriter(
                pair.master.take_writer().map_err(io::Error::other)?,
            ));
            Transport {
                child,
                master: pair.master,
                reader,
                writer,
            }
        };
        let Transport {
            mut child,
            master,
            reader,
            writer,
            #[cfg(windows)]
            job,
            #[cfg(windows)]
            console_output,
        } = transport;
        #[cfg(windows)]
        let waiting_job = job.as_ref().map(|job| job.try_clone()).transpose()?;
        let killer = child.clone_killer();
        let writer = SharedWriter(Arc::new(Mutex::new(writer)));
        let attached = Arc::new(AtomicBool::new(false));
        let termination = Arc::new(AtomicBool::new(false));
        let owned = Arc::new(Process {
            killer: Mutex::new(killer),
            master: Mutex::new(master),
            trace: trace.clone(),
            termination: termination.clone(),
            #[cfg(windows)]
            job,
        });
        let (output, incoming) = mpsc::sync_channel(32);
        #[cfg(windows)]
        if let Some(mut console) = console_output {
            let side_output = output.clone();
            let side_attached = attached.clone();
            let side_wake = wake.clone();
            let side_trace = trace.clone();
            std::thread::Builder::new()
                .name("remote-console-prompts".into())
                .spawn(move || {
                    let mut bytes = [0; 4096];
                    while let Ok(count) = console.read(&mut bytes) {
                        if count == 0 {
                            break;
                        }
                        side_trace.output("Console", &bytes[..count]);
                        // Authentication may write directly to CONOUT$ instead of
                        // stderr. Display that during login; protocol bytes always
                        // use the separate, lossless data pipe.
                        if !side_attached.load(Ordering::Acquire) {
                            if side_output.send(bytes[..count].to_vec()).is_err() {
                                break;
                            }
                            side_wake.notify();
                        }
                    }
                })?;
        }
        let (ready, endpoint) = mpsc::sync_channel(1);
        let (keys, outgoing) = mpsc::sync_channel::<Input>(32);
        let input_phase = attached.clone();
        let mut input_writer = writer.clone();
        let input_wake = wake.clone();
        std::thread::Builder::new()
            .name("remote-login-input".into())
            .spawn(move || {
                for input in outgoing {
                    if input_phase.load(Ordering::Acquire) {
                        break;
                    }
                    let result = match input {
                        Input::Text(bytes) => input_writer.write_all(&bytes),
                        Input::Key(key, app_cursor, win32) => {
                            input_writer.0.lock().unwrap().key(key, app_cursor, win32)
                        }
                    };
                    if result.and_then(|_| input_writer.flush()).is_err() {
                        input_wake.notify();
                        break;
                    }
                }
            })?;
        let output_wake = wake.clone();
        let output_trace = trace.clone();
        std::thread::Builder::new()
            .name("remote-login-output".into())
            .spawn(move || {
                let result = discover(reader, writer, output, &output_wake, attached, &output_trace);
                if let Err(error) = &result { output_trace.event("Setup failed", &error.to_string()); }
                let _ = ready.send(result);
                output_wake.notify();
            })?;
        let exit_wake = wake.clone();
        std::thread::Builder::new()
            .name("remote-login-exit".into())
            .spawn(move || {
                let status = child.wait();
                let origin = if termination.load(Ordering::Acquire) { " (local cleanup requested)" } else { "" };
                #[cfg(windows)]
                let source = "Local helper exit";
                #[cfg(not(windows))]
                let source = "Transport exit";
                trace.event(source, &match status {
                    Ok(status) => format!("{status}; exit code {}{origin}", status.exit_code()),
                    Err(error) => format!("Could not wait for transport: {error}{origin}"),
                });
                #[cfg(windows)]
                if let Some(job) = waiting_job {
                    console::terminate_job(&job);
                }
                exit_wake.notify();
            })?;
        Ok(Setup {
            owned,
            incoming,
            endpoint,
            keys,
            wake,
            identity,
            recipe: Some(recipe),
        })
    }
}

pub(crate) struct Process {
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    pub(crate) trace: super::diagnostics::Attempt,
    termination: Arc<AtomicBool>,
    #[cfg(windows)]
    job: Option<std::os::windows::io::OwnedHandle>,
}
impl Process {
    pub(crate) fn terminate(&self) {
        if !self.termination.swap(true, Ordering::AcqRel) { self.trace.event("Local cleanup", "Stopping the owned SSH/job transport"); }
        #[cfg(windows)]
        if let Some(job) = &self.job { console::terminate_job(job); }
        // No wait for SSH, its PTY writer, or an acknowledgement on a dead link.
        if let Ok(mut killer) = self.killer.try_lock() { let _ = killer.kill(); }
    }
}
impl Drop for Process {
    fn drop(&mut self) {
        if !self.termination.swap(true, Ordering::AcqRel) { self.trace.event("Local cleanup", "Transport owner closed"); }
        #[cfg(windows)]
        if let Some(job) = &self.job {
            console::terminate_job(job);
        }
        if let Ok(killer) = self.killer.get_mut() {
            let _ = killer.kill();
        }
    }
}

#[derive(Clone)]
struct SharedWriter(Arc<Mutex<Box<dyn LoginWriter>>>);
impl Write for SharedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.lock().unwrap().flush()
    }
}

fn discover(
    mut reader: impl Read + Send + 'static,
    writer: SharedWriter,
    output: mpsc::SyncSender<Vec<u8>>,
    wake: &UiWake,
    attached: Arc<AtomicBool>,
    trace: &super::diagnostics::Attempt,
) -> io::Result<client::Client> {
    let mut buffer = [0; 4096];
    let mut line = Vec::new();
    let mut oversized = false;
    loop {
        let len = match reader.read(&mut buffer) {
            Ok(len) => len,
            Err(error) => { trace.output("Transport output", &line); return Err(error); }
        };
        if len == 0 {
            trace.output("Transport output", &line);
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "SSH session ended before the headless service started",
            ));
        }
        for (index, &byte) in buffer[..len].iter().enumerate() {
            if !oversized {
                line.push(byte);
            }
            if line.len() > wire::MAX_LINE {
                line.clear();
                oversized = true;
            }
            if byte == b'\n' {
                if !oversized && let Some(hello) = client::Client::parse_hello(&line)? {
                    trace.event("Handshake", &format!("Headless {} (protocol {}), workspace {}", hello.app_version, hello.version, hello.root_uri));
                    attached.store(true, Ordering::Release);
                    let remainder = io::Cursor::new(buffer[index + 1..len].to_vec()).chain(reader);
                    return client::Client::attach_with_diagnostics(BufReader::new(remainder), writer, hello, Some(trace.clone()));
                }
                if !oversized { trace.output("Transport output", &line); }
                line.clear();
                oversized = false;
            }
        }
        output
            .send(buffer[..len].to_vec())
            .map_err(|_| io::Error::other("Remote setup cancelled"))?;
        wake.notify();
    }
}

pub(crate) struct Setup {
    owned: Arc<Process>,
    incoming: mpsc::Receiver<Vec<u8>>,
    endpoint: mpsc::Receiver<io::Result<client::Client>>,
    keys: mpsc::SyncSender<Input>,
    wake: UiWake,
    identity: Vec<u8>,
    recipe: Option<Arc<Recipe>>,
}
impl Setup {
    pub(crate) fn poll_client(&self) -> Option<io::Result<client::Client>> {
        match self.endpoint.try_recv() {
            Ok(client) => Some(client.map_err(|error| self.owned.trace.error(error))),
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(io::Error::other("Remote setup worker stopped"))),
            Err(mpsc::TryRecvError::Empty) => None,
        }
    }
    pub(crate) fn take_output(&self) -> Option<Vec<u8>> { self.incoming.try_recv().ok() }
    pub(crate) fn send_key(&self, key: crossterm::event::KeyEvent, grid: &crate::terminal::TermGrid) -> io::Result<()> {
        self.keys.try_send(Input::Key(key, grid.application_cursor_keys, grid.win32_input_mode)).map_err(io::Error::other)
    }
    pub(crate) fn send_paste(&self, text: &str, bracketed: bool) -> io::Result<()> {
        let bytes = if bracketed { format!("\x1b[200~{text}\x1b[201~").into_bytes() } else { text.as_bytes().to_vec() };
        self.keys.try_send(Input::Text(bytes)).map_err(io::Error::other)
    }
    pub(crate) fn terminate(&self) { self.owned.terminate(); }
    pub(crate) fn into_workspace(self, client: client::Client) -> workspace::Workspace {
        let mut workspace = workspace::Workspace::with_connection_identity(client, &self.identity);
        workspace.connection = Some(self.owned);
        workspace.recipe = self.recipe;
        workspace
    }
    #[cfg(all(test, windows))]
    pub(crate) fn workspace_for_test(self) -> io::Result<workspace::Workspace> {
        let start = std::time::Instant::now();
        loop {
            for _ in self.incoming.try_iter() {}
            if let Some(client) = self.poll_client() { return Ok(self.into_workspace(client?)); }
            if start.elapsed() > std::time::Duration::from_secs(60) { return Err(io::Error::other("Setup timed out")); }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    #[cfg(test)]
    pub(crate) fn endpoint_for_test(self) -> io::Result<(client::Client, Arc<Process>)> {
        let start = std::time::Instant::now();
        loop {
            for _ in self.incoming.try_iter() {}
            match self
                .endpoint
                .recv_timeout(std::time::Duration::from_millis(50))
            {
                Ok(client) => return Ok((client?, self.owned)),
                Err(mpsc::RecvTimeoutError::Timeout)
                    if start.elapsed() < std::time::Duration::from_secs(60) => {}
                Err(error) => {
                    return Err(io::Error::other(format!(
                        "Headless setup timed out: {error}"
                    )));
                }
            }
        }
    }
    #[cfg(all(test, windows))]
    pub(crate) fn type_for_test(&self, text: &str) -> io::Result<()> {
        self.keys
            .try_send(Input::Text(text.as_bytes().to_vec()))
            .map_err(io::Error::other)
    }
    #[cfg(all(test, windows))]
    pub(crate) fn wait_prompt_for_test(&self, text: &str) -> io::Result<()> {
        let start = std::time::Instant::now();
        let mut seen = Vec::new();
        loop {
            match self
                .incoming
                .recv_timeout(std::time::Duration::from_millis(50))
            {
                Ok(bytes) => {
                    seen.extend_from_slice(&bytes);
                    if String::from_utf8_lossy(&seen).contains(text) {
                        return Ok(());
                    }
                    if seen.len() > 128 * 1024 {
                        seen.drain(..64 * 1024);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout)
                    if start.elapsed() < std::time::Duration::from_secs(20) => {}
                Err(error) => {
                    return Err(io::Error::other(format!(
                        "SSH prompt did not arrive: {error}"
                    )));
                }
            }
        }
    }
    pub(crate) fn wait<B: ratatui::backend::Backend>(
        self,
        terminal: &mut ratatui::Terminal<B>,
        events: &mut TerminalEventPump,
        colors: &crate::app_theme::ThemeColors,
    ) -> io::Result<Option<workspace::Workspace>> {
        let size = terminal.get_frame().area().as_size();
        let mut grid = crate::terminal::TermGrid::new(
            size.width as usize,
            size.height.saturating_sub(2).max(2) as usize,
        );
        let mut parser = vte::Parser::new();
        let mut rows = crate::ui_terminal_rows::RowCache::default();
        let mut dirty = true;
        let mut has_output = false;
        loop {
            let observed = self.wake.generation();
            match self.poll_client() {
                Some(Ok(client)) => {
                    return Ok(Some(self.into_workspace(client)));
                }
                Some(Err(error)) => {
                    return Err(io::Error::new(error.kind(), format!("{error}\n\n{}", self.owned.trace.log.report())));
                }
                None => {}
            }
            let mut output_count = 0;
            for _ in 0..16 {
                let Ok(bytes) = self.incoming.try_recv() else {
                    break;
                };
                for byte in bytes {
                    parser.advance(&mut grid, byte);
                }
                output_count += 1;
                dirty = true;
            }
            if !has_output && output_count > 0 {
                has_output = grid
                    .cells
                    .iter()
                    .flatten()
                    .any(|cell| !cell.ch.is_whitespace());
            }
            let mut input_count = 0;
            for _ in 0..64 {
                let Some(event) = events.try_recv()? else {
                    break;
                };
                if let Some(size) = crate::terminal_output::apply_resize(terminal, &event)
                    .map_err(|error| io::Error::other(error.to_string()))?
                {
                    grid.resize(size.width.max(2) as usize, size.height.saturating_sub(2).max(2) as usize);
                    self.owned.master.lock().unwrap().resize(portable_pty::PtySize {
                        cols: size.width.max(2), rows: size.height.saturating_sub(2).max(2),
                        pixel_width: 0, pixel_height: 0,
                    }).map_err(io::Error::other)?;
                    dirty = true;
                }
                input_count += 1;
                match event {
                    Event::Key(key)
                        if key.code == KeyCode::Esc && key.modifiers == KeyModifiers::NONE =>
                    {
                        return Ok(None);
                    }
                    Event::Key(key) => {
                        self.keys.try_send(Input::Key(key, grid.application_cursor_keys, grid.win32_input_mode)).map_err(|_| io::Error::other("Login input is congested; connection cancelled without starting the editor"))?;
                    }
                    Event::Paste(text) => {
                        let bytes = if grid.bracketed_paste {
                            format!("\x1b[200~{text}\x1b[201~").into_bytes()
                        } else {
                            text.into_bytes()
                        };
                        self.keys
                            .try_send(Input::Text(bytes))
                            .map_err(|_| io::Error::other("Login paste queue is full"))?;
                    }
                    Event::Resize(..) => {}
                    _ => {}
                }
            }
            if dirty {
                terminal
                    .draw(|frame| draw_setup(frame, &grid, has_output, colors, &mut rows))
                    .map_err(|e| io::Error::other(e.to_string()))?;
                dirty = false;
            }
            // Reaching a drain budget can leave data queued from before
            // `observed`. Its producer has already notified us: waiting again
            // would freeze the shell until an unrelated key/output arrives.
            // Give each queue another turn without adding an idle timer.
            if output_count < 16 && input_count < 64 {
                self.wake.wait_for_change(observed, None);
            }
        }
    }
}

fn draw_setup(
    frame: &mut ratatui::Frame,
    grid: &crate::terminal::TermGrid,
    has_output: bool,
    colors: &crate::app_theme::ThemeColors,
    rows: &mut crate::ui_terminal_rows::RowCache,
) {
    use ratatui::{
        layout::Rect,
        style::Style,
        widgets::Paragraph,
    };
    let area = frame.area();
    frame.buffer_mut().set_style(area, Style::default().fg(colors.ui.fg_terminal).bg(colors.ui.bg_terminal));
    frame.render_widget(
        Paragraph::new("Remote connection · Esc to cancel")
            .style(Style::default().fg(colors.ui.fg_title).bg(colors.ui.bg_title)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    rows.paint(frame.buffer_mut(), grid, Rect::new(
            area.x,
            area.y.saturating_add(1),
            area.width,
            area.height.saturating_sub(2),
        ), &colors.ui);
    if !has_output && area.height > 3 {
        frame.render_widget(
            Paragraph::new(
                "Waiting for connection output…\nSSH authentication and remote launch messages appear here.\nThe service uses this SSH connection; no second terminal is needed.",
            )
            .style(Style::default().fg(colors.ui.description_color(colors.ui.bg_terminal)).bg(colors.ui.bg_terminal))
            .wrap(ratatui::widgets::Wrap { trim: false }),
            Rect::new(
                area.x,
                area.y.saturating_add(2),
                area.width,
                area.height.saturating_sub(3),
            ),
        );
    }
    frame.render_widget(
        Paragraph::new("Use this window for SSH authentication · Waiting for remote workspace")
            .style(Style::default().fg(colors.fg_statusbar).bg(colors.bg_statusbar)),
        Rect::new(area.x, area.bottom().saturating_sub(1), area.width, 1),
    );
    if area.height > 2 && grid.cursor_visible {
        frame.set_cursor_position((
            grid.cursor_col.min(area.width.saturating_sub(1) as usize) as u16,
            (grid.cursor_row as u16)
                .saturating_add(1)
                .min(area.bottom().saturating_sub(2)),
        ));
    }
}

#[cfg(test)]
#[path = "remote_launch_tests.rs"]
mod tests;
