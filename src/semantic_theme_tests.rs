use super::*;
use lsp_types::{SemanticTokenModifier, SemanticTokenType};
use serde_json::json;

fn resolve(
    theme: &SemanticTheme,
    kind: &'static str,
    modifiers: &[&'static str],
    language: &str,
) -> Option<Style> {
    let legend = SemanticTokensLegend {
        token_types: vec![SemanticTokenType::new(kind)],
        token_modifiers: modifiers
            .iter()
            .map(|s| SemanticTokenModifier::new(*s))
            .collect(),
    };
    let token = DecodedSemanticToken {
        line: 0,
        start_char: 0,
        length: 3,
        token_type: 0,
        _token_modifiers: (1u32 << modifiers.len()) - 1,
    };
    theme.style(&token, &legend, language)
}

fn theme(tokens: Value, semantic: Value) -> SemanticTheme {
    SemanticTheme::new(
        tokens.as_array().unwrap(),
        semantic.as_object().unwrap(),
        "#272822",
    )
}

#[test]
fn monokai_semantics_use_vscode_scope_fallbacks_including_font_styles() {
    let theme = crate::app_theme_loader::builtin_theme("monokai")
        .unwrap()
        .semantic_colors
        .as_ref()
        .unwrap();
    for kind in [
        "namespace",
        "type",
        "struct",
        "class",
        "interface",
        "enum",
        "typeAlias",
    ] {
        let style = Style::default().patch(resolve(theme, kind, &[], "rust").unwrap());
        assert_eq!(style.fg, Some(Color::Rgb(166, 226, 46)), "{kind}");
        assert!(style.add_modifier.contains(Modifier::UNDERLINED), "{kind}");
    }
    let parameter = Style::default().patch(resolve(theme, "parameter", &[], "rust").unwrap());
    assert_eq!(parameter.fg, Some(Color::Rgb(253, 151, 31)));
    assert!(parameter.add_modifier.contains(Modifier::ITALIC));
    for (kind, modifiers) in [("type", vec!["defaultLibrary"]), ("builtinType", vec![])] {
        let style = Style::default()
            .add_modifier(Modifier::UNDERLINED)
            .patch(resolve(theme, kind, &modifiers, "rust").unwrap());
        assert_eq!(style.fg, Some(Color::Rgb(102, 217, 239)));
        assert!(style.add_modifier.contains(Modifier::ITALIC));
        assert!(!style.add_modifier.contains(Modifier::UNDERLINED));
    }
}

#[test]
fn semantic_selectors_merge_each_property_and_use_vscode_specificity() {
    let theme = theme(
        json!([]),
        crate::vscode_theme::parse_jsonc(
            r##"{
        "variable:rust":{"foreground":"#112233","italic":true},
        "*.readonly.static":{"foreground":"#445566","bold":true},
        "variable.readonly":{"underline":true},
        "*.static":{"strikethrough":true}
    }"##,
        )
        .unwrap(),
    );
    let style = resolve(&theme, "variable", &["readonly", "static"], "rust").unwrap();
    assert_eq!(style.fg, Some(Color::Rgb(0x44, 0x55, 0x66)));
    assert!(style.add_modifier.contains(
        Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED | Modifier::CROSSED_OUT
    ));
}

#[test]
fn explicit_semantic_properties_override_defaults_and_missing_properties_fall_back() {
    let theme = theme(
        json!([
            {"scope":"entity.name.type","settings":{"foreground":"#112233","fontStyle":"italic underline"}}
        ]),
        json!({"type":{"foreground":"#445566", "italic":false}}),
    );
    let style = Style::default()
        .add_modifier(Modifier::ITALIC)
        .patch(resolve(&theme, "type", &[], "rust").unwrap());
    assert_eq!(style.fg, Some(Color::Rgb(0x44, 0x55, 0x66)));
    assert!(style.add_modifier.contains(Modifier::UNDERLINED));
    assert!(!style.add_modifier.contains(Modifier::ITALIC));
}

#[test]
fn empty_font_style_clears_syntax_modifiers_and_unknown_scopes_keep_syntax_colors() {
    let theme = theme(json!([]), json!({"variable":{"fontStyle":""}}));
    let syntax = Style::default().fg(Color::Rgb(1, 2, 3)).add_modifier(
        Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED | Modifier::CROSSED_OUT,
    );
    let style = syntax.patch(resolve(&theme, "variable", &[], "rust").unwrap());
    assert_eq!(style.fg, syntax.fg);
    assert_eq!(style.add_modifier, Modifier::empty());
    assert!(resolve(&theme, "unmapped", &[], "rust").is_none());
}

#[test]
fn ordered_scope_probes_stop_at_the_first_styled_probe() {
    let theme = theme(
        json!([
            {"scope":"entity.name.type","settings":{"fontStyle":"italic"}},
            {"scope":"support.type","settings":{"foreground":"#123456"}}
        ]),
        json!({}),
    );
    let style = resolve(&theme, "type", &[], "python").unwrap();
    assert_eq!(style.fg, None);
    assert!(style.add_modifier.contains(Modifier::ITALIC));
}

#[test]
fn rust_custom_types_inherit_explicit_semantic_rules_without_affecting_other_languages() {
    let theme = theme(json!([]), json!({"type":{"foreground":"#123456"}}));
    for kind in ["typeAlias", "union", "builtinType"] {
        assert_eq!(
            resolve(&theme, kind, &[], "rust").unwrap().fg,
            Some(Color::Rgb(0x12, 0x34, 0x56))
        );
        assert!(resolve(&theme, kind, &[], "python").is_none());
    }
}

#[test]
fn semantic_font_styles_reach_wrapped_and_unwrapped_editor_cells() {
    use crate::{
        app::{App, Tab},
        editor::Editor,
    };
    use ratatui::{Terminal, backend::TestBackend};
    for wrap in [false, true] {
        let mut editor = Editor::test_with_text(&"example ".repeat(40));
        editor.file_path = Some("semantic-style.rs".into());
        editor.word_wrap = wrap;
        let mut app = App::test_new();
        app.tabs = vec![Tab::new_file(editor)];
        app.show_semantic_tokens = true;
        app.show_minimap = false;
        app.sticky_scroll = false;
        let (client, _server) = crate::lsp::LspClient::test_pair("rust", &["*.rs"]);
        app.lsp_manager.clients.push(client);
        app.lsp_manager.semantic_token_legend = Some(SemanticTokensLegend {
            token_types: vec![SemanticTokenType::VARIABLE],
            token_modifiers: vec![],
        });
        app.lsp_semantic_tokens = vec![DecodedSemanticToken {
            line: 0,
            start_char: 0,
            length: 320,
            token_type: 0,
            _token_modifiers: 0,
        }];
        app.semantic_theme_colors = Some(theme(
            json!([]),
            json!({"variable":{
            "foreground":"#123456","fontStyle":"bold italic underline strikethrough"}}),
        ));
        let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                crate::ui_editor::draw_editor(f, &mut app, area);
            })
            .unwrap();
        let area = app.code_area.unwrap();
        let cell = &terminal.backend().buffer()[(area.x + 2, area.y)];
        assert_eq!(cell.fg, Color::Rgb(0x12, 0x34, 0x56));
        assert!(cell.modifier.contains(
            Modifier::BOLD | Modifier::ITALIC | Modifier::UNDERLINED | Modifier::CROSSED_OUT
        ));
    }
}
