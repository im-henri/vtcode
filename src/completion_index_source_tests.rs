use super::*;

#[test]
fn every_extension_and_independent_directory_contributes_vocabulary() {
    let (_temporary, root, _) = fixture();
    let inputs = [
        (root.join("core/main.c"), "readCurrent"),
        (root.join("other/main.h"), "readOther"),
        (root.join("script.custom_syntax"), "readCustom"),
        (root.join("no_extension"), "readUnclassified"),
    ];
    let (index, _) = CompletionIndex::build_for_benchmark(root, inputs);
    let words = index.query("read", "");
    for expected in ["readCurrent", "readOther", "readCustom", "readUnclassified"] {
        assert!(words.iter().any(|word| word.word == expected), "{expected}");
    }
}

#[test]
fn file_names_and_extensions_cannot_change_vocabulary_order() {
    let (_temporary, root, _) = fixture();
    let text = [
        "socket.readHeader(); readHeader();",
        "socket.readPacket(); readPacket();",
    ];
    let build = |names: [&str; 2]| {
        CompletionIndex::build_for_benchmark(
            root.clone(),
            names
                .into_iter()
                .zip(text)
                .map(|(name, text)| (root.join(name), text)),
        )
        .0
    };
    let first = build(["core/peer.h", "other/program.rs"]);
    let second = build(["independent/random.custom", "peer.h"]);
    let words = |index: &CompletionIndex| {
        index
            .query("read", "socket.")
            .into_iter()
            .map(|word| (word.word, word.frequency, word.context_score))
            .collect::<Vec<_>>()
    };
    assert_eq!(words(&first), words(&second));
}

#[test]
fn active_file_vocabulary_survives_workspace_prefix_pressure() {
    let (_temporary, root, _) = fixture();
    let inputs: Vec<_> = (0..800)
        .map(|i| (root.join(format!("{i}.custom")), format!("localWord{i}")))
        .collect();
    let (index, _) = CompletionIndex::build_for_benchmark(
        root.clone(),
        inputs
            .iter()
            .map(|(path, text)| (path.clone(), text.as_str())),
    );
    let file = crate::completion_source::Source::new(Some(&root.join("799.custom")), "").file;
    assert_eq!(
        index.current_vocabulary(file, "local"),
        vec![("localWord799".into(), 1, 0)]
    );
}

#[test]
fn packed_file_record_is_shared_between_worker_and_query_catalog() {
    let (_temporary, root, cache) = fixture();
    let index = CompletionIndex::new_for_test(root.clone(), cache);
    settle(&index, 0);
    let shared = Arc::clone(&index.handle.as_ref().unwrap().0);
    let mut worker = Worker::new(Arc::clone(&shared));
    let path = root.join("shared.custom");
    worker.index_snapshot(
        path.clone(),
        ropey::Rope::from_str("readHeader readHeader"),
        &IndexSettings::default(),
    );
    while !worker.scans.is_empty() {
        worker.advance_scan(&IndexSettings::default());
    }
    let file = crate::completion_source::Source::new(Some(&path), "").file;
    assert!(Arc::ptr_eq(
        &worker.files[&path],
        &shared.catalog.read().unwrap().files[&file]
    ));
    worker.remove(&path);
    assert!(index.query("read", "").is_empty());
}
