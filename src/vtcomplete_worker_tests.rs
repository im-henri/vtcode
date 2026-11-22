use super::*;
use crate::rope_changes::changed_lines;
#[path = "vtcomplete_scheduling_tests.rs"]
mod scheduling_tests;

fn contextual(text: &str, query: &str) -> Vec<String> {
    let mut active = snapshot(1, text, text.lines().count().saturating_sub(1));
    active.cursor_col = active.rope.line(active.cursor_line).len_chars();
    compute_contextual(
        &active,
        query,
        "Unspecified Language",
        &Default::default(),
        &mut CompletionCache::default(),
        || false,
    )
    .unwrap()
    .suggestions
}

#[test]
fn occurrence_target_can_be_bound_by_a_later_visible_use_in_the_full_worker() {
    let before = "dispatch(payloadOld, channelOld); validate(payloadOld)\ndispatch(pay";
    let text = format!("{before}, channelNew); validate(payloadNew)");
    let mut active = snapshot(91, &text, 1);
    active.cursor_col = before.rsplit('\n').next().unwrap().chars().count();
    for explicit in [false, true] {
        let result = compute_contextual_invoked(
            &active,
            "pay",
            "Unknown language",
            &Default::default(),
            &mut CompletionCache::default(),
            explicit,
            || false,
        )
        .unwrap();
        assert_eq!(
            result.suggestions.first().map(String::as_str),
            Some("payloadNew")
        );
        assert_eq!(result.suggestions.len(), result.details.len());
    }
}

#[test]
fn more_specific_literal_occurrence_survives_an_optional_target_mapping_collision() {
    let before = "choose(channel, rightPayload, finalMode)\nprepare(channel); choose(channel, rightSpecific, finalMode)\nprepare(channel); choose(channel, ri";
    let text = format!("{before}, finalMode)");
    let mut active = snapshot(92, &text, 2);
    active.cursor_col = before.rsplit('\n').next().unwrap().chars().count();
    let result = compute_contextual_invoked(
        &active,
        "ri",
        "Unknown language",
        &Default::default(),
        &mut CompletionCache::default(),
        false,
        || false,
    )
    .unwrap();
    assert_eq!(
        result.suggestions.first().map(String::as_str),
        Some("rightSpecific")
    );
}

#[test]
fn compatible_shorter_context_survives_an_incompatible_longer_example() {
    let before = "choose(channel, rightPayload, finalMode)\nprepare(channel); choose(channel, rightWrong, wrongMode)\nprepare(channel); choose(channel, ri";
    let text = format!("{before}, finalMode)");
    let mut active = snapshot(92, &text, 2);
    active.cursor_col = before.rsplit('\n').next().unwrap().chars().count();
    for explicit in [false, true] {
        let result = compute_contextual_invoked(
            &active,
            "ri",
            "Unknown language",
            &Default::default(),
            &mut CompletionCache::default(),
            explicit,
            || false,
        )
        .unwrap();
        assert_eq!(
            result.suggestions.first().map(String::as_str),
            Some("rightPayload")
        );
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
    let mut active = snapshot(1, "renderFrame()\nrenderFrame()\nren", 2);
    active.cursor_col = 3;
    let mut options = PopupOptions::default();
    let mut settings = options.preferences;
    settings.complete_calls = false;
    settings.min_prefix_chars = 3;
    settings.context_triggers = false;
    options.preferences = settings;
    let result = compute_contextual(
        &active,
        "ren",
        "Unknown",
        &options,
        &mut CompletionCache::default(),
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
fn explicit_completion_supports_one_letter_and_empty_member_prefixes() {
    for (text, col, expected) in [("obj.x", 5, "x"), ("obj.", 4, "")] {
        let mut active = snapshot(1, text, 0);
        active.cursor_col = col;
        assert!(matches!(resolve_prefix_with_minimum(&active, 0, &|| false),
            Some(ResolvedPrefix::Text(prefix)) if prefix == expected));
    }
    let words = contextual("logger.printLine()\nlogger.printLine()\nlogger.", "");
    assert_eq!(words.first().map(String::as_str), Some("printLine()"));
}

#[test]
fn automatic_completion_stays_quiet_in_quotes_and_allows_comments() {
    for text in ["value = \"logg", "value = 'logg", "value = `logg"] {
        let mut active = snapshot(1, text, 0);
        active.cursor_col = text.chars().count();
        assert!(quiet_context(&active), "{text:?}");
    }
    for text in [
        "// logg",
        "value // logg",
        "# logg",
        "-- logg",
        "/* logg",
        "/* nested /* comment */\n * logg",
        "// quote \"logg",
        "logger.pr",
        "f(\"value\"); logg",
        "&'static value",
        "value /* comment */ logg",
        "fn f<'a, 'b>() {}\nlogger.pr",
        "fn f<'a: 'b + 'c>() {}\nlogger.pr",
        "'outer: loop {}\nlogger.pr",
    ] {
        let mut active = snapshot(1, text, text.lines().count() - 1);
        active.cursor_col = active.rope.line(active.cursor_line).len_chars();
        assert!(!quiet_context(&active), "{text:?}");
    }
}

#[test]
fn contextual_completion_is_cancelled_and_neighborhood_work_is_bounded() {
    let text =
        "unrelated ".repeat(1000) + "\n" + &"other_unrelated\n".repeat(40_000) + "needle_value\nne";
    let mut active = snapshot(1, &text, 40_002);
    active.cursor_col = 2;
    let mut cache = CompletionCache::default();
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
    assert_eq!(result.outcome, VtCompleteWorkOutcome::FuzzyWorkLimited);
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
            },
            prefix,
            expected,
        )
    })
    .collect();
    let options = PopupOptions::default();
    let mut cache = CompletionCache::default();
    let started = Instant::now();
    let cold = compute_contextual(
        &cases[0].0,
        cases[0].1,
        "Unknown",
        &options,
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
            compute_contextual(active, prefix, "Unknown", &options, &mut cache, || false).unwrap();
        assert!(
            result
                .suggestions
                .iter()
                .any(|word| word.strip_suffix("()").unwrap_or(word) == *expected)
        );
    }
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
                    &options,
                    &mut cache,
                    || false,
                )
                .unwrap(),
            );
        }
        let elapsed = started.elapsed();
        eprintln!(
            "contextual completion: warm_queries={count} total={elapsed:?} per_request={:?}",
            elapsed / count
        );
    }
}

#[test]
fn streaming_words_reuse_bounded_scratch_without_leaking_cancelled_tokens() {
    let mut scores = Scores::new(
        "",
        "",
        crate::fuzzy_score::FuzzyWorkBudget::new(scoring::work_limits()),
    );
    let mut scratch = String::with_capacity(MAX_COMPLETION_WORD_BYTES);
    let storage = scratch.as_ptr();
    for text in ["alpha beta", "界界 αβγ", "", "delta"] {
        let rope = ropey::Rope::from_str(text);
        scratch.push_str("cancelled_partial_token");
        assert_eq!(
            scan_words(rope.line(0), 0, &mut scores, &mut scratch, &|| false),
            Some(true)
        );
        assert_eq!(scratch.as_ptr(), storage);
        assert_eq!(scratch.capacity(), MAX_COMPLETION_WORD_BYTES);
    }
    assert_eq!(scores.matches.len(), 5);
    for word in ["alpha", "beta", "界界", "αβγ", "delta"] {
        assert!(scores.matches.contains_key(word));
    }
}

/// The original one-character-at-a-time scan, kept to check the ASCII path.
fn reference_scan_words(line: ropey::RopeSlice<'_>, scores: &mut Scores<'_>) {
    let mut word = String::new();
    let mut oversized = false;
    for chunk in line.chunks() {
        assert!(scores.charge_scan_bytes(chunk.len()));
        for ch in chunk.chars() {
            if identifier_char(ch) {
                if !oversized {
                    if word.len() + ch.len_utf8() <= MAX_COMPLETION_WORD_BYTES {
                        word.push(ch);
                    } else {
                        scores.defer_candidate();
                        word.clear();
                        oversized = true;
                    }
                }
            } else {
                if !word.is_empty() {
                    scores.add(&word, 3, false, &|| false).unwrap();
                    word.clear();
                }
                oversized = false;
            }
        }
    }
    if !word.is_empty() {
        scores.add(&word, 3, false, &|| false).unwrap();
    }
}

#[test]
fn ascii_word_scan_matches_character_scan_across_chunks_and_oversized_words() {
    let long = "x".repeat(MAX_COMPLETION_WORD_BYTES - 1);
    let texts = [
        "alpha beta_gamma $delta 42abc a-b",
        &format!("{long} {long}yz {long}y end"),
        &format!("pre {}界 tail", "q".repeat(3000)),
        &format!(
            "{} mixed αβγ{} ascii_after",
            "word ".repeat(900),
            "z".repeat(40)
        ),
        &"identifier_".repeat(1000),
        "trailing_",
        "",
    ];
    for text in texts {
        let rope = ropey::Rope::from_str(text);
        let budget = || crate::fuzzy_score::FuzzyWorkBudget::new(scoring::work_limits());
        let mut fast = Scores::new("", "", budget());
        let mut reference = Scores::new("", "", budget());
        assert_eq!(
            scan_words(rope.line(0), 3, &mut fast, &mut String::new(), &|| false),
            Some(true)
        );
        reference_scan_words(rope.line(0), &mut reference);
        let words = |scores: &Scores<'_>| {
            let mut words: Vec<_> = scores
                .matches
                .iter()
                .map(|(word, entry)| (word.clone(), entry.proximity))
                .collect();
            words.sort();
            words
        };
        assert_eq!(words(&fast), words(&reference), "{text:.40}");
        assert_eq!(fast.deferred_candidates, reference.deferred_candidates);
    }
}

fn snapshot(id: u64, text: &str, cursor_line: usize) -> VtCompleteSnapshot {
    VtCompleteSnapshot {
        source: Default::default(),
        document_id: id,
        rope: ropey::Rope::from_str(text),
        cursor_line,
        cursor_col: 0,
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
fn cancelled_work_stops_within_a_single_large_line_and_shared_chunk_walk() {
    use std::cell::Cell;
    let rope = ropey::Rope::from_str(&"print_line unrelated ".repeat(100_000));
    let checks = Cell::new(0);
    let mut scores = Scores::new(
        "zz",
        "zz",
        crate::fuzzy_score::FuzzyWorkBudget::new(scoring::work_limits()),
    );
    assert!(
        scan_words(rope.line(0), 0, &mut scores, &mut String::new(), &|| {
            checks.set(checks.get() + 1);
            checks.get() >= 3
        })
        .is_none()
    );
    assert_eq!(checks.get(), 3);
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
    let mut active = snapshot(1, "pri print_line print_value", 0);
    active.cursor_col = 3;
    VtCompleteRequest {
        active,
        edit_baseline: None,
        options: Default::default(),
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
    assert_eq!(queue.cache.as_ref().unwrap().document, Some(1));
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
}

#[test]
fn oversized_tokens_are_skipped_and_later_words_are_still_suggested() {
    let text = format!("pri print_{} print_short", "x".repeat(200_000));
    let mut current = snapshot(1, &text, 0);
    current.cursor_col = 3;
    let result = compute_contextual(
        &current,
        "pri",
        "Plain Text",
        &Default::default(),
        &mut CompletionCache::default(),
        || false,
    )
    .unwrap();
    assert_eq!(result.outcome, VtCompleteWorkOutcome::FuzzyWorkLimited);
    assert_eq!(result.suggestions, ["print_short"]);
    assert!(result.deferred_candidates > 0);
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

#[test]
fn word_index_matches_full_scan_through_edits_and_limits() {
    let result =
        |active: &VtCompleteSnapshot, prefix: &str, cache: &mut CompletionCache, full: bool| {
            FULL_SCAN.set(full);
            let result = compute_contextual_invoked(
                active,
                prefix,
                "Rust",
                &Default::default(),
                cache,
                true,
                || false,
            )
            .unwrap();
            FULL_SCAN.set(false);
            (
                result.suggestions,
                format!("{:?}", result.details),
                result.outcome,
                result.deferred_candidates,
            )
        };
    let long = "w".repeat(MAX_COMPLETION_WORD_BYTES + 3);
    let documents = [
        include_str!("vtcomplete_worker.rs").to_owned(),
        include_str!("ui_row.rs").to_owned(),
        format!("let {long} = café_naïve + 界面_value;\nfn go() {{ value_one(); }}\n"),
        // Beyond the indexed size: always the full scan.
        "let spare_value = other_value + 1;\n".repeat(10_000),
        "x\n".repeat(3),
    ];
    let mut checked = 0;
    INDEXED.set(0);
    for (document_index, text) in documents.iter().enumerate() {
        let mut rope = ropey::Rope::from_str(text);
        let mut cache = CompletionCache::default();
        let mut seed = 0x9e37_79b9_u32 ^ document_index as u32;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as usize
        };
        for step in 0..12 {
            // Edit between requests so the cached index follows changes.
            if step > 0 {
                let at = next() % (rope.len_chars() + 1);
                match next() % 4 {
                    0 => rope.insert(at, "va"),
                    1 => rope.insert(at, "\nnew_line_value("),
                    2 if rope.len_chars() > at + 5 => rope.remove(at..at + 5),
                    _ => rope.insert(at, " value_two "),
                }
            }
            let cursor_line = next() % rope.len_lines();
            let mut active = snapshot(document_index as u64, "", cursor_line);
            active.rope = rope.clone();
            let line = rope.line(cursor_line).to_string();
            let columns = line.trim_end_matches(&['\r', '\n'][..]).chars().count();
            active.cursor_col = next() % (columns + 1);
            for prefix in ["va", "s", "value_", "zz"] {
                let indexed = result(&active, prefix, &mut cache, false);
                let reference = result(&active, prefix, &mut CompletionCache::default(), true);
                assert_eq!(
                    indexed, reference,
                    "document {document_index} step {step} {prefix:?}"
                );
                checked += 1;
            }
        }
    }
    assert_eq!(checked, 5 * 12 * 4);
    // Every request on the four indexable documents used the index.
    assert_eq!(INDEXED.get(), 4 * 12 * 4);
}
