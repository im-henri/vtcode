//! Workspace scans run beside the remote disk. Unsaved/open buffers are searched
//! locally and excluded from this request; their contents are never retransmitted
//! merely to perform a workspace search.
use super::*;
use crate::{
    app::App, app_global_search::GlobalSearchSpec, background::CancellationToken,
    search_regex::SearchRegex,
};
use std::{collections::BTreeMap, sync::Arc};

#[cfg(test)]
#[path = "remote_replace_tests.rs"]
mod replace_tests;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Query {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub include: String,
    pub exclude: String,
    pub limit: usize,
    pub files_exclude: BTreeMap<String, serde_json::Value>,
    pub search_exclude: BTreeMap<String, serde_json::Value>,
    pub use_ignores: bool,
    pub parent_ignores: bool,
    pub global_ignores: bool,
    pub follow_links: bool,
    pub open_files: Vec<RemotePath>,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Hit {
    pub path: RemotePath,
    pub line: usize,
    pub start: usize,
    pub end: usize,
    pub text: Arc<str>,
}

/// Only path metadata is needed to apply the node's path rules to local drafts.
/// No buffer contents cross the connection for filtering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Scope {
    pub include: String,
    pub exclude: String,
    pub open_files: Vec<RemotePath>,
}

#[derive(Default)]
pub(super) struct ScopeCache(Option<(Scope, Vec<usize>)>);

pub(crate) fn scope(root: &Path, query: Scope, cancel: &CancellationToken) -> io::Result<Reply> {
    if cancel.is_cancelled() { return Err(io::ErrorKind::Interrupted.into()); }
    let include = crate::search_paths::SearchPaths::cached(root, &query.include);
    let exclude = crate::search_paths::SearchPaths::cached(root, &query.exclude);
    let mut indices = Vec::new();
    for (index, path) in query.open_files.iter().enumerate() {
        if cancel.is_cancelled() { return Err(io::ErrorKind::Interrupted.into()); }
        let native = path.resolve(root)?;
        let candidate = crate::search_paths::Candidate::new(&native);
        if include.includes_open(&candidate) && !exclude.excludes(&candidate, root) {
            indices.push(index);
        }
    }
    Ok(Reply::SearchScope { indices })
}

impl workspace::Workspace {
    pub(crate) fn search_scope(&self, paths: &[PathBuf], include: &str, exclude: &str,
        cancel: &CancellationToken) -> io::Result<Vec<usize>> {
        if cancel.is_cancelled() { return Err(io::ErrorKind::Interrupted.into()); }
        if paths.is_empty() || (include.trim().is_empty() && exclude.trim().is_empty()) {
            return Ok((0..paths.len()).collect());
        }
        let query = Scope { include: include.into(), exclude: exclude.into(),
            open_files: paths.iter().map(|path| self.remote_path(path)).collect::<io::Result<_>>()? };
        if let Some((previous, indices)) = &self.search_scope_cache.lock().unwrap().0
            && previous == &query {
            return Ok(indices.clone());
        }
        let Reply::SearchScope { indices } = self.client.call_cancellable(Command::SearchScope(query.clone()), cancel)? else {
            return Err(io::Error::other("Invalid remote search scope reply"));
        };
        if indices.iter().any(|&index| index >= paths.len())
            || indices.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(io::Error::other("Invalid remote search scope indices"));
        }
        // Query text and content versions do not affect a pure path filter.
        // Keep one bounded entry; never hold its mutex across network I/O.
        self.search_scope_cache.lock().unwrap().0 = Some((query, indices.clone()));
        Ok(indices)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Replace {
    pub options: crate::workspace_replace::Options,
    pub files: Vec<crate::workspace_replace::File<RemotePath>>,
}

pub(crate) fn replace(
    root: &Path,
    query: Replace,
    cancel: &CancellationToken,
    mut changed: impl FnMut(&RemotePath),
) -> io::Result<Reply> {
    // This is the existing remote search-result budget, not a file-size cap.
    // Validate the entire request before beginning any file mutation.
    let mut total = 0usize;
    let mut seen = rustc_hash::FxHashSet::default();
    if query.options.query.is_empty() || query.files.len() > 100_000 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid workspace replacement targets",
        ));
    }
    for file in &query.files {
        total = total
            .checked_add(file.matches.len())
            .ok_or_else(|| io::Error::other("Replacement count overflow"))?;
        if total > 100_000 || !seen.insert(&file.path) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Too many or duplicate replacement targets",
            ));
        }
    }
    drop(seen);
    let program =
        crate::app_search::SnapshotReplacer::workspace(&query.options).map_err(io::Error::other)?;
    let mut outcome = crate::workspace_replace::Outcome::default();
    for file in query.files {
        if cancel.is_cancelled() {
            outcome.cancelled = true;
            break;
        }
        let result = file.path.resolve(root).and_then(|native| {
            crate::workspace_replace::file(&native, file.matches, &program, cancel)
        });
        match result {
            Ok((count, written)) => {
                outcome.replacements += count;
                outcome.files_changed += usize::from(count > 0);
                outcome.written_files += usize::from(written);
                if written {
                    changed(&file.path);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {
                outcome.cancelled = true;
                break;
            }
            Err(_) => outcome.failures += 1,
        }
    }
    outcome.cancelled |= cancel.is_cancelled();
    Ok(Reply::Replaced(outcome))
}

pub(crate) fn execute(root: &Path, query: Query, cancel: &CancellationToken) -> io::Result<Reply> {
    if cancel.is_cancelled() { return Err(io::ErrorKind::Interrupted.into()); }
    let regex = SearchRegex::workspace(
        &query.text,
        query.case_sensitive,
        query.whole_word,
        query.regex,
    )
    .map_err(io::Error::other)?;
    let includes = crate::search_paths::SearchPaths::cached(root, &query.include);
    let excludes = crate::search_paths::SearchPaths::cached(root, &query.exclude);
    let setting_excludes = Arc::new(crate::search_paths::ExcludeRules::new(
        &query.files_exclude,
        &query.search_exclude,
    ));
    let open_files = query
        .open_files
        .iter()
        .map(|path| git::from_native(&path.resolve(root)?, root))
        .collect::<io::Result<rustc_hash::FxHashSet<_>>>()?;
    let mut visited = rustc_hash::FxHashSet::default();
    let mut results = Vec::new();
    let mut truncated = false;
    for path in crate::search_walk::paths(
        root, includes, excludes, setting_excludes,
        crate::search_walk::Options {
            use_ignores: query.use_ignores, parent_ignores: query.parent_ignores,
            global_ignores: query.global_ignores, follow_links: query.follow_links,
        }, cancel,
    ) {
        if cancel.is_cancelled() {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let path = path.as_path();
        let remote = git::from_native(path, root)?;
        if open_files.contains(&remote) || !visited.insert(remote) {
            continue;
        }
        let spec = GlobalSearchSpec {
            query: &query.text,
            regex: Some(&regex),
            include: "",
            exclude: "",
            max_results: query.limit.min(100_000),
            case_sensitive: query.case_sensitive,
            open_buffers: None,
        };
        App::search_single_file(path, root, spec, &mut results, &mut truncated, cancel);
        if truncated {
            break;
        }
    }
    if cancel.is_cancelled() { return Err(io::ErrorKind::Interrupted.into()); }
    let hits = results
        .into_iter()
        .map(|result| {
            Ok(Hit {
                path: git::from_native(&result.file_path, root)?,
                line: result.line_num,
                start: result.col_start,
                end: result.col_end,
                text: result.line_text,
            })
        })
        .collect::<io::Result<_>>()?;
    Ok(Reply::Search { hits, truncated })
}
