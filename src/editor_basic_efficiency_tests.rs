use super::*;
use std::{hint::black_box, time::Instant};

fn wrapped_editor(lines: usize) -> Editor {
    let mut editor = Editor::test_with_text(
        &"alpha βeta gamma delta epsilon zeta eta theta iota\n".repeat(lines),
    );
    editor.word_wrap = true;
    editor.viewport_width = 19;
    editor.viewport_height = 20;
    editor.cursor_line = 10;
    editor.cursor_col = 8;
    install_exact(&mut editor);
    editor
}

fn install_exact(editor: &mut Editor) {
    let folds: Vec<_> = editor.folded_ranges.iter().map(|(&a, &b)| (a, b)).collect();
    let width = editor.effective_wrap_width();
    let offsets =
        Editor::build_wrap_row_offsets(&editor.rope, width, editor.tab_size, &folds, || false)
            .unwrap();
    assert!(editor.install_wrap_layout(width, offsets));
}

fn assert_exact(editor: &mut Editor) {
    editor.refresh_wrap_caches();
    let width = editor.effective_wrap_width();
    assert!(
        editor.wrap_layout_ready(width),
        "a local edit must retain exact scrolling without a new worker"
    );
    let folds: Vec<_> = editor.folded_ranges.iter().map(|(&a, &b)| (a, b)).collect();
    let expected =
        Editor::build_wrap_row_offsets(&editor.rope, width, editor.tab_size, &folds, || false)
            .unwrap();
    assert_eq!(editor.wrapped_total_rows(width), *expected.last().unwrap());
    for line in (0..editor.line_count()).step_by(97).chain(8..14) {
        assert_eq!(
            editor.wrapped_row_for_position(line, 0, width),
            expected[line],
            "line {line}"
        );
    }
}

#[test]
fn local_history_retains_exact_wrap_layout_through_undo_redo() {
    let mut editor = wrapped_editor(10_000);
    for _ in 0..8 {
        editor.insert_char('λ');
        assert_exact(&mut editor);
        editor.undo();
        assert_exact(&mut editor);
        editor.redo();
        assert_exact(&mut editor);
        editor.break_undo_group();
    }
}

#[test]
fn structural_history_and_fold_removal_still_invalidate_wrap_layout() {
    let mut editor = wrapped_editor(10_000);
    editor.insert_char('\n');
    install_exact(&mut editor);
    editor.undo();
    editor.refresh_wrap_caches();
    assert!(!editor.wrap_layout_ready(19));
    install_exact(&mut editor);
    editor.redo();
    editor.refresh_wrap_caches();
    assert!(!editor.wrap_layout_ready(19));
    install_exact(&mut editor);
    editor.folded_ranges.insert(50, 200);
    install_exact(&mut editor);
    editor.insert_char('x');
    editor.refresh_wrap_caches();
    editor.undo();
    assert!(editor.folded_ranges.is_empty());
    assert!(
        !editor.wrap_layout_ready(19),
        "unfolding must remove zero-row weights"
    );
    install_exact(&mut editor);
    assert_exact(&mut editor);
}

#[test]
fn shared_view_refresh_and_save_cleanup_retain_local_wrap_layout() {
    let mut source = wrapped_editor(10_000);
    let mut peer = Editor::new();
    peer.copy_document_from(&source);
    peer.word_wrap = true;
    peer.viewport_width = 19;
    install_exact(&mut peer);
    source.insert_char('λ');
    peer.refresh_document();
    assert_exact(&mut peer);
    peer.undo();
    source.refresh_document();
    assert_exact(&mut source);
    assert_exact(&mut peer);

    source.file_path = Some("wrap-cleanup-test.txt".into());
    source.cursor_line = 12;
    source.cursor_col = source.line_len(12);
    source.paste_plain_with_text("  \t");
    source.refresh_wrap_caches();
    let request = source.background_save_request(true, false, false).unwrap();
    let prepared = request.prepare_text();
    source.apply_background_save(
        request.source_content_version,
        &request.rope,
        prepared,
        "UTF-8",
        None,
        None,
        Some((12, 12)),
        LARGE_FILE_THRESHOLD,
    );
    assert_exact(&mut source);
    peer.refresh_document();
    assert_exact(&mut peer);
    peer.tab_size = 8;
    peer.publish_document();
    source.refresh_document();
    assert!(
        !source.wrap_layout_ready(19),
        "indentation changes retain their full invalidation"
    );
}

fn saved_editor(text: &str, trim: bool, newline: bool, crlf: bool) -> Editor {
    let mut editor = Editor::test_with_text(text);
    editor.file_path = Some("prepared-save-test.txt".into());
    editor.cached_line_ending = if crlf { "CRLF" } else { "LF" };
    let normalized = prepare_save_rope(&editor.rope, trim, newline, crlf);
    editor.rope = normalized.clone();
    editor.record_saved_snapshot(normalized);
    editor.record_save_options(Some(editor.save_options(trim, newline)));
    editor
}

#[test]
fn save_preparation_reuse_requires_exact_source_and_options() {
    for trim in [false, true] {
        for newline in [false, true] {
            for crlf in [false, true] {
                let editor = saved_editor("λ  \nnext\t\r\nlast  ", trim, newline, crlf);
                let request = editor
                    .background_save_request(trim, newline, false)
                    .unwrap();
                assert!(request.prepare_text().is_instance(&request.rope));
                for change in 0..5 {
                    let mut changed = request.clone();
                    match change {
                        0 => changed.trim_trailing = !trim,
                        1 => changed.insert_final_newline = !newline,
                        2 => changed.line_ending = if crlf { "LF" } else { "CRLF" },
                        3 => changed.rope.insert(0, "changed \n"),
                        _ => changed.prepared_options = None,
                    }
                    let reference = prepare_save_rope(
                        &changed.rope,
                        changed.trim_trailing,
                        changed.insert_final_newline,
                        changed.line_ending == "CRLF",
                    );
                    assert_eq!(changed.prepare_text(), reference);
                }
            }
        }
    }
}

#[test]
fn prepared_save_still_validates_external_edits_and_handles_option_changes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("file.txt");
    let mut editor = saved_editor("first  \nlast  ", false, false, false);
    editor.file_path = Some(path.clone());
    fs::write(&path, editor.rope.to_string()).unwrap();
    let request = editor.background_save_request(false, false, false).unwrap();
    assert!(!write_background_save(&request).unwrap().disk_written);
    fs::write(&path, "outside edit\n").unwrap();
    assert!(save::is_conflict(
        &write_background_save(&request).unwrap_err()
    ));
    assert_eq!(fs::read_to_string(&path).unwrap(), "outside edit\n");
    fs::write(&path, editor.rope.to_string()).unwrap();
    let changed = editor.background_save_request(true, true, false).unwrap();
    let saved = write_background_save(&changed).unwrap();
    assert!(saved.disk_written);
    assert_eq!(fs::read_to_string(&path).unwrap(), "first\nlast\n");
    assert_eq!(saved.rope.to_string(), "first\nlast\n");
}

#[test]
fn borrowed_search_prefill_preserves_selection_and_line_ending_semantics() {
    for text in ["", "α界 plain", "a\nb\r\nc", "α\rβ\rc", "\n\n", "a\r \nb"] {
        let mut editor = Editor::test_with_text(text);
        for a in 0..editor.line_count() + 1 {
            for b in 0..editor.line_count() + 1 {
                for col in [0, 1, 3, 100] {
                    editor.selection_anchor = Some((a, col));
                    editor.cursor_line = b;
                    editor.cursor_col = 3;
                    let expected = editor
                        .selected_text()
                        .filter(|s| !s.is_empty() && !s.contains('\n'));
                    assert_eq!(
                        editor.selected_search_text(),
                        expected,
                        "{text:?}, {a}, {b}, {col}"
                    );
                }
            }
        }
    }
    let mut editor = Editor::test_with_text(&"selection across several rows\n".repeat(150_000));
    editor.select_all();
    assert_eq!(editor.selected_search_text(), None);
}

#[test]
fn find_replace_entry_points_keep_multiline_and_single_line_prefill_behavior() {
    use crate::app::{ActivePane, App, Tab};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    for route in 0..3 {
        for text in ["whole query", "first\nsecond"] {
            let mut editor = Editor::test_with_text(text);
            editor.select_all();
            let mut app = App::test_new();
            app.tabs = vec![Tab::new_file(editor)];
            app.active_tab = 0;
            app.active_pane = ActivePane::Editor;
            app.search_query = "existing".into();
            match route {
                0 => {
                    app.execute_palette_command("find");
                }
                1 => {
                    app.execute_palette_command("find_replace");
                }
                _ => {
                    crate::input_keyboard::handle_keyboard_event(
                        &mut app,
                        KeyEvent::new(KeyCode::Char('h'), KeyModifiers::CONTROL),
                    );
                }
            }
            assert_eq!(
                app.search_query,
                if text.contains('\n') {
                    "existing"
                } else {
                    text
                }
            );
            assert!(app.search_active);
            assert_eq!(app.replace_mode, route != 0);
        }
    }
}

#[test]
#[ignore = "manual single-CPU benchmark of unchanged save preparation and rejected search-prefill copying"]
fn benchmark_basic_save_and_search_preparation() {
    for operation in ["save", "find"] {
        let mut editor = saved_editor(
            &"alpha beta gamma delta 0123456789\r\n".repeat(120_000),
            true,
            true,
            true,
        );
        editor.select_all();
        let request = editor.background_save_request(true, true, false).unwrap();
        let mut times = [Vec::new(), Vec::new()];
        for round in 0..7 {
            for optimized in [round % 2 == 0, round % 2 != 0] {
                let start = Instant::now();
                for _ in 0..16 {
                    if operation == "save" {
                        black_box(if optimized {
                            request.prepare_text()
                        } else {
                            prepare_save_rope(&request.rope, true, true, true)
                        });
                    } else {
                        black_box(if optimized {
                            editor.selected_search_text()
                        } else {
                            editor
                                .selected_text()
                                .filter(|s| !s.is_empty() && !s.contains('\n'))
                        });
                    }
                }
                times[usize::from(optimized)].push(start.elapsed());
            }
        }
        for values in &mut times {
            values.sort_unstable();
        }
        eprintln!(
            "basic preparation operation={operation} bytes={} iterations=16 before_us={} after_us={}",
            request.rope.len_bytes(),
            times[0][3].as_micros(),
            times[1][3].as_micros()
        );
    }
}

#[test]
#[ignore = "manual single-CPU benchmark: history navigation followed by exact wrap-layout availability"]
fn benchmark_basic_wrapped_history() {
    let mut times = [Vec::new(), Vec::new()];
    for round in 0..7 {
        let mut results = Vec::new();
        for optimized in [round % 2 == 0, round % 2 != 0] {
            let mut editor = wrapped_editor(50_000);
            editor.insert_char('λ');
            editor.refresh_wrap_caches();
            let start = Instant::now();
            for _ in 0..12 {
                if !optimized {
                    editor.cached_row_offsets = None;
                }
                editor.undo();
                if !editor.wrap_layout_ready(19) {
                    install_exact(&mut editor);
                }
                if !optimized {
                    editor.cached_row_offsets = None;
                }
                editor.redo();
                if !editor.wrap_layout_ready(19) {
                    install_exact(&mut editor);
                }
            }
            times[usize::from(optimized)].push(start.elapsed());
            results.push((
                editor.rope.clone(),
                editor.cursor_line,
                editor.cursor_col,
                editor.wrapped_total_rows(19),
            ));
        }
        assert_eq!(results[0], results[1]);
    }
    for values in &mut times {
        values.sort_unstable();
    }
    eprintln!(
        "basic wrapped history lines=50000 cycles=12 before_us={} after_us={}",
        times[0][3].as_micros(),
        times[1][3].as_micros()
    );
}
