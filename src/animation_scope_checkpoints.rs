//! Worker-side reuse and interval lookup for structural scope analysis.

use ropey::Rope;

const PREFIX_POLL_CHUNKS: usize = 128;
const BUILD_POLL_ENTRIES: usize = 4_096;

/// Find an unchanged prefix without decoding or hashing its text. Both Rope
/// snapshots remain alive while their shared chunk pointers are compared.
/// Stopping at the first non-shared chunk is conservative: a copied boundary
/// chunk may contain more unchanged text, but that text must be parsed again.
pub(crate) fn shared_prefix_chars(
    old: &Rope,
    new: &Rope,
    mut cancelled: impl FnMut() -> bool,
) -> Option<usize> {
    if cancelled() {
        return None;
    }
    if old.is_instance(new) {
        return Some(old.len_chars());
    }
    let mut shared_bytes = 0;
    let mut shared_chunks: usize = 0;
    for (left, right) in old.chunks().zip(new.chunks()) {
        if left.as_ptr() != right.as_ptr() || left.len() != right.len() {
            break;
        }
        shared_bytes += left.len();
        shared_chunks += 1;
        if shared_chunks.is_multiple_of(PREFIX_POLL_CHUNKS) {
            if cancelled() {
                return None;
            }
            std::thread::yield_now();
        }
    }
    if cancelled() {
        return None;
    }
    // Chunk boundaries are UTF-8 boundaries. Rope metadata performs this
    // conversion once, rather than decoding every shared chunk to count chars.
    Some(old.byte_to_char(shared_bytes))
}

/// Select the last checkpoint at or before the first potentially changed
/// character. Checkpoints must be ordered by their source offsets.
pub(crate) fn checkpoint_before<T>(
    checkpoints: &[T],
    unchanged_prefix: usize,
    offset: impl Fn(&T) -> usize,
) -> Option<usize> {
    checkpoints
        .partition_point(|point| offset(point) <= unchanged_prefix)
        .checked_sub(1)
}

/// Maxima over opener-ordered scope ends. An encoded end is `end + 1`; zero
/// denotes an unfinished node. The render path can find the innermost complete
/// enclosing scope without traversing an arbitrarily deep closed subtree.
pub(crate) struct MaxEndTree {
    maxima: Vec<usize>,
    leaf_count: usize,
    len: usize,
}

impl MaxEndTree {
    /// The UI fallback has a strict small source budget and needs no worker
    /// cancellation or scheduler yields while constructing its interval tree.
    pub(crate) fn build_bounded<I>(ends: I) -> Option<Self>
    where
        I: IntoIterator<Item = usize>,
        I::IntoIter: ExactSizeIterator,
    {
        let ends = ends.into_iter();
        let len = ends.len();
        if len == 0 {
            return Some(Self {
                maxima: Vec::new(),
                leaf_count: 0,
                len: 0,
            });
        }
        let leaf_count = len.checked_next_power_of_two()?;
        let mut maxima = vec![0; leaf_count.checked_mul(2)?];
        for (index, end) in ends.enumerate() {
            maxima[leaf_count + index] = end;
        }
        for index in (1..leaf_count).rev() {
            maxima[index] = maxima[index * 2].max(maxima[index * 2 + 1]);
        }
        Some(Self {
            maxima,
            leaf_count,
            len,
        })
    }

    pub(crate) fn build<I>(ends: I, mut cancelled: impl FnMut() -> bool) -> Option<Self>
    where
        I: IntoIterator<Item = usize>,
        I::IntoIter: ExactSizeIterator,
    {
        if cancelled() {
            return None;
        }
        let ends = ends.into_iter();
        let len = ends.len();
        if len == 0 {
            return Some(Self {
                maxima: Vec::new(),
                leaf_count: 0,
                len: 0,
            });
        }
        let leaf_count = len.checked_next_power_of_two()?;
        let entries = leaf_count.checked_mul(2)?;
        let mut maxima = Vec::with_capacity(entries);
        // Allocation reserves space once. Initialize in polling intervals so
        // cancellation also interrupts preparation of large sparse trees.
        while maxima.len() < entries {
            if cancelled() {
                return None;
            }
            std::thread::yield_now();
            maxima.resize(
                maxima.len().saturating_add(BUILD_POLL_ENTRIES).min(entries),
                0,
            );
        }
        for (index, end) in ends.enumerate() {
            if index.is_multiple_of(BUILD_POLL_ENTRIES) {
                if cancelled() {
                    return None;
                }
                std::thread::yield_now();
            }
            maxima[leaf_count + index] = end;
        }
        for (work, index) in (1..leaf_count).rev().enumerate() {
            if work.is_multiple_of(BUILD_POLL_ENTRIES) {
                if cancelled() {
                    return None;
                }
                std::thread::yield_now();
            }
            maxima[index] = maxima[index * 2].max(maxima[index * 2 + 1]);
        }
        if cancelled() {
            return None;
        }
        Some(Self {
            maxima,
            leaf_count,
            len,
        })
    }

    /// Return the rightmost node at or before `last_open_index` whose encoded
    /// end is at least `min_end` (the cursor offset plus one). This is O(log n),
    /// independent of source nesting depth. Empty/unfinished nodes are skipped.
    pub(crate) fn rightmost_before(&self, last_open_index: usize, min_end: usize) -> Option<usize> {
        if self.len == 0 {
            return None;
        }
        self.find_rightmost(
            1,
            0,
            self.leaf_count,
            last_open_index.min(self.len - 1),
            min_end.max(1),
        )
    }

    fn find_rightmost(
        &self,
        node: usize,
        start: usize,
        end: usize,
        last: usize,
        min_end: usize,
    ) -> Option<usize> {
        if start > last || self.maxima[node] < min_end {
            return None;
        }
        if end - start == 1 {
            return Some(start);
        }
        let middle = start + (end - start) / 2;
        if last >= middle
            && let Some(found) = self.find_rightmost(node * 2 + 1, middle, end, last, min_end)
        {
            return Some(found);
        }
        self.find_rightmost(node * 2, start, middle, last, min_end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_prefix_uses_retained_chunk_identity_and_exact_character_offsets() {
        let old = Rope::from_str(&"🦀界 café\r\nordinary row\n".repeat(12_000));
        let unchanged = old.clone();
        assert_eq!(
            shared_prefix_chars(&old, &unchanged, || false),
            Some(old.len_chars())
        );

        for edit in [old.len_chars() / 2, old.len_chars() - 4] {
            let mut new = old.clone();
            new.insert(edit, "changed");
            let prefix = shared_prefix_chars(&old, &new, || false).unwrap();
            assert!(prefix > 0 && prefix <= edit);
            assert_eq!(old.slice(..prefix), new.slice(..prefix));
        }
        let replacement = Rope::from_str(&old.to_string());
        assert_eq!(shared_prefix_chars(&old, &replacement, || false), Some(0));
    }

    #[test]
    fn shared_prefix_handles_truncation_empty_sources_and_cancellation() {
        let old = Rope::from_str(&"retained prefix 界\n".repeat(60_000));
        let mut shorter = old.clone();
        shorter.remove(old.len_chars() / 2..);
        let prefix = shared_prefix_chars(&old, &shorter, || false).unwrap();
        assert!(prefix > 0 && prefix <= shorter.len_chars());
        assert_eq!(old.slice(..prefix), shorter.slice(..prefix));
        assert_eq!(shared_prefix_chars(&Rope::new(), &old, || false), Some(0));
        assert_eq!(shared_prefix_chars(&old, &old, || true), None);

        let mut changed = old.clone();
        changed.insert(old.len_chars() - 1, "x");
        let mut polls = 0;
        assert_eq!(
            shared_prefix_chars(&old, &changed, || {
                polls += 1;
                polls == 2
            }),
            None
        );
        assert_eq!(polls, 2);
    }

    #[test]
    fn checkpoint_predecessor_includes_the_exact_safe_boundary() {
        let points = [(0, "start"), (10, "token"), (30, "next")];
        for (prefix, expected) in [
            (0, Some(0)),
            (9, Some(0)),
            (10, Some(1)),
            (29, Some(1)),
            (30, Some(2)),
            (usize::MAX, Some(2)),
        ] {
            assert_eq!(
                checkpoint_before(&points, prefix, |point| point.0),
                expected
            );
        }
        assert_eq!(checkpoint_before(&[(5, ())], 4, |point| point.0), None);
        assert_eq!(checkpoint_before::<()>(&[], 0, |_| 0), None);
    }

    #[test]
    fn interval_queries_match_a_linear_oracle_for_all_prefixes_and_ends() {
        for count in [0, 1, 2, 3, 7, 32, 79] {
            let ends: Vec<_> = (0..count)
                .map(|index| {
                    if index % 5 == 0 {
                        0
                    } else {
                        (index * 37 + count * 11) % 97 + 1
                    }
                })
                .collect();
            let tree = MaxEndTree::build(ends.iter().copied(), || false).unwrap();
            for last in 0..count + 3 {
                for cursor in 0..110 {
                    let expected = ends
                        .iter()
                        .enumerate()
                        .take(last.saturating_add(1))
                        .filter(|(_, end)| **end >= cursor.max(1))
                        .map(|(index, _)| index)
                        .next_back();
                    assert_eq!(
                        tree.rightmost_before(last, cursor),
                        expected,
                        "count={count}, last={last}, cursor={cursor}"
                    );
                }
            }
        }
    }

    #[test]
    fn interval_queries_handle_deep_closed_subtrees_and_full_width_offsets() {
        let depth = 20_000;
        let mut ends = vec![200_001];
        ends.extend((0..depth).map(|index| 100_000 - index));
        ends.push(0);
        let tree = MaxEndTree::build(ends.iter().copied(), || false).unwrap();
        assert_eq!(tree.rightmost_before(depth + 1, 150_001), Some(0));
        assert_eq!(tree.rightmost_before(depth + 1, 250_001), None);
        let tree = MaxEndTree::build([0, usize::MAX], || false).unwrap();
        assert_eq!(tree.rightmost_before(usize::MAX, usize::MAX), Some(1));
    }

    #[test]
    fn interval_tree_construction_polls_during_zero_fill_leaf_fill_and_reduction() {
        let count: usize = 20_000;
        let init_polls = count.next_power_of_two() * 2 / BUILD_POLL_ENTRIES;
        let leaf_polls = count.div_ceil(BUILD_POLL_ENTRIES);
        for cancel_at in [1, 3, 1 + init_polls + 2, 1 + init_polls + leaf_polls + 2] {
            let mut polls = 0;
            let result = MaxEndTree::build(std::iter::repeat_n(1, count), || {
                polls += 1;
                polls == cancel_at
            });
            assert!(result.is_none(), "cancel_at={cancel_at}");
            assert_eq!(polls, cancel_at);
        }
    }

    #[test]
    #[ignore = "comparative scope prefix and interval query benchmark"]
    fn benchmark_scope_prefix_reuse_and_interval_queries() {
        use std::{
            hint::black_box,
            time::{Duration, Instant},
        };
        let old =
            Rope::from_str(&"🦀 ordinary source row with unchanged lexical state\n".repeat(80_000));
        let edit = old.len_chars() - 10;
        let mut new = old.clone();
        new.insert(edit, "§changed");
        let mut reference_time = Duration::ZERO;
        let mut shared_time = Duration::ZERO;
        for iteration in 0..12 {
            for reference in [iteration % 2 == 0, iteration % 2 != 0] {
                let start = Instant::now();
                let prefix = if reference {
                    old.chars()
                        .zip(new.chars())
                        .take_while(|(a, b)| a == b)
                        .count()
                } else {
                    shared_prefix_chars(&old, &new, || false).unwrap()
                };
                black_box(prefix);
                if reference {
                    reference_time += start.elapsed();
                    assert_eq!(prefix, edit);
                } else {
                    shared_time += start.elapsed();
                    assert!(prefix > 0 && prefix <= edit);
                }
            }
        }
        eprintln!(
            "scope prefix 12 passes: decoded={reference_time:?}, shared_chunks={shared_time:?}"
        );

        let depth = 100_000;
        let mut ends = vec![300_001];
        ends.extend((0..depth).map(|index| 200_000 - index));
        let tree = MaxEndTree::build(ends.iter().copied(), || false).unwrap();
        let mut linear_time = Duration::ZERO;
        let mut indexed_time = Duration::ZERO;
        for iteration in 0..200 {
            for reference in [iteration % 2 == 0, iteration % 2 != 0] {
                let start = Instant::now();
                let found = if reference {
                    black_box(&ends).iter().rposition(|&end| end >= 250_001)
                } else {
                    tree.rightmost_before(black_box(depth), black_box(250_001))
                };
                black_box(found);
                if reference {
                    linear_time += start.elapsed();
                } else {
                    indexed_time += start.elapsed();
                }
                assert_eq!(found, Some(0));
            }
        }
        eprintln!(
            "scope query 200 passes: closed_ancestors={linear_time:?}, max_end={indexed_time:?}"
        );
    }
}
