//! Resolve ignore rules while building Explorer snapshots, never while drawing.
use super::{FileEntry, git_cache::Metrics};
use crate::git_ignore::Rules as IgnoreRules;
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};

/// Cache repository boundaries per directory, including negative lookups. A
/// workspace may contain several repositories even when its own root has none.
/// One boundary probe per new directory replaces discovery/index I/O per file.
pub(super) struct IgnoreForest {
    root: PathBuf,
    repositories: Vec<IgnoreMatcher>,
    repository_roots: FxHashMap<PathBuf, usize>,
    directories: FxHashMap<PathBuf, Option<usize>>,
    /// Repository of the previous entry's parent; listings visit siblings
    /// consecutively. Repository indices are stable for the forest's life.
    last_directory: Option<(PathBuf, Option<usize>)>,
}

/// One repository's Git index for a decoration batch, read lazily at most
/// once, plus whether the current parent directory contains tracked files.
#[derive(Default)]
pub(super) struct IndexLookup {
    index: Option<Option<git2::Index>>,
    parent: Option<(PathBuf, bool)>,
}
impl IgnoreForest {
    pub(super) fn new(root: &Path, metrics: &mut Metrics) -> Self {
        let matcher = IgnoreMatcher::new(root, metrics);
        let repository = matcher.as_ref().map(|_| 0);
        let repository_roots = matcher
            .as_ref()
            .map(|matcher| (matcher.workdir.clone(), 0))
            .into_iter()
            .collect();
        Self {
            root: root.into(),
            repository_roots,
            repositories: matcher.into_iter().collect(),
            directories: [(root.to_owned(), repository)].into_iter().collect(),
            last_directory: None,
        }
    }
    #[cfg(test)]
    pub(super) fn empty(root: &Path) -> Self {
        Self {
            root: root.into(),
            repositories: Vec::new(),
            repository_roots: FxHashMap::default(),
            directories: [(root.to_owned(), None)].into_iter().collect(),
            last_directory: None,
        }
    }
    fn repository(&mut self, directory: &Path, metrics: &mut Metrics) -> Option<usize> {
        let mut missing = Vec::new();
        let mut ancestor = directory;
        while !self.directories.contains_key(ancestor) {
            if !ancestor.starts_with(&self.root) {
                return None;
            }
            missing.push(ancestor);
            ancestor = ancestor.parent()?;
        }
        let mut current = self.directories[ancestor];
        for directory in missing.into_iter().rev() {
            metrics.boundary();
            if directory.join(".git").symlink_metadata().is_ok() {
                if let Some(known) = self.repository_roots.get(directory) {
                    current = Some(*known);
                } else if let Some(matcher) = IgnoreMatcher::new(directory, metrics) {
                    // A malformed .git entry must not masquerade as a new
                    // repository after discovery fell back to its parent.
                    if matcher.workdir == directory {
                        current = Some(self.repositories.len());
                        self.repository_roots
                            .insert(directory.into(), self.repositories.len());
                        self.repositories.push(matcher);
                    }
                }
            }
            self.directories.insert(directory.into(), current);
        }
        // Decorations already have a separate byte/count budget. Bound the
        // additional directory memo too, including deep recursive name finds.
        // Keeping repository matchers avoids rereading their config/index.
        if self.directories.len() > 4_096 {
            self.directories
                .retain(|path, _| path == &self.root || path == directory);
        }
        current
    }
    pub(super) fn ignored(
        &mut self,
        root: &Path,
        canonical_root: &Path,
        entry: &FileEntry,
        indices: &mut FxHashMap<usize, IndexLookup>,
        metrics: &mut Metrics,
    ) -> Option<bool> {
        let path = canonical_root.join(entry.path.strip_prefix(root).ok()?);
        let parent = path.parent()?;
        let repository = match &self.last_directory {
            Some((last, repository)) if last.as_path() == parent => *repository,
            _ => {
                let repository = self.repository(parent, metrics);
                self.last_directory = Some((parent.to_owned(), repository));
                repository
            }
        }?;
        self.repositories[repository].ignored(
            &path,
            entry.is_dir,
            indices.entry(repository).or_default(),
            metrics,
        )
    }
    pub(super) fn invalidate_directories(&mut self, roots: &FxHashSet<PathBuf>) {
        self.last_directory = None;
        for repository in &mut self.repositories {
            repository.invalidate_directories(roots);
        }
        self.directories.retain(|path, _| {
            path == &self.root || !path.ancestors().any(|ancestor| roots.contains(ancestor))
        });
    }
}

pub(super) struct IgnoreMatcher {
    git_dir: PathBuf,
    workdir: PathBuf,
    rules: IgnoreRules,
}

impl IgnoreMatcher {
    pub(super) fn new(canonical_root: &Path, metrics: &mut Metrics) -> Option<Self> {
        metrics.repository();
        let repo = git2::Repository::discover(canonical_root).ok()?;
        let workdir = repo.workdir()?;
        let workdir = std::fs::canonicalize(workdir).unwrap_or_else(|_| workdir.to_owned());
        canonical_root.strip_prefix(&workdir).ok()?;
        Some(Self {
            git_dir: repo.path().into(),
            rules: IgnoreRules::new(&repo, &workdir, &mut || metrics.rule()),
            workdir,
        })
    }

    /// `path` is the canonical path of the entry inside this repository.
    pub(super) fn ignored(
        &mut self,
        path: &Path,
        is_dir: bool,
        lookup: &mut IndexLookup,
        metrics: &mut Metrics,
    ) -> Option<bool> {
        metrics.matched();
        let ignored = self.rules.is_ignored(path, is_dir, &mut || metrics.rule());
        // Bound retained directory rule chains even during whole-tree searches.
        // Evict between lookups, after recursive parent resolution has finished.
        self.rules.trim(1_024);
        if !ignored {
            return Some(false);
        }
        let relative = path.strip_prefix(&self.workdir).ok()?;
        // Git metadata cannot be tracked; displaying .git alone must not read
        // a potentially huge index when no user files match ignore rules.
        if relative.components().any(|part| part.as_os_str() == ".git") {
            return Some(true);
        }
        let index = lookup.index
            .get_or_insert_with(|| {
                metrics.index();
                git2::Repository::open(&self.git_dir)
                    .and_then(|repo| repo.index())
                    .ok()
            })
            .as_ref()?;
        #[cfg(windows)]
        if relative.to_str().is_none() {
            return None;
        }
        // A parent without any tracked file below it cannot contain tracked
        // exceptions. Ignored folders (node_modules, target, build output)
        // are expanded as whole listings: check the index once per parent,
        // not four stage lookups per child.
        if let Some(parent) = relative.parent().filter(|parent| !parent.as_os_str().is_empty()) {
            let untracked = match &lookup.parent {
                Some((cached, untracked)) if cached.as_path() == parent => *untracked,
                _ => {
                    let untracked = index.find_prefix(index_prefix(parent)).is_err();
                    lookup.parent = Some((parent.to_owned(), untracked));
                    untracked
                }
            };
            if untracked {
                return Some(true);
            }
        }
        // Ignore rules apply only to untracked paths. Include conflict stages
        // and gitlinks, and keep folders containing tracked files undimmed.
        if (0..=3).any(|stage| index.get_path(relative, stage).is_some()) {
            return Some(false);
        }
        if is_dir && index.find_prefix(index_prefix(relative)).is_ok() {
            return Some(false);
        }
        Some(true)
    }

    pub(super) fn invalidate_directories(&mut self, roots: &FxHashSet<PathBuf>) {
        self.rules.invalidate_directories(roots);
    }
}

/// The index path prefix of every tracked file below `relative`.
fn index_prefix(relative: &Path) -> Vec<u8> {
    let mut prefix = relative.as_os_str().as_encoded_bytes().to_vec();
    #[cfg(windows)]
    for byte in &mut prefix {
        if *byte == b'\\' {
            *byte = b'/';
        }
    }
    prefix.push(b'/');
    prefix
}
