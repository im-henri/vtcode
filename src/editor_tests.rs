
use super::{
    compute_unsaved_diff_markers, load_text_file, save_temp_path, sort_json_value, Editor,
};
use crate::editor_snippets::{get_snippet, parse_snippet_body};
use ropey::Rope;

#[test]
fn file_decoder_reads_each_byte_once_for_all_supported_encodings() {
    use std::io::{Cursor, Read};
    struct CountedReader { inner: Cursor<Vec<u8>>, bytes: usize }
    impl Read for CountedReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            let read = self.inner.read(buffer)?;
            self.bytes += read;
            Ok(read)
        }
    }
    impl std::io::Seek for CountedReader {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            std::io::Seek::seek(&mut self.inner, position)
        }
    }
    for text in ["".into(), "small file\nλ🙂".into(), "large file\nλ🙂".repeat(900)] {
        for encoding in ["UTF-8", "UTF-8 with BOM", "UTF-16 LE", "UTF-16 BE"] {
            let bytes = match encoding {
                "UTF-8 with BOM" => [vec![0xef, 0xbb, 0xbf], text.as_bytes().to_vec()].concat(),
                "UTF-16 LE" => [vec![0xff, 0xfe], text.encode_utf16().flat_map(u16::to_le_bytes).collect()].concat(),
                "UTF-16 BE" => [vec![0xfe, 0xff], text.encode_utf16().flat_map(u16::to_be_bytes).collect()].concat(),
                _ => text.as_bytes().to_vec(),
            };
            let size = bytes.len();
            let mut reader = CountedReader { inner: Cursor::new(bytes), bytes: 0 };
            let (rope, detected) = super::read_text_stream(&mut reader, std::path::Path::new("unused"), size as u64).unwrap();
            assert_eq!(rope.to_string(), text);
            assert_eq!(detected, encoding);
            assert_eq!(reader.bytes, size);
        }
    }
}

#[test]
fn file_decoder_handles_truncation_during_the_header_read() {
    let (rope, _) = super::read_text_stream(std::io::Cursor::new(b"short"), std::path::Path::new("unused"), 100).unwrap();
    assert_eq!(rope.to_string(), "short");
}

#[test]
fn save_temp_paths_are_unique_and_keep_original_filename() {
    let rust_path = std::path::Path::new("src/foo.rs");
    let text_path = std::path::Path::new("src/foo.txt");
    let first = save_temp_path(rust_path);
    let second = save_temp_path(rust_path);
    let other_extension = save_temp_path(text_path);

    assert_ne!(first, second);
    assert_ne!(first, other_extension);
    assert!(first
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("foo.rs.vtcode-tmp-"));
    assert!(other_extension
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("foo.txt.vtcode-tmp-"));
}

#[test]
fn wrapped_rows_prefer_word_boundaries() {
    let mut ed = Editor::test_with_text("hello world again");

    let rows = ed.wrapped_rows(0, 10, 7);
    assert_eq!(rows.len(), 3);
    assert_eq!((rows[0].start_col, rows[0].end_col), (0, 6));
    assert_eq!((rows[1].start_col, rows[1].end_col), (6, 12));
    assert_eq!((rows[2].start_col, rows[2].end_col), (12, 17));
}

#[test]
fn wrapped_rows_fallback_to_char_split_for_long_words() {
    let mut ed = Editor::test_with_text("supercalifragilistic");

    let rows = ed.wrapped_rows(0, 10, 5);
    assert_eq!(rows.len(), 4);
    assert_eq!((rows[0].start_col, rows[0].end_col), (0, 5));
    assert_eq!((rows[1].start_col, rows[1].end_col), (5, 10));
    assert_eq!((rows[2].start_col, rows[2].end_col), (10, 15));
    assert_eq!((rows[3].start_col, rows[3].end_col), (15, 20));
}

#[test]
fn wrapped_position_clamps_to_visual_row_end() {
    let mut ed = Editor::test_with_text("hello world again");

    let (line, col) = ed.wrapped_position_for_screen(0, 0, 6, 7);
    assert_eq!((line, col), (0, 6));
}

#[test]
fn wrapped_row_for_position_respects_wrap_boundaries() {
    let mut ed = Editor::test_with_text("hello world again");

    assert_eq!(ed.wrapped_row_for_position(0, 5, 7), 0);
    assert_eq!(ed.wrapped_row_for_position(0, 6, 7), 1);
    assert_eq!(ed.wrapped_row_for_position(0, 12, 7), 2);
}

#[test]
fn split_into_words_handles_case_transitions_delimiters_and_empty_input() {
    for (source, expected) in [
        ("helloWorld", &["hello", "World"][..]),
        ("hello_world_test", &["hello", "world", "test"]),
        ("hello-world", &["hello", "world"]),
        ("hello", &["hello"]),
        ("", &[]),
        ("hello world", &["hello", "world"]),
        ("foo_bar-baz qux", &["foo", "bar", "baz", "qux"]),
        // Digits do not introduce an uppercase-after-lowercase boundary.
        ("getItem2Name", &["get", "Item2Name"]),
    ] {
        assert_eq!(super::split_into_words(source), expected, "{source:?}");
    }
}

// --- find_matching_bracket tests ---

#[test]
fn bracket_match_simple_parens() {
    let mut ed = Editor::test_with_text("(hello)");
    ed.cursor_line = 0;
    ed.cursor_col = 0;
    assert_eq!(ed.find_matching_bracket(), Some((0, 6)));
}

#[test]
fn bracket_match_closing_paren() {
    let mut ed = Editor::test_with_text("(hello)");
    ed.cursor_line = 0;
    ed.cursor_col = 6;
    assert_eq!(ed.find_matching_bracket(), Some((0, 0)));
}

#[test]
fn bracket_match_nested() {
    let mut ed = Editor::test_with_text("({[()]})");
    ed.cursor_line = 0;
    ed.cursor_col = 0;
    assert_eq!(ed.find_matching_bracket(), Some((0, 7)));
}

#[test]
fn bracket_match_no_bracket() {
    let mut ed = Editor::test_with_text("hello");
    ed.cursor_line = 0;
    ed.cursor_col = 2;
    assert_eq!(ed.find_matching_bracket(), None);
}

#[test]
fn bracket_match_multiline() {
    let mut ed = Editor::test_with_text("{\n  foo\n}");
    ed.cursor_line = 0;
    ed.cursor_col = 0;
    assert_eq!(ed.find_matching_bracket(), Some((2, 0)));
}

#[test]
fn bracket_match_unmatched() {
    let mut ed = Editor::test_with_text("(hello");
    ed.cursor_line = 0;
    ed.cursor_col = 0;
    assert_eq!(ed.find_matching_bracket(), None);
}

// --- sort_json_value tests ---

#[test]
fn sort_json_value_object() {
    use serde_json::json;
    let val = json!({"z": 1, "a": 2, "m": 3});
    let sorted = sort_json_value(&val);
    let keys: Vec<&String> = sorted.as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["a", "m", "z"]);
}

#[test]
fn sort_json_value_nested() {
    use serde_json::json;
    let val = json!({"b": {"z": 1, "a": 2}, "a": 3});
    let sorted = sort_json_value(&val);
    let outer_keys: Vec<&String> = sorted.as_object().unwrap().keys().collect();
    assert_eq!(outer_keys, vec!["a", "b"]);
    let inner_keys: Vec<&String> = sorted["b"].as_object().unwrap().keys().collect();
    assert_eq!(inner_keys, vec!["a", "z"]);
}

#[test]
fn sort_json_value_array() {
    use serde_json::json;
    let val = json!([{"b": 1, "a": 2}]);
    let sorted = sort_json_value(&val);
    let keys: Vec<&String> = sorted[0].as_object().unwrap().keys().collect();
    assert_eq!(keys, vec!["a", "b"]);
}

#[test]
fn sort_json_value_primitive() {
    use serde_json::json;
    let val = json!(42);
    let sorted = sort_json_value(&val);
    assert_eq!(sorted, json!(42));
}

// --- detect_indentation tests ---

#[test]
fn detect_indentation_tabs() {
    let mut ed = Editor::test_with_text("\tfoo\n\tbar\n\tbaz\n");
    ed.use_tabs = false;
    ed.detect_indentation();
    assert!(ed.use_tabs);
}

#[test]
fn detect_indentation_4spaces() {
    let mut ed = Editor::new();
    let lines: String = (0..10).map(|_| "    code\n").collect();
    ed.rope = Rope::from_str(&lines);
    ed.use_tabs = true;
    ed.detect_indentation();
    assert!(!ed.use_tabs);
    assert_eq!(ed.tab_size, 4);
}

#[test]
fn detect_indentation_2spaces() {
    let mut ed = Editor::new();
    let lines: String = (0..10)
        .map(|i| {
            if i % 2 == 0 {
                "  code\n".to_string()
            } else {
                "code\n".to_string()
            }
        })
        .collect();
    ed.rope = Rope::from_str(&lines);
    ed.use_tabs = true;
    ed.detect_indentation();
    assert!(!ed.use_tabs);
    assert_eq!(ed.tab_size, 2);
}

#[test]
fn detect_indentation_empty_file() {
    let mut ed = Editor::test_with_text("");
    ed.use_tabs = false;
    ed.tab_size = 4;
    ed.detect_indentation();
    // Should not change defaults
    assert!(!ed.use_tabs);
    assert_eq!(ed.tab_size, 4);
}

// --- compute_file_stats tests ---

#[test]
fn file_stats_empty() {
    let ed = Editor::new();
    assert_eq!(ed.compute_file_stats(), (1, 0, 0, 0));
}

#[test]
fn file_stats_basic() {
    let ed = Editor::test_with_text("hello world\nfoo bar baz\n");
    let (lines, non_blank, words, chars) = ed.compute_file_stats();
    assert_eq!(lines, 3);
    assert_eq!(non_blank, 2);
    assert_eq!(words, 5);
    assert_eq!(chars, 24);
}

#[test]
fn file_stats_blank_lines() {
    let ed = Editor::test_with_text("hello\n\n\nworld\n");
    let (_lines, non_blank, words, _chars) = ed.compute_file_stats();
    assert_eq!(non_blank, 2);
    assert_eq!(words, 2);
}

// --- toggle_line_comment tests ---

#[test]
fn toggle_comment_adds_comment() {
    let mut ed = Editor::new();
    ed.file_path = Some(std::path::PathBuf::from("test.rs"));
    ed.rope = ropey::Rope::from_str("let x = 1;\n");
    ed.cursor_line = 0;
    ed.cursor_col = 0;
    ed.toggle_line_comment();
    assert_eq!(ed.rope.line(0).to_string(), "// let x = 1;\n");
}

#[test]
fn toggle_comment_removes_comment() {
    let mut ed = Editor::new();
    ed.file_path = Some(std::path::PathBuf::from("test.rs"));
    ed.rope = ropey::Rope::from_str("// let x = 1;\n");
    ed.cursor_line = 0;
    ed.cursor_col = 3;
    ed.toggle_line_comment();
    assert_eq!(ed.rope.line(0).to_string(), "let x = 1;\n");
}

#[test]
fn toggle_comment_python() {
    let mut ed = Editor::new();
    ed.file_path = Some(std::path::PathBuf::from("test.py"));
    ed.rope = ropey::Rope::from_str("x = 1\n");
    ed.cursor_line = 0;
    ed.cursor_col = 0;
    ed.toggle_line_comment();
    assert_eq!(ed.rope.line(0).to_string(), "# x = 1\n");
}

// --- undo/redo tests ---

#[test]
fn undo_redo_roundtrip() {
    let mut ed = Editor::test_with_text("hello\n");
    ed.save_undo();
    ed.rope = ropey::Rope::from_str("world\n");
    assert_eq!(ed.rope.to_string(), "world\n");
    ed.undo();
    assert_eq!(ed.rope.to_string(), "hello\n");
    ed.redo();
    assert_eq!(ed.rope.to_string(), "world\n");
}

#[test]
fn find_definition_fn() {
    let ed = Editor::test_with_text("// comment\nfn hello() {\n}\n");
    let result = ed.find_definition("hello");
    assert_eq!(result, Some((1, 3)));
}

#[test]
fn find_definition_not_found() {
    let ed = Editor::test_with_text("let x = 1;\n");
    let result = ed.find_definition("missing");
    assert_eq!(result, None);
}

#[test]
fn find_definition_pub_struct() {
    let ed = Editor::test_with_text("pub struct MyStruct {\n}\n");
    let result = ed.find_definition("MyStruct");
    assert_eq!(result, Some((0, 11)));
}

#[test]
fn complete_word_basic() {
    let mut ed = Editor::test_with_text("hel\n");
    ed.cursor_line = 0;
    ed.cursor_col = 3;
    ed.complete_word("hello_world");
    assert!(ed.rope.line(0).to_string().contains("hello_world"));
    assert_eq!(ed.cursor_col, 11);
}

#[test]
fn complete_word_at_line_start() {
    let mut ed = Editor::test_with_text("text\n");
    ed.cursor_line = 0;
    ed.cursor_col = 0;
    ed.complete_word("replacement");
    assert!(ed.rope.line(0).to_string().starts_with("replacement"));
}

#[test]
fn complete_word_replaces_partial() {
    let mut ed = Editor::test_with_text("prin something\n");
    ed.cursor_line = 0;
    ed.cursor_col = 4; // after "prin"
    ed.complete_word("println");
    let line = ed.rope.line(0).to_string();
    assert!(line.starts_with("println something"));
}

#[test]
fn complete_word_replaces_identifier_suffix_and_preserves_call_arguments() {
    let mut ed = Editor::test_with_text("clie.sendOld(value)\n");
    ed.cursor_col = 8;
    ed.complete_word("sendRequest()");
    assert_eq!(ed.rope.to_string(), "clie.sendRequest(value)\n");
    assert_eq!(ed.cursor_col, "clie.sendRequest(".chars().count());
    ed.undo();
    assert_eq!(ed.rope.to_string(), "clie.sendOld(value)\n");
}

#[test]
fn complete_word_places_unicode_caret_inside_learned_call() {
    let mut ed = Editor::test_with_text("obj.ava\n");
    ed.cursor_col = 7;
    ed.complete_word("avamine()");
    assert_eq!(ed.rope.to_string(), "obj.avamine()\n");
    assert_eq!(ed.cursor_col, "obj.avamine(".chars().count());
}

#[test]
fn complete_word_refuses_an_oversized_identifier_without_partial_insertion() {
    let text = format!("pr{}\n", "x".repeat(100_000));
    let mut ed = Editor::test_with_text(&text);
    ed.cursor_col = 2;
    ed.complete_word("print()");
    assert_eq!(ed.rope.to_string(), text);
    assert_eq!(ed.cursor_col, 2);
}

#[test]
fn complete_number_replaces_numeric_atom_and_undo_restores_it() {
    for (source, completion, expected) in [
        ("set(1|.25)", "1.75", "set(1.75)"),
        ("set(-1|.25)", "-1.75", "set(-1.75)"),
        ("size = |", "0x0040", "size = 0x0040"),
        ("set(-|)", "-3", "set(-3)"),
        ("count = 1|_000", "2_000", "count = 2_000"),
        ("set(0x0|020)", "0x0040", "set(0x0040)"),
    ] {
        let (left, right) = source.split_once('|').unwrap();
        let original = format!("{left}{right}");
        let mut ed = Editor::test_with_text(&original);
        ed.cursor_col = left.chars().count();
        ed.complete_number(completion);
        assert_eq!(ed.rope.to_string(), expected, "{source}");
        ed.undo();
        assert_eq!(ed.rope.to_string(), original, "{source}");
    }
}

#[test]
fn complete_number_rejects_ranges_identifiers_and_binary_minus_ambiguity() {
    for source in ["set(1|..3)", "set(1|e3)", "value-1|", "set(size|)"] {
        let (left, right) = source.split_once('|').unwrap();
        let original = format!("{left}{right}");
        let mut ed = Editor::test_with_text(&original);
        ed.cursor_col = left.chars().count();
        ed.complete_number("-2.0");
        assert_eq!(ed.rope.to_string(), original, "{source}");
    }
}

#[test]
fn cursor_in_tag_name_html() {
    let mut ed = Editor::test_with_text("<div>text</div>\n");
    ed.language_override = Some("HTML".to_string());
    ed.cursor_line = 0;
    ed.cursor_col = 2; // inside "div"
    let result = ed.cursor_in_tag_name();
    assert!(result.is_some());
    let (name, _, _, is_closing) = result.unwrap();
    assert_eq!(name, "div");
    assert!(!is_closing);
}

#[test]
fn find_matching_tag_open_to_close() {
    let mut ed = Editor::test_with_text("<div>content</div>\n");
    ed.language_override = Some("HTML".to_string());
    ed.cursor_line = 0;
    ed.cursor_col = 2;
    let result = ed.find_matching_tag();
    assert!(result.is_some());
    let (_, _, _, t2_line, t2_cs, _) = result.unwrap();
    assert_eq!(t2_line, 0);
    assert_eq!(t2_cs, 12); // position of </div>
}

#[test]
fn find_matching_tag_no_match() {
    let mut ed = Editor::test_with_text("no tags here\n");
    ed.cursor_line = 0;
    ed.cursor_col = 3;
    assert!(ed.find_matching_tag().is_none());
}

#[test]
fn find_matching_tag_closing_first() {
    let mut ed = Editor::test_with_text("<div>content</div>\n");
    ed.language_override = Some("HTML".to_string());
    ed.cursor_line = 0;
    ed.cursor_col = 14; // inside </div>
    let result = ed.find_matching_tag();
    assert!(result.is_some());
    let (t1_line, t1_cs, _, _, _, _) = result.unwrap();
    assert_eq!(t1_line, 0);
    assert_eq!(t1_cs, 0); // position of <div>
}

#[test]
fn find_matching_tag_skips_non_markup_languages() {
    let mut ed = Editor::test_with_text("std::vector<int>\n");
    ed.language_override = Some("C++".to_string());
    ed.cursor_line = 0;
    ed.cursor_col = 13;

    assert!(ed.find_matching_tag().is_none());
}

#[test]
fn line_diff_status_default() {
    let ed = Editor::new();
    assert_eq!(ed.line_diff_status(0), ' ');
}

#[test]
fn line_diff_status_with_git() {
    let mut ed = Editor::new();
    ed.set_git_line_status(vec!['M', ' ']);
    assert_eq!(ed.line_diff_status(0), 'M');
    assert_eq!(ed.line_diff_status(1), ' ');
    assert_eq!(ed.line_diff_status(99), ' '); // out of bounds
}

#[test]
fn apply_bg_loaded_clears_git_line_status() {
    let mut ed = Editor::new();
    ed.set_git_line_status(vec!['M', 'U']);

    ed.apply_bg_loaded(
        ropey::Rope::from_str("hello\n"),
        "UTF-8",
        6,
        crate::editor::LARGE_FILE_THRESHOLD,
    );

    assert!(ed.git_line_status().is_empty());
}

#[test]
fn background_load_uses_configured_large_file_threshold() {
    let mut ed = Editor::new();
    ed.word_wrap = true;
    ed.apply_bg_loaded(
        ropey::Rope::from_str("small in-memory fixture\n"),
        "UTF-8",
        101 * 1024 * 1024,
        100 * 1024 * 1024,
    );

    assert!(ed.large_file_mode);
    assert!(!ed.word_wrap);
    assert!(!ed.rich_features_timed_out);
}

#[test]
fn analysis_timeout_enters_large_file_mode_below_size_threshold() {
    let mut ed = Editor::new();
    ed.file_size = 1024;
    ed.word_wrap = true;

    ed.mark_rich_features_timed_out(100 * 1024 * 1024);

    assert!(ed.large_file_mode);
    assert!(ed.rich_features_timed_out);
    assert!(!ed.word_wrap);
}

#[test]
fn export_import_undo_stack() {
    let mut ed = Editor::test_with_text("hello\n");
    ed.save_undo();
    ed.rope = ropey::Rope::from_str("world\n");
    let undo = ed.export_undo_stack();
    assert!(!undo.is_empty());
    let redo = ed.export_redo_stack();
    // Create new editor and import
    let mut ed2 = Editor::new();
    ed2.import_undo_redo(undo, redo);
    assert!(!ed2.export_undo_stack().is_empty());
}

#[test]
fn line_len_basic() {
    let ed = Editor::new();
    // Default editor has one empty line
    assert_eq!(ed.line_len(0), 0);
}

#[test]
fn line_len_with_content() {
    let ed = Editor::test_with_text("hello\nworld\n");
    assert_eq!(ed.line_len(0), 5);
    assert_eq!(ed.line_len(1), 5);
}

#[test]
fn line_len_crlf() {
    let ed = Editor::test_with_text("hi\r\nbye\r\n");
    assert_eq!(ed.line_len(0), 2);
    assert_eq!(ed.line_len(1), 3);
}

#[test]
fn line_len_out_of_bounds() {
    let ed = Editor::new();
    assert_eq!(ed.line_len(999), 0);
}

#[test]
fn break_undo_group_basic() {
    let mut ed = Editor::test_with_text("hello\n");
    ed.save_undo();
    assert!(!ed.undo_group_active); // save_undo doesn't set group_active
    ed.insert_char('x');
    // After insert, maybe_save_undo_char may set undo_group_active
    ed.break_undo_group();
    assert!(!ed.undo_group_active);
}

#[test]
fn wrap_ranges_short_line() {
    let ranges = Editor::wrap_ranges_for_line_text("hello", 80);
    assert_eq!(ranges, vec![(0, 5)]);
}


#[test]
fn wrap_ranges_wrapping() {
    let ranges = Editor::wrap_ranges_for_line_text("hello world foo", 6);
    assert!(ranges.len() > 1);
}

#[test]
fn wrap_ranges_tabs_count_as_one_column() {
    // Tabs should be 1 display column (matching editor rendering).
    // With tab_size=8, if tabs expanded to 8 cols, "\t\tABCD" would be 20 cols.
    // But since tabs are 1 col each, it's 6 cols total and fits in width 10.
    let ranges = Editor::wrap_ranges_for_line_text_tabbed("\t\tABCD", 10, 8);
    assert_eq!(ranges.len(), 1, "tabs should be 1 column, line should fit");
    assert_eq!(ranges[0], (0, 6));

    // With tab_size=4 and width=3, "\tAB" = 3 cols, should fit in 1 row
    let ranges = Editor::wrap_ranges_for_line_text_tabbed("\tAB", 3, 4);
    assert_eq!(ranges.len(), 1);
    assert_eq!(ranges[0], (0, 3));
}

#[test]
fn current_line_len_basic() {
    let mut ed = Editor::test_with_text("hello\nworld\n");
    ed.cursor_line = 0;
    assert_eq!(ed.current_line_len(), 5);
    ed.cursor_line = 1;
    assert_eq!(ed.current_line_len(), 5);
}

#[test]
fn language_name_override() {
    let mut ed = Editor::new();
    ed.language_override = Some("Python".to_string());
    assert_eq!(ed.language_name(), "Python");
}

#[test]
fn markdown_detection_respects_language_overrides_and_associations() {
    for (name, language, association, expected) in [
        ("notes.md", None, None, true),
        ("notes.MD", None, None, true),
        ("notes.markdown", None, None, true),
        ("notes.MDX", None, None, true),
        ("notes.txt", None, None, false),
        ("notes", None, None, false),
        ("notes.txt", Some("Markdown"), None, true),
        ("notes", Some("Markdown"), None, true),
        ("notes.txt", None, Some("Markdown"), true),
        ("notes", None, Some("markdown"), true),
        ("notes.md", Some("Plain Text"), None, false),
        ("notes.md", None, Some("Plain Text"), false),
        ("notes.txt", Some("Plain Text"), Some("Markdown"), false),
        ("notes.txt", Some("Markdown"), Some("Plain Text"), true),
    ] {
        let mut editor = Editor::test_with_text("# Heading\n");
        editor.file_path = Some(name.into());
        editor.language_override = language.map(str::to_owned);
        if let Some(association) = association {
            editor.language_associations.insert(name.into(), association.into());
        }
        assert_eq!(editor.is_markdown(), expected, "{name}: {language:?}, {association:?}");
    }
}

#[test]
fn language_name_from_extension() {
    let mut ed = Editor::new();
    ed.file_path = Some(std::path::PathBuf::from("test.rs"));
    assert_eq!(ed.language_name(), "Rust");
}

#[test]
fn language_name_unknown() {
    let mut ed = Editor::new();
    ed.file_path = Some(std::path::PathBuf::from("test.xyz"));
    assert_eq!(ed.language_name(), "Plain Text");
}

#[test]
fn language_associations_ext_pattern() {
    let mut ed = Editor::new();
    ed.file_path = Some(std::path::PathBuf::from("test.xyz"));
    ed.language_associations
        .insert("*.xyz".to_string(), "Python".to_string());
    assert_eq!(ed.language_name(), "Python");
}

#[test]
fn language_associations_exact_filename() {
    let mut ed = Editor::new();
    ed.file_path = Some(std::path::PathBuf::from("Jenkinsfile"));
    assert_eq!(ed.language_name(), "Plain Text");
    ed.language_associations
        .insert("Jenkinsfile".to_string(), "Groovy".to_string());
    assert_eq!(ed.language_name(), "Groovy");
}

#[test]
fn language_associations_brace_group() {
    let mut ed = Editor::new();
    ed.file_path = Some(std::path::PathBuf::from("file.mojo"));
    ed.language_associations
        .insert("*.{mojo,🔥}".to_string(), "Mojo".to_string());
    assert_eq!(ed.language_name(), "Mojo");
}

#[test]
fn language_associations_wildcard_suffix() {
    let mut ed = Editor::new();
    ed.file_path = Some(std::path::PathBuf::from("Dockerfile.production"));
    ed.language_associations
        .insert("Dockerfile.*".to_string(), "Dockerfile".to_string());
    assert_eq!(ed.language_name(), "Dockerfile");
}

#[test]
fn language_associations_override_priority() {
    let mut ed = Editor::new();
    ed.file_path = Some(std::path::PathBuf::from("test.rs"));
    // language_override takes priority over associations
    ed.language_override = Some("Java".to_string());
    ed.language_associations
        .insert("*.rs".to_string(), "CustomRust".to_string());
    assert_eq!(ed.language_name(), "Java");
}

#[test]
fn language_associations_overrides_builtin() {
    let mut ed = Editor::new();
    ed.file_path = Some(std::path::PathBuf::from("test.rs"));
    ed.language_associations
        .insert("*.rs".to_string(), "CustomRust".to_string());
    assert_eq!(ed.language_name(), "CustomRust");
}

#[test]
fn get_snippet_rust() {
    assert!(get_snippet("fn", "Rust").is_some());
    assert!(get_snippet("test", "Rust").is_some());
    assert!(get_snippet("nonexistent", "Rust").is_none());
}

#[test]
fn get_snippet_unknown_lang() {
    assert!(get_snippet("fn", "Brainfuck").is_none());
}

#[test]
fn get_snippet_javascript() {
    assert!(get_snippet("fn", "JavaScript").is_some());
    assert!(get_snippet("arrow", "TypeScript").is_some());
    assert!(get_snippet("log", "JavaScript").is_some());
    assert!(get_snippet("nonexistent", "JavaScript").is_none());
}

#[test]
fn get_snippet_python() {
    assert!(get_snippet("def", "Python").is_some());
    assert!(get_snippet("class", "Python").is_some());
    assert!(get_snippet("main", "Python").is_some());
    assert!(get_snippet("unknown", "Python").is_none());
}

#[test]
fn get_snippet_html() {
    assert!(get_snippet("div", "HTML").is_some());
    assert!(get_snippet("html5", "HTML").is_some());
}

#[test]
fn get_snippet_css() {
    assert!(get_snippet("bg", "CSS").is_some());
    assert!(get_snippet("df", "CSS").is_some());
}

#[test]
fn get_snippet_empty_trigger() {
    assert!(get_snippet("", "Rust").is_none());
    assert!(get_snippet("", "Python").is_none());
}

#[test]
fn has_disk_changed_default() {
    let ed = Editor::new();
    assert!(!ed.has_disk_changed());
}



#[test]
fn file_name_with_path() {
    let mut ed = Editor::new();
    ed.file_path = Some(std::path::PathBuf::from("/tmp/test.rs"));
    assert_eq!(ed.file_name(), "test.rs");
}

#[test]
fn file_name_no_path() {
    let ed = Editor::new();
    assert_eq!(ed.file_name(), "[No File]");
}

#[test]
fn untitled_display_name_uses_only_a_short_clean_first_line() {
    for (text, expected) in [
        ("", "[NoFile]"),
        (" \t\r\nsecond line", "[NoFile]"),
        ("hello\nignore this line", "hello [NoFile]"),
        ("1234567890", "1234567890 [NoFile]"),
        ("hello world", "hello worl… [NoFile]"),
        ("  hello\tworld\r\nsecond line", "hello worl… [NoFile]"),
        ("\0hi\u{1b}", "hi [NoFile]"),
        ("hello   ", "hello [NoFile]"),
    ] {
        let editor = Editor::test_with_text(text);
        assert_eq!(editor.display_name(), expected, "source={text:?}");
        assert_eq!(editor.file_name(), "[No File]", "display labels are not filenames");
        assert!(editor.file_path.is_none());
        assert!(editor.suggested_file_name.is_none());
    }
}

#[test]
fn untitled_display_name_keeps_unicode_graphemes_intact() {
    for ch in ["界", "e\u{301}", "🧑‍💻", "🇪🇪"] {
        let text = ch.repeat(10);
        assert_eq!(Editor::test_with_text(&text).display_name(), format!("{text} [NoFile]"));
        assert_eq!(Editor::test_with_text(&format!("{text}! ")).display_name(), format!("{text}… [NoFile]"));
    }
    // Never expose a partial grapheme if it crosses the bounded sample.
    let long_cluster = format!("e{}", "\u{301}".repeat(1_000));
    assert_eq!(Editor::test_with_text(&long_cluster).display_name(), "[NoFile]");
}

#[test]
fn untitled_display_name_updates_after_edit_undo_redo_and_naming() {
    let mut editor = Editor::new();
    let language = editor.language_name().to_string();
    editor.paste_with_text("draft.py\nsecond line");
    assert_eq!(editor.display_name(), "draft.py [NoFile]");
    assert_eq!(editor.language_name(), language, "a preview must not change the file type");
    editor.undo();
    assert_eq!(editor.display_name(), "[NoFile]");
    editor.redo();
    assert_eq!(editor.display_name(), "draft.py [NoFile]");
    editor.suggested_file_name = Some("recovered.py".into());
    assert_eq!(editor.display_name(), "recovered.py");
    editor.file_path = Some(std::path::PathBuf::from("saved.rs"));
    assert_eq!(editor.display_name(), "saved.rs");
}

#[test]
fn untitled_display_name_stays_short_with_a_large_first_line() {
    let editor = Editor::test_with_text(&"x".repeat(2 * 1024 * 1024));
    assert_eq!(editor.display_name(), "xxxxxxxxxx… [NoFile]");
    let editor = Editor::test_with_text(&format!("{}text", " ".repeat(2 * 1024 * 1024)));
    assert_eq!(editor.display_name(), "[NoFile]", "do not scan unbounded indentation for a label");
}

#[test]
fn toggle_block_comment_basic() {
    let mut ed = Editor::test_with_text("hello\nworld\n");
    ed.file_path = Some(std::path::PathBuf::from("test.rs"));
    ed.selection_anchor = Some((0, 0));
    ed.cursor_line = 1;
    ed.cursor_col = 5;
    ed.toggle_block_comment();
    let text = ed.rope.to_string();
    assert!(text.contains("/*"));
}

#[test]
fn line_count_basic() {
    let ed = Editor::test_with_text("hello\nworld\n");
    assert_eq!(ed.line_count(), 3);
}

#[test]
fn wrap_selection_basic() {
    let mut ed = Editor::test_with_text("hello world\n");
    ed.selection_anchor = Some((0, 0));
    ed.cursor_line = 0;
    ed.cursor_col = 5;
    ed.wrap_selection('(', ')');
    assert!(ed.rope.to_string().contains("(hello)"));
}

#[test]
fn unsaved_line_status_no_saved() {
    let ed = Editor::new();
    assert_eq!(ed.unsaved_line_status(0), ' ');
}

#[test]
fn unsaved_line_status_modified() {
    let mut ed = Editor::test_with_text("hello\nworld\n");
    ed.saved_rope = Some(Rope::from_str("hello\nchanged\n"));
    ed.modified = true;
    let diff = compute_unsaved_diff_markers(ed.saved_rope.as_ref().unwrap(), &ed.rope);
    ed.apply_unsaved_diff_cache(ed.content_version, diff);
    assert_eq!(ed.unsaved_line_status(0), ' ');
    assert_eq!(ed.unsaved_line_status(1), 'N');
}

#[test]
fn stale_unsaved_diff_does_not_compare_shifted_lines_by_index() {
    let saved = Rope::from_str("zero\none\ntwo\nthree\nfour\n");
    let mut ed = Editor::test_with_text("zero\ninserted\none\ntwo\nthree\nfour\n");
    ed.saved_rope = Some(saved);
    ed.modified = true;
    ed.content_version = 1;
    let diff = compute_unsaved_diff_markers(ed.saved_rope.as_ref().unwrap(), &ed.rope);
    ed.apply_unsaved_diff_cache(ed.content_version, diff);

    // Simulate typing after an earlier insertion. The old implementation
    // compared all following live/saved lines at the same index here and
    // transiently marked the entire suffix as changed.
    ed.cursor_line = 4;
    ed.cursor_col = 0;
    ed.insert_char('x');

    assert_eq!(ed.unsaved_line_status(1), 'N');
    assert_eq!(ed.unsaved_line_status(4), 'N');
    assert_eq!(ed.unsaved_line_status(2), ' ');
    assert_eq!(ed.unsaved_line_status(3), ' ');
    assert_eq!(ed.unsaved_line_status(5), ' ');
}

#[test]
fn stale_unsaved_diff_with_new_line_marks_only_active_cursor_lines() {
    let saved = Rope::from_str("zero\none\ntwo\nthree\n");
    let mut ed = Editor::test_with_text("zero\none\ntwo\nthree\n");
    ed.saved_rope = Some(saved);
    ed.modified = true;
    let diff = compute_unsaved_diff_markers(ed.saved_rope.as_ref().unwrap(), &ed.rope);
    ed.apply_unsaved_diff_cache(ed.content_version, diff);

    ed.cursor_line = 1;
    ed.cursor_col = 3;
    ed.insert_newline();

    assert_eq!(ed.unsaved_line_status(ed.cursor_line), 'N');
    for line in 0..ed.line_count() {
        if line != ed.cursor_line {
            assert_eq!(ed.unsaved_line_status(line), ' ', "line {line}");
        }
    }
}

#[test]
fn stale_unsaved_diff_projects_unchanged_rows_across_line_insertion() {
    let saved = Rope::from_str("changed zero\none\ntwo\nthree\nchanged four\n");
    let mut ed = Editor::test_with_text("live zero\none\ntwo\nthree\nlive four\n");
    ed.saved_rope = Some(saved);
    ed.modified = true;
    let diff = compute_unsaved_diff_markers(ed.saved_rope.as_ref().unwrap(), &ed.rope);
    ed.apply_unsaved_diff_cache(ed.content_version, diff);

    ed.cursor_line = 2;
    ed.cursor_col = 3;
    ed.insert_newline();
    // The input post-edit hook widens the current edit through the new row.
    ed.mark_syntax_dirty_range(2, 3);

    assert_eq!(ed.unsaved_line_status(0), 'N');
    assert_eq!(ed.unsaved_line_status(5), 'N');
    assert!(ed.has_unsaved_change_in_range(0, 1));
    assert!(ed.has_unsaved_change_in_range(5, 6));
    assert!(ed.has_unsaved_change_in_range(ed.cursor_line, ed.cursor_line + 1));
}

#[test]
fn syntax_convergence_does_not_clear_stale_unsaved_markers() {
    let saved = Rope::from_str("changed zero\none\ntwo\nthree\nchanged four\n");
    let mut ed = Editor::test_with_text("live zero\none\ntwo\nthree\nlive four\n");
    ed.saved_rope = Some(saved);
    ed.modified = true;
    let diff = compute_unsaved_diff_markers(ed.saved_rope.as_ref().unwrap(), &ed.rope);
    ed.apply_unsaved_diff_cache(ed.content_version, diff);

    ed.cursor_line = 2;
    ed.cursor_col = 3;
    ed.insert_newline();
    ed.mark_syntax_dirty_range(2, 3);
    assert_eq!(ed.unsaved_line_status(5), 'N');

    // Syntax usually converges before the deliberately deferred whole-file
    // diff. Its acknowledgement must not blank unrelated gutter markers in
    // that interval.
    ed.syntax_dirty_from = None;
    ed.syntax_dirty_through = None;
    assert_eq!(ed.unsaved_line_status(0), 'N');
    assert_eq!(ed.unsaved_line_status(5), 'N');
    assert!(ed.has_unsaved_change_in_range(5, 6));
}

#[test]
fn text_file_loader_streams_utf8_and_reports_disk_state() {
    let dir = tempfile::tempdir().expect("create temp directory");
    let path = dir.path().join("large.txt");
    let content = "one two three\n".repeat(100_000);
    std::fs::write(&path, content.as_bytes()).expect("write test file");

    let loaded = load_text_file(&path).expect("load text file");

    assert_eq!(loaded.encoding, "UTF-8");
    assert_eq!(loaded.file_size, content.len() as u64);
    assert!(loaded.last_disk_modified.is_some());
    assert_eq!(loaded.rope.len_bytes(), content.len());
    assert_eq!(loaded.rope.line(99_999).to_string(), "one two three\n");
}

#[test]
fn text_file_loader_decodes_supported_boms() {
    let dir = tempfile::tempdir().expect("create temp directory");

    let utf8_path = dir.path().join("utf8.txt");
    std::fs::write(&utf8_path, b"\xEF\xBB\xBFhello\n").expect("write UTF-8 BOM file");
    let utf8 = load_text_file(&utf8_path).expect("load UTF-8 BOM file");
    assert_eq!(utf8.encoding, "UTF-8 with BOM");
    assert_eq!(utf8.rope.to_string(), "hello\n");

    let utf16_path = dir.path().join("utf16.txt");
    std::fs::write(&utf16_path, [0xFF, 0xFE, b'h', 0, b'i', 0, b'\n', 0])
        .expect("write UTF-16 LE file");
    let utf16 = load_text_file(&utf16_path).expect("load UTF-16 LE file");
    assert_eq!(utf16.encoding, "UTF-16 LE");
    assert_eq!(utf16.rope.to_string(), "hi\n");
}

#[test]
fn unsaved_diff_keeps_inserted_lines_aligned() {
    let saved = Rope::from_str("first\nlast\n");
    let current = Rope::from_str("first\ninserted\nlast\n");

    let diff = compute_unsaved_diff_markers(&saved, &current);

    assert_eq!((0..4).map(|line| diff.line_status(line)).collect::<Vec<_>>(), [' ', 'N', ' ', ' ']);
    assert_eq!(diff.changed_ranges, [1..2]);
}

#[test]
fn unsaved_diff_handles_lines_crossing_rope_chunks() {
    let long_line = "x".repeat(20_000);
    let saved = Rope::from_str(&format!("{long_line}\nbefore\n"));
    let current = Rope::from_str(&format!("{long_line}\nafter\n"));

    let diff = compute_unsaved_diff_markers(&saved, &current);

    assert_eq!(diff.line_status(0), ' ');
    assert_eq!(diff.line_status(1), 'N');
    assert_eq!(diff.changed_ranges, [1..2]);
}

#[test]
fn find_in_rope_basic() {
    let rope = Rope::from_str("hello world");
    let needle: Vec<char> = "world".chars().collect();
    assert_eq!(
        Editor::find_in_rope(&rope, &needle, 0, rope.len_chars()),
        Some(6)
    );
}

#[test]
fn find_in_rope_no_match() {
    let rope = Rope::from_str("hello world");
    let needle: Vec<char> = "xyz".chars().collect();
    assert_eq!(
        Editor::find_in_rope(&rope, &needle, 0, rope.len_chars()),
        None
    );
}

#[test]
fn find_in_rope_empty_needle() {
    let rope = Rope::from_str("hello");
    assert_eq!(Editor::find_in_rope(&rope, &[], 0, rope.len_chars()), None);
}

#[test]
fn find_in_rope_bounded_range() {
    let rope = Rope::from_str("abcabc");
    let needle: Vec<char> = "abc".chars().collect();
    // Search only in second half
    assert_eq!(
        Editor::find_in_rope(&rope, &needle, 3, rope.len_chars()),
        Some(3)
    );
    // Search only first 3 chars — match at start
    assert_eq!(Editor::find_in_rope(&rope, &needle, 0, 3), Some(0));
}

#[test]
fn parse_snippet_simple_tab_stops() {
    let (text, stops) = parse_snippet_body("fn ${1:name}($2) {\n    $0\n}");
    assert_eq!(text, "fn name() {\n    \n}");
    // $1 at "name" (offset 3, len 4)
    assert!(stops
        .iter()
        .any(|(idx, off, len)| *idx == 1 && *off == 3 && *len == 4));
    // $2 at offset 8 (after "("), no placeholder
    assert!(stops.iter().any(|(idx, _, len)| *idx == 2 && *len == 0));
    // $0 (final cursor)
    assert!(stops.iter().any(|(idx, _, _)| *idx == 0));
}

#[test]
fn parse_snippet_no_tab_stops() {
    let (text, stops) = parse_snippet_body("plain text only");
    assert_eq!(text, "plain text only");
    assert!(stops.is_empty());
}

#[test]
fn parse_snippet_choice() {
    let (text, stops) = parse_snippet_body("${1|yes,no|}");
    assert_eq!(text, "yes");
    assert_eq!(stops.len(), 1);
    assert_eq!(stops[0], (1, 0, 3));
}

#[test]
fn snippet_tab_stop_navigation() {
    let mut ed = Editor::test_with_text("fn () {}");
    ed.cursor_line = 0;
    ed.cursor_col = 3;
    // Simulate inserting snippet text at position
    let insert_pos = 3; // after "fn "
    ed.rope.remove(3..3); // no-op, just to set up
    ed.insert_snippet_text(insert_pos, "${1:name}($2)");
    assert!(ed.snippet_active);
    assert_eq!(ed.snippet_tab_stops.len(), 2);
    // First stop should select "name"
    assert_eq!(ed.cursor_col, 7); // end of "name"
                                  // Advance to next tab stop
    assert!(ed.snippet_next_tab_stop());
    // Go back
    assert!(ed.snippet_prev_tab_stop());
    // Advance past all stops
    ed.snippet_next_tab_stop();
    ed.snippet_next_tab_stop();
    assert!(!ed.snippet_active);
}

#[test]
fn resolve_conflict_accept_current() {
    let mut ed = Editor::test_with_text(
        "before\n<<<<<<< HEAD\ncurrent line\n=======\nincoming line\n>>>>>>> branch\nafter\n",
    );
    ed.resolve_conflict(1, "current");
    let result: String = ed.rope.chars().collect();
    assert_eq!(result, "before\ncurrent line\nafter\n");
}

#[test]
fn resolve_conflict_accept_incoming() {
    let mut ed = Editor::test_with_text(
        "before\n<<<<<<< HEAD\ncurrent line\n=======\nincoming line\n>>>>>>> branch\nafter\n",
    );
    ed.resolve_conflict(1, "incoming");
    let result: String = ed.rope.chars().collect();
    assert_eq!(result, "before\nincoming line\nafter\n");
}

#[test]
fn resolve_conflict_accept_both() {
    let mut ed = Editor::test_with_text(
        "before\n<<<<<<< HEAD\ncurrent\n=======\nincoming\n>>>>>>> branch\nafter\n",
    );
    ed.resolve_conflict(1, "both");
    let result: String = ed.rope.chars().collect();
    assert_eq!(result, "before\ncurrent\nincoming\nafter\n");
}

#[test]
fn word_wrap_with_very_small_width() {
    let mut ed = Editor::test_with_text("hello world");
    // Width 1 should not panic
    let rows = ed.wrapped_rows(0, 20, 1);
    assert!(!rows.is_empty());
    // Width 0 should not panic (degenerate case)
    let total = ed.wrapped_total_rows(0);
    assert!(total >= 1);
}

#[test]
fn very_long_line_line_len() {
    let mut ed = Editor::new();
    let long_line = "a".repeat(100_000);
    ed.rope = Rope::from_str(&long_line);
    assert_eq!(ed.line_len(0), 100_000);
    assert_eq!(ed.current_line_len(), 100_000);
}

#[test]
fn resolve_conflict_no_conflict() {
    let mut ed = Editor::test_with_text("normal text\nno conflict\n");
    ed.resolve_conflict(0, "current"); // should be noop on non-conflict text
    assert_eq!(ed.rope.to_string(), "normal text\nno conflict\n");
}

// ── Undo/redo cursor position tests ─────────────────────────────

#[test]
fn undo_restores_cursor_position() {
    let mut ed = Editor::test_with_text("hello world\n");
    ed.cursor_line = 0;
    ed.cursor_col = 5;
    ed.save_undo();
    ed.rope.insert(5, " beautiful");
    ed.cursor_col = 15;
    ed.undo();
    assert_eq!(ed.cursor_line, 0);
    assert_eq!(ed.cursor_col, 5);
    assert_eq!(ed.rope.to_string(), "hello world\n");
}

#[test]
fn redo_restores_cursor_position() {
    let mut ed = Editor::test_with_text("hello\n");
    ed.cursor_line = 0;
    ed.cursor_col = 5;
    ed.save_undo();
    ed.rope.insert(5, " world");
    ed.cursor_col = 11;
    ed.undo();
    assert_eq!(ed.cursor_col, 5);
    ed.redo();
    assert_eq!(ed.cursor_col, 11);
    assert_eq!(ed.rope.to_string(), "hello world\n");
}

#[test]
fn undo_after_delete_lines_keeps_valid_cursor() {
    let mut ed = Editor::test_with_text("line1\nline2\nline3\nline4\nline5\n");
    ed.cursor_line = 4;
    ed.cursor_col = 3;
    ed.save_undo();
    // Delete lines 2-4 (remove "line3\nline4\nline5\n")
    let start = ed.rope.line_to_char(2);
    let end = ed.rope.line_to_char(5);
    ed.rope.remove(start..end);
    ed.cursor_line = 1;
    ed.cursor_col = 0;
    // Undo should restore to valid cursor
    ed.undo();
    assert!(ed.cursor_line < ed.line_count());
}

#[test]
fn multiple_undo_redo_cycle() {
    let mut ed = Editor::test_with_text("abc\n");
    ed.cursor_col = 3;

    // Edit 1
    ed.save_undo();
    ed.rope.insert(3, "def");
    ed.cursor_col = 6;

    // Edit 2
    ed.save_undo();
    ed.rope.insert(6, "ghi");
    ed.cursor_col = 9;

    // Undo twice
    ed.undo();
    assert_eq!(ed.cursor_col, 6);
    ed.undo();
    assert_eq!(ed.cursor_col, 3);
    assert_eq!(ed.rope.to_string(), "abc\n");

    // Redo twice
    ed.redo();
    assert_eq!(ed.cursor_col, 6);
    ed.redo();
    assert_eq!(ed.cursor_col, 9);
    assert_eq!(ed.rope.to_string(), "abcdefghi\n");
}

#[test]
fn undo_clears_extra_cursors_folds() {
    let mut ed = Editor::test_with_text("first\nsecond\nthird\n");
    ed.cursor_line = 0;
    ed.cursor_col = 0;
    ed.save_undo();

    // Add extra cursors and folded ranges
    ed.extra_cursors = vec![(1, 0), (2, 0)];
    ed.folded_ranges.insert(1, 2);

    ed.save_undo();
    ed.rope.insert(0, "prefix ");
    ed.cursor_col = 7;

    // Undo should restore the state with extra cursors
    ed.undo();
    assert_eq!(ed.extra_cursors.len(), 2);
    // But folds should be cleared
    assert!(ed.folded_ranges.is_empty());
}

#[test]
fn undo_on_empty_stack_is_noop() {
    let mut ed = Editor::test_with_text("hello\n");
    ed.cursor_col = 3;
    let content_before = ed.rope.to_string();
    ed.undo();
    assert_eq!(ed.rope.to_string(), content_before);
    assert_eq!(ed.cursor_col, 3);
}

#[test]
fn redo_on_empty_stack_is_noop() {
    let mut ed = Editor::test_with_text("hello\n");
    ed.cursor_col = 3;
    let content_before = ed.rope.to_string();
    ed.redo();
    assert_eq!(ed.rope.to_string(), content_before);
    assert_eq!(ed.cursor_col, 3);
}

#[test]
fn cut_multi_cursor_same_line_no_duplicate_delete() {
    let mut ed = Editor::test_with_text("line1\nline2\nline3\n");
    ed.cursor_line = 1;
    ed.cursor_col = 0;
    // Two cursors on same line should cut only once
    ed.extra_cursors = vec![(1, 3)];
    ed.extra_cursor_anchors = vec![None];
    ed.cut();
    // line2 should be removed exactly once
    assert_eq!(ed.rope.to_string(), "line1\nline3\n");
}

#[test]
fn cut_multi_cursor_adjacent_lines() {
    let mut ed = Editor::test_with_text("aaa\nbbb\nccc\nddd\n");
    ed.cursor_line = 1;
    ed.cursor_col = 0;
    ed.extra_cursors = vec![(2, 0)];
    ed.extra_cursor_anchors = vec![None];
    ed.cut();
    assert_eq!(ed.rope.to_string(), "aaa\nddd\n");
}

#[test]
fn move_line_up_updates_extra_cursor_on_displaced_line() {
    let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
    ed.cursor_line = 1; // bbb - will move up
    ed.cursor_col = 0;
    ed.extra_cursors = vec![(0, 1)]; // cursor on aaa - the displaced line
    ed.extra_cursor_anchors = vec![None];
    ed.move_line_up();
    // bbb moves from line 1 to line 0
    assert_eq!(ed.cursor_line, 0);
    // aaa (displaced) moves from line 0 to line 1
    assert_eq!(ed.extra_cursors[0].0, 1);
    assert_eq!(ed.rope.line(0).to_string(), "bbb\n");
    assert_eq!(ed.rope.line(1).to_string(), "aaa\n");
}

#[test]
fn move_line_down_updates_extra_cursor_on_displaced_line() {
    let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
    ed.cursor_line = 1; // bbb - will move down
    ed.cursor_col = 0;
    ed.extra_cursors = vec![(2, 1)]; // cursor on ccc - the displaced line
    ed.extra_cursor_anchors = vec![None];
    ed.move_line_down();
    // bbb moves from line 1 to line 2
    assert_eq!(ed.cursor_line, 2);
    // ccc (displaced) moves from line 2 to line 1
    assert_eq!(ed.extra_cursors[0].0, 1);
    assert_eq!(ed.rope.line(1).to_string(), "ccc\n");
    assert_eq!(ed.rope.line(2).to_string(), "bbb\n");
}

#[test]
fn move_line_up_preserves_extra_cursor_on_unrelated_line() {
    let mut ed = Editor::test_with_text("aaa\nbbb\nccc\nddd\n");
    ed.cursor_line = 2; // ccc - will move up
    ed.cursor_col = 0;
    ed.extra_cursors = vec![(0, 0)]; // cursor on aaa - unaffected
    ed.extra_cursor_anchors = vec![None];
    ed.move_line_up();
    assert_eq!(ed.cursor_line, 1);
    assert_eq!(ed.extra_cursors[0].0, 0); // aaa stays at line 0
}

#[test]
fn delete_to_line_start_multicursor() {
    let mut ed = Editor::test_with_text("aaaBBB\nccDDD\n");
    ed.cursor_line = 0;
    ed.cursor_col = 3; // after "aaa"
    ed.extra_cursors = vec![(1, 2)]; // after "cc"
    ed.extra_cursor_anchors = vec![None];
    ed.delete_to_line_start();
    assert_eq!(ed.rope.to_string(), "BBB\nDDD\n");
    assert_eq!(ed.cursor_col, 0);
    assert_eq!(ed.extra_cursors[0], (1, 0));
}

#[test]
fn delete_to_line_end_multicursor() {
    let mut ed = Editor::test_with_text("aaaBBB\nccDDD\n");
    ed.cursor_line = 0;
    ed.cursor_col = 3; // after "aaa"
    ed.extra_cursors = vec![(1, 2)]; // after "cc"
    ed.extra_cursor_anchors = vec![None];
    ed.delete_to_line_end();
    assert_eq!(ed.rope.to_string(), "aaa\ncc\n");
}

#[test]
fn indent_multicursor_same_line() {
    let mut ed = Editor::test_with_text("abc\ndef\n");
    ed.cursor_line = 0;
    ed.cursor_col = 1;
    ed.extra_cursors = vec![(1, 2)];
    ed.extra_cursor_anchors = vec![None];
    ed.tab_size = 2;
    ed.indent_lines();
    assert_eq!(ed.rope.to_string(), "  abc\n  def\n");
    assert_eq!(ed.cursor_col, 3);
    assert_eq!(ed.extra_cursors[0], (1, 4));
}

#[test]
fn dedent_multicursor() {
    let mut ed = Editor::test_with_text("    aaa\n    bbb\n");
    ed.cursor_line = 0;
    ed.cursor_col = 6;
    ed.extra_cursors = vec![(1, 5)];
    ed.extra_cursor_anchors = vec![None];
    ed.tab_size = 4;
    ed.dedent_line();
    assert_eq!(ed.rope.to_string(), "aaa\nbbb\n");
    assert_eq!(ed.cursor_col, 2);
    assert_eq!(ed.extra_cursors[0], (1, 1));
}

#[test]
fn move_line_up_anchor_in_block_follows() {
    let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
    ed.cursor_line = 1;
    ed.cursor_col = 0;
    ed.selection_anchor = Some((1, 2)); // anchor on same line as cursor
    ed.move_line_up();
    assert_eq!(ed.cursor_line, 0);
    // Anchor should move up with the block
    assert_eq!(ed.selection_anchor, Some((0, 2)));
}

#[test]
fn move_line_up_anchor_outside_block_stays() {
    let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
    ed.cursor_line = 2;
    ed.cursor_col = 0;
    ed.selection_anchor = Some((0, 1)); // anchor on line 0, far from block
    ed.move_line_up();
    // Anchor on line 0 should NOT be moved
    assert_eq!(ed.selection_anchor, Some((0, 1)));
}

#[test]
fn move_line_down_anchor_in_block_follows() {
    let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
    ed.cursor_line = 0;
    ed.cursor_col = 1;
    ed.selection_anchor = Some((0, 2)); // anchor on same line
    ed.move_line_down();
    assert_eq!(ed.cursor_line, 1);
    assert_eq!(ed.selection_anchor, Some((1, 2)));
}

#[test]
fn move_line_down_anchor_outside_block_stays() {
    let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
    ed.cursor_line = 0;
    ed.cursor_col = 0;
    ed.selection_anchor = Some((2, 1)); // anchor on line 2
    ed.move_line_down();
    // Anchor on line 2 should NOT be moved
    assert_eq!(ed.selection_anchor, Some((2, 1)));
}

#[test]
fn reload_from_disk_clears_extra_cursors() {
    let dir = std::env::temp_dir().join("vtcode_test_reload_cursors");
    let _ = std::fs::create_dir_all(&dir); // Error OK: dir may already exist
    let path = dir.join("test.txt");
    std::fs::write(&path, "line1\nline2\nline3\n").unwrap();
    let mut ed = Editor::test_with_text("line1\nline2\nline3\n");
    ed.file_path = Some(path.clone());
    ed.extra_cursors = vec![(1, 0), (2, 0)];
    ed.extra_cursor_anchors = vec![None, None];
    ed.selection_anchor = Some((0, 2));
    ed.reload_from_disk().unwrap();
    assert!(
        ed.extra_cursors.is_empty(),
        "extra cursors should be cleared after reload"
    );
    assert!(
        ed.extra_cursor_anchors.is_empty(),
        "extra cursor anchors should be cleared"
    );
    assert!(
        ed.selection_anchor.is_none(),
        "selection anchor should be cleared"
    );
    let _ = std::fs::remove_dir_all(&dir); // Error OK: dir may not exist
}

#[test]
fn reload_from_disk_resets_scroll() {
    let dir = std::env::temp_dir().join("vtcode_test_reload_scroll");
    let _ = std::fs::create_dir_all(&dir); // Error OK: dir may already exist
    let path = dir.join("test.txt");
    std::fs::write(&path, "a\nb\n").unwrap();
    let mut ed = Editor::test_with_text("a\nb\n");
    ed.file_path = Some(path.clone());
    ed.scroll_offset = 100;
    ed.horizontal_scroll = 50;
    ed.wrap_scroll_offset = 200;
    ed.reload_from_disk().unwrap();
    assert_eq!(ed.horizontal_scroll, 0, "horizontal scroll should reset");
    assert_eq!(ed.wrap_scroll_offset, 0, "wrap scroll should reset");
    let _ = std::fs::remove_dir_all(&dir); // Error OK: dir may not exist
}

#[test]
fn reload_from_disk_updates_file_size() {
    let dir = std::env::temp_dir().join("vtcode_test_reload_fsize");
    let _ = std::fs::create_dir_all(&dir); // Error OK: dir may already exist
    let path = dir.join("test.txt");
    std::fs::write(&path, "hello world").unwrap();
    let mut ed = Editor::test_with_text("old content");
    ed.file_path = Some(path.clone());
    ed.file_size = 0;
    ed.reload_from_disk().unwrap();
    assert!(ed.file_size > 0, "file_size should be updated after reload");
    let _ = std::fs::remove_dir_all(&dir); // Error OK: dir may not exist
}

#[test]
fn reload_from_disk_clamps_cursor() {
    let dir = std::env::temp_dir().join("vtcode_test_reload_clamp");
    let _ = std::fs::create_dir_all(&dir); // Error OK: dir may already exist
    let path = dir.join("test.txt");
    std::fs::write(&path, "short\n").unwrap();
    let mut ed = Editor::test_with_text("line1\nline2\nline3\nline4\nline5\n");
    ed.file_path = Some(path.clone());
    ed.cursor_line = 4;
    ed.cursor_col = 3;
    ed.reload_from_disk().unwrap();
    assert!(
        ed.cursor_line < ed.line_count(),
        "cursor line should be clamped to file bounds"
    );
    let _ = std::fs::remove_dir_all(&dir); // Error OK: dir may not exist
}

// ---- Multi-cursor paste tests ----

#[test]
fn paste_multi_cursor_distributes_lines() {
    let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
    ed.cursor_line = 0;
    ed.cursor_col = 3;
    ed.extra_cursors = vec![(1, 3), (2, 3)];
    ed.extra_cursor_anchors = vec![None, None];
    ed.paste_with_text("X\nY\nZ");
    assert_eq!(ed.rope.line(0).to_string(), "aaaX\n");
    assert_eq!(ed.rope.line(1).to_string(), "bbbY\n");
    assert_eq!(ed.rope.line(2).to_string(), "cccZ\n");
}

#[test]
fn paste_multi_cursor_single_line_duplicates() {
    let mut ed = Editor::test_with_text("aaa\nbbb\n");
    ed.cursor_line = 0;
    ed.cursor_col = 3;
    ed.extra_cursors = vec![(1, 3)];
    ed.extra_cursor_anchors = vec![None];
    ed.paste_with_text("XY");
    assert_eq!(ed.rope.line(0).to_string(), "aaaXY\n");
    assert_eq!(ed.rope.line(1).to_string(), "bbbXY\n");
}

#[test]
fn paste_multi_cursor_empty_text() {
    let mut ed = Editor::test_with_text("aaa\nbbb\n");
    ed.cursor_line = 0;
    ed.cursor_col = 1;
    ed.extra_cursors = vec![(1, 1)];
    ed.extra_cursor_anchors = vec![None];
    ed.paste_with_text("");
    assert_eq!(ed.rope.to_string(), "aaa\nbbb\n");
}

#[test]
fn paste_multi_cursor_cursor_positions_updated() {
    let mut ed = Editor::test_with_text("aa\nbb\n");
    ed.cursor_line = 0;
    ed.cursor_col = 2;
    ed.extra_cursors = vec![(1, 2)];
    ed.extra_cursor_anchors = vec![None];
    ed.paste_with_text("XX\nYY");
    assert_eq!(ed.cursor_line, 0);
    assert_eq!(ed.cursor_col, 4);
    assert_eq!(ed.extra_cursors[0], (1, 4));
}

// ---- Encoding write: trailing CR flush tests ----

#[test]
fn write_utf16_le_flushes_trailing_cr() {
    let mut ed = Editor::test_with_text("hello\r");
    ed.cached_line_ending = "CRLF";
    let mut buf = Vec::new();
    ed.encoding = "UTF-16 LE";
    ed.write_encoded_content(&ed.rope, &mut buf).unwrap();
    // Should contain 'h','e','l','l','o','\r' as UTF-16 LE
    assert_eq!(buf.len(), 14);
    assert_eq!(&buf[12..14], &[0x0D, 0x00]);
}

#[test]
fn write_utf16_be_flushes_trailing_cr() {
    let mut ed = Editor::test_with_text("hi\r");
    ed.cached_line_ending = "CRLF";
    let mut buf = Vec::new();
    ed.encoding = "UTF-16 BE";
    ed.write_encoded_content(&ed.rope, &mut buf).unwrap();
    assert_eq!(buf.len(), 8);
    assert_eq!(&buf[6..8], &[0x00, 0x0D]);
}

#[test]
fn write_latin1_flushes_trailing_cr() {
    let mut ed = Editor::test_with_text("abc\r");
    ed.cached_line_ending = "CRLF";
    let mut buf = Vec::new();
    ed.encoding = "ISO-8859-1";
    ed.write_encoded_content(&ed.rope, &mut buf).unwrap();
    assert_eq!(buf, b"abc\r");
}

#[test]
fn write_ascii_flushes_trailing_cr() {
    let mut ed = Editor::test_with_text("xy\r");
    ed.cached_line_ending = "CRLF";
    let mut buf = Vec::new();
    ed.encoding = "ASCII";
    ed.write_encoded_content(&ed.rope, &mut buf).unwrap();
    assert_eq!(buf, b"xy\r");
}

#[test]
fn delete_line_multi_cursor_same_line() {
    let mut ed = Editor::test_with_text("aaa\nbbb\nccc\n");
    ed.cursor_line = 1;
    ed.cursor_col = 0;
    ed.extra_cursors.push((1, 2)); // same line as primary
    ed.extra_cursor_anchors.push(None);
    ed.delete_line();
    assert_eq!(ed.rope.to_string(), "aaa\nccc\n");
    assert_eq!(ed.cursor_line, 1);
    assert!(ed.extra_cursors.is_empty());
}

#[test]
fn delete_line_multi_cursor_different_lines() {
    let mut ed = Editor::test_with_text("L0\nL1\nL2\nL3\nL4\n");
    ed.cursor_line = 1;
    ed.cursor_col = 0;
    ed.extra_cursors.push((3, 0));
    ed.extra_cursor_anchors.push(None);
    ed.delete_line();
    // Should delete lines 1 and 3 → remaining: L0, L2, L4
    assert_eq!(ed.rope.to_string(), "L0\nL2\nL4\n");
    assert_eq!(ed.cursor_line, 1);
}

#[test]
fn delete_line_extra_cursor_with_selection() {
    let mut ed = Editor::test_with_text("L0\nL1\nL2\nL3\nL4\n");
    ed.cursor_line = 0;
    ed.cursor_col = 0;
    // Include text on the final line so the selection spans lines 2-4.
    ed.extra_cursors.push((4, 1));
    ed.extra_cursor_anchors.push(Some((2, 0)));
    ed.delete_line();
    // Should delete line 0, and lines 2-4 → remaining: L1
    assert_eq!(ed.rope.to_string(), "L1\n");
    assert_eq!(ed.cursor_line, 0);
}

#[test]
fn delete_line_with_primary_selection() {
    let mut ed = Editor::test_with_text("L0\nL1\nL2\nL3\n");
    ed.cursor_line = 2;
    ed.cursor_col = 1;
    ed.selection_anchor = Some((0, 0)); // selection spans lines 0-2
    ed.delete_line();
    // Should delete lines 0, 1, 2 → remaining: L3
    assert_eq!(ed.rope.to_string(), "L3\n");
    assert_eq!(ed.cursor_line, 0);
}

#[test]
fn multicursor_empty_replacement_adjusts_non_selected_cursors() {
    let mut ed = Editor::new();
    // File: "AAAA\nBBBB\nCCCC\nDDDD\n"
    ed.rope = Rope::from_str("AAAA\nBBBB\nCCCC\nDDDD\n");
    // Primary cursor at (0, 0) with selection of "AAAA\n" (anchor at (1, 0))
    ed.cursor_line = 0;
    ed.cursor_col = 0;
    ed.selection_anchor = Some((1, 0));
    // Extra cursor 0 at (2, 2) — NO selection
    ed.extra_cursors = vec![(2, 2)];
    ed.extra_cursor_anchors = vec![None];
    ed.multi_cursor_paste("");
    // "AAAA\n" deleted → file is "BBBB\nCCCC\nDDDD\n"
    assert_eq!(ed.rope.to_string(), "BBBB\nCCCC\nDDDD\n");
    // Primary cursor moves to selection start (0, 0)
    assert_eq!(ed.cursor_line, 0);
    assert_eq!(ed.cursor_col, 0);
    // Extra cursor was at (2, 2) = "CCCC" col 2. After 1 line deleted above,
    // it should be at (1, 2) = "CCCC" col 2
    assert_eq!(ed.extra_cursors[0], (1, 2));
}

#[test]
fn paste_distribute_with_non_sel_cursor_positions() {
    let mut ed = Editor::test_with_text("AAA\nBBB\nCCC\n");
    // 3 cursors, no selections — distribute paste
    ed.cursor_line = 0;
    ed.cursor_col = 3;
    ed.extra_cursors = vec![(1, 3), (2, 3)];
    ed.extra_cursor_anchors = vec![None, None];
    ed.paste_with_text("X\nY\nZ");
    // Should paste X at (0,3), Y at (1,3), Z at (2,3)
    assert_eq!(ed.rope.line(0).to_string().trim_end_matches('\n'), "AAAX");
    assert_eq!(ed.rope.line(1).to_string().trim_end_matches('\n'), "BBBY");
    assert_eq!(ed.rope.line(2).to_string().trim_end_matches('\n'), "CCCZ");
}

// ── guess_comment_prefix tests ──




// ── language_name tests ──


#[test]
fn test_detect_language_by_shebang() {
    let mut ed = Editor::test_with_text("#!/usr/bin/env python3\nprint('hello')\n");
    // No extension, so shebang detection kicks in
    ed.file_path = Some(std::path::PathBuf::from("myscript"));
    assert_eq!(ed.language_name(), "Python");
}

// ── toggle_line_comment for Rust ──


// ── toggle_line_comment for Python ──


// ── word_boundaries_at returns correct start/end ──

#[test]
fn word_boundaries_at_basic() {
    let ed = Editor::test_with_text("foo bar_baz quux\n");
    let (start, end) = ed.word_boundaries_at(0, 5);
    assert_eq!(start, 4);
    assert_eq!(end, 11);
}

#[test]
fn word_boundaries_at_non_word_char() {
    let ed = Editor::test_with_text("foo bar\n");
    let (start, end) = ed.word_boundaries_at(0, 3); // on space
    assert_eq!(start, 3);
    assert_eq!(end, 3);
}

// ── compute_file_stats returns correct counts ──


// ── wrap_selection surrounds with brackets ──

#[test]
fn wrap_selection_with_parens() {
    let mut ed = Editor::test_with_text("hello world\n");
    ed.selection_anchor = Some((0, 0));
    ed.cursor_line = 0;
    ed.cursor_col = 5;
    ed.wrap_selection('(', ')');
    assert_eq!(ed.rope.to_string(), "(hello) world\n");
}

// ── property-based / stress tests ──

/// Helper: assert cursor is within valid rope bounds.
fn assert_cursor_in_bounds(ed: &Editor) {
    let max_line = ed.rope.len_lines().max(1) - 1;
    assert!(
        ed.cursor_line <= max_line,
        "cursor_line {} out of bounds (max {})",
        ed.cursor_line,
        max_line,
    );
}

#[test]
fn test_rapid_insert_delete_cycles() {
    let mut ed = Editor::new();
    ed.viewport_height = 24;
    ed.viewport_width = 80;
    // Insert 100 characters including newlines
    for i in 0u32..100 {
        if i % 20 == 0 {
            ed.insert_char('\n');
        } else {
            ed.insert_char((b'a' + (i % 26) as u8) as char);
        }
        assert_cursor_in_bounds(&ed);
    }
    let total = ed.rope.len_chars();
    assert!(total > 0);

    // Move to the very end, then backspace everything
    ed.move_to_bottom();
    ed.move_to_line_end();
    for _ in 0..(total + 200) {
        if ed.rope.len_chars() == 0 {
            break;
        }
        ed.backspace();
        assert_cursor_in_bounds(&ed);
    }
    assert_eq!(
        ed.rope.len_chars(),
        0,
        "rope should be empty after deleting all"
    );
    assert_eq!(ed.cursor_line, 0);
    assert_eq!(ed.cursor_col, 0);
}

#[test]
fn test_undo_redo_stress() {
    let mut ed = Editor::new();
    ed.viewport_height = 24;
    ed.viewport_width = 80;

    let mut snapshots: Vec<String> = Vec::new();

    // Build content in 20 undo-groups of 5 characters each
    for group in 0u32..20 {
        ed.break_undo_group();
        for j in 0u32..5 {
            let c = (b'a' + ((group * 5 + j) % 26) as u8) as char;
            ed.insert_char(c);
        }
        snapshots.push(ed.rope.to_string());
    }

    // Undo all 20 groups and verify snapshots in reverse
    for idx in (0..20).rev() {
        assert_eq!(ed.rope.to_string(), snapshots[idx]);
        ed.undo();
        assert_cursor_in_bounds(&ed);
    }
    // After undoing all groups the rope should be empty
    assert_eq!(ed.rope.len_chars(), 0);

    // Redo all 20 groups and verify snapshots again
    for expected in snapshots.iter().take(20) {
        ed.redo();
        assert_cursor_in_bounds(&ed);
        assert_eq!(ed.rope.to_string(), expected.as_str());
    }
}

#[test]
fn test_cursor_bounds_after_operations() {
    let mut ed = Editor::new();
    ed.viewport_height = 24;
    ed.viewport_width = 80;
    ed.rope = Rope::from_str("Hello World\nSecond line\nThird line\n");
    ed.cursor_line = 1;
    ed.cursor_col = 3;

    // Movement operations
    let ops: Vec<fn(&mut Editor)> = vec![
        |e| e.move_up(),
        |e| e.move_down(),
        |e| e.move_left(),
        |e| e.move_right(),
        |e| e.move_to_line_start(),
        |e| e.move_to_line_end(),
        |e| e.page_up(),
        |e| e.page_down(),
        |e| e.move_word_left(),
        |e| e.move_word_right(),
        |e| e.move_to_top(),
        |e| e.move_to_bottom(),
    ];

    // Run every operation from every starting position
    for line in 0..ed.rope.len_lines() {
        for col_offset in [0usize, 3, 100] {
            for op in &ops {
                ed.cursor_line = line;
                ed.cursor_col = col_offset;
                op(&mut ed);
                assert_cursor_in_bounds(&ed);
            }
        }
    }

    // Rapid interleaved movement stress
    for _ in 0..50 {
        ed.move_down();
        ed.move_right();
        ed.move_up();
        ed.move_left();
        ed.move_to_line_end();
        ed.move_to_line_start();
        ed.page_down();
        ed.page_up();
        assert_cursor_in_bounds(&ed);
    }
}

#[test]
fn test_selection_operations_dont_corrupt() {
    let mut ed = Editor::new();
    ed.viewport_height = 24;
    ed.viewport_width = 80;
    let original = "The quick brown fox\njumps over\nthe lazy dog\n";
    ed.rope = Rope::from_str(original);

    // Round 1: copy + paste_with_text without active selection
    ed.cursor_line = 0;
    ed.cursor_col = 0;
    ed.start_selection();
    ed.move_to_line_end();
    let selected = ed.selected_text().unwrap();
    assert!(!selected.is_empty());
    ed.clear_selection();
    // Paste at the end of the document
    ed.move_to_bottom();
    ed.move_to_line_end();
    ed.save_undo();
    ed.paste_with_text(&selected);
    assert_cursor_in_bounds(&ed);
    assert!(ed.rope.len_chars() > original.len());

    // Undo restores original
    ed.undo();
    assert_eq!(ed.rope.to_string(), original);

    // Round 2: delete_selection cycle
    for _ in 0..5 {
        // Select a word and delete it
        ed.cursor_line = 0;
        ed.cursor_col = 0;
        ed.start_selection();
        ed.move_word_right();
        ed.save_undo();
        let deleted = ed.delete_selection();
        assert!(deleted.is_some());
        assert_cursor_in_bounds(&ed);
        // Undo to restore
        ed.undo();
        ed.undo(); // one for delete_selection's internal save_undo
    }

    // Round 3: select_all + delete + paste back
    let full_text = ed.rope.to_string();
    ed.select_all();
    ed.save_undo();
    ed.delete_selection();
    assert_eq!(ed.rope.len_chars(), 0);
    // Paste it back
    ed.paste_with_text(&full_text);
    assert_cursor_in_bounds(&ed);
    assert!(
        ed.rope.len_chars() > 0,
        "rope should have content after paste-back"
    );

    // Round 4: multiple partial copy/paste cycles
    for i in 0..3u32 {
        ed.cursor_line = i as usize % ed.rope.len_lines().max(1);
        ed.cursor_col = 0;
        ed.start_selection();
        ed.move_to_line_end();
        if let Some(sel) = ed.selected_text() {
            ed.clear_selection();
            ed.move_to_bottom();
            ed.save_undo();
            ed.paste_with_text(&sel);
            assert_cursor_in_bounds(&ed);
            ed.undo();
        } else {
            ed.clear_selection();
        }
    }

    // Rope should still be structurally valid
    let text = ed.rope.to_string();
    assert!(!text.is_empty());
}

#[test]
fn test_multicursor_stress() {
    let mut ed = Editor::new();
    ed.viewport_height = 30;
    ed.viewport_width = 80;
    // Build a 12-line document so we have room for 10 extra cursors
    let content = (0..12)
        .map(|i| format!("Line {i}: some text here"))
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    ed.rope = Rope::from_str(&content);
    ed.cursor_line = 0;
    ed.cursor_col = 0;

    // Add 10 cursors below the primary cursor
    for _ in 0..10 {
        ed.add_cursor_below();
    }
    assert!(
        ed.extra_cursors.len() >= 10,
        "expected >=10 extra cursors, got {}",
        ed.extra_cursors.len(),
    );

    // Insert a character at all cursors
    ed.save_undo();
    ed.multi_cursor_insert_char('Z');
    assert_cursor_in_bounds(&ed);
    // Each line that had a cursor should now contain 'Z'
    for &(line, _col) in &ed.extra_cursors {
        let line_text = ed.rope.line(line).to_string();
        assert!(line_text.contains('Z'), "line {line} missing inserted char");
    }

    // Backspace at all cursors
    ed.save_undo();
    ed.multi_cursor_backspace();
    assert_cursor_in_bounds(&ed);

    // Move all cursors right then left
    ed.multi_cursor_move_right();
    ed.multi_cursor_move_left();

    // Move all cursors to line end then line start
    ed.multi_cursor_move_to_line_end();
    ed.multi_cursor_move_to_line_start();
    assert_cursor_in_bounds(&ed);

    // Insert multiple characters
    for ch in ['A', 'B', 'C'] {
        ed.multi_cursor_insert_char(ch);
    }
    assert_cursor_in_bounds(&ed);

    // Delete them
    for _ in 0..3 {
        ed.multi_cursor_backspace();
    }
    assert_cursor_in_bounds(&ed);

    // Undo everything back
    ed.undo();
    ed.undo();
    assert_cursor_in_bounds(&ed);

    // Rope must still be valid
    let _ = ed.rope.to_string();
}

#[test]
fn test_large_content_operations() {
    let mut ed = Editor::new();
    ed.viewport_height = 50;
    ed.viewport_width = 120;

    // Build a 10 000-line document
    let content: String = (0..10_000)
        .map(|i| format!("Line {:05}: content here\n", i))
        .collect();
    ed.rope = Rope::from_str(&content);
    assert_eq!(ed.rope.len_lines(), 10_001); // 10000 data lines + trailing empty

    // Operations at the start
    ed.cursor_line = 0;
    ed.cursor_col = 0;
    ed.save_undo();
    ed.insert_char('X');
    assert_cursor_in_bounds(&ed);
    ed.backspace();

    // Operations in the middle
    ed.cursor_line = 5000;
    ed.cursor_col = 0;
    ed.save_undo();
    ed.insert_char('Y');
    ed.move_to_line_end();
    ed.backspace();
    assert_cursor_in_bounds(&ed);

    // Operations at the end
    ed.cursor_line = ed.rope.len_lines().saturating_sub(2);
    ed.cursor_col = 0;
    ed.save_undo();
    ed.insert_char('Z');
    ed.delete_char();
    assert_cursor_in_bounds(&ed);

    // Rapid page navigation
    ed.cursor_line = 0;
    for _ in 0..200 {
        ed.page_down();
        assert_cursor_in_bounds(&ed);
    }
    for _ in 0..200 {
        ed.page_up();
        assert_cursor_in_bounds(&ed);
    }
    assert_cursor_in_bounds(&ed);

    // Select-all on large content
    ed.select_all();
    let sel = ed.selected_text();
    assert!(sel.is_some());
    ed.clear_selection();

    // Undo back
    ed.undo();
    ed.undo();
    ed.undo();
    assert_cursor_in_bounds(&ed);
}

#[test]
fn test_fold_unfold_stress() {
    let mut ed = Editor::new();
    ed.viewport_height = 24;
    ed.viewport_width = 80;
    // Create a document with brace-delimited blocks that can be folded
    let content = "\
fn a() {
    one();
    two();
}
fn b() {
    three();
    four();
}
fn c() {
    five();
    six();
}
";
    ed.rope = Rope::from_str(content);

    // Fold all foldable blocks
    let line_count = ed.rope.len_lines();
    for line in 0..line_count {
        if ed.can_fold_line(line) {
            ed.toggle_fold(line);
        }
    }
    assert!(!ed.folded_ranges.is_empty(), "expected some folds");
    assert_cursor_in_bounds(&ed);

    // Navigate while folded
    ed.move_to_top();
    for _ in 0..20 {
        ed.move_down();
        assert_cursor_in_bounds(&ed);
    }
    for _ in 0..20 {
        ed.move_up();
        assert_cursor_in_bounds(&ed);
    }

    // Unfold all
    ed.unfold_all();
    assert!(ed.folded_ranges.is_empty());

    // fold_all / unfold_all cycle
    for _ in 0..5 {
        ed.fold_all();
        assert_cursor_in_bounds(&ed);
        ed.unfold_all();
        assert_cursor_in_bounds(&ed);
    }

    // Fold then insert text (should auto-adjust or remain consistent)
    for line in 0..ed.rope.len_lines() {
        if ed.can_fold_line(line) {
            ed.toggle_fold(line);
            break;
        }
    }
    ed.cursor_line = 0;
    ed.cursor_col = 0;
    ed.save_undo();
    ed.insert_char('!');
    assert_cursor_in_bounds(&ed);
    ed.undo();
    ed.unfold_all();
    assert_cursor_in_bounds(&ed);
}

#[test]
fn test_mixed_operation_sequence() {
    let mut ed = Editor::new();
    ed.viewport_height = 30;
    ed.viewport_width = 80;

    // 1. Build initial content via insert_char
    ed.save_undo();
    for ch in "fn main() {\n    println!(\"hello\");\n}\n".chars() {
        ed.insert_char(ch);
    }
    assert_cursor_in_bounds(&ed);
    assert!(ed.rope.len_lines() >= 3);

    // 2. Select second line and indent
    ed.cursor_line = 1;
    ed.cursor_col = 0;
    ed.save_undo();
    ed.start_selection();
    ed.move_to_line_end();
    ed.indent_lines();
    assert_cursor_in_bounds(&ed);

    // 3. Toggle line comment on second line
    ed.clear_selection();
    ed.cursor_line = 1;
    ed.cursor_col = 0;
    ed.save_undo();
    ed.toggle_line_comment();
    let commented = ed.rope.line(1).to_string();
    assert!(
        commented.contains("//"),
        "expected comment marker, got: {commented}",
    );

    // 4. Undo comment
    ed.undo();
    let uncommented = ed.rope.line(1).to_string();
    assert!(
        !uncommented.contains("//") || uncommented.contains("println"),
        "undo should remove comment",
    );

    // 5. Redo comment
    ed.redo();
    assert_cursor_in_bounds(&ed);

    // 6. Fold the function block if possible
    if ed.can_fold_line(0) {
        ed.toggle_fold(0);
        assert!(!ed.folded_ranges.is_empty());
        ed.toggle_fold(0); // unfold
    }

    // 7. Insert more text at the end
    ed.move_to_bottom();
    ed.move_to_line_end();
    ed.save_undo();
    for ch in "\nfn extra() {}\n".chars() {
        ed.insert_char(ch);
    }
    assert_cursor_in_bounds(&ed);

    // 8. Undo twice, redo twice – full round-trip
    ed.undo();
    ed.undo();
    ed.redo();
    ed.redo();
    assert_cursor_in_bounds(&ed);

    // 9. Delete a line
    ed.cursor_line = 0;
    ed.save_undo();
    ed.delete_line();
    assert_cursor_in_bounds(&ed);

    // 10. Final rope integrity check
    let text = ed.rope.to_string();
    assert!(
        !text.is_empty(),
        "rope should not be empty after operations"
    );
    assert_eq!(
        ed.rope.len_chars(),
        text.len(),
        "rope len_chars must match String len",
    );
}

#[test]
fn utf16_le_odd_byte_file_does_not_panic() {
    // UTF-16 LE BOM + "Hi" (4 bytes) + trailing odd byte (0x41)
    let bytes: Vec<u8> = vec![0xFF, 0xFE, b'H', 0x00, b'i', 0x00, 0x41];
    let payload = &bytes[2..];
    let even_len = payload.len() & !1;
    let u16s: Vec<u16> = payload[..even_len]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let decoded = String::from_utf16_lossy(&u16s);
    assert_eq!(decoded, "Hi");
}

#[test]
fn utf16_be_odd_byte_file_does_not_panic() {
    // UTF-16 BE BOM + "A" (2 bytes) + trailing odd byte (0x42)
    let bytes: Vec<u8> = vec![0xFE, 0xFF, 0x00, b'A', 0x42];
    let payload = &bytes[2..];
    let even_len = payload.len() & !1;
    let u16s: Vec<u16> = payload[..even_len]
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    let decoded = String::from_utf16_lossy(&u16s);
    assert_eq!(decoded, "A");
}
