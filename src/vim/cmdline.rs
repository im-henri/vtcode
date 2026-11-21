//! The `:`, `/` and `?` command line.

use super::exec::{self, MKind, cursor, push_history, push_jump, set_cursor, update_curswant};
use super::keys::{K, Key};
use super::parse::Cmd;
use super::text::{Pos, Text};
use super::{Host, Mode, VimState};

#[derive(Clone, Debug)]
pub(crate) struct Completion {
    pub(crate) items: Vec<String>,
    pub(crate) index: Option<usize>,
    /// Char index where the completed word starts.
    pub(crate) start: usize,
    pub(crate) original: String,
}

#[derive(Clone, Debug)]
pub(crate) struct CmdLine {
    pub(crate) kind: char,
    pub(crate) text: Vec<char>,
    pub(crate) cursor: usize,
    pub(crate) pending: Option<Cmd>,
    saved_cursor: Pos,
    saved_scroll: (usize, usize),
    hist_idx: Option<usize>,
    hist_prefix: String,
    pub(crate) awaiting_register: bool,
    awaiting_literal: bool,
    pub(crate) count: usize,
    pub(crate) completion: Option<Completion>,
    /// The match previewed by 'incsearch': (start, end) char offsets.
    pub(crate) preview: Option<(usize, usize)>,
    pub(crate) resume_insert: Option<super::insert::Resume>,
}

impl CmdLine {
    pub(crate) fn string(&self) -> String {
        self.text.iter().collect()
    }
}

pub(crate) fn open(
    vim: &mut VimState,
    host: &mut dyn Host,
    kind: char,
    pending: Option<Cmd>,
    init: String,
) {
    if vim.is_visual() && kind != ':' {
        // Searching from Visual mode extends the selection.
    }
    // `:` also works with no file open (Ex commands such as `:Explore` or
    // `:VT` reach the workbench); a search needs a buffer to search.
    let (saved_cursor, saved_scroll) = match host.ed_ref() {
        Some(ed) => (cursor(ed), (ed.scroll_offset, ed.wrap_scroll_offset)),
        None if kind == ':' => (Pos { line: 0, col: 0 }, (0, 0)),
        None => return,
    };
    let text: Vec<char> = init.chars().collect();
    let cursor_pos = text.len();
    vim.cmdline = Some(CmdLine {
        kind,
        text,
        cursor: cursor_pos,
        pending,
        saved_cursor,
        saved_scroll,
        hist_idx: None,
        hist_prefix: String::new(),
        awaiting_register: false,
        awaiting_literal: false,
        count: 1,
        completion: None,
        preview: None,
        resume_insert: vim.insert_oneshot.take(),
    });
    vim.cmdline_visual = if vim.is_visual() {
        Some(vim.mode)
    } else {
        None
    };
    vim.mode = Mode::Cmdline;
    vim.message = None;
}

fn close(vim: &mut VimState, host: &mut dyn Host) -> Option<CmdLine> {
    let c = vim.cmdline.take()?;
    vim.mode = match vim.cmdline_visual.take() {
        Some(m) => m,
        None => Mode::Normal,
    };
    if let Some(r) = c.resume_insert.clone() {
        vim.insert_oneshot = Some(r);
    }
    let _ = host;
    Some(c)
}

pub(crate) fn cancel(vim: &mut VimState, host: &mut dyn Host) {
    let Some(c) = close(vim, host) else {
        return;
    };
    if matches!(c.kind, '/' | '?') {
        if let Some(ed) = host.ed() {
            ed.cursor_line = c.saved_cursor.line;
            ed.cursor_col = c.saved_cursor.col;
            ed.scroll_offset = c.saved_scroll.0;
            ed.wrap_scroll_offset = c.saved_scroll.1;
        }
    }
    vim.incsearch = None;
    if vim.is_visual() {
        if let Some(ed) = host.ed() {
            exec::sync_visual(vim, ed);
        }
    } else if let Some(ed) = host.ed() {
        exec::clamp_normal(ed);
    }
    resume_after(vim, host);
}

/// Resume Insert mode after a command started with `<C-o>`.
fn resume_after(vim: &mut VimState, host: &mut dyn Host) {
    if vim.mode == Mode::Normal
        && let Some(r) = vim.insert_oneshot.take()
    {
        super::insert::resume(vim, host, r, None);
    }
}

fn word_before(text: &[char], cursor: usize) -> usize {
    let mut i = cursor;
    while i > 0 && text[i - 1] == ' ' {
        i -= 1;
    }
    let word = |c: char| c.is_alphanumeric() || c == '_';
    if i > 0 && word(text[i - 1]) {
        while i > 0 && word(text[i - 1]) {
            i -= 1;
        }
    } else if i > 0 {
        i -= 1;
    }
    i
}

pub(crate) fn key(vim: &mut VimState, host: &mut dyn Host, key: Key) {
    let Some(c) = vim.cmdline.as_mut() else {
        vim.mode = Mode::Normal;
        return;
    };
    if c.awaiting_literal {
        c.awaiting_literal = false;
        let ch = match key.k {
            K::Char(ch) if key.ctrl => ((ch.to_ascii_uppercase() as u8) & 0x1f) as char,
            K::Char(ch) => ch,
            K::Tab => '\t',
            K::Enter => '\r',
            K::Esc => '\x1b',
            _ => return,
        };
        c.text.insert(c.cursor, ch);
        c.cursor += 1;
        changed(vim, host);
        return;
    }
    if c.awaiting_register {
        c.awaiting_register = false;
        let insert: Option<String> = if key.is_ctrl('w') || key.is_ctrl('a') {
            host.ed_ref().and_then(|ed| {
                let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                let p = cursor(ed);
                let chars = t.line_chars(p.line);
                let is = |ch: char| {
                    if key.is_ctrl('w') {
                        vim.opts.iskeyword.is_word(ch)
                    } else {
                        !ch.is_whitespace()
                    }
                };
                let mut s = p.col.min(chars.len());
                while s < chars.len() && !is(chars[s]) {
                    s += 1;
                }
                let mut b = s;
                while b > 0 && is(chars[b - 1]) {
                    b -= 1;
                }
                let mut e = s;
                while e < chars.len() && is(chars[e]) {
                    e += 1;
                }
                (e > b).then(|| chars[b..e].iter().collect())
            })
        } else if key.is_ctrl('l') {
            host.ed_ref()
                .map(|ed| Text::of(&ed.rope, ed.vim_buf.trailing_empty).line_string(ed.cursor_line))
        } else if key.is_ctrl('r') || key.is_ctrl('o') || key.is_ctrl('p') {
            c.awaiting_register = true;
            return;
        } else if let Some(r) = key.char() {
            super::ops::read_reg(vim, host, Some(r))
                .map(|reg| reg.text.trim_end_matches('\n').replace('\n', "\r"))
        } else {
            None
        };
        if let (Some(s), Some(c)) = (insert, vim.cmdline.as_mut()) {
            for ch in s.chars() {
                c.text.insert(c.cursor, ch);
                c.cursor += 1;
            }
            changed(vim, host);
        }
        return;
    }
    let is_tab = key.k == K::Tab && !key.ctrl && !key.alt || key.is_ctrl('i');
    let is_backtab = key.k == K::BackTab;
    if !is_tab && !is_backtab && !(c.completion.is_some() && (key.is_ctrl('n') || key.is_ctrl('p')))
    {
        c.completion = None;
    }
    if key.is_escape() || key.is_ctrl('c') {
        cancel(vim, host);
        return;
    }
    if key == Key::ENTER || key.is_ctrl('j') || key.is_ctrl('m') {
        execute(vim, host);
        return;
    }
    let mut text_changed = false;
    match (key.k, key.ctrl, key.alt) {
        (K::Backspace, _, false) | (K::Char('h'), true, false) => {
            if c.text.is_empty() {
                cancel(vim, host);
                return;
            }
            if c.cursor > 0 {
                c.cursor -= 1;
                c.text.remove(c.cursor);
                text_changed = true;
            }
        }
        (K::Delete, false, false) => {
            if c.cursor < c.text.len() {
                c.text.remove(c.cursor);
                text_changed = true;
            } else if c.cursor > 0 {
                c.cursor -= 1;
                c.text.remove(c.cursor);
                text_changed = true;
            }
        }
        (K::Char('w'), true, false) => {
            let s = word_before(&c.text, c.cursor);
            c.text.drain(s..c.cursor);
            c.cursor = s;
            text_changed = true;
        }
        (K::Char('u'), true, false) => {
            c.text.drain(..c.cursor);
            c.cursor = 0;
            text_changed = true;
        }
        (K::Left, false, false) if !key.shift => c.cursor = c.cursor.saturating_sub(1),
        (K::Right, false, false) if !key.shift => c.cursor = (c.cursor + 1).min(c.text.len()),
        (K::Left, _, _) => c.cursor = word_before(&c.text, c.cursor),
        (K::Right, _, _) => {
            let mut i = c.cursor;
            while i < c.text.len() && c.text[i] != ' ' {
                i += 1;
            }
            while i < c.text.len() && c.text[i] == ' ' {
                i += 1;
            }
            c.cursor = i;
        }
        (K::Home, _, _) | (K::Char('b'), true, false) => c.cursor = 0,
        (K::End, _, _) | (K::Char('e'), true, false) => c.cursor = c.text.len(),
        (K::Up, false, _)
        | (K::Down, false, _)
        | (K::Char('p'), true, false)
        | (K::Char('n'), true, false) => {
            if c.completion.is_some() && key.ctrl {
                complete(vim, host, key.is_ctrl('n'));
                return;
            }
            let prefix_filter = matches!(key.k, K::Up | K::Down);
            history(vim, matches!(key.k, K::Up | K::Char('p')), prefix_filter);
            text_changed = true;
        }
        (K::Char('r'), true, false) => {
            c.awaiting_register = true;
            return;
        }
        (K::Char('v') | K::Char('q'), true, false) => {
            c.awaiting_literal = true;
            return;
        }
        (K::Char('g') | K::Char('t'), true, false) if matches!(c.kind, '/' | '?') => {
            step_preview(vim, host, key.is_ctrl('g'));
            return;
        }
        (K::Tab, false, false) | (K::Char('i'), true, false) | (K::BackTab, _, _) => {
            if matches!(c.kind, '/' | '?') {
                if !is_backtab {
                    c.text.insert(c.cursor, '\t');
                    c.cursor += 1;
                    text_changed = true;
                } else {
                    return;
                }
            } else {
                complete(vim, host, !is_backtab);
                return;
            }
        }
        (K::Char(ch), false, false) => {
            c.text.insert(c.cursor, ch);
            c.cursor += 1;
            text_changed = true;
            c.hist_idx = None;
        }
        _ => return,
    }
    if text_changed {
        changed(vim, host);
    }
}

fn changed(vim: &mut VimState, host: &mut dyn Host) {
    let Some(c) = vim.cmdline.as_mut() else {
        return;
    };
    if !c.text.is_empty() || c.hist_idx.is_none() {
        // Typing resets the history position.
    }
    if matches!(c.kind, '/' | '?') && vim.opts.incsearch {
        preview(vim, host);
    }
}

/// Split `pattern/offset` at the first unescaped delimiter.
pub(crate) fn split_pattern(text: &str, delim: char) -> (String, Option<String>) {
    let mut pat = String::new();
    let mut chars = text.chars().peekable();
    let mut in_class = false;
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            pat.push(ch);
            if let Some(n) = chars.next() {
                pat.push(n);
            }
            continue;
        }
        if ch == '[' {
            in_class = true;
        }
        if ch == ']' {
            in_class = false;
        }
        if ch == delim && !in_class {
            return (pat, Some(chars.collect()));
        }
        pat.push(ch);
    }
    (pat, None)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Offset {
    #[default]
    None,
    Lines(isize),
    End(isize),
    Start(isize),
}

pub(crate) fn parse_offset(s: &str) -> Offset {
    let s = s.trim();
    if s.is_empty() {
        return Offset::None;
    }
    let num = |t: &str| -> isize {
        if t.is_empty() {
            return 0;
        }
        if t == "+" {
            return 1;
        }
        if t == "-" {
            return -1;
        }
        t.parse().unwrap_or(0)
    };
    match s.chars().next() {
        Some('e') => Offset::End(num(&s[1..])),
        Some('s' | 'b') => Offset::Start(num(&s[1..])),
        Some('+' | '-' | '0'..='9') => Offset::Lines(if s.chars().all(|c| c.is_ascii_digit()) {
            s.parse().unwrap_or(0)
        } else {
            num(s)
        }),
        _ => Offset::None,
    }
}

fn step(t: &Text<'_>, p: &mut Pos, n: isize) {
    for _ in 0..n.unsigned_abs() {
        let r = if n > 0 { t.incl(p) } else { t.decl(p) };
        if r == super::text::Step::Fail {
            break;
        }
    }
}

/// Apply a search offset to a match starting at `start` char offset.
pub(crate) fn apply_offset(t: &Text<'_>, start: usize, end: usize, off: Offset) -> (Pos, MKind) {
    match off {
        Offset::None => (t.pos(start), MKind::Exclusive),
        Offset::Lines(n) => {
            // "[num] lines downwards, in column 1".
            let line = (t.pos(start).line as isize + n).clamp(0, t.last_line() as isize) as usize;
            (Pos::new(line, 0), MKind::Line)
        }
        Offset::End(n) => {
            let mut p = t.pos(end.saturating_sub(1).max(start));
            step(t, &mut p, n);
            (p, MKind::Inclusive)
        }
        Offset::Start(n) => {
            let mut p = t.pos(start);
            step(t, &mut p, n);
            (
                p,
                if n > 0 {
                    MKind::Inclusive
                } else {
                    MKind::Exclusive
                },
            )
        }
    }
}

fn preview(vim: &mut VimState, host: &mut dyn Host) {
    let Some(c) = vim.cmdline.as_ref() else {
        return;
    };
    let forward = c.kind == '/';
    let (pat, _) = split_pattern(&c.string(), c.kind);
    let saved = c.saved_cursor;
    let saved_scroll = c.saved_scroll;
    let Some(ed) = host.ed() else {
        return;
    };
    ed.cursor_line = saved.line;
    ed.cursor_col = saved.col;
    ed.scroll_offset = saved_scroll.0;
    ed.wrap_scroll_offset = saved_scroll.1;
    if pat.is_empty() {
        vim.incsearch = None;
        if let Some(c) = vim.cmdline.as_mut() {
            c.preview = None;
        }
        return;
    }
    let Ok(re) = super::search::compile(&pat, &vim.search_options()) else {
        return;
    };
    let hay = exec::haystack(ed);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let from = t.idx(saved);
    let found = super::search::search(&ed.rope, &hay, &re, from, forward, vim.opts.wrapscan, false);
    vim.incsearch = Some(pat);
    if let Some(c) = vim.cmdline.as_mut() {
        c.preview = found.map(|(s, e, _)| (s, e));
    }
    if let Some((s, _, _)) = found {
        let p = t.pos(s);
        ed.cursor_line = p.line;
        ed.cursor_col = p.col;
        ed.ensure_visible();
    }
}

/// `<C-g>`/`<C-t>` while typing a search: move the preview between matches.
fn step_preview(vim: &mut VimState, host: &mut dyn Host, forward: bool) {
    let Some(c) = vim.cmdline.as_ref() else {
        return;
    };
    let (pat, _) = split_pattern(&c.string(), c.kind);
    let Some(ed) = host.ed() else {
        return;
    };
    let Ok(re) = super::search::compile(&pat, &vim.search_options()) else {
        return;
    };
    let hay = exec::haystack(ed);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let from = t.idx(cursor(ed));
    if let Some((s, e, _)) =
        super::search::search(&ed.rope, &hay, &re, from, forward, vim.opts.wrapscan, false)
    {
        let p = t.pos(s);
        ed.cursor_line = p.line;
        ed.cursor_col = p.col;
        ed.ensure_visible();
        if let Some(c) = vim.cmdline.as_mut() {
            c.preview = Some((s, e));
            c.saved_cursor = p;
        }
    }
}

fn history(vim: &mut VimState, older: bool, prefix: bool) {
    let Some(c) = vim.cmdline.as_mut() else {
        return;
    };
    let list = if c.kind == ':' {
        &vim.history_cmd
    } else {
        &vim.history_search
    };
    if list.is_empty() {
        return;
    }
    if c.hist_idx.is_none() {
        c.hist_prefix = if prefix { c.string() } else { String::new() };
    }
    let mut idx = c.hist_idx.unwrap_or(list.len());
    loop {
        if older {
            if idx == 0 {
                return;
            }
            idx -= 1;
        } else {
            idx += 1;
            if idx >= list.len() {
                c.hist_idx = None;
                c.text = c.hist_prefix.chars().collect();
                c.cursor = c.text.len();
                return;
            }
        }
        if list[idx].starts_with(&c.hist_prefix) {
            break;
        }
    }
    c.hist_idx = Some(idx);
    c.text = list[idx].chars().collect();
    c.cursor = c.text.len();
}

fn complete(vim: &mut VimState, host: &mut dyn Host, forward: bool) {
    let Some(c) = vim.cmdline.as_mut() else {
        return;
    };
    if let Some(comp) = &mut c.completion {
        if comp.items.is_empty() {
            return;
        }
        let n = comp.items.len();
        comp.index = Some(match comp.index {
            None => {
                if forward {
                    0
                } else {
                    n - 1
                }
            }
            Some(i) if forward => {
                if i + 1 >= n {
                    usize::MAX
                } else {
                    i + 1
                }
            }
            Some(i) => {
                if i == 0 {
                    usize::MAX
                } else {
                    i - 1
                }
            }
        });
        let word: String = match comp.index {
            Some(usize::MAX) => {
                comp.index = None;
                comp.original.clone()
            }
            Some(i) => comp.items[i].clone(),
            None => comp.original.clone(),
        };
        let start = comp.start;
        c.text.truncate(start);
        c.text.extend(word.chars());
        c.cursor = c.text.len();
        return;
    }
    let before: String = c.text[..c.cursor].iter().collect();
    let (start, items) = super::ex::completions(vim, host, &before);
    let Some(c) = vim.cmdline.as_mut() else {
        return;
    };
    if items.is_empty() {
        return;
    }
    let original: String = c.text[start..c.cursor].iter().collect();
    c.text.truncate(c.cursor);
    c.completion = Some(Completion {
        items,
        index: None,
        start,
        original,
    });
    complete(vim, host, forward);
}

fn execute(vim: &mut VimState, host: &mut dyn Host) {
    let Some(c) = close(vim, host) else {
        return;
    };
    let text = c.string();
    vim.incsearch = None;
    match c.kind {
        ':' => {
            push_history(&mut vim.history_cmd, &text);
            if !text.trim().is_empty() {
                vim.regs.last_cmdline = text.clone();
            }
            if vim.is_visual() {
                exec::exit_visual(vim, host);
            }
            if let Err(e) = super::ex::run(vim, host, &text) {
                vim.set_error(e);
            }
            if vim.mode == Mode::Normal {
                if let Some(ed) = host.ed() {
                    exec::clamp_normal(ed);
                }
            }
            resume_after(vim, host);
        }
        _ => {
            let forward = c.kind == '/';
            let (mut pat, off) = split_pattern(&text, c.kind);
            let mut offset = off.as_deref().map(parse_offset).unwrap_or_default();
            if pat.is_empty() {
                match &vim.last_search {
                    Some((p, _)) => {
                        pat = p.clone();
                        if off.is_none() {
                            offset = vim.last_offset;
                        }
                    }
                    None => {
                        vim.set_error("E35: No previous regular expression");
                        return;
                    }
                }
            }
            push_history(&mut vim.history_search, &text);
            vim.last_search = Some((pat.clone(), forward));
            vim.last_offset = offset;
            vim.regs.last_search = pat.clone();
            vim.hl_hidden = false;
            if let Some(ed) = host.ed() {
                ed.cursor_line = c.saved_cursor.line;
                ed.cursor_col = c.saved_cursor.col;
            }
            let Some(target) = search_target(vim, host, &pat, forward, c.count.max(1), offset)
            else {
                if let Some(ed) = host.ed() {
                    ed.scroll_offset = c.saved_scroll.0;
                    ed.wrap_scroll_offset = c.saved_scroll.1;
                }
                if let Some(cmd) = &c.pending {
                    let _ = cmd;
                }
                if vim.is_visual() {
                    if let Some(ed) = host.ed() {
                        exec::sync_visual(vim, ed);
                    }
                }
                return;
            };
            if let Some(cmd) = c.pending {
                if let super::parse::Action::Op {
                    op,
                    force,
                    target: tg,
                    ..
                } = cmd.action.clone()
                {
                    super::ops::op_target(
                        vim,
                        host,
                        &cmd,
                        op,
                        force,
                        tg,
                        cmd.count1(),
                        cmd.has_count(),
                        Some((target.0, target.1, pat, forward)),
                    );
                }
            } else if let Some(ed) = host.ed() {
                push_jump(vim, ed);
                let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                let p = if vim.is_visual() {
                    t.clamp_insert(target.0)
                } else {
                    t.clamp_normal(target.0)
                };
                set_cursor(ed, p);
                update_curswant(vim, ed);
                if vim.is_visual() {
                    exec::sync_visual(vim, ed);
                }
            }
            resume_after(vim, host);
        }
    }
}

/// Run a search with an offset from the cursor.
pub(crate) fn search_target(
    vim: &mut VimState,
    host: &mut dyn Host,
    pat: &str,
    forward: bool,
    count: usize,
    offset: Offset,
) -> Option<(Pos, MKind)> {
    let origin = host.ed_ref().map(cursor)?;
    let first = search_target_once(vim, host, pat, forward, count, offset)?;
    if first.0 != origin || offset == Offset::None {
        return Some(first);
    }
    // Retry from the match itself.
    let p = exec::search_motion(vim, host, pat, forward, count, false)?;
    let ed = host.ed()?;
    let saved = cursor(ed);
    ed.cursor_line = p.line;
    ed.cursor_col = p.col;
    let r = search_target_once(vim, host, pat, forward, 1, offset);
    if let Some(ed) = host.ed() {
        ed.cursor_line = saved.line;
        ed.cursor_col = saved.col;
    }
    r.or(Some(first))
}

fn search_target_once(
    vim: &mut VimState,
    host: &mut dyn Host,
    pat: &str,
    forward: bool,
    count: usize,
    offset: Offset,
) -> Option<(Pos, MKind)> {
    let p = exec::search_motion(vim, host, pat, forward, count, false)?;
    let ed = host.ed_ref()?;
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let start = t.idx(p);
    let end = if matches!(offset, Offset::End(_)) {
        let re = super::search::compile(pat, &vim.search_options()).ok()?;
        let hay = exec::haystack(ed);
        let b = ed.rope.char_to_byte(start);
        re.find_iter(&hay)
            .find(|&(s, _)| s == b)
            .map(|(_, e)| ed.rope.byte_to_char(e))
            .unwrap_or(start + 1)
    } else {
        start + 1
    };
    Some(apply_offset(&t, start, end, offset))
}
