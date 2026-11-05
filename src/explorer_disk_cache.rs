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
    metadata: bool,
    order: Option<Projection>,
    read_at: Instant,
    last_used: u64,
}

struct Projection {
    sort: ExplorerSortMode,
    locale: String,
    indices: Arc<Vec<usize>>,
}

/// Different worker projections can sort the same immutable directory data
/// without copying paths or changing another in-flight request's ordering.
pub(super) struct OrderedListing {
    entries: Arc<Vec<DirectoryEntry>>,
    indices: Arc<Vec<usize>>,
}
impl OrderedListing {
    pub(super) fn iter(&self) -> impl Iterator<Item = &DirectoryEntry> {
        self.indices.iter().map(|&index| &self.entries[index])
    }
}

fn needs_metadata(sort: ExplorerSortMode) -> bool {
    matches!(sort, ExplorerSortMode::Size | ExplorerSortMode::Modified)
}

#[derive(Default)]
struct State {
    generation: u64,
    clock: u64,
    watched: bool,
    canonical: Option<(PathBuf, PathBuf)>,
    directories: FxHashMap<PathBuf, Listing>,
    entries: usize,
    #[cfg(test)]
    reads: usize,
}

#[derive(Clone, Default)]
pub(super) struct DirectoryCache(Arc<Mutex<State>>);

impl DirectoryCache {
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
        state.directories.retain(|directory, listing| {
            let affected = parents.contains(directory.as_path())
                || directory.ancestors().any(|ancestor| paths.contains(ancestor));
            if !structural {
                if affected {
                    // Content writes invalidate size/date keys, not names.
                    listing.metadata = false;
                    if listing.order.as_ref().is_some_and(|order| needs_metadata(order.sort)) {
                        listing.order = None;
                    }
                }
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

    pub(super) fn contains_path(&self, path: &Path, _sort: ExplorerSortMode) -> bool {
        let Some(parent) = path.parent() else {
            return false;
        };
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state
            .directories
            .get(parent)
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
        let key = directory.to_path_buf();
        let generation = {
            let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            state.clock = state.clock.wrapping_add(1);
            let used = state.clock;
            let watched = state.watched && watcher_covers_directory;
            if let Some(listing) = state.directories.get_mut(&key) {
                let ttl = if watched { WATCHED_TTL } else { UNWATCHED_TTL };
                if listing.read_at.elapsed() < ttl && (!needs_metadata(sort) || listing.metadata) {
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
                    metadata: needs_metadata(sort),
                    order: None,
                    read_at: Instant::now(),
                    last_used,
                },
            );
            state.entries += entries.len();
        }
        Some(entries)
    }

    pub(super) fn get_ordered(
        &self,
        directory: &Path,
        sort: ExplorerSortMode,
        ordering: &crate::file_order::FileOrder,
        watcher_covers_directory: bool,
        cancel: Option<&crate::background::CancellationToken>,
        read: impl FnOnce() -> Option<Vec<DirectoryEntry>>,
    ) -> Option<OrderedListing> {
        let entries = self.get_or_read(directory, sort, watcher_covers_directory, cancel, read)?;
        {
            let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(listing) = state.directories.get(directory)
                && Arc::ptr_eq(&listing.entries, &entries)
                && let Some(order) = &listing.order
                && order.sort == sort && order.locale == ordering.locale() {
                return Some(OrderedListing { entries, indices: order.indices.clone() });
            }
        }
        let mut indices: Vec<_> = (0..entries.len()).collect();
        indices.sort_by(|&a, &b| {
            let (_, a_dir, a_size, a_time, a_name) = &entries[a];
            let (_, b_dir, b_size, b_time, b_name) = &entries[b];
            b_dir.cmp(a_dir).then_with(|| {
                let name = || ordering.names(a_name, b_name);
                match sort {
                    ExplorerSortMode::Name => name(),
                    ExplorerSortMode::Size => b_size.cmp(a_size).then_with(name),
                    ExplorerSortMode::Modified => b_time.cmp(a_time).then_with(name),
                    ExplorerSortMode::Type => if *a_dir { name() } else { ordering.extensions(a_name, b_name) },
                }
            })
        });
        if cancel.is_some_and(|token| token.is_cancelled()) { return None; }
        let indices = Arc::new(indices);
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(listing) = state.directories.get_mut(directory)
            && Arc::ptr_eq(&listing.entries, &entries)
            && (!needs_metadata(sort) || listing.metadata) {
            listing.order = Some(Projection { sort, locale: ordering.locale().to_owned(), indices: indices.clone() });
        }
        Some(OrderedListing { entries, indices })
    }

    #[cfg(test)]
    pub(super) fn read_count(&self) -> usize {
        self.0.lock().unwrap().reads
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(directory: &Path) -> Vec<DirectoryEntry> {
        [("z.h", 1, 3), ("õ.h", 3, 1), ("a.rs", 2, 2)].into_iter()
            .map(|(name, size, modified)| (directory.join(name), false, size,
                SystemTime::UNIX_EPOCH + Duration::from_secs(modified), name.to_owned())).collect()
    }
    fn names(listing: &OrderedListing) -> Vec<&str> {
        listing.iter().map(|entry| entry.4.as_str()).collect()
    }

    #[test]
    fn sort_and_locale_projections_share_one_snapshot_and_keep_inflight_ordering() {
        let cache = DirectoryCache::default();
        let directory = Path::new("workspace");
        let en = crate::file_order::FileOrder::new("en-US").unwrap();
        let et = crate::file_order::FileOrder::new("et-EE").unwrap();
        let read = |sort, locale: &crate::file_order::FileOrder| {
            cache.get_ordered(directory, sort, locale, true, None, || Some(sample(directory))).unwrap()
        };
        let original = read(ExplorerSortMode::Name, &en);
        let again = read(ExplorerSortMode::Name, &en);
        assert!(Arc::ptr_eq(&original.indices, &again.indices), "unchanged projections do not sort again");
        assert_eq!(names(&read(ExplorerSortMode::Type, &en)), ["õ.h", "z.h", "a.rs"]);
        assert_eq!(names(&read(ExplorerSortMode::Name, &et)), ["a.rs", "z.h", "õ.h"]);
        assert_eq!(names(&original), ["a.rs", "õ.h", "z.h"], "a new locale cannot mutate an old worker's result");
        assert_eq!(cache.read_count(), 1, "name/type/locale changes need no directory reread");
        assert_eq!(names(&read(ExplorerSortMode::Modified, &et)), ["z.h", "a.rs", "õ.h"]);
        assert_eq!(names(&read(ExplorerSortMode::Size, &et)), ["õ.h", "a.rs", "z.h"]);
        assert_eq!(names(&read(ExplorerSortMode::Name, &en)), ["a.rs", "õ.h", "z.h"]);
        assert_eq!(cache.read_count(), 2, "size and date share one metadata snapshot");
    }

    #[test]
    fn content_invalidation_preserves_names_but_reloads_metadata_before_sorting() {
        let cache = DirectoryCache::default();
        let directory = Path::new("workspace");
        let order = crate::file_order::FileOrder::new("en-US").unwrap();
        cache.get_ordered(directory, ExplorerSortMode::Size, &order, true, None, || Some(sample(directory))).unwrap();
        cache.invalidate_paths([directory.join("a.rs").as_path()], false);
        cache.get_ordered(directory, ExplorerSortMode::Name, &order, true, None,
            || panic!("content-only writes do not change filenames")).unwrap();
        let modified = cache.get_ordered(directory, ExplorerSortMode::Modified, &order, true, None, || {
            let mut entries = sample(directory);
            entries[2].3 += Duration::from_secs(20);
            Some(entries)
        }).unwrap();
        assert_eq!(names(&modified)[0], "a.rs");
        assert_eq!(cache.read_count(), 2);
        cache.invalidate_paths([directory.join("a.rs").as_path()], true);
        cache.get_ordered(directory, ExplorerSortMode::Type, &order, true, None, || Some(sample(directory))).unwrap();
        assert_eq!(cache.read_count(), 3, "structure changes still require a fresh directory listing");
    }

    #[test]
    fn cancelled_or_invalidated_projections_cannot_publish_a_stale_order() {
        let cache = DirectoryCache::default();
        let directory = Path::new("workspace");
        let ordering = crate::file_order::FileOrder::new("en-US").unwrap();
        cache.get_ordered(directory, ExplorerSortMode::Name, &ordering, true, None, || {
            cache.invalidate_paths([directory], true);
            Some(sample(directory))
        }).unwrap();
        assert!(cache.0.lock().unwrap().directories.is_empty());
        let token = crate::background::CancellationToken::new();
        assert!(cache.get_ordered(directory, ExplorerSortMode::Name, &ordering, true, Some(&token), || {
            token.cancel();
            Some(sample(directory))
        }).is_none());
        assert!(cache.0.lock().unwrap().directories.is_empty());
    }

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
