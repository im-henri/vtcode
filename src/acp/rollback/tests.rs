use super::*;

#[path = "live_tests.rs"]
mod live_tests;
#[path = "recovery_tests.rs"]
mod recovery_tests;
#[path = "safety_tests.rs"]
mod safety_tests;

struct Fixture {
    root: tempfile::TempDir,
    data: tempfile::TempDir,
    journal: Journal,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        git2::Repository::init(root.path()).unwrap();
        let journal = Journal::open(root.path(), data.path()).unwrap();
        Self {
            root,
            data,
            journal,
        }
    }
    fn write(&self, path: &str, text: &str) {
        fs::write(self.root.path().join(path), text).unwrap();
    }
    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.root.path().join(path)).unwrap()
    }
    fn begin(&mut self) -> u64 {
        self.journal
            .begin("agent", "chat", "change files", BTreeSet::new())
            .unwrap()
    }
    fn finish(&mut self) {
        self.journal.finish(BTreeSet::new()).unwrap();
    }
    fn decline(&mut self, selection: Selection) -> Result<Vec<PathBuf>> {
        let plan = self.journal.plan("agent", "chat", &selection)?;
        self.journal.apply(plan)
    }
}

#[test]
fn requires_git_and_exclusive_journal_lock() {
    let root = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    assert!(Journal::open(root.path(), data.path()).is_err());
    let f = Fixture::new();
    assert!(Journal::open(f.root.path(), f.data.path()).is_err());
    assert!(
        Journal::open(f.root.path(), data.path()).is_err(),
        "lock is shared across portable installations"
    );
}

#[cfg(unix)]
#[test]
fn closing_journal_releases_lock_while_an_inherited_descriptor_stays_open() {
    let f = Fixture::new();
    // dup/try_clone shares the same open file description as an FD inherited
    // by a concurrently forked terminal/tool before it reaches exec.
    let inherited = f.journal._lock.0.try_clone().unwrap();
    drop(f.journal);
    let reopened = Journal::open(f.root.path(), f.data.path())
        .expect("An inherited descriptor must not prolong the closed journal's lease");
    assert!(Journal::open(f.root.path(), f.data.path()).is_err());
    drop(inherited);
    assert!(Journal::open(f.root.path(), f.data.path()).is_err(),
        "Dropping the old descriptor must not release the new owner's lock");
    drop(reopened);
    assert!(Journal::open(f.root.path(), f.data.path()).is_ok());
}

#[test]
fn capture_pin_retry_is_idempotent_and_keeps_both_observed_versions() {
    let mut f = Fixture::new();
    f.write("a", "before");
    let id = f.begin();
    f.write("a", "first attempt");
    let files = f.journal.snapshot(None).unwrap();
    f.journal.pin(id, "after", &files).unwrap();
    f.journal.pin(id, "after", &files).unwrap();
    f.write("a", "second attempt");
    f.finish();
    f.decline(Selection::All).unwrap();
    assert_eq!(f.read("a"), "before");
}

#[test]
fn user_edits_between_messages_survive_checkpoint_restore() {
    let mut f = Fixture::new();
    f.write("a", "first\nmiddle\nlast\n");
    let cp = f.begin();
    f.write("a", "FIRST\nmiddle\nlast\n");
    f.finish();
    f.write("a", "FIRST\nuser middle\nlast\n");
    f.begin();
    f.write("a", "FIRST\nuser middle\nLAST\n");
    f.finish();
    f.decline(Selection::Checkpoint(cp)).unwrap();
    assert_eq!(f.read("a"), "first\nuser middle\nlast\n");
}

#[test]
fn accepted_later_edits_and_modified_created_files_are_not_discarded() {
    let mut f = Fixture::new();
    f.write("a", "before");
    f.begin();
    f.write("a", "first");
    f.finish();
    let old_id = f.journal.summary("agent", "chat").edits[0].id;
    f.begin();
    f.write("a", "second");
    f.write("created", "agent");
    f.finish();
    let edits = f.journal.summary("agent", "chat").edits;
    let latest = edits
        .iter()
        .rev()
        .find(|e| e.path == Path::new("a"))
        .unwrap()
        .id;
    f.journal
        .accept("agent", "chat", &Selection::Edit(latest))
        .unwrap();
    assert!(f.decline(Selection::Edit(old_id)).is_err());
    assert_eq!(f.read("a"), "second");
    f.write("created", "user additions");
    assert!(
        f.decline(Selection::File(PathBuf::from("created")))
            .is_err()
    );
    assert_eq!(f.read("created"), "user additions");
}

#[test]
fn truncated_or_corrupt_history_and_objects_fail_without_touching_files() {
    let mut f = Fixture::new();
    f.write("a", "before");
    f.begin();
    f.write("a", "after");
    f.finish();
    let blob = f.journal.state.edits[0]
        .before
        .as_ref()
        .unwrap()
        .blob
        .hash
        .clone();
    let repo = git2::Repository::open(f.root.path()).unwrap();
    let object = repo
        .path()
        .join("objects")
        .join(&blob[..2])
        .join(&blob[2..]);
    // Remove only this test's immutable blob; no application file is modified.
    #[cfg(windows)]
    {
        let mut permissions = fs::metadata(&object).unwrap().permissions();
        // Windows readonly bit only; this block is never used for Unix modes.
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        fs::set_permissions(&object, permissions).unwrap();
    }
    fs::remove_file(object).unwrap();
    assert!(f.decline(Selection::All).is_err());
    assert_eq!(f.read("a"), "after");
    let Fixture {
        root,
        data,
        journal,
    } = f;
    drop(journal);
    fs::write(data.path().join("state.json"), b"{\"schema\":").unwrap();
    assert!(Journal::open(root.path(), data.path()).is_err());
    assert_eq!(fs::read_to_string(root.path().join("a")).unwrap(), "after");
}

#[test]
fn recovery_preserves_the_actual_racing_original_and_refuses_newer_work() {
    let mut f = Fixture::new();
    f.write("a", "old");
    f.begin();
    f.write("a", "agent");
    f.finish();
    let mut plan = f.journal.plan("agent", "chat", &Selection::All).unwrap();
    let backup = PathBuf::from(".vtcode-rollback-test");
    plan.replacements[0].backup = Some(backup.clone());
    let mut state = f.journal.state.clone();
    state.transaction = Some(plan);
    f.journal.save(state).unwrap();
    f.write("a", "user racing save");
    fs::rename(f.root.path().join("a"), f.root.path().join(&backup)).unwrap();
    f.write("a", "newer current work");
    assert!(f.journal.recover().is_err());
    assert_eq!(f.read("a"), "newer current work");
    assert_eq!(f.read(backup.to_str().unwrap()), "user racing save");
    fs::remove_file(f.root.path().join("a")).unwrap();
    f.journal.recover().unwrap();
    assert_eq!(f.read("a"), "user racing save");
    assert!(
        f.root.path().join(backup).exists(),
        "unexpected displaced contents remain recoverable"
    );
}

#[test]
fn read_only_files_preflight_the_whole_batch() {
    let mut f = Fixture::new();
    f.write("a", "old");
    f.write("b", "old");
    f.begin();
    f.write("a", "agent");
    f.write("b", "agent");
    f.finish();
    let path = f.root.path().join("b");
    let original = fs::metadata(&path).unwrap().permissions();
    let mut readonly = original.clone();
    readonly.set_readonly(true);
    fs::set_permissions(&path, readonly).unwrap();
    assert!(f.decline(Selection::All).is_err());
    assert_eq!(f.read("a"), "agent");
    fs::set_permissions(path, original).unwrap();
}

#[test]
fn file_scope_leaves_other_files_pending_and_handles_bom_and_no_final_newline() {
    let mut f = Fixture::new();
    f.write("a", "\u{feff}héllo\r\nlast");
    f.write("b", "old");
    f.begin();
    f.write("a", "\u{feff}HELLO\r\nlast");
    f.write("b", "new");
    f.finish();
    f.decline(Selection::File(PathBuf::from("a"))).unwrap();
    assert_eq!(f.read("a"), "\u{feff}héllo\r\nlast");
    assert_eq!(f.read("b"), "new");
    assert_eq!(
        f.journal
            .summary("agent", "chat")
            .edits
            .iter()
            .map(Edit::pending)
            .sum::<usize>(),
        1
    );
}

#[test]
fn review_markers_do_not_confuse_files_with_the_same_suffix() {
    let mut f = Fixture::new();
    f.write("a", "old");
    f.begin();
    f.write("a", "new");
    f.finish();
    let summary = f.journal.summary("agent", "chat");
    // ACP file edits use canonical paths; Windows temp paths can contain aliases.
    let root = f.journal.workspace_root();
    assert!(summary.pending_file(&root.join("a")).is_some());
    assert!(summary.pending_file(&root.join("nested/a")).is_none());
}

#[cfg(windows)]
#[test]
fn review_markers_match_windows_path_spellings_after_file_is_deleted() {
    let mut f = Fixture::new();
    f.write("a", "old");
    f.begin();
    f.write("a", "new");
    f.finish();
    let summary = f.journal.summary("agent", "chat");
    let canonical = f.journal.workspace_root().join("a");
    let ordinary = crate::acp::files::agent_path(&canonical);
    let alternate = PathBuf::from(ordinary.to_string_lossy().replace('\\', "/").to_uppercase());

    fs::remove_file(&canonical).unwrap();
    for path in [&canonical, &ordinary, &alternate] {
        assert!(summary.pending_file(path).is_some(), "{}", path.display());
        let sibling = path.parent().unwrap().join("nested/a");
        assert!(summary.pending_file(&sibling).is_none());
    }
}

#[test]
fn tracked_ignored_files_and_new_ignore_rules_do_not_hide_existing_edits() {
    let mut f = Fixture::new();
    f.write("a", "old");
    let repo = git2::Repository::open(f.root.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a")).unwrap();
    index.write().unwrap();
    f.write(".gitignore", "a\n");
    f.write("b", "old");
    f.begin();
    f.write("a", "new");
    f.write("b", "new");
    f.write(".gitignore", "a\nb\n");
    f.finish();
    f.decline(Selection::All).unwrap();
    assert_eq!(f.read("a"), "old");
    assert_eq!(f.read("b"), "old");
}

#[test]
fn repository_metadata_cannot_be_a_snapshot_root() {
    let f = Fixture::new();
    let data = tempfile::tempdir().unwrap();
    assert!(Journal::open(&f.root.path().join(".git"), data.path()).is_err());
}

#[test]
fn chunk_merge_preserves_disjoint_user_changes_across_many_positions() {
    for agent in 0..12 {
        for user in 0..12 {
            if agent == user {
                continue;
            }
            let before: Vec<_> = (0..12).map(|i| format!("line {i}\r\n")).collect();
            let mut after = before.clone();
            after[agent] = format!("agent {agent}\r\n");
            let mut current = after.clone();
            current[user] = format!("user {user}\r\n");
            let mut expected = before.clone();
            expected[user] = format!("user {user}\r\n");
            assert_eq!(
                merge::merge(&after.concat(), &before.concat(), &current.concat()).unwrap(),
                expected.concat()
            );
        }
    }
    assert!(
        merge::merge(
            "same\nsame\nend\n",
            "same\nend\n",
            "start\nsame\nsame\nend\n"
        )
        .is_err()
    );
}

#[test]
fn undo_rollback_restores_content_and_exact_review_decisions_after_restart() {
    let mut f = Fixture::new();
    f.write("a", "first\nmiddle\nlast\n");
    let cp = f.begin();
    f.write("a", "FIRST\nmiddle\nLAST\n");
    f.finish();
    let id = f.journal.summary("agent", "chat").edits[0].id;
    f.journal
        .accept("agent", "chat", &Selection::Chunk(id, 0))
        .unwrap();
    f.decline(Selection::Checkpoint(cp)).unwrap();
    let Fixture {
        root,
        data,
        journal,
    } = f;
    drop(journal);
    let mut journal = Journal::open(root.path(), data.path()).unwrap();
    assert!(journal.summary("agent", "chat").can_undo);
    fs::write(root.path().join("a"), "first\nuser middle\nlast\n").unwrap();
    let plan = journal.undo_plan("agent", "chat").unwrap();
    journal.apply(plan).unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("a")).unwrap(),
        "FIRST\nuser middle\nLAST\n"
    );
    let summary = journal.summary("agent", "chat");
    assert!(!summary.can_undo);
    assert_eq!(summary.edits[0].chunks[0].decision, Decision::Accepted);
    assert_eq!(summary.edits[0].chunks[1].decision, Decision::Pending);
}

#[test]
fn undo_rollback_refuses_overlaps_and_restores_created_files() {
    let mut f = Fixture::new();
    f.write("a", "old");
    f.begin();
    f.write("a", "agent");
    f.write("new", "created");
    f.finish();
    f.decline(Selection::All).unwrap();
    f.write("a", "user");
    assert!(f.journal.undo_plan("agent", "chat").is_err());
    assert!(!f.root.path().join("new").exists());
    f.write("a", "old");
    let plan = f.journal.undo_plan("agent", "chat").unwrap();
    f.journal.apply(plan).unwrap();
    assert_eq!(f.read("a"), "agent");
    assert_eq!(f.read("new"), "created");
}

#[test]
fn malformed_recovery_paths_and_decisions_are_rejected_before_mutation() {
    let mut f = Fixture::new();
    f.write("a", "old");
    f.begin();
    f.write("a", "new");
    f.finish();
    let mut plan = f.journal.plan("agent", "chat", &Selection::All).unwrap();
    plan.replacements[0].backup = Some(PathBuf::from("../outside"));
    assert!(f.journal.apply(plan).is_err());
    assert_eq!(f.read("a"), "new");
    let mut plan = f.journal.plan("agent", "chat", &Selection::All).unwrap();
    plan.decisions[0].1 = usize::MAX;
    assert!(f.journal.apply(plan).is_err());
    assert_eq!(f.read("a"), "new");
    let mut plan = f.journal.plan("agent", "chat", &Selection::All).unwrap();
    plan.replacements[0].path = PathBuf::from(".git/config");
    assert!(f.journal.apply(plan).is_err());
    assert_eq!(f.read("a"), "new");
}

#[test]
fn parallel_capture_reuses_duplicate_content_and_excludes_tracked_metadata() {
    let mut f = Fixture::new();
    // Reach the parallel capture threshold while retaining all eight workers.
    for i in 0..64 {
        f.write(&format!("file-{i}"), "same bytes\r\n");
    }
    fs::create_dir(f.root.path().join(".vtcode")).unwrap();
    f.write(".vtcode/settings.json", "private metadata");
    let repo = git2::Repository::open(f.root.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new(".vtcode/settings.json")).unwrap();
    index.write().unwrap();
    f.begin();
    let files = &f.journal.state.active.first().unwrap().before;
    assert_eq!(files.len(), 64);
    assert!(
        files
            .values()
            .all(|file| file.blob == files[Path::new("file-0")].blob)
    );
    f.finish();
    assert!(f.journal.summary("agent", "chat").edits.is_empty());
}

#[test]
fn failed_journal_save_blocks_further_actions_and_preserves_files() {
    let mut f = Fixture::new();
    f.write("a", "old");
    f.begin();
    f.write("a", "agent");
    f.finish();
    let directory = f.journal.directory.clone();
    f.journal.directory = directory.join("missing-parent");
    assert!(f.journal.accept("agent", "chat", &Selection::All).is_err());
    f.journal.directory = directory;
    assert!(f.journal.plan("agent", "chat", &Selection::All).is_err());
    assert_eq!(f.read("a"), "agent");
}

#[test]
#[ignore = "manual checkpoint throughput and object reuse measurement"]
fn checkpoint_benchmark() {
    let mut f = Fixture::new();
    let payload = "content\n".repeat(2048);
    for i in 0..512 {
        f.write(&format!("file-{i}.txt"), &format!("{i}\n{payload}"));
    }
    f.write(".gitignore", "ignored/\n");
    fs::create_dir(f.root.path().join("ignored")).unwrap();
    fs::write(
        f.root.path().join("ignored/large.bin"),
        vec![0u8; 16 * 1024 * 1024],
    )
    .unwrap();
    let start = std::time::Instant::now();
    f.begin();
    let first = start.elapsed();
    f.finish();
    let blobs = |root: &Path| {
        let repo = git2::Repository::open(root).unwrap();
        let odb = repo.odb().unwrap();
        let mut count = 0;
        odb.foreach(|id| {
            if repo.find_object(*id, None).unwrap().kind() == Some(git2::ObjectType::Blob) {
                count += 1;
            }
            true
        })
        .unwrap();
        count
    };
    let initial_blobs = blobs(f.root.path());
    let start = std::time::Instant::now();
    f.begin();
    let unchanged = start.elapsed();
    f.finish();
    assert_eq!(blobs(f.root.path()), initial_blobs);
    assert_eq!(initial_blobs, 513, "ignored file must not enter snapshots");
    let start = std::time::Instant::now();
    f.begin();
    f.write("file-17.txt", "changed\n");
    f.finish();
    let turn = start.elapsed();
    assert_eq!(blobs(f.root.path()), initial_blobs + 1);
    eprintln!(
        "512 text files / 8 MiB, 16 MiB ignored: first={first:?}, unchanged={unchanged:?}, capture changed turn={turn:?}; unchanged checkpoints added 0 blobs, changed turn added 1 blob"
    );
}

#[test]
fn accepts_without_writing_and_checkpoint_still_reverses_accepted_changes() {
    let mut f = Fixture::new();
    f.write("a", "original\r\n");
    let cp = f.begin();
    f.write("a", "agent\r\n");
    f.finish();
    let before = fs::metadata(f.root.path().join("a"))
        .unwrap()
        .modified()
        .unwrap();
    f.journal.accept("agent", "chat", &Selection::All).unwrap();
    assert_eq!(
        fs::metadata(f.root.path().join("a"))
            .unwrap()
            .modified()
            .unwrap(),
        before
    );
    assert_eq!(f.journal.summary("agent", "chat").edits[0].pending(), 0);
    f.decline(Selection::Checkpoint(cp)).unwrap();
    assert_eq!(f.read("a"), "original\r\n");
}

#[test]
fn acceptance_keeps_running_checkpoints_and_later_changes_pending() {
    for editor in [false, true] {
        let mut f = Fixture::new();
        f.write("a", "first\nkeep\nlast\n");
        let original = f.begin();
        f.write("a", "FIRST\nkeep\nLAST\n");
        f.finish();
        let edit = f.journal.summary("agent", "chat").edits[0].id;
        let active = f.begin();
        let other = f
            .journal
            .begin("other", "other-chat", "also working", BTreeSet::new())
            .unwrap();
        f.write("a", "FIRST\nworking\nLAST\n");
        let path = f.root.path().join("a");
        let modified = fs::metadata(&path).unwrap().modified().unwrap();

        if editor {
            f.journal
                .accept_editor(&Selection::File("a".into()))
                .unwrap();
        } else {
            f.journal.accept("agent", "chat", &Selection::All).unwrap();
        }

        assert_eq!(f.journal.summary("agent", "chat").active, Some(active));
        assert_eq!(f.journal.summary("other", "other-chat").active, Some(other));
        assert_eq!(f.journal.summary("agent", "chat").edits[0].pending(), 0);
        assert_eq!(f.read("a"), "FIRST\nworking\nLAST\n");
        assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
        assert!(f.decline(Selection::All).is_err());
        assert!(f.decline(Selection::Checkpoint(original)).is_err());
        assert!(f.journal.plan_editor(&Selection::File("a".into())).is_err());

        f.journal
            .finish_checkpoint(active, BTreeSet::new())
            .unwrap();
        f.journal.finish_checkpoint(other, BTreeSet::new()).unwrap();
        let summary = f.journal.summary("agent", "chat");
        assert!(summary.active.is_none());
        assert!(
            summary
                .edits
                .iter()
                .find(|e| e.id == edit)
                .unwrap()
                .chunks
                .iter()
                .all(|c| c.decision == Decision::Accepted)
        );
        assert!(
            summary
                .edits
                .iter()
                .filter(|e| e.checkpoint == active)
                .any(|e| e.pending() > 0)
        );
    }
}

#[test]
fn queued_acceptance_resolves_only_the_chunks_visible_at_the_click() {
    let mut f = Fixture::new();
    f.write("a", "first\nkeep\nlast\n");
    f.begin();
    f.write("a", "FIRST\nkeep\nLAST\n");
    f.finish();
    let summary = f.journal.summary("agent", "chat");
    let visible = summary.pending_chunks(&Selection::All).unwrap();
    assert_eq!(visible.len(), 2);
    assert_eq!(
        summary
            .pending_chunks(&Selection::File("a".into()))
            .unwrap(),
        visible
    );
    assert!(
        summary
            .pending_chunks(&Selection::Checkpoint(summary.checkpoints[0].id))
            .is_err()
    );
    let later = f.begin();
    f.write("a", "FIRST\nlater\nLAST\n");
    f.finish();

    f.journal.accept_chunks(&visible).unwrap();

    let summary = f.journal.summary("agent", "chat");
    assert_eq!(summary.pending_chunks(&Selection::All).unwrap().len(), 1);
    assert!(
        summary
            .edits
            .iter()
            .filter(|e| e.checkpoint != later)
            .all(|e| e.pending() == 0)
    );
    assert_eq!(
        summary
            .edits
            .iter()
            .find(|e| e.checkpoint == later)
            .unwrap()
            .pending(),
        1
    );
    assert_eq!(f.read("a"), "FIRST\nlater\nLAST\n");
}

#[test]
fn accepting_exact_chunks_is_atomic_idempotent_and_respects_recovery() {
    let mut f = Fixture::new();
    f.write("a", "first\nkeep\nlast\n");
    let checkpoint = f.begin();
    f.write("a", "FIRST\nkeep\nLAST\n");
    f.finish();
    let edit = f.journal.summary("agent", "chat").edits[0].id;
    let version = f.journal.state.version;
    assert!(
        f.journal
            .accept_chunks(&[(edit, 0), (u64::MAX, 0)])
            .is_err()
    );
    assert_eq!(f.journal.state.version, version);
    assert_eq!(f.journal.summary("agent", "chat").edits[0].pending(), 2);
    f.journal.accept_chunks(&[(edit, 0), (edit, 0)]).unwrap();
    let version = f.journal.state.version;
    f.journal.accept_chunks(&[(edit, 0)]).unwrap();
    assert_eq!(f.journal.state.version, version);
    f.decline(Selection::Chunk(edit, 1)).unwrap();
    let version = f.journal.state.version;
    assert!(f.journal.accept_chunks(&[(edit, 0), (edit, 1)]).is_err());
    assert_eq!(f.journal.state.version, version);
    assert_eq!(f.read("a"), "FIRST\nkeep\nlast\n");
    let plan = f
        .journal
        .plan("agent", "chat", &Selection::Checkpoint(checkpoint))
        .unwrap();
    f.journal.state.transaction = Some(plan);
    assert!(
        f.journal
            .accept_chunks(&[(edit, 0)])
            .unwrap_err()
            .contains("recovery")
    );
}

#[test]
fn chunk_decisions_compose_and_preserve_user_edits() {
    let mut f = Fixture::new();
    f.write("a", "first\nmiddle\nlast\n");
    f.begin();
    f.write("a", "FIRST\nmiddle\nLAST\n");
    f.finish();
    let edit = &f.journal.summary("agent", "chat").edits[0];
    assert_eq!(edit.chunks.len(), 2);
    let id = edit.id;
    f.journal
        .accept("agent", "chat", &Selection::Chunk(id, 0))
        .unwrap();
    f.write("a", "FIRST\nuser middle\nLAST\n");
    f.decline(Selection::Chunk(id, 1)).unwrap();
    assert_eq!(f.read("a"), "FIRST\nuser middle\nlast\n");
}

#[test]
fn rejecting_all_preflights_every_file_and_never_partially_handles_conflicts() {
    let mut f = Fixture::new();
    f.write("a", "old\n");
    f.write("z", "old\n");
    f.begin();
    f.write("a", "agent\n");
    f.write("z", "agent\n");
    f.finish();
    f.write("z", "user\n");
    assert!(f.decline(Selection::All).is_err());
    assert_eq!(f.read("a"), "agent\n");
    assert_eq!(f.read("z"), "user\n");
    assert_eq!(
        f.journal
            .summary("agent", "chat")
            .edits
            .iter()
            .map(Edit::pending)
            .sum::<usize>(),
        2
    );
}

#[test]
fn distinguishes_missing_empty_created_deleted_and_binary_files() {
    let mut f = Fixture::new();
    f.write("empty", "");
    f.write("deleted", "original");
    fs::write(f.root.path().join("binary"), [0, 255, 1]).unwrap();
    f.begin();
    f.write("empty", "now text");
    f.write("new-empty", "");
    fs::remove_file(f.root.path().join("deleted")).unwrap();
    fs::write(f.root.path().join("binary"), [0, 255, 2]).unwrap();
    f.finish();
    f.decline(Selection::All).unwrap();
    assert_eq!(f.read("empty"), "");
    assert_eq!(f.read("deleted"), "original");
    assert!(!f.root.path().join("new-empty").exists());
    assert_eq!(fs::read(f.root.path().join("binary")).unwrap(), [0, 255, 1]);
}

#[test]
fn reverse_order_restores_message_state_without_touching_earlier_work() {
    let mut f = Fixture::new();
    f.write("a", "initial");
    f.begin();
    f.write("a", "first");
    f.finish();
    let second = f.begin();
    f.write("a", "second");
    f.finish();
    f.begin();
    f.write("a", "third");
    f.finish();
    f.decline(Selection::Checkpoint(second)).unwrap();
    assert_eq!(f.read("a"), "first");
    f.decline(Selection::All).unwrap();
    assert_eq!(f.read("a"), "initial");
}

#[test]
fn partial_declines_are_not_undone_again() {
    let mut f = Fixture::new();
    f.write("a", "a\nb\nc\n");
    let cp = f.begin();
    f.write("a", "A\nb\nC\n");
    f.finish();
    let id = f.journal.summary("agent", "chat").edits[0].id;
    f.decline(Selection::Chunk(id, 0)).unwrap();
    f.decline(Selection::Checkpoint(cp)).unwrap();
    assert_eq!(f.read("a"), "a\nb\nc\n");
}

#[test]
fn changed_plan_and_another_chats_edits_cannot_be_overwritten() {
    let mut f = Fixture::new();
    f.write("a", "old");
    f.begin();
    f.write("a", "agent");
    f.finish();
    let plan = f.journal.plan("agent", "chat", &Selection::All).unwrap();
    f.write("a", "newer");
    assert!(f.journal.apply(plan).is_err());
    assert_eq!(f.read("a"), "newer");
    assert!(
        f.journal
            .plan("other-agent", "chat", &Selection::All)
            .is_err()
    );
}

#[test]
fn git_index_branch_and_preexisting_changes_are_preserved_and_objects_reused() {
    let mut f = Fixture::new();
    f.write("a", "staged");
    f.write(".gitignore", "ignored\n");
    f.write("ignored", "skip");
    let repo = git2::Repository::open(f.root.path()).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("a")).unwrap();
    index.write().unwrap();
    let index_bytes = fs::read(repo.path().join("index")).unwrap();
    f.write("a", "preexisting dirty");
    f.begin();
    let active = f.journal.state.active.first().unwrap();
    assert!(!active.before.contains_key(Path::new("ignored")));
    let digest = active.before[Path::new("a")].blob.hash.clone();
    f.write("a", "agent");
    f.finish();
    f.decline(Selection::All).unwrap();
    assert_eq!(f.read("a"), "preexisting dirty");
    f.begin();
    assert_eq!(
        f.journal.state.active.first().unwrap().before[Path::new("a")]
            .blob
            .hash,
        digest
    );
    assert_eq!(fs::read(repo.path().join("index")).unwrap(), index_bytes);
    assert!(repo.head().is_err());
    assert!(
        repo.references_glob("refs/vtcode/checkpoints/*")
            .unwrap()
            .count()
            > 0
    );
}

#[test]
fn checkpoints_survive_restart_and_protect_user_changes_during_turn() {
    let mut f = Fixture::new();
    f.write("a", "old");
    f.begin();
    f.write("a", "user and agent");
    let Fixture {
        root,
        data,
        journal,
    } = f;
    drop(journal);
    let mut journal = Journal::open(root.path(), data.path()).unwrap();
    assert!(journal.active().is_some());
    journal
        .finish(BTreeSet::from([PathBuf::from("a")]))
        .unwrap();
    assert!(journal.plan("agent", "chat", &Selection::All).is_err());
    assert_eq!(
        fs::read_to_string(root.path().join("a")).unwrap(),
        "user and agent"
    );
}

#[test]
fn prepared_transaction_can_be_recovered_after_a_partial_replace() {
    let mut f = Fixture::new();
    f.write("a", "old a");
    f.write("b", "old b");
    f.begin();
    f.write("a", "new a");
    f.write("b", "new b");
    f.finish();
    let mut plan = f.journal.plan("agent", "chat", &Selection::All).unwrap();
    plan.replacements[0].backup = Some(PathBuf::from(".vtcode-rollback-test"));
    let mut state = f.journal.state.clone();
    state.transaction = Some(plan.clone());
    f.journal.save(state).unwrap();
    f.journal.replace(&plan.replacements[0]).unwrap();
    assert_eq!(f.read("a"), "old a");
    let Fixture {
        root,
        data,
        journal,
    } = f;
    drop(journal);
    let mut journal = Journal::open(root.path(), data.path()).unwrap();
    assert!(journal.interrupted());
    journal.recover().unwrap();
    assert_eq!(fs::read_to_string(root.path().join("a")).unwrap(), "new a");
    assert_eq!(fs::read_to_string(root.path().join("b")).unwrap(), "new b");
    assert!(!journal.interrupted());
}

#[cfg(unix)]
#[test]
fn symlink_retargeting_never_touches_external_files() {
    let mut f = Fixture::new();
    f.write("a", "old");
    f.begin();
    f.write("a", "agent");
    f.finish();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("a"), "outside").unwrap();
    fs::remove_file(f.root.path().join("a")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("a"), f.root.path().join("a")).unwrap();
    assert!(f.decline(Selection::All).is_err());
    assert_eq!(
        fs::read_to_string(outside.path().join("a")).unwrap(),
        "outside"
    );
}

#[test]
fn concurrent_checkpoints_protect_shared_changes_and_keep_exclusive_edits_reversible() {
    let mut f = Fixture::new();
    f.write("before-overlap", "original");
    f.write("shared", "original");
    f.write("after-overlap", "original");
    let a = f.begin();
    f.write("before-overlap", "first chat only");
    let b = f
        .journal
        .begin("other", "chat", "second chat", BTreeSet::new())
        .unwrap();
    assert_eq!(f.journal.summary("agent", "chat").active, Some(a));
    assert_eq!(f.journal.summary("other", "chat").active, Some(b));
    assert_eq!(f.journal.summary("other", "unrelated").active, None);
    assert!(
        f.journal
            .begin("agent", "chat", "duplicate", BTreeSet::new())
            .is_err()
    );
    f.write("shared", "concurrent changes");
    f.journal.finish_checkpoint(a, BTreeSet::new()).unwrap();
    assert_eq!(f.journal.summary("agent", "chat").active, None);
    assert_eq!(f.journal.summary("other", "chat").active, Some(b));
    f.write("after-overlap", "second chat only");
    f.journal.finish_checkpoint(b, BTreeSet::new()).unwrap();
    for owner in ["agent", "other"] {
        let summary = f.journal.summary(owner, "chat");
        let shared = summary
            .edits
            .iter()
            .find(|e| e.path == Path::new("shared"))
            .unwrap();
        assert!(shared.conflict.as_ref().unwrap().contains("concurrently"));
        assert!(f.journal.plan(owner, "chat", &Selection::All).is_err());
        assert!(
            f.journal
                .plan(
                    owner,
                    "chat",
                    &Selection::Checkpoint(if owner == "agent" { a } else { b })
                )
                .is_err()
        );
        let (_, before, after) = f.journal.review(shared.id).unwrap();
        assert_eq!(before, "original");
        assert_eq!(after, "concurrent changes");
    }
    f.decline(Selection::File("before-overlap".into())).unwrap();
    let plan = f
        .journal
        .plan("other", "chat", &Selection::File("after-overlap".into()))
        .unwrap();
    f.journal.apply(plan).unwrap();
    assert_eq!(f.read("before-overlap"), "original");
    assert_eq!(f.read("after-overlap"), "original");
    assert_eq!(f.read("shared"), "concurrent changes");
}

#[test]
fn concurrent_checkpoints_finish_in_either_order_and_detect_three_way_overlap() {
    let mut f = Fixture::new();
    let a = f.begin();
    let b = f
        .journal
        .begin("agent", "second", "second", BTreeSet::new())
        .unwrap();
    f.write("from-ab", "shared A and B");
    let c = f
        .journal
        .begin("agent", "third", "third", BTreeSet::new())
        .unwrap();
    f.write("from-abc", "shared A, B and C");
    f.journal.finish_checkpoint(b, BTreeSet::new()).unwrap();
    f.write("from-ac", "shared A and C");
    f.journal.finish_checkpoint(c, BTreeSet::new()).unwrap();
    f.write("only-a", "A after all others finished");
    f.journal.finish_checkpoint(a, BTreeSet::new()).unwrap();
    let third = f.journal.summary("agent", "third");
    assert_eq!(third.edits.len(), 2);
    assert!(third.edits.iter().all(|e| e.conflict.is_some()));
    let second = f.journal.summary("agent", "second");
    assert_eq!(second.edits.len(), 2);
    assert!(second.edits.iter().all(|e| e.conflict.is_some()));
    f.decline(Selection::File("only-a".into())).unwrap();
    assert!(!f.root.path().join("only-a").exists());
    for name in ["from-ab", "from-abc", "from-ac"] {
        assert!(f.root.path().join(name).exists());
    }
}

#[test]
fn concurrent_interrupted_turns_recover_independently_after_restart() {
    let mut f = Fixture::new();
    f.write("a", "original");
    f.begin();
    f.journal
        .begin("other", "chat", "second", BTreeSet::new())
        .unwrap();
    f.write("a", "combined work before crash");
    let Fixture {
        root,
        data,
        journal,
    } = f;
    drop(journal);
    let mut journal = Journal::open(root.path(), data.path()).unwrap();
    journal.finish_uncertain().unwrap();
    assert!(journal.active().is_none());
    for owner in ["agent", "other"] {
        assert_eq!(journal.summary(owner, "chat").edits.len(), 1);
        assert!(journal.plan(owner, "chat", &Selection::All).is_err());
        journal.accept(owner, "chat", &Selection::All).unwrap();
    }
    assert_eq!(
        fs::read_to_string(root.path().join("a")).unwrap(),
        "combined work before crash"
    );
}

#[test]
fn legacy_single_turn_history_migrates_without_losing_its_snapshot() {
    let mut f = Fixture::new();
    f.write("a", "original");
    let a = f.begin();
    let mut state = serde_json::to_value(&f.journal.state).unwrap();
    state["schema"] = 1.into();
    state["active"] = state["active"][0].clone();
    state["active"].as_object_mut().unwrap().remove("shared");
    state.as_object_mut().unwrap().remove("boundary");
    let Fixture {
        root,
        data,
        journal,
    } = f;
    drop(journal);
    fs::write(
        data.path().join("state.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
    let mut journal = Journal::open(root.path(), data.path()).unwrap();
    assert_eq!(journal.active(), Some(a));
    let b = journal
        .begin("agent", "second", "parallel after upgrade", BTreeSet::new())
        .unwrap();
    journal.finish_checkpoint(b, BTreeSet::new()).unwrap();
    fs::write(root.path().join("a"), "agent").unwrap();
    journal.finish_checkpoint(a, BTreeSet::new()).unwrap();
    let plan = journal.plan("agent", "chat", &Selection::All).unwrap();
    journal.apply(plan).unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("a")).unwrap(),
        "original"
    );
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(data.path().join("state.json")).unwrap()).unwrap();
    assert_eq!(state["schema"], 2);
    assert!(state["active"].is_array());
}
