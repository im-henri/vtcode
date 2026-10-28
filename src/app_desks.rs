//! Desks own arrangements of views. Documents, workers and processes remain in
//! the workbench's existing registry, including while their desk is hidden.
use crate::app::{ActivePane, App, EditorGroup, SidebarView, SplitNode, Tab, TabKind};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct DeskLayout {
    pub tree: Option<SplitNode>,
    pub active_group: usize,
    pub explorer: bool,
    pub explorer_width: u16,
    pub sidebar: String,
    pub panel: bool,
    pub panel_tab: u8,
    pub panel_height: u16,
    pub terminal: usize,
    pub terminal_sidebar: bool,
    pub terminal_revealed: bool,
    pub chat: bool,
    pub chat_visible: bool,
    pub chat_width: u16,
    pub chat_agent: u64,
    pub chat_session: Option<String>,
    pub find: String,
    pub replace: String,
    pub search: String,
    pub search_replace: String,
    pub search_include: String,
    pub search_exclude: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct Desk {
    pub id: u64,
    pub name: String,
    pub layout: Option<DeskLayout>,
    #[serde(default)]
    pub closed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct TabGroup {
    pub id: u64,
    pub name: String,
    pub color: u8,
    pub collapsed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedDeskTool {
    pane: usize,
    index: usize,
    kind: String,
    path: Option<std::path::PathBuf>,
    pinned: bool,
    selected: bool,
    group: Option<u64>,
    scroll: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct SavedDesks {
    pub items: Vec<Desk>,
    pub active: u64,
    pub next_id: u64,
    pub owners: Vec<u64>,
    pub groups: Vec<TabGroup>,
    /// (pane, saved tab, named group). The active pane's files live in Session.tabs.
    pub members: Vec<(usize, usize, u64)>,
    pub documents: Vec<(usize, usize, u64)>,
    #[serde(default)]
    pub tools: Vec<SavedDeskTool>,
    #[serde(default)]
    pub closed: Vec<u64>,
}

/// View membership is stored separately from text, so historic/tool views and
/// two views of the same file can belong to different groups.
pub(crate) struct Desks {
    pub items: Vec<Desk>,
    pub active: u64,
    pub host: usize,
    pub previous: Option<u64>,
    pub closed: Vec<u64>,
    pub next_id: u64,
    pub groups: rustc_hash::FxHashMap<u64, TabGroup>,
    pub membership: rustc_hash::FxHashMap<u64, u64>,
    pub selected_tabs: std::collections::BTreeSet<u64>,
    pub navigation: rustc_hash::FxHashMap<u64, crate::app_navigation::NavigationHistory>,
    pub popup: crate::ui_desks::Popup,
    pub sync_stamp: Option<DocumentStamp>,
    pub group_drag: Option<(u64, usize, (u16, u16), bool)>,
}

pub(crate) struct DocumentStamp {
    view: u64,
    identity: u64,
    version: u64,
    document_revision: u64,
    history: (u64, usize, Option<u64>, usize, Option<u64>),
    rope: ropey::Rope,
    saved: Option<ropey::Rope>,
    modified: bool,
    encoding: &'static str,
    saved_encoding: &'static str,
    tab_count: usize,
    pane_count: usize,
}

impl DocumentStamp {
    pub(crate) fn unchanged(
        &self,
        editor: &crate::editor::Editor,
        tabs: usize,
        panes: usize,
    ) -> bool {
        self.view == editor.navigation_id
            && self.identity == editor.save_identity
            && self.version == editor.content_version
            && self.document_revision == editor.document_revision()
            && self.modified == editor.modified
            && self.encoding == editor.encoding
            && self.saved_encoding == editor.saved_encoding
            && self.history == editor.undo_history_key()
            && self.rope.is_instance(&editor.rope)
            && self.tab_count == tabs
            && self.pane_count == panes
            && match (&self.saved, &editor.saved_rope) {
                (Some(a), Some(b)) => a.is_instance(b),
                (None, None) => true,
                _ => false,
            }
    }
    pub(crate) fn capture(editor: &crate::editor::Editor, tabs: usize, panes: usize) -> Self {
        Self {
            view: editor.navigation_id,
            identity: editor.save_identity,
            version: editor.content_version,
            document_revision: editor.document_revision(),
            history: editor.undo_history_key(),
            rope: editor.rope.clone(),
            saved: editor.saved_rope.clone(),
            modified: editor.modified,
            encoding: editor.encoding,
            saved_encoding: editor.saved_encoding,
            tab_count: tabs,
            pane_count: panes,
        }
    }
}

impl Default for Desks {
    fn default() -> Self {
        Self {
            items: vec![Desk {
                id: 0,
                name: "Main".into(),
                layout: None,
                closed: false,
            }],
            active: 0,
            host: 0,
            previous: None,
            closed: Vec::new(),
            next_id: 1,
            groups: Default::default(),
            membership: Default::default(),
            selected_tabs: Default::default(),
            navigation: Default::default(),
            popup: Default::default(),
            sync_stamp: None,
            group_drag: None,
        }
    }
}

impl Desks {
    fn allocate_id(&mut self) -> u64 {
        loop {
            let id = self.next_id;
            self.next_id = self.next_id.wrapping_add(1).max(1);
            if !self.items.iter().any(|d| d.id == id) && !self.groups.contains_key(&id) {
                return id;
            }
        }
    }
}

impl App {
    pub(crate) fn save_desks(&self) -> SavedDesks {
        let mut saved = SavedDesks {
            items: self.desks.items.clone(),
            active: self.desks.active,
            next_id: self.desks.next_id,
            owners: self.groups.iter().map(|g| g.desk).collect(),
            groups: self.desks.groups.values().cloned().collect(),
            closed: self.desks.closed.clone(),
            ..Default::default()
        };
        saved.groups.sort_by_key(|g| g.id);
        if let Some(desk) = saved.items.iter_mut().find(|d| d.id == saved.active) {
            desk.layout = Some(self.capture_desk_layout());
        }
        let mut save = |pane: usize, tabs: &[Tab]| {
            let active = if pane == self.active_group {
                self.active_tab
            } else {
                self.groups[pane].active_tab
            };
            for (index, tab) in tabs.iter().enumerate() {
                let kind = match tab.kind {
                    TabKind::Settings => "settings",
                    TabKind::KeyboardShortcuts => "keys",
                    TabKind::MarkdownPreview => "markdown",
                    _ => continue,
                };
                saved.tools.push(SavedDeskTool {
                    pane,
                    index,
                    kind: kind.into(),
                    path: tab.editor.file_path.clone(),
                    pinned: tab.pinned,
                    selected: index == active,
                    group: self
                        .desks
                        .membership
                        .get(&tab.editor.navigation_id)
                        .copied(),
                    scroll: tab.editor.scroll_offset,
                });
            }
            for (index, tab) in tabs.iter().filter(|t| t.kind == TabKind::File).enumerate() {
                if let Some(&group) = self.desks.membership.get(&tab.editor.navigation_id) {
                    saved.members.push((pane, index, group));
                }
                saved
                    .documents
                    .push((pane, index, tab.editor.save_identity));
            }
        };
        save(self.active_group, &self.tabs);
        for (i, g) in self.groups.iter().enumerate() {
            if i != self.active_group {
                save(i, &g.tabs);
            }
        }
        saved
    }

    pub(crate) fn restore_desks(&mut self, saved: &SavedDesks) {
        if saved.items.is_empty() {
            return;
        }
        let current = self.capture_desk_layout();
        self.desks = Desks::default();
        let mut seen = rustc_hash::FxHashSet::default();
        self.desks.items = saved
            .items
            .iter()
            .filter(|d| d.id < u64::MAX && seen.insert(d.id))
            .cloned()
            .collect();
        if self.desks.items.is_empty() {
            self.desks = Desks::default();
            return;
        }
        for desk in &mut self.desks.items {
            desk.name = clean_name(&desk.name, "Desk");
        }
        self.desks.active = self
            .desks
            .items
            .iter()
            .find(|d| d.id == saved.active && !d.closed)
            .or_else(|| self.desks.items.iter().find(|d| !d.closed))
            .unwrap_or(&self.desks.items[0])
            .id;
        self.desks
            .items
            .iter_mut()
            .find(|d| d.id == self.desks.active)
            .unwrap()
            .closed = false;
        let mut closed = rustc_hash::FxHashSet::default();
        self.desks.closed = saved
            .closed
            .iter()
            .copied()
            .filter(|id| {
                closed.insert(*id) && self.desks.items.iter().any(|d| d.id == *id && d.closed)
            })
            .collect();
        self.desks.next_id = saved
            .next_id
            .min(u64::MAX - 1)
            .max(self.desks.items.iter().map(|d| d.id + 1).max().unwrap_or(1))
            .max(
                saved
                    .groups
                    .iter()
                    .filter(|g| g.id < u64::MAX - 1)
                    .map(|g| g.id + 1)
                    .max()
                    .unwrap_or(1),
            );
        self.desks.groups = saved
            .groups
            .iter()
            .filter(|g| g.id < u64::MAX - 1)
            .cloned()
            .map(|mut g| {
                g.name = clean_name(&g.name, "Group");
                g.color %= 6;
                (g.id, g)
            })
            .collect();
        self.materialize_desk_pane();
        for (i, g) in self.groups.iter_mut().enumerate() {
            g.desk = saved
                .owners
                .get(i)
                .copied()
                .filter(|id| seen.contains(id))
                .unwrap_or(self.desks.active);
        }
        // Session.tabs is the active pane's storage, even if a damaged desk
        // manifest disagrees about which pane was focused.
        self.groups[self.active_group].desk = self.desks.active;
        for desk in &mut self.desks.items {
            let mut panes: Vec<_> = self
                .groups
                .iter()
                .enumerate()
                .filter_map(|(i, g)| (g.desk == desk.id).then_some(i))
                .collect();
            if panes.is_empty() {
                panes.push(self.groups.len());
                let mut pane = EditorGroup::new();
                pane.desk = desk.id;
                self.groups.push(pane);
            }
            let layout = desk.layout.get_or_insert_with(|| current.clone());
            if !panes.contains(&layout.active_group) {
                layout.active_group = panes[0];
            }
            if desk.id == self.desks.active {
                layout.active_group = self.active_group;
            }
            layout.tree = validated_desk_tree(layout.tree.take(), &panes);
        }
        let mut documents: rustc_hash::FxHashMap<u64, crate::editor::Editor> =
            rustc_hash::FxHashMap::default();
        for &(pane, index, id) in &saved.documents {
            let tabs = if pane == self.active_group {
                Some(&mut self.tabs)
            } else {
                self.groups.get_mut(pane).map(|g| &mut g.tabs)
            };
            if let Some(tab) = tabs.and_then(|tabs| tabs.get_mut(index)) {
                if let Some(source) = documents.get(&id) {
                    tab.editor.copy_document_from(source);
                } else {
                    let mut handle = crate::editor::Editor::new();
                    handle.copy_document_from(&tab.editor);
                    documents.insert(id, handle);
                }
            }
        }
        for &(pane, index, id) in &saved.members {
            let tabs = if pane == self.active_group {
                Some(&self.tabs)
            } else {
                self.groups.get(pane).map(|g| &g.tabs)
            };
            if let Some(tab) = tabs.and_then(|tabs| tabs.get(index))
                && self.desks.groups.contains_key(&id)
            {
                self.desks.membership.insert(tab.editor.navigation_id, id);
            }
        }
        let mut selected_tools = Vec::new();
        for tool in &saved.tools {
            let kind = match tool.kind.as_str() {
                "settings" => TabKind::Settings,
                "keys" => TabKind::KeyboardShortcuts,
                "markdown" => TabKind::MarkdownPreview,
                _ => continue,
            };
            let mut editor = if kind == TabKind::MarkdownPreview {
                let Some(path) = tool.path.as_ref() else {
                    continue;
                };
                self.queue_background_file_load(path.clone(), true);
                crate::editor::Editor::new_loading(path, 0)
            } else {
                self.new_editor()
            };
            editor.file_path = tool.path.clone();
            editor.read_only = true;
            editor.scroll_offset = tool.scroll;
            let view = editor.navigation_id;
            let mut tab = Tab::new_file(editor);
            tab.kind = kind;
            tab.pinned = tool.pinned;
            if let Some(id) = tool.group.filter(|id| self.desks.groups.contains_key(id)) {
                self.desks.membership.insert(view, id);
            }
            let (tabs, active) = if tool.pane == self.active_group {
                (&mut self.tabs, &mut self.active_tab)
            } else if let Some(g) = self.groups.get_mut(tool.pane) {
                (&mut g.tabs, &mut g.active_tab)
            } else {
                continue;
            };
            let index = tool.index.min(tabs.len());
            tabs.insert(index, tab);
            if index <= *active && tabs.len() > 1 {
                *active += 1;
            }
            if tool.selected {
                selected_tools.push((tool.pane, view));
            }
        }
        for (pane, view) in selected_tools {
            let (tabs, active) = if pane == self.active_group {
                (&self.tabs, &mut self.active_tab)
            } else if let Some(g) = self.groups.get_mut(pane) {
                (&g.tabs, &mut g.active_tab)
            } else {
                continue;
            };
            if let Some(index) = tabs.iter().position(|t| t.editor.navigation_id == view) {
                *active = index;
            }
        }
        self.settings_editor_active = self
            .tabs
            .get(self.active_tab)
            .is_some_and(|t| t.kind == TabKind::Settings);
        if let Some(layout) = self
            .desks
            .items
            .iter()
            .find(|d| d.id == self.desks.active)
            .and_then(|d| d.layout.clone())
        {
            // The top-level session has already validated these visible layout
            // settings (including Zen mode); don't overwrite them a second time.
            let mut layout = layout;
            layout.explorer = current.explorer;
            layout.explorer_width = current.explorer_width;
            layout.sidebar = current.sidebar;
            layout.panel = current.panel;
            layout.panel_tab = current.panel_tab;
            layout.panel_height = current.panel_height;
            layout.search = current.search;
            layout.search_replace = current.search_replace;
            layout.search_include = current.search_include;
            layout.search_exclude = current.search_exclude;
            self.apply_desk_layout(layout);
        }
    }
    /// Worker results address a stable view, even when its desk is hidden.
    /// Publish the resulting document before a stale peer can become active.
    pub(crate) fn sync_document_view(&mut self, view: u64) {
        self.desks.sync_stamp = None;
        if let Some(index) = self
            .tabs
            .iter()
            .position(|t| t.editor.navigation_id == view)
        {
            let active = self.active_tab;
            self.active_tab = index;
            self.sync_document_views(false);
            self.active_tab = active;
        } else if let Some((pane, index)) = self.groups.iter().enumerate().find_map(|(i, g)| {
            g.tabs
                .iter()
                .position(|t| t.editor.navigation_id == view)
                .map(|j| (i, j))
        }) {
            let active = self.active_tab;
            // The active pane's slot is empty; use it to preserve the live tabs
            // while projecting the worker's source, without changing UI focus.
            let current = self.active_group;
            self.groups[current].tabs = std::mem::take(&mut self.tabs);
            self.tabs = std::mem::take(&mut self.groups[pane].tabs);
            self.active_group = pane;
            self.active_tab = index;
            self.sync_document_views(false);
            self.groups[pane].tabs = std::mem::take(&mut self.tabs);
            self.tabs = std::mem::take(&mut self.groups[current].tabs);
            self.active_group = current;
            self.active_tab = active;
        }
    }
    /// Compact empty panes across every desk with one index remapping. Keep one
    /// empty pane per desk so an empty arrangement is still a valid destination.
    pub(crate) fn compact_desk_groups(&mut self) {
        let mut keep = rustc_hash::FxHashSet::default();
        for desk in &self.desks.items {
            let panes: Vec<_> = self
                .groups
                .iter()
                .enumerate()
                .filter(|(_, g)| g.desk == desk.id)
                .map(|(i, _)| i)
                .collect();
            let populated: Vec<_> = panes
                .iter()
                .copied()
                .filter(|&i| !self.groups[i].tabs.is_empty())
                .collect();
            if populated.is_empty() {
                if let Some(&i) = panes.first() {
                    keep.insert(i);
                }
            } else {
                keep.extend(populated);
            }
        }
        let remap: rustc_hash::FxHashMap<_, _> = (0..self.groups.len())
            .filter(|i| keep.contains(i))
            .enumerate()
            .map(|(new, old)| (old, new))
            .collect();
        let repair = |tree: &mut Option<SplitNode>, active: &mut usize, desk: u64| {
            let fallback = self
                .groups
                .iter()
                .enumerate()
                .find(|(i, g)| g.desk == desk && keep.contains(i))
                .map(|(i, _)| i)
                .unwrap_or(0);
            if let Some(tree) = tree.as_mut() {
                for leaf in tree.leaves() {
                    if !keep.contains(&leaf) {
                        tree.remove_leaf(leaf);
                    }
                }
                tree.remap_indices(&remap);
                tree.simplify();
            }
            *active = remap
                .get(active)
                .or_else(|| remap.get(&fallback))
                .copied()
                .unwrap_or(0);
            if tree.as_ref().is_some_and(|t| t.leaves().len() <= 1) {
                *tree = None;
            }
        };
        repair(
            &mut self.split_tree,
            &mut self.active_group,
            self.desks.active,
        );
        for desk in &mut self.desks.items {
            if let Some(layout) = &mut desk.layout {
                repair(&mut layout.tree, &mut layout.active_group, desk.id);
            }
        }
        self.navigation.remap_groups(&remap);
        for history in self.desks.navigation.values_mut() {
            history.remap_groups(&remap);
        }
        let mut index = 0;
        self.groups.retain(|_| {
            let result = keep.contains(&index);
            index += 1;
            result
        });
    }

    pub(crate) fn merge_desk_panes(&mut self) {
        let panes = self.desk_panes();
        if panes.len() < 2 {
            return;
        }
        self.save_active_group();
        for i in panes {
            if i != self.active_group {
                let other = std::mem::take(&mut self.groups[i].tabs);
                self.groups[self.active_group].tabs.extend(other);
            }
        }
        self.split_tree = None;
        self.compact_desk_groups();
        self.load_group(self.active_group);
        crate::tab_collection::complete_mru(
            &mut self.tab_mru_order,
            self.active_tab,
            self.tabs.len(),
        );
    }
    pub(crate) fn group_in_current_desk(&self, index: usize) -> bool {
        self.groups
            .get(index)
            .is_none_or(|group| group.desk == self.desks.active)
    }

    pub(crate) fn desk_panes(&self) -> Vec<usize> {
        if self.groups.is_empty() {
            return vec![self.active_group];
        }
        if let Some(tree) = &self.split_tree {
            return tree.leaves();
        }
        vec![self.active_group]
    }

    pub(crate) fn desk_count(&self) -> usize {
        self.desks.items.iter().filter(|desk| !desk.closed).count()
    }

    pub(crate) fn current_desk_is_empty(&self) -> bool {
        self.tabs.is_empty()
            && self.groups.iter().enumerate().all(|(index, pane)| {
                index == self.active_group
                    || pane.desk != self.desks.active
                    || pane.tabs.is_empty()
            })
    }

    pub(crate) fn current_desk_name(&self) -> &str {
        self.desks
            .items
            .iter()
            .find(|desk| desk.id == self.desks.active)
            .map_or("Main", |desk| desk.name.as_str())
    }

    pub(crate) fn capture_desk_layout(&self) -> DeskLayout {
        DeskLayout {
            tree: self.split_tree.clone(),
            active_group: self.active_group,
            explorer: self.show_explorer,
            explorer_width: self.explorer_width,
            sidebar: format!("{:?}", self.sidebar_view),
            panel: self.show_panel,
            panel_tab: self.panel_tab,
            panel_height: self.panel_height,
            terminal: self.active_terminal,
            terminal_sidebar: self.show_terminal_sidebar,
            terminal_revealed: self.terminal_sidebar_revealed,
            chat: self.chat.layout.dock_visible,
            chat_visible: self.chat.visible,
            chat_width: self.chat.width,
            chat_agent: self.chat.layout.dock_agent,
            chat_session: self
                .chat
                .agent_state(self.chat.layout.dock_agent)
                .and_then(|agent| agent.dock_selected.as_ref().or(agent.selected.as_ref()))
                .map(ToString::to_string),
            find: self.search_query.clone(),
            replace: self.replace_query.clone(),
            search: self.global_search_query.clone(),
            search_replace: self.global_search_replace_query.clone(),
            search_include: self.search_include_pattern.clone(),
            search_exclude: self.search_exclude_pattern.clone(),
        }
    }

    fn apply_desk_layout(&mut self, layout: DeskLayout) {
        crate::ui_panels::invalidate_layout(self);
        self.panel_chrome.maximized = false;
        self.split_tree = layout.tree;
        self.active_group = layout.active_group;
        self.show_explorer = layout.explorer && !self.zen_mode;
        self.explorer_width = layout.explorer_width.max(15);
        self.sidebar_view = match layout.sidebar.as_str() {
            "Search" => SidebarView::Search,
            "SourceControl" => SidebarView::SourceControl,
            "Settings" => SidebarView::Settings,
            "Extensions" => SidebarView::Extensions,
            "Outline" => SidebarView::Outline,
            _ => SidebarView::Explorer,
        };
        self.show_panel = layout.panel && !self.zen_mode;
        self.panel_tab = Self::normalize_panel_tab(layout.panel_tab);
        self.panel_height = layout.panel_height.max(3);
        self.active_terminal = layout.terminal.min(self.terminals.len().saturating_sub(1));
        self.show_terminal_sidebar = layout.terminal_sidebar;
        self.terminal_sidebar_revealed = layout.terminal_revealed;
        self.chat.layout.dock_visible = layout.chat;
        self.chat.visible = layout.chat_visible;
        self.chat.width = layout.chat_width;
        if self.chat.switch_connection(layout.chat_agent) {
            self.chat.layout.dock_agent = layout.chat_agent;
            let session = layout
                .chat_session
                .map(vtcode::acp::protocol::SessionId::new)
                .filter(|id| self.chat.sessions.contains_key(id));
            self.chat.dock_selected = session.clone();
            self.chat.select_view(session);
        }
        self.search_query = layout.find;
        self.search_history.edited();
        self.search_cursor = self.search_query.len();
        self.search_selection_anchor = None;
        self.replace_query = layout.replace;
        self.replace_cursor = self.replace_query.len();
        self.replace_selection_anchor = None;
        self.global_search_query = layout.search;
        for field in 0..4 { self.global_search_field_history_mut(field).edited(); }
        self.global_search_replace_query = layout.search_replace;
        self.search_include_pattern = layout.search_include;
        self.search_exclude_pattern = layout.search_exclude;
        self.global_search_field_cursors = [
            self.global_search_query.len(),
            self.global_search_replace_query.len(),
            self.search_include_pattern.len(),
            self.search_exclude_pattern.len(),
        ];
        self.global_search_field_selection_anchors = [None; 4];
        self.global_search_active = self.show_explorer && self.sidebar_view == SidebarView::Search;
        self.sync_activity_bar_selection();
    }

    pub(crate) fn materialize_desk_pane(&mut self) {
        if self.groups.is_empty() {
            let mut pane = EditorGroup::new();
            pane.desk = self.desks.active;
            self.groups.push(pane);
            self.active_group = 0;
        }
    }

    /// Switch without opening files, saving text, or restarting language servers.
    pub(crate) fn switch_desk(&mut self, id: u64) {
        if id == self.desks.active {
            self.desks.popup = Default::default();
            return;
        }
        let Some(layout) = self
            .desks
            .items
            .iter()
            .find(|d| d.id == id && !d.closed)
            .and_then(|d| d.layout.clone())
        else {
            return;
        };
        self.cancel_remote_location();
        self.push_search_history();
        self.push_global_search_history();
        self.materialize_desk_pane();
        if let Some(editor) = self.editor_mut() {
            editor.break_undo_group();
        }
        self.sync_duplicate_file_tabs();
        self.suspend_diff_tab();
        self.clear_transient_editor_state();
        self.clear_drag_states();
        self.desks.group_drag = None;
        self.dismiss_vtcomplete();
        self.clear_tab_tooltip();
        self.search_input_focused = false;
        self.global_search_active = false;
        self.tab_context_active = false;
        self.context_menu_active = false;
        self.explorer_context_active = false;
        self.sc_context_active = false;
        self.breadcrumb_dropdown_active = false;
        self.quick_open_active = false;
        self.palette.active = false;
        self.invalidate_visible_highlights();
        let outgoing = self.capture_desk_layout();
        let old = self.desks.active;
        if let Some(desk) = self.desks.items.iter_mut().find(|desk| desk.id == old) {
            desk.layout = Some(outgoing);
        }
        self.desks
            .navigation
            .insert(old, std::mem::take(&mut self.navigation));
        self.save_active_group();
        self.desks.active = id;
        self.desks.previous = Some(old);
        self.desks.selected_tabs.clear();
        self.desks.popup = Default::default();
        self.apply_desk_layout(layout);
        self.load_group(self.active_group);
        self.navigation = self.desks.navigation.remove(&id).unwrap_or_default();
        self.active_pane = ActivePane::Editor;
        self.settings_editor_active = self
            .tabs
            .get(self.active_tab)
            .is_some_and(|t| t.kind == TabKind::Settings);
        self.sync_diff_tab();
        self.tab_scroll_manual_anchor = None;
        self.split_dividers.clear();
        *self.tab_animation_cache.borrow_mut() = None;
        // Hidden panes must never retain mouse hit targets from their last frame.
        for group in &mut self.groups {
            group.code_area = None;
            group.tab_bar_area = None;
            group.editor_area = None;
            group.minimap_area = None;
            group.scrollbar_area = None;
            group.h_scrollbar_area = None;
            group.gutter_area = None;
            group.breadcrumb_area = None;
        }
        self.code_area = None;
        self.tab_bar_area = None;
        self.refresh_git_line_status_async();
    }

    pub(crate) fn new_desk(&mut self, name: &str, duplicate: bool) -> u64 {
        self.materialize_desk_pane();
        self.sync_duplicate_file_tabs();
        let id = self.desks.allocate_id();
        let name = clean_name(name, &format!("Desk {}", self.desk_count() + 1));
        let mut layout = self.capture_desk_layout();
        if duplicate {
            self.save_active_group();
            let panes = self.desk_panes();
            let mut remap = rustc_hash::FxHashMap::default();
            let mut group_remap = rustc_hash::FxHashMap::default();
            for source in panes {
                let mut pane = EditorGroup::new();
                pane.desk = id;
                pane.active_tab = self.groups[source].active_tab;
                pane.tab_scroll_offset = self.groups[source].tab_scroll_offset;
                pane.tab_mru_order = self.groups[source].tab_mru_order.clone();
                for tab in &self.groups[source].tabs {
                    let clone = self.clone_desk_tab(tab);
                    if let Some(&old_group) = self.desks.membership.get(&tab.editor.navigation_id) {
                        let new_group = *group_remap
                            .entry(old_group)
                            .or_insert_with(|| self.desks.allocate_id());
                        if let Some(mut group) = self.desks.groups.get(&old_group).cloned() {
                            group.id = new_group;
                            self.desks.groups.insert(new_group, group);
                            self.desks
                                .membership
                                .insert(clone.editor.navigation_id, new_group);
                        }
                    }
                    pane.tabs.push(clone);
                }
                remap.insert(source, self.groups.len());
                self.groups.push(pane);
            }
            if let Some(tree) = &mut layout.tree {
                tree.remap_indices(&remap);
            }
            layout.active_group = remap[&layout.active_group];
            self.load_group(self.active_group);
        } else {
            let mut pane = EditorGroup::new();
            pane.desk = id;
            layout.active_group = self.groups.len();
            self.groups.push(pane);
            layout.tree = None;
            layout.explorer = true;
            layout.sidebar = "Explorer".into();
            layout.panel = false;
            layout.terminal_sidebar = false;
            layout.terminal_revealed = false;
            layout.chat = false;
            layout.chat_visible = false;
            layout.find.clear();
            layout.replace.clear();
            layout.search.clear();
            layout.search_replace.clear();
            layout.search_include.clear();
            layout.search_exclude.clear();
        }
        self.desks.items.push(Desk {
            id,
            name,
            layout: Some(layout),
            closed: false,
        });
        self.switch_desk(id);
        id
    }

    pub(crate) fn clone_desk_tab(&self, tab: &Tab) -> Tab {
        let source = &tab.editor;
        let mut editor = self.new_editor();
        editor.copy_document_from(source);
        editor.cursor_line = source.cursor_line;
        editor.cursor_col = source.cursor_col;
        editor.selection_anchor = source.selection_anchor;
        editor.extra_cursors = source.extra_cursors.clone();
        editor.extra_cursor_anchors = source.extra_cursor_anchors.clone();
        editor.scroll_offset = source.scroll_offset;
        editor.horizontal_scroll = source.horizontal_scroll;
        editor.wrap_scroll_offset = source.wrap_scroll_offset;
        editor.word_wrap = source.word_wrap;
        editor.word_wrap_column = source.word_wrap_column;
        editor.whitespace_mode = source.whitespace_mode;
        editor.typewriter_mode = source.typewriter_mode;
        editor.cursor_surrounding_lines = source.cursor_surrounding_lines;
        editor.folded_ranges = source.folded_ranges.clone();
        editor.bookmarks = source.bookmarks.clone();
        editor.pending_session_restore = source.pending_session_restore.clone();
        editor.bg_loading = source.bg_loading;
        Tab {
            editor,
            pinned: tab.pinned,
            preview: false,
            kind: tab.kind,
            extension_view: tab.extension_view.clone(),
            file_search: Default::default(),
            terminal_idx: tab.terminal_idx,
        }
    }

    /// Closing an arrangement retains its views for Reopen Closed Desk. This
    /// deliberately does not discard a document or terminate a running process.
    pub(crate) fn close_desk(&mut self) {
        let old = self.desks.active;
        let target = self
            .desks
            .items
            .iter()
            .find(|desk| desk.id != old && !desk.closed)
            .map(|d| d.id);
        if let Some(target) = target {
            self.switch_desk(target);
        } else {
            self.new_desk("Main", false);
        }
        if let Some(desk) = self.desks.items.iter_mut().find(|d| d.id == old) {
            desk.closed = true;
        }
        self.desks.closed.retain(|&id| id != old);
        self.desks.closed.push(old);
        self.desks.popup = Default::default();
    }

    pub(crate) fn reopen_desk(&mut self) {
        let id = self.desks.closed.pop();
        if let Some(desk) = self
            .desks
            .items
            .iter_mut()
            .find(|d| Some(d.id) == id && d.closed)
        {
            desk.closed = false;
            let id = desk.id;
            self.switch_desk(id);
        }
    }

    pub(crate) fn tab_group(&self, tab: &Tab) -> Option<&TabGroup> {
        self.desks
            .membership
            .get(&tab.editor.navigation_id)
            .and_then(|id| self.desks.groups.get(id))
    }

    pub(crate) fn group_selected_tabs(&mut self, name: &str, existing: Option<u64>) {
        if self.tabs.is_empty() {
            return;
        }
        let id = existing.unwrap_or_else(|| {
            let id = self.desks.allocate_id();
            self.desks.groups.insert(
                id,
                TabGroup {
                    id,
                    name: clean_name(name, "Group"),
                    color: (id % 6) as u8,
                    collapsed: false,
                },
            );
            id
        });
        let active = self
            .tabs
            .get(self.active_tab)
            .map(|tab| tab.editor.navigation_id);
        let selected = &self.desks.selected_tabs;
        for tab in &mut self.tabs {
            if selected.contains(&tab.editor.navigation_id)
                || (selected.is_empty() && Some(tab.editor.navigation_id) == active)
            {
                tab.preview = false;
                self.desks.membership.insert(tab.editor.navigation_id, id);
            }
        }
        self.normalize_tab_groups();
        self.desks.selected_tabs.clear();
        self.desks.popup = Default::default();
        self.ensure_active_tab_visible();
    }

    /// Keep groups contiguous while preserving each group's internal order and
    /// tracking selection by stable view identity rather than a moving index.
    pub(crate) fn normalize_tab_groups(&mut self) {
        let active = self
            .tabs
            .get(self.active_tab)
            .map(|tab| tab.editor.navigation_id);
        let mru: Vec<_> = self
            .tab_mru_order
            .iter()
            .filter_map(|&i| self.tabs.get(i).map(|t| t.editor.navigation_id))
            .collect();
        let mut order = rustc_hash::FxHashMap::default();
        let mut next = 0;
        for tab in &self.tabs {
            let key = self
                .desks
                .membership
                .get(&tab.editor.navigation_id)
                .map(|id| (true, *id))
                .unwrap_or((false, tab.editor.navigation_id));
            order.entry(key).or_insert_with(|| {
                let n = next;
                next += 1;
                n
            });
        }
        self.tabs.sort_by_key(|tab| {
            let key = self
                .desks
                .membership
                .get(&tab.editor.navigation_id)
                .map(|id| (true, *id))
                .unwrap_or((false, tab.editor.navigation_id));
            order[&key]
        });
        self.active_tab = self
            .tabs
            .iter()
            .position(|tab| Some(tab.editor.navigation_id) == active)
            .unwrap_or(0);
        let index: rustc_hash::FxHashMap<_, _> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| (t.editor.navigation_id, i))
            .collect();
        self.tab_mru_order = mru.iter().filter_map(|id| index.get(id).copied()).collect();
    }

    pub(crate) fn move_tabs_to_desk(&mut self, target: u64, copy: bool, ids: &[u64]) {
        self.sync_duplicate_file_tabs();
        let Some(pane) = self
            .desks
            .items
            .iter()
            .find(|d| d.id == target && !d.closed)
            .and_then(|d| d.layout.as_ref())
            .map(|l| l.active_group)
        else {
            return;
        };
        if target == self.desks.active || pane >= self.groups.len() {
            return;
        }
        let mut moving = Vec::new();
        if copy {
            for tab in self
                .tabs
                .iter()
                .filter(|t| ids.contains(&t.editor.navigation_id))
            {
                moving.push(self.clone_desk_tab(tab));
            }
        } else {
            crate::tab_collection::retain(
                &mut self.tabs,
                &mut self.active_tab,
                &mut self.tab_mru_order,
                |_, tab| {
                    if ids.contains(&tab.editor.navigation_id) {
                        moving.push(std::mem::replace(
                            tab,
                            Tab::new_file(crate::editor::Editor::new()),
                        ));
                        false
                    } else {
                        true
                    }
                },
            );
        }
        for tab in &mut moving {
            tab.preview = false;
            if let Some(&group) = self.desks.membership.get(&tab.editor.navigation_id) {
                if self
                    .tabs
                    .iter()
                    .any(|t| self.desks.membership.get(&t.editor.navigation_id) == Some(&group))
                {
                    self.desks.membership.remove(&tab.editor.navigation_id);
                }
            }
        }
        self.groups[pane].tabs.extend(moving);
        let group = &mut self.groups[pane];
        crate::tab_collection::complete_mru(
            &mut group.tab_mru_order,
            group.active_tab,
            group.tabs.len(),
        );
        self.desks.selected_tabs.clear();
        self.desks.popup = Default::default();
        self.clear_transient_editor_state();
        self.sync_diff_tab();
        self.close_empty_groups();
    }

    pub(crate) fn move_named_group(&mut self, id: u64, target: usize, before: Option<u64>) {
        self.materialize_desk_pane();
        let source = self
            .groups
            .iter()
            .enumerate()
            .find_map(|(i, g)| {
                let tabs = if i == self.active_group {
                    &self.tabs
                } else {
                    &g.tabs
                };
                tabs.iter()
                    .any(|t| self.desks.membership.get(&t.editor.navigation_id) == Some(&id))
                    .then_some(i)
            })
            .unwrap_or(self.active_group);
        if target >= self.groups.len() || !self.group_in_current_desk(target) {
            return;
        }
        if source != self.active_group {
            self.switch_group(source);
        }
        let active = self
            .tabs
            .get(self.active_tab)
            .map(|t| t.editor.navigation_id);
        let members: Vec<_> = self
            .tabs
            .iter()
            .filter(|t| self.desks.membership.get(&t.editor.navigation_id) == Some(&id))
            .map(|t| t.editor.navigation_id)
            .collect();
        if members.is_empty() || before.is_some_and(|view| members.contains(&view)) {
            return;
        }
        let mru: Vec<_> = self
            .tab_mru_order
            .iter()
            .filter_map(|&i| self.tabs.get(i).map(|t| t.editor.navigation_id))
            .collect();
        let follows_active = active.is_some_and(|view| members.contains(&view));
        let mut moving = Vec::new();
        crate::tab_collection::retain(
            &mut self.tabs,
            &mut self.active_tab,
            &mut self.tab_mru_order,
            |_, t| {
                if members.contains(&t.editor.navigation_id) {
                    moving.push(std::mem::replace(
                        t,
                        Tab::new_file(crate::editor::Editor::new()),
                    ));
                    false
                } else {
                    true
                }
            },
        );
        if target == self.active_group {
            let at = before
                .and_then(|id| self.tabs.iter().position(|t| t.editor.navigation_id == id))
                .unwrap_or(self.tabs.len());
            self.tabs.splice(at..at, moving);
            self.active_tab = active
                .and_then(|id| self.tabs.iter().position(|t| t.editor.navigation_id == id))
                .unwrap_or(self.active_tab);
            let indices: rustc_hash::FxHashMap<_, _> = self
                .tabs
                .iter()
                .enumerate()
                .map(|(i, t)| (t.editor.navigation_id, i))
                .collect();
            self.tab_mru_order = mru
                .iter()
                .filter_map(|id| indices.get(id).copied())
                .collect();
            self.normalize_tab_groups();
        } else if let Some(group) = self.groups.get_mut(target) {
            let selected = if follows_active {
                active
            } else {
                group
                    .tabs
                    .get(group.active_tab)
                    .map(|t| t.editor.navigation_id)
            };
            let old_mru: Vec<_> = group
                .tab_mru_order
                .iter()
                .filter_map(|&i| group.tabs.get(i).map(|t| t.editor.navigation_id))
                .collect();
            let mut at = before
                .and_then(|id| group.tabs.iter().position(|t| t.editor.navigation_id == id))
                .unwrap_or(group.tabs.len());
            if let Some(target_group) = group
                .tabs
                .get(at)
                .and_then(|t| self.desks.membership.get(&t.editor.navigation_id))
            {
                at = group
                    .tabs
                    .iter()
                    .position(|t| {
                        self.desks.membership.get(&t.editor.navigation_id) == Some(target_group)
                    })
                    .unwrap_or(at);
            }
            group.tabs.splice(at..at, moving);
            let indices: rustc_hash::FxHashMap<_, _> = group
                .tabs
                .iter()
                .enumerate()
                .map(|(i, t)| (t.editor.navigation_id, i))
                .collect();
            group.active_tab = selected
                .and_then(|id| indices.get(&id).copied())
                .unwrap_or(0);
            group.tab_mru_order = old_mru
                .iter()
                .filter_map(|id| indices.get(id).copied())
                .collect();
            crate::tab_collection::complete_mru(
                &mut group.tab_mru_order,
                group.active_tab,
                group.tabs.len(),
            );
            for view in members {
                self.navigation.move_editor(view, target);
            }
            self.close_empty_groups();
            if follows_active {
                let destination = self.groups.iter().enumerate().find_map(|(i, g)| {
                    g.tabs
                        .iter()
                        .any(|t| Some(t.editor.navigation_id) == active)
                        .then_some(i)
                });
                if let Some(pane) = destination {
                    self.switch_group(pane);
                }
            }
        }
        self.desks.group_drag = None;
        self.desks.popup = Default::default();
    }
}

pub(crate) fn clean_name(name: &str, fallback: &str) -> String {
    let value: String = name
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    if value.is_empty() {
        fallback.into()
    } else {
        value
    }
}

fn validated_desk_tree(tree: Option<SplitNode>, panes: &[usize]) -> Option<SplitNode> {
    if panes.len() < 2 {
        return None;
    }
    fn valid(tree: &SplitNode) -> bool {
        match tree {
            SplitNode::Leaf(_) => true,
            SplitNode::Split {
                children, ratios, ..
            } => {
                children.len() >= 2
                    && children.len() == ratios.len()
                    && ratios.iter().all(|v| v.is_finite() && *v > 0.0)
                    && ratios.iter().sum::<f32>().is_finite()
                    && children.iter().all(valid)
            }
        }
    }
    tree.filter(|tree| {
        let mut leaves = tree.leaves();
        leaves.sort_unstable();
        leaves == panes && valid(tree)
    })
    .or_else(|| {
        Some(SplitNode::Split {
            direction: crate::app::SplitDirection::Horizontal,
            children: panes.iter().copied().map(SplitNode::Leaf).collect(),
            ratios: vec![1.0 / panes.len() as f32; panes.len()],
        })
    })
}

#[cfg(test)]
#[path = "app_desks_tests.rs"]
mod tests;
