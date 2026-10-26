//! Asynchronous transport. `call` is for file/tool workers only, never the UI.
//! Submissions are bounded and nonblocking; network stalls cannot backpressure
//! the terminal input pump or retain an unbounded history of Rope snapshots.
#[cfg(test)]
#[path = "remote_client_deadline_tests.rs"]
mod deadline_tests;
#[path = "remote_liveness.rs"]
mod liveness;
#[path = "remote_cpu_capture.rs"]
mod cpu_capture;
use super::*;
#[cfg(test)]
use std::io::BufReader;
use std::{
    collections::HashMap,
    io::{BufRead, Read, Write},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

type Completion = mpsc::SyncSender<Delivery>;
struct Pending {
    completed: Completion,
    retained: usize,
    operation: Option<diagnostics::Operation>,
}

/// Own a newly allocated server handle until a worker actually accepts it.
/// Dropping a cancelled/timed-out receiver also drops its queued delivery.
struct Delivery {
    result: Option<Result<Reply, Failure>>,
    cleanup: Option<(
        std::sync::Weak<Shared>,
        std::sync::Weak<mpsc::SyncSender<Option<Request>>>,
    )>,
}
impl Delivery {
    fn error(error: Failure) -> Self {
        Self {
            result: Some(Err(error)),
            cleanup: None,
        }
    }
    fn new(
        result: Result<Reply, Failure>,
        shared: &Arc<Shared>,
        wake: &std::sync::Weak<mpsc::SyncSender<Option<Request>>>,
    ) -> Self {
        let resource = match &result {
            Ok(Reply::Agent(agent::Reply::Started { .. })) => true,
            Ok(Reply::Context(context::Reply::Files { source, .. })) => source.is_some(),
            Ok(Reply::Agent(agent::Reply::History(data))) => data.transfer().is_some(),
            Ok(Reply::Rollback(reply)) => {
                reply.opened
                    || reply
                        .state
                        .as_ref()
                        .and_then(text::Data::transfer)
                        .is_some()
            }
            Ok(Reply::Text(
                text::Reply::Opened { .. }
                | text::Reply::Source { .. }
                | text::Reply::Upload { .. },
            ))
            | Ok(Reply::Binary(binary::Reply::Opened { .. })) => true,
            _ => false,
        };
        Self {
            result: Some(result),
            cleanup: resource.then(|| (Arc::downgrade(shared), wake.clone())),
        }
    }
    fn take(mut self) -> Result<Reply, Failure> {
        self.result.take().expect("one delivery acceptance")
    }
}
impl Drop for Delivery {
    fn drop(&mut self) {
        let Some((shared, wake)) = &self.cleanup else {
            return;
        };
        let Some(shared) = shared
            .upgrade()
            .filter(|shared| !shared.closed.load(Ordering::Acquire))
        else {
            return;
        };
        match &self.result {
            Some(Ok(Reply::Context(context::Reply::Files {
                source: Some(source),
                ..
            }))) => {
                shared.text_closed.lock().unwrap().insert(*source);
            }
            Some(Ok(Reply::Agent(agent::Reply::Started { agent }))) => {
                shared.agent_closed.lock().unwrap().insert(*agent);
            }
            Some(Ok(Reply::Agent(agent::Reply::History(data)))) => {
                if let Some(transfer) = data.transfer() {
                    shared.text_closed.lock().unwrap().insert(transfer);
                }
            }
            Some(Ok(Reply::Rollback(reply))) => {
                if reply.opened && reply.journal != 0 {
                    shared.rollback_closed.lock().unwrap().insert(reply.journal);
                }
                if let Some(transfer) = reply.state.as_ref().and_then(text::Data::transfer) {
                    shared.text_closed.lock().unwrap().insert(transfer);
                }
            }
            Some(Ok(Reply::Text(
                text::Reply::Opened { transfer, .. }
                | text::Reply::Source { transfer, .. }
                | text::Reply::Upload { transfer },
            ))) => {
                shared.text_closed.lock().unwrap().insert(*transfer);
            }
            Some(Ok(Reply::Binary(binary::Reply::Opened { source, .. }))) => {
                shared.binary_closed.lock().unwrap().insert(*source);
            }
            _ => return,
        }
        if let Some(wake) = wake.upgrade() {
            let _ = wake.try_send(None);
        }
    }
}
struct Shared {
    diagnostics: Option<super::diagnostics::Attempt>,
    git_output: OnceLock<super::diagnostics::Log>,
    capture: cpu_capture::CaptureState,
    failure: Mutex<Option<String>>,
    liveness: liveness::State,
    pending: Mutex<HashMap<u64, Pending>>,
    capacity: Condvar,
    closed: AtomicBool,
    streams: Mutex<HashMap<u64, Arc<process::Stream>>>,
    credits: Mutex<HashMap<u64, usize>>,
    deletion_progress: Mutex<HashMap<u64, crate::file_deletion::Progress>>,
    killed: Mutex<std::collections::HashSet<u64>>,
    cancelled: Mutex<std::collections::HashSet<u64>>,
    resized: Mutex<HashMap<u64, (u16, u16)>>,
    workspace_changes: Mutex<HashMap<String, watch::Batch>>,
    histories: Mutex<HashMap<u64, mpsc::SyncSender<history::Page>>>,
    binary_pages: Arc<Mutex<binary::Pages>>,
    binary_closed: Mutex<std::collections::HashSet<u64>>,
    text_closed: Mutex<std::collections::HashSet<u64>>,
    context_sources: Mutex<HashMap<u64, std::sync::Weak<context::Source>>>,
    rollback_closed: Mutex<std::collections::HashSet<u64>>,
    agents: Mutex<HashMap<u64, Arc<agent::Channel>>>,
    agent_closed: Mutex<std::collections::HashSet<u64>>,
    agent_credits: Mutex<std::collections::HashSet<(u64, u64)>>,
    agent_cancelled: Mutex<HashMap<(u64, vtcode::acp::protocol::SessionId), Request>>,
    agent_histories_closed: Mutex<HashMap<u64, usize>>,
    text_active: Mutex<usize>,
    text_ready: Condvar,
    wake: Mutex<crate::background::UiWake>,
}
impl Shared {
    fn take(&self, id: u64) -> Option<Pending> {
        let entry = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&id);
        self.capacity.notify_all();
        entry
    }
    fn operation(&self, id: u64) -> Option<diagnostics::Operation> {
        // No diagnostic lock is held with the admission lock. Formatting is
        // deferred to the viewer, never performed by these transport workers.
        if self.diagnostics.is_none() && self.git_output.get().is_none() { return None; }
        self.pending.lock().unwrap().get(&id).and_then(|pending| pending.operation.clone())
    }
    fn stop(&self, id: u64, error: &io::Error) {
        if let Some(pending) = self.take(id)
            && let Some(operation) = &pending.operation {
            operation.error(error);
        }
    }
    fn fail(&self, error: String) {
        if self.closed.swap(true, Ordering::AcqRel) { return; }
        *self.failure.lock().unwrap() = Some(error.clone());
        if let Some(trace) = &self.diagnostics { trace.event("Disconnected", &error); }
        self.capture.disconnected();
        self.liveness.wake();
        let pending = std::mem::take(&mut *self.pending.lock().unwrap_or_else(|e| e.into_inner()));
        for (_, pending) in pending {
            if let Some(operation) = &pending.operation { operation.failed(&error); }
            let _ = pending.completed.try_send(Delivery::error(error.clone().into()));
        }
        self.capacity.notify_all();
        {
            // Pair the closed predicate with the permit mutex. Otherwise a
            // waiter can check `connected`, miss this notification, and then
            // sleep forever even after every pending RPC has been failed.
            let _active = self.text_active.lock().unwrap();
            self.text_ready.notify_all();
        }
        let batch = std::mem::take(&mut *self.agents.lock().unwrap());
        for (_, channel) in batch {
            channel.end(error.clone());
        }
        let streams = std::mem::take(&mut *self.streams.lock().unwrap());
        for (_, stream) in streams {
            stream.end(Some(error.clone()));
        }
        self.wake.lock().unwrap().notify();
    }
}
struct Handle {
    sender: Arc<mpsc::SyncSender<Option<Request>>>,
    controls: mpsc::SyncSender<Request>,
    shared: Arc<Shared>,
    next_id: AtomicU64,
    hello: Hello,
}
impl Drop for Handle {
    fn drop(&mut self) {
        self.shared.fail("Remote connection closed".into());
    }
}
#[derive(Clone)]
pub(crate) struct Client(Arc<Handle>, Option<Arc<WorkspaceContext>>);

struct WorkspaceContext {
    root: String,
    hello: Hello,
}
pub(super) struct TextPermit(Client);
impl Drop for TextPermit {
    fn drop(&mut self) {
        *self.0.0.shared.text_active.lock().unwrap() -= 1;
        self.0.0.shared.text_ready.notify_all();
    }
}

impl Client {
    #[cfg(test)]
    pub(crate) fn expire_silent_peer_for_test(&self) {
        liveness::expire_silent_peer_for_test(Arc::downgrade(&self.0.shared), Arc::downgrade(&self.0.sender)).unwrap();
    }
    #[cfg(test)]
    pub(crate) fn next_request_id_for_test(&self) -> u64 {
        self.0.next_id.load(Ordering::Relaxed)
    }
    #[cfg(all(test, windows))]
    pub(crate) fn heartbeat_nonce_for_test(&self) -> u64 { self.0.shared.liveness.acknowledged_for_test() }

    #[cfg(test)]
    pub(crate) fn connect(
        reader: impl Read + Send + 'static,
        writer: impl Write + Send + 'static,
    ) -> io::Result<Self> {
        let mut reader = BufReader::new(reader);
        let hello = Self::read_hello(&mut reader)?;
        Self::attach(reader, writer, hello)
    }
    #[cfg(test)]
    pub(crate) fn read_hello(reader: &mut impl BufRead) -> io::Result<Hello> {
        let mut line = Vec::new();
        for _ in 0..4096 {
            if !wire::read_line(reader, &mut line)? {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Headless service did not start",
                ));
            }
            if let Some(hello) = Self::parse_hello(&line)? {
                return Ok(hello);
            }
        }
        Err(io::Error::other(
            "Headless service readiness limit exceeded",
        ))
    }
    pub(crate) fn parse_hello(line: &[u8]) -> io::Result<Option<Hello>> {
        let Some(data) = wire::record(line).strip_prefix(b"VTCR-READY:") else {
            return Ok(None);
        };
        let data = URL_SAFE_NO_PAD
            .decode(data.trim_ascii_end())
            .map_err(io::Error::other)?;
        let hello: Hello = serde_json::from_slice(&data)?;
        if hello.version != VERSION || hello.app_version != env!("CARGO_PKG_VERSION") {
            let headless_version = if hello.app_version.is_empty() {
                "unknown (version not reported)".into()
            } else {
                hello.app_version.escape_debug().to_string()
            };
            return Err(io::Error::other(format!(
                "VTCode version mismatch: local {} (protocol {}), headless {} (protocol {}). Install matching VTCode builds on both machines.",
                env!("CARGO_PKG_VERSION"),
                VERSION,
                headless_version,
                hello.version,
            )));
        }
        let root = url::Url::parse(&hello.root_uri).map_err(io::Error::other)?;
        if root.scheme() != "file" || !root.path().starts_with('/') || hello.root.contains('\0') {
            return Err(io::Error::other("Invalid headless workspace root URI"));
        }
        Ok(Some(hello))
    }
    #[cfg(test)]
    pub(crate) fn attach(
        reader: impl BufRead + Send + 'static,
        writer: impl Write + Send + 'static,
        hello: Hello,
    ) -> io::Result<Self> {
        Self::attach_with_diagnostics(reader, writer, hello, None)
    }
    pub(crate) fn attach_with_diagnostics(
        reader: impl BufRead + Send + 'static,
        writer: impl Write + Send + 'static,
        hello: Hello,
        diagnostics: Option<super::diagnostics::Attempt>,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            git_output: OnceLock::new(),
            diagnostics,
            capture: Default::default(),
            failure: Mutex::default(),
            liveness: Default::default(),
            pending: Mutex::new(HashMap::new()),
            capacity: Condvar::new(),
            closed: AtomicBool::new(false),
            streams: Mutex::new(HashMap::new()),
            credits: Mutex::new(HashMap::new()),
            deletion_progress: Mutex::default(),
            killed: Mutex::default(),
            cancelled: Mutex::default(),
            resized: Mutex::default(),
            workspace_changes: Mutex::default(),
            histories: Mutex::default(),
            binary_pages: Arc::default(),
            binary_closed: Mutex::default(),
            text_closed: Mutex::default(),
            context_sources: Mutex::default(),
            rollback_closed: Mutex::default(),
            agents: Default::default(),
            agent_closed: Default::default(),
            agent_credits: Default::default(),
            agent_cancelled: Default::default(),
            agent_histories_closed: Default::default(),
            text_active: Mutex::default(),
            text_ready: Condvar::new(),
            wake: Mutex::default(),
        });
        let (sender, jobs) = mpsc::sync_channel::<Option<Request>>(128);
        let sender = Arc::new(sender);
        let (controls, input) = mpsc::sync_channel::<Request>(128);
        let output_shared = shared.clone();
        let mut writer = liveness::Writer { inner: writer, shared: shared.clone() };
        let session = hello.session;
        std::thread::Builder::new().name("remote-output".into()).spawn(move || {
            crate::worker_priority::lower_current_thread();
            // Separate any setup keystrokes still in flight from the first
            // framed request after readiness; the service ignores this line.
            if let Err(error) = writer.write_all(b"\n").and_then(|_| writer.flush()) { output_shared.fail(error.to_string()); return; }
            for job in jobs {
                output_shared.capacity.notify_all();
                if output_shared.closed.load(Ordering::Acquire) { break; }
                let credits = std::mem::take(&mut *output_shared.credits.lock().unwrap());
                let result = (|| -> io::Result<()> {
                    write_controls(&mut writer, &output_shared, &input, session)?;
                    let batch = std::mem::take(&mut *output_shared.killed.lock().unwrap());
                    for process in batch {
                        let request = Request { id: 0, workspace: None, command: Command::ProcessKill { process } };
                        wire::write_message(&mut writer, 'C', session, 0, &wire::encode(&request)?)?;
                    }
                    for (process, bytes) in credits {
                        let request = Request { id: 0, workspace: None, command: Command::ProcessCredit { process, bytes } };
                        wire::write_message(&mut writer, 'C', session, 0, &wire::encode(&request)?)?;
                    }
                    if let Some(request) = job {
                        let operation = if request.id == 0 {
                            output_shared.diagnostics.as_ref().and_then(|trace| trace.notification(&request.command))
                        } else {
                            let pending = output_shared.pending.lock().unwrap();
                            let Some(pending) = pending.get(&request.id) else { return Ok(()); };
                            pending.operation.clone()
                        };
                        if let Command::Spawn { process, .. } = &request.command
                            && !output_shared.streams.lock().unwrap().contains_key(process)
                        {
                            if let Some(pending) = output_shared.take(request.id) {
                                if let Some(operation) = &pending.operation { operation.failed("Remote process was closed before startup"); }
                                let _ = pending.completed.try_send(Delivery::error("Remote process was closed before startup".into()));
                            }
                            return Ok(());
                        }
                        if let Some(operation) = &operation { operation.stage("Encoding request"); }
                        let data = match wire::encode(&request) {
                            Ok(data) => data,
                            Err(error) if request.id != 0 => {
                                if let Some(pending) = output_shared.take(request.id) {
                                    if let Some(operation) = &pending.operation { operation.error(&error); }
                                    let _ = pending.completed.try_send(Delivery::error(error.to_string().into()));
                                }
                                return Ok(());
                            }
                            Err(error) => return Err(error),
                        };
                        if let Some(operation) = &operation { operation.stage("Sending request"); }
                        let sent = wire::write_interleaved(&mut writer, 'C', session, request.id, &data, |writer| {
                            if request.id == 0 { return Ok(()); }
                            write_controls(writer, &output_shared, &input, session)?;
                            let batch = std::mem::take(&mut *output_shared.credits.lock().unwrap());
                            for (process, bytes) in batch {
                                let credit = Request { id: 0, workspace: None, command: Command::ProcessCredit { process, bytes } };
                                wire::write_message(writer, 'C', session, 0, &wire::encode(&credit)?)?;
                            }
                            let batch = std::mem::take(&mut *output_shared.killed.lock().unwrap());
                            for process in batch {
                                let kill = Request { id: 0, workspace: None, command: Command::ProcessKill { process } };
                                wire::write_message(writer, 'C', session, 0, &wire::encode(&kill)?)?;
                            }
                            Ok(())
                        });
                        if let Some(operation) = &operation {
                            if let Err(error) = &sent { operation.error(error); }
                            else if request.id == 0 { operation.notification_sent(); }
                            else { operation.stage("Awaiting response"); }
                        }
                        sent?;
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    output_shared.fail(format!("Remote connection interrupted: {error}. A save may have completed; verify before retrying."));
                    return;
                }
            }
        })?;
        let input_shared = shared.clone();
        let input_wake = Arc::downgrade(&sender);
        let mut reader = liveness::Reader { inner: reader, shared: shared.clone() };
        let spawn = std::thread::Builder::new().name("remote-input".into()).spawn(move || {
            crate::worker_priority::lower_current_thread();
            let mut decoder = wire::Decoder::new('S', session);
            let mut line = Vec::new();
            let result = (|| -> io::Result<()> {
                while match wire::read_line(&mut reader, &mut line) {
                    Ok(more) => more,
                    Err(error) => {
                        if let Some(trace) = &input_shared.diagnostics { trace.output("Transport output", &line); }
                        return Err(error);
                    }
                } {
                    let Some((id, data)) = decoder.line_with_output(&line, |output| {
                        if let Some(trace) = &input_shared.diagnostics { trace.output("Transport output", output); }
                    })? else { continue; };
                    let response: Response = serde_json::from_slice(&data)?;
                    if (response.id == 0 && id & wire::EVENT_ID == 0) || (response.id != 0 && response.id != id) {
                        return Err(io::Error::other("Remote response ID mismatch"));
                    }
                    if response.id == 0 {
                        match response.result.map_err(Failure::into_io)? {
                            Reply::Heartbeat { nonce } => {
                                input_shared.liveness.acknowledge(nonce);
                                if let Some(trace) = &input_shared.diagnostics {
                                    trace.event("Connection check", "Remote service responded");
                                }
                            }
                            Reply::AgentEvent(delivery) => {
                                let channel = input_shared.agents.lock().unwrap().get(&delivery.agent).cloned();
                                if let Some(channel) = channel { channel.push(delivery)?; }
                                else { input_shared.agent_closed.lock().unwrap().insert(delivery.agent); if let Some(wake) = input_wake.upgrade() { let _ = wake.try_send(None); } }
                            }
                            Reply::AgentEnd { agent, error } => {
                                if let Some(channel) = input_shared.agents.lock().unwrap().get(&agent) { channel.end(error); }
                            }
                            Reply::ProcessData { process, data } => {
                                let stream = input_shared.streams.lock().unwrap().get(&process).cloned();
                                if let Some(stream) = stream { stream.push(URL_SAFE_NO_PAD.decode(data).map_err(io::Error::other)?)?; }
                            }
                            Reply::ProcessEnd { process, error } => {
                                let stream = input_shared.streams.lock().unwrap().get(&process).cloned();
                                if let Some(stream) = stream { stream.end(error); }
                            }
                            Reply::ProcessInputConsumed { process, bytes } => {
                                let stream = input_shared.streams.lock().unwrap().get(&process).cloned();
                                if let Some(stream) = stream {
                                    let mut state = stream.state.lock().unwrap();
                                    if bytes > process::WINDOW || state.input_credit.saturating_add(bytes) > process::WINDOW { return Err(io::Error::other("Invalid remote input credit")); }
                                    state.input_credit += bytes;
                                    drop(state); stream.ready.notify_all();
                                }
                            }
                            Reply::DeletionProgress { request, progress } => {
                                if let Some(current) = input_shared.deletion_progress.lock().unwrap().get_mut(&request) { *current = progress; }
                            }
                            Reply::WorkspaceChanges(batch) => {
                                if let Some(trace) = &input_shared.diagnostics { trace.workspace_changes(&batch); }
                                let mut changes = input_shared.workspace_changes.lock().unwrap();
                                // A late notification from a previous folder must
                                // never be applied to the new folder's file keys.
                                if !changes.contains_key(&batch.root_uri) && changes.len() >= 8 {
                                    if let Some(old) = changes.keys().next().cloned() { changes.remove(&old); }
                                }
                                changes.entry(batch.root_uri.clone()).or_default().merge(batch);
                                input_shared.wake.lock().unwrap().notify();
                            }
                            Reply::HistoryPage { history, page } => {
                                if let Some(sender) = input_shared.histories.lock().unwrap().get(&history) {
                                    sender.try_send(page).map_err(io::Error::other)?;
                                }
                            }
                            _ => return Err(io::Error::other("Invalid remote event")),
                        }
                        continue;
                    }
                    let completed = input_shared.take(id);
                    if let Some(operation) = completed.as_ref().and_then(|pending| pending.operation.as_ref()) {
                        operation.reply(&response.result);
                    }
                    let delivery = Delivery::new(response.result, &input_shared, &input_wake);
                    if let Some(completed) = completed { let _ = completed.completed.try_send(delivery); }
                }
                Err(io::Error::new(io::ErrorKind::UnexpectedEof, "Remote service disconnected"))
            })();
            input_shared.fail(format!("{}. Your local edits remain unsaved. An interrupted save must be verified before retrying.", result.unwrap_err()));
        });
        if let Err(error) = spawn {
            shared.fail(error.to_string());
            return Err(error);
        }
        let client = Self(
            Arc::new(Handle {
                sender,
                controls,
                shared: shared.clone(),
                next_id: AtomicU64::new(1),
                hello,
            }),
            None,
        );
        if let Err(error) = liveness::start(Arc::downgrade(&shared), Arc::downgrade(&client.0.sender)) {
            shared.fail(error.to_string());
            return Err(error);
        }
        Ok(client)
    }
    pub(crate) fn disconnect(&self) {
        self.0.shared.fail("Remote connection closed; verify any interrupted save before retrying. Your local edits are retained.".into());
    }
    pub(crate) fn disconnect_reason(&self) -> Option<String> { self.0.shared.failure.lock().unwrap().clone() }
    pub(crate) fn diagnostic_log(&self) -> Option<super::diagnostics::Log> {
        self.0.shared.diagnostics.as_ref().map(|trace| trace.log.clone())
    }
    pub(crate) fn set_git_output(&self, log: &super::diagnostics::Log) {
        if self.0.shared.git_output.get().is_none() {
            let _ = self.0.shared.git_output.set(log.clone());
        }
    }
    pub(super) fn file_operation(&self, name: &'static str, path: &RemotePath) -> Option<diagnostics::Operation> {
        self.0.shared.diagnostics.as_ref().map(|trace| trace.operation(name, path))
    }
    pub(super) fn transfer_operation(&self, name: &'static str, transfer: u64, bytes: usize) -> Option<diagnostics::Operation> {
        self.0.shared.diagnostics.as_ref().map(|trace| trace.transfer(name, transfer, bytes))
    }
    pub(crate) fn hello(&self) -> &Hello {
        self.1
            .as_ref()
            .map_or(&self.0.hello, |context| &context.hello)
    }
    pub(crate) fn same_connection(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0.shared, &other.0.shared)
    }
    pub(super) fn register_context(&self, id: u64) -> Arc<context::Source> {
        let source = Arc::new(context::Source {
            client: self.clone(),
            id,
        });
        self.0
            .shared
            .context_sources
            .lock()
            .unwrap()
            .insert(id, Arc::downgrade(&source));
        source
    }
    pub(super) fn retain_context(&self, id: u64) -> io::Result<Arc<context::Source>> {
        let source = self
            .0
            .shared
            .context_sources
            .lock()
            .unwrap()
            .get(&id)
            .and_then(std::sync::Weak::upgrade)
            .ok_or_else(|| io::Error::other("Prepared file context is closed"))?;
        if source.client.hello().root_uri != self.hello().root_uri {
            return Err(io::Error::other(
                "Prepared file context belongs to another workspace",
            ));
        }
        Ok(source)
    }
    pub(super) fn release_context(&self, id: u64) {
        self.0.shared.context_sources.lock().unwrap().remove(&id);
        self.close_text(id);
    }
    pub(super) fn binary_pages(&self) -> Arc<Mutex<binary::Pages>> {
        self.0.shared.binary_pages.clone()
    }
    pub(super) fn text_permit(&self) -> io::Result<TextPermit> {
        self.text_permit_cancellable(None)
    }
    pub(super) fn text_permit_cancellable(
        &self,
        cancel: Option<&crate::background::CancellationToken>,
    ) -> io::Result<TextPermit> {
        let mut active = self.0.shared.text_active.lock().unwrap();
        while *active >= 2 {
            if cancel.is_some_and(|cancel| cancel.is_cancelled()) {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            if !self.connected() {
                return Err(io::Error::from(io::ErrorKind::NotConnected));
            }
            active = if cancel.is_some() {
                self.0
                    .shared
                    .text_ready
                    .wait_timeout(active, Duration::from_millis(50))
                    .unwrap()
                    .0
            } else {
                self.0.shared.text_ready.wait(active).unwrap()
            };
        }
        if !self.connected() {
            return Err(io::Error::from(io::ErrorKind::NotConnected));
        }
        if cancel.is_some_and(|cancel| cancel.is_cancelled()) {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        *active += 1;
        Ok(TextPermit(self.clone()))
    }
    pub(super) fn close_text(&self, transfer: u64) {
        if !self.connected() {
            return;
        }
        self.0.shared.text_closed.lock().unwrap().insert(transfer);
        let _ = self.0.sender.try_send(None);
    }
    pub(super) fn close_binary(&self, source: u64) {
        if !self.connected() {
            return;
        }
        self.0.shared.binary_closed.lock().unwrap().insert(source);
        let _ = self.0.sender.try_send(None);
    }
    fn request(&self, id: u64, command: Command) -> Request {
        Request {
            id,
            workspace: self.1.as_ref().map(|context| context.root.clone()),
            command,
        }
    }
    pub(crate) fn open_workspace(&self, path: RemotePath) -> io::Result<Self> {
        let Reply::Workspace { root, root_uri } = self.call(Command::OpenWorkspace { path })?
        else {
            return Err(io::Error::other("Invalid remote workspace response"));
        };
        let uri = url::Url::parse(&root_uri).map_err(io::Error::other)?;
        if uri.scheme() != "file"
            || uri
                .host_str()
                .is_some_and(|host| !host.is_empty() && host != "localhost")
        {
            return Err(io::Error::other("Invalid remote workspace URI"));
        }
        Ok(Self(
            self.0.clone(),
            Some(Arc::new(WorkspaceContext {
                root: root_uri.clone(),
                hello: Hello {
                    root,
                    root_uri,
                    ..self.hello().clone()
                },
            })),
        ))
    }
    pub(crate) fn connected(&self) -> bool {
        !self.0.shared.closed.load(Ordering::Acquire)
    }
    pub(super) fn register_agent(&self) -> io::Result<(u64, Arc<agent::Channel>)> {
        let mut agents = self.0.shared.agents.lock().unwrap();
        if agents.len() >= 8 {
            return Err(io::Error::other("Agent connection limit reached"));
        }
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        let channel = Arc::new(agent::Channel::default());
        agents.insert(id, channel.clone());
        Ok((id, channel))
    }
    pub(super) fn agent_channel(&self, agent: u64) -> Option<Arc<agent::Channel>> {
        self.0.shared.agents.lock().unwrap().get(&agent).cloned()
    }
    pub(super) fn close_agent(&self, agent: u64) {
        if let Some(channel) = self.0.shared.agents.lock().unwrap().remove(&agent) {
            channel.end("Agent disconnected".into());
        }
        if self.connected() {
            self.0.shared.agent_closed.lock().unwrap().insert(agent);
            let _ = self.0.sender.try_send(None);
        }
    }
    pub(super) fn credit_agent(&self, agent: u64, event: u64) {
        if self.connected() {
            self.0
                .shared
                .agent_credits
                .lock()
                .unwrap()
                .insert((agent, event));
            let _ = self.0.sender.try_send(None);
        }
    }
    pub(super) fn cancel_agent(
        &self,
        agent: u64,
        session: vtcode::acp::protocol::SessionId,
        through: u64,
        queued: bool,
    ) {
        if self.connected() {
            let request = self.request(
                0,
                Command::Agent(agent::Command::Cancel {
                    agent,
                    session: session.clone(),
                    through,
                    queued,
                }),
            );
            let mut cancellations = self.0.shared.agent_cancelled.lock().unwrap();
            if let Some(old) = cancellations.get_mut(&(agent, session.clone())) {
                if let (
                    Command::Agent(agent::Command::Cancel {
                        through: previous,
                        queued: was_queued,
                        ..
                    }),
                    Command::Agent(agent::Command::Cancel {
                        through, queued, ..
                    }),
                ) = (&mut old.command, &request.command)
                {
                    *previous = (*previous).max(*through);
                    *was_queued |= *queued;
                }
            } else {
                cancellations.insert((agent, session), request);
            }
            let _ = self.0.sender.try_send(None);
        }
    }
    pub(super) fn close_agent_history(&self, history: u64) {
        if self.connected() {
            *self
                .0
                .shared
                .agent_histories_closed
                .lock()
                .unwrap()
                .entry(history)
                .or_default() += 1;
            let _ = self.0.sender.try_send(None);
        }
    }
    pub(super) fn close_rollback(&self, journal: u64) {
        if self.connected() {
            self.0
                .shared
                .rollback_closed
                .lock()
                .unwrap()
                .insert(journal);
            let _ = self.0.sender.try_send(None);
        }
    }
    pub(crate) fn set_wake(&self, wake: crate::background::UiWake) {
        *self.0.shared.wake.lock().unwrap() = wake;
    }
    pub(crate) fn take_workspace_changes(&self) -> Option<watch::Batch> {
        self.0
            .shared
            .workspace_changes
            .lock()
            .unwrap()
            .remove(&self.hello().root_uri)
    }
    pub(super) fn register_history(&self) -> io::Result<(u64, mpsc::Receiver<history::Page>)> {
        let mut histories = self.0.shared.histories.lock().unwrap();
        if histories.len() >= 4 {
            return Err(io::Error::other("Remote history stream limit reached"));
        }
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::sync_channel(1);
        histories.insert(id, sender);
        Ok((id, receiver))
    }
    pub(super) fn unregister_history(&self, id: u64) {
        self.0.shared.histories.lock().unwrap().remove(&id);
    }
    pub(crate) fn notify(&self, command: Command) -> io::Result<()> {
        if !self.connected() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "Remote connection interrupted",
            ));
        }
        match command {
            Command::Cancel { request } => {
                self.0.shared.cancelled.lock().unwrap().insert(request);
            }
            Command::ProcessResize {
                process,
                width,
                height,
            } => {
                self.0
                    .shared
                    .resized
                    .lock()
                    .unwrap()
                    .insert(process, (width, height));
            }
            command => {
                return self
                    .0
                    .sender
                    .try_send(Some(self.request(0, command)))
                    .map_err(io::Error::other);
            }
        }
        // Coalesced control survives a full ordinary-data queue.
        let _ = self.0.sender.try_send(None);
        Ok(())
    }
    pub(crate) fn notify_from_worker(&self, command: Command) -> io::Result<()> {
        if !self.connected() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "Remote connection interrupted",
            ));
        }
        if matches!(command, Command::ProcessInput { .. }) {
            self.0
                .controls
                .send(self.request(0, command))
                .map_err(io::Error::other)?;
            let _ = self.0.sender.try_send(None);
            return Ok(());
        }
        self.0
            .sender
            .send(Some(self.request(0, command)))
            .map_err(io::Error::other)
    }
    pub(crate) fn credit(&self, process: u64, bytes: usize) {
        *self
            .0
            .shared
            .credits
            .lock()
            .unwrap()
            .entry(process)
            .or_default() += bytes;
        // A full queue already guarantees the writer is awake and will drain
        // credits before its next message. Credits themselves cannot be lost.
        let _ = self.0.sender.try_send(None);
    }
    #[cfg(test)]
    pub(crate) fn spawn(
        &self,
        program: String,
        args: Vec<String>,
        cwd: RemotePath,
        terminal: Option<(u16, u16)>,
        wake: crate::background::UiWake,
    ) -> io::Result<process::RemoteProcess> {
        let process = self.spawn_async(program, args, cwd, terminal, wake)?;
        {
            let mut state = process.reader.stream.state.lock().unwrap();
            while !state.started && !state.end {
                state = process.reader.stream.ready.wait(state).unwrap();
            }
            if !state.started {
                return Err(io::Error::other(
                    state
                        .error
                        .clone()
                        .unwrap_or_else(|| "Remote process failed to start".into()),
                ));
            }
        }
        Ok(process)
    }
    /// Queue Spawn before returning the pipe endpoint. Following stdin messages
    /// use the same ordered writer, so initialization cannot overtake startup.
    /// Neither process creation nor its acknowledgement waits on the UI thread.
    pub(crate) fn spawn_async(
        &self,
        program: String,
        args: Vec<String>,
        cwd: RemotePath,
        terminal: Option<(u16, u16)>,
        wake: crate::background::UiWake,
    ) -> io::Result<process::RemoteProcess> {
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        let stream = Arc::new(process::Stream::new(wake));
        {
            let mut streams = self.0.shared.streams.lock().unwrap();
            if streams.len() >= 16 {
                return Err(io::Error::other("Remote process limit reached"));
            }
            streams.insert(id, stream.clone());
        }
        let submitted = self.submit(Command::Spawn {
            process: id,
            program,
            args,
            cwd,
            terminal,
        });
        let (request_id, result) = match submitted {
            Ok(submitted) => submitted,
            Err(error) => {
                self.close_process(id);
                return Err(error);
            }
        };
        let started = stream.clone();
        let client = self.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("remote-tool-start".into())
            .spawn(move || {
                match result
                    .recv_timeout(Duration::from_secs(120))
                    .map(Delivery::take)
                {
                    Ok(Ok(Reply::Done)) => {
                        started.state.lock().unwrap().started = true;
                        started.ready.notify_all();
                    }
                    result => {
                        started.end(Some(format!("Remote process could not start: {result:?}")));
                        client.0.shared.stop(request_id, &io::Error::new(io::ErrorKind::TimedOut, "Remote process did not start"));
                        client.close_process(id);
                    }
                }
            })
        {
            self.close_process(id);
            return Err(error);
        }
        Ok(process::RemoteProcess {
            client: self.clone(),
            id,
            reader: process::Reader {
                stream,
                id,
                client: self.clone(),
            },
        })
    }
    pub(crate) fn close_process(&self, id: u64) {
        if let Some(stream) = self.0.shared.streams.lock().unwrap().remove(&id) {
            stream.end(None);
        }
        self.0.shared.credits.lock().unwrap().remove(&id);
        self.0.shared.resized.lock().unwrap().remove(&id);
        if self.connected() {
            self.0.shared.killed.lock().unwrap().insert(id);
            // Coalesced control survives a full data queue. Spawn processing
            // also verifies that the local endpoint still exists.
            let _ = self.0.sender.try_send(None);
        }
    }
    pub(crate) fn call(&self, command: Command) -> io::Result<Reply> {
        self.call_timeout(command, Duration::from_secs(120))
    }
    /// File size/slow storage cannot impose a whole-operation deadline. These
    /// waits run on workers; disconnection always wakes them through Shared::fail.
    pub(super) fn call_file(&self, command: Command) -> io::Result<Reply> {
        let (id, result) = self.submit_inner(command, Some((Duration::MAX, None)))?;
        self.receive_file(id, result, None, false)
    }
    pub(super) fn call_file_cancellable(
        &self,
        command: Command,
        cancel: &crate::background::CancellationToken,
    ) -> io::Result<Reply> {
        let (id, result) = self.submit_inner(command, Some((Duration::MAX, Some(cancel))))?;
        // Keep the admission/resource window occupied until a cancelled open's
        // reply is drained. Dropping its Delivery releases any source header.
        // Otherwise repeated clicks on a stalled link can fill every node slot
        // with headers which the client has not received yet.
        self.receive_file(id, result, Some(cancel), true)
    }
    fn receive_file(
        &self,
        id: u64,
        result: mpsc::Receiver<Delivery>,
        cancel: Option<&crate::background::CancellationToken>,
        drain_cancelled: bool,
    ) -> io::Result<Reply> {
        let reply = if let Some(cancel) = cancel {
            loop {
                if cancel.is_cancelled() {
                    if let Some(operation) = self.0.shared.operation(id) { operation.stage("Cancellation requested"); }
                    let _ = self.notify(Command::Cancel { request: id });
                    if drain_cancelled {
                        // Once cancellation has been sent there is no state to
                        // poll. Completion/disconnection wakes this receiver;
                        // dropping its delivery releases any unused header.
                        if let Err(error) = result.recv() {
                            self.0.shared.stop(id, &io::Error::other(error.to_string()));
                            return Err(io::Error::other(error));
                        }
                    } else {
                        self.0.shared.stop(id, &io::ErrorKind::Interrupted.into());
                    }
                    return Err(io::ErrorKind::Interrupted.into());
                }
                match result.recv_timeout(Duration::from_millis(50)) {
                    Ok(reply) => {
                        if cancel.is_cancelled() {
                            drop(reply);
                            return Err(io::ErrorKind::Interrupted.into());
                        }
                        break Ok(reply);
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(error) => break Err(io::Error::other(error)),
                }
            }
        } else {
            result.recv().map_err(io::Error::other)
        };
        if let Err(error) = &reply {
            self.0.shared.stop(id, error);
        }
        reply?.take().map_err(Failure::into_io)
    }
    pub(super) fn pipeline(
        &self,
        commands: impl IntoIterator<Item = Command>,
        mut consume: impl FnMut(Reply) -> io::Result<()>,
    ) -> io::Result<()> {
        self.pipeline_cancellable(commands, None, &mut consume)
    }
    pub(super) fn pipeline_cancellable(
        &self,
        commands: impl IntoIterator<Item = Command>,
        cancel: Option<&crate::background::CancellationToken>,
        mut consume: impl FnMut(Reply) -> io::Result<()>,
    ) -> io::Result<()> {
        let mut commands = commands.into_iter();
        let mut pending = std::collections::VecDeque::new();
        let result = (|| {
            loop {
                while pending.len() < 8 {
                    let Some(command) = commands.next() else {
                        break;
                    };
                    pending.push_back(self.submit_inner(command, Some((Duration::MAX, cancel)))?);
                }
                let Some((id, reply)) = pending.pop_front() else {
                    break;
                };
                consume(self.receive_file(id, reply, cancel, false)?)?;
            }
            Ok(())
        })();
        for (id, _) in pending {
            self.0.shared.stop(id, &io::ErrorKind::Interrupted.into());
            let _ = self.notify(Command::Cancel { request: id });
        }
        result
    }
    pub(crate) fn call_cancellable(
        &self,
        command: Command,
        cancel: &crate::background::CancellationToken,
    ) -> io::Result<Reply> {
        self.call_progress(command, cancel, |_| {})
    }
    pub(crate) fn call_progress(
        &self,
        command: Command,
        cancel: &crate::background::CancellationToken,
        mut progress: impl FnMut(crate::file_deletion::Progress),
    ) -> io::Result<Reply> {
        if cancel.is_cancelled() {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let filesystem = matches!(command, Command::Filesystem(_));
        let mutation = filesystem || matches!(command, Command::Replace(_));
        let (id, result) =
            self.submit_inner(command, Some((Duration::from_secs(120), Some(cancel))))?;
        if filesystem {
            self.0
                .shared
                .deletion_progress
                .lock()
                .unwrap()
                .insert(id, Default::default());
        }
        let reply = self.wait_progress(
            id,
            result,
            cancel,
            mutation,
            Duration::from_secs(120),
            &mut progress,
        );
        self.0.shared.deletion_progress.lock().unwrap().remove(&id);
        reply
    }
    fn wait_progress(
        &self,
        id: u64,
        result: mpsc::Receiver<Delivery>,
        cancel: &crate::background::CancellationToken,
        mutation: bool,
        timeout: Duration,
        progress: &mut impl FnMut(crate::file_deletion::Progress),
    ) -> io::Result<Reply> {
        let mut start = std::time::Instant::now();
        let mut cancelled = false;
        loop {
            if mutation
                && let Some(current) = self
                    .0
                    .shared
                    .deletion_progress
                    .lock()
                    .unwrap()
                    .get(&id)
                    .copied()
            {
                progress(current);
            }
            if cancel.is_cancelled() && !cancelled {
                if let Some(operation) = self.0.shared.operation(id) { operation.stage("Cancellation requested"); }
                self.notify(Command::Cancel { request: id })?;
                cancelled = true;
                start = std::time::Instant::now();
            }
            if cancelled && !mutation {
                self.0.shared.stop(id, &io::ErrorKind::Interrupted.into());
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Remote operation cancelled",
                ));
            }
            // A bulk disk mutation can legitimately take longer than one RPC
            // timeout. It stays cancellable and disconnect wakes this receiver.
            // Bound an unanswered cancellation from when it was actually sent,
            // rather than expiring a healthy long-running batch's acknowledgement.
            if (!mutation || cancelled) && start.elapsed() > timeout {
                if !cancelled {
                    let _ = self.notify(Command::Cancel { request: id });
                }
                self.0.shared.stop(id, &io::Error::new(io::ErrorKind::TimedOut, "Remote operation timed out"));
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    if mutation {
                        "Remote operation timed out; verify interrupted disk changes before retrying"
                    } else {
                        "Remote operation timed out"
                    },
                ));
            }
            match result.recv_timeout(Duration::from_millis(50)) {
                Ok(reply) => return reply.take().map_err(Failure::into_io),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => return Err(io::Error::other(error)),
            }
        }
    }
    pub(crate) fn call_timeout(&self, command: Command, timeout: Duration) -> io::Result<Reply> {
        let started = std::time::Instant::now();
        let (id, result) = self.submit_inner(command, Some((timeout, None)))?;
        let reply = result.recv_timeout(timeout.saturating_sub(started.elapsed()));
        if reply.is_err() {
            self.0.shared.stop(id, &io::Error::new(io::ErrorKind::TimedOut, "Remote request timed out; verify interrupted saves before retrying"));
        }
        reply.map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "Remote request timed out; local edits are retained. Verify interrupted saves before retrying."))?.take()
            .map_err(Failure::into_io)
    }
    fn submit(&self, command: Command) -> io::Result<(u64, mpsc::Receiver<Delivery>)> {
        self.submit_inner(command, None)
    }

    /// File/tool workers may sleep for transfer capacity; the UI's process
    /// submissions remain nonblocking. A restored tab must not turn into an
    /// error page just because two other opens occupy the memory window.
    fn submit_inner(
        &self,
        command: Command,
        wait: Option<(Duration, Option<&crate::background::CancellationToken>)>,
    ) -> io::Result<(u64, mpsc::Receiver<Delivery>)> {
        if !self.connected() {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "Remote service disconnected; local edits are retained",
            ));
        }
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        let git_operation = crate::git_output::is_git_command(&command).then(|| {
            self.0.shared.git_output.get().and_then(|log| log.git_request(
                self.0.shared.diagnostics.as_ref().map_or(0, |trace| trace.id),
                id, &command, &self.hello().root,
            ))
        }).flatten();
        let operation = git_operation.or_else(|| self
            .0
            .shared
            .diagnostics
            .as_ref()
            .and_then(|trace| trace.request(id, &command, &self.hello().root)));
        let submitted = (|| {
            let (completed, result) = mpsc::sync_channel(1);
            let retained = match &command {
                Command::Canonicalize { paths } => paths
                    .iter()
                    .map(|path| path.0.len().saturating_add(256))
                    .sum::<usize>()
                    .saturating_mul(4),
                Command::Agent(agent::Command::Send { .. }) => text::INLINE_BYTES * 4,
                Command::Agent(agent::Command::HistoryRead { .. }) => text::INLINE_BYTES * 4,
                Command::Agent(agent::Command::Start { config, .. }) => config
                    .command
                    .len()
                    .saturating_add(config.args.iter().map(String::len).sum::<usize>())
                    .saturating_add(
                        config
                            .env
                            .iter()
                            .map(|(key, value)| key.len().saturating_add(value.len()))
                            .sum::<usize>(),
                    ),
                Command::Rollback(command) => command.retained_bytes(),
                Command::Open { .. } | Command::OpenLocation { .. } | Command::Reload { .. } => {
                    text::INLINE_BYTES * 4
                }
                Command::Text(text::Command::Read { start, end, .. }) => {
                    end.saturating_sub(*start).saturating_mul(4)
                }
                Command::Text(text::Command::Append { text, .. }) => text.len(),
                Command::Save { change, .. } => change.text.len(),
                Command::Format { text, .. } => text.len(),
                Command::Binary(binary::Command::Read { .. }) => 64 * 1024,
                Command::Binary(binary::Command::Save { parts, .. }) => parts
                    .iter()
                    .map(|part| match part {
                        binary::Part::Bytes(data) => data.len(),
                        _ => std::mem::size_of::<binary::Part>(),
                    })
                    .sum(),
                Command::Spawn { program, args, .. } => {
                    program.len() + args.iter().map(String::len).sum::<usize>()
                }
                Command::Filesystem(
                    fs::Task::Paste { sources, .. } | fs::Task::Delete { paths: sources },
                ) => sources.iter().map(|p| p.0.len()).sum(),
                _ => 0,
            };
            if retained > MAX_PAYLOAD_BYTES * 2 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Remote operation exceeds the transfer memory limit",
                ));
            }
            let started = std::time::Instant::now();
            {
                let mut pending = self
                    .0
                    .shared
                    .pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                loop {
                    if !self.connected() {
                        return Err(io::Error::new(
                            io::ErrorKind::NotConnected,
                            "Remote service disconnected",
                        ));
                    }
                    if wait.is_some_and(|(_, cancel)| {
                        cancel.is_some_and(|cancel| cancel.is_cancelled())
                    }) {
                        return Err(io::Error::from(io::ErrorKind::Interrupted));
                    }
                    let held: usize = pending.values().map(|pending| pending.retained).sum();
                    if pending.len() < 8 && retained <= (MAX_PAYLOAD_BYTES * 2).saturating_sub(held)
                    {
                        break;
                    }
                    let Some((timeout, cancel)) = wait else {
                        return Err(io::Error::new(
                            io::ErrorKind::WouldBlock,
                            "Remote transfer window is busy",
                        ));
                    };
                    let remaining = timeout.saturating_sub(started.elapsed());
                    if remaining.is_zero() {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "Waiting for remote transfer capacity timed out; local edits are retained",
                        ));
                    }
                    let duration = if cancel.is_some() {
                        remaining.min(Duration::from_millis(50))
                    } else {
                        remaining.min(Duration::from_secs(60))
                    };
                    pending = self
                        .0
                        .shared
                        .capacity
                        .wait_timeout(pending, duration)
                        .unwrap_or_else(|error| error.into_inner())
                        .0;
                }
                pending.insert(
                    id,
                    Pending {
                        completed,
                        retained,
                        operation: operation.clone(),
                    },
                );
            }
            let mut request = Some(self.request(id, command));
            loop {
                match self.0.sender.try_send(request) {
                    Ok(()) => break,
                    Err(mpsc::TrySendError::Full(returned)) => {
                        request = returned;
                        let error = if !self.connected() {
                            Some(io::Error::from(io::ErrorKind::NotConnected))
                        } else if let Some((timeout, cancel)) = wait {
                            if cancel.is_some_and(|cancel| cancel.is_cancelled()) {
                                Some(io::Error::from(io::ErrorKind::Interrupted))
                            } else if started.elapsed() >= timeout {
                                Some(io::Error::from(io::ErrorKind::TimedOut))
                            } else {
                                None
                            }
                        } else {
                            Some(io::Error::new(
                                io::ErrorKind::WouldBlock,
                                "Remote submission queue is busy",
                            ))
                        };
                        if let Some(error) = error {
                            self.0.shared.take(id);
                            return Err(error);
                        }
                        let pending = self.0.shared.pending.lock().unwrap();
                        drop(
                            self.0
                                .shared
                                .capacity
                                .wait_timeout(pending, Duration::from_millis(50))
                                .unwrap(),
                        );
                    }
                    Err(mpsc::TrySendError::Disconnected(_)) => {
                        self.0.shared.take(id);
                        return Err(io::Error::from(io::ErrorKind::NotConnected));
                    }
                }
            }
            Ok((id, result))
        })();
        if let Err(error) = &submitted
            && let Some(operation) = &operation
        {
            operation.error(error);
        }
        submitted
    }
}

fn write_controls(
    writer: &mut impl Write,
    shared: &Shared,
    input: &mpsc::Receiver<Request>,
    session: u64,
) -> io::Result<()> {
    if let Some(nonce) = shared.liveness.take_outgoing() {
        wire::write_message(writer, 'C', session, 0, &wire::encode(&Request {
            id: 0, workspace: None, command: Command::Heartbeat { nonce },
        })?)?;
    }
    let batch = std::mem::take(&mut *shared.agent_closed.lock().unwrap());
    for agent in batch {
        wire::write_message(
            writer,
            'C',
            session,
            0,
            &wire::encode(&Request {
                id: 0,
                workspace: None,
                command: Command::Agent(agent::Command::Stop { agent }),
            })?,
        )?;
    }
    let batch = std::mem::take(&mut *shared.agent_cancelled.lock().unwrap());
    for (_, request) in batch {
        wire::write_message(writer, 'C', session, 0, &wire::encode(&request)?)?;
    }
    let batch = std::mem::take(&mut *shared.agent_credits.lock().unwrap());
    for (agent, event) in batch {
        wire::write_message(
            writer,
            'C',
            session,
            0,
            &wire::encode(&Request {
                id: 0,
                workspace: None,
                command: Command::Agent(agent::Command::Credit { agent, event }),
            })?,
        )?;
    }
    let batch = std::mem::take(&mut *shared.agent_histories_closed.lock().unwrap());
    for (history, count) in batch {
        for _ in 0..count {
            wire::write_message(
                writer,
                'C',
                session,
                0,
                &wire::encode(&Request {
                    id: 0,
                    workspace: None,
                    command: Command::Agent(agent::Command::HistoryClose { history }),
                })?,
            )?;
        }
    }
    let batch = std::mem::take(&mut *shared.rollback_closed.lock().unwrap());
    for journal in batch {
        wire::write_message(
            writer,
            'C',
            session,
            0,
            &wire::encode(&Request {
                id: 0,
                workspace: None,
                command: Command::Rollback(rollback::Command::Close { journal }),
            })?,
        )?;
    }
    let batch = std::mem::take(&mut *shared.text_closed.lock().unwrap());
    for transfer in batch {
        wire::write_message(
            writer,
            'C',
            session,
            0,
            &wire::encode(&Request {
                id: 0,
                workspace: None,
                command: Command::Text(text::Command::Close { transfer }),
            })?,
        )?;
    }
    let batch = std::mem::take(&mut *shared.binary_closed.lock().unwrap());
    for source in batch {
        wire::write_message(
            writer,
            'C',
            session,
            0,
            &wire::encode(&Request {
                id: 0,
                workspace: None,
                command: Command::Binary(binary::Command::Close { source }),
            })?,
        )?;
    }
    let batch = std::mem::take(&mut *shared.cancelled.lock().unwrap());
    for request in batch {
        wire::write_message(
            writer,
            'C',
            session,
            0,
            &wire::encode(&Request {
                id: 0,
                workspace: None,
                command: Command::Cancel { request },
            })?,
        )?;
    }
    let batch = std::mem::take(&mut *shared.resized.lock().unwrap());
    for (process, (width, height)) in batch {
        wire::write_message(
            writer,
            'C',
            session,
            0,
            &wire::encode(&Request {
                id: 0,
                workspace: None,
                command: Command::ProcessResize {
                    process,
                    width,
                    height,
                },
            })?,
        )?;
    }
    // One input block per bulk frame: neither a paste nor a large document
    // transfer can monopolize the link. Spawn was acknowledged before input.
    if let Ok(request) = input.try_recv() {
        wire::write_message(writer, 'C', session, 0, &wire::encode(&request)?)?;
    }
    Ok(())
}
