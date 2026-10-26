//! Best-effort disposal of unused agent sessions on an orderly disconnect.
use super::*;

impl Worker {
    pub(super) fn discard_empty_sessions(&mut self) {
        if !self.initialized.as_ref().is_some_and(|info| {
            info.agent_capabilities
                .session_capabilities
                .delete
                .is_some()
        }) {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut attempted = HashSet::new();
        loop {
            let empty: Vec<_> = self
                .empty_sessions
                .iter()
                .filter(|id| {
                    !attempted.contains(*id)
                        && !self
                            .pending
                            .values()
                            .any(|p| p.session.as_ref() == Some(*id))
                })
                .cloned()
                .collect();
            for id in empty {
                attempted.insert(id.clone());
                let _ = self.command(Command::DeleteSession(id));
            }
            let waiting = self.pending.values().any(|p| {
                matches!(
                    p.operation,
                    Operation::NewSession | Operation::DeleteSession
                ) || p
                    .session
                    .as_ref()
                    .is_some_and(|id| self.empty_sessions.contains(id))
            });
            if !waiting || Instant::now() >= deadline {
                break;
            }
            match self
                .receiver
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(Work::Frame(frame))
                    if frame.get("method").is_none()
                        || frame.get("method").and_then(Value::as_str)
                            == Some("session/update") =>
                {
                    if self.frame(frame).is_err() {
                        break;
                    }
                }
                Ok(Work::Closed(_)) | Err(_) => break,
                // Never start queued prompts or host actions during shutdown.
                _ => {}
            }
        }
    }
}
