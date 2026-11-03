//! Park passive filesystem/UI services without suspending live user jobs.
use crate::app::App;
use std::path::{Path, PathBuf};
use rustc_hash::{FxHashMap, FxHashSet};

#[derive(Default)]
pub(super) struct DeferredWork {
    explorer_full: bool,
    explorer_tree: bool,
    explorer_structure: bool,
    explorer_paths: FxHashMap<PathBuf, Option<bool>>,
    status: Option<Status>,
    branch: bool,
    lines: bool,
    history: bool,
    more_history: bool,
}

struct Status {
    paths: Option<FxHashSet<PathBuf>>,
    passive: bool,
}

pub(crate) enum GitWork { Branch, Lines, History, MoreHistory }

impl App {
    pub(crate) fn defer_explorer_refresh(&mut self, full: bool) -> bool {
        if self.workspace_visible { return false; }
        let work = &mut self.workspace_observation.work;
        work.explorer_full |= full;
        work.explorer_tree = true;
        if full { work.explorer_paths.clear(); }
        true
    }

    pub(crate) fn defer_explorer_paths<'a>(&mut self,
        paths: impl IntoIterator<Item = (&'a Path, Option<bool>)>, structural: bool) {
        let work = &mut self.workspace_observation.work;
        if work.explorer_full { return; }
        work.explorer_structure |= structural;
        for (path, is_dir) in paths {
            work.explorer_paths.insert(path.to_path_buf(), is_dir);
            if work.explorer_paths.len() > 1024 {
                work.explorer_paths.clear();
                work.explorer_full = true;
                break;
            }
        }
    }

    pub(crate) fn defer_git_status(&mut self, paths: Option<&FxHashSet<PathBuf>>, passive: bool) -> bool {
        if self.workspace_visible { return false; }
        let pending = self.workspace_observation.work.status.get_or_insert_with(|| Status {
            paths: Some(FxHashSet::default()), passive: true,
        });
        pending.passive &= passive;
        match (&mut pending.paths, paths) {
            (Some(pending), Some(paths)) => for path in paths {
                pending.insert(path.clone());
                if pending.len() > 1024 { break; }
            },
            (pending, _) => *pending = None,
        }
        if pending.paths.as_ref().is_some_and(|paths| paths.len() > 1024) { pending.paths = None; }
        true
    }

    pub(crate) fn defer_git_work(&mut self, kind: GitWork) -> bool {
        if self.workspace_visible { return false; }
        let work = &mut self.workspace_observation.work;
        match kind {
            GitWork::Branch => work.branch = true,
            GitWork::Lines => work.lines = true,
            GitWork::History => work.history = true,
            GitWork::MoreHistory => work.more_history = true,
        }
        true
    }

    pub(crate) fn start_workspace_watcher(&mut self) {
        if !self.workspace_visible {
            return;
        }
        self.workspace_observation.started = true;
        if let Some(remote) = &self.remote_workspace {
            let _ = remote.client.notify(crate::remote::Command::Watch);
        } else {
            self.background
                .start_file_watcher(self.explorer.root.clone());
        }
    }

    pub(crate) fn start_visible_workspace_services(&mut self) {
        if !self.workspace_visible {
            return;
        }
        if !self.workspace_observation.assets_started {
            self.workspace_observation.assets_started = true;
            self.request_external_themes_async();
            self.request_syntax_assets_async();
        }
        self.start_workspace_watcher();
    }

    pub(super) fn suspend_workspace_observation(&mut self) {
        self.workspace_observation.file_checks.cancel();
        self.workspace_observation.file_checks = crate::background::CancellationToken::new();
        self.background.pause_file_watcher();
        if let Some(remote) = &self.remote_workspace {
            let _ = remote.client.notify(crate::remote::Command::PauseWatch);
        }
        self.completion_index.set_workspace_suspended(true);
        self.lsp_manager.retain_document_feedback(None, None);
        self.lsp_manager.retain_document_highlight(None);
        self.lsp_manager.cancel_completion_resolve();
        // Finite reads already requested can finish. Preserve their results and
        // sleeping Git history cursors; starting new passive work stays gated.
    }

    pub(super) fn resume_workspace_observation(&mut self) {
        self.completion_index.set_workspace_suspended(false);
        if self.workspace_observation.started {
            self.start_visible_workspace_services();
        }
        let check_all = std::mem::take(&mut self.workspace_observation.check_all_files);
        let pending = std::mem::take(&mut self.workspace_observation.pending);
        let mut work = std::mem::take(&mut self.workspace_observation.work);
        if pending.rescan || pending.structure || pending.git || !pending.paths.is_empty() {
            self.apply_workspace_file_changes(self.explorer.root.clone(), pending.paths.into_values().collect(),
                pending.structure, pending.rescan, pending.git);
        }
        let checks = std::mem::take(&mut self.workspace_observation.checks);
        if !pending.rescan {
            let checks = if check_all {
                self.tabs.iter().chain(self.groups.iter().flat_map(|group| &group.tabs))
                    .filter(|tab| tab.kind == crate::app::TabKind::File)
                    .filter_map(|tab| tab.editor.file_path.clone()).collect::<FxHashSet<_>>()
            } else { checks };
            for path in checks { self.queue_workspace_file_check(path); }
        }
        // A reader finishing in the background, a search, or an agent's known
        // path edit must not invalidate unrelated files, Git history and caches.
        if !pending.rescan {
            if work.explorer_full { self.refresh_explorer_async(); }
            else {
                if !work.explorer_paths.is_empty() {
                    self.refresh_explorer_changed_path_states(work.explorer_paths.iter()
                        .map(|(path, is_dir)| (path.as_path(), *is_dir)), work.explorer_structure);
                }
                if work.explorer_tree { self.refresh_explorer_tree_async(); }
            }
        }
        if let Some(mut status) = work.status {
            let metadata = self.absorb_git_watcher_refresh(&mut status.paths);
            work.branch |= metadata;
            work.history |= metadata;
            self.start_source_control_status_refresh_with_feedback(status.paths, status.passive);
        }
        if work.history { self.refresh_git_log_async(); }
        else if work.more_history { self.load_more_git_history(); }
        if work.branch { self.refresh_git_branch_async(); }
        if work.lines { self.refresh_git_line_status_async(); }
        self.sync_diff_tab();
        if let Some(path) = self.pending_git_compare_path.clone() {
            self.start_pending_git_compare_for_path(&path);
        }
        self.refresh_compare_diff();
    }

    pub(crate) fn workspace_file_check_token(&self) -> crate::background::CancellationToken {
        self.workspace_observation.file_checks.clone()
    }

    pub(crate) fn defer_workspace_file_check(&mut self, path: &std::path::Path) -> bool {
        if self.workspace_visible { return false; }
        let observation = &mut self.workspace_observation;
        if !observation.check_all_files {
            observation.checks.insert(path.to_path_buf());
            if observation.checks.len() > 1024 {
                observation.checks.clear();
                observation.check_all_files = true;
            }
        }
        true
    }
}
