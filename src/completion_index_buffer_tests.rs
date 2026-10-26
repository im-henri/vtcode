//! Buffer-only learning must never reinterpret a remote identity as a disk path.
use super::*;

fn has(index: &CompletionIndex, word: &str) -> bool {
    index
        .query(&word[..word.len() - 1], "")
        .iter()
        .any(|entry| entry.word == word)
}

#[test]
fn remote_buffer_index_uses_loaded_text_and_persists_only_learned_data_without_workspace_scans() {
    let (_temp, root, cache) = fixture();
    let source = root.join("opened.rs");
    std::fs::write(&source, "nativeDiskOnlyIdentifier").unwrap();
    std::fs::write(root.join("unopened.rs"), "unopenedSecretIdentifier").unwrap();
    std::fs::create_dir(&cache).unwrap();
    let checkpoint = cache.join("completion-index.jsonl");
    std::fs::write(&checkpoint, "must not read or rewrite this checkpoint").unwrap();
    let history = cache.join("completion-history.json");
    std::fs::write(&history, "must not read or rewrite this history").unwrap();
    let index =
        CompletionIndex::new_with_mode(root.clone(), cache.clone(), IndexSettings::default(), true);
    let mut settings = index.settings();
    settings.project_wide = true;
    settings.speed = IndexSpeed::Fast;
    index.configure(settings);
    assert!(!index.settings().project_wide);
    index.rescan();
    index.changed(root.clone());
    assert!(index.observe_buffer(
        source.clone(),
        ropey::Rope::from_str("loadedRemoteIdentifier"),
        false
    ));
    settle(&index, 1);
    assert!(has(&index, "loadedRemoteIdentifier"));
    assert!(!has(&index, "nativeDiskOnlyIdentifier"));
    assert!(!has(&index, "unopenedSecretIdentifier"));
    assert!(index.status().error.is_none());
    index.release(source.clone());
    index.rescan();
    wait(&index, |s| {
        s.phase == IndexPhase::Idle && s.pending_files == 0
    });
    assert!(
        has(&index, "loadedRemoteIdentifier"),
        "clean closed files remain learned"
    );
    assert!(index.remove_file(&source));
    settle(&index, 0);
    assert_eq!(
        std::fs::read_to_string(source).unwrap(),
        "nativeDiskOnlyIdentifier"
    );
    let checkpoint = std::fs::read_to_string(checkpoint).unwrap();
    assert_eq!(
        checkpoint.lines().count(),
        1,
        "removal must persist in the local checkpoint"
    );
    let header: CacheHeader = serde_json::from_str(checkpoint.lines().next().unwrap()).unwrap();
    assert_eq!(header.root, root);
    assert_eq!(
        std::fs::read_to_string(history).unwrap(),
        "must not read or rewrite this history"
    );
    wait(&index, |_| cache.join("completion-settings.json").is_file());
    let saved: IndexSettings =
        serde_json::from_slice(&std::fs::read(cache.join("completion-settings.json")).unwrap())
            .unwrap();
    assert!(!saved.project_wide);
}

#[test]
fn remote_buffer_close_keeps_queued_saved_text_and_discards_unsaved_text() {
    let (_temp, root, cache) = fixture();
    let index = CompletionIndex::new_with_mode(root.clone(), cache, IndexSettings::default(), true);
    let activity = crate::completion_activity::Activity::default();
    index.set_activity(activity.clone());
    let foreground = activity.enter();
    let saved = root.join("saved.rs");
    let dirty = root.join("dirty.rs");
    index.observe_buffer(
        saved.clone(),
        ropey::Rope::from_str("savedRemoteIdentifier"),
        false,
    );
    index.observe_buffer(
        dirty.clone(),
        ropey::Rope::from_str("discardedRemoteIdentifier"),
        true,
    );
    index.release(saved);
    index.release(dirty);
    drop(foreground);
    settle(&index, 1);
    assert!(has(&index, "savedRemoteIdentifier"));
    assert!(!has(&index, "discardedRemoteIdentifier"));
}

#[test]
fn remote_clean_buffer_scan_never_validates_against_a_native_file() {
    let (_temp, root, _) = fixture();
    let path = root.join("remote.rs");
    std::fs::write(&path, "nativeBeforeIdentifier").unwrap();
    let source = "loadedRemoteIdentifier\n".repeat(stream::CHUNK_BYTES / 8);
    let mut scan = stream::Scan::buffer(
        path.clone(),
        ropey::Rope::from_str(&source),
        source.len(),
        false,
    );
    assert!(!scan.step().unwrap());
    std::fs::write(path, "nativeAfterIdentifier").unwrap();
    while !scan.step().unwrap() {}
    let record = scan.finish();
    assert!(!record.dirty);
    assert_eq!(record.stamp.as_ref().unwrap().bytes, source.len() as u64);
    assert!(
        record
            .terms
            .iter()
            .any(|term| term.word.as_ref() == "loadedRemoteIdentifier")
    );
}

#[test]
fn remote_buffer_learning_preserves_background_files_through_focus_changes_and_size_limits() {
    let (_temp, root, cache) = fixture();
    let mut settings = IndexSettings::default();
    settings.speed = IndexSpeed::Fast;
    let index = CompletionIndex::new_with_mode(root.clone(), cache, settings, true);
    let large = |word: &str| ropey::Rope::from_str(&format!("{word}\n").repeat(120_000));
    let first = root.join("first.rs");
    index.observe_active_buffer(first.clone(), large("firstRemoteIdentifier"), false);
    wait(&index, |s| s.current_file.as_ref() == Some(&first));
    let second = root.join("second.rs");
    index.observe_active_buffer(second.clone(), large("secondRemoteIdentifier"), false);
    wait(&index, |s| s.current_file.as_ref() == Some(&second));
    index.observe_active_buffer(
        root.join("third.rs"),
        ropey::Rope::from_str("thirdRemoteIdentifier"),
        false,
    );
    settle(&index, 3);
    for word in [
        "firstRemoteIdentifier",
        "secondRemoteIdentifier",
        "thirdRemoteIdentifier",
    ] {
        assert!(has(&index, word), "focus must not discard {word}");
    }
    let mut settings = index.settings();
    settings.max_file_bytes = 1024 * 1024;
    index.configure(settings);
    settle(&index, 1);
    assert!(has(&index, "thirdRemoteIdentifier"));
    assert!(!has(&index, "firstRemoteIdentifier"));
    assert!(!has(&index, "secondRemoteIdentifier"));
}

#[test]
fn remote_buffer_keys_preserve_case_and_reject_other_workspace_identities() {
    let (_temp, base, cache) = fixture();
    let root = base.join("\0remote-one");
    let other = base.join("\0remote-two");
    let first = root.join("File.rs");
    let second = root.join("file.rs");
    assert!(!same_index_file_path(&first, &second));
    let index = CompletionIndex::new_with_mode(root, cache, IndexSettings::default(), true);
    index.observe_buffer(
        first.clone(),
        ropey::Rope::from_str("upperCaseRemoteIdentifier"),
        false,
    );
    index.observe_buffer(
        second,
        ropey::Rope::from_str("lowerCaseRemoteIdentifier"),
        false,
    );
    index.observe_buffer(
        other.join("file.rs"),
        ropey::Rope::from_str("otherRemoteIdentifier"),
        false,
    );
    settle(&index, 2);
    assert!(!has(&index, "otherRemoteIdentifier"));
    assert!(index.remove_file(&first));
    settle(&index, 1);
    assert!(!has(&index, "upperCaseRemoteIdentifier"));
    assert!(has(&index, "lowerCaseRemoteIdentifier"));
    assert!(index.status().error.is_none());
}
