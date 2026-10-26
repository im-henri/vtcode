//! Terminal-cell widths of the expanded tree, cached between tree updates.
use std::cell::Cell;
use unicode_width::UnicodeWidthStr;
use super::{FileEntry, FileExplorer, GitStatus};

#[derive(Clone, Copy)]
pub(crate) enum GitBadgeVisibility { None, Tracked, All }

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
    pub(super) fn invalidate(&self) { self.measured.set([None; 3]); }
}

impl FileEntry {
    pub(crate) fn prefix_width(&self) -> usize {
        1usize.saturating_add(self.depth.saturating_mul(2)).saturating_add(
            if self.is_dir { 2 } else { UnicodeWidthStr::width(crate::file_icons::for_name(&self.name).0) })
    }
}

impl FileExplorer {
    pub(crate) fn indexed_row_width(&self, path: &std::path::Path, visibility: GitBadgeVisibility) -> Option<usize> {
        // Rendering reads an indexed snapshot. Most dirty documents may be
        // outside its expanded tree: an absent key must not scan every entry.
        // Keep find_path's general stale-index fallback for navigation callers.
        if !self.path_indices.contains_key(&Self::path_key(path))
            && self.parent_indices.len() == self.entries.len() { return None; }
        self.find_path(path).map(|index| self.row_width(&self.entries[index], visibility))
    }

    pub(crate) fn row_width(&self, entry: &FileEntry, visibility: GitBadgeVisibility) -> usize {
        let badges = if matches!(visibility, GitBadgeVisibility::None) { 0 } else if entry.is_dir {
            entry.git_status_summary.statuses().filter(|status| visibility.shows(*status)).count()
        } else { usize::from(visibility.shows(entry.git_status)) };
        let mut trailing = badges.saturating_mul(2).saturating_sub(1);
        if entry.is_dir && !self.find.filter
            && let Some(found) = self.find.results.rows.get(&entry.path).filter(|found| found.descendants > 0)
        {
            trailing += usize::from(trailing > 0) + found.descendants.ilog10() as usize + 1;
        }
        entry.prefix_width().saturating_add(UnicodeWidthStr::width(entry.name.as_str()))
            .saturating_add(trailing).saturating_add(usize::from(trailing > 0))
    }

    pub(crate) fn content_width(&self, visibility: GitBadgeVisibility) -> usize {
        let key = (self.entries.as_ptr() as usize, self.entries.len());
        let mut measured = self.horizontal.measured.get();
        let cached = &mut measured[visibility as usize];
        if let Some((ptr, len, width)) = *cached
            && (ptr, len) == key { return width; }
        let width = self.entries.iter().map(|entry| self.row_width(entry, visibility)).max().unwrap_or(0);
        *cached = Some((key.0, key.1, width));
        self.horizontal.measured.set(measured);
        width
    }
}
