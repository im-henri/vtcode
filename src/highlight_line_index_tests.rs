use super::*;
use std::collections::BTreeMap;

#[test]
fn shifted_index_reports_each_discarded_value_once_without_visiting_survivors() {
    let mut index = LineIndex::<usize>::default();
    for line in 0..1000 { index.insert(line, line * 2); }
    let mut removed = Vec::new();
    index.shift_after_with_removed(100, -20, 979, |value| removed.push(value));
    removed.sort_unstable();
    assert_eq!(removed, (101..=120).map(|line| line * 2).collect::<Vec<_>>());
    assert_eq!(index.len(), 980);
    assert_eq!(index.get(&101), Some(&242));
    removed.clear();
    index.shift_after_with_removed(100, 50, 1020, |value| removed.push(value));
    removed.sort_unstable();
    assert_eq!(removed, (991..1000).map(|line| line * 2).collect::<Vec<_>>());
    assert_eq!(index.len(), 971);
}

fn contents<const HOT: usize>(index: &LineIndex<u64, HOT>) -> BTreeMap<usize, u64> {
    let mut contents = BTreeMap::new();
    index.for_each(.., |line, &value| {
        contents.insert(line, value);
    });
    contents
}

fn check_tree<T, const HOT: usize>(index: &LineIndex<T, HOT>) -> usize {
    fn visit<T, const HOT: usize>(
        index: &LineIndex<T, HOT>,
        root: Link,
        shift: i128,
        low: i128,
        high: i128,
        priority: u64,
        seen: &mut [bool],
    ) -> (usize, usize) {
        let Some(slot) = root else {
            return (0, 0);
        };
        assert!(
            !std::mem::replace(&mut seen[slot], true),
            "cycle/repeated slot"
        );
        let node = index.node(slot);
        let line = node.line as i128 + shift;
        assert!(line > low && line < high, "unsorted coordinate {line}");
        assert!(node.priority <= priority, "unbalanced priorities");
        let (left, a) = visit(
            index,
            node.left,
            shift + node.shift,
            low,
            line,
            node.priority,
            seen,
        );
        let (right, b) = visit(
            index,
            node.right,
            shift + node.shift,
            line,
            high,
            node.priority,
            seen,
        );
        (1 + left + right, 1 + a.max(b))
    }
    let mut seen = vec![false; index.nodes.len()];
    let (count, height) = visit(
        index,
        index.root,
        0,
        -1,
        usize::MAX as i128 + 1,
        u64::MAX,
        &mut seen,
    );
    assert_eq!(count, index.len());
    for &slot in &index.free {
        assert!(
            !std::mem::replace(&mut seen[slot], true),
            "duplicate free slot"
        );
        assert!(index.nodes[slot].is_none());
    }
    assert!(seen.into_iter().all(|used| used));
    height
}

fn random_operations<const HOT: usize>() {
    let mut index = LineIndex::<u64, HOT>::default();
    let mut expected = BTreeMap::new();
    let mut seed = 4321u64;
    let mut next = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        seed >> 32
    };
    for step in 0..12_000 {
        let line = (next() % 700) as usize;
        match next() % 9 {
            0..=2 => {
                assert_eq!(index.insert(line, step), expected.insert(line, step));
            }
            3 => {
                assert_eq!(index.remove(&line), expected.remove(&line));
            }
            4 => {
                if let Some(value) = index.get_mut(&line) {
                    *value ^= 99;
                }
                if let Some(value) = expected.get_mut(&line) {
                    *value ^= 99;
                }
            }
            5 => {
                let delta = (next() % 121) as i128 - 60;
                let last = line + (next() % 900) as usize;
                index.shift_after(line, delta, last);
                if delta != 0 {
                    expected = expected
                        .into_iter()
                        .filter_map(|(at, value)| {
                            if at <= line {
                                Some((at, value))
                            } else {
                                let shifted = at as i128 + delta;
                                (shifted > line as i128 && shifted <= last as i128)
                                    .then_some((shifted as usize, value))
                            }
                        })
                        .collect();
                }
            }
            6 => {
                let end = line + (next() % 100) as usize;
                index.for_each_mut(line..end, |at, value| {
                    *value ^= at as u64;
                });
                for (&at, value) in expected.range_mut(line..end) {
                    *value ^= at as u64;
                }
            }
            7 => {
                let divisor = next() % 7 + 1;
                let actual = index
                    .last_before_where(line, |_, value| value % divisor == 0)
                    .map(|(at, &value)| (at, value));
                let wanted = expected
                    .range(..=line)
                    .rev()
                    .find(|(_, value)| *value % divisor == 0)
                    .map(|(&at, &value)| (at, value));
                assert_eq!(actual, wanted);
            }
            _ => {
                assert_eq!(index.get(&line), expected.get(&line));
            }
        }
        assert_eq!(index.len(), expected.len());
        if step % 37 == 0 {
            assert_eq!(contents(&index), expected, "step {step}");
            assert!(check_tree(&index) < 64);
            for at in 0..800 {
                assert_eq!(index.get(&at), expected.get(&at), "step {step}, row {at}");
            }
        }
    }
    assert_eq!(contents(&index), expected);
    index.clear();
    assert!(index.is_empty());
    assert_eq!(index.len(), 0);
    assert!(index.get(&0).is_none());
    index.insert(0, 42);
    assert_eq!(index.get(&0), Some(&42));
    check_tree(&index);
}

#[test]
fn randomized_coordinates_match_ordered_maps_with_and_without_warm_lookups() {
    random_operations::<0>();
    random_operations::<128>();
}

#[test]
fn range_boundaries_pending_shifts_and_extreme_coordinates_are_exact() {
    let mut index = LineIndex::<u64>::default();
    for line in [0, 1, 10, 20, 30, usize::MAX - 1, usize::MAX] {
        index.insert(line, line as u64);
    }
    index.shift_after(1, -8, usize::MAX);
    assert_eq!(index.get(&2), Some(&10));
    index.shift_after(1, 8, usize::MAX);
    assert_eq!(index.get(&10), Some(&10));
    for (start, end) in [(0, 0), (0, 1), (1, 20), (20, 10), (usize::MAX, usize::MAX)] {
        let mut actual = Vec::new();
        index.for_each((Bound::Included(start), Bound::Included(end)), |line, _| {
            actual.push(line)
        });
        let expected: Vec<_> = contents(&index)
            .keys()
            .filter(|&&line| line >= start && line <= end)
            .copied()
            .collect();
        assert_eq!(actual, expected);
    }
    let mut actual = Vec::new();
    index.for_each(
        (Bound::Excluded(usize::MAX), Bound::Unbounded),
        |line, _| actual.push(line),
    );
    index.for_each(..0, |line, _| actual.push(line));
    assert!(actual.is_empty());
    index.shift_after(usize::MAX - 1, 1, usize::MAX);
    assert!(index.get(&usize::MAX).is_none());
    assert_eq!(
        index.get(&(usize::MAX - 1)),
        Some(&((usize::MAX - 1) as u64))
    );
    check_tree(&index);
}

#[test]
fn warm_slots_survive_rotations_but_never_survive_coordinate_changes_or_reuse() {
    let mut index = LineIndex::<u64, 8>::default();
    for line in 0..100 {
        index.insert(line, line as u64);
    }
    for line in 92..100 {
        assert_eq!(index.get(&line), Some(&(line as u64)));
    }
    for line in 100..1_000 {
        index.insert(line, line as u64);
    }
    assert_eq!(index.get(&99), Some(&99));
    index.insert(99, 9999);
    assert_eq!(index.get(&99), Some(&9999));
    index.shift_after(98, 3, 1_003);
    assert_eq!(index.get(&99), None);
    assert_eq!(index.get(&102), Some(&9999));
    assert_eq!(index.remove(&102), Some(9999));
    index.insert(99, 123);
    assert_eq!(index.get(&102), None);
    assert_eq!(index.get(&99), Some(&123));
    index.epoch = u64::MAX;
    assert_eq!(index.get(&99), Some(&123));
    index.shift_after(98, -1, 1_002);
    assert_eq!(index.get(&99), None);
    check_tree(&index);
}

#[test]
fn repeated_newlines_visit_only_tree_paths_and_reuse_storage() {
    let mut index = LineIndex::<u64>::default();
    for line in 0..40_000 {
        index.insert(line * 2, line as u64);
    }
    let nodes = index.nodes.as_ptr();
    let free = index.free.as_ptr();
    let capacity = index.nodes.len();
    assert!(check_tree(&index) < 64);
    for _ in 0..500 {
        index.visits.set(0);
        index.shift_after(30_000, 1, 80_001);
        assert!(
            index.visits.get() < 500,
            "insertion walked {} nodes",
            index.visits.get()
        );
        index.visits.set(0);
        index.shift_after(30_000, -1, 80_000);
        assert!(
            index.visits.get() < 500,
            "deletion walked {} nodes",
            index.visits.get()
        );
    }
    assert_eq!(index.nodes.as_ptr(), nodes);
    assert_eq!(index.free.as_ptr(), free);
    for line in 1..=2_000 {
        index.remove(&(line * 2));
    }
    for line in 1..=2_000 {
        index.insert(line * 2, line as u64);
    }
    assert_eq!(index.nodes.len(), capacity);
    assert_eq!(index.nodes.as_ptr(), nodes);
    assert_eq!(index.free.as_ptr(), free);
    for line in 0..40_000 {
        assert_eq!(index.get(&(line * 2)), Some(&(line as u64)));
    }
    check_tree(&index);
}
