use super::*;
use crate::{
    animations::AnimationState,
    scope_bubble_config::{self as config, Config, Configs, Syntax},
};
use std::sync::Arc;

fn profile(syntax: Syntax, pairs: &[Delimiter], blocks: bool) -> Config {
    Config {
        syntax,
        delimiters: Some(pairs.to_vec()),
        language_blocks: blocks,
    }
}
fn editor(text: &str, language: &str, rule: Config) -> Editor {
    let mut editor = Editor::test_with_text(text);
    editor.language_override = Some(language.into());
    editor.scope_bubble_rules = Rules::compile(&[(language.into(), rule)].into());
    editor
}
fn build(editor: &Editor) -> StructuralScopeIndex {
    StructuralScopeRequest::from_editor(editor)
        .unwrap()
        .build(None, || false)
        .unwrap()
}
fn at(editor: &mut Editor, marker: &str) {
    let text = editor.rope.to_string();
    let offset = text[..text.find(marker).unwrap()].chars().count();
    editor.cursor_line = editor.rope.char_to_line(offset);
    editor.cursor_col = offset - editor.rope.line_to_char(editor.cursor_line);
}
fn selected(editor: &Editor, pair: Option<Pair>) -> Option<String> {
    pair.map(|(a, b)| {
        editor
            .rope
            .slice((editor.rope.line_to_char(a.0) + a.1)..=(editor.rope.line_to_char(b.0) + b.1))
            .to_string()
    })
}

#[test]
fn scope_profiles_custom_c_like_language_uses_strings_and_comments_not_highlighting() {
    let mut editor = editor(
        "task {\n // } ) ]\n value = \"} ) ]\";\n /* { */\n work();\n}\n",
        "My DSL",
        profile(Syntax::Cpp, &[Delimiter::Braces], false),
    );
    at(&mut editor, "work");
    let index = build(&editor);
    assert_eq!(index.enclosing_scope(&editor), Some(((0, 5), (5, 0))));
    assert_eq!(enclosing_scope(&editor), index.enclosing_scope(&editor));
    assert_eq!(editor.syntax_language_token(), Some("My DSL"));
    editor.language_override = None;
    editor.file_path = Some("source.custom".into());
    editor
        .language_associations
        .insert("*.custom".into(), "My DSL".into());
    assert_eq!(
        build(&editor).enclosing_scope(&editor),
        Some(((0, 5), (5, 0)))
    );
}

#[test]
fn scope_profiles_builtin_cpp_selects_innermost_parentheses_and_brackets() {
    let mut editor = editor(
        "void f() {\n call(\n   values[\n     index\n   ]\n );\n}\n",
        "C++",
        profile(Syntax::Automatic, &Delimiter::ALL, false),
    );
    let index = build(&editor);
    at(&mut editor, "index");
    assert_eq!(
        selected(&editor, index.enclosing_scope(&editor)).as_deref(),
        Some("[\n     index\n   ]")
    );
    at(&mut editor, "values");
    assert_eq!(
        selected(&editor, index.enclosing_scope(&editor)).as_deref(),
        Some("(\n   values[\n     index\n   ]\n )")
    );
    for line in 0..editor.line_count() {
        for col in 0..editor.rope.line(line).len_chars() {
            editor.cursor_line = line;
            editor.cursor_col = col;
            assert_eq!(
                index.enclosing_scope(&editor),
                enclosing_scope(&editor),
                "{line}:{col}"
            );
        }
    }
}

#[test]
fn scope_profiles_selected_pairs_keep_literal_dialects_and_regex_state() {
    for (syntax, source) in [
        (
            Syntax::Cpp,
            "(\n R\"raw()]}\"{)raw\", // ) ] }\n value\n)\n",
        ),
        (
            Syntax::Rust,
            "[\n r#\" } ) ] \"#, /* ] /* ) */ ] */\n value\n]\n",
        ),
        (
            Syntax::Javascript,
            "call(\n /[)\\]}]/, ` } ${ {nested: `inner ${1}`} } )`,\n value\n)\n",
        ),
        (
            Syntax::Csharp,
            "call(\n @\" ) \"\" ]\", $\"{new { X = \"}\" }} )\",\n value\n)\n",
        ),
        (
            Syntax::Python,
            "[\n ''' ) ] }\n [ { ''', # ) ] }\n f\"{ {'key': ') ] }'}['key'] }\",\n 9 // 2,\n value\n]\n",
        ),
    ] {
        let mut editor = editor(source, "Custom", profile(syntax, &Delimiter::ALL, false));
        at(&mut editor, "value");
        let index = build(&editor);
        let pair = index.enclosing_scope(&editor).expect(syntax.label());
        assert_eq!(pair.0.0, 0, "{}", syntax.label());
        assert_eq!(pair.1.0, editor.line_count() - 2, "{}", syntax.label());
        for line in 0..editor.line_count() {
            editor.cursor_line = line;
            editor.cursor_col = 0;
            assert_eq!(
                index.enclosing_scope(&editor),
                enclosing_scope(&editor),
                "{} {line}",
                syntax.label()
            );
        }
    }
}

#[test]
fn scope_profiles_rule_changes_invalidate_workers_and_stationary_cursor_cache() {
    let mut editor = editor(
        "{\n call(\n value\n );\n}\n",
        "Custom",
        profile(Syntax::C, &[Delimiter::Braces], false),
    );
    at(&mut editor, "value");
    let old = build(&editor);
    let mut state = AnimationState::default();
    assert_eq!(state.scope(&editor, None), Some(((0, 0), (4, 0))));
    editor.scope_bubble_rules =
        Rules::compile(&[("Custom".into(), profile(Syntax::C, &Delimiter::ALL, false))].into());
    assert!(!old.matches(&editor));
    assert_eq!(old.enclosing_scope(&editor), None);
    let expected = Some(((1, 5), (3, 1)));
    assert_eq!(state.scope(&editor, None), expected);
    assert_eq!(build(&editor).enclosing_scope(&editor), expected);
    editor.scope_bubble_rules =
        Rules::compile(&[("Custom".into(), profile(Syntax::C, &[], false))].into());
    assert_eq!(state.scope(&editor, None), None);
    assert!(!build(&editor).has_scopes());
    editor.scope_bubble_rules = Rules::compile(
        &[(
            "Custom".into(),
            profile(Syntax::Disabled, &Delimiter::ALL, true),
        )]
        .into(),
    );
    assert!(StructuralScopeRequest::from_editor(&editor).is_none());
    assert_eq!(state.scope(&editor, None), None);
}

#[test]
fn scope_profiles_typing_selected_delimiters_does_not_reuse_outer_brace() {
    let mut editor = editor(
        "{\n value\n}\n",
        "Custom",
        profile(Syntax::C, &Delimiter::ALL, false),
    );
    at(&mut editor, "value");
    let mut state = AnimationState::default();
    assert_eq!(state.scope(&editor, None), Some(((0, 0), (2, 0))));
    let offset = editor.rope.line_to_char(1) + 1;
    editor.rope.insert(offset + 5, ")");
    editor.rope.insert(offset, "(");
    editor.content_version += 1;
    editor.wrap_revision += 1;
    editor.cursor_col += 1;
    assert_eq!(state.scope(&editor, None), Some(((1, 1), (1, 7))));
    assert_eq!(
        state.scope(&editor, None),
        build(&editor).enclosing_scope(&editor)
    );
}

#[test]
fn scope_profiles_mismatched_pairs_and_incomplete_strings_never_guess() {
    for source in [
        "(\n [\n value\n )\n ]",
        "(\n \"unterminated\n )",
        "(\n value\n",
    ] {
        let mut editor = editor(source, "Custom", profile(Syntax::C, &Delimiter::ALL, false));
        editor.cursor_line = 1;
        assert_eq!(build(&editor).enclosing_scope(&editor), None, "{source}");
        assert_eq!(enclosing_scope(&editor), None, "{source}");
    }
}

#[test]
fn scope_profiles_preprocessor_alternatives_cannot_merge_different_pair_kinds() {
    for (source, expected) in [
        ("#if A\n(\n#else\n[\n#endif\n value\n)\n", None),
        (
            "#if A\n(\n#else\n(\n#endif\n value\n)\n",
            Some(((0, 0), (6, 0))),
        ),
    ] {
        let mut editor = editor(source, "Custom", profile(Syntax::C, &Delimiter::ALL, false));
        at(&mut editor, "value");
        assert_eq!(build(&editor).enclosing_scope(&editor), expected);
        assert_eq!(enclosing_scope(&editor), expected);
    }
}

#[test]
fn scope_profiles_language_blocks_can_be_enabled_without_delimiters() {
    let mut editor = editor(
        "if ready:\n    run()\nnext()\n",
        "Custom",
        profile(Syntax::Python, &[], true),
    );
    at(&mut editor, "run");
    assert_eq!(
        build(&editor).enclosing_scope(&editor),
        Some(((0, 0), (1, 8)))
    );
    editor.scope_bubble_rules =
        Rules::compile(&[("Custom".into(), profile(Syntax::Python, &[], false))].into());
    assert_eq!(build(&editor).enclosing_scope(&editor), None);
}

#[test]
fn scope_profiles_keyword_styles_reuse_existing_parsers_and_disable_without_edits() {
    let mut editor = editor(
        "if ready then\n  print('end')\nend\n",
        "My Language",
        profile(Syntax::Lua, &[], true),
    );
    at(&mut editor, "print");
    let mut state = AnimationState::default();
    assert_eq!(state.scope(&editor, None), Some(((0, 0), (2, 0))));
    editor.scope_bubble_rules =
        Rules::compile(&[("My Language".into(), profile(Syntax::Lua, &[], false))].into());
    assert_eq!(state.scope(&editor, None), None);
}

#[test]
fn scope_profiles_work_is_linear_prefix_edits_reuse_checkpoints_and_fallback_is_bounded() {
    let mut editor = editor(
        &"{\n call(\n values[index]);\n}\n".repeat(5_000),
        "Custom",
        profile(Syntax::C, &Delimiter::ALL, false),
    );
    let first = build(&editor);
    assert!(first.work < editor.rope.len_chars() * 4, "{}", first.work);
    assert!(first.nodes.len() <= editor.rope.len_chars());
    let end = editor.rope.len_chars() - 2;
    editor.rope.insert(end, " ");
    editor.content_version += 1;
    editor.wrap_revision += 1;
    let request = StructuralScopeRequest::from_editor(&editor).unwrap();
    let incremental = request.build(Some(&first), || false).unwrap();
    let fresh = build(&editor);
    assert!(
        incremental.restart > editor.rope.len_chars() - 10_000,
        "restart={} length={} work={}",
        incremental.restart,
        editor.rope.len_chars(),
        incremental.work
    );
    assert!(incremental.work < 20_000);
    assert_eq!(incremental.nodes.len(), fresh.nodes.len());
    for (a, b) in incremental.nodes.iter().zip(&fresh.nodes) {
        assert_eq!((a.start, a.end, a.delimiter), (b.start, b.end, b.delimiter));
    }
    editor.cursor_line = 10_000;
    assert!(ImmediateScopeIndex::build(&editor).is_none());
    assert!(request.build(Some(&first), || true).is_none());
    // Changing the selected delimiters must not reuse checkpoints from the old mask.
    editor.scope_bubble_rules = Rules::compile(
        &[(
            "Custom".into(),
            profile(Syntax::C, &[Delimiter::Braces], false),
        )]
        .into(),
    );
    let changed = StructuralScopeRequest::from_editor(&editor)
        .unwrap()
        .build(Some(&first), || false)
        .unwrap();
    assert_eq!(changed.restart, 0);
    assert!(
        changed
            .nodes
            .iter()
            .all(|node| node.delimiter == Delimiter::Braces)
    );
}

#[test]
fn scope_profiles_settings_merge_per_language_and_aliases_compile_once() {
    let user: crate::settings::Settings = serde_json::from_str(r#"{"animationScopeBubbleLanguages":{"C++":{"syntax":"automatic","delimiters":["{}","()"]},"My DSL":{"syntax":"cpp"}}}"#).unwrap();
    let workspace: crate::settings::Settings =
        serde_json::from_str(r#"{"animationScopeBubbleLanguages":{"CPP":{"syntax":"disabled"}}}"#)
            .unwrap();
    let effective = user.merge(&workspace);
    let rules = Rules::compile(effective.animation_scope_bubble_languages.as_ref().unwrap());
    for alias in ["C++", "cpp", " C/C++ Header ", "HPP"] {
        assert_eq!(rules.get(alias).unwrap().syntax, Syntax::Disabled);
    }
    assert_eq!(rules.get("MY DSL").unwrap().syntax, Syntax::Cpp);
    assert!(rules.get("unconfigured").is_none());
    assert_eq!(rules.configs.len(), 2);
    let roundtrip: crate::settings::Settings =
        serde_json::from_value(serde_json::to_value(&effective).unwrap()).unwrap();
    assert_eq!(
        roundtrip.animation_scope_bubble_languages,
        effective.animation_scope_bubble_languages
    );
    assert!(Arc::ptr_eq(
        &Rules::compile(&Configs::new()),
        &Rules::empty()
    ));
    assert_eq!(config::language_key("  My DSL "), "my dsl");
}
