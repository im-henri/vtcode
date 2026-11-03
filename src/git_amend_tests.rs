use crate::{
    app::{
        ActivePane, App, GitCommitInfo, GitFileChange, SidebarView, SourceControlRepositoryState,
    },
    remote,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::{path::Path, sync::Arc, time::Duration};

const ORIGINAL: &str = "Original subject λ\n\nFirst paragraph with Unicode 界.\nSecond line.\n\n- One detail\n- Another detail\n\nSigned-off-by: Test <test@example.com>\n\n";
const REPLACEMENT: &str = "Revised subject 🦀\n\nReplacement body.\n\nAnother paragraph.\n\nReviewed-by: Reviewer <reviewer@example.com>\n";

struct Fixture {
    app: App,
    directory: tempfile::TempDir,
    head: git2::Oid,
    parent: git2::Oid,
    staged_tree: git2::Oid,
}

impl Fixture {
    fn new(remote: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init(directory.path()).unwrap();
        let mut config = repo.config().unwrap();
        config.set_bool("core.autocrlf", false).unwrap();
        config.set_str("user.name", "Test").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();
        let signature = repo.signature().unwrap();
        let mut index = repo.index().unwrap();
        let mut commit = |text: &str, message: &str, parent: Option<git2::Oid>| {
            std::fs::write(directory.path().join("file.txt"), text).unwrap();
            index.add_path(Path::new("file.txt")).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            let parent = parent.map(|parent| repo.find_commit(parent).unwrap());
            repo.commit(
                Some("HEAD"),
                &signature,
                &signature,
                message,
                &tree,
                &parent.iter().collect::<Vec<_>>(),
            )
            .unwrap()
        };
        let parent = commit("base\n", "Parent commit", None);
        let head = commit("before\n", ORIGINAL, Some(parent));
        std::fs::write(directory.path().join("file.txt"), "staged change\n").unwrap();
        index.add_path(Path::new("file.txt")).unwrap();
        index.write().unwrap();
        let staged_tree = index.write_tree().unwrap();
        let workspace = remote.then(|| {
            remote::workspace::Workspace::new(remote::tests::connection(
                directory.path(),
                Arc::default(),
                Arc::default(),
            ))
        });
        let mut app = App::new_deferred(workspace.as_ref().map_or_else(
            || directory.path().into(),
            |workspace| workspace.view_root.clone(),
        ));
        if let Some(workspace) = &workspace {
            workspace.client.set_wake(app.background.wake_handle());
        }
        app.remote_workspace = workspace;
        app.show_explorer = true;
        app.sidebar_view = SidebarView::SourceControl;
        app.active_pane = ActivePane::Explorer;
        app.source_control_repository_state = SourceControlRepositoryState::Available;
        app.source_control_files = vec![GitFileChange {
            path: "file.txt".into(),
            directory: false,
            status: 'M',
            staged: true,
            original_path: None,
        }];
        app.sc_graph_collapsed = true;
        // History deliberately holds only a subject, just as production does.
        app.git_log = vec![GitCommitInfo {
            graph: String::new(),
            hash: head.to_string()[..7].into(),
            full_hash: head.to_string(),
            message: "Original subject λ".into(),
            author: "Test".into(),
            time_ago: String::new(),
            branches: Vec::new(),
        }];
        Self {
            app,
            directory,
            head,
            parent,
            staged_tree,
        }
    }

    fn amend(&mut self, keyboard: bool) {
        if keyboard {
            self.app.source_control_input_mode = true;
            crate::input_keyboard::handle_keyboard_event(
                &mut self.app,
                KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            );
        } else {
            let area = Rect::new(2, 1, 44, 26);
            self.app.explorer_area = Some(area);
            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            terminal
                .draw(|frame| {
                    crate::ui_source_control::draw_source_control_sidebar(
                        frame,
                        &mut self.app,
                        area,
                    )
                })
                .unwrap();
            let button = self.app.source_control_amend_area.unwrap();
            crate::mouse::handle_mouse(
                &mut self.app,
                MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: button.x + 1,
                    row: button.y,
                    modifiers: KeyModifiers::NONE,
                },
            );
        }
        assert!(
            self.app.bg_git_action_running,
            "{:?}",
            self.app.notification_history
        );
    }

    fn finish(&mut self) {
        self.app
            .wait_for_background(Duration::from_secs(10), |app| !app.bg_git_action_running);
        assert!(
            !self.app.sc_amend_mode,
            "{:?}",
            self.app.notification_history
        );
    }

    fn assert_commit(&self, message: &str) {
        let repo = git2::Repository::open(self.directory.path()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_ne!(head.id(), self.head);
        assert_eq!(head.message_bytes(), message.as_bytes());
        assert_eq!(head.tree_id(), self.staged_tree);
        assert_eq!(head.parent_count(), 1);
        assert_eq!(head.parent_id(0).unwrap(), self.parent);
        assert_eq!(
            std::fs::read_to_string(self.directory.path().join("file.txt")).unwrap(),
            "staged change\n"
        );
    }
}

#[test]
fn amend_button_and_shortcut_preserve_full_messages_locally_and_remotely() {
    for remote in [false, true] {
        for keyboard in [false, true] {
            let mut fixture = Fixture::new(remote);
            if keyboard {
                fixture.app.git_log.clear();
            }
            fixture.amend(keyboard);
            assert!(fixture.app.source_control_commit_msg.is_empty());
            fixture.finish();
            fixture.assert_commit(ORIGINAL);
        }
    }
}

#[test]
fn explicit_multiline_amend_messages_keep_every_paragraph_and_trailer() {
    for remote in [false, true] {
        for keyboard in [false, true] {
            let mut fixture = Fixture::new(remote);
            fixture.app.source_control_commit_msg = REPLACEMENT.into();
            fixture.app.source_control_commit_cursor = REPLACEMENT.len();
            fixture.amend(keyboard);
            fixture.finish();
            fixture.assert_commit(REPLACEMENT);
            assert!(fixture.app.source_control_commit_msg.is_empty());
        }
    }
}

#[test]
fn amend_ignores_stale_history_and_keeps_a_draft_entered_while_it_runs() {
    for remote in [false, true] {
        let mut fixture = Fixture::new(remote);
        fixture.app.git_log[0].message = "Obsolete subject from another branch".into();
        fixture.app.git_log[0].full_hash = fixture.parent.to_string();
        fixture.amend(false);
        fixture.app.source_control_commit_msg = "Next commit\n\nNew draft".into();
        fixture.app.source_control_commit_cursor = fixture.app.source_control_commit_msg.len();
        fixture.finish();
        fixture.assert_commit(ORIGINAL);
        assert_eq!(
            fixture.app.source_control_commit_msg,
            "Next commit\n\nNew draft"
        );
    }
}

#[test]
fn amend_without_staged_changes_or_with_a_whitespace_message_does_not_rewrite_head() {
    let mut fixture = Fixture::new(false);
    fixture.app.source_control_commit_msg = " \n\t".into();
    fixture.app.git_amend_commit();
    assert!(!fixture.app.bg_git_action_running);
    assert_eq!(
        fixture.app.notification_history.last().unwrap().0,
        "Commit message cannot be empty"
    );
    fixture.app.source_control_commit_msg.clear();
    fixture.app.source_control_files.clear();
    fixture.app.git_amend_commit();
    assert!(!fixture.app.bg_git_action_running);
    assert_eq!(
        fixture.app.notification_history.last().unwrap().0,
        "No staged changes to commit"
    );
    let repo = git2::Repository::open(fixture.directory.path()).unwrap();
    assert_eq!(repo.head().unwrap().target(), Some(fixture.head));
}
