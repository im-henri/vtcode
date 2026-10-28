use crate::textmate::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[test]
fn every_bundled_grammar_matches_vscode_colors_and_styles() {
    let registry =
        crate::syntax_assets::build_syntax_set(&[
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("grammars")
        ])
        .unwrap();
    let reference: Value = serde_json::from_reader(flate2::read::GzDecoder::new(
        include_bytes!("testdata/textmate_reference.json.gz").as_slice(),
    ))
    .unwrap();
    let source = include_str!("testdata/theme_scope_corpus.txt");
    use sha2::Digest;
    assert_eq!(
        format!("{:x}", sha2::Sha256::digest(source.as_bytes())),
        reference["corpusSha256"].as_str().unwrap()
    );
    let themes: BTreeMap<_, _> = ["dark", "light", "monokai", "high-contrast"]
        .into_iter()
        .map(|key| {
            (
                key,
                crate::app_theme_loader::builtin_theme(key)
                    .unwrap()
                    .textmate_theme
                    .as_ref()
                    .unwrap(),
            )
        })
        .collect();
    let mut mismatches = Vec::new();
    let mut failures = 0;
    for expected in reference["languages"].as_array().unwrap() {
        let id = expected["id"].as_str().unwrap();
        let syntax = registry
            .syntaxes()
            .iter()
            .find(|s| s.id == id)
            .unwrap_or_else(|| panic!("Missing TextMate grammar {id}"));
        for expected_theme in expected["themes"].as_array().unwrap() {
            let key = expected_theme["key"].as_str().unwrap();
            let mut parser = crate::textmate_backend::State::new(syntax, &themes[key]);
            for (line_number, line) in source.trim_end().lines().enumerate() {
                let spans = parser.line(line, true, &mut Vec::new());
                let actual: Vec<_> = spans
                    .iter()
                    .flat_map(|(s, t)| std::iter::repeat_n(*s, t.len()))
                    .collect();
                assert_eq!(
                    actual.len(),
                    line.len(),
                    "{id}:{line_number}: token boundaries preserve text"
                );
                let runs = expected_theme["lines"][line_number].as_array().unwrap();
                for (i, run) in runs.iter().enumerate() {
                    let start = run[0].as_u64().unwrap() as usize;
                    let end = runs
                        .get(i + 1)
                        .map_or(line.len(), |r| r[0].as_u64().unwrap() as usize);
                    let color = Color::parse(run[1].as_str().unwrap()).unwrap();
                    let font = run[2].as_u64().unwrap() as u8;
                    if let Some(style) = actual[start..end]
                        .iter()
                        .find(|s| s.foreground != color || s.font_style.bits() != font)
                    {
                        failures += 1;
                        if mismatches.len() < 32 {
                            mismatches.push(format!("{id}/{key}:{}:{start} {:?}: expected {color:?}, font {font}; got {style:?}",line_number+1,&line[start..end]));
                        }
                    }
                }
            }
        }
    }
    assert_eq!(failures, 0, "{}", mismatches.join("\n"));
    let covered = reference["languages"].as_array().unwrap().len();
    assert_eq!(
        covered + 1,
        registry.syntaxes().len(),
        "All shipped grammars except Plain Text require a reference"
    );
}

#[test]
fn registry_detects_file_names_aliases_and_ambiguous_extensions() {
    let registry = SyntaxRegistry::embedded();
    for (file, id) in [
        ("rs", "rust"),
        ("py", "python"),
        ("mm", "objective-cpp"),
        ("rest", "rst"),
        ("Dockerfile", "docker"),
        ("CMakeLists.txt", "cmake"),
        ("tsx", "tsx"),
    ] {
        assert_eq!(
            registry
                .find_syntax_by_extension(file)
                .map(|s| s.id.as_str()),
            Some(id),
            "{file}"
        );
    }
    for (alias, id) in [
        ("C++", "cpp"),
        ("JS", "javascript"),
        ("TypeScript", "typescript"),
        ("Rust", "rust"),
    ] {
        assert_eq!(
            registry.find_syntax_by_token(alias).map(|s| s.id.as_str()),
            Some(id),
            "{alias}"
        );
    }
    for (header, id) in [
        ("# -*- mode: python -*-", "python"),
        ("// -*- c++ -*-", "cpp"),
        ("# vim: set ft=ruby:", "ruby"),
    ] {
        assert_eq!(
            registry
                .find_syntax_by_first_line(header)
                .map(|s| s.id.as_str()),
            Some(id)
        );
    }
}

fn feature_parser(grammar: Value) -> crate::textmate_backend::State {
    let definition =
        crate::textmate_grammar::parse_source(grammar.to_string().as_bytes(), "Feature").unwrap();
    let mut builder = SyntaxRegistryBuilder::new();
    builder.add(definition);
    let registry = builder.build().unwrap();
    let theme=Theme::new("Feature","#ffffff","#000000",&[
        json!({"scope":"comment","settings":{"foreground":"#778899"}}),
        json!({"scope":"entity.name.tag","settings":{"foreground":"#00ff00"}}),
        json!({"scope":"keyword.control","settings":{"foreground":"#ff0000","fontStyle":"bold italic underline strikethrough"}}),
    ]).unwrap();
    crate::textmate_backend::State::new(
        registry.find_syntax_by_scope("source.feature").unwrap(),
        &theme,
    )
}

#[test]
fn multiline_while_rules_resume_from_immutable_checkpoints_and_exit_cleanly() {
    let mut parser = feature_parser(json!({"scopeName":"source.feature","patterns":[
        {"begin":"^>","while":"^>","name":"comment.quote.feature","patterns":[]}
    ]}));
    let mut comments = Vec::new();
    let opening = parser.line("> first", true, &mut comments);
    assert!(
        opening
            .iter()
            .all(|(s, _)| s.foreground == Color::parse("#778899").unwrap())
    );
    assert_eq!(comments, [(0, 7)]);
    let mut checkpoint = parser.clone();
    assert_eq!(
        parser.line("> second", true, &mut Vec::new()),
        checkpoint.line("> second", true, &mut Vec::new())
    );
    let exit = parser.line("plain", true, &mut Vec::new());
    assert!(exit.iter().all(|(s, _)| s.foreground == Color::WHITE));
}

#[test]
fn capture_retokenization_applies_injections_and_all_four_font_styles() {
    let mut parser = feature_parser(json!({"scopeName":"source.feature","patterns":[
        {"begin":"BEGIN","end":"END","name":"meta.block.feature","beginCaptures":{
            "0":{"patterns":[{"match":"BEGIN","name":"entity.name.tag.feature"}]}
        }}
    ],"injections":{"L:meta.block.feature":{"patterns":[{"match":"BEGIN","name":"keyword.control.feature"}]}}}));
    let spans = parser.line("BEGIN content END", true, &mut Vec::new());
    let first = spans.first().unwrap();
    assert_eq!(first.1, "BEGIN");
    assert_eq!(first.0.foreground, Color::parse("#ff0000").unwrap());
    assert_eq!(
        first.0.font_style,
        FontStyle::BOLD | FontStyle::ITALIC | FontStyle::UNDERLINE | FontStyle::STRIKETHROUGH
    );
}

#[test]
fn textmate_plist_grammars_keep_their_scopes_and_patterns() {
    let source=crate::textmate_grammar::parse_source(br#"<?xml version="1.0"?><plist version="1.0"><dict>
        <key>name</key><string>XML Grammar</string><key>scopeName</key><string>source.feature</string>
        <key>patterns</key><array><dict><key>match</key><string>magic</string>
        <key>name</key><string>keyword.control.feature</string></dict></array></dict></plist>"#,"Fallback").unwrap();
    assert_eq!(source.name, "XML Grammar");
    let mut parser = feature_parser(source.grammar);
    assert_eq!(
        parser.line("magic", true, &mut Vec::new())[0].0.foreground,
        Color::parse("#ff0000").unwrap()
    );
}
