//! Vim regular expressions on top of the `regex` crate, plus buffer search
//! and `:substitute` replacement expansion.
//!
//! Supported: magic levels (`\v` `\m` `\M` `\V`), `\(\)` `\%(\)` `\|` `\+`
//! `\=` `\?` `\{n,m}` `\{-}`, `\<` `\>`, `\zs` `\ze`, `\c` `\C`, character
//! classes (`\s \S \d \D \w \W \a \A \l \L \u \U \x \X \o \O \h \H \k \i
//! \f \p`), `\_x` multiline classes, `\_.`, `\n`, `\t`, `\e`, `\%^`, `\%$`,
//! `~` (last substitute string). Back-references and look-around are not
//! available in the regex crate and report an error.

use regex::Regex;
use ropey::Rope;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Magic {
    Very,
    Magic,
    No,
    VeryNo,
}

#[derive(Clone, Debug)]
pub(crate) struct VimRegex {
    pub(crate) re: Regex,
    /// The pattern used `\zs`/`\ze`; the match is capture group `vzs`.
    pub(crate) zs: bool,
    pub(crate) source: String,
}

pub(crate) struct Options<'a> {
    pub(crate) ignorecase: bool,
    pub(crate) smartcase: bool,
    pub(crate) magic: bool,
    pub(crate) last_sub: Option<&'a str>,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Self {
            ignorecase: false,
            smartcase: false,
            magic: true,
            last_sub: None,
        }
    }
}

fn push_lit(out: &mut String, c: char) {
    if regex_syntax_meta(c) {
        out.push('\\');
    }
    out.push(c);
}

fn regex_syntax_meta(c: char) -> bool {
    matches!(
        c,
        '\\' | '.'
            | '+'
            | '*'
            | '?'
            | '('
            | ')'
            | '|'
            | '['
            | ']'
            | '{'
            | '}'
            | '^'
            | '$'
            | '#'
            | '&'
            | '-'
            | '~'
    )
}

fn class(c: char) -> Option<&'static str> {
    Some(match c {
        's' => r"[ \t]",
        'S' => r"[^ \t\r\n]",
        'd' => r"[0-9]",
        'D' => r"[^0-9\r\n]",
        'w' => r"[0-9A-Za-z_]",
        'W' => r"[^0-9A-Za-z_\r\n]",
        'a' => r"[A-Za-z]",
        'A' => r"[^A-Za-z\r\n]",
        'l' => r"[a-z]",
        'L' => r"[^a-z\r\n]",
        'u' => r"[A-Z]",
        'U' => r"[^A-Z\r\n]",
        'x' => r"[0-9A-Fa-f]",
        'X' => r"[^0-9A-Fa-f\r\n]",
        'o' => r"[0-7]",
        'O' => r"[^0-7\r\n]",
        'h' => r"[A-Za-z_]",
        'H' => r"[^A-Za-z_\r\n]",
        'k' | 'i' => r"[\w]",
        'K' | 'I' => r"[\w&&[^0-9]]",
        'f' | 'F' => r"[^\s]",
        'p' | 'P' => r"[^\x00-\x1f\n]",
        _ => return None,
    })
}

/// Translate a Vim pattern. Returns the Rust pattern, whether `\zs`/`\ze`
/// were used, and an explicit case override.
fn translate(pat: &str, opts: &Options<'_>) -> Result<(String, bool, Option<bool>), String> {
    let chars: Vec<char> = pat.chars().collect();
    let mut magic = if opts.magic { Magic::Magic } else { Magic::No };
    let mut out = String::new();
    let mut i = 0;
    let mut case: Option<bool> = None;
    let mut zs_at: Option<usize> = None;
    let mut ze_at: Option<usize> = None;
    let mut depth = 0i32;
    // Is the next atom at the start of a branch (for `^` and `*`)?
    let mut at_start = true;
    while i < chars.len() {
        let c = chars[i];
        let start_before = at_start;
        at_start = false;
        if c == '\\' {
            i += 1;
            let Some(&n) = chars.get(i) else {
                out.push_str(r"\\");
                break;
            };
            i += 1;
            match n {
                'v' => {
                    magic = Magic::Very;
                    at_start = start_before;
                    continue;
                }
                'm' => {
                    magic = Magic::Magic;
                    at_start = start_before;
                    continue;
                }
                'M' => {
                    magic = Magic::No;
                    at_start = start_before;
                    continue;
                }
                'V' => {
                    magic = Magic::VeryNo;
                    at_start = start_before;
                    continue;
                }
                'c' => {
                    case = Some(false);
                    at_start = start_before;
                    continue;
                }
                'C' => {
                    case = Some(true);
                    at_start = start_before;
                    continue;
                }
                // A line break: lines in a CRLF buffer end in "\r\n". A `^`
                // after it is the start of the next line, as in Vim.
                'n' => {
                    out.push_str(r"(?:\r?\n)");
                    at_start = true;
                }
                't' => out.push_str(r"\t"),
                'e' => out.push_str(r"\x1b"),
                'r' => out.push_str(r"\r"),
                '\\' => out.push_str(r"\\"),
                '/' => out.push('/'),
                'z' => match chars.get(i) {
                    Some('s') => {
                        zs_at = Some(out.len());
                        i += 1;
                    }
                    Some('e') => {
                        ze_at = Some(out.len());
                        i += 1;
                    }
                    _ => return Err("E68: Invalid character after \\z".into()),
                },
                '%' => match chars.get(i) {
                    Some('^') => {
                        out.push_str(r"\A");
                        i += 1;
                    }
                    Some('$') => {
                        out.push_str(r"\z");
                        i += 1;
                    }
                    Some('(') => {
                        out.push_str("(?:");
                        depth += 1;
                        i += 1;
                        at_start = true;
                    }
                    Some('[') => return Err("E70: \\%[ is not supported".into()),
                    Some('V' | '#') => {
                        i += 1;
                    }
                    _ => return Err("E71: Invalid character after \\%".into()),
                },
                '_' => {
                    let Some(&k) = chars.get(i) else {
                        return Err("E63: Invalid use of \\_".into());
                    };
                    i += 1;
                    match k {
                        '.' => out.push_str(r"(?:\r\n|(?s:.))"),
                        '^' => out.push('^'),
                        '$' => out.push('$'),
                        's' => out.push_str(r"[ \t\r\n]"),
                        '[' => {
                            let (cls, used) = bracket(&chars[i..], true)?;
                            out.push_str(&cls);
                            i += used;
                        }
                        k => match class(k) {
                            Some(cls) => {
                                let inner = &cls[1..cls.len() - 1];
                                if let Some(neg) = inner.strip_prefix('^') {
                                    out.push_str(&format!("[^{}]", neg.trim_end_matches(r"\r\n")));
                                } else {
                                    out.push_str(&format!("[{inner}\\r\\n]"));
                                }
                            }
                            None => return Err("E63: Invalid use of \\_".into()),
                        },
                    }
                }
                '1'..='9' => return Err("E65: Back-references are not supported".into()),
                '@' => return Err("E64: Look-around is not supported".into()),
                n => {
                    // Characters whose meaning flips with the backslash.
                    let special_with_bs = match magic {
                        Magic::Very => false,
                        Magic::Magic => {
                            matches!(n, '(' | ')' | '|' | '+' | '=' | '?' | '{' | '<' | '>')
                        }
                        Magic::No => matches!(
                            n,
                            '(' | ')'
                                | '|'
                                | '+'
                                | '='
                                | '?'
                                | '{'
                                | '<'
                                | '>'
                                | '.'
                                | '*'
                                | '['
                                | '~'
                        ),
                        Magic::VeryNo => matches!(
                            n,
                            '(' | ')'
                                | '|'
                                | '+'
                                | '='
                                | '?'
                                | '{'
                                | '<'
                                | '>'
                                | '.'
                                | '*'
                                | '['
                                | '~'
                                | '^'
                                | '$'
                        ),
                    };
                    if special_with_bs {
                        i -= 1;
                        let (s, adv, st) = special(&chars, i, &mut depth, opts)?;
                        out.push_str(&s);
                        i += adv;
                        at_start = st;
                    } else if let Some(cls) = class(n) {
                        out.push_str(cls);
                    } else {
                        push_lit(&mut out, n);
                    }
                }
            }
            continue;
        }
        let special_plain = match magic {
            Magic::Very => matches!(
                c,
                '(' | ')'
                    | '|'
                    | '+'
                    | '='
                    | '?'
                    | '{'
                    | '<'
                    | '>'
                    | '.'
                    | '*'
                    | '['
                    | '~'
                    | '^'
                    | '$'
                    | '@'
                    | '%'
            ),
            Magic::Magic => matches!(c, '.' | '*' | '[' | '~' | '^' | '$'),
            Magic::No => matches!(c, '^' | '$'),
            Magic::VeryNo => false,
        };
        if !special_plain {
            push_lit(&mut out, c);
            i += 1;
            continue;
        }
        match c {
            '^' => {
                if start_before || magic == Magic::Very {
                    out.push('^');
                    at_start = true;
                } else {
                    out.push_str(r"\^");
                }
                i += 1;
            }
            '$' => {
                let at_end = i + 1 == chars.len()
                    || chars[i + 1..].starts_with(&['\\', '|'])
                    || chars[i + 1..].starts_with(&['\\', ')'])
                    || chars[i + 1..].starts_with(&['\\', 'n'])
                    || (magic == Magic::Very && matches!(chars.get(i + 1), Some('|' | ')')));
                if at_end || magic == Magic::Very {
                    out.push('$');
                } else {
                    out.push_str(r"\$");
                }
                i += 1;
            }
            '.' => {
                out.push('.');
                i += 1;
            }
            '*' => {
                if start_before {
                    out.push_str(r"\*");
                } else {
                    out.push('*');
                }
                i += 1;
            }
            '[' => match bracket(&chars[i + 1..], false) {
                Ok((cls, used)) => {
                    out.push_str(&cls);
                    i += 1 + used;
                }
                Err(_) => {
                    out.push_str(r"\[");
                    i += 1;
                }
            },
            '~' => {
                for ch in opts.last_sub.unwrap_or("").chars() {
                    push_lit(&mut out, ch);
                }
                i += 1;
            }
            _ => {
                let (s, adv, st) = special(&chars, i, &mut depth, opts)?;
                out.push_str(&s);
                i += adv;
                at_start = st;
            }
        }
    }
    if depth != 0 {
        return Err("E54: Unmatched \\(".into());
    }
    let zs = zs_at.is_some() || ze_at.is_some();
    if zs {
        let s = zs_at.unwrap_or(0);
        let e = ze_at.unwrap_or(out.len()).max(s);
        let pre = &out[..s];
        let mid = &out[s..e];
        let post = &out[e..];
        out = format!("(?:{pre})(?P<vzs>{mid})(?:{post})");
    }
    Ok((out, zs, case))
}

/// The meaning of a special atom whose first character is at `chars[i]`
/// (possibly preceded by a backslash at `chars[i]`). Returns (rust, consumed, branch_start).
fn special(
    chars: &[char],
    i: usize,
    depth: &mut i32,
    opts: &Options<'_>,
) -> Result<(String, usize, bool), String> {
    let bs = chars[i] == '\\';
    let c = chars[i + usize::from(bs)];
    let mut used = 1 + usize::from(bs);
    let s = match c {
        '(' => {
            *depth += 1;
            return Ok(("(".into(), used, true));
        }
        ')' => {
            *depth -= 1;
            if *depth < 0 {
                return Err("E55: Unmatched \\)".into());
            }
            ")".into()
        }
        '|' => return Ok(("|".into(), used, true)),
        '+' => "+".into(),
        '=' | '?' => "?".into(),
        '<' | '>' => r"\b".into(),
        '.' => ".".into(),
        '*' => "*".into(),
        '~' => {
            let mut s = String::new();
            for ch in opts.last_sub.unwrap_or("").chars() {
                push_lit(&mut s, ch);
            }
            s
        }
        '^' => "^".into(),
        '$' => "$".into(),
        '[' => {
            let (cls, n) = bracket(&chars[i + used..], false)?;
            used += n;
            cls
        }
        '{' => {
            // \{n,m}, \{-n,m}, closing with } or \}.
            let mut j = i + used;
            let mut body = String::new();
            while j < chars.len() && chars[j] != '}' {
                if chars[j] == '\\' && chars.get(j + 1) == Some(&'}') {
                    break;
                }
                body.push(chars[j]);
                j += 1;
            }
            if j >= chars.len() {
                return Err("E60: Missing }".into());
            }
            j += if chars[j] == '\\' { 2 } else { 1 };
            used = j - i;
            let lazy = body.starts_with('-');
            let body = body.trim_start_matches('-');
            let q = if body.is_empty() {
                "*".to_string()
            } else if !body.contains(',') {
                format!("{{{body}}}")
            } else {
                let (a, b) = body.split_once(',').unwrap();
                format!("{{{},{}}}", if a.is_empty() { "0" } else { a }, b)
            };
            if lazy { format!("{q}?") } else { q }
        }
        '@' | '%' => return Err(format!("E64: \\{c} is not supported")),
        other => {
            let mut s = String::new();
            push_lit(&mut s, other);
            s
        }
    };
    Ok((s, used, false))
}

/// Translate a `[...]` collection (after the `[`). Returns (rust, consumed).
fn bracket(chars: &[char], newline: bool) -> Result<(String, usize), String> {
    let mut out = String::from("[");
    let mut i = 0;
    let negated = chars.first() == Some(&'^');
    if negated {
        out.push('^');
        i += 1;
    }
    if chars.get(i) == Some(&']') {
        out.push_str(r"\]");
        i += 1;
    }
    loop {
        let Some(&c) = chars.get(i) else {
            return Err("E769: Missing ]".into());
        };
        i += 1;
        match c {
            ']' => break,
            '[' if chars.get(i) == Some(&':') => {
                let rest: String = chars[i..].iter().collect();
                if let Some(end) = rest.find(":]") {
                    out.push_str("[");
                    out.push_str(&rest[..end + 2]);
                    i += rest[..end + 2].chars().count();
                } else {
                    out.push_str(r"\[");
                }
            }
            '\\' => {
                let Some(&n) = chars.get(i) else {
                    out.push_str(r"\\");
                    continue;
                };
                i += 1;
                match n {
                    'n' => out.push_str(r"\r\n"),
                    't' => out.push_str(r"\t"),
                    'e' => out.push_str(r"\x1b"),
                    'r' => out.push_str(r"\r"),
                    '\\' => out.push_str(r"\\"),
                    ']' => out.push_str(r"\]"),
                    '^' => out.push_str(r"\^"),
                    '-' => out.push_str(r"\-"),
                    'd' => out.push_str("0-9"),
                    's' => out.push_str(r" \t"),
                    other => {
                        out.push('\\');
                        out.push(other);
                    }
                }
            }
            '[' | '&' | '~' => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    if newline != negated {
        out.push_str(r"\r\n");
    }
    out.push(']');
    Ok((out, i))
}

pub(crate) fn compile(pat: &str, opts: &Options<'_>) -> Result<VimRegex, String> {
    let (rust, zs, case) = translate(pat, opts)?;
    let has_upper = {
        // Smartcase ignores escaped characters such as `\S`.
        let mut upper = false;
        let mut chars = pat.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                chars.next();
                continue;
            }
            if c.is_uppercase() {
                upper = true;
                break;
            }
        }
        upper
    };
    let insensitive = match case {
        Some(sensitive) => !sensitive,
        None => opts.ignorecase && !(opts.smartcase && has_upper),
    };
    let flags = if insensitive { "(?mRi)" } else { "(?mR)" };
    let re = Regex::new(&format!("{flags}{rust}")).map_err(|e| {
        let msg = e.to_string();
        format!(
            "E486: Invalid pattern: {}",
            msg.lines().last().unwrap_or(&msg)
        )
    })?;
    Ok(VimRegex {
        re,
        zs,
        source: pat.to_string(),
    })
}

impl VimRegex {
    /// Matches as (start_byte, end_byte) in `text`.
    pub(crate) fn find_iter<'t>(
        &'t self,
        text: &'t str,
    ) -> impl Iterator<Item = (usize, usize)> + 't {
        let mut at = 0usize;
        std::iter::from_fn(move || {
            if at > text.len() {
                return None;
            }
            let caps = self.re.captures_at(text, at)?;
            let whole = caps.get(0)?;
            let m = if self.zs {
                caps.name("vzs").unwrap_or(whole)
            } else {
                whole
            };
            at = if whole.end() > whole.start() {
                whole.end()
            } else {
                let mut n = whole.end() + 1;
                while n < text.len() && !text.is_char_boundary(n) {
                    n += 1;
                }
                n
            };
            Some((m.start(), m.end()))
        })
    }
}

/// Search the rope. `from` is a char index; forward searches start after
/// it (or at it when `accept_at`), backward searches before it.
pub(crate) fn search(
    rope: &Rope,
    hay: &str,
    re: &VimRegex,
    from: usize,
    forward: bool,
    wrap: bool,
    accept_at: bool,
) -> Option<(usize, usize, bool)> {
    let from_b = rope.char_to_byte(from.min(rope.len_chars()));
    let to_chars = |(s, e): (usize, usize)| (rope.byte_to_char(s), rope.byte_to_char(e));
    if forward {
        let start = if accept_at {
            from_b
        } else {
            let mut n = from_b + 1;
            while n < hay.len() && !hay.is_char_boundary(n) {
                n += 1;
            }
            n.min(hay.len())
        };
        let found = re.find_iter(hay).find(|&(s, _)| s >= start);
        if let Some(m) = found {
            let (s, e) = to_chars(m);
            return Some((s, e, false));
        }
        if wrap {
            let m = re.find_iter(hay).next()?;
            let (s, e) = to_chars(m);
            return Some((s, e, true));
        }
        None
    } else {
        let mut last = None;
        let mut overall_last = None;
        for m in re.find_iter(hay) {
            if m.0 < from_b || (accept_at && m.0 == from_b) {
                last = Some(m);
            }
            overall_last = Some(m);
        }
        if let Some(m) = last {
            let (s, e) = to_chars(m);
            return Some((s, e, false));
        }
        if wrap {
            let m = overall_last?;
            let (s, e) = to_chars(m);
            return Some((s, e, true));
        }
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CaseMode {
    None,
    Upper,
    Lower,
}

/// Expand a `:s` replacement for one match. `caps` gives `\0`..`\9`.
pub(crate) fn expand_replacement(
    rep: &str,
    caps: &regex::Captures<'_>,
    re: &VimRegex,
    magic: bool,
) -> String {
    // The added `vzs` group shifts the numbers of user groups after it.
    let vzs = if re.zs {
        re.re.capture_names().position(|n| n == Some("vzs"))
    } else {
        None
    };
    let group = |n: usize| -> &str {
        if n == 0 && vzs.is_some() {
            return caps
                .name("vzs")
                .or_else(|| caps.get(0))
                .map_or("", |m| m.as_str());
        }
        let real = match vzs {
            Some(v) if n >= v => n + 1,
            _ => n,
        };
        caps.get(real).map_or("", |m| m.as_str())
    };
    let mut out = String::new();
    let mut one: CaseMode = CaseMode::None;
    let mut all: CaseMode = CaseMode::None;
    let push = |out: &mut String, s: &str, one: &mut CaseMode, all: CaseMode| {
        for ch in s.chars() {
            let c = match (*one, all) {
                (CaseMode::Upper, _) => {
                    *one = CaseMode::None;
                    ch.to_uppercase().collect::<String>()
                }
                (CaseMode::Lower, _) => {
                    *one = CaseMode::None;
                    ch.to_lowercase().collect::<String>()
                }
                (_, CaseMode::Upper) => ch.to_uppercase().collect(),
                (_, CaseMode::Lower) => ch.to_lowercase().collect(),
                _ => ch.to_string(),
            };
            out.push_str(&c);
        }
    };
    let chars: Vec<char> = rep.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c == '\\' {
            let Some(&n) = chars.get(i) else {
                out.push('\\');
                break;
            };
            i += 1;
            match n {
                '0'..='9' => push(&mut out, group(n as usize - '0' as usize), &mut one, all),
                'n' | 'r' => out.push('\n'),
                't' => out.push('\t'),
                'u' => one = CaseMode::Upper,
                'l' => one = CaseMode::Lower,
                'U' => all = CaseMode::Upper,
                'L' => all = CaseMode::Lower,
                'E' | 'e' => {
                    all = CaseMode::None;
                    one = CaseMode::None;
                }
                '&' if !magic => push(&mut out, group(0), &mut one, all),
                other => {
                    let s = other.to_string();
                    push(&mut out, &s, &mut one, all);
                }
            }
        } else if c == '&' && magic {
            push(&mut out, group(0), &mut one, all);
        } else if c == '\r' {
            out.push('\n');
        } else {
            let s = c.to_string();
            push(&mut out, &s, &mut one, all);
        }
    }
    out
}

/// `~` in a replacement means the previous replacement string.
pub(crate) fn resolve_tilde(rep: &str, previous: &str, magic: bool) -> String {
    if !magic {
        return rep.replace("\\~", previous);
    }
    let mut out = String::new();
    let mut chars = rep.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.push('\\');
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else if c == '~' {
            out.push_str(previous);
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(pat: &str, text: &str) -> Vec<String> {
        let re = compile(pat, &Options::default()).unwrap();
        re.find_iter(text)
            .map(|(s, e)| text[s..e].to_string())
            .collect()
    }

    #[test]
    fn magic_syntax_translates() {
        assert_eq!(m(r"a\+", "caaab"), ["aaa"]);
        assert_eq!(m(r"\<foo\>", "foo foobar foo"), ["foo", "foo"]);
        assert_eq!(m(r"\(ab\)\{2}", "ababab"), ["abab"]);
        assert_eq!(m(r"x\{-1,}", "xxx"), ["x", "x", "x"]);
        assert_eq!(m(r"a.c", "abc a+c"), ["abc", "a+c"]);
        assert_eq!(m(r"a+c", "a+c aac"), ["a+c"]);
        assert_eq!(m(r"\v(a|b)+", "abba c"), ["abba"]);
        assert_eq!(m(r"\Va.c", "abc a.c"), ["a.c"]);
        assert_eq!(m(r"foo\zsbar", "foobar bar"), ["bar"]);
        assert_eq!(m(r"foo\zebar", "foobar foo"), ["foo"]);
        assert_eq!(m(r"\d\+", "a12b3"), ["12", "3"]);
        assert_eq!(m(r"^\s*x", "  x\nx"), ["  x", "x"]);
        assert_eq!(m(r"a$", "a\nba"), ["a", "a"]);
        assert_eq!(m(r"[abc]\+", "xcabz"), ["cab"]);
        assert_eq!(m(r"a\|b", "cab"), ["a", "b"]);
        assert_eq!(m(r"a\_sb", "a\nb a b"), ["a\nb", "a b"]);
        assert_eq!(m(r"(x)", "(x)"), ["(x)"]);
        assert_eq!(m(r"{}", "{}"), ["{}"]);
        assert_eq!(m(r"a*", "*a"), ["", "a", ""]);
        assert!(compile(r"\(a", &Options::default()).is_err());
    }

    #[test]
    fn case_rules_follow_ignorecase_smartcase_and_overrides() {
        let o = Options {
            ignorecase: true,
            smartcase: true,
            ..Options::default()
        };
        assert!(compile("foo", &o).unwrap().re.is_match("FOO"));
        assert!(!compile("Foo", &o).unwrap().re.is_match("FOO"));
        assert!(compile(r"Foo\c", &o).unwrap().re.is_match("FOO"));
        assert!(!compile(r"foo\C", &o).unwrap().re.is_match("FOO"));
        assert!(
            compile(r"\Sx", &o).unwrap().re.is_match("AX"),
            "escaped classes do not trigger smartcase"
        );
    }

    #[test]
    fn replacement_expansion() {
        let re = compile(r"\(\w\+\) \(\w\+\)", &Options::default()).unwrap();
        let caps = re.re.captures("hello world").unwrap();
        assert_eq!(
            expand_replacement(r"\2 \1", &caps, &re, true),
            "world hello"
        );
        assert_eq!(
            expand_replacement(r"\u\1-&", &caps, &re, true),
            "Hello-hello world"
        );
        assert_eq!(expand_replacement(r"\U\1\E!", &caps, &re, true), "HELLO!");
        assert_eq!(expand_replacement(r"a\rb", &caps, &re, true), "a\nb");
        assert_eq!(expand_replacement(r"\&", &caps, &re, true), "&");
        assert_eq!(resolve_tilde("x~y", "Q", true), "xQy");
    }
}
