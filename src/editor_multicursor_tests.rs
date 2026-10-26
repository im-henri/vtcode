    use ropey::Rope;

    use crate::editor::Editor;

    fn inserted_positions(text: &str, ch: char) -> Vec<usize> {
        text.chars()
            .enumerate()
            .filter_map(|(idx, c)| if c == ch { Some(idx) } else { None })
            .collect()
    }

    fn configure_multi_cursors(ed: &mut Editor, cols: &[usize]) {
        ed.cursor_line = 0;
        ed.cursor_col = cols[0];
        ed.extra_cursors = cols.iter().skip(1).map(|&c| (0, c)).collect();
        ed.extra_cursor_anchors = vec![None; cols.len().saturating_sub(1)];
    }

    #[test]
    fn wrapped_same_line_insert_matches_standard_multicursor_offsets() {
        let mut ed = Editor::new();
        ed.word_wrap = true;
        ed.rope = Rope::from_str("abcdefghijklmnopqrstuvwxyz");
        configure_multi_cursors(&mut ed, &[2, 12, 22]);

        ed.multi_cursor_insert_char('X');

        let line = ed.rope.line(0).to_string();
        assert_eq!(inserted_positions(&line, 'X'), vec![2, 13, 24]);
    }

    #[test]
    fn non_wrapped_same_line_insert_keeps_standard_multicursor_semantics() {
        let mut ed = Editor::new();
        ed.word_wrap = false;
        ed.rope = Rope::from_str("abcdefghijklmnopqrstuvwxyz");
        configure_multi_cursors(&mut ed, &[2, 12, 22]);

        ed.multi_cursor_insert_char('X');

        let line = ed.rope.line(0).to_string();
        assert_eq!(inserted_positions(&line, 'X'), vec![2, 13, 24]);
    }

    #[test]
    fn wrapped_same_line_multi_key_insert_appends_in_typed_order() {
        let mut ed = Editor::new();
        ed.word_wrap = true;
        ed.rope = Rope::from_str("huge port editor users");
        configure_multi_cursors(&mut ed, &[4, 9, 16, 22]);

        for c in ['A', 'B', 'C'] {
            ed.multi_cursor_insert_char(c);
        }

        let line = ed.rope.line(0).to_string();
        assert_eq!(line, "hugeABC portABC editorABC usersABC");
    }

    #[test]
    fn wrapped_same_line_multi_key_backspace_removes_in_typed_order() {
        let mut ed = Editor::new();
        ed.word_wrap = true;
        ed.rope = Rope::from_str("hugeABC portABC editorABC usersABC");
        configure_multi_cursors(&mut ed, &[7, 15, 25, 34]);

        for _ in 0..3 {
            ed.multi_cursor_backspace();
        }

        let line = ed.rope.line(0).to_string();
        assert_eq!(line, "huge port editor users");
    }

    #[test]
    fn wrapped_same_line_multi_key_delete_removes_in_typed_order() {
        let mut ed = Editor::new();
        ed.word_wrap = true;
        ed.rope = Rope::from_str("hugeABC portABC editorABC usersABC");
        configure_multi_cursors(&mut ed, &[4, 12, 22, 31]);

        for _ in 0..3 {
            ed.multi_cursor_delete();
        }

        let line = ed.rope.line(0).to_string();
        assert_eq!(line, "huge port editor users");
    }

    #[test]
    fn transpose_words_basic() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 6; // in "world"
        ed.transpose_words();
        assert_eq!(ed.rope.line(0).to_string(), "world hello\n");
    }

    #[test]
    fn transpose_words_at_start() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0; // at start, no previous word
        ed.transpose_words(); // should be no-op
        assert_eq!(ed.rope.line(0).to_string(), "hello world\n");
    }

    #[test]
    fn transpose_words_single_word() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.transpose_words(); // should be no-op (no previous word)
        assert_eq!(ed.rope.line(0).to_string(), "hello\n");
    }

    // --- add_cursor_above / add_cursor_below tests ---

    #[test]
    fn add_cursor_above_basic() {
        let mut ed = Editor::test_with_text("a\nb\nc\n");
        ed.cursor_line = 2;
        ed.cursor_col = 0;
        ed.add_cursor_above();
        assert_eq!(ed.extra_cursors.len(), 1);
        assert_eq!(ed.extra_cursors[0], (1, 0));
    }

    #[test]
    fn toggle_extra_cursor_adds_then_removes_the_same_position() {
        let mut ed = Editor::test_with_text("abcdef\n");
        ed.cursor_col = 0;

        ed.toggle_extra_cursor((0, 3));
        assert_eq!(ed.extra_cursors, vec![(0, 3)]);
        assert_eq!(ed.extra_cursor_anchors, vec![None]);

        ed.extra_cursor_anchors[0] = Some((0, 1));
        ed.toggle_extra_cursor((0, 3));
        assert!(ed.extra_cursors.is_empty());
        assert!(ed.extra_cursor_anchors.is_empty());
    }

    #[test]
    fn toggle_extra_cursor_does_not_duplicate_the_primary_cursor() {
        let mut ed = Editor::test_with_text("abcdef\n");
        ed.cursor_col = 3;

        ed.toggle_extra_cursor((0, 3));

        assert!(ed.extra_cursors.is_empty());
        assert!(ed.extra_cursor_anchors.is_empty());
    }

    #[test]
    fn toggle_primary_cursor_promotes_the_latest_secondary_cursor() {
        let mut ed = Editor::test_with_text("abcdef\nghijkl\nmnopqr\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.selection_anchor = Some((0, 1));
        ed.extra_cursors = vec![(1, 2), (2, 4)];
        ed.extra_cursor_anchors = vec![None, Some((2, 1))];
        ed.desired_col = Some(9);

        ed.toggle_extra_cursor((0, 3));

        assert_eq!((ed.cursor_line, ed.cursor_col), (2, 4));
        assert_eq!(ed.selection_anchor, Some((2, 1)));
        assert_eq!(ed.extra_cursors, vec![(1, 2)]);
        assert_eq!(ed.extra_cursor_anchors, vec![None]);
        assert_eq!(ed.desired_col, None);
    }

    #[test]
    fn add_cursor_below_basic() {
        let mut ed = Editor::test_with_text("a\nb\nc\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.add_cursor_below();
        assert_eq!(ed.extra_cursors.len(), 1);
        assert_eq!(ed.extra_cursors[0], (1, 0));
    }

    #[test]
    fn add_cursor_above_at_top() {
        let mut ed = Editor::test_with_text("a\nb\n");
        ed.cursor_line = 0;
        ed.add_cursor_above();
        assert!(ed.extra_cursors.is_empty()); // can't go above line 0
    }

    // --- multi_cursor_insert_char test ---

    #[test]
    fn multi_cursor_insert_basic() {
        let mut ed = Editor::test_with_text("ab\ncd\n");
        ed.cursor_line = 0;
        ed.cursor_col = 1;
        ed.extra_cursors.push((1, 1));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_insert_char('X');
        assert!(ed.rope.to_string().contains("aXb"));
        assert!(ed.rope.to_string().contains("cXd"));
    }

    // --- multi_cursor_backspace test ---

    #[test]
    fn multi_cursor_backspace_basic() {
        let mut ed = Editor::test_with_text("ab\ncd\n");
        ed.cursor_line = 0;
        ed.cursor_col = 1;
        ed.extra_cursors.push((1, 1));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_backspace();
        assert!(ed.rope.to_string().contains("b\n"));
        assert!(ed.rope.to_string().contains("d\n"));
    }

    // --- transpose_chars test ---

    #[test]
    fn transpose_chars_basic() {
        let mut ed = Editor::test_with_text("ab\n");
        ed.cursor_line = 0;
        ed.cursor_col = 1;
        ed.transpose_chars();
        assert_eq!(ed.rope.line(0).to_string(), "ba\n");
    }

    #[test]
    fn transpose_chars_at_start() {
        let mut ed = Editor::test_with_text("ab\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.transpose_chars(); // should be no-op
        assert_eq!(ed.rope.line(0).to_string(), "ab\n");
    }

    // --- multi_cursor_delete test ---

    #[test]
    fn multi_cursor_delete_basic() {
        let mut ed = Editor::test_with_text("abc\ndef\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.extra_cursors.push((1, 0));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_delete();
        assert!(ed.rope.to_string().starts_with("bc\n"));
    }

    #[test]
    fn multi_cursor_move_left_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.extra_cursors.push((1, 2));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_move_left();
        assert_eq!(ed.cursor_col, 2);
        assert_eq!(ed.extra_cursors[0], (1, 1));
    }

    #[test]
    fn multi_cursor_move_right_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.extra_cursors.push((1, 2));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_move_right();
        assert_eq!(ed.cursor_col, 4);
        assert_eq!(ed.extra_cursors[0], (1, 3));
    }

    #[test]
    fn multi_cursor_move_up_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\nfoo\n");
        ed.cursor_line = 2;
        ed.cursor_col = 1;
        ed.extra_cursors.push((1, 2));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_move_up();
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.extra_cursors[0].0, 0);
    }

    #[test]
    fn multi_cursor_move_down_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\nfoo\n");
        ed.cursor_line = 0;
        ed.cursor_col = 1;
        ed.extra_cursors.push((1, 2));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_move_down();
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.extra_cursors[0].0, 2);
    }

    #[test]
    fn multi_cursor_select_move_right_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.extra_cursors.push((1, 0));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_select_move_right();
        assert_eq!(ed.cursor_col, 1);
        // anchor should be set
        assert!(ed.selection_anchor.is_some());
    }

    #[test]
    fn multi_cursor_select_move_left_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.extra_cursors.push((1, 2));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_select_move_left();
        assert_eq!(ed.cursor_col, 2);
        assert!(ed.selection_anchor.is_some());
    }

    #[test]
    fn multi_cursor_move_word_left_basic() {
        let mut ed = Editor::test_with_text("hello world\nfoo bar\n");
        ed.cursor_line = 0;
        ed.cursor_col = 11;
        ed.extra_cursors.push((1, 7));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_move_word_left();
        assert_eq!(ed.cursor_col, 6); // start of "world"
    }

    #[test]
    fn multi_cursor_move_word_right_basic() {
        let mut ed = Editor::test_with_text("hello world\nfoo bar\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.extra_cursors.push((1, 0));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_move_word_right();
        assert_eq!(ed.cursor_col, 5); // end of "hello"
    }

    #[test]
    fn multi_cursor_move_to_line_start_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.extra_cursors.push((1, 4));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_move_to_line_start();
        assert_eq!(ed.cursor_col, 0);
        assert_eq!(ed.extra_cursors[0], (1, 0));
    }

    #[test]
    fn multi_cursor_move_to_line_end_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.extra_cursors.push((1, 0));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_move_to_line_end();
        assert_eq!(ed.cursor_col, 5);
        assert_eq!(ed.extra_cursors[0], (1, 5));
    }

    #[test]
    fn multi_cursor_select_move_up_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\nthird\n");
        ed.cursor_line = 1;
        ed.cursor_col = 2;
        ed.extra_cursors.push((2, 3));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_select_move_up();
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.extra_cursors[0].0, 1);
    }

    #[test]
    fn multi_cursor_select_move_down_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\nthird\n");
        ed.cursor_line = 0;
        ed.cursor_col = 2;
        ed.extra_cursors.push((1, 3));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_select_move_down();
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.extra_cursors[0].0, 2);
    }

    #[test]
    fn multi_cursor_select_move_word_left_basic() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 11;
        ed.extra_cursors.push((0, 5));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_select_move_word_left();
        // Cursor should move word left from positions
        assert!(ed.cursor_col < 11);
    }

    #[test]
    fn multi_cursor_select_move_word_right_basic() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.extra_cursors.push((0, 6));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_select_move_word_right();
        assert!(ed.cursor_col > 0);
    }

    #[test]
    fn multi_cursor_select_move_to_line_start_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.extra_cursors.push((1, 4));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_select_move_to_line_start();
        assert_eq!(ed.cursor_col, 0);
        assert_eq!(ed.extra_cursors[0].1, 0);
    }

    #[test]
    fn multi_cursor_select_move_to_line_end_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.extra_cursors.push((1, 0));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_select_move_to_line_end();
        assert_eq!(ed.cursor_col, 5);
        assert_eq!(ed.extra_cursors[0].1, 5);
    }

    #[test]
    fn multi_cursor_delete_word_left_basic() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.extra_cursors.push((0, 11));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_delete_word_left();
        // Both words should be partially deleted
        let text = ed.rope.to_string();
        assert!(text.len() < "hello world\n".len());
    }

    #[test]
    fn multi_cursor_delete_word_right_basic() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.extra_cursors.push((0, 6));
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_delete_word_right();
        let text = ed.rope.to_string();
        // "hello " and "world" should be deleted, leaving "\n"
        assert!(text.len() < "hello world\n".len());
    }

    #[test]
    fn multi_cursor_delete_word_right_at_eol() {
        let mut ed = Editor::test_with_text("aaa\nbbb\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3; // end of first line
        ed.extra_cursors.push((1, 3)); // end of second line
        ed.extra_cursor_anchors.push(None);
        ed.multi_cursor_delete_word_right();
        // Should join lines
        let text = ed.rope.to_string();
        assert!(text.len() < "aaa\nbbb\n".len());
    }

    #[test]
    fn add_cursor_above_clamps_column_to_shorter_line() {
        let mut ed = Editor::test_with_text("hi\nthis is a long line\n");
        ed.cursor_line = 1;
        ed.cursor_col = 18; // column 18 on "this is a long line"
        ed.add_cursor_above();
        assert_eq!(ed.extra_cursors.len(), 1);
        // Line 0 "hi" only has 2 chars, so column should be clamped
        assert_eq!(ed.extra_cursors[0].0, 0);
        assert!(ed.extra_cursors[0].1 <= 2, "cursor_col {} should be <= 2", ed.extra_cursors[0].1);
    }

    #[test]
    fn add_cursor_below_clamps_column_to_shorter_line() {
        let mut ed = Editor::test_with_text("this is a long line\nhi\n");
        ed.cursor_line = 0;
        ed.cursor_col = 18;
        ed.add_cursor_below();
        assert_eq!(ed.extra_cursors.len(), 1);
        assert_eq!(ed.extra_cursors[0].0, 1);
        assert!(ed.extra_cursors[0].1 <= 2, "cursor_col {} should be <= 2", ed.extra_cursors[0].1);
    }

    #[test]
    fn multi_cursor_distribute_paste_with_selections() {
        let mut ed = Editor::test_with_text("aaa bbb\nccc ddd\n");
        // Select "aaa" on line 0 (cursor at col 3, anchor at col 0)
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.selection_anchor = Some((0, 0));
        // Select "ccc" on line 1
        ed.extra_cursors = vec![(1, 3)];
        ed.extra_cursor_anchors = vec![Some((1, 0))];
        // Paste 2 lines (one per cursor) — should replace selections
        ed.paste_with_text("xxx\nyyy");
        let text = ed.rope.to_string();
        assert_eq!(text, "xxx bbb\nyyy ddd\n");
    }

    #[test]
    fn multi_cursor_paste_same_text_with_selections() {
        let mut ed = Editor::test_with_text("aaa bbb\nccc ddd\n");
        // Select "aaa" on line 0
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.selection_anchor = Some((0, 0));
        // Select "ccc" on line 1
        ed.extra_cursors = vec![(1, 3)];
        ed.extra_cursor_anchors = vec![Some((1, 0))];
        // Paste single line — replaces each selection with same text
        ed.paste_with_text("ZZ");
        let text = ed.rope.to_string();
        assert_eq!(text, "ZZ bbb\nZZ ddd\n");
    }

    // ---- Multi-cursor backspace at col 0 (line joining) tests ----

    #[test]
    fn multi_cursor_backspace_joins_lines() {
        let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.extra_cursors = vec![(2, 0)];
        ed.extra_cursor_anchors = vec![None];
        ed.multi_cursor_backspace();
        assert_eq!(ed.rope.to_string(), "aaabbbccc\n");
    }

    #[test]
    fn multi_cursor_backspace_same_line_mixed() {
        let mut ed = Editor::test_with_text("hello world");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.extra_cursors = vec![(0, 10)];
        ed.extra_cursor_anchors = vec![None];
        ed.multi_cursor_backspace();
        assert_eq!(ed.rope.to_string(), "hell word");
    }

    #[test]
    fn multi_cursor_backspace_at_file_start() {
        let mut ed = Editor::test_with_text("abc\ndef\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.extra_cursors = vec![(1, 2)];
        ed.extra_cursor_anchors = vec![None];
        ed.multi_cursor_backspace();
        assert_eq!(ed.rope.line(0).to_string(), "abc\n");
        assert_eq!(ed.rope.line(1).to_string(), "df\n");
    }

    #[test]
    fn multi_cursor_delete_at_line_end() {
        let mut ed = Editor::test_with_text("aaa\nbbb\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.extra_cursors = vec![];
        ed.extra_cursor_anchors = vec![];
        ed.multi_cursor_delete();
        assert_eq!(ed.rope.to_string(), "aaabbb\n");
    }

    #[test]
    fn multi_cursor_move_up_word_wrap() {
        let mut ed = Editor::new();
        // A long line that will wrap at width 10: "0123456789ABCDE\n" wraps into 2 rows
        ed.rope = Rope::from_str("0123456789ABCDE\nshort\n");
        ed.word_wrap = true;
        ed.viewport_width = 10;
        // Primary cursor on line 1 col 2 ("short")
        ed.cursor_line = 1;
        ed.cursor_col = 2;
        // Extra cursor on line 0 col 12 (in wrapped segment 1: "ABCDE", visual col 2)
        ed.extra_cursors = vec![(0, 12)];
        ed.extra_cursor_anchors = vec![None];
        ed.multi_cursor_move_up();
        // Primary cursor should move to wrapped row above line 1 = line 0 row 1 (cols 10-14)
        // visual col 2 → col 12
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 12);
        // Extra cursor at (0,12) in wrapped row 1, moving up goes to row 0 (cols 0-9)
        // visual col 2 → col 2
        assert_eq!(ed.extra_cursors[0], (0, 2));
    }

    #[test]
    fn multi_cursor_move_down_word_wrap() {
        let mut ed = Editor::test_with_text("0123456789ABCDE\nshort\n");
        ed.word_wrap = true;
        ed.viewport_width = 10;
        // Primary cursor on line 0 col 3 (in first wrapped segment)
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        // Extra cursor on line 0 col 12 (in second wrapped segment)
        ed.extra_cursors = vec![(0, 12)];
        ed.extra_cursor_anchors = vec![None];
        ed.multi_cursor_move_down();
        // Primary cursor at row 0 moves to row 1 (line 0 second segment, cols 10-14)
        // visual col 3 → col 13
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 13);
        // Extra cursor at row 1 (line 0 second segment) moves to row 2 (line 1 "short")
        // visual col 2 → col 2
        assert_eq!(ed.extra_cursors[0], (1, 2));
    }

    // ── add_cursor_below stacks multiple cursors ──

    #[test]
    fn add_cursor_below_multiple_times() {
        let mut ed = Editor::test_with_text("aaa\nbbb\nccc\nddd\n");
        ed.cursor_line = 0;
        ed.cursor_col = 1;
        ed.add_cursor_below();
        ed.add_cursor_below();
        ed.add_cursor_below();
        assert_eq!(ed.extra_cursors.len(), 3);
        assert_eq!(ed.extra_cursors[0], (1, 1));
        assert_eq!(ed.extra_cursors[1], (2, 1));
        assert_eq!(ed.extra_cursors[2], (3, 1));
    }

    // ── add_cursor_above stacks multiple cursors ──

    #[test]
    fn add_cursor_above_multiple_times() {
        let mut ed = Editor::test_with_text("aaa\nbbb\nccc\nddd\n");
        ed.cursor_line = 3;
        ed.cursor_col = 1;
        ed.add_cursor_above();
        ed.add_cursor_above();
        assert_eq!(ed.extra_cursors.len(), 2);
        assert_eq!(ed.extra_cursors[0], (2, 1));
        assert_eq!(ed.extra_cursors[1], (1, 1));
    }

    // ── add_cursor_below at bottom is no-op ──

    #[test]
    fn add_cursor_below_at_bottom_no_new_line() {
        let mut ed = Editor::test_with_text("aaa\nbbb\n");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        let before = ed.extra_cursors.len();
        ed.add_cursor_below();
        // There's still an empty line 2 (after trailing newline), so a cursor is added
        // but on a line with nothing useful. The cursor count increases by 1.
        assert_eq!(ed.extra_cursors.len(), before + 1);
    }

    // ── multi_cursor_insert_char on multiple lines ──

    #[test]
    fn multi_cursor_insert_char_on_different_lines() {
        let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
        ed.cursor_line = 0;
        ed.cursor_col = 1;
        ed.extra_cursors = vec![(1, 1), (2, 1)];
        ed.extra_cursor_anchors = vec![None, None];
        ed.multi_cursor_insert_char('X');
        assert_eq!(ed.rope.line(0).to_string(), "aXaa\n");
        assert_eq!(ed.rope.line(1).to_string(), "bXbb\n");
        assert_eq!(ed.rope.line(2).to_string(), "cXcc\n");
    }

    // ── multi_cursor_backspace on different lines ──

    #[test]
    fn multi_cursor_backspace_on_different_lines() {
        let mut ed = Editor::test_with_text("abc\ndef\nghi\n");
        ed.cursor_line = 0;
        ed.cursor_col = 2;
        ed.extra_cursors = vec![(1, 2), (2, 2)];
        ed.extra_cursor_anchors = vec![None, None];
        ed.multi_cursor_backspace();
        assert_eq!(ed.rope.line(0).to_string(), "ac\n");
        assert_eq!(ed.rope.line(1).to_string(), "df\n");
        assert_eq!(ed.rope.line(2).to_string(), "gi\n");
    }
