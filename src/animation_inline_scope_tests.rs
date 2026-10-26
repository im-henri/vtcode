use super::*;
use crate::animations::AnimationState;

fn editor(source: &str, language: &str) -> Editor {
    let mut editor = Editor::test_with_text(source);
    editor.language_override = Some(language.into());
    editor
}

fn index(editor: &Editor, previous: Option<&StructuralScopeIndex>) -> StructuralScopeIndex {
    StructuralScopeRequest::from_editor(editor)
        .unwrap()
        .build(previous, || false)
        .unwrap()
}

#[test]
fn inline_scopes_follow_horizontal_navigation_through_nested_sibling_and_empty_blocks() {
    let source = " { α(); { β(); } γ(); } { δ(); } {} ";
    let positions = |brace| {
        source
            .chars()
            .enumerate()
            .filter_map(|(col, ch)| (ch == brace).then_some(col))
            .collect::<Vec<_>>()
    };
    let opens = positions('{');
    let closes = positions('}');
    let pairs = [
        ((0, opens[1]), (0, closes[0])),
        ((0, opens[0]), (0, closes[1])),
        ((0, opens[2]), (0, closes[2])),
        ((0, opens[3]), (0, closes[3])),
    ];
    for language in [
        "C",
        "C++",
        "C#",
        "Rust",
        "Java",
        "JavaScript",
        "Go",
        "JSON",
        "CSS",
    ] {
        let mut editor = editor(source, language);
        let indexed = index(&editor, None);
        assert!(indexed.has_scopes(), "{language}");
        let mut immediate_state = AnimationState::default();
        let mut indexed_state = AnimationState::default();
        let columns = 0..=source.chars().count();
        for col in columns.clone().chain(columns.rev()) {
            editor.cursor_col = col;
            let expected = pairs
                .iter()
                .copied()
                .find(|pair| pair.0.1 <= col && col <= pair.1.1);
            assert_eq!(
                immediate_state.scope(&editor, None),
                expected,
                "{language} immediate col {col}"
            );
            assert_eq!(
                indexed_state.scope(&editor, Some(&indexed)),
                expected,
                "{language} indexed col {col}"
            );
        }
    }
}

#[test]
fn inline_braces_in_comments_and_strings_do_not_hide_the_enclosing_inline_scope() {
    let source =
        "void run() {\n  if (ok) { puts(\"} { }\"); /* } { */ use(); } else { next(); }\n}\n";
    let line = source.lines().nth(1).unwrap();
    let left = (
        (1, line.find('{').unwrap()),
        (1, line.find("} else").unwrap()),
    );
    let right = (
        (1, line.find("{ next").unwrap()),
        (1, line.rfind('}').unwrap()),
    );
    for language in ["C", "C++", "C#", "JavaScript", "Rust"] {
        let mut editor = editor(source, language);
        let indexed = index(&editor, None);
        editor.cursor_line = 1;
        for (col, expected) in [
            (line.find("{ }").unwrap(), left),
            (line.find("/*").unwrap() + 3, left),
            (line.find("use()").unwrap(), left),
            (line.find("next()").unwrap(), right),
        ] {
            editor.cursor_col = col;
            assert_eq!(
                indexed.enclosing_scope(&editor),
                Some(expected),
                "{language} col {col}"
            );
            assert_eq!(
                enclosing_scope(&editor),
                Some(expected),
                "{language} fallback col {col}"
            );
        }
    }
}

#[test]
fn inline_children_win_inside_each_preprocessor_branch() {
    let source = "void run() {\n#if FIRST\n  if (ok) { one(); }\n#elif SECOND\n  if (ok) { two(); }\n#else\n  if (ok) { other(); }\n#endif\n}\n";
    let mut editor = editor(source, "C++");
    let indexed = index(&editor, None);
    for line in [2, 4, 6] {
        let text = editor.rope.line(line).to_string();
        let pair = (
            (line, text.find('{').unwrap()),
            (line, text.find('}').unwrap()),
        );
        let mut state = AnimationState::default();
        editor.cursor_line = line;
        editor.cursor_col = pair.0.1 + 2;
        assert_eq!(state.scope(&editor, None), Some(pair));
        assert_eq!(state.scope(&editor, Some(&indexed)), Some(pair));
    }
}

#[test]
fn incremental_inline_scopes_survive_newline_and_delimiter_edits_after_a_checkpoint() {
    let source = format!(
        "fn run() {{ let value = {{ {}value(); }}; {{ next(); }} }}\n",
        " ".repeat(16_000)
    );
    let mut editor = editor(&source, "Rust");
    let mut previous = index(&editor, None);
    for insertion in ["\n", "{ nested(); }", "}", "{", "\n"] {
        let at = editor.rope.len_chars() - 6;
        editor.rope.insert(at, insertion);
        editor.content_version += 1;
        let reused = index(&editor, Some(&previous));
        let fresh = index(&editor, None);
        assert!(
            reused.restart > 8_000,
            "reuse checkpoints within a single-line block"
        );
        for offset in editor.rope.len_chars() - 70..editor.rope.len_chars() {
            editor.cursor_line = editor.rope.char_to_line(offset);
            editor.cursor_col = offset - editor.rope.line_to_char(editor.cursor_line);
            assert_eq!(
                reused.enclosing_scope(&editor),
                fresh.enclosing_scope(&editor),
                "after {insertion:?} at {offset}"
            );
        }
        previous = reused;
    }
}
