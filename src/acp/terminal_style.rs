//! Safe ANSI transcript decoding. Styles survive paging; control sequences never
//! reach the host terminal, and an unfinished output line stays bounded.
use serde::{Deserialize, Serialize};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;
use vte::Perform;

pub const MAX_LINE_CHARS: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnsiColor {
    Indexed(u8),
    Rgb(u8, u8, u8),
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextStyle {
    pub fg: Option<AnsiColor>,
    pub bg: Option<AnsiColor>,
    /// Bold, dim, italic, underline, reverse, strikethrough.
    pub attributes: u8,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StyledRun {
    pub start: usize,
    pub end: usize,
    pub style: TextStyle,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TerminalLine {
    pub text: String,
    pub runs: Vec<StyledRun>,
}
impl TerminalLine {
    fn push(&mut self, text: &str, style: TextStyle) {
        let start = self.text.len();
        self.text.push_str(text);
        if let Some(last) = self.runs.last_mut().filter(|r| r.style == style) {
            last.end = self.text.len();
        } else {
            self.runs.push(StyledRun {
                start,
                end: self.text.len(),
                style,
            });
        }
    }
    pub fn window(&self, column: usize, width: usize) -> Self {
        let mut result = Self::default();
        let mut x = 0;
        let mut run = 0;
        for (byte, grapheme) in self.text.grapheme_indices(true) {
            let next = x + grapheme.width();
            if next > column.saturating_add(width) {
                break;
            }
            while self.runs.get(run).is_some_and(|r| r.end <= byte) {
                run += 1;
            }
            let style = self.runs.get(run).map_or(TextStyle::default(), |r| r.style);
            if x >= column {
                result.push(grapheme, style);
            } else if next > column {
                result.push(&" ".repeat(next - column), style);
            }
            x = next;
        }
        result
    }
}

pub struct Decoder {
    pub lines: Vec<TerminalLine>,
    tail: Vec<(char, TextStyle)>,
    cursor: usize,
    style: TextStyle,
    parser: vte::Parser,
}
impl Default for Decoder {
    fn default() -> Self {
        Self {
            lines: Vec::new(),
            tail: Vec::new(),
            cursor: 0,
            style: TextStyle::default(),
            parser: vte::Parser::new(),
        }
    }
}
impl Decoder {
    pub fn append(&mut self, bytes: &[u8]) {
        let mut parser = std::mem::replace(&mut self.parser, vte::Parser::new());
        for &byte in bytes {
            parser.advance(self, byte);
        }
        self.parser = parser;
    }
    pub fn current_line(&self) -> TerminalLine {
        let mut line = TerminalLine::default();
        for (ch, style) in &self.tail {
            line.push(ch.encode_utf8(&mut [0; 4]), *style);
        }
        line
    }
    fn newline(&mut self) {
        self.lines.push(self.current_line());
        self.tail.clear();
        self.cursor = 0;
    }
    pub fn input(&mut self, text: &str) {
        if !self.tail.is_empty() {
            self.newline();
        }
        let saved = self.style;
        self.style = TextStyle::default();
        for line in text.trim_end_matches('\n').split('\n') {
            for c in "[user input] ".chars().chain(line.chars()) {
                if c.is_control() {
                    for escaped in c.escape_default() {
                        self.print(escaped);
                    }
                } else {
                    self.print(c);
                }
            }
            self.newline();
        }
        self.style = saved;
    }
    fn sgr(&mut self, params: &vte::Params) {
        let groups: Vec<_> = params.iter().collect();
        let mut i = 0;
        while i < groups.len() {
            let p = groups[i][0];
            match p {
                0 => self.style = TextStyle::default(),
                1 => self.style.attributes |= 1,
                2 => self.style.attributes |= 2,
                3 => self.style.attributes |= 4,
                4 => self.style.attributes |= 8,
                7 => self.style.attributes |= 16,
                9 => self.style.attributes |= 32,
                22 => self.style.attributes &= !3,
                23 => self.style.attributes &= !4,
                24 => self.style.attributes &= !8,
                27 => self.style.attributes &= !16,
                29 => self.style.attributes &= !32,
                30..=37 => self.style.fg = Some(AnsiColor::Indexed((p - 30) as u8)),
                90..=97 => self.style.fg = Some(AnsiColor::Indexed((p - 90 + 8) as u8)),
                40..=47 => self.style.bg = Some(AnsiColor::Indexed((p - 40) as u8)),
                100..=107 => self.style.bg = Some(AnsiColor::Indexed((p - 100 + 8) as u8)),
                39 => self.style.fg = None,
                49 => self.style.bg = None,
                38 | 48 => {
                    let sub = groups[i];
                    let mut consumed = 0;
                    let values: Vec<u16> = if sub.len() > 1 {
                        sub[1..].to_vec()
                    } else {
                        groups[i + 1..].iter().map(|g| g[0]).collect()
                    };
                    let color = match values.as_slice() {
                        [5, n, ..] if *n <= 255 => {
                            consumed = 2;
                            Some(AnsiColor::Indexed(*n as u8))
                        }
                        [2, rest @ ..] => {
                            let rest = if sub.len() > 1 && rest.len() == 4 {
                                &rest[1..]
                            } else {
                                rest
                            };
                            if let [r, g, b, ..] = rest
                                && *r <= 255
                                && *g <= 255
                                && *b <= 255
                            {
                                consumed = 4;
                                Some(AnsiColor::Rgb(*r as u8, *g as u8, *b as u8))
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if let Some(color) = color {
                        if p == 38 {
                            self.style.fg = Some(color);
                        } else {
                            self.style.bg = Some(color);
                        }
                        if sub.len() == 1 {
                            i += consumed;
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}
impl Perform for Decoder {
    fn print(&mut self, c: char) {
        if self.cursor >= MAX_LINE_CHARS {
            self.newline();
        }
        self.tail.resize(
            self.tail.len().max(self.cursor),
            (' ', TextStyle::default()),
        );
        if self.cursor < self.tail.len() {
            self.tail[self.cursor] = (c, self.style);
        } else {
            self.tail.push((c, self.style));
        }
        self.cursor += 1;
    }
    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' => self.newline(),
            b'\r' => self.cursor = 0,
            b'\t' => {
                for _ in 0..(4 - self.cursor % 4) {
                    self.print(' ');
                }
            }
            8 => self.cursor = self.cursor.saturating_sub(1),
            _ => {}
        }
    }
    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        if ignore || !intermediates.is_empty() {
            return;
        }
        let n = params
            .iter()
            .next()
            .and_then(|p| p.first())
            .copied()
            .unwrap_or(0) as usize;
        match action {
            'm' => self.sgr(params),
            'K' => match n {
                0 => self.tail.truncate(self.cursor),
                1 => {
                    for c in self.tail.iter_mut().take(self.cursor + 1) {
                        *c = (' ', self.style);
                    }
                }
                2 => self.tail.clear(),
                _ => {}
            },
            'G' => self.cursor = n.saturating_sub(1).min(MAX_LINE_CHARS),
            'C' => self.cursor = (self.cursor + n.max(1)).min(MAX_LINE_CHARS),
            'D' => self.cursor = self.cursor.saturating_sub(n.max(1)),
            _ => {}
        }
    }
}
pub fn parse(text: &str) -> Vec<TerminalLine> {
    let mut decoder = Decoder::default();
    decoder.append(text.as_bytes());
    let tail = decoder.current_line();
    if !tail.text.is_empty() {
        decoder.lines.push(tail);
    }
    decoder.lines
}
