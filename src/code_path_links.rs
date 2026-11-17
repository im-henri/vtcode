//! Literal file references in source directives, with byte ranges for editor hit testing.
use regex::Regex;
use std::{ops::Range, sync::OnceLock};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Syntax {
    CFamily,
    Rust,
    JavaScript,
    TypeScript,
    Shell,
    CMake,
}

impl Syntax {
    pub(crate) fn for_editor(editor: &crate::editor::Editor) -> Option<Self> {
        let language = crate::file_associations::language_id(editor.language_name());
        Some(match language.as_str() {
            "c" | "cpp" | "cheader" | "c++header" | "c/c++header" | "objective-c"
            | "objective-cpp" | "objc" | "glsl" | "hlsl" => Self::CFamily,
            "rust" | "rs" => Self::Rust,
            "javascript" | "javascriptreact" | "javascriptjsx" | "js" | "jsx" => Self::JavaScript,
            "typescript" | "typescriptreact" | "typescriptjsx" | "ts" | "tsx" => Self::TypeScript,
            "shellscript" | "sh" | "zsh" => Self::Shell,
            "cmake" => Self::CMake,
            "plaintext" if editor.syntax_language_token().is_none() => {
                let suffix = editor
                    .syntax_file_path()?
                    .extension()?
                    .to_str()?
                    .to_ascii_lowercase();
                match suffix.as_str() {
                    "c" | "h" | "cpp" | "hpp" | "cxx" | "hxx" | "cc" | "hh" | "m" | "mm"
                    | "ixx" | "cppm" | "ccm" | "cxxm" | "glsl" | "hlsl" => Self::CFamily,
                    _ => return None,
                }
            }
            _ => return None,
        })
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Search {
    Include { angled: bool },
    Relative,
    Module { typescript: bool },
    Shell,
    CMake,
}

pub(crate) struct Reference {
    pub range: Range<usize>,
    pub path: String,
    pub search: Search,
}

pub(crate) fn parse(text: &str, syntax: Syntax) -> Vec<Reference> {
    let mut references = Vec::new();
    match syntax {
        Syntax::CFamily => references.extend(include(text)),
        Syntax::Rust => {
            static PREFIX: OnceLock<Regex> = OnceLock::new();
            let regex = PREFIX.get_or_init(|| outside_literals(
                r"\binclude(?:_str|_bytes)?[ \t]*![ \t]*\([ \t]*|\#[ \t]*\[[ \t]*path[ \t]*=[ \t]*",
            ));
            for prefix in regex
                .captures_iter(text)
                .filter_map(|capture| capture.name("prefix"))
            {
                let operand = &text[prefix.end()..];
                let literal = if operand.starts_with('r') {
                    rust_raw(operand)
                } else {
                    quoted(operand, "\"")
                };
                if let Some((range, path)) = literal {
                    references.extend(reference(prefix.end(), range, path, Search::Relative));
                }
            }
        }
        Syntax::JavaScript | Syntax::TypeScript => {
            static PREFIX: OnceLock<Regex> = OnceLock::new();
            let regex = PREFIX.get_or_init(|| outside_literals(concat!(
                r"\b(?:import|require)[ \t]*\([ \t]*|",
                r#"^[ \t]*(?:import\b(?:[^;"'`\r\n]*\bfrom\b)?|export\b[^;"'`\r\n]*\bfrom\b|\}[ \t]*from\b)[ \t]*"#,
            )));
            for prefix in regex
                .captures_iter(text)
                .filter_map(|capture| capture.name("prefix"))
            {
                if text[..prefix.start()].ends_with('.') {
                    continue;
                }
                let operand = &text[prefix.end()..];
                if let Some((range, path)) = quoted(operand, "\"'`")
                    && (explicit_relative(&path)
                        || path.starts_with(['/', '\\'])
                        || drive_path(&path)
                        || path.starts_with("file://"))
                    && !(operand.starts_with('`') && path.contains("${"))
                {
                    if prefix.as_str().trim_end().ends_with('(') {
                        let rest = operand[range.end + 1..].trim_start();
                        if !rest.is_empty()
                            && !rest.starts_with([')', ','])
                            && !rest.starts_with("/*")
                            && !rest.starts_with("//")
                        {
                            continue;
                        }
                    }
                    references.extend(reference(
                        prefix.end(),
                        range,
                        path,
                        Search::Module {
                            typescript: syntax == Syntax::TypeScript,
                        },
                    ));
                }
            }
        }
        Syntax::Shell => references.extend(shell_source(text)),
        Syntax::CMake => references.extend(cmake_include(text)),
    }
    references
}

pub(crate) fn explicit_relative(path: &str) -> bool {
    matches!(path, "." | "..")
        || ["./", "../", ".\\", "..\\"]
            .iter()
            .any(|&prefix| path.starts_with(prefix))
}

fn drive_path(path: &str) -> bool {
    matches!(path.as_bytes(), [letter, b':', b'/' | b'\\', ..] if letter.is_ascii_alphabetic())
}

fn include(text: &str) -> Option<Reference> {
    static PREFIX: OnceLock<Regex> = OnceLock::new();
    let prefix = PREFIX
        .get_or_init(|| {
            Regex::new(concat!(
                r"^(?:[ \t]|/\*[^\r\n]*?\*/)*#(?:[ \t]|/\*[^\r\n]*?\*/)*",
                r"(?:include|import)\b(?:[ \t]|/\*[^\r\n]*?\*/)*",
                r"|^[ \t]*(?:export[ \t]+)?import\b[ \t]*",
            ))
            .unwrap()
        })
        .find(text)?;
    let operand = &text[prefix.end()..];
    let closing = match operand.as_bytes().first()? {
        b'<' => '>',
        b'"' => '"',
        _ => return None,
    };
    // Header names preserve backslashes, including native Windows paths.
    let end = 1 + operand[1..].find(closing)?;
    reference(
        prefix.end(),
        1..end,
        operand[1..end].to_owned(),
        Search::Include {
            angled: closing == '>',
        },
    )
}

fn outside_literals(prefix: &str) -> Regex {
    Regex::new(&format!(
        r#"(?P<prefix>{prefix})|//[^\r\n]*|/\*[^\r\n]*?(?:\*/|$)|"(?:\\.|[^"\\\r\n])*"|'(?:\\.|[^'\\\r\n])*'|`(?:\\.|[^`\\\r\n])*`"#,
    )).unwrap()
}

fn quoted(operand: &str, delimiters: &str) -> Option<(Range<usize>, String)> {
    let mut chars = operand.char_indices();
    let (_, quote) = chars.next()?;
    if !delimiters.contains(quote) {
        return None;
    }
    let mut path = String::new();
    while let Some((offset, ch)) = chars.next() {
        if ch == quote {
            return Some((1..offset, path));
        }
        if ch == '\\' {
            let (_, escaped) = chars.next()?;
            if !matches!(escaped, '\\' | '/' | '\'' | '"' | '`' | ' ') {
                return None;
            }
            path.push(escaped);
        } else {
            path.push(ch);
        }
    }
    None
}

fn rust_raw(operand: &str) -> Option<(Range<usize>, String)> {
    let delimiter = operand.strip_prefix('r')?;
    let hashes = delimiter.bytes().take_while(|&byte| byte == b'#').count();
    let body = delimiter.get(hashes..)?.strip_prefix('"')?;
    let start = hashes + 2;
    let (end, _) = body
        .match_indices('"')
        .find(|(offset, _)| body[*offset + 1..].starts_with(&delimiter[..hashes]))?;
    Some((start..start + end, body[..end].to_owned()))
}

fn shell_source(text: &str) -> Option<Reference> {
    static PREFIX: OnceLock<Regex> = OnceLock::new();
    static WORD: OnceLock<Regex> = OnceLock::new();
    let prefix = PREFIX
        .get_or_init(|| Regex::new(r"^[ \t]*(?:source|\.)[ \t]+").unwrap())
        .find(text)?;
    let word = WORD
        .get_or_init(|| {
            Regex::new(r#"^(?:'[^'\r\n]*'|"(?:\\.|[^"\\\r\n])*"|\\.|[^\s;|&<>'"\\])+"#).unwrap()
        })
        .find(&text[prefix.end()..])?;
    let raw = word.as_str();
    if let Some(path) = raw
        .strip_prefix('\'')
        .and_then(|raw| raw.strip_suffix('\''))
        && !path.contains('\'')
    {
        return reference(
            prefix.end(),
            1..raw.len() - 1,
            path.to_owned(),
            Search::Shell,
        );
    }
    if raw.starts_with(['-', '#', '~']) || raw.contains(['$', '`']) {
        return None;
    }
    let fully_quoted = raw.starts_with('"') && raw.ends_with('"') && raw.matches('"').count() == 2;
    if !fully_quoted && raw.contains(['*', '?', '[']) {
        return None;
    }
    let path = shell_words::split(raw).ok()?.into_iter().next()?;
    let range = if fully_quoted {
        1..raw.len() - 1
    } else {
        word.range()
    };
    reference(prefix.end(), range, path, Search::Shell)
}

fn cmake_include(text: &str) -> Option<Reference> {
    static PREFIX: OnceLock<Regex> = OnceLock::new();
    let prefix = PREFIX
        .get_or_init(|| Regex::new(r"(?i)^[ \t]*include[ \t]*\([ \t]*").unwrap())
        .find(text)?;
    let operand = &text[prefix.end()..];
    let (range, path) = if operand.starts_with('"') {
        quoted(operand, "\"")?
    } else {
        let end = operand
            .find(|ch: char| ch.is_whitespace() || ch == ')')
            .unwrap_or(operand.len());
        (0..end, operand[..end].to_owned())
    };
    let path = match path.strip_prefix("${CMAKE_CURRENT_LIST_DIR}/") {
        Some(relative) => format!("./{relative}"),
        None => path,
    };
    if path.contains(['$', ';', '#']) {
        return None;
    }
    reference(prefix.end(), range, path, Search::CMake)
}

fn reference(
    offset: usize,
    range: Range<usize>,
    path: String,
    search: Search,
) -> Option<Reference> {
    if path.is_empty()
        || path.len() > 4096
        || path.chars().any(char::is_control)
        || (path.contains("://") && !path.starts_with("file://"))
    {
        return None;
    }
    Some(Reference {
        range: offset + range.start..offset + range.end,
        path,
        search,
    })
}
