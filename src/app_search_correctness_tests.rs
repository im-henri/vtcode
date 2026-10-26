use super::*;
use crate::{
    app::{Tab, TabKind},
    editor::Editor,
};

fn app(text: &str) -> App {
    let mut app = App::test_new();
    let mut editor = Editor::test_with_text(text);
    editor.record_saved_snapshot(editor.rope.clone());
    app.tabs.push(Tab::new_file(editor));
    app
}

fn assert_valid_positions(editor: &Editor) {
    for cursor in editor.collect_multi_cursors() {
        assert!(
            cursor.line < editor.line_count(),
            "cursor row: {}",
            cursor.line
        );
        assert!(cursor.col <= editor.line_len(cursor.line));
        if let Some((line, col)) = cursor.anchor {
            assert!(line < editor.line_count(), "anchor row: {line}");
            assert!(col <= editor.line_len(line));
        }
    }
}

#[test]
fn find_centers_live_matches_and_next_previous_even_when_already_visible() {
    let text: String = (0..100)
        .map(|line| if [20, 24, 80].contains(&line) { "needle\n" } else { "text\n" })
        .collect();
    for height in [1, 2, 20, 31] {
        let mut app = app(&text);
        let editor = app.editor_mut().unwrap();
        editor.viewport_height = height;
        editor.scroll_offset = 18;
        app.execute_palette_command("find");
        app.search_query = "needle".into();
        app.update_search();
        for expected in [20, 24, 80, 20] {
            let editor = app.editor().unwrap();
            assert_eq!(editor.selected_text().as_deref(), Some("needle"));
            assert_eq!(editor.cursor_line, expected);
            assert_eq!(editor.scroll_offset, expected - height / 2);
            assert_eq!(editor.wrap_scroll_offset, editor.scroll_offset);
            app.search_next();
        }
        app.search_prev();
        app.search_prev();
        assert_eq!(app.editor().unwrap().cursor_line, 80);
        assert_eq!(app.editor().unwrap().scroll_offset, 80 - height / 2);
        let before = app.editor().unwrap().scroll_offset;
        app.search_query = "no such text".into();
        app.update_search();
        app.search_next();
        app.search_prev();
        assert_eq!(app.editor().unwrap().scroll_offset, before);
    }
}

#[test]
fn find_centers_using_visible_rows_and_unfolds_the_match() {
    let text: String = (0..150)
        .map(|line| if line == 95 { "needle\n" } else { "text\n" })
        .collect();
    let mut app = app(&text);
    let editor = app.editor_mut().unwrap();
    editor.viewport_height = 20;
    editor.folded_ranges.extend([(15, 90), (92, 98)]);
    app.search_query = "needle".into();
    app.execute_palette_command("find");
    let editor = app.editor().unwrap();
    assert_eq!(editor.visible_line_at(editor.scroll_offset, 10), Some(95));
    assert_eq!(editor.folded_ranges.len(), 1);
    assert_eq!(editor.folded_ranges.get(&15), Some(&90));
}

#[test]
fn find_centers_wrapped_matches_and_keeps_source_scroll_in_sync() {
    let mut app = app(&format!("{}needle{}", "\t界abcd ".repeat(30), " text".repeat(30)));
    let editor = app.editor_mut().unwrap();
    editor.word_wrap = true;
    editor.viewport_width = 16;
    editor.viewport_height = 9;
    app.search_query = "needle".into();
    app.execute_palette_command("find");
    let editor = app.editor_mut().unwrap();
    let width = editor.effective_wrap_width();
    let row = editor.wrapped_row_for_position(editor.cursor_line, editor.cursor_col, width);
    assert!(row > editor.viewport_height);
    assert_eq!(row - editor.wrap_scroll_offset, editor.viewport_height / 2);
    let top = editor.wrapped_row_at(editor.wrap_scroll_offset, width).unwrap();
    assert_eq!(editor.scroll_offset, top.line_idx);
    assert_eq!(editor.selected_text().as_deref(), Some("needle"));
}

#[test]
fn find_clamps_centering_at_the_start_of_a_file() {
    let mut app = app("needle\ntext\nneedle");
    app.editor_mut().unwrap().viewport_height = 20;
    app.search_query = "needle".into();
    app.execute_palette_command("find");
    assert_eq!(app.editor().unwrap().scroll_offset, 0);
    app.search_next();
    assert_eq!(app.editor().unwrap().cursor_line, 2);
    assert_eq!(app.editor().unwrap().scroll_offset, 0);
}

#[test]
fn find_centers_in_the_active_split_height_without_scrolling_other_panes() {
    use ratatui::{Terminal, backend::TestBackend};
    let text: String = (0..150).map(|line| format!("line_{line}\n")).collect();
    let mut app = app(&text);
    app.tabs.push(Tab::new_file(Editor::test_with_text(&text)));
    app.create_split_group_dir(1, crate::app::SplitDirection::Vertical);
    app.show_explorer = false;
    app.show_minimap = false;
    app.sticky_scroll = false;
    let mut terminal = Terminal::new(TestBackend::new(120, 48)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    let other_scroll = app.groups[0].tabs[0].editor.scroll_offset;
    app.search_query = "line_100".into();
    app.execute_palette_command("find");
    terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
    let code = app.code_area.unwrap();
    assert!(code.height < 24);
    let editor = app.editor().unwrap();
    assert_eq!(editor.cursor_line - editor.scroll_offset, code.height as usize / 2);
    let row: String = (code.x..code.right())
        .map(|x| terminal.backend().buffer()[(x, code.y + code.height / 2)].symbol())
        .collect();
    assert!(row.contains("line_100"), "{row:?}");
    assert_eq!(app.groups[0].tabs[0].editor.scroll_offset, other_scroll);
}

#[test]
fn replace_all_multiline_preserves_valid_carets_and_undo_selections() {
    for ending in ["\n", "\r\n", "\r"] {
        let original = format!("remove{ending}é界{ending}remove{ending}tail");
        let mut app = app(&original);
        let editor = app.editor_mut().unwrap();
        editor.cursor_line = 1;
        editor.cursor_col = 2;
        editor.selection_anchor = Some((3, 4));
        editor.extra_cursors = vec![(3, 4)];
        editor.extra_cursor_anchors = vec![Some((2, 0))];
        app.search_query = "remove\n".into();
        app.update_search();
        app.replace_all();
        let editor = app.editor_mut().unwrap();
        assert_eq!(editor.rope.to_string(), format!("é界{ending}tail"));
        assert_valid_positions(editor);
        assert_eq!((editor.cursor_line, editor.cursor_col), (0, 2));
        assert_eq!(editor.extra_cursors, [(1, 4)]);
        editor.multi_cursor_insert_char('!');
        assert_eq!(editor.rope.to_string(), format!("é界!{ending}tail!"));
        editor.undo();
        editor.undo();
        assert_eq!(editor.rope.to_string(), original);
        assert_eq!(editor.selection_anchor, Some((3, 4)));
        assert_eq!(editor.extra_cursor_anchors, [Some((2, 0))]);
        assert!(!editor.modified);
        editor.redo();
        assert_valid_positions(editor);
    }
}

#[test]
fn unchanged_replacements_preserve_redo_and_saved_state() {
    for all in [false, true] {
        let mut app = app("foo foo");
        let editor = app.editor_mut().unwrap();
        editor.insert_char('!');
        editor.undo();
        let version = editor.content_version;
        app.search_query = "foo".into();
        app.replace_query = "foo".into();
        app.update_search();
        if all {
            app.replace_all();
        } else {
            app.replace_current();
        }
        let editor = app.editor_mut().unwrap();
        assert_eq!(editor.rope.to_string(), "foo foo");
        assert!(!editor.modified);
        assert_eq!(editor.content_version, version);
        assert!(editor.can_redo());
        assert_eq!(app.search_matches.len(), 2);
        if !all {
            assert_eq!(
                app.search_current, 1,
                "an unchanged replacement still advances Find"
            );
        }
    }
}

fn result(editor: &Editor, text: &str) -> crate::background::OpenFileReplaceResult {
    crate::background::OpenFileReplaceResult {
        path: editor.file_path.clone().unwrap(),
        source_document_id: editor.save_identity,
        source_content_version: editor.content_version,
        prepared: crate::formatters::prepare(&editor.rope, text),
        replacements: 1,
    }
}

#[test]
fn workspace_replace_does_not_modify_read_only_or_revision_tabs() {
    for revision in [false, true] {
        let mut app = app("foo");
        app.tabs[0].editor.file_path = Some(PathBuf::from("file.txt"));
        let result = result(&app.tabs[0].editor, "bar");
        if revision {
            app.tabs[0].kind = TabKind::GitRevision;
        } else {
            app.tabs[0].editor.read_only = true;
        }
        assert!(!apply_open_replace_result(&mut app.tabs, &result));
        assert_eq!(app.tabs[0].editor.rope.to_string(), "foo");
        assert!(!app.tabs[0].editor.can_undo());
    }
}

#[test]
fn workspace_replace_shared_views_create_only_one_undo_step() {
    let mut app = app("foo");
    app.tabs[0].editor.file_path = Some(PathBuf::from("file.txt"));
    let mut shared = Editor::new();
    shared.copy_document_from(&app.tabs[0].editor);
    app.tabs.push(Tab::new_file(shared));
    let result = result(&app.tabs[0].editor, "bar");
    assert!(apply_open_replace_result(&mut app.tabs, &result));
    app.tabs[1].editor.refresh_document();
    assert_eq!(app.tabs[1].editor.rope.to_string(), "bar");
    app.tabs[1].editor.undo();
    assert_eq!(app.tabs[1].editor.rope.to_string(), "foo");
    assert!(!app.tabs[1].editor.can_undo());
    app.tabs[0].editor.refresh_document();
    assert_eq!(app.tabs[0].editor.rope.to_string(), "foo");
}

#[test]
fn workspace_replace_rejects_an_edit_from_another_shared_view() {
    let mut app = app("foo");
    app.tabs[0].editor.file_path = Some(PathBuf::from("file.txt"));
    let result = result(&app.tabs[0].editor, "bar");
    let mut shared = Editor::new();
    shared.copy_document_from(&app.tabs[0].editor);
    shared.insert_char('!');
    assert!(!apply_open_replace_result(&mut app.tabs, &result));
    assert_eq!(app.tabs[0].editor.rope.to_string(), "!foo");
    shared.refresh_document();
    assert_eq!(shared.rope.to_string(), "!foo");
}

#[test]
fn asynchronous_replace_rechecks_writability() {
    let mut app = app("foo");
    let target = app.editor_content_target().unwrap();
    let source_document_id = app.editor().unwrap().save_identity;
    let prepared = crate::formatters::prepare(&app.editor().unwrap().rope, "bar");
    app.editor_mut().unwrap().read_only = true;
    app.bg_file_replace_running = true;
    app.background
        .sender
        .send(crate::background::BackgroundResult::FileReplaceDone {
            generation: app.file_replace_generation,
            target,
            source_document_id,
            prepared,
            replacements: 1,
        })
        .unwrap();
    app.poll_background_results();
    assert_eq!(app.editor().unwrap().rope.to_string(), "foo");
    assert!(!app.editor().unwrap().can_undo());
}

#[test]
fn workspace_replace_does_not_target_a_reopened_file_at_the_same_version() {
    let mut app = app("foo");
    app.tabs[0].editor.file_path = Some(PathBuf::from("file.txt"));
    let result = result(&app.tabs[0].editor, "bar");
    let mut reopened = Editor::test_with_text("foo");
    reopened.file_path = Some(result.path.clone());
    assert_eq!(reopened.content_version, result.source_content_version);
    app.tabs[0] = Tab::new_file(reopened);
    assert!(!apply_open_replace_result(&mut app.tabs, &result));
    assert_eq!(app.tabs[0].editor.rope.to_string(), "foo");
}

#[test]
fn find_actions_refresh_stale_shared_offsets_without_overwriting_new_edits() {
    for action in [
        App::replace_current,
        App::replace_all,
        App::search_next,
        App::jump_to_current_match,
    ] {
        let mut app = app("prefix\nfoo\nend");
        app.search_query = "foo".into();
        app.replace_query = "bar".into();
        app.update_search();
        let mut shared = Editor::new();
        shared.copy_document_from(app.editor().unwrap());
        shared.select_all();
        shared.paste_plain_with_text("new foo");
        action(&mut app);
        assert_eq!(app.editor().unwrap().rope.to_string(), "new foo");
        assert_eq!(app.search_matches, [(0, 4, 7)]);
        assert_valid_positions(app.editor().unwrap());
        shared.refresh_document();
        assert_eq!(shared.rope.to_string(), "new foo");
        // The refreshed match can be replaced on the next action.
        app.replace_current();
        assert_eq!(app.editor().unwrap().rope.to_string(), "new bar");
    }
}

#[test]
fn saving_without_text_changes_keeps_find_navigation_immediately_available() {
    let mut app = app("foo foo");
    app.editor_mut().unwrap().insert_char('!');
    app.editor_mut().unwrap().set_cursor(0, 0);
    app.search_query = "foo".into();
    app.update_search();
    let editor = app.editor_mut().unwrap();
    editor.record_saved_snapshot(editor.rope.clone());
    app.search_next();
    assert_eq!(app.editor().unwrap().selection_range(), Some((0, 1, 0, 4)));
}

#[test]
fn asynchronous_replace_keeps_navigation_done_while_the_worker_was_running() {
    let mut app = app("foo\né界\nfoo\ntail");
    let target = app.editor_content_target().unwrap();
    let source_document_id = app.editor().unwrap().save_identity;
    let prepared = crate::formatters::prepare(&app.editor().unwrap().rope, "é界\ntail");
    app.editor_mut().unwrap().set_cursor(3, 4);
    app.bg_file_replace_running = true;
    app.background
        .sender
        .send(crate::background::BackgroundResult::FileReplaceDone {
            generation: app.file_replace_generation,
            target,
            source_document_id,
            prepared,
            replacements: 2,
        })
        .unwrap();
    app.poll_background_results();
    let editor = app.editor_mut().unwrap();
    assert_eq!(editor.rope.to_string(), "é界\ntail");
    assert_eq!((editor.cursor_line, editor.cursor_col), (1, 4));
    editor.undo();
    assert_eq!((editor.cursor_line, editor.cursor_col), (3, 4));
}

#[test]
fn asynchronous_replace_rejects_a_different_untitled_document() {
    let mut app = app("foo");
    let target = app.editor_content_target().unwrap();
    let source_document_id = app.editor().unwrap().save_identity;
    let prepared = crate::formatters::prepare(&app.editor().unwrap().rope, "bar");
    let mut other = Editor::test_with_text("xyz");
    other.record_saved_snapshot(other.rope.clone());
    app.tabs[0] = Tab::new_file(other);
    assert_eq!(app.editor_content_target().unwrap(), target);
    app.background
        .sender
        .send(crate::background::BackgroundResult::FileReplaceDone {
            generation: app.file_replace_generation,
            target,
            source_document_id,
            prepared,
            replacements: 1,
        })
        .unwrap();
    app.poll_background_results();
    assert_eq!(app.editor().unwrap().rope.to_string(), "xyz");
}

#[test]
fn workspace_search_reads_the_current_shared_buffer_before_replacing() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("file.txt");
    std::fs::write(&path, "foo").unwrap();
    let mut app = App::new(workspace.path().to_path_buf());
    let mut editor = Editor::test_with_text("foo");
    editor.file_path = Some(path.clone());
    editor.record_saved_snapshot(editor.rope.clone());
    let mut shared = Editor::new();
    shared.copy_document_from(&editor);
    app.tabs.push(Tab::new_file(editor));
    shared.insert_char('!');
    // No frame or pane switch has refreshed the app's view yet.
    app.global_search_query = "!foo".into();
    app.update_global_search();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_global_search_running
    });
    assert_eq!(app.global_search_results.len(), 1);
    app.global_search_replace_query = "é界".into();
    app.replace_all_in_files();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_global_replace_running && !app.bg_global_search_running
    });
    shared.refresh_document();
    assert_eq!(shared.rope.to_string(), "é界");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "foo");
    shared.undo();
    assert_eq!(shared.rope.to_string(), "!foo");
    shared.undo();
    assert_eq!(shared.rope.to_string(), "foo");
}

#[test]
fn workspace_replace_uses_the_file_buffer_even_with_a_revision_tab_open_first() {
    let workspace = tempfile::tempdir().unwrap();
    let path = workspace.path().join("file.txt");
    std::fs::write(&path, "foo").unwrap();
    let mut app = App::new(workspace.path().to_path_buf());
    let mut revision = Tab::new_file(Editor::test_with_text("foo"));
    revision.editor.file_path = Some(path.clone());
    revision.editor.read_only = true;
    revision.kind = TabKind::GitRevision;
    app.tabs.push(revision);
    let mut editor = Editor::test_with_text("foo");
    editor.file_path = Some(path.clone());
    app.tabs.push(Tab::new_file(editor));
    app.active_tab = 1;
    app.global_search_query = "foo".into();
    app.update_global_search();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_global_search_running
    });
    app.global_search_replace_query = "bar".into();
    app.replace_all_in_files();
    app.wait_for_background(std::time::Duration::from_secs(5), |app| {
        !app.bg_global_replace_running && !app.bg_global_search_running
    });
    assert_eq!(app.tabs[0].editor.rope.to_string(), "foo");
    assert_eq!(app.tabs[1].editor.rope.to_string(), "bar");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "foo");
}

#[test]
fn replacement_offsets_keep_retained_unicode_text_and_large_multicursor_edits_aligned() {
    for ending in ["\n", "\r\n"] {
        for replacement in ["", "é界", "é\n界"] {
            let original = format!("foo keep😀{ending}").repeat(256);
            let mut app = app(&original);
            let editor = app.editor_mut().unwrap();
            editor.set_cursor(0, 9);
            editor.extra_cursors = (1..256).map(|line| (line, 9)).collect();
            app.search_query = "foo".into();
            app.replace_query = replacement.into();
            app.update_search();
            app.replace_all();
            let editor = app.editor_mut().unwrap();
            let replacement = replacement.replace('\n', ending);
            let after = format!("{replacement} keep😀{ending}").repeat(256);
            assert_eq!(editor.rope.to_string(), after);
            assert_valid_positions(editor);
            editor.multi_cursor_insert_char('!');
            assert_eq!(
                editor.rope.to_string(),
                format!("{replacement} keep😀!{ending}").repeat(256)
            );
            editor.undo();
            assert_eq!(editor.rope.to_string(), after);
            editor.undo();
            assert_eq!(editor.rope.to_string(), original);
            assert!(!editor.modified);
            editor.redo();
            assert_eq!(editor.rope.to_string(), after);
        }
    }
}
