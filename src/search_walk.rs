//! One include-aware, cancellable traversal for local and headless disk search.
//! Settings globs always use the workspace root. Ignore files never filter
//! workspace searches, including explicit roots outside the workspace.
use crate::{
    background::CancellationToken,
    native_paths::strip_spelling_prefix,
    search_paths::{Candidate, ExcludeRules, PathAliases, SearchPaths},
};
use rustc_hash::{FxHashMap, FxHashSet};
use std::{
    cmp::Reverse,
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, RwLock},
};

#[cfg(test)]
thread_local! { static VISITED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
pub(crate) fn take_visited_entries() -> usize {
    VISITED.replace(0)
}

pub(crate) struct Options {
    pub follow_links: bool,
    pub workers: usize,
    pub open_aliases: Option<Arc<OpenFileAliases>>,
    pub path_aliases: Arc<PathAliases>,
}

/// Keep a key per matching save destination. Follow symlinks like the editor
/// and retain distinct directory entries for hard links.
#[derive(Default)]
pub(crate) struct MatchedFiles {
    parents: FxHashMap<PathBuf, PathBuf>,
    seen: FxHashSet<PathBuf>,
}

impl MatchedFiles {
    pub(crate) fn insert(&mut self, path: &Path, is_symlink: bool) -> bool {
        if is_symlink && let Ok(target) = path.canonicalize() {
            return self.seen.insert(target);
        }
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return true;
        };
        if !self.parents.contains_key(parent) {
            let actual = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
            self.parents.insert(parent.to_path_buf(), actual);
        }
        self.seen.insert(self.parents[parent].join(name))
    }
}

pub(crate) fn has_parent_traversal(path: &Path) -> bool {
    path.components().any(|component| component == Component::ParentDir)
}

/// Project live files into the roots and followed links that can
/// name them. Ordinary file candidates need only an in-memory lookup.
#[derive(Default)]
pub(crate) struct OpenFileAliases {
    roots: FxHashMap<PathBuf, usize>,
    actual: BTreeMap<PathBuf, usize>,
    linked: OnceLock<RwLock<FxHashMap<PathBuf, usize>>>,
}

impl OpenFileAliases {
    pub(crate) fn new<'a>(
        open_paths: impl Iterator<Item = (usize, &'a Path)>,
        includes: &SearchPaths,
        follow_links: bool,
        cancel: &CancellationToken,
    ) -> Option<Arc<Self>> {
        let mut aliases = Self::default();
        let mut open_paths = open_paths
            .filter(|(_, path)| {
                follow_links || cfg!(windows) || includes.has_path_scopes() || !path.is_absolute()
                    || has_parent_traversal(path)
            })
            .peekable();
        open_paths.peek()?;

        let roots: Vec<_> = includes.roots()
            .take_while(|_| !cancel.is_cancelled())
            .filter_map(|root| root.canonicalize().ok().map(|actual| (root, actual)))
            .collect();
        for (index, path) in open_paths.take_while(|_| !cancel.is_cancelled()) {
            let Ok(actual) = path.canonicalize() else { continue; };
            for (root, actual_root) in &roots {
                if let Some(relative) = strip_spelling_prefix(&actual, actual_root) {
                    let walked = root.join(relative);
                    if walked != path {
                        aliases.roots.entry(walked).or_insert(index);
                    }
                }
            }
            if follow_links {
                aliases.actual.entry(actual).or_insert(index);
            }
        }
        (!aliases.roots.is_empty() || !aliases.actual.is_empty()).then(|| Arc::new(aliases))
    }

    pub(crate) fn get(&self, path: &Path) -> Option<usize> {
        self.roots.get(path).copied().or_else(|| {
            self.linked.get()?.read().unwrap().get(path).copied()
        })
    }

    fn register_link(&self, path: &Path) {
        if self.actual.is_empty() {
            return;
        }
        let Ok(target) = path.canonicalize() else { return; };
        // Canonical descendants are contiguous in component order.
        let mut descendants = self.actual.range(target.clone()..).map_while(|(file, &index)| {
            file.strip_prefix(&target).ok().map(|relative| (relative, index))
        });
        let Some(first) = descendants.next() else { return; };
        let mut linked = self.linked.get_or_init(RwLock::default).write().unwrap();
        for (relative, index) in std::iter::once(first).chain(descendants) {
            linked.entry(path.join(relative)).or_insert(index);
        }
    }
}

/// Only the portions of explicit roots that overlap through different names
/// need a physical key. Resolve roots once and keep file filtering lexical.
#[derive(Default)]
struct OverlappingAliases(Vec<(PathBuf, PathBuf)>);

impl OverlappingAliases {
    fn new(includes: &SearchPaths, cancel: &CancellationToken) -> Self {
        let mut roots = includes.roots().peekable();
        let first = roots.next();
        if roots.peek().is_none() {
            return Self::default();
        }

        let mut by_actual: BTreeMap<PathBuf, Vec<&Path>> = BTreeMap::new();
        for root in first.into_iter().chain(roots).take_while(|_| !cancel.is_cancelled()) {
            if let Ok(actual) = root.canonicalize() {
                by_actual.entry(actual).or_default().push(root);
            }
        }
        let mut aliases = BTreeMap::new();
        for (actual, spellings) in &by_actual {
            let mut overlaps = spellings.len() > 1;
            for parent in actual.ancestors().skip(1) {
                let Some(outer_spellings) = by_actual.get(parent) else { continue; };
                let relative = actual.strip_prefix(parent).expect("canonical ancestor");
                for outer in outer_spellings {
                    let walked = outer.join(relative);
                    if walked != spellings[0] {
                        aliases.insert(walked, actual.clone());
                        overlaps = true;
                    }
                }
            }
            if overlaps {
                aliases.extend(spellings.iter().map(|root| (root.to_path_buf(), actual.clone())));
            }
        }
        let mut aliases: Vec<_> = aliases.into_iter().collect();
        // A nested alias can refine how a path inside a larger alias resolves.
        aliases.sort_by_cached_key(|(path, _)| Reverse(path.components().count()));
        Self(aliases)
    }

    fn key(&self, path: &Path) -> Option<PathBuf> {
        self.0.iter().find_map(|(spelling, actual)| {
            strip_spelling_prefix(path, spelling).map(|relative| actual.join(relative))
        })
    }
}

pub(crate) fn paths(
    workspace: &Path,
    includes: Arc<SearchPaths>,
    excludes: Arc<SearchPaths>,
    settings: Arc<ExcludeRules>,
    options: Options,
    cancel: &CancellationToken,
) -> impl Iterator<Item = (PathBuf, bool)> {
    let aliases = OverlappingAliases::new(&includes, cancel);
    let mut seen = FxHashSet::default();
    let walks = includes
        .roots()
        .take_while(|_| !cancel.is_cancelled())
        .filter(|folder| {
            !crate::fs_ops::is_copy_temporary_path(folder)
                && !settings.excludes_tree(folder, workspace)
        })
        .map(|folder| {
            builder(
                workspace, folder, &includes, &excludes, &settings, &options, cancel,
            )
            .build()
        })
        .collect::<Vec<_>>();
    let cancel = cancel.clone();
    let workspace = workspace.to_owned();
    let path_aliases = options.path_aliases;
    walks
        .into_iter()
        .flatten()
        .take_while(move |_| !cancel.is_cancelled())
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        // ignore::Walk intentionally bypasses filter_entry for depth-zero
        // roots, which can themselves be explicitly included files.
        .filter(move |entry| {
            entry.depth() != 0 || {
                let candidate = Candidate::new(entry.path());
                includes.includes(&candidate)
                    && !excludes.excludes_with_aliases(&candidate, &workspace, &path_aliases)
                    && !settings.excludes_tree(entry.path(), &workspace)
            }
        })
        .filter(move |entry| aliases.key(entry.path()).is_none_or(|key| seen.insert(key)))
        .map(|entry| {
            let is_symlink = entry.path_is_symlink();
            (entry.into_path(), is_symlink)
        })
}

/// Traverse and search beside the disk with a bounded number of workers. Both
/// paths share exactly the same pruning and ignore configuration.
pub(crate) fn visit(
    workspace: &Path,
    includes: Arc<SearchPaths>,
    excludes: Arc<SearchPaths>,
    settings: Arc<ExcludeRules>,
    options: Options,
    cancel: &CancellationToken,
    visit: impl Fn(&Path, bool) + Sync,
) {
    let aliases = OverlappingAliases::new(&includes, cancel);
    let seen = Mutex::new(FxHashSet::default());
    for folder in includes.roots() {
        if cancel.is_cancelled() {
            break;
        }
        if crate::fs_ops::is_copy_temporary_path(folder) || settings.excludes_tree(folder, workspace) {
            continue;
        }
        builder(
            workspace, folder, &includes, &excludes, &settings, &options, cancel,
        )
        .threads(options.workers)
        .build_parallel()
        .run(|| {
            Box::new(|entry| {
                if cancel.is_cancelled() {
                    return ignore::WalkState::Quit;
                }
                if let Ok(entry) = entry
                    && entry.file_type().is_some_and(|kind| kind.is_file())
                    && (entry.depth() != 0 || {
                        let candidate = Candidate::new(entry.path());
                        includes.includes(&candidate)
                            && !excludes.excludes_with_aliases(&candidate, workspace, &options.path_aliases)
                            && !settings.excludes_tree(entry.path(), workspace)
                    })
                    && aliases.key(entry.path()).is_none_or(|key| seen.lock().unwrap().insert(key))
                {
                    visit(entry.path(), entry.path_is_symlink());
                }
                ignore::WalkState::Continue
            })
        });
    }
}

fn builder(
    workspace: &Path,
    folder: &Path,
    includes: &Arc<SearchPaths>,
    excludes: &Arc<SearchPaths>,
    settings: &Arc<ExcludeRules>,
    options: &Options,
    cancel: &CancellationToken,
) -> ignore::WalkBuilder {
    let mut walk = ignore::WalkBuilder::new(folder);
    let workspace = workspace.to_owned();
    let includes = includes.clone();
    let excludes = excludes.clone();
    let settings = settings.clone();
    let cancel = cancel.clone();
    let open_aliases = options.open_aliases.clone();
    let path_aliases = options.path_aliases.clone();
    walk.hidden(false)
        .require_git(false)
        .parents(false)
        .ignore(false)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .follow_links(options.follow_links)
        .filter_entry(move |entry| {
            #[cfg(test)]
            VISITED.set(VISITED.get() + 1);
            // Let the iterator yield once to observe cancellation instead
            // of internally filtering the rest of a large directory.
            if cancel.is_cancelled() {
                return true;
            }
            if crate::fs_ops::is_copy_temporary_name(entry.file_name()) {
                return false;
            }
            let candidate = Candidate::new(entry.path());
            let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
            let in_scope = if is_dir {
                includes.may_descend(&candidate)
            } else {
                includes.includes(&candidate)
            };
            let included = in_scope
                && !settings.excludes(&candidate, &workspace)
                && !excludes.excludes_with_aliases(&candidate, &workspace, &path_aliases);
            if included && entry.path_is_symlink()
                && let Some(aliases) = &open_aliases
            {
                // The walker applies this filter before scheduling the entry.
                aliases.register_link(entry.path());
            }
            included
        });
    walk
}
