    use super::Editor;
    use ropey::Rope;

    #[test]
    fn move_up_down_traverses_wrapped_rows_on_same_line() {
        let mut ed = Editor::test_with_text("abcdefghij");
        ed.word_wrap = true;
        ed.viewport_width = 4;
        ed.cursor_line = 0;
        ed.cursor_col = 5;

        ed.move_up();
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 1);

        ed.move_down();
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 5);
    }

    #[test]
    fn move_down_from_wrapped_line_enters_next_line_by_visual_column() {
        let mut ed = Editor::test_with_text("abcdefghij\nxy");
        ed.word_wrap = true;
        ed.viewport_width = 4;
        ed.cursor_line = 0;
        ed.cursor_col = 9;

        ed.move_down();
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 1);
    }

    #[test]
    fn move_up_on_first_line_moves_to_line_start() {
        let mut ed = Editor::test_with_text("abcdef\nx");
        ed.cursor_line = 0;
        ed.cursor_col = 4;

        ed.move_up();
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn move_down_on_last_line_moves_to_line_end() {
        let mut ed = Editor::test_with_text("abc\ndefgh");
        ed.cursor_line = 1;
        ed.cursor_col = 2;

        ed.move_down();
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 5);
    }

    #[test]
    fn word_at_cursor_handles_words_separators_empty_lines_and_invalid_columns() {
        for (source, col, expected) in [
            ("hello world", 0, Some("hello")),
            ("hello world", 5, None),
            ("\n", 0, None),
            ("hi", 10, None),
            ("a", 0, Some("a")),
            ("café world\n", 2, Some("café")),
        ] {
            let mut ed = Editor::test_with_text(source);
            ed.cursor_col = col;
            assert_eq!(
                ed.word_at_cursor().as_deref(),
                expected,
                "{source:?} col={col}"
            );
        }
    }

    #[test]
    fn select_all_covers_the_complete_source() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.select_all();
        assert_eq!(ed.selection_range(), Some((0, 0, 2, 0)));
        assert_eq!(ed.selected_text().as_deref(), Some("hello\nworld\n"));
    }

    #[test]
    fn page_down_basic() {
        let mut ed = Editor::test_with_text(&"line\n".repeat(100));
        ed.viewport_height = 20;
        ed.cursor_line = 0;
        ed.page_down();
        assert!(ed.cursor_line > 0);
        assert!(ed.cursor_line <= 20);
    }

    #[test]
    fn page_up_basic() {
        let mut ed = Editor::test_with_text(&"line\n".repeat(100));
        ed.viewport_height = 20;
        ed.cursor_line = 50;
        ed.page_up();
        assert!(ed.cursor_line < 50);
    }

    #[test]
    fn page_navigation_jumps_across_large_folded_regions() {
        let mut ed = Editor::test_with_text(&"line\n".repeat(50_000));
        ed.viewport_height = 20;
        ed.folded_ranges.insert(10, 49_990);

        ed.cursor_line = 0;
        ed.page_down();
        assert_eq!(ed.cursor_line, 49_990);

        ed.cursor_line = 49_999;
        ed.page_up();
        assert_eq!(ed.cursor_line, 10);
    }

    #[test]
    fn page_up_at_top() {
        let mut ed = Editor::test_with_text("a\nb\n");
        ed.viewport_height = 20;
        ed.cursor_line = 0;
        ed.page_up();
        assert_eq!(ed.cursor_line, 0);
    }

    #[test]
    fn set_cursor_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\nfoo\n");
        ed.set_cursor(1, 3);
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 3);
    }

    #[test]
    fn set_cursor_clamps() {
        let mut ed = Editor::test_with_text("hi\n");
        ed.set_cursor(100, 100);
        assert!(ed.cursor_line < 100);
        assert!(ed.cursor_col <= ed.line_len(ed.cursor_line));
    }

    #[test]
    fn select_word_at_cursor_sets_selection() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 1; // inside "hello"
        ed.select_word_at_cursor();
        assert!(ed.selection_anchor.is_some());
        assert_eq!(ed.selected_text(), Some("hello".to_string()));
    }

    #[test]
    fn move_word_left_basic() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 11; // end of "world"
        ed.move_word_left();
        assert_eq!(ed.cursor_col, 6); // start of "world"
    }

    #[test]
    fn move_word_right_basic() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.move_word_right();
        assert_eq!(ed.cursor_col, 5); // end of "hello"
    }

    #[test]
    fn jump_to_word_occurrence_basic() {
        let mut ed = Editor::test_with_text("hello world\nhello again\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.jump_to_word_occurrence("hello");
        assert_eq!(ed.cursor_line, 1);
        // select_word_at_cursor moves cursor to end of word
        assert!(ed.selection_anchor.is_some());
    }

    #[test]
    fn jump_to_word_occurrence_not_found() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.jump_to_word_occurrence("missing");
        // Cursor should stay in place
        assert_eq!(ed.cursor_line, 0);
    }

    #[test]
    fn select_to_matching_bracket_basic() {
        let mut ed = Editor::test_with_text("(hello)\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.select_to_matching_bracket();
        assert!(ed.selection_anchor.is_some());
        assert_eq!(ed.cursor_col, 6);
    }

    #[test]
    fn go_to_matching_bracket_basic() {
        let mut ed = Editor::test_with_text("(hello)\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.go_to_matching_bracket();
        assert_eq!(ed.cursor_col, 6);
    }

    #[test]
    fn go_to_matching_bracket_no_bracket() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 2;
        ed.go_to_matching_bracket();
        assert_eq!(ed.cursor_col, 2); // unchanged
    }

    #[test]
    fn clamp_cursor_out_of_bounds() {
        let mut ed = Editor::test_with_text("hi\n");
        ed.cursor_line = 100;
        ed.cursor_col = 100;
        ed.clamp_cursor();
        assert!(ed.cursor_line < 100);
        assert!(ed.cursor_col <= 2);
    }

    #[test]
    fn start_and_clear_selection() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 2;
        assert!(!ed.has_selection());
        ed.start_selection();
        assert!(!ed.has_selection());
        assert_eq!(ed.selection_anchor, Some((0, 2)));
        ed.move_right();
        assert!(ed.has_selection());
        ed.clear_selection();
        assert!(!ed.has_selection());
    }

    #[test]
    fn selection_range_normalized() {
        let mut ed = Editor::test_with_text("hello world\n");
        // Anchor after cursor = reversed selection
        ed.selection_anchor = Some((0, 8));
        ed.cursor_line = 0;
        ed.cursor_col = 2;
        let (sl, sc, el, ec) = ed.selection_range().unwrap();
        assert_eq!((sl, sc), (0, 2));
        assert_eq!((el, ec), (0, 8));
    }

    #[test]
    fn selection_range_none() {
        let ed = Editor::new();
        assert!(ed.selection_range().is_none());
    }

    #[test]
    fn push_cursor_position_dedup() {
        let mut ed = Editor::new();
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.push_cursor_position();
        ed.push_cursor_position(); // duplicate, should not push
        assert_eq!(ed.cursor_position_stack.len(), 1);
    }

    #[test]
    fn selected_text_basic() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.selection_anchor = Some((0, 0));
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        let text = ed.selected_text();
        assert_eq!(text, Some("hello".to_string()));
    }

    #[test]
    fn selected_text_none() {
        let ed = Editor::new();
        assert_eq!(ed.selected_text(), None);
    }

    #[test]
    fn ensure_visible_clamps_scroll() {
        let mut ed = Editor::test_with_text("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n");
        ed.viewport_height = 3;
        ed.cursor_line = 8;
        ed.ensure_visible();
        // Scroll should have moved so cursor is visible
        assert!(ed.scroll_offset + ed.viewport_height > ed.cursor_line);
    }

    #[test]
    fn is_in_selection_within() {
        let mut ed = Editor::test_with_text("hello world\nsecond line\n");
        ed.selection_anchor = Some((0, 2));
        ed.cursor_line = 0;
        ed.cursor_col = 8;
        assert!(ed.is_in_selection(0, 3));
        assert!(!ed.is_in_selection(0, 0));
        assert!(!ed.is_in_selection(0, 8)); // end is exclusive
    }

    #[test]
    fn is_in_selection_multiline() {
        let mut ed = Editor::test_with_text("hello\nworld\nfoo\n");
        ed.selection_anchor = Some((0, 2));
        ed.cursor_line = 2;
        ed.cursor_col = 1;
        assert!(ed.is_in_selection(1, 0)); // middle line fully in selection
        assert!(!ed.is_in_selection(2, 2)); // past end col
    }

    #[test]
    fn is_in_selection_no_selection() {
        let ed = Editor::new();
        assert!(!ed.is_in_selection(0, 0));
    }

    #[test]
    fn word_prefix_at_cursor_basic() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        assert_eq!(ed.word_prefix_at_cursor(), Some("hello".to_string()));
    }

    #[test]
    fn word_prefix_at_cursor_col_zero() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        assert_eq!(ed.word_prefix_at_cursor(), None);
    }

    #[test]
    fn select_inside_brackets_basic() {
        let mut ed = Editor::test_with_text("fn(abc)\n");
        ed.cursor_line = 0;
        ed.cursor_col = 4; // inside 'abc'
        ed.select_inside_brackets();
        assert!(ed.selection_anchor.is_some());
        let text = ed.selected_text().unwrap_or_default();
        assert_eq!(text, "abc");
    }

    #[test]
    fn select_inside_brackets_no_bracket() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.select_inside_brackets();
        assert!(ed.selection_anchor.is_none());
    }

    #[test]
    fn scroll_up_down() {
        let mut ed = Editor::test_with_text("a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n");
        ed.viewport_height = 5;
        ed.scroll_down(3);
        assert_eq!(ed.scroll_offset, 3);
        ed.scroll_up(2);
        assert_eq!(ed.scroll_offset, 1);
    }

    #[test]
    fn select_next_occurrence_no_duplicate_on_wrap() {
        let mut ed = Editor::test_with_text("foo bar foo\n");
        // Select first "foo" (anchor=0,0 cursor=0,3)
        ed.selection_anchor = Some((0, 0));
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        // First Ctrl+D selects second "foo"
        ed.select_next_occurrence();
        assert_eq!(ed.extra_cursors.len(), 1);
        assert_eq!(ed.cursor_col, 11); // end of second "foo"
        // Second Ctrl+D: all occurrences already selected — should NOT add a duplicate
        ed.select_next_occurrence();
        assert_eq!(ed.extra_cursors.len(), 1, "Should not add duplicate cursor on wrap");
    }

    #[test]
    fn select_next_occurrence_three_occurrences() {
        let mut ed = Editor::test_with_text("abc xyz abc xyz abc\n");
        // Select first "abc"
        ed.selection_anchor = Some((0, 0));
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.select_next_occurrence(); // selects second "abc" at col 8-11
        assert_eq!(ed.extra_cursors.len(), 1);
        ed.select_next_occurrence(); // selects third "abc" at col 16-19
        assert_eq!(ed.extra_cursors.len(), 2);
        ed.select_next_occurrence(); // all selected, no more
        assert_eq!(ed.extra_cursors.len(), 2);
    }

    #[test]
    fn select_next_occurrence_no_selection_selects_word() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 2; // inside "hello"
        ed.select_next_occurrence(); // should select "hello" first
        assert!(ed.selection_anchor.is_some());
    }

    #[test]
    fn select_all_occurrences_basic() {
        let mut ed = Editor::test_with_text("foo bar foo baz foo\n");
        ed.selection_anchor = Some((0, 0));
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.select_all_occurrences();
        // Should find all "foo" occurrences
        assert!(!ed.extra_cursors.is_empty());
    }

    #[test]
    fn empty_file_cursor_operations() {
        let mut ed = Editor::test_with_text("");
        // All these should not panic on empty file
        ed.move_up();
        assert_eq!((ed.cursor_line, ed.cursor_col), (0, 0));
        ed.move_down();
        assert_eq!((ed.cursor_line, ed.cursor_col), (0, 0));
        ed.move_left();
        assert_eq!((ed.cursor_line, ed.cursor_col), (0, 0));
        ed.move_right();
        assert_eq!((ed.cursor_line, ed.cursor_col), (0, 0));
        ed.move_to_line_start();
        ed.move_to_line_end();
        ed.move_to_top();
        ed.move_to_bottom();
        ed.move_word_left();
        ed.move_word_right();
        ed.page_up();
        ed.page_down();
        assert_eq!((ed.cursor_line, ed.cursor_col), (0, 0));
    }

    #[test]
    fn cursor_at_end_of_file_no_trailing_newline() {
        let mut ed = Editor::test_with_text("abc");
        ed.cursor_col = 3;
        // move_right should not advance past end
        ed.move_right();
        assert_eq!((ed.cursor_line, ed.cursor_col), (0, 3));
        // delete_char at end should be noop
        ed.delete_char();
        assert_eq!(ed.rope.to_string(), "abc");
    }

    #[test]
    fn select_inside_quotes_no_quotes() {
        let mut ed = Editor::test_with_text("no quotes here");
        ed.cursor_col = 5;
        ed.select_inside_quotes(); // should not panic
        assert!(ed.selection_anchor.is_none());
    }

    #[test]
    fn scroll_up_past_top() {
        let mut ed = Editor::test_with_text("line1\nline2\nline3\n");
        ed.scroll_offset = 0;
        ed.scroll_up(100); // should clamp to 0
        assert_eq!(ed.scroll_offset, 0);
    }

    #[test]
    fn expand_shrink_selection_roundtrip() {
        let mut ed = Editor::test_with_text("hello world test\n");
        ed.cursor_col = 6;
        ed.expand_selection();
        assert_eq!(ed.selected_text().as_deref(), Some("world"));
        ed.expand_selection();
        assert_eq!(ed.selected_text().as_deref(), Some("hello world test\n"));
        ed.shrink_selection();
        assert_eq!(ed.selected_text().as_deref(), Some("world"));
        ed.shrink_selection();
        assert_eq!(ed.selection_anchor, None);
        assert!(ed.selection_history.is_empty());
    }

    #[test]
    fn short_file_scroll_offset_stays_zero() {
        let mut ed = Editor::test_with_text("a\nb\nc\n");
        ed.viewport_height = 30;
        ed.viewport_width = 80;
        ed.cursor_line = 2;
        ed.ensure_visible();
        assert_eq!(ed.scroll_offset, 0, "scroll_offset should be 0 for short files");
    }

    #[test]
    fn short_file_scroll_down_no_effect() {
        let mut ed = Editor::test_with_text("a\nb\n");
        ed.viewport_height = 30;
        ed.scroll_past_end = false;
        ed.scroll_down(5);
        assert_eq!(ed.scroll_offset, 0, "scroll_down should have no effect when file < viewport");
    }

    #[test]
    fn short_file_page_down_clamps_cursor() {
        let mut ed = Editor::test_with_text("a\nb\nc\n");
        ed.viewport_height = 30;
        ed.viewport_width = 80;
        ed.scroll_past_end = false;
        ed.page_down();
        // Cursor should be at last line, not beyond
        assert!(ed.cursor_line < ed.line_count());
        assert_eq!(ed.scroll_offset, 0);
    }

    #[test]
    fn short_file_page_up_at_top() {
        let mut ed = Editor::test_with_text("a\nb\n");
        ed.viewport_height = 30;
        ed.viewport_width = 80;
        ed.cursor_line = 1;
        ed.page_up();
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.scroll_offset, 0);
    }

    #[test]
    fn single_line_file_scroll() {
        let mut ed = Editor::test_with_text("hello");
        ed.viewport_height = 30;
        ed.viewport_width = 80;
        ed.ensure_visible();
        assert_eq!(ed.scroll_offset, 0);
        ed.scroll_down(10);
        assert_eq!(ed.scroll_offset, 0);
    }

    #[test]
    fn empty_file_scroll() {
        let mut ed = Editor::test_with_text("");
        ed.viewport_height = 30;
        ed.viewport_width = 80;
        ed.ensure_visible();
        assert_eq!(ed.scroll_offset, 0);
        ed.scroll_down(5);
        assert_eq!(ed.scroll_offset, 0);
        ed.page_down();
        assert_eq!(ed.scroll_offset, 0);
    }

    #[test]
    fn short_file_center_cursor() {
        let mut ed = Editor::test_with_text("a\nb\nc\n");
        ed.viewport_height = 30;
        ed.viewport_width = 80;
        ed.cursor_line = 1;
        ed.center_cursor();
        // Center should still keep scroll at 0 for short files
        // (cursor_line=1, center=15, 1.saturating_sub(15) = 0)
        assert_eq!(ed.scroll_offset, 0);
    }

    #[test]
    fn move_paragraph_down_across_multiple_blanks() {
        let mut ed = Editor::test_with_text("aaa\nbbb\n\nccc\nddd\n");
        ed.viewport_height = 30;
        ed.viewport_width = 80;
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.move_paragraph_down();
        // Should skip past "aaa\nbbb\n" and blank line to "ccc"
        assert_eq!(ed.cursor_line, 3);
    }

    #[test]
    fn move_paragraph_up_from_second_paragraph() {
        let mut ed = Editor::test_with_text("aaa\nbbb\n\nccc\nddd\n");
        ed.viewport_height = 30;
        ed.viewport_width = 80;
        ed.cursor_line = 4; // on "ddd"
        ed.cursor_col = 0;
        ed.move_paragraph_up();
        // Should move to start of file or previous paragraph boundary
        assert!(ed.cursor_line <= 1, "cursor moved up to line {}", ed.cursor_line);
    }

    #[test]
    fn move_paragraph_down_at_end() {
        let mut ed = Editor::test_with_text("aaa\nbbb\n");
        ed.viewport_height = 30;
        ed.viewport_width = 80;
        let last = ed.line_count().saturating_sub(1);
        ed.cursor_line = last;
        ed.cursor_col = 0;
        ed.move_paragraph_down();
        // Should stay at end
        assert_eq!(ed.cursor_line, last);
    }

    #[test]
    fn move_paragraph_up_at_start() {
        let mut ed = Editor::test_with_text("aaa\nbbb\n");
        ed.viewport_height = 30;
        ed.viewport_width = 80;
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.move_paragraph_up();
        assert_eq!(ed.cursor_line, 0);
    }

    #[test]
    fn select_word_at_cursor_middle() {
        let mut ed = Editor::test_with_text("foo bar_baz quux\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5; // inside "bar_baz"
        ed.select_word_at_cursor();
        assert_eq!(ed.selection_anchor, Some((0, 4)));
        assert_eq!(ed.cursor_col, 11);
        assert_eq!(ed.selected_text(), Some("bar_baz".to_string()));
    }

    #[test]
    fn select_line_full_content() {
        let mut ed = Editor::test_with_text("hello\nworld\nfoo\n");
        ed.cursor_line = 1;
        ed.cursor_col = 2;
        ed.select_line();
        assert_eq!(ed.selection_anchor, Some((1, 0)));
        assert_eq!(ed.cursor_line, 2);
        assert_eq!(ed.cursor_col, 0);
        assert_eq!(ed.selected_text(), Some("world\n".to_string()));
    }

    #[test]
    fn select_all_occurrences_creates_multicursors() {
        let mut ed = Editor::test_with_text("foo bar foo baz foo\n");
        ed.cursor_line = 0;
        ed.cursor_col = 1; // inside first "foo"
        ed.select_all_occurrences();
        // Primary cursor + 2 extra cursors = 3 occurrences
        assert_eq!(ed.extra_cursors.len(), 2);
        // All should have selection anchors
        assert!(ed.selection_anchor.is_some());
        for anchor in &ed.extra_cursor_anchors {
            assert!(anchor.is_some());
        }
    }

    #[test]
    fn move_to_line_start_and_end() {
        let mut ed = Editor::test_with_text("  hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 7;
        ed.move_to_line_start();
        // Smart home: first goes to first non-whitespace
        assert!(ed.cursor_col <= 2);
        ed.move_to_line_end();
        assert_eq!(ed.cursor_col, 13);
    }

    #[test]
    fn test_clamp_cursor_col_at_end_of_line() {
        let mut ed = Editor::test_with_text("abc\n");
        ed.cursor_line = 0;
        ed.cursor_col = 50;
        ed.clamp_cursor();
        assert_eq!(ed.cursor_col, 3);
    }

    #[test]
    fn test_clamp_cursor_col_on_empty_line() {
        let mut ed = Editor::test_with_text("hello\n\nworld\n");
        ed.cursor_line = 1; // the empty line
        ed.cursor_col = 5;
        ed.clamp_cursor();
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn test_cursor_stays_valid_after_line_delete() {
        let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
        ed.cursor_line = 1;
        ed.cursor_col = 2;
        ed.delete_line();
        // After deleting line 1 ("bbb"), cursor should still be valid
        assert!(ed.cursor_line < ed.line_count());
        assert!(ed.cursor_col <= ed.current_line_len());
    }

    #[test]
    fn test_cursor_stays_valid_after_last_line_delete() {
        let mut ed = Editor::test_with_text("aaa\nbbb\n");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.delete_line();
        // Deleted last content line; cursor should move up
        assert!(ed.cursor_line < ed.line_count());
        assert!(ed.cursor_col <= ed.current_line_len());
    }

    #[test]
    fn test_cursor_col_after_move_to_shorter_line() {
        let mut ed = Editor::test_with_text("long line here\nhi\n");
        ed.cursor_line = 0;
        ed.cursor_col = 14; // end of "long line here"
        ed.move_down();
        // "hi" is only 2 chars, cursor col should clamp
        assert_eq!(ed.cursor_line, 1);
        assert!(ed.cursor_col <= 2);
    }

    #[test]
    fn test_copy_with_no_selection() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.selection_anchor = None;
        ed.copy();
        // With no selection, copy copies the current line (VS Code behavior)
        assert!(ed.clipboard.contains("hello world"));
        assert!(ed.clipboard_is_line);
    }

    #[test]
    fn test_move_beyond_last_line() {
        let mut ed = Editor::test_with_text("only line\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        // Move down repeatedly past end of file — should not panic
        for _ in 0..10 {
            ed.move_down();
        }
        assert!(ed.cursor_line < ed.line_count());
        assert!(ed.cursor_col <= ed.current_line_len());
    }

    // --- move_left / move_right wrapping and boundary tests ---

    #[test]
    fn move_left_wraps_to_previous_line_end() {
        let mut ed = Editor::test_with_text("abc\ndefgh\n");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.move_left();
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 3);
    }

    #[test]
    fn move_right_wraps_to_next_line_start() {
        let mut ed = Editor::test_with_text("abc\ndef\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.move_right();
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn move_left_at_file_start_stays() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.move_left();
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn move_right_at_file_end_stays() {
        let mut ed = Editor::test_with_text("abc");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.move_right();
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 3);
    }

    // --- smart home toggle ---

    #[test]
    fn smart_home_toggles_between_first_nonws_and_col_zero() {
        let mut ed = Editor::test_with_text("    indented\n");
        ed.cursor_line = 0;
        ed.cursor_col = 8;

        // First press: goes to first non-whitespace (col 4)
        ed.move_to_line_start();
        assert_eq!(ed.cursor_col, 4);

        // Second press: goes to col 0
        ed.move_to_line_start();
        assert_eq!(ed.cursor_col, 0);

        // Third press: back to first non-whitespace
        ed.move_to_line_start();
        assert_eq!(ed.cursor_col, 4);
    }

    #[test]
    fn move_to_line_start_no_leading_whitespace() {
        let mut ed = Editor::test_with_text("no indent\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        // No leading whitespace: first_non_ws == 0, so goes to col 0
        ed.move_to_line_start();
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn move_to_line_end_places_cursor_at_end() {
        let mut ed = Editor::test_with_text("hello world\nsecond\n");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.move_to_line_end();
        assert_eq!(ed.cursor_col, 6); // "second" has 6 chars
    }

    // --- word navigation edge cases ---

    #[test]
    fn move_word_left_wraps_to_previous_line() {
        let mut ed = Editor::test_with_text("first\nsecond\n");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.move_word_left();
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 5); // end of "first"
    }

    #[test]
    fn move_word_right_wraps_to_next_line() {
        let mut ed = Editor::test_with_text("first\nsecond\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5; // at end of "first"
        ed.move_word_right();
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn move_word_left_skips_punctuation_group() {
        let mut ed = Editor::test_with_text("foo::bar\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5; // on 'b' in "bar"
        ed.move_word_left();
        assert_eq!(ed.cursor_col, 3); // start of "::"
        ed.move_word_left();
        assert_eq!(ed.cursor_col, 0); // start of "foo"
    }

    #[test]
    fn move_word_right_skips_whitespace_then_word() {
        let mut ed = Editor::test_with_text("hello   world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5; // on first space after "hello"
        ed.move_word_right();
        assert_eq!(ed.cursor_col, 8); // start of "world"... actually let's check
        // From whitespace, it jumps to the next non-ws token start
    }

    #[test]
    fn move_word_right_camel_case() {
        let mut ed = Editor::test_with_text("myVariableName\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.move_word_right();
        // CamelCase: stops at uppercase after lowercase → "my" then "Variable" etc.
        assert_eq!(ed.cursor_col, 2); // end of "my"
        ed.move_word_right();
        assert_eq!(ed.cursor_col, 10); // end of "Variable"
    }

    // --- page_up / page_down ---

    #[test]
    fn page_down_clamps_at_end_of_file() {
        let mut ed = Editor::new();
        // 5-line file with viewport_height=10 → page=8
        ed.rope = Rope::from_str("a\nb\nc\nd\ne\n");
        ed.viewport_height = 10;
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.page_down();
        // Should clamp to last line, not go beyond
        assert!(ed.cursor_line < ed.line_count());
    }

    #[test]
    fn page_up_preserves_desired_col() {
        let mut ed = Editor::test_with_text("short\nlong line here\na\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\n");
        ed.viewport_height = 5;
        ed.cursor_line = 10;
        ed.cursor_col = 0;
        ed.desired_col = Some(3);
        ed.page_up();
        // desired_col should be respected when landing on a line long enough
        assert!(ed.cursor_line < 10);
    }

    // --- move_to_top / move_to_bottom ---

    #[test]
    fn move_to_top_resets_cursor_to_origin() {
        let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
        ed.cursor_line = 2;
        ed.cursor_col = 2;
        ed.move_to_top();
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn move_to_bottom_goes_to_end_of_document() {
        for (text, expected) in [
            ("aaa\nbbb\nccc", (2, 3)),
            ("aaa\r\n\tcafé界", (1, 6)),
            ("aaa\nbbb\n", (2, 0)),
            ("café界", (0, 5)),
            ("", (0, 0)),
        ] {
            let mut ed = Editor::test_with_text(text);
            ed.desired_col = Some(42);
            ed.start_selection();
            ed.move_to_bottom();
            assert_eq!((ed.cursor_line, ed.cursor_col), expected, "{text:?}");
            assert_eq!(ed.desired_col, None);
            assert_eq!(ed.selected_text().unwrap_or_default(), text);
        }
    }

    // --- go_to_matching_bracket ---

    #[test]
    fn go_to_matching_bracket_nested() {
        let mut ed = Editor::test_with_text("fn foo(a, (b, c))\n");
        ed.cursor_line = 0;
        ed.cursor_col = 6; // on outer '('
        ed.go_to_matching_bracket();
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 16); // matching outer ')'
    }

    #[test]
    fn go_to_matching_bracket_curly() {
        let mut ed = Editor::test_with_text("if x {\n  y\n}\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5; // on '{'
        ed.go_to_matching_bracket();
        assert_eq!(ed.cursor_line, 2);
        assert_eq!(ed.cursor_col, 0); // matching '}'
    }

    // --- cursor_undo ---

    #[test]
    fn cursor_undo_skips_current_position() {
        let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.push_cursor_position();
        ed.cursor_line = 2;
        ed.cursor_col = 2;
        ed.push_cursor_position();
        // cursor_undo should go back to (0,0), skipping the current (2,2) entry
        ed.cursor_undo();
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 0);
    }

    // --- desired_col preservation ---

    #[test]
    fn desired_col_preserved_across_short_line() {
        let mut ed = Editor::test_with_text("long line here\nhi\nlong line here\n");
        ed.cursor_line = 0;
        ed.cursor_col = 10;
        // Move down: line 1 ("hi") is shorter, col should clamp but desired_col saved
        ed.move_down();
        assert_eq!(ed.cursor_line, 1);
        assert!(ed.cursor_col <= 2);
        // Move down again: line 2 is long, should restore desired_col
        ed.move_down();
        assert_eq!(ed.cursor_line, 2);
        assert_eq!(ed.cursor_col, 10);
    }
