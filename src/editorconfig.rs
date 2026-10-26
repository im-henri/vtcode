//! Lightweight .editorconfig parser and resolver.
//!
//! Walks up the directory tree from a file path, parses .editorconfig files,
//! matches sections against the file name using glob patterns, and returns
//! resolved settings.

use std::path::Path;

/// Resolved editorconfig settings for a file.
#[derive(Debug, Default, Clone)]
pub struct EditorConfigSettings {
    pub indent_style: Option<IndentStyle>,
    pub indent_size: Option<usize>,
    pub tab_width: Option<usize>,
    pub end_of_line: Option<EndOfLine>,
    pub trim_trailing_whitespace: Option<bool>,
    pub insert_final_newline: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IndentStyle {
    Tab,
    Space,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum EndOfLine {
    Lf,
    Crlf,
    Cr,
}

impl EditorConfigSettings {
    /// Merge `other` into self; `other` values take priority where set.
    fn merge_from(&mut self, other: &EditorConfigSettings) {
        if other.indent_style.is_some() { self.indent_style = other.indent_style; }
        if other.indent_size.is_some() { self.indent_size = other.indent_size; }
        if other.tab_width.is_some() { self.tab_width = other.tab_width; }
        if other.end_of_line.is_some() { self.end_of_line = other.end_of_line; }
        if other.trim_trailing_whitespace.is_some() { self.trim_trailing_whitespace = other.trim_trailing_whitespace; }
        if other.insert_final_newline.is_some() { self.insert_final_newline = other.insert_final_newline; }
    }

    /// Get effective tab width (tab_width if set, else indent_size, else None).
    pub(crate) fn effective_tab_width(&self) -> Option<usize> {
        self.tab_width.or(self.indent_size)
    }
}

/// Resolve editorconfig settings for a file by walking up the directory tree.
pub(crate) fn resolve(file_path: &Path) -> EditorConfigSettings {
    let file_path = match file_path.canonicalize() {
        Ok(p) => p,
        Err(_) => file_path.to_path_buf(),
    };
    let file_name = match file_path.file_name().and_then(|n| n.to_str()) {
        Some(n) => n.to_string(),
        None => return EditorConfigSettings::default(),
    };

    let mut configs: Vec<(std::path::PathBuf, String)> = Vec::new();
    let mut dir = file_path.parent();

    // Collect .editorconfig files from closest to farthest
    while let Some(d) = dir {
        let ec_path = d.join(".editorconfig");
        // Opening directly avoids a separate metadata request per ancestor.
        if let Ok(content) = std::fs::read_to_string(&ec_path) {
            let is_root = content.lines().any(|l| {
                let l = l.trim();
                l.eq_ignore_ascii_case("root = true") || l.eq_ignore_ascii_case("root=true")
            });
            configs.push((d.to_path_buf(), content));
            if is_root {
                break;
            }
        }
        dir = d.parent();
    }

    // Process from farthest to closest (so closest settings win)
    configs.reverse();

    let mut result = EditorConfigSettings::default();
    for (_dir, content) in &configs {
        let sections = parse_editorconfig(content);
        for (pattern, settings) in &sections {
            if glob_match(pattern, &file_name) {
                result.merge_from(settings);
            }
        }
    }

    result
}

/// Parse an .editorconfig file into a list of (glob_pattern, settings) sections.
fn parse_editorconfig(content: &str) -> Vec<(String, EditorConfigSettings)> {
    let mut sections: Vec<(String, EditorConfigSettings)> = Vec::new();
    let mut current_pattern: Option<String> = None;
    let mut current_settings = EditorConfigSettings::default();

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }

        // Section header: [pattern]
        if line.starts_with('[') && line.ends_with(']') {
            // Save previous section
            if let Some(pattern) = current_pattern.take() {
                sections.push((pattern, current_settings));
                current_settings = EditorConfigSettings::default();
            }
            current_pattern = Some(line[1..line.len()-1].trim().to_string());
            continue;
        }

        // Key = value
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim().to_lowercase();
            let value = value.trim().to_lowercase();

            if current_pattern.is_none() {
                continue; // Skip root-level properties (like root=true)
            }

            match key.as_str() {
                "indent_style" => {
                    current_settings.indent_style = match value.as_str() {
                        "tab" => Some(IndentStyle::Tab),
                        "space" => Some(IndentStyle::Space),
                        _ => None,
                    };
                }
                "indent_size" => {
                    if value != "tab" {
                        current_settings.indent_size = value.parse().ok();
                    }
                }
                "tab_width" => {
                    current_settings.tab_width = value.parse().ok();
                }
                "end_of_line" => {
                    current_settings.end_of_line = match value.as_str() {
                        "lf" => Some(EndOfLine::Lf),
                        "crlf" => Some(EndOfLine::Crlf),
                        "cr" => Some(EndOfLine::Cr),
                        _ => None,
                    };
                }
                "trim_trailing_whitespace" => {
                    current_settings.trim_trailing_whitespace = match value.as_str() {
                        "true" => Some(true),
                        "false" => Some(false),
                        _ => None,
                    };
                }
                "insert_final_newline" => {
                    current_settings.insert_final_newline = match value.as_str() {
                        "true" => Some(true),
                        "false" => Some(false),
                        _ => None,
                    };
                }
                _ => {} // Ignore unknown properties
            }
        }
    }

    // Save last section
    if let Some(pattern) = current_pattern {
        sections.push((pattern, current_settings));
    }

    sections
}

/// Simple glob pattern matcher supporting *, ?, {a,b}, and [chars].
/// Matches against the filename only (not full path), unless pattern contains /.
fn glob_match(pattern: &str, filename: &str) -> bool {
    // Handle brace expansion: {a,b,c}
    if let Some(start) = pattern.find('{') {
        if let Some(end) = pattern[start..].find('}') {
            let end = start + end;
            let prefix = &pattern[..start];
            let suffix = &pattern[end+1..];
            let alternatives = &pattern[start+1..end];
            return alternatives.split(',').any(|alt| {
                let expanded = format!("{prefix}{alt}{suffix}");
                glob_match(&expanded, filename)
            });
        }
    }

    glob_match_simple(pattern.as_bytes(), filename.as_bytes())
}

fn glob_match_simple(pattern: &[u8], text: &[u8]) -> bool {
    let mut pi = 0;
    let mut ti = 0;
    let mut star_pattern_idx = usize::MAX;
    let mut star_text_idx = 0;

    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == b'?' || pattern[pi].eq_ignore_ascii_case(&text[ti])) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == b'*' {
            star_pattern_idx = pi;
            star_text_idx = ti;
            pi += 1;
        } else if pi < pattern.len() && pattern[pi] == b'[' {
            // Character class [abc] or [a-z]
            if let Some((matched, next_pi)) = match_char_class(&pattern[pi..], text[ti]) {
                if matched {
                    pi += next_pi;
                    ti += 1;
                    continue;
                }
            }
            if star_pattern_idx != usize::MAX {
                pi = star_pattern_idx + 1;
                star_text_idx += 1;
                ti = star_text_idx;
            } else {
                return false;
            }
        } else if star_pattern_idx != usize::MAX {
            pi = star_pattern_idx + 1;
            star_text_idx += 1;
            ti = star_text_idx;
        } else {
            return false;
        }
    }

    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }

    pi == pattern.len()
}

/// Match a character class [abc] or [a-z]. Returns (matched, bytes_consumed).
fn match_char_class(pattern: &[u8], ch: u8) -> Option<(bool, usize)> {
    if pattern.is_empty() || pattern[0] != b'[' {
        return None;
    }
    let mut i = 1;
    let negated = i < pattern.len() && pattern[i] == b'!';
    if negated { i += 1; }

    let mut matched = false;
    while i < pattern.len() && pattern[i] != b']' {
        if i + 2 < pattern.len() && pattern[i + 1] == b'-' {
            // Range: a-z
            let lo = pattern[i];
            let hi = pattern[i + 2];
            if ch >= lo && ch <= hi {
                matched = true;
            }
            i += 3;
        } else {
            if pattern[i] == ch {
                matched = true;
            }
            i += 1;
        }
    }

    if i < pattern.len() && pattern[i] == b']' {
        Some((if negated { !matched } else { matched }, i + 1))
    } else {
        None // Unclosed bracket
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glob_star() {
        assert!(glob_match("*", "foo.rs"));
        assert!(glob_match("*.rs", "foo.rs"));
        assert!(!glob_match("*.rs", "foo.py"));
        assert!(glob_match("*.{rs,py}", "foo.py"));
        assert!(glob_match("*.{rs,py}", "foo.rs"));
        assert!(!glob_match("*.{rs,py}", "foo.js"));
    }

    #[test]
    fn test_glob_question() {
        assert!(glob_match("?.rs", "a.rs"));
        assert!(!glob_match("?.rs", "ab.rs"));
    }

    #[test]
    fn test_glob_brackets() {
        assert!(glob_match("[abc].rs", "a.rs"));
        assert!(!glob_match("[abc].rs", "d.rs"));
        assert!(glob_match("[a-z].rs", "m.rs"));
        assert!(!glob_match("[a-z].rs", "1.rs"));
    }

    #[test]
    fn test_parse_editorconfig() {
        let content = r#"
root = true

[*]
indent_style = space
indent_size = 4
end_of_line = lf
trim_trailing_whitespace = true
insert_final_newline = true

[*.md]
trim_trailing_whitespace = false

[Makefile]
indent_style = tab
"#;
        let sections = parse_editorconfig(content);
        assert_eq!(sections.len(), 3);

        // [*] section
        assert_eq!(sections[0].0, "*");
        assert_eq!(sections[0].1.indent_style, Some(IndentStyle::Space));
        assert_eq!(sections[0].1.indent_size, Some(4));
        assert_eq!(sections[0].1.end_of_line, Some(EndOfLine::Lf));
        assert_eq!(sections[0].1.trim_trailing_whitespace, Some(true));
        assert_eq!(sections[0].1.insert_final_newline, Some(true));

        // [*.md] section
        assert_eq!(sections[1].0, "*.md");
        assert_eq!(sections[1].1.trim_trailing_whitespace, Some(false));

        // [Makefile] section
        assert_eq!(sections[2].0, "Makefile");
        assert_eq!(sections[2].1.indent_style, Some(IndentStyle::Tab));
    }

    #[test]
    fn test_merge_settings() {
        let content = r#"
[*]
indent_style = space
indent_size = 4
trim_trailing_whitespace = true

[*.md]
indent_size = 2
trim_trailing_whitespace = false
"#;
        let sections = parse_editorconfig(content);
        let mut result = EditorConfigSettings::default();
        for (pattern, settings) in &sections {
            if glob_match(pattern, "readme.md") {
                result.merge_from(settings);
            }
        }
        assert_eq!(result.indent_style, Some(IndentStyle::Space)); // from [*]
        assert_eq!(result.indent_size, Some(2)); // overridden by [*.md]
        assert_eq!(result.trim_trailing_whitespace, Some(false)); // overridden by [*.md]
    }

    #[test]
    fn test_brace_expansion() {
        assert!(glob_match("*.{js,ts,jsx,tsx}", "app.tsx"));
        assert!(glob_match("*.{js,ts,jsx,tsx}", "index.js"));
        assert!(!glob_match("*.{js,ts,jsx,tsx}", "style.css"));
    }
}
