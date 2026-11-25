//! vim-surround's keys, which most long-time Vim users have in their
//! fingers: `ds{c}` deletes a surrounding pair, `cs{from}{to}` changes it,
//! `ys{motion}{c}` and `yss{c}` add one. Plain Vim gives these keys no
//! meaning (`s` is not a motion), so nothing else is shadowed.
//!
//! Targets: `( ) b`, `[ ] r`, `{ } B`, `< > a`, quotes and any other
//! punctuation, and `t` for an HTML/XML tag (`ds`/`cs` only). An opening
//! bracket as the new surrounding adds a space inside: `ysiw(` gives
//! `( word )`, `ysiw)` gives `(word)`.

use super::exec::Tx;
use super::keys::Key;
use super::parse::{self, Ctx, Parsed};
use super::text::{Pos, Text};
use super::{Host, VimState};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Pending {
    /// `ds`: the target comes next.
    Delete,
    /// `cs`: the target, then the replacement.
    Change(Option<char>),
    /// `ys`: the motion keys so far; then the surrounding char.
    Add { motion: Vec<Key>, complete: bool },
}

/// `d`, `c` or `y` is pending and `s` was typed: start surround.
pub(crate) fn begins(vim: &VimState, key: Key) -> Option<Pending> {
    if key != Key::ch('s') || vim.is_visual() {
        return None;
    }
    match vim.pending.as_slice() {
        [k] if *k == Key::ch('d') => Some(Pending::Delete),
        [k] if *k == Key::ch('c') => Some(Pending::Change(None)),
        [k] if *k == Key::ch('y') => Some(Pending::Add {
            motion: Vec::new(),
            complete: false,
        }),
        _ => None,
    }
}

/// The next key of a surround command.
pub(crate) fn key(vim: &mut VimState, host: &mut dyn Host, key: Key) {
    let Some(p) = vim.surround.take() else {
        return;
    };
    if key.is_escape() || key.is_ctrl('c') {
        return;
    }
    match p {
        Pending::Delete => {
            if let Some(c) = key.char() {
                run(vim, host, |vim, host| delete(vim, host, c, None));
            }
        }
        Pending::Change(None) => {
            if let Some(c) = key.char() {
                vim.surround = Some(Pending::Change(Some(c)));
            }
        }
        Pending::Change(Some(from)) => {
            if let Some(to) = key.char() {
                run(vim, host, |vim, host| delete(vim, host, from, Some(to)));
            }
        }
        Pending::Add {
            mut motion,
            complete,
        } => {
            if complete {
                if let Some(c) = key.char() {
                    run(vim, host, |vim, host| add(vim, host, &motion, c));
                }
                return;
            }
            // `yss`: the line.
            if motion.is_empty() && key == Key::ch('s') {
                vim.surround = Some(Pending::Add {
                    motion: vec![Key::ch('s')],
                    complete: true,
                });
                return;
            }
            motion.push(key);
            let mut probe = vec![Key::ch('y')];
            probe.extend(motion.iter().copied());
            match parse::parse(&probe, Ctx::Normal) {
                Parsed::Need => {
                    vim.surround = Some(Pending::Add {
                        motion,
                        complete: false,
                    })
                }
                Parsed::Bad => vim.beep(),
                _ => {
                    vim.surround = Some(Pending::Add {
                        motion,
                        complete: true,
                    })
                }
            }
        }
    }
}

fn run(
    vim: &mut VimState,
    host: &mut dyn Host,
    f: impl FnOnce(&mut VimState, &mut dyn Host) -> Result<(), String>,
) {
    if host.ed_ref().is_some_and(|ed| ed.read_only) {
        vim.set_error("E21: Cannot make changes, 'modifiable' is off");
        return;
    }
    if let Err(e) = f(vim, host) {
        vim.set_error(e);
    }
}

/// The open and close strings for a new surrounding.
fn pair(c: char) -> (String, String) {
    match c {
        '(' => ("( ".into(), " )".into()),
        ')' | 'b' => ("(".into(), ")".into()),
        '[' => ("[ ".into(), " ]".into()),
        ']' | 'r' => ("[".into(), "]".into()),
        '{' => ("{ ".into(), " }".into()),
        '}' | 'B' => ("{".into(), "}".into()),
        '<' | '>' | 'a' => ("<".into(), ">".into()),
        c => (c.to_string(), c.to_string()),
    }
}

/// The characters a target names: (open, close, text object key).
fn target(c: char) -> Option<(char, char, char)> {
    Some(match c {
        '(' | ')' | 'b' => ('(', ')', '('),
        '[' | ']' | 'r' => ('[', ']', '['),
        '{' | '}' | 'B' => ('{', '}', '{'),
        '<' | '>' | 'a' => ('<', '>', '<'),
        '"' | '\'' | '`' => (c, c, c),
        't' => ('<', '>', 't'),
        c if c.is_ascii_punctuation() => (c, c, c),
        _ => return None,
    })
}

/// Save the cursor, run keys, and give back the `<`/`>` (or `[`/`]`) marks.
fn select(
    vim: &mut VimState,
    host: &mut dyn Host,
    keys: &str,
    marks: (char, char),
) -> Option<(Pos, Pos)> {
    let saved = host.ed_ref().map(|ed| {
        (
            ed.cursor_line,
            ed.cursor_col,
            ed.vim_buf.marks.get(&marks.0).copied(),
            ed.vim_buf.marks.get(&marks.1).copied(),
        )
    })?;
    let saved_visual = host.ed_ref().and_then(|ed| ed.vim_buf.last_visual);
    if let Some(ed) = host.ed() {
        ed.vim_buf.last_visual = None;
    }
    if let Some(ed) = host.ed() {
        ed.vim_buf.marks.remove(&marks.0);
        ed.vim_buf.marks.remove(&marks.1);
    }
    let parsed = super::keys::parse(keys, &[]);
    let message = vim.message.take();
    vim.feed_keys(host, &parsed, false, false);
    if vim.is_visual() {
        super::exec::exit_visual(vim, host);
    }
    vim.message = message;
    let ed = host.ed()?;
    let got = if marks.0 == '<' {
        // The last Visual area stands for `'<` and `'>`.
        match ed.vim_buf.last_visual.take() {
            Some(v) => {
                let (a, b) = if (v.start.line, v.start.col) <= (v.end.line, v.end.col) {
                    (v.start, v.end)
                } else {
                    (v.end, v.start)
                };
                (Some(a), Some(b))
            }
            None => (None, None),
        }
    } else {
        (
            ed.vim_buf.marks.get(&marks.0).copied(),
            ed.vim_buf.marks.get(&marks.1).copied(),
        )
    };
    ed.cursor_line = saved.0;
    ed.cursor_col = saved.1;
    for (m, old) in [(marks.0, saved.2), (marks.1, saved.3)] {
        match old {
            Some(p) => {
                ed.vim_buf.marks.insert(m, p);
            }
            None => {
                ed.vim_buf.marks.remove(&m);
            }
        }
    }
    ed.vim_buf.last_visual = saved_visual;
    match got {
        (Some(a), Some(b)) => Some((a, b)),
        _ => None,
    }
}

/// `ds{c}` (`to` None) and `cs{c}{to}`.
fn delete(
    vim: &mut VimState,
    host: &mut dyn Host,
    c: char,
    to: Option<char>,
) -> Result<(), String> {
    let (open, close, obj) = target(c).ok_or("E474: Invalid surround target")?;
    let (a, b) = select(vim, host, &format!("va{obj}<Esc>"), ('<', '>'))
        .ok_or("E486: No surrounding pair found")?;
    let ed = host.ed().ok_or("")?;
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let (s, e) = (t.idx(a), t.idx(b) + 1);
    let area: Vec<char> = ed
        .rope
        .slice(s..e.min(ed.rope.len_chars()))
        .chars()
        .collect();
    // The pair inside the selection (quotes take surrounding white space).
    let (os, oe, cs, ce) = if obj == 't' {
        let oe = area
            .iter()
            .position(|&ch| ch == '>')
            .ok_or("E486: No tag found")?
            + 1;
        let cs = area
            .iter()
            .rposition(|&ch| ch == '<')
            .ok_or("E486: No tag found")?;
        (0, oe, cs, area.len())
    } else {
        let o = area
            .iter()
            .position(|&ch| ch == open)
            .ok_or("E486: No surrounding pair found")?;
        let cl = area
            .iter()
            .rposition(|&ch| ch == close)
            .ok_or("E486: No surrounding pair found")?;
        if cl <= o {
            return Err("E486: No surrounding pair found".into());
        }
        (o, o + 1, cl, cl + 1)
    };
    let (new_open, new_close) = match to {
        Some(to) => {
            let (mut o, mut c2) = pair(to);
            // An opening bracket replacing one keeps the inner spacing
            // the user typed only when asked for.
            if to == 't' {
                o = String::new();
                c2 = String::new();
            }
            (o, c2)
        }
        None => (String::new(), String::new()),
    };
    // Deleting a pair that had inner spaces from `( x )` keeps them, as
    // vim-surround does for `ds)`; `ds(` trims them.
    let (mut oe, mut cs) = (oe, cs);
    if to.is_none() && matches!(c, '(' | '[' | '{') {
        while oe < cs && area[oe] == ' ' {
            oe += 1;
        }
        while cs > oe && area[cs - 1] == ' ' {
            cs -= 1;
        }
    }
    let mut tx = Tx::default();
    tx.edit(ed, s + cs, s + ce, &new_close);
    tx.edit(ed, s + os, s + oe, &new_open);
    tx.finish(ed);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let p = t.clamp_normal(t.pos(s + os));
    super::exec::set_cursor(ed, p);
    super::exec::update_curswant(vim, ed);
    Ok(())
}

/// `ys{motion}{c}` and `yss{c}`.
fn add(vim: &mut VimState, host: &mut dyn Host, motion: &[Key], c: char) -> Result<(), String> {
    let (open, close) = pair(c);
    let ed = host.ed_ref().ok_or("")?;
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let (s, e) = if motion == [Key::ch('s')] {
        let line = ed.cursor_line.min(t.last_line());
        let first = t.first_nonblank(line);
        let len = t.len(line);
        let mut end = len;
        let chars = t.line_chars(line);
        while end > first && matches!(chars[end - 1], ' ' | '\t') {
            end -= 1;
        }
        (t.idx(Pos::new(line, first)), t.idx(Pos::new(line, end)))
    } else {
        let keys: String = std::iter::once("\"_y".to_string())
            .chain(motion.iter().map(|k| super::keys::format_key(*k)))
            .collect();
        let (a, b) = select(vim, host, &keys, ('[', ']')).ok_or("E20: Mark not set")?;
        let ed = host.ed_ref().ok_or("")?;
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        let s = t.idx(a);
        let mut e = (t.idx(b) + 1).min(ed.rope.len_chars());
        // The text, not the white space a word motion takes along.
        while e > s && matches!(ed.rope.char(e - 1), ' ' | '\t' | '\n' | '\r') {
            e -= 1;
        }
        (s, e)
    };
    let ed = host.ed().ok_or("")?;
    let mut tx = Tx::default();
    tx.edit(ed, e, e, &close);
    tx.edit(ed, s, s, &open);
    tx.finish(ed);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let p = t.clamp_normal(t.pos(s));
    super::exec::set_cursor(ed, p);
    super::exec::update_curswant(vim, ed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::vim::tests::run;

    fn after(text: &str, col: usize, keys: &str) -> String {
        run(text, (0, col), keys).lines.join("\n")
    }

    #[test]
    fn delete_change_and_add_surroundings() {
        assert_eq!(after("say \"hi there\" now", 6, "ds\""), "say hi there now");
        assert_eq!(after("f(a, b)", 3, "ds("), "fa, b");
        assert_eq!(after("f( a )", 3, "ds("), "fa");
        assert_eq!(after("f( a )", 3, "ds)"), "f a ");
        assert_eq!(after("x = \"a\"", 5, "cs\"'"), "x = 'a'");
        assert_eq!(after("[1, 2]", 1, "cs])"), "(1, 2)");
        assert_eq!(after("<b>bold</b>", 4, "dst"), "bold");
        assert_eq!(after("word here", 1, "ysiw)"), "(word) here");
        assert_eq!(after("word here", 1, "ysiw("), "( word ) here");
        assert_eq!(after("word here", 0, "ysw\""), "\"word\" here");
        assert_eq!(after("  call(x)  ", 3, "yss]"), "  [call(x)]  ");
        assert_eq!(after("a b", 0, "ysiw*"), "*a* b");
        // One undo step each.
        assert_eq!(after("word", 0, "ysiw)u"), "word");
        // Plain d, c, y keep working.
        assert_eq!(after("ab cd", 0, "dw"), "cd");
    }
}
