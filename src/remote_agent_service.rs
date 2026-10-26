use super::*;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

const WINDOW: usize = 2 * 1024 * 1024;
type Key = (p::RequestId, u64);
struct PendingFile {
    request: acp::FileRequest,
    cancel: crate::background::CancellationToken,
    claimed: bool,
    result: Option<Result<serde_json::Value, p::Error>>,
    edit: Option<acp::FileEdit>,
}
struct Reservation {
    bytes: usize,
    large: bool,
    source: Option<u64>,
    history: Option<u64>,
}
#[derive(Default)]
struct Window {
    pending: HashMap<(u64, u64), Reservation>,
    bytes: usize,
    large: bool,
}
#[derive(Default)]
struct Budget {
    window: Mutex<Window>,
    ready: Condvar,
}
struct History {
    owner: u64,
    source: Arc<acp::terminal_history::TerminalHistory>,
    references: usize,
    events: usize,
    closed: bool,
}
#[derive(Default)]
struct Histories {
    next: u64,
    values: HashMap<u64, History>,
    sources: HashMap<(u64, usize), u64>,
}
impl Histories {
    fn export(
        &mut self,
        owner: u64,
        source: Arc<acp::terminal_history::TerminalHistory>,
    ) -> io::Result<u64> {
        let key = (owner, Arc::as_ptr(&source) as usize);
        if let Some(id) = self.sources.get(&key).copied()
            && let Some(value) = self.values.get_mut(&id)
        {
            value.events += 1;
            return Ok(id);
        }
        if self.values.len() >= 2048 {
            return Err(io::Error::other(
                "Too many retained agent terminal histories",
            ));
        }
        self.next = self
            .next
            .checked_add(1)
            .ok_or_else(|| io::Error::other("History identity overflow"))?;
        self.values.insert(
            self.next,
            History {
                owner,
                source,
                references: 0,
                events: 1,
                closed: false,
            },
        );
        self.sources.insert(key, self.next);
        Ok(self.next)
    }
    fn retire(&mut self, id: u64) {
        if self
            .values
            .get(&id)
            .is_some_and(|value| value.references == 0 && value.events == 0)
            && let Some(value) = self.values.remove(&id)
        {
            self.sources
                .remove(&(value.owner, Arc::as_ptr(&value.source) as usize));
        }
    }
    fn finish_event(&mut self, id: u64) {
        if let Some(value) = self.values.get_mut(&id) {
            value.events = value.events.saturating_sub(1);
        }
        self.retire(id);
    }
}
struct Entry {
    id: u64,
    root: PathBuf,
    workspace: acp::Workspace,
    handle: acp::ClientHandle,
    closed: AtomicBool,
    files: Mutex<HashMap<Key, PendingFile>>,
    active: Mutex<HashMap<p::SessionId, u64>>,
    cancelled: Mutex<HashMap<p::SessionId, u64>>,
    synthetic: Mutex<BTreeMap<String, (p::SessionId, u64)>>,
    wake: mpsc::SyncSender<()>,
    finished: Arc<(Mutex<bool>, Condvar)>,
}
#[derive(Default)]
struct Notifications {
    stops: std::collections::HashSet<u64>,
    credits: std::collections::HashSet<(u64, u64)>,
    histories: HashMap<u64, usize>,
}
struct Flow {
    pending: Arc<Mutex<Notifications>>,
    wake: mpsc::SyncSender<()>,
}
pub(crate) struct Manager {
    entries: Mutex<HashMap<u64, Arc<Entry>>>,
    budget: Arc<Budget>,
    histories: Arc<Mutex<Histories>>,
    text: Arc<Mutex<text::Store>>,
    output: mpsc::SyncSender<Response>,
    serial: AtomicU64,
    retiring: Mutex<Vec<(acp::ClientHandle, Arc<(Mutex<bool>, Condvar)>)>>,
    flow: Mutex<Option<Flow>>,
}
impl Manager {
    /// Resource notifications describe bounded live handles, so coalesce them
    /// instead of filling an RPC mailbox with a burst of individual drops.
    /// The reader only updates memory; history cleanup may touch a slow disk.
    pub(crate) fn notify(self: &Arc<Self>, command: Command) -> io::Result<()> {
        let mut flow = self.flow.lock().unwrap();
        if flow.is_none() {
            let (wake, receiver) = mpsc::sync_channel(1);
            let pending = Arc::new(Mutex::new(Notifications::default()));
            let work = pending.clone();
            let manager = Arc::downgrade(self);
            std::thread::Builder::new()
                .name("remote-agent-flow".into())
                .spawn(move || {
                    crate::worker_priority::lower_current_thread();
                    while receiver.recv().is_ok() {
                        let Some(manager) = manager.upgrade() else {
                            return;
                        };
                        let notifications = std::mem::take(&mut *work.lock().unwrap());
                        for agent in notifications.stops {
                            manager.stop(agent);
                        }
                        for (agent, event) in notifications.credits {
                            manager.release(agent, event);
                        }
                        for (history, count) in notifications.histories {
                            let mut histories = manager.histories.lock().unwrap();
                            if let Some(value) = histories.values.get_mut(&history) {
                                value.references = value.references.saturating_sub(count);
                            }
                            histories.retire(history);
                        }
                    }
                })?;
            *flow = Some(Flow { pending, wake });
        }
        let flow = flow.as_ref().unwrap();
        let mut pending = flow.pending.lock().unwrap();
        match command {
            Command::Stop { agent } => {
                pending.stops.insert(agent);
            }
            Command::Credit { agent, event } => {
                pending.credits.insert((agent, event));
            }
            Command::HistoryClose { history } => {
                let count = pending.histories.entry(history).or_default();
                *count = count.saturating_add(1);
            }
            _ => return Err(io::Error::other("Not an agent resource notification")),
        }
        if pending.stops.len() > 32 || pending.credits.len() > 256 || pending.histories.len() > 4096
        {
            return Err(io::Error::other(
                "Agent notifications exceed the live resource window",
            ));
        }
        drop(pending);
        let _ = flow.wake.try_send(());
        Ok(())
    }
    pub(crate) fn shutdown(&self) {
        let ids: Vec<_> = self.entries.lock().unwrap().keys().copied().collect();
        for id in ids {
            self.stop(id);
        }
        for (handle, finished) in &*self.retiring.lock().unwrap() {
            if !*finished.0.lock().unwrap() {
                handle.terminate();
            }
        }
    }
    pub(crate) fn new(
        output: mpsc::SyncSender<Response>,
        text: Arc<Mutex<text::Store>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            entries: Default::default(),
            budget: Default::default(),
            histories: Default::default(),
            text,
            output,
            serial: AtomicU64::new(1),
            retiring: Default::default(),
            flow: Default::default(),
        })
    }
    fn entry(&self, root: &Path, id: u64) -> io::Result<Arc<Entry>> {
        let entry = self
            .entries
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "Agent is disconnected"))?;
        if entry.root != root || entry.closed.load(Ordering::Acquire) {
            return Err(io::Error::other(
                "Agent belongs to another or closed workspace",
            ));
        }
        Ok(entry)
    }
    pub(crate) fn execute(
        self: &Arc<Self>,
        root: &Path,
        command: Command,
    ) -> io::Result<super::super::Reply> {
        match command {
            Command::Start { agent, config } => self.start(root, agent, config),
            Command::Stop { agent } => {
                self.stop(agent);
                Ok(super::super::Reply::Done)
            }
            Command::Credit { agent, event } => {
                self.release(agent, event);
                Ok(super::super::Reply::Done)
            }
            Command::Send {
                agent,
                sequence,
                packet,
            } => self.send(root, agent, sequence, packet),
            Command::SendSource {
                agent,
                sequence,
                transfer,
            } => {
                let rope = self.text.lock().unwrap().snapshot(transfer)?;
                self.send(root, agent, sequence, text::Data::decode_rope(&rope)?)
            }
            Command::Cancel {
                agent,
                session,
                through,
                queued,
            } => {
                let entry = self.entry(root, agent)?;
                entry
                    .cancelled
                    .lock()
                    .unwrap()
                    .entry(session.clone())
                    .and_modify(|old| *old = (*old).max(through))
                    .or_insert(through);
                for file in entry
                    .files
                    .lock()
                    .unwrap()
                    .values()
                    .filter(|file| file.request.session_id() == &session)
                {
                    file.cancel.cancel();
                }
                if entry
                    .active
                    .lock()
                    .unwrap()
                    .get(&session)
                    .is_some_and(|sequence| *sequence <= through)
                {
                    entry
                        .handle
                        .send_blocking(acp::Command::Cancel(session))
                        .map_err(io::Error::other)?;
                } else if queued {
                    entry
                        .synthetic
                        .lock()
                        .unwrap()
                        .insert(session.to_string(), (session, through));
                    let _ = entry.wake.try_send(());
                }
                Ok(super::super::Reply::Done)
            }
            Command::File {
                agent,
                id,
                ticket,
                write,
            } => self.file(root, agent, (id, ticket), write),
            Command::HistoryAcquire { history } => {
                let mut histories = self.histories.lock().unwrap();
                let value = histories
                    .values
                    .get_mut(&history)
                    .ok_or_else(|| io::Error::other("Terminal history was released"))?;
                value.references = value
                    .references
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("History reference overflow"))?;
                Ok(super::super::Reply::Done)
            }
            Command::HistoryClose { history } => {
                let mut histories = self.histories.lock().unwrap();
                if let Some(value) = histories.values.get_mut(&history) {
                    value.references = value.references.saturating_sub(1);
                }
                histories.retire(history);
                Ok(super::super::Reply::Done)
            }
            Command::HistoryRead {
                history,
                first,
                column,
                width,
                height,
            } => {
                let source = self
                    .histories
                    .lock()
                    .unwrap()
                    .values
                    .get(&history)
                    .filter(|value| value.references != 0)
                    .map(|value| value.source.clone())
                    .ok_or_else(|| io::Error::other("Terminal history is not acquired"))?;
                let page = source.page(first, column, width, height)?;
                Ok(super::super::Reply::Agent(Reply::History(
                    text::Data::encode(&self.text, &page)?,
                )))
            }
        }
    }
    fn start(
        self: &Arc<Self>,
        root: &Path,
        id: u64,
        config: acp::AgentConfig,
    ) -> io::Result<super::super::Reply> {
        let mut entries = self.entries.lock().unwrap();
        if id == 0 || entries.contains_key(&id) || entries.len() >= 8 {
            return Err(io::Error::other(
                "Agent connection limit or duplicate identity",
            ));
        }
        let (wake, awake) = mpsc::sync_channel(1);
        let signal = wake.clone();
        let storage = crate::portable::workspace_dir(root).join("agent-terminals");
        let client = acp::Client::connect_hosted(config, root, &storage, move || {
            let _ = signal.try_send(());
        })
        .map_err(io::Error::other)?;
        let entry = Arc::new(Entry {
            id,
            root: root.into(),
            workspace: client.workspace().clone(),
            handle: client.handle(),
            closed: AtomicBool::new(false),
            files: Default::default(),
            active: Default::default(),
            cancelled: Default::default(),
            synthetic: Default::default(),
            wake,
            finished: Default::default(),
        });
        entries.insert(id, entry.clone());
        let manager = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name("remote-agent-events".into())
            .spawn(move || {
                crate::worker_priority::lower_current_thread();
                struct Finished(Arc<(Mutex<bool>, Condvar)>);
                impl Drop for Finished {
                    fn drop(&mut self) {
                        *self.0.0.lock().unwrap() = true;
                        self.0.1.notify_all();
                    }
                }
                let _finished = Finished(entry.finished.clone());
                let client = client;
                let mut outputs = HashMap::new();
                while awake.recv().is_ok() {
                    if entry.closed.load(Ordering::Acquire) {
                        break;
                    }
                    let synthetic = std::mem::take(&mut *entry.synthetic.lock().unwrap());
                    for (_, (session_id, generation)) in synthetic {
                        let event = acp::Event::PromptFinished {
                            session_id,
                            response: p::PromptResponse::new(p::StopReason::Cancelled),
                        };
                        let Some(manager) = manager.upgrade() else {
                            return;
                        };
                        if let Err(error) =
                            manager.emit(&entry, event, Some(generation), &mut outputs)
                        {
                            manager.failed(entry.id, error);
                            return;
                        }
                    }
                    while let Some(event) = client.try_recv() {
                        let ended = matches!(event, acp::Event::Disconnected(_));
                        let prompt = match &event {
                            acp::Event::PromptFinished { session_id, .. }
                            | acp::Event::Error {
                                operation: acp::Operation::Prompt,
                                session_id: Some(session_id),
                                ..
                            } => entry.active.lock().unwrap().remove(session_id),
                            _ => None,
                        };
                        let Some(manager) = manager.upgrade() else {
                            return;
                        };
                        if let Err(error) = manager.emit(&entry, event, prompt, &mut outputs) {
                            manager.failed(entry.id, error);
                            return;
                        }
                        if ended {
                            return;
                        }
                    }
                }
            });
        if let Err(error) = spawned {
            entries.remove(&id);
            return Err(error);
        }
        Ok(super::super::Reply::Agent(Reply::Started { agent: id }))
    }
    fn send(
        &self,
        root: &Path,
        id: u64,
        sequence: u64,
        packet: codec::Packet<acp::Command>,
    ) -> io::Result<super::super::Reply> {
        let entry = self.entry(root, id)?;
        let mut command = codec::node_command(root, packet)?;
        context::materialize(root, &self.text, &mut command)?;
        let mut files = entry.files.lock().unwrap();
        let file = if let acp::Command::FileResponse { id, ticket, .. } = &command {
            Some((id.clone(), *ticket))
        } else {
            None
        };
        if let acp::Command::FileResponse {
            id,
            ticket,
            result,
            edit,
        } = &mut command
        {
            if let Some(pending) = files.get(&(id.clone(), *ticket)) {
                if let Some(cached) = &pending.result {
                    *result = cached.clone();
                }
                *edit = pending.edit.clone();
            } else {
                *edit = None;
            }
        }
        let mut active = entry.active.lock().unwrap();
        let prompt = if let acp::Command::Prompt { session_id, .. } = &command {
            if entry
                .cancelled
                .lock()
                .unwrap()
                .get(session_id)
                .is_some_and(|through| sequence <= *through)
            {
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            if active.contains_key(session_id) {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "This agent conversation already has a running prompt",
                ));
            }
            Some(session_id.clone())
        } else {
            None
        };
        entry
            .handle
            .send(command)
            .map_err(|error| io::Error::new(io::ErrorKind::WouldBlock, error))?;
        // Keep cached disk results until the native queue actually accepts the
        // response. A full queue must not turn a retry into an empty result.
        if let Some(file) = file {
            files.remove(&file);
        }
        if let Some(session) = prompt {
            active.insert(session, sequence);
        }
        Ok(super::super::Reply::Done)
    }
    fn file(&self, root: &Path, id: u64, key: Key, write: bool) -> io::Result<super::super::Reply> {
        let entry = self.entry(root, id)?;
        let (request, cancel) = {
            let mut files = entry.files.lock().unwrap();
            let pending = files
                .get_mut(&key)
                .ok_or_else(|| io::Error::other("Agent file request is no longer active"))?;
            if pending.claimed
                || !entry.handle.file_request_live(&key.0, key.1)
                || write != matches!(pending.request, acp::FileRequest::Write(_))
            {
                return Err(io::Error::other(
                    "Agent file request was already handled, cancelled, or has a different type",
                ));
            }
            pending.claimed = true;
            let request = match &mut pending.request {
                acp::FileRequest::Write(source) => {
                    let content = std::mem::take(&mut source.content);
                    let mut request = source.clone();
                    request.content = content;
                    acp::FileRequest::Write(request)
                }
                request => request.clone(),
            };
            (request, pending.cancel.clone())
        };
        let (result, edit) = match request {
            acp::FileRequest::Read(request) => (
                if cancel.is_cancelled() {
                    Err(p::Error::request_cancelled())
                } else {
                    entry.workspace.read(&request, None).and_then(|response| {
                        serde_json::to_value(response)
                            .map_err(|error| p::Error::new(-32603, error.to_string()))
                    })
                },
                None,
            ),
            acp::FileRequest::Write(request) => {
                match entry.workspace.write_cancellable(&request, &|| {
                    cancel.is_cancelled() || !entry.handle.file_request_live(&key.0, key.1)
                }) {
                    Ok(edit) => (Ok(serde_json::json!({})), Some(edit)),
                    Err(error) => (Err(error), None),
                }
            }
        };
        let prepared = result.as_ref().map(|_| ()).map_err(Clone::clone);
        if let Some(pending) = entry.files.lock().unwrap().get_mut(&key) {
            pending.result = Some(result);
            pending.edit = edit;
        }
        Ok(super::super::Reply::Agent(Reply::FilePrepared(prepared)))
    }
    fn emit(
        &self,
        entry: &Entry,
        mut event: acp::Event,
        prompt: Option<u64>,
        outputs: &mut HashMap<String, String>,
    ) -> io::Result<()> {
        if let acp::Event::FileRequest {
            id,
            ticket,
            request,
        } = &mut event
        {
            // Content stays beside the agent. The UI needs the validated path
            // and range to protect/read its buffer, not a round trip of writes.
            let contents = if let acp::FileRequest::Write(request) = &mut *request {
                Some(std::mem::take(&mut request.content))
            } else {
                None
            };
            let mut actual = request.clone();
            if let (Some(contents), acp::FileRequest::Write(actual)) = (contents, &mut actual) {
                actual.content = contents;
            }
            entry.files.lock().unwrap().insert(
                (id.clone(), *ticket),
                PendingFile {
                    request: actual,
                    cancel: Default::default(),
                    claimed: false,
                    result: None,
                    edit: None,
                },
            );
        }
        let history = if let acp::Event::Terminal(terminal) = &event {
            terminal
                .history
                .as_ref()
                .map(|source| {
                    self.histories
                        .lock()
                        .unwrap()
                        .export(entry.id, source.clone())
                })
                .transpose()?
        } else {
            None
        };
        struct Unpublished<'a> {
            histories: &'a Mutex<Histories>,
            id: Option<u64>,
        }
        impl Drop for Unpublished<'_> {
            fn drop(&mut self) {
                if let Some(id) = self.id {
                    self.histories.lock().unwrap().finish_event(id);
                }
            }
        }
        let mut unpublished = Unpublished {
            histories: &self.histories,
            id: history,
        };
        let wire = Update {
            event: codec::event(&entry.root, event, history, outputs)?,
            prompt,
        };
        let rope = text::Data::encode_rope(&wire)?;
        drop(wire);
        let serial = self.serial.fetch_add(1, Ordering::Relaxed);
        let bytes = rope.len_bytes().min(WINDOW);
        let large = rope.len_bytes() > text::INLINE_BYTES;
        let mut window = self.budget.window.lock().unwrap();
        while !entry.closed.load(Ordering::Acquire)
            && (window.pending.len() >= 8
                || window.bytes + bytes > WINDOW
                || (large && window.large))
        {
            window = self.budget.ready.wait(window).unwrap();
        }
        if entry.closed.load(Ordering::Acquire) {
            drop(window);
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        window.bytes += bytes;
        window.large |= large;
        window.pending.insert(
            (entry.id, serial),
            Reservation {
                bytes,
                large,
                source: None,
                history,
            },
        );
        unpublished.id = None;
        drop(window);
        let data = loop {
            if entry.closed.load(Ordering::Acquire) {
                self.release(entry.id, serial);
                return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            match text::Data::from_rope(&self.text, rope.clone()) {
                Ok(data) => break data,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => {
                    self.release(entry.id, serial);
                    return Err(error);
                }
            }
        };
        let mut window = self.budget.window.lock().unwrap();
        if let Some(pending) = window.pending.get_mut(&(entry.id, serial)) {
            pending.source = data.transfer();
        } else {
            drop(window);
            if let Some(source) = data.transfer() {
                self.text.lock().unwrap().close(source);
            }
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        drop(window);
        if let Err(error) = self.output.send(Response {
            id: 0,
            result: Ok(super::super::Reply::AgentEvent(Delivery {
                agent: entry.id,
                serial,
                data,
            })),
        }) {
            self.release(entry.id, serial);
            return Err(io::Error::other(error));
        }
        Ok(())
    }
    fn release(&self, agent: u64, event: u64) {
        let mut window = self.budget.window.lock().unwrap();
        let pending = window.pending.remove(&(agent, event));
        if let Some(pending) = &pending {
            window.bytes -= pending.bytes;
            if pending.large {
                window.large = false;
            }
        }
        drop(window);
        if let Some(pending) = pending {
            if let Some(source) = pending.source {
                self.text.lock().unwrap().close(source);
            }
            if let Some(history) = pending.history {
                self.histories.lock().unwrap().finish_event(history);
            }
            self.budget.ready.notify_all();
        }
    }
    pub(super) fn stop(&self, id: u64) {
        let Some(entry) = self.entries.lock().unwrap().remove(&id) else {
            return;
        };
        entry.closed.store(true, Ordering::Release);
        for file in entry.files.lock().unwrap().values() {
            file.cancel.cancel();
        }
        let pending: Vec<_> = self
            .budget
            .window
            .lock()
            .unwrap()
            .pending
            .keys()
            .filter(|(agent, _)| *agent == id)
            .map(|(_, event)| *event)
            .collect();
        for event in pending {
            self.release(id, event);
        }
        self.budget.ready.notify_all();
        let mut histories = self.histories.lock().unwrap();
        let owned: Vec<_> = histories
            .values
            .iter()
            .filter_map(|(key, value)| (value.owner == id).then_some(*key))
            .collect();
        for history in owned {
            if let Some(value) = histories.values.get_mut(&history) {
                value.closed = true;
            }
            histories.retire(history);
        }
        drop(histories);
        entry.handle.stop();
        let _ = entry.wake.try_send(());
        let handle = entry.handle.clone();
        let finished = entry.finished.clone();
        let mut retiring = self.retiring.lock().unwrap();
        retiring.retain(|(_, finished)| !*finished.0.lock().unwrap());
        retiring.push((handle.clone(), finished.clone()));
        drop(retiring);
        // Graceful disconnect can remove empty agent sessions. A bounded wait
        // runs only on a service worker and still owns a hard process-tree stop.
        let fallback = handle.clone();
        if std::thread::Builder::new()
            .name("agent-shutdown".into())
            .spawn(move || {
                let done = finished.0.lock().unwrap();
                let (done, _) = finished
                    .1
                    .wait_timeout_while(done, Duration::from_millis(1600), |done| !*done)
                    .unwrap();
                if !*done {
                    handle.terminate();
                }
            })
            .is_err()
        {
            fallback.terminate();
        }
    }
    fn failed(&self, agent: u64, error: io::Error) {
        self.stop(agent);
        let _ = self.output.send(Response {
            id: 0,
            result: Ok(super::super::Reply::AgentEnd {
                agent,
                error: error.to_string(),
            }),
        });
    }
}
impl Drop for Manager {
    fn drop(&mut self) {
        self.shutdown();
    }
}
