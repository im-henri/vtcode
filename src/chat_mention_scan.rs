//! Borrowed file-mention ranges. Ordinary prose and fenced code are skipped
//! with byte searches; callers allocate only paths they actually need.
use std::ops::Range;

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Span {
    pub range: Range<usize>,
    pub path: Range<usize>,
    pub closed: bool,
}

pub(super) struct Scanner<'a> {
    text: &'a str,
    position: usize,
    search_end: usize,
    ticks: usize,
}

impl<'a> Scanner<'a> {
    pub fn new(text: &'a str, search_end: usize) -> Self {
        debug_assert!(text.is_char_boundary(search_end));
        Self {
            text,
            position: 0,
            search_end,
            ticks: 0,
        }
    }

    fn next_start(&mut self) -> Option<usize> {
        let bytes = self.text.as_bytes();
        while self.position < self.search_end {
            let offset = memchr::memchr2(b'@', b'`', &bytes[self.position..self.search_end])?;
            let at = self.position + offset;
            self.position = at + 1;
            if bytes[at] == b'`' {
                let count = bytes[at..].iter().take_while(|&&byte| byte == b'`').count();
                if self.ticks == 0 {
                    self.ticks = count;
                } else if self.ticks == count {
                    self.ticks = 0;
                }
                self.position = at + count;
                continue;
            }
            if self.ticks != 0 {
                continue;
            }
            if at == 0
                || self.text[..at]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_whitespace() || matches!(c, '(' | '[' | '{'))
            {
                return Some(at);
            }
        }
        None
    }
}

impl Iterator for Scanner<'_> {
    type Item = Span;
    fn next(&mut self) -> Option<Span> {
        let start = self.next_start()?;
        let bytes = self.text.as_bytes();
        let quote = bytes
            .get(self.position)
            .copied()
            .filter(|byte| matches!(byte, b'\'' | b'"'));
        self.position += usize::from(quote.is_some());
        let path_start = self.position;
        if let Some(quote) = quote {
            self.position += memchr::memchr2(quote, b'\n', &bytes[self.position..])
                .unwrap_or(bytes.len() - self.position);
        } else {
            while self.position < bytes.len() {
                let byte = bytes[self.position];
                if byte.is_ascii() {
                    if byte.is_ascii_whitespace()
                        || matches!(byte, 0x0b | b')' | b']' | b'}' | b',')
                    {
                        break;
                    }
                    self.position += 1;
                } else {
                    let ch = self.text[self.position..].chars().next().unwrap();
                    if ch.is_whitespace() {
                        break;
                    }
                    self.position += ch.len_utf8();
                }
            }
        }
        let path_end = self.position;
        let closed = quote.is_none_or(|quote| bytes.get(self.position) == Some(&quote));
        if quote.is_some() && closed {
            self.position += 1;
        }
        Some(Span {
            range: start..self.position,
            path: path_start..path_end,
            closed,
        })
    }
}

pub(super) fn has_mentions(text: &str) -> bool {
    Scanner::new(text, text.len()).next_start().is_some()
}

#[cfg(test)]
#[path = "chat_mention_scan_tests.rs"]
mod tests;
