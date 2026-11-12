use crate::{
    app::{App, EditorGroup, Tab, TabKind},
    background::BackgroundResult,
    editor::{Editor, PreparedGitLineStatus},
    source_control_rows::Visibility,
};
use ratatui::{Terminal, backend::TestBackend, style::Color};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

fn repository(committed: bool) -> (tempfile::TempDir, git2::Repository) {
    let directory = tempfile::tempdir().unwrap();
    let repo = git2::Repository::init(directory.path()).unwrap();
    repo.config()
        .unwrap()
        .set_bool("core.autocrlf", false)
        .unwrap();
    if committed {
        commit_tracked_file(directory.path(), &repo, "original\n");
    }
    (directory, repo)
}

fn commit_tracked_file(root: &Path, repo: &git2::Repository, text: &str) {
    std::fs::write(root.join("tracked.txt"), text).unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("tracked.txt")).unwrap();
    index.write().unwrap();
    let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
    let signature = git2::Signature::now("Test", "test@example.com").unwrap();
    repo.commit(Some("HEAD"), &signature, &signature, "Initial", &tree, &[]).unwrap();
}

fn app(root: &Path, remote: bool) -> App {
    let mut app = App::new_deferred(root.into());
    if remote {
        let workspace = crate::remote::workspace::Workspace::new(crate::remote::tests::connection(
            root,
            Arc::default(),
            Arc::default(),
        ));
        workspace.client.set_wake(app.background.wake_handle());
        app.remote_workspace = Some(workspace);
    }
    app.sc_visibility.untracked = false;
    app
}

fn path(app: &App, root: &Path, name: &str) -> PathBuf {
    app.remote_workspace.as_ref().map_or_else(
        || root.join(name),
        |workspace| {
            workspace.view_key(&crate::remote::RemotePath::new(name.as_bytes().to_vec()).unwrap())
        },
    )
}

fn tab(path: PathBuf, text: &str) -> Tab {
    let mut editor = Editor::test_with_text(text);
    editor.file_path = Some(path);
    editor.saved_rope = Some(editor.rope.clone());
    Tab::new_file(editor)
}

fn wait(app: &mut App) {
    app.wait_for_background(Duration::from_secs(10), |app| {
        app.editor().is_some_and(|editor| !editor.bg_loading)
            && !app.bg_git_diff_running
            && !app.bg_git_diff_pending
    });
}

fn assert_overview(app: &mut App, changed: bool) {
    app.show_minimap = true;
    app.show_breadcrumbs = false;
    app.sticky_scroll = false;
    app.cursor_visible = false;
    app.colors.ui.fg_gutter_added = Color::Rgb(1, 2, 3);
    app.colors.ui.fg_overview_added = Color::Rgb(7, 8, 9);
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui_editor::draw_editor(frame, app, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let minimap = app.minimap_area.unwrap();
    assert_eq!(
        (minimap.y..minimap.bottom())
            .any(|y| buffer[(minimap.right() - 1, y)].bg == app.colors.ui.fg_gutter_added),
        changed,
        "minimap"
    );
    for (area, color) in [
        (app.gutter_area.unwrap(), app.colors.ui.fg_gutter_added),
        (
            app.scrollbar_area.unwrap(),
            app.colors.ui.fg_overview_added,
        ),
    ] {
        assert_eq!(
            (area.y..area.bottom())
                .any(|y| { (area.x..area.right()).any(|x| buffer[(x, y)].fg == color) }),
            changed,
            "area={area:?}"
        );
    }
}

#[test]
fn opening_hidden_untracked_files_skips_comparisons_and_markers_locally_and_remotely() {
    for committed in [false, true] {
        let (directory, _repo) = repository(committed);
        let contents: String = (0..120).map(|line| format!("new line {line}\n")).collect();
        std::fs::write(directory.path().join("new.txt"), &contents).unwrap();
        for remote in [false, true] {
            let mut app = app(directory.path(), remote);
            let file = path(&app, directory.path(), "new.txt");
            let log = app.git_output();
            app.open_file(&file);
            wait(&mut app);
            assert_eq!(app.editor().unwrap().file_path.as_ref(), Some(&file));
            assert_eq!(app.editor().unwrap().rope.to_string(), contents);
            assert!(app.editor().unwrap().git_line_status().is_empty());
            assert!(app.editor().unwrap().git_change_lines().is_empty());
            assert!(!log.report().contains("Read line diff"));
            assert_overview(&mut app, false);

            app.set_source_control_visibility(Visibility::default());
            wait(&mut app);
            assert_eq!(app.editor().unwrap().git_line_status()[0], 'U');
            assert!(app.editor().unwrap().git_line_status_untracked);
            assert_overview(&mut app, true);
            let revision = app.editor().unwrap().overview_change_revision;

            app.set_source_control_visibility(Visibility { untracked: false });
            assert!(app.editor().unwrap().git_line_status().is_empty());
            assert!(app.editor().unwrap().overview_change_revision > revision);
            wait(&mut app);
            assert!(app.editor().unwrap().git_line_status().is_empty());
            assert_overview(&mut app, false);
        }
    }
}

#[test]
fn hiding_untracked_files_preserves_tracked_and_staged_markers_locally_and_remotely() {
    let (directory, repo) = repository(true);
    std::fs::write(directory.path().join("tracked.txt"), "modified\n").unwrap();
    std::fs::write(directory.path().join("added.txt"), "staged\n").unwrap();
    let mut index = repo.index().unwrap();
    index.add_path(Path::new("added.txt")).unwrap();
    // A staged deletion with a recreated worktree file also has WT_NEW, but
    // its staged history must remain visible when untracked files are hidden.
    index.remove_path(Path::new("tracked.txt")).unwrap();
    index.write().unwrap();
    assert!(
        repo.status_file(Path::new("tracked.txt"))
            .unwrap()
            .is_wt_new()
    );
    for remote in [false, true] {
        let mut app = app(directory.path(), remote);
        for (name, contents, marker) in [
            ("added.txt", "staged\n", 'S'),
            ("tracked.txt", "modified\n", 'U'),
        ] {
            let file = path(&app, directory.path(), name);
            app.tabs = vec![tab(file, contents)];
            app.active_tab = 0;
            app.refresh_git_line_status_async();
            wait(&mut app);
            assert_eq!(app.editor().unwrap().git_line_status()[0], marker);
            assert!(!app.editor().unwrap().git_line_status_untracked);
        }
    }
}

#[test]
fn undo_after_save_clears_worktree_markers_when_the_buffer_matches_the_index() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    for (remote, staged) in [(false, false), (true, false), (false, true), (true, true)] {
        let (directory, repo) = repository(true);
        let disk_path = directory.path().join("tracked.txt");
        let original = if staged { "original\nstaged line\n" } else { "original\n" };
        if staged {
            std::fs::write(&disk_path, original).unwrap();
            let mut index = repo.index().unwrap();
            index.add_path(Path::new("tracked.txt")).unwrap();
            index.write().unwrap();
        }
        let original = if remote {
            original.to_owned()
        } else {
            repo.config().unwrap().set_bool("core.autocrlf", true).unwrap();
            let checkout = original.replace('\n', "\r\n");
            std::fs::write(&disk_path, &checkout).unwrap();
            checkout
        };
        let mut app = app(directory.path(), remote);
        let file = path(&app, directory.path(), "tracked.txt");
        app.open_file(&file);
        wait(&mut app);
        app.active_pane = crate::app::ActivePane::Editor;
        let editor = app.editor_mut().unwrap();
        editor.set_cursor(0, 0);
        editor.insert_char('x');
        editor.insert_newline();
        editor.insert_char('y');
        assert!(editor.undo_stack.len() > 1);

        app.save_current_file();
        app.wait_for_background(Duration::from_secs(10), |app| app.pending_file_saves.is_empty());
        wait(&mut app);
        let written = std::fs::read_to_string(&disk_path).unwrap();
        assert_ne!(written, original);
        assert!(!app.editor().unwrap().modified);
        assert!(app.editor().unwrap().git_change_lines().iter().any(|(_, changes)| changes.unstaged().is_some()));
        assert!(!crate::ui_editor::cached_overview_change_rows(&mut app, 30, None).is_empty());

        // Save can add its own undo entry when it normalizes line endings.
        let undo_steps = app.editor().unwrap().undo_stack.len();
        for _ in 0..undo_steps {
            crate::input_keyboard::handle_keyboard_event(
                &mut app, KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL),
            );
        }
        let editor = app.editor().unwrap();
        assert_eq!(editor.rope, original.as_str(), "remote={remote}, staged={staged}, undo steps={undo_steps}");
        assert!(editor.modified);
        assert_eq!(std::fs::read_to_string(&disk_path).unwrap(), written);
        assert!(!editor.has_unsaved_change_in_range(0, editor.line_count()));
        assert!(editor.git_change_lines().iter().all(|(_, changes)| changes.unstaged().is_none()));
        assert_eq!(editor.line_diff_status(1), if staged { 'S' } else { ' ' });
        assert_eq!(crate::ui_editor::cached_overview_change_rows(&mut app, 30, None).is_empty(), !staged);
        if !staged { assert_overview(&mut app, false); }
    }
}

#[test]
fn blank_line_marker_survives_save_and_undo_restores_a_clean_git_gutter() {
    use crate::line_changes::ChangeKind;

    for remote in [false, true] {
        let (directory, repo) = repository(false);
        let original = "one\n\n\nlast\n";
        let disk_path = directory.path().join("tracked.txt");
        commit_tracked_file(directory.path(), &repo, &format!("\u{feff}{original}"));
        let mut app = app(directory.path(), remote);
        let file = path(&app, directory.path(), "tracked.txt");
        app.open_file(&file);
        wait(&mut app);
        let editor = app.editor_mut().unwrap();
        assert_eq!(editor.rope, original);
        assert!(editor.git_change_lines().is_empty());
        editor.set_cursor(0, 3);
        editor.insert_newline();

        for saved in [false, true] {
            if saved {
                app.save_current_file();
                app.wait_for_background(Duration::from_secs(10), |app| app.pending_file_saves.is_empty());
                wait(&mut app);
            }
            let editor = app.editor().unwrap();
            assert_eq!(editor.modified, !saved);
            let changed: Vec<_> = (0..editor.line_count()).filter_map(|line| {
                editor.line_changes(line).working().map(|kind| (line, kind))
            }).collect();
            assert_eq!(changed, [(1, ChangeKind::Added)], "remote={remote}, saved={saved}");
            let status = editor.line_hunk_status(1);
            app.open_hunk_popup(0, 0, file.clone(), 1, status);
            app.wait_for_hunk_preview();
            assert!(app.hunk_peek.is_some(), "remote={remote}, saved={saved}");
            app.close_hunk_popup();
        }

        app.editor_mut().unwrap().undo();
        let editor = app.editor().unwrap();
        assert_eq!(editor.rope, original);
        assert!(editor.modified);
        assert!(editor.uses_git_index_markers());
        assert!((0..editor.line_count()).all(|line| editor.line_changes(line).is_empty()));
        assert_eq!(std::fs::read_to_string(disk_path).unwrap(), "\u{feff}one\n\n\n\nlast\n");
        assert_overview(&mut app, false);
    }
}

#[test]
fn hiding_untracked_markers_clears_all_panes_and_rejects_late_results_without_touching_unsaved_or_snapshot_changes()
 {
    let mut app = App::test_new();
    let file = PathBuf::from("untracked.txt");
    let mut untracked = tab(file.clone(), "one\ntwo");
    let mut status = PreparedGitLineStatus::from_diffs(2, &[], &['U', 'U']);
    status.untracked = true;
    untracked
        .editor
        .set_prepared_git_line_status(status.clone());
    untracked.editor.insert_char('x');
    let mut duplicate = tab(file.clone(), "one\ntwo");
    duplicate
        .editor
        .set_prepared_git_line_status(untracked.editor.prepared_git_line_status());
    let mut tracked = tab("tracked.txt".into(), "changed");
    tracked.editor.set_git_line_status(vec!['U']);
    let mut snapshot = tab(file.clone(), "one\ntwo");
    snapshot.kind = TabKind::Diff(1);
    snapshot.editor.set_git_line_status(vec!['U', 'U']);
    // Keep the active untitled document idle so the test controls late delivery.
    app.tabs = vec![Tab::new_file(Editor::new()), untracked, tracked, snapshot];
    app.groups.push(EditorGroup {
        tabs: vec![duplicate],
        ..Default::default()
    });
    app.set_source_control_visibility(Visibility { untracked: false });
    assert!(app.tabs[1].editor.git_line_status().is_empty());
    assert!(
        app.groups.last().unwrap().tabs[0]
            .editor
            .git_line_status()
            .is_empty()
    );
    assert_eq!(app.tabs[1].editor.unsaved_line_status(0), 'N');
    assert_eq!(app.tabs[2].editor.git_line_status(), ['U']);
    assert_eq!(app.tabs[3].editor.git_line_status(), ['U', 'U']);

    app.bg_git_diff_running = true;
    app.background
        .sender
        .send(BackgroundResult::GitLineDiff {
            file_path: file,
            status,
        })
        .unwrap();
    app.poll_background_results();
    assert!(!app.bg_git_diff_running && !app.bg_git_diff_pending);
    assert!(app.tabs[1].editor.git_line_status().is_empty());
    assert!(
        app.groups.last().unwrap().tabs[0]
            .editor
            .git_line_status()
            .is_empty()
    );
    assert_eq!(app.tabs[1].editor.unsaved_line_status(0), 'N');
    assert_eq!(app.tabs[3].editor.git_line_status(), ['U', 'U']);
}

#[test]
fn a_suppressed_result_rechecks_if_untracked_files_were_reenabled_before_delivery() {
    let (directory, _repo) = repository(false);
    std::fs::write(directory.path().join("new.txt"), "new\n").unwrap();
    let mut app = app(directory.path(), false);
    let file = directory.path().join("new.txt");
    app.tabs = vec![tab(file.clone(), "new\n")];
    app.active_tab = 0;
    app.sc_visibility = Visibility::default();
    let mut status = PreparedGitLineStatus::from_diffs(0, &[], &[]);
    status.untracked = true;
    app.bg_git_diff_running = true;
    app.background
        .sender
        .send(BackgroundResult::GitLineDiff {
            file_path: file,
            status,
        })
        .unwrap();
    app.poll_background_results();
    wait(&mut app);
    assert_eq!(app.editor().unwrap().git_line_status()[0], 'U');
    assert!(app.editor().unwrap().git_line_status_untracked);
}
