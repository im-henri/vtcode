use super::*;

#[test]
fn cancelled_or_expired_diffs_never_publish_partial_change_markers() {
    let saved = Rope::from_str(&"before\n".repeat(1_000));
    let current = Rope::from_str(&"after\n".repeat(1_000));
    assert!(compute_unsaved_diff_markers_cancellable(&saved, &current, None, &|| true).is_none());
    assert!(
        compute_unsaved_diff_markers_cancellable(
            &saved,
            &current,
            Some(std::time::Instant::now()),
            &|| false,
        )
        .is_none()
    );
    let checks = std::cell::Cell::new(0);
    assert!(
        compute_unsaved_diff_markers_cancellable(&saved, &current, None, &|| {
            checks.set(checks.get() + 1);
            checks.get() >= 8
        })
        .is_none()
    );
    assert_eq!(checks.get(), 8);
    assert_same(&saved, &current);
    assert_eq!(
        compute_unsaved_diff_markers_cancellable(
            &saved,
            &current,
            Some(std::time::Instant::now() + std::time::Duration::from_secs(5)),
            &|| false
        )
        .unwrap(),
        reference_diff(&saved, &current)
    );
}

fn reference_diff(saved: &Rope, current: &Rope) -> UnsavedDiff {
    fn lines(rope: &Rope) -> Vec<Cow<'_, str>> {
        rope.lines()
            .map(|line| match line.as_str() {
                Some(text) => Cow::Borrowed(text),
                None => Cow::Owned(line.to_string()),
            })
            .collect()
    }

    let old_lines = lines(saved);
    let new_lines = lines(current);
    let old_line_refs: Vec<&str> = old_lines.iter().map(Cow::as_ref).collect();
    let new_line_refs: Vec<&str> = new_lines.iter().map(Cow::as_ref).collect();

    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_slices(&old_line_refs, &new_line_refs);
    let mut result = Vec::with_capacity(new_lines.len());
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => result.push(' '),
            ChangeTag::Insert => result.push('N'),
            ChangeTag::Delete => {}
        }
    }
    for op in diff.ops() {
        if let similar::DiffOp::Replace {
            new_index, new_len, ..
        } = *op
        {
            for i in new_index..(new_index + new_len) {
                if i < result.len() {
                    result[i] = 'N';
                }
            }
        }
    }
    result.resize(new_lines.len(), ' ');
    let mut changed_ranges: Vec<std::ops::Range<usize>> = Vec::new();
    for (line, &status) in result.iter().enumerate() {
        if status == 'N' {
            if let Some(last) = changed_ranges.last_mut()
                && last.end == line
            {
                last.end += 1;
            } else {
                changed_ranges.push(line..line + 1);
            }
        }
    }
    UnsavedDiff {
        line_count: result.len(),
        changed_ranges,
    }
}

fn assert_same(saved: &Rope, current: &Rope) {
    assert_eq!(crate::rope_changes::equal(saved, current), saved == current);
    let expected = reference_diff(saved, current);
    let actual = compute_unsaved_diff_markers(saved, current);
    assert_eq!(actual, expected, "old={saved}, new={current}");
}

#[test]
fn local_diff_matches_whole_document_for_shared_and_replaced_ropes() {
    for text in [
        "",
        "aaa\naaa\naaa",
        "a\r\nb\r\nc\r\n",
        "α界café\nend",
        "a\rb\rc",
        "\n\n\n",
    ] {
        let saved = Rope::from_str(text);
        assert_same(&saved, &saved);
        for from in 0..=saved.len_chars() {
            for to in from..=saved.len_chars() {
                for insert in ["", "\n", "\r", "\r\n", "界", "a\na\n", "x\ny"] {
                    let mut current = saved.clone();
                    current.remove(from..to);
                    current.insert(from, insert);
                    assert_same(&saved, &current);
                    assert_same(&saved, &Rope::from_str(&current.to_string()));
                }
            }
        }
    }
}

#[test]
fn local_diff_handles_multiple_disjoint_edits_and_repeated_lines() {
    let saved = Rope::from_str(&"a\nb\na\nb\nc\n".repeat(200));
    let mut current = saved.clone();
    let mut seed = 73u64;
    for step in 0..200 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let from = seed as usize % (current.len_chars() + 1);
        let to = (from + (seed >> 32) as usize % 10).min(current.len_chars());
        current.remove(from..to);
        current.insert(from, ["a\nb\n", "", "c", "\r\n"][step % 4]);
        assert_same(&saved, &current);
    }
}

#[test]
#[ignore = "manual optimized unsaved gutter comparison, setup excluded"]
fn benchmark_local_unsaved_diff() {
    use std::{hint::black_box, time::Instant};
    let saved = Rope::from_str(&"    let value = compute(input); // source line\n".repeat(100_000));
    let mut current = saved.clone();
    current.insert(current.line_to_char(50_000) + 10, "new_");
    let mut times = [Vec::new(), Vec::new()];
    for sample in 0..7 {
        for optimized in [sample % 2 == 0, sample % 2 != 0] {
            let start = Instant::now();
            let result = if optimized {
                compute_unsaved_diff_markers(&saved, &current)
            } else {
                reference_diff(&saved, &current)
            };
            times[usize::from(optimized)].push(start.elapsed());
            assert_eq!(black_box(result.changed_ranges), [50_000..50_001]);
        }
    }
    for samples in &mut times {
        samples.sort_unstable();
    }
    eprintln!(
        "100k-line local unsaved diff: before={:?}, after={:?}",
        times[0][3], times[1][3]
    );
}

#[test]
#[ignore = "manual optimized measurement of small edits in many-line documents"]
fn benchmark_routine_unsaved_changes() {
    use std::{hint::black_box, time::Instant};
    for lines in [2_000, 500_000] {
        let saved = Rope::from_str(&"let value = compute(input);\n".repeat(lines));
        let mut current = saved.clone();
        current.insert(current.line_to_char(lines / 2) + 4, "new_");
        black_box(compute_unsaved_diff_markers(&saved, &current));
        let mut samples = Vec::new();
        for _ in 0..7 {
            let started = Instant::now();
            for _ in 0..32 {
                black_box(compute_unsaved_diff_markers(
                    black_box(&saved),
                    black_box(&current),
                ));
            }
            samples.push(started.elapsed());
        }
        samples.sort_unstable();
        eprintln!(
            "routine unsaved changes: lines={lines}, iterations=32, median_ns={}",
            samples[3].as_nanos()
        );
    }
}
