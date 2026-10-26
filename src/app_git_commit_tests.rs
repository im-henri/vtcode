use super::collect_source_control_status;
use crate::app::{App, SidebarView, SourceControlRepositoryState};
use crate::ui_modal::{self, Kind};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::path::Path;
use std::time::Duration;

const MESSAGE: &str = "Commit all saved changes";

struct Fixture {
    // Join the app's background workers before removing the repository.
    app: App,
    directory: tempfile::TempDir,
}

impl Fixture {
    fn new(with_head: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let repo = git2::Repository::init(root).unwrap();
        let mut config = repo.config().unwrap();
        config.set_bool("core.autocrlf", false).unwrap();
        config.set_str("user.name", "VTCode Test").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();
        std::fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(root.join("modified.txt"), "before\n").unwrap();
        std::fs::write(root.join("deleted.txt"), "delete me\n").unwrap();
        if with_head {
            let mut index = repo.index().unwrap();
            index
                .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
                .unwrap();
            index.write().unwrap();
            let tree_oid = index.write_tree().unwrap();
            let tree = repo.find_tree(tree_oid).unwrap();
            let signature = repo.signature().unwrap();
            repo.commit(Some("HEAD"), &signature, &signature, "Initial", &tree, &[])
                .unwrap();
        }
        std::fs::write(root.join("modified.txt"), "after\n").unwrap();
        std::fs::remove_file(root.join("deleted.txt")).unwrap();
        std::fs::write(root.join("new.txt"), "new\n").unwrap();
        std::fs::write(root.join("ignored.txt"), "ignored\n").unwrap();
        drop(config);
        drop(repo);
        let mut app = App::new(root.to_path_buf());
        app.source_control_files = collect_source_control_status(root.to_path_buf()).files;
        app.source_control_repository_state = SourceControlRepositoryState::Available;
        app.source_control_commit_msg = MESSAGE.into();
        app.source_control_commit_cursor = MESSAGE.len();
        Self { app, directory }
    }

    fn repository(&self) -> git2::Repository {
        git2::Repository::open(self.directory.path()).unwrap()
    }

    fn finish_git(&mut self) {
        self.app
            .wait_for_background(Duration::from_secs(5), |app| !app.bg_git_action_running);
    }

    fn last_notification(&self) -> &str {
        &self.app.notification_history.last().unwrap().0
    }

    fn assert_worktree_preserved(&self) {
        let root = self.directory.path();
        assert_eq!(
            std::fs::read_to_string(root.join("modified.txt")).unwrap(),
            "after\n"
        );
        assert!(!root.join("deleted.txt").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("new.txt")).unwrap(),
            "new\n"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("ignored.txt")).unwrap(),
            "ignored\n"
        );
    }
}

fn click(app: &mut App, area: Rect) {
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

fn key(app: &mut App, code: KeyCode) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, KeyModifiers::NONE));
}

fn render_confirmation(app: &mut App) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| ui_modal::draw_active(frame, app))
        .unwrap();
    terminal
}

fn click_commit(app: &mut App) {
    app.sidebar_view = SidebarView::SourceControl;
    app.show_explorer = true;
    app.sc_graph_collapsed = true;
    let area = Rect::new(2, 1, 38, 22);
    app.explorer_area = Some(area);
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui_source_control::draw_source_control_sidebar(frame, app, area))
        .unwrap();
    click(app, app.source_control_commit_area.unwrap());
}

#[test]
fn commit_without_staging_can_be_cancelled_without_changing_files_or_message() {
    let mut fixture = Fixture::new(true);
    let repo = fixture.repository();
    let head = repo.head().unwrap().target().unwrap();
    let index = std::fs::read(repo.path().join("index")).unwrap();
    // Each cancellation must leave this same repository/draft unchanged;
    // rebuilding it would only repeat setup and conceal cross-open state.
    for cancel in 0..4 {
        click_commit(&mut fixture.app);
        assert_eq!(ui_modal::active(&fixture.app), Some(Kind::StageAllCommit));
        assert!(!fixture.app.bg_git_action_running);
        let terminal = render_confirmation(&mut fixture.app);
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("No changes are staged."));
        assert!(text.contains("Stage all & commit"));
        assert_eq!(std::fs::read(repo.path().join("index")).unwrap(), index);
        match cancel {
            0 => key(&mut fixture.app, KeyCode::Esc),
            1 => {
                key(&mut fixture.app, KeyCode::Tab);
                key(&mut fixture.app, KeyCode::Enter);
            }
            2 => {
                let area = fixture.app.modal.buttons[1].0;
                click(&mut fixture.app, area);
            }
            _ => {
                let area = fixture.app.modal.close;
                click(&mut fixture.app, area);
            }
        }
        assert!(ui_modal::active(&fixture.app).is_none());
        assert!(!fixture.app.bg_git_action_running);
        assert_eq!(fixture.app.source_control_commit_msg, MESSAGE);
        assert_eq!(repo.head().unwrap().target().unwrap(), head);
        assert_eq!(std::fs::read(repo.path().join("index")).unwrap(), index);
        fixture.assert_worktree_preserved();
    }
}

#[test]
fn confirmed_commit_stages_modifications_deletions_and_new_files_but_not_ignored_files() {
    let mut fixture = Fixture::new(true);
    let original_head = fixture.repository().head().unwrap().target().unwrap();
    click_commit(&mut fixture.app);
    render_confirmation(&mut fixture.app);
    let area = fixture.app.modal.buttons[0].0;
    click(&mut fixture.app, area);
    assert!(fixture.app.sc_stage_all_commit_request.is_none());
    assert!(fixture.app.bg_git_action_running);
    // Activating the confirmation twice cannot create a second commit.
    fixture.app.confirm_stage_all_commit();
    fixture.finish_git();
    let repo = fixture.repository();
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(commit.message(), Some(MESSAGE));
    assert_eq!(commit.parent_count(), 1);
    assert_eq!(commit.parent_id(0).unwrap(), original_head);
    let tree = commit.tree().unwrap();
    let modified = tree.get_path(Path::new("modified.txt")).unwrap();
    assert_eq!(repo.find_blob(modified.id()).unwrap().content(), b"after\n");
    assert!(tree.get_path(Path::new("new.txt")).is_ok());
    assert!(tree.get_path(Path::new("deleted.txt")).is_err());
    assert!(tree.get_path(Path::new("ignored.txt")).is_err());
    assert_eq!(repo.index().unwrap().write_tree().unwrap(), tree.id());
    assert!(fixture.app.source_control_commit_msg.is_empty());
    fixture.assert_worktree_preserved();
}

#[test]
fn confirmed_commit_can_create_the_first_commit() {
    let mut fixture = Fixture::new(false);
    fixture.app.git_commit();
    assert_eq!(ui_modal::active(&fixture.app), Some(Kind::StageAllCommit));
    key(&mut fixture.app, KeyCode::Enter);
    fixture.finish_git();
    let repo = fixture.repository();
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(commit.parent_count(), 0);
    assert_eq!(commit.message(), Some(MESSAGE));
    assert!(
        commit
            .tree()
            .unwrap()
            .get_path(Path::new("new.txt"))
            .is_ok()
    );
    fixture.assert_worktree_preserved();
}

#[test]
fn commit_with_staged_changes_commits_only_the_index_snapshot() {
    let mut fixture = Fixture::new(true);
    let repo = fixture.repository();
    std::fs::write(fixture.directory.path().join("modified.txt"), "staged\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("modified.txt")).unwrap();
    index.write().unwrap();
    std::fs::write(fixture.directory.path().join("modified.txt"), "after\n").unwrap();
    fixture.app.source_control_files =
        collect_source_control_status(fixture.directory.path().to_path_buf()).files;
    fixture.app.git_commit();
    assert!(fixture.app.sc_stage_all_commit_request.is_none());
    assert!(fixture.app.bg_git_action_running);
    fixture.finish_git();
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    let tree = commit.tree().unwrap();
    let entry = tree.get_path(Path::new("modified.txt")).unwrap();
    assert_eq!(repo.find_blob(entry.id()).unwrap().content(), b"staged\n");
    assert!(tree.get_path(Path::new("deleted.txt")).is_ok());
    assert!(tree.get_path(Path::new("new.txt")).is_err());
    assert!(fixture.app.source_control_commit_msg.is_empty());
    fixture.assert_worktree_preserved();
}

#[test]
fn commit_with_stale_staged_status_asks_before_staging_anything() {
    let mut fixture = Fixture::new(true);
    let repo = fixture.repository();
    let head = repo.head().unwrap().target().unwrap();
    let index = std::fs::read(repo.path().join("index")).unwrap();
    fixture.app.source_control_files.first_mut().unwrap().staged = true;
    fixture.app.git_commit();
    fixture.finish_git();
    assert_eq!(ui_modal::active(&fixture.app), Some(Kind::StageAllCommit));
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert_eq!(std::fs::read(repo.path().join("index")).unwrap(), index);
    assert_eq!(fixture.app.source_control_commit_msg, MESSAGE);
    fixture.assert_worktree_preserved();
    fixture.app.confirm_stage_all_commit();
    fixture.finish_git();
    assert_eq!(
        repo.head().unwrap().peel_to_commit().unwrap().message(),
        Some(MESSAGE)
    );
    assert_ne!(repo.head().unwrap().target().unwrap(), head);
}

#[test]
fn commit_validation_does_not_prompt_while_busy_or_without_a_message() {
    let mut fixture = Fixture::new(true);
    fixture.app.source_control_commit_msg = " \n ".into();
    fixture.app.git_commit();
    assert!(fixture.app.sc_stage_all_commit_request.is_none());
    assert!(!fixture.app.bg_git_action_running);
    assert_eq!(
        fixture.last_notification(),
        "Commit message cannot be empty"
    );
    fixture.app.source_control_commit_msg = MESSAGE.into();
    for remote in [false, true] {
        fixture.app.bg_git_action_running = !remote;
        fixture.app.bg_git_remote_running = remote;
        fixture.app.git_commit();
        assert!(fixture.app.sc_stage_all_commit_request.is_none());
        assert_eq!(
            fixture.last_notification(),
            "Another git operation is already running"
        );
    }
    fixture.app.bg_git_action_running = false;
    fixture.app.bg_git_remote_running = false;
    fixture.app.git_commit();
    fixture.app.bg_git_remote_running = true;
    fixture.app.confirm_stage_all_commit();
    assert!(!fixture.app.bg_git_action_running);
    assert_eq!(fixture.app.source_control_commit_msg, MESSAGE);
    fixture.app.bg_git_remote_running = false;
    fixture.assert_worktree_preserved();
}

#[test]
fn commit_confirmation_cannot_stage_a_different_workspace() {
    for change_workspace in [false, true] {
        let mut fixture = Fixture::new(true);
        let other = Fixture::new(true);
        let original_head = fixture.repository().head().unwrap().target().unwrap();
        let other_head = other.repository().head().unwrap().target().unwrap();
        fixture.app.git_commit();
        if change_workspace {
            fixture.app.change_workspace(other.directory.path());
        } else {
            fixture.app.explorer.root = other.directory.path().to_path_buf();
        }
        fixture.app.confirm_stage_all_commit();
        assert!(fixture.app.sc_stage_all_commit_request.is_none());
        assert!(!fixture.app.bg_git_action_running);
        assert_eq!(
            fixture.repository().head().unwrap().target().unwrap(),
            original_head
        );
        assert_eq!(
            other.repository().head().unwrap().target().unwrap(),
            other_head
        );
        assert_eq!(
            other.repository().index().unwrap().write_tree().unwrap(),
            other
                .repository()
                .head()
                .unwrap()
                .peel_to_tree()
                .unwrap()
                .id()
        );
        fixture.assert_worktree_preserved();
        other.assert_worktree_preserved();
    }
}

#[test]
fn commit_confirmation_rechecks_changes_and_never_creates_an_empty_commit() {
    let mut fixture = Fixture::new(true);
    let repo = fixture.repository();
    let head = repo.head().unwrap().target().unwrap();
    fixture.app.git_commit();
    let root = fixture.directory.path().to_path_buf();
    std::fs::write(root.join("modified.txt"), "before\n").unwrap();
    std::fs::write(root.join("deleted.txt"), "delete me\n").unwrap();
    std::fs::remove_file(root.join("new.txt")).unwrap();
    fixture.app.confirm_stage_all_commit();
    fixture.finish_git();
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert!(fixture.last_notification().contains("No changes to commit"));
    assert_eq!(fixture.app.source_control_commit_msg, MESSAGE);
    assert_eq!(
        std::fs::read_to_string(root.join("modified.txt")).unwrap(),
        "before\n"
    );
}

#[test]
fn commit_confirmation_keeps_conflicts_and_worktree_intact() {
    let mut fixture = Fixture::new(true);
    let repo = fixture.repository();
    let head = repo.head().unwrap().target().unwrap();
    let mut index = repo.index().unwrap();
    let entries = [1, 2, 3].map(|stage| {
        let mut entry = index.get_path(Path::new("modified.txt"), 0).unwrap();
        entry.flags = (entry.flags & !0x3000) | (stage << 12);
        entry.id = repo
            .blob(format!("conflict stage {stage}\n").as_bytes())
            .unwrap();
        entry
    });
    index.remove_path(Path::new("modified.txt")).unwrap();
    for entry in entries {
        index.add(&entry).unwrap();
    }
    index.write().unwrap();
    assert!(index.has_conflicts());
    let before = std::fs::read(repo.path().join("index")).unwrap();
    fixture.app.source_control_files.clear();
    fixture.app.git_commit();
    fixture.app.confirm_stage_all_commit();
    fixture.finish_git();
    assert!(
        fixture
            .last_notification()
            .contains("Resolve merge conflicts")
    );
    assert_eq!(fixture.app.source_control_commit_msg, MESSAGE);
    assert_eq!(repo.head().unwrap().target().unwrap(), head);
    assert_eq!(std::fs::read(repo.path().join("index")).unwrap(), before);
    fixture.assert_worktree_preserved();
}

#[test]
fn failed_confirmed_commit_keeps_the_message_and_staged_changes() {
    let mut fixture = Fixture::new(true);
    let repo = fixture.repository();
    let head = repo.head().unwrap();
    let original_head = head.target().unwrap();
    let lock = repo.path().join(format!("{}.lock", head.name().unwrap()));
    std::fs::write(lock, "lock").unwrap();
    fixture.app.git_commit();
    fixture.app.confirm_stage_all_commit();
    fixture.finish_git();
    assert!(
        fixture
            .last_notification()
            .starts_with("Git stage and commit failed:")
    );
    fixture
        .app
        .wait_for_background(Duration::from_secs(5), |app| {
            !app.source_control_files.is_empty()
                && app.source_control_files.iter().all(|file| file.staged)
        });
    assert_eq!(fixture.app.source_control_commit_msg, MESSAGE);
    assert_eq!(repo.head().unwrap().target().unwrap(), original_head);
    let tree = repo
        .find_tree(repo.index().unwrap().write_tree().unwrap())
        .unwrap();
    assert!(tree.get_path(Path::new("new.txt")).is_ok());
    assert!(tree.get_path(Path::new("deleted.txt")).is_err());
    fixture.assert_worktree_preserved();
}

#[test]
fn successful_confirmed_commit_preserves_a_new_commit_message_draft() {
    let mut fixture = Fixture::new(true);
    fixture.app.git_commit();
    fixture.app.confirm_stage_all_commit();
    fixture.app.source_control_commit_msg = "Next commit draft".into();
    fixture.app.source_control_commit_cursor = fixture.app.source_control_commit_msg.len();
    fixture.finish_git();
    assert_eq!(
        fixture
            .repository()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .message(),
        Some(MESSAGE)
    );
    assert_eq!(fixture.app.source_control_commit_msg, "Next commit draft");
    assert_eq!(
        fixture.app.source_control_commit_cursor,
        "Next commit draft".len()
    );
}

#[test]
fn successful_confirmed_commit_preserves_another_workspaces_draft() {
    let mut fixture = Fixture::new(true);
    let other = Fixture::new(true);
    let other_head = other.repository().head().unwrap().target().unwrap();
    fixture.app.git_commit();
    fixture.app.confirm_stage_all_commit();
    fixture.app.change_workspace(other.directory.path());
    // Even identical messages belong to different commit requests.
    fixture.app.source_control_commit_msg = MESSAGE.into();
    fixture.app.source_control_commit_cursor = MESSAGE.len();
    fixture.finish_git();
    assert_eq!(
        fixture
            .repository()
            .head()
            .unwrap()
            .peel_to_commit()
            .unwrap()
            .message(),
        Some(MESSAGE)
    );
    assert_eq!(
        other.repository().head().unwrap().target().unwrap(),
        other_head
    );
    assert_eq!(fixture.app.source_control_commit_msg, MESSAGE);
    assert_eq!(fixture.app.source_control_commit_cursor, MESSAGE.len());
    other.assert_worktree_preserved();
}
