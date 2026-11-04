use super::*;
use crate::{
    app::{ActivePane, Tab},
    background::BackgroundResult,
    editor::Editor,
    search_results::SearchRow,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn fixture(text: &str) -> (tempfile::TempDir, App, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("a.txt");
    std::fs::write(&path, text).unwrap();
    std::fs::write(root.path().join("b.txt"), "other needle\n").unwrap();
    let mut app = App::new_deferred(root.path().into());
    app.source_control_enabled = false;
    let mut editor = Editor::test_with_text(text);
    editor.file_path = Some(path.clone());
    editor.detect_line_ending();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_pane = ActivePane::Editor;
    app.global_search_query = "needle".into();
    (root, app, path)
}

fn search(app: &mut App) {
    app.update_global_search();
    app.wait_for_background(Duration::from_secs(5), |app| !app.bg_global_search_running);
    assert!(app.global_search_error.is_none(), "{:?}", app.global_search_error);
}

fn finish_edits(app: &mut App) {
    assert!(app.poll_pending_global_search(Instant::now() + Duration::from_secs(1)));
    app.wait_for_background(Duration::from_secs(5), |app| app.global_search_model_updates.pending.is_empty());
    assert!(app.global_search_error.is_none(), "{:?}", app.global_search_error);
}

fn key(app: &mut App, code: KeyCode) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, KeyModifiers::NONE));
}

#[test]
fn workspace_search_edits_remove_deleted_words_and_lines_before_save_and_follow_undo_redo() {
    let source = "界 needle needle\r\nkeep\r\nneedle tail";
    let (root, mut app, path) = fixture(source);
    search(&mut app);
    assert_eq!(app.global_search_results.len(), 4);
    let other = app.global_search_results.iter().find(|found| found.file_path != path).unwrap().line_text.clone();
    let generation = app.search_generation;
    let editor = app.editor_mut().unwrap();
    editor.selection_anchor = Some((0, 2));
    editor.cursor_col = 8;
    key(&mut app, KeyCode::Backspace);
    assert_eq!(app.global_search_model_updates.pending.len(), 1);
    finish_edits(&mut app);
    assert_eq!(app.search_generation, generation, "edits must not restart the workspace scan");
    assert_eq!(app.global_search_results.len(), 3);
    let found: Vec<_> = app.global_search_results.iter().filter(|found| found.file_path == path).collect();
    assert_eq!((found[0].line_num, found[0].col_start, found[0].col_end), (1, 3, 9));
    assert_eq!(&*found[0].line_text, "界  needle");
    assert_eq!(found[1].line_num, 3);
    let unchanged = app.global_search_results.iter().find(|found| found.file_path != path).unwrap();
    assert!(std::sync::Arc::ptr_eq(&other, &unchanged.line_text));
    assert_eq!(app.global_search_results.file_count(), 2);

    app.execute_palette_command("undo");
    finish_edits(&mut app);
    assert_eq!(app.global_search_results.len(), 4);
    app.execute_palette_command("redo");
    finish_edits(&mut app);
    assert_eq!(app.global_search_results.len(), 3);
    app.execute_palette_command("delete_line");
    finish_edits(&mut app);
    assert_eq!(app.global_search_results.len(), 2);
    let found = app.global_search_results.iter().find(|found| found.file_path == path).unwrap();
    assert_eq!((found.line_num, found.col_start, found.col_end), (2, 0, 6));
    assert_eq!(&*found.line_text, "needle tail");
    assert_eq!(std::fs::read_to_string(root.path().join("a.txt")).unwrap(), source);
}

#[test]
fn workspace_search_edits_update_surviving_match_positions_and_previews() {
    let (_root, mut app, path) = fixture("needle\nneedle");
    search(&mut app);
    key(&mut app, KeyCode::Enter);
    finish_edits(&mut app);
    let rows: Vec<_> = app.global_search_results.iter().filter(|found| found.file_path == path)
        .map(|found| found.line_num).collect();
    assert_eq!(rows, [2, 3]);
    key(&mut app, KeyCode::Char('界'));
    finish_edits(&mut app);
    let found = app.global_search_results.iter().find(|found| found.file_path == path).unwrap();
    assert_eq!((found.line_num, found.col_start, found.col_end), (2, 1, 7));
    assert_eq!(&*found.line_text, "界needle");
    assert_eq!(app.global_search_results.len(), 3);
}

#[test]
fn workspace_search_edits_debounce_buffer_changes_independently_of_query_typing() {
    let (_root, mut app, path) = fixture("needle");
    search(&mut app);
    app.search_debounce_ms = 60_000;
    key(&mut app, KeyCode::Char('x'));
    let first = app.global_search_model_updates.pending_since.unwrap();
    assert_eq!(app.global_search_debounce_wait(first), Some(MODEL_SEARCH_QUIET_PERIOD));
    assert!(!app.poll_pending_global_search(first + MODEL_SEARCH_QUIET_PERIOD - Duration::from_millis(1)));
    key(&mut app, KeyCode::Char('y'));
    let latest = app.global_search_model_updates.pending_since.unwrap();
    assert!(app.poll_pending_global_search(latest + MODEL_SEARCH_QUIET_PERIOD));
    app.wait_for_background(Duration::from_secs(5), |app| app.global_search_model_updates.pending.is_empty());
    let found = app.global_search_results.iter().find(|found| found.file_path == path).unwrap();
    assert_eq!(found.col_start, 2);
    assert_eq!(&*found.line_text, "xyneedle");
    assert!(app.global_search_debounce_wait(Instant::now()).is_none());
    assert!(!app.bg_global_search_running);
}

#[test]
fn workspace_search_edits_refresh_small_and_background_pastes() {
    for bytes in [16, crate::app_paste::BACKGROUND_BYTES + 1] {
        let (_root, mut app, path) = fixture("needle");
        search(&mut app);
        app.editor_mut().unwrap().select_all();
        app.paste_editor_text("x".repeat(bytes), false);
        app.wait_for_background(Duration::from_secs(5), |app| !app.paste_in_progress());
        finish_edits(&mut app);
        assert_eq!(app.global_search_results.file_matches(&path), 0);
        assert_eq!(app.global_search_results.len(), 1);
        app.execute_palette_command("undo");
        finish_edits(&mut app);
        assert_eq!(app.global_search_results.file_matches(&path), 1);
    }
}

#[test]
fn workspace_search_edits_recheck_case_word_boundaries_and_regex_contents() {
    for (query, replacement, case_sensitive, whole_word, regex, expected) in [
        ("needle", "Needle", true, false, false, 0),
        ("needle", "Needle", false, false, false, 1),
        ("needle", "needlex", true, false, false, 1),
        ("needle", "needlex", true, true, false, 0),
        (r"\bneedle\b", "needlex", true, false, true, 0),
        (r"needle\r?\nkeep", "needle\r\nkept", true, false, true, 0),
    ] {
        let (_root, mut app, path) = fixture("needle\r\nkeep");
        app.global_search_query = query.into();
        app.global_search_case_sensitive = case_sensitive;
        app.global_search_whole_word = whole_word;
        app.global_search_regex = regex;
        search(&mut app);
        assert_eq!(app.global_search_results.file_matches(&path), 1);
        app.editor_mut().unwrap().select_all();
        app.paste_editor_text(replacement.into(), false);
        finish_edits(&mut app);
        assert_eq!(app.global_search_results.file_matches(&path), expected, "{query:?} -> {replacement:?}");
    }
}

#[test]
fn workspace_search_edits_keep_other_files_within_the_global_result_limit() {
    let (_root, mut app, path) = fixture("needle");
    app.max_global_search_results = 2;
    search(&mut app);
    assert_eq!(app.global_search_results.len(), 2);
    app.editor_mut().unwrap().select_all();
    app.paste_editor_text("needle needle needle".into(), false);
    finish_edits(&mut app);
    assert_eq!(app.global_search_results.len(), 2);
    assert_eq!(app.global_search_results.file_matches(&path), 1);
    assert_eq!(app.global_search_results.file_count(), 2);
    assert!(app.global_search_truncated);
}

#[test]
fn workspace_search_edits_respect_query_options_and_restore_a_files_last_match_on_undo() {
    for (query, case_sensitive, whole_word, regex) in [
        ("needle", true, false, false),
        ("NEEDLE", false, false, false),
        ("needle", true, true, false),
        (r"\bneedl[eE]\b", true, false, true),
        (r"needle\r?\nkeep", true, false, true),
    ] {
        let source = "界 needle\r\nkeep tail";
        let (_root, mut app, path) = fixture(source);
        app.global_search_query = query.into();
        app.global_search_case_sensitive = case_sensitive;
        app.global_search_whole_word = whole_word;
        app.global_search_regex = regex;
        search(&mut app);
        assert_eq!(app.global_search_results.file_matches(&path), 1, "{query}");
        app.editor_mut().unwrap().select_all();
        key(&mut app, KeyCode::Char('x'));
        finish_edits(&mut app);
        assert_eq!(app.global_search_results.file_matches(&path), 0, "{query}");
        assert!(app.global_search_results.tracks_file(&path));
        app.execute_palette_command("undo");
        finish_edits(&mut app);
        assert_eq!(app.global_search_results.file_matches(&path), 1, "{query}");
        app.execute_palette_command("redo");
        finish_edits(&mut app);
        assert_eq!(app.global_search_results.file_matches(&path), 0, "{query}");
    }
}

#[test]
fn workspace_search_edits_reject_old_streams_buffer_versions_and_cleared_queries() {
    let (_root, mut app, path) = fixture("needle");
    search(&mut app);
    let old = app.global_search_results.iter().find(|found| found.file_path == path).unwrap().clone();
    let generation = app.search_generation;
    app.editor_mut().unwrap().select_all();
    key(&mut app, KeyCode::Char('x'));
    let serial = app.global_search_model_updates.serial;
    app.background.sender.send(BackgroundResult::GlobalSearchResults {
        results: vec![old.clone(); app.max_global_search_results + 1],
        done: false, generation, truncated: false,
    }).unwrap();
    app.poll_background_results();
    assert!(!app.global_search_truncated, "discarded matches must not consume the result budget");
    assert_eq!(app.global_search_results.len(), 2);
    key(&mut app, KeyCode::Char('y'));
    app.background.sender.send(BackgroundResult::GlobalSearchFileResults {
        path: path.clone(), serial, generation, result: Ok((vec![old.clone()], false)),
    }).unwrap();
    app.poll_background_results();
    assert_eq!(app.global_search_model_updates.pending.len(), 1);
    finish_edits(&mut app);
    assert_eq!(app.global_search_results.len(), 1);
    assert_eq!(app.global_search_results.file_matches(&path), 0);
    app.queue_global_search_model_update(path.clone(), ropey::Rope::from_str("needle"));
    let serial = app.global_search_model_updates.serial;
    app.clear_global_search();
    app.background.sender.send(BackgroundResult::GlobalSearchFileResults {
        path, serial, generation, result: Ok((vec![old], false)),
    }).unwrap();
    app.poll_background_results();
    assert!(app.global_search_results.is_empty());
    assert!(app.global_search_model_updates.pending.is_empty());
    assert!(app.global_search_debounce_wait(Instant::now()).is_none());
}

#[test]
fn workspace_search_edits_preserve_dismissals_collapsed_files_and_selection() {
    let (root, mut app, path) = fixture("needle needle");
    search(&mut app);
    app.dismiss_global_search_result(SearchRow::Result(0));
    let other = root.path().join("b.txt");
    app.global_search_results.set_collapsed(&other, true);
    app.global_search_selected = 1;
    app.global_search_scroll = app.global_search_results.header_row(1).unwrap();
    let editor = app.editor_mut().unwrap();
    editor.selection_anchor = Some((0, 7));
    editor.cursor_col = 13;
    key(&mut app, KeyCode::Backspace);
    finish_edits(&mut app);
    assert_eq!(app.global_search_results.file_matches(&path), 0, "the explicitly dismissed match stays hidden");
    assert_eq!(app.global_search_results.len(), 1);
    assert_eq!(app.global_search_results.file_count(), 1);
    assert!(app.global_search_results.is_collapsed(&other));
    assert_eq!(app.global_search_results.row_count(), 1);
    assert_eq!(app.global_search_selected, 0);
    assert_eq!(app.global_search_scroll, 0);
    assert_eq!(app.global_search_results[0].file_path, other);
}

#[test]
fn workspace_search_edits_coalesce_changes_to_several_open_files() {
    let (root, mut app, path) = fixture("needle");
    search(&mut app);
    app.editor_mut().unwrap().select_all();
    key(&mut app, KeyCode::Char('x'));
    let mut other = Editor::test_with_text("other needle\n");
    let other_path = root.path().join("b.txt");
    other.file_path = Some(other_path.clone());
    app.tabs.push(Tab::new_file(other));
    app.active_tab = 1;
    key(&mut app, KeyCode::Char('界'));
    assert_eq!(app.global_search_model_updates.pending.len(), 2);
    finish_edits(&mut app);
    assert_eq!(app.global_search_results.file_matches(&path), 0);
    assert_eq!(app.global_search_results.file_matches(&other_path), 1);
    assert_eq!(&*app.global_search_results[0].line_text, "界other needle");
    assert_eq!(app.global_search_results[0].col_start, 7);
}

#[test]
fn workspace_search_edits_refresh_remote_buffers_without_another_workspace_rpc() {
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("a.txt"), "needle needle").unwrap();
    let traffic = Arc::new(AtomicUsize::new(0));
    let workspace = crate::remote::workspace::Workspace::new(crate::remote::tests::connection(
        root.path(), Arc::default(), traffic.clone(),
    ));
    let path = workspace.view_key(&crate::remote::RemotePath::new(b"a.txt".to_vec()).unwrap());
    let mut app = App::new_deferred(workspace.view_root.clone());
    app.source_control_enabled = false;
    app.remote_workspace = Some(workspace.clone());
    let mut editor = Editor::test_with_text("needle needle");
    editor.file_path = Some(path.clone());
    app.tabs = vec![Tab::new_file(editor)];
    app.active_pane = ActivePane::Editor;
    app.global_search_query = "needle".into();
    search(&mut app);
    assert_eq!(app.global_search_results.len(), 2);
    let before = traffic.load(Ordering::Relaxed);
    let editor = app.editor_mut().unwrap();
    editor.selection_anchor = Some((0, 0));
    editor.cursor_col = 6;
    key(&mut app, KeyCode::Backspace);
    finish_edits(&mut app);
    assert_eq!(app.global_search_results.len(), 1);
    assert_eq!(app.global_search_results[0].file_path, path);
    assert_eq!(app.global_search_results[0].col_start, 1);
    assert_eq!(traffic.load(Ordering::Relaxed), before);
    workspace.client.disconnect();
}
