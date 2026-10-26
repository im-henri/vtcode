//! Black-box replays shared verbatim by the baseline and candidate executables.
//! Only test instrumentation reads the inventory or knows the expected answers.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::Path,
    time::{Duration, Instant},
};

fn spelling(word: &str) -> &str {
    word.strip_suffix("()").unwrap_or(word)
}

fn active_text(text: &str, path: &Path) -> VtCompleteSnapshot {
    let rope = ropey::Rope::from_str(text);
    let line = rope.len_lines() - 1;
    VtCompleteSnapshot {
        source: crate::completion_source::Source::new(Some(path), "Unknown"),
        cursor_col: rope.line(line).len_chars(),
        cursor_line: line,
        rope,
        document_id: 1,
        active: true,
    }
}

#[test]
fn active_file_relationship_beats_unrelated_generated_workspace_clones() {
    let root = tempfile::tempdir().unwrap();
    let generated = "bind(item, decode_cache_index);\nbind(item, decode_cache_state);\n".repeat(80);
    let (index, _) = crate::completion_index::CompletionIndex::build_for_benchmark(
        root.path().to_owned(),
        (0..64).map(|i| {
            (
                root.path().join(format!("copy{i}.custom")),
                generated.as_str(),
            )
        }),
    );
    assert_eq!(index.status().indexed_files, 64);
    let active = active_text(
        "let user_id = decode_user_id(payload);\nlet task_id = decode_task_id(payload);\nlet entry_id = decode_entry_id(payload);\nlet object_id = decode_object_id(payload);\nlet cache_id = decode",
        &root.path().join("active.unmapped"),
    );
    let result = compute_contextual_invoked(
        &active,
        "decode",
        "Unknown",
        &index,
        &mut TokenCache::default(),
        false,
        || false,
    )
    .unwrap();
    assert_eq!(
        result.suggestions.first().map(|word| spelling(word)),
        Some("decode_cache_id")
    );
}

#[test]
fn unrelated_candidate_work_cannot_starve_a_verified_current_buffer_counter() {
    for (noise_bytes, stem_bytes) in [(312, 6), (100, 140)] {
        let stem: String = format!("record_{}", "binding_".repeat(18))
            .chars()
            .take(stem_bytes)
            .collect();
        let prefix: String = stem.chars().take(24).collect();
        let noise = "abcdefghijklmnopqrstuvwxyz".repeat(50);
        let mut text = String::new();
        for line in 0..70 {
            for column in 0..16 {
                text.push_str(&format!("noise_{line}_{column}_{} ", &noise[..noise_bytes]));
            }
            text.push('\n');
        }
        for n in 1..=4 {
            text.push_str(&format!("let {stem}{n} = input{n};\n"));
        }
        text.push_str(&format!("let {prefix}"));
        let active = active_text(&text, Path::new("active"));
        let result = compute_contextual_invoked(
            &active,
            &prefix,
            "Unknown",
            &crate::completion_index::CompletionIndex::default(),
            &mut TokenCache::default(),
            false,
            || false,
        )
        .unwrap();
        assert_eq!(
            result.suggestions.first().map(|word| spelling(word)),
            Some(format!("{stem}5").as_str())
        );
        assert_eq!(result.outcome, VtCompleteWorkOutcome::FuzzyWorkLimited);
    }
}

#[test]
fn two_pairs_rank_known_names_but_novel_programs_need_an_independent_witness() {
    let pairs = "let user_id = decode_user_id(payload);\nlet task_id = decode_task_id(payload);\n";
    let index = crate::completion_index::CompletionIndex::default();
    let mut cache = TokenCache::default();
    let request = |text: &str, cache: &mut TokenCache| {
        compute_contextual_invoked(
            &active_text(text, Path::new("active.custom")),
            "decode",
            "Unknown",
            &index,
            cache,
            false,
            || false,
        )
        .unwrap()
    };
    let two = request(&format!("{pairs}let cache_id = decode"), &mut cache);
    assert!(
        !two.suggestions
            .iter()
            .any(|word| spelling(word) == "decode_cache_id")
    );
    let known = request(
        &format!("let existing = decode_cache_id;\n\n{pairs}let cache_id = decode"),
        &mut cache,
    );
    assert_eq!(
        known.suggestions.first().map(|word| spelling(word)),
        Some("decode_cache_id")
    );
    let three = request(
        &format!("{pairs}let entry_id = decode_entry_id(payload);\nlet cache_id = decode"),
        &mut cache,
    );
    assert_eq!(
        three.suggestions.first().map(|word| spelling(word)),
        Some("decode_cache_id")
    );
}

#[test]
#[ignore = "frozen transformation inventory; quality and real-worker latency"]
fn benchmark_transformations_replay() {
    let input =
        std::fs::read(std::env::var_os("VTCODE_TRANSFORM_INPUT").expect("frozen inventory"))
            .unwrap();
    let inventory: serde_json::Value = serde_json::from_slice(&input).unwrap();
    let split = std::env::var("VTCODE_TRANSFORM_SPLIT").unwrap_or_else(|_| "development".into());
    assert!(matches!(split.as_str(), "development" | "holdout"));
    let cases: Vec<_> = inventory["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|case| case["split"] == split)
        .collect();
    assert!(!cases.is_empty());
    let index = crate::completion_index::CompletionIndex::default();
    let background = crate::background::BackgroundWorkers::new();
    let worker = VtCompleteWorker::default();
    let mut rows = Vec::new();
    let mut cold_ns = Vec::new();
    let mut worker_ns = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        let visible = case["visible"].as_str().unwrap();
        let prefix = case["prefix"].as_str().unwrap();
        let expected = case["expected"].as_str();
        let caret = case["caret"]
            .as_u64()
            .map(|v| v as usize)
            .unwrap_or_else(|| visible.chars().count());
        let tokens: BTreeSet<_> = visible
            .split(|ch| !identifier_char(ch))
            .filter(|word| !word.is_empty())
            .collect();
        if case["novel"] == true {
            assert!(
                !tokens.contains(expected.unwrap()),
                "hidden answer in {}",
                case["id"]
            );
        }
        let source = case["source"].as_str().unwrap();
        let rope = ropey::Rope::from_str(visible);
        let line = rope.char_to_line(caret);
        assert_eq!(
            rope.slice(caret - prefix.chars().count()..caret)
                .to_string(),
            prefix
        );
        let active = VtCompleteSnapshot {
            source: crate::completion_source::Source::new(Some(Path::new(source)), "Unknown"),
            cursor_col: caret - rope.line_to_char(line),
            cursor_line: line,
            rope,
            document_id: i as u64 + 1,
            active: true,
        };
        learning_replay_tests::begin_trace(expected);
        let started = Instant::now();
        let result = compute_contextual_invoked(
            &active,
            prefix,
            "Unknown",
            &index,
            &mut TokenCache::default(),
            false,
            || false,
        )
        .unwrap();
        cold_ns.push(started.elapsed().as_nanos());
        let recalled = learning_replay_tests::candidate_recalled();
        let rank = expected.and_then(|answer| {
            result
                .suggestions
                .iter()
                .position(|word| spelling(word) == answer)
        });
        let unwanted: Vec<_> = result
            .suggestions
            .iter()
            .map(|word| spelling(word))
            .filter(|word| {
                !tokens.contains(word)
                    && Some(*word) != expected
                    && !case["allowed"]
                        .as_array()
                        .is_some_and(|allowed| allowed.iter().any(|value| value == word))
            })
            .map(str::to_owned)
            .collect();
        for repeat in 0..3 {
            let generation = (i * 3 + repeat + 1) as u64;
            let started = Instant::now();
            worker.submit(
                VtCompleteRequest {
                    active: active.clone(),
                    edit_baseline: None,
                    index: index.clone(),
                    explicit: false,
                    language: "Unknown".into(),
                    generation,
                    active_group: 0,
                    active_tab: 0,
                    content_version: 1,
                    cancel: CancellationToken::new(),
                    sender: background.sender.clone(),
                },
                background.interactive_sender(),
            );
            let BackgroundResult::VtComplete {
                generation: received,
                suggestions,
                ..
            } = background
                .receiver
                .recv_timeout(Duration::from_secs(10))
                .unwrap()
            else {
                panic!("wrong result")
            };
            assert_eq!(received, generation);
            if repeat > 0 {
                worker_ns.push(started.elapsed().as_nanos());
            }
            assert_eq!(
                suggestions, result.suggestions,
                "cold/queued difference at {}",
                case["id"]
            );
        }
        rows.push(serde_json::json!({
            "id":case["id"], "group":case["group"], "workload":case["workload"],
            "source":source, "visible":visible, "caret":caret, "prefix":prefix,
            "expected":expected, "positive":expected.is_some(),
            "novel":case["novel"], "rank":rank, "candidateRecall":recalled,
            "suggestions":result.suggestions, "unwantedNovel":unwanted,
            "quietViolation":case["quiet"] == true && !result.suggestions.is_empty(),
            "limited":matches!(result.outcome, VtCompleteWorkOutcome::FuzzyWorkLimited),
        }));
    }
    let mut groups = serde_json::Map::new();
    for row in &rows {
        let group = row["group"].as_str().unwrap();
        let counts = groups.entry(group.to_owned()).or_insert_with(|| {
            serde_json::json!({
                "cases":0,"positiveCases":0,"top1":0,"top5":0,"candidateRecall":0,
                "unwantedNovelCases":0,"quietViolations":0,"limited":0
            })
        });
        for (key, increment) in [
            ("cases", true),
            ("positiveCases", row["positive"] == true),
            ("top1", row["rank"] == 0),
            ("top5", row["rank"].as_u64().is_some_and(|v| v < 5)),
            (
                "candidateRecall",
                row["positive"] == true && row["candidateRecall"] == true,
            ),
            (
                "unwantedNovelCases",
                !row["unwantedNovel"].as_array().unwrap().is_empty(),
            ),
            ("quietViolations", row["quietViolation"] == true),
            ("limited", row["limited"] == true),
        ] {
            let value = counts[key].as_u64().unwrap() + u64::from(increment);
            counts[key] = value.into();
        }
    }
    let percentile = |values: &mut Vec<u128>, p: usize| {
        values.sort_unstable();
        values[(values.len() - 1) * p / 100] as f64 / 1000.
    };
    let report = serde_json::json!({
        "split":split,"inventorySha256":format!("{:x}",Sha256::digest(&input)),
        "fixtureSha256":format!("{:x}",Sha256::digest(serde_json::to_vec(&cases).unwrap())),
        "mode":std::env::var("VTCODE_GENERATION_MODE").unwrap_or_else(|_|"full".into()),
        "coldP95Us":percentile(&mut cold_ns,95),"workerWarmP95Us":percentile(&mut worker_ns,95),
        "completionEvidenceBytes":std::mem::size_of::<CompletionEvidence>(),
        "groups":groups,"rows":rows,
    });
    std::fs::write(
        std::env::var_os("VTCODE_TRANSFORM_REPORT").expect("report path"),
        serde_json::to_vec_pretty(&report).unwrap(),
    )
    .unwrap();
    eprintln!(
        "{}",
        serde_json::json!({"split":split,"cases":cases.len(),"groups":report["groups"],
        "coldP95Us":report["coldP95Us"],"workerWarmP95Us":report["workerWarmP95Us"]})
    );
}
