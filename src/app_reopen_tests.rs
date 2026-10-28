use crate::{app::App, remote};
use ratatui::{Terminal, backend::TestBackend};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

fn app(root: &Path, use_remote: bool) -> (App, Option<remote::workspace::Workspace>, PathBuf) {
    let remote = use_remote.then(|| {
        remote::workspace::Workspace::new(remote::tests::connection(
            root,
            Arc::default(),
            Arc::default(),
        ))
    });
    let mut app = App::new_deferred(
        remote
            .as_ref()
            .map_or_else(|| root.to_owned(), |remote| remote.view_root.clone()),
    );
    app.source_control_enabled = false;
    app.show_explorer = false;
    let path = remote.as_ref().map_or_else(
        || root.join("file.txt"),
        |remote| remote.target_key("file.txt").unwrap(),
    );
    app.remote_workspace = remote.clone();
    (app, remote, path)
}

fn wait(app: &mut App) {
    app.wait_for_background(Duration::from_secs(5), |app| {
        app.editor().is_some_and(|editor| !editor.bg_loading)
    });
    let editor = app.editor().unwrap();
    assert!(!editor.bg_loading && !editor.load_failed);
}

fn position(app: &App) -> (usize, usize, usize, usize, usize) {
    let editor = app.editor().unwrap();
    (
        editor.scroll_offset,
        editor.horizontal_scroll,
        editor.wrap_scroll_offset,
        editor.cursor_line,
        editor.cursor_col,
    )
}

fn content() -> String {
    (0..300)
        .map(|i| format!("line {i}: {}\n", "a long word wrapped line ".repeat(9)))
        .collect()
}

#[test]
fn reopening_files_restores_the_exact_viewport_locally_and_remotely() {
    for use_remote in [false, true] {
        for wrapped in [false, true] {
            for recent_command in [false, true] {
                let root = tempfile::tempdir().unwrap();
                std::fs::write(root.path().join("file.txt"), content()).unwrap();
                let (mut app, remote, path) = app(root.path(), use_remote);
                app.default_word_wrap = wrapped;
                app.open_file(&path);
                wait(&mut app);
                let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
                terminal
                    .draw(|frame| crate::ui::draw(frame, &mut app))
                    .unwrap();
                let editor = app.editor_mut().unwrap();
                // Scrolling independently of the cursor must be preserved too.
                editor.cursor_line = 4;
                editor.cursor_col = 17;
                if wrapped {
                    let width = editor.effective_wrap_width();
                    let row = editor.wrapped_row_for_position(125, width + 20, width);
                    editor.set_wrap_scroll_offset(row);
                } else {
                    editor.scroll_offset = 125;
                    editor.wrap_scroll_offset = 125;
                    editor.horizontal_scroll = 9;
                }
                let expected = position(&app);
                app.force_close_tab(0);
                if recent_command {
                    app.reopen_closed_tab();
                } else {
                    app.open_file(&path);
                }
                assert!(app.editor().unwrap().bg_loading);
                wait(&mut app);
                terminal
                    .draw(|frame| crate::ui::draw(frame, &mut app))
                    .unwrap();
                assert_eq!(
                    position(&app),
                    expected,
                    "remote={use_remote}, wrapped={wrapped}, recent={recent_command}"
                );
                if let Some(remote) = remote {
                    remote.client.call(remote::Command::Shutdown).unwrap();
                }
            }
        }
    }
}

#[test]
fn closing_a_reopened_placeholder_keeps_the_saved_position() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("file.txt"), content()).unwrap();
    let (mut app, _, path) = app(root.path(), false);
    app.open_file(&path);
    wait(&mut app);
    app.editor_mut().unwrap().scroll_offset = 125;
    app.force_close_tab(0);
    app.open_file(&path);
    assert!(app.editor().unwrap().bg_loading);
    app.force_close_tab(0);
    app.reopen_closed_tab();
    wait(&mut app);
    assert_eq!(app.editor().unwrap().scroll_offset, 125);
}

#[test]
fn explicit_navigation_wins_over_a_remembered_closed_view() {
    for use_remote in [false, true] {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file.txt"), content()).unwrap();
        let (mut app, remote, path) = app(root.path(), use_remote);
        app.open_file(&path);
        wait(&mut app);
        app.editor_mut().unwrap().scroll_offset = 125;
        app.force_close_tab(0);
        app.open_file(&path);
        app.editor_mut().unwrap().set_cursor(18, 3);
        wait(&mut app);
        assert_eq!(
            (
                app.editor().unwrap().cursor_line,
                app.editor().unwrap().cursor_col
            ),
            (18, 3)
        );
        assert_ne!(app.editor().unwrap().scroll_offset, 125);
        if let Some(remote) = remote {
            remote.client.call(remote::Command::Shutdown).unwrap();
        }
    }
}

#[test]
fn a_shortened_file_clamps_its_saved_view_and_reopening_a_live_tab_keeps_its_position() {
    let root = tempfile::tempdir().unwrap();
    let native = root.path().join("file.txt");
    std::fs::write(&native, content()).unwrap();
    let (mut app, _, path) = app(root.path(), false);
    app.open_file(&path);
    wait(&mut app);
    app.editor_mut().unwrap().scroll_offset = 125;
    app.force_close_tab(0);
    app.open_file(&path);
    wait(&mut app);
    app.editor_mut().unwrap().scroll_offset = 200;
    app.reopen_closed_tab();
    assert_eq!(app.editor().unwrap().scroll_offset, 200);
    app.force_close_tab(0);
    std::fs::write(&native, "short\n").unwrap();
    app.open_file(&path);
    wait(&mut app);
    assert!(app.editor().unwrap().scroll_offset < app.editor().unwrap().line_count());
}
