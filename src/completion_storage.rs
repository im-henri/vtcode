//! Retained vocabulary is IDs and packed counters, not per-file term objects.
//! Rich context is sparse and bounded separately from the workspace vocabulary.
use super::*;

const PAGE: usize = 256;
const CONTEXT_BUDGET: usize = 512 * 1024;

#[derive(Clone)]
struct Word {
    key: Arc<str>,
    frequency: u32,
    calls: u32,
    domains: [u32; 3],
    // Occupies the slot's alignment padding. Aggregate statistics may saturate;
    // ID lifetime must instead follow the exact number of file contributions.
    owners: u32,
}

impl Word {
    fn spelling(&self) -> &str {
        self.key
            .split_once('\0')
            .map_or(self.key.as_ref(), |(_, word)| word)
    }
}

struct WordPage {
    values: Box<[Option<Word>]>,
    live: usize,
}
#[derive(Default)]
struct Words {
    pages: Vec<WordPage>,
    free: Vec<u32>,
    next: u32,
}
impl Words {
    fn get(&self, id: u32) -> Option<&Word> {
        self.pages
            .get(id as usize / PAGE)?
            .values
            .get(id as usize % PAGE)?
            .as_ref()
    }
    fn get_mut(&mut self, id: u32) -> Option<&mut Word> {
        self.pages
            .get_mut(id as usize / PAGE)?
            .values
            .get_mut(id as usize % PAGE)?
            .as_mut()
    }
    fn insert(&mut self, word: Word) -> u32 {
        let id = self.free.pop().unwrap_or_else(|| {
            let id = self.next;
            self.next += 1;
            id
        });
        let page = id as usize / PAGE;
        if page == self.pages.len() {
            self.pages.push(WordPage {
                values: Box::new([]),
                live: 0,
            });
        }
        let page = &mut self.pages[page];
        if page.values.is_empty() {
            page.values = (0..PAGE).map(|_| None).collect();
        }
        page.values[id as usize % PAGE] = Some(word);
        page.live += 1;
        id
    }
    fn remove(&mut self, id: u32) -> Word {
        let page = &mut self.pages[id as usize / PAGE];
        let word = page.values[id as usize % PAGE].take().unwrap();
        page.live -= 1;
        if page.live == 0 {
            page.values = Box::new([]);
        }
        self.free.push(id);
        word
    }
    fn memory(&self) -> usize {
        self.pages.capacity() * std::mem::size_of::<WordPage>()
            + self.free.capacity() * 4
            + self
                .pages
                .iter()
                .filter(|page| !page.values.is_empty())
                .map(|page| page.values.len() * std::mem::size_of::<Option<Word>>() + 16)
                .sum::<usize>()
    }
}

fn put(out: &mut Vec<u8>, mut value: u32) {
    while value >= 128 {
        out.push(value as u8 | 128);
        value >>= 7;
    }
    out.push(value as u8);
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn get(&mut self) -> Option<u32> {
        let mut value = 0;
        for shift in (0..35).step_by(7) {
            let byte = *self.bytes.get(self.at)?;
            self.at += 1;
            if shift == 28 && byte > 15 {
                return None;
            }
            value |= u32::from(byte & 127) << shift;
            if byte & 128 == 0 {
                return Some(value);
            }
        }
        None
    }
    fn hash(&mut self) -> Option<u64> {
        let end = self.at.checked_add(8)?;
        let hash = u64::from_le_bytes(self.bytes.get(self.at..end)?.try_into().ok()?);
        self.at = end;
        Some(hash)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub(super) id: u32,
    pub(super) frequency: u16,
    pub(super) calls: u16,
    pub(super) domains: [u16; 3],
}

#[derive(Default)]
pub(super) struct Entries {
    words: Arc<[u8]>,
    contexts: Box<[u8]>,
    pub(super) len: usize,
}
impl Entries {
    pub(super) fn memory(&self) -> usize {
        96 + self.words.len() + self.contexts.len()
    }
    pub(super) fn context_bytes(&self) -> usize {
        self.contexts.len()
    }
    pub(super) fn without_contexts(&self) -> Self {
        Self {
            words: Arc::clone(&self.words),
            contexts: Box::new([]),
            len: self.len,
        }
    }
    pub(super) fn iter(&self) -> impl Iterator<Item = Entry> + '_ {
        let mut input = Reader::new(&self.words);
        let mut id = 0u32;
        std::iter::from_fn(move || {
            id = id.checked_add(input.get()?)?;
            let frequency = u16::try_from(input.get()?).ok()?;
            let flags = *input.bytes.get(input.at)?;
            input.at += 1;
            let calls = match flags & 3 {
                0 => 0,
                1 => frequency,
                _ => u16::try_from(input.get()?).ok()?,
            };
            let domains = match flags >> 2 {
                0 => [frequency, 0, 0],
                1 => [0, frequency, 0],
                2 => [0, 0, frequency],
                _ => [
                    u16::try_from(input.get()?).ok()?,
                    u16::try_from(input.get()?).ok()?,
                    u16::try_from(input.get()?).ok()?,
                ],
            };
            Some(Entry {
                id,
                frequency,
                calls,
                domains,
            })
        })
    }
    fn contexts(&self, mut visit: impl FnMut(u64, u32, u32, u32)) {
        let mut input = Reader::new(&self.contexts);
        while let Some(hash) = input.hash() {
            let Some(count) = input.get() else { break };
            let mut id = 0u32;
            for _ in 0..count {
                let Some(delta) = input.get() else { return };
                let Some(next) = id.checked_add(delta) else {
                    return;
                };
                id = next;
                let Some(uses) = input.get() else { return };
                let Some(calls) = input.get() else { return };
                visit(hash, id, uses, calls);
            }
        }
    }
}

#[derive(Clone)]
pub(super) struct StoredRecord {
    pub(super) path: PathBuf,
    pub(super) stamp: Option<Stamp>,
    pub(super) hash: u64,
    pub(super) terms: Arc<Entries>,
    pub(super) examples: Arc<examples::Data>,

    pub(super) dirty: bool,
}
impl StoredRecord {
    pub(super) fn memory(&self) -> usize {
        384 + self.path.capacity()
            + self.path.as_os_str().len() * 2
            + self.terms.memory()
            + self.examples.memory()
    }
    pub(super) fn reducible_memory(&self) -> usize {
        self.terms.context_bytes()
            + if self.examples.tokens() > 8 {
                self.examples.tokens() * 64
            } else {
                0
            }
    }
    pub(super) fn decoded(&self, catalog: &Catalog) -> FileRecord {
        let mut terms: BTreeMap<u32, FileTerm> = self
            .terms
            .iter()
            .map(|entry| {
                let word = catalog.words.get(entry.id).expect("live file word ID");
                (
                    entry.id,
                    FileTerm {
                        word: Arc::from(word.spelling()),
                        frequency: entry.frequency,
                        calls: entry.calls,
                        domains: entry.domains,
                        contexts: FileContexts::default(),
                    },
                )
            })
            .collect();
        self.terms.contexts(|hash, id, uses, calls| {
            if let Some(term) = terms.get_mut(&id) {
                term.contexts
                    .push(FileContext(hash, uses as u16, calls as u16));
            }
        });
        let mut terms: Vec<_> = terms.into_values().collect();
        terms.sort_unstable_by(|a, b| a.word.cmp(&b.word));
        FileRecord {
            path: self.path.clone(),
            stamp: self.stamp.clone(),
            hash: self.hash,
            terms: Arc::new(terms),
            examples: Arc::clone(&self.examples),
            dirty: self.dirty,
            memory_bytes: 0,
            reducible_bytes: 0,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct Conditional {
    pub(super) id: u32,
    uses: u32,
    calls: u32,
}

#[derive(Default)]
pub(super) struct Catalog {
    pub(super) terms: BTreeMap<Arc<str>, u32>,
    words: Words,
    pub(super) buckets: BTreeMap<char, Vec<u32>>,
    pub(super) initials: BTreeMap<String, Vec<u32>>,
    pub(super) by_context: BTreeMap<u64, Vec<Conditional>>,
    pub(super) files: BTreeMap<u64, Arc<StoredRecord>>,
    pub(super) history: crate::completion_history::History,
    pub(super) examples: examples::Index,
    pub(super) memory_bytes: usize,
    name_bytes: usize,
    context_bytes: usize,
}

fn insert_id(words: &Words, list: &mut Vec<u32>, id: u32) {
    let key = &words.get(id).unwrap().key;
    if let Err(at) = list.binary_search_by(|other| words.get(*other).unwrap().key.cmp(key)) {
        list.reserve_exact(1);
        list.insert(at, id);
    }
}
fn remove_id(words: &Words, list: &mut Vec<u32>, id: u32) {
    let key = &words.get(id).unwrap().key;
    if let Ok(at) = list.binary_search_by(|other| words.get(*other).unwrap().key.cmp(key)) {
        list.remove(at);
    }
    if list.len() * 2 < list.capacity() {
        list.shrink_to_fit();
    }
}

impl Catalog {
    #[cfg(test)]
    pub(super) fn add(&mut self, terms: &mut [FileTerm]) -> Arc<StoredRecord> {
        let record = FileRecord {
            path: format!("/catalog{}.custom", self.files.len()).into(),
            stamp: None,
            hash: 0,
            terms: Arc::new(terms.to_vec()),
            examples: Arc::default(),
            dirty: true,
            memory_bytes: 0,
            reducible_bytes: 0,
        };
        self.pack(record, &IndexSettings::default())
    }
    #[cfg(test)]
    pub(super) fn spelling(&self, id: u32) -> Option<&str> {
        self.words.get(id).map(Word::spelling)
    }
    pub(super) fn example_budget(settings: &IndexSettings) -> usize {
        (4 * 1024 * 1024).min(retained_limit(settings) / 4)
    }
    pub(super) fn retained_tokens() -> usize {
        #[cfg(test)]
        if let Ok(value) = std::env::var("VTCODE_EFFICIENCY_TOKENS") {
            if let Ok(tokens) = value.parse::<usize>() {
                return tokens.clamp(32, 2048);
            }
        }
        64
    }
    fn context_memory(&self) -> usize {
        if self.by_context.is_empty() {
            return 0;
        }
        512 + self.by_context.len() * 112
            + self
                .by_context
                .values()
                .map(|v| v.capacity() * 12 + 16)
                .sum::<usize>()
    }
    pub(super) fn refresh_memory(&mut self) {
        self.context_bytes = self.context_memory();
        self.memory_bytes = self.name_bytes
            + self.words.memory()
            + self.terms.len() * 64
            + usize::from(!self.terms.is_empty()) * 256
            + self.buckets.len() * 96
            + self
                .buckets
                .values()
                .map(|v| v.capacity() * 4 + 16)
                .sum::<usize>()
            + self.initials.len() * 128
            + self
                .initials
                .iter()
                .map(|(k, v)| k.capacity() + 16 + v.capacity() * 4 + 16)
                .sum::<usize>()
            + self.context_bytes
            + self.files.len() * 64
            + usize::from(!self.files.is_empty()) * 256
            + self.examples.memory();
    }
    #[cfg(test)]
    pub(super) fn ledger(&self, files: usize) -> serde_json::Value {
        serde_json::json!({"fileRecords":files,"catalog":self.memory_bytes,"wordPages":self.words.memory(),
            "internedNames":self.name_bytes,"dictionaryNodes":self.terms.len()*64,
            "fuzzyIds":self.buckets.values().chain(self.initials.values()).map(|v|v.capacity()*4).sum::<usize>(),
            "contexts":self.context_bytes,"examplePostings":self.examples.memory()})
    }
    pub(super) fn additional_memory(&self, terms: &[FileTerm]) -> usize {
        256 + terms
            .iter()
            .map(|term| {
                if self.terms.contains_key(&key(&term.word)) {
                    20
                } else {
                    512 + term.word.len() * 3 + boundaries(&term.word).len() * 16
                }
            })
            .sum::<usize>()
    }
    fn word_id(&mut self, source: &FileTerm) -> u32 {
        let lookup = key(&source.word);
        if let Some(&id) = self.terms.get(&lookup) {
            return id;
        }
        let id = self.words.insert(Word {
            key: Arc::clone(&lookup),
            frequency: 0,
            calls: 0,
            domains: [0; 3],
            owners: 0,
        });
        self.name_bytes += lookup.len() + 32;
        self.terms.insert(lookup, id);
        for boundary in boundaries(&source.word) {
            insert_id(&self.words, self.buckets.entry(boundary).or_default(), id);
        }
        insert_id(
            &self.words,
            self.initials
                .entry(crate::completion_context::initials(&source.word))
                .or_default(),
            id,
        );
        id
    }
    pub(super) fn pack(
        &mut self,
        record: FileRecord,
        settings: &IndexSettings,
    ) -> Arc<StoredRecord> {
        let mut entries = Vec::with_capacity(record.terms.len());
        let mut contexts = BTreeMap::<u64, Vec<Conditional>>::new();
        let limit = CONTEXT_BUDGET.min(retained_limit(settings) / 16);
        for source in record.terms.iter() {
            let id = self.word_id(source);
            let word = self.words.get_mut(id).unwrap();
            word.owners += 1;
            word.frequency = word.frequency.saturating_add(u32::from(source.frequency));
            word.calls = word.calls.saturating_add(u32::from(source.calls));
            for (total, count) in word.domains.iter_mut().zip(source.domains) {
                *total = total.saturating_add(u32::from(count));
            }
            entries.push(Entry {
                id,
                frequency: source.frequency,
                calls: source.calls,
                domains: source.domains,
            });
            for &FileContext(hash, uses, calls) in &source.contexts {
                let existing = self.by_context.get(&hash);
                let at = existing.and_then(|list| {
                    list.binary_search_by(|item| {
                        self.words
                            .get(item.id)
                            .unwrap()
                            .key
                            .cmp(&self.words.get(id).unwrap().key)
                    })
                    .ok()
                });
                let growth = 160 + usize::from(self.by_context.is_empty()) * 512;
                if at.is_none() && self.context_bytes + growth > limit {
                    continue;
                }
                let list = self.by_context.entry(hash).or_default();
                let at = match at {
                    Some(at) => at,
                    None => {
                        let at = list
                            .binary_search_by(|item| {
                                self.words
                                    .get(item.id)
                                    .unwrap()
                                    .key
                                    .cmp(&self.words.get(id).unwrap().key)
                            })
                            .unwrap_err();
                        list.reserve_exact(1);
                        list.insert(
                            at,
                            Conditional {
                                id,
                                uses: 0,
                                calls: 0,
                            },
                        );
                        self.context_bytes += growth;
                        at
                    }
                };
                list[at].uses = list[at].uses.saturating_add(u32::from(uses));
                list[at].calls = list[at].calls.saturating_add(u32::from(calls));
                contexts.entry(hash).or_default().push(Conditional {
                    id,
                    uses: u32::from(uses),
                    calls: u32::from(calls),
                });
            }
        }
        entries.sort_unstable_by_key(|entry| entry.id);
        let mut packed = Vec::new();
        let mut previous = 0;
        for entry in &entries {
            put(&mut packed, entry.id - previous);
            previous = entry.id;
            put(&mut packed, u32::from(entry.frequency));
            let calls = if entry.calls == 0 {
                0
            } else if entry.calls == entry.frequency {
                1
            } else {
                2
            };
            let domain = if entry.domains == [entry.frequency, 0, 0] {
                0
            } else if entry.domains == [0, entry.frequency, 0] {
                1
            } else if entry.domains == [0, 0, entry.frequency] {
                2
            } else {
                3
            };
            packed.push(calls | domain << 2);
            if calls == 2 {
                put(&mut packed, u32::from(entry.calls));
            }
            if domain == 3 {
                for count in entry.domains {
                    put(&mut packed, u32::from(count));
                }
            }
        }
        let mut conditional = Vec::new();
        for (hash, mut counts) in contexts {
            counts.sort_unstable_by_key(|count| count.id);
            conditional.extend_from_slice(&hash.to_le_bytes());
            put(&mut conditional, counts.len() as u32);
            let mut previous = 0;
            for count in counts {
                put(&mut conditional, count.id - previous);
                previous = count.id;
                put(&mut conditional, count.uses);
                put(&mut conditional, count.calls);
            }
        }
        let terms = Arc::new(Entries {
            words: packed.into(),
            contexts: conditional.into(),
            len: entries.len(),
        });
        let file = crate::completion_source::Source::new(Some(&record.path), "").file;
        self.examples.insert(file, Arc::clone(&record.examples));
        let stored = Arc::new(StoredRecord {
            path: record.path,
            stamp: record.stamp,
            hash: record.hash,
            terms,
            examples: record.examples,
            dirty: record.dirty,
        });
        self.files.insert(file, Arc::clone(&stored));
        self.refresh_memory();
        stored
    }
    pub(super) fn drop_contexts(&mut self, entries: &Entries) {
        entries.contexts(|hash, id, uses, calls| {
            if let Some(list) = self.by_context.get_mut(&hash) {
                if let Some(at) = list.iter().position(|item| item.id == id) {
                    list[at].uses = list[at].uses.saturating_sub(uses);
                    list[at].calls = list[at].calls.saturating_sub(calls);
                    if list[at].uses == 0 {
                        list.remove(at);
                    }
                }
                if list.is_empty() {
                    self.by_context.remove(&hash);
                } else if list.len() * 2 < list.capacity() {
                    list.shrink_to_fit();
                }
            }
        });
        if self.by_context.is_empty() {
            self.by_context = BTreeMap::new();
        }
    }
    pub(super) fn remove_record(&mut self, record: &StoredRecord) {
        let file = crate::completion_source::Source::new(Some(&record.path), "").file;
        self.files.remove(&file);
        self.examples.remove(file);
        self.drop_contexts(&record.terms);
        for entry in record.terms.iter() {
            let word = self.words.get_mut(entry.id).unwrap();
            word.owners -= 1;
            word.frequency = word.frequency.saturating_sub(u32::from(entry.frequency));
            word.calls = word.calls.saturating_sub(u32::from(entry.calls));
            for (total, count) in word.domains.iter_mut().zip(entry.domains) {
                *total = total.saturating_sub(u32::from(count));
            }
            if word.owners != 0 {
                continue;
            }
            let spelling = word.spelling().to_owned();
            for boundary in boundaries(&spelling) {
                if let Some(list) = self.buckets.get_mut(&boundary) {
                    remove_id(&self.words, list, entry.id);
                    if list.is_empty() {
                        self.buckets.remove(&boundary);
                    }
                }
            }
            let initials = crate::completion_context::initials(&spelling);
            if let Some(list) = self.initials.get_mut(&initials) {
                remove_id(&self.words, list, entry.id);
                if list.is_empty() {
                    self.initials.remove(&initials);
                }
            }
            let word = self.words.remove(entry.id);
            self.name_bytes = self.name_bytes.saturating_sub(word.key.len() + 32);
            self.terms.remove(&word.key);
        }
        if self.terms.is_empty() {
            self.words = Words::default();
            self.terms = BTreeMap::new();
            self.initials = BTreeMap::new();
            self.buckets = BTreeMap::new();
        }
        if self.files.is_empty() {
            self.files = BTreeMap::new();
        }
        self.refresh_memory();
    }
    pub(super) fn vocabulary(&self, file: u64, prefix: &str) -> Vec<(String, u32, u32)> {
        let Some(record) = self.files.get(&file) else {
            return Vec::new();
        };
        let lower = prefix.to_lowercase();
        record
            .terms
            .iter()
            .filter(|entry| entry.domains[0] > 0)
            .filter_map(|entry| {
                let word = self.words.get(entry.id)?;
                (subsequence(&lower, word.key.split('\0').next().unwrap_or_default())
                    || crate::completion_match::typo_prefix(word.spelling(), prefix))
                .then(|| {
                    (
                        word.spelling().to_string(),
                        u32::from(entry.domains[0]),
                        u32::from(entry.calls),
                    )
                })
            })
            .take(MAX_QUERY_CANDIDATES)
            .collect()
    }
    pub(super) fn query_cancellable(&self, prefix: &str, context: &str,
        structural: crate::completion_context::Context, history: &BTreeMap<String, i64>,
        broad: bool, cancelled: &impl Fn() -> bool) -> Vec<LearnedCandidate> {
        if cancelled() { return Vec::new(); }
        let lower = prefix.to_lowercase();
        let mut keys: Vec<_> = structural
            .keys
            .into_iter()
            .zip([230, 165, 220, 160, 180, 36, 54, 16])
            .filter(|(k, _)| *k != 0)
            .collect();
        keys.extend(context_keys(context).into_iter().map(|key| (key, 100)));
        let mut ids = BTreeSet::new();
        for word in history.keys() {
            if let Some(id) = self.terms.get(&key(word)) {
                ids.insert(*id);
            }
        }
        if broad || structural.structural() || prefix.chars().count() >= 2 {
            for (hash, _) in &keys {
                if cancelled() { return Vec::new(); }
                if let Some(list) = self.by_context.get(hash) {
                    for item in list.iter().take(if broad { 512 } else { 128 }) {
                        if ids.len() >= 2304 {
                            break;
                        }
                        if self.words.get(item.id).is_some_and(|w| {
                            subsequence(&lower, w.key.split('\0').next().unwrap_or_default())
                        }) {
                            ids.insert(item.id);
                        }
                    }
                }
            }
            let initial_limit = (ids.len() + 512).min(3328);
            for (initials, list) in
                self.initials
                    .range(lower.clone()..)
                    .take(if broad { 256 } else { 16 })
            {
                if !initials.starts_with(&lower) || ids.len() >= initial_limit {
                    break;
                }
                for &id in list.iter().take(if broad { 128 } else { 16 }) {
                    if ids.len() >= initial_limit {
                        break;
                    }
                    ids.insert(id);
                }
            }
            for (key, &id) in self
                .terms
                .range(Arc::<str>::from(lower.as_str())..)
                .take(if broad { 768 } else { 128 })
            {
                if !key.starts_with(&lower) || ids.len() >= MAX_QUERY_CANDIDATES {
                    break;
                }
                ids.insert(id);
            }
            if broad {
                if let Some(list) = lower.chars().next().and_then(|ch| self.buckets.get(&ch)) {
                    for &id in list {
                        if ids.len() >= MAX_QUERY_CANDIDATES {
                            break;
                        }
                        ids.insert(id);
                    }
                }
            }
        }
        let mut candidates = Vec::new();
        for (at, id) in ids.into_iter().enumerate() {
            if at % 32 == 0 && cancelled() { return Vec::new(); }
            let Some(word) = self.words.get(id) else {
                continue;
            };
            if !subsequence(&lower, word.key.split('\0').next().unwrap_or_default())
                && !crate::completion_match::typo_prefix(word.spelling(), prefix)
            {
                continue;
            }
            if word.spelling() == prefix && word.calls < 2 {
                continue;
            }
            let history = history.get(word.spelling()).copied().unwrap_or(0);
            let mut score = 0;
            let mut receiver = false;
            let mut primary = None;
            for (hash, weight) in &keys {
                let Some(list) = self.by_context.get(hash) else {
                    continue;
                };
                let Ok(at) = list
                    .binary_search_by(|item| self.words.get(item.id).unwrap().key.cmp(&word.key))
                else {
                    continue;
                };
                let item = list[at];
                score = score.max(weight + i64::from(item.uses.min(8)) * 5);
                receiver |= structural.keys[..2].contains(hash);
                if let Some(rank) = [0, 1, 4, 2, 3]
                    .into_iter()
                    .position(|slot| structural.keys[slot] == *hash && *hash != 0)
                    && primary.is_none_or(|(old, _, _)| rank < old)
                {
                    primary = Some((rank, item.uses, item.calls));
                }
            }
            let (_, uses, calls) = primary.unwrap_or_default();
            let retrieval = if word.spelling().starts_with(prefix) {
                3
            } else if word.key.starts_with(&lower)
                || crate::completion_context::initials_start_with(word.spelling(), &lower)
            {
                2
            } else {
                0
            };
            candidates.push((id, score, history, retrieval, uses, calls, receiver));
        }
        let compare = |a: &(u32, i64, i64, u8, u32, u32, bool),
                       b: &(u32, i64, i64, u8, u32, u32, bool)| {
            (b.1 + b.2 + i64::from(b.3) * 45)
                .cmp(&(a.1 + a.2 + i64::from(a.3) * 45))
                .then_with(|| b.3.cmp(&a.3))
                .then_with(|| {
                    self.words
                        .get(b.0)
                        .unwrap()
                        .frequency
                        .cmp(&self.words.get(a.0).unwrap().frequency)
                })
                .then_with(|| {
                    self.words
                        .get(a.0)
                        .unwrap()
                        .spelling()
                        .cmp(self.words.get(b.0).unwrap().spelling())
                })
        };
        if candidates.len() > MAX_QUERY_RESULTS {
            candidates.select_nth_unstable_by(MAX_QUERY_RESULTS, compare);
            candidates.truncate(MAX_QUERY_RESULTS);
        }
        candidates.sort_unstable_by(compare);
        if cancelled() { return Vec::new(); }
        candidates
            .into_iter()
            .map(|(id, score, history, _, uses, calls, receiver)| {
                let w = self.words.get(id).unwrap();
                let (uses_for_call, calls_for_call) = if uses > 0 {
                    (uses, calls)
                } else {
                    (
                        if w.domains == [0; 3] {
                            w.frequency
                        } else {
                            w.domains[0]
                        },
                        w.calls,
                    )
                };
                LearnedCandidate {
                    word: w.spelling().to_string(),
                    context_score: score,
                    frequency: w.frequency,
                    callable: calls_for_call >= 2
                        && u64::from(calls_for_call) * 3 >= u64::from(uses_for_call) * 2,
                    domain: if w.domains[0] > 0 || w.domains == [0; 3] {
                        crate::completion_context::Domain::Code
                    } else if w.domains[1] >= w.domains[2] {
                        crate::completion_context::Domain::String
                    } else {
                        crate::completion_context::Domain::Comment
                    },
                    receiver_match: receiver,
                    history_score: history,
                    context_uses: uses,
                    context_calls: calls,
                }
            })
            .collect()
    }
}

#[cfg(test)]
#[path = "completion_storage_tests.rs"]
mod tests;
