//! Native closed-file replacement shared by the workbench and headless node.
//! Open buffers stay local; a batch sends positions/options, never file bodies.
use crate::{app_search::SnapshotReplacer, background::CancellationToken};
use serde::{Deserialize, Serialize};
use std::{io, path::Path};

#[cfg(test)]
#[path = "workspace_replace_file_tests.rs"]
mod tests;

pub(crate) struct Snapshot {
    pub identity: u64,
    pub version: u64,
    pub rope: ropey::Rope,
    pub writable: bool,
}
pub(crate) struct Target {
    pub path: std::path::PathBuf,
    pub matches: Vec<(usize, usize, usize)>,
    pub snapshot: Option<Snapshot>,
    pub pending_save: bool,
}
pub(crate) struct Job {
    pub workspace_generation: u64,
    pub generation: u64,
    pub targets: Vec<Target>,
    pub options: Options,
    pub remote: Option<crate::remote::workspace::Workspace>,
}
impl Job {
    pub(crate) fn run(
        self,
        cancel: CancellationToken,
        sender: crate::background::BackgroundResultSender,
    ) {
        use crate::background::{BackgroundResult, OpenFileReplaceResult};
        let Self {
            workspace_generation,
            generation,
            targets,
            options,
            remote,
        } = self;
        // A closed-only remote operation does not compile the pattern locally.
        let local_program = std::cell::OnceCell::new();
        let mut opened = Vec::new();
        let mut closed = Vec::new();
        let mut open_failures = 0;
        let mut error = None;
        for target in targets {
            if cancel.is_cancelled() {
                break;
            }
            let Some(snapshot) = target.snapshot else {
                if target.pending_save {
                    open_failures += 1;
                } else {
                    closed.push(File {
                        path: target.path,
                        matches: target.matches,
                    });
                }
                continue;
            };
            if !snapshot.writable {
                open_failures += 1;
                continue;
            }
            let program = match local_program.get_or_init(|| SnapshotReplacer::workspace(&options))
            {
                Ok(program) => program,
                Err(detail) => {
                    error = Some(detail.clone());
                    break;
                }
            };
            let requested = target.matches.len();
            match program.prepare(snapshot.rope, target.matches, &cancel) {
                Ok((prepared, count)) if count == requested => {
                    if count > 0 {
                        opened.push(OpenFileReplaceResult {
                            path: target.path,
                            source_document_id: snapshot.identity,
                            source_content_version: snapshot.version,
                            prepared,
                            replacements: count,
                        });
                    }
                }
                Ok(_) => open_failures += 1,
                Err(detail) if detail.kind() == io::ErrorKind::Interrupted => break,
                Err(_) => open_failures += 1,
            }
        }
        // One notification for the entire local phase; never wait on node I/O
        // before presenting a completed local edit and its shared undo entry.
        let _ = sender.send(BackgroundResult::GlobalReplaceOpenPrepared {
            workspace_generation,
            generation,
            open_files: opened,
            failures: open_failures,
        });
        let mut outcome = Outcome::default();
        let mut candidates = rustc_hash::FxHashSet::default();
        if !cancel.is_cancelled() && error.is_none() {
            if let Some(remote) = remote {
                drop(local_program);
                let mut files = Vec::with_capacity(closed.len());
                for file in closed {
                    match remote.remote_path(&file.path) {
                        Ok(path) => {
                            files.push(File {
                                path,
                                matches: file.matches,
                            });
                            candidates.insert(file.path);
                        }
                        Err(_) => outcome.failures += 1,
                    }
                }
                if !files.is_empty() {
                    let response = remote.client.call_cancellable(
                        crate::remote::Command::Replace(crate::remote::search::Replace {
                            options,
                            files,
                        }),
                        &cancel,
                    );
                    match response {
                        Ok(crate::remote::Reply::Replaced(result)) => outcome.add(result),
                        Err(detail)
                            if detail.kind() == io::ErrorKind::Interrupted
                                && cancel.is_cancelled() =>
                        {
                            outcome.cancelled = true
                        }
                        Err(detail) => error = Some(detail.to_string()),
                        Ok(_) => error = Some("Invalid remote replacement reply".into()),
                    }
                }
            } else {
                for target in closed {
                    if cancel.is_cancelled() {
                        break;
                    }
                    let program =
                        match local_program.get_or_init(|| SnapshotReplacer::workspace(&options)) {
                            Ok(program) => program,
                            Err(detail) => {
                                error = Some(detail.clone());
                                break;
                            }
                        };
                    match file(&target.path, target.matches, program, &cancel) {
                        Ok((count, written)) => {
                            outcome.replacements += count;
                            outcome.files_changed += usize::from(count > 0);
                            outcome.written_files += usize::from(written);
                            if written {
                                candidates.insert(target.path);
                            }
                        }
                        Err(detail) if detail.kind() == io::ErrorKind::Interrupted => {
                            outcome.cancelled = true;
                            break;
                        }
                        Err(_) => outcome.failures += 1,
                    }
                }
            }
        }
        outcome.cancelled |= cancel.is_cancelled();
        let _ = sender.send(BackgroundResult::GlobalReplaceDone {
            workspace_generation,
            generation,
            outcome,
            candidates,
            error,
        });
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Options {
    pub query: String,
    pub replacement: String,
    pub case_sensitive: bool,
    pub regex: bool,
    pub whole_word: bool,
    pub preserve_case: bool,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct File<P> {
    pub path: P,
    pub matches: Vec<(usize, usize, usize)>,
}
#[derive(Default, Debug, Serialize, Deserialize)]
pub(crate) struct Outcome {
    pub replacements: usize,
    pub files_changed: usize,
    pub written_files: usize,
    pub failures: usize,
    pub cancelled: bool,
}
impl Outcome {
    pub(crate) fn add(&mut self, other: Self) {
        self.replacements = self.replacements.saturating_add(other.replacements);
        self.files_changed = self.files_changed.saturating_add(other.files_changed);
        self.written_files = self.written_files.saturating_add(other.written_files);
        self.failures = self.failures.saturating_add(other.failures);
        self.cancelled |= other.cancelled;
    }
}

/// Count and whether bytes were published. The latter avoids invalidating a
/// saved-checkpoint cache for a verified no-op replacement.
pub(crate) fn file(
    path: &Path,
    matches: Vec<(usize, usize, usize)>,
    program: &SnapshotReplacer<'_>,
    cancel: &CancellationToken,
) -> io::Result<(usize, bool)> {
    if cancel.is_cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    if matches.is_empty() {
        return Ok((0, false));
    }
    let requested = matches.len();
    let (source, metadata) = crate::fs_ops::open_document_source(path, false)?;
    if metadata.permissions().readonly() {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let loaded = crate::editor::decode_text_source(source, metadata, path, Some(cancel))?;
    if matches!(loaded.encoding, "Binary" | "UTF-8 (lossy)") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Workspace replacement requires lossless text",
        ));
    }
    let expected_disk = Some(crate::editor::DiskSaveBaseline {
        rope: loaded.rope.clone(),
        encoding: loaded.encoding,
    });
    let (prepared, count) = program.prepare(loaded.rope, matches, cancel)?;
    // A file is one operation. Do not publish a partial replacement if any of
    // the displayed ranges became stale, invalid or overlapping.
    if count != requested {
        return Err(io::Error::other(
            "Search matches changed before replacement",
        ));
    }
    if cancel.is_cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    let saved = crate::editor::write_background_save(&crate::editor::BackgroundSaveRequest {
        prepared_options: None,
        source_document_id: 0,
        path: path.to_owned(),
        rope: prepared.rope().clone(),
        encoding: loaded.encoding,
        // The prepared text already has its intended newlines. Avoid changing
        // unrelated LF rows in a mixed LF/CRLF file through save normalization.
        line_ending: "LF",
        trim_trailing: false,
        insert_final_newline: false,
        source_content_version: 0,
        fail_if_exists: false,
        expected_disk,
    })?;
    Ok((count, saved.disk_written))
}

#[derive(Default)]
pub(crate) struct State {
    pub cancel: Option<CancellationToken>,
    pub open: Outcome,
}
impl State {
    pub(crate) fn cancel(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.open = Outcome::default();
    }
}
impl Drop for State {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
    }
}
