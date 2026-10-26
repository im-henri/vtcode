//! Bounded, best-effort keyword block pairs for the optional scope animation.
//! This is a decoration helper, so an incomplete or ambiguous block is safer
//! to leave undecorated than to guess a distant connection.

use crate::editor::Editor;

const MAX_LINES: usize = 512;
const MAX_CHARS: usize = 32 * 1024;
type Position = (usize, usize);
type Pair = (Position, Position);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dialect {
    Preprocessor,
    Shell,
    Fish,
    Vim,
    Cmake,
    Php,
    Lua,
    Ruby,
    Julia,
    Matlab,
    Fortran,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Family {
    If,
    Loop,
    For,
    While,
    Case,
    Function,
    Def,
    Try,
    Macro,
    Block,
    Repeat,
    Class,
    Module,
    Declare,
    Select,
    Associate,
}

#[derive(Clone, Copy)]
enum Event {
    Open(Family),
    Close(Family),
    End,
}

fn dialect(editor: &Editor) -> Option<Dialect> {
    let rule = editor.scope_bubble_rules.for_editor(editor);
    if rule.is_some_and(|rule| !rule.language_blocks || rule.syntax == crate::scope_bubble_config::Syntax::Disabled) {
        return None;
    }
    let path = editor.syntax_file_path();
    let extension = path
        .and_then(|path| path.extension())
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let filename = path
        .and_then(|path| path.file_name())
        .and_then(|filename| filename.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    // Unlike language_name(), this never copies an unbounded shebang line.
    let language = rule.and_then(|rule| rule.syntax_for(editor).language())
        .or_else(|| editor.syntax_language_token())
        .unwrap_or("")
        .to_ascii_lowercase();
    let detected = match language.as_str() {
        "c" | "c++" | "cpp" | "c/c++ header" | "c#" | "csharp" | "objective-c"
        | "objective-c++" | "glsl" | "hlsl" => Some(Dialect::Preprocessor),
        "shell script" | "shell" | "bash" | "zsh" | "sh" => Some(Dialect::Shell),
        "fish" | "fish shell" => Some(Dialect::Fish),
        "vim" | "viml" | "vimscript" | "vim script" => Some(Dialect::Vim),
        "cmake" => Some(Dialect::Cmake),
        "php" => Some(Dialect::Php),
        "lua" => Some(Dialect::Lua),
        "ruby" => Some(Dialect::Ruby),
        "julia" => Some(Dialect::Julia),
        "matlab" | "octave" => Some(Dialect::Matlab),
        "fortran" | "fortran modern" | "fortran free form" => Some(Dialect::Fortran),
        _ => None,
    };
    // Explicit language overrides take precedence over an old file extension.
    if !language.is_empty() {
        return detected;
    }
    match extension.as_str() {
        "c" | "h" | "cc" | "hh" | "cpp" | "hpp" | "cxx" | "hxx" | "cs" | "m" | "mm" | "glsl"
        | "hlsl" | "vert" | "frag" | "geom" | "comp" | "tesc" | "tese" => {
            Some(Dialect::Preprocessor)
        }
        "sh" | "bash" | "zsh" => Some(Dialect::Shell),
        "fish" => Some(Dialect::Fish),
        "vim" => Some(Dialect::Vim),
        "cmake" => Some(Dialect::Cmake),
        "php" | "phtml" => Some(Dialect::Php),
        "lua" => Some(Dialect::Lua),
        "rb" => Some(Dialect::Ruby),
        "jl" => Some(Dialect::Julia),
        "f" | "f90" | "f95" | "f03" | "f08" | "for" => Some(Dialect::Fortran),
        _ if filename == "cmakelists.txt" => Some(Dialect::Cmake),
        _ if matches!(filename.as_str(), ".vimrc" | "vimrc" | ".gvimrc" | "gvimrc") => {
            Some(Dialect::Vim)
        }
        _ if matches!(filename.as_str(), ".bashrc" | ".bash_profile" | ".zshrc") => {
            Some(Dialect::Shell)
        }
        _ if extension.is_empty() => {
            let first: String = editor.rope.line(0).chars().take(256).collect();
            (first.starts_with("#!")
                && (first.contains("bash") || first.contains("/sh") || first.contains("zsh")))
            .then_some(Dialect::Shell)
        }
        _ => None,
    }
}

#[derive(Default)]
struct LexState {
    quote: Option<char>,
    block_comment: bool,
    long_end: Option<Vec<char>>,
    ruby_comment: bool,
    heredoc_end: Option<String>,
}

fn starts(chars: &[char], offset: usize, needle: &[char]) -> bool {
    chars.get(offset..offset.saturating_add(needle.len())) == Some(needle)
}

// Lua and CMake both use [=[ ... ]=] strings, with any number of '=' signs.
fn long_delimiter(chars: &[char], offset: usize) -> Option<(usize, Vec<char>)> {
    if chars.get(offset) != Some(&'[') {
        return None;
    }
    let mut end = offset + 1;
    while chars.get(end) == Some(&'=') && end - offset <= 16 {
        end += 1;
    }
    if chars.get(end) != Some(&'[') {
        return None;
    }
    let mut closing = vec![']'];
    closing.extend(std::iter::repeat_n('=', end - offset - 1));
    closing.push(']');
    Some((end - offset + 1, closing))
}

/// Replace comments/strings with spaces so token columns stay in logical chars.
fn code_chars(chars: &[char], dialect: Dialect, state: &mut LexState, code: &mut Vec<char>) {
    code.clear();
    code.resize(chars.len(), ' ');
    if let Some(end) = state.heredoc_end.as_deref() {
        let last = chars
            .iter()
            .rposition(|ch| !matches!(ch, '\r' | '\n'))
            .map_or(0, |index| index + 1);
        let first = chars[..last]
            .iter()
            .position(|ch| *ch != '\t')
            .unwrap_or(last);
        if chars[first..last].iter().copied().eq(end.chars()) {
            state.heredoc_end = None;
        }
        return;
    }
    if dialect == Dialect::Ruby {
        let first = chars
            .iter()
            .position(|ch| !ch.is_whitespace())
            .unwrap_or(chars.len());
        let last = chars
            .iter()
            .rposition(|ch| !ch.is_whitespace())
            .map_or(first, |index| index + 1);
        let text = &chars[first..last];
        if text.iter().copied().eq("=begin".chars()) {
            state.ruby_comment = true;
        }
        if state.ruby_comment {
            if text.iter().copied().eq("=end".chars()) {
                state.ruby_comment = false;
            }
            return;
        }
    }
    let mut i = 0;
    while i < chars.len() {
        if let Some(end) = state.long_end.as_deref() {
            if starts(chars, i, end) {
                i += end.len();
                state.long_end = None;
            } else {
                i += 1;
            }
            continue;
        }
        if state.block_comment {
            if starts(chars, i, &['*', '/']) {
                state.block_comment = false;
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        if let Some(quote) = state.quote {
            if chars[i] == '\\' && quote != '\'' {
                i = (i + 2).min(chars.len());
            } else if chars[i] == quote {
                if quote == '\'' && chars.get(i + 1) == Some(&'\'') && dialect != Dialect::Shell {
                    i += 2;
                } else {
                    state.quote = None;
                    i += 1;
                }
            } else {
                i += 1;
            }
            continue;
        }
        if dialect == Dialect::Php {
            if starts(chars, i, &['/', '/']) {
                break;
            }
            if starts(chars, i, &['/', '*']) {
                state.block_comment = true;
                i += 2;
                continue;
            }
        }
        if dialect == Dialect::Julia && starts(chars, i, &['"', '"', '"']) {
            state.long_end = Some(vec!['"', '"', '"']);
            i += 3;
            continue;
        }
        if dialect == Dialect::Shell
            && starts(chars, i, &['<', '<'])
            && chars.get(i + 2) != Some(&'<')
        {
            let mut from = i + 2 + usize::from(chars.get(i + 2) == Some(&'-'));
            while chars.get(from).is_some_and(|ch| matches!(*ch, ' ' | '\t')) {
                from += 1;
            }
            let quote = chars
                .get(from)
                .copied()
                .filter(|ch| matches!(*ch, '\'' | '"'));
            from += usize::from(quote.is_some());
            let mut to = from;
            while chars
                .get(to)
                .is_some_and(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
            {
                to += 1;
            }
            if to > from && quote.is_none_or(|quote| chars.get(to) == Some(&quote)) {
                state.heredoc_end = Some(chars[from..to].iter().collect());
                break;
            }
        }
        let comment = match dialect {
            Dialect::Shell | Dialect::Fish | Dialect::Ruby | Dialect::Cmake | Dialect::Julia => {
                chars[i] == '#'
            }
            Dialect::Lua => starts(chars, i, &['-', '-']),
            Dialect::Matlab => chars[i] == '%',
            Dialect::Fortran => chars[i] == '!',
            Dialect::Vim => chars[i] == '"' && code[..i].iter().all(|ch| ch.is_whitespace()),
            Dialect::Php => chars[i] == '#',
            Dialect::Preprocessor => false,
        };
        if comment {
            let offset = i + if dialect == Dialect::Lua { 2 } else { 1 };
            if matches!(dialect, Dialect::Lua | Dialect::Cmake)
                && let Some((length, end)) = long_delimiter(chars, offset)
            {
                state.long_end = Some(end);
                i = offset + length;
                continue;
            }
            if dialect == Dialect::Julia && chars.get(offset) == Some(&'=') {
                state.long_end = Some(vec!['=', '#']);
                i = offset + 1;
                continue;
            }
            if dialect == Dialect::Matlab && chars.get(offset) == Some(&'{') {
                state.long_end = Some(vec!['%', '}']);
                i = offset + 1;
                continue;
            }
            break;
        }
        if matches!(dialect, Dialect::Lua | Dialect::Cmake)
            && let Some((length, end)) = long_delimiter(chars, i)
        {
            state.long_end = Some(end);
            i += length;
            continue;
        }
        if matches!(chars[i], '\'' | '"' | '`') {
            state.quote = Some(chars[i]);
            i += 1;
            continue;
        }
        code[i] = chars[i];
        i += 1;
    }
    if !matches!(
        dialect,
        Dialect::Shell
            | Dialect::Fish
            | Dialect::Ruby
            | Dialect::Lua
            | Dialect::Preprocessor
            | Dialect::Php
            | Dialect::Julia
    ) {
        state.quote = None;
    }
}

// Lua's reserved keywords can appear inside expressions (anonymous functions)
// and on the same line. A loop's `do` belongs to its existing opener.
fn lua_events(code: &[char], pending_do: &mut usize, events: &mut Vec<(usize, Event)>) {
    let mut from = 0;
    while from < code.len() {
        if !code[from].is_ascii_alphabetic() && code[from] != '_' {
            from += 1;
            continue;
        }
        let mut end = from + 1;
        while code
            .get(end)
            .is_some_and(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        {
            end += 1;
        }
        let Some(word) = KeywordWord::new(&code[from..end]) else {
            from = end;
            continue;
        };
        let event = match word.as_str() {
            "if" => Some(Event::Open(Family::If)),
            "function" => Some(Event::Open(Family::Function)),
            "for" | "while" => {
                *pending_do += 1;
                Some(Event::Open(Family::Loop))
            }
            "do" if *pending_do > 0 => {
                *pending_do -= 1;
                None
            }
            "do" => Some(Event::Open(Family::Block)),
            "repeat" => Some(Event::Open(Family::Repeat)),
            "until" => Some(Event::Close(Family::Repeat)),
            "end" => Some(Event::End),
            _ => None,
        };
        if let Some(event) = event {
            events.push((from, event));
        }
        from = end;
    }
}

/// All supported reserved words are short ASCII tokens. Keep them on the
/// stack; ordinary long identifiers cannot be block delimiters.
struct KeywordWord {
    bytes: [u8; 32],
    len: usize,
}

impl KeywordWord {
    fn new(chars: &[char]) -> Option<Self> {
        if chars.is_empty() || chars.len() > 32 {
            return None;
        }
        let mut word = Self {
            bytes: [0; 32],
            len: chars.len(),
        };
        for (out, &ch) in word.bytes.iter_mut().zip(chars) {
            *out = ch as u8;
        }
        Some(word)
    }
    fn as_str(&self) -> &str {
        // Tokenizers accept ASCII alphabetic/digit/underscore characters only.
        std::str::from_utf8(&self.bytes[..self.len]).unwrap()
    }
    fn make_ascii_lowercase(&mut self) {
        self.bytes[..self.len].make_ascii_lowercase();
    }
}

fn first_word(chars: &[char], from: usize) -> Option<(KeywordWord, usize, usize)> {
    let mut start = from;
    while chars.get(start).is_some_and(|ch| ch.is_whitespace()) {
        start += 1;
    }
    let mut end = start;
    while chars
        .get(end)
        .is_some_and(|ch| ch.is_ascii_alphabetic() || *ch == '_')
    {
        end += 1;
    }
    Some((KeywordWord::new(&chars[start..end])?, start, end))
}

fn has_word(chars: &[char], expected: &str) -> bool {
    chars
        .split(|ch| !ch.is_ascii_alphanumeric() && *ch != '_')
        .any(|word| {
            word.len() == expected.len()
                && word
                    .iter()
                    .zip(expected.bytes())
                    .all(|(&ch, byte)| ch.eq_ignore_ascii_case(&(byte as char)))
        })
}

fn statement_event(chars: &[char], dialect: Dialect) -> Option<(usize, Event)> {
    let first = chars.iter().position(|ch| !ch.is_whitespace())?;
    let from = first + usize::from(dialect == Dialect::Vim && chars[first] == ':');
    let (mut word, column, end) = first_word(chars, from)?;
    if matches!(dialect, Dialect::Cmake | Dialect::Php | Dialect::Fortran) {
        word.make_ascii_lowercase();
    }
    if matches!(dialect, Dialect::Lua | Dialect::Julia)
        && matches!(word.as_str(), "local" | "mutable")
    {
        word = first_word(chars, end)?.0;
    }
    let tail = &chars[end..];
    if dialect == Dialect::Cmake && tail.iter().find(|ch| !ch.is_whitespace()) != Some(&'(') {
        return None;
    }
    let event = match dialect {
        Dialect::Shell => match word.as_str() {
            "if" => Event::Open(Family::If),
            "fi" => Event::Close(Family::If),
            "case" => Event::Open(Family::Case),
            "esac" => Event::Close(Family::Case),
            "for" | "while" | "until" | "select" => Event::Open(Family::Loop),
            "done" => Event::Close(Family::Loop),
            _ => return None,
        },
        Dialect::Vim | Dialect::Cmake | Dialect::Php => {
            let family = match word.as_str() {
                "if" | "endif" => Family::If,
                "for" | "endfor" | "foreach" | "endforeach" => Family::For,
                "while" | "endwhile" => Family::While,
                "function" | "endfunction" => Family::Function,
                "def" | "enddef" if dialect == Dialect::Vim => Family::Def,
                "try" | "endtry" if dialect == Dialect::Vim => Family::Try,
                "macro" | "endmacro" if dialect == Dialect::Cmake => Family::Macro,
                "block" | "endblock" if dialect == Dialect::Cmake => Family::Block,
                "switch" | "endswitch" if dialect == Dialect::Php => Family::Case,
                "declare" | "enddeclare" if dialect == Dialect::Php => Family::Declare,
                _ => return None,
            };
            if word.as_str().starts_with("end") {
                Event::Close(family)
            } else {
                if dialect == Dialect::Php && !tail.iter().any(|ch| *ch == ':') {
                    return None;
                }
                Event::Open(family)
            }
        }
        Dialect::Lua | Dialect::Fish | Dialect::Ruby | Dialect::Julia | Dialect::Matlab => {
            match word.as_str() {
                "end" => Event::End,
                "if" => {
                    if dialect == Dialect::Lua && !has_word(tail, "then") {
                        return None;
                    }
                    Event::Open(Family::If)
                }
                "unless" if dialect == Dialect::Ruby => Event::Open(Family::If),
                "for" | "while" | "until" | "parfor" => {
                    if word.as_str() == "until" && dialect == Dialect::Lua {
                        Event::Close(Family::Repeat)
                    } else {
                        Event::Open(Family::Loop)
                    }
                }
                "repeat" if dialect == Dialect::Lua => Event::Open(Family::Repeat),
                "function" => Event::Open(Family::Function),
                "def" if dialect == Dialect::Ruby => Event::Open(Family::Def),
                "try" if matches!(dialect, Dialect::Julia | Dialect::Matlab) => {
                    Event::Open(Family::Try)
                }
                "case" if dialect == Dialect::Ruby => Event::Open(Family::Case),
                "switch" if matches!(dialect, Dialect::Fish | Dialect::Matlab) => {
                    Event::Open(Family::Case)
                }
                "class" | "classdef" | "struct" => Event::Open(Family::Class),
                "module" if matches!(dialect, Dialect::Ruby | Dialect::Julia) => {
                    Event::Open(Family::Module)
                }
                "macro" if dialect == Dialect::Julia => Event::Open(Family::Macro),
                "do" | "begin" | "let" | "quote" => Event::Open(Family::Block),
                "methods" | "properties" | "events" | "enumeration" | "spmd"
                    if dialect == Dialect::Matlab =>
                {
                    Event::Open(Family::Block)
                }
                "endif" if dialect == Dialect::Matlab => Event::Close(Family::If),
                "endfor" | "endwhile" if dialect == Dialect::Matlab => Event::Close(Family::Loop),
                "endswitch" if dialect == Dialect::Matlab => Event::Close(Family::Case),
                "endfunction" if dialect == Dialect::Matlab => Event::Close(Family::Function),
                "end_try_catch" if dialect == Dialect::Matlab => Event::Close(Family::Try),
                _ => return None,
            }
        }
        Dialect::Fortran => {
            let (combined, closing) = if word.as_str() == "end" {
                let mut combined = first_word(chars, end)?.0;
                combined.make_ascii_lowercase();
                if combined.as_str().starts_with("end") {
                    return None;
                }
                (combined, true)
            } else {
                let closing = word.as_str().starts_with("end");
                (word, closing)
            };
            let combined = combined.as_str();
            let token = combined.strip_prefix("end").unwrap_or(combined);
            let family = match token {
                "if" if closing || has_word(tail, "then") => Family::If,
                "do" => Family::Loop,
                "select" => Family::Select,
                "block" => Family::Block,
                "associate" => Family::Associate,
                "function" | "subroutine" => Family::Function,
                _ => return None,
            };
            if closing {
                Event::Close(family)
            } else {
                Event::Open(family)
            }
        }
        Dialect::Preprocessor => unreachable!(),
    };
    Some((column, event))
}

/// Return the innermost explicit keyword block containing the cursor row.
/// Both endpoints must fit inside a 512-line / 32 Ki-character neighborhood.
/// Unsupported grammars and huge lines take the cheap exit. The same bounded
/// neighborhood is safe to inspect in the large-file renderer too.
#[cfg(test)]
pub(crate) fn keyword_scope(editor: &Editor) -> Option<Pair> {
    KeywordScopeIndex::build(editor)?.enclosing_scope(editor.cursor_line)
}

/// Reuse the bounded keyword neighborhood across cursor movement. The central
/// half of a window remains queryable; approaching an edge refreshes it so
/// distant closers can enter the budget without reparsing on every row.
pub(crate) struct KeywordScopeIndex {
    first: usize,
    end: usize,
    total: usize,
    pairs: Vec<Pair>,
    lookup: crate::animation_structural_scope::MaxEndTree,
    uncertain_before: Option<usize>,
    uncertain_after: Option<usize>,
}

impl KeywordScopeIndex {
    pub(crate) fn covers_cursor(&self, line: usize) -> bool {
        let margin = (self.end - self.first) / 4;
        line >= self.first
            && line < self.end
            && (self.first == 0 || line >= self.first + margin)
            && (self.end == self.total || line < self.end - margin)
            // Missing endpoints may enter a newly centered window even while
            // the cursor remains in this window's central half. A completed
            // inner pair is still reliable; otherwise refresh that boundary
            // span instead of retaining a stale negative result.
            && (!(self.uncertain_before.is_some_and(|end| line <= end)
                || self.uncertain_after.is_some_and(|start| line >= start))
                || self.enclosing_scope(line).is_some())
    }

    pub(crate) fn enclosing_scope(&self, line: usize) -> Option<Pair> {
        let last = self
            .pairs
            .partition_point(|pair| pair.0.0 <= line)
            .checked_sub(1)?;
        self.lookup
            .rightmost_before(last, line.saturating_add(1))
            .map(|index| self.pairs[index])
    }

    pub(crate) fn build(editor: &Editor) -> Option<Self> {
        if editor.cursor_line >= editor.rope.len_lines() {
            return None;
        }
        let dialect = dialect(editor)?;
        let total = editor.rope.len_lines();
        let cursor = editor.cursor_line;
        let mut first = cursor.saturating_sub(MAX_LINES / 2);
        let mut end = first.saturating_add(MAX_LINES).min(total);
        first = end.saturating_sub(MAX_LINES);
        let end_char = if end == total {
            editor.rope.len_chars()
        } else {
            editor.rope.line_to_char(end)
        };
        let mut char_count = end_char - editor.rope.line_to_char(first);
        while char_count > MAX_CHARS {
            if first == cursor && end == cursor + 1 {
                return None;
            }
            if cursor - first > end.saturating_sub(cursor + 1) {
                char_count -= editor.rope.line(first).len_chars();
                first += 1;
            } else {
                end -= 1;
                char_count -= editor.rope.line(end).len_chars();
            }
        }
        if dialect == Dialect::Preprocessor {
            let regions =
                crate::animation_structural_scope::preprocessor_keyword_regions(editor, first, end);
            return Self::from_pairs(
                first,
                end,
                total,
                regions.pairs,
                regions.uncertain_before,
                regions.uncertain_after,
            );
        }
        let mut state = LexState::default();
        let mut pending_lua_do = 0;
        let mut stack: Vec<(Family, Position)> = Vec::new();
        let mut pairs = Vec::new();
        let mut uncertain_before = None;
        let mut uncertain_after = None;
        // Keep line decoding, lexical masking, and keyword events in reusable
        // scratch buffers. A 512-row neighborhood no longer allocates three
        // temporary vectors for every row.
        let mut chars = Vec::new();
        let mut code = Vec::new();
        let mut events = Vec::new();
        for line in first..end {
            chars.clear();
            chars.extend(editor.rope.line(line).chars());
            code_chars(&chars, dialect, &mut state, &mut code);
            events.clear();
            if dialect == Dialect::Lua {
                lua_events(&code, &mut pending_lua_do, &mut events);
            } else {
                let mut start = 0;
                for segment_end in (0..=code.len()).filter(|&index| {
                    index == code.len()
                        || (code[index] == ';' && dialect != Dialect::Fortran)
                        || (code[index] == '|' && dialect == Dialect::Vim)
                }) {
                    if let Some((column, event)) =
                        statement_event(&code[start..segment_end], dialect)
                    {
                        events.push((start + column, event));
                    }
                    start = segment_end + 1;
                }
            }
            for &(column, event) in &events {
                let position = (line, column);
                match event {
                    Event::Open(family) => stack.push((family, position)),
                    Event::Close(family) if stack.last().is_some_and(|entry| entry.0 != family) => {
                        // Do not bridge malformed or partially visible nesting.
                        uncertain_after = Some(
                            uncertain_after
                                .map_or(stack[0].1.0, |start: usize| start.min(stack[0].1.0)),
                        );
                        uncertain_before = Some(position.0);
                        stack.clear();
                    }
                    Event::Close(_) | Event::End => {
                        if let Some((_, opener)) = stack.pop() {
                            pairs.push((opener, position));
                        } else {
                            uncertain_before = Some(position.0);
                        }
                    }
                }
            }
        }
        if let Some((_, opener)) = stack.first() {
            uncertain_after = Some(uncertain_after.map_or(opener.0, |start| start.min(opener.0)));
        }
        Self::from_pairs(first, end, total, pairs, uncertain_before, uncertain_after)
    }

    fn from_pairs(
        first: usize,
        end: usize,
        total: usize,
        mut pairs: Vec<Pair>,
        uncertain_before: Option<usize>,
        uncertain_after: Option<usize>,
    ) -> Option<Self> {
        // The first branch and the whole conditional share their opener.
        // Rightmost lookup must choose the smaller branch on those rows.
        pairs.sort_unstable_by_key(|pair| (pair.0, std::cmp::Reverse(pair.1)));
        let lookup = crate::animation_structural_scope::MaxEndTree::build_bounded(
            pairs.iter().map(|pair| pair.1.0.saturating_add(1)),
        )?;
        Some(Self {
            first,
            end,
            total,
            pairs,
            lookup,
            uncertain_before,
            uncertain_after,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor(text: &str, language: &str, line: usize) -> Editor {
        let mut editor = Editor::test_with_text(text);
        editor.language_override = Some(language.into());
        editor.cursor_line = line;
        editor
    }

    #[test]
    fn nested_vim_conditionals_select_the_cursor_block_at_either_end_or_body() {
        let text = "if outer\n  if inner\n    echo 'hello'\n  endif\nendif\n";
        for line in [1, 2, 3] {
            assert_eq!(
                keyword_scope(&editor(text, "Vim", line)),
                Some(((1, 2), (3, 2)))
            );
        }
        for line in [0, 4] {
            assert_eq!(
                keyword_scope(&editor(text, "Vim", line)),
                Some(((0, 0), (4, 0)))
            );
        }
    }

    #[test]
    fn mixed_shell_families_preserve_nesting_and_inline_closers() {
        let text = "if test -f file; then\n  for value in a b; do\n    case $value in\n      a) echo 'fi done esac' ;;\n    esac\n  done\nfi\n";
        assert_eq!(
            keyword_scope(&editor(text, "Shell Script", 3)),
            Some(((2, 4), (4, 4)))
        );
        assert_eq!(
            keyword_scope(&editor(text, "Shell Script", 5)),
            Some(((1, 2), (5, 2)))
        );
        assert_eq!(
            keyword_scope(&editor("if true; then echo yes; fi\n", "Bash", 0)),
            Some(((0, 0), (0, 24)))
        );
    }

    #[test]
    fn preprocessor_directives_skip_strings_and_multiline_comments() {
        let text = "#if OUTER\n/*\n#if FAKE\n#endif\n*/\nconst char *s = \"#endif\";\n# ifdef INNER\nint value;\n# endif\n#endif\n";
        assert_eq!(
            keyword_scope(&editor(text, "C++", 7)),
            Some(((6, 0), (8, 0)))
        );
        assert_eq!(
            keyword_scope(&editor(text, "C++", 5)),
            Some(((0, 0), (9, 0)))
        );
        assert_eq!(keyword_scope(&editor("#if X\n#endif\n", "Python", 0)), None);
    }

    #[test]
    fn cmake_long_strings_and_comments_cannot_close_a_real_block() {
        let text = "if(OUTER)\n  set(value [=[\nendif()\n]=])\n  #[[\nendif()\n]]\n  foreach(value IN ITEMS a b)\n    message(\"endif()\")\n  endforeach()\nendif()\n";
        assert_eq!(
            keyword_scope(&editor(text, "CMake", 8)),
            Some(((7, 2), (9, 2)))
        );
        assert_eq!(
            keyword_scope(&editor(text, "CMake", 3)),
            Some(((0, 0), (10, 0)))
        );
    }

    #[test]
    fn end_only_has_meaning_in_a_supported_language() {
        assert_eq!(
            keyword_scope(&editor("if ready\n  value\nend\n", "Plain Text", 1)),
            None
        );
        assert_eq!(
            keyword_scope(&editor("if ready\n  value\nend\n", "Ruby", 1)),
            Some(((0, 0), (2, 0)))
        );
        assert_eq!(
            keyword_scope(&editor(
                "if ready then\n  -- endif end\n  print('end')\nend\n",
                "Lua",
                2
            )),
            Some(((0, 0), (3, 0)))
        );
        assert_eq!(
            keyword_scope(&editor("if (ready) {\n  value();\n}\nendif;\n", "PHP", 1)),
            None
        );
        assert_eq!(
            keyword_scope(&editor("if ($ready):\n  value();\nendif;\n", "PHP", 1)),
            Some(((0, 0), (2, 0)))
        );
    }

    #[test]
    fn fortran_spaced_closers_and_vim_loop_families_match() {
        assert_eq!(
            keyword_scope(&editor(
                "IF (ready) THEN\n  DO i = 1, 5\n    print *, 'end if'\n  END DO\nEND IF\n",
                "Fortran",
                2
            )),
            Some(((1, 2), (3, 2)))
        );
        assert_eq!(
            keyword_scope(&editor(
                "for item in items\n  while ready\n    echo \"endwhile\"\n  endwhile\nendfor\n",
                "Vim Script",
                2
            )),
            Some(((1, 2), (3, 2)))
        );
    }

    #[test]
    fn mismatched_families_and_out_of_window_endpoints_are_not_guessed() {
        assert_eq!(
            keyword_scope(&editor(
                "if ready\n  for item in items\nendif\nendfor\n",
                "Vim",
                1
            )),
            None
        );
        let text = format!("if ready\n{}endif\n", "echo value\n".repeat(MAX_LINES));
        assert_eq!(keyword_scope(&editor(&text, "Vim", MAX_LINES / 2)), None);
        let huge = format!("if ready\n{}\nendif\n", "x".repeat(MAX_CHARS + 1));
        assert_eq!(keyword_scope(&editor(&huge, "Vim", 1)), None);
        assert_eq!(keyword_scope(&editor(&huge, "Vim", 0)), None);
    }

    #[test]
    fn filenames_select_a_dialect_and_large_files_keep_bounded_scopes() {
        let mut editor = editor("if ready\n  echo 'endif'\nendif\n", "Vim", 1);
        editor.language_override = None;
        editor.file_path = Some(".vimrc".into());
        assert_eq!(keyword_scope(&editor), Some(((0, 0), (2, 0))));
        editor.large_file_mode = true;
        assert_eq!(keyword_scope(&editor), Some(((0, 0), (2, 0))));

        // Enabling the guide in large-file mode must retain the same strict
        // character and line budgets as the ordinary editor path.
        editor.rope =
            ropey::Rope::from_str(&format!("if ready\n{}\nendif\n", "x".repeat(MAX_CHARS + 1)));
        assert_eq!(keyword_scope(&editor), None);
        editor.rope = ropey::Rope::from_str(&format!(
            "if ready\n{}endif\n",
            "echo value\n".repeat(MAX_LINES)
        ));
        editor.cursor_line = MAX_LINES / 2;
        assert_eq!(keyword_scope(&editor), None);
    }

    #[test]
    fn case_sensitive_languages_do_not_match_command_or_constant_names() {
        assert_eq!(
            keyword_scope(&editor("IF condition\nwork\nFI\n", "Bash", 1)),
            None
        );
        assert_eq!(keyword_scope(&editor("IF\nvalue\nEND\n", "Ruby", 1)), None);
        assert_eq!(
            keyword_scope(&editor("#IF X\nvalue\n#ENDIF\n", "C", 1)),
            None
        );
    }

    #[test]
    fn multiline_literals_and_here_documents_do_not_supply_fake_blocks() {
        let cpp = "#if REAL\nauto text = R\"tag(\n#if FAKE\n#endif\n)tag\";\n#endif\n";
        assert_eq!(
            keyword_scope(&editor(cpp, "C++", 3)),
            Some(((0, 0), (5, 0)))
        );
        let shell = "if true; then\n  cat <<'EOF'\nif fake; then\nfi\nEOF\nfi\n";
        assert_eq!(
            keyword_scope(&editor(shell, "Bash", 3)),
            Some(((0, 0), (5, 0)))
        );
        let lua = "if outer then\n  local f = function() if inner then return 'end' end end\n  while ready do\n    work()\n  end\nend\n";
        assert_eq!(
            keyword_scope(&editor(lua, "Lua", 3)),
            Some(((2, 2), (4, 2)))
        );
        assert_eq!(
            keyword_scope(&editor(lua, "Lua", 5)),
            Some(((0, 0), (5, 0)))
        );
    }

    #[test]
    fn cached_keyword_neighborhood_reuses_nested_and_sibling_membership() {
        let source = "if outer\n  if first\n    echo one\n  endif\n  if second\n    echo two\n  endif\nendif\necho outside\n";
        let index = KeywordScopeIndex::build(&editor(source, "Vim", 2)).unwrap();
        let expected = [
            Some(((0, 0), (7, 0))),
            Some(((1, 2), (3, 2))),
            Some(((1, 2), (3, 2))),
            Some(((1, 2), (3, 2))),
            Some(((4, 2), (6, 2))),
            Some(((4, 2), (6, 2))),
            Some(((4, 2), (6, 2))),
            Some(((0, 0), (7, 0))),
            None,
            None,
        ];
        for (line, expected) in expected.into_iter().enumerate() {
            assert!(index.covers_cursor(line), "line {line}");
            assert_eq!(index.enclosing_scope(line), expected, "line {line}");
        }
        assert!(!index.covers_cursor(10));
        assert_eq!(index.enclosing_scope(10), None);
    }

    #[test]
    fn keyword_cache_refreshes_before_the_old_window_edge() {
        let mut lines = vec!["echo value\n"; 1_000];
        lines[350] = "if ready\n";
        lines[600] = "endif\n";
        let source = lines.concat();
        let mut editor = editor(&source, "Vim", 128);
        let initial = KeywordScopeIndex::build(&editor).unwrap();
        assert_eq!((initial.first, initial.end), (0, MAX_LINES));
        assert!(initial.covers_cursor(128));
        assert!(initial.covers_cursor(349));
        assert!(!initial.covers_cursor(383));
        assert!(!initial.covers_cursor(384));
        assert_eq!(initial.enclosing_scope(384), None);

        editor.cursor_line = 384;
        let refreshed = KeywordScopeIndex::build(&editor).unwrap();
        assert_eq!((refreshed.first, refreshed.end), (128, 640));
        assert!(refreshed.covers_cursor(384));
        assert_eq!(refreshed.enclosing_scope(384), Some(((350, 0), (600, 0))));
        assert!(!refreshed.covers_cursor(255));
        assert!(refreshed.covers_cursor(256));
    }

    #[test]
    fn keyword_cache_coverage_respects_character_limited_windows() {
        let source = format!("echo {}\n", "x".repeat(394)).repeat(1_000);
        let editor = editor(&source, "Vim", 500);
        let index = KeywordScopeIndex::build(&editor).unwrap();
        assert!(index.first > 0);
        assert!(index.end < editor.rope.len_lines());
        assert!(index.end - index.first < MAX_LINES);
        assert!(
            editor.rope.line_to_char(index.end) - editor.rope.line_to_char(index.first)
                <= MAX_CHARS
        );
        assert!(index.covers_cursor(editor.cursor_line));
        assert!(!index.covers_cursor(index.first));
        assert!(!index.covers_cursor(index.end - 1));
        assert_eq!(index.enclosing_scope(editor.cursor_line), None);
    }

    #[test]
    fn incomplete_keyword_boundary_refreshes_for_newly_visible_endpoints() {
        let mut lines = vec!["echo value\n"; 1_000];
        lines[300] = "if ready\n";
        lines[620] = "endif\n";
        let source = lines.concat();
        let mut editor = editor(&source, "Vim", 128);
        let before = KeywordScopeIndex::build(&editor).unwrap();
        assert!(before.covers_cursor(128));
        // 375 lies in the central half of [0, 512), but the closer at 620
        // becomes visible in a fresh [119, 631) window around that row.
        assert!(!before.covers_cursor(375));
        assert_eq!(before.enclosing_scope(375), None);
        editor.cursor_line = 375;
        let inside = KeywordScopeIndex::build(&editor).unwrap();
        assert_eq!(inside.enclosing_scope(375), Some(((300, 0), (620, 0))));
        assert!(inside.covers_cursor(375));

        editor.cursor_line = 670;
        let after = KeywordScopeIndex::build(&editor).unwrap();
        assert_eq!((after.first, after.end), (414, 926));
        assert!(after.covers_cursor(670));
        // Moving backward can bring an absent opener into view in the same
        // way, even before reaching the old central-half boundary at 542.
        assert!(!after.covers_cursor(550));
        assert_eq!(after.enclosing_scope(550), None);
        editor.cursor_line = 550;
        let inside = KeywordScopeIndex::build(&editor).unwrap();
        assert_eq!(inside.enclosing_scope(550), Some(((300, 0), (620, 0))));
        assert!(inside.covers_cursor(550));
    }

    #[test]
    fn complete_inner_keyword_scopes_reuse_an_incomplete_outer_window() {
        let mut lines = vec!["echo value\n"; 1_000];
        lines[100] = "if outer\n";
        lines[300] = "  if inner\n";
        lines[330] = "  endif\n";
        lines[620] = "endif\n";
        let source = lines.concat();
        let index = KeywordScopeIndex::build(&editor(&source, "Vim", 128)).unwrap();
        assert!(!index.covers_cursor(200));
        for line in [300, 315, 330] {
            assert!(index.covers_cursor(line));
            assert_eq!(index.enclosing_scope(line), Some(((300, 2), (330, 2))));
        }
        assert!(!index.covers_cursor(331));
    }

    #[test]
    fn malformed_keyword_nesting_does_not_cache_an_ambiguous_negative() {
        let source = "if outer\n  for item in items\nendif\nendfor\n";
        let index = KeywordScopeIndex::build(&editor(source, "Vim", 1)).unwrap();
        for line in 0..4 {
            assert_eq!(index.enclosing_scope(line), None);
            assert!(!index.covers_cursor(line));
        }
    }
}
