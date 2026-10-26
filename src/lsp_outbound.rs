//! Remote language servers may stop reading for seconds. Retain the latest
//! document snapshot between request barriers, with a hard queue bound.
use super::LspOutbound;
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, mpsc},
};

pub(super) enum Sender {
    Local(mpsc::Sender<LspOutbound>),
    Remote(Arc<Shared>),
}
pub(super) struct Receiver(Arc<Shared>);
pub(super) struct Shared {
    state: Mutex<State>,
    ready: Condvar,
}
#[derive(Default)]
struct State {
    messages: VecDeque<LspOutbound>,
    json_bytes: usize,
    closed: bool,
}

pub(super) fn channel() -> (Sender, Receiver) {
    let shared = Arc::new(Shared {
        state: Mutex::default(),
        ready: Condvar::new(),
    });
    (Sender::Remote(shared.clone()), Receiver(shared))
}
impl Sender {
    pub(super) fn send(&self, message: LspOutbound) -> std::io::Result<()> {
        let Self::Remote(shared) = self else {
            let Self::Local(sender) = self else {
                unreachable!()
            };
            return sender.send(message).map_err(std::io::Error::other);
        };
        let mut state = shared.state.lock().unwrap();
        if state.closed {
            return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe));
        }
        if let LspOutbound::DidChange { uri, version, text } = &message {
            for previous in state.messages.iter_mut().rev() {
                match previous {
                    LspOutbound::DidChange {
                        uri: previous_uri,
                        version: previous_version,
                        text: previous_text,
                    }
                    | LspOutbound::DidOpen {
                        uri: previous_uri,
                        version: previous_version,
                        text: previous_text,
                        ..
                    } if previous_uri == uri => {
                        *previous_version = *version;
                        *previous_text = text.clone();
                        return Ok(());
                    }
                    LspOutbound::Json(_)
                    | LspOutbound::DidClose { .. }
                    | LspOutbound::SyncKind(_) => break,
                    _ => {}
                }
            }
        }
        let bytes = match &message {
            LspOutbound::Json(json) => json.len(),
            _ => 0,
        };
        if state.messages.len() >= 256
            || bytes > (4 * 1024 * 1024usize).saturating_sub(state.json_bytes)
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "Remote language server is congested; editor input remains local",
            ));
        }
        state.json_bytes += bytes;
        state.messages.push_back(message);
        drop(state);
        shared.ready.notify_one();
        Ok(())
    }
}
impl Drop for Sender {
    fn drop(&mut self) {
        if let Self::Remote(shared) = self {
            shared.state.lock().unwrap().closed = true;
            shared.ready.notify_all();
        }
    }
}
impl Receiver {
    pub(super) fn recv(&self) -> Result<LspOutbound, mpsc::RecvError> {
        let mut state = self.0.state.lock().unwrap();
        while state.messages.is_empty() && !state.closed {
            state = self.0.ready.wait(state).unwrap();
        }
        let message = state.messages.pop_front().ok_or(mpsc::RecvError)?;
        if let LspOutbound::Json(json) = &message {
            state.json_bytes -= json.len();
        }
        Ok(message)
    }
}
impl Drop for Receiver {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().closed = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stalled_remote_lsp_keeps_latest_snapshot_without_crossing_requests() {
        let (tx, rx) = channel();
        for version in 1..=10_000 {
            tx.send(LspOutbound::DidChange {
                uri: "file:///code.rs".into(),
                version,
                text: ropey::Rope::from_str(&version.to_string()),
            })
            .unwrap();
        }
        tx.send(LspOutbound::Json("request".into())).unwrap();
        tx.send(LspOutbound::DidChange {
            uri: "file:///code.rs".into(),
            version: 10_001,
            text: ropey::Rope::new(),
        })
        .unwrap();
        assert!(matches!(
            rx.recv().unwrap(),
            LspOutbound::DidChange {
                version: 10_000,
                ..
            }
        ));
        assert!(matches!(rx.recv().unwrap(), LspOutbound::Json(_)));
        assert!(matches!(
            rx.recv().unwrap(),
            LspOutbound::DidChange {
                version: 10_001,
                ..
            }
        ));
        for _ in 0..256 {
            tx.send(LspOutbound::Json(String::new())).unwrap();
        }
        assert!(tx.send(LspOutbound::Json("full".into())).is_err());
        drop(rx);
        assert!(tx.send(LspOutbound::Json("closed".into())).is_err());
    }
}
