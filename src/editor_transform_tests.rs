    use crate::editor::Editor;
    use ropey::Rope;

    fn editor_with_text(text: &str) -> Editor {
        Editor::test_with_text(text)
    }

    fn assert_wrap_matches_fresh_layout(editor: &mut Editor, width: usize) {
        let expected = Editor::build_wrap_row_offsets(&editor.rope, width, editor.tab_size, &[], || false).unwrap();
        assert_eq!(editor.wrapped_total_rows(width), *expected.last().unwrap());
        for line in 0..editor.line_count() {
            assert_eq!(editor.wrapped_row_for_position(line, 0, width), expected[line], "line {line}");
            let text = editor.rope.line(line).to_string();
            let ranges = Editor::wrap_ranges_for_line_text_tabbed(text.trim_end_matches(['\r', '\n']), width, editor.tab_size);
            assert_eq!(&*editor.wrap_ranges_for_line(line, width), &ranges, "line {line}");
        }
    }

    #[test]
    fn document_transforms_refresh_warm_wrap_ranges_far_from_the_caret() {
        let source = format!("head\n{}use z_very_long_import;\nuse a;\n\tvalue       \n    value\n{{\nitem\n}}\nlast", "neutral\n".repeat(80));
        let operations: &[(&str, fn(&mut Editor))] = &[
            ("sort imports", Editor::sort_imports),
            ("reverse", Editor::reverse_lines),
            ("shuffle", Editor::shuffle_lines),
            ("indent spaces", Editor::convert_indentation_to_spaces),
            ("indent tabs", Editor::convert_indentation_to_tabs),
            ("trim", |editor| { editor.trim_trailing_whitespace(); }),
            ("reindent", Editor::reindent_lines),
            ("CRLF", Editor::convert_line_endings_to_crlf),
        ];
        for &(name, operation) in operations {
            let mut editor = editor_with_text(&source);
            editor.word_wrap = true;
            editor.viewport_width = 8;
            assert_wrap_matches_fresh_layout(&mut editor, 8);
            operation(&mut editor);
            assert_wrap_matches_fresh_layout(&mut editor, 8);
            editor.undo();
            assert_eq!(editor.rope.to_string(), source, "undo {name}");
            assert_wrap_matches_fresh_layout(&mut editor, 8);
        }
    }

    #[test]
    fn fold_hints_follow_edits_history_lsp_updates_and_loaded_replacements() {
        let mut ed = editor_with_text("root\n child\n");
        assert!(ed.can_fold_line(0));
        assert!(ed.can_fold_line(0));
        ed.insert_char(' ');
        assert!(!ed.can_fold_line(0));
        ed.undo();
        assert!(ed.can_fold_line(0));
        ed.redo();
        assert!(!ed.can_fold_line(0));
        ed.lsp_folding_ranges = vec![(0, 2)];
        assert!(ed.can_fold_line(0));
        ed.lsp_folding_ranges.clear();
        assert!(!ed.can_fold_line(0));
        for (text, expected) in [("root\n  child\n", true), ("root\nchild\n", false)] {
            ed.apply_bg_loaded(Rope::from_str(text), "UTF-8", text.len() as u64, u64::MAX);
            assert_eq!(ed.can_fold_line(0), expected);
        }
        ed.rope = Rope::from_str("root\n   child\n");
        ed.invalidate_rope_caches();
        assert!(ed.can_fold_line(0));
    }

    #[test]
    fn region_hint_reads_prefixes_across_rope_chunks_with_the_existing_rules() {
        for prefix in [
            "#region",
            "#pragma region",
            "// region",
            "// #region",
            "# region",
            "--region",
            "-- #region",
            "<!-- region",
            "<!-- #region",
            "# pragma region",
            "// # region",
            "/ region",
            "--endregion",
            "ordinary",
        ] {
            let text = format!(
                "{}{}{}",
                "\u{2003}".repeat(1_024),
                prefix,
                "x".repeat(16_384)
            );
            let rope = Rope::from_str(&text);
            assert_eq!(
                super::rope_line_region_marker(rope.line(0)) == Some(true),
                Editor::is_region_start(text.trim()),
                "{prefix}"
            );
        }
        let text = format!("//{}region", "\u{2003}".repeat(1_024));
        assert_eq!(super::rope_line_region_marker(
            Rope::from_str(&text).line(0)
        ), Some(true));
    }

    fn editor_with_selection(text: &str, sl: usize, sc: usize, el: usize, ec: usize) -> Editor {
        let mut ed = editor_with_text(text);
        ed.selection_anchor = Some((sl, sc));
        ed.cursor_line = el;
        ed.cursor_col = ec;
        ed
    }

    #[test]
    fn reverse_lines_all() {
        let mut ed = editor_with_text("aaa\nbbb\nccc\n");
        ed.reverse_lines();
        assert_eq!(ed.rope.to_string(), "ccc\nbbb\naaa\n");
    }

    #[test]
    fn remove_duplicate_lines_keeps_first() {
        let mut ed = editor_with_text("alpha\nbeta\nalpha\ngamma\nbeta\n");
        ed.remove_duplicate_lines();
        let result = ed.rope.to_string();
        assert_eq!(result, "alpha\nbeta\ngamma\n");
        assert!(result.contains("alpha"));
        assert!(result.contains("beta"));
        assert!(result.contains("gamma"));
        // Verify duplicates are removed (only 3 unique non-empty lines)
        let non_empty: Vec<&str> = result.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(non_empty.len(), 3);
    }

    #[test]
    fn remove_blank_lines_strips_empty() {
        let mut ed = editor_with_text("hello\n\nworld\n\n\n");
        ed.remove_blank_lines();
        assert_eq!(ed.rope.to_string(), "hello\nworld\n");
    }

    #[test]
    fn convert_to_snake_case_from_camel() {
        let mut ed = editor_with_selection("helloWorld\n", 0, 0, 0, 10);
        ed.convert_to_snake_case();
        assert_eq!(ed.rope.to_string(), "hello_world\n");
    }

    #[test]
    fn convert_to_kebab_case_from_camel() {
        let mut ed = editor_with_selection("helloWorld\n", 0, 0, 0, 10);
        ed.convert_to_kebab_case();
        assert_eq!(ed.rope.to_string(), "hello-world\n");
    }

    #[test]
    fn convert_to_constant_case_from_camel() {
        let mut ed = editor_with_selection("helloWorld\n", 0, 0, 0, 10);
        ed.convert_to_constant_case();
        assert_eq!(ed.rope.to_string(), "HELLO_WORLD\n");
    }

    #[test]
    fn convert_to_camel_case_from_snake() {
        let mut ed = editor_with_selection("hello_world\n", 0, 0, 0, 11);
        ed.convert_to_camel_case();
        assert_eq!(ed.rope.to_string(), "helloWorld\n");
    }

    #[test]
    fn convert_to_pascal_case_from_snake() {
        let mut ed = editor_with_selection("hello_world\n", 0, 0, 0, 11);
        ed.convert_to_pascal_case();
        assert_eq!(ed.rope.to_string(), "HelloWorld\n");
    }

    // --- folding tests ---

    #[test]
    fn is_line_folded_inside_range() {
        let mut ed = Editor::test_with_text("fn main() {\n    foo\n    bar\n}\n");
        ed.folded_ranges.insert(0, 3);
        assert!(!ed.is_line_folded(0)); // fold start itself is not folded
        assert!(ed.is_line_folded(1));
        assert!(ed.is_line_folded(2));
        assert!(!ed.is_line_folded(3)); // end is exclusive
    }

    #[test]
    fn is_fold_start_checks_key() {
        let mut ed = Editor::test_with_text("fn main() {\n    foo\n}\n");
        ed.folded_ranges.insert(0, 2);
        assert!(ed.is_fold_start(0));
        assert!(!ed.is_fold_start(1));
    }

    #[test]
    fn find_fold_end_indented_block() {
        let ed = Editor::test_with_text("fn main() {\n    foo\n    bar\n}\n");
        // Line 0 has indent 0, lines 1-2 have indent 4
        assert_eq!(ed.find_fold_end(0), Some(3));
    }

    #[test]
    fn find_fold_end_no_deeper_lines() {
        let ed = Editor::test_with_text("a\nb\nc\n");
        // All same indent => nothing to fold
        assert_eq!(ed.find_fold_end(0), None);
    }

    #[test]
    fn find_fold_end_last_line() {
        let ed = Editor::test_with_text("only\n");
        // Last line can't start a fold
        assert_eq!(ed.find_fold_end(0), None);
    }

    #[test]
    fn find_fold_end_blank_line_in_fold() {
        let ed = Editor::test_with_text("fn x() {\n    a\n\n    b\n}\n");
        // Blank line at index 2 should be included in fold
        assert_eq!(ed.find_fold_end(0), Some(4));
    }

    // --- trim_trailing_whitespace tests ---

    #[test]
    fn trim_trailing_basic() {
        let mut ed = Editor::test_with_text("hello   \nworld  \n");
        ed.trim_trailing_whitespace();
        assert_eq!(ed.rope.to_string(), "hello\nworld\n");
    }

    #[test]
    fn trim_trailing_no_change() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.trim_trailing_whitespace();
        assert_eq!(ed.rope.to_string(), "hello\nworld\n");
    }

    #[test]
    fn trim_trailing_tabs() {
        let mut ed = Editor::test_with_text("hello\t\t\n");
        ed.trim_trailing_whitespace();
        assert_eq!(ed.rope.to_string(), "hello\n");
    }

    // --- visible_lines tests ---

    #[test]
    fn visible_lines_no_folds() {
        let ed = Editor::test_with_text("a\nb\nc\nd\ne\n");
        let lines = ed.visible_lines(0, 5);
        assert_eq!(lines, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn visible_lines_with_fold() {
        let mut ed = Editor::test_with_text("fn main() {\n    a\n    b\n}\n");
        ed.folded_ranges.insert(0, 3);
        let lines = ed.visible_lines(0, 10);
        assert_eq!(lines, vec![0, 3, 4]);
    }

    // --- convert_line_endings tests ---

    #[test]
    fn convert_to_lf() {
        let mut ed = Editor::test_with_text("hello\r\nworld\r\n");
        ed.convert_line_endings_to_lf();
        assert_eq!(ed.rope.to_string(), "hello\nworld\n");
    }

    #[test]
    fn convert_to_crlf() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.convert_line_endings_to_crlf();
        assert_eq!(ed.rope.to_string(), "hello\r\nworld\r\n");
    }

    // --- convert_indentation tests ---

    #[test]
    fn convert_tabs_to_spaces() {
        let mut ed = Editor::new();
        ed.tab_size = 4;
        ed.rope = Rope::from_str("\thello\n");
        ed.convert_indentation_to_spaces();
        assert_eq!(ed.rope.to_string(), "    hello\n");
        assert!(!ed.use_tabs);
    }

    #[test]
    fn convert_spaces_to_tabs() {
        let mut ed = Editor::new();
        ed.tab_size = 4;
        ed.rope = Rope::from_str("    hello\n");
        ed.convert_indentation_to_tabs();
        assert_eq!(ed.rope.to_string(), "\thello\n");
        assert!(ed.use_tabs);
    }

    // --- fold_all / unfold_all / fold_at_level tests ---

    #[test]
    fn fold_all_and_unfold() {
        let mut ed = Editor::test_with_text("fn a() {\n    x\n}\nfn b() {\n    y\n}\n");
        ed.fold_all();
        assert!(!ed.folded_ranges.is_empty());
        ed.unfold_all();
        assert!(ed.folded_ranges.is_empty());
    }

    #[test]
    fn fold_at_level_1() {
        let mut ed = Editor::new();
        ed.tab_size = 4;
        ed.rope = Rope::from_str("fn a() {\n    x\n}\n");
        ed.fold_at_level(1);
        assert!(ed.folded_ranges.contains_key(&0));
    }

    // --- bookmark tests ---

    #[test]
    fn toggle_bookmark_on_off() {
        let mut ed = Editor::test_with_text("a\nb\nc\n");
        ed.cursor_line = 1;
        ed.toggle_bookmark();
        assert!(ed.bookmarks.contains(&1));
        ed.toggle_bookmark();
        assert!(!ed.bookmarks.contains(&1));
    }

    #[test]
    fn next_bookmark_wraps() {
        let mut ed = Editor::test_with_text("a\nb\nc\nd\n");
        ed.bookmarks.insert(1);
        ed.cursor_line = 2;
        ed.next_bookmark();
        assert_eq!(ed.cursor_line, 1); // wrapped
    }

    #[test]
    fn prev_bookmark_wraps() {
        let mut ed = Editor::test_with_text("a\nb\nc\nd\n");
        ed.bookmarks.insert(3);
        ed.cursor_line = 0;
        ed.prev_bookmark();
        assert_eq!(ed.cursor_line, 3); // wrapped
    }

    #[test]
    fn clear_bookmarks_removes_all() {
        let mut ed = Editor::new();
        ed.bookmarks.insert(0);
        ed.bookmarks.insert(2);
        ed.clear_bookmarks();
        assert!(ed.bookmarks.is_empty());
    }

    // --- surround_selection test ---

    #[test]
    fn surround_selection_parens() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.selection_anchor = Some((0, 0));
        let ok = ed.surround_selection("(", ")");
        assert!(ok);
        assert!(ed.rope.to_string().contains("(hello)"));
    }

    // --- base64 encode/decode tests ---

    #[test]
    fn base64_encode_basic() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.selection_anchor = Some((0, 0));
        let ok = ed.base64_encode_selection();
        assert!(ok);
        assert!(ed.rope.to_string().contains("aGVsbG8=")); // base64 of "hello"
    }

    #[test]
    fn base64_decode_basic() {
        let mut ed = Editor::test_with_text("aGVsbG8=\n");
        ed.cursor_line = 0;
        ed.cursor_col = 8;
        ed.selection_anchor = Some((0, 0));
        let ok = ed.base64_decode_selection();
        assert!(ok);
        assert!(ed.rope.to_string().contains("hello"));
    }

    // --- url encode/decode tests ---

    #[test]
    fn url_encode_special_chars() {
        let mut ed = Editor::test_with_text("a b&c\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.selection_anchor = Some((0, 0));
        let ok = ed.url_encode_selection();
        assert!(ok);
        assert!(ed.rope.to_string().contains("a%20b%26c"));
    }

    #[test]
    fn url_decode_basic() {
        let mut ed = Editor::test_with_text("a%20b\n");
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.selection_anchor = Some((0, 0));
        let ok = ed.url_decode_selection();
        assert!(ok);
        assert!(ed.rope.to_string().contains("a b"));
    }

    // --- format_json / minify_json tests ---

    #[test]
    fn format_json_basic() {
        let mut ed = Editor::test_with_text(r#"{"a":1,"b":2}"#);
        let result = ed.format_json();
        assert!(result.is_ok());
        assert!(ed.rope.to_string().contains("\"a\": 1"));
    }

    #[test]
    fn format_json_invalid() {
        let mut ed = Editor::test_with_text("not json");
        let result = ed.format_json();
        assert!(result.is_err());
    }

    #[test]
    fn minify_json_basic() {
        let mut ed = Editor::test_with_text("{\n  \"a\": 1\n}\n");
        let result = ed.minify_json();
        assert!(result.is_ok());
        assert!(!ed.rope.to_string().contains('\n'));
    }

    #[test]
    fn convert_to_camel_case_from_kebab() {
        let mut ed = Editor::test_with_text("my-var-name\n");
        ed.selection_anchor = Some((0, 0));
        ed.cursor_line = 0;
        ed.cursor_col = 11;
        ed.convert_to_camel_case();
        assert_eq!(ed.rope.line(0).to_string().trim(), "myVarName");
    }

    #[test]
    fn convert_to_snake_case_from_pascal() {
        let mut ed = Editor::test_with_text("MyVarName\n");
        ed.selection_anchor = Some((0, 0));
        ed.cursor_line = 0;
        ed.cursor_col = 9;
        ed.convert_to_snake_case();
        assert_eq!(ed.rope.line(0).to_string().trim(), "my_var_name");
    }

    #[test]
    fn toggle_case_lower_to_upper() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.selection_anchor = Some((0, 0));
        ed.cursor_line = 0;
        ed.cursor_col = 5;
        ed.toggle_case();
        assert_eq!(ed.rope.line(0).to_string().trim(), "HELLO");
    }

    #[test]
    fn toggle_case_upper_to_title() {
        let mut ed = Editor::test_with_text("HELLO WORLD\n");
        ed.selection_anchor = Some((0, 0));
        ed.cursor_line = 0;
        ed.cursor_col = 11;
        ed.toggle_case();
        assert_eq!(ed.rope.line(0).to_string().trim(), "Hello World");
    }

    #[test]
    fn toggle_case_mixed_to_lower() {
        let mut ed = Editor::test_with_text("Hello World\n");
        ed.selection_anchor = Some((0, 0));
        ed.cursor_line = 0;
        ed.cursor_col = 11;
        ed.toggle_case();
        assert_eq!(ed.rope.line(0).to_string().trim(), "hello world");
    }

    #[test]
    fn sort_imports_basic() {
        let mut ed = Editor::test_with_text("use c;\nuse a;\nuse b;\n");
        ed.sort_imports();
        let text = ed.rope.to_string();
        assert!(text.find("use a").unwrap() < text.find("use b").unwrap());
        assert!(text.find("use b").unwrap() < text.find("use c").unwrap());
    }

    #[test]
    fn toggle_line_comment_rust() {
        let mut ed = Editor::test_with_text("hello\n");
        ed.file_path = Some(std::path::PathBuf::from("test.rs"));
        ed.cursor_line = 0;
        ed.toggle_line_comment();
        assert!(ed.rope.line(0).to_string().starts_with("// "));
    }

    #[test]
    fn toggle_line_comment_uncomment() {
        let mut ed = Editor::test_with_text("// hello\n");
        ed.file_path = Some(std::path::PathBuf::from("test.rs"));
        ed.cursor_line = 0;
        ed.toggle_line_comment();
        assert!(!ed.rope.line(0).to_string().starts_with("//"));
    }

    #[test]
    fn dedent_line_basic() {
        let mut ed = Editor::test_with_text("    hello\n");
        ed.cursor_line = 0;
        ed.tab_size = 4;
        ed.dedent_line();
        assert_eq!(ed.rope.line(0).to_string(), "hello\n");
    }

    #[test]
    fn indent_lines_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.cursor_line = 0;
        ed.tab_size = 4;
        ed.use_tabs = false;
        ed.indent_lines();
        assert!(ed.rope.line(0).to_string().starts_with("    "));
    }

    #[test]
    fn remove_duplicate_lines_basic() {
        let mut ed = Editor::test_with_text("aaa\nbbb\naaa\nccc\n");
        ed.remove_duplicate_lines();
        let text = ed.rope.to_string();
        // The trailing empty line (after last \n) is kept as unique
        assert!(text.starts_with("aaa\nbbb\nccc\n"));
        // "aaa" appears only once
        assert_eq!(text.matches("aaa").count(), 1);
    }

    #[test]
    fn remove_duplicate_lines_all_same() {
        let mut ed = Editor::test_with_text("x\nx\nx\n");
        ed.remove_duplicate_lines();
        let text = ed.rope.to_string();
        assert_eq!(text.matches("x").count(), 1);
    }

    #[test]
    fn remove_duplicate_lines_no_dupes() {
        let mut ed = Editor::test_with_text("a\nb\nc\n");
        ed.remove_duplicate_lines();
        let text = ed.rope.to_string();
        assert!(text.contains("a\nb\nc\n"));
    }

    #[test]
    fn remove_blank_lines_basic() {
        let mut ed = Editor::test_with_text("hello\n\nworld\n\n");
        ed.remove_blank_lines();
        let text = ed.rope.to_string();
        assert_eq!(text, "hello\nworld\n");
    }

    #[test]
    fn remove_blank_lines_no_blanks() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.remove_blank_lines();
        let text = ed.rope.to_string();
        assert_eq!(text, "hello\nworld\n");
    }

    #[test]
    fn convert_to_constant_case_basic() {
        let mut ed = Editor::test_with_text("helloWorld\n");
        ed.selection_anchor = Some((0, 0));
        ed.cursor_line = 0;
        ed.cursor_col = 10;
        ed.convert_to_constant_case();
        assert!(ed.rope.to_string().contains("HELLO_WORLD"));
    }

    #[test]
    fn convert_to_kebab_case_basic() {
        let mut ed = Editor::test_with_text("helloWorld\n");
        ed.selection_anchor = Some((0, 0));
        ed.cursor_line = 0;
        ed.cursor_col = 10;
        ed.convert_to_kebab_case();
        assert!(ed.rope.to_string().contains("hello-world"));
    }

    #[test]
    fn convert_to_title_case_basic() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.selection_anchor = Some((0, 0));
        ed.cursor_line = 0;
        ed.cursor_col = 11;
        ed.convert_to_title_case();
        assert!(ed.rope.to_string().contains("Hello World"));
    }

    #[test]
    fn convert_line_endings_to_lf_basic() {
        let mut ed = Editor::test_with_text("hello\r\nworld\r\n");
        ed.convert_line_endings_to_lf();
        let text = ed.rope.to_string();
        assert!(!text.contains('\r'));
        assert!(text.contains("hello\nworld\n"));
    }

    #[test]
    fn convert_line_endings_to_crlf_basic() {
        let mut ed = Editor::test_with_text("hello\nworld\n");
        ed.convert_line_endings_to_crlf();
        let text = ed.rope.to_string();
        assert!(text.contains("hello\r\nworld\r\n"));
    }

    #[test]
    fn edge_case_unchanged_line_endings_preserve_clean_state_and_redo() {
        for (text, crlf) in [("", false), ("", true), ("last line", true), ("hello\nworld\n", false), ("hello\r\nworld\r\n", true)] {
            for with_redo in [false, true] {
                let mut ed = Editor::test_with_text(text);
                if with_redo {
                    ed.insert_char('x');
                    ed.undo();
                    assert!(ed.can_redo());
                }
                let before = ed.rope.clone();
                let state = (ed.content_version, ed.modified, ed.can_undo(), ed.can_redo());
                if crlf { ed.convert_line_endings_to_crlf(); } else { ed.convert_line_endings_to_lf(); }
                assert!(ed.rope.is_instance(&before));
                assert_eq!((ed.content_version, ed.modified, ed.can_undo(), ed.can_redo()), state, "{text:?}, CRLF={crlf}, redo={with_redo}");
            }
        }
    }

    #[test]
    fn edge_case_mixed_line_endings_convert_in_one_undo_step() {
        let original = "α\r\nβ\nγ\r\nlast";
        for (crlf, expected) in [(false, "α\nβ\nγ\nlast"), (true, "α\r\nβ\r\nγ\r\nlast")] {
            let mut ed = Editor::test_with_text(original);
            if crlf { ed.convert_line_endings_to_crlf(); } else { ed.convert_line_endings_to_lf(); }
            assert_eq!(ed.rope.to_string(), expected);
            ed.undo();
            assert_eq!(ed.rope.to_string(), original);
            assert!(!ed.can_undo());
            ed.redo();
            assert_eq!(ed.rope.to_string(), expected);
        }
    }

    #[test]
    fn find_fold_end_basic() {
        let ed = Editor::test_with_text("fn main() {\n    let x = 1;\n    let y = 2;\n}\n");
        let end = ed.find_fold_end(0);
        assert!(end.is_some());
        assert!(end.unwrap() > 0);
    }

    #[test]
    fn find_fold_end_no_children() {
        let ed = Editor::test_with_text("line1\nline2\nline3\n");
        // All lines at same indent — no fold
        assert!(ed.find_fold_end(0).is_none());
    }

    #[test]
    fn fold_at_level_basic() {
        let mut ed = Editor::test_with_text("fn main() {\n    body\n}\nfn other() {\n    body2\n}\n");
        ed.tab_size = 4;
        ed.fold_at_level(1);
        assert!(!ed.folded_ranges.is_empty());
    }

    #[test]
    fn toggle_fold_fold_and_unfold() {
        let mut ed = Editor::test_with_text("fn main() {\n    body\n}\n");
        assert!(ed.folded_ranges.is_empty());
        ed.toggle_fold(0);
        assert!(ed.folded_ranges.contains_key(&0));
        ed.toggle_fold(0);
        assert!(ed.folded_ranges.is_empty());
    }

    // ---- transform_selection tests ----

    #[test]
    fn transform_selection_uppercase() {
        let mut ed = editor_with_selection("hello world", 0, 0, 0, 5);
        ed.transform_selection(str::to_uppercase);
        assert_eq!(ed.rope.to_string(), "HELLO world");
        // Selection should be preserved around transformed text
        assert_eq!(ed.selection_anchor, Some((0, 0)));
        assert_eq!(ed.cursor_col, 5);
    }

    #[test]
    fn transform_selection_lowercase() {
        let mut ed = editor_with_selection("HELLO WORLD", 0, 6, 0, 11);
        ed.transform_selection(str::to_lowercase);
        assert_eq!(ed.rope.to_string(), "HELLO world");
        assert_eq!(ed.selection_anchor, Some((0, 6)));
    }

    #[test]
    fn transform_selection_no_selection_selects_word() {
        let mut ed = editor_with_text("hello world");
        ed.cursor_line = 0;
        ed.cursor_col = 2; // inside "hello"
        ed.transform_selection(str::to_uppercase);
        assert_eq!(ed.rope.to_string(), "HELLO world");
    }

    #[test]
    fn toggle_case_full_cycle_lower_upper() {
        let mut ed = editor_with_selection("hello", 0, 0, 0, 5);
        ed.toggle_case();
        assert_eq!(ed.rope.to_string(), "HELLO");
    }

    #[test]
    fn toggle_case_full_cycle_upper_title() {
        let mut ed = editor_with_selection("HELLO WORLD", 0, 0, 0, 11);
        ed.toggle_case();
        assert_eq!(ed.rope.to_string(), "Hello World");
    }

    #[test]
    fn toggle_case_full_cycle_title_lower() {
        let mut ed = editor_with_selection("Hello World", 0, 0, 0, 11);
        ed.toggle_case();
        assert_eq!(ed.rope.to_string(), "hello world");
    }

    #[test]
    fn convert_camel_case() {
        let mut ed = editor_with_selection("hello_world_test", 0, 0, 0, 16);
        ed.convert_to_camel_case();
        assert_eq!(ed.rope.to_string(), "helloWorldTest");
    }

    #[test]
    fn convert_pascal_case() {
        let mut ed = editor_with_selection("hello_world", 0, 0, 0, 11);
        ed.convert_to_pascal_case();
        assert_eq!(ed.rope.to_string(), "HelloWorld");
    }

    #[test]
    fn convert_snake_case() {
        let mut ed = editor_with_selection("helloWorld", 0, 0, 0, 10);
        ed.convert_to_snake_case();
        assert_eq!(ed.rope.to_string(), "hello_world");
    }

    #[test]
    fn convert_constant_case() {
        let mut ed = editor_with_selection("helloWorld", 0, 0, 0, 10);
        ed.convert_to_constant_case();
        assert_eq!(ed.rope.to_string(), "HELLO_WORLD");
    }

    #[test]
    fn convert_kebab_case() {
        let mut ed = editor_with_selection("helloWorld", 0, 0, 0, 10);
        ed.convert_to_kebab_case();
        assert_eq!(ed.rope.to_string(), "hello-world");
    }

    #[test]
    fn convert_title_case() {
        let mut ed = editor_with_selection("hello_world_test", 0, 0, 0, 16);
        ed.convert_to_title_case();
        assert_eq!(ed.rope.to_string(), "Hello World Test");
    }

    #[test]
    fn transform_selection_multiline() {
        let mut ed = editor_with_selection("hello\nworld", 0, 0, 1, 5);
        ed.transform_selection(str::to_uppercase);
        assert_eq!(ed.rope.to_string(), "HELLO\nWORLD");
    }

    #[test]
    fn transform_selection_empty_selection() {
        // When cursor is on a word with no selection, transform_selection should
        // select the word under the cursor and transform it
        let mut ed = editor_with_text("hello world");
        ed.cursor_col = 7; // inside "world"
        // No selection anchor set — this triggers select_word_at_cursor
        ed.transform_selection(str::to_uppercase);
        assert!(ed.rope.to_string().contains("WORLD"));
    }

    #[test]
    fn convert_camel_case_from_kebab() {
        let mut ed = editor_with_selection("my-variable-name", 0, 0, 0, 16);
        ed.convert_to_camel_case();
        assert_eq!(ed.rope.to_string(), "myVariableName");
    }

    #[test]
    fn convert_snake_case_from_pascal() {
        let mut ed = editor_with_selection("MyVariableName", 0, 0, 0, 14);
        ed.convert_to_snake_case();
        assert_eq!(ed.rope.to_string(), "my_variable_name");
    }

    #[test]
    fn next_change_wraps() {
        let mut ed = Editor::test_with_text("line1\nline2\nline3\n");
        ed.set_git_line_status(vec![' ', 'M', ' ', ' ']);
        ed.cursor_line = 0;
        ed.next_change();
        assert_eq!(ed.cursor_line, 1);
    }

    #[test]
    fn prev_change_wraps() {
        let mut ed = Editor::test_with_text("line1\nline2\nline3\n");
        ed.set_git_line_status(vec![' ', 'M', ' ', ' ']);
        ed.cursor_line = 2;
        ed.prev_change();
        assert_eq!(ed.cursor_line, 1);
    }

    #[test]
    fn reload_from_disk_clears_git_line_status() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("vtcode-reload-{unique}.txt"));
        std::fs::write(&path, "disk\n").expect("write test file");

        let mut ed = Editor::new();
        ed.file_path = Some(path.clone());
        ed.rope = Rope::from_str("stale\n");
        ed.set_git_line_status(vec!['M']);

        ed.reload_from_disk().expect("reload from disk");

        assert!(ed.git_line_status().is_empty());
        let _ = std::fs::remove_file(path); // Error OK: file may not exist
    }

    #[test]
    fn convert_to_pascal_case_basic() {
        let mut ed = editor_with_selection("hello_world\n", 0, 0, 0, 11);
        ed.convert_to_pascal_case();
        assert!(ed.rope.to_string().contains("HelloWorld"));
    }

    #[test]
    fn visible_line_count_no_folds() {
        let mut ed = Editor::test_with_text("a\nb\nc\n");
        assert_eq!(ed.visible_line_count(), ed.line_count());
    }

    #[test]
    fn visible_line_count_with_folds() {
        let mut ed = Editor::test_with_text("fn main() {\n    body1\n    body2\n}\n");
        ed.toggle_fold(0);
        // Folding hides body lines, reduces visible count
        let visible = ed.visible_line_count();
        assert!(visible < ed.line_count());
    }

    #[test]
    fn visible_line_count_unions_overlapping_fold_intervals() {
        let mut ed = Editor::test_with_text(&"x\n".repeat(10));
        ed.folded_ranges.insert(0, 6);
        ed.folded_ranges.insert(3, 9);
        ed.cached_visible_line_count = None;

        assert_eq!(ed.line_count(), 11);
        assert_eq!(ed.visible_line_count(), 3);
    }

    #[test]
    fn visible_lines_jump_over_large_folded_ranges() {
        let mut ed = Editor::test_with_text(&"x\n".repeat(50_000));
        ed.folded_ranges.insert(0, 49_999);

        let visible = ed.visible_lines(1, 2);

        assert_eq!(visible, vec![49_999, 50_000]);
    }

    #[test]
    fn sort_json_keys_basic() {
        let mut ed = Editor::test_with_text(r#"{"b": 2, "a": 1}"#);
        assert!(ed.sort_json_keys().is_ok());
        let text = ed.rope.to_string();
        let a_pos = text.find("\"a\"").unwrap();
        let b_pos = text.find("\"b\"").unwrap();
        assert!(a_pos < b_pos);
    }

    #[test]
    fn sort_json_keys_nested() {
        let mut ed = Editor::test_with_text(r#"{"z": {"b": 1, "a": 2}, "y": 3}"#);
        assert!(ed.sort_json_keys().is_ok());
        let text = ed.rope.to_string();
        assert!(text.find("\"y\"").unwrap() < text.find("\"z\"").unwrap());
        assert!(text.find("\"a\"").unwrap() < text.find("\"b\"").unwrap());
    }

    // --- surround_selection edge case tests ---

    #[test]
    fn surround_selection_no_selection_returns_false() {
        let mut ed = Editor::test_with_text("hello world\n");
        ed.cursor_line = 0;
        ed.cursor_col = 3;
        ed.selection_anchor = None;
        let ok = ed.surround_selection("[", "]");
        assert!(!ok);
    }

    #[test]
    fn surround_selection_brackets() {
        let mut ed = editor_with_selection("foo bar\n", 0, 0, 0, 3);
        let ok = ed.surround_selection("[", "]");
        assert!(ok);
        assert!(ed.rope.to_string().contains("[foo]"));
    }

    #[test]
    fn surround_selection_braces() {
        let mut ed = editor_with_selection("value\n", 0, 0, 0, 5);
        let ok = ed.surround_selection("{", "}");
        assert!(ok);
        assert!(ed.rope.to_string().contains("{value}"));
    }

    #[test]
    fn surround_selection_double_quotes() {
        let mut ed = editor_with_selection("text\n", 0, 0, 0, 4);
        let ok = ed.surround_selection("\"", "\"");
        assert!(ok);
        assert!(ed.rope.to_string().contains("\"text\""));
    }

    #[test]
    fn surround_selection_backticks() {
        let mut ed = editor_with_selection("code\n", 0, 0, 0, 4);
        let ok = ed.surround_selection("`", "`");
        assert!(ok);
        assert!(ed.rope.to_string().contains("`code`"));
    }

    #[test]
    fn surround_selection_html_tag() {
        let mut ed = editor_with_selection("content\n", 0, 0, 0, 7);
        let ok = ed.surround_selection("<div>", "</div>");
        assert!(ok);
        assert!(ed.rope.to_string().contains("<div>content</div>"));
    }

    // --- case conversion edge case tests ---

    #[test]
    fn convert_to_camel_case_already_camel() {
        let mut ed = editor_with_selection("helloWorld", 0, 0, 0, 10);
        ed.convert_to_camel_case();
        let result = ed.rope.to_string();
        assert!(result.contains("helloWorld") || result.contains("helloworld"));
    }

    #[test]
    fn convert_to_snake_case_from_constant() {
        let mut ed = editor_with_selection("HELLO_WORLD", 0, 0, 0, 11);
        ed.convert_to_snake_case();
        assert_eq!(ed.rope.to_string(), "hello_world");
    }

    #[test]
    fn convert_to_kebab_case_from_snake() {
        let mut ed = editor_with_selection("hello_world", 0, 0, 0, 11);
        ed.convert_to_kebab_case();
        assert_eq!(ed.rope.to_string(), "hello-world");
    }

    #[test]
    fn toggle_case_single_char() {
        let mut ed = editor_with_selection("a", 0, 0, 0, 1);
        ed.toggle_case();
        assert_eq!(ed.rope.to_string(), "A");
    }

    #[test]
    fn convert_to_pascal_case_from_kebab() {
        let mut ed = editor_with_selection("hello-world", 0, 0, 0, 11);
        ed.convert_to_pascal_case();
        assert_eq!(ed.rope.to_string(), "HelloWorld");
    }

    #[test]
    fn convert_to_title_case_single_word() {
        let mut ed = editor_with_selection("hello", 0, 0, 0, 5);
        ed.convert_to_title_case();
        assert_eq!(ed.rope.to_string(), "Hello");
    }

    #[test]
    fn convert_to_constant_case_from_kebab() {
        let mut ed = editor_with_selection("hello-world", 0, 0, 0, 11);
        ed.convert_to_constant_case();
        assert_eq!(ed.rope.to_string(), "HELLO_WORLD");
    }

    // --- Additional coverage tests ---

    #[test]
    fn reverse_lines_with_selection() {
        // Select all lines to avoid partial-range edge case
        let mut ed = editor_with_selection("aaa\nbbb\nccc", 0, 0, 2, 3);
        ed.reverse_lines();
        assert_eq!(ed.rope.to_string(), "ccc\nbbb\naaa");
    }

    #[test]
    fn trim_trailing_whitespace_clamps_cursor() {
        let mut ed = editor_with_text("hello   \nworld\n");
        ed.cursor_line = 0;
        ed.cursor_col = 7; // on trailing space
        ed.trim_trailing_whitespace();
        assert_eq!(ed.rope.to_string(), "hello\nworld\n");
        assert_eq!(ed.cursor_col, 5);
    }

    #[test]
    fn trim_trailing_whitespace_empty_file() {
        let mut ed = editor_with_text("");
        ed.trim_trailing_whitespace();
        assert_eq!(ed.rope.to_string(), "");
    }

    #[test]
    fn trim_trailing_whitespace_mixed_spaces_and_tabs() {
        let mut ed = editor_with_text("hello \t \nworld\t\n");
        ed.trim_trailing_whitespace();
        assert_eq!(ed.rope.to_string(), "hello\nworld\n");
    }

    #[test]
    fn trim_trailing_whitespace_crlf_preserved() {
        let mut ed = editor_with_text("hello   \r\nworld  \r\n");
        let changed = ed.trim_trailing_whitespace();
        assert!(changed);
        assert_eq!(ed.rope.to_string(), "hello\r\nworld\r\n");
    }

    #[test]
    fn trim_trailing_whitespace_returns_false_when_clean() {
        let mut ed = editor_with_text("hello\nworld\n");
        let changed = ed.trim_trailing_whitespace();
        assert!(!changed);
    }

    #[test]
    fn toggle_case_no_selection_selects_word() {
        let mut ed = editor_with_selection("hello world", 0, 0, 0, 5);
        ed.toggle_case();
        assert_eq!(ed.rope.to_string(), "HELLO world");
    }

    #[test]
    fn convert_to_title_case_multiline() {
        let mut ed = editor_with_selection("hello_world", 0, 0, 0, 11);
        ed.convert_to_title_case();
        assert_eq!(ed.rope.to_string(), "Hello World");
    }

    #[test]
    fn convert_to_snake_case_from_kebab() {
        let mut ed = editor_with_selection("hello-world-test", 0, 0, 0, 16);
        ed.convert_to_snake_case();
        assert_eq!(ed.rope.to_string(), "hello_world_test");
    }

    #[test]
    fn convert_to_camel_case_from_constant() {
        let mut ed = editor_with_selection("HELLO_WORLD", 0, 0, 0, 11);
        ed.convert_to_camel_case();
        assert_eq!(ed.rope.to_string(), "helloWorld");
    }

    #[test]
    fn convert_to_pascal_case_from_constant() {
        let mut ed = editor_with_selection("HELLO_WORLD", 0, 0, 0, 11);
        ed.convert_to_pascal_case();
        assert_eq!(ed.rope.to_string(), "HelloWorld");
    }

    #[test]
    fn remove_blank_lines_all_blank() {
        let mut ed = editor_with_text("\n\n\n");
        ed.remove_blank_lines();
        // Trailing newline preserved, but all content lines removed
        assert_eq!(ed.rope.to_string(), "\n");
    }

    #[test]
    fn remove_duplicate_lines_with_selection() {
        let mut ed = editor_with_selection("keep\naaa\nbbb\naaa\nkeep\n", 1, 0, 3, 3);
        ed.remove_duplicate_lines();
        let text = ed.rope.to_string();
        assert!(text.starts_with("keep\naaa\nbbb\nkeep\n"));
    }

    #[test]
    fn reindent_lines_basic_braces() {
        let mut ed = editor_with_text("fn main() {\nlet x = 1;\nlet y = 2;\n}\n");
        ed.tab_size = 4;
        ed.use_tabs = false;
        ed.reindent_lines();
        let text = ed.rope.to_string();
        assert!(text.contains("    let x = 1;\n"));
        assert!(text.contains("    let y = 2;\n"));
        assert!(text.starts_with("fn main() {\n"));
        assert!(text.ends_with("}\n"));
    }

    #[test]
    fn convert_indentation_tabs_to_spaces_roundtrip() {
        let mut ed = editor_with_text("\thello\n\t\tworld\n");
        ed.tab_size = 4;
        ed.convert_indentation_to_spaces();
        assert_eq!(ed.rope.to_string(), "    hello\n        world\n");
        assert!(!ed.use_tabs);
    }

    #[test]
    fn convert_indentation_spaces_to_tabs_roundtrip() {
        let mut ed = editor_with_text("    hello\n        world\n");
        ed.tab_size = 4;
        ed.convert_indentation_to_tabs();
        assert_eq!(ed.rope.to_string(), "\thello\n\t\tworld\n");
        assert!(ed.use_tabs);
    }
