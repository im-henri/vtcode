//! Search result metadata is maintained when batches arrive or file groups collapse,
//! rather than rediscovered by every paint, click and keyboard navigation.
use crate::app::GlobalSearchResult;
use rustc_hash::{FxHashMap, FxHashSet};
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
    first_result: usize,
    collapsed: bool,
    sort_key: Option<crate::file_order::SearchPathKey>,
    dismissed: bool,
    dismissed_matches: FxHashSet<(usize, usize, usize)>,
    edited: bool,
}

struct Group {
    start: usize,
    end: usize,
    row: usize,
    collapsed: bool,
}

/// Read-only slice access keeps the index valid even when results are replaced.
/// File groups and match positions stay sorted as streamed batches arrive.
#[derive(Default)]
pub(crate) struct SearchResults {
    results: Vec<GlobalSearchResult>,
    files: FxHashMap<PathBuf, FileInfo>,
    file_count: usize,
    groups: Vec<Group>,
    rows: usize,
    remote: Option<crate::remote::workspace::Workspace>,
    ordering: Option<std::sync::Arc<crate::file_order::FileOrder>>,
    revision: u64,
}

impl SearchResults {
    pub(crate) fn set_remote(&mut self, remote: Option<crate::remote::workspace::Workspace>) {
        debug_assert!(self.results.is_empty());
        self.remote = remote;
    }

    fn compare(files: &FxHashMap<PathBuf, FileInfo>, order: &crate::file_order::FileOrder,
        a: &GlobalSearchResult, b: &GlobalSearchResult) -> std::cmp::Ordering {
        let position = || a.line_num.cmp(&b.line_num).then(a.col_start.cmp(&b.col_start)).then(a.col_end.cmp(&b.col_end));
        if a.file_path == b.file_path { return position(); }
        order.paths(files[&a.file_path].sort_key.as_ref().unwrap(), files[&b.file_path].sort_key.as_ref().unwrap())
            // Distinct files with equivalent display paths must not interleave.
            .then_with(|| a.file_path.cmp(&b.file_path))
            .then_with(position)
    }

    pub(crate) fn index_of(&self, result: &GlobalSearchResult) -> Option<usize> {
        if self.files.get(&result.file_path).is_none_or(|file| file.matches == 0) { return None; }
        self.results.binary_search_by(|other| Self::compare(&self.files, self.ordering.as_ref().unwrap(), other, result)).ok()
    }

    pub(crate) fn header_row(&self, index: usize) -> Option<usize> {
        if index >= self.results.len() { return None; }
        Some(self.groups[self.groups.partition_point(|group| group.start <= index) - 1].row)
    }
    pub(crate) fn file_header_index(&self, path: &Path) -> Option<usize> {
        let file = self.files.get(path)?;
        (file.matches > 0).then_some(file.first_result)
    }
    pub(crate) fn clear(&mut self) {
        self.results.clear();
        self.files.clear();
        self.file_count = 0;
        self.groups.clear();
        self.rows = 0;
        self.remote = None;
        self.revision = self.revision.wrapping_add(1);
    }

    pub(crate) fn revision(&self) -> u64 { self.revision }

    pub(crate) fn tracks_file(&self, path: &Path) -> bool {
        self.files.contains_key(path)
    }

    pub(crate) fn mark_file_edited(&mut self, path: &Path) -> bool {
        let Some(file) = self.files.get_mut(path) else { return false; };
        file.edited = true;
        true
    }

    pub(crate) fn accepts_streamed_result(&self, result: &GlobalSearchResult) -> bool {
        self.accepts_result(result, false)
    }

    pub(crate) fn accepts_refreshed_result(&self, result: &GlobalSearchResult) -> bool {
        self.accepts_result(result, true)
    }

    fn accepts_result(&self, result: &GlobalSearchResult, refreshed: bool) -> bool {
        self.files.get(&result.file_path).is_none_or(|file| {
            (refreshed || !file.edited) && !file.dismissed
                && !file.dismissed_matches.contains(&(result.line_num, result.col_start, result.col_end))
        })
    }

    /// Replace one open model without resetting other files, collapsed groups,
    /// or explicit dismissals. Original search batches for this file are stale.
    pub(crate) fn replace_file(&mut self, path: &Path, results: Vec<GlobalSearchResult>) {
        let Some(file) = self.files.get_mut(path) else { return; };
        if file.matches > 0 {
            self.file_count -= 1;
            file.matches = 0;
            self.results.retain(|result| result.file_path != path);
        }
        file.edited = true;
        self.groups.clear();
        self.rows = 0;
        self.append_groups(0);
        self.extend_results(results, true);
        self.revision = self.revision.wrapping_add(1);
    }

    /// Dismiss only from this search. Remember removals while later batches
    /// arrive; clear() starts a fresh search with every match eligible again.
    pub(crate) fn dismiss(&mut self, row: SearchRow) -> Option<Range<usize>> {
        let (SearchRow::Header(index) | SearchRow::Result(index)) = row;
        let result = self.results.get(index)?;
        let file = self.files.get_mut(&result.file_path)?;
        let range = match row {
            SearchRow::Header(_) => {
                file.dismissed = true;
                let group = &self.groups[self.groups.partition_point(|group| group.start <= index) - 1];
                group.start..group.end
            }
            SearchRow::Result(_) => {
                file.dismissed_matches.insert((result.line_num, result.col_start, result.col_end));
                index..index + 1
            }
        };
        file.matches -= range.len();
        if file.matches == 0 { self.file_count -= 1; }
        self.results.drain(range.clone());
        self.groups.clear();
        self.rows = 0;
        self.append_groups(0);
        self.revision = self.revision.wrapping_add(1);
        Some(range)
    }

    fn append_groups(&mut self, start: usize) {
        for index in start..self.results.len() {
            let result = &self.results[index];
            let collapsed = self.files[&result.file_path].collapsed;
            if self.groups.last().is_none_or(|last| self.results[last.start].file_path != result.file_path) {
                self.files.get_mut(&result.file_path).unwrap().first_result = index;
                self.groups.push(Group { start: index, end: index, row: self.rows, collapsed });
                self.rows += 1;
            }
            self.groups.last_mut().unwrap().end = index + 1;
            if !collapsed { self.rows += 1; }
        }
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
        self.revision = self.revision.wrapping_add(1);
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

impl SearchResults {
    fn extend_results<T: IntoIterator<Item = GlobalSearchResult>>(&mut self, values: T, refreshed: bool) {
        let mut values: Vec<_> = values.into_iter()
            .filter(|result| self.accepts_result(result, refreshed)).collect();
        if values.is_empty() { return; }
        let order = self.ordering.get_or_insert_with(crate::file_order::FileOrder::system).clone();
        for result in &values {
            let file = self.files.entry(result.file_path.clone()).or_default();
            if file.matches == 0 { self.file_count += 1; }
            file.matches += 1;
            if file.sort_key.is_none() {
                file.sort_key = Some(self.remote.as_ref().and_then(|remote| remote.search_sort_key(&result.file_path))
                    .unwrap_or_else(|| crate::file_order::SearchPathKey::local(&result.file_path)));
            }
        }
        let compare = |a: &GlobalSearchResult, b: &GlobalSearchResult| Self::compare(&self.files, &order, a, b);
        values.sort_by(compare);
        let append = self.results.last().is_none_or(|last| compare(last, &values[0]).is_le());
        let start = if append {
            let start = self.results.len();
            self.results.extend(values);
            start
        } else {
            let mut merged = Vec::with_capacity(self.results.len() + values.len());
            let mut old = std::mem::take(&mut self.results).into_iter().peekable();
            let mut new = values.into_iter().peekable();
            while let (Some(a), Some(b)) = (old.peek(), new.peek()) {
                let same_file = a.file_path == b.file_path;
                let stream = if compare(a, b).is_le() { &mut old } else { &mut new };
                merged.push(stream.next().unwrap());
                if !same_file {
                    // A file group precedes every match in the other file.
                    while stream.peek().is_some_and(|next| next.file_path == merged.last().unwrap().file_path) {
                        merged.push(stream.next().unwrap());
                    }
                }
            }
            merged.extend(old); merged.extend(new);
            self.results = merged;
            self.groups.clear(); self.rows = 0;
            0
        };
        self.append_groups(start);
        self.revision = self.revision.wrapping_add(1);
    }
}

impl Extend<GlobalSearchResult> for SearchResults {
    fn extend<T: IntoIterator<Item = GlobalSearchResult>>(&mut self, values: T) {
        self.extend_results(values, false);
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
                assert_eq!(results.file_header_index(&result.file_path), Some(index));
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
    fn edited_file_replacement_keeps_indexes_and_rejects_original_streamed_matches() {
        let mut results: SearchResults = [result("a"), result("b"), result("c")].into_iter().collect();
        results.set_collapsed(Path::new("b"), true);
        results.mark_file_edited(Path::new("a"));
        results.extend([result("a")]);
        assert_eq!(results.file_matches(Path::new("a")), 1);
        results.replace_file(Path::new("a"), Vec::new());
        assert_eq!(results.file_count(), 2);
        assert!(results.tracks_file(Path::new("a")));
        assert_matches_scan(&results);
        results.replace_file(Path::new("a"), vec![result("a")]);
        assert_eq!(results.file_count(), 3);
        assert_matches_scan(&results);
        results.dismiss(SearchRow::Header(1));
        results.replace_file(Path::new("b"), vec![result("b")]);
        assert_eq!(results.file_matches(Path::new("b")), 0);
        assert!(results.is_collapsed(Path::new("b")));
        assert_matches_scan(&results);
    }

    #[test]
    fn dismissals_keep_counts_and_rows_consistent_and_filter_later_batches_until_clear() {
        let found = |file: &str, line, start| {
            let mut found = result(file);
            found.line_num = line;
            found.col_start = start;
            found.col_end = start + 1;
            found
        };
        let mut results: SearchResults = [found("a", 1, 0), found("a", 1, 2), found("b", 2, 0), found("c", 1, 0)]
            .into_iter().collect();
        results.set_collapsed(Path::new("b"), true);
        let revision = results.revision();
        assert_eq!(results.dismiss(SearchRow::Result(0)), Some(0..1));
        assert_ne!(results.revision(), revision);
        assert_eq!(results.file_matches(Path::new("a")), 1);
        assert_matches_scan(&results);
        results.extend([found("a", 1, 0), found("a", 3, 0)]);
        assert_eq!(results.file_matches(Path::new("a")), 2);
        assert_matches_scan(&results);
        let b = results.iter().position(|found| found.file_path == Path::new("b")).unwrap();
        results.dismiss(SearchRow::Header(b));
        results.extend([found("b", 4, 0), found("d", 1, 0)]);
        assert_eq!(results.file_matches(Path::new("b")), 0);
        assert_eq!(results.file_count(), 3);
        assert_matches_scan(&results);
        results.clear();
        results.extend([found("a", 1, 0), found("b", 2, 0)]);
        assert_eq!(results.len(), 2);
        assert_matches_scan(&results);
    }

    #[test]
    fn dismissing_last_matches_and_collapsed_files_removes_their_headers() {
        let mut results: SearchResults = [result("a"), result("b")].into_iter().collect();
        results.set_collapsed(Path::new("b"), true);
        results.dismiss(SearchRow::Result(0));
        assert_eq!(results.row_count(), 1);
        assert_eq!(results.file_count(), 1);
        assert_matches_scan(&results);
        results.dismiss(SearchRow::Header(0));
        assert!(results.is_empty());
        assert_matches_scan(&results);
        assert_eq!(results.dismiss(SearchRow::Result(usize::MAX)), None);
        assert_eq!(results.dismiss(SearchRow::Header(0)), None);
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
        for (index, result) in results.iter().enumerate().filter(|(_, result)| result.file_path == Path::new("a")) {
            assert_eq!(results.result_row(index), None, "{:?}", result.file_path);
        }
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

    #[test]
    fn streamed_search_ordering_is_independent_of_batches_and_groups_split_files_once() {
        let files = ["system_defines.h", "system.h", "z.h", "a/file2.h", "a/file10.h",
            "a/deep/x.h", "folder10/x.h", "folder2/x.h"];
        let mut expected = Vec::new();
        for file in files {
            for (line, col) in [(1, 2), (1, 10), (3, 1)] {
                let mut found = result(file); found.line_num = line; found.col_start = col; found.col_end = col + 1;
                expected.push(found);
            }
        }
        let positions = |values: &SearchResults| values.iter()
            .map(|row| (row.file_path.clone(), row.line_num, row.col_start)).collect::<Vec<_>>();
        let expected_positions = expected.iter().map(|row| (row.file_path.clone(), row.line_num, row.col_start)).collect::<Vec<_>>();
        for rotation in 0..expected.len() {
            let mut input = expected.clone();
            input.rotate_left(rotation);
            if rotation % 2 == 0 { input.reverse(); }
            let mut results = SearchResults { ordering: Some(std::sync::Arc::new(crate::file_order::FileOrder::new("en-US").unwrap())),
                ..Default::default() };
            results.set_collapsed(Path::new("a/file2.h"), true);
            for chunk in input.chunks(1 + rotation % 7) {
                results.extend(chunk.iter().cloned());
                assert_matches_scan(&results);
                assert_eq!(results.groups.len(), results.file_count());
            }
            assert_eq!(positions(&results), expected_positions);
            assert_eq!(results.row_count(), expected.len() + files.len() - 3);
        }
    }

    #[test]
    fn large_streamed_search_keeps_order_groups_and_index_without_repainting_work() {
        let mut results = SearchResults::default();
        let started = std::time::Instant::now();
        let mut batch = Vec::new();
        for sequence in 0..1000 {
            let file = (sequence * 997) % 1000;
            for line in (1..=20).rev() {
                let mut found = result(&format!("src/folder{}/file{file}.rs", file % 13));
                found.line_num = line;
                batch.push(found);
            }
            if batch.len() >= 260 { results.extend(std::mem::take(&mut batch)); }
        }
        results.extend(batch);
        assert_eq!(results.len(), 20_000);
        assert_eq!(results.file_count(), 1000);
        assert_eq!(results.groups.len(), 1000);
        assert_matches_scan(&results);
        let order = results.ordering.as_ref().unwrap();
        assert!(results.windows(2).all(|pair| SearchResults::compare(&results.files, order, &pair[0], &pair[1]).is_le()));
        println!("20,000 shuffled streamed matches: {:?}", started.elapsed());
    }
}
