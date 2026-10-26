//! History stays agent-owned. Never infer emptiness from a title or opaque metadata.
use super::*;

impl ChatState {
    pub(super) fn discard_history_load(&mut self) {
        let Some(id) = self.loading_session.take() else {
            return;
        };
        self.sessions.remove(&id);
        self.forget_yolo(&id);
        self.views.remove(&Some(id.clone()));
        self.unread_tabs.remove(&id);
        self.permissions.retain(|p| p.request.session_id != id);
    }

    pub(super) fn supports_history_deletion(&self) -> bool {
        self.initialized.as_ref().is_some_and(|info| {
            info.agent_capabilities
                .session_capabilities
                .delete
                .is_some()
        })
    }

    pub(super) fn empty_conversation(&self, id: &p::SessionId) -> bool {
        !self.queues.get(id).is_some_and(|queue| !queue.is_empty()) && self.sessions
            .get(id)
            .is_some_and(|s| !s.has_conversation && !s.busy)
    }

    pub(crate) fn history_delete_unavailable(&self, id: &p::SessionId) -> Option<&'static str> {
        if self.deleting_sessions.contains(id) {
            Some("Removing conversation…")
        } else if self.queues.get(id).is_some_and(|queue| !queue.is_empty()) {
            Some("Edit or remove queued messages before deleting this chat.")
        } else if self.sessions.get(id).is_some_and(|s| s.busy) {
            Some("Stop the running response before deleting this chat.")
        } else if self.rollback.conversation_busy(self.active_agent, Some(id)) {
            Some("Wait for checkpoint capture before deleting this chat.")
        } else if self
            .sessions
            .get(id)
            .is_some_and(|s| s.rollback.edits.iter().any(|e| e.pending() > 0))
        {
            Some("Accept or decline this chat's pending changes before deleting it.")
        } else if self.control_busy {
            Some("Waiting for the agent…")
        } else {
            // A history entry need not have been loaded in this process. The
            // durable journal, rather than the transcript cache, owns its edits.
            self.rollback
                .history_delete_blocked(&self.checkpoint_owner(), id)
        }
    }

    pub(super) fn history_detail(&self, id: &p::SessionId) -> String {
        self.history_delete_unavailable(id)
            .map(str::to_owned)
            .unwrap_or_else(|| {
                if !self.supports_history_deletion() {
                    "Remove from vtcode history. The agent keeps its copy.".into()
                } else if self.open_tabs.contains(id) {
                    "Open tab".into()
                } else {
                    String::new()
                }
            })
    }

    pub(super) fn forget_conversation(&mut self, id: &p::SessionId) {
        self.close_tab(id);
        self.sessions.remove(id);
        self.forget_yolo(id);
        self.session_order.retain(|session| session != id);
        self.remote_sessions
            .retain(|session| &session.session_id != id);
        self.permissions.retain(|p| &p.request.session_id != id);
        self.views.remove(&Some(id.clone()));
        self.unread_tabs.remove(id);
    }
}

impl App {
    pub(super) fn set_chat_history_hidden(&mut self, id: &p::SessionId, hidden: bool) -> bool {
        if let Err(error) = self.chat.history_filter.set(id, hidden) {
            self.chat.error = Some(error);
            false
        } else {
            true
        }
    }
    pub(super) fn discard_empty_chat_history(&mut self) {
        if self.chat.control_busy || !self.chat.connected() {
            return;
        }
        for id in std::mem::take(&mut self.chat.deferred_discards) {
            if self.send_acp(Command::DeleteSession(id.clone())) {
                self.chat.deleting_sessions.insert(id);
            } else {
                self.chat.deferred_discards.insert(id);
            }
        }
    }

    pub(super) fn delete_chat_history(&mut self, id: p::SessionId) {
        if let Some(reason) = self.chat.history_delete_unavailable(&id) {
            self.chat.error = Some(reason.into());
            return;
        }
        if !self.chat.supports_history_deletion() {
            if !self.set_chat_history_hidden(&id, true) {
                return;
            }
            let history_open = self.chat.picker == Some(Picker::Sessions);
            self.chat.removed_sessions.insert(id.clone());
            self.chat.forget_conversation(&id);
            if history_open {
                self.build_chat_choices(Picker::Sessions);
            }
            return;
        }
        if self.send_acp(Command::DeleteSession(id.clone())) {
            self.chat.deleting_sessions.insert(id);
            self.chat.error = None;
        }
        if self.chat.picker == Some(Picker::Sessions) {
            self.build_chat_choices(Picker::Sessions);
        }
    }
}
