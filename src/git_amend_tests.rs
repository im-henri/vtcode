use crate::{
    app::{
        ActivePane, App, GitCommitInfo, GitFileChange, SidebarView, SourceControlRepositoryState,
        TabKind,
    },
    remote,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend, layout::Rect};
use std::{path::Path, sync::Arc, time::Duration};

const ORIGINAL: &str = "\nOriginal subject λ\n\nFirst paragraph with Unicode 界.\nSecond line.\n\n- One detail\n- Another detail\n\nSigned-off-by: Test <test@example.com>\n\n";
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
        let signature = git2::Signature::now("Original author", "author@example.com").unwrap();
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
            self.app.active_pane = ActivePane::Explorer;
            self.app.source_control_input_mode = true;
            self.key(KeyCode::Enter, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
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
            self.click(button);
        }
        assert_eq!(
            self.app.tabs[self.app.active_tab].kind,
            TabKind::AmendCommit
        );
        self.app
            .wait_for_background(Duration::from_secs(10), |app| {
                app.amend_commits
                    .values()
                    .any(|draft| draft.target.is_some() || draft.error.is_some())
            });
        assert_eq!(self.app.editor().unwrap().rope.to_string(), ORIGINAL);
        assert!(!self.app.editor().unwrap().read_only);
        self.assert_unchanged();
    }

    fn key(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        crate::input_keyboard::handle_keyboard_event(&mut self.app, KeyEvent::new(code, modifiers));
    }

    fn click(&mut self, rect: Rect) {
        crate::mouse::handle_mouse(
            &mut self.app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: rect.x + rect.width / 2,
                row: rect.y,
                modifiers: KeyModifiers::NONE,
            },
        );
    }

    fn render(&mut self) -> String {
        let mut terminal = Terminal::new(TestBackend::new(110, 30)).unwrap();
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut self.app))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(110)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn edit(&mut self, message: &str) {
        self.key(KeyCode::Char('a'), KeyModifiers::CONTROL);
        self.app.paste_editor_text(message.into(), false);
        assert_eq!(self.app.editor().unwrap().rope.to_string(), message);
    }

    fn submit(&mut self, keyboard: bool) {
        if keyboard {
            self.key(KeyCode::Char('a'), KeyModifiers::ALT);
        } else {
            let screen = self.render();
            assert!(screen.contains("Amend Commit"), "{screen}");
            assert!(screen.contains("Abort"), "{screen}");
            let button = self
                .app
                .amend_commits
                .values()
                .next()
                .unwrap()
                .buttons
                .unwrap()
                .amend;
            self.click(button);
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
    }

    fn assert_unchanged(&self) {
        let repo = git2::Repository::open(self.directory.path()).unwrap();
        assert_eq!(repo.head().unwrap().target(), Some(self.head));
        assert!(!self.app.bg_git_action_running);
    }

    fn assert_commit(&self, message: &str) {
        self.assert_commit_bytes(message.as_bytes());
    }

    fn assert_commit_bytes(&self, message: &[u8]) {
        let repo = git2::Repository::open(self.directory.path()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        assert_ne!(head.id(), self.head);
        assert_eq!(head.message_raw_bytes(), message);
        assert_eq!(head.tree_id(), self.staged_tree);
        assert_eq!(head.parent_count(), 1);
        assert_eq!(head.parent_id(0).unwrap(), self.parent);
        assert_eq!(head.author().name(), Some("Original author"));
        assert_eq!(head.committer().name(), Some("Test"));
        let reflog = repo.reflog("HEAD").unwrap();
        assert_eq!(reflog.get(0).unwrap().id_new(), head.id());
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
            fixture.submit(keyboard);
            fixture.finish();
            fixture.assert_commit(ORIGINAL);
            assert!(fixture.app.amend_commits.is_empty());
        }
    }
}

#[test]
fn amend_tab_places_both_actions_at_the_left_edge() {
    let mut fixture = Fixture::new(false);
    fixture.amend(true);
    let view = fixture.app.editor().unwrap().navigation_id;
    let mut terminal = Terminal::new(TestBackend::new(80, 10)).unwrap();

    for width in [70, 20, 11] {
        let area = Rect::new(4, 2, width, 6);
        terminal
            .draw(|frame| crate::ui_git_amend::draw(frame, &mut fixture.app, area))
            .unwrap();

        let buttons = fixture.app.amend_commits[&view].buttons.unwrap();
        assert_eq!(buttons.amend.x, area.x);
        assert_eq!(buttons.abort.x, buttons.amend.right() + 1);
        assert!(buttons.abort.right() <= area.right());

        let button_text = |button: Rect| {
            (button.x..button.right())
                .map(|x| terminal.backend().buffer()[(x, button.y)].symbol())
                .collect::<String>()
        };
        assert!(button_text(buttons.amend).contains("Amend"));
        assert!(button_text(buttons.abort).contains("Abort"));
    }
}

#[test]
fn explicit_multiline_amend_messages_keep_every_paragraph_and_trailer() {
    for remote in [false, true] {
        for keyboard in [false, true] {
            let mut fixture = Fixture::new(remote);
            fixture.app.source_control_commit_msg = "Next commit".into();
            fixture.amend(keyboard);
            fixture.edit(REPLACEMENT);
            fixture.key(KeyCode::Char('s'), KeyModifiers::CONTROL);
            fixture.key(
                KeyCode::Char('s'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            );
            assert!(!fixture.app.save_as_active);
            fixture.assert_unchanged();
            fixture.submit(keyboard);
            fixture.finish();
            fixture.assert_commit(REPLACEMENT);
            assert_eq!(fixture.app.source_control_commit_msg, "Next commit");
            assert!(fixture.app.amend_commits.is_empty());
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
        fixture.submit(true);
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
fn message_only_amend_keeps_the_tree_and_rejects_a_blank_draft() {
    let mut fixture = Fixture::new(false);
    let repo = git2::Repository::open(fixture.directory.path()).unwrap();
    let tree = repo.find_commit(fixture.head).unwrap().tree().unwrap();
    let mut index = repo.index().unwrap();
    index.read_tree(&tree).unwrap();
    index.write().unwrap();
    fixture.staged_tree = tree.id();
    fixture.app.source_control_files.clear();
    fixture.amend(false);
    fixture.edit(" \n\t");
    fixture.key(KeyCode::Char('a'), KeyModifiers::ALT);
    fixture.assert_unchanged();
    assert!(
        fixture
            .render()
            .contains("Enter a commit message before amending.")
    );
    let mut terminal = Terminal::new(TestBackend::new(20, 1)).unwrap();
    terminal
        .draw(|frame| crate::ui_git_amend::draw(frame, &mut fixture.app, frame.area()))
        .unwrap();
    fixture.edit(REPLACEMENT);
    fixture.submit(true);
    fixture.finish();
    fixture.assert_commit(REPLACEMENT);
}

#[test]
fn abort_button_and_tab_close_keep_head_and_index_intact() {
    let mut fixture = Fixture::new(false);
    fixture.amend(false);
    fixture.edit(REPLACEMENT);
    fixture.key(KeyCode::Char('w'), KeyModifiers::CONTROL);
    let screen = fixture.render();
    assert!(screen.contains("Abort Amend?"), "{screen}");
    assert!(screen.contains("Keep editing"), "{screen}");
    fixture.key(KeyCode::Enter, KeyModifiers::NONE);
    assert_eq!(fixture.app.editor().unwrap().rope.to_string(), REPLACEMENT);
    assert!(!fixture.app.confirm_close_active);
    fixture.key(KeyCode::Char('w'), KeyModifiers::CONTROL);
    fixture.key(KeyCode::Char('y'), KeyModifiers::NONE);
    assert!(fixture.app.amend_commits.is_empty());
    fixture.assert_unchanged();

    fixture.amend(true);
    let view = fixture.app.editor().unwrap().navigation_id;
    fixture.amend(true);
    assert_eq!(fixture.app.editor().unwrap().navigation_id, view);
    fixture.app.close_saved_tabs();
    assert!(fixture.app.amend_tab_is_active());
    fixture.render();
    let tabs =
        crate::ui_chrome_tabs::TabBarHits::new(&fixture.app, fixture.app.tab_bar_area.unwrap());
    let (_, _, close) = tabs.tabs[0];
    fixture.click(close);
    assert!(fixture.render().contains("Abort Amend?"));
    fixture.key(KeyCode::Esc, KeyModifiers::NONE);
    fixture.render();
    let abort = fixture
        .app
        .amend_commits
        .values()
        .next()
        .unwrap()
        .buttons
        .unwrap()
        .abort;
    fixture.click(abort);
    assert!(fixture.app.amend_commits.is_empty());
    fixture.assert_unchanged();
    let repo = git2::Repository::open(fixture.directory.path()).unwrap();
    assert_eq!(
        repo.index().unwrap().write_tree().unwrap(),
        fixture.staged_tree
    );
}

#[test]
fn changing_branches_at_the_same_commit_keeps_the_draft_and_both_refs() {
    for remote in [false, true] {
        let mut fixture = Fixture::new(remote);
        fixture.amend(true);
        fixture.edit(REPLACEMENT);
        let repo = git2::Repository::open(fixture.directory.path()).unwrap();
        let original_ref = repo.head().unwrap().name().unwrap().to_owned();
        repo.branch("different", &repo.find_commit(fixture.head).unwrap(), false)
            .unwrap();
        repo.set_head("refs/heads/different").unwrap();
        fixture.submit(true);
        fixture.finish();
        fixture.assert_unchanged();
        assert_eq!(
            repo.find_reference(&original_ref).unwrap().target(),
            Some(fixture.head)
        );
        assert_eq!(fixture.app.editor().unwrap().rope.to_string(), REPLACEMENT);
        assert!(!fixture.app.editor().unwrap().read_only);
        assert!(fixture.render().contains("HEAD changed."));
    }
}

#[test]
fn detached_head_amend_closes_its_inactive_pane_and_keeps_the_branch() {
    let mut fixture = Fixture::new(false);
    let repo = git2::Repository::open(fixture.directory.path()).unwrap();
    let original_ref = repo.head().unwrap().name().unwrap().to_owned();
    repo.set_head_detached(fixture.head).unwrap();
    fixture.app.new_file();
    let other = fixture.app.editor().unwrap().navigation_id;
    fixture.amend(true);
    fixture.edit(REPLACEMENT);
    let view = fixture.app.editor().unwrap().navigation_id;
    fixture.app.create_split_group(fixture.app.active_tab);
    fixture.app.switch_group(0);
    assert_eq!(fixture.app.editor().unwrap().navigation_id, other);
    let screen = fixture.render();
    assert!(screen.contains("Detached HEAD"), "{screen}");
    fixture.click(fixture.app.amend_commits[&view].buttons.unwrap().amend);
    fixture.finish();
    fixture.assert_commit(REPLACEMENT);
    assert_eq!(
        repo.find_reference(&original_ref).unwrap().target(),
        Some(fixture.head)
    );
    assert!(fixture.app.amend_commits.is_empty());
    assert_eq!(fixture.app.editor().unwrap().navigation_id, other);
}

#[test]
fn keeping_a_legacy_encoded_message_preserves_its_bytes() {
    let mut fixture = Fixture::new(false);
    let repo = git2::Repository::open(fixture.directory.path()).unwrap();
    let commit = repo.find_commit(fixture.head).unwrap();
    let original = b"\nSubject caf\xe9\n\nBody\n";
    let mut object = commit.raw_header_bytes().to_vec();
    object.extend_from_slice(b"encoding ISO-8859-1\n\n");
    object.extend_from_slice(original);
    fixture.head = repo
        .odb()
        .unwrap()
        .write(git2::ObjectType::Commit, &object)
        .unwrap();
    repo.head()
        .unwrap()
        .set_target(fixture.head, "legacy commit")
        .unwrap();
    let draft = crate::git_amend::read_message(fixture.directory.path()).unwrap();
    assert_eq!(draft.text, "\nSubject café\n\nBody\n");
    crate::git_amend::execute(fixture.directory.path(), draft.target, None).unwrap();
    fixture.assert_commit_bytes(original);
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(commit.message_encoding(), Some("ISO-8859-1"));
    let draft = crate::git_amend::read_message(fixture.directory.path()).unwrap();
    crate::git_amend::execute(fixture.directory.path(), draft.target, Some(REPLACEMENT)).unwrap();
    fixture.assert_commit(REPLACEMENT);
    let commit = repo.head().unwrap().peel_to_commit().unwrap();
    assert_eq!(commit.message_encoding(), Some("UTF-8"));
}
