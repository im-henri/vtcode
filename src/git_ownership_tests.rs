use super::*;
use crate::{
    app::{App, SidebarView, SourceControlRepositoryState as State},
    background::{BackgroundResult, CancellationToken},
    git_repositories::Discovery,
};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::time::{Duration, Instant};

fn init(root: &Path) {
    std::fs::create_dir_all(root).unwrap();
    let repo = git2::Repository::init(root).unwrap();
    std::fs::write(root.join("file.txt"), "base\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("file.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Test", "test@example.test").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "Initial", &tree, &[]).unwrap();
}

fn app(root: &Path) -> App {
    let mut app = App::new_deferred(root.to_owned());
    app.source_control_enabled = true;
    app.source_control_cancel = CancellationToken::new();
    app.sidebar_view = SidebarView::SourceControl;
    app.activity_bar_selected = 2;
    app.active_pane = crate::app::ActivePane::Explorer;
    app.show_explorer = true;
    app
}

fn refresh(app: &mut App) {
    app.refresh_source_control_async();
    app.wait_for_background(Duration::from_secs(15), |app| {
        !app.bg_git_status_running && !app.bg_git_status_pending
    });
}

fn frame(app: &mut App, name: &str) -> String {
    let mut terminal = Terminal::new(TestBackend::new(90, 26)).unwrap();
    let area = Rect::new(0, 0, 90, 26);
    app.explorer_area = Some(area);
    app.loading.begin_frame();
    terminal.draw(|frame| {
        crate::ui_source_control::draw_source_control_sidebar(frame, app, area);
        crate::ui_loading::draw_sidebar(frame, app, area);
    }).unwrap();
    crate::ui_loading::finish_frame(app, Instant::now());
    crate::ui_control_visibility_tests::snapshot(terminal.backend().buffer(), name);
    terminal.backend().buffer().content.iter().map(|cell| cell.symbol()).collect()
}

fn publish(app: &mut App, discovery: Discovery) {
    app.bg_git_status_running = true;
    app.source_control_sender().send(BackgroundResult::GitRepositories {
        root: app.explorer.root.clone(), workspace_generation: app.workspace_generation,
        repository_generation: app.repositories.membership_generation, discovery, scans: Vec::new(),
    }).unwrap();
    app.poll_background_results();
}

#[test]
fn git_ownership_failure_is_not_a_missing_repository_and_stops_loading() {
    let directory = tempfile::tempdir().unwrap();
    init(directory.path());
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let cause = git2::Error::new(git2::ErrorCode::Owner, git2::ErrorClass::Config, "repository is not owned by current user");
    let failure = Failure::opening(&root, &cause).unwrap();
    assert_eq!(failure.kind, FailureKind::Ownership);
    let mut app = app(&root);
    publish(&mut app, Discovery::failed(&root, failure));
    assert_eq!(app.source_control_repository_state, State::Failed);
    assert!(!app.bg_git_status_running && !app.bg_git_log_running && !app.bg_git_branch_running);
    let text = frame(&mut app, "Source Control repository ownership failure");
    assert!(text.contains("Git repository requires trust"));
    assert!(text.contains("Trust Repository") && text.contains("Retry") && text.contains("safe.directory"));
    assert!(!text.contains("Detecting Git repository") && !text.contains("Initialize Repository"));
    assert!(app.source_control_initialize_area.is_none());
    assert!(app.loading.next_wait(true, Instant::now()).is_none());
    // Enter retries; it must never implicitly trust a repository.
    crate::ui_source_control::handle_key(&mut app, crossterm::event::KeyCode::Enter, crossterm::event::KeyModifiers::NONE);
    assert!(app.bg_git_status_running && !app.bg_git_action_running);
    app.wait_for_background(Duration::from_secs(15), |app| {
        !app.bg_git_status_running && app.source_control_repository_state == State::Available
    });
    assert!(app.repositories.errors.is_empty());
}

#[test]
fn git_ownership_repository_identity_comes_from_git_instead_of_directory_name_guesses() {
    let directory = tempfile::tempdir().unwrap();
    init(directory.path());
    let child = directory.path().join("ordinary");
    std::fs::create_dir_all(child.join("objects")).unwrap();
    std::fs::write(child.join("HEAD"), "ordinary project data").unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    assert_eq!(metadata_root(&child).map(|path| std::fs::canonicalize(path).unwrap()), Some(root));
    let bare = directory.path().join("bare");
    git2::Repository::init_bare(&bare).unwrap();
    assert_eq!(metadata_root(&bare), Some(std::fs::canonicalize(&bare).unwrap()));
}

#[test]
fn git_ownership_discovery_reports_corruption_without_scanning_below_it_and_retry_recovers() {
    let directory = tempfile::tempdir().unwrap();
    init(directory.path());
    let nested = directory.path().join("nested");
    init(&nested);
    let config = directory.path().join(".git/config");
    let original = std::fs::read(&config).unwrap();
    std::fs::write(&config, "[invalid config").unwrap();
    let discovered = crate::git_repositories::discover(directory.path(), &CancellationToken::new());
    assert_eq!(discovered.roots, [std::fs::canonicalize(directory.path()).unwrap()]);
    assert_eq!(discovered.errors.len(), 1);
    assert_eq!(discovered.errors[0].kind, FailureKind::Repository);
    let mut app = app(directory.path());
    refresh(&mut app);
    assert_eq!(app.source_control_repository_state, State::Failed);
    let text = frame(&mut app, "Source Control repository read failure");
    assert!(text.contains("Could not load Git repository") && text.contains("Retry"));
    assert!(!text.contains("Trust Repository") && !text.contains("Initialize Repository"));
    assert!(app.loading.next_wait(true, Instant::now()).is_none());
    std::fs::write(config, original).unwrap();
    RecoveryAction::Retry.run(&mut app);
    app.wait_for_background(Duration::from_secs(15), |app| {
        !app.bg_git_status_running && app.source_control_repository_state == State::Available
            && !app.bg_git_log_running && !app.bg_git_branch_running
    });
    assert!(app.repositories.errors.is_empty());
    assert!(app.repositories.entries.iter().any(|repo| repo.root == std::fs::canonicalize(&nested).unwrap()));
    assert!(!app.git_log.is_empty(), "History must restart after access recovers");
}

#[test]
fn git_ownership_status_errors_preserve_the_repository_instead_of_publishing_empty_changes() {
    let directory = tempfile::tempdir().unwrap();
    init(directory.path());
    let mut app = app(directory.path());
    refresh(&mut app);
    assert_eq!(app.source_control_repository_state, State::Available);
    let index = directory.path().join(".git/index");
    let original = std::fs::read(&index).unwrap();
    std::fs::write(&index, b"invalid index").unwrap();
    refresh(&mut app);
    assert_eq!(app.source_control_repository_state, State::Failed);
    assert_eq!(app.repositories.open_count(), 1);
    assert!(!app.repositories.errors.is_empty());
    std::fs::write(index, original).unwrap();
    refresh(&mut app);
    assert_eq!(app.source_control_repository_state, State::Available);
    assert!(app.repositories.errors.is_empty());
}

#[test]
fn git_ownership_one_failed_project_does_not_hide_healthy_repository_headers() {
    let directory = tempfile::tempdir().unwrap();
    let broken = directory.path().join("broken");
    let healthy = directory.path().join("healthy");
    init(&broken);
    init(&healthy);
    std::fs::write(broken.join(".git/config"), "[invalid config").unwrap();
    let mut app = app(directory.path());
    refresh(&mut app);
    assert_eq!(app.source_control_repository_state, State::Failed);
    frame(&mut app, "Source Control mixed repository access");
    assert_eq!(app.repositories.headers.len(), 2);
    app.select_source_control_repository(&std::fs::canonicalize(&healthy).unwrap());
    assert_eq!(app.source_control_repository_state, State::Available);
}

#[test]
fn git_ownership_stale_trust_controls_cannot_trust_a_different_repository() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first");
    let second = directory.path().join("second");
    init(&first);
    init(&second);
    let first = std::fs::canonicalize(first).unwrap();
    let second = std::fs::canonicalize(second).unwrap();
    let failure = |root| Failure { root, message: "Different owner".into(), kind: FailureKind::Ownership };
    let mut app = app(directory.path());
    publish(&mut app, Discovery {
        roots: vec![first.clone(), second.clone()], parent: None,
        workspace: directory.path().to_owned(), errors: vec![failure(first), failure(second.clone())],
    });
    frame(&mut app, "Source Control trust controls");
    let displayed = app.repositories.recovery_root.clone();
    app.select_source_control_repository(&second);
    assert_ne!(displayed, app.repositories.active);
    RecoveryAction::Trust.run(&mut app);
    assert!(!app.bg_git_action_running, "A stale Trust button must not authorize another repository");
}

#[test]
fn git_ownership_stale_failure_cannot_change_another_workspaces_loading_state() {
    let directory = tempfile::tempdir().unwrap();
    init(directory.path());
    let other = tempfile::tempdir().unwrap();
    let mut app = app(directory.path());
    let old_root = app.explorer.root.clone();
    let generation = app.workspace_generation;
    app.explorer.root = other.path().to_owned();
    app.workspace_generation += 1;
    app.bg_git_status_running = true;
    app.source_control_sender().send(BackgroundResult::GitRepositories {
        root: old_root.clone(), workspace_generation: generation,
        repository_generation: app.repositories.membership_generation,
        discovery: Discovery::failed(&old_root, Failure::reading(&old_root, "Old read failed")), scans: Vec::new(),
    }).unwrap();
    app.poll_background_results();
    assert!(app.bg_git_status_running);
    assert_eq!(app.source_control_repository_state, State::Checking);
    assert!(app.repositories.errors.is_empty());
    app.bg_git_status_running = false;
}

#[cfg(windows)]
#[test]
fn git_ownership_safe_directory_uses_git_compatible_unc_and_extended_drive_paths() {
    for (path, expected) in [
        (r"\\?\UNC\server\share\my work\repo", "%(prefix)///server/share/my work/repo"),
        (r"\\server\share\repo", "%(prefix)///server/share/repo"),
        (r"\\?\Z:\mywork\mydir", "Z:/mywork/mydir"),
        (r"Z:\mywork\mydir", "Z:/mywork/mydir"),
        (r"\\?\UNC\server\share\repo\", "%(prefix)///server/share/repo"),
        (r"Z:\mywork\mydir\", "Z:/mywork/mydir"),
        (r"\\server\share\", "%(prefix)///server/share"),
        (r"Z:\", "Z:/"),
    ] {
        assert_eq!(safe_directory_value(Path::new(path)).unwrap(), std::ffi::OsString::from(expected));
    }
}

#[cfg(unix)]
#[test]
fn git_ownership_safe_directory_preserves_native_bytes_and_refuses_trust_wildcards() {
    use std::os::unix::ffi::OsStrExt;
    let path = Path::new(std::ffi::OsStr::from_bytes(b"/project/raw-\xff\\name"));
    assert_eq!(safe_directory_value(path).unwrap(), path.as_os_str());
    assert!(safe_directory_value(Path::new("/project/*")).is_err());
    assert!(safe_directory_value(Path::new("*")).is_err());
    assert_eq!(safe_directory_value(Path::new("/project/work/")).unwrap(), std::ffi::OsStr::new("/project/work"));
    assert_eq!(safe_directory_value(Path::new("/")).unwrap(), std::ffi::OsStr::new("/"));
}

#[test]
#[ignore = "Runs in a child process against an explicitly supplied disposable, foreign-owned repository"]
fn git_ownership_fixture() {
    let root = PathBuf::from(std::env::var_os("VTCODE_GIT_OWNER_REPO").expect("disposable repository"));
    let config = PathBuf::from(std::env::var_os("GIT_CONFIG_GLOBAL").expect("isolated Git config"));
    assert_eq!(std::fs::canonicalize(git2::Config::find_global().unwrap()).unwrap(),
        std::fs::canonicalize(&config).unwrap(), "Never change the user's real Git configuration");
    let error = git2::Repository::open(&root).err().expect("Fixture must be rejected by the ownership guard");
    assert_eq!(error.code(), git2::ErrorCode::Owner, "{error}");
    println!("Reproduced ownership failure: {error}");
    let remote = std::env::var_os("VTCODE_GIT_OWNER_REMOTE").map(|_| {
        crate::remote::workspace::Workspace::new(crate::remote::tests::connection(
            &root, std::sync::Arc::default(), std::sync::Arc::default(),
        ))
    });
    let mut app = app(remote.as_ref().map_or(root.as_path(), |workspace| workspace.view_root.as_path()));
    if let Some(remote) = &remote {
        remote.client.set_wake(app.background.wake_handle());
        app.remote_workspace = Some(remote.clone());
    }
    refresh(&mut app);
    assert_eq!(app.source_control_repository_state, State::Failed);
    assert_eq!(app.repositories.failure().unwrap().kind, FailureKind::Ownership);
    let text = frame(&mut app, "Real foreign-owned repository failure");
    assert!(text.contains("Trust Repository") && !text.contains("Detecting Git repository"));
    if std::env::var_os("VTCODE_GIT_OWNER_PROBE_ONLY").is_some() { return; }
    RecoveryAction::Trust.run(&mut app);
    let diagnostic = std::cell::RefCell::new(String::new());
    app.wait_for_background(Duration::from_secs(30), |app| {
        let current = format!("state={:?}; action={}; status={}/{}; history={}; branch={}; active={:?}; errors={:?}; notices={:?}",
            app.source_control_repository_state, app.bg_git_action_running, app.bg_git_status_running,
            app.bg_git_status_pending, app.bg_git_log_running, app.bg_git_branch_running,
            app.repositories.active, app.repositories.errors, app.notification_history);
        if *diagnostic.borrow() != current {
            println!("{current}");
            *diagnostic.borrow_mut() = current;
        }
        !app.bg_git_action_running && !app.bg_git_status_running && !app.bg_git_status_pending
            && app.source_control_repository_state == State::Available
            && !app.bg_git_log_running && !app.bg_git_branch_running
    });
    git2::Repository::open(&root).unwrap();
    let status = std::process::Command::new("git").arg("--no-optional-locks")
        .arg("-C").arg(&root).args(["status", "--porcelain"]).output().unwrap();
    assert!(status.status.success(), "Git CLI must accept the same trust entry: {}", String::from_utf8_lossy(&status.stderr));
    let global = git2::Config::open(&config).unwrap();
    let mut entries = global.multivar("safe.directory", None).unwrap();
    let entry = entries.next().unwrap().unwrap();
    assert_ne!(entry.value(), Some("*"));
    assert!(entries.next().is_none(), "Trust must add one repository, not a broad allowlist");
    assert!(!app.git_log.is_empty());
    frame(&mut app, "Recovered foreign-owned repository");
    assert!(app.loading.next_wait(true, Instant::now()).is_none());
    if let Some(remote) = remote { remote.client.call(crate::remote::Command::Shutdown).unwrap(); }
    println!("Recovered status and history with one repository-specific trust entry");
}
