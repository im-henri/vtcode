use crate::app::{App, GitFileChange};
use rustc_hash::FxHashSet;
use std::path::PathBuf;
use crate::source_control_rows::{Group, Layout, Visibility};

type ChangeKey = (String, bool);

pub(crate) struct SourceControlSender {
    sender: crate::background::BackgroundResultSender,
    cancel: crate::background::CancellationToken,
    repository: Option<(PathBuf, u64)>,
}

impl SourceControlSender {
    pub(crate) fn is_cancelled(&self) -> bool { self.cancel.is_cancelled() }

    pub(crate) fn cancellation(&self) -> &crate::background::CancellationToken { &self.cancel }

    pub(crate) fn send(&self, result: crate::background::BackgroundResult) -> Result<(), ()> {
        if self.is_cancelled() { return Ok(()); }
        let result = if let Some((root, generation)) = &self.repository {
            crate::background::BackgroundResult::RepositoryRead { root: root.clone(), generation: *generation, result: Box::new(result) }
        } else { result };
        self.sender.send(crate::background::BackgroundResult::SourceControlRead {
            cancel: self.cancel.clone(), result: Box::new(result),
        }).map_err(|_| ())
    }
}

#[derive(Default)]
pub(crate) struct SourceControlSelection {
    // Separate sets allow lookups by &str, without cloning paths to construct
    // temporary (String, bool) keys on every painted row or selection scan.
    keys: [FxHashSet<String>; 2],
    anchor: Option<ChangeKey>,
    explicit: bool,
}

pub(crate) struct DiscardRequest {
    pub root: PathBuf,
    pub changes: Vec<GitFileChange>,
    pub all: bool,
}

fn key(change: &GitFileChange) -> ChangeKey {
    (change.path.clone(), change.staged)
}

impl SourceControlSelection {
    pub(crate) fn retain_visible(&mut self, files: &mut Vec<GitFileChange>, selected: usize,
        visibility: Visibility) -> usize {
        let focused = files.get(selected).map(key);
        files.retain(|file| visibility.shows(Group::of(file)));
        self.reconcile_focused(focused, selected, files, visibility)
    }

    pub(crate) fn reconcile(&mut self, previous: &[GitFileChange], selected: usize,
        files: &[GitFileChange], visibility: Visibility) -> usize {
        let focused = previous.get(selected).map(key);
        self.reconcile_focused(focused, selected, files, visibility)
    }

    pub(crate) fn update_worktree(&mut self, files: &mut Vec<GitFileChange>, selected: usize,
        changed: Vec<GitFileChange>, paths: &crate::app_git::refresh::WorktreePaths, visibility: Visibility) -> usize {
        let focused = files.get(selected).map(key);
        // The index did not change. In particular a staged rename must keep
        // its original path even when only its destination was edited.
        files.retain(|file| file.staged || !paths.contains(&file.path));
        if !changed.is_empty() {
            // Both lists are already ordered. Move records in a linear merge;
            // do not clone every cached path or sort the repository again.
            let compare = |a: &GitFileChange, b: &GitFileChange|
                Group::of(a).cmp(&Group::of(b)).then(a.path.cmp(&b.path));
            if files.last().is_none_or(|last| !compare(last, &changed[0]).is_gt()) {
                files.extend(changed);
            } else {
                let mut previous = std::mem::take(files).into_iter().peekable();
                let mut changed = changed.into_iter().peekable();
                files.reserve(previous.len() + changed.len());
                while let (Some(old), Some(new)) = (previous.peek(), changed.peek()) {
                    if compare(old, new).is_le() { files.push(previous.next().unwrap()); }
                    else { files.push(changed.next().unwrap()); }
                }
                files.extend(previous);
                files.extend(changed);
            }
        }
        self.reconcile_focused(focused, selected, files, visibility)
    }

    fn reconcile_focused(&mut self, focused: Option<ChangeKey>, mut selected: usize,
        files: &[GitFileChange], visibility: Visibility) -> usize {
        if self.explicit {
            let mut available = [FxHashSet::default(), FxHashSet::default()];
            for file in files.iter().filter(|file| visibility.shows(Group::of(file))) {
                available[usize::from(file.staged)].insert(file.path.as_str());
            }
            for (selected, available) in self.keys.iter_mut().zip(&available) {
                selected.retain(|path| available.contains(path.as_str()));
            }
            if self.anchor.as_ref().is_some_and(|key| {
                !available[usize::from(key.1)].contains(key.0.as_str())
            }) {
                self.anchor = None;
            }
        }
        selected = focused
            .and_then(|focused| files.iter().position(|file| file.staged == focused.1 && file.path == focused.0))
            .unwrap_or(
                selected
                    .min(files.len().saturating_sub(1)),
            );
        if !files.get(selected).is_some_and(|file| visibility.shows(Group::of(file))) {
            selected = Layout::new(files, visibility, [false; 4]).file_at(0).unwrap_or(0);
        }
        selected
    }
}

impl App {
    pub(crate) fn source_control_sender(&self) -> SourceControlSender {
        SourceControlSender {
            sender: self.background.sender.clone(),
            cancel: self.source_control_cancel.clone(),
            repository: None,
        }
    }

    pub(crate) fn repository_sender(&self) -> SourceControlSender {
        let mut sender = self.source_control_sender();
        sender.repository = Some((self.source_control_root().to_owned(), self.repositories.view_generation));
        sender
    }

    pub(crate) fn save_source_control_enabled(&mut self, scope: crate::settings::SettingsScope, enabled: bool) {
        if let Err(error) = self.patch_settings_scope(scope, "sourceControlEnabled", |settings| {
            settings.source_control_enabled = Some(enabled);
        }) {
            self.notify_error(format!("Could not save Source Control setting: {error}"));
        }
    }

    pub(crate) fn toggle_workspace_source_control(&mut self) {
        self.save_source_control_enabled(crate::settings::SettingsScope::Workspace, !self.source_control_enabled);
    }

    pub(crate) fn save_source_control_show_untracked(&mut self, scope: crate::settings::SettingsScope, show: bool) {
        if let Err(error) = self.patch_settings_scope(scope, "sourceControlShowUntracked", |settings| {
            settings.source_control_show_untracked = Some(show);
        }) {
            self.notify_error(format!("Could not save Show untracked files: {error}"));
        }
    }

    pub(crate) fn set_source_control_enabled(&mut self, enabled: bool) {
        // The Explorer may have been replaced by a folder switch even when
        // the effective preference did not change.
        self.explorer.set_source_control_enabled(enabled);
        if self.source_control_enabled == enabled {
            if !enabled { self.source_control_repository_state = crate::app::SourceControlRepositoryState::Disabled; }
            return;
        }
        self.source_control_enabled = enabled;
        let pending_compare = self.pending_git_compare_path.clone();
        self.cancel_source_control_reads();
        // Keep an interrupted comparison attached to its view so re-enabling
        // resumes it; a folder switch uses cancel_source_control_reads alone.
        self.pending_git_compare_path = pending_compare;
        self.source_control_last_refresh = None;
        self.git_branch_picker_active = false;
        self.commit_details.generation = self.commit_details.generation.wrapping_add(1);
        self.commit_details.loading = false;
        if self.commit_details.diff_loading && self.commit_details.diff_full_cache.is_none() {
            self.commit_details.diff_file = None;
        }
        self.commit_details.diff_loading = false;
        self.commit_details.review.pending = None;
        self.clear_git_history();
        self.cached_git_branch.clear();
        self.blame_cache = None;
        self.blame_cache_key = None;
        self.blame_requested_path = None;
        self.blame_documents.clear();
        self.inline_blame_hover = None;
        self.update_source_control_files(Vec::new());
        self.source_control_input_mode = false;
        self.source_control_toggle_area = None;
        self.source_control_actions_area = None;
        self.repositories.reopen_button = None;
        self.source_control_initialize_area = None;
        self.source_control_selection = Default::default();
        self.source_control_scroll = 0;
        self.resizing_sc_graph = false;
        self.sc_graph_click_start = None;
        self.commit_input.scrollbar_drag = None;
        self.sc_context_active = false;
        self.sc_context_target = None;
        self.sc_context_targets.clear();
        self.sc_discard_confirm_active = false;
        self.sc_discard_request = None;
        self.sc_stage_all_commit_request = None;
        if self.source_control_action_menu_open() || self.source_control_filter_menu_open() {
            self.context_menu_active = false;
        }
        if self.hunk_popup_status != 'N' { self.close_hunk_popup(); }
        for tab in self.tabs.iter_mut().chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs)) {
            // Snapshot diffs keep their comparison markers; normal documents
            // keep their independent saved-buffer/undo tracking.
            if tab.kind == crate::app::TabKind::File { tab.editor.set_git_line_status(Vec::new()); }
            tab.editor.blame.enabled = false;
            tab.editor.blame.selected = None;
            tab.editor.blame.history_loading = false;
            tab.editor.blame.history_request = tab.editor.blame.history_request.wrapping_add(1);
        }
        self.source_control_repository_state = if enabled {
            crate::app::SourceControlRepositoryState::Checking
        } else {
            crate::app::SourceControlRepositoryState::Disabled
        };
        if !enabled && self.bg_explorer_refresh_running {
            // Replace a partial ignore-decorated snapshot, without starting an
            // otherwise unnecessary filesystem refresh when already idle.
            self.refresh_explorer_tree_async();
        }
        if enabled {
            self.refresh_source_control_async();
            self.refresh_git_branch_async();
            self.refresh_git_line_status_async();
            self.refresh_git_log_async();
            self.refresh_explorer_tree_async();
            if let Some(path) = self.pending_git_compare_path.clone() {
                self.start_pending_git_compare_for_path(&path);
            }
            if self.commit_details.active { self.load_commit_details(); }
        }
    }

    pub(crate) fn cancel_source_control_reads(&mut self) {
        // A commit, discard, checkout or remote operation that has already
        // started must finish normally; only optional readers are cancelled.
        self.source_control_cancel.cancel();
        self.source_control_cancel = crate::background::CancellationToken::new();
        if !self.source_control_enabled { self.source_control_cancel.cancel(); }
        self.bg_git_status_running = false;
        self.bg_git_status_pending = false;
        self.pending_git_status_paths = None;
        self.bg_git_branch_running = false;
        self.bg_git_branch_pending = false;
        self.bg_git_diff_running = false;
        self.bg_git_diff_pending = false;
        self.bg_blame_running = false;
        self.bg_git_branch_picker_running = false;
        self.git_compare_generation = self.git_compare_generation.wrapping_add(1);
        self.bg_git_compare_running = false;
        self.pending_git_compare_path = None;
        self.cancel_source_control_diff_refresh();
    }

    pub(crate) fn source_control_layout(&self) -> Layout {
        Layout::new(&self.source_control_files, self.sc_visibility,
            [self.sc_merge_collapsed, self.sc_staged_collapsed, self.sc_changes_collapsed, self.sc_untracked_collapsed])
    }

    pub(crate) fn source_control_group_collapsed(&self, group: Group) -> bool {
        match group {
            Group::Merge => self.sc_merge_collapsed,
            Group::Staged => self.sc_staged_collapsed,
            Group::Unstaged => self.sc_changes_collapsed,
            Group::Untracked => self.sc_untracked_collapsed,
        }
    }

    pub(crate) fn set_source_control_group_collapsed(&mut self, group: Group, collapsed: bool) {
        match group {
            Group::Merge => self.sc_merge_collapsed = collapsed,
            Group::Staged => self.sc_staged_collapsed = collapsed,
            Group::Unstaged => self.sc_changes_collapsed = collapsed,
            Group::Untracked => self.sc_untracked_collapsed = collapsed,
        }
    }

    pub(crate) fn source_control_file_visible(&self, index: usize) -> bool {
        self.source_control_files.get(index).is_some_and(|file| self.sc_visibility.shows(Group::of(file)))
    }

    pub(crate) fn explorer_git_badge_visibility(&self) -> crate::explorer::GitBadgeVisibility {
        use crate::explorer::GitBadgeVisibility;
        if !self.source_control_enabled { GitBadgeVisibility::None }
        else if self.sc_visibility.untracked { GitBadgeVisibility::All }
        else { GitBadgeVisibility::Tracked }
    }

    pub(crate) fn set_source_control_visibility(&mut self, visibility: Visibility) {
        if self.sc_visibility == visibility { return; }
        self.sc_visibility = visibility;
        // A previous multi-selection must not silently retain newly hidden paths.
        self.source_control_selection = SourceControlSelection::default();
        if !self.source_control_file_visible(self.source_control_selected) {
            self.source_control_selected = self.source_control_layout().file_at(0).unwrap_or(0);
        }
        self.source_control_scroll = 0;
        self.sc_context_active = false;
        self.sc_context_targets.clear();
        self.source_control_stage_all_area = None;
        self.source_control_unstage_all_area = None;
        self.source_control_discard_all_area = None;
        self.source_control_untracked_stage_area = None;
        self.source_control_untracked_discard_area = None;
        if self.repositories.discovered {
            if visibility.untracked {
                // A tracked-only or path-limited snapshot cannot populate
                // the newly visible group. Coalesce a complete refresh.
                if self.source_control_enabled { self.refresh_source_control_async(); }
            } else {
                self.clear_untracked_source_control_cache();
            }
        }
    }

    pub(crate) fn source_control_group_changes(&self, group: Group) -> Vec<GitFileChange> {
        self.source_control_files.iter()
            .filter(|file| self.sc_visibility.shows(group) && Group::of(file) == group)
            .cloned().collect()
    }

    pub(crate) fn source_control_file_selected(&self, index: usize) -> bool {
        if !self.source_control_file_visible(index) { return false; }
        self.source_control_files.get(index).is_some_and(|file| {
            if self.source_control_selection.explicit {
                self.source_control_selection.keys[usize::from(file.staged)].contains(file.path.as_str())
            } else {
                index == self.source_control_selected
            }
        })
    }

    pub(crate) fn selected_source_control_changes(&self) -> Vec<GitFileChange> {
        self.source_control_files
            .iter()
            .enumerate()
            .filter(|(index, _)| self.source_control_file_selected(*index))
            .map(|(_, file)| file.clone())
            .collect()
    }

    pub(crate) fn select_source_control_file(&mut self, index: usize, toggle: bool, range: bool) {
        if !self.source_control_file_visible(index) { return; }
        let Some(target) = self.source_control_files.get(index).map(key) else {
            return;
        };
        if !self.source_control_selection.explicit {
            if let Some(current) = self.source_control_files.get(self.source_control_selected)
                .filter(|file| self.sc_visibility.shows(Group::of(file))) {
                self.source_control_selection.keys[usize::from(current.staged)].insert(current.path.clone());
                self.source_control_selection.anchor = Some(key(current));
            }
            self.source_control_selection.explicit = true;
        }
        if range {
            let anchor = self
                .source_control_selection
                .anchor
                .as_ref()
                .and_then(|anchor| {
                    self.source_control_files
                        .iter()
                        .position(|file| file.staged == anchor.1 && file.path == anchor.0)
                })
                .unwrap_or(index);
            if !toggle {
                self.source_control_selection.keys.iter_mut().for_each(FxHashSet::clear);
            }
            for file in &self.source_control_files[anchor.min(index)..=anchor.max(index)] {
                if self.sc_visibility.shows(Group::of(file)) && !self.source_control_group_collapsed(Group::of(file)) {
                    self.source_control_selection.keys[usize::from(file.staged)].insert(file.path.clone());
                }
            }
        } else {
            if toggle && self.source_control_selection.keys[usize::from(target.1)].contains(target.0.as_str()) {
                self.source_control_selection.keys[usize::from(target.1)].remove(target.0.as_str());
            } else {
                if !toggle {
                    self.source_control_selection.keys.iter_mut().for_each(FxHashSet::clear);
                }
                self.source_control_selection.keys[usize::from(target.1)].insert(target.0.clone());
            }
            self.source_control_selection.anchor = Some(target);
        }
        self.source_control_selected = index;
        self.source_control_input_mode = false;
        self.sc_graph_focused = false;
    }

    pub(crate) fn preserve_source_control_selection(&mut self, index: usize) {
        if !self.source_control_file_selected(index) {
            self.select_source_control_file(index, false, false);
        } else {
            self.source_control_selected = index;
        }
        self.active_pane = crate::app::ActivePane::Explorer;
        self.source_control_input_mode = false;
        self.sc_graph_focused = false;
    }

    pub(crate) fn select_all_source_control_files(&mut self) {
        self.source_control_selection.explicit = true;
        self.source_control_selection.keys.iter_mut().for_each(FxHashSet::clear);
        for file in &self.source_control_files {
            if self.sc_visibility.shows(Group::of(file)) && !self.source_control_group_collapsed(Group::of(file)) {
                self.source_control_selection.keys[usize::from(file.staged)].insert(file.path.clone());
            }
        }
        self.source_control_selection.anchor = self
            .source_control_files
            .get(self.source_control_selected)
            .filter(|file| self.sc_visibility.shows(Group::of(file)))
            .map(key);
    }

    pub(crate) fn update_source_control_files(&mut self, files: Vec<GitFileChange>) {
        self.source_control_selected = self.source_control_selection.reconcile(
            &self.source_control_files, self.source_control_selected, &files, self.sc_visibility);
        self.source_control_files = files;
    }

    pub(crate) fn request_source_control_discard(
        &mut self,
        mut changes: Vec<GitFileChange>,
        all: bool,
    ) {
        if !self.source_control_enabled { return; }
        changes.sort_by(|a, b| a.path.cmp(&b.path).then(b.staged.cmp(&a.staged)));
        changes.dedup_by(|a, b| a.path == b.path);
        if changes.is_empty() {
            return;
        }
        if self.bg_git_action_running || self.bg_git_remote_running {
            self.notify("Another git operation is already running");
            return;
        }
        self.sc_discard_request = Some(DiscardRequest {
            root: self.source_control_root().to_owned(),
            changes,
            all,
        });
        self.sc_discard_confirm_active = true;
        self.modal = crate::ui_modal::ModalState::default();
    }

    pub(crate) fn request_source_control_discard_all(&mut self) {
        let changes = self
            .source_control_files
            .iter()
            .filter(|file| !file.staged)
            .cloned()
            .collect();
        self.request_source_control_discard(changes, true);
    }

    pub(crate) fn execute_source_control_discard_confirm(&mut self, confirmed: bool) {
        let request = self.sc_discard_request.take();
        self.sc_discard_confirm_active = false;
        if !confirmed {
            return;
        }
        if let Some(request) = request {
            if !self.current_repository_matches(&request.root) {
                self.notify("Workspace changed; discard cancelled");
                return;
            }
            self.git_discard_changes(request.changes);
        }
    }
}

#[cfg(test)]
#[path = "app_source_control_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "app_source_control_enabled_tests.rs"]
mod enabled_tests;
