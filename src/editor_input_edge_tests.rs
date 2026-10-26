use super::Editor;

fn warm_columns(editor: &Editor, through: usize) {
    use crate::ui_editor_highlight::{HighlightedLine, PLAIN_STYLE};
    for line in 0..editor.line_count() {
        let text = editor
            .rope
            .line(line)
            .slice(..editor.line_len(line))
            .to_string();
        let view = HighlightedLine::new(None, &text, text.chars().count(), PLAIN_STYLE);
        editor.render_line_cache.borrow_mut().line(
            line,
            (
                editor.content_version,
                editor.wrap_revision,
                editor.tab_size,
            ),
            &editor.rope,
            &view,
            None,
            through,
            false,
        );
    }
}

#[test]
fn cached_columns_match_cold_rope_queries_with_partial_rows_and_clamped_ranges() {
    for source in ["\t abc\tdef\r\nlast", "é 界 é\t 👩‍💻\rnext", "", "plain ASCII\n"] {
        for through in [0, 1, 3, 9, usize::MAX] {
            for tab in [1, 4, 8] {
                let mut cold = Editor::test_with_text(source);
                cold.tab_size = tab;
                let mut warm = Editor::test_with_text(source);
                warm.tab_size = tab;
                warm_columns(&warm, through);
                for line in 0..cold.line_count() + 1 {
                    for start in [0, 2, 10, 1000] {
                        for end in [0, 1, 8, usize::MAX] {
                            assert_eq!(warm.cell_at_source_column(line, start, end),
                                cold.cell_at_source_column(line, start, end));
                            for cell in [0, 1, 5, 20, 1000] {
                                assert_eq!(warm.source_column_at_cell(line, start, end, cell),
                                    cold.source_column_at_cell(line, start, end, cell),
                                    "source={source:?}, through={through}, line={line}, start={start}, end={end}, cell={cell}");
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn vertical_navigation_preserves_screen_column_across_tabs_and_short_lines() {
    for wrapped in [false, true] {
        for through in [0, 1, usize::MAX] {
            let mut editor = Editor::test_with_text("\tabc\n    abc\n\n    abc\n\tabc");
            editor.word_wrap = wrapped;
            editor.viewport_width = 20;
            editor.tab_size = 4;
            editor.cursor_col = 2;
            editor.selection_anchor = Some((0, 2));
            if through != 0 {
                warm_columns(&editor, through);
            }
            for expected in [(1, 5), (2, 0), (3, 5), (4, 2)] {
                editor.move_down();
                assert_eq!(
                    (editor.cursor_line, editor.cursor_col),
                    expected,
                    "wrapped={wrapped}"
                );
            }
            for expected in [(3, 5), (2, 0), (1, 5), (0, 2)] {
                editor.move_up();
                assert_eq!(
                    (editor.cursor_line, editor.cursor_col),
                    expected,
                    "wrapped={wrapped}"
                );
            }
            assert_eq!(editor.selection_anchor, Some((0, 2)));
        }
    }
}

#[test]
fn vertical_navigation_preserves_screen_column_across_wide_and_combining_text() {
    for through in [0, 1, usize::MAX] {
        let mut editor = Editor::test_with_text("abcdef\n界界ab\ne\u{301}abcde\nabcdef");
        editor.cursor_col = 5;
        if through != 0 {
            warm_columns(&editor, through);
        }
        for expected in [(1, 3), (2, 6), (3, 5)] {
            editor.move_down();
            assert_eq!((editor.cursor_line, editor.cursor_col), expected);
        }
    }
}

#[test]
fn page_navigation_in_wrapped_lines_keeps_the_column_within_the_visual_row() {
    let mut editor = Editor::test_with_text("abcdefghijklmnop");
    editor.word_wrap = true;
    editor.viewport_width = 4;
    editor.viewport_height = 4;
    editor.cursor_col = 5;
    editor.page_down();
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 13));
    editor.page_up();
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 5));
}

#[test]
fn soft_tabs_align_to_tab_stops_after_mixed_indentation() {
    for (prefix, spaces) in [("\t", 4), ("  \t", 4), ("\t ", 3), (" \t  ", 2)] {
        let mut editor = Editor::test_with_text(&format!("{prefix}tail"));
        editor.tab_size = 4;
        editor.use_tabs = false;
        editor.cursor_col = prefix.chars().count();
        editor.insert_tab();
        assert_eq!(
            editor.rope.to_string(),
            format!("{prefix}{}tail", " ".repeat(spaces))
        );
        editor.undo();
        assert_eq!(editor.rope.to_string(), format!("{prefix}tail"));
    }
}

#[test]
fn boundary_deletions_preserve_redo_without_creating_empty_undo_steps() {
    for (operation, at_end) in [
        (Editor::backspace as fn(&mut Editor), false),
        (Editor::delete_char, true),
        (Editor::delete_word_left, false),
        (Editor::delete_word_right, true),
    ] {
        for source in ["", "word", "word\r\n"] {
            let mut editor = Editor::test_with_text(source);
            editor.insert_char('x');
            editor.undo();
            if at_end {
                editor.move_to_bottom();
            }
            let version = editor.content_version;
            operation(&mut editor);
            assert_eq!(editor.rope.to_string(), source);
            assert_eq!(
                editor.content_version, version,
                "noop at_end={at_end}, source={source:?}"
            );
            assert!(!editor.can_undo());
            assert!(editor.can_redo());
            editor.redo();
            assert_eq!(editor.rope.to_string(), format!("x{source}"));
        }
    }
}

#[test]
fn word_deletion_across_a_line_ending_creates_one_undo_step() {
    for ending in ["\n", "\r\n"] {
        for backwards in [false, true] {
            let source = format!("first{ending}second");
            let mut editor = Editor::test_with_text(&source);
            if backwards {
                editor.cursor_line = 1;
                editor.delete_word_left();
            } else {
                editor.cursor_col = 5;
                editor.delete_word_right();
            }
            assert_eq!(editor.rope.to_string(), "firstsecond");
            editor.undo();
            assert_eq!(editor.rope.to_string(), source);
            assert!(
                !editor.can_undo(),
                "extra empty undo step: backwards={backwards}, ending={ending:?}"
            );
            editor.redo();
            assert_eq!(editor.rope.to_string(), "firstsecond");
        }
    }
}

#[test]
fn word_deletion_removes_the_selection_without_touching_adjacent_words() {
    for operation in [Editor::delete_word_left, Editor::delete_word_right] {
        for reversed in [false, true] {
            let source = "before αβ\r\nγδ after";
            let mut editor = Editor::test_with_text(source);
            let (anchor, cursor) = if reversed {
                ((1, 2), (0, 7))
            } else {
                ((0, 7), (1, 2))
            };
            editor.selection_anchor = Some(anchor);
            (editor.cursor_line, editor.cursor_col) = cursor;
            operation(&mut editor);
            assert_eq!(editor.rope.to_string(), "before  after");
            assert_eq!((editor.cursor_line, editor.cursor_col), (0, 7));
            assert_eq!(editor.selection_anchor, None);
            editor.undo();
            assert_eq!(editor.rope.to_string(), source);
            assert_eq!(editor.selection_anchor, Some(anchor));
            assert_eq!((editor.cursor_line, editor.cursor_col), cursor);
            assert!(!editor.can_undo());
        }
    }
}

#[test]
fn multicursor_navigation_uses_screen_columns_for_every_caret_and_anchor() {
    for wrapped in [false, true] {
        for select in [false, true] {
            let mut editor = Editor::test_with_text("\tabc\n    abc\n\tabc\n    abc");
            editor.tab_size = 4;
            editor.word_wrap = wrapped;
            editor.viewport_width = 20;
            editor.cursor_col = 2;
            editor.extra_cursors = vec![(2, 2)];
            editor.extra_cursor_anchors = vec![None];
            if select {
                editor.multi_cursor_select_move_down();
            } else {
                editor.multi_cursor_move_down();
            }
            assert_eq!((editor.cursor_line, editor.cursor_col), (1, 5));
            assert_eq!(editor.extra_cursors, [(3, 5)]);
            if select {
                assert_eq!(editor.selection_anchor, Some((0, 2)));
                assert_eq!(editor.extra_cursor_anchors, [Some((2, 2))]);
                editor.multi_cursor_select_move_up();
            } else {
                editor.multi_cursor_move_up();
            }
            assert_eq!((editor.cursor_line, editor.cursor_col), (0, 2));
            assert_eq!(editor.extra_cursors, [(2, 2)]);
        }
    }
}

#[test]
fn multicursor_soft_tabs_respect_hard_tabs_between_carets() {
    let source = " \t tail\n\ttail";
    let mut editor = Editor::test_with_text(source);
    editor.tab_size = 4;
    editor.cursor_col = 1;
    editor.extra_cursors = vec![(0, 3), (1, 1)];
    editor.extra_cursor_anchors = vec![None, None];
    editor.multi_cursor_insert_tab();
    assert_eq!(editor.rope.to_string(), "    \t    tail\n\t    tail");
    editor.undo();
    assert_eq!(editor.rope.to_string(), source);
    assert!(!editor.can_undo());
}

#[test]
fn page_navigation_preserves_screen_column_between_tabbed_and_plain_lines() {
    let mut editor = Editor::test_with_text("\tabc\nshort\n    abc\nshort\n\tabc");
    editor.tab_size = 4;
    editor.viewport_height = 4;
    editor.cursor_col = 2;
    editor.page_down();
    assert_eq!((editor.cursor_line, editor.cursor_col), (2, 5));
    editor.page_down();
    assert_eq!((editor.cursor_line, editor.cursor_col), (4, 2));
    editor.page_up();
    assert_eq!((editor.cursor_line, editor.cursor_col), (2, 5));
    editor.page_up();
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 2));
}

#[test]
fn column_projection_stays_current_after_edits_undo_and_tab_size_changes() {
    let mut editor = Editor::test_with_text("\tabc\n    abc");
    editor.tab_size = 4;
    warm_columns(&editor, usize::MAX);
    assert_eq!(editor.cell_at_source_column(0, 0, 2), 5);
    editor.insert_char(' ');
    assert_eq!(editor.cell_at_source_column(0, 0, 2), 4);
    assert_eq!(editor.source_column_at_cell(0, 0, 5, 4).0, 2);
    editor.undo();
    assert_eq!(editor.cell_at_source_column(0, 0, 2), 5);
    warm_columns(&editor, usize::MAX);
    editor.tab_size = 8;
    assert_eq!(editor.cell_at_source_column(0, 0, 2), 9);
    assert_eq!(editor.source_column_at_cell(0, 0, 4, 9).0, 2);
}

#[test]
fn backspace_with_a_zero_tab_size_removes_a_space_without_panicking() {
    let mut editor = Editor::test_with_text("  value");
    editor.tab_size = 0;
    editor.cursor_col = 2;
    editor.backspace();
    assert_eq!(editor.rope.to_string(), " value");
    editor.undo();
    assert_eq!(editor.rope.to_string(), "  value");
}

#[test]
fn boundary_deletions_still_reveal_the_caret_after_manual_scrolling() {
    for (operation, at_end) in [
        (Editor::backspace as fn(&mut Editor), false),
        (Editor::delete_char, true),
        (Editor::delete_word_left, false),
        (Editor::delete_word_right, true),
    ] {
        let source = "row\n".repeat(20);
        let mut editor = Editor::test_with_text(&source);
        editor.viewport_height = 5;
        editor.viewport_width = 20;
        editor.cursor_line = if at_end { 20 } else { 0 };
        editor.scroll_offset = if at_end { 0 } else { 10 };
        operation(&mut editor);
        assert!(editor.cursor_line >= editor.scroll_offset);
        assert!(editor.cursor_line < editor.scroll_offset + editor.viewport_height);
        assert_eq!(editor.rope.to_string(), source);
        assert_eq!(editor.content_version, 0);
        assert!(!editor.can_undo());
    }
}
