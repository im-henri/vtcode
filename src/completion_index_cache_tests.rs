//! Cache pressure and lifecycle coverage through the actual background service.
use super::*;
#[path = "completion_index_cache_bench.rs"]
mod benchmarks;

fn record(path: PathBuf, text: &str) -> FileRecord {
    let mut parser = stream::Extractor::default();
    parser.feed(text);
    let (terms, examples) = parser.finish_with_examples();
    FileRecord {
        path,
        stamp: None,
        hash: hash(text.as_bytes()),
        terms: terms.into(),
        examples: Arc::new(examples),
        dirty: true,
        memory_bytes: 0,
        reducible_bytes: 0,
    }
}

fn synchronous(root: &Path, cache: &Path) -> (CompletionIndex, Worker) {
    let shared = Arc::new(Shared {
        root: root.to_owned(),
        cache_dir: cache.to_owned(),
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
    (index, Worker::new(shared))
}

fn admit(worker: &mut Worker, record: FileRecord, settings: &IndexSettings) {
    worker.install_with_policy(record, settings, true);
    for _ in 0..10_000 {
        if worker.admission.is_none() {
            return;
        }
        worker.advance_admission(settings);
    }
    panic!("cache reclamation did not finish");
}

fn wait_word(index: &CompletionIndex, prefix: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while index.query(prefix, "").is_empty() {
        assert!(
            Instant::now() < deadline,
            "missing {prefix}: {:?}",
            index.status()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn index_parks_through_foreground_work_and_resumes_without_another_observation() {
    let (_temporary, root, cache) = fixture();
    let index = CompletionIndex::new_in(root.clone(), cache);
    let activity = crate::completion_activity::Activity::default();
    index.set_activity(activity.clone());
    let foreground = activity.enter();
    assert!(index.observe_active_buffer(
        root.join("active"),
        ropey::Rope::from_str("activeIdentifier"),
        true
    ));
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(index.file_page(0, 32).total, 0);
    assert!(index.query("activeId", "").is_empty());
    drop(foreground);
    wait_word(&index, "activeId");
    settle(&index, 1);
}

#[test]
fn dirty_checkpoint_waits_for_quiet_and_flushes_on_its_own_deadline() {
    let (_temporary, root, cache) = fixture();
    let (index, mut worker) = synchronous(&root, &cache);
    worker.restore();
    let path = root.join("saved.rs");
    let settings = IndexSettings::default();
    let saved = |word: &str| {
        std::fs::write(&path, word).unwrap();
        let mut record = record(path.clone(), word);
        record.stamp = Stamp::read(&path);
        record.dirty = false;
        record
    };
    admit(&mut worker, saved("firstIdentifier"), &settings);
    worker.save();
    admit(&mut worker, saved("latestCheckpointIdentifier"), &settings);
    let activity = crate::completion_activity::Activity::default();
    index.set_activity(activity.clone());
    activity.note_input();
    worker.save();
    assert!(worker.cache_dirty);
    assert!(worker.pending_save_wait().is_some());
    let checkpoint = cache.join("completion-index.jsonl");
    assert!(
        !std::fs::read_to_string(&checkpoint)
            .unwrap()
            .contains("latestCheckpointIdentifier")
    );
    let background = std::thread::spawn(move || worker.run());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if std::fs::read_to_string(&checkpoint)
            .unwrap()
            .contains("latestCheckpointIdentifier")
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "dirty cache must flush without another input or index event"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(index);
    background.join().unwrap();
}

#[test]
fn opening_files_at_capacity_replaces_lru_and_refocusing_restores_evicted_files() {
    let (_temporary, root, cache) = fixture();
    let index = CompletionIndex::new_in(root.clone(), cache);
    index.configure(IndexSettings {
        max_files: 2,
        speed: IndexSpeed::Fast,
        ..Default::default()
    });
    let a = root.join("z-first.custom");
    let b = root.join("a-second.custom");
    let c = root.join("m-third.custom");
    for (path, text) in [
        (&a, "firstIdentifier sharedIdentifier"),
        (&b, "secondIdentifier sharedIdentifier"),
        (&c, "thirdIdentifier sharedIdentifier"),
    ] {
        std::fs::write(path, text).unwrap();
    }
    let open = |path: &Path, word: &str| {
        assert!(index.observe_active_buffer(path.to_owned(), ropey::Rope::new(), false));
        wait_word(&index, word);
        settle(&index, 2.min(index.file_page(0, 32).total));
    };
    open(&a, "firstId");
    open(&b, "secondId");
    open(&a, "firstId"); // a is hot despite being alphabetically last.
    open(&c, "thirdId");
    wait(&index, |status| {
        status.evicted_files == 1 && status.phase == IndexPhase::Idle
    });
    assert_eq!(index.file_page(0, 32).paths, vec![c.clone(), a.clone()]);
    assert!(index.query("secondId", "").is_empty());
    assert_eq!(index.query("sharedId", "")[0].frequency, 2);
    assert_eq!(index.status().evicted_files, 1);
    open(&b, "secondId");
    assert_eq!(index.file_page(0, 32).paths, vec![b, c]);
    assert!(index.query("firstId", "").is_empty());
    assert_eq!(index.query("sharedId", "")[0].frequency, 2);
}

#[test]
fn memory_pressure_recycles_entries_and_balances_all_retained_allocations() {
    let (_temporary, root, cache) = fixture();
    let (index, mut worker) = synchronous(&root, &cache);
    let settings = IndexSettings {
        max_memory_mb: 4,
        ..Default::default()
    };
    let pinned = root.join("pinned.custom");
    admit(
        &mut worker,
        record(pinned.clone(), "pinnedIdentifier sharedIdentifier"),
        &settings,
    );
    worker.shared.active_file.store(
        crate::completion_source::Source::new(Some(&pinned), "").file,
        Ordering::Release,
    );
    for n in 0..240 {
        let text: String = (0..96)
            .map(|term| format!("receiver{n}.file{n}Value{term}(); sharedIdentifier;\n"))
            .collect();
        let path = root.join(format!("file{n}.custom"));
        admit(&mut worker, record(path.clone(), &text), &settings);
        assert!(
            worker.files.contains_key(&path),
            "the newest requested file must be admitted"
        );
        assert!(
            worker.files.contains_key(&pinned),
            "background opens must not evict the active file"
        );
        let catalog = worker.shared.catalog.read().unwrap();
        assert!(
            worker.memory + worker.recency.memory() + catalog.memory_bytes
                <= retained_limit(&settings)
        );
        assert_eq!(
            worker.example_bytes,
            worker
                .files
                .values()
                .map(|r| r.examples.memory())
                .sum::<usize>()
        );
        assert_eq!(
            catalog.examples.retained_memory(),
            catalog.examples.memory() + worker.example_bytes
        );
    }
    assert!(
        worker.evicted > 100,
        "exercise repeated RAM eviction, not just the file-count limit"
    );
    assert!(!index.query("pinnedId", "").is_empty());
    let paths: Vec<_> = worker.files.keys().cloned().collect();
    for path in paths {
        worker.remove(&path);
    }
    assert_eq!(
        (worker.memory, worker.example_bytes, worker.recency.memory()),
        (0, 0, 0)
    );
    assert_eq!(worker.shared.catalog.read().unwrap().memory_bytes, 0);
}

#[test]
fn active_open_preempts_cold_scans_and_a_full_queue() {
    let (_temporary, root, cache) = fixture();
    let index = CompletionIndex::new_in(root.clone(), cache);
    index.configure(IndexSettings {
        paused: true,
        speed: IndexSpeed::Fast,
        max_files: 2,
        ..Default::default()
    });
    wait(&index, |status| status.phase == IndexPhase::Paused);
    let large = ropey::Rope::from_str(&"coldIdentifier ".repeat(80_000));
    for name in ["a-large", "b-large"] {
        assert!(index.observe_buffer(root.join(name), large.clone(), true));
    }
    for n in 2..MAX_PENDING_FILES {
        assert!(index.observe_buffer(
            root.join(format!("c{n:03}")),
            ropey::Rope::from_str("queuedIdentifier"),
            true
        ));
    }
    let active = root.join("z-active");
    // The caller retries backpressure without losing the active-file priority.
    assert!(!index.observe_active_buffer(
        active.clone(),
        ropey::Rope::from_str("activeIdentifier"),
        true
    ));
    let mut settings = index.settings();
    settings.paused = false;
    index.configure(settings);
    let started = Instant::now();
    loop {
        if index.observe_active_buffer(
            active.clone(),
            ropey::Rope::from_str("activeIdentifier"),
            true,
        ) {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(5));
    }
    wait_word(&index, "activeId");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "active open must bypass the large cold scans"
    );
    index.cancel();
}

#[test]
fn erase_drops_a_staged_candidate_before_it_can_return() {
    let (_temporary, root, cache) = fixture();
    let (index, mut worker) = synchronous(&root, &cache);
    worker.restore();
    let settings = IndexSettings {
        max_files: 1,
        ..Default::default()
    };
    let first = root.join("first");
    let next = root.join("next");
    admit(
        &mut worker,
        record(first.clone(), "firstIdentifier"),
        &settings,
    );
    worker.install_with_policy(record(next.clone(), "nextIdentifier"), &settings, true);
    assert!(worker.admission.is_some());
    assert!(index.remove_file(&next));
    worker.erase_pending();
    assert!(worker.admission.is_none());
    assert!(worker.files.contains_key(&first));
    assert!(index.query("nextId", "").is_empty());
}

#[test]
fn pending_admission_respects_newer_edits_close_and_cancel() {
    for mode in 0..3 {
        let (_temporary, root, cache) = fixture();
        let (index, mut worker) = synchronous(&root, &cache);
        worker.restore();
        let settings = IndexSettings {
            max_files: 1,
            speed: IndexSpeed::Fast,
            ..Default::default()
        };
        index.configure(settings.clone());
        let first = root.join("first");
        let next = root.join("next");
        admit(
            &mut worker,
            record(first.clone(), "firstIdentifier"),
            &settings,
        );
        worker.install_with_policy(
            record(next.clone(), "staleDraftIdentifier"),
            &settings,
            true,
        );
        assert!(worker.admission.is_some());
        match mode {
            0 => {
                assert!(index.observe_active_buffer(
                    next.clone(),
                    ropey::Rope::from_str("latestDraftIdentifier"),
                    true
                ));
            }
            1 => index.release(next.clone()),
            _ => index.cancel(),
        }
        let background = std::thread::spawn(move || worker.run());
        if mode == 0 {
            wait_word(&index, "latestDraft");
            wait(&index, |status| {
                status.phase == IndexPhase::Idle && status.pending_files == 0
            });
            assert_eq!(index.file_page(0, 32).paths, vec![next]);
        } else {
            wait(&index, |status| {
                status.pending_files == 0
                    && status.phase
                        == if mode == 1 {
                            IndexPhase::Idle
                        } else {
                            IndexPhase::Cancelled
                        }
            });
            assert_eq!(index.file_page(0, 32).paths, vec![first]);
        }
        assert!(index.query("staleDraft", "").is_empty());
        drop(index);
        background.join().unwrap();
    }
}

#[test]
fn background_discovery_at_capacity_keeps_the_active_file_and_saves_new_working_set() {
    let (_temporary, root, cache) = fixture();
    let (index, mut worker) = synchronous(&root, &cache);
    worker.restore();
    let settings = IndexSettings {
        max_files: 1,
        ..Default::default()
    };
    let first = root.join("first");
    let active = root.join("active");
    for (path, word) in [(&first, "firstIdentifier"), (&active, "activeIdentifier")] {
        std::fs::write(path, word).unwrap();
    }
    admit(&mut worker, record(first, "firstIdentifier"), &settings);
    let mut current = record(active.clone(), "activeIdentifier");
    current.stamp = Stamp::read(&active);
    current.dirty = false;
    admit(&mut worker, current, &settings);
    worker.install(
        record(root.join("unopened"), "unopenedIdentifier"),
        &settings,
    );
    assert!(worker.limited);
    assert!(worker.checkpoint_complete);
    assert!(worker.files.contains_key(&active));
    assert!(index.query("unopenedId", "").is_empty());
    worker.save();
    let checkpoint = std::fs::read_to_string(cache.join("completion-index.jsonl")).unwrap();
    let restored: FileRecord = serde_json::from_str(checkpoint.lines().nth(1).unwrap()).unwrap();
    assert_eq!(restored.path, active);
}
