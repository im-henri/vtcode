//! Exact content summaries reused across edits, owned and locked only by the
//! analysis worker. No document text or per-line metadata is copied on the UI.
use crate::background::StickyScopeLine;
use crate::rope_line_blocks::{BLOCK_LINES, Block};
use ropey::{Rope, RopeSlice};
use std::{collections::VecDeque, sync::Arc, time::Instant};

const MAX_SOURCE_BYTES: usize = 32 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 4 * 1024 * 1024;
const MAX_DOCUMENTS: usize = 8;

type Metrics = (usize, usize, usize, usize, Arc<[StickyScopeLine]>);

#[derive(Default)]
struct Summary {
    errors: usize,
    warnings: usize,
    width: usize,
    words: usize,
    // Relative line and indentation, not a precomputed parent: an earlier
    // edit can change scope ancestry even when this block's text is unchanged.
    scopes: Vec<(usize, usize)>,
}

struct Snapshot {
    document: u64,
    source: Rope,
    blocks: Vec<Block<Summary>>,
    retained_bytes: usize,
    metrics: Metrics,
}

#[derive(Default)]
pub(crate) struct Cache {
    snapshots: VecDeque<Snapshot>,
    source_bytes: usize,
    cache_bytes: usize,
    #[cfg(test)]
    scanned_lines: usize,
}

impl Cache {
    pub(crate) fn analyze(
        &mut self,
        document: u64,
        rope: &Rope,
        deadline: Instant,
        cancelled: &impl Fn() -> bool,
    ) -> Option<Metrics> {
        let stopped = || cancelled() || Instant::now() >= deadline;
        if stopped() {
            return None;
        }
        #[cfg(test)]
        {
            self.scanned_lines = 0;
        }

        let previous_index = self
            .snapshots
            .iter()
            .position(|old| old.document == document);
        if let Some(index) = previous_index
            && self.snapshots[index].source.is_instance(rope)
        {
            let metrics = self.snapshots[index].metrics.clone();
            if stopped() {
                return None;
            }
            let snapshot = self.snapshots.remove(index).unwrap();
            self.snapshots.push_back(snapshot);
            return Some(metrics);
        }
        let previous = previous_index.map(|index| &self.snapshots[index]);
        let mut blocks = Vec::new();
        let mut start = 0;
        let mut end = rope.len_lines();
        let mut suffix = &[][..];
        if let Some(old) = previous {
            let plan = crate::rope_line_blocks::plan(&old.blocks, &old.source, rope, &stopped)?;
            blocks.extend_from_slice(&old.blocks[..plan.prefix]);
            start = plan.rescan.start;
            end = plan.rescan.end;
            suffix = &old.blocks[plan.suffix..];
        }

        let mut lines = rope.lines_at(start);
        while start < end {
            if stopped() {
                return None;
            }
            let count = BLOCK_LINES.min(end - start);
            let mut summary = Summary::default();
            for (offset, line) in lines.by_ref().take(count).enumerate() {
                // A block bounds deadline polling for ordinary short lines;
                // long lines also check it between Rope chunks below.
                if cancelled() {
                    return None;
                }
                summarize_line(line, offset, &mut summary, &stopped)?;
                #[cfg(test)]
                {
                    self.scanned_lines += 1;
                }
            }
            blocks.push(Block {
                start,
                lines: count,
                summary: Arc::new(summary),
            });
            start += count;
        }
        for old in suffix {
            if stopped() {
                return None;
            }
            blocks.push(Block {
                start,
                lines: old.lines,
                summary: Arc::clone(&old.summary),
            });
            start += old.lines;
        }

        let mut errors = 0;
        let mut warnings = 0;
        let mut width = 0;
        let mut words = 0;
        let mut scopes = Vec::new();
        let mut parents: Vec<(usize, usize)> = Vec::new();
        let mut retained = blocks.capacity() * std::mem::size_of::<Block<Summary>>();
        for block in &blocks {
            if stopped() {
                return None;
            }
            let summary = &block.summary;
            errors += summary.errors;
            warnings += summary.warnings;
            width = width.max(summary.width);
            words += summary.words;
            retained += std::mem::size_of::<Summary>()
                + summary.scopes.capacity() * std::mem::size_of::<(usize, usize)>();
            for &(line, indent) in &summary.scopes {
                while parents
                    .last()
                    .is_some_and(|&(parent_indent, _)| parent_indent >= indent)
                {
                    parents.pop();
                }
                scopes.push(StickyScopeLine {
                    line: block.start + line,
                    parent: parents.last().map(|&(_, index)| index),
                });
                parents.push((indent, scopes.len() - 1));
            }
        }
        if stopped() {
            return None;
        }
        // Share the completed result with the UI instead of reconstructing its
        // ancestry and allocating a fresh vector when an unchanged tab returns.
        // Its exact storage is part of the existing aggregate metadata budget.
        let scopes: Arc<[StickyScopeLine]> = scopes.into();
        retained += scopes.len() * std::mem::size_of::<StickyScopeLine>()
            + 2 * std::mem::size_of::<usize>();
        let metrics = (errors, warnings, width, words, scopes);
        if stopped() {
            return None;
        }
        // Commit only complete scans. Keep recently used documents so changing
        // tabs does not throw away unchanged block summaries. The limits apply
        // to the entire cache; counting logical Rope bytes is conservative when
        // snapshots happen to share storage. Cancellation never changes LRU order.
        if let Some(index) = previous_index {
            self.discard_snapshot(index);
        }
        if rope.len_bytes() <= MAX_SOURCE_BYTES && retained <= MAX_CACHE_BYTES {
            self.source_bytes += rope.len_bytes();
            self.cache_bytes += retained;
            self.snapshots.push_back(Snapshot {
                document,
                source: rope.clone(),
                blocks,
                retained_bytes: retained,
                metrics: metrics.clone(),
            });
            while self.snapshots.len() > MAX_DOCUMENTS
                || self.source_bytes > MAX_SOURCE_BYTES
                || self.cache_bytes > MAX_CACHE_BYTES
            {
                self.discard_snapshot(0);
            }
        }
        Some(metrics)
    }

    fn discard_snapshot(&mut self, index: usize) {
        let old = self.snapshots.remove(index).expect("existing cache entry");
        self.source_bytes -= old.source.len_bytes();
        self.cache_bytes -= old.retained_bytes;
    }
}

fn summarize_line(
    line: RopeSlice<'_>,
    offset: usize,
    summary: &mut Summary,
    stopped: &impl Fn() -> bool,
) -> Option<()> {
    // Most source lines are contiguous. Keep this path as cheap as the old
    // full scan, without constructing Rope character iterators for its edges.
    if let Some(text) = line.as_str() {
        let (errors, warnings) = super::count_diagnostic_markers(text);
        summary.errors += errors;
        summary.warnings += warnings;
        summary.words += text.split_whitespace().count();
        let clean = text.trim_end_matches(['\r', '\n']);
        let width = if clean.is_ascii() {
            clean.len()
        } else {
            clean.chars().count()
        };
        summary.width = summary.width.max(width);
        if matches!(clean.as_bytes().last(), Some(b'{' | b'(' | b'[' | b':')) {
            let indent = clean
                .bytes()
                .take_while(|&byte| byte == b' ' || byte == b'\t')
                .count();
            summary.scopes.push((offset, indent));
        }
        return Some(());
    }
    let eol = line
        .chars_at(line.len_chars())
        .reversed()
        .take_while(|&ch| ch == '\r' || ch == '\n')
        .count();
    let width = line.len_chars() - eol;
    summary.width = summary.width.max(width);
    {
        // Rope chunks end at UTF-8 boundaries. Count words across those
        // boundaries and carry the tiny ASCII marker token, not the line.
        let mut in_word = false;
        let mut marker = [0; 5];
        let mut length = 0usize;
        let finish = |length: usize, marker: &[u8; 5], summary: &mut Summary| match (length, marker)
        {
            (3, [b'B', b'U', b'G', ..]) | (5, b"FIXME") => summary.errors += 1,
            (4, [b'T', b'O', b'D', b'O', _]) | (4, [b'H', b'A', b'C', b'K', _]) => {
                summary.warnings += 1
            }
            _ => {}
        };
        for chunk in line.chunks() {
            if stopped() {
                return None;
            }
            let first_word = chunk.chars().next().is_some_and(|ch| !ch.is_whitespace());
            summary.words += chunk.split_whitespace().count() - usize::from(in_word && first_word);
            in_word = chunk
                .chars()
                .next_back()
                .is_some_and(|ch| !ch.is_whitespace());
            for byte in chunk.bytes() {
                if super::is_marker_word_char(byte) {
                    if length < marker.len() {
                        marker[length] = byte;
                    }
                    length = (length + 1).min(marker.len() + 1);
                } else {
                    finish(length, &marker, summary);
                    length = 0;
                }
            }
        }
        finish(length, &marker, summary);
    }
    if width > 0 && matches!(line.char(width - 1), '{' | '(' | '[' | ':') {
        let mut indent = 0;
        for chunk in line.chunks() {
            if stopped() {
                return None;
            }
            let count = chunk
                .bytes()
                .take_while(|&byte| byte == b' ' || byte == b'\t')
                .count();
            indent += count;
            if count < chunk.len() {
                break;
            }
        }
        summary.scopes.push((offset, indent));
    }
    Some(())
}

#[cfg(test)]
#[path = "editor_content_analysis_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "editor_content_cache_budget_tests.rs"]
mod budget_tests;
