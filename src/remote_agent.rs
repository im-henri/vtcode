//! Hosted agents use the normal native ACP implementation on the compute node.
//! The local workbench keeps ordinary chat/session models and editor buffers.
use super::*;
#[path = "remote_agent_client.rs"]
mod client;
#[path = "remote_agent_codec.rs"]
mod codec;
#[path = "remote_agent_service.rs"]
mod service;
pub(crate) use client::connect;
pub(super) use service::Manager;
#[cfg(test)]
#[path = "remote_agent_tests.rs"]
mod tests;
use std::sync::{Condvar, Mutex};
#[cfg(test)]
pub(crate) use tests::config as test_config;
#[cfg(all(test, windows))]
pub(crate) use tests::ssh_roundtrip as test_ssh_roundtrip;
use vtcode::acp::{self as acp, protocol as p};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Command {
    Start {
        agent: u64,
        config: acp::AgentConfig,
    },
    Send {
        agent: u64,
        sequence: u64,
        packet: codec::Packet<acp::Command>,
    },
    SendSource {
        agent: u64,
        sequence: u64,
        transfer: u64,
    },
    Cancel {
        agent: u64,
        session: p::SessionId,
        through: u64,
        queued: bool,
    },
    Stop {
        agent: u64,
    },
    Credit {
        agent: u64,
        event: u64,
    },
    File {
        agent: u64,
        id: p::RequestId,
        ticket: u64,
        write: bool,
    },
    HistoryAcquire {
        history: u64,
    },
    HistoryRead {
        history: u64,
        first: Option<usize>,
        column: usize,
        width: usize,
        height: usize,
    },
    HistoryClose {
        history: u64,
    },
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Reply {
    Started { agent: u64 },
    FilePrepared(Result<(), p::Error>),
    History(text::Data),
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Delivery {
    pub agent: u64,
    pub serial: u64,
    pub data: text::Data,
}
#[derive(Debug, Serialize, Deserialize)]
struct Update {
    event: codec::Event,
    prompt: Option<u64>,
}

#[derive(Default)]
pub(super) struct Channel {
    state: Mutex<(std::collections::VecDeque<Delivery>, Option<String>)>,
    ready: Condvar,
}
impl Channel {
    pub(super) fn push(&self, delivery: Delivery) -> io::Result<()> {
        let mut state = self.state.lock().unwrap();
        if state.0.len() >= 8
            || matches!(&delivery.data, text::Data::Inline(text) if text.len() > text::INLINE_BYTES)
        {
            return Err(io::Error::other("Agent delivery exceeds its credit window"));
        }
        state.0.push_back(delivery);
        self.ready.notify_one();
        Ok(())
    }
    pub(super) fn end(&self, error: String) {
        self.state.lock().unwrap().1 = Some(error);
        self.ready.notify_all();
    }
    pub(super) fn recv(&self) -> Result<Delivery, String> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(delivery) = state.0.pop_front() {
                return Ok(delivery);
            }
            if let Some(error) = &state.1 {
                return Err(error.clone());
            }
            state = self.ready.wait(state).unwrap();
        }
    }
}
