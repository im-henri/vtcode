//! Repository discovery and native libgit2 status collection live on the remote
//! filesystem. The client receives one snapshot, not a network stat per file.
use super::*;

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
    let head =
        crate::app_git::head_file_content(&repo, &relative, staged).map_err(io::Error::other)?;
    let (index, conflicted) = crate::app_git::line_diff_index_content(&repo, &relative).map_err(io::Error::other)?;
    Ok(Reply::GitSources(Sources {
        repository: from_native(&workdir, root)?,
        head,
        index,
        conflicted,
        ignored,
        staged,
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
    changed: Option<Vec<RemotePath>>,
    cancel: &crate::background::CancellationToken,
) -> io::Result<Reply> {
    if cancel.is_cancelled() {
        return Err(io::Error::from(io::ErrorKind::Interrupted));
    }
    let scoped = known.is_some();
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
        None => crate::git_repositories::discover(root, cancel),
    };
    let mut roots = discovery.roots.clone();
    if parent_open && let Some(parent) = &discovery.parent { roots.push(parent.clone()); }
    let closed = closed.iter().map(|path| path.resolve(root)).collect::<io::Result<Vec<_>>>()?;
    roots.retain(|path| !closed.contains(path));
    let changed = if scoped {
        changed.map(|paths| paths.iter().map(|path| path.resolve(root)).collect::<io::Result<_>>()).transpose()?
    } else { None };
    let scans = crate::app_git::refresh::collect_with_paths(&mut discovery, roots, changed, root,
        cancel, include_untracked, Path::to_path_buf);
    let errors = discovery.errors.into_iter().map(|error| {
        error.try_map(|path| from_native(&path, root))
    }).collect::<io::Result<_>>()?;
    let discovery = Discovery {
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
    };
    let scans = scans.into_iter().map(|scan| {
        let worktree_paths = scan.worktree_paths.map(|scope| {
            Ok::<_, io::Error>(WorktreeScope {
                ignore_case: scope.ignore_case(),
                paths: scope.paths.into_iter().map(relative_native_path).collect::<io::Result<_>>()?,
            })
        }).transpose()?;
        Ok(Scan {
            root: from_native(&scan.root, root)?,
            files: scan.files.into_iter().map(|file| file.try_map_paths(relative_native_path)).collect::<io::Result<_>>()?,
            branch: scan.branch,
            worktree_paths,
        })
    }).collect::<io::Result<_>>()?;
    if cancel.is_cancelled() {
        return Err(io::Error::from(io::ErrorKind::Interrupted));
    }
    Ok(Reply::GitStatus { discovery, scans })
}

impl workspace::Workspace {
    pub(crate) fn git_diff_sources(&self, key: &Path, hint: Option<&Path>, staged: bool,
        known: Option<crate::git_diff_sources::Revisions>, cancel: &crate::background::CancellationToken,
    ) -> io::Result<crate::git_diff_sources::Sources> {
        let file = self.remote_path(key)?;
        let repository = hint.map(|path| self.remote_path(path)).transpose()?;
        let Reply::GitDiffSources(sources) = self.client.call_cancellable(
            Command::GitDiffSources { file, repository, staged, known }, cancel)?
        else { return Err(io::Error::other("Invalid conditional Git source reply")); };
        Ok(sources)
    }

    pub(super) fn refresh_git_source_cache(&self, force: bool) -> u64 {
        let mut state = self.git_source_state.lock().unwrap();
        if force || state.checked.elapsed() >= std::time::Duration::from_secs(30) {
            *self.git_sources.lock().unwrap() = Default::default();
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
        let epoch = self.refresh_git_source_cache(false);
        let file = self.remote_path(key)?;
        if let Some(cached) = self
            .git_sources
            .lock()
            .unwrap()
            .get(&file)
            .filter(|source| source.staged == staged)
            .cloned()
        {
            return Ok(cached);
        }
        let repository = hint.map(|path| self.remote_path(path)).transpose()?;
        let Reply::GitSources(sources) = self.client.call(Command::GitSources {
            file: file.clone(),
            repository,
            staged,
        })?
        else {
            return Err(io::Error::other("Invalid Git source reply"));
        };
        let bytes = sources.head.as_ref().map_or(0, |text| text.len())
            + sources.index.as_ref().map_or(0, |text| text.len());
        let sources = std::sync::Arc::new(sources);
        // Invalidations and cache publication share this short worker lock.
        // A response started before a refresh may complete afterward, but it
        // must not repopulate the cache with its superseded HEAD/index state.
        let state = self.git_source_state.lock().unwrap();
        if state.epoch == epoch {
            self.git_sources
                .lock()
                .unwrap()
                .insert(file, bytes, sources.clone());
        }
        Ok(sources)
    }
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
        if let Some(roots) = &roots {
            for root in roots {
                let cached = snapshots.get(root).filter(|snapshot| {
                    snapshot.scan.include_untracked == include_untracked
                        && snapshot.checked.elapsed() < std::time::Duration::from_secs(30)
                });
                let unaffected = partial.is_some_and(|paths| {
                    paths.iter().all(|path| {
                        !repository_contains(root, path) && !repository_contains(path, root)
                    })
                });
                if let Some(snapshot) = cached.filter(|_| unaffected).map(|snapshot| snapshot.scan.clone())
                {
                    skipped.push(root.clone());
                    refreshed.insert(root.clone(), snapshot);
                } else {
                    delta_safe &= cached.is_some();
                    selected.push(root.clone());
                }
            }
        }
        // A failed/cancelled partial read must leave its repositories missing,
        // so the next refresh retries them rather than reusing stale snapshots.
        for root in &selected {
            if let Some(cached) = snapshots.take(root) { previous.insert(root.clone(), cached); }
        }
        let (discovery, scans) = if partial.is_some() && selected.is_empty() {
            (known.unwrap(), Vec::new())
        } else {
            let mut excluded = closed.clone();
            excluded.extend(skipped);
            let Reply::GitStatus { discovery, scans } = self.client.call_cancellable(
                Command::GitStatus {
                    known,
                    parent_open,
                    closed: excluded,
                    include_untracked,
                    changed: if delta_safe { partial.cloned() } else { None },
                },
                cancel,
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
        let errors = discovery.errors.iter().cloned().map(|error| {
            error.try_map(|path| Ok::<_, io::Error>(self.view_key(&path)))
        }).collect::<io::Result<_>>()?;
        let result_discovery = crate::git_repositories::Discovery {
            errors,
            roots: discovery
                .roots
                .iter()
                .map(|path| self.view_key(path))
                .collect(),
            parent: discovery.parent.as_ref().map(|path| self.view_key(path)),
            workspace: self.view_root.clone(),
        };
        for scan in scans {
            let remote_root = scan.root.clone();
            let key = self.view_key(&scan.root);
            let local_path = |path: RemotePath| -> io::Result<PathBuf> {
                if path.is_absolute() { return Err(io::Error::other("Git status path must be repository-relative")); }
                let remote = remote_root.join_relative(&path.0)?;
                Ok(self.view_key(&remote).strip_prefix(&key).map_err(io::Error::other)?.to_owned())
            };
            let mut files = scan.files.into_iter().map(|file| {
                file.try_map_paths(|path| local_path(path).map(|path| path.to_string_lossy().into_owned()))
            }).collect::<io::Result<Vec<_>>>()?;
            // The node's raw path ordering may change when view keys escape
            // names that the client OS cannot represent directly.
            files.sort_unstable_by(|a, b| crate::source_control_rows::Group::of(a)
                .cmp(&crate::source_control_rows::Group::of(b)).then(a.path.cmp(&b.path)));
            let mut merged = crate::git_repositories::Scan {
                root: key.clone(),
                files,
                branch: scan.branch,
                worktree_paths: None,
                include_untracked,
            };
            let checked = if let Some(scope) = scan.worktree_paths {
                let mut paths = crate::app_git::refresh::WorktreePaths::new(
                    scope.paths.into_iter().map(local_path).collect::<io::Result<_>>()?);
                paths.set_ignore_case(scope.ignore_case);
                let mut cached = previous.remove(&remote_root)
                    .ok_or_else(|| io::Error::other("Git status delta has no cached baseline"))?;
                crate::app_git::refresh::merge_worktree(&mut cached.scan.files, merged.files, &paths);
                merged.files = cached.scan.files;
                // A path delta does not revalidate unrelated files. Retain the
                // full-snapshot deadline so missed shared-disk events expire.
                cached.checked
            } else { std::time::Instant::now() };
            let bytes = merged
                .files
                .iter()
                .map(|file| {
                    std::mem::size_of_val(file)
                        + file.path.len()
                        + file.original_path.as_ref().map_or(0, |path| path.len())
                })
                .sum();
            snapshots.insert(
                remote_root.clone(),
                bytes,
                CachedStatus {
                    scan: merged.clone(),
                    checked,
                },
            );
            refreshed.insert(remote_root, merged);
        }
        let scans = active
            .into_iter()
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
}
