use super::*;
#[path = "vtcomplete_scheduling_tests.rs"]
mod scheduling_tests;

fn contextual(text: &str, query: &str) -> Vec<String> {
    let mut active = snapshot(1, text, text.lines().count().saturating_sub(1), true);
    active.cursor_col = active.rope.line(active.cursor_line).len_chars();
    compute_contextual(
        &active,
        query,
        "Unspecified Language",
        &Default::default(),
        &mut TokenCache::default(),
        || false,
    )
    .unwrap()
    .suggestions
}

#[test]
fn occurrence_target_can_be_bound_by_a_later_visible_use_in_the_full_worker() {
    let before = "dispatch(payloadOld, channelOld); validate(payloadOld)\ndispatch(pay";
    let text = format!("{before}, channelNew); validate(payloadNew)");
    let mut active = snapshot(91, &text, 1, true);
    active.cursor_col = before.rsplit('\n').next().unwrap().chars().count();
    for explicit in [false, true] {
        let result = compute_contextual_invoked(
            &active,
            "pay",
            "Unknown language",
            &Default::default(),
            &mut TokenCache::default(),
            explicit,
            || false,
        )
        .unwrap();
        assert_eq!(result.suggestions.first().map(String::as_str), Some("payloadNew"));
        assert_eq!(result.suggestions.len(), result.details.len());
    }
}

#[test]
fn more_specific_literal_occurrence_survives_an_optional_target_mapping_collision() {
    let before = "choose(channel, rightPayload, finalMode)\nprepare(channel); choose(channel, rightSpecific, finalMode)\nprepare(channel); choose(channel, ri";
    let text = format!("{before}, finalMode)");
    let mut active = snapshot(92, &text, 2, true);
    active.cursor_col = before.rsplit('\n').next().unwrap().chars().count();
    let result = compute_contextual_invoked(
        &active, "ri", "Unknown language", &Default::default(),
        &mut TokenCache::default(), false, || false,
    ).unwrap();
    assert_eq!(result.suggestions.first().map(String::as_str), Some("rightSpecific"));
}

#[test]
fn compatible_shorter_context_survives_an_incompatible_longer_example() {
    let before = "choose(channel, rightPayload, finalMode)\nprepare(channel); choose(channel, rightWrong, wrongMode)\nprepare(channel); choose(channel, ri";
    let text = format!("{before}, finalMode)");
    let mut active = snapshot(92, &text, 2, true);
    active.cursor_col = before.rsplit('\n').next().unwrap().chars().count();
    for explicit in [false, true] {
        let result = compute_contextual_invoked(&active, "ri", "Unknown language",
            &Default::default(), &mut TokenCache::default(), explicit, || false).unwrap();
        assert_eq!(result.suggestions.first().map(String::as_str), Some("rightPayload"));
    }
}

#[test]
fn learned_receiver_and_assignment_context_outweigh_unrelated_words() {
    let words = contextual(
        "logger.printLine(value)\nlogger.printLine(other)\nbuffer.printBuffer(value)\nlogger.pr",
        "pr",
    );
    assert_eq!(words.first().map(String::as_str), Some("printLine()"));
    assert!(words.iter().any(|word| word == "printBuffer"));
    let values = contextual(
        "enabled = true\nenabled = true\nother = truncate\nenabled = tr",
        "tr",
    );
    assert_eq!(values.first().map(String::as_str), Some("true"));
}

#[test]
fn camel_case_abbreviations_beat_flat_subsequences() {
    let words = contextual("garbuncle getUserConfig\nguc", "guc");
    assert_eq!(words.first().map(String::as_str), Some("getUserConfig"));
    let words = contextual("hatching HTTPClient\nhc", "hc");
    assert_eq!(words.first().map(String::as_str), Some("HTTPClient"));
}

#[test]
fn call_suffix_requires_repeated_predominant_evidence() {
    assert_eq!(
        contextual("renderFrame()\nrenderFrame()\nren", "ren"),
        ["renderFrame()"]
    );
    assert_eq!(contextual("renderFrame()\nren", "ren"), ["renderFrame"]);
    assert_eq!(
        contextual(
            "renderFrame() renderFrame() renderFrame renderFrame renderFrame\nren",
            "ren"
        ),
        ["renderFrame"]
    );
}

#[test]
fn function_call_preference_and_minimum_prefix_control_actual_behavior() {
    let mut active = snapshot(1, "renderFrame()\nrenderFrame()\nren", 2, true);
    active.cursor_col = 3;
    let index = crate::completion_index::CompletionIndex::default();
    let mut settings = index.settings();
    settings.complete_calls = false;
    settings.min_prefix_chars = 3;
    settings.context_triggers = false;
    index.configure(settings.clone());
    let result = compute_contextual(
        &active,
        "ren",
        "Unknown",
        &index,
        &mut TokenCache::default(),
        || false,
    )
    .unwrap();
    assert_eq!(result.suggestions, ["renderFrame"]);
    let mut editor = crate::editor::Editor::test_with_text("re");
    editor.cursor_col = 2;
    assert!(!automatic_trigger(&editor, &settings));
    editor.rope.insert(2, "n");
    editor.cursor_col = 3;
    assert!(automatic_trigger(&editor, &settings));
    editor.rope = ropey::Rope::from_str("let value = &");
    editor.cursor_col = editor.rope.len_chars();
    assert!(!automatic_trigger(&editor, &settings));
    settings.context_triggers = true;
    assert!(automatic_trigger(&editor, &settings));
}

#[test]
fn context_cache_reuses_unchanged_lines_and_invalidates_exact_edits() {
    let mut active = snapshot(
        1,
        "logger.printLine()\nlogger.printLine()\nlogger.pr",
        2,
        true,
    );
    active.cursor_col = 9;
    let mut cache = TokenCache::default();
    for query in ["pr", "pri"] {
        compute_contextual(
            &active,
            query,
            "Unknown",
            &Default::default(),
            &mut cache,
            || false,
        )
        .unwrap();
    }
    assert_eq!(cache.builds, 1);
    let document = &cache.documents[0];
    let unchanged = document.context_lines[&0].1.as_ptr();
    active.rope.insert(active.rope.len_chars(), "i");
    active.cursor_col += 1;
    compute_contextual(
        &active,
        "pri",
        "Unknown",
        &Default::default(),
        &mut cache,
        || false,
    )
    .unwrap();
    assert_eq!(cache.documents[0].context_lines[&0].1.as_ptr(), unchanged);
    assert_eq!(
        cache.documents[0].context_lines[&2].1.last().unwrap().word,
        "pri"
    );
    active.rope.insert(0, "header\n");
    active.cursor_line += 1;
    compute_contextual(
        &active,
        "pri",
        "Unknown",
        &Default::default(),
        &mut cache,
        || false,
    )
    .unwrap();
    assert_eq!(cache.documents[0].context_lines[&1].1.as_ptr(), unchanged);
    assert!(cache.documents[0].context_bytes <= 64 * 1024);
}

#[test]
fn explicit_completion_supports_one_letter_and_empty_member_prefixes() {
    for (text, col, expected) in [("obj.x", 5, "x"), ("obj.", 4, "")] {
        let mut active = snapshot(1, text, 0, true);
        active.cursor_col = col;
        assert!(matches!(resolve_prefix_with_minimum(&active, 0, &|| false),
            Some(ResolvedPrefix::Text(prefix)) if prefix == expected));
    }
    let words = contextual("logger.printLine()\nlogger.printLine()\nlogger.", "");
    assert_eq!(words.first().map(String::as_str), Some("printLine()"));
}

#[test]
fn automatic_completion_stays_quiet_in_comments_and_quotes() {
    for text in [
        "// logg",
        "value // logg",
        "# logg",
        "-- logg",
        "value = \"logg",
        "value = 'logg",
        "/* logg",
    ] {
        let mut active = snapshot(1, text, 0, true);
        active.cursor_col = text.chars().count();
        assert!(quiet_context(&active), "{text:?}");
    }
    for text in [
        "logger.pr",
        "f(\"value\"); logg",
        "&'static value",
        "value /* comment */ logg",
        "fn f<'a, 'b>() {}\nlogger.pr",
        "fn f<'a: 'b + 'c>() {}\nlogger.pr",
        "'outer: loop {}\nlogger.pr",
    ] {
        let mut active = snapshot(1, text, text.lines().count() - 1, true);
        active.cursor_col = active.rope.line(active.cursor_line).len_chars();
        assert!(!quiet_context(&active), "{text:?}");
    }
}

#[test]
fn contextual_completion_is_cancelled_and_neighborhood_work_is_bounded() {
    let text =
        "unrelated ".repeat(1000) + "\n" + &"other_unrelated\n".repeat(10_000) + "needle_value\nne";
    let mut active = snapshot(1, &text, 10_002, true);
    active.cursor_col = 2;
    let mut cache = TokenCache::default();
    assert!(
        compute_contextual(
            &active,
            "ne",
            "Unknown",
            &Default::default(),
            &mut cache,
            || true
        )
        .is_none()
    );
    let result = compute_contextual(
        &active,
        "ne",
        "Unknown",
        &Default::default(),
        &mut cache,
        || false,
    )
    .unwrap();
    assert!(result.suggestions.iter().any(|word| word == "needle_value"));
    assert!(cache.documents[0].context_lines.len() <= 256);
    assert!(
        cache.documents[0]
            .context_lines
            .keys()
            .all(|line| active.cursor_line.abs_diff(*line) < 128)
    );
}

#[test]
#[ignore = "manual current-engine completion latency measurement; run --ignored --nocapture"]
fn benchmark_contextual_hot_path() {
    use std::{hint::black_box, time::Instant};

    let mut text = String::with_capacity(900_000);
    for row in 0..9_997 {
        text.push_str(&format!(
            "client.getUserConfig(v{0}); logger.printLine(v{0}); buffer.pushMessage(v{0});\n",
            row % 64,
        ));
    }
    text.push_str("logger.pr\nclient.guc\nbuffer.pm");
    let rope = ropey::Rope::from_str(&text);
    assert_eq!(rope.len_lines(), 10_000);
    assert!(rope.len_bytes() <= MAX_DOCUMENT_BYTES);
    let cases: Vec<_> = [
        ("pr", "printLine", 9),
        ("guc", "getUserConfig", 10),
        ("pm", "pushMessage", 9),
    ]
    .into_iter()
    .enumerate()
    .map(|(offset, (prefix, expected, col))| {
        (
            VtCompleteSnapshot {
                source: Default::default(),
                rope: rope.clone(),
                document_id: 1,
                cursor_line: 9_997 + offset,
                cursor_col: col,
                active: true,
            },
            prefix,
            expected,
        )
    })
    .collect();
    let index = crate::completion_index::CompletionIndex::default();
    let mut cache = TokenCache::default();
    let started = Instant::now();
    let cold = compute_contextual(
        &cases[0].0,
        cases[0].1,
        "Unknown",
        &index,
        &mut cache,
        || false,
    )
    .unwrap();
    let cold_elapsed = started.elapsed();
    assert!(
        cold.suggestions
            .iter()
            .any(|word| word.strip_suffix("()").unwrap_or(word) == cases[0].2)
    );
    for (active, prefix, expected) in &cases {
        let result =
            compute_contextual(active, prefix, "Unknown", &index, &mut cache, || false).unwrap();
        assert!(
            result
                .suggestions
                .iter()
                .any(|word| word.strip_suffix("()").unwrap_or(word) == *expected)
        );
    }
    let builds = cache.builds;
    eprintln!(
        "contextual completion: lines={} bytes={} cold_first={cold_elapsed:?}",
        rope.len_lines(),
        rope.len_bytes()
    );
    for count in [100u32, 1_000] {
        let started = Instant::now();
        for iteration in 0..count {
            let (active, prefix, _) = &cases[iteration as usize % cases.len()];
            black_box(
                compute_contextual(
                    black_box(active),
                    black_box(prefix),
                    "Unknown",
                    &index,
                    &mut cache,
                    || false,
                )
                .unwrap(),
            );
        }
        let elapsed = started.elapsed();
        assert_eq!(
            cache.builds, builds,
            "warm queries rebuilt the active document index"
        );
        eprintln!(
            "contextual completion: warm_queries={count} total={elapsed:?} per_request={:?}",
            elapsed / count
        );
    }
}

#[test]
fn streaming_words_reuse_bounded_scratch_without_leaking_cancelled_tokens() {
    let mut scores = Scores::new(
        "zz",
        "zz",
        crate::fuzzy_score::FuzzyWorkBudget::new(scoring::work_limits()),
    );
    let mut scratch = String::with_capacity(MAX_COMPLETION_WORD_BYTES);
    let storage = scratch.as_ptr();
    let mut words = Vec::new();
    for text in ["alpha beta", "界界 αβγ", "", "delta"] {
        let rope = ropey::Rope::from_str(text);
        scratch.push_str("cancelled_partial_token");
        assert_eq!(
            visit_words(
                rope.line(0),
                &mut scores,
                &mut scratch,
                &|| false,
                |_, token| {
                    if let WordToken::Word(word) = token {
                        words.push(word.to_owned());
                    }
                    Some(())
                }
            ),
            Some(true)
        );
        assert!(scratch.is_empty());
        assert_eq!(scratch.as_ptr(), storage);
        assert_eq!(scratch.capacity(), MAX_COMPLETION_WORD_BYTES);
    }
    assert_eq!(words, ["alpha", "beta", "界界", "αβγ", "delta"]);
    let rope = ropey::Rope::from_str(&("x".repeat(MAX_COMPLETION_WORD_BYTES + 1) + " small"));
    let mut oversized = 0;
    visit_words(
        rope.line(0),
        &mut scores,
        &mut scratch,
        &|| false,
        |_, token| {
            match token {
                WordToken::Oversized => oversized += 1,
                WordToken::Word(word) => assert_eq!(word, "small"),
            }
            Some(())
        },
    )
    .unwrap();
    assert_eq!(oversized, 1);
    assert_eq!(scratch.as_ptr(), storage);
}

#[test]
fn a_request_cannot_evict_the_indexes_it_already_used() {
    let mut documents: Vec<_> = (0..8)
        .map(|id| {
            snapshot(
                id,
                &format!("print_{id} αβ{id} other_value\n").repeat(30),
                10,
                id == 0,
            )
        })
        .collect();
    let mut cache = TokenCache::default();
    for _ in 0..3 {
        for prefix in ["zz", "pri", "αβ", "other"] {
            assert_oracle(&documents, prefix, "Plain Text", &mut cache);
            assert_eq!(cache.documents.len(), MAX_DOCUMENTS);
            assert_eq!(
                cache.builds, MAX_DOCUMENTS,
                "sequential tabs thrashed the cache"
            );
            assert!(
                cache
                    .documents
                    .iter()
                    .any(|document| document.document_id == 0)
            );
        }
    }
    // A newly active tab gets a slot before any inactive tab is traversed.
    documents[0].active = false;
    documents[7].active = true;
    assert_oracle(&documents, "zz", "Plain Text", &mut cache);
    assert!(
        cache
            .documents
            .iter()
            .any(|document| document.document_id == 7)
    );
    let builds = cache.builds;
    for _ in 0..3 {
        documents[7].rope.insert(2, "x");
        assert_oracle(&documents, "pri", "Plain Text", &mut cache);
        assert_eq!(cache.builds, builds);
    }
    // Closed/reordered documents and counter wrap cannot pin obsolete slots.
    documents.drain(0..6);
    cache.use_counter = u64::MAX;
    assert_oracle(&documents, "zz", "Plain Text", &mut cache);
    assert!(cache.documents.len() <= MAX_DOCUMENTS);
    assert_eq!(cache.use_counter, 1);
    let new = snapshot(99, "print_new", 0, true);
    assert_oracle(&[new], "pri", "Plain Text", &mut cache);
    assert!(
        cache
            .documents
            .iter()
            .any(|document| document.document_id == 99)
    );
}

#[test]
#[ignore = "manual optimized-profile cache admission comparison"]
fn benchmark_low_end_vtcomplete_tabs() {
    use std::time::Instant;
    let documents: Vec<_> = (0..6)
        .map(|id| {
            snapshot(
                id,
                &format!("print_{id} αβ{id} other_value repeated_word\n").repeat(2_000),
                1_000,
                id == 0,
            )
        })
        .collect();
    let mut times = [Vec::new(), Vec::new()];
    for round in 0..7 {
        for reference in [round % 2 == 0, round % 2 != 0] {
            let mut cache = TokenCache {
                reference_admission: reference,
                ..Default::default()
            };
            compute(documents.iter(), "zz", "Plain Text", &mut cache, || false).unwrap();
            let warm_builds = cache.builds;
            let started = Instant::now();
            for _ in 0..8 {
                for prefix in ["zz", "pri", "other"] {
                    let result =
                        compute(documents.iter(), prefix, "Plain Text", &mut cache, || false)
                            .unwrap();
                    assert_eq!(result.outcome, VtCompleteWorkOutcome::Exact);
                    std::hint::black_box(result);
                }
            }
            times[usize::from(reference)].push(started.elapsed() / 24);
            if round == 6 {
                eprintln!(
                    "completion reference={reference}: rebuilt indexes={}",
                    cache.builds - warm_builds
                );
            }
            for prefix in ["zz", "pri", "other"] {
                assert_oracle(&documents, prefix, "Plain Text", &mut cache);
            }
        }
    }
    for samples in &mut times {
        samples.sort();
    }
    eprintln!(
        "6-tab completion per query: before={:?}, after={:?}",
        times[1][3], times[0][3]
    );
}

fn snapshot(id: u64, text: &str, cursor_line: usize, active: bool) -> VtCompleteSnapshot {
    VtCompleteSnapshot {
        source: Default::default(),
        document_id: id,
        rope: ropey::Rope::from_str(text),
        cursor_line,
        cursor_col: 0,
        active,
    }
}

fn assert_oracle(
    snapshots: &[VtCompleteSnapshot],
    prefix: &str,
    language: &str,
    cache: &mut TokenCache,
) {
    let expected = original_compute_vtcomplete_suggestions(
        snapshots.to_vec(),
        prefix,
        language,
        &CancellationToken::new(),
    );
    let actual = compute(snapshots.iter(), prefix, language, cache, || false)
        .map(|result| result.suggestions);
    assert_eq!(actual, expected, "prefix={prefix:?}, language={language:?}");
}

#[test]
fn cached_completion_matches_original_ranking_threshold_and_unicode() {
    let mut cache = TokenCache::default();
    let many = (0..150)
        .map(|i| format!("prefix_{i:03} "))
        .collect::<String>();
    let snapshots = vec![
        snapshot(
            1,
            &format!(
                "{many}\nprint_line Print_Line pair_right\nαβγ αβδε İdent ident i̇dent\nprint_result print_line"
            ),
            2,
            true,
        ),
        snapshot(
            2,
            "print_value println print_long print_value\nαβζ Σσς 変数名 変数二\n",
            0,
            false,
        ),
        snapshot(
            3,
            "print_other pair_other unrelated unrelated unrelated",
            0,
            false,
        ),
    ];
    for _ in 0..3 {
        for prefix in [
            "pri",
            "prefix",
            "prl",
            "p_r",
            "αβ",
            "変数",
            "İd",
            "i̇",
            "zz",
            "print_line",
        ] {
            for language in ["Plain Text", "Rust", "Python", "TypeScript"] {
                assert_oracle(&snapshots, prefix, language, &mut cache);
            }
        }
    }
}

#[test]
fn incremental_index_matches_original_through_edits_undo_and_external_replacement() {
    let mut cache = TokenCache::default();
    let mut current = snapshot(
        1,
        &"print_line αβγ value_42 print_result\r\n変数名 next_value\n".repeat(160),
        130,
        true,
    );
    let others = [
        snapshot(2, "print_other println αβδε 変数二\n", 0, false),
        snapshot(3, "other_value next_item", 0, false),
    ];
    let mut history = Vec::new();
    let mut random = 0x51ca_fe89u64;
    for round in 0..240 {
        history.push(current.rope.clone());
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        let at = random as usize % (current.rope.len_chars() + 1);
        match round % 7 {
            0 => current.rope.insert(at, "print_added "),
            1 => current.rope.insert(at, "\n"),
            2 => current.rope.insert(at, "界α🙂\r\n"),
            3 if at < current.rope.len_chars() => current
                .rope
                .remove(at..(at + 5).min(current.rope.len_chars())),
            4 => current.rope = ropey::Rope::from_str(&current.rope.to_string()),
            5 if history.len() > 4 => current.rope = history[history.len() - 4].clone(),
            _ => current.rope.insert(at, "_"),
        }
        current.cursor_line = round * 19 % current.rope.len_lines();
        let snapshots = vec![current.clone(), others[0].clone(), others[1].clone()];
        for prefix in ["pri", "αβ", "変数", "zz", "next"] {
            assert_oracle(
                &snapshots,
                prefix,
                if round % 2 == 0 { "Rust" } else { "Plain Text" },
                &mut cache,
            );
        }
    }
}

fn assert_line_change(old: &ropey::Rope, new: &ropey::Rope) {
    let (old_range, new_range) = changed_lines(old, new, &|| false).unwrap();
    let mut lines: Vec<_> = old.lines().map(|line| line.to_string()).collect();
    lines.splice(
        old_range,
        new.lines()
            .skip(new_range.start)
            .take(new_range.len())
            .map(|line| line.to_string()),
    );
    assert_eq!(
        lines,
        new.lines().map(|line| line.to_string()).collect::<Vec<_>>(),
        "old={old:?} new={new:?}"
    );
}

#[test]
fn shared_chunk_change_ranges_preserve_lines_and_partial_utf8_boundaries() {
    let samples = [
        "",
        "\n",
        "\r",
        "\r\n",
        "a",
        "a\r",
        "a\r\n",
        "a\n",
        "a\nb",
        "a\nb\n",
        "界",
        "畍",
        "α",
        "β",
        "🙂",
        "🙃",
        "界\nα\r\n🙂",
        "畍\nβ\r\n🙃",
    ];
    for old in samples {
        for new in samples {
            assert_line_change(&ropey::Rope::from_str(old), &ropey::Rope::from_str(new));
        }
    }
    let mut short = vec![String::new()];
    for _ in 0..3 {
        let previous = short.clone();
        for prefix in previous {
            for ch in ['a', '\r', '\n', '界'] {
                short.push(format!("{prefix}{ch}"));
            }
        }
        short.sort();
        short.dedup();
    }
    let short: Vec<_> = short
        .iter()
        .map(|text| ropey::Rope::from_str(text))
        .collect();
    for old in &short {
        for new in &short {
            assert_line_change(old, new);
        }
    }
    let original = ropey::Rope::from_str(&"source_word 界 αβ🙂\r\n".repeat(2000));
    for position in [
        0,
        1,
        1023,
        2050,
        original.len_chars() / 2,
        original.len_chars(),
    ] {
        for insert in ["x", "\n", "α\r\n", "🙂", "\r"] {
            let mut changed = original.clone();
            changed.insert(position, insert);
            assert_line_change(&original, &changed);
            assert_line_change(&changed, &original);
        }
    }
}

#[test]
fn local_edit_invalidates_only_changed_line_postings() {
    let mut cache = TokenCache::default();
    let mut current = snapshot(1, &"print_line other_word\n".repeat(200), 100, true);
    compute(
        std::iter::once(&current),
        "zz",
        "Plain Text",
        &mut cache,
        || false,
    )
    .unwrap();
    let LineIndex::Cached(previous) = &cache.documents[0].lines[98] else {
        panic!("line not cached");
    };
    let previous = previous.as_ptr();
    current
        .rope
        .insert(current.rope.line_to_char(99) + 2, "new_");
    cache.prepare(&current, &|| false).unwrap();
    let index = &cache.documents[0];
    let LineIndex::Cached(retained) = &index.lines[98] else {
        panic!("line cache lost");
    };
    assert_eq!(retained.as_ptr(), previous);
    assert!(matches!(index.lines[99], LineIndex::Missing));
    assert!(matches!(index.lines[100], LineIndex::Cached(_)));
    assert_eq!(
        index
            .lines
            .iter()
            .filter(|line| matches!(line, LineIndex::Missing))
            .count(),
        1
    );
    assert_oracle(&[current], "pri", "Plain Text", &mut cache);
}

#[test]
fn indexing_limits_preserve_exact_streaming_results_and_bound_retention() {
    let mut cache = TokenCache::default();
    let many_words = (0..MAX_WORDS + 150)
        .map(|i| format!("word_{i} "))
        .collect::<String>();
    for id in 0..MAX_DOCUMENTS + 3 {
        let current = snapshot(
            id as u64,
            &format!("{many_words}\nprint_line print_other\n"),
            1,
            true,
        );
        assert_oracle(&[current], "zz", "Rust", &mut cache);
        assert!(cache.documents.len() <= MAX_DOCUMENTS);
        for doc in &cache.documents {
            assert!(doc.words.len() <= MAX_WORDS);
            assert!(doc.word_bytes <= MAX_WORD_BYTES);
            assert!(doc.postings <= MAX_POSTINGS);
            assert!(doc.lines.len() <= MAX_DOCUMENT_LINES);
            assert!(doc.rope.len_bytes() <= MAX_DOCUMENT_BYTES);
        }
    }
    for text in [
        "print_line unrelated ".repeat(64 * 1024 / 10),
        "print_line unrelated\n".repeat(MAX_DOCUMENT_BYTES / 10),
    ] {
        let current = snapshot(99, &text, 0, true);
        assert_oracle(
            std::slice::from_ref(&current),
            "pri",
            "Plain Text",
            &mut cache,
        );
        if text.len() > MAX_DOCUMENT_BYTES {
            assert!(cache.documents.iter().all(|doc| doc.document_id != 99));
        } else {
            assert!(matches!(
                cache
                    .documents
                    .iter()
                    .find(|doc| doc.document_id == 99)
                    .unwrap()
                    .lines[0],
                LineIndex::Cached(_)
            ));
        }
    }
    let mut scores = Scores::new(
        "zz",
        "zz",
        crate::fuzzy_score::FuzzyWorkBudget::new(scoring::work_limits()),
    );
    for index in 0..MAX_REJECTED_WORDS * 2 {
        scores
            .add(&format!("word_{index}"), index, false, &|| false)
            .unwrap();
    }
    assert!(scores.rejected.len() <= MAX_REJECTED_WORDS);
    assert!(scores.rejected_bytes <= MAX_REJECTED_BYTES);
}

#[test]
fn cancelled_work_stops_within_a_single_large_line_and_shared_chunk_walk() {
    use std::cell::Cell;
    let rope = ropey::Rope::from_str(&"print_line unrelated ".repeat(100_000));
    let checks = Cell::new(0);
    let words = Cell::new(0);
    let mut scores = Scores::new(
        "zz",
        "zz",
        crate::fuzzy_score::FuzzyWorkBudget::new(scoring::work_limits()),
    );
    assert!(
        visit_words(
            rope.line(0),
            &mut scores,
            &mut String::new(),
            &|| {
                checks.set(checks.get() + 1);
                checks.get() >= 3
            },
            |_, _| {
                words.set(words.get() + 1);
                Some(())
            }
        )
        .is_none()
    );
    assert!(
        words.get() < 1000,
        "cancelled scan traversed the large logical line"
    );
    let mut changed = rope.clone();
    changed.insert(changed.len_chars() / 2, "x");
    checks.set(0);
    assert!(
        changed_lines(&rope, &changed, &|| {
            checks.set(checks.get() + 1);
            checks.get() >= 3
        })
        .is_none()
    );
}

#[test]
fn deferred_prefix_matches_editor_for_unicode_carets_and_huge_identifiers() {
    use crate::editor::Editor;
    use std::cell::Cell;
    for text in [
        "",
        "a",
        "_",
        "α",
        "界",
        "🙂",
        "ab",
        "αb",
        "a\u{301}",
        "_x\nβγ\r\n界 value",
        "α🙂_界",
    ] {
        let mut editor = Editor::test_with_text(text);
        for line in 0..editor.rope.len_lines() {
            for col in 0..editor.rope.line(line).len_chars() + 3 {
                editor.cursor_line = line;
                editor.cursor_col = col;
                let expected = editor
                    .word_prefix_at_cursor()
                    .filter(|prefix| prefix.len() >= 2);
                let snapshot = VtCompleteSnapshot {
                    source: Default::default(),
                    rope: editor.rope.clone(),
                    document_id: editor.navigation_id,
                    cursor_line: line,
                    cursor_col: col,
                    active: true,
                };
                assert_eq!(
                    has_vtcomplete_prefix(&editor),
                    editor
                        .word_prefix_at_cursor()
                        .is_some_and(|word| !word.is_empty()),
                    "{text:?} {line}:{col}"
                );
                assert_eq!(
                    resolve_prefix(&snapshot, &|| false).and_then(|prefix| match prefix {
                        ResolvedPrefix::Text(text) => Some(text),
                        ResolvedPrefix::WorkLimited => None,
                    }),
                    expected,
                    "{text:?} {line}:{col}"
                );
            }
        }
    }
    let text = "識別名".repeat(100_000);
    let mut editor = Editor::test_with_text(&text);
    editor.cursor_col = editor.rope.len_chars();
    assert!(has_vtcomplete_prefix(&editor));
    let snapshot = VtCompleteSnapshot {
        source: Default::default(),
        rope: editor.rope.clone(),
        document_id: editor.navigation_id,
        cursor_line: 0,
        cursor_col: editor.cursor_col,
        active: true,
    };
    let checks = Cell::new(0);
    assert!(
        resolve_prefix(&snapshot, &|| {
            checks.set(checks.get() + 1);
            checks.get() >= 3
        })
        .is_none()
    );
    assert!(matches!(
        resolve_prefix(&snapshot, &|| false),
        Some(ResolvedPrefix::WorkLimited)
    ));
}

fn request(generation: u64, sender: BackgroundResultSender) -> VtCompleteRequest {
    let mut active = snapshot(1, "pri print_line print_value", 0, true);
    active.cursor_col = 3;
    VtCompleteRequest {
        active,
        edit_baseline: None,
        index: Default::default(),
        explicit: false,
        language: "Plain Text".into(),
        generation,
        active_group: 0,
        active_tab: 0,
        content_version: generation,
        cancel: CancellationToken::new(),
        sender,
    }
}

#[test]
fn latest_pending_request_replaces_backlog_and_retains_worker_cache() {
    let mut background = crate::background::BackgroundWorkers::new();
    let worker = VtCompleteWorker::default();
    let mut cancelled = Vec::new();
    for generation in 0..200 {
        let request = request(generation, background.sender.clone());
        cancelled.push(request.cancel.clone());
        assert_eq!(worker.push(request), generation == 0);
    }
    assert!(cancelled[..199].iter().all(CancellationToken::is_cancelled));
    assert!(!cancelled[199].is_cancelled());
    VtCompleteWorker::run(Arc::clone(&worker.queue));
    let results = background.poll();
    assert_eq!(results.len(), 1);
    assert!(matches!(
        &results[0],
        BackgroundResult::VtComplete {
            generation: 199,
            ..
        }
    ));
    let queue = worker.queue.lock().unwrap();
    assert!(!queue.running);
    assert!(queue.pending.is_none());
    assert_eq!(queue.cache.as_ref().unwrap().documents.len(), 1);
    drop(queue);
    assert!(worker.push(request(200, background.sender.clone())));
    VtCompleteWorker::run(Arc::clone(&worker.queue));
    assert_eq!(background.poll().len(), 1);
}

#[test]
fn cancelled_pending_request_never_publishes() {
    let mut background = crate::background::BackgroundWorkers::new();
    let worker = VtCompleteWorker::default();
    let request = request(1, background.sender.clone());
    request.cancel.cancel();
    assert!(worker.push(request));
    VtCompleteWorker::run(Arc::clone(&worker.queue));
    assert!(background.poll().is_empty());
}

#[test]
fn dropping_owner_cancels_active_and_pending_work_without_waiting_for_worker() {
    use std::{sync::mpsc, time::Duration};
    let background = crate::background::BackgroundWorkers::new();
    let worker = VtCompleteWorker::default();
    let active = request(1, background.sender.clone());
    let active_cancel = active.cancel.clone();
    assert!(worker.push(active));
    let queue = Arc::clone(&worker.queue);
    let worker_queue = Arc::clone(&queue);
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let thread = std::thread::spawn(move || {
        let mut processed = 0;
        VtCompleteWorker::run_with(worker_queue, |request, _, _| {
            processed += 1;
            started_tx.send(request.generation).unwrap();
            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(request.cancel.is_cancelled());
        });
        processed
    });
    assert_eq!(started_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
    let pending = request(2, background.sender.clone());
    let pending_cancel = pending.cancel.clone();
    assert!(!worker.push(pending));
    drop(worker);
    // The worker is deliberately blocked. Dropping the owner must cancel
    // both tokens immediately without joining it or acquiring a parsing lock.
    assert!(active_cancel.is_cancelled());
    assert!(pending_cancel.is_cancelled());
    assert!(queue.lock().unwrap().pending.is_none());
    release_tx.send(()).unwrap();
    assert_eq!(thread.join().unwrap(), 1);
    let queue = queue.lock().unwrap();
    assert!(!queue.running);
    assert!(queue.active_cancel.is_none());
}

#[test]
fn inactive_snapshots_are_reused_until_exact_sources_order_or_eligibility_change() {
    use crate::{app::Tab, editor::Editor};
    let mut worker = VtCompleteWorker::default();
    let mut tabs: Vec<_> = ["pri print_line", "print_other", "print_value"]
        .into_iter()
        .map(|text| Tab::new_file(Editor::test_with_text(text)))
        .collect();
    let (_, first) = worker.snapshots(&tabs, 0);
    tabs[0].editor.insert_char('x');
    let (_, next) = worker.snapshots(&tabs, 0);
    assert!(Arc::ptr_eq(&first, &next));
    tabs[1].editor.insert_char('x');
    let (_, changed) = worker.snapshots(&tabs, 0);
    assert!(!Arc::ptr_eq(&next, &changed));
    tabs.swap(1, 2);
    let (_, reordered) = worker.snapshots(&tabs, 0);
    assert!(!Arc::ptr_eq(&changed, &reordered));
    tabs[1].editor.bg_loading = true;
    let (_, loading) = worker.snapshots(&tabs, 0);
    assert_eq!(loading.len(), 1);
    tabs[2].editor.large_file_mode = true;
    let (_, large) = worker.snapshots(&tabs, 0);
    assert!(large.is_empty());
}

#[test]
fn oversized_snapshot_lists_do_not_retain_closed_documents_after_requests_finish() {
    use crate::{app::Tab, editor::Editor};
    let mut worker = VtCompleteWorker::default();
    let mut tabs: Vec<_> = (0..MAX_RETAINED_SNAPSHOTS + 2)
        .map(|_| Tab::new_file(Editor::test_with_text("pri print_line")))
        .collect();
    let (_, first) = worker.snapshots(&tabs, 0);
    assert!(worker.retained_inactive_snapshots.is_none());
    let (_, second) = worker.snapshots(&tabs, 0);
    assert!(
        Arc::ptr_eq(&first, &second),
        "an active typing backlog still shares oversized lists"
    );
    tabs.truncate(1);
    drop(first);
    drop(second);
    assert!(
        worker.inactive_snapshots.upgrade().is_none(),
        "closed document snapshots survived their requests"
    );

    tabs.push(Tab::new_file(Editor::test_with_text(
        &"x".repeat(MAX_RETAINED_SNAPSHOT_BYTES + 1),
    )));
    let (_, large) = worker.snapshots(&tabs, 0);
    assert!(worker.retained_inactive_snapshots.is_none());
    tabs.truncate(1);
    drop(large);
    assert!(worker.inactive_snapshots.upgrade().is_none());
}

#[test]
#[ignore = "manual release vtcomplete traversal and incremental indexing comparison"]
fn benchmark_vtcomplete_index_and_local_edits() {
    use std::{hint::black_box, time::Instant};
    let documents = vec![
        snapshot(
            1,
            &"print_line print_result unrelated repeated_word αβγ\n".repeat(10_000),
            5000,
            true,
        ),
        snapshot(
            2,
            &"print_other other_result unrelated repeated_word αβδε\n".repeat(10_000),
            0,
            false,
        ),
    ];
    let queries = ["pri", "prl", "zz", "αβ", "res", "unr"];
    let cancel = CancellationToken::new();
    let mut before = Vec::new();
    let mut after = Vec::new();
    for round in 0..7 {
        let indexed = || {
            let mut cache = TokenCache::default();
            compute(documents.iter(), "zz", "Plain Text", &mut cache, || {
                cancel.is_cancelled()
            })
            .unwrap();
            let started = Instant::now();
            let mut current = documents.clone();
            for edit in 0..12 {
                let at = current[0].rope.line_to_char(5000) + 2;
                current[0].rope.insert(at, "x");
                for prefix in queries {
                    black_box(
                        compute(current.iter(), prefix, "Plain Text", &mut cache, || {
                            cancel.is_cancelled()
                        })
                        .unwrap(),
                    );
                }
                black_box(edit);
            }
            let elapsed = started.elapsed();
            for prefix in queries {
                assert_oracle(&current, prefix, "Plain Text", &mut cache);
            }
            elapsed
        };
        let original = || {
            let started = Instant::now();
            let mut current = documents.clone();
            for _ in 0..12 {
                let at = current[0].rope.line_to_char(5000) + 2;
                current[0].rope.insert(at, "x");
                for prefix in queries {
                    black_box(
                        original_compute_vtcomplete_suggestions(
                            current.clone(),
                            prefix,
                            "Plain Text",
                            &cancel,
                        )
                        .unwrap(),
                    );
                }
            }
            started.elapsed()
        };
        if round % 2 == 0 {
            before.push(original());
            after.push(indexed());
        } else {
            after.push(indexed());
            before.push(original());
        }
    }
    before.sort();
    after.sort();
    eprintln!(
        "vtcomplete 2×10000 lines / 12 local edits × 6 prefixes: original={:?}, indexed={:?}",
        before[3], after[3]
    );
}

#[test]
fn canonical_fresh_scores_remove_retained_history_and_preserve_unicode_prefixes() {
    let mut scores = Scores::new(
        "aaa",
        "aaa",
        crate::fuzzy_score::FuzzyWorkBudget::new(scoring::work_limits()),
    );
    scores.add("a_aa", 0, false, &|| false).unwrap();
    scores.add("aaaa", 0, false, &|| false).unwrap();
    assert_eq!(scores.matches["aaaa"].fuzzy_score, 71);
    assert!(scores.matches["aaaa"].prefix_match);
    for (query, candidate) in [
        ("a_", "A_identifier"),
        ("_a", "_A_name"),
        ("İ", "İdentifier"),
        ("ΟΣ", "ΟΣA"),
        ("ος", "ος_suffix"),
        ("変数", "変数名"),
        ("α_", "α_識別名"),
    ] {
        let lower = query.to_lowercase();
        let mut scores = Scores::new(
            query,
            &lower,
            crate::fuzzy_score::FuzzyWorkBudget::new(scoring::work_limits()),
        );
        scores.add(candidate, 0, false, &|| false).unwrap();
        let expected = fuzzy_matcher::FuzzyMatcher::fuzzy_match(
            &fuzzy_matcher::skim::SkimMatcherV2::default(),
            &candidate.to_lowercase(),
            &lower,
        );
        assert_eq!(
            scores.matches.get(candidate).map(|score| score.fuzzy_score),
            expected,
            "{candidate:?}/{query:?}"
        );
    }
}

#[test]
fn work_limits_preserve_prefixes_keywords_and_distinguish_deferred_from_rejected() {
    let limits = crate::fuzzy_score::FuzzyWorkLimits {
        candidate_cells: 1,
        request_cells: 1,
        ..scoring::work_limits()
    };
    let mut scores = Scores::new("as", "as", crate::fuzzy_score::FuzzyWorkBudget::new(limits));
    scores.add("ab_s", 0, false, &|| false).unwrap();
    assert!(scores.deferred.contains("ab_s"));
    assert!(!scores.rejected.contains("ab_s"));
    scores.add("async", 0, true, &|| false).unwrap();
    scores.add("as_value", 1, false, &|| false).unwrap();
    let result = scores.finish();
    assert_eq!(result.outcome, VtCompleteWorkOutcome::FuzzyWorkLimited);
    assert_eq!(result.suggestions, ["async", "as_value"]);
    assert!(result.deferred_candidates > 0);

    let current = snapshot(1, "unrelated document words", 0, true);
    let result = compute_with_limits(
        std::iter::once(&current),
        "as",
        "Rust",
        &mut TokenCache::default(),
        crate::fuzzy_score::FuzzyWorkLimits {
            request_bytes: 4,
            ..scoring::work_limits()
        },
        || false,
    )
    .unwrap();
    assert_eq!(result.outcome, VtCompleteWorkOutcome::FuzzyWorkLimited);
    assert!(result.suggestions.iter().any(|word| word == "async"));
}

#[test]
fn oversized_tokens_and_partial_scans_are_bounded_and_recover_exactly() {
    let current = snapshot(
        1,
        &format!("pri print_{} print_short", "x".repeat(200_000)),
        0,
        true,
    );
    let mut cache = TokenCache::default();
    let result = compute(
        std::iter::once(&current),
        "pri",
        "Plain Text",
        &mut cache,
        || false,
    )
    .unwrap();
    assert_eq!(result.outcome, VtCompleteWorkOutcome::FuzzyWorkLimited);
    assert_eq!(result.suggestions, ["print_short"]);
    assert!(result.deferred_candidates > 0);
    assert!(
        cache.documents[0]
            .words
            .iter()
            .all(|word| word.len() <= MAX_COMPLETION_WORD_BYTES)
    );
    assert!(matches!(cache.documents[0].lines[0], LineIndex::Streaming));

    let current = snapshot(2, &"print_line unrelated\n".repeat(200), 0, true);
    let result = compute_with_limits(
        std::iter::once(&current),
        "pri",
        "Plain Text",
        &mut cache,
        crate::fuzzy_score::FuzzyWorkLimits {
            request_bytes: 256,
            ..scoring::work_limits()
        },
        || false,
    )
    .unwrap();
    assert_eq!(result.outcome, VtCompleteWorkOutcome::FuzzyWorkLimited);
    assert_eq!(result.suggestions, ["print_line"]);
    let result = compute(
        std::iter::once(&current),
        "pri",
        "Plain Text",
        &mut cache,
        || false,
    )
    .unwrap();
    assert_eq!(result.outcome, VtCompleteWorkOutcome::Exact);
    assert_oracle(&[current], "pri", "Plain Text", &mut cache);
}

#[test]
fn cancelled_candidate_is_neither_rejected_nor_deferred() {
    use std::cell::Cell;
    let mut scores = Scores::new(
        "aaa",
        "aaa",
        crate::fuzzy_score::FuzzyWorkBudget::new(scoring::work_limits()),
    );
    let checks = Cell::new(0);
    assert!(
        scores
            .add("aaaa", 0, false, &|| {
                checks.set(checks.get() + 1);
                checks.get() >= 2
            })
            .is_none()
    );
    assert!(scores.matches.is_empty());
    assert!(scores.rejected.is_empty());
    assert!(scores.deferred.is_empty());
}

#[test]
#[ignore = "manual release comparison of exact long prefixes and bounded non-prefix work"]
fn benchmark_vtcomplete_budgeted_identifiers() {
    use std::{hint::black_box, time::Instant};
    let query = "a".repeat(2048);
    let candidate = format!("{query}{}", "suffix".repeat(250));
    let expected = 31 + 20 * (query.len() as i64 - 1);
    let mut original = Vec::new();
    let mut optimized = Vec::new();
    for round in 0..7 {
        let reference = || {
            let started = Instant::now();
            let score = fuzzy_matcher::FuzzyMatcher::fuzzy_match(
                &fuzzy_matcher::skim::SkimMatcherV2::default(),
                &candidate,
                &query,
            );
            let elapsed = started.elapsed();
            assert_eq!(score, Some(expected));
            elapsed
        };
        let prepared = || {
            let started = Instant::now();
            let mut scores = Scores::new(
                &query,
                &query,
                crate::fuzzy_score::FuzzyWorkBudget::new(scoring::work_limits()),
            );
            scores
                .add(black_box(&candidate), 0, false, &|| false)
                .unwrap();
            let elapsed = started.elapsed();
            assert_eq!(scores.matches[&candidate].fuzzy_score, expected);
            elapsed
        };
        if round % 2 == 0 {
            original.push(reference());
            optimized.push(prepared());
        } else {
            optimized.push(prepared());
            original.push(reference());
        }
    }
    original.sort();
    optimized.sort();
    let query = format!("{}b", "a".repeat(1024));
    let candidate = format!("{}bb", "a".repeat(2000));
    let mut scores = Scores::new(
        &query,
        &query,
        crate::fuzzy_score::FuzzyWorkBudget::new(scoring::work_limits()),
    );
    let started = Instant::now();
    scores.add(&candidate, 0, false, &|| false).unwrap();
    let limited = started.elapsed();
    assert!(scores.deferred.contains(candidate.as_str()));
    assert!(!scores.rejected.contains(candidate.as_str()));
    eprintln!(
        "2048-scalar exact prefix /3548-byte word: fresh={:?}, prepared={:?}; bounded non-prefix={limited:?}",
        original[3], optimized[3]
    );
}

fn original_add_vtcomplete_candidate(
    word_scores: &mut rustc_hash::FxHashMap<String, VtCompleteCandidate>,
    candidate: &str,
    prefix: &str,
    prefix_lower: &str,
    proximity: usize,
    keyword: bool,
) {
    if candidate.len() < 2 || candidate == prefix || candidate.eq_ignore_ascii_case(prefix) {
        return;
    }

    // Scores depend on the word and the query, which stay fixed throughout
    // this request. Repeated occurrences can only improve proximity/priority.
    if let Some(entry) = word_scores.get_mut(candidate) {
        entry.proximity = entry.proximity.min(proximity);
        entry.keyword |= keyword;
        return;
    }

    let candidate_lower = candidate.to_lowercase();
    let prefix_match = candidate.starts_with(prefix) || candidate_lower.starts_with(prefix_lower);
    let Some(fuzzy_score) = fuzzy_matcher::FuzzyMatcher::fuzzy_match(
        &fuzzy_matcher::skim::SkimMatcherV2::default(),
        &candidate_lower,
        prefix_lower,
    ) else {
        return;
    };

    word_scores.insert(
        candidate.to_string(),
        VtCompleteCandidate {
            proximity,
            fuzzy_score,
            prefix_match,
            keyword,
        },
    );
}

fn original_compute_vtcomplete_suggestions(
    snapshots: Vec<VtCompleteSnapshot>,
    prefix: &str,
    language: &str,
    cancel: &crate::background::CancellationToken,
) -> Option<Vec<String>> {
    const WORD_LIMIT: usize = 50;
    const SUGGESTION_LIMIT: usize = 10;
    let prefix_lower = prefix.to_lowercase();
    let mut word_scores: rustc_hash::FxHashMap<String, VtCompleteCandidate> =
        rustc_hash::FxHashMap::default();

    let scan_line =
        |rope: &ropey::Rope,
         line_idx: usize,
         proximity: usize,
         scores: &mut rustc_hash::FxHashMap<String, VtCompleteCandidate>| {
            let mut current_word = String::new();
            for chunk in rope.line(line_idx).chunks() {
                for ch in chunk.chars() {
                    if ch.is_alphanumeric() || ch == '_' {
                        current_word.push(ch);
                    } else {
                        if current_word.len() >= 2 {
                            original_add_vtcomplete_candidate(
                                scores,
                                &current_word,
                                prefix,
                                &prefix_lower,
                                proximity,
                                false,
                            );
                        }
                        current_word.clear();
                    }
                }
            }
            if current_word.len() >= 2 {
                original_add_vtcomplete_candidate(
                    scores,
                    &current_word,
                    prefix,
                    &prefix_lower,
                    proximity,
                    false,
                );
            }
        };

    // Search the active document from the cursor outward. This gives nearby
    // symbols priority without a file-size radius cutoff; if few candidates
    // match, the worker continues through the entire rope.
    for snapshot in snapshots.iter().filter(|snapshot| snapshot.active) {
        let line_count = snapshot.rope.len_lines();
        for distance in 0..line_count {
            if cancel.is_cancelled() {
                return None;
            }
            if let Some(line) = snapshot.cursor_line.checked_sub(distance) {
                scan_line(&snapshot.rope, line, distance, &mut word_scores);
            }
            if distance > 0 {
                let line = snapshot.cursor_line.saturating_add(distance);
                if line < line_count {
                    scan_line(&snapshot.rope, line, distance, &mut word_scores);
                }
            }
            if word_scores.len() >= WORD_LIMIT {
                break;
            }
        }
    }
    for snapshot in snapshots.iter().filter(|snapshot| !snapshot.active) {
        for line in 0..snapshot.rope.len_lines() {
            if cancel.is_cancelled() {
                return None;
            }
            scan_line(&snapshot.rope, line, 1000 + line, &mut word_scores);
            if word_scores.len() >= WORD_LIMIT {
                break;
            }
        }
        if word_scores.len() >= WORD_LIMIT {
            break;
        }
    }

    for &keyword in language_keywords(language) {
        original_add_vtcomplete_candidate(
            &mut word_scores,
            keyword,
            prefix,
            &prefix_lower,
            0,
            true,
        );
    }
    let mut sorted: Vec<(String, VtCompleteCandidate)> = word_scores.into_iter().collect();
    sorted.sort_unstable_by(|a, b| {
        b.1.keyword
            .cmp(&a.1.keyword)
            .then_with(|| b.1.prefix_match.cmp(&a.1.prefix_match))
            .then_with(|| b.1.fuzzy_score.cmp(&a.1.fuzzy_score))
            .then_with(|| a.1.proximity.cmp(&b.1.proximity))
            .then_with(|| a.0.cmp(&b.0))
    });
    Some(
        sorted
            .into_iter()
            .take(SUGGESTION_LIMIT)
            .map(|(word, _)| word)
            .collect(),
    )
}
