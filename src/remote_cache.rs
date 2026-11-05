//! Saved checkpoints are an optimization, not the authority for file contents.
//! Eviction never loses edits: the client still owns its buffer and undo tree,
//! and the agent can validate a missing checkpoint against the current disk.
use super::RemotePath;
use std::collections::HashMap;

pub(super) struct Cache<T, K = RemotePath> {
    entries: HashMap<K, (u64, usize, T)>,
    bytes: usize,
    clock: u64,
    max_entries: usize,
    max_bytes: usize,
}
impl<T, K> Default for Cache<T, K> {
    fn default() -> Self {
        Self::with_limits(256, 128 * 1024 * 1024)
    }
}
impl<T, K> Cache<T, K> {
    pub(super) fn with_limits(max_entries: usize, max_bytes: usize) -> Self {
        Self { entries: HashMap::new(), bytes: 0, clock: 0, max_entries, max_bytes }
    }
}
impl<T, K: Clone + std::hash::Hash + Eq> Cache<T, K> {
    pub(super) fn get(&mut self, path: &K) -> Option<&T> {
        self.clock = self.clock.wrapping_add(1);
        let entry = self.entries.get_mut(path)?;
        entry.0 = self.clock;
        Some(&entry.2)
    }
    pub(super) fn insert(&mut self, path: K, bytes: usize, value: T) {
        self.remove(&path);
        if self.max_entries == 0 || bytes > self.max_bytes {
            return;
        }
        while self.bytes.saturating_add(bytes) > self.max_bytes || self.entries.len() >= self.max_entries {
            let Some(path) = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.0)
                .map(|(path, _)| path.clone())
            else {
                break;
            };
            self.remove(&path);
        }
        self.clock = self.clock.wrapping_add(1);
        self.bytes += bytes;
        self.entries.insert(path, (self.clock, bytes, value));
    }
    pub(super) fn remove(&mut self, path: &K) {
        let _ = self.take(path);
    }
    pub(super) fn take(&mut self, path: &K) -> Option<T> {
        self.entries.remove(path).map(|(_, bytes, value)| {
            self.bytes -= bytes;
            value
        })
    }
    pub(super) fn remove_matching(&mut self, mut matches: impl FnMut(&K) -> bool) {
        self.entries.retain(|key, (_, bytes, _)| {
            let remove = matches(key);
            if remove {
                self.bytes -= *bytes;
            }
            !remove
        });
    }
}
