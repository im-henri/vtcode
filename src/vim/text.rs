//! Vim's view of a rope: lines, columns and cursor stepping.
//!
//! A newline-terminated file has no extra empty line in Vim, although the
//! rope (and VS Code-style rendering) shows one. Vim motions use
//! [`Text::lines`], which leaves that phantom line out.

use ropey::Rope;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub(crate) struct Pos {
    pub(crate) line: usize,
    pub(crate) col: usize,
}

impl Pos {
    pub(crate) const fn new(line: usize, col: usize) -> Self {
        Self { line, col }
    }
}

/// Result of stepping the cursor, mirroring Vim's `inc()`/`dec()`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Moved within the line.
    Same,
    /// Moved onto the end-of-line position (Vim's NUL).
    OntoEol,
    /// Crossed to another line.
    Line,
    /// Could not move: at start or end of the buffer.
    Fail,
}

#[derive(Clone, Copy)]
pub(crate) struct Text<'a> {
    pub(crate) rope: &'a Rope,
    lines: usize,
}

impl<'a> Text<'a> {
    pub(crate) fn new(rope: &'a Rope) -> Self {
        Self::of(rope, false)
    }

    /// `trailing_empty`: the empty text after the final line break is a
    /// line of its own (Vim kept an empty last line).
    pub(crate) fn of(rope: &'a Rope, trailing_empty: bool) -> Self {
        let mut lines = rope.len_lines().max(1);
        if lines > 1 && rope.len_chars() > 0 && !trailing_empty {
            let last = rope.char(rope.len_chars() - 1);
            if last == '\n' || last == '\r' {
                lines -= 1;
            }
        }
        Self { rope, lines }
    }

    /// Number of Vim lines.
    pub(crate) fn lines(&self) -> usize {
        self.lines
    }

    pub(crate) fn last_line(&self) -> usize {
        self.lines - 1
    }

    /// Length of a line's content in characters, without its line ending.
    pub(crate) fn len(&self, line: usize) -> usize {
        if line >= self.rope.len_lines() {
            return 0;
        }
        let slice = self.rope.line(line);
        let mut len = slice.len_chars();
        let mut tail = slice.chars_at(len).reversed();
        match tail.next() {
            Some('\n') => {
                len -= 1;
                if tail.next() == Some('\r') {
                    len -= 1;
                }
            }
            Some('\r') => len -= 1,
            _ => {}
        }
        len
    }

    pub(crate) fn is_empty_line(&self, line: usize) -> bool {
        self.len(line) == 0
    }

    pub(crate) fn line_start(&self, line: usize) -> usize {
        self.rope.line_to_char(line.min(self.rope.len_lines()))
    }

    /// Character offset just past the line's content (before its ending).
    pub(crate) fn line_end(&self, line: usize) -> usize {
        self.line_start(line) + self.len(line)
    }

    /// Offset of the next line's start, or the end of the rope.
    pub(crate) fn line_next_start(&self, line: usize) -> usize {
        if line + 1 < self.rope.len_lines() {
            self.rope.line_to_char(line + 1)
        } else {
            self.rope.len_chars()
        }
    }

    pub(crate) fn idx(&self, pos: Pos) -> usize {
        self.line_start(pos.line) + pos.col.min(self.len(pos.line))
    }

    pub(crate) fn pos(&self, idx: usize) -> Pos {
        let idx = idx.min(self.rope.len_chars());
        let line = self.rope.char_to_line(idx);
        let col = idx - self.rope.line_to_char(line);
        Pos::new(line, col.min(self.len(line)))
    }

    /// Character under `pos`, or `None` on the end-of-line position.
    pub(crate) fn char_at(&self, pos: Pos) -> Option<char> {
        if pos.line >= self.lines || pos.col >= self.len(pos.line) {
            return None;
        }
        Some(self.rope.char(self.line_start(pos.line) + pos.col))
    }

    /// Vim's `gchar_pos`: NUL is reported as `'\0'`.
    pub(crate) fn gchar(&self, pos: Pos) -> char {
        self.char_at(pos).unwrap_or('\0')
    }

    pub(crate) fn line_string(&self, line: usize) -> String {
        if line >= self.rope.len_lines() {
            return String::new();
        }
        let start = self.line_start(line);
        self.rope.slice(start..start + self.len(line)).to_string()
    }

    pub(crate) fn line_chars(&self, line: usize) -> Vec<char> {
        if line >= self.rope.len_lines() {
            return Vec::new();
        }
        let start = self.line_start(line);
        self.rope
            .slice(start..start + self.len(line))
            .chars()
            .collect()
    }

    pub(crate) fn slice(&self, start: usize, end: usize) -> String {
        let end = end.min(self.rope.len_chars());
        let start = start.min(end);
        self.rope.slice(start..end).to_string()
    }

    /// Vim's `inc()`: step forward one character.
    pub(crate) fn inc(&self, pos: &mut Pos) -> Step {
        let len = self.len(pos.line);
        if pos.col < len {
            pos.col += 1;
            return if pos.col < len {
                Step::Same
            } else {
                Step::OntoEol
            };
        }
        if pos.line + 1 < self.lines {
            pos.line += 1;
            pos.col = 0;
            return Step::Line;
        }
        Step::Fail
    }

    /// Vim's `dec()`: step back one character.
    pub(crate) fn dec(&self, pos: &mut Pos) -> Step {
        let len = self.len(pos.line);
        if pos.col > len {
            pos.col = len;
        }
        if pos.col > 0 {
            pos.col -= 1;
            return Step::Same;
        }
        if pos.line > 0 {
            pos.line -= 1;
            pos.col = self.len(pos.line);
            return Step::Line;
        }
        Step::Fail
    }

    /// `inc()` that skips the end-of-line position (Vim's `incl()`).
    pub(crate) fn incl(&self, pos: &mut Pos) -> Step {
        let r = self.inc(pos);
        if matches!(r, Step::OntoEol | Step::Line) && pos.col > 0 {
            return self.inc(pos);
        }
        r
    }

    /// `dec()` that skips the end-of-line position (Vim's `decl()`).
    pub(crate) fn decl(&self, pos: &mut Pos) -> Step {
        let r = self.dec(pos);
        if r == Step::Line && pos.col > 0 {
            return self.dec(pos);
        }
        r
    }

    pub(crate) fn first_nonblank(&self, line: usize) -> usize {
        let chars = self.line_chars(line);
        chars
            .iter()
            .position(|c| !matches!(c, ' ' | '\t'))
            .unwrap_or(chars.len())
    }

    /// Last column a Normal-mode cursor may occupy.
    pub(crate) fn max_normal_col(&self, line: usize) -> usize {
        self.len(line).saturating_sub(1)
    }

    pub(crate) fn clamp_normal(&self, pos: Pos) -> Pos {
        let line = pos.line.min(self.last_line());
        Pos::new(line, pos.col.min(self.max_normal_col(line)))
    }

    pub(crate) fn clamp_insert(&self, pos: Pos) -> Pos {
        let line = pos.line.min(self.rope.len_lines().max(1) - 1);
        Pos::new(line, pos.col.min(self.len(line)))
    }

    /// Display column (0-based) of `col`, expanding tabs and wide characters.
    pub(crate) fn vcol(&self, line: usize, col: usize, tabstop: usize) -> usize {
        let mut v = 0;
        for (i, c) in self.line_chars(line).into_iter().enumerate() {
            if i >= col {
                break;
            }
            v += char_width(c, v, tabstop);
        }
        if col > self.len(line) {
            v + (col - self.len(line))
        } else {
            v
        }
    }

    /// Display column just past the character at `col`.
    pub(crate) fn vcol_end(&self, line: usize, col: usize, tabstop: usize) -> usize {
        let v = self.vcol(line, col, tabstop);
        match self.char_at(Pos::new(line, col)) {
            Some(c) => v + char_width(c, v, tabstop),
            None => v + 1,
        }
    }

    /// Column of the character covering display column `want`; returns the
    /// line length when the line is shorter.
    pub(crate) fn col_for_vcol(&self, line: usize, want: usize, tabstop: usize) -> usize {
        let mut v = 0;
        for (i, c) in self.line_chars(line).into_iter().enumerate() {
            let w = char_width(c, v, tabstop);
            if v + w > want {
                return i;
            }
            v += w;
        }
        self.len(line)
    }

    pub(crate) fn line_vwidth(&self, line: usize, tabstop: usize) -> usize {
        self.vcol(line, self.len(line), tabstop)
    }
}

pub(crate) fn char_width(c: char, vcol: usize, tabstop: usize) -> usize {
    if c == '\t' {
        let ts = tabstop.max(1);
        ts - vcol % ts
    } else {
        unicode_width::UnicodeWidthChar::width(c)
            .unwrap_or(0)
            .max(if c < ' ' { 2 } else { 0 })
    }
}

/// Keyword character test (`iskeyword`). `extra` holds additional
/// characters, `removed` characters excluded from the default set.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Keyword {
    pub(crate) extra: Vec<char>,
    pub(crate) removed: Vec<char>,
}

impl Keyword {
    pub(crate) fn is_word(&self, c: char) -> bool {
        if self.removed.contains(&c) {
            return false;
        }
        if self.extra.contains(&c) {
            return true;
        }
        c == '_'
            || c.is_alphanumeric()
            || (c as u32 >= 192 && c as u32 <= 255 && c != '×' && c != '÷')
    }

    /// Vim's `cls()` for `w`/`b`/`e`: 0 blank (including EOL), 1
    /// punctuation, 2 keyword; Unicode symbols and CJK get classes of their own.
    pub(crate) fn class(&self, c: char, bigword: bool) -> u32 {
        if c == ' ' || c == '\t' || c == '\0' || c == '\u{a0}' || c == '\u{3000}' {
            return 0;
        }
        if bigword {
            return 1;
        }
        if self.is_word(c) {
            let cp = c as u32;
            // Vim gives CJK ideographs, kana and hangul their own classes so
            // that `w` stops between scripts.
            return match cp {
                0x3040..=0x309f => 0x3040,
                0x30a0..=0x30ff => 0x30a0,
                0x4e00..=0x9fff | 0x3400..=0x4dbf | 0xf900..=0xfaff => 0x4e00,
                0xac00..=0xd7a3 => 0xac00,
                _ => 2,
            };
        }
        let cp = c as u32;
        if cp > 0x7f && !c.is_alphanumeric() {
            // Emoji and other symbols: one class per general range.
            return match cp {
                0x1f000..=0x1faff | 0x2600..=0x27bf => 3,
                0x2000..=0x206f => 1,
                _ => 1,
            };
        }
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phantom_final_line_is_hidden() {
        let rope = Rope::from_str("a\nb\n");
        let t = Text::new(&rope);
        assert_eq!(t.lines(), 2);
        let rope = Rope::from_str("a\nb");
        assert_eq!(Text::new(&rope).lines(), 2);
        let rope = Rope::from_str("");
        assert_eq!(Text::new(&rope).lines(), 1);
        let rope = Rope::from_str("\n");
        assert_eq!(Text::new(&rope).lines(), 1);
        let rope = Rope::from_str("a\r\nb\r\n");
        let t = Text::new(&rope);
        assert_eq!((t.lines(), t.len(0), t.len(1)), (2, 1, 1));
    }

    #[test]
    fn stepping_matches_vim_inc_and_dec() {
        let rope = Rope::from_str("ab\n\ncd");
        let t = Text::new(&rope);
        let mut p = Pos::new(0, 0);
        assert_eq!(t.inc(&mut p), Step::Same);
        assert_eq!(t.inc(&mut p), Step::OntoEol);
        assert_eq!(t.inc(&mut p), Step::Line);
        assert_eq!(p, Pos::new(1, 0));
        assert_eq!(t.inc(&mut p), Step::Line);
        assert_eq!(p, Pos::new(2, 0));
        let mut end = Pos::new(2, 2);
        assert_eq!(t.inc(&mut end), Step::Fail);
        let mut q = Pos::new(2, 0);
        assert_eq!(t.dec(&mut q), Step::Line);
        assert_eq!(q, Pos::new(1, 0));
        assert_eq!(t.dec(&mut q), Step::Line);
        assert_eq!(q, Pos::new(0, 2));
    }

    #[test]
    fn virtual_columns_expand_tabs_and_wide_chars() {
        let rope = Rope::from_str("\tab界c");
        let t = Text::new(&rope);
        assert_eq!(t.vcol(0, 1, 8), 8);
        assert_eq!(t.vcol(0, 4, 8), 12);
        assert_eq!(t.col_for_vcol(0, 3, 8), 0);
        assert_eq!(t.col_for_vcol(0, 11, 8), 3);
        assert_eq!(t.col_for_vcol(0, 99, 8), 5);
    }
}
