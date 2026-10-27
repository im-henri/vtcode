use super::*;
use crate::{app::Tab, editor::Editor};
use std::time::Duration;

fn app_with_text(text: &str) -> App {
    let mut app = App::new_deferred(std::env::current_dir().unwrap());
    app.source_control_enabled = false;
    app.tabs.push(Tab::new_file(Editor::test_with_text(text)));
    app.active_pane = ActivePane::Editor;
    app
}

fn finish(app: &mut App) {
    app.wait_for_background(Duration::from_secs(20), |app| !app.paste_in_progress());
}

#[test]
fn million_line_copy_paste_is_atomic_undoable_and_never_exports_an_oversized_terminal_packet() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let payload = "row é🦀0123456789\n".repeat(1_000_000);
    assert!(payload.len() > 10 * 1024 * 1024);
    let mut app = app_with_text(&payload);
    app.editor_mut().unwrap().select_all();
    app.copy_editor_selection();
    assert_eq!(app.editor().unwrap().clipboard.len(), payload.len());
    assert!(
        crate::clipboard::test_terminal_output().is_empty(),
        "do not flood the host with a large OSC52 packet"
    );
    assert!(
        app.side_notice.visible(),
        "copy export failure must be visible"
    );
    app.tabs
        .push(Tab::new_file(Editor::test_with_text("before\n")));
    app.active_tab = 1;
    app.large_file_threshold_mb = 1;
    let editor = app.editor_mut().unwrap();
    editor.cursor_line = 1;
    editor.word_wrap = true;
    let original = editor.rope.clone();
    let navigation = editor.navigation_id;
    app.execute_palette_command("paste");
    assert!(app.paste_pending_for(navigation));
    assert!(app.editor().unwrap().rope.is_instance(&original));
    assert_eq!(
        crate::ui_loading::editor_label(&app, app.editor().unwrap()),
        Some("Preparing paste…")
    );
    finish(&mut app);
    let editor = app.editor_mut().unwrap();
    assert_eq!(editor.rope.len_lines(), 1_000_002);
    assert_eq!(editor.rope.len_bytes(), 7 + payload.len());
    assert_eq!(editor.rope.slice(7..).to_string(), payload);
    assert_eq!(editor.undo_stack.len(), 1);
    assert!(
        editor
            .undo_stack
            .back()
            .unwrap()
            .rope
            .is_instance(&original)
    );
    assert!(!editor.word_wrap);
    editor.undo();
    assert_eq!(editor.rope, "before\n");
    assert_eq!((editor.cursor_line, editor.cursor_col), (1, 0));
    editor.redo();
    assert_eq!(editor.rope.len_lines(), 1_000_002);
}

#[test]
fn large_paste_cancellation_covers_completed_but_unapplied_results_and_repeated_requests() {
    let mut app = app_with_text("keep");
    let original = app.editor().unwrap().rope.clone();
    app.paste_editor_text("x".repeat(BACKGROUND_BYTES + 1), true);
    let serial = app.bulk_paste.job.as_ref().unwrap().serial;
    app.paste_editor_text("y".repeat(BACKGROUND_BYTES + 1), true);
    assert_eq!(app.bulk_paste.job.as_ref().unwrap().serial, serial);
    app.cancel_pending_paste();
    finish(&mut app);
    assert!(app.editor().unwrap().rope.is_instance(&original));
    assert!(app.editor().unwrap().undo_stack.is_empty());
    assert!(
        app.notification_history
            .iter()
            .any(|(text, _, _)| text.contains("Paste cancelled"))
    );
}

#[test]
fn large_paste_rejects_edits_cursor_changes_and_closed_or_replaced_destinations() {
    for change in 0..4 {
        let mut app = app_with_text("original");
        app.paste_editor_text("x".repeat(BACKGROUND_BYTES + 1), true);
        match change {
            0 => app.editor_mut().unwrap().insert_char('!'),
            1 => app.editor_mut().unwrap().cursor_col = 1,
            2 => {
                app.tabs.clear();
                app.tabs
                    .push(Tab::new_file(Editor::test_with_text("replacement")));
            }
            _ => {
                app.editor_mut().unwrap().read_only = true;
            }
        }
        let expected = app.editor().unwrap().rope.clone();
        finish(&mut app);
        assert!(
            app.editor().unwrap().rope.is_instance(&expected),
            "change={change}"
        );
        assert!(
            app.notification_history
                .iter()
                .any(|(text, _, _)| text.contains("Paste cancelled"))
        );
        assert!(app.side_notice.visible());
    }
}

#[test]
fn large_paste_finishes_in_the_original_tab_after_a_tab_switch() {
    let mut app = app_with_text("original");
    app.paste_editor_text("x".repeat(BACKGROUND_BYTES + 1), true);
    app.tabs
        .push(Tab::new_file(Editor::test_with_text("other document")));
    app.active_tab = 1;
    finish(&mut app);
    assert_eq!(app.editor().unwrap().rope, "other document");
    assert!(app.tabs[0].editor.modified);
    assert_eq!(
        app.tabs[0].editor.rope.len_bytes(),
        "original".len() + BACKGROUND_BYTES + 1
    );
}

#[test]
fn rejected_terminal_paste_stays_rejected_after_the_original_worker_finishes() {
    let mut app = app_with_text("keep");
    app.paste_editor_text("x".repeat(BACKGROUND_BYTES + 1), true);
    for bytes in [1_000_000, 2_000_000, 3_000_000] {
        app.terminal_paste_status(PasteStatus::Receiving { bytes });
    }
    assert_eq!(
        app.notification_history
            .iter()
            .filter(|(text, _, _)| text.contains("additional paste was cancelled"))
            .count(),
        1
    );
    app.wait_for_background(Duration::from_secs(20), |app| app.bulk_paste.job.is_none());
    let expected = app.editor().unwrap().rope.clone();
    crate::app_event_loop::handle_paste_input(
        &mut app,
        "this already-queued second paste must not be inserted",
    );
    assert!(app.editor().unwrap().rope.is_instance(&expected));
    assert!(!app.paste_in_progress());
    assert_eq!(app.editor().unwrap().undo_stack.len(), 1);
}

#[test]
fn large_smart_paste_at_column_zero_preserves_copied_indentation() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for indent in ["    ", "\t "] {
        let body = format!("{indent}// café comment\r\n{indent}    // nested comment\r\n");
        let payload = body.repeat(BACKGROUND_BYTES / body.len() + 1);
        let normalized = payload.replace("\r\n", "\n");
        crate::clipboard::remember_line_copy(&payload, false);
        let original = "before\n\nafter";
        let mut app = app_with_text(original);
        app.editor_mut().unwrap().cursor_line = 1;
        app.paste_editor_text(payload, true);
        assert!(app.paste_in_progress());
        finish(&mut app);
        let editor = app.editor_mut().unwrap();
        let expected = format!("before\n{normalized}\nafter");
        assert!(editor.rope == expected.as_str(), "background paste must preserve copied indentation at column zero");
        assert_eq!((editor.cursor_line, editor.cursor_col), (1 + normalized.bytes().filter(|&byte| byte == b'\n').count(), 0));
        assert_eq!(editor.undo_stack.len(), 1);
        editor.undo();
        assert_eq!(editor.rope.to_string(), original);
        editor.redo();
        assert_eq!(editor.rope.to_string(), expected);
    }
}

#[test]
fn prepared_paste_matches_inline_semantics_for_unicode_indentation_selection_and_multicursors() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    for text in [
        "é🦀\n  next\n",
        "  first\r\n    next\r",
        "\n \t\n",
        "value",
        "a\nb",
    ] {
        crate::clipboard::remember_line_copy(text, false);
        for smart in [false, true] {
            for mode in 0..4 {
                let configure = |editor: &mut Editor| {
                    editor.cursor_line = 1;
                    editor.cursor_col = 2;
                    editor.bookmarks.extend([0, 1, 3]);
                    editor.folded_ranges.insert(2, 3);
                    if mode == 1 {
                        editor.selection_anchor = Some((0, 2));
                    }
                    if mode == 2 {
                        editor.selection_anchor = Some((3, 2));
                    }
                    if mode == 3 {
                        editor.extra_cursors = vec![(2, 1)];
                        editor.extra_cursor_anchors = vec![None];
                    }
                };
                let source = "header\n  target\nmore\ntail";
                let mut expected = Editor::test_with_text(source);
                configure(&mut expected);
                if smart {
                    expected.paste_with_text(text);
                } else {
                    expected.paste_plain_with_text(text);
                }
                let mut editor = Editor::test_with_text(source);
                configure(&mut editor);
                let prepared = crate::editor_paste::Snapshot::capture(&editor, text)
                    .unwrap()
                    .prepare(text.into(), smart, &CancellationToken::new())
                    .unwrap();
                prepared.apply(&mut editor, u64::MAX).unwrap();
                assert_eq!(
                    editor.rope, expected.rope,
                    "{text:?}, smart={smart}, mode={mode}"
                );
                assert_eq!(
                    (editor.cursor_line, editor.cursor_col),
                    (expected.cursor_line, expected.cursor_col)
                );
                assert_eq!(editor.extra_cursors, expected.extra_cursors);
                assert_eq!(editor.extra_cursor_anchors, expected.extra_cursor_anchors);
                assert_eq!(editor.bookmarks, expected.bookmarks);
                assert_eq!(editor.folded_ranges, expected.folded_ranges);
                assert_eq!(editor.undo_stack.len(), 1);
                editor.undo();
                assert_eq!(editor.rope, source);
            }
        }
    }
}

#[test]
fn large_paste_line_copy_keeps_crlf_and_cross_thread_clipboard_semantics() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    let payload = format!("{}\r\n", "λ".repeat(BACKGROUND_BYTES));
    crate::clipboard::remember_line_copy(&payload, true);
    let mut app = app_with_text("target");
    app.editor_mut().unwrap().cursor_col = 3;
    app.paste_editor_text(payload, true);
    finish(&mut app);
    let editor = app.editor().unwrap();
    assert_eq!(editor.rope.len_lines(), 2);
    assert_eq!(editor.rope.line(1), "target");
    assert_eq!((editor.cursor_line, editor.cursor_col), (1, 3));
}

#[test]
fn receiving_paste_errors_cancellation_and_destination_changes_are_visible_and_non_destructive() {
    let mut app = app_with_text("keep");
    app.terminal_paste_status(PasteStatus::Receiving { bytes: 1_000_000 });
    assert!(app.side_notice.visible());
    app.cancel_pending_paste();
    crate::app_event_loop::handle_paste_input(
        &mut app,
        "must not paste even if completion was already queued",
    );
    assert_eq!(app.editor().unwrap().rope, "keep");
    app.terminal_paste_status(PasteStatus::Receiving { bytes: 1_000_000 });
    app.tabs
        .push(Tab::new_file(Editor::test_with_text("other")));
    app.active_tab = 1;
    crate::app_event_loop::handle_paste_input(&mut app, "must not paste in another document");
    assert_eq!(app.editor().unwrap().rope, "other");
    for reason in [
        PasteFailure::AllocationFailed,
        PasteFailure::Interrupted,
        PasteFailure::Cancelled,
    ] {
        app.terminal_paste_status(PasteStatus::Failed {
            bytes: 10_000_000,
            reason,
        });
        assert!(app.side_notice.visible());
        assert_eq!(app.editor().unwrap().rope, "other");
    }
}

#[test]
fn pending_paste_cannot_silently_save_the_old_text() {
    let mut app = app_with_text("keep");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("paste.txt");
    app.editor_mut().unwrap().file_path = Some(path.clone());
    app.editor_mut().unwrap().modified = true;
    let request = app
        .editor()
        .unwrap()
        .background_save_request(false, false, false)
        .unwrap();
    app.paste_editor_text("x".repeat(BACKGROUND_BYTES + 1), true);
    // Saving another view of the same document must also wait for the paste.
    let mut other_view = Editor::new();
    other_view.copy_document_from(&app.tabs[0].editor);
    app.tabs.push(Tab::new_file(other_view));
    app.active_tab = 1;
    app.save_current_file();
    assert!(!app.save_as_active);
    app.execute_palette_command("save_as");
    assert!(!app.save_as_active);
    app.execute_palette_command("save_all");
    assert!(
        app.save_open_file_by_path(&path)
            .unwrap_err()
            .contains("Paste is still in progress")
    );
    assert!(!app.queue_background_save_job(request));
    assert!(
        crate::input_dialogs::save_text_documents_before_exit(&mut app)
            .unwrap_err()
            .contains("paste")
    );
    app.auto_save = crate::app::AutoSaveMode::OnWindowChange;
    app.auto_save_on_window_change();
    assert!(app.pending_file_saves.is_empty());
    assert!(!path.exists(), "never save an obsolete pre-paste snapshot");
    assert!(
        app.notification_history
            .iter()
            .any(|(text, _, _)| text.contains("still in progress"))
    );
    app.cancel_pending_paste();
    finish(&mut app);
}

#[test]
fn million_line_paste_keeps_default_settings_and_wrap_work_off_the_input_thread() {
    let mut app = app_with_text("before\n");
    let editor = app.editor_mut().unwrap();
    editor.cursor_line = 1;
    editor.word_wrap = true;
    let payload = "row é🦀0123456789\n".repeat(1_000_000);
    let bytes = payload.len();
    let started = std::time::Instant::now();
    app.paste_editor_text(payload, true);
    let dispatch = started.elapsed();
    assert_eq!(app.editor().unwrap().rope, "before\n");
    let preparing = std::time::Instant::now();
    finish(&mut app);
    let prepared_and_applied = preparing.elapsed();
    let editor = app.editor_mut().unwrap();
    assert_eq!(editor.rope.len_lines(), 1_000_002);
    assert!(
        editor.word_wrap,
        "retain the user's wrap preference below the large-file threshold"
    );
    assert!(editor.needs_background_wrap_layout(80));
    assert!(
        editor.cached_row_offsets.is_none(),
        "do not build a million-line wrap map while applying the paste"
    );
    eprintln!(
        "million-line paste ({bytes} UTF-8 bytes): dispatch {dispatch:?}; prepare and apply {prepared_and_applied:?}"
    );
}

#[test]
fn clipboard_history_pastes_its_entry_instead_of_reading_the_live_clipboard() {
    let _clipboard = crate::clipboard::test_terminal_clipboard();
    crate::clipboard::set_text("live clipboard").unwrap();
    let mut app = app_with_text("target:");
    app.editor_mut().unwrap().cursor_col = 7;
    app.clipboard_history
        .extend(["older".into(), "latest".into()]);
    let key = crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Char('V'),
        crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::SHIFT,
    );
    crate::input_keyboard::handle_keyboard_event(&mut app, key);
    assert_eq!(app.editor().unwrap().rope, "target:latest");
    assert_eq!(app.editor().unwrap().undo_stack.len(), 1);
    crate::input_keyboard::handle_keyboard_event(&mut app, key);
    assert_eq!(app.editor().unwrap().rope, "target:older");
    app.editor_mut().unwrap().undo();
    assert_eq!(app.editor().unwrap().rope, "target:");
}
