//! Ephemeral, per-conversation automatic approvals. Durable agent rules stay agent-owned.
use super::*;

#[cfg(test)]
#[path = "app_acp_permissions_tests.rs"]
mod tests;

pub(super) struct YoloApproval {
    pub(super) ticket: u64,
    session: p::SessionId,
    option: p::PermissionOptionId,
    tool: String,
}

impl ChatState {
    pub(crate) fn yolo_enabled(&self) -> bool {
        self.selected
            .as_ref()
            .is_some_and(|id| self.yolo_sessions.contains(id))
    }

    pub(super) fn toggle_yolo(&mut self) -> bool {
        if self.initialized.is_none() || self.closing {
            return false;
        }
        let Some(id) = self.session().map(|session| session.id.clone()) else {
            return false;
        };
        if self.yolo_sessions.remove(&id) {
            false
        } else {
            self.yolo_sessions.insert(id);
            true
        }
    }

    pub(super) fn forget_yolo(&mut self, session: &p::SessionId) {
        self.yolo_sessions.remove(session);
        self.yolo_approvals
            .retain(|_, approval| &approval.session != session);
    }

    pub(super) fn reset_yolo(&mut self) {
        self.yolo_sessions.clear();
        self.yolo_approvals.clear();
    }
}

impl App {
    pub(super) fn receive_chat_permission(
        &mut self,
        id: p::RequestId,
        ticket: u64,
        request: p::RequestPermissionRequest,
        send: impl FnOnce(&mut Self, Command) -> bool,
    ) {
        if self.chat.yolo_approvals.get(&id).is_some_and(|approval| approval.ticket == ticket)
            || self.chat.permissions.iter().any(|pending| pending.id == id && pending.ticket == ticket)
        {
            return;
        }
        self.chat.yolo_approvals.remove(&id);
        self.chat.permissions.retain(|pending| pending.id != id);
        self.chat.mark_tab_unread(&request.session_id, self.terminal_has_focus);
        let tool_title = self
            .chat
            .sessions
            .get(&request.session_id)
            .and_then(|session| {
                session.entries.iter().rev().find_map(|entry| match entry {
                    ChatEntry::Tool(tool)
                        if tool.tool_call_id == request.tool_call.tool_call_id =>
                    {
                        Some(tool.title.clone())
                    }
                    _ => None,
                })
            });
        if let Some(session) = self.chat.sessions.get_mut(&request.session_id) {
            session.apply(p::SessionUpdate::ToolCallUpdate(request.tool_call.clone()));
        }
        let permission = Permission {
            id,
            ticket,
            request,
            tool_title,
        };
        if self.try_yolo_permission(&permission, send) {
            return;
        }
        if self.chat.selected.as_ref() == Some(&permission.request.session_id) {
            // Enter in an existing draft must never approve a new request.
            self.chat.focus = None;
        }
        self.chat.permissions.push_back(permission);
        if !self.chat.visible {
            self.notify("Agent needs permission · open Agent Chat");
        }
    }

    pub(super) fn toggle_chat_yolo(&mut self, mut send: impl FnMut(&mut Self, Command) -> bool) {
        if !self.chat.toggle_yolo() {
            return;
        }
        // Turning on the toggle is also an explicit opt-in for requests already
        // waiting in this conversation. Keep all other chats' cards untouched.
        let selected = self.chat.selected.clone();
        let pending = std::mem::take(&mut self.chat.connection.permissions);
        for permission in pending {
            if selected.as_ref() != Some(&permission.request.session_id)
                || !self.try_yolo_permission(&permission, |app, command| send(app, command))
            {
                self.chat.permissions.push_back(permission);
            }
        }
        if self.chat.picker == Some(Picker::Permissions) {
            if self.chat.permission().is_none() {
                self.chat.picker = None;
                self.chat.choices.clear();
            } else {
                self.build_chat_choices(Picker::Permissions);
            }
        }
    }

    fn try_yolo_permission(
        &mut self,
        permission: &Permission,
        send: impl FnOnce(&mut Self, Command) -> bool,
    ) -> bool {
        let request = &permission.request;
        // Use the request's conversation, including background chats, and check
        // the current policy for every request rather than caching it per turn.
        if !self.chat.yolo_sessions.contains(&request.session_id) {
            return false;
        }
        let tool = permission
            .tool_title
            .as_deref()
            .or(request.tool_call.fields.title.as_deref())
            .unwrap_or("Tool")
            .to_owned();
        let Some(option) = request
            .options
            .iter()
            .find(|option| option.kind == p::PermissionOptionKind::AllowOnce)
        else {
            // Never substitute Allow always: that would outlive the toggle.
            if let Some(session) = self.chat.sessions.get_mut(&request.session_id) {
                session.push(ChatEntry::Notice(format!(
                    "YOLO needs a manual choice for {tool}: the agent did not offer Allow once."
                )));
            }
            return false;
        };
        let option = option.option_id.clone();
        if !send(
            self,
            Command::Permission {
                id: permission.id.clone(),
                ticket: permission.ticket,
                option: Some(option.clone()),
            },
        ) {
            // A failed enqueue must retain the ordinary approval UI.
            return false;
        }
        self.chat.yolo_approvals.insert(
            permission.id.clone(),
            YoloApproval {
                ticket: permission.ticket,
                session: request.session_id.clone(),
                option,
                tool,
            },
        );
        true
    }

    pub(super) fn chat_permission_responded(
        &mut self,
        id: &p::RequestId,
        ticket: u64,
        option: &p::PermissionOptionId,
    ) {
        if self.chat.yolo_approvals.get(id).is_none_or(|approval| approval.ticket != ticket) { return; }
        let Some(approval) = self.chat.yolo_approvals.remove(id) else {
            return;
        };
        if &approval.option != option {
            return;
        }
        self.chat.mark_tab_unread(&approval.session, self.terminal_has_focus);
        if let Some(session) = self.chat.sessions.get_mut(&approval.session) {
            // Log only after the transport accepts the live selected response.
            // Turning YOLO off does not erase an approval already sent.
            session.push(ChatEntry::Notice(format!(
                "Accepted with YOLO · {}",
                approval.tool
            )));
        }
    }
}
