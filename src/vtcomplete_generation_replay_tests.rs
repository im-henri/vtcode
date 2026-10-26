//! Controlled new-name editing replays. Answers are never indexed or in the
//! visible buffer; native ordinary ranking has its own unchanged replay.
use super::*;
use std::{
    collections::BTreeSet,
    path::PathBuf,
    time::{Duration, Instant},
};

#[test]
fn novel_predictions_work_without_an_index_and_do_not_depend_on_a_language_or_extension() {
    let text = "my_var1 = a\nmy_var2 = b\nmy";
    for (path, language) in [
        ("source.rs", "Rust"),
        ("source.customdialect", "Unknown"),
        ("source", "Unregistered language"),
    ] {
        let rope = ropey::Rope::from_str(text);
        let active = VtCompleteSnapshot {
            source: crate::completion_source::Source::new(
                Some(std::path::Path::new(path)),
                language,
            ),
            cursor_line: 2,
            cursor_col: 2,
            rope,
            document_id: 1,
            active: true,
        };
        let result = compute_contextual_invoked(
            &active,
            "my",
            language,
            &crate::completion_index::CompletionIndex::default(),
            &mut TokenCache::default(),
            false,
            || false,
        )
        .unwrap();
        assert_eq!(
            result.suggestions.first().map(String::as_str),
            Some("my_var3")
        );
        assert_eq!(result.details[0].annotation, "sequence");
    }
}

#[test]
fn generation_checks_the_visible_suffix_and_keeps_snippet_and_manual_completion_behavior() {
    let text = "slot1 = source1\nslot2 = source2\nsl = source9";
    let active = VtCompleteSnapshot {
        source: Default::default(),
        rope: ropey::Rope::from_str(text),
        document_id: 1,
        cursor_line: 2,
        cursor_col: 2,
        active: true,
    };
    let index = crate::completion_index::CompletionIndex::default();
    let result = compute_contextual_invoked(
        &active,
        "sl",
        "Unknown",
        &index,
        &mut TokenCache::default(),
        false,
        || false,
    )
    .unwrap();
    assert_eq!(
        result.suggestions.first().map(String::as_str),
        Some("slot9")
    );
    assert!(!result.suggestions.iter().any(|word| word == "slot3"));
    let mut active = active;
    active.rope = ropey::Rope::from_str("fn_item1 = a\nfn_item2 = b\nfn");
    active.cursor_col = 2;
    let result = compute_contextual_invoked(
        &active,
        "fn",
        "Rust",
        &index,
        &mut TokenCache::default(),
        false,
        || false,
    )
    .unwrap();
    assert_eq!(result.suggestions[0], "fn");
    assert_eq!(
        result.details[0].kind,
        crate::app_vtcomplete::CompletionKind::Snippet
    );
    active.rope = ropey::Rope::from_str("slot1 = a\nslot2 = b\ns");
    active.cursor_col = 1;
    for explicit in [false, true] {
        let early = compute_contextual_invoked(
            &active, "s", "Unknown", &index, &mut TokenCache::default(), explicit, || false,
        ).unwrap();
        assert_eq!(early.suggestions.first().map(String::as_str), Some("slot3"));
        assert_eq!(early.details[0].annotation, "sequence");
    }
}

#[test]
fn two_nonunit_identifier_items_predict_the_third_and_respect_prefix_settings() {
    let mut active = active_text_for_early_sequence("slot10 = a\nslot20 = b\ns");
    let index = crate::completion_index::CompletionIndex::default();
    let result = compute_contextual_invoked(&active, "s", "Unknown", &index,
        &mut TokenCache::default(), false, || false).unwrap();
    assert_eq!(result.suggestions.first().map(String::as_str), Some("slot30"));
    active.rope = ropey::Rope::from_str("slot10 = a\nslot20 = b\nslot35 = c\ns");
    active.cursor_line = 3;
    let result = compute_contextual_invoked(&active, "s", "Unknown", &index,
        &mut TokenCache::default(), false, || false).unwrap();
    assert!(!result.suggestions.iter().any(|word| word == "slot50"));
    let mut editor = crate::editor::Editor::test_with_text("s");
    editor.cursor_col = 1;
    let mut settings = index.settings();
    settings.min_prefix_chars = 2;
    settings.context_triggers = false;
    assert!(!automatic_trigger(&editor, &settings));
}

fn active_text_for_early_sequence(text: &str) -> VtCompleteSnapshot {
    let rope = ropey::Rope::from_str(text);
    let cursor_line = rope.len_lines() - 1;
    VtCompleteSnapshot {
        source: Default::default(), cursor_line, cursor_col: rope.line(cursor_line).len_chars(),
        rope, document_id: 1, active: true,
    }
}

struct Probe {
    group: &'static str,
    visible: String,
    prefix: String,
    expected: Option<String>,
    stem: Option<String>,
    caret: Option<(usize, usize)>,
}

fn positive(group: &'static str, before: String, prefix: String, expected: String) -> Probe {
    Probe {
        group,
        visible: format!("{before}{prefix}"),
        prefix,
        expected: Some(expected),
        stem: None,
        caret: None,
    }
}
fn negative(group: &'static str, before: String, prefix: String, stem: Option<String>) -> Probe {
    Probe {
        group,
        visible: format!("{before}{prefix}"),
        prefix,
        expected: None,
        stem,
        caret: None,
    }
}
fn probes(roots: &[String]) -> Vec<Probe> {
    let mut result = Vec::new();
    for (i, root) in roots.iter().enumerate() {
        let prefix: String = root.chars().take(2).collect();
        let start = 3 + (i % 17) as u64;
        result.push(positive(
            "increment",
            format!("{root}{start} = a\n{root}{} = b\n", start + 1),
            prefix.clone(),
            format!("{root}{}", start + 2),
        ));
        result.push(positive(
            "decrement",
            format!("{root}{} = a\n{root}{} = b\n", start + 2, start + 1),
            prefix.clone(),
            format!("{root}{start}"),
        ));
        result.push(positive(
            "stride",
            format!(
                "{root}{start} = a\n{root}{} = b\n{root}{} = c\n",
                start + 3,
                start + 6
            ),
            prefix.clone(),
            format!("{root}{}", start + 9),
        ));
        result.push(positive(
            "padding",
            format!("{root}_007 = a\n{root}_008 = b\n"),
            prefix.clone(),
            format!("{root}_009"),
        ));
        result.push(positive(
            "carry",
            format!("{root}8 = a\n{root}9 = b\n"),
            prefix.clone(),
            format!("{root}10"),
        ));
        result.push(positive(
            "multiple-fields",
            format!("{root}_1_2 = a\n{root}_2_3 = b\n"),
            prefix.clone(),
            format!("{root}_3_4"),
        ));
        result.push(positive(
            "alternating",
            format!("take_{root}1()\nreturn_{root}1()\ntake_{root}2()\n"),
            "ret".into(),
            format!("return_{root}2"),
        ));
        result.push(positive(
            "period-two",
            format!("{root}1 = a\nother1 = a\n{root}2 = b\nother2 = b\n"),
            prefix.clone(),
            format!("{root}3"),
        ));
        result.push(positive(
            "prefix-transform",
            format!("load_{root}(one)\nstore_{root}(one)\nload_next_{root}(two)\n"),
            "sto".into(),
            format!("store_next_{root}"),
        ));
        result.push(positive(
            "suffix-transform",
            format!("{root}_input = a\n{root}_output = a\nnext_{root}_input = b\n"),
            format!("next_{root}_o"),
            format!("next_{root}_output"),
        ));
        result.push(positive("affix-from-pairs", format!("{root}_item = a\n{root}_items = a\n{root}_entry = b\n{root}_entrys = b\n{root}_object = c\n"), prefix.clone(), format!("{root}_objects")));
        result.push(positive(
            "unicode-number",
            format!("{root}_界1 = a\n{root}_界2 = b\n"),
            prefix.clone(),
            format!("{root}_界3"),
        ));
        result.push(negative(
            "irregular",
            format!("{root}1 = a\n{root}4 = b\n{root}9 = c\n"),
            prefix.clone(),
            Some(root.clone()),
        ));
        result.push(negative(
            "single",
            format!("{root}1 = a\n"),
            prefix.clone(),
            None,
        ));
        // Two values now deliberately establish the simple arithmetic rule.
        result.push(positive(
            "two-item-stride",
            format!("{root}1 = a\n{root}8 = b\n"),
            prefix.clone(),
            format!("{root}15"),
        ));
        result.push(negative(
            "scope-boundary",
            format!("{{\n{root}1 = a\n{root}2 = b\n}}\n"),
            prefix.clone(),
            None,
        ));
        result.push(negative(
            "indent-boundary",
            format!("    {root}1 = a\n    {root}2 = b\n"),
            prefix.clone(),
            None,
        ));
        result.push(negative(
            "literal-context-conflict",
            format!("{root}1 = fixed\n{root}2 = fixed\n"),
            prefix.clone(),
            None,
        ));
        let last = result.last_mut().unwrap();
        last.visible.push_str(" = incompatible");
        last.caret = Some((2, prefix.chars().count()));
        result.push(negative(
            "renaming-conflict",
            format!("open_{root}(a, a)\nclose_{root}(a, a)\nopen_next_{root}(b, c)\n"),
            "clo".into(),
            None,
        ));
        result.push(negative(
            "duplicate-evidence",
            format!("{root}1 = a\n{} ", format!("{root}2 = b\n").repeat(30))
                .trim_end_matches(' ')
                .to_string(),
            prefix.clone(),
            Some(root.clone()),
        ));
        result.push(negative(
            "comment",
            format!("/*\n{root}1 = a\n{root}2 = b\n"),
            prefix.clone(),
            None,
        ));
        result.push(negative(
            "string",
            format!("text = `\n{root}1 = a\n{root}2 = b\n"),
            prefix.clone(),
            None,
        ));
    }
    result
}

fn roots(split: &str) -> Vec<String> {
    let root = PathBuf::from(std::env::var_os("VTCODE_GENERATION_CORPUS").expect("frozen corpus"));
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(root.join("manifest.json")).unwrap()).unwrap();
    let mut words = BTreeSet::new();
    use sha2::{Digest, Sha256};
    for file in manifest["files"].as_array().unwrap() {
        let text = std::fs::read_to_string(root.join(file["path"].as_str().unwrap())).unwrap();
        assert_eq!(
            format!("{:x}", Sha256::digest(text.as_bytes())),
            file["sha256"].as_str().unwrap()
        );
        words.extend(
            text.split(|ch| !identifier_char(ch))
                .filter(|word| {
                    (6..=18).contains(&word.len())
                        && word.is_ascii()
                        && word.bytes().all(|b| b.is_ascii_alphabetic() || b == b'_')
                        && word.as_bytes()[0].is_ascii_lowercase()
                })
                .map(str::to_owned),
        );
    }
    let seed = if split == "development" {
        b"development-v1".as_slice()
    } else {
        b"holdout-v1".as_slice()
    };
    let mut selected: Vec<_> = words
        .into_iter()
        .filter_map(|word| {
            let digest = Sha256::digest(word.as_bytes());
            let development = digest[0] % 2 == 0;
            if development != (split == "development") {
                return None;
            }
            let mut hash = Sha256::new();
            hash.update(seed);
            hash.update(word.as_bytes());
            Some((hash.finalize().to_vec(), word))
        })
        .collect();
    selected.sort_unstable();
    selected
        .into_iter()
        .take(16)
        .map(|(_, word)| word)
        .collect()
}

#[test]
#[ignore = "controlled novel-name quality and real-worker latency; frozen corpus required"]
fn benchmark_generated_identifier_replay() {
    let split = std::env::var("VTCODE_GENERATION_SPLIT").unwrap_or_else(|_| "development".into());
    assert!(matches!(split.as_str(), "development" | "holdout"));
    let roots = roots(&split);
    let probes = probes(&roots);
    let index = crate::completion_index::CompletionIndex::default();
    let background = crate::background::BackgroundWorkers::new();
    let worker = VtCompleteWorker::default();
    let mut rows = Vec::new();
    let mut worker_ns = Vec::new();
    let mut cold_ns = Vec::new();
    for (i, probe) in probes.iter().enumerate() {
        let tokens: BTreeSet<_> = probe
            .visible
            .split(|ch| !identifier_char(ch))
            .filter(|word| !word.is_empty())
            .collect();
        if let Some(expected) = &probe.expected {
            assert!(
                !tokens.contains(expected.as_str()),
                "hidden answer in input"
            );
        }
        let rope = ropey::Rope::from_str(&probe.visible);
        let (line, col) = probe.caret.unwrap_or_else(|| {
            let line = rope.len_lines() - 1;
            (line, rope.line(line).len_chars())
        });
        let active = VtCompleteSnapshot {
            source: crate::completion_source::Source::new(None, "Unknown"),
            rope,
            document_id: i as u64 + 1,
            cursor_line: line,
            cursor_col: col,
            active: true,
        };
        learning_replay_tests::begin_trace(probe.expected.as_deref());
        let started = Instant::now();
        let result = compute_contextual_invoked(
            &active,
            &probe.prefix,
            "Unknown",
            &index,
            &mut TokenCache::default(),
            false,
            || false,
        )
        .unwrap();
        cold_ns.push(started.elapsed().as_nanos());
        let recalled = learning_replay_tests::candidate_recalled();
        let rank = probe
            .expected
            .as_ref()
            .and_then(|expected| result.suggestions.iter().position(|word| word == expected));
        let stem_rank = probe
            .stem
            .as_ref()
            .and_then(|stem| result.suggestions.iter().position(|word| word == stem));
        let invented: Vec<_> = result
            .suggestions
            .iter()
            .filter(|word| {
                !tokens.contains(word.as_str())
                    && probe.expected.as_ref() != Some(word)
                    && probe.stem.as_ref() != Some(word)
            })
            .cloned()
            .collect();
        let mut last = Vec::new();
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
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
            else {
                panic!("wrong result")
            };
            assert_eq!(received, generation);
            if repeat > 0 {
                worker_ns.push(started.elapsed().as_nanos());
            }
            last = suggestions;
        }
        assert_eq!(result.suggestions, last, "direct and queued outputs differ");
        rows.push(serde_json::json!({"group":probe.group,"root":roots[i/22],"positive":probe.expected.is_some(),"visible":probe.visible,"prefix":probe.prefix,"expected":probe.expected,"stem":probe.stem,"rank":rank,"stemRank":stem_rank,"candidateRecall":recalled,"suggestions":result.suggestions,"unwantedNovel":invented}));
    }
    let percentile = |values: &mut Vec<u128>, p: usize| {
        values.sort_unstable();
        values[(values.len() - 1) * p / 100] as f64 / 1000.
    };
    let positives: Vec<_> = rows.iter().filter(|row| row["positive"] == true).collect();
    let negatives: Vec<_> = rows.iter().filter(|row| row["positive"] == false).collect();
    let report = serde_json::json!({"split":split,"mode":std::env::var("VTCODE_GENERATION_MODE").unwrap_or_else(|_|"full".into()),"roots":roots,"positiveCases":positives.len(),"negativeCases":negatives.len(),"top1":positives.iter().filter(|row|row["rank"]==0).count(),"top5":positives.iter().filter(|row|row["rank"].as_u64().is_some_and(|rank|rank<5)).count(),"candidateRecall":positives.iter().filter(|row|row["candidateRecall"]==true).count(),"negativeUnwantedCases":negatives.iter().filter(|row|!row["unwantedNovel"].as_array().unwrap().is_empty()).count(),"coldP95Us":percentile(&mut cold_ns,95),"workerWarmP95Us":percentile(&mut worker_ns,95),"completionEvidenceBytes":std::mem::size_of::<CompletionEvidence>(),"rows":rows});
    if let Some(path) = std::env::var_os("VTCODE_GENERATION_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    eprintln!(
        "{}",
        serde_json::json!({"split":report["split"],"positiveCases":report["positiveCases"],"negativeCases":report["negativeCases"],"top1":report["top1"],"top5":report["top5"],"candidateRecall":report["candidateRecall"],"negativeUnwantedCases":report["negativeUnwantedCases"],"coldP95Us":report["coldP95Us"],"workerWarmP95Us":report["workerWarmP95Us"]})
    );
}
