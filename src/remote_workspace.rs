//! Adapter between the workbench's existing path-keyed view model and a remote
//! filesystem. These are opaque VIEW KEYS, not a disk mirror. Their NUL root
//! makes any accidentally unrouted native filesystem call fail on every OS.
//! Only RemotePath crosses the service boundary. No remote data is written to
//! a local path carrying the same spelling.
use super::*;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub(crate) struct Baseline {
    pub checkpoint: Arc<Checkpoint>,
    pub rope: ropey::Rope,
    pub encoding: &'static str,
}

#[derive(Debug)]
pub(crate) struct Validation {
    pub changed: bool,
    pub size: u64,
    pub modified: Option<std::time::SystemTime>,
}

#[derive(Clone)]
pub(crate) struct Workspace {
    pub connection: Option<Arc<launch::Process>>,
    pub(crate) recipe: Option<Arc<launch::Recipe>>,
    pub client: client::Client,
    pub view_root: PathBuf,
    /// A client-independent locator, derived once from the node's root URI.
    /// Native Windows spelling must never be joined as a POSIX relative path.
    display_root: Arc<str>,
    root_unicode: bool,
    connection_identity: Arc<[u8]>,
    recovery: Arc<Mutex<Option<RecoveryCheckpoints>>>,
    pub(super) git_sources: Arc<Mutex<cache::Cache<Arc<git::Sources>>>>,
    pub(super) blame_sources: Arc<Mutex<cache::Cache<Arc<blame::Base>>>>,
    pub(super) git_snapshots: Arc<Mutex<git::Snapshots>>,
    pub(super) git_source_state: Arc<Mutex<git::SourceState>>,
    directories: Arc<Mutex<cache::Cache<Arc<Vec<Entry>>>>>,
    pub(super) explorer_epoch: Arc<std::sync::atomic::AtomicU64>,
    disconnect_reported: Arc<std::sync::atomic::AtomicBool>,
    disconnect_requested: Arc<std::sync::atomic::AtomicBool>,
    pub(super) search_scope_cache: Arc<Mutex<super::search::ScopeCache>>,
}
impl std::fmt::Debug for Workspace {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RemoteWorkspace")
            .field("root", &self.client.hello().root)
            .finish_non_exhaustive()
    }
}
pub(crate) struct Saved {
    pub rope: ropey::Rope,
    pub transformed_lines: Option<(usize, usize)>,
    pub size: u64,
    pub checkpoint: Arc<Checkpoint>,
    pub modified: Option<std::time::SystemTime>,
    pub disk_written: bool,
}

pub(crate) enum Location {
    Directory(PathBuf),
    File(crate::editor::LoadedTextFile),
}
pub(crate) fn is_view_key(path: &Path) -> bool {
    path.as_os_str().as_encoded_bytes().contains(&0)
}
pub(crate) fn display_name(path: &Path) -> Option<String> {
    if !is_view_key(path) {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    // Ordinary names can keep the editor's borrowed, allocation-free label.
    if !name.contains('%') {
        return None;
    }
    let bytes = uncomponent(name).ok()?;
    let mut display = String::with_capacity(bytes.len());
    for ch in String::from_utf8_lossy(&bytes).chars() {
        if ch.is_control() {
            display.extend(ch.escape_default());
        } else {
            display.push(ch);
        }
    }
    Some(display)
}
/// A human label for a hosted location, without retaining a connection in the
/// chat renderer. Identity and actions continue to use the original view key.
pub(crate) fn display_key(path: &Path) -> Option<String> {
    if !is_view_key(path) {
        return None;
    }
    let mut root = PathBuf::new();
    let mut marker = false;
    for part in path.components() {
        root.push(part.as_os_str());
        if marker {
            let value = key_path(&root, path).ok()?.display();
            let mut label = String::new();
            for ch in value.chars() {
                if ch.is_control() {
                    label.extend(ch.escape_default());
                } else {
                    label.push(ch);
                }
            }
            return Some(if label.is_empty() { ".".into() } else { label });
        }
        marker = part
            .as_os_str()
            .to_str()
            .is_some_and(|value| value.starts_with("\0vtcode-remote-"));
    }
    None
}
fn key_path(root: &Path, key: &Path) -> io::Result<RemotePath> {
    let relative = key
        .strip_prefix(root)
        .map_err(|_| io::Error::other("File does not belong to this remote workspace"))?;
    let mut path = RemotePath::default();
    let mut absolute = false;
    for part in relative.components() {
        let std::path::Component::Normal(part) = part else {
            return Err(io::Error::other("Invalid remote view key"));
        };
        if path.0.is_empty() && !absolute && part == "~host" {
            absolute = true;
            continue;
        }
        path = path.child(&uncomponent(
            part.to_str()
                .ok_or_else(|| io::Error::other("Invalid remote view key"))?,
        )?)?;
    }
    path.1 = absolute;
    Ok(path)
}
fn component(bytes: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(bytes) {
        let mut result = String::new();
        for ch in text.chars() {
            if ch.is_control() || "\\:/%<>\"|?*~".contains(ch) {
                for byte in ch.to_string().bytes() {
                    result.push_str(&format!("%{byte:02x}"));
                }
            } else {
                result.push(ch);
            }
        }
        return result;
    }
    let mut result = String::new();
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b" ._()-".contains(&b) {
            result.push(b as char);
        } else {
            result.push_str(&format!("%{b:02x}"));
        }
    }
    result
}
fn uncomponent(text: &str) -> io::Result<Vec<u8>> {
    let mut result = Vec::new();
    let mut bytes = text.as_bytes();
    while !bytes.is_empty() {
        if bytes[0] == b'%' {
            let escaped = bytes
                .get(1..3)
                .and_then(|b| std::str::from_utf8(b).ok())
                .ok_or_else(|| io::Error::other("Invalid remote view key"))?;
            result.push(u8::from_str_radix(escaped, 16).map_err(io::Error::other)?);
            bytes = &bytes[3..];
        } else {
            result.push(bytes[0]);
            bytes = &bytes[1..];
        }
    }
    Ok(result)
}
impl Workspace {
    #[cfg(test)]
    pub(crate) fn new(client: client::Client) -> Self {
        Self::with_connection_identity(client, &[])
    }
    pub(crate) fn with_connection_identity(client: client::Client, identity: &[u8]) -> Self {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        digest.update(identity);
        digest.update([0]);
        digest.update(client.hello().root_uri.as_bytes());
        let id = URL_SAFE_NO_PAD.encode(digest.finalize());
        let root_uri =
            url::Url::parse(&client.hello().root_uri).expect("validated service root URI");
        let decoded_root =
            super::lsp::uri_path(&root_uri).and_then(|bytes| String::from_utf8(bytes).ok());
        let root_unicode = decoded_root.is_some();
        let display_root = decoded_root
            .filter(|path| {
                root_uri.host_str().is_none()
                    && !path.chars().any(char::is_control)
                    && path.trim_end_matches('/').trim() == path.trim_end_matches('/')
            })
            .unwrap_or_else(|| root_uri.to_string());
        let display_root = Arc::from(display_root.trim_end_matches('/'));
        let name = root_uri
            .path()
            .rsplit('/')
            .find(|name| !name.is_empty())
            .unwrap_or("Remote");
        let name = uncomponent(name).unwrap_or_else(|_| b"Remote".to_vec());
        #[cfg(windows)]
        let prefix = "C:\\";
        #[cfg(not(windows))]
        let prefix = "/";
        let view_root =
            PathBuf::from(format!("{prefix}\0vtcode-remote-{id}")).join(component(&name));
        Self {
            client,
            view_root,
            display_root,
            root_unicode,
            connection_identity: Arc::from(identity),
            recovery: Arc::default(),
            directories: Arc::default(),
            explorer_epoch: Arc::default(),
            connection: None,
            recipe: None,
            disconnect_reported: Arc::default(),
            disconnect_requested: Arc::default(),
            search_scope_cache: Arc::default(),
            git_sources: Arc::default(),
            blame_sources: Arc::default(),
            git_snapshots: Arc::default(),
            git_source_state: Arc::default(),
        }
    }
    /// Resolve and validate on a worker. Each workspace keeps immutable request
    /// context, so responses for an old folder cannot be reinterpreted in a new one.
    pub(crate) fn open_workspace(&self, key: &Path) -> io::Result<Self> {
        let path = self.remote_path(key)?;
        let client = self.client.open_workspace(path)?;
        let mut workspace = Self::with_connection_identity(client, &self.connection_identity);
        workspace.connection = self.connection.clone();
        workspace.recipe = self.recipe.clone();
        Ok(workspace)
    }
    pub(crate) fn take_disconnect(&self) -> bool {
        let disconnected = !self.client.connected()
            && !self
                .disconnect_reported
                .swap(true, std::sync::atomic::Ordering::Relaxed);
        if disconnected && let Some(connection) = &self.connection { connection.terminate(); }
        disconnected
    }
    pub(crate) fn disconnect(&self) {
        self.disconnect_requested.store(true, std::sync::atomic::Ordering::Release);
        self.client.disconnect();
        if let Some(connection) = &self.connection { connection.terminate(); }
    }
    pub(crate) fn reconnect_allowed(&self) -> bool {
        self.recipe.is_some() && !self.disconnect_requested.load(std::sync::atomic::Ordering::Acquire)
    }
    pub(crate) fn diagnostics(&self) -> Option<super::diagnostics::Log> {
        self.recipe.as_ref().map(|recipe| recipe.diagnostics.clone()).or_else(|| self.client.diagnostic_log())
    }
    /// Resolve a new transport's initial root back to the current open folder.
    /// New scheduler allocations may choose a different host, but must expose
    /// the same requested folder before any buffers or tools can be retargeted.
    pub(crate) fn resume_on(&self, connected: Self) -> io::Result<Self> {
        let mut next = if connected.client.hello().root_uri == self.client.hello().root_uri {
            connected
        } else {
            let uri = url::Url::parse(&self.client.hello().root_uri).map_err(io::Error::other)?;
            let bytes = super::lsp::uri_path(&uri).ok_or_else(|| io::Error::other("Invalid reconnect folder"))?;
            // Directory URIs carry a trailing slash; RemotePath components do
            // not. Keep the filesystem root '/' valid too.
            let mut path = bytes.as_slice();
            while path.len() > 1 && path.ends_with(b"/") { path = &path[..path.len() - 1]; }
            connected.open_workspace(&connected.view_key(&RemotePath::absolute(path)?))?
        };
        if next.view_root != self.view_root { return Err(io::Error::other("Reconnected workspace identity changed; local buffers were not retargeted")); }
        next.recovery = self.recovery.clone();
        next.recipe = self.recipe.clone();
        Ok(next)
    }
    pub(crate) fn view_key(&self, path: &RemotePath) -> PathBuf {
        let mut key = self.view_root.clone();
        if path.1 {
            key.push("~host");
        }
        if !path.0.is_empty() {
            for part in path.0.split(|b| *b == b'/') {
                key.push(component(part));
            }
        }
        key
    }
    pub(crate) fn remote_path(&self, key: &Path) -> io::Result<RemotePath> {
        key_path(&self.view_root, key)
    }
    pub(crate) fn target_key(&self, text: &str) -> io::Result<PathBuf> {
        let text = text.trim();
        if text.is_empty() {
            return Err(io::Error::other("Enter a remote path"));
        }
        if text.starts_with("file://") {
            return self
                .file_uri_key(text)
                .ok_or_else(|| io::Error::other("Invalid remote file URL"));
        }
        let mut parts = Vec::new();
        for part in text.split('/') {
            match part {
                "" | "." => {}
                ".." => {
                    if parts.pop().is_none() && !text.starts_with('/') {
                        return Err(io::Error::other(
                            "Use an absolute remote path outside the workspace",
                        ));
                    }
                }
                _ => parts.push(part),
            }
        }
        let normalized = format!(
            "{}{}",
            if text.starts_with('/') { "/" } else { "" },
            parts.join("/")
        );
        let text = normalized.as_str();
        if text.trim_end_matches('/') == self.display_root.as_ref() {
            return Ok(self.view_root.clone());
        }
        let path = if text.starts_with('/') {
            let relative = text
                .strip_prefix(self.display_root.as_ref())
                .and_then(|tail| tail.strip_prefix('/'));
            match relative {
                Some(relative) => RemotePath::new(relative.as_bytes().to_vec())?,
                None => RemotePath::absolute(text.as_bytes())?,
            }
        } else {
            RemotePath::new(text.as_bytes().to_vec())?
        };
        Ok(self.view_key(&path))
    }
    pub(crate) fn parent_key(&self, key: &Path) -> io::Result<Option<PathBuf>> {
        let path = self.remote_path(key)?;
        if let Some(parent) = path.parent() {
            return Ok(Some(self.view_key(&parent)));
        }
        if path.1 {
            return Ok(None);
        }
        let uri = url::Url::parse(&self.client.hello().root_uri).map_err(io::Error::other)?;
        let mut bytes = super::lsp::uri_path(&uri)
            .ok_or_else(|| io::Error::other("Invalid remote root URI"))?;
        while bytes.len() > 1 && bytes.last() == Some(&b'/') {
            bytes.pop();
        }
        let absolute = RemotePath::absolute(&bytes)?;
        Ok(absolute.parent().map(|parent| self.view_key(&parent)))
    }
    pub(crate) fn display_path(&self, key: &Path) -> io::Result<String> {
        let path = self.remote_path(key)?;
        let text = std::str::from_utf8(&path.0).ok();
        if self.display_root.starts_with("file://")
            || text.is_none_or(|text| {
                text.trim() != text
                    || text.contains(['"', '\\'])
                    || text.chars().any(char::is_control)
            })
        {
            // Text fields must round-trip arbitrary names without trimming
            // filename whitespace, embedding controls, or replacing bytes.
            let uri = url::Url::from_file_path(key)
                .map_err(|_| io::Error::other("Invalid remote view key"))?;
            return self
                .lsp_uri(uri.as_str(), true)
                .ok_or_else(|| io::Error::other("Invalid remote file URL"));
        }
        Ok(if path.1 {
            path.display()
        } else {
            format!("{}/{}", self.display_root, path.display())
        })
    }
    pub(crate) fn initial_save_path(&self, suggested: Option<&str>) -> String {
        let path = RemotePath::new(
            suggested
                .filter(|name| !name.is_empty())
                .unwrap_or("untitled.txt")
                .as_bytes()
                .to_vec(),
        )
        .unwrap_or_else(|_| RemotePath::new(b"untitled.txt".to_vec()).unwrap());
        self.display_path(&self.view_key(&path))
            .unwrap_or_else(|_| "untitled.txt".into())
    }
    /// Agent context and copied paths use node-native spelling, not editable UI
    /// locators. Keep Windows separators on the node and POSIX backslashes as
    /// filename bytes; use a URL only when JSON cannot represent a native name.
    pub(crate) fn context_path(&self, key: &Path) -> io::Result<String> {
        let path = self.remote_path(key)?;
        let Ok(text) = std::str::from_utf8(&path.0) else {
            return self.display_path(key);
        };
        if !self.root_unicode {
            return self.display_path(key);
        }
        let root = &self.client.hello().root;
        if root.starts_with('/') {
            Ok(if path.is_absolute() {
                path.display()
            } else {
                format!("{}/{text}", root.trim_end_matches('/'))
            })
        } else {
            let text = text.replace('/', "\\");
            Ok(if path.is_absolute() {
                text
            } else {
                format!("{}\\{text}", root.trim_end_matches(['/', '\\']))
            })
        }
    }
    pub(crate) fn resolve_from_key(&self, folder: &Path, value: &str) -> io::Result<PathBuf> {
        if value.starts_with('/') || value.starts_with("file://") {
            return self.target_key(value);
        }
        let key_uri = url::Url::from_file_path(folder)
            .map_err(|_| io::Error::other("Invalid remote folder"))?;
        let uri = self
            .lsp_uri(key_uri.as_str(), true)
            .ok_or_else(|| io::Error::other("Invalid remote folder"))?;
        let mut base = url::Url::parse(&uri).map_err(io::Error::other)?;
        if !base.path().ends_with('/') {
            base.path_segments_mut()
                .map_err(|_| io::Error::other("Invalid remote folder"))?
                .push("");
        }
        let mut relative = String::new();
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
                relative.push(byte as char);
            } else {
                use std::fmt::Write;
                write!(&mut relative, "%{byte:02X}").unwrap();
            }
        }
        let uri = base.join(&relative).map_err(io::Error::other)?;
        self.file_uri_key(uri.as_str())
            .ok_or_else(|| io::Error::other("Invalid remote folder"))
    }
    pub(crate) fn load(&self, key: &Path) -> io::Result<crate::editor::LoadedTextFile> {
        self.load_progress(key, |_, _| {})
    }
    pub(crate) fn reload(
        &self,
        key: &Path,
        baseline: Option<&Baseline>,
    ) -> io::Result<crate::editor::LoadedTextFile> {
        let path = self.remote_path(key)?;
        let operation = self.client.file_operation("Reload document", &path);
        super::diagnostics::Operation::run(
            operation.as_ref(),
            || {
                let _permit = self.client.text_permit()?;
                let reply = self.client.call_file(Command::Reload {
                    path,
                    known: baseline.map(|old| old.checkpoint.revision.clone()),
                    read_changed: true,
                })?;
                if let Reply::Unchanged {
                    identity,
                    size,
                    modified,
                } = reply
                {
                    let old = baseline.ok_or_else(|| {
                        io::Error::other("Unchanged reply without a known baseline")
                    })?;
                    return Ok(crate::editor::LoadedTextFile {
                        rope: old.rope.clone(),
                        encoding: old.encoding,
                        file_size: size,
                        last_disk_modified: modified,
                        remote_checkpoint: Some(Arc::new(Checkpoint {
                            revision: old.checkpoint.revision.clone(),
                            conflicted: false,
                            identity: Some(identity),
                        })),
                    });
                }
                if let Some(operation) = &operation {
                    operation.stage("Decode document / receive text");
                }
                self.decode_opened(reply, None, |_, _| {})
            },
            |loaded| {
                format!(
                    "{} bytes; {} lines",
                    loaded.rope.len_bytes(),
                    loaded.rope.len_lines()
                )
            },
        )
    }
    pub(crate) fn validate_saved(&self, key: &Path, baseline: &Baseline) -> io::Result<Validation> {
        // A dirty editor needs only the verdict and metadata, never the file body.
        match self.client.call_file(Command::Reload { path: self.remote_path(key)?,
            known: Some(baseline.checkpoint.revision.clone()), read_changed: false })? {
            Reply::Unchanged { size, modified, .. } => Ok(Validation { changed: false, size, modified }),
            Reply::Changed { size, modified } => Ok(Validation { changed: true, size, modified }),
            _ => Err(io::Error::other("Invalid remote validation response")),
        }
    }
    pub(crate) fn resolve_aliases(
        &self,
        paths: &[PathBuf],
    ) -> io::Result<Vec<(PathBuf, Option<PathBuf>)>> {
        let mut resolved = Vec::with_capacity(paths.len());
        for batch in paths.chunks(256) {
            let paths = batch
                .iter()
                .map(|path| self.remote_path(path))
                .collect::<io::Result<Vec<_>>>()?;
            let Reply::CanonicalPaths(values) =
                self.client.call_file(Command::Canonicalize { paths })?
            else {
                return Err(io::Error::other("Invalid canonical path response"));
            };
            if values.len() != batch.len() {
                return Err(io::Error::other("Incomplete canonical path response"));
            }
            resolved.extend(
                batch.iter().cloned().zip(
                    values
                        .into_iter()
                        .map(|path| path.map(|path| self.view_key(&path))),
                ),
            );
        }
        Ok(resolved)
    }
    pub(crate) fn load_progress(
        &self,
        key: &Path,
        progress: impl FnMut(u64, u64),
    ) -> io::Result<crate::editor::LoadedTextFile> {
        let path = self.remote_path(key)?;
        let operation = self.client.file_operation("Open document", &path);
        super::diagnostics::Operation::run(
            operation.as_ref(),
            || {
                let _permit = self.client.text_permit()?;
                if let Some(operation) = &operation {
                    operation.stage("Request file");
                }
                let reply = self.client.call_file(Command::Open { path })?;
                if let Some(operation) = &operation {
                    operation.stage("Decode document / receive text");
                }
                self.decode_opened(reply, None, progress)
            },
            |loaded| {
                format!(
                    "{} bytes; {} lines",
                    loaded.rope.len_bytes(),
                    loaded.rope.len_lines()
                )
            },
        )
    }
    pub(crate) fn open_location(
        &self,
        key: &Path,
        cancel: &crate::background::CancellationToken,
        progress: impl FnMut(u64, u64),
    ) -> io::Result<Location> {
        let path = self.remote_path(key)?;
        let operation = self.client.file_operation("Open location", &path);
        super::diagnostics::Operation::run(
            operation.as_ref(),
            || {
                let _permit = self.client.text_permit_cancellable(Some(cancel))?;
                if let Some(operation) = &operation {
                    operation.stage("Request location");
                }
                let reply = self
                    .client
                    .call_file_cancellable(Command::OpenLocation { path }, cancel)?;
                if let Reply::Directory { identity } = reply {
                    if identity.is_absolute() {
                        return Err(io::Error::other(
                            "This directory is outside the current workspace.",
                        ));
                    }
                    return Ok(Location::Directory(self.view_key(&identity)));
                }
                if let Some(operation) = &operation {
                    operation.stage("Decode document / receive text");
                }
                self.decode_opened(reply, Some(cancel), progress)
                    .map(Location::File)
            },
            |location| match location {
                Location::File(loaded) => format!(
                    "{} bytes; {} lines",
                    loaded.rope.len_bytes(),
                    loaded.rope.len_lines()
                ),
                Location::Directory(_) => "Directory".into(),
            },
        )
    }
    fn decode_opened(
        &self,
        reply: Reply,
        cancel: Option<&crate::background::CancellationToken>,
        mut progress: impl FnMut(u64, u64),
    ) -> io::Result<crate::editor::LoadedTextFile> {
        let (rope, enc, revision, identity, size, modified) = match reply {
            Reply::Opened {
                text,
                encoding,
                revision,
                identity,
                size,
                modified,
            } => (
                ropey::Rope::from_str(&text),
                encoding,
                revision,
                identity,
                size,
                modified,
            ),
            Reply::Text(text::Reply::Opened {
                transfer,
                characters,
                bytes,
                encoding,
                revision,
                identity,
                size,
                modified,
            }) => {
                progress(0, bytes as u64);
                let rope = self.receive_text(transfer, characters, bytes, cancel, &mut progress)?;
                (rope, encoding, revision, identity, size, modified)
            }
            _ => return Err(io::Error::other("Invalid remote open response")),
        };
        let encoding = read_encoding(&enc)?;
        Ok(crate::editor::LoadedTextFile {
            rope,
            encoding,
            file_size: size,
            last_disk_modified: modified,
            remote_checkpoint: Some(Arc::new(Checkpoint {
                revision,
                conflicted: false,
                identity: Some(identity),
            })),
        })
    }
    pub(crate) fn set_recovery(&self, checkpoints: Option<RecoveryCheckpoints>) {
        *self.recovery.lock().unwrap() = checkpoints;
    }
    pub(crate) fn check_recovery(
        &self,
        key: &Path,
        loaded: &mut crate::editor::LoadedTextFile,
        restores: &[crate::session::SessionRestore],
    ) {
        let changed = restores
            .iter()
            .filter_map(|restore| restore.tab.pending_unsaved_content.as_ref())
            .any(|rope| !crate::rope_changes::equal(rope, &loaded.rope));
        if !changed {
            return;
        }
        let Some(current) = &loaded.remote_checkpoint else {
            return;
        };
        let checkpoints = self.recovery.lock().unwrap().clone();
        let previous = checkpoints
            .as_ref()
            .and_then(|checkpoints| checkpoints.get(key.to_string_lossy().as_ref()));
        if previous
            .is_none_or(|previous| previous.conflicted || previous.revision != current.revision)
        {
            loaded.remote_checkpoint = Some(Arc::new(Checkpoint {
                revision: current.revision.clone(),
                conflicted: true,
                identity: current.identity.clone(),
            }));
        }
    }
    pub(crate) fn save_progress(
        &self,
        request: &crate::editor::BackgroundSaveRequest,
        checkpoint: Option<(Arc<Checkpoint>, ropey::Rope, &'static str)>,
        known_metadata: Option<(u64, Option<std::time::SystemTime>)>,
        mut progress: impl FnMut(u64, u64),
    ) -> io::Result<Saved> {
        let path = self.remote_path(&request.path)?;
        let operation = self.client.file_operation("Save document", &path);
        super::diagnostics::Operation::run(
            operation.as_ref(),
            || {
                if let Some(operation) = &operation {
                    operation.stage("Prepare save / compute changes");
                }
                let base = request.expected_disk.as_ref().map(|expected| {
                    checkpoint
                        .as_ref()
                        .filter(|(_, rope, encoding)| {
                            *encoding == expected.encoding
                                && crate::rope_changes::equal(rope, &expected.rope)
                        })
                        .map_or_else(
                            || revision(&expected.rope, expected.encoding),
                            |(checkpoint, _, _)| checkpoint.revision.clone(),
                        )
                });
                let rope = crate::editor::prepare_save_rope(
                    &request.rope,
                    request.trim_trailing,
                    request.insert_final_newline,
                    request.line_ending == "CRLF",
                );
                if checkpoint
                    .as_ref()
                    .is_some_and(|(checkpoint, _, _)| checkpoint.conflicted)
                    && request.expected_disk.as_ref().is_some_and(|expected| {
                        expected.encoding != request.encoding
                            || !crate::rope_changes::equal(&expected.rope, &rope)
                    })
                {
                    return Err(crate::editor::save::conflict_message(
                "The remote file changed while disconnected. Your recovered edits are retained. Compare with disk before replacing it.".into(),
            ));
                }
                let transformed_lines = (!rope.is_instance(&request.rope)).then(|| {
                    let (old, new) =
                        crate::rope_changes::changed_lines(&request.rope, &rope, &|| false)
                            .unwrap();
                    (
                        old.start.min(new.start),
                        old.end.max(new.end).saturating_sub(1),
                    )
                });
                // No local changes means no write or validation RPC. Native
                // watcher/focus/fallback handling owns external changes; a
                // real edit still uses the node's exact saved-revision guard.
                if !request.fail_if_exists
                    && let Some((size, modified)) = known_metadata
                    && let Some(expected) = &request.expected_disk
                    && expected.encoding == request.encoding
                    && crate::rope_changes::equal(&expected.rope, &rope)
                    && let Some((checkpoint, previous, encoding)) = &checkpoint
                    && !checkpoint.conflicted && *encoding == expected.encoding
                    && crate::rope_changes::equal(previous, &expected.rope)
                {
                    return Ok(Saved { rope, transformed_lines, size, modified,
                        checkpoint: checkpoint.clone(), disk_written: false });
                }
                let old = request
                    .expected_disk
                    .as_ref()
                    .map_or_else(ropey::Rope::new, |old| old.rope.clone());
                let (old_range, new_range) = crate::rope_changes::changed_chars(&old, &rope);
                let inserted_bytes = rope.slice(new_range.clone()).len_bytes();
                if let Some(operation) = &operation {
                    operation.stage("Upload changes / await save");
                }
                let reply = if inserted_bytes <= text::INLINE_BYTES {
                    self.client.call_file(Command::Save {
                        path: path.clone(),
                        base,
                        encoding: request.encoding.into(),
                        change: Change {
                            start: old_range.start,
                            end: old_range.end,
                            text: rope.slice(new_range).to_string(),
                        },
                    })?
                } else {
                    progress(0, inserted_bytes as u64);
                    self.send_text_save(
                        path.clone(),
                        base,
                        old_range,
                        &rope,
                        new_range,
                        request.encoding,
                        progress,
                    )?
                };
                let Reply::Saved {
                    revision,
                    identity,
                    size,
                    modified,
                } = reply
                else {
                    return Err(io::Error::other("Invalid remote save response"));
                };
                if request.expected_disk.is_none() {
                    self.invalidate_directories();
                }
                Ok(Saved {
                    rope,
                    transformed_lines,
                    size,
                    modified,
                    disk_written: true,
                    checkpoint: Arc::new(Checkpoint {
                        revision,
                        conflicted: false,
                        identity: Some(identity),
                    }),
                })
            },
            |saved| {
                format!(
                    "{} bytes; {} lines; {}",
                    saved.size,
                    saved.rope.len_lines(),
                    if saved.disk_written { "saved remotely" } else { "unchanged; no remote request" },
                )
            },
        )
    }
    pub(crate) fn invalidate_directories(&self) {
        *self.directories.lock().unwrap() = Default::default();
        self.explorer_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    pub(crate) fn list(&self, path: &RemotePath) -> io::Result<Arc<Vec<Entry>>> {
        if let Some(entries) = self.directories.lock().unwrap().get(path).cloned() {
            return Ok(entries);
        }
        let Reply::Entries(entries) = self.client.call(Command::List { path: path.clone() })?
        else {
            return Err(io::Error::other("Invalid remote directory response"));
        };
        let bytes = entries
            .iter()
            .map(|entry| std::mem::size_of::<Entry>() + entry.path.0.len())
            .sum();
        let entries = Arc::new(entries);
        self.directories
            .lock()
            .unwrap()
            .insert(path.clone(), bytes, entries.clone());
        Ok(entries)
    }
}
