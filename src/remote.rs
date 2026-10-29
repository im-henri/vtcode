//! Local editing with a remote workspace service. This module has no terminal
//! rendering or keyboard protocol: an opened document is an ordinary local Rope.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::{
    io,
    path::{Path, PathBuf},
};
#[path = "remote_agent.rs"]
pub(crate) mod agent;

#[path = "remote_binary.rs"]
pub(crate) mod binary;
#[path = "remote_blame.rs"]
pub(crate) mod blame;
#[path = "remote_cache.rs"]
mod cache;
#[path = "remote_client.rs"]
pub(crate) mod client;
#[path = "remote_context.rs"]
pub(crate) mod context;
#[path = "remote_diagnostics.rs"]
pub(crate) mod diagnostics;
#[cfg(test)]
#[path = "remote_core_workflow_tests.rs"]
mod core_workflow_tests;
#[path = "remote_explorer.rs"]
pub(crate) mod explorer;
#[path = "remote_fs.rs"]
pub(crate) mod fs;
#[path = "remote_git.rs"]
pub(crate) mod git;
#[path = "remote_history.rs"]
pub(crate) mod history;
#[path = "remote_hunk.rs"]
pub(crate) mod hunk;
#[path = "remote_launch.rs"]
pub(crate) mod launch;
#[path = "remote_lsp.rs"]
pub(crate) mod lsp;
#[path = "remote_observability.rs"]
pub(crate) mod observability;
#[path = "remote_process.rs"]
pub(crate) mod process;
#[path = "remote_rollback.rs"]
pub(crate) mod rollback;
#[path = "remote_search.rs"]
pub(crate) mod search;
#[path = "remote_service.rs"]
pub(crate) mod service;
#[cfg(unix)]
#[path = "remote_signal.rs"]
mod signal;
#[path = "remote_text.rs"]
pub(crate) mod text;
#[path = "remote_watch.rs"]
pub(crate) mod watch;
#[path = "remote_wire.rs"]
pub(crate) mod wire;
#[path = "remote_workspace.rs"]
pub(crate) mod workspace;

// Repository access failures and repository-specific trust are part of Git snapshots/actions.
pub(crate) const VERSION: u32 = 26;
/// Per-message/queue memory budget, not a file size limit. Larger documents use
/// remote_text's bounded pipeline instead of a document-sized JSON allocation.
pub(crate) const MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;

/// Opaque workspace-relative POSIX bytes, never a native client PathBuf. Keeping
/// separators out of components also preserves Linux names containing `\` or
/// `:` when the client is Windows. Root is the empty byte sequence.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct RemotePath(Vec<u8>, bool);

impl Serialize for RemotePath {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Directory/status batches contain thousands of short paths. Encode
        // these directly into a small stack buffer instead of allocating one
        // temporary String per name. Long paths stream through serde_json's
        // collect_str implementation without a path-sized temporary either.
        let mut encoded = [0; 256];
        encoded[0] = b'/';
        let prefix = usize::from(self.1);
        if let Ok(size) = URL_SAFE_NO_PAD.encode_slice(&self.0, &mut encoded[prefix..]) {
            return serializer.serialize_str(
                std::str::from_utf8(&encoded[..prefix + size]).expect("base64 is ASCII"),
            );
        }
        struct Encoded<'a>(&'a RemotePath);
        impl std::fmt::Display for Encoded<'_> {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                if self.0.1 {
                    formatter.write_str("/")?;
                }
                std::fmt::Display::fmt(
                    &base64::display::Base64Display::new(&self.0.0, &URL_SAFE_NO_PAD),
                    formatter,
                )
            }
        }
        serializer.collect_str(&Encoded(self))
    }
}
impl<'de> Deserialize<'de> for RemotePath {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct PathVisitor;
        impl<'de> serde::de::Visitor<'de> for PathVisitor {
            type Value = RemotePath;
            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a base64 remote path")
            }
            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                let absolute = value.starts_with('/');
                let encoded = value.strip_prefix('/').unwrap_or(value);
                if encoded.len() > base64::encoded_len(RemotePath::MAX_BYTES, false).unwrap() {
                    return Err(E::custom("Remote workspace path is too long"));
                }
                let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(E::custom)?;
                let mut path = RemotePath::new(bytes).map_err(E::custom)?;
                path.1 = absolute;
                Ok(path)
            }
        }
        // JSON frame slices provide a borrowed string; reader/escaped/value
        // deserializers still work through visit_str, without an extra copy.
        deserializer.deserialize_str(PathVisitor)
    }
}
impl RemotePath {
    const MAX_BYTES: usize = 32 * 1024;
    pub(crate) fn new(bytes: Vec<u8>) -> io::Result<Self> {
        if bytes.len() > Self::MAX_BYTES
            || bytes.contains(&0)
            || bytes.starts_with(b"/")
            || (!bytes.is_empty()
                && bytes
                    .split(|b| *b == b'/')
                    .any(|p| p.is_empty() || p == b"." || p == b".."))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invalid remote workspace path",
            ));
        }
        Ok(Self(bytes, false))
    }
    pub(crate) fn absolute(bytes: &[u8]) -> io::Result<Self> {
        let bytes = bytes
            .strip_prefix(b"/")
            .ok_or_else(|| io::Error::other("Remote absolute path must begin with /"))?;
        let mut path = Self::new(bytes.to_vec())?;
        path.1 = true;
        Ok(path)
    }
    pub(crate) fn child(&self, name: &[u8]) -> io::Result<Self> {
        if name.is_empty() || name.contains(&b'/') {
            return Err(io::Error::other("Invalid remote filename"));
        }
        self.join_relative(name)
    }
    pub(crate) fn join_relative(&self, relative: &[u8]) -> io::Result<Self> {
        let size = self
            .0
            .len()
            .saturating_add(usize::from(!self.0.is_empty()))
            .saturating_add(relative.len());
        if relative.is_empty() || relative.starts_with(b"/") || size > Self::MAX_BYTES {
            return Err(io::Error::other("Invalid relative remote path"));
        }
        let mut bytes = Vec::with_capacity(size);
        bytes.extend_from_slice(&self.0);
        if !bytes.is_empty() {
            bytes.push(b'/');
        }
        bytes.extend_from_slice(relative);
        let mut path = Self::new(bytes)?;
        path.1 = self.1;
        Ok(path)
    }
    pub(crate) fn display(&self) -> String {
        format!(
            "{}{}",
            if self.1 { "/" } else { "" },
            String::from_utf8_lossy(&self.0)
        )
    }
    pub(crate) fn relative_to(&self, base: &Self) -> io::Result<Self> {
        if self.1 != base.1 {
            return Err(io::Error::other("Git path is outside the repository"));
        }
        let bytes = if base.0.is_empty() {
            self.0.as_slice()
        } else {
            self.0
                .strip_prefix(base.0.as_slice())
                .and_then(|tail| tail.strip_prefix(b"/"))
                .ok_or_else(|| io::Error::other("Git path is outside the repository"))?
        };
        if bytes.is_empty() {
            return Err(io::Error::other("Choose a file inside the repository"));
        }
        Self::new(bytes.to_vec())
    }
    pub(crate) fn into_native_relative(self) -> io::Result<PathBuf> {
        if self.1 || self.0.is_empty() {
            return Err(io::Error::other("Git paths must be repository-relative"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            Ok(std::ffi::OsString::from_vec(self.0).into())
        }
        #[cfg(not(unix))]
        {
            self.resolve(Path::new(""))
        }
    }
    pub(crate) fn name(&self) -> String {
        String::from_utf8_lossy(self.0.rsplit(|b| *b == b'/').next().unwrap_or_default())
            .into_owned()
    }
    pub(crate) fn file_name_bytes(&self) -> Option<&[u8]> {
        self.0
            .rsplit(|byte| *byte == b'/')
            .next()
            .filter(|name| !name.is_empty())
    }
    pub(crate) fn is_absolute(&self) -> bool {
        self.1
    }
    pub(crate) fn parent(&self) -> Option<Self> {
        if self.0.is_empty() {
            None
        } else {
            Some(Self(
                self.0
                    .iter()
                    .rposition(|b| *b == b'/')
                    .map_or_else(Vec::new, |p| self.0[..p].to_vec()),
                self.1,
            ))
        }
    }
    /// Agent-only conversion. Do not expose a client-native path accessor.
    fn resolve(&self, root: &Path) -> io::Result<PathBuf> {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Ok(if self.1 { Path::new("/") } else { root }
                .join(std::ffi::OsStr::from_bytes(&self.0)))
        }
        #[cfg(windows)]
        {
            // A Windows service cannot represent POSIX component spellings with
            // Windows separators or prefixes. Reject rather than reinterpret.
            let text = std::str::from_utf8(&self.0).map_err(io::Error::other)?;
            if self.1 {
                let bytes = text.as_bytes();
                if bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
                    return Ok(PathBuf::from(format!("\\\\?\\{text}\\")));
                }
                if bytes.len() < 3
                    || !bytes[0].is_ascii_alphabetic()
                    || bytes[1] != b':'
                    || bytes[2] != b'/'
                    || text[2..].contains(['\\', ':'])
                {
                    return Err(io::Error::other(
                        "This absolute path cannot be represented on the Windows headless service",
                    ));
                }
                // Match canonical roots, including long paths, without a
                // filesystem lookup for every decoded remote location.
                return Ok(PathBuf::from(format!(r"\\?\{}", text.replace('/', r"\"))));
            }
            if text.contains(['\\', ':']) {
                return Err(io::Error::other("Unsupported Windows filename"));
            }
            Ok(root.join(text))
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Hello {
    pub version: u32,
    #[serde(default)]
    pub app_version: String,
    pub root: String,
    /// Canonical, lossless URI supplied by the agent. Display text may contain
    /// replacement characters for a Unix directory with non-UTF-8 bytes.
    pub root_uri: String,
    pub session: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub path: RemotePath,
    pub is_dir: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Revision(pub [u8; 32]);
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Checkpoint {
    pub revision: Revision,
    pub conflicted: bool,
    /// Node-side canonical identity, shared with the document and its recovery
    /// record. This never requires a filesystem lookup on the input thread.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<RemotePath>,
}
pub(crate) type RecoveryCheckpoints =
    std::sync::Arc<std::collections::BTreeMap<String, std::sync::Arc<Checkpoint>>>;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Request {
    pub id: u64,
    /// Immutable request context. Old in-flight work keeps its original root
    /// when the workbench opens another folder on this connection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    pub command: Command,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Command {
    PerformanceSnapshot,
    Context(context::Command),
    Agent(agent::Command),
    Rollback(rollback::Command),
    Text(text::Command),
    Binary(binary::Command),
    Canonicalize {
        paths: Vec<RemotePath>,
    },
    OpenWorkspace {
        path: RemotePath,
    },
    List {
        path: RemotePath,
    },
    FilesystemRoots,
    Open {
        path: RemotePath,
    },
    /// Validate bytes against the client's saved revision. Dirty models need
    /// only the verdict; clean models may receive changed text in this reply.
    Reload { path: RemotePath, known: Option<Revision>, read_changed: bool },
    /// Resolve an arbitrary link and open its file in the same transaction.
    OpenLocation {
        path: RemotePath,
    },
    /// Change offsets count Unicode scalar values, matching Ropey, not bytes or
    /// UTF-16. The base revision identifies the exact saved checkpoint.
    Save {
        path: RemotePath,
        base: Option<Revision>,
        change: Change,
        encoding: String,
    },
    Watch,
    Inspect {
        paths: Vec<RemotePath>,
    },
    Close {
        path: RemotePath,
    },
    CreateDirectory {
        path: RemotePath,
    },
    Rename {
        from: RemotePath,
        to: RemotePath,
    },
    Remove {
        path: RemotePath,
        directory: bool,
    },
    Spawn {
        process: u64,
        program: String,
        args: Vec<String>,
        cwd: RemotePath,
        terminal: Option<(u16, u16)>,
        /// Older peers omit this and retain the original 64 KiB window.
        #[serde(default)]
        output_window: usize,
    },
    ProcessInput {
        process: u64,
        data: String,
    },
    ProcessCredit {
        process: u64,
        bytes: usize,
    },
    ProcessResize {
        process: u64,
        width: u16,
        height: u16,
    },
    ProcessKill {
        process: u64,
    },
    GitStatus {
        known: Option<git::Discovery>,
        parent_open: bool,
        closed: Vec<RemotePath>,
        include_untracked: bool,
        changed: Option<Vec<RemotePath>>,
    },
    GitAction {
        repo: RemotePath,
        task: crate::git_tasks::Task<RemotePath>,
    },
    GitSources {
        file: RemotePath,
        repository: Option<RemotePath>,
        staged: bool,
    },
    GitDiffSources {
        file: RemotePath,
        repository: Option<RemotePath>,
        staged: bool,
        known: Option<crate::git_diff_sources::Revisions>,
    },
    GitBranches {
        repo: RemotePath,
    },
    GitNetwork {
        repo: RemotePath,
        args: Vec<String>,
        push: bool,
    },
    CommitDetails {
        repo: RemotePath,
        hash: String,
    },
    CommitDiff {
        repo: RemotePath,
        hash: String,
        file: crate::commit_details::CommitFile,
        limit: u64,
    },
    HunkSources {
        file: RemotePath,
        repository: Option<RemotePath>,
        staged: bool,
    },
    HunkAction(hunk::Action),
    Format {
        path: RemotePath,
        config: crate::formatters::Config,
        text: String,
        lines: Option<(usize, usize)>,
    },
    FormatSource {
        transfer: u64,
        path: RemotePath,
        config: crate::formatters::Config,
        lines: Option<(usize, usize)>,
    },
    Blame {
        file: RemotePath,
        revision: Option<blame::Revision>,
    },
    BlameHistory {
        file: RemotePath,
        commit: blame::Commit,
        source: Option<blame::Revision>,
        line: usize,
        limit: u64,
    },
    BlameSnapshot {
        revision: blame::Revision,
        line: usize,
        limit: u64,
    },
    BlameLineSnapshot {
        file: RemotePath,
        commit: blame::Commit,
        source: Option<blame::Revision>,
        line: usize,
        limit: u64,
    },
    Explorer(explorer::Query),
    Tasks,
    HistoryStart {
        history: u64,
        repository: RemotePath,
    },
    HistoryNext {
        history: u64,
    },
    HistoryClose {
        history: u64,
    },
    Files {
        generation: u64,
        known_version: Option<u64>,
        #[serde(default)]
        excludes: crate::settings::ExcludePatterns,
    },
    Search(search::Query),
    SearchScope(search::Scope),
    Replace(search::Replace),
    Filesystem(fs::Task),
    Cancel {
        request: u64,
    },
    /// Legacy protocol-20 request. Current clients never send idle probes.
    Heartbeat { nonce: u64 },
    Ping,
    Shutdown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Change {
    pub start: usize,
    pub end: usize,
    pub text: String,
}
impl Change {
    #[cfg(test)]
    pub(crate) fn between(old: &ropey::Rope, new: &ropey::Rope) -> Self {
        let (old_range, new_range) = crate::rope_changes::changed_chars(old, new);
        Self {
            start: old_range.start,
            end: old_range.end,
            text: new.slice(new_range).to_string(),
        }
    }
    pub(crate) fn apply(&self, old: &ropey::Rope) -> io::Result<ropey::Rope> {
        if self.start > self.end || self.end > old.len_chars() {
            return Err(io::Error::other("Invalid document change"));
        }
        let mut text = old.clone();
        text.remove(self.start..self.end);
        text.insert(self.start, &self.text);
        Ok(text)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Response {
    pub id: u64,
    pub result: Result<Reply, Failure>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Failure {
    kind: FailureKind,
    message: String,
}
#[derive(Debug, Serialize, Deserialize)]
enum FailureKind {
    SaveConflict,
    Missing,
    NotDirectory,
    Exists,
    Permission,
    Invalid,
    Interrupted,
    Busy,
    Timeout,
    Disconnected,
    Other,
}
impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        use io::ErrorKind as K;
        let kind = if crate::editor::save::is_conflict(&error) {
            FailureKind::SaveConflict
        } else {
            match error.kind() {
                K::NotFound => FailureKind::Missing,
                K::NotADirectory => FailureKind::NotDirectory,
                K::AlreadyExists => FailureKind::Exists,
                K::PermissionDenied => FailureKind::Permission,
                K::InvalidInput | K::InvalidData => FailureKind::Invalid,
                K::Interrupted => FailureKind::Interrupted,
                K::WouldBlock => FailureKind::Busy,
                K::TimedOut => FailureKind::Timeout,
                K::NotConnected | K::BrokenPipe | K::UnexpectedEof => FailureKind::Disconnected,
                _ => FailureKind::Other,
            }
        };
        Self {
            kind,
            message: error.to_string(),
        }
    }
}
impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self {
            kind: FailureKind::Other,
            message,
        }
    }
}
impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl std::error::Error for Failure {}
impl Failure {
    fn into_io(self) -> io::Error {
        use io::ErrorKind as K;
        if matches!(self.kind, FailureKind::SaveConflict) {
            return crate::editor::save::conflict_message(self.message);
        }
        let kind = match self.kind {
            FailureKind::SaveConflict => unreachable!(),
            FailureKind::Missing => K::NotFound,
            FailureKind::NotDirectory => K::NotADirectory,
            FailureKind::Exists => K::AlreadyExists,
            FailureKind::Permission => K::PermissionDenied,
            FailureKind::Invalid => K::InvalidInput,
            FailureKind::Interrupted => K::Interrupted,
            FailureKind::Busy => K::WouldBlock,
            FailureKind::Timeout => K::TimedOut,
            FailureKind::Disconnected => K::NotConnected,
            FailureKind::Other => K::Other,
        };
        io::Error::new(kind, self.message)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Reply {
    PerformanceSnapshot(observability::Snapshot),
    /// Retained for protocol compatibility with older clients.
    Heartbeat { nonce: u64 },
    Context(context::Reply),
    Agent(agent::Reply),
    AgentEvent(agent::Delivery),
    AgentEnd {
        agent: u64,
        error: String,
    },
    Rollback(rollback::Reply),
    Text(text::Reply),
    Binary(binary::Reply),
    Workspace {
        root: String,
        root_uri: String,
    },
    Entries(Vec<Entry>),
    FilesystemRoots(Vec<(String, RemotePath)>),
    CanonicalPaths(Vec<Option<RemotePath>>),
    Directory {
        identity: RemotePath,
    },
    Unchanged { identity: RemotePath, size: u64, modified: Option<std::time::SystemTime> },
    Changed { size: u64, modified: Option<std::time::SystemTime> },
    Opened {
        text: String,
        encoding: String,
        revision: Revision,
        identity: RemotePath,
        size: u64,
        modified: Option<std::time::SystemTime>,
    },
    Saved {
        revision: Revision,
        identity: RemotePath,
        size: u64,
        modified: Option<std::time::SystemTime>,
    },
    Done,
    ProcessData {
        process: u64,
        data: String,
    },
    ProcessEnd {
        process: u64,
        error: Option<String>,
    },
    ProcessInputConsumed {
        process: u64,
        bytes: usize,
    },
    GitStatus {
        discovery: git::Discovery,
        scans: Vec<git::Scan>,
    },
    GitAction(crate::background::GitActionOutcome),
    GitSources(git::Sources),
    GitDiffSources(crate::git_diff_sources::Sources),
    GitBranches(Vec<crate::app::GitBranchPickerItem>),
    GitNetwork(git::NetworkResult),
    CommitDetails(crate::commit_details::CommitData),
    CommitDiff(git::CommitDiff),
    HunkSources(hunk::Sources),
    Formatted(String),
    Blame(blame::Sources),
    BlameHistory(blame::History),
    BlameSnapshot(blame::Snapshot),
    Explorer(explorer::Tree),
    Tasks(Vec<crate::app::TaskDefinition>),
    Files {
        version: u64,
        files: Option<Vec<RemotePath>>,
    },
    Search {
        hits: Vec<search::Hit>,
        truncated: bool,
    },
    SearchProgress { request: u64, hits: Vec<search::Hit> },
    SearchScope { indices: Vec<usize> },
    Replaced(crate::workspace_replace::Outcome),
    Filesystem(fs::Outcome),
    DeletionProgress {
        request: u64,
        progress: crate::file_deletion::Progress,
    },
    WorkspaceChanges(watch::Batch),
    FileStates(Vec<watch::State>),
    HistoryPage {
        history: u64,
        page: history::Page,
    },
}

pub(crate) fn revision(rope: &ropey::Rope, encoding: &str) -> Revision {
    revision_with(rope, encoding, || false).unwrap()
}

pub(crate) fn revision_cancellable(
    rope: &ropey::Rope,
    encoding: &str,
    cancel: &crate::background::CancellationToken,
) -> io::Result<Revision> {
    revision_with(rope, encoding, || cancel.is_cancelled())
}

fn revision_with(
    rope: &ropey::Rope,
    encoding: &str,
    mut cancelled: impl FnMut() -> bool,
) -> io::Result<Revision> {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(encoding.as_bytes());
    hash.update([0]);
    for chunk in rope.chunks() {
        if cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        hash.update(chunk.as_bytes());
    }
    if cancelled() {
        return Err(io::ErrorKind::Interrupted.into());
    }
    Ok(Revision(hash.finalize().into()))
}

pub(crate) fn encoding(value: &str) -> io::Result<&'static str> {
    match value {
        "UTF-8" => Ok("UTF-8"),
        "UTF-8 with BOM" => Ok("UTF-8 with BOM"),
        "UTF-16 LE" => Ok("UTF-16 LE"),
        "UTF-16 BE" => Ok("UTF-16 BE"),
        _ => Err(io::Error::other(
            "Remote saving requires a lossless supported text encoding",
        )),
    }
}

pub(crate) fn read_encoding(value: &str) -> io::Result<&'static str> {
    match value {
        "Binary" => Ok("Binary"),
        "UTF-8 (lossy)" => Ok("UTF-8 (lossy)"),
        _ => encoding(value),
    }
}

#[cfg(test)]
#[path = "remote_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "remote_path_codec_tests.rs"]
mod path_codec_tests;

#[cfg(test)]
#[path = "remote_idle_tests.rs"]
mod idle_tests;

#[cfg(all(test, windows))]
#[path = "remote_ssh_tests.rs"]
mod ssh_tests;
