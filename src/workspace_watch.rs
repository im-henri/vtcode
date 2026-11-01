//! Shared watcher batching and registration policy for desktop and headless.
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::io;
#[path = "workspace_watch_noise.rs"]
mod noise;
pub(crate) use noise::Filter as NoiseFilter;

/// Prune only storage which the event classifier already excludes. Ordinary
/// project directories keep coverage regardless of names or search settings.
pub(crate) fn directory_is_relevant(root: &Path, path: &Path, git: &[PathBuf]) -> bool {
    // Explicit subscriptions win over pruning, including the ancestors
    // needed to reach a Git directory inside another store's private tree.
    if root.starts_with(path) || git.iter().any(|directory| directory.starts_with(path)) {
        return true;
    }
    if let Ok(relative) = path.strip_prefix(root) {
        let mut parts = relative.components();
        if !parts.by_ref().any(|part| part.as_os_str() == ".git") { return true; }
        if !git.iter().any(|directory| path.starts_with(directory)) {
            return !parts.next().is_some_and(|part| matches!(part.as_os_str().to_str(), Some("objects" | "logs")));
        }
    }
    for directory in git.iter().rev() {
        if let Ok(relative) = path.strip_prefix(directory) {
            return !relative.components().next().is_some_and(|part|
                matches!(part.as_os_str().to_str(), Some("objects" | "logs")));
        }
    }
    true
}

pub(crate) fn native_watcher(
    handler: impl notify::EventHandler,
    config: notify::Config,
    directory_filter: impl Fn(&Path) -> bool + Send + Sync + 'static,
) -> notify::Result<notify::RecommendedWatcher> {
    #[cfg(target_os = "linux")]
    {
        notify::INotifyWatcher::new_with_event_batching_and_directory_filter(
            handler, config, std::time::Duration::from_millis(10), directory_filter)
    }
    #[cfg(not(target_os = "linux"))]
    {
        use notify::Watcher;
        let _ = directory_filter;
        notify::RecommendedWatcher::new(handler, config)
    }
}

const MAX_BURST_PATHS: usize = 1024;

/// Retain exact file changes while small. Large output bursts invalidate their
/// affected directories, without turning a busy build into lost watch coverage.
#[derive(Default)]
pub(crate) struct Burst {
    pub(crate) event: crate::background::WorkspaceEventBurst,
    pub(crate) paths: rustc_hash::FxHashSet<PathBuf>,
    metadata: rustc_hash::FxHashSet<PathBuf>,
    data: rustc_hash::FxHashSet<PathBuf>,
    git_paths: rustc_hash::FxHashSet<PathBuf>,
    subtrees: rustc_hash::FxHashSet<PathBuf>,
}
impl Burst {
    pub(crate) fn push_with_dependencies(&mut self, root: &Path, event: notify::Result<notify::Event>, dependencies: &[PathBuf]) {
        let had_paths = event.as_ref().is_ok_and(|event| !event.paths.is_empty());
        let event = event.map(|mut event| {
            event.paths.retain(|path| {
                if path.starts_with(root) { return true; }
                let Some(relative) = dependencies.iter().rev().find_map(|directory| path.strip_prefix(directory).ok())
                    // Retired subscriptions can still have queued events.
                    // They are not project changes in the current scope.
                    else { return false };
                if relative.as_os_str().is_empty() && matches!(event.kind,
                    notify::EventKind::Remove(_) | notify::EventKind::Modify(notify::event::ModifyKind::Name(_))) {
                    self.event.rescan = true;
                    self.event.watcher_failed = true;
                }
                let mut classified = crate::background::WorkspaceEventBurst::default();
                classified.push(root, Ok(notify::Event::new(event.kind).add_path(root.join(".git").join(relative))));
                if classified.git_metadata_changed {
                    self.event.git_metadata_changed = true;
                    self.git_paths.insert(path.clone());
                    let metadata = matches!(event.kind,
                        notify::EventKind::Modify(notify::event::ModifyKind::Metadata(_)));
                    if metadata && !self.data.contains(path) { self.metadata.insert(path.clone()); }
                    else if !metadata { self.data.insert(path.clone()); self.metadata.remove(path); }
                }
                false
            });
            event
        });
        // Consuming a Git-only event is not a native event with no paths;
        // the latter signals lost coverage and requires a rescan.
        if had_paths && !self.event.rescan && self.git_paths.len() <= MAX_BURST_PATHS
            && event.as_ref().is_ok_and(|event| event.paths.is_empty() && !event.need_rescan()) { return; }
        self.push(root, event);
    }
    pub(crate) fn push(&mut self, root: &Path, event: notify::Result<notify::Event>) {
        let metadata = event.as_ref().is_ok_and(|event| matches!(event.kind,
            notify::EventKind::Modify(notify::event::ModifyKind::Metadata(_))));
        let mut index = self.event.paths.len();
        let git_paths = &mut self.git_paths;
        let meta = &mut self.metadata;
        let data = &mut self.data;
        self.event.push_observing_git(root, event, |path| {
            git_paths.insert(path.to_owned());
            if metadata && !data.contains(path) { meta.insert(path.to_owned()); }
            else if !metadata { data.insert(path.to_owned()); meta.remove(path); }
        });
        self.event.rescan |= self.git_paths.len() > MAX_BURST_PATHS;
        if self.event.rescan {
            self.clear_paths();
            return;
        }
        while index < self.event.paths.len() {
            let path = &self.event.paths[index];
            if !Self::exact_invalidation(path)
                && path.ancestors().any(|ancestor| self.subtrees.contains(ancestor))
            {
                self.event.paths.swap_remove(index);
                continue;
            }
            if metadata && !self.data.contains(path) { self.metadata.insert(path.clone()); }
            else if !metadata { self.data.insert(path.clone()); self.metadata.remove(path); }
            if self.paths.contains(path) {
                self.event.paths.swap_remove(index);
            } else {
                self.paths.insert(path.clone());
                index += 1;
            }
            if self.paths.len() > MAX_BURST_PATHS {
                self.compact_paths(root);
                break;
            }
        }
    }

    fn clear_paths(&mut self) {
        self.event.paths.clear();
        self.paths.clear();
        self.metadata.clear();
        self.data.clear();
        self.git_paths.clear();
        self.subtrees.clear();
    }

    fn exact_invalidation(path: &Path) -> bool {
        path.file_name().is_some_and(|name| matches!(name.to_str(),
            Some(".git" | ".gitignore" | ".gitattributes" | ".gitmodules")))
    }

    fn compact_paths(&mut self, root: &Path) {
        // First combine siblings. If changes span thousands of output folders,
        // combine those by workspace subtree. Isolated source edits stay exact.
        for top_level in [false, true] {
            let mut groups = rustc_hash::FxHashMap::<PathBuf, usize>::default();
            for path in &self.event.paths {
                if Self::exact_invalidation(path) { continue; }
                let Ok(relative) = path.strip_prefix(root) else { continue; };
                let parent = if top_level {
                    relative.components().next().map(|part| root.join(part.as_os_str()))
                } else { path.parent().filter(|parent| *parent != root).map(Path::to_path_buf) };
                if let Some(parent) = parent.filter(|parent| parent.starts_with(root) && parent != root) {
                    *groups.entry(parent).or_default() += 1;
                }
            }
            let combined: Vec<_> = groups.into_iter().filter_map(|(path, count)| (count > 1).then_some(path)).collect();
            self.subtrees.extend(combined.iter().cloned());
            self.event.paths.retain(|path| Self::exact_invalidation(path)
                || !path.ancestors().any(|ancestor| self.subtrees.contains(ancestor)));
            self.event.paths.extend(self.subtrees.iter().filter(|path| {
                !path.ancestors().skip(1).any(|ancestor| self.subtrees.contains(ancestor))
            }).cloned());
            self.event.paths.sort_unstable();
            self.event.paths.dedup();
            if self.event.paths.len() <= MAX_BURST_PATHS { break; }
        }
        if self.event.paths.len() > MAX_BURST_PATHS {
            // Changes spread across the entire root, or genuine lost events,
            // still require conservative workspace-wide recovery.
            self.event.rescan = true;
            self.clear_paths();
            return;
        }
        self.event.structure_changed = true;
        self.paths = self.event.paths.iter().cloned().collect();
        self.subtrees.retain(|path| self.paths.contains(path));
        // Isolated paths in a large native event may not have reached the
        // deduplication loop yet. Forward them conservatively as data changes.
        self.metadata.retain(|path| self.git_paths.contains(path));
        self.data.retain(|path| self.git_paths.contains(path));
        self.data.extend(self.paths.iter().filter(|path| !self.subtrees.contains(*path)).cloned());
    }
}


#[cfg(test)]
#[path = "workspace_watch_tests.rs"]
mod tests;
