use super::*;

#[test]
fn prepared_formatting_swaps_the_worker_snapshot_and_maps_the_latest_carets() {
    let mut editor = Editor::test_with_text("a=1\n# caret 😀\nc=3\n");
    let original = editor.rope.clone();
    let prepared = prepare(&original, "a = 1\n# caret 😀\nc = 3\n");
    let worker_snapshot = prepared.rope.clone();
    // These movements happen after the worker has prepared its result.
    editor.set_cursor(1, 9);
    editor.extra_cursors = vec![(0, 3), (2, 3)];
    editor.extra_cursor_anchors = vec![Some((0, 0)), Some((2, 0))];
    assert_eq!(prepared.apply(&mut editor), Ok(2));
    assert!(editor.rope.is_instance(&worker_snapshot));
    assert!(editor.undo_stack.back().unwrap().rope.is_instance(&original));
    assert_eq!((editor.cursor_line, editor.cursor_col), (1, 9));
    assert_eq!(editor.extra_cursors, vec![(0, 5), (2, 5)]);
    assert!(editor.extra_cursor_anchors.is_empty());
    editor.undo();
    assert!(editor.rope.is_instance(&original));
    assert_eq!(editor.extra_cursors, vec![(0, 3), (2, 3)]);
    assert_eq!(editor.extra_cursor_anchors, vec![Some((0, 0)), Some((2, 0))]);
    editor.redo();
    assert!(editor.rope.is_instance(&worker_snapshot));
}

#[test]
fn prepared_formatting_cursor_boundaries_cover_insertion_replacement_and_deletion() {
    for (before, after, positions) in [
        ("ab", "a😀b", vec![(0, 0), (1, 2), (2, 3)]),
        ("aXYZb", "a😀b", vec![(0, 0), (1, 1), (2, 2), (4, 2), (5, 3)]),
        ("aXYZb", "ab", vec![(0, 0), (1, 1), (2, 1), (4, 1), (5, 2)]),
        ("x=1\n# keep\ny=2\n", "x = 1\n# keep\ny = 2\n", vec![(0, 0), (3, 5), (4, 6), (11, 13), (14, 18), (15, 19)]),
    ] {
        let source = Rope::from_str(before);
        let prepared = prepare(&source, after);
        for (old, new) in positions {
            let line = prepared.rope.char_to_line(new);
            assert_eq!(prepared.map_cursor(old), (line, new - prepared.rope.line_to_char(line)), "{before:?}, {old}");
        }
    }
}

#[test]
fn prepared_formatting_is_separate_from_typing_before_and_after_it() {
    let mut editor = Editor::test_with_text("value=1\n");
    editor.set_cursor(0, 7);
    editor.insert_char('0');
    prepare(&editor.rope, "value = 10\n").apply(&mut editor).unwrap();
    editor.insert_char('1');
    assert_eq!(editor.rope.to_string(), "value = 101\n");
    for text in ["value = 10\n", "value=10\n", "value=1\n"] {
        editor.undo();
        assert_eq!(editor.rope.to_string(), text);
    }
    for text in ["value=10\n", "value = 10\n", "value = 101\n"] {
        editor.redo();
        assert_eq!(editor.rope.to_string(), text);
    }
}

#[test]
fn prepared_formatting_preserves_unchanged_state_and_rejects_unwritable_targets() {
    let mut editor = Editor::test_with_text("é😀\r\nend");
    editor.set_cursor(1, 3);
    editor.selection_anchor = Some((0, 0));
    let version = editor.content_version;
    let original = editor.rope.clone();
    assert_eq!(prepare(&editor.rope, "é😀\r\nend").apply(&mut editor), Ok(0));
    assert!(editor.rope.is_instance(&original));
    assert_eq!(editor.content_version, version);
    assert!(editor.undo_stack.is_empty());
    assert_eq!(editor.selection_anchor, Some((0, 0)));
    for loading in [false, true] {
        editor.read_only = !loading;
        editor.bg_loading = loading;
        assert!(prepare(&editor.rope, "changed").apply(&mut editor).is_err());
        assert!(editor.rope.is_instance(&original));
    }
}

#[test]
fn prepared_formatting_round_trips_mixed_separators_and_unicode_fragments() {
    let fragments = ["", "\n", "\r", "\r\n", "é😀", "e\u{301}\r\n", "\t x\n", "a\nb\r\nc"];
    for first in fragments {
        for second in fragments {
            let before = format!("{first}{second}");
            for after in [format!("{second}{first}"), format!("head\r\n{second}end") ] {
                let mut editor = Editor::test_with_text(&before);
                prepare(&editor.rope, &after).apply(&mut editor).unwrap();
                assert_eq!(editor.rope.to_string(), after, "{before:?}");
                if before != after {
                    editor.undo();
                    assert_eq!(editor.rope.to_string(), before);
                }
            }
        }
    }
}

#[test]
fn fully_changed_large_regions_preserve_the_surrounding_text_and_undo() {
    let mut before = "# unchanged header 😀\r\n".to_string();
    let mut after = before.clone();
    for index in 0..10_000 {
        before.push_str(&format!("value_{index}={index}\r\n"));
        after.push_str(&format!("value_{index} = {index}\r\n"));
    }
    before.push_str("# unchanged footer 😀\r\n");
    after.push_str("# unchanged footer 😀\r\n");
    let mut editor = Editor::test_with_text(&before);
    editor.set_cursor(10_001, 10);
    let prepared = prepare(&editor.rope, &after);
    assert_eq!(prepared.len(), 1);
    prepared.apply(&mut editor).unwrap();
    assert_eq!(editor.rope.to_string(), after);
    assert_eq!((editor.cursor_line, editor.cursor_col), (10_001, 10));
    editor.undo();
    assert_eq!(editor.rope.to_string(), before);
}

#[test]
#[ignore = "formatter diff/preparation performance comparison"]
fn benchmark_formatter_preparation() {
    for lines in [1_000, 10_000, 100_000] {
        let before: String = (0..lines).map(|i| format!("value_{i}={i}\n")).collect();
        let after: String = (0..lines).map(|i| format!("value_{i} = {i}\n")).collect();
        let mut editor = Editor::test_with_text(&before);
        let start = std::time::Instant::now();
        let baseline = similar::TextDiff::configure().timeout(Duration::from_millis(200))
            .diff_lines(before.as_str(), after.as_str());
        std::hint::black_box(&baseline);
        let old_diff = start.elapsed();
        let start = std::time::Instant::now();
        let prepared = prepare(&editor.rope, &after);
        let preparation = start.elapsed();
        let start = std::time::Instant::now();
        prepared.apply(&mut editor).unwrap();
        let apply = start.elapsed();
        assert_eq!(editor.rope.to_string(), after);
        eprintln!("{lines} lines: old diff {old_diff:?}; worker preparation {preparation:?}; UI apply {apply:?}");
    }
}
