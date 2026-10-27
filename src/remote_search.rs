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

#[cfg(test)]
#[path = "remote_search_streaming_tests.rs"]
mod streaming_tests;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Query {
    /// Opt in so older clients still receive one complete reply.
    #[serde(default)]
    pub stream: bool,
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
    let mut collected = Vec::new();
    let Reply::Search { hits, truncated } = execute_with_workers(root, query, cancel, 1, |hits| {
        collected.extend(hits);
        Ok(())
    })?
    else {
        unreachable!()
    };
    collected.extend(hits);
    Ok(Reply::Search {
        hits: collected,
        truncated,
    })
}

pub(crate) fn execute_streaming(
    root: &Path,
    query: Query,
    cancel: &CancellationToken,
    publish: impl FnMut(Vec<Hit>) -> io::Result<()>,
) -> io::Result<Reply> {
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(8);
    execute_with_workers(root, query, cancel, workers, publish)
}

struct Batches<F> {
    hits: Vec<Hit>,
    bytes: usize,
    total: usize,
    limit: usize,
    truncated: bool,
    published: bool,
    last_publish: std::time::Instant,
    publish: F,
}

impl<F: FnMut(Vec<Hit>) -> io::Result<()>> Batches<F> {
    fn flush(&mut self) -> io::Result<()> {
        if !self.hits.is_empty() {
            (self.publish)(std::mem::take(&mut self.hits))?;
            self.bytes = 0;
            self.published = true;
            self.last_publish = std::time::Instant::now();
        }
        Ok(())
    }

    fn flush_due(&mut self) -> io::Result<()> {
        if !self.published || self.last_publish.elapsed() >= std::time::Duration::from_millis(40) {
            self.flush()?;
        }
        Ok(())
    }

    fn append(
        &mut self,
        root: &Path,
        matches: Vec<crate::app::GlobalSearchResult>,
        limited: bool,
    ) -> io::Result<()> {
        let remaining = self.limit.saturating_sub(self.total);
        self.truncated |= limited || matches.len() > remaining;
        let Some(first) = matches.first() else {
            return self.flush_due();
        };
        // Encode match paths once per file, rather than once per match.
        let path = git::from_native(&first.file_path, root)?;
        for found in matches.into_iter().take(remaining) {
            let size = found
                .line_text
                .len()
                .saturating_add(path.0.len())
                .saturating_add(128);
            if !self.hits.is_empty()
                && (self.hits.len() >= 128 || self.bytes.saturating_add(size) > 128 * 1024)
            {
                self.flush()?;
            }
            self.bytes = self.bytes.saturating_add(size);
            self.hits.push(Hit {
                path: path.clone(),
                line: found.line_num,
                start: found.col_start,
                end: found.col_end,
                text: found.line_text,
            });
            self.total += 1;
        }
        self.flush_due()
    }
}

fn execute_with_workers(
    root: &Path,
    query: Query,
    cancel: &CancellationToken,
    workers: usize,
    publish: impl FnMut(Vec<Hit>) -> io::Result<()>,
) -> io::Result<Reply> {
    if cancel.is_cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
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
    let deduplicate = includes.roots().count() > 1;
    let visited = std::sync::Mutex::new(rustc_hash::FxHashSet::default());
    let selected = |path: &Path| -> io::Result<bool> {
        // A single traversal cannot repeat a path. Avoid allocating and
        // hashing a wire path for every nonmatching file in that common case.
        if open_files.is_empty() && !deduplicate {
            return Ok(true);
        }
        // Preserve the node's identity normalization for absolute includes,
        // including ordinary/extended Windows spellings of an open file.
        let path = git::from_native(path, root)?;
        Ok(!open_files.contains(&path) && (!deduplicate || visited.lock().unwrap().insert(path)))
    };
    let options = crate::search_walk::Options {
        use_ignores: query.use_ignores,
        parent_ignores: query.parent_ignores,
        global_ignores: query.global_ignores,
        follow_links: query.follow_links,
    };
    let limit = query.limit.min(100_000);
    let spec = GlobalSearchSpec {
        query: &query.text,
        regex: Some(&regex),
        include: "",
        exclude: "",
        max_results: limit,
        case_sensitive: query.case_sensitive,
        open_buffers: None,
    };
    let mut batches = Batches {
        hits: Vec::new(),
        bytes: 0,
        total: 0,
        limit,
        truncated: false,
        published: false,
        last_publish: std::time::Instant::now(),
        publish,
    };
    if workers == 1 {
        for path in
            crate::search_walk::paths(root, includes, excludes, setting_excludes, options, cancel)
        {
            if cancel.is_cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            if !selected(&path)? {
                continue;
            }
            let mut results = Vec::new();
            let mut truncated = false;
            App::search_single_file(
                &path,
                root,
                GlobalSearchSpec {
                    max_results: limit.saturating_sub(batches.total),
                    ..spec
                },
                &mut results,
                &mut truncated,
                cancel,
            );
            batches.append(root, results, truncated)?;
            if batches.truncated {
                break;
            }
        }
    } else {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        };
        // Keep the producer bounded on slow SSH links. Cancelling this token
        // stops traversal and in-file scans without cancelling the caller when
        // the search merely reaches its result limit.
        let scan_cancel = CancellationToken::new();
        let accepted = AtomicUsize::new(0);
        std::thread::scope(|scope| -> io::Result<()> {
            let (sender, receiver) = mpsc::sync_channel(workers * 2);
            let scan_cancel_worker = scan_cancel.clone();
            let accepted = &accepted;
            let selected = &selected;
            scope.spawn(move || {
                crate::worker_priority::lower_current_thread();
                crate::search_walk::visit(
                    root,
                    includes,
                    excludes,
                    setting_excludes,
                    options,
                    &scan_cancel_worker,
                    workers,
                    |path| {
                        if scan_cancel_worker.is_cancelled() {
                            return;
                        }
                        match selected(path) {
                            Ok(true) => {}
                            Ok(false) => return,
                            Err(error) => {
                                let _ = sender.send(Err(error));
                                return;
                            }
                        }
                        let mut results = Vec::new();
                        let mut truncated = false;
                        App::search_single_file(
                            path,
                            root,
                            GlobalSearchSpec {
                                max_results: limit.saturating_sub(accepted.load(Ordering::Relaxed)),
                                ..spec
                            },
                            &mut results,
                            &mut truncated,
                            &scan_cancel_worker,
                        );
                        if !results.is_empty() || truncated {
                            let _ = sender.send(Ok((results, truncated)));
                        }
                    },
                );
            });
            let result = (|| {
                loop {
                    if cancel.is_cancelled() {
                        return Err(io::ErrorKind::Interrupted.into());
                    }
                    match receiver.recv_timeout(std::time::Duration::from_millis(20)) {
                        Ok(matches) => {
                            let (results, truncated) = matches?;
                            batches.append(root, results, truncated)?;
                            accepted.store(batches.total, Ordering::Relaxed);
                            if batches.truncated {
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => batches.flush_due()?,
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                Ok(())
            })();
            scan_cancel.cancel();
            // Release workers blocked on publishing before joining the walk.
            drop(receiver);
            result
        })?;
    }
    if cancel.is_cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    Ok(Reply::Search {
        hits: batches.hits,
        truncated: batches.truncated,
    })
}
