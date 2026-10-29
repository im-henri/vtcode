//! Headless filesystem service. It deliberately initializes neither App nor a
//! renderer, clipboard, theme, watcher tree, language assets, or input pump.
use super::*;
use std::{
    collections::HashMap,
    fs,
    io::{BufReader, Read, Write},
};

#[cfg(test)]
#[path = "remote_performance_tests.rs"]
mod performance_tests;

#[derive(Clone)]
pub(super) struct Document {
    pub(super) rope: ropey::Rope,
    pub(super) encoding: &'static str,
    pub(super) revision: Revision,
}

/// Read and mutation workers share immutable saved checkpoints. The root is
/// part of the cache identity; a delayed read from another workspace can only
/// evict an optimization, never become a baseline for the wrong file. No disk
/// I/O or decoding runs while this lock is held.
#[derive(Default)]
struct Documents {
    root: Option<PathBuf>,
    entries: cache::Cache<Document>,
}
impl Documents {
    fn get(&mut self, root: &Path, path: &RemotePath) -> Option<Document> {
        (self.root.as_deref() == Some(root))
            .then(|| self.entries.get(path).cloned())
            .flatten()
    }
    fn insert(&mut self, root: &Path, path: RemotePath, document: Document) {
        if self.root.as_deref() != Some(root) {
            self.entries = Default::default();
            self.root = Some(root.into());
        }
        self.entries
            .insert(path, document.rope.len_bytes(), document);
    }
    fn remove(&mut self, root: &Path, path: &RemotePath, subtree: bool) {
        if self.root.as_deref() == Some(root) {
            if subtree {
                self.entries.remove_under(path);
            } else {
                self.entries.remove(path);
            }
        }
    }
}

pub(crate) struct Service {
    root: PathBuf,
    documents: std::sync::Arc<std::sync::Mutex<Documents>>,
    processes: Option<process::Manager>,
    process_ownership: Option<process::Ownership>,
    index: Option<FileIndex>,
    watcher: Option<watch::Watch>,
    output: Option<std::sync::mpsc::SyncSender<Response>>,
    histories: Option<history::Manager>,
    explorer: Option<(u64, crate::explorer::FileExplorer)>,
    binary: binary::Manager,
    text: std::sync::Arc<std::sync::Mutex<text::Store>>,
    rollback: rollback::Manager,
    agents: Option<std::sync::Arc<agent::Manager>>,
    observations: watch::Registry,
    git_observation: Option<std::sync::Arc<watch::Observation>>,
}
struct FileIndex {
    generation: u64,
    built: std::time::Instant,
    version: u64,
    files: Vec<RemotePath>,
    labels: Vec<String>,
    include_hidden: bool,
    excludes: Option<crate::settings::ExcludePatterns>,
    watch_stamp: watch::Stamp,
    _observation: std::sync::Arc<watch::Observation>,
}
pub(super) fn read_document(
    native: &Path,
) -> io::Result<(Document, u64, Option<std::time::SystemTime>)> {
    let loaded = crate::editor::load_text_file(native)?;
    let encoding = loaded.encoding;
    let rope = loaded.rope;
    let revision = revision(&rope, encoding);
    Ok((
        Document {
            rope,
            encoding,
            revision,
        },
        loaded.file_size,
        loaded.last_disk_modified,
    ))
}
impl Service {
    pub(crate) fn new(root: &Path) -> io::Result<Self> {
        let root = fs::canonicalize(root)?;
        if !root.is_dir() {
            return Err(io::Error::other("Remote workspace must be a directory"));
        }
        Ok(Self::from_root(root))
    }
    fn from_root(root: PathBuf) -> Self {
        Self {
            root,
            documents: Default::default(),
            processes: None,
            process_ownership: None,
            index: None,
            watcher: None,
            output: None,
            histories: None,
            explorer: None,
            binary: Default::default(),
            text: Default::default(),
            rollback: Default::default(),
            agents: None,
            observations: Default::default(),
            git_observation: None,
        }
    }
    pub(crate) fn hello(&self, session: u64) -> Hello {
        Hello {
            version: VERSION,
            app_version: env!("CARGO_PKG_VERSION").into(),
            root: self.root.to_string_lossy().into_owned(),
            root_uri: url::Url::from_directory_path(&self.root)
                .expect("canonical absolute workspace")
                .into(),
            session,
        }
    }
    fn opened_reply(
        &mut self,
        path: RemotePath,
        identity: RemotePath,
        doc: Document,
        size: u64,
        modified: Option<std::time::SystemTime>,
    ) -> io::Result<Reply> {
        let reply = if doc.rope.len_bytes() > text::INLINE_BYTES {
            Reply::Text(text::Reply::Opened {
                transfer: self.text.lock().unwrap().read(doc.rope.clone())?,
                characters: doc.rope.len_chars(),
                bytes: doc.rope.len_bytes(),
                encoding: doc.encoding.into(),
                revision: doc.revision.clone(),
                identity,
                size,
                modified,
            })
        } else {
            Reply::Opened {
                text: doc.rope.to_string(),
                encoding: doc.encoding.into(),
                revision: doc.revision.clone(),
                identity,
                size,
                modified,
            }
        };
        self.documents.lock().unwrap().insert(&self.root, path, doc);
        Ok(reply)
    }
    fn open_location(
        &mut self,
        path: RemotePath,
        cancel: &crate::background::CancellationToken,
    ) -> io::Result<Reply> {
        self.open_source(path, cancel, true)
    }
    fn reload_source(&mut self, path: RemotePath, known: Option<Revision>, read_changed: bool,
        cancel: &crate::background::CancellationToken) -> io::Result<Reply> {
        if cancel.is_cancelled() { return Err(io::ErrorKind::Interrupted.into()); }
        let native = path.resolve(&self.root)?.canonicalize()?;
        let identity = git::from_native(&native, &self.root)?;
        let cached = self.documents.lock().unwrap().get(&self.root, &path);
        if let Some(doc) = cached && doc.encoding != "Binary" {
            let baseline = crate::editor::DiskSaveBaseline { rope: doc.rope.clone(), encoding: doc.encoding };
            match baseline.validate_cancellable(&native, cancel) {
                Ok(metadata) => {
                    if known.as_ref() == Some(&doc.revision) {
                        return Ok(Reply::Unchanged { identity, size: metadata.len(), modified: metadata.modified().ok() });
                    }
                    return if read_changed { self.opened_reply(path, identity, doc, metadata.len(), metadata.modified().ok()) }
                        else { Ok(Reply::Changed { size: metadata.len(), modified: metadata.modified().ok() }) };
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
                Err(_) if !read_changed && known.as_ref() == Some(&doc.revision) => {
                    let (_, metadata) = crate::fs_ops::open_document_source(&native, false)?;
                    return Ok(Reply::Changed { size: metadata.len(), modified: metadata.modified().ok() });
                }
                Err(_) => (), // Changed or unavailable: use the safe source reader below.
            }
        }
        let (file, metadata) = crate::fs_ops::open_document_source(&native, false)?;
        let loaded = crate::editor::decode_text_source(file, metadata, &native, Some(cancel))?;
        let doc = Document { revision: revision_cancellable(&loaded.rope, loaded.encoding, cancel)?,
            rope: loaded.rope, encoding: loaded.encoding };
        if known.as_ref() == Some(&doc.revision) {
            self.documents.lock().unwrap().insert(&self.root, path, doc);
            Ok(Reply::Unchanged { identity, size: loaded.file_size, modified: loaded.last_disk_modified })
        } else if read_changed {
            self.opened_reply(path, identity, doc, loaded.file_size, loaded.last_disk_modified)
        } else {
            self.documents.lock().unwrap().insert(&self.root, path, doc);
            Ok(Reply::Changed { size: loaded.file_size, modified: loaded.last_disk_modified })
        }
    }
    fn open_source(
        &mut self,
        path: RemotePath,
        cancel: &crate::background::CancellationToken,
        allow_directory: bool,
    ) -> io::Result<Reply> {
        let check = || {
            if cancel.is_cancelled() {
                Err(io::Error::from(io::ErrorKind::Interrupted))
            } else {
                Ok(())
            }
        };
        check()?;
        let native = path.resolve(&self.root)?.canonicalize()?;
        let identity = git::from_native(&native, &self.root)?;
        check()?;
        let (file, metadata) = crate::fs_ops::open_document_source(&native, allow_directory)?;
        check()?;
        if metadata.is_dir() {
            return Ok(Reply::Directory { identity });
        }
        let loaded = crate::editor::decode_text_source(file, metadata, &native, Some(cancel))?;
        let doc = Document {
            revision: revision_cancellable(&loaded.rope, loaded.encoding, cancel)?,
            rope: loaded.rope,
            encoding: loaded.encoding,
        };
        self.opened_reply(
            path,
            identity,
            doc,
            loaded.file_size,
            loaded.last_disk_modified,
        )
    }
    pub(crate) fn execute(&mut self, command: Command) -> io::Result<Reply> {
        match command {
            Command::Context(context::Command::Suggest {
                generation,
                query,
                preferred,
                explicit,
            }) => self.context_suggestions(
                generation,
                query,
                preferred,
                explicit,
                &crate::background::CancellationToken::new(),
            ),
            Command::Context(command) => context::execute(
                &self.root,
                command,
                &crate::background::CancellationToken::new(),
                &self.text,
            )
            .map(Reply::Context),
            Command::Canonicalize { paths } => {
                if paths.len() > 256 {
                    return Err(io::Error::other("Canonical path batch is too large"));
                }
                let workspace = vtcode::acp::Workspace::new(&self.root)
                    .map_err(|error| io::Error::other(error.message.to_string()))?;
                Ok(Reply::CanonicalPaths(
                    paths
                        .into_iter()
                        .map(|path| {
                            path.resolve(&self.root)
                                .ok()
                                .and_then(|path| workspace.resolve(&path).ok())
                                .and_then(|path| git::from_native(&path, &self.root).ok())
                        })
                        .collect(),
                ))
            }
            Command::Agent(command) => self
                .agents
                .as_ref()
                .ok_or_else(|| io::Error::other("Agent service is unavailable"))?
                .execute(&self.root, command),
            Command::Rollback(command) => self.rollback.execute(&self.root, command, &self.text),
            Command::Binary(command) => self.binary.execute(&self.root, command).map(Reply::Binary),
            Command::OpenWorkspace { path } => {
                let directory = Service::new(&path.resolve(&self.root)?)?;
                let hello = directory.hello(0);
                Ok(Reply::Workspace {
                    root: hello.root,
                    root_uri: hello.root_uri,
                })
            }
            Command::HistoryStart {
                history,
                repository,
            } => {
                let root = repository.resolve(&self.root)?;
                self.histories
                    .as_mut()
                    .ok_or_else(|| io::Error::other("History transport unavailable"))?
                    .start(history, root)?;
                Ok(Reply::Done)
            }
            Command::HistoryNext { history } => {
                self.histories
                    .as_ref()
                    .ok_or_else(|| io::Error::other("History transport unavailable"))?
                    .next(history)?;
                Ok(Reply::Done)
            }
            Command::HistoryClose { history } => {
                if let Some(histories) = &mut self.histories {
                    histories.close(history);
                }
                Ok(Reply::Done)
            }
            Command::Watch => {
                if self.watcher.is_none() {
                    self.watcher = Some(watch::Watch::start(
                        self.root.clone(),
                        self.output
                            .clone()
                            .ok_or_else(|| io::Error::other("Watch transport unavailable"))?,
                        self.observations.get(&self.root),
                    )?);
                }
                Ok(Reply::Done)
            }
            Command::Inspect { paths } => watch::inspect(
                &self.root,
                paths,
                &crate::background::CancellationToken::new(),
            ),
            Command::Filesystem(task) => super::fs::execute(
                &self.root,
                task,
                &crate::background::CancellationToken::new(),
                |_, _| {},
            )
            .map(Reply::Filesystem),
            Command::Cancel { .. } => Ok(Reply::Done),
            Command::Explorer(query) => explorer::read(
                &self.root,
                &mut self.explorer,
                query,
                &crate::background::CancellationToken::new(),
            )
            .map(Reply::Explorer),
            Command::Tasks => Ok(Reply::Tasks(crate::app_terminal::discover_tasks(
                &self.root,
            ))),
            Command::Format {
                path,
                config,
                text,
                lines,
            } => format(
                &self.root,
                path,
                config,
                ropey::Rope::from_str(&text),
                lines,
                &crate::background::CancellationToken::new(),
                &self.text,
                self.process_ownership.as_ref(),
            ),
            Command::FormatSource {
                transfer,
                path,
                config,
                lines,
            } => {
                let source = self.text.lock().unwrap().snapshot(transfer)?;
                format(
                    &self.root,
                    path,
                    config,
                    source,
                    lines,
                    &crate::background::CancellationToken::new(),
                    &self.text,
                    self.process_ownership.as_ref(),
                )
            }
            Command::Search(query) => search::execute(
                &self.root,
                query,
                &crate::background::CancellationToken::new(),
            ),
            Command::SearchScope(query) => search::scope(&self.root, query,
                &crate::background::CancellationToken::new()),
            Command::Replace(query) => search::replace(
                &self.root,
                query,
                &crate::background::CancellationToken::new(),
                |path| {
                    self.documents
                        .lock()
                        .unwrap()
                        .remove(&self.root, path, false);
                },
            ),
            Command::Files {
                generation,
                known_version,
                excludes,
            } => {
                self.ensure_file_index(
                    generation,
                    true,
                    Some(&excludes),
                    &crate::background::CancellationToken::new(),
                )?;
                let index = self.index.as_ref().unwrap();
                Ok(Reply::Files {
                    version: index.version,
                    files: (known_version != Some(index.version)).then(|| index.files.clone()),
                })
            }
            Command::GitStatus {
                known,
                parent_open,
                closed,
                include_untracked,
                changed,
            } => {
                let excluded = closed.clone();
                let reply = git::status(
                &self.root,
                known,
                parent_open,
                closed,
                include_untracked,
                changed,
                &crate::background::CancellationToken::new(),
                )?;
                if let Reply::GitStatus { discovery, .. } = &reply {
                    let observation = self.observations.get(&self.root);
                    observation.repositories(&self.root, discovery, parent_open, &excluded);
                    self.git_observation = Some(observation);
                }
                Ok(reply)
            },
            Command::GitSources {
                file,
                repository,
                staged,
            } => git::sources(&self.root, file, repository, staged),
            Command::GitDiffSources { file, repository, staged, known } => {
                let file = file.resolve(&self.root)?;
                let repository = repository.as_ref().map(|path| path.resolve(&self.root)).transpose()?;
                let repo = crate::git_repositories::open_file_repository(&file, repository.as_deref()).map_err(io::Error::other)?;
                let relative = crate::app_git::repository_relative_path(
                    repo.workdir().ok_or_else(|| io::Error::other("Bare repository"))?, &file)
                    .ok_or_else(|| io::Error::other("File is outside the repository"))?;
                crate::git_diff_sources::read(&repo, &relative, staged, known)
                    .map(Reply::GitDiffSources).map_err(io::Error::other)
            }
            Command::GitBranches { repo } => {
                crate::app_git_branch::branches(&repo.resolve(&self.root)?)
                    .map(Reply::GitBranches)
                    .map_err(io::Error::other)
            }
            Command::GitNetwork { repo, args, push } => Ok(Reply::GitNetwork(git::network(
                &repo.resolve(&self.root)?,
                args,
                push,
            ))),
            Command::CommitDetails { repo, hash } => {
                crate::commit_details::load(&repo.resolve(&self.root)?, &hash)
                    .map(Reply::CommitDetails)
                    .map_err(io::Error::other)
            }
            Command::HunkSources {
                file,
                repository,
                staged,
            } => hunk::sources(&self.root, file, repository, staged).map(Reply::HunkSources),
            Command::HunkAction(action) => hunk::execute(&self.root, action).map(Reply::GitAction),
            Command::Blame { file, revision } => {
                blame::sources(&self.root, file, revision).map(Reply::Blame)
            }
            Command::BlameHistory {
                file,
                commit,
                source,
                line,
                limit,
            } => blame::history(&self.root, file, commit, source, line, limit)
                .map(Reply::BlameHistory),
            Command::BlameSnapshot {
                revision,
                line,
                limit,
            } => blame::snapshot(&self.root, revision, line, limit).map(Reply::BlameSnapshot),
            Command::BlameLineSnapshot {
                file,
                commit,
                source,
                line,
                limit,
            } => blame::line_snapshot(&self.root, file, commit, source, line, limit)
                .map(Reply::BlameSnapshot),
            Command::CommitDiff {
                repo,
                hash,
                file,
                limit,
            } => {
                let diff = crate::commit_details::read_diff(
                    &repo.resolve(&self.root)?,
                    &hash,
                    &file,
                    limit.min((MAX_PAYLOAD_BYTES / 2) as u64),
                )
                .map_err(io::Error::other)?;
                Ok(Reply::CommitDiff(git::CommitDiff {
                    path: git::from_native(&diff.path, &self.root)?,
                    base: diff.base,
                    target: diff.target,
                    labels: diff.labels,
                }))
            }
            Command::GitAction { repo, task } => {
                let task = task.try_map_paths(RemotePath::into_native_relative)?;
                let outcome = crate::git_tasks::execute(&repo.resolve(&self.root)?, task)
                    .map_err(io::Error::other)?;
                Ok(Reply::GitAction(outcome))
            }
            Command::Spawn {
                process,
                program,
                args,
                cwd,
                terminal,
                output_window,
            } => {
                let cwd = cwd.resolve(&self.root)?;
                self.processes
                    .as_mut()
                    .ok_or_else(|| io::Error::other("Remote process transport unavailable"))?
                    .spawn(process, &program, &args, &cwd, terminal, output_window)?;
                Ok(Reply::Done)
            }
            Command::ProcessInput { process, data } => {
                self.processes
                    .as_ref()
                    .ok_or_else(|| io::Error::other("Remote process transport unavailable"))?
                    .input(process, &data)?;
                Ok(Reply::Done)
            }
            Command::ProcessCredit { process, bytes } => {
                self.processes
                    .as_ref()
                    .ok_or_else(|| io::Error::other("Remote process transport unavailable"))?
                    .credit(process, bytes)?;
                Ok(Reply::Done)
            }
            Command::ProcessResize {
                process,
                width,
                height,
            } => {
                self.processes
                    .as_mut()
                    .ok_or_else(|| io::Error::other("Remote process transport unavailable"))?
                    .resize(process, width, height)?;
                Ok(Reply::Done)
            }
            Command::ProcessKill { process } => {
                self.processes
                    .as_mut()
                    .ok_or_else(|| io::Error::other("Remote process transport unavailable"))?
                    .kill(process);
                Ok(Reply::Done)
            }
            Command::PerformanceSnapshot => Ok(Reply::PerformanceSnapshot(observability::snapshot())),
            Command::Heartbeat { nonce } => Ok(Reply::Heartbeat { nonce }),
            Command::Ping => Ok(Reply::Done),
            Command::Shutdown => Ok(Reply::Done),
            Command::FilesystemRoots => {
                let roots = crate::filesystem_roots::list()?.into_iter()
                    .map(|(name, path)| Ok((name, git::from_native(&path, &self.root)?)))
                    .collect::<io::Result<_>>()?;
                Ok(Reply::FilesystemRoots(roots))
            }
            Command::List { path } => {
                let mut entries = Vec::new();
                for entry in fs::read_dir(path.resolve(&self.root)?)? {
                    let entry = entry?;
                    let name = entry.file_name();
                    #[cfg(unix)]
                    let bytes = {
                        use std::os::unix::ffi::OsStrExt;
                        name.as_bytes()
                    };
                    #[cfg(not(unix))]
                    let bytes = name
                        .to_str()
                        .ok_or_else(|| io::Error::other("Invalid remote filename"))?
                        .as_bytes();
                    if bytes == b".git" {
                        continue;
                    }
                    let kind = entry.file_type()?;
                    entries.push(Entry {
                        path: path.child(bytes)?,
                        is_dir: kind.is_dir() || (kind.is_symlink() && entry.path().is_dir()),
                    });
                    if entries.len() > 100_000 {
                        return Err(io::Error::other("Directory entry limit exceeded"));
                    }
                }
                entries.sort_unstable_by(|a, b| {
                    b.is_dir
                        .cmp(&a.is_dir)
                        .then_with(|| a.path.0.cmp(&b.path.0))
                });
                Ok(Reply::Entries(entries))
            }
            Command::Open { path } => {
                self.open_source(path, &crate::background::CancellationToken::new(), false)
            }
            Command::OpenLocation { path } => {
                self.open_location(path, &crate::background::CancellationToken::new())
            }
            Command::Reload { path, known, read_changed } => self.reload_source(path, known, read_changed, &crate::background::CancellationToken::new()),
            Command::Save {
                path,
                base,
                change,
                encoding: requested_encoding,
            } => {
                let encoding = encoding(&requested_encoding)?;
                let old = self.base_document(&path, base.as_ref())?;
                let source = old
                    .as_ref()
                    .map_or_else(ropey::Rope::new, |old| old.rope.clone());
                let rope = change.apply(&source)?;
                self.persist_document(path, old, rope, encoding)
            }
            Command::Text(text::Command::Read {
                transfer,
                start,
                end,
            }) => self
                .text
                .lock()
                .unwrap()
                .chunk(transfer, start..end)
                .map(|text| Reply::Text(text::Reply::Chunk(text))),
            Command::Text(text::Command::BeginSave {
                path,
                base,
                start,
                end,
                characters,
                bytes,
                encoding: selected,
            }) => {
                let source = self.base_document(&path, base.as_ref())?;
                if start > end || end > source.as_ref().map_or(0, |source| source.rope.len_chars())
                {
                    return Err(io::Error::other("Invalid save range"));
                }
                let save = text::Save {
                    root: self.root.clone(),
                    path,
                    source,
                    range: start..end,
                    encoding: encoding(&selected)?,
                };
                let transfer = self
                    .text
                    .lock()
                    .unwrap()
                    .begin(Some(save), characters, bytes)?;
                Ok(Reply::Text(text::Reply::Upload { transfer }))
            }
            Command::Text(text::Command::Append {
                transfer,
                offset,
                text,
            }) => {
                self.text.lock().unwrap().append(transfer, offset, &text)?;
                Ok(Reply::Done)
            }
            Command::Text(text::Command::Finish { transfer }) => {
                let (save, inserted) = self.text.lock().unwrap().finish(transfer)?;
                let save = save
                    .ok_or_else(|| io::Error::other("Tool input cannot be committed as a save"))?;
                if save.root != self.root {
                    return Err(io::Error::other("Save belongs to another remote folder"));
                }
                let mut rope = save
                    .source
                    .as_ref()
                    .map_or_else(ropey::Rope::new, |source| source.rope.clone());
                let suffix = rope.split_off(save.range.end);
                rope.remove(save.range.start..);
                rope.append(inserted);
                rope.append(suffix);
                self.persist_document(save.path, save.source, rope, save.encoding)
            }
            Command::Text(text::Command::BeginBlob { characters, bytes }) => {
                let transfer = self.text.lock().unwrap().begin(None, characters, bytes)?;
                Ok(Reply::Text(text::Reply::Upload { transfer }))
            }
            Command::Text(text::Command::FinishBlob { transfer }) => self
                .text
                .lock()
                .unwrap()
                .finish_blob(transfer)
                .map(Reply::Text),
            Command::Text(text::Command::Close { transfer }) => {
                self.text.lock().unwrap().close(transfer);
                Ok(Reply::Done)
            }
            Command::Close { path } => {
                self.documents
                    .lock()
                    .unwrap()
                    .remove(&self.root, &path, false);
                Ok(Reply::Done)
            }
            Command::CreateDirectory { path } => {
                fs::create_dir(path.resolve(&self.root)?)?;
                Ok(Reply::Done)
            }
            Command::Rename { from, to } => {
                let source = from.resolve(&self.root)?;
                let destination = to.resolve(&self.root)?;
                // Never let an explorer rename overwrite another file.
                crate::fs_ops::rename_no_replace(&source, &destination)?;
                self.clear_under(&from);
                self.clear_under(&to);
                Ok(Reply::Done)
            }
            Command::Remove { path, directory } => {
                if path.0.is_empty() {
                    return Err(io::Error::other("Cannot remove remote workspace root"));
                }
                let native = path.resolve(&self.root)?;
                if directory {
                    fs::remove_dir(&native)?;
                } else {
                    fs::remove_file(&native)?;
                }
                self.clear_under(&path);
                Ok(Reply::Done)
            }
        }
    }

    fn ensure_file_index(
        &mut self,
        generation: u64,
        include_hidden: bool,
        excludes: Option<&crate::settings::ExcludePatterns>,
        cancel: &crate::background::CancellationToken,
    ) -> io::Result<()> {
        let observation = self.observations.get(&self.root);
        let watch_stamp = observation.stamp();
        if self.index.as_ref().is_some_and(|index| {
            index.generation == generation
                && (!include_hidden || index.include_hidden)
                && index.excludes.as_ref() == excludes
                && !excludes.is_some_and(|map| map.values().any(serde_json::Value::is_object))
                && index.watch_stamp == watch_stamp
                // Shared filesystems can accept a watch yet omit other
                // machines' changes. Recheck on use, never on an idle timer.
                && index.built.elapsed() < std::time::Duration::from_secs(if watch_stamp.active { 30 } else { 2 })
        }) {
            return Ok(());
        }
        let include_hidden = include_hidden
            || self
                .index
                .as_ref()
                .is_some_and(|index| index.include_hidden);
        let rules = excludes.map(|map| crate::search_paths::ExcludeRules::new(map, &Default::default()));
        let mut files = Vec::new();
        let mut bytes = 0;
        for entry in walkdir::WalkDir::new(&self.root)
            .min_depth(1)
            .into_iter()
            .filter_entry(|entry| {
                let name = entry.file_name().to_str().unwrap_or("");
                !cancel.is_cancelled()
                    && !crate::editor::is_save_temporary_path(entry.path())
                    && rules.as_ref().map_or_else(|| (include_hidden || !name.starts_with('.'))
                        && (!entry.file_type().is_dir() || !crate::app_global_search::EXCLUDE_DIRS.contains(&name)),
                        |rules| !rules.excludes(&crate::search_paths::Candidate::new(entry.path()), &self.root))
            })
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            if cancel.is_cancelled() {
                return Err(io::ErrorKind::Interrupted.into());
            }
            let path = git::from_native(entry.path(), &self.root)?;
            bytes += path.0.len();
            if bytes > 32 * 1024 * 1024 || files.len() >= 1_000_000 {
                return Err(io::Error::other("Remote file index exceeds limit"));
            }
            files.push(path);
        }
        if cancel.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let labels = if include_hidden {
            files
                .iter()
                .map(|path| path.display().to_lowercase())
                .collect()
        } else {
            Vec::new()
        };
        let version = self
            .index
            .as_ref()
            .map_or(1, |index| index.version + u64::from(index.files != files));
        self.index = Some(FileIndex {
            generation,
            built: std::time::Instant::now(),
            version,
            files,
            labels,
            include_hidden,
            excludes: excludes.cloned(),
            watch_stamp,
            _observation: observation,
        });
        Ok(())
    }

    fn context_suggestions(
        &mut self,
        generation: u64,
        query: String,
        preferred: Vec<RemotePath>,
        explicit: Option<RemotePath>,
        cancel: &crate::background::CancellationToken,
    ) -> io::Result<Reply> {
        self.ensure_file_index(generation, true, None, cancel)?;
        let index = self.index.as_ref().unwrap();
        context::suggest(
            &self.root,
            &index.files,
            &index.labels,
            &query,
            preferred,
            explicit,
            cancel,
        )
        .map(Reply::Context)
    }
    fn clear_under(&mut self, path: &RemotePath) {
        self.documents
            .lock()
            .unwrap()
            .remove(&self.root, path, true);
    }
    fn base_document(
        &mut self,
        path: &RemotePath,
        base: Option<&Revision>,
    ) -> io::Result<Option<Document>> {
        let Some(base) = base else {
            return Ok(None);
        };
        if let Some(old) = self.documents.lock().unwrap().get(&self.root, path)
            && old.revision == *base
        {
            return Ok(Some(old));
        }
        let native = path.resolve(&self.root)?;
        let (loaded, _, _) = read_document(&native)?;
        if loaded.revision != *base {
            return Err(crate::editor::save::conflict(&native));
        }
        Ok(Some(loaded))
    }
    fn persist_document(
        &mut self,
        path: RemotePath,
        old: Option<Document>,
        rope: ropey::Rope,
        encoding: &'static str,
    ) -> io::Result<Reply> {
        let request = crate::editor::BackgroundSaveRequest {
            prepared_options: None,
            source_document_id: 0,
            path: path.resolve(&self.root)?,
            rope,
            encoding,
            line_ending: "LF",
            trim_trailing: false,
            insert_final_newline: false,
            source_content_version: 0,
            fail_if_exists: old.is_none(),
            expected_disk: old.as_ref().map(|old| crate::editor::DiskSaveBaseline {
                rope: old.rope.clone(),
                encoding: old.encoding,
            }),
        };
        let saved = crate::editor::write_background_save(&request)?;
        // The save is already committed. A concurrent rename/removal must not
        // turn this successful save into an apparent write failure.
        let identity = request
            .path
            .canonicalize()
            .ok()
            .and_then(|native| git::from_native(&native, &self.root).ok())
            .unwrap_or_else(|| path.clone());
        let revision = old
            .as_ref()
            .filter(|old| old.encoding == encoding && old.rope.is_instance(&saved.rope))
            .map_or_else(
                || revision(&saved.rope, encoding),
                |old| old.revision.clone(),
            );
        let size = saved
            .metadata
            .as_ref()
            .map_or(saved.rope.len_bytes() as u64, fs::Metadata::len);
        let modified = saved
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok());
        self.documents.lock().unwrap().insert(
            &self.root,
            path,
            Document {
                rope: saved.rope,
                encoding,
                revision: revision.clone(),
            },
        );
        Ok(Reply::Saved {
            revision,
            identity,
            size,
            modified,
        })
    }
}

pub(crate) fn serve(
    root: &Path,
    reader: impl Read,
    writer: impl Write + Send + 'static,
) -> io::Result<()> {
    serve_service(Service::new(root)?, reader, writer)
}

fn serve_service(
    mut service: Service,
    reader: impl Read,
    mut writer: impl Write + Send + 'static,
) -> io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    let process_ownership = process::Ownership::default();
    service.process_ownership = Some(process_ownership.clone());
    use std::hash::BuildHasher;
    let session = std::collections::hash_map::RandomState::new().hash_one(std::process::id());
    let hello = service.hello(session);
    // Printable readiness record is intentionally outside framed traffic so a
    // client can discover the unknown allocated compute node on its shell stream.
    let ready = format!(
        "VTCR-READY:{}",
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(&hello)?)
    );
    if ready.len() + 32 > wire::MAX_LINE {
        return Err(io::Error::other(
            "Remote workspace path exceeds the connection-header limit",
        ));
    }
    wire::write_record(&mut writer, &ready)?;
    writer.flush()?;
    let writer = std::sync::Arc::new(std::sync::Mutex::new(writer));
    let output_writer = writer.clone();
    let (output, messages) = std::sync::mpsc::sync_channel::<Response>(128);
    let current_search = std::sync::Arc::new(AtomicU64::new(0));
    let output_search = current_search.clone();
    std::thread::Builder::new()
        .name("headless-output".into())
        .spawn(move || {
            let mut queue = wire::OutputQueue::default();
            let _ = (|| -> io::Result<()> {
                loop {
                    let prepare_deadline = std::time::Instant::now()
                        + std::time::Duration::from_millis(1);
                    queue.discard_obsolete_search(output_search.load(Ordering::Acquire))?;
                    if queue.is_empty() {
                        let Ok(message) = messages.recv() else {
                            return Ok(());
                        };
                        queue.push(message)?;
                    }
                    // Bound encoding time as well as message count. Preparing
                    // sixteen bulk chunks before the first write adds visible
                    // latency even when the output pipe is otherwise idle.
                    for _ in 0..16.min(queue.available_slots()) {
                        if std::time::Instant::now() >= prepare_deadline { break; }
                        let Ok(message) = messages.try_recv() else { break; };
                        queue.push(message)?;
                    }
                    queue.discard_obsolete_search(output_search.load(Ordering::Acquire))?;
                    if !queue.is_empty() {
                        queue.step(&mut *output_writer.lock().unwrap(), session)?;
                    }
                }
            })();
        })?;
    // Separate actors retain their own state and bounded queues. Network input
    // never waits for filesystem I/O, a repository scan, or process backpressure.
    let canonical = service.root.clone();
    let agent_manager = agent::Manager::new(output.clone(), service.text.clone());
    service.agents = Some(agent_manager.clone());
    let text_store = service.text.clone();
    let documents = service.documents.clone();
    let observations = service.observations.clone();
    let new_service = || {
        let mut service = Service::from_root(canonical.clone());
        service.text = text_store.clone();
        service.agents = Some(agent_manager.clone());
        service.process_ownership = Some(process_ownership.clone());
        service.observations = observations.clone();
        service
    };
    let cancels = std::sync::Arc::new(std::sync::Mutex::new(HashMap::<
        u64,
        crate::background::CancellationToken,
    >::new()));
    struct CancelOnDisconnect(
        std::sync::Arc<std::sync::Mutex<HashMap<u64, crate::background::CancellationToken>>>,
    );
    impl Drop for CancelOnDisconnect {
        fn drop(&mut self) {
            for token in self.0.lock().unwrap().values() {
                token.cancel();
            }
        }
    }
    let cancel_on_disconnect = CancelOnDisconnect(cancels.clone());
    let mut active_search: Option<crate::background::CancellationToken> = None;
    let files =
        actor("headless-files", service, output.clone(), cancels.clone())?.drain_on_disconnect();
    let mut read_service = new_service();
    read_service.documents = documents.clone();
    let mut reads = LazyActor::new(
        "headless-reads",
        read_service,
        output.clone(),
        cancels.clone(),
    );
    let mut git = new_service();
    git.histories = Some(history::Manager::new(output.clone()));
    let mut git = LazyActor::new("headless-git", git, output.clone(), cancels.clone());
    let mut network = LazyActor::new(
        "headless-git-network",
        new_service(),
        output.clone(),
        cancels.clone(),
    );
    let mut index = LazyActor::new(
        "headless-index",
        new_service(),
        output.clone(),
        cancels.clone(),
    );
    let mut explorer = LazyActor::new(
        "headless-explorer",
        new_service(),
        output.clone(),
        cancels.clone(),
    );
    // A stat on an overloaded/network filesystem can be arbitrarily slow.
    // Keep fallback observations out of both durable file I/O and the watcher
    // actor (whose root changes would tear down the current subscription).
    let mut metadata = LazyActor::new(
        "headless-metadata",
        new_service(),
        output.clone(),
        cancels.clone(),
    );
    // Diagnostics must stay available while a data worker is stalled on I/O.
    // This actor is created only by an explicit user capture.
    let mut diagnostics = None;
    let mut binary = LazyActor::new(
        "headless-binary",
        new_service(),
        output.clone(),
        cancels.clone(),
    );
    let mut search = LazyActor::new(
        "headless-search",
        new_service(),
        output.clone(),
        cancels.clone(),
    );
    let mut filesystem_service = new_service();
    filesystem_service.documents = documents;
    let mut filesystem = LazyActor::new(
        "headless-filesystem",
        filesystem_service,
        output.clone(),
        cancels.clone(),
    )
    .drain_on_disconnect();
    let mut formatter = LazyActor::new(
        "headless-formatter",
        new_service(),
        output.clone(),
        cancels.clone(),
    )
    .drain_on_disconnect();
    let mut rollback: Option<Actor> = None;
    let mut agent_control = None;
    let mut agent_files = None;
    let mut agent_history = None;
    let mut context_actor = None;
    let mut observer = new_service();
    observer.output = Some(output.clone());
    let mut observer = LazyActor::new(
        "headless-observer",
        observer,
        output.clone(),
        cancels.clone(),
    );
    let mut tools = new_service();
    tools.processes = Some(process::Manager::new(
        output.clone(),
        process_ownership.clone(),
    ));
    // Actor teardown is asynchronous. Kill owned tools before main can exit,
    // including on EOF, a malformed stream, or a hangup from a dead SSH job.
    let mut processes =
        LazyActor::new("headless-processes", tools, output.clone(), cancels.clone());
    let _stop_processes = process_ownership.shutdown_guard();
    struct StopAgents(std::sync::Arc<agent::Manager>);
    impl Drop for StopAgents {
        fn drop(&mut self) {
            self.0.shutdown();
        }
    }
    let _stop_agents = StopAgents(agent_manager.clone());
    // Cancel queued work before killing tools and joining mutation workers, so
    // disconnect cannot start another save, deletion or formatter from a queue.
    let _cancel_on_disconnect = cancel_on_disconnect;
    let mut decoder = wire::Decoder::new('C', session);
    let mut early_cancellations = std::collections::HashSet::new();
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    while wire::read_line(&mut reader, &mut line)? {
        let Some((id, data)) = decoder.line(&line)? else {
            continue;
        };
        let request: Request = serde_json::from_slice(&data)?;
        if request.id != id {
            return Err(io::Error::other("Remote request ID mismatch"));
        }
        if let Command::Heartbeat { nonce } = &request.command {
            // Answer only legacy peers that explicitly send this request.
            // Current clients have no idle timer or automatic probes.
            if id == 0 {
                output.send(Response { id, result: Ok(Reply::Heartbeat { nonce: *nonce }) })
                    .map_err(|error| io::Error::new(io::ErrorKind::BrokenPipe, error))?;
            } else {
                let response = wire::encode(&Response {
                    id, result: Ok(Reply::Heartbeat { nonce: *nonce }),
                })?;
                wire::write_message(&mut *writer.lock().unwrap(), 'S', session, id, &response)?;
            }
            continue;
        }
        if id == 0
            && matches!(
                request.command,
                Command::Agent(
                    agent::Command::Stop { .. }
                        | agent::Command::Credit { .. }
                        | agent::Command::HistoryClose { .. }
                )
            )
        {
            let Command::Agent(command) = request.command else {
                unreachable!()
            };
            agent_manager.notify(command)?;
            continue;
        }
        if id == 0 {
            // Cleanup has separate bounded capacity, so closing a view cannot
            // disconnect the entire editor when its data worker is saturated.
            // Connection-wide IDs also require no workspace/cache retargeting.
            match &request.command {
                Command::Text(text::Command::Close { transfer }) => {
                    files.close(ResourceClose::Text(*transfer))?;
                    continue;
                }
                Command::Binary(binary::Command::Close { source }) => {
                    binary.close(ResourceClose::Binary(*source))?;
                    continue;
                }
                Command::HistoryClose { history } => {
                    git.close(ResourceClose::History(*history))?;
                    continue;
                }
                Command::Rollback(rollback::Command::Close { journal }) => {
                    if let Some(worker) = &rollback {
                        worker.close(ResourceClose::Rollback(*journal))?;
                    }
                    continue;
                }
                _ => {}
            }
        }
        if id == 0 && matches!(request.command, Command::Close { .. }) {
            // This only evicts a bounded LRU checkpoint, not a leased resource.
            // Ordinary eviction can reclaim it if the data queue is saturated.
            match files.enqueue(request, None) {
                Ok(()) | Err(std::sync::mpsc::TrySendError::Full(_)) => continue,
                Err(error) => return Err(io::Error::new(io::ErrorKind::BrokenPipe, error)),
            }
        }
        if matches!(request.command, Command::Shutdown) && id != 0 {
            // Publish the final acknowledgement before main can exit and tear
            // down its output thread. Queueing it alone races process exit.
            let response = wire::encode(&Response {
                id,
                result: Ok(Reply::Done),
            })?;
            wire::write_message(&mut *writer.lock().unwrap(), 'S', session, id, &response)?;
            break;
        }
        if let Command::Cancel { request: cancelled } = &request.command {
            let _ = current_search.compare_exchange(*cancelled, 0, Ordering::AcqRel, Ordering::Acquire);
            if let Some(cancel) = cancels.lock().unwrap().get(cancelled) {
                cancel.cancel();
            } else if decoder.in_progress(*cancelled) {
                // Cancellation can overtake the remaining frames of an upload.
                // Remember it until that request finishes reassembly.
                if early_cancellations.len() >= wire::MAX_PENDING {
                    return Err(io::Error::other("Remote cancellation limit exceeded"));
                }
                early_cancellations.insert(*cancelled);
            }
            continue;
        }
        if early_cancellations.remove(&id) {
            output
                .send(Response {
                    id,
                    result: Err(io::Error::from(io::ErrorKind::Interrupted).into()),
                })
                .map_err(io::Error::other)?;
            continue;
        }
        if id == 0
            && !matches!(
                request.command,
                Command::ProcessInput { .. }
                    | Command::ProcessCredit { .. }
                    | Command::ProcessResize { .. }
                    | Command::ProcessKill { .. }
                    | Command::Close { .. }
                    | Command::Watch
                    | Command::HistoryClose { .. }
                    | Command::Binary(binary::Command::Close { .. })
                    | Command::Text(text::Command::Close { .. })
                    | Command::Rollback(rollback::Command::Close { .. })
                    | Command::Agent(
                        agent::Command::Stop { .. }
                            | agent::Command::Credit { .. }
                            | agent::Command::Cancel { .. }
                            | agent::Command::HistoryClose { .. }
                    )
            )
        {
            return Err(io::Error::other(
                "Only bounded process notifications may omit a request ID",
            ));
        }
        let cancel = if matches!(
            request.command,
            Command::Context(_)
                | Command::Search(_)
                | Command::SearchScope(_)
                | Command::Replace(_)
                | Command::Filesystem(_)
                | Command::Format { .. }
                | Command::FormatSource { .. }
                | Command::Explorer(_)
                | Command::Inspect { .. }
                | Command::OpenLocation { .. }
                | Command::Open { .. }
                | Command::Reload { .. }
                | Command::GitStatus { .. }
                | Command::GitDiffSources { .. }
        ) {
            let cancel = crate::background::CancellationToken::new();
            if matches!(request.command, Command::Search(_)) {
                current_search.store(id, Ordering::Release);
                if let Some(old) = active_search.replace(cancel.clone()) { old.cancel(); }
            }
            cancels.lock().unwrap().insert(id, cancel.clone());
            Some(cancel)
        } else {
            None
        };
        let queue = match request.command {
            Command::PerformanceSnapshot => {
                if diagnostics.is_none() {
                    diagnostics = Some(actor("headless-diagnostics", new_service(), output.clone(), cancels.clone())?);
                }
                diagnostics.as_ref().unwrap()
            }
            Command::Open { .. } | Command::OpenLocation { .. } | Command::Reload { .. } => reads.get()?,
            Command::Context(context::Command::Suggest { .. }) => index.get()?,
            Command::Context(_) => {
                if context_actor.is_none() {
                    context_actor = Some(actor(
                        "headless-chat-context",
                        new_service(),
                        output.clone(),
                        cancels.clone(),
                    )?);
                }
                context_actor.as_ref().unwrap()
            }
            Command::Rollback(_) => {
                if rollback.is_none() {
                    rollback = Some(
                        actor(
                            "headless-checkpoints",
                            new_service(),
                            output.clone(),
                            cancels.clone(),
                        )?
                        .drain_on_disconnect(),
                    );
                }
                rollback.as_ref().unwrap()
            }
            Command::Agent(agent::Command::File { .. }) => {
                if agent_files.is_none() {
                    agent_files = Some(actor(
                        "headless-agent-files",
                        new_service(),
                        output.clone(),
                        cancels.clone(),
                    )?);
                }
                agent_files.as_ref().unwrap()
            }
            Command::Agent(agent::Command::HistoryRead { .. }) => {
                if agent_history.is_none() {
                    agent_history = Some(actor(
                        "headless-agent-history",
                        new_service(),
                        output.clone(),
                        cancels.clone(),
                    )?);
                }
                agent_history.as_ref().unwrap()
            }
            Command::Agent(_) => {
                if agent_control.is_none() {
                    agent_control = Some(actor(
                        "headless-agents",
                        new_service(),
                        output.clone(),
                        cancels.clone(),
                    )?);
                }
                agent_control.as_ref().unwrap()
            }
            Command::Binary(_) => binary.get()?,
            Command::Spawn { .. }
            | Command::ProcessInput { .. }
            | Command::ProcessCredit { .. }
            | Command::ProcessResize { .. }
            | Command::ProcessKill { .. } => processes.get()?,
            Command::GitStatus { .. }
            | Command::GitAction { .. }
            | Command::GitSources { .. }
            | Command::GitDiffSources { .. }
            | Command::GitBranches { .. }
            | Command::CommitDetails { .. }
            | Command::CommitDiff { .. }
            | Command::HunkSources { .. }
            | Command::HunkAction(_)
            | Command::Blame { .. }
            | Command::BlameHistory { .. }
            | Command::BlameSnapshot { .. }
            | Command::BlameLineSnapshot { .. }
            | Command::HistoryStart { .. }
            | Command::HistoryNext { .. }
            | Command::HistoryClose { .. } => git.get()?,
            Command::GitNetwork { .. } => network.get()?,
            Command::Files { .. } => index.get()?,
            Command::Explorer(_) => explorer.get()?,
            Command::FilesystemRoots | Command::Inspect { .. } | Command::SearchScope(_) => metadata.get()?,
            Command::Search(_) => search.get()?,
            Command::Filesystem(_) | Command::Replace(_) => filesystem.get()?,
            Command::Format { .. } | Command::FormatSource { .. } => formatter.get()?,
            Command::Watch => observer.get()?,
            _ => &files,
        };
        if let Err(error) = queue.enqueue(request, cancel) {
            cancels.lock().unwrap().remove(&id);
            if id == 0 {
                return Err(io::Error::other("Remote control queue limit exceeded"));
            }
            output
                .try_send(Response {
                    id,
                    result: Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        format!("Remote worker is busy: {error}"),
                    )
                    .into()),
                })
                .map_err(io::Error::other)?;
        }
    }
    Ok(())
}

type Cancellations =
    std::sync::Arc<std::sync::Mutex<HashMap<u64, crate::background::CancellationToken>>>;

/// Optional services consume no thread or worker stack until their first use.
/// Once started they retain the same isolated queue and teardown guarantees.
struct LazyActor {
    name: &'static str,
    service: Option<Service>,
    output: std::sync::mpsc::SyncSender<Response>,
    cancels: Cancellations,
    actor: Option<Actor>,
    drain: bool,
}
impl LazyActor {
    fn new(
        name: &'static str,
        service: Service,
        output: std::sync::mpsc::SyncSender<Response>,
        cancels: Cancellations,
    ) -> Self {
        Self {
            name,
            service: Some(service),
            output,
            cancels,
            actor: None,
            drain: false,
        }
    }
    fn drain_on_disconnect(mut self) -> Self {
        self.drain = true;
        self
    }
    fn close(&self, resource: ResourceClose) -> io::Result<()> {
        // An unused service has no handles and must stay lazy.
        self.actor
            .as_ref()
            .map_or(Ok(()), |actor| actor.close(resource))
    }
    fn get(&mut self) -> io::Result<&Actor> {
        if self.actor.is_none() {
            let mut worker = actor(
                self.name,
                self.service.take().expect("unstarted actor"),
                self.output.clone(),
                self.cancels.clone(),
            )?;
            if self.drain {
                worker = worker.drain_on_disconnect();
            }
            self.actor = Some(worker);
        }
        Ok(self.actor.as_ref().unwrap())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum ResourceClose {
    Text(u64),
    Binary(u64),
    History(u64),
    Rollback(u64),
}

#[derive(Default)]
struct ResourceCloses {
    pending: std::sync::atomic::AtomicBool,
    entries: std::sync::Mutex<HashMap<ResourceClose, u64>>,
}
impl ResourceCloses {
    fn apply(&self, service: &mut Service, completed: u64, ready: &mut Vec<ResourceClose>) {
        use std::sync::atomic::Ordering;
        if !self.pending.load(Ordering::Acquire) {
            return;
        }
        {
            let mut entries = self.entries.lock().unwrap();
            entries.retain(|resource, after| {
                if *after > completed {
                    return true;
                }
                ready.push(*resource);
                false
            });
            self.pending.store(!entries.is_empty(), Ordering::Release);
        }
        // Never hold the mailbox lock while releasing file/journal handles.
        if ready
            .iter()
            .any(|resource| matches!(resource, ResourceClose::Text(_)))
        {
            let mut text = service.text.lock().unwrap();
            for resource in ready.iter() {
                if let ResourceClose::Text(id) = resource {
                    text.close(*id);
                }
            }
        }
        for resource in ready.drain(..) {
            match resource {
                ResourceClose::Text(_) => {}
                ResourceClose::Binary(id) => service.binary.close(id),
                ResourceClose::History(id) => {
                    if let Some(histories) = &mut service.histories {
                        histories.close(id);
                    }
                }
                ResourceClose::Rollback(id) => service.rollback.close(id),
            }
        }
    }
}

type ActorRequest = (Request, Option<crate::background::CancellationToken>);
struct Actor {
    sender: std::sync::mpsc::SyncSender<Option<(u64, ActorRequest)>>,
    // Only the connection reader enqueues work. Tickets preserve ordering when
    // a cancelled history start is still queued when its Close arrives.
    queued: std::cell::Cell<u64>,
    closes: std::sync::Arc<ResourceCloses>,
    worker: Option<std::thread::JoinHandle<()>>,
    join: bool,
}
impl Actor {
    fn enqueue(
        &self,
        request: Request,
        cancel: Option<crate::background::CancellationToken>,
    ) -> Result<(), std::sync::mpsc::TrySendError<Option<(u64, ActorRequest)>>> {
        let ticket = self
            .queued
            .get()
            .checked_add(1)
            .expect("remote work ticket overflow");
        self.sender.try_send(Some((ticket, (request, cancel))))?;
        self.queued.set(ticket);
        Ok(())
    }
    fn close(&self, resource: ResourceClose) -> io::Result<()> {
        let mut closes = self.closes.entries.lock().unwrap();
        // This exceeds the live binary (1024), text/context (40), history (4)
        // and journal (8) handle limits while still bounding malformed peers.
        if closes.len() >= 2048 && !closes.contains_key(&resource) {
            return Err(io::Error::other("Remote cleanup queue limit exceeded"));
        }
        let wake = closes.is_empty();
        closes.insert(resource, self.queued.get());
        self.closes
            .pending
            .store(true, std::sync::atomic::Ordering::Release);
        if !wake {
            // The first release already woke the worker. A later barrier can
            // only refer to data already in its inbox; no extra wake is needed.
            return Ok(());
        }
        // Hold the mailbox lock through the wake attempt. A full inbox keeps
        // the worker awake until it reaches the release ticket; it cannot
        // empty the inbox and then miss the notification.
        match self.sender.try_send(None) {
            Ok(()) | Err(std::sync::mpsc::TrySendError::Full(_)) => Ok(()),
            Err(error) => Err(io::Error::new(io::ErrorKind::BrokenPipe, error)),
        }
    }
    fn drain_on_disconnect(mut self) -> Self {
        // Finish durable checkpoint writes before the daemon exits. This runs
        // only on the service, never on the workbench's input/render thread.
        self.join = true;
        self
    }
}
impl Drop for Actor {
    fn drop(&mut self) {
        // Process resources must be killed before the daemon's main returns.
        // Slow filesystem jobs do not hold connection teardown hostage.
        if self.join {
            let (replacement, _) = std::sync::mpsc::sync_channel(1);
            drop(std::mem::replace(&mut self.sender, replacement));
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }
}
fn actor(
    name: &'static str,
    mut service: Service,
    output: std::sync::mpsc::SyncSender<Response>,
    cancels: std::sync::Arc<std::sync::Mutex<HashMap<u64, crate::background::CancellationToken>>>,
) -> io::Result<Actor> {
    let default_root = service.root.clone();
    let join = service.processes.is_some();
    let (sender, requests) =
        std::sync::mpsc::sync_channel::<Option<(u64, ActorRequest)>>(if join { 128 } else { 8 });
    let closes = std::sync::Arc::new(ResourceCloses::default());
    let worker_closes = closes.clone();
    let worker = std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            // Linux children inherit their spawning thread's nice value.
            // Shells and tools must retain the service's priority, rather than
            // inherit the scheduling hint intended for background file jobs.
            if !join {
                crate::worker_priority::lower_current_thread();
            }
            let mut completed = 0;
            let mut ready = Vec::new();
            for message in requests {
                worker_closes.apply(&mut service, completed, &mut ready);
                let Some((ticket, (request, cancel))) = message else {
                    continue;
                };
                let root = request.workspace.as_ref().map_or_else(
                    || Ok(default_root.clone()),
                    |root| {
                        url::Url::parse(root)
                            .map_err(io::Error::other)
                            .and_then(|uri| {
                                uri.to_file_path()
                                    .map_err(|_| io::Error::other("Invalid workspace context URI"))
                            })
                    },
                );
                let root = match root {
                    Ok(root) => root,
                    Err(error) => {
                        completed = ticket;
                        worker_closes.apply(&mut service, completed, &mut ready);
                        cancels.lock().unwrap().remove(&request.id);
                        let _ = output.send(Response {
                            id: request.id,
                            result: Err(error.into()),
                        });
                        continue;
                    }
                };
                if matches!(request.command, Command::Close { .. }) && service.root != root {
                    // There is no checkpoint for this old root in the current
                    // cache. Do not switch roots just to evict a nonexistent
                    // entry, or evict a same-named file in the current folder.
                    completed = ticket;
                    worker_closes.apply(&mut service, completed, &mut ready);
                    if request.id != 0 {
                        let _ = output.send(Response {
                            id: request.id,
                            result: Ok(Reply::Done),
                        });
                    }
                    continue;
                }
                if service.root != root {
                    service.root = root;
                    service.index = None;
                    service.explorer = None;
                    // Process and history IDs are connection-wide. Their
                    // resources retain their own cwd; only the watcher changes.
                    service.watcher = None;
                }
                let process_id = match &request.command {
                    Command::ProcessInput { process, .. }
                    | Command::ProcessResize { process, .. }
                    | Command::ProcessCredit { process, .. } => Some(*process),
                    _ => None,
                };
                let result = if cancel.as_ref().is_some_and(|cancel| cancel.is_cancelled()) {
                    Err(io::Error::from(io::ErrorKind::Interrupted))
                } else {
                    match request.command {
                        Command::Open { path } => {
                            service.open_source(path, cancel.as_ref().unwrap(), false)
                        }
                        Command::OpenLocation { path } => {
                            service.open_location(path, cancel.as_ref().unwrap())
                        }
                        Command::Reload { path, known, read_changed } => service.reload_source(path, known, read_changed, cancel.as_ref().unwrap()),
                        Command::Inspect { paths } => {
                            watch::inspect(&service.root, paths, cancel.as_ref().unwrap())
                        }
                        Command::Context(context::Command::Suggest {
                            generation,
                            query,
                            preferred,
                            explicit,
                        }) => service.context_suggestions(
                            generation,
                            query,
                            preferred,
                            explicit,
                            cancel.as_ref().unwrap(),
                        ),
                        Command::Context(command) => context::execute(
                            &service.root,
                            command,
                            cancel.as_ref().unwrap(),
                            &service.text,
                        )
                        .map(Reply::Context),
                        Command::GitStatus {
                            known,
                            parent_open,
                            closed,
                            include_untracked,
                            changed,
                        } => git::status(
                            &service.root,
                            known,
                            parent_open,
                            closed,
                            include_untracked,
                            changed,
                            cancel.as_ref().unwrap(),
                        ),
                        Command::Search(query) => {
                            if query.stream {
                                search::execute_streaming(&service.root, query, cancel.as_ref().unwrap(), |hits| {
                                    output.send(Response { id: 0, result: Ok(Reply::SearchProgress { request: request.id, hits }) })
                                        .map_err(io::Error::other)
                                })
                            } else {
                                search::execute(&service.root, query, cancel.as_ref().unwrap())
                            }
                        }
                        Command::SearchScope(query) => search::scope(&service.root, query, cancel.as_ref().unwrap()),
                        Command::Replace(query) => search::replace(
                            &service.root,
                            query,
                            cancel.as_ref().unwrap(),
                            |path| {
                                service.documents.lock().unwrap().remove(
                                    &service.root,
                                    path,
                                    false,
                                );
                            },
                        ),
                        Command::Explorer(query) => explorer::read(
                            &service.root,
                            &mut service.explorer,
                            query,
                            cancel.as_ref().unwrap(),
                        )
                        .map(Reply::Explorer),
                        Command::Format {
                            path,
                            config,
                            text,
                            lines,
                        } => format(
                            &service.root,
                            path,
                            config,
                            ropey::Rope::from_str(&text),
                            lines,
                            cancel.as_ref().unwrap(),
                            &service.text,
                            service.process_ownership.as_ref(),
                        ),
                        Command::FormatSource {
                            transfer,
                            path,
                            config,
                            lines,
                        } => {
                            let source = service.text.lock().unwrap().snapshot(transfer);
                            source.and_then(|source| {
                                format(
                                    &service.root,
                                    path,
                                    config,
                                    source,
                                    lines,
                                    cancel.as_ref().unwrap(),
                                    &service.text,
                                    service.process_ownership.as_ref(),
                                )
                            })
                        }
                        Command::Filesystem(task) => {
                            let mut last = std::time::Instant::now();
                            super::fs::execute(
                                &service.root,
                                task,
                                cancel.as_ref().unwrap(),
                                |progress, force| {
                                    if force
                                        || last.elapsed() >= std::time::Duration::from_millis(80)
                                    {
                                        last = std::time::Instant::now();
                                        let _ = output.try_send(Response {
                                            id: 0,
                                            result: Ok(Reply::DeletionProgress {
                                                request: request.id,
                                                progress,
                                            }),
                                        });
                                    }
                                },
                            )
                            .map(Reply::Filesystem)
                        }
                        command => service.execute(command),
                    }
                }
                .map_err(Failure::from);
                completed = ticket;
                worker_closes.apply(&mut service, completed, &mut ready);
                cancels.lock().unwrap().remove(&request.id);
                if request.id == 0
                    && let (Some(process), Err(error)) = (process_id, &result)
                {
                    if let Some(manager) = &mut service.processes {
                        manager.kill(process);
                    }
                    if output
                        .send(Response {
                            id: 0,
                            result: Ok(Reply::ProcessEnd {
                                process,
                                error: Some(error.to_string()),
                            }),
                        })
                        .is_err()
                    {
                        // No more queued acquisitions will execute after the
                        // reply pipe disappears. Release pending leases even
                        // when their barriers refer to that skipped work.
                        worker_closes.apply(&mut service, u64::MAX, &mut ready);
                        return;
                    }
                }
                if request.id != 0
                    && output
                        .send(Response {
                            id: request.id,
                            result,
                        })
                        .is_err()
                {
                    worker_closes.apply(&mut service, u64::MAX, &mut ready);
                    return;
                }
            }
            worker_closes.apply(&mut service, u64::MAX, &mut ready);
        })?;
    Ok(Actor {
        sender,
        queued: std::cell::Cell::new(0),
        closes,
        worker: Some(worker),
        join,
    })
}

fn format(
    root: &Path,
    path: RemotePath,
    config: crate::formatters::Config,
    source: ropey::Rope,
    lines: Option<(usize, usize)>,
    cancel: &crate::background::CancellationToken,
    transfers: &std::sync::Arc<std::sync::Mutex<text::Store>>,
    ownership: Option<&process::Ownership>,
) -> io::Result<Reply> {
    let started = |killer| {
        if let Some(ownership) = ownership {
            ownership.track(killer);
        }
    };
    let output = crate::formatters::run_output_tracked(
        &config,
        &path.resolve(root)?,
        root,
        &source,
        lines,
        cancel.flag(),
        std::time::Duration::from_secs(30),
        Some(&started),
    )
    .map_err(io::Error::other)?;
    if output.len() <= text::INLINE_BYTES {
        return Ok(Reply::Formatted(output));
    }
    let rope = ropey::Rope::from_str(&output);
    let characters = rope.len_chars();
    let bytes = rope.len_bytes();
    let transfer = transfers.lock().unwrap().read(rope)?;
    Ok(Reply::Text(text::Reply::Source {
        transfer,
        characters,
        bytes,
    }))
}

pub(crate) fn run(root: &Path) -> io::Result<()> {
    #[cfg(unix)]
    let mut input = signal::Input::new()?;
    #[cfg(windows)]
    let _raw = {
        use std::io::IsTerminal;
        if io::stdin().is_terminal() {
            let mode = crate::windows_input::ConsoleInputMode::save()?;
            mode.prepare_bridge()?;
            Some(mode)
        } else {
            None
        }
    };
    #[cfg(unix)]
    let _raw = RawTerminal::enter()?;
    #[cfg(unix)]
    {
        serve(root, &mut input, io::stdout())
    }
    #[cfg(not(unix))]
    {
        serve(root, io::stdin(), io::stdout())
    }
}

#[cfg(unix)]
struct RawTerminal(Option<libc::termios>);
#[cfg(unix)]
impl RawTerminal {
    fn enter() -> io::Result<Self> {
        unsafe {
            if libc::isatty(libc::STDIN_FILENO) == 0 {
                return Ok(Self(None));
            }
            let mut old = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut old) != 0 {
                return Err(io::Error::last_os_error());
            }
            let mut raw = old;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(Some(old)))
        }
    }
}
#[cfg(unix)]
impl Drop for RawTerminal {
    fn drop(&mut self) {
        if let Some(old) = self.0 {
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &old);
            }
        }
    }
}
