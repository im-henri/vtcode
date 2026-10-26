use super::*;
use crate::{
    app::{App, SidebarView, SourceControlRepositoryState as State},
    background::{BackgroundResult, CancellationToken},
};
use std::time::{Duration, Instant};

#[path = "git_repository_reopen_tests.rs"]
mod reopen;

fn init(root: &Path, text: &str) {
    std::fs::create_dir_all(root).unwrap();
    let repo = git2::Repository::init(root).unwrap();
    let mut config = repo.config().unwrap();
    config.set_bool("core.autocrlf", false).unwrap();
    config.set_str("user.name", "Test").unwrap();
    config.set_str("user.email", "test@example.com").unwrap();
    std::fs::write(root.join("same.txt"), text).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("same.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = repo.signature().unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "Initial", &tree, &[])
        .unwrap();
}

fn app_at(root: &Path) -> App {
    let mut app = App::test_new();
    app.explorer = crate::explorer::FileExplorer::new_deferred(root.to_owned());
    app.show_explorer = true;
    app.sidebar_view = SidebarView::SourceControl;
    app.active_pane = crate::app::ActivePane::Explorer;
    app
}

fn wait(app: &mut App, ready: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        app.poll_background_results();
        if ready(app) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "background work did not complete: {:?}",
            app.source_control_repository_state
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn refresh(app: &mut App) {
    app.refresh_source_control_async();
    wait(app, |app| {
        app.repositories.discovered && !app.bg_git_status_running && !app.bg_git_status_pending
    });
}

fn draw(app: &mut App, width: u16, height: u16) -> String {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    app.explorer_area = Some(Rect::new(0, 0, width, height));
    app.text_input_regions.clear();
    terminal
        .draw(|frame| {
            crate::ui_source_control::draw_source_control_sidebar(frame, app, frame.area())
        })
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn click(app: &mut App, area: Rect) {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    crate::mouse::handle_mouse(
        app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        },
    );
}

fn index_text(root: &Path) -> String {
    let repo = git2::Repository::open(root).unwrap();
    let entry = repo
        .index()
        .unwrap()
        .get_path(Path::new("same.txt"), 0)
        .unwrap();
    String::from_utf8(repo.find_blob(entry.id).unwrap().content().to_vec()).unwrap()
}

#[test]
fn parent_repository_requires_explicit_open_and_keeps_the_workspace_folder() {
    let temp = tempfile::tempdir().unwrap();
    let parent = canonical(temp.path());
    init(&parent, "outside workspace\n");
    let workspace = parent.join("src/deep");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(parent.join("outside.txt"), "must not appear by default").unwrap();
    let mut app = app_at(&workspace);
    refresh(&mut app);
    assert_eq!(app.repositories.parent.as_ref(), Some(&parent));
    assert_eq!(app.source_control_repository_state, State::Missing);
    assert!(app.repositories.active.is_none());
    assert!(app.source_control_files.is_empty());
    assert_eq!(app.source_control_change_count(), 0);
    let screen = draw(&mut app, 64, 24);
    assert!(screen.contains("Git repository found in the parent folder."));
    assert!(!screen.contains("outside.txt"));
    let button = app.repositories.parent_button.unwrap();
    click(&mut app, button);
    wait(&mut app, |app| {
        !app.bg_git_status_running && app.source_control_repository_state == State::Available
    });
    assert_eq!(app.source_control_root(), parent);
    assert_eq!(app.explorer.root, workspace);
    assert!(
        app.source_control_files
            .iter()
            .any(|file| file.path == "outside.txt")
    );
}

#[test]
fn discovery_finds_nested_ignored_repositories_and_git_files_without_entering_git_objects() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = canonical(temp.path()).join("workspace");
    init(&workspace, "outer\n");
    std::fs::write(workspace.join(".gitignore"), "ignored/\ngenerated/\n").unwrap();
    let nested = workspace.join("src/deep/project");
    let ignored = workspace.join("ignored");
    init(&nested, "nested\n");
    init(&ignored, "ignored project\n");
    // A fake repository inside an ignored generated tree must not be traversed.
    let generated = workspace.join("generated/cache/project");
    init(&generated, "generated\n");
    let linked = workspace.join("linked");
    std::fs::create_dir_all(&linked).unwrap();
    let metadata = canonical(temp.path()).join("metadata");
    init(&metadata, "linked\n");
    let repo = git2::Repository::open(&metadata).unwrap();
    repo.config()
        .unwrap()
        .set_str("core.worktree", linked.to_str().unwrap())
        .unwrap();
    std::fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", repo.path().display()),
    )
    .unwrap();
    let result = discover(&workspace, &CancellationToken::new());
    assert_eq!(
        result.roots,
        vec![workspace.clone(), ignored, linked, nested]
    );
    assert!(result.parent.is_none());
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(discover(&workspace, &cancelled).roots.is_empty());
}

struct Fixture {
    app: App,
    _directory: tempfile::TempDir,
    root: PathBuf,
    nested: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = canonical(directory.path());
        init(&root, "outer before\n");
        let nested = root.join("nested");
        init(&nested, "inner before\n");
        std::fs::write(root.join(".git/info/exclude"), "nested/\n").unwrap();
        std::fs::write(root.join("same.txt"), "outer after\n").unwrap();
        std::fs::write(nested.join("same.txt"), "inner after\n").unwrap();
        let mut app = app_at(&root);
        refresh(&mut app);
        assert_eq!(app.repositories.entries.len(), 2);
        Self {
            app,
            _directory: directory,
            root,
            nested,
        }
    }
}

#[test]
fn repository_switches_preserve_drafts_selection_and_input_history_without_changing_files() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    assert_eq!(app.source_control_root(), fixture.root);
    app.source_control_commit_msg = "outer draft".into();
    app.source_control_commit_cursor = 4;
    app.source_control_commit_selection_anchor = Some(1);
    app.commit_input.scroll = 3;
    app.sc_changes_collapsed = true;
    app.select_source_control_repository(&fixture.nested);
    assert_eq!(app.source_control_root(), fixture.nested);
    assert!(app.source_control_commit_msg.is_empty());
    assert_eq!(app.commit_input.scroll, 0);
    app.source_control_commit_msg = "inner draft".into();
    app.source_control_commit_cursor = 7;
    app.commit_input.scroll = 1;
    app.select_source_control_repository(&fixture.root);
    assert_eq!(app.source_control_commit_msg, "outer draft");
    assert_eq!(app.source_control_commit_cursor, 4);
    assert_eq!(app.source_control_commit_selection_anchor, Some(1));
    assert_eq!(app.commit_input.scroll, 3);
    assert!(app.sc_changes_collapsed);
    app.select_source_control_repository(&fixture.nested);
    assert_eq!(app.source_control_commit_msg, "inner draft");
    assert_eq!(app.source_control_commit_cursor, 7);
    assert_eq!(app.commit_input.scroll, 1);
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("same.txt")).unwrap(),
        "outer after\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.nested.join("same.txt")).unwrap(),
        "inner after\n"
    );
}

#[test]
fn staging_and_committing_same_named_files_is_scoped_to_the_selected_repository() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    app.select_source_control_repository(&fixture.nested);
    let index = app
        .source_control_files
        .iter()
        .position(|file| file.path == "same.txt" && !file.staged)
        .unwrap();
    app.git_stage_file(index);
    wait(app, |app| {
        !app.bg_git_action_running && !app.bg_git_status_running
    });
    assert_eq!(index_text(&fixture.nested), "inner after\n");
    assert_eq!(index_text(&fixture.root), "outer before\n");
    app.source_control_commit_msg = "Only the inner repository".into();
    app.git_commit();
    wait(app, |app| {
        !app.bg_git_action_running && !app.bg_git_status_running
    });
    let inner = git2::Repository::open(&fixture.nested).unwrap();
    let outer = git2::Repository::open(&fixture.root).unwrap();
    assert_eq!(
        inner.head().unwrap().peel_to_commit().unwrap().message(),
        Some("Only the inner repository")
    );
    assert_eq!(
        outer.head().unwrap().peel_to_commit().unwrap().message(),
        Some("Initial")
    );
}

#[test]
fn nested_diff_uses_its_repository_even_after_the_source_control_selection_changes() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    app.select_source_control_repository(&fixture.nested);
    let index = app
        .source_control_files
        .iter()
        .position(|file| file.path == "same.txt" && !file.staged)
        .unwrap();
    app.open_source_control_diff(index);
    wait(app, |app| {
        app.pending_git_compare_path.is_none() && !app.bg_git_compare_running
    });
    assert_eq!(
        app.editor().unwrap().file_path.as_deref(),
        Some(fixture.nested.join("same.txt").as_path())
    );
    assert_eq!(app.editor().unwrap().rope.to_string(), "inner after\n");
    app.select_source_control_repository(&fixture.root);
    app.refresh_source_control_diffs();
    wait(app, |app| !app.diff_tabs.refresh_running);
    assert_eq!(app.editor().unwrap().rope.to_string(), "inner after\n");
}

#[test]
fn repository_headers_close_and_reopen_independently_and_counts_honor_untracked_filter() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    std::fs::write(fixture.nested.join("new.txt"), "new\n").unwrap();
    refresh(app);
    assert_eq!(app.source_control_change_count(), 3);
    app.set_source_control_visibility(crate::source_control_rows::Visibility { untracked: false });
    assert_eq!(app.source_control_change_count(), 2);
    for width in [20, 40, 80] {
        draw(app, width, 28);
        assert_eq!(app.repositories.headers.len(), 2);
        assert_eq!(app.repositories.closes.len(), 2);
    }
    let close = app
        .repositories
        .closes
        .iter()
        .find(|(_, root)| root == &fixture.nested)
        .unwrap()
        .0;
    click(app, close);
    wait(app, |app| !app.bg_git_status_running);
    assert_eq!(app.source_control_change_count(), 1);
    assert!(app.repositories.closed.contains(&fixture.nested));
    assert!(!draw(app, 60, 28).contains("nested"));
    let saved = serde_json::to_string(&app.saved_repositories()).unwrap();
    let mut restored = app_at(&fixture.root);
    restored.restore_repositories(serde_json::from_str(&saved).unwrap());
    refresh(&mut restored);
    assert!(restored.repositories.closed.contains(&fixture.nested));
    restored.reopen_source_control_repositories();
    wait(&mut restored, |app| !app.bg_git_status_running);
    assert!(restored.repositories.closed.is_empty());
    assert_eq!(restored.repositories.entries.len(), 2);
}

#[test]
fn stale_repository_reads_and_commit_completion_cannot_modify_another_repository() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    let old = app.repository_sender();
    app.source_control_commit_msg = "outer commit".into();
    app.select_source_control_repository(&fixture.nested);
    app.source_control_commit_msg = "inner draft".into();
    old.send(BackgroundResult::GitBranch("must not appear".into()))
        .unwrap();
    app.poll_background_results();
    assert_ne!(app.cached_git_branch, "must not appear");
    app.finish_repository_commit(
        &crate::app_git::GitCommitRequest {
            root: fixture.root.clone(),
            message: "outer commit".into(),
        },
        false,
    );
    assert_eq!(app.source_control_commit_msg, "inner draft");
    app.select_source_control_repository(&fixture.root);
    assert!(app.source_control_commit_msg.is_empty());
    old.send(BackgroundResult::GitBranch("old generation".into()))
        .unwrap();
    app.poll_background_results();
    assert_ne!(app.cached_git_branch, "old generation");
}

#[test]
fn deleting_a_nested_git_directory_and_refreshing_removes_the_repository() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    app.select_source_control_repository(&fixture.nested);
    app.clear_git_history();
    wait(app, |app| !app.bg_git_branch_running);
    std::fs::remove_dir_all(fixture.nested.join(".git")).unwrap();
    app.repositories.rescan = true;
    refresh(app);
    assert_eq!(app.repositories.entries.len(), 1);
    assert_eq!(app.source_control_root(), fixture.root);
    assert_eq!(app.source_control_repository_state, State::Available);
}

#[test]
fn closed_repositories_stay_closed_during_refresh_without_loading_flicker() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    app.close_source_control_repository(&fixture.nested);
    wait(app, |app| !app.bg_git_status_running);
    app.close_source_control_repository(&fixture.root);
    wait(app, |app| !app.bg_git_status_running);
    assert_eq!(app.source_control_repository_state, State::Missing);
    assert!(app.source_control_files.is_empty());
    for _ in 0..3 {
        app.refresh_source_control_async();
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.bg_git_status_running {
            app.poll_background_results();
            assert_eq!(app.source_control_repository_state, State::Missing);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    assert_eq!(app.source_control_change_count(), 0);
}

#[test]
fn repository_actions_cannot_implicitly_stage_a_parent_before_discovery() {
    let temp = tempfile::tempdir().unwrap();
    let parent = canonical(temp.path());
    init(&parent, "before\n");
    std::fs::write(parent.join("same.txt"), "do not stage\n").unwrap();
    let workspace = parent.join("subfolder");
    std::fs::create_dir(&workspace).unwrap();
    let mut app = app_at(&workspace);
    app.git_stage_all();
    wait(&mut app, |app| {
        !app.bg_git_action_running && !app.bg_git_status_running
    });
    assert_eq!(index_text(&parent), "before\n");
    assert!(app.repositories.active.is_none());
}

#[test]
fn discard_and_stage_all_commit_confirmations_cannot_follow_repository_switches() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    app.select_source_control_repository(&fixture.nested);
    app.request_source_control_discard_all();
    app.select_source_control_repository(&fixture.root);
    app.execute_source_control_discard_confirm(true);
    assert!(!app.bg_git_action_running);
    assert_eq!(
        std::fs::read_to_string(fixture.nested.join("same.txt")).unwrap(),
        "inner after\n"
    );
    assert_eq!(
        std::fs::read_to_string(fixture.root.join("same.txt")).unwrap(),
        "outer after\n"
    );
    app.select_source_control_repository(&fixture.nested);
    app.source_control_commit_msg = "must not follow selection".into();
    app.git_commit();
    assert!(app.sc_stage_all_commit_request.is_some());
    app.select_source_control_repository(&fixture.root);
    app.confirm_stage_all_commit();
    assert!(!app.bg_git_action_running);
    assert_eq!(index_text(&fixture.nested), "inner before\n");
    assert_eq!(index_text(&fixture.root), "outer before\n");
}

#[test]
fn ordinary_status_refresh_reuses_discovery_and_retains_branch_sync_information() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    wait(app, |app| !app.bg_git_branch_running);
    let branch = app.cached_git_branch.clone();
    app.repository_sender()
        .send(BackgroundResult::GitBranch(format!("{branch} ↑2 ↓1")))
        .unwrap();
    app.poll_background_results();
    let extra = fixture.root.join("another");
    init(&extra, "new repository\n");
    refresh(app);
    assert_eq!(
        app.repositories.entries.len(),
        2,
        "ordinary edits must not traverse the workspace again"
    );
    assert_eq!(app.cached_git_branch, format!("{branch} ↑2 ↓1"));
    app.execute_git_command("git_refresh");
    wait(app, |app| !app.bg_git_status_running);
    assert_eq!(
        app.repositories.entries.len(),
        3,
        "explicit refresh discovers new projects"
    );
    app.set_source_control_enabled(false);
    assert_eq!(app.source_control_change_count(), 0);
}

#[test]
fn open_repository_menu_reopens_only_the_chosen_project() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    app.close_source_control_repository(&fixture.nested);
    wait(app, |app| !app.bg_git_status_running);
    app.close_source_control_repository(&fixture.root);
    wait(app, |app| !app.bg_git_status_running);
    app.open_repository_picker();
    assert_eq!(app.context_menu_items.len(), 2);
    app.context_menu_selected = app
        .repositories
        .reopen_candidates
        .iter()
        .position(|root| root == &fixture.nested)
        .unwrap();
    app.reopen_selected_repository();
    wait(app, |app| !app.bg_git_status_running);
    assert_eq!(app.source_control_root(), fixture.nested);
    assert!(app.repositories.closed.contains(&fixture.root));
    assert!(!app.repositories.closed.contains(&fixture.nested));
}

#[test]
fn commit_button_below_repository_headers_targets_the_selected_project() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    app.select_source_control_repository(&fixture.nested);
    app.source_control_commit_msg = "inner commit".into();
    draw(app, 48, 28);
    let commit = app.source_control_commit_area.unwrap();
    click(app, commit);
    assert_eq!(
        app.sc_stage_all_commit_request.as_ref().unwrap().root,
        fixture.nested
    );
}

#[test]
fn repository_list_overflow_and_keyboard_switching_keep_every_project_reachable() {
    use crossterm::event::{KeyCode, KeyModifiers};
    let mut fixture = Fixture::new();
    for index in 0..7 {
        init(&fixture.root.join(format!("project-{index}")), "project\n");
    }
    let app = &mut fixture.app;
    app.repositories.rescan = true;
    refresh(app);
    draw(app, 40, 24);
    assert_eq!(app.repositories.entries.len(), 9);
    assert_eq!(app.repositories.headers.len(), 6);
    for _ in 0..8 {
        crate::ui_source_control::handle_key(app, KeyCode::Down, KeyModifiers::ALT);
        draw(app, 40, 24);
    }
    assert_eq!(app.source_control_root(), fixture.root.join("project-6"));
    assert!(
        app.repositories
            .headers
            .iter()
            .any(|(_, root)| root == app.source_control_root())
    );
    let old = app.repositories.header_scroll;
    let header = app.repositories.headers[0].0;
    crate::mouse_scroll::handle_scroll_up(app, header.x, header.y, KeyModifiers::NONE);
    assert_eq!(app.repositories.header_scroll, old - 1);
    draw(app, 40, 24);
    crate::mouse_scroll::handle_scroll_down(app, header.x, header.y, KeyModifiers::NONE);
    draw(app, 40, 24);
    let active = app
        .repositories
        .headers
        .iter()
        .find(|(_, root)| root == app.source_control_root())
        .unwrap()
        .0;
    click(app, active);
    assert!(!app.repositories.expanded);
    crate::ui_source_control::handle_key(app, KeyCode::Enter, KeyModifiers::NONE);
    assert!(app.repositories.expanded);
}

#[test]
fn initial_discovery_keeps_requests_bound_to_the_same_workspace_despite_path_aliases() {
    let directory = tempfile::tempdir().unwrap();
    init(directory.path(), "before\n");
    let mut app = app_at(directory.path());
    app.source_control_commit_msg = "draft created before discovery".into();
    let request = crate::app_git::GitCommitRequest {
        root: directory.path().to_owned(),
        message: app.source_control_commit_msg.clone(),
    };
    refresh(&mut app);
    assert!(app.current_repository_matches(&request.root));
    // An initial activation must retain the existing draft as well as its identity.
    assert_eq!(app.source_control_commit_msg, request.message);
    app.finish_repository_commit(&request, false);
    assert!(app.source_control_commit_msg.is_empty());
    let other = tempfile::tempdir().unwrap();
    app.explorer.root = other.path().to_owned();
    assert!(!app.current_repository_matches(&request.root));
}

#[test]
fn late_reads_and_commit_completion_remain_safe_after_closing_every_repository() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    app.source_control_commit_msg = "completed commit".into();
    let sender = app.repository_sender();
    app.close_source_control_repository(&fixture.root);
    wait(app, |app| !app.bg_git_status_running);
    app.close_source_control_repository(&fixture.nested);
    wait(app, |app| !app.bg_git_status_running);
    sender
        .send(BackgroundResult::GitBranch("closed branch".into()))
        .unwrap();
    app.poll_background_results();
    assert!(app.cached_git_branch.is_empty());
    assert!(!app.bg_git_branch_running);
    app.finish_repository_commit(
        &crate::app_git::GitCommitRequest {
            root: fixture.root.clone(),
            message: "completed commit".into(),
        },
        false,
    );
    app.open_repository_picker();
    app.context_menu_selected = app
        .repositories
        .reopen_candidates
        .iter()
        .position(|root| root == &fixture.root)
        .unwrap();
    app.reopen_selected_repository();
    wait(app, |app| !app.bg_git_status_running);
    assert!(app.source_control_commit_msg.is_empty());
}

#[test]
fn git_subprocess_cannot_fall_back_to_parent_after_nested_metadata_disappears() {
    let mut fixture = Fixture::new();
    fixture.app.clear_git_history();
    wait(&mut fixture.app, |app| !app.bg_git_branch_running);
    let mut command = git_command(&fixture.nested).unwrap();
    std::fs::rename(
        fixture.nested.join(".git"),
        fixture.root.join("moved-git-metadata"),
    )
    .unwrap();
    // Unpinned discovery really would select the wrong project here.
    assert_eq!(
        canonical(
            git2::Repository::discover(&fixture.nested)
                .unwrap()
                .workdir()
                .unwrap()
        ),
        fixture.root
    );
    assert!(
        !command
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(git_command(&fixture.nested).is_err());
}

#[test]
fn discovery_prunes_dependency_caches_even_when_the_workspace_has_no_gitignore() {
    let directory = tempfile::tempdir().unwrap();
    let root = canonical(directory.path());
    init(&root.join("src/project"), "project\n");
    init(&root.join("node_modules/package/repository"), "cache\n");
    init(&root.join("target/cache/repository"), "cache\n");
    let discovery = discover(&root, &CancellationToken::new());
    assert_eq!(discovery.roots, vec![root.join("src/project")]);
}

#[test]
fn collapsed_repository_cannot_receive_file_actions_from_blank_sidebar_space() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    draw(app, 48, 28);
    let header = app
        .repositories
        .headers
        .iter()
        .find(|(_, root)| root == &fixture.root)
        .unwrap()
        .0;
    click(app, header);
    draw(app, 48, 28);
    assert!(app.source_control_detail_area().is_none());
    crate::mouse_selection::handle_right_button_down(app, 10, 14);
    assert!(!app.sc_context_active);
    let scroll = app.source_control_scroll;
    crate::mouse_scroll::handle_scroll_down(app, 10, 14, crossterm::event::KeyModifiers::NONE);
    assert_eq!(app.source_control_scroll, scroll);
    assert!(app.source_control_commit_area.is_none());
}

#[test]
fn refreshing_an_inactive_repository_preserves_selection_by_file_identity() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    std::fs::write(fixture.root.join("z.txt"), "z\n").unwrap();
    refresh(app);
    let selected = app
        .source_control_files
        .iter()
        .position(|file| file.path == "z.txt")
        .unwrap();
    app.select_source_control_file(selected, false, false);
    app.select_source_control_repository(&fixture.nested);
    std::fs::write(fixture.root.join("a.txt"), "a\n").unwrap();
    refresh(app);
    app.select_source_control_repository(&fixture.root);
    assert_eq!(
        app.source_control_files[app.source_control_selected].path,
        "z.txt"
    );
    let selected = app.selected_source_control_changes();
    assert_eq!(selected.len(), 1);
    assert_eq!(selected[0].path, "z.txt");
    app.select_source_control_repository(&fixture.nested);
    std::fs::remove_file(fixture.root.join("z.txt")).unwrap();
    refresh(app);
    app.select_source_control_repository(&fixture.root);
    assert!(
        app.selected_source_control_changes().is_empty(),
        "a removed selection must not silently target a different file"
    );
}

#[test]
fn status_and_progress_from_before_close_or_reopen_cannot_restore_an_obsolete_repository_list() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    let generation = app.repositories.membership_generation;
    // Hold an old scan in flight while the user closes both projects.
    app.bg_git_status_running = true;
    app.close_source_control_repository(&fixture.nested);
    app.close_source_control_repository(&fixture.root);
    app.background
        .sender
        .send(BackgroundResult::GitRepositoryFound {
            root: fixture.root.clone(),
            workspace_generation: app.workspace_generation,
            repository_generation: generation,
        })
        .unwrap();
    app.background
        .sender
        .send(BackgroundResult::GitRepositories {
            root: fixture.root.clone(),
            workspace_generation: app.workspace_generation,
            repository_generation: generation,
            discovery: Discovery {
                roots: vec![fixture.root.clone(), fixture.nested.clone()],
                parent: None,
                workspace: fixture.root.clone(),
            },
            scans: vec![Scan {
                root: fixture.root.clone(),
                branch: "obsolete".into(),
                worktree_paths: None,
                include_untracked: true,
                files: vec![crate::app::GitFileChange {
                    path: "ghost.txt".into(),
                    original_path: None,
                    status: 'M',
                    staged: false,
                }],
            }],
        })
        .unwrap();
    app.poll_background_results();
    assert_eq!(app.source_control_repository_state, State::Missing);
    wait(app, |app| !app.bg_git_status_running);
    assert_eq!(app.source_control_change_count(), 0);
    assert!(
        app.repositories
            .entries
            .iter()
            .all(|repo| repo.files.is_empty())
    );
    // Now hold a scan of the closed list while one project is reopened.
    let generation = app.repositories.membership_generation;
    app.bg_git_status_running = true;
    app.open_repository_picker();
    app.context_menu_selected = app
        .repositories
        .reopen_candidates
        .iter()
        .position(|root| root == &fixture.nested)
        .unwrap();
    app.reopen_selected_repository();
    app.background
        .sender
        .send(BackgroundResult::GitRepositories {
            root: fixture.root.clone(),
            workspace_generation: app.workspace_generation,
            repository_generation: generation,
            discovery: Discovery {
                roots: vec![fixture.root.clone(), fixture.nested.clone()],
                parent: None,
                workspace: fixture.root.clone(),
            },
            scans: Vec::new(),
        })
        .unwrap();
    app.poll_background_results();
    assert_ne!(
        app.source_control_repository_state,
        State::Available,
        "an old empty reply must not claim the reopened project has no changes"
    );
    wait(app, |app| !app.bg_git_status_running);
    assert_eq!(app.source_control_root(), fixture.nested);
    assert!(
        app.source_control_files
            .iter()
            .any(|file| file.path == "same.txt")
    );
}

#[test]
fn deleted_files_still_diff_when_all_their_parent_directories_were_removed() {
    let mut fixture = Fixture::new();
    let file = fixture.nested.join("dir/deep/gone.txt");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "deleted content\n").unwrap();
    let repo = git2::Repository::open(&fixture.nested).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("dir/deep/gone.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = repo.signature().unwrap();
    let parent = repo.head().unwrap().peel_to_commit().unwrap();
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "Add deleted fixture",
        &tree,
        &[&parent],
    )
    .unwrap();
    std::fs::remove_dir_all(fixture.nested.join("dir")).unwrap();
    // Discovery from the file alone must also work before any workspace scan.
    assert_eq!(
        canonical(
            open_file_repository(&file, None)
                .unwrap()
                .workdir()
                .unwrap()
        ),
        fixture.nested
    );
    let app = &mut fixture.app;
    refresh(app);
    app.select_source_control_repository(&fixture.nested);
    let index = app
        .source_control_files
        .iter()
        .position(|file| file.path == "dir/deep/gone.txt")
        .unwrap();
    app.open_source_control_diff(index);
    wait(app, |app| {
        app.pending_git_compare_path.is_none() && !app.bg_git_compare_running
    });
    assert_eq!(
        app.compare_base_content.as_deref(),
        Some("deleted content\n")
    );
    assert_eq!(app.editor().unwrap().rope.to_string(), "");
    assert!(app.editor().unwrap().read_only);
    app.select_source_control_repository(&fixture.root);
    app.refresh_source_control_diffs();
    wait(app, |app| !app.diff_tabs.refresh_running);
    assert_eq!(
        app.compare_base_content.as_deref(),
        Some("deleted content\n")
    );
}

#[test]
fn file_repository_hint_cannot_fall_back_to_parent_when_nested_metadata_is_removed() {
    let mut fixture = Fixture::new();
    let file = fixture.nested.join("same.txt");
    fixture.app.clear_git_history();
    wait(&mut fixture.app, |app| !app.bg_git_branch_running);
    let hint = fixture.app.repository_hint_for_path(&file);
    assert_eq!(hint.as_deref(), Some(fixture.nested.as_path()));
    std::fs::rename(fixture.nested.join(".git"), fixture.root.join("moved-git")).unwrap();
    assert!(open_file_repository(&file, hint.as_deref()).is_err());
    fixture.app.hunk_popup_active = true;
    fixture.app.hunk_popup_file = Some(file);
    fixture.app.hunk_popup_status = 'M';
    fixture.app.repositories.rescan = true;
    refresh(&mut fixture.app);
    assert!(!fixture.app.hunk_popup_active);
}

#[test]
fn explorer_decorations_combine_open_repositories_without_overwriting_siblings() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    for path in [
        fixture.root.join("same.txt"),
        fixture.nested.join("same.txt"),
    ] {
        assert!(
            app.explorer
                .git_statuses
                .get(&path)
                .unwrap()
                .contains(crate::explorer::GitStatus::Modified)
        );
    }
    app.close_source_control_repository(&fixture.nested);
    wait(app, |app| !app.bg_git_status_running);
    assert!(
        app.explorer
            .git_statuses
            .contains_key(&fixture.root.join("same.txt"))
    );
    assert!(
        !app.explorer
            .git_statuses
            .contains_key(&fixture.nested.join("same.txt"))
    );
}

#[test]
fn parent_traversals_and_missing_directories_resolve_to_the_actual_repository() {
    let fixture = Fixture::new();
    let through_child = fixture._directory.path().join("nested/../same.txt");
    let hint = fixture.app.repository_hint_for_path(&through_child);
    assert_ne!(hint.as_deref(), Some(fixture.nested.as_path()));
    assert_eq!(
        canonical(
            open_file_repository(&through_child, hint.as_deref())
                .unwrap()
                .workdir()
                .unwrap()
        ),
        fixture.root
    );
    let missing = fixture
        ._directory
        .path()
        .join("nested/../removed/deep/gone.txt");
    assert_eq!(
        crate::app_git::repository_relative_path(&fixture.root, &missing).as_deref(),
        Some(Path::new("removed/deep/gone.txt"))
    );
    let outside = fixture
        ._directory
        .path()
        .join("nested/../../missing-outside/gone.txt");
    assert!(crate::app_git::repository_relative_path(&fixture.root, &outside).is_none());
}

#[test]
fn opening_a_repository_file_reuses_an_existing_unsaved_editor_buffer() {
    let mut fixture = Fixture::new();
    let raw_path = fixture._directory.path().join("same.txt");
    let mut editor = crate::editor::Editor::test_with_text("unsaved text\n");
    editor.file_path = Some(raw_path);
    editor.insert_char('X');
    fixture.app.tabs = vec![crate::app::Tab::new_file(editor)];
    fixture.app.active_tab = 0;
    fixture.app.open_file(&fixture.root.join("same.txt"));
    assert_eq!(
        fixture.app.tabs.len(),
        1,
        "canonical Git paths must not create an independent copy of an open file"
    );
    assert_eq!(
        fixture.app.editor().unwrap().rope.to_string(),
        "Xunsaved text\n"
    );
    assert!(fixture.app.editor().unwrap().modified);
}

#[cfg(windows)]
#[test]
fn windows_repository_paths_match_disk_and_unc_verbatim_spellings_without_io() {
    assert!(App::same_open_path(
        Path::new(r"C:\Projects\File.txt"),
        Path::new(r"\\?\c:\projects\file.txt")
    ));
    assert!(App::same_open_path(
        Path::new(r"\\server\share\folder\file.txt"),
        Path::new(r"\\?\UNC\SERVER\SHARE\folder\FILE.txt")
    ));
    assert!(!App::same_open_path(
        Path::new(r"C:\Projects\File.txt"),
        Path::new(r"\\?\D:\Projects\File.txt")
    ));
    assert!(!App::same_open_path(
        Path::new(r"\\server\share\file.txt"),
        Path::new(r"\\?\UNC\server\other\file.txt")
    ));
}

#[cfg(unix)]
#[test]
fn repository_file_open_preserves_a_symlink_workspaces_existing_unsaved_buffer() {
    let fixture = Fixture::new();
    let alias_directory = tempfile::tempdir().unwrap();
    let alias = alias_directory.path().join("workspace-link");
    std::os::unix::fs::symlink(&fixture.root, &alias).unwrap();
    let mut app = app_at(&alias);
    refresh(&mut app);
    let mut editor = crate::editor::Editor::test_with_text("unsaved\n");
    editor.file_path = Some(alias.join("same.txt"));
    editor.insert_char('X');
    app.tabs = vec![crate::app::Tab::new_file(editor)];
    app.active_tab = 0;
    let change = app
        .source_control_files
        .iter()
        .find(|file| file.path == "same.txt")
        .unwrap()
        .clone();
    app.open_source_control_context_menu(0, 0, change);
    let open = app
        .source_control_context_menu_items()
        .iter()
        .position(|label| *label == "Open File")
        .unwrap();
    app.execute_source_control_context(open);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.editor().unwrap().rope.to_string(), "Xunsaved\n");
    assert_eq!(
        app.editor().unwrap().file_path.as_ref(),
        Some(&alias.join("same.txt"))
    );
}

#[test]
fn parent_and_child_diffs_keep_distinct_baselines_when_both_track_the_same_file() {
    let directory = tempfile::tempdir().unwrap();
    let root = canonical(directory.path());
    init(&root, "root\n");
    let nested = root.join("nested");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(nested.join("same.txt"), "parent baseline\n").unwrap();
    let parent_repo = git2::Repository::open(&root).unwrap();
    let mut index = parent_repo.index().unwrap();
    index.add_path(Path::new("nested/same.txt")).unwrap();
    index.write().unwrap();
    let tree = parent_repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = parent_repo.signature().unwrap();
    let head = parent_repo.head().unwrap().peel_to_commit().unwrap();
    parent_repo
        .commit(
            Some("HEAD"),
            &signature,
            &signature,
            "Track nested file",
            &tree,
            &[&head],
        )
        .unwrap();
    init(&nested, "child baseline\n");
    std::fs::write(nested.join("same.txt"), "working version\n").unwrap();
    let mut app = app_at(&root);
    refresh(&mut app);
    let parent_file = app
        .source_control_files
        .iter()
        .position(|file| file.path == "nested/same.txt" && !file.staged)
        .unwrap();
    app.open_source_control_diff(parent_file);
    wait(&mut app, |app| {
        app.pending_git_compare_path.is_none() && !app.bg_git_compare_running
    });
    let parent_tab = app.active_tab;
    assert_eq!(
        app.compare_base_content.as_deref(),
        Some("parent baseline\n")
    );
    app.select_source_control_repository(&nested);
    let child_file = app
        .source_control_files
        .iter()
        .position(|file| file.path == "same.txt" && !file.staged)
        .unwrap();
    app.open_source_control_diff(child_file);
    wait(&mut app, |app| {
        app.pending_git_compare_path.is_none() && !app.bg_git_compare_running
    });
    let child_tab = app.active_tab;
    assert_ne!(parent_tab, child_tab);
    assert_eq!(
        app.compare_base_content.as_deref(),
        Some("child baseline\n")
    );
    assert_ne!(
        crate::ui_chrome_tabs::tab_display_name(&app, parent_tab, &app.tabs[parent_tab]),
        crate::ui_chrome_tabs::tab_display_name(&app, child_tab, &app.tabs[child_tab])
    );
    app.active_tab = parent_tab;
    app.sync_diff_tab();
    app.refresh_source_control_diffs();
    wait(&mut app, |app| !app.diff_tabs.refresh_running);
    assert_eq!(
        app.compare_base_content.as_deref(),
        Some("parent baseline\n")
    );
    app.active_tab = child_tab;
    app.sync_diff_tab();
    assert_eq!(
        app.compare_base_content.as_deref(),
        Some("child baseline\n")
    );
    app.select_source_control_repository(&root);
    let parent_file = app
        .source_control_files
        .iter()
        .position(|file| file.path == "nested/same.txt" && !file.staged)
        .unwrap();
    app.git_stage_file(parent_file);
    wait(&mut app, |app| {
        !app.bg_git_action_running && !app.bg_git_status_running
    });
    let updated_parent = git2::Repository::open(&root).unwrap();
    let entry = updated_parent
        .index()
        .unwrap()
        .get_path(Path::new("nested/same.txt"), 0)
        .unwrap();
    assert_eq!(
        updated_parent.find_blob(entry.id).unwrap().content(),
        b"working version\n"
    );
    assert_eq!(index_text(&nested), "child baseline\n");
}

#[test]
fn equivalent_repository_path_spellings_reuse_the_same_diff_tab() {
    let mut fixture = Fixture::new();
    let app = &mut fixture.app;
    let path = fixture.root.join("same.txt");
    app.open_source_control_diff_in_repository(
        &path,
        false,
        false,
        Some(fixture._directory.path().to_owned()),
    );
    wait(app, |app| {
        app.pending_git_compare_path.is_none() && !app.bg_git_compare_running
    });
    let first = app.active_tab;
    app.open_source_control_diff_in_repository(&path, false, false, Some(fixture.root.clone()));
    assert_eq!(app.active_tab, first);
    assert_eq!(
        app.tabs
            .iter()
            .filter(|tab| matches!(tab.kind, crate::app::TabKind::Diff(_)))
            .count(),
        1
    );
}
