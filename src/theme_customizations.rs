//! Merge color settings without losing nested scopes, then select overrides for
//! the current display name. Invalid values never replace a working theme color.
use std::collections::BTreeMap;

use serde_json::{Map, Value};

pub(crate) fn merge(
    base: &BTreeMap<String, Value>,
    overrides: &BTreeMap<String, Value>,
) -> BTreeMap<String, Value> {
    let mut merged = base.clone();
    for (key, value) in overrides {
        if key.starts_with('[')
            && let (Some(Value::Object(existing)), Value::Object(overrides)) =
                (merged.get_mut(key), value)
        {
            existing.extend(overrides.clone());
        } else {
            merged.insert(key.clone(), value.clone());
        }
    }
    merged
}

fn matches_scope(key: &str, name: &str) -> bool {
    let mut rest = key;
    let mut matched = false;
    while let Some(scope) = rest.strip_prefix('[') {
        let Some((pattern, tail)) = scope.split_once(']') else {
            return false;
        };
        if pattern.is_empty() || pattern.contains('[') {
            return false;
        }
        let prefix = pattern.starts_with('*');
        let suffix = pattern.ends_with('*');
        let literal = pattern.trim_matches('*');
        // VS Code supports wildcards at either end, not arbitrary glob syntax.
        if literal.contains('*') {
            return false;
        }
        matched |= match (prefix, suffix) {
            (true, true) => name.contains(literal),
            (true, false) => name.ends_with(literal),
            (false, true) => name.starts_with(literal),
            (false, false) => name == literal,
        };
        rest = tail;
    }
    matched && rest.is_empty()
}

pub(crate) fn resolve(
    original: &Map<String, Value>,
    settings: &BTreeMap<String, Value>,
    name: &str,
) -> Map<String, Value> {
    let mut result = original.clone();
    let mut apply = |id: &str, value: &Value| {
        let Some(color) = value.as_str() else {
            return;
        };
        if color == "default" {
            // Reset the authored color to its registered light/dark/HC fallback.
            result.remove(id);
        } else if crate::vscode_theme::parse_syntax_color(color).is_some() {
            result.insert(id.to_string(), value.clone());
        }
    };
    for (key, value) in settings {
        if !key.starts_with('[') {
            apply(key, value);
        }
    }
    // Deterministic order follows our JSON/BTree settings order, as with other
    // settings. Every matching block contributes; unrelated colors are retained.
    for (key, value) in settings {
        if matches_scope(key, name)
            && let Value::Object(colors) = value
        {
            for (id, value) in colors {
                apply(id, value);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_workspace_overrides_keep_user_colors_and_global_colors() {
        let user = serde_json::from_value(serde_json::json!({
            "editor.background": "#123", "[Dark+]": {
                "toolbar.hoverBackground": "#456", "tab.activeForeground": "#789"
            }
        }))
        .unwrap();
        let workspace = serde_json::from_value(serde_json::json!({
            "[Dark+]": { "tab.activeForeground": "#abc" }
        }))
        .unwrap();
        let merged = merge(&user, &workspace);
        let resolved = resolve(&Map::new(), &merged, "Dark+");
        assert_eq!(resolved["editor.background"], "#123");
        assert_eq!(resolved["toolbar.hoverBackground"], "#456");
        assert_eq!(resolved["tab.activeForeground"], "#abc");
    }

    #[test]
    fn combined_and_wildcard_scopes_apply_only_to_matching_themes() {
        for (key, names) in [
            ("[Dark+][Monokai]", vec!["Dark+", "Monokai"]),
            ("[GitHub*]", vec!["GitHub Light", "GitHub Dark"]),
            ("[*Light]", vec!["GitHub Light"]),
            ("[*Dark*]", vec!["GitHub Dark Dimmed", "Dark+"]),
            ("[*]", vec!["Anything"]),
        ] {
            for name in names {
                assert!(matches_scope(key, name), "{key} / {name}");
            }
        }
        for key in [
            "[Light]",
            "[Dark*]trailing",
            "[Dark",
            "[]",
            "[Da*rk]",
            "[Dark+][",
        ] {
            assert!(!matches_scope(key, "Dark+"), "{key}");
        }
    }

    #[test]
    fn invalid_customizations_do_not_destroy_colors_and_default_removes_authored_value() {
        let original = serde_json::json!({
            "editor.background": "#123456", "button.background": "#456789",
            "tab.border": "#aabbcc", "toolbar.hoverBackground": "#654321"
        });
        let settings = serde_json::from_value(serde_json::json!({
            "editor.background": "invalid", "button.background": null,
            "tab.border": "#0000", "toolbar.hoverBackground": "default"
        }))
        .unwrap();
        let colors = resolve(original.as_object().unwrap(), &settings, "Dark+");
        assert_eq!(colors["editor.background"], "#123456");
        assert_eq!(colors["button.background"], "#456789");
        assert_eq!(colors["tab.border"], "#0000");
        assert!(!colors.contains_key("toolbar.hoverBackground"));
    }
}
