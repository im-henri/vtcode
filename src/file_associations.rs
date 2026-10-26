//! Shared, deterministic filename matching for syntax and language services.
use rustc_hash::FxHashMap;
use std::path::Path;

pub(crate) type Associations = FxHashMap<String, String>;

pub(crate) fn resolve<'a>(
    associations: &'a Associations,
    path: &Path,
) -> Option<(&'a str, &'a str)> {
    if associations.is_empty() {
        return None;
    }
    let filename = path.file_name()?.to_str()?;
    // Exact filenames always win, independently of hash-map iteration order.
    if let Some((pattern, language)) = associations.get_key_value(filename) {
        return Some((pattern, language));
    }
    associations
        .iter()
        .filter_map(|(pattern, language)| {
            pattern_score(pattern, filename).map(|score| ((score, pattern), language))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .map(|((_, pattern), language)| (pattern.as_str(), language.as_str()))
}

fn pattern_score(pattern: &str, filename: &str) -> Option<(usize, bool)> {
    if let Some(suffix) = pattern.strip_prefix("*.") {
        if let Some(group) = suffix.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            return group
                .split(',')
                .filter_map(|suffix| {
                    suffix_match(filename, suffix.trim()).then_some((suffix.trim().len(), false))
                })
                .max();
        }
        return suffix_match(filename, suffix).then_some((suffix.len(), true));
    }
    if let Some(prefix) = pattern.strip_suffix(".*") {
        return filename
            .strip_prefix(prefix)
            .is_some_and(|rest| rest.starts_with('.'))
            .then_some((prefix.len(), true));
    }
    None
}

fn suffix_match(filename: &str, suffix: &str) -> bool {
    !suffix.is_empty()
        && filename
            .strip_suffix(suffix)
            .is_some_and(|prefix| prefix.ends_with('.'))
}

pub(crate) fn validate_pattern(pattern: &str) -> Result<(), String> {
    let literal = |value: &str| {
        !value.is_empty()
            && !value
                .chars()
                .any(|c| c.is_control() || "*/\\?[]{}".contains(c))
    };
    let valid = if let Some(suffix) = pattern.strip_prefix("*.") {
        if let Some(group) = suffix.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            group.split(',').all(|s| literal(s.trim()))
        } else {
            literal(suffix)
        }
    } else if let Some(prefix) = pattern.strip_suffix(".*") {
        literal(prefix)
    } else {
        literal(pattern)
    };
    if pattern.len() > 100 || !valid {
        return Err("Use a filename or pattern such as *.py, *.d.ts, *.{c,h}, or Dockerfile.* (up to 100 bytes).".into());
    }
    Ok(())
}

/// Grammar display names and common IDs share the same formatter/LSP identity.
pub(crate) fn language_id(language: &str) -> String {
    match language.trim().to_ascii_lowercase().as_str() {
        "c++" | "cpp" => "cpp".into(),
        "c#" | "csharp" => "csharp".into(),
        "plain text" | "text" | "plaintext" => "plaintext".into(),
        "shell script (bash)" | "shellscript" | "bash" => "shellscript".into(),
        "javascript (babel)" => "javascript".into(),
        "objective-c++" => "objective-cpp".into(),
        name => name.replace(' ', ""),
    }
}

pub(crate) fn server_supports(server: &str, language: &str) -> bool {
    server.eq_ignore_ascii_case(language)
        || (matches!(server, "c" | "cpp" | "c++")
            && matches!(language, "c" | "cpp" | "objective-c" | "objective-cpp"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn most_specific_match_wins_and_filenames_are_case_sensitive() {
        let map = [
            ("*.ts", "TypeScript"),
            ("*.d.ts", "JavaScript"),
            ("*.{js,ts}", "Plain Text"),
            ("types.d.ts", "C++"),
            ("Dockerfile.*", "Dockerfile"),
            ("*.prod", "Python"),
        ]
        .map(|(a, b)| (a.into(), b.into()))
        .into_iter()
        .collect();
        for (file, expected) in [
            ("a.ts", "TypeScript"),
            ("a.d.ts", "JavaScript"),
            ("types.d.ts", "C++"),
            ("TYPES.d.ts", "JavaScript"),
            ("Dockerfile.prod", "Dockerfile"),
            ("a.js", "Plain Text"),
        ] {
            assert_eq!(
                resolve(&map, Path::new(file)).unwrap().1,
                expected,
                "{file}"
            );
        }
        for file in ["a.TS", "a.prod.bak", "unrelated"] {
            assert!(resolve(&map, Path::new(file)).is_none());
        }
    }

    #[test]
    fn supported_patterns_validate_and_handle_unicode_and_dotfiles() {
        for pattern in [
            "*.py",
            "*.d.ts",
            "*.{c,h}",
            "Dockerfile.*",
            ".env",
            "项目 file",
        ] {
            assert!(validate_pattern(pattern).is_ok(), "{pattern}");
        }
        for pattern in [
            "",
            "*",
            "*.",
            "src/*.py",
            "*.{}",
            "*.{py,}",
            "file?",
            "bad\nname",
        ] {
            assert!(validate_pattern(pattern).is_err(), "{pattern}");
        }
        let map = [
            (".env".into(), "Shell Script (Bash)".into()),
            ("*.文件".into(), "Python".into()),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            resolve(&map, Path::new(".env")).unwrap().1,
            "Shell Script (Bash)"
        );
        assert_eq!(resolve(&map, Path::new("例.文件")).unwrap().1, "Python");
    }
}
