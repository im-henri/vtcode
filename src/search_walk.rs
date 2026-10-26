//! One include-aware, cancellable traversal for local and headless disk search.
//! Keep the original roots: starting at a narrower folder would lose inherited
//! ignore files and change root-relative exclusion rules.
use crate::{
    background::CancellationToken,
    search_paths::{Candidate, ExcludeRules, SearchPaths},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

#[cfg(test)]
thread_local! { static VISITED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
pub(crate) fn take_visited_entries() -> usize {
    VISITED.replace(0)
}

#[derive(Clone, Copy)]
pub(crate) struct Options {
    pub use_ignores: bool,
    pub parent_ignores: bool,
    pub global_ignores: bool,
    pub follow_links: bool,
}

pub(crate) fn paths(
    workspace: &Path,
    includes: Arc<SearchPaths>,
    excludes: Arc<SearchPaths>,
    settings: Arc<ExcludeRules>,
    options: Options,
    cancel: &CancellationToken,
) -> impl Iterator<Item = PathBuf> {
    let walks = includes
        .roots()
        .take_while(|_| !cancel.is_cancelled())
        .map(|folder| {
            let mut walk = ignore::WalkBuilder::new(folder);
            let folder = folder.to_owned();
            let workspace = workspace.to_owned();
            let includes = includes.clone();
            let excludes = excludes.clone();
            let settings = settings.clone();
            let cancel = cancel.clone();
            walk.hidden(false)
                .require_git(false)
                .parents(options.use_ignores && options.parent_ignores)
                .ignore(options.use_ignores)
                .git_ignore(options.use_ignores)
                .git_exclude(options.use_ignores)
                .git_global(options.use_ignores && options.global_ignores)
                .follow_links(options.follow_links)
                .filter_entry(move |entry| {
                    #[cfg(test)]
                    VISITED.set(VISITED.get() + 1);
                    // Let the iterator yield once to observe cancellation instead
                    // of internally filtering the rest of a large directory.
                    if cancel.is_cancelled() {
                        return true;
                    }
                    if crate::formatters::is_temporary_path(entry.path()) {
                        return false;
                    }
                    let candidate = Candidate::new(entry.path());
                    let in_scope = if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                        includes.may_descend(&candidate)
                    } else {
                        includes.includes(&candidate)
                    };
                    in_scope
                        && !settings.excludes(&candidate, &folder)
                        && !excludes.excludes(&candidate, &workspace)
                });
            walk.build()
        })
        .collect::<Vec<_>>();
    let cancel = cancel.clone();
    let workspace = workspace.to_owned();
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
                includes.includes(&candidate) && !excludes.excludes(&candidate, &workspace)
            }
        })
        .map(|entry| entry.into_path())
}
