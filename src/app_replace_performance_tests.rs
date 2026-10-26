use super::*;

fn reference_replace(
    mut rope: ropey::Rope,
    mut matches: Vec<(usize, usize, usize)>,
    options: ReplaceSnapshotOptions<'_>,
) -> (ropey::Rope, usize) {
    use crate::search_regex::{SearchRegex, SearchText};
    let workspace = options.line_numbers_are_one_based;
    let compiled = if workspace {
        SearchRegex::workspace(
            options.search_query,
            options.case_sensitive,
            options.whole_word,
            options.use_regex,
        )
    } else {
        SearchRegex::editor(
            options.search_query,
            options.case_sensitive,
            options.whole_word,
            options.use_regex,
        )
    };
    let Ok(regex) = compiled else {
        return (rope, 0);
    };
    if !workspace && matches.len() >= crate::app_search_algo::MAX_SEARCH_MATCHES {
        matches = regex
            .scan(
                &rope,
                options.selection,
                usize::MAX,
                &crate::background::CancellationToken::new(),
            )
            .0;
    }
    let pattern = crate::search_replace::Replacement::new(options.replacement, options.use_regex);
    let source = rope.clone();
    let bounds = options
        .selection
        .map(|(sl, sc, el, ec)| {
            let position = |row: usize, col: usize| {
                (source.line_to_char(row.min(source.len_lines() - 1)) + col).min(source.len_chars())
            };
            (position(sl, sc), position(el, ec))
        })
        .unwrap_or((0, source.len_chars()));
    let raw_document = regex
        .needs_document
        .then(|| source.slice(bounds.0..bounds.1).to_string());
    let document = raw_document
        .as_ref()
        .map(|text| SearchText::new(text, true));
    let mut line_buffer = None;
    let mut buffered_row = None;
    let mut replaced = 0;
    let mut last_start = source.len_chars();
    let crlf = source.chunks().any(|chunk| chunk.contains("\r\n"));
    matches.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    for (line_num, col_start, col_end) in matches {
        let row = if workspace {
            line_num.saturating_sub(1)
        } else {
            line_num
        };
        if row >= source.len_lines() {
            continue;
        }
        let line_start = source.line_to_char(row);
        let start = line_start.saturating_add(col_start);
        let end = line_start.saturating_add(col_end);
        if start > end || end > last_start || start < bounds.0 || end > bounds.1 {
            continue;
        }
        let (input, offset) = if let Some(document) = &document {
            (document, source.char_to_byte(bounds.0))
        } else {
            if buffered_row != Some(row) {
                let line = source.line(row).to_string();
                let clean_end = line_start + line.trim_end_matches(['\r', '\n']).chars().count();
                let low = line_start.max(bounds.0);
                let high = clean_end.min(bounds.1);
                if low > high {
                    continue;
                }
                line_buffer = Some((
                    SearchText::new(&source.slice(low..high).to_string(), true),
                    source.char_to_byte(low),
                ));
                buffered_row = Some(row);
            }
            let (input, offset) = line_buffer.as_ref().unwrap();
            (input, *offset)
        };
        let start_byte = input.normalized_offset(source.char_to_byte(start) - offset);
        let end_byte = input.normalized_offset(source.char_to_byte(end) - offset);
        let mut actual_replacement = if workspace && options.use_regex {
            // Match.replaceString tries the match text first, then its preview
            // context, and finally falls back to the parsed static template.
            let matched = SearchText::new(&source.slice(start..end).to_string(), true);
            if let Some(captures) = regex.captures_at(&matched.text, 0) {
                crate::search_replace::expand_workspace(
                    options.replacement,
                    &matched.text,
                    &captures,
                    options.preserve,
                )
            } else if let Some(captures) = regex
                .captures_at(&input.text, start_byte)
                .filter(|captures| captures.range.start == start_byte)
            {
                crate::search_replace::expand_workspace(
                    options.replacement,
                    &input.text,
                    &captures,
                    options.preserve,
                )
            } else {
                crate::search_replace::workspace_pattern(options.replacement).0
            }
        } else {
            let Some(captures) = regex.captures_at(&input.text, start_byte) else {
                continue;
            };
            if captures.range != (start_byte..end_byte) {
                continue;
            }
            pattern.expand(&input.text, &captures, options.preserve)
        };
        actual_replacement = actual_replacement.replace("\r\n", "\n").replace('\r', "\n");
        if crlf {
            actual_replacement = actual_replacement.replace('\n', "\r\n");
        }
        rope.remove(start..end);
        rope.insert(start, &actual_replacement);
        last_start = start;
        replaced += 1;
    }
    (rope, replaced)
}

fn options<'a>(
    query: &'a str,
    replacement: &'a str,
    regex: bool,
    workspace: bool,
) -> ReplaceSnapshotOptions<'a> {
    ReplaceSnapshotOptions {
        line_numbers_are_one_based: workspace,
        search_query: query,
        replacement,
        case_sensitive: false,
        use_regex: regex,
        whole_word: false,
        preserve: false,
        selection: None,
    }
}

fn matches(rope: &ropey::Rope, options: &ReplaceSnapshotOptions<'_>) -> Vec<(usize, usize, usize)> {
    use crate::search_regex::SearchRegex;
    let regex = if options.line_numbers_are_one_based {
        SearchRegex::workspace(
            options.search_query,
            options.case_sensitive,
            options.whole_word,
            options.use_regex,
        )
    } else {
        SearchRegex::editor(
            options.search_query,
            options.case_sensitive,
            options.whole_word,
            options.use_regex,
        )
    }
    .unwrap();
    let mut matches = regex
        .scan(
            rope,
            options.selection,
            usize::MAX,
            &crate::background::CancellationToken::new(),
        )
        .0;
    if options.line_numbers_are_one_based {
        for entry in &mut matches {
            entry.0 += 1;
        }
    }
    matches
}

#[test]
fn batched_replace_preserves_editor_and_workspace_regex_semantics() {
    let text = "Foo foo FOO foO café 界 value=12;\nbar foo\n".repeat(40);
    for workspace in [false, true] {
        for crlf in [false, true] {
            let rope = ropey::Rope::from_str(&if crlf {
                text.replace('\n', "\r\n")
            } else {
                text.clone()
            });
            for (query, regex) in [
                ("foo", false),
                ("(foo)", true),
                (r"foo\nbar", true),
                ("(?=foo)", true),
                (r"café|界", true),
            ] {
                for replacement in ["bar", "$1界", "", "one\r\ntwo\rthree\n", "$&"] {
                    for selection in [None, Some((2, 3, 36, 5))] {
                        for preserve in [false, true] {
                            let config = || ReplaceSnapshotOptions {
                                preserve,
                                selection,
                                ..options(query, replacement, regex, workspace)
                            };
                            let hits = matches(&rope, &config());
                            let actual =
                                replace_snapshot_matches(rope.clone(), hits.clone(), config());
                            let expected = reference_replace(rope.clone(), hits, config());
                            assert_eq!(
                                actual, expected,
                                "workspace={workspace}, crlf={crlf}, query={query}, replacement={replacement:?}, selection={selection:?}, preserve={preserve}"
                            );
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn replacement_batches_preserve_sparse_edits_chunk_boundaries_and_insert_order() {
    for stride in [1, 3, 17, 257, 4097] {
        let source = ropey::Rope::from_str(&"界aé ".repeat(stride * 80));
        let mut actual = source.clone();
        let mut expected = source.clone();
        let mut batch = ReplacementBatch::default();
        for index in (0..64).rev() {
            let start = index * stride;
            let end = start + usize::from(index % 3 != 0);
            let replacement = if index % 4 == 0 { "" } else { "🌍 longer\n" };
            expected.remove(start..end);
            expected.insert(start, replacement);
            batch.push(&mut actual, start, end, replacement.into());
        }
        batch.flush(&mut actual);
        assert_eq!(actual, expected, "stride={stride}");
    }
    let mut actual = ropey::Rope::from_str("tail");
    let mut expected = actual.clone();
    let mut batch = ReplacementBatch::default();
    for index in 0..4_120 {
        let replacement = format!("{index}:");
        expected.insert(0, &replacement);
        batch.push(&mut actual, 0, 0, replacement);
        assert!(batch.edits.len() <= 4096);
    }
    batch.flush(&mut actual);
    assert_eq!(actual, expected);
}

#[test]
fn replacement_batches_bound_large_payloads_and_preserve_untouched_text() {
    let source = ropey::Rope::from_str(&"0123456789\n".repeat(40));
    let mut actual = source.clone();
    let mut expected = source.clone();
    let mut batch = ReplacementBatch::default();
    let large = "界".repeat(400_000);
    for index in (0..32).rev() {
        let start = index * 11;
        let text = if index == 16 { large.as_str() } else { "small" };
        expected.remove(start..start + 3);
        expected.insert(start, text);
        batch.push(&mut actual, start, start + 3, text.into());
    }
    batch.flush(&mut actual);
    assert_eq!(actual, expected);
    assert_eq!(source.to_string(), "0123456789\n".repeat(40));
}

#[test]
fn replacement_batches_handle_truncated_matches_and_overlapping_input() {
    let count = crate::app_search_algo::MAX_SEARCH_MATCHES + 25;
    let source = ropey::Rope::from_str(&"foo ".repeat(count));
    let hits = (0..crate::app_search_algo::MAX_SEARCH_MATCHES)
        .map(|i| (0, i * 4, i * 4 + 3))
        .collect();
    let (actual, replaced) =
        replace_snapshot_matches(source, hits, options("foo", "界", false, false));
    assert_eq!(replaced, count);
    assert_eq!(actual.to_string(), "界 ".repeat(count));

    let source = ropey::Rope::from_str(&"foo foo\n".repeat(40));
    let config = || options("foo", "", false, false);
    let mut hits = matches(&source, &config());
    hits.extend([
        (0, 0, 3),
        (0, 0, 7),
        (9, 2, 4),
        (0, 0, usize::MAX),
        (usize::MAX, 0, 3),
    ]);
    assert_eq!(
        replace_snapshot_matches(source.clone(), hits.clone(), config()),
        reference_replace(source, hits, config())
    );
}

#[test]
#[ignore = "comparative release benchmark"]
fn benchmark_editor_replace_batches() {
    use std::{hint::black_box, time::Instant};
    for (case, text, query, regex) in [
        (
            "dense literal",
            "foo value foo\n".repeat(8_000),
            "foo",
            false,
        ),
        (
            "unicode regex",
            "é Foo foo 界\r\n".repeat(8_000),
            "(foo)",
            true,
        ),
        (
            "sparse literal",
            format!("{}foo\n", "padding ".repeat(900)).repeat(64),
            "foo",
            false,
        ),
    ] {
        let rope = ropey::Rope::from_str(&text);
        let config = || options(query, "longer", regex, false);
        let hits = matches(&rope, &config());
        let expected = reference_replace(rope.clone(), hits.clone(), config());
        assert_eq!(
            replace_snapshot_matches(rope.clone(), hits.clone(), config()),
            expected
        );
        let mut old = Vec::new();
        let mut new = Vec::new();
        for round in 0..9 {
            for optimized in [round % 2 == 0, round % 2 != 0] {
                let snapshot = black_box(rope.clone());
                let matches = black_box(hits.clone());
                let started = Instant::now();
                let result = if optimized {
                    replace_snapshot_matches(snapshot, matches, config())
                } else {
                    reference_replace(snapshot, matches, config())
                };
                black_box(&result);
                if optimized {
                    new.push(started.elapsed());
                } else {
                    old.push(started.elapsed());
                }
            }
        }
        old.sort_unstable();
        new.sort_unstable();
        eprintln!(
            "replace {case}: before {:?}, after {:?}, {:.2}x",
            old[4],
            new[4],
            old[4].as_secs_f64() / new[4].as_secs_f64()
        );
    }
}
