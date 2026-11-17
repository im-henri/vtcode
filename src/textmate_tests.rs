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
        ("Expr.G4", "antlr"),
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
        ("ANTLR4", "antlr"),
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

#[test]
fn antlr4_file_highlights_grammar_rules_literals_and_comments() {
    let registry = SyntaxRegistry::embedded();
    let path = std::path::Path::new("Expr.g4");
    let syntax = crate::highlight::select_syntax(&registry, Some(path), None, "grammar Expr;");
    assert_eq!(syntax.name, "ANTLR4");
    assert_eq!(syntax.scope, "source.antlr");
    assert!(registry.find_syntax_by_extension("Expr.g").is_none());

    let theme = Theme::new(
        "ANTLR4 test",
        "#ffffff",
        "#000000",
        &[
            json!({"scope":"keyword.other.antlr","settings":{"foreground":"#ff0000"}}),
            json!({"scope":"entity.name.function.parser.antlr","settings":{"foreground":"#00ff00"}}),
            json!({"scope":"string.quoted.single.antlr","settings":{"foreground":"#0000ff"}}),
            json!({"scope":"comment.line.double-slash.antlr","settings":{"foreground":"#888888"}}),
        ],
    )
    .unwrap();
    let mut parser = crate::textmate_backend::State::new(syntax, &theme);
    for (line, sample, color) in [
        ("grammar Expr;", "grammar", "#ff0000"),
        ("expr: NUMBER;", "expr", "#00ff00"),
        ("NUMBER: '1';", "'1'", "#0000ff"),
        ("// A comment", "// A comment", "#888888"),
    ] {
        let start = line.find(sample).unwrap();
        let expected = Color::parse(color).unwrap();
        let spans = parser.line(line, true, &mut Vec::new());
        let colors: Vec<_> = spans
            .iter()
            .flat_map(|(style, text)| std::iter::repeat_n(style.foreground, text.len()))
            .collect();
        assert!(
            colors[start..start + sample.len()]
                .iter()
                .all(|color| *color == expected),
            "{line}"
        );
    }
}

#[test]
fn c_family_filenames_select_the_expected_full_language_grammar() {
    for registry in [SyntaxRegistry::embedded(), crate::syntax_assets::build_syntax_set(&[
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("grammars")
    ]).unwrap()] {
        for (file, id) in [
            ("c", "c"), ("sample.c", "c"), ("sample.i", "c"),
            ("sample.C", "cpp"), ("sample.cp", "cpp"), ("sample.cpp", "cpp"),
            ("sample.cxx", "cpp"), ("sample.h", "cpp"), ("sample.hpp", "cpp"),
            ("sample.cs", "csharp"), ("sample.CS", "csharp"), ("script.csx", "csharp"),
            ("sample.m", "objective-c"), ("sample.M", "objective-cpp"),
        ] {
            assert_eq!(registry.find_syntax_by_extension(file).map(|syntax| syntax.id.as_str()),
                Some(id), "wrong grammar for {file}");
        }
    }
}

#[test]
fn detected_c_colors_match_manually_selected_c_for_complete_documents() {
    let registry = crate::syntax_assets::build_syntax_set(&[
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("grammars")
    ]).unwrap();
    let path = std::path::Path::new("source.c");
    let detected = crate::highlight::select_syntax(&registry, Some(path), None, "#include <stdio.h>");
    let manual = crate::highlight::select_syntax(&registry, Some(path), Some("C"), "#include <stdio.h>");
    assert_eq!(detected.id, manual.id);
    assert_eq!(detected.scope, "source.c");
    let source = "#include <stdio.h>\n#define LIMIT 3\ntypedef struct Node { const char *label; int count; } Node;\nstatic int add(const Node *node, int value) {\n    /* a multiline\n       comment */\n    if (node == NULL) return -1;\n    printf(\"%s %d\\n\", node->label, value + LIMIT);\n    return node->count + value;\n}\n";
    for key in ["dark", "light", "monokai", "high-contrast"] {
        let file = crate::app_theme_loader::builtin_theme(key).unwrap();
        let theme = file.textmate_theme.as_ref().unwrap();
        let mut automatic = crate::textmate_backend::State::new(detected, theme);
        let mut selected = crate::textmate_backend::State::new(manual, theme);
        for line in source.lines() {
            let mut automatic_comments = Vec::new();
            let mut selected_comments = Vec::new();
            assert_eq!(automatic.line(line, true, &mut automatic_comments),
                selected.line(line, true, &mut selected_comments), "{key}: {line}");
            assert_eq!(automatic_comments, selected_comments);
        }
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

fn assert_scope_colors(syntax: &Syntax, rules: &[(&str, &str)], lines: &[(&str, &str, &str)]) {
    let rules: Vec<_> = rules
        .iter()
        .map(|(scope, color)| json!({"scope": scope, "settings": {"foreground": color}}))
        .collect();
    let theme = Theme::new("Git test", "#ffffff", "#000000", &rules).unwrap();
    let mut parser = crate::textmate_backend::State::new(syntax, &theme);
    for (line, sample, color) in lines {
        let spans = parser.line(line, true, &mut Vec::new());
        if sample.is_empty() {
            continue;
        }
        let start = line.find(sample).unwrap();
        let expected = Color::parse(color).unwrap();
        let colors: Vec<_> = spans
            .iter()
            .flat_map(|(style, text)| std::iter::repeat_n(style.foreground, text.len()))
            .collect();
        assert!(
            colors[start..start + sample.len()].iter().all(|c| *c == expected),
            "{line:?}: {sample:?} should be {color}, got {:?}",
            &colors[start..start + sample.len()]
        );
    }
}

#[test]
fn git_commit_messages_highlight_subject_trailers_and_references() {
    let registry = SyntaxRegistry::embedded();
    for name in ["COMMIT_EDITMSG", "MERGE_MSG", "SQUASH_MSG", ".git/COMMIT_EDITMSG"] {
        let syntax = crate::highlight::select_syntax(
            &registry,
            Some(std::path::Path::new(name)),
            None,
            "",
        );
        assert_eq!(syntax.id, "git-commit", "{name}");
    }
    let syntax = registry
        .find_syntax_by_token(crate::git_amend::LANGUAGE)
        .unwrap();
    assert_eq!(syntax.id, "git-commit");
    assert_scope_colors(
        syntax,
        &[
            ("markup.heading", "#ff0000"),
            ("keyword.other.type", "#00ff00"),
            ("keyword.other.trailer", "#0000ff"),
            ("markup.underline.link", "#ffff00"),
            ("constant.other.reference.issue", "#00ffff"),
            ("comment", "#888888"),
            ("invalid.illegal", "#ff00ff"),
        ],
        &[
            ("feat(editor): Highlight commit messages", "Highlight commit messages", "#ff0000"),
            ("", "", ""),
            ("Fixes #42 by adding a grammar.", "#42", "#00ffff"),
            ("See https://example.com/docs for details.", "https://example.com/docs", "#ffff00"),
            ("Signed-off-by: A Person <a@example.com>", "Signed-off-by", "#0000ff"),
            ("Signed-off-by: A Person <a@example.com>", "a@example.com", "#ffff00"),
            ("# Please enter the commit message", "# Please enter the commit message", "#888888"),
        ],
    );
    // The type prefix of a Conventional Commits subject stands out.
    assert_scope_colors(
        syntax,
        &[("markup.heading", "#ff0000"), ("keyword.other.type", "#00ff00")],
        &[("fix: Repair the parser", "fix", "#00ff00")],
    );
    // Subjects longer than 72 characters are flagged.
    let long = format!("{}{}", "a".repeat(72), "overflow");
    assert_scope_colors(
        syntax,
        &[("markup.heading", "#ff0000"), ("invalid.illegal", "#ff00ff")],
        &[(&long, "overflow", "#ff00ff")],
    );
}

#[test]
fn git_log_output_highlights_hashes_refs_headers_and_patches() {
    let registry = SyntaxRegistry::embedded();
    let syntax = registry.find_syntax_by_name("Git Log").unwrap();
    assert_eq!(syntax.scope, "text.git-log");
    assert_eq!(registry.find_syntax_by_extension("history.gitlog").unwrap().id, "git-log");
    assert_eq!(
        registry
            .find_syntax_by_first_line("commit 0123456789abcdef0123456789abcdef01234567 (HEAD -> main)")
            .unwrap()
            .id,
        "git-log"
    );
    assert_eq!(
        registry.find_syntax_by_first_line("* commit a033e42d").unwrap().id,
        "git-log"
    );
    let rules = [
        ("constant.sha", "#ffff00"),
        ("keyword.other.head", "#00ffff"),
        ("entity.name.type.branch", "#00ff00"),
        ("string.other.remote-branch", "#ff0000"),
        ("entity.name.tag", "#ff8800"),
        ("keyword.other.header", "#0000ff"),
        ("markup.underline.link", "#8888ff"),
        ("constant.numeric.date", "#888800"),
        ("markup.inserted", "#00aa00"),
        ("markup.deleted", "#aa0000"),
        ("punctuation.definition.graph", "#444444"),
    ];
    let header = "commit a033e42d1f2b3c4d5e6f708192a3b4c5d6e7f809 (HEAD -> main, origin/main, tag: v0.7.4)";
    assert_scope_colors(
        syntax,
        &rules,
        &[
            (header, "a033e42d1f2b3c4d5e6f708192a3b4c5d6e7f809", "#ffff00"),
            (header, "HEAD", "#00ffff"),
            (header, "main", "#00ff00"),
            (header, "origin/main", "#ff0000"),
            (header, "v0.7.4", "#ff8800"),
            ("Author: A Person <a@example.com>", "Author", "#0000ff"),
            ("Author: A Person <a@example.com>", "a@example.com", "#8888ff"),
            ("Date:   Mon Nov 16 12:00:00 2026 +0200", "Mon Nov 16 12:00:00 2026 +0200", "#888800"),
            (" src/textmate.rs | 4 ++--", "++", "#00aa00"),
            (" src/textmate.rs | 4 ++--", "--", "#aa0000"),
            (" 1 file changed, 2 insertions(+), 2 deletions(-)", "2 insertions(+)", "#00aa00"),
            ("* a033e42 (HEAD -> main) Release VTCode", "a033e42", "#ffff00"),
            ("* a033e42 (HEAD -> main) Release VTCode", "*", "#444444"),
            ("| * 1fd6f9d Open an editable commit message tab", "1fd6f9d", "#ffff00"),
        ],
    );
    // Patches from `git log -p` use the diff grammar until the next commit.
    assert_scope_colors(
        syntax,
        &rules,
        &[
            ("diff --git a/x b/x", "", ""),
            ("+added", "+added", "#00aa00"),
            ("-removed", "-removed", "#aa0000"),
            ("commit 1fd6f9d0aa", "1fd6f9d0aa", "#ffff00"),
        ],
    );
}
