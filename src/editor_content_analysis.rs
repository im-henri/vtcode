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
    // Relative positions only, never absolute ancestry: an earlier edit can
    // change it even when this block's text is unchanged. Scopes closed inside
    // the block carry their end; the rest are closed by later blocks.
    scopes: Vec<LocalScope>,
    // Non-blank lines indented less than every earlier line of the block, with
    // the block's previous non-blank line. Only these can close scopes opened
    // in earlier blocks, so merging stays proportional to blocks and scopes.
    minima: Vec<(usize, usize, Option<usize>)>,
    last_nonblank: Option<usize>,
}

struct LocalScope {
    offset: usize,
    indent: usize,
    /// Index of the enclosing scope in this block, if it opened here too.
    parent: Option<usize>,
    /// Last non-blank body line, when the scope closes inside this block.
    end: Option<usize>,
}

/// Per-block scope bookkeeping while its lines are summarized.
#[derive(Default)]
struct ScopeScan {
    open: Vec<usize>,
    min_indent: Option<usize>,
}

impl ScopeScan {
    fn line(&mut self, summary: &mut Summary, offset: usize, indent: usize, opener: bool) {
        while let Some(&index) = self.open.last() {
            if summary.scopes[index].indent < indent {
                break;
            }
            summary.scopes[index].end = summary.last_nonblank;
            self.open.pop();
        }
        if self.min_indent.is_none_or(|min| indent < min) {
            summary.minima.push((offset, indent, summary.last_nonblank));
            self.min_indent = Some(indent);
        }
        if opener {
            summary.scopes.push(LocalScope {
                offset,
                indent,
                parent: self.open.last().copied(),
                end: None,
            });
            self.open.push(summary.scopes.len() - 1);
        }
        summary.last_nonblank = Some(offset);
    }

    /// Openers closed by their very next line have no body. They cannot
    /// enclose anything, so drop them instead of storing them per block.
    fn finish(self, summary: &mut Summary) {
        let mut renumbered = vec![usize::MAX; summary.scopes.len()];
        let mut kept = 0;
        for index in 0..summary.scopes.len() {
            let scope = &summary.scopes[index];
            if scope.end.is_some_and(|end| end <= scope.offset) {
                continue;
            }
            renumbered[index] = kept;
            let parent = scope.parent.map(|parent| renumbered[parent]);
            summary.scopes.swap(kept, index);
            summary.scopes[kept].parent = parent;
            kept += 1;
        }
        summary.scopes.truncate(kept);
        summary.scopes.shrink_to_fit();
        summary.minima.shrink_to_fit();
    }
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
            let mut scan = ScopeScan::default();
            for (offset, line) in lines.by_ref().take(count).enumerate() {
                // A block bounds deadline polling for ordinary short lines;
                // long lines also check it between Rope chunks below.
                if cancelled() {
                    return None;
                }
                summarize_line(line, offset, &mut summary, &mut scan, &stopped)?;
                #[cfg(test)]
                {
                    self.scanned_lines += 1;
                }
            }
            scan.finish(&mut summary);
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
        // `end` stays usize::MAX while a scope is open across blocks.
        let mut scopes: Vec<StickyScopeLine> = Vec::new();
        let mut open: Vec<(usize, usize)> = Vec::new();
        let mut last_nonblank: Option<usize> = None;
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
                + summary.scopes.capacity() * std::mem::size_of::<LocalScope>()
                + summary.minima.capacity() * std::mem::size_of::<(usize, usize, Option<usize>)>();
            let base = scopes.len();
            let mut minima = summary.minima.iter().peekable();
            let mut close = |open: &mut Vec<(usize, usize)>,
                             scopes: &mut Vec<StickyScopeLine>,
                             last_nonblank: Option<usize>,
                             upto: usize| {
                while let Some(&&(offset, indent, previous)) = minima.peek() {
                    if offset > upto {
                        break;
                    }
                    minima.next();
                    let end = previous.map(|line| block.start + line).or(last_nonblank);
                    while open
                        .last()
                        .is_some_and(|&(open_indent, _)| open_indent >= indent)
                    {
                        let (_, index) = open.pop().unwrap();
                        scopes[index].end = end.unwrap_or(scopes[index].line);
                    }
                }
            };
            for scope in &summary.scopes {
                close(&mut open, &mut scopes, last_nonblank, scope.offset);
                let parent = match scope.parent {
                    Some(local) => Some(base + local),
                    None => open.last().map(|&(_, index)| index),
                };
                scopes.push(StickyScopeLine {
                    line: block.start + scope.offset,
                    parent,
                    end: scope.end.map_or(usize::MAX, |end| block.start + end),
                });
                if scope.end.is_none() {
                    open.push((scope.indent, scopes.len() - 1));
                }
            }
            close(&mut open, &mut scopes, last_nonblank, usize::MAX);
            if let Some(line) = summary.last_nonblank {
                last_nonblank = Some(block.start + line);
            }
        }
        for (_, index) in open {
            scopes[index].end = last_nonblank.unwrap_or(scopes[index].line);
        }
        let scopes = crate::background::drop_empty_scopes(scopes);
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
    scan: &mut ScopeScan,
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
        if !clean.trim_start_matches([' ', '\t']).is_empty() {
            let opener = matches!(clean.as_bytes().last(), Some(b'{' | b'(' | b'[' | b':'));
            let indent = crate::background::scope_indent(clean.bytes());
            scan.line(summary, offset, indent, opener);
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
    // Leading whitespace may span Rope chunks; it ends at the first other byte.
    let mut leading = line
        .chunks()
        .flat_map(str::bytes)
        .take(width.min(1 << 20))
        .take_while(|&byte| byte == b' ' || byte == b'\t');
    let indent = crate::background::scope_indent(leading.by_ref());
    let blank = line.chars().take(width).all(|ch| ch == ' ' || ch == '\t');
    if !blank {
        let opener = width > 0 && matches!(line.char(width - 1), '{' | '(' | '[' | ':');
        scan.line(summary, offset, indent, opener);
    }
    if stopped() {
        return None;
    }
    Some(())
}

#[cfg(test)]
#[path = "editor_content_analysis_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "editor_content_cache_budget_tests.rs"]
mod budget_tests;
