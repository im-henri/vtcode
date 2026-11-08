use super::*;
use std::time::Instant;

fn fixture() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(temp.path()).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", false)
        .unwrap();
    for name in ["modified.txt", "deleted.txt", "moved.txt"] {
        std::fs::write(temp.path().join(name), format!("{name}\n").repeat(30)).unwrap();
    }
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "Initial", &tree, &[])
        .unwrap();
    temp
}

fn key(files: Vec<GitFileChange<PathBuf>>) -> Vec<(PathBuf, Option<PathBuf>, char, bool, bool)> {
    let mut files: Vec<_> = files
        .into_iter()
        .map(|file| {
            (
                file.path,
                file.original_path,
                file.status,
                file.staged,
                file.directory,
            )
        })
        .collect();
    files.sort();
    files
}

fn assert_matches_libgit2(root: &Path, include_untracked: bool) {
    let repo = git2::Repository::open(root).unwrap();
    assert!(
        !prefer_native(&repo),
        "small fixtures also exercise the bundled backend"
    );
    let native = read(
        &repo,
        include_untracked,
        &CancellationToken::new(),
        Path::to_path_buf,
    )
    .unwrap()
    .unwrap();
    let bundled = crate::app_git::collect_source_control_status_with_paths(
        root.to_owned(),
        || {},
        &CancellationToken::new(),
        include_untracked,
        Path::to_path_buf,
    );
    assert!(bundled.error.is_none());
    assert_eq!(key(native), key(bundled.files));
}

#[test]
fn native_status_preserves_staged_and_unstaged_renames_deletions_and_literal_names() {
    let temp = fixture();
    let root = temp.path();
    std::fs::rename(root.join("moved.txt"), root.join("renamed [λ].txt")).unwrap();
    std::fs::remove_file(root.join("deleted.txt")).unwrap();
    std::fs::write(root.join("modified.txt"), "staged\n").unwrap();
    let repo = git2::Repository::open(root).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    std::fs::write(root.join("modified.txt"), "modified after staging\n").unwrap();
    std::fs::write(root.join("renamed [λ].txt"), "changed after rename\n").unwrap();
    std::fs::write(root.join("--new file.txt"), "new\n").unwrap();
    for visible in [true, false] {
        assert_matches_libgit2(root, visible);
    }
}

#[test]
fn untracked_scanner_keeps_ignore_rules_and_hides_working_copies_inside_collapsed_directories() {
    let temp = fixture();
    let root = temp.path();
    std::fs::write(root.join(".gitignore"), "*.cache\n").unwrap();
    std::fs::write(root.join(".git/info/exclude"), "local-only.txt\n").unwrap();
    std::fs::write(root.join("local-only.txt"), "ignored\n").unwrap();
    for path in [
        "fresh/deep/new.txt",
        "mixed/.vtcode-format-test/tmp.txt",
        "mixed/visible.txt",
        "only-temporary/.vtcode-copy-ABcd12345678/tmp.txt",
        ".vtcode-format-root/tmp.txt",
        "ignored-tree/output.cache",
    ] {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "new\n").unwrap();
    }
    let repo = git2::Repository::open(root).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_path(Path::new(".vtcode-format-root/tmp.txt"))
        .unwrap();
    index.write().unwrap();
    std::fs::write(root.join(".vtcode-format-root/tmp.txt"), "tracked edit\n").unwrap();
    assert_matches_libgit2(root, true);
}

#[test]
fn native_status_handles_unborn_repositories_and_intent_to_add() {
    let temp = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(temp.path()).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", false)
        .unwrap();
    std::fs::write(temp.path().join("new.txt"), "new\n").unwrap();
    assert_matches_libgit2(temp.path(), true);
    let mut command = command(&repo).unwrap();
    command.args(["add", "--intent-to-add", "--", "new.txt"]);
    assert!(
        run_output(&mut command, &CancellationToken::new())
            .unwrap()
            .is_some()
    );
    assert_matches_libgit2(temp.path(), false);
}

#[test]
fn many_untracked_directories_keep_standalone_files_unique() {
    let temp = fixture();
    for number in 0..128 {
        let directory = temp.path().join(format!("new-{number}-{}", "long-name".repeat(8)));
        std::fs::create_dir(&directory).unwrap();
        std::fs::write(directory.join("file.txt"), "new\n").unwrap();
    }
    std::fs::write(temp.path().join("standalone.txt"), "new\n").unwrap();
    assert_matches_libgit2(temp.path(), true);
}

#[test]
fn large_full_refreshes_use_native_status_and_file_events_keep_literal_scopes() {
    let temp = fixture();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let repo = git2::Repository::open(&root).unwrap();
    let mut index = repo.index().unwrap();
    // Populate a large index without creating thousands of unnecessary files.
    // Missing indexed paths are real tracked deletions in the full snapshot.
    for number in 0..NATIVE_STATUS_MIN_ENTRIES {
        let mut entry = index.get(0).unwrap();
        entry.path = format!("padding-{number:05}.txt").into_bytes();
        index.add(&entry).unwrap();
    }
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    let sig = git2::Signature::now("Test", "test@example.com").unwrap();
    repo.commit(Some("HEAD"), &sig, &sig, "Large index", &tree, &[&parent])
        .unwrap();
    assert!(prefer_native(&repo));
    std::fs::write(root.join("modified.txt"), "changed\n").unwrap();
    let before = READS.get();
    let snapshot = crate::app_git::collect_source_control_status_with_paths(
        root.clone(),
        || {},
        &CancellationToken::new(),
        true,
        Path::to_path_buf,
    );
    assert!(snapshot.error.is_none(), "{:?}", snapshot.error);
    assert_eq!(READS.get(), before + 1);
    assert_eq!(snapshot.files.len(), NATIVE_STATUS_MIN_ENTRIES + 1);
    let mut discovery = crate::git_repositories::Discovery {
        workspace: root.clone(),
        roots: vec![root.clone()],
        parent: None,
        errors: Vec::new(),
    };
    let scans = crate::app_git::refresh::collect_with_paths(
        &mut discovery,
        vec![root.clone()],
        Some(crate::app_git::refresh::WorktreeEvents {
            paths: [root.join("modified.txt")].into_iter().collect(),
            revalidate: false,
        }),
        &root,
        &CancellationToken::new(),
        true,
        |path| path.to_string_lossy().into_owned(),
    );
    assert_eq!(
        READS.get(),
        before + 1,
        "file events must not launch whole-repository Git commands"
    );
    assert_eq!(scans.len(), 1);
    assert_eq!(scans[0].files.len(), 1);
    assert_eq!(scans[0].files[0].path, "modified.txt");
    assert!(scans[0].worktree_paths.is_some());
}

#[test]
fn existing_head_files_do_not_need_an_index_or_a_repository_wide_rename_scan() {
    let temp = fixture();
    let repo = git2::Repository::open(temp.path()).unwrap();
    let expected = repo
        .head()
        .unwrap()
        .peel_to_tree()
        .unwrap()
        .get_path(Path::new("modified.txt"))
        .unwrap()
        .id();
    std::fs::write(repo.path().join("index"), "deliberately unreadable index\n").unwrap();
    assert_eq!(
        crate::app_git::head_file_id(&repo, Path::new("modified.txt"), true).unwrap(),
        Some(expected)
    );
}

#[cfg(windows)]
#[test]
fn native_status_keeps_a_configured_fsmonitor_daemon_alive() {
    let temp = fixture();
    let repo = git2::Repository::open(temp.path()).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.fsmonitor", true)
        .unwrap();
    struct StopMonitor<'a>(&'a git2::Repository);
    impl Drop for StopMonitor<'_> {
        fn drop(&mut self) {
            let _ = command(self.0)
                .unwrap()
                .args(["fsmonitor--daemon", "stop"])
                .output();
        }
    }
    let _stop = StopMonitor(&repo);
    read(&repo, true, &CancellationToken::new(), Path::to_path_buf)
        .unwrap()
        .unwrap();
    let result = command(&repo)
        .unwrap()
        .args(["fsmonitor--daemon", "status"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "the Git status process must leave Git's filesystem monitor running: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn porcelain_parser_preserves_conflicts_raw_filenames_and_rename_direction() {
    for conflict in ["DD", "AU", "UD", "UA", "DU", "AA", "UU"] {
        let bytes = format!("{conflict} conflict\0");
        let rows = parse_status(bytes.as_bytes(), Path::to_path_buf).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].status, rows[0].staged), ('!', false));
    }
    let rows = parse_status(b"RM new\nname\0old\tname\0 T type\0", Path::to_path_buf).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].path, Path::new("new\nname"));
    assert_eq!(
        rows[0].original_path.as_deref(),
        Some(Path::new("old\tname"))
    );
    assert_eq!((rows[1].status, rows[1].staged), ('M', false));
    assert_eq!((rows[2].status, rows[2].staged), ('M', false));
    for invalid in [
        b"M file\0".as_slice(),
        b" M missing terminator",
        b"R  new\0",
        b"ZZ file\0",
    ] {
        assert!(parse_status(invalid, Path::to_path_buf).is_err());
    }
}

#[cfg(unix)]
#[test]
fn porcelain_paths_keep_non_utf8_bytes_for_remote_snapshots() {
    use std::os::unix::ffi::OsStrExt;
    let files = parse_status(b" M raw-\xff\0", Path::to_path_buf).unwrap();
    assert_eq!(files[0].path.as_os_str().as_bytes(), b"raw-\xff");
}

#[test]
fn missing_git_can_fall_back_and_precancelled_status_does_not_spawn() {
    let mut command = Command::new("vtcode-nonexistent-git-status-command");
    assert!(
        run_output(&mut command, &CancellationToken::new())
            .unwrap()
            .is_none()
    );
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        run_output(&mut command, &cancel).unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
}

#[test]
fn status_cancellation_child() {
    if std::env::var_os("VTCODE_STATUS_CANCELLATION_CHILD").is_none() {
        return;
    }
    let block = "x".repeat(8192);
    loop {
        println!("{block}");
        eprintln!("{block}");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn cancelling_a_running_status_drains_both_pipes_and_reaps_the_process() {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "git_status::tests::status_cancellation_child",
            "--nocapture",
        ])
        .env("VTCODE_STATUS_CANCELLATION_CHILD", "1");
    let cancel = CancellationToken::new();
    let signal = cancel.clone();
    let thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        signal.cancel();
    });
    let start = Instant::now();
    assert_eq!(
        run_output(&mut command, &cancel).unwrap_err().kind(),
        io::ErrorKind::Interrupted
    );
    thread.join().unwrap();
    assert!(start.elapsed() < Duration::from_secs(5));
}

#[test]
#[ignore = "read-only benchmark: set VTCODE_GIT_BENCH_REPO to an existing repository"]
fn benchmark_source_control_repository() {
    let root =
        PathBuf::from(std::env::var_os("VTCODE_GIT_BENCH_REPO").expect("VTCODE_GIT_BENCH_REPO"));
    let cancel = CancellationToken::new();
    for sample in 0..3 {
        let repo = git2::Repository::open(&root).unwrap();
        let entries = repo.index().unwrap().len();
        let mut options = git2::StatusOptions::new();
        options
            .include_untracked(true)
            .recurse_untracked_dirs(false)
            .renames_head_to_index(true);
        crate::app_git::ignore_working_copies(&repo).unwrap();
        let start = Instant::now();
        let old = repo.statuses(Some(&mut options)).unwrap().len();
        let bundled = start.elapsed();
        let start = Instant::now();
        let native = read(&repo, true, &cancel, Path::to_path_buf)
            .unwrap()
            .unwrap();
        let optimized = start.elapsed();
        let start = Instant::now();
        let discovery = crate::git_repositories::discover(&root, &cancel);
        assert!(discovery.errors.is_empty(), "{:?}", discovery.errors);
        let discovery_time = start.elapsed();
        eprintln!(
            "sample={sample} indexed={entries} libgit2={bundled:?} native={optimized:?} discovery={discovery_time:?} repositories={} status_entries={old}/{}",
            discovery.roots.len(),
            native.len()
        );
    }
    let mut app = crate::app::App::test_new();
    app.explorer = crate::explorer::FileExplorer::new_deferred(root);
    app.workspace_watcher_active = true;
    let start = Instant::now();
    app.refresh_source_control_async();
    app.wait_for_background(Duration::from_secs(30), |app| {
        app.source_control_repository_state == crate::app::SourceControlRepositoryState::Available
    });
    let ready = start.elapsed();
    app.wait_for_background(Duration::from_secs(30), |app| {
        !app.bg_git_status_running && !app.bg_git_status_pending
    });
    eprintln!(
        "startup first_snapshot={ready:?} complete_discovery_and_status={:?} repositories={}",
        start.elapsed(),
        app.repositories.entries.len()
    );
    let mut full = Vec::new();
    for _ in 0..3 {
        let start = Instant::now();
        app.refresh_source_control_async();
        app.wait_for_background(Duration::from_secs(30), |app| !app.bg_git_status_running && !app.bg_git_status_pending);
        full.push(start.elapsed());
    }
    full.sort();
    let start = Instant::now();
    for _ in 0..1_000 {
        // Bypass the preexisting two-second throttle to measure reuse of the
        // watched snapshot when panels are visited less frequently too.
        app.source_control_last_refresh = Some(Instant::now() - Duration::from_secs(3));
        app.refresh_source_control_for_navigation();
        assert!(!app.bg_git_status_running && !app.bg_git_status_pending);
    }
    eprintln!("navigation: previous_full_refresh={:?} cached_per_visit={:?}; status_workers_per_visit=1->0", full[1], start.elapsed()/1_000);
}

#[test]
fn many_entries_without_stat_data_use_gits_scanner() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let repo = git2::Repository::init(root).unwrap();
    let mut index = repo.index().unwrap();
    for number in 0..300 {
        let name = format!("file-{number:03}.txt");
        std::fs::write(root.join(&name), "base\n").unwrap();
        index.add_path(Path::new(&name)).unwrap();
    }
    index.write().unwrap();
    let base = repo.find_tree(index.write_tree().unwrap()).unwrap();
    for number in 0..300 {
        let name = format!("file-{number:03}.txt");
        std::fs::write(root.join(&name), "changed contents
").unwrap();
        index.add_path(Path::new(&name)).unwrap();
    }
    index.write().unwrap();
    assert!(!prefer_native(&repo), "freshly staged entries carry stat data");
    // Unstaging changed paths rebuilds their entries without stat data.
    index.read_tree(&base).unwrap();
    index.write().unwrap();
    assert!(prefer_native(&repo));
}
