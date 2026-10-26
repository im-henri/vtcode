//! Session-scoped prompts wait for the previous turn and its checkpoint to settle.
use super::*;
#[cfg(test)]
#[path = "app_acp_queue_tests.rs"]
mod tests;

#[derive(Clone, Debug)]
pub(crate) struct QueuedPrompt {
    pub id: u64,
    pub text: String,
    pub files: Vec<PathBuf>,
    pub paste: Option<std::sync::Arc<crate::app_chat_files::hosted::paste::FrozenPaste>>,
}

#[derive(Default)]
pub(crate) struct PromptQueue {
    pub items: VecDeque<QueuedPrompt>,
    pub paused: bool,
    pub scroll: usize,
    pub visible_rows: usize,
    pub priority: Option<u64>,
    // Retain the original draft until the checkpoint worker has actually sent it.
    pub inflight: Option<QueuedPrompt>,
    pub pause_after_priority: bool,
    pub context_direct: Option<u64>,
}
impl PromptQueue {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty() && self.inflight.is_none()
    }
}

impl ChatState {
    pub(crate) fn has_queued_prompt(&self) -> bool {
        self.selected
            .as_ref()
            .and_then(|id| self.queues.get(id))
            .is_some_and(|q| !q.is_empty())
    }
}

impl App {
    pub(super) fn queue_remote_chat_prompt(&mut self) {
        let Some(id) = self.chat.selected.clone().filter(|_| self.chat.connected()) else {
            self.chat.error = Some("Connect an agent and create a conversation first.".into()); return;
        };
        if !self.commit_remote_chat_attachments_for_send() { return; }
        let direct = !self.chat.busy() && !self.chat.has_queued_prompt() && !self.chat.checkpoint_pending();
        self.enqueue_chat_prompt();
        if direct && let Some(queue) = self.chat.queues.get_mut(&id) {
            queue.context_direct = queue.items.front().map(|prompt| prompt.id);
        }
        self.dispatch_chat_queue();
    }

    pub(super) fn enqueue_chat_prompt(&mut self) {
        if self.chat.control_busy && !self.chat.busy() {
            return;
        }
        let Some(id) = self.chat.selected.clone().filter(|_| self.chat.connected()) else {
            return;
        };
        if self.chat.input.trim().is_empty() && self.chat.files.attached.is_empty() {
            return;
        }
        if self.chat.queues.get(&id).is_some_and(|queue| queue.items.len() >= 32) {
            self.chat.error = Some("The queue is full (32 messages). Your draft is still in the composer.".into());
            return;
        }
        let paste = if self.remote_workspace.is_some() { self.freeze_remote_chat_paste() } else { None };
        let state = &mut self.chat.connection;
        let queue = state.queues.entry(id).or_default();
        if queue.is_empty() {
            queue.paused = false;
            queue.priority = None;
            queue.pause_after_priority = false;
        }
        state.next_queue_id += 1;
        queue.items.push_back(QueuedPrompt {
            id: state.next_queue_id,
            text: std::mem::take(&mut state.input),
            files: std::mem::take(&mut state.files.attached),
            paste,
        });
        queue.scroll = queue.items.len().saturating_sub(3);
        state.cursor = 0;
        state.anchor = None;
        state.input_history = Default::default();
        state.input_goal = None;
        state.files.scroll = 0;
        state.files.dismiss();
        state.error = None;
        state.focus = None;
        self.background.wake_handle().notify();
    }

    pub(super) fn edit_queued_prompt(&mut self, item: u64) {
        if !self.chat.input.is_empty()
            || !self.chat.files.attached.is_empty()
            || self.chat.terminal_input.is_some()
            || self.chat.loading_id
        {
            self.chat.error =
                Some("Queue or clear your current draft before editing a queued message.".into());
            return;
        }
        let Some(queue) = self
            .chat.connection
            .selected
            .as_ref()
            .and_then(|id| self.chat.connection.queues.get_mut(id))
        else {
            return;
        };
        let Some(index) = queue.items.iter().position(|p| p.id == item) else {
            return;
        };
        let prompt = queue.items.remove(index).unwrap();
        if queue.context_direct == Some(item) { queue.context_direct = None; }
        if queue.priority == Some(item) {
            queue.priority = None;
            queue.paused = true;
            queue.pause_after_priority = false;
        }
        if let Some(id) = self.chat.selected.clone()
            && self.chat.files.preparing.get(&id).is_some_and(|job| job.item == item)
        {
            self.chat.files.preparing.remove(&id);
        }
        self.chat.input = prompt.text;
        self.chat.files.attached = prompt.files;
        self.chat.files.scroll = 0;
        self.chat.cursor = self.chat.input.len();
        self.chat.anchor = None;
        self.chat.input_history = Default::default();
        self.chat.input_goal = None;
        self.chat.error = None;
        self.chat.focus = None;
        if let Some(paste) = prompt.paste { self.restore_remote_chat_paste(paste); }
    }

    pub(super) fn remove_queued_prompt(&mut self, item: u64) {
        if let Some(queue) = self
            .chat.connection
            .selected
            .as_ref()
            .and_then(|id| self.chat.connection.queues.get_mut(id))
        {
            queue.items.retain(|p| p.id != item);
            if queue.context_direct == Some(item) { queue.context_direct = None; }
            if queue.priority == Some(item) {
                queue.priority = None;
                queue.paused = true;
                queue.pause_after_priority = false;
            }
        }
        if let Some(id) = self.chat.selected.clone()
            && self.chat.files.preparing.get(&id).is_some_and(|job| job.item == item)
        {
            self.chat.files.preparing.remove(&id);
        }
    }

    pub(super) fn cancel_chat_turn(&mut self, id: &p::SessionId) -> bool {
        let preparing = self.chat.files.preparing.remove(id).is_some();
        if preparing {
            self.chat.status = "Message cancelled".into();
        }
        let Some(session) = self.chat.sessions.get(id) else {
            return preparing;
        };
        if !session.busy || session.cancelling {
            return true;
        }
        if self.chat.rollback.cancel_preparing(self.chat.active_agent, Some(id))
            || self.send_acp(Command::Cancel(id.clone()))
        {
            self.chat.sessions.get_mut(id).unwrap().cancelling = true;
            true
        } else {
            false
        }
    }

    pub(super) fn send_queued_prompt_now(&mut self, item: u64) {
        let Some(id) = self.chat.selected.clone() else {
            return;
        };
        if !self.chat.connected() {
            return;
        }
        let Some(queue) = self.chat.queues.get_mut(&id) else {
            return;
        };
        let Some(index) = queue.items.iter().position(|p| p.id == item) else {
            return;
        };
        let prompt = queue.items.remove(index).unwrap();
        queue.items.push_front(prompt);
        queue.priority = Some(item);
        queue.paused = false;
        queue.scroll = 0;
        if !self.cancel_chat_turn(&id) {
            self.chat.queues.get_mut(&id).unwrap().paused = true;
        }
        self.background.wake_handle().notify();
    }

    pub(super) fn pause_chat_queues(&mut self) {
        self.chat.files.preparing.clear();
        self.chat.files.attaching.clear();
        self.chat.files.pasting.clear();
        for queue in self.chat.queues.values_mut() {
            queue.paused = true;
            queue.priority = None;
            queue.pause_after_priority = false;
        }
    }

    pub(super) fn queued_checkpoint_finished(
        &mut self,
        id: &p::SessionId,
        sent: bool,
        cancelled: bool,
    ) {
        let Some(queue) = self.chat.queues.get_mut(id) else {
            return;
        };
        let Some(prompt) = queue.inflight.take() else {
            return;
        };
        if sent {
            if queue.context_direct == Some(prompt.id) { queue.context_direct = None; }
            if queue.priority == Some(prompt.id) {
                queue.priority = None;
                if std::mem::take(&mut queue.pause_after_priority) {
                    queue.paused = true;
                }
            }
        } else {
            // A forced successor stays first if the interrupted prompt never
            // reached the agent. Preserve that unsent prompt for later editing.
            let index = usize::from(
                queue
                    .items
                    .front()
                    .is_some_and(|p| Some(p.id) == queue.priority),
            );
            queue.items.insert(index, prompt);
            if !cancelled || queue.priority.is_none() {
                queue.paused = true;
            } else {
                queue.pause_after_priority = true;
            }
        }
    }

    pub(super) fn dispatch_chat_queue(&mut self) -> bool {
        if !self.chat.connected()
            || self.chat.control_busy
            || self.chat.rollback.reviewing()
        {
            return false;
        }
        let id = self
            .chat
            .session_order
            .iter()
            .find(|id| {
                self.chat
                    .queues
                    .get(*id)
                    .is_some_and(|q| !q.paused && !q.items.is_empty() && q.inflight.is_none())
                    && !self.chat.files.preparing.contains_key(*id)
                    && !self.chat.rollback.conversation_busy(self.chat.active_agent, Some(*id))
                    && !self.chat.deleting_sessions.contains(*id)
                    && !self
                        .chat
                        .permissions
                        .iter()
                        .any(|p| &p.request.session_id == *id)
                    && self.chat.sessions.get(*id).is_some_and(|s| {
                        !s.busy
                            && s.terminals
                                .values()
                                .all(|t| t.released || t.exit_status.is_some())
                    })
            })
            .cloned();
        let Some(id) = id else {
            return false;
        };
        if let Some(changed) = self.checkpoint_storage_blocked() {
            return changed;
        }
        let prompt = self.chat.queues[&id].items.front().unwrap().clone();
        if self.remote_workspace.is_some() && (!prompt.files.is_empty() || prompt.paste.is_some() || crate::app_chat_files::has_mentions(&prompt.text)) {
            if let Err(error) = self.start_remote_prompt_context(id.clone(), prompt.id, prompt.text, prompt.files, prompt.paste) {
                self.chat_context_failed(&id, prompt.id, error);
            }
            return true;
        }
        let prepared = self.chat_prompt_with_files_for(&prompt.text, &prompt.files);
        let (content, display) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => {
                self.chat.queues.get_mut(&id).unwrap().paused = true;
                let error = Some(format!("Queued message was not sent: {error}"));
                if self.chat.selected.as_ref() == Some(&id) {
                    self.chat.error = error;
                } else {
                    self.chat.views.entry(Some(id.clone())).or_default().error = error;
                }
                self.chat.mark_tab_unread(&id, self.terminal_has_focus);
                return true;
            }
        };
        self.send_prepared_queued_prompt(id, content, display, None)
    }

    fn send_prepared_queued_prompt(&mut self, id: p::SessionId, content: Vec<p::ContentBlock>, display: String, source: Option<std::sync::Arc<crate::remote::context::Source>>) -> bool {
        if !self.send_chat_with_checkpoint_context(id.clone(), content, &display, source) {
            self.chat.queues.get_mut(&id).unwrap().paused = true;
            return true;
        }
        let queue = self.chat.queues.get_mut(&id).unwrap();
        queue.inflight = queue.items.pop_front();
        self.chat
            .sessions
            .get_mut(&id)
            .unwrap()
            .begin_prompt(display);
        if !self.chat.rollback.cancel_preparing(self.chat.active_agent, Some(&id)) {
            self.queued_checkpoint_finished(&id, true, false);
        }
        self.set_chat_history_hidden(&id, false);
        self.chat.mark_tab_unread(&id, self.terminal_has_focus);
        self.chat.status = "Responding…".into();
        if self.chat.selected.as_ref() == Some(&id) {
            self.chat.follow = true;
        }
        true
    }

    fn chat_context_failed(&mut self, id: &p::SessionId, item: u64, error: String) {
        let direct = self.chat.queues.get_mut(id).is_some_and(|queue| {
            queue.paused = true;
            queue.context_direct == Some(item)
        });
        let previous = self.chat.selected.clone();
        self.chat.select_view(Some(id.clone()));
        if direct { self.edit_queued_prompt(item); }
        self.chat.error = Some(format!("Message was not sent: {error}"));
        self.chat.select_view(previous);
        self.chat.mark_tab_unread(id, self.terminal_has_focus);
    }

    pub(crate) fn apply_chat_prompt_context(&mut self, id: p::SessionId, serial: u64, result: Result<crate::app_chat_files::hosted::PreparedPrompt, String>, mut draft: Option<crate::app_chat_files::hosted::paste::ResolvedDraft>) -> bool {
        if self.chat.files.preparing.get(&id).is_none_or(|job| job.serial != serial) { return false; }
        let job = self.chat.files.preparing.remove(&id).unwrap();
        if self.chat.client.as_ref().is_none_or(|client| !client.handle().same_connection(&job.handle))
            || self.chat_file_root() != job.root
            || self.remote_workspace.as_ref().is_none_or(|workspace| workspace.view_root != job.root || !workspace.client.same_connection(&job.client))
            || self.chat.sessions.get(&id).is_none_or(|session| session.history_epoch != job.epoch)
            || self.chat.deleting_sessions.contains(&id)
            || self.chat.queues.get(&id).is_none_or(|queue| queue.paused || queue.items.front().is_none_or(|prompt| prompt.id != job.item)) {
            if let Some(queue) = self.chat.queues.get_mut(&id)
                && queue.items.front().is_some_and(|prompt| prompt.id == job.item)
            {
                queue.paused = true;
            }
            return true;
        }
        if self.chat.control_busy || self.chat.rollback.reviewing()
            || self.chat.rollback.conversation_busy(self.chat.active_agent, Some(&id))
            || self.chat.sessions.get(&id).is_some_and(|session| session.busy || session.terminals.values().any(|terminal| !terminal.released && terminal.exit_status.is_none()))
            || self.chat.permissions.iter().any(|permission| permission.request.session_id == id) {
            return true;
        }
        if let Some(draft) = &mut draft {
            let prompt = self.chat.queues.get_mut(&id).unwrap().items.front_mut().unwrap();
            prompt.text = std::mem::take(&mut draft.input.text);
            prompt.files = std::mem::take(&mut draft.files);
            prompt.paste = None;
        }
        if result.as_ref().is_ok_and(|prepared| prepared.content.is_empty()) {
            let queue = self.chat.queues.get_mut(&id).unwrap();
            let prompt = queue.items.pop_front().unwrap();
            if queue.context_direct == Some(job.item) { queue.context_direct = None; }
            if queue.priority == Some(job.item) {
                queue.priority = None;
                if queue.pause_after_priority { queue.paused = true; queue.pause_after_priority = false; }
            }
            if let Some(draft) = draft {
                let previous = self.chat.selected.clone();
                self.chat.select_view(Some(id));
                let mut restored = false;
                if self.chat.files.attached.is_empty() {
                    self.chat.with_composer_input(|text, cursor, anchor, history| {
                        if text.is_empty() {
                            *text = prompt.text; *cursor = draft.input.cursor; *anchor = draft.input.anchor; *history = draft.input.history;
                            restored = true;
                        }
                    });
                }
                if restored { self.chat.error = draft.error; }
                self.chat.select_view(previous);
            }
            self.chat.status = "Ready".into();
            return true;
        }
        match result {
            Ok(prepared) => { self.send_prepared_queued_prompt(id, prepared.content, prepared.display, prepared.source); }
            Err(error) => {
                self.chat_context_failed(&id, job.item, error);
                if let Some(draft) = draft
                    && self.chat.queues.get(&id).is_some_and(|queue| !queue.items.iter().any(|prompt| prompt.id == job.item))
                {
                    let previous = self.chat.selected.clone();
                    self.chat.select_view(Some(id));
                    self.chat.with_composer_input(|_, cursor, anchor, history| {
                        *cursor = draft.input.cursor; *anchor = draft.input.anchor; *history = draft.input.history;
                    });
                    self.chat.select_view(previous);
                }
            }
        }
        true
    }
}
