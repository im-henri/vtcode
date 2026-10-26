use super::*;
use std::cell::Cell;

#[test]
fn full_wrap_cache_preserves_nearby_rows_in_both_browsing_directions() {
    for ascending in [true, false] {
        let mut editor = Editor::test_with_text(&"value words / other\n".repeat(10001));
        let start = usize::from(!ascending);
        for line in start..start + 10000 { editor.wrap_ranges_for_line(line, 12); }
        let requested = if ascending { 10000 } else { 0 };
        let nearby = if ascending { 9999 } else { 1 };
        let old = editor.wrap_ranges_for_line(nearby, 12);
        editor.wrap_ranges_for_line(requested, 12);
        assert_eq!(editor.wrap_cache.len(), MAX_CACHED_WRAP_LINES);
        assert!(std::rc::Rc::ptr_eq(&old, &editor.wrap_ranges_for_line(nearby, 12)));
        let distant = if ascending { 0 } else { 10000 };
        assert!(!editor.wrap_cache.contains_key(&(distant, 12, editor.tab_size)));
        for line in if ascending { 5000..=9999 } else { 1..=5000 } {
            assert!(editor.wrap_cache.contains_key(&(line, 12, editor.tab_size)));
        }
    }
}

#[test]
fn local_wrap_invalidation_seeks_all_width_variants_and_keeps_distant_rows() {
    let mut editor = Editor::test_with_text(&"value words / other界 value\n".repeat(3000));
    let mut before = Vec::new();
    for line in 0..2500 {
        for width in [12, 24, 80] {
            before.push((line, width, editor.wrap_ranges_for_line(line, width)));
        }
    }
    editor.set_cursor(1200, 5);
    editor.insert_char('界');
    for (line, width, old) in before {
        let current = editor.wrap_ranges_for_line(line, width);
        assert_eq!(std::rc::Rc::ptr_eq(&old, &current), !(1199..=1201).contains(&line));
        if line == 1200 {
            assert_eq!(*current, Editor::wrap_ranges_for_line_text_tabbed(
                editor.rope.line(line).to_string().trim_end(), width, editor.tab_size));
        }
    }
    editor.undo();
    assert_eq!(*editor.wrap_ranges_for_line(1200, 12), Editor::wrap_ranges_for_line_text_tabbed("value words / other界 value", 12, editor.tab_size));
}

#[test]
#[ignore = "manual optimized-profile wrap invalidation comparison"]
fn benchmark_indexed_wrap_invalidation() {
    use std::{collections::BTreeMap, hint::black_box, time::Instant};
    let data = std::rc::Rc::new(vec![(0usize, 10usize)]);
    let mut indexed: BTreeMap<_, _> = (0..9000).map(|i| ((i, 80, 4), data.clone())).collect();
    let mut scanned: rustc_hash::FxHashMap<_, _> = (0..9000).map(|i| ((i, 80, 4), data.clone())).collect();
    let mut samples = [Vec::new(), Vec::new()];
    for round in 0..7 {
        for direct in [round % 2 == 0, round % 2 != 0] {
            let start = Instant::now();
            for _ in 0..1000 {
                if direct {
                    let keys: Vec<_> = indexed.range((4499, 0, 0)..=(4501, usize::MAX, usize::MAX)).map(|(&key, _)| key).collect();
                    for key in keys { indexed.remove(&key); }
                    for line in 4499..=4501 { indexed.insert((line, 80, 4), data.clone()); }
                    black_box(&indexed);
                } else {
                    scanned.retain(|&(line, _, _), _| line < 4499 || line > 4501);
                    for line in 4499..=4501 { scanned.insert((line, 80, 4), data.clone()); }
                    black_box(&scanned);
                }
            }
            samples[usize::from(direct)].push(start.elapsed() / 1000);
        }
    }
    for sample in &mut samples { sample.sort(); }
    eprintln!("9000 cached wrap lines: scan={:?} indexed={:?}", samples[0][3], samples[1][3]);
}

fn original_range_map(text: &str, width: usize) -> Vec<(usize, usize)> {
    if text.is_ascii() {
        let mut ranges = Vec::new();
        visit_wrap_ranges(
            text.as_bytes(),
            width,
            |c| c.is_ascii_whitespace(),
            ascii_wrap_break,
            |start, end| ranges.push((start, end)),
        );
        ranges
    } else {
        super::tests::reference_wrap(text, width)
    }
}

#[test]
fn streamed_range_maps_match_frozen_segmentation_across_rope_chunks() {
    let mut cases = vec![
        "word/path, next; value- ".repeat(800),
        "väärtus界界,\u{2003} next/界;\t".repeat(800),
        "界".repeat(8_000),
        " \t".repeat(8_000),
        "\u{2003}".repeat(8_000),
        String::new(),
    ];
    let alphabet = [
        'x', '界', 'ä', ' ', '\t', '\u{2003}', '-', '/', '\\', ',', ';',
    ];
    let mut random = 0xa2c9_7314u64;
    for _ in 0..256 {
        let mut text = String::new();
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        for _ in 0..(random >> 32) as usize % 512 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            text.push(alphabet[(random >> 32) as usize % alphabet.len()]);
        }
        cases.push(text);
    }
    for text in cases {
        for ending in ["", "\n", "\r\n", "\r\r\n"] {
            let mut editor = Editor::test_with_text(&format!("{text}{ending}"));
            for width in [0, 1, 2, 7, 31, 32, 33, 80, 1_024, 4_094, 4_095, usize::MAX] {
                let expected = super::tests::reference_wrap(&text, width.max(1));
                let actual = editor.wrap_ranges_for_line(0, width);
                assert_eq!(*actual, expected, "width={width} ending={ending:?}");
            }
        }
    }
}

#[test]
fn unicode_map_windows_preserve_whitespace_lookahead_and_fallback_boundaries() {
    for width in [31, 32, 33, 80, 4_094, 4_095] {
        for leading in [0, 1, width - 2, width - 1, width] {
            for spaces in [1, 2, 3, width + 5] {
                let whitespace = ['\u{85}', '\u{a0}', '\u{1680}', '\u{2003}', '\u{2028}', '\u{2029}', '\u{202f}', '\u{205f}', '\u{3000}'];
                let whitespace: String = whitespace.into_iter().cycle().take(spaces).collect();
                let text = format!("{}{whitespace}界;{}", "界".repeat(leading), "尾".repeat(width + 3));
                let expected = super::tests::reference_wrap(&text, width);
                let mut ranges = Vec::new();
                visit_unicode_wrap_map(text.chars(), text.chars().count(), width,
                    |start, end| ranges.push((start, end)));
                assert_eq!(ranges, expected, "width={width}, leading={leading}, spaces={spaces}");
                assert_eq!(Editor::wrap_ranges_for_line_text(&text, width), expected);
                let mut editor = Editor::test_with_text(&format!("{text}\r\n"));
                assert_eq!(*editor.wrap_ranges_for_line(0, width), expected);
            }
        }
    }
}

#[test]
fn scroll_sync_uses_only_row_offsets_and_keeps_wrap_ranges_cold() {
    let text = format!("header\n{}\ntail", "界 word ".repeat(20_000));
    let mut editor = Editor::test_with_text(&text);
    editor.word_wrap = true;
    let offsets = Editor::build_wrap_row_offsets(&editor.rope, 80, 4, &[], || false).unwrap();
    assert!(editor.install_wrap_layout(80, offsets.clone()));
    for row in [
        0,
        1,
        123,
        offsets[2] - 1,
        offsets[2],
        offsets[3],
        offsets[3] + 20,
    ] {
        editor.wrap_scroll_offset = row;
        let expected = editor
            .wrapped_row_at(row, 80)
            .map_or(editor.cursor_line, |row| row.line_idx);
        editor.wrap_cache.clear();
        editor.sync_line_scroll_from_wrap(80);
        assert_eq!(editor.scroll_offset, expected);
        assert!(
            editor.wrap_cache.is_empty(),
            "scroll sync must not segment a line"
        );
    }
}

#[test]
#[ignore = "manual release wrap materialization and scroll benchmark"]
fn benchmark_wrap_range_materialization_and_scroll_sync() {
    use std::time::Instant;
    for (label, text) in [
        ("ASCII", "word/path, next; value- ".repeat(90_000)),
        ("Unicode", "väärtus界界,\u{2003} next/界;\t".repeat(70_000)),
    ] {
        let mut editor = Editor::test_with_text(&text);
        let expected = original_range_map(&text, 80);
        let mut before = Vec::new();
        let mut after = Vec::new();
        for sample in 0..7 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                editor.wrap_cache.clear();
                let started = Instant::now();
                let result = if optimized {
                    editor.wrap_ranges_for_line(0, 80)
                } else {
                    let copied = editor.rope.line(0).to_string();
                    std::rc::Rc::new(original_range_map(
                        copied.trim_end_matches(['\n', '\r']),
                        80,
                    ))
                };
                let elapsed = started.elapsed();
                assert_eq!(*result, expected);
                std::hint::black_box(result);
                if optimized {
                    after.push(elapsed);
                } else {
                    before.push(elapsed);
                }
            }
        }
        before.sort();
        after.sort();
        eprintln!(
            "{label} ~2MiB range map: copied {:?}, streamed {:?}",
            before[3], after[3]
        );

        let offsets = Editor::build_wrap_row_offsets(&editor.rope, 80, 4, &[], || false).unwrap();
        editor.install_wrap_layout(80, offsets);
        editor.wrap_scroll_offset = 123;
        let mut old = Vec::new();
        let mut new = Vec::new();
        for sample in 0..7 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                editor.wrap_cache.clear();
                let started = Instant::now();
                if optimized {
                    editor.sync_line_scroll_from_wrap(80);
                } else {
                    editor.scroll_offset = editor
                        .wrapped_row_at(editor.wrap_scroll_offset, 80)
                        .map_or(editor.cursor_line, |row| row.line_idx);
                }
                let elapsed = started.elapsed();
                assert_eq!(editor.scroll_offset, 0);
                if optimized {
                    new.push(elapsed);
                } else {
                    old.push(elapsed);
                }
            }
        }
        old.sort();
        new.sort();
        eprintln!(
            "{label} cold-range scroll synchronization: old {:?}, offsets {:?}",
            old[3], new[3]
        );
    }
}

fn assert_streamed_wrap_counts(text: &str, endings: &[&str], widths: &[usize]) {
    for ending in endings {
        let rope = ropey::Rope::from_str(&format!("{text}{ending}"));
        for &width in widths {
            let expected = Editor::wrap_ranges_for_line_text_tabbed(text, width, 4).len();
            let actual = Editor::wrap_row_count_for_rope_line_until(rope.line(0), width, &|| false);
            assert_eq!(
                actual,
                Some(expected),
                "width={width} ending={ending:?} chars={} source={:?}",
                text.chars().count(),
                text.chars().take(20).collect::<String>()
            );
        }
    }
}

#[test]
fn streaming_wrap_counts_match_painting_across_unicode_chunks_and_widths() {
    let mut cases = vec![
        String::new(),
        " \t \t ".into(),
        "aa   bb  cc".into(),
        "ää\u{2003}\u{2003}界界,--x".into(),
    ];
    let alphabet = [
        'a', '界', 'ä', ' ', '\t', '\u{2003}', '-', '/', '\\', ',', ';',
    ];
    let mut random = 0x1256abcdu64;
    for _ in 0..160 {
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        let len = (random >> 32) as usize % 320;
        let mut text = String::new();
        for _ in 0..len {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            text.push(alphabet[(random >> 32) as usize % alphabet.len()]);
        }
        cases.push(text);
    }
    for text in cases {
        assert_streamed_wrap_counts(
            &text,
            &["", "\n", "\r\n"],
            &[0, 1, 2, 3, 4, 7, 16, 79, 80, 1_024, usize::MAX],
        );
    }
}

#[test]
fn streamed_wrap_counts_cross_rope_chunks_for_words_and_whitespace() {
    // The small matrix above covers every width/ending combination. Long lines
    // separately exercise chunk transitions, narrow splits and wide windows.
    for text in [
        "word / path, next; value\\key- ".repeat(1_024),
        "väärtus界界,\u{2003} next/界;\t".repeat(1_024),
        "x".repeat(20_000),
        "界".repeat(20_000),
        " \t".repeat(10_000),
        "\u{2003}".repeat(20_000),
    ] {
        assert_streamed_wrap_counts(&text, &["\r\n"], &[1, 7, 80, 1_024, usize::MAX]);
    }
}

#[test]
fn streamed_document_counts_keep_folded_prefix_offsets_exact() {
    let lines = [
        "short".into(),
        "word ".repeat(400),
        "界\u{2003}value,".repeat(400),
        "hidden ".repeat(500),
        "\t".repeat(3_000),
        "tail".into(),
    ];
    let source = lines.join("\r\n");
    let rope = ropey::Rope::from_str(&source);
    let folds = [(1, 4)];
    for width in [1, 4, 80, 125] {
        let mut expected = vec![0];
        for line in 0..rope.len_lines() {
            let hidden = folds.iter().any(|&(start, end)| line > start && line < end);
            let rows = if hidden {
                0
            } else {
                Editor::wrap_ranges_for_line_text_tabbed(lines[line].as_str(), width, 4).len()
            };
            expected.push(expected.last().unwrap() + rows);
        }
        assert_eq!(
            Editor::build_wrap_row_offsets(&rope, width, 4, &folds, || false),
            Some(expected)
        );
    }
}

#[test]
fn unicode_cancellation_bounds_decoding_independently_of_width() {
    for width in [1, 80, 4_096, 65_536] {
        let decoded = Cell::new(0);
        let probes = Cell::new(0);
        let chars = (0..1_000_000).map(|_| {
            decoded.set(decoded.get() + 1);
            '界'
        });
        let result = count_unicode_wrap_rows(chars, 1_000_000, width, &|| {
            probes.set(probes.get() + 1);
            probes.get() >= 2
        });
        assert_eq!(result, None);
        assert_eq!(probes.get(), 2);
        assert!(decoded.get() > 0 && decoded.get() <= WRAP_CANCELLATION_WORK);
    }
}

#[test]
fn ascii_cancellation_bounds_long_backward_scans_and_chunk_loading() {
    let text = "x".repeat(65_536 + WRAP_CANCELLATION_WORK);
    for width in [80, 65_536] {
        let scanned = Cell::new(0);
        let probes = Cell::new(0);
        let result = count_ascii_wrap_rows(
            std::iter::once(text.as_str()),
            text.len(),
            width,
            &|| {
                probes.set(probes.get() + 1);
                probes.get() >= 3
            },
            |c| {
                scanned.set(scanned.get() + 1);
                ascii_wrap_break(c)
            },
        );
        assert_eq!(result, None);
        assert_eq!(probes.get(), 3);
        assert!(scanned.get() > 0 && scanned.get() <= WRAP_CANCELLATION_WORK);
    }
    let chunk = "x".repeat(1_024);
    let loaded = Cell::new(0);
    let probes = Cell::new(0);
    let chunks = (0..1_024).map(|_| {
        loaded.set(loaded.get() + 1);
        chunk.as_str()
    });
    let result = count_ascii_wrap_rows(
        chunks,
        1_024 * 1_024,
        1_024 * 1_024 - 1,
        &|| {
            probes.set(probes.get() + 1);
            probes.get() >= 4
        },
        ascii_wrap_break,
    );
    assert_eq!(result, None);
    assert_eq!(
        loaded.get(),
        2,
        "a cancelled wide window must not retain every chunk"
    );
}

#[test]
fn folded_prefix_fill_and_fenwick_conversion_are_interruptible() {
    let rope = ropey::Rope::from_str(&"\n".repeat(20_000));
    let probes = Cell::new(0);
    assert!(
        Editor::build_wrap_row_offsets(&rope, 80, 4, &[(0, 20_000)], || {
            probes.set(probes.get() + 1);
            probes.get() >= 4
        })
        .is_none()
    );
    assert_eq!(probes.get(), 4);
    for stop_after in [2, 4] {
        let probes = Cell::new(0);
        assert!(
            WrapRowOffsets::from_offsets_cancellable((0..=8_192).collect(), || {
                probes.set(probes.get() + 1);
                probes.get() >= stop_after
            })
            .is_none()
        );
        assert_eq!(probes.get(), stop_after);
    }
}

#[test]
fn whole_long_lines_cancel_before_the_worker_finishes_one_logical_line() {
    // Keep several cancellation intervals available without constructing
    // megabytes the worker is required to leave unread.
    for source in [
        "word ".repeat(WRAP_CANCELLATION_WORK * 2),
        "界\u{2003}value ".repeat(WRAP_CANCELLATION_WORK * 2),
    ] {
        let rope = ropey::Rope::from_str(&source);
        let probes = Cell::new(0);
        assert!(
            Editor::build_wrap_row_offsets(&rope, 80, 4, &[], || {
                probes.set(probes.get() + 1);
                probes.get() >= 6
            })
            .is_none()
        );
        assert_eq!(probes.get(), 6);
    }
}

fn legacy_worker_counts(rope: &ropey::Rope, width: usize) -> Vec<usize> {
    let mut offsets = Vec::with_capacity(rope.len_lines() + 1);
    let mut total = 0;
    for line in rope.lines() {
        offsets.push(total);
        let line = line.to_string();
        total += Editor::wrap_row_count_for_line_text(line.trim_end_matches(['\n', '\r']), width);
    }
    offsets.push(total);
    offsets
}

#[test]
#[ignore = "manual release streaming wrap benchmark"]
fn benchmark_streamed_long_line_wrap_counting() {
    for (label, source) in [
        ("ASCII code", "word/path, next; value- ".repeat(90_000)),
        (
            "Unicode code",
            "väärtus界界,\u{2003} next/界;\t".repeat(70_000),
        ),
        ("ASCII whitespace", " \t".repeat(1_000_000)),
        ("Unicode whitespace", "\u{2003}".repeat(700_000)),
    ] {
        let rope = ropey::Rope::from_str(&source);
        let mut before = Vec::new();
        let mut after = Vec::new();
        for sample in 0..7 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                let started = std::time::Instant::now();
                let offsets = if optimized {
                    Editor::build_wrap_row_offsets(&rope, 80, 4, &[], || false).unwrap()
                } else {
                    legacy_worker_counts(&rope, 80)
                };
                let elapsed = started.elapsed();
                assert_eq!(offsets, legacy_worker_counts(&rope, 80));
                std::hint::black_box(offsets);
                if optimized {
                    after.push(elapsed);
                } else {
                    before.push(elapsed);
                }
            }
        }
        before.sort();
        after.sort();
        eprintln!(
            "~2MiB single line wrap counts {label}: copied={:?} streamed={:?} per operation",
            before[3], after[3]
        );
    }
}
