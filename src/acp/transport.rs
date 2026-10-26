use super::{
    AgentConfig, FileEdit, FileRequest, ProtocolVersion, Workspace, files::failure,
    process::Process, protocol as p,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Duration, Instant};

#[path = "terminals.rs"]
mod terminals;
use terminals::Terminal;
#[path = "history.rs"]
mod history;

const MAX_FRAME: usize = 8 * 1024 * 1024;
const MAX_PENDING: usize = 128;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Command {
    NewSession,
    LoadSession(p::SessionId),
    ResumeSession(p::SessionId),
    ListSessions {
        cursor: Option<String>,
    },
    Authenticate(p::AuthMethodId),
    Prompt {
        session_id: p::SessionId,
        content: Vec<p::ContentBlock>,
    },
    SetMode {
        session_id: p::SessionId,
        mode_id: p::SessionModeId,
    },
    SetConfig {
        session_id: p::SessionId,
        config_id: p::SessionConfigId,
        value: p::SessionConfigOptionValue,
    },
    Cancel(p::SessionId),
    CloseSession(p::SessionId),
    DeleteSession(p::SessionId),
    Permission {
        id: p::RequestId,
        ticket: u64,
        option: Option<p::PermissionOptionId>,
    },
    FileResponse {
        id: p::RequestId,
        ticket: u64,
        result: Result<Value, p::Error>,
        edit: Option<FileEdit>,
    },
    TerminalInput {
        session_id: p::SessionId,
        terminal_id: String,
        text: String,
    },
    KillTerminal {
        session_id: p::SessionId,
        terminal_id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operation {
    Initialize,
    NewSession,
    LoadSession,
    ResumeSession,
    ListSessions,
    Authenticate,
    Prompt,
    SetMode,
    SetConfig,
    CloseSession,
    DeleteSession,
    Terminal,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TerminalSnapshot {
    pub session_id: p::SessionId,
    pub id: String,
    pub command: String,
    pub output: String,
    pub truncated: bool,
    pub exit_status: Option<p::TerminalExitStatus>,
    pub released: bool,
    pub cwd: PathBuf,
    #[serde(skip)]
    pub history: Option<Arc<super::terminal_history::TerminalHistory>>,
    pub history_version: u64,
    pub killed: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Event {
    Initialized(Box<p::InitializeResponse>),
    Authenticated,
    SessionOpened {
        session_id: p::SessionId,
        modes: Option<p::SessionModeState>,
        config_options: Vec<p::SessionConfigOption>,
        loaded: bool,
    },
    SessionsListed(p::ListSessionsResponse),
    SessionClosed(p::SessionId),
    SessionDeleted(p::SessionId),
    Update(p::SessionNotification),
    ModeChanged {
        session_id: p::SessionId,
        mode_id: p::SessionModeId,
    },
    ConfigChanged {
        session_id: p::SessionId,
        options: Vec<p::SessionConfigOption>,
    },
    PromptFinished {
        session_id: p::SessionId,
        response: p::PromptResponse,
    },
    Permission {
        id: p::RequestId,
        ticket: u64,
        request: p::RequestPermissionRequest,
    },
    /// Emitted after a live permission response is accepted by the transport.
    PermissionResponded {
        id: p::RequestId,
        ticket: u64,
        option: p::PermissionOptionId,
    },
    PermissionResolved(p::RequestId, u64),
    FileRequest {
        id: p::RequestId,
        ticket: u64,
        request: FileRequest,
    },
    FileEdited(FileEdit),
    Terminal(TerminalSnapshot),
    Diagnostic(String),
    Error {
        operation: Operation,
        session_id: Option<p::SessionId>,
        error: p::Error,
    },
    Disconnected(String),
}

/// Cloneable command endpoint. Enqueueing never blocks the UI thread.
pub trait CommandSink: Send + Sync {
    fn send(&self, command: Command, blocking: bool) -> Result<(), String>;
    /// This must only signal teardown; hosts call it on their input thread.
    fn disconnect(&self);
    fn file_request_live(&self, _id: &p::RequestId, _ticket: u64) -> bool { true }
}

/// A hosted event owns its delivery credit until the UI consumes or drops it.
/// Receipt callbacks must enqueue control work only, without waiting on I/O.
pub struct HostedEvent {
    event: Option<Event>,
    receipt: Option<Box<dyn FnOnce() + Send>>,
}
impl HostedEvent {
    pub fn new(event: Event, receipt: impl FnOnce() + Send + 'static) -> Self {
        Self { event: Some(event), receipt: Some(Box::new(receipt)) }
    }
    fn take(mut self) -> Event {
        if let Some(receipt) = self.receipt.take() { receipt(); }
        self.event.take().unwrap()
    }
}
impl Drop for HostedEvent {
    fn drop(&mut self) { if let Some(receipt) = self.receipt.take() { receipt(); } }
}
enum Inbox { Local(mpsc::Receiver<Event>), Hosted(mpsc::Receiver<HostedEvent>) }
impl Inbox {
    fn try_recv(&self) -> Option<Event> {
        match self { Self::Local(events) => events.try_recv().ok(), Self::Hosted(events) => events.try_recv().ok().map(HostedEvent::take) }
    }
    fn recv(&self) -> Result<Event, mpsc::RecvError> {
        match self { Self::Local(events) => events.recv(), Self::Hosted(events) => events.recv().map(HostedEvent::take) }
    }
    fn recv_timeout(&self, timeout: Duration) -> Result<Event, mpsc::RecvTimeoutError> {
        match self { Self::Local(events) => events.recv_timeout(timeout), Self::Hosted(events) => events.recv_timeout(timeout).map(HostedEvent::take) }
    }
}

#[derive(Clone)]
enum Endpoint {
    Local(mpsc::SyncSender<Work>),
    Hosted(Arc<dyn CommandSink>),
}

#[derive(Clone)]
pub struct ClientHandle {
    endpoint: Endpoint,
    lifetime: Option<Arc<Lifetime>>,
}
pub(super) struct Lifetime {
    stopped: Arc<AtomicBool>,
    processes: std::sync::Mutex<Vec<super::process::ProcessKiller>>,
    files: std::sync::Mutex<HashMap<p::RequestId, u64>>,
}
impl Lifetime {
    pub(super) fn register(&self, process: &Process) {
        let killer = process.killer();
        let mut processes = self.processes.lock().unwrap_or_else(|e| e.into_inner());
        processes.retain(super::process::ProcessKiller::active);
        processes.push(killer.clone());
        if self.stopped.load(Ordering::Acquire) { killer.kill(); }
    }
    fn terminate(&self) {
        self.stopped.store(true, Ordering::Release);
        for process in &*self.processes.lock().unwrap_or_else(|e| e.into_inner()) { process.kill(); }
    }
}

impl ClientHandle {
    pub fn same_connection(&self, other: &Self) -> bool {
        match (&self.endpoint, &other.endpoint) {
            (Endpoint::Hosted(a), Endpoint::Hosted(b)) => Arc::ptr_eq(a, b),
            (Endpoint::Local(_), Endpoint::Local(_)) => match (&self.lifetime, &other.lifetime) { (Some(a), Some(b)) => Arc::ptr_eq(a, b), _ => false },
            _ => false,
        }
    }
    pub fn file_request_live(&self, id: &p::RequestId, ticket: u64) -> bool {
        match &self.endpoint {
            Endpoint::Hosted(sink) => sink.file_request_live(id, ticket),
            Endpoint::Local(_) => self.lifetime.as_ref().is_some_and(|lifetime| !lifetime.stopped.load(Ordering::Acquire) && lifetime.files.lock().unwrap().get(id) == Some(&ticket)),
        }
    }
    /// Signal shutdown without waiting for a worker or a remote connection.
    pub fn stop(&self) {
        match &self.endpoint {
            Endpoint::Hosted(sink) => sink.disconnect(),
            Endpoint::Local(sender) => {
                self.lifetime.as_ref().unwrap().stopped.store(true, Ordering::Release);
                let _ = sender.try_send(Work::Shutdown);
            }
        }
    }
    /// Service teardown fallback, including when a worker is blocked on I/O.
    pub fn terminate(&self) {
        self.stop();
        if let Some(lifetime) = &self.lifetime { lifetime.terminate(); }
    }
    /// For host background workers only. UI callers must use `send`.
    pub fn send_blocking(&self, command: Command) -> Result<(), String> {
        let Endpoint::Local(sender) = &self.endpoint else {
            let Endpoint::Hosted(sink) = &self.endpoint else { unreachable!() };
            return sink.send(command, true);
        };
        sender
            .send(Work::Command(command))
            .map_err(|_| "The agent is disconnected".into())
    }
    pub fn send(&self, command: Command) -> Result<(), String> {
        let Endpoint::Local(sender) = &self.endpoint else {
            let Endpoint::Hosted(sink) = &self.endpoint else { unreachable!() };
            return sink.send(command, false);
        };
        sender
            .try_send(Work::Command(command))
            .map_err(|e| match e {
                mpsc::TrySendError::Full(_) => {
                    "The agent is busy processing updates; try again".into()
                }
                mpsc::TrySendError::Disconnected(_) => "The agent is disconnected".into(),
            })
    }
}

pub struct Client {
    handle: ClientHandle,
    events: Inbox,
    finished: Option<mpsc::Receiver<()>>,
    workspace: Workspace,
}

impl Client {
    /// Adapt a hosted transport without constructing a process, canonicalizing
    /// a client-native path, or doing any blocking work on the host's UI.
    pub fn hosted(workspace: Workspace, sink: Arc<dyn CommandSink>, events: mpsc::Receiver<HostedEvent>) -> Self {
        Self { handle: ClientHandle { endpoint: Endpoint::Hosted(sink), lifetime: None }, events: Inbox::Hosted(events),
            finished: None, workspace }
    }
    pub fn connect(
        config: AgentConfig,
        root: &Path,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        let executable = std::env::current_exe().map_err(|e| e.to_string())?;
        let logs = executable
            .parent()
            .ok_or("Cannot locate terminal storage")?
            .join(".vtcode/agent-terminals");
        Self::connect_with_terminal_storage(config, root, &logs, wake)
    }

    pub fn connect_with_terminal_storage(
        config: AgentConfig,
        root: &Path,
        terminal_storage: &Path,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> Result<Self, String> {
        Self::connect_inner(config, root, terminal_storage, wake, 256)
    }

    /// A headless host drains into its own bounded transport. Small queues
    /// propagate backpressure to the agent instead of retaining large events.
    pub fn connect_hosted(config: AgentConfig, root: &Path, terminal_storage: &Path,
        wake: impl Fn() + Send + Sync + 'static) -> Result<Self, String> {
        Self::connect_inner(config, root, terminal_storage, wake, 8)
    }

    fn connect_inner(config: AgentConfig, root: &Path, terminal_storage: &Path,
        wake: impl Fn() + Send + Sync + 'static, capacity: usize) -> Result<Self, String> {
        config.validate()?;
        let workspace = Workspace::new(root).map_err(|e| e.message)?;
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let (event_tx, events) = mpsc::sync_channel(capacity);
        let (finished_tx, finished) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let lifetime = Arc::new(Lifetime { stopped: stopped.clone(), processes: Default::default(), files: Default::default() });
        let sink = Sink {
            sender: event_tx,
            wake: Arc::new(wake),
            stopped: stopped.clone(),
        };
        let worker_sender = sender.clone();
        let worker_workspace = workspace.clone();
        let worker_stopped = stopped.clone();
        let terminal_storage = terminal_storage.to_path_buf();
        let worker_lifetime = lifetime.clone();
        thread::Builder::new()
            .name("acp-connection".into())
            .spawn(move || {
                match Worker::start(
                    config,
                    worker_workspace,
                    worker_sender,
                    receiver,
                    sink.clone(),
                    worker_stopped,
                    terminal_storage,
                    worker_lifetime,
                ) {
                    Ok(mut worker) => {
                        let reason = worker.run();
                        if worker.stopped.load(Ordering::Acquire) {
                            worker.discard_empty_sessions();
                        }
                        worker.fail_pending(&reason);
                        // Clean up before reporting disconnection so reconnect has
                        // no overlap with the old process tree.
                        drop(worker);
                        sink.emit(Event::Disconnected(reason));
                    }
                    Err(error) => sink.emit(Event::Disconnected(error.to_string())),
                }
                let _ = finished_tx.send(());
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            handle: ClientHandle { endpoint: Endpoint::Local(sender), lifetime: Some(lifetime) },
            events: Inbox::Local(events),
            finished: Some(finished),
            workspace,
        })
    }
    pub fn handle(&self) -> ClientHandle {
        self.handle.clone()
    }
    pub fn send(&self, command: Command) -> Result<(), String> {
        self.handle.send(command)
    }
    pub fn try_recv(&self) -> Option<Event> {
        self.events.try_recv()
    }
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Event, mpsc::RecvTimeoutError> {
        self.events.recv_timeout(timeout)
    }
    pub fn recv(&self) -> Result<Event, mpsc::RecvError> { self.events.recv() }
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }
    pub fn disconnect(&self) {
        if let Endpoint::Hosted(sink) = &self.handle.endpoint {
            sink.disconnect();
            return;
        }
        self.handle.stop();
        // Give the worker a bounded chance to remove empty sessions before the
        // process exits. Event backpressure cannot hold up this shutdown path.
        if let Some(finished) = &self.finished {
            if finished.recv_timeout(Duration::from_millis(1500)).is_err() { self.handle.terminate(); }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.disconnect();
    }
}

#[derive(Clone)]
struct Sink {
    sender: mpsc::SyncSender<Event>,
    wake: Arc<dyn Fn() + Send + Sync>,
    stopped: Arc<AtomicBool>,
}
impl Sink {
    fn emit(&self, mut event: Event) {
        loop {
            match self.sender.try_send(event) {
                Ok(()) => {
                    (self.wake)();
                    return;
                }
                Err(mpsc::TrySendError::Disconnected(_)) => return,
                Err(mpsc::TrySendError::Full(pending)) => {
                    // Backpressure must not prevent disconnect from tearing
                    // down the process when a host stops draining events.
                    if self.stopped.load(Ordering::Acquire) {
                        return;
                    }
                    event = pending;
                    thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }
}

enum Work {
    Command(Command),
    Frame(Value),
    Closed(String),
    Stderr(String),
    TerminalBytes(String, Vec<u8>, bool),
    TerminalInputError(String, String),
    Shutdown,
}

struct Pending {
    operation: Operation,
    session: Option<p::SessionId>,
    value: Option<String>,
    deadline: Option<Instant>,
}

enum HostRequest {
    Permission { request: Box<p::RequestPermissionRequest>, ticket: u64 },
    File { session: p::SessionId, ticket: u64 },
}

struct Worker {
    _process: Process,
    sender: mpsc::SyncSender<Work>,
    receiver: mpsc::Receiver<Work>,
    writer: mpsc::SyncSender<String>,
    sink: Sink,
    stopped: Arc<AtomicBool>,
    workspace: Workspace,
    timeout: Duration,
    initialized: Option<p::InitializeResponse>,
    next_id: u64,
    pending: HashMap<u64, Pending>,
    host: HashMap<p::RequestId, HostRequest>,
    inbound: HashSet<p::RequestId>,
    early_updates: VecDeque<p::SessionNotification>,
    sessions: HashSet<p::SessionId>,
    empty_sessions: HashSet<p::SessionId>,
    cancelling: HashSet<p::SessionId>,
    terminals: HashMap<String, Terminal>,
    terminal_storage: PathBuf,
    next_terminal: u64,
    next_file_ticket: u64,
    stderr_tail: String,
    lifetime: Arc<Lifetime>,
}
impl Drop for Worker {
    fn drop(&mut self) { self.lifetime.files.lock().unwrap_or_else(|e| e.into_inner()).clear(); }
}

impl Worker {
    fn start(
        config: AgentConfig,
        workspace: Workspace,
        sender: mpsc::SyncSender<Work>,
        receiver: mpsc::Receiver<Work>,
        sink: Sink,
        stopped: Arc<AtomicBool>,
        terminal_storage: PathBuf,
        lifetime: Arc<Lifetime>,
    ) -> std::io::Result<Self> {
        let mut command = super::process::command(
            &config.command,
            workspace.root(),
            config
                .env
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut process = Process::spawn(&mut command).map_err(|error| {
            let hint = if error.kind() == std::io::ErrorKind::NotFound {
                " Check Executable and any PATH override in Settings > Agent Chat. Install the command or add its launcher directory to PATH, then reconnect. On Windows, .exe, .com, .cmd, and .bat suffixes are resolved automatically."
            } else { "" };
            std::io::Error::new(error.kind(), format!("Could not start agent {:?}: {error}.{hint}", config.command))
        })?;
        lifetime.register(&process);
        let stdout = process.child.stdout.take().expect("piped stdout");
        let stderr = process.child.stderr.take().expect("piped stderr");
        let mut stdin = process.child.stdin.take().expect("piped stdin");
        let (writer, lines) = mpsc::sync_channel::<String>(128);
        let failure_sender = sender.clone();
        thread::Builder::new()
            .name("acp-stdin".into())
            .spawn(move || {
                while let Ok(line) = lines.recv() {
                    if let Err(error) = stdin
                        .write_all(line.as_bytes())
                        .and_then(|_| stdin.write_all(b"\n"))
                        .and_then(|_| stdin.flush())
                    {
                        let _ = failure_sender
                            .send(Work::Closed(format!("Could not write to agent: {error}")));
                        break;
                    }
                }
            })?;
        let read_sender = sender.clone();
        thread::Builder::new()
            .name("acp-stdout".into())
            .spawn(move || read_frames(stdout, read_sender))?;
        let log_sender = sender.clone();
        thread::Builder::new()
            .name("acp-stderr".into())
            .spawn(move || {
                let mut reader = stderr;
                let mut bytes = [0; 4096];
                while let Ok(count) = reader.read(&mut bytes) {
                    if count == 0 {
                        break;
                    }
                    if log_sender
                        .send(Work::Stderr(
                            String::from_utf8_lossy(&bytes[..count]).into_owned(),
                        ))
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        let mut worker = Self {
            _process: process,
            sender,
            receiver,
            writer,
            sink,
            stopped,
            workspace,
            timeout: Duration::from_secs(config.request_timeout_secs),
            initialized: None,
            next_id: 0,
            pending: HashMap::new(),
            host: HashMap::new(),
            inbound: HashSet::new(),
            early_updates: VecDeque::new(),
            sessions: HashSet::new(),
            empty_sessions: HashSet::new(),
            cancelling: HashSet::new(),
            terminals: HashMap::new(),
            terminal_storage,
            next_terminal: 0,
            next_file_ticket: 0,
            stderr_tail: String::new(),
            lifetime,
        };
        let capabilities = p::ClientCapabilities::new()
            .fs(p::FileSystemCapabilities::new()
                .read_text_file(true)
                .write_text_file(true))
            .terminal(true)
            // Cursor's ACP adapter exposes separate effort/context selectors
            // only when this client extension is advertised. They use ordinary
            // configOptions and set_config_option, which vtcode already supports.
            .meta(serde_json::Map::from_iter([(
                "parameterizedModelPicker".into(), json!(true),
            )]))
            .session(
                p::ClientSessionCapabilities::new().config_options(
                    p::SessionConfigOptionsCapabilities::new()
                        .boolean(p::BooleanConfigOptionCapabilities::new()),
                ),
            );
        let initialize = p::InitializeRequest::new(ProtocolVersion::V1)
            .client_capabilities(capabilities)
            .client_info(p::Implementation::new("vtcode", env!("CARGO_PKG_VERSION")));
        worker
            .request("initialize", &initialize, Operation::Initialize, None, None)
            .map_err(|e| std::io::Error::other(e.message))?;
        Ok(worker)
    }

    fn run(&mut self) -> String {
        loop {
            if self.stopped.load(Ordering::Acquire) {
                return "Disconnected".into();
            }
            self.poll_terminals();
            let now = Instant::now();
            let expired: Vec<_> = self
                .pending
                .iter()
                .filter(|(_, p)| p.deadline.is_some_and(|d| d <= now))
                .map(|(id, _)| *id)
                .collect();
            for id in expired {
                if let Some(pending) = self.pending.remove(&id) {
                    let fatal =
                        matches!(pending.operation, Operation::Initialize | Operation::Prompt);
                    self.cleanup_failed(&pending);
                    self.error(
                        pending.operation,
                        pending.session,
                        failure("The agent did not respond before the request deadline"),
                    );
                    if fatal {
                        return "Agent timed out; reconnect to continue".into();
                    }
                }
            }
            let wait = if self.terminals.values().any(|terminal| !terminal.complete()) {
                Some(Duration::from_millis(20))
            } else {
                self.pending
                    .values()
                    .filter_map(|p| p.deadline)
                    .min()
                    .map(|d| d.saturating_duration_since(Instant::now()))
            };
            let work = match wait {
                Some(wait) => match self.receiver.recv_timeout(wait) {
                    Ok(work) => work,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(_) => return "Connection closed".into(),
                },
                None => match self.receiver.recv() {
                    Ok(work) => work,
                    Err(_) => return "Connection closed".into(),
                },
            };
            match work {
                Work::Shutdown => return "Disconnected".into(),
                Work::Closed(reason) => {
                    return if self.stderr_tail.trim().is_empty() {
                        reason
                    } else {
                        format!("{reason}\n{}", self.stderr_tail.trim())
                    };
                }
                Work::Stderr(text) => {
                    self.stderr_tail.push_str(&text);
                    trim_front(&mut self.stderr_tail, 8192);
                    self.sink.emit(Event::Diagnostic(text));
                }
                Work::Frame(frame) => {
                    if let Err(error) = self.frame(frame) {
                        return format!("ACP protocol error: {}", error.message);
                    }
                }
                Work::Command(command) => {
                    let (operation, session) = command_context(&command);
                    if let Err(error) = self.command(command) {
                        let broken_transport = error.code == p::ErrorCode::Other(-32099);
                        self.error(operation, session, error);
                        if broken_transport {
                            return "Agent input stopped accepting messages".into();
                        }
                    }
                }
                Work::TerminalBytes(id, bytes, eof) => {
                    if let Some(terminal) = self.terminals.get_mut(&id) {
                        terminal.append(bytes, eof);
                    }
                }
                Work::TerminalInputError(id, error) => {
                    if let Some(terminal) = self.terminals.get_mut(&id) {
                        terminal.input_error(error.clone());
                        let session = terminal.snapshot.session_id.clone();
                        self.error(Operation::Terminal, Some(session), failure(error));
                    }
                }
            }
        }
    }

    fn request<T: Serialize>(
        &mut self,
        method: &str,
        params: &T,
        operation: Operation,
        session: Option<p::SessionId>,
        value: Option<String>,
    ) -> Result<(), p::Error> {
        if self.pending.len() >= MAX_PENDING {
            return Err(failure("Too many outstanding ACP requests"));
        }
        self.next_id += 1;
        let id = self.next_id;
        self.write(json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))?;
        let deadline = if operation == Operation::Prompt {
            None
        } else {
            Some(Instant::now() + self.timeout)
        };
        self.pending.insert(
            id,
            Pending {
                operation,
                session,
                value,
                deadline,
            },
        );
        Ok(())
    }

    fn write(&self, value: Value) -> Result<(), p::Error> {
        let line = serde_json::to_string(&value).map_err(|e| failure(e.to_string()))?;
        if line.len() > MAX_FRAME {
            return Err(failure("ACP message exceeds 8 MiB"));
        }
        self.writer
            .try_send(line)
            .map_err(|_| p::Error::new(-32099, "Agent input is blocked or disconnected"))
    }
    fn response<T: Serialize>(
        &mut self,
        id: p::RequestId,
        result: Result<T, p::Error>,
    ) -> Result<(), p::Error> {
        self.inbound.remove(&id);
        let sent = self.write(match result {
            Ok(value) => json!({"jsonrpc":"2.0","id":id,"result":value}),
            Err(error) => json!({"jsonrpc":"2.0","id":id,"error":error}),
        });
        match sent {
            // A legal text file may exceed the wire limit after JSON escaping.
            // Respond with a small error so the peer is never left waiting.
            Err(error) if error.code != p::ErrorCode::Other(-32099) => {
                self.write(json!({"jsonrpc":"2.0","id":id,"error":error}))
            }
            other => other,
        }
    }
    fn error(&self, operation: Operation, session_id: Option<p::SessionId>, error: p::Error) {
        self.sink.emit(Event::Error {
            operation,
            session_id,
            error,
        });
    }
    fn known_session(&self, id: &p::SessionId) -> Result<(), p::Error> {
        if self.sessions.contains(id) {
            Ok(())
        } else {
            Err(failure("Unknown ACP session"))
        }
    }
    fn idle_session(&self, id: &p::SessionId) -> Result<(), p::Error> {
        self.known_session(id)?;
        if self
            .pending
            .values()
            .any(|p| p.session.as_ref() == Some(id))
        {
            Err(failure(
                "Wait for this session's current operation to finish",
            ))
        } else {
            Ok(())
        }
    }

    fn command(&mut self, command: Command) -> Result<(), p::Error> {
        match command {
            Command::Permission { id, ticket, option } => {
                let Some(HostRequest::Permission { request, ticket: live }) = self.host.get(&id) else {
                    return Ok(());
                };
                if ticket != *live { return Ok(()); }
                if option
                    .as_ref()
                    .is_some_and(|id| !request.options.iter().any(|o| &o.option_id == id))
                {
                    return Err(p::Error::invalid_params());
                }
                let selected = option.clone();
                let outcome = option.map_or(p::RequestPermissionOutcome::Cancelled, |id| {
                    p::RequestPermissionOutcome::Selected(p::SelectedPermissionOutcome::new(id))
                });
                self.response(id.clone(), Ok(p::RequestPermissionResponse::new(outcome)))?;
                self.host.remove(&id);
                if let Some(option) = selected {
                    self.sink.emit(Event::PermissionResponded {
                        id: id.clone(),
                        ticket,
                        option,
                    });
                }
                self.sink.emit(Event::PermissionResolved(id, ticket));
                return Ok(());
            }
            Command::FileResponse { id, ticket, result, edit } => {
                if matches!(self.host.get(&id), Some(HostRequest::File { ticket: live, .. }) if *live == ticket) {
                    self.host.remove(&id);
                    self.lifetime.files.lock().unwrap().remove(&id);
                    self.response(id, result)?;
                }
                // An already running atomic write can complete after a cancel.
                // Always report the actual edit even if its RPC was cancelled.
                if let Some(edit) = edit {
                    self.sink.emit(Event::FileEdited(edit));
                }
                return Ok(());
            }
            _ => {}
        }
        let initialized = self
            .initialized
            .as_ref()
            .ok_or_else(|| failure("The agent has not initialized yet"))?;
        match command {
            Command::NewSession => self.request(
                "session/new",
                &p::NewSessionRequest::new(self.workspace.agent_root()),
                Operation::NewSession,
                None,
                None,
            ),
            Command::LoadSession(id) => {
                if !initialized.agent_capabilities.load_session {
                    return Err(failure("This agent does not support loading sessions"));
                }
                if self.sessions.contains(&id) {
                    return Err(failure("This session is already open"));
                }
                self.request(
                    "session/load",
                    &p::LoadSessionRequest::new(id.clone(), self.workspace.agent_root()),
                    Operation::LoadSession,
                    Some(id.clone()),
                    None,
                )?;
                self.sessions.insert(id); // Replay notifications may precede the response.
                Ok(())
            }
            Command::ResumeSession(id) => {
                if initialized
                    .agent_capabilities
                    .session_capabilities
                    .resume
                    .is_none()
                {
                    return Err(failure("This agent does not support resuming sessions"));
                }
                if self.sessions.contains(&id) {
                    return Err(failure("This session is already open"));
                }
                self.request(
                    "session/resume",
                    &p::ResumeSessionRequest::new(id.clone(), self.workspace.agent_root()),
                    Operation::ResumeSession,
                    Some(id.clone()),
                    None,
                )?;
                self.sessions.insert(id);
                Ok(())
            }
            Command::ListSessions { cursor } => {
                if initialized
                    .agent_capabilities
                    .session_capabilities
                    .list
                    .is_none()
                {
                    return Err(failure("This agent does not support listing sessions"));
                }
                self.request(
                    "session/list",
                    &p::ListSessionsRequest::new()
                        .cwd(self.workspace.agent_root())
                        .cursor(cursor),
                    Operation::ListSessions,
                    None,
                    None,
                )
            }
            Command::Authenticate(id) => {
                if !initialized.auth_methods.iter().any(|m| m.id() == &id) {
                    return Err(failure("Unknown authentication method"));
                }
                if !initialized
                    .auth_methods
                    .iter()
                    .any(|m| m.id() == &id && matches!(m, p::AuthMethod::Agent(_)))
                {
                    return Err(failure(
                        "This authentication method requires an interactive sign-in outside Agent Chat. Sign in using the agent's command, then reconnect.",
                    ));
                }
                self.request(
                    "authenticate",
                    &p::AuthenticateRequest::new(id),
                    Operation::Authenticate,
                    None,
                    None,
                )
            }
            Command::Prompt {
                session_id,
                content,
            } => {
                self.idle_session(&session_id)?;
                if content.is_empty() {
                    return Err(p::Error::invalid_params());
                }
                let caps = &initialized.agent_capabilities.prompt_capabilities;
                for block in &content {
                    match block {
                        p::ContentBlock::Image(_) if !caps.image => {
                            return Err(failure("The agent does not accept images"));
                        }
                        p::ContentBlock::Audio(_) if !caps.audio => {
                            return Err(failure("The agent does not accept audio"));
                        }
                        p::ContentBlock::Resource(_) if !caps.embedded_context => {
                            return Err(failure("The agent does not accept embedded context"));
                        }
                        _ => {}
                    }
                }
                let result = self.request(
                    "session/prompt",
                    &p::PromptRequest::new(session_id.clone(), content),
                    Operation::Prompt,
                    Some(session_id.clone()),
                    None,
                );
                if result.is_ok() {
                    self.empty_sessions.remove(&session_id);
                }
                result
            }
            Command::SetMode {
                session_id,
                mode_id,
            } => {
                self.idle_session(&session_id)?;
                self.request(
                    "session/set_mode",
                    &p::SetSessionModeRequest::new(session_id.clone(), mode_id.clone()),
                    Operation::SetMode,
                    Some(session_id),
                    Some(mode_id.to_string()),
                )
            }
            Command::SetConfig {
                session_id,
                config_id,
                value,
            } => {
                self.idle_session(&session_id)?;
                self.request(
                    "session/set_config_option",
                    &p::SetSessionConfigOptionRequest::new(session_id.clone(), config_id, value),
                    Operation::SetConfig,
                    Some(session_id),
                    None,
                )
            }
            Command::Cancel(id) => {
                self.known_session(&id)?;
                // The prompt response can race with a user's Stop click. Do
                // not leave an idle session marked cancelled for its next turn.
                if !self
                    .pending
                    .values()
                    .any(|p| p.operation == Operation::Prompt && p.session.as_ref() == Some(&id))
                {
                    return Ok(());
                }
                self.write(json!({"jsonrpc":"2.0","method":"session/cancel","params":p::CancelNotification::new(id.clone())}))?;
                self.cancelling.insert(id.clone());
                self.cancel_host(&id);
                for pending in self
                    .pending
                    .values_mut()
                    .filter(|p| p.operation == Operation::Prompt && p.session.as_ref() == Some(&id))
                {
                    pending.deadline =
                        Some(Instant::now() + self.timeout.min(Duration::from_secs(5)));
                }
                for terminal in self
                    .terminals
                    .values_mut()
                    .filter(|t| t.snapshot.session_id == id)
                {
                    terminal.kill();
                }
                Ok(())
            }
            Command::CloseSession(id) => {
                self.idle_session(&id)?;
                if initialized
                    .agent_capabilities
                    .session_capabilities
                    .close
                    .is_none()
                {
                    return Err(failure("This agent does not support closing sessions"));
                }
                self.request(
                    "session/close",
                    &p::CloseSessionRequest::new(id.clone()),
                    Operation::CloseSession,
                    Some(id),
                    None,
                )
            }
            Command::DeleteSession(id) => {
                if initialized
                    .agent_capabilities
                    .session_capabilities
                    .delete
                    .is_none()
                {
                    return Err(failure(
                        "This agent does not support deleting session history",
                    ));
                }
                // History may refer to sessions that have never been loaded.
                if self.sessions.contains(&id) {
                    self.idle_session(&id)?;
                }
                self.request(
                    "session/delete",
                    &p::DeleteSessionRequest::new(id.clone()),
                    Operation::DeleteSession,
                    Some(id),
                    None,
                )
            }
            Command::TerminalInput {
                session_id,
                terminal_id,
                text,
            } => self.terminal_mut(&session_id, &terminal_id)?.input(&text),
            Command::KillTerminal {
                session_id,
                terminal_id,
            } => {
                self.terminal_mut(&session_id, &terminal_id)?.kill();
                Ok(())
            }
            Command::Permission { .. } | Command::FileResponse { .. } => unreachable!(),
        }
    }

    fn frame(&mut self, frame: Value) -> Result<(), p::Error> {
        if frame.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || !frame.is_object() {
            return Err(p::Error::invalid_request());
        }
        if let Some(method) = frame.get("method").and_then(Value::as_str) {
            let params = frame.get("params").cloned().unwrap_or_else(|| json!({}));
            if let Some(id) = frame.get("id") {
                let id: p::RequestId = decode(id.clone())?;
                if !self.inbound.insert(id.clone()) {
                    return Err(failure("Agent reused an outstanding request ID"));
                }
                if self.inbound.len() > MAX_PENDING {
                    self.response::<Value>(
                        id,
                        Err(failure("Too many outstanding agent requests")),
                    )?;
                    return Ok(());
                }
                let result = self.host_request(id.clone(), method, params);
                if let Err(error) = result {
                    self.response::<Value>(id, Err(error))?;
                }
            } else if method == "session/update" {
                // Forward compatibility: an unknown update kind is diagnostic,
                // not a reason to tear down an otherwise valid conversation.
                match decode::<p::SessionNotification>(params) {
                    Ok(notification) => {
                        if super::model::is_conversation_update(&notification.update) {
                            self.empty_sessions.remove(&notification.session_id);
                        }
                        if self.sessions.contains(&notification.session_id) {
                            self.sink.emit(Event::Update(notification));
                        } else if self
                            .pending
                            .values()
                            .any(|p| p.operation == Operation::NewSession)
                            && self.early_updates.len() < 64
                        {
                            self.early_updates.push_back(notification);
                        } else {
                            self.sink.emit(Event::Diagnostic(
                                "Ignored update for an unknown session".into(),
                            ));
                        }
                    }
                    Err(error) => self.sink.emit(Event::Diagnostic(format!(
                        "Ignored session update: {}",
                        error.message
                    ))),
                }
            }
            return Ok(());
        }
        let Some(id) = frame.get("id").and_then(Value::as_u64) else {
            return Err(p::Error::invalid_request());
        };
        if frame.get("result").is_some() == frame.get("error").is_some() {
            return Err(p::Error::invalid_request());
        }
        let Some(pending) = self.pending.remove(&id) else {
            return Ok(());
        }; // Late response after timeout.
        if let Some(error) = frame.get("error") {
            let error = decode::<p::Error>(error.clone())
                .unwrap_or_else(|_| failure("Malformed ACP error response"));
            self.cleanup_failed(&pending);
            let fatal = pending.operation == Operation::Initialize;
            self.error(pending.operation, pending.session, error.clone());
            return if fatal { Err(error) } else { Ok(()) };
        }
        let result = frame.get("result").cloned().unwrap_or(Value::Null);
        let operation = pending.operation.clone();
        let session = pending.session.clone();
        if let Err(error) = self.result(pending, result) {
            self.cleanup_failed(&Pending {
                operation: operation.clone(),
                session: session.clone(),
                value: None,
                deadline: None,
            });
            self.error(operation.clone(), session, error.clone());
            if operation == Operation::Initialize {
                return Err(error);
            }
        }
        Ok(())
    }

    fn result(&mut self, pending: Pending, value: Value) -> Result<(), p::Error> {
        match pending.operation {
            Operation::Initialize => {
                let result: p::InitializeResponse = decode(value)?;
                if result.protocol_version != ProtocolVersion::V1 {
                    return Err(failure(format!(
                        "Unsupported ACP protocol version: {} (vtcode supports 1)",
                        result.protocol_version
                    )));
                }
                self.initialized = Some(result.clone());
                self.sink.emit(Event::Initialized(Box::new(result)));
            }
            Operation::Authenticate => {
                let _: p::AuthenticateResponse = decode(value)?;
                self.sink.emit(Event::Authenticated);
            }
            Operation::NewSession => {
                let result: p::NewSessionResponse = decode(value)?;
                if !self.sessions.insert(result.session_id.clone()) {
                    return Err(failure("The agent reused an open session ID"));
                }
                let session_id = result.session_id;
                self.empty_sessions.insert(session_id.clone());
                self.sink.emit(Event::SessionOpened {
                    session_id: session_id.clone(),
                    modes: result.modes,
                    config_options: result.config_options.unwrap_or_default(),
                    loaded: false,
                });
                self.early_updates.retain(|update| {
                    if update.session_id == session_id {
                        if super::model::is_conversation_update(&update.update) {
                            self.empty_sessions.remove(&session_id);
                        }
                        self.sink.emit(Event::Update(update.clone()));
                        false
                    } else {
                        true
                    }
                });
                if !self
                    .pending
                    .values()
                    .any(|p| p.operation == Operation::NewSession)
                {
                    self.early_updates.clear();
                }
            }
            Operation::LoadSession => {
                let result: p::LoadSessionResponse = decode(value)?;
                self.sink.emit(Event::SessionOpened {
                    session_id: pending.session.expect("load session"),
                    modes: result.modes,
                    config_options: result.config_options.unwrap_or_default(),
                    loaded: true,
                });
            }
            Operation::ResumeSession => {
                let result: p::ResumeSessionResponse = decode(value)?;
                self.sink.emit(Event::SessionOpened {
                    session_id: pending.session.expect("resume session"),
                    modes: result.modes,
                    config_options: result.config_options.unwrap_or_default(),
                    loaded: true,
                });
            }
            Operation::ListSessions => self.sink.emit(Event::SessionsListed(decode(value)?)),
            Operation::Prompt => {
                let result: p::PromptResponse = decode(value)?;
                let id = pending.session.expect("prompt session");
                self.cancelling.remove(&id);
                self.cancel_host(&id);
                self.sink.emit(Event::PromptFinished {
                    session_id: id,
                    response: result,
                });
            }
            Operation::SetMode => {
                let _: p::SetSessionModeResponse = decode(value)?;
                self.sink.emit(Event::ModeChanged {
                    session_id: pending.session.expect("mode session"),
                    mode_id: p::SessionModeId::new(pending.value.expect("mode ID")),
                });
            }
            Operation::SetConfig => {
                let result: p::SetSessionConfigOptionResponse = decode(value)?;
                self.sink.emit(Event::ConfigChanged {
                    session_id: pending.session.expect("config session"),
                    options: result.config_options,
                });
            }
            Operation::CloseSession | Operation::DeleteSession => {
                let deleted = pending.operation == Operation::DeleteSession;
                if deleted {
                    let _: p::DeleteSessionResponse = decode(value)?;
                } else {
                    let _: p::CloseSessionResponse = decode(value)?;
                }
                let id = pending.session.expect("close session");
                self.sessions.remove(&id);
                if deleted {
                    self.empty_sessions.remove(&id);
                }
                self.cancel_host(&id);
                let terminals: Vec<_> = self
                    .terminals
                    .iter()
                    .filter(|(_, t)| t.snapshot.session_id == id)
                    .map(|(key, _)| key.clone())
                    .collect();
                for key in terminals {
                    if let Some(mut terminal) = self.terminals.remove(&key) {
                        for waiter in std::mem::take(&mut terminal.waiters) {
                            self.response::<Value>(waiter, Err(failure("Session closed")))?;
                        }
                    }
                }
                self.sink.emit(if deleted {
                    Event::SessionDeleted(id)
                } else {
                    Event::SessionClosed(id)
                });
            }
            Operation::Terminal => {}
        }
        Ok(())
    }

    fn host_request(
        &mut self,
        id: p::RequestId,
        method: &str,
        params: Value,
    ) -> Result<(), p::Error> {
        if self.initialized.is_none() {
            return Err(failure("ACP initialization has not completed"));
        }
        if self.host.contains_key(&id) {
            return Err(failure("Duplicate active request ID"));
        }
        if self.host.len() >= MAX_PENDING {
            return Err(failure("Too many outstanding agent requests"));
        }
        match method {
            "session/request_permission" => {
                let request: p::RequestPermissionRequest = decode(params)?;
                self.known_session(&request.session_id)?;
                if self.cancelling.contains(&request.session_id) {
                    return self.response(
                        id,
                        Ok(p::RequestPermissionResponse::new(
                            p::RequestPermissionOutcome::Cancelled,
                        )),
                    );
                }
                self.next_file_ticket = self.next_file_ticket.checked_add(1).ok_or_else(|| failure("Host request generation overflow"))?;
                let ticket = self.next_file_ticket;
                self.host.insert(
                    id.clone(),
                    HostRequest::Permission { request: Box::new(request.clone()), ticket },
                );
                self.sink.emit(Event::Permission { id, ticket, request });
            }
            "fs/read_text_file" | "fs/write_text_file" => {
                let mut request = if method == "fs/read_text_file" {
                    FileRequest::Read(decode(params)?)
                } else {
                    FileRequest::Write(decode(params)?)
                };
                self.known_session(request.session_id())?;
                if self.cancelling.contains(request.session_id()) {
                    return Err(p::Error::request_cancelled());
                }
                let canonical = self.workspace.resolve(request.path())?;
                *request.path_mut() = canonical;
                self.next_file_ticket = self.next_file_ticket.checked_add(1).ok_or_else(|| failure("File request generation overflow"))?;
                let ticket = self.next_file_ticket;
                self.lifetime.files.lock().unwrap().insert(id.clone(), ticket);
                self.host
                    .insert(id.clone(), HostRequest::File { session: request.session_id().clone(), ticket });
                self.sink.emit(Event::FileRequest { id, ticket, request });
            }
            "terminal/create" => {
                let request: p::CreateTerminalRequest = decode(params)?;
                self.known_session(&request.session_id)?;
                if self.cancelling.contains(&request.session_id) {
                    return Err(p::Error::request_cancelled());
                }
                if self.terminals.len() >= 32 {
                    return Err(failure(
                        "Release a terminal before creating more (limit: 32)",
                    ));
                }
                self.next_terminal += 1;
                let terminal_id = format!("vtcode-{}", self.next_terminal);
                let terminal = Terminal::spawn(
                    terminal_id.clone(),
                    request,
                    &self.workspace,
                    self.sender.clone(),
                    &self.terminal_storage,
                    &self.lifetime,
                )?;
                self.sink.emit(Event::Terminal(terminal.snapshot.clone()));
                self.terminals.insert(terminal_id.clone(), terminal);
                self.response(id, Ok(p::CreateTerminalResponse::new(terminal_id)))?;
            }
            "terminal/output" => {
                let request: p::TerminalOutputRequest = decode(params)?;
                let terminal =
                    self.terminal_mut(&request.session_id, &request.terminal_id.to_string())?;
                let result = p::TerminalOutputResponse::new(
                    terminal.snapshot.output.clone(),
                    terminal.snapshot.truncated,
                )
                .exit_status(terminal.snapshot.exit_status.clone());
                self.response(id, Ok(result))?;
            }
            "terminal/wait_for_exit" => {
                let request: p::WaitForTerminalExitRequest = decode(params)?;
                let terminal =
                    self.terminal_mut(&request.session_id, &request.terminal_id.to_string())?;
                if terminal.complete() {
                    let status = terminal
                        .snapshot
                        .exit_status
                        .clone()
                        .expect("completed status");
                    self.response(id, Ok(p::WaitForTerminalExitResponse::new(status)))?;
                } else if terminal.waiters.len() >= MAX_PENDING {
                    return Err(failure("Too many terminal waiters"));
                } else {
                    terminal.waiters.push(id);
                }
            }
            "terminal/kill" => {
                let request: p::KillTerminalRequest = decode(params)?;
                self.terminal_mut(&request.session_id, &request.terminal_id.to_string())?
                    .kill();
                self.response(id, Ok(p::KillTerminalResponse::new()))?;
            }
            "terminal/release" => {
                let request: p::ReleaseTerminalRequest = decode(params)?;
                let key = request.terminal_id.to_string();
                self.terminal_mut(&request.session_id, &key)?;
                if let Some(mut terminal) = self.terminals.remove(&key) {
                    terminal.kill();
                    terminal.snapshot.released = true;
                    self.sink.emit(Event::Terminal(terminal.snapshot.clone()));
                    for waiter in std::mem::take(&mut terminal.waiters) {
                        self.response::<Value>(waiter, Err(failure("Terminal released")))?;
                    }
                }
                self.response(id, Ok(p::ReleaseTerminalResponse::new()))?;
            }
            _ => return Err(p::Error::method_not_found()),
        }
        Ok(())
    }

    fn terminal_mut(
        &mut self,
        session: &p::SessionId,
        terminal: &str,
    ) -> Result<&mut Terminal, p::Error> {
        self.known_session(session)?;
        self.terminals
            .get_mut(terminal)
            .filter(|t| &t.snapshot.session_id == session)
            .ok_or_else(|| failure("Unknown terminal in this session"))
    }
    fn poll_terminals(&mut self) {
        let mut replies = Vec::new();
        for terminal in self.terminals.values_mut() {
            if terminal.poll() {
                self.sink.emit(Event::Terminal(terminal.snapshot.clone()));
            }
            if terminal.complete() {
                for id in std::mem::take(&mut terminal.waiters) {
                    replies.push((
                        id,
                        terminal
                            .snapshot
                            .exit_status
                            .clone()
                            .expect("completed status"),
                    ));
                }
            }
        }
        for (id, status) in replies {
            let _ = self.response(id, Ok(p::WaitForTerminalExitResponse::new(status)));
        }
    }
    fn cancel_host(&mut self, session: &p::SessionId) {
        let ids: Vec<_> = self
            .host
            .iter()
            .filter(|(_, request)| match request {
                HostRequest::Permission { request, .. } => &request.session_id == session,
                HostRequest::File { session: id, .. } => id == session,
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            match self.host.remove(&id) {
                Some(HostRequest::Permission { ticket, .. }) => {
                    let _ = self.response(
                        id.clone(),
                        Ok(p::RequestPermissionResponse::new(
                            p::RequestPermissionOutcome::Cancelled,
                        )),
                    );
                    self.sink.emit(Event::PermissionResolved(id, ticket));
                }
                Some(HostRequest::File { .. }) => {
                    self.lifetime.files.lock().unwrap().remove(&id);
                    let _ = self.response::<Value>(id, Err(p::Error::request_cancelled()));
                }
                None => {}
            }
        }
    }
    fn fail_pending(&mut self, reason: &str) {
        for (_, pending) in self.pending.drain() {
            self.sink.emit(Event::Error {
                operation: pending.operation,
                session_id: pending.session,
                error: failure(reason),
            });
        }
        for (id, request) in self.host.drain() {
            if let HostRequest::Permission { ticket, .. } = request {
                self.sink.emit(Event::PermissionResolved(id, ticket));
            }
        }
    }
    fn cleanup_failed(&mut self, pending: &Pending) {
        if matches!(
            pending.operation,
            Operation::LoadSession | Operation::ResumeSession
        ) && let Some(id) = &pending.session
        {
            self.sessions.remove(id);
        }
        if pending.operation == Operation::Prompt
            && let Some(id) = &pending.session
        {
            self.cancelling.remove(id);
            self.cancel_host(id);
            for terminal in self
                .terminals
                .values_mut()
                .filter(|t| &t.snapshot.session_id == id)
            {
                terminal.kill();
            }
        }
        if pending.operation == Operation::NewSession
            && !self
                .pending
                .values()
                .any(|p| p.operation == Operation::NewSession)
        {
            self.early_updates.clear();
        }
    }
}

fn decode<T: DeserializeOwned>(value: Value) -> Result<T, p::Error> {
    serde_json::from_value(value)
        .map_err(|e| p::Error::new(-32602, format!("Invalid ACP payload: {e}")))
}

pub fn command_context(command: &Command) -> (Operation, Option<p::SessionId>) {
    match command {
        Command::NewSession => (Operation::NewSession, None),
        Command::LoadSession(id) => (Operation::LoadSession, Some(id.clone())),
        Command::ResumeSession(id) => (Operation::ResumeSession, Some(id.clone())),
        Command::ListSessions { .. } => (Operation::ListSessions, None),
        Command::Authenticate(_) => (Operation::Authenticate, None),
        Command::Prompt { session_id, .. } => (Operation::Prompt, Some(session_id.clone())),
        Command::SetMode { session_id, .. } => (Operation::SetMode, Some(session_id.clone())),
        Command::SetConfig { session_id, .. } => (Operation::SetConfig, Some(session_id.clone())),
        Command::CloseSession(id) => (Operation::CloseSession, Some(id.clone())),
        Command::DeleteSession(id) => (Operation::DeleteSession, Some(id.clone())),
        Command::TerminalInput { session_id, .. } | Command::KillTerminal { session_id, .. } => {
            (Operation::Terminal, Some(session_id.clone()))
        }
        Command::Cancel(id) => (Operation::Prompt, Some(id.clone())),
        _ => (Operation::Prompt, None),
    }
}

fn read_frames(reader: impl Read, sender: mpsc::SyncSender<Work>) {
    let mut reader = BufReader::new(reader);
    loop {
        let mut bytes = Vec::new();
        match reader
            .by_ref()
            .take((MAX_FRAME + 1) as u64)
            .read_until(b'\n', &mut bytes)
        {
            Ok(0) => {
                let _ = sender.send(Work::Closed("Agent closed stdout".into()));
                break;
            }
            Ok(_) if bytes.len() > MAX_FRAME => {
                let _ = sender.send(Work::Closed("ACP frame exceeds 8 MiB".into()));
                break;
            }
            Ok(_) => {
                if !bytes.ends_with(b"\n") {
                    let _ =
                        sender.send(Work::Closed("Incomplete ACP frame at end of stream".into()));
                    break;
                }
                match serde_json::from_slice(&bytes) {
                    Ok(frame) => {
                        if sender.send(Work::Frame(frame)).is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender
                            .send(Work::Closed(format!("Invalid JSON on ACP stdout: {error}")));
                        break;
                    }
                }
            }
            Err(error) => {
                let _ = sender.send(Work::Closed(format!("Could not read ACP stdout: {error}")));
                break;
            }
        }
    }
}

fn trim_front(text: &mut String, limit: usize) -> bool {
    if text.len() <= limit {
        return false;
    }
    let mut start = text.len() - limit;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text.drain(..start);
    true
}
