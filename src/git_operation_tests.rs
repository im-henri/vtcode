use super::*;
use crate::{
    app::{ActivePane, App, SidebarView, SourceControlRepositoryState, Tab},
    editor::Editor,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use std::{
    path::Path,
    process::{Command, Output},
    sync::Arc,
    time::Duration,
};

const MESSAGE: &str = "Topic subject\n\nThe complete original body.\nSecond body line.\n";

struct Fixture {
    directory: tempfile::TempDir,
    commits: Vec<String>,
    main: String,
}

fn git(root: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(root)
        .args(args)
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}
fn checked(root: &Path, args: &[&str]) {
    let output = git(root, args);
    assert!(
        output.status.success(),
        "{args:?}: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
fn commit(root: &Path, message: &str) -> String {
    let repo = Repository::open(root).unwrap();
    let mut index = repo.index().unwrap();
    index
        .add_all(["*"], git2::IndexAddOption::DEFAULT, None)
        .unwrap();
    index.write().unwrap();
    let tree_id = index.write_tree().unwrap();
    let tree = repo.find_tree(tree_id).unwrap();
    let signature = repo.signature().unwrap();
    let parent = repo.head().ok().and_then(|head| head.peel_to_commit().ok());
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        message,
        &tree,
        &parent.iter().collect::<Vec<_>>(),
    )
    .unwrap()
    .to_string()
}

impl Fixture {
    fn new(two: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let repo = Repository::init(root).unwrap();
        repo.set_head("refs/heads/main").unwrap();
        let mut config = repo.config().unwrap();
        config.set_str("user.name", "VTCode Test").unwrap();
        config.set_str("user.email", "test@example.com").unwrap();
        config.set_str("core.hooksPath", "hooks-disabled").unwrap();
        for name in [
            "core.autocrlf",
            "commit.gpgsign",
            "rebase.autoStash",
            "rebase.updateRefs",
        ] {
            config.set_bool(name, false).unwrap();
        }
        drop(config);
        drop(repo);
        std::fs::write(root.join("one.txt"), "base one\n").unwrap();
        std::fs::write(root.join("two.txt"), "base two\n").unwrap();
        commit(root, "Base\n");
        checked(root, &["checkout", "-b", "topic"]);
        std::fs::write(root.join("one.txt"), "topic one\n").unwrap();
        let mut commits = vec![commit(root, MESSAGE)];
        if two {
            std::fs::write(root.join("two.txt"), "topic two\n").unwrap();
            commits.push(commit(
                root,
                "Second subject\n\nSecond commit's full body.\n",
            ));
        }
        checked(root, &["checkout", "main"]);
        std::fs::write(root.join("one.txt"), "main one\n").unwrap();
        if two {
            std::fs::write(root.join("two.txt"), "main two\n").unwrap();
        }
        let main = commit(root, "Main changes\n");
        Self {
            directory,
            commits,
            main,
        }
    }
    fn root(&self) -> &Path {
        self.directory.path()
    }
    fn state(&self) -> State {
        read(&Repository::open(self.root()).unwrap())
            .unwrap()
            .unwrap()
    }
    fn head(&self) -> String {
        Repository::open(self.root())
            .unwrap()
            .head()
            .unwrap()
            .target()
            .unwrap()
            .to_string()
    }
    fn external_start(&self, kind: Kind) {
        let output = if kind == Kind::Rebase {
            checked(self.root(), &["checkout", "topic"]);
            git(self.root(), &["rebase", "main"])
        } else {
            let mut args = vec!["cherry-pick"];
            args.extend(self.commits.iter().map(String::as_str));
            git(self.root(), &args)
        };
        assert!(!output.status.success(), "fixture must stop for conflicts");
        assert_eq!(self.state().kind, kind);
    }
    fn resolve(&self, path: &str, value: &str) {
        std::fs::write(self.root().join(path), value).unwrap();
        crate::app_git::changes::stage(
            self.root(),
            &[crate::app::GitFileChange {
                directory: false,
                path: path.to_string(),
                status: '!',
                staged: false,
                original_path: None,
            }],
        )
        .unwrap();
    }
    fn continue_action(&self) -> Action {
        let state = self.state();
        Action::Continue {
            kind: state.kind,
            token: state.token,
        }
    }
    fn app(&self) -> App {
        let mut app = App::new_deferred(self.root().to_path_buf());
        app.settings_persistence_enabled = false;
        app.source_control_enabled = true;
        app.sidebar_view = SidebarView::SourceControl;
        app.active_pane = ActivePane::Explorer;
        app.show_explorer = true;
        app.sc_graph_collapsed = true;
        refresh(&mut app);
        app
    }
}

fn refresh(app: &mut App) {
    app.refresh_source_control_async();
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.bg_git_status_running && !app.bg_git_status_pending
    });
    assert_eq!(
        app.source_control_repository_state,
        SourceControlRepositoryState::Available
    );
}
fn render(app: &mut App, width: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, 28)).unwrap();
    terminal
        .draw(|frame| {
            crate::ui_source_control::draw_source_control_sidebar(frame, app, frame.area())
        })
        .unwrap();
    terminal.backend().buffer().clone()
}
fn text(buffer: &Buffer) -> String {
    buffer.content.iter().map(|cell| cell.symbol()).collect()
}

#[test]
fn terminal_started_operations_require_saved_staged_resolutions_and_preserve_complete_messages() {
    for kind in [Kind::Rebase, Kind::CherryPick] {
        let fixture = Fixture::new(false);
        fixture.external_start(kind);
        let head = fixture.head();
        let action = fixture.continue_action();
        assert!(
            execute(fixture.root(), action.clone())
                .unwrap_err()
                .contains("Resolve and stage")
        );
        assert_eq!(fixture.head(), head);
        std::fs::write(fixture.root().join("one.txt"), "resolved one\n").unwrap();
        assert!(
            fixture
                .state()
                .blocker
                .unwrap()
                .contains("Resolve and stage")
        );
        fixture.resolve("one.txt", "resolved one\n");
        assert!(fixture.state().blocker.is_none());
        let outcome = execute(fixture.root(), action).unwrap();
        assert!(outcome.message.contains("completed"));
        let repo = Repository::open(fixture.root()).unwrap();
        assert!(read(&repo).unwrap().is_none());
        assert_eq!(
            repo.head().unwrap().peel_to_commit().unwrap().message(),
            Some(MESSAGE)
        );
        assert_eq!(
            std::fs::read_to_string(fixture.root().join("one.txt")).unwrap(),
            "resolved one\n"
        );
    }
}

#[test]
fn continuation_rechecks_staged_markers_unstaged_edits_and_operation_identity() {
    for kind in [Kind::Rebase, Kind::CherryPick] {
        let fixture = Fixture::new(false);
        fixture.external_start(kind);
        let original = fixture.state();
        checked(fixture.root(), &["add", "one.txt"]); // Simulate external staging with markers still present.
        assert!(
            fixture
                .state()
                .blocker
                .unwrap()
                .contains("conflict markers")
        );
        assert!(
            execute(fixture.root(), fixture.continue_action())
                .unwrap_err()
                .contains("conflict markers")
        );
        fixture.resolve("one.txt", "resolved one\n");
        std::fs::write(fixture.root().join("one.txt"), "edited after staging\n").unwrap();
        assert!(fixture.state().blocker.unwrap().contains("stage"));
        assert!(execute(fixture.root(), fixture.continue_action()).is_err());
        fixture.resolve("one.txt", "resolved one\n");
        execute(
            fixture.root(),
            Action::Abort {
                kind,
                token: original.token.clone(),
            },
        )
        .unwrap();
        let head = fixture.head();
        assert!(
            execute(
                fixture.root(),
                Action::Continue {
                    kind,
                    token: original.token
                }
            )
            .is_err()
        );
        assert_eq!(fixture.head(), head);
    }
}

#[test]
fn continue_can_pause_on_the_next_commit_and_old_controls_cannot_act_on_it() {
    for kind in [Kind::Rebase, Kind::CherryPick] {
        let fixture = Fixture::new(true);
        fixture.external_start(kind);
        let first = fixture.continue_action();
        fixture.resolve("one.txt", "resolved one\n");
        let outcome = execute(fixture.root(), first.clone()).unwrap();
        assert!(outcome.message.contains("paused"));
        assert!(
            fixture
                .state()
                .blocker
                .unwrap()
                .contains("Resolve and stage")
        );
        assert!(
            execute(fixture.root(), first)
                .unwrap_err()
                .contains("operation changed")
        );
        fixture.resolve("two.txt", "resolved two\n");
        execute(fixture.root(), fixture.continue_action()).unwrap();
        assert!(
            read(&Repository::open(fixture.root()).unwrap())
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn start_abort_skip_and_empty_cherry_pick_use_native_git_state() {
    for kind in [Kind::Rebase, Kind::CherryPick] {
        for abort in [true, false] {
            let fixture = Fixture::new(false);
            if kind == Kind::Rebase {
                checked(fixture.root(), &["checkout", "topic"]);
            }
            let before = fixture.head();
            let target = if kind == Kind::Rebase {
                "main".into()
            } else {
                fixture.commits[0].clone()
            };
            let outcome = execute(fixture.root(), Action::Start { kind, target }).unwrap();
            assert!(outcome.message.contains("paused"));
            let state = fixture.state();
            let action = if abort {
                Action::Abort {
                    kind,
                    token: state.token,
                }
            } else {
                Action::Skip {
                    kind,
                    token: state.token,
                }
            };
            execute(fixture.root(), action).unwrap();
            assert!(
                read(&Repository::open(fixture.root()).unwrap())
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                fixture.head(),
                if abort { before } else { fixture.main.clone() }
            );
        }
    }
    let fixture = Fixture::new(false);
    fixture.external_start(Kind::CherryPick);
    fixture.resolve("one.txt", "main one\n");
    assert!(fixture.state().blocker.unwrap().contains("Skip Commit"));
    assert!(execute(fixture.root(), fixture.continue_action()).is_err());
    let state = fixture.state();
    execute(
        fixture.root(),
        Action::Skip {
            kind: state.kind,
            token: state.token,
        },
    )
    .unwrap();
    assert_eq!(fixture.head(), fixture.main);
}

#[test]
fn start_rejects_dirty_worktrees_invalid_targets_and_nested_operations() {
    let fixture = Fixture::new(false);
    let before = fixture.head();
    assert!(
        execute(
            fixture.root(),
            Action::Start {
                kind: Kind::CherryPick,
                target: "--abort".into()
            }
        )
        .is_err()
    );
    std::fs::write(fixture.root().join("one.txt"), "unsaved on disk\n").unwrap();
    assert!(
        execute(
            fixture.root(),
            Action::Start {
                kind: Kind::Rebase,
                target: "topic".into()
            }
        )
        .unwrap_err()
        .contains("Commit or stash")
    );
    assert_eq!(fixture.head(), before);
    std::fs::write(fixture.root().join("one.txt"), "main one\n").unwrap();
    fixture.external_start(Kind::CherryPick);
    assert!(
        execute(
            fixture.root(),
            Action::Start {
                kind: Kind::Rebase,
                target: "topic".into()
            }
        )
        .unwrap_err()
        .contains("current Git operation")
    );
    assert!(crate::app_git::commit_repository(fixture.root(), "wrong commit", false).is_err());
    assert!(
        crate::git_tasks::execute::<String>(
            fixture.root(),
            crate::git_tasks::Task::CreateBranch {
                branch_name: "must-not-exist".into()
            }
        )
        .is_err()
    );
    assert!(
        Repository::open(fixture.root())
            .unwrap()
            .find_branch("must-not-exist", git2::BranchType::Local)
            .is_err()
    );
    {
        let repo = Repository::open(fixture.root()).unwrap();
        repo.reference(
            "refs/remotes/origin/topic",
            fixture.commits[0].parse().unwrap(),
            false,
            "test remote reference",
        )
        .unwrap();
    }
    assert!(
        crate::git_tasks::execute::<String>(
            fixture.root(),
            crate::git_tasks::Task::CheckoutRemote {
                remote_ref: "origin/topic".into(),
                local_name: "must-not-exist".into(),
            }
        )
        .is_err()
    );
    assert!(
        Repository::open(fixture.root())
            .unwrap()
            .find_branch("must-not-exist", git2::BranchType::Local)
            .is_err()
    );
}

#[test]
fn rebase_apply_backend_and_empty_merge_backend_resolutions_continue_correctly() {
    for apply in [true, false] {
        let fixture = Fixture::new(false);
        checked(fixture.root(), &["checkout", "topic"]);
        let args: &[&str] = if apply {
            &["rebase", "--apply", "main"]
        } else {
            &["rebase", "main"]
        };
        assert!(!git(fixture.root(), args).status.success());
        let state = fixture.state();
        assert_eq!(state.kind, Kind::Rebase);
        fixture.resolve(
            "one.txt",
            if apply {
                "apply resolution\n"
            } else {
                "main one\n"
            },
        );
        assert!(fixture.state().blocker.is_none());
        execute(fixture.root(), fixture.continue_action()).unwrap();
        assert!(
            read(&Repository::open(fixture.root()).unwrap())
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn invalid_start_keeps_the_target_form_open_for_correction() {
    let fixture = Fixture::new(false);
    let mut app = fixture.app();
    app.start_git_operation_dialog(Kind::CherryPick);
    app.git_operation_ui.target = "missing-commit-reference".into();
    app.submit_git_operation_dialog();
    assert!(app.git_operation_ui.pending);
    assert!(app.git_operation_ui.request.is_some());
    app.wait_for_background(Duration::from_secs(10), |app| !app.bg_git_action_running);
    assert!(!app.git_operation_ui.pending);
    assert!(app.git_operation_ui.request.is_some());
    assert_eq!(app.git_operation_ui.target, "missing-commit-reference");
    assert!(
        app.modal
            .error
            .as_ref()
            .unwrap()
            .contains("valid branch or commit")
    );
    assert_eq!(fixture.head(), fixture.main);
}

#[test]
fn starting_an_already_applied_cherry_pick_exposes_skip_in_source_control() {
    let fixture = Fixture::new(false);
    let outcome = execute(
        fixture.root(),
        Action::Start {
            kind: Kind::CherryPick,
            target: fixture.main.clone(),
        },
    )
    .unwrap();
    assert!(outcome.message.contains("paused"));
    assert!(fixture.state().blocker.unwrap().contains("Skip Commit"));
}

#[test]
fn linked_worktrees_use_their_own_git_directory_and_leave_the_main_worktree_alone() {
    let fixture = Fixture::new(false);
    let linked = tempfile::tempdir().unwrap();
    let worktree = linked.path().join("linked");
    checked(
        fixture.root(),
        &[
            "worktree",
            "add",
            "-b",
            "linked-topic",
            worktree.to_str().unwrap(),
            "topic",
        ],
    );
    assert!(!git(&worktree, &["rebase", "main"]).status.success());
    let repo = Repository::open(&worktree).unwrap();
    assert!(worktree.join(".git").is_file());
    let state = read(&repo).unwrap().unwrap();
    std::fs::write(worktree.join("one.txt"), "linked resolution\n").unwrap();
    checked(&worktree, &["add", "one.txt"]);
    execute(
        &worktree,
        Action::Continue {
            kind: state.kind,
            token: state.token,
        },
    )
    .unwrap();
    assert!(read(&repo).unwrap().is_none());
    assert_eq!(fixture.head(), fixture.main);
    assert_eq!(
        std::fs::read_to_string(fixture.root().join("one.txt")).unwrap(),
        "main one\n"
    );
}

#[test]
fn remote_status_and_actions_can_continue_terminal_started_conflicts() {
    use crate::remote::{Command, RemotePath, Reply};
    for kind in [Kind::Rebase, Kind::CherryPick] {
        let fixture = Fixture::new(false);
        fixture.external_start(kind);
        let client =
            crate::remote::tests::connection(fixture.root(), Arc::default(), Arc::default());
        let Reply::GitStatus { scans, .. } = client
            .call(Command::GitStatus { revalidate: false,
                changed: None,
                include_untracked: false,
                known: None,
                parent_open: false,
                closed: vec![],
            })
            .unwrap()
        else {
            panic!("status reply")
        };
        let state = scans[0].operation.clone().unwrap();
        assert_eq!(state.kind, kind);
        let task = crate::git_tasks::Task::Operation(Action::Continue {
            kind,
            token: state.token,
        });
        assert!(
            client
                .call(Command::GitAction {
                    repo: RemotePath::default(),
                    task
                })
                .is_err()
        );
        fixture.resolve("one.txt", "remote resolution\n");
        let task = crate::git_tasks::Task::Operation(fixture.continue_action());
        let Reply::GitAction(outcome) = client
            .call(Command::GitAction {
                repo: RemotePath::default(),
                task,
            })
            .unwrap()
        else {
            panic!("action reply")
        };
        assert!(outcome.message.contains("completed"));
        assert!(
            read(&Repository::open(fixture.root()).unwrap())
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn metadata_events_reveal_terminal_started_operations_and_ui_actions_complete_them() {
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    for kind in [Kind::Rebase, Kind::CherryPick] {
        let fixture = Fixture::new(false);
        if kind == Kind::Rebase {
            checked(fixture.root(), &["checkout", "topic"]);
        }
        let mut app = fixture.app();
        assert!(app.current_git_operation().is_none());
        app.start_git_operation_dialog(kind);
        assert!(
            app.sidebar_is_visible(),
            "opening the form keeps Source Control visible"
        );
        app.git_operation_ui.target = if kind == Kind::Rebase {
            "main".into()
        } else {
            fixture.commits[0].clone()
        };
        let mut terminal = Terminal::new(TestBackend::new(120, 38)).unwrap();
        terminal
            .draw(|frame| crate::ui_modal::draw_active(frame, &mut app))
            .unwrap();
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        assert!(app.bg_git_action_running);
        app.wait_for_background(Duration::from_secs(10), |app| {
            !app.bg_git_action_running && !app.bg_git_status_running && !app.bg_git_status_pending
        });
        assert_eq!(app.current_git_operation().unwrap().kind, kind);
        fixture.resolve("one.txt", "ui resolution\n");
        // Exercise the same invalidation used by an external `git add`.
        let root = app.explorer.root.clone();
        app.apply_workspace_file_changes(root, vec![], false, false, true);
        app.wait_for_background(Duration::from_secs(10), |app| {
            !app.bg_git_status_running && !app.bg_git_status_pending
        });
        assert!(app.git_operation_blocker().is_none());
        terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .unwrap();
        let button = app
            .git_operation_ui
            .buttons
            .iter()
            .find(|(_, index, _, _)| *index == 0)
            .unwrap()
            .0;
        crate::mouse::handle_mouse(
            &mut app,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: button.x,
                row: button.y,
                modifiers: KeyModifiers::NONE,
            },
        );
        assert!(app.bg_git_action_running);
        app.wait_for_background(Duration::from_secs(10), |app| {
            !app.bg_git_action_running && !app.bg_git_status_running && !app.bg_git_status_pending
        });
        assert!(app.current_git_operation().is_none());
        assert!(
            read(&Repository::open(fixture.root()).unwrap())
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn a_restarted_operation_and_another_repository_reject_stale_controls() {
    for kind in [Kind::Rebase, Kind::CherryPick] {
        let fixture = Fixture::new(false);
        fixture.external_start(kind);
        let first = fixture.state();
        execute(
            fixture.root(),
            Action::Abort {
                kind,
                token: first.token.clone(),
            },
        )
        .unwrap();
        fixture.external_start(kind);
        assert_ne!(fixture.state().token, first.token);
        assert!(
            execute(
                fixture.root(),
                Action::Abort {
                    kind,
                    token: first.token
                }
            )
            .unwrap_err()
            .contains("operation changed")
        );
        fixture.resolve("one.txt", "resolved one\n");
        let mut app = fixture.app();
        render(&mut app, 48);
        let button = app
            .git_operation_ui
            .buttons
            .iter()
            .find(|(_, index, _, _)| *index == 0)
            .unwrap()
            .0;
        let other = Fixture::new(false);
        app.repositories.active = Some(other.root().to_path_buf());
        assert!(crate::ui_git_operation::click(&mut app, button.x, button.y));
        assert!(!app.bg_git_action_running);
        assert!(fixture.state().blocker.is_none());
    }
}

#[test]
fn conflict_examples_already_present_in_the_replayed_commit_do_not_block_continue() {
    for kind in [Kind::Rebase, Kind::CherryPick] {
        let mut fixture = Fixture::new(false);
        checked(fixture.root(), &["checkout", "topic"]);
        std::fs::write(
            fixture.root().join("example.txt"),
            "<<<<<<< HEAD\nexample current\n=======\nexample incoming\n>>>>>>> example\n",
        )
        .unwrap();
        {
            let repo = Repository::open(fixture.root()).unwrap();
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("example.txt")).unwrap();
            index.write().unwrap();
            let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
            fixture.commits[0] = repo
                .head()
                .unwrap()
                .peel_to_commit()
                .unwrap()
                .amend(Some("HEAD"), None, None, None, None, Some(&tree))
                .unwrap()
                .to_string();
        }
        checked(fixture.root(), &["checkout", "main"]);
        fixture.external_start(kind);
        fixture.resolve("one.txt", "resolved one\n");
        assert!(fixture.state().blocker.is_none());
        execute(fixture.root(), fixture.continue_action()).unwrap();
    }
}

#[test]
fn source_control_detects_external_state_explains_blockers_and_keeps_commit_drafts() {
    for kind in [Kind::Rebase, Kind::CherryPick] {
        let fixture = Fixture::new(false);
        fixture.external_start(kind);
        let mut app = fixture.app();
        assert_eq!(app.current_git_operation().unwrap().kind, kind);
        app.source_control_commit_msg = "Keep this draft for later".into();
        for (theme_name, theme) in [
            ("dark", crate::app_theme::EditorTheme::Dark),
            ("light", crate::app_theme::EditorTheme::Light),
        ] {
            app.colors = theme.colors();
            for width in [24, 48] {
                let buffer = render(&mut app, width);
                preview(
                    &buffer,
                    &format!("{theme_name}-{}-{width}-conflicts", kind.command()),
                );
                assert!(text(&buffer).contains(kind.name()));
                assert!(text(&buffer).contains("Resolve"));
                assert!(
                    app.source_control_commit_area.is_none()
                        && app.source_control_amend_area.is_none()
                );
                assert!(
                    app.text_input_area(crate::text_input::TextInputId::SourceControlCommit)
                        .is_none()
                );
            }
        }
        app.continue_git_operation();
        assert!(!app.bg_git_action_running);
        fixture.resolve("one.txt", "resolved one\n");
        refresh(&mut app);
        assert!(app.git_operation_blocker().is_none());
        let mut editor = Editor::test_with_text("unsaved resolution\n");
        editor.file_path = Some(app.source_control_root().join("one.txt"));
        editor.modified = true;
        app.tabs.push(Tab::new_file(editor));
        assert!(app.git_operation_blocker().unwrap().contains("Save"));
        app.continue_git_operation();
        assert!(!app.bg_git_action_running);
        app.tabs.clear();
        app.colors = crate::app_theme::EditorTheme::Dark.colors();
        let buffer = render(&mut app, 48);
        preview(&buffer, &format!("dark-{}-48-ready", kind.command()));
        assert!(text(&buffer).contains("Ready"));
        crate::ui_source_control::handle_key(&mut app, KeyCode::Char('m'), KeyModifiers::NONE);
        crate::ui_source_control::handle_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        app.wait_for_background(Duration::from_secs(10), |app| {
            !app.bg_git_action_running && !app.bg_git_status_running && !app.bg_git_status_pending
        });
        assert!(app.current_git_operation().is_none());
        assert_eq!(app.source_control_commit_msg, "Keep this draft for later");
    }
}

#[test]
fn operation_dialogs_support_keyboard_editing_cancel_and_abort_confirmation() {
    let fixture = Fixture::new(false);
    let mut app = fixture.app();
    app.start_git_operation_dialog(Kind::CherryPick);
    assert_eq!(
        crate::ui_modal::active(&app),
        Some(crate::ui_modal::Kind::GitOperationTarget)
    );
    app.git_operation_ui.target = fixture.commits[0].clone();
    app.git_operation_ui.cursor = app.git_operation_ui.target.len();
    let mut terminal = Terminal::new(TestBackend::new(100, 36)).unwrap();
    terminal
        .draw(|frame| crate::ui_modal::draw_active(frame, &mut app))
        .unwrap();
    preview(terminal.backend().buffer(), "start-cherry-pick");
    assert!(text(terminal.backend().buffer()).contains("Cherry-pick Commit"));
    crate::ui_modal::handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.git_operation_ui.request.is_none());
    assert_eq!(fixture.head(), fixture.main);
    fixture.external_start(Kind::CherryPick);
    refresh(&mut app);
    let state = fixture.state();
    app.confirm_git_operation_action(crate::ui_git_operation::Choice::Abort);
    terminal
        .draw(|frame| crate::ui_modal::draw_active(frame, &mut app))
        .unwrap();
    assert_eq!(app.modal.focus, 1, "Abort confirmation starts on Cancel");
    assert!(text(terminal.backend().buffer()).contains("discards"));
    crate::ui_modal::handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.git_operation_ui.request.is_none());
    assert_eq!(fixture.state().token, state.token);
    app.confirm_git_operation_action(crate::ui_git_operation::Choice::Abort);
    app.submit_git_operation_dialog();
    app.wait_for_background(Duration::from_secs(10), |app| {
        !app.bg_git_action_running && !app.bg_git_status_running
    });
    assert!(
        read(&Repository::open(fixture.root()).unwrap())
            .unwrap()
            .is_none()
    );
}

fn preview(buffer: &Buffer, name: &str) {
    if std::env::var_os("VTCODE_GIT_OPERATION_PREVIEW").is_none() {
        return;
    }
    let rgb = |color| match color {
        ratatui::style::Color::Rgb(r, g, b) => [r, g, b],
        _ => [128, 128, 128],
    };
    let cells: Vec<_> = buffer
        .content
        .iter()
        .map(|cell| {
            serde_json::json!({
                "text": cell.symbol(), "fg": rgb(cell.fg), "bg": rgb(cell.bg),
                "bold": cell.modifier.contains(ratatui::style::Modifier::BOLD),
            })
        })
        .collect();
    std::fs::write(
        format!("target/git-operation-{name}.json"),
        serde_json::to_vec(&serde_json::json!({
            "width": buffer.area.width, "height": buffer.area.height, "cells": cells,
        }))
        .unwrap(),
    )
    .unwrap();
}
