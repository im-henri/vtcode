use super::*;
use crate::{
    app::SourceControlRepositoryState as State,
    background::{BackgroundResult, CancellationToken},
};

fn repository(root: &Path, branch: &str) -> git2::Repository {
    let mut options = git2::RepositoryInitOptions::new();
    options.initial_head(branch);
    let repo = git2::Repository::init_opts(root, &options).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", false)
        .unwrap();
    repo
}

fn query() -> git::StatusQuery {
    git::StatusQuery {
        known: None,
        parent_open: false,
        closed: Vec::new(),
        include_untracked: true,
        changed: None,
        revalidate: false,
    }
}

fn options() -> Option<git::StatusProgressOptions> {
    Some(git::StatusProgressOptions { preferred: None })
}

#[test]
fn remote_git_publishes_branch_before_status_and_primary_files_before_nested_discovery() {
    let dir = tempfile::tempdir().unwrap();
    repository(dir.path(), "startup-branch");
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(root.join(".git/info/exclude"), "late-project/\n").unwrap();
    crate::app_git::refresh::STATUS_READS.set(0);
    let mut phases = 0;
    let reply = git::status_reporting(
        &root,
        query(),
        options(),
        &CancellationToken::new(),
        |progress| {
            match progress {
                git::StatusProgress::Repository { root: path, branch } => {
                    assert_eq!(phases, 0);
                    assert_eq!(path, RemotePath::default());
                    assert_eq!(branch, "startup-branch");
                    assert_eq!(crate::app_git::refresh::STATUS_READS.get(), 0);
                    std::fs::write(root.join("from-header.txt"), "visible\n").unwrap();
                }
                git::StatusProgress::Snapshot { discovery, scan } => {
                    assert_eq!(phases, 1);
                    assert_eq!(discovery.roots, vec![RemotePath::default()]);
                    assert_eq!(scan.branch, "startup-branch");
                    assert!(
                        scan.files
                            .iter()
                            .any(|file| file.path.0 == b"from-header.txt")
                    );
                    assert_eq!(crate::app_git::refresh::STATUS_READS.get(), 1);
                    repository(&root.join("late-project"), "nested-branch");
                    std::fs::write(root.join("late-project/nested.txt"), "nested\n").unwrap();
                }
            }
            phases += 1;
            Ok(())
        },
    )
    .unwrap();
    let Reply::GitStatus { discovery, scans } = reply else {
        panic!()
    };
    assert_eq!(phases, 2);
    assert_eq!(discovery.roots.len(), 2);
    assert_eq!(
        scans.len(),
        1,
        "the primary must not be read or sent a second time"
    );
    assert_eq!(scans[0].root.0, b"late-project");
    assert_eq!(crate::app_git::refresh::STATUS_READS.get(), 2);
}

#[test]
fn remote_git_cancellation_after_the_branch_skips_status_and_discovery() {
    let dir = tempfile::tempdir().unwrap();
    repository(dir.path(), "startup-branch");
    let cancel = CancellationToken::new();
    crate::app_git::refresh::STATUS_READS.set(0);
    let mut phases = 0;
    let result = git::status_reporting(dir.path(), query(), options(), &cancel, |progress| {
        assert!(matches!(progress, git::StatusProgress::Repository { .. }));
        phases += 1;
        cancel.cancel();
        Ok(())
    });
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    assert_eq!(phases, 1);
    assert_eq!(crate::app_git::refresh::STATUS_READS.get(), 0);
}

#[test]
fn remote_git_bootstrap_respects_the_restored_repository_and_missing_selections() {
    let dir = tempfile::tempdir().unwrap();
    repository(dir.path(), "root-branch");
    repository(&dir.path().join("nested"), "nested-branch");
    std::fs::write(dir.path().join(".git/info/exclude"), "nested/\n").unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    for (preferred, branch) in [("nested", "nested-branch"), ("missing", "root-branch")] {
        let mut phases = 0;
        let progress = Some(git::StatusProgressOptions {
            preferred: Some(RemotePath::new(preferred.as_bytes().to_vec()).unwrap()),
        });
        let reply = git::status_reporting(
            &root,
            query(),
            progress,
            &CancellationToken::new(),
            |progress| {
                match progress {
                    git::StatusProgress::Repository { branch: actual, .. } => {
                        assert_eq!(actual, branch)
                    }
                    git::StatusProgress::Snapshot { scan, .. } => assert_eq!(scan.branch, branch),
                }
                phases += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(phases, 2);
        assert!(matches!(reply, Reply::GitStatus { scans, .. } if scans.len() == 1));
    }
}

fn inner(result: &BackgroundResult) -> &BackgroundResult {
    match result {
        BackgroundResult::SourceControlRead { result, .. } => result,
        result => result,
    }
}

#[test]
fn ssh_source_control_shows_branch_and_files_before_completing_discovery() {
    use ratatui::{Terminal, backend::TestBackend, layout::Rect};
    for preferred in [None, Some("nested"), Some("missing")] {
        let dir = tempfile::tempdir().unwrap();
        repository(dir.path(), "startup-branch");
        repository(&dir.path().join("nested"), "nested-branch");
        std::fs::write(dir.path().join("nested/nested.txt"), "nested\n").unwrap();
        std::fs::write(dir.path().join(".git/info/exclude"), "nested/\n").unwrap();
        std::fs::write(dir.path().join("file.txt"), "visible\n").unwrap();
        let workspace =
            workspace::Workspace::new(connection(dir.path(), Arc::default(), Arc::default()));
        let (expected_root, branch, file) = if preferred == Some("nested") {
            (
                workspace.target_key("nested").unwrap(),
                "nested-branch",
                "nested.txt",
            )
        } else {
            (workspace.view_root.clone(), "startup-branch", "file.txt")
        };
        let mut app = app_for_workspace(&workspace);
        app.repositories.preferred = preferred.map(|path| workspace.target_key(path).unwrap());
        app.source_control_enabled = true;
        app.refresh_source_control_async();
        let found = app
            .background
            .receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        let initial = app
            .background
            .receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        let complete = app
            .background
            .receiver
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
        assert!(
            matches!(inner(&found), BackgroundResult::GitRepositoryFound { repository, branch: actual_branch, .. }
        if repository == &expected_root && actual_branch == branch)
        );
        assert!(
            matches!(inner(&initial), BackgroundResult::GitRepositories { complete: false, scans, .. }
        if scans.len() == 1 && scans[0].files.len() == 1)
        );
        assert!(
            matches!(inner(&complete), BackgroundResult::GitRepositories { complete: true, scans, .. } if scans.len() == 1)
        );
        app.background.sender.send(found).unwrap();
        app.poll_background_results();
        assert_eq!(app.source_control_repository_state, State::Loading);
        assert_eq!(app.cached_git_branch, branch);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                crate::ui_source_control::draw_source_control_sidebar(
                    frame,
                    &mut app,
                    Rect::new(0, 0, 80, 24),
                )
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains(branch) && text.contains("Loading Git changes"));
        app.background.sender.send(initial).unwrap();
        app.poll_background_results();
        assert_eq!(app.source_control_repository_state, State::Available);
        assert!(app.bg_git_status_running && !app.repositories.discovered);
        assert_eq!(app.source_control_files[0].path, file);
        app.background.sender.send(complete).unwrap();
        app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_status_running);
        assert_eq!(app.source_control_files[0].path, file);
        assert_eq!(app.cached_git_branch, branch);
    }
}

#[test]
fn streamed_primary_snapshot_is_cached_and_regular_navigation_needs_no_second_rpc() {
    let dir = tempfile::tempdir().unwrap();
    repository(dir.path(), "startup-branch");
    std::fs::write(dir.path().join("file.txt"), "visible\n").unwrap();
    let traffic = Arc::new(AtomicUsize::new(0));
    let workspace =
        workspace::Workspace::new(connection(dir.path(), Arc::default(), traffic.clone()));
    let cancel = CancellationToken::new();
    let mut snapshots = 0;
    let mut received = None;
    let (discovery, scans) = workspace
        .git_snapshot_progress(query(), options(), &cancel, |update| {
            if let git::StatusUpdate::Snapshot { scan, .. } = update {
                received = Some(Instant::now());
                assert_eq!(scan.files[0].path, "file.txt");
                snapshots += 1;
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(snapshots, 1);
    assert!(scans.is_empty());
    assert!(
        workspace
            .git_snapshots
            .lock()
            .unwrap()
            .get(&RemotePath::default())
            .unwrap()
            .checked
            <= received.unwrap(),
        "finishing nested discovery must not extend the primary snapshot's freshness deadline"
    );
    let requests = traffic.load(Ordering::Relaxed);
    let known = git::Discovery {
        roots: discovery
            .roots
            .iter()
            .map(|path| workspace.remote_path(path).unwrap())
            .collect(),
        parent: None,
        errors: Vec::new(),
    };
    let (_, scans) = workspace
        .git_snapshot(
            true,
            Some(known),
            false,
            Vec::new(),
            Some(Vec::new()),
            &cancel,
        )
        .unwrap();
    assert_eq!(scans[0].files[0].path, "file.txt");
    assert_eq!(scans[0].branch, "startup-branch");
    assert_eq!(traffic.load(Ordering::Relaxed), requests);
}
