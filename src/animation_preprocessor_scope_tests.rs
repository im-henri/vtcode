use super::*;
use crate::{animation_scope::keyword_scope, animations::AnimationState};

fn editor(text: &str, language: &str) -> Editor {
    let mut editor = Editor::test_with_text(text);
    editor.language_override = Some(language.into());
    editor
}

fn index(editor: &Editor) -> StructuralScopeIndex {
    StructuralScopeRequest::from_editor(editor)
        .unwrap()
        .build(None, || false)
        .unwrap()
}

fn assert_scope(editor: &mut Editor, line: usize, col: usize, expected: Option<Pair>) {
    editor.cursor_line = line;
    editor.cursor_col = col;
    let indexed = index(editor);
    let mut state = AnimationState::default();
    assert_eq!(
        state.scope(editor, None),
        expected,
        "immediate {line}:{col}"
    );
    assert_eq!(
        state.scope(editor, Some(&indexed)),
        expected,
        "worker {line}:{col}"
    );
}

#[test]
fn every_conditional_branch_has_its_own_region_including_its_directive() {
    for language in [
        "C",
        "C++",
        "C#",
        "Objective-C",
        "Objective-C++",
        "GLSL",
        "HLSL",
    ] {
        for opener in ["#if FIRST", "#ifdef FIRST", "#ifndef FIRST"] {
            let source = format!(
                "{opener}\n  one();\n#elif SECOND\n  two();\n#elifdef THIRD\n  three();\n#elifndef FOURTH\n  four();\n#else\n  other();\n#endif\noutside();\n"
            );
            let mut editor = editor(&source, language);
            let cached = index(&editor);
            let mut state = AnimationState::default();
            for (line, expected) in [
                (0, Some(((0, 0), (1, 7)))),
                (1, Some(((0, 0), (1, 7)))),
                (2, Some(((2, 0), (3, 7)))),
                (3, Some(((2, 0), (3, 7)))),
                (4, Some(((4, 0), (5, 9)))),
                (5, Some(((4, 0), (5, 9)))),
                (6, Some(((6, 0), (7, 8)))),
                (7, Some(((6, 0), (7, 8)))),
                (8, Some(((8, 0), (9, 9)))),
                (9, Some(((8, 0), (9, 9)))),
                (10, Some(((0, 0), (10, 0)))),
                (11, None),
                (2, Some(((2, 0), (3, 7)))),
                (0, Some(((0, 0), (1, 7)))),
            ] {
                editor.cursor_line = line;
                assert_eq!(
                    keyword_scope(&editor),
                    expected,
                    "{language} {opener} row {line}"
                );
                assert_eq!(
                    state.scope(&editor, Some(&cached)),
                    expected,
                    "cached row {line}"
                );
            }
        }
    }
}

#[test]
fn nested_conditionals_preserve_smaller_brace_and_unbraced_statement_scopes() {
    let source = "void run() {\n#if OUTER\n  if (ready) {\n    work();\n  }\n#elif SECOND\n#ifndef INNER\n  if (ok)\n    use();\n#else\n  fallback();\n#endif\n#else\n  other();\n#endif\n  after();\n}\n";
    let mut editor = editor(source, "C++");
    for (line, col, pair) in [
        (1, 0, ((1, 0), (4, 2))),
        (3, 5, ((2, 13), (4, 2))),
        (5, 0, ((5, 0), (11, 5))),
        (6, 0, ((6, 0), (8, 9))),
        (8, 5, ((7, 2), (8, 9))),
        (9, 0, ((9, 0), (10, 12))),
        (10, 2, ((9, 0), (10, 12))),
        (11, 0, ((6, 0), (11, 0))),
        (12, 0, ((12, 0), (13, 9))),
        (14, 0, ((1, 0), (14, 0))),
        (15, 3, ((0, 11), (16, 0))),
    ] {
        assert_scope(&mut editor, line, col, Some(pair));
    }
}

#[test]
fn macro_replacements_and_lexical_lookalikes_cannot_supply_braces_or_branches() {
    for ending in ["\n", "\r\n"] {
        let source = [
            "#define OPEN {",
            "#define CLOSE }",
            "#define MANY(x) \\",
            "  } { { if (x) \\",
            "  #if FAKE",
            "#define RAW R\"tag(",
            "#else",
            "} {",
            ")tag\"",
            "void run() {",
            "#if REAL",
            "// #elif NO \\",
            "#else",
            "/* #else",
            "#endif */",
            "const char *s = R\"tag(",
            "#elif NO",
            ")tag\";",
            "#define COMMENT /*",
            "*/ } { {",
            "#define CONTINUED \\",
            "#endif",
            "work();",
            "#else",
            "other();",
            "#endif",
            "after();",
            "}",
            "",
        ]
        .join(ending);
        let mut editor = editor(&source, "C++");
        assert_scope(&mut editor, 22, 0, Some(((10, 0), (22, 6))));
        assert_scope(&mut editor, 24, 0, Some(((23, 0), (24, 7))));
        assert_scope(&mut editor, 26, 0, Some(((9, 11), (27, 0))));
    }
}

#[test]
fn only_logical_line_starts_recognize_directives_and_split_keywords_are_supported() {
    let source = "# /* gap */ if REAL\nwork(); #else\nwork(); \\\n#else\n#el\\\nif NEXT\nother();\n#else\nlast();\n#endif\n";
    let mut editor = editor(source, "C++");
    for (line, expected) in [
        (1, ((0, 0), (3, 4))),
        (3, ((0, 0), (3, 4))),
        (4, ((4, 0), (6, 7))),
        (5, ((4, 0), (6, 7))),
        (7, ((7, 0), (8, 6))),
        (9, ((0, 0), (9, 0))),
    ] {
        editor.cursor_line = line;
        assert_eq!(keyword_scope(&editor), Some(expected));
    }
}

#[test]
fn alternative_opening_braces_join_at_shared_closers_without_swallowing_outer_scope() {
    let source = "void run() {\n#if FIRST\n  if (a) {\n#elif SECOND\n  if (b) {\n#else\n  if (c) {\n#endif\n    shared();\n  }\n  after();\n}\n";
    let mut editor = editor(source, "C++");
    for (line, pair) in [
        (2, ((1, 0), (2, 9))),
        (4, ((3, 0), (4, 9))),
        (6, ((5, 0), (6, 9))),
        (8, ((1, 0), (9, 2))),
        (10, ((0, 11), (11, 0))),
    ] {
        assert_scope(&mut editor, line, 3, Some(pair));
    }

    let source = "void run() {\n#if A\n#if B\n {\n#else\n {\n#endif\n#else\n {\n#endif\n  shared();\n }\n after();\n}\n";
    let mut editor = self::editor(source, "C++");
    assert_scope(&mut editor, 10, 2, Some(((1, 0), (11, 1))));
    assert_scope(&mut editor, 12, 2, Some(((0, 11), (13, 0))));
}

#[test]
fn alternative_closing_braces_join_at_endif_and_later_functions_remain_independent() {
    let source = "void run() {\n  before();\n#if A\n  first();\n}\n#elif B\n  second();\n}\n#else\n  third();\n}\n#endif\nvoid next() {\n  next();\n}\n";
    let mut editor = editor(source, "C++");
    assert_scope(&mut editor, 1, 2, Some(((0, 11), (11, 0))));
    assert_scope(&mut editor, 3, 2, Some(((2, 0), (4, 0))));
    assert_scope(&mut editor, 6, 2, Some(((5, 0), (7, 0))));
    assert_scope(&mut editor, 9, 2, Some(((8, 0), (10, 0))));
    assert_scope(&mut editor, 13, 2, Some(((12, 12), (14, 0))));
}

#[test]
fn branches_that_close_one_block_and_open_another_merge_both_sides() {
    let source = "void run() {\n {\n  before();\n#if A\n }\n {\n#else\n }\n {\n#endif\n  shared();\n }\n after();\n}\n";
    let mut editor = editor(source, "C++");
    assert_scope(&mut editor, 2, 2, Some(((1, 1), (9, 0))));
    assert_scope(&mut editor, 10, 2, Some(((3, 0), (11, 1))));
    assert_scope(&mut editor, 12, 2, Some(((0, 11), (13, 0))));
}

#[test]
fn incompatible_or_optional_braces_never_connect_alternative_branches() {
    for conditional in [
        "#if A\n {\n#else\n }\n#endif\n",
        "#if A\n {\n#endif\n",
        "#if A\n {\n#else\n {\n {\n#endif\n",
        "#if A\n }\n#else\n untouched();\n#endif\n",
    ] {
        let source = format!(
            "void run() {{\n{conditional} {{\n local();\n }}\n}}\nvoid next() {{\n next();\n}}\n"
        );
        let mut editor = editor(&source, "C++");
        let local = conditional.lines().count() + 2;
        assert_scope(
            &mut editor,
            local,
            2,
            Some(((local - 1, 1), (local + 1, 1))),
        );
        let next = local + 4;
        assert_scope(&mut editor, next, 2, Some(((next - 1, 12), (next + 1, 0))));
        editor.cursor_line = 0;
        editor.cursor_col = 12;
        assert_eq!(
            index(&editor).enclosing_scope(&editor),
            None,
            "{conditional}"
        );
    }
}

#[test]
fn incomplete_and_malformed_conditionals_do_not_guess_whole_regions() {
    for source in [
        "#if A\nfirst();\n#else\nother();\n",
        "#if A\nfirst();\n#else\nother();\n#else\nlast();\n#endif\n",
        "#if A\nfirst();\n#else\nother();\n#elif B\nlast();\n#endif\n",
        "#else\nfirst();\n#endif\n",
    ] {
        let mut editor = editor(source, "C++");
        for row in 0..editor.line_count() {
            editor.cursor_line = row;
            assert_eq!(keyword_scope(&editor), None, "row {row}: {source}");
        }
    }
    let mut editor = editor("void run() {\n#if A\n}\n", "C++");
    assert_scope(&mut editor, 0, 12, None);
}

#[test]
fn empty_branches_and_blank_body_rows_still_select_the_target_directive() {
    let source = "#if A\n#elif B\n\n#else\n#endif\n";
    let mut editor = editor(source, "C");
    for (line, pair) in [
        (0, ((0, 0), (0, 4))),
        (1, ((1, 0), (2, 0))),
        (2, ((1, 0), (2, 0))),
        (3, ((3, 0), (3, 4))),
        (4, ((0, 0), (4, 0))),
    ] {
        editor.cursor_line = line;
        assert_eq!(keyword_scope(&editor), Some(pair));
    }
}

#[test]
fn edits_inside_and_after_conditional_branches_match_fresh_worker_indexes() {
    let source = format!(
        "{}void run() {{\n#if A\n {{\n{}#else\n {{\n#endif\n{} }}\n}}\n",
        "void unit() {\n work();\n}\n".repeat(500),
        " branch();\n".repeat(600),
        " shared();\n".repeat(700)
    );
    for marker in ["branch();", "shared();"] {
        let mut editor = editor(&source, "C++");
        let old = index(&editor);
        let offset = editor.rope.byte_to_char(source.rfind(marker).unwrap());
        editor.rope.insert(offset, "{\n nested();\n }\n ");
        editor.content_version += 1;
        let request = StructuralScopeRequest::from_editor(&editor).unwrap();
        let reused = request.build(Some(&old), || false).unwrap();
        let fresh = request.build(None, || false).unwrap();
        let nodes = |index: &StructuralScopeIndex| {
            index
                .nodes
                .iter()
                .map(|n| {
                    (
                        n.start,
                        n.end,
                        n.parent,
                        n.depth,
                        n.anchor,
                        n.uncertain_from,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(nodes(&reused), nodes(&fresh), "edit {marker}");
        assert_eq!(reused.coverage, fresh.coverage);
        assert!(reused.restart > 0);
        let conditional = source.find("#if A").unwrap();
        if marker == "branch();" {
            assert!(reused.restart <= conditional);
        } else {
            assert!(reused.restart > conditional);
        }
    }
}

#[test]
fn cached_navigation_and_directive_edits_refresh_the_selected_branch() {
    let mut editor = editor("#if A\n first();\n#else\n other();\n#endif\n", "C++");
    let mut state = AnimationState::default();
    let old = index(&editor);
    editor.cursor_line = 3;
    assert_eq!(state.scope(&editor, Some(&old)), Some(((2, 0), (3, 8))));
    let at = editor.rope.line_to_char(2);
    editor.rope.insert(at, "#elif B\n middle();\n");
    editor.content_version += 1;
    assert_eq!(state.scope(&editor, Some(&old)), Some(((2, 0), (3, 9))));
    let fresh = index(&editor);
    assert_eq!(state.scope(&editor, Some(&fresh)), Some(((2, 0), (3, 9))));
    editor.cursor_line = 5;
    assert_eq!(state.scope(&editor, Some(&fresh)), Some(((4, 0), (5, 8))));
}

#[test]
fn correcting_an_incompatible_branch_restores_ancestors_from_before_the_checkpoint() {
    let source = format!(
        "void run() {{\n{}#if A\n {{\n#else\n unchanged();\n#endif\n shared();\n }}\n}}\n",
        "    work();\n".repeat(800)
    );
    let mut editor = editor(&source, "C++");
    let old = index(&editor);
    let start = editor
        .rope
        .byte_to_char(source.find(" unchanged();").unwrap());
    editor.rope.remove(start..start + " unchanged();".len());
    editor.rope.insert(start, " {");
    editor.content_version += 1;
    let request = StructuralScopeRequest::from_editor(&editor).unwrap();
    let reused = request.build(Some(&old), || false).unwrap();
    let fresh = request.build(None, || false).unwrap();
    assert!(reused.restart > 0);
    for line in [0, 700, 802, 804, 806, 807] {
        editor.cursor_line = line;
        editor.cursor_col = 12.min(editor.line_len(line));
        assert_eq!(
            reused.enclosing_scope(&editor),
            fresh.enclosing_scope(&editor),
            "row {line}"
        );
    }
    editor.cursor_line = 700;
    assert_eq!(reused.enclosing_scope(&editor), Some(((0, 11), (808, 0))));
}

#[test]
fn preprocessing_respects_cancellation_and_the_keyword_window_budget() {
    let source = ropey::Rope::from_str(&format!(
        "#{}if READY\nwork();\n#endif\n",
        "\\\n".repeat(20_000)
    ));
    let polls = Cell::new(0);
    let cancelled = || {
        polls.set(polls.get() + 1);
        polls.get() >= 2
    };
    let control = ScanControl::new(&cancelled);
    let input = CharInput::rope(&source);
    let mut lexer = Lexer::controlled(&input, Dialect::Cpp, 0, Some(&control));
    assert!(lexer.next_index_event().is_err());
    assert!(control.was_cancelled.get());
    assert!(control.work.get() < source.len_chars() / 2);

    let mut editor = editor(
        &format!(
            "#if A\n{}#else\nother();\n#endif\n",
            "work();\n".repeat(MAX_LINES)
        ),
        "C++",
    );
    editor.cursor_line = MAX_LINES / 2;
    assert_eq!(keyword_scope(&editor), None);
    editor.rope = ropey::Rope::from_str(&format!(
        "#if A\n{}\n#else\nother();\n#endif\n",
        "x".repeat(MAX_CHARS + 1)
    ));
    editor.cursor_line = 1;
    assert_eq!(keyword_scope(&editor), None);
}
