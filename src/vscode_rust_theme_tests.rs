use super::*;
use crate::textmate::FontStyle;
use crate::textmate_backend::HighlightLines;

#[test]
fn rust_colors_and_font_styles_match_vscode_tokenizer_in_all_bundled_themes() {
    let set = crate::syntax_assets::build_syntax_set(&[
        Path::new(env!("CARGO_MANIFEST_DIR")).join("grammars/Rust")
    ])
    .unwrap();
    let syntax = set.find_syntax_by_extension("rs").unwrap();
    let source = include_str!("testdata/rust_highlighting.rs");
    let reference: Value =
        serde_json::from_str(include_str!("testdata/rust_theme_reference.json")).unwrap();
    let mut mismatches = Vec::new();
    for expected in reference["themes"].as_array().unwrap() {
        let key = expected["key"].as_str().unwrap();
        let theme = crate::app_theme_loader::builtin_theme(key).unwrap();
        let mut parser = HighlightLines::new(syntax, theme.textmate_theme.as_ref().unwrap());
        for (line_index, line) in source.lines().enumerate() {
            let input = format!("{line}\n");
            let spans = parser.highlight_line(&input);
            let styles: Vec<_> = spans
                .iter()
                .flat_map(|(s, text)| std::iter::repeat_n(*s, text.len()))
                .collect();
            let runs = expected["lines"][line_index].as_array().unwrap();
            for (index, run) in runs.iter().enumerate() {
                let from = run[0].as_u64().unwrap() as usize;
                let to = runs
                    .get(index + 1)
                    .map_or(line.len(), |next| next[0].as_u64().unwrap() as usize);
                let color = parse_syntax_color(run[1].as_str().unwrap()).unwrap();
                let font = run[2].as_u64().unwrap();
                let mut font_style = FontStyle::empty();
                if font & 1 != 0 {
                    font_style |= FontStyle::ITALIC;
                }
                if font & 2 != 0 {
                    font_style |= FontStyle::BOLD;
                }
                if font & 4 != 0 {
                    font_style |= FontStyle::UNDERLINE;
                }
                if let Some(actual) = styles[from..to]
                    .iter()
                    .find(|s| s.foreground != color || s.font_style != font_style)
                {
                    if mismatches.len() < 40 {
                        mismatches.push(format!("{key}:{}:{from} {:?}: expected {color:?} {font_style:?}, got {actual:?}",
                            line_index + 1, &line[from..to]));
                    }
                }
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn semantic_highlighting_can_be_disabled_by_an_imported_theme() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("disabled.json");
    std::fs::write(
        &path,
        r##"{"semanticHighlighting": false,
        "tokenColors":[{"scope":"entity.name.type","settings":{"foreground":"#123456"}}],
        "semanticTokenColors":{"type":"#abcdef"}}"##,
    )
    .unwrap();
    assert!(
        load_theme_file(&path, None, None)
            .unwrap()
            .semantic_colors
            .is_none()
    );
}

#[test]
fn inherited_semantic_rules_keep_their_source_order_and_child_overrides_win_ties() {
    use lsp_types::{SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend};
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("base.json"),
        r##"{"semanticTokenColors":{
        "variable.readonly":"#111111", "variable.static":"#222222"}}"##,
    )
    .unwrap();
    let path = temp.path().join("child.json");
    std::fs::write(
        &path,
        r##"{"include":"base.json","semanticTokenColors":{
        "variable.readonly":"#333333"}}"##,
    )
    .unwrap();
    let theme = load_theme_file(&path, None, None).unwrap();
    let legend = SemanticTokensLegend {
        token_types: vec![SemanticTokenType::VARIABLE],
        token_modifiers: vec![
            SemanticTokenModifier::READONLY,
            SemanticTokenModifier::STATIC,
        ],
    };
    let token = crate::lsp::DecodedSemanticToken {
        line: 0,
        start_char: 0,
        length: 1,
        token_type: 0,
        _token_modifiers: 3,
    };
    assert_eq!(
        theme
            .semantic_colors
            .unwrap()
            .style(&token, &legend, "rust")
            .unwrap()
            .fg,
        Some(ratatui::style::Color::Rgb(0x33, 0x33, 0x33))
    );
}
