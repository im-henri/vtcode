use super::*;
use crate::{
    app::{App, Tab},
    editor::Editor,
};

fn tree() -> (tempfile::TempDir, PathBuf, Vec<PathBuf>) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tree");
    std::fs::create_dir(&root).unwrap();
    let paths: Vec<_> = (0..4).map(|i| root.join(format!("file{i}.txt"))).collect();
    for path in &paths {
        std::fs::write(path, "original\n").unwrap();
    }
    (dir, root, paths)
}

fn run(targets: Vec<PathBuf>, cancel: &CancellationToken) -> (FilesystemActionOutcome, Progress) {
    let mut last = Progress::default();
    let result = perform(targets, cancel, |progress, _| last = progress);
    (result, last)
}

#[test]
fn nested_deletion_counts_files_and_directories_and_deduplicates_selected_children() {
    let (dir, root, paths) = tree();
    let nested = root.join("nested");
    std::fs::create_dir(&nested).unwrap();
    std::fs::create_dir(nested.join("empty")).unwrap();
    std::fs::write(nested.join("child.txt"), "child").unwrap();
    let keep = dir.path().join("keep.txt");
    std::fs::write(&keep, "keep").unwrap();
    let (outcome, last) = run(
        vec![paths[0].clone(), root.clone(), root.clone(), nested],
        &CancellationToken::new(),
    );
    assert_eq!(last.phase, Phase::Complete);
    assert_eq!(last.total, 8);
    assert_eq!(last.deleted, 8);
    assert_eq!(last.processed, 8);
    assert!(!outcome.warning);
    assert_eq!(outcome.deleted.len(), 1);
    assert!(!root.exists());
    assert_eq!(std::fs::read_to_string(keep).unwrap(), "keep");
}

#[test]
fn cancelling_during_counting_does_not_modify_anything() {
    let (_dir, root, paths) = tree();
    let cancel = CancellationToken::new();
    let mut last = Progress::default();
    let outcome = perform(vec![root.clone()], &cancel, |progress, _| {
        last = progress;
        if progress.phase == Phase::Counting && progress.total == 3 {
            cancel.cancel();
        }
    });
    assert_eq!(last.phase, Phase::Cancelled);
    assert!(!last.total_known);
    assert_eq!(last.deleted, 0);
    assert!(outcome.deleted.is_empty());
    assert!(root.exists());
    assert!(paths.iter().all(|path| path.exists()));
}

#[test]
fn cancellation_after_first_unlink_keeps_all_remaining_entries() {
    let (_dir, root, paths) = tree();
    let cancel = CancellationToken::new();
    let mut last = Progress::default();
    let outcome = perform(vec![root.clone()], &cancel, |progress, _| {
        last = progress;
        if progress.deleted == 1 {
            cancel.cancel();
        }
    });
    assert_eq!(last.phase, Phase::Cancelled);
    assert_eq!(last.total, 5);
    assert!(last.total_known);
    assert_eq!(last.processed, 1);
    assert_eq!(last.deleted, 1);
    assert!(
        outcome.deleted.is_empty(),
        "a partly deleted directory must not close every child tab"
    );
    assert!(root.exists());
    assert_eq!(paths.iter().filter(|path| path.exists()).count(), 3);
    assert!(outcome.message.contains("remaining items kept"));
}

#[test]
fn queued_cancellation_never_probes_or_deletes_the_path() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let (outcome, progress) = run(vec![PathBuf::from("missing-parent/missing-file")], &cancel);
    assert_eq!(progress.phase, Phase::Cancelled);
    assert_eq!(progress.deleted, 0);
    assert!(!outcome.warning);
}

#[test]
fn tree_changes_between_counting_and_deleting_do_not_break_completion_counts() {
    for added in [true, false] {
        let (_dir, root, paths) = tree();
        let cancel = CancellationToken::new();
        let mut last = Progress::default();
        perform(vec![root.clone()], &cancel, |progress, force| {
            last = progress;
            if progress.phase == Phase::Deleting && force {
                if added {
                    std::fs::write(root.join("new.txt"), "new").unwrap();
                } else {
                    std::fs::remove_file(&paths[0]).unwrap();
                }
            }
        });
        assert_eq!(last.phase, Phase::Complete);
        assert_eq!(last.total, last.processed);
        assert_eq!(last.total, if added { 6 } else { 4 });
        assert!(!root.exists());
    }
}

#[cfg(unix)]
#[test]
fn links_inside_directories_and_selected_directory_links_never_delete_the_target() {
    use std::os::unix::fs::symlink;
    let (dir, root, _) = tree();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let keep = outside.join("keep.txt");
    std::fs::write(&keep, "keep").unwrap();
    symlink(&outside, root.join("link")).unwrap();
    symlink(dir.path().join("does-not-exist"), root.join("dangling")).unwrap();
    let selected = dir.path().join("selected-link");
    symlink(&outside, &selected).unwrap();
    let (_, progress) = run(
        vec![root.clone(), selected.clone()],
        &CancellationToken::new(),
    );
    assert_eq!(progress.phase, Phase::Complete);
    assert!(!root.exists());
    assert!(std::fs::symlink_metadata(selected).is_err());
    assert_eq!(std::fs::read_to_string(keep).unwrap(), "keep");
}

#[cfg(windows)]
#[test]
fn windows_junctions_are_removed_without_deleting_their_targets() {
    let (dir, root, _) = tree();
    let outside = dir.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let keep = outside.join("keep.txt");
    std::fs::write(&keep, "keep").unwrap();
    let selected = dir.path().join("selected-junction");
    for link in [root.join("junction"), selected.clone()] {
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let (_, progress) = run(vec![root.clone(), selected], &CancellationToken::new());
    assert_eq!(progress.phase, Phase::Complete);
    assert!(!root.exists());
    assert_eq!(std::fs::read_to_string(keep).unwrap(), "keep");
}

#[cfg(unix)]
#[test]
fn directory_replaced_by_a_link_after_counting_is_not_followed() {
    let (dir, root, _) = tree();
    let outside = dir.path().join("outside");
    let moved = dir.path().join("moved");
    std::fs::create_dir(&outside).unwrap();
    let keep = outside.join("keep.txt");
    std::fs::write(&keep, "keep").unwrap();
    let (_, progress) = {
        let mut last = Progress::default();
        let outcome = perform(
            vec![root.clone()],
            &CancellationToken::new(),
            |progress, force| {
                last = progress;
                if progress.phase == Phase::Deleting && force {
                    std::fs::rename(&root, &moved).unwrap();
                    std::os::unix::fs::symlink(&outside, &root).unwrap();
                }
            },
        );
        (outcome, last)
    };
    assert_eq!(progress.phase, Phase::Complete);
    assert_eq!(progress.deleted, 1);
    assert!(moved.join("file0.txt").exists());
    assert_eq!(std::fs::read_to_string(keep).unwrap(), "keep");
}

#[test]
fn invalid_target_fails_preparation_without_deleting_valid_siblings() {
    let (_dir, root, paths) = tree();
    let (outcome, progress) = run(
        vec![paths[0].clone(), root.join("absent-parent/file")],
        &CancellationToken::new(),
    );
    assert_eq!(progress.phase, Phase::Failed);
    assert!(outcome.warning);
    assert_eq!(progress.deleted, 0);
    assert!(paths.iter().all(|path| path.exists()));
}

#[test]
fn deep_directories_work_beyond_the_bounded_handle_cache() {
    let (_dir, root, _) = tree();
    let mut parent = root.clone();
    for _ in 0..40 {
        parent.push("d");
        std::fs::create_dir(&parent).unwrap();
        std::fs::write(parent.join("file"), "text").unwrap();
    }
    let (_, progress) = run(vec![root.clone()], &CancellationToken::new());
    assert_eq!(progress.phase, Phase::Complete);
    assert_eq!(progress.failed, 0);
    assert!(!root.exists());
}

#[test]
fn mid_traversal_directory_replacement_cannot_delete_files_through_its_link() {
    let (dir, root, _) = tree();
    let selected = root.join("inside");
    let outside = dir.path().join("outside");
    let moved = dir.path().join("moved");
    std::fs::create_dir(&selected).unwrap();
    std::fs::create_dir(&outside).unwrap();
    for name in ["one", "two", "three"] {
        std::fs::write(selected.join(name), "selected").unwrap();
        std::fs::write(outside.join(name), "keep").unwrap();
    }
    let mut swapped = false;
    perform(
        vec![selected.clone()],
        &CancellationToken::new(),
        |progress, _| {
            if progress.deleted == 1 && !swapped {
                swapped = true;
                std::fs::rename(&selected, &moved).unwrap();
                #[cfg(unix)]
                std::os::unix::fs::symlink(&outside, &selected).unwrap();
                #[cfg(windows)]
                {
                    let output = std::process::Command::new("cmd")
                        .args(["/C", "mklink", "/J"])
                        .arg(&selected)
                        .arg(&outside)
                        .output()
                        .unwrap();
                    assert!(output.status.success());
                }
            }
        },
    );
    assert!(swapped);
    for name in ["one", "two", "three"] {
        assert_eq!(std::fs::read_to_string(outside.join(name)).unwrap(), "keep");
    }
}

#[cfg(windows)]
#[test]
fn locked_windows_file_reports_partial_failure_and_retains_its_parent() {
    use std::os::windows::fs::OpenOptionsExt;
    let (_dir, root, paths) = tree();
    let locked = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&paths[0])
        .unwrap();
    let (outcome, progress) = run(vec![root.clone()], &CancellationToken::new());
    assert_eq!(progress.phase, Phase::Failed);
    assert!(progress.failed > 0);
    assert!(outcome.warning);
    assert!(outcome.deleted.is_empty());
    assert!(root.exists() && paths[0].exists());
    assert!(paths[1..].iter().all(|path| !path.exists()));
    drop(locked);
}

#[test]
fn cancelled_partial_deletion_recovers_only_deleted_dirty_files() {
    let (_dir, root, paths) = tree();
    let mut app = App::test_new();
    app.tabs = paths
        .iter()
        .map(|path| {
            let mut editor = Editor::test_with_text("original\n");
            editor.file_path = Some(path.clone());
            editor.saved_rope = Some(editor.rope.clone());
            editor.insert_char('!');
            Tab::new_file(editor)
        })
        .collect();
    let cancel = CancellationToken::new();
    let outcome = perform(vec![root.clone()], &cancel, |progress, _| {
        if progress.deleted == 1 {
            cancel.cancel();
        }
    });
    app.background
        .sender
        .send(BackgroundResult::FilesystemActionDone {
            action: "Delete",
            result: Ok(outcome),
        })
        .unwrap();
    app.poll_background_results();
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.tabs.iter().any(|tab| tab.editor.file_path.is_none())
    });
    assert_eq!(app.tabs.len(), 4);
    assert_eq!(
        app.tabs
            .iter()
            .filter(|tab| tab.editor.file_path.is_none())
            .count(),
        1
    );
    for tab in &mut app.tabs {
        assert!(tab.editor.modified);
        assert_eq!(tab.editor.rope.to_string(), "!original\n");
        if let Some(path) = &tab.editor.file_path {
            assert!(path.exists());
        }
        tab.editor.undo();
        assert_eq!(tab.editor.rope.to_string(), "original\n");
    }
}

fn card() -> App {
    let mut app = App::test_new();
    app.file_deletion = Some(Deletion {
        shared: Arc::new(Shared::default()),
        cancel: CancellationToken::new(),
        label: "A folder".into(),
        started: Instant::now(),
        finished: None,
        shown: false,
        area: None,
        button: None,
    });
    app
}

fn draw(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|f| crate::ui_file_deletion::draw(f, app))
        .unwrap();
    terminal.backend().buffer().clone()
}

#[test]
fn progress_card_has_no_fast_operation_flash_and_cancel_does_not_click_the_editor() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let mut app = card();
    draw(&mut app, 100, 30);
    assert!(app.file_deletion.as_ref().unwrap().area.is_none());
    app.file_deletion.as_mut().unwrap().started -= SHOW_DELAY;
    let buffer = draw(&mut app, 100, 30);
    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    assert!(text.contains("Deletion queued"));
    let button = app.file_deletion.as_ref().unwrap().button.unwrap();
    crate::mouse::handle_mouse(
        &mut app,
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: button.x,
            row: button.y,
            modifiers: KeyModifiers::NONE,
        },
    );
    assert!(app.file_deletion.as_ref().unwrap().cancel.is_cancelled());
    let buffer = draw(&mut app, 100, 30);
    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    assert!(text.contains("Stopping deletion"));
    app.file_deletion.as_mut().unwrap().finished = Some(Instant::now() - FINISHED_DELAY);
    assert!(app.update_file_deletion_timer());
    assert!(app.file_deletion.is_none());
}

#[test]
fn progress_updates_coalesce_even_when_ui_is_busy() {
    let workers = crate::background::BackgroundWorkers::new();
    let shared = Shared::default();
    let cancel = CancellationToken::new();
    let mut reporter = Reporter {
        shared: &shared,
        sender: &workers.sender,
        cancel: &cancel,
        last: Instant::now(),
    };
    for total in 0..10_000 {
        reporter.publish(
            Progress {
                total,
                ..Default::default()
            },
            true,
        );
    }
    assert_eq!(shared.snapshot().total, 9_999);
    assert_eq!(workers.receiver.try_iter().count(), 1);
    shared.acknowledge();
    reporter.publish(
        Progress {
            total: 10_000,
            ..Default::default()
        },
        true,
    );
    assert_eq!(workers.receiver.try_iter().count(), 1);
}

#[test]
fn hidden_or_tiny_progress_ui_does_not_keep_requesting_immediate_redraws() {
    let mut app = card();
    app.file_deletion.as_mut().unwrap().started -= SHOW_DELAY;
    app.terminal_has_focus = false;
    assert!(!app.update_file_deletion_timer());
    app.terminal_has_focus = true;
    assert!(app.update_file_deletion_timer());
    draw(&mut app, 10, 3);
    assert!(app.file_deletion.as_ref().unwrap().area.is_none());
    assert_eq!(app.file_deletion.as_ref().unwrap().next_wait(), None);
    assert!(!app.update_file_deletion_timer());
}

#[test]
fn confirming_explorer_delete_runs_the_worker_and_finishes_the_operation() {
    let (_dir, root, _) = tree();
    let mut app = App::test_new();
    app.request_explorer_delete(root.clone());
    assert!(app.confirm_delete_active);
    app.execute_confirm_delete(true);
    assert!(!app.confirm_delete_active);
    assert!(app.bg_filesystem_action_running);
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.bg_filesystem_action_running
    });
    assert!(!root.exists());
    assert!(
        app.notification_history
            .iter()
            .any(|note| note.0 == "Deleted 5 items")
    );
    assert!(
        app.file_deletion
            .as_ref()
            .is_none_or(|deletion| deletion.finished.is_some())
    );
}

#[test]
fn fast_failure_still_shows_a_dismissible_result() {
    let mut app = card();
    let deletion = app.file_deletion.as_mut().unwrap();
    deletion.started = Instant::now() + Duration::from_secs(30);
    *deletion.shared.progress.lock().unwrap() = Progress {
        phase: Phase::Failed,
        failed: 1,
        ..Default::default()
    };
    app.background
        .sender
        .send(BackgroundResult::FilesystemActionDone {
            action: "Delete",
            result: Ok(FilesystemActionOutcome {
                message: "Permission denied".into(),
                warning: true,
                ..Default::default()
            }),
        })
        .unwrap();
    app.poll_background_results();
    assert!(app.file_deletion.as_ref().unwrap().finished.is_some());
    let buffer = draw(&mut app, 100, 30);
    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
    assert!(text.contains("Deletion needs attention"));
    assert!(text.contains("Close"));
    assert!(app.notification_history.iter().any(|note| note.0 == "Permission denied" && note.1 == crate::app::NotifyLevel::Warning));
}
