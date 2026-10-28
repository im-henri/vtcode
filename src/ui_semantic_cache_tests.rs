use super::*;
use crate::{
    editor::Editor,
    lsp::{DecodedSemanticToken, LspClient, LspManager, TestLspServer},
};
use lsp_types::{SemanticTokenModifier, SemanticTokenType, SemanticTokensLegend};

fn fixture() -> (
    LspManager,
    Vec<TestLspServer>,
    crate::semantic_theme::SemanticTheme,
) {
    let mut manager = LspManager::new(std::path::PathBuf::from("."));
    let mut servers = Vec::new();
    for (language, pattern) in [("python", "*.py"), ("rust", "*.rs")] {
        let (client, server) = LspClient::test_pair(language, &[pattern]);
        manager.clients.push(client);
        servers.push(server);
    }
    manager.semantic_token_legend = Some(SemanticTokensLegend {
        token_types: vec![SemanticTokenType::VARIABLE, SemanticTokenType::FUNCTION],
        token_modifiers: vec![SemanticTokenModifier::READONLY],
    });
    let theme = crate::semantic_theme::SemanticTheme::from_iter([
        ("variable".into(), (1, 2, 3)),
        ("variable.readonly:rust".into(), (4, 5, 6)),
        ("variable:python".into(), (7, 8, 9)),
    ]);
    (manager, servers, theme)
}

fn tokens() -> Vec<DecodedSemanticToken> {
    [(0, 0), (0, 1), (0, 1), (1, 0), (99, 0)]
        .into_iter()
        .map(|(token_type, _token_modifiers)| DecodedSemanticToken {
            line: 0,
            start_char: 0,
            length: 3,
            token_type,
            _token_modifiers,
        })
        .collect()
}

#[test]
fn editor_style_cache_preserves_language_server_and_missing_style_behavior() {
    let (mut manager, _servers, theme) = fixture();
    let tokens = tokens();
    for configured in [true, false] {
        if !configured {
            manager.clients.clear();
        }
        for filename in ["example.rs", "example.py", "example.txt"] {
            let mut editor = Editor::test_with_text("example");
            editor.file_path = Some(filename.into());
            for theme in [None, Some(&theme)] {
                let mut cache = FxHashMap::default();
                let mut reference = FxHashMap::default();
                for _ in 0..20 {
                    for token in &tokens {
                        let actual = editor_semantic_color_for_token(
                            token, &manager, &editor, theme, &mut cache,
                        );
                        let expected = semantic_color_for_token(
                            token,
                            manager.semantic_legend_for_file(editor.file_path.as_deref()),
                            editor.language_name(),
                            theme,
                            &mut reference,
                        );
                        assert_eq!(actual, expected, "file={filename}, configured={configured}");
                    }
                }
                assert_eq!(cache.len(), if theme.is_some() { 4 } else { 0 });
            }
        }
    }
}

#[test]
#[ignore = "comparative release benchmark"]
fn benchmark_editor_semantic_style_cache() {
    use std::{hint::black_box, time::Instant};
    let (manager, _servers, theme) = fixture();
    let mut editor = Editor::test_with_text("value");
    editor.file_path = Some("src/very/deep/project/module/source.rs".into());
    let tokens = tokens();
    let mut old = Vec::new();
    let mut new = Vec::new();
    for round in 0..9 {
        for optimized in [round % 2 == 0, round % 2 != 0] {
            let start = Instant::now();
            for _ in 0..100 {
                let mut cache = FxHashMap::default();
                for token in tokens.iter().cycle().take(2_000) {
                    black_box(if optimized {
                        editor_semantic_color_for_token(
                            black_box(token),
                            black_box(&manager),
                            black_box(&editor),
                            Some(&theme),
                            &mut cache,
                        )
                    } else {
                        semantic_color_for_token(
                            black_box(token),
                            manager
                                .semantic_legend_for_file(black_box(editor.file_path.as_deref())),
                            black_box(&editor).language_name(),
                            Some(&theme),
                            &mut cache,
                        )
                    });
                }
            }
            let elapsed = start.elapsed() / 100;
            if optimized {
                new.push(elapsed);
            } else {
                old.push(elapsed);
            }
        }
    }
    old.sort_unstable();
    new.sort_unstable();
    eprintln!(
        "semantic styles (2000 tokens): before {:?}, after {:?}, {:.2}x",
        old[4],
        new[4],
        old[4].as_secs_f64() / new[4].as_secs_f64()
    );
}
