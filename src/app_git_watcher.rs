//! Filesystem bursts must not turn a running build into a continuous Git scan.
use crate::app::App;
use rustc_hash::FxHashSet;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const QUIET: Duration = Duration::from_millis(600);
const MAX_DELAY: Duration = Duration::from_secs(2);
const MAX_PATHS: usize = 8_192;

#[derive(Default)]
pub(crate) struct WatcherRefresh {
    pending: Option<Pending>,
}

struct Pending {
    root: PathBuf,
    first: Instant,
    last: Instant,
    // None is an authoritative refresh (metadata, overflow, or lost coverage).
    paths: Option<FxHashSet<PathBuf>>,
    refs_changed: bool,
}

impl WatcherRefresh {
    pub(crate) fn pending_for(&self, root: &std::path::Path) -> bool {
        self.pending.as_ref().is_some_and(|pending| pending.root == root)
    }

    #[cfg(test)]
    fn queue(&mut self, root: PathBuf, paths: Option<FxHashSet<PathBuf>>, now: Instant) {
        let refs_changed = paths.is_none();
        self.queue_with_refs(root, paths, refs_changed, now);
    }

    fn queue_with_refs(&mut self, root: PathBuf, paths: Option<FxHashSet<PathBuf>>, refs_changed: bool, now: Instant) {
        if paths.as_ref().is_some_and(FxHashSet::is_empty) {
            return;
        }
        if let Some(pending) = self.pending.as_mut().filter(|pending| pending.root == root) {
            pending.last = now;
            pending.refs_changed |= refs_changed;
            merge(&mut pending.paths, paths);
        } else {
            self.pending = Some(Pending {
                root,
                first: now,
                last: now,
                paths,
                refs_changed,
            });
        }
        if let Some(pending) = &mut self.pending
            && let Some(paths) = &mut pending.paths
        {
            // Widen only overflowing scopes, rather than turning thousands of
            // ignored object names into a full scan of every repository.
            while paths.len() > MAX_PATHS {
                *paths = std::mem::take(paths)
                    .into_iter()
                    .map(|path| {
                        path.parent()
                            .filter(|parent| parent.starts_with(&pending.root))
                            .unwrap_or(&pending.root)
                            .to_owned()
                    })
                    .collect();
            }
        }
    }

    fn take_refresh(&mut self, root: &std::path::Path) -> Option<Pending> {
        self.pending
            .take()
            .filter(|pending| pending.root == root)
    }

    #[cfg(test)]
    fn take(&mut self, root: &std::path::Path) -> Option<Option<FxHashSet<PathBuf>>> {
        self.take_refresh(root).map(|pending| pending.paths)
    }
}

fn merge(target: &mut Option<FxHashSet<PathBuf>>, paths: Option<FxHashSet<PathBuf>>) {
    match (target, paths) {
        (Some(target), Some(paths)) => target.extend(paths),
        (target, _) => *target = None,
    }
}

impl App {
    pub(crate) fn refresh_source_control_from_watcher(
        &mut self,
        paths: FxHashSet<PathBuf>,
        metadata: bool,
    ) {
        if !self.source_control_enabled {
            return;
        }
        // Open-file diffs react immediately; status lists can wait for a quiet
        // interval. Saves, Git actions and explicit refreshes bypass this queue.
        if metadata {
            self.refresh_source_control_diff_git_changed(&paths);
        } else {
            self.refresh_source_control_diffs_for_paths(Some(&paths));
        }
        // Writing the index changes staged rows, not commits or ahead/behind.
        // Unknown metadata and overflows retain the authoritative refs refresh.
        let refs_changed = metadata && (paths.is_empty() || !paths.iter().all(|path|
            path.components().any(|part| part.as_os_str() == ".git")
                && path.file_name().is_some_and(|name| name == "index" || name == "index.lock")));
        self.git_watcher_refresh.queue_with_refs(
            self.explorer.root.clone(),
            (!metadata).then_some(paths),
            refs_changed,
            Instant::now(),
        );
    }

    pub(crate) fn git_watcher_refresh_wait(&self, now: Instant) -> Option<Duration> {
        if !self.workspace_visible || !self.source_control_enabled || self.bg_git_status_running {
            return None;
        }
        let pending = self.git_watcher_refresh.pending.as_ref()?;
        if pending.root != self.explorer.root {
            return None;
        }
        Some(
            (pending.last + QUIET)
                .min(pending.first + MAX_DELAY)
                .saturating_duration_since(now),
        )
    }

    pub(crate) fn poll_git_watcher_refresh(&mut self, now: Instant) -> bool {
        if self.git_watcher_refresh_wait(now) != Some(Duration::ZERO) {
            return false;
        }
        let Some(pending) = self.git_watcher_refresh.take_refresh(&self.explorer.root) else {
            return false;
        };
        let passive = self.source_control_repository_state
            == crate::app::SourceControlRepositoryState::Available;
        self.start_source_control_status_refresh_with_feedback(pending.paths, passive);
        if pending.refs_changed {
            self.refresh_git_log_async();
            self.refresh_git_branch_async();
        }
        true
    }

    pub(crate) fn absorb_git_watcher_refresh(
        &mut self,
        paths: &mut Option<FxHashSet<PathBuf>>,
    ) -> bool {
        if let Some(pending) = self.git_watcher_refresh.take_refresh(&self.explorer.root) {
            merge(paths, pending.paths);
            return pending.refs_changed;
        }
        false
    }
}

#[cfg(test)]
#[path = "app_git_watcher_tests.rs"]
mod tests;
