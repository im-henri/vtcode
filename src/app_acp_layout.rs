//! Conversation placement uses real workbench tabs and editor-group drop zones.
use super::*;
use crate::app::TabDropZone;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Conversation {
    pub agent: AgentId,
    pub session: p::SessionId,
}

#[derive(Clone)]
pub(crate) struct Surface {
    pub agent: AgentId,
    pub key: Option<u64>,
    pub group: usize,
    pub session: Option<p::SessionId>,
    pub area: Rect,
    pub hits: Vec<ChatHit>,
    pub scrollbars: Vec<ChatScrollbar>,
    pub transcript: Option<Rect>,
    pub terminals: Vec<(String, Rect)>,
    pub tabs: Option<Rect>,
    pub agents: Option<Rect>,
}
pub(crate) struct ChatLayout {
    pub editors: HashMap<u64, Conversation>,
    pub active: Option<u64>,
    pub rendering: Option<u64>,
    pub dock_visible: bool,
    pub dock_agent: AgentId,
    pub placements: Vec<(u64, usize, Rect)>,
    pub surfaces: Vec<Surface>,
    pub dragging: Option<p::SessionId>,
}
impl Default for ChatLayout {
    fn default() -> Self {
        Self {
            editors: HashMap::new(),
            active: None,
            rendering: None,
            dock_visible: true,
            dock_agent: 0,
            placements: Vec::new(),
            surfaces: Vec::new(),
            dragging: None,
        }
    }
}

impl ChatState {
    pub(crate) fn editor_tab_title(&self, key: u64) -> String {
        self.layout
            .editors
            .get(&key)
            .and_then(|view| self.agent_state(view.agent)?.sessions.get(&view.session))
            .map_or("Conversation", |s| s.title.as_str())
            .chars()
            .filter(|c| !c.is_control())
            .collect()
    }
}

impl App {
    pub(crate) fn move_chat_to_dock(&mut self, id: p::SessionId) {
        if !self.activate_editor_chat(&id) {
            return;
        }
        let agent = self.chat.active_agent;
        self.close_tab(self.active_tab);
        self.chat.switch_connection(agent);
        self.chat.open_tab(id);
        self.chat.layout.dock_agent = self.chat.active_agent;
        self.chat.visible = true;
        self.active_pane = ActivePane::Chat;
        self.cancel_tab_drag();
    }

    pub(crate) fn place_chat_in_editor(
        &mut self,
        id: p::SessionId,
        group: usize,
        zone: Option<TabDropZone>,
    ) {
        if !self.chat.sessions.contains_key(&id) {
            return;
        }
        if !self.chat.background && group != self.active_group {
            self.switch_group(group);
        }
        let mut editor = crate::editor::Editor::new();
        editor.read_only = true;
        let key = editor.navigation_id;
        let mut tab = crate::app::Tab::new_file(editor);
        tab.kind = TabKind::Chat(key);
        if self.chat.background && group != self.active_group {
            let Some(target) = self.groups.get_mut(group) else {
                return;
            };
            target.tabs.push(tab);
        } else {
            self.tabs.push(tab);
            if !self.chat.background {
                self.active_tab = self.tabs.len() - 1;
            }
        }
        let view = Conversation {
            agent: self.chat.active_agent,
            session: id.clone(),
        };
        self.chat.layout.editors.insert(key, view);
        self.chat.open_tabs.retain(|tab| tab != &id);
        if self.chat.dock_selected.as_ref() == Some(&id) {
            self.chat.dock_selected = self.chat.open_tabs.last().cloned();
        }
        // Moving the last conversation of one agent must not hide the shared
        // agent selector while other connections are still open.
        if self.chat.agent_order.len() == 1
            && self.chat.open_tabs.is_empty()
            && self.chat.layout.dock_agent == self.chat.active_agent
            && !self.chat.background
        {
            self.chat.layout.dock_visible = false;
        }
        if self.chat.background {
            return;
        }
        if let Some(zone) = zone {
            self.create_split_group_at(self.active_tab, zone);
        }
        self.chat.select_tab(Some(id));
        self.chat.layout.active = Some(key);
        self.chat.visible = true;
        self.active_pane = ActivePane::Chat;
        self.ensure_active_tab_visible();
    }

    pub(crate) fn activate_editor_chat(&mut self, id: &p::SessionId) -> bool {
        let Some(key) = self.chat.layout.editors.iter().find_map(|(&key, view)| {
            (view.agent == self.chat.active_agent && &view.session == id).then_some(key)
        }) else {
            return false;
        };
        let found = self
            .tabs
            .iter()
            .position(|tab| tab.kind == TabKind::Chat(key))
            .map(|i| (self.active_group, i))
            .or_else(|| {
                self.groups.iter().enumerate().find_map(|(g, group)| {
                    group
                        .tabs
                        .iter()
                        .position(|tab| tab.kind == TabKind::Chat(key))
                        .map(|i| (g, i))
                })
            });
        let Some((group, index)) = found else {
            return false;
        };
        self.switch_group(group);
        self.active_tab = index;
        self.chat.select_tab(Some(id.clone()));
        self.chat.layout.active = Some(key);
        self.chat.visible = true;
        self.active_pane = ActivePane::Chat;
        self.ensure_active_tab_visible();
        true
    }

    pub(crate) fn activate_chat_surface(&mut self, mouse: MouseEvent) {
        if !matches!(
            mouse.kind,
            MouseEventKind::Down(_)
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight
        ) {
            return;
        }
        let Some(surface) = self
            .chat
            .layout
            .surfaces
            .iter()
            .find(|s| s.area.contains((mouse.column, mouse.row).into()))
            .cloned()
        else {
            return;
        };
        self.chat.switch_connection(surface.agent);
        if surface.key.is_some() && surface.group != self.active_group {
            self.switch_group(surface.group);
        }
        if self.chat.selected != surface.session || self.chat.layout.active != surface.key {
            self.chat.select_tab(surface.session.clone());
        }
        self.chat.layout.active = surface.key;
        self.chat.layout.rendering = surface.key;
        self.chat.area = Some(surface.area);
        self.chat.hits = surface.hits;
        self.chat.scrollbars = surface.scrollbars;
        self.chat.transcript_area = surface.transcript;
        self.chat.terminal_areas = surface.terminals;
        self.chat.tab_area = surface.tabs;
        self.chat.agent_tab_area = surface.agents;
        self.chat.acknowledge_selected_activity();
    }

    pub(crate) fn handle_chat_tab_drag(&mut self, mouse: MouseEvent) -> bool {
        if let Some(id) = self.chat.layout.dragging.clone() {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left) => {
                    self.drag_has_moved |= self.drag_start_pos != Some((mouse.column, mouse.row));
                    if self.drag_has_moved {
                        crate::mouse_drag::update_tab_drop_target(self, mouse.column, mouse.row);
                        if let Some(target) = self.chat.hits.iter().find_map(|h| {
                            if h.area.contains((mouse.column, mouse.row).into())
                                && let ChatAction::Switch(target) = &h.action
                            {
                                Some(target.clone())
                            } else {
                                None
                            }
                        }) && target != id
                            && let (Some(from), Some(to)) = (
                                self.chat.open_tabs.iter().position(|t| t == &id),
                                self.chat.open_tabs.iter().position(|t| t == &target),
                            )
                        {
                            let tab = self.chat.open_tabs.remove(from);
                            self.chat.open_tabs.insert(to, tab);
                        }
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    if self.drag_has_moved
                        && let Some((group, zone)) =
                            crate::mouse_drag::tab_drop_target_at(self, mouse.column, mouse.row)
                    {
                        self.place_chat_in_editor(id, group, zone);
                    }
                    self.chat.layout.dragging = None;
                    self.cancel_tab_drag();
                }
                MouseEventKind::Down(_) => {
                    self.chat.layout.dragging = None;
                    return false;
                }
                _ => {}
            }
            return true;
        }
        // Native chat tabs can return to the chat dock. All other destinations
        // continue through the existing tab move/split implementation.
        let Some(key) = self.dragged_chat_key() else {
            return false;
        };
        let dock = self
            .chat
            .layout
            .surfaces
            .iter()
            .any(|s| s.key.is_none() && s.area.contains((mouse.column, mouse.row).into()))
            || self.workbench_body_area.is_some_and(|a| {
                a.contains((mouse.column, mouse.row).into())
                    && mouse.column >= a.right().saturating_sub(1)
            });
        if dock && mouse.kind == MouseEventKind::Drag(MouseButton::Left) {
            self.drag_has_moved |= self.drag_start_pos != Some((mouse.column, mouse.row));
            self.tab_drop_zone = None;
            self.tab_drop_target_group = None;
            return true;
        }
        if dock && mouse.kind == MouseEventKind::Up(MouseButton::Left) && self.drag_has_moved {
            let view = self.chat.layout.editors.get(&key).cloned();
            if let Some(view) = view {
                self.chat.switch_connection(view.agent);
                self.move_chat_to_dock(view.session);
            }
            self.cancel_tab_drag();
            return true;
        }
        false
    }
    pub(crate) fn dragged_chat_key(&self) -> Option<u64> {
        let index = self.dragging_tab?;
        let source = self.dragging_tab_source_group.unwrap_or(self.active_group);
        let tab = if source == self.active_group {
            self.tabs.get(index)
        } else {
            self.groups.get(source)?.tabs.get(index)
        }?;
        if let TabKind::Chat(key) = tab.kind {
            Some(key)
        } else {
            None
        }
    }
    fn cancel_tab_drag(&mut self) {
        self.dragging_tab = None;
        self.dragging_tab_source_group = None;
        self.drag_start_pos = None;
        self.drag_has_moved = false;
        self.tab_drop_zone = None;
        self.tab_drop_target_group = None;
    }

    pub(crate) fn prune_editor_chats(&mut self) {
        let valid_keys: HashSet<_> = self
            .chat
            .layout
            .editors
            .iter()
            .filter_map(|(&key, view)| {
                self.chat
                    .agent_state(view.agent)
                    .is_some_and(|state| state.sessions.contains_key(&view.session))
                    .then_some(key)
            })
            .collect();
        self.chat
            .layout
            .editors
            .retain(|key, _| valid_keys.contains(key));
        let valid = &self.chat.layout.editors;
        fn prune(
            tabs: &mut Vec<crate::app::Tab>,
            active: &mut usize,
            mru: &mut Vec<usize>,
            valid: &HashMap<u64, Conversation>,
            open: &mut HashSet<u64>,
        ) {
            crate::tab_collection::retain(tabs, active, mru, |_, tab| {
                if let TabKind::Chat(key) = tab.kind {
                    valid.contains_key(&key) && { open.insert(key); true }
                } else { true }
            });
        }
        let mut open = HashSet::new();
        prune(&mut self.tabs, &mut self.active_tab, &mut self.tab_mru_order, valid, &mut open);
        for group in &mut self.groups {
            prune(&mut group.tabs, &mut group.active_tab, &mut group.tab_mru_order, valid, &mut open);
        }
        self.chat.layout.editors.retain(|key, _| open.contains(key));
        if self
            .chat
            .layout
            .active
            .is_some_and(|key| !open.contains(&key))
        {
            self.chat.layout.active = None;
        }
    }
}
