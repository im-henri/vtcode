//! Per-conversation UI state. ACP sessions keep running when their tab is hidden.
use super::*;

#[derive(Default)]
pub(super) struct ChatView {
    input: String,
    cursor: usize,
    anchor: Option<usize>,
    input_layout: crate::commit_input::CommitInputState,
    input_history: TextInputHistory,
    input_goal: Option<usize>,
    attachments: Vec<PathBuf>,
    attachment_scroll: usize,
    terminal_input: Option<String>,
    terminal_views: crate::app_chat_terminals::TerminalViews,
    loading_id: bool,
    saved_draft: Option<(String, usize, Option<usize>, TextInputHistory)>,
    pub(super) error: Option<String>,
    scroll: usize,
    max_scroll: usize,
    /// Storing the inverse keeps a fresh view following the live transcript.
    scrolled_back: bool,
    pub(super) expanded: HashSet<usize>,
    pub(super) collapsed: HashSet<usize>,
    cache: crate::ui_chat::ChatCache,
    reveal_entry: Option<usize>,
    expansion_scroll: Option<(usize, usize)>,
}

impl AgentState {
    pub(super) fn has_chat_drafts(&self) -> bool {
        !self.input.trim().is_empty()
            || !self.files.attached.is_empty()
            || self.saved_draft.as_ref().is_some_and(|draft| !draft.0.trim().is_empty())
            || self.views.values().any(|view| {
                !view.input.trim().is_empty()
                    || !view.attachments.is_empty()
                    || view.saved_draft.as_ref().is_some_and(|draft| !draft.0.trim().is_empty())
            })
    }
}

impl ChatState {
    pub(crate) fn poll_saved_terminal_views(&mut self) -> bool {
        self.views.values_mut().fold(false, |changed, view| {
            crate::app_chat_terminals::poll_views(&mut view.terminal_views) || changed
        })
    }

    fn swap_view(&mut self, view: &mut ChatView) {
        use std::mem::swap;
        swap(&mut self.input, &mut view.input);
        swap(&mut self.cursor, &mut view.cursor);
        swap(&mut self.anchor, &mut view.anchor);
        swap(&mut self.input_layout, &mut view.input_layout);
        swap(&mut self.input_history, &mut view.input_history);
        swap(&mut self.input_goal, &mut view.input_goal);
        // Keep completion generations global so an old file search cannot land
        // in a different tab. Only attachments belong to the saved view.
        swap(&mut self.files.attached, &mut view.attachments);
        swap(&mut self.files.scroll, &mut view.attachment_scroll);
        swap(&mut self.terminal_input, &mut view.terminal_input);
        swap(&mut self.terminal_views, &mut view.terminal_views);
        swap(&mut self.loading_id, &mut view.loading_id);
        swap(&mut self.saved_draft, &mut view.saved_draft);
        swap(&mut self.error, &mut view.error);
        swap(&mut self.scroll, &mut view.scroll);
        swap(&mut self.max_scroll, &mut view.max_scroll);
        let follow = self.follow;
        self.follow = !view.scrolled_back;
        view.scrolled_back = !follow;
        swap(&mut self.expanded, &mut view.expanded);
        swap(&mut self.collapsed, &mut view.collapsed);
        swap(&mut self.cache, &mut view.cache);
        swap(&mut self.reveal_entry, &mut view.reveal_entry);
        swap(&mut self.expansion_scroll, &mut view.expansion_scroll);
    }

    pub(crate) fn select_view(&mut self, id: Option<p::SessionId>) {
        if self.selected != id {
            let mut view = self.views.remove(&id).unwrap_or_default();
            self.swap_view(&mut view);
            let previous = self.selected.take();
            self.views.insert(previous, view);
            self.selected = id;
        }
    }

    pub(crate) fn select_tab(&mut self, id: Option<p::SessionId>) {
        self.files.dismiss();
        self.picker = None;
        self.choices.clear();
        self.focus = None;
        self.terminal_areas.clear();
        self.reveal_terminal = None;
        if !self.background {
            self.scroll_drag = None;
            self.scrollbars.clear();
        }
        self.select_view(id);
        if !self.background {
            self.acknowledge_selected_activity();
        }
        // A command may have exited while its input field was in another tab.
        if self.terminal_input.as_ref().is_some_and(|id| {
            !self
                .session()
                .and_then(|s| s.terminals.get(id))
                .is_some_and(|t| !t.released && !t.killed && t.exit_status.is_none())
        }) {
            self.restore_draft();
        }
        self.reveal_tab = true;
    }

    pub(super) fn open_tab(&mut self, id: p::SessionId) {
        if !self.open_tabs.contains(&id) {
            self.open_tabs.push(id.clone());
        }
        // Keep a draft typed before connecting when the first session opens.
        if self.selected.is_none() && !self.views.contains_key(&Some(id.clone())) {
            if let Some(job) = self.files.attaching.remove(&None) { self.files.attaching.insert(Some(id.clone()), job); }
            self.selected = Some(id.clone());
        }
        self.select_tab(Some(id));
        self.dock_selected = self.selected.clone();
        if self.background && let Some(id) = self.selected.clone() {
            self.unread_tabs.insert(id);
        }
        if !self.background {
            self.layout.dock_agent = self.active_agent;
            self.layout.dock_visible = true;
            self.layout.active = None;
        }
    }

    pub(super) fn close_tab(&mut self, id: &p::SessionId) {
        let index = self.open_tabs.iter().position(|tab| tab == id).unwrap_or(0);
        self.open_tabs.retain(|tab| tab != id);
        if self.selected.as_ref() == Some(id) {
            let next = self
                .open_tabs
                .get(index)
                .or_else(|| self.open_tabs.last())
                .cloned();
            self.select_tab(next);
        }
        if self.dock_selected.as_ref() == Some(id) {
            self.dock_selected = self.open_tabs.get(index).or_else(|| self.open_tabs.last()).cloned();
        }
        self.tab_offset = self.tab_offset.min(self.open_tabs.len().saturating_sub(1));
        self.reveal_tab = true;
    }

    pub(super) fn mark_tab_unread(&mut self, id: &p::SessionId, window_focused: bool) {
        if self.background || self.selected.as_ref() != Some(id) || !self.visible || self.area.is_none()
            || !window_focused || !self.follow {
            self.unread_tabs.insert(id.clone());
        }
    }

    pub(crate) fn acknowledge_selected_activity(&mut self) {
        if self.follow && let Some(id) = &self.connection.selected {
            self.connection.unread_tabs.remove(id);
        }
    }
}

impl App {
    /// Clear unread markers before painting editor tab bars. Only a visible
    /// transcript at its live end counts as seen; background repaints do not.
    pub(crate) fn acknowledge_visible_chat_activity(&mut self) {
        if !self.terminal_has_focus || self.change_dir_active {
            return;
        }
        let seen: Vec<_> = self.chat.layout.surfaces.iter()
            .filter(|surface| match surface.key {
                None => self.chat.layout.dock_visible && !self.zen_mode
                    && surface.agent == self.chat.layout.dock_agent
                    && self.chat.agent_state(surface.agent)
                        .is_some_and(|state| state.dock_selected == surface.session),
                Some(key) => {
                    let kind = crate::app::TabKind::Chat(key);
                    self.tabs.get(self.active_tab).is_some_and(|tab| tab.kind == kind)
                        || self.groups.iter().enumerate().any(|(i, group)| {
                            !self.zen_mode && i != self.active_group
                                && group.tabs.get(group.active_tab).is_some_and(|tab| tab.kind == kind)
                        })
                }
            })
            .filter(|surface| surface.transcript.is_some_and(|area| area.width > 0 && area.height > 0))
            .filter_map(|surface| Some((surface.agent, surface.session.clone()?)))
            .collect();
        for (agent, id) in seen {
            let state = if agent == self.chat.active_agent {
                &mut self.chat.connection
            } else if let Some(state) = self.chat.connections.get_mut(&agent) {
                state
            } else { continue; };
            let following = if state.selected.as_ref() == Some(&id) {
                state.follow
            } else {
                state.views.get(&Some(id.clone())).is_none_or(|view| !view.scrolled_back)
            };
            if following {
                state.unread_tabs.remove(&id);
            }
        }
    }
}
