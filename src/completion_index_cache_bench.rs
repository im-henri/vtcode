//! Opt-in real-source pressure replay. UI-facing timings include the production
//! queue and concurrent catalog access, not terminal rendering or frame times.
use super::*;

fn percentile(samples: &mut [f64], percent: usize) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[(samples.len().saturating_sub(1) * percent) / 100]
}

fn window(text: &str, n: usize) -> &str {
    let mut start = (n * 7919) % text.len().max(1);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    let mut end = (start + 8192).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[start..end]
}

#[test]
#[ignore = "20,000 real-source admissions at the default RAM cap plus concurrent worker churn"]
fn benchmark_real_repository_cache_churn() {
    use sha2::{Digest, Sha256};
    let source = std::env::var_os("VTCODE_CAPACITY_SOURCE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src"));
    let mut paths: Vec<_> = ignore::WalkBuilder::new(&source)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(ignore::DirEntry::into_path)
        .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
        .collect();
    paths.sort();
    assert!(
        paths.len() >= 100,
        "use the actual repository source inventory"
    );
    let mut digest = Sha256::new();
    let corpus: Vec<_> = paths
        .iter()
        .map(|path| {
            let text = std::fs::read_to_string(path).unwrap();
            digest.update(
                path.strip_prefix(&source)
                    .unwrap()
                    .to_string_lossy()
                    .as_bytes(),
            );
            digest.update([0]);
            digest.update(text.as_bytes());
            text
        })
        .collect();
    let corpus_sha = format!("{:x}", digest.finalize());
    let (_temporary, root, cache) = fixture();
    let (index, mut worker) = synchronous(&root, &cache);
    let settings = IndexSettings::default();
    let started = Instant::now();
    let mut source_bytes = 0;
    let mut admission_us = Vec::new();
    for n in 0..20_000 {
        let text = window(&corpus[n % corpus.len()], n);
        source_bytes += text.len();
        let path = root.join(format!("program{n}.custom"));
        let parsed = record(path.clone(), text);
        let started = Instant::now();
        admit(&mut worker, parsed, &settings);
        admission_us.push(started.elapsed().as_secs_f64() * 1e6);
        assert!(worker.files.contains_key(&path));
    }
    let build_ms = started.elapsed().as_secs_f64() * 1000.;
    let memory = worker.memory
        + worker.recency.memory()
        + worker.shared.catalog.read().unwrap().memory_bytes;
    assert!(memory <= retained_limit(&settings));
    assert!(
        worker.evicted > 0,
        "replay must reach the default RAM ceiling: {memory} bytes, {} files",
        worker.files.len()
    );
    assert_eq!(
        worker.skipped, 0,
        "demand admissions must replace cache entries rather than reject files"
    );
    let default_cap = serde_json::json!({
        "inputFiles":20000,"sourceBytes":source_bytes,"budgetMiB":settings.max_memory_mb,
        "residentFiles":worker.files.len(),"evictedFiles":worker.evicted,"skippedFiles":worker.skipped,
        "retainedEstimatedBytes":memory,"replayMs":build_ms,
        "workerAdmissionP95Us":percentile(&mut admission_us,95),
        "workerAdmissionMaxUs":percentile(&mut admission_us,100),
        "description":"Deterministic 8 KiB real-source windows with distinct file identities, full extraction and demand admission; scheduler cooldown and source I/O excluded."
    });
    drop(index);
    drop(worker);

    let (_temporary2, root, cache) = fixture();
    let (index, mut worker) = synchronous(&root, &cache);
    // A smaller configured cache makes every interaction compete with a hot
    // resident set. Pacing remains the production Balanced default.
    let settings = IndexSettings {
        max_memory_mb: 4,
        ..Default::default()
    };
    index.configure(settings.clone());
    for n in 0..128 {
        admit(
            &mut worker,
            record(
                root.join(format!("prefill{n}.custom")),
                &corpus[n % corpus.len()],
            ),
            &settings,
        );
    }
    let background = std::thread::spawn(move || worker.run());
    let mut submission_us = Vec::new();
    let mut query_us = Vec::new();
    let mut ready_ms = Vec::new();
    let mut peak_estimated_bytes = 0;
    for n in 0..160 {
        let path = root.join(format!("live{}.custom", n % 96));
        let expected = format!("activeProbe{n}");
        let rope = ropey::Rope::from_str(&format!(
            "{expected}();\n{}",
            &corpus[(n * 7919) % corpus.len()]
        ));
        let file = crate::completion_source::Source::new(Some(&path), "").file;
        let ready = Instant::now();
        let submit = Instant::now();
        assert!(index.observe_active_buffer(path, rope, true));
        submission_us.push(submit.elapsed().as_secs_f64() * 1e6);
        loop {
            let query = Instant::now();
            std::hint::black_box(index.query("sel", "self."));
            query_us.push(query.elapsed().as_secs_f64() * 1e6);
            if index
                .current_vocabulary(file, "activeProbe")
                .iter()
                .any(|(word, _, _)| word == &expected)
            {
                break;
            }
            assert!(
                ready.elapsed() < Duration::from_secs(8),
                "active file starved: {:?}",
                index.status()
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        ready_ms.push(ready.elapsed().as_secs_f64() * 1000.);
        let status = index.status();
        peak_estimated_bytes = peak_estimated_bytes.max(status.memory_bytes);
        assert!(status.memory_bytes <= settings.max_memory_mb * 1024 * 1024);
    }
    wait(&index, |status| {
        status.phase == IndexPhase::Idle && status.pending_files == 0
    });
    let revision = index.revision();
    std::thread::sleep(Duration::from_millis(180));
    assert_eq!(
        index.revision(),
        revision,
        "churn must finish in a sleeping worker"
    );
    let status = index.status();
    assert!(status.evicted_files > 0);
    let concurrent = serde_json::json!({
        "activeVisits":160,"distinctLiveFiles":96,"fileLimit":settings.max_files,"budgetMiB":4,"speed":"balanced",
        "residentFiles":status.indexed_files,"evictedFiles":status.evicted_files,
        "peakPublishedEstimatedBytes":peak_estimated_bytes,"querySamples":query_us.len(),
        "submissionP50Us":percentile(&mut submission_us,50),
        "submissionP95Us":percentile(&mut submission_us,95),
        "submissionMaxUs":percentile(&mut submission_us,100),
        "queryP50Us":percentile(&mut query_us,50),"queryP95Us":percentile(&mut query_us,95),
        "queryMaxUs":percentile(&mut query_us,100),
        "activeReadyP50Ms":percentile(&mut ready_ms,50),"activeReadyP95Ms":percentile(&mut ready_ms,95),
        "activeReadyMaxMs":percentile(&mut ready_ms,100),"idleRevisionStable":true,
        "description":"Complete real source files plus one admission marker, current dirty buffer on each visit, concurrent project queries and default Balanced pacing; includes publication contention and scheduler delay, excludes source I/O and rendering."
    });
    drop(index);
    background.join().unwrap();
    let report = serde_json::json!({"corpusFiles":corpus.len(),"corpusSha256":corpus_sha,
        "defaultRamPressure":default_cap,"concurrentWorkerPressure":concurrent});
    eprintln!("{}", serde_json::to_string_pretty(&report).unwrap());
    if let Some(path) = std::env::var_os("VTCODE_CACHE_CHURN_REPORT") {
        std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
}
