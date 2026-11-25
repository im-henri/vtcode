//! Operators and Normal/Visual mode commands.

use super::exec::{
    self, MKind, RKind, Region, Tx, block_cols, clamp_normal, cursor, enter_visual, eval_motion,
    exit_visual, object_area, push_jump, region_text, set_cursor, set_mark, sync_visual,
    update_curswant, visual_region,
};
use super::insert;
use super::parse::{Action, Cmd, FoldCmd, Force, InsertKind, Motion, Op, Scroll, Simple, Target};
use super::register::{Reg, RegKind, Store};
use super::text::{Pos, Text, char_width};
use super::{Act, Change, Host, HostClip, Mode, VimState, VisualKind};
use crate::editor::Editor;

fn ts(ed: &Editor) -> usize {
    ed.tab_size.max(1)
}
fn sw(ed: &Editor) -> usize {
    ed.tab_size.max(1)
}

fn read_only(vim: &mut VimState, host: &mut dyn Host) -> bool {
    if host.ed_ref().is_some_and(|ed| ed.read_only) {
        vim.set_error("E21: Cannot make changes, 'modifiable' is off");
        return true;
    }
    false
}

fn record_change(
    vim: &mut VimState,
    cmd: &Cmd,
    visual: Option<(VisualKind, usize, usize, bool)>,
    search: Option<(String, bool)>,
) {
    if vim.replaying > 0 {
        return;
    }
    vim.last_change = Some(Change {
        cmd: cmd.clone(),
        visual,
        search,
        insert: None,
    });
}

/// Store a yank or delete, honouring `clipboard=unnamedplus`.
fn store(vim: &mut VimState, host: &mut dyn Host, reg: Option<char>, value: Reg, how: Store) {
    let clip_default = vim.opts.clipboard_unnamed;
    let mut clip = HostClip(host);
    vim.regs.store(reg, value, how, &mut clip, clip_default);
}

pub(crate) fn read_reg(vim: &mut VimState, host: &mut dyn Host, reg: Option<char>) -> Option<Reg> {
    let file = host.file_name();
    let r = reg.unwrap_or(if vim.opts.clipboard_unnamed { '+' } else { '"' });
    let mut clip = HostClip(host);
    vim.regs.get(r, &mut clip, file.as_deref())
}

// ---------------------------------------------------------------------------
// Operator + target

#[allow(clippy::too_many_arguments)]
pub(crate) fn op_target(
    vim: &mut VimState,
    host: &mut dyn Host,
    cmd: &Cmd,
    op: Op,
    force: Option<Force>,
    target: Target,
    count: usize,
    has_count: bool,
    search_target: Option<(Pos, MKind, String, bool)>,
) {
    if op.changes_text() && op != Op::Fold && read_only(vim, host) {
        return;
    }
    let Some(ed) = host.ed_ref() else {
        return;
    };
    let cur = cursor(ed);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let cur = t.clamp_normal(cur);
    let mut big_motion = false;
    let mut no_adjust = false;
    let mut region = match target {
        Target::Lines => {
            let last = cur.line + count - 1;
            if count > 1 && cur.line >= t.last_line() {
                vim.beep();
                return;
            }
            let last = last.min(t.last_line());
            let start = if op == Op::Yank {
                cur
            } else {
                cur.min(Pos::new(cur.line, t.first_nonblank(cur.line)))
            };
            if op != Op::Yank
                && let Some(ed) = host.ed()
            {
                // The undo step remembers this cursor, as in Vim.
                ed.cursor_col = start.col;
            }
            Region {
                start,
                end: Pos::new(last, 0),
                kind: RKind::Line,
                nl: false,
            }
        }
        Target::Obj { obj, inner } => {
            let Some(a) = object_area(&t, cur, None, obj, inner, count, &vim.opts.iskeyword) else {
                vim.beep();
                return;
            };
            if a.failed {
                // Vim leaves the cursor where the object search stopped.
                let end = t.clamp_normal(a.end);
                if let Some(ed) = host.ed() {
                    set_cursor(ed, end);
                }
                vim.beep();
                return;
            }
            let kind = if a.linewise {
                RKind::Line
            } else {
                RKind::Char {
                    inclusive: a.inclusive,
                }
            };
            Region {
                start: a.start,
                end: a.end,
                kind,
                nl: false,
            }
        }
        Target::Motion(m) => {
            let res = if let Some((p, kind, _, _)) = &search_target {
                Some(exec::MRes {
                    pos: *p,
                    kind: *kind,
                    keep_curswant: false,
                    jump: true,
                    eol: false,
                    start: None,
                })
            } else {
                eval_motion(vim, host, m, count, has_count, Some(op))
            };
            let Some(res) = res else {
                vim.beep();
                return;
            };
            if res.jump {
                if let Some(ed) = host.ed() {
                    push_jump(vim, ed);
                }
            }
            big_motion = matches!(
                m,
                Motion::Percent
                    | Motion::SentFwd
                    | Motion::SentBack
                    | Motion::Mark { line: false, .. }
                    | Motion::Search(_)
                    | Motion::SearchNext
                    | Motion::SearchPrev
                    | Motion::ParaFwd
                    | Motion::ParaBack
            );
            no_adjust = matches!(m, Motion::BackChar | Motion::Left) && res.pos.line != cur.line;
            let start = res.start.unwrap_or(cur);
            let (s, e) = if res.start.is_some() {
                (start, res.pos)
            } else if res.pos < cur {
                (res.pos, cur)
            } else {
                (cur, res.pos)
            };
            let kind = match res.kind {
                MKind::Line => RKind::Line,
                MKind::Inclusive => RKind::Char { inclusive: true },
                MKind::Exclusive => RKind::Char { inclusive: false },
            };
            if res.eol {
                vim.curswant = Some(usize::MAX);
            }
            Region {
                start: s,
                end: e,
                kind,
                nl: false,
            }
        }
    };
    let Some(ed) = host.ed_ref() else {
        return;
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    // o_v, o_V, o_CTRL-V
    match force {
        Some(Force::Char) => {
            region.kind = match region.kind {
                RKind::Line => RKind::Char { inclusive: false },
                RKind::Char { inclusive } => RKind::Char {
                    inclusive: !inclusive,
                },
                k => k,
            };
        }
        Some(Force::Line) => region.kind = RKind::Line,
        Some(Force::Block) => {
            let ts = ts(ed);
            let a = t.vcol(region.start.line, region.start.col, ts);
            let b = t.vcol(region.end.line, region.end.col, ts);
            let left = a.min(b);
            let right = t
                .vcol_end(region.start.line, region.start.col, ts)
                .max(t.vcol_end(region.end.line, region.end.col, ts));
            region.kind = RKind::Block { left, right };
        }
        None => {}
    }
    // Vim's "exclusive" adjustment (:help exclusive).
    if let RKind::Char { inclusive: false } = region.kind
        && region.end.col == 0
        && region.end.line > region.start.line
        && !no_adjust
    {
        let start_in_indent = region.start.col <= t.first_nonblank(region.start.line);
        region.end.line -= 1;
        if start_in_indent {
            region.kind = RKind::Line;
        } else {
            let len = t.len(region.end.line);
            region.end.col = len;
            if len > 0 {
                region.end.col = len - 1;
                region.kind = RKind::Char { inclusive: true };
            }
        }
    }
    // d{motion} spanning lines with only blanks around becomes linewise.
    if op == Op::Delete
        && force.is_none()
        && region.start.line != region.end.line
        && let RKind::Char { inclusive } = region.kind
    {
        let after: String = t
            .line_chars(region.end.line)
            .iter()
            .skip(region.end.col + usize::from(inclusive))
            .collect();
        if after.trim().is_empty() && region.start.col <= t.first_nonblank(region.start.line) {
            region.kind = RKind::Line;
        }
    }
    // Empty exclusive region (`dh` at column 0): nothing to do.
    if let RKind::Char { inclusive: false } = region.kind
        && region.start == region.end
        && !matches!(
            op,
            Op::Change
                | Op::Indent
                | Op::Outdent
                | Op::Reindent
                | Op::Filter
                | Op::Format
                | Op::FormatKeep
                | Op::Comment
                | Op::Fold
                | Op::Tilde
                | Op::Lower
                | Op::Upper
                | Op::Rot13
        )
    {
        // Vim still makes an (empty) undo step for `x` on an empty line, and
        // `.` repeats that `x` afterwards.
        if op == Op::Delete
            && let Some(ed) = host.ed()
        {
            let line = ed.cursor_line;
            ed.save_undo_for_range(line, line);
            record_change(vim, cmd, None, search_target.map(|(_, _, p, f)| (p, f)));
        }
        return;
    }
    let search = search_target.map(|(_, _, p, f)| (p, f));
    if op.changes_text() && op != Op::Fold {
        record_change(vim, cmd, None, search);
    }
    apply(vim, host, cmd.reg, op, region, count, big_motion, false);
}

/// Apply an operator to a region.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    vim: &mut VimState,
    host: &mut dyn Host,
    reg: Option<char>,
    op: Op,
    region: Region,
    count: usize,
    big_motion: bool,
    visual: bool,
) {
    match op {
        Op::Yank => {
            let Some(ed) = host.ed() else {
                return;
            };
            let value = region_text(ed, &region);
            let (s, e) = region_bounds(ed, &region);
            ed.vim_buf.marks.insert('[', s);
            ed.vim_buf.marks.insert(']', e);
            let p = match region.kind {
                RKind::Block { left, .. } => {
                    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                    Pos::new(
                        region.start.line,
                        t.col_for_vcol(region.start.line, left, ts(ed)),
                    )
                }
                RKind::Line if visual => Pos::new(region.start.line, 0),
                _ => region.start,
            };
            let flash = (s, e, matches!(region.kind, RKind::Line));
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let p = t.clamp_normal(p);
            set_cursor(ed, p);
            let lines = value.text.matches('\n').count() + usize::from(value.kind != RegKind::Line);
            store(vim, host, reg, value, Store::Yank);
            if lines > vim.opts.report && matches!(region.kind, RKind::Line | RKind::Block { .. }) {
                vim.set_message(format!(
                    "{} lines yanked",
                    region.end.line - region.start.line + 1
                ));
            }
            if vim.opts.highlight_yank {
                let _ = host.act(Act::YankFlash {
                    start: flash.0,
                    end: flash.1,
                    linewise: flash.2,
                });
            }
        }
        Op::Delete => delete(vim, host, reg, region, big_motion, false),
        Op::Change => change(vim, host, reg, region, count, big_motion),
        Op::Indent | Op::Outdent => {
            let times = if visual { count } else { 1 };
            shift(vim, host, region, op == Op::Indent, times);
        }
        Op::Tilde | Op::Lower | Op::Upper | Op::Rot13 => {
            let region = match region.kind {
                RKind::Char { inclusive: false }
                    if region.start == region.end && region.start == Pos::new(0, 0) =>
                {
                    Region {
                        kind: RKind::Char { inclusive: true },
                        ..region
                    }
                }
                RKind::Char { inclusive: false }
                    if region.start == region.end && region.start.col == 0 =>
                {
                    let len = host.ed_ref().map_or(0, |ed| {
                        Text::of(&ed.rope, ed.vim_buf.trailing_empty).len(region.start.line)
                    });
                    if len == 0 {
                        return;
                    }
                    Region {
                        end: Pos::new(region.start.line, len - 1),
                        kind: RKind::Char { inclusive: true },
                        ..region
                    }
                }
                RKind::Char { inclusive: false } if region.start == region.end => return,
                _ => region,
            };
            case_op(vim, host, region, op)
        }
        Op::Reindent => {
            let Some(ed) = host.ed() else {
                return;
            };
            lines_via_selection(ed, region.start.line, region.end.line, |ed| {
                ed.reindent_lines()
            });
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            set_cursor(
                ed,
                Pos::new(region.start.line, t.first_nonblank(region.start.line)),
            );
        }
        Op::Comment => {
            let Some(ed) = host.ed() else {
                return;
            };
            lines_via_selection(ed, region.start.line, region.end.line, |ed| {
                ed.toggle_line_comment()
            });
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            set_cursor(
                ed,
                Pos::new(region.start.line, t.first_nonblank(region.start.line)),
            );
        }
        Op::Format | Op::FormatKeep => format(vim, host, region, op == Op::FormatKeep),
        Op::Fold => {
            let Some(ed) = host.ed() else {
                return;
            };
            let (s, e) = region.line_span();
            if e > s {
                ed.collapse_range(s, e + 1);
                set_cursor(
                    ed,
                    Pos::new(
                        s,
                        ed.cursor_col
                            .min(Text::of(&ed.rope, ed.vim_buf.trailing_empty).max_normal_col(s)),
                    ),
                );
            }
        }
        Op::Filter => {
            let (s, e) = region.line_span();
            let range = if s == e {
                ".".to_string()
            } else {
                format!(".,.+{}", e - s)
            };
            if let Some(ed) = host.ed() {
                set_cursor(ed, Pos::new(s, ed.cursor_col));
            }
            super::cmdline::open(vim, host, ':', None, format!("{range}!"));
        }
    }
}

fn region_bounds(ed: &Editor, r: &Region) -> (Pos, Pos) {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    match r.kind {
        RKind::Line => (
            Pos::new(r.start.line, 0),
            Pos::new(r.end.line, t.len(r.end.line)),
        ),
        RKind::Char { inclusive } => {
            let mut e = r.end;
            if !inclusive && e.col > 0 {
                e.col -= 1;
            }
            (r.start, e)
        }
        RKind::Block { left, right } => {
            let ts = ts(ed);
            let sc = t.col_for_vcol(r.start.line, left, ts);
            let ec = if right == usize::MAX {
                t.len(r.end.line)
            } else {
                t.col_for_vcol(r.end.line, right.saturating_sub(1), ts)
            };
            (Pos::new(r.start.line, sc), Pos::new(r.end.line, ec))
        }
    }
}

/// Run an editor command on whole lines through a temporary selection.
fn lines_via_selection(ed: &mut Editor, start: usize, end: usize, f: impl FnOnce(&mut Editor)) {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let saved_sel = (ed.vim_selection.take(), ed.vim_block.take());
    ed.selection_anchor = Some((start, 0));
    ed.cursor_line = end;
    ed.cursor_col = t.len(end);
    if start == end && t.len(end) == 0 {
        ed.selection_anchor = None;
    }
    f(ed);
    ed.selection_anchor = None;
    let _ = saved_sel;
}

/// Char offsets for deleting whole lines `s..=e`, keeping Vim's EOL rules.
fn line_delete_span(t: &Text<'_>, s: usize, e: usize) -> (usize, usize) {
    let rope_len = t.rope.len_chars();
    let end = t.line_next_start(e);
    let ends_with_eol = rope_len > 0 && matches!(t.rope.char(rope_len - 1), '\n' | '\r');
    if end >= rope_len && !ends_with_eol && s > 0 {
        // The last line has no line break: remove the one before it.
        return (t.line_end(s - 1), rope_len);
    }
    (t.line_start(s), end)
}

fn delete(
    vim: &mut VimState,
    host: &mut dyn Host,
    reg: Option<char>,
    region: Region,
    big_motion: bool,
    for_change: bool,
) {
    let Some(ed) = host.ed() else {
        return;
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    if matches!(region.kind, RKind::Line)
        && t.lines() == 1
        && t.len(0) == 0
        && !for_change
        && ed.vim_buf.ml_empty
    {
        vim.set_message("--No lines in buffer--");
        return;
    }
    let value = region_text(ed, &region);
    if value.text.is_empty() && matches!(region.kind, RKind::Char { .. }) && !for_change {
        return;
    }
    let small = matches!(region.kind, RKind::Char { .. })
        && region.start.line == region.end.line
        && !big_motion;
    let mut tx = Tx::default();
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    match region.kind {
        RKind::Char { .. } => {
            let (s, e) = region.char_span(&t);
            tx.edit(ed, s, e, "");
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let p = t.pos(s);
            let p = if for_change { p } else { t.clamp_normal(p) };
            set_cursor(ed, p);
        }
        RKind::Line => {
            let (s, e) = line_delete_span(&t, region.start.line, region.end.line);
            let all = region.start.line == 0 && region.end.line >= t.last_line();
            if e > s {
                tx.edit(ed, s, e, "");
            } else if !for_change {
                // Deleting the only (empty) line still makes an undo step.
                let line = ed.cursor_line;
                ed.save_undo_for_range(line, line);
            }
            if all && !for_change {
                ed.vim_buf.ml_empty = true;
                ed.vim_buf.trailing_empty = false;
            }
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let line = region.start.line.min(t.last_line());
            set_cursor(ed, Pos::new(line, t.first_nonblank(line)));
        }
        RKind::Block { left, right } => {
            let ts = ts(ed);
            for line in (region.start.line..=region.end.line).rev() {
                let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                let b = super::exec::block_prep(&t, line, left, right, ts, true);
                if b.textlen == 0 {
                    continue;
                }
                let base = t.line_start(line);
                tx.edit(
                    ed,
                    base + b.textcol,
                    base + b.textcol + b.textlen,
                    &" ".repeat(b.startspaces + b.endspaces),
                );
            }
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let col = t.col_for_vcol(region.start.line, left, ts);
            let p = Pos::new(region.start.line, col);
            set_cursor(
                ed,
                if for_change {
                    t.clamp_insert(p)
                } else {
                    t.clamp_normal(p)
                },
            );
        }
    }
    let lines = region.end.line - region.start.line + 1;
    tx.finish(ed);
    store(vim, host, reg, value, Store::Delete { small });
    if matches!(region.kind, RKind::Line) && lines > vim.opts.report && !for_change {
        vim.set_message(format!("{lines} fewer lines"));
    }
}

fn change(
    vim: &mut VimState,
    host: &mut dyn Host,
    reg: Option<char>,
    region: Region,
    count: usize,
    big_motion: bool,
) {
    let autoindent = vim.opts.autoindent;
    match region.kind {
        RKind::Line => {
            let Some(ed) = host.ed() else {
                return;
            };
            let value = region_text(ed, &region);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let indent: String = if autoindent {
                t.line_chars(region.start.line)
                    .into_iter()
                    .take_while(|&c| c == ' ' || c == '\t')
                    .collect()
            } else {
                String::new()
            };
            let s = t.line_start(region.start.line);
            let e = t.line_end(region.end.line);
            let mut tx = Tx::default();
            tx.edit(ed, s, e, &indent);
            let col = indent.chars().count();
            set_cursor(ed, Pos::new(region.start.line, col));
            tx.finish(ed);
            store(vim, host, reg, value, Store::Delete { small: false });
            insert::begin(vim, host, InsertKind::Before, 1, true, None);
            if !indent.is_empty()
                && let Some(s) = &mut vim.insert
            {
                s.auto_indent = Some(region.start.line);
            }
        }
        RKind::Block { left, right } => {
            delete(vim, host, reg, region, big_motion, true);
            let lines = (region.start.line + 1..=region.end.line).collect::<Vec<_>>();
            insert::begin(
                vim,
                host,
                InsertKind::Before,
                1,
                true,
                Some(insert::BlockInsert {
                    lines,
                    top: region.start.line,
                    left,
                    vcol: left,
                    append: false,
                    eol: right == usize::MAX,
                }),
            );
        }
        RKind::Char { .. } => {
            delete(vim, host, reg, region, big_motion, true);
            let _ = count;
            insert::begin(vim, host, InsertKind::Before, 1, true, None);
        }
    }
}

fn indent_string(ed: &Editor, vcols: usize) -> String {
    if ed.use_tabs {
        let ts = ts(ed);
        format!("{}{}", "\t".repeat(vcols / ts), " ".repeat(vcols % ts))
    } else {
        " ".repeat(vcols)
    }
}

pub(crate) fn shift_lines(
    vim: &VimState,
    ed: &mut Editor,
    tx: &mut Tx,
    s: usize,
    e: usize,
    right: bool,
    times: usize,
) {
    let width = sw(ed) * times.max(1);
    for line in s..=e {
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        if line > t.last_line() {
            break;
        }
        let chars = t.line_chars(line);
        if chars.is_empty() {
            continue;
        }
        let ws = chars.iter().take_while(|&&c| c == ' ' || c == '\t').count();
        if right && ws == chars.len() {
            continue;
        }
        let cur = t.vcol(line, ws, ts(ed));
        let new = if right {
            if vim.opts.shiftround {
                (cur / sw(ed) + times.max(1)) * sw(ed)
            } else {
                cur + width
            }
        } else if vim.opts.shiftround && cur % sw(ed) != 0 {
            (cur / sw(ed)).saturating_sub(times.max(1) - 1) * sw(ed)
        } else {
            cur.saturating_sub(width)
        };
        let ind = indent_string(ed, new);
        if chars[..ws].iter().collect::<String>() != ind {
            let base = t.line_start(line);
            tx.edit(ed, base, base + ws, &ind);
        }
    }
}

fn shift(vim: &mut VimState, host: &mut dyn Host, region: Region, right: bool, times: usize) {
    let Some(ed) = host.ed() else {
        return;
    };
    let mut tx = Tx::default();
    let (s, e) = region.line_span();
    if let RKind::Block { left, .. } = region.kind {
        // Block shift moves the text starting at the block's left column.
        let ts = ts(ed);
        let start_col = Text::of(&ed.rope, ed.vim_buf.trailing_empty).col_for_vcol(s, left, ts);
        for line in s..=e {
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            if t.len(line) == 0 || t.line_vwidth(line, ts) < left {
                continue;
            }
            let col = t.col_for_vcol(line, left, ts);
            let base = t.line_start(line);
            if right {
                let chars = t.line_chars(line);
                // The first character starting at or after the block edge.
                let mut c = 0;
                let mut v = 0;
                while c < chars.len() && v < left {
                    v += char_width(chars[c], v, ts);
                    c += 1;
                }
                // White space just before it (a split tab included) joins.
                let mut ws_start = c;
                while ws_start > 0 && matches!(chars[ws_start - 1], ' ' | '\t') {
                    ws_start -= 1;
                }
                let mut run_end = c;
                while run_end < chars.len() && matches!(chars[run_end], ' ' | '\t') {
                    run_end += 1;
                }
                let ws_vcol = t.vcol(line, ws_start, ts);
                let total = t.vcol(line, run_end, ts) - ws_vcol + sw(ed) * times;
                let fill = if ed.use_tabs {
                    let first_tab = ts - ws_vcol % ts;
                    if total >= first_tab {
                        format!(
                            "{}{}",
                            "\t".repeat(1 + (total - first_tab) / ts),
                            " ".repeat((total - first_tab) % ts)
                        )
                    } else {
                        " ".repeat(total)
                    }
                } else {
                    " ".repeat(total)
                };
                tx.edit(ed, base + ws_start, base + run_end, &fill);
            } else {
                let chars = t.line_chars(line);
                let n = chars[col..]
                    .iter()
                    .take_while(|&&c| c == ' ' || c == '\t')
                    .count()
                    .min(sw(ed) * times);
                tx.edit(ed, base + col, base + col + n, "");
            }
        }
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        set_cursor(ed, t.clamp_normal(Pos::new(s, start_col)));
    } else {
        shift_lines(vim, ed, &mut tx, s, e, right, times);
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        set_cursor(ed, Pos::new(s, t.first_nonblank(s)));
    }
    let n = e - s + 1;
    tx.finish(ed);
    if n > vim.opts.report {
        let dir = if right { '>' } else { '<' };
        vim.set_message(format!(
            "{n} lines {dir}ed {} time{}",
            times,
            if times == 1 { "" } else { "s" }
        ));
    }
}

fn transform(op: Op, s: &str) -> String {
    s.chars()
        .map(|c| match op {
            Op::Upper => c.to_uppercase().collect::<String>(),
            Op::Lower => c.to_lowercase().collect(),
            Op::Tilde => {
                if c.is_uppercase() {
                    c.to_lowercase().collect()
                } else if c.is_lowercase() {
                    c.to_uppercase().collect()
                } else {
                    c.to_string()
                }
            }
            Op::Rot13 => {
                let r = match c {
                    'a'..='z' => (((c as u8 - b'a') + 13) % 26 + b'a') as char,
                    'A'..='Z' => (((c as u8 - b'A') + 13) % 26 + b'A') as char,
                    _ => c,
                };
                r.to_string()
            }
            _ => c.to_string(),
        })
        .collect()
}

fn case_op(vim: &mut VimState, host: &mut dyn Host, region: Region, op: Op) {
    let Some(ed) = host.ed() else {
        return;
    };
    let mut tx = Tx::default();
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let cursor_to;
    match region.kind {
        RKind::Char { .. } => {
            let (s, e) = region.char_span(&t);
            let old = t.slice(s, e);
            let new = transform(op, &old);
            if new != old {
                tx.edit(ed, s, e, &new);
            }
            cursor_to = region.start;
        }
        RKind::Line => {
            let s = t.line_start(region.start.line);
            let e = t.line_end(region.end.line);
            let old = t.slice(s, e);
            let new = transform(op, &old);
            if new != old {
                tx.edit(ed, s, e, &new);
            }
            cursor_to = region.start;
        }
        RKind::Block { left, right } => {
            let ts = ts(ed);
            for line in region.start.line..=region.end.line {
                let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                if let Some((sc, ec)) = block_cols(&t, line, left, right, ts) {
                    let base = t.line_start(line);
                    let old = t.slice(base + sc, base + ec);
                    let new = transform(op, &old);
                    if new != old {
                        tx.edit(ed, base + sc, base + ec, &new);
                    }
                }
            }
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            cursor_to = Pos::new(
                region.start.line,
                t.col_for_vcol(region.start.line, left, ts),
            );
        }
    }
    tx.finish(ed);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    set_cursor(ed, t.clamp_normal(cursor_to));
    let _ = vim;
}

/// `gq`/`gw`: reflow paragraphs to 'textwidth' (79 when unset), keeping
/// the indent and comment leader of each paragraph's first line.
fn format(vim: &mut VimState, host: &mut dyn Host, region: Region, keep_cursor: bool) {
    let tw = if vim.opts.textwidth > 0 {
        vim.opts.textwidth
    } else {
        79
    };
    let Some(ed) = host.ed() else {
        return;
    };
    let saved = cursor(ed);
    let (s, e) = region.line_span();
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let leaders = ["///", "//!", "//", "#", "--", ";", "*", ">", "%"];
    let mut out: Vec<String> = Vec::new();
    let mut line = s;
    while line <= e {
        let text = t.line_string(line);
        if text.trim().is_empty() {
            out.push(text);
            line += 1;
            continue;
        }
        let indent: String = text.chars().take_while(|c| c.is_whitespace()).collect();
        let rest = &text[indent.len()..];
        let leader = leaders
            .iter()
            .find(|l| rest.starts_with(**l))
            .map(|l| {
                let after = &rest[l.len()..];
                let sp: String = after.chars().take_while(|c| *c == ' ').collect();
                format!("{l}{sp}")
            })
            .unwrap_or_default();
        let prefix = format!("{indent}{leader}");
        let mut words: Vec<String> = Vec::new();
        while line <= e {
            let l = t.line_string(line);
            if l.trim().is_empty() {
                break;
            }
            let li: String = l.chars().take_while(|c| c.is_whitespace()).collect();
            let body = &l[li.len()..];
            let body = if !leader.is_empty() && body.starts_with(leader.trim_end()) {
                &body[leader.trim_end().len()..]
            } else if leader.is_empty() {
                body
            } else {
                break;
            };
            words.extend(body.split_whitespace().map(str::to_string));
            line += 1;
        }
        let mut cur = prefix.clone();
        let mut has_word = false;
        let width = |s: &str| s.chars().map(|c| char_width(c, 0, ts(ed))).sum::<usize>();
        for w in words {
            if has_word && width(&cur) + 1 + width(&w) > tw {
                out.push(cur);
                cur = prefix.clone();
                has_word = false;
            }
            if has_word {
                cur.push(' ');
            }
            cur.push_str(&w);
            has_word = true;
        }
        out.push(cur.trim_end().to_string());
    }
    let start = t.line_start(s);
    let end = t.line_end(e);
    let new = out.join("\n");
    let mut tx = Tx::default();
    if t.slice(start, end) != new {
        tx.edit(ed, start, end, &new);
    }
    tx.finish(ed);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    if keep_cursor {
        set_cursor(ed, t.clamp_normal(saved));
    } else {
        let last = (s + out.len()).saturating_sub(1).min(t.last_line());
        set_cursor(ed, Pos::new(last, t.first_nonblank(last)));
    }
}

// ---------------------------------------------------------------------------
// Visual operators

pub(crate) fn visual_op(vim: &mut VimState, host: &mut dyn Host, cmd: &Cmd, op: Op) {
    let Some(ed) = host.ed() else {
        return;
    };
    let region = visual_region(vim, ed);
    let kind = vim.visual_kind;
    let lines = region.end.line - region.start.line;
    let t0 = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let cols = match region.kind {
        RKind::Char { .. } if lines == 0 => {
            let ts = ts(ed);
            t0.vcol_end(region.end.line, region.end.col, ts)
                .saturating_sub(t0.vcol(region.start.line, region.start.col, ts))
        }
        RKind::Char { .. } => region.end.col,
        RKind::Block { left, right } => {
            if right == usize::MAX {
                usize::MAX
            } else {
                right - left
            }
        }
        _ => 0,
    };
    let eol = vim.curswant_eol;
    save_last_visual_marks(vim, ed);
    if op.changes_text() && op != Op::Fold && read_only(vim, host) {
        exit_visual(vim, host);
        return;
    }
    let mut region = region;
    if op == Op::Change && kind == VisualKind::Block && eol {
        region.kind = RKind::Block {
            left: match region.kind {
                RKind::Block { left, .. } => left,
                _ => 0,
            },
            right: usize::MAX,
        };
    }
    // Leave Visual mode first so the operator sees Normal-mode state.
    exit_visual_keep(vim, host);
    if op.changes_text() && op != Op::Fold && op != Op::Filter {
        record_change(vim, cmd, Some((kind, lines, cols, eol)), None);
    }
    if op == Op::Filter {
        super::cmdline::open(vim, host, ':', None, "'<,'>!".into());
        return;
    }
    let count = cmd.count1();
    apply(vim, host, cmd.reg, op, region, count, true, true);
}

fn save_last_visual_marks(vim: &VimState, ed: &mut Editor) {
    super::exec::save_last_visual(vim, ed);
}

/// Exit Visual mode without moving the cursor back onto the line.
fn exit_visual_keep(vim: &mut VimState, host: &mut dyn Host) {
    if let Some(ed) = host.ed() {
        save_last_visual_marks(vim, ed);
        ed.vim_selection = None;
        ed.vim_block = None;
        ed.selection_anchor = None;
    }
    vim.mode = Mode::Normal;
}

/// Rebuild a Visual area of the recorded size at the cursor for `.`.
pub(crate) fn replay_visual(
    vim: &mut VimState,
    ed: &Editor,
    (kind, lines, cols, eol): (VisualKind, usize, usize, bool),
) -> Region {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let cur = t.clamp_normal(cursor(ed));
    let end_line = (cur.line + lines).min(t.last_line());
    vim.visual_kind = kind;
    match kind {
        VisualKind::Line => Region {
            start: Pos::new(cur.line, 0),
            end: Pos::new(end_line, 0),
            kind: RKind::Line,
            nl: false,
        },
        VisualKind::Char => {
            let end = if lines == 0 {
                let ts = ts(ed);
                let want = (t.vcol(cur.line, cur.col, ts) + cols).saturating_sub(1);
                Pos::new(cur.line, t.col_for_vcol(cur.line, want, ts))
            } else {
                Pos::new(end_line, cols)
            };
            Region {
                start: cur,
                end: t.clamp_insert(end),
                kind: RKind::Char { inclusive: true },
                nl: true,
            }
        }
        VisualKind::Block => {
            let ts = ts(ed);
            let left = t.vcol(cur.line, cur.col, ts);
            let right = if eol || cols == usize::MAX {
                usize::MAX
            } else {
                left + cols
            };
            Region {
                start: Pos::new(cur.line, 0),
                end: Pos::new(end_line, 0),
                kind: RKind::Block { left, right },
                nl: false,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Simple commands

fn op_cmd(cmd: &Cmd, op: Op, target: Target) -> Cmd {
    Cmd {
        reg: cmd.reg,
        count: cmd.count,
        action: Action::Op {
            op,
            force: None,
            target,
            count2: None,
        },
    }
}

pub(crate) fn simple(
    vim: &mut VimState,
    host: &mut dyn Host,
    cmd: &Cmd,
    s: Simple,
    count: usize,
    has_count: bool,
) {
    let visual = vim.is_visual();
    if visual {
        if visual_simple(vim, host, cmd, s, count) {
            return;
        }
    }
    match s {
        Simple::Cancel => {
            if visual {
                exit_visual(vim, host);
            }
            if let Some(ed) = host.ed() {
                ed.extra_cursors.clear();
                ed.extra_cursor_anchors.clear();
            }
            let _ = host.act(Act::Command("vim_escape".into()));
        }
        Simple::Insert(kind) => {
            if read_only(vim, host) {
                return;
            }
            record_change(vim, cmd, None, None);
            insert::begin(vim, host, kind, count, false, None);
        }
        Simple::Visual(force) => {
            let kind = match force {
                Force::Char => VisualKind::Char,
                Force::Line => VisualKind::Line,
                Force::Block => VisualKind::Block,
            };
            if visual {
                if vim.visual_kind == kind {
                    exit_visual(vim, host);
                    return;
                }
                vim.visual_kind = kind;
                vim.mode = Mode::Visual(kind);
                if let Some(ed) = host.ed() {
                    sync_visual(vim, ed);
                }
                return;
            }
            enter_visual(vim, host, kind);
            if has_count && kind != VisualKind::Line {
                if let Some(ed) = host.ed() {
                    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                    let c = cursor(ed);
                    set_cursor(
                        ed,
                        Pos::new(c.line, (c.col + count - 1).min(t.max_normal_col(c.line))),
                    );
                    sync_visual(vim, ed);
                }
            } else if has_count {
                if let Some(ed) = host.ed() {
                    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                    let c = cursor(ed);
                    let line = (c.line + count - 1).min(t.last_line());
                    set_cursor(ed, Pos::new(line, c.col.min(t.max_normal_col(line))));
                    sync_visual(vim, ed);
                }
            }
        }
        Simple::Reselect => {
            let Some(ed) = host.ed() else {
                return;
            };
            let Some(lv) = ed.vim_buf.last_visual else {
                vim.set_error("E19: no previous Visual area");
                return;
            };
            let current = if visual {
                Some(super::LastVisual {
                    kind: vim.visual_kind,
                    start: vim.visual_anchor,
                    end: cursor(ed),
                    curswant_eol: vim.curswant_eol,
                })
            } else {
                None
            };
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            vim.visual_anchor = t.clamp_insert(lv.start);
            vim.visual_kind = lv.kind;
            vim.curswant_eol = lv.curswant_eol;
            vim.mode = Mode::Visual(lv.kind);
            set_cursor(ed, t.clamp_insert(lv.end));
            if let Some(c) = current {
                ed.vim_buf.last_visual = Some(c);
            }
            update_curswant(vim, ed);
            if lv.curswant_eol {
                vim.curswant = Some(usize::MAX);
            }
            sync_visual(vim, ed);
        }
        Simple::DeleteChar => {
            let c = op_cmd(cmd, Op::Delete, Target::Motion(Motion::Right));
            run_op(vim, host, &c);
        }
        Simple::DeleteCharBefore => {
            let c = op_cmd(cmd, Op::Delete, Target::Motion(Motion::Left));
            run_op(vim, host, &c);
        }
        Simple::Substitute => {
            let c = op_cmd(cmd, Op::Change, Target::Motion(Motion::Right));
            run_op_or_insert(vim, host, &c);
        }
        Simple::SubstituteLine => {
            let c = op_cmd(cmd, Op::Change, Target::Lines);
            run_op(vim, host, &c);
        }
        Simple::DeleteToEnd => {
            let c = op_cmd(cmd, Op::Delete, Target::Motion(Motion::LineEnd));
            run_op(vim, host, &c);
        }
        Simple::ChangeToEnd => {
            let c = op_cmd(cmd, Op::Change, Target::Motion(Motion::LineEnd));
            run_op_or_insert(vim, host, &c);
        }
        Simple::YankLine => {
            let c = op_cmd(cmd, Op::Yank, Target::Lines);
            run_op(vim, host, &c);
        }
        Simple::Replace(c) => {
            if read_only(vim, host) {
                return;
            }
            replace_chars(vim, host, cmd, c, count);
        }
        Simple::ReplaceMode | Simple::VirtualReplace => {
            if read_only(vim, host) {
                return;
            }
            record_change(vim, cmd, None, None);
            insert::begin_replace(vim, host, count);
        }
        Simple::ToggleCase => {
            if read_only(vim, host) {
                return;
            }
            if vim.opts.tildeop {
                return;
            }
            let Some(ed) = host.ed() else {
                return;
            };
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let c = t.clamp_normal(cursor(ed));
            let len = t.len(c.line);
            if len == 0 {
                return;
            }
            let n = count.min(len - c.col);
            record_change(vim, cmd, None, None);
            let s = t.idx(c);
            let old = t.slice(s, s + n);
            let new = transform(Op::Tilde, &old);
            let mut tx = Tx::default();
            if old != new {
                tx.edit(ed, s, s + n, &new);
            }
            tx.finish(ed);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            set_cursor(ed, t.clamp_normal(Pos::new(c.line, c.col + n)));
            update_curswant(vim, ed);
        }
        Simple::Join(spaces) => {
            if read_only(vim, host) {
                return;
            }
            record_change(vim, cmd, None, None);
            let Some(ed) = host.ed() else {
                return;
            };
            let line = cursor(ed).line;
            let n = count.max(2);
            join(vim, ed, line, line + n - 1, spaces);
        }
        Simple::Put {
            before,
            cursor_after,
            indent,
        } => {
            if read_only(vim, host) {
                return;
            }
            record_change(vim, cmd, None, None);
            put(vim, host, cmd.reg, before, cursor_after, indent, count);
        }
        Simple::UndoTime(newer) => {
            if read_only(vim, host) {
                return;
            }
            let Some(ed) = host.ed() else {
                return;
            };
            if !super::undo_tree::travel(ed, newer, count) {
                vim.set_message(if newer {
                    "Already at newest change"
                } else {
                    "Already at oldest change"
                });
            }
            clamp_normal(ed);
            update_curswant(vim, ed);
        }
        Simple::Undo | Simple::Redo
            if host.ed_ref().is_some_and(super::undo_tree::after_travel) =>
        {
            let newer = matches!(s, Simple::Redo);
            let Some(ed) = host.ed() else {
                return;
            };
            if !super::undo_tree::travel(ed, newer, count) {
                vim.set_message(if newer {
                    "Already at newest change"
                } else {
                    "Already at oldest change"
                });
            }
            clamp_normal(ed);
            update_curswant(vim, ed);
        }
        Simple::Undo => {
            let Some(ed) = host.ed() else {
                return;
            };
            let mut done = 0;
            for _ in 0..count {
                if !ed.can_undo() {
                    break;
                }
                ed.undo();
                done += 1;
            }
            ed.selection_anchor = None;
            clamp_normal(ed);
            update_curswant(vim, ed);
            if done == 0 {
                vim.set_message("Already at oldest change");
            }
        }
        Simple::Redo => {
            let Some(ed) = host.ed() else {
                return;
            };
            let mut done = 0;
            for _ in 0..count {
                if !ed.can_redo() {
                    break;
                }
                ed.redo();
                done += 1;
            }
            ed.selection_anchor = None;
            clamp_normal(ed);
            update_curswant(vim, ed);
            if done == 0 {
                vim.set_message("Already at newest change");
            }
        }
        Simple::UndoLine => {
            let Some(ed) = host.ed() else {
                return;
            };
            let Some((line, text)) = ed.vim_buf.line_undo.clone() else {
                return;
            };
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            if line > t.last_line() {
                return;
            }
            let current = t.line_string(line);
            let (s, e) = (t.line_start(line), t.line_end(line));
            let mut tx = Tx::default();
            tx.edit(ed, s, e, &text);
            tx.finish(ed);
            ed.vim_buf.line_undo = Some((line, current));
            set_cursor(ed, Pos::new(line, 0));
        }
        Simple::Repeat => repeat(vim, host, if has_count { Some(count) } else { None }),
        Simple::SetMark(c) => {
            let Some(ed) = host.ed() else {
                return;
            };
            let p = cursor(ed);
            if !set_mark(vim, ed, c, p) {
                vim.set_error("E191: Argument must be a letter or forward/backward quote");
            }
        }
        Simple::Record(r) => {
            if vim.recording.is_some() {
                return;
            }
            vim.recording = Some((r, Vec::new()));
        }
        Simple::Play(r) => play(vim, host, r, count),
        Simple::WriteQuit => {
            let _ = super::ex::run(vim, host, "x");
        }
        Simple::QuitNoWrite => {
            let _ = super::ex::run(vim, host, "q!");
        }
        Simple::Increment(up) | Simple::IncrementSeq(up) => {
            if read_only(vim, host) {
                return;
            }
            record_change(vim, cmd, None, None);
            let Some(ed) = host.ed() else {
                return;
            };
            let c = cursor(ed);
            let delta = if up { count as i128 } else { -(count as i128) };
            if let Some(col) = increment_at(vim, ed, c.line, c.col, usize::MAX, delta) {
                set_cursor(ed, Pos::new(c.line, col));
                update_curswant(vim, ed);
            }
        }
        Simple::Scroll(sc) => scroll(vim, host, sc, count, has_count),
        Simple::Fold(f) => fold(vim, host, f, count),
        Simple::NextTab => {
            let _ = host.act(Act::TabNext(has_count.then_some(count)));
        }
        Simple::PrevTab => {
            let _ = host.act(Act::TabPrev(count));
        }
        Simple::Definition => cmd_act(vim, host, "go_to_definition"),
        Simple::Declaration => cmd_act(vim, host, "go_to_declaration"),
        Simple::TypeDefinition => cmd_act(vim, host, "go_to_type_definition"),
        Simple::References => cmd_act(vim, host, "find_references"),
        Simple::Hover => cmd_act(vim, host, "vim_hover"),
        Simple::CodeAction => cmd_act(vim, host, "code_action"),
        Simple::GotoFile => cmd_act(vim, host, "open_file_at_cursor"),
        Simple::OpenUrl => cmd_act(vim, host, "vim_open_url"),
        Simple::NextDiagnostic => {
            for _ in 0..count {
                cmd_act(vim, host, "next_diagnostic");
            }
        }
        Simple::PrevDiagnostic => {
            for _ in 0..count {
                cmd_act(vim, host, "prev_diagnostic");
            }
        }
        Simple::NextChange => {
            for _ in 0..count {
                cmd_act(vim, host, "next_change");
            }
        }
        Simple::PrevChange => {
            for _ in 0..count {
                cmd_act(vim, host, "prev_change");
            }
        }
        Simple::JumpOlder => jump_list(vim, host, true, count),
        Simple::JumpNewer => jump_list(vim, host, false, count),
        Simple::Alternate => {
            let _ = host.act(Act::Alternate);
        }
        Simple::FileInfo => file_info(vim, host),
        Simple::WordCount => word_count(vim, host),
        Simple::CharInfo => {
            let Some(ed) = host.ed_ref() else {
                return;
            };
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            match t.char_at(cursor(ed)) {
                Some(c) => {
                    let cp = c as u32;
                    vim.set_message(format!(
                        "<{}> {cp}, Hex {cp:02x}, Oct {cp:03o}",
                        if c.is_control() {
                            format!("^{}", ((cp as u8) ^ 0x40) as char)
                        } else {
                            c.to_string()
                        }
                    ));
                }
                None => vim.set_message("NUL"),
            }
        }
        Simple::ChangeOlder | Simple::ChangeNewer => {
            let Some(ed) = host.ed() else {
                return;
            };
            let changes = &ed.vim_buf.changes;
            if changes.is_empty() {
                vim.set_error("E664: changelist is empty");
                return;
            }
            let idx = ed.vim_buf.change_idx;
            let target = if s == Simple::ChangeOlder {
                idx.checked_sub(count)
            } else {
                Some(idx + count)
            };
            match target.filter(|&i| i < changes.len()) {
                Some(i) => {
                    let p = changes[i];
                    ed.vim_buf.change_idx = i;
                    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                    set_cursor(ed, t.clamp_normal(p));
                    update_curswant(vim, ed);
                }
                None => vim.set_error(if s == Simple::ChangeOlder {
                    "E662: At start of changelist"
                } else {
                    "E663: At end of changelist"
                }),
            }
        }
        Simple::RepeatSub => {
            let _ = super::ex::run(vim, host, "s");
        }
        Simple::RepeatSubAll => {
            let _ = super::ex::run(vim, host, "%s//~/&");
        }
        Simple::CmdLine => {
            let init = if has_count {
                if count == 1 {
                    ".".to_string()
                } else {
                    format!(".,.+{}", count - 1)
                }
            } else {
                String::new()
            };
            super::cmdline::open(vim, host, ':', None, init);
        }
        Simple::CmdWindow(kind) => super::cmdline::open_window(vim, host, kind, String::new()),
        Simple::Window(w) => {
            if let Err(e) = host.act(Act::Window(w, count)) {
                vim.set_error(e);
            }
        }
        Simple::Redraw => vim.message = None,
        Simple::BlankLine(below) => {
            if read_only(vim, host) {
                return;
            }
            record_change(vim, cmd, None, None);
            let Some(ed) = host.ed() else {
                return;
            };
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let c = cursor(ed);
            let at = if below {
                t.line_next_start(c.line)
            } else {
                t.line_start(c.line)
            };
            let mut text = "\n".repeat(count);
            let mut tx = Tx::default();
            if below
                && at >= ed.rope.len_chars()
                && !matches!(ed.rope.chars().last(), Some('\n' | '\r') | None)
            {
                text = "\n".repeat(count);
            }
            tx.edit(ed, at, at, &text);
            tx.finish(ed);
            let line = if below { c.line } else { c.line + count };
            set_cursor(ed, Pos::new(line, c.col));
        }
        Simple::VisualSwap
        | Simple::VisualSwapCorner
        | Simple::VisualBlockInsert(_)
        | Simple::VisualSearch(_) => {}
    }
}

fn cmd_act(vim: &mut VimState, host: &mut dyn Host, id: &str) {
    if let Err(e) = host.act(Act::Command(id.into())) {
        vim.set_error(e);
    }
}

fn run_op(vim: &mut VimState, host: &mut dyn Host, c: &Cmd) {
    if let Action::Op {
        op, force, target, ..
    } = c.action.clone()
    {
        op_target(
            vim,
            host,
            c,
            op,
            force,
            target,
            c.count1(),
            c.has_count(),
            None,
        );
    }
}

/// `s`/`C` on an empty line still enter Insert mode.
fn run_op_or_insert(vim: &mut VimState, host: &mut dyn Host, c: &Cmd) {
    if read_only(vim, host) {
        return;
    }
    let empty = host
        .ed_ref()
        .is_some_and(|ed| Text::of(&ed.rope, ed.vim_buf.trailing_empty).len(cursor(ed).line) == 0);
    if empty {
        record_change(vim, c, None, None);
        if let Some((r, value)) = c.reg.map(|r| (r, Reg::new("", RegKind::Char))) {
            store(vim, host, Some(r), value, Store::Delete { small: true });
        }
        insert::begin(vim, host, InsertKind::Before, 1, false, None);
        return;
    }
    run_op(vim, host, c);
}

fn jump_list(vim: &mut VimState, host: &mut dyn Host, older: bool, count: usize) {
    super::exec::jump_list(vim, host, older, count);
}

fn visual_simple(
    vim: &mut VimState,
    host: &mut dyn Host,
    cmd: &Cmd,
    s: Simple,
    count: usize,
) -> bool {
    match s {
        Simple::VisualSwap => {
            let Some(ed) = host.ed() else {
                return true;
            };
            let c = cursor(ed);
            let a = vim.visual_anchor;
            vim.visual_anchor = c;
            set_cursor(ed, a);
            update_curswant(vim, ed);
            sync_visual(vim, ed);
        }
        Simple::VisualSwapCorner => {
            let Some(ed) = host.ed() else {
                return true;
            };
            if vim.visual_kind != VisualKind::Block {
                return visual_simple(vim, host, cmd, Simple::VisualSwap, count);
            }
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let ts = ts(ed);
            let c = cursor(ed);
            let a = vim.visual_anchor;
            let cv = t.vcol(c.line, c.col, ts);
            let av = t.vcol(a.line, a.col, ts);
            vim.visual_anchor = Pos::new(a.line, t.col_for_vcol(a.line, cv, ts));
            set_cursor(ed, Pos::new(c.line, t.col_for_vcol(c.line, av, ts)));
            update_curswant(vim, ed);
            sync_visual(vim, ed);
        }
        Simple::VisualBlockInsert(append) => {
            if read_only(vim, host) {
                exit_visual(vim, host);
                return true;
            }
            let Some(ed) = host.ed() else {
                return true;
            };
            let region = visual_region(vim, ed);
            let kind = vim.visual_kind;
            let eol = vim.curswant_eol;
            exit_visual_keep(vim, host);
            let Some(ed) = host.ed() else {
                return true;
            };
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            match (kind, region.kind) {
                (VisualKind::Block, RKind::Block { left, right }) => {
                    record_change(
                        vim,
                        cmd,
                        Some((
                            kind,
                            region.end.line - region.start.line,
                            if right == usize::MAX {
                                usize::MAX
                            } else {
                                right - left
                            },
                            eol,
                        )),
                        None,
                    );
                    let ts = ts(ed);
                    let line = region.start.line;
                    let (vcol, col) = if append {
                        if right == usize::MAX {
                            (usize::MAX, t.len(line))
                        } else {
                            // Pad a short first line to the block's right edge.
                            let w = t.line_vwidth(line, ts);
                            if w < right {
                                let mut tx = Tx::default();
                                let at = t.line_end(line);
                                tx.edit(ed, at, at, &" ".repeat(right - w));
                                tx.finish(ed);
                            }
                            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                            (right, t.col_for_vcol(line, right, ts))
                        }
                    } else {
                        (left, t.col_for_vcol(line, left, ts))
                    };
                    set_cursor(ed, Pos::new(line, col));
                    let lines = (region.start.line + 1..=region.end.line).collect();
                    insert::begin(
                        vim,
                        host,
                        InsertKind::Before,
                        1,
                        false,
                        Some(insert::BlockInsert {
                            lines,
                            top: region.start.line,
                            left,
                            vcol,
                            append,
                            eol: right == usize::MAX,
                        }),
                    );
                }
                _ => {
                    // Charwise/linewise: I inserts at the start, A after the end.
                    record_change(
                        vim,
                        &Cmd {
                            reg: None,
                            count: None,
                            action: Action::Simple(Simple::Insert(InsertKind::Before)),
                        },
                        None,
                        None,
                    );
                    let p = if append {
                        match region.kind {
                            RKind::Line => Pos::new(region.end.line, t.len(region.end.line)),
                            _ => Pos::new(
                                region.end.line,
                                (region.end.col + 1).min(t.len(region.end.line)),
                            ),
                        }
                    } else {
                        match region.kind {
                            RKind::Line => {
                                Pos::new(region.start.line, t.first_nonblank(region.start.line))
                            }
                            _ => region.start,
                        }
                    };
                    set_cursor(ed, p);
                    insert::begin(vim, host, InsertKind::Before, 1, false, None);
                }
            }
        }
        Simple::VisualSearch(forward) => {
            let Some(ed) = host.ed() else {
                return true;
            };
            let region = visual_region(vim, ed);
            let text = region_text(ed, &region).text;
            exit_visual(vim, host);
            let esc: String = text
                .chars()
                .map(|c| match c {
                    '\\' | '/' | '?' | '.' | '*' | '$' | '^' | '~' | '[' | ']' => format!("\\{c}"),
                    '\n' => "\\n".into(),
                    c => c.to_string(),
                })
                .collect();
            let pat = format!(
                "\\V{}",
                esc.replace("\\.", ".")
                    .replace("\\*", "*")
                    .replace("\\~", "~")
                    .replace("\\[", "[")
                    .replace("\\]", "]")
                    .replace("\\$", "$")
                    .replace("\\^", "^")
            );
            vim.last_search = Some((pat.clone(), forward));
            vim.regs.last_search = pat.clone();
            vim.hl_hidden = false;
            if let Some(p) = super::exec::search_motion(vim, host, &pat, forward, count, false) {
                if let Some(ed) = host.ed() {
                    push_jump(vim, ed);
                    set_cursor(ed, p);
                    update_curswant(vim, ed);
                }
            }
        }
        Simple::Replace(c) => {
            if read_only(vim, host) {
                exit_visual(vim, host);
                return true;
            }
            let Some(ed) = host.ed() else {
                return true;
            };
            let region = visual_region(vim, ed);
            exit_visual_keep(vim, host);
            let Some(ed) = host.ed() else {
                return true;
            };
            let mut tx = Tx::default();
            let ts = ts(ed);
            let ch = if c == '\r' { '\n' } else { c };
            let spans: Vec<(usize, usize)> = {
                let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                match region.kind {
                    RKind::Char { .. } => {
                        let (s, e) = region.char_span(&t);
                        let mut v = Vec::new();
                        for line in region.start.line..=region.end.line {
                            let ls = t.line_start(line).max(s);
                            let le = t.line_end(line).min(e);
                            if le > ls {
                                v.push((ls, le));
                            }
                        }
                        v
                    }
                    RKind::Line => (region.start.line..=region.end.line)
                        .map(|l| (t.line_start(l), t.line_end(l)))
                        .collect(),
                    RKind::Block { left, right } => {
                        // Vim's op_replace for blocks: cells, not characters.
                        for l in (region.start.line..=region.end.line).rev() {
                            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                            let b = super::exec::block_prep(&t, l, left, right, ts, true);
                            if b.before || ch == '\n' {
                                continue;
                            }
                            let end_vcol = if right == usize::MAX {
                                t.line_vwidth(l, ts).saturating_sub(1)
                            } else {
                                right - 1
                            };
                            let mut numc = end_vcol + 1 - left;
                            if b.is_short {
                                numc = t.line_vwidth(l, ts).saturating_sub(left);
                            }
                            let base = t.line_start(l);
                            let text = format!(
                                "{}{}{}",
                                " ".repeat(b.startspaces),
                                ch.to_string().repeat(numc),
                                if b.is_short {
                                    String::new()
                                } else {
                                    " ".repeat(b.endspaces)
                                }
                            );
                            tx.edit(ed, base + b.textcol, base + b.textcol + b.textlen, &text);
                        }
                        Vec::new()
                    }
                }
            };
            for &(s, e) in spans.iter().rev() {
                let n = e - s;
                if ch == '\n' {
                    tx.edit(ed, s, e, "\n");
                } else {
                    tx.edit(ed, s, e, &ch.to_string().repeat(n));
                }
            }
            tx.finish(ed);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let start = match region.kind {
                RKind::Line => Pos::new(region.start.line, 0),
                RKind::Block { left, .. } => Pos::new(
                    region.start.line,
                    t.col_for_vcol(region.start.line, left, ts),
                ),
                _ => region.start,
            };
            set_cursor(ed, t.clamp_normal(start));
        }
        Simple::DeleteToEnd | Simple::YankLine | Simple::ChangeToEnd => {
            let Some(ed) = host.ed() else {
                return true;
            };
            let mut region = visual_region(vim, ed);
            let block = vim.visual_kind == VisualKind::Block;
            if block {
                if let RKind::Block { left, .. } = region.kind {
                    region.kind = RKind::Block {
                        left,
                        right: usize::MAX,
                    };
                }
            } else {
                region.kind = RKind::Line;
            }
            let kind = vim.visual_kind;
            exit_visual_keep(vim, host);
            let op = match s {
                Simple::DeleteToEnd => Op::Delete,
                Simple::YankLine => Op::Yank,
                _ => Op::Change,
            };
            if op != Op::Yank {
                if read_only(vim, host) {
                    return true;
                }
                record_change(
                    vim,
                    cmd,
                    Some((
                        if block { kind } else { VisualKind::Line },
                        region.end.line - region.start.line,
                        usize::MAX,
                        true,
                    )),
                    None,
                );
            }
            apply(vim, host, cmd.reg, op, region, 1, true, true);
        }
        Simple::Join(spaces) => {
            if read_only(vim, host) {
                exit_visual(vim, host);
                return true;
            }
            let Some(ed) = host.ed() else {
                return true;
            };
            let region = visual_region(vim, ed);
            exit_visual_keep(vim, host);
            record_change(
                vim,
                cmd,
                Some((
                    VisualKind::Line,
                    region.end.line - region.start.line,
                    0,
                    false,
                )),
                None,
            );
            let Some(ed) = host.ed() else {
                return true;
            };
            let end = region.end.line.max(region.start.line + 1);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            if region.start.line >= t.last_line() {
                let col = if matches!(region.kind, RKind::Line) {
                    0
                } else {
                    region.start.col
                };
                set_cursor(ed, t.clamp_normal(Pos::new(region.start.line, col)));
                vim.beep();
                return true;
            }
            join(vim, ed, region.start.line, end, spaces);
        }
        Simple::Put { before, .. } => {
            if read_only(vim, host) {
                exit_visual(vim, host);
                return true;
            }
            visual_put(vim, host, cmd, before, count);
        }
        Simple::Increment(up) | Simple::IncrementSeq(up) => {
            if read_only(vim, host) {
                exit_visual(vim, host);
                return true;
            }
            let progressive = matches!(cmd.action, Action::Simple(Simple::IncrementSeq(_)));
            let Some(ed) = host.ed() else {
                return true;
            };
            let region = visual_region(vim, ed);
            exit_visual_keep(vim, host);
            let Some(ed) = host.ed() else {
                return true;
            };
            let ts = ts(ed);
            let mut step = 0i128;
            for line in region.start.line..=region.end.line {
                let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                let (sc, ec) = match region.kind {
                    RKind::Char { .. } => {
                        let s = if line == region.start.line {
                            region.start.col
                        } else {
                            0
                        };
                        let e = if line == region.end.line {
                            region.end.col + 1
                        } else {
                            t.len(line)
                        };
                        (s, e)
                    }
                    RKind::Line => (0, t.len(line)),
                    RKind::Block { left, right } => match block_cols(&t, line, left, right, ts) {
                        Some(v) => v,
                        None => continue,
                    },
                };
                let n = if progressive {
                    step + count as i128
                } else {
                    count as i128
                };
                let delta = if up { n } else { -n };
                if increment_at(vim, ed, line, sc, ec, delta).is_some() {
                    step = n;
                }
            }
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            set_cursor(ed, t.clamp_normal(region.start));
        }
        Simple::Insert(_) | Simple::Substitute | Simple::SubstituteLine => {}
        Simple::CmdLine => {
            let start = host.ed_ref().map(|ed| {
                let r = visual_region(vim, ed);
                if matches!(r.kind, RKind::Line) {
                    Pos::new(r.start.line, 0)
                } else {
                    r.start
                }
            });
            exit_visual_keep(vim, host);
            if let (Some(p), Some(ed)) = (start, host.ed()) {
                set_cursor(ed, p);
            }
            super::cmdline::open(vim, host, ':', None, "'<,'>".into());
        }
        Simple::ToggleCase => {
            visual_op(vim, host, cmd, Op::Tilde);
        }
        Simple::DeleteChar | Simple::DeleteCharBefore => visual_op(vim, host, cmd, Op::Delete),
        Simple::Scroll(_)
        | Simple::Fold(_)
        | Simple::Window(_)
        | Simple::Cancel
        | Simple::Visual(_)
        | Simple::Reselect
        | Simple::Definition
        | Simple::Hover
        | Simple::References
        | Simple::SetMark(_)
        | Simple::Undo
        | Simple::Redo
        | Simple::NextTab
        | Simple::PrevTab => return false,
        _ => return false,
    }
    true
}

fn replace_chars(vim: &mut VimState, host: &mut dyn Host, cmd: &Cmd, c: char, count: usize) {
    let Some(ed) = host.ed() else {
        return;
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let cur = t.clamp_normal(cursor(ed));
    let len = t.len(cur.line);
    if len == 0 || cur.col + count > len {
        return;
    }
    record_change(vim, cmd, None, None);
    let s = t.idx(cur);
    let mut tx = Tx::default();
    if c == '\r' || c == '\n' {
        // r<CR> replaces the characters with one line break.
        let indent: String = if vim.opts.autoindent {
            let chars = t.line_chars(cur.line);
            let mut rest: Vec<char> = chars.clone();
            rest.drain(cur.col..(cur.col + count).min(chars.len()));
            let ws = rest
                .iter()
                .take_while(|&&c| c == ' ' || c == '\t')
                .count()
                .min(cur.col);
            let width = {
                let mut v = 0;
                for &c in &rest[..ws] {
                    v += char_width(c, v, ts(ed));
                }
                v
            };
            if ed.use_tabs {
                format!(
                    "{}{}",
                    "\t".repeat(width / ts(ed)),
                    " ".repeat(width % ts(ed))
                )
            } else {
                " ".repeat(width)
            }
        } else {
            String::new()
        };
        let rest = t.line_chars(cur.line);
        let lead = rest[(cur.col + count).min(rest.len())..]
            .iter()
            .take_while(|&&c| c == ' ' || c == '\t')
            .count();
        tx.edit(ed, s, s + count + lead, &format!("\n{indent}"));
        tx.finish(ed);
        // Vim types <CR><Esc>: Esc moves back over the indent's last char.
        set_cursor(
            ed,
            Pos::new(cur.line + 1, indent.chars().count().saturating_sub(1)),
        );
    } else {
        tx.edit(ed, s, s + count, &c.to_string().repeat(count));
        tx.finish(ed);
        set_cursor(ed, Pos::new(cur.line, cur.col + count - 1));
    }
    update_curswant(vim, ed);
}

/// `J`/`gJ` on lines `first..=last`.
pub(crate) fn join(vim: &mut VimState, ed: &mut Editor, first: usize, last: usize, spaces: bool) {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    if first >= t.last_line() {
        // `3J` on the last line only moves to its start (`J` beeps).
        if last > first + 1 {
            set_cursor(ed, Pos::new(first, 0));
        } else {
            vim.beep();
        }
        return;
    }
    let last = last.min(t.last_line());
    let mut tx = Tx::default();
    let mut cursor_col = 0;
    for _ in first..last {
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        let cur_chars = t.line_chars(first);
        let next_chars = t.line_chars(first + 1);
        let join_at = t.line_end(first);
        let next_start = t.line_start(first + 1);
        let (remove_lead, insert) = if spaces {
            let lead = next_chars
                .iter()
                .take_while(|&&c| c == ' ' || c == '\t')
                .count();
            let rest = &next_chars[lead..];
            // Trailing whitespace of the current line stays; Vim adds one
            // space unless it already ends in white, the next line is empty
            // or starts with ')'.
            let ends_white = cur_chars.last().is_some_and(|&c| c == ' ' || c == '\t');
            let ins = if rest.is_empty() || rest.first() == Some(&')') || ends_white {
                ""
            } else if vim.opts.joinspaces && cur_chars.last().is_some_and(|c| ".!?".contains(*c)) {
                "  "
            } else if cur_chars.is_empty() {
                ""
            } else {
                " "
            };
            (lead, ins)
        } else {
            (0, "")
        };
        let mut ins = insert.to_string();
        if spaces && cur_chars.is_empty() {
            ins.clear();
        }
        // The cursor goes where the last line was joined: on the inserted
        // space, or on the first joined character.
        cursor_col = cur_chars.len();
        tx.edit(ed, join_at, next_start + remove_lead, &ins);
    }
    tx.finish(ed);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    set_cursor(ed, t.clamp_normal(Pos::new(first, cursor_col)));
    update_curswant(vim, ed);
}

/// Put register text. Returns false when the register is empty.
pub(crate) fn put(
    vim: &mut VimState,
    host: &mut dyn Host,
    reg: Option<char>,
    before: bool,
    cursor_after: bool,
    indent: bool,
    count: usize,
) {
    let Some(value) = read_reg(vim, host, reg) else {
        vim.set_error(format!("E353: Nothing in register {}", reg.unwrap_or('"')));
        return;
    };
    if value.text.is_empty() {
        vim.set_error(format!("E353: Nothing in register {}", reg.unwrap_or('"')));
        return;
    }
    let Some(ed) = host.ed() else {
        return;
    };
    put_value(vim, ed, &value, before, cursor_after, indent, count);
}

pub(crate) fn put_value(
    vim: &mut VimState,
    ed: &mut Editor,
    value: &Reg,
    before: bool,
    cursor_after: bool,
    indent: bool,
    count: usize,
) {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let cur = t.clamp_normal(cursor(ed));
    let mut tx = Tx::default();
    let count = count.max(1);
    match value.kind {
        RegKind::Line => {
            let mut text = value.text.clone();
            if !text.ends_with('\n') {
                text.push('\n');
            }
            if indent {
                // ]p: adjust indent to the current line.
                let target: String = t
                    .line_chars(cur.line)
                    .into_iter()
                    .take_while(|&c| c == ' ' || c == '\t')
                    .collect();
                let first_indent: usize = text.lines().next().map_or(0, |l| {
                    l.chars().take_while(|&c| c == ' ' || c == '\t').count()
                });
                text = text
                    .lines()
                    .map(|l| {
                        let lead = l.chars().take_while(|&c| c == ' ' || c == '\t').count();
                        let keep: String = l.chars().skip(first_indent.min(lead)).collect();
                        if l.trim().is_empty() {
                            String::new()
                        } else {
                            format!("{target}{keep}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n";
            }
            let text = text.repeat(count);
            let n_lines = text.matches('\n').count();
            let line = if before { cur.line } else { cur.line + 1 };
            let rope_len = ed.rope.len_chars();
            let ends_eol = rope_len > 0 && matches!(ed.rope.char(rope_len - 1), '\n' | '\r');
            if !before && cur.line == t.last_line() && !ends_eol {
                // Append after a last line without a line break.
                let body = text.strip_suffix('\n').unwrap_or(&text);
                tx.edit(ed, rope_len, rope_len, &format!("\n{body}"));
            } else {
                let at = if before {
                    t.line_start(cur.line)
                } else {
                    t.line_next_start(cur.line)
                };
                tx.edit(ed, at, at, &text);
            }
            tx.finish(ed);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            if cursor_after {
                let l = (line + n_lines).min(t.last_line());
                set_cursor(ed, Pos::new(l, 0));
            } else {
                set_cursor(ed, Pos::new(line, t.first_nonblank(line)));
            }
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            ed.vim_buf.marks.insert('[', Pos::new(line, 0));
            let last = (line + n_lines - 1).min(t.last_line());
            ed.vim_buf.marks.insert(']', Pos::new(last, 0));
        }
        RegKind::Char => {
            let text = value.text.repeat(count);
            let empty = t.len(cur.line) == 0;
            let at = if before || empty {
                t.idx(cur)
            } else {
                t.idx(cur) + 1
            };
            tx.edit(ed, at, at, &text);
            tx.finish(ed);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let n = text.chars().count();
            let end = t.pos(at + n);
            let start = t.pos(at);
            let last = t.pos(at + n.saturating_sub(1));
            let target = if cursor_after {
                end
            } else if text.contains('\n') {
                // Multi-line text: cursor at the start of the new text.
                t.clamp_normal(start)
            } else {
                t.clamp_normal(last)
            };
            set_cursor(ed, target);
            ed.vim_buf.marks.insert('[', start);
            ed.vim_buf.marks.insert(']', last);
        }
        RegKind::Block(width) => {
            let ts = ts(ed);
            let col_v = if before || t.len(cur.line) == 0 {
                t.vcol(cur.line, cur.col, ts)
            } else {
                t.vcol_end(cur.line, cur.col, ts)
            };
            let lines: Vec<String> = value.block_lines().iter().map(|s| s.to_string()).collect();
            for (i, piece) in lines.iter().enumerate() {
                let line = cur.line + i;
                let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                if line > t.last_line() {
                    let end = ed.rope.len_chars();
                    tx.edit(ed, end, end, "\n");
                }
                let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                let w = t.line_vwidth(line, ts);
                let base = t.line_start(line);
                // Vim measures each character as if at column 0 here.
                let pw: usize = piece.chars().map(|c| char_width(c, 0, ts)).sum();
                let mut chunk = String::new();
                for k in 0..count {
                    chunk.push_str(piece);
                    // Pad to the block width between copies and before text.
                    if k + 1 < count || col_v < w {
                        chunk.push_str(&" ".repeat(width.saturating_sub(pw)));
                    }
                }
                if w < col_v {
                    let at = base + t.len(line);
                    tx.edit(
                        ed,
                        at,
                        at,
                        &format!("{}{}", " ".repeat(col_v - w), chunk.trim_end_matches(' ')),
                    );
                } else {
                    // A tab straddling the put column is split into spaces.
                    let chars = t.line_chars(line);
                    let (mut c, mut v) = (0, 0);
                    while c < chars.len() && v < col_v {
                        v += char_width(chars[c], v, ts);
                        c += 1;
                    }
                    if v > col_v {
                        let start_v = v - char_width(chars[c - 1], t.vcol(line, c - 1, ts), ts);
                        let before = col_v - start_v;
                        let after = v - col_v;
                        tx.edit(
                            ed,
                            base + c - 1,
                            base + c,
                            &format!("{}{chunk}{}", " ".repeat(before), " ".repeat(after)),
                        );
                    } else {
                        tx.edit(ed, base + c, base + c, &chunk);
                    }
                }
            }
            tx.finish(ed);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let col = t.col_for_vcol(cur.line, col_v, ts);
            if cursor_after {
                let last = cur.line + lines.len() - 1;
                set_cursor(
                    ed,
                    t.clamp_insert(Pos::new(
                        last,
                        t.col_for_vcol(last, col_v + width * count, ts),
                    )),
                );
            } else {
                set_cursor(ed, Pos::new(cur.line, col));
            }
        }
    }
    update_curswant(vim, ed);
}

fn visual_put(vim: &mut VimState, host: &mut dyn Host, cmd: &Cmd, before: bool, count: usize) {
    let Some(value) = read_reg(vim, host, cmd.reg) else {
        exit_visual(vim, host);
        vim.set_error("E353: Nothing in register");
        return;
    };
    let Some(ed) = host.ed() else {
        return;
    };
    let region = visual_region(vim, ed);
    let kind = vim.visual_kind;
    exit_visual_keep(vim, host);
    record_change(
        vim,
        cmd,
        Some((kind, region.end.line - region.start.line, 0, false)),
        None,
    );
    let Some(ed) = host.ed() else {
        return;
    };
    let replaced = region_text(ed, &region);
    // Replacing the line of an emptied buffer deletes nothing.
    let nothing_replaced = ed.vim_buf.ml_empty;
    // Delete the selection, then put before the cursor position.
    let mut tx = Tx::default();
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let was_line = matches!(region.kind, RKind::Line);
    match region.kind {
        RKind::Char { .. } => {
            let (s, e) = region.char_span(&t);
            tx.edit(ed, s, e, "");
            tx.finish(ed);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let p = t.pos(s);
            ed.cursor_line = p.line;
            ed.cursor_col = p.col;
            if value.kind == RegKind::Line {
                // Linewise text replacing characters goes on its own lines.
                let mut tx = Tx::default();
                let at = t.idx(p);
                tx.edit(ed, at, at, "\n");
                tx.finish(ed);
                let mut v = value.clone();
                if v.text.ends_with('\n') {
                    v.text.pop();
                }
                let line = p.line + 1;
                let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                let at = t.line_start(line);
                let mut tx = Tx::default();
                tx.edit(ed, at, at, &format!("{}\n", v.text.repeat(count)));
                tx.finish(ed);
                set_cursor(
                    ed,
                    Pos::new(
                        line,
                        Text::of(&ed.rope, ed.vim_buf.trailing_empty).first_nonblank(line),
                    ),
                );
            } else {
                let at_eol = t.char_at(p).is_none() && p.col > 0;
                if at_eol {
                    ed.cursor_col -= 1;
                }
                put_value(vim, ed, &value, !at_eol, false, false, count);
            }
        }
        RKind::Line => {
            let (s, e) = {
                let end = t.line_next_start(region.end.line);
                (t.line_start(region.start.line), end)
            };
            let last_line_no_eol =
                e >= ed.rope.len_chars() && !matches!(ed.rope.chars().last(), Some('\n' | '\r'));
            let mut text = value.text.repeat(count);
            if value.kind != RegKind::Line {
                text.push('\n');
            }
            if value.kind == RegKind::Line && value.text.is_empty() {
                text.clear();
            }
            if last_line_no_eol {
                text.pop();
            }
            tx.edit(ed, s, e, &text);
            tx.finish(ed);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let line = region.start.line.min(t.last_line());
            set_cursor(
                ed,
                Pos::new(
                    line,
                    if value.kind == RegKind::Line {
                        t.first_nonblank(line)
                    } else {
                        0
                    },
                ),
            );
        }
        RKind::Block { left, right } => {
            let ts = ts(ed);
            for line in (region.start.line..=region.end.line).rev() {
                let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                if let Some((sc, ec)) = block_cols(&t, line, left, right, ts) {
                    let base = t.line_start(line);
                    tx.edit(ed, base + sc, base + ec, "");
                }
            }
            tx.finish(ed);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let p = Pos::new(
                region.start.line,
                t.col_for_vcol(region.start.line, left, ts),
            );
            ed.cursor_line = p.line;
            ed.cursor_col = p.col;
            put_value(vim, ed, &value, true, false, false, count);
        }
    }
    let _ = was_line;
    // `P` keeps the register; `p` stores the replaced text.
    if !before && !nothing_replaced {
        store(vim, host, None, replaced, Store::Delete { small: false });
    }
}

/// Add `delta` to the first number at/after `col` (before `end`).
/// Returns the column of the number's last character.
pub(crate) fn increment_at(
    vim: &VimState,
    ed: &mut Editor,
    line: usize,
    col: usize,
    end: usize,
    delta: i128,
) -> Option<usize> {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let chars = t.line_chars(line);
    let end = end.min(chars.len());
    let o = &vim.opts;
    // Find the number under or after the cursor (Vim looks back too).
    let mut i = col.min(chars.len());
    let is_hex = |c: char| c.is_ascii_hexdigit();
    // Hex/bin prefix under the cursor.
    let mut start = None;
    let mut s = i;
    while s > 0 && (chars[s - 1].is_ascii_alphanumeric()) {
        s -= 1;
    }
    let word: String = chars[s..chars.len()]
        .iter()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect();
    if o.nrformats_hex
        && (word.starts_with("0x") || word.starts_with("0X"))
        && word.len() > 2
        && word[2..].chars().all(is_hex)
        && i < s + word.len()
    {
        start = Some((s, 16));
    } else if o.nrformats_bin
        && (word.starts_with("0b") || word.starts_with("0B"))
        && word.len() > 2
        && word[2..].chars().all(|c| c == '0' || c == '1')
        && i < s + word.len()
    {
        start = Some((s, 2));
    }
    if start.is_none() {
        if i < chars.len() && chars[i].is_ascii_digit() {
            while i > 0 && chars[i - 1].is_ascii_digit() {
                i -= 1;
            }
        } else {
            while i < end && !chars[i].is_ascii_digit() {
                if o.nrformats_alpha && chars[i].is_ascii_alphabetic() {
                    break;
                }
                i += 1;
            }
            if i >= end {
                return None;
            }
        }
        if o.nrformats_alpha && chars[i].is_ascii_alphabetic() {
            let c = chars[i];
            let base = if c.is_ascii_lowercase() { b'a' } else { b'A' };
            let v = (c as u8 - base) as i128 + delta;
            let v = v.clamp(0, 25) as u8 + base;
            let mut tx = Tx::default();
            let at = t.line_start(line) + i;
            tx.edit(ed, at, at + 1, &(v as char).to_string());
            tx.finish(ed);
            return Some(i);
        }
        // Hex/bin literals found by scanning forward.
        if i + 1 < chars.len()
            && chars[i] == '0'
            && o.nrformats_hex
            && matches!(chars[i + 1], 'x' | 'X')
            && chars.get(i + 2).is_some_and(|c| is_hex(*c))
        {
            start = Some((i, 16));
        } else if i + 1 < chars.len()
            && chars[i] == '0'
            && o.nrformats_bin
            && matches!(chars[i + 1], 'b' | 'B')
            && chars.get(i + 2).is_some_and(|c| *c == '0' || *c == '1')
        {
            start = Some((i, 2));
        } else {
            start = Some((i, 10));
        }
    }
    let (s, radix) = start?;
    let base = t.line_start(line);
    let mut tx = Tx::default();
    let result_end;
    if radix == 10 {
        let mut e = s;
        while e < chars.len() && chars[e].is_ascii_digit() {
            e += 1;
        }
        let neg = s > 0 && chars[s - 1] == '-';
        let digits: String = chars[s..e].iter().collect();
        let octal = o.nrformats_octal
            && digits.len() > 1
            && digits.starts_with('0')
            && digits.chars().all(|c| ('0'..='7').contains(&c));
        let (ns, value) = if octal {
            (s, i128::from_str_radix(&digits, 8).ok()?)
        } else {
            let v: i128 = digits.parse().ok()?;
            if neg { (s - 1, -v) } else { (s, v) }
        };
        let new = value + delta;
        let text = if octal {
            format!("0{:o}", new.max(0))
        } else {
            // Keep leading zeros' width for zero-padded numbers.
            let width = if digits.starts_with('0') && digits.len() > 1 {
                digits.len()
            } else {
                0
            };
            if new < 0 {
                format!("-{:0width$}", -new, width = width)
            } else {
                format!("{:0width$}", new, width = width)
            }
        };
        tx.edit(ed, base + ns, base + e, &text);
        result_end = ns + text.chars().count() - 1;
    } else {
        let mut e = s + 2;
        let ok = |c: char| {
            if radix == 16 {
                c.is_ascii_hexdigit()
            } else {
                c == '0' || c == '1'
            }
        };
        while e < chars.len() && ok(chars[e]) {
            e += 1;
        }
        let digits: String = chars[s + 2..e].iter().collect();
        let v = u64::from_str_radix(&digits, radix).ok()?;
        let new = v.wrapping_add(delta as i64 as u64);
        let upper = digits.chars().any(|c| c.is_ascii_uppercase());
        let body = if radix == 16 {
            let s = format!("{:0width$x}", new, width = digits.len());
            if upper { s.to_uppercase() } else { s }
        } else {
            format!("{:0width$b}", new, width = digits.len())
        };
        let prefix: String = chars[s..s + 2].iter().collect();
        let text = format!("{prefix}{body}");
        tx.edit(ed, base + s, base + e, &text);
        result_end = s + text.chars().count() - 1;
    }
    tx.finish(ed);
    Some(result_end)
}

fn scroll(vim: &mut VimState, host: &mut dyn Host, sc: Scroll, count: usize, has_count: bool) {
    let visual = vim.is_visual();
    let Some(ed) = host.ed() else {
        return;
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let height = ed.viewport_height.max(1);
    let last = t.last_line();
    let cur = cursor(ed);
    let move_view = |ed: &mut Editor, top: usize| {
        let top = top.min(last);
        ed.scroll_offset = top;
        ed.wrap_scroll_offset = top;
    };
    match sc {
        Scroll::HalfDown | Scroll::HalfUp => {
            if has_count {
                vim.scroll_lines = count;
            }
            let n = if vim.scroll_lines > 0 {
                vim.scroll_lines
            } else {
                height / 2
            }
            .max(1);
            if sc == Scroll::HalfDown {
                if cur.line >= last {
                    return;
                }
                let top = (ed.scroll_offset + n).min(
                    last.saturating_sub(height.saturating_sub(1))
                        .max(ed.scroll_offset),
                );
                move_view(ed, top);
                let line = (cur.line + n).min(last);
                set_cursor_keep_view(vim, ed, line, visual);
            } else {
                if cur.line == 0 {
                    return;
                }
                let top = ed.scroll_offset.saturating_sub(n);
                move_view(ed, top);
                let line = cur.line.saturating_sub(n);
                set_cursor_keep_view(vim, ed, line, visual);
            }
        }
        Scroll::PageDown => {
            if cur.line >= last {
                return;
            }
            let n = (height.saturating_sub(2)).max(1) * count;
            let top = (ed.scroll_offset + n).min(last);
            move_view(ed, top);
            let line = top.max(cur.line.min(last)).min(last);
            let line = if line < top { top } else { line.max(top) };
            let line = line.min(last);
            set_cursor_keep_view(vim, ed, line.max(top.min(last)), visual);
        }
        Scroll::PageUp => {
            if ed.scroll_offset == 0 && cur.line == 0 {
                return;
            }
            let n = (height.saturating_sub(2)).max(1) * count;
            let top = ed.scroll_offset.saturating_sub(n);
            move_view(ed, top);
            let bottom = (top + height).saturating_sub(1).min(last);
            let line = cur.line.min(bottom);
            set_cursor_keep_view(vim, ed, line, visual);
        }
        Scroll::LineDown | Scroll::LineUp => {
            let top = if sc == Scroll::LineDown {
                (ed.scroll_offset + count).min(last)
            } else {
                ed.scroll_offset.saturating_sub(count)
            };
            move_view(ed, top);
            let so = ed.cursor_surrounding_lines.min(height / 2);
            let min = if top == 0 { 0 } else { top + so };
            let max = (top + height).saturating_sub(1 + so).min(last);
            let line = cur.line.clamp(min.min(max), max.max(min.min(max)));
            if line != cur.line {
                set_cursor_keep_view(vim, ed, line, visual);
            }
        }
        Scroll::Top(first) | Scroll::Center(first) | Scroll::Bottom(first) => {
            let line = if has_count {
                (count - 1).min(last)
            } else {
                cur.line
            };
            let col = if first {
                t.first_nonblank(line)
            } else if line == cur.line {
                cur.col
            } else {
                cur.col.min(t.max_normal_col(line))
            };
            ed.cursor_line = line;
            ed.cursor_col = col;
            match sc {
                Scroll::Top(_) => move_view(ed, line),
                Scroll::Center(_) => move_view(ed, line.saturating_sub(height / 2)),
                _ => move_view(ed, (line + 1).saturating_sub(height)),
            }
        }
        Scroll::Left(_)
        | Scroll::Right(_)
        | Scroll::HalfLeft
        | Scroll::HalfRight
        | Scroll::CursorLeft
        | Scroll::CursorRight => {
            if ed.word_wrap {
                return;
            }
            let w = ed.viewport_width.max(1);
            let h = ed.horizontal_scroll;
            ed.horizontal_scroll = match sc {
                Scroll::Left(_) => h.saturating_sub(count),
                Scroll::Right(_) => h + count,
                Scroll::HalfLeft => h.saturating_sub(w / 2),
                Scroll::HalfRight => h + w / 2,
                Scroll::CursorLeft => t.vcol(cur.line, cur.col, ts(ed)),
                _ => (t.vcol(cur.line, cur.col, ts(ed)) + 1).saturating_sub(w),
            };
            // Keep the cursor on screen.
            let ts = ts(ed);
            let v = t.vcol(cur.line, cur.col, ts);
            let h = ed.horizontal_scroll;
            if v < h {
                ed.cursor_col = t
                    .col_for_vcol(cur.line, h, ts)
                    .min(t.max_normal_col(cur.line));
            }
            if v >= h + w {
                ed.cursor_col = t
                    .col_for_vcol(cur.line, h + w - 1, ts)
                    .min(t.max_normal_col(cur.line));
            }
        }
    }
    if visual {
        sync_visual(vim, ed);
    }
}

fn set_cursor_keep_view(vim: &mut VimState, ed: &mut Editor, line: usize, visual: bool) {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let line = line.min(t.last_line());
    let col = if vim.opts.startofline {
        t.first_nonblank(line)
    } else {
        let want = vim.curswant.unwrap_or(0);
        if want == usize::MAX {
            t.max_normal_col(line)
        } else {
            t.col_for_vcol(line, want, ts(ed)).min(if visual {
                t.len(line)
            } else {
                t.max_normal_col(line)
            })
        }
    };
    ed.cursor_line = line;
    ed.cursor_col = col;
    // Keep the view the scroll command chose, only nudge it if needed.
    let top = ed.scroll_offset;
    let h = ed.viewport_height.max(1);
    if line < top || line >= top + h {
        ed.ensure_visible();
    }
    if vim.opts.startofline {
        update_curswant(vim, ed);
    }
}

fn fold(vim: &mut VimState, host: &mut dyn Host, f: FoldCmd, count: usize) {
    let Some(ed) = host.ed() else {
        return;
    };
    let line = ed.cursor_line;
    let containing = |ed: &Editor, line: usize| {
        ed.folded_range_containing(line)
            .map(|(s, _)| s)
            .or_else(|| ed.folded_ranges.get(&line).map(|_| line))
    };
    match f {
        FoldCmd::Open | FoldCmd::OpenAll | FoldCmd::OpenCursor => {
            for _ in 0..count.max(1) {
                match containing(ed, line) {
                    Some(s) => {
                        ed.toggle_fold(s);
                    }
                    None => break,
                }
                if f == FoldCmd::Open {
                    break;
                }
            }
            if f != FoldCmd::Open {
                while let Some(s) = containing(ed, line) {
                    ed.toggle_fold(s);
                }
            }
        }
        FoldCmd::Close | FoldCmd::CloseAll => {
            if containing(ed, line).is_some() && f == FoldCmd::Close {
                return;
            }
            // Close the innermost foldable block containing the cursor.
            let mut start = line;
            loop {
                if let Some(end) = ed.find_fold_end(start).filter(|&e| e > line) {
                    ed.collapse_range(start, end);
                    break;
                }
                if start == 0 {
                    vim.set_error("E490: No fold found");
                    break;
                }
                start -= 1;
            }
        }
        FoldCmd::Toggle | FoldCmd::ToggleAll => {
            if let Some(s) = containing(ed, line) {
                ed.toggle_fold(s);
            } else {
                return fold(vim, host, FoldCmd::Close, count);
            }
        }
        FoldCmd::OpenEverything => ed.unfold_all(),
        FoldCmd::CloseEverything => ed.fold_all(),
        FoldCmd::Delete | FoldCmd::DeleteAll => {
            if let Some(s) = containing(ed, line) {
                ed.toggle_fold(s);
            }
            if f == FoldCmd::DeleteAll {
                ed.unfold_all();
            }
        }
        FoldCmd::Recompute | FoldCmd::ToggleEnable => {}
        FoldCmd::Next | FoldCmd::Prev | FoldCmd::StartNext | FoldCmd::StartPrev => {
            let forward = matches!(f, FoldCmd::Next | FoldCmd::StartNext);
            let starts: Vec<usize> = ed.folded_ranges.keys().copied().collect();
            let target = if forward {
                starts.iter().copied().find(|&s| s > line)
            } else {
                starts.iter().rev().copied().find(|&s| s < line)
            };
            if let Some(l) = target {
                let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                set_cursor(ed, Pos::new(l, t.first_nonblank(l)));
            }
        }
    }
    clamp_normal(ed);
}

fn file_info(vim: &mut VimState, host: &mut dyn Host) {
    let name = host.file_name().unwrap_or_else(|| "[No Name]".into());
    let Some(ed) = host.ed_ref() else {
        return;
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let lines = t.lines();
    let pct = (ed.cursor_line + 1) * 100 / lines.max(1);
    let modified = if ed.modified { " [Modified]" } else { "" };
    let ro = if ed.read_only { " [readonly]" } else { "" };
    vim.set_message(format!(
        "\"{name}\"{modified}{ro} {lines} line{} --{pct}%--",
        if lines == 1 { "" } else { "s" }
    ));
}

fn word_count(vim: &mut VimState, host: &mut dyn Host) {
    let Some(ed) = host.ed_ref() else {
        return;
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let text = ed.rope.to_string();
    let words = text.split_whitespace().count();
    let before: usize = ed
        .rope
        .slice(..t.idx(cursor(ed)))
        .to_string()
        .split_whitespace()
        .count();
    vim.set_message(format!(
        "Col {} of {}; Line {} of {}; Word {} of {}; Char {} of {}",
        ed.cursor_col + 1,
        t.len(ed.cursor_line),
        ed.cursor_line + 1,
        t.lines(),
        before.max(1).min(words),
        words,
        t.idx(cursor(ed)) + 1,
        ed.rope.len_chars()
    ));
}

// ---------------------------------------------------------------------------
// Repeat and macros

pub(crate) fn repeat(vim: &mut VimState, host: &mut dyn Host, count: Option<usize>) {
    let Some(change) = vim.last_change.clone() else {
        return;
    };
    let mut cmd = change.cmd.clone();
    if let Some(c) = count {
        cmd.count = Some(c);
        if let Action::Op { count2, .. } = &mut cmd.action {
            *count2 = None;
        }
        if let Some(l) = &mut vim.last_change {
            l.cmd.count = Some(c);
            if let Action::Op { count2, .. } = &mut l.cmd.action {
                *count2 = None;
            }
        }
    }
    // Numbered registers advance on repeat ("1p... -> "2p).
    if let Some(r) = cmd
        .reg
        .filter(|r| r.is_ascii_digit() && *r != '0' && *r != '9')
    {
        cmd.reg = Some((r as u8 + 1) as char);
        if let Some(l) = &mut vim.last_change {
            l.cmd.reg = cmd.reg;
        }
    }
    vim.replaying += 1;
    let insert_part = change.insert.clone();
    if let Some(v) = change.visual {
        let op = match &cmd.action {
            Action::VisualOp(op) => Some(*op),
            Action::Simple(Simple::DeleteToEnd) => Some(Op::Delete),
            Action::Simple(Simple::ChangeToEnd) => Some(Op::Change),
            _ => None,
        };
        if let Some(ed) = host.ed() {
            let mut region = replay_visual(vim, ed, v);
            match (&cmd.action, op) {
                (Action::Simple(Simple::DeleteToEnd | Simple::ChangeToEnd), _)
                    if v.0 == VisualKind::Line =>
                {
                    region.kind = RKind::Line
                }
                _ => {}
            }
            match op {
                Some(op) => apply(vim, host, cmd.reg, op, region, cmd.count1(), true, true),
                None => {
                    // Visual J, p, r, I/A: rebuild the selection and run it.
                    let (s, e) = (region.start, region.end);
                    vim.visual_anchor = s;
                    vim.visual_kind = v.0;
                    vim.mode = Mode::Visual(v.0);
                    vim.curswant_eol = v.3;
                    if let Some(ed) = host.ed() {
                        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                        let end = match region.kind {
                            RKind::Block { right, .. } if right != usize::MAX => {
                                Pos::new(e.line, t.col_for_vcol(e.line, right - 1, ts(ed)))
                            }
                            RKind::Block { .. } => Pos::new(e.line, t.max_normal_col(e.line)),
                            _ => e,
                        };
                        ed.cursor_line = end.line;
                        ed.cursor_col = end.col;
                        sync_visual(vim, ed);
                    }
                    super::exec::run(vim, host, cmd.clone());
                }
            }
        }
    } else if let Some((pat, fwd)) = change.search.clone() {
        if let Action::Op {
            op, force, target, ..
        } = cmd.action.clone()
        {
            vim.last_search = Some((pat.clone(), fwd));
            let offset = vim.last_offset;
            if let Some((p, kind)) = super::cmdline::search_target(vim, host, &pat, fwd, 1, offset)
            {
                op_target(
                    vim,
                    host,
                    &cmd,
                    op,
                    force,
                    target,
                    cmd.count1(),
                    cmd.has_count(),
                    Some((p, kind, pat, fwd)),
                );
            }
        }
    } else {
        super::exec::run(vim, host, cmd.clone());
    }
    if vim.is_insert() {
        match insert_part {
            Some(ins) => insert::replay(vim, host, &ins),
            None => insert::finish(vim, host, true),
        }
    }
    vim.replaying -= 1;
    if let Some(l) = &mut vim.last_change {
        if l.insert.is_none() {
            l.insert = change.insert;
        }
    }
}

/// Encode keys as register text (control characters for special keys).
pub(crate) fn keys_to_text(keys: &[super::Key]) -> String {
    use super::keys::K;
    let mut s = String::new();
    for k in keys {
        match k.k {
            K::Char(c)
                if k.ctrl && !k.alt && (c.is_ascii_alphabetic() || "[\\]^_@".contains(c)) =>
            {
                s.push(((c.to_ascii_uppercase() as u8) & 0x1f) as char);
            }
            K::Char(c) if !k.ctrl && !k.alt => s.push(c),
            K::Esc if !k.ctrl && !k.alt && !k.shift => s.push('\x1b'),
            K::Enter if !k.ctrl && !k.alt && !k.shift => s.push('\r'),
            K::Tab if !k.ctrl && !k.alt && !k.shift => s.push('\t'),
            K::Backspace if !k.ctrl && !k.alt && !k.shift => s.push('\x08'),
            _ => {
                // Other keys use private-use code points.
                let notation = super::keys::format_key(*k);
                s.push('\u{E000}');
                s.push_str(&notation);
                s.push('\u{E001}');
            }
        }
    }
    s
}

pub(crate) fn text_to_keys(text: &str) -> Vec<super::Key> {
    use super::keys::{K, Key};
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{E000}' => {
                let mut notation = String::new();
                for n in chars.by_ref() {
                    if n == '\u{E001}' {
                        break;
                    }
                    notation.push(n);
                }
                out.extend(super::keys::parse(&notation, &[]));
            }
            '\x1b' => out.push(Key::ESC),
            '\r' | '\n' => out.push(Key::ENTER),
            '\t' => out.push(Key::plain(K::Tab)),
            '\x08' => out.push(Key::plain(K::Backspace)),
            c if (c as u32) < 0x20 => out.push(Key::ctrl(((c as u8) | 0x60) as char)),
            c => out.push(Key::ch(c)),
        }
    }
    out
}

fn play(vim: &mut VimState, host: &mut dyn Host, r: char, count: usize) {
    let r = match r {
        '@' => match vim.last_macro {
            Some(r) => r,
            None => {
                vim.set_error("E748: No previously used register");
                return;
            }
        },
        r => r,
    };
    if r == ':' {
        let Some(last) = vim.last_ex.clone() else {
            vim.set_error("E30: No previous command line");
            return;
        };
        vim.last_macro = Some(':');
        for _ in 0..count {
            let _ = super::ex::run(vim, host, &last);
        }
        return;
    }
    let Some(reg) = read_reg(vim, host, Some(r)) else {
        return;
    };
    vim.last_macro = Some(r);
    let mut keys = Vec::new();
    for _ in 0..count.max(1) {
        keys.extend(text_to_keys(&reg.text));
        if reg.kind == RegKind::Line && !reg.text.ends_with('\n') {
            keys.push(super::Key::ENTER);
        }
    }
    vim.queue_keys(&keys, true, true);
    let _ = host;
}
