//! Incremental UTF-8 extraction. A source can be a gigabyte without allocating
//! a gigabyte-sized string or monopolizing the background worker.
use super::*;
use sha2::{Digest, Sha256};
use std::io::Read;

pub(super) const CHUNK_BYTES: usize = 64 * 1024;

pub(super) struct Extractor {
    terms: BTreeMap<String, FileTerm>,
    analyzer: crate::completion_context::Analyzer,
    last_code: Option<(usize, String, [u64; 8])>,

    examples: examples::Builder,
}

impl Default for Extractor {
    fn default() -> Self {
        Self {
            terms: BTreeMap::new(),
            analyzer: Default::default(),
            last_code: None,
            examples: examples::Builder::bounded(Catalog::retained_tokens()),
        }
    }
}

impl Extractor {
    pub(super) fn feed(&mut self, text: &str) {
        self.examples.feed(text);
        let terms = &mut self.terms;
        let last = &mut self.last_code;
        self.analyzer
            .feed(text, |event| Self::record(terms, last, event));
    }

    fn record(
        terms: &mut BTreeMap<String, FileTerm>,
        last: &mut Option<(usize, String, [u64; 8])>,
        event: crate::completion_context::Event<'_>,
    ) {
        use crate::completion_context::{Domain, Event};
        match event {
            Event::Call { offset } => {
                if let Some((previous, word, keys)) = last
                    && *previous == offset
                    && let Some(term) = terms.get_mut(word)
                {
                    term.calls = term.calls.saturating_add(1);
                    for key in keys {
                        if let Some(entry) = term.contexts.iter_mut().find(|entry| entry.0 == *key)
                        {
                            entry.2 = entry.2.saturating_add(1);
                        }
                    }
                }
            }
            Event::Word {
                word,
                context,
                offset,
            } => {
                if context.domain == Domain::Code {
                    if let Some((position, previous, keys)) = last {
                        *position = offset;
                        *keys = context.keys;
                        previous.clear();
                        previous.push_str(word);
                    } else {
                        *last = Some((offset, word.to_owned(), context.keys));
                    }
                }
                if !terms.contains_key(word) {
                    if terms.len() >= MAX_TERMS_PER_FILE {
                        return;
                    }
                    terms.insert(
                        word.to_owned(),
                        FileTerm {
                            word: word.into(),
                            frequency: 0,
                            calls: 0,
                            contexts: Default::default(),
                            domains: [0; 3],
                        },
                    );
                }
                let term = terms.get_mut(word).unwrap();
                term.frequency = term.frequency.saturating_add(1);
                term.domains[context.domain.slot()] =
                    term.domains[context.domain.slot()].saturating_add(1);
                for key in context.keys.into_iter().filter(|&key| key != 0) {
                    if let Some(entry) = term.contexts.iter_mut().find(|entry| entry.0 == key) {
                        entry.1 = entry.1.saturating_add(1);
                    } else if term.contexts.len() < MAX_CONTEXTS {
                        term.contexts.push(FileContext(key, 1, 0));
                    }
                }
            }
        }
    }

    #[cfg(test)]
    pub(super) fn finish(self) -> Vec<FileTerm> {
        self.finish_with_examples().0
    }

    pub(super) fn finish_with_examples(mut self) -> (Vec<FileTerm>, examples::Data) {
        let terms = &mut self.terms;
        let last = &mut self.last_code;
        self.analyzer
            .flush(|event| Self::record(terms, last, event));
        (self.terms.into_values().collect(), self.examples.finish())
    }
}

enum Source {
    File(std::fs::File),
    Rope(ropey::Rope),
}

pub(super) struct Scan {
    pub(super) path: PathBuf,
    pub(super) demand: bool,
    source: Source,
    offset: usize,
    total: usize,
    limit: usize,
    stamp: Option<Stamp>,
    parser: Extractor,
    hasher: Sha256,
    carry: Vec<u8>,
    dirty: bool,
    verify: bool,
    expected_hash: Option<[u8; 32]>,
    snapshot_hash: Option<u64>,
}

impl Scan {
    pub(super) fn disk(path: PathBuf, stamp: Stamp, limit: usize) -> std::io::Result<Self> {
        let source = Source::File(std::fs::File::open(&path)?);
        Ok(Self {
            demand: false,
            total: stamp.bytes as usize,
            path,
            source,

            offset: 0,
            limit,
            stamp: Some(stamp),
            parser: Extractor::default(),
            hasher: Sha256::new(),
            carry: Vec::new(),
            dirty: false,
            verify: false,
            expected_hash: None,
            snapshot_hash: None,
        })
    }

    pub(super) fn snapshot(path: PathBuf, rope: ropey::Rope, limit: usize, dirty: bool) -> Self {
        let stamp = Stamp::read(&path);
        let mut scan = Self::buffer(path, rope, limit, true);
        scan.stamp = stamp;
        scan.verify = !dirty;
        scan
    }

    /// A loaded remote buffer is authoritative. Never stat or reopen its key.
    pub(super) fn buffer(path: PathBuf, rope: ropey::Rope, limit: usize, dirty: bool) -> Self {
        let bytes = rope.len_bytes();
        Self {
            demand: true,
            total: bytes,
            path,
            source: Source::Rope(rope),
            offset: 0,
            limit,
            // Retain source size for later limit changes, without claiming a
            // native file revision. Buffer scans never validate this stamp.
            stamp: Some(Stamp {
                bytes: bytes as u64,
                modified_ns: 0,
            }),
            parser: Extractor::default(),
            hasher: Sha256::new(),
            carry: Vec::new(),
            dirty,
            verify: false,
            expected_hash: None,
            snapshot_hash: None,
        }
    }

    pub(super) fn progress(&self) -> (usize, usize) {
        (self.offset, self.total)
    }

    pub(super) fn clean_buffer(&self) -> bool {
        matches!(self.source, Source::Rope(_)) && !self.dirty && !self.verify
    }

    /// Process at most one small source slice; true means ready to install.
    pub(super) fn step(&mut self) -> std::io::Result<bool> {
        let eof;
        match &mut self.source {
            Source::Rope(rope) => {
                let end = rope.char_to_byte(
                    rope.byte_to_char(
                        self.offset
                            .saturating_add(CHUNK_BYTES)
                            .min(rope.len_bytes()),
                    ),
                );
                for text in rope.byte_slice(self.offset..end).chunks() {
                    self.hasher.update(text.as_bytes());
                    self.parser.feed(text);
                }
                self.offset = end;
                eof = end == rope.len_bytes();
            }
            Source::File(file) => {
                let mut bytes = vec![0; CHUNK_BYTES];
                let count = file.read(&mut bytes)?;
                if count == 0 {
                    eof = true;
                } else {
                    self.offset = self.offset.saturating_add(count);
                    if self.offset > self.limit {
                        return Err(std::io::Error::other(
                            "File exceeds VTComplete indexing limit",
                        ));
                    }
                    self.hasher.update(&bytes[..count]);
                    if self.expected_hash.is_none() {
                        self.carry.extend_from_slice(&bytes[..count]);
                        if self.carry.contains(&0) {
                            return Err(std::io::Error::other("Binary file"));
                        }
                        match std::str::from_utf8(&self.carry) {
                            Ok(text) => {
                                self.parser.feed(text);
                                self.carry.clear();
                            }
                            Err(error) if error.error_len().is_none() => {
                                let end = error.valid_up_to();
                                self.parser
                                    .feed(std::str::from_utf8(&self.carry[..end]).unwrap());
                                self.carry.drain(..end);
                            }
                            Err(_) => return Err(std::io::Error::other("Non UTF-8 file")),
                        }
                    }
                    eof = self.offset >= self.total;
                }
            }
        }
        if !eof {
            return Ok(false);
        }
        if !self.carry.is_empty() {
            return Err(std::io::Error::other("Incomplete UTF-8 file"));
        }
        let digest: [u8; 32] = self.hasher.clone().finalize().into();
        if self.verify {
            self.verify = false;
            self.snapshot_hash = Some(u64::from_le_bytes(digest[..8].try_into().unwrap()));
            if let Some(stamp) = Stamp::read(&self.path)
                && stamp.bytes as usize == self.total
                && let Ok(file) = std::fs::File::open(&self.path)
            {
                self.stamp = Some(stamp);
                self.source = Source::File(file);
                self.expected_hash = Some(digest);
                self.hasher = Sha256::new();
                self.offset = 0;
                return Ok(false);
            }
        }
        if let Some(expected) = self.expected_hash {
            self.dirty = expected != digest || Stamp::read(&self.path) != self.stamp;
        } else if !self.dirty
            && matches!(self.source, Source::File(_))
            && Stamp::read(&self.path) != self.stamp
        {
            return Err(std::io::Error::other("File changed while being indexed"));
        }
        Ok(true)
    }

    pub(super) fn finish(self) -> FileRecord {
        let digest = self.hasher.finalize();
        let (terms, examples) = self.parser.finish_with_examples();
        FileRecord {
            path: self.path,
            stamp: self.stamp,
            hash: self
                .snapshot_hash
                .unwrap_or_else(|| u64::from_le_bytes(digest[..8].try_into().unwrap())),
            terms: terms.into(),
            examples: Arc::new(examples),

            dirty: self.dirty,
            memory_bytes: 0,
            reducible_bytes: 0,
        }
    }
}
