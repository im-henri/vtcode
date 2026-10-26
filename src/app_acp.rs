//! Workbench adapter for the UI-independent ACP service.
use crate::{
    app::{ActivePane, App, TabKind},
    text_input::{TextInput, TextInputHistory, TextInputId},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::PathBuf,
};
use vtcode::acp::{
    AgentConfig, ChatEntry, Client, Command, Event, FileRequest, Operation, Session, protocol as p,
};

#[path = "app_acp_tabs.rs"]
mod tabs;
use tabs::ChatView;
#[path = "app_acp_pointer.rs"]
mod pointer;
pub(crate) use pointer::{ChatScrollbar, ScrollDrag};
#[path = "app_acp_layout.rs"]
pub(crate) mod layout;
#[path = "app_acp_history.rs"]
mod history;
#[path = "app_acp_history_filter.rs"]
mod history_filter;
#[path = "app_acp_rollback.rs"]
pub(crate) mod rollback;
#[path = "app_acp_queue.rs"]
pub(crate) mod queue;
#[path = "app_acp_agents.rs"]
mod agents;
#[path = "app_acp_agent_close.rs"]
mod agent_close;
#[path = "app_acp_control.rs"]
mod control;
#[path = "app_acp_permissions.rs"]
mod permissions;
pub(crate) type AgentId = u64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Picker {
    Agents,
    Sessions,
    Modes,
    Models,
    Thinking,
    Options,
    Values(String),
    Auth,
    Commands,
    Permissions,
    Attachments,
    Changes,
    ChangedFile(PathBuf),
    Change(u64),
    ChangeChunk(u64, usize),
    Checkpoints,
    Restore(u64),
    // Include the history epoch so a trimmed transcript cannot retarget a menu.
    Message(usize, u64),
    ThinkingInfo,
    Display,
    Tasks,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ChatAction {
    OutputDetail,
    ThinkingDetail,
    SetOutputDetail(OutputDetail),
    SetThinkingDetail(bool),
    TaskDetails(usize),
    Yolo,
    Hide,
    MoveToDock,
    PermissionDetails,
    DismissPicker,
    Configure,
    Picker(Picker),
    Pick(usize),
    Connect(usize),
    SwitchAgent(AgentId),
    CloseAgent(AgentId),
    AgentPage(bool),
    Disconnect,
    Reconnect,
    New,
    Send,
    Stop,
    EditQueued(u64),
    RemoveQueued(u64),
    SendQueued(u64),
    PauseQueue,
    QueuePage(bool),
    Follow,
    Attach,
    RemoveAttachment(usize),
    AttachmentPage(bool),
    ChooseFile(usize),
    LoadById,
    Switch(p::SessionId),
    CloseTab(p::SessionId),
    DeleteSession(p::SessionId),
    TabPage(bool),
    Mode(p::SessionModeId),
    Model(p::SessionConfigId, p::SessionConfigOptionValue),
    Config(p::SessionConfigId, p::SessionConfigOptionValue),
    Auth(p::AuthMethodId),
    AgentCommand(String),
    MoreSessions,
    Permission(p::RequestId, u64, Option<p::PermissionOptionId>),
    ToggleEntry(usize),
    Copy(usize),
    Review(std::sync::Arc<p::Diff>),
    OpenFile(PathBuf, Option<u32>),
    Terminal(String),
    StopTerminal(String),
    RevealTerminal(String),
    FocusTerminal(String),
    ScrollTerminal(String, isize),
    PanTerminal(String, isize),
    TerminalTop(String),
    FollowTerminal(String),
    BackToChat,
    CloseSession,
    Resolve(vtcode::acp::rollback::Selection, bool),
    ReviewChange(u64),
    RecoverRollback,
    UndoRollback,
    RetryCheckpoint,
    RetryCheckpointStorage,
}

#[derive(Clone)]
pub(crate) struct Choice {
    pub label: String,
    pub detail: String,
    pub action: ChatAction,
}
#[derive(Clone)]
pub(crate) struct ChatHit {
    pub area: Rect,
    pub action: ChatAction,
}
pub(crate) struct Permission {
    pub id: p::RequestId,
    pub ticket: u64,
    pub request: p::RequestPermissionRequest,
    /// The request title can be a pattern or path, replacing the tool's name.
    pub tool_title: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum OutputDetail {
    Minimal,
    #[default]
    Compact,
    Full,
}
impl OutputDetail {
    pub fn label(self) -> &'static str {
        match self { Self::Minimal => "Minimal", Self::Compact => "Compact", Self::Full => "Full" }
    }
    pub fn parse(value: &str) -> Self {
        match value { "minimal" => Self::Minimal, "full" => Self::Full, _ => Self::Compact }
    }
}

/// Shared workbench presentation and checkpoint storage, with isolated agent connections.
pub(crate) struct ChatState {
    pub visible: bool,
    pub width: u16,
    pub area: Option<Rect>,
    pub transcript_area: Option<Rect>,
    pub context_usage_area: Option<Rect>,
    pub model_selector_area: Option<Rect>,
    pub hits: Vec<ChatHit>,
    pub resizing: bool,
    pub scrollbars: Vec<ChatScrollbar>,
    pub scroll_drag: Option<ScrollDrag>,
    pub output_detail: OutputDetail,
    pub thinking_expanded: bool,
    pub activity: crate::ui_chat::ActivityAnimation,
    pub layout: layout::ChatLayout,
    pub rollback: rollback::ReviewState,
    pub active_agent: AgentId,
    pub confirm_close_agent: Option<AgentId>,
    pub agent_order: Vec<AgentId>,
    next_poll_agent: usize,
    pub agent_tab_offset: usize,
    pub agent_tab_area: Option<Rect>,
    pub agent_tab_width: u16,
    pub reveal_agent: bool,
    pub background: bool,
    pub(crate) connection: AgentState,
    connections: HashMap<AgentId, AgentState>,
    next_agent: AgentId,
}

/// Everything owned by one ACP transport, including its conversation views.
pub(crate) struct AgentState {
    pub focus: Option<ChatAction>,
    pub collapsed: HashSet<usize>,
    pub client: Option<Client>,
    pub agent: Option<AgentConfig>,
    pub initialized: Option<p::InitializeResponse>,
    pub workspace: PathBuf,
    pub status: String,
    pub error: Option<String>,
    pub control_busy: bool,
    pub(super) control_request: Option<(Operation, Option<p::SessionId>)>,
    /// Replay may arrive before loading establishes a continuable session.
    pub loading_session: Option<p::SessionId>,
    pub closing: bool,
    pub model_change: Option<p::SessionId>,
    pub sessions: HashMap<p::SessionId, Session>,
    pub session_order: Vec<p::SessionId>,
    pub selected: Option<p::SessionId>,
    pub open_tabs: Vec<p::SessionId>,
    pub unread_tabs: HashSet<p::SessionId>,
    pub tab_area: Option<Rect>,
    pub tab_offset: usize,
    pub tab_width: u16,
    pub reveal_tab: bool,
    views: HashMap<Option<p::SessionId>, ChatView>,
    pub remote_sessions: Vec<p::SessionInfo>,
    pub deleting_sessions: HashSet<p::SessionId>,
    removed_sessions: HashSet<p::SessionId>,
    deferred_discards: HashSet<p::SessionId>,
    history_filter: history_filter::HistoryFilter,
    pub next_cursor: Option<String>,
    pub permissions: VecDeque<Permission>,
    yolo_sessions: HashSet<p::SessionId>,
    yolo_approvals: HashMap<p::RequestId, permissions::YoloApproval>,
    pub picker: Option<Picker>,
    pub choices: Vec<Choice>,
    pub picker_selected: usize,
    pub picker_scroll: usize,
    pub input: String,
    pub cursor: usize,
    pub anchor: Option<usize>,
    pub input_layout: crate::commit_input::CommitInputState,
    pub input_history: TextInputHistory,
    pub input_goal: Option<usize>,
    pub files: crate::app_chat_files::ChatFiles,
    pub terminal_input: Option<String>,
    pub terminal_views: crate::app_chat_terminals::TerminalViews,
    pub terminal_areas: Vec<(String, Rect)>,
    pub reveal_terminal: Option<String>,
    pub reveal_entry: Option<usize>,
    pub expansion_scroll: Option<(usize, usize)>,
    pub loading_id: bool,
    saved_draft: Option<(String, usize, Option<usize>, TextInputHistory)>,
    pub scroll: usize,
    pub max_scroll: usize,
    pub follow: bool,
    pub expanded: HashSet<usize>,
    pub cache: crate::ui_chat::ChatCache,
    pub diagnostics: VecDeque<String>,
    pub queues: HashMap<p::SessionId, queue::PromptQueue>,
    next_queue_id: u64,
    pub dock_selected: Option<p::SessionId>,
    pub opening_group: Option<usize>,
}

impl Default for ChatState {
    fn default() -> Self {
        Self {
            visible: false,
            width: 58,
            area: None,
            transcript_area: None,
            context_usage_area: None,
            model_selector_area: None,
            hits: Vec::new(),
            resizing: false,
            scrollbars: Vec::new(),
            scroll_drag: None,
            output_detail: OutputDetail::default(),
            thinking_expanded: false,
            activity: Default::default(),
            layout: Default::default(),
            rollback: Default::default(),
            active_agent: 0,
            confirm_close_agent: None,
            agent_order: Vec::new(),
            next_poll_agent: 0,
            agent_tab_offset: 0,
            agent_tab_area: None,
            agent_tab_width: 0,
            reveal_agent: true,
            background: false,
            connection: AgentState::default(),
            connections: HashMap::new(),
            next_agent: 1,
        }
    }
}

impl Default for AgentState {
    fn default() -> Self {
        Self {
            focus: None,
            collapsed: HashSet::new(),
            client: None,
            agent: None,
            initialized: None,
            workspace: PathBuf::new(),
            status: "Choose an agent to begin".into(),
            error: None,
            control_busy: false,
            control_request: None,
            loading_session: None,
            closing: false,
            model_change: None,
            sessions: HashMap::new(),
            session_order: Vec::new(),
            selected: None,
            open_tabs: Vec::new(),
            unread_tabs: HashSet::new(),
            tab_area: None,
            tab_offset: 0,
            tab_width: 0,
            reveal_tab: true,
            views: HashMap::new(),
            remote_sessions: Vec::new(),
            deleting_sessions: HashSet::new(),
            removed_sessions: HashSet::new(),
            deferred_discards: HashSet::new(),
            history_filter: Default::default(),
            next_cursor: None,
            permissions: VecDeque::new(),
            yolo_sessions: HashSet::new(),
            yolo_approvals: HashMap::new(),
            picker: None,
            choices: Vec::new(),
            picker_selected: 0,
            picker_scroll: 0,
            input: String::new(),
            cursor: 0,
            anchor: None,
            input_layout: Default::default(),
            input_history: Default::default(),
            input_goal: None,
            files: Default::default(),
            terminal_input: None,
            terminal_views: Default::default(),
            terminal_areas: Vec::new(),
            reveal_terminal: None,
            reveal_entry: None,
            expansion_scroll: None,
            loading_id: false,
            saved_draft: None,
            scroll: 0,
            max_scroll: 0,
            follow: true,
            expanded: HashSet::new(),
            cache: Default::default(),
            diagnostics: VecDeque::new(),
            queues: HashMap::new(),
            next_queue_id: 0,
            dock_selected: None,
            opening_group: None,
        }
    }
}

// Conversation operations address the currently loaded agent. Shared layout
// and rollback remain in place when switch_connection swaps AgentState.
impl std::ops::Deref for ChatState {
    type Target = AgentState;
    fn deref(&self) -> &AgentState { &self.connection }
}
impl std::ops::DerefMut for ChatState {
    fn deref_mut(&mut self) -> &mut AgentState { &mut self.connection }
}

impl ChatState {
    pub(crate) fn entry_expanded(&self, index: usize) -> bool {
        self.expanded.contains(&index) || (!self.collapsed.contains(&index)
            && self.session().is_some_and(|s| match s.entries.get(index) {
                Some(ChatEntry::Thought(_)) => self.thinking_expanded,
                Some(ChatEntry::Tool(_) | ChatEntry::Terminal(_)) => self.output_detail == OutputDetail::Full,
                _ => false,
            }))
    }

    /// Reset only the affected card type, including conversations in other panes.
    fn reset_expansion(&mut self, thoughts: bool) {
        let clear = |session: Option<&Session>, expanded: &mut HashSet<usize>, collapsed: &mut HashSet<usize>| {
            let keep = |index: &usize| session.is_some_and(|s| s.entries.get(*index)
                .is_some_and(|entry| matches!(entry, ChatEntry::Thought(_)) != thoughts));
            expanded.retain(keep);
            collapsed.retain(keep);
        };
        for state in std::iter::once(&mut self.connection).chain(self.connections.values_mut()) {
            clear(state.selected.as_ref().and_then(|id| state.sessions.get(id)), &mut state.expanded, &mut state.collapsed);
            for (id, view) in &mut state.views {
                clear(id.as_ref().and_then(|id| state.sessions.get(id)), &mut view.expanded, &mut view.collapsed);
            }
        }
    }
    pub(crate) fn loading_conversation(&self) -> bool {
        self.layout.rendering == self.layout.active
            && self.control_busy
            && (self.selected.is_none()
                || match &self.control_request {
                    Some((operation, _)) => matches!(
                        operation,
                        Operation::NewSession | Operation::LoadSession | Operation::ResumeSession
                    ),
                    None => {
                        self.status == "Creating conversation…"
                            || self.status == "Loading conversation…"
                    }
                })
    }
    pub(crate) fn with_composer_input<R>(&mut self, apply: impl FnOnce(&mut String, &mut usize, &mut Option<usize>, &mut TextInputHistory) -> R) -> R {
        let state = &mut self.connection;
        if let Some((text, cursor, anchor, history)) = &mut state.saved_draft {
            apply(text, cursor, anchor, history)
        } else {
            apply(&mut state.input, &mut state.cursor, &mut state.anchor, &mut state.input_history)
        }
    }

    fn auxiliary_input(&mut self) {
        self.files.dismiss();
        if self.saved_draft.is_none() {
            self.saved_draft = Some((
                std::mem::take(&mut self.input),
                self.cursor,
                self.anchor,
                std::mem::take(&mut self.input_history),
            ));
        }
        self.input.clear();
        self.cursor = 0;
        self.anchor = None;
        self.input_goal = None;
        self.input_history = Default::default();
    }
    fn restore_draft(&mut self) {
        self.files.dismiss();
        self.terminal_input = None;
        self.loading_id = false;
        if let Some((text, cursor, anchor, history)) = self.saved_draft.take() {
            self.input = text;
            self.cursor = cursor;
            self.anchor = anchor;
            self.input_history = history;
            self.input_goal = None;
        }
    }
    pub fn session(&self) -> Option<&Session> {
        self.selected.as_ref().and_then(|id| self.sessions.get(id))
    }
    pub fn session_mut(&mut self) -> Option<&mut Session> {
        self.connection.selected
            .as_ref()
            .and_then(|id| self.connection.sessions.get_mut(id))
    }
    pub fn busy(&self) -> bool {
        self.session().is_some_and(|s| s.busy) || self.selected.as_ref().is_some_and(|id| self.files.preparing.contains_key(id))
    }
    pub fn connected(&self) -> bool {
        self.client.is_some() && self.initialized.is_some()
    }
    pub fn permission(&self) -> Option<&Permission> {
        self.permissions
            .iter()
            .find(|p| Some(&p.request.session_id) == self.selected.as_ref())
    }
    pub fn mode_name(&self) -> Option<&str> {
        if let Some(option) = self.primary_config(p::SessionConfigOptionCategory::Mode) {
            return Some(config_value_name(option));
        }
        let modes = self.session()?.modes.as_ref()?;
        modes
            .available_modes
            .iter()
            .find(|m| m.id == modes.current_mode_id)
            .map(|m| m.name.as_str())
    }

    pub fn primary_config(
        &self,
        category: p::SessionConfigOptionCategory,
    ) -> Option<&p::SessionConfigOption> {
        self.session()?.config_options.iter().find(|option| {
            config_category(option) == Some(category.clone())
                && matches!(option.kind, p::SessionConfigKind::Select(_))
        })
    }

    pub fn thinking_option(&self) -> Option<&p::SessionConfigOption> {
        self.session()?.config_options.iter().find(|option| {
            config_category(option) == Some(p::SessionConfigOptionCategory::ThoughtLevel)
                && config_supported(option)
        })
    }

    pub fn thinking_status(&self) -> (String, Option<String>) {
        if let Some(option) = self.thinking_option() {
            let value = config_value_name(option).to_owned();
            let reason = (!config_changeable(option)).then(|| thinking_readonly_reason(&value));
            (value, reason)
        } else {
            (
                "?".into(),
                Some("The agent has not reported its current thinking effort. It cannot be changed here.".into()),
            )
        }
    }

    pub fn thinking_loading(&self) -> bool {
        self.model_change.is_some() && self.model_change == self.selected
    }

    #[cfg(test)]
    pub fn model_name(&self) -> Option<&str> {
        self.primary_config(p::SessionConfigOptionCategory::Model)
            .map(config_value_name)
    }
}

/// ACP categories describe placement without depending on an agent vendor.
/// Older agents sometimes omit the category on the standard mode/model IDs.
pub(crate) fn config_category(
    option: &p::SessionConfigOption,
) -> Option<p::SessionConfigOptionCategory> {
    option
        .category
        .clone()
        .or_else(|| match option.id.to_string().as_str() {
            "mode" => Some(p::SessionConfigOptionCategory::Mode),
            "model" => Some(p::SessionConfigOptionCategory::Model),
            "thought_level" | "reasoning_effort" | "thinking_effort" | "reasoning-effort" => {
                Some(p::SessionConfigOptionCategory::ThoughtLevel)
            }
            _ => None,
        })
}

pub(crate) fn config_changeable(option: &p::SessionConfigOption) -> bool {
    match &option.kind {
        p::SessionConfigKind::Select(select) => match &select.options {
            p::SessionConfigSelectOptions::Ungrouped(values) => {
                values.iter().any(|v| v.value != select.current_value)
            }
            p::SessionConfigSelectOptions::Grouped(groups) => groups
                .iter()
                .flat_map(|g| &g.options)
                .any(|v| v.value != select.current_value),
            _ => false,
        },
        p::SessionConfigKind::Boolean(_) => true,
        _ => false,
    }
}

fn thinking_readonly_reason(value: &str) -> String {
    format!("The agent reports {value} and offers no other effort settings. It cannot be changed here.")
}

pub(crate) fn config_supported(option: &p::SessionConfigOption) -> bool {
    matches!(
        option.kind,
        p::SessionConfigKind::Select(_) | p::SessionConfigKind::Boolean(_)
    )
}

pub(crate) fn config_value_name(option: &p::SessionConfigOption) -> &str {
    match &option.kind {
        p::SessionConfigKind::Select(select) => {
            let value = match &select.options {
                p::SessionConfigSelectOptions::Ungrouped(values) => {
                    values.iter().find(|v| v.value == select.current_value)
                }
                p::SessionConfigSelectOptions::Grouped(groups) => groups
                    .iter()
                    .flat_map(|g| &g.options)
                    .find(|v| v.value == select.current_value),
                _ => None,
            };
            value
                .map(|v| v.name.as_str())
                .unwrap_or(&select.current_value.0)
        }
        p::SessionConfigKind::Boolean(value) => {
            if value.current_value {
                "On"
            } else {
                "Off"
            }
        }
        _ => &option.name,
    }
}

fn config_choices(option: &p::SessionConfigOption) -> Vec<Choice> {
    if config_category(option) == Some(p::SessionConfigOptionCategory::ThoughtLevel)
        && !config_changeable(option)
    {
        let value = config_value_name(option);
        return vec![Choice {
            label: format!("Thinking: {value}"),
            detail: thinking_readonly_reason(value),
            action: ChatAction::DismissPicker,
        }];
    }
    let mut choices = Vec::new();
    match &option.kind {
        p::SessionConfigKind::Select(select) => {
            let values: Vec<_> = match &select.options {
                p::SessionConfigSelectOptions::Ungrouped(options) => {
                    options.iter().map(|v| (v, None)).collect()
                }
                p::SessionConfigSelectOptions::Grouped(groups) => groups
                    .iter()
                    .flat_map(|g| g.options.iter().map(move |v| (v, Some(g.name.as_str()))))
                    .collect(),
                _ => Vec::new(),
            };
            for (value, group) in values {
                let detail = [
                    group,
                    value
                        .description
                        .as_deref()
                        .or(option.description.as_deref()),
                ]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
                choices.push(Choice {
                    label: format!(
                        "{}{}",
                        if value.value == select.current_value {
                            "✓ "
                        } else {
                            ""
                        },
                        value.name
                    ),
                    detail,
                    action: ChatAction::Config(
                        option.id.clone(),
                        p::SessionConfigOptionValue::ValueId {
                            value: value.value.clone(),
                        },
                    ),
                });
            }
        }
        p::SessionConfigKind::Boolean(value) => {
            for enabled in [true, false] {
                choices.push(Choice {
                    label: format!(
                        "{}{}",
                        if value.current_value == enabled {
                            "✓ "
                        } else {
                            ""
                        },
                        if enabled { "On" } else { "Off" }
                    ),
                    detail: option.description.clone().unwrap_or_default(),
                    action: ChatAction::Config(
                        option.id.clone(),
                        p::SessionConfigOptionValue::Boolean { value: enabled },
                    ),
                });
            }
        }
        _ => {}
    }
    choices
}

impl App {
    pub(crate) fn focus_chat(&mut self) {
        if let Some(key) = self.chat.layout.active {
            if let Some(view) = self.chat.layout.editors.get(&key).cloned() {
                self.chat.switch_connection(view.agent);
                self.activate_editor_chat(&view.session);
            }
        } else { self.chat.layout.dock_visible = true; }
        if self.zen_mode {
            self.execute_palette_command("toggle_zen");
        }
        self.auto_save_on_focus_change();
        self.chat.visible = true;
        self.chat.acknowledge_selected_activity();
        self.active_pane = ActivePane::Chat;
        self.terminal_panel_focused = false;
        self.search_input_focused = false;
        self.cursor_visible = true;
    }

    pub(crate) fn execute_acp_command(&mut self, id: &str) -> bool {
        let action = match id {
            "acp_chat" => {
                if self.chat.visible && self.chat.layout.dock_visible
                    && self.active_pane == ActivePane::Chat && self.chat.layout.active.is_none() {
                    self.chat_action(ChatAction::Hide);
                } else {
                    // The workbench button targets the dock, even when a
                    // conversation in an editor tab currently has focus.
                    self.select_chat_agent(self.chat.layout.dock_agent);
                }
                return true;
            }
            "acp_connect" => ChatAction::Picker(Picker::Agents),
            "acp_new" => ChatAction::New,
            "acp_sessions" => ChatAction::Picker(Picker::Sessions),
            "acp_modes" => ChatAction::Picker(Picker::Modes),
            "acp_options" => ChatAction::Picker(Picker::Options),
            "acp_display" => ChatAction::Picker(Picker::Display),
            "acp_output_detail" => ChatAction::OutputDetail,
            "acp_thinking_detail" => ChatAction::ThinkingDetail,
            "acp_auth" => ChatAction::Picker(Picker::Auth),
            "acp_stop" => ChatAction::Stop,
            "acp_disconnect" => ChatAction::Disconnect,
            "acp_configure" => ChatAction::Configure,
            "acp_changes" => ChatAction::Picker(Picker::Changes),
            "acp_checkpoints" => ChatAction::Picker(Picker::Checkpoints),
            _ => return false,
        };
        self.focus_chat();
        self.chat_action(action);
        true
    }

    fn send_acp(&mut self, command: Command) -> bool {
        let control_request = control::request(&command);
        let result = self
            .chat
            .client
            .as_ref()
            .ok_or_else(|| "Connect an agent first".to_string())
            .and_then(|c| c.send(command));
        if let Err(error) = result {
            self.chat.error = Some(error);
            false
        } else {
            if let Some(request) = control_request {
                self.chat.control_request = Some(request);
            }
            true
        }
    }

    fn start_acp_connection(&mut self, config: AgentConfig, reconnect: bool) {
        if self.chat.closing { return; }
        if self.chat.rollback.changing_workspace(&self.explorer.root) {
            self.chat.error = Some("Wait for the previous workspace's checkpoint capture before connecting.".into());
            return;
        }
        if reconnect && self.chat.queues.values().any(|queue| !queue.is_empty()) {
            self.chat.error = Some("Edit or remove queued messages before reconnecting this agent.".into());
            return;
        }
        if reconnect && self.chat.rollback.agent_busy(self.chat.active_agent) {
            self.chat.error = Some("Wait for checkpoint capture before reconnecting.".into());
            return;
        }
        self.chat.restore_draft();
        self.chat.client = None;
        self.chat.initialized = None;
        self.chat.model_change = None;
        self.chat.loading_session = None;
        self.chat.permissions.clear();
        self.chat.reset_yolo();
        self.chat.sessions.clear();
        self.chat.queues.clear();
        let agent = self.chat.active_agent;
        self.chat.layout.editors.retain(|_, view| view.agent != agent);
        self.chat.layout.active = None;
        self.chat.layout.dock_agent = agent;
        self.chat.layout.dock_visible = true;
        self.chat.dock_selected = None;
        self.chat.opening_group = None;
        self.chat.session_order.clear();
        self.chat.open_tabs.clear();
        self.chat.unread_tabs.clear();
        self.chat.views.clear();
        self.chat.tab_offset = 0;
        self.chat.reveal_tab = true;
        self.chat.selected = None;
        self.chat.remote_sessions.clear();
        self.chat.deleting_sessions.clear();
        self.chat.removed_sessions.clear();
        self.chat.deferred_discards.clear();
        self.chat.next_cursor = None;
        self.chat.expanded.clear();
        self.chat.collapsed.clear();
        self.chat.cache = Default::default();
        self.chat.terminal_input = None;
        self.chat.terminal_views.clear();
        self.chat.terminal_areas.clear();
        self.chat.reveal_terminal = None;
        self.chat.loading_id = false;
        self.chat.diagnostics.clear();
        self.chat.error = None;
        self.chat.picker = None;
        self.chat.control_busy = true;
        self.chat.control_request = Some((Operation::Initialize, None));
        self.chat.status = format!("Connecting to {}…", config.name);
        self.chat.workspace = self.explorer.root.clone();
        match history_filter::HistoryFilter::load(
            crate::portable::workspace_dir(&self.explorer.root),
            &config,
        ) {
            Ok(filter) => self.chat.history_filter = filter,
            Err(error) => {
                self.chat.error = Some(error);
                self.chat.control_busy = false;
                self.chat.control_request = None;
                return;
            }
        }
        let wake = self.background.wake_handle();
        let terminal_storage =
            crate::portable::workspace_dir(&self.explorer.root).join("agent-terminals");
        let result = if let Some(workspace) = &self.remote_workspace {
            crate::remote::agent::connect(workspace.clone(), config.clone(), wake).map_err(|error| error.to_string())
        } else { Client::connect_with_terminal_storage(config.clone(), &self.explorer.root, &terminal_storage, move || wake.notify()) };
        match result {
            Ok(client) => {
                self.initialize_chat_rollback(&config);
                self.chat.agent = Some(config);
                self.chat.client = Some(client);
            }
            Err(error) => {
                self.chat.error = Some(error);
                self.chat.control_busy = false;
                self.chat.control_request = None;
                self.chat.status = "Connection failed".into();
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn poll_acp(&mut self) -> bool {
        self.poll_acp_until(std::time::Instant::now() + crate::ui_work::TURN_BUDGET)
    }

    pub(crate) fn poll_acp_until(&mut self, deadline: std::time::Instant) -> bool {
        let active = self.chat.active_agent;
        let background = self.chat.background;
        let mut ids = self.chat.agent_order.clone();
        if !ids.contains(&active) { ids.push(active); }
        let mut changed = self.poll_chat_rollback();
        let mut remaining = 64;
        for visited in 0..ids.len() {
            let index = self.chat.next_poll_agent % ids.len();
            self.chat.next_poll_agent = (index + 1) % ids.len();
            let id = ids[index];
            let connection_changed = self.with_agent_context(id, background || id != active,
                |app| app.poll_acp_connection(deadline, &mut remaining)).unwrap_or(false);
            changed |= connection_changed;
            if visited + 1 < ids.len() && (remaining == 0 || ((remaining < 64 || connection_changed) && std::time::Instant::now() >= deadline)) {
                self.background.wake_handle().notify();
                break;
            }
        }
        changed |= self.poll_chat_rollback();
        changed | self.finish_closing_chat_agents()
    }

    fn poll_acp_connection(&mut self, deadline: std::time::Instant, remaining: &mut usize) -> bool {
        let mut changed = self.poll_chat_terminals();
        if self
            .chat
            .files
            .completion
            .as_ref()
            .is_some_and(|c| c.root != self.explorer.root)
        {
            self.chat.files.dismiss();
            changed = true;
        }
        if self.chat.client.is_some() && self.chat.workspace != self.explorer.root {
            self.chat_action(ChatAction::Disconnect);
            self.chat.status = "Workspace changed. Connect an agent for this workspace.".into();
            return true;
        }
        while *remaining > 0 {
            let Some(event) = self.chat.client.as_ref().and_then(Client::try_recv) else {
                return self.dispatch_chat_queue() || changed;
            };
            changed = true;
            *remaining -= 1;
            self.apply_acp_event(event);
            if std::time::Instant::now() >= deadline { break; }
        }
        // There may be more events queued; never lose a wake after a bounded drain.
        self.background.wake_handle().notify();
        changed
    }

    pub(crate) fn apply_acp_event(&mut self, event: Event) {
        match event {
            Event::Initialized(info) => {
                if !self.chat.finish_control(&Operation::Initialize, None) {
                    return;
                }
                self.chat.initialized = Some(*info);
                self.chat.status = "Creating conversation…".into();
                self.chat.control_busy = self.send_acp(Command::NewSession);
            }
            Event::Authenticated => {
                if !self.chat.finish_control(&Operation::Authenticate, None) {
                    return;
                }
                self.chat.error = self.chat.rollback.initialization_error().map(str::to_owned);
                self.chat.status = "Authenticated".into();
                if self.chat.selected.is_none() {
                    self.chat.control_busy = self.send_acp(Command::NewSession);
                } else {
                    self.chat.control_busy = false;
                }
            }
            Event::SessionOpened {
                session_id,
                modes,
                config_options,
                loaded,
            } => {
                let operation = if loaded {
                    if self
                        .chat
                        .control_request
                        .as_ref()
                        .is_some_and(|(op, _)| *op == Operation::ResumeSession)
                    {
                        Operation::ResumeSession
                    } else {
                        Operation::LoadSession
                    }
                } else {
                    Operation::NewSession
                };
                if !self
                    .chat
                    .finish_control(&operation, Some(&session_id).filter(|_| loaded))
                {
                    // An obsolete reply must not select a different chat or
                    // consume the editor destination of the current request.
                    return;
                }
                if self.chat.loading_session.as_ref() == Some(&session_id) {
                    self.chat.loading_session = None;
                }
                let state = &mut self.chat.connection;
                let session = state
                    .sessions
                    .entry(session_id.clone())
                    .or_insert_with(|| Session::new(session_id.clone()));
                session.modes = modes;
                session.config_options = config_options;
                // A saved chat may have partial, empty or delayed replay. Only
                // locally created unused sessions are disposable empty chats.
                if loaded {
                    session.has_conversation = true;
                }
                if session.title == "New conversation"
                    && let Some(title) = state
                        .remote_sessions
                        .iter()
                        .find(|remote| remote.session_id == session_id)
                        .and_then(|remote| remote.title.as_ref())
                {
                    session.title = title.clone();
                }
                if !self.chat.session_order.contains(&session_id) {
                    self.chat.session_order.push(session_id.clone());
                }
                let empty = self.chat.empty_conversation(&session_id);
                self.chat.open_tab(session_id.clone());
                if let Some(group) = self.chat.opening_group.take() {
                    self.place_chat_in_editor(session_id.clone(), group, None);
                }
                self.chat.error = self.chat.rollback.initialization_error().map(str::to_owned);
                self.set_chat_history_hidden(&session_id, empty);
                self.chat.status = "Ready".into();
                self.refresh_chat_rollback();
            }
            Event::SessionsListed(result) => {
                for session in result.sessions {
                    if self.chat.removed_sessions.contains(&session.session_id)
                        || self.chat.history_filter.contains(&session.session_id)
                        || self.chat.empty_conversation(&session.session_id)
                    {
                        continue;
                    }
                    if !self
                        .chat
                        .remote_sessions
                        .iter()
                        .any(|s| s.session_id == session.session_id)
                    {
                        self.chat.remote_sessions.push(session);
                    }
                }
                self.chat.next_cursor = result.next_cursor;
                self.chat.finish_control(&Operation::ListSessions, None);
                if self.chat.picker == Some(Picker::Sessions) {
                    self.build_chat_choices(Picker::Sessions);
                }
            }
            Event::SessionClosed(id) => {
                self.chat.close_tab(&id);
                self.chat.sessions.remove(&id);
                self.chat.forget_yolo(&id);
                self.chat.session_order.retain(|s| s != &id);
                self.chat.permissions.retain(|p| p.request.session_id != id);
                self.chat.views.remove(&Some(id.clone()));
                self.chat.unread_tabs.remove(&id);
                self.chat.finish_control(&Operation::CloseSession, Some(&id));
            }
            Event::SessionDeleted(id) => {
                let history_open = self.chat.picker == Some(Picker::Sessions);
                self.chat.deleting_sessions.remove(&id);
                self.chat.removed_sessions.insert(id.clone());
                self.set_chat_history_hidden(&id, true);
                self.chat.forget_conversation(&id);
                if !self.chat.control_busy {
                    self.chat.status = "Conversation removed".into();
                }
                if history_open {
                    self.build_chat_choices(Picker::Sessions);
                }
            }
            Event::Update(notification) => {
                let id = notification.session_id.clone();
                if self.chat.removed_sessions.contains(&id) {
                    return;
                }
                self.chat.mark_tab_unread(&id, self.terminal_has_focus);
                let session = self
                    .chat
                    .sessions
                    .entry(notification.session_id.clone())
                    .or_insert_with(|| Session::new(notification.session_id));
                let epoch = session.history_epoch;
                session.apply(notification.update);
                if session.history_epoch != epoch {
                    if self.chat.selected.as_ref() == Some(&id) {
                        self.chat.expanded.clear();
                        self.chat.collapsed.clear();
                        self.chat.focus = None;
                    } else if let Some(view) = self.chat.views.get_mut(&Some(id.clone())) {
                        view.expanded.clear();
                        view.collapsed.clear();
                    }
                }
                if self
                    .chat
                    .sessions
                    .get(&id)
                    .is_some_and(|s| s.has_conversation)
                {
                    self.set_chat_history_hidden(&id, false);
                }
            }
            Event::ModeChanged {
                session_id,
                mode_id,
            } => {
                if let Some(session) = self.chat.sessions.get_mut(&session_id)
                    && let Some(modes) = &mut session.modes
                {
                    modes.current_mode_id = mode_id;
                }
                if self
                    .chat
                    .finish_control(&Operation::SetMode, Some(&session_id))
                {
                    self.chat.status = "Ready".into();
                }
            }
            Event::ConfigChanged {
                session_id,
                options,
            } => {
                if let Some(session) = self.chat.sessions.get_mut(&session_id) {
                    session.config_options = options;
                }
                if self
                    .chat
                    .finish_control(&Operation::SetConfig, Some(&session_id))
                {
                    self.chat.status = "Ready".into();
                }
                if self.chat.model_change.as_ref() == Some(&session_id) {
                    self.chat.model_change = None;
                    if self.chat.selected.as_ref() == Some(&session_id)
                        && self.chat.picker == Some(Picker::Thinking)
                    {
                        self.build_chat_choices(Picker::Thinking);
                        self.chat.picker_selected = self
                            .chat
                            .choices
                            .iter()
                            .position(|c| c.label.starts_with("✓ "))
                            .unwrap_or(0);
                        self.chat.picker_scroll = 0;
                    }
                }
            }
            Event::PromptFinished {
                session_id,
                response,
            } => {
                if response.stop_reason != p::StopReason::EndTurn
                    && let Some(queue) = self.chat.queues.get_mut(&session_id)
                    && queue.priority.is_none()
                {
                    queue.paused = true;
                }
                // A selection/focus change may have happened since the last
                // paint. Keep the result unread until its live view is drawn.
                self.chat.unread_tabs.insert(session_id.clone());
                if let Some(session) = self.chat.sessions.get_mut(&session_id) {
                    session.finish(Some(response.stop_reason));
                }
                if !self.chat.control_busy {
                    self.chat.status = "Ready".into();
                }
            }
            Event::Permission { id, ticket, request } => {
                self.receive_chat_permission(id, ticket, request, Self::send_acp);
            }
            Event::PermissionResponded { id, ticket, option } => {
                self.chat_permission_responded(&id, ticket, &option);
            }
            Event::PermissionResolved(id, ticket) => {
                let visible_request = self.chat.permissions.iter().any(|pending| {
                    pending.id == id && pending.ticket == ticket && self.chat.selected.as_ref() == Some(&pending.request.session_id)
                });
                if self.chat.yolo_approvals.get(&id).is_some_and(|approval| approval.ticket == ticket) { self.chat.yolo_approvals.remove(&id); }
                self.chat.permissions.retain(|p| p.id != id || p.ticket != ticket);
                if visible_request { self.chat.focus = None; }
            }
            Event::FileRequest { id, ticket, request } => self.acp_file_request(id, ticket, request),
            Event::FileEdited(edit) => {
                self.chat.mark_tab_unread(&edit.session_id, self.terminal_has_focus);
                self.chat.rollback.note_agent_write(
                    self.chat.active_agent,
                    &edit.session_id,
                    &edit.path,
                );
                let paths: Vec<_> = self.acp_file_editors(&edit.path).into_iter()
                    .filter_map(|tab| tab.editor.file_path.as_ref())
                    .collect::<rustc_hash::FxHashSet<_>>().into_iter().cloned()
                    .collect();
                for path in paths {
                    self.queue_file_reload(path);
                }
                // An ordinary write changes content, not file eligibility or
                // name-sorted directories. Empty-before also covers newly
                // created files, so conservatively refresh structure there.
                self.refresh_explorer_changed_paths([edit.path.as_path()], edit.before.is_empty());
                self.refresh_source_control_changed_paths([edit.path.clone()]);
                if let Some(session) = self.chat.sessions.get_mut(&edit.session_id) {
                    session.push(ChatEntry::Edit(edit));
                }
            }
            Event::Terminal(snapshot) => {
                self.chat.mark_tab_unread(&snapshot.session_id, self.terminal_has_focus);
                if self.chat.selected.as_ref() == Some(&snapshot.session_id)
                    && self.chat.terminal_input.as_ref() == Some(&snapshot.id)
                    && (snapshot.released || snapshot.exit_status.is_some() || snapshot.killed)
                {
                    self.chat.restore_draft();
                }
                if let Some(session) = self.chat.sessions.get_mut(&snapshot.session_id) {
                    session.terminal(snapshot);
                }
            }
            Event::Diagnostic(message) => {
                self.chat.diagnostics.push_back(message);
                while self.chat.diagnostics.len() > 32 {
                    self.chat.diagnostics.pop_front();
                }
            }
            Event::Error {
                operation,
                session_id,
                error,
            } => {
                let control_finished = self.chat.finish_control(&operation, session_id.as_ref());
                if operation == Operation::SetConfig
                    && self.chat.model_change.is_some()
                    && self.chat.model_change == session_id
                {
                    self.chat.model_change = None;
                    if self.chat.selected == session_id
                        && self.chat.picker == Some(Picker::Thinking)
                    {
                        self.chat.picker = None;
                        self.chat.choices.clear();
                    }
                }
                if operation == Operation::Prompt
                    && let Some(queue) = session_id
                        .as_ref()
                        .and_then(|id| self.chat.queues.get_mut(id))
                {
                    queue.paused = true;
                }
                if control_finished
                    && matches!(
                        operation,
                        Operation::NewSession | Operation::LoadSession | Operation::ResumeSession
                    )
                {
                    self.chat.opening_group = None;
                }
                if matches!(operation, Operation::LoadSession | Operation::ResumeSession)
                    && session_id
                        .as_ref()
                        .is_some_and(|id| self.chat.loading_session.as_ref() == Some(id))
                {
                    // Replay can precede a failed load response. It does not
                    // establish a usable agent session: discard the partial
                    // view so History can load it again and show the error in
                    // the current pane. Keep the agent's saved history entry.
                    self.chat.discard_history_load();
                }
                let message = format!("{:?}: {}", operation, error.message);
                if operation == Operation::DeleteSession {
                    if let Some(id) = &session_id {
                        self.chat.deleting_sessions.remove(id);
                    }
                    // History actions can target a background or unloaded chat.
                    self.chat.error = Some(message.clone());
                }
                if let Some(session) = session_id
                    .as_ref()
                    .and_then(|id| self.chat.sessions.get_mut(id))
                {
                    if operation == Operation::Prompt {
                        session.finish(None);
                    }
                    session.push(ChatEntry::Error(message.clone()));
                }
                if let Some(id) = session_id.filter(|id| {
                    Some(id) != self.chat.selected.as_ref() && self.chat.sessions.contains_key(id)
                }) {
                    self.chat.mark_tab_unread(&id, self.terminal_has_focus);
                    self.chat.views.entry(Some(id)).or_default().error = Some(message);
                } else {
                    self.chat.error = Some(message);
                }
                if !self.chat.control_busy {
                    self.chat.status = "Needs attention".into();
                }
            }
            Event::Disconnected(reason) => {
                self.chat.discard_history_load();
                self.chat.model_change = None;
                self.pause_chat_queues();
                self.chat.opening_group = None;
                self.chat.client = None;
                self.chat.initialized = None;
                self.chat.permissions.clear();
                self.chat.reset_yolo();
                self.chat.control_busy = false;
                self.chat.control_request = None;
                self.chat.deleting_sessions.clear();
                self.chat.deferred_discards.clear();
                for session in self.chat.sessions.values_mut() {
                    session.disconnect();
                }
                self.chat.status = "Disconnected".into();
                self.chat.error = Some(reason);
                self.chat.restore_draft();
            }
        }
        self.discard_empty_chat_history();
        let invalid_picker = match &self.chat.picker {
            Some(Picker::Tasks) => !self.chat.session().and_then(|s| s.tasks.as_ref())
                .is_some_and(|tasks| !tasks.entries.is_empty()),
            Some(Picker::Values(id)) => !self.chat.session().is_some_and(|s| {
                s.config_options
                    .iter()
                    .any(|o| o.id.to_string() == *id && config_supported(o))
            }),
            Some(Picker::Message(index, epoch)) => !self.chat.session().is_some_and(|s| {
                s.history_epoch == *epoch && matches!(s.entries.get(*index), Some(ChatEntry::User(_)))
            }),
            _ => false,
        };
        if invalid_picker {
            self.chat.picker = None;
            self.chat.choices.clear();
            self.chat.focus = None;
        }
        if let Some(picker) = self.chat.picker.clone() {
            self.build_chat_choices(picker);
            self.chat.picker_selected = self
                .chat
                .picker_selected
                .min(self.chat.choices.len().saturating_sub(1));
        }
    }

    fn same_acp_file_path(open: &std::path::Path, requested: &std::path::Path) -> bool {
        Self::same_open_path(open, requested)
            || Self::same_open_path(&rollback::resolve_review_path(open), requested)
    }

    pub(super) fn acp_editor_identity(&self, editor: &crate::editor::Editor) -> Option<PathBuf> {
        if let Some(workspace) = &self.remote_workspace
            && let Some(path) = editor.remote_checkpoint.as_ref().and_then(|checkpoint| checkpoint.identity.as_ref())
        { return Some(workspace.view_key(path)); }
        editor.file_path.clone()
    }

    fn acp_editor_matches(&self, editor: &crate::editor::Editor, path: &std::path::Path) -> bool {
        editor.file_path.as_ref().is_some_and(|open| Self::same_acp_file_path(open, path))
            || self.acp_editor_identity(editor).is_some_and(|identity| Self::same_acp_file_path(&identity, path))
    }

    pub(super) fn unresolved_agent_files(&self) -> bool {
        self.remote_workspace.is_some() && self.tabs.iter().chain(self.groups.iter().flat_map(|group| &group.tabs))
            .any(|tab| tab.kind == TabKind::File && tab.editor.bg_loading && tab.editor.file_path.as_ref().is_some_and(|path| crate::remote::workspace::is_view_key(path)))
    }

    pub(super) fn agent_open_paths(&self) -> Vec<PathBuf> {
        self.tabs.iter().chain(self.groups.iter().flat_map(|group| &group.tabs))
            .filter(|tab| tab.kind == TabKind::File).filter_map(|tab| tab.editor.file_path.clone())
            .chain(self.pending_file_saves.keys().cloned()).filter(|path| crate::remote::workspace::is_view_key(path))
            .collect::<std::collections::BTreeSet<_>>().into_iter().collect()
    }

    pub(super) fn record_agent_aliases(&mut self, aliases: &[(PathBuf, Option<PathBuf>)]) {
        let Some(workspace) = &self.remote_workspace else { return; };
        let identities: HashMap<_, _> = aliases.iter().filter_map(|(path, identity)| Some((path, workspace.remote_path(identity.as_ref()?).ok()?))).collect();
        for tab in self.tabs.iter_mut().chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs)) {
            let Some(identity) = tab.editor.file_path.as_ref().and_then(|path| identities.get(path)) else { continue; };
            let Some(checkpoint) = &tab.editor.remote_checkpoint else { continue; };
            if checkpoint.identity.as_ref() == Some(identity) { continue; }
            let mut checkpoint = (**checkpoint).clone();
            checkpoint.identity = Some(identity.clone());
            tab.editor.set_remote_checkpoint(Some(std::sync::Arc::new(checkpoint)));
        }
    }

    fn acp_file_editors(&self, path: &std::path::Path) -> Vec<&crate::app::Tab> {
        self.tabs
            .iter()
            .chain(self.groups.iter().flat_map(|g| &g.tabs))
            .filter(|tab| {
                tab.kind == TabKind::File
                    && self.acp_editor_matches(&tab.editor, path)
            })
            .collect()
    }

    fn acp_write_blocked(&self, path: &std::path::Path, editors: &[&crate::app::Tab]) -> bool {
        self.settings_effective.read_only.unwrap_or(false)
            || self.unresolved_agent_files()
            || self.chat.rollback.file_busy(path)
            || editors
                .iter()
                .any(|tab| tab.editor.modified || tab.editor.read_only || tab.editor.bg_loading)
            || self
                .pending_file_saves
                .keys()
                .any(|p| Self::same_acp_file_path(p, path)
                    || (self.remote_workspace.is_some() && crate::remote::workspace::is_view_key(p)))
    }

    fn acp_file_request(&mut self, id: p::RequestId, ticket: u64, request: FileRequest) {
        if self.remote_workspace.is_none() { self.acp_file_request_ready(id, ticket, request, None); return; }
        let Some(client) = &self.chat.client else { return; };
        let workspace = client.workspace().for_request(id.clone(), ticket);
        let handle = client.handle();
        let paths = self.agent_open_paths();
        let sender = self.background.sender.clone();
        let agent = self.chat.active_agent;
        self.background.spawn("agent-file-aliases", move || {
            let aliases = workspace.resolve_many(&paths).map(|resolved| paths.into_iter().zip(resolved).collect());
            let _ = sender.send(crate::background::BackgroundResult::AgentFileResolved { agent, handle, id, ticket, request, aliases });
        });
    }

    pub(crate) fn apply_agent_file_resolved(&mut self, agent: AgentId, handle: vtcode::acp::ClientHandle, id: p::RequestId, ticket: u64, request: FileRequest, aliases: Result<Vec<(PathBuf, Option<PathBuf>)>, p::Error>) {
        self.with_agent_context(agent, true, |app| {
            if app.chat.client.as_ref().is_none_or(|client| !client.handle().same_connection(&handle)) { return; }
            match aliases {
                Ok(aliases) => app.acp_file_request_ready(id, ticket, request, Some(&aliases)),
                Err(error) => { let _ = handle.send(Command::FileResponse { id, ticket, result: Err(error), edit: None }); }
            }
        });
    }

    fn acp_file_request_ready(&mut self, id: p::RequestId, ticket: u64, request: FileRequest, aliases: Option<&[(PathBuf, Option<PathBuf>)]>) {
        if let Some(aliases) = aliases { self.record_agent_aliases(aliases); }
        let Some(client) = &self.chat.client else {
            return;
        };
        let workspace = client.workspace().for_request(id.clone(), ticket);
        let handle = client.handle();
        let resolved = workspace.resolve(request.path());
        let path = resolved.as_deref().unwrap_or_else(|_| request.path());
        let editors: Vec<_> = if let Some(aliases) = aliases {
            // Resolve once on the node per request, then inspect *current*
            // buffers on the UI. Renames and retargeted symlinks cannot leave a
            // stale load-time identity granting access to the wrong document.
            let aliases: HashMap<_, _> = aliases.iter().map(|(path, identity)| (path, identity)).collect();
            let mut missing = false;
            let editors = self.tabs.iter().chain(self.groups.iter().flat_map(|group| &group.tabs)).filter(|tab| {
                if tab.kind != TabKind::File { return false; }
                let Some(open) = &tab.editor.file_path else { return false; };
                if !crate::remote::workspace::is_view_key(open) { return false; }
                let Some(identity) = aliases.get(open) else { missing = true; return false; };
                if identity.is_none() && (tab.editor.modified || tab.editor.bg_loading || tab.editor.read_only) { missing = true; }
                identity.as_deref().is_some_and(|identity| Self::same_open_path(identity, path))
            }).collect();
            if missing {
                let _ = handle.send(Command::FileResponse { id, ticket, result: Err(p::Error::new(-32000, "Open files changed while validating this request. Retry after file operations finish.")), edit: None });
                return;
            }
            editors
        } else { self.acp_file_editors(path) };
        if self.unresolved_agent_files() || editors.iter().any(|tab| tab.editor.bg_loading) {
            let _ = handle.send(Command::FileResponse { id, ticket, result: Err(p::Error::new(-32000, "Wait for the open files to finish loading, then retry this file request.")), edit: None });
            return;
        }
        if self
            .chat
            .sessions
            .get(request.session_id())
            .is_some_and(|s| s.cancelling)
            || editors
                .iter()
                .any(|tab| tab.editor.rope.len_bytes() > vtcode::acp::MAX_FILE_BYTES)
        {
            let _ = handle.send(Command::FileResponse {
                id,
                ticket,
                result: Err(p::Error::new(
                    -32000,
                    "File request cancelled or buffer exceeds the 8 MiB text limit",
                )),
                edit: None,
            });
            return;
        }
        if matches!(request, FileRequest::Write(_)) && self.acp_write_blocked(path, &editors) {
            let _ = handle.send(Command::FileResponse { id, ticket, result: Err(p::Error::new(-32000, "The file has unsaved edits, is read-only, or has a pending save. Save or revert it before the agent retries.")), edit: None });
            return;
        }
        let buffer = editors
            .iter()
            .max_by_key(|tab| tab.editor.modified)
            .map(|tab| tab.editor.rope.clone());
        let modified_buffers: Vec<_> = editors.iter().filter(|tab| tab.editor.modified).map(|tab| tab.editor.rope.clone()).collect();
        // Disk IO is isolated from the UI; open buffers are snapshotted above.
        let failure_handle = handle.clone();
        let failure_id = id.clone();
        let agent = self.chat.active_agent;
        let wake = self.background.wake_handle();
        let write_guard = self.chat.rollback.file_request_guard(
            agent,
            request.session_id(),
            matches!(request, FileRequest::Write(_)),
        );
        if let Err(error) = std::thread::Builder::new()
            .name("acp-files".into())
            .spawn(move || {
                let _guard = write_guard;
                if matches!(&request, FileRequest::Read(_)) && modified_buffers.first().is_some_and(|first| modified_buffers.iter().skip(1).any(|other| !crate::rope_changes::equal(first, other))) {
                    let _ = handle.send_blocking(Command::FileResponse { id, ticket, result: Err(p::Error::new(-32000, "This file has conflicting unsaved buffers through different aliases. Save or reconcile them before retrying.")), edit: None });
                    return;
                }
                let buffer = buffer.map(|rope| rope.to_string());
                let (result, edit) = match request {
                    FileRequest::Read(request) => (
                        workspace.read(&request, buffer.as_deref()).and_then(|r| {
                            serde_json::to_value(r)
                                .map_err(|e| p::Error::new(-32603, e.to_string()))
                        }),
                        None,
                    ),
                    FileRequest::Write(request) => match workspace.write_cancellable(&request, &|| !handle.file_request_live(&id, ticket)) {
                        Ok(edit) => (Ok(serde_json::json!({})), Some(edit)),
                        Err(error) => (Err(error), None),
                    },
                };
                let _ = handle.send_blocking(Command::FileResponse { id, ticket, result, edit });
                drop(_guard);
                wake.notify();
            })
        {
            let _ = failure_handle.send(Command::FileResponse {
                id: failure_id,
                ticket,
                result: Err(p::Error::new(-32603, error.to_string())),
                edit: None,
            });
        }
    }

    pub(crate) fn chat_action(&mut self, action: ChatAction) {
        self.chat.focus = None;
        match action {
            ChatAction::Resolve(selection, accept) => self.resolve_chat_changes(selection, accept),
            ChatAction::ReviewChange(id) => self.review_chat_change(id),
            ChatAction::RecoverRollback => self.recover_chat_rollback(),
            ChatAction::UndoRollback => self.undo_chat_rollback(),
            ChatAction::RetryCheckpoint => {
                let agent = self.chat.active_agent;
                let session = self.chat.selected.clone();
                self.chat.rollback.retry_checkpoint(agent, session.as_ref());
            },
            ChatAction::RetryCheckpointStorage => self.retry_chat_checkpoint_storage(),
            ChatAction::DismissPicker => self.chat.picker = None,
            ChatAction::Hide => {
                if self.chat.layout.active.is_some() {
                    if let Some(id) = self.chat.selected.clone() { self.chat_action(ChatAction::CloseTab(id)); }
                    return;
                }
                self.chat.files.dismiss();
                // Editor conversations can remain visible and regain focus on
                // redraw. Their visibility must not reopen this dock.
                self.chat.layout.dock_visible = false;
                self.chat.visible = false;
                self.chat.picker = None;
                self.active_pane = ActivePane::Editor;
            }
            ChatAction::Configure => {
                self.chat.picker = None;
                self.open_agent_settings_page();
            }
            ChatAction::Picker(picker) => {
                self.chat.files.dismiss();
                self.focus_chat();
                if self.chat.picker.as_ref() == Some(&picker) {
                    self.chat.picker = None;
                    return;
                }
                self.chat.picker_selected = 0;
                self.chat.picker_scroll = 0;
                self.build_chat_choices(picker.clone());
                self.chat.picker_selected = if picker == Picker::Tasks {
                    self.chat.session().and_then(|s| s.tasks.as_ref()).map_or(0, |tasks| tasks.current())
                } else {
                    self.chat.choices.iter()
                        .position(|choice| choice.label.starts_with("✓ "))
                        .unwrap_or(0)
                };
                if picker == Picker::Sessions
                    && self
                        .chat
                        .initialized
                        .as_ref()
                        .is_some_and(|i| i.agent_capabilities.session_capabilities.list.is_some())
                    && !self.chat.control_busy
                {
                    self.chat.remote_sessions.clear();
                    self.chat.next_cursor = None;
                    self.chat.control_busy = self.send_acp(Command::ListSessions { cursor: None });
                }
            }
            ChatAction::Pick(index) => {
                if let Some(choice) = self.chat.choices.get(index).cloned() {
                    self.chat.picker_selected = index;
                    if !matches!(choice.action, ChatAction::TaskDetails(_) | ChatAction::SetOutputDetail(_) | ChatAction::SetThinkingDetail(_)) {
                        self.chat.picker = None;
                    }
                    self.chat_action(choice.action);
                }
            }
            ChatAction::TaskDetails(index) => {
                if self.chat.picker == Some(Picker::Tasks) {
                    self.chat.picker_selected = index.min(self.chat.choices.len().saturating_sub(1));
                }
            }
            ChatAction::Connect(index) => {
                if let Some(config) = self
                    .settings_effective
                    .acp_agents
                    .as_ref()
                    .and_then(|agents| agents.get(index))
                    .cloned()
                {
                    self.connect_acp(config);
                }
            }
            ChatAction::SwitchAgent(id) => self.select_chat_agent(id),
            ChatAction::CloseAgent(id) => self.request_close_chat_agent(id),
            ChatAction::AgentPage(previous) => {
                self.chat.agent_tab_offset = if previous { self.chat.agent_tab_offset.saturating_sub(1) }
                    else { (self.chat.agent_tab_offset + 1).min(self.chat.agent_order.len().saturating_sub(1)) };
                self.chat.reveal_agent = false;
            }
            ChatAction::Reconnect => {
                if let Some(config) = self.chat.agent.clone() {
                    self.start_acp_connection(config, true);
                }
            }
            ChatAction::Disconnect => {
                self.chat.discard_history_load();
                self.chat.model_change = None;
                self.pause_chat_queues();
                self.chat.client = None;
                self.chat.initialized = None;
                self.chat.permissions.clear();
                self.chat.reset_yolo();
                self.chat.control_busy = false;
                self.chat.control_request = None;
                self.chat.status = "Disconnected".into();
                for session in self.chat.sessions.values_mut() {
                    session.disconnect();
                }
                self.chat.restore_draft();
            }
            ChatAction::New => {
                if !self.chat.control_busy && !self.chat.closing {
                    self.chat.opening_group = self.chat.layout.active.map(|_| self.active_group);
                    self.chat.control_busy = self.send_acp(Command::NewSession);
                    self.chat.status = "Creating conversation…".into();
                }
            }
            ChatAction::Send => self.send_chat_prompt(),
            ChatAction::MoveToDock => {
                if let Some(id) = self.chat.selected.clone() { self.move_chat_to_dock(id); }
            }
            ChatAction::PermissionDetails => {
                if let Some(index) = self.chat.permission().and_then(|p| {
                    self.chat.session()?.entries.iter().position(|entry| {
                        matches!(entry, ChatEntry::Tool(tool) if tool.tool_call_id == p.request.tool_call.tool_call_id)
                    })
                }) {
                    self.chat.expanded.insert(index);
                    self.chat.collapsed.remove(&index);
                    self.chat.reveal_entry = Some(index);
                    self.chat.follow = false;
                    self.chat.picker = None;
                }
            }
            ChatAction::Stop => {
                if let Some(id) = self.chat.selected.clone() {
                    if let Some(queue) = self.chat.queues.get_mut(&id) { queue.paused = true; queue.priority = None; queue.pause_after_priority = false; }
                    self.cancel_chat_turn(&id);
                }
            }
            ChatAction::EditQueued(id) => self.edit_queued_prompt(id),
            ChatAction::RemoveQueued(id) => self.remove_queued_prompt(id),
            ChatAction::SendQueued(id) => self.send_queued_prompt_now(id),
            ChatAction::PauseQueue => {
                if let Some(queue) = self.chat.connection.selected.as_ref().and_then(|id| self.chat.connection.queues.get_mut(id)) {
                    queue.paused = !queue.paused;
                    queue.pause_after_priority = false;
                    if queue.paused { queue.priority = None; }
                }
                self.background.wake_handle().notify();
            }
            ChatAction::QueuePage(previous) => {
                if let Some(queue) = self.chat.connection.selected.as_ref().and_then(|id| self.chat.connection.queues.get_mut(id)) {
                    let page = queue.visible_rows.max(1);
                    queue.scroll = if previous { queue.scroll.saturating_sub(page) } else { (queue.scroll + page).min(queue.items.len().saturating_sub(1)) };
                }
            }
            ChatAction::Follow => {
                self.chat.follow = true;
                self.chat.scroll = self.chat.max_scroll;
            }
            ChatAction::Attach => self.browse_chat_files(),
            ChatAction::RemoveAttachment(index) => {
                let selected = self.chat.selected.clone();
                self.chat.files.attaching.remove(&selected);
                if index < self.chat.files.attached.len() {
                    self.chat.files.attached.remove(index);
                    self.chat.files.scroll = self
                        .chat
                        .files
                        .scroll
                        .min(self.chat.files.attached.len().saturating_sub(3));
                    self.chat.focus = None;
                    self.chat.error = None;
                }
            }
            ChatAction::AttachmentPage(up) => {
                self.chat.files.scroll = if up {
                    self.chat.files.scroll.saturating_sub(3)
                } else {
                    (self.chat.files.scroll + 3)
                        .min(self.chat.files.attached.len().saturating_sub(3))
                };
            }
            ChatAction::ChooseFile(index) => self.choose_chat_file(index),
            ChatAction::LoadById => {
                self.chat.auxiliary_input();
                self.chat.loading_id = true;
                self.chat.terminal_input = None;
            }
            ChatAction::Switch(id) => {
                if self.activate_editor_chat(&id) { return; }
                // Replay alone must not expose an unestablished session.
                if self.chat.loading_session.as_ref() == Some(&id) { return; }
                if self.chat.sessions.contains_key(&id) {
                    self.chat.open_tab(id);
                } else if !self.chat.control_busy {
                    self.chat.restore_draft();
                    let command = if self
                        .chat
                        .initialized
                        .as_ref()
                        .is_some_and(|i| i.agent_capabilities.load_session)
                    {
                        Command::LoadSession(id.clone())
                    } else {
                        Command::ResumeSession(id.clone())
                    };
                    self.chat.control_busy = self.send_acp(command);
                    if self.chat.control_busy {
                        self.chat.loading_session = Some(id);
                        self.chat.status = "Loading conversation…".into();
                    }
                }
            }
            ChatAction::CloseTab(id) => {
                if self.activate_editor_chat(&id) {
                    self.close_tab(self.active_tab);
                    return;
                }
                self.chat.close_tab(&id);
                if self.chat.empty_conversation(&id) {
                    if self.chat.supports_history_deletion() && self.chat.connected() {
                        self.chat.deferred_discards.insert(id.clone());
                    }
                    self.chat.removed_sessions.insert(id.clone());
                    self.chat.forget_conversation(&id);
                    self.discard_empty_chat_history();
                }
            }
            ChatAction::DeleteSession(id) => self.delete_chat_history(id),
            ChatAction::TabPage(previous) => {
                self.chat.tab_offset = if previous {
                    self.chat.tab_offset.saturating_sub(1)
                } else {
                    (self.chat.tab_offset + 1).min(self.chat.open_tabs.len().saturating_sub(1))
                };
                self.chat.reveal_tab = false;
            }
            ChatAction::Mode(mode_id) => {
                if !self.chat.busy()
                    && !self.chat.control_busy
                    && let Some(session_id) = self.chat.selected.clone()
                {
                    self.chat.control_busy = self.send_acp(Command::SetMode {
                        session_id,
                        mode_id,
                    });
                }
            }
            ChatAction::Config(config_id, value) => {
                if !self.chat.busy()
                    && !self.chat.control_busy
                    && self.chat.session().is_some_and(|s| {
                        s.config_options.iter().any(|o| o.id == config_id && config_changeable(o))
                    })
                    && let Some(session_id) = self.chat.selected.clone()
                {
                    self.chat.control_busy = self.send_acp(Command::SetConfig {
                        session_id,
                        config_id,
                        value,
                    });
                }
            }
            ChatAction::Model(config_id, value) => {
                if self.chat.busy() || self.chat.control_busy { return; }
                let Some(option) = self.chat.primary_config(p::SessionConfigOptionCategory::Model)
                    .filter(|o| o.id == config_id) else { return; };
                let current = matches!((&option.kind, &value),
                    (p::SessionConfigKind::Select(select), p::SessionConfigOptionValue::ValueId { value })
                        if select.current_value == *value);
                if !current {
                    if !config_choices(option).iter().any(|c| c.action == ChatAction::Config(config_id.clone(), value.clone())) {
                        return;
                    }
                    let Some(session_id) = self.chat.selected.clone() else { return; };
                    if !self.send_acp(Command::SetConfig { session_id: session_id.clone(), config_id, value }) {
                        return;
                    }
                    self.chat.control_busy = true;
                    self.chat.model_change = Some(session_id);
                    self.chat.error = None;
                    self.chat.status = "Changing model…".into();
                }
                self.chat.picker = None;
                self.chat_action(ChatAction::Picker(Picker::Thinking));
            }
            ChatAction::Auth(id) => {
                if !self.chat.control_busy {
                    self.chat.control_busy = self.send_acp(Command::Authenticate(id));
                    self.chat.status = "Authenticating…".into();
                }
            }
            ChatAction::AgentCommand(name) => {
                self.chat.input = format!("/{name} ");
                self.chat.cursor = self.chat.input.len();
                self.chat.anchor = None;
            }
            ChatAction::MoreSessions => {
                if self.chat.control_busy || self.chat.next_cursor.is_none() { return; }
                self.chat.control_busy = self.send_acp(Command::ListSessions {
                    cursor: self.chat.next_cursor.clone(),
                });
                self.build_chat_choices(Picker::Sessions);
            }
            ChatAction::Permission(id, ticket, option) => {
                self.send_acp(Command::Permission { id, ticket, option });
            }
            ChatAction::ToggleEntry(index) => {
                if self.chat.session().is_some_and(|s| {
                    matches!(s.entries.get(index), Some(ChatEntry::Terminal(_) | ChatEntry::Tool(_)))
                }) {
                    self.chat.follow = false;
                }
                if self.chat.entry_expanded(index) {
                    self.chat.expanded.remove(&index);
                    self.chat.collapsed.insert(index);
                } else {
                    // Opening current thinking at the live end resumes its
                    // stream after tool inspection paused following. Preserve
                    // the scroll anchor when the user is reading history.
                    if self.chat.scroll == self.chat.max_scroll
                        && self.chat.session().is_some_and(|s| s.busy
                            && index == s.entries.len().saturating_sub(1)
                            && matches!(s.entries.get(index), Some(ChatEntry::Thought(_))))
                    {
                        self.chat.follow = true;
                    }
                    if self.chat.cache.entry_has_terminal(index) {
                        self.chat.expansion_scroll = Some((index, self.chat.cache.entry_height(index)));
                    }
                    self.chat.expanded.insert(index);
                    self.chat.collapsed.remove(&index);
                }
            }
            ChatAction::OutputDetail => {
                self.chat.output_detail = match self.chat.output_detail {
                    OutputDetail::Minimal => OutputDetail::Compact,
                    OutputDetail::Compact => OutputDetail::Full,
                    OutputDetail::Full => OutputDetail::Minimal,
                };
                self.chat.reset_expansion(false);
                self.chat.cache = Default::default();
                self.save_settings();
            }
            ChatAction::SetOutputDetail(detail) => {
                if self.chat.output_detail != detail {
                    self.chat.output_detail = detail;
                    self.chat.reset_expansion(false);
                    self.chat.cache = Default::default();
                    self.save_settings();
                }
                if self.chat.picker == Some(Picker::Display) {
                    self.build_chat_choices(Picker::Display);
                }
            }
            ChatAction::Yolo => self.toggle_chat_yolo(Self::send_acp),
            ChatAction::ThinkingDetail => {
                self.chat.thinking_expanded = !self.chat.thinking_expanded;
                self.chat.reset_expansion(true);
                self.save_settings();
            }
            ChatAction::SetThinkingDetail(expanded) => {
                if self.chat.thinking_expanded != expanded {
                    self.chat.thinking_expanded = expanded;
                    self.chat.reset_expansion(true);
                    self.save_settings();
                }
                if self.chat.picker == Some(Picker::Display) {
                    self.build_chat_choices(Picker::Display);
                }
            }
            ChatAction::Copy(index) => {
                if let Some(entry) = self.chat.session().and_then(|s| s.entries.get(index)) {
                    let text = match entry {
                        ChatEntry::Agent(s) | ChatEntry::User(s) | ChatEntry::Thought(s) => {
                            s.clone()
                        }
                        _ => return,
                    };
                    match crate::clipboard::set_text(&text) {
                        Ok(()) => {
                            self.push_clipboard_history(&text);
                            if !self.chat.control_busy {
                                self.chat.status = "Copied message".into();
                            }
                        }
                        Err(error) => self.chat.error = Some(format!("Clipboard: {error}")),
                    }
                }
            }
            ChatAction::Review(diff) => {
                self.open_acp_diff(&diff);
                if self.workbench_body_area.is_some_and(|a| a.width == 0) {
                    self.chat.visible = false;
                }
            }
            ChatAction::OpenFile(path, line) => {
                self.open_chat_location(&path, line);
            }
            ChatAction::Terminal(id) => {
                if self
                    .chat
                    .session()
                    .and_then(|s| s.terminals.get(&id))
                    .is_some_and(|t| !t.released && !t.killed && t.exit_status.is_none())
                {
                    self.chat.auxiliary_input();
                    self.chat.terminal_input = Some(id);
                    self.chat.loading_id = false;
                    self.focus_chat();
                }
            }
            ChatAction::StopTerminal(terminal_id) => {
                if let Some(session_id) = self.chat.selected.clone() {
                    if self.send_acp(Command::KillTerminal {
                        session_id,
                        terminal_id: terminal_id.clone(),
                    }) {
                        if let Some(t) = self.chat.session_mut()
                            .and_then(|s| s.terminals.get_mut(&terminal_id))
                        {
                            t.killed = true;
                        }
                        if self.chat.terminal_input.as_ref() == Some(&terminal_id) {
                            self.chat.restore_draft();
                        }
                    }
                }
            }
            ChatAction::RevealTerminal(id) => self.reveal_chat_terminal(id),
            ChatAction::FocusTerminal(id) => self.chat.focus = Some(ChatAction::FocusTerminal(id)),
            ChatAction::ScrollTerminal(id, amount) => self.scroll_chat_terminal(&id, amount),
            ChatAction::PanTerminal(id, amount) => self.pan_chat_terminal(&id, amount),
            ChatAction::TerminalTop(id) => {
                self.chat.follow = false;
                if let Some(view) = self.chat.terminal_views.get_mut(&id) {
                    view.first = Some(0);
                    view.column = 0;
                }
                self.chat.focus = Some(ChatAction::FocusTerminal(id));
            }
            ChatAction::FollowTerminal(id) => {
                if let Some(view) = self.chat.terminal_views.get_mut(&id) {
                    view.first = None;
                }
                self.chat.focus = Some(ChatAction::FocusTerminal(id));
            }
            ChatAction::BackToChat => self.chat.restore_draft(),
            ChatAction::CloseSession => {
                if self.chat.has_queued_prompt() {
                    self.chat.error = Some("Edit or remove queued messages before closing this conversation.".into());
                    return;
                }
                if !self.chat.busy()
                    && !self.chat.control_busy && !self.chat.checkpoint_pending()
                    && let Some(id) = self.chat.selected.clone()
                {
                    self.chat.control_busy = self.send_acp(Command::CloseSession(id));
                }
            }

        }
    }

    fn send_chat_prompt(&mut self) {
        if self
            .chat
            .selected
            .as_ref()
            .is_some_and(|id| self.chat.deleting_sessions.contains(id))
        {
            return;
        }
        if self.chat.input.trim().is_empty()
            && self.chat.terminal_input.is_none()
            && (self.chat.files.attached.is_empty() || self.chat.loading_id)
            && !self.chat.files.attaching.contains_key(&self.chat.selected)
        {
            return;
        }
        if self.chat.loading_id {
            let id = p::SessionId::new(self.chat.input.trim().to_owned());
            self.chat.restore_draft();
            self.chat_action(ChatAction::Switch(id));
            return;
        } else if let Some(terminal_id) = self.chat.terminal_input.clone() {
            if let Some(session_id) = self.chat.selected.clone()
                && !self.send_acp(Command::TerminalInput {
                    session_id,
                    terminal_id,
                    text: format!("{}\n", self.chat.input),
                })
            {
                return;
            }
        } else {
            if self.remote_workspace.is_some() {
                self.queue_remote_chat_prompt();
                return;
            }
            if self.chat.busy()
                || self.chat.has_queued_prompt()
                || self.chat.checkpoint_pending()
                || self.chat.rollback.initializing()
            {
                self.enqueue_chat_prompt();
                return;
            }
            if self.chat.busy() || self.chat.control_busy {
                return;
            }
            let Some(session_id) = self.chat.selected.clone() else {
                self.chat.error = Some("Connect an agent and create a conversation first.".into());
                return;
            };
            let (content, text) = match self.chat_prompt_with_files() {
                Ok(prompt) => prompt,
                Err(error) => {
                    self.chat.error = Some(error);
                    return;
                }
            };
            if !self.send_chat_with_checkpoint(session_id, content, &text) {
                return;
            }
            if let Some(session) = self.chat.session_mut() {
                session.begin_prompt(text);
            }
            self.chat.reveal_tab = true;
            self.chat.files.attached.clear();
            self.chat.files.scroll = 0;
            self.chat.files.dismiss();
            self.chat.error = None;
            self.chat.follow = true;
            if let Some(id) = self.chat.selected.clone() {
                self.set_chat_history_hidden(&id, false);
            }
            self.chat.status = "Responding…".into();
        }
        self.chat.input.clear();
        self.chat.cursor = 0;
        self.chat.anchor = None;
        self.chat.input_history = Default::default();
    }

    fn build_chat_choices(&mut self, picker: Picker) {
        if self.build_rollback_choices(&picker) { return; }
        let mut choices = Vec::new();
        let mut add = |label: String, detail: String, action| {
            choices.push(Choice {
                label,
                detail,
                action,
            })
        };
        match &picker {
            Picker::Display => {
                for (detail, description) in [
                    (OutputDetail::Minimal, "Show tool summaries and commands. Expand individual cards for output."),
                    (OutputDetail::Compact, "Show a short output preview. Expand a card to read more."),
                    (OutputDetail::Full, "Expand tool output by default. Terminals keep their scrollable viewport."),
                ] {
                    let mark = if self.chat.output_detail == detail { "✓ " } else { "" };
                    add(format!("{mark}Tool output: {}", detail.label()), description.into(),
                        ChatAction::SetOutputDetail(detail));
                }
                for (expanded, label, description) in [
                    (false, "Collapsed", "Keep thinking text folded. Individual thinking cards can still be expanded."),
                    (true, "Expanded", "Show thinking text as it streams, before the answer. This does not change model effort."),
                ] {
                    let mark = if self.chat.thinking_expanded == expanded { "✓ " } else { "" };
                    add(format!("{mark}Thinking text: {label}"), description.into(),
                        ChatAction::SetThinkingDetail(expanded));
                }
            }
            Picker::Tasks => {
                if let Some(tasks) = self.chat.session().and_then(|s| s.tasks.as_ref()) {
                    for (index, entry) in tasks.entries.iter().enumerate() {
                        let priority = match entry.priority {
                            Some(p::PlanEntryPriority::High) => "High priority",
                            Some(p::PlanEntryPriority::Medium) => "Medium priority",
                            Some(p::PlanEntryPriority::Low) => "Low priority",
                            _ => "Priority not reported",
                        };
                        add(entry.content.lines().next().unwrap_or_default().into(),
                            format!("{} · {priority}\n{}", entry.status.label(), entry.content),
                            ChatAction::TaskDetails(index));
                    }
                }
            }
            Picker::Thinking => {
                if !self.chat.thinking_loading() {
                    if let Some(option) = self.chat.thinking_option() {
                        for choice in config_choices(option) {
                            add(choice.label, choice.detail, choice.action);
                        }
                    } else {
                        let (value, reason) = self.chat.thinking_status();
                        add(format!("Thinking: {value}"), reason.unwrap_or_default(), ChatAction::DismissPicker);
                    }
                    if self.chat.primary_config(p::SessionConfigOptionCategory::Model).is_some() {
                        add("← Models".into(), "Choose a different model".into(), ChatAction::Picker(Picker::Models));
                    }
                }
            }
            Picker::Message(index, epoch) => {
                if let Some(session) = self.chat.session()
                    && session.history_epoch == *epoch
                    && matches!(session.entries.get(*index), Some(ChatEntry::User(_)))
                {
                    add("Copy message".into(), String::new(), ChatAction::Copy(*index));
                    if let Some(id) = session.checkpoints.get(index) {
                        add(
                            "Restore files before this message".into(),
                            "Review this message's file checkpoint".into(),
                            ChatAction::Picker(Picker::Restore(*id)),
                        );
                    }
                }
            }
            Picker::ThinkingInfo => {
                let (value, reason) = self.chat.thinking_status();
                add(
                    format!("Thinking: {value}"),
                    reason.unwrap_or_default(),
                    ChatAction::DismissPicker,
                );
            }
            Picker::Attachments => {
                for (index, path) in self.chat.files.attached.iter().enumerate() {
                    add(
                        crate::app_chat_files::file_label(path, self.chat_file_root()),
                        "Remove from this message".into(),
                        ChatAction::RemoveAttachment(index),
                    );
                }
            }
            Picker::Agents => {
                for &id in &self.chat.agent_order {
                    if let Some(state) = self.chat.agent_state(id) {
                        add(format!("{}{}", if id == self.chat.active_agent { "✓ " } else { "" }, state.tab_name()),
                            format!("{} · Switch to open agent", state.activity()), ChatAction::SwitchAgent(id));
                    }
                }
                for (index, agent) in self.settings_effective.acp_agents.iter().flatten().enumerate() {
                    if self.chat.configured_agent(agent).is_none() {
                        add(agent.name.clone(), "Open in a new agent tab".into(), ChatAction::Connect(index));
                    }
                }
                add(
                    "Configure agents…".into(),
                    String::new(),
                    ChatAction::Configure,
                );
                if self.chat.initialized.as_ref().is_some_and(|info| !info.auth_methods.is_empty()) {
                    add("Authenticate…".into(), "Sign in to this agent".into(), ChatAction::Picker(Picker::Auth));
                }
                if self.chat.connected() {
                    add("Disconnect this agent".into(), String::new(), ChatAction::Disconnect);
                } else if self.chat.agent.is_some() && !self.chat.closing {
                    add("Reconnect this agent".into(), String::new(), ChatAction::Reconnect);
                }
            }
            Picker::Sessions => {
                add("New conversation".into(), String::new(), ChatAction::New);
                for id in &self.chat.session_order {
                    if let Some(session) = self.chat.sessions.get(id)
                        && session.has_conversation
                        && !self.chat.removed_sessions.contains(id)
                    {
                        add(
                            format!(
                                "{}{}{}",
                                if self.chat.selected.as_ref() == Some(id) {
                                    "✓ "
                                } else {
                                    ""
                                },
                                session.title,
                                if self
                                    .chat
                                    .permissions
                                    .iter()
                                    .any(|p| &p.request.session_id == id)
                                {
                                    " · permission needed"
                                } else {
                                    ""
                                }
                            ),
                            self.chat.history_detail(id),
                            ChatAction::Switch(id.clone()),
                        );
                    }
                }
                if self.chat.initialized.as_ref().is_some_and(|i| {
                    i.agent_capabilities.load_session
                        || i.agent_capabilities.session_capabilities.resume.is_some()
                }) {
                    for session in &self.chat.remote_sessions {
                        if !self.chat.sessions.contains_key(&session.session_id)
                            && !self.chat.removed_sessions.contains(&session.session_id)
                        {
                            add(
                                session
                                    .title
                                    .clone()
                                    .unwrap_or_else(|| session.session_id.to_string()),
                                self.chat.history_detail(&session.session_id),
                                ChatAction::Switch(session.session_id.clone()),
                            );
                        }
                    }
                    add(
                        "Load by session ID…".into(),
                        String::new(),
                        ChatAction::LoadById,
                    );
                }
                if self.chat.next_cursor.is_some() {
                    add(
                        "Load more conversations…".into(),
                        String::new(),
                        ChatAction::MoreSessions,
                    );
                }
                if self
                    .chat
                    .initialized
                    .as_ref()
                    .is_some_and(|i| i.agent_capabilities.session_capabilities.close.is_some())
                    && self.chat.selected.is_some()
                {
                    add(
                        "Close current conversation".into(),
                        String::new(),
                        ChatAction::CloseSession,
                    );
                }
            }
            Picker::Modes | Picker::Models => {
                let category = if picker == Picker::Modes {
                    p::SessionConfigOptionCategory::Mode
                } else {
                    p::SessionConfigOptionCategory::Model
                };
                if let Some(option) = self.chat.primary_config(category.clone()) {
                    for choice in config_choices(option) {
                        let action = match choice.action {
                            ChatAction::Config(id, value) if picker == Picker::Models => ChatAction::Model(id, value),
                            action => action,
                        };
                        add(choice.label, choice.detail, action);
                    }
                    if let Some(session) = self.chat.session() {
                        for extra in session.config_options.iter().filter(|extra| {
                            extra.id != option.id
                                && config_category(extra) == Some(category.clone())
                        }) {
                            add(
                                extra.name.clone(),
                                config_value_name(extra).into(),
                                ChatAction::Picker(Picker::Values(extra.id.to_string())),
                            );
                        }
                    }
                    if picker == Picker::Models {
                        let (value, reason) = self.chat.thinking_status();
                        add(format!("Thinking: {value}…"),
                            reason.unwrap_or_else(|| "Change thinking for the current model".into()),
                            ChatAction::Picker(Picker::Thinking));
                    }
                } else if picker == Picker::Modes
                    && let Some(modes) = self.chat.session().and_then(|s| s.modes.as_ref())
                {
                    for mode in &modes.available_modes {
                        add(
                            format!(
                                "{}{}",
                                if mode.id == modes.current_mode_id {
                                    "✓ "
                                } else {
                                    ""
                                },
                                mode.name
                            ),
                            mode.description.clone().unwrap_or_default(),
                            ChatAction::Mode(mode.id.clone()),
                        );
                    }
                }
            }
            Picker::Options => {
                if self
                    .chat
                    .primary_config(p::SessionConfigOptionCategory::Mode)
                    .is_none()
                    && let Some(name) = self.chat.mode_name()
                {
                    add(
                        "Mode".into(),
                        name.into(),
                        ChatAction::Picker(Picker::Modes),
                    );
                }
                if let Some(session) = self.chat.session() {
                    for option in session
                        .config_options
                        .iter()
                        .filter(|option| config_supported(option))
                    {
                        let readonly_thinking = self.chat.thinking_option()
                            .is_some_and(|o| o.id == option.id)
                            && !config_changeable(option);
                        add(
                            option.name.clone(),
                            config_value_name(option).into(),
                            ChatAction::Picker(if readonly_thinking {
                                Picker::ThinkingInfo
                            } else if self.chat.primary_config(p::SessionConfigOptionCategory::Model)
                                .is_some_and(|o| o.id == option.id) {
                                Picker::Models
                            } else {
                                Picker::Values(option.id.to_string())
                            }),
                        );
                    }
                    if self.chat.thinking_option().is_none() {
                        add(
                            "Thinking: ?".into(),
                            "The agent does not expose thinking effort".into(),
                            ChatAction::Picker(Picker::ThinkingInfo),
                        );
                    }
                }
            }
            Picker::Values(id) => {
                if let Some(option) = self
                    .chat
                    .session()
                    .and_then(|s| s.config_options.iter().find(|o| o.id.to_string() == *id))
                {
                    for choice in config_choices(option) {
                        add(choice.label, choice.detail, choice.action);
                    }
                }
            }
            Picker::Auth => {
                if let Some(info) = &self.chat.initialized {
                    for method in &info.auth_methods {
                        add(
                            method.name().into(),
                            method.description().unwrap_or_default().into(),
                            ChatAction::Auth(method.id().clone()),
                        );
                    }
                }
            }
            Picker::Commands => {
                if let Some(session) = self.chat.session() {
                    if self.chat.rollback.enabled && session.rollback.edits.iter().any(|edit| edit.pending() > 0) {
                        add("Review agent changes…".into(), "Review pending changes by file or chunk.".into(),
                            ChatAction::Picker(Picker::Changes));
                    }
                    for command in &session.commands {
                        add(
                            format!("/{}", command.name),
                            command.description.clone(),
                            ChatAction::AgentCommand(command.name.clone()),
                        );
                    }
                    if self.chat.rollback.enabled {
                        add("Message checkpoints…".into(),
                            "Review saved file states, or click a user message to restore its checkpoint.".into(),
                            ChatAction::Picker(Picker::Checkpoints));
                    }
                }
            }
            Picker::Permissions => {
                if let Some(permission) = self.chat.permission() {
                    add("View full request details".into(), String::new(), ChatAction::PermissionDetails);
                    for option in &permission.request.options {
                        add(
                            option.name.clone(),
                            String::new(),
                            ChatAction::Permission(
                                permission.id.clone(),
                                permission.ticket,
                                Some(option.option_id.clone()),
                            ),
                        );
                    }
                    add(
                        "Cancel request".into(),
                        String::new(),
                        ChatAction::Permission(permission.id.clone(), permission.ticket, None),
                    );
                }
            }
            Picker::Changes | Picker::ChangedFile(_) | Picker::Change(_) | Picker::ChangeChunk(_, _) | Picker::Checkpoints | Picker::Restore(_) => unreachable!(),
        }
        self.chat.choices = choices;
        self.chat.picker = Some(picker);
    }

    pub(crate) fn handle_chat_key(&mut self, key: KeyEvent) -> bool {
        if !self.chat.visible
            || self.active_pane != ActivePane::Chat
            || crate::ui_quick_picker::active(self).is_some()
            || crate::ui_modal::active(self).is_some()
            || self.palette.active
        {
            return false;
        }
        if self.chat.picker.is_none() && self.chat_terminal_key(key) {
            return true;
        }
        if self
            .lookup_keybinding(key.modifiers, key.code)
            .is_some_and(|id| {
                matches!(
                    id,
                    "quit"
                        | "save"
                        | "palette"
                        | "acp_chat"
                        | "toggle_explorer"
                        | "toggle_terminal"
                        | "global_search"
                        | "source_control"
                        | "extensions"
                        | "settings"
                        | "toggle_menu_bar"
                        | "toggle_problems"
                        | "show_shortcuts"
                        | "new_terminal"
                        | "new_terminal_in_editor_tab"
                        | "new_file"
                        | "go_to_line"
                        | "clipboard_history"
                )
            })
        {
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(
                key.code,
                KeyCode::Tab | KeyCode::BackTab | KeyCode::PageUp | KeyCode::PageDown
            )
        {
            if self.chat.layout.active.is_some() {
                self.active_pane = ActivePane::Editor;
                return false;
            }
            if self.chat.open_tabs.len() > 1 {
                let current = self
                    .chat
                    .open_tabs
                    .iter()
                    .position(|id| Some(id) == self.chat.selected.as_ref())
                    .unwrap_or(0);
                let previous = matches!(key.code, KeyCode::BackTab | KeyCode::PageUp)
                    || key.modifiers.contains(KeyModifiers::SHIFT);
                let count = self.chat.open_tabs.len();
                let next = if previous {
                    (current + count - 1) % count
                } else {
                    (current + 1) % count
                };
                self.chat_action(ChatAction::Switch(self.chat.open_tabs[next].clone()));
                return true;
            }
            return false;
        }
        if key.modifiers == KeyModifiers::CONTROL
            && key.code == KeyCode::Char('w')
            && let Some(id) = self.chat.selected.clone()
        {
            self.chat_action(ChatAction::CloseTab(id));
            return true;
        }
        if key.code == KeyCode::F(1)
            || key.code == KeyCode::F(11)
            || (key.modifiers == KeyModifiers::CONTROL
                && matches!(key.code, KeyCode::Char('p' | 'q' | ',')))
            || (key
                .modifiers
                .contains(KeyModifiers::CONTROL | KeyModifiers::SHIFT)
                && matches!(key.code, KeyCode::Char('p' | 'P' | 'i' | 'I')))
        {
            return false;
        }
        if self.chat.picker.is_some() {
            if self.chat.picker == Some(Picker::Sessions) && key.code == KeyCode::Delete {
                if let Some(Choice {
                    action: ChatAction::Switch(id),
                    ..
                }) = self.chat.choices.get(self.chat.picker_selected).cloned()
                {
                    self.chat_action(ChatAction::DeleteSession(id));
                }
                return true;
            }
            match key.code {
                KeyCode::Esc => self.chat.picker = None,
                KeyCode::Up | KeyCode::BackTab => {
                    self.chat.picker_selected = self.chat.picker_selected.saturating_sub(1)
                }
                KeyCode::Down | KeyCode::Tab => {
                    self.chat.picker_selected = (self.chat.picker_selected + 1)
                        .min(self.chat.choices.len().saturating_sub(1))
                }
                KeyCode::Enter => self.chat_action(ChatAction::Pick(self.chat.picker_selected)),
                _ => {}
            }
            return true;
        }
        if self.chat.files.completion.is_some()
            && self.chat.focus.is_none()
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            let completion = self.chat.files.completion.as_mut().unwrap();
            match key.code {
                KeyCode::Esc => {
                    self.chat.files.dismiss();
                    return true;
                }
                KeyCode::Up | KeyCode::BackTab => {
                    completion.selected = completion.selected.saturating_sub(1);
                    return true;
                }
                KeyCode::Down => {
                    completion.selected =
                        (completion.selected + 1).min(completion.paths.len().saturating_sub(1));
                    return true;
                }
                KeyCode::Tab | KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                    let selected = completion.selected;
                    if !completion.paths.is_empty() {
                        self.choose_chat_file(selected);
                    }
                    return true;
                }
                _ => {}
            }
        }
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                let n = self.chat.hits.len();
                if n > 0 {
                    let previous = self
                        .chat
                        .focus
                        .as_ref()
                        .and_then(|a| self.chat.hits.iter().position(|h| &h.action == a));
                    let index = match previous {
                        None => 0,
                        Some(i)
                            if key.code == KeyCode::BackTab
                                || key.modifiers.contains(KeyModifiers::SHIFT) =>
                        {
                            (i + n - 1) % n
                        }
                        Some(i) => (i + 1) % n,
                    };
                    self.chat.focus = Some(self.chat.hits[index].action.clone());
                }
            }
            KeyCode::Enter if self.chat.focus.is_some() => {
                if let Some(action) = self.chat.focus.clone()
                    && self.chat.hits.iter().any(|h| h.action == action)
                {
                    self.chat_action(action);
                }
            }
            KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.chat_action(ChatAction::Send)
            }
            KeyCode::Esc => {
                if self.chat.focus.take().is_some() {
                } else if self.chat.terminal_input.is_some() || self.chat.loading_id {
                    self.chat_action(ChatAction::BackToChat);
                } else if self.chat.busy() {
                    self.chat_action(ChatAction::Stop);
                } else {
                    self.active_pane = ActivePane::Editor;
                }
            }
            KeyCode::PageUp => {
                self.chat.follow = false;
                self.chat.scroll = self.chat.scroll.saturating_sub(10);
            }
            KeyCode::PageDown => {
                self.chat.scroll = (self.chat.scroll + 10).min(self.chat.max_scroll);
                self.chat.follow = self.chat.scroll == self.chat.max_scroll;
            }
            KeyCode::End
                if key.modifiers.contains(KeyModifiers::CONTROL) && self.chat.input.is_empty() =>
            {
                self.chat_action(ChatAction::Follow)
            }
            _ => {
                self.chat.focus = None;
                self.edit_text_input(TextInputId::Chat, key.code, key.modifiers, None);
            }
        }
        true
    }

    pub(crate) fn edit_chat_input(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        paste: Option<&str>,
    ) {
        let clipboard_paste = paste.is_none()
            && ((modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
                && !modifiers.contains(KeyModifiers::ALT)
                && matches!(code, KeyCode::Char('v' | 'V')))
                || (modifiers == KeyModifiers::SHIFT && code == KeyCode::Insert));
        let clipboard = if clipboard_paste {
            crate::clipboard::get_text().ok()
        } else {
            None
        };
        let paste = paste.or(clipboard.as_deref());
        if paste.is_some_and(|text| self.try_paste_chat_files(text)) {
            self.refresh_chat_input_layout();
            return;
        }
        let state = &mut self.chat.connection;
        state
            .input_layout
            .layout(&state.input, state.input_layout.width.max(1));
        let vertical = matches!(code, KeyCode::Up | KeyCode::Down)
            && !modifiers.contains(KeyModifiers::CONTROL)
            && paste.is_none();
        if vertical {
            use unicode_width::UnicodeWidthStr;
            let row = state.input_layout.cursor_row(state.cursor);
            if let Some(range) = state.input_layout.rows.get(row) {
                let column = state.input[range.start..state.cursor.min(range.end)].width();
                let column = *state.input_goal.get_or_insert(column);
                let target = if code == KeyCode::Up {
                    row.saturating_sub(1)
                } else {
                    (row + 1).min(state.input_layout.rows.len().saturating_sub(1))
                };
                if modifiers.contains(KeyModifiers::SHIFT) {
                    state.anchor.get_or_insert(state.cursor);
                } else {
                    state.anchor = None;
                }
                state.cursor = state.input_layout.byte_at_column(target, column);
            }
        } else {
            state.input_goal = None;
            state.input_history.edit(
                TextInput::new(&mut state.input, &mut state.cursor, &mut state.anchor)
                    .multiline(true),
                code,
                modifiers,
                paste,
            );
        }
        self.refresh_chat_input_layout();
    }

    pub(crate) fn refresh_chat_input_layout(&mut self) {
        let state = &mut self.chat.connection;
        state
            .input_layout
            .layout(&state.input, state.input_layout.width.max(1));
        state.input_layout.reveal(state.cursor);
        self.maintain_remote_chat_paste();
        self.refresh_chat_file_completion();
    }

    pub(crate) fn handle_chat_mouse(&mut self, mouse: MouseEvent) -> bool {
        if self.menu_active {
            return false;
        }
        let point = (mouse.column, mouse.row).into();
        if self.handle_chat_tab_drag(mouse) { return true; }
        // A workbench tab dragged over a conversation must reach the normal
        // tab move/split handler, including drops inside its own chat pane.
        if self.dragged_chat_key().is_some()
            && matches!(mouse.kind, MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left))
        {
            return false;
        }
        self.activate_chat_surface(mouse);
        if self.handle_chat_scrollbar(mouse) { return true; }
        if self.chat.resizing {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(area) = self.chat.area {
                        self.chat.width = area.right().saturating_sub(mouse.column).max(36);
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => self.chat.resizing = false,
                _ => {}
            }
            return true;
        }
        let Some(area) = self.chat.area.filter(|a| a.contains(point)) else {
            if matches!(mouse.kind, MouseEventKind::Down(_)) {
                self.chat.picker = None;
                self.chat.focus = None;
                self.chat.files.dismiss();
            }
            return false;
        };
        if self.handle_chat_file_drop(mouse) {
            return true;
        }
        if self.chat.files.popup.is_some_and(|a| a.contains(point)) && self.chat.picker.is_none() {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(index) = self.chat.hits.iter().rev().find_map(|h| {
                        if h.area.contains(point)
                            && let ChatAction::ChooseFile(i) = h.action
                        {
                            Some(i)
                        } else {
                            None
                        }
                    }) {
                        self.choose_chat_file(index);
                    }
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    if let Some(c) = &mut self.chat.files.completion {
                        c.selected = if mouse.kind == MouseEventKind::ScrollUp {
                            c.selected.saturating_sub(3)
                        } else {
                            (c.selected + 3).min(c.paths.len().saturating_sub(1))
                        };
                    }
                }
                _ => {}
            }
            return true;
        }
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && mouse.column == area.x
            && area.x > 6
            && self.chat.layout.active.is_none()
        {
            self.chat.resizing = true;
            return true;
        }
        if self.chat.picker.is_none()
            && self.handle_background_text_input_mouse(
                mouse.kind,
                mouse.column,
                mouse.row,
                mouse.modifiers,
            )
        {
            self.chat.focus = None;
            self.refresh_chat_file_completion();
            return true;
        }
        match mouse.kind {
            MouseEventKind::Moved if self.chat.picker.is_some() => {
                if let Some(index) = self.chat.hits.iter().rev().find_map(|hit| {
                    if hit.area.contains(point) && let ChatAction::Pick(index) = hit.action {
                        Some(index)
                    } else {
                        None
                    }
                }) {
                    // Preview the hovered choice; only a click or Enter applies it.
                    self.chat.picker_selected = index;
                }
                self.mouse_hover_changed = true;
            }
            MouseEventKind::Down(MouseButton::Middle) if self.chat.picker.is_none() => {
                if let Some(action) = self.chat.hits.iter().rev().find_map(|h| {
                    if !h.area.contains(point) { return None; }
                    match &h.action {
                        ChatAction::Switch(id) | ChatAction::CloseTab(id) => Some(ChatAction::CloseTab(id.clone())),
                        ChatAction::SwitchAgent(id) | ChatAction::CloseAgent(id) => Some(ChatAction::CloseAgent(*id)),
                        _ => None,
                    }
                }) { self.chat_action(action); }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                self.chat.files.dismiss();
                self.focus_chat();
                let hit = self
                    .chat
                    .hits
                    .iter()
                    .rev()
                    .find(|h| h.area.contains(point))
                    .cloned();
                if let Some(hit) = hit {
                    if let ChatAction::Switch(id) = &hit.action {
                        self.chat.layout.dragging = Some(id.clone());
                        self.drag_start_pos = Some((mouse.column, mouse.row));
                        self.drag_has_moved = false;
                    }
                    self.chat_action(hit.action);
                } else {
                    self.chat.picker = None;
                    self.chat.focus = None;
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = mouse.kind == MouseEventKind::ScrollUp;
                if self.chat.picker.is_some() {
                    self.chat.picker_selected = if up {
                        self.chat.picker_selected.saturating_sub(2)
                    } else {
                        (self.chat.picker_selected + 2)
                            .min(self.chat.choices.len().saturating_sub(1))
                    };
                } else if self.chat.agent_tab_area.is_some_and(|area| area.contains(point)) {
                    self.chat_action(ChatAction::AgentPage(up));
                } else if self.chat.tab_area.is_some_and(|area| area.contains(point)) {
                    self.chat_action(ChatAction::TabPage(up));
                } else if let Some((id, _)) = self.chat.terminal_areas.iter()
                    .find(|(_, area)| area.contains(point)).cloned()
                {
                    self.scroll_chat_terminal(&id, if up { -3 } else { 3 });
                } else {
                    self.chat.scroll = if up {
                        self.chat.scroll.saturating_sub(3)
                    } else {
                        (self.chat.scroll + 3).min(self.chat.max_scroll)
                    };
                    self.chat.follow = self.chat.scroll == self.chat.max_scroll;
                }
            }
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight if self.chat.picker.is_none() => {
                if let Some((id, _)) = self.chat.terminal_areas.iter()
                    .find(|(_, area)| area.contains(point)).cloned()
                {
                    let amount = if mouse.kind == MouseEventKind::ScrollLeft { -8 } else { 8 };
                    self.pan_chat_terminal(&id, amount);
                }
            }
            _ => {}
        }
        true
    }
}
