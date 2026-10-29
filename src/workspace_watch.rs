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

/// Repeated notifications for one hot file are one change, not an overflow
/// requiring a workspace rescan. Once overflowed, retain flags, not stat work.
#[derive(Default)]
pub(crate) struct Burst {
    pub(crate) event: crate::background::WorkspaceEventBurst,
    pub(crate) paths: std::collections::HashSet<PathBuf>,
    metadata: std::collections::HashSet<PathBuf>,
    data: std::collections::HashSet<PathBuf>,
    git_paths: std::collections::HashSet<PathBuf>,
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
        if had_paths && self.git_paths.len() <= 1024
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
        for path in &self.event.paths[index..] {
            if metadata && !self.data.contains(path) { self.metadata.insert(path.clone()); }
            else if !metadata { self.data.insert(path.clone()); self.metadata.remove(path); }
        }
        self.event.rescan |= self.git_paths.len() > 1024;
        if self.event.rescan {
            self.event.paths.clear();
            self.paths.clear();
            self.metadata.clear();
            self.data.clear();
            self.git_paths.clear();
            return;
        }
        while index < self.event.paths.len() {
            let path = &self.event.paths[index];
            if self.paths.contains(path) {
                self.event.paths.swap_remove(index);
            } else if self.paths.len() == 1024 {
                self.event.rescan = true;
                self.event.paths.clear();
                self.paths.clear();
                break;
            } else {
                self.paths.insert(path.clone());
                index += 1;
            }
        }
    }
}


#[cfg(test)]
#[path = "workspace_watch_tests.rs"]
mod tests;
