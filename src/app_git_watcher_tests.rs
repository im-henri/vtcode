use super::*;
use crate::app::SourceControlRepositoryState;

fn paths(names: &[&str]) -> Option<FxHashSet<PathBuf>> {
    Some(names.iter().map(PathBuf::from).collect())
}

#[test]
fn sustained_build_notifications_have_bounded_refresh_rate_and_a_final_flush() {
    let mut app = App::test_new();
    let root = app.explorer.root.clone();
    let start = Instant::now();
    let mut flushes = Vec::new();
    let mut received = FxHashSet::default();
    // The reported workload: hundreds of unique paths every 300 ms, with
    // tracked source changes interleaved with compiler and simulator output.
    for batch in 0..42 {
        let now = start + Duration::from_millis(batch * 300);
        let mut changed: FxHashSet<_> = (0..180)
            .map(|file| root.join(format!("build/test-{batch}/unit-{file}.o")))
            .collect();
        if batch % 7 == 0 {
            changed.insert(root.join("src/simulator.cc"));
        }
        app.git_watcher_refresh
            .queue(root.clone(), Some(changed), now);
        if app.git_watcher_refresh_wait(now) == Some(Duration::ZERO) {
            received.extend(app.git_watcher_refresh.take(&root).unwrap().unwrap());
            flushes.push(now);
        }
    }
    assert_eq!(
        flushes.len(),
        5,
        "42 notifications must not cause 42 status reads"
    );
    let final_event = start + Duration::from_millis(41 * 300);
    assert_eq!(app.git_watcher_refresh_wait(final_event), Some(QUIET));
    received.extend(app.git_watcher_refresh.take(&root).unwrap().unwrap());
    assert_eq!(
        received.len(),
        42 * 180 + 1,
        "no changed path may be lost between refreshes"
    );
}

#[test]
fn watcher_refresh_absorbs_metadata_and_explicit_requests_without_losing_paths() {
    let mut app = App::test_new();
    let root = app.explorer.root.clone();
    let now = Instant::now();
    app.git_watcher_refresh
        .queue(root.clone(), paths(&["first"]), now);
    app.git_watcher_refresh
        .queue(root.clone(), None, now + Duration::from_millis(300));
    app.git_watcher_refresh.queue(
        root.clone(),
        paths(&["last"]),
        now + Duration::from_millis(600),
    );
    let mut explicit = paths(&["saved"]);
    app.absorb_git_watcher_refresh(&mut explicit);
    assert!(
        explicit.is_none(),
        "metadata invalidation cannot be downgraded"
    );
    assert!(app.git_watcher_refresh_wait(now).is_none());

    app.git_watcher_refresh
        .queue(root.clone(), paths(&["first"]), now);
    let mut explicit = paths(&["saved"]);
    app.absorb_git_watcher_refresh(&mut explicit);
    assert_eq!(explicit.unwrap(), paths(&["first", "saved"]).unwrap());
    app.git_watcher_refresh
        .queue(root, paths(&["old-workspace"]), now);
    app.explorer.root = PathBuf::from("another-workspace");
    assert!(!app.poll_git_watcher_refresh(now + MAX_DELAY));
    app.cancel_source_control_reads();
    assert!(app.git_watcher_refresh.pending.is_none());
}

#[test]
fn watcher_queue_is_bounded_and_does_not_spin_while_a_reader_is_running() {
    let mut app = App::test_new();
    let now = Instant::now();
    let root = app.explorer.root.clone();
    app.git_watcher_refresh.queue(
        root.clone(),
        Some((0..=MAX_PATHS).map(|i| root.join(i.to_string())).collect()),
        now,
    );
    assert_eq!(
        app.git_watcher_refresh
            .pending
            .as_ref()
            .unwrap()
            .paths
            .as_ref()
            .unwrap(),
        &std::iter::once(root).collect()
    );
    app.bg_git_status_running = true;
    assert_eq!(app.git_watcher_refresh_wait(now + MAX_DELAY), None);
    assert!(!app.poll_git_watcher_refresh(now + MAX_DELAY));
    app.bg_git_status_running = false;
    assert_eq!(
        app.git_watcher_refresh_wait(now + MAX_DELAY),
        Some(Duration::ZERO)
    );
}

#[test]
fn real_file_notifications_defer_status_but_explicit_refresh_runs_immediately() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    std::fs::write(root.join("source.cc"), "original\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("source.cc")).unwrap();
    index.write().unwrap();
    let mut app = App::new_deferred(root.clone());
    app.workspace_watcher_active = true;
    app.refresh_source_control_async();
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.bg_git_status_running && !app.bg_git_status_pending
    });
    assert_eq!(
        app.source_control_repository_state,
        SourceControlRepositoryState::Available
    );
    std::fs::write(root.join("source.cc"), "modified\n").unwrap();
    let change = crate::background::inspect_workspace_path(root.join("source.cc"));
    app.apply_workspace_file_changes(root.clone(), vec![change], false, false, false);
    assert!(!app.bg_git_status_running && !app.bg_git_status_pending);
    assert!(app.git_watcher_refresh_wait(Instant::now()).is_some());
    assert!(app.poll_git_watcher_refresh(Instant::now() + MAX_DELAY));
    assert!(app.bg_git_status_running && app.bg_git_status_passive);
    app.refresh_source_control_async();
    assert!(
        !app.bg_git_status_passive,
        "explicit refresh restores loading feedback"
    );
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.bg_git_status_running && !app.bg_git_status_pending
    });
    assert!(
        app.source_control_files
            .iter()
            .any(|file| file.path == "source.cc" && file.status == 'M')
    );
}

#[test]
fn overflow_and_metadata_storms_coalesce_history_and_branch_reads_too() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    git2::Repository::init(&root).unwrap();
    let mut app = App::new_deferred(root);
    app.workspace_watcher_active = true;
    app.refresh_source_control_async();
    let ready = |app: &App| {
        !app.bg_git_status_running
            && !app.bg_git_status_pending
            && !app.bg_git_log_running
            && !app.bg_git_branch_running
            && !app.bg_git_branch_pending
    };
    app.wait_for_background(Duration::from_secs(10), ready);
    let generation = app.git_history.generation;
    for _ in 0..12 {
        app.refresh_source_control_from_watcher(FxHashSet::default(), true);
    }
    assert_eq!(app.git_history.generation, generation);
    assert!(
        ready(&app),
        "overflow notifications must not repeatedly start history and branch workers"
    );
    assert!(app.poll_git_watcher_refresh(Instant::now() + MAX_DELAY));
    assert_eq!(app.git_history.generation, generation + 1);
    app.wait_for_background(Duration::from_secs(10), ready);
}

#[test]
fn index_only_events_update_staging_without_reloading_history_or_branch_counts() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let repo = git2::Repository::init(&root).unwrap();
    let mut app = App::new_deferred(root.clone());
    app.workspace_watcher_active = true;
    let ready = |app: &App| !app.bg_git_status_running && !app.bg_git_status_pending
        && !app.bg_git_log_running && !app.bg_git_branch_running && !app.bg_git_branch_pending;
    app.refresh_source_control_async();
    app.wait_for_background(Duration::from_secs(10), ready);
    let generation = app.git_history.generation;
    std::fs::write(root.join("staged.txt"), "staged\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(std::path::Path::new("staged.txt")).unwrap();
    index.write().unwrap();
    app.refresh_source_control_from_watcher([root.join(".git/index"), root.join(".git/index.lock")].into_iter().collect(), true);
    assert!(app.poll_git_watcher_refresh(Instant::now() + MAX_DELAY));
    assert_eq!(app.git_history.generation, generation);
    assert!(!app.bg_git_branch_running && !app.bg_git_branch_pending);
    app.wait_for_background(Duration::from_secs(10), ready);
    assert!(app.source_control_files.iter().any(|file| file.path == "staged.txt" && file.staged));
    // A mixed HEAD/index burst cannot be downgraded to index-only refresh.
    app.refresh_source_control_from_watcher([root.join(".git/index")].into_iter().collect(), true);
    app.refresh_source_control_from_watcher([root.join(".git/HEAD")].into_iter().collect(), true);
    assert!(app.poll_git_watcher_refresh(Instant::now() + MAX_DELAY));
    assert_eq!(app.git_history.generation, generation + 1);
    app.wait_for_background(Duration::from_secs(10), ready);
}
