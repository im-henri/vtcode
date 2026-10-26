//! Verified, bounded source edits used to keep scope identity across typing.
//! Shared Rope chunks skip unchanged text; uncertain lexical changes invalidate
//! the decoration until the structural worker supplies a fresh result.

use ropey::Rope;

type Position = (usize, usize);
type Pair = (Position, Position);
const MAX_TEXT: usize = 32 * 1024;
// This comparison runs during drawing, unlike the cancellable worker index.
// Shared text must not turn an edit in a huge file into an unbounded metadata
// walk. Uncertain scope continuity can wait for the worker's exact revision.
const MAX_CHUNKS: usize = 512;

#[derive(Clone, Copy, Debug)]
pub(super) struct ScopeEdit {
    start: usize,
    old_end: usize,
    new_end: usize,
    preserves_scope: bool,
    preserves_depth: bool,
    preserves_regex_state: bool,
    depth_invalid_from: Option<usize>,
}

impl ScopeEdit {
    pub(super) fn between(old: &Rope, new: &Rope) -> Option<Self> {
        if old.is_instance(new) {
            return Some(Self {
                start: 0,
                old_end: 0,
                new_end: 0,
                preserves_scope: true,
                preserves_depth: true,
                preserves_regex_state: true,
                depth_invalid_from: None,
            });
        }
        let mut chunks = MAX_CHUNKS;
        let mut text = MAX_TEXT;
        let prefix = equal_edge(old.chunks(), new.chunks(), false, &mut chunks, &mut text)?;
        let start = old.byte_to_char(prefix);
        let suffix = equal_edge(
            old.chunks_at_byte(old.len_bytes()).0.reversed(),
            new.chunks_at_byte(new.len_bytes()).0.reversed(),
            true,
            &mut chunks,
            &mut text,
        )?;
        let suffix_start = old.len_bytes().saturating_sub(suffix);
        let mut suffix_char = old.byte_to_char(suffix_start);
        if old.char_to_byte(suffix_char) < suffix_start {
            suffix_char += 1;
        }
        let suffix = (old.len_chars() - suffix_char)
            .min(old.len_chars().saturating_sub(start))
            .min(new.len_chars().saturating_sub(start));
        let mut result = Self {
            start,
            old_end: old.len_chars() - suffix,
            new_end: new.len_chars() - suffix,
            preserves_scope: false,
            preserves_depth: false,
            preserves_regex_state: false,
            depth_invalid_from: None,
        };
        if result.old_end - start > MAX_TEXT || result.new_end - start > MAX_TEXT {
            return None;
        }
        let mut old_skipped = skipped_lines(old, start, result.old_end);
        let mut new_skipped = skipped_lines(new, start, result.new_end);
        let mut skipped_changed = false;
        let mut any_skipped = false;
        loop {
            let before = old_skipped.next();
            let after = new_skipped.next();
            if before.is_none() && after.is_none() {
                break;
            }
            skipped_changed |= before != after;
            any_skipped |= before == Some(true) || after == Some(true);
        }
        let line_break_changed = old
            .slice(start..result.old_end)
            .chars()
            .chain(new.slice(start..result.new_end).chars())
            .any(|ch| matches!(ch, '\n' | '\r'));
        if any_skipped && (skipped_changed || line_break_changed) {
            // The palette index ignores entire pathological lines. Crossing
            // its limit, or moving a bracket across its newline, can change
            // prefix depth without editing the bracket itself.
            result.depth_invalid_from = Some(old.char_to_line(start));
        }
        // Changes to quotes, comments, literal prefixes, delimiters or regex
        // control words can change arbitrarily distant lexical state. Ordinary
        // identifier/value edits on plain source lines do none of those things.
        result.preserves_depth = !old
            .slice(start..result.old_end)
            .chars()
            .any(structural_char)
            && !new
                .slice(start..result.new_end)
                .chars()
                .any(structural_char);
        // Round and square delimiters change raw palette depth, but do not
        // change curly scope membership in the C/Rust family. JavaScript also
        // requires its regex/control token signature to remain unchanged.
        if !old
            .slice(start..result.old_end)
            .chars()
            .any(|ch| matches!(ch, '{' | '}'))
            && !new
                .slice(start..result.new_end)
                .chars()
                .any(|ch| matches!(ch, '{' | '}'))
            && let Some((before, after)) =
                plain_lines(old, start, result.old_end).zip(plain_lines(new, start, result.new_end))
            && same_lexical_barriers(&before, &after)
            && (!(before.contains(['"', '/']) || after.contains(['"', '/'])) || !line_break_changed)
        {
            result.preserves_scope = true;
            result.preserves_regex_state = regex_signature(&before).eq(regex_signature(&after));
        }
        Some(result)
    }

    pub(super) fn preserves_scope(self, editor: &crate::editor::Editor) -> bool {
        let rule = editor.scope_bubble_rules.for_editor(editor);
        if rule.is_some_and(|rule| rule.delimiters.is_some_and(|mask|
            mask & !crate::scope_bubble_config::Delimiter::Braces.bit() != 0))
            && !self.preserves_depth
        {
            // Parentheses/brackets now affect membership as well as palette depth.
            return false;
        }
        let language = rule.and_then(|rule| rule.syntax_for(editor).language())
            .or_else(|| editor.syntax_language_token());
        if rule.is_some() && language.is_some_and(|name| {
            crate::scope_bubble_config::language_key(name) == "python"
        }) { return false; }
        let javascript = if let Some(language) = language {
            [
                "javascript",
                "javascript (babel)",
                "javascriptreact",
                "jsx",
                "typescript",
                "typescriptreact",
                "tsx",
            ]
            .into_iter()
            .any(|name| language.eq_ignore_ascii_case(name))
        } else {
            editor
                .syntax_file_path()
                .and_then(|path| path.extension())
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    ["js", "mjs", "cjs", "jsx", "ts", "tsx"]
                        .into_iter()
                        .any(|name| extension.eq_ignore_ascii_case(name))
                })
        };
        self.preserves_scope && (!javascript || self.preserves_regex_state)
    }

    pub(super) fn preserves_depth_at(self, old: &Rope, position: Position) -> bool {
        if self
            .depth_invalid_from
            .is_some_and(|line| position.0 >= line)
        {
            return false;
        }
        self.preserves_depth
            || old.get_line(position.0).is_some_and(|line| {
                position.1 < line.len_chars()
                    && old.line_to_char(position.0) + position.1 < self.start
            })
    }

    pub(super) fn cursor(self, old: &Rope, new: &Rope, position: Position) -> Option<Position> {
        self.position(old, new, position).or_else(|| {
            let line = old.get_line(position.0)?;
            if position.1 > line.len_chars() {
                return None;
            }
            let offset = old.line_to_char(position.0) + position.1;
            if offset < self.start || offset >= self.old_end {
                return None;
            }
            let line = new.char_to_line(self.new_end);
            Some((line, self.new_end - new.line_to_char(line)))
        })
    }

    pub(super) fn position(self, old: &Rope, new: &Rope, position: Position) -> Option<Position> {
        let line = old.get_line(position.0)?;
        if position.1 > line.len_chars() {
            return None;
        }
        let offset = old.line_to_char(position.0) + position.1;
        let offset = if offset < self.start {
            offset
        } else if offset >= self.old_end {
            offset.checked_add_signed(self.new_end as isize - self.old_end as isize)?
        } else {
            return None;
        };
        let line = new.char_to_line(offset);
        Some((line, offset - new.line_to_char(line)))
    }

    pub(super) fn pair(self, old: &Rope, new: &Rope, pair: Pair) -> Option<Pair> {
        Some((
            self.position(old, new, pair.0)?,
            self.position(old, new, pair.1)?,
        ))
    }
}

fn skipped_lines(source: &Rope, start: usize, end: usize) -> impl Iterator<Item = bool> + '_ {
    let first = source.char_to_line(start);
    let last = source.char_to_line(end);
    source
        .lines_at(first)
        .take(last - first + 1)
        .map(|line| line.len_chars() > crate::background::MAX_BRACKET_LINE_CHARS)
}

fn structural_char(ch: char) -> bool {
    matches!(ch, '{' | '}' | '[' | ']' | '(' | ')')
}

fn plain_lines(source: &Rope, start: usize, end: usize) -> Option<String> {
    let first = source.char_to_line(start);
    let last = source.char_to_line(end);
    let start = source.line_to_char(first);
    let end = source
        .get_line(last + 1)
        .map_or(source.len_chars(), |_| source.line_to_char(last + 1));
    if end - start > MAX_TEXT {
        return None;
    }
    let lines = source.slice(start..end);
    Some(lines.to_string())
}

/// Ordinary text can change inside a quoted string or comment, or elsewhere
/// on the same line, without changing the lexical barriers themselves. Keep
/// quote/comment runs intact and abstain around escapes and raw delimiters.
fn same_lexical_barriers(before: &str, after: &str) -> bool {
    let sensitive = |source: &str| {
        source
            .chars()
            .any(|ch| matches!(ch, '\'' | '`' | '\\' | '#' | '$' | '@'))
            || source.char_indices().any(|(at, ch)| {
                if ch == '"' {
                    let prefix = source[..at]
                        .rsplit(|ch: char| !ch.is_alphanumeric() && ch != '_')
                        .next()
                        .unwrap_or("");
                    matches!(prefix, "r" | "br" | "cr" | "R" | "uR" | "UR" | "LR" | "u8R")
                } else if ch == ')' {
                    // A C++ raw closing delimiter can be on a later line than
                    // its opener. Its name is semantically significant text.
                    source[at + 1..].chars().take(17).find(|next| {
                        next.is_whitespace() || matches!(*next, '"' | '(' | ')' | '\\')
                    }) == Some('"')
                } else {
                    false
                }
            })
    };
    if sensitive(before) || sensitive(after) {
        return false;
    }
    // Moving a newline across a quote can move that quote out of a line
    // comment even when the total number of line breaks stays unchanged.
    let line_sensitive = before.contains(['"', '/']) || after.contains(['"', '/']);
    let ordinary =
        |ch: char| !matches!(ch, '"' | '/' | '*') && !(line_sensitive && matches!(ch, '\n' | '\r'));
    before
        .split(ordinary)
        .filter(|run| !run.is_empty())
        .eq(after.split(ordinary).filter(|run| !run.is_empty()))
}

fn control_word(word: &str) -> bool {
    matches!(
        word,
        "if" | "while"
            | "for"
            | "with"
            | "switch"
            | "catch"
            | "return"
            | "throw"
            | "case"
            | "yield"
            | "await"
            | "typeof"
            | "void"
            | "delete"
            | "in"
            | "instanceof"
            | "else"
            | "do"
    )
}

/// JavaScript's identifier/operator sequence controls whether a later slash
/// begins a regex. Identifier contents can change freely unless they become
/// a control word, while token insertion/removal must be analyzed again.
fn regex_signature(source: &str) -> impl Iterator<Item = &str> {
    let mut chars = source.char_indices().peekable();
    std::iter::from_fn(move || {
        loop {
            let (start, ch) = chars.next()?;
            if ch.is_whitespace() {
                continue;
            }
            let identifier = ch.is_alphabetic() || ch == '_';
            if identifier || ch.is_ascii_digit() {
                while chars.peek().is_some_and(|(_, next)| {
                    if identifier {
                        next.is_alphanumeric() || *next == '_'
                    } else {
                        next.is_ascii_digit()
                    }
                }) {
                    chars.next();
                }
                let end = chars.peek().map_or(source.len(), |(index, _)| *index);
                let token = &source[start..end];
                return Some(if control_word(token) {
                    token
                } else if identifier {
                    "\0word"
                } else {
                    "\0number"
                });
            } else {
                return Some(&source[start..start + ch.len_utf8()]);
            }
        }
    })
}

/// Compare exact text while cheaply skipping immutable chunks. The limits
/// bound both decoding and metadata work even for unshared replacement Ropes.
fn equal_edge<'a>(
    mut before: impl Iterator<Item = &'a str>,
    mut after: impl Iterator<Item = &'a str>,
    reverse: bool,
    chunks: &mut usize,
    text: &mut usize,
) -> Option<usize> {
    let mut left = before.next().unwrap_or("").as_bytes();
    let mut right = after.next().unwrap_or("").as_bytes();
    let mut equal = 0;
    while !left.is_empty() && !right.is_empty() {
        *chunks = chunks.checked_sub(1)?;
        let count = left.len().min(right.len());
        let (a, b) = if reverse {
            (&left[left.len() - count..], &right[right.len() - count..])
        } else {
            (&left[..count], &right[..count])
        };
        if a.as_ptr() != b.as_ptr() {
            *text = text.checked_sub(count)?;
            let matching = if reverse {
                a.iter()
                    .rev()
                    .zip(b.iter().rev())
                    .take_while(|(a, b)| a == b)
                    .count()
            } else {
                a.iter().zip(b.iter()).take_while(|(a, b)| a == b).count()
            };
            equal += matching;
            if matching != count {
                return Some(equal);
            }
        } else {
            equal += count;
        }
        // An edge can split a Unicode scalar when chunk boundaries differ.
        // Store byte remainders until a complete equal edge has been measured.
        if reverse {
            left = &left[..left.len() - count];
            right = &right[..right.len() - count];
        } else {
            left = &left[count..];
            right = &right[count..];
        }
        if left.is_empty() {
            left = before.next().unwrap_or("").as_bytes();
        }
        if right.is_empty() {
            right = after.next().unwrap_or("").as_bytes();
        }
    }
    Some(equal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn huge_shared_sources_cannot_exhaust_the_draw_thread_with_chunk_metadata_walks() {
        let old = Rope::from_str(&"ordinary retained text\n".repeat(160_000));
        let mut new = old.clone();
        new.insert(old.len_chars() / 2, "change");
        let visited = Cell::new(0usize);
        let mut chunks = MAX_CHUNKS;
        let mut text = MAX_TEXT;
        assert_eq!(
            equal_edge(
                old.chunks().inspect(|_| visited.set(visited.get() + 1)),
                new.chunks(),
                false,
                &mut chunks,
                &mut text,
            ),
            None
        );
        assert!(
            visited.get() <= MAX_CHUNKS + 2,
            "{} chunks visited",
            visited.get()
        );
        assert!(ScopeEdit::between(&old, &new).is_none());
    }

    #[test]
    fn streamed_regex_signatures_keep_control_words_and_ignore_identifier_contents() {
        assert!(
            regex_signature("let old = 12 + value;")
                .eq(regex_signature("let renamed = 567 + changed;"))
        );
        assert!(!regex_signature("return /x/").eq(regex_signature("renamed /x/")));
        assert!(!regex_signature("value = one;").eq(regex_signature("value += one;")));
    }
}
