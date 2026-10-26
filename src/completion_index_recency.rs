//! Worker-owned LRU metadata. Focusing a file costs O(log n), independent of
//! vocabulary size. Victim selection never scans the entire resident cache.
use std::collections::BTreeMap;

#[derive(Default)]
pub(super) struct Recency {
    clock: u64,
    stamps: BTreeMap<u64, u64>,
    order: BTreeMap<u64, u64>,
}

impl Recency {
    pub(super) fn touch(&mut self, file: u64) {
        if self.clock == u64::MAX {
            // Only reachable after 2^64 lifecycle events; retain relative order.
            let old = std::mem::take(&mut self.order);
            self.clock = 0;
            for (_, id) in old {
                self.clock += 1;
                self.stamps.insert(id, self.clock);
                self.order.insert(self.clock, id);
            }
        }
        self.clock += 1;
        if let Some(previous) = self.stamps.insert(file, self.clock) {
            self.order.remove(&previous);
        }
        self.order.insert(self.clock, file);
    }

    pub(super) fn remove(&mut self, file: u64) {
        if let Some(stamp) = self.stamps.remove(&file) {
            self.order.remove(&stamp);
        }
    }

    pub(super) fn victim(&self, active: u64, incoming: u64) -> Option<u64> {
        // At most two entries are protected, so this visits at most three.
        self.order
            .values()
            .copied()
            .find(|&id| id != active && id != incoming)
    }

    pub(super) fn memory(&self) -> usize {
        self.order.len() * 128 + usize::from(!self.order.is_empty()) * 512
    }

    pub(super) fn insertion_cost(&self) -> usize {
        128 + usize::from(self.order.is_empty()) * 512
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn least_recent_file_is_selected_without_evicting_active_or_incoming() {
        let mut cache = Recency::default();
        for id in 1..=10_000 {
            cache.touch(id);
        }
        cache.touch(1);
        assert_eq!(cache.victim(2, 3), Some(4));
        assert_eq!(cache.victim(0, 0), Some(2));
        cache.remove(2);
        assert_eq!(cache.victim(0, 0), Some(3));
        for id in 1..=10_000 {
            cache.remove(id);
        }
        assert_eq!(cache.memory(), 0);
        assert_eq!(cache.victim(0, 0), None);
    }
}
