use super::*;

fn editor(source: &str, language: &str, line: usize, column: usize) -> Editor {
    let mut editor = Editor::test_with_text(source);
    editor.language_override = Some(language.into());
    editor.cursor_line = line;
    editor.cursor_col = column;
    editor
}

fn index(editor: &Editor) -> StructuralScopeIndex {
    StructuralScopeRequest::from_editor(editor)
        .unwrap()
        .build(None, || false)
        .unwrap()
}

#[test]
fn indentation_membership_includes_whitespace_and_line_end_but_not_dedented_rows() {
    let source =
        "def run():\n    if ready:\n        work()\n        finish()\n    after()\noutside()\n";
    let mut editor = editor(source, "Python", 3, 0);
    let index = index(&editor);
    for column in [0, 7, 8, 15, 16] {
        editor.cursor_col = column;
        assert_eq!(index.enclosing_scope(&editor), Some(((1, 4), (3, 15))));
        assert_eq!(enclosing_scope(&editor), index.enclosing_scope(&editor));
    }
    editor.cursor_line = 4;
    editor.cursor_col = 0;
    assert_eq!(index.enclosing_scope(&editor), Some(((0, 0), (4, 10))));
    editor.cursor_line = 5;
    assert_eq!(index.enclosing_scope(&editor), None);
}

#[test]
fn cached_language_scopes_handle_deep_files_and_reuse_unchanged_source() {
    for (language, source, line) in [
        (
            "Python",
            "def run():\n    if ready:\n        work()\n".repeat(2_000),
            5_999,
        ),
        (
            "C++",
            "void run() {\n    if (ready)\n        work();\n}\n".repeat(2_000),
            7_998,
        ),
    ] {
        let mut editor = editor(&source, language, line, 9);
        assert!(ImmediateScopeIndex::build(&editor).is_none());
        let old = index(&editor);
        let pair = old.enclosing_scope(&editor).expect(language);
        assert_eq!(pair.0.0, line - 1);
        editor.file_path = Some("renamed".into());
        let reused = StructuralScopeRequest::from_editor(&editor)
            .unwrap()
            .build(Some(&old), || false)
            .unwrap();
        assert!(std::sync::Arc::ptr_eq(
            old.language_scopes.as_ref().unwrap(),
            reused.language_scopes.as_ref().unwrap()
        ));
        assert_eq!(reused.enclosing_scope(&editor), Some(pair));
        for col in 8..14 {
            editor.cursor_col = col;
            assert_eq!(reused.enclosing_scope(&editor), Some(pair));
        }
    }
}

#[test]
fn indentation_and_terminator_edits_reject_stale_membership() {
    let mut python = editor(
        "def run():\n    if ready:\n        work()\n    after()\n",
        "Python",
        2,
        8,
    );
    let old = index(&python);
    assert_eq!(old.enclosing_scope(&python).unwrap().0, (1, 4));
    let start = python.rope.line_to_char(2);
    python.rope.remove(start..start + 4);
    python.content_version += 1;
    assert!(old.enclosing_scope(&python).is_none());
    let fresh = StructuralScopeRequest::from_editor(&python)
        .unwrap()
        .build(Some(&old), || false)
        .unwrap();
    assert_eq!(fresh.enclosing_scope(&python).unwrap().0, (0, 0));

    let mut cpp = editor("if (ready)\n    work();", "C++", 1, 5);
    let old = index(&cpp);
    assert!(old.enclosing_scope(&cpp).is_some());
    cpp.rope.remove(cpp.rope.len_chars() - 1..);
    cpp.content_version += 1;
    let fresh = StructuralScopeRequest::from_editor(&cpp)
        .unwrap()
        .build(Some(&old), || false)
        .unwrap();
    assert!(fresh.enclosing_scope(&cpp).is_none());
}

#[test]
fn python_paths_associations_overrides_and_shebangs_select_the_scanner() {
    for name in ["sample.py", "sample.pyi", "sample.pyw"] {
        let mut editor = editor("def run():\n    work()\n", "Python", 1, 5);
        editor.language_override = None;
        editor.file_path = Some(name.into());
        assert!(index(&editor).enclosing_scope(&editor).is_some());
        editor.language_override = Some("Plain Text".into());
        assert!(StructuralScopeRequest::from_editor(&editor).is_none());
    }
    let mut editor = editor(
        "#!/usr/bin/env python3\ndef run():\n    work()\n",
        "Python",
        2,
        5,
    );
    editor.language_override = None;
    editor.file_path = Some("script".into());
    assert_eq!(
        index(&editor).enclosing_scope(&editor),
        Some(((1, 0), (2, 9)))
    );
    editor
        .language_associations
        .insert("script".into(), "Plain Text".into());
    assert!(StructuralScopeRequest::from_editor(&editor).is_none());
}

#[test]
fn worker_and_immediate_language_indexes_agree_on_all_cursor_positions() {
    for (language, source) in [
        (
            "Python",
            "def run():\n    if ready:\n        work()\n    else:\n        finish()\noutside()\n",
        ),
        (
            "C++",
            "void run() {\n    if (ready)\n        for (int i = 0; i < 5; ++i)\n            work(i);\n    else\n        finish();\n}\n",
        ),
        (
            "Java",
            "class Run {\n void run() {\n  while (ready)\n   work();\n }\n}\n",
        ),
        (
            "C#",
            "class Run {\n void Run() {\n  foreach (var item in items)\n   Work(item);\n }\n}\n",
        ),
    ] {
        let mut editor = editor(source, language, 0, 0);
        let index = index(&editor);
        for line in 0..editor.line_count() {
            editor.cursor_line = line;
            for col in 0..=editor.line_len(line) {
                editor.cursor_col = col;
                assert_eq!(
                    index.enclosing_scope(&editor),
                    enclosing_scope(&editor),
                    "{language} {line}:{col}"
                );
            }
        }
    }
}

#[test]
#[ignore = "diagnostic timing for large-file scope indexing and cached cursor movement"]
fn benchmark_language_scope_navigation() {
    use std::{hint::black_box, time::Instant};
    for (language, unit, stride) in [
        ("Python", "def run():\n    if ready:\n        work()\n", 3),
        (
            "C++",
            "void run() {\n    if (ready)\n        work();\n}\n",
            4,
        ),
    ] {
        let source = unit.repeat(10_000);
        let mut editor = editor(&source, language, 2, 9);
        let started = Instant::now();
        let index = index(&editor);
        let indexing = started.elapsed();
        let mut animations = crate::animations::AnimationState::default();
        let started = Instant::now();
        for query in 0..20_000 {
            editor.cursor_line = 2 + (query % 10_000) * stride;
            editor.cursor_col = 8 + query % 5;
            black_box(animations.scope(&editor, Some(&index)));
        }
        let navigation = started.elapsed();
        eprintln!(
            "{language}: {} lines, worker={indexing:?}, 20000 cached queries={navigation:?}, mean={:?}",
            editor.line_count(),
            navigation / 20_000
        );
    }
}
