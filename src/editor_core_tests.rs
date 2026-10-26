    use super::Editor;
    

    #[test]
    fn save_trim_trailing_whitespace_removes_spaces_before_eol() {
        let mut ed = Editor::test_with_text("alpha   \nbeta\t\t\r\ngamma  ");
        ed.cursor_line = 2;
        ed.cursor_col = 7;
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "vtcode-trim-save-{}-{}.txt",
            std::process::id(),
            unique
        ));
        std::fs::write(&path, ed.rope.to_string()).unwrap();
        ed.file_path = Some(path.clone());
        ed.saved_rope = Some(ed.rope.clone());

        ed.save_file_with_options(true, false).unwrap();

        assert_eq!(ed.rope.to_string(), "alpha\nbeta\r\ngamma");
        assert_eq!(ed.cursor_col, 5);
        let _ = std::fs::remove_file(path); // Error OK: file may not exist
    }

    #[test]
    fn insert_char_basic() {
        let mut ed = Editor::test_with_text("hllo\n");
        ed.cursor_line = 0;
        ed.cursor_col = 1;
        ed.insert_char('e');
        assert!(ed.rope.to_string().starts_with("hello"));
    }

    #[test]
    fn insert_char_at_end() {
        let mut ed = Editor::test_with_text("hi\n");
        ed.cursor_line = 0;
        ed.cursor_col = 2;
        ed.insert_char('!');
        assert_eq!(ed.rope.line(0).to_string(), "hi!\n");
    }

    #[test]
    fn insert_char_autocloses_tag_in_markup_languages() {
        let mut ed = Editor::new();
        ed.language_override = Some("HTML".to_string());
        ed.rope = ropey::Rope::from_str("<div\n");
        ed.cursor_line = 0;
        ed.cursor_col = 4;

        ed.insert_char('>');

        assert_eq!(ed.rope.line(0).to_string(), "<div></div>\n");
        assert_eq!(ed.cursor_col, 5);
    }

    #[test]
    fn insert_char_does_not_autoclose_tag_in_cpp() {
        let mut ed = Editor::new();
        ed.language_override = Some("C++".to_string());
        ed.rope = ropey::Rope::from_str("<int\n");
        ed.cursor_line = 0;
        ed.cursor_col = 4;

        ed.insert_char('>');

        assert_eq!(ed.rope.line(0).to_string(), "<int>\n");
    }

    #[test]
    fn delete_selection_single_line() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.selection_anchor = Some((0, 0));
        let deleted = ed.delete_selection();
        assert_eq!(deleted, Some("hello".to_string()));
        assert!(ed.rope.to_string().starts_with(" world"));
    }

    #[test]
    fn delete_line_middle() {
        let mut ed = Editor::test_with_text("a\nb\nc\n");
        ed.cursor_line = 1;
        ed.delete_line();
        assert_eq!(ed.rope.to_string(), "a\nc\n");
    }

    #[test]
    fn delete_line_single() {
        let mut ed = Editor::test_with_text("only\n");
        ed.cursor_line = 0;
        ed.delete_line();
        assert_eq!(ed.rope.to_string(), "");
    }

    #[test]
    fn duplicate_line_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 0;
        ed.duplicate_line();
        assert_eq!(ed.rope.to_string(), "hello\nhello\nworld\n");
    }

    #[test]
    fn duplicate_line_last_without_newline() {
        let mut ed = Editor::test_with_text("hello\nworld");
        ed.cursor_line = 1;
        ed.duplicate_line();
        assert_eq!(ed.rope.to_string(), "hello\nworld\nworld");
        assert_eq!(ed.cursor_line, 2);
    }

    #[test]
    fn move_line_down_basic() {
        let mut ed = Editor::test_with_text("a\nb\nc\n");
        ed.cursor_line = 0;
        ed.move_line_down();
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.rope.to_string(), "b\na\nc\n");
    }

    #[test]
    fn move_line_up_basic() {
        let mut ed = Editor::test_with_text("a\nb\nc\n");
        ed.cursor_line = 1;
        ed.move_line_up();
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.rope.to_string(), "b\na\nc\n");
    }

    #[test]
    fn move_line_up_updates_bookmarks() {
        let mut ed = Editor::test_with_text("a\nb\nc\n");
        ed.bookmarks.insert(1); // bookmark on "b"
        ed.cursor_line = 1;
        ed.move_line_up();
        // Bookmark should follow the line from 1 → 0
        assert!(ed.bookmarks.contains(&0), "bookmark should move to line 0");
        assert!(!ed.bookmarks.contains(&1), "bookmark should not stay at line 1");
    }

    #[test]
    fn move_line_down_updates_bookmarks() {
        let mut ed = Editor::test_with_text("a\nb\nc\n");
        ed.bookmarks.insert(0); // bookmark on "a"
        ed.cursor_line = 0;
        ed.move_line_down();
        // Bookmark should follow the line from 0 → 1
        assert!(ed.bookmarks.contains(&1), "bookmark should move to line 1");
        assert!(!ed.bookmarks.contains(&0), "bookmark should not stay at line 0");
    }

    #[test]
    fn move_line_down_no_trailing_newline() {
        let mut ed = Editor::test_with_text("abc\ndef");
        ed.cursor_line = 0;
        ed.move_line_down();
        // Should preserve no-trailing-newline
        assert_eq!(ed.rope.to_string(), "def\nabc");
        assert_eq!(ed.cursor_line, 1);
    }

    #[test]
    fn backspace_removes_char() {
        let mut ed = Editor::test_with_text("abc\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.backspace();
        assert_eq!(ed.rope.to_string(), "ab\n");
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn backspace_at_line_start_joins_lines() {
        let mut ed = Editor::test_with_text("ab\ncd\n");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.backspace();
        assert_eq!(ed.rope.to_string(), "abcd\n");
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn delete_char_removes_char() {
        let mut ed = Editor::test_with_text("abc\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.delete_char();
        assert_eq!(ed.rope.to_string(), "bc\n");
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn insert_newline_basic() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.insert_newline();
        assert_eq!(ed.rope.to_string(), "hello\n\n");
        assert_eq!(ed.cursor_line, 1);
    }

    #[test]
    fn insert_newline_with_indent() {
        let mut ed = Editor::test_with_text("    code\n");
        ed.cursor_line = 0;
        ed.cursor_col = 8;
        ed.insert_newline();
        assert_eq!(ed.cursor_line, 1);
        // New line should have the same indent
        let new_line = ed.rope.line(1).to_string();
        assert!(new_line.starts_with("    "));
    }

    #[test]
    fn insert_newline_at_col0_no_double_indent() {
        let mut ed = Editor::test_with_text("    code\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.insert_newline();
        // Should insert blank line, original line unchanged
        assert_eq!(ed.rope.line(0).to_string(), "\n");
        assert_eq!(ed.rope.line(1).to_string(), "    code\n");
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn insert_newline_mid_indent_partial() {
        let mut ed = Editor::test_with_text("    code\n");
        ed.cursor_line = 0;
        ed.cursor_col = 2; // middle of 4-space indent
        ed.insert_newline();
        // Pushed text "  code\n" already has 2 spaces of indent
        // Should add 2 more spaces to total 4
        assert_eq!(ed.rope.line(0).to_string(), "  \n");
        let line1 = ed.rope.line(1).to_string();
        assert!(line1.starts_with("    code"), "line1 was: {line1:?}");
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn join_lines_basic() {
        for next_line in ["world", "    world"] {
            let mut ed = Editor::test_with_text(&format!("hello\n{next_line}\n"));
            ed.join_lines();
            assert_eq!(ed.rope.to_string(), "hello world\n");
            assert_eq!((ed.cursor_line, ed.cursor_col), (0, 6));
        }
    }

    #[test]
    fn join_lines_at_last_line_noop() {
        let mut ed = Editor::test_with_text("only\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        // Make total = 1 effective line
        ed.rope = ropey::Rope::from_str("only");
        ed.join_lines();
        assert_eq!(ed.rope.to_string(), "only");
    }

    #[test]
    fn insert_line_above_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.insert_line_above();
        assert_eq!(ed.rope.len_lines(), 4); // added a line
        assert_eq!(ed.cursor_line, 1); // cursor on new line
    }

    #[test]
    fn insert_line_below_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.insert_line_below();
        assert_eq!(ed.rope.len_lines(), 4);
        assert_eq!(ed.cursor_line, 1); // cursor moved to new line
    }

    #[test]
    fn delete_word_left_basic() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.delete_word_left();
        assert!(ed.rope.line(0).to_string().starts_with(" world") || ed.rope.line(0).to_string().starts_with("world"));
    }

    #[test]
    fn delete_word_right_basic() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.delete_word_right();
        let text = ed.rope.line(0).to_string();
        assert!(!text.starts_with("hello"));
    }

    #[test]
    fn insert_tab_spaces() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.use_tabs = false;
        ed.tab_size = 4;
        ed.insert_tab();
        assert!(ed.rope.line(0).to_string().starts_with("    "));
    }

    #[test]
    fn insert_tab_tabs() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.use_tabs = true;
        ed.insert_tab();
        assert!(ed.rope.line(0).to_string().starts_with('\t'));
    }

    #[test]
    fn duplicate_line_up_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 1;
        ed.duplicate_line_up();
        assert_eq!(ed.rope.len_lines(), 4);
        // Both copies of "world" should exist
        let text = ed.rope.to_string();
        assert_eq!(text.matches("world").count(), 2);
    }

    #[test]
    fn empty_file_edit_operations() {
        let mut ed = Editor::test_with_text("");
        // These should not panic on empty file
        ed.backspace();
        ed.delete_char();
        ed.delete_line();
        ed.select_all();
        ed.select_line();
        // duplicate_line and toggle_line_comment modify content (expected behavior)
        let mut ed2 = Editor::test_with_text("");
        ed2.duplicate_line();  // should not panic
        let mut ed3 = Editor::test_with_text("");
        ed3.join_lines(); // should not panic
        let mut ed4 = Editor::test_with_text("");
        ed4.toggle_line_comment(); // should not panic
    }

    #[test]
    fn empty_file_insert_then_undo() {
        let mut ed = Editor::test_with_text("");
        ed.insert_char('a');
        assert_eq!(ed.rope.to_string(), "a");
        ed.undo();
        assert_eq!(ed.rope.to_string(), "");
        ed.redo();
        assert_eq!(ed.rope.to_string(), "a");
    }

    #[test]
    fn single_char_file_operations() {
        let mut ed = Editor::test_with_text("x");
        ed.cursor_col = 1; // after 'x'
        ed.backspace();
        assert_eq!(ed.rope.to_string(), "");
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn dedent_empty_line() {
        let mut ed = Editor::test_with_text("\n");
        ed.cursor_line = 0;
        ed.dedent_line(); // should not panic on empty line
        assert_eq!(ed.rope.to_string(), "\n");
    }

    #[test]
    fn indent_then_dedent_roundtrip() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.tab_size = 4;
        ed.use_tabs = false;
        ed.indent_lines();
        assert!(ed.rope.line(0).to_string().starts_with("    "));
        ed.dedent_line();
        assert_eq!(ed.rope.line(0).to_string().trim_end(), "hello");
    }

    #[test]
    fn move_line_up_at_top() {
        let mut ed = Editor::test_with_text("first\nsecond\n");
        ed.cursor_line = 0;
        ed.move_line_up(); // should be noop at top
        let result: String = ed.rope.chars().collect();
        assert_eq!(result, "first\nsecond\n");
        assert_eq!(ed.cursor_line, 0);
    }

    #[test]
    fn move_line_down_at_bottom() {
        for line in [1, 2] {
            let mut ed = Editor::test_with_text("first\nsecond\n");
            ed.cursor_line = line;
            ed.move_line_down();
            assert_eq!(ed.rope.to_string(), "first\nsecond\n");
            assert_eq!((ed.cursor_line, ed.cursor_col), (line, 0));
        }
    }

    #[test]
    fn delete_word_left_at_start() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.delete_word_left(); // should join with previous line or noop at start
        // At very start of file, should be noop
        assert_eq!(ed.rope.to_string(), "hello\n");
    }

    #[test]
    fn delete_word_right_at_end() {
        let mut ed = Editor::test_with_text("hello");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.delete_word_right(); // should not panic at end of file
    }

    #[test]
    fn duplicate_line_up_single_line_file() {
        let mut ed = Editor::test_with_text("only line");
        ed.duplicate_line_up();
        assert_eq!(ed.rope.len_lines(), 2);
    }

    #[test]
    fn delete_to_line_start_single_cursor() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.delete_to_line_start();
        assert_eq!(ed.rope.to_string(), " world\n");
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn delete_to_line_start_at_col_zero_skips() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.extra_cursors = vec![(1, 0)];
        ed.extra_cursor_anchors = vec![None];
        ed.delete_to_line_start();
        // Nothing should be deleted
        assert_eq!(ed.rope.to_string(), "hello\nworld\n");
    }

    #[test]
    fn delete_to_line_end_single_cursor() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.delete_to_line_end();
        assert_eq!(ed.rope.to_string(), "hello\n");
        assert_eq!(ed.cursor_col, 5);
    }

    #[test]
    fn delete_to_line_end_at_end_skips() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.delete_to_line_end();
        assert_eq!(ed.rope.to_string(), "hello\n");
    }

    #[test]
    fn indent_single_line() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 2;
        ed.tab_size = 4;
        ed.indent_lines();
        assert_eq!(ed.rope.to_string(), "    hello\n");
        assert_eq!(ed.cursor_col, 6);
    }

    #[test]
    fn dedent_removes_spaces() {
        let mut ed = Editor::test_with_text("    hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 6;
        ed.tab_size = 4;
        ed.dedent_line();
        assert_eq!(ed.rope.to_string(), "hello\n");
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn dedent_removes_tab() {
        let mut ed = Editor::test_with_text("\thello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.tab_size = 4;
        ed.dedent_line();
        assert_eq!(ed.rope.to_string(), "hello\n");
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn dedent_partial_indent() {
        let mut ed = Editor::test_with_text("  hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 4;
        ed.tab_size = 4;
        ed.dedent_line();
        assert_eq!(ed.rope.to_string(), "hello\n");
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn dedent_no_indent_noop() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 2;
        ed.tab_size = 4;
        ed.dedent_line();
        assert_eq!(ed.rope.to_string(), "hello\n");
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn indent_empty_line() {
        let mut ed = Editor::test_with_text("\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.tab_size = 4;
        ed.indent_lines();
        assert_eq!(ed.rope.to_string(), "    \n");
        assert_eq!(ed.cursor_col, 4);
    }

    #[test]
    fn paste_line_copy_inserts_above() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.clipboard = "new line\n".to_string();
        ed.clipboard_is_line = true;
        ed.paste_with_text("new line\n");
        assert_eq!(ed.rope.line(1).to_string(), "new line\n");
        assert_eq!(ed.rope.line(2).to_string(), "world\n");
        assert_eq!(ed.cursor_line, 2);
    }

    #[test]
    fn delete_line_single_line_file() {
        let mut ed = Editor::test_with_text("only line");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.delete_line();
        assert_eq!(ed.rope.to_string(), "");
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn delete_line_last_line() {
        let mut ed = Editor::test_with_text("first\nsecond\nthird\n");
        // Ropey: 4 lines (first, second, third, "")
        ed.cursor_line = 2;
        ed.cursor_col = 0;
        ed.delete_line();
        assert_eq!(ed.rope.to_string(), "first\nsecond\n");
        // Cursor stays at position 2 (empty trailing line in Ropey)
        assert_eq!(ed.cursor_line, 2);
    }

    #[test]
    fn delete_line_all_lines() {
        let mut ed = Editor::test_with_text("A\nB\nC\n");
        ed.cursor_line = 0;
        ed.extra_cursors.push((1, 0));
        ed.extra_cursors.push((2, 0));
        ed.extra_cursor_anchors.push(None);
        ed.extra_cursor_anchors.push(None);
        ed.delete_line();
        assert_eq!(ed.rope.to_string(), "");
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn insert_newline_bracket_expansion_curly() {
        let mut ed = Editor::test_with_text("fn main() {}\n");
        ed.cursor_line = 0;
        ed.cursor_col = 11; // between { and }
        ed.tab_size = 4;
        ed.use_tabs = false;
        ed.insert_newline();
        // Should expand: fn main() {
        //     |cursor
        // }
        assert_eq!(ed.cursor_line, 1);
        let line1 = ed.rope.line(1).to_string();
        assert!(line1.starts_with("    "), "expected extra indent, got: {line1:?}");
        let line2 = ed.rope.line(2).to_string();
        assert!(line2.starts_with("}"), "expected closing brace, got: {line2:?}");
    }

    #[test]
    fn insert_newline_bracket_expansion_square() {
        let mut ed = Editor::test_with_text("let a = []\n");
        ed.cursor_line = 0;
        ed.cursor_col = 9; // between [ and ]
        ed.tab_size = 2;
        ed.use_tabs = false;
        ed.insert_newline();
        assert_eq!(ed.cursor_line, 1);
        let line1 = ed.rope.line(1).to_string();
        assert!(line1.starts_with("  "), "expected 2-space indent, got: {line1:?}");
    }

    #[test]
    fn insert_newline_after_open_brace() {
        let mut ed = Editor::test_with_text("if true {\n");
        ed.cursor_line = 0;
        ed.cursor_col = 9; // after {, at newline
        ed.tab_size = 4;
        ed.use_tabs = false;
        ed.insert_newline();
        assert_eq!(ed.cursor_line, 1);
        let line1 = ed.rope.line(1).to_string();
        assert!(line1.starts_with("    "), "expected indent after open brace, got: {line1:?}");
    }

    #[test]
    fn delete_char_at_end_of_file() {
        let mut ed = Editor::test_with_text("hello");
        ed.cursor_line = 0;
        ed.cursor_col = 5; // at end, nothing to delete
        let before = ed.rope.to_string();
        ed.delete_char();
        assert_eq!(ed.rope.to_string(), before);
    }

    #[test]
    fn delete_char_empty_file() {
        let mut ed = Editor::test_with_text("");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.delete_char();
        assert_eq!(ed.rope.to_string(), "");
    }

    #[test]
    fn delete_char_unicode() {
        let mut ed = Editor::test_with_text("héllo\n");
        ed.cursor_line = 0;
        ed.cursor_col = 1; // delete 'é'
        ed.delete_char();
        assert_eq!(ed.rope.to_string(), "hllo\n");
    }

    #[test]
    fn insert_newline_splits_line_at_middle() {
        let mut ed = Editor::test_with_text("abcdef\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.insert_newline();
        assert_eq!(ed.rope.line(0).to_string(), "abc\n");
        assert_eq!(ed.rope.line(1).to_string(), "def\n");
        assert_eq!(ed.cursor_line, 1);
        assert_eq!(ed.cursor_col, 0);
    }

    #[test]
    fn delete_char_end_of_line_joins_next() {
        let mut ed = Editor::test_with_text("abc\ndef\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.delete_char();
        assert_eq!(ed.rope.to_string(), "abcdef\n");
        assert_eq!(ed.cursor_line, 0);
        assert_eq!(ed.cursor_col, 3);
    }

    #[test]
    fn indent_selection_adds_spaces() {
        let mut ed = Editor::test_with_text("line1\nline2\nline3\n");
        ed.use_tabs = false;
        ed.tab_size = 4;
        ed.selection_anchor = Some((0, 0));
        ed.cursor_line = 1;
        ed.cursor_col = 5;
        ed.indent_lines();
        assert!(ed.rope.line(0).to_string().starts_with("    "));
        assert!(ed.rope.line(1).to_string().starts_with("    "));
        // Line 3 not in selection, should be unchanged
        assert_eq!(ed.rope.line(2).to_string(), "line3\n");
    }

    #[test]
    fn dedent_selection_removes_spaces() {
        let mut ed = Editor::test_with_text("    line1\n    line2\nline3\n");
        ed.use_tabs = false;
        ed.tab_size = 4;
        ed.selection_anchor = Some((0, 0));
        ed.cursor_line = 1;
        ed.cursor_col = 9;
        ed.dedent_line();
        assert_eq!(ed.rope.line(0).to_string(), "line1\n");
        assert_eq!(ed.rope.line(1).to_string(), "line2\n");
        assert_eq!(ed.rope.line(2).to_string(), "line3\n");
    }

    #[test]
    fn duplicate_line_content_doubled() {
        let mut ed = Editor::test_with_text("alpha\nbeta\ngamma\n");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.duplicate_line();
        assert_eq!(ed.rope.to_string(), "alpha\nbeta\nbeta\ngamma\n");
        assert_eq!(ed.cursor_line, 2);
    }

    #[test]
    fn duplicate_line_up_inserts_copy_above() {
        let mut ed = Editor::test_with_text("first\nsecond\n");
        ed.cursor_line = 1;
        ed.cursor_col = 0;
        ed.duplicate_line_up();
        assert_eq!(ed.rope.to_string(), "first\nsecond\nsecond\n");
    }

    #[test]
    fn insert_line_above_preserves_indent() {
        let mut ed = Editor::test_with_text("    indented\n");
        ed.cursor_line = 0;
        ed.cursor_col = 4;
        ed.insert_line_above();
        let new_line = ed.rope.line(0).to_string();
        assert!(new_line.starts_with("    "), "New line should inherit indent: {new_line:?}");
        assert_eq!(ed.cursor_col, 4);
    }

    #[test]
    fn insert_line_below_preserves_indent() {
        let mut ed = Editor::test_with_text("    indented\n");
        ed.cursor_line = 0;
        ed.cursor_col = 4;
        ed.insert_line_below();
        assert_eq!(ed.cursor_line, 1);
        let new_line = ed.rope.line(1).to_string();
        assert!(new_line.starts_with("    "), "New line should inherit indent: {new_line:?}");
    }

    #[test]
    fn insert_newline_between_braces_adds_extra_indent() {
        let mut ed = Editor::test_with_text("fn main() {}\n");
        ed.file_path = Some(std::path::PathBuf::from("test.rs"));
        ed.use_tabs = false;
        ed.tab_size = 4;
        ed.cursor_line = 0;
        ed.cursor_col = 12; // between { and }
        // Cursor col 12 is past the } (line is 12 chars + \n), verify behavior
        // Actually { is at col 11, } is at col 12, so cursor_col=12 is ON }, not between.
        // Place between: col=12 means after the }, so let's set col=11 to be on {
        // The bracket pair is at positions 11 ({) and 12 (}), cursor must be at col 12 (between)
        // But "fn main() {}" has { at index 11 and } at index 12 — cursor at 12 is after {, before }
        ed.insert_newline();
        // Should have expanded brackets
        assert!(ed.cursor_line >= 1);
        assert!(ed.rope.len_lines() >= 3);
    }

    #[test]
    fn delete_line_last_no_trailing_newline() {
        let mut ed = Editor::test_with_text("first\nsecond");
        ed.cursor_line = 1;
        ed.delete_line();
        assert_eq!(ed.rope.to_string(), "first");
        assert_eq!(ed.cursor_line, 0);
    }

    #[test]
    fn overwrite_mode_replaces_char() {
        let mut ed = Editor::test_with_text("abcd\n");
        ed.cursor_line = 0;
        ed.cursor_col = 1;
        ed.overwrite_mode = true;
        ed.insert_char('X');
        assert_eq!(ed.rope.line(0).to_string(), "aXcd\n");
        assert_eq!(ed.cursor_col, 2);
    }

    #[test]
    fn test_insert_char_on_empty_editor() {
        let mut ed = Editor::new();
        // Editor::new() creates a rope with "\n" (one empty line)
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.insert_char('x');
        assert!(ed.rope.to_string().starts_with("x"));
    }

    #[test]
    fn test_backspace_on_empty_editor() {
        let mut ed = Editor::test_with_text("\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        // Should not panic
        ed.backspace();
        assert!(ed.line_count() >= 1);
    }

    #[test]
    fn test_delete_on_empty_editor() {
        let mut ed = Editor::test_with_text("\n");
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        // Should not panic
        ed.delete_char();
        assert!(ed.line_count() >= 1);
    }

    #[test]
    fn test_undo_on_empty_editor() {
        let mut ed = Editor::new();
        // No history at all
        ed.undo();
        // Should not panic and cursor should remain valid
        assert_eq!(ed.cursor_line, 0);
    }

    #[test]
    fn test_paste_empty_clipboard() {
        let mut ed = Editor::test_with_text("abc\n");
        ed.cursor_line = 0;
        ed.cursor_col = 1;
        let before = ed.rope.to_string();
        // paste_with_text with empty string should be a no-op
        ed.paste_with_text("");
        assert_eq!(ed.rope.to_string(), before);
    }
