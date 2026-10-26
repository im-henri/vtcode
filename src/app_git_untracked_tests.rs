use super::*;
use crate::source_control_rows::Visibility;

fn visibility(app: &mut App, untracked: bool) {
    app.set_source_control_visibility(Visibility { untracked });
}

fn status(root: &Path, include_untracked: bool) -> Vec<GitFileChange> {
    collect_source_control_status_with_paths(
        root.to_owned(),
        || {},
        &CancellationToken::new(),
        include_untracked,
        |path| path.to_string_lossy().into_owned(),
    )
    .files
}

fn tracked_only(files: &[GitFileChange]) -> bool {
    files.iter().all(|file| file.staged || file.status != '?')
}

#[test]
fn hidden_untracked_status_preserves_staged_additions_renames_and_worktree_changes() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    init(&root);
    let repo = git2::Repository::open(&root).unwrap();
    std::fs::rename(root.join("first.txt"), root.join("renamed.txt")).unwrap();
    std::fs::write(root.join("added.txt"), "staged addition\n").unwrap();
    let mut index = repo.index().unwrap();
    index.remove_path(Path::new("first.txt")).unwrap();
    for path in ["renamed.txt", "added.txt"] {
        index.add_path(Path::new(path)).unwrap();
    }
    index.write().unwrap();
    std::fs::write(root.join("renamed.txt"), "modified after staging\n").unwrap();
    std::fs::remove_file(root.join("other.txt")).unwrap();
    std::fs::create_dir_all(root.join("generated/deep")).unwrap();
    std::fs::write(root.join("generated/deep/output.bin"), "untracked\n").unwrap();

    let visible = status(&root, true);
    let hidden = status(&root, false);
    assert_eq!(visible.len(), hidden.len() + 1);
    assert!(tracked_only(&hidden));
    assert_eq!(
        key(&hidden),
        key(&visible
            .into_iter()
            .filter(|file| file.status != '?')
            .collect::<Vec<_>>())
    );
    for (path, marker, staged) in [
        ("added.txt", 'A', true),
        ("renamed.txt", 'R', true),
        ("renamed.txt", 'M', false),
        ("other.txt", 'D', false),
    ] {
        assert!(
            hidden
                .iter()
                .any(|file| file.path == path && file.status == marker && file.staged == staged)
        );
    }
    assert_eq!(
        hidden
            .iter()
            .find(|file| file.status == 'R')
            .unwrap()
            .original_path
            .as_deref(),
        Some("first.txt")
    );

    let mut discovery = Discovery {
        roots: vec![root.clone()],
        parent: None,
        workspace: root.clone(),
    };
    let scans = collect(
        &mut discovery,
        vec![root.clone()],
        Some(
            [root.join("renamed.txt"), root.join("generated")]
                .into_iter()
                .collect(),
        ),
        &root,
        &CancellationToken::new(),
        false,
    );
    assert_eq!(scans.len(), 1);
    assert!(!scans[0].include_untracked);
    assert!(scans[0].worktree_paths.is_some());
    assert_eq!(scans[0].files.len(), 1);
    assert_eq!(
        (scans[0].files[0].path.as_str(), scans[0].files[0].status),
        ("renamed.txt", 'M')
    );
}

#[test]
fn persisted_hidden_preference_is_lazy_and_applies_to_initial_discovery() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    init(&root);
    std::fs::write(root.join("first.txt"), "modified\n").unwrap();
    std::fs::write(root.join("untracked.txt"), "new\n").unwrap();
    let mut app = app_at(&root);
    app.apply_settings(&crate::settings::Settings {
        source_control_show_untracked: Some(false),
        ..Default::default()
    });
    assert!(
        !app.bg_git_status_running,
        "Applying startup settings must not start Git work"
    );
    full(&mut app);
    assert_eq!(key(&app.source_control_files), key(&status(&root, false)));
    assert_eq!(app.source_control_change_count(), 1);
    assert_eq!(app.repositories.entries[0].counts, [0, 0, 1, 0]);
}

#[test]
fn enabling_untracked_rescans_every_repository_after_scoped_hidden_updates() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let roots = [root.join("left"), root.join("right")];
    for repo in &roots {
        init(repo);
    }
    let mut app = app_at(&root);
    visibility(&mut app, false);
    full(&mut app);
    for repo in &roots {
        std::fs::write(repo.join("first.txt"), "modified\n").unwrap();
        std::fs::write(repo.join("new.txt"), "unreported new file\n").unwrap();
    }
    changed(&mut app, [roots[0].join("first.txt")]);
    assert!(roots.iter().all(|root| tracked_only(files(&app, root))));
    assert!(
        files(&app, &roots[1]).is_empty(),
        "The scoped hidden scan must remain scoped"
    );
    visibility(&mut app, true);
    assert!(app.bg_git_status_running);
    settle(&mut app);
    for repo in &roots {
        assert_matches_full(&app, repo);
    }
    assert_eq!(app.source_control_change_count(), 4);

    // A later metadata/manual scan still keeps newly staged files when hidden.
    visibility(&mut app, false);
    let repo = git2::Repository::open(&roots[1]).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("new.txt")).unwrap();
    index.write().unwrap();
    changed(&mut app, [roots[1].join(".git/index")]);
    for repo in &roots {
        assert_eq!(key(files(&app, repo)), key(&status(repo, false)));
    }
    assert!(
        files(&app, &roots[1])
            .iter()
            .any(|file| file.path == "new.txt" && file.staged)
    );
}

#[test]
fn hiding_prunes_repository_caches_and_selections_without_io_or_losing_drafts() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let roots = [root.join("left"), root.join("right")];
    for repo in &roots {
        init(repo);
        std::fs::write(repo.join("first.txt"), "modified\n").unwrap();
        std::fs::write(repo.join("new.txt"), "new\n").unwrap();
    }
    let mut app = app_at(&root);
    full(&mut app);
    for (index, repo) in roots.iter().enumerate() {
        app.select_source_control_repository(repo);
        app.source_control_commit_msg = format!("draft {index}");
        let untracked = app
            .source_control_files
            .iter()
            .position(|file| file.status == '?')
            .unwrap();
        app.select_source_control_file(untracked, false, false);
    }
    visibility(&mut app, false);
    assert!(
        !app.bg_git_status_running,
        "Hiding must not launch a replacement scan"
    );
    for (index, repo) in roots.iter().enumerate() {
        assert_eq!(files(&app, repo).len(), 1);
        assert!(tracked_only(files(&app, repo)));
        app.select_source_control_repository(repo);
        assert_eq!(app.source_control_commit_msg, format!("draft {index}"));
        assert!(
            app.selected_source_control_changes()
                .iter()
                .all(|file| file.status != '?')
        );
    }
    assert!(
        app.repositories
            .entries
            .iter()
            .all(|repo| repo.counts == [0, 0, 1, 0])
    );
    assert_eq!(app.source_control_change_count(), 2);
}

fn completed_status(app: &App) -> crate::background::BackgroundResult {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let result = app
            .background
            .receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        if let crate::background::BackgroundResult::SourceControlRead { result: inner, .. } =
            &result
            && let crate::background::BackgroundResult::GitRepositories { .. } = &**inner
        {
            return result;
        }
        // Consume the optional discovery announcement, not the final scan.
        assert!(Instant::now() < deadline);
    }
}

#[test]
fn enabling_while_a_tracked_only_scan_is_running_rejects_the_incomplete_reply() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    init(&root);
    let mut app = app_at(&root);
    visibility(&mut app, false);
    full(&mut app);
    std::fs::write(root.join("new.txt"), "new\n").unwrap();
    app.refresh_source_control_changed_paths([root.join("first.txt")]);
    let old = completed_status(&app);
    visibility(&mut app, true);
    assert!(app.bg_git_status_pending);
    assert!(
        app.pending_git_status_paths.is_none(),
        "Enabling requires a complete refresh"
    );
    app.background.sender.send(old).unwrap();
    settle(&mut app);
    assert_matches_full(&app, &root);
    assert_eq!(app.source_control_change_count(), 1);
}

#[test]
fn hiding_while_a_full_scan_is_running_filters_its_reply_without_rescanning() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    init(&root);
    std::fs::write(root.join("first.txt"), "modified\n").unwrap();
    std::fs::write(root.join("new.txt"), "new\n").unwrap();
    let mut app = app_at(&root);
    app.refresh_source_control_async();
    let old = completed_status(&app);
    // Changing this file without reporting an event distinguishes reuse of
    // the pending full result from an unnecessary replacement scan.
    std::fs::write(root.join("first.txt"), "first\n").unwrap();
    visibility(&mut app, false);
    assert!(!app.bg_git_status_pending);
    app.background.sender.send(old).unwrap();
    settle(&mut app);
    assert_eq!(app.source_control_files.len(), 1);
    assert_eq!(app.source_control_files[0].status, 'M');
    assert!(tracked_only(&app.source_control_files));
    changed(&mut app, [root.join("first.txt")]);
    assert!(app.source_control_files.is_empty());
}

#[test]
fn rapid_visibility_changes_and_disabled_source_control_do_not_leave_stale_rows() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    init(&root);
    std::fs::write(root.join("new.txt"), "new\n").unwrap();
    let mut app = app_at(&root);
    full(&mut app);
    visibility(&mut app, false);
    visibility(&mut app, true);
    visibility(&mut app, false);
    settle(&mut app);
    assert!(app.source_control_files.is_empty());
    app.set_source_control_enabled(false);
    visibility(&mut app, true);
    assert!(!app.bg_git_status_running);
    app.set_source_control_enabled(true);
    settle(&mut app);
    assert_matches_full(&app, &root);
}

#[test]
#[ignore = "manual optimized benchmark: untracked enumeration enabled versus disabled"]
fn benchmark_hidden_untracked_status() {
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temp.path()).unwrap();
    let roots: Vec<_> = (0..4)
        .map(|index| root.join(format!("project-{index}")))
        .collect();
    for repo in &roots {
        init(repo);
        std::fs::write(repo.join("first.txt"), "modified\n").unwrap();
        std::fs::create_dir(repo.join("generated")).unwrap();
        for index in 0..1500 {
            std::fs::write(repo.join(format!("generated/file-{index:04}")), "output\n").unwrap();
        }
    }
    let mut timings = [Vec::new(), Vec::new()];
    let cancel = CancellationToken::new();
    for sample in 0..9 {
        for hidden in [sample % 2 == 0, sample % 2 != 0] {
            let mut discovery = Discovery {
                roots: roots.clone(),
                parent: None,
                workspace: root.clone(),
            };
            let start = Instant::now();
            let scans = collect(&mut discovery, roots.clone(), None, &root, &cancel, !hidden);
            let elapsed = start.elapsed();
            assert_eq!(scans.len(), 4);
            assert_eq!(
                scans.iter().map(|scan| scan.files.len()).sum::<usize>(),
                if hidden { 4 } else { 6004 }
            );
            for scan in &scans {
                assert_eq!(
                    scan.files.iter().filter(|file| file.status == 'M').count(),
                    1
                );
            }
            std::hint::black_box(&scans);
            if sample > 1 {
                timings[usize::from(hidden)].push(elapsed);
            }
        }
    }
    for times in &mut timings {
        times.sort();
    }
    eprintln!(
        "full local Git status, 4 repos / 6,000 generated files: visible={:?}, hidden={:?}; status rows=6004->4",
        timings[0][timings[0].len() / 2],
        timings[1][timings[1].len() / 2]
    );
}
