//! Matching remote caches survive restarts without opening native source paths.
use super::*;

const URI: &str = "file:///srv/project/";

fn remote_index(root: &Path, cache: &Path, uri: &str) -> CompletionIndex {
    let mut settings = IndexSettings::default();
    settings.speed = IndexSpeed::Fast;
    CompletionIndex::new_with_identity(
        root.to_owned(),
        cache.to_owned(),
        settings,
        true,
        Some(uri.into()),
    )
}

fn contains(index: &CompletionIndex, word: &str) -> bool {
    index
        .query(&word[..word.len() - 1], "")
        .iter()
        .any(|entry| entry.word == word)
}

fn paused(index: &CompletionIndex, files: usize) {
    wait(index, |s| {
        s.indexed_files == files
            && s.pending_files == 0
            && s.phase == IndexPhase::Paused
            && s.settings == index.settings()
    });
}

#[test]
fn remote_cache_restart_restores_saved_learning_and_omits_unsaved_buffers() {
    let (_temp, base, cache) = fixture();
    let root = base.join("\0remote");
    let index = remote_index(&root, &cache, URI);
    for (name, word, dirty) in [
        ("first.rs", "firstCachedRemoteIdentifier", false),
        ("second.rs", "secondCachedRemoteIdentifier", false),
        ("dirty.rs", "unsavedRemoteIdentifier", true),
    ] {
        index.observe_buffer(root.join(name), ropey::Rope::from_str(word), dirty);
    }
    settle(&index, 3);
    let data = std::fs::read_to_string(cache.join("completion-index.jsonl")).unwrap();
    let header: CacheHeader = serde_json::from_str(data.lines().next().unwrap()).unwrap();
    assert_eq!(header.root, root);
    assert_eq!(header.remote_root_uri.as_deref(), Some(URI));
    assert_eq!(data.lines().count(), 3);
    assert!(!data.contains("unsavedRemoteIdentifier"));
    drop(index);
    let restored = remote_index(&root, &cache, URI);
    settle(&restored, 2);
    assert!(contains(&restored, "firstCachedRemoteIdentifier"));
    assert!(contains(&restored, "secondCachedRemoteIdentifier"));
    assert!(!contains(&restored, "unsavedRemoteIdentifier"));
    assert!(restored.status().error.is_none());
}

#[test]
fn remote_cache_requires_both_connection_identity_and_the_headless_root_uri() {
    let (_temp, base, cache) = fixture();
    let root = base.join("\0remote-one");
    let index = remote_index(&root, &cache, URI);
    index.observe_buffer(
        root.join("source.rs"),
        ropey::Rope::from_str("scopedRemoteIdentifier"),
        false,
    );
    settle(&index, 1);
    drop(index);
    for (root, uri) in [
        (root.clone(), "file:///srv/different-project/"),
        (base.join("\0remote-other-connection"), URI),
    ] {
        let mismatched = remote_index(&root, &cache, uri);
        settle(&mismatched, 0);
        assert!(!contains(&mismatched, "scopedRemoteIdentifier"));
    }
    let matched = remote_index(&root, &cache, URI);
    settle(&matched, 1);
    assert!(contains(&matched, "scopedRemoteIdentifier"));
}

#[test]
fn remote_cache_loads_while_paused_and_removal_survives_paused_and_off_restarts() {
    let (_temp, base, cache) = fixture();
    let root = base.join("\0remote");
    let index = remote_index(&root, &cache, URI);
    for (name, word) in [
        ("remove.rs", "removedRemoteIdentifier"),
        ("keep.rs", "retainedRemoteIdentifier"),
    ] {
        index.observe_buffer(root.join(name), ropey::Rope::from_str(word), false);
    }
    settle(&index, 2);
    let mut settings = index.settings();
    settings.paused = true;
    index.configure(settings);
    paused(&index, 2);
    drop(index);
    let restored = remote_index(&root, &cache, URI);
    paused(&restored, 2);
    assert!(contains(&restored, "retainedRemoteIdentifier"));
    assert!(restored.remove_file(&root.join("remove.rs")));
    paused(&restored, 1);
    drop(restored);
    let restored = remote_index(&root, &cache, URI);
    paused(&restored, 1);
    assert!(!contains(&restored, "removedRemoteIdentifier"));
    let mut settings = restored.settings();
    settings.enabled = false;
    restored.configure(settings);
    wait(&restored, |s| s.phase == IndexPhase::Disabled);
    assert!(restored.remove_file(&root.join("keep.rs")));
    wait(&restored, |s| {
        s.phase == IndexPhase::Disabled && s.indexed_files == 0 && s.pending_files == 0
    });
    drop(restored);
    let restored = remote_index(&root, &cache, URI);
    let mut settings = restored.settings();
    settings.enabled = true;
    settings.paused = false;
    restored.configure(settings);
    settle(&restored, 0);
    assert!(!contains(&restored, "retainedRemoteIdentifier"));
}

#[test]
fn remote_cache_cannot_overwrite_a_newly_loaded_buffer_during_restore() {
    let (_temp, base, cache) = fixture();
    let root = base.join("\0remote");
    let path = root.join("source.rs");
    let index = remote_index(&root, &cache, URI);
    index.observe_buffer(
        path.clone(),
        ropey::Rope::from_str("previousRemoteIdentifier"),
        false,
    );
    settle(&index, 1);
    drop(index);
    let restored = remote_index(&root, &cache, URI);
    restored.observe_active_buffer(
        path,
        ropey::Rope::from_str("currentRemoteIdentifier"),
        false,
    );
    wait(&restored, |_| {
        contains(&restored, "currentRemoteIdentifier")
    });
    settle(&restored, 1);
    assert!(!contains(&restored, "previousRemoteIdentifier"));
}

#[test]
fn remote_cache_migrates_existing_learning_preferences_into_the_remote_directory() {
    let (_temp, base, cache) = fixture();
    let root = base.join("\0remote-preference-migration");
    let previous = crate::settings::project_settings_path(&root)
        .parent()
        .unwrap()
        .join("completion-settings.json");
    let mut settings = IndexSettings::default();
    settings.paused = true;
    settings.speed = IndexSpeed::Low;
    settings.max_memory_mb = 16;
    write_json(&previous, &settings).unwrap();
    let index = remote_index(&root, &cache, URI);
    wait(&index, |s| {
        s.phase == IndexPhase::Paused && cache.join("completion-settings.json").exists()
    });
    assert_eq!(index.settings(), settings);
    let saved: IndexSettings =
        serde_json::from_slice(&std::fs::read(cache.join("completion-settings.json")).unwrap())
            .unwrap();
    assert_eq!(saved, settings);
    std::fs::remove_file(previous).unwrap();
}
