//! Agent-independent ACP v1 client. Wire types come from the official Rust
//! schema; the stdio adapter uses threads to fit synchronous hosts without
//! requiring an async executor. No executable, mode, or tool name is special.
pub use agent_client_protocol_schema::{ProtocolVersion, v1 as protocol};

mod files;
mod model;
pub mod locations;
mod process;
pub use process::command as local_command;
pub use process::Process as LocalProcess;
pub use process::ProcessKiller;
pub use process::without_controlling_terminal;
pub mod rollback;
mod tasks;
pub mod terminal_history;
pub mod terminal_style;
mod transport;

pub use files::{FileEdit, FileRequest, FileBackend, MAX_FILE_BYTES, Workspace};
pub use files::line_range as text_range;
pub use model::{ChatEntry, Session, content_text};
pub use tasks::{TaskEntry, TaskList, TaskStatus, tool_tasks};
pub use transport::{Client, ClientHandle, Command, CommandSink, HostedEvent, Event, Operation, TerminalSnapshot, command_context};

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Explicit user configuration, never inferred from an agent vendor. Arguments
/// are passed literally (no shell interpolation). Environment inherits the
/// editor's environment, with these overrides applied only to the child.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default = "default_timeout")]
    pub request_timeout_secs: u64,
}

fn default_timeout() -> u64 {
    60
}

impl AgentConfig {
    pub fn new(name: impl Into<String>, command: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            request_timeout_secs: default_timeout(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() || self.command.trim().is_empty() {
            return Err("An ACP agent needs a name and executable command".into());
        }
        if !(1..=3600).contains(&self.request_timeout_secs) {
            return Err("ACP requestTimeoutSecs must be between 1 and 3600".into());
        }
        Ok(())
    }
}
