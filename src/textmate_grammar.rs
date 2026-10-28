//! Read TextMate JSON/plist grammars and VS Code grammar contributions intact.
use crate::textmate::{GrammarSource, SyntaxRegistry, SyntaxRegistryBuilder};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

pub(crate) fn is_grammar_path(path: &Path) -> bool {
    path.extension().is_some_and(|e| {
        e.eq_ignore_ascii_case("json")
            || e.eq_ignore_ascii_case("tmLanguage")
            || e.eq_ignore_ascii_case("plist")
    })
}
pub(crate) fn parse_value(bytes: &[u8]) -> Result<Value, String> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| "TextMate files must use UTF-8.")?
        .trim_start_matches('\u{feff}');
    if text.trim_start().starts_with('<') {
        let plist = plist::Value::from_reader_xml(text.as_bytes()).map_err(|e| e.to_string())?;
        serde_json::to_value(plist).map_err(|e| e.to_string())
    } else {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }
}
pub(crate) fn parse_source(bytes: &[u8], fallback: &str) -> Result<GrammarSource, String> {
    let grammar = parse_value(bytes)?;
    let scope = grammar["scopeName"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("TextMate grammar needs a scopeName.")?;
    if !grammar["patterns"].is_array() {
        return Err("TextMate grammar needs a patterns array.".into());
    }
    let name = grammar["name"].as_str().unwrap_or(fallback).to_owned();
    Ok(GrammarSource {
        id: scope.to_owned(),
        name,
        aliases: vec![],
        filenames: vec![],
        grammar,
    })
}
pub(crate) fn read_source(path: &Path) -> Result<GrammarSource, String> {
    parse_source(
        &std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?,
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("TextMate"),
    )
    .map_err(|e| format!("{}: {e}", path.display()))
}
pub(crate) fn add_package(
    builder: &mut SyntaxRegistryBuilder,
    manifest: &Path,
) -> Result<BTreeSet<PathBuf>, String> {
    let package: Value =
        serde_json::from_slice(&std::fs::read(manifest).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let mut files = BTreeSet::new();
    let root = manifest.parent().ok_or("Package has no parent folder")?;
    for contribution in package
        .pointer("/contributes/grammars")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let path = root.join(
            contribution["path"]
                .as_str()
                .ok_or("Grammar contribution needs a path")?,
        );
        let mut source = read_source(&path)?;
        if let Some(scope) = contribution["scopeName"].as_str() {
            if source.grammar["scopeName"] != scope {
                return Err(format!(
                    "{}: scopeName disagrees with package.json",
                    path.display()
                ));
            }
        }
        if let Some(inject) = contribution.get("injectTo") {
            source.grammar["injectTo"] = inject.clone();
        }
        if let Some(language) = package
            .pointer("/contributes/languages")
            .and_then(Value::as_array)
            .and_then(|ls| {
                ls.iter()
                    .find(|l| !l["id"].is_null() && l["id"] == contribution["language"])
            })
        {
            source.id = language["id"].as_str().unwrap_or(&source.id).to_owned();
            source.aliases = language["aliases"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            if let Some(name) = source.aliases.first() {
                source.name = name.clone();
            }
            if let Some(extensions) = language["extensions"].as_array() {
                source.grammar["fileTypes"] = extensions
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|s| Value::String(s.trim_start_matches('.').into()))
                    .collect();
            }
            source.filenames = language["filenames"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            if let Some(first) = language.get("firstLine") {
                source.grammar["firstLineMatch"] = first.clone();
            }
        }
        builder.add(source);
        files.insert(path);
    }
    Ok(files)
}

/// Check a new grammar's dependencies before replacing an installed file.
/// Nested repositories inherit enclosing repositories, as in TextMate.
pub(crate) fn validate_references(
    source: &GrammarSource,
    registry: &SyntaxRegistry,
) -> Result<(), String> {
    fn visit(
        value: &Value,
        repositories: &[BTreeSet<String>],
        registry: &SyntaxRegistry,
    ) -> Result<(), String> {
        match value {
            Value::Object(object) => {
                let mut repositories = repositories.to_vec();
                if let Some(repository) = object.get("repository").and_then(Value::as_object) {
                    repositories.push(repository.keys().cloned().collect());
                }
                if let Some(include) = object.get("include").and_then(Value::as_str) {
                    if let Some(key) = include.strip_prefix('#') {
                        if !repositories.iter().any(|r| r.contains(key)) {
                            return Err(format!("Unavailable TextMate repository: {include}"));
                        }
                    } else if !matches!(include, "$self" | "$base") {
                        let scope = include.split('#').next().unwrap_or(include);
                        if registry.find_syntax_by_scope(scope).is_none() {
                            return Err(format!(
                                "Unavailable TextMate grammar: {scope}. Install its required grammar first."
                            ));
                        }
                    }
                }
                for key in ["match", "begin", "end", "while"] {
                    if let Some(pattern) = object.get(key).and_then(Value::as_str) {
                        // End/while backreferences are substituted from the begin
                        // match before compilation by the TextMate tokenizer.
                        let pattern = if matches!(key, "end" | "while") {
                            static BACKREF: std::sync::LazyLock<regex::Regex> =
                                std::sync::LazyLock::new(|| {
                                    regex::Regex::new(r"\\[1-9][0-9]*").unwrap()
                                });
                            BACKREF.replace_all(pattern, "(?:)")
                        } else {
                            std::borrow::Cow::Borrowed(pattern)
                        };
                        ferroni::api::Regex::new(&pattern)
                            .map_err(|e| format!("Invalid TextMate {key} pattern: {e}"))?;
                    }
                }
                for child in object.values() {
                    visit(child, &repositories, registry)?;
                }
            }
            Value::Array(values) => {
                for child in values {
                    visit(child, repositories, registry)?;
                }
            }
            _ => (),
        }
        Ok(())
    }
    visit(&source.grammar, &[], registry)
}
