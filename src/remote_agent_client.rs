use super::*;
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

const QUEUE_BYTES: usize = 32 * 1024 * 1024;
struct Item {
    sequence: u64,
    command: acp::Command,
    cancel: crate::background::CancellationToken,
    bytes: usize,
    _context: Vec<Arc<context::Source>>,
}
struct Prompt {
    sequence: u64,
    cancel: crate::background::CancellationToken,
}
#[derive(Default)]
struct Queue {
    items: VecDeque<Item>,
    bytes: usize,
    next: u64,
    started: bool,
    prompts: HashMap<p::SessionId, Prompt>,
    current: Option<crate::background::CancellationToken>,
}
struct Lease {
    workspace: workspace::Workspace,
    id: u64,
    closed: AtomicBool,
    cancel: crate::background::CancellationToken,
    queue: Mutex<Queue>,
    ready: Condvar,
    events: std::sync::mpsc::SyncSender<acp::HostedEvent>,
    wake: crate::background::UiWake,
}
impl Lease {
    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.cancel.cancel();
        {
            let mut queue = self.queue.lock().unwrap();
            for item in &queue.items {
                item.cancel.cancel();
            }
            if let Some(current) = &queue.current {
                current.cancel();
            }
            for prompt in queue.prompts.values() {
                prompt.cancel.cancel();
            }
            // A disconnected client can remain visible in a chat. Its closed
            // queue must not retain prompt bodies or node snapshot leases.
            queue.items.clear();
            queue.prompts.clear();
            queue.bytes = 0;
        }
        self.workspace.client.close_agent(self.id);
        self.ready.notify_all();
    }
    fn event(self: &Arc<Self>, event: acp::Event, serial: Option<u64>) -> bool {
        let lease = Arc::downgrade(self);
        let mut event = acp::HostedEvent::new(event, move || {
            if let (Some(lease), Some(serial)) = (lease.upgrade(), serial) {
                lease.workspace.client.credit_agent(lease.id, serial);
            }
        });
        loop {
            if self.closed.load(Ordering::Acquire) {
                return false;
            }
            match self.events.try_send(event) {
                Ok(()) => {
                    self.wake.notify();
                    return true;
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                    self.close();
                    return false;
                }
                Err(std::sync::mpsc::TrySendError::Full(pending)) => {
                    event = pending;
                    // Only the background event worker waits. Disconnect must
                    // still release it when a host retains an undrained inbox.
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        }
    }
    fn request(&self, command: Command) -> io::Result<super::super::Reply> {
        self.workspace
            .client
            .call_cancellable(super::super::Command::Agent(command), &self.cancel)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.close();
    }
}

fn size(command: &acp::Command) -> usize {
    fn json(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::String(text) => text.len().saturating_add(32),
            serde_json::Value::Array(values) => values
                .iter()
                .fold(64usize, |total, value| total.saturating_add(json(value))),
            serde_json::Value::Object(values) => {
                values.iter().fold(64usize, |total, (key, value)| {
                    total.saturating_add(key.len()).saturating_add(json(value))
                })
            }
            _ => 32,
        }
    }
    fn content(block: &p::ContentBlock) -> usize {
        match block {
            p::ContentBlock::Text(text) => text.text.len(),
            p::ContentBlock::Image(image) => image.data.len(),
            p::ContentBlock::Audio(audio) => audio.data.len(),
            p::ContentBlock::Resource(resource) => match &resource.resource {
                p::EmbeddedResourceResource::TextResourceContents(resource) => resource.text.len(),
                p::EmbeddedResourceResource::BlobResourceContents(resource) => resource.blob.len(),
                _ => 1024,
            },
            _ => 1024,
        }
    }
    match command {
        acp::Command::Prompt {
            content: blocks, ..
        } => blocks.iter().fold(1024usize, |total, block| {
            total.saturating_add(content(block))
        }),
        acp::Command::FileResponse { result, .. } => 1024usize.saturating_add(match result {
            Ok(value) => json(value),
            Err(error) => error
                .message
                .len()
                .saturating_add(error.data.as_ref().map_or(0, json)),
        }),
        acp::Command::TerminalInput { text, .. } => text.len().saturating_add(1024),
        _ => 1024,
    }
}
impl acp::CommandSink for Lease {
    fn disconnect(&self) {
        self.close();
    }
    fn send(&self, mut command: acp::Command, blocking: bool) -> Result<(), String> {
        let context =
            context::retain(&self.workspace.client, &command).map_err(|error| error.to_string())?;
        let mut queue = self.queue.lock().unwrap();
        if self.closed.load(Ordering::Acquire) {
            return Err("The agent is disconnected".into());
        }
        if let acp::Command::Cancel(session) = command {
            let prompt = queue.prompts.get(&session).map(|prompt| {
                prompt.cancel.cancel();
                prompt.sequence
            });
            let through = prompt.unwrap_or(queue.next);
            let mut released = 0;
            queue.items.retain(|item| {
                let cancelled = item.sequence <= through && matches!(&item.command, acp::Command::Prompt { session_id, .. } if session_id == &session);
                if cancelled { released += item.bytes; }
                !cancelled
            });
            queue.bytes -= released;
            self.ready.notify_all();
            let before_start = !queue.started;
            if before_start {
                queue.prompts.remove(&session);
            }
            drop(queue);
            if before_start {
                // No prompt has been submitted yet. Its cancelled upload can
                // never reach the agent; finish it without waiting for SSH.
                if prompt.is_some() {
                    let _ = self.events.try_send(acp::HostedEvent::new(
                        acp::Event::PromptFinished {
                            session_id: session.clone(),
                            response: p::PromptResponse::new(p::StopReason::Cancelled),
                        },
                        || {},
                    ));
                    self.wake.notify();
                }
            } else {
                self.workspace
                    .client
                    .cancel_agent(self.id, session, through, prompt.is_some());
            }
            return Ok(());
        }
        if let acp::Command::FileResponse { edit, .. } = &mut command {
            *edit = None;
        }
        let bytes = size(&command);
        if bytes > QUEUE_BYTES {
            return Err("Agent input exceeds its pending-memory budget".into());
        }
        // Host requests are capped at 128 on the native ACP side. Reserve room
        // for their small failure/permission replies even when ordinary input
        // has filled its queue, so rejecting a write cannot silently hang it.
        let response = matches!(
            command,
            acp::Command::FileResponse { .. } | acp::Command::Permission { .. }
        );
        let (items, budget) = if response && bytes <= 16 * 1024 {
            (160, QUEUE_BYTES + 2 * 1024 * 1024)
        } else {
            (32, QUEUE_BYTES)
        };
        while queue.items.len() >= items || queue.bytes.saturating_add(bytes) > budget {
            if !blocking {
                return Err("The agent is busy processing input; try again".into());
            }
            queue = self.ready.wait(queue).unwrap();
            if self.closed.load(Ordering::Acquire) {
                return Err("The agent is disconnected".into());
            }
        }
        queue.next = queue
            .next
            .checked_add(1)
            .ok_or("Agent input generation overflow")?;
        let sequence = queue.next;
        let cancel = crate::background::CancellationToken::new();
        if let acp::Command::Prompt { session_id, .. } = &command {
            queue.prompts.insert(
                session_id.clone(),
                Prompt {
                    sequence,
                    cancel: cancel.clone(),
                },
            );
        }
        queue.bytes += bytes;
        queue.items.push_back(Item {
            sequence,
            command,
            cancel,
            bytes,
            _context: context,
        });
        drop(queue);
        self.ready.notify_all();
        Ok(())
    }
}

#[derive(Clone)]
struct Files {
    lease: Arc<Lease>,
    request: Option<(p::RequestId, u64)>,
}
impl std::fmt::Debug for Files {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HostedAgentFiles").finish_non_exhaustive()
    }
}
fn failure(error: impl ToString) -> p::Error {
    p::Error::new(-32000, error.to_string())
}
impl Files {
    fn prepare(&self, write: bool) -> Result<(), p::Error> {
        let (id, ticket) = self
            .request
            .as_ref()
            .ok_or_else(|| failure("File access needs an active agent request"))?;
        match self
            .lease
            .request(Command::File {
                agent: self.lease.id,
                id: id.clone(),
                ticket: *ticket,
                write,
            })
            .map_err(failure)?
        {
            super::super::Reply::Agent(Reply::FilePrepared(result)) => result,
            _ => Err(failure("Invalid agent file response")),
        }
    }
}
impl acp::FileBackend for Files {
    fn for_request(&self, id: p::RequestId, ticket: u64) -> Option<Arc<dyn acp::FileBackend>> {
        Some(Arc::new(Self {
            lease: self.lease.clone(),
            request: Some((id, ticket)),
        }))
    }
    fn resolve(&self, path: &Path) -> Result<PathBuf, p::Error> {
        let path_key = self.lease.workspace.remote_path(path).map_err(failure)?;
        if path_key.1 {
            return Err(failure("Agent file access is limited to this workspace"));
        }
        Ok(path.to_path_buf())
    }
    fn resolve_many(&self, paths: &[PathBuf]) -> Result<Vec<Option<PathBuf>>, p::Error> {
        self.lease
            .workspace
            .resolve_aliases(paths)
            .map(|paths| paths.into_iter().map(|(_, path)| path).collect())
            .map_err(failure)
    }
    fn read(
        &self,
        request: &p::ReadTextFileRequest,
        buffer: Option<&str>,
    ) -> Result<p::ReadTextFileResponse, p::Error> {
        self.resolve(&request.path)?;
        if let Some(buffer) = buffer {
            if buffer.len() > acp::MAX_FILE_BYTES {
                return Err(failure("ACP text files are limited to 8 MiB"));
            }
            return acp::text_range(buffer, request.line, request.limit)
                .map(p::ReadTextFileResponse::new);
        }
        self.prepare(false)?;
        // The node already holds the disk result for the original request.
        // Its response command substitutes that result without copying it here
        // and back over SSH. Unsaved local text takes the branch above.
        Ok(p::ReadTextFileResponse::new(String::new()))
    }
    fn write(&self, request: &p::WriteTextFileRequest) -> Result<acp::FileEdit, p::Error> {
        self.resolve(&request.path)?;
        self.prepare(true)?;
        Ok(acp::FileEdit {
            session_id: request.session_id.clone(),
            path: request.path.clone(),
            before: String::new(),
            after: String::new(),
        })
    }
}

type PageKey = (Option<usize>, usize, usize, usize, u64);
struct HistorySource {
    workspace: workspace::Workspace,
    id: u64,
    version: AtomicU64,
    history: Mutex<Weak<acp::terminal_history::TerminalHistory>>,
    pages: Mutex<VecDeque<(PageKey, acp::terminal_history::TerminalPage)>>,
}
impl Drop for HistorySource {
    fn drop(&mut self) {
        self.workspace.client.close_agent_history(self.id);
    }
}
impl acp::terminal_history::HistorySource for HistorySource {
    fn page(
        &self,
        first: Option<usize>,
        column: usize,
        width: usize,
        height: usize,
    ) -> io::Result<acp::terminal_history::TerminalPage> {
        let key = (
            first,
            column,
            width,
            height,
            self.version.load(Ordering::Acquire),
        );
        if let Some((_, page)) = self
            .pages
            .lock()
            .unwrap()
            .iter()
            .find(|(cached, _)| cached == &key)
        {
            return Ok(page.clone());
        }
        let _permit = self.workspace.client.text_permit()?;
        let result = self
            .workspace
            .client
            .call_file(super::super::Command::Agent(Command::HistoryRead {
                history: self.id,
                first,
                column,
                width,
                height,
            }))?;
        let super::super::Reply::Agent(Reply::History(data)) = result else {
            return Err(io::Error::other("Invalid terminal history response"));
        };
        let page: acp::terminal_history::TerminalPage = data.decode(&self.workspace)?;
        let mut pages = self.pages.lock().unwrap();
        pages.push_front(((first, column, width, height, page.version), page.clone()));
        pages.truncate(8);
        Ok(page)
    }
}
fn history(
    lease: &Lease,
    id: u64,
    version: u64,
    sources: &mut HashMap<u64, Weak<HistorySource>>,
) -> io::Result<Arc<acp::terminal_history::TerminalHistory>> {
    let source = if let Some(source) = sources.get(&id).and_then(Weak::upgrade) {
        source
    } else {
        if !matches!(
            lease
                .workspace
                .client
                .call_file(super::super::Command::Agent(Command::HistoryAcquire {
                    history: id
                }))?,
            super::super::Reply::Done
        ) {
            return Err(io::Error::other("Invalid terminal history acknowledgement"));
        }
        let source = Arc::new(HistorySource {
            workspace: lease.workspace.clone(),
            id,
            version: AtomicU64::new(version),
            history: Default::default(),
            pages: Default::default(),
        });
        sources.retain(|_, source| source.strong_count() != 0);
        sources.insert(id, Arc::downgrade(&source));
        source
    };
    source.version.fetch_max(version, Ordering::AcqRel);
    let mut weak = source.history.lock().unwrap();
    if let Some(history) = weak.upgrade() {
        return Ok(history);
    }
    let history = Arc::new(acp::terminal_history::TerminalHistory::hosted(
        source.clone(),
    ));
    *weak = Arc::downgrade(&history);
    Ok(history)
}

pub(crate) fn connect(
    workspace: workspace::Workspace,
    config: acp::AgentConfig,
    wake: crate::background::UiWake,
) -> io::Result<acp::Client> {
    config.validate().map_err(io::Error::other)?;
    let (id, channel) = workspace.client.register_agent()?;
    let (events, receiver) = std::sync::mpsc::sync_channel(2);
    let lease = Arc::new(Lease {
        workspace: workspace.clone(),
        id,
        closed: AtomicBool::new(false),
        cancel: Default::default(),
        queue: Default::default(),
        ready: Default::default(),
        events,
        wake,
    });
    let result = (|| -> io::Result<()> {
        let starting = Arc::downgrade(&lease);
        std::thread::Builder::new()
            .name("remote-agent-start".into())
            .spawn(move || {
                crate::worker_priority::lower_current_thread();
                let Some(lease) = starting.upgrade() else {
                    return;
                };
                let result = lease.request(Command::Start { agent: id, config });
                match result {
                    Ok(super::super::Reply::Agent(Reply::Started { agent }))
                        if agent == id && !lease.closed.load(Ordering::Acquire) =>
                    {
                        lease.queue.lock().unwrap().started = true;
                        lease.ready.notify_all();
                    }
                    result => {
                        let error = result.err().map_or_else(
                            || "Agent connection was cancelled".into(),
                            |error| error.to_string(),
                        );
                        channel.end(error);
                    }
                }
            })?;
        let reading = Arc::downgrade(&lease);
        let channel = workspace
            .client
            .agent_channel(id)
            .ok_or_else(|| io::Error::other("Agent channel was closed"))?;
        std::thread::Builder::new()
            .name("remote-agent-reader".into())
            .spawn(move || {
                crate::worker_priority::lower_current_thread();
                let mut outputs = HashMap::new();
                let mut sources = HashMap::new();
                loop {
                    let delivery = channel.recv();
                    let Some(lease) = reading.upgrade() else {
                        return;
                    };
                    if lease.closed.load(Ordering::Acquire) {
                        return;
                    }
                    let result = delivery.map_err(io::Error::other).and_then(|delivery| {
                        let serial = delivery.serial;
                        let update: Update = delivery
                            .data
                            .decode_cancellable(&lease.workspace, Some(&lease.cancel))?;
                        let (mut event, source) =
                            codec::local_event(&lease.workspace, update.event, &mut outputs)?;
                        if let Some(sequence) = update.prompt {
                            let session = match &event {
                                acp::Event::PromptFinished { session_id, .. }
                                | acp::Event::Error {
                                    operation: acp::Operation::Prompt,
                                    session_id: Some(session_id),
                                    ..
                                } => Some(session_id),
                                _ => None,
                            };
                            if let Some(session) = session {
                                let mut queue = lease.queue.lock().unwrap();
                                if queue
                                    .prompts
                                    .get(session)
                                    .is_none_or(|prompt| sequence < prompt.sequence)
                                {
                                    lease.workspace.client.credit_agent(id, serial);
                                    return Ok(true);
                                }
                                queue.prompts.remove(session);
                            }
                        }
                        if let (Some(source), acp::Event::Terminal(terminal)) = (source, &mut event)
                        {
                            terminal.history = Some(history(
                                &lease,
                                source,
                                terminal.history_version,
                                &mut sources,
                            )?);
                        }
                        let ended = matches!(event, acp::Event::Disconnected(_));
                        let sent = lease.event(event, Some(serial));
                        if ended {
                            lease.close();
                        }
                        Ok(sent && !ended)
                    });
                    match result {
                        Ok(true) => {}
                        Ok(false) => return,
                        Err(error) => {
                            if !lease.closed.load(Ordering::Acquire) {
                                lease.event(acp::Event::Disconnected(error.to_string()), None);
                            }
                            lease.close();
                            return;
                        }
                    }
                }
            })?;
        let writing = Arc::downgrade(&lease);
        std::thread::Builder::new()
            .name("remote-agent-input".into())
            .spawn(move || {
                crate::worker_priority::lower_current_thread();
                loop {
                    let Some(lease) = writing.upgrade() else {
                        return;
                    };
                    let item = {
                        let mut queue = lease.queue.lock().unwrap();
                        while (!queue.started || queue.items.is_empty())
                            && !lease.closed.load(Ordering::Acquire)
                        {
                            queue = lease.ready.wait(queue).unwrap();
                        }
                        if lease.closed.load(Ordering::Acquire) {
                            return;
                        }
                        let item = queue.items.pop_front().unwrap();
                        queue.bytes -= item.bytes;
                        queue.current = Some(item.cancel.clone());
                        lease.ready.notify_all();
                        item
                    };
                    let (operation, session) = acp::command_context(&item.command);
                    let result = (|| -> io::Result<()> {
                        if item.cancel.is_cancelled() {
                            return Err(io::Error::from(io::ErrorKind::Interrupted));
                        }
                        let packet = codec::command(&lease.workspace, item.command)?;
                        let rope = text::Data::encode_rope(&packet)?;
                        // Keep the typed packet for the usual small first
                        // attempt. Large payloads release it before uploading;
                        // only backpressure retries need to decode the Rope.
                        let mut inline = if rope.len_bytes() <= text::INLINE_BYTES {
                            Some(packet)
                        } else {
                            None
                        };
                        let blob = if rope.len_bytes() > text::INLINE_BYTES {
                            Some(lease.workspace.send_blob(&rope, &item.cancel)?)
                        } else {
                            None
                        };
                        loop {
                            let command = if let Some(blob) = &blob {
                                Command::SendSource {
                                    agent: id,
                                    sequence: item.sequence,
                                    transfer: blob.transfer,
                                }
                            } else {
                                Command::Send {
                                    agent: id,
                                    sequence: item.sequence,
                                    packet: match inline.take() {
                                        Some(packet) => packet,
                                        None => text::Data::decode_rope(&rope)?,
                                    },
                                }
                            };
                            match lease.workspace.client.call_cancellable(
                                super::super::Command::Agent(command),
                                &item.cancel,
                            ) {
                                Ok(super::super::Reply::Done) => return Ok(()),
                                Ok(_) => {
                                    return Err(io::Error::other(
                                        "Invalid agent command acknowledgement",
                                    ));
                                }
                                Err(error)
                                    if error.kind() == io::ErrorKind::WouldBlock
                                        && !item.cancel.is_cancelled()
                                        && !lease.closed.load(Ordering::Acquire) =>
                                {
                                    std::thread::sleep(Duration::from_millis(10))
                                }
                                Err(error) => return Err(error),
                            }
                        }
                    })();
                    lease.queue.lock().unwrap().current = None;
                    if let Err(error) = result {
                        if lease.closed.load(Ordering::Acquire) {
                            return;
                        }
                        if error.kind() != io::ErrorKind::Interrupted {
                            if operation == acp::Operation::Prompt
                                && let Some(session) = &session
                            {
                                let mut queue = lease.queue.lock().unwrap();
                                if queue
                                    .prompts
                                    .get(session)
                                    .is_some_and(|prompt| prompt.sequence == item.sequence)
                                {
                                    queue.prompts.remove(session);
                                }
                            }
                            lease.event(
                                acp::Event::Error {
                                    operation,
                                    session_id: session,
                                    error: failure(error),
                                },
                                None,
                            );
                        }
                    }
                }
            })?;
        Ok(())
    })();
    if let Err(error) = result {
        lease.close();
        return Err(error);
    }
    let files = Arc::new(Files {
        lease: lease.clone(),
        request: None,
    });
    let hosted_workspace = acp::Workspace::hosted(workspace.view_root.clone(), files);
    Ok(acp::Client::hosted(hosted_workspace, lease, receiver))
}
