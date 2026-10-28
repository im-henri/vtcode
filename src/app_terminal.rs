use crate::app::{
    ActivePane, App, Tab, TabKind, TaskDefinition, TerminalDock, TerminalTab,
};
use crate::editor::Editor;
use std::time::Instant;
#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
const TERMINAL_PROCESS_BUDGET: Duration = Duration::from_millis(4);

#[cfg(test)]
#[path = "app_terminal_responsiveness_tests.rs"]
mod responsiveness_tests;

#[cfg(test)]
#[path = "app_terminal_creation_tests.rs"]
mod creation_tests;

impl App {
    /// Create a new terminal tab
    pub(crate) fn new_terminal_tab(&mut self) {
        self.new_terminal_tab_in(TerminalDock::Right);
    }
    pub(crate) fn new_terminal_tab_at(&mut self, cwd: &std::path::Path) {
        self.insert_terminal(TerminalTab::new(String::new(),cwd.to_owned()));
        self.terminals[self.active_terminal].name = format!("{}: Terminal", self.active_terminal + 1);
        self.show_terminal_sidebar = true;
        self.terminal_sidebar_revealed = true;
        self.terminal_panel_focused = true;
    }

    pub(crate) fn new_terminal_tab_in(&mut self, dock: TerminalDock) {
        let cwd = self.explorer.root.clone();
        self.insert_terminal(TerminalTab::new_in(String::new(), cwd, dock));
        self.terminals[self.active_terminal].name = format!("{}: Terminal", self.active_terminal + 1);
    }

    fn insert_terminal(&mut self, terminal: TerminalTab) {
        if self.terminals.len() == 1 && !self.terminal_is_open(0) {
            self.terminals[0] = terminal;
            self.active_terminal = 0;
        } else {
            self.terminals.push(terminal);
            self.active_terminal = self.terminals.len() - 1;
        }
    }

    pub(crate) fn new_terminal_in_editor_tab(&mut self) {
        self.new_terminal_tab();
        self.detach_terminal_to_tab();
        self.ensure_active_tab_visible();
        self.notify("New terminal opened in editor tab");
    }

    pub(crate) fn terminal_is_in_dock(&self, index: usize, dock: TerminalDock) -> bool {
        self.terminals
            .get(index)
            .is_some_and(|terminal| terminal.dock == dock && !self.terminal_is_in_editor(index))
    }

    pub(crate) fn active_terminal_in_dock(&self, dock: TerminalDock) -> Option<usize> {
        if self.terminal_is_in_dock(self.active_terminal, dock) {
            return Some(self.active_terminal);
        }
        (0..self.terminals.len()).find(|&index| self.terminal_is_in_dock(index, dock))
    }

    pub(crate) fn has_terminal_in_dock(&self, dock: TerminalDock) -> bool {
        self.active_terminal_in_dock(dock).is_some()
    }

    /// Hidden and not-yet-launched sessions still count as open.
    pub(crate) fn terminal_is_open(&self, index: usize) -> bool {
        self.terminals.get(index).is_some_and(|terminal| {
            !terminal.is_placeholder || terminal.terminal_pty.is_some()
                || terminal.terminal_launch_error.is_some() || terminal.pending_command.is_some()
                || self.terminal_is_in_editor(index) || self.show_terminal_sidebar
        })
    }

    pub(crate) fn docked_terminal_has_focus(&self) -> bool {
        self.terminal_panel_focused && self.show_terminal_sidebar
            && self.active_terminal < self.terminals.len()
            && !self.terminal_is_in_editor(self.active_terminal)
    }

    pub(crate) fn toggle_terminal_sidebar(&mut self) {
        if self.show_terminal_sidebar {
            self.show_terminal_sidebar = false;
            self.terminal_panel_focused = false;
            self.notify("Terminal collapsed");
            return;
        }

        let index = self.active_terminal_in_dock(TerminalDock::Right).unwrap_or_else(|| {
            self.new_terminal_tab_in(TerminalDock::Right);
            self.active_terminal
        });
        self.active_terminal = index;
        self.terminals[index].is_placeholder = false;
        self.terminal_sidebar_revealed = true;
        self.show_terminal_sidebar = true;
        self.terminal_panel_focused = true;
        self.active_pane = ActivePane::Editor;
        self.notify("Terminal opened on the right");
    }

    /// Close the active terminal. Keep one empty terminal slot because the rest
    /// of the application deliberately treats the terminal collection as non-empty.
    pub(crate) fn close_terminal_tab(&mut self) {
        self.close_terminal_tab_at(self.active_terminal);
    }

    pub(crate) fn close_terminal_tab_at(&mut self, index: usize) {
        if index >= self.terminals.len() {
            return;
        }
        self.terminal_selection_drag = None;
        self.terminal_mouse_capture = None;
        self.terminal_clicks = Default::default();
        self.terminal_surfaces.clear();
        if self.terminals.len() == 1 {
            self.remove_terminal_editor_tabs(index);
            let cwd = self.explorer.root.clone();
            let dock = self.terminals[0].dock;
            self.terminals[0] = TerminalTab::placeholder(cwd, dock);
            self.active_terminal = 0;
            self.dragging_terminal_tab = None;
            self.terminal_panel_focused = false;
            match dock {
                TerminalDock::Right => {
                    self.show_terminal_sidebar = false;
                    self.terminal_sidebar_revealed = false;
                }
            }
            return;
        }

        self.remove_terminal_editor_tabs(index);
        self.terminals.remove(index);
        for desk in &mut self.desks.items {
            if let Some(layout)=&mut desk.layout {
                if layout.terminal>index {layout.terminal-=1;}
                layout.terminal=layout.terminal.min(self.terminals.len().saturating_sub(1));
            }
        }
        for tab in &mut self.tabs {
            if let Some(terminal_idx) = &mut tab.terminal_idx {
                if *terminal_idx > index {
                    *terminal_idx -= 1;
                }
            }
        }
        for group in &mut self.groups {
            for tab in &mut group.tabs {
                if let Some(terminal_idx) = &mut tab.terminal_idx {
                    if *terminal_idx > index {
                        *terminal_idx -= 1;
                    }
                }
            }
        }
        if index < self.active_terminal {
            self.active_terminal -= 1;
        }
        if self.active_terminal >= self.terminals.len() {
            self.active_terminal = self.terminals.len() - 1;
        }
        if let Some(dragging) = self.dragging_terminal_tab {
            self.dragging_terminal_tab = if dragging == index {
                None
            } else if dragging > index {
                Some(dragging - 1)
            } else {
                Some(dragging)
            };
        }
        if !self.has_terminal_in_dock(TerminalDock::Right) {
            self.show_terminal_sidebar = false;
            self.terminal_sidebar_revealed = false;
        }
        if !self.docked_terminal_has_focus() {
            self.terminal_panel_focused = false;
        }
    }

    /// Drain terminal output and close every tab whose child process has exited.
    /// Closing in reverse order keeps terminal indices stable when multiple
    /// processes finish during the same event-loop tick.
    #[cfg(test)]
    pub(crate) fn process_terminals(&mut self) -> bool {
        let deadline = Instant::now() + TERMINAL_PROCESS_BUDGET;
        self.process_terminals_until(deadline, || true)
    }

    pub(crate) fn process_terminals_until(&mut self, deadline: Instant, mut continue_processing: impl FnMut() -> bool) -> bool {
        let mut needs_redraw = false;
        let mut processed_output = false;
        let mut exited = Vec::new();
        let count = self.terminals.len();
        for visited in 0..count {
            let index = self.next_terminal_to_process % count;
            self.next_terminal_to_process = (index + 1) % count;
            let mut clipboard_update = None;
            if let Some(pty) = self.terminals[index].terminal_pty.as_mut() {
                // Input can arrive while one terminal is being parsed. Check
                // between parser quanta, not only when moving to another PTY.
                if pty.process_while(|| Instant::now() < deadline && continue_processing()) {
                    processed_output = true;
                    needs_redraw |= self.terminal_surfaces.iter().any(|surface| {
                        surface.index == index && !surface.area.is_empty()
                    });
                }
                if pty.take_exit_event() {
                    processed_output = true;
                    exited.push(index);
                }
                clipboard_update = pty.grid.clipboard_update.take();
            }
            if let Some(text) = clipboard_update {
                if let Err(error) = crate::clipboard::set_text(text) {
                    self.notify(format!("Terminal copy failed: {error}"));
                    needs_redraw = true;
                }
            }
            if visited + 1 < count && ((processed_output && Instant::now() >= deadline) || !continue_processing()) {
                // Resume at the next terminal, including quiet terminals whose
                // one-shot exit event still needs handling. Do not lose that
                // work if all producers finished before this processing turn.
                self.background.wake_handle().notify();
                break;
            }
        }
        if !exited.is_empty() {
            needs_redraw = true;
        }
        exited.sort_unstable();
        for index in exited.into_iter().rev() {
            self.close_terminal_tab_at(index);
        }
        needs_redraw
    }

    /// Remove any editor-tab view attached to `index`. This is intentionally
    /// done before the terminal vector is compacted so no tab can silently
    /// start pointing at the next terminal or at an out-of-bounds slot.
    fn remove_terminal_editor_tabs(&mut self, index: usize) {
        let is_match = |tab: &Tab| {
            tab.kind == TabKind::Terminal && tab.terminal_idx == Some(index)
        };
        let mut removed = false;
        let mut keep = |_: usize, tab: &mut Tab| {
            let remove = is_match(tab);
            removed |= remove;
            !remove
        };
        crate::tab_collection::retain(&mut self.tabs, &mut self.active_tab, &mut self.tab_mru_order, &mut keep);
        for group in &mut self.groups {
            crate::tab_collection::retain(&mut group.tabs, &mut group.active_tab, &mut group.tab_mru_order, &mut keep);
        }
        if removed && self.split_active() {
            self.close_empty_groups();
        }
    }

    pub(crate) fn terminal_is_in_editor(&self, index: usize) -> bool {
        let has_terminal = |tab: &Tab| {
            tab.kind == TabKind::Terminal && tab.terminal_idx == Some(index)
        };
        self.tabs.iter().any(has_terminal)
            || self.groups.iter().enumerate().any(|(i,group)| self.group_in_current_desk(i) && group.tabs.iter().any(has_terminal))
    }

    /// Move a panel terminal into an editor group, where it behaves like any
    /// other editor tab (including tab reordering and split-pane dragging).
    pub(crate) fn detach_terminal_to_group(&mut self, index: usize, target_group: usize) -> bool {
        if index >= self.terminals.len() || self.terminal_is_in_editor(index) {
            return false;
        }
        let source_dock = self.terminals[index].dock;
        if target_group != self.active_group {
            if target_group >= self.groups.len() {
                return false;
            }
            self.switch_group(target_group);
        }

        let name = self.terminals[index].name.clone();
        let mut editor = Editor::new();
        editor.file_path = None;
        editor.read_only = true;
        let tab = Tab {
            editor,
            pinned: false,
            preview: false,
            kind: TabKind::Terminal,
            extension_view: None,
            file_search: Default::default(),
            terminal_idx: Some(index),
        };
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
        self.active_terminal = index;
        self.terminals[index].is_placeholder = false;
        self.active_pane = ActivePane::Editor;
        self.touch_tab_mru(self.active_tab);
        self.terminal_panel_focused = false;
        match source_dock {
            TerminalDock::Right if !self.has_terminal_in_dock(TerminalDock::Right) => {
                self.show_terminal_sidebar = false;
                self.terminal_sidebar_revealed = false;
            }
            _ => {}
        }
        self.notify(format!("Terminal \"{name}\" moved to editor tab"));
        true
    }

    /// Move the active terminal from the panel into the active editor group.
    pub(crate) fn detach_terminal_to_tab(&mut self) {
        self.detach_terminal_to_group(self.active_terminal, self.active_group);
    }

    /// Move a terminal out of any editor group and into a workbench dock.
    pub(crate) fn move_terminal_to_dock(
        &mut self,
        index: usize,
        dock: TerminalDock,
    ) -> bool {
        if index >= self.terminals.len() {
            return false;
        }
        self.remove_terminal_editor_tabs(index);
        self.terminals[index].dock = dock;
        self.terminals[index].is_placeholder = false;
        self.active_terminal = index;
        self.active_pane = ActivePane::Editor;
        self.terminal_panel_focused = true;

        self.terminal_sidebar_revealed = true;
        self.show_terminal_sidebar = true;
        self.notify("Terminal moved to the right side");
        true
    }

    pub(crate) fn ensure_terminal_pty_at(
        &mut self,
        index: usize,
        width: usize,
        height: usize,
    ) {
        let Some(tab) = self.terminals.get(index) else {
            return;
        };
        if tab.terminal_pty.is_some() || tab.terminal_launch_error.is_some() {
            return;
        }

        let cwd = tab.terminal_cwd.clone();
        self.terminals[index].is_placeholder = false;
        let wake = self.background.wake_handle();
        let result = if let Some(remote) = &self.remote_workspace {
            remote.remote_path(&cwd)
                .and_then(|cwd| remote.client.spawn_async(String::new(), Vec::new(), cwd, Some((width.max(2) as u16, height.max(2) as u16)), wake.clone()))
                .and_then(|process| crate::terminal::TerminalEmulator::from_remote(process, width.max(2), height.max(2), wake))
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error>)
        } else { crate::terminal::TerminalEmulator::spawn_in_dir_with_wake(
            width.max(2),
            height.max(2),
            &cwd,
            wake,
        ) };

        match result {
            Ok(emu) => {
                let profile_name = emu.profile_name.clone();
                let tab = &mut self.terminals[index];
                if tab.name.ends_with(": Terminal") {
                    tab.name = format!("{}: {profile_name}", index + 1);
                }
                tab.terminal_launch_error = None;
                tab.terminal_pty = Some(emu);
                if let Some(command) = tab.pending_command.take() {
                    if let Some(pty) = &tab.terminal_pty {
                        pty.write(command.as_bytes());
                        pty.write(b"\r");
                    }
                }
            }
            Err(e) => {
                self.terminals[index].terminal_launch_error = Some(e.to_string());
            }
        }
    }

    pub(crate) fn retry_terminal_pty_at(
        &mut self,
        index: usize,
        width: usize,
        height: usize,
    ) {
        if let Some(tab) = self.terminals.get_mut(index) {
            tab.terminal_launch_error = None;
            tab.terminal_pty = None;
        }
        self.ensure_terminal_pty_at(index, width, height);
    }

    /// Discover project task definitions without running their commands.
    pub(crate) fn discover_tasks(&mut self) { self.task_list = discover_tasks(&self.explorer.root); }

    /// Execute a task by running its command in a new terminal tab
    pub(crate) fn run_task(&mut self, command: &str, label: &str) {
        self.last_task = Some(command.to_string());
        // Create a new terminal tab for the task
        let cwd = self.explorer.root.clone();
        let name = format!("Task: {label}");
        let mut terminal = TerminalTab::new(name, cwd);
        terminal.pending_command = Some(command.to_string());
        self.insert_terminal(terminal);
        // Reveal after insertion so an unopened slot can still be reused.
        self.show_terminal_sidebar = true;
        self.terminal_sidebar_revealed = true;
        self.terminal_panel_focused = true;
        self.active_pane = ActivePane::Editor;
    }

    /// Open the task picker UI
    pub(crate) fn open_task_picker(&mut self) {
        if let Some(remote) = self.remote_workspace.clone() {
            self.task_list.clear();
            self.task_picker_active = true;
            self.task_picker_query.clear(); self.task_picker_cursor = 0;
            self.task_picker_selected = 0; self.task_picker_selection_anchor = None;
            self.status_message = "Loading remote tasks...".into();
            let sender = self.background.sender.clone(); let generation = self.workspace_generation;
            self.background.spawn("remote-tasks",move || {
                let result = remote.client.call(crate::remote::Command::Tasks).and_then(|reply| match reply {
                    crate::remote::Reply::Tasks(tasks) => Ok(tasks), _ => Err(std::io::Error::other("Invalid task list reply")),
                }).map_err(|error| error.to_string());
                let _ = sender.send(crate::background::BackgroundResult::WorkspaceResult { generation,
                    result: Box::new(crate::background::BackgroundResult::RemoteTasks(result)) });
            });
            return;
        }
        self.discover_tasks();
        if self.task_list.is_empty() {
            self.status_message =
                "No tasks found. Add .vscode/tasks.json or use a recognized project format.".into();
            return;
        }
        self.task_picker_active = true;
        self.task_picker_selected = 0;
        self.task_picker_query.clear();
        self.task_picker_cursor = 0;
        self.task_picker_selection_anchor = None;
    }

}

pub(crate) fn discover_tasks(root: &std::path::Path) -> Vec<TaskDefinition> {
    let mut definitions = Vec::new();
        // Parse .vscode/tasks.json
        let tasks_json_path = root.join(".vscode").join("tasks.json");
        if let Ok(content) = std::fs::read_to_string(&tasks_json_path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(tasks) = json.get("tasks").and_then(|t| t.as_array()) {
                    for task in tasks {
                        let label = task
                            .get("label")
                            .and_then(|l| l.as_str())
                            .unwrap_or("")
                            .to_string();
                        let command =
                            if let Some(cmd) = task.get("command").and_then(|c| c.as_str()) {
                                let args: Vec<String> = task
                                    .get("args")
                                    .and_then(|a| a.as_array())
                                    .map(|arr| {
                                        arr.iter()
                                            .filter_map(|v| v.as_str().map(String::from))
                                            .collect()
                                    })
                                    .unwrap_or_default();
                                if args.is_empty() {
                                    cmd.to_string()
                                } else {
                                    format!("{} {}", cmd, args.join(" "))
                                }
                            } else {
                                continue;
                            };
                        if !label.is_empty() {
                            definitions.push(TaskDefinition {
                                label,
                                command,
                                source: "vscode".into(),
                            });
                        }
                    }
                }
            }
        }

        // Auto-detect Cargo tasks
        if root.join("Cargo.toml").exists() {
            definitions.push(TaskDefinition {
                label: "cargo build".into(),
                command: "cargo build".into(),
                source: "cargo".into(),
            });
            definitions.push(TaskDefinition {
                label: "cargo test".into(),
                command: "cargo test".into(),
                source: "cargo".into(),
            });
            definitions.push(TaskDefinition {
                label: "cargo run".into(),
                command: "cargo run".into(),
                source: "cargo".into(),
            });
            definitions.push(TaskDefinition {
                label: "cargo clippy".into(),
                command: "cargo clippy".into(),
                source: "cargo".into(),
            });
            definitions.push(TaskDefinition {
                label: "cargo check".into(),
                command: "cargo check".into(),
                source: "cargo".into(),
            });
        }

        // Auto-detect npm/package.json scripts
        let pkg_path = root.join("package.json");
        if let Ok(content) = std::fs::read_to_string(&pkg_path) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(scripts) = json.get("scripts").and_then(|s| s.as_object()) {
                    for (name, _cmd) in scripts {
                        definitions.push(TaskDefinition {
                            label: format!("npm: {name}"),
                            command: format!("npm run {name}"),
                            source: "npm".into(),
                        });
                    }
                }
            }
        }

        // Auto-detect Makefile
        if root.join("Makefile").exists() || root.join("makefile").exists() {
            definitions.push(TaskDefinition {
                label: "make".into(),
                command: "make".into(),
                source: "make".into(),
            });
            definitions.push(TaskDefinition {
                label: "make clean".into(),
                command: "make clean".into(),
                source: "make".into(),
            });
        }

        // Auto-detect Python
        if root.join("setup.py").exists() || root.join("pyproject.toml").exists() {
            definitions.push(TaskDefinition {
                label: "python: test".into(),
                command: "python -m pytest".into(),
                source: "python".into(),
            });
        }
    definitions
}

#[cfg(test)]
mod tests {
    use crate::app::{App, TabKind, TerminalDock};
    use crate::terminal::TerminalEmulator;

    #[test]
    fn new_terminal_tab_reuses_empty_slot_and_preserves_unlaunched_sessions() {
        for dock in [TerminalDock::Right] {
            let mut app = App::test_new();
            assert!(!app.terminal_is_open(0));
            app.new_terminal_tab_in(dock);
            assert_eq!(app.terminals.len(), 1);
            assert_eq!(app.active_terminal, 0);
            assert_eq!(app.terminals[0].dock, dock);
            assert_eq!(app.terminals[0].name, "1: Terminal");
            assert_eq!(app.terminals[0].terminal_cwd, app.explorer.root);
            assert!(app.terminal_is_open(0));
            assert!(app.terminals[0].terminal_pty.is_none());

            // A second request before the first paint creates another session.
            app.new_terminal_tab_in(dock);
            assert_eq!(app.terminals.len(), 2);
            assert_eq!(app.active_terminal, 1);
            assert_eq!(app.terminals[1].name, "2: Terminal");
            assert!(app.terminal_is_open(0) && app.terminal_is_open(1));
        }
    }

    #[test]
    fn new_terminal_preserves_hidden_sessions_including_before_first_paint() {
        for state in ["unlaunched", "running", "error", "task"] {
            let mut app = App::test_new();
            match state {
                "unlaunched" => {
                    app.toggle_terminal_sidebar();
                    app.toggle_terminal_sidebar();
                }
                "running" => app.terminals[0].terminal_pty = Some(
                    TerminalEmulator::test_with_output(40, 10, "original output")),
                "error" => app.terminals[0].terminal_launch_error = Some("original error".into()),
                _ => app.terminals[0].pending_command = Some("original command".into()),
            }
            app.terminals[0].name = "Original".into();
            app.new_terminal_tab();
            assert_eq!(app.terminals.len(), 2, "{state}");
            assert_eq!(app.terminals[0].name, "Original");
            assert_eq!(app.active_terminal, 1);
            match state {
                "running" => assert!(app.terminals[0].terminal_pty.is_some()),
                "error" => assert_eq!(app.terminals[0].terminal_launch_error.as_deref(), Some("original error")),
                "task" => assert_eq!(app.terminals[0].pending_command.as_deref(), Some("original command")),
                _ => assert!(app.terminal_is_open(0)),
            }
        }
    }

    #[test]
    fn first_task_reuses_empty_slot_and_subsequent_tasks_keep_their_commands() {
        let mut app = App::test_new();
        app.run_task("first command", "First");
        assert_eq!(app.terminals.len(), 1);
        assert_eq!(app.terminals[0].name, "Task: First");
        assert_eq!(app.terminals[0].pending_command.as_deref(), Some("first command"));
        app.run_task("second command", "Second");
        assert_eq!(app.terminals.len(), 2);
        assert_eq!(app.terminals[0].pending_command.as_deref(), Some("first command"));
        assert_eq!(app.terminals[1].pending_command.as_deref(), Some("second command"));
        assert_eq!(app.terminals[1].dock, TerminalDock::Right);
        assert!(app.show_terminal_sidebar && app.docked_terminal_has_focus());
    }

    #[test]
    fn detach_terminal_creates_normal_editor_tab() {
        let mut app = App::test_new();
        app.show_panel = true;

        assert!(app.detach_terminal_to_group(0, app.active_group));
        let tab = app.tabs.last().expect("terminal editor tab");
        assert_eq!(tab.kind, TabKind::Terminal);
        assert_eq!(tab.terminal_idx, Some(0));
        assert!(app.terminal_is_in_editor(0));
        // Moving a right-side terminal must not close an unrelated bottom panel.
        assert!(app.show_panel);
        assert!(!app.show_terminal_sidebar);
        assert!(!app.terminal_panel_focused);
    }

    #[test]
    fn detach_terminal_does_not_duplicate_editor_tab() {
        let mut app = App::test_new();
        assert!(app.detach_terminal_to_group(0, app.active_group));
        assert!(!app.detach_terminal_to_group(0, app.active_group));
        assert_eq!(
            app.tabs.iter().filter(|tab| tab.kind == TabKind::Terminal).count(),
            1
        );
    }

    #[test]
    fn close_terminal_tab_removes_entry() {
        let mut app = App::test_new();
        app.new_terminal_tab();
        app.new_terminal_tab();
        let count = app.terminals.len();
        app.close_terminal_tab();
        assert_eq!(app.terminals.len(), count - 1);
    }

    #[test]
    fn closing_last_terminal_replaces_it_with_a_fresh_real_shell_slot() {
        let mut app = App::test_new();
        app.show_terminal_sidebar = true;
        app.terminal_sidebar_revealed = true;
        app.terminal_panel_focused = true;
        app.terminals[0].terminal_launch_error = Some("failed".to_string());
        app.close_terminal_tab();
        assert_eq!(app.terminals.len(), 1);
        assert!(app.terminals[0].terminal_launch_error.is_none());
        assert!(app.terminals[0].terminal_pty.is_none());
        assert!(app.terminals[0].pending_command.is_none());
        assert!(!app.show_terminal_sidebar);
        assert!(!app.terminal_sidebar_revealed);
        assert!(!app.terminal_panel_focused);
    }

    #[test]
    fn closing_last_terminal_keeps_the_problems_panel_visible() {
        let mut app = App::test_new();
        app.show_terminal_sidebar = true;
        app.show_panel = true;
        app.panel_tab = 1;

        app.close_terminal_tab();

        assert!(app.show_panel);
        assert_eq!(app.panel_tab, 1);
        assert!(!app.show_terminal_sidebar);
    }

    #[test]
    fn exited_process_closes_its_visible_terminal_tab() {
        let (program, args) = crate::terminal::test_child_command();

        let mut app = App::test_new();
        app.show_terminal_sidebar = true;
        app.terminal_sidebar_revealed = true;
        app.terminal_panel_focused = true;
        app.terminals[0].name = "exiting".to_string();
        let cwd = std::env::current_dir().expect("current directory");
        app.terminals[0].terminal_pty = Some(
            TerminalEmulator::spawn_command_in_dir(&program, &args, 80, 24, &cwd)
                .expect("spawn short-lived terminal process"),
        );

        let mut closed = false;
        for _ in 0..100 {
            app.process_terminals();
            if app.terminals[0].name == "1: Terminal"
                && app.terminals[0].terminal_pty.is_none()
            {
                closed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        assert!(closed, "exited terminal should be replaced by the hidden placeholder");
        assert!(!app.show_terminal_sidebar);
        assert!(!app.terminal_sidebar_revealed);
        assert!(!app.terminal_panel_focused);
    }

    #[test]
    fn toggle_terminal_opens_and_collapses_the_right_dock() {
        let mut app = App::test_new();
        assert!(!app.show_terminal_sidebar);

        app.toggle_terminal_sidebar();
        assert!(app.show_terminal_sidebar);
        assert!(app.docked_terminal_has_focus());

        app.toggle_terminal_sidebar();
        assert!(!app.show_terminal_sidebar);
        assert!(!app.terminal_panel_focused);
    }

    #[test]
    fn terminal_can_move_from_editor_tab_to_right_without_changing_notifications() {
        let mut app = App::test_new();
        app.show_panel = true;
        app.panel_tab = 3;
        assert!(app.detach_terminal_to_group(0, app.active_group));
        assert!(app.move_terminal_to_dock(0, TerminalDock::Right));

        assert!(!app.terminal_is_in_editor(0));
        assert!(app.terminal_is_in_dock(0, TerminalDock::Right));
        assert!(app.show_terminal_sidebar);
        assert!(app.show_panel);
        assert_eq!(app.panel_tab, 3);
    }

    #[test]
    fn closing_attached_terminal_removes_stale_editor_reference() {
        let mut app = App::test_new();
        app.new_terminal_tab();
        assert!(app.detach_terminal_to_group(0, app.active_group));

        app.close_terminal_tab_at(0);

        assert_eq!(app.terminals.len(), 1);
        assert!(app
            .tabs
            .iter()
            .all(|tab| tab.kind != TabKind::Terminal));
    }

}
