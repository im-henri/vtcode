//! Reuse directory snapshots between Explorer projections. No filesystem work
//! happens under the cache lock; invalidation also rejects in-flight scans.
use super::ExplorerSortMode;
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

pub(super) type DirectoryEntry = (PathBuf, bool, u64, SystemTime, String);

const UNWATCHED_TTL: Duration = Duration::from_secs(2);
// Some shared filesystems accept watches but omit remote clients' changes.
// Revalidate on use even with a healthy watcher; explicit Refresh is immediate.
const WATCHED_TTL: Duration = Duration::from_secs(30);
const MAX_DIRECTORIES: usize = 1_024;
const MAX_ENTRIES: usize = 100_000;

struct Listing {
    entries: Arc<Vec<DirectoryEntry>>,
    read_at: Instant,
    last_used: u64,
}

#[derive(Default)]
struct State {
    generation: u64,
    clock: u64,
    watched: bool,
    canonical: Option<(PathBuf, PathBuf)>,
    directories: FxHashMap<(PathBuf, ExplorerSortMode), Listing>,
    entries: usize,
    #[cfg(test)]
    reads: usize,
}

#[derive(Clone, Default)]
pub(super) struct DirectoryCache(Arc<Mutex<State>>);

impl DirectoryCache {
    pub(super) fn for_new_order(&self) -> Self {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        Self(Arc::new(Mutex::new(State { watched: state.watched,
            canonical: state.canonical.clone(), ..Default::default() })))
    }
    pub(super) fn set_watched(&self, watched: bool) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.watched != watched {
            // Never extend the lifetime of snapshots built before coverage
            // started, or keep trusted snapshots after the watcher stops.
            state.generation = state.generation.wrapping_add(1);
            state.directories.clear();
            state.entries = 0;
            state.watched = watched;
        }
    }

    pub(super) fn clear(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.generation = state.generation.wrapping_add(1);
        state.directories.clear();
        state.entries = 0;
        state.canonical = None;
    }

    pub(super) fn invalidate_paths<'a>(
        &self,
        paths: impl IntoIterator<Item = &'a Path>,
        structural: bool,
    ) {
        let mut paths: FxHashSet<_> = paths.into_iter().collect();
        if paths.is_empty() {
            return;
        }
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        // Watchers may report canonical paths even when the workspace was
        // opened through an alias or a path containing parent components.
        let aliases: Vec<PathBuf> = state
            .canonical
            .as_ref()
            .filter(|(root, canonical)| root != canonical)
            .map(|(root, canonical)| {
                paths
                    .iter()
                    .filter_map(|path| {
                        path.strip_prefix(canonical)
                            .ok()
                            .map(|relative| root.join(relative))
                    })
                    .collect()
            })
            .unwrap_or_default();
        paths.extend(aliases.iter().map(PathBuf::as_path));
        let parents: FxHashSet<_> = paths.iter().filter_map(|path| path.parent()).collect();
        state.generation = state.generation.wrapping_add(1);
        state.directories.retain(|(directory, sort), _| {
            if !structural && matches!(sort, ExplorerSortMode::Name | ExplorerSortMode::Type) {
                return true;
            }
            !parents.contains(directory.as_path())
                && !directory
                    .ancestors()
                    .any(|ancestor| paths.contains(ancestor))
        });
        state.entries = state
            .directories
            .values()
            .map(|entry| entry.entries.len())
            .sum();
        if state
            .canonical
            .as_ref()
            .is_some_and(|(root, _)| paths.contains(&root.as_path()))
        {
            state.canonical = None;
        }
    }

    pub(super) fn contains_path(&self, path: &Path, sort: ExplorerSortMode) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state
            .directories
            .get(&(parent.to_path_buf(), sort))
            .is_some_and(|listing| listing.entries.iter().any(|entry| entry.0 == path))
    }

    pub(super) fn canonical_root(&self, root: &Path) -> PathBuf {
        let generation = {
            let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((path, canonical)) = &state.canonical {
                if path == root {
                    return canonical.clone();
                }
            }
            state.generation
        };
        let Ok(canonical) = std::fs::canonicalize(root) else {
            return root.to_path_buf();
        };
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.generation == generation {
            state.canonical = Some((root.to_path_buf(), canonical.clone()));
        }
        canonical
    }

    pub(super) fn get_or_read(
        &self,
        directory: &Path,
        sort: ExplorerSortMode,
        watcher_covers_directory: bool,
        cancel: Option<&crate::background::CancellationToken>,
        read: impl FnOnce() -> Option<Vec<DirectoryEntry>>,
    ) -> Option<Arc<Vec<DirectoryEntry>>> {
        if cancel.is_some_and(|token| token.is_cancelled()) {
            return None;
        }
        let key = (directory.to_path_buf(), sort);
        let generation = {
            let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            state.clock = state.clock.wrapping_add(1);
            let used = state.clock;
            let watched = state.watched && watcher_covers_directory;
            if let Some(listing) = state.directories.get_mut(&key) {
                let ttl = if watched { WATCHED_TTL } else { UNWATCHED_TTL };
                if listing.read_at.elapsed() < ttl {
                    listing.last_used = used;
                    return Some(Arc::clone(&listing.entries));
                }
            }
            if let Some(old) = state.directories.remove(&key) {
                state.entries -= old.entries.len();
            }
            #[cfg(test)]
            {
                state.reads += 1;
            }
            state.generation
        };
        let entries = Arc::new(read()?);
        if cancel.is_some_and(|token| token.is_cancelled()) {
            return None;
        }
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.generation == generation && entries.len() <= MAX_ENTRIES {
            // Evict only the least recently used snapshots, not the entire
            // workspace, when browsing exceeds the cache's memory budget.
            if let Some(old) = state.directories.remove(&key) {
                state.entries -= old.entries.len();
            }
            while state.directories.len() >= MAX_DIRECTORIES
                || state.entries.saturating_add(entries.len()) > MAX_ENTRIES
            {
                let Some(oldest) = state
                    .directories
                    .iter()
                    .min_by_key(|(_, listing)| listing.last_used)
                    .map(|(key, _)| key.clone())
                else {
                    break;
                };
                if let Some(old) = state.directories.remove(&oldest) {
                    state.entries -= old.entries.len();
                }
            }
            state.clock = state.clock.wrapping_add(1);
            let last_used = state.clock;
            state.directories.insert(
                key,
                Listing {
                    entries: Arc::clone(&entries),
                    read_at: Instant::now(),
                    last_used,
                },
            );
            state.entries += entries.len();
        }
        Some(entries)
    }

    #[cfg(test)]
    pub(super) fn read_count(&self) -> usize {
        self.0.lock().unwrap().reads
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignored_directories_expire_even_with_a_running_watcher() {
        let cache = DirectoryCache::default();
        cache.set_watched(true);
        let path = Path::new("workspace/target");
        let read = || {
            cache
                .get_or_read(path, ExplorerSortMode::Name, false, None, || {
                    Some(Vec::new())
                })
                .unwrap()
        };
        read();
        cache
            .0
            .lock()
            .unwrap()
            .directories
            .values_mut()
            .for_each(|entry| entry.read_at -= UNWATCHED_TTL);
        read();
        assert_eq!(cache.read_count(), 2);
    }

    #[test]
    fn watched_directories_eventually_revalidate_missed_remote_changes() {
        let cache = DirectoryCache::default();
        cache.set_watched(true);
        let path = Path::new("workspace");
        let read = || {
            cache
                .get_or_read(
                    path,
                    ExplorerSortMode::Name,
                    true,
                    None,
                    || Some(Vec::new()),
                )
                .unwrap()
        };
        read();
        cache
            .0
            .lock()
            .unwrap()
            .directories
            .values_mut()
            .for_each(|entry| entry.read_at -= WATCHED_TTL);
        read();
        assert_eq!(cache.read_count(), 2);
    }

    #[test]
    fn failed_or_cancelled_reads_are_not_cached() {
        let cache = DirectoryCache::default();
        let path = Path::new("workspace");
        assert!(cache
            .get_or_read(path, ExplorerSortMode::Name, true, None, || None)
            .is_none());
        let token = crate::background::CancellationToken::new();
        assert!(cache
            .get_or_read(path, ExplorerSortMode::Name, true, Some(&token), || {
                token.cancel();
                Some(Vec::new())
            })
            .is_none());
        assert!(cache
            .get_or_read(path, ExplorerSortMode::Name, true, Some(&token), || panic!(
                "cancelled scan did I/O"
            ))
            .is_none());
        cache
            .get_or_read(
                path,
                ExplorerSortMode::Name,
                true,
                None,
                || Some(Vec::new()),
            )
            .unwrap();
        assert_eq!(cache.read_count(), 3);
    }

    #[test]
    fn directory_cache_has_a_fixed_capacity() {
        let cache = DirectoryCache::default();
        for index in 0..=MAX_DIRECTORIES {
            cache
                .get_or_read(
                    &PathBuf::from(format!("folder-{index}")),
                    ExplorerSortMode::Name,
                    true,
                    None,
                    || Some(Vec::new()),
                )
                .unwrap();
        }
        assert!(cache.0.lock().unwrap().directories.len() <= MAX_DIRECTORIES);
    }

    #[test]
    fn capacity_eviction_preserves_recently_used_directories() {
        let cache = DirectoryCache::default();
        cache.set_watched(true);
        let read = |path: &Path| {
            cache
                .get_or_read(
                    path,
                    ExplorerSortMode::Name,
                    true,
                    None,
                    || Some(Vec::new()),
                )
                .unwrap();
        };
        for index in 0..MAX_DIRECTORIES {
            read(&PathBuf::from(format!("folder-{index}")));
        }
        read(Path::new("folder-0"));
        read(Path::new("extra"));
        let count = cache.read_count();
        read(Path::new("folder-0"));
        assert_eq!(cache.read_count(), count);
        read(Path::new("folder-1"));
        assert_eq!(cache.read_count(), count + 1);
    }

    #[test]
    fn invalidation_during_a_read_cannot_install_a_stale_listing() {
        let cache = DirectoryCache::default();
        let path = Path::new("workspace");
        cache
            .get_or_read(path, ExplorerSortMode::Name, true, None, || {
                cache.invalidate_paths([path], true);
                Some(Vec::new())
            })
            .unwrap();
        cache
            .get_or_read(
                path,
                ExplorerSortMode::Name,
                true,
                None,
                || Some(Vec::new()),
            )
            .unwrap();
        assert_eq!(cache.read_count(), 2);
    }

    #[test]
    fn expiry_watcher_transitions_and_explicit_refresh_revalidate_listings() {
        let cache = DirectoryCache::default();
        let path = Path::new("workspace");
        let read = || {
            cache
                .get_or_read(
                    path,
                    ExplorerSortMode::Name,
                    true,
                    None,
                    || Some(Vec::new()),
                )
                .unwrap();
        };
        read();
        read();
        assert_eq!(cache.read_count(), 1);
        cache
            .0
            .lock()
            .unwrap()
            .directories
            .values_mut()
            .for_each(|entry| entry.read_at -= UNWATCHED_TTL);
        read();
        assert_eq!(cache.read_count(), 2);
        cache.set_watched(true);
        read();
        cache
            .0
            .lock()
            .unwrap()
            .directories
            .values_mut()
            .for_each(|entry| entry.read_at -= UNWATCHED_TTL);
        read();
        assert_eq!(cache.read_count(), 3);
        cache.set_watched(false);
        read();
        cache.clear();
        read();
        assert_eq!(cache.read_count(), 5);
    }
}
