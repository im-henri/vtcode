//! Metadata-only spans shared by file and transport workers. Chunk traffic,
//! terminal input, repaint-driven reads and unchanged polling are deliberately
//! excluded. Descriptions are built once, never from Debug/serialized payloads.
use super::{Attempt, Body, Log};
use crate::remote::{self as remote, Command, RemotePath, Reply};
use std::{
    io,
    sync::{Arc, Mutex, atomic::Ordering},
    time::Instant,
};

pub(super) struct Description {
    pub id: u64,
    pub scope: &'static str,
    pub name: &'static str,
    pub target: String,
}
#[derive(Clone)]
pub(crate) struct Operation(Arc<Span>);
struct Span {
    trace: Attempt,
    description: Arc<Description>,
    start: Instant,
    state: Mutex<State>,
}
struct State {
    phase: &'static str,
    finished: bool,
    progress: Option<(Instant, u64)>,
}
impl Span {
    fn record(&self, phase: &'static str, detail: String) {
        self.trace.log.append(
            self.trace.id,
            "Operation",
            Body::Operation {
                description: self.description.clone(),
                phase,
                duration: self.start.elapsed(),
                detail,
            },
        );
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        let state = self
            .state
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        if !state.finished {
            let phase = state.phase;
            self.record(
                "Stopped",
                format!("No result accepted; last phase: {phase}"),
            );
        }
    }
}
impl Operation {
    pub(crate) fn result<T, E: std::fmt::Display>(
        &self,
        result: &Result<T, E>,
        summary: impl FnOnce(&T) -> String,
    ) {
        match result {
            Ok(value) => self.completed(summary(value)),
            Err(error) => self.failed(&error.to_string()),
        }
    }
    fn output(&self, phase: &'static str, text: &str) {
        // Reuse already returned subprocess output, on its worker. Retain at
        // most the log's own byte budget, and sanitize bounded UTF-8 chunks.
        let mut start = text.len().saturating_sub(super::MAX_TEXT);
        while !text.is_char_boundary(start) { start += 1; }
        if start > 0 {
            self.0.record(phase, format!("[{start} earlier output bytes omitted]"));
        }
        struct Output<'a> { operation: &'a Operation, phase: &'static str, text: String }
        impl Output<'_> {
            fn flush(&mut self) {
                if !self.text.trim().is_empty() {
                    self.operation.0.record(self.phase, std::mem::take(&mut self.text));
                } else { self.text.clear(); }
            }
        }
        impl vte::Perform for Output<'_> {
            fn print(&mut self, character: char) {
                if self.text.len() + character.len_utf8() > 1800 { self.flush(); }
                self.text.push(character);
            }
            fn execute(&mut self, byte: u8) {
                if matches!(byte, b'\n' | b'\r' | b'\t') {
                    self.print(if byte == b'\t' { ' ' } else { '\n' });
                }
            }
        }
        // One parser spans all record boundaries, including split ANSI/OSC
        // sequences and UTF-8. No escape sequence can reach the Output reader.
        let mut output = Output { operation: self, phase, text: String::new() };
        let mut parser = vte::Parser::new();
        for &byte in text[start..].as_bytes() { parser.advance(&mut output, byte); }
        output.flush();
    }
    pub(crate) fn network(&self, result: &remote::git::NetworkResult) {
        let last_phase = {
            let mut state = self.0.state.lock().unwrap();
            if state.finished { return; }
            state.finished = true;
            state.phase
        };
        self.output("stdout", &result.stdout);
        self.output("stderr", &result.stderr);
        if result.success {
            self.0.record("Completed", "Git command exited successfully".into());
        } else {
            self.0.record("Failed", format!("last phase: {last_phase}; Git command exited unsuccessfully; see stderr above"));
        }
    }
    fn new(
        trace: Attempt,
        id: u64,
        scope: &'static str,
        name: &'static str,
        target: String,
    ) -> Self {
        let operation = Self(Arc::new(Span {
            trace,
            description: Arc::new(Description {
                id,
                scope,
                name,
                target,
            }),
            start: Instant::now(),
            state: Mutex::new(State {
                phase: "Queued",
                finished: false,
                progress: None,
            }),
        }));
        operation.0.record("Queued", String::new());
        operation
    }
    pub(crate) fn stage(&self, phase: &'static str) {
        let mut state = self.0.state.lock().unwrap();
        if state.finished || state.phase == phase {
            return;
        }
        state.phase = phase;
        self.0.record(phase, String::new());
    }
    pub(crate) fn progress(&self, completed: u64, total: u64) {
        let mut state = self.0.state.lock().unwrap();
        if state.finished
            || state.progress.is_some_and(|(last, previous)| {
                completed == previous || (completed != total && last.elapsed().as_secs() < 5)
            })
        {
            return;
        }
        state.progress = Some((Instant::now(), completed));
        state.phase = "Transferring";
        self.0.record(
            "Transferring",
            format!(
                "{completed}/{total} bytes ({}%)",
                completed
                    .saturating_mul(100)
                    .checked_div(total)
                    .unwrap_or(100)
            ),
        );
    }
    fn finish(&self, phase: &'static str, detail: String) {
        let mut state = self.0.state.lock().unwrap();
        if state.finished {
            return;
        }
        state.finished = true;
        let detail = if matches!(phase, "Completed" | "Sent") {
            detail
        } else {
            format!(
                "last phase: {}{}{}",
                state.phase,
                if detail.is_empty() { "" } else { "; " },
                detail
            )
        };
        self.0.record(phase, detail);
    }
    pub(crate) fn error(&self, error: &io::Error) {
        self.finish(
            match error.kind() {
                io::ErrorKind::Interrupted => "Cancelled",
                io::ErrorKind::TimedOut => "Timed out",
                io::ErrorKind::WouldBlock => "Busy",
                _ => "Failed",
            },
            super::plain(error.to_string().as_bytes()),
        );
    }
    pub(crate) fn failed(&self, error: &str) {
        self.finish("Failed", super::plain(error.as_bytes()));
    }
    pub(crate) fn completed(&self, detail: String) {
        self.finish("Completed", detail);
    }
    pub(crate) fn notification_sent(&self) {
        self.finish(
            "Sent",
            "Notification; no remote acknowledgement is expected".into(),
        );
    }
    pub(crate) fn reply(&self, result: &Result<Reply, remote::Failure>) {
        match result {
            Ok(Reply::GitNetwork(result)) => self.network(result),
            Err(error) => self.finish(
                match error.kind {
                    remote::FailureKind::SaveConflict => "Save conflict",
                    remote::FailureKind::Interrupted => "Cancelled",
                    remote::FailureKind::Timeout => "Timed out",
                    _ => "Failed",
                },
                super::plain(error.message.as_bytes()),
            ),
            Ok(reply) => self.completed(match reply {
                Reply::Opened { size, .. } | Reply::Saved { size, .. } => format!("{size} bytes"),
                Reply::Text(
                    remote::text::Reply::Opened {
                        bytes, transfer, ..
                    }
                    | remote::text::Reply::Source {
                        bytes, transfer, ..
                    },
                ) => format!("{bytes} bytes; transfer #{transfer} follows"),
                Reply::Text(remote::text::Reply::Upload { transfer }) => {
                    format!("transfer #{transfer} ready for upload")
                }
                Reply::Entries(entries) => format!("{} entries", entries.len()),
                Reply::GitStatus { discovery, scans } => format!(
                    "{} repositories, {} updated snapshots",
                    discovery.roots.len(),
                    scans.len()
                ),
                Reply::GitBranches(branches) => format!("{} branches", branches.len()),
                Reply::Search { hits, truncated } => {
                    format!("{} matches; truncated={truncated}", hits.len())
                }
                Reply::Files { files, .. } => files.as_ref().map_or_else(
                    || "Unchanged".into(),
                    |files| format!("{} files", files.len()),
                ),
                Reply::Formatted(text) => format!("{} result bytes", text.len()),
                Reply::Changed { size, .. } => format!("Changed on disk; {size} bytes"),
                Reply::Unchanged { size, .. } => format!("Unchanged on disk; {size} bytes"),
                Reply::Directory { .. } => "Directory".into(),
                _ => String::new(),
            }),
        }
    }
    /// Include local preparation and complete streaming in file timings, not
    /// just the header RPC. The closure keeps every early error/cancel paired.
    pub(crate) fn run<T>(
        operation: Option<&Self>,
        work: impl FnOnce() -> io::Result<T>,
        summary: impl FnOnce(&T) -> String,
    ) -> io::Result<T> {
        let result = work();
        if let Some(operation) = operation {
            match &result {
                Ok(value) => operation.completed(summary(value)),
                Err(error) => operation.error(error),
            }
        }
        result
    }
}

impl Log {
    pub(crate) fn operation(&self, scope: &'static str, name: &'static str, target: &str) -> Operation {
        Operation::new(
            Attempt { log: self.clone(), id: 0 },
            self.0.next_operation.fetch_add(1, Ordering::Relaxed),
            scope, name, field(target),
        )
    }
    pub(crate) fn git_request(&self, attempt: u64, id: u64, command: &Command, workspace: &str) -> Option<Operation> {
        Attempt { log: self.clone(), id: attempt }.request(id, command, workspace)
    }
}

impl Attempt {
    pub(crate) fn workspace_changes(&self, batch: &remote::watch::Batch) {
        // One record per already-debounced native notification, not per OS
        // event. No extra messages, node work or file metadata reads.
        self.log.append(self.id, "Filesystem notification", Body::WorkspaceChanges {
            workspace: field(&batch.root_uri), paths: batch.states.len(),
            structural: batch.structural, git: batch.git, rescan: batch.rescan, active: batch.active,
        });
    }
    pub(crate) fn notification(&self, command: &Command) -> Option<Operation> {
        // Called by the existing output worker, never the input/UI pump. File
        // closes are fire-and-forget; do not pretend the node acknowledged one.
        if !matches!(
            command,
            Command::Close { .. } | Command::Watch | Command::Shutdown
        ) {
            return None;
        }
        let (name, target) = describe(command)?;
        Some(Operation::new(
            self.clone(),
            self.log.0.next_operation.fetch_add(1, Ordering::Relaxed),
            "notification",
            name,
            target,
        ))
    }
    pub(crate) fn transfer(&self, name: &'static str, transfer: u64, bytes: usize) -> Operation {
        Operation::new(
            self.clone(),
            transfer,
            "transfer",
            name,
            format!("{bytes} bytes"),
        )
    }
    pub(crate) fn operation(&self, name: &'static str, path: &RemotePath) -> Operation {
        Operation::new(
            self.clone(),
            self.log.0.next_operation.fetch_add(1, Ordering::Relaxed),
            "file",
            name,
            path_label(path),
        )
    }
    pub(crate) fn request(&self, id: u64, command: &Command, workspace: &str) -> Option<Operation> {
        let (name, target) = describe(command)?;
        let target = if target.is_empty() {
            format!("[workspace {}]", field(workspace))
        } else {
            format!("{target} [workspace {}]", field(workspace))
        };
        Some(Operation::new(self.clone(), id, "request", name, target))
    }
}

// Bounded, one-line, unambiguous metadata, including unusual Unix path names.
// Escaping is only done once per operation, never per frame/chunk/keypress.
fn field(value: &str) -> String {
    let mut result = String::new();
    let mut characters = value.chars();
    for character in characters.by_ref().take(512) {
        push_character(&mut result, character);
    }
    if characters.next().is_some() {
        result.push('…');
    }
    result
}
fn push_character(result: &mut String, character: char) {
    if character.is_control() || matches!(character, '\\' | '"') {
        result.extend(character.escape_default());
    } else {
        result.push(character);
    }
}
fn path_label(path: &RemotePath) -> String {
    use std::fmt::Write;
    if path.0.is_empty() && !path.1 {
        return "\".\"".into();
    }
    let bytes = &path.0[..path.0.len().min(2048)];
    let mut label = if path.1 {
        "\"/".to_owned()
    } else {
        "\"".to_owned()
    };
    let mut count = 0;
    let mut truncated = bytes.len() < path.0.len();
    'parts: for part in bytes.utf8_chunks() {
        for character in part.valid().chars() {
            if count == 512 {
                truncated = true;
                break 'parts;
            }
            push_character(&mut label, character);
            count += 1;
        }
        for byte in part.invalid() {
            if count == 512 {
                truncated = true;
                break 'parts;
            }
            let _ = write!(label, "\\x{byte:02x}");
            count += 1;
        }
    }
    if truncated {
        label.push('…');
    }
    label.push('"');
    label
}
fn describe(command: &Command) -> Option<(&'static str, String)> {
    use remote::{agent, binary, context, fs, text};
    let path = |name, path: &RemotePath| Some((name, path_label(path)));
    match command {
        Command::OpenWorkspace { path: p } => path("Open workspace", p),
        Command::List { path: p } => path("Read directory", p),
        Command::FilesystemRoots => Some(("List file systems", String::new())),
        Command::Open { path: p } => path("Read file", p),
        Command::OpenLocation { path: p } => path("Open location", p),
        Command::Save {
            path: p,
            change,
            encoding,
            ..
        } => Some((
            "Save file",
            format!(
                "{}; replace characters {}..{} with {} bytes; encoding={}",
                path_label(p),
                change.start,
                change.end,
                change.text.len(),
                field(encoding)
            ),
        )),
        Command::Reload {
            path: p,
            read_changed,
            ..
        } => path(
            if *read_changed {
                "Reload file"
            } else {
                "Check saved revision"
            },
            p,
        ),
        Command::Close { path: p } => path("Close file", p),
        Command::CreateDirectory { path: p } => path("Create directory", p),
        Command::Remove { path: p, .. } => path("Remove path", p),
        Command::Rename { from, to } => Some((
            "Rename path",
            format!("{} → {}", path_label(from), path_label(to)),
        )),
        Command::Filesystem(fs::Task::Create { path: p, directory }) => path(
            if *directory {
                "Create directory"
            } else {
                "Create file"
            },
            p,
        ),
        Command::Filesystem(fs::Task::Rename { from, to }) => Some((
            "Rename path",
            format!("{} → {}", path_label(from), path_label(to)),
        )),
        Command::Filesystem(fs::Task::Paste {
            sources,
            directory,
            cut,
        }) => Some((
            if *cut { "Move paths" } else { "Copy paths" },
            format!("{} paths → {}", sources.len(), path_label(directory)),
        )),
        Command::Filesystem(fs::Task::Delete { paths }) => Some((
            "Delete paths",
            format!(
                "{} paths; first={}",
                paths.len(),
                paths.first().map_or_else(String::new, path_label)
            ),
        )),
        Command::Text(text::Command::BeginSave { path: p, bytes, .. }) => Some((
            "Begin streamed save",
            format!("{}; {bytes} upload bytes", path_label(p)),
        )),
        Command::Text(text::Command::Finish { transfer }) => {
            Some(("Commit streamed save", format!("transfer #{transfer}")))
        }
        Command::Binary(binary::Command::Open { path: p }) => path("Open binary file", p),
        Command::Binary(binary::Command::Save {
            parts, destination, ..
        }) => Some((
            "Save binary file",
            format!(
                "{} pieces; {}",
                parts.len(),
                destination
                    .as_ref()
                    .map_or_else(|| "original path".into(), |(p, _)| path_label(p))
            ),
        )),
        Command::Explorer(_) => Some(("Refresh Explorer", String::new())),
        Command::GitStatus { known, include_untracked, .. } => Some(("Git status",
            format!("untracked={include_untracked}; discover={}", known.is_none()))),
        Command::GitAction { repo, task } => {
            use crate::git_tasks::Task;
            path(
                match task {
                    Task::Stage(_) => "Git stage files",
                    Task::Unstage(_) => "Git unstage files",
                    Task::Discard(_) => "Git discard files",
                    Task::StageAll => "Git stage all",
                    Task::UnstageAll => "Git unstage all",
                    Task::Init => "Git initialize",
                    Task::TrustRepository => "Git trust repository",
                    Task::Amend { .. } => "Git amend",
                    Task::Stash { .. } => "Git stash",
                    Task::StashPop => "Git stash pop",
                    Task::CreateBranch { .. } => "Git create branch",
                    Task::CheckoutLocal { .. } | Task::CheckoutRemote { .. } => {
                        "Git checkout branch"
                    }
                    Task::Commit { .. } => "Git commit",
                },
                repo,
            )
        }
        Command::GitBranches { repo } => path("Git branches", repo),
        Command::GitNetwork { repo, push, args } => path(
            if *push {
                "Git push"
            } else if args.len() == 3 && args[0] == "fetch" && args[1] == "--all" && args[2] == "--prune" {
                "Git fetch --all --prune"
            } else if args.first().is_some_and(|arg| arg == "fetch") {
                "Git fetch"
            } else if args.first().is_some_and(|arg| arg == "pull") {
                "Git pull"
            } else {
                "Git network command"
            },
            repo,
        ),
        Command::GitSources { file, .. } | Command::GitDiffSources { file, .. } => path("Git diff sources", file),
        Command::HunkSources { file, .. } => path("Read hunk sources", file),
        Command::HunkAction(action) => path(
            match action.kind {
                remote::hunk::Kind::Stage => "Stage hunk",
                remote::hunk::Kind::Unstage => "Unstage hunk",
                remote::hunk::Kind::Discard => "Discard hunk",
            },
            &action.file,
        ),
        Command::CommitDetails { repo, .. } => path("Read commit details", repo),
        Command::CommitDiff { file, .. } => Some(("Read commit diff", field(&file.path))),
        Command::Format { path: p, text, .. } => Some((
            "Format file",
            format!("{}; {} input bytes", path_label(p), text.len()),
        )),
        Command::FormatSource { path: p, .. } => path("Format streamed file", p),
        Command::Blame { file, .. } | Command::BlameHistory { file, .. } => {
            path("Read Git blame", file)
        }
        Command::HistoryStart { repository, .. } => path("Read Git history", repository),
        Command::HistoryNext { history } => Some(("Request Git history page", format!("history #{history}"))),
        Command::Files {
            known_version: None,
            ..
        } => Some(("Enumerate workspace files", String::new())),
        Command::Search(_) => Some(("Search workspace", String::new())),
        Command::Replace(_) => Some(("Replace in workspace", String::new())),
        Command::Spawn {
            process,
            cwd,
            terminal,
            ..
        } => Some((
            if terminal.is_some() {
                "Start terminal"
            } else {
                "Start process"
            },
            format!("process #{process}; cwd={}", path_label(cwd)),
        )),
        Command::Agent(agent::Command::Start { agent, .. }) => {
            Some(("Start agent", format!("agent #{agent}")))
        }
        Command::Agent(agent::Command::File { agent, write, .. }) => Some((
            if *write {
                "Agent file write"
            } else {
                "Agent file read"
            },
            format!("agent #{agent}"),
        )),
        Command::Context(context::Command::Files { files, .. }) => {
            Some(("Prepare file context", format!("{} files", files.len())))
        }
        Command::Rollback(_) => Some(("Agent checkpoint", String::new())),
        Command::Tasks => Some(("Read tasks", String::new())),
        Command::Watch => Some(("Watch workspace", String::new())),
        Command::Shutdown => Some(("Stop remote service", String::new())),
        // Never record input or per-chunk/per-frame/polling traffic. Failures
        // for these still appear in the transport error and outer file span.
        _ => None,
    }
}
