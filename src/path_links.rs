//! Path and diagnostic locations shared by terminal output and editor text.
use std::{ops::Range, sync::OnceLock};

const MAX_LINKS: usize = 64;
const MAX_PATH_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Location {
    pub path: String,
    // Editor coordinates are zero based; terminal diagnostics are one based.
    pub line: Option<u32>,
    pub column: u32,
}

pub(crate) struct Parsed {
    pub range: Range<usize>,
    pub location: Location,
}

fn location_suffix(text: &str) -> Option<(u32, u32, usize)> {
    static PATTERN: OnceLock<regex::Regex> = OnceLock::new();
    let pattern = PATTERN.get_or_init(|| {
        regex::Regex::new(concat!(
            r"(?i)^\s*(?:,\s*)?(?:",
            r"line\s+(?P<line>[0-9]{1,10})(?:\s*,?\s*(?:column|col)\s+(?P<column>[0-9]{1,10}))?",
            r"|:\s*(?P<colon_line>[0-9]{1,10})(?:\s*:\s*(?P<colon_column>[0-9]{1,10}))?",
            r"|\(\s*(?P<paren_line>[0-9]{1,10})(?:\s*,\s*(?P<paren_column>[0-9]{1,10}))?\s*\))"
        ))
        .unwrap()
    });
    let capture = pattern.captures(text)?;
    let number = |names: &[&str]| {
        names
            .iter()
            .find_map(|name| capture.name(name))
            .and_then(|value| value.as_str().parse::<u32>().ok())?
            .checked_sub(1)
    };
    let line = number(&["line", "colon_line", "paren_line"])?;
    let column = if ["column", "colon_column", "paren_column"]
        .iter()
        .any(|name| capture.name(name).is_some())
    {
        number(&["column", "colon_column", "paren_column"])?
    } else {
        0
    };
    Some((line, column, capture.get(0)?.end()))
}

fn path_shape(path: &str) -> bool {
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || path.contains("://")
        || path.chars().any(char::is_control)
    {
        return false;
    }
    if matches!(path, "." | "..") {
        return true;
    }
    if path.contains(['/', '\\']) {
        return true;
    }
    if matches!(
        path,
        "Makefile" | "Dockerfile" | "CMakeLists.txt" | "BUILD" | "WORKSPACE"
    ) {
        return true;
    }
    path.rsplit_once('.').is_some_and(|(name, extension)| {
        (!name.is_empty() || path.starts_with('.'))
            && extension.len() <= 24
            && extension
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
            && extension
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_+-".contains(&byte))
    })
}

fn parse_candidate(text: &str, path: Range<usize>, range: Range<usize>) -> Option<Parsed> {
    let raw = &text[path.clone()];
    if raw.contains("://") {
        return None;
    }
    // A location can be inside the quotes/token, including Windows drive paths.
    for (offset, ch) in raw.char_indices() {
        if matches!(ch, ':' | '(')
            && let Some((line, column, length)) = location_suffix(&text[path.start + offset..])
            && path_shape(&raw[..offset])
        {
            let end = path.start + offset + length;
            if end < path.end && !text[end..path.end].starts_with([':', ')', ']', '}', ',', ';']) {
                continue;
            }
            return Some(Parsed {
                range: range.start..(path.start + offset + length).max(range.end),
                location: Location {
                    path: raw[..offset].into(),
                    line: Some(line),
                    column,
                },
            });
        }
    }
    if !path_shape(raw) {
        return None;
    }
    let location = location_suffix(&text[range.end..]);
    Some(Parsed {
        range: range.start..range.end + location.map_or(0, |(_, _, length)| length),
        location: Location {
            path: raw.into(),
            line: location.map(|(line, _, _)| line),
            column: location.map_or(0, |(_, column, _)| column),
        },
    })
}

pub(crate) fn parse(text: &str) -> Vec<Parsed> {
    static QUOTED: OnceLock<regex::Regex> = OnceLock::new();
    static TOKENS: OnceLock<regex::Regex> = OnceLock::new();
    let quoted = QUOTED
        .get_or_init(|| regex::Regex::new(r#""([^"\r\n]+)"|'([^'\r\n]+)'|`([^`\r\n]+)`"#).unwrap());
    let tokens = TOKENS.get_or_init(|| regex::Regex::new(r#"[^\s"'`]+"#).unwrap());
    let mut links = Vec::new();
    let mut excluded = Vec::new();
    for capture in quoted.captures_iter(text).take(MAX_LINKS) {
        let whole = capture.get(0).unwrap().range();
        let path = capture.iter().skip(1).flatten().next().unwrap().range();
        if let Some(link) = parse_candidate(text, path, whole.clone()) {
            links.push(link);
        }
        excluded.push(whole);
    }
    let mut quote = 0;
    for token in tokens.find_iter(text) {
        if links.len() == MAX_LINKS {
            break;
        }
        while quote < excluded.len() && excluded[quote].end <= token.start() {
            quote += 1;
        }
        if excluded
            .get(quote)
            .is_some_and(|range| range.start < token.end())
        {
            continue;
        }
        let raw = token.as_str();
        let left = raw.trim_start_matches(['(', '[', '{', '<']);
        let start = token.start() + raw.len() - left.len();
        let value = left.trim_end_matches([':', ',', ';', ']', '}', '>']);
        let mut value = if matches!(value, "." | "..") {
            value
        } else {
            value.trim_end_matches('.')
        };
        // Keep balanced diagnostic parentheses and strip prose punctuation.
        let opens = value.bytes().filter(|&byte| byte == b'(').count();
        let closes = value.bytes().filter(|&byte| byte == b')').count();
        for _ in 0..closes.saturating_sub(opens) {
            let Some(without) = value.strip_suffix(')') else {
                break;
            };
            value = without;
        }
        let end = start + value.len();
        if let Some(link) = parse_candidate(text, start..end, start..end) {
            links.push(link);
        } else {
            let value = value.trim_end_matches(')');
            let end = start + value.len();
            if let Some(link) = parse_candidate(text, start..end, start..end) {
                links.push(link);
            }
        }
    }
    links.sort_unstable_by_key(|link| link.range.start);
    links
}

pub(crate) fn parse_markdown(text: &str) -> Vec<Parsed> {
    use pulldown_cmark::{Event, Parser, Tag};

    let mut paths = parse(text);
    if !text.contains("](") && !text.contains("<file://") {
        return paths;
    }
    let mut inline = Vec::new();
    // Only the hovered line is available here. Inline destinations are
    // self-contained, including spaces in angle-bracket destinations.
    for (event, range) in Parser::new(text).into_offset_iter() {
        let Event::Start(Tag::Link { dest_url, .. }) = event else {
            continue;
        };
        paths.retain(|path| path.range.end <= range.start || range.end <= path.range.start);
        if let Some(location) = markdown_location(&dest_url) {
            inline.push(Parsed { range, location });
        }
    }
    paths.extend(inline);
    paths.sort_unstable_by_key(|path| path.range.start);
    paths
}

fn markdown_location(destination: &str) -> Option<Location> {
    let (path, fragment) = destination.split_once('#').unwrap_or((destination, ""));
    if path.is_empty() || path.len() > MAX_PATH_BYTES || path.starts_with("//") {
        return None;
    }
    let mut location = parse_candidate(path, 0..path.len(), 0..path.len())
        .map(|parsed| parsed.location)
        .unwrap_or_else(|| Location {
            path: path.to_owned(),
            line: None,
            column: 0,
        });
    if !location.path.starts_with("file://") {
        let drive = matches!(location.path.as_bytes(), [letter, b':', b'/' | b'\\', ..]
            if letter.is_ascii_alphabetic());
        if !drive && url::Url::parse(&location.path).is_ok() {
            return None;
        }
        location.path = decode_percent_escapes(&location.path);
    }
    if location.path.chars().any(char::is_control) {
        return None;
    }
    if location.line.is_none() {
        location.line = fragment
            .strip_prefix('L')
            .and_then(|value| value.parse::<u32>().ok())
            .and_then(|line| line.checked_sub(1));
    }
    Some(location)
}

pub(crate) fn decode_percent_escapes(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'%'
            && cursor + 2 < bytes.len()
            && let (Some(a), Some(b)) = (
                char::from(bytes[cursor + 1]).to_digit(16),
                char::from(bytes[cursor + 2]).to_digit(16),
            )
        {
            decoded.push((a * 16 + b) as u8);
            cursor += 3;
        } else {
            decoded.push(bytes[cursor]);
            cursor += 1;
        }
    }
    String::from_utf8(decoded).unwrap_or_else(|_| text.to_owned())
}
