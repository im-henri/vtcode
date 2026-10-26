use super::collect_source_control_status_with_progress;
use crate::app::{App, GitFileChange, SourceControlRepositoryState as State};
use crate::background::BackgroundResult;
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer, layout::Rect};
use std::time::Duration;

fn read_result(result: &BackgroundResult) -> &BackgroundResult {
    match result {
        BackgroundResult::SourceControlRead { result, .. } => result,
        result => result,
    }
}

fn change() -> GitFileChange {
    GitFileChange {
        path: "new.txt".into(),
        original_path: None,
        status: '?',
        staged: false,
    }
}

// Exercise the production multi-repository result, including its workspace and
// membership lifetime checks, rather than a retired single-repository handler.
fn status(
    root: std::path::PathBuf,
    workspace_generation: u64,
    repository_generation: u64,
) -> BackgroundResult {
    BackgroundResult::GitRepositories {
        discovery: crate::git_repositories::Discovery {
            workspace: root.clone(),
            roots: vec![root.clone()],
            parent: None,
        },
        scans: vec![crate::git_repositories::Scan {
            root: root.clone(),
            files: vec![change()],
            branch: String::new(),
            worktree_paths: None,
            include_untracked: true,
        }],
        root,
        workspace_generation,
        repository_generation,
    }
}

fn text(buffer: &Buffer, area: Rect) -> String {
    (area.y..area.bottom())
        .map(|y| {
            (area.x..area.right())
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn discovery_is_reported_before_scanning_files_and_only_for_a_repository() {
    let dir = tempfile::tempdir().unwrap();
    drop(git2::Repository::init(dir.path()).unwrap());
    let mut reported = false;
    let snapshot = collect_source_control_status_with_progress(dir.path().into(), || {
        reported = true;
        // This file must be in the result: discovery precedes the expensive scan.
        std::fs::write(dir.path().join("new.txt"), "new\n").unwrap();
    });
    assert!(reported && snapshot.repository_found);
    assert_eq!(snapshot.files.len(), 1);
    assert_eq!(snapshot.files[0].path, "new.txt");

    let empty = tempfile::tempdir().unwrap();
    let snapshot = collect_source_control_status_with_progress(empty.path().into(), || {
        panic!("a folder without Git must not enter the loading phase");
    });
    assert!(!snapshot.repository_found);
}

#[test]
fn disabling_after_discovery_skips_worktree_status_scan() {
    let dir = tempfile::tempdir().unwrap();
    drop(git2::Repository::init(dir.path()).unwrap());
    std::fs::write(dir.path().join("new.txt"), "new\n").unwrap();
    let cancel = crate::background::CancellationToken::new();
    let snapshot = super::collect_source_control_status_cancellable(dir.path().into(), || {
        cancel.cancel();
    }, &cancel);
    assert!(snapshot.repository_found);
    assert!(snapshot.files.is_empty());
    let cancelled = super::collect_source_control_status_cancellable(dir.path().into(), || {
        panic!("a cancelled scan must not discover a repository");
    }, &cancel);
    assert!(!cancelled.repository_found);
}

#[test]
fn git_worker_sends_discovery_before_status_and_keeps_refreshes_quiet() {
    let dir = tempfile::tempdir().unwrap();
    drop(git2::Repository::init(dir.path()).unwrap());
    std::fs::write(dir.path().join("new.txt"), "new\n").unwrap();
    let mut app = App::new(dir.path().into());
    app.refresh_source_control_async();
    let found = app
        .background
        .receiver
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert!(
        matches!(read_result(&found), BackgroundResult::GitRepositoryFound { root, workspace_generation, .. }
        if *root == app.explorer.root && *workspace_generation == app.workspace_generation)
    );
    let status = app
        .background
        .receiver
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    assert!(
        matches!(read_result(&status), BackgroundResult::GitRepositories { scans, .. }
        if scans.len() == 1 && scans[0].files.len() == 1)
    );
    app.background.sender.send(found).unwrap();
    app.background.sender.send(status).unwrap();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_git_status_running);
    assert_eq!(app.source_control_repository_state, State::Available);

    app.refresh_source_control_async();
    assert_eq!(app.source_control_repository_state, State::Available);
    assert_eq!(
        app.source_control_files.len(),
        1,
        "retain the last known count during a refresh"
    );
    // Selecting the discovered repository can also queue branch/history reads.
    // The refresh itself must not announce discovery again.
    let mut other_reads = Vec::new();
    let status = loop {
        let result = app.background.receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(!matches!(read_result(&result), BackgroundResult::GitRepositoryFound { .. }), "no redundant discovery event");
        if matches!(read_result(&result), BackgroundResult::GitRepositories { .. }) { break result; }
        other_reads.push(result);
    };
    for result in other_reads { app.background.sender.send(result).unwrap(); }
    app.background.sender.send(status).unwrap();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_git_status_running);
}

fn assert_missing_repository_ui(app: &mut App) {
    assert_eq!(app.source_control_repository_state, State::Missing);
    let mut terminal = Terminal::new(TestBackend::new(100, 18)).unwrap();
    let bar = Rect::new(0, 0, 6, 18);
    let panel = Rect::new(6, 0, 94, 18);
    terminal.draw(|frame| {
        crate::ui_chrome::draw_activity_bar(frame, app, bar);
        crate::ui_source_control::draw_source_control_sidebar(frame, app, panel);
    }).unwrap();
    let buffer = terminal.backend().buffer();
    let source_control = text(buffer, panel);
    assert!(source_control.contains("No Git repository"));
    assert!(source_control.contains("Initialize Repository"));
    assert!(!source_control.contains("Detecting") && !source_control.contains("Loading"));
    assert!(!text(buffer, bar).contains('?'));
    assert!(app.source_control_initialize_area.is_some());
    assert!(app.source_control_actions_area.is_some());
}

#[test]
fn missing_repository_stays_visible_during_refreshes_and_queued_rechecks() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new_deferred(dir.path().into());
    app.refresh_source_control_async();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_git_status_running);
    assert_missing_repository_ui(&mut app);

    // A fresh scan must not replace a known negative result with the startup
    // detection screen, even while another refresh is waiting behind it.
    for _ in 0..3 {
        app.refresh_source_control_async();
        assert!(app.bg_git_status_running);
        assert_missing_repository_ui(&mut app);
        app.refresh_source_control_async();
        assert!(app.bg_git_status_pending);
        assert_missing_repository_ui(&mut app);
        app.wait_for_background(Duration::from_secs(5), |app| {
            assert_eq!(app.source_control_repository_state, State::Missing);
            !app.bg_git_status_running
        });
        assert_missing_repository_ui(&mut app);
    }
}

#[test]
fn navigation_and_file_watcher_refreshes_do_not_flash_repository_detection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file.txt");
    std::fs::write(&path, "text\n").unwrap();
    let mut app = App::new_deferred(dir.path().into());
    app.refresh_source_control_async();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_git_status_running);
    app.source_control_last_refresh = Some(std::time::Instant::now() - Duration::from_secs(3));
    app.refresh_source_control_for_navigation();
    assert_missing_repository_ui(&mut app);
    for _ in 0..3 {
        app.background.sender.send(BackgroundResult::FilesChanged {
            root: app.explorer.root.clone(),
            changes: vec![crate::background::WorkspacePathChange {
                observed_at: std::time::Instant::now(), path: path.clone(),
                exists: true, modified: None, len: Some(5),
            }],
            structure_changed: false, rescan: false, git_metadata_changed: false,
        }).unwrap();
        app.poll_background_results();
        assert_missing_repository_ui(&mut app);
    }
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_git_status_running);
    assert_missing_repository_ui(&mut app);
}

#[test]
fn a_new_repository_transitions_from_missing_to_loading_after_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = App::new_deferred(dir.path().into());
    app.refresh_source_control_async();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_git_status_running);
    assert_missing_repository_ui(&mut app);
    drop(git2::Repository::init(dir.path()).unwrap());
    std::fs::write(dir.path().join("new.txt"), "new\n").unwrap();
    app.refresh_source_control_async();
    assert_missing_repository_ui(&mut app);
    let found = app.background.receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    let status = app.background.receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(read_result(&found), BackgroundResult::GitRepositoryFound { .. }));
    assert!(matches!(read_result(&status), BackgroundResult::GitRepositories { scans, .. } if scans.len() == 1));
    // Hold the scan result back to observe the loading phase deterministically.
    app.background.sender.send(found).unwrap();
    app.poll_background_results();
    assert_eq!(app.source_control_repository_state, State::Loading);
    app.background.sender.send(status).unwrap();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_git_status_running);
    assert_eq!(app.source_control_repository_state, State::Available);
    assert_eq!(app.source_control_files.len(), 1);
}

#[test]
fn removing_a_repository_does_not_leave_later_refreshes_in_detection_mode() {
    let dir = tempfile::tempdir().unwrap();
    drop(git2::Repository::init(dir.path()).unwrap());
    let mut app = App::new_deferred(dir.path().into());
    app.refresh_source_control_async();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_git_status_running);
    assert_eq!(app.source_control_repository_state, State::Available);
    // Status publication can precede the last branch/ignore read releasing a
    // Windows directory handle. Retry only this external fixture mutation;
    // the subsequent editor-state assertions must still succeed immediately.
    let rename_started = std::time::Instant::now();
    loop {
        match std::fs::rename(dir.path().join(".git"), dir.path().join("former-git")) {
            Ok(()) => break,
            Err(error) if cfg!(windows) && matches!(error.raw_os_error(), Some(5 | 32 | 33))
                && rename_started.elapsed() < Duration::from_secs(5) => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("Cannot remove the fixture's Git directory: {error}"),
        }
    }
    app.refresh_source_control_async();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_git_status_running);
    assert_missing_repository_ui(&mut app);
    app.refresh_source_control_async();
    assert_missing_repository_ui(&mut app);
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_git_status_running);
    assert_missing_repository_ui(&mut app);
}

#[test]
fn progress_changes_the_loading_phase_without_publishing_a_zero_count() {
    let dir = tempfile::tempdir().unwrap();
    drop(git2::Repository::init(dir.path()).unwrap());
    std::fs::write(dir.path().join("new.txt"), "new\n").unwrap();
    let mut app = App::new_deferred(dir.path().into());
    app.bg_git_status_running = true;
    let found = |root, workspace_generation| BackgroundResult::GitRepositoryFound {
        root,
        workspace_generation,
        repository_generation: 0,
    };
    for stale in [
        found(app.explorer.root.join("other"), app.workspace_generation),
        found(
            app.explorer.root.clone(),
            app.workspace_generation.wrapping_add(1),
        ),
    ] {
        app.background.sender.send(stale).unwrap();
        assert!(
            !app.poll_background_results(),
            "stale progress must not redraw"
        );
        assert_eq!(app.source_control_repository_state, State::Checking);
    }
    app.background
        .sender
        .send(found(app.explorer.root.clone(), app.workspace_generation))
        .unwrap();
    assert!(app.poll_background_results());
    assert_eq!(app.source_control_repository_state, State::Loading);
    assert!(app.bg_git_status_running);
    assert!(app.source_control_last_refresh.is_none());

    app.background
        .sender
        .send(status(
            app.explorer.root.clone(),
            app.workspace_generation,
            app.repositories.membership_generation,
        ))
        .unwrap();
    assert!(app.poll_background_results());
    assert_eq!(app.source_control_repository_state, State::Available);
    assert!(!app.bg_git_status_running);
    assert_eq!(app.source_control_files.len(), 1);
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.bg_git_log_running && !app.bg_git_branch_running
    });
    app.background
        .sender
        .send(found(app.explorer.root.clone(), app.workspace_generation))
        .unwrap();
    assert!(!app.poll_background_results());
    assert_eq!(app.source_control_repository_state, State::Available);

    // Returning to the same folder must not accept its previous visit's result.
    let previous_generation = app.workspace_generation;
    app.workspace_generation = app.workspace_generation.wrapping_add(1);
    app.source_control_repository_state = State::Checking;
    app.source_control_files.clear();
    app.source_control_last_refresh = None;
    app.background
        .sender
        .send(status(
            app.explorer.root.clone(),
            previous_generation,
            app.repositories.membership_generation,
        ))
        .unwrap();
    assert!(!app.poll_background_results());
    assert_eq!(app.source_control_repository_state, State::Checking);
    assert!(app.source_control_files.is_empty());
    assert!(app.source_control_last_refresh.is_none());
}

#[test]
fn loading_labels_and_unknown_badges_distinguish_detection_scan_and_completion() {
    let mut app = App::test_new();
    app.show_explorer = true;
    app.activity_bar_selected = 2;
    let mut terminal = Terminal::new(TestBackend::new(100, 18)).unwrap();
    let bar = Rect::new(0, 0, 6, 18);
    let panel = Rect::new(6, 0, 94, 18);
    let source_control = crate::ui_chrome::activity_item_regions(bar)
        .into_iter()
        .find(|(_, index)| *index == 2)
        .unwrap()
        .0;
    for (state, message, badge) in [
        (State::Checking, "Detecting Git repository…", "?"),
        (State::Loading, "Loading Git changes…", "?"),
        (State::Available, "Actions", "1"),
        (State::Missing, "No Git repository", ""),
    ] {
        app.source_control_repository_state = state;
        app.source_control_files = if state == State::Available {
            vec![change()]
        } else {
            Vec::new()
        };
        terminal
            .draw(|frame| {
                crate::ui_chrome::draw_activity_bar(frame, &app, bar);
                crate::ui_source_control::draw_source_control_sidebar(frame, &mut app, panel);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rendered = text(buffer, panel);
        assert!(rendered.contains(message), "{state:?}: {rendered}");
        assert!(!rendered.contains("Checking for a Git repository"));
        let badge_text = text(
            buffer,
            Rect::new(source_control.x + 1, source_control.bottom() - 1, 4, 1),
        );
        assert_eq!(badge_text.trim(), badge, "{state:?}");
        if matches!(state, State::Checking | State::Loading) {
            assert!(app.source_control_initialize_area.is_none());
            assert!(app.source_control_commit_area.is_none());
            assert!(app.source_control_actions_area.is_some());
            assert!(!rendered.contains("No changes"));
            if state == State::Loading {
                assert!(rendered.contains("Scanning tracked and untracked files"));
            }
        }
    }
    app.source_control_repository_state = State::Available;
    app.source_control_files.clear();
    terminal
        .draw(|frame| crate::ui_chrome::draw_activity_bar(frame, &app, bar))
        .unwrap();
    assert!(!text(terminal.backend().buffer(), source_control).contains('?'));
}

#[test]
fn activity_badge_respects_untracked_filter_without_hiding_collapsed_changes() {
    use crate::source_control_rows::Group;
    let bar = Rect::new(0, 0, 6, 18);
    let region = crate::ui_chrome::activity_item_regions(bar)
        .into_iter()
        .find(|(_, index)| *index == 2)
        .unwrap()
        .0;
    let badge = Rect::new(region.x + 1, region.bottom() - 1, 4, 1);
    let mut terminal = Terminal::new(TestBackend::new(6, 18)).unwrap();
    for counts in [[1, 1, 1, 1], [1, 1, 1, 150], [0, 0, 0, 3], [1, 0, 0, 0], [0, 0, 0, 0]] {
        for collapsed in [false, true] {
            let mut app = App::test_new();
            app.source_control_repository_state = State::Available;
            app.source_control_files = Group::ALL
                .into_iter()
                .flat_map(|group| {
                    (0..counts[group as usize]).map(move |index| GitFileChange {
                        path: format!("{group:?}-{index}.txt"),
                        staged: group == Group::Staged,
                        status: if group == Group::Merge { '!' } else if group == Group::Untracked { '?' } else { 'M' },
                        original_path: None,
                    })
                })
                .collect();
            for group in Group::ALL {
                app.set_source_control_group_collapsed(group, collapsed);
            }
            for expected_count in [
                counts.iter().sum::<usize>(),
                counts[0] + counts[1] + counts[2],
                counts.iter().sum(),
            ] {
                terminal
                    .draw(|frame| crate::ui_chrome::draw_activity_bar(frame, &app, bar))
                    .unwrap();
                let expected = match expected_count {
                    0 => String::new(),
                    1..=99 => expected_count.to_string(),
                    _ => "99+".into(),
                };
                assert_eq!(
                    text(terminal.backend().buffer(), badge).trim(),
                    expected,
                    "counts={counts:?}, collapsed={collapsed}, visibility={:?}",
                    app.sc_visibility
                );
                app.open_source_control_filter_menu(0, 0);
                crate::input_pickers::handle_context_menu_key(&mut app, KeyCode::Char(' '));
            }
            assert!(!app.sc_visibility.untracked);
            for state in [State::Checking, State::Loading] {
                app.source_control_repository_state = state;
                terminal
                    .draw(|frame| crate::ui_chrome::draw_activity_bar(frame, &app, bar))
                    .unwrap();
                assert_eq!(text(terminal.backend().buffer(), badge).trim(), "?");
            }
            app.source_control_repository_state = State::Available;
            let mut staged_new_file = change();
            staged_new_file.staged = true;
            staged_new_file.status = 'A';
            app.update_source_control_files(vec![staged_new_file, change()]);
            terminal
                .draw(|frame| crate::ui_chrome::draw_activity_bar(frame, &app, bar))
                .unwrap();
            assert_eq!(text(terminal.backend().buffer(), badge).trim(), "1");
        }
    }
}

#[test]
fn compact_activity_bars_keep_the_unknown_count_beside_the_label() {
    let mut app = App::test_new();
    app.source_control_repository_state = State::Loading;
    for height in [6, 12, 18] {
        let mut terminal = Terminal::new(TestBackend::new(6, height)).unwrap();
        let bar = Rect::new(0, 0, 6, height);
        terminal
            .draw(|frame| crate::ui_chrome::draw_activity_bar(frame, &app, bar))
            .unwrap();
        let region = crate::ui_chrome::activity_item_regions(bar)
            .into_iter()
            .find(|(_, index)| *index == 2)
            .unwrap()
            .0;
        let rendered = text(terminal.backend().buffer(), region);
        assert!(
            rendered.contains('?') && rendered.contains("SRC"),
            "{height}: {rendered}"
        );
        if region.height >= 2 {
            assert!(rendered.contains("CTRL"));
        }
    }
}

#[test]
fn loading_panels_fit_small_sizes_and_do_not_activate_hidden_controls() {
    let mut app = App::test_new();
    for state in [State::Checking, State::Loading] {
        app.source_control_repository_state = state;
        for key in [
            KeyCode::Enter,
            KeyCode::Char(' '),
            KeyCode::Char('m'),
            KeyCode::Tab,
            KeyCode::Delete,
        ] {
            crate::ui_source_control::handle_key(&mut app, key, KeyModifiers::NONE);
        }
        assert!(
            !app.source_control_input_mode
                && !app.bg_git_action_running
                && !app.sc_discard_confirm_active
        );
        for (width, height) in [(1, 1), (12, 5), (24, 8), (40, 12)] {
            for right in [false, true] {
                app.sidebar_right = right;
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| {
                        crate::ui_source_control::draw_source_control_sidebar(
                            frame,
                            &mut app,
                            Rect::new(0, 0, width, height),
                        )
                    })
                    .unwrap();
                assert!(app.source_control_initialize_area.is_none());
            }
        }
    }
}
