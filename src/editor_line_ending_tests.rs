use super::*;
use std::hint::black_box;
use std::time::Instant;

#[path = "editor_line_ending_reference.rs"]
mod reference;

fn compare_conversion(source: &str, crlf: bool) {
    let mut actual = Editor::test_with_text(source);
    let mut expected = Editor::test_with_text(source);
    for editor in [&mut actual, &mut expected] {
        editor.cursor_line = editor.line_count() / 2;
        editor.cursor_col = editor.line_len(editor.cursor_line).min(2);
        editor.selection_anchor = Some((0, 0));
        editor.extra_cursors = vec![(0, 0)];
        editor.extra_cursor_anchors = vec![None];
        editor.bookmarks.extend([0, editor.line_count() - 1]);
        editor.syntax_dirty_from = None;
        editor.syntax_dirty_through = None;
    }
    let before = actual.rope.clone();
    let revision = actual.wrap_revision;
    if crlf {
        actual.convert_line_endings_to_crlf();
        expected.reference_line_endings_to_crlf();
    } else {
        actual.convert_line_endings_to_lf();
        expected.reference_line_endings_to_lf();
    }
    assert!(
        actual.rope == expected.rope,
        "CRLF={crlf}, input prefix={:?}",
        source.chars().take(100).collect::<String>()
    );
    assert_eq!(before.to_string(), source);
    assert_eq!(actual.modified, expected.modified);
    assert_eq!(actual.content_version, expected.content_version);
    assert_eq!(actual.syntax_dirty_from, expected.syntax_dirty_from);
    assert_eq!(actual.syntax_dirty_through, expected.syntax_dirty_through);
    assert_eq!(actual.bookmarks, expected.bookmarks);
    assert_eq!(actual.selection_anchor, expected.selection_anchor);
    assert_eq!(
        (actual.cursor_line, actual.cursor_col),
        (expected.cursor_line, expected.cursor_col)
    );
    assert_eq!(actual.extra_cursors, expected.extra_cursors);
    if before.len_lines() > 64
        && before.len_bytes() / before.len_lines() <= 4096
        && before.len_lines() == actual.rope.len_lines()
    {
        assert!(
            actual.wrap_revision - revision <= 2,
            "one invalidation for undo, one for the conversion"
        );
    }
    if actual.modified {
        let converted = actual.rope.clone();
        actual.undo();
        assert_eq!(actual.rope, before);
        assert!(!actual.can_undo());
        actual.redo();
        assert_eq!(actual.rope, converted);
    } else {
        assert!(actual.rope.is_instance(&before));
        assert!(!actual.can_undo());
    }
}

#[test]
fn line_ending_conversion_matches_reference_for_short_mixed_inputs() {
    for code in 0..5usize.pow(5) {
        let mut code = code;
        let source: String = (0..5)
            .map(|_| {
                let ch = ['a', 'é', '😀', '\r', '\n'][code % 5];
                code /= 5;
                ch
            })
            .collect();
        compare_conversion(&source, false);
        compare_conversion(&source, true);
    }
}

#[test]
fn line_ending_conversion_matches_reference_across_rope_chunks() {
    for row in ["α\nβ\r\nγ\r", "\r\r\n", "😀\n", "\r\n\n", "\r", "é"].iter() {
        for prefix in [0, 1, 1023, 2048] {
            let source = format!("{}{}tail", "x".repeat(prefix), row.repeat(1000));
            compare_conversion(&source, false);
            compare_conversion(&source, true);
        }
    }
}

#[test]
fn line_ending_conversion_shares_untouched_text_and_preserves_no_op_history() {
    let source = format!(
        "{}\n{}{}",
        "long α ".repeat(15_000),
        "row\r\n".repeat(1000),
        "long β ".repeat(15_000)
    );
    let rope = Rope::from_str(&source);
    let (converted, first, last) = converted_line_endings(&rope, true, true).unwrap();
    assert_eq!((first, last), (0, 0));
    assert_eq!(converted.to_string(), source.replacen('\n', "\r\n", 1));
    assert_eq!(
        rope.chunks().next().unwrap().as_ptr(),
        converted.chunks().next().unwrap().as_ptr()
    );
    assert_eq!(
        rope.chunks().last().unwrap().as_ptr(),
        converted.chunks().last().unwrap().as_ptr()
    );
    assert!(converted_line_endings(&converted, true, true).is_none());

    let mut editor = Editor::test_with_text(&source.replace("\r\n", "\n"));
    editor.insert_char('x');
    editor.undo();
    let before = editor.rope.clone();
    let version = (editor.content_version, editor.wrap_revision);
    editor.convert_line_endings_to_lf();
    assert!(editor.rope.is_instance(&before));
    assert_eq!((editor.content_version, editor.wrap_revision), version);
    assert!(editor.can_redo());
}

#[test]
#[ignore = "manual performance measurement"]
fn benchmark_line_ending_conversion() {
    for (name, text, crlf) in [
        ("to_crlf", "let café = 42;\n".repeat(100_000), true),
        ("to_lf", "let café = 42;\r\n".repeat(100_000), false),
        ("unchanged", "let café = 42;\r\n".repeat(100_000), true),
        (
            "sparse",
            format!(
                "{}\n{}",
                "long line α ".repeat(80_000),
                "long line β ".repeat(80_000)
            ),
            true,
        ),
    ] {
        let original = Rope::from_str(&text);
        let mut samples = [Vec::new(), Vec::new()];
        for run in 0..9 {
            for reference in if run % 2 == 0 {
                [true, false]
            } else {
                [false, true]
            } {
                let mut editor = Editor::new();
                editor.rope = original.clone();
                let start = Instant::now();
                match (crlf, reference) {
                    (true, true) => editor.reference_line_endings_to_crlf(),
                    (false, true) => editor.reference_line_endings_to_lf(),
                    (true, false) => editor.convert_line_endings_to_crlf(),
                    (false, false) => editor.convert_line_endings_to_lf(),
                }
                black_box(&editor);
                if run >= 2 {
                    samples[usize::from(!reference)].push(start.elapsed());
                }
            }
        }
        for sample in &mut samples {
            sample.sort();
        }
        eprintln!(
            "line_endings_{name} bytes={} before={:?} after={:?}",
            text.len(),
            samples[0][3],
            samples[1][3]
        );
    }
}
