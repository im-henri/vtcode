use super::*;
use std::{cell::Cell, time::Duration};

fn analyze(cache: &mut Cache, rope: &Rope) -> Metrics {
    let actual = cache
        .analyze(1, rope, Instant::now() + Duration::from_secs(20), &|| false)
        .unwrap();
    assert_eq!(actual, super::super::analyze_editor_content(rope));
    actual
}

#[test]
fn repeated_identical_newlines_do_not_accumulate_partial_blocks() {
    let mut rope = Rope::from_str(&"value\n".repeat(2_000));
    let mut cache = Cache::default();
    analyze(&mut cache, &rope);
    for _ in 0..500 {
        rope.insert(rope.line_to_char(128), "\n");
        cache
            .analyze(1, &rope, Instant::now() + Duration::from_secs(20), &|| {
                false
            })
            .unwrap();
        let blocks = &cache.snapshots.back().unwrap().blocks;
        assert!(blocks.len() <= rope.len_lines().div_ceil(BLOCK_LINES) + 1);
        assert!(cache.scanned_lines <= BLOCK_LINES * 2);
    }
    analyze(&mut cache, &rope);
}

#[test]
fn local_edits_reuse_whole_blocks_and_rebuild_scope_ancestry() {
    let mut rope = Rope::from_str(&"outer {\n  inner:\n    TODO BUG\n  }\n}\n".repeat(4_000));
    let original = rope.clone();
    let mut cache = Cache::default();
    analyze(&mut cache, &rope);
    assert_eq!(cache.scanned_lines, rope.len_lines());
    analyze(&mut cache, &rope);
    assert_eq!(cache.scanned_lines, 0);
    for line in [0, 126, 127, 128, 255, 19_998, 20_000] {
        let at = rope.line_to_char(line);
        rope.insert(at, "\tFIXME HACK 変数\n\t");
        analyze(&mut cache, &rope);
        assert!(
            cache.scanned_lines <= BLOCK_LINES * 2 + 2,
            "{}",
            cache.scanned_lines
        );
    }
    // Undo/Redo and equivalent independent storage require exact comparisons.
    analyze(&mut cache, &original);
    analyze(&mut cache, &Rope::from_str(&original.to_string()));
    assert_eq!(cache.scanned_lines, 0);
    analyze(&mut cache, &rope);
    // A new document identity must not inherit summaries from a reused tab.
    let actual = cache
        .analyze(2, &rope, Instant::now() + Duration::from_secs(20), &|| {
            false
        })
        .unwrap();
    assert_eq!(actual, super::super::analyze_editor_content(&rope));
    assert_eq!(cache.scanned_lines, rope.len_lines());
}

#[test]
fn incremental_metrics_match_fresh_scans_across_mixed_edits_and_line_endings() {
    for ending in ["\n", "\r\n", "\r"] {
        let mut rope = Rope::from_str(
            &format!(
                "root {{{ending}  TODO ä\u{2003}界:{ending}\tBUG FIXME HACK{ending}}}{ending}"
            )
            .repeat(160),
        );
        let mut cache = Cache::default();
        analyze(&mut cache, &rope);
        let mut state = 719_u64;
        let inserts = [
            "x",
            "",
            "\r",
            "\n",
            "\r\n",
            "\tnew:\n",
            "é\u{a0}value BUG ",
            "\n  (\n",
            "_TODO FIXME_",
            "   ",
        ];
        for step in 0..180 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let start = state as usize % (rope.len_chars() + 1);
            let end = (start + (state >> 24) as usize % 45).min(rope.len_chars());
            let before = rope.clone();
            rope.remove(start..end);
            rope.insert(start, inserts[step % inserts.len()]);
            analyze(&mut cache, &rope);
            if step % 11 == 0 {
                analyze(&mut cache, &before);
                analyze(&mut cache, &rope);
            }
        }
        rope.remove(..);
        analyze(&mut cache, &rope);
        rope.insert(0, "\r\nBUG\r\n");
        analyze(&mut cache, &rope);
    }
}

#[test]
fn chunked_lines_preserve_words_markers_unicode_and_scope_widths() {
    let mut cache = Cache::default();
    for pad in 990..1_025 {
        let text = format!(
            "{}TODOéFIXME\u{2003}BUG HACK FIXME_ _TODO BUGGY\r\n",
            " ".repeat(pad)
        );
        let mut rope = Rope::from_str(&text.repeat(4));
        rope.insert(pad + 2, "\u{a0}");
        analyze(&mut cache, &rope);
    }
    for token in [
        "TODO",
        "FIXME",
        "HACK",
        "BUG",
        "with_words",
        "界\u{2003}字",
        "    ",
        "\u{a0}\t",
    ] {
        let rope = Rope::from_str(&format!("{}:", token.repeat(3_000)));
        analyze(&mut cache, &rope);
    }
}

#[test]
fn cancelled_and_expired_scans_do_not_publish_or_discard_the_previous_snapshot() {
    let mut cache = Cache::default();
    let original = Rope::from_str(&"let x = 1;\n".repeat(1_000));
    analyze(&mut cache, &original);
    let mut changed = original.clone();
    changed.insert(10, &"TODO BUG αβ ".repeat(100_000));
    let checks = Cell::new(0);
    assert!(
        cache
            .analyze(
                1,
                &changed,
                Instant::now() + Duration::from_secs(20),
                &|| {
                    checks.set(checks.get() + 1);
                    checks.get() == 20
                }
            )
            .is_none()
    );
    assert_eq!(
        checks.get(),
        20,
        "cancellation is checked within a long line"
    );
    assert!(
        cache
            .snapshots
            .back()
            .unwrap()
            .source
            .is_instance(&original)
    );
    assert!(
        cache
            .analyze(1, &changed, Instant::now(), &|| false)
            .is_none()
    );
    analyze(&mut cache, &original);
    assert_eq!(cache.scanned_lines, 0);
    analyze(&mut cache, &changed);
}

#[test]
fn repeated_insertions_do_not_accumulate_tiny_blocks() {
    let mut cache = Cache::default();
    let mut rope = Rope::from_str(&"a\n".repeat(2_000));
    analyze(&mut cache, &rope);
    for _ in 0..300 {
        rope.insert(1, "\n");
        cache
            .analyze(1, &rope, Instant::now() + Duration::from_secs(20), &|| {
                false
            })
            .unwrap();
    }
    analyze(&mut cache, &rope);
    assert!(
        cache.snapshots.back().unwrap().blocks.len() <= rope.len_lines().div_ceil(BLOCK_LINES) + 2
    );
}

#[test]
fn cache_budget_does_not_limit_exact_results() {
    let rope = Rope::from_str(&"scope:\n".repeat(280_000));
    let mut cache = Cache::default();
    let metrics = analyze(&mut cache, &rope);
    assert_eq!(metrics.4.len(), 280_000);
    assert!(cache.snapshots.is_empty());
}

#[test]
#[ignore = "manual optimized performance measurement"]
fn benchmark_incremental_content_analysis() {
    use std::hint::black_box;
    for lines in [2_000, 100_000] {
        let original =
            Rope::from_str(&"    let value = function(argument); // TODO check\n".repeat(lines));
        let mut edited = original.clone();
        edited.insert(edited.line_to_char(lines / 2) + 4, "new_");
        let reference = super::super::analyze_editor_content(&edited);
        let mut cache = Cache::default();
        analyze(&mut cache, &original);
        let mut samples = [Vec::new(), Vec::new(), Vec::new()];
        for round in 0..9 {
            for mode in [round % 3, (round + 1) % 3, (round + 2) % 3] {
                let source = if round % 2 == 0 { &edited } else { &original };
                let now = Instant::now();
                let result = match mode {
                    0 => super::super::analyze_editor_content(source),
                    1 => Cache::default()
                        .analyze(1, source, now + Duration::from_secs(20), &|| false)
                        .unwrap(),
                    _ => cache
                        .analyze(1, source, now + Duration::from_secs(20), &|| false)
                        .unwrap(),
                };
                samples[mode].push(now.elapsed());
                black_box(&result);
                if round % 2 == 0 {
                    assert_eq!(result, reference);
                }
            }
        }
        for values in &mut samples {
            values.sort();
        }
        eprintln!(
            "content analysis {lines} lines: full reference={:?}, cold={:?}, local edit={:?}; retained blocks={}",
            samples[0][4],
            samples[1][4],
            samples[2][4],
            cache.snapshots.back().unwrap().blocks.len()
        );
    }
}

#[test]
#[ignore = "manual optimized measurement of repeated tab switches"]
fn benchmark_routine_content_switches() {
    use std::hint::black_box;
    for lines in [2_000, 40_000] {
        let documents: Vec<_> = (1..=3)
            .map(|id| {
                let text =
                    format!("function_{id} {{\n    let value = compute(input); // TODO\n}}\n");
                Rope::from_str(&text.repeat(lines / 3))
            })
            .collect();
        let expected: Vec<_> = documents
            .iter()
            .map(super::super::analyze_editor_content)
            .collect();
        let mut cache = Cache::default();
        for (id, source) in documents.iter().enumerate() {
            cache
                .analyze(
                    id as u64 + 1,
                    source,
                    Instant::now() + Duration::from_secs(20),
                    &|| false,
                )
                .unwrap();
        }
        let mut samples = Vec::new();
        for _ in 0..7 {
            let started = Instant::now();
            for turn in 0..18 {
                let id = turn % documents.len();
                let metrics = cache
                    .analyze(
                        id as u64 + 1,
                        black_box(&documents[id]),
                        Instant::now() + Duration::from_secs(20),
                        &|| false,
                    )
                    .unwrap();
                black_box(&metrics);
            }
            samples.push(started.elapsed());
        }
        for (id, source) in documents.iter().enumerate() {
            assert_eq!(
                cache
                    .analyze(
                        id as u64 + 1,
                        source,
                        Instant::now() + Duration::from_secs(20),
                        &|| false
                    )
                    .unwrap(),
                expected[id]
            );
        }
        samples.sort_unstable();
        eprintln!(
            "routine content switches: lines={lines}, switches=18, median_ns={}",
            samples[3].as_nanos()
        );
    }
}
