//! Frozen, chronological native-code probes. Only earlier active-file bytes
//! and other opened files enter the predictor, never the target's hidden tail.
use super::*;
use std::{collections::BTreeMap, path::PathBuf, time::{Duration, Instant}};

const FILES: &[&str] = &[
    "animation_config.rs", "animation_inline_scope_tests.rs",
    "animation_language_scope_tests.rs", "animation_multicursor_tests.rs",
    "animation_pacing.rs", "animation_preprocessor_scope_tests.rs",
    "animation_scope.rs", "animation_scope_blame_tests.rs",
    "animation_scope_checkpoints.rs", "animation_scope_edit_tests.rs",
    "animation_scope_edits.rs", "animation_scope_indentation.rs",
    "animation_scope_preprocessor.rs",
];

fn percentile(samples: &mut [u128], percent: usize) -> f64 {
    samples.sort_unstable();
    samples[(samples.len() - 1) * percent / 100] as f64 / 1000.
}

#[test]
#[ignore = "native vtcode editing replay, quality and real interactive-worker latency"]
fn benchmark_vtcode_editing_replay() {
    use sha2::{Digest, Sha256};
    let source = std::env::var_os("VTCODE_CAPACITY_SOURCE").map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"));
    let files: Vec<_> = FILES.iter().map(|&name| {
        (name, std::fs::read_to_string(source.join(name)).unwrap())
    }).collect();
    let workspace = std::env::var_os("VTCODE_VTCODE_REPLAY_WORKSPACE").is_some();
    let mut workspace_paths: Vec<_> = if workspace {
        std::fs::read_dir(&source).unwrap().map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "rs")).collect()
    } else { Vec::new() };
    workspace_paths.sort();
    let workspace_files: Vec<_> = workspace_paths.iter().map(|path|
        (path.file_name().unwrap().to_string_lossy().into_owned(),std::fs::read_to_string(path).unwrap())).collect();
    let indexed: Vec<_> = if workspace {
        workspace_files.iter().map(|(name,text)| (name.as_str(),text.as_str())).collect()
    } else { files.iter().map(|(name,text)| (*name,text.as_str())).collect() };
    let mut digest = Sha256::new();
    for (name, text) in &files {
        digest.update(name.as_bytes()); digest.update([0]); digest.update(text.as_bytes());
    }
    let directory = tempfile::tempdir().unwrap();
    let background = crate::background::BackgroundWorkers::new();
    let queue = VtCompleteWorker::default();
    let mut direct_us = Vec::new();
    let mut queued_us = Vec::new();
    let mut cold_us = Vec::new();
    let mut rows = Vec::new();
    let (mut top1, mut top5, mut recall, mut unwanted, mut negatives, mut limited) = (0,0,0,0,0,0);
    for (document, (name, text)) in files.iter().enumerate() {
        let root = directory.path().join(format!("session{document}"));
        let (index, _records) = crate::completion_index::CompletionIndex::build_for_benchmark(
            root.clone(), indexed.iter().filter(|(other, _)| other != name)
                .map(|(name, text)| (root.join(name), *text)),
        );
        if workspace { assert_eq!(index.status().indexed_files, indexed.len()-1); }
        let mut seen = BTreeMap::<&str, usize>::new();
        let mut probes = Vec::new();
        let mut offset = 0;
        for word in text.split_inclusive(|ch: char| !identifier_char(ch)) {
            let token = word.trim_end_matches(|ch: char| !identifier_char(ch));
            if token.len() >= 8 && token.is_ascii() && seen.get(token).copied().unwrap_or(0) >= 2 {
                let prefix = &token[..3];
                if seen.keys().filter(|word| word.starts_with(prefix)).count() >= 2 {
                    probes.push((offset, token));
                }
            }
            if !token.is_empty() { *seen.entry(token).or_default() += 1; }
            offset += word.len();
        }
        // Evenly spaced, outcome-independent selection over every source file.
        let count = probes.len().min(40);
        let mut cache = TokenCache::default();
        for number in 0..count {
            let (offset, expected) = probes[number * probes.len() / count];
            let prefix = &expected[..3];
            let visible = &text[..offset + prefix.len()];
            let rope = ropey::Rope::from_str(visible);
            let line = rope.len_lines() - 1;
            let active = VtCompleteSnapshot {
                source: crate::completion_source::Source::new(Some(&root.join(name)), "Rust"),
                cursor_line: line, cursor_col: rope.line(line).len_chars(), rope,
                document_id: document as u64 + 1, active: true,
            };
            learning_replay_tests::begin_trace(Some(expected));
            let started = Instant::now();
            let result = compute_contextual_invoked(&active, prefix, "Rust", &index, &mut cache, false, || false).unwrap();
            let elapsed = started.elapsed().as_nanos();
            if number == 0 { cold_us.push(elapsed); } else { direct_us.push(elapsed); }
            let found = learning_replay_tests::candidate_recalled();
            let rank = result.suggestions.iter().position(|word| word.trim_end_matches("()") == expected);
            top1 += usize::from(rank == Some(0));
            top5 += usize::from(rank.is_some_and(|rank| rank < 5));
            recall += usize::from(found);
            limited += usize::from(result.outcome != VtCompleteWorkOutcome::Exact);
            rows.push(serde_json::json!({"file":name,"offset":offset,"expected":expected,"rank":rank,"recalled":found}));
            let generation = rows.len() as u64;
            let started = Instant::now();
            queue.submit(VtCompleteRequest {
                edit_baseline: None,
                active: active.clone(), index: index.clone(), explicit: false,
                language: "Rust".into(), generation, active_group: 0, active_tab: 0,
                content_version: generation, cancel: CancellationToken::new(), sender: background.sender.clone(),
            }, background.interactive_sender());
            let result = background.receiver.recv_timeout(Duration::from_secs(5)).unwrap();
            let elapsed = started.elapsed().as_nanos();
            assert!(matches!(result, BackgroundResult::VtComplete {generation: received, ..} if received == generation));
            if number != 0 { queued_us.push(elapsed); }
            if number % 10 == 0 {
                let mut negative = active;
                negative.rope.insert(negative.rope.len_chars(), "zzzznonexistent");
                negative.cursor_col += "zzzznonexistent".len();
                let absent = format!("{prefix}zzzznonexistent");
                let result = compute_contextual_invoked(&negative, &absent, "Rust", &index, &mut cache, false, || false).unwrap();
                negatives += 1;
                unwanted += usize::from(!result.suggestions.is_empty());
            }
        }
    }
    let report = serde_json::json!({
        "corpusSha256":format!("{:x}",digest.finalize()), "cases":rows.len(), "top1":top1,
        "indexScope":if workspace {"full native source"} else {"reported opened files"},
        "seedFilesPerSession":indexed.len()-1,
        "top5":top5,"candidateRecall":recall,"negativeCases":negatives,"unwanted":unwanted,
        "limited":limited,"directWarmP50Us":percentile(&mut direct_us,50),
        "directWarmP95Us":percentile(&mut direct_us,95),"workerWarmP50Us":percentile(&mut queued_us,50),
        "workerWarmP95Us":percentile(&mut queued_us,95),"coldP95Us":percentile(&mut cold_us,95),"rows":rows,
    });
    eprintln!("vtcode replay {}", serde_json::to_string(&report).unwrap());
    if let Some(path) = std::env::var_os("VTCODE_VTCODE_REPLAY_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    assert!(report["cases"].as_u64().unwrap() >= 100);
}
