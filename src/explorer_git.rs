//! Resolve ignore rules while building Explorer snapshots, never while drawing.
use super::{FileEntry, git_cache::Metrics};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use rustc_hash::{FxHashMap, FxHashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Cache repository boundaries per directory, including negative lookups. A
/// workspace may contain several repositories even when its own root has none.
/// One boundary probe per new directory replaces discovery/index I/O per file.
pub(super) struct IgnoreForest {
    root: PathBuf,
    repositories: Vec<IgnoreMatcher>,
    repository_roots: FxHashMap<PathBuf, usize>,
    directories: FxHashMap<PathBuf, Option<usize>>,
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
        }
    }
    #[cfg(test)]
    pub(super) fn empty(root: &Path) -> Self {
        Self {
            root: root.into(),
            repositories: Vec::new(),
            repository_roots: FxHashMap::default(),
            directories: [(root.to_owned(), None)].into_iter().collect(),
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
        indices: &mut FxHashMap<usize, Option<Option<git2::Index>>>,
        metrics: &mut Metrics,
    ) -> Option<bool> {
        let path = canonical_root.join(entry.path.strip_prefix(root).ok()?);
        let Some(repository) = self.repository(path.parent()?, metrics) else {
            return None;
        };
        self.repositories[repository].ignored(
            root,
            canonical_root,
            entry,
            indices.entry(repository).or_default(),
            metrics,
        )
    }
    pub(super) fn invalidate_directories(&mut self, roots: &FxHashSet<PathBuf>) {
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
            rules: IgnoreRules::new(&repo, &workdir, metrics),
            workdir,
        })
    }

    pub(super) fn ignored(
        &mut self,
        root: &Path,
        canonical_root: &Path,
        entry: &FileEntry,
        index: &mut Option<Option<git2::Index>>,
        metrics: &mut Metrics,
    ) -> Option<bool> {
        let path = canonical_root.join(entry.path.strip_prefix(root).ok()?);
        metrics.matched();
        let ignored = self.rules.is_ignored(&path, entry.is_dir, metrics);
        // Bound retained directory rule chains even during whole-tree searches.
        // Evict between lookups, after recursive parent resolution has finished.
        if self.rules.directories.len() >= 1_024 {
            self.rules.directories.clear();
        }
        if !ignored {
            return Some(false);
        }
        let relative = path.strip_prefix(&self.workdir).ok()?;
        // Git metadata cannot be tracked; displaying .git alone must not read
        // a potentially huge index when no user files match ignore rules.
        if relative.components().any(|part| part.as_os_str() == ".git") {
            return Some(true);
        }
        let index = index
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
        // Ignore rules apply only to untracked paths. Include conflict stages
        // and gitlinks, and keep folders containing tracked files undimmed.
        if (0..=3).any(|stage| index.get_path(relative, stage).is_some()) {
            return Some(false);
        }
        if entry.is_dir {
            let mut prefix = relative.as_os_str().as_encoded_bytes().to_vec();
            #[cfg(windows)]
            for byte in &mut prefix {
                if *byte == b'\\' {
                    *byte = b'/';
                }
            }
            prefix.push(b'/');
            if index.find_prefix(prefix).is_ok() {
                return Some(false);
            }
        }
        Some(true)
    }

    pub(super) fn invalidate_directories(&mut self, roots: &FxHashSet<PathBuf>) {
        self.rules
            .directories
            .retain(|path, _| !path.ancestors().any(|p| roots.contains(p)));
    }
}

struct DirectoryRules {
    // Only nonempty rule sets, shared with descendants. Match a flat chain,
    // rather than walking/hashing every ancestor for every file.
    matchers: Vec<Arc<Gitignore>>,
    ignored: bool,
}

struct IgnoreRules {
    workdir: PathBuf,
    directories: FxHashMap<PathBuf, DirectoryRules>,
    global: Vec<Arc<Gitignore>>,
    case_insensitive: bool,
}

impl IgnoreRules {
    fn new(repo: &git2::Repository, workdir: &Path, metrics: &mut Metrics) -> Self {
        let config = repo.config().ok();
        let case_insensitive = config
            .as_ref()
            .and_then(|config| config.get_bool("core.ignorecase").ok())
            .unwrap_or(false);
        let mut global = Vec::new();
        if let Some(path) = config
            .as_ref()
            .and_then(|config| config.get_path("core.excludesfile").ok())
            .or_else(ignore::gitignore::gitconfig_excludes_path)
        {
            append_rules(
                &mut global,
                read_rules(workdir, &workdir.join(path), case_insensitive, metrics),
            );
        }
        append_rules(
            &mut global,
            read_rules(
                workdir,
                &repo.commondir().join("info/exclude"),
                case_insensitive,
                metrics,
            ),
        );
        Self {
            workdir: workdir.into(),
            directories: FxHashMap::default(),
            global,
            case_insensitive,
        }
    }

    fn directory(&mut self, path: &Path, metrics: &mut Metrics) {
        if self.directories.contains_key(path) {
            return;
        }
        let ignored = path != self.workdir && self.is_ignored(path, true, metrics);
        // Git does not read ignore files beneath an excluded directory: a
        // child negation cannot resurrect a path with an ignored parent.
        let mut matchers = if ignored {
            Vec::new()
        } else if path == self.workdir {
            self.global.clone()
        } else {
            self.directories[path.parent().unwrap()].matchers.clone()
        };
        if !ignored {
            let file = path.join(".gitignore");
            if !file.is_symlink() {
                append_rules(
                    &mut matchers,
                    read_rules(path, &file, self.case_insensitive, metrics),
                );
            }
        }
        self.directories
            .insert(path.to_owned(), DirectoryRules { matchers, ignored });
    }

    fn is_ignored(&mut self, path: &Path, is_dir: bool, metrics: &mut Metrics) -> bool {
        if path.file_name().is_some_and(|name| name == ".git") {
            return true;
        }
        let Some(parent) = path
            .parent()
            .filter(|parent| parent.starts_with(&self.workdir))
        else {
            return false;
        };
        self.directory(parent, metrics);
        if self.directories[parent].ignored {
            return true;
        }
        // The deepest .gitignore wins, including standalone negations of
        // patterns in parent files (libgit2's ignore lookup drops those).
        for matcher in self.directories[parent].matchers.iter().rev() {
            match matcher.matched(path, is_dir) {
                ignore::Match::Ignore(_) => return true,
                ignore::Match::Whitelist(_) => return false,
                ignore::Match::None => {}
            }
        }
        false
    }
}

fn append_rules(matchers: &mut Vec<Arc<Gitignore>>, rules: Gitignore) {
    if !rules.is_empty() {
        matchers.push(Arc::new(rules));
    }
}

fn read_rules(
    root: &Path,
    file: &Path,
    case_insensitive: bool,
    metrics: &mut Metrics,
) -> Gitignore {
    metrics.rule();
    let mut builder = GitignoreBuilder::new(root);
    let _ = builder.case_insensitive(case_insensitive);
    let _ = builder.add(file); // Keep valid patterns even if another line is invalid.
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}
