//! Change kinds and Git sources stay independent through overview projection.

use similar::DiffOp;
use std::borrow::Cow;

/// An insertion can slide across identical lines without changing the diff.
/// Prefer the actual edited rows in that case, preserving the order of the
/// remaining equal rows so staged markers project to the right coordinates.
pub(crate) fn anchor_insertions<'a>(
    ops: &'a [DiffOp],
    anchors: &[usize],
    mut same_line: impl FnMut(usize, usize) -> Option<bool>,
) -> Option<Cow<'a, [DiffOp]>> {
    if anchors.is_empty() || !ops.iter().any(|op| matches!(op, DiffOp::Insert { .. })) {
        return Some(Cow::Borrowed(ops));
    }
    let mut aligned = Vec::with_capacity(ops.len());
    let mut remaining = ops.iter().copied().peekable();
    while let Some(op) = remaining.next() {
        let DiffOp::Insert { old_index, new_index, new_len } = op else {
            aligned.push(op);
            continue;
        };
        let (right_old, right) = match remaining.peek() {
            Some(DiffOp::Equal { old_index, len, .. }) => (*old_index, *len),
            _ => (old_index, 0),
        };
        // A compacted insertion's zero-length old range can use a different
        // boundary from its neighbors. The equal ops own the old row indices.
        let (old_start, left) = match aligned.last() {
            Some(DiffOp::Equal { old_index, len, .. }) => (*old_index, *len),
            _ => (right_old, 0),
        };
        let start = new_index - left;
        let end = new_index + new_len + right;
        let preferred = &anchors[anchors.partition_point(|&row| row < start)
            ..anchors.partition_point(|&row| row < end)];
        let added = new_index..new_index + new_len;
        if preferred.iter().all(|row| added.contains(row)) {
            aligned.push(op);
            continue;
        }

        // Move only within a run of identical rows. An unrelated insertion
        // hint cannot change which text the diff considers added or modified.
        let mut run = added.clone();
        for row in added.start + 1..added.end {
            if !same_line(added.start, row)? {
                run.end = added.start;
                break;
            }
        }
        if run.is_empty() {
            aligned.push(op);
            continue;
        }
        while run.start > preferred[0] && same_line(added.start, run.start - 1)? {
            run.start -= 1;
        }
        while run.end <= preferred[preferred.len() - 1] && same_line(added.start, run.end)? {
            run.end += 1;
        }
        let mut rows: Vec<_> = preferred.iter().copied().filter(|row| run.contains(row))
            .take(new_len).collect();
        if rows.iter().all(|row| added.contains(row)) {
            aligned.push(op);
            continue;
        }
        // Keep unanchored additions as ranges even when the insertion is
        // large. Default to the first unclaimed rows of the original op.
        let mut default_end = added.start + new_len - rows.len();
        for &row in &rows {
            if (added.start..default_end).contains(&row) { default_end += 1; }
        }
        let defaults = added.start..default_end;
        rows.retain(|row| !defaults.contains(row));
        let mut ranges: Vec<_> = rows.chunk_by(|a, b| *a + 1 == *b)
            .map(|group| group[0]..group[group.len() - 1] + 1).collect();
        if !defaults.is_empty() {
            let index = ranges.partition_point(|range| range.start < defaults.start);
            ranges.insert(index, defaults);
        }

        if left != 0 { aligned.pop(); }
        if right != 0 { remaining.next(); }
        let mut old = old_start;
        let mut new = start;
        for range in ranges {
            let len = range.start - new;
            if len != 0 {
                aligned.push(DiffOp::Equal { old_index: old, new_index: new, len });
                old += len;
                new += len;
            }
            aligned.push(DiffOp::Insert { old_index: old, new_index: new, new_len: range.len() });
            new = range.end;
        }
        if new < end {
            aligned.push(DiffOp::Equal { old_index: old, new_index: new, len: end - new });
        }
    }
    Some(Cow::Owned(aligned))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ChangeKind {
    Added,
    Removed,
    Replaced,
    Modified,
    Unsaved,
}

impl ChangeKind {
    pub(crate) const fn symbol(self) -> &'static str {
        match self {
            Self::Added => "+",
            Self::Removed => "−",
            Self::Replaced => "±",
            Self::Modified | Self::Unsaved => "┃",
        }
    }

    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::Added => "added lines",
            Self::Removed => "removed lines",
            Self::Replaced => "added and removed lines",
            Self::Modified => "modified lines",
            Self::Unsaved => "unsaved changes",
        }
    }
}

/// Each Git source has its own set of kinds. A range can be combined with OR
/// without dropping a deletion that shares a cell with an addition.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct LineChanges(u16);

impl LineChanges {
    const ADDED: u16 = 1;
    const REMOVED: u16 = 2;
    const MODIFIED: u16 = 4;
    const KIND_MASK: u16 = Self::ADDED | Self::REMOVED | Self::MODIFIED;
    const STAGED_SHIFT: u32 = 3;
    pub(crate) const BIT_COUNT: usize = 9;
    pub(crate) const UNSAVED: Self = Self(1 << 6);
    pub(crate) const UNSAVED_ADDED: Self = Self(1 << 7);
    pub(crate) const UNSAVED_REMOVED: Self = Self(1 << 8);

    pub(crate) const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    pub(crate) const fn bits(self) -> u16 {
        self.0
    }

    pub(crate) const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub(crate) const fn merge(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    const fn kind_bits(marker: char) -> u16 {
        match marker {
            'A' => Self::ADDED,
            'D' => Self::REMOVED,
            'M' => Self::ADDED | Self::REMOVED,
            'U' | 'S' => Self::MODIFIED,
            _ => 0,
        }
    }

    pub(crate) const fn git(staged: char, unstaged: char) -> Self {
        Self(Self::kind_bits(staged) << Self::STAGED_SHIFT | Self::kind_bits(unstaged))
    }

    const fn kind(bits: u16) -> Option<ChangeKind> {
        match bits & (Self::ADDED | Self::REMOVED) {
            Self::ADDED => Some(ChangeKind::Added),
            Self::REMOVED => Some(ChangeKind::Removed),
            0 if bits & Self::MODIFIED != 0 => Some(ChangeKind::Modified),
            0 => None,
            _ => Some(ChangeKind::Replaced),
        }
    }

    pub(crate) const fn staged(self) -> Option<ChangeKind> {
        Self::kind((self.0 >> Self::STAGED_SHIFT) & Self::KIND_MASK)
    }

    pub(crate) const fn unstaged(self) -> Option<ChangeKind> {
        Self::kind(self.0 & Self::KIND_MASK)
    }

    pub(crate) const fn unsaved(self) -> bool {
        self.0 & (Self::UNSAVED.0 | Self::UNSAVED_ADDED.0 | Self::UNSAVED_REMOVED.0) != 0
    }

    pub(crate) const fn summary(self) -> Option<ChangeKind> {
        let git = Self::kind((self.0 | (self.0 >> Self::STAGED_SHIFT)) & Self::KIND_MASK);
        if git.is_some() { git } else { self.working() }
    }

    pub(crate) const fn working(self) -> Option<ChangeKind> {
        if self.0 & Self::UNSAVED.0 != 0 {
            Some(ChangeKind::Unsaved)
        } else if self.0 & Self::UNSAVED_ADDED.0 != 0 {
            Some(ChangeKind::Added)
        } else if self.0 & Self::UNSAVED_REMOVED.0 != 0 {
            Some(ChangeKind::Removed)
        } else {
            self.unstaged()
        }
    }

    pub(crate) fn primary(self) -> Option<ChangeKind> {
        self.working().or(self.staged())
    }
}

impl From<char> for LineChanges {
    fn from(marker: char) -> Self {
        match marker {
            'N' => Self::UNSAVED,
            'S' => Self::git(marker, ' '),
            _ => Self::git(' ', marker),
        }
    }
}
