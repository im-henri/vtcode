//! Explicit chat attachments use bounded text reads, never the document loader.
//! Inspecting aliases is separate so open local Ropes can take precedence without
//! downloading their saved contents. All methods run on background workers.
use super::*;
use crate::background::CancellationToken;
use std::io::Read;
#[path = "remote_context_source.rs"]
mod source;
pub(super) use source::{Snapshot, Snapshots, materialize};
pub(crate) use source::{Source, retain};
#[cfg(test)]
#[path = "remote_context_tests.rs"]
mod tests;

pub(crate) const FILE_LIMIT: usize = 128 * 1024;
pub(crate) const TOTAL_LIMIT: usize = 1024 * 1024;
pub(crate) const FILE_COUNT: usize = 32;
pub(crate) const INSPECT_BATCH: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Kind {
    File,
    Directory,
    Missing,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Info {
    pub identity: RemotePath,
    pub kind: Kind,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Command {
    Inspect {
        paths: Vec<RemotePath>,
    },
    Files {
        files: Vec<(RemotePath, RemotePath)>,
        retain: bool,
    },
    Suggest {
        generation: u64,
        query: String,
        preferred: Vec<RemotePath>,
        explicit: Option<RemotePath>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Reply {
    Inspected(Vec<Result<Info, String>>),
    Files {
        source: Option<u64>,
        sizes: Vec<usize>,
    },
    Suggestions(Vec<RemotePath>),
}

fn check(cancel: &CancellationToken) -> io::Result<()> {
    if cancel.is_cancelled() {
        Err(io::ErrorKind::Interrupted.into())
    } else {
        Ok(())
    }
}

/// Keep a stable identity for a new, unsaved file, while rejecting broken links
/// and inaccessible ancestors instead of guessing their target.
pub(crate) fn inspect_native(path: &Path) -> io::Result<(PathBuf, Kind)> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let kind = match metadata {
        Some(metadata) if metadata.is_file() => Kind::File,
        Some(metadata) if metadata.is_dir() => Kind::Directory,
        Some(_) => Kind::Other,
        None => Kind::Missing,
    };
    let mut ancestor = path;
    let mut missing = Vec::new();
    let mut identity = loop {
        match ancestor.canonicalize() {
            Ok(path) => break path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if ancestor.symlink_metadata().is_ok() {
                    return Err(error);
                }
                missing.push(
                    ancestor
                        .file_name()
                        .ok_or_else(|| io::Error::other("Invalid file path"))?,
                );
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| io::Error::other("Invalid file path"))?;
            }
            Err(error) => return Err(error),
        }
    };
    for part in missing.into_iter().rev() {
        identity.push(part);
    }
    Ok((identity, kind))
}

pub(crate) fn read_native(path: &Path, cancel: &CancellationToken) -> io::Result<String> {
    check(cancel)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A path retargeted to a FIFO must not strand a worker in open().
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::other("choose an existing text file."));
    }
    if metadata.len() > FILE_LIMIT as u64 {
        return Err(io::Error::other(
            "file context is limited to 128 KiB per file.",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    let mut input = file.take(FILE_LIMIT as u64 + 1);
    let mut chunk = [0; 16 * 1024];
    loop {
        check(cancel)?;
        let read = input.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    if bytes.len() > FILE_LIMIT {
        return Err(io::Error::other(
            "file context is limited to 128 KiB per file.",
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| io::Error::other("only UTF-8 text files can be attached."))?;
    if text.contains('\0') {
        return Err(io::Error::other("binary files cannot be attached."));
    }
    Ok(text)
}

pub(super) fn execute(
    root: &Path,
    command: Command,
    cancel: &CancellationToken,
    store: &std::sync::Mutex<text::Store>,
) -> io::Result<Reply> {
    match command {
        Command::Suggest { .. } => Err(io::Error::other(
            "File suggestions require the filename index",
        )),
        Command::Inspect { paths } => {
            if paths.len() > INSPECT_BATCH {
                return Err(io::Error::other("File inspection batch is too large"));
            }
            let mut values = Vec::with_capacity(paths.len());
            for path in paths {
                check(cancel)?;
                values.push(
                    path.resolve(root)
                        .and_then(|path| inspect_native(&path))
                        .and_then(|(identity, kind)| {
                            Ok(Info {
                                identity: git::from_native(&identity, root)?,
                                kind,
                            })
                        })
                        .map_err(|error| error.to_string()),
                );
            }
            Ok(Reply::Inspected(values))
        }
        Command::Files { files, retain } => {
            if files.len() > FILE_COUNT {
                return Err(io::Error::other("Attach up to 32 files per message."));
            }
            let mut texts = Vec::new();
            let mut sizes = Vec::with_capacity(files.len());
            let mut total = 0;
            for (path, expected) in files {
                check(cancel)?;
                let loaded = (|| -> io::Result<String> {
                    let native = path.resolve(root)?;
                    let (identity, kind) = inspect_native(&native)?;
                    if git::from_native(&identity, root)? != expected {
                        return Err(io::Error::other(
                            "An attached file moved or its link changed. Send the message again.",
                        ));
                    }
                    if kind != Kind::File {
                        return Err(io::Error::other("choose an existing text file."));
                    }
                    read_native(&identity, cancel)
                })()
                .map_err(|error| {
                    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
                })?;
                let text = loaded;
                total += text.len();
                if total > TOTAL_LIMIT {
                    return Err(io::Error::other(
                        "File context is limited to 1 MiB per message. Remove some files.",
                    ));
                }
                sizes.push(text.len());
                if retain {
                    texts.push((path, text));
                }
            }
            check(cancel)?;
            let source = if retain && !texts.is_empty() {
                Some(store.lock().unwrap().context(Snapshot {
                    root: root.to_path_buf(),
                    files: texts,
                })?)
            } else {
                None
            };
            Ok(Reply::Files { source, sizes })
        }
    }
}

pub(super) fn suggest(
    root: &Path,
    files: &[RemotePath],
    labels: &[String],
    query: &str,
    preferred: Vec<RemotePath>,
    explicit: Option<RemotePath>,
    cancel: &CancellationToken,
) -> io::Result<Reply> {
    use std::collections::{BinaryHeap, HashSet};
    const LIMIT: usize = 40;
    let query = query.trim_start_matches("./").to_lowercase();
    let mut seen = HashSet::new();
    let mut best = BinaryHeap::new();
    if preferred.len() > INSPECT_BATCH {
        return Err(io::Error::other("Too many preferred file suggestions"));
    }
    let preferred_labels: Vec<_> = preferred
        .iter()
        .map(|path| path.display().to_lowercase())
        .collect();
    for (path, label) in preferred.iter().zip(&preferred_labels) {
        check(cancel)?;
        if seen.insert(path.clone()) && path.resolve(root)?.is_file() {
            add_candidate(&mut best, path, label, &query, true);
        }
    }
    for (path, label) in files.iter().zip(labels) {
        check(cancel)?;
        if !seen.contains(path) {
            add_candidate(&mut best, path, label, &query, false);
        }
    }
    let mut paths: Vec<_> = best
        .into_sorted_vec()
        .into_iter()
        .map(|value| value.2.clone())
        .collect();
    if let Some(path) = explicit
        && path.resolve(root)?.is_file()
    {
        paths.retain(|value| value != &path);
        paths.insert(0, path);
    }
    paths.truncate(LIMIT);
    Ok(Reply::Suggestions(paths))
}

fn add_candidate<'a>(
    heap: &mut std::collections::BinaryHeap<Ranked<'a>>,
    path: &'a RemotePath,
    label: &'a str,
    query: &str,
    preferred: bool,
) {
    let Some(score) = (if query.is_empty() {
        Some(0)
    } else {
        crate::app::fuzzy_match_score(label, query)
    }) else {
        return;
    };
    let value = Ranked(
        std::cmp::Reverse(score + if preferred { 1000 } else { 0 }),
        label,
        path,
    );
    if heap.len() < 40 {
        heap.push(value);
    } else if let Some(mut worst) = heap.peek_mut()
        && value < *worst
    {
        *worst = value;
    }
}
struct Ranked<'a>(std::cmp::Reverse<i64>, &'a str, &'a RemotePath);
impl PartialEq for Ranked<'_> {
    fn eq(&self, other: &Self) -> bool {
        (&self.0, &self.1) == (&other.0, &other.1)
    }
}
impl Eq for Ranked<'_> {}
impl PartialOrd for Ranked<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Ranked<'_> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (&self.0, &self.1).cmp(&(&other.0, &other.1))
    }
}

impl workspace::Workspace {
    pub(crate) fn inspect_context(
        &self,
        paths: &[PathBuf],
        cancel: &CancellationToken,
    ) -> io::Result<Vec<Result<(PathBuf, Kind), String>>> {
        let mut values = Vec::with_capacity(paths.len());
        for batch in paths.chunks(INSPECT_BATCH) {
            let paths = batch
                .iter()
                .map(|path| self.remote_path(path))
                .collect::<io::Result<Vec<_>>>()?;
            let super::Reply::Context(Reply::Inspected(found)) = self
                .client
                .call_cancellable(super::Command::Context(Command::Inspect { paths }), cancel)?
            else {
                return Err(io::Error::other("Invalid file context inspection response"));
            };
            if found.len() != batch.len() {
                return Err(io::Error::other(
                    "Incomplete file context inspection response",
                ));
            }
            values.extend(
                found
                    .into_iter()
                    .map(|info| info.map(|info| (self.view_key(&info.identity), info.kind))),
            );
        }
        Ok(values)
    }

    pub(crate) fn file_context(
        &self,
        files: &[(PathBuf, PathBuf)],
        retain: bool,
        cancel: &CancellationToken,
    ) -> io::Result<(Vec<usize>, Option<std::sync::Arc<Source>>)> {
        let files = files
            .iter()
            .map(|(path, identity)| Ok((self.remote_path(path)?, self.remote_path(identity)?)))
            .collect::<io::Result<Vec<_>>>()?;
        let count = files.len();
        let super::Reply::Context(Reply::Files { source, sizes }) = self.client.call_cancellable(
            super::Command::Context(Command::Files { files, retain }),
            cancel,
        )?
        else {
            return Err(io::Error::other("Invalid file context response"));
        };
        // Accept ownership before validating metadata so every error path drops
        // the server allocation, just like a cancelled transport delivery.
        let source = source.map(|id| self.client.register_context(id));
        if sizes.len() != count
            || sizes.iter().any(|bytes| *bytes > FILE_LIMIT)
            || sizes
                .iter()
                .try_fold(0usize, |sum, bytes| sum.checked_add(*bytes))
                .is_none_or(|total| total > TOTAL_LIMIT)
            || source.is_some() != (retain && count != 0)
            || source.as_ref().is_some_and(|source| source.id == 0)
        {
            return Err(io::Error::other("Invalid prepared file context metadata"));
        }
        Ok((sizes, source))
    }
}
