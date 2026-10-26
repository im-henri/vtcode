use super::*;
#[path = "completion_index_cache_tests.rs"]
mod cache_tests;
#[path = "completion_index_capacity_tests.rs"]
mod capacity_tests;
#[path = "completion_index_removal_tests.rs"]
mod removal_tests;
#[path = "completion_index_source_tests.rs"]
mod source_tests;
#[path = "completion_index_buffer_tests.rs"]
mod buffer_tests;
#[path = "completion_index_remote_cache_tests.rs"]
mod remote_cache_tests;

fn wait(index: &CompletionIndex, predicate: impl Fn(&IndexStatus) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let status = index.status();
        if predicate(&status) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "index did not settle: {status:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn settle(index: &CompletionIndex, files: usize) {
    wait(index, |status| {
        status.indexed_files == files
            && matches!(status.phase, IndexPhase::Idle | IndexPhase::Limited)
    });
}

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("project");
    let cache = temporary.path().join("state");
    std::fs::create_dir(&root).unwrap();
    (temporary, root, cache)
}

#[test]
fn a_completed_scan_can_yield_finalization_without_replaying_source_tokens() {
    let (_temporary, root, _) = fixture();
    let path = root.join("replayed.rs");
    let text = "receiver.sendPacket(); 変数.結果();\n".repeat(3);
    std::fs::write(&path, &text).unwrap();
    for disk in [false, true] {
        let make = || {
            if disk {
                stream::Scan::disk(
                    path.clone(),
                    Stamp::read(&path).unwrap(),
                    DEFAULT_MAX_FILE_BYTES,
                )
                .unwrap()
            } else {
                stream::Scan::snapshot(
                    path.clone(),
                    ropey::Rope::from_str(&text),
                    DEFAULT_MAX_FILE_BYTES,
                    true,
                )
            }
        };
        let mut immediate = make();
        while !immediate.step().unwrap() {}
        let expected = serde_json::to_value(immediate.finish()).unwrap();
        let mut yielded = make();
        while !yielded.step().unwrap() {}
        assert!(yielded.step().unwrap());
        assert!(yielded.step().unwrap());
        assert_eq!(serde_json::to_value(yielded.finish()).unwrap(), expected);
    }
}

#[test]
fn streaming_extractor_preserves_words_calls_and_context_across_all_boundaries() {
    let text = "&source_value; client.sendPacket (payload); client.sendPacket(); 変数.結果(引数); let copy = &source_value;";
    let expected = serde_json::to_value(extract(text)).unwrap();
    for split in (0..=text.len()).filter(|&split| text.is_char_boundary(split)) {
        let mut parser = stream::Extractor::default();
        parser.feed(&text[..split]);
        parser.feed(&text[split..]);
        assert_eq!(
            serde_json::to_value(parser.finish()).unwrap(),
            expected,
            "split at {split}"
        );
    }
    let mut parser = stream::Extractor::default();
    for ch in text.chars() {
        parser.feed(ch.encode_utf8(&mut [0; 4]));
    }
    assert_eq!(serde_json::to_value(parser.finish()).unwrap(), expected);
    let mut parser = stream::Extractor::default();
    parser.feed(&"x".repeat(MAX_WORD_BYTES + 10));
    parser.feed(" validIdentifier");
    let terms = parser.finish();
    assert_eq!(terms.len(), 1);
    assert_eq!(terms[0].word.as_ref(), "validIdentifier");
}

#[test]
fn ordered_examples_preserve_tokens_across_arbitrary_stream_boundaries() {
    let text =
        "register(каналA, handlerA);\nunregister(каналA, handlerA);\nregister(каналB, handlerB);";
    let mut complete = stream::Extractor::default();
    complete.feed(text);
    let expected = serde_json::to_value(complete.finish_with_examples().1).unwrap();
    for split in (0..=text.len()).filter(|&split| text.is_char_boundary(split)) {
        let mut parser = stream::Extractor::default();
        parser.feed(&text[..split]);
        parser.feed(&text[split..]);
        let data = parser.finish_with_examples().1;
        assert!(data.valid());
        assert_eq!(
            serde_json::to_value(data).unwrap(),
            expected,
            "split at {split}"
        );
    }
    let mut parser = stream::Extractor::default();
    for ch in text.chars() {
        parser.feed(ch.encode_utf8(&mut [0; 4]));
    }
    assert_eq!(
        serde_json::to_value(parser.finish_with_examples().1).unwrap(),
        expected
    );
}

#[test]
fn ordered_examples_share_the_existing_memory_ceiling_under_admission_pressure() {
    let (_temporary, root, cache) = fixture();
    let index = CompletionIndex::new_for_test(root.clone(), cache);
    settle(&index, 0);
    let shared = Arc::clone(&index.handle.as_ref().unwrap().0);
    let mut worker = Worker::new(Arc::clone(&shared));
    let settings = IndexSettings {
        max_memory_mb: 4,
        ..Default::default()
    };
    for file in 0..64 {
        let text: String = (0..48).map(|n| format!(
            "register{file}(channel{n}, handler{n}); unregister{file}(channel{n}, handler{n});\n"
        )).collect();
        let mut parser = stream::Extractor::default();
        parser.feed(&text);
        let (terms, examples) = parser.finish_with_examples();
        worker.install(
            FileRecord {
                path: root.join(format!("file{file}.never_seen")),
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
        let catalog = shared.catalog.read().unwrap();
        assert!(worker.memory + catalog.memory_bytes <= retained_limit(&settings));
        assert_eq!(
            catalog.examples.retained_memory(),
            catalog.examples.memory()
                + worker
                    .files
                    .values()
                    .map(|r| r.examples.memory())
                    .sum::<usize>()
        );
        drop(catalog);
        if worker.limited {
            break;
        }
    }
    assert!(
        worker.limited
            || worker
                .files
                .values()
                .any(|record| record.examples.tokens() < Catalog::retained_tokens()),
        "workload must exercise actual admission pressure or evidence compaction"
    );
    assert!(!worker.files.is_empty());
    assert!(index.example_memory() > 0);
}

#[test]
fn memory_pressure_compacts_evidence_without_discarding_open_file_vocabulary() {
    let (_temporary, root, cache) = fixture();
    let index = CompletionIndex::new_for_test(root.clone(), cache);
    settle(&index, 0);
    let shared = Arc::clone(&index.handle.as_ref().unwrap().0);
    let mut worker = Worker::new(Arc::clone(&shared));
    let settings = IndexSettings {
        max_memory_mb: 4,
        ..Default::default()
    };
    for file in 0..40 {
        let text: String = (0..48).map(|value| format!(
            "emit(commonChannel, file{file}Value{value}); unemit(commonChannel, file{file}Value{value});\n"
        )).collect();
        let mut parser = stream::Extractor::default();
        parser.feed(&text);
        let (terms, examples) = parser.finish_with_examples();
        worker.install(
            FileRecord {
                path: root.join(format!("file{file}.custom")),
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
    assert_eq!(worker.files.len(), 40);
    assert!(!worker.limited);
    assert!(
        worker.memory + shared.catalog.read().unwrap().memory_bytes <= retained_limit(&settings)
    );
    let compact = worker
        .files
        .values()
        .find(|record| record.examples.tokens() < Catalog::retained_tokens())
        .unwrap()
        .clone();
    let file = crate::completion_source::Source::new(Some(&compact.path), "").file;
    let decoded = compact.decoded(&shared.catalog.read().unwrap());
    let unique = decoded
        .terms
        .iter()
        .find(|term| term.word.starts_with("file"))
        .unwrap()
        .word
        .clone();
    assert!(
        index
            .current_vocabulary(file, &unique)
            .iter()
            .any(|(word, _, _)| word == unique.as_ref())
    );
    // Replacing near a full budget counts the replacement once.
    worker.install(decoded, &settings);
    assert_eq!(worker.files.len(), 40);
    assert!(!worker.limited);
    let catalog = shared.catalog.read().unwrap();
    let ids: Vec<_> = worker
        .files
        .values()
        .map(|record| {
            record
                .terms
                .iter()
                .find(|term| catalog.spelling(term.id).unwrap() == "commonChannel")
                .unwrap()
                .id
        })
        .collect();
    assert!(ids.iter().all(|id| *id == ids[0]));
    drop(catalog);
    let paths: Vec<_> = worker.files.keys().cloned().collect();
    for path in paths {
        worker.remove(&path);
    }
    assert_eq!(worker.memory, 0);
    let catalog = shared.catalog.read().unwrap();
    assert_eq!(catalog.memory_bytes, 0);
    assert_eq!(catalog.examples.memory(), 0);
    assert!(catalog.terms.is_empty());
}

#[test]
fn streaming_file_handles_utf8_boundaries_and_rejects_binary_or_changed_sources() {
    let (_temporary, root, _) = fixture();
    let path = root.join("streamed");
    let text = format!(
        "{}識別名(); &tailIdentifier",
        " ".repeat(stream::CHUNK_BYTES - 1)
    );
    std::fs::write(&path, &text).unwrap();
    let mut scan =
        stream::Scan::disk(path.clone(), Stamp::read(&path).unwrap(), MAX_FILE_BYTES).unwrap();
    assert!(!scan.step().unwrap());
    assert_eq!(scan.progress().0, stream::CHUNK_BYTES);
    assert!(scan.step().unwrap());
    assert_eq!(
        serde_json::to_value(scan.finish().terms).unwrap(),
        serde_json::to_value(extract(&text)).unwrap()
    );
    for content in [b"hello\0world".as_slice(), b"invalid\xff".as_slice()] {
        std::fs::write(&path, content).unwrap();
        let mut scan =
            stream::Scan::disk(path.clone(), Stamp::read(&path).unwrap(), MAX_FILE_BYTES).unwrap();
        assert!(scan.step().is_err());
    }
    std::fs::write(&path, "initialIdentifier").unwrap();
    let mut scan =
        stream::Scan::disk(path.clone(), Stamp::read(&path).unwrap(), MAX_FILE_BYTES).unwrap();
    std::fs::write(&path, "replacementIdentifierLonger").unwrap();
    assert!(scan.step().is_err());
}

#[test]
fn large_open_buffer_is_admitted_and_background_scan_can_pause_resume_and_cancel() {
    let (_temporary, root, cache) = fixture();
    let path = root.join("large.custom");
    let text = format!("{} &tailIdentifier", " ".repeat(9 * 1024 * 1024));
    let index = CompletionIndex::new_for_test(root.clone(), cache);
    index.configure(IndexSettings {
        paused: false,
        speed: IndexSpeed::Fast,
        ..IndexSettings::default()
    });
    assert!(index.observe_buffer(path.clone(), ropey::Rope::from_str(&text), true));
    wait(&index, |s| {
        s.phase == IndexPhase::Indexing && s.processed_bytes > 0
    });
    let mut settings = index.settings();
    settings.paused = true;
    index.configure(settings.clone());
    wait(&index, |s| s.phase == IndexPhase::Paused);
    let bytes = index.status().processed_bytes;
    let revision = index.revision();
    std::thread::sleep(Duration::from_millis(180));
    assert_eq!(index.status().processed_bytes, bytes);
    assert_eq!(index.revision(), revision);
    settings.paused = false;
    index.configure(settings);
    settle(&index, 1);
    assert_eq!(index.query("tail", "&")[0].word, "tailIdentifier");
    assert!(index.status().memory_bytes < 64 * 1024);
    assert!(index.observe_buffer(root.join("cancelled"), ropey::Rope::from_str(&text), true));
    wait(&index, |s| {
        s.phase == IndexPhase::Indexing && s.processed_bytes > 0
    });
    index.cancel();
    wait(&index, |s| s.phase == IndexPhase::Cancelled);
    assert_eq!(index.status().pending_files, 0);
    assert_eq!(index.status().indexed_files, 1);
}

#[test]
fn expanded_preferences_bound_and_persist_together() {
    assert_eq!(IndexSettings::default().max_file_bytes, 100 * 1024 * 1024);
    let (_temporary, root, cache) = fixture();
    let index = CompletionIndex::new_for_test(root.clone(), cache.clone());
    let settings = IndexSettings {
        max_file_bytes: MAX_FILE_BYTES,
        min_prefix_chars: 3,
        context_triggers: false,
        complete_calls: false,
        use_history: false,
        paused: true,
        ..IndexSettings::default()
    };
    index.configure(settings.clone());
    wait(&index, |s| s.phase == IndexPhase::Paused);
    drop(index);
    let restored = CompletionIndex::new_for_test(root, cache);
    wait(&restored, |s| s.phase == IndexPhase::Paused);
    assert_eq!(restored.settings(), settings);
}

#[test]
fn lowering_file_limit_discards_a_paused_large_scan() {
    let (_temporary, root, cache) = fixture();
    let index = CompletionIndex::new_for_test(root.clone(), cache);
    index.configure(IndexSettings {
        paused: false,
        speed: IndexSpeed::Fast,
        ..IndexSettings::default()
    });
    assert!(index.observe_buffer(
        root.join("large"),
        ropey::Rope::from_str(&" ".repeat(3 * 1024 * 1024)),
        true
    ));
    wait(&index, |s| {
        s.phase == IndexPhase::Indexing && s.processed_bytes > 0
    });
    let mut settings = index.settings();
    settings.paused = true;
    index.configure(settings.clone());
    wait(&index, |s| s.phase == IndexPhase::Paused);
    settings.max_file_bytes = 1024 * 1024;
    settings.paused = false;
    index.configure(settings);
    settle(&index, 0);
    assert_eq!(index.status().pending_files, 0);
}

#[test]
#[ignore = "manual 100 MiB streaming and memory probe"]
fn benchmark_hundred_megabyte_source_uses_bounded_slices() {
    use std::io::Write;
    let (_temporary, root, _) = fixture();
    let path = root.join("100MiB.source");
    let mut file = std::fs::File::create(&path).unwrap();
    let block = [b' '; stream::CHUNK_BYTES];
    for _ in 0..(100 * 1024 * 1024 / stream::CHUNK_BYTES - 1) {
        file.write_all(&block).unwrap();
    }
    file.write_all(&block[..stream::CHUNK_BYTES - b" &largeTailIdentifier".len()])
        .unwrap();
    file.write_all(b" &largeTailIdentifier").unwrap();
    drop(file);
    let mut scan = stream::Scan::disk(
        path.clone(),
        Stamp::read(&path).unwrap(),
        DEFAULT_MAX_FILE_BYTES,
    )
    .unwrap();
    let start = Instant::now();
    let mut slices = 0;
    loop {
        let before = scan.progress().0;
        let complete = scan.step().unwrap();
        assert!(scan.progress().0 - before <= stream::CHUNK_BYTES);
        slices += 1;
        if complete {
            break;
        }
    }
    let record = scan.finish();
    assert_eq!(record.terms.len(), 1);
    assert_eq!(record.terms[0].word.as_ref(), "largeTailIdentifier");
    eprintln!(
        "100 MiB source: {slices} slices, {:?} active processing, {} retained terms",
        start.elapsed(),
        record.terms.len()
    );
}

#[test]
fn opened_files_are_the_only_default_source_and_idle_really_sleeps() {
    let (_temporary, root, cache) = fixture();
    let first = root.join("first.any-language");
    let unopened = root.join("unopened.unknown");
    std::fs::write(&first, "openedIdentifier openedIdentifier").unwrap();
    std::fs::write(&unopened, "unopenedIdentifier").unwrap();
    // Exercise the production constructor, not the explicitly running fixture.
    let index = CompletionIndex::new_in(root.clone(), cache.clone());
    settle(&index, 0);
    assert!(!index.settings().paused);
    assert!(!index.settings().project_wide);
    let missing: IndexSettings = serde_json::from_str("{}").unwrap();
    assert!(!missing.paused);
    assert!(!missing.project_wide);
    let chosen: IndexSettings =
        serde_json::from_str(r#"{"paused":false,"projectWide":true}"#).unwrap();
    assert!(!chosen.paused);
    assert!(chosen.project_wide);
    index.changed(unopened);
    assert!(index.observe_active_buffer(
        first,
        ropey::Rope::from_str("openedIdentifier openedIdentifier"),
        false,
    ));
    // Refresh validates the working set without discovering unopened files.
    index.rescan();
    settle(&index, 1);
    assert_eq!(index.query("openedI", "")[0].frequency, 2);
    assert!(index.query("unopenedI", "").is_empty());
    let revision = index.revision();
    std::thread::sleep(Duration::from_millis(180));
    assert_eq!(
        index.revision(),
        revision,
        "idle worker must not poll or publish"
    );
    drop(index);
    let restored = CompletionIndex::new_in(root, cache);
    settle(&restored, 1);
    assert!(
        !restored.settings().paused,
        "automatic learning must survive reopening without configuration"
    );
    assert!(!restored.settings().project_wide);
}

#[test]
fn learned_receiver_context_beats_frequency_and_supports_empty_explicit_query() {
    let (_temporary, root, cache) = fixture();
    let text = "socket.sendPacket(); socket.sendPacket();\nlogger.sendMessage(); logger.sendMessage(); logger.sendMessage();\n変数.結果(); 変数.結果();";
    let path = root.join("source.custom");
    std::fs::write(&path, text).unwrap();
    let index = CompletionIndex::new_for_test(root, cache);
    assert!(index.observe(path, ropey::Rope::from_str(text)));
    settle(&index, 1);
    let candidates = index.query("sP", "socket.");
    assert_eq!(candidates[0].word, "sendPacket");
    assert!(candidates[0].callable);
    let candidates = index.query("send", "socket.");
    assert_eq!(candidates[0].word, "sendPacket");
    assert!(candidates[0].context_score > candidates[1].context_score);
    let explicit = index.query("", "socket.");
    assert_eq!(explicit[0].word, "sendPacket");
    assert_eq!(index.query("結", "変数.")[0].word, "結果");
    index.record_acceptance("sendMessage", "socket.");
    assert!(
        index
            .query("send", "socket.")
            .iter()
            .any(|entry| entry.word == "sendMessage")
    );
}

#[test]
fn accepted_choice_learning_can_be_disabled_without_changing_context_evidence() {
    let (_temporary, root, cache) = fixture();
    let path = root.join("history");
    let text = "client.sendPacket(); client.sendMessage();";
    std::fs::write(&path, text).unwrap();
    let index = CompletionIndex::new_for_test(root, cache);
    index.observe(path, ropey::Rope::from_str(text));
    settle(&index, 1);
    let score = || {
        let entry = &index.query("sendM", "client.")[0];
        entry.context_score + entry.history_score
    };
    let baseline = score();
    index.record_acceptance("sendMessage", "client.");
    assert!(score() > baseline);
    let mut settings = index.settings();
    settings.use_history = false;
    index.configure(settings);
    assert_eq!(score(), baseline);
    index.record_acceptance("sendMessage", "client.");
    assert_eq!(score(), baseline);
}

#[test]
fn cache_is_streamed_validated_and_never_restores_stale_or_dirty_words() {
    let (_temporary, root, cache) = fixture();
    let path = root.join("source");
    std::fs::write(&path, "savedIdentifier").unwrap();
    let index = CompletionIndex::new_for_test(root.clone(), cache.clone());
    index.observe(path.clone(), ropey::Rope::from_str("savedIdentifier"));
    settle(&index, 1);
    let checkpoint =
        std::fs::read_to_string(cache.join("completion-index.jsonl")).unwrap_or_else(|error| {
            panic!("checkpoint missing: {error}; status: {:?}", index.status())
        });
    assert_eq!(checkpoint.lines().count(), 2);
    drop(index);
    // A saved change between sessions must be validated before publication.
    std::fs::write(&path, "replacementIdentifierLonger").unwrap();
    let index = CompletionIndex::new_for_test(root.clone(), cache.clone());
    settle(&index, 1);
    assert!(index.query("saved", "").is_empty());
    assert_eq!(
        index.query("replacement", "")[0].word,
        "replacementIdentifierLonger"
    );
    index.observe(path.clone(), ropey::Rope::from_str("unsavedIdentifier"));
    settle(&index, 1);
    // Wait until the dirty snapshot, rather than the old Idle status, lands.
    let deadline = Instant::now() + Duration::from_secs(5);
    while index.query("unsaved", "").is_empty() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    wait(&index, |s| s.phase == IndexPhase::Idle);
    assert!(
        !std::fs::read_to_string(cache.join("completion-index.jsonl"))
            .unwrap()
            .contains("unsavedIdentifier")
    );
    index.release(path);
    let deadline = Instant::now() + Duration::from_secs(5);
    while index.query("replacement", "").is_empty() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(index.query("unsaved", "").is_empty());
}

#[test]
fn project_walk_is_opt_in_ignores_generated_and_binary_files_and_honors_limits() {
    let (_temporary, root, cache) = fixture();
    std::fs::write(root.join(".gitignore"), "ignored.custom\n").unwrap();
    std::fs::write(root.join("a.custom"), "firstProjectIdentifier").unwrap();
    std::fs::write(root.join("b.custom"), "secondProjectIdentifier").unwrap();
    std::fs::write(root.join("c.custom"), "thirdProjectIdentifier").unwrap();
    std::fs::write(root.join("ignored.custom"), "ignoredIdentifier").unwrap();
    std::fs::write(root.join("binary"), b"binaryIdentifier\0").unwrap();
    std::fs::create_dir(root.join("target")).unwrap();
    std::fs::write(root.join("target/generated"), "generatedIdentifier").unwrap();
    let index = CompletionIndex::new_for_test(root, cache);
    settle(&index, 0);
    index.configure(IndexSettings {
        project_wide: true,
        paused: false,
        max_files: 2,
        speed: IndexSpeed::Fast,
        ..IndexSettings::default()
    });
    settle(&index, 2);
    assert_eq!(index.status().phase, IndexPhase::Limited);
    assert!(index.query("ignored", "").is_empty());
    assert!(index.query("generated", "").is_empty());
    assert!(index.query("binary", "").is_empty());
}

#[test]
fn project_watcher_ignores_new_excluded_files_but_admits_new_source_and_explicitly_opened_files() {
    let (_temporary, root, cache) = fixture();
    std::fs::write(root.join(".gitignore"), "ignored.custom\n").unwrap();
    std::fs::write(root.join("existing.custom"), "existingIdentifier").unwrap();
    let index = CompletionIndex::new_for_test(root.clone(), cache);
    index.configure(IndexSettings {
        project_wide: true,
        paused: false,
        speed: IndexSpeed::Fast,
        ..IndexSettings::default()
    });
    settle(&index, 1);
    let ignored = root.join("ignored.custom");
    let normal = root.join("new.custom");
    let hidden = root.join(".hidden.custom");
    std::fs::write(&ignored, "ignoredWatcherIdentifier").unwrap();
    std::fs::write(&normal, "newWatcherIdentifier").unwrap();
    std::fs::write(&hidden, "hiddenWatcherIdentifier").unwrap();
    index.changed(ignored.clone());
    index.changed(hidden);
    index.changed(normal);
    settle(&index, 2);
    assert!(index.query("ignoredWatcher", "").is_empty());
    assert!(index.query("hiddenWatcher", "").is_empty());
    assert_eq!(
        index.query("newWatcher", "")[0].word,
        "newWatcherIdentifier"
    );
    index.observe(ignored, ropey::Rope::from_str("ignoredWatcherIdentifier"));
    settle(&index, 3);
    assert!(!index.query("ignoredWatcher", "").is_empty());
}

#[test]
fn project_rescan_revalidates_known_deleted_files_and_preserves_dirty_overlays() {
    let (_temporary, root, cache) = fixture();
    let deleted = root.join("deleted.custom");
    let dirty = root.join("dirty.custom");
    std::fs::write(&deleted, "deletedIdentifier").unwrap();
    std::fs::write(&dirty, "savedIdentifier").unwrap();
    let index = CompletionIndex::new_for_test(root, cache);
    index.configure(IndexSettings {
        project_wide: true,
        paused: false,
        speed: IndexSpeed::Fast,
        ..IndexSettings::default()
    });
    settle(&index, 2);
    index.observe(dirty, ropey::Rope::from_str("unsavedIdentifier"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while index.query("unsaved", "").is_empty() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    std::fs::remove_file(&deleted).unwrap();
    index.rescan();
    settle(&index, 1);
    assert!(index.query("deleted", "").is_empty());
    assert!(!index.query("unsaved", "").is_empty());
    assert!(index.query("saved", "").is_empty());
}

#[test]
fn pause_cancel_disable_and_project_preferences_persist() {
    let (_temporary, root, cache) = fixture();
    let path = root.join("source");
    std::fs::write(&path, "pausedIdentifier").unwrap();
    let index = CompletionIndex::new_for_test(root.clone(), cache.clone());
    settle(&index, 0);
    let mut settings = IndexSettings {
        paused: true,
        speed: IndexSpeed::Low,
        ..IndexSettings::default()
    };
    index.configure(settings.clone());
    wait(&index, |status| status.phase == IndexPhase::Paused);
    index.observe(path.clone(), ropey::Rope::from_str("pausedIdentifier"));
    std::thread::sleep(Duration::from_millis(50));
    assert!(index.query("paused", "").is_empty());
    settings.paused = false;
    index.configure(settings.clone());
    settle(&index, 1);
    index.cancel();
    wait(&index, |status| status.phase == IndexPhase::Cancelled);
    assert!(!index.query("paused", "").is_empty());
    settings.enabled = false;
    index.configure(settings.clone());
    wait(&index, |status| status.phase == IndexPhase::Disabled);
    assert!(index.query("paused", "").is_empty());
    drop(index);
    let reopened = CompletionIndex::new_for_test(root, cache);
    wait(&reopened, |status| status.phase == IndexPhase::Disabled);
    assert_eq!(reopened.settings(), settings);
}

#[test]
fn changed_subdirectory_removes_deleted_contributions_and_unchanged_does_not_duplicate() {
    let (_temporary, root, cache) = fixture();
    let folder = root.join("nested");
    std::fs::create_dir(&folder).unwrap();
    let path = folder.join("file");
    std::fs::write(&path, "nestedIdentifier nestedIdentifier").unwrap();
    let index = CompletionIndex::new_for_test(root, cache);
    index.observe(
        path.clone(),
        ropey::Rope::from_str("nestedIdentifier nestedIdentifier"),
    );
    settle(&index, 1);
    index.changed(path.clone());
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(index.query("nested", "")[0].frequency, 2);
    std::fs::remove_file(&path).unwrap();
    std::fs::remove_dir(&folder).unwrap();
    index.changed(folder);
    settle(&index, 0);
    assert!(index.query("nested", "").is_empty());
}

#[test]
fn queue_is_bounded_and_reports_backpressure_for_retry() {
    let (_temporary, root, cache) = fixture();
    let index = CompletionIndex::new_for_test(root.clone(), cache);
    index.configure(IndexSettings {
        paused: true,
        ..IndexSettings::default()
    });
    wait(&index, |status| status.phase == IndexPhase::Paused);
    for n in 0..MAX_PENDING_FILES {
        assert!(index.observe(
            root.join(format!("{n}")),
            ropey::Rope::from_str("queuedIdentifier")
        ));
    }
    assert!(!index.observe(
        root.join("overflow"),
        ropey::Rope::from_str("queuedIdentifier")
    ));
    index.cancel();
    assert!(index.observe(
        root.join("retry"),
        ropey::Rope::from_str("queuedIdentifier")
    ));
}

#[test]
fn cooldown_scales_with_actual_cpu_work_and_cache_records_are_bounded() {
    assert!(
        IndexSpeed::Balanced.cooldown(Duration::from_millis(120)) >= Duration::from_millis(1080)
    );
    assert!(IndexSpeed::Low.cooldown(Duration::from_millis(120)) >= Duration::from_millis(2880));
    let mut reader = std::io::Cursor::new(vec![b'x'; 1025]);
    assert!(read_cache_line(&mut reader, 1024).is_err());
    let terms = extract("thing.singleCall(); thing.often(); thing.often(); bareIdentifier");
    let once = terms
        .iter()
        .find(|term| term.word.as_ref() == "singleCall")
        .unwrap();
    assert_eq!(once.calls, 1);
    assert_eq!(
        once.contexts[0].0,
        crate::completion_context::hash_parts(1, &["thing"])
    );
}

#[test]
fn bounded_file_reader_accepts_small_and_exact_limit_files_and_refuses_oversized_content() {
    let (_temporary, root, _cache) = fixture();
    let path = root.join("growing-source");
    std::fs::write(&path, b"smallIdentifier").unwrap();
    assert_eq!(read_bounded(&path, 1024).unwrap(), b"smallIdentifier");
    std::fs::write(&path, vec![b'x'; 1024]).unwrap();
    assert_eq!(read_bounded(&path, 1024).unwrap().len(), 1024);
    std::fs::write(&path, vec![b'x'; 8192]).unwrap();
    assert_eq!(
        read_bounded(&path, 1024).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[test]
fn context_cap_drops_only_untracked_contributions_and_removal_restores_empty_catalog() {
    let mut catalog = Catalog::default();
    let mut records = Vec::new();
    for n in 0..40 {
        records.push(catalog.add(&mut extract(&format!("receiver{n}.commonMethod();"))));
    }
    for record in records.iter().rev() {
        catalog.remove_record(record);
    }
    assert!(catalog.terms.is_empty());
    assert!(catalog.buckets.is_empty());
    assert!(catalog.by_context.is_empty());
    assert_eq!(catalog.memory_bytes, 0);
}

#[test]
fn disk_cache_size_is_independent_of_the_estimated_ram_budget() {
    use std::io::Write;
    let (_temporary, root, cache) = fixture();
    let shared = || {
        Arc::new(Shared {
            root: root.clone(),
            cache_dir: cache.clone(),
            buffer_only: false,
            control: Mutex::new(Control::default()),
            available: Condvar::new(),
            catalog: RwLock::new(Catalog::default()),
            known: RwLock::new(BTreeSet::new()),
            status: Mutex::new(IndexStatus::default()),
            wake: Mutex::new(crate::background::UiWake::default()),
            revision: AtomicU64::new(0),
            active_file: AtomicU64::new(0),
        })
    };
    let settings = IndexSettings {
        max_memory_mb: 4,
        ..Default::default()
    };
    let mut seed = Worker::new(shared());
    for n in 0..4 {
        let path = root.join(format!("source{n}.custom"));
        let text = format!("learnedIdentifier{n}");
        std::fs::write(&path, &text).unwrap();
        seed.install(
            FileRecord {
                path: path.clone(),
                stamp: Stamp::read(&path),
                hash: hash(text.as_bytes()),
                terms: extract(&text).into(),
                examples: Arc::default(),
                dirty: false,
                memory_bytes: 0,
                reducible_bytes: 0,
            },
            &settings,
        );
    }
    seed.save();
    let path = cache.join("completion-index.jsonl");
    let original = std::fs::read_to_string(&path).unwrap();
    let mut file = std::fs::File::create(&path).unwrap();
    for (n, line) in original.lines().enumerate() {
        // Each valid JSON record remains below the existing 2 MiB read bound.
        if n > 0 {
            file.write_all(&vec![b' '; 1_100_000]).unwrap();
        }
        writeln!(file, "{line}").unwrap();
    }
    drop(file);
    assert!(std::fs::metadata(&path).unwrap().len() > 4 * 1024 * 1024);
    let mut restored = Worker::new(shared());
    restored.restore_cache(&settings);
    assert!(
        restored.restore.is_some(),
        "serialized disk bytes are not retained RAM"
    );
    while restored.restore.is_some() {
        restored.validate_next(&settings);
    }
    assert_eq!(restored.files.len(), 4);
    restored.publish(IndexPhase::Idle, None, true);
    assert!(restored.shared.status.lock().unwrap().memory_bytes < 4 * 1024 * 1024);
}

#[test]
fn interrupted_warm_restore_preserves_the_complete_checkpoint() {
    let (_temporary, root, cache) = fixture();
    let shared = || {
        Arc::new(Shared {
            root: root.clone(),
            cache_dir: cache.clone(),
            buffer_only: false,
            control: Mutex::new(Control::default()),
            available: Condvar::new(),
            catalog: RwLock::new(Catalog::default()),
            known: RwLock::new(BTreeSet::new()),
            status: Mutex::new(IndexStatus::default()),
            wake: Mutex::new(crate::background::UiWake::default()),
            revision: AtomicU64::new(0),
            active_file: AtomicU64::new(0),
        })
    };
    let settings = IndexSettings::default();
    let mut seed = Worker::new(shared());
    for file in 0..8 {
        let path = root.join(format!("file{file}"));
        let text = format!("persistedIdentifier{file}");
        std::fs::write(&path, &text).unwrap();
        seed.install(
            FileRecord {
                path: path.clone(),
                stamp: Stamp::read(&path),
                hash: hash(text.as_bytes()),
                terms: extract(&text).into(),
                examples: Arc::default(),

                dirty: false,
                memory_bytes: 0,
                reducible_bytes: 0,
            },
            &settings,
        );
    }
    seed.save();
    let path = cache.join("completion-index.jsonl");
    let complete = std::fs::read(&path).unwrap();
    let mut restored = Worker::new(shared());
    restored.restore();
    assert!(restored.validate_next(&settings));
    assert_eq!(restored.files.len(), 1);
    restored.restore = None; // cancel/disable/close during the paced restore
    restored.save();
    assert_eq!(std::fs::read(&path).unwrap(), complete);
    restored.restore_cache(&settings);
    while restored.restore.is_some() {
        restored.validate_next(&settings);
    }
    assert!(restored.checkpoint_complete);
    assert_eq!(restored.files.len(), 8);
}

#[test]
fn configuration_before_startup_is_persisted() {
    let (_temporary, root, cache) = fixture();
    let index = CompletionIndex::new_for_test(root, cache.clone());
    let settings = IndexSettings {
        enabled: false,
        speed: IndexSpeed::Low,
        ..IndexSettings::default()
    };
    index.configure(settings.clone());
    wait(&index, |status| status.phase == IndexPhase::Disabled);
    let bytes = std::fs::read(cache.join("completion-settings.json")).unwrap();
    assert_eq!(
        serde_json::from_slice::<IndexSettings>(&bytes).unwrap(),
        settings
    );
}

#[test]
#[ignore = "vocabulary-only microbenchmark; excludes ordered examples and is not project capacity evidence"]
fn benchmark_ten_thousand_file_catalog() {
    let (_temporary, root, cache) = fixture();
    let index = CompletionIndex::new_for_test(root.clone(), cache);
    settle(&index, 0);
    // Build records directly to measure retained structures independently of
    // disk speed and the intentional per-file background cooldown.
    let handle = index.handle.as_ref().unwrap();
    let mut worker = Worker::new(Arc::clone(&handle.0));
    let settings = IndexSettings::default();
    let started = Instant::now();
    for file in 0..10_000 {
        let text = format!(
            "let localResult{file} = socket.sendPacket(request); socket.sendPacket(request);\nif enabled {{ logger.writeMessage(localResult{file}); }}\nreturn localResult{file};"
        );
        worker.install(
            FileRecord {
                path: root.join(format!("module_{file}.unknown")),
                stamp: None,
                hash: hash(text.as_bytes()),
                terms: extract(&text).into(),
                examples: Arc::default(),

                dirty: true,
                memory_bytes: 0,
                reducible_bytes: 0,
            },
            &settings,
        );
    }
    assert_eq!(
        worker.files.len(),
        10_000,
        "dictionary-only admission should fit 10k tiny synthetic records"
    );
    let estimated = worker.memory + handle.0.catalog.read().unwrap().memory_bytes;
    assert!(estimated <= settings.max_memory_mb * 1024 * 1024);
    let build = started.elapsed();
    let started = Instant::now();
    for _ in 0..1000 {
        let candidates = std::hint::black_box(index.query(std::hint::black_box("sP"), "socket."));
        assert_eq!(candidates[0].word, "sendPacket");
    }
    let query = started.elapsed() / 1000;
    let source =
        crate::completion_source::Source::new(Some(&root.join("module_9999.unknown")), "Unknown");
    let context = crate::completion_context::Context::default();
    let history = BTreeMap::new();
    let started = Instant::now();
    for _ in 0..250 {
        let candidates = index.current_vocabulary(source.file, "local");
        assert_eq!(
            candidates[0].0, "localResult9999",
            "own file must survive broad project prefix limits"
        );
    }
    let typed = started.elapsed() / 250;
    let started = Instant::now();
    for _ in 0..1000 {
        let candidates = index.query_for_invocation("local", "", context, &history, false);
        assert!(!candidates.is_empty());
    }
    let automatic = started.elapsed() / 1000;
    eprintln!(
        "10k dictionary-only records (no learned examples): estimate {:.2}MiB, build {build:?}, context fuzzy query {query:?}, explicit broad-prefix query {typed:?}, automatic broad-prefix query {automatic:?}",
        estimated as f64 / 1024.0 / 1024.0
    );
}

#[test]
fn structural_history_is_persisted_even_without_project_wide_indexing() {
    let (_temporary, root, cache) = fixture();
    let index = CompletionIndex::new_for_test(root.clone(), cache.clone());
    settle(&index, 0);
    let mut context = crate::completion_context::Context::default();
    context.keys[0] = crate::completion_context::hash_parts(1, &["client"]);
    index.record_choice("sendPacket", "sp", context, true);
    assert!(
        index
            .history_scores("sp", context, true)
            .contains_key("sendPacket")
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !cache.join("completion-history.json").exists() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(index);
    let restored = CompletionIndex::new_for_test(root, cache);
    settle(&restored, 0);
    assert!(
        restored
            .history_scores("sp", context, true)
            .contains_key("sendPacket")
    );
    let mut settings = restored.settings();
    settings.use_history = false;
    restored.configure(settings);
    assert!(restored.history_scores("sp", context, true).is_empty());
}

#[test]
fn rare_initials_are_retrieved_beyond_broad_prefix_and_alphabetical_caps() {
    let (_temporary, root, cache) = fixture();
    let index = CompletionIndex::new_for_test(root, cache);
    settle(&index, 0);
    let handle = index.handle.as_ref().unwrap();
    let mut catalog = handle.0.catalog.write().unwrap();
    for group in 0..3 {
        let source: String = (0..2000)
            .map(|n| format!("getThing{} ", group * 2000 + n))
            .collect();
        catalog.add(&mut extract(&source));
    }
    catalog.add(&mut extract("get_user_config HTTPClient"));
    drop(catalog);
    assert_eq!(index.query("guc", "")[0].word, "get_user_config");
    assert_eq!(index.query("hc", "")[0].word, "HTTPClient");
}

#[test]
fn project_call_evidence_is_specific_to_the_receiver_or_assignment() {
    let (_temporary, root, cache) = fixture();
    let index = CompletionIndex::new_for_test(root.clone(), cache);
    index.observe(root.join("calls"), ropey::Rope::from_str("worker.commit(); worker.commit(); config.commit = true; config.commit = false;\nhandler = commit; handler = commit;"));
    settle(&index, 1);
    let method = &index.query("co", "worker.")[0];
    assert!(method.callable);
    let field = &index.query("co", "config.")[0];
    assert!(!field.callable);
    assert_eq!(field.context_calls, 0);
    assert!(field.context_uses >= 2);
    assert!(!index.query("co", "handler = ")[0].callable);
    index.release(root.join("calls"));
    settle(&index, 0);
    assert!(
        index
            .handle
            .as_ref()
            .unwrap()
            .0
            .catalog
            .read()
            .unwrap()
            .terms
            .is_empty()
    );
}

#[test]
fn incompatible_cache_versions_are_discarded_without_a_migration_worker() {
    let (_temporary, root, cache) = fixture();
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(
        cache.join("completion-index.jsonl"),
        format!("{}\n", serde_json::json!({"version":0,"root":root})),
    )
    .unwrap();
    let index = CompletionIndex::new_for_test(root, cache);
    settle(&index, 0);
    assert!(index.query("read", "").is_empty());
}
