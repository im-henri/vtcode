//! Sparse highlighting coordinates. A newline edit splits an ordered index and
//! offsets its suffix root; descendants keep their coordinates until visited.
//! Slots stay stable across shifts and rotations. A small direct-mapped lookup
//! cache keeps repeated viewport reads constant-time between coordinate edits.

use std::cell::Cell;
#[cfg(test)]
use std::ops::Index;
use std::ops::{Bound, RangeBounds};

type Link = Option<usize>;

struct Node<T> {
    line: usize,
    value: T,
    priority: u64,
    left: Link,
    right: Link,
    shift: i128,
}

#[derive(Clone, Copy)]
struct Lookup {
    line: usize,
    slot: usize,
    epoch: u64,
}

pub(super) struct LineIndex<T, const HOT: usize = 128> {
    nodes: Vec<Option<Node<T>>>,
    free: Vec<usize>,
    root: Link,
    sequence: u64,
    epoch: u64,
    hot: [Cell<Lookup>; HOT],
    #[cfg(test)]
    visits: Cell<usize>,
}

impl<T, const HOT: usize> Default for LineIndex<T, HOT> {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            free: Vec::new(),
            root: None,
            sequence: 0,
            epoch: 1,
            hot: std::array::from_fn(|_| {
                Cell::new(Lookup {
                    line: 0,
                    slot: 0,
                    epoch: 0,
                })
            }),
            #[cfg(test)]
            visits: Cell::new(0),
        }
    }
}

impl<T, const HOT: usize> LineIndex<T, HOT> {
    pub(super) fn len(&self) -> usize {
        self.nodes.len() - self.free.len()
    }
    pub(super) fn is_empty(&self) -> bool {
        self.root.is_none()
    }

    fn node(&self, slot: usize) -> &Node<T> {
        #[cfg(test)]
        self.visits.set(self.visits.get() + 1);
        self.nodes[slot].as_ref().unwrap()
    }
    fn node_mut(&mut self, slot: usize) -> &mut Node<T> {
        self.nodes[slot].as_mut().unwrap()
    }

    fn invalidate_lookups(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            for lookup in &self.hot {
                lookup.set(Lookup {
                    line: 0,
                    slot: 0,
                    epoch: 0,
                });
            }
            self.epoch = 1;
        }
    }

    fn find(&self, line: usize) -> Link {
        if HOT > 0 {
            let lookup = self.hot[line % HOT].get();
            if lookup.epoch == self.epoch && lookup.line == line {
                return Some(lookup.slot);
            }
        }
        let mut root = self.root;
        let mut shift = 0;
        while let Some(slot) = root {
            let node = self.node(slot);
            let key = node.line as i128 + shift;
            if key == line as i128 {
                if HOT > 0 {
                    self.hot[line % HOT].set(Lookup {
                        line,
                        slot,
                        epoch: self.epoch,
                    });
                }
                return Some(slot);
            }
            shift += node.shift;
            root = if key > line as i128 {
                node.left
            } else {
                node.right
            };
        }
        None
    }

    pub(super) fn get(&self, line: &usize) -> Option<&T> {
        self.find(*line).map(|slot| &self.node(slot).value)
    }

    pub(super) fn get_mut(&mut self, line: &usize) -> Option<&mut T> {
        let slot = self.find(*line)?;
        Some(&mut self.node_mut(slot).value)
    }

    fn offset(&mut self, root: Link, shift: i128) {
        if let Some(slot) = root {
            let node = self.node_mut(slot);
            node.line = usize::try_from(node.line as i128 + shift).expect("bounded line shift");
            node.shift += shift;
        }
    }

    fn push(&mut self, slot: usize) {
        let node = self.node_mut(slot);
        let (left, right, shift) = (node.left, node.right, std::mem::take(&mut node.shift));
        if shift != 0 {
            self.offset(left, shift);
            self.offset(right, shift);
        }
    }

    /// Partition into keys <= `line` and keys > `line`. i128 bounds handle
    /// deletions past zero and edits at usize::MAX without wrapping.
    fn split(&mut self, root: Link, line: i128) -> (Link, Link) {
        let Some(slot) = root else {
            return (None, None);
        };
        self.push(slot);
        if self.node(slot).line as i128 <= line {
            let (left, right) = self.split(self.node(slot).right, line);
            self.node_mut(slot).right = left;
            (Some(slot), right)
        } else {
            let (left, right) = self.split(self.node(slot).left, line);
            self.node_mut(slot).left = right;
            (left, Some(slot))
        }
    }

    fn merge(&mut self, left: Link, right: Link) -> Link {
        let (Some(a), Some(b)) = (left, right) else {
            return left.or(right);
        };
        if self.node(a).priority > self.node(b).priority {
            self.push(a);
            let joined = self.merge(self.node(a).right, right);
            self.node_mut(a).right = joined;
            Some(a)
        } else {
            self.push(b);
            let joined = self.merge(left, self.node(b).left);
            self.node_mut(b).left = joined;
            Some(b)
        }
    }

    pub(super) fn insert(&mut self, line: usize, value: T) -> Option<T> {
        if let Some(previous) = self.get_mut(&line) {
            return Some(std::mem::replace(previous, value));
        }
        // SplitMix64 priorities keep sorted worker batches balanced, without
        // an RNG, clock reads, hashing document text, or per-node allocations.
        self.sequence = self.sequence.wrapping_add(0x9e3779b97f4a7c15);
        let mut priority = self.sequence;
        priority = (priority ^ (priority >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        priority = (priority ^ (priority >> 27)).wrapping_mul(0x94d049bb133111eb);
        priority ^= priority >> 31;
        let node = Node {
            line,
            value,
            priority,
            left: None,
            right: None,
            shift: 0,
        };
        let slot = if let Some(slot) = self.free.pop() {
            self.nodes[slot] = Some(node);
            slot
        } else {
            self.nodes.push(Some(node));
            // Deleting a range must not allocate a separate list of its rows.
            if self.free.capacity() < self.nodes.len() {
                self.free.reserve(self.nodes.len() - self.free.len());
            }
            self.nodes.len() - 1
        };
        let (left, right) = self.split(self.root, line as i128);
        let left = self.merge(left, Some(slot));
        self.root = self.merge(left, right);
        None
    }

    fn release(&mut self, root: Link, removed: &mut impl FnMut(T)) {
        if let Some(slot) = root {
            let node = self.nodes[slot].take().unwrap();
            self.release(node.left, removed);
            self.release(node.right, removed);
            self.free.push(slot);
            removed(node.value);
        }
    }

    pub(super) fn remove(&mut self, line: &usize) -> Option<T> {
        self.find(*line)?;
        let (left, right) = self.split(self.root, *line as i128);
        let (left, removed) = self.split(left, *line as i128 - 1);
        let slot = removed.unwrap();
        let node = self.nodes[slot].take().unwrap();
        debug_assert!(node.left.is_none() && node.right.is_none());
        self.free.push(slot);
        self.root = self.merge(left, right);
        self.invalidate_lookups();
        Some(node.value)
    }

    /// Keep the prefix through `line`, discard deleted/out-of-bounds suffix
    /// entries, and move the surviving suffix as one subtree. Work is expected
    /// O(log n + removed entries), independent of the number of retained rows.
    pub(super) fn shift_after(&mut self, line: usize, delta: i128, last: usize) {
        self.shift_after_with_removed(line, delta, last, drop);
    }

    /// Account for released values while visiting only discarded nodes. Lazy
    /// shifts still leave all surviving rows and their allocations untouched.
    pub(super) fn shift_after_with_removed(&mut self, line: usize, delta: i128, last: usize,
        mut removed: impl FnMut(T))
    {
        if delta == 0 {
            return;
        }
        let (left, right) = self.split(self.root, line as i128);
        let (deleted, right) = self.split(right, line as i128 - delta);
        let (right, past_end) = self.split(right, last as i128 - delta);
        self.release(deleted, &mut removed);
        self.release(past_end, &mut removed);
        self.offset(right, delta);
        self.root = self.merge(left, right);
        self.invalidate_lookups();
    }

    pub(super) fn clear(&mut self) {
        self.nodes.clear();
        self.free.clear();
        self.root = None;
        self.invalidate_lookups();
    }

    pub(super) fn last_before_where(
        &self,
        last: usize,
        mut predicate: impl FnMut(usize, &T) -> bool,
    ) -> Option<(usize, &T)> {
        fn visit<'a, T, const HOT: usize>(
            index: &'a LineIndex<T, HOT>,
            root: Link,
            shift: i128,
            last: usize,
            predicate: &mut impl FnMut(usize, &T) -> bool,
        ) -> Option<(usize, &'a T)> {
            let node = index.node(root?);
            let line = (node.line as i128 + shift) as usize;
            let child_shift = shift + node.shift;
            if line <= last {
                if let Some(found) = visit(index, node.right, child_shift, last, predicate) {
                    return Some(found);
                }
                if predicate(line, &node.value) {
                    return Some((line, &node.value));
                }
            }
            visit(index, node.left, child_shift, last, predicate)
        }
        visit(self, self.root, 0, last, &mut predicate)
    }

    pub(super) fn for_each(
        &self,
        range: impl RangeBounds<usize>,
        mut visit: impl FnMut(usize, &T),
    ) {
        let (start, end) = bounds(range);
        self.visit(self.root, 0, start, end, &mut visit);
    }

    fn visit(
        &self,
        root: Link,
        shift: i128,
        start: i128,
        end: i128,
        visit: &mut impl FnMut(usize, &T),
    ) {
        let Some(slot) = root else {
            return;
        };
        let node = self.node(slot);
        let line = node.line as i128 + shift;
        let child_shift = shift + node.shift;
        if line > start {
            self.visit(node.left, child_shift, start, end, visit);
        }
        if line >= start && line <= end {
            visit(line as usize, &node.value);
        }
        if line < end {
            self.visit(node.right, child_shift, start, end, visit);
        }
    }

    pub(super) fn for_each_mut(
        &mut self,
        range: impl RangeBounds<usize>,
        mut visit: impl FnMut(usize, &mut T),
    ) {
        let (start, end) = bounds(range);
        self.visit_mut(self.root, start, end, &mut visit);
    }

    fn visit_mut(
        &mut self,
        root: Link,
        start: i128,
        end: i128,
        visit: &mut impl FnMut(usize, &mut T),
    ) {
        let Some(slot) = root else {
            return;
        };
        self.push(slot);
        let node = self.node(slot);
        let (line, left, right) = (node.line as i128, node.left, node.right);
        if line > start {
            self.visit_mut(left, start, end, visit);
        }
        if line >= start && line <= end {
            visit(line as usize, &mut self.node_mut(slot).value);
        }
        if line < end {
            self.visit_mut(right, start, end, visit);
        }
    }
}

fn bounds(range: impl RangeBounds<usize>) -> (i128, i128) {
    let start = match range.start_bound() {
        Bound::Included(&line) => line as i128,
        Bound::Excluded(&line) => line as i128 + 1,
        Bound::Unbounded => 0,
    };
    let end = match range.end_bound() {
        Bound::Included(&line) => line as i128,
        Bound::Excluded(&line) => line as i128 - 1,
        Bound::Unbounded => usize::MAX as i128,
    };
    (start, end)
}

#[cfg(test)]
impl<T, const HOT: usize> Index<&usize> for LineIndex<T, HOT> {
    type Output = T;
    fn index(&self, line: &usize) -> &T {
        self.get(line).unwrap()
    }
}

#[cfg(test)]
#[path = "highlight_line_index_tests.rs"]
mod tests;
