use super::Editor;
use crate::ui_editor_highlight::{HighlightedLine, PLAIN_STYLE};

fn prime_rows(editor: &Editor, rows: std::ops::Range<usize>) {
    for row in rows {
        let line = editor.rope.line(row);
        let text = line.to_string();
        let text = text.trim_end_matches(['\r', '\n']);
        let highlighted = HighlightedLine::new(None, text, text.chars().count(), PLAIN_STYLE);
        editor.render_line_cache.borrow_mut().line(
            row,
            (
                editor.content_version,
                editor.wrap_revision,
                editor.tab_size,
            ),
            &editor.rope,
            &highlighted,
            None,
            200,
            true,
        );
        editor.can_fold_line(row);
    }
}

fn clear_dirty(editor: &mut Editor) {
    editor.syntax_dirty_from = None;
    editor.syntax_dirty_through = None;
    editor.wrap_dirty_range = None;
    editor.reset_unsaved_diff_cache();
}

#[test]
fn shared_typing_keeps_unchanged_glyphs_and_fold_hints() {
    let mut active = Editor::test_with_text(&"fn café() { work(); }\n".repeat(2_000));
    active.record_saved_snapshot(active.rope.clone());
    let mut other = Editor::new();
    other.copy_document_from(&active);
    prime_rows(&other, 10..70);
    clear_dirty(&mut other);
    let builds = other.render_line_cache.borrow().builds;
    let folds = other.fold_hint_cache.borrow().builds;
    for _ in 0..4 {
        active.set_cursor(1_000, 5);
        active.insert_char('x');
        other.refresh_document();
        assert_eq!(other.syntax_dirty_from, Some(1_000));
        assert_eq!(other.syntax_dirty_through, Some(1_000));
        assert_eq!(other.unsaved_diff_dirty_from, Some(1_000));
        assert_eq!(other.unsaved_diff_dirty_through, Some(1_000));
        prime_rows(&other, 10..70);
        assert_eq!(other.render_line_cache.borrow().builds, builds);
        assert_eq!(other.fold_hint_cache.borrow().builds, folds);
        assert!(other.rope.is_instance(&active.rope));
        clear_dirty(&mut other);
    }
}

#[test]
fn shared_refresh_tracks_missed_edits_and_history_without_dirtying_the_prefix() {
    let mut active = Editor::test_with_text(&"let café = [1, 2];\n".repeat(300));
    active.record_saved_snapshot(active.rope.clone());
    let mut other = Editor::new();
    other.copy_document_from(&active);
    prime_rows(&other, 0..30);
    clear_dirty(&mut other);
    for row in [100, 200, 150] {
        active.set_cursor(row, 5);
        active.paste_plain_with_text("界");
    }
    other.refresh_document();
    assert_eq!(other.syntax_dirty_from, Some(100));
    assert_eq!(other.syntax_dirty_through, Some(200));
    for redo in [false, false, true, true] {
        clear_dirty(&mut other);
        if redo {
            active.redo();
        } else {
            active.undo();
        }
        other.refresh_document();
        assert!(other.syntax_dirty_from.unwrap() >= 100);
        assert!(other.syntax_dirty_through.unwrap() <= 200);
        assert_eq!(other.rope, active.rope);
        let fresh = Editor::test_with_text(&other.rope.to_string());
        for row in [0, 100, 150, 200, 299] {
            for col in 0..other.line_len(row) {
                assert_eq!(
                    other.cell_at_source_column(row, 0, col),
                    fresh.cell_at_source_column(row, 0, col)
                );
            }
        }
    }
}

#[test]
fn shared_refresh_handles_crlf_boundaries_unicode_and_equal_text_replacements() {
    for (old, new, first) in [
        ("prefix\naa\rbb\ntail", "prefix\naa\r\nbb\ntail", 1),
        ("prefix\naa\r\nbb\ntail", "prefix\naa\rbb\ntail", 1),
        ("prefix\n界é\ntail", "prefix\n界ê\ntail", 1),
        ("prefix\naa\nbb\ntail", "prefix\naa bb\ntail", 1),
        ("prefix\nbody\ntail", "prefix\nbody\n\ntail", 2),
    ] {
        let mut active = Editor::test_with_text(old);
        let mut other = Editor::new();
        other.copy_document_from(&active);
        clear_dirty(&mut other);
        active.save_undo();
        active.rope = ropey::Rope::from_str(new);
        active.modified = true;
        active.publish_document();
        other.refresh_document();
        assert_eq!(other.rope, active.rope);
        assert_eq!(other.syntax_dirty_from, Some(first), "{old:?} -> {new:?}");
        assert!(other.syntax_dirty_through.unwrap() >= first);
        active.undo();
        other.refresh_document();
        assert_eq!(other.rope.to_string(), old);
    }
}

#[test]
fn metadata_refresh_keeps_text_caches_and_saved_checkpoint_changes_invalidate_diff() {
    let mut active = Editor::test_with_text(&"let café = 123;\n".repeat(200));
    active.record_saved_snapshot(active.rope.clone());
    let mut other = Editor::new();
    other.copy_document_from(&active);
    prime_rows(&other, 10..70);
    clear_dirty(&mut other);
    let builds = other.render_line_cache.borrow().builds;
    let folds = other.fold_hint_cache.borrow().builds;
    active.set_read_only(true);
    other.refresh_document();
    assert!(other.read_only);
    assert_eq!(other.syntax_dirty_from, None);
    assert_eq!(other.unsaved_diff_dirty_from, None);
    prime_rows(&other, 10..70);
    assert_eq!(other.render_line_cache.borrow().builds, builds);
    assert_eq!(other.fold_hint_cache.borrow().builds, folds);
    active.set_read_only(false);
    active.insert_char('x');
    other.refresh_document();
    clear_dirty(&mut other);
    active.record_saved_snapshot(active.rope.clone());
    other.refresh_document();
    assert!(!other.modified);
    assert!(other.saved_rope.as_ref().unwrap().is_instance(&active.rope));
    assert_eq!(other.unsaved_diff_dirty_from, Some(0));
}

#[test]
#[ignore = "manual optimized component benchmark"]
fn benchmark_shared_refresh_and_line_commands() {
    use std::{hint::black_box, time::Instant};
    for count in [2_000, 100_000] {
        let text = "fn sample() { let café = [1, 2, 3]; }\n".repeat(count);
        let mut active = Editor::test_with_text(&text);
        let mut other = Editor::new();
        other.copy_document_from(&active);
        prime_rows(&other, 10..70);
        let mut timings = Vec::new();
        for _ in 0..15 {
            active.set_cursor(count / 2, 5);
            active.insert_char('x');
            let before = other.render_line_cache.borrow().builds;
            let started = Instant::now();
            other.refresh_document();
            prime_rows(&other, 10..70);
            timings.push(started.elapsed());
            black_box(other.render_line_cache.borrow().builds - before);
        }
        timings.sort();
        eprintln!(
            "Shared refresh + 60 retained rows, {count} lines: {:?}",
            timings[7]
        );
    }
    for (name, text, operation) in [
        (
            "blank-free",
            "let café = 123;\n".repeat(100_000),
            Editor::remove_blank_lines as fn(&mut Editor),
        ),
        (
            "blank-filter",
            "let café = 123;\n\n".repeat(50_000),
            Editor::remove_blank_lines,
        ),
        (
            "duplicates",
            "let café = 123;\n".repeat(100_000),
            Editor::remove_duplicate_lines,
        ),
        (
            "unique",
            (0..100_000)
                .map(|row| format!("let value_{row} = 123;\n"))
                .collect(),
            Editor::remove_duplicate_lines,
        ),
        (
            "reverse",
            "let café = 123;\nother();\n".repeat(50_000),
            Editor::reverse_lines,
        ),
        (
            "import-free",
            "let café = 123;\n".repeat(100_000),
            Editor::sort_imports,
        ),
        (
            "sorted-imports",
            (0..50_000)
                .map(|row| format!("use item_{row:05};\n"))
                .collect(),
            Editor::sort_imports,
        ),
    ] {
        let mut samples = Vec::new();
        for _ in 0..9 {
            let mut editor = Editor::test_with_text(&text);
            let started = Instant::now();
            operation(&mut editor);
            black_box(&editor.rope);
            samples.push(started.elapsed());
        }
        samples.sort();
        eprintln!("Line command {name}: {:?}", samples[4]);
    }
}
