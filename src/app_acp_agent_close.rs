//! Agent tabs close independently, after their checkpoint workers settle.
use super::*;

impl ChatState {
    pub(crate) fn agent_open_conversations(&self, agent: AgentId) -> usize {
        let Some(state) = self.agent_state(agent) else {
            return 0;
        };
        state
            .open_tabs
            .iter()
            .chain(
                self.layout
                    .editors
                    .values()
                    .filter(|view| view.agent == agent)
                    .map(|view| &view.session),
            )
            .collect::<HashSet<_>>()
            .len()
    }
}

impl App {
    pub(super) fn request_close_chat_agent(&mut self, id: AgentId) {
        let Some(state) = self.chat.agent_state(id) else {
            return;
        };
        if state.closing || !self.chat.agent_order.contains(&id) {
            return;
        }
        let needs_confirmation = self.chat.agent_open_conversations(id) > 0
            || state.has_chat_drafts()
            || state.control_busy
            || state.queues.values().any(|queue| !queue.is_empty())
            || state.sessions.values().any(|session| {
                session.busy
                    || session
                        .terminals
                        .values()
                        .any(|terminal| !terminal.released && terminal.exit_status.is_none())
            });
        if needs_confirmation {
            self.chat.confirm_close_agent = Some(id);
            self.chat.picker = None;
            self.chat.files.dismiss();
            self.modal = Default::default();
        } else {
            self.close_chat_agent(id);
        }
    }

    pub(crate) fn confirm_close_chat_agent(&mut self) {
        if let Some(id) = self.chat.confirm_close_agent.take() {
            self.close_chat_agent(id);
        }
    }

    fn close_chat_agent(&mut self, id: AgentId) {
        if !self.chat.agent_order.contains(&id) {
            return;
        }
        self.with_agent_context(id, true, |app| {
            app.chat_action(ChatAction::Disconnect);
            app.chat.closing = true;
            app.chat.opening_group = None;
            app.chat.picker = None;
            app.chat.files.dismiss();
            app.chat.status = "Closing agent…".into();
        });
        self.finish_closing_chat_agents();
    }

    pub(super) fn finish_closing_chat_agents(&mut self) -> bool {
        let closing: Vec<_> = self
            .chat
            .agents()
            .filter_map(|(id, state)| state.closing.then_some(id))
            .collect();
        let mut changed = false;
        for id in closing {
            // Do not orphan journal jobs or file-write guards by removing their
            // owning connection before their replies have been handled.
            if self.chat.rollback.agent_capture_failed(id) {
                self.with_agent_context(id, true, |app| {
                    app.chat.closing = false;
                    app.chat.error = Some("The agent was stopped, but a rollback checkpoint could not be saved. Retry the checkpoint before closing this agent.".into());
                });
                changed = true;
            } else if !self.chat.rollback.agent_busy(id) {
                self.remove_chat_agent(id);
                changed = true;
            }
        }
        changed
    }

    fn remove_chat_agent(&mut self, id: AgentId) {
        let position = self
            .chat
            .agent_order
            .iter()
            .position(|agent| *agent == id)
            .unwrap_or(0);
        self.chat.agent_order.retain(|agent| *agent != id);
        let next = self
            .chat
            .agent_order
            .get(position)
            .or_else(|| self.chat.agent_order.last())
            .copied();
        let was_active = self.chat.active_agent == id;
        let dock_closed = self.chat.layout.dock_agent == id;
        if was_active {
            if let Some(next) = next {
                self.chat.switch_connection(next);
            } else {
                // Never reuse an ID: stale mouse targets and asynchronous work
                // must not address a newly opened connection.
                self.chat.active_agent = self.chat.next_agent;
                self.chat.next_agent += 1;
                self.chat.connection = AgentState::default();
            }
        }
        self.chat.connections.remove(&id);
        self.chat.layout.editors.retain(|_, view| view.agent != id);
        self.chat
            .layout
            .surfaces
            .retain(|surface| surface.agent != id);
        self.prune_editor_chats();
        if dock_closed {
            self.chat.layout.dock_agent = next.unwrap_or(self.chat.active_agent);
        }
        self.chat.agent_tab_offset = self
            .chat
            .agent_tab_offset
            .min(self.chat.agent_order.len().saturating_sub(1));
        self.chat.reveal_agent = true;
        if was_active {
            let pane = self.active_pane;
            let visible = self.chat.visible;
            let dock_visible = self.chat.layout.dock_visible;
            self.select_chat_agent(self.chat.layout.dock_agent);
            if pane != ActivePane::Chat {
                self.active_pane = pane;
                self.chat.visible = visible;
                self.chat.layout.dock_visible = dock_visible;
            }
        }
    }
}
