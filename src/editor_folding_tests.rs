use crate::editor::Editor;

const NESTED_CPP: &str = include_str!("../tests/fixtures/folding/nested-preprocessor.cpp");

fn line(source: &str, marker: &str) -> usize {
    source.lines().position(|line| line.trim() == marker).unwrap_or_else(|| panic!("missing {marker}"))
}

fn cpp(source: &str) -> Editor {
    let mut editor = Editor::test_with_text(source);
    editor.file_path = Some("folding.cpp".into());
    editor
}

#[test]
fn nested_cpp_folds_span_outdented_directives_and_allman_braces() {
    for source in [NESTED_CPP.to_owned(), NESTED_CPP.replace('\n', "\r\n")] {
        let editor = cpp(&source);
        for (start, end) in [
            ("namespace sample {", "} // namespace"),
            ("struct Processor", "}; // Processor"),
            ("int run(int limit)", "} // run"),
            ("if (limit > 0) {", "} // positive_limit"),
            ("for (int i = 0; i < limit; ++i)", "} // loop"),
            ("if (i % 2 == 0) {", "} // even"),
            ("if (i > 4) {", "} // platform_limit"),
            ("else", "} // small_platform_value"),
            ("if (total < 0)", "} // negative_total"),
            ("while (FOLD_NONZERO(total)) {", "} // negative_loop"),
            ("int fallback(int value) {", "} // fallback_method"),
            ("if (value > 0)", "} // fallback_positive"),
            ("#if FOLD_CACHE", "#endif // cache"),
            ("#ifdef _WIN32", "#endif // platform"),
            ("#else // platform", "#endif // platform"),
            ("#elif FOLD_FAST_PATH", "#else // cache_fallback"),
            ("#else // cache_fallback", "#endif // cache"),
            ("#ifndef FOLD_IGNORE_NEGATIVE", "#endif // negative"),
        ] {
            let start_line = line(&source, start);
            let expected_end = line(&source, end);
            assert_eq!(editor.find_fold_end(start_line), Some(expected_end), "{start}");
            assert!(editor.can_fold_line(start_line), "missing gutter hint for {start}");
        }
        let start = line(&source, "#define FOLD_ASSERT(condition) \\");
        assert_eq!(editor.find_fold_end(start), Some(line(&source, "} while (false)") + 1));
        assert!(editor.can_fold_line(start));
    }
}

#[test]
fn collapsing_nested_cpp_keeps_following_methods_and_closing_lines_visible() {
    let mut editor = cpp(NESTED_CPP);
    let inner = line(NESTED_CPP, "if (i % 2 == 0) {");
    let start = line(NESTED_CPP, "int run(int limit)");
    let end = line(NESTED_CPP, "} // run");
    editor.toggle_fold(inner);
    editor.toggle_fold(start);
    assert_eq!(editor.folded_ranges.len(), 1);
    assert_eq!(editor.folded_ranges.get(&start), Some(&end));
    for hidden in start + 1..end { assert!(editor.is_line_folded(hidden), "line {hidden}"); }
    assert!(!editor.is_line_folded(end));
    assert!(!editor.is_line_folded(line(NESTED_CPP, "int fallback(int value) {")));
    assert_eq!(editor.visible_line_at(start, 1), Some(end));
    editor.toggle_fold(start);
    assert!(editor.folded_ranges.is_empty());
}

#[test]
fn folding_cpp_by_level_keeps_ranges_disjoint_and_navigation_consistent() {
    for level in 1..=7 {
        let mut editor = cpp(NESTED_CPP);
        editor.fold_at_level(level);
        let mut previous_end = 0;
        for (&start, &end) in &editor.folded_ranges {
            assert!(start >= previous_end, "overlapping ranges at level {level}");
            for hidden in start + 1..end {
                assert!(editor.is_line_folded(hidden), "level={level}, line={hidden}");
            }
            assert_eq!(editor.visible_line_at(start, 1), Some(end));
            previous_end = end;
        }
        assert_eq!(editor.visible_line_count(), editor.visible_lines(0, editor.line_count()).len());
    }
}

#[test]
fn duplicate_and_invalid_lsp_folds_choose_the_largest_valid_range() {
    let mut editor = Editor::test_with_text("a\nb\nc\nd\ne\nf\ng\n");
    editor.lsp_folding_ranges = vec![(0, 1), (0, 5), (0, 0), (0, usize::MAX)];
    assert_eq!(editor.find_fold_end(0), Some(6));
    assert!(editor.can_fold_line(0));
    editor.lsp_folding_ranges = vec![(0, 0), (0, usize::MAX)];
    assert_eq!(editor.find_fold_end(0), None);
    assert!(!editor.can_fold_line(0));
}

#[test]
fn allman_headers_fold_in_braced_languages_without_lsp_ranges() {
    let source = "void example()\n{\n    if (ready)\n    {\n        work();\n    }\n}\nnext();\n";
    for extension in ["cpp", "cs", "java", "js", "rs"] {
        let mut editor = Editor::test_with_text(source);
        editor.file_path = Some(format!("example.{extension}").into());
        assert_eq!(editor.find_fold_end(0), Some(6), "{extension}");
        assert_eq!(editor.find_fold_end(2), Some(5), "{extension}");
        assert!(editor.can_fold_line(0), "{extension}");
        assert!(editor.can_fold_line(2), "{extension}");
    }
}

#[test]
fn preprocessor_fold_hints_follow_edits_undo_and_language_changes() {
    let mut editor = cpp("header\n#if FEATURE\n\n    child\n#endif\nnext\n");
    assert!(editor.can_fold_line(0));
    assert_eq!(editor.find_fold_end(0), Some(5));
    editor.cursor_line = 1;
    editor.cursor_col = 0;
    editor.insert_char('x');
    assert!(!editor.can_fold_line(0));
    editor.undo();
    assert!(editor.can_fold_line(0));
    editor.file_path = Some("example.py".into());
    assert!(!editor.can_fold_line(0), "Python comments are not C preprocessor directives");
    editor.file_path = Some("example.hpp".into());
    assert!(editor.can_fold_line(0), "C++ headers use preprocessor folding too");
}

#[test]
fn directive_boundaries_and_macro_continuations_do_not_create_nested_conditionals() {
    let source = concat!(
        "#if OUTER\n",
        "#define TEXT \\\n",
        "    #endif \\\n",
        "    value\n",
        "#ifdef_extra\n",
        "    work();\n",
        "#endif\n",
        "next();\n",
    );
    let editor = cpp(source);
    assert_eq!(editor.find_fold_end(0), Some(6));
    assert_eq!(editor.find_fold_end(1), Some(4));
    assert_eq!(editor.find_fold_end(4), None);
    assert!(!editor.can_fold_line(4));
}

#[test]
fn unbraced_if_folds_stop_at_the_enclosing_preprocessor_branch() {
    let source = "int run(int n) {\n#if ONE\n    if (n > 0)\n        --n;\n#else\n    if (n < 0)\n        ++n;\n#endif\n    return n;\n}\n";
    let editor = cpp(source);
    assert_eq!(editor.find_fold_end(0), Some(9));
    assert_eq!(editor.find_fold_end(2), Some(4));
    assert_eq!(editor.find_fold_end(5), Some(7));
    for statement in [3, 6] {
        assert_eq!(editor.find_fold_end(statement), None);
        assert!(!editor.can_fold_line(statement), "branch boundaries must not create fake fold hints");
    }
}

#[test]
fn directives_after_statements_do_not_turn_them_into_fold_headers() {
    let editor = cpp(NESTED_CPP);
    for statement in ["int total = 0;", "total += i * 2;", "return total;"] {
        let row = line(NESTED_CPP, statement);
        assert_eq!(editor.find_fold_end(row), None, "{statement}");
        assert!(!editor.can_fold_line(row), "{statement}");
    }
}
