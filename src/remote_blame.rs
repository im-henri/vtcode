//! Transfer committed attribution once, then compare local edits on a worker.
//! A commit dictionary avoids repeating metadata/messages for every source line.
use super::*;
use crate::{background::BlameCacheKey, git_blame as native};
use std::sync::Arc;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Revision {
    workdir: RemotePath,
    path: RemotePath,
    working_file: RemotePath,
    hash: String,
    date: String,
    summary: String,
    syntax_path: RemotePath,
}
impl Revision {
    pub(crate) fn from_native(root: &Path, revision: &native::Revision) -> io::Result<Self> {
        Ok(Self {
            workdir: git::from_native(&revision.workdir, root)?,
            path: git::from_native(&revision.path, Path::new(""))?,
            working_file: git::from_native(&revision.working_file, root)?,
            syntax_path: git::from_native(&revision.syntax_path, root)?,
            hash: revision.hash.clone(),
            date: revision.date.clone(),
            summary: revision.summary.clone(),
        })
    }
    pub(super) fn resolve(self, root: &Path) -> io::Result<Arc<native::Revision>> {
        Ok(Arc::new(native::Revision {
            workdir: self.workdir.resolve(root)?,
            path: self.path.resolve(Path::new(""))?,
            working_file: self.working_file.resolve(root)?,
            syntax_path: self.syntax_path.resolve(root)?,
            hash: self.hash,
            date: self.date,
            summary: self.summary,
        }))
    }
    pub(crate) fn for_client(
        self,
        workspace: &workspace::Workspace,
    ) -> io::Result<Arc<native::Revision>> {
        Ok(Arc::new(native::Revision {
            workdir: workspace.view_key(&self.workdir),
            path: relative_key(workspace, &self.path)?,
            working_file: workspace.view_key(&self.working_file),
            syntax_path: workspace.view_key(&self.syntax_path),
            hash: self.hash,
            date: self.date,
            summary: self.summary,
        }))
    }
}
fn relative_key(workspace: &workspace::Workspace, path: &RemotePath) -> io::Result<PathBuf> {
    if path.1 {
        return Err(io::Error::other("Git tree path is not relative"));
    }
    workspace
        .view_key(path)
        .strip_prefix(&workspace.view_root)
        .map(Path::to_path_buf)
        .map_err(io::Error::other)
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Commit {
    hash: String,
    author: String,
    email: String,
    age: String,
    date: String,
    committed_at: String,
    summary: String,
    path: RemotePath,
    message: String,
}
impl Commit {
    fn resolve(self) -> io::Result<native::Commit> {
        Ok(native::Commit {
            hash: self.hash,
            author: self.author,
            email: self.email,
            age: self.age,
            date: self.date,
            committed_at: self.committed_at,
            summary: self.summary,
            path: self.path.resolve(Path::new(""))?,
            message: self.message,
        })
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct History {
    file: RemotePath,
    hash: String,
    summary: String,
    before_label: String,
    after_label: String,
    before: String,
    after: String,
    line: usize,
    revision: Revision,
    previous_revision: Option<Revision>,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    revision: Revision,
    previous_revision: Option<Revision>,
    previous_notice: Option<String>,
    source: Sources,
    line: usize,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Sources {
    text: String,
    head: String,
    revision: Option<Revision>,
    commits: Vec<Commit>,
    /// (commit dictionary index, original Git line), in committed-buffer order.
    lines: Vec<(Option<usize>, Option<usize>)>,
    notice: Option<String>,
}
pub(super) struct Base {
    pub document: native::Document,
}

pub(crate) fn sources(
    root: &Path,
    file: RemotePath,
    revision: Option<Revision>,
) -> io::Result<Sources> {
    let revision = revision
        .map(|revision| revision.resolve(root))
        .transpose()?;
    let document = native::prepare_head(
        file.resolve(root)?,
        revision,
        (MAX_PAYLOAD_BYTES / 4) as u64,
    )
    .map_err(io::Error::other)?;
    encode(root, document)
}
pub(crate) fn history(
    root: &Path,
    file: RemotePath,
    commit: Commit,
    source: Option<Revision>,
    line: usize,
    limit: u64,
) -> io::Result<History> {
    let source = source.map(|source| source.resolve(root)).transpose()?;
    let contents = native::read_history_at(
        file.resolve(root)?,
        &commit.resolve()?,
        line,
        limit.min((MAX_PAYLOAD_BYTES / 4) as u64),
        source.as_deref(),
    )
    .map_err(io::Error::other)?;
    Ok(History {
        file,
        hash: contents.hash,
        summary: contents.summary,
        before_label: contents.before_label,
        after_label: contents.after_label,
        before: contents.before,
        after: contents.after,
        line: contents.line,
        revision: Revision::from_native(root, &contents.revision)?,
        previous_revision: contents
            .previous_revision
            .as_deref()
            .map(|revision| Revision::from_native(root, revision))
            .transpose()?,
    })
}
pub(crate) fn snapshot(
    root: &Path,
    revision: Revision,
    line: usize,
    limit: u64,
) -> io::Result<Snapshot> {
    let snapshot = native::prepare_snapshot(
        revision.resolve(root)?,
        line,
        limit.min((MAX_PAYLOAD_BYTES / 4) as u64),
    )
    .map_err(io::Error::other)?;
    Ok(Snapshot {
        revision: Revision::from_native(root, &snapshot.revision)?,
        previous_revision: snapshot
            .previous_revision
            .as_deref()
            .map(|revision| Revision::from_native(root, revision))
            .transpose()?,
        previous_notice: snapshot.previous_notice,
        source: encode(
            root,
            Arc::try_unwrap(snapshot.document)
                .map_err(|_| io::Error::other("Shared headless blame snapshot"))?,
        )?,
        line: snapshot.line,
    })
}
pub(crate) fn line_snapshot(
    root: &Path,
    file: RemotePath,
    commit: Commit,
    source: Option<Revision>,
    line: usize,
    limit: u64,
) -> io::Result<Snapshot> {
    let path = file.resolve(root)?;
    let source = source.map(|source| source.resolve(root)).transpose()?;
    let repo = git2::Repository::discover(source.as_ref().map_or_else(
        || path.parent().unwrap_or(&path),
        |revision| revision.workdir.as_path(),
    ))
    .map_err(io::Error::other)?;
    let commit = commit.resolve()?;
    let info = repo
        .find_commit(git2::Oid::from_str(&commit.hash).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;
    let working_file = source
        .as_ref()
        .map_or(path.clone(), |revision| revision.working_file.clone());
    let revision =
        native::revision_for(&repo, &info, commit.path, working_file).map_err(io::Error::other)?;
    snapshot(root, Revision::from_native(root, &revision)?, line, limit)
}
fn encode(root: &Path, document: native::Document) -> io::Result<Sources> {
    let mut commits = Vec::new();
    let mut indices = std::collections::HashMap::new();
    let mut lines = Vec::new();
    for (line, original) in document.lines.iter().zip(&document.commit_lines) {
        let index = if let native::Line::Committed(commit) = line {
            if let Some(index) = indices.get(&commit.hash) {
                Some(*index)
            } else {
                let index = commits.len();
                indices.insert(commit.hash.clone(), index);
                commits.push(Commit {
                    hash: commit.hash.clone(),
                    author: commit.author.clone(),
                    email: commit.email.clone(),
                    age: commit.age.clone(),
                    date: commit.date.clone(),
                    committed_at: commit.committed_at.clone(),
                    summary: commit.summary.clone(),
                    path: git::from_native(&commit.path, Path::new(""))?,
                    message: commit.message.clone(),
                });
                Some(index)
            }
        } else {
            None
        };
        lines.push((index, *original));
    }
    Ok(Sources {
        text: document.rope.to_string(),
        head: document.key.map_or_else(String::new, |key| key.head_id),
        revision: document
            .revision
            .as_deref()
            .map(|revision| Revision::from_native(root, revision))
            .transpose()?,
        commits,
        lines,
        notice: document.notice,
    })
}
impl Sources {
    fn decode(self, workspace: &workspace::Workspace, path: PathBuf) -> io::Result<Base> {
        let rope = ropey::Rope::from_str(&self.text);
        if self.lines.len() > rope.len_lines() {
            return Err(io::Error::other("Invalid remote blame line count"));
        }
        let commits = self
            .commits
            .into_iter()
            .map(|commit| {
                Ok(Arc::new(native::Commit {
                    hash: commit.hash,
                    author: commit.author,
                    email: commit.email,
                    age: commit.age,
                    date: commit.date,
                    committed_at: commit.committed_at,
                    summary: commit.summary,
                    path: relative_key(workspace, &commit.path)?,
                    message: commit.message,
                }))
            })
            .collect::<io::Result<Vec<_>>>()?;
        let mut lines = Vec::new();
        let mut commit_lines = Vec::new();
        for (index, original) in self.lines {
            lines.push(match index {
                Some(index) => native::Line::Committed(
                    commits
                        .get(index)
                        .ok_or_else(|| io::Error::other("Invalid remote blame commit"))?
                        .clone(),
                ),
                None => native::Line::Empty,
            });
            commit_lines.push(original);
        }
        Ok(Base {
            document: native::Document {
                path: path.clone(),
                revision: self
                    .revision
                    .map(|revision| revision.for_client(workspace))
                    .transpose()?,
                key: Some(BlameCacheKey {
                    file_path: path,
                    head_id: self.head,
                    disk_modified: None,
                    disk_len: self.text.len() as u64,
                    line_count: rope.len_lines(),
                }),
                rope,
                lines,
                commit_lines,
                notice: self.notice,
            },
        })
    }
}

impl workspace::Workspace {
    fn blame_revision(&self, revision: &native::Revision) -> io::Result<Revision> {
        let path = self.remote_path(&self.view_root.join(&revision.path))?;
        Ok(Revision {
            workdir: self.remote_path(&revision.workdir)?,
            path,
            working_file: self.remote_path(&revision.working_file)?,
            syntax_path: self.remote_path(&revision.syntax_path)?,
            hash: revision.hash.clone(),
            date: revision.date.clone(),
            summary: revision.summary.clone(),
        })
    }
    fn blame_commit(&self, commit: &native::Commit) -> io::Result<Commit> {
        Ok(Commit {
            hash: commit.hash.clone(),
            author: commit.author.clone(),
            email: commit.email.clone(),
            age: commit.age.clone(),
            date: commit.date.clone(),
            committed_at: commit.committed_at.clone(),
            summary: commit.summary.clone(),
            message: commit.message.clone(),
            path: self.remote_path(&self.view_root.join(&commit.path))?,
        })
    }
    pub(crate) fn blame_history(
        &self,
        path: &Path,
        commit: &native::Commit,
        line: usize,
        limit: u64,
        source: Option<&native::Revision>,
    ) -> io::Result<native::History> {
        let command = Command::BlameHistory {
            file: self.remote_path(path)?,
            commit: self.blame_commit(commit)?,
            line,
            limit,
            source: source
                .map(|revision| self.blame_revision(revision))
                .transpose()?,
        };
        let Reply::BlameHistory(history) = self.client.call(command)? else {
            return Err(io::Error::other("Invalid blame history reply"));
        };
        Ok(native::prepare_history_contents(native::HistoryContents {
            file_path: self.view_key(&history.file),
            hash: history.hash,
            summary: history.summary,
            before_label: history.before_label,
            after_label: history.after_label,
            before: history.before,
            after: history.after,
            line: history.line,
            revision: history.revision.for_client(self)?,
            previous_revision: history
                .previous_revision
                .map(|revision| revision.for_client(self))
                .transpose()?,
        }))
    }
    fn decode_snapshot(&self, snapshot: Snapshot) -> io::Result<native::Snapshot> {
        let revision = snapshot.revision.for_client(self)?;
        let base = snapshot.source.decode(self, revision.file_path())?;
        Ok(native::Snapshot {
            revision,
            previous_revision: snapshot
                .previous_revision
                .map(|revision| revision.for_client(self))
                .transpose()?,
            previous_notice: snapshot.previous_notice,
            document: Arc::new(base.document),
            line: snapshot.line,
        })
    }
    pub(crate) fn blame_snapshot(
        &self,
        revision: &native::Revision,
        line: usize,
        limit: u64,
    ) -> io::Result<native::Snapshot> {
        let Reply::BlameSnapshot(snapshot) = self.client.call(Command::BlameSnapshot {
            revision: self.blame_revision(revision)?,
            line,
            limit,
        })?
        else {
            return Err(io::Error::other("Invalid blame snapshot reply"));
        };
        self.decode_snapshot(snapshot)
    }
    pub(crate) fn blame_line_snapshot(
        &self,
        path: &Path,
        commit: &native::Commit,
        source: Option<&native::Revision>,
        line: usize,
        limit: u64,
    ) -> io::Result<native::Snapshot> {
        let Reply::BlameSnapshot(snapshot) = self.client.call(Command::BlameLineSnapshot {
            file: self.remote_path(path)?,
            commit: self.blame_commit(commit)?,
            source: source
                .map(|revision| self.blame_revision(revision))
                .transpose()?,
            line,
            limit,
        })?
        else {
            return Err(io::Error::other("Invalid blame file reply"));
        };
        self.decode_snapshot(snapshot)
    }
    pub(crate) fn prepare_blame(
        &self,
        path: PathBuf,
        rope: ropey::Rope,
        revision: Option<Arc<native::Revision>>,
        cancel: &crate::background::CancellationToken,
    ) -> native::Document {
        let result = (|| -> io::Result<_> {
            let epoch = self.refresh_git_source_cache(false);
            let file = self.remote_path(&path)?;
            let cached = self
                .blame_sources
                .lock()
                .unwrap()
                .get(&file)
                .filter(|base| base.document.revision == revision)
                .cloned();
            let base = if let Some(base) = cached {
                base
            } else {
                let source_revision = revision
                    .as_deref()
                    .map(|revision| self.blame_revision(revision))
                    .transpose()?;
                let Reply::Blame(source) = self.client.call_cancellable(
                    Command::Blame {
                        file: file.clone(),
                        revision: source_revision,
                    },
                    cancel,
                )?
                else {
                    return Err(io::Error::other("Invalid blame reply"));
                };
                let base = Arc::new(source.decode(self, path.clone())?);
                let bytes = base.document.rope.len_bytes()
                    + base.document.lines.len()
                        * (std::mem::size_of::<native::Line>()
                            + std::mem::size_of::<Option<usize>>());
                let state = self.git_source_state.lock().unwrap();
                if state.epoch == epoch {
                    self.blame_sources
                        .lock()
                        .unwrap()
                        .insert(file, bytes, base.clone());
                }
                base
            };
            remap(&base.document, &path, &rope, revision.clone(), cancel)
        })();
        result.unwrap_or_else(|error| native::Document {
            path,
            revision,
            rope,
            key: None,
            lines: Vec::new(),
            commit_lines: Vec::new(),
            notice: Some(error.to_string()),
        })
    }
}

fn remap(
    base: &native::Document,
    path: &Path,
    rope: &ropey::Rope,
    revision: Option<Arc<native::Revision>>,
    cancel: &crate::background::CancellationToken,
) -> io::Result<native::Document> {
    let original = base.rope.to_string();
    let current = rope.to_string();
    let old = original.replace("\r\n", "\n");
    let new = current.replace("\r\n", "\n");
    let diff = similar::TextDiff::configure()
        .timeout(std::time::Duration::from_millis(250))
        .diff_lines(&old, &new);
    let mut mapping = vec![None; new.split_inclusive('\n').count()];
    for op in diff
        .ops()
        .iter()
        .filter(|op| op.tag() == similar::DiffTag::Equal)
    {
        for (old, new) in op.old_range().zip(op.new_range()) {
            mapping[new] = Some(old);
        }
    }
    let source_rows: Vec<_> = std::iter::once(0)
        .chain(original.match_indices('\n').map(|(byte, _)| byte + 1))
        .map(|byte| base.rope.byte_to_line(byte))
        .collect();
    let mut lines = Vec::with_capacity(rope.len_lines());
    let mut commit_lines = Vec::with_capacity(rope.len_lines());
    let mut git_line = 0;
    for row in rope.lines() {
        if cancel.is_cancelled() {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        let source = mapping
            .get(git_line)
            .copied()
            .flatten()
            .and_then(|line| source_rows.get(line))
            .copied();
        let line = if row.len_chars() == 0 {
            native::Line::Empty
        } else {
            source
                .and_then(|line| base.lines.get(line))
                .cloned()
                .unwrap_or(native::Line::Uncommitted)
        };
        let original = source.and_then(|line| base.commit_lines.get(line).copied().flatten());
        lines.push(line);
        commit_lines.push(original);
        git_line += row.bytes().filter(|&byte| byte == b'\n').count();
    }
    Ok(native::Document {
        path: path.to_owned(),
        revision,
        rope: rope.clone(),
        key: base.key.clone(),
        lines,
        commit_lines,
        notice: base.notice.clone(),
    })
}
