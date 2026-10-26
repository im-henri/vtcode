use super::{FileEdit, TerminalSnapshot, protocol as p};
use std::collections::HashMap;

const MAX_ENTRY_TEXT: usize = 512 * 1024;
const MAX_ENTRIES: usize = 1000;
const MAX_HISTORY_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug)]
pub enum ChatEntry {
    User(String),
    Agent(String),
    Thought(String),
    Tool(Box<p::ToolCall>),
    Plan(p::Plan),
    Edit(FileEdit),
    Terminal(String),
    Notice(String),
    Error(String),
}

#[derive(Clone, Debug)]
pub struct Session {
    pub id: p::SessionId,
    pub title: String,
    pub entries: Vec<ChatEntry>,
    pub modes: Option<p::SessionModeState>,
    pub config_options: Vec<p::SessionConfigOption>,
    pub commands: Vec<p::AvailableCommand>,
    pub usage: Option<p::UsageUpdate>,
    pub terminals: HashMap<String, TerminalSnapshot>,
    /// Latest reported TODOs survive transcript eviction and remain scoped to
    /// this conversation. A new prompt does not invent or clear task statuses.
    pub tasks: Option<super::TaskList>,
    pub busy: bool,
    pub cancelling: bool,
    pub stop_reason: Option<p::StopReason>,
    /// Message history was observed, sent, or resumed without replay.
    pub has_conversation: bool,
    pub revision: u64,
    /// Changes when old entries are evicted, invalidating index-based UI state.
    pub history_epoch: u64,
    /// Durable review metadata is independent of the bounded transcript.
    pub rollback: super::rollback::Summary,
    pub checkpoints: std::collections::BTreeMap<usize, u64>,
    entry_revisions: Vec<u64>,
    entry_bytes: Vec<usize>,
    history_bytes: usize,
    plan_index: Option<usize>,
    tasks_revision: u64,
    tool_task_revisions: HashMap<p::ToolCallId, u64>,
    local_user_echo: bool,
    stream_boundary: bool,
}

impl Session {
    pub fn new(id: p::SessionId) -> Self {
        Self {
            id,
            title: "New conversation".into(),
            entries: Vec::new(),
            modes: None,
            config_options: Vec::new(),
            commands: Vec::new(),
            usage: None,
            terminals: HashMap::new(),
            tasks: None,
            busy: false,
            cancelling: false,
            stop_reason: None,
            has_conversation: false,
            revision: 0,
            history_epoch: 0,
            rollback: Default::default(),
            checkpoints: Default::default(),
            entry_revisions: Vec::new(),
            entry_bytes: Vec::new(),
            history_bytes: 0,
            plan_index: None,
            tasks_revision: 0,
            tool_task_revisions: HashMap::new(),
            local_user_echo: false,
            stream_boundary: true,
        }
    }

    pub fn begin_prompt(&mut self, text: String) {
        self.has_conversation = true;
        if self.entries.is_empty() {
            self.title = text.chars().take(64).collect();
        }
        self.push(ChatEntry::User(text));
        self.busy = true;
        self.cancelling = false;
        self.stop_reason = None;
        self.plan_index = None;
        self.local_user_echo = true;
        self.stream_boundary = true;
    }

    pub fn finish(&mut self, reason: Option<p::StopReason>) {
        if reason.is_none() || reason == Some(p::StopReason::Cancelled) {
            for index in 0..self.entries.len() {
                if let ChatEntry::Tool(tool) = &mut self.entries[index]
                    && matches!(
                        tool.status,
                        p::ToolCallStatus::Pending | p::ToolCallStatus::InProgress
                    )
                {
                    tool.status = p::ToolCallStatus::Failed;
                    self.touch(index);
                }
            }
        }
        self.busy = false;
        self.cancelling = false;
        self.stop_reason = reason;
        self.stream_boundary = true;
        self.local_user_echo = false;
        self.revision += 1;
    }

    pub fn push(&mut self, entry: ChatEntry) {
        if matches!(&entry, ChatEntry::User(text) | ChatEntry::Agent(text) if !text.trim().is_empty())
        {
            self.has_conversation = true;
        }
        let bytes = entry_size(&entry);
        self.entries.push(entry);
        self.entry_revisions.push(self.revision + 1);
        self.entry_bytes.push(bytes);
        self.history_bytes += bytes;
        self.stream_boundary = true;
        self.revision += 1;
        self.trim();
    }

    pub fn entry_revision(&self, index: usize) -> u64 {
        self.entry_revisions
            .get(index)
            .copied()
            .unwrap_or(self.revision)
    }

    pub fn set_rollback(&mut self, summary: super::rollback::Summary) {
        self.rollback = summary;
        for index in 0..self.entries.len() {
            if matches!(self.entries[index], ChatEntry::User(_) | ChatEntry::Edit(_)) {
                self.touch(index);
            }
        }
        self.revision += 1;
    }

    pub fn set_checkpoint(&mut self, index: usize, checkpoint: u64) {
        if matches!(self.entries.get(index), Some(ChatEntry::User(_))) {
            self.checkpoints.insert(index, checkpoint);
            self.touch(index);
        }
    }

    fn touch(&mut self, index: usize) {
        self.revision += 1;
        self.entry_revisions[index] = self.revision;
        let bytes = entry_size(&self.entries[index])
            + match &self.entries[index] {
                ChatEntry::Terminal(id) => self.terminals.get(id).map_or(0, |t| t.output.len()),
                _ => 0,
            };
        self.history_bytes = self.history_bytes.saturating_sub(self.entry_bytes[index]) + bytes;
        self.entry_bytes[index] = bytes;
    }

    fn trim(&mut self) {
        if self.entries.len() <= MAX_ENTRIES && self.history_bytes <= MAX_HISTORY_BYTES {
            return;
        }
        // Preserve live terminal controls even while older chat is evicted.
        // There are at most 32 hosted commands per connection. Keep the newest
        // entry intact too, including complete diff snapshots.
        let mut remaining = self.entries.len();
        let mut removed = vec![false; remaining];
        for (index, entry) in self
            .entries
            .iter()
            .enumerate()
            .take(remaining.saturating_sub(1))
        {
            if remaining <= MAX_ENTRIES - 100 && self.history_bytes <= MAX_HISTORY_BYTES {
                break;
            }
            if matches!(entry, ChatEntry::Terminal(id) if self.terminals.get(id)
                .is_some_and(|t| !t.released && t.exit_status.is_none()))
            {
                continue;
            }
            removed[index] = true;
            remaining -= 1;
            self.history_bytes = self.history_bytes.saturating_sub(self.entry_bytes[index]);
        }
        if remaining == self.entries.len() {
            return;
        }
        let mut next = 0;
        let positions: Vec<_> = removed
            .iter()
            .map(|removed| {
                if *removed {
                    None
                } else {
                    let index = next;
                    next += 1;
                    Some(index)
                }
            })
            .collect();
        let mut index = 0;
        self.entries.retain(|_| {
            let keep = !removed[index];
            index += 1;
            keep
        });
        self.checkpoints = std::mem::take(&mut self.checkpoints)
            .into_iter()
            .filter_map(|(index, id)| positions[index].map(|index| (index, id)))
            .collect();
        index = 0;
        self.entry_bytes.retain(|_| {
            let keep = !removed[index];
            index += 1;
            keep
        });
        index = 0;
        self.entry_revisions.retain(|_| {
            let keep = !removed[index];
            index += 1;
            keep
        });
        self.plan_index = self.plan_index.and_then(|i| positions[i]);
        self.tool_task_revisions.retain(|id, _| {
            self.entries.iter()
                .any(|entry| matches!(entry, ChatEntry::Tool(tool) if &tool.tool_call_id == id))
        });
        self.terminals.retain(|id, _| {
            self.entries
                .iter()
                .any(|e| matches!(e, ChatEntry::Terminal(key) if key == id))
        });
        self.history_epoch += 1;
    }

    pub fn disconnect(&mut self) {
        self.finish(None);
        for terminal in self.terminals.values_mut() {
            terminal.released = true;
        }
        for index in 0..self.entries.len() {
            if matches!(self.entries[index], ChatEntry::Terminal(_)) {
                self.touch(index);
            }
        }
    }

    pub fn apply(&mut self, update: p::SessionUpdate) {
        self.has_conversation |= is_conversation_update(&update);
        match update {
            p::SessionUpdate::AgentMessageChunk(chunk) => self.chunk(chunk.content, 1),
            p::SessionUpdate::AgentThoughtChunk(chunk) => self.chunk(chunk.content, 2),
            p::SessionUpdate::UserMessageChunk(chunk) => {
                if !self.local_user_echo {
                    if !matches!(self.entries.last(), Some(ChatEntry::User(_))) {
                        self.plan_index = None;
                    }
                    self.chunk(chunk.content, 0);
                }
            }
            p::SessionUpdate::ToolCall(tool) => {
                let id = tool.tool_call_id.clone();
                let previous = self
                    .tool_index(&tool.tool_call_id)
                    .and_then(|index| self.tool_tasks(index));
                if let Some(index) = self.tool_index(&tool.tool_call_id) {
                    self.entries[index] = ChatEntry::Tool(Box::new(tool));
                    self.touch(index);
                } else {
                    self.push(ChatEntry::Tool(Box::new(tool)));
                }
                if let Some(index) = self.tool_index(&id) {
                    self.update_tool_tasks(index, previous);
                }
                self.stream_boundary = true;
            }
            p::SessionUpdate::ToolCallUpdate(update) => {
                let previous = self
                    .tool_index(&update.tool_call_id)
                    .and_then(|index| self.tool_tasks(index));
                let id = update.tool_call_id.clone();
                if let Some(index) = self.tool_index(&update.tool_call_id) {
                    if let ChatEntry::Tool(tool) = &mut self.entries[index] {
                        tool.update(update.fields);
                    }
                    self.touch(index);
                } else {
                    let mut tool = p::ToolCall::new(update.tool_call_id, "Tool activity");
                    tool.update(update.fields);
                    self.push(ChatEntry::Tool(Box::new(tool)));
                }
                if let Some(index) = self.tool_index(&id) {
                    self.update_tool_tasks(index, previous);
                }
                self.stream_boundary = true;
            }
            p::SessionUpdate::Plan(plan) => {
                self.tasks = Some(super::TaskList::from_plan(&plan));
                self.tasks_revision = self.revision + 1;
                if let Some(index) = self.plan_index {
                    self.entries[index] = ChatEntry::Plan(plan);
                    self.touch(index);
                } else {
                    self.push(ChatEntry::Plan(plan));
                    self.plan_index = Some(self.entries.len() - 1);
                }
            }
            p::SessionUpdate::AvailableCommandsUpdate(update) => {
                self.commands = update.available_commands
            }
            p::SessionUpdate::CurrentModeUpdate(update) => {
                if let Some(modes) = &mut self.modes {
                    modes.current_mode_id = update.current_mode_id;
                } else {
                    self.modes = Some(p::SessionModeState::new(update.current_mode_id, vec![]));
                }
            }
            p::SessionUpdate::ConfigOptionUpdate(update) => {
                self.config_options = update.config_options
            }
            p::SessionUpdate::SessionInfoUpdate(update) => {
                if let agent_client_protocol_schema::MaybeUndefined::Value(title) = update.title {
                    self.title = title;
                }
            }
            p::SessionUpdate::UsageUpdate(update) => self.usage = Some(update),
            _ => {}
        }
        self.revision += 1;
        self.trim();
    }

    fn tool_index(&self, id: &p::ToolCallId) -> Option<usize> {
        self.entries
            .iter()
            .rposition(|entry| matches!(entry, ChatEntry::Tool(tool) if &tool.tool_call_id == id))
    }

    fn tool_tasks(&self, index: usize) -> Option<super::TaskList> {
        if let ChatEntry::Tool(tool) = &self.entries[index] {
            super::tool_tasks(tool)
        } else {
            None
        }
    }

    fn update_tool_tasks(&mut self, index: usize, previous: Option<super::TaskList>) {
        if let Some(tasks) = self.tool_tasks(index)
            && let ChatEntry::Tool(tool) = &self.entries[index]
        {
            let id = tool.tool_call_id.clone();
            let revision = self.tool_task_revisions.entry(id).or_insert(self.revision);
            if previous.as_ref() != Some(&tasks) {
                *revision = self.revision;
            }
            // Only successful tools publish tasks. Use the payload's age,
            // rather than a later completion notification: a tool finishing
            // after a newer native plan must not revert that plan's statuses.
            if tool.status == p::ToolCallStatus::Completed && *revision >= self.tasks_revision {
                self.tasks_revision = *revision;
                self.tasks = Some(tasks);
            }
        }
    }

    fn chunk(&mut self, content: p::ContentBlock, role: u8) {
        let text = content_text(&content);
        if !self.stream_boundary {
            let previous = match (role, self.entries.last_mut()) {
                (0, Some(ChatEntry::User(s)))
                | (1, Some(ChatEntry::Agent(s)))
                | (2, Some(ChatEntry::Thought(s))) => Some(s),
                _ => None,
            };
            if let Some(previous) = previous {
                append_bounded(previous, &text);
                self.touch(self.entries.len() - 1);
                return;
            }
        }
        let mut bounded = String::new();
        append_bounded(&mut bounded, &text);
        self.push(match role {
            0 => ChatEntry::User(bounded),
            2 => ChatEntry::Thought(bounded),
            _ => ChatEntry::Agent(bounded),
        });
        self.stream_boundary = false;
    }

    pub fn terminal(&mut self, snapshot: TerminalSnapshot) {
        if !self.terminals.contains_key(&snapshot.id) {
            self.push(ChatEntry::Terminal(snapshot.id.clone()));
        }
        let index = self
            .entries
            .iter()
            .position(|e| matches!(e, ChatEntry::Terminal(id) if id == &snapshot.id));
        self.terminals.insert(snapshot.id.clone(), snapshot);
        if let Some(index) = index {
            self.touch(index);
        }
        self.revision += 1;
        self.trim();
    }
}

/// Metadata, startup diagnostics and configuration do not make a conversation.
pub(super) fn is_conversation_update(update: &p::SessionUpdate) -> bool {
    match update {
        p::SessionUpdate::UserMessageChunk(chunk) | p::SessionUpdate::AgentMessageChunk(chunk) => {
            !super::content_text(&chunk.content).trim().is_empty()
        }
        _ => false,
    }
}

fn entry_size(entry: &ChatEntry) -> usize {
    match entry {
        ChatEntry::User(s)
        | ChatEntry::Agent(s)
        | ChatEntry::Thought(s)
        | ChatEntry::Notice(s)
        | ChatEntry::Error(s)
        | ChatEntry::Terminal(s) => s.len(),
        ChatEntry::Edit(edit) => edit.before.len() + edit.after.len(),
        ChatEntry::Plan(plan) => plan.entries.iter().map(|e| e.content.len()).sum(),
        ChatEntry::Tool(tool) => {
            // Count serialized bytes without allocating another copy of large
            // tool outputs. This runs only when that tool changes.
            struct Counter(usize);
            impl std::io::Write for Counter {
                fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                    self.0 += bytes.len();
                    Ok(bytes.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            let mut counter = Counter(0);
            let _ = serde_json::to_writer(&mut counter, tool);
            counter.0
        }
    }
}

pub fn content_text(content: &p::ContentBlock) -> String {
    match content {
        p::ContentBlock::Text(text) => text.text.clone(),
        p::ContentBlock::ResourceLink(link) => format!("\n[{}]({})\n", link.name, link.uri),
        p::ContentBlock::Image(image) => format!("\n[Image · {}]\n", image.mime_type),
        p::ContentBlock::Audio(audio) => format!("\n[Audio · {}]\n", audio.mime_type),
        p::ContentBlock::Resource(resource) => match &resource.resource {
            p::EmbeddedResourceResource::TextResourceContents(text) => {
                format!("\n{}\n{}\n", text.uri, text.text)
            }
            p::EmbeddedResourceResource::BlobResourceContents(blob) => {
                format!("\n[Resource · {}]\n", blob.uri)
            }
            _ => "\n[Resource]\n".into(),
        },
        _ => "\n[Unsupported content]\n".into(),
    }
}

fn append_bounded(destination: &mut String, text: &str) {
    if destination.len() >= MAX_ENTRY_TEXT {
        return;
    }
    let available = MAX_ENTRY_TEXT - destination.len();
    if text.len() <= available {
        destination.push_str(text);
    } else {
        let mut end = available;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        destination.push_str(&text[..end]);
        destination.push_str("\n[Message truncated in this view]");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn update(session: &mut Session, value: serde_json::Value) {
        session.apply(serde_json::from_value(value).unwrap());
    }
    #[test]
    fn streaming_merges_only_adjacent_chunks_and_preserves_partial_tool_fields() {
        let mut session = Session::new("test".into());
        session.begin_prompt("Hello".into());
        update(
            &mut session,
            serde_json::json!({"sessionUpdate":"user_message_chunk","content":{"type":"text","text":"Hello"}}),
        );
        assert_eq!(session.entries.len(), 1);
        for text in ["Hello ", "世界"] {
            update(
                &mut session,
                serde_json::json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text}}),
            );
        }
        let first_revision = session.entry_revision(1);
        update(
            &mut session,
            serde_json::json!({"sessionUpdate":"tool_call_update","toolCallId":"late","title":"Read file","content":[{"type":"content","content":{"type":"text","text":"file contents"}}]}),
        );
        update(
            &mut session,
            serde_json::json!({"sessionUpdate":"tool_call_update","toolCallId":"late","status":"completed"}),
        );
        assert!(matches!(&session.entries[1], ChatEntry::Agent(s) if s == "Hello 世界"));
        assert_eq!(session.entry_revision(1), first_revision);
        assert!(
            matches!(&session.entries[2], ChatEntry::Tool(t) if t.title == "Read file" && t.status == p::ToolCallStatus::Completed && t.content.len() == 1)
        );
        update(
            &mut session,
            serde_json::json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"After tool"}}),
        );
        assert_eq!(session.entries.len(), 4);
    }
    #[test]
    fn history_trimming_is_bounded_and_disconnect_clears_running_activity() {
        let mut session = Session::new("test".into());
        // Entry-count eviction and checkpoint remapping have their own test.
        // Cross the byte limit once, without also reaching the entry limit.
        let huge = "x".repeat(1024 * 1024);
        for _ in 0..(MAX_HISTORY_BYTES / huge.len() + 1) {
            session.push(ChatEntry::Agent(huge.clone()));
        }
        assert!(session.entries.len() < MAX_ENTRIES && session.history_epoch > 0);
        assert!(session.history_bytes <= MAX_HISTORY_BYTES);
        update(
            &mut session,
            serde_json::json!({"sessionUpdate":"tool_call","toolCallId":"running","title":"Check","status":"in_progress"}),
        );
        session.terminal(TerminalSnapshot {
            session_id: session.id.clone(),
            id: "run".into(),
            command: "command".into(),
            output: String::new(),
            truncated: false,
            exit_status: None,
            released: false,
            cwd: Default::default(),
            history: None,
            history_version: 0,
            killed: false,
            error: None,
        });
        session.disconnect();
        assert!(session.terminals["run"].released);
        assert!(
            session
                .entries
                .iter()
                .any(|e| matches!(e, ChatEntry::Tool(t) if t.status == p::ToolCallStatus::Failed))
        );
    }

    #[test]
    fn history_trimming_preserves_live_terminal_controls_and_checkpoint_positions() {
        let mut session = Session::new("test".into());
        session.terminal(TerminalSnapshot {
            session_id: session.id.clone(),
            id: "live".into(),
            command: "waiting".into(),
            output: String::new(),
            truncated: false,
            exit_status: None,
            released: false,
            cwd: Default::default(),
            history: None,
            history_version: 0,
            killed: false,
            error: None,
        });
        for _ in 0..(MAX_ENTRIES - 3) {
            session.push(ChatEntry::Notice("activity".into()));
        }
        let index = session.entries.len();
        session.push(ChatEntry::User("checkpoint message".into()));
        session.set_checkpoint(index, 42);
        for _ in 0..300 {
            session.push(ChatEntry::Notice("activity".into()));
        }
        assert!(matches!(&session.entries[0], ChatEntry::Terminal(id) if id == "live"));
        assert!(session.terminals.contains_key("live"));
        assert!(session.entries.len() <= MAX_ENTRIES);
        let checkpoint = session
            .checkpoints
            .iter()
            .find(|(_, id)| **id == 42)
            .unwrap()
            .0;
        assert!(
            matches!(&session.entries[*checkpoint], ChatEntry::User(text) if text == "checkpoint message")
        );
        session.disconnect();
        for _ in 0..MAX_ENTRIES {
            session.push(ChatEntry::Notice("activity".into()));
        }
        assert!(!session.terminals.contains_key("live"));
        assert!(session.checkpoints.is_empty());
    }
}
