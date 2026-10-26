//! Preserve VS Code's TextMate scopes while adapting its regex rules to Syntect.
//! No theme-specific aliases or replacement colors are applied here.
use serde_json::{Map, Value, json};
use std::path::Path;
use syntect::parsing::{SyntaxDefinition, SyntaxSetBuilder};

pub(crate) fn add_package(builder: &mut SyntaxSetBuilder, manifest: &Path) -> Result<(), String> {
    let package: Value =
        serde_json::from_str(&std::fs::read_to_string(manifest).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    let Some(grammars) = package
        .pointer("/contributes/grammars")
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    let languages = package
        .pointer("/contributes/languages")
        .and_then(Value::as_array);
    let root = manifest.parent().unwrap();
    for contribution in grammars {
        let relative = contribution["path"].as_str().ok_or("Grammar has no path")?;
        let path = root.join(relative);
        let grammar: Value = serde_json::from_str(
            &std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?,
        )
        .map_err(|e| e.to_string())?;
        let language = languages.and_then(|ls| {
            ls.iter()
                .find(|l| l["id"] == contribution["language"] && !l["id"].is_null())
        });
        let definition =
            convert(&grammar, language).map_err(|e| format!("{}: {e}", path.display()))?;
        builder.add(definition);
    }
    Ok(())
}

fn captures(value: Option<&Value>) -> Result<Value, String> {
    let mut result = Map::new();
    if let Some(captures) = value.and_then(Value::as_object) {
        for (index, capture) in captures {
            if capture.get("patterns").is_some() {
                return Err("Retokenized TextMate captures are not supported".into());
            }
            if let Some(name) = capture.get("name") {
                result.insert(index.clone(), name.clone());
            }
        }
    }
    Ok(Value::Object(result))
}

fn patterns(value: Option<&Value>) -> Result<Vec<Value>, String> {
    let mut result = Vec::new();
    if let Some(rules) = value.and_then(Value::as_array) {
        for rule in rules {
            result.extend(pattern(rule)?);
        }
    }
    Ok(result)
}

fn pattern(rule: &Value) -> Result<Vec<Value>, String> {
    if rule.get("while").is_some() || rule.get("repository").is_some() {
        return Err("TextMate while rules and nested repositories are not supported".into());
    }
    if let Some(include) = rule["include"].as_str() {
        let include = if let Some(local) = include.strip_prefix('#') {
            format!("rule-{local}")
        } else if matches!(include, "$self" | "$base") {
            "main".into()
        } else {
            format!("scope:{include}")
        };
        return Ok(vec![json!({"include": include})]);
    }
    if let Some(regex) = rule.get("match") {
        let mut result =
            json!({"match": regex_source(regex)?, "captures": captures(rule.get("captures"))?});
        if let Some(name) = rule.get("name") {
            result["scope"] = name.clone();
        }
        return Ok(vec![result]);
    }
    if let Some(begin) = rule.get("begin") {
        let mut body = vec![json!({"meta_include_prototype": false})];
        if let Some(name) = rule.get("name") {
            body.push(json!({"meta_scope": name}));
        }
        if let Some(name) = rule.get("contentName") {
            body.push(json!({"meta_content_scope": name}));
        }
        let end = json!({"match": regex_source(&rule.get("end").cloned().unwrap_or(json!("(?!)")))?, "captures": captures(rule.get("endCaptures").or_else(|| rule.get("captures")))?, "pop": true});
        let end_last = rule
            .get("applyEndPatternLast")
            .is_some_and(|v| v == true || v == 1);
        if !end_last {
            body.push(end.clone());
        }
        body.extend(patterns(rule.get("patterns"))?);
        if end_last {
            body.push(end);
        }
        return Ok(vec![
            json!({"match": regex_source(begin)?, "captures": captures(rule.get("beginCaptures").or_else(|| rule.get("captures")))?, "push": body}),
        ]);
    }
    patterns(rule.get("patterns"))
}

fn regex_source(value: &Value) -> Result<String, String> {
    let source = value.as_str().ok_or("TextMate regex must be a string")?;
    // Syntect rewrites $ anchors before compiling. Remove Oniguruma inline
    // comments first so '$' in prose cannot introduce a ')' inside a comment.
    let mut result = String::new();
    let mut chars = source.chars().peekable();
    let mut in_class = false;
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            result.push(ch);
            if let Some(escaped) = chars.next() {
                result.push(escaped);
            }
            continue;
        }
        if ch == '(' && !in_class && chars.clone().take(2).collect::<String>() == "?#" {
            chars.next();
            chars.next();
            for comment in chars.by_ref() {
                if comment == ')' {
                    break;
                }
            }
            continue;
        }
        if ch == '[' {
            in_class = true;
        } else if ch == ']' {
            in_class = false;
        }
        result.push(ch);
    }
    Ok(result)
}

fn convert(grammar: &Value, language: Option<&Value>) -> Result<SyntaxDefinition, String> {
    let yaml = sublime_source(grammar, language)?;
    SyntaxDefinition::load_from_str(&yaml, true, None).map_err(|e| e.to_string())
}

/// Standalone TextMate files carry their own name and fileTypes, without a
/// VS Code package manifest. Persist converted data in our native format.
pub(crate) fn standalone_sublime_source(source: &str) -> Result<String, String> {
    let grammar: Value = serde_json::from_str(source).map_err(|e| e.to_string())?;
    let language = json!({"extensions": grammar["fileTypes"].as_array().cloned().unwrap_or_default()});
    sublime_source(&grammar, Some(&language))
}

fn sublime_source(grammar: &Value, language: Option<&Value>) -> Result<String, String> {
    if grammar.get("injections").is_some() || grammar.get("injectionSelector").is_some() {
        return Err("TextMate grammar injections are not supported".into());
    }
    let scope = grammar["scopeName"]
        .as_str()
        .ok_or("Grammar has no scopeName")?;
    let mut contexts = Map::new();
    contexts.insert("main".into(), json!(patterns(grammar.get("patterns"))?));
    if let Some(repository) = grammar["repository"].as_object() {
        for (name, rule) in repository {
            contexts.insert(format!("rule-{name}"), json!(pattern(rule)?));
        }
    }
    let name = language
        .and_then(|l| l["aliases"][0].as_str())
        .or_else(|| grammar["name"].as_str())
        .unwrap_or(scope);
    let mut extensions: Vec<Value> = language
        .and_then(|l| l["extensions"].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|s| json!(s.trim_start_matches('.')))
        .collect();
    if let Some(files) = language.and_then(|l| l["filenames"].as_array()) {
        extensions.extend(files.clone());
    }
    let mut syntax = json!({"name":name,"scope":scope,"file_extensions":extensions,"hidden":language.is_none(),"contexts":contexts});
    if let Some(first) = language
        .and_then(|l| l.get("firstLine"))
        .or_else(|| grammar.get("firstLineMatch"))
    {
        syntax["first_line_match"] = first.clone();
    }
    let mut yaml = String::new();
    yaml_rust2::YamlEmitter::new(&mut yaml)
        .dump(&to_yaml(&syntax))
        .map_err(|e| e.to_string())?;
    Ok(yaml)
}

// Capture numbers must be YAML integers (JSON object keys are always strings).
fn to_yaml(value: &Value) -> yaml_rust2::Yaml {
    use yaml_rust2::Yaml;
    match value {
        Value::Object(map) => Yaml::Hash(
            map.iter()
                .map(|(k, v)| {
                    (
                        k.parse::<i64>()
                            .map(Yaml::Integer)
                            .unwrap_or_else(|_| Yaml::String(k.clone())),
                        to_yaml(v),
                    )
                })
                .collect(),
        ),
        Value::Array(a) => Yaml::Array(a.iter().map(to_yaml).collect()),
        Value::String(s) => Yaml::String(s.clone()),
        Value::Bool(b) => Yaml::Boolean(*b),
        _ => Yaml::Null,
    }
}
