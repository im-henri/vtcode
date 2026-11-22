//! Words of each document line, kept across popup requests.
//!
//! Every keystroke used to retokenize and hash every word occurrence within
//! the scan budget around the caret. The index keeps each line as interned
//! word ids and follows edits line by line, so a request only checks a stamp
//! per occurrence and scores each distinct word once, at its nearest line.
use super::{MAX_COMPLETION_WORD_BYTES, identifier_char};
use rustc_hash::FxHashMap;

/// Requests on larger documents scan near the caret only, so building an index
/// of the whole document would cost more than it saves. Documents between this
/// and the request's scan budget finish their last lines with the exact scan.
pub(super) const MAX_INDEXED_BYTES: usize = 4 * 1024 * 1024;

/// Bytes of missing lines one request tokenizes. A large document is indexed
/// over several requests, which scan meanwhile, so no single keystroke pays
/// for the whole build and a cancelled request keeps the lines it finished.
pub(super) const FILL_BYTES_PER_REQUEST: usize = 256 * 1024;

/// Changed lines tokenized while following an edit; a larger replacement,
/// such as a paste, is left to the bounded fill instead.
const EAGER_CHANGED_LINES: usize = 256;

#[derive(Default)]
pub(super) struct WordIndex {
    rope: ropey::Rope,
    /// `lines` has one entry per line of `rope`.
    valid: bool,
    /// `None` for a line not tokenized yet.
    lines: Vec<Option<LineWords>>,
    /// Number of `None` lines.
    missing: usize,
    /// Every line before this one is tokenized.
    fill_from: usize,
    /// Interned words were cleared and the index is still filling.
    rebuilding: bool,
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
    /// Brings the index up to `rope`. Returns `Some(false)` when the document
    /// is too large to index, or while its lines are still being filled in.
    pub(super) fn update(
        &mut self,
        rope: &ropey::Rope,
        cancelled: &impl Fn() -> bool,
    ) -> Option<bool> {
        if rope.len_bytes() > MAX_INDEXED_BYTES {
            *self = Self::default();
            return Some(false);
        }
        if !(self.valid && self.rope.is_instance(rope)) {
            self.follow(rope, cancelled)?;
        }
        self.fill(cancelled)
    }

    /// Matches the line layout to `rope`. A cancelled update changes nothing.
    fn follow(&mut self, rope: &ropey::Rope, cancelled: &impl Fn() -> bool) -> Option<()> {
        let changed =
            if self.valid && (self.rebuilding || self.words.len() <= 2 * self.built_words + 4096) {
                crate::rope_changes::changed_lines(&self.rope, rope, cancelled)
            } else {
                None
            };
        if cancelled() {
            return None;
        }
        let mut scratch = String::new();
        match changed {
            Some((old, new)) if old.end <= self.lines.len() => {
                let eager = new.len() <= EAGER_CHANGED_LINES;
                let mut replacement = Vec::with_capacity(new.len());
                for line in new.clone() {
                    if cancelled() {
                        return None;
                    }
                    replacement.push(eager.then(|| self.tokenize(rope.line(line), &mut scratch)));
                }
                self.missing -= self.lines[old.clone()]
                    .iter()
                    .filter(|line| line.is_none())
                    .count();
                self.missing += replacement.iter().filter(|line| line.is_none()).count();
                // Lines before `fill_from` stay tokenized: those before the
                // edit are unchanged, and the replacement is when eager.
                self.fill_from = if !eager {
                    self.fill_from.min(new.start)
                } else if self.fill_from <= old.start {
                    self.fill_from
                } else if self.fill_from >= old.end {
                    self.fill_from - old.len() + new.len()
                } else {
                    new.end
                };
                self.lines.splice(old, replacement);
            }
            _ => {
                self.lines.clear();
                self.lines.resize_with(rope.len_lines(), || None);
                self.missing = self.lines.len();
                self.fill_from = 0;
                self.rebuilding = true;
                self.ids.clear();
                self.words.clear();
            }
        }
        self.valid = self.lines.len() == rope.len_lines();
        if !self.valid {
            *self = Self::default();
            return None;
        }
        self.rope = rope.clone();
        Some(())
    }

    /// Tokenizes missing lines within the per-request allowance. Returns
    /// whether every line is indexed.
    fn fill(&mut self, cancelled: &impl Fn() -> bool) -> Option<bool> {
        if self.missing == 0 {
            return Some(true);
        }
        let rope = self.rope.clone();
        let mut scratch = String::new();
        let mut bytes = 0;
        let mut filled = 0usize;
        for (index, line) in (self.fill_from..).zip(rope.lines_at(self.fill_from)) {
            if self.missing == 0 || bytes >= FILL_BYTES_PER_REQUEST {
                break;
            }
            if self.lines[index].is_none() {
                if filled % 256 == 0 && cancelled() {
                    return None;
                }
                filled += 1;
                bytes += line.len_bytes() + 1;
                self.lines[index] = Some(self.tokenize(line, &mut scratch));
                self.missing -= 1;
            }
            if self.lines[index].is_some() && self.fill_from == index {
                self.fill_from = index + 1;
            }
        }
        if self.missing > 0 {
            return Some(false);
        }
        if self.rebuilding {
            self.rebuilding = false;
            self.built_words = self.words.len();
        }
        Some(true)
    }

    pub(super) fn line(&self, line: usize) -> &LineWords {
        self.lines[line]
            .as_ref()
            .expect("the index is complete when read")
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

    /// Whether [`Self::first_visit`] already saw `id` in this request.
    pub(super) fn visited(&self, id: u32) -> bool {
        self.seen[id as usize] == self.stamp
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
