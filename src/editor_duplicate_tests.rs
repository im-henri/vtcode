use crate::editor::Editor;

type State = ((usize, usize), Option<(usize, usize)>, Vec<(usize, usize)>, Vec<Option<(usize, usize)>>);

fn state(editor: &Editor) -> State {
    ((editor.cursor_line, editor.cursor_col), editor.selection_anchor,
        editor.extra_cursors.clone(), editor.extra_cursor_anchors.clone())
}

fn duplicate_and_roundtrip(editor: &mut Editor, up: bool, expected: &str) {
    let before = editor.rope.clone();
    let before_state = state(editor);
    let selected = editor.selected_text();
    if up { editor.duplicate_line_up(); } else { editor.duplicate_line(); }
    assert_eq!(editor.rope, expected);
    assert_eq!(editor.selected_text(), selected);
    let after_state = state(editor);
    editor.undo();
    assert_eq!(editor.rope, before);
    assert_eq!(state(editor), before_state);
    editor.redo();
    assert_eq!(editor.rope, expected);
    assert_eq!(state(editor), after_state);
}

#[test]
fn duplicate_keeps_disjoint_selection_blocks_and_adjacent_carets_separate() {
    let source = "head\nalpha\nbeta\ngap\ngamma\ndelta\nend";
    let expected = "head\nalpha\nbeta\nalpha\nbeta\ngap\ngamma\ndelta\ngamma\ndelta\nend\nend";
    for (up, cursor, anchor, extras, anchors) in [
        (true, (2, 2), (1, 1), [(6, 2), (10, 1)], [Some((7, 3)), None]),
        (false, (4, 2), (3, 1), [(8, 2), (11, 1)], [Some((9, 3)), None]),
    ] {
        let mut editor = Editor::test_with_text(source);
        (editor.cursor_line, editor.cursor_col) = (2, 2);
        editor.selection_anchor = Some((1, 1));
        editor.extra_cursors = vec![(4, 2), (6, 1)];
        editor.extra_cursor_anchors = vec![Some((5, 3)), None];
        duplicate_and_roundtrip(&mut editor, up, expected);
        assert_eq!((editor.cursor_line, editor.cursor_col), cursor);
        assert_eq!(editor.selection_anchor, Some(anchor));
        assert_eq!(editor.extra_cursors, extras);
        assert_eq!(editor.extra_cursor_anchors, anchors);
    }
}

#[test]
fn duplicate_copies_overlapping_selected_rows_once_and_keeps_every_selection() {
    for up in [false, true] {
        let mut editor = Editor::test_with_text("head\nalpha\nbeta\ngamma\ndelta\nend");
        (editor.cursor_line, editor.cursor_col) = (3, 2);
        editor.selection_anchor = Some((1, 1));
        editor.extra_cursors = vec![(2, 1), (2, 3)];
        editor.extra_cursor_anchors = vec![Some((4, 2)), None];
        duplicate_and_roundtrip(&mut editor, up,
            "head\nalpha\nbeta\ngamma\ndelta\nalpha\nbeta\ngamma\ndelta\nend");
        let offset = if up { 0 } else { 4 };
        assert_eq!((editor.cursor_line, editor.cursor_col), (3 + offset, 2));
        assert_eq!(editor.selection_anchor, Some((1 + offset, 1)));
        assert_eq!(editor.extra_cursors, [(2 + offset, 1), (2 + offset, 3)]);
        assert_eq!(editor.extra_cursor_anchors, [Some((4 + offset, 2)), None]);
    }
}

#[test]
fn duplicate_maps_an_exclusive_selection_end_separately_from_a_caret_on_that_line() {
    for (up, cursor, anchor, extra) in [
        (true, (3, 0), (1, 1), (5, 1)), (false, (5, 0), (3, 1), (6, 1)),
    ] {
        let mut editor = Editor::test_with_text("head\nalpha\nbeta\ngamma\ntail");
        (editor.cursor_line, editor.cursor_col) = (3, 0);
        editor.selection_anchor = Some((1, 1));
        editor.extra_cursors = vec![(3, 1)];
        duplicate_and_roundtrip(&mut editor, up, "head\nalpha\nbeta\nalpha\nbeta\ngamma\ngamma\ntail");
        assert_eq!((editor.cursor_line, editor.cursor_col), cursor);
        assert_eq!(editor.selection_anchor, Some(anchor));
        assert_eq!(editor.extra_cursors, [extra]);
    }
}

#[test]
fn duplicate_all_rows_preserves_blank_lines_eof_and_selection_for_repeated_commands() {
    for (source, twice, three_times) in [
        ("", "\n", "\n\n"), ("\n", "\n\n", "\n\n\n"),
        ("a\nb", "a\nb\na\nb", "a\nb\na\nb\na\nb"),
        ("a\nb\n", "a\nb\na\nb\n", "a\nb\na\nb\na\nb\n"),
        ("a\n\n", "a\n\na\n\n", "a\n\na\n\na\n\n"),
        ("a\r\nb", "a\r\nb\r\na\r\nb", "a\r\nb\r\na\r\nb\r\na\r\nb"),
    ] {
        for up in [false, true] {
            let mut editor = Editor::test_with_text(source);
            editor.detect_line_ending();
            editor.select_all();
            duplicate_and_roundtrip(&mut editor, up, twice);
            duplicate_and_roundtrip(&mut editor, up, three_times);
        }
    }
}
