use super::*;

#[test]
fn a_small_edit_keeps_unsaved_storage_proportional_to_change_ranges() {
    let saved = Rope::from_str(&"a\n".repeat(500_000));
    let mut current = saved.clone();
    current.insert(current.line_to_char(250_000), "new ");
    let changes = compute_unsaved_diff_markers(&saved, &current);
    assert_eq!(changes.line_count, current.len_lines());
    assert_eq!(changes.changed_ranges, [250_000..250_001]);
    assert!(
        changes.changed_ranges.capacity() * std::mem::size_of::<std::ops::Range<usize>>() < 1_024
    );
    assert_eq!(changes.line_status(249_999), ' ');
    assert_eq!(changes.line_status(250_000), 'N');
    assert_eq!(changes.line_status(250_001), ' ');
    assert_eq!(changes.line_status(usize::MAX), ' ');
}

#[test]
fn a_large_contiguous_change_is_kept_as_one_range() {
    let saved = Rope::from_str("before\n");
    let current = Rope::from_str(&"after\n".repeat(100_000));
    let changes = compute_unsaved_diff_markers(&saved, &current);
    assert_eq!(changes.line_count, 100_001);
    assert_eq!(changes.changed_ranges, [0..100_000]);
    assert!(
        changes.changed_ranges.capacity() * std::mem::size_of::<std::ops::Range<usize>>() < 1_024
    );
    assert_eq!(changes.line_status(99_999), 'N');
    assert_eq!(changes.line_status(100_000), ' ');
}

#[test]
fn sparse_unsaved_queries_preserve_boundaries_empty_ranges_and_eof() {
    let changes = UnsavedDiff {
        line_count: 12,
        changed_ranges: vec![1..3, 5..6, 9..12],
    };
    for start in 0..=15 {
        for end in 0..=15 {
            let expected = start < end
                && (start..end.min(12)).any(|line| [1, 2, 5, 9, 10, 11].contains(&line));
            assert_eq!(
                changes.has_change_in_range(start, end),
                expected,
                "{start}..{end}"
            );
        }
    }
    assert!(changes.has_change_in_range(10, usize::MAX));
    assert!(!changes.has_change_in_range(12, usize::MAX));
    assert!(!changes.has_change_in_range(usize::MAX, usize::MAX));
    assert!(!UnsavedDiff::default().has_change_in_range(0, usize::MAX));
}

#[test]
fn single_line_diff_shortcuts_preserve_any_matching_line() {
    let saved = Rope::from_str("anchor\n");
    let expanded = Rope::from_str("new before\nanchor\nnew after\n");
    let changes = compute_unsaved_diff_markers(&saved, &expanded);
    assert_eq!(changes.changed_ranges, [0..1, 2..3]);
    let reduced = compute_unsaved_diff_markers(&expanded, &saved);
    assert!(reduced.changed_ranges.is_empty());
    assert_eq!(reduced.line_count, saved.len_lines());
}

#[test]
fn exact_diff_shortcuts_still_obey_deadlines_and_cancellation() {
    let saved = Rope::from_str("before\n");
    let current = Rope::from_str(&"after\n".repeat(10_000));
    let polls = std::cell::Cell::new(0);
    assert!(
        compute_unsaved_diff_markers_cancellable(&saved, &current, None, &|| {
            polls.set(polls.get() + 1);
            polls.get() >= 8
        })
        .is_none()
    );
    assert!(
        compute_unsaved_diff_markers_cancellable(
            &saved,
            &current,
            Some(std::time::Instant::now()),
            &|| false
        )
        .is_none()
    );
    let changes = compute_unsaved_diff_markers(&saved, &current);
    assert_eq!(changes.changed_ranges, [0..10_000]);
}
