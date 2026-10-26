// Ported behavior from VS Code's replacePattern.ts and base/common/search.ts.
// Reference revision and MIT attribution: THIRD_PARTY_NOTICES.md.
use crate::search_regex::SearchCaptures;

pub(crate) fn preserve_case(original: &str, replacement: &str) -> String {
    if original.is_empty() {
        return replacement.to_owned();
    }
    let compatible = |ch| {
        original.contains(ch)
            && replacement.contains(ch)
            && original.split(ch).count() == replacement.split(ch).count()
    };
    let hyphens = compatible('-');
    let underscores = compatible('_');
    if hyphens != underscores {
        let separator = if hyphens { '-' } else { '_' };
        return original
            .split(separator)
            .zip(replacement.split(separator))
            .map(|(original, replacement)| preserve_case(original, replacement))
            .collect::<Vec<_>>()
            .join(&separator.to_string());
    }
    if original.to_uppercase() == original {
        return replacement.to_uppercase();
    }
    if original.to_lowercase() == original {
        return replacement.to_lowercase();
    }
    let first = original.chars().next().unwrap();
    let mut chars = replacement.chars();
    let Some(next) = chars.next() else {
        return String::new();
    };
    if first.to_lowercase().to_string() != first.to_string() {
        next.to_uppercase().chain(chars).collect()
    } else if first.to_uppercase().to_string() != first.to_string() {
        next.to_lowercase().chain(chars).collect()
    } else {
        replacement.to_owned()
    }
}

enum Piece {
    Text(String),
    Capture(usize, Vec<char>),
}

pub(crate) struct Replacement {
    pieces: Vec<Piece>,
    dynamic: bool,
}

impl Replacement {
    pub(crate) fn new(source: &str, regex: bool) -> Self {
        if !regex {
            return Self {
                pieces: vec![Piece::Text(source.to_owned())],
                dynamic: false,
            };
        }
        let mut chars = source.chars().peekable();
        let mut pieces = Vec::new();
        let mut text = String::new();
        let mut case_ops = Vec::new();
        while let Some(ch) = chars.next() {
            match ch {
                '\\' => match chars.next() {
                    Some('n') => text.push('\n'),
                    Some('t') => text.push('\t'),
                    Some('\\') => text.push('\\'),
                    Some(op @ ('u' | 'U' | 'l' | 'L')) => case_ops.push(op),
                    Some(other) => {
                        text.push('\\');
                        text.push(other);
                    }
                    None => text.push('\\'),
                },
                '$' => match chars.next() {
                    Some('$') => text.push('$'),
                    Some(next @ ('0'..='9' | '&')) => {
                        let mut index = next.to_digit(10).unwrap_or(0) as usize;
                        if index > 0 && chars.peek().is_some_and(char::is_ascii_digit) {
                            index =
                                index * 10 + chars.next().unwrap().to_digit(10).unwrap() as usize;
                        }
                        if !text.is_empty() {
                            pieces.push(Piece::Text(std::mem::take(&mut text)));
                        }
                        pieces.push(Piece::Capture(index, std::mem::take(&mut case_ops)));
                    }
                    Some(other) => {
                        text.push('$');
                        text.push(other);
                    }
                    None => text.push('$'),
                },
                other => text.push(other),
            }
        }
        if !text.is_empty() {
            pieces.push(Piece::Text(text));
        }
        let dynamic = pieces
            .iter()
            .any(|piece| matches!(piece, Piece::Capture(_, _)));
        Self { pieces, dynamic }
    }

    pub(crate) fn expand(&self, text: &str, captures: &SearchCaptures, preserve: bool) -> String {
        let mut result = String::new();
        for piece in &self.pieces {
            match piece {
                Piece::Text(text) => result.push_str(text),
                Piece::Capture(index, operations) => {
                    let mut index = *index;
                    let mut remainder = String::new();
                    while index >= captures.groups.len() && index > 0 {
                        remainder.insert(0, char::from(b'0' + (index % 10) as u8));
                        index /= 10;
                    }
                    let value = if index == 0 && !remainder.is_empty() {
                        format!("${remainder}")
                    } else {
                        format!(
                            "{}{remainder}",
                            captures.groups[index]
                                .as_ref()
                                .map_or("", |range| &text[range.clone()])
                        )
                    };
                    result.push_str(&apply_case_ops(&value, operations));
                }
            }
        }
        if preserve && !self.dynamic {
            preserve_case(&text[captures.range.clone()], &result)
        } else {
            result
        }
    }
}

fn apply_case_ops(text: &str, operations: &[char]) -> String {
    let mut result = Vec::new();
    let mut op = 0;
    // VS Code counts replacement case operations in UTF-16 code units.
    for unit in text.encode_utf16() {
        match (char::from_u32(u32::from(unit)), operations.get(op)) {
            (Some(ch), Some('u' | 'U')) => {
                result.extend(ch.to_uppercase().collect::<String>().encode_utf16())
            }
            (Some(ch), Some('l' | 'L')) => {
                result.extend(ch.to_lowercase().collect::<String>().encode_utf16())
            }
            _ => result.push(unit),
        }
        if matches!(operations.get(op), Some('u' | 'l')) {
            op += 1;
        }
    }
    String::from_utf16_lossy(&result)
}

/// Workspace replace uses services/search/common/replace.ts. Its parameter
/// detection and preserve-case order differ from the editor's parser.
pub(crate) fn workspace_pattern(source: &str) -> (String, bool) {
    let mut parsed = String::new();
    let mut chars = source.chars().peekable();
    let mut parameters = false;
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => parsed.push('\n'),
                Some('t') => parsed.push('\t'),
                Some('\\') => parsed.push('\\'),
                Some(other) => {
                    parsed.push('\\');
                    parsed.push(other);
                }
                None => parsed.push('\\'),
            }
        } else if ch == '$' {
            parsed.push('$');
            if let Some(next) = chars.next() {
                if next == '0' {
                    parsed.push('&');
                    parameters = true;
                } else {
                    parsed.push(next);
                    if matches!(next, '`' | '\'') {
                        parameters = true;
                    } else if matches!(next, '1'..='9') {
                        let extra_digits = chars.clone().take_while(char::is_ascii_digit).count();
                        parameters |= extra_digits < 2;
                    }
                }
            }
        } else {
            parsed.push(ch);
        }
    }
    (parsed, parameters)
}

#[cfg(test)]
pub(crate) fn expand_workspace(
    source: &str,
    text: &str,
    captures: &SearchCaptures,
    preserve: bool,
) -> String {
    WorkspaceReplacement::new(source).expand(text, captures, preserve)
}

/// Parse the workspace template once per operation, not once per match.
pub(crate) struct WorkspaceReplacement {
    parsed: String,
    parameters: bool,
}
impl WorkspaceReplacement {
    pub(crate) fn new(source: &str) -> Self {
        let (parsed, parameters) = workspace_pattern(source);
        Self { parsed, parameters }
    }
    pub(crate) fn fallback(&self) -> &str {
        &self.parsed
    }
    pub(crate) fn needs_captures(&self) -> bool {
        self.parameters
    }
    pub(crate) fn expand(&self, text: &str, captures: &SearchCaptures, preserve: bool) -> String {
        let mut parsed = if preserve {
            preserve_case(&text[captures.range.clone()], &self.parsed)
        } else {
            self.parsed.clone()
        };
        let parameters = self.parameters;
        if !parameters {
            return parsed;
        }
        static CASE_OPS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        let case_ops = CASE_OPS.get_or_init(|| {
            regex::Regex::new(r"([\s\S]*?)((?:\\[uUlL])+?|)(\$[0-9]+)([\s\S]*?)").unwrap()
        });
        if parsed.contains(['u', 'U', 'l', 'L']) && parsed.contains('\\') {
            parsed = case_ops
                .replace_all(&parsed, |ops: &regex::Captures<'_>| {
                    let index = ops[3][1..].parse::<usize>().unwrap_or(usize::MAX);
                    let group = captures
                        .groups
                        .get(index)
                        .and_then(|range| range.as_ref())
                        .map(|range| &text[range.clone()]);
                    if ops[2].is_empty() || group.is_none_or(str::is_empty) {
                        return ops[0].to_owned();
                    }
                    let group = group.unwrap();
                    let operations = ops[2].chars().filter(|&ch| ch != '\\').collect::<Vec<_>>();
                    let units = group.encode_utf16().collect::<Vec<_>>();
                    let mut converted = Vec::new();
                    let mut consumed = 0;
                    for (i, operation) in operations.iter().enumerate() {
                        if i >= units.len() {
                            break;
                        }
                        if matches!(operation, 'U' | 'L') {
                            let rest = String::from_utf16_lossy(&units[i..]);
                            converted.extend(
                                if *operation == 'U' {
                                    rest.to_uppercase()
                                } else {
                                    rest.to_lowercase()
                                }
                                .encode_utf16(),
                            );
                            consumed = units.len();
                            break;
                        }
                        if let Some(ch) = char::from_u32(u32::from(units[i])) {
                            converted.extend(
                                if *operation == 'u' {
                                    ch.to_uppercase().collect::<String>()
                                } else {
                                    ch.to_lowercase().collect::<String>()
                                }
                                .encode_utf16(),
                            );
                        } else {
                            converted.push(units[i]);
                        }
                        consumed = i + 1;
                    }
                    converted.extend_from_slice(&units[consumed..]);
                    format!(
                        "{}{}{}",
                        &ops[1],
                        String::from_utf16_lossy(&converted),
                        &ops[4]
                    )
                })
                .into_owned();
        }
        let mut result = String::new();
        let mut chars = parsed.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch != '$' {
                result.push(ch);
                continue;
            }
            match chars.peek().copied() {
                Some('$') => {
                    chars.next();
                    result.push('$');
                }
                Some('&') => {
                    chars.next();
                    result.push_str(&text[captures.range.clone()]);
                }
                Some('`') => {
                    chars.next();
                    result.push_str(&text[..captures.range.start]);
                }
                Some('\'') => {
                    chars.next();
                    result.push_str(&text[captures.range.end..]);
                }
                Some(first @ '1'..='9') => {
                    let first_index = first.to_digit(10).unwrap() as usize;
                    let mut index = first_index;
                    let second = chars.clone().nth(1).filter(char::is_ascii_digit);
                    if let Some(second) = second {
                        let both = index * 10 + second.to_digit(10).unwrap() as usize;
                        if both < captures.groups.len() {
                            index = both;
                        }
                    }
                    if index < captures.groups.len() {
                        chars.next();
                        if index != first_index {
                            chars.next();
                        }
                        if let Some(range) = &captures.groups[index] {
                            result.push_str(&text[range.clone()]);
                        }
                    } else {
                        result.push('$');
                    }
                }
                Some('<') if !captures.names.is_empty() => {
                    let mut probe = chars.clone();
                    probe.next();
                    let name: String = probe.by_ref().take_while(|&ch| ch != '>').collect();
                    if parsed.contains(&format!("$<{name}>")) {
                        chars.next();
                        for _ in name.chars() {
                            chars.next();
                        }
                        chars.next();
                        if let Some((_, Some(range))) =
                            captures.names.iter().find(|(group, _)| *group == name)
                        {
                            result.push_str(&text[range.clone()]);
                        }
                    } else {
                        result.push('$');
                    }
                }
                _ => result.push('$'),
            }
        }
        result
    }
}
