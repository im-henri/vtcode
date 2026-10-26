use super::Editor;

fn editor(text: &str) -> Editor {
    let mut editor = Editor::test_with_text(text);
    editor.detect_line_ending();
    editor
}

fn assert_valid_positions(editor: &Editor) {
    for (line, col) in std::iter::once((editor.cursor_line, editor.cursor_col))
        .chain(editor.selection_anchor)
        .chain(editor.extra_cursors.iter().copied())
        .chain(editor.extra_cursor_anchors.iter().copied().flatten())
    {
        assert!(line < editor.line_count(), "line {line} outside document");
        assert!(
            col <= editor.line_len(line),
            "column {col} outside line {line}"
        );
    }
}

fn assert_undo_redo(editor: &mut Editor, before: &str, after: &str) {
    assert_eq!(editor.rope.to_string(), after);
    assert_valid_positions(editor);
    editor.undo();
    assert_eq!(editor.rope.to_string(), before);
    assert_valid_positions(editor);
    editor.redo();
    assert_eq!(editor.rope.to_string(), after);
    assert_valid_positions(editor);
}

#[test]
fn edge_case_reverse_preserves_final_newlines_and_separator_styles() {
    for ending in ["\n", "\r\n", "\r"] {
        for trailing in ["", ending] {
            let before = format!("α{ending}ββ{ending}界🙂{trailing}");
            let after = format!("界🙂{ending}ββ{ending}α{trailing}");
            let mut editor = editor(&before);
            editor.reverse_lines();
            assert_undo_redo(&mut editor, &before, &after);
        }
    }
    let before = "one\r\ntwo\nthree\r";
    let mut editor = editor(before);
    editor.reverse_lines();
    assert_undo_redo(&mut editor, before, "three\r\ntwo\none\r");
}

#[test]
fn edge_case_reversing_selected_lines_never_joins_the_unselected_suffix() {
    for ending in ["\n", "\r\n", "\r"] {
        for reverse_selection in [false, true] {
            let before = format!("keep{ending}one{ending}two{ending}tail");
            let mut editor = editor(&before);
            let (anchor, cursor) = if reverse_selection { ((3, 0), (1, 0)) } else { ((1, 0), (3, 0)) };
            editor.selection_anchor = Some(anchor);
            (editor.cursor_line, editor.cursor_col) = cursor;
            editor.reverse_lines();
            assert_undo_redo(&mut editor, &before, &format!("keep{ending}two{ending}one{ending}tail"));
        }
    }
}

#[test]
fn edge_case_shuffling_selected_lines_preserves_their_boundaries() {
    for ending in ["\n", "\r\n", "\r"] {
        let before = format!("keep{ending}one{ending}two{ending}three{ending}tail");
        let mut editor = editor(&before);
        editor.selection_anchor = Some((1, 0));
        editor.cursor_line = 4;
        editor.shuffle_lines();
        let after = editor.rope.to_string();
        let middle = after.strip_prefix(&format!("keep{ending}")).unwrap()
            .strip_suffix(&format!("{ending}tail")).expect("unselected suffix stays separated");
        let mut lines: Vec<_> = middle.split(ending).collect();
        lines.sort_unstable();
        assert_eq!(lines, ["one", "three", "two"]);
        assert_valid_positions(&editor);
        if after != before { assert_undo_redo(&mut editor, &before, &after); }
    }
}

#[test]
fn edge_case_removing_duplicate_lines_preserves_eof_and_valid_carets() {
    for ending in ["\n", "\r\n", "\r"] {
        for trailing in ["", ending] {
            let before = format!("α{ending}ββ{ending}α{ending}ββ{trailing}");
            let after = format!("α{ending}ββ{trailing}");
            let mut editor = editor(&before);
            editor.cursor_line = 3;
            editor.cursor_col = 2;
            editor.extra_cursors = vec![(2, 1)];
            editor.extra_cursor_anchors = vec![Some((3, 2))];
            editor.remove_duplicate_lines();
            assert_undo_redo(&mut editor, &before, &after);
            editor.insert_char('!');
            assert_valid_positions(&editor);
        }
    }
}

#[test]
fn edge_case_removing_blank_lines_preserves_mixed_separators_and_carets() {
    let before = "α\r\n \t\r\nββ\n\n界🙂";
    let mut editor = editor(before);
    editor.cursor_line = 4;
    editor.cursor_col = 2;
    editor.selection_anchor = Some((3, 0));
    editor.extra_cursors = vec![(2, 2)];
    editor.extra_cursor_anchors = vec![Some((4, 2))];
    editor.remove_blank_lines();
    assert_undo_redo(&mut editor, before, "α\r\nββ\n界🙂");
}

#[test]
fn edge_case_noop_line_commands_preserve_redo_and_clean_state() {
    for operation in [
        Editor::reverse_lines, Editor::shuffle_lines, Editor::remove_duplicate_lines,
        Editor::remove_blank_lines, |editor: &mut Editor| { editor.trim_trailing_whitespace(); },
    ] {
        let mut editor = editor("word");
        editor.insert_char('x');
        editor.undo();
        let version = editor.content_version;
        let undo_count = editor.undo_stack.len();
        let modified = editor.modified;
        operation(&mut editor);
        assert_eq!(editor.content_version, version);
        assert_eq!(editor.undo_stack.len(), undo_count);
        assert_eq!(editor.modified, modified);
        editor.redo();
        assert_eq!(editor.rope.to_string(), "xword");
    }
}

#[test]
fn edge_case_trimming_keeps_all_caret_endpoints_within_their_lines() {
    let before = "α   \r\nββ\t\r\n界🙂  ";
    let mut editor = editor(before);
    editor.cursor_col = 4;
    editor.selection_anchor = Some((1, 3));
    editor.extra_cursors = vec![(2, 4)];
    editor.extra_cursor_anchors = vec![Some((0, 3))];
    editor.trim_trailing_whitespace();
    assert_undo_redo(&mut editor, before, "α\r\nββ\r\n界🙂");
}

#[test]
fn edge_case_trim_on_save_clamps_secondary_carets_and_selection_anchors() {
    let mut editor = editor("α   \r\nββ\t\r\n界🙂  ");
    editor.cursor_col = 4;
    editor.selection_anchor = Some((1, 3));
    editor.extra_cursors = vec![(2, 4)];
    editor.extra_cursor_anchors = vec![Some((0, 3))];
    editor.prepare_save_content(true, true);
    assert_eq!(editor.rope.to_string(), "α\r\nββ\r\n界🙂\r\n");
    assert_valid_positions(&editor);
    assert_eq!(editor.extra_cursors, [(2, 2)]);
    assert_eq!(editor.extra_cursor_anchors, [Some((0, 1))]);
}

#[test]
fn edge_case_line_transformations_keep_carets_attached_to_surviving_text() {
    let mut editor = editor("one\ntwo\nthree\nfour");
    editor.cursor_line = 1;
    editor.cursor_col = 2;
    editor.extra_cursors = vec![(3, 3)];
    editor.extra_cursor_anchors = vec![Some((2, 1))];
    editor.bookmarks.extend([0, 3]);
    editor.reverse_lines();
    assert_eq!((editor.cursor_line, editor.cursor_col), (2, 2));
    assert_eq!(editor.extra_cursors, [(0, 3)]);
    assert_eq!(editor.extra_cursor_anchors, [Some((1, 1))]);
    assert_eq!(editor.bookmarks.iter().copied().collect::<Vec<_>>(), [0, 3]);
    assert_undo_redo(&mut editor, "one\ntwo\nthree\nfour", "four\nthree\ntwo\none");

    let mut editor = super::Editor::test_with_text("keep\none\none\ntwo\ntail");
    editor.selection_anchor = Some((1, 0));
    editor.cursor_line = 4;
    editor.extra_cursors = vec![(3, 2)];
    editor.extra_cursor_anchors = vec![Some((4, 3))];
    editor.bookmarks.extend([2, 3, 4]);
    editor.remove_duplicate_lines();
    assert_eq!(editor.rope.to_string(), "keep\none\ntwo\ntail");
    assert_eq!((editor.cursor_line, editor.cursor_col), (3, 0));
    assert_eq!(editor.selection_anchor, Some((1, 0)));
    assert_eq!(editor.extra_cursors, [(2, 2)]);
    assert_eq!(editor.extra_cursor_anchors, [Some((3, 3))]);
    assert_eq!(editor.bookmarks.iter().copied().collect::<Vec<_>>(), [2, 3]);
    assert_valid_positions(&editor);
}

#[test]
fn edge_case_line_commands_handle_empty_files_and_preserve_lone_cr_endings() {
    for operation in [
        Editor::reverse_lines, Editor::shuffle_lines, Editor::remove_duplicate_lines,
        Editor::remove_blank_lines, |editor: &mut Editor| { editor.trim_trailing_whitespace(); },
    ] {
        for before in ["", "\r", "\r\n", "\n"] {
            let mut editor = editor(before);
            let version = editor.content_version;
            operation(&mut editor);
            assert_eq!(editor.rope.to_string(), before);
            assert_eq!(editor.content_version, version);
            assert_valid_positions(&editor);
        }
    }
    let mut editor = editor("first  \rsecond\t\rlast ");
    editor.trim_trailing_whitespace();
    assert_undo_redo(&mut editor, "first  \rsecond\t\rlast ", "first\rsecond\rlast");
}

fn check_encoded_save_preserves_carriage_returns(encoding: &'static str) {
    let dir = tempfile::tempdir().unwrap();
    // Include runs spanning Rope chunks, a CR before text, existing CRLF,
    // a lone LF to convert, and trailing CRs with no following character.
    let prefix = "\r".repeat(2500);
    let source = format!("{prefix}alpha\r\rbravo\r\r\ncharlie\nlast\r\r");
    for line_ending in ["LF", "CRLF"] {
        let expected = if line_ending == "CRLF" {
            format!("{prefix}alpha\r\rbravo\r\r\ncharlie\r\nlast\r\r")
        } else {
            source.clone()
        };
        let expected_bytes = match encoding {
            "UTF-16 LE" => [vec![0xff, 0xfe], expected.encode_utf16().flat_map(u16::to_le_bytes).collect()].concat(),
            "UTF-16 BE" => [vec![0xfe, 0xff], expected.encode_utf16().flat_map(u16::to_be_bytes).collect()].concat(),
            _ => expected.into_bytes(),
        };
        for background in [false, true] {
            let path = dir.path().join(format!("{line_ending}-{background}.txt"));
            let mut editor = editor(&source);
            assert!(editor.rope.chunks().count() > 1);
            editor.encoding = encoding;
            editor.cached_line_ending = line_ending;
            editor.file_path = Some(path.clone());
            if background {
                super::write_background_save(&super::BackgroundSaveRequest {
                    prepared_options: None,
                    source_document_id: editor.navigation_id,
                    path: path.clone(),
                    rope: editor.rope.clone(),
                    encoding,
                    line_ending,
                    trim_trailing: false,
                    insert_final_newline: false,
                    source_content_version: editor.content_version,
                    fail_if_exists: false,
                    expected_disk: None,
                }).unwrap();
            } else {
                editor.save_file_with_options(false, false).unwrap();
            }
            assert_eq!(std::fs::read(&path).unwrap(), expected_bytes,
                "{encoding}, {line_ending}, background={background}");
        }
    }
}

#[test]
fn utf16_le_save_preserves_repeated_carriage_returns() {
    check_encoded_save_preserves_carriage_returns("UTF-16 LE");
}

#[test]
fn utf16_be_save_preserves_repeated_carriage_returns() {
    check_encoded_save_preserves_carriage_returns("UTF-16 BE");
}

#[test]
fn latin1_save_preserves_repeated_carriage_returns() {
    check_encoded_save_preserves_carriage_returns("ISO-8859-1");
}

#[test]
fn ascii_save_preserves_repeated_carriage_returns() {
    check_encoded_save_preserves_carriage_returns("ASCII");
}

#[test]
fn utf8_save_preserves_repeated_carriage_returns() {
    check_encoded_save_preserves_carriage_returns("UTF-8");
}

#[test]
fn sort_imports_preserves_line_separators_and_final_newline() {
    for ending in ["\n", "\r\n", "\r"] {
        for final_ending in ["", ending] {
            let before = format!("use z;{ending}use a;{ending}{ending}use y;{ending}use b;{final_ending}");
            let after = format!("use a;{ending}use z;{ending}{ending}use b;{ending}use y;{final_ending}");
            let mut editor = editor(&before);
            editor.sort_imports();
            assert_undo_redo(&mut editor, &before, &after);
        }
    }
}

#[test]
fn sort_imports_preserves_mixed_line_separators() {
    let before = "use z;\r\nuse a;\nuse b;\r\nbody();";
    let mut editor = editor(before);
    editor.sort_imports();
    assert_undo_redo(&mut editor, before, "use a;\r\nuse b;\nuse z;\r\nbody();");
}

#[test]
fn sort_imports_keeps_all_caret_positions_in_bounds() {
    let before = "use z_界界界界;\nuse a;\n\nuse z_長長長長;\nuse b;";
    let mut editor = editor(before);
    editor.cursor_col = editor.line_len(0);
    editor.selection_anchor = Some((0, editor.cursor_col - 1));
    editor.extra_cursors = vec![(3, editor.line_len(3))];
    editor.extra_cursor_anchors = vec![Some((3, editor.line_len(3) - 1))];
    editor.sort_imports();
    assert_valid_positions(&editor);
    assert_undo_redo(&mut editor, before,
        "use a;\nuse z_界界界界;\n\nuse b;\nuse z_長長長長;");
    editor.insert_char('x');
    assert_valid_positions(&editor);
}

#[test]
fn sort_imports_noop_preserves_redo_and_document_version() {
    let mut editor = editor("use a;\nuse b;");
    editor.insert_char('x');
    editor.undo();
    let version = editor.content_version;
    let undo_count = editor.undo_stack.len();
    editor.sort_imports();
    assert_eq!(editor.content_version, version);
    assert_eq!(editor.undo_stack.len(), undo_count);
    editor.redo();
    assert_eq!(editor.rope.to_string(), "xuse a;\nuse b;");
}

#[test]
fn duplicate_last_line_preserves_the_document_line_ending() {
    for ending in ["\n", "\r\n"] {
        for operation in [Editor::duplicate_line, Editor::duplicate_line_up] {
            for last in ["界🙂", ""] {
                let before = format!("head{ending}{last}");
                let mut editor = editor(&before);
                editor.cursor_line = 1;
                editor.cursor_col = last.chars().count();
                operation(&mut editor);
                assert_undo_redo(
                    &mut editor,
                    &before,
                    &format!("head{ending}{last}{ending}{last}"),
                );
            }
        }
    }
}

#[test]
fn delete_last_line_removes_the_entire_crlf_separator() {
    for last in ["界🙂", ""] {
        let before = format!("head\r\n{last}");
        let mut editor = editor(&before);
        editor.cursor_line = 1;
        editor.delete_line();
        assert_undo_redo(&mut editor, &before, "head");
    }
}

#[test]
fn move_final_line_up_preserves_crlf_without_a_final_newline() {
    let before = "head\r\n界🙂";
    let mut editor = editor(before);
    editor.cursor_line = 1;
    editor.cursor_col = 2;
    editor.move_line_up();
    assert_undo_redo(&mut editor, before, "界🙂\r\nhead");
}

#[test]
fn move_line_down_to_eof_preserves_crlf() {
    let before = "head\r\n界🙂";
    let mut editor = editor(before);
    editor.cursor_col = 4;
    editor.move_line_down();
    assert_undo_redo(&mut editor, before, "界🙂\r\nhead");
}

#[test]
fn move_block_to_eof_preserves_selected_blank_lines() {
    for ending in ["\n", "\r\n"] {
        let before = format!("α{ending}{ending}tail");
        let mut editor = editor(&before);
        editor.selection_anchor = Some((0, 0));
        editor.cursor_line = 2;
        editor.move_line_down();
        assert_undo_redo(&mut editor, &before, &format!("tail{ending}α{ending}"));
    }
}

#[test]
fn move_up_keeps_reverse_whole_line_selection_on_the_moved_block() {
    let before = "head\nα\nbb\ntail\nlast";
    let mut editor = editor(before);
    editor.cursor_line = 1;
    editor.selection_anchor = Some((3, 0));
    editor.move_line_up();
    assert_eq!(editor.selected_text().as_deref(), Some("α\nbb\n"));
    assert_eq!(editor.selection_anchor, Some((2, 0)));
    assert_undo_redo(&mut editor, before, "α\nbb\nhead\ntail\nlast");
}

#[test]
fn move_down_keeps_reverse_whole_line_selection_on_the_moved_block() {
    let before = "head\nα\nbb\ntail\nlast";
    let mut editor = editor(before);
    editor.cursor_line = 1;
    editor.selection_anchor = Some((3, 0));
    editor.move_line_down();
    assert_eq!(editor.selected_text().as_deref(), Some("α\nbb\n"));
    assert_eq!(editor.selection_anchor, Some((4, 0)));
    assert_undo_redo(&mut editor, before, "head\ntail\nα\nbb\nlast");
}

#[test]
fn join_ending_on_a_blank_line_does_not_join_the_unselected_next_line() {
    for ending in ["\n", "\r\n"] {
        for blank in ["", " \t"] {
            let before = format!("α{ending}{blank}{ending}outside");
            let mut editor = editor(&before);
            editor.join_lines();
            assert_undo_redo(&mut editor, &before, &format!("α{ending}outside"));
        }
    }
}

#[test]
fn join_empty_lines_preserves_the_unselected_next_line() {
    let before = "\r\n \t\r\noutside";
    let mut editor = editor(before);
    editor.join_lines();
    assert_undo_redo(&mut editor, before, "\r\noutside");
}

#[test]
fn incomplete_overlapping_block_comment_delimiters_do_not_panic() {
    for (path, before, after) in [
        ("main.c", "/*/", "/* /*/ */"),
        ("page.html", "<!-->", "<!-- <!--> -->"),
    ] {
        let mut editor = editor(before);
        editor.file_path = Some(path.into());
        editor.selection_anchor = Some((0, 0));
        editor.cursor_col = before.chars().count();
        editor.toggle_block_comment();
        assert_undo_redo(&mut editor, before, after);
    }
}

#[test]
fn uncomment_block_keeps_caret_valid_when_the_document_shrinks() {
    for before in ["/**/", "/* 界🙂 */", "/*\n  界🙂\n*/"] {
        let mut editor = editor(before);
        editor.file_path = Some("main.c".into());
        editor.selection_anchor = Some((0, 0));
        editor.cursor_line = editor.line_count() - 1;
        editor.cursor_col = editor.line_len(editor.cursor_line);
        let expected = if before == "/**/" { "" } else { "界🙂" };
        editor.toggle_block_comment();
        assert_undo_redo(&mut editor, before, expected);
        editor.insert_char('x');
        assert_eq!(editor.rope.to_string(), format!("{expected}x"));
    }
}

#[test]
fn removing_paired_line_comment_strips_only_one_pair() {
    for operation in [Editor::toggle_line_comment, Editor::remove_line_comment] {
        let before = "/*/* inner */*/";
        let mut editor = editor(before);
        editor.file_path = Some("style.css".into());
        operation(&mut editor);
        assert_undo_redo(&mut editor, before, "/* inner */");
    }
}

#[test]
fn comment_prefix_edits_do_not_move_carets_before_the_prefix() {
    for operation in [Editor::toggle_line_comment, Editor::add_line_comment] {
        let before = "    界🙂";
        let mut editor = editor(before);
        editor.file_path = Some("main.c".into());
        editor.cursor_col = 1;
        editor.extra_cursors = vec![(0, 2), (0, 6)];
        editor.extra_cursor_anchors = vec![Some((0, 0)), Some((0, 5))];
        operation(&mut editor);
        assert_eq!(editor.cursor_col, 1);
        assert_eq!(editor.extra_cursors, [(0, 2), (0, 9)]);
        assert_eq!(editor.extra_cursor_anchors, [Some((0, 0)), Some((0, 8))]);
        assert_undo_redo(&mut editor, before, "    // 界🙂");
    }
}

#[test]
fn removing_comment_prefix_maps_carets_inside_and_before_it() {
    for operation in [Editor::toggle_line_comment, Editor::remove_line_comment] {
        let before = "    // 界🙂";
        let mut editor = editor(before);
        editor.file_path = Some("main.c".into());
        editor.cursor_col = 1;
        editor.extra_cursors = vec![(0, 5), (0, 9)];
        editor.extra_cursor_anchors = vec![Some((0, 2)), Some((0, 8))];
        operation(&mut editor);
        assert_eq!(editor.cursor_col, 1);
        assert_eq!(editor.extra_cursors, [(0, 4), (0, 6)]);
        assert_eq!(editor.extra_cursor_anchors, [Some((0, 2)), Some((0, 5))]);
        assert_undo_redo(&mut editor, before, "    界🙂");
    }
}

#[test]
fn move_whole_line_selection_to_eof_keeps_its_content_selected() {
    for reverse in [false, true] {
        let mut editor = editor("α\r\ntail");
        let (cursor, anchor) = if reverse {
            ((0, 0), (1, 0))
        } else {
            ((1, 0), (0, 0))
        };
        (editor.cursor_line, editor.cursor_col) = cursor;
        editor.selection_anchor = Some(anchor);
        editor.move_line_down();
        assert_eq!(editor.selected_text().as_deref(), Some("α"));
        assert_undo_redo(&mut editor, "α\r\ntail", "tail\r\nα");
    }
}

#[test]
fn explicit_comment_commands_exclude_the_zero_column_end_line() {
    for (path, commented) in [
        ("main.c", "// one\noutside"),
        ("style.css", "/* one */\noutside"),
    ] {
        let mut editor = editor("one\noutside");
        editor.file_path = Some(path.into());
        editor.selection_anchor = Some((0, 0));
        editor.cursor_line = 1;
        editor.add_line_comment();
        assert_eq!(editor.rope.to_string(), commented);
        editor.remove_line_comment();
        assert_eq!(editor.rope.to_string(), "one\noutside");
        assert_valid_positions(&editor);
    }
}

#[test]
fn uncomment_multiline_block_remaps_secondary_cursors_and_selections() {
    let before = "before\n/*\n  界🙂\n*/\nafter";
    let mut editor = editor(before);
    editor.file_path = Some("main.c".into());
    editor.selection_anchor = Some((1, 0));
    editor.cursor_line = 3;
    editor.cursor_col = 2;
    editor.extra_cursors = vec![(0, 2), (2, 3), (4, 3)];
    editor.extra_cursor_anchors = vec![None, Some((2, 2)), Some((4, 0))];
    editor.toggle_block_comment();
    assert_eq!(editor.extra_cursors, [(0, 2), (1, 1), (2, 3)]);
    assert_eq!(
        editor.extra_cursor_anchors,
        [None, Some((1, 0)), Some((2, 0))]
    );
    assert_undo_redo(&mut editor, before, "before\n界🙂\nafter");
}

#[test]
fn paired_comment_toggle_round_trips_blank_lines_and_outer_whitespace() {
    for path in ["style.css", "page.html"] {
        let before = "\u{2003}first  \r\n \t\r\n  second\t\r\noutside";
        let mut editor = editor(before);
        editor.file_path = Some(path.into());
        editor.selection_anchor = Some((0, 0));
        editor.cursor_line = 3;
        editor.toggle_line_comment();
        assert_valid_positions(&editor);
        editor.toggle_line_comment();
        assert_eq!(editor.rope.to_string(), before);
        assert_valid_positions(&editor);
    }
}

#[test]
fn paired_comment_removal_maps_carets_in_delimiters_and_trailing_whitespace() {
    let before = "  /* 界🙂 */  \r\noutside";
    for operation in [Editor::toggle_line_comment, Editor::remove_line_comment] {
        let mut editor = editor(before);
        editor.file_path = Some("style.css".into());
        editor.cursor_col = 12;
        editor.extra_cursors = vec![(0, 1), (0, 4), (0, 6), (0, 9)];
        editor.extra_cursor_anchors = vec![None, None, Some((0, 5)), None];
        operation(&mut editor);
        assert_eq!(editor.cursor_col, 6);
        assert_eq!(editor.extra_cursors, [(0, 1), (0, 2), (0, 3), (0, 4)]);
        assert_eq!(editor.extra_cursor_anchors[2], Some((0, 2)));
        assert_undo_redo(&mut editor, before, "  界🙂  \r\noutside");
    }
}

#[test]
fn removing_incomplete_paired_comments_is_a_noop() {
    for (path, before) in [("style.css", "/*/"), ("page.html", "<!-->")] {
        let mut editor = editor(before);
        editor.file_path = Some(path.into());
        editor.remove_line_comment();
        assert_eq!(editor.rope.to_string(), before);
        assert!(!editor.modified);
        assert!(!editor.can_undo());
    }
}

#[test]
fn implicit_block_comment_undo_restores_the_original_caret_without_selecting_the_line() {
    let mut editor = editor("one + two");
    editor.file_path = Some("main.c".into());
    editor.cursor_col = 3;
    editor.toggle_block_comment();
    assert_eq!(editor.rope.to_string(), "/* one + two */");
    editor.undo();
    assert_eq!(editor.rope.to_string(), "one + two");
    assert_eq!((editor.cursor_line, editor.cursor_col), (0, 3));
    assert_eq!(editor.selection_anchor, None);
}

#[test]
fn moving_selected_blocks_preserves_text_selection_and_warm_wrap_layout() {
    let lines = ["α", "", "longer🙂", "z"];
    for ending in ["\n", "\r\n"] {
        for trailing in [false, true] {
            let before = lines.join(ending) + if trailing { ending } else { "" };
            for first in 0..lines.len() {
                for last in first..lines.len() {
                    for down in [false, true] {
                        if (!down && first == 0) || (down && last + 1 == lines.len()) {
                            continue;
                        }
                        for reverse in [false, true] {
                            let mut editor = editor(&before);
                            editor.word_wrap = true;
                            editor.viewport_width = 4;
                            let start = (first, 0);
                            let end = if last + 1 < editor.line_count() {
                                (last + 1, 0)
                            } else {
                                (last, editor.line_len(last))
                            };
                            let (cursor, anchor) =
                                if reverse { (start, end) } else { (end, start) };
                            (editor.cursor_line, editor.cursor_col) = cursor;
                            editor.selection_anchor = Some(anchor);
                            let selection = editor.selected_text().unwrap_or_default();
                            for line in 0..editor.line_count() {
                                editor.wrap_ranges_for_line(line, 4);
                            }
                            let mut expected = lines.to_vec();
                            if down {
                                expected[first..=last + 1].rotate_right(1);
                                editor.move_line_down();
                            } else {
                                expected[first - 1..=last].rotate_left(1);
                                editor.move_line_up();
                            }
                            let after = expected.join(ending) + if trailing { ending } else { "" };
                            let expected_selection = if down && last + 2 == lines.len() && !trailing
                            {
                                selection.strip_suffix(ending).unwrap_or(&selection)
                            } else {
                                &selection
                            };
                            assert_eq!(
                                editor.selected_text().unwrap_or_default(),
                                expected_selection,
                                "first={first}, last={last}, down={down}, reverse={reverse}, trailing={trailing}"
                            );
                            for line in 0..editor.line_count() {
                                let text = editor.rope.line(line).to_string();
                                let ranges = Editor::wrap_ranges_for_line_text_tabbed(
                                    text.trim_end_matches(['\r', '\n']),
                                    4,
                                    editor.tab_size,
                                );
                                assert_eq!(&*editor.wrap_ranges_for_line(line, 4), &ranges);
                            }
                            assert_undo_redo(&mut editor, &before, &after);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn line_copy_paste_recognizes_crlf_and_adds_a_missing_final_separator() {
    for clipboard in ["copied\r\n", "copied\n", "copied"] {
        let before = "head\ntarget";
        let mut editor = editor(before);
        editor.cursor_line = 1;
        editor.cursor_col = 3;
        editor.clipboard = clipboard.into();
        editor.clipboard_is_line = true;
        editor.paste_with_text(clipboard);
        assert_eq!((editor.cursor_line, editor.cursor_col), (2, 3));
        assert_undo_redo(&mut editor, before, "head\ncopied\ntarget");
    }
}

#[test]
fn line_copy_paste_over_a_selection_replaces_at_the_selection() {
    let before = "prefix old suffix";
    let mut editor = editor(before);
    editor.selection_anchor = Some((0, 7));
    editor.cursor_col = 10;
    editor.clipboard = "copied\n".into();
    editor.clipboard_is_line = true;
    editor.paste_with_text("copied\n");
    assert_undo_redo(&mut editor, before, "prefix copied\n suffix");
}

#[test]
fn plain_paste_replaces_all_cursor_selections_in_one_undo_step() {
    let before = "  one\n  two\n  three";
    let mut editor = editor(before);
    editor.cursor_col = 5;
    editor.selection_anchor = Some((0, 2));
    editor.extra_cursors = vec![(1, 5), (2, 7)];
    editor.extra_cursor_anchors = vec![Some((1, 2)), Some((2, 2))];
    editor.paste_plain_with_text("界\r\n x");
    assert_eq!(editor.undo_stack.len(), 1);
    assert_eq!((editor.cursor_line, editor.cursor_col), (1, 2));
    assert_eq!(editor.extra_cursors, [(3, 2), (5, 2)]);
    assert_undo_redo(&mut editor, before, "  界\n x\n  界\n x\n  界\n x");
}

#[test]
fn delete_lines_excludes_the_zero_column_selection_end() {
    for ending in ["\n", "\r\n"] {
        for reverse in [false, true] {
            let before = format!("head{ending}one{ending}two{ending}tail");
            let mut editor = editor(&before);
            let (cursor, anchor) = if reverse { ((1, 0), (2, 0)) } else { ((2, 0), (1, 0)) };
            (editor.cursor_line, editor.cursor_col) = cursor;
            editor.selection_anchor = Some(anchor);
            editor.delete_line();
            assert_undo_redo(&mut editor, &before, &format!("head{ending}two{ending}tail"));
        }
    }
}

#[test]
fn delete_lines_uses_the_same_selection_boundary_for_secondary_cursors() {
    let before = "head\none\nkeep\ntwo\ntail";
    let mut editor = editor(before);
    editor.cursor_line = 1;
    editor.extra_cursors = vec![(4, 0)];
    editor.extra_cursor_anchors = vec![Some((3, 0))];
    editor.delete_line();
    assert_undo_redo(&mut editor, before, "head\nkeep\ntail");
}

#[test]
fn saving_rebases_undo_dirty_states_and_breaks_typing_groups() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor("original");
    editor.file_path = Some(dir.path().join("example.txt"));
    editor.save_file_with_options(false, false).unwrap();
    editor.cursor_col = 8;
    editor.insert_char('a');
    editor.save_file_with_options(false, false).unwrap();
    editor.insert_char('b');
    editor.undo();
    assert_eq!(editor.rope.to_string(), "originala");
    assert!(!editor.modified, "undo to the latest save must be clean");
    editor.undo();
    assert_eq!(editor.rope.to_string(), "original");
    assert!(editor.modified, "the old saved content now differs from disk");
    editor.redo();
    assert_eq!(editor.rope.to_string(), "originala");
    assert!(!editor.modified);
    editor.redo();
    assert!(editor.modified);
}

#[test]
fn background_save_after_undo_marks_the_old_baseline_dirty() {
    let mut editor = editor("original");
    editor.saved_rope = Some(editor.rope.clone());
    editor.cursor_col = 8;
    editor.insert_char('a');
    let source_version = editor.content_version;
    let written = editor.rope.clone();
    editor.undo();
    assert!(!editor.modified);
    editor.apply_background_save(source_version, &written, written.clone(), "UTF-8", None, None, None, u64::MAX);
    assert_eq!(editor.rope.to_string(), "original");
    assert!(editor.modified, "an in-flight save changed the disk baseline after undo");
    editor.redo();
    assert!(!editor.modified, "redo returns to the newly saved content");
}

#[test]
fn background_save_marks_its_snapshot_clean_in_typing_history() {
    let mut editor = editor("original");
    editor.saved_rope = Some(editor.rope.clone());
    editor.cursor_col = 8;
    editor.insert_char('a');
    let source_version = editor.content_version;
    let written = editor.rope.clone();
    editor.break_undo_group();
    editor.insert_char('b');
    editor.apply_background_save(source_version, &written, written.clone(), "UTF-8", None, None, None, u64::MAX);
    assert!(editor.modified);
    editor.undo();
    assert_eq!(editor.rope.to_string(), "originala");
    assert!(!editor.modified);
    editor.undo();
    assert!(editor.modified);
}

#[test]
fn current_save_reply_keeps_local_history_identity_without_comparing_file_contents() {
    let mut editor = editor("original");
    editor.saved_rope = Some(editor.rope.clone());
    editor.cursor_col = 8;
    editor.insert_char('a');
    editor.apply_background_save(
        editor.content_version,
        &editor.rope.clone(),
        ropey::Rope::from_str("originala"),
        "UTF-8",
        None,
        None,
        None,
        u64::MAX,
    );
    assert!(!editor.modified);
    editor.undo();
    assert_eq!(editor.rope.to_string(), "original");
    assert!(editor.modified);
    editor.redo();
    assert_eq!(editor.rope.to_string(), "originala");
    assert!(!editor.modified);
}

#[test]
fn undoing_a_disk_reload_marks_the_previous_content_unsaved() {
    let mut editor = editor("original");
    editor.saved_rope = Some(editor.rope.clone());
    editor.apply_reloaded_file(super::LoadedTextFile {
            remote_checkpoint: None,
        rope: ropey::Rope::from_str("external edit"),
        encoding: "UTF-8",
        file_size: 13,
        last_disk_modified: None,
    });
    assert!(!editor.modified);
    editor.undo();
    assert_eq!(editor.rope.to_string(), "original");
    assert!(editor.modified);
    editor.redo();
    assert_eq!(editor.rope.to_string(), "external edit");
    assert!(!editor.modified);
}

#[test]
fn unchanged_disk_reload_preserves_redo_and_does_not_add_an_undo_step() {
    let mut editor = editor("saved\n");
    editor.saved_rope = Some(editor.rope.clone());
    editor.insert_char('x');
    editor.undo();
    let version = editor.content_version;
    let undo_len = editor.undo_stack.len();
    editor.apply_reloaded_file(super::LoadedTextFile {
            remote_checkpoint: None,
        rope: ropey::Rope::from_str("saved\n"), encoding: "UTF-8", file_size: 6, last_disk_modified: None,
    });
    assert_eq!(editor.content_version, version);
    assert_eq!(editor.undo_stack.len(), undo_len);
    editor.redo();
    assert_eq!(editor.rope.to_string(), "xsaved\n");
    assert!(editor.modified);
    editor.undo();
    assert!(!editor.modified);
}

#[test]
fn disk_reload_undo_invalidates_changes_far_from_the_cursor() {
    let mut editor = editor("first\nmiddle\nlast\n");
    editor.cursor_line = 2;
    editor.apply_reloaded_file(super::LoadedTextFile {
            remote_checkpoint: None,
        rope: ropey::Rope::from_str("FIRST\nmiddle\nlast\n"), encoding: "UTF-8", file_size: 18, last_disk_modified: None,
    });
    editor.syntax_dirty_from = None;
    editor.syntax_dirty_through = None;
    editor.undo();
    assert_eq!(editor.syntax_dirty_from, Some(0));
    assert!(editor.syntax_dirty_through.is_some_and(|line| line >= 2));
}

#[test]
fn synchronous_opens_decode_utf16_before_checking_for_binary_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let text = "hello 界🙂\r\nnext line";
    for little_endian in [false, true] {
        let path = dir.path().join(if little_endian { "little.txt" } else { "big.txt" });
        let mut bytes = if little_endian { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
        for unit in text.encode_utf16() {
            bytes.extend(if little_endian { unit.to_le_bytes() } else { unit.to_be_bytes() });
        }
        std::fs::write(&path, &bytes).unwrap();
        let mut editor = editor("old\ncontents");
        editor.extra_cursors = vec![(1, 8)];
        editor.extra_cursor_anchors = vec![Some((1, 2))];
        editor.open_file(&path).unwrap();
        assert_eq!(editor.rope.to_string(), text);
        assert_eq!(editor.encoding, if little_endian { "UTF-16 LE" } else { "UTF-16 BE" });
        assert_eq!(editor.cached_line_ending, "CRLF");
        assert_eq!(editor.file_size, bytes.len() as u64);
        assert!(editor.extra_cursors.is_empty());
        assert_valid_positions(&editor);
        editor.save_file_with_options(false, false).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }
}

#[test]
fn synchronous_binary_open_cannot_undo_back_to_the_previous_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("binary.bin");
    std::fs::write(&path, b"binary\0data").unwrap();
    let mut editor = editor("old file");
    editor.insert_char('x');
    editor.open_file(&path).unwrap();
    assert_eq!(editor.encoding, "Binary");
    assert!(!editor.can_undo());
    assert!(!editor.can_redo());
    assert!(!editor.modified);
    assert!(editor.saved_rope.as_ref().unwrap().is_instance(&editor.rope));
}
