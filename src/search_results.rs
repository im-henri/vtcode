//! Search result metadata is maintained when batches arrive or file groups collapse,
//! rather than rediscovered by every paint, click and keyboard navigation.
use crate::app::GlobalSearchResult;
use rustc_hash::FxHashMap;
use std::{
    ops::{Deref, Range},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SearchRow {
    Header(usize),
    Result(usize),
}

#[derive(Default)]
struct FileInfo {
    matches: usize,
    collapsed: bool,
}

struct Group {
    start: usize,
    end: usize,
    row: usize,
    collapsed: bool,
}

/// Read-only slice access keeps the index valid even when results are replaced.
/// Separate groups for noncontiguous occurrences of a path preserve arrival order.
#[derive(Default)]
pub(crate) struct SearchResults {
    results: Vec<GlobalSearchResult>,
    files: FxHashMap<PathBuf, FileInfo>,
    file_count: usize,
    groups: Vec<Group>,
    rows: usize,
}

impl SearchResults {
    pub(crate) fn clear(&mut self) {
        self.results.clear();
        self.files.clear();
        self.file_count = 0;
        self.groups.clear();
        self.rows = 0;
    }

    pub(crate) fn file_count(&self) -> usize {
        self.file_count
    }
    pub(crate) fn row_count(&self) -> usize {
        self.rows
    }

    pub(crate) fn file_matches(&self, path: &Path) -> usize {
        self.files.get(path).map_or(0, |file| file.matches)
    }

    pub(crate) fn is_collapsed(&self, path: &Path) -> bool {
        self.files.get(path).is_some_and(|file| file.collapsed)
    }

    pub(crate) fn all_collapsed(&self) -> bool {
        !self.results.is_empty() && self.rows == self.groups.len()
    }

    pub(crate) fn set_collapsed(&mut self, path: &Path, collapsed: bool) {
        let file = self.files.entry(path.to_path_buf()).or_default();
        if file.collapsed != collapsed {
            file.collapsed = collapsed;
            self.reindex_rows();
        }
    }

    pub(crate) fn set_all_collapsed(&mut self, collapsed: bool) {
        if self.files.values().any(|file| file.collapsed != collapsed) {
            for file in self.files.values_mut() {
                file.collapsed = collapsed;
            }
            self.reindex_rows();
        }
    }

    fn reindex_rows(&mut self) {
        self.rows = 0;
        for group in &mut self.groups {
            group.row = self.rows;
            group.collapsed = self.files[&self.results[group.start].file_path].collapsed;
            self.rows += 1 + if group.collapsed {
                0
            } else {
                group.end - group.start
            };
        }
    }

    pub(crate) fn row_at(&self, row: usize) -> Option<SearchRow> {
        if row >= self.rows {
            return None;
        }
        let group = &self.groups[self.groups.partition_point(|group| group.row <= row) - 1];
        Some(if row == group.row {
            SearchRow::Header(group.start)
        } else {
            SearchRow::Result(group.start + row - group.row - 1)
        })
    }

    pub(crate) fn visible_rows(
        &self,
        range: Range<usize>,
    ) -> impl Iterator<Item = (usize, SearchRow)> + '_ {
        (range.start..range.end.min(self.rows)).map(|row| (row, self.row_at(row).unwrap()))
    }

    pub(crate) fn result_row(&self, index: usize) -> Option<usize> {
        if index >= self.results.len() {
            return None;
        }
        let group = &self.groups[self.groups.partition_point(|group| group.start <= index) - 1];
        (!group.collapsed).then_some(group.row + index - group.start + 1)
    }

    pub(crate) fn first_visible_from(&self, index: usize) -> Option<usize> {
        let first = self.groups.partition_point(|group| group.end <= index);
        self.groups[first..]
            .iter()
            .find(|group| !group.collapsed)
            .map(|group| group.start.max(index))
    }

    pub(crate) fn last_visible_before(&self, index: usize) -> Option<usize> {
        let last = self.groups.partition_point(|group| group.start < index);
        self.groups[..last]
            .iter()
            .rev()
            .find(|group| !group.collapsed)
            .map(|group| group.end.min(index) - 1)
    }
}

impl Extend<GlobalSearchResult> for SearchResults {
    fn extend<T: IntoIterator<Item = GlobalSearchResult>>(&mut self, values: T) {
        let values = values.into_iter();
        self.results.reserve(values.size_hint().0);
        for result in values {
            let collapsed = if let Some(file) = self.files.get_mut(&result.file_path) {
                if file.matches == 0 {
                    self.file_count += 1;
                }
                file.matches += 1;
                file.collapsed
            } else {
                self.file_count += 1;
                self.files.insert(
                    result.file_path.clone(),
                    FileInfo {
                        matches: 1,
                        collapsed: false,
                    },
                );
                false
            };
            if self
                .results
                .last()
                .is_none_or(|last| last.file_path != result.file_path)
            {
                self.groups.push(Group {
                    start: self.results.len(),
                    end: self.results.len(),
                    row: self.rows,
                    collapsed,
                });
                self.rows += 1;
            }
            self.results.push(result);
            self.groups.last_mut().unwrap().end = self.results.len();
            if !collapsed {
                self.rows += 1;
            }
        }
    }
}

impl FromIterator<GlobalSearchResult> for SearchResults {
    fn from_iter<T: IntoIterator<Item = GlobalSearchResult>>(values: T) -> Self {
        let mut results = Self::default();
        results.extend(values);
        results
    }
}

impl From<Vec<GlobalSearchResult>> for SearchResults {
    fn from(values: Vec<GlobalSearchResult>) -> Self {
        values.into_iter().collect()
    }
}

impl Deref for SearchResults {
    type Target = [GlobalSearchResult];
    fn deref(&self) -> &Self::Target {
        &self.results
    }
}

impl<'a> IntoIterator for &'a SearchResults {
    type Item = &'a GlobalSearchResult;
    type IntoIter = std::slice::Iter<'a, GlobalSearchResult>;
    fn into_iter(self) -> Self::IntoIter {
        self.results.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(file: &str) -> GlobalSearchResult {
        GlobalSearchResult {
            file_path: file.into(),
            line_num: 1,
            line_text: "needle".into(),
            col_start: 0,
            col_end: 6,
            context_before: None,
            context_after: None,
        }
    }

    fn assert_matches_scan(results: &SearchResults) {
        let files: rustc_hash::FxHashSet<_> =
            results.iter().map(|result| &result.file_path).collect();
        assert_eq!(results.file_count(), files.len());
        let mut rows = Vec::new();
        let mut previous = None;
        for (index, result) in results.iter().enumerate() {
            if previous != Some(&result.file_path) {
                rows.push(SearchRow::Header(index));
                previous = Some(&result.file_path);
            }
            if !results.is_collapsed(&result.file_path) {
                rows.push(SearchRow::Result(index));
            }
        }
        assert_eq!(results.row_count(), rows.len());
        for (row, hit) in rows.iter().enumerate() {
            assert_eq!(results.row_at(row), Some(*hit));
            if let SearchRow::Result(index) = hit {
                assert_eq!(results.result_row(*index), Some(row));
            }
        }
        assert_eq!(results.row_at(rows.len()), None);
        for index in 0..=results.len() {
            let visible = |i: &usize| !results.is_collapsed(&results[*i].file_path);
            assert_eq!(
                results.first_visible_from(index),
                (index..results.len()).find(visible)
            );
            assert_eq!(
                results.last_visible_before(index),
                (0..index).rev().find(visible)
            );
        }
    }

    #[test]
    fn streamed_batches_collapse_and_noncontiguous_files_keep_index_consistent() {
        let mut results = SearchResults::default();
        results.set_collapsed(Path::new("future"), true);
        assert_matches_scan(&results);
        for file in ["a", "a", "b", "a", "c", "c"] {
            results.extend([result(file)]);
            assert_matches_scan(&results);
        }
        assert_eq!(results.file_count(), 3);
        assert_eq!(results.file_matches(Path::new("a")), 3);
        results.set_collapsed(Path::new("a"), true);
        assert_matches_scan(&results);
        results.extend([result("c"), result("a"), result("a")]);
        assert_matches_scan(&results);
        assert_eq!(results.result_row(7), None);
        results.set_all_collapsed(true);
        assert!(results.all_collapsed());
        assert_matches_scan(&results);
        results.extend([result("new")]);
        assert!(!results.all_collapsed());
        assert_matches_scan(&results);
        results.set_all_collapsed(false);
        assert_matches_scan(&results);
        results.clear();
        assert_eq!(results.file_count(), 0);
        assert!(!results.all_collapsed());
        assert_matches_scan(&results);
        results.extend([result("a")]);
        assert!(!results.is_collapsed(Path::new("a")));
        assert_matches_scan(&results);
        results = vec![result("replacement")].into();
        assert_eq!(results.file_matches(Path::new("a")), 0);
        assert_eq!(results.file_matches(Path::new("replacement")), 1);
        assert_matches_scan(&results);
        assert_eq!(results.row_at(usize::MAX), None);
        assert_eq!(results.result_row(usize::MAX), None);
        assert_eq!(results.visible_rows(usize::MAX..usize::MAX).count(), 0);
    }
}
