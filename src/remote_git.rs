//! Repository discovery and native libgit2 status collection live on the remote
//! filesystem. Bootstrap metadata and the primary snapshot can arrive before
//! nested discovery; all filesystem work stays on the node.
use super::*;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct StatusProgressOptions {
    pub preferred: Option<RemotePath>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum StatusProgress {
    Repository { root: RemotePath, branch: String },
    Snapshot { discovery: Discovery, scan: Box<Scan> },
}

pub(crate) struct StatusQuery {
    pub include_untracked: bool,
    pub known: Option<Discovery>,
    pub parent_open: bool,
    pub closed: Vec<RemotePath>,
    pub changed: Option<Vec<RemotePath>>,
    pub revalidate: bool,
}

pub(crate) enum StatusUpdate {
    Repository { root: PathBuf, branch: String },
    Snapshot { discovery: crate::git_repositories::Discovery, scan: Box<crate::git_repositories::Scan> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Discovery {
    pub roots: Vec<RemotePath>,
    pub parent: Option<RemotePath>,
    pub errors: Vec<crate::git_trust::Failure<RemotePath>>,
}
#[derive(Clone)]
pub(super) struct CachedStatus {
    pub scan: crate::git_repositories::Scan,
    pub checked: std::time::Instant,
}
pub(super) type Snapshots = super::cache::Cache<CachedStatus>;

pub(super) struct SourceState {
    pub checked: std::time::Instant,
    pub epoch: u64,
}
impl Default for SourceState {
    fn default() -> Self {
        Self {
            checked: std::time::Instant::now(),
            epoch: 0,
        }
    }
}

fn repository_contains(repository: &RemotePath, path: &RemotePath) -> bool {
    repository.1 == path.1
        && (repository.0.is_empty()
            || repository.0 == path.0
            || path
                .0
                .strip_prefix(repository.0.as_slice())
                .is_some_and(|tail| tail.starts_with(b"/")))
}

fn open_roots(discovery: &Discovery, parent_open: bool, closed: &[RemotePath]) -> Vec<RemotePath> {
    let mut roots = discovery.roots.clone();
    if parent_open && let Some(parent) = &discovery.parent {
        roots.push(parent.clone());
    }
    roots.retain(|root| !closed.contains(root));
    roots
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Scan {
    #[serde(default)]
    pub operation: Option<crate::git_operation::State>,
    pub root: RemotePath,
    pub files: Vec<crate::app::GitFileChange<RemotePath>>,
    pub branch: String,
    pub worktree_paths: Option<WorktreeScope>,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct WorktreeScope {
    pub paths: Vec<RemotePath>,
    pub ignore_case: bool,
}
/// Completion signal for an in-flight Git source request.
#[derive(Default)]
pub(crate) struct SourceFlight {
    done: std::sync::Mutex<bool>,
    ready: std::sync::Condvar,
}

impl SourceFlight {
    fn wait(&self) {
        let mut done = self.done.lock().unwrap_or_else(|error| error.into_inner());
        while !*done {
            done = self.ready.wait(done).unwrap_or_else(|error| error.into_inner());
        }
    }
}

/// Publishes completion (success, error or unwind) and retires the flight.
struct FlightGuard<'a> {
    map: &'a std::sync::Mutex<std::collections::HashMap<(RemotePath, bool), std::sync::Arc<SourceFlight>>>,
    key: (RemotePath, bool),
    flight: std::sync::Arc<SourceFlight>,
}

impl Drop for FlightGuard<'_> {
    fn drop(&mut self) {
        self.map.lock().unwrap_or_else(|error| error.into_inner()).remove(&self.key);
        *self.flight.done.lock().unwrap_or_else(|error| error.into_inner()) = true;
        self.flight.ready.notify_all();
    }
}

/// Git blob identities of the `head` and `index` texts in [`Sources`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SourceRevisions {
    pub head: Option<[u8; 20]>,
    pub index: Option<[u8; 20]>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Sources {
    pub repository: RemotePath,
    pub head: Option<std::sync::Arc<str>>,
    pub index: Option<std::sync::Arc<str>>,
    // For an unmerged index, `index` holds the available conflict baseline.
    // HEAD retains its actual contents for explicit comparisons.
    #[serde(default)]
    pub conflicted: bool,
    pub ignored: bool,
    pub staged: bool,
    /// Present when the texts can be revalidated by identity (not for
    /// conflicts). Older peers omit it and always send the texts.
    #[serde(default)]
    pub revisions: Option<SourceRevisions>,
    /// The request's `known` revisions still match: `head` and `index` were
    /// omitted, and the caller keeps the texts it already has.
    #[serde(default)]
    pub unchanged: bool,
    /// A blob exceeded the request's `max_bytes`; no texts were sent.
    #[serde(default)]
    pub oversized: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct NetworkResult {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct CommitDiff {
    pub path: RemotePath,
    pub base: String,
    pub target: String,
    pub labels: crate::app::DiffLabels,
}
pub(crate) fn network(root: &Path, args: Vec<String>, push: bool) -> NetworkResult {
    let prepared = if push {
        crate::app_git_branch::push_command(root)
    } else {
        crate::git_repositories::git_command(root).map(|command| (command, args))
    };
    match prepared.and_then(|(mut command, args)| {
        vtcode::acp::without_controlling_terminal(&mut command);
        command
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(args)
            .output()
            .map_err(|error| error.to_string())
    }) {
        Ok(result) => NetworkResult {
            stdout: String::from_utf8_lossy(&result.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&result.stderr).into_owned(),
            success: result.status.success(),
        },
        Err(error) => NetworkResult {
            stdout: String::new(),
            stderr: error,
            success: false,
        },
    }
}
pub(crate) fn sources(
    root: &Path,
    file: RemotePath,
    repository: Option<RemotePath>,
    staged: bool,
    known: Option<SourceRevisions>,
    max_bytes: Option<u64>,
) -> io::Result<Reply> {
    let file = file.resolve(root)?;
    let repository = repository
        .as_ref()
        .map(|path| path.resolve(root))
        .transpose()?;
    let repo = crate::git_repositories::open_file_repository(&file, repository.as_deref())
        .map_err(io::Error::other)?;
    let workdir = std::fs::canonicalize(
        repo.workdir()
            .ok_or_else(|| io::Error::other("Bare repositories have no working files"))?,
    )?;
    let relative = crate::app_git::repository_relative_path(&workdir, &file)
        .ok_or_else(|| io::Error::other("File is outside the repository"))?;
    let ignored = repo
        .status_file(&relative)
        .is_ok_and(|status| status.is_ignored());
    let head_id = crate::app_git::head_file_id(&repo, &relative, staged).map_err(io::Error::other)?;
    let (index_id, conflicted) = crate::app_git::line_diff_index_id(&repo, &relative).map_err(io::Error::other)?;
    if let Some(limit) = max_bytes {
        // Object headers give sizes without inflating the blobs.
        let odb = repo.odb().map_err(io::Error::other)?;
        let oversized = |id: Option<git2::Oid>| id.is_some_and(|id| odb.read_header(id)
            .is_ok_and(|(size, _)| size as u64 > limit));
        if oversized(head_id) || oversized(index_id) {
            return Ok(Reply::GitSources(Sources {
                repository: from_native(&workdir, root)?, head: None, index: None, conflicted, ignored,
                staged, revisions: None, unchanged: false, oversized: true,
            }));
        }
    }
    let bytes = |id: git2::Oid| <[u8; 20]>::try_from(id.as_bytes()).ok();
    let revisions = (!conflicted).then(|| SourceRevisions {
        head: head_id.and_then(bytes),
        index: index_id.and_then(bytes),
    });
    // Status refreshes (saves, focus, periodic revalidation) rarely change
    // HEAD or the index. Avoid resending whole files for an unchanged blob.
    let unchanged = revisions.is_some() && revisions == known;
    let (head, index) = if unchanged {
        (None, None)
    } else {
        (crate::app_git::blob_file_content(&repo, head_id).map_err(io::Error::other)?,
            crate::app_git::blob_file_content(&repo, index_id).map_err(io::Error::other)?)
    };
    Ok(Reply::GitSources(Sources {
        repository: from_native(&workdir, root)?,
        head,
        index,
        conflicted,
        ignored,
        staged,
        revisions,
        unchanged,
        oversized: false,
    }))
}

pub(crate) fn from_native(path: &Path, root: &Path) -> io::Result<RemotePath> {
    #[cfg(windows)]
    if path.is_absolute()
        && root.is_absolute()
        && let Some(relative) = crate::app_git::repository_relative_path(root, path)
    {
        // libgit2 returns ordinary drive paths, whereas filesystem canonical
        // paths may use the extended-length prefix. Preserve one remote identity
        // for both spellings, including historical files that no longer exist.
        return RemotePath::new(relative.to_string_lossy().replace('\\', "/").into_bytes());
    }
    #[cfg(windows)]
    if path.is_absolute() {
        // A parent repository (or an explicitly opened outside file) needs a
        // volume-qualified path. URI paths normalize Win32 extended prefixes
        // without treating a Windows spelling as POSIX-relative components.
        let uri = url::Url::from_file_path(path)
            .map_err(|_| io::Error::other("Unsupported absolute Windows path"))?;
        let mut bytes = super::lsp::uri_path(&uri)
            .ok_or_else(|| io::Error::other("Invalid absolute Windows path URI"))?;
        while bytes.last() == Some(&b'/') { bytes.pop(); }
        return RemotePath::absolute(&bytes);
    }
    let (native, absolute) = match path.strip_prefix(root) {
        Ok(relative) => (relative, false),
        Err(_) => (path, true),
    };
    #[cfg(unix)]
    let bytes = {
        use std::os::unix::ffi::OsStrExt;
        native.as_os_str().as_bytes().to_vec()
    };
    #[cfg(windows)]
    let bytes = native.to_string_lossy().replace('\\', "/").into_bytes();
    if !absolute {
        RemotePath::new(bytes)
    } else {
        RemotePath::absolute(&bytes)
    }
}

fn relative_native_path(path: PathBuf) -> io::Result<RemotePath> {
    #[cfg(unix)]
    let mut bytes = { use std::os::unix::ffi::OsStringExt; path.into_os_string().into_vec() };
    #[cfg(not(unix))]
    let mut bytes = path.to_str().ok_or_else(|| io::Error::other("Git path is not representable as UTF-8"))?.replace('\\', "/").into_bytes();
    // libgit2 returns nested repositories as untracked directory entries.
    if bytes.last() == Some(&b'/') { bytes.pop(); }
    RemotePath::new(bytes)
}

pub(crate) fn status(
    root: &Path,
    known: Option<Discovery>,
    parent_open: bool,
    closed: Vec<RemotePath>,
    include_untracked: bool,
    (changed, revalidate): (Option<Vec<RemotePath>>, bool),
    cancel: &crate::background::CancellationToken,
) -> io::Result<Reply> {
    status_reporting(root, StatusQuery { known, parent_open, closed, include_untracked, changed, revalidate },
        None, cancel, |_| Ok(()))
}

pub(crate) fn status_reporting(
    root: &Path, query: StatusQuery, progress: Option<StatusProgressOptions>,
    cancel: &crate::background::CancellationToken,
    mut report: impl FnMut(StatusProgress) -> io::Result<()>,
) -> io::Result<Reply> {
    let StatusQuery { known, parent_open, closed, include_untracked, changed, revalidate } = query;
    if cancel.is_cancelled() {
        return Err(io::Error::from(io::ErrorKind::Interrupted));
    }
    let scoped = known.is_some();
    let closed = closed.iter().map(|path| path.resolve(root)).collect::<io::Result<Vec<_>>>()?;
    let preferred = progress.as_ref().and_then(|options| options.preferred.as_ref())
        .map(|path| path.resolve(root)).transpose()?;
    let mut announced = false;
    let mut published = None;
    let mut failure = None;
    let mut early_errors = Vec::new();
    let mut found = |workspace: &Path, repository: &Path, repo: &git2::Repository, fallback: bool| {
        if progress.is_none() || announced || cancel.is_cancelled() || closed.iter().any(|path| path == repository)
            || (!repository.starts_with(workspace) && !parent_open)
            || !fallback && preferred.as_deref().is_some_and(|preferred| preferred != repository)
        { return; }
        announced = true;
        let publish = (|| -> io::Result<()> {
            report(StatusProgress::Repository {
                root: from_native(repository, root)?,
                branch: crate::git_repositories::branch_name(repo).map_err(io::Error::other)?,
            })?;
            if cancel.is_cancelled() { return Err(io::ErrorKind::Interrupted.into()); }
            let snapshot = crate::app_git::read_source_control_status(repo, repository, cancel,
                Path::to_path_buf, &mut None, include_untracked, false);
            if let Some(error) = snapshot.error { early_errors.push(error); return Ok(()); }
            if cancel.is_cancelled() { return Err(io::ErrorKind::Interrupted.into()); }
            if snapshot.workdir.as_deref() != Some(repository) { return Ok(()); }
            let scan = encode_scan(crate::git_repositories::Scan {
                root: repository.to_owned(), files: snapshot.files, branch: snapshot.branch,
                operation: snapshot.operation, worktree_paths: None, include_untracked,
            }, root)?;
            let discovery = encode_discovery(crate::git_repositories::Discovery {
                workspace: workspace.to_owned(),
                roots: if repository.starts_with(workspace) { vec![repository.to_owned()] } else { Vec::new() },
                parent: (!repository.starts_with(workspace)).then(|| repository.to_owned()), errors: Vec::new(),
            }, root)?;
            report(StatusProgress::Snapshot { discovery, scan: Box::new(scan) })?;
            published = Some(repository.to_owned());
            Ok(())
        })();
        if let Err(error) = publish { failure = Some(error); cancel.cancel(); }
    };
    let mut discovery = match known {
        Some(known) => crate::git_repositories::Discovery {
            errors: Vec::new(),
            roots: known
                .roots
                .iter()
                .map(|path| path.resolve(root))
                .collect::<io::Result<_>>()?,
            parent: known
                .parent
                .as_ref()
                .map(|path| path.resolve(root))
                .transpose()?,
            workspace: root.to_path_buf(),
        },
        None => crate::git_repositories::discover_reporting(root, cancel, |workspace, repository, repo| found(workspace, repository, repo, false)),
    };
    let mut roots = discovery.roots.clone();
    if parent_open && let Some(parent) = &discovery.parent { roots.push(parent.clone()); }
    roots.retain(|path| !closed.contains(path));
    // Known roots or a missing restored selection need the same fast initial
    // snapshot. A primary already scanned during discovery is never reread.
    if progress.is_some()
        && let Some(repository) = preferred.as_ref().filter(|path| roots.contains(path)).or_else(|| roots.first())
        && let Ok(repo) = git2::Repository::open(repository)
    { found(&discovery.workspace, repository, &repo, true); }
    if let Some(error) = failure { return Err(error); }
    discovery.errors.extend(early_errors);
    roots.retain(|path| published.as_ref() != Some(path));
    let changed = if scoped {
        changed.map(|paths| paths.iter().map(|path| path.resolve(root)).collect::<io::Result<_>>()).transpose()?
    } else { None };
    let changed = changed.map(|paths| crate::app_git::refresh::WorktreeEvents { paths, revalidate });
    let scans = crate::app_git::refresh::collect_with_paths(&mut discovery, roots, changed, root,
        cancel, include_untracked, Path::to_path_buf);
    let discovery = encode_discovery(discovery, root)?;
    let scans = scans.into_iter().map(|scan| encode_scan(scan, root)).collect::<io::Result<_>>()?;
    if cancel.is_cancelled() {
        return Err(io::Error::from(io::ErrorKind::Interrupted));
    }
    Ok(Reply::GitStatus { discovery, scans })
}

fn encode_discovery(discovery: crate::git_repositories::Discovery, root: &Path) -> io::Result<Discovery> {
    let errors = discovery.errors.into_iter().map(|error| {
        error.try_map(|path| from_native(&path, root))
    }).collect::<io::Result<_>>()?;
    Ok(Discovery {
        errors,
        roots: discovery
            .roots
            .iter()
            .map(|path| from_native(path, root))
            .collect::<io::Result<_>>()?,
        parent: discovery
            .parent
            .as_ref()
            .map(|path| from_native(path, root))
            .transpose()?,
    })
}

fn encode_scan(scan: crate::git_repositories::Scan<PathBuf>, root: &Path) -> io::Result<Scan> {
    let worktree_paths = scan.worktree_paths.map(|scope| {
        Ok::<_, io::Error>(WorktreeScope {
            ignore_case: scope.ignore_case(),
            paths: scope.paths.into_iter().map(relative_native_path).collect::<io::Result<_>>()?,
        })
    }).transpose()?;
    Ok(Scan {
        operation: scan.operation,
        root: from_native(&scan.root, root)?,
        files: scan.files.into_iter().map(|file| file.try_map_paths(relative_native_path)).collect::<io::Result<_>>()?,
        branch: scan.branch,
        worktree_paths,
    })
}

impl workspace::Workspace {
    pub(crate) fn git_file_stats(
        &self,
        root: &Path,
        change: crate::app::GitFileChange,
        cancel: &crate::background::CancellationToken,
    ) -> io::Result<crate::app_source_control_hover::ChangeStats> {
        if !self.client.hello().git_file_stats {
            return Ok(crate::app_source_control_hover::ChangeStats::Unavailable);
        }
        let repo = self.remote_path(root)?;
        let change = change.try_map_paths(|path| self.remote_path(&root.join(path))?.relative_to(&repo))?;
        let Reply::GitFileStats(stats) = self.client.call_cancellable(
            Command::GitFileStats { repo, change },
            cancel,
        )? else {
            return Err(io::Error::other("Invalid Git file statistics reply"));
        };
        Ok(stats)
    }

    pub(crate) fn git_diff_sources(&self, key: &Path, hint: Option<&Path>, staged: bool,
        known: Option<crate::git_diff_sources::Revisions>, cancel: &crate::background::CancellationToken,
    ) -> io::Result<crate::git_diff_sources::Sources> {
        let file = self.remote_path(key)?;
        // An unstaged diff compares against the index blob. Gutter markers
        // usually fetched that exact blob already; offer its identity so an
        // unchanged index is not sent again, and fill the text in locally.
        let cached = (known.is_none() && !staged).then(|| {
            self.git_sources.lock().unwrap().get(&file)
                .filter(|(_, sources)| !sources.staged && !sources.conflicted)
                .and_then(|(_, sources)| Some((sources.revisions?.index?, sources.index.clone()?)))
        }).flatten();
        let request_known = known.or_else(|| cached.as_ref().map(|(id, _)| crate::git_diff_sources::Revisions {
            base: Some(*id), index: Some(*id),
        }));
        let repository = hint.map(|path| self.remote_path(path)).transpose()?;
        let Reply::GitDiffSources(mut sources) = self.client.call_cancellable(
            Command::GitDiffSources { file, repository, staged, known: request_known }, cancel)?
        else { return Err(io::Error::other("Invalid conditional Git source reply")); };
        if let Some((id, text)) = cached
            && sources.base.is_none()
            && sources.revisions.base == Some(id)
        {
            sources.base = Some(text);
        }
        Ok(sources)
    }

    pub(super) fn refresh_git_source_cache(&self, force: bool) -> u64 {
        let mut state = self.git_source_state.lock().unwrap();
        if force || state.checked.elapsed() >= std::time::Duration::from_secs(30) {
            // Git sources are tagged with their epoch: older entries become
            // stale and are revalidated by blob identity, not downloaded again.
            *self.blame_sources.lock().unwrap() = Default::default();
            state.epoch = state.epoch.wrapping_add(1);
            state.checked = std::time::Instant::now();
        }
        state.epoch
    }
    pub(crate) fn commit_details(
        &self,
        root: &Path,
        hash: String,
    ) -> io::Result<crate::commit_details::CommitData> {
        let repo = self.remote_path(root)?;
        let Reply::CommitDetails(data) = self.client.call(Command::CommitDetails { repo, hash })?
        else {
            return Err(io::Error::other("Invalid commit details reply"));
        };
        Ok(data)
    }
    pub(crate) fn commit_diff(
        &self,
        root: &Path,
        hash: String,
        file: crate::commit_details::CommitFile,
        limit: u64,
    ) -> io::Result<crate::commit_details::CommitDiff> {
        let repo = self.remote_path(root)?;
        let Reply::CommitDiff(diff) = self.client.call(Command::CommitDiff {
            repo,
            hash,
            file,
            limit,
        })?
        else {
            return Err(io::Error::other("Invalid commit diff reply"));
        };
        Ok(crate::commit_details::prepare_diff(
            crate::commit_details::DiffContents {
                path: self.view_key(&diff.path),
                base: diff.base,
                target: diff.target,
                labels: diff.labels,
            },
        ))
    }
    pub(crate) fn git_sources(
        &self,
        key: &Path,
        hint: Option<&Path>,
        staged: bool,
    ) -> io::Result<std::sync::Arc<Sources>> {
        self.git_sources_limited(key, hint, staged, None)?
            .ok_or_else(|| io::Error::other("Unexpected oversized Git source reply"))
    }

    /// Start fetching a file's Git baseline while the file itself is still
    /// being opened, so gutter markers need no extra round trip afterwards.
    /// Blobs larger than `limit` are skipped (and not cached).
    pub(crate) fn prefetch_git_sources(&self, key: &Path, hint: Option<&Path>, limit: u64) {
        if self.client.hello().git_sources_limit {
            let _ = self.git_sources_limited(key, hint, false, Some(limit));
        }
    }

    fn git_sources_limited(
        &self,
        key: &Path,
        hint: Option<&Path>,
        staged: bool,
        max_bytes: Option<u64>,
    ) -> io::Result<Option<std::sync::Arc<Sources>>> {
        let file = self.remote_path(key)?;
        let (epoch, cached, _flight) = loop {
            let epoch = self.refresh_git_source_cache(false);
            let cached = self
                .git_sources
                .lock()
                .unwrap()
                .get(&file)
                .filter(|(_, source)| source.staged == staged)
                .cloned();
            if let Some((checked, sources)) = &cached
                && *checked == epoch
            {
                return Ok(Some(sources.clone()));
            }
            let flight_key = (file.clone(), staged);
            let waiting = {
                let mut inflight = self.git_sources_inflight.lock().unwrap();
                match inflight.get(&flight_key) {
                    Some(flight) => Some(flight.clone()),
                    None => {
                        let flight = std::sync::Arc::new(SourceFlight::default());
                        inflight.insert(flight_key.clone(), flight.clone());
                        break (epoch, cached, FlightGuard { map: &self.git_sources_inflight, key: flight_key, flight });
                    }
                }
            };
            // Another caller is fetching this file; reuse its result.
            if let Some(flight) = waiting { flight.wait(); }
        };
        let previous = cached.map(|(_, sources)| sources);
        let known = previous.as_ref().and_then(|sources| sources.revisions);
        let repository = hint.map(|path| self.remote_path(path)).transpose()?;
        let Reply::GitSources(mut sources) = self.client.call(Command::GitSources {
            file: file.clone(),
            repository,
            staged,
            known,
            max_bytes,
        })?
        else {
            return Err(io::Error::other("Invalid Git source reply"));
        };
        if sources.oversized {
            return Ok(None);
        }
        if sources.unchanged {
            let previous = previous.ok_or_else(|| io::Error::other("Unexpected unchanged Git source reply"))?;
            sources.head = previous.head.clone();
            sources.index = previous.index.clone();
            sources.unchanged = false;
        }
        let bytes = sources.head.as_ref().map_or(0, |text| text.len())
            + sources.index.as_ref().map_or(0, |text| text.len());
        let sources = std::sync::Arc::new(sources);
        // A response that started before a refresh keeps its request epoch,
        // so the next use revalidates it instead of trusting superseded state.
        let _state = self.git_source_state.lock().unwrap();
        self.git_sources
            .lock()
            .unwrap()
            .insert(file, bytes, (epoch, sources.clone()));
        Ok(Some(sources))
    }
    #[cfg(test)]
    pub(crate) fn git_snapshot(
        &self,
        include_untracked: bool,
        known: Option<Discovery>,
        parent_open: bool,
        closed: Vec<RemotePath>,
        changed: Option<Vec<RemotePath>>,
        cancel: &crate::background::CancellationToken,
    ) -> io::Result<(
        crate::git_repositories::Discovery,
        Vec<crate::git_repositories::Scan>,
    )> {
        self.git_snapshot_progress(StatusQuery { include_untracked, known, parent_open, closed, changed, revalidate: false },
            None, cancel, |_| Ok(()))
    }

    pub(crate) fn git_snapshot_progress(
        &self, query: StatusQuery, progress: Option<StatusProgressOptions>,
        cancel: &crate::background::CancellationToken,
        mut consume: impl FnMut(StatusUpdate) -> io::Result<()>,
    ) -> io::Result<(crate::git_repositories::Discovery, Vec<crate::git_repositories::Scan>)> {
        let StatusQuery { include_untracked, known, parent_open, closed, changed, .. } = query;
        if cancel.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        // Status requests are serialized on a worker. A known worktree event
        // cannot change an unrelated repository, nor HEAD/index/blame sources.
        // Manual refresh, Git metadata, rescans and unwatched workspaces still
        // take the full path. Missing/evicted snapshots are always rescanned.
        let roots = known
            .as_ref()
            .map(|known| open_roots(known, parent_open, &closed));
        let partial = changed.as_ref().filter(|paths| {
            roots
                .as_ref()
                .is_some_and(|roots| roots.iter().all(|root| !root.1))
                && paths.iter().all(|path| !path.1)
        });
        let mut snapshots = self.git_snapshots.lock().unwrap();
        if partial.is_none() {
            *snapshots = Default::default();
        }
        let mut skipped = Vec::new();
        let mut selected = Vec::new();
        let mut refreshed = std::collections::HashMap::new();
        let mut previous = std::collections::HashMap::new();
        let mut delta_safe = partial.is_some();
        let mut revalidate = false;
        if let Some(roots) = &roots {
            for root in roots {
                let cached = snapshots.get(root).filter(|snapshot| {
                    snapshot.scan.include_untracked == include_untracked
                });
                let expired = cached.is_some_and(|snapshot| snapshot.checked.elapsed() >= std::time::Duration::from_secs(30));
                let unaffected = partial.is_some_and(|paths| {
                    paths.iter().all(|path| {
                        !repository_contains(root, path) && !repository_contains(path, root)
                    })
                });
                if let Some(snapshot) = cached.filter(|_| unaffected && !expired).map(|snapshot| snapshot.scan.clone())
                {
                    skipped.push(root.clone());
                    refreshed.insert(root.clone(), snapshot);
                } else {
                    // Keep event hints when only the deadline expired. The node
                    // can reject ignored/hidden output before a full traversal.
                    // Navigation and missing baselines still require full reads.
                    delta_safe &= cached.is_some() && !(unaffected && expired);
                    revalidate |= expired;
                    selected.push(root.clone());
                }
            }
        }
        // A failed/cancelled partial read must leave its repositories missing,
        // so the next refresh retries them rather than reusing stale snapshots.
        for root in &selected {
            if let Some(cached) = snapshots.take(root) { previous.insert(root.clone(), cached); }
        }
        let mut early_scans = Vec::new();
        let mut published = std::collections::HashSet::new();
        let (discovery, scans) = if partial.is_some() && selected.is_empty() {
            (known.unwrap(), Vec::new())
        } else {
            let mut excluded = closed.clone();
            excluded.extend(skipped);
            let Reply::GitStatus { discovery, scans } = self.client.git_status(
                Command::GitStatus {
                    known,
                    parent_open,
                    closed: excluded,
                    include_untracked,
                    changed: if delta_safe { partial.cloned() } else { None },
                    revalidate: delta_safe && revalidate,
                    progress,
                },
                cancel,
                |progress| {
                    match progress {
                        StatusProgress::Repository { root, branch } => consume(StatusUpdate::Repository { root: self.view_key(&root), branch }),
                        StatusProgress::Snapshot { discovery, scan } => {
                            let discovery = self.decode_status_discovery(discovery)?;
                            let scan = self.decode_status_scan(*scan, include_untracked)?;
                            published.insert(scan.root.clone());
                            early_scans.push((scan.clone(), std::time::Instant::now()));
                            consume(StatusUpdate::Snapshot { discovery, scan: Box::new(scan) })
                        }
                    }
                },
            )?
            else {
                return Err(io::Error::other("Invalid remote Git status reply"));
            };
            (discovery, scans)
        };
        if cancel.is_cancelled() { return Err(io::ErrorKind::Interrupted.into()); }
        let refresh_sources = partial.is_none()
            || changed
                .as_ref()
                .is_some_and(|paths| paths.iter().any(|path| path.name() == ".gitignore"));
        self.refresh_git_source_cache(refresh_sources);
        let active = open_roots(&discovery, parent_open, &closed);
        let result_discovery = self.decode_status_discovery(discovery)?;
        let scans = scans.into_iter().map(|scan| self.decode_status_scan(scan, include_untracked));
        let scans = scans.map(|scan| scan.map(|scan| (scan, std::time::Instant::now())));
        for scan in early_scans.into_iter().map(Ok).chain(scans) {
            let (mut merged, received) = scan?;
            let remote_root = self.remote_path(&merged.root)?;
            let checked = if let Some(paths) = merged.worktree_paths.take() {
                let mut cached = previous.remove(&remote_root)
                    .ok_or_else(|| io::Error::other("Git status delta has no cached baseline"))?;
                crate::app_git::refresh::merge_worktree(&mut cached.scan.files, merged.files, &paths);
                merged.files = cached.scan.files;
                // A path delta does not revalidate unrelated files. Retain the
                // full-snapshot deadline so missed shared-disk events expire.
                cached.checked
            } else { received };
            let bytes = merged
                .files
                .iter()
                .map(|file| {
                    std::mem::size_of_val(file)
                        + file.path.len()
                        + file.original_path.as_ref().map_or(0, |path| path.len())
                })
                .sum();
            let already_published = published.contains(&merged.root);
            if !already_published { refreshed.insert(remote_root.clone(), merged.clone()); }
            snapshots.insert(
                remote_root.clone(),
                bytes,
                CachedStatus {
                    scan: merged,
                    checked,
                },
            );
        }
        let scans = active
            .into_iter()
            .filter(|root| !published.contains(&self.view_key(root)))
            .filter_map(|root| {
                refreshed
                    .remove(&root)
                    .or_else(|| snapshots.get(&root)
                        .filter(|snapshot| snapshot.scan.include_untracked == include_untracked)
                        .map(|snapshot| snapshot.scan.clone()))
            })
            .collect();
        Ok((result_discovery, scans))
    }

    fn decode_status_discovery(&self, discovery: Discovery) -> io::Result<crate::git_repositories::Discovery> {
        Ok(crate::git_repositories::Discovery {
            errors: discovery.errors.into_iter().map(|error| error.try_map(|path| Ok::<_, io::Error>(self.view_key(&path))))
                .collect::<io::Result<_>>()?,
            roots: discovery.roots.iter().map(|path| self.view_key(path)).collect(),
            parent: discovery.parent.as_ref().map(|path| self.view_key(path)),
            workspace: self.view_root.clone(),
        })
    }

    fn decode_status_scan(&self, scan: Scan, include_untracked: bool) -> io::Result<crate::git_repositories::Scan> {
        let key = self.view_key(&scan.root);
        let local_path = |path: RemotePath| -> io::Result<PathBuf> {
            if path.is_absolute() { return Err(io::Error::other("Git status path must be repository-relative")); }
            let remote = scan.root.join_relative(&path.0)?;
            Ok(self.view_key(&remote).strip_prefix(&key).map_err(io::Error::other)?.to_owned())
        };
        let mut files = scan.files.into_iter().map(|file| {
            file.try_map_paths(|path| local_path(path).map(|path| path.to_string_lossy().into_owned()))
        }).collect::<io::Result<Vec<_>>>()?;
        files.sort_unstable_by(|a, b| crate::source_control_rows::Group::of(a)
            .cmp(&crate::source_control_rows::Group::of(b)).then(a.path.cmp(&b.path)));
        let worktree_paths = scan.worktree_paths.map(|scope| {
            let mut paths = crate::app_git::refresh::WorktreePaths::new(
                scope.paths.into_iter().map(local_path).collect::<io::Result<_>>()?);
            paths.set_ignore_case(scope.ignore_case);
            Ok::<_, io::Error>(paths)
        }).transpose()?;
        Ok(crate::git_repositories::Scan { root: key, files, operation: scan.operation, branch: scan.branch,
            worktree_paths, include_untracked })
    }
}
