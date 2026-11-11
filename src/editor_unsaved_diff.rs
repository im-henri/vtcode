//! Exact unsaved-change ranges for the current buffer. Storage follows
//! actual change runs, not every line in the document or a large pasted block.
use ropey::{Rope, RopeSlice};
use similar::DiffOp;
use std::{borrow::Cow, cell::Cell, ops::Range, sync::Arc, time::Instant};

use crate::line_changes::LineChanges;

/// Rows created by line insertion commands in an immutable document snapshot.
/// These are tie-breakers for equivalent diffs, never a source of markers.
#[derive(Clone)]
pub(crate) struct InsertionAnchors {
    snapshot: Rope,
    rows: Arc<[usize]>,
}

impl InsertionAnchors {
    pub(crate) fn rows(&self, snapshot: &Rope) -> &[usize] {
        if self.snapshot.is_instance(snapshot) { &self.rows } else { &[] }
    }

    pub(crate) fn rebase(mut self, current: &Rope) -> Option<Self> {
        if self.snapshot.is_instance(current) {
            return Some(self);
        }
        if self.snapshot.len_lines() != current.len_lines() {
            // A generic multiline replacement has no exact row identity
            // inside its edited window. Retain the proven prefix and suffix.
            let remaining = Cell::new(2_048usize);
            let (old, new) = crate::rope_changes::changed_lines(&self.snapshot, current, &|| {
                let work = remaining.get();
                remaining.set(work.saturating_sub(1));
                work == 0
            })?;
            self.rows = self.rows.iter().filter_map(|&row| {
                if row < old.start { Some(row) }
                else if row >= old.end { Some(new.end + (row - old.end)) }
                else { None }
            }).collect();
        }
        if self.rows.is_empty() {
            return None;
        }
        self.snapshot = current.clone();
        Some(self)
    }

    pub(crate) fn insert(previous: Option<Self>, before: &Rope, after: &Rope, row: usize) -> Option<Self> {
        let count = after.len_lines().saturating_sub(before.len_lines());
        if count == 0 {
            return previous;
        }
        let previous = previous.and_then(|anchors| anchors.rebase(before));
        let rows = previous.as_ref().map_or(&[][..], |anchors| &anchors.rows);
        let split = rows.partition_point(|&existing| existing < row);
        let rows = rows[..split].iter().copied().chain(row..row + count)
            .chain(rows[split..].iter().map(|&existing| existing + count)).collect();
        Some(Self { snapshot: after.clone(), rows })
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct UnsavedDiff {
    pub line_count: usize,
    /// Sorted, disjoint, nonempty current-line ranges. Adjacent runs are merged.
    pub changed_ranges: Vec<Range<usize>>,
    /// Insertions within the changed ranges, in current-line coordinates.
    pub added_ranges: Vec<Range<usize>>,
    /// Deletions anchored to the following line, or the last row at EOF.
    pub removed_lines: Vec<usize>,
}

impl UnsavedDiff {
    #[cfg(test)]
    pub(crate) fn line_status(&self, line: usize) -> char {
        if self.line_changes(line).unsaved() { 'N' } else { ' ' }
    }

    pub(crate) fn line_changes(&self, line: usize) -> LineChanges {
        if line >= self.line_count {
            return LineChanges::default();
        }
        let removed = self.removed_lines.binary_search(&line).is_ok();
        match (intersects(&self.changed_ranges, line, line + 1), removed) {
            (false, false) => LineChanges::default(),
            (false, true) => LineChanges::UNSAVED_REMOVED,
            (true, false) if intersects(&self.added_ranges, line, line + 1) => LineChanges::UNSAVED_ADDED,
            (true, _) => LineChanges::UNSAVED,
        }
    }

    pub(crate) fn has_change_in_range(&self, start: usize, end: usize) -> bool {
        if start >= end || start >= self.line_count {
            return false;
        }
        let removed = self.removed_lines.partition_point(|&line| line < start);
        intersects(&self.changed_ranges, start, end)
            || self.removed_lines.get(removed).is_some_and(|&line| line < end)
    }
}

fn intersects(ranges: &[Range<usize>], start: usize, end: usize) -> bool {
    let index = ranges.partition_point(|range| range.end <= start);
    ranges.get(index).is_some_and(|range| range.start < end)
}

fn line_content(line: RopeSlice<'_>) -> RopeSlice<'_> {
    line.slice(..super::Editor::line_content_len(line))
}

pub(crate) fn compute_live_unsaved_diff(saved: &Rope, current: &Rope, anchors: &[usize]) -> Option<UnsavedDiff> {
    // Bound the shared-prefix/suffix walk as well as the changed window.
    let remaining = Cell::new(2_048usize);
    compute(saved, current, anchors, None, &|| {
        let work = remaining.get();
        remaining.set(work.saturating_sub(1));
        work == 0
    }, true)
}

pub(crate) fn compute_unsaved_diff_markers_cancellable(
    saved: &Rope,
    current: &Rope,
    anchors: &[usize],
    deadline: Option<Instant>,
    cancelled: &impl Fn() -> bool,
) -> Option<UnsavedDiff> {
    compute(saved, current, anchors, deadline, cancelled, false)
}

fn compute(
    saved: &Rope,
    current: &Rope,
    anchors: &[usize],
    deadline: Option<Instant>,
    cancelled: &impl Fn() -> bool,
    live: bool,
) -> Option<UnsavedDiff> {
    let stop = || cancelled() || deadline.is_some_and(|limit| Instant::now() >= limit);
    // A local edit shares most Rope leaves with the saved snapshot. Only
    // materialize and diff the lines between the equal prefix and suffix.
    let (old_range, new_range) = crate::rope_changes::changed_lines(saved, current, &stop)?;
    // Match Git's treatment of line endings and of Ropey's empty EOF row.
    let file_lines = |rope: &Rope| {
        rope.len_lines() - usize::from(rope.line(rope.len_lines() - 1).len_bytes() == 0)
    };
    let old_count = file_lines(saved);
    let new_count = file_lines(current);
    let common = old_count.min(new_count);
    let mut first = old_range.start.min(common);
    let old_end = old_range.end.min(old_count);
    let new_end = new_range.end.min(new_count);
    if live && [(saved, old_end), (current, new_end)].iter().any(|&(rope, end)| {
        end - first > 128 || rope.line_to_byte(end) - rope.line_to_byte(first) > 64 * 1024
    }) {
        return None;
    }
    // A byte difference can be just a terminator or precede identical blank
    // lines. Anchor the diff on equal line contents before classifying edits.
    while first < common && (old_count != new_count || first < old_end || first < new_end) {
        if stop() {
            return None;
        }
        if line_content(saved.line(first)) != line_content(current.line(first)) {
            break;
        }
        first += 1;
    }
    let shared_suffix = (old_count - old_end).min(new_count - new_end).min(common - first);
    let mut old_end = old_count - shared_suffix;
    let mut new_end = new_count - shared_suffix;
    while first < old_end && first < new_end {
        if stop() {
            return None;
        }
        if line_content(saved.line(old_end - 1)) != line_content(current.line(new_end - 1)) {
            break;
        }
        old_end -= 1;
        new_end -= 1;
    }
    let old_range = first..old_end;
    let new_range = first..new_end;
    let mut ops = changed_ops(saved, current, old_range, new_range, deadline, &stop)?;
    if !anchors.is_empty() && ops.iter().any(|op| matches!(op, DiffOp::Insert { .. })) {
        // Include the clipped equal rows so insertions can be anchored within
        // identical lines on either side of the changed window.
        if first != 0 {
            ops.insert(0, DiffOp::Equal { old_index: 0, new_index: 0, len: first });
        }
        if new_end < new_count {
            ops.push(DiffOp::Equal { old_index: old_end, new_index: new_end, len: new_count - new_end });
        }
    }
    let ops = crate::line_changes::anchor_insertions(&ops, anchors, |a, b| {
        (!stop()).then(|| line_content(current.line(a)) == line_content(current.line(b)))
    })?;
    let mut result = UnsavedDiff { line_count: current.len_lines(), ..Default::default() };
    for op in ops.iter() {
        if stop() {
            return None;
        }
        match *op {
            DiffOp::Delete { new_index, .. } => {
                let line = new_index.min(new_count.saturating_sub(1));
                if result.removed_lines.last() != Some(&line) {
                    result.removed_lines.push(line);
                }
            }
            DiffOp::Insert { .. } | DiffOp::Replace { .. } => {
                let range = op.new_range();
                if matches!(op, DiffOp::Insert { .. }) {
                    result.added_ranges.push(range.clone());
                }
                if let Some(last) = result.changed_ranges.last_mut()
                    && last.end == range.start
                {
                    last.end = range.end;
                } else {
                    result.changed_ranges.push(range);
                }
            }
            DiffOp::Equal { .. } => {}
        }
    }
    Some(result)
}

fn changed_ops(
    saved: &Rope,
    current: &Rope,
    old_range: Range<usize>,
    new_range: Range<usize>,
    deadline: Option<Instant>,
    stop: &impl Fn() -> bool,
) -> Option<Vec<DiffOp>> {
    if stop() {
        return None;
    }
    let old_index = old_range.start;
    let new_index = new_range.start;
    let old_len = old_range.len();
    let new_len = new_range.len();
    match (old_len, new_len) {
        (0, 0) => return Some(Vec::new()),
        (0, _) => return Some(vec![DiffOp::Insert { old_index, new_index, new_len }]),
        (_, 0) => return Some(vec![DiffOp::Delete { old_index, new_index, old_len }]),
        _ => {}
    }
    fn lines<'a>(
        rope: &'a Rope,
        range: Range<usize>,
        stop: &impl Fn() -> bool,
    ) -> Option<Vec<Cow<'a, str>>> {
        rope.lines_at(range.start)
            .take(range.len())
            .map(|line| {
                if stop() {
                    return None;
                }
                let content = line_content(line);
                Some(match content.as_str() {
                    Some(text) => Cow::Borrowed(text),
                    None => Cow::Owned(content.to_string()),
                })
            })
            .collect()
    }
    let old_lines = lines(saved, old_range, stop)?;
    let new_lines = lines(current, new_range, stop)?;
    // Ordinary one-line edits and replacing one line with a pasted block need
    // no general diff if the single line has no exact counterpart. Myers can
    // otherwise spend seconds searching a highly unbalanced replacement.
    let single = if old_lines.len() == 1 {
        Some((&old_lines[0], &new_lines))
    } else if new_lines.len() == 1 {
        Some((&new_lines[0], &old_lines))
    } else {
        None
    };
    if let Some((needle, candidates)) = single {
        let mut found = false;
        for line in candidates {
            if stop() {
                return None;
            }
            if line == needle {
                found = true;
                break;
            }
        }
        if !found {
            return Some(vec![DiffOp::Replace { old_index, new_index, old_len, new_len }]);
        }
    }
    if stop() {
        return None;
    }
    let ops = similar::capture_diff_slices_deadline(similar::Algorithm::Myers, &old_lines, &new_lines, deadline);
    // A deadline can produce an approximate diff. Never install that as exact
    // change data, or cache a cancelled partial result.
    if stop() {
        return None;
    }
    Some(ops.into_iter().map(|op| match op {
        DiffOp::Equal { old_index: old, new_index: new, len } =>
            DiffOp::Equal { old_index: old_index + old, new_index: new_index + new, len },
        DiffOp::Delete { old_index: old, new_index: new, old_len } =>
            DiffOp::Delete { old_index: old_index + old, new_index: new_index + new, old_len },
        DiffOp::Insert { old_index: old, new_index: new, new_len } =>
            DiffOp::Insert { old_index: old_index + old, new_index: new_index + new, new_len },
        DiffOp::Replace { old_index: old, new_index: new, old_len, new_len } =>
            DiffOp::Replace { old_index: old_index + old, new_index: new_index + new, old_len, new_len },
    }).collect())
}
