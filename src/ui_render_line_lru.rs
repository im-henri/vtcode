use rustc_hash::FxHashMap;

// Stable slots let repeated viewport hits update recency without searching or
// shifting the other decoded lines. The owner enforces its byte/entry budget.
pub(crate) struct LineLru<T> {
    indices: FxHashMap<usize, usize>,
    slots: Vec<Option<Entry<T>>>,
    free: Vec<usize>,
    oldest: Option<usize>,
    newest: Option<usize>,
}

struct Entry<T> {
    key: usize,
    value: T,
    previous: Option<usize>,
    next: Option<usize>,
}

impl<T> Default for LineLru<T> {
    fn default() -> Self {
        Self {
            indices: FxHashMap::default(),
            slots: Vec::new(),
            free: Vec::new(),
            oldest: None,
            newest: None,
        }
    }
}

impl<T> LineLru<T> {
    pub(crate) fn len(&self) -> usize {
        self.indices.len()
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    pub(crate) fn get(&self, key: usize) -> Option<&T> {
        let slot = *self.indices.get(&key)?;
        Some(&self.slots[slot].as_ref().unwrap().value)
    }

    fn unlink(&mut self, slot: usize) {
        let entry = self.slots[slot].as_ref().unwrap();
        let (previous, next) = (entry.previous, entry.next);
        if let Some(previous) = previous {
            self.slots[previous].as_mut().unwrap().next = next;
        } else {
            self.oldest = next;
        }
        if let Some(next) = next {
            self.slots[next].as_mut().unwrap().previous = previous;
        } else {
            self.newest = previous;
        }
    }

    fn link_newest(&mut self, slot: usize) {
        let entry = self.slots[slot].as_mut().unwrap();
        entry.previous = self.newest;
        entry.next = None;
        if let Some(previous) = self.newest {
            self.slots[previous].as_mut().unwrap().next = Some(slot);
        } else {
            self.oldest = Some(slot);
        }
        self.newest = Some(slot);
    }

    pub(crate) fn touch(&mut self, key: usize) {
        if let Some(&slot) = self.indices.get(&key)
            && self.newest != Some(slot)
        {
            self.unlink(slot);
            self.link_newest(slot);
        }
    }

    /// A single lookup for warm rows, with storage from the oldest row passed
    /// to the builder on eviction. Entry-limited caches can reuse allocations.
    pub(crate) fn get_or_insert_recycled(&mut self, key: usize, limit: usize,
        build: impl FnOnce(Option<T>) -> T) -> &T
    {
        self.get_or_insert_recycled_mut(key, limit, build)
    }

    /// Updating an existing generation retains its slot and backing storage.
    pub(crate) fn get_or_insert_recycled_mut(&mut self, key: usize, limit: usize,
        build: impl FnOnce(Option<T>) -> T) -> &mut T
    {
        debug_assert!(limit > 0);
        let slot = if let Some(&slot) = self.indices.get(&key) { slot } else {
            let recycled = if self.len() >= limit { self.pop_front().map(|(_, value)| value) } else { None };
            self.push_back((key, build(recycled)));
            self.indices[&key]
        };
        if self.newest != Some(slot) {
            self.unlink(slot);
            self.link_newest(slot);
        }
        &mut self.slots[slot].as_mut().unwrap().value
    }

    pub(crate) fn remove(&mut self, key: usize) -> Option<T> {
        let slot = self.indices.remove(&key)?;
        self.unlink(slot);
        let entry = self.slots[slot].take().unwrap();
        self.free.push(slot);
        Some(entry.value)
    }

    pub(crate) fn push_back(&mut self, (key, value): (usize, T)) {
        self.remove(key);
        let slot = self.free.pop().unwrap_or_else(|| {
            self.slots.push(None);
            self.slots.len() - 1
        });
        self.slots[slot] = Some(Entry {
            key,
            value,
            previous: None,
            next: None,
        });
        self.indices.insert(key, slot);
        self.link_newest(slot);
    }

    pub(crate) fn pop_front(&mut self) -> Option<(usize, T)> {
        let key = self.slots[self.oldest?].as_ref().unwrap().key;
        Some((key, self.remove(key).unwrap()))
    }

    #[cfg(test)]
    pub(crate) fn front(&self) -> Option<(&usize, &T)> {
        let entry = self.slots[self.oldest?].as_ref().unwrap();
        Some((&entry.key, &entry.value))
    }

    pub(crate) fn retain(&mut self, mut keep: impl FnMut((&usize, &T)) -> bool) {
        for slot in 0..self.slots.len() {
            if let Some(entry) = &self.slots[slot]
                && !keep((&entry.key, &entry.value))
            {
                self.remove(entry.key);
            }
        }
    }

    pub(crate) fn clear(&mut self) {
        self.indices.clear();
        self.slots.clear();
        self.free.clear();
        self.oldest = None;
        self.newest = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn indexed_recency_matches_queue_through_eviction_removal_and_reuse() {
        let mut cache = LineLru::default();
        let mut reference = VecDeque::new();
        let mut random = 7u64;
        for step in 0..20_000 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let key = ((random >> 32) % 37) as usize;
            match random % 9 {
                0..=3 => {
                    cache.push_back((key, step));
                    reference.retain(|&(k, _)| k != key);
                    reference.push_back((key, step));
                }
                4 => {
                    cache.touch(key);
                    if let Some(index) = reference.iter().position(|&(k, _)| k == key) {
                        let entry = reference.remove(index).unwrap();
                        reference.push_back(entry);
                    }
                }
                5 => {
                    let removed = reference
                        .iter()
                        .position(|&(k, _)| k == key)
                        .and_then(|index| reference.remove(index))
                        .map(|(_, value)| value);
                    assert_eq!(cache.remove(key), removed);
                }
                6 => assert_eq!(cache.pop_front(), reference.pop_front()),
                7 => {
                    cache.retain(|(k, _)| k % 3 != key % 3);
                    reference.retain(|(k, _)| k % 3 != key % 3);
                }
                _ if step % 100 == 0 => {
                    cache.clear();
                    reference.clear();
                }
                _ => {}
            }
            assert_eq!(cache.len(), reference.len());
            assert_eq!(
                cache.front().map(|(&k, &v)| (k, v)),
                reference.front().copied()
            );
            for key in 0..37 {
                assert_eq!(
                    cache.get(key).copied(),
                    reference.iter().find(|&&(k, _)| k == key).map(|&(_, v)| v)
                );
            }
            assert!(cache.slots.len() <= 37, "removed slots must be reused");
        }
        assert_eq!(
            std::iter::from_fn(|| cache.pop_front()).collect::<Vec<_>>(),
            reference.into_iter().collect::<Vec<_>>()
        );
    }
}
