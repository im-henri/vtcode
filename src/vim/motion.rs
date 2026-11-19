//! Cursor motions, ported from Vim's `search.c` and `normal.c` so that
//! edge cases (empty lines, end of buffer, operator special cases) match.

use super::text::{Keyword, Pos, Step, Text};

/// A cursor walking over [`Text`], tracking `cls()` for word motions.
pub(crate) struct Walker<'a, 'k> {
    pub(crate) t: Text<'a>,
    pub(crate) pos: Pos,
    pub(crate) kw: &'k Keyword,
    pub(crate) big: bool,
}

impl<'a, 'k> Walker<'a, 'k> {
    pub(crate) fn new(t: Text<'a>, pos: Pos, kw: &'k Keyword, big: bool) -> Self {
        Self { t, pos, kw, big }
    }
    fn cls(&self) -> u32 { self.kw.class(self.t.gchar(self.pos), self.big) }
    fn inc(&mut self) -> Step { self.t.inc(&mut self.pos) }
    fn dec(&mut self) -> Step { self.t.dec(&mut self.pos) }
    fn line_empty(&self) -> bool { self.t.is_empty_line(self.pos.line) }

    /// Returns true when the buffer edge stopped the walk.
    fn skip_chars(&mut self, class: u32, forward: bool) -> bool {
        while self.cls() == class {
            let r = if forward { self.inc() } else { self.dec() };
            if r == Step::Fail { return true; }
        }
        false
    }

    /// Vim's `fwd_word()`. `eol` is set when an operator is pending.
    pub(crate) fn fwd_word(&mut self, count: usize, eol: bool) -> bool {
        let mut count = count as isize;
        while count > 0 {
            count -= 1;
            let sclass = self.cls();
            let last_line = self.pos.line == self.t.last_line();
            let i = self.inc();
            if i == Step::Fail || (i != Step::Same && last_line) {
                return false;
            }
            if i != Step::Same && eol && count == 0 {
                return true;
            }
            if sclass != 0 {
                while self.cls() == sclass {
                    let i = self.inc();
                    if i == Step::Fail || (i != Step::Same && eol && count == 0) {
                        return true;
                    }
                }
            }
            while self.cls() == 0 {
                if self.pos.col == 0 && self.line_empty() {
                    break;
                }
                let i = self.inc();
                if i == Step::Fail || (i != Step::Same && eol && count == 0) {
                    return true;
                }
            }
        }
        true
    }

    /// Vim's `bck_word()`.
    pub(crate) fn bck_word(&mut self, count: usize, mut stop: bool) -> bool {
        for _ in 0..count {
            let sclass = self.cls();
            if self.dec() == Step::Fail {
                return false;
            }
            let mut finished = false;
            if !stop || sclass == self.cls() || sclass == 0 {
                loop {
                    if self.cls() != 0 { break; }
                    if self.pos.col == 0 && self.line_empty() {
                        finished = true;
                        break;
                    }
                    if self.dec() == Step::Fail {
                        return true;
                    }
                }
                if !finished {
                    let c = self.cls();
                    if self.skip_chars(c, false) {
                        return true;
                    }
                }
            }
            if !finished {
                self.inc();
            }
            stop = false;
        }
        true
    }

    /// Vim's `end_word()`.
    pub(crate) fn end_word(&mut self, count: usize, mut stop: bool, empty: bool) -> bool {
        for _ in 0..count {
            let sclass = self.cls();
            if self.inc() == Step::Fail {
                return false;
            }
            let mut finished = false;
            if self.cls() == sclass && sclass != 0 {
                if self.skip_chars(sclass, true) {
                    return false;
                }
            } else if !stop || sclass == 0 {
                while self.cls() == 0 {
                    if self.pos.col == 0 && self.line_empty() && empty {
                        finished = true;
                        break;
                    }
                    if self.inc() == Step::Fail {
                        return false;
                    }
                }
                if !finished {
                    let c = self.cls();
                    if self.skip_chars(c, true) {
                        return false;
                    }
                }
            }
            if !finished {
                self.dec();
            }
            stop = false;
        }
        true
    }

    /// Vim's `bckend_word()` (`ge`).
    pub(crate) fn bckend_word(&mut self, count: usize, eol: bool) -> bool {
        for _ in 0..count {
            let sclass = self.cls();
            let i = self.dec();
            if i == Step::Fail {
                return false;
            }
            if eol && i == Step::Line {
                return true;
            }
            if sclass != 0 {
                while self.cls() == sclass {
                    let i = self.dec();
                    if i == Step::Fail || (eol && i == Step::Line) {
                        return true;
                    }
                }
            }
            while self.cls() == 0 {
                if self.pos.col == 0 && self.line_empty() {
                    break;
                }
                let i = self.dec();
                if i == Step::Fail || (eol && i == Step::Line) {
                    return true;
                }
            }
        }
        true
    }
}

/// `f`/`F`/`t`/`T` within the cursor line. Returns the new column.
pub(crate) fn find_char(
    t: &Text<'_>,
    pos: Pos,
    c: char,
    forward: bool,
    till: bool,
    count: usize,
    is_repeat: bool,
) -> Option<usize> {
    let chars = t.line_chars(pos.line);
    let len = chars.len();
    let mut col = pos.col as isize;
    // ";" after "t" must move even when the target is adjacent (cpo-;).
    let mut stop = !(is_repeat && count == 1 && till);
    for _ in 0..count.max(1) {
        loop {
            if forward {
                col += 1;
                if col as usize >= len { return None; }
            } else {
                if col <= 0 { return None; }
                col -= 1;
            }
            if chars[col as usize] == c && stop {
                break;
            }
            stop = true;
        }
    }
    if till {
        col -= if forward { 1 } else { -1 };
    }
    Some(col as usize)
}

const PARAGRAPH_MACROS: &str = "IPLPPPQPP TPHPLIPpLpItpplpipbp";
const SECTION_MACROS: &str = "SHNHH HUnhsh";

fn in_macro(opt: &str, s: &[char]) -> bool {
    let o: Vec<char> = opt.chars().collect();
    let mut i = 0;
    while i < o.len() {
        let a = o[i];
        let b = o.get(i + 1).copied().unwrap_or(' ');
        let s0 = s.first().copied().unwrap_or('\0');
        let s1 = s.get(1).copied().unwrap_or('\0');
        if a == s0 && ((s1 == '\0' || s1 == ' ') && (b == ' ' || i + 1 >= o.len()) || b == s1) {
            return true;
        }
        i += 2;
    }
    false
}

/// Vim's `startPS()`: is `line` a paragraph or section boundary?
pub(crate) fn start_ps(t: &Text<'_>, line: usize, para: char, both: bool) -> bool {
    let chars = t.line_chars(line);
    let s0 = chars.first().copied().unwrap_or('\0');
    if s0 == para || s0 == '\x0c' || (both && s0 == '}') {
        return true;
    }
    if s0 == '.' && (in_macro(SECTION_MACROS, &chars[1..]) || (para == '\0' && in_macro(PARAGRAPH_MACROS, &chars[1..]))) {
        return true;
    }
    false
}

/// Vim's `findpar()`. Returns the target and whether the motion became
/// inclusive.
pub(crate) fn find_par(t: &Text<'_>, from: Pos, forward: bool, count: usize, what: char, both: bool) -> Option<(Pos, bool)> {
    let mut curr = from.line as isize;
    let last = t.last_line() as isize;
    let mut count = count as isize;
    while count > 0 {
        count -= 1;
        let mut did_skip = false;
        let mut first = true;
        loop {
            if !t.is_empty_line(curr as usize) {
                did_skip = true;
            }
            if !first && did_skip && start_ps(t, curr as usize, what, both) {
                break;
            }
            curr += if forward { 1 } else { -1 };
            if curr < 0 || curr > last {
                if count > 0 {
                    return None;
                }
                curr -= if forward { 1 } else { -1 };
                break;
            }
            first = false;
        }
    }
    let mut curr = curr as usize;
    if both && t.char_at(Pos::new(curr, 0)) == Some('}') {
        curr += 1;
    }
    let mut incl = false;
    let col = if curr == t.last_line() && what != '}' && (forward || t.lines() == 1) {
        let len = t.len(curr);
        if len != 0 {
            incl = true;
            len - 1
        } else { 0 }
    } else { 0 };
    Some((Pos::new(curr.min(t.last_line()), col), incl))
}

fn is_white(c: char) -> bool { c == ' ' || c == '\t' }

/// Vim's `findsent()`.
pub(crate) fn find_sent(t: &Text<'_>, from: Pos, forward: bool, count: usize) -> Option<Pos> {
    let mut pos = from;
    let func = |p: &mut Pos| if forward { t.incl(p) } else { t.decl(p) };
    for remaining in (0..count).rev() {
        let mut noskip = false;
        'found: {
            if t.gchar(pos) == '\0' {
                loop {
                    if func(&mut pos) == Step::Fail { break; }
                    if t.gchar(pos) != '\0' { break; }
                }
                if forward { break 'found; }
            } else if forward && pos.col == 0 && start_ps(t, pos.line, '\0', false) {
                if pos.line == t.last_line() {
                    return None;
                }
                pos.line += 1;
                break 'found;
            } else if !forward {
                t.decl(&mut pos);
            }

            let mut found_dot = false;
            loop {
                let c = t.gchar(pos);
                if !(is_white(c) || ".!?)]\"'".contains(c) && c != '\0') {
                    break;
                }
                let mut tpos = pos;
                if t.decl(&mut tpos) == Step::Fail || (t.is_empty_line(tpos.line) && forward) {
                    break;
                }
                if found_dot { break; }
                if ".!?".contains(c) { found_dot = true; }
                let tc = t.gchar(tpos);
                if ")]\"'".contains(c) && !(".!?)]\"'".contains(tc) && tc != '\0') {
                    break;
                }
                t.decl(&mut pos);
            }

            let startlnum = pos.line;
            loop {
                let c = t.gchar(pos);
                if c == '\0' || (pos.col == 0 && start_ps(t, pos.line, '\0', false)) {
                    if !forward && pos.line != startlnum {
                        pos.line += 1;
                        pos.col = 0;
                    }
                    break;
                }
                if c == '.' || c == '!' || c == '?' {
                    let mut tpos = pos;
                    let mut failed = false;
                    let mut nc;
                    loop {
                        if t.inc(&mut tpos) == Step::Fail { failed = true; nc = '\0'; break; }
                        nc = t.gchar(tpos);
                        if !(")]\"'".contains(nc) && nc != '\0') { break; }
                    }
                    if failed || nc == ' ' || nc == '\t' || nc == '\0' {
                        pos = tpos;
                        if t.gchar(pos) == '\0' {
                            t.inc(&mut pos);
                        }
                        break;
                    }
                }
                if func(&mut pos) == Step::Fail {
                    if remaining > 0 {
                        return None;
                    }
                    noskip = true;
                    break;
                }
            }
        }
        while !noskip && is_white(t.gchar(pos)) {
            if t.incl(&mut pos) == Step::Fail { break; }
        }
    }
    Some(pos)
}

/// Vim's `adjust_cursor()`: leave the NUL after a forward motion.
/// Returns true when the motion became inclusive.
pub(crate) fn adjust_off_eol(t: &Text<'_>, pos: &mut Pos) -> bool {
    if pos.col > 0 && t.char_at(*pos).is_none() {
        pos.col -= 1;
        return true;
    }
    false
}

const OPEN: [char; 3] = ['(', '[', '{'];
const CLOSE: [char; 3] = [')', ']', '}'];

/// `%`: find the bracket match for the first bracket at or after the cursor
/// on its line, or a C comment / preprocessor match.
pub(crate) fn match_pair(t: &Text<'_>, pos: Pos) -> Option<Pos> {
    let chars = t.line_chars(pos.line);
    // Preprocessor conditionals when the cursor line is one.
    let first = t.first_nonblank(pos.line);
    if chars.get(first) == Some(&'#') {
        if let Some(p) = match_preproc(t, pos.line) {
            return Some(p);
        }
    }
    // C comment delimiters under the cursor.
    if let Some(p) = match_comment(t, pos) {
        return Some(p);
    }
    let mut col = pos.col;
    let (c, idx) = loop {
        let c = *chars.get(col)?;
        if let Some(i) = OPEN.iter().position(|&o| o == c) { break (c, (i, true)); }
        if let Some(i) = CLOSE.iter().position(|&o| o == c) { break (c, (i, false)); }
        col += 1;
    };
    let _ = c;
    let (open, close) = (OPEN[idx.0], CLOSE[idx.0]);
    let start = Pos::new(pos.line, col);
    let in_quote = quote_state_at(&chars, col);
    let mut depth = 0usize;
    let mut p = start;
    let forward = idx.1;
    loop {
        let r = if forward { t.inc(&mut p) } else { t.dec(&mut p) };
        if r == Step::Fail { return None; }
        let Some(ch) = t.char_at(p) else { continue; };
        if ch != open && ch != close { continue; }
        // Skip brackets inside quotes on the same line unless the start
        // bracket was itself quoted (Vim's "inquote" heuristic).
        let line_chars;
        let lc = if p.line == pos.line { &chars } else { line_chars = t.line_chars(p.line); &line_chars };
        if !in_quote && quote_state_at(lc, p.col) { continue; }
        if is_escaped(lc, p.col) { continue; }
        if (ch == close) == forward {
            if depth == 0 { return Some(p); }
            depth -= 1;
        } else {
            depth += 1;
        }
    }
}

fn is_escaped(chars: &[char], col: usize) -> bool {
    let mut n = 0;
    let mut i = col;
    while i > 0 && chars[i - 1] == '\\' { n += 1; i -= 1; }
    n % 2 == 1 && col > 0 && chars.get(col - 1) == Some(&'\\') && {
        // Only treat a backslash escape when inside quotes, like Vim.
        quote_state_at(chars, col)
    }
}

/// Is `col` inside a double-quoted string on this line?
fn quote_state_at(chars: &[char], col: usize) -> bool {
    let mut inside = false;
    let mut i = 0;
    while i < col.min(chars.len()) {
        match chars[i] {
            '\\' if inside => i += 1,
            '"' => inside = !inside,
            '\'' if i + 2 < chars.len() && chars[i + 2] == '\'' => i += 2,
            _ => {}
        }
        i += 1;
    }
    inside
}

fn match_comment(t: &Text<'_>, pos: Pos) -> Option<Pos> {
    let chars = t.line_chars(pos.line);
    let c = *chars.get(pos.col)?;
    let next = chars.get(pos.col + 1).copied();
    let prev = pos.col.checked_sub(1).and_then(|i| chars.get(i)).copied();
    let at_open = (c == '/' && next == Some('*')) || (c == '*' && prev == Some('/'));
    let at_close = (c == '*' && next == Some('/')) || (c == '/' && prev == Some('*'));
    if at_open {
        let from = t.idx(pos);
        let text = t.slice(from, t.rope.len_chars());
        let close = text.find("*/")?;
        let n = text[..close].chars().count();
        return Some(t.pos(from + n + 1));
    }
    if at_close {
        let to = t.idx(pos);
        let text = t.slice(0, to);
        let open = text.rfind("/*")?;
        return Some(t.pos(text[..open].chars().count()));
    }
    None
}

fn preproc_kind(t: &Text<'_>, line: usize) -> Option<u8> {
    let chars = t.line_chars(line);
    let first = t.first_nonblank(line);
    if chars.get(first) != Some(&'#') { return None; }
    let rest: String = chars[first + 1..].iter().collect();
    let word = rest.trim_start();
    if word.starts_with("if") { Some(0) }
    else if word.starts_with("el") { Some(1) }
    else if word.starts_with("endif") { Some(2) }
    else { None }
}

fn match_preproc(t: &Text<'_>, line: usize) -> Option<Pos> {
    let kind = preproc_kind(t, line)?;
    let mut depth = 0;
    let to_pos = |l: usize| Pos::new(l, t.first_nonblank(l));
    if kind == 2 {
        // #endif: back to the matching #if.
        let mut l = line;
        while l > 0 {
            l -= 1;
            match preproc_kind(t, l) {
                Some(2) => depth += 1,
                Some(0) if depth == 0 => return Some(to_pos(l)),
                Some(0) => depth -= 1,
                _ => {}
            }
        }
        return None;
    }
    let mut l = line;
    while l < t.last_line() {
        l += 1;
        match preproc_kind(t, l) {
            Some(0) => depth += 1,
            Some(1) if depth == 0 => return Some(to_pos(l)),
            Some(2) if depth == 0 => return Some(to_pos(l)),
            Some(2) => depth -= 1,
            _ => {}
        }
    }
    None
}

/// `[(`, `[{`, `])`, `]}`: go to the enclosing unmatched bracket.
pub(crate) fn unmatched(t: &Text<'_>, pos: Pos, open: char, close: char, forward: bool, count: usize) -> Option<Pos> {
    let mut p = pos;
    for _ in 0..count.max(1) {
        let mut depth = 0usize;
        loop {
            let r = if forward { t.inc(&mut p) } else { t.dec(&mut p) };
            if r == Step::Fail { return None; }
            let Some(c) = t.char_at(p) else { continue; };
            if c == (if forward { open } else { close }) {
                depth += 1;
            } else if c == (if forward { close } else { open }) {
                if depth == 0 { break; }
                depth -= 1;
            }
        }
    }
    Some(p)
}

/// `[[`, `]]`, `[]`, `][`: lines starting with `{` or `}` in column 0.
pub(crate) fn section(t: &Text<'_>, pos: Pos, forward: bool, brace: char, count: usize) -> Pos {
    let mut line = pos.line;
    for _ in 0..count.max(1) {
        loop {
            if forward {
                if line >= t.last_line() { return Pos::new(t.last_line(), 0); }
                line += 1;
            } else {
                if line == 0 { return Pos::new(0, 0); }
                line -= 1;
            }
            if t.char_at(Pos::new(line, 0)) == Some(brace) || t.char_at(Pos::new(line, 0)) == Some('\x0c') {
                break;
            }
        }
    }
    Pos::new(line, 0)
}

/// `[m`/`]m`/`[M`/`]M`: start or end of a method (brace one level inside
/// a class-like block). Approximated by braces that open/close at depth >= 1.
pub(crate) fn method_brace(t: &Text<'_>, pos: Pos, forward: bool, want_open: bool, count: usize) -> Option<Pos> {
    let mut p = pos;
    let target = if want_open { '{' } else { '}' };
    for _ in 0..count.max(1) {
        loop {
            let r = if forward { t.inc(&mut p) } else { t.dec(&mut p) };
            if r == Step::Fail { return None; }
            if t.char_at(p) == Some(target) { break; }
        }
    }
    Some(p)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    fn walk(text: &str, pos: (usize, usize), f: impl FnOnce(&mut Walker) -> bool) -> ((usize, usize), bool) {
        let rope = Rope::from_str(text);
        let t = Text::new(&rope);
        let kw = Keyword::default();
        let mut w = Walker::new(t, Pos::new(pos.0, pos.1), &kw, false);
        let ok = f(&mut w);
        ((w.pos.line, w.pos.col), ok)
    }

    #[test]
    fn word_motions_handle_punctuation_and_blank_lines() {
        assert_eq!(walk("foo.bar baz", (0, 0), |w| w.fwd_word(1, false)).0, (0, 3));
        assert_eq!(walk("foo.bar baz", (0, 3), |w| w.fwd_word(1, false)).0, (0, 4));
        assert_eq!(walk("foo\n\nbar", (0, 0), |w| w.fwd_word(1, false)).0, (1, 0));
        assert_eq!(walk("foo\n  bar", (0, 0), |w| w.fwd_word(1, false)).0, (1, 2));
        // Operator-pending w stops at the end of the line.
        assert_eq!(walk("foo\n  bar", (0, 0), |w| w.fwd_word(1, true)).0, (0, 3));
        assert_eq!(walk("foo bar", (0, 4), |w| w.bck_word(1, false)).0, (0, 0));
        assert_eq!(walk("foo bar", (0, 0), |w| w.end_word(1, false, false)).0, (0, 2));
        assert_eq!(walk("foo bar", (0, 2), |w| w.end_word(1, false, false)).0, (0, 6));
        assert_eq!(walk("foo bar", (0, 5), |w| w.bckend_word(1, false)).0, (0, 2));
        assert_eq!(walk("a\n\nb", (2, 0), |w| w.bck_word(1, false)).0, (1, 0));
    }

    #[test]
    fn find_char_and_till_repeat_skip_adjacent_targets() {
        let rope = Rope::from_str("a,b,c");
        let t = Text::new(&rope);
        assert_eq!(find_char(&t, Pos::new(0, 0), ',', true, false, 2, false), Some(3));
        assert_eq!(find_char(&t, Pos::new(0, 0), ',', true, true, 1, false), Some(0));
        assert_eq!(find_char(&t, Pos::new(0, 0), ',', true, true, 1, true), Some(2));
        assert_eq!(find_char(&t, Pos::new(0, 4), ',', false, true, 1, false), Some(4));
        assert_eq!(find_char(&t, Pos::new(0, 0), 'z', true, false, 1, false), None);
    }

    #[test]
    fn percent_matches_nested_brackets_and_skips_quoted_ones() {
        let rope = Rope::from_str("f(a, \")\", (b))");
        let t = Text::new(&rope);
        assert_eq!(match_pair(&t, Pos::new(0, 0)), Some(Pos::new(0, 13)));
        assert_eq!(match_pair(&t, Pos::new(0, 13)), Some(Pos::new(0, 1)));
        let rope = Rope::from_str("#if X\na\n#else\nb\n#endif");
        let t = Text::new(&rope);
        assert_eq!(match_pair(&t, Pos::new(0, 0)), Some(Pos::new(2, 0)));
        assert_eq!(match_pair(&t, Pos::new(2, 0)), Some(Pos::new(4, 0)));
        assert_eq!(match_pair(&t, Pos::new(4, 0)), Some(Pos::new(0, 0)));
    }
}
