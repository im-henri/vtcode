use super::*;
use crate::app::{DiffMode, SplitDirection};
use std::path::{Path, PathBuf};

fn repository() -> (tempfile::TempDir, PathBuf, App) {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let path = root.join("example.txt");
    {
        let repo = git2::Repository::init(&root).unwrap();
        repo.config()
            .unwrap()
            .set_bool("core.autocrlf", false)
            .unwrap();
        let mut index = repo.index().unwrap();
        std::fs::write(&path, "original content\nunchanged\n").unwrap();
        index.add_path(Path::new("example.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "Initial", &tree, &[])
            .unwrap();
        std::fs::write(&path, "staged content\nunchanged\n").unwrap();
        index.add_path(Path::new("example.txt")).unwrap();
        index.write().unwrap();
        std::fs::write(&path, "worktree content\nunchanged\n").unwrap();
    }
    let mut app = fixture();
    app.explorer = crate::explorer::FileExplorer::new_deferred(root);
    app.tabs.clear();
    app.show_explorer = false;
    (directory, path, app)
}

fn text(buffer: &Buffer, area: Rect) -> String {
    (area.y..area.bottom())
        .flat_map(|y| (area.x..area.right()).map(move |x| buffer[(x, y)].symbol()))
        .collect()
}

fn assert_loading(app: &mut App, terminal: &mut Terminal<TestBackend>, cosmetic: bool) {
    let frame = render(app, terminal, cosmetic);
    let area = app.editor_area.unwrap();
    let content = text(&frame, area);
    assert!(content.contains("Loading diff…"), "{content}");
    assert!(!content.contains("staged content") && !content.contains("worktree content"));
    assert!(app.loading.active());
    assert_eq!(app.code_area, None);
    assert_eq!(app.gutter_area, None);
    assert_eq!(app.scrollbar_area, None);
    assert!(
        (area.y..area.bottom())
            .any(|y| { !underlines(&frame, Rect::new(area.x, y, area.width, 1)).is_empty() })
    );
}

#[test]
fn source_control_loading_covers_disk_reads_and_comparison_until_colored_diff_is_ready() {
    for staged in [false, true] {
        for mode in [DiffMode::Inline, DiffMode::SideBySide] {
            let (_directory, path, mut app) = repository();
            app.preferred_diff_mode = mode;
            app.open_source_control_diff_in_repository(
                &path,
                false,
                staged,
                Some(app.explorer.root.clone()),
            );
            let mut terminal = Terminal::new(TestBackend::new(120, 26)).unwrap();
            assert_loading(&mut app, &mut terminal, false);
            assert_eq!(
                editor_label(&app, app.editor().unwrap()),
                Some("Loading diff…")
            );
            if !staged {
                // Hold comparison dispatch to inspect the frame after disk I/O.
                app.bg_git_compare_running = true;
                app.wait_for_background(Duration::from_secs(5), |app| {
                    !app.editor().unwrap().bg_loading
                });
                assert!(app.pending_git_compare_path.is_some());
                assert_loading(&mut app, &mut terminal, false);
                app.bg_git_compare_running = false;
                app.start_pending_git_compare_for_path(&path);
            }
            app.show_tab_bar = false;
            app.show_breadcrumbs = false;
            app.show_status_bar = false;
            assert_loading(&mut app, &mut terminal, true);
            app.wait_for_background(Duration::from_secs(5), |app| {
                app.pending_git_compare_path.is_none() && !app.bg_git_compare_running
            });
            assert!(
                app.should_render_diff_for_active_editor(),
                "{}",
                app.status_message
            );
            assert!(app.diff_mode == mode);
            let frame = render(&mut app, &mut terminal, false);
            let displayed = text(&frame, app.editor_area.unwrap());
            assert!(!displayed.contains("Loading diff"));
            assert!(displayed.contains(if staged {
                "staged content"
            } else {
                "worktree content"
            }));
            assert!(
                frame
                    .content
                    .iter()
                    .any(|cell| cell.bg == app.colors.bg_diff_added)
            );
            assert!(
                frame
                    .content
                    .iter()
                    .any(|cell| cell.bg == app.colors.bg_diff_removed)
            );
            assert!(!app.loading.active());
            assert_eq!(editor_label(&app, app.editor().unwrap()), None);
        }
    }
}

#[test]
fn pending_diff_in_an_inactive_split_does_not_mask_the_active_file_or_lose_its_state() {
    let (_directory, path, mut app) = repository();
    app.new_file();
    for ch in "active document".chars() {
        app.editor_mut().unwrap().insert_char(ch);
    }
    app.open_source_control_diff_in_repository(&path, false, true, Some(app.explorer.root.clone()));
    app.create_split_group_dir(1, SplitDirection::Horizontal);
    let diff_group = app.active_group;
    app.switch_group(0);
    let mut terminal = Terminal::new(TestBackend::new(120, 26)).unwrap();
    for _ in 0..3 {
        let frame = render(&mut app, &mut terminal, false);
        let pending_area = app.groups[diff_group].editor_area.unwrap();
        assert!(text(&frame, pending_area).contains("Loading diff…"));
        assert!(text(&frame, app.editor_area.unwrap()).contains("active document"));
        assert_eq!(app.editor().unwrap().rope.to_string(), "active document");
        assert!(!app.git_compare_pending_for_active_editor());
        assert_eq!(editor_label(&app, app.editor().unwrap()), None);
        assert!(app.loading.active());
    }
    app.switch_group(diff_group);
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.should_render_diff_for_active_editor()
    });
    render(&mut app, &mut terminal, false);
    assert!(!app.loading.active());
}

#[test]
fn failed_comparison_stops_loading_and_stays_failed_when_rendered_in_an_inactive_split() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing.txt");
    let mut app = fixture();
    app.explorer = crate::explorer::FileExplorer::new_deferred(directory.path().into());
    app.tabs.clear();
    app.show_explorer = false;
    app.open_source_control_diff_in_repository(&path, false, true, Some(directory.path().into()));
    let mut terminal = Terminal::new(TestBackend::new(120, 26)).unwrap();
    assert_loading(&mut app, &mut terminal, false);
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.pending_git_compare_path.is_none()
    });
    assert!(!app.notification_history.is_empty());
    let frame = render(&mut app, &mut terminal, false);
    assert!(!text(&frame, app.editor_area.unwrap()).contains("Loading diff"));
    assert!(!app.loading.active());
    app.new_file();
    app.create_split_group_dir(0, SplitDirection::Horizontal);
    let diff_group = app.active_group;
    app.switch_group(0);
    for _ in 0..3 {
        let frame = render(&mut app, &mut terminal, false);
        assert!(
            !text(&frame, app.groups[diff_group].editor_area.unwrap()).contains("Loading diff")
        );
        assert!(
            !app.loading.active(),
            "painting a failed view must not restart loading"
        );
    }
}

#[test]
fn a_file_read_failure_replaces_loading_with_the_error() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("missing.txt");
    let mut app = fixture();
    app.explorer = crate::explorer::FileExplorer::new_deferred(directory.path().into());
    app.tabs.clear();
    app.show_explorer = false;
    app.open_source_control_diff_in_repository(&path, false, false, Some(directory.path().into()));
    let mut terminal = Terminal::new(TestBackend::new(100, 26)).unwrap();
    assert_loading(&mut app, &mut terminal, false);
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.editor().unwrap().bg_loading
    });
    let frame = render(&mut app, &mut terminal, false);
    let displayed = text(&frame, app.editor_area.unwrap());
    assert!(displayed.contains("Error loading file"));
    assert!(!displayed.contains("Loading diff"));
    assert!(app.pending_git_compare_path.is_none());
    assert!(!app.loading.active());
}

#[test]
fn large_file_mode_clears_the_pending_comparison_and_reports_why_it_paused() {
    let (_directory, path, mut app) = repository();
    std::fs::write(&path, "worktree content\n".repeat(70_000)).unwrap();
    app.large_file_threshold_mb = 1;
    app.open_source_control_diff_in_repository(
        &path,
        false,
        false,
        Some(app.explorer.root.clone()),
    );
    let mut terminal = Terminal::new(TestBackend::new(100, 26)).unwrap();
    assert_loading(&mut app, &mut terminal, false);
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.editor().unwrap().bg_loading
    });
    assert!(app.editor().unwrap().large_file_mode);
    assert!(app.pending_git_compare_path.is_none());
    assert!(!app.bg_git_compare_running);
    assert!(
        app.notification_history
            .iter()
            .any(|(message, _, _)| message.contains("Diff is paused"))
    );
    let frame = render(&mut app, &mut terminal, false);
    assert!(!text(&frame, app.editor_area.unwrap()).contains("Loading diff"));
    assert!(!app.loading.active());
}

#[test]
fn unavailable_files_in_an_inactive_split_do_not_keep_the_loading_indicator_running() {
    for large in [false, true] {
        let (_directory, path, mut app) = repository();
        if large {
            std::fs::write(&path, "worktree content\n".repeat(70_000)).unwrap();
            app.large_file_threshold_mb = 1;
        } else {
            std::fs::remove_file(&path).unwrap();
        }
        app.new_file();
        app.open_source_control_diff_in_repository(
            &path,
            false,
            false,
            Some(app.explorer.root.clone()),
        );
        app.create_split_group_dir(1, SplitDirection::Horizontal);
        let diff_group = app.active_group;
        app.switch_group(0);
        app.wait_for_background(Duration::from_secs(5), |app| {
            !app.groups[diff_group].tabs[0].editor.bg_loading
        });
        let mut terminal = Terminal::new(TestBackend::new(120, 26)).unwrap();
        for _ in 0..2 {
            let frame = render(&mut app, &mut terminal, false);
            let displayed = text(&frame, app.groups[diff_group].editor_area.unwrap());
            assert!(!displayed.contains("Loading diff"));
            if !large {
                assert!(displayed.contains("Error loading file"));
            }
            assert!(!app.loading.active());
        }
    }
}

#[test]
fn closing_a_loading_diff_clears_feedback_and_ignores_its_late_reply() {
    let (_directory, path, mut app) = repository();
    app.new_file();
    app.open_source_control_diff_in_repository(&path, false, true, Some(app.explorer.root.clone()));
    let mut terminal = Terminal::new(TestBackend::new(100, 26)).unwrap();
    assert_loading(&mut app, &mut terminal, false);
    app.force_close_tab(app.active_tab);
    app.sync_diff_tab();
    app.wait_for_background(Duration::from_secs(5), |app| {
        !app.background.has_active_jobs() && !app.background.has_pending_results()
    });
    let frame = render(&mut app, &mut terminal, false);
    assert!(!text(&frame, app.editor_area.unwrap()).contains("Loading diff"));
    assert!(!app.loading.active());
    assert!(app.pending_git_compare_path.is_none());
    assert!(!app.should_render_diff_for_active_editor());
}
