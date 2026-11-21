//! Subword tokens for the GPTComplete language model.
//!
//! Text is cut into pieces (a word with the space before it, one digit, a run
//! of signs, a line end with the next line's indentation), and every piece is
//! spelled with byte-pair tokens: the 256 single bytes, and pairs merged into
//! longer tokens in the order they were learned. Any text at all can be
//! written this way, so the model can write names and words it has never
//! seen, in any language.
use rustc_hash::FxHashMap;

/// Longest piece; longer ones are cut.
const MAX_PIECE: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Digit,
    Space,
    Line,
    Sign,
}

fn class(ch: char) -> Class {
    match ch {
        '\n' => Class::Line,
        ' ' | '\t' | '\r' => Class::Space,
        '0'..='9' => Class::Digit,
        '_' => Class::Word,
        ch if ch.is_alphabetic() => Class::Word,
        _ => Class::Sign,
    }
}

/// Byte ranges of the pieces of `text`, in order and covering all of it.
/// A single space belongs to the piece after it.
pub(crate) fn pieces(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(text.len() / 3 + 1);
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let end_of = |index: usize| chars.get(index).map_or(text.len(), |&(at, _)| at);
    let mut index = 0;
    while index < chars.len() {
        let (start, ch) = chars[index];
        let mut next = index + 1;
        match class(ch) {
            Class::Line => {
                while next < chars.len() && matches!(chars[next].1, ' ' | '\t') {
                    next += 1;
                }
            }
            Class::Space => {
                while next < chars.len() && class(chars[next].1) == Class::Space {
                    next += 1;
                }
                let ends = next >= chars.len() || class(chars[next].1) == Class::Line;
                if !ends && ch == ' ' && next - index == 1 {
                    // One space: it starts the next piece.
                    let (_, following) = chars[next];
                    next = run(&chars, next, class(following));
                } else if !ends && chars[next - 1].1 == ' ' {
                    // The last space of a run goes with the next piece.
                    next -= 1;
                }
            }
            class => next = run(&chars, index, class),
        }
        out.push((start, end_of(next)));
        index = next;
    }
    // Long pieces are cut, at character boundaries.
    let mut cut = Vec::with_capacity(out.len());
    for (mut start, end) in out {
        while end - start > MAX_PIECE {
            let mut at = start + MAX_PIECE;
            while !text.is_char_boundary(at) {
                at -= 1;
            }
            cut.push((start, at));
            start = at;
        }
        cut.push((start, end));
    }
    cut
}

/// End of the run of `class` starting at `index`: a word or a run of signs,
/// or a single digit.
fn run(chars: &[(usize, char)], index: usize, class: Class) -> usize {
    let mut next = index + 1;
    if class == Class::Digit {
        return next;
    }
    while next < chars.len() && self::class(chars[next].1) == class {
        next += 1;
    }
    next
}

/// The learned merges and the bytes of every token.
pub(crate) struct Subwords {
    /// Token made of each pair, with its rank (lower merges first).
    ranks: FxHashMap<(u16, u16), u16>,
    merges: Vec<(u16, u16)>,
    spellings: Vec<Box<[u8]>>,
}

impl Subwords {
    pub(crate) fn new(merges: Vec<(u16, u16)>) -> Option<Self> {
        let mut spellings: Vec<Box<[u8]>> = (0..=255u8).map(|byte| Box::from([byte])).collect();
        let mut ranks = FxHashMap::default();
        for (rank, &(a, b)) in merges.iter().enumerate() {
            let (a_bytes, b_bytes) = (spellings.get(a as usize)?, spellings.get(b as usize)?);
            let joined: Box<[u8]> = [&a_bytes[..], &b_bytes[..]].concat().into();
            spellings.push(joined);
            ranks.insert((a, b), rank as u16);
        }
        // The last token, after the merges, starts a document.
        (spellings.len() < u16::MAX as usize).then_some(Self {
            ranks,
            merges,
            spellings,
        })
    }

    /// Number of tokens, the document start included.
    pub(crate) fn len(&self) -> usize {
        self.spellings.len() + 1
    }

    /// Index of the document-start token among the model's outputs.
    pub(crate) fn document(&self) -> u16 {
        self.spellings.len() as u16
    }

    /// The bytes of a token; nothing for the document start.
    pub(crate) fn bytes(&self, token: u16) -> &[u8] {
        self.spellings
            .get(token as usize)
            .map_or(&[], |bytes| bytes)
    }

    /// Tokens spelling one piece.
    fn spell(&self, piece: &[u8], out: &mut Vec<u16>) {
        let mut symbols: Vec<u16> = piece.iter().map(|&byte| u16::from(byte)).collect();
        while symbols.len() > 1 {
            let best = symbols
                .windows(2)
                .enumerate()
                .filter_map(|(at, pair)| Some((*self.ranks.get(&(pair[0], pair[1]))?, at)))
                .min();
            let Some((rank, _)) = best else { break };
            let (a, b) = self.merges[rank as usize];
            let token = 256 + rank;
            let mut merged = Vec::with_capacity(symbols.len());
            let mut at = 0;
            while at < symbols.len() {
                if at + 1 < symbols.len() && symbols[at] == a && symbols[at + 1] == b {
                    merged.push(token);
                    at += 2;
                } else {
                    merged.push(symbols[at]);
                    at += 1;
                }
            }
            symbols = merged;
        }
        out.extend(symbols);
    }

    /// Append tokens and their byte offsets, returning the start of the last piece.
    pub(crate) fn encode(
        &self,
        text: &str,
        cache: &mut Cache,
        tokens: &mut Vec<u16>,
        starts: &mut Vec<u32>,
    ) -> usize {
        let mut last = 0;
        for (start, end) in pieces(text) {
            last = start;
            let piece = &text.as_bytes()[start..end];
            let spelled = match cache.0.get(piece) {
                Some(spelled) => spelled,
                None => {
                    if cache.0.len() > 200_000 {
                        cache.0.clear();
                    }
                    let mut spelled = Vec::new();
                    self.spell(piece, &mut spelled);
                    cache.0.entry(piece.into()).or_insert(spelled.into())
                }
            };
            let mut at = start;
            for &token in spelled.iter() {
                tokens.push(token);
                starts.push(at as u32);
                at += self.spellings[token as usize].len();
            }
        }
        last
    }

    /// The merges as stored in a model file: their count, then each pair.
    #[allow(dead_code)] // Used by the replay benchmark's export.
    pub(crate) fn encode_merges(&self) -> Vec<u8> {
        let mut bytes = (self.merges.len() as u32).to_le_bytes().to_vec();
        for &(a, b) in &self.merges {
            bytes.extend(a.to_le_bytes());
            bytes.extend(b.to_le_bytes());
        }
        bytes
    }

    pub(crate) fn decode_merges(bytes: &mut &[u8]) -> Option<Self> {
        let count = u32::from_le_bytes(bytes.get(..4)?.try_into().ok()?) as usize;
        let body = bytes.get(4..4 + count * 4)?;
        let merges = body
            .as_chunks::<4>()
            .0
            .iter()
            .map(|pair| {
                (
                    u16::from_le_bytes([pair[0], pair[1]]),
                    u16::from_le_bytes([pair[2], pair[3]]),
                )
            })
            .collect();
        *bytes = &bytes[4 + count * 4..];
        Self::new(merges)
    }
}

/// Spellings of pieces already met.
#[derive(Default)]
pub(crate) struct Cache(FxHashMap<Box<[u8]>, Box<[u16]>>);

/// Learn `count` merges from pieces and how often each occurs: each time,
/// the pair of tokens standing together most often becomes a new token.
#[allow(dead_code)] // Used by the replay benchmark's export.
pub(crate) fn learn(pieces: &FxHashMap<Box<[u8]>, u64>, count: usize) -> Vec<(u16, u16)> {
    use std::collections::BinaryHeap;
    let mut words: Vec<Vec<u16>> = Vec::with_capacity(pieces.len());
    let mut uses: Vec<i64> = Vec::with_capacity(pieces.len());
    let mut sorted: Vec<(&Box<[u8]>, &u64)> = pieces.iter().collect();
    sorted.sort_unstable();
    for (piece, &times) in sorted {
        if piece.len() > 1 {
            words.push(piece.iter().map(|&byte| u16::from(byte)).collect());
            uses.push(times as i64);
        }
    }
    let mut pairs: FxHashMap<(u16, u16), i64> = FxHashMap::default();
    let mut holders: FxHashMap<(u16, u16), Vec<u32>> = FxHashMap::default();
    for (index, word) in words.iter().enumerate() {
        for pair in word.windows(2) {
            *pairs.entry((pair[0], pair[1])).or_default() += uses[index];
            holders
                .entry((pair[0], pair[1]))
                .or_default()
                .push(index as u32);
        }
    }
    // Most frequent first; ties go to the smaller pair, so that learning is
    // the same on every run.
    let mut heap: BinaryHeap<(i64, std::cmp::Reverse<(u16, u16)>)> = pairs
        .iter()
        .map(|(&pair, &times)| (times, std::cmp::Reverse(pair)))
        .collect();
    let mut merges = Vec::with_capacity(count);
    while merges.len() < count {
        let Some((times, std::cmp::Reverse(pair))) = heap.pop() else {
            break;
        };
        let current = pairs.get(&pair).copied().unwrap_or(0);
        if current != times {
            if current > 0 {
                heap.push((current, std::cmp::Reverse(pair)));
            }
            continue;
        }
        if times < 2 {
            break;
        }
        let token = (256 + merges.len()) as u16;
        merges.push(pair);
        let mut held = holders.remove(&pair).unwrap_or_default();
        held.sort_unstable();
        held.dedup();
        let mut changed: Vec<(u16, u16)> = Vec::new();
        for index in held {
            let word = &mut words[index as usize];
            if !word.windows(2).any(|other| (other[0], other[1]) == pair) {
                continue;
            }
            let weight = uses[index as usize];
            for other in word.windows(2) {
                let key = (other[0], other[1]);
                *pairs.entry(key).or_default() -= weight;
                changed.push(key);
            }
            let mut merged = Vec::with_capacity(word.len());
            let mut at = 0;
            while at < word.len() {
                if at + 1 < word.len() && (word[at], word[at + 1]) == pair {
                    merged.push(token);
                    at += 2;
                } else {
                    merged.push(word[at]);
                    at += 1;
                }
            }
            *word = merged;
            for other in word.windows(2) {
                let key = (other[0], other[1]);
                *pairs.entry(key).or_default() += weight;
                changed.push(key);
                if key.0 == token || key.1 == token {
                    holders.entry(key).or_default().push(index);
                }
            }
        }
        pairs.remove(&pair);
        changed.sort_unstable();
        changed.dedup();
        for key in changed {
            if let Some(&times) = pairs.get(&key)
                && times > 0
                && key != pair
            {
                heap.push((times, std::cmp::Reverse(key)));
            }
        }
    }
    merges
}

#[cfg(test)]
#[path = "vtcomplete_inline_subwords_tests.rs"]
mod tests;
