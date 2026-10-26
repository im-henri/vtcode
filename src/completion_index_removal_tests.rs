use super::*;

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
    (
        CompletionIndex {
            handle: Some(Arc::new(Handle(Arc::clone(&shared)))),
            ..Default::default()
        },
        Worker::new(shared),
    )
}

fn install_text(worker: &mut Worker, path: &Path, text: &str, saved: bool) {
    let mut extractor = stream::Extractor::default();
    extractor.feed(text);
    let (terms, examples) = extractor.finish_with_examples();
    worker.install(
        FileRecord {
            path: path.to_owned(),
            stamp: saved.then(|| Stamp::read(path)).flatten(),
            hash: hash(text.as_bytes()),
            terms: Arc::new(terms),
            examples: Arc::new(examples),
            dirty: !saved,
            memory_bytes: 0,
            reducible_bytes: 0,
        },
        &IndexSettings::default(),
    );
}

fn checkpoint_paths(cache: &Path) -> Vec<PathBuf> {
    let text = std::fs::read_to_string(cache.join("completion-index.jsonl")).unwrap();
    text.lines()
        .skip(1)
        .map(|line| serde_json::from_str::<FileRecord>(line).unwrap().path)
        .collect()
}

#[test]
fn file_pages_cover_all_indexed_files_and_bound_the_last_page() {
    let (_temporary, root, cache) = fixture();
    let (index, mut worker) = synchronous(&root, &cache);
    worker.restore();
    for n in 0..75 {
        install_text(
            &mut worker,
            &root.join(format!("file{n:03}.custom")),
            "sharedIdentifier",
            false,
        );
    }
    let first = index.file_page(0, usize::MAX);
    let middle = index.file_page(32, 32);
    let last = index.file_page(usize::MAX, 32);
    assert_eq!((first.total, first.offset, first.paths.len()), (75, 0, 32));
    assert_eq!((last.offset, last.paths.len()), (64, 11));
    let all: Vec<_> = first
        .paths
        .into_iter()
        .chain(middle.paths)
        .chain(last.paths)
        .collect();
    assert_eq!(
        all,
        (0..75)
            .map(|n| root.join(format!("file{n:03}.custom")))
            .collect::<Vec<_>>()
    );
    assert!(index.file_page(0, 0).paths.is_empty());
}

#[test]
fn erase_detaches_all_learned_evidence_but_preserves_shared_owners() {
    let (_temporary, root, cache) = fixture();
    let (index, mut worker) = synchronous(&root, &cache);
    worker.restore();
    let path = root.join("erase.custom");
    let kept = root.join("kept.custom");
    install_text(
        &mut worker,
        &path,
        "register(chanOld, handlerOld); unregister(chanOld, handlerOld); &exclusiveErasedName; sharedRetainedName",
        false,
    );
    install_text(
        &mut worker,
        &kept,
        "sharedRetainedName; keptOnlyName",
        false,
    );
    let query = examples::Query::new("register(chanNew, handlerNew); unregister(chanNew, ", ")");
    let mut before = examples::Evidence::default();
    index.learn_examples(&query, 0, &mut before);
    assert!(before.candidates().any(|word| word == "handlerNew"));
    assert!(index.remove_file(&path));
    assert!(index.remove_file(&path));
    worker.erase_pending();
    assert_eq!(index.file_page(0, 32).paths, [kept]);
    assert!(index.query("exclusive", "&").is_empty());
    assert_eq!(index.query("sharedRetained", "")[0].frequency, 1);
    let mut after = examples::Evidence::default();
    index.learn_examples(&query, 0, &mut after);
    assert!(!after.candidates().any(|word| word == "handlerNew"));
    let catalog = worker.shared.catalog.read().unwrap();
    assert!(!catalog.terms.contains_key(&key("exclusiveErasedName")));
    let id = crate::completion_source::Source::new(Some(&path), "").file;
    assert!(!catalog.files.contains_key(&id));
    assert!(
        !catalog
            .by_context
            .values()
            .flatten()
            .any(|entry| catalog.spelling(entry.id) == Some("exclusiveErasedName"))
    );
    assert!(worker.shared.control.lock().unwrap().pending.is_empty());
}

#[test]
fn erase_rewrites_checkpoint_survives_restart_and_creates_no_exclusion_rule() {
    let (_temporary, root, cache) = fixture();
    let path = root.join("erase.custom");
    let kept = root.join("kept.custom");
    std::fs::write(&path, "exclusiveDiskIdentifier").unwrap();
    std::fs::write(&kept, "keptDiskIdentifier").unwrap();
    let stamp = Stamp::read(&path);
    let (index, mut worker) = synchronous(&root, &cache);
    worker.restore();
    install_text(&mut worker, &path, "exclusiveDiskIdentifier", true);
    install_text(&mut worker, &kept, "keptDiskIdentifier", true);
    worker.save();
    assert_eq!(checkpoint_paths(&cache).len(), 2);
    assert!(index.remove_file(&path));
    worker.erase_pending();
    worker.save();
    assert_eq!(checkpoint_paths(&cache), [kept.clone()]);
    let bytes = std::fs::read_to_string(cache.join("completion-index.jsonl")).unwrap();
    assert!(!bytes.contains("exclusiveDiskIdentifier"));
    assert!(!bytes.contains("erase.custom"));
    assert_eq!(Stamp::read(&path), stamp);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "exclusiveDiskIdentifier"
    );
    assert!(!cache.join("completion-removed-files.json").exists());
    drop(index);
    drop(worker);
    let (again, mut worker) = synchronous(&root, &cache);
    worker.restore();
    while worker.validate_next(&IndexSettings::default()) {}
    assert_eq!(again.file_page(0, 32).paths, [kept]);
    assert!(again.query("exclusiveDisk", "").is_empty());
    // The ordinary indexing API suffices. There is no restore/unignore step.
    assert!(again.observe_buffer(
        path.clone(),
        ropey::Rope::from_str("freshObservedIdentifier"),
        true
    ));
    assert!(matches!(
        worker.shared.control.lock().unwrap().pending.get(&path),
        Some(Pending::DirtySnapshot(_))
    ));
    worker.index_snapshot(
        path.clone(),
        ropey::Rope::from_str("freshObservedIdentifier"),
        &IndexSettings::default(),
    );
    while !worker.scans.is_empty() {
        worker.advance_scan(&IndexSettings::default());
    }
    assert_eq!(
        again.query("freshObserved", "")[0].word,
        "freshObservedIdentifier"
    );
}

#[test]
fn erase_during_partial_restore_preserves_every_unloaded_peer_record() {
    let (_temporary, root, cache) = fixture();
    let (seed, mut writer) = synchronous(&root, &cache);
    writer.restore();
    let paths: Vec<_> = (0..4)
        .map(|n| root.join(format!("file{n}.custom")))
        .collect();
    for (n, path) in paths.iter().enumerate() {
        let text = format!("savedIdentifier{n}");
        std::fs::write(path, &text).unwrap();
        install_text(&mut writer, path, &text, true);
    }
    writer.save();
    drop(seed);
    drop(writer);
    let (index, mut worker) = synchronous(&root, &cache);
    worker.restore();
    assert!(worker.restore.is_some());
    worker.validate_next(&IndexSettings::default());
    assert_eq!(worker.files.len(), 1);
    assert!(index.remove_file(&paths[3]));
    worker.erase_pending();
    worker.save();
    assert_eq!(checkpoint_paths(&cache), paths[..3]);
    while worker.validate_next(&IndexSettings::default()) {}
    assert_eq!(index.file_page(0, 32).paths, paths[..3]);
    assert!(
        !index
            .query("savedIdentifier", "")
            .iter()
            .any(|word| word.word == "savedIdentifier3")
    );
}

#[test]
fn erase_discards_queued_inflight_restore_and_walk_work_without_a_tombstone() {
    let (_temporary, root, cache) = fixture();
    let (index, mut worker) = synchronous(&root, &cache);
    worker.restore();
    let path = root.join("erase.custom");
    std::fs::write(&path, "savedIdentifier").unwrap();
    let settings = IndexSettings::default();
    worker.queue_snapshot(
        path.clone(),
        ropey::Rope::from_str(&"inflightIdentifier ".repeat(20000)),
        &settings,
        true,
    );
    worker.advance_scan(&settings);
    assert!(!worker.scans.is_empty());
    worker.validation.push_back(path.clone());
    worker.begin_walk();
    assert!(index.remove_file(&path));
    index.release(path.clone());
    index.changed(path.clone());
    assert!(index.observe(path.clone(), ropey::Rope::from_str("queuedBeforeErase")));
    assert!(matches!(
        worker.shared.control.lock().unwrap().pending.get(&path),
        Some(Pending::Erase)
    ));
    worker.erase_pending();
    assert!(worker.scans.is_empty());
    assert!(worker.validation.is_empty());
    assert!(worker.walk.is_none());
    assert!(worker.shared.control.lock().unwrap().pending.is_empty());
    assert!(index.query("inflight", "").is_empty());
    worker.index_snapshot(
        path.clone(),
        ropey::Rope::from_str("newObservation"),
        &settings,
    );
    while !worker.scans.is_empty() {
        worker.advance_scan(&settings);
    }
    assert_eq!(index.query("newObs", "")[0].word, "newObservation");
}

#[test]
fn accepted_erases_survive_cancel_disable_and_pause_without_resuming_indexing() {
    for mode in 0..3 {
        let (_temporary, root, cache) = fixture();
        let (index, mut worker) = synchronous(&root, &cache);
        worker.restore();
        let path = root.join("erase.custom");
        std::fs::write(&path, "persistentIdentifier").unwrap();
        install_text(&mut worker, &path, "persistentIdentifier", true);
        worker.save();
        assert!(index.remove_file(&path));
        let phase = match mode {
            0 => {
                index.cancel();
                IndexPhase::Cancelled
            }
            1 => {
                index.configure(IndexSettings {
                    enabled: false,
                    ..Default::default()
                });
                IndexPhase::Disabled
            }
            _ => {
                index.configure(IndexSettings {
                    paused: true,
                    ..Default::default()
                });
                IndexPhase::Paused
            }
        };
        if mode == 0 {
            worker.shared.control.lock().unwrap().settings.paused = false;
        }
        worker.erase_pending();
        worker.save();
        assert!(checkpoint_paths(&cache).is_empty());
        assert!(index.file_page(0, 32).paths.is_empty());
        assert_eq!(index.status().phase, phase);
        assert!(index.query("persistent", "").is_empty());
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "persistentIdentifier"
        );
    }
}

#[test]
fn checkpoint_failure_keeps_original_data_and_reports_the_error_without_leftover_scratch() {
    use std::io::Write;
    let (_temporary, root, cache) = fixture();
    let (index, mut worker) = synchronous(&root, &cache);
    worker.restore();
    let path = root.join("erase.custom");
    std::fs::write(&path, "protectedIdentifier").unwrap();
    install_text(&mut worker, &path, "protectedIdentifier", true);
    worker.save();
    let checkpoint = cache.join("completion-index.jsonl");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&checkpoint)
        .unwrap()
        .write_all(b"invalid record\n")
        .unwrap();
    let original = std::fs::read(&checkpoint).unwrap();
    assert!(index.remove_file(&path));
    worker.erase_pending();
    assert_eq!(std::fs::read(&checkpoint).unwrap(), original);
    assert!(worker.files.contains_key(&path));
    assert!(index.status().error.is_some());
    assert_eq!(index.query("protected", "")[0].word, "protectedIdentifier");
    assert!(!std::fs::read_dir(&cache).unwrap().any(|entry| {
        entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|ext| ext == "tmp")
    }));
}

#[test]
fn erase_requests_are_bounded_validate_paths_and_can_replace_a_full_queue_entry() {
    let (_temporary, root, cache) = fixture();
    let (index, mut worker) = synchronous(&root, &cache);
    worker.restore();
    assert!(!index.remove_file(&root));
    assert!(!index.remove_file(&root.join("..").join("outside")));
    assert!(!index.remove_file(&root.join("x".repeat(MAX_CACHE_PATH_BYTES + 1))));
    for n in 0..MAX_PENDING_FILES {
        assert!(index.observe(
            root.join(format!("file{n}.custom")),
            ropey::Rope::from_str("queuedIdentifier")
        ));
    }
    assert!(!index.remove_file(&root.join("other.custom")));
    assert!(index.remove_file(&root.join("file0.custom")));
    worker.erase_pending();
    let control = worker.shared.control.lock().unwrap();
    assert_eq!(control.pending.len(), MAX_PENDING_FILES - 1);
    assert_eq!(
        control.pending_bytes,
        control
            .pending
            .values()
            .map(Pending::retained_bytes)
            .sum::<usize>()
    );
}

#[cfg(windows)]
#[test]
fn erase_removes_all_windows_path_aliases_from_memory_and_the_checkpoint() {
    let (_temporary, root, cache) = fixture();
    let (index, mut worker) = synchronous(&root, &cache);
    worker.restore();
    let path = root.join("alias.custom");
    let alias = root.join("ALIAS.CUSTOM");
    std::fs::write(&path, "sourceUntouched").unwrap();
    install_text(&mut worker, &path, "firstAliasIdentifier", true);
    install_text(&mut worker, &alias, "secondAliasIdentifier", true);
    worker.save();
    assert_eq!(checkpoint_paths(&cache).len(), 2);
    assert!(index.remove_file(&alias));
    worker.erase_pending();
    worker.save();
    assert!(checkpoint_paths(&cache).is_empty());
    assert!(worker.files.is_empty());
    assert!(index.query("AliasIdentifier", "").is_empty());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "sourceUntouched");
}

#[test]
fn obsolete_exclusion_artifacts_are_deleted_without_reading_or_retaining_rules() {
    let (_temporary, root, cache) = fixture();
    std::fs::create_dir_all(&cache).unwrap();
    for name in [
        "completion-removed-files.json",
        "completion-removed-files.backup.json",
    ] {
        std::fs::write(cache.join(name), "not a preferences schema").unwrap();
    }
    let (_index, mut worker) = synchronous(&root, &cache);
    worker.restore();
    assert!(!cache.join("completion-removed-files.json").exists());
    assert!(!cache.join("completion-removed-files.backup.json").exists());
    assert!(worker.files.is_empty());
}

#[test]
fn an_accepted_erase_is_flushed_when_the_last_query_handle_closes_immediately() {
    for _ in 0..8 {
        let (_temporary, root, cache) = fixture();
        let path = root.join("shutdown.custom");
        std::fs::write(&path, "shutdownIdentifier").unwrap();
        let (seed, mut writer) = synchronous(&root, &cache);
        writer.restore();
        install_text(&mut writer, &path, "shutdownIdentifier", true);
        writer.save();
        drop(seed);
        drop(writer);
        let index = CompletionIndex::new_for_test(root, cache.clone());
        index.configure(IndexSettings {
            paused: false,
            speed: IndexSpeed::Fast,
            ..Default::default()
        });
        settle(&index, 1);
        assert!(index.remove_file(&path));
        drop(index);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let checkpoint = std::fs::read_to_string(cache.join("completion-index.jsonl")).unwrap();
            if !checkpoint.contains("shutdownIdentifier") {
                break;
            }
            assert!(Instant::now() < deadline, "shutdown lost an accepted erase");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(std::fs::read_to_string(path).unwrap(), "shutdownIdentifier");
    }
}
