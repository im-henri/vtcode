//! Text objects, ported from Vim's `textobject.c`.

use super::motion::{Walker, find_sent, start_ps};
use super::text::{Keyword, Pos, Step, Text};

/// The area selected by a text object. In Visual mode `start` becomes the
/// new Visual start and `end` the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Area {
    pub(crate) start: Pos,
    pub(crate) end: Pos,
    pub(crate) inclusive: bool,
    pub(crate) linewise: bool,
    /// The object failed part way; Vim leaves the cursor at `end`.
    pub(crate) failed: bool,
}

/// Visual state passed to text objects: (visual start, cursor).
pub(crate) type Vis = Option<(Pos, Pos)>;

fn is_white(c: char) -> bool {
    c == ' ' || c == '\t'
}

struct Cur<'a, 'k> {
    t: Text<'a>,
    p: Pos,
    kw: &'k Keyword,
    big: bool,
}

impl<'a, 'k> Cur<'a, 'k> {
    fn cls(&self) -> u32 {
        self.kw.class(self.t.gchar(self.p), self.big)
    }
    fn walker(&self) -> Walker<'a, 'k> {
        Walker::new(self.t, self.p, self.kw, self.big)
    }
    fn with<R>(&mut self, f: impl FnOnce(&mut Walker<'a, 'k>) -> R) -> R {
        let mut w = self.walker();
        let r = f(&mut w);
        self.p = w.pos;
        r
    }
    fn back_in_line(&mut self) {
        let sclass = self.cls();
        loop {
            if self.p.col == 0 {
                break;
            }
            self.t.dec(&mut self.p);
            if self.cls() != sclass {
                self.t.inc(&mut self.p);
                break;
            }
        }
    }
    fn oneleft(&mut self) -> bool {
        if self.p.col == 0 {
            return false;
        }
        self.p.col -= 1;
        true
    }
}

/// `iw`, `aw`, `iW`, `aW`.
pub(crate) fn word(
    t: Text<'_>,
    cursor: Pos,
    vis: Vis,
    count: usize,
    include: bool,
    big: bool,
    kw: &Keyword,
) -> Option<Area> {
    let mut c = Cur {
        t,
        p: cursor,
        kw,
        big,
    };
    let mut start_pos = cursor;
    let mut vstart = vis.map(|v| v.0);
    let mut inclusive = true;
    let mut include_white = false;
    let mut count = count.max(1);
    if vis.is_none() || vis.is_some_and(|(v, cur)| v == cur) {
        c.back_in_line();
        start_pos = c.p;
        if (c.cls() == 0) == include {
            if !c.with(|w| w.end_word(1, true, true)) {
                return None;
            }
        } else {
            c.with(|w| w.fwd_word(1, true));
            if c.p.col == 0 {
                t.decl(&mut c.p);
            } else {
                c.oneleft();
            }
            if include {
                include_white = true;
            }
        }
        if vis.is_some() {
            vstart = Some(start_pos);
        }
        count -= 1;
    }
    while count > 0 {
        inclusive = true;
        let partial = Some(Area {
            start: vstart.unwrap_or(start_pos),
            end: c.p,
            inclusive: true,
            linewise: false,
            failed: true,
        });
        let fail = |partial: Option<Area>| partial;
        if vstart.is_some_and(|v| c.p < v) {
            if t.decl(&mut c.p) == Step::Fail {
                return fail(partial);
            }
            if include != (c.cls() != 0) {
                if !c.with(|w| w.bck_word(1, true)) {
                    return None;
                }
            } else {
                if !c.with(|w| w.bckend_word(1, true)) {
                    return None;
                }
                t.incl(&mut c.p);
            }
        } else {
            if t.incl(&mut c.p) == Step::Fail {
                return fail(partial);
            }
            if include != (c.cls() == 0) {
                if !c.with(|w| w.fwd_word(1, true)) && count > 1 {
                    return fail(partial);
                }
                if !c.oneleft() {
                    inclusive = false;
                }
            } else if !c.with(|w| w.end_word(1, true, true)) {
                return None;
            }
        }
        count -= 1;
    }
    if include_white && (c.cls() != 0 || (c.p.col == 0 && !inclusive)) {
        let end = c.p;
        c.p = start_pos;
        if c.oneleft() {
            c.back_in_line();
            if c.cls() == 0 && c.p.col > 0 {
                if vstart.is_some() {
                    vstart = Some(c.p);
                } else {
                    start_pos = c.p;
                }
            }
        }
        c.p = end;
    }
    Some(Area {
        start: vstart.unwrap_or(start_pos),
        end: c.p,
        inclusive,
        linewise: false,
        failed: false,
    })
}

fn find_first_blank(t: &Text<'_>, p: &mut Pos) {
    while t.decl(p) != Step::Fail {
        if !is_white(t.gchar(*p)) {
            t.incl(p);
            break;
        }
    }
}

fn findsent_forward(t: &Text<'_>, p: &mut Pos, count: usize, mut at_start: bool) {
    for i in (0..count).rev() {
        *p = find_sent(t, *p, true, 1).unwrap_or(*p);
        if at_start {
            find_first_blank(t, p);
        }
        if i == 0 || at_start {
            t.decl(p);
        }
        at_start = !at_start;
    }
}

/// `is`, `as` (operator-pending and single-character Visual areas).
/// Vim's `current_sent()` extension of an existing Visual area.
fn sentence_extend(
    t: &Text<'_>,
    visual: Pos,
    mut pos: Pos,
    cursor: Pos,
    start_pos: Pos,
    count: usize,
    include: bool,
) -> Area {
    let mut cur = cursor;
    let mut count = count;
    if start_pos < visual {
        // The cursor is at the start of the Visual area.
        let mut at_start_sent = true;
        if pos != cur {
            at_start_sent = false;
            while pos < cur {
                if !is_white(t.gchar(pos)) {
                    at_start_sent = true;
                    break;
                }
                if t.incl(&mut pos) == Step::Fail {
                    break;
                }
            }
            if at_start_sent {
                cur = find_sent(t, cur, false, 1).unwrap_or(cur);
            } else {
                cur = start_pos;
            }
        }
        if include {
            count *= 2;
        }
        for _ in 0..count {
            if at_start_sent {
                find_first_blank(t, &mut cur);
            }
            let c = t.gchar(cur);
            if !at_start_sent || (!include && !is_white(c)) {
                cur = find_sent(t, cur, false, 1).unwrap_or(cur);
            }
            at_start_sent = !at_start_sent;
        }
    } else {
        // The cursor is at the end of the Visual area.
        t.incl(&mut pos);
        let mut at_start_sent = true;
        if pos != cur {
            at_start_sent = false;
            while pos < cur {
                if !is_white(t.gchar(pos)) {
                    at_start_sent = true;
                    break;
                }
                if t.incl(&mut pos) == Step::Fail {
                    break;
                }
            }
            if at_start_sent {
                cur = find_sent(t, cur, false, 1).unwrap_or(cur);
            } else {
                cur = start_pos;
            }
        }
        if include {
            count *= 2;
        }
        findsent_forward(t, &mut cur, count, at_start_sent);
    }
    Area {
        start: visual,
        end: cur,
        inclusive: true,
        linewise: false,
        failed: false,
    }
}

pub(crate) fn sentence(
    t: Text<'_>,
    cursor: Pos,
    vis: Vis,
    count: usize,
    include: bool,
) -> Option<Area> {
    let visual = vis.is_some();
    let mut start_pos = cursor;
    let mut pos = start_pos;
    let mut cur = find_sent(&t, cursor, true, 1).unwrap_or(cursor);
    if let Some((v, c)) = vis
        && v != c
    {
        return Some(sentence_extend(&t, v, pos, cur, start_pos, count, include));
    }
    while is_white(t.gchar(pos)) {
        if t.incl(&mut pos) == Step::Fail {
            break;
        }
    }
    let start_blank;
    if pos == cur {
        start_blank = true;
        find_first_blank(&t, &mut start_pos);
    } else {
        start_blank = false;
        cur = find_sent(&t, cur, false, 1).unwrap_or(cur);
        start_pos = cur;
    }
    let ncount = if include {
        count * 2
    } else if start_blank {
        count - 1
    } else {
        count
    };
    if ncount > 0 {
        findsent_forward(&t, &mut cur, ncount, true);
    } else {
        t.decl(&mut cur);
    }
    if include {
        if start_blank {
            find_first_blank(&t, &mut cur);
            if is_white(t.gchar(cur)) {
                t.decl(&mut cur);
            }
        } else if !is_white(t.gchar(cur)) {
            find_first_blank(&t, &mut start_pos);
        }
    }
    if let Some((v, _)) = vis {
        // Avoid getting stuck with "is" on a single space before a sentence.
        if start_pos == cur {
            return Some(sentence_extend(&t, v, pos, cur, start_pos, count, include));
        }
        let _ = visual;
        return Some(Area {
            start: start_pos,
            end: cur,
            inclusive: true,
            linewise: false,
            failed: false,
        });
    }
    let mut end = cur;
    let inclusive = t.incl(&mut end) == Step::Fail;
    if inclusive {
        end = cur;
    }
    Some(Area {
        start: start_pos,
        end,
        inclusive,
        linewise: false,
        failed: false,
    })
}

fn linewhite(t: &Text<'_>, line: usize) -> bool {
    t.line_chars(line).iter().all(|&c| is_white(c))
}

/// `ip`, `ap`.
pub(crate) fn paragraph(
    t: Text<'_>,
    cursor: Pos,
    vis: Vis,
    count: usize,
    include: bool,
) -> Option<Area> {
    let last = t.last_line();
    let mut start_lnum = cursor.line;
    if let Some((v, _)) = vis
        && v.line != cursor.line
    {
        // Extend a multi-line Visual area.
        let forward = start_lnum >= v.line;
        let edge = if forward { last } else { 0 };
        for _ in 0..count.max(1) {
            if start_lnum == edge {
                return None;
            }
            let mut prev_white: Option<bool> = None;
            for _ in 0..2 {
                start_lnum = if forward {
                    start_lnum + 1
                } else {
                    start_lnum - 1
                };
                let start_white = linewhite(&t, start_lnum);
                if prev_white == Some(start_white) {
                    start_lnum = if forward {
                        start_lnum - 1
                    } else {
                        start_lnum + 1
                    };
                    break;
                }
                loop {
                    if start_lnum == edge {
                        break;
                    }
                    let next = if forward {
                        start_lnum + 1
                    } else {
                        start_lnum - 1
                    };
                    if start_white != linewhite(&t, next)
                        || (!start_white
                            && start_ps(
                                &t,
                                if forward { start_lnum + 1 } else { start_lnum },
                                '\0',
                                false,
                            ))
                    {
                        break;
                    }
                    start_lnum = next;
                }
                if !include || start_lnum == edge {
                    break;
                }
                prev_white = Some(start_white);
            }
        }
        return Some(Area {
            start: v,
            end: Pos::new(start_lnum, 0),
            inclusive: true,
            linewise: true,
            failed: false,
        });
    }
    let white_in_front = linewhite(&t, start_lnum);
    while start_lnum > 0 {
        if white_in_front {
            if !linewhite(&t, start_lnum - 1) {
                break;
            }
        } else if linewhite(&t, start_lnum - 1) || start_ps(&t, start_lnum, '\0', false) {
            break;
        }
        start_lnum -= 1;
    }
    // end_lnum: the last white line from start (or start - 1 if none).
    let mut e = start_lnum;
    while e <= last && linewhite(&t, e) {
        e += 1;
    }
    let mut end_lnum = e as isize - 1;
    let mut i = count.max(1) as isize;
    if !include && white_in_front {
        i -= 1;
    }
    while i > 0 {
        i -= 1;
        if end_lnum == last as isize {
            return None;
        }
        let do_white = !include && linewhite(&t, (end_lnum + 1) as usize);
        if include || !do_white {
            end_lnum += 1;
            while (end_lnum as usize) < last
                && !linewhite(&t, end_lnum as usize + 1)
                && !start_ps(&t, end_lnum as usize + 1, '\0', false)
            {
                end_lnum += 1;
            }
        }
        if i == 0 && white_in_front && include {
            break;
        }
        if include || do_white {
            while (end_lnum as usize) < last && linewhite(&t, end_lnum as usize + 1) {
                end_lnum += 1;
            }
        }
    }
    let end_lnum = end_lnum.max(start_lnum as isize) as usize;
    if !white_in_front && !linewhite(&t, end_lnum) && include {
        while start_lnum > 0 && linewhite(&t, start_lnum - 1) {
            start_lnum -= 1;
        }
    }
    Some(Area {
        start: Pos::new(start_lnum, 0),
        end: Pos::new(end_lnum, 0),
        inclusive: true,
        linewise: true,
        failed: false,
    })
}

/// Vim's `findmatchlimit(NULL, initc, 0, 0)` with `cpo=%`: the unmatched
/// `what` before the cursor (for an opening bracket) or after it (closing).
fn find_unmatched(t: &Text<'_>, from: Pos, what: char) -> Option<Pos> {
    let (open, close, backward) = match what {
        '(' => ('(', ')', true),
        '[' => ('[', ']', true),
        '{' => ('{', '}', true),
        '<' => ('<', '>', true),
        ')' => ('(', ')', false),
        ']' => ('[', ']', false),
        '}' => ('{', '}', false),
        '>' => ('<', '>', false),
        _ => return None,
    };
    let mut p = from;
    let mut depth = 0usize;
    loop {
        let r = if backward {
            t.dec(&mut p)
        } else {
            t.inc(&mut p)
        };
        if r == Step::Fail {
            return None;
        }
        let Some(c) = t.char_at(p) else {
            continue;
        };
        let (same, other) = if backward {
            (close, open)
        } else {
            (open, close)
        };
        if c == same {
            depth += 1;
        } else if c == other {
            if depth == 0 {
                return Some(p);
            }
            depth -= 1;
        }
    }
}

fn inindent1(t: &Text<'_>, p: Pos) -> bool {
    let chars = t.line_chars(p.line);
    let ws = chars.iter().take_while(|&&c| is_white(c)).count();
    ws >= p.col + 1
}

/// `i(`, `a(`, `i{`, `a[`, `i<` and friends.
pub(crate) fn block(
    t: Text<'_>,
    cursor: Pos,
    vis: Vis,
    count: usize,
    include: bool,
    what: char,
    other: char,
) -> Option<Area> {
    let old_pos = cursor;
    let mut cur = cursor;
    let mut old_start = cursor;
    let mut old_end = cursor;
    match vis {
        Some((v, c)) if v != c => {
            if v < c {
                old_start = v;
                cur = v;
            } else {
                old_end = v;
            }
        }
        _ => {
            if what == '{' {
                while inindent1(&t, cur) {
                    if t.inc(&mut cur) != Step::Same {
                        break;
                    }
                }
            }
            if t.char_at(cur) == Some(what) {
                cur.col += 1;
            }
        }
    }
    let mut start_pos = None;
    if find_unmatched(&t, cur, what).is_some() {
        for _ in 0..count.max(1) {
            match find_unmatched(&t, cur, what) {
                Some(p) => {
                    cur = p;
                    start_pos = Some(p);
                }
                None => return None,
            }
        }
    } else {
        // Each count moves to the next opening bracket after the cursor.
        for _ in 0..count.max(1) {
            let mut p = cur;
            let found = loop {
                if t.inc(&mut p) == Step::Fail {
                    break None;
                }
                if t.char_at(p) == Some(what) {
                    break Some(p);
                }
            };
            match found {
                Some(p) => {
                    cur = p;
                    start_pos = Some(p);
                }
                None => break,
            }
        }
    }
    let mut start_pos = start_pos?;
    let mut end = find_unmatched(&t, start_pos, other)?;
    let _ = old_pos;
    let mut sol = false;
    // Exclude the brackets; a Visual area that would not grow moves on to
    // the enclosing block and tries again.
    if !include {
        loop {
            t.incl(&mut start_pos);
            sol = end.col == 0;
            t.decl(&mut end);
            while inindent1(&t, end) {
                sol = true;
                if t.decl(&mut end) != Step::Same {
                    break;
                }
            }
            if !(vis.is_some_and(|(v, c)| v != c)
                && start_pos >= old_start
                && end <= old_end
                && start_pos != end)
            {
                break;
            }
            let mut p = old_start;
            t.decl(&mut p);
            let s = find_unmatched(&t, p, what)?;
            start_pos = s;
            end = find_unmatched(&t, s, other)?;
        }
    }
    if vis.is_some() {
        let mut end = end;
        if sol && t.char_at(end).is_some() {
            t.inc(&mut end);
        }
        return Some(Area {
            start: start_pos,
            end,
            inclusive: true,
            linewise: false,
            failed: false,
        });
    }
    let mut inclusive = false;
    if sol {
        t.incl(&mut end);
    } else if start_pos <= end {
        inclusive = true;
    } else {
        end = start_pos;
    }
    Some(Area {
        start: start_pos,
        end,
        inclusive,
        linewise: false,
        failed: false,
    })
}

fn find_next_quote(line: &[char], mut col: usize, q: char, escape: bool) -> Option<usize> {
    loop {
        let c = *line.get(col)?;
        if escape && c == '\\' {
            col += 1;
            line.get(col)?;
        } else if c == q {
            return Some(col);
        }
        col += 1;
    }
}

fn find_prev_quote(line: &[char], mut col: usize, q: char, escape: bool) -> usize {
    while col > 0 {
        col -= 1;
        let mut n = 0;
        if escape {
            while col > n && line[col - n - 1] == '\\' {
                n += 1;
            }
        }
        if n % 2 == 1 {
            col -= n;
        } else if line[col] == q {
            break;
        }
    }
    col
}

/// `i"`, `a"`, `i'`, `a'`, `` i` ``, `` a` ``.
pub(crate) fn quote(
    t: Text<'_>,
    cursor: Pos,
    vis: Vis,
    count: usize,
    include: bool,
    q: char,
) -> Option<Area> {
    let line = t.line_chars(cursor.line);
    let vis_empty = vis.is_none_or(|(v, c)| v == c);
    let inside_quotes = false;
    let mut col_start = cursor.col;
    let col_end;
    if !vis_empty {
        let (v, c) = vis.unwrap();
        // Visual area that already covers the inside of a string: "i" again
        // includes the quotes, as Vim does.
        let (lo, hi) = if v < c { (v, c) } else { (c, v) };
        if lo.line == hi.line
            && lo.col > 0
            && line.get(lo.col - 1) == Some(&q)
            && line.get(hi.col + 1) == Some(&q)
        {
            col_start = lo.col - 1;
            let mut ce = hi.col + 1;
            if include {
                while line.get(ce + 1).is_some_and(|&c| is_white(c)) {
                    ce += 1;
                }
            }
            let start = Pos::new(cursor.line, col_start);
            return Some(Area {
                start,
                end: Pos::new(cursor.line, ce),
                inclusive: true,
                linewise: false,
                failed: false,
            });
        }
    }
    if line.get(col_start) == Some(&q) {
        let first_col = col_start;
        let mut s = 0;
        loop {
            s = find_next_quote(&line, s, q, false)?;
            if s > first_col {
                return None;
            }
            let e = find_next_quote(&line, s + 1, q, true)?;
            if s <= first_col && first_col <= e {
                col_start = s;
                col_end = e;
                break;
            }
            s = e + 1;
        }
    } else {
        col_start = find_prev_quote(&line, col_start, q, true);
        if line.get(col_start) != Some(&q) {
            col_start = find_next_quote(&line, col_start, q, false)?;
        }
        col_end = find_next_quote(&line, col_start + 1, q, true)?;
    }
    let mut col_end = col_end;
    if include {
        if line.get(col_end + 1).is_some_and(|&c| is_white(c)) {
            while line.get(col_end + 1).is_some_and(|&c| is_white(c)) {
                col_end += 1;
            }
        } else {
            while col_start > 0 && is_white(line[col_start - 1]) {
                col_start -= 1;
            }
        }
    }
    if !include && count < 2 && (vis_empty || !inside_quotes) {
        col_start += 1;
    }
    let start = Pos::new(cursor.line, col_start);
    let mut end = Pos::new(cursor.line, col_end);
    let mut inclusive = false;
    if include || count > 1 {
        if t.inc(&mut end) == Step::OntoEol {
            inclusive = true;
        }
    }
    if vis.is_some() {
        // Visual areas are inclusive of the cursor character.
        let mut e = end;
        if !inclusive {
            if e.col == 0 {
                return None;
            }
            e.col -= 1;
        }
        if e < start {
            return Some(Area {
                start,
                end: start,
                inclusive: true,
                linewise: false,
                failed: false,
            });
        }
        return Some(Area {
            start,
            end: e,
            inclusive: true,
            linewise: false,
            failed: false,
        });
    }
    Some(Area {
        start,
        end,
        inclusive,
        linewise: false,
        failed: false,
    })
}

struct Tag {
    open_start: usize,
    open_end: usize,
    close_start: usize,
    close_end: usize,
}

/// `it`, `at`: XML/HTML tag blocks, pairing tags case-insensitively.
pub(crate) fn tag(t: Text<'_>, cursor: Pos, vis: Vis, count: usize, include: bool) -> Option<Area> {
    let text: Vec<char> = t.rope.chars().collect();
    let mut stack: Vec<(String, usize, usize)> = Vec::new();
    let mut pairs: Vec<Tag> = Vec::new();
    let mut i = 0;
    while i < text.len() {
        if text[i] != '<' {
            i += 1;
            continue;
        }
        let Some(rel) = text[i..].iter().position(|&c| c == '>') else {
            break;
        };
        let end = i + rel + 1;
        let inner: String = text[i + 1..end - 1].iter().collect();
        if inner.starts_with('!') || inner.starts_with('?') || inner.ends_with('/') {
            i = end;
            continue;
        }
        if let Some(name) = inner.strip_prefix('/') {
            let name = name.trim().to_ascii_lowercase();
            if let Some(idx) = stack.iter().rposition(|(n, _, _)| *n == name) {
                let (_, os, oe) = stack[idx].clone();
                stack.truncate(idx);
                pairs.push(Tag {
                    open_start: os,
                    open_end: oe,
                    close_start: i,
                    close_end: end,
                });
            }
        } else {
            let name: String = inner
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '/')
                .collect::<String>()
                .to_ascii_lowercase();
            if !name.is_empty()
                && name
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_' || c == ':')
            {
                stack.push((name, i, end));
            }
        }
        i = end;
    }
    let (lo, hi) = match vis {
        Some((v, c)) if v != c => {
            let (a, b) = if v < c { (v, c) } else { (c, v) };
            (t.idx(a), t.idx(b) + 1)
        }
        _ => {
            let c = t.idx(cursor);
            (c, c + 1)
        }
    };
    let mut candidates: Vec<&Tag> = pairs
        .iter()
        .filter(|p| p.open_start <= lo && hi <= p.close_end)
        .collect();
    candidates.sort_by_key(|p| p.close_end - p.open_start);
    // A Visual area that already equals a tag's inner/outer area selects the
    // next enclosing tag.
    if vis.is_some_and(|(v, c)| v != c) {
        candidates.retain(|p| {
            let (s, e) = if include {
                (p.open_start, p.close_end)
            } else {
                (p.open_end, p.close_start)
            };
            !(s == lo && e == hi) && s <= lo && hi <= e
        });
    }
    let p = candidates.get(count.max(1) - 1)?;
    let (s, e) = if include {
        (p.open_start, p.close_end)
    } else {
        (p.open_end, p.close_start)
    };
    if e <= s {
        let pos = t.pos(s);
        return Some(Area {
            start: pos,
            end: pos,
            inclusive: false,
            linewise: false,
            failed: false,
        });
    }
    Some(Area {
        start: t.pos(s),
        end: t.pos(e - 1),
        inclusive: true,
        linewise: false,
        failed: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn area(
        text: &str,
        at: (usize, usize),
        f: impl Fn(Text<'_>, Pos) -> Option<Area>,
    ) -> Option<String> {
        let rope = Rope::from_str(text);
        let t = Text::new(&rope);
        let a = f(t, Pos::new(at.0, at.1))?;
        if a.linewise {
            return Some(t.slice(t.line_start(a.start.line), t.line_next_start(a.end.line)));
        }
        let end = t.idx(a.end) + usize::from(a.inclusive);
        Some(t.slice(t.idx(a.start), end))
    }

    #[test]
    fn word_objects_select_trailing_or_leading_white() {
        let kw = Keyword::default();
        assert_eq!(
            area("foo bar baz", (0, 5), |t, p| word(
                t, p, None, 1, false, false, &kw
            ))
            .unwrap(),
            "bar"
        );
        assert_eq!(
            area("foo bar baz", (0, 5), |t, p| word(
                t, p, None, 1, true, false, &kw
            ))
            .unwrap(),
            "bar "
        );
        assert_eq!(
            area("foo bar", (0, 5), |t, p| word(
                t, p, None, 1, true, false, &kw
            ))
            .unwrap(),
            " bar"
        );
        assert_eq!(
            area("a.b c", (0, 0), |t, p| word(
                t, p, None, 1, false, true, &kw
            ))
            .unwrap(),
            "a.b"
        );
    }

    #[test]
    fn block_and_quote_objects() {
        assert_eq!(
            area("f(a, (b))", (0, 3), |t, p| block(
                t, p, None, 1, false, '(', ')'
            ))
            .unwrap(),
            "a, (b)"
        );
        assert_eq!(
            area("f(a, (b))", (0, 6), |t, p| block(
                t, p, None, 2, true, '(', ')'
            ))
            .unwrap(),
            "(a, (b))"
        );
        assert_eq!(
            area("f()", (0, 1), |t, p| block(t, p, None, 1, false, '(', ')')).unwrap(),
            ""
        );
        assert_eq!(
            area("{\n  x\n}", (1, 2), |t, p| block(
                t, p, None, 1, false, '{', '}'
            ))
            .unwrap(),
            "  x\n"
        );
        assert_eq!(
            area("x = \"a b\" + 1", (0, 6), |t, p| quote(
                t, p, None, 1, false, '"'
            ))
            .unwrap(),
            "a b"
        );
        assert_eq!(
            area("x = \"a b\" + 1", (0, 6), |t, p| quote(
                t, p, None, 1, true, '"'
            ))
            .unwrap(),
            "\"a b\" "
        );
        assert_eq!(
            area("x = \"a b\"", (0, 0), |t, p| quote(
                t, p, None, 1, false, '"'
            ))
            .unwrap(),
            "a b"
        );
    }

    #[test]
    fn paragraph_and_tag_objects() {
        let text = "a\nb\n\nc\n";
        assert_eq!(
            area(text, (0, 0), |t, p| paragraph(t, p, None, 1, false)).unwrap(),
            "a\nb\n"
        );
        assert_eq!(
            area(text, (0, 0), |t, p| paragraph(t, p, None, 1, true)).unwrap(),
            "a\nb\n\n"
        );
        assert_eq!(
            area("<a><b>x</b></a>", (0, 6), |t, p| tag(t, p, None, 1, false)).unwrap(),
            "x"
        );
        assert_eq!(
            area("<a><b>x</b></a>", (0, 6), |t, p| tag(t, p, None, 2, true)).unwrap(),
            "<a><b>x</b></a>"
        );
    }
}
