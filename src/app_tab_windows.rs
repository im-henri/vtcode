//! Detached windows own arrangements of editor groups in the same workbench.
use crate::{
    app::{ActivePane, App, EditorGroup},
    app_desks::Desk,
    tab_windows::{MAIN_WINDOW, Request, WindowId},
};

impl App {
    pub(crate) fn detached_window_id(&self) -> Option<WindowId> {
        self.desks
            .items
            .iter()
            .find(|desk| desk.id == self.desks.active)?
            .window
    }

    pub(crate) fn has_detached_windows(&self) -> bool {
        self.desks.items.iter().any(|desk| desk.window.is_some())
    }

    pub(crate) fn group_in_detached_window(&self, group: usize) -> bool {
        self.groups.get(group).is_some_and(|pane| {
            self.desks
                .items
                .iter()
                .any(|desk| desk.id == pane.desk && desk.window.is_some())
        })
    }

    pub(crate) fn window_desk(&self, window: WindowId) -> Option<&Desk> {
        if window == MAIN_WINDOW {
            let desk = if self.detached_window_id().is_some() {
                self.desks.main_window_desk
            } else {
                self.desks.active
            };
            self.desks
                .items
                .iter()
                .find(|item| item.id == desk && !item.closed && item.window.is_none())
                .or_else(|| {
                    self.desks
                        .items
                        .iter()
                        .find(|item| !item.closed && item.window.is_none())
                })
        } else {
            self.desks
                .items
                .iter()
                .find(|desk| desk.window == Some(window))
        }
    }

    pub(crate) fn activate_window(&mut self, window: WindowId) -> bool {
        let Some(desk) = self.window_desk(window).map(|desk| desk.id) else {
            return false;
        };
        if desk != self.desks.active {
            self.forward_terminal_focus(false);
            self.switch_desk(desk);
            self.terminal_panel_focused = false;
            self.menu_active = false;
            self.menu_open = false;
            self.chord_pending = false;
            self.shortcut_chord_prefix = None;
            self.wheel_scroll = None;
            self.clear_drag_states();
        }
        true
    }

    pub(crate) fn open_detached_window(&mut self, window: WindowId, view: u64) -> bool {
        if self.tab_location(view).is_none() || self.window_desk(window).is_some() {
            return false;
        }
        let was_displayed = self.workspace_is_displayed();
        self.materialize_desk_pane();
        let id = self.desks.allocate_id();
        let mut pane = EditorGroup::new();
        pane.desk = id;
        let mut layout = self.capture_desk_layout();
        layout.tree = None;
        layout.active_group = self.groups.len();
        layout.find.clear();
        layout.replace.clear();
        layout.search.clear();
        layout.search_replace.clear();
        layout.search_include.clear();
        layout.search_exclude.clear();
        self.groups.push(pane);
        self.desks.items.push(Desk {
            id,
            name: format!("Detached window {window}"),
            layout: Some(layout),
            closed: false,
            window: Some(window),
        });
        if self.move_tab_to_window(view, window) {
            if !was_displayed {
                self.resume_workspace_observation();
            }
            true
        } else {
            self.return_detached_tabs(window);
            false
        }
    }

    pub(crate) fn tab_window(&self, view: u64) -> Option<WindowId> {
        let (group, _) = self.tab_location(view)?;
        let desk = self
            .groups
            .get(group)
            .map_or(self.desks.active, |group| group.desk);
        Some(
            self.desks
                .items
                .iter()
                .find(|item| item.id == desk)
                .and_then(|desk| desk.window)
                .unwrap_or(MAIN_WINDOW),
        )
    }

    pub(crate) fn collect_populated_windows(
        &self,
        windows: &mut rustc_hash::FxHashSet<WindowId>,
        desks: &mut rustc_hash::FxHashSet<u64>,
    ) {
        desks.clear();
        if !self.tabs.is_empty() {
            desks.insert(self.desks.active);
        }
        desks.extend(self.groups.iter().enumerate().filter_map(|(index, group)| {
            (index != self.active_group && !group.tabs.is_empty()).then_some(group.desk)
        }));
        windows.extend(self.desks.items.iter().filter_map(|desk| {
            desk.window.filter(|_| desks.contains(&desk.id))
        }));
    }

    pub(crate) fn window_name(&self, window: WindowId) -> String {
        if window == MAIN_WINDOW {
            return String::from("Main Window");
        }
        let name = self
            .window_desk(window)
            .and_then(|desk| {
                if desk.id == self.desks.active {
                    self.tabs.get(self.active_tab)
                } else {
                    let pane = self.groups.get(desk.layout.as_ref()?.active_group)?;
                    pane.tabs.get(pane.active_tab)
                }
            })
            .map(|tab| tab.editor.display_name());
        match name {
            Some(name) => format!("Window {window} · {name}"),
            None => format!("Window {window}"),
        }
    }

    pub(crate) fn move_tab_to_window(&mut self, view: u64, window: WindowId) -> bool {
        let Some((source, index)) = self.tab_location(view) else {
            return false;
        };
        let Some(desk) = self.window_desk(window) else {
            return false;
        };
        let target = if desk.id == self.desks.active {
            self.active_group
        } else if let Some(layout) = &desk.layout {
            layout.active_group
        } else {
            return false;
        };
        if self.tab_window(view) == Some(window) || self.groups.get(target).is_none() {
            return false;
        }

        self.sync_duplicate_file_tabs();
        self.suspend_diff_tab();
        if !self.move_tab_between_groups(source, index, target) {
            self.sync_diff_tab();
            return false;
        }
        let Some((group, index)) = self.tab_location(view) else {
            return false;
        };
        if group == self.active_group {
            self.active_tab = index;
            self.active_pane = ActivePane::Editor;
            self.touch_tab_mru(index);
            self.tab_scroll_manual_anchor = None;
            self.ensure_active_tab_visible();
        } else {
            let pane = &mut self.groups[group];
            pane.active_tab = index;
            pane.tab_strip_scroll = None;
            pane.tab_mru_order.retain(|&tab| tab != index);
            pane.tab_mru_order.insert(0, index);
        }
        self.desks.selected_tabs.remove(&view);
        self.sync_diff_tab();
        true
    }

    /// Retire a disconnected terminal without closing any of its tabs or
    /// releasing their document, extension, agent or PTY handles.
    pub(crate) fn return_detached_tabs(&mut self, window: WindowId) -> bool {
        let Some(detached) = self
            .window_desk(window)
            .filter(|desk| desk.window.is_some())
        else {
            return false;
        };
        let id = detached.id;
        if self.detached_window_id() == Some(window) {
            crate::ui_modal::dismiss_tab_dialog(self);
            self.activate_window(MAIN_WINDOW);
        }
        let Some(home) = self.window_desk(MAIN_WINDOW) else {
            return false;
        };
        let target = if home.id == self.desks.active {
            self.active_group
        } else if let Some(layout) = &home.layout {
            layout.active_group
        } else {
            return false;
        };
        let mut returned = Vec::new();
        for pane in self.groups.iter_mut().filter(|pane| pane.desk == id) {
            returned.append(&mut pane.tabs);
        }
        if target == self.active_group {
            self.tabs.append(&mut returned);
            crate::tab_collection::complete_mru(
                &mut self.tab_mru_order,
                self.active_tab,
                self.tabs.len(),
            );
        } else {
            let pane = &mut self.groups[target];
            pane.tabs.append(&mut returned);
            crate::tab_collection::complete_mru(
                &mut pane.tab_mru_order,
                pane.active_tab,
                pane.tabs.len(),
            );
        }
        self.desks.items.retain(|desk| desk.id != id);
        self.desks.navigation.remove(&id);
        self.save_active_group();
        self.compact_desk_groups();
        self.load_group(self.active_group);
        self.sync_diff_tab();
        if !self.workspace_is_displayed() {
            self.suspend_workspace_observation();
        }
        true
    }

    pub(crate) fn return_all_detached_tabs(&mut self) {
        while let Some(window) = self.desks.items.iter().find_map(|desk| desk.window) {
            if !self.return_detached_tabs(window) {
                break;
            }
        }
    }

    pub(crate) fn request_tab_window(&mut self, index: usize, choose: bool) {
        let Some(view) = self.tabs.get(index).map(|tab| tab.editor.navigation_id) else {
            return;
        };
        if choose {
            crate::ui_desks::open(self, crate::ui_desks::Mode::Window { view });
        } else {
            self.tab_window_request = Some(Request::Open { view, point: None });
        }
    }

    pub(crate) fn detached_chrome_command(&mut self, command: &str) -> bool {
        if self.detached_window_id().is_none() {
            return false;
        }
        match command {
            "new_terminal" | "toggle_terminal" => {
                if command == "new_terminal" || self.focused_terminal_index().is_none() {
                    self.new_terminal_in_editor_tab();
                }
            }
            "show_explorer"
            | "toggle_explorer"
            | "toggle_sidebar"
            | "source_control"
            | "extensions"
            | "global_search"
            | "toggle_panel"
            | "show_notifications"
            | "toggle_problems"
            | "move_terminal_to_right"
            | "show_output" => {
                self.notify("This view is available in the main window");
            }
            _ => return false,
        }
        true
    }
}
