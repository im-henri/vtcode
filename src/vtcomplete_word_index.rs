//! Words of each document line, kept across popup requests.
//!
//! Every keystroke used to retokenize and hash every word occurrence within
//! the scan budget around the caret. The index keeps each line as interned
//! word ids and follows edits line by line, so a request only checks a stamp
//! per occurrence and scores each distinct word once, at its nearest line.
use super::{MAX_COMPLETION_WORD_BYTES, identifier_char};
use rustc_hash::FxHashMap;

/// Larger documents exceed the request's scan budget, which only the full
/// scan applies exactly.
pub(super) const MAX_INDEXED_BYTES: usize = 320 * 1024;

#[derive(Default)]
pub(super) struct WordIndex {
    rope: ropey::Rope,
    valid: bool,
    lines: Vec<LineWords>,
    ids: FxHashMap<Box<str>, u32>,
    words: Vec<Box<str>>,
    /// Distinct words after the last full build; edits only add more.
    built_words: usize,
    seen: Vec<u32>,
    stamp: u32,
}

#[derive(Default)]
pub(super) struct LineWords {
    /// Words of at least two bytes, in order. Shorter ones never score.
    pub(super) words: Box<[u32]>,
    pub(super) bytes: usize,
    /// Identifier runs longer than a completion word, each deferred once.
    pub(super) oversized: u32,
}

impl WordIndex {
    /// Brings the index up to `rope`, or returns `Some(false)` when the
    /// document is too large to index.
    pub(super) fn update(
        &mut self,
        rope: &ropey::Rope,
        cancelled: &impl Fn() -> bool,
    ) -> Option<bool> {
        if rope.len_bytes() > MAX_INDEXED_BYTES {
            *self = Self::default();
            return Some(false);
        }
        if self.valid && self.rope.is_instance(rope) {
            return Some(true);
        }
        let changed = if self.valid && self.words.len() <= 2 * self.built_words + 4096 {
            crate::rope_changes::changed_lines(&self.rope, rope, cancelled)
        } else {
            None
        };
        // A cancelled update leaves no half-applied state behind.
        self.valid = false;
        if cancelled() {
            return None;
        }
        let mut scratch = String::new();
        match changed {
            Some((old, new)) if old.end <= self.lines.len() => {
                let mut replacement = Vec::with_capacity(new.len());
                for line in new {
                    if cancelled() {
                        return None;
                    }
                    replacement.push(self.tokenize(rope.line(line), &mut scratch));
                }
                self.lines.splice(old, replacement);
            }
            _ => {
                self.lines.clear();
                self.ids.clear();
                self.words.clear();
                for (index, line) in rope.lines().enumerate() {
                    if index % 256 == 0 && cancelled() {
                        return None;
                    }
                    let words = self.tokenize(line, &mut scratch);
                    self.lines.push(words);
                }
                self.built_words = self.words.len();
            }
        }
        if self.lines.len() != rope.len_lines() {
            return None;
        }
        self.rope = rope.clone();
        self.valid = true;
        Some(true)
    }

    pub(super) fn line(&self, line: usize) -> &LineWords {
        &self.lines[line]
    }

    pub(super) fn word(&self, id: u32) -> &str {
        &self.words[id as usize]
    }

    /// Starts a request; [`Self::first_visit`] is true once per word.
    pub(super) fn begin_request(&mut self) {
        self.seen.resize(self.words.len(), 0);
        self.stamp = self.stamp.wrapping_add(1);
        if self.stamp == 0 {
            self.seen.fill(0);
            self.stamp = 1;
        }
    }

    pub(super) fn first_visit(&mut self, id: u32) -> bool {
        let seen = &mut self.seen[id as usize];
        let first = *seen != self.stamp;
        *seen = self.stamp;
        first
    }

    /// Splits a line exactly as `scan_words` does.
    fn tokenize(&mut self, line: ropey::RopeSlice<'_>, word: &mut String) -> LineWords {
        let mut words = Vec::new();
        let mut oversized = 0;
        let mut skipping = false;
        word.clear();
        let mut finish = |word: &mut String, words: &mut Vec<u32>| {
            if word.len() >= 2 {
                words.push(self.intern(word));
            }
            word.clear();
        };
        for chunk in line.chunks() {
            for ch in chunk.chars() {
                if identifier_char(ch) {
                    if !skipping {
                        if word.len() + ch.len_utf8() <= MAX_COMPLETION_WORD_BYTES {
                            word.push(ch);
                        } else {
                            oversized += 1;
                            word.clear();
                            skipping = true;
                        }
                    }
                } else {
                    finish(word, &mut words);
                    skipping = false;
                }
            }
        }
        finish(word, &mut words);
        LineWords {
            words: words.into(),
            bytes: line.len_bytes(),
            oversized,
        }
    }

    fn intern(&mut self, word: &str) -> u32 {
        if let Some(&id) = self.ids.get(word) {
            return id;
        }
        let id = self.words.len() as u32;
        self.words.push(word.into());
        self.ids.insert(word.into(), id);
        id
    }
}
