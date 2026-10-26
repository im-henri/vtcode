use super::*;

fn paths(names: &[&str]) -> BTreeSet<PathBuf> {
    names.iter().map(|name| PathBuf::from(*name)).collect()
}

#[test]
fn live_selection_handles_directories_root_fallback_and_excluded_paths() {
    let mut f = Fixture::new();
    fs::create_dir(f.root.path().join("folder")).unwrap();
    f.write("folder/a", "before\n");
    f.write("b", "before\n");
    let cp = f.begin();
    f.write("folder/a", "after\n");
    f.write("b", "after\n");
    fs::create_dir(f.root.path().join("nested")).unwrap();
    git2::Repository::init(f.root.path().join("nested")).unwrap();
    f.write("nested/foreign", "outside capture coverage\n");
    f.journal
        .capture_checkpoint(cp, BTreeSet::new(), &paths(&["nested", ".git"]))
        .unwrap();
    assert!(f.journal.state.edits.is_empty());
    let directory = if cfg!(windows) { "FOLDER" } else { "folder" };
    f.journal
        .capture_checkpoint(cp, BTreeSet::new(), &paths(&[directory]))
        .unwrap();
    assert_eq!(f.journal.state.edits.len(), 1);
    assert_eq!(f.journal.state.edits[0].path, Path::new("folder/a"));
    assert!(
        f.journal
            .capture_checkpoint(cp, BTreeSet::new(), &paths(&["../outside"]))
            .is_err()
    );
    f.journal
        .capture_checkpoint(cp, BTreeSet::new(), &paths(&[""]))
        .unwrap();
    assert_eq!(f.journal.state.edits.len(), 2);
    f.finish();
    assert_eq!(f.journal.state.edits.len(), 2);
}

#[test]
fn live_captures_are_reviewable_and_acceptable_before_finish_without_duplicate_edits() {
    let mut f = Fixture::new();
    f.write("a", "first\nkeep\nlast\n");
    f.write("b", "other before\n");
    let cp = f.begin();
    f.write("a", "FIRST\nkeep\nlast\n");
    f.write("b", "other after\n");
    f.journal
        .capture_checkpoint(cp, BTreeSet::new(), &paths(&["a"]))
        .unwrap();
    let summary = f.journal.summary("agent", "chat");
    assert_eq!(summary.active, Some(cp));
    assert_eq!(summary.edits.len(), 1);
    assert_eq!(summary.edits[0].path, Path::new("a"));
    let first = summary.pending_chunks(&Selection::All).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(
        f.journal.inline_reviews()[0].text.as_ref(),
        "FIRST\nkeep\nlast\n"
    );
    f.journal.accept_chunks(&first).unwrap();
    assert_eq!(f.journal.active(), Some(cp));
    let version = f.journal.state.version;
    f.journal
        .capture_checkpoint(cp, BTreeSet::new(), &paths(&["a"]))
        .unwrap();
    assert_eq!(
        f.journal.state.version, version,
        "unchanged notifications are a no-op"
    );

    f.write("a", "FIRST\nkeep\nLAST\n");
    f.journal
        .capture_checkpoint(cp, BTreeSet::new(), &paths(&["a"]))
        .unwrap();
    let summary = f.journal.summary("agent", "chat");
    assert_eq!(summary.edits.len(), 2);
    assert_eq!(summary.edits[0].id, first[0].0);
    assert_eq!(summary.edits[0].chunks[0].decision, Decision::Accepted);
    assert_eq!(summary.pending_chunks(&Selection::All).unwrap().len(), 1);
    assert!(f.journal.plan("agent", "chat", &Selection::All).is_err());
    f.finish();
    let summary = f.journal.summary("agent", "chat");
    assert_eq!(summary.active, None);
    assert_eq!(
        summary.edits.len(),
        3,
        "only the unobserved b change is added at finish"
    );
    f.decline(Selection::All).unwrap();
    assert_eq!(f.read("a"), "FIRST\nkeep\nlast\n");
    assert_eq!(f.read("b"), "other before\n");
    f.decline(Selection::Checkpoint(cp)).unwrap();
    assert_eq!(f.read("a"), "first\nkeep\nlast\n");
}

#[test]
fn live_created_deleted_and_recreated_files_keep_pinned_history_through_gc_and_restart() {
    let mut f = Fixture::new();
    f.write(".gitignore", "# initially empty\n");
    f.write("a", "original\n");
    let cp = f.begin();
    fs::remove_file(f.root.path().join("a")).unwrap();
    f.journal
        .capture_checkpoint(cp, BTreeSet::new(), &paths(&["a"]))
        .unwrap();
    assert!(f.journal.state.edits[0].after.is_none());
    f.write(".gitignore", "*\n");
    f.write("a", "recreated\n");
    f.write("ignored-new", "outside coverage\n");
    f.journal
        .capture_checkpoint(cp, BTreeSet::new(), &paths(&["a", "ignored-new"]))
        .unwrap();
    assert_eq!(f.journal.state.edits.len(), 2);
    assert!(f.journal.state.edits[1].before.is_none());
    let created_blob = f.journal.state.edits[1]
        .after
        .as_ref()
        .unwrap()
        .blob
        .clone();
    fs::remove_file(f.root.path().join("a")).unwrap();
    f.journal
        .capture_checkpoint(cp, BTreeSet::new(), &paths(&["a"]))
        .unwrap();
    let Fixture {
        root,
        data,
        journal,
    } = f;
    drop(journal);
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root.path())
        .arg("-c")
        .arg(format!("safe.directory={}", root.path().display()))
        .args(["gc", "--prune=now"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut journal = Journal::open(root.path(), data.path()).unwrap();
    assert_eq!(journal.active(), Some(cp));
    assert_eq!(
        storage::bytes(&journal.repository, &created_blob).unwrap(),
        b"recreated\n"
    );
    journal.finish_uncertain().unwrap();
    assert!(
        journal.state.edits.iter().all(|e| e.conflict.is_some()),
        "crash recovery protects earlier live edits too"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("ignored-new")).unwrap(),
        "outside coverage\n"
    );
}

#[test]
fn later_protection_and_concurrent_changes_also_flag_earlier_live_chunks() {
    for concurrent in [false, true] {
        let mut f = Fixture::new();
        f.write("a", "before\n");
        let first = f.begin();
        f.write("a", "first write\n");
        f.journal
            .capture_checkpoint(first, BTreeSet::new(), &paths(&["a"]))
            .unwrap();
        let chunks = f
            .journal
            .summary("agent", "chat")
            .pending_chunks(&Selection::All)
            .unwrap();
        f.journal.accept_chunks(&chunks).unwrap();
        if concurrent {
            let other = f
                .journal
                .begin("other", "chat", "concurrent", BTreeSet::new())
                .unwrap();
            f.write("a", "later shared write\n");
            f.journal
                .capture_checkpoint(other, BTreeSet::new(), &paths(&["a"]))
                .unwrap();
            f.journal.finish_checkpoint(other, BTreeSet::new()).unwrap();
        } else {
            f.journal
                .capture_checkpoint(first, paths(&["a"]), &paths(&["a"]))
                .unwrap();
        }
        assert!(f.journal.state.edits[0].conflict.is_some());
        assert_eq!(
            f.journal.state.edits[0].chunks[0].decision,
            Decision::Accepted
        );
        f.finish();
        let before = f.read("a");
        assert!(f.decline(Selection::Checkpoint(first)).is_err());
        assert_eq!(f.read("a"), before);
    }
}
