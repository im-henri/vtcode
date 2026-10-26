//! Bounded occurrence cache. Identifiers are parameters, punctuation is literal.
//! No syntax names, extensions, paths, or repository relationships enter this model.
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

const WORD: u32 = 1 << 31;
const MAX_TOKENS: usize = 2048;
const MAX_WORDS: usize = 1024;
const MAX_WORD_BYTES: usize = 160;
const LENGTHS: [usize; 5] = [2, 4, 8, 16, 32];
const MAX_COMPARISONS: usize = 24_576;
const MAX_OCCURRENCES: usize = 384;
const MAX_HASH_TOKENS: usize = 131_072;
const MAX_LOOKUPS: usize = 8192;
const RIGHT: usize = 8;

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    #[default]
    Full,
    LiteralOnly,
    NoRightContext,
    NoDeduplicate,
    NoStatistical,
    RawWorkspacePrior,
    Disabled,
    NoExtension,
    NoLiteralBackoff,
    FixedChannelOrder,
    MixtureScale192,
    Phase1,
    UniformConcentration,
    UnconditionalFilePrior,
    NoActiveFileOccurrences,
}
#[cfg(test)]
thread_local! {
    static MODE: std::cell::Cell<Mode> = const { std::cell::Cell::new(Mode::Full) };
}
#[cfg(test)]
pub(crate) fn with_mode<T>(mode: Mode, action: impl FnOnce() -> T) -> T {
    struct Reset(Mode);
    impl Drop for Reset {
        fn drop(&mut self) {
            MODE.with(|current| current.set(self.0));
        }
    }
    let _reset = Reset(MODE.with(|current| current.replace(mode)));
    action()
}
#[cfg(test)]
pub(crate) fn raw_workspace_prior() -> bool {
    MODE.with(|mode| mode.get() == Mode::RawWorkspacePrior)
}
pub(crate) fn likelihood_scale() -> f64 {
    #[cfg(test)]
    if MODE.with(|mode| matches!(mode.get(), Mode::MixtureScale192 | Mode::Phase1)) {
        return 192.;
    }
    32.
}
pub(crate) fn file_prior_without_context() -> bool {
    #[cfg(test)]
    if MODE.with(|mode| matches!(mode.get(), Mode::UnconditionalFilePrior | Mode::Phase1)) {
        return true;
    }
    false
}
fn specificity_order() -> bool {
    #[cfg(test)]
    if MODE.with(|mode| matches!(mode.get(), Mode::FixedChannelOrder | Mode::Phase1)) {
        return false;
    }
    true
}
fn full_window_mode() -> bool {
    #[cfg(test)]
    if MODE.with(|mode| matches!(mode.get(), Mode::NoExtension | Mode::Phase1)) {
        return false;
    }
    true
}
fn literal_backoff_mode() -> bool {
    #[cfg(test)]
    if MODE.with(|mode| matches!(mode.get(), Mode::NoLiteralBackoff | Mode::Phase1)) {
        return false;
    }
    true
}
fn grounded_active_file() -> bool {
    #[cfg(test)]
    if MODE.with(|mode| mode.get() == Mode::NoActiveFileOccurrences) {
        return false;
    }
    true
}

fn target_concentration(length: usize) -> f64 {
    #[cfg(test)]
    if MODE.with(|mode| matches!(mode.get(), Mode::UniformConcentration | Mode::Phase1)) {
        return 2.;
    }
    // A long, bijectively verified context supplies substantially more
    // constraints than a short co-occurrence. Preserve conservative smoothing
    // for short matches while allowing those longer witnesses to refine a
    // strong current-file unigram prior.
    if length >= 16 { 0.5 } else { 2. }
}

/// A type cache is the noninformative workspace base: a known spelling gets
/// one vote, irrespective of how many generated copies contain it. Repetition
/// remains useful in the local and retrieved conditional distributions.
pub(crate) fn workspace_count(frequency: u32) -> f64 {
    #[cfg(test)]
    if raw_workspace_prior() {
        return f64::from(frequency.min(16)) + 1.;
    }
    f64::from(frequency.min(1)) + 1.
}

#[derive(Clone, Default, Serialize)]
pub(crate) struct Data {
    words: Vec<String>,
    tokens: Vec<u32>,
}

// Validate cache collection limits while decoding, before an untrusted array
// can grow an allocation that would later be rejected by Data::valid.
impl<'de> Deserialize<'de> for Data {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct CachedData {
            #[serde(deserialize_with = "deserialize_example_words")]
            words: Vec<String>,
            #[serde(deserialize_with = "deserialize_example_tokens")]
            tokens: Vec<u32>,
        }
        let data = CachedData::deserialize(deserializer)?;
        Ok(Self {
            words: data.words,
            tokens: data.tokens,
        })
    }
}

struct CachedWord(String);
impl<'de> Deserialize<'de> for CachedWord {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct WordVisitor;
        impl serde::de::Visitor<'_> for WordVisitor {
            type Value = CachedWord;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(
                    formatter,
                    "an identifier of at most {MAX_WORD_BYTES} UTF-8 bytes"
                )
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                if value.len() > MAX_WORD_BYTES {
                    return Err(E::custom(
                        "completion example identifier exceeds 160 UTF-8 bytes",
                    ));
                }
                Ok(CachedWord(value.to_owned()))
            }
            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                if value.len() > MAX_WORD_BYTES {
                    return Err(E::custom(
                        "completion example identifier exceeds 160 UTF-8 bytes",
                    ));
                }
                Ok(CachedWord(value))
            }
        }
        deserializer.deserialize_str(WordVisitor)
    }
}

fn deserialize_example_sequence<'de, D, T, const LIMIT: usize>(
    deserializer: D,
    message: &'static str,
) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct SequenceVisitor<T, const LIMIT: usize> {
        message: &'static str,
        marker: std::marker::PhantomData<T>,
    }
    impl<'de, T: Deserialize<'de>, const LIMIT: usize> serde::de::Visitor<'de>
        for SequenceVisitor<T, LIMIT>
    {
        type Value = Vec<T>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(formatter, "a sequence of at most {LIMIT} elements")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut sequence: A,
        ) -> Result<Vec<T>, A::Error> {
            use serde::de::Error;
            let hint = sequence.size_hint().unwrap_or(0);
            if hint > LIMIT {
                return Err(A::Error::custom(self.message));
            }
            let mut values = Vec::with_capacity(hint);
            while values.len() < LIMIT {
                let Some(value) = sequence.next_element()? else {
                    return Ok(values);
                };
                values.push(value);
            }
            if sequence.next_element::<T>()?.is_some() {
                return Err(A::Error::custom(self.message));
            }
            Ok(values)
        }
    }
    deserializer.deserialize_seq(SequenceVisitor::<T, LIMIT> {
        message,
        marker: std::marker::PhantomData,
    })
}

fn deserialize_example_words<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<String>, D::Error> {
    deserialize_example_sequence::<D, CachedWord, MAX_WORDS>(
        deserializer,
        "completion examples words exceed 1024 entries",
    )
    .map(|words| words.into_iter().map(|word| word.0).collect())
}

fn deserialize_example_tokens<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<u32>, D::Error> {
    deserialize_example_sequence::<D, u32, MAX_TOKENS>(
        deserializer,
        "completion examples tokens exceed 2048 entries",
    )
}

#[cfg(test)]
#[path = "completion_examples_cache_tests.rs"]
mod cache_tests;

impl Data {
    pub(crate) fn tokens(&self) -> usize {
        self.tokens.len()
    }

    /// Release richer sequence evidence under workspace memory pressure while
    /// retaining a literal/parameterized seed for this file. No source reread.
    pub(crate) fn compact(&mut self) {
        let keep = (self.tokens.len() / 4).max(8).min(self.tokens.len());
        self.truncate_to(keep);
    }

    pub(crate) fn truncate_to(&mut self, keep: usize) {
        if self.tokens.len() <= keep {
            return;
        }
        self.tokens.truncate(keep);
        // A prefix keeps its existing token IDs. Release the unused suffix of
        // the dictionary directly, without cloning or renumbering spellings.
        let words = self
            .tokens
            .iter()
            .filter(|token| **token & WORD != 0)
            .map(|token| (*token & !WORD) as usize + 1)
            .max()
            .unwrap_or(0);
        self.words.truncate(words);
        self.tokens.shrink_to_fit();
        self.words.shrink_to_fit();
    }

    pub(crate) fn memory(&self) -> usize {
        64 + self.tokens.capacity() * 4
            + self.words.capacity() * 24
            + self.words.iter().map(|s| s.capacity() + 16).sum::<usize>()
    }
    pub(crate) fn valid(&self) -> bool {
        self.tokens.len() <= MAX_TOKENS
            && self.words.len() <= MAX_WORDS
            && self.words.iter().all(|w| w.len() <= MAX_WORD_BYTES)
            && self.tokens.iter().all(|&t| {
                if t & WORD != 0 {
                    (t & !WORD) < self.words.len() as u32
                } else {
                    char::from_u32(t).is_some()
                }
            })
    }
    fn word(&self, t: u32) -> Option<&str> {
        (t & WORD != 0).then(|| self.words[(t & !WORD) as usize].as_str())
    }
}

pub(crate) struct Builder {
    limit: usize,
    data: Data,
    ids: FxHashMap<String, u32>,
    pending: String,
    oversized: bool,
}
impl Default for Builder {
    fn default() -> Self {
        Self::bounded(MAX_TOKENS)
    }
}
impl Builder {
    pub(crate) fn bounded(limit: usize) -> Self {
        Self {
            limit: limit.min(MAX_TOKENS),
            data: Data::default(),
            ids: FxHashMap::default(),
            pending: String::new(),
            oversized: false,
        }
    }
    #[cfg(test)]
    fn working_memory(&self) -> usize {
        self.data.memory()
            + self.ids.capacity() * 64
            + self
                .ids
                .keys()
                .map(|word| word.capacity() + 16)
                .sum::<usize>()
            + self.pending.capacity()
            + 128
    }
    pub(crate) fn feed(&mut self, text: &str) {
        if self.data.tokens.len() >= self.limit {
            return;
        }
        for ch in text.chars() {
            if ch.is_alphanumeric() || ch == '_' || ch == '$' {
                if self.pending.len() + ch.len_utf8() <= MAX_WORD_BYTES {
                    self.pending.push(ch);
                } else {
                    self.oversized = true;
                }
            } else {
                self.flush();
                if self.data.tokens.len() < self.limit && (!ch.is_whitespace() || ch == '\n') {
                    self.data.tokens.push(ch as u32);
                }
            }
            if self.data.tokens.len() >= self.limit {
                break;
            }
        }
    }
    fn flush(&mut self) {
        if !self.pending.is_empty() && !self.oversized && self.data.tokens.len() < self.limit {
            let id = if let Some(&id) = self.ids.get(&self.pending) {
                Some(id)
            } else if self.data.words.len() < MAX_WORDS {
                let id = WORD | self.data.words.len() as u32;
                self.data.words.push(self.pending.clone());
                self.ids.insert(self.pending.clone(), id);
                Some(id)
            } else {
                None
            };
            if let Some(id) = id {
                self.data.tokens.push(id);
            } else {
                self.data.tokens.push(0);
            } // a barrier, never join separated text
        } else if self.oversized && self.data.tokens.len() < self.limit {
            self.data.tokens.push(0);
        }
        self.pending.clear();
        self.oversized = false;
    }
    pub(crate) fn finish(mut self) -> Data {
        self.flush();
        self.data
    }
}

fn parse(text: &str) -> Data {
    let mut builder = Builder::default();
    builder.feed(text);
    builder.finish()
}

fn matching_role(
    source: &Data,
    source_at: Option<usize>,
    query: &Data,
    query_at: Option<usize>,
    forward: &FxHashMap<u32, u32>,
    reverse: &FxHashMap<u32, u32>,
) -> bool {
    match (
        source_at.and_then(|at| source.tokens.get(at)).copied(),
        query_at.and_then(|at| query.tokens.get(at)).copied(),
    ) {
        (None, None) => true,
        (Some(a), Some(b)) => match (source.word(a), query.word(b)) {
            (Some(aw), Some(bw)) => {
                forward.get(&a).map_or(aw == bw, |&mapped| mapped == b)
                    && reverse.get(&b).is_none_or(|&original| original == a)
            }
            (None, None) => a == b && a != 0,
            _ => false,
        },
        _ => false,
    }
}

// Canonical first-occurrence IDs are equivalent to window-relative predecessor
// encoding. Window-local IDs matter: slicing an encoding built for a whole file
// would retain invalid predecessor distances across the window boundary.
fn signature(data: &Data, start: usize, end: usize, parameter: bool) -> u64 {
    let mut hash = FxHasher::default();
    let mut names = [0u32; 72];
    let mut count = 0;
    for &token in &data.tokens[start..end] {
        if let Some(word) = data.word(token) {
            if parameter {
                let id = names[..count]
                    .iter()
                    .position(|&t| t == token)
                    .unwrap_or_else(|| {
                        names[count] = token;
                        count += 1;
                        count - 1
                    });
                (WORD | id as u32).hash(&mut hash);
            } else {
                WORD.hash(&mut hash);
                word.hash(&mut hash);
            }
        } else {
            token.hash(&mut hash);
        }
    }
    hash.finish()
}

type Key = (u8, u64);
fn keys(data: &Data, mut emit: impl FnMut(Key, u64, u16)) {
    for at in 0..data.tokens.len() {
        let Some(_) = data.word(data.tokens[at]) else {
            continue;
        };
        let begin = at.saturating_sub(32);
        // A retrieval representative must preserve literal anchors and suffix
        // words. Renamed copies may differ in compatibility with this query;
        // canonical confidence deduplication happens after verification instead.
        let fingerprint = signature(data, begin, (at + RIGHT + 1).min(data.tokens.len()), false);
        for n in LENGTHS.into_iter().filter(|&n| n <= at) {
            emit(
                (n as u8, signature(data, at - n, at, false)),
                fingerprint,
                at as u16,
            );
            if n >= 4 {
                emit(
                    (n as u8 | 128, signature(data, at - n, at, true)),
                    fingerprint,
                    at as u16,
                );
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct Index {
    files: BTreeMap<u64, Arc<Data>>,
    postings: BTreeMap<Key, Vec<Location>>,
    bytes: usize,
}

// One contiguous, sorted posting list per retrieval key. The previous three
// nested hash tables allocated two extra tables for almost every occurrence.
#[derive(Clone, Copy)]
struct Location {
    fingerprint: u64,
    file: u64,
    at: u16,
}

fn posting_keys(data: &Data) -> Vec<(Key, u64, u16)> {
    let mut result = Vec::with_capacity(data.tokens.len() * 9);
    keys(data, |key, fingerprint, at| {
        result.push((key, fingerprint, at))
    });
    // Keep the last occurrence of the same literal window in each file,
    // exactly as the previous map did. Local repeat weight is verified later.
    result.sort_unstable_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)).then_with(|| b.2.cmp(&a.2)));
    result.dedup_by_key(|entry| (entry.0, entry.1));
    result
}

impl Index {
    pub(crate) fn memory(&self) -> usize {
        // Conservative tree-node charges, including minimum node occupancy
        // and root slack. Unlike HashMap::capacity, these cannot hide allocated
        // buckets behind tombstones during repeated file replacement.
        self.bytes
            + self.files.len() * 48
            + usize::from(!self.files.is_empty()) * 256
            + self.postings.len() * 128
            + usize::from(!self.postings.is_empty()) * 512
    }
    #[cfg(test)]
    pub(crate) fn retained_memory(&self) -> usize {
        self.memory() + self.files.values().map(|data| data.memory()).sum::<usize>()
    }
    pub(crate) fn additional_memory(&self, data: &Data) -> usize {
        let entries = posting_keys(data);
        let mut new_keys = 0;
        let mut growth = 0;
        for batch in entries.chunk_by(|a, b| a.0 == b.0) {
            let existing = self.postings.get(&batch[0].0);
            new_keys += usize::from(existing.is_none());
            let (len, capacity) = existing.map_or((0, 0), |list| (list.len(), list.capacity()));
            growth +=
                (len + batch.len()).saturating_sub(capacity) * std::mem::size_of::<Location>();
        }
        growth
            + new_keys * 128
            + usize::from(self.postings.is_empty() && new_keys > 0) * 512
            + 48
            + usize::from(self.files.is_empty()) * 256
    }
    pub(crate) fn insert(&mut self, file: u64, data: Arc<Data>) {
        self.remove(file);
        let entries = posting_keys(&data);
        for batch in entries.chunk_by(|a, b| a.0 == b.0) {
            let list = self.postings.entry(batch[0].0).or_default();
            let previous = list.capacity();
            list.reserve_exact(batch.len());
            self.bytes += (list.capacity() - previous) * std::mem::size_of::<Location>();
            for &(_, fingerprint, at) in batch {
                let position =
                    list.binary_search_by_key(&(fingerprint, file), |p| (p.fingerprint, p.file));
                match position {
                    Ok(position) => list[position].at = at,
                    Err(position) => list.insert(
                        position,
                        Location {
                            fingerprint,
                            file,
                            at,
                        },
                    ),
                }
            }
        }
        self.files.insert(file, data);
    }
    pub(crate) fn remove(&mut self, file: u64) -> usize {
        let before = self.memory();
        if let Some(data) = self.files.remove(&file) {
            let entries = posting_keys(&data);
            for batch in entries.chunk_by(|a, b| a.0 == b.0) {
                let key = batch[0].0;
                if let Some(list) = self.postings.get_mut(&key) {
                    let previous = list.capacity();
                    list.retain(|location| location.file != file);
                    if list.is_empty() {
                        self.postings.remove(&key);
                        self.bytes -= previous * std::mem::size_of::<Location>();
                    } else {
                        if list.len().saturating_mul(2) < list.capacity() {
                            list.shrink_to_fit();
                        }
                        self.bytes -=
                            (previous - list.capacity()) * std::mem::size_of::<Location>();
                    }
                }
            }
        }
        before - self.memory()
    }
    pub(crate) fn query(&self, query: &Query, active_file: u64, evidence: &mut Evidence) {
        if !query.enabled {
            return;
        }
        let extend = query.full_window;
        for &n in LENGTHS.iter().rev() {
            if n > query.left.tokens.len() {
                continue;
            }
            for parameter in [false, true] {
                if parameter && (!query.parameter || n < 4) {
                    continue;
                }
                let pool = usize::from(!parameter);
                if n < evidence.pools[pool].length && !extend {
                    continue;
                }
                if !evidence.charge_hash(n) {
                    return;
                }
                let key = (
                    n as u8 | if parameter { 128 } else { 0 },
                    signature(
                        &query.left,
                        query.left.tokens.len() - n,
                        query.left.tokens.len(),
                        parameter,
                    ),
                );
                let Some(locations) = self.postings.get(&key) else {
                    continue;
                };
                let mut position = 0;
                while position < locations.len() {
                    evidence.lookups += 1;
                    if evidence.exhausted() {
                        return;
                    }
                    let end = position
                        + locations[position..]
                            .partition_point(|p| p.fingerprint == locations[position].fingerprint);
                    let group = &locations[position..end];
                    for &Location { file, at, .. } in
                        group.iter().filter(|p| p.file != active_file).chain(
                            group
                                .iter()
                                .filter(|p| p.file == active_file && grounded_active_file()),
                        )
                    {
                        if let Some(data) = self.files.get(&file) {
                            evidence.observe(
                                query,
                                data,
                                at as usize,
                                n,
                                parameter,
                                pool,
                                file,
                                file == active_file,
                            );
                        }
                        if evidence.exhausted() {
                            return;
                        }
                        if query.deduplicate {
                            break;
                        }
                    }
                    position = end;
                }
            }
        }
    }
}

pub(crate) struct Query {
    left: Data,
    right: Data,
    pub(crate) parameter: bool,
    pub(crate) suffix: bool,
    pub(crate) deduplicate: bool,
    enabled: bool,
    full_window: bool,
}
impl Query {
    pub(crate) fn new(left: &str, right: &str) -> Self {
        // Caller supplies a bounded tail. Keep the last 64 tokens, not the first.
        let left_start = left
            .char_indices()
            .rev()
            .nth(MAX_TOKENS - 1)
            .map_or(0, |(at, _)| at);
        let mut left = parse(&left[left_start..]);
        if left.tokens.len() > 64 {
            left.tokens.drain(..left.tokens.len() - 64);
        }
        let right_end = right
            .char_indices()
            .nth(MAX_TOKENS)
            .map_or(right.len(), |(at, _)| at);
        let mut right = parse(&right[..right_end]);
        right.tokens.truncate(RIGHT);
        let query = Self {
            left,
            right,
            parameter: true,
            suffix: true,
            deduplicate: true,
            enabled: true,
            full_window: full_window_mode(),
        };
        #[cfg(test)]
        let query = MODE.with(|mode| {
            let mut query = query;
            query.parameter = mode.get() != Mode::LiteralOnly;
            query.suffix = mode.get() != Mode::NoRightContext;
            query.deduplicate = mode.get() != Mode::NoDeduplicate;
            query.enabled = mode.get() != Mode::Disabled;
            query
        });
        query
    }
}

#[derive(Default)]
struct Pool {
    counts: FxHashMap<String, f64>,
    calls: FxHashMap<String, f64>,
    total: f64,
    length: usize,
}
#[derive(Default)]
pub(crate) struct Evidence {
    // Within each origin the longer verified context updates the shorter one;
    // less local origins supply the backoff estimate. Keep both literal and
    // renamed contexts so a more specific relationship can refine an exact seed.
    pools: [Pool; 6], // retrieved parameter/literal, file parameter/literal, recent parameter/literal
    // Retain one shorter compatible prior only when the longest context has
    // no suffix-compatible target. Negative long witnesses still downweight it.
    backoff: [Pool; 6],
    seen: FxHashSet<(usize, usize, u64)>,
    retrieved_weights: FxHashMap<(usize, usize, u64), u8>,
    comparisons: usize,
    occurrences: usize,
    hash_tokens: usize,
    lookups: usize,
    local_sources: u64,
    pub(crate) limited: bool,
}
impl Evidence {
    fn backoff_active(&self, pool: usize) -> bool {
        self.pools[pool].counts.is_empty()
            && self.backoff[pool].total > 0.
            && self.backoff[pool].length < self.pools[pool].length
    }
    fn effective_pool(&self, pool: usize) -> &Pool {
        if self.backoff_active(pool) {
            &self.backoff[pool]
        } else {
            &self.pools[pool]
        }
    }
    fn retain_depths(&mut self, pool: usize) {
        let longest = self.pools[pool].length;
        let shorter = self.backoff[pool].length;
        self.seen
            .retain(|&(p, length, _)| p != pool || length == longest || length == shorter);
        self.retrieved_weights
            .retain(|&(p, length, _), _| p != pool || length == longest || length == shorter);
    }
    fn exhausted(&mut self) -> bool {
        self.limited |= self.comparisons >= MAX_COMPARISONS
            || self.occurrences >= MAX_OCCURRENCES
            || self.hash_tokens >= MAX_HASH_TOKENS
            || self.lookups >= MAX_LOOKUPS;
        self.limited
    }
    fn charge_hash(&mut self, tokens: usize) -> bool {
        if tokens > MAX_HASH_TOKENS.saturating_sub(self.hash_tokens) {
            self.limited = true;
            return false;
        }
        self.hash_tokens += tokens;
        true
    }
    fn local_repeat_weight(&mut self, data: &Data, at: usize, n: usize, right: usize) -> u8 {
        let target = data.tokens[at];
        let call = data.tokens.get(at + 1) == Some(&('(' as u32));
        let suffix = &data.tokens[at + 1..(at + 1 + right).min(data.tokens.len())];
        for prior in (at.saturating_sub(32).max(n)..at).rev() {
            self.comparisons += 1;
            if self.exhausted() {
                return 1;
            }
            if data.tokens[prior] != target
                || (data.tokens.get(prior + 1) == Some(&('(' as u32))) != call
            {
                continue;
            }
            let prior_suffix = &data.tokens[prior + 1..(prior + 1 + right).min(data.tokens.len())];
            if prior_suffix.len() != suffix.len() {
                continue;
            }
            let cost = n + suffix.len();
            if cost > MAX_COMPARISONS.saturating_sub(self.comparisons) {
                self.limited = true;
                return 1;
            }
            self.comparisons += cost;
            // Only repeated occurrences of the exact same context count as
            // local repetition. A preceding declaration/binding is not a use.
            if data.tokens[prior - n..prior] == data.tokens[at - n..at] && prior_suffix == suffix {
                return 2;
            }
        }
        1
    }
    pub(crate) fn candidates(&self) -> impl Iterator<Item = &str> {
        (0..self.pools.len())
            .flat_map(|pool| self.effective_pool(pool).counts.keys().map(String::as_str))
    }
    #[cfg(test)]
    pub(crate) fn mass(&self) -> f64 {
        self.pools
            .iter()
            .enumerate()
            .map(|(i, p)| {
                p.total
                    + if self.backoff_active(i) {
                        self.backoff[i].total
                    } else {
                        0.
                    }
            })
            .sum()
    }
    /// Dirichlet backoff: no evidence is an exact identity. Contradictions stay
    /// in each denominator instead of inflating the surviving candidate mass.
    pub(crate) fn probability(&self, word: &str, workspace: f64) -> f64 {
        #[cfg(test)]
        if MODE.with(|mode| mode.get() == Mode::NoStatistical) {
            return workspace;
        }
        let update_pool = |p, pool: &Pool| {
            let alpha = target_concentration(pool.length);
            (pool.counts.get(word).copied().unwrap_or(0.) + alpha * p) / (pool.total + alpha)
        };
        let update = |p, slot| {
            let prior = if self.backoff_active(slot) {
                update_pool(p, &self.backoff[slot])
            } else {
                p
            };
            update_pool(prior, &self.pools[slot])
        };
        if specificity_order() {
            return (0..self.pools.len())
                .step_by(2)
                .fold(workspace, |prior, slot| {
                    if self.pools[slot].length > self.pools[slot + 1].length {
                        update(update(prior, slot + 1), slot)
                    } else {
                        update(update(prior, slot), slot + 1)
                    }
                });
        }
        (0..self.pools.len()).fold(workspace, update)
    }
    pub(crate) fn call_probability(&self, word: &str) -> Option<f64> {
        let mut supported = false;
        let probability = (0..self.pools.len()).fold(0.5, |prior, slot| {
            let pool = self.effective_pool(slot);
            let uses = pool.counts.get(word).copied().unwrap_or(0.);
            supported |= uses > 0.;
            (pool.calls.get(word).copied().unwrap_or(0.) + 2. * prior) / (uses + 2.)
        });
        supported.then_some(probability)
    }
    pub(crate) fn callable(&self, word: &str) -> Option<bool> {
        let probability = self.call_probability(word)?;
        // Parameter and literal channels can describe the same occurrence.
        // Count their maximum within each origin rather than inventing two
        // independent call observations from a single use.
        let calls: f64 = (0..self.pools.len())
            .step_by(2)
            .map(|slot| {
                [self.effective_pool(slot), self.effective_pool(slot + 1)]
                    .into_iter()
                    .map(|pool| pool.calls.get(word).copied().unwrap_or(0.))
                    .fold(0., f64::max)
            })
            .sum();
        Some(calls >= 2. && probability >= 2. / 3.)
    }
    pub(crate) fn collect(&mut self, query: &Query, text: &str, recent: bool) {
        if !query.enabled {
            return;
        }
        let extend = query.full_window;
        let data = parse(text);
        self.local_sources += 1;
        let source = self.local_sources;
        let base = if recent { 4 } else { 2 };
        // Prefer long occurrences first; a short context must not consume the
        // work allowance before an available relational example is considered.
        for &n in LENGTHS.iter().rev() {
            if n > query.left.tokens.len() {
                continue;
            }
            let mut query_signatures = [None; 2];
            for (slot, parameter) in [false, true].into_iter().enumerate() {
                if parameter && (!query.parameter || n < 4) {
                    continue;
                }
                let pool = base + usize::from(!parameter);
                if n < self.pools[pool].length && !extend {
                    continue;
                }
                if !self.charge_hash(n) {
                    return;
                }
                query_signatures[slot] = Some(signature(
                    &query.left,
                    query.left.tokens.len() - n,
                    query.left.tokens.len(),
                    parameter,
                ));
            }
            if query_signatures.iter().all(Option::is_none) {
                continue;
            }
            for at in (n..data.tokens.len()).rev() {
                if data.word(data.tokens[at]).is_none() {
                    continue;
                }
                self.lookups += 1;
                if self.exhausted() {
                    return;
                }
                for (slot, parameter) in [false, true].into_iter().enumerate() {
                    let Some(query_signature) = query_signatures[slot] else {
                        continue;
                    };
                    if !self.charge_hash(n) {
                        return;
                    }
                    if signature(&data, at - n, at, parameter) == query_signature {
                        let pool = base + usize::from(!parameter);
                        self.observe(query, &data, at, n, parameter, pool, source, false);
                    }
                }
            }
        }
    }
    /// A target's first occurrence can be at the caret even when a later use
    /// already names it. Bind that repetition from the visible suffix using
    /// the same injective map as the left context. A fresh call operation is
    /// an anchor, not a parameter that can change merely to fit the example.
    fn right_binding<'a>(
        &mut self,
        query: &'a Query,
        data: &Data,
        at: usize,
        forward: &FxHashMap<u32, u32>,
        reverse: &FxHashMap<u32, u32>,
    ) -> Option<&'a str> {
        if data.tokens.len().saturating_sub(at + 1) < query.right.tokens.len() {
            return None;
        }
        let target = data.tokens[at];
        let suffix = &data.tokens[at + 1..at + 1 + query.right.tokens.len()];
        self.comparisons += suffix.len();
        if self.exhausted() || !suffix.contains(&target) {
            return None;
        }
        let mut right_forward = FxHashMap::<u32, &str>::default();
        let mut right_reverse = FxHashMap::<&str, u32>::default();
        for (&a, &b) in forward {
            let name = query.left.word(b)?;
            right_forward.insert(a, name);
        }
        for (&b, &a) in reverse {
            right_reverse.insert(query.left.word(b)?, a);
        }
        let mut binding = None;
        for (offset, (&a, &b)) in data.tokens[at + 1..]
            .iter()
            .zip(&query.right.tokens)
            .enumerate()
        {
            self.comparisons += 1;
            if self.exhausted() {
                return None;
            }
            match (data.word(a), query.right.word(b)) {
                (Some(aw), Some(bw)) => {
                    if right_forward.get(&a).is_some_and(|&mapped| mapped != bw)
                        || right_reverse.get(bw).is_some_and(|&original| original != a)
                    {
                        return None;
                    }
                    if a != target
                        && !right_forward.contains_key(&a)
                        && data.tokens.get(at + offset + 2) == Some(&('(' as u32))
                        && aw != bw
                    {
                        return None;
                    }
                    right_forward.insert(a, bw);
                    right_reverse.insert(bw, a);
                    if a == target {
                        binding = Some(bw);
                    }
                }
                (None, None) if a == b && a != 0 => {}
                _ => return None,
            }
        }
        binding.filter(|word| Some(*word) != data.word(target))
    }
    fn observe(
        &mut self,
        query: &Query,
        data: &Data,
        at: usize,
        n: usize,
        parameter: bool,
        pool: usize,
        source: u64,
        grounded_only: bool,
    ) {
        if parameter {
            let width = if query.full_window {
                32.min(at).min(query.left.tokens.len())
            } else {
                n
            };
            if !self.charge_hash(width) {
                return;
            }
            // Parameterized evidence needs a literal identifier anchor. Check
            // this necessary condition before spending the mapping allowance;
            // punctuation-only similarities and wholly renamed clones cannot
            // starve a later, anchored relationship.
            let anchored = data.tokens[at - width..at]
                .iter()
                .zip(&query.left.tokens[query.left.tokens.len() - width..])
                .any(|(&a, &b)| matches!((data.word(a), query.left.word(b)), (Some(a), Some(b)) if a == b));
            if !anchored {
                return;
            }
        }
        self.occurrences += 1;
        let extend = query.full_window;
        if n < self.pools[pool].length && !extend || self.exhausted() {
            return;
        }
        let left = &query.left;
        let mut forward = FxHashMap::<u32, u32>::default();
        let mut reverse = FxHashMap::<u32, u32>::default();
        let mut literal_words = 0;
        for (&a, &b) in data.tokens[at - n..at]
            .iter()
            .zip(&left.tokens[left.tokens.len() - n..])
        {
            self.comparisons += 1;
            match (data.word(a), left.word(b)) {
                (Some(aw), Some(bw)) => {
                    literal_words += usize::from(aw == bw);
                    if !parameter && aw != bw {
                        return;
                    }
                    if forward.get(&a).is_some_and(|&old| old != b)
                        || reverse.get(&b).is_some_and(|&old| old != a)
                    {
                        return;
                    }
                    forward.insert(a, b);
                    reverse.insert(b, a);
                }
                (None, None) if a == b && a != 0 => {}
                _ => return,
            }
        }
        if extend && parameter {
            let source_start = at - n;
            let query_start = left.tokens.len() - n;
            let first = data.tokens[source_start];
            let query_first = left.tokens[query_start];
            if data.word(first).is_some() && data.word(first) != left.word(query_first) {
                // A renamed word at the seed's left edge has a missing role
                // constraint. A suffix such as `value\nlabel:` must not map a
                // labelled value to an unrelated standalone expression.
                let mut supported = false;
                for offset in 0..n {
                    self.comparisons += 1;
                    if self.exhausted() {
                        return;
                    }
                    if data.tokens[source_start + offset] != first {
                        continue;
                    }
                    self.comparisons += 2;
                    if matching_role(
                        data,
                        (source_start + offset).checked_sub(1),
                        left,
                        (query_start + offset).checked_sub(1),
                        &forward,
                        &reverse,
                    ) && matching_role(
                        data,
                        Some(source_start + offset + 1),
                        left,
                        Some(query_start + offset + 1),
                        &forward,
                        &reverse,
                    ) {
                        supported = true;
                        break;
                    }
                }
                if !supported {
                    return;
                }
            }
        }
        // A literal seed is already a verified witness. Optional parameterized
        // extension may fail later at the target as well as within the prefix;
        // retain the same exact checkpoint for both kinds of collision.
        let literal_seed = (extend && !parameter && literal_backoff_mode())
            .then(|| (forward.clone(), reverse.clone(), n, literal_words));
        let (mut parameter, mut pool, mut n) = if extend {
            let mut verified = n;
            while verified < 32 && verified < at && verified < left.tokens.len() {
                if self.exhausted() {
                    return;
                }
                let source_at = at - verified - 1;
                let query_at = left.tokens.len() - verified - 1;
                let a = data.tokens[source_at];
                let b = left.tokens[query_at];
                self.comparisons += 1;
                match (data.word(a), left.word(b)) {
                    (Some(aw), Some(bw)) => {
                        // A literal-only query never learns a rename through
                        // extension after its exact lookup seed.
                        if !query.parameter && aw != bw {
                            break;
                        }
                        if forward.get(&a).is_some_and(|&mapped| mapped != b)
                            || reverse.get(&b).is_some_and(|&original| original != a)
                        {
                            if let Some((initial_forward, initial_reverse, _, _)) = &literal_seed {
                                forward = initial_forward.clone();
                                reverse = initial_reverse.clone();
                                verified = n;
                                break;
                            } else {
                                return;
                            }
                        }
                        if !forward.contains_key(&a) {
                            self.comparisons += 2;
                            if !matching_role(
                                data,
                                source_at.checked_sub(1),
                                left,
                                query_at.checked_sub(1),
                                &forward,
                                &reverse,
                            ) || !matching_role(
                                data,
                                Some(source_at + 1),
                                left,
                                Some(query_at + 1),
                                &forward,
                                &reverse,
                            ) {
                                break;
                            }
                        }
                        literal_words += usize::from(aw == bw);
                        forward.insert(a, b);
                        reverse.insert(b, a);
                    }
                    (None, None) if a == b && a != 0 => {}
                    _ => break,
                }
                verified += 1;
            }
            let renamed = forward.iter().any(|(&a, &b)| data.word(a) != left.word(b));
            (renamed, (pool / 2) * 2 + usize::from(!renamed), verified)
        } else {
            (parameter, pool, n)
        };
        let target = data.tokens[at];
        let Some(literal) = data.word(target) else {
            return;
        };
        let mut bound = forward.get(&target).copied();
        if extend
            && bound.is_none()
            && let Some(query_target) = left.words.iter().position(|word| word == literal)
            && reverse
                .get(&(WORD | query_target as u32))
                .is_some_and(|&original| original != target)
        {
            if let Some((seed_forward, seed_reverse, seed_length, seed_literals)) = literal_seed
                && n > seed_length
            {
                forward = seed_forward;
                reverse = seed_reverse;
                n = seed_length;
                literal_words = seed_literals;
                parameter = false;
                pool = pool / 2 * 2 + 1;
                bound = forward.get(&target).copied();
            } else {
                return;
            }
        }
        let right_bound = (extend
            && query.parameter
            && query.suffix
            && !query.right.tokens.is_empty()
            && bound.is_none()
            && literal_words > 0)
            .then(|| self.right_binding(query, data, at, &forward, &reverse))
            .flatten();
        let (parameter, pool) = if right_bound.is_some() {
            (true, pool / 2 * 2)
        } else {
            (parameter, pool)
        };
        if n < self.pools[pool].length
            && (!extend || !self.pools[pool].counts.is_empty() || n < self.backoff[pool].length)
            || self.exhausted()
        {
            return;
        }
        // An unanchored shape alone is not evidence for a new identifier.
        if parameter
            && right_bound.is_none()
            && (forward.len() < 2 || literal_words == 0 || bound.is_none())
        {
            return;
        }
        let word = right_bound
            .or_else(|| bound.and_then(|t| left.word(t)))
            .unwrap_or(literal);
        // The active file may have an older published snapshot. It can refine
        // a relationship between already visible names, but must not invent
        // confidence for an absent stale token at the caret.
        if grounded_only && !left.words.iter().any(|known| known == word) {
            return;
        }
        let mut compatible = true;
        if query.suffix {
            if data.tokens.len().saturating_sub(at + 1) < query.right.tokens.len() {
                compatible = false;
            }
            let mut right_forward = FxHashMap::<u32, &str>::default();
            let mut right_reverse = FxHashMap::<&str, u32>::default();
            for (&a, &b) in &forward {
                let name = left.word(b).unwrap();
                right_forward.insert(a, name);
                right_reverse.insert(name, a);
            }
            for (&a, &b) in data.tokens[at + 1..]
                .iter()
                .zip(&query.right.tokens)
                .take(RIGHT)
            {
                self.comparisons += 1;
                match (data.word(a), query.right.word(b)) {
                    (Some(aw), Some(bw)) if !parameter => {
                        if aw != bw {
                            compatible = false;
                            break;
                        }
                    }
                    (Some(_), Some(bw)) => {
                        // Right tokens have a separate dictionary. Compare names,
                        // keeping both directions of the original left mapping.
                        if right_forward.get(&a).is_some_and(|&mapped| mapped != bw)
                            || right_reverse.get(bw).is_some_and(|&original| original != a)
                        {
                            compatible = false;
                            break;
                        }
                        right_forward.insert(a, bw);
                        right_reverse.insert(bw, a);
                    }
                    (None, None) if a == b => {}
                    _ => {
                        compatible = false;
                        break;
                    }
                }
            }
        }
        // A shorter context is a backoff candidate only when it actually fits
        // the suffix. Its negative witnesses cannot erase a compatible prior;
        // the longest contradictory pool already supplies the uncertainty mass.
        if n < self.pools[pool].length && !compatible {
            return;
        }
        let retrieved = pool < 2 && query.deduplicate;
        let call = data.tokens.get(at + 1) == Some(&('(' as u32));
        let known_right = if query.suffix {
            query.right.tokens.len()
        } else {
            0
        };
        let fp = if retrieved {
            let end = (at + known_right + 1).min(data.tokens.len());
            if !self.charge_hash(end - (at - n)) {
                return;
            }
            let mut h = FxHasher::default();
            // Unused source history and an unseen suffix are not independent
            // evidence. Fingerprint exactly the context this query verified.
            signature(data, at - n, end, parameter).hash(&mut h);
            word.hash(&mut h);
            parameter.hash(&mut h);
            call.hash(&mut h);
            compatible.hash(&mut h);
            h.finish()
        } else {
            let mut hash = FxHasher::default();
            (source, at, pool).hash(&mut hash);
            hash.finish()
        };
        if n > self.pools[pool].length {
            let previous = std::mem::take(&mut self.pools[pool]);
            if extend && !previous.counts.is_empty() && previous.length > self.backoff[pool].length
            {
                self.backoff[pool] = previous;
            }
            self.pools[pool] = Pool {
                length: n,
                ..Pool::default()
            };
            self.retain_depths(pool);
        }
        let shorter = n < self.pools[pool].length;
        if shorter && n > self.backoff[pool].length {
            self.backoff[pool] = Pool {
                length: n,
                ..Pool::default()
            };
            self.retain_depths(pool);
        }
        let weight = if retrieved {
            let observed = self.local_repeat_weight(data, at, n, known_right);
            let previous = self.retrieved_weights.entry((pool, n, fp)).or_default();
            if observed <= *previous {
                return;
            }
            let delta = observed - *previous;
            *previous = observed;
            f64::from(delta)
        } else {
            if !self.seen.insert((pool, n, fp)) {
                return;
            }
            1.
        };
        let p = if shorter {
            &mut self.backoff[pool]
        } else {
            &mut self.pools[pool]
        };
        p.total += weight;
        if compatible {
            *p.counts.entry(word.to_owned()).or_default() += weight;
            if call {
                *p.calls.entry(word.to_owned()).or_default() += weight;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn consistent_renaming_adapts_relationship() {
        let q = Query::new("register(chanB, handlerB)\nunregister(chanB, ", ")");
        let mut e = Evidence::default();
        e.collect(
            &q,
            "register(chanA, handlerA)\nunregister(chanA, handlerA)\n",
            true,
        );
        assert!(e.probability("handlerB", 0.1) > e.probability("handlerA", 0.1));
    }
    #[test]
    fn mappings_are_injective_and_suffix_contradictions_keep_mass() {
        let q = Query::new("register(chanB, chanB)\nunregister(chanB, ", ")");
        let mut e = Evidence::default();
        e.collect(
            &q,
            "register(chanA, handlerA)\nunregister(chanA, handlerA)\n",
            true,
        );
        assert!(!e.candidates().any(|w| w == "chanB"));
        let q = Query::new("choose(chanB, ", ", finalB)");
        let mut e = Evidence::default();
        e.collect(&q, "choose(chanB, handlerB, wrongB)", true);
        assert!(e.mass() > 0.);
        assert!(e.probability("handlerB", 0.2) < 0.2);
    }
    #[test]
    fn streaming_chunks_and_bounds() {
        let mut b = Builder::default();
        b.feed("register(cha");
        b.feed("nnel, handler)");
        let d = b.finish();
        assert!(d.valid());
        assert!(d.words.iter().any(|w| w == "channel"));
        assert!(parse(&"word ".repeat(10000)).tokens.len() <= MAX_TOKENS);
    }
}

#[cfg(test)]
#[path = "completion_examples_tests.rs"]
mod focused_tests;
