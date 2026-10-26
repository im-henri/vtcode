use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};

#[path = "explorer_width.rs"]
mod width;
pub(crate) use width::GitBadgeVisibility;

#[path = "explorer_selection.rs"]
mod selection;
pub(crate) use selection::Selection;

#[path = "explorer_disk_cache.rs"]
mod disk_cache;
use disk_cache::DirectoryCache;

#[path = "explorer_git.rs"]
mod git;
#[path = "explorer_git_cache.rs"]
mod git_cache;
use git_cache::IgnoreCache;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum GitStatus {
    Unmodified,
    Modified,
    Renamed,
    Added,
    Deleted,
    Untracked,
    Ignored,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitStatusSummary(u8);

impl GitStatusSummary {
    fn bit(status: GitStatus) -> u8 {
        match status {
            GitStatus::Unmodified => 0,
            GitStatus::Modified => 1 << 0,
            GitStatus::Renamed => 1 << 5,
            GitStatus::Added => 1 << 1,
            GitStatus::Untracked => 1 << 2,
            GitStatus::Deleted => 1 << 3,
            GitStatus::Ignored => 1 << 4,
        }
    }

    fn insert(&mut self, status: GitStatus) {
        self.0 |= Self::bit(status);
    }

    pub(crate) fn contains(self, status: GitStatus) -> bool {
        let bit = Self::bit(status);
        bit != 0 && self.0 & bit != 0
    }

    pub(crate) fn statuses(self) -> impl Iterator<Item = GitStatus> {
        [
            GitStatus::Modified,
            GitStatus::Renamed,
            GitStatus::Added,
            GitStatus::Untracked,
            GitStatus::Deleted,
            GitStatus::Ignored,
        ]
        .into_iter()
        .filter(move |status| self.contains(*status))
    }

    pub(crate) fn primary(self) -> GitStatus {
        [
            GitStatus::Deleted,
            GitStatus::Renamed,
            GitStatus::Added,
            GitStatus::Untracked,
            GitStatus::Modified,
            GitStatus::Ignored,
        ]
        .into_iter()
        .find(|status| self.contains(*status))
        .unwrap_or(GitStatus::Unmodified)
    }

    /// Choose the color from visible changes, so a hidden untracked child
    /// cannot mask a tracked modification's color on its parent directory.
    pub(crate) fn primary_visible(self, visibility: GitBadgeVisibility) -> GitStatus {
        match visibility {
            GitBadgeVisibility::None => GitStatus::Unmodified,
            GitBadgeVisibility::Tracked => Self(self.0 & !Self::bit(GitStatus::Untracked)).primary(),
            GitBadgeVisibility::All => self.primary(),
        }
    }
}

impl From<GitStatus> for GitStatusSummary {
    fn from(status: GitStatus) -> Self {
        let mut summary = Self::default();
        summary.insert(status);
        summary
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ExplorerSortMode {
    Name,
    Size,
    Modified,
    Type,
}

impl ExplorerSortMode {
    #[cfg(test)]
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Size => "Size",
            Self::Modified => "Date Modified",
            Self::Type => "Type",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub depth: usize,
    pub expanded: bool,
    pub git_status: GitStatus,
    pub git_status_summary: GitStatusSummary,
}

/// Immutable state needed to rebuild the visible Explorer tree on a worker.
pub(crate) struct ExplorerRefreshRequest {
    root: PathBuf,
    directory_cache: DirectoryCache,
    ignore_cache: IgnoreCache,
    sort_mode: ExplorerSortMode,
    expanded_dirs: FxHashSet<PathBuf>,
    find: Option<crate::explorer_find::Query>,
    cancel: Option<crate::background::CancellationToken>,
}

pub struct FileExplorer {
    pub(crate) find: crate::explorer_find::State,
    pub root: PathBuf,
    canonical_root: PathBuf,
    pub entries: Vec<FileEntry>,
    pub selected: usize,
    pub scroll_offset: usize,
    pub(crate) horizontal: width::HorizontalScroll,
    pub visible_height: usize,
    pub git_statuses: FxHashMap<PathBuf, GitStatusSummary>,
    pub sort_mode: ExplorerSortMode,
    expanded_dirs: FxHashSet<PathBuf>,
    /// File to select after the next asynchronous tree rebuild.
    pending_reveal: Option<PathBuf>,
    /// Parent lookup follows tree depth, independent of wide sibling lists.
    parent_indices: Vec<Option<usize>>,
    /// Compact path fingerprints avoid duplicating every full workspace path.
    /// Verify the candidate path on lookup so hash collisions remain harmless.
    path_indices: FxHashMap<u64, usize>,
    directory_cache: DirectoryCache,
    ignore_cache: IgnoreCache,
}

impl FileExplorer {
    pub(crate) fn insert_remote_placeholder(&mut self, path: PathBuf, name: String, is_dir: bool) {
        if self.find_path(&path).is_some() { return; }
        let parent = path.parent().unwrap_or(&self.root);
        let (position, depth) = match self.find_path(parent) {
            Some(index) => {
                self.entries[index].expanded = true;
                self.expanded_dirs.insert(parent.to_owned());
                (index + 1, self.entries[index].depth + 1)
            }
            None => (0, 0),
        };
        self.entries.insert(position, FileEntry { path, name, is_dir, depth, expanded: false,
            git_status: GitStatus::Unmodified, git_status_summary: Default::default() });
        self.rebuild_parent_indices(); self.selected = position; self.ensure_selection_visible();
    }
    pub(crate) fn expanded_snapshot(&self) -> FxHashSet<PathBuf> { self.expanded_dirs.clone() }
    pub(crate) fn cancel_unloaded_expansions(&mut self, paths: FxHashSet<PathBuf>) {
        for path in paths {
            self.expanded_dirs.remove(&path);
            // Filtered results have a complete local tree and independent
            // collapse state; cancelling an old normal scan must not alter it.
            if !self.find.filtering() && let Some(index) = self.find_path(&path) {
                self.entries[index].expanded = false;
            }
        }
    }
    pub(crate) fn new(root: PathBuf) -> Self {
        let directory_cache = DirectoryCache::default();
        let canonical_root = root.clone();
        let mut explorer = Self {
            find: Default::default(),
            root,
            canonical_root,
            entries: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            horizontal: Default::default(),
            visible_height: 0,
            git_statuses: FxHashMap::default(),
            sort_mode: ExplorerSortMode::Name,
            expanded_dirs: FxHashSet::default(),
            pending_reveal: None,
            parent_indices: Vec::new(),
            path_indices: FxHashMap::default(),
            directory_cache,
            ignore_cache: IgnoreCache::default(),
        };
        explorer.refresh();
        explorer
    }

    /// Construct immediately and let `App::refresh_explorer_async` perform
    /// canonicalization and directory traversal. Used while switching an
    /// already-running workbench to another folder.
    pub(crate) fn new_deferred(root: PathBuf) -> Self {
        Self {
            find: Default::default(),
            canonical_root: root.clone(),
            root,
            entries: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            horizontal: Default::default(),
            visible_height: 0,
            git_statuses: FxHashMap::default(),
            sort_mode: ExplorerSortMode::Name,
            expanded_dirs: FxHashSet::default(),
            pending_reveal: None,
            parent_indices: Vec::new(),
            path_indices: FxHashMap::default(),
            directory_cache: DirectoryCache::default(),
            ignore_cache: IgnoreCache::default(),
        }
    }

    pub(crate) fn refresh(&mut self) {
        self.invalidate_directory_cache();
        self.canonical_root = self.directory_cache.canonical_root(&self.root);
        // Collect currently expanded dirs before clearing entries
        self.expanded_dirs.clear();
        for e in &self.entries {
            if e.is_dir && e.expanded {
                self.expanded_dirs.insert(e.path.clone());
            }
        }
        self.entries.clear();
        let root = self.root.clone();
        self.scan_dir(&root, 0, false, None);
        self.ignore_cache.mark_ignored(&self.root, &self.canonical_root, &mut self.entries, None);
        self.rebuild_parent_indices();
    }

    pub(crate) fn refresh_request(&self) -> ExplorerRefreshRequest {
        ExplorerRefreshRequest {
            root: self.root.clone(),
            directory_cache: self.directory_cache.clone(),
            ignore_cache: self.ignore_cache.clone(),
            sort_mode: self.sort_mode,
            expanded_dirs: self.expanded_dirs.clone(),
            find: self.find.request(),
            cancel: self.find.cancel.clone(),
        }
    }

    pub(crate) fn headless_request(&mut self, expanded: FxHashSet<PathBuf>, sort: ExplorerSortMode,
        find: Option<crate::explorer_find::Query>, cancel: crate::background::CancellationToken) -> ExplorerRefreshRequest {
        self.expanded_dirs = expanded;
        self.sort_mode = sort;
        let mut request = self.refresh_request();
        request.find = find; request.cancel = Some(cancel); request
    }

    pub(crate) fn build_refresh_entries(
        request: ExplorerRefreshRequest,
    ) -> (PathBuf, Vec<FileEntry>, crate::explorer_find::Matches) {
        let root = request.root.clone();
        let canonical_root = request.directory_cache.canonical_root(&root);
        let mut explorer = Self {
            find: Default::default(),
            root: request.root,
            canonical_root: canonical_root.clone(),
            entries: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            horizontal: Default::default(),
            visible_height: 0,
            // Git decorations are applied from the current UI snapshot when
            // entries arrive. Cloning a potentially huge status map before
            // every folder expansion was both redundant and immediately stale.
            git_statuses: FxHashMap::default(),
            sort_mode: request.sort_mode,
            expanded_dirs: request.expanded_dirs,
            pending_reveal: None,
            parent_indices: Vec::new(),
            path_indices: FxHashMap::default(),
            directory_cache: request.directory_cache,
            ignore_cache: request.ignore_cache,
        };
        let matcher = request.find.as_ref().map(crate::explorer_find::NameMatcher::new).transpose();
        let matcher = match matcher {
            Ok(matcher) => matcher,
            Err(error) => {
                // Invalid input does not need a recursive scan. Keep the normal
                // tree in highlight mode; filter mode shows the error without hits.
                if request.find.as_ref().is_some_and(|query| !query.filter) {
                    explorer.scan_dir(&root, 0, false, request.cancel.as_ref());
                    explorer.ignore_cache.mark_ignored(&root, &canonical_root, &mut explorer.entries, request.cancel.as_ref());
                }
                return (canonical_root, explorer.entries, crate::explorer_find::Matches {
                    error: Some(error), ..Default::default()
                });
            }
        };
        explorer.scan_dir(&root, 0, request.find.is_some(), request.cancel.as_ref());
        let decorate = |entries: &mut [FileEntry]| explorer.ignore_cache.mark_ignored(
            &root, &canonical_root, entries, request.cancel.as_ref());
        let (entries, matches) = if let Some((query, matcher)) = request.find.zip(matcher) {
            crate::explorer_find::project(explorer.entries, &query, &matcher, request.cancel.as_ref(), decorate)
        } else {
            decorate(&mut explorer.entries);
            (explorer.entries, Default::default())
        };
        (canonical_root, entries, matches)
    }

    pub(crate) fn invalidate_directory_cache(&self) {
        self.directory_cache.clear();
        self.ignore_cache.invalidate();
    }

    pub(crate) fn invalidate_git_decorations(&self) {
        self.ignore_cache.invalidate();
    }

    pub(crate) fn set_source_control_enabled(&mut self, enabled: bool) {
        if self.ignore_cache.enabled() == enabled { return; }
        self.ignore_cache.set_enabled(enabled);
        if !enabled {
            self.git_statuses.clear();
            self.refresh_visible_git_statuses();
        }
    }

    pub(crate) fn invalidate_directory_paths<'a>(&self, paths: impl IntoIterator<Item = &'a Path>, structural: bool) {
        // Content-only saves cannot change ignore matching. Collect references,
        // not copies, and cap pending work during large watcher bursts.
        let mut affected = Vec::new();
        self.directory_cache.invalidate_paths(paths.into_iter().inspect(|path| {
            if affected.len() <= 1_024 && (structural || path.file_name().is_some_and(|name| name == ".gitignore")) {
                affected.push(*path);
            }
        }), structural);
        self.ignore_cache.invalidate_paths(affected);
    }

    pub(crate) fn set_directory_watcher_active(&self, active: bool) {
        self.directory_cache.set_watched(active);
        self.ignore_cache.set_watched(active);
    }

    pub(crate) fn apply_refresh_entries(
        &mut self,
        canonical_root: PathBuf,
        entries: Vec<FileEntry>,
        matches: crate::explorer_find::Matches,
    ) {
        let selected_path = self
            .entries
            .get(self.selected)
            .map(|entry| entry.path.clone());
        self.canonical_root = canonical_root;
        self.find.results = matches;
        // Expansion may have changed locally while this filesystem refresh
        // was running. Always project the new snapshot using the current state.
        self.entries = if self.find.filtering() {
            self.find.results.filtered_entries(&self.find.collapsed).unwrap_or(entries)
        } else { entries };
        self.find.loading = false;
        self.rebuild_parent_indices();
        self.refresh_visible_git_statuses();
        let reveal_path = self.pending_reveal.take();
        self.selected = reveal_path
            .as_ref()
            .and_then(|path| self.find_path(path))
            .or_else(|| selected_path.as_ref().and_then(|path| self.find_visible_selection(path)))
            .unwrap_or_else(|| self.selected.min(self.entries.len().saturating_sub(1)));
        if std::mem::take(&mut self.find.select_first) {
            self.select_find_match(false, true);
        }
        self.ensure_selection_visible();
    }

    /// A filtered folder toggle changes visibility only, not the search results.
    /// Keep background filesystem refreshes independent of these local changes.
    pub(crate) fn rebuild_filtered_entries(&mut self) -> bool {
        if !self.find.filtering() { return false; }
        let Some(entries) = self.find.results.filtered_entries(&self.find.collapsed) else {
            return false;
        };
        let selected_path = self.entries.get(self.selected).map(|entry| entry.path.clone());
        self.entries = entries;
        self.rebuild_parent_indices();
        self.refresh_visible_git_statuses();
        self.selected = selected_path.as_ref()
            .and_then(|path| self.find_visible_selection(path))
            .unwrap_or_else(|| self.selected.min(self.entries.len().saturating_sub(1)));
        self.ensure_selection_visible();
        true
    }

    fn find_visible_selection(&self, path: &Path) -> Option<usize> {
        if self.find.filtering() {
            // Collapse All can hide the selection. Select its visible ancestor
            // instead of an unrelated row that happens to occupy its old index.
            path.ancestors().find_map(|path| self.find_path(path))
        } else {
            self.find_path(path)
        }
    }

    /// Save the normal tree once; filtered expansion must never leak into it.
    pub(crate) fn prepare_find(&mut self) {
        if self.find.filtering() {
            if self.find.saved_view.is_none() {
                self.find.saved_view = Some(crate::explorer_find::View {
                    entries: std::mem::take(&mut self.entries),
                    selected: self.selected,
                    offset: self.scroll_offset,
                });
            }
            self.entries.clear();
            self.selected = 0;
            self.scroll_offset = 0;
        } else if let Some(view) = self.find.saved_view.take() {
            self.entries = view.entries;
            self.selected = view.selected;
            self.scroll_offset = view.offset;
            self.refresh_visible_git_statuses();
        }
        self.rebuild_parent_indices();
        self.find.results = Default::default();
        self.find.collapsed.clear();
        self.find.loading = self.find.request().is_some();
        self.find.select_first = self.find.loading;
    }

    pub(crate) fn select_find_match(&mut self, backwards: bool, first: bool) {
        let is_match = |&i: &usize| {
            let entry = &self.entries[i];
            self.find.results.rows.get(&entry.path).is_some_and(|matched| {
                matched.matched || (!entry.expanded && matched.descendants > 0)
            })
        };
        let end = self.entries.len();
        let next = if first {
            if backwards { (0..end).rev().find(is_match) } else { (0..end).find(is_match) }
        } else if backwards {
            let split = self.selected.min(end);
            (0..split).rev().chain((split..end).rev()).find(is_match)
        } else {
            let split = self.selected.saturating_add(1).min(end);
            (split..end).chain(0..split).find(is_match)
        };
        if let Some(index) = next {
            self.selected = index;
            self.ensure_selection_visible();
        }
    }

    fn ensure_selection_visible(&mut self) {
        let visible_height = self.visible_height.max(1);
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset.saturating_add(visible_height) {
            self.scroll_offset = self.selected.saturating_sub(visible_height - 1);
        }
        self.scroll_offset = self.scroll_offset.min(
            self.entries
                .len()
                .saturating_sub(visible_height),
        );
        // Sticky folders cover the first rows of the scrolled slice. A selected
        // folder is still visible in that header; other covered entries need
        // enough space above them to clear it.
        let sticky = self.sticky_indices(visible_height);
        if self.selected < self.scroll_offset + sticky.len() && !sticky.contains(&self.selected) {
            self.scroll_offset = self.selected.saturating_sub(Self::sticky_limit(visible_height));
        }
    }

    fn sticky_limit(height: usize) -> usize {
        // Always leave at least one row for tree content.
        5.min(height.saturating_sub(1))
    }

    fn rebuild_parent_indices(&mut self) {
        self.horizontal.invalidate();
        self.parent_indices.clear();
        self.parent_indices.reserve(self.entries.len());
        self.path_indices.clear();
        self.path_indices.reserve(self.entries.len());
        let mut ancestors: Vec<usize> = Vec::new();
        for (index, entry) in self.entries.iter().enumerate() {
            self.path_indices
                .entry(Self::path_key(&entry.path))
                .or_insert(index);
            while ancestors
                .last()
                .is_some_and(|&parent| self.entries[parent].depth >= entry.depth)
            {
                ancestors.pop();
            }
            self.parent_indices.push(ancestors.last().copied());
            if entry.is_dir {
                ancestors.push(index);
            }
        }
    }

    fn path_key(path: &Path) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hash = rustc_hash::FxHasher::default();
        path.hash(&mut hash);
        hash.finish()
    }

    pub(crate) fn find_path(&self, path: &Path) -> Option<usize> {
        if let Some(&index) = self.path_indices.get(&Self::path_key(path))
            && self
                .entries
                .get(index)
                .is_some_and(|entry| entry.path == path)
        {
            return Some(index);
        }
        // Directly constructed trees and rare fingerprint collisions use the
        // same exact lexical-path semantics as the original linear lookup.
        self.entries.iter().position(|entry| entry.path == path)
    }

    fn parent_index(&self, index: usize) -> Option<usize> {
        if self.parent_indices.len() == self.entries.len() {
            return self.parent_indices.get(index).copied().flatten();
        }
        // Tests/extensions may construct entries directly; normal refreshes
        // always install the index together with the rebuilt tree.
        let depth = self.entries.get(index)?.depth;
        (0..index).rev().find(|&parent| {
            self.entries[parent].is_dir && self.entries[parent].depth < depth
        })
    }

    pub(crate) fn viewport_offset(&self, height: usize) -> usize {
        self.scroll_offset
            .min(self.entries.len().saturating_sub(height.max(1)))
    }

    /// Ancestor rows pinned over the scrolled slice, from outermost to innermost.
    /// Check each header's own row so a following sibling pushes an old folder
    /// out of the sticky area as soon as its subtree leaves that row.
    pub(crate) fn sticky_indices(&self, height: usize) -> Vec<usize> {
        let offset = self.viewport_offset(height);
        let mut sticky: Vec<usize> = Vec::new();
        if offset == 0 {
            return sticky;
        }
        for row in 0..Self::sticky_limit(height) {
            let index = offset + row;
            let Some(entry) = self.entries.get(index) else {
                break;
            };
            let min_depth = sticky.last().map_or(0, |&parent| self.entries[parent].depth + 1);
            if sticky
                .last()
                .is_some_and(|&parent| !entry.path.starts_with(&self.entries[parent].path))
            {
                break;
            }
            let mut ancestor = None;
            let mut parent = self.parent_index(index);
            while let Some(i) = parent {
                let candidate = &self.entries[i];
                if candidate.depth < min_depth {
                    break;
                }
                if candidate.expanded {
                    ancestor = Some(i);
                }
                parent = self.parent_index(i);
            }
            let Some(ancestor) = ancestor else {
                break;
            };
            sticky.push(ancestor);
        }
        sticky
    }

    /// Shared by rendering and pointer handling, including pinned folder rows.
    pub(crate) fn viewport_indices(&self, height: usize) -> Vec<usize> {
        let offset = self.viewport_offset(height);
        let mut rows = self.sticky_indices(height);
        rows.extend(offset + rows.len()..offset.saturating_add(height).min(self.entries.len()));
        rows
    }

    pub(crate) fn entry_at_row(&self, row: usize, height: usize) -> Option<usize> {
        if row >= height {
            return None;
        }
        let sticky = self.sticky_indices(height);
        sticky.get(row).copied().or_else(|| {
            let index = self.viewport_offset(height) + row;
            (index < self.entries.len()).then_some(index)
        })
    }

    fn scan_dir(&mut self, dir: &Path, depth: usize, find_all: bool,
        cancel: Option<&crate::background::CancellationToken>) {
        // Prevent stack overflow from symlink loops or extremely deep directories
        const MAX_DEPTH: usize = 64;
        if depth >= MAX_DEPTH || cancel.is_some_and(|token| token.is_cancelled()) {
            return;
        }
        let sort_mode = self.sort_mode;
        let cache = self.directory_cache.clone();
        let covered = crate::background::workspace_path_is_watched(&self.root, dir);
        let Some(items) = cache.get_or_read(dir, sort_mode, covered, cancel, || {
            let mut items: Vec<_> = match std::fs::read_dir(dir) {
                Ok(entries) => entries
                    .take_while(|_| !cancel.is_some_and(|token| token.is_cancelled()))
                    .filter_map(Result::ok)
                    .filter_map(|e| {
                        if e.file_name().to_str().is_some_and(|name| name.starts_with(crate::formatters::TEMP_DIR_PREFIX)) {
                            return None;
                        }
                        let ft = e.file_type().ok()?;
                        // Treat symlink dirs as files to prevent recursion loops
                        let is_dir = ft.is_dir() && !ft.is_symlink();
                        // Size/date sorting used to call metadata from inside the
                        // comparator, causing O(n log n) filesystem requests. Read
                        // each key once before sorting instead.
                        let metadata = matches!(
                            sort_mode,
                            ExplorerSortMode::Size | ExplorerSortMode::Modified
                        )
                        .then(|| e.metadata().ok())
                        .flatten();
                        let size = metadata.as_ref().map(std::fs::Metadata::len).unwrap_or(0);
                        let modified = metadata
                            .as_ref()
                            .and_then(|metadata| metadata.modified().ok())
                            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                        // Cache the case-insensitive name once per entry rather
                        // than allocating it on every comparison.
                        let path = e.path();
                        let name_key = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_lowercase();
                        Some((path, is_dir, size, modified, name_key))
                    })
                    .collect(),
                Err(_) => return None,
            };

            items.sort_unstable_by(
                |(a_path, a_dir, a_size, a_time, a_name), (b_path, b_dir, b_size, b_time, b_name)| {
                    // Always sort directories first
                    let dir_ord = b_dir.cmp(a_dir);
                    if dir_ord != std::cmp::Ordering::Equal {
                        return dir_ord;
                    }
                    // Use the original name only to make case-only ties deterministic.
                    let name_order = || {
                        a_name
                            .cmp(b_name)
                            .then_with(|| a_path.file_name().cmp(&b_path.file_name()))
                    };
                    match sort_mode {
                        ExplorerSortMode::Name => name_order(),
                        ExplorerSortMode::Size => b_size.cmp(a_size).then_with(name_order),
                        ExplorerSortMode::Modified => b_time.cmp(a_time).then_with(name_order),
                        ExplorerSortMode::Type => {
                            let a_ext = a_path.extension().and_then(|e| e.to_str()).unwrap_or("");
                            let b_ext = b_path.extension().and_then(|e| e.to_str()).unwrap_or("");
                            a_ext.cmp(b_ext).then_with(name_order)
                        }
                    }
                },
            );

            Some(items)
        }) else { return; };

        for (path, is_dir, _, _, _) in items.iter() {
            let path = path.clone();
            let is_dir = *is_dir;
            if cancel.is_some_and(|token| token.is_cancelled()) { return; }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?")
                .to_string();

            let already_expanded = is_dir && self.expanded_dirs.contains(&path);

            let git_status_summary = Self::git_status_summary_for_path(
                &self.git_statuses,
                &self.root,
                &self.canonical_root,
                &path,
            );
            let git_status = git_status_summary.primary();
            let recurse = (already_expanded || (find_all && is_dir)).then(|| path.clone());

            self.entries.push(FileEntry {
                path,
                name,
                is_dir,
                depth,
                expanded: already_expanded,
                git_status,
                git_status_summary,
            });

            // If this dir was previously expanded, recurse
            if let Some(path) = recurse {
                self.scan_dir(&path, depth + 1, find_all, cancel);
            }
        }
    }

    pub(crate) fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.ensure_selection_visible();
        }
    }

    pub(crate) fn move_down(&mut self) {
        if !self.entries.is_empty() && self.selected < self.entries.len() - 1 {
            self.selected += 1;
            self.ensure_selection_visible();
        }
    }

    pub(crate) fn scroll_up(&mut self, amount: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
    }

    pub(crate) fn scroll_down(&mut self, amount: usize) {
        let max = self
            .entries
            .len()
            .saturating_sub(self.visible_height.max(1));
        self.scroll_offset = (self.scroll_offset + amount).min(max);
    }

    #[cfg(test)]
    pub(crate) fn toggle_or_open(&mut self) -> Option<PathBuf> {
        let (path, tree_changed) = self.toggle_or_open_deferred();
        if tree_changed {
            self.refresh();
        }
        path
    }

    /// Update expansion state without touching the filesystem. The caller can
    /// immediately return to the event loop and queue an asynchronous refresh.
    pub(crate) fn toggle_or_open_deferred(&mut self) -> (Option<PathBuf>, bool) {
        if self.entries.is_empty() {
            return (None, false);
        }

        let entry = &self.entries[self.selected];
        let path = entry.path.clone();
        let is_dir = entry.is_dir;

        if is_dir {
            // Toggle expansion
            let was_expanded = self.entries[self.selected].expanded;
            self.entries[self.selected].expanded = !was_expanded;

            // Update expanded_dirs incrementally
            if self.find.filtering() {
                if was_expanded { self.find.collapsed.insert(path.clone()); }
                else { self.find.collapsed.remove(&path); }
            } else if was_expanded {
                self.expanded_dirs.remove(&path);
            } else {
                self.expanded_dirs.insert(path.clone());
            }

            if was_expanded { self.remove_collapsed_descendants(self.selected); }

            (None, true)
        } else {
            (Some(path), false)
        }
    }

    #[cfg(test)]
    pub(crate) fn collapse_current(&mut self) {
        if self.collapse_current_deferred() {
            self.refresh();
        }
    }

    /// Collapse expansion state or move selection to the parent without any
    /// directory reads. Returns whether the visible tree needs rebuilding.
    pub(crate) fn collapse_current_deferred(&mut self) -> bool {
        if self.entries.is_empty() {
            return false;
        }

        let entry = &self.entries[self.selected];
        if entry.is_dir && entry.expanded {
            let path = entry.path.clone();
            if self.find.filtering() { self.find.collapsed.insert(path); }
            else { self.expanded_dirs.remove(&path); }
            self.entries[self.selected].expanded = false;
            self.remove_collapsed_descendants(self.selected);
            true
        } else {
            // Navigate to parent directory
            if let Some(parent) = self.parent_index(self.selected) {
                self.selected = parent;
            }
            self.ensure_selection_visible();
            false
        }
    }

    #[cfg(test)]
    pub(crate) fn collapse_all(&mut self) {
        if self.collapse_all_deferred() {
            self.refresh();
        }
    }

    pub(crate) fn collapse_all_deferred(&mut self) -> bool {
        if self.find.filtering() {
            let changed = self.entries.iter().any(|e| e.is_dir && e.expanded);
            let tree = self.find.results.filtered_tree.as_ref().unwrap_or(&self.entries);
            self.find.collapsed.extend(tree.iter().filter(|e| e.is_dir).map(|e| e.path.clone()));
            for entry in &mut self.entries { entry.expanded = false; }
            self.scroll_offset = 0;
            return changed;
        }
        let changed = !self.expanded_dirs.is_empty();
        self.expanded_dirs.clear();
        for entry in &mut self.entries {
            entry.expanded = false;
        }
        let selected_root = self.entries.get(self.selected).and_then(|entry|
            self.entries[..=self.selected].iter().rfind(|root| root.depth == 0 && entry.path.starts_with(&root.path)))
            .map(|entry| entry.path.clone());
        self.entries.retain(|entry| entry.depth == 0);
        self.rebuild_parent_indices();
        self.selected = selected_root.as_deref().and_then(|path| self.find_path(path)).unwrap_or(0);
        self.scroll_offset = 0;
        changed
    }

    /// Collapse is a local tree projection, independent of disk/SSH latency.
    /// Keep sibling rows and path indices aligned while a refresh is pending.
    fn remove_collapsed_descendants(&mut self, index: usize) {
        let depth = self.entries[index].depth;
        let count = self.entries[index + 1..].iter().take_while(|entry| entry.depth > depth).count();
        if count != 0 {
            self.entries.drain(index + 1..index + 1 + count);
            self.rebuild_parent_indices();
            self.ensure_selection_visible();
        }
    }

    pub(crate) fn select_path(&mut self, target: &Path) {
        // A search result can be inside ancestors absent from the normal tree.
        // Record the complete chain before scanning, not just cached entries.
        self.expanded_dirs.extend(self.entries.iter()
            .filter(|entry| entry.is_dir && entry.expanded).map(|entry| entry.path.clone()));
        let mut parent = target.parent();
        while let Some(p) = parent {
            if p == self.root || !p.starts_with(&self.root) {
                break;
            }
            self.expanded_dirs.insert(p.to_path_buf());
            parent = p.parent();
        }
        self.pending_reveal = None;
        self.entries.clear();
        let root = self.root.clone();
        self.scan_dir(&root, 0, false, None);
        self.ignore_cache.mark_ignored(&self.root, &self.canonical_root, &mut self.entries, None);
        self.rebuild_parent_indices();
        if let Some(pos) = self.find_path(target) {
            self.selected = pos;
            self.ensure_selection_visible();
        }
    }

    fn git_status_summary_for_path(
        statuses: &FxHashMap<PathBuf, GitStatusSummary>,
        root: &Path,
        canonical_root: &Path,
        path: &Path,
    ) -> GitStatusSummary {
        statuses
            .get(path)
            .copied()
            .or_else(|| {
                if statuses.is_empty() || root == canonical_root {
                    return None;
                }
                path.strip_prefix(root)
                    .ok()
                    .and_then(|relative| statuses.get(&canonical_root.join(relative)).copied())
            })
            .unwrap_or_default()
    }

    fn insert_git_status(
        statuses: &mut FxHashMap<PathBuf, GitStatusSummary>,
        path: &Path,
        status: GitStatus,
    ) -> bool {
        // Every inserted status is propagated to all ancestors. Encountering
        // it again therefore proves the rest of this ancestor walk is done.
        if statuses.get(path).is_some_and(|summary| summary.contains(status)) {
            return false;
        }
        statuses.entry(path.to_path_buf()).or_default().insert(status);
        true
    }

    fn refresh_visible_git_statuses(&mut self) {
        self.horizontal.invalidate();
        let enabled = self.ignore_cache.enabled();
        let statuses = &self.git_statuses;
        let root = &self.root;
        let canonical_root = &self.canonical_root;
        for entry in &mut self.entries {
            // Ignore state belongs to the Explorer snapshot. Git's change list
            // omits these paths and must not erase their cached decoration.
            let ignored = enabled && entry.git_status_summary.contains(GitStatus::Ignored);
            entry.git_status_summary =
                Self::git_status_summary_for_path(statuses, root, canonical_root, &entry.path);
            if ignored {
                entry.git_status_summary.insert(GitStatus::Ignored);
            }
            entry.git_status = entry.git_status_summary.primary();
        }
    }

    pub(crate) fn root_git_status_summary(&self) -> GitStatusSummary {
        Self::git_status_summary_for_path(
            &self.git_statuses,
            &self.root,
            &self.canonical_root,
            &self.root,
        )
    }

    /// Single-repository adapter for fixtures; runtime combines all repositories.
    #[cfg(test)]
    pub(crate) fn apply_git_status_changes<'a>(
        &mut self,
        workdir: Option<&Path>,
        changes: impl IntoIterator<Item = (&'a str, char)>,
    ) {
        let workdir = workdir.unwrap_or(&self.canonical_root).to_owned();
        self.apply_git_repository_changes(changes.into_iter().map(|(path, status)| (workdir.as_path(), path, status)));
    }

    /// Combine repository snapshots in one pass without allocating temporary
    /// absolute path strings or clearing another project's decorations.
    pub(crate) fn apply_git_repository_changes<'a>(
        &mut self,
        changes: impl IntoIterator<Item = (&'a Path, &'a str, char)>,
    ) {
        self.git_statuses.clear();
        if !self.ignore_cache.enabled() { self.refresh_visible_git_statuses(); return; }

        for (workdir, relative_path, marker) in changes {
            let status = match marker {
                'A' => GitStatus::Added,
                'D' => GitStatus::Deleted,
                '?' => GitStatus::Untracked,
                'M' => GitStatus::Modified,
                'R' => GitStatus::Renamed,
                _ => continue,
            };
            let full_path = workdir.join(relative_path);

            // A path may have both staged and unstaged records. Keep every
            // distinct state so mixed directory decorations tell the full story.
            // Bubble every distinct descendant status up through each visible
            // ancestor by walking the Git path itself. This also works when the
            // repository root is above or below the Explorer workspace root.
            // It is computed once per background refresh, so drawing remains a
            // constant-time lookup per entry.
            for path in full_path.ancestors() {
                if !Self::insert_git_status(
                    &mut self.git_statuses,
                    path,
                    status,
                ) {
                    break;
                }
            }
        }

        if self.root != self.canonical_root {
            // Add alternate spellings after propagation. An alias can equal a
            // yet-unvisited ancestor (notably for roots containing `..`), so
            // adding it during the walk would invalidate the early-stop proof.
            let aliases: Vec<_> = self.git_statuses.iter().filter_map(|(path, summary)| {
                path.strip_prefix(&self.canonical_root)
                    .ok().map(|relative| self.root.join(relative))
                    .or_else(|| path.strip_prefix(&self.root).ok()
                        .map(|relative| self.canonical_root.join(relative)))
                    .map(|path| (path, *summary))
            }).collect();
            for (path, summary) in aliases {
                self.git_statuses.entry(path).or_default().0 |= summary.0;
            }
        }

        self.refresh_visible_git_statuses();
    }

    /// Calculate the destination directory for a drag-drop operation.
    /// If the target is a directory, files are dropped into it.
    /// If the target is a file, files are dropped into its parent directory.
    pub(crate) fn calc_drop_dest_dir(target: &FileEntry) -> PathBuf {
        if target.is_dir {
            target.path.clone()
        } else {
            target.path.parent().unwrap_or(&target.path).to_path_buf()
        }
    }

    /// Check whether a drag-drop move is valid.
    /// Returns false if:
    /// - Source is already in the destination directory (no-op move)
    /// - Destination is inside the source (would create a cycle for directories)
    pub(crate) fn is_valid_move(drag_path: &Path, dest_dir: &Path) -> bool {
        drag_path.parent() != Some(dest_dir) && !dest_dir.starts_with(drag_path)
    }

    /// Record a file to reveal after the worker rebuilds the tree. This method
    /// performs no filesystem I/O and is safe to call from the render loop.
    pub(crate) fn reveal_file_deferred(&mut self, file_path: &Path) -> bool {
        if let Some(position) = self.find_path(file_path) {
            self.pending_reveal = None;
            self.selected = position;
            self.ensure_selection_visible();
            return false;
        }
        // An automatic editor reveal must not clear the query or repeatedly
        // rebuild a filtered tree that intentionally excludes this file.
        if self.find.filtering() { return false; }

        // Opening/revealing a known path can precede its create notification.
        // Refresh missing ancestors, but reuse listings for cached closed folders.
        let missing: Vec<_> = file_path.ancestors()
            .take_while(|path| *path != self.root && path.starts_with(&self.root))
            .take_while(|path| !self.directory_cache.contains_path(path, self.sort_mode))
            .collect();
        self.directory_cache.invalidate_paths(missing, true);

        // Build list of ancestor directories that need to be expanded
        let mut ancestors: Vec<PathBuf> = Vec::new();
        let mut current = file_path.parent();
        while let Some(parent) = current {
            if parent == self.root || parent.starts_with(&self.root) {
                if parent != self.root {
                    ancestors.push(parent.to_path_buf());
                }
                current = parent.parent();
            } else {
                break;
            }
        }

        // Ensure all ancestor directories are expanded
        for ancestor in &ancestors {
            self.expanded_dirs.insert(ancestor.clone());
        }
        self.pending_reveal = Some(file_path.to_path_buf());
        true
    }
}

#[cfg(test)]
#[path = "explorer_io_tests.rs"]
mod io_tests;

#[cfg(test)]
#[path = "explorer_git_tests.rs"]
mod git_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Build a FileExplorer with manually set entries (no filesystem needed).
    fn make_explorer(entries: Vec<FileEntry>) -> FileExplorer {
        let mut explorer = FileExplorer {
            find: Default::default(),
            root: PathBuf::from("/project"),
            canonical_root: PathBuf::from("/project"),
            entries,
            selected: 0,
            scroll_offset: 0,
            horizontal: Default::default(),
            visible_height: 5,
            git_statuses: FxHashMap::default(),
            sort_mode: ExplorerSortMode::Name,
            expanded_dirs: FxHashSet::default(),
            pending_reveal: None,
            parent_indices: Vec::new(),
            path_indices: FxHashMap::default(),
            directory_cache: DirectoryCache::default(),
            ignore_cache: IgnoreCache::default(),
        };
        explorer.rebuild_parent_indices();
        explorer
    }

    /// Frozen original propagation: visit every ancestor and insert both
    /// lexical spellings immediately, even when this repeats completed work.
    fn reference_git_statuses(root: &Path, canonical: &Path, workdir: &Path,
        changes: &[(String, char)]) -> FxHashMap<PathBuf, GitStatusSummary>
    {
        let mut statuses = FxHashMap::<PathBuf, GitStatusSummary>::default();
        reference_apply_git_statuses(&mut statuses, root, canonical, workdir, changes);
        statuses
    }

    fn reference_apply_git_statuses(statuses: &mut FxHashMap<PathBuf, GitStatusSummary>,
        root: &Path, canonical: &Path, workdir: &Path, changes: &[(String, char)])
    {
        statuses.clear();
        for (relative, marker) in changes {
            let status = match marker {
                'A' => GitStatus::Added, 'D' => GitStatus::Deleted,
                '?' => GitStatus::Untracked, 'M' => GitStatus::Modified,
                'R' => GitStatus::Renamed, _ => continue,
            };
            let full = workdir.join(relative);
            for path in full.ancestors() {
                statuses.entry(path.to_path_buf()).or_default().insert(status);
                let alias = path.strip_prefix(canonical).ok().map(|p| root.join(p))
                    .or_else(|| path.strip_prefix(root).ok().map(|p| canonical.join(p)));
                if let Some(alias) = alias {
                    statuses.entry(alias).or_default().insert(status);
                }
            }
        }
    }

    #[test]
    fn git_status_ancestor_deduplication_matches_original_with_aliases_and_mixed_states() {
        let mut changes: Vec<_> = (0..250).map(|index| {
            (format!("src/package-{}/nested/file-{index}.rs", index % 13),
                ['A', 'D', '?', 'M', 'R', '!'][index % 6])
        }).collect();
        changes.extend([
            ("src/package-0/nested/file-0.rs".into(), 'M'),
            ("src/package-0/nested/file-0.rs".into(), '?'),
            ("src/package-0/nested".into(), 'A'),
            ("../outside/path.txt".into(), 'D'),
            ("src".into(), 'R'),
        ]);
        for (root, canonical) in [
            ("/repo", "/repo"),
            ("/repo/nested/..", "/repo"),
            ("/workspace", "/canonical/workspace"),
            ("/repo/src", "/canonical/repo/src"),
        ] {
            for workdir in [root, canonical, "/repo", "/repo/src/deeper"] {
                for reversed in [false, true] {
                    if reversed { changes.reverse(); }
                    let mut explorer = FileExplorer::new_deferred(PathBuf::from(root));
                    explorer.canonical_root = PathBuf::from(canonical);
                    explorer.apply_git_status_changes(Some(Path::new(workdir)),
                        changes.iter().map(|(path, marker)| (path.as_str(), *marker)));
                    assert_eq!(explorer.git_statuses,
                        reference_git_statuses(Path::new(root), Path::new(canonical),
                            Path::new(workdir), &changes),
                        "root={root} canonical={canonical} workdir={workdir}");
                }
            }
        }
    }

    #[test]
    #[ignore = "manual comparative release benchmark"]
    fn benchmark_shared_explorer_git_ancestors() {
        let root = Path::new("/workspace");
        let canonical = Path::new("/canonical/workspace");
        let changes: Vec<_> = (0..20_000).map(|index|
            (format!("src/packages/shared/deep/tree/modules/component/file-{index}.rs"), 'M'))
            .collect();
        let expected = reference_git_statuses(root, canonical, canonical, &changes);
        let mut explorer = FileExplorer::new_deferred(root.to_path_buf());
        explorer.canonical_root = canonical.to_path_buf();
        let mut original = expected.clone();
        let mut samples = [Vec::new(), Vec::new()];
        for sample in 0..7 {
            for optimized in [sample % 2 == 0, sample % 2 != 0] {
                let started = std::time::Instant::now();
                let actual = if optimized {
                    explorer.apply_git_status_changes(Some(canonical),
                        changes.iter().map(|(path, marker)| (path.as_str(), *marker)));
                    &explorer.git_statuses
                } else {
                    reference_apply_git_statuses(&mut original, root, canonical, canonical, &changes);
                    samples[0].push(started.elapsed());
                    assert_eq!(std::hint::black_box(&original), &expected);
                    continue;
                };
                samples[1].push(started.elapsed());
                assert_eq!(std::hint::black_box(actual), &expected);
            }
        }
        samples.iter_mut().for_each(|samples| samples.sort());
        eprintln!("Explorer 20,000 changed files sharing eight ancestors: original={:?}, deduplicated={:?}",
            samples[0][3], samples[1][3]);
    }

    fn dir_entry(path: &str, name: &str, depth: usize) -> FileEntry {
        FileEntry {
            path: PathBuf::from(path),
            name: name.to_string(),
            is_dir: true,
            depth,
            expanded: false,
            git_status: GitStatus::Unmodified,
            git_status_summary: GitStatusSummary::default(),
        }
    }

    fn file_entry(path: &str, name: &str, depth: usize) -> FileEntry {
        FileEntry {
            path: PathBuf::from(path),
            name: name.to_string(),
            is_dir: false,
            depth,
            expanded: false,
            git_status: GitStatus::Unmodified,
            git_status_summary: GitStatusSummary::default(),
        }
    }

    fn reference_sticky_indices(explorer: &FileExplorer, height: usize) -> Vec<usize> {
        let offset = explorer.viewport_offset(height);
        let mut sticky: Vec<usize> = Vec::new();
        if offset == 0 { return sticky; }
        for row in 0..FileExplorer::sticky_limit(height) {
            let index = offset + row;
            let Some(entry) = explorer.entries.get(index) else { break; };
            let min_depth = sticky.last().map_or(0, |&parent| explorer.entries[parent].depth + 1);
            if sticky.last().is_some_and(|&parent| !entry.path.starts_with(&explorer.entries[parent].path)) { break; }
            let mut depth = entry.depth;
            let mut ancestor = None;
            for i in (0..index).rev() {
                if depth <= min_depth { break; }
                let parent = &explorer.entries[i];
                if parent.depth < depth {
                    depth = parent.depth;
                    if parent.is_dir && parent.expanded { ancestor = Some(i); }
                }
            }
            let Some(ancestor) = ancestor else { break; };
            sticky.push(ancestor);
        }
        sticky
    }

    fn wide_explorer(count: usize) -> FileExplorer {
        let mut root = dir_entry("/project/wide", "wide", 0);
        root.expanded = true;
        let mut entries = vec![root];
        entries.extend((0..count).map(|index| {
            file_entry(&format!("/project/wide/file-{index}.rs"), "file.rs", 1)
        }));
        make_explorer(entries)
    }

    fn reference_find_next(explorer: &FileExplorer, backwards: bool, first: bool) -> Option<usize> {
        let matches: Vec<_> = explorer.entries.iter().enumerate().filter_map(|(i, entry)| {
            explorer.find.results.rows.get(&entry.path).filter(|matched| {
                matched.matched || (!entry.expanded && matched.descendants > 0)
            }).map(|_| i)
        }).collect();
        if first {
            if backwards { matches.last() } else { matches.first() }
        } else if backwards {
            matches.iter().rev().find(|&&i| i < explorer.selected).or_else(|| matches.last())
        } else {
            matches.iter().find(|&&i| i > explorer.selected).or_else(|| matches.first())
        }.copied()
    }

    #[test]
    fn find_navigation_matches_full_scan_for_wrapping_collapsed_folders_and_no_hits() {
        let mut explorer = wide_explorer(40);
        for stride in [1, 3, 11, 100] {
            explorer.find.results.rows.clear();
            for (i, entry) in explorer.entries.iter().enumerate() {
                explorer.find.results.rows.insert(entry.path.clone(), crate::explorer_find::Match {
                    matched: i > 0 && i % stride == 0,
                    descendants: usize::from(i == 0),
                    ..Default::default()
                });
            }
            for expanded in [true, false] {
                explorer.entries[0].expanded = expanded;
                for selected in (0..45).chain([usize::MAX]) {
                    for backwards in [false, true] {
                        for first in [false, true] {
                            explorer.selected = selected;
                            let expected = reference_find_next(&explorer, backwards, first).unwrap_or(selected);
                            explorer.select_find_match(backwards, first);
                            assert_eq!(explorer.selected, expected);
                        }
                    }
                }
            }
        }
        explorer.entries.clear();
        explorer.select_find_match(false, true);
    }

    #[test]
    #[ignore = "comparative optimized benchmark of Explorer result navigation"]
    fn benchmark_explorer_find_navigation() {
        let mut explorer = wide_explorer(50_000);
        explorer.visible_height = 40;
        for entry in &explorer.entries {
            explorer.find.results.rows.insert(entry.path.clone(), crate::explorer_find::Match {
                matched: true, ..Default::default()
            });
        }
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..7 {
            for fast in [round % 2 == 0, round % 2 != 0] {
                explorer.selected = 0;
                let started = std::time::Instant::now();
                for step in 0..300 {
                    if fast { explorer.select_find_match(step % 3 == 0, false); }
                    else {
                        explorer.selected = reference_find_next(&explorer, step % 3 == 0, false).unwrap();
                        explorer.ensure_selection_visible();
                    }
                    std::hint::black_box(explorer.selected);
                }
                samples[usize::from(fast)].push(started.elapsed() / 300);
            }
        }
        for times in &mut samples { times.sort(); }
        eprintln!("Explorer next match / 50k rows: scan={:?} indexed={:?}", samples[0][3], samples[1][3]);
    }

    #[test]
    fn indexed_sticky_ancestors_match_reference_across_wide_tree() {
        let mut explorer = wide_explorer(10_000);
        for height in [0, 1, 2, 5, 20] {
            for offset in [0, 1, 2, 100, 9_990, 10_000] {
                explorer.scroll_offset = offset;
                assert_eq!(explorer.sticky_indices(height), reference_sticky_indices(&explorer, height));
            }
        }
        explorer.selected = 9_999;
        assert!(!explorer.collapse_current_deferred());
        assert_eq!(explorer.selected, 0);
    }

    #[test]
    fn indexed_reveal_handles_wide_trees_replacement_and_hash_collisions() {
        let mut explorer = wide_explorer(20_000);
        for index in [1, 20_000, 9_999, 20, 15_001] {
            let path = explorer.entries[index].path.clone();
            assert!(!explorer.reveal_file_deferred(&path));
            assert_eq!(explorer.selected, index);
        }
        let path = explorer.entries[15_000].path.clone();
        // A compact fingerprint is only a hint: a colliding/stale candidate
        // must never reveal the wrong file.
        explorer
            .path_indices
            .insert(FileExplorer::path_key(&path), 1);
        assert_eq!(explorer.find_path(&path), Some(15_000));
        explorer.entries[15_000].path = PathBuf::from("/project/replacement.rs");
        assert_eq!(explorer.find_path(&path), None);
        assert_eq!(
            explorer.find_path(Path::new("/project/replacement.rs")),
            Some(15_000)
        );
        explorer.rebuild_parent_indices();
        assert_eq!(
            explorer.find_path(Path::new("/project/replacement.rs")),
            Some(15_000)
        );
    }

    #[test]
    fn refresh_applies_current_git_decorations_without_copying_statuses_to_worker() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("changed.rs");
        std::fs::write(&path, "source").unwrap();
        let mut explorer = FileExplorer::new(workspace.path().to_path_buf());
        explorer.apply_git_status_changes(Some(workspace.path()), [("changed.rs", 'M')]);
        let request = explorer.refresh_request();
        explorer.apply_git_status_changes(Some(workspace.path()), [("changed.rs", 'A')]);
        let (canonical, entries, matches) = FileExplorer::build_refresh_entries(request);
        assert!(
            entries
                .iter()
                .all(|entry| entry.git_status == GitStatus::Unmodified)
        );
        explorer.apply_refresh_entries(canonical, entries, matches);
        assert_eq!(
            explorer.entries[explorer.find_path(&path).unwrap()].git_status,
            GitStatus::Added
        );
    }

    #[test]
    #[ignore = "comparative release benchmark of large-tree tab reveal"]
    fn benchmark_explorer_path_reveal() {
        let explorer = wide_explorer(50_000);
        let paths: Vec<_> = (0..500)
            .map(|index| explorer.entries[1 + (index * 97 % 50_000)].path.clone())
            .collect();
        let mut linear = Vec::new();
        let mut indexed = Vec::new();
        for round in 0..7 {
            for reuse in [round % 2 == 0, round % 2 != 0] {
                let start = std::time::Instant::now();
                for path in &paths {
                    let result = if reuse {
                        explorer.find_path(std::hint::black_box(path))
                    } else {
                        explorer
                            .entries
                            .iter()
                            .position(|entry| entry.path == *path)
                    };
                    std::hint::black_box(result);
                }
                if reuse {
                    indexed.push(start.elapsed());
                } else {
                    linear.push(start.elapsed());
                }
            }
        }
        linear.sort();
        indexed.sort();
        eprintln!(
            "Explorer 50k entries / 500 tab reveals: linear={:?}, indexed={:?}",
            linear[3], indexed[3]
        );
    }

    #[test]
    #[ignore = "manual release benchmark"]
    fn benchmark_explorer_sticky_ancestors() {
        let mut explorer = wide_explorer(50_000);
        explorer.scroll_offset = 49_980;
        let mut scanning = Vec::new();
        let mut indexed = Vec::new();
        for _ in 0..7 {
            let started = std::time::Instant::now();
            for _ in 0..500 {
                std::hint::black_box(reference_sticky_indices(std::hint::black_box(&explorer), 20));
            }
            scanning.push(started.elapsed());
            let started = std::time::Instant::now();
            for _ in 0..500 {
                std::hint::black_box(explorer.sticky_indices(std::hint::black_box(20)));
            }
            indexed.push(started.elapsed());
        }
        scanning.sort();
        indexed.sort();
        eprintln!("explorer sticky headers 50,000 siblings, 500 scrolls: scan={:?}, indexed={:?}", scanning[3], indexed[3]);
    }

    #[test]
    fn name_sort_ignores_case_and_keeps_directories_first_after_refresh() {
        let temp = tempfile::tempdir().unwrap();
        for name in ["HiRISC", "HiRISC_s7", "HiRISC_v2", "arm_cache", "axi_rv32"] {
            std::fs::create_dir(temp.path().join(name)).unwrap();
        }
        for name in ["README.md", "alpha.txt"] {
            std::fs::write(temp.path().join(name), "").unwrap();
        }
        for name in ["Zulu.rs", "beta.rs"] {
            std::fs::write(temp.path().join("HiRISC").join(name), "").unwrap();
        }

        let mut explorer = FileExplorer::new(temp.path().to_path_buf());
        assert_eq!(
            explorer
                .entries
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            [
                "arm_cache", "axi_rv32", "HiRISC", "HiRISC_s7", "HiRISC_v2", "alpha.txt", "README.md"
            ]
        );

        explorer.selected = 2;
        explorer.toggle_or_open_deferred();
        let (root, entries, matches) = FileExplorer::build_refresh_entries(explorer.refresh_request());
        explorer.apply_refresh_entries(root, entries, matches);
        assert_eq!(
            explorer
                .entries
                .iter()
                .map(|e| e.name.as_str())
                .collect::<Vec<_>>(),
            [
                "arm_cache", "axi_rv32", "HiRISC", "beta.rs", "Zulu.rs", "HiRISC_s7",
                "HiRISC_v2", "alpha.txt", "README.md"
            ]
        );
    }

    #[test]
    fn other_sort_modes_break_ties_with_case_insensitive_names() {
        let temp = tempfile::tempdir().unwrap();
        let modified = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for name in ["Zulu.rs", "alpha.rs", "Beta.rs"] {
            let file = std::fs::File::create(temp.path().join(name)).unwrap();
            file.set_times(std::fs::FileTimes::new().set_modified(modified))
                .unwrap();
        }
        let mut explorer = FileExplorer::new(temp.path().to_path_buf());
        for mode in [
            ExplorerSortMode::Size,
            ExplorerSortMode::Modified,
            ExplorerSortMode::Type,
        ] {
            explorer.sort_mode = mode;
            explorer.refresh();
            assert_eq!(
                explorer
                    .entries
                    .iter()
                    .map(|e| e.name.as_str())
                    .collect::<Vec<_>>(),
                ["alpha.rs", "Beta.rs", "Zulu.rs"],
                "{mode:?}"
            );
        }
    }

    #[test]
    fn sticky_scrolling_tracks_nested_folders_and_sibling_boundaries() {
        let mut explorer = make_explorer(vec![
            dir_entry("/project/src", "src", 0),
            dir_entry("/project/src/nested", "nested", 1),
            file_entry("/project/src/nested/a", "a", 2),
            file_entry("/project/src/nested/b", "b", 2),
            file_entry("/project/src/nested/c", "c", 2),
            file_entry("/project/src/other", "other", 1),
            dir_entry("/project/tests", "tests", 0),
            file_entry("/project/tests/a", "a", 1),
            file_entry("/project/tests/b", "b", 1),
            file_entry("/project/tests/c", "c", 1),
        ]);
        for entry in &mut explorer.entries {
            entry.expanded = entry.is_dir;
        }
        explorer.visible_height = 3;
        assert_eq!(explorer.viewport_indices(3), vec![0, 1, 2]);
        explorer.scroll_offset = 2;
        assert_eq!(explorer.viewport_indices(3), vec![0, 1, 4]);
        assert_eq!(explorer.entry_at_row(0, 3), Some(0));
        assert_eq!(explorer.entry_at_row(1, 3), Some(1));
        assert_eq!(explorer.entry_at_row(2, 3), Some(4));
        assert_eq!(explorer.entry_at_row(3, 3), None);

        explorer.scroll_offset = 4;
        assert_eq!(explorer.viewport_indices(3), vec![0, 5, 6]);
        explorer.scroll_offset = 6;
        assert_eq!(explorer.viewport_indices(3), vec![6, 7, 8]);
        explorer.scroll_down(100);
        assert_eq!(explorer.viewport_indices(3), vec![6, 8, 9]);

        // Moving up into a row covered by a header must reveal that file.
        explorer.selected = 9;
        for _ in 0..9 {
            explorer.move_up();
            assert!(explorer.viewport_indices(3).contains(&explorer.selected));
        }
        for _ in 0..9 {
            explorer.move_down();
            assert!(explorer.viewport_indices(3).contains(&explorer.selected));
        }
        explorer.scroll_offset = 2;
        explorer.selected = 3;
        explorer.ensure_selection_visible();
        assert!(explorer.viewport_indices(3).contains(&3));
    }

    #[test]
    fn sticky_scrolling_leaves_content_visible_in_tiny_and_deep_trees() {
        let mut path = PathBuf::from("/project");
        let mut entries = Vec::new();
        for depth in 0..8 {
            path.push("nested");
            let mut entry = dir_entry(path.to_str().unwrap(), "nested", depth);
            entry.expanded = true;
            entries.push(entry);
        }
        for name in ["a", "b", "c", "d", "e", "f", "g", "h"] {
            entries.push(file_entry(path.join(name).to_str().unwrap(), name, 8));
        }
        let mut explorer = make_explorer(entries);
        explorer.scroll_offset = 8;
        assert!(explorer.viewport_indices(0).is_empty());
        assert_eq!(explorer.viewport_indices(1), vec![8]);
        assert_eq!(explorer.viewport_indices(2), vec![0, 9]);
        assert_eq!(explorer.sticky_indices(8), vec![0, 1, 2, 3, 4]);
        assert_eq!(explorer.viewport_indices(8).last(), Some(&15));
        assert!(make_explorer(vec![]).viewport_indices(5).is_empty());
    }

    #[test]
    fn deferred_reveal_selects_after_worker_tree_rebuild() {
        let temp = tempfile::tempdir().expect("temporary workspace");
        let nested = temp.path().join("one").join("two");
        std::fs::create_dir_all(&nested).expect("nested directory");
        let target = nested.join("target.rs");
        std::fs::write(&target, "fn main() {}\n").expect("target file");

        let mut explorer = FileExplorer::new_deferred(temp.path().to_path_buf());
        explorer.visible_height = 2;
        assert!(explorer.reveal_file_deferred(&target));
        assert!(explorer.entries.is_empty(), "UI path must not scan directories");

        let request = explorer.refresh_request();
        let (canonical_root, entries, matches) = FileExplorer::build_refresh_entries(request);
        explorer.apply_refresh_entries(canonical_root, entries, matches);

        assert_eq!(explorer.entries[explorer.selected].path, target);
        assert!(explorer.selected >= explorer.scroll_offset);
        assert!(explorer.selected < explorer.scroll_offset + explorer.visible_height);
    }

    fn noncanonical_test_root(label: &str) -> (PathBuf, PathBuf) {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::current_dir()
            .expect("current directory")
            .join("target")
            .join("test-paths")
            .join(format!("vtcode-explorer-{label}-{unique}"));
        std::fs::create_dir_all(root.join("nested")).expect("test directory");
        let file = root.join("tracked.txt");
        std::fs::write(&file, "contents\n").expect("test file");
        (root, file)
    }

    #[test]
    fn git_status_matches_files_from_noncanonical_workspace_root() {
        let (root, file) = noncanonical_test_root("relative-root");
        let noncanonical_root = root.join("nested").join("..");
        let mut explorer = FileExplorer {
            find: Default::default(),
            root: noncanonical_root.clone(),
            canonical_root: std::fs::canonicalize(&noncanonical_root).expect("canonical root"),
            entries: Vec::new(),
            selected: 0,
            scroll_offset: 0,
            horizontal: Default::default(),
            visible_height: 5,
            git_statuses: FxHashMap::default(),
            sort_mode: ExplorerSortMode::Name,
            expanded_dirs: FxHashSet::default(),
            pending_reveal: None,
            parent_indices: Vec::new(),
            path_indices: FxHashMap::default(),
            directory_cache: DirectoryCache::default(),
            ignore_cache: IgnoreCache::default(),
        };
        explorer.git_statuses.insert(
            std::fs::canonicalize(&file).expect("canonical file"),
            GitStatus::Modified.into(),
        );
        explorer.scan_dir(&noncanonical_root, 0, false, None);
        let tracked = explorer
            .entries
            .iter()
            .find(|entry| {
                entry
                    .path
                    .file_name()
                    .is_some_and(|name| name == "tracked.txt")
            })
            .expect("tracked file in explorer");
        assert_eq!(tracked.git_status, GitStatus::Modified);

        drop(explorer);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn deferred_explorer_construction_does_not_scan_the_filesystem() {
        let root = PathBuf::from("/workspace");
        let explorer = FileExplorer::new_deferred(root.clone());

        assert_eq!(explorer.root, root);
        assert!(explorer.entries.is_empty());
    }

    #[test]
    fn deferred_directory_toggle_only_mutates_expansion_state() {
        let mut explorer = make_explorer(vec![
            dir_entry("/project/src", "src", 0),
            file_entry("/project/README.md", "README.md", 0),
        ]);
        let original_len = explorer.entries.len();

        let (path, tree_changed) = explorer.toggle_or_open_deferred();

        assert_eq!(path, None);
        assert!(tree_changed);
        assert_eq!(explorer.entries.len(), original_len);
        assert!(explorer.expanded_dirs.contains(Path::new("/project/src")));
    }

    #[test]
    fn background_git_status_updates_visible_file_decorations() {
        let mut explorer = make_explorer(vec![file_entry("/project/src/main.rs", "main.rs", 1)]);

        explorer.apply_git_status_changes(None, [("src/main.rs", 'M')]);

        assert_eq!(explorer.entries[0].git_status, GitStatus::Modified);
    }

    #[test]
    fn background_git_status_decorates_changed_file_ancestors() {
        let mut explorer = make_explorer(vec![
            dir_entry("/project/src", "src", 0),
            dir_entry("/project/src/editor", "editor", 1),
            file_entry("/project/src/editor/main.rs", "main.rs", 2),
        ]);

        explorer.apply_git_status_changes(None, [("src/editor/main.rs", 'M')]);

        assert!(
            explorer
                .entries
                .iter()
                .all(|entry| entry.git_status == GitStatus::Modified)
        );
    }

    #[test]
    fn directory_decoration_preserves_all_descendant_statuses() {
        let mut explorer = make_explorer(vec![
            dir_entry("/project/src", "src", 0),
            file_entry("/project/src/changed.rs", "changed.rs", 1),
            file_entry("/project/src/new.rs", "new.rs", 1),
        ]);

        explorer.apply_git_status_changes(
            None,
            [("src/changed.rs", 'M'), ("src/new.rs", 'A')],
        );

        assert_eq!(explorer.entries[0].git_status, GitStatus::Added);
        assert_eq!(
            explorer.entries[0]
                .git_status_summary
                .statuses()
                .collect::<Vec<_>>(),
            vec![GitStatus::Modified, GitStatus::Added]
        );
        assert_eq!(explorer.entries[1].git_status, GitStatus::Modified);
        assert_eq!(explorer.entries[2].git_status, GitStatus::Added);
    }

    #[test]
    fn directory_decoration_clears_with_git_status() {
        let mut explorer = make_explorer(vec![dir_entry("/project/src", "src", 0)]);
        explorer.apply_git_status_changes(None, [("src/main.rs", 'M')]);
        assert_eq!(explorer.entries[0].git_status, GitStatus::Modified);

        explorer.apply_git_status_changes(None, std::iter::empty());

        assert_eq!(explorer.entries[0].git_status, GitStatus::Unmodified);
        assert_eq!(
            explorer.entries[0].git_status_summary,
            GitStatusSummary::default()
        );
    }

    #[test]
    fn collapsed_filesystem_directory_keeps_descendant_statuses() {
        let (root, _) = noncanonical_test_root("collapsed-status");
        std::fs::create_dir_all(root.join("src").join("nested")).expect("nested directory");
        // This fixture lives under the checkout's ignored test-output folder.
        // Give it its own repository so those parent rules do not apply.
        let repo = git2::Repository::init(&root).expect("isolated fixture repository");
        repo.config().unwrap().set_str("core.excludesFile", "").unwrap();
        let canonical_root = std::fs::canonicalize(&root).expect("canonical root");
        let mut explorer = FileExplorer::new(root.clone());

        explorer.apply_git_status_changes(
            Some(&canonical_root),
            [("src/nested/changed.rs", 'M'), ("src/nested/new.rs", 'A')],
        );

        let src = explorer
            .entries
            .iter()
            .find(|entry| entry.name == "src")
            .expect("collapsed src directory");
        assert!(!src.expanded);
        assert_eq!(
            src.git_status_summary.statuses().collect::<Vec<_>>(),
            vec![GitStatus::Modified, GitStatus::Added]
        );
        assert_eq!(
            explorer
                .root_git_status_summary()
                .statuses()
                .collect::<Vec<_>>(),
            vec![GitStatus::Modified, GitStatus::Added]
        );

        drop(explorer);
        drop(repo);
        let _ = std::fs::remove_dir_all(root);
    }

    // ── calc_drop_dest_dir ──

    #[test]
    fn test_dest_dir_on_directory() {
        let entry = dir_entry("/project/src", "src", 0);
        let dest = FileExplorer::calc_drop_dest_dir(&entry);
        assert_eq!(dest, PathBuf::from("/project/src"));
    }

    #[test]
    fn test_dest_dir_on_file() {
        let entry = file_entry("/project/src/main.rs", "main.rs", 1);
        let dest = FileExplorer::calc_drop_dest_dir(&entry);
        assert_eq!(dest, PathBuf::from("/project/src"));
    }

    #[test]
    fn test_dest_dir_on_root_file() {
        let entry = file_entry("/Cargo.toml", "Cargo.toml", 0);
        let dest = FileExplorer::calc_drop_dest_dir(&entry);
        assert_eq!(dest, PathBuf::from("/"));
    }

    // ── is_valid_move ──

    #[test]
    fn test_reject_same_directory_move() {
        let drag = Path::new("/project/src/main.rs");
        let dest = Path::new("/project/src");
        assert!(!FileExplorer::is_valid_move(drag, dest));
    }

    #[test]
    fn test_reject_move_into_self() {
        let drag = Path::new("/project/src");
        let dest = Path::new("/project/src/sub");
        assert!(!FileExplorer::is_valid_move(drag, dest));
    }

    #[test]
    fn test_accept_cross_directory_move() {
        let drag = Path::new("/project/src/main.rs");
        let dest = Path::new("/project/tests");
        assert!(FileExplorer::is_valid_move(drag, dest));
    }

    #[test]
    fn test_accept_move_to_parent() {
        let drag = Path::new("/project/src/sub/file.rs");
        let dest = Path::new("/project/src");
        assert!(FileExplorer::is_valid_move(drag, dest));
    }

    #[test]
    fn test_reject_move_dir_into_own_child() {
        let drag = Path::new("/project/src");
        let dest = Path::new("/project/src/nested/deep");
        assert!(!FileExplorer::is_valid_move(drag, dest));
    }

    // ── move_up / move_down ──

    #[test]
    fn test_move_up_from_top_stays() {
        let mut exp = make_explorer(vec![file_entry("/a", "a", 0), file_entry("/b", "b", 0)]);
        exp.selected = 0;
        exp.move_up();
        assert_eq!(exp.selected, 0);
    }

    #[test]
    fn test_move_up_decrements() {
        let mut exp = make_explorer(vec![
            file_entry("/a", "a", 0),
            file_entry("/b", "b", 0),
            file_entry("/c", "c", 0),
        ]);
        exp.selected = 2;
        exp.move_up();
        assert_eq!(exp.selected, 1);
    }

    #[test]
    fn test_move_down_from_bottom_stays() {
        let mut exp = make_explorer(vec![file_entry("/a", "a", 0), file_entry("/b", "b", 0)]);
        exp.selected = 1;
        exp.move_down();
        assert_eq!(exp.selected, 1);
    }

    #[test]
    fn test_move_down_increments() {
        let mut exp = make_explorer(vec![file_entry("/a", "a", 0), file_entry("/b", "b", 0)]);
        exp.selected = 0;
        exp.move_down();
        assert_eq!(exp.selected, 1);
    }

    #[test]
    fn test_move_down_scrolls_when_past_visible() {
        let mut exp = make_explorer(vec![
            file_entry("/a", "a", 0),
            file_entry("/b", "b", 0),
            file_entry("/c", "c", 0),
            file_entry("/d", "d", 0),
            file_entry("/e", "e", 0),
            file_entry("/f", "f", 0),
        ]);
        exp.visible_height = 3;
        exp.selected = 2;
        exp.move_down();
        assert_eq!(exp.selected, 3);
        assert_eq!(exp.scroll_offset, 1);
    }

    #[test]
    fn test_move_up_scrolls_when_before_offset() {
        let mut exp = make_explorer(vec![
            file_entry("/a", "a", 0),
            file_entry("/b", "b", 0),
            file_entry("/c", "c", 0),
        ]);
        exp.selected = 1;
        exp.scroll_offset = 1;
        exp.move_up();
        assert_eq!(exp.selected, 0);
        assert_eq!(exp.scroll_offset, 0);
    }

    // ── scroll_up / scroll_down ──

    #[test]
    fn test_scroll_up_saturates_at_zero() {
        let mut exp = make_explorer(vec![file_entry("/a", "a", 0)]);
        exp.scroll_offset = 0;
        exp.scroll_up(5);
        assert_eq!(exp.scroll_offset, 0);
    }

    #[test]
    fn test_scroll_down_clamps_to_max() {
        let mut exp = make_explorer(vec![
            file_entry("/a", "a", 0),
            file_entry("/b", "b", 0),
            file_entry("/c", "c", 0),
        ]);
        exp.visible_height = 2;
        exp.scroll_down(100);
        // max = 3 - max(2,1) = 1
        assert_eq!(exp.scroll_offset, 1);
    }

    // ── empty explorer ──

    #[test]
    fn test_move_on_empty_entries() {
        let mut exp = make_explorer(vec![]);
        exp.move_up();
        exp.move_down();
        assert_eq!(exp.selected, 0);
    }

    // ── collapse_current ──

    #[test]
    fn test_collapse_navigates_to_parent() {
        let mut exp = make_explorer(vec![
            dir_entry("/project/src", "src", 0),
            file_entry("/project/src/main.rs", "main.rs", 1),
            file_entry("/project/src/lib.rs", "lib.rs", 1),
        ]);
        exp.entries[0].expanded = true;
        exp.selected = 2; // lib.rs
        exp.collapse_current();
        // Should navigate to parent dir "src" at index 0
        assert_eq!(exp.selected, 0);
    }

    // ── sort mode label ──

    #[test]
    fn test_sort_mode_labels() {
        assert_eq!(ExplorerSortMode::Name.label(), "Name");
        assert_eq!(ExplorerSortMode::Size.label(), "Size");
        assert_eq!(ExplorerSortMode::Modified.label(), "Date Modified");
        assert_eq!(ExplorerSortMode::Type.label(), "Type");
    }

    #[test]
    fn test_toggle_or_open_file_returns_path() {
        let mut exp = make_explorer(vec![
            dir_entry("/project/src", "src", 0),
            file_entry("/project/src/main.rs", "main.rs", 1),
        ]);
        exp.selected = 1; // file entry
        let result = exp.toggle_or_open();
        assert_eq!(result, Some(PathBuf::from("/project/src/main.rs")));
    }

    #[test]
    fn test_toggle_or_open_empty() {
        let mut exp = make_explorer(vec![]);
        let result = exp.toggle_or_open();
        assert_eq!(result, None);
    }

    #[test]
    fn test_collapse_all_clears_expanded() {
        let mut exp = make_explorer(vec![
            dir_entry("/project/src", "src", 0),
            file_entry("/project/src/main.rs", "main.rs", 1),
        ]);
        exp.entries[0].expanded = true;
        exp.expanded_dirs.insert(PathBuf::from("/project/src"));
        exp.selected = 1;
        exp.collapse_all();
        assert!(exp.expanded_dirs.is_empty());
        assert_eq!(exp.scroll_offset, 0);
    }

    #[test]
    fn test_collapse_current_on_empty() {
        let mut exp = make_explorer(vec![]);
        exp.collapse_current(); // should not panic
    }

    #[test]
    fn test_collapse_all_on_empty() {
        let mut exp = make_explorer(vec![]);
        exp.collapse_all(); // should not panic
    }

    #[test]
    fn test_is_valid_move_prevents_circular() {
        // Moving a parent into its own child should be rejected
        let drag = Path::new("/a/b/c");
        let dest = Path::new("/a/b/c/d");
        assert!(!FileExplorer::is_valid_move(drag, dest));
    }

    #[test]
    fn test_is_valid_move_allows_sibling() {
        // Moving between sibling directories should be allowed
        let drag = Path::new("/a/x/file.txt");
        let dest = Path::new("/a/b");
        assert!(FileExplorer::is_valid_move(drag, dest));
    }
}
