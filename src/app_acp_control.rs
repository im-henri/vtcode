//! Only the reply to the outstanding control request may release its UI gate.
use super::*;

pub(super) fn request(command: &Command) -> Option<(Operation, Option<p::SessionId>)> {
    Some(match command {
        Command::NewSession => (Operation::NewSession, None),
        Command::LoadSession(id) => (Operation::LoadSession, Some(id.clone())),
        Command::ResumeSession(id) => (Operation::ResumeSession, Some(id.clone())),
        Command::ListSessions { .. } => (Operation::ListSessions, None),
        Command::Authenticate(_) => (Operation::Authenticate, None),
        Command::SetMode { session_id, .. } => (Operation::SetMode, Some(session_id.clone())),
        Command::SetConfig { session_id, .. } => (Operation::SetConfig, Some(session_id.clone())),
        Command::CloseSession(id) => (Operation::CloseSession, Some(id.clone())),
        _ => return None,
    })
}

impl AgentState {
    pub(super) fn finish_control(
        &mut self,
        operation: &Operation,
        session: Option<&p::SessionId>,
    ) -> bool {
        if matches!(
            operation,
            Operation::Prompt | Operation::Terminal | Operation::DeleteSession
        ) {
            return false;
        }
        if self
            .control_request
            .as_ref()
            .is_some_and(|(pending, id)| pending != operation || id.as_ref() != session)
        {
            return false;
        }
        self.control_request = None;
        self.control_busy = false;
        true
    }
}

#[cfg(test)]
#[path = "app_acp_control_tests.rs"]
mod tests;
