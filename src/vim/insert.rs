//! Insert and Replace mode.
//!
//! Ordinary typing goes through the workbench so completion, snippets,
//! auto-pairs and format-on-type keep working. On `<Esc>` the session's
//! edits are collapsed into one undo step and recorded as a text diff,
//! which `.`, counts and Visual-block insert replay exactly.

use ropey::Rope;

use super::exec::{Tx, clamp_normal, cursor, set_cursor, update_curswant};
use super::keys::{K, Key};
use super::parse::InsertKind;
use super::text::{Pos, Text};
use super::{Host, Mode, VimState};
use crate::editor::Editor;

/// Text inserted by a session, relative to where it started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Inserted {
    /// Offset of the changed range from the session start (may be < 0
    /// after backspacing over older text).
    pub(crate) offset: isize,
    /// Characters of the original text replaced.
    pub(crate) removed: usize,
    pub(crate) text: String,
    /// Cursor offset into `text` when the session ended.
    pub(crate) cursor: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct BlockInsert {
    /// Further lines to receive the text.
    pub(crate) lines: Vec<usize>,
    /// The block's top line and left display column (cursor after Esc).
    pub(crate) top: usize,
    pub(crate) left: usize,
    /// Display column of the insert, `usize::MAX` for `$A`.
    pub(crate) vcol: usize,
    pub(crate) append: bool,
    pub(crate) eol: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Await {
    Register,
    Literal,
    /// `<C-v>` followed by a character code: decimal `065`, `o101`,
    /// `x41`, `u00e9`, `U0001F600`.
    Code {
        radix: u32,
        max: u8,
        len: u8,
        value: u32,
    },
    Digraph(Option<char>),
    /// `<C-x>`: the completion sub-mode key comes next.
    CtrlX,
}

#[derive(Clone, Debug)]
pub(crate) struct Session {
    pub(crate) kind: InsertKind,
    pub(crate) count: usize,
    pub(crate) start_idx: usize,
    start_rope: Rope,
    boundary: Option<u64>,
    pre_saved: bool,
    pub(crate) block: Option<BlockInsert>,
    pub(crate) awaiting: Option<Await>,
    nav_id: u64,
    /// A line whose indent was inserted automatically (`cc`).
    pub(crate) auto_indent: Option<usize>,
}

#[derive(Clone, Debug)]
pub(crate) struct Resume {
    pub(crate) replace: bool,
    /// The line `<C-o>` was pressed on, when the cursor was past its end
    /// (Vim's `ins_at_eol`).
    pub(crate) eol_line: Option<usize>,
}

pub(crate) fn undo_back_id(ed: &Editor) -> Option<u64> {
    ed.undo_stack.back().map(|s| s.snapshot_id)
}

fn new_session(
    ed: &Editor,
    kind: InsertKind,
    count: usize,
    pre_saved: bool,
    block: Option<BlockInsert>,
) -> Session {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    Session {
        kind,
        count: count.max(1),
        start_idx: t.idx(t.clamp_insert(cursor(ed))),
        start_rope: ed.rope.clone(),
        boundary: undo_back_id(ed),
        pre_saved,
        block,
        awaiting: None,
        nav_id: ed.navigation_id,
        auto_indent: None,
    }
}

/// Enter Insert mode. `pre_saved` tells whether the command already made
/// an undo step (`cw` deleting text) that the typing belongs to.
pub(crate) fn begin(
    vim: &mut VimState,
    host: &mut dyn Host,
    kind: InsertKind,
    count: usize,
    pre_saved: bool,
    block: Option<BlockInsert>,
) {
    let Some(ed) = host.ed() else {
        return;
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let cur = t.clamp_insert(cursor(ed));
    let mut pre_saved = pre_saved;
    ed.selection_anchor = None;
    ed.vim_selection = None;
    ed.vim_block = None;
    ed.overwrite_mode = false;
    match kind {
        InsertKind::Before => set_cursor(ed, cur),
        InsertKind::After => {
            let col = if t.len(cur.line) == 0 {
                0
            } else {
                (cur.col + 1).min(t.len(cur.line))
            };
            set_cursor(ed, Pos::new(cur.line, col));
        }
        InsertKind::LineStart => set_cursor(ed, Pos::new(cur.line, t.first_nonblank(cur.line))),
        InsertKind::Column0 => set_cursor(ed, Pos::new(cur.line, 0)),
        InsertKind::LineEnd => set_cursor(ed, Pos::new(cur.line, t.len(cur.line))),
        InsertKind::LastInsert => {
            let p = ed.vim_buf.marks.get(&'^').copied().unwrap_or(cur);
            set_cursor(ed, t.clamp_insert(p));
        }
        InsertKind::OpenBelow | InsertKind::OpenAbove => {
            let before = undo_back_id(ed);
            if kind == InsertKind::OpenBelow {
                if cur.line >= t.last_line() && !rope_ends_with_eol(&ed.rope) {
                    // Last line without a line break: add one at the end.
                    let mut tx = Tx::default();
                    let indent = if vim.opts.autoindent {
                        super::indent_like(ed, &t, cur.line)
                    } else {
                        String::new()
                    };
                    let end = ed.rope.len_chars();
                    tx.edit(ed, end, end, &format!("\n{indent}"));
                    tx.finish(ed);
                    let line = cur.line + 1;
                    set_cursor(ed, Pos::new(line, indent.chars().count()));
                } else if cur.line >= t.last_line() && rope_ends_with_eol(&ed.rope) {
                    // The phantom line exists: open the new line before it.
                    let mut tx = Tx::default();
                    let indent = if vim.opts.autoindent {
                        super::indent_like(ed, &t, cur.line)
                    } else {
                        String::new()
                    };
                    let at = t.line_end(cur.line);
                    tx.edit(ed, at, at, &format!("\n{indent}"));
                    tx.finish(ed);
                    set_cursor(ed, Pos::new(cur.line + 1, indent.chars().count()));
                } else {
                    ed.cursor_col = cur.col;
                    host.open_line(false, vim.opts.autoindent);
                }
            } else {
                ed.cursor_col = cur.col;
                host.open_line(true, vim.opts.autoindent);
            }
            let Some(ed) = host.ed() else {
                return;
            };
            pre_saved = undo_back_id(ed) != before;
        }
    }
    let ed = host.ed().unwrap();
    let mut session = new_session(ed, kind, count, pre_saved, block);
    if matches!(kind, InsertKind::OpenBelow | InsertKind::OpenAbove) {
        session.auto_indent = Some(ed.cursor_line);
    }
    vim.insert = Some(session);
    vim.mode = Mode::Insert;
    vim.replace_stack.clear();
}

/// Expand tabs to spaces as if typed at the cursor with 'expandtab'.
fn expand_tabs_at(ed: &Editor, text: &str) -> String {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let p = t.clamp_insert(cursor(ed));
    let ts = ed.tab_size.max(1);
    let mut vcol = t.vcol(p.line, p.col, ts);
    let mut out = String::new();
    for c in text.chars() {
        match c {
            '\t' => {
                let n = ts - vcol % ts;
                out.push_str(&" ".repeat(n));
                vcol += n;
            }
            '\n' => {
                out.push(c);
                vcol = 0;
            }
            c => {
                out.push(c);
                vcol += super::text::char_width(c, vcol, ts);
            }
        }
    }
    out
}

fn rope_ends_with_eol(rope: &Rope) -> bool {
    rope.len_chars() > 0 && matches!(rope.char(rope.len_chars() - 1), '\n' | '\r')
}

pub(crate) fn begin_replace(vim: &mut VimState, host: &mut dyn Host, count: usize) {
    begin(vim, host, InsertKind::Before, count, false, None);
    vim.mode = Mode::Replace;
}

/// Restart the session after cursor movement (arrow keys), like Vim's
/// undo break on `<Left>`.
fn restart(vim: &mut VimState, host: &mut dyn Host) {
    let mode = vim.mode;
    finish_session(vim, host, false, false);
    if let Some(ed) = host.ed() {
        vim.insert = Some(new_session(ed, InsertKind::Before, 1, false, None));
    }
    vim.mode = mode;
}

fn native(vim: &mut VimState, host: &mut dyn Host, key: Key) {
    host.native_key(key);
    // The workbench may have switched files or left the editor.
    let same = host.ed_ref().is_some_and(|ed| {
        vim.insert
            .as_ref()
            .is_some_and(|s| s.nav_id == ed.navigation_id)
    });
    if !same && vim.is_insert() {
        vim.insert = None;
        vim.mode = Mode::Normal;
        if let Some(ed) = host.ed() {
            clamp_normal(ed);
        }
    }
}

pub(crate) fn key(vim: &mut VimState, host: &mut dyn Host, key: Key) {
    if let Some(aw) = vim.insert.as_ref().and_then(|s| s.awaiting) {
        if let Some(s) = &mut vim.insert {
            s.awaiting = None;
        }
        match aw {
            Await::Register => {
                if key.is_escape() {
                    return;
                }
                if key.is_ctrl('r') || key.is_ctrl('o') || key.is_ctrl('p') {
                    if let Some(s) = &mut vim.insert {
                        s.awaiting = Some(Await::Register);
                    }
                    return;
                }
                if key.is_ctrl('w') {
                    let word = host.ed_ref().and_then(|ed| ed.word_at_cursor());
                    if let Some(w) = word {
                        insert_text(host, &w);
                    }
                    return;
                }
                if let Some(r) = key.char() {
                    if r == '=' {
                        let mode = vim.mode;
                        super::cmdline::open_expr(
                            vim,
                            host,
                            super::cmdline::ExprTarget::Insert(mode),
                        );
                        return;
                    }
                    match super::ops::read_reg(vim, host, Some(r)) {
                        Some(reg) => {
                            let mut text = reg.text;
                            if reg.kind == super::register::RegKind::Line && !text.ends_with('\n') {
                                text.push('\n');
                            }
                            if let Some(ed) = host.ed()
                                && !ed.use_tabs
                                && text.contains('\t')
                            {
                                text = expand_tabs_at(ed, &text);
                            }
                            insert_text(host, &text);
                        }
                        None => {}
                    }
                }
            }
            Await::Code {
                radix,
                max,
                len,
                value,
            } => {
                let digit = key
                    .char()
                    .filter(|_| !key.ctrl && !key.alt)
                    .and_then(|c| c.to_digit(radix));
                if let Some(d) = digit {
                    let value = value * radix + d;
                    let len = len + 1;
                    // Decimal stops before 255 would overflow, like Vim.
                    if len >= max || (radix == 10 && value * 10 > 255) {
                        if let Some(ch) = char::from_u32(value) {
                            type_char(vim, host, ch);
                        }
                    } else if let Some(s) = &mut vim.insert {
                        s.awaiting = Some(Await::Code {
                            radix,
                            max,
                            len,
                            value,
                        });
                    }
                    return;
                }
                // Another key ends the code: insert what was typed, then
                // handle the key as usual.
                if len > 0 {
                    if let Some(ch) = char::from_u32(value) {
                        type_char(vim, host, ch);
                    }
                } else {
                    let lead = match (radix, max) {
                        (8, _) => 'o',
                        (16, 2) => 'x',
                        (16, 4) => 'u',
                        _ => 'U',
                    };
                    type_char(vim, host, lead);
                }
                self::key(vim, host, key);
                return;
            }
            Await::CtrlX => {
                use super::complete::Kind;
                if key.is_ctrl('n') || key.is_ctrl('p') {
                    super::complete::start(vim, host, Kind::Keyword, key.is_ctrl('n'));
                } else if key.is_ctrl('l') {
                    super::complete::start(vim, host, Kind::Line, false);
                } else if key.is_ctrl('f') {
                    super::complete::start(vim, host, Kind::File, true);
                } else if key.is_ctrl('o') || key.is_ctrl('u') {
                    // Omni completion is the language server's.
                    let _ = host.act(super::Act::Command("vim_complete".into()));
                } else if key.is_ctrl('e') || key.is_ctrl('y') {
                    copy_from_adjacent(host, key.is_ctrl('e'));
                } else if !key.is_escape() {
                    self::key(vim, host, key);
                }
                return;
            }
            Await::Literal => {
                if !key.ctrl && !key.alt {
                    let code = match key.char() {
                        Some(c @ '0'..='9') => Some((10, 3, 1, c as u32 - '0' as u32)),
                        Some('o' | 'O') => Some((8, 3, 0, 0)),
                        Some('x' | 'X') => Some((16, 2, 0, 0)),
                        Some('u') => Some((16, 4, 0, 0)),
                        Some('U') => Some((16, 8, 0, 0)),
                        _ => None,
                    };
                    if let Some((radix, max, len, value)) = code {
                        if let Some(s) = &mut vim.insert {
                            s.awaiting = Some(Await::Code {
                                radix,
                                max,
                                len,
                                value,
                            });
                        }
                        return;
                    }
                }
                let s = match key.k {
                    K::Char(c) if key.ctrl => ((c.to_ascii_uppercase() as u8) & 0x1f) as char,
                    K::Char(c) => c,
                    K::Tab => '\t',
                    K::Enter => '\r',
                    K::Esc => '\x1b',
                    K::Backspace => '\x08',
                    _ => return,
                };
                insert_text(host, &s.to_string());
            }
            Await::Digraph(first) => {
                let Some(c) = key.char() else {
                    return;
                };
                match first {
                    None => {
                        if let Some(s) = &mut vim.insert {
                            s.awaiting = Some(Await::Digraph(Some(c)));
                        }
                    }
                    Some(a) => {
                        let ch = super::ex::digraph(a, c).unwrap_or(c);
                        type_char(vim, host, ch);
                    }
                }
            }
        }
        return;
    }
    if vim.abbrev_block > 0 {
        vim.abbrev_block -= 1;
    }
    if vim.ins_complete.is_some() {
        if key.is_ctrl('n') || key.is_ctrl('p') {
            super::complete::cycle(vim, host, key.is_ctrl('n'));
            dismiss_popups(host);
            return;
        }
        if key.is_ctrl('e') {
            super::complete::cancel(vim, host);
            return;
        }
        super::complete::accept(vim);
        if key.is_ctrl('y') {
            return;
        }
    }
    if vim.mode == Mode::Insert && abbrev_trigger(vim, key) {
        let min = vim.insert.as_ref().map_or(0, |s| s.start_idx);
        let end_only = key.is_ctrl(']');
        let trigger = (!end_only).then_some(key);
        match super::complete::expand_insert(vim, host, min, trigger) {
            super::complete::Expanded::Queued => return,
            _ if end_only => return,
            _ => {}
        }
    }
    // Enter in the command-line window runs the line, also from Insert mode.
    if key == Key::ENTER && host.ed_ref().is_some_and(|ed| ed.vim_buf.cmdwin.is_some()) {
        finish(vim, host, false);
        super::cmdline::window_enter(vim, host);
        return;
    }
    if key.is_escape() || (key.is_ctrl('c') && vim.opts.ctrl_keys) {
        if host.completion_visible() {
            host.native_key(Key::ESC);
        }
        finish(vim, host, true);
        return;
    }
    if key.ctrl && !key.alt && !key.shift && vim.opts.ctrl_keys {
        let handled = match key.k {
            K::Char('o') => {
                let replace = vim.mode == Mode::Replace;
                // As in Vim, the Normal-mode cursor steps back onto the
                // last character, but the column Insert mode was at stays
                // the desired column for the command.
                let before = host.ed_ref().map(|ed| {
                    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                    let p = cursor(ed);
                    (
                        p.line,
                        p.col >= t.len(p.line),
                        t.vcol(p.line, p.col, super::exec::tabstop(ed)),
                    )
                });
                finish_session(vim, host, false, true);
                vim.mode = Mode::Normal;
                if let Some(ed) = host.ed() {
                    let len = Text::of(&ed.rope, ed.vim_buf.trailing_empty).len(ed.cursor_line);
                    if len > 0 && ed.cursor_col >= len {
                        ed.cursor_col = len - 1;
                    }
                }
                if let Some((_, _, vcol)) = before {
                    vim.curswant = Some(vcol);
                }
                vim.curswant_eol = false;
                let eol_line = before.and_then(|(line, eol, _)| eol.then_some(line));
                vim.insert_oneshot = Some(Resume { replace, eol_line });
                true
            }
            K::Char('w') => {
                delete_word_before(vim, host);
                true
            }
            K::Char('u') => {
                delete_line_before(host);
                true
            }
            K::Char('h') => {
                backspace(vim, host);
                true
            }
            K::Char('t') | K::Char('d') => {
                shift_current(vim, host, key.k == K::Char('t'));
                true
            }
            K::Char('r') => {
                if let Some(s) = &mut vim.insert {
                    s.awaiting = Some(Await::Register);
                }
                true
            }
            K::Char('v') | K::Char('q') => {
                if let Some(s) = &mut vim.insert {
                    s.awaiting = Some(Await::Literal);
                }
                true
            }
            K::Char('k') => {
                if let Some(s) = &mut vim.insert {
                    s.awaiting = Some(Await::Digraph(None));
                }
                true
            }
            K::Char('a') => {
                let t = vim.regs.last_inserted.clone();
                insert_text(host, &t);
                true
            }
            K::Char('@') => {
                let t = vim.regs.last_inserted.clone();
                insert_text(host, &t);
                finish(vim, host, true);
                true
            }
            K::Char('e') | K::Char('y') => {
                if host.completion_visible() {
                    if key.k == K::Char('e') {
                        host.native_key(Key::ESC);
                    } else {
                        host.native_key(Key::plain(K::Tab));
                    }
                } else {
                    copy_from_adjacent(host, key.k == K::Char('e'));
                }
                true
            }
            K::Char('n') | K::Char('p') => {
                // Vim's keyword completion from the buffer, also while the
                // editor's suggestion popup is open (Tab and the arrows
                // still choose from that; <C-x><C-o> asks the language server).
                dismiss_popups(host);
                super::complete::start(
                    vim,
                    host,
                    super::complete::Kind::Keyword,
                    key.k == K::Char('n'),
                );
                dismiss_popups(host);
                true
            }
            K::Char('x') => {
                if let Some(s) = &mut vim.insert {
                    s.awaiting = Some(Await::CtrlX);
                }
                true
            }
            K::Char('j') | K::Char('m') => {
                native(vim, host, Key::ENTER);
                true
            }
            K::Char('i') => {
                native(vim, host, Key::plain(K::Tab));
                true
            }
            K::Char('[') => {
                finish(vim, host, true);
                true
            }
            _ => false,
        };
        if handled {
            return;
        }
    }
    match key.k {
        K::Insert if !key.ctrl && !key.alt => {
            vim.mode = if vim.mode == Mode::Insert {
                Mode::Replace
            } else {
                Mode::Insert
            };
            return;
        }
        K::Left | K::Right | K::Up | K::Down | K::Home | K::End | K::PageUp | K::PageDown
            if !host.completion_visible()
                || matches!(key.k, K::Left | K::Right | K::Home | K::End) =>
        {
            native(vim, host, key);
            if vim.is_insert() {
                restart(vim, host);
            }
            return;
        }
        _ => {}
    }
    if vim.mode == Mode::Replace {
        if let Some(c) = key.char() {
            replace_char(vim, host, c);
            return;
        }
        if key.k == K::Backspace && !key.ctrl && !key.alt {
            replace_backspace(vim, host);
            return;
        }
    }
    native(vim, host, key);
}

fn dismiss_popups(host: &mut dyn Host) {
    if host.completion_visible() {
        let _ = host.act(super::Act::Command("vim_dismiss_popups".into()));
    }
}

/// Keys after which Vim checks for an abbreviation: a non-keyword
/// character, Enter, Tab, Esc and `<C-]>`.
fn abbrev_trigger(vim: &VimState, key: Key) -> bool {
    if vim.abbrevs.is_empty() || key.alt {
        return false;
    }
    if key.is_escape() || key.is_ctrl(']') || key == Key::ENTER {
        return true;
    }
    if key.ctrl {
        return false;
    }
    match key.k {
        K::Tab => true,
        K::Char(c) => !vim.opts.iskeyword.is_word(c),
        _ => false,
    }
}

fn type_char(vim: &mut VimState, host: &mut dyn Host, c: char) {
    if vim.mode == Mode::Replace {
        replace_char(vim, host, c);
    } else {
        insert_text(host, &c.to_string());
    }
}

/// Insert text literally at the cursor (no auto-pairs or completion).
pub(crate) fn insert_text(host: &mut dyn Host, text: &str) {
    let Some(ed) = host.ed() else {
        return;
    };
    if text.is_empty() {
        return;
    }
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let at = t.idx(t.clamp_insert(cursor(ed)));
    let mut tx = Tx::default();
    tx.edit(ed, at, at, text);
    tx.finish(ed);
    let n = text.chars().count()
        + if super::exec::doc_eol(ed) == "\r\n" {
            text.matches('\n').count()
        } else {
            0
        };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    set_cursor(ed, t.pos(at + n));
}

fn replace_char(vim: &mut VimState, host: &mut dyn Host, c: char) {
    let Some(ed) = host.ed() else {
        return;
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let p = t.clamp_insert(cursor(ed));
    let at = t.idx(p);
    let old = t.char_at(p);
    let mut tx = Tx::default();
    tx.edit(ed, at, at + usize::from(old.is_some()), &c.to_string());
    tx.finish(ed);
    vim.replace_stack.push(old);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    set_cursor(ed, t.pos(at + 1));
}

fn replace_backspace(vim: &mut VimState, host: &mut dyn Host) {
    let Some(ed) = host.ed() else {
        return;
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let p = t.clamp_insert(cursor(ed));
    let Some(old) = vim.replace_stack.pop() else {
        // Before the replaced text Backspace only moves the cursor.
        if p.col > 0 {
            set_cursor(ed, Pos::new(p.line, p.col - 1));
        }
        return;
    };
    if p.col == 0 {
        return;
    }
    let at = t.idx(p) - 1;
    let mut tx = Tx::default();
    match old {
        Some(c) => tx.edit(ed, at, at + 1, &c.to_string()),
        None => tx.edit(ed, at, at + 1, ""),
    }
    tx.finish(ed);
    set_cursor(ed, Pos::new(p.line, p.col - 1));
}

fn backspace(vim: &mut VimState, host: &mut dyn Host) {
    if vim.mode == Mode::Replace {
        replace_backspace(vim, host);
    } else {
        native(vim, host, Key::plain(K::Backspace));
    }
}

/// `i_CTRL-W`: delete the word before the cursor.
fn delete_word_before(vim: &mut VimState, host: &mut dyn Host) {
    let kw = vim.opts.iskeyword.clone();
    let Some(ed) = host.ed() else {
        return;
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let p = t.clamp_insert(cursor(ed));
    let at = t.idx(p);
    if p.col == 0 {
        if p.line == 0 {
            return;
        }
        // Join with the previous line (backspace=eol).
        let mut tx = Tx::default();
        let prev_end = t.line_end(p.line - 1);
        let col = t.len(p.line - 1);
        tx.edit(ed, prev_end, at, "");
        tx.finish(ed);
        set_cursor(ed, Pos::new(p.line - 1, col));
        return;
    }
    let chars = t.line_chars(p.line);
    let mut i = p.col;
    while i > 0 && matches!(chars[i - 1], ' ' | '\t') {
        i -= 1;
    }
    if i > 0 {
        let class = kw.class(chars[i - 1], false);
        while i > 0 && kw.class(chars[i - 1], false) == class && !matches!(chars[i - 1], ' ' | '\t')
        {
            i -= 1;
        }
    }
    // Stop at the start of the insert if it is on this line and we have
    // not reached it yet (Vim deletes typed text first).
    if let Some(s) = &vim.insert {
        let sp = Text::new(&s.start_rope).pos(s.start_idx);
        if sp.line == p.line
            && sp.col < p.col
            && sp.col > i
            && Text::of(&ed.rope, ed.vim_buf.trailing_empty).idx(sp) == s.start_idx
        {
            i = sp.col;
        }
    }
    let mut tx = Tx::default();
    tx.edit(ed, at - (p.col - i), at, "");
    tx.finish(ed);
    set_cursor(ed, Pos::new(p.line, i));
}

/// `i_CTRL-U`: delete before the cursor, keeping the indent.
fn delete_line_before(host: &mut dyn Host) {
    let Some(ed) = host.ed() else {
        return;
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let p = t.clamp_insert(cursor(ed));
    let fnb = t.first_nonblank(p.line);
    let to = if p.col > fnb { fnb } else { 0 };
    if to == p.col {
        if p.col == 0 && p.line > 0 {
            let mut tx = Tx::default();
            let prev_end = t.line_end(p.line - 1);
            let at = t.idx(p);
            let col = t.len(p.line - 1);
            tx.edit(ed, prev_end, at, "");
            tx.finish(ed);
            set_cursor(ed, Pos::new(p.line - 1, col));
        }
        return;
    }
    let base = t.line_start(p.line);
    let mut tx = Tx::default();
    tx.edit(ed, base + to, base + p.col, "");
    tx.finish(ed);
    set_cursor(ed, Pos::new(p.line, to));
}

fn shift_current(vim: &mut VimState, host: &mut dyn Host, right: bool) {
    let Some(ed) = host.ed() else {
        return;
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let p = t.clamp_insert(cursor(ed));
    let mut tx = Tx::default();
    let sw = ed.tab_size.max(1);
    let old_vcol = t.vcol(p.line, p.col, sw);
    let chars = t.line_chars(p.line);
    let ws = chars.iter().take_while(|&&c| c == ' ' || c == '\t').count();
    let cur = t.vcol(p.line, ws, sw);
    let new = if right {
        (cur / sw + 1) * sw
    } else if cur % sw != 0 {
        cur / sw * sw
    } else {
        cur.saturating_sub(sw)
    };
    let ind = if ed.use_tabs {
        format!("{}{}", "\t".repeat(new / sw), " ".repeat(new % sw))
    } else {
        " ".repeat(new)
    };
    let base = t.line_start(p.line);
    if chars[..ws].iter().collect::<String>() != ind {
        tx.edit(ed, base, base + ws, &ind);
    }
    let _ = vim;
    tx.finish(ed);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let after = t.len(p.line);
    let new_ws = ind.chars().count();
    let col = if p.col <= ws {
        // In the indent: move by the change in width.
        let v = (t.vcol(p.line, 0, sw) as isize + p.col as isize).max(0) as usize;
        let _ = v;
        let target = (old_vcol as isize + new as isize - cur as isize).max(0) as usize;
        t.col_for_vcol(p.line, target, sw).min(new_ws)
    } else {
        (p.col as isize + new_ws as isize - ws as isize).max(0) as usize
    };
    set_cursor(ed, Pos::new(p.line, col.min(after)));
}

fn copy_from_adjacent(host: &mut dyn Host, below: bool) {
    let Some(ed) = host.ed() else {
        return;
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let p = t.clamp_insert(cursor(ed));
    let other = if below {
        p.line + 1
    } else {
        p.line.wrapping_sub(1)
    };
    if other >= t.lines() {
        return;
    }
    let ts = ed.tab_size.max(1);
    let v = t.vcol(p.line, p.col, ts);
    let col = t.col_for_vcol(other, v, ts);
    if let Some(c) = t.char_at(Pos::new(other, col)) {
        insert_text(host, &c.to_string());
    }
}

/// Byte-wise common prefix length of two ropes, in chars.
fn common_prefix(a: &Rope, b: &Rope) -> usize {
    let (mut ia, mut ib) = (a.chunks(), b.chunks());
    let (mut ca, mut cb): (&[u8], &[u8]) = (&[], &[]);
    let mut bytes = 0usize;
    loop {
        if ca.is_empty() {
            match ia.next() {
                Some(c) => ca = c.as_bytes(),
                None => break,
            }
        }
        if cb.is_empty() {
            match ib.next() {
                Some(c) => cb = c.as_bytes(),
                None => break,
            }
        }
        let n = ca.len().min(cb.len());
        let same = ca[..n]
            .iter()
            .zip(&cb[..n])
            .take_while(|(x, y)| x == y)
            .count();
        bytes += same;
        if same < n {
            break;
        }
        ca = &ca[n..];
        cb = &cb[n..];
    }
    // Back up to a char boundary present in both.
    let mut bytes = bytes.min(a.len_bytes()).min(b.len_bytes());
    while bytes > 0
        && (a.try_byte_to_char(bytes).is_err() || !is_boundary(a, bytes) || !is_boundary(b, bytes))
    {
        bytes -= 1;
    }
    a.byte_to_char(bytes)
}

fn is_boundary(r: &Rope, byte: usize) -> bool {
    if byte == 0 || byte >= r.len_bytes() {
        return true;
    }
    let c = r.byte_to_char(byte);
    r.char_to_byte(c) == byte
}

/// Common suffix length in chars, not overlapping `prefix`.
fn common_suffix(a: &Rope, b: &Rope, prefix: usize) -> usize {
    let max = (a.len_chars() - prefix).min(b.len_chars() - prefix);
    let mut n = 0;
    let mut ia = a.chars_at(a.len_chars()).reversed();
    let mut ib = b.chars_at(b.len_chars()).reversed();
    while n < max {
        match (ia.next(), ib.next()) {
            (Some(x), Some(y)) if x == y => n += 1,
            _ => break,
        }
    }
    n
}

/// Compute what a session inserted.
fn diff(s: &Session, ed: &Editor) -> Inserted {
    let before = &s.start_rope;
    let after = &ed.rope;
    let p = common_prefix(before, after);
    let suf = common_suffix(before, after, p);
    let b_end = before.len_chars() - suf;
    let a_end = after.len_chars() - suf;
    let text = super::exec::plain(&after.slice(p..a_end).to_string());
    let t = Text::new(after);
    let cur = t.idx(t.clamp_insert(cursor(ed)));
    let cursor_rel = cur.saturating_sub(p).min(a_end - p);
    // Count the cursor in `\n` units for CRLF documents.
    let cursor_rel = super::exec::plain(&after.slice(p..p + cursor_rel).to_string())
        .chars()
        .count();
    Inserted {
        offset: p as isize - s.start_idx as isize,
        removed: b_end - p,
        text,
        cursor: cursor_rel,
    }
}

/// Collapse the undo steps made during the session into one.
fn collapse_undo(s: &Session, ed: &mut Editor) {
    let caret = ed.caret_state();
    let mut deque = ed.undo_stack.write();
    let idx = s
        .boundary
        .and_then(|id| deque.iter().rposition(|u| u.snapshot_id == id));
    let keep = match idx {
        Some(i) => i + 1 + usize::from(!s.pre_saved),
        None => usize::from(!s.pre_saved).max(if s.pre_saved { 1 } else { 0 }),
    };
    if deque.len() > keep {
        deque.truncate(keep);
        // The merged step now ends where the whole insert session ended.
        if let Some(top) = deque.back_mut() {
            top.other_caret = Some(caret);
        }
    }
    drop(deque);
    ed.break_undo_group();
}

/// Apply recorded inserted text at the cursor (for `.` and counts).
fn apply_inserted(host: &mut dyn Host, ins: &Inserted) {
    let Some(ed) = host.ed() else {
        return;
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let cur = t.idx(t.clamp_insert(cursor(ed))) as isize;
    let start = (cur + ins.offset).max(0) as usize;
    let end = (start + ins.removed).min(ed.rope.len_chars());
    let mut tx = Tx::default();
    tx.edit(ed, start, end, &ins.text);
    tx.finish(ed);
    // Cursor position in rope chars (CRLF adds one per line break).
    let eol_extra = super::exec::doc_eol(ed).len() - 1;
    let prefix: String = ins.text.chars().take(ins.cursor).collect();
    let n = ins.cursor + prefix.matches('\n').count() * eol_extra;
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    set_cursor(ed, t.pos(start + n));
}

/// Replay a recorded insertion during `.`.
pub(crate) fn replay(vim: &mut VimState, host: &mut dyn Host, ins: &Inserted) {
    apply_inserted(host, ins);
    finish(vim, host, true);
}

/// End the session and return to Normal mode.
pub(crate) fn finish(vim: &mut VimState, host: &mut dyn Host, move_left: bool) {
    vim.ins_complete = None;
    finish_session(vim, host, move_left, true);
    vim.mode = Mode::Normal;
    vim.insert_oneshot = None;
    vim.replace_stack.clear();
    if let Some(ed) = host.ed() {
        ed.overwrite_mode = false;
        clamp_normal(ed);
        update_curswant(vim, ed);
    }
    let _ = host.act(super::Act::Command("vim_dismiss_popups".into()));
}

fn finish_session(vim: &mut VimState, host: &mut dyn Host, move_left: bool, final_part: bool) {
    let Some(s) = vim.insert.take() else {
        return;
    };
    let Some(ed) = host.ed() else {
        return;
    };
    if ed.navigation_id != s.nav_id {
        return;
    }
    let had_eol = super::ends_with_eol(&s.start_rope);
    if !(had_eol && ed.vim_buf.trailing_empty) {
        super::update_trailing(ed, had_eol);
    }
    let ins = diff(&s, ed);
    let replace = vim.mode == Mode::Replace;
    // Counts: repeat the inserted text (and for o/O the new line).
    if final_part && s.count > 1 && !ins.text.is_empty() {
        for _ in 1..s.count {
            let Some(ed) = host.ed() else {
                return;
            };
            match s.kind {
                InsertKind::OpenBelow | InsertKind::OpenAbove => {
                    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                    let line = ed.cursor_line;
                    let at = t.line_end(line);
                    let mut tx = Tx::default();
                    tx.edit(ed, at, at, &format!("\n{}", ins.text));
                    tx.finish(ed);
                    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                    set_cursor(ed, Pos::new(line + 1, t.len(line + 1)));
                }
                _ => {
                    let mut once = ins.clone();
                    if replace {
                        // Replace mode overwrites with each copy.
                        once.offset = 0;
                        once.removed = ins.text.chars().count().min(
                            Text::of(&ed.rope, ed.vim_buf.trailing_empty)
                                .len(ed.cursor_line)
                                .saturating_sub(ed.cursor_col),
                        );
                    } else {
                        once.offset = 0;
                        once.removed = 0;
                    }
                    once.cursor = once.text.chars().count();
                    apply_inserted(host, &once);
                }
            }
        }
    }
    let Some(ed) = host.ed() else {
        return;
    };
    // Visual-block insert: repeat on the other lines of the block.
    if final_part
        && let Some(block) = &s.block
        && !ins.text.is_empty()
        && !ins.text.contains('\n')
        && ins.removed == 0
    {
        let ts = ed.tab_size.max(1);
        let mut tx = Tx::default();
        for &line in block.lines.iter().rev() {
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            if line > t.last_line() {
                continue;
            }
            let width = t.line_vwidth(line, ts);
            let base = t.line_start(line);
            if block.vcol == usize::MAX || block.eol && block.append {
                let at = t.line_end(line);
                tx.edit(ed, at, at, &ins.text);
            } else if block.append {
                if width < block.vcol {
                    let at = t.line_end(line);
                    tx.edit(
                        ed,
                        at,
                        at,
                        &format!("{}{}", " ".repeat(block.vcol - width), ins.text),
                    );
                } else {
                    let col = t.col_for_vcol(line, block.vcol, ts);
                    tx.edit(ed, base + col, base + col, &ins.text);
                }
            } else if width >= block.vcol {
                let col = t.col_for_vcol(line, block.vcol, ts);
                tx.edit(ed, base + col, base + col, &ins.text);
            }
        }
        tx.finish(ed);
        // The cursor returns to the block's top-left corner.
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        let col = t.col_for_vcol(block.top, block.left, ts);
        set_cursor(ed, t.clamp_normal(Pos::new(block.top, col)));
    }
    if final_part {
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        let line = ed.cursor_line.min(t.last_line());
        let chars = t.line_chars(line);
        let fresh = matches!(s.kind, InsertKind::OpenBelow | InsertKind::OpenAbove)
            || ins.text.contains('\n')
            || s.auto_indent == Some(line);
        if fresh
            && !chars.is_empty()
            && chars.iter().all(|&c| c == ' ' || c == '\t')
            && ed.cursor_col >= chars.len()
            && (ins
                .text
                .ends_with(chars.iter().collect::<String>().as_str())
                || s.auto_indent == Some(line))
        {
            let base = t.line_start(line);
            let mut tx = Tx::default();
            tx.edit(ed, base, base + chars.len(), "");
            tx.finish(ed);
            ed.cursor_col = 0;
        }
    }
    collapse_undo(&s, ed);
    // Marks and registers.
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let p = t.clamp_insert(cursor(ed));
    ed.vim_buf.marks.insert('^', p);
    if !ins.text.is_empty() || ins.removed > 0 {
        let start = t.pos((s.start_idx as isize + ins.offset).max(0) as usize);
        ed.vim_buf.marks.insert('.', start);
        ed.vim_buf.marks.insert('[', start);
        ed.vim_buf.marks.insert(']', p);
        let changes = &mut ed.vim_buf.changes;
        if changes.last().is_none_or(|c| c.line != start.line) {
            changes.push(start);
        }
        ed.vim_buf.change_idx = ed.vim_buf.changes.len();
    }
    if !ins.text.is_empty() {
        vim.regs.last_inserted = ins.text.clone();
    }
    if vim.replaying == 0
        && let Some(change) = &mut vim.last_change
    {
        if change.insert.is_none() || final_part {
            change.insert = Some(ins);
        }
    }
    if move_left && s.block.is_none() {
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        let p = t.clamp_insert(cursor(ed));
        if p.col > 0 {
            ed.cursor_col = p.col - 1;
        }
    }
}

/// Resume Insert mode after a `<C-o>` command.
/// Back to Insert mode after the `<C-o>` command. `before`: the cursor
/// before that command's last key.
pub(crate) fn resume(
    vim: &mut VimState,
    host: &mut dyn Host,
    r: Resume,
    before: Option<(usize, usize)>,
) {
    vim.insert_oneshot = None;
    let Some(ed) = host.ed() else {
        return;
    };
    // Vim's edit() restart, which sees the cursor where the command left it.
    {
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        let p = cursor(ed);
        let len = t.len(p.line);
        // During <C-o> Vim lets the cursor rest past the end of the line: a
        // command that deleted from the cursor to the end (`x`, `dw`, `D`)
        // leaves it there, where our Normal mode stepped back onto the
        // last character.
        let rested_past_end = before
            .is_some_and(|(line, col)| line == p.line && col == len && len > 0 && p.col + 1 == len);
        // Then, on the last character, continue after it when <C-o> was
        // pressed past the end of this line, or when the wanted column is
        // beyond the cursor (`$`, `j` onto a shorter line, a failed motion).
        let vcol = t.vcol(p.line, p.col, super::exec::tabstop(ed));
        let wanted_beyond = vim.curswant.is_some_and(|w| w > vcol);
        if rested_past_end
            || ((r.eol_line == Some(p.line) || wanted_beyond) && len > 0 && p.col + 1 == len)
        {
            ed.cursor_col = len;
        }
    }
    vim.insert = Some(new_session(ed, InsertKind::Before, 1, false, None));
    vim.mode = if r.replace {
        Mode::Replace
    } else {
        Mode::Insert
    };
}
