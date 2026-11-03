use crate::editor::Editor;

const SOURCE: &str = "#if OUTER\nfirst();\n#if INNER\ninner();\n#else\nalternate();\n#endif\n#elif NEXT\nnext();\n#else\nfallback();\n#endif\ntail();";

fn cpp(source: &str) -> Editor {
    let mut editor = Editor::test_with_text(source);
    editor.file_path = Some("conditional.cpp".into());
    editor
}

#[test]
fn branch_folding_preserves_sibling_directives_and_their_bodies() {
    for source in [SOURCE.to_string(), SOURCE.replace('\n', "\r\n")] {
        let mut editor = cpp(&source);
        let scope = editor.conditional_fold(0).unwrap();
        assert_eq!(scope.branches, [0, 7, 9]);
        assert_eq!(scope.end, 11);
        assert_eq!(editor.find_fold_end(0), Some(7));
        assert_eq!(editor.find_fold_end(2), Some(4));
        editor.toggle_fold(0);
        assert_eq!(editor.visible_lines(0, 20), [0, 7, 8, 9, 10, 11, 12]);
        editor.toggle_fold(7);
        assert_eq!(editor.visible_lines(0, 20), [0, 7, 9, 10, 11, 12]);
        assert!(!editor.is_line_folded(10));
        assert!(!editor.is_line_folded(11));
        assert!(editor.collapse_conditional(8));
        assert_eq!(editor.folded_ranges.len(), 1);
        assert_eq!(editor.folded_ranges.get(&0), Some(&11));
        assert_eq!(editor.visible_lines(0, 20), [0, 11, 12]);
        assert!(editor.collapse_conditional_branch(0));
        assert_eq!(editor.folded_ranges.get(&0), Some(&7));
        assert_eq!(editor.visible_lines(0, 20), [0, 7, 8, 9, 10, 11, 12]);
    }
}

#[test]
fn provider_ranges_cannot_silently_change_branch_folding_into_whole_folding() {
    let mut editor = cpp(SOURCE);
    editor.lsp_folding_ranges = vec![(0, 11), (7, 11)];
    assert_eq!(editor.find_fold_end(0), Some(7));
    assert_eq!(editor.find_fold_end(7), Some(9));
    editor.toggle_fold(0);
    assert_eq!(editor.folded_ranges.get(&0), Some(&7));
    assert!(editor.collapse_conditional(0));
    assert_eq!(editor.folded_ranges.get(&0), Some(&11));
    assert!(!editor.is_line_folded(11));
    editor.unfold_all();
    editor.file_path = Some("conditional.py".into());
    assert!(editor.conditional_fold(0).is_none());
    assert_eq!(editor.find_fold_end(0), Some(12));
}

#[test]
fn complete_and_individual_scopes_are_available_from_every_branch_or_body() {
    let editor = cpp(SOURCE);
    for (line, expected_start, expected_branch) in [
        (0, 0, 0),
        (1, 0, 0),
        (2, 2, 2),
        (4, 2, 4),
        (6, 2, 4),
        (7, 0, 7),
        (8, 0, 7),
        (10, 0, 9),
        (11, 0, 9),
    ] {
        let scope = editor.conditional_at(line).unwrap();
        assert_eq!(scope.start, expected_start, "line={line}");
        assert_eq!(scope.branch_at(line).0, expected_branch, "line={line}");
    }
    assert!(editor.conditional_at(12).is_none());
    let mut editor = cpp(SOURCE);
    assert!(editor.collapse_conditional_branch(8));
    assert_eq!(editor.folded_ranges.get(&7), Some(&9));
    assert!(!editor.is_line_folded(0));
    assert!(!editor.is_line_folded(10));
}

#[test]
fn fold_all_remains_an_explicit_whole_tree_action() {
    let mut editor = cpp(SOURCE);
    editor.fold_all();
    assert_eq!(editor.folded_ranges.get(&0), Some(&11));
    assert_eq!(editor.folded_ranges.len(), 1);
    editor.unfold_all();
    editor.fold_at_level(1);
    assert_eq!(
        editor
            .folded_ranges
            .iter()
            .map(|(&start, &end)| (start, end))
            .collect::<Vec<_>>(),
        [(0, 7), (7, 9), (9, 11)]
    );
}

#[test]
fn conditional_metadata_is_retained_across_redraws_and_invalidated_by_edits() {
    let mut editor = cpp(SOURCE);
    let first = editor.conditional_fold(0).unwrap();
    for _ in 0..100 {
        assert!(std::sync::Arc::ptr_eq(
            &first,
            &editor.conditional_fold(0).unwrap()
        ));
    }
    assert_eq!(editor.conditional_fold_cache.borrow().builds, 1);
    editor.set_cursor(8, 0);
    editor.insert_char('\n');
    let edited = editor.conditional_fold(0).unwrap();
    assert_eq!(edited.end, 12);
    assert_eq!(edited.branches, [0, 7, 10]);
    assert!(!std::sync::Arc::ptr_eq(&first, &edited));
    editor.undo();
    assert_eq!(editor.conditional_fold(0).unwrap().end, 11);
}

#[test]
fn continuations_and_unmatched_conditionals_never_hide_unrelated_branches() {
    let source = "#if OUTER\n#define TEXT \\\n#if FAKE \\\n#else \\\n#endif\nwork();\n#elif NEXT\nnext();\n#else\nfallback();\n#endif\ntail();";
    for source in [source.to_string(), source.replace('\n', "\r\n")] {
        let mut editor = cpp(&source);
        let scope = editor.conditional_fold(0).unwrap();
        assert_eq!(scope.branches, [0, 6, 8]);
        assert_eq!(scope.end, 10);
        assert!(editor.conditional_fold(2).is_none());
        editor.toggle_fold(0);
        assert_eq!(editor.visible_lines(0, 20), [0, 6, 7, 8, 9, 10, 11]);
    }
    let mut editor = cpp("#if A\nwork();\n#else\nother();");
    assert!(editor.conditional_fold(0).is_none());
    assert!(!editor.collapse_conditional(0));
    editor.toggle_fold(0);
    assert_eq!(editor.folded_ranges.get(&0), Some(&2));
}

#[test]
fn empty_branches_offer_no_misleading_fold_arrow_even_with_provider_ranges() {
    let mut editor = cpp("#if A\n#else\nother();\n#endif\ntail();");
    editor.lsp_folding_ranges = vec![(0, 3)];
    assert!(!editor.can_fold_line(0));
    assert_eq!(editor.find_fold_end(0), None);
    assert!(editor.can_fold_line(1));
    assert!(editor.collapse_conditional(0));
    assert!(editor.is_line_folded(1));
    assert!(editor.collapse_conditional_branch(0));
    assert!(editor.folded_ranges.is_empty());
    assert!(!editor.is_line_folded(2));
}

#[test]
fn wrapped_gutter_lookup_does_not_materialize_a_long_lines_text_or_segments() {
    let mut editor = Editor::test_with_text(&"x".repeat(200_000));
    editor.word_wrap = true;
    editor.viewport_width = 80;
    editor.viewport_height = 20;
    assert!(editor.install_wrap_layout(80, vec![0, 2500]));
    editor.set_wrap_scroll_offset(100);
    assert!(editor.wrap_cache.is_empty());
    for row in 0..20 {
        assert_eq!(editor.source_line_at_screen(row), Some((0, false)));
    }
    assert!(editor.wrap_cache.is_empty());
    editor.set_wrap_scroll_offset(0);
    assert_eq!(editor.source_line_at_screen(0), Some((0, true)));
    assert_eq!(editor.source_line_at_screen(1), Some((0, false)));
    assert!(editor.wrap_cache.is_empty());
}
