//! Finding the files of a workspace that matter for the code at the caret.
//!
//! Each file is reduced to the set of identifiers it uses. A request is
//! answered by weighting the identifiers around the caret by how rare they
//! are in the workspace and ranking files by the weight they share: a file
//! that uses the same unusual names is probably about the same thing. The
//! pattern predictor then learns from those files as if they were open.
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};
use std::hash::Hasher;

/// Identifiers per file that are kept; the rest of a huge file adds little.
const MAX_IDENTIFIERS: usize = 6000;
/// Identifiers shorter than this carry no signal.
const MIN_IDENTIFIER: usize = 3;
const MAX_IDENTIFIER: usize = 64;
/// Identifiers used by more than this share of the files are too common to
/// tell files apart.
const COMMON_SHARE: f32 = 0.3;

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'$' || byte >= 0x80
}

/// Hashes of the distinct identifiers in `text`, skipping numbers.
pub(crate) fn identifiers(text: &str) -> Vec<u32> {
    let bytes = text.as_bytes();
    let mut seen = FxHashSet::default();
    let mut at = 0;
    while at < bytes.len() {
        if !is_word(bytes[at]) {
            at += 1;
            continue;
        }
        let start = at;
        while at < bytes.len() && is_word(bytes[at]) {
            at += 1;
        }
        let word = &bytes[start..at];
        if word.len() < MIN_IDENTIFIER || word.len() > MAX_IDENTIFIER || word[0].is_ascii_digit() {
            continue;
        }
        let mut hasher = FxHasher::default();
        hasher.write(word);
        seen.insert(hasher.finish() as u32);
        if seen.len() >= MAX_IDENTIFIERS {
            break;
        }
    }
    seen.into_iter().collect()
}

#[derive(Default)]
pub(crate) struct Retriever {
    /// Identifier hash -> files using it.
    postings: FxHashMap<u32, Vec<u32>>,
    /// Identifier count of each file, and the key the caller gave it.
    sizes: Vec<u32>,
    keys: Vec<u64>,
}

impl Retriever {
    #[allow(dead_code)] // Reported by the replay benchmark and the tests.
    pub(crate) fn len(&self) -> usize {
        self.keys.len()
    }

    /// Add a file under `key`.
    pub(crate) fn add(&mut self, key: u64, text: &str) {
        let ids = identifiers(text);
        if ids.is_empty() {
            return;
        }
        let file = self.keys.len() as u32;
        self.keys.push(key);
        self.sizes.push(ids.len() as u32);
        for id in ids {
            self.postings.entry(id).or_default().push(file);
        }
    }

    /// The `limit` files that share the most distinctive identifiers with
    /// `context`, best first, leaving out `exclude`.
    pub(crate) fn query(&self, context: &str, exclude: u64, limit: usize) -> Vec<u64> {
        let total = self.keys.len() as f32;
        if total < 2. || limit == 0 {
            return Vec::new();
        }
        let common = (total * COMMON_SHARE).max(8.);
        let mut scores: FxHashMap<u32, f32> = FxHashMap::default();
        for id in identifiers(context) {
            let Some(files) = self.postings.get(&id) else {
                continue;
            };
            let count = files.len() as f32;
            if count > common {
                continue;
            }
            let weight = ((total + 1.) / (count + 0.5)).ln();
            for &file in files {
                *scores.entry(file).or_default() += weight;
            }
        }
        let mut ranked: Vec<(f32, u64)> = scores
            .into_iter()
            .filter(|&(file, _)| self.keys[file as usize] != exclude)
            .map(|(file, score)| {
                // A huge file shares more names by chance.
                let size = self.sizes[file as usize] as f32;
                (score / (1. + size / 1500.).sqrt(), self.keys[file as usize])
            })
            .collect();
        ranked.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        ranked.truncate(limit);
        ranked.into_iter().map(|(_, key)| key).collect()
    }
}

#[cfg(test)]
#[path = "vtcomplete_inline_retrieval_tests.rs"]
mod tests;
