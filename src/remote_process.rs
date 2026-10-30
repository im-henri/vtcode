//! Remote tools and terminals share the workspace connection. Per-process
//! credits stop a verbose tool at its pipe/PTY, rather than blocking filesystem
//! replies or accumulating unlimited output in the client's reader.
use super::*;
use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::{Arc, Condvar, Mutex, mpsc},
};

/// Stdin credit and the output window used by older protocol-20 clients.
pub(crate) const WINDOW: usize = 64 * 1024;
/// Enough output in flight to avoid a credit round trip every 64 KiB. Still
/// bounded per process, including terminals whose tabs are hidden or stalled.
pub(crate) const OUTPUT_WINDOW: usize = 1024 * 1024;
const BLOCK: usize = 4096;
const OUTPUT_BLOCK: usize = 32 * 1024;
const MAX_PROCESSES: usize = 16;
type Output = mpsc::SyncSender<Response>;

struct Flow {
    credit: usize,
    window: usize,
    closed: bool,
}
type Credits = Arc<(Mutex<Flow>, Condvar)>;
enum Child {
    Pipes(vtcode::acp::LocalProcess),
    Terminal(Box<dyn portable_pty::Child + Send + Sync>, bool),
}
impl Child {
    fn kill(&mut self) {
        match self {
            Self::Pipes(child) => child.kill(),
            Self::Terminal(child, killed) => {
                if !*killed {
                    *killed = true;
                    #[cfg(unix)]
                    if let Some(pid) = child
                        .process_id()
                        .filter(|pid| *pid > 1 && *pid <= i32::MAX as u32)
                    {
                        // portable-pty starts the owned, still-unreaped child in
                        // a new session/group. Include its shell-command children.
                        unsafe {
                            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
                        }
                    }
                    let _ = child.kill();
                }
            }
        }
    }
    fn wait(&mut self) {
        match self {
            Self::Pipes(child) => {
                let _ = child.child.wait();
            }
            Self::Terminal(child, _) => {
                let _ = child.wait();
            }
        }
    }
}
struct Running {
    child: Arc<Mutex<Child>>,
    input: Arc<(Mutex<InputQueue>, Condvar)>,
    flow: Credits,
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    _shell_integration: crate::terminal_shell::Integration,
}
impl Drop for Running {
    fn drop(&mut self) {
        self.input.0.lock().unwrap().closed = true;
        self.input.1.notify_all();
        self.flow.0.lock().unwrap().closed = true;
        self.flow.1.notify_all();
        let mut child = self.child.lock().unwrap();
        child.kill();
        child.wait();
    }
}
pub(crate) struct Manager {
    running: HashMap<u64, Running>,
    output: Output,
    lifetime: Ownership,
}
#[derive(Clone, Default)]
pub(crate) struct Ownership(Arc<Mutex<Lifetime>>);
#[derive(Default)]
struct Lifetime {
    stopped: bool,
    children: Vec<std::sync::Weak<Mutex<Child>>>,
    native: Vec<vtcode::acp::ProcessKiller>,
}
impl Ownership {
    pub(crate) fn track(&self, killer: vtcode::acp::ProcessKiller) {
        let mut state = self.0.lock().unwrap();
        if state.stopped {
            drop(state);
            killer.kill();
        } else {
            state.native.retain(|process| process.active());
            state.native.push(killer);
        }
    }
    pub(crate) fn shutdown_guard(&self) -> Shutdown {
        Shutdown(self.clone())
    }
}
pub(crate) struct Shutdown(Ownership);
impl Drop for Shutdown {
    fn drop(&mut self) {
        let (children, native) = {
            let mut state = self.0.0.lock().unwrap_or_else(|error| error.into_inner());
            state.stopped = true;
            (
                std::mem::take(&mut state.children),
                std::mem::take(&mut state.native),
            )
        };
        for process in native {
            process.kill();
        }
        // Do not wait for actor/channel teardown or a tool's output consumer.
        for child in children.into_iter().filter_map(|child| child.upgrade()) {
            child
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .kill();
        }
    }
}
impl Manager {
    pub(crate) fn new(output: Output, lifetime: Ownership) -> Self {
        Self {
            running: HashMap::new(),
            output,
            lifetime,
        }
    }
    pub(crate) fn spawn(
        &mut self,
        id: u64,
        program: &str,
        args: &[String],
        cwd: &Path,
        terminal: Option<(u16, u16)>,
        output_window: usize,
    ) -> io::Result<()> {
        let window = output_window.max(WINDOW);
        if window > OUTPUT_WINDOW {
            return Err(io::Error::other("Remote process output window exceeds limit"));
        }
        if self.lifetime.0.lock().unwrap().stopped {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Remote service is stopping",
            ));
        }
        let shell;
        let program = if program.is_empty() && terminal.is_some() {
            shell = std::env::var(if cfg!(windows) { "COMSPEC" } else { "SHELL" }).unwrap_or_else(
                |_| {
                    if cfg!(windows) {
                        "cmd.exe".into()
                    } else {
                        "/bin/sh".into()
                    }
                },
            );
            shell.as_str()
        } else {
            program
        };
        self.running
            .retain(|_, running| !running.flow.0.lock().unwrap().closed);
        if self.running.len() >= MAX_PROCESSES || self.running.contains_key(&id) {
            return Err(io::Error::other("Remote process limit or duplicate ID"));
        }
        let integration = if terminal.is_some() {
            crate::terminal_shell::Integration::prepare(program, args).unwrap_or_default()
        } else {
            Default::default()
        };
        let (child, reader, writer, master): (
            Child,
            Box<dyn Read + Send>,
            Box<dyn Write + Send>,
            _,
        ) = if let Some((width, height)) = terminal {
            let pair = portable_pty::native_pty_system()
                .openpty(portable_pty::PtySize {
                    cols: width.max(2),
                    rows: height.max(2),
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(io::Error::other)?;
            let mut command = portable_pty::CommandBuilder::new(program);
            command.args(args);
            integration.apply(&mut command);
            command.cwd(cwd);
            command.env("TERM", "xterm-256color");
            command.env("TERM_PROGRAM", "vtcode");
            let child = pair
                .slave
                .spawn_command(command)
                .map_err(io::Error::other)?;
            drop(pair.slave);
            let reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
            let writer = pair.master.take_writer().map_err(io::Error::other)?;
            (
                Child::Terminal(child, false),
                reader,
                writer,
                Some(pair.master),
            )
        } else {
            let mut command = std::process::Command::new(program);
            command
                .args(args)
                .current_dir(cwd)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
            }
            let mut child = vtcode::acp::LocalProcess::spawn(&mut command)?;
            let reader = Box::new(child.child.stdout.take().unwrap()) as Box<dyn Read + Send>;
            let writer = Box::new(child.child.stdin.take().unwrap()) as Box<dyn Write + Send>;
            (Child::Pipes(child), reader, writer, None)
        };
        let child = Arc::new(Mutex::new(child));
        {
            let mut state = self.lifetime.0.lock().unwrap();
            if state.stopped {
                drop(state);
                child.lock().unwrap().kill();
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "Remote service stopped while starting tool",
                ));
            }
            state.children.retain(|child| child.strong_count() != 0);
            state.children.push(Arc::downgrade(&child));
        }
        let flow = Arc::new((
            Mutex::new(Flow {
                credit: window,
                window,
                closed: false,
            }),
            Condvar::new(),
        ));
        let input = Arc::new((Mutex::new(InputQueue::default()), Condvar::new()));
        let received = input.clone();
        let running = Running {
            child: child.clone(),
            input,
            flow: flow.clone(),
            master,
            _shell_integration: integration,
        };
        let input_child = child.clone();
        let input_output = self.output.clone();
        std::thread::Builder::new()
            .name("remote-tool-input".into())
            .spawn(move || {
                let mut writer = writer;
                while let Some(bytes) = InputQueue::receive(&received) {
                    if writer
                        .write_all(&bytes)
                        .and_then(|_| writer.flush())
                        .is_err()
                    {
                        input_child.lock().unwrap().kill();
                        break;
                    }
                    if input_output
                        .send(Response {
                            id: 0,
                            result: Ok(Reply::ProcessInputConsumed {
                                process: id,
                                bytes: bytes.len(),
                            }),
                        })
                        .is_err()
                    {
                        input_child.lock().unwrap().kill();
                        break;
                    }
                }
            })?;
        let output = self.output.clone();
        let output_flow = flow.clone();
        std::thread::Builder::new()
            .name("remote-tool-output".into())
            .spawn(move || {
                let block = if window == WINDOW { BLOCK } else { OUTPUT_BLOCK };
                let result = stream_output(id, reader, &output_flow, &output, block);
                output_flow.0.lock().unwrap().closed = true;
                output_flow.1.notify_all();
                let _ = output.send(Response {
                    id: 0,
                    result: Ok(Reply::ProcessEnd {
                        process: id,
                        error: result.err().map(|error| error.to_string()),
                    }),
                });
            })?;
        self.running.insert(id, running);
        Ok(())
    }
    pub(crate) fn input(&self, id: u64, data: &str) -> io::Result<()> {
        let bytes = URL_SAFE_NO_PAD.decode(data).map_err(io::Error::other)?;
        if bytes.len() > BLOCK {
            return Err(io::Error::other("Remote process input block exceeds limit"));
        }
        let running = self
            .running
            .get(&id)
            .ok_or_else(|| io::Error::other("Remote process not found"))?;
        let mut input = running.input.0.lock().unwrap();
        // Input credit counts bytes, not messages: a burst of small keystrokes
        // must not exhaust a sixteen-message queue and terminate the process.
        input.push(bytes, WINDOW)?;
        drop(input);
        running.input.1.notify_one();
        Ok(())
    }
    pub(crate) fn credit(&self, id: u64, bytes: usize) -> io::Result<()> {
        let Some(running) = self.running.get(&id) else {
            return Ok(());
        };
        let mut flow = running.flow.0.lock().unwrap();
        if bytes > flow.window || flow.credit.saturating_add(bytes) > flow.window {
            return Err(io::Error::other("Invalid remote process credit"));
        }
        flow.credit += bytes;
        drop(flow);
        running.flow.1.notify_one();
        Ok(())
    }
    pub(crate) fn resize(&mut self, id: u64, width: u16, height: u16) -> io::Result<()> {
        if let Some(master) = self
            .running
            .get_mut(&id)
            .and_then(|running| running.master.as_mut())
        {
            master
                .resize(portable_pty::PtySize {
                    cols: width.max(2),
                    rows: height.max(2),
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .map_err(io::Error::other)?;
        }
        Ok(())
    }
    pub(crate) fn kill(&mut self, id: u64) {
        self.running.remove(&id);
    }
}

fn stream_output(
    id: u64,
    mut reader: impl Read,
    flow: &Credits,
    output: &Output,
    block: usize,
) -> io::Result<()> {
    let mut buffer = vec![0; block];
    loop {
        let count = match reader.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            return Ok(());
        }
        let mut ready = flow.0.lock().unwrap();
        while ready.credit < count && !ready.closed {
            ready = flow.1.wait(ready).unwrap();
        }
        if ready.closed {
            return Ok(());
        }
        ready.credit -= count;
        drop(ready);
        output
            .send(Response {
                id: 0,
                result: Ok(Reply::ProcessData {
                    process: id,
                    data: URL_SAFE_NO_PAD.encode(&buffer[..count]),
                }),
            })
            .map_err(io::Error::other)?;
    }
}

/// Owned local endpoint. Reading waits only on a tool/terminal worker; writes
/// enqueue bounded messages and never wait for the server to consume stdin.
pub(crate) struct RemoteProcess {
    pub client: client::Client,
    pub id: u64,
    pub reader: Reader,
}
pub(crate) struct Reader {
    pub(super) stream: Arc<Stream>,
    pub(super) id: u64,
    pub(super) client: client::Client,
}
pub(super) struct Stream {
    pub state: Mutex<StreamState>,
    pub ready: Condvar,
    pub wake: crate::background::UiWake,
}
pub(super) struct StreamState {
    pub chunks: std::collections::VecDeque<Vec<u8>>,
    pub bytes: usize,
    pub offset: usize,
    pub end: bool,
    pub error: Option<String>,
    pub started: bool,
    pub input_credit: usize,
}
impl Stream {
    pub(super) fn new(wake: crate::background::UiWake) -> Self {
        Self {
            state: Mutex::new(StreamState {
                chunks: Default::default(),
                bytes: 0,
                offset: 0,
                end: false,
                error: None,
                started: false,
                input_credit: WINDOW,
            }),
            ready: Condvar::new(),
            wake,
        }
    }
    pub(super) fn push(&self, data: Vec<u8>) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.bytes + data.len() > OUTPUT_WINDOW {
            return Err(io::Error::other("Remote process exceeded output credit"));
        }
        state.bytes += data.len();
        state.chunks.push_back(data);
        drop(state);
        self.ready.notify_all();
        self.wake.notify();
        Ok(())
    }
    pub(super) fn end(&self, error: Option<String>) {
        let mut state = self.state.lock().unwrap();
        state.end = true;
        state.error = error;
        drop(state);
        self.ready.notify_all();
        self.wake.notify();
    }
}
impl Read for Reader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.read_inner(out, true)
    }
}
impl Reader {
    pub(crate) fn ready(&self) -> bool {
        let state = self.stream.state.lock().unwrap();
        state.bytes != 0 || state.end
    }
    pub(crate) fn try_read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.read_inner(out, false)
    }
    fn read_inner(&mut self, out: &mut [u8], wait: bool) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        let mut state = self.stream.state.lock().unwrap();
        while state.chunks.is_empty() && !state.end {
            if !wait {
                return Err(io::Error::from(io::ErrorKind::WouldBlock));
            }
            state = self.stream.ready.wait(state).unwrap();
        }
        if let Some(chunk) = state.chunks.front() {
            let chunk_len = chunk.len();
            let count = (chunk_len - state.offset).min(out.len());
            out[..count].copy_from_slice(&chunk[state.offset..state.offset + count]);
            state.offset += count;
            if state.offset == chunk_len {
                state.chunks.pop_front();
                state.offset = 0;
            }
            state.bytes -= count;
            drop(state);
            self.client.credit(self.id, count);
            Ok(count)
        } else if let Some(error) = state.error.take() {
            Err(io::Error::other(error))
        } else {
            Ok(0)
        }
    }
}

pub(crate) struct Writer {
    pub client: client::Client,
    pub id: u64,
    stream: Arc<Stream>,
}
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let count = bytes.len().min(BLOCK);
        if count == 0 {
            return Ok(0);
        }
        let mut state = self.stream.state.lock().unwrap();
        while (!state.started || state.input_credit < count) && !state.end {
            state = self.stream.ready.wait(state).unwrap();
        }
        if state.end {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Remote process stopped",
            ));
        }
        state.input_credit -= count;
        drop(state);
        if let Err(error) = self.client.notify_from_worker(Command::ProcessInput {
            process: self.id,
            data: URL_SAFE_NO_PAD.encode(&bytes[..count]),
        }) {
            self.stream.end(Some(error.to_string()));
            return Err(error);
        }
        Ok(count)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) struct Killer {
    pub client: client::Client,
    pub id: u64,
    pub(super) stream: Arc<Stream>,
}
impl Killer {
    pub(crate) fn ended(&self) -> bool {
        !self.client.connected() || self.stream.state.lock().unwrap().end
    }
    pub(crate) fn kill(&self) {
        self.client.close_process(self.id);
    }
}
impl Drop for Killer {
    fn drop(&mut self) {
        self.kill();
    }
}
impl RemoteProcess {
    pub(crate) fn split(self) -> (Reader, Writer, Killer) {
        let writer = Writer {
            client: self.client.clone(),
            id: self.id,
            stream: self.reader.stream.clone(),
        };
        let killer = Killer {
            client: self.client,
            id: self.id,
            stream: self.reader.stream.clone(),
        };
        (self.reader, writer, killer)
    }
}

/// UI-facing stdin queue. The pipe writer waits for remote credits on its own
/// thread; a congested terminal never blocks editor input or rendering.
pub(crate) struct QueuedWriter {
    queue: Arc<(Mutex<InputQueue>, Condvar)>,
}
#[derive(Default)]
struct InputQueue {
    chunks: std::collections::VecDeque<Vec<u8>>,
    bytes: usize,
    closed: bool,
}
impl InputQueue {
    fn push(&mut self, bytes: Vec<u8>, limit: usize) -> io::Result<()> {
        if self.closed || self.bytes.saturating_add(bytes.len()) > limit {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "Remote terminal input queue is full",
            ));
        }
        self.bytes += bytes.len();
        // Coalesce only bytes already waiting. Never delay a key in the hope
        // that more input will arrive, or rewrite escape/paste sequences.
        if let Some(last) = self.chunks.back_mut()
            && last.len().saturating_add(bytes.len()) <= BLOCK
        {
            last.extend_from_slice(&bytes);
        } else if !bytes.is_empty() {
            self.chunks.push_back(bytes);
        }
        Ok(())
    }
    fn receive(queue: &Arc<(Mutex<Self>, Condvar)>) -> Option<Vec<u8>> {
        let mut state = queue.0.lock().unwrap();
        while state.chunks.is_empty() && !state.closed {
            state = queue.1.wait(state).unwrap();
        }
        if state.closed {
            return None;
        }
        let bytes = state.chunks.pop_front().unwrap();
        state.bytes -= bytes.len();
        Some(bytes)
    }
}
impl QueuedWriter {
    pub(crate) fn new(mut writer: Writer) -> io::Result<Self> {
        let queue = Arc::new((Mutex::new(InputQueue::default()), Condvar::new()));
        let worker = queue.clone();
        let stream = writer.stream.clone();
        std::thread::Builder::new()
            .name("remote-terminal-input".into())
            .spawn(move || {
                while let Some(bytes) = InputQueue::receive(&worker) {
                    if let Err(error) = writer.write_all(&bytes) {
                        stream.end(Some(error.to_string()));
                        return;
                    }
                }
            })?;
        Ok(Self { queue })
    }
}
impl Write for QueuedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut queue = self.queue.0.lock().unwrap();
        queue.push(bytes.to_vec(), 4 * 1024 * 1024)?;
        drop(queue);
        self.queue.1.notify_one();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Drop for QueuedWriter {
    fn drop(&mut self) {
        self.queue.0.lock().unwrap().closed = true;
        self.queue.1.notify_all();
    }
}

#[cfg(test)]
#[path = "remote_process_tests.rs"]
mod tests;
