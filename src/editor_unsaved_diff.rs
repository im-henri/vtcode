//! Exact unsaved-change ranges prepared on the analysis worker. Storage follows
//! actual change runs, not every line in the document or a large pasted block.
use ropey::Rope;
use std::{borrow::Cow, ops::Range, time::Instant};

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct UnsavedDiff {
    pub line_count: usize,
    /// Sorted, disjoint, nonempty current-line ranges. Adjacent runs are merged.
    pub changed_ranges: Vec<Range<usize>>,
}

impl UnsavedDiff {
    pub(crate) fn line_status(&self, line: usize) -> char {
        if line >= self.line_count {
            return ' ';
        }
        let index = self
            .changed_ranges
            .partition_point(|range| range.end <= line);
        if self
            .changed_ranges
            .get(index)
            .is_some_and(|range| range.start <= line)
        {
            'N'
        } else {
            ' '
        }
    }

    pub(crate) fn has_change_in_range(&self, start: usize, end: usize) -> bool {
        if start >= end || start >= self.line_count {
            return false;
        }
        let index = self
            .changed_ranges
            .partition_point(|range| range.end <= start);
        self.changed_ranges
            .get(index)
            .is_some_and(|range| range.start < end)
    }
}

pub(crate) fn compute_unsaved_diff_markers_cancellable(
    saved: &Rope,
    current: &Rope,
    deadline: Option<Instant>,
    cancelled: &impl Fn() -> bool,
) -> Option<UnsavedDiff> {
    let stop = || cancelled() || deadline.is_some_and(|limit| Instant::now() >= limit);
    // Most edits share every Rope leaf except the local edit. Only materialize
    // and diff changed lines; the equal prefix/suffix never enter the diff engine.
    let (old_range, new_range) = crate::rope_changes::changed_lines(saved, current, &stop)?;
    let entire_change = || UnsavedDiff {
        line_count: current.len_lines(),
        changed_ranges: if new_range.is_empty() {
            Vec::new()
        } else {
            vec![new_range.clone()]
        },
    };
    if stop() {
        return None;
    }
    // Pure insertion/deletion cannot have an equal line inside the changed
    // window. There is no reason to materialize its text or run an edit search.
    if old_range.is_empty() || new_range.is_empty() {
        return Some(entire_change());
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
                Some(match line.as_str() {
                    Some(text) => Cow::Borrowed(text),
                    None => Cow::Owned(line.to_string()),
                })
            })
            .collect()
    }
    let old_lines = lines(saved, old_range, &stop)?;
    let new_lines = lines(current, new_range.clone(), &stop)?;
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
            return Some(entire_change());
        }
    }
    let old_refs: Vec<_> = old_lines.iter().map(Cow::as_ref).collect();
    let new_refs: Vec<_> = new_lines.iter().map(Cow::as_ref).collect();
    if stop() {
        return None;
    }
    let mut config = similar::TextDiff::configure();
    if let Some(deadline) = deadline {
        config.deadline(deadline);
    }
    let diff = config.diff_slices(&old_refs, &new_refs);
    // A deadline can produce an approximate diff. Never install that as exact
    // change data, or cache a cancelled partial result.
    if stop() {
        return None;
    }
    let mut changed_ranges: Vec<Range<usize>> = Vec::new();
    for op in diff.ops() {
        if stop() {
            return None;
        }
        if matches!(
            op,
            similar::DiffOp::Insert { .. } | similar::DiffOp::Replace { .. }
        ) {
            let range = op.new_range();
            if range.is_empty() {
                continue;
            }
            let range = new_range.start + range.start..new_range.start + range.end;
            if let Some(last) = changed_ranges.last_mut()
                && last.end == range.start
            {
                last.end = range.end;
            } else {
                changed_ranges.push(range);
            }
        }
    }
    Some(UnsavedDiff {
        line_count: current.len_lines(),
        changed_ranges,
    })
}
