//! Opt-in measurements against real source, independently of filesystem speed
//! and the scheduler's intentional cooldown. Uses the production extractor and
//! admission path; query timings do not include file access.
//! The fixed before/after workload uses top-level src/*.rs. For the complete
//! recursive source inventory use scripts/profile_completion_heap.ps1 with
//! -RecursiveCorpus; it preserves nested file identities instead of basenames.
use super::*;

const REPORTED: &[&str] = &[
    "animation_config.rs",
    "animation_inline_scope_tests.rs",
    "animation_language_scope_tests.rs",
    "animation_multicursor_tests.rs",
    "animation_pacing.rs",
    "animation_preprocessor_scope_tests.rs",
    "animation_scope.rs",
    "animation_scope_blame_tests.rs",
    "animation_scope_checkpoints.rs",
    "animation_scope_edit_tests.rs",
    "animation_scope_edits.rs",
    "animation_scope_indentation.rs",
    "animation_scope_preprocessor.rs",
];

#[test]
#[ignore = "real vtcode corpus capacity and in-memory query measurements"]
fn benchmark_vtcode_index_capacity() {
    use sha2::{Digest, Sha256};
    let source = std::env::var_os("VTCODE_CAPACITY_SOURCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"));
    let mut all: Vec<_> = std::fs::read_dir(&source)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .collect();
    all.sort();
    let scenarios = [
        (
            "reported-13",
            REPORTED.iter().map(|name| source.join(name)).collect(),
        ),
        ("vtcode-src", all),
    ];
    let mut rows = Vec::new();
    for (name, paths) in scenarios {
        let (_temporary, root, cache_dir) = fixture();
        let shared = Arc::new(Shared {
            root: root.clone(),
            cache_dir,
            buffer_only: false,
            control: Mutex::new(Control::default()),
            available: Condvar::new(),
            catalog: RwLock::new(Catalog::default()),
            known: RwLock::new(BTreeSet::new()),
            status: Mutex::new(IndexStatus::default()),
            wake: Mutex::new(crate::background::UiWake::default()),
            revision: AtomicU64::new(0),
            active_file: AtomicU64::new(0),
        });
        let index = CompletionIndex {
            handle: Some(Arc::new(Handle(Arc::clone(&shared)))),
            ..Default::default()
        };
        let mut worker = Worker::new(Arc::clone(&shared));
        let settings = IndexSettings::default();
        let mut bytes = 0;
        let mut digest = Sha256::new();
        let started = Instant::now();
        for path in &paths {
            let text = std::fs::read_to_string(path).unwrap();
            digest.update(path.file_name().unwrap().to_string_lossy().as_bytes());
            digest.update([0]);
            digest.update(text.as_bytes());
            bytes += text.len();
            let mut parser = stream::Extractor::default();
            parser.feed(&text);
            let (terms, examples) = parser.finish_with_examples();
            worker.install(
                FileRecord {
                    path: root.join(path.file_name().unwrap()),
                    stamp: None,
                    hash: hash(text.as_bytes()),
                    terms: terms.into(),
                    examples: Arc::new(examples),

                    dirty: true,
                    memory_bytes: 0,
                    reducible_bytes: 0,
                },
                &settings,
            );
        }
        let build_ms = started.elapsed().as_secs_f64() * 1000.;
        let catalog = shared.catalog.read().unwrap();
        let retained = worker.memory + catalog.memory_bytes;
        let examples = catalog.examples.retained_memory();
        let vocabulary = catalog.terms.len();
        let ledger = catalog.ledger(worker.memory);
        drop(catalog);
        let mut query_us = Vec::new();
        for _ in 0..50 {
            for (prefix, context) in [
                ("animation_", ""),
                ("sel", "self."),
                ("sc", "scope."),
                ("cP", ""),
            ] {
                let started = Instant::now();
                std::hint::black_box(index.query(prefix, context));
                query_us.push(started.elapsed().as_nanos() as f64 / 1000.);
            }
        }
        query_us.sort_by(f64::total_cmp);
        rows.push(serde_json::json!({
            "workload": name, "inputFiles": paths.len(), "sourceBytes": bytes,
            "corpusSha256": format!("{:x}", digest.finalize()),
            "indexedFiles": worker.files.len(), "skippedFiles": worker.skipped,
            "limited": worker.limited, "retainedEstimatedBytes": retained,
            "exampleEstimatedBytes": examples, "vocabulary": vocabulary,
            "ledger": ledger,
            "buildMs": build_ms, "queryP50Us": query_us[query_us.len()/2],
            "queryP95Us": query_us[query_us.len()*95/100],
        }));
        let paths: Vec<_> = worker.files.keys().cloned().collect();
        for path in paths {
            worker.remove(&path);
        }
        assert_eq!(worker.memory, 0);
        assert_eq!(
            shared.catalog.read().unwrap().memory_bytes,
            0,
            "capacity accounting must balance after complete removal"
        );
    }
    let report = serde_json::json!({"budgetMiB":48,"scenarios":rows});
    eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
    if let Some(path) = std::env::var_os("VTCODE_CAPACITY_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}

#[test]
#[ignore = "10,000 real 2 KiB source windows with full production learning enabled"]
fn benchmark_ten_thousand_real_code_windows() {
    use sha2::{Digest, Sha256};
    let source = std::env::var_os("VTCODE_CAPACITY_SOURCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"));
    let mut paths: Vec<_> = std::fs::read_dir(source)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .collect();
    paths.sort();
    let corpus: Vec<_> = paths
        .iter()
        .map(|path| std::fs::read_to_string(path).unwrap())
        .collect();
    let (_temporary, root, _) = fixture();
    let mut digest = Sha256::new();
    let mut source_bytes = 0;
    let mut inputs = Vec::new();
    for file in 0..10_000 {
        let text = &corpus[file % corpus.len()];
        let mut start = (file * 7919) % text.len().max(1);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        let mut end = (start + 2048).min(text.len());
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        let window = &text[start..end];
        digest.update(window.as_bytes());
        digest.update([0]);
        source_bytes += window.len();
        inputs.push((root.join(format!("program{file}.custom")), window));
    }
    let started = Instant::now();
    let (index, _records) = CompletionIndex::build_for_benchmark(root, inputs);
    let status = index.status();
    let report = serde_json::json!({
        "inputFiles":10000,"sourceBytes":source_bytes,"corpusSha256":format!("{:x}",digest.finalize()),
        "indexedFiles":status.indexed_files,"skippedFiles":status.skipped_files,
        "budgetMiB":status.settings.max_memory_mb,"retainedEstimatedBytes":status.memory_bytes,
        "exampleEstimatedBytes":index.example_memory(),"buildMs":started.elapsed().as_secs_f64()*1000.,
        "description":"Deterministic 2 KiB windows from real vtcode sources, separate custom-extension file identities; full production extractor, examples and admission; not 10,000 complete programs."
    });
    eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
    if let Some(path) = std::env::var_os("VTCODE_TEN_K_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    assert!(status.memory_bytes <= retained_limit(&status.settings));
}
