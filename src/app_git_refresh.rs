//! Local watcher status reads: literal worktree prefixes, with complete reads
//! reserved for discovery, metadata, and rules that affect other files.
use super::*;
use crate::{
    background::CancellationToken,
    git_repositories::{Discovery, Scan},
};
use rustc_hash::FxHashSet;
use std::borrow::Cow;

#[cfg(test)]
#[path = "app_git_refresh_tests.rs"]
mod tests;

#[derive(Clone)]
pub(crate) struct WorktreePaths {
    pub paths: Vec<PathBuf>,
    keys: FxHashSet<String>,
    ignore_case: bool,
}

fn path_key(path: &str, ignore_case: bool) -> Cow<'_, str> {
    if path
        .bytes()
        .any(|byte| (cfg!(windows) && byte == b'\\') || (ignore_case && byte.is_ascii_uppercase()))
    {
        #[cfg(windows)]
        let mut key = path.replace('\\', "/");
        #[cfg(not(windows))]
        let mut key = path.to_owned();
        if ignore_case {
            key.make_ascii_lowercase();
        }
        return Cow::Owned(key);
    }
    Cow::Borrowed(path)
}

impl WorktreePaths {
    pub(crate) fn new(mut paths: Vec<PathBuf>) -> Self {
        for path in &mut paths {
            *path = path.components().collect();
        }
        paths.sort_unstable();
        paths.dedup();
        let keys: FxHashSet<_> = paths
            .iter()
            .map(|path| path_key(&path.to_string_lossy(), false).into_owned())
            .collect();
        // A directory already covers its descendants. Hashing ancestors keeps
        // a large watcher burst from comparing every prefix with every file.
        paths.retain(|path| {
            let text = path.to_string_lossy();
            let key = path_key(&text, false);
            !key.rmatch_indices('/')
                .any(|(end, _)| keys.contains(&key[..end]))
        });
        Self {
            paths,
            keys,
            ignore_case: false,
        }
    }

    pub(crate) fn set_ignore_case(&mut self, ignore_case: bool) {
        if self.ignore_case != ignore_case {
            self.ignore_case = ignore_case;
            self.keys = self
                .paths
                .iter()
                .map(|path| path_key(&path.to_string_lossy(), ignore_case).into_owned())
                .collect();
        }
    }

    pub(crate) fn contains(&self, path: &str) -> bool {
        let key = path_key(path.trim_end_matches('/'), self.ignore_case);
        self.keys.contains(key.as_ref())
            || key
                .rmatch_indices('/')
                .any(|(end, _)| self.keys.contains(&key[..end]))
    }
}

fn native_key(path: &Path) -> Cow<'_, Path> {
    #[cfg(windows)]
    {
        Cow::Owned(PathBuf::from(
            path_key(&path.to_string_lossy(), true).as_ref(),
        ))
    }
    #[cfg(not(windows))]
    {
        Cow::Borrowed(path)
    }
}

fn event_paths(
    paths: FxHashSet<PathBuf>,
    workspace: &Path,
    canonical: &Path,
) -> Option<Vec<PathBuf>> {
    let absolute_workspace = if workspace.is_absolute() {
        workspace.to_owned()
    } else {
        std::env::current_dir().ok()?.join(workspace)
    };
    paths
        .into_iter()
        .map(|path| {
            // Metadata can arrive through explicit callers too. Do not treat an
            // index/HEAD/config event as an ordinary worktree file.
            if path.components().any(|part| {
                part.as_os_str()
                    .to_str()
                    .is_some_and(|name| path_key(name, cfg!(windows)) == ".git")
            }) {
                return None;
            }
            let path = if path.is_absolute() {
                path
            } else {
                std::env::current_dir().ok()?.join(path)
            };
            if path
                .components()
                .any(|part| part == std::path::Component::ParentDir)
            {
                // Resolve traversals on the worker, including deleted suffixes.
                return repository_relative_path(canonical, &path)
                    .map(|relative| canonical.join(relative));
            }
            Some(
                if let Some(relative) =
                    crate::native_paths::strip_prefix(&path, &absolute_workspace)
                {
                    canonical.join(relative)
                } else {
                    path
                },
            )
        })
        .collect()
}

pub(super) fn collect(
    discovery: &mut Discovery,
    roots: Vec<PathBuf>,
    changed: Option<FxHashSet<PathBuf>>,
    workspace: &Path,
    cancel: &CancellationToken,
    include_untracked: bool,
) -> Vec<Scan> {
    if cancel.is_cancelled() {
        return Vec::new();
    }
    let changed = changed.and_then(|paths| event_paths(paths, workspace, &discovery.workspace));
    let boundaries: FxHashSet<_> = discovery
        .roots
        .iter()
        .map(|path| native_key(path).into_owned())
        .collect();
    let mut scans = Vec::new();
    for workdir in roots {
        if cancel.is_cancelled() {
            break;
        }
        let mut paths = if let Some(changed) = &changed {
            let mut prefixes = Vec::new();
            let mut full = false;
            for path in changed {
                if crate::native_paths::strip_prefix(&workdir, path).is_some() {
                    // Repository root/ancestor deletion or replacement.
                    full = true;
                    break;
                }
                let Some(mut relative) = crate::native_paths::strip_prefix(path, &workdir) else {
                    continue;
                };
                if path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        matches!(
                            path_key(name, cfg!(windows)).as_ref(),
                            ".gitignore" | ".gitattributes" | ".gitmodules"
                        )
                    })
                {
                    full = true;
                    break;
                }
                // Changes inside a nested worktree can dirty its superproject's
                // gitlink (or untracked repository entry). Ask for that boundary,
                // rather than a child filename that the outer index cannot see.
                let lookup = native_key(path);
                for (ancestor, lookup) in path.ancestors().skip(1).zip(lookup.ancestors().skip(1)) {
                    if crate::native_paths::same(ancestor, &workdir) {
                        break;
                    }
                    if boundaries.contains(lookup) {
                        relative = crate::native_paths::strip_prefix(ancestor, &workdir)
                            .unwrap_or(relative);
                    }
                }
                // Local status rows currently use UTF-8 display paths. An
                // unrepresentable name needs a complete snapshot so merging
                // cannot mistake two lossy spellings for the same file.
                if relative.to_str().is_none() {
                    full = true;
                    break;
                }
                prefixes.push(relative.to_owned());
            }
            if !full && prefixes.is_empty() {
                continue;
            }
            (!full).then(|| WorktreePaths::new(prefixes))
        } else {
            None
        };
        let snapshot = collect_source_control_status_impl(
            workdir.clone(),
            || {},
            cancel,
            |path| path.to_string_lossy().into_owned(),
            paths.as_mut(),
            include_untracked,
        );
        if cancel.is_cancelled() {
            break;
        }
        if !snapshot.repository_found || snapshot.workdir.as_ref() != Some(&workdir) {
            discovery.roots.retain(|root| root != &workdir);
            continue;
        }
        scans.push(Scan {
            root: workdir,
            files: snapshot.files,
            branch: snapshot.branch,
            worktree_paths: paths,
            include_untracked,
        });
    }
    scans
}
