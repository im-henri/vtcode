//! Saved checkpoints are an optimization, not the authority for file contents.
//! Eviction never loses edits: the client still owns its buffer and undo tree,
//! and the agent can validate a missing checkpoint against the current disk.
use super::RemotePath;
use std::collections::HashMap;

pub(super) struct Cache<T> {
    entries: HashMap<RemotePath, (u64, usize, T)>,
    bytes: usize,
    clock: u64,
}
impl<T> Default for Cache<T> {
    fn default() -> Self {
        Self {
            entries: HashMap::new(),
            bytes: 0,
            clock: 0,
        }
    }
}
impl<T> Cache<T> {
    pub(super) fn get(&mut self, path: &RemotePath) -> Option<&T> {
        self.clock = self.clock.wrapping_add(1);
        let entry = self.entries.get_mut(path)?;
        entry.0 = self.clock;
        Some(&entry.2)
    }
    pub(super) fn insert(&mut self, path: RemotePath, bytes: usize, value: T) {
        self.remove(&path);
        if bytes > 128 * 1024 * 1024 {
            return;
        }
        while self.bytes + bytes > 128 * 1024 * 1024 || self.entries.len() >= 256 {
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
    pub(super) fn remove(&mut self, path: &RemotePath) {
        if let Some((_, bytes, _)) = self.entries.remove(path) {
            self.bytes -= bytes;
        }
    }
    pub(super) fn remove_under(&mut self, path: &RemotePath) {
        self.entries.retain(|key, (_, bytes, _)| {
            let remove = key == path
                || (key.1 == path.1
                    && key
                        .0
                        .strip_prefix(path.0.as_slice())
                        .is_some_and(|tail| tail.starts_with(b"/")));
            if remove {
                self.bytes -= *bytes;
            }
            !remove
        });
    }
}
