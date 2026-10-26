//! Reuse plans for immutable per-block analyses of a Rope. Text equality is
//! exact, with shared Rope leaves skipped by the common changed-range helper.
use ropey::Rope;
use std::{ops::Range, sync::Arc};

pub(crate) const BLOCK_LINES: usize = 128;

pub(crate) struct Block<T> {
    pub(crate) start: usize,
    pub(crate) lines: usize,
    pub(crate) summary: Arc<T>,
}

impl<T> Clone for Block<T> {
    fn clone(&self) -> Self {
        Self {
            start: self.start,
            lines: self.lines,
            summary: self.summary.clone(),
        }
    }
}

pub(crate) struct Plan {
    pub(crate) prefix: usize,
    pub(crate) suffix: usize,
    pub(crate) rescan: Range<usize>,
}

pub(crate) fn plan<T>(
    blocks: &[Block<T>],
    old: &Rope,
    new: &Rope,
    stopped: &impl Fn() -> bool,
) -> Option<Plan> {
    let (before, after) = crate::rope_changes::changed_lines(old, new, stopped)?;
    if before.is_empty() && after.is_empty() {
        return Some(Plan {
            prefix: blocks.len(),
            suffix: blocks.len(),
            rescan: new.len_lines()..new.len_lines(),
        });
    }
    let mut prefix = blocks.partition_point(|block| block.start + block.lines <= before.start);
    // Equal newlines can move the minimal changed range past a previously
    // short prefix block. Absorb that block as well as short suffix blocks.
    while prefix > 0 && blocks[prefix - 1].lines < BLOCK_LINES {
        prefix -= 1;
    }
    let mut suffix = blocks
        .partition_point(|block| block.start < before.end)
        .max(prefix);
    // Absorb short adjacent blocks left by insertions. Otherwise a repeated
    // newline edit could retain one tiny allocation per keystroke indefinitely.
    while blocks
        .get(suffix)
        .is_some_and(|block| block.lines < BLOCK_LINES)
    {
        suffix += 1;
    }
    let start = prefix
        .checked_sub(1)
        .map_or(0, |i| blocks[i].start + blocks[i].lines);
    let end = blocks.get(suffix).map_or(new.len_lines(), |block| {
        after.end + block.start - before.end
    });
    Some(Plan {
        prefix,
        suffix,
        rescan: start..end,
    })
}
