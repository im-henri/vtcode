//! Terminal-cell widths of the expanded tree, cached between tree updates.
use super::{FileEntry, FileExplorer, GitStatus};
use std::cell::Cell;
use unicode_width::UnicodeWidthStr;

#[derive(Clone, Copy)]
pub(crate) enum GitBadgeVisibility {
    None,
    Tracked,
    All,
}

impl GitBadgeVisibility {
    pub(crate) fn color_status(self, status: GitStatus) -> GitStatus {
        match (self, status) {
            (Self::None, _) | (Self::Tracked, GitStatus::Untracked) => GitStatus::Unmodified,
            _ => status,
        }
    }

    pub(crate) fn shows(self, status: GitStatus) -> bool {
        match status {
            GitStatus::Unmodified | GitStatus::Ignored => false,
            GitStatus::Untracked => matches!(self, Self::All),
            _ => !matches!(self, Self::None),
        }
    }
}

#[derive(Default)]
pub(crate) struct HorizontalScroll {
    pub offset: usize,
    // Structural updates invalidate explicitly, even if the vector reuses its
    // allocation. The identity check also handles replaced/truncated snapshots.
    // Keep each decoration mode, so repeatedly toggling visibility need not
    // rescan a large expanded tree. Neither cache owns paths or Git statuses.
    measured: Cell<[Option<(usize, usize, usize)>; 3]>,
}

impl HorizontalScroll {
    pub(super) fn invalidate(&self) {
        self.measured.set([None; 3]);
    }

    pub(super) fn install_widths(&self, entries: &[FileEntry], widths: [usize; 3]) {
        self.measured
            .set(widths.map(|width| Some((entries.as_ptr() as usize, entries.len(), width))));
    }

    pub(super) fn update_widths(&self, before: [usize; 3], after: [usize; 3]) {
        let mut measured = self.measured.get();
        for ((cached, before), after) in measured.iter_mut().zip(before).zip(after) {
            if let Some((_, _, width)) = cached {
                if *width == before && after < before {
                    *cached = None;
                } else {
                    *width = (*width).max(after);
                }
            }
        }
        self.measured.set(measured);
    }
}

impl FileEntry {
    pub(crate) fn prefix_width(&self) -> usize {
        1usize
            .saturating_add(self.depth.saturating_mul(2))
            .saturating_add(if self.is_dir {
                2
            } else {
                UnicodeWidthStr::width(crate::file_icons::for_name(&self.name).0)
            })
    }
}

fn trailing_width(entry: &FileEntry, descendants: usize, visibility: GitBadgeVisibility) -> usize {
    let badges = if matches!(visibility, GitBadgeVisibility::None) {
        0
    } else if entry.is_dir {
        entry
            .git_status_summary
            .statuses()
            .filter(|status| visibility.shows(*status))
            .count()
    } else {
        usize::from(visibility.shows(entry.git_status))
    };
    let mut width = badges.saturating_mul(2).saturating_sub(1);
    if descendants > 0 {
        width += usize::from(width > 0) + descendants.ilog10() as usize + 1;
    }
    width + usize::from(width > 0)
}

fn match_count(
    entry: &FileEntry,
    matches: &crate::explorer_find::Matches,
    show_counts: bool,
) -> usize {
    if entry.is_dir && show_counts {
        matches
            .rows
            .get(&entry.path)
            .map_or(0, |found| found.descendants)
    } else {
        0
    }
}

pub(super) fn measure_rows(
    entries: &[FileEntry],
    matches: &crate::explorer_find::Matches,
    show_counts: bool,
) -> [usize; 3] {
    let mut widths = [0; 3];
    for entry in entries {
        for (width, row_width) in widths
            .iter_mut()
            .zip(measure_row(entry, matches, show_counts))
        {
            *width = (*width).max(row_width);
        }
    }
    widths
}

pub(super) fn measure_row(
    entry: &FileEntry,
    matches: &crate::explorer_find::Matches,
    show_counts: bool,
) -> [usize; 3] {
    let base = entry.prefix_width() + UnicodeWidthStr::width(entry.name.as_str());
    let descendants = match_count(entry, matches, show_counts);
    [
        GitBadgeVisibility::None,
        GitBadgeVisibility::Tracked,
        GitBadgeVisibility::All,
    ]
    .map(|visibility| base + trailing_width(entry, descendants, visibility))
}

impl FileExplorer {
    pub(crate) fn indexed_row_width(
        &self,
        path: &std::path::Path,
        visibility: GitBadgeVisibility,
    ) -> Option<usize> {
        self.indexed_position(path)
            .map(|index| self.row_width(&self.entries[index], visibility))
    }

    /// The row showing `path`, for rendering. Rendering reads an indexed
    /// snapshot. Most open documents may be outside its expanded tree: an
    /// absent key must not scan every entry. Keep find_path's general
    /// stale-index fallback for navigation callers.
    pub(crate) fn indexed_position(&self, path: &std::path::Path) -> Option<usize> {
        if !self.indices.paths.contains_key(&Self::path_key(path))
            && self.indices.parents.len() == self.entries.len()
        {
            return None;
        }
        self.find_path(path)
    }

    pub(crate) fn row_width(&self, entry: &FileEntry, visibility: GitBadgeVisibility) -> usize {
        let descendants = match_count(entry, &self.find.results, !self.find.filter);
        entry
            .prefix_width()
            .saturating_add(UnicodeWidthStr::width(entry.name.as_str()))
            .saturating_add(trailing_width(entry, descendants, visibility))
    }

    pub(crate) fn content_width(&self, visibility: GitBadgeVisibility) -> usize {
        let key = (self.entries.as_ptr() as usize, self.entries.len());
        let mut measured = self.horizontal.measured.get();
        let cached = &mut measured[visibility as usize];
        if let Some((ptr, len, width)) = *cached
            && (ptr, len) == key
        {
            return width;
        }
        let width = self
            .entries
            .iter()
            .map(|entry| self.row_width(entry, visibility))
            .max()
            .unwrap_or(0);
        *cached = Some((key.0, key.1, width));
        self.horizontal.measured.set(measured);
        width
    }
}
