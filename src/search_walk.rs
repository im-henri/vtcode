//! One include-aware, cancellable traversal for local and headless disk search.
//! Settings globs always use the workspace root. Ignore files never filter
//! workspace searches, including explicit roots outside the workspace.
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
        .filter(|folder| !settings.excludes_tree(folder, workspace))
        .map(|folder| {
            builder(
                workspace, folder, &includes, &excludes, &settings, options, cancel,
            )
            .build()
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
                    && !settings.excludes_tree(entry.path(), &workspace)
            }
        })
        .map(|entry| entry.into_path())
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
    workers: usize,
    visit: impl Fn(&Path) + Sync,
) {
    for folder in includes.roots() {
        if cancel.is_cancelled() {
            break;
        }
        if settings.excludes_tree(folder, workspace) { continue; }
        builder(
            workspace, folder, &includes, &excludes, &settings, options, cancel,
        )
        .threads(workers)
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
                        includes.includes(&candidate) && !excludes.excludes(&candidate, workspace)
                            && !settings.excludes_tree(entry.path(), workspace)
                    })
                {
                    visit(entry.path());
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
    options: Options,
    cancel: &CancellationToken,
) -> ignore::WalkBuilder {
    let mut walk = ignore::WalkBuilder::new(folder);
    let workspace = workspace.to_owned();
    let includes = includes.clone();
    let excludes = excludes.clone();
    let settings = settings.clone();
    let cancel = cancel.clone();
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
            let candidate = Candidate::new(entry.path());
            let in_scope = if entry.file_type().is_some_and(|kind| kind.is_dir()) {
                includes.may_descend(&candidate)
            } else {
                includes.includes(&candidate)
            };
            in_scope
                && !settings.excludes(&candidate, &workspace)
                && !excludes.excludes(&candidate, &workspace)
        });
    walk
}
