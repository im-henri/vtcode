use super::{Editor, UndoSnapshot};
use crate::app::{ActivePane, App, Tab};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ropey::Rope;

fn saved_editor(text: &str) -> Editor {
    let mut editor = Editor::test_with_text(text);
    editor.file_path = Some("rebuild_syntax_cache.rs".into());
    editor.saved_rope = Some(editor.rope.clone());
    editor
}

fn app(editor: Editor) -> App {
    let mut app = App::test_new();
    app.tabs = vec![Tab::new_file(editor)];
    app.active_tab = 0;
    app.active_pane = ActivePane::Editor;
    app.quick_suggestions = false;
    app
}

fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    crate::input_keyboard::handle_keyboard_event(app, KeyEvent::new(code, modifiers));
}

fn snapshot(rope: Rope, modified: bool) -> UndoSnapshot {
    (rope, 0, 0, vec![], vec![], None, modified)
}

fn select_print_arm(editor: &mut Editor) {
    let text = editor.rope.to_string();
    let start = editor.rope.byte_to_char(text.find("Ok((count").unwrap());
    let end = editor
        .rope
        .byte_to_char(text.find("folder.display()").unwrap() + "folder.display()".len());
    let position = |index| {
        let line = editor.rope.char_to_line(index);
        (line, index - editor.rope.line_to_char(line))
    };
    editor.selection_anchor = Some(position(start));
    (editor.cursor_line, editor.cursor_col) = position(end);
}

#[test]
fn selected_code_deletion_and_replacement_restore_all_rows_in_one_undo() {
    let source = include_str!("../examples/rebuild_syntax_cache.rs").replace("\r\n", "\n");
    for crlf in [false, true] {
        let source = if crlf {
            source.replace('\n', "\r\n")
        } else {
            source.clone()
        };
        for backwards in [false, true] {
            for code in [
                KeyCode::Backspace,
                KeyCode::Delete,
                KeyCode::Char(' '),
                KeyCode::Enter,
            ] {
                let mut editor = saved_editor(&source);
                select_print_arm(&mut editor);
                if backwards {
                    let anchor = editor.selection_anchor.unwrap();
                    editor.selection_anchor = Some((editor.cursor_line, editor.cursor_col));
                    (editor.cursor_line, editor.cursor_col) = anchor;
                }
                let mut app = app(editor);
                key(&mut app, code, KeyModifiers::NONE);
                let deleted = app.editor().unwrap().rope.clone();
                assert_ne!(deleted, source);
                assert!(app.editor().unwrap().modified);
                key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
                let editor = app.editor().unwrap();
                assert_eq!(
                    editor.rope, source,
                    "key={code:?} backwards={backwards} crlf={crlf}"
                );
                assert!(!editor.modified);
                assert!(
                    !editor.can_undo(),
                    "one replacement must not leave an intermediate deletion"
                );
                key(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL);
                assert_eq!(app.editor().unwrap().rope, deleted);
                assert!(app.editor().unwrap().modified);
            }
        }
    }
}

#[test]
fn undo_of_a_later_edit_keeps_deleted_rows_marked_unsaved() {
    let source = include_str!("../examples/rebuild_syntax_cache.rs");
    let mut editor = saved_editor(source);
    select_print_arm(&mut editor);
    let mut app = app(editor);
    key(&mut app, KeyCode::Delete, KeyModifiers::NONE);
    let deleted = app.editor().unwrap().rope.clone();
    key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(app.editor().unwrap().rope, deleted);
    assert!(app.tabs[0].has_unsaved_changes());
    assert_ne!(app.editor().unwrap().unsaved_line_status(13), ' ');
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(app.editor().unwrap().rope, source);
    assert!(!app.tabs[0].has_unsaved_changes());
}

#[test]
fn restored_clean_history_flag_cannot_hide_missing_rows_or_allow_silent_close() {
    let source = include_str!("../examples/rebuild_syntax_cache.rs");
    let mut editor = saved_editor(source);
    let saved = editor.rope.clone();
    select_print_arm(&mut editor);
    editor.discard_selection();
    let deleted = editor.rope.clone();
    let line = editor.cursor_line;
    editor.insert_char(' ');
    let final_text = editor.rope.clone();
    // Recovered history can contain an incorrect clean flag. The text and
    // current disk baseline must prevail when navigating that history.
    editor.import_undo_redo(
        vec![snapshot(saved, false), snapshot(deleted.clone(), false)],
        vec![],
    );
    let mut app = app(editor);
    for _ in 0..2 {
        key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(app.editor().unwrap().rope, deleted);
        assert!(
            app.tabs[0].has_unsaved_changes(),
            "missing rows must still be unsaved"
        );
        assert_ne!(app.editor().unwrap().unsaved_line_status(0), ' ');

        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 1)).unwrap();
        terminal
            .draw(|frame| crate::ui_chrome_tabs::draw_tab_bar(frame, &app, frame.area()))
            .unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol() == "●")
        );
        app.close_tab(0);
        assert!(
            app.confirm_close_active,
            "closing must warn about the deleted rows"
        );
        assert_eq!(app.tabs.len(), 1);
        app.confirm_close_active = false;

        key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(app.editor().unwrap().rope, source);
        assert!(!app.tabs[0].has_unsaved_changes());
        key(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL);
        assert_eq!(app.editor().unwrap().rope, deleted);
        assert!(app.tabs[0].has_unsaved_changes());
        key(&mut app, KeyCode::Char('y'), KeyModifiers::CONTROL);
        assert_eq!(app.editor().unwrap().rope, final_text);
        assert!(app.tabs[0].has_unsaved_changes());
        assert!(line < app.editor().unwrap().line_count());
    }
}

#[test]
fn restored_history_compares_against_the_current_disk_baseline() {
    // Same byte/line lengths deliberately exercise content, not just sizes.
    let mut editor = saved_editor("let value = 11;\n");
    editor.paste_plain_with_text("// unsaved\n");
    editor.import_undo_redo(vec![snapshot("let value = 10;\n".into(), false)], vec![]);
    editor.undo();
    assert_eq!(editor.rope, "let value = 10;\n");
    assert!(
        editor.modified,
        "a flag from an earlier disk version cannot mark this version saved"
    );
    editor.redo();
    assert!(editor.modified);
    editor.undo();
    assert!(editor.modified);
}

#[test]
fn a_clean_redo_entry_must_also_match_the_saved_document() {
    let mut editor = saved_editor("before\n");
    editor.import_undo_redo(vec![], vec![snapshot("after\n".into(), false)]);
    editor.redo();
    assert_eq!(editor.rope, "after\n");
    assert!(editor.modified);
    editor.undo();
    assert_eq!(editor.rope, "before\n");
    assert!(!editor.modified);
}

#[test]
fn snapshot_creation_and_repeated_checkpoint_cannot_preserve_an_incorrect_clean_flag() {
    for repeat_checkpoint in [false, true] {
        let mut editor = saved_editor("original\n");
        let saved = editor.rope.clone();
        // Simulate an intermediate edit whose caller has not updated the flag.
        editor.rope.remove(0..4);
        if repeat_checkpoint {
            editor.record_saved_snapshot(saved);
        } else {
            editor.save_undo();
            assert!(editor.undo_stack.back().unwrap().modified);
        }
        assert!(editor.modified);
        assert_eq!(editor.rope, "inal\n");
    }
}

#[test]
fn recovered_clean_snapshots_reuse_the_saved_rope_after_validation() {
    let text = "unchanged line\n".repeat(50_000);
    let mut editor = saved_editor(&text);
    let saved = editor.rope.clone();
    editor.paste_plain_with_text("changed\n");
    let restored = Rope::from_str(&text);
    assert!(!restored.is_instance(&saved));
    editor.import_undo_redo(vec![snapshot(restored, false)], vec![]);
    for _ in 0..3 {
        editor.undo();
        assert!(!editor.modified);
        assert!(
            editor.rope.is_instance(&saved),
            "later checkpoints must use the constant-time identity check"
        );
        editor.redo();
        assert!(editor.modified);
    }
}

#[test]
fn clean_snapshot_verification_preserves_explicit_non_text_changes() {
    let mut editor = saved_editor("unchanged text\n");
    editor.encoding = "UTF-16 LE";
    editor.modified = true;
    editor.insert_char('x');
    editor.undo();
    assert_eq!(editor.rope, "unchanged text\n");
    assert!(
        editor.modified,
        "an unsaved encoding change still needs saving"
    );
}

#[test]
fn undo_redraws_the_restored_code_before_any_background_work_finishes() {
    let source = include_str!("../examples/rebuild_syntax_cache.rs");
    let mut app = app(saved_editor(source));
    app.show_minimap = false;
    app.sticky_scroll = false;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 35)).unwrap();
    let draw = |app: &mut App, terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>| {
        terminal
            .draw(|frame| crate::ui_editor::draw_editor(frame, app, frame.area()))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
    };
    assert!(draw(&mut app, &mut terminal).contains("folder.display()"));
    select_print_arm(app.editor_mut().unwrap());
    key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
    assert!(!draw(&mut app, &mut terminal).contains("folder.display()"));
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    let rendered = draw(&mut app, &mut terminal);
    assert!(rendered.contains("Ok((count, fingerprint))"));
    assert!(rendered.contains("folder.display()"));
    assert_eq!(app.editor().unwrap().rope, source);
    assert!(!app.tabs[0].has_unsaved_changes());
}

#[test]
fn dirty_history_flags_are_cleared_when_undo_or_redo_matches_saved_text() {
    for source in [
        "",
        "saved",
        "saved\n",
        "\t界🙂 é\r\nend",
        "a\rb\rc",
        "a\r\nb\nc\r",
    ] {
        for redo_first in [false, true] {
            let mut editor = saved_editor(source);
            let baseline = editor.rope.clone();
            editor.paste_plain_with_text("changed");
            let changed = editor.rope.clone();
            let restored = snapshot(Rope::from_str(source), true);
            if redo_first {
                editor.import_undo_redo(vec![], vec![restored]);
            } else {
                editor.import_undo_redo(vec![restored], vec![]);
            }
            for _ in 0..3 {
                if redo_first {
                    editor.redo();
                } else {
                    editor.undo();
                }
                assert_eq!(editor.rope, source);
                assert!(
                    !editor.modified,
                    "source={source:?}, redo_first={redo_first}"
                );
                assert!(editor.rope.is_instance(&baseline));
                assert_eq!(editor.unsaved_line_status(editor.cursor_line), ' ');
                assert!(!editor.has_unsaved_change_in_range(0, editor.line_count()));
                if redo_first {
                    editor.undo();
                } else {
                    editor.redo();
                }
                assert_eq!(editor.rope, changed);
                assert!(editor.modified);
            }
        }
    }
}

#[test]
fn undo_to_a_recreated_saved_buffer_clears_the_tab_dot_and_close_warning() {
    let source = "line one\nline two\n";
    let mut editor = saved_editor(source);
    editor.select_all();
    editor.paste_plain_with_text("different\n");
    editor.select_all();
    editor.paste_plain_with_text(source);
    assert!(!editor.rope.is_instance(editor.saved_rope.as_ref().unwrap()));
    editor.insert_char('x');
    let mut app = app(editor);
    key(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
    assert_eq!(app.editor().unwrap().rope, source);
    assert!(!app.tabs[0].has_unsaved_changes());
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 1)).unwrap();
    terminal
        .draw(|frame| crate::ui_chrome_tabs::draw_tab_bar(frame, &app, frame.area()))
        .unwrap();
    assert!(
        !terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|cell| cell.symbol() == "●")
    );
    app.close_tab(0);
    assert!(!app.confirm_close_active);
    assert!(app.tabs.is_empty());
}

#[test]
fn saving_recreated_content_recognizes_earlier_equal_history_snapshots() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("equal-history.txt");
    std::fs::write(&path, "original\n").unwrap();
    let mut editor = saved_editor("original\n");
    editor.file_path = Some(path.clone());
    let first = editor.rope.clone();
    for text in ["different\n", "original\n"] {
        editor.select_all();
        editor.paste_plain_with_text(text);
    }
    assert!(!editor.rope.is_instance(&first));
    editor.save_file_with_options(false, false).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "original\n");
    for _ in 0..3 {
        editor.undo();
        assert_eq!(editor.rope, "different\n");
        assert!(editor.modified);
        editor.undo();
        assert_eq!(editor.rope, "original\n");
        assert!(
            !editor.modified,
            "equal content predates this save, but still matches disk"
        );
        editor.redo();
        assert!(editor.modified);
        editor.redo();
        assert_eq!(editor.rope, "original\n");
        assert!(!editor.modified);
    }
}

#[test]
fn equal_lengths_do_not_make_different_history_content_clean() {
    let source = format!("{}last=1\n", "unchanged line\n".repeat(20_000));
    for position in [0, source.len() / 2, source.len() - 2] {
        let mut editor = saved_editor(&source);
        let mut changed = editor.rope.clone();
        changed.remove(position..position + 1);
        changed.insert(position, "X");
        assert_eq!(changed.len_bytes(), editor.rope.len_bytes());
        assert_eq!(changed.len_lines(), editor.rope.len_lines());
        for modified in [true, false] {
            editor.import_undo_redo(vec![snapshot(changed.clone(), modified)], vec![]);
            editor.undo();
            assert_eq!(editor.rope, changed);
            assert!(
                editor.modified,
                "difference at {position} must not be hidden"
            );
        }
    }
}

#[test]
fn undo_preserves_an_encoding_change_made_after_the_saved_history_entry() {
    let mut editor = saved_editor("unchanged\n");
    editor.insert_char('x');
    editor.encoding = "UTF-16 LE";
    editor.undo();
    assert_eq!(editor.rope, "unchanged\n");
    assert!(
        editor.modified,
        "the old clean history flag cannot hide a pending encoding change"
    );
    assert_eq!(editor.unsaved_line_status(0), ' ');
    assert!(!editor.has_unsaved_change_in_range(0, editor.line_count()));
    editor.redo();
    assert!(editor.modified);
    editor.encoding = "UTF-8";
    editor.undo();
    assert!(!editor.modified);
}

#[test]
fn loaded_and_reloaded_encoding_is_the_saved_baseline_for_undo() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("encoded.txt");
    for encoding in [
        "UTF-8 with BOM",
        "UTF-16 LE",
        "UTF-16 BE",
        "ASCII",
        "ISO-8859-1",
    ] {
        let mut writer = saved_editor("saved\r\n");
        std::fs::write(&path, "saved\r\n").unwrap();
        writer.file_path = Some(path.clone());
        writer.encoding = encoding;
        writer.save_file_with_options(false, false).unwrap();
        for background in [false, true] {
            let mut editor = saved_editor("older content");
            if background {
                let loaded = crate::editor::load_text_file(&path).unwrap();
                editor.apply_bg_loaded(loaded.rope, loaded.encoding, loaded.file_size, u64::MAX);
                editor.file_path = Some(path.clone());
            } else {
                editor.open_file(&path).unwrap();
            }
            assert_eq!(editor.saved_encoding, editor.encoding);
            editor.insert_char('x');
            editor.undo();
            assert_eq!(editor.rope, "saved\r\n");
            assert!(!editor.modified, "{encoding}, background={background}");
            editor.encoding = "UTF-8";
            editor.reload_from_disk().unwrap();
            editor.insert_char('x');
            editor.undo();
            assert!(!editor.modified);
        }
    }
}

#[test]
fn background_save_preserves_encoding_changes_made_after_it_started() {
    for newer_edit in [false, true] {
        let mut editor = saved_editor("saved\n");
        editor.insert_char('x');
        let request = editor.background_save_request(false, false, false).unwrap();
        editor.encoding = "UTF-16 LE";
        if newer_edit {
            editor.break_undo_group();
            editor.insert_char('y');
        }
        editor.apply_background_save(
            request.source_content_version,
            &request.rope,
            request.rope.clone(),
            request.encoding,
            None,
            None,
            None,
            u64::MAX,
        );
        assert_eq!(editor.saved_encoding, "UTF-8");
        assert!(editor.modified);
        if newer_edit {
            editor.undo();
            assert!(
                editor.modified,
                "undoing the text edit leaves the encoding change"
            );
        }
        assert_eq!(editor.rope, "xsaved\n");
        editor.encoding = "UTF-8";
        editor.insert_char('z');
        editor.undo();
        assert_eq!(editor.rope, "xsaved\n");
        assert!(!editor.modified);
    }
}

#[test]
fn background_save_recognizes_equal_content_recreated_while_writing() {
    let mut editor = saved_editor("before\n");
    editor.select_all();
    editor.paste_plain_with_text("saved\n");
    let request = editor.background_save_request(false, false, false).unwrap();
    editor.select_all();
    editor.paste_plain_with_text("different\n");
    editor.select_all();
    editor.paste_plain_with_text("saved\n");
    assert!(!editor.rope.is_instance(&request.rope));
    editor.apply_background_save(
        request.source_content_version,
        &request.rope,
        request.rope.clone(),
        request.encoding,
        None,
        None,
        None,
        u64::MAX,
    );
    assert!(!editor.modified);
    editor.undo();
    assert!(editor.modified);
    editor.undo();
    assert!(!editor.modified);
    editor.redo();
    assert!(editor.modified);
    editor.redo();
    assert!(!editor.modified);
}
