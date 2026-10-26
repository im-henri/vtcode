//! Persistent ignore decorations. Workers own the expensive state while using
//! it; UI invalidation only queues paths under a short lock, without Git or I/O.
use super::{FileEntry, GitStatus, git::IgnoreForest};
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const UNWATCHED_TTL: Duration = Duration::from_secs(2);
const WATCHED_TTL: Duration = Duration::from_secs(30);
const MAX_PATHS: usize = 100_000;
const MAX_PATH_BYTES: usize = 16 * 1024 * 1024;
const MAX_INVALIDATIONS: usize = 1_024;

struct Decoration {
    is_dir: bool,
    ignored: bool,
}

struct Snapshot {
    root: PathBuf,
    canonical_root: PathBuf,
    created: Instant,
    unwatched: bool,
    // Cache unsuccessful discovery too; ordinary folders need no repeated Git I/O.
    matcher: IgnoreForest,
    paths: FxHashMap<PathBuf, Decoration>,
    path_bytes: usize,
}

impl Snapshot {
    fn new(root: &Path, canonical_root: &Path, metrics: &mut Metrics) -> Self {
        Self {
            root: root.into(),
            canonical_root: canonical_root.into(),
            created: Instant::now(),
            unwatched: false,
            matcher: IgnoreForest::new(canonical_root, metrics),
            paths: FxHashMap::default(),
            path_bytes: 0,
        }
    }

    fn invalidate_paths(&mut self, paths: FxHashSet<PathBuf>) {
        let mut subtrees = FxHashSet::default();
        let mut canonical_subtrees = FxHashSet::default();
        for changed in paths {
            let Ok(relative) = changed
                .strip_prefix(&self.root)
                .or_else(|_| changed.strip_prefix(&self.canonical_root))
            else {
                continue;
            };
            let path = self.root.join(relative);
            let canonical = self.canonical_root.join(relative);
            let ignore_file = path.file_name().is_some_and(|name| name == ".gitignore");
            let known_file = self.paths.get(&path).is_some_and(|entry| !entry.is_dir);
            if ignore_file || !known_file {
                // A directory move/replacement can remove nested ignore rules.
                // Unknown paths are treated conservatively, without stat calls.
                subtrees.insert(if ignore_file {
                    path.parent().unwrap().to_owned()
                } else {
                    path
                });
                canonical_subtrees.insert(if ignore_file {
                    canonical.parent().unwrap().to_owned()
                } else {
                    canonical
                });
            }
            // Replacing/deleting a known file does not change path-based rules
            // or the Git index. Keep its result; a file/directory type change is
            // checked on lookup, and Git metadata has separate invalidation.
        }
        if !subtrees.is_empty() {
            // Scan once per watcher burst, not once per changed path.
            self.paths
                .retain(|path, _| !path.ancestors().any(|p| subtrees.contains(p)));
            self.path_bytes = self
                .paths
                .keys()
                .map(|p| p.as_os_str().as_encoded_bytes().len())
                .sum();
            self.matcher.invalidate_directories(&canonical_subtrees);
        }
    }

    fn mark(
        &mut self,
        entries: &mut [FileEntry],
        cancel: Option<&crate::background::CancellationToken>,
        source_control_cancel: &crate::background::CancellationToken,
        metrics: &mut Metrics,
    ) {
        // Index is large and not Send. Read it lazily, at most once per batch,
        // only when an uncached ignored path needs a tracked-file exception.
        let mut indices = FxHashMap::default();
        for entry in entries {
            if source_control_cancel.is_cancelled() || cancel.is_some_and(|cancel| cancel.is_cancelled()) {
                break;
            }
            let ignored = if let Some(cached) = self
                .paths
                .get(&entry.path)
                .filter(|cached| cached.is_dir == entry.is_dir)
            {
                metrics.hit();
                cached.ignored
            } else {
                if !self.unwatched {
                    self.unwatched = entry.path.parent().is_some_and(|parent| {
                        !crate::background::workspace_path_is_watched(&self.root, parent)
                    });
                }
                let ignored = self.matcher.ignored(
                    &self.root,
                    &self.canonical_root,
                    entry,
                    &mut indices,
                    metrics,
                );
                if let Some(ignored) = ignored {
                    self.remember(
                        &entry.path,
                        Decoration {
                            is_dir: entry.is_dir,
                            ignored,
                        },
                    );
                }
                ignored.unwrap_or(false)
            };
            if entry.git_status_summary.contains(GitStatus::Ignored) != ignored {
                entry.git_status_summary.0 ^= super::GitStatusSummary::bit(GitStatus::Ignored);
                entry.git_status = entry.git_status_summary.primary();
            }
        }
    }

    fn remember(&mut self, path: &Path, decoration: Decoration) {
        if let Some(cached) = self.paths.get_mut(path) {
            *cached = decoration;
            return;
        }
        let bytes = path.as_os_str().as_encoded_bytes().len();
        // Preserve warm entries when a search exceeds capacity. Clearing the
        // entire map at the limit would thrash on every subsequent large search.
        // Expiry and targeted invalidation make room without an O(n) LRU scan.
        if self.paths.len() < MAX_PATHS && self.path_bytes.saturating_add(bytes) <= MAX_PATH_BYTES {
            self.paths.insert(path.to_owned(), decoration);
            self.path_bytes += bytes;
        }
    }
}

#[derive(Default)]
struct Pending {
    all: bool,
    paths: FxHashSet<PathBuf>,
}

#[derive(Default)]
struct State {
    disabled: bool,
    cancel: crate::background::CancellationToken,
    ticket: u64,
    watched: bool,
    pending: Pending,
    snapshot: Option<Snapshot>,
    metrics: Metrics,
}

struct Checkout {
    cancel: crate::background::CancellationToken,
    ticket: u64,
    watched: bool,
    pending: Pending,
    snapshot: Option<Snapshot>,
}

#[derive(Clone, Default)]
pub(super) struct IgnoreCache(Arc<Mutex<State>>);

impl IgnoreCache {
    pub(super) fn set_enabled(&self, enabled: bool) {
        let retired = {
            let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            if state.disabled == !enabled { return; }
            state.disabled = !enabled;
            state.cancel.cancel();
            state.cancel = crate::background::CancellationToken::new();
            if !enabled { state.cancel.cancel(); }
            state.ticket = state.ticket.wrapping_add(1);
            state.pending = Pending::default();
            state.snapshot.take()
        };
        drop(retired);
    }

    pub(super) fn enabled(&self) -> bool {
        !self.0.lock().unwrap_or_else(|e| e.into_inner()).disabled
    }

    pub(super) fn invalidate(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.disabled { return; }
        state.pending.all = true;
        state.pending.paths.clear();
    }

    pub(super) fn invalidate_paths<'a>(&self, paths: impl IntoIterator<Item = &'a Path>) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.disabled || state.pending.all {
            return;
        }
        for path in paths {
            if state.pending.paths.len() == MAX_INVALIDATIONS {
                state.pending.all = true;
                state.pending.paths.clear();
                break;
            }
            state.pending.paths.insert(path.to_owned());
        }
    }

    pub(super) fn set_watched(&self, watched: bool) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.watched != watched {
            state.watched = watched;
            state.pending.all = true;
            state.pending.paths.clear();
        }
    }

    fn checkout(&self) -> Option<Checkout> {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.disabled { return None; }
        state.ticket = state.ticket.wrapping_add(1);
        Some(Checkout {
            cancel: state.cancel.clone(),
            ticket: state.ticket,
            watched: state.watched,
            pending: std::mem::take(&mut state.pending),
            snapshot: state.snapshot.take(),
        })
    }

    fn checkin(&self, ticket: u64, snapshot: Snapshot, metrics: Metrics) {
        let retired = {
            let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
            state.metrics.add(metrics);
            if state.ticket == ticket {
                state.snapshot.replace(snapshot)
            } else {
                // A newer request owns the cache; an older worker cannot replace it.
                Some(snapshot)
            }
        };
        drop(retired); // Free rules/results outside the lock used by the UI.
    }

    pub(super) fn mark_ignored(
        &self,
        root: &Path,
        canonical_root: &Path,
        entries: &mut [FileEntry],
        cancel: Option<&crate::background::CancellationToken>,
    ) {
        if entries.is_empty() || cancel.is_some_and(|cancel| cancel.is_cancelled()) {
            return;
        }
        let Some(checkout) = self.checkout() else { return; };
        if checkout.cancel.is_cancelled() { return; }
        let mut metrics = Metrics::default();
        let mut snapshot = checkout
            .snapshot
            .filter(|snapshot| {
                let ttl = if checkout.watched && !snapshot.unwatched {
                    WATCHED_TTL
                } else {
                    UNWATCHED_TTL
                };
                !checkout.pending.all
                    && snapshot.root == root
                    && snapshot.canonical_root == canonical_root
                    && snapshot.created.elapsed() < ttl
            })
            .unwrap_or_else(|| Snapshot::new(root, canonical_root, &mut metrics));
        snapshot.invalidate_paths(checkout.pending.paths);
        snapshot.mark(entries, cancel, &checkout.cancel, &mut metrics);
        // Invalidation queued while this worker ran remains pending for the next
        // checkout. Partially completed, cancelled scans still have valid cache hits.
        self.checkin(checkout.ticket, snapshot, metrics);
    }

    #[cfg(test)]
    pub(super) fn metrics(&self) -> Metrics {
        self.0.lock().unwrap().metrics
    }

    #[cfg(test)]
    pub(super) fn expire(&self) {
        if let Some(snapshot) = &mut self.0.lock().unwrap().snapshot {
            snapshot.created -= WATCHED_TTL;
        }
    }
}

// Counters are compiled out in production; tests assert absence of I/O/work,
// avoiding flaky elapsed-time thresholds in the regular test suite.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(super) struct Metrics {
    #[cfg(test)]
    pub repository_loads: usize,
    #[cfg(test)]
    pub rule_loads: usize,
    #[cfg(test)]
    pub index_loads: usize,
    #[cfg(test)]
    pub matches: usize,
    #[cfg(test)]
    pub hits: usize,
    #[cfg(test)]
    pub boundary_checks: usize,
}

impl Metrics {
    pub(super) fn boundary(&mut self) {
        #[cfg(test)]
        { self.boundary_checks += 1; }
    }
    pub(super) fn repository(&mut self) {
        #[cfg(test)]
        {
            self.repository_loads += 1;
        }
    }
    pub(super) fn rule(&mut self) {
        #[cfg(test)]
        {
            self.rule_loads += 1;
        }
    }
    pub(super) fn index(&mut self) {
        #[cfg(test)]
        {
            self.index_loads += 1;
        }
    }
    pub(super) fn matched(&mut self) {
        #[cfg(test)]
        {
            self.matches += 1;
        }
    }
    fn hit(&mut self) {
        #[cfg(test)]
        {
            self.hits += 1;
        }
    }
    fn add(&mut self, _other: Self) {
        #[cfg(test)]
        {
            self.repository_loads += _other.repository_loads;
            self.rule_loads += _other.rule_loads;
            self.index_loads += _other.index_loads;
            self.matches += _other.matches;
            self.hits += _other.hits;
            self.boundary_checks += _other.boundary_checks;
        }
    }
}

#[cfg(test)]
#[path = "explorer_git_cache_tests.rs"]
mod tests;
