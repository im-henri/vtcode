//! Remove deleted disk files from every editor group without losing unsaved text.
use crate::app::{App, Tab, TabKind};
use crate::native_paths::{same_spelling, strip_spelling_prefix};
use std::path::{Path, PathBuf};

fn normalized(path: &Path) -> PathBuf {
    if crate::remote::workspace::is_view_key(path) { return path.to_owned(); }
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

impl App {
    /// `confirmed` is for a successful local delete or rollback. Watcher and
    /// periodic probes also inspect never-saved paths; those are not deletions.
    pub(crate) fn close_deleted_file_tabs(&mut self, path: &Path, confirmed: bool) -> bool {
        // A hidden pane may still have a clean view of a shared dirty document.
        // Classify all views from current shared state before detaching any.
        for tab in self.tabs.iter_mut().chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs)) {
            if matches!(tab.kind, TabKind::File | TabKind::MarkdownPreview) { tab.editor.refresh_document(); }
        }
        let target = normalized(path);
        // A save can briefly displace its old file. Its completion requests a
        // fresh probe; sibling tabs in a deleted directory can still close.
        let saving: Vec<_> = self
            .pending_file_saves
            .keys()
            .map(|path| normalized(path))
            .collect();
        let mut aliases_to_check = Vec::new();
        let mut matches = |tab: &Tab| {
            if !matches!(tab.kind, TabKind::File | TabKind::MarkdownPreview)
                || (tab.editor.bg_loading && tab.editor.pending_session_restore.is_some())
                || !(confirmed
                    || tab.editor.last_disk_modified.is_some()
                    || tab.editor.saved_rope.is_some()
                    || (tab.editor.bg_loading && tab.editor.pending_session_restore.is_none()))
            {
                return false;
            }
            let Some(path) = &tab.editor.file_path else { return false; };
            let candidate = normalized(path);
            if !confirmed && saving.iter().any(|saving| same_spelling(&candidate, saving)) {
                return false;
            }
            if strip_spelling_prefix(&candidate, &target).is_some() {
                return true;
            }
            if cfg!(windows) && !crate::remote::workspace::is_view_key(path)
                && crate::native_paths::strip_possible_alias_prefix(&candidate, &target).is_some()
            {
                // The notification establishes absence for its own spelling.
                // Inspect a possible alias before changing its editor state.
                aliases_to_check.push(path.clone());
            }
            false
        };
        let mut closed_paths = Vec::new();
        let mut recovered = 0;
        let detach = |tab: &mut Tab, paths: &mut Vec<PathBuf>, recovered: &mut usize| {
            if tab.kind == TabKind::File { tab.editor.refresh_document(); }
            // Another view has already recovered this shared document.
            if tab.kind == TabKind::File && tab.editor.file_path.is_none() {
                tab.preview = false;
                return true;
            }
            if tab.kind == TabKind::File && tab.editor.modified {
                paths.push(tab.editor.file_path.take().unwrap());
                tab.editor.last_disk_modified = None;
                tab.editor.disk_unavailable = false;
                tab.editor.file_size = 0;
                tab.editor.saved_rope = None;
                tab.editor.remote_checkpoint = None;
                tab.editor.invalidate_unsaved_diff_cache();
                for state in tab
                    .editor
                    .undo_stack
                    .write().iter_mut()
                    .chain(tab.editor.redo_stack.write().iter_mut())
                {
                    state.modified = state.rope.len_bytes() != 0;
                }
                tab.editor.clear_git_line_status();
                tab.editor.pending_session_restore = None;
                tab.preview = false;
                tab.editor.publish_document();
                *recovered += 1;
                true
            } else {
                false
            }
        };

        // Remove inactive views first: closing the last active tab can compact
        // the groups and move another group's tabs into the active fields.
        let mut changed = false;
        for group in &mut self.groups {
            let mut group_changed = false;
            crate::tab_collection::retain(&mut group.tabs, &mut group.active_tab, &mut group.tab_mru_order, |_, tab| {
                if !matches(tab) {
                    return true;
                }
                group_changed = true;
                if detach(tab, &mut closed_paths, &mut recovered) {
                    return true;
                }
                closed_paths.push(tab.editor.file_path.clone().unwrap());
                false
            });
            if group_changed {
                group.hovered_tab = None;
                group.hovered_close_btn = false;
                group.visible_highlight_key = None;
                group.provisional_highlight_key = None;
                if let Some(token) = group.visible_highlight_cancel_token.take() {
                    token.cancel();
                }
                if let Some(token) = group.provisional_highlight_cancel_token.take() {
                    token.cancel();
                }
                changed = true;
            }
        }
        for index in (0..self.tabs.len()).rev() {
            if !matches(&self.tabs[index]) {
                continue;
            }
            changed = true;
            if detach(&mut self.tabs[index], &mut closed_paths, &mut recovered) {
                continue;
            }
            if index == self.active_tab {
                // This dialog otherwise saves whichever neighbour becomes active.
                self.save_as_active = false;
            }
            self.force_close_tab(index);
        }
        if !aliases_to_check.is_empty() {
            self.queue_file_disk_checks(aliases_to_check);
        }
        if !changed {
            return false;
        }
        for path in closed_paths {
            self.forget_remote_file_changes(&path);
            self.lsp_manager.did_close(&path);
        }
        for tab in self.tabs.iter_mut().chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs)) {
            if tab.kind == TabKind::File { tab.editor.refresh_document(); }
        }
        // Reopen Closed Editor must not recreate a deleted file with empty text.
        self.closed_tab_paths
            .retain(|(path, _, _)| strip_spelling_prefix(&normalized(path), &target).is_none());
        self.closed_file_views
            .retain(|path, _| strip_spelling_prefix(&normalized(path), &target).is_none());
        self.navigation.remove_path(path);
        if self
            .groups
            .iter()
            .enumerate()
            .any(|(index, group)| index != self.active_group && group.tabs.is_empty())
        {
            self.close_empty_groups();
        }
        self.markdown_preview = self
            .tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .any(|tab| tab.kind == TabKind::MarkdownPreview);
        self.reconcile_navigation();
        self.clear_transient_editor_state();
        self.validate_diff_state_for_active_tab();
        self.hovered_tab = None;
        self.hovered_close_button = false;
        self.tab_scroll_manual_anchor = None;
        self.ensure_active_tab_visible();
        if recovered != 0 {
            self.notify_warn(format!(
                "Unsaved edits from {} kept in Untitled",
                path.display()
            ));
        }
        true
    }
}

#[cfg(test)]
#[path = "app_deleted_files_tests.rs"]
mod tests;
