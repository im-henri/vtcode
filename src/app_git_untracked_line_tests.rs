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
        std::fs::write(directory.path().join("tracked.txt"), "original\n").unwrap();
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("tracked.txt")).unwrap();
        index.write().unwrap();
        let tree = repo.find_tree(index.write_tree().unwrap()).unwrap();
        let signature = git2::Signature::now("Test", "test@example.com").unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, "Initial", &tree, &[])
            .unwrap();
    }
    (directory, repo)
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
    app.colors.ui.fg_gutter_modified = Color::Rgb(1, 2, 3);
    app.colors.ui.fg_minimap_modified = Color::Rgb(4, 5, 6);
    app.colors.ui.fg_overview_modified = Color::Rgb(7, 8, 9);
    let mut terminal = Terminal::new(TestBackend::new(140, 30)).unwrap();
    terminal
        .draw(|frame| crate::ui_editor::draw_editor(frame, app, frame.area()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    for (area, color) in [
        (app.gutter_area.unwrap(), app.colors.ui.fg_gutter_modified),
        (app.minimap_area.unwrap(), app.colors.ui.fg_minimap_modified),
        (
            app.scrollbar_area.unwrap(),
            app.colors.ui.fg_overview_modified,
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
