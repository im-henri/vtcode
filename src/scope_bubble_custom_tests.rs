use super::*;
use crate::{scope_bubble_config::Config, textmate::{GrammarSource, SyntaxRegistry, SyntaxRegistryBuilder}};
use std::sync::Arc;

const SOURCE: &str = r#"module Core
{
    input text = "a misleading } brace";
    // a misleading { brace
    /* another } brace */
#if FEATURE
    process decode
    {
        if (ready)
            issue();
        else {
            recover();
        }
    }
#else
    process fallback {
        fallback_work();
    }
#endif
}
"#;

fn grammar() -> GrammarSource {
    let mut source = crate::textmate_grammar::parse_source(include_bytes!("testdata/scope-bubble-custom.tmLanguage.json"), "Scope Example").unwrap();
    source.aliases.push("Scope Alias".into());
    source.filenames.push("Scopefile".into());
    source
}

fn registry(source: GrammarSource) -> Arc<SyntaxRegistry> {
    let mut builder = SyntaxRegistryBuilder::new();
    builder.add(source);
    Arc::new(builder.build().unwrap())
}

fn rules(registry: Arc<SyntaxRegistry>) -> Arc<Rules> {
    Rules::empty().with_automatic(crate::scope_bubble_config::Automatic::new(registry))
}

fn index(editor: &Editor) -> StructuralScopeIndex {
    StructuralScopeRequest::from_editor(editor).expect("custom language has a scope parser")
        .build(None, || false).unwrap()
}

fn at(editor: &mut Editor, marker: &str) {
    let offset = SOURCE[..SOURCE.find(marker).unwrap()].chars().count();
    editor.cursor_line = editor.rope.char_to_line(offset);
    editor.cursor_col = offset - editor.rope.line_to_char(editor.cursor_line);
}

#[test]
fn registered_custom_c_like_scopes_match_cpp_with_braces_comments_and_preprocessor() {
    let mut sources = vec![grammar()];
    // Also validate a supplied real-world grammar without embedding private
    // language assets or a machine-specific path in the regression fixture.
    if let Some(path) = std::env::var_os("VTCODE_SCOPE_TEST_GRAMMAR") {
        sources.push(crate::textmate_grammar::read_source(std::path::Path::new(&path)).unwrap());
    }
    for source in sources {
        let extension = source.grammar["fileTypes"][0].as_str().unwrap().to_owned();
        let rules = rules(registry(source.clone()));
        assert!(rules.automatic.is_some(), "{} declares C-style blocks and comments", source.name);
        assert!(rules.configs.is_empty(), "automatic defaults are not persisted as user preferences");
        let mut reference = Editor::test_with_text(SOURCE);
        reference.language_override = Some("C++".into());
        let expected = index(&reference);
        for mode in 0..3 {
            let mut editor = Editor::test_with_text(SOURCE);
            editor.scope_bubble_rules = rules.clone();
            editor.file_path = Some(format!("file.{extension}").into());
            if mode == 1 { editor.language_override = Some(source.name.clone()); }
            if mode == 2 {
                editor.file_path = Some("file.other".into());
                editor.language_associations.insert("*.other".into(), source.id.clone());
            }
            let actual = index(&editor);
            for marker in ["input text", "issue();", "recover();", "fallback_work();"] {
                at(&mut editor, marker);
                at(&mut reference, marker);
                let scope = actual.enclosing_scope(&editor);
                assert!(scope.is_some(), "{}, mode={mode}, marker={marker}", source.name);
                assert_eq!(scope, expected.enclosing_scope(&reference));
                assert_eq!(scope, enclosing_scope(&editor), "worker and immediate paths agree");
            }
            assert_eq!(editor.language_override.as_deref(), (mode == 1).then_some(source.name.as_str()));
        }
    }
}

#[test]
fn custom_scope_automatic_profiles_honor_explicit_rules_and_language_overrides() {
    let automatic = crate::scope_bubble_config::Automatic::new(registry(grammar()));
    let mut editor = Editor::test_with_text("task {\n call[\n value\n ];\n}\n");
    editor.file_path = Some("file.scopedef".into());
    editor.cursor_line = 2;
    editor.cursor_col = 2;
    editor.scope_bubble_rules = Rules::compile(&[("Scope Example".into(), Config {
        syntax: Syntax::Automatic, delimiters: Some(vec![Delimiter::Brackets]), language_blocks: false,
    })].into()).with_automatic(automatic.clone());
    assert_eq!(index(&editor).enclosing_scope(&editor), Some(((1, 5), (3, 1))));
    editor.scope_bubble_rules = Rules::compile(&[("Scope Example".into(), Config {
        syntax: Syntax::Disabled, ..Default::default()
    })].into()).with_automatic(automatic.clone());
    assert!(StructuralScopeRequest::from_editor(&editor).is_none());
    editor.scope_bubble_rules = Rules::empty().with_automatic(automatic);
    let mut settings = crate::animation_config::AnimationSettings::default();
    settings.set(crate::animation_config::AnimationEffect::ScopeConnector, true);
    assert!(settings.scope_enabled_for(&editor));
    settings.scope_disabled_languages = vec!["scope example".into()];
    assert_eq!(crate::scope_bubble_config::language(&editor), "Scope Example");
    assert!(!settings.scope_enabled_for(&editor), "language switches use the detected grammar even for its alternate file extensions");
    for mode in ["Plain Text", "Python"] {
        editor.language_override = Some(mode.into());
        assert_ne!(editor.scope_bubble_rules.for_editor(&editor).map(|rule| rule.syntax), Some(Syntax::Cpp));
    }
    editor.language_override = Some("Plain Text".into());
    assert!(StructuralScopeRequest::from_editor(&editor).is_none());
    editor.language_override = None;
    editor.file_path = Some("unknown.unregistered".into());
    assert!(StructuralScopeRequest::from_editor(&editor).is_none(), "unrelated files are not guessed from their content");
}

#[test]
fn custom_scope_grammar_metadata_is_cached_and_rule_changes_invalidate_analysis() {
    let registry = registry(grammar());
    let automatic = crate::scope_bubble_config::Automatic::new(registry).unwrap();
    let rules = Rules::empty().with_automatic(Some(automatic.clone()));
    assert!(Arc::ptr_eq(&rules, &rules.with_automatic(Some(automatic.clone()))));
    let mut editor = Editor::test_with_text(SOURCE);
    editor.file_path = Some("file.customscope".into());
    editor.scope_bubble_rules = rules;
    let request = StructuralScopeRequest::from_editor(&editor).unwrap();
    editor.scope_bubble_rules = editor.scope_bubble_rules.with_automatic(None);
    assert!(!request.matches(&editor));
    assert!(StructuralScopeRequest::from_editor(&editor).is_none());
}

#[test]
fn registered_custom_language_paints_scope_bubbles_in_the_editor() {
    use crate::{app::{ActivePane, App, Tab}, animation_config::AnimationEffect};
    use ratatui::{Terminal, backend::TestBackend};
    let automatic = crate::scope_bubble_config::Automatic::new(registry(grammar()));
    for enabled in [false, true] {
        let mut editor = Editor::test_with_text("module Core\n{\n    process decode {\n        issue();\n    }\n}\n");
        editor.file_path = Some("file.customscope".into());
        editor.cursor_line = 3;
        editor.cursor_col = 10;
        let mut app = App::test_new();
        app.show_explorer = false;
        app.show_minimap = false;
        app.active_pane = ActivePane::Editor;
        app.animation_settings.set(AnimationEffect::ScopeConnector, true);
        let rules = Rules::empty().with_automatic(enabled.then(|| automatic.clone().unwrap()));
        editor.scope_bubble_rules = rules.clone();
        app.animation_settings.scope_languages = rules;
        app.tabs = vec![Tab::new_file(editor)];
        app.active_tab = 0;
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        app.animations.advance_frame(&app.animation_settings, std::time::Instant::now() + std::time::Duration::from_secs(2));
        terminal.draw(|frame| crate::ui::draw(frame, &mut app)).unwrap();
        let corners = terminal.backend().buffer().content.iter()
            .filter(|cell| matches!(cell.symbol(), "╭" | "╰" | "╮" | "╯")).count();
        assert_eq!(corners >= 2, enabled, "custom grammar defaults must reach the actual renderer");
    }
}
