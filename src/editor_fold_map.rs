//! Compact source/visible-row conversion. The index contains one entry per
//! hidden interval, never one entry per source line, and is rebuilt only when
//! folds change. Keeping invalidation with the map also covers restored views.
use std::{
    cell::OnceCell,
    collections::{BTreeMap, btree_map},
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
struct HiddenRange {
    start: usize,
    end: usize,
    hidden_before: usize,
}

impl HiddenRange {
    fn header_row(&self) -> usize {
        self.start - 1 - self.hidden_before
    }

    fn hidden_through_end(&self) -> usize {
        self.hidden_before + (self.end - self.start)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct FoldedRanges {
    ranges: BTreeMap<usize, usize>,
    index: OnceCell<Vec<HiddenRange>>,
    revision: u64,
}

impl FoldedRanges {
    fn index(&self) -> &[HiddenRange] {
        self.index.get_or_init(|| {
            let mut ranges: Vec<HiddenRange> = Vec::with_capacity(self.ranges.len());
            for (&header, &end) in &self.ranges {
                let start = header.saturating_add(1);
                if start >= end {
                    continue;
                }
                if let Some(last) = ranges.last_mut()
                    && start <= last.end
                {
                    last.end = last.end.max(end);
                    continue;
                }
                let hidden_before = ranges.last().map_or(0, HiddenRange::hidden_through_end);
                ranges.push(HiddenRange {
                    start,
                    end,
                    hidden_before,
                });
            }
            ranges
        })
    }

    pub(crate) fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) fn containing(&self, line: usize) -> Option<(usize, usize)> {
        let index = self.index();
        let end = index.partition_point(|range| range.start <= line);
        let range = end.checked_sub(1).map(|i| &index[i])?;
        (line < range.end).then_some((range.start - 1, range.end))
    }

    pub(crate) fn visible_count(&self, total: usize) -> usize {
        let index = self.index();
        let end = index.partition_point(|range| range.start < total);
        let hidden = end.checked_sub(1).map_or(0, |i| {
            let range = &index[i];
            range.hidden_before + (range.end.min(total) - range.start)
        });
        total.saturating_sub(hidden)
    }

    /// Hidden source lines share their visible fold header's row.
    pub(crate) fn row_for_line(&self, line: usize) -> usize {
        let index = self.index();
        let end = index.partition_point(|range| range.start <= line);
        end.checked_sub(1).map_or(line, |i| {
            let range = &index[i];
            if line < range.end {
                range.header_row()
            } else {
                line - range.hidden_through_end()
            }
        })
    }

    /// Also maps the boundary after the last visible row to the source EOF.
    pub(crate) fn line_for_row(&self, row: usize, total: usize) -> usize {
        let index = self.index();
        let end = index.partition_point(|range| range.header_row() < row);
        end.checked_sub(1).map_or(row.min(total), |i| {
            row.saturating_add(index[i].hidden_through_end()).min(total)
        })
    }
}

impl Deref for FoldedRanges {
    type Target = BTreeMap<usize, usize>;

    fn deref(&self) -> &Self::Target {
        &self.ranges
    }
}

impl DerefMut for FoldedRanges {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.index.take();
        self.revision = NEXT_REVISION.fetch_add(1, Ordering::Relaxed);
        &mut self.ranges
    }
}

impl<'a> IntoIterator for &'a FoldedRanges {
    type Item = (&'a usize, &'a usize);
    type IntoIter = btree_map::Iter<'a, usize, usize>;

    fn into_iter(self) -> Self::IntoIter {
        self.ranges.iter()
    }
}

impl PartialEq for FoldedRanges {
    fn eq(&self, other: &Self) -> bool {
        self.ranges == other.ranges
    }
}

impl Eq for FoldedRanges {}

impl PartialEq<BTreeMap<usize, usize>> for FoldedRanges {
    fn eq(&self, other: &BTreeMap<usize, usize>) -> bool {
        self.ranges == *other
    }
}
