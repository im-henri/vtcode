//! Insert-mode completion the Vim way (`<C-n>`, `<C-p>`, `<C-x><C-l>`,
//! `<C-x><C-f>`) and abbreviations (`:iabbrev`, `:cabbrev`).
//!
//! Completion inserts the first match right away; `<C-n>`/`<C-p>` cycle
//! through the matches and back to the typed text, `<C-e>` restores it and
//! `<C-y>` (or any other key) keeps the match.

use super::exec::{Tx, cursor, set_cursor};
use super::keys::Key;
use super::text::{Pos, Text};
use super::{Host, VimState};
use crate::editor::Editor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Keyword,
    Line,
    File,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Keyword => "Keyword completion (^N^P)",
            Kind::Line => "Whole line completion (^L^N^P)",
            Kind::File => "File name completion (^F^N^P)",
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct InsComplete {
    kind: Kind,
    line: usize,
    start_col: usize,
    /// The text that was typed before completing.
    original: String,
    items: Vec<String>,
    /// `None`: the original text is shown.
    idx: Option<usize>,
}

/// Replace `line[start_col..cursor]` with `text` and put the cursor after it.
fn replace_before_cursor(ed: &mut Editor, line: usize, start_col: usize, text: &str) {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let p = t.clamp_insert(cursor(ed));
    if p.line != line {
        return;
    }
    let start = t.idx(Pos::new(line, start_col.min(p.col)));
    let end = t.idx(p);
    let mut tx = Tx::default();
    tx.edit(ed, start, end, text);
    tx.finish(ed);
    set_cursor(ed, Pos::new(line, start_col + text.chars().count()));
}

fn show(vim: &mut VimState) {
    let Some(c) = &vim.ins_complete else {
        return;
    };
    let state = match c.idx {
        Some(i) => format!("match {} of {}", i + 1, c.items.len()),
        None => "Back at original".into(),
    };
    vim.set_message(format!("-- {} {state}", c.kind.label()));
}

/// The matches, nearest first in the direction of `<C-n>`: words after the
/// cursor, then from the top down to it.
fn keyword_items(vim: &VimState, ed: &Editor, prefix: &str, start: Pos) -> Vec<String> {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let kw = &vim.opts.iskeyword;
    let ic = vim.opts.ignorecase;
    let matches_prefix = |w: &str| {
        if ic {
            w.to_lowercase().starts_with(&prefix.to_lowercase())
        } else {
            w.starts_with(prefix)
        }
    };
    let lines = t.lines();
    let mut seen = std::collections::HashSet::new();
    let mut after = Vec::new();
    let mut before = Vec::new();
    for l in 0..lines {
        let chars = t.line_chars(l);
        let mut i = 0;
        while i < chars.len() {
            if !kw.is_word(chars[i]) {
                i += 1;
                continue;
            }
            let s = i;
            while i < chars.len() && kw.is_word(chars[i]) {
                i += 1;
            }
            // The word being completed is not a match.
            if l == start.line && s == start.col {
                continue;
            }
            let w: String = chars[s..i].iter().collect();
            if w.chars().count() <= prefix.chars().count() || !matches_prefix(&w) {
                continue;
            }
            if (l, s) > (start.line, start.col) {
                after.push(w);
            } else {
                before.push(w);
            }
        }
        if after.len() + before.len() > 50_000 {
            break;
        }
    }
    after
        .into_iter()
        .chain(before)
        .filter(|w| seen.insert(w.clone()))
        .collect()
}

fn line_items(ed: &Editor, prefix: &str, line: usize) -> Vec<String> {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let mut seen = std::collections::HashSet::new();
    let order = (line + 1..t.lines()).chain(0..line);
    order
        .map(|l| t.line_string(l).trim_start().to_string())
        .filter(|s| s.len() > prefix.len() && s.starts_with(prefix))
        .filter(|s| seen.insert(s.clone()))
        .collect()
}

fn file_items(host: &dyn Host, typed: &str) -> Vec<String> {
    let norm = typed.replace('\\', "/");
    let (dir, partial) = match norm.rfind('/') {
        Some(i) => (&typed[..=i], &norm[i + 1..]),
        None => ("", norm.as_str()),
    };
    let base = host.cwd().unwrap_or_default();
    let expanded = if let Some(rest) = dir.strip_prefix('~') {
        super::paths::home()
            .map(|h| h.display().to_string() + rest)
            .unwrap_or_else(|| dir.to_string())
    } else {
        dir.to_string()
    };
    let path = std::path::Path::new(&expanded);
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let Ok(rd) = std::fs::read_dir(&full) else {
        return Vec::new();
    };
    let mut items: Vec<String> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            (name.starts_with(partial) && (partial.starts_with('.') || !name.starts_with('.')))
                .then(|| format!("{dir}{name}"))
        })
        .collect();
    items.sort();
    items
}

/// Start completing (`<C-n>` when `forward`, `<C-p>` otherwise).
pub(crate) fn start(vim: &mut VimState, host: &mut dyn Host, kind: Kind, forward: bool) {
    let Some(ed) = host.ed_ref() else {
        return;
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let p = t.clamp_insert(cursor(ed));
    let chars = t.line_chars(p.line);
    let mut s = p.col.min(chars.len());
    match kind {
        Kind::Keyword => {
            while s > 0 && vim.opts.iskeyword.is_word(chars[s - 1]) {
                s -= 1;
            }
        }
        Kind::Line => {
            s = chars
                .iter()
                .take_while(|c| matches!(c, ' ' | '\t'))
                .count()
                .min(p.col);
        }
        Kind::File => {
            while s > 0
                && !chars[s - 1].is_whitespace()
                && !"\"'`()[]{}<>,;=".contains(chars[s - 1])
            {
                s -= 1;
            }
        }
    }
    let original: String = chars[s..p.col.min(chars.len())].iter().collect();
    let items = match kind {
        Kind::Keyword => keyword_items(vim, ed, &original, Pos::new(p.line, s)),
        Kind::Line => line_items(ed, &original, p.line),
        Kind::File => file_items(host, &original),
    };
    if items.is_empty() {
        vim.set_message(format!("-- {} Pattern not found", kind.label()));
        vim.bell = Some(std::time::Instant::now());
        return;
    }
    let idx = if forward { 0 } else { items.len() - 1 };
    let text = items[idx].clone();
    vim.ins_complete = Some(InsComplete {
        kind,
        line: p.line,
        start_col: s,
        original,
        items,
        idx: Some(idx),
    });
    if let Some(ed) = host.ed() {
        replace_before_cursor(ed, p.line, s, &text);
    }
    show(vim);
}

/// `<C-n>`/`<C-p>` while completing.
pub(crate) fn cycle(vim: &mut VimState, host: &mut dyn Host, forward: bool) {
    let Some(c) = vim.ins_complete.as_mut() else {
        return;
    };
    let n = c.items.len();
    c.idx = match (c.idx, forward) {
        (None, true) => Some(0),
        (None, false) => Some(n - 1),
        (Some(i), true) if i + 1 < n => Some(i + 1),
        (Some(i), false) if i > 0 => Some(i - 1),
        _ => None,
    };
    let text = c.idx.map_or(c.original.clone(), |i| c.items[i].clone());
    let (line, start) = (c.line, c.start_col);
    if let Some(ed) = host.ed() {
        replace_before_cursor(ed, line, start, &text);
    }
    show(vim);
}

/// `<C-e>`: back to what was typed.
pub(crate) fn cancel(vim: &mut VimState, host: &mut dyn Host) {
    if let Some(c) = vim.ins_complete.take()
        && let Some(ed) = host.ed()
    {
        replace_before_cursor(ed, c.line, c.start_col, &c.original);
    }
    vim.message = None;
}

/// Keep the current match.
pub(crate) fn accept(vim: &mut VimState) {
    if vim.ins_complete.take().is_some() {
        vim.message = None;
    }
}

// ---------------------------------------------------------------------------
// Abbreviations

pub(crate) enum Expanded {
    No,
    /// The text was replaced; handle the key as usual.
    Done,
    /// The expansion was queued as keys, with the key after it.
    Queued,
}

fn find_abbrev<'a>(
    vim: &'a VimState,
    before: &[char],
    min_col: usize,
    insert: bool,
) -> Option<(&'a super::ex::Abbrev, usize)> {
    let kw = &vim.opts.iskeyword;
    vim.abbrevs
        .iter()
        .filter(|a| if insert { a.insert } else { a.cmdline })
        .filter_map(|a| {
            let lhs: Vec<char> = a.lhs.chars().collect();
            if lhs.is_empty() || lhs.len() > before.len() || !before.ends_with(&lhs) {
                return None;
            }
            let start = before.len() - lhs.len();
            if start < min_col {
                return None;
            }
            let prev = start.checked_sub(1).map(|i| before[i]);
            let full_id = lhs.iter().all(|&c| kw.is_word(c));
            let ok = match prev {
                None => true,
                Some(c) if full_id => !kw.is_word(c),
                // end-id and non-id abbreviations follow a blank.
                Some(c) => c == ' ' || c == '\t',
            };
            ok.then_some((a, start))
        })
        .max_by_key(|(a, _)| a.lhs.len())
}

fn rhs_text(vim: &mut VimState, host: &mut dyn Host, a: &super::ex::Abbrev) -> Option<String> {
    if a.expr {
        let mut cx = super::eval::Ctx::new(vim, host);
        return super::eval::eval(&a.rhs, &mut cx).ok().map(|v| v.to_str());
    }
    Some(a.rhs.clone())
}

/// Expand an Insert-mode abbreviation before the cursor. `trigger` is the
/// key that ended it (queued after a multi-key expansion).
pub(crate) fn expand_insert(
    vim: &mut VimState,
    host: &mut dyn Host,
    min_idx: usize,
    trigger: Option<Key>,
) -> Expanded {
    if vim.abbrev_block > 0 || !vim.abbrevs.iter().any(|a| a.insert) {
        return Expanded::No;
    }
    let Some(ed) = host.ed_ref() else {
        return Expanded::No;
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let p = t.clamp_insert(cursor(ed));
    let chars = t.line_chars(p.line);
    let before = &chars[..p.col.min(chars.len())];
    let line_start = t.idx(Pos::new(p.line, 0));
    let min_col = min_idx.saturating_sub(line_start);
    let Some((a, start)) = find_abbrev(vim, before, min_col, true) else {
        return Expanded::No;
    };
    let a = a.clone();
    let Some(rhs) = rhs_text(vim, host, &a) else {
        return Expanded::No;
    };
    let line = p.line;
    if let Some(ed) = host.ed() {
        replace_before_cursor(ed, line, start, "");
    }
    let keys = if a.expr {
        super::ops::text_to_keys(&rhs)
    } else {
        super::keys::parse(&rhs, &vim.leader.clone())
    };
    let plain = keys.iter().all(|k| k.char().is_some() && !k.ctrl && !k.alt);
    if plain {
        super::insert::insert_text(host, &rhs_plain(&keys));
        return Expanded::Done;
    }
    let mut queued = keys;
    queued.extend(trigger);
    vim.abbrev_block = queued.len();
    vim.queue_keys(&queued, !a.noremap, false);
    Expanded::Queued
}

fn rhs_plain(keys: &[Key]) -> String {
    keys.iter().filter_map(|k| k.char()).collect()
}

/// Expand a command-line abbreviation before the cursor.
pub(crate) fn expand_cmdline(vim: &mut VimState, host: &mut dyn Host) {
    if !vim.abbrevs.iter().any(|a| a.cmdline) {
        return;
    }
    let Some(c) = vim.cmdline.as_ref() else {
        return;
    };
    let before: Vec<char> = c.text[..c.cursor].to_vec();
    let Some((a, start)) = find_abbrev(vim, &before, 0, false) else {
        return;
    };
    let a = a.clone();
    let Some(rhs) = rhs_text(vim, host, &a) else {
        return;
    };
    let keys = if a.expr {
        super::ops::text_to_keys(&rhs)
    } else {
        super::keys::parse(&rhs, &[])
    };
    let text = rhs_plain(&keys);
    let Some(c) = vim.cmdline.as_mut() else {
        return;
    };
    let end = c.cursor;
    c.text.splice(start..end, text.chars());
    c.cursor = start + text.chars().count();
}

#[cfg(test)]
mod tests {
    use crate::vim::tests::run;

    #[test]
    fn keyword_completion_inserts_cycles_and_restores() {
        let o = run("foobar foobaz\nfo", (1, 0), "A<C-n><Esc>");
        assert_eq!(o.lines[1], "foobar");
        let o = run("foobar foobaz\nfo", (1, 0), "A<C-n><C-n><Esc>");
        assert_eq!(o.lines[1], "foobaz");
        let o = run("foobar foobaz\nfo", (1, 0), "A<C-p><Esc>");
        assert_eq!(o.lines[1], "foobaz");
        let o = run("foobar foobaz\nfo", (1, 0), "A<C-n><C-n><C-n><Esc>");
        assert_eq!(o.lines[1], "fo", "past the last match is the typed text");
        let o = run("foobar\nfo", (1, 0), "A<C-n><C-e><Esc>");
        assert_eq!(o.lines[1], "fo");
        let o = run("foobar\nfo", (1, 0), "A<C-n>!<Esc>");
        assert_eq!(o.lines[1], "foobar!");
        let o = run("    let total = 1;\nlet", (1, 0), "A<C-x><C-l><Esc>");
        assert_eq!(o.lines[1], "let total = 1;");
    }

    #[test]
    fn abbreviations_expand_in_insert_mode_and_on_the_command_line() {
        let o = run("x", (0, 0), ":iabbrev teh the<CR>A teh teh. tehx <Esc>");
        assert_eq!(o.lines[0], "x the the. tehx ");
        let o = run("x", (0, 0), ":iab sig -- <lt>CR>me<CR>A sig<Esc>");
        assert_eq!(o.lines, ["x -- ", "me"]);
        let o = run("a\nb", (0, 0), ":cabbrev D d<CR>:D<CR>");
        assert_eq!(o.lines, ["b"]);
        let o = run("x", (0, 0), ":iabbrev <lt>expr> yr 6*7<CR>A yr <Esc>");
        assert_eq!(o.lines[0], "x 42 ");
    }
}
