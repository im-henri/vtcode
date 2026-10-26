use super::Editor;

#[test]
fn camel_case_word_navigation_agrees_at_digit_boundaries() {
    for prefix in ["foo2", "界2", "2"] {
        let mut editor = Editor::test_with_text(&format!("{prefix}Bar"));
        let boundary = prefix.chars().count();
        editor.move_word_right();
        assert_eq!(editor.cursor_col, boundary);
        editor.move_word_right();
        assert_eq!(editor.cursor_col, boundary + 3);
        editor.move_word_left();
        assert_eq!(editor.cursor_col, boundary);
        editor.move_word_left();
        assert_eq!(editor.cursor_col, 0);
    }
}

#[test]
fn join_lines_with_an_empty_selection_behaves_like_an_unselected_caret() {
    let mut editor = Editor::test_with_text("one\n  two");
    editor.cursor_col = 1;
    editor.start_selection();
    editor.join_lines();
    assert_eq!(editor.rope.to_string(), "one two");
    editor.undo();
    assert_eq!(editor.rope.to_string(), "one\n  two");
}

fn saved(text: &str) -> Editor {
    let mut editor = Editor::test_with_text(text);
    editor.record_saved_snapshot(editor.rope.clone());
    editor
}

#[test]
fn reverse_occurrence_selection_advances_without_duplicating_the_original() {
    let mut editor = saved("界foo x 界foo y 界foo");
    editor.selection_anchor = Some((0, 4));
    editor.cursor_col = 0;
    editor.select_next_occurrence();
    assert_eq!(editor.selection_range(), Some((0, 7, 0, 11)));
    assert_eq!(editor.extra_cursors, [(0, 0)]);
    editor.select_next_occurrence();
    assert_eq!(editor.selection_range(), Some((0, 14, 0, 18)));
    editor.select_next_occurrence();
    assert_eq!(editor.extra_cursors.len(), 2);
    editor.multi_cursor_insert_char('x');
    assert_eq!(editor.rope.to_string(), "x x x y x");
    editor.undo();
    assert_eq!(editor.rope.to_string(), "界foo x 界foo y 界foo");
}

#[test]
fn skip_occurrence_replaces_only_the_active_selection() {
    for previously_added in [false, true] {
        let mut editor = saved("foo x foo y foo");
        editor.select_next_occurrence();
        if previously_added {
            editor.select_next_occurrence();
        }
        editor.skip_occurrence_select_next();
        let expected_start = if previously_added { 12 } else { 6 };
        assert_eq!(
            editor.selection_range(),
            Some((0, expected_start, 0, expected_start + 3))
        );
        assert_eq!(editor.extra_cursors.len(), usize::from(previously_added));
        if previously_added {
            assert_eq!(editor.extra_cursors, [(0, 3)]);
            assert_eq!(editor.extra_cursor_anchors, [Some((0, 0))]);
        }
    }
}

#[test]
fn shrink_after_replacing_expanded_selection_never_restores_stale_coordinates() {
    let mut editor = saved("one\nother\nlast");
    editor.set_cursor(2, 1);
    editor.select_word_at_cursor();
    editor.expand_selection();
    editor.insert_char_replacing_selection('x');
    assert_eq!(editor.rope.to_string(), "x");
    editor.shrink_selection();
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 1));
    assert!(editor.selection_anchor.is_none());
    editor.insert_char_replacing_selection('y');
    assert_eq!(editor.rope.to_string(), "xy");
}

#[test]
fn shrinking_the_first_expansion_restores_the_original_caret() {
    let mut editor = saved("a\n  hello界 tail");
    editor.set_cursor(1, 4);
    editor.expand_selection();
    assert_eq!(editor.selected_text().as_deref(), Some("hello界"));
    editor.shrink_selection();
    assert!(editor.selection_anchor.is_none());
    assert_eq!((editor.cursor_line, editor.cursor_col), (1, 4));
}

#[test]
fn selection_expansion_skips_closed_siblings_before_the_selection() {
    let mut editor = saved("(first(α) + value)");
    editor.set_cursor(0, 13);
    editor.select_word_at_cursor();
    editor.expand_selection();
    assert_eq!(editor.selected_text().as_deref(), Some("first(α) + value"));
    editor.expand_selection();
    assert_eq!(
        editor.selected_text().as_deref(),
        Some("(first(α) + value)")
    );
}

#[test]
fn case_conversion_clamps_virtual_selection_to_its_source_line() {
    for reverse in [false, true] {
        let mut editor = saved("foo\nKEEP");
        editor.selection_anchor = Some((0, if reverse { 6 } else { 0 }));
        editor.cursor_col = if reverse { 0 } else { 6 };
        editor.transform_selection(str::to_uppercase);
        assert_eq!(editor.rope.to_string(), "FOO\nKEEP");
        editor.undo();
        assert_eq!(editor.rope.to_string(), "foo\nKEEP");
        editor.redo();
        assert_eq!(editor.rope.to_string(), "FOO\nKEEP");
    }
}

#[test]
fn unchanged_transforms_preserve_saved_state_and_redo() {
    for operation in 0..3 {
        let mut editor = saved("hello");
        editor.insert_char('x');
        editor.undo();
        editor.select_all();
        let version = editor.content_version;
        match operation {
            0 => editor.transform_selection(str::to_lowercase),
            1 => {
                editor.url_decode_selection();
            }
            _ => {
                editor.base64_decode_selection();
            }
        }
        assert_eq!(editor.rope.to_string(), "hello");
        assert!(!editor.modified);
        assert_eq!(editor.content_version, version);
        assert!(editor.can_redo());
        editor.redo();
        assert_eq!(editor.rope.to_string(), "xhello");
    }
}

#[test]
fn url_decoding_handles_non_ascii_text_after_a_percent() {
    for text in ["%界", "%a界", "%🙂", "é%界%20tail", "%éx"] {
        let mut editor = saved(text);
        editor.select_all();
        editor.url_decode_selection();
        assert_eq!(editor.rope.to_string(), text.replace("%20", " "));
    }
}

#[test]
fn minifying_json_cannot_leave_selection_endpoints_outside_the_document() {
    let mut editor = saved("{\n  \"value\": 12,\n  \"tail\": 3\n}");
    editor.selection_anchor = Some((3, 1));
    editor.cursor_line = 1;
    editor.cursor_col = 4;
    editor.extra_cursors = vec![(2, 7)];
    editor.extra_cursor_anchors = vec![Some((3, 0))];
    editor.minify_json().unwrap();
    for state in editor.collect_multi_cursors() {
        assert!(state.line < editor.line_count());
        assert!(state.col <= editor.line_len(state.line));
        if let Some((line, col)) = state.anchor {
            assert!(line < editor.line_count());
            assert!(col <= editor.line_len(line));
        }
    }
    editor.undo();
    assert_eq!(editor.extra_cursors, [(2, 7)]);
    assert_eq!(editor.selection_anchor, Some((3, 1)));
}

#[test]
fn horizontal_reveal_accounts_for_tabs_and_wide_unicode() {
    for text in ["\t\t\t\t\t\t\t\tEND", "界界界界界界界界界界END"] {
        let mut editor = saved(text);
        editor.viewport_width = 16;
        editor.viewport_height = 5;
        for col in 0..=editor.line_len(0) {
            editor.set_cursor(0, col);
            let cell = editor.cell_at_source_column(0, editor.horizontal_scroll, col);
            assert!(editor.horizontal_scroll <= col);
            assert!(
                cell < editor.viewport_width,
                "col={col} scroll={} cell={cell}",
                editor.horizontal_scroll
            );
        }
    }
}

#[test]
fn occurrence_selection_never_adds_a_partial_overlap_after_wrapping() {
    let mut editor = saved("aaaa z aaaa");
    editor.selection_anchor = Some((0, 1));
    editor.cursor_col = 3;
    for _ in 0..6 {
        editor.select_next_occurrence();
    }
    let mut spans: Vec<_> = editor
        .collect_multi_cursors()
        .into_iter()
        .filter_map(|cursor| editor.selection_span_for_cursor(cursor))
        .map(|span| (span.start_idx, span.end_idx))
        .collect();
    spans.sort_unstable();
    assert_eq!(spans, [(1, 3), (7, 9), (9, 11)]);
    editor.multi_cursor_insert_char('x');
    assert_eq!(editor.rope.to_string(), "axa z xx");
    editor.undo();
    assert_eq!(editor.rope.to_string(), "aaaa z aaaa");
}

#[test]
fn transformed_multiline_selection_keeps_secondary_carets_on_retained_text() {
    for ending in ["\n", "\r\n", "\r"] {
        let original = format!("α{ending}β{ending}keep界");
        let mut editor = saved(&original);
        editor.selection_anchor = Some((0, 0));
        editor.cursor_line = 1;
        editor.cursor_col = 1;
        editor.extra_cursors = vec![(2, 5)];
        editor.extra_cursor_anchors = vec![Some((2, 4))];
        editor.transform_selection(|_| "replacement".into());
        assert_eq!(
            editor.rope.to_string(),
            format!("replacement{ending}keep界")
        );
        assert_eq!(editor.extra_cursors, [(1, 5)]);
        assert_eq!(editor.extra_cursor_anchors, [Some((1, 4))]);
        editor.multi_cursor_insert_char('x');
        assert_eq!(editor.rope.to_string(), format!("x{ending}keepx"));
        editor.undo();
        editor.undo();
        assert_eq!(editor.rope.to_string(), original);
        assert_eq!(editor.extra_cursors, [(2, 5)]);
        editor.redo();
        editor.redo();
        assert_eq!(editor.rope.to_string(), format!("x{ending}keepx"));
    }
}

#[test]
fn shared_document_changes_invalidate_selection_expansion_history() {
    let mut editor = saved("one\ntwo\nlast");
    editor.set_cursor(2, 1);
    editor.select_word_at_cursor();
    editor.expand_selection();
    let mut other = Editor::new();
    other.copy_document_from(&editor);
    other.select_all();
    other.insert_char_replacing_selection('x');
    editor.refresh_document();
    let position = (
        editor.cursor_line,
        editor.cursor_col,
        editor.selection_anchor,
    );
    editor.shrink_selection();
    assert_eq!(
        (
            editor.cursor_line,
            editor.cursor_col,
            editor.selection_anchor
        ),
        position
    );
    assert!(editor.selection_history.is_empty());
    editor.clear_selection();
    editor.insert_char_replacing_selection('y');
    assert_eq!(editor.rope.to_string(), "xy");
}

#[test]
fn selection_transformations_use_exact_ranges_for_crlf_unicode_and_virtual_columns() {
    for ending in ["\n", "\r\n", "\r", ""] {
        for transform in 0..3 {
            for reverse in [false, true] {
                let original = format!("é界{ending}tail");
                let mut editor = saved(&original);
                let actual_len = editor.line_len(0);
                let far = actual_len + 15;
                editor.selection_anchor = Some((0, if reverse { far } else { 0 }));
                editor.cursor_col = if reverse { 0 } else { far };
                let selected = editor.selected_text().unwrap();
                match transform {
                    0 => editor.transform_selection(|s| format!("<{s}>")),
                    1 => {
                        editor.surround_selection("<", ">");
                    }
                    _ => {
                        editor.base64_encode_selection();
                    }
                }
                let after = editor.rope.to_string();
                if transform < 2 {
                    let suffix = if ending.is_empty() {
                        String::new()
                    } else {
                        format!("{ending}tail")
                    };
                    assert_eq!(after, format!("<{selected}>{suffix}"));
                }
                editor.undo();
                assert_eq!(editor.rope.to_string(), original);
                assert!(!editor.modified);
                editor.redo();
                assert_eq!(editor.rope.to_string(), after);
            }
        }
    }
}

#[test]
fn unchanged_json_commands_preserve_redo_and_selection() {
    for command in [
        Editor::format_json,
        Editor::minify_json,
        Editor::sort_json_keys,
    ] {
        let mut editor = saved("{\"a\":1}");
        command(&mut editor).unwrap();
        let original = editor.rope.to_string();
        editor.record_saved_snapshot(editor.rope.clone());
        editor.insert_char('x');
        editor.undo();
        editor.select_all();
        let selection = editor.selection_range();
        let version = editor.content_version;
        command(&mut editor).unwrap();
        assert_eq!(editor.rope.to_string(), original);
        assert!(!editor.modified);
        assert_eq!(editor.selection_range(), selection);
        assert_eq!(editor.content_version, version);
        assert!(editor.can_redo());
    }
}
