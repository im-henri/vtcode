use std::path::PathBuf;

use crate::app::{App, ContextMenuItem, GitFileChange};
use crate::helpers::len_as_u16;

impl App {
    pub(crate) fn tab_compare_path(&self, tab: usize) -> Option<&std::path::Path> {
        self.tabs
            .get(tab)
            .filter(|tab| tab.kind == crate::app::TabKind::File)
            .and_then(|tab| tab.editor.file_path.as_deref())
    }

    pub(crate) fn tab_context_command_disabled(&self, command: &str) -> bool {
        match command {
            "tab_group_edit" | "tab_group_remove" => self.tabs.get(self.tab_context_tab).is_none_or(|t| self.tab_group(t).is_none()),
            "duplicate_tab" => !self.can_duplicate_tab(self.tab_context_tab),
            "tab_select_compare" => self.tab_compare_path(self.tab_context_tab).is_none(),
            "tab_compare_selected" => self
                .tab_compare_path(self.tab_context_tab)
                .is_none_or(|path| !self.can_compare_with_selected(path)),
            _ => false,
        }
    }

    pub(crate) fn execute_tab_compare_command(&mut self, tab: usize, command: &str) {
        let Some(path) = self.tab_compare_path(tab).map(std::path::Path::to_path_buf) else {
            return;
        };
        match command {
            "tab_select_compare" => self.select_file_for_compare(path),
            "tab_compare_selected" => self.compare_file_with_selected(path),
            _ => {}
        }
    }

    pub(crate) fn open_explorer_context_menu(&mut self, x: u16, y: u16) {
        let Some((target, is_file)) = self
            .explorer
            .entries
            .get(self.explorer.selected)
            .map(|entry| (entry.path.clone(), !entry.is_dir))
        else {
            return;
        };
        self.explorer_context_extension_commands.clear();
        self.explorer_context_target = Some(target);
        self.explorer_context_extension_commands = if is_file {
            self.extension_manager.explorer_context_commands()
        } else {
            Vec::new()
        };
        self.activate_explorer_context_menu(x, y);
    }

    /// Open the safe workspace-root menu for the folder header or unused tree
    /// space. The root is not an Explorer entry, so it must be targeted
    /// explicitly—especially when the workspace has no children at all.
    pub(crate) fn open_explorer_workspace_context_menu(&mut self, x: u16, y: u16) {
        self.clear_explorer_selection();
        self.explorer_context_extension_commands.clear();
        self.explorer_context_target = Some(self.explorer.root.clone());
        self.activate_explorer_context_menu(x, y);
    }

    fn activate_explorer_context_menu(&mut self, x: u16, y: u16) {
        self.explorer_context_pos = (x, y);
        self.explorer_context_inner = None;
        self.explorer_context_selected = 0;
        self.explorer_context_active = true;
    }

    pub(crate) fn explorer_context_is_workspace_root(&self) -> bool {
        self.explorer_context_target.as_ref() == Some(&self.explorer.root)
    }

    pub(crate) fn set_explorer_single_selection(&mut self, idx: usize) {
        if idx >= self.explorer.entries.len() {
            return;
        }
        self.explorer.selected = idx;
        self.explorer_selection_anchor = Some(idx);
        self.explorer_multi_selected.clear();
        self.explorer_multi_selected
            .push(self.explorer.entries[idx].path.clone());
    }

    pub(crate) fn clear_explorer_selection(&mut self) {
        self.explorer_multi_selected.clear();
        self.explorer_selection_anchor = None;
    }

    pub(crate) fn toggle_explorer_selection(&mut self, idx: usize) {
        if idx >= self.explorer.entries.len() {
            return;
        }
        self.explorer.selected = idx;
        if self.explorer_selection_anchor.is_none() {
            self.explorer_selection_anchor = Some(idx);
        }
        let path = self.explorer.entries[idx].path.clone();
        if let Some(existing) = self.explorer_multi_selected.iter().position(|p| p == &path) {
            self.explorer_multi_selected.remove(existing);
        } else {
            self.explorer_multi_selected.push(path.clone());
        }
        if self.explorer_multi_selected.is_empty() {
            self.explorer_multi_selected.push(path);
        }
    }

    pub(crate) fn range_select_explorer(&mut self, idx: usize, additive: bool) {
        if idx >= self.explorer.entries.len() {
            return;
        }
        let anchor = self
            .explorer_selection_anchor
            .unwrap_or(self.explorer.selected);
        let start = anchor.min(idx);
        let end = anchor.max(idx);
        if !additive {
            self.explorer_multi_selected.clear();
        }
        let entries = &self.explorer.entries[start..=end.min(self.explorer.entries.len() - 1)];
        if entries.len() > 8
            && entries.len().saturating_add(self.explorer_multi_selected.len()) > 32
        {
            // Preserve selection order and existing additive selections, but
            // do not search the growing selection again for every new row.
            // Borrow paths while deduplicating; only newly selected files need
            // an owned path. This also handles duplicate constructed entries.
            let mut selected: rustc_hash::FxHashSet<_> =
                self.explorer_multi_selected.iter().collect();
            let additions: Vec<_> = entries.iter()
                .filter(|entry| selected.insert(&entry.path))
                .map(|entry| entry.path.clone())
                .collect();
            self.explorer_multi_selected.extend(additions);
        } else {
            for entry in entries {
                if !self
                    .explorer_multi_selected
                    .iter()
                    .any(|p| p == &entry.path)
                {
                    self.explorer_multi_selected.push(entry.path.clone());
                }
            }
        }
        self.explorer.selected = idx;
    }

    pub(crate) fn request_explorer_delete(&mut self, target: PathBuf) {
        self.request_explorer_delete_targets(vec![target]);
    }

    pub(crate) fn request_explorer_selection_delete(&mut self) {
        // Keyboard actions own the selection, even when Ctrl-click left the
        // focused row deselected. Context-menu actions keep their own target.
        if !self.explorer_multi_selected.is_empty() {
            self.confirm_close_active = false;
            self.exit_confirm_active = false;
            self.request_explorer_delete_targets(self.explorer_multi_selected.to_vec());
        } else if let Some(entry) = self.explorer.entries.get(self.explorer.selected) {
            let target = entry.path.clone();
            self.confirm_close_active = false;
            self.exit_confirm_active = false;
            self.request_explorer_delete(target);
        }
    }

    pub(crate) fn request_explorer_delete_targets(&mut self, mut targets: Vec<PathBuf>) {
        // Borrow paths while marking duplicates, then compact the existing
        // vector. Large selections stay linear without cloning every path
        // again for an owned hash set or changing their confirmation order.
        let keep: Vec<_> = {
            let mut seen = rustc_hash::FxHashSet::default();
            targets.iter().map(|target| seen.insert(target)).collect()
        };
        let mut keep = keep.into_iter();
        targets.retain(|_| keep.next() == Some(true));
        if targets.is_empty() {
            return;
        }
        self.explorer_context_active = false;
        self.confirm_delete_target = targets.first().cloned();
        self.confirm_delete_targets = targets;
        self.confirm_delete_active = true;
        self.confirm_delete_selected = 0;
        self.modal = Default::default();
    }

    pub(crate) fn request_explorer_move(&mut self, source: PathBuf, destination: PathBuf) {
        self.explorer_context_active = false;
        self.confirm_move_source = Some(source);
        self.confirm_move_destination = Some(destination);
        self.confirm_move_active = true;
        self.confirm_move_selected = 0;
    }

    pub(crate) fn explorer_context_targets(&self) -> Vec<PathBuf> {
        let Some(target) = self.explorer_context_target.as_ref() else {
            return Vec::new();
        };
        if self.explorer_multi_selected.len() > 1 && self.explorer_multi_selected.contains(target) {
            return self.explorer_multi_selected.to_vec();
        }
        vec![target.clone()]
    }

    pub(crate) fn open_source_control_context_menu(
        &mut self,
        x: u16,
        y: u16,
        target: GitFileChange,
    ) {
        if let Some(index) = self.source_control_index_for_target(&target) {
            self.preserve_source_control_selection(index);
            self.sc_context_targets = self.selected_source_control_changes();
        } else {
            self.sc_context_targets = vec![target.clone()];
        }
        self.sc_context_target = Some(target);
        self.sc_context_pos = (x, y);
        self.sc_context_inner = None;
        self.sc_context_selected = 0;
        self.sc_context_active = true;
    }

    pub(crate) fn open_source_control_action_menu(&mut self, x: u16, y: u16) {
        self.context_menu_button_drag = None;
        self.context_menu_items = if self.source_control_enabled {
            [
                ("Refresh", "git_refresh", true),
                ("Fetch", "git_fetch", false),
                ("Pull", "git_pull", false),
                ("Push", "git_push", true),
                ("Switch / Create Branch…", "git_checkout", true),
                (if self.sc_visibility.untracked { "✓ Show untracked files" } else { "  Show untracked files" },
                    "git_show_untracked", true),
                ("Disable Source Control", "source_control_toggle", false),
            ].into_iter().map(|(label, id, separator_after)| ContextMenuItem {
                label: label.into(), shortcut: String::new(), id, separator_after,
            }).collect()
        } else {
            vec![ContextMenuItem { label: "Enable Source Control".into(), shortcut: String::new(),
                id: "source_control_toggle", separator_after: false }]
        };
        if self.source_control_enabled && self.source_control_action_menu_items() {
            let mut index = self.context_menu_items.len().saturating_sub(1);
            let mut add = |label: &str, id: &'static str| {
                self.context_menu_items.insert(index, ContextMenuItem {
                    label: label.into(), shortcut: String::new(), id, separator_after: true,
                });
                index += 1;
            };
            if self.repositories.parent.is_some() && !self.repositories.parent_open { add("Open Parent Repository…", "git_open_parent"); }
            if self.repositories.active.is_some() { add("Close Repository", "git_close_repository"); }
            if self.repositories.closed_roots().next().is_some() { add("Reopen Repository…", "git_open_repository"); }
        }
        self.context_menu_pos = (x, y);
        self.context_menu_inner = None;
        self.context_menu_selected = 0;
        self.context_menu_active = true;
    }

    pub(crate) fn open_source_control_filter_menu(&mut self, x: u16, y: u16) {
        self.context_menu_button_drag = None;
        self.context_menu_items = vec![ContextMenuItem {
            label: format!("{} Show untracked files", if self.sc_visibility.untracked { "✓" } else { " " }),
            shortcut: String::new(),
            id: "git_show_untracked",
            separator_after: false,
        }];
        self.context_menu_pos = (x, y);
        self.context_menu_inner = None;
        self.context_menu_selected = 0;
        self.context_menu_active = true;
    }

    pub(crate) fn source_control_filter_menu_open(&self) -> bool {
        self.context_menu_active && self.context_menu_items.first().is_some_and(|item| item.id == "git_show_untracked")
    }

    pub(crate) fn source_control_action_menu_open(&self) -> bool {
        self.context_menu_active && self.source_control_action_menu_items()
    }

    pub(crate) fn source_control_action_menu_items(&self) -> bool {
        self.context_menu_items.last().is_some_and(|item| item.id == "source_control_toggle")
    }

    pub(crate) fn source_control_menu_command_disabled(&self, id: &str) -> bool {
        self.source_control_action_menu_items()
            && matches!(id, "git_fetch" | "git_pull" | "git_push" | "git_checkout")
            && (self.source_control_repository_state != crate::app::SourceControlRepositoryState::Available
                || self.bg_git_remote_running || self.bg_git_action_running)
    }

    pub(crate) fn source_control_context_menu_items(&self) -> Vec<&'static str> {
        if let Some(target) = &self.sc_context_target {
            if self.sc_context_targets.len() > 1 {
                let mut items = if target.directory { vec!["Reveal in Explorer"] } else { vec!["Open Diff", "Open File"] };
                if self.sc_context_targets.iter().any(|file| !file.staged) { items.push("Stage Selected"); }
                if self.sc_context_targets.iter().any(|file| file.staged) { items.push("Unstage Selected"); }
                items.extend(["Discard Selected", "Cancel"]);
                return items;
            }
            if target.directory { return vec!["Reveal in Explorer", "Stage", "Discard", "Cancel"]; }
            vec![
                "Open Diff",
                "Open File",
                if target.staged { "Unstage" } else { "Stage" },
                "Discard",
                "Cancel",
            ]
        } else {
            vec!["Cancel"]
        }
    }

    pub(crate) fn open_hunk_popup(
        &mut self,
        x: u16,
        y: u16,
        file: impl Into<Option<PathBuf>>,
        line: usize,
        status: char,
    ) {
        self.close_hunk_popup();
        self.hunk_popup_pos = (x, y);
        self.hunk_popup_selected = 0;
        self.hunk_popup_file = file.into();
        self.hunk_popup_line = line;
        self.hunk_popup_status = status;
        self.hunk_popup_active = true;
        self.request_hunk_preview();
    }

    pub(crate) fn close_hunk_popup(&mut self) {
        self.hunk_popup_generation = self.hunk_popup_generation.wrapping_add(1);
        if let Some(feedback) = self.hunk_preview_feedback.take() {
            feedback.cancel();
        }
        self.hunk_peek = None;
        self.hunk_popup_active = false;
        self.hunk_popup_selected = 0;
        self.hunk_popup_file = None;
        self.hunk_popup_line = 0;
        self.hunk_popup_status = ' ';
    }

    pub(crate) fn hunk_popup_items(&self) -> Vec<&'static str> {
        if let Some(feedback) = &self.hunk_preview_feedback {
            return if feedback.loading() { vec!["Close Preview"] } else { vec!["Retry Preview", "Close Preview"] };
        }
        match self.hunk_popup_status {
            'N' if self.hunk_peek.is_some() => vec!["Revert Unsaved Hunk", "Close Preview"],
            'S' => vec!["Revert Hunk", "Close Preview"],
            'U' | 'A' | 'M' | 'D' | '?' => vec!["Stage Hunk", "Discard Hunk", "Close Preview"],
            _ => vec!["Close Preview"],
        }
    }

    pub(crate) fn hunk_popup_rect(&self, term_w: u16, term_h: u16) -> ratatui::layout::Rect {
        if self.hunk_peek.is_some() || self.hunk_preview_feedback.is_some() {
            return crate::ui_hunk_peek::peek_rect(self, term_w, term_h);
        }
        let items = self.hunk_popup_items();
        let menu_width = items
            .iter()
            .map(|item| len_as_u16(item.len()))
            .max()
            .unwrap_or(0)
            .saturating_add(2)
            .max(18);
        let menu_height = len_as_u16(items.len()) + 2;
        let (raw_x, raw_y) = self.hunk_popup_pos;
        let x = raw_x.min(term_w.saturating_sub(menu_width));
        let y = raw_y.min(term_h.saturating_sub(menu_height));
        ratatui::layout::Rect::new(x, y, menu_width, menu_height)
    }

    pub(crate) fn execute_hunk_popup_action(&mut self, idx: usize) {
        let items = self.hunk_popup_items();
        let action = match items.get(idx) {
            Some(action) => *action,
            None => {
                self.close_hunk_popup();
                return;
            }
        };
        if action == "Close Preview" {
            self.close_hunk_popup();
            return;
        }
        if action == "Retry Preview" {
            self.retry_hunk_preview();
            return;
        }
        if action == "Revert Unsaved Hunk" {
            match self.revert_unsaved_hunk() {
                Ok(()) => self.notify("Reverted unsaved hunk"),
                Err(error) => self.notify_error(error),
            }
            self.close_hunk_popup();
            return;
        }
        let file = self.hunk_popup_file.clone();
        let line = self.hunk_popup_line;
        let Some(file) = file else {
            self.close_hunk_popup();
            return;
        };
        // Keep the preview alive until the action validates the exact hunk
        // shown to the user against the current Git patch.
        match action {
            "Stage Hunk" => self.git_stage_hunk(&file, line),
            "Revert Hunk" => self.git_unstage_hunk(&file, line),
            "Discard Hunk" => self.git_discard_hunk(&file, line),
            _ => {}
        }
        self.close_hunk_popup();
    }

    fn source_control_index_for_target(&self, target: &GitFileChange) -> Option<usize> {
        self.source_control_files
            .iter()
            .position(|f| {
                f.path == target.path && f.staged == target.staged && f.status == target.status
            })
            .or_else(|| {
                self.source_control_files
                    .iter()
                    .position(|f| f.path == target.path)
            })
    }

    pub(crate) fn execute_source_control_context(&mut self, idx: usize) {
        self.sc_context_active = false;
        self.sc_context_inner = None;
        let items = self.source_control_context_menu_items();
        let action = match items.get(idx) {
            Some(a) => *a,
            None => return,
        };
        let target = self.sc_context_target.clone();
        let mut targets = std::mem::take(&mut self.sc_context_targets);
        if targets.is_empty() { targets.extend(target.clone()); }
        match action {
            "Open File" | "Reveal in Explorer" => {
                if let Some(target) = target.as_ref() {
                    self.open_source_control_entry(target);
                }
            }
            "Open Diff" => {
                if let Some(target) = target.as_ref() {
                    if let Some(file_idx) = self.source_control_index_for_target(target) {
                        self.open_source_control_diff(file_idx);
                    }
                }
            }
            "Stage" | "Stage Selected" => self.git_stage_changes(targets),
            "Unstage" | "Unstage Selected" => self.git_unstage_changes(targets),
            "Discard" | "Discard Selected" => self.request_source_control_discard(targets, false),
            _ => {}
        }
        self.sc_context_target = None;
    }

    pub(crate) fn execute_confirm_delete(&mut self, confirmed: bool) {
        let mut targets = std::mem::take(&mut self.confirm_delete_targets);
        if targets.is_empty() {
            if let Some(target) = self.confirm_delete_target.take() {
                targets.push(target);
            }
        } else {
            self.confirm_delete_target = None;
        }
        self.confirm_delete_active = false;
        if !confirmed || targets.is_empty() {
            return;
        }

        self.start_file_deletion(targets);
    }

    pub(crate) fn execute_confirm_move(&mut self, confirmed: bool) {
        let source = self.confirm_move_source.take();
        let destination = self.confirm_move_destination.take();
        self.confirm_move_active = false;
        if !confirmed {
            return;
        }
        let (Some(source), Some(destination)) = (source, destination) else {
            return;
        };
        if let Some(remote) = &self.remote_workspace {
            let task = remote.remote_path(&source).and_then(|from| remote.remote_path(&destination).map(|to| crate::remote::fs::Task::Rename { from, to }));
            match task { Ok(task) => self.run_remote_filesystem_action("Move", task, None), Err(error) => self.notify_error(error.to_string()) }
            return;
        }

        self.run_filesystem_action_async("explorer-move", "Move", move || {
            if !source.exists() {
                return Err(format!("{} no longer exists", source.display()));
            }
            if destination.exists() {
                return Err(format!("{} already exists", destination.display()));
            }
            let destination_parent = destination
                .parent()
                .ok_or_else(|| "destination has no parent directory".to_string())?;
            if !destination_parent.is_dir() {
                return Err(format!(
                    "destination directory {} no longer exists",
                    destination_parent.display()
                ));
            }
            crate::app_explorer::rename_no_replace(&source, &destination).map_err(|error| error.to_string())?;
            let message = format!("Moved to {}", destination_parent.display());
            Ok(crate::background::FilesystemActionOutcome {
                message,
                renames: vec![(source, destination)],
                ..Default::default()
            })
        });
    }

    pub(crate) fn binary_open_prompt_option_count(&self) -> usize {
        // Default open action + extension commands + cancel
        self.binary_open_prompt_commands.len() + 2
    }

    pub(crate) fn execute_binary_open_prompt_choice(&mut self, choice: usize) {
        let path = self.binary_open_prompt_path.clone();
        let extension_commands = self.binary_open_prompt_commands.clone();
        let open_normally = self.binary_open_prompt_open_normally;
        self.binary_open_prompt_active = false;
        self.binary_open_prompt_path = None;
        self.binary_open_prompt_selected = 0;
        self.binary_open_prompt_commands.clear();
        self.binary_open_prompt_open_normally = false;

        let Some(path) = path else {
            return;
        };

        if choice == 0 {
            if open_normally {
                self.binary_open_prompt_skip_once = Some(path.clone());
                self.open_file(&path);
            } else {
                self.open_binary_file_info_tab(&path);
            }
            return;
        }
        if choice == extension_commands.len() + 1 {
            self.notify("Cancelled");
            return;
        }

        if let Some(command) = extension_commands.get(choice.saturating_sub(1)) {
            self.run_extension_command_with_optional_context(
                &command.extension_id,
                &command.command_id,
                Some(&path),
            );
        }
    }
}

#[cfg(test)]
#[path = "explorer_selection_performance_tests.rs"]
mod selection_performance_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn make_app() -> App {
        let tmp = std::env::temp_dir().join("vtcode_ctx_test");
        let _ = std::fs::create_dir_all(&tmp);
        App::new(tmp)
    }

    // --- explorer_context_targets ---

    #[test]
    fn explorer_context_targets_empty_when_no_target() {
        let app = make_app();
        assert!(app.explorer_context_targets().is_empty());
    }

    #[test]
    fn explorer_context_targets_single_when_no_multi() {
        let mut app = make_app();
        let path = PathBuf::from("/some/file.rs");
        app.explorer_context_target = Some(path.clone());
        let targets = app.explorer_context_targets();
        assert_eq!(targets, vec![path]);
    }

    #[test]
    fn explorer_context_targets_returns_multi_when_target_in_multi() {
        let mut app = make_app();
        let a = PathBuf::from("/a.rs");
        let b = PathBuf::from("/b.rs");
        app.explorer_context_target = Some(a.clone());
        app.explorer_multi_selected = vec![a.clone(), b.clone()].into();
        let targets = app.explorer_context_targets();
        assert_eq!(targets, vec![a, b]);
    }

    #[test]
    fn explorer_context_targets_single_when_target_not_in_multi() {
        let mut app = make_app();
        let a = PathBuf::from("/a.rs");
        let b = PathBuf::from("/b.rs");
        let c = PathBuf::from("/c.rs");
        app.explorer_context_target = Some(c.clone());
        app.explorer_multi_selected = vec![a, b].into();
        let targets = app.explorer_context_targets();
        assert_eq!(targets, vec![c]);
    }

    #[test]
    fn drag_move_is_staged_for_confirmation() {
        let mut app = make_app();
        let source = PathBuf::from("source/file.rs");
        let destination = PathBuf::from("destination/file.rs");

        app.request_explorer_move(source.clone(), destination.clone());

        assert!(app.confirm_move_active);
        assert_eq!(app.confirm_move_source, Some(source));
        assert_eq!(app.confirm_move_destination, Some(destination));
        assert_eq!(app.confirm_move_selected, 0);
        assert!(!app.bg_filesystem_action_running);
    }

    #[test]
    fn cancelling_drag_move_clears_request_without_touching_filesystem() {
        let mut app = make_app();
        app.request_explorer_move(
            PathBuf::from("source/file.rs"),
            PathBuf::from("destination/file.rs"),
        );

        app.execute_confirm_move(false);

        assert!(!app.confirm_move_active);
        assert!(app.confirm_move_source.is_none());
        assert!(app.confirm_move_destination.is_none());
        assert!(!app.bg_filesystem_action_running);
    }

    // --- source_control_action_menu_open ---

    #[test]
    fn action_menu_not_open_by_default() {
        let app = make_app();
        assert!(!app.source_control_action_menu_open());
    }

    #[test]
    fn action_menu_open_after_opening() {
        let mut app = make_app();
        app.open_source_control_action_menu(5, 5);
        assert!(app.source_control_action_menu_open());
        assert_eq!(app.context_menu_items.len(), 7);
        assert_eq!(app.context_menu_items[0].id, "git_refresh");
        assert_eq!(app.context_menu_items[4].id, "git_checkout");
        assert_eq!(app.context_menu_items[5].id, "git_show_untracked");
        assert_eq!(app.context_menu_items[6].id, "source_control_toggle");
    }

    // --- source_control_context_menu_items ---

    #[test]
    fn context_menu_items_cancel_when_no_target() {
        let app = make_app();
        assert_eq!(app.source_control_context_menu_items(), vec!["Cancel"]);
    }

    #[test]
    fn context_menu_items_unstaged_file() {
        let mut app = make_app();
        app.sc_context_target = Some(GitFileChange { directory: false,
            original_path: None,
            path: "file.rs".to_string(),
            status: 'M',
            staged: false,
        });
        let items = app.source_control_context_menu_items();
        assert_eq!(items, vec!["Open Diff", "Open File", "Stage", "Discard", "Cancel"]);
    }

    #[test]
    fn context_menu_items_staged_file() {
        let mut app = make_app();
        app.sc_context_target = Some(GitFileChange { directory: false,
            original_path: None,
            path: "file.rs".to_string(),
            status: 'A',
            staged: true,
        });
        let items = app.source_control_context_menu_items();
        assert_eq!(items, vec!["Open Diff", "Open File", "Unstage", "Discard", "Cancel"]);
    }

    // --- hunk_popup_items ---

    #[test]
    fn hunk_popup_items_staged_hunk() {
        let mut app = make_app();
        app.hunk_popup_status = 'S';
        assert_eq!(app.hunk_popup_items(), vec!["Revert Hunk", "Close Preview"]);
    }

    #[test]
    fn hunk_popup_items_unstaged_modified() {
        let mut app = make_app();
        app.hunk_popup_status = 'M';
        assert_eq!(
            app.hunk_popup_items(),
            vec!["Stage Hunk", "Discard Hunk", "Close Preview"]
        );
    }

    #[test]
    fn hunk_popup_items_untracked() {
        let mut app = make_app();
        app.hunk_popup_status = '?';
        assert_eq!(
            app.hunk_popup_items(),
            vec!["Stage Hunk", "Discard Hunk", "Close Preview"]
        );
    }

    #[test]
    fn hunk_popup_items_unknown_status() {
        let mut app = make_app();
        app.hunk_popup_status = 'X';
        assert_eq!(app.hunk_popup_items(), vec!["Close Preview"]);
    }

    // --- hunk_popup_rect ---

    #[test]
    fn hunk_popup_rect_basic() {
        let mut app = make_app();
        app.hunk_popup_status = 'M';
        app.hunk_popup_pos = (10, 5);
        let rect = app.hunk_popup_rect(80, 24);
        assert!(rect.width >= 18);
        assert_eq!(rect.height, 5); // 3 items + 2 border
        assert!(rect.x <= 80);
        assert!(rect.y <= 24);
    }

    #[test]
    fn hunk_popup_rect_clamps_to_terminal() {
        let mut app = make_app();
        app.hunk_popup_status = 'S';
        app.hunk_popup_pos = (75, 22);
        let rect = app.hunk_popup_rect(80, 24);
        assert!(rect.x + rect.width <= 80);
        assert!(rect.y + rect.height <= 24);
    }

    // --- binary_open_prompt_option_count ---

    #[test]
    fn binary_open_prompt_count_no_commands() {
        let app = make_app();
        assert_eq!(app.binary_open_prompt_option_count(), 2);
    }

    // --- open_hunk_popup / close_hunk_popup ---

    #[test]
    fn open_and_close_hunk_popup() {
        let mut app = make_app();
        app.open_hunk_popup(10, 20, PathBuf::from("test.rs"), 42, 'M');
        assert!(app.hunk_popup_active);
        assert_eq!(app.hunk_popup_pos, (10, 20));
        assert_eq!(app.hunk_popup_line, 42);
        assert_eq!(app.hunk_popup_status, 'M');
        assert_eq!(app.hunk_popup_file, Some(PathBuf::from("test.rs")));

        app.close_hunk_popup();
        assert!(!app.hunk_popup_active);
        assert_eq!(app.hunk_popup_file, None);
        assert_eq!(app.hunk_popup_line, 0);
        assert_eq!(app.hunk_popup_status, ' ');
    }

    // --- open_source_control_context_menu ---

    #[test]
    fn open_source_control_context_menu_sets_state() {
        let mut app = make_app();
        let target = GitFileChange { directory: false,
            original_path: None,
            path: "main.rs".to_string(),
            status: 'M',
            staged: false,
        };
        app.open_source_control_context_menu(15, 25, target.clone());
        assert!(app.sc_context_active);
        assert_eq!(app.sc_context_pos, (15, 25));
        assert_eq!(app.sc_context_target.as_ref().unwrap().path, "main.rs");
    }
}
