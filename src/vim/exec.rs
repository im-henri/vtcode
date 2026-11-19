//! Command execution: motions, Visual mode and simple commands. Operators
//! live in [`super::ops`]-style helpers at the bottom of this module.

use super::motion::{self, Walker};
use super::parse::{Action, Cmd, Motion, Obj, Op, Simple, Target};
use super::register::{Reg, RegKind};
use super::text::{Pos, Text};
use super::{textobj, Act, BlockArea, Host, Jump, LastVisual, Mode, VimState, VisualKind};
use crate::editor::Editor;

// ---------------------------------------------------------------------------
// Editing transaction: one undo step per Vim command.

#[derive(Default)]
pub(crate) struct Tx {
    pub(crate) saved: bool,
    begun: bool,
    had_eol: Option<bool>,
    pub(crate) changed: bool,
    /// Changed area for the `[` and `]` marks (char offsets after edits).
    pub(crate) first: Option<usize>,
    pub(crate) last: usize,
}

pub(crate) fn doc_eol(ed: &Editor) -> &'static str {
    match ed.cached_line_ending {
        "CRLF" => "\r\n",
        "CR" => "\r",
        _ => "\n",
    }
}

/// Buffer text with line endings normalized to `\n`.
pub(crate) fn plain(s: &str) -> String {
    if s.contains('\r') { s.replace("\r\n", "\n").replace('\r', "\n") } else { s.to_string() }
}

impl Tx {
    /// Replace chars `start..end` with `text` (`\n` line breaks).
    pub(crate) fn edit(&mut self, ed: &mut Editor, start: usize, end: usize, text: &str) {
        let len = ed.rope.len_chars();
        let (start, end) = (start.min(len), end.min(len).max(start.min(len)));
        if start == end && text.is_empty() { return; }
        let eol = doc_eol(ed);
        let text = if eol != "\n" && text.contains('\n') { text.replace('\n', eol) } else { text.to_string() };
        if !self.begun {
            ed.begin_document_edit();
            self.begun = true;
        }
        let sl = ed.rope.char_to_line(start);
        let el = ed.rope.char_to_line(end);
        let at_line_start = start == ed.rope.line_to_char(sl);
        let ends_at_line_start = end == ed.rope.line_to_char(el);
        if !self.saved {
            ed.save_undo_for_range(sl, el);
            ed.snippet_cancel();
            self.saved = true;
        } else {
            ed.content_version += 1;
            ed.cached_wrapped_total = None;
        }
        ed.desired_col = None;
        if self.had_eol.is_none() {
            self.had_eol = Some(super::ends_with_eol(&ed.rope));
        }
        if start < end { ed.rope.remove(start..end); }
        if !text.is_empty() { ed.rope.insert(start, &text); }
        // Judged against the buffer before the whole command.
        super::update_trailing(ed, self.had_eol.unwrap_or(false));
        let added = text.matches('\n').count() + if eol == "\r" { text.matches('\r').count() } else { 0 };
        let delta = added as isize - (el - sl) as isize;
        if delta != 0 {
            // Whole lines removed or inserted at a line start move the lines
            // from `sl`; otherwise lines after `sl` are joined/split.
            let whole = at_line_start && ends_at_line_start && (text.is_empty() || text.ends_with('\n'));
            // Also moves Vim marks with the text.
            ed.shift_bookmarks(if whole { sl } else { sl + 1 }, delta);
        }
        ed.mark_syntax_dirty_range(sl, sl + added);
        ed.modified = true;
        self.changed = true;
        let ins = text.chars().count();
        // Track the changed area in post-edit offsets.
        match self.first {
            None => { self.first = Some(start); self.last = start + ins; }
            Some(f) => {
                let removed = end - start;
                let shift = |o: usize| if o >= end { o + ins - removed } else if o > start { start + ins.min(o - start) } else { o };
                self.first = Some(f.min(start));
                self.last = shift(self.last).max(start + ins);
            }
        }
    }

    pub(crate) fn finish(self, ed: &mut Editor) {
        if let Some(had_eol) = self.had_eol {
            super::update_trailing(ed, had_eol);
            if ed.rope.len_chars() > 0 { ed.vim_buf.ml_empty = false; }
        }
        if self.changed {
            ed.clear_stale_folds();
            ed.refresh_modified_from_saved();
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            if let Some(f) = self.first {
                let start = t.pos(f);
                let mut end = t.pos(self.last.saturating_sub(usize::from(self.last > f)));
                end.col = end.col.min(t.max_normal_col(end.line));
                ed.vim_buf.marks.insert('[', start);
                ed.vim_buf.marks.insert(']', end);
                ed.vim_buf.marks.insert('.', start);
                let changes = &mut ed.vim_buf.changes;
                if changes.last().is_none_or(|p| p.line != start.line) {
                    changes.push(start);
                    if changes.len() > 100 { changes.remove(0); }
                } else if let Some(p) = changes.last_mut() {
                    *p = start;
                }
                ed.vim_buf.change_idx = ed.vim_buf.changes.len();
            }
        }
        if self.begun { ed.end_document_edit(); }
    }
}

// ---------------------------------------------------------------------------
// Cursor helpers

pub(crate) fn cursor(ed: &Editor) -> Pos { Pos::new(ed.cursor_line, ed.cursor_col) }

pub(crate) fn set_cursor(ed: &mut Editor, p: Pos) {
    ed.cursor_line = p.line;
    ed.cursor_col = p.col;
    ed.desired_col = None;
    ed.ensure_visible();
}

/// Keep the cursor off the end-of-line position and the phantom last line.
pub(crate) fn clamp_normal(ed: &mut Editor) {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let p = t.clamp_normal(cursor(ed));
    if p != cursor(ed) {
        ed.cursor_line = p.line;
        ed.cursor_col = p.col;
    }
}

fn tabstop(ed: &Editor) -> usize { ed.tab_size.max(1) }

/// Column for a vertical move to `line` keeping `curswant`.
fn col_for_curswant(vim: &VimState, ed: &Editor, line: usize, visual: bool) -> usize {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let want = vim.curswant.unwrap_or(0);
    if want == usize::MAX {
        return if visual && vim.curswant_eol { t.len(line) } else { t.max_normal_col(line) };
    }
    let col = t.col_for_vcol(line, want, tabstop(ed));
    if visual { col.min(t.len(line)) } else { col.min(t.max_normal_col(line)) }
}

pub(crate) fn update_curswant(vim: &mut VimState, ed: &Editor) {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let p = cursor(ed);
    let on_tab = t.char_at(p) == Some('\t') && !vim.is_insert();
    vim.curswant = Some(if on_tab { t.vcol_end(p.line, p.col, tabstop(ed)) - 1 } else { t.vcol(p.line, p.col, tabstop(ed)) });
}

fn fold_down(ed: &Editor, line: usize) -> usize {
    // A closed fold is skipped as a whole.
    if let Some(&end) = ed.folded_ranges.get(&line) { return end; }
    if let Some((_, end)) = ed.folded_range_containing(line) { return end; }
    line + 1
}

fn fold_start(ed: &Editor, line: usize) -> usize {
    ed.folded_range_containing(line).map_or(line, |(s, _)| s)
}

// ---------------------------------------------------------------------------
// Motions

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MKind { Exclusive, Inclusive, Line }

#[derive(Clone, Copy, Debug)]
pub(crate) struct MRes {
    pub(crate) pos: Pos,
    pub(crate) kind: MKind,
    /// Keep the current curswant (vertical motions).
    pub(crate) keep_curswant: bool,
    pub(crate) jump: bool,
    /// For `$`: curswant becomes "end of line".
    pub(crate) eol: bool,
    /// Operator-only: the region start differs from the cursor (`gn`).
    pub(crate) start: Option<Pos>,
}

impl MRes {
    fn new(pos: Pos, kind: MKind) -> Self {
        Self { pos, kind, keep_curswant: false, jump: false, eol: false, start: None }
    }
}

/// Evaluate a motion from the cursor. `op` is the pending operator.
pub(crate) fn eval_motion(
    vim: &mut VimState,
    host: &mut dyn Host,
    m: Motion,
    count: usize,
    has_count: bool,
    op: Option<Op>,
) -> Option<MRes> {
    let visual = vim.is_visual();
    let (viewport, scroll) = {
        let ed = host.ed_ref()?;
        (ed.viewport_height.max(1), ed.scroll_offset)
    };
    let ed = host.ed_ref()?;
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let cur = t.clamp_insert(cursor(ed));
    let ts = tabstop(ed);
    let kw = vim.opts.iskeyword.clone();
    let n = count.max(1);
    use MKind::*;
    let res = match m {
        Motion::Left | Motion::BackChar => {
            let wrap = m == Motion::BackChar && vim.opts.whichwrap.contains('b');
            let mut p = cur;
            let mut moved = false;
            for _ in 0..n {
                if p.col > 0 {
                    p.col -= 1;
                    moved = true;
                } else if wrap && p.line > 0 {
                    p.line -= 1;
                    p.col = if op.is_some() { t.len(p.line) } else { t.max_normal_col(p.line) };
                    moved = true;
                } else {
                    break;
                }
            }
            if !moved && op.is_none() { return None; }
            MRes::new(p, Exclusive)
        }
        Motion::Right | Motion::FwdChar => {
            // Vim's nv_right(): in Visual mode the cursor may move onto the
            // end-of-line position; with an operator a failed move makes the
            // motion inclusive so `dl` on the last character deletes it.
            let wrap = m == Motion::FwdChar && vim.opts.whichwrap.contains('s');
            let mut p = cur;
            let mut moved = 0;
            let mut inclusive = false;
            let mut i = 0;
            while i < n {
                let len = t.len(p.line);
                let can = if visual { p.col < len } else { p.col + 1 < len };
                if can {
                    p.col += 1;
                    moved += 1;
                    i += 1;
                    continue;
                }
                if wrap && p.line + 1 < t.lines() {
                    if op.is_some() && !inclusive && len > 0 {
                        inclusive = true;
                    } else {
                        p.line += 1;
                        p.col = 0;
                        inclusive = false;
                        moved += 1;
                    }
                    i += 1;
                    continue;
                }
                if op.is_none() {
                    if moved == 0 { return None; }
                } else if len > 0 {
                    inclusive = true;
                }
                break;
            }
            MRes::new(p, if inclusive { Inclusive } else { Exclusive })
        }
        Motion::Up | Motion::Down => {
            let mut line = cur.line;
            let down = m == Motion::Down;
            let mut moved = 0;
            for _ in 0..n {
                if down {
                    let next = fold_down(ed, line);
                    if next >= t.lines() { break; }
                    line = next;
                } else {
                    if line == 0 { break; }
                    line = fold_start(ed, line - 1);
                }
                moved += 1;
            }
            if moved == 0 { return None; }
            let col = col_for_curswant(vim, ed, line, visual);
            let mut r = MRes::new(Pos::new(line, col), Line);
            r.keep_curswant = true;
            r
        }
        Motion::DisplayDown | Motion::DisplayUp => {
            if !ed.word_wrap {
                return eval_motion(vim, host, if m == Motion::DisplayDown { Motion::Down } else { Motion::Up }, count, has_count, op)
                    .map(|mut r| { r.kind = Exclusive; r });
            }
            let ed = host.ed()?;
            let before = cursor(ed);
            for _ in 0..n {
                if m == Motion::DisplayDown { ed.move_down() } else { ed.move_up() }
            }
            let p = cursor(ed);
            ed.cursor_line = before.line;
            ed.cursor_col = before.col;
            if p == before { return None; }
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let mut r = MRes::new(t.clamp_normal(p), Exclusive);
            if visual { r.pos = t.clamp_insert(p); }
            r
        }
        Motion::WordFwd(big) => {
            let mut w = Walker::new(t, cur, &kw, big);
            // "cw" on a non-blank acts like "ce".
            if op == Some(Op::Change) {
                let c = t.gchar(cur);
                if c != '\0' && c != ' ' && c != '\t' {
                    let ok = w.end_word(n, true, false);
                    if !ok && w.pos == cur { return None; }
                    return Some(MRes::new(w.pos, Inclusive));
                }
            }
            let ok = w.fwd_word(n, op.is_some());
            let mut p = w.pos;
            let mut kind = Exclusive;
            if cur < p && motion::adjust_off_eol(&t, &mut p) && op.is_some() {
                kind = Inclusive;
            } else if cur < p && visual && t.char_at(p).is_none() && p.col > 0 {
                p.col -= 1;
            }
            if !ok && op.is_none() { return None; }
            MRes::new(p, kind)
        }
        Motion::WordBack(big) => {
            let mut w = Walker::new(t, cur, &kw, big);
            if !w.bck_word(n, false) { return None; }
            MRes::new(w.pos, Exclusive)
        }
        Motion::WordEnd(big) => {
            let mut w = Walker::new(t, cur, &kw, big);
            if !w.end_word(n, false, false) && w.pos == cur && op.is_none() { return None; }
            let mut p = w.pos;
            if !visual { motion::adjust_off_eol(&t, &mut p); }
            MRes::new(p, Inclusive)
        }
        Motion::WordEndBack(big) => {
            let mut w = Walker::new(t, cur, &kw, big);
            if !w.bckend_word(n, false) { return None; }
            MRes::new(w.pos, Inclusive)
        }
        Motion::LineStart => MRes::new(Pos::new(cur.line, 0), Exclusive),
        Motion::FirstNonBlank => MRes::new(Pos::new(cur.line, t.first_nonblank(cur.line)), Exclusive),
        Motion::LineEnd => {
            let line = cur.line + n - 1;
            if line >= t.lines() { return None; }
            let col = if op.is_some() || visual { t.len(line).saturating_sub(usize::from(op.is_some() && t.len(line) > 0)) } else { t.max_normal_col(line) };
            let col = if visual && op.is_none() { t.len(line) } else { col };
            let mut r = MRes::new(Pos::new(line, col), Inclusive);
            r.eol = true;
            if op.is_some() && t.len(line) == 0 { r.kind = Exclusive; }
            r
        }
        Motion::LastNonBlank => {
            let line = (cur.line + n - 1).min(t.last_line());
            let chars = t.line_chars(line);
            let col = chars.iter().rposition(|&c| c != ' ' && c != '\t').unwrap_or(0);
            MRes::new(Pos::new(line, col), Inclusive)
        }
        Motion::Column => {
            let col = t.col_for_vcol(cur.line, n - 1, ts).min(if op.is_some() { t.len(cur.line) } else { t.max_normal_col(cur.line) });
            let mut r = MRes::new(Pos::new(cur.line, col), Exclusive);
            r.keep_curswant = true;
            vim.curswant = Some(n - 1);
            r
        }
        Motion::GotoLine | Motion::GotoFirstLine => {
            let line = if has_count { (count.max(1) - 1).min(t.last_line()) } else if m == Motion::GotoLine { t.last_line() } else { 0 };
            let col = if vim.opts.startofline { t.first_nonblank(line) } else { col_for_curswant(vim, ed, line, visual) };
            let mut r = MRes::new(Pos::new(line, col), Line);
            r.jump = true;
            r.keep_curswant = !vim.opts.startofline;
            r
        }
        Motion::LineUpFirstNonBlank => {
            if cur.line < n { return None; }
            let line = cur.line - n;
            MRes::new(Pos::new(line, t.first_nonblank(line)), Line)
        }
        Motion::LineDownFirstNonBlank => {
            let line = cur.line + n;
            if line >= t.lines() { return None; }
            MRes::new(Pos::new(line, t.first_nonblank(line)), Line)
        }
        Motion::CurrentLineFirstNonBlank => {
            let line = cur.line + n - 1;
            if line >= t.lines() { return None; }
            MRes::new(Pos::new(line, t.first_nonblank(line)), Line)
        }
        Motion::ScreenTop | Motion::ScreenMiddle | Motion::ScreenBottom => {
            let so = ed.cursor_surrounding_lines.min(viewport.saturating_sub(1) / 2);
            let visible: Vec<usize> = ed.visible_lines(scroll, viewport).into_iter().filter(|&l| l < t.lines()).collect();
            if visible.is_empty() { return None; }
            let last_visible = visible.len() - 1;
            let at_top = scroll == 0;
            let at_bottom = visible.last().is_some_and(|&l| l >= t.last_line());
            let idx = match m {
                Motion::ScreenTop => {
                    let min = if at_top { 0 } else { so };
                    (n - 1).max(min).min(last_visible)
                }
                Motion::ScreenBottom => {
                    let min = if at_bottom { 0 } else { so };
                    last_visible.saturating_sub((n - 1).max(min))
                }
                _ => last_visible / 2,
            };
            let line = visible[idx.min(last_visible)];
            let mut r = MRes::new(Pos::new(line, t.first_nonblank(line)), Line);
            r.jump = true;
            r
        }
        Motion::ParaFwd | Motion::ParaBack => {
            let (mut p, incl) = motion::find_par(&t, cur, m == Motion::ParaFwd, n, '\0', false)?;
            if visual && incl { p.col = t.len(p.line); }
            let mut r = MRes::new(p, if incl { Inclusive } else { Exclusive });
            r.jump = true;
            r
        }
        Motion::SentFwd | Motion::SentBack => {
            let mut p = motion::find_sent(&t, cur, m == Motion::SentFwd, n)?;
            let mut kind = Exclusive;
            if motion::adjust_off_eol(&t, &mut p) && op.is_some() { kind = Inclusive; }
            let mut r = MRes::new(p, kind);
            r.jump = true;
            r
        }
        Motion::Percent => {
            let mut r = if has_count {
                if count > 100 { return None; }
                let line = ((count * t.lines()).div_ceil(100)).max(1) - 1;
                MRes::new(Pos::new(line, t.first_nonblank(line)), Line)
            } else {
                MRes::new(motion::match_pair(&t, cur)?, Inclusive)
            };
            r.jump = true;
            r
        }
        Motion::Find { forward, till, c } => {
            vim.last_find = Some((c, forward, till));
            let col = motion::find_char(&t, cur, c, forward, till, n, false)?;
            MRes::new(Pos::new(cur.line, col), if forward { Inclusive } else { Exclusive })
        }
        Motion::RepeatFind | Motion::RepeatFindRev => {
            let (c, fwd, till) = vim.last_find?;
            let forward = if m == Motion::RepeatFind { fwd } else { !fwd };
            let col = motion::find_char(&t, cur, c, forward, till, n, true)?;
            MRes::new(Pos::new(cur.line, col), if forward { Inclusive } else { Exclusive })
        }
        Motion::Mark { c, line } => {
            let p = match get_mark(vim, host, c) {
                Ok(p) => p,
                Err(e) => { vim.set_error(e); return None; }
            };
            let ed = host.ed_ref()?;
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let p = t.clamp_normal(p);
            let mut r = if line {
                MRes::new(Pos::new(p.line, t.first_nonblank(p.line)), Line)
            } else {
                MRes::new(p, Exclusive)
            };
            r.jump = !matches!(c, '[' | ']' | '<' | '>');
            r.jump = true;
            r
        }
        Motion::SearchNext | Motion::SearchPrev => {
            let (pat, fwd) = vim.last_search.clone().or_else(|| {
                let s = vim.regs.last_search.clone();
                (!s.is_empty()).then_some((s, true))
            }).or_else(|| { vim.set_error("E35: No previous regular expression"); None })?;
            let forward = if m == Motion::SearchNext { fwd } else { !fwd };
            vim.hl_hidden = false;
            let p = search_motion(vim, host, &pat, forward, n, false)?;
            let mut r = MRes::new(p, Exclusive);
            r.jump = true;
            r
        }
        Motion::StarFwd(whole) | Motion::StarBack(whole) => {
            let forward = matches!(m, Motion::StarFwd(_));
            let (word, keyword) = word_under_cursor(&t, cur, &kw)?;
            let escaped: String = word.chars().map(|c| if "\\/.*$^~[]".contains(c) { format!("\\{c}") } else { c.to_string() }).collect();
            let pat = if whole && keyword { format!("\\<{escaped}\\>") } else { escaped };
            vim.last_search = Some((pat.clone(), forward));
            vim.regs.last_search = pat.clone();
            push_history(&mut vim.history_search, &pat);
            vim.hl_hidden = false;
            // Start from the word's start so `#` skips the current word.
            let start = word_start(&t, cur, &kw);
            let ed = host.ed()?;
            let saved = cursor(ed);
            ed.cursor_line = start.line;
            ed.cursor_col = start.col;
            let found = search_motion(vim, host, &pat, forward, n, false);
            if let Some(ed) = host.ed() { ed.cursor_line = saved.line; ed.cursor_col = saved.col; }
            let mut r = MRes::new(found?, Exclusive);
            r.jump = true;
            r
        }
        Motion::DisplayLineStart | Motion::DisplayFirstNonBlank | Motion::DisplayMiddle | Motion::DisplayLineEnd => {
            let (start, end) = display_line_span(ed, cur);
            let col = match m {
                Motion::DisplayLineStart => start,
                Motion::DisplayFirstNonBlank => {
                    let chars = t.line_chars(cur.line);
                    (start..end).find(|&i| !matches!(chars.get(i), Some(' ' | '\t'))).unwrap_or(start)
                }
                // Half a screen width to the right of the first visible column.
                Motion::DisplayMiddle => {
                    let w = if ed.word_wrap { ed.effective_wrap_width() } else { ed.viewport_width.max(1) };
                    let base = t.vcol(cur.line, start, ts);
                    t.col_for_vcol(cur.line, base + w / 2, ts).min(t.max_normal_col(cur.line))
                }
                _ => end.saturating_sub(1).max(start),
            };
            let kind = if m == Motion::DisplayLineEnd { Inclusive } else { Exclusive };
            MRes::new(Pos::new(cur.line, col.min(t.len(cur.line))), kind)
        }
        Motion::LineMiddle => {
            let pct = if has_count { count.min(100) } else { 50 };
            let width = t.line_vwidth(cur.line, ts);
            let col = t.col_for_vcol(cur.line, width * pct / 100, ts);
            MRes::new(Pos::new(cur.line, col.min(t.max_normal_col(cur.line))), Exclusive)
        }
        Motion::ByteOffset => {
            let p = t.pos(count.max(1) - 1);
            let mut r = MRes::new(t.clamp_normal(p), Exclusive);
            r.jump = true;
            r
        }
        Motion::SectionFwd(c) | Motion::SectionBack(c) => {
            let p = motion::section(&t, cur, matches!(m, Motion::SectionFwd(_)), c, n);
            let mut r = MRes::new(p, Exclusive);
            r.jump = true;
            r
        }
        Motion::Unmatched(c) => {
            let (open, close) = match c { '(' | ')' => ('(', ')'), _ => ('{', '}') };
            let p = motion::unmatched(&t, cur, open, close, matches!(c, ')' | '}'), n)?;
            MRes::new(p, if matches!(c, ')' | '}') { Inclusive } else { Exclusive })
        }
        Motion::Method { forward, open } => {
            let p = motion::method_brace(&t, cur, forward, open, n)?;
            MRes::new(p, Exclusive)
        }
        Motion::Search(_) => return None,
        Motion::MatchArea(forward) => {
            let (pat, _) = vim.last_search.clone()?;
            let (s, e) = match_area(vim, host, &pat, forward)?;
            let mut r = MRes::new(e, Inclusive);
            r.start = Some(s);
            r
        }
    };
    let mut res = res;
    if m.is_jump() { res.jump = true; }
    Some(res)
}

/// The keyword (or non-blank run) under or after the cursor for `*`.
/// The identifier `*` and `#` search for (Vim's `find_ident_under_cursor`):
/// the keyword under or after the cursor, or else the non-blank string.
fn word_under_cursor(t: &Text<'_>, cur: Pos, kw: &super::text::Keyword) -> Option<(String, bool)> {
    let chars = t.line_chars(cur.line);
    let class = |c: char| if c == ' ' || c == '\t' { 0 } else if kw.is_word(c) { 2 } else { 1 };
    for keyword_pass in [true, false] {
        // 1. Skip to the start of an identifier (or any non-blank string).
        let mut col = cur.col.min(chars.len());
        while col < chars.len() {
            let c = class(chars[col]);
            if c != 0 && (!keyword_pass || c != 1) { break; }
            col += 1;
        }
        if col >= chars.len() { continue; }
        let this = class(chars[col]);
        if keyword_pass && this != 2 { continue; }
        // 2. Back up to its start.
        while col > 0 {
            let prev = class(chars[col - 1]);
            // `*` uses FIND_IDENT: stop at any class change.
            if prev != this { break; }
            col -= 1;
        }
        // 3. Find its end.
        let mut end = col;
        while end < chars.len() && if keyword_pass { class(chars[end]) == this } else { class(chars[end]) != 0 } { end += 1; }
        return Some((chars[col..end].iter().collect(), keyword_pass));
    }
    None
}

fn word_start(t: &Text<'_>, cur: Pos, kw: &super::text::Keyword) -> Pos {
    let chars = t.line_chars(cur.line);
    let mut s = cur.col.min(chars.len());
    if chars.get(s).is_some_and(|&c| kw.is_word(c)) {
        while s > 0 && kw.is_word(chars[s - 1]) { s -= 1; }
    }
    Pos::new(cur.line, s)
}

/// Columns covered by the screen row containing `p`.
fn display_line_span(ed: &Editor, p: Pos) -> (usize, usize) {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    if ed.word_wrap {
        let width = ed.effective_wrap_width();
        let ranges = crate::editor::Editor::wrap_ranges_for_line_text_tabbed(&t.line_string(p.line), width, tabstop(ed));
        for (s, e) in ranges {
            if p.col >= s && (p.col < e || e == t.len(p.line)) {
                return (s, e);
            }
        }
        return (0, t.len(p.line));
    }
    let h = ed.horizontal_scroll;
    let w = ed.viewport_width.max(1);
    let ts = tabstop(ed);
    let start = t.col_for_vcol(p.line, h, ts);
    let end = t.col_for_vcol(p.line, h + w, ts).max(start);
    (start, end.min(t.len(p.line)))
}

pub(crate) fn push_history(list: &mut Vec<String>, entry: &str) {
    if entry.is_empty() { return; }
    list.retain(|e| e != entry);
    list.push(entry.to_string());
    if list.len() > 200 { list.remove(0); }
}

// ---------------------------------------------------------------------------
// Search

pub(crate) fn haystack(ed: &Editor) -> String { ed.rope.to_string() }

/// Find the `count`th match from the cursor. Reports wrap and errors.
pub(crate) fn search_motion(vim: &mut VimState, host: &mut dyn Host, pat: &str, forward: bool, count: usize, accept_at: bool) -> Option<Pos> {
    let re = match vim.compile(pat) {
        Ok(re) => re,
        Err(e) => { vim.set_error(e); return None; }
    };
    let ed = host.ed_ref()?;
    let hay = vim.haystack(ed);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let mut from = t.idx(cursor(ed));
    let mut wrapped = false;
    let mut accept = accept_at;
    for _ in 0..count.max(1) {
        match super::search::search(&ed.rope, &hay, &re, from, forward, vim.opts.wrapscan, accept) {
            Some((s, _, w)) => { from = s; wrapped |= w; }
            None => {
                let msg = if vim.opts.wrapscan {
                    format!("E486: Pattern not found: {pat}")
                } else if forward {
                    format!("E385: Search hit BOTTOM without match for: {pat}")
                } else {
                    format!("E384: Search hit TOP without match for: {pat}")
                };
                vim.set_error(msg);
                return None;
            }
        }
        accept = false;
    }
    let p = t.pos(from);
    let p = Pos::new(p.line.min(t.last_line()), p.col);
    if wrapped {
        vim.set_message(if forward { "search hit BOTTOM, continuing at TOP" } else { "search hit TOP, continuing at BOTTOM" });
    } else {
        vim.set_message(format!("{}{pat}", if forward { '/' } else { '?' }));
    }
    Some(p)
}

/// `gn`: the match under or after the cursor (before it for `gN`).
fn match_area(vim: &mut VimState, host: &mut dyn Host, pat: &str, forward: bool) -> Option<(Pos, Pos)> {
    let re = super::search::compile(pat, &vim.search_options()).ok()?;
    let ed = host.ed_ref()?;
    let hay = haystack(ed);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let c = t.idx(cursor(ed));
    let cb = ed.rope.char_to_byte(c);
    let matches: Vec<(usize, usize)> = re.find_iter(&hay).filter(|(s, e)| e > s).collect();
    let pick = if forward {
        matches.iter().find(|&&(s, e)| cb >= s && cb < e).or_else(|| matches.iter().find(|&&(s, _)| s >= cb)).or_else(|| matches.first())
    } else {
        matches.iter().rev().find(|&&(s, e)| cb >= s && cb < e).or_else(|| matches.iter().rev().find(|&&(s, _)| s < cb)).or_else(|| matches.last())
    }?;
    let s = t.pos(ed.rope.byte_to_char(pick.0));
    let e = t.pos(ed.rope.byte_to_char(pick.1) - 1);
    Some((s, e))
}

// ---------------------------------------------------------------------------
// Marks and jumps

pub(crate) fn get_mark(vim: &VimState, host: &mut dyn Host, c: char) -> Result<Pos, String> {
    let ed = host.ed_ref().ok_or("E20: Mark not set")?;
    match c {
        'a'..='z' | '[' | ']' | '<' | '>' | '.' | '^' | '"' => {
            if c == '<' || c == '>' {
                if let Some(v) = &ed.vim_buf.last_visual {
                    let (s, e) = if v.start <= v.end { (v.start, v.end) } else { (v.end, v.start) };
                    let (s, e) = match v.kind {
                        VisualKind::Line => (Pos::new(s.line, 0), Pos::new(e.line, usize::MAX)),
                        _ => (s, e),
                    };
                    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                    let pick = if c == '<' { s } else { e };
                    return Ok(Pos::new(pick.line.min(t.last_line()), pick.col.min(t.max_normal_col(pick.line.min(t.last_line())))));
                }
                return Err("E20: Mark not set".into());
            }
            if c == '"' {
                return Ok(ed.vim_buf.marks.get(&'"').copied().unwrap_or_default());
            }
            ed.vim_buf.marks.get(&c).copied().ok_or_else(|| "E20: Mark not set".into())
        }
        '`' | '\'' => {
            ed.vim_buf.marks.get(&'\'').copied().ok_or_else(|| "E20: Mark not set".to_string()).or(Ok(Pos::default()))
        }
        'A'..='Z' | '0'..='9' => {
            let j = vim.global_marks.get(&c).ok_or("E20: Mark not set")?;
            if j.nav_id == ed.navigation_id || (j.path.is_some() && j.path == ed.file_path) {
                Ok(j.pos)
            } else {
                Err(format!("\u{0}{c}"))
            }
        }
        _ => Err("E78: Unknown mark".into()),
    }
}

pub(crate) fn set_mark(vim: &mut VimState, ed: &mut Editor, c: char, p: Pos) -> bool {
    match c {
        'a'..='z' | '[' | ']' | '<' | '>' | '\'' | '`' | '"' | '^' | '.' => {
            let c = if c == '`' { '\'' } else { c };
            ed.vim_buf.marks.insert(c, p);
            true
        }
        'A'..='Z' => {
            vim.global_marks.insert(c, Jump { nav_id: ed.navigation_id, path: ed.file_path.clone(), pos: p });
            true
        }
        _ => false,
    }
}

/// Remember the cursor before a jump (jumplist and the `''` mark).
pub(crate) fn push_jump(vim: &mut VimState, ed: &mut Editor) {
    let p = cursor(ed);
    ed.vim_buf.marks.insert('\'', p);
    let j = Jump { nav_id: ed.navigation_id, path: ed.file_path.clone(), pos: p };
    vim.jumps.truncate(vim.jump_idx.min(vim.jumps.len()));
    vim.jumps.retain(|e| !(e.nav_id == j.nav_id && e.pos.line == j.pos.line));
    vim.jumps.push(j);
    if vim.jumps.len() > 100 { vim.jumps.remove(0); }
    vim.jump_idx = vim.jumps.len();
}

pub(crate) fn jump_list(vim: &mut VimState, host: &mut dyn Host, older: bool, count: usize) {
    let Some(ed) = host.ed() else { return; };
    if older && vim.jump_idx == vim.jumps.len() {
        // Save the current position so <C-i> can come back.
        let p = cursor(ed);
        let j = Jump { nav_id: ed.navigation_id, path: ed.file_path.clone(), pos: p };
        vim.jumps.retain(|e| !(e.nav_id == j.nav_id && e.pos.line == j.pos.line));
        vim.jumps.push(j);
        vim.jump_idx = vim.jumps.len() - 1;
    }
    let target = if older {
        vim.jump_idx.checked_sub(count.max(1))
    } else {
        let t = vim.jump_idx + count.max(1);
        (t < vim.jumps.len()).then_some(t)
    };
    let Some(target) = target else { return; };
    vim.jump_idx = target;
    let j = vim.jumps[target].clone();
    let same = host.ed_ref().is_some_and(|ed| ed.navigation_id == j.nav_id || (j.path.is_some() && ed.file_path == j.path));
    if same {
        if let Some(ed) = host.ed() {
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            set_cursor(ed, t.clamp_normal(j.pos));
            update_curswant(vim, ed);
        }
    } else if let Err(e) = host.act(Act::JumpTo { nav_id: j.nav_id, path: j.path.clone(), pos: j.pos }) {
        vim.set_error(e);
    }
}

// ---------------------------------------------------------------------------
// Visual mode

pub(crate) fn enter_visual(vim: &mut VimState, host: &mut dyn Host, kind: VisualKind) {
    let Some(ed) = host.ed() else { return; };
    if !vim.is_visual() {
        vim.visual_anchor = cursor(ed);
        vim.curswant_eol = false;
    }
    vim.visual_kind = kind;
    vim.mode = Mode::Visual(kind);
    ed.extra_cursors.clear();
    ed.extra_cursor_anchors.clear();
    sync_visual(vim, ed);
}

/// The Visual area as an operator region.
pub(crate) fn visual_region(vim: &VimState, ed: &Editor) -> Region {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let a = t.clamp_insert(vim.visual_anchor);
    let c = t.clamp_insert(cursor(ed));
    let (s, e) = if a <= c { (a, c) } else { (c, a) };
    match vim.visual_kind {
        VisualKind::Char => {
            // The character under the end is included; on the end-of-line
            // position the line break is (Vim moves the end to the start of
            // the next line, exclusive).
            Region { start: s, end: e, kind: RKind::Char { inclusive: true }, nl: true }
        }
        VisualKind::Line => Region { start: Pos::new(s.line, 0), end: Pos::new(e.line, 0), kind: RKind::Line, nl: false },
        VisualKind::Block => {
            let ts = tabstop(ed);
            let va = t.vcol(a.line, a.col, ts);
            let vc = t.vcol(c.line, c.col, ts);
            let left = va.min(vc);
            let right = if vim.curswant_eol {
                usize::MAX
            } else {
                t.vcol_end(a.line, a.col, ts).max(t.vcol_end(c.line, c.col, ts))
            };
            Region { start: Pos::new(s.line.min(e.line), 0), end: Pos::new(a.line.max(c.line), 0), kind: RKind::Block { left, right }, nl: false }
        }
    }
}

/// Mirror the Visual area into the editor's selection.
pub(crate) fn sync_visual(vim: &VimState, ed: &mut Editor) {
    if !vim.is_visual() {
        ed.vim_selection = None;
        ed.vim_block = None;
        return;
    }
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let r = visual_region(vim, ed);
    ed.selection_anchor = Some((vim.visual_anchor.line, vim.visual_anchor.col));
    match r.kind {
        RKind::Char { .. } => {
            let end_idx = r.char_span(&t).1.max(t.idx(r.end));
            let e = ed.rope.char_to_line(end_idx);
            let ec = end_idx - ed.rope.line_to_char(e);
            ed.vim_selection = Some((r.start.line, r.start.col, e, ec));
            ed.vim_block = None;
        }
        RKind::Line => {
            let (el, ec) = if r.end.line + 1 < ed.rope.len_lines() { (r.end.line + 1, 0) } else { (r.end.line, t.len(r.end.line)) };
            ed.vim_selection = Some((r.start.line, 0, el, ec));
            ed.vim_block = None;
        }
        RKind::Block { left, right } => {
            ed.vim_block = Some(BlockArea { top: r.start.line, bottom: r.end.line, left, right });
            let ts = tabstop(ed);
            let sc = t.col_for_vcol(r.start.line, left, ts);
            let ec = if right == usize::MAX { t.len(r.end.line) } else { t.col_for_vcol(r.end.line, right, ts) };
            ed.vim_selection = Some((r.start.line, sc, r.end.line, ec.max(if r.start.line == r.end.line { sc } else { 0 })));
        }
    }
}

pub(crate) fn save_last_visual(vim: &VimState, ed: &mut Editor) {
    let lv = LastVisual { kind: vim.visual_kind, start: vim.visual_anchor, end: cursor(ed), curswant_eol: vim.curswant_eol };
    ed.vim_buf.last_visual = Some(lv);
}

pub(crate) fn exit_visual(vim: &mut VimState, host: &mut dyn Host) {
    if let Some(ed) = host.ed() {
        if vim.is_visual() {
            save_last_visual(vim, ed);
        }
        ed.vim_selection = None;
        ed.vim_block = None;
        ed.selection_anchor = None;
        clamp_normal(ed);
    }
    if vim.is_visual() {
        vim.mode = Mode::Normal;
    }
}

// ---------------------------------------------------------------------------
// Regions

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RKind {
    Char { inclusive: bool },
    Line,
    Block { left: usize, right: usize },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Region {
    pub(crate) start: Pos,
    pub(crate) end: Pos,
    pub(crate) kind: RKind,
    /// Visual mode: an inclusive end on the end-of-line position takes the
    /// line break (operators never do).
    pub(crate) nl: bool,
}

impl Region {
    /// Char offsets `[start, end)` for a characterwise region.
    pub(crate) fn char_span(&self, t: &Text<'_>) -> (usize, usize) {
        let s = t.idx(self.start);
        let mut e = t.idx(self.end);
        if let RKind::Char { inclusive: true } = self.kind {
            if t.char_at(self.end).is_some() {
                e += 1;
            } else if self.nl && self.end.line < t.last_line() {
                e = t.line_next_start(self.end.line);
            }
        }
        (s, e.max(s))
    }

    pub(crate) fn line_span(&self) -> (usize, usize) { (self.start.line, self.end.line) }
}

/// One line of a block, as Vim's `block_prep()` computes it. Characters
/// that straddle the block's edges (tabs, wide characters) are split into
/// spaces: `startspaces`/`endspaces` are the cells kept (delete) or taken
/// (yank) as spaces.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct BlockLine {
    pub(crate) textcol: usize,
    pub(crate) textlen: usize,
    pub(crate) startspaces: usize,
    pub(crate) endspaces: usize,
    pub(crate) is_short: bool,
    /// The line ends before the block starts.
    pub(crate) before: bool,
}

pub(crate) fn block_prep(t: &Text<'_>, line: usize, left: usize, right: usize, ts: usize, is_del: bool) -> BlockLine {
    let chars = t.line_chars(line);
    let end_vcol = if right == usize::MAX { usize::MAX } else { right.saturating_sub(1) };
    let mut b = BlockLine::default();
    let mut vcol = 0usize;
    let mut i = 0usize;
    let mut incr = 0usize;
    let mut prev = 0usize;
    while vcol < left && i < chars.len() {
        incr = super::text::char_width(chars[i], vcol, ts);
        vcol += incr;
        prev = i;
        i += 1;
    }
    let start_vcol = vcol;
    let start_char_vcols = incr;
    let mut pstart = i;
    if start_vcol < left {
        b.is_short = true;
        b.before = true;
        if !is_del && right != usize::MAX { b.endspaces = end_vcol - left + 1; }
        b.textcol = chars.len();
        return b;
    }
    b.startspaces = start_vcol - left;
    if is_del && b.startspaces > 0 { b.startspaces = start_char_vcols - b.startspaces; }
    let mut pend = pstart;
    let mut ev = start_vcol;
    if ev > end_vcol {
        // The whole block lies inside one character.
        b.startspaces = end_vcol - left + 1;
        if is_del {
            b.startspaces = start_char_vcols - (start_vcol - left);
            b.endspaces = ev - end_vcol - 1;
        }
    } else {
        let mut prev_pend = pend;
        while ev <= end_vcol && pend < chars.len() {
            prev_pend = pend;
            incr = super::text::char_width(chars[pend], ev, ts);
            ev += incr;
            pend += 1;
        }
        if ev <= end_vcol {
            b.is_short = true;
        } else if ev > end_vcol {
            b.endspaces = ev - end_vcol - 1;
            if !is_del && b.endspaces > 0 {
                b.endspaces = incr - b.endspaces;
                if pend != pstart { pend = prev_pend; }
            }
        }
    }
    if is_del && b.startspaces > 0 { pstart = prev; }
    b.textcol = pstart;
    b.textlen = pend.saturating_sub(pstart);
    b
}

/// Block columns on one line: (start_col, end_col, pad_before, pad_after)
/// where pads are display cells of a split tab or short line.
pub(crate) fn block_cols(t: &Text<'_>, line: usize, left: usize, right: usize, ts: usize) -> Option<(usize, usize)> {
    let width = t.line_vwidth(line, ts);
    if left >= width { return None; }
    let sc = t.col_for_vcol(line, left, ts);
    let ec = if right == usize::MAX { t.len(line) } else {
        let c = t.col_for_vcol(line, right.saturating_sub(1), ts);
        (c + 1).min(t.len(line))
    };
    Some((sc, ec.max(sc)))
}

/// Text of a region for a register.
pub(crate) fn region_text(ed: &Editor, r: &Region) -> Reg {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    match r.kind {
        RKind::Char { .. } => {
            let (s, e) = r.char_span(&t);
            Reg::new(plain(&t.slice(s, e)), RegKind::Char)
        }
        RKind::Line => {
            let mut s = String::new();
            for line in r.start.line..=r.end.line.min(t.last_line()) {
                s.push_str(&t.line_string(line));
                s.push('\n');
            }
            Reg::new(s, RegKind::Line)
        }
        RKind::Block { left, right } => {
            let ts = tabstop(ed);
            let mut lines = Vec::new();
            let mut width = 0;
            for line in r.start.line..=r.end.line.min(t.last_line()) {
                let b = block_prep(&t, line, left, right, ts, false);
                let chars = t.line_chars(line);
                let mut piece = " ".repeat(b.startspaces);
                piece.extend(&chars[b.textcol.min(chars.len())..(b.textcol + b.textlen).min(chars.len())]);
                piece.push_str(&" ".repeat(b.endspaces));
                width = width.max(piece.chars().count());
                lines.push(piece);
            }
            let w = if right == usize::MAX {
                (r.start.line..=r.end.line.min(t.last_line())).map(|l| t.line_vwidth(l, ts).saturating_sub(left)).max().unwrap_or(0).max(1)
            } else { right - left };
            let _ = width;
            Reg::new(lines.join("\n"), RegKind::Block(w))
        }
    }
}

// ---------------------------------------------------------------------------
// Command dispatch

/// After the workbench handled a key in Normal mode: re-establish Vim's
/// invariants (selection from the mouse or Ctrl+A becomes Visual mode).
pub(crate) fn after_native(vim: &mut VimState, host: &mut dyn Host) {
    let Some(ed) = host.ed() else { return; };
    if vim.mode == Mode::Normal && ed.vim_selection.is_none()
        && let Some(anchor) = ed.selection_anchor.filter(|&a| a != (ed.cursor_line, ed.cursor_col))
    {
        vim.visual_anchor = Pos::new(anchor.0, anchor.1);
        vim.visual_kind = VisualKind::Char;
        vim.mode = Mode::Visual(VisualKind::Char);
        // The editor's selection is half-open; Vim's includes the cursor.
        let c = cursor(ed);
        if c > vim.visual_anchor && c.col > 0 {
            ed.cursor_col -= 1;
        }
        sync_visual(vim, ed);
        return;
    }
    if vim.mode == Mode::Normal {
        clamp_normal(ed);
    }
}

pub(crate) fn run(vim: &mut VimState, host: &mut dyn Host, cmd: Cmd) {
    let count = cmd.count1();
    let has_count = cmd.has_count();
    if vim.curswant.is_none() {
        if let Some(ed) = host.ed() { update_curswant(vim, ed); }
    }
    if let Some(ed) = host.ed() {
        if vim.mode == Mode::Normal && !ed.extra_cursors.is_empty() && matches!(cmd.action, Action::Simple(Simple::Cancel)) {
            ed.extra_cursors.clear();
            ed.extra_cursor_anchors.clear();
            return;
        }
    }
    match cmd.action.clone() {
        Action::Motion(Motion::Search(forward)) => {
            super::cmdline::open(vim, host, if forward { '/' } else { '?' }, None, String::new());
            if let Some(c) = &mut vim.cmdline { c.count = count; }
        }
        Action::Motion(m) => {
            let visual = vim.is_visual();
            let Some(r) = eval_motion(vim, host, m, count, has_count, None) else { vim.beep(); return; };
            let Some(ed) = host.ed() else { return; };
            if r.jump { push_jump(vim, ed); }
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let p = if visual { t.clamp_insert(r.pos) } else { t.clamp_normal(r.pos) };
            if let Some(start) = r.start.filter(|_| visual) {
                // `gn` in Visual mode selects the match.
                if vim.visual_anchor == cursor(ed) { vim.visual_anchor = start; }
            }
            set_cursor(ed, p);
            if r.eol { vim.curswant = Some(usize::MAX); vim.curswant_eol = true; }
            else if !r.keep_curswant { update_curswant(vim, ed); if visual { vim.curswant_eol = false; } }
            if visual { sync_visual(vim, ed); }
            if m == Motion::MatchArea(true) || m == Motion::MatchArea(false) {
                if !visual {
                    if let Some(s) = r.start {
                        vim.visual_anchor = s;
                        enter_visual(vim, host, VisualKind::Char);
                    }
                }
            }
        }
        Action::Obj { obj, inner } => {
            if !vim.is_visual() { return; }
            visual_object(vim, host, obj, inner, count);
        }
        Action::Op { op, force, target, .. } => {
            if let Target::Motion(Motion::Search(forward)) = target {
                super::cmdline::open(vim, host, if forward { '/' } else { '?' }, Some(cmd.clone()), String::new());
                return;
            }
            super::exec::op_target(vim, host, &cmd, op, force, target, count, has_count, None);
        }
        Action::VisualOp(op) => visual_op(vim, host, &cmd, op),
        Action::Simple(s) => simple(vim, host, &cmd, s, count, has_count),
    }
    // Most commands reset the wanted column (Vim's w_set_curswant).
    let keeps = matches!(cmd.action, Action::Motion(_))
        || matches!(cmd.action, Action::Simple(Simple::Scroll(_) | Simple::Cancel | Simple::Visual(_) | Simple::Redraw | Simple::Window(_)));
    if !keeps && !vim.is_insert() && vim.mode != Mode::Cmdline {
        if let Some(ed) = host.ed() {
            if !(vim.curswant == Some(usize::MAX) && matches!(cmd.action, Action::Simple(Simple::Reselect))) {
                update_curswant(vim, ed);
            }
        }
    }
}

fn visual_object(vim: &mut VimState, host: &mut dyn Host, obj: Obj, inner: bool, count: usize) {
    let Some(ed) = host.ed() else { return; };
    let cur = cursor(ed);
    let vis = Some((vim.visual_anchor, cur));
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let area = object_area(&t, cur, vis, obj, inner, count, &vim.opts.iskeyword);
    let Some(a) = area else { vim.beep(); return; };
    if a.linewise {
        vim.visual_anchor = if vim.visual_kind == VisualKind::Line && vim.visual_anchor.line != cur.line { vim.visual_anchor } else { a.start };
        vim.visual_kind = VisualKind::Line;
        vim.mode = Mode::Visual(VisualKind::Line);
    } else {
        if vim.visual_kind != VisualKind::Char || obj != Obj::Paragraph {
            vim.visual_kind = VisualKind::Char;
            vim.mode = Mode::Visual(VisualKind::Char);
        }
        vim.visual_anchor = a.start;
    }
    let end = a.end;
    if a.failed { vim.beep(); }
    set_cursor(ed, end);
    update_curswant(vim, ed);
    sync_visual(vim, ed);
}

pub(crate) fn object_area(t: &Text<'_>, cur: Pos, vis: textobj::Vis, obj: Obj, inner: bool, count: usize, kw: &super::text::Keyword) -> Option<textobj::Area> {
    match obj {
        Obj::Word(big) => textobj::word(*t, cur, vis, count, !inner, big, kw),
        Obj::Sentence => textobj::sentence(*t, cur, vis, count, !inner),
        Obj::Paragraph => textobj::paragraph(*t, cur, vis, count, !inner),
        Obj::Block(open, close) => textobj::block(*t, cur, vis, count, !inner, open, close),
        Obj::Quote(q) => textobj::quote(*t, cur, vis, count, !inner, q),
        Obj::Tag => textobj::tag(*t, cur, vis, count, !inner),
        Obj::Entire => {
            let last = t.last_line();
            Some(textobj::Area { start: Pos::new(0, 0), end: Pos::new(last, 0), inclusive: true, linewise: true, failed: false })
        }
    }
}

pub(crate) use super::ops::{op_target, simple, visual_op};
