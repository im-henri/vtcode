//! ACP transports are independent; switching a view never tears one down.
use super::*;
#[cfg(test)]
#[path = "app_acp_agents_tests.rs"]
mod tests;

impl AgentState {
    pub(crate) fn name(&self) -> &str {
        self.initialized
            .as_ref()
            .and_then(|info| info.agent_info.as_ref())
            .map(|info| info.title.as_deref().unwrap_or(&info.name))
            .or_else(|| self.agent.as_ref().map(|config| config.name.as_str()))
            .unwrap_or("Agent Chat")
    }
    pub(crate) fn tab_name(&self) -> &str {
        self.agent
            .as_ref()
            .map_or_else(|| self.name(), |config| config.name.as_str())
    }

    pub(crate) fn activity(&self) -> &'static str {
        if self.closing {
            "Closing…"
        } else if !self.permissions.is_empty() {
            "Permission needed"
        } else if self.sessions.values().any(|session| session.busy) {
            "Working…"
        } else if !self.files.preparing.is_empty() {
            "Preparing message…"
        } else if self.error.is_some() {
            "Needs attention"
        } else if self.control_busy {
            "Connecting / loading…"
        } else if !self.unread_tabs.is_empty() {
            "New activity"
        } else if self.client.is_none() {
            "Disconnected"
        } else if self.queues.values().any(|q| !q.is_empty()) {
            "Queued"
        } else {
            "Ready"
        }
    }
    pub(super) fn checkpoint_owner(&self) -> String {
        self.agent
            .as_ref()
            .map(|config| {
                git2::Oid::hash_object(
                    git2::ObjectType::Blob,
                    &serde_json::to_vec(&(&config.command, &config.args, &config.env))
                        .unwrap_or_default(),
                )
                .unwrap()
                .to_string()
            })
            .unwrap_or_default()
    }
}

impl ChatState {
    pub(crate) fn agent_state(&self, id: AgentId) -> Option<&AgentState> {
        if id == self.active_agent {
            Some(&self.connection)
        } else {
            self.connections.get(&id)
        }
    }

    pub(crate) fn agents(&self) -> impl Iterator<Item = (AgentId, &AgentState)> {
        std::iter::once((self.active_agent, &self.connection))
            .chain(self.connections.iter().map(|(&id, state)| (id, state)))
    }

    pub(crate) fn all_sessions(&self) -> impl Iterator<Item = &Session> {
        self.agents().flat_map(|(_, state)| state.sessions.values())
    }

    pub(crate) fn any_agent_running(&self) -> bool {
        self.all_sessions().any(|session| session.busy) || self.agents().any(|(_, state)| !state.files.preparing.is_empty())
    }

    pub(crate) fn checkpoint_pending(&self) -> bool {
        self.rollback
            .conversation_busy(self.active_agent, self.selected.as_ref())
    }

    pub(super) fn configured_agent(&self, config: &AgentConfig) -> Option<AgentId> {
        self.agents().find_map(|(id, state)| {
            state
                .agent
                .as_ref()
                .filter(|open| {
                    open.command == config.command
                        && open.args == config.args
                        && open.env == config.env
                })
                .map(|_| id)
        })
    }

    pub(crate) fn switch_connection(&mut self, id: AgentId) -> bool {
        if id == self.active_agent {
            return true;
        }
        let Some(next) = self.connections.remove(&id) else {
            return false;
        };
        let old = std::mem::replace(&mut self.connection, next);
        self.connections.insert(self.active_agent, old);
        self.active_agent = id;
        true
    }
}

impl App {
    /// Temporarily service or paint another transport without selecting its tab.
    pub(crate) fn with_agent_context<T>(
        &mut self,
        id: AgentId,
        background: bool,
        work: impl FnOnce(&mut Self) -> T,
    ) -> Option<T> {
        let previous = self.chat.active_agent;
        if !self.chat.switch_connection(id) {
            return None;
        }
        let was_background = std::mem::replace(&mut self.chat.background, background);
        let result = work(self);
        self.chat.background = was_background;
        self.chat.switch_connection(previous);
        Some(result)
    }

    pub(crate) fn select_chat_agent(&mut self, id: AgentId) {
        if self.chat.agent_state(id).is_none() {
            return;
        }
        self.chat.files.dismiss();
        self.chat.picker = None;
        self.chat.focus = None;
        self.chat.switch_connection(id);
        let selected = self
            .chat
            .dock_selected
            .clone()
            .filter(|id| self.chat.open_tabs.contains(id))
            .or_else(|| {
                self.chat
                    .selected
                    .clone()
                    .filter(|id| self.chat.open_tabs.contains(id))
            })
            .or_else(|| self.chat.open_tabs.first().cloned());
        self.chat.select_tab(selected);
        self.chat.layout.active = None;
        self.chat.layout.dock_agent = id;
        self.chat.layout.dock_visible = true;
        self.chat.agent_tab_area = None;
        self.chat.reveal_agent = true;
        self.chat.hits.clear();
        self.focus_chat();
    }

    pub(super) fn connect_acp(&mut self, config: AgentConfig) {
        if self.chat.agent.is_some() && !self.chat.agent_order.contains(&self.chat.active_agent) {
            let id = self.chat.active_agent;
            self.chat.agent_order.push(id);
        }
        if let Some(id) = self.chat.configured_agent(&config) {
            self.select_chat_agent(id);
            return;
        }
        self.chat.files.dismiss();
        self.chat.picker = None;
        self.chat.focus = None;
        if self.chat.agent.is_some() {
            let id = self.chat.next_agent;
            self.chat.next_agent += 1;
            self.chat.connections.insert(id, AgentState::default());
            self.chat.switch_connection(id);
        }
        let id = self.chat.active_agent;
        if !self.chat.agent_order.contains(&id) {
            self.chat.agent_order.push(id);
        }
        self.chat.agent = Some(config.clone());
        self.chat.layout.dock_agent = id;
        self.chat.layout.active = None;
        self.chat.layout.dock_visible = true;
        self.chat.reveal_agent = true;
        self.chat.hits.clear();
        self.start_acp_connection(config, false);
    }
}
