use super::protocol as p;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum FileRequest {
    Read(p::ReadTextFileRequest),
    Write(p::WriteTextFileRequest),
}

impl FileRequest {
    pub fn session_id(&self) -> &p::SessionId {
        match self {
            Self::Read(r) => &r.session_id,
            Self::Write(r) => &r.session_id,
        }
    }
    pub fn path(&self) -> &Path {
        match self {
            Self::Read(r) => &r.path,
            Self::Write(r) => &r.path,
        }
    }
    pub fn path_mut(&mut self) -> &mut PathBuf {
        match self {
            Self::Read(request) => &mut request.path,
            Self::Write(request) => &mut request.path,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileEdit {
    pub session_id: p::SessionId,
    pub path: PathBuf,
    pub before: String,
    pub after: String,
}

/// The client-provided filesystem is scoped to the workspace. Canonicalization
/// checks existing ancestors too, so a new file cannot escape through a symlink.
/// This is a host access policy, not an OS sandbox for the agent executable.
pub trait FileBackend: Send + Sync + std::fmt::Debug {
    fn for_request(
        &self,
        _id: p::RequestId,
        _ticket: u64,
    ) -> Option<std::sync::Arc<dyn FileBackend>> {
        None
    }
    /// Pure lookup on an already validated hosted path; no I/O on the UI.
    fn resolve(&self, path: &Path) -> Result<PathBuf, p::Error>;
    /// Batch alias validation on a background file worker. Returning None for
    /// an unavailable path never authorizes a write through that path.
    fn resolve_many(&self, paths: &[PathBuf]) -> Result<Vec<Option<PathBuf>>, p::Error> {
        Ok(paths.iter().map(|path| self.resolve(path).ok()).collect())
    }
    /// Hosts call read/write only on their file workers.
    fn read(
        &self,
        request: &p::ReadTextFileRequest,
        buffer: Option<&str>,
    ) -> Result<p::ReadTextFileResponse, p::Error>;
    fn write(&self, request: &p::WriteTextFileRequest) -> Result<FileEdit, p::Error>;
}

#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
    io_lock: std::sync::Arc<std::sync::Mutex<()>>,
    backend: Option<std::sync::Arc<dyn FileBackend>>,
}

impl Workspace {
    pub fn new(root: &Path) -> Result<Self, p::Error> {
        let root = root.canonicalize().map_err(io_error)?;
        if !root.is_dir() {
            return Err(failure("The ACP workspace must be a directory"));
        }
        Ok(Self {
            root,
            io_lock: Default::default(),
            backend: None,
        })
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn hosted(root: PathBuf, backend: std::sync::Arc<dyn FileBackend>) -> Self {
        Self {
            root,
            io_lock: Default::default(),
            backend: Some(backend),
        }
    }
    pub fn for_request(&self, id: p::RequestId, ticket: u64) -> Self {
        let mut workspace = self.clone();
        if let Some(backend) = self
            .backend
            .as_ref()
            .and_then(|backend| backend.for_request(id, ticket))
        {
            workspace.backend = Some(backend);
        }
        workspace
    }
    pub fn resolve_many(&self, paths: &[PathBuf]) -> Result<Vec<Option<PathBuf>>, p::Error> {
        if let Some(backend) = &self.backend {
            return backend.resolve_many(paths);
        }
        Ok(paths.iter().map(|path| self.resolve(path).ok()).collect())
    }

    /// ACP agents often interpolate cwd into shell scripts. Keep canonical
    /// paths internally, but don't expose Win32's verbatim device-path syntax.
    pub fn agent_root(&self) -> PathBuf {
        agent_path(&self.root)
    }

    pub fn resolve(&self, path: &Path) -> Result<PathBuf, p::Error> {
        if let Some(backend) = &self.backend {
            return backend.resolve(path);
        }
        if !path.is_absolute() || path.components().any(|c| c == Component::ParentDir) {
            return Err(failure(
                "ACP paths must be absolute and must not contain '..'",
            ));
        }
        let mut ancestor = path;
        let mut missing = Vec::new();
        while !ancestor.exists() {
            // A broken symlink is not a missing path that we may create through.
            if ancestor.symlink_metadata().is_ok() {
                return Err(failure("Broken symlink in ACP path"));
            }
            missing.push(
                ancestor
                    .file_name()
                    .ok_or_else(|| failure("Invalid ACP path"))?,
            );
            ancestor = ancestor
                .parent()
                .ok_or_else(|| failure("Invalid ACP path"))?;
        }
        let mut resolved = ancestor.canonicalize().map_err(io_error)?;
        if !resolved.starts_with(&self.root) {
            return Err(failure("ACP file access is limited to this workspace"));
        }
        for part in missing.into_iter().rev() {
            resolved.push(part);
        }
        Ok(resolved)
    }

    pub fn read(
        &self,
        request: &p::ReadTextFileRequest,
        buffer: Option<&str>,
    ) -> Result<p::ReadTextFileResponse, p::Error> {
        if let Some(backend) = &self.backend {
            return backend.read(request, buffer);
        }
        let _guard = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        let path = self.resolve(&request.path)?;
        let disk;
        let text = if let Some(buffer) = buffer {
            buffer
        } else {
            disk = read_text(&path)?;
            &disk
        };
        if text.len() > MAX_FILE_BYTES {
            return Err(failure("ACP text files are limited to 8 MiB"));
        }
        Ok(p::ReadTextFileResponse::new(line_range(
            text,
            request.line,
            request.limit,
        )?))
    }

    pub fn write(&self, request: &p::WriteTextFileRequest) -> Result<FileEdit, p::Error> {
        self.write_cancellable(request, &|| false)
    }
    pub fn write_cancellable(
        &self,
        request: &p::WriteTextFileRequest,
        cancelled: &impl Fn() -> bool,
    ) -> Result<FileEdit, p::Error> {
        if let Some(backend) = &self.backend {
            return backend.write(request);
        }
        let _guard = self.io_lock.lock().unwrap_or_else(|e| e.into_inner());
        if cancelled() {
            return Err(p::Error::request_cancelled());
        }
        let path = self.resolve(&request.path)?;
        if request.content.len() > MAX_FILE_BYTES {
            return Err(failure("ACP text files are limited to 8 MiB"));
        }
        let before = if path.exists() {
            read_text(&path)?
        } else {
            String::new()
        };
        let parent = path
            .parent()
            .ok_or_else(|| failure("Cannot write the workspace root"))?;
        std::fs::create_dir_all(parent).map_err(io_error)?;
        // Recheck after directory creation and use an adjacent temporary file so
        // failed writes never leave an existing file partially overwritten.
        let path = self.resolve(&request.path)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
        if let Ok(metadata) = path.metadata() {
            if metadata.permissions().readonly() {
                return Err(failure("The file is read-only"));
            }
            temporary
                .as_file()
                .set_permissions(metadata.permissions())
                .map_err(io_error)?;
        }
        temporary
            .write_all(request.content.as_bytes())
            .map_err(io_error)?;
        temporary.as_file().sync_all().map_err(io_error)?;
        if cancelled() {
            return Err(p::Error::request_cancelled());
        }
        temporary
            .persist(&path)
            .map_err(|error| io_error(error.error))?;
        Ok(FileEdit {
            session_id: request.session_id.clone(),
            path,
            before,
            after: request.content.clone(),
        })
    }
}

pub(super) fn agent_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    if let Some(Component::Prefix(prefix)) = path.components().next() {
        use std::path::Prefix;
        let mut normal = match prefix.kind() {
            Prefix::VerbatimDisk(drive) => PathBuf::from(format!("{}:\\", drive as char)),
            Prefix::VerbatimUNC(server, share) => {
                let mut root = PathBuf::from(r"\\");
                root.push(server);
                root.push(share);
                root
            }
            _ => return path.into(),
        };
        for component in path.components().skip(1) {
            if component != Component::RootDir {
                normal.push(component.as_os_str());
            }
        }
        return normal;
    }
    path.into()
}

fn read_text(path: &Path) -> Result<String, p::Error> {
    let (file, _) = crate::file_source::open_document_source(path, false).map_err(io_error)?;
    let mut bytes = Vec::new();
    file.take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(failure("ACP text files are limited to 8 MiB"));
    }
    String::from_utf8(bytes).map_err(|_| failure("ACP text files must be UTF-8"))
}

pub fn line_range(text: &str, line: Option<u32>, limit: Option<u32>) -> Result<String, p::Error> {
    if line == Some(0) {
        return Err(p::Error::invalid_params().data("line is 1-based"));
    }
    if line.is_none() && limit.is_none() {
        return Ok(text.to_owned());
    }
    // Preserve CRLF and the final newline. str::lines + join would change both.
    Ok(text
        .split_inclusive('\n')
        .skip(line.unwrap_or(1) as usize - 1)
        .take(limit.map_or(usize::MAX, |n| n as usize))
        .collect())
}

pub(super) fn failure(message: impl Into<String>) -> p::Error {
    p::Error::new(-32000, message)
}
pub(super) fn io_error(error: std::io::Error) -> p::Error {
    failure(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn fifo_read_and_write_reject_without_a_peer_and_leave_the_workspace_usable() {
        use std::{
            os::unix::{
                ffi::OsStrExt,
                fs::{FileTypeExt, OpenOptionsExt},
            },
            sync::mpsc,
            time::Duration,
        };
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(directory.path()).unwrap();
        let mut results = Vec::new();
        for write in [false, true] {
            let path = directory
                .path()
                .join(if write { "write.fifo" } else { "read.fifo" });
            let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            let worker_workspace = workspace.clone();
            let worker_path = path.clone();
            let (completed, ready) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                let result = if write {
                    worker_workspace
                        .write(&p::WriteTextFileRequest::new(
                            "fifo",
                            &worker_path,
                            "must not replace a pipe",
                        ))
                        .map(|_| ())
                } else {
                    worker_workspace
                        .read(&p::ReadTextFileRequest::new("fifo", &worker_path), None)
                        .map(|_| ())
                };
                let _ = completed.send(result);
            });
            let early = ready.recv_timeout(Duration::from_secs(3));
            let blocked = early.is_err();
            // Unblock the old implementation before joining on a regression.
            // Opening the writer nonblocking also keeps failure cleanup bounded.
            if blocked {
                let end = std::time::Instant::now() + Duration::from_secs(5);
                loop {
                    match std::fs::OpenOptions::new()
                        .write(true)
                        .custom_flags(libc::O_NONBLOCK)
                        .open(&path)
                    {
                        Ok(peer) => {
                            drop(peer);
                            break;
                        }
                        Err(error)
                            if error.raw_os_error() == Some(libc::ENXIO)
                                && std::time::Instant::now() < end =>
                        {
                            std::thread::yield_now()
                        }
                        Err(error) => panic!("cannot unblock regressed FIFO reader: {error}"),
                    }
                }
            }
            let result =
                early.unwrap_or_else(|_| ready.recv_timeout(Duration::from_secs(5)).unwrap());
            worker.join().unwrap();
            assert!(
                result.is_err(),
                "a pipe must never be read or replaced as a text file"
            );
            assert!(path.symlink_metadata().unwrap().file_type().is_fifo());
            eprintln!(
                "ACP FIFO {} blocked without a peer: {blocked}",
                if write { "write" } else { "read" }
            );
            results.push(blocked);
        }
        let regular = directory.path().join("regular.txt");
        workspace
            .write(&p::WriteTextFileRequest::new(
                "after-fifo",
                &regular,
                "safe\n",
            ))
            .unwrap();
        assert_eq!(std::fs::read_to_string(regular).unwrap(), "safe\n");
        assert_eq!(
            results,
            [false, false],
            "nonregular sources must be rejected before any blocking read/open"
        );
    }
    #[cfg(windows)]
    #[test]
    fn agent_cwd_is_shell_compatible_without_weakening_canonical_checks() {
        assert_eq!(
            agent_path(Path::new(r"\\?\C:\folder\世界")),
            PathBuf::from(r"C:\folder\世界")
        );
        assert_eq!(
            agent_path(Path::new(r"\\?\UNC\server\share\folder")),
            PathBuf::from(r"\\server\share\folder")
        );
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        assert!(
            !workspace
                .agent_root()
                .to_string_lossy()
                .starts_with(r"\\?\")
        );
        assert_eq!(
            workspace.resolve(&workspace.agent_root()).unwrap(),
            workspace.root()
        );
        let outside = tempfile::tempdir().unwrap();
        assert!(workspace.resolve(&agent_path(outside.path())).is_err());
        let result = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; Set-Location -LiteralPath $env:VTCODE_ACP_CWD_TEST; if (!(Test-Path -LiteralPath '.')) { exit 1 }"])
            .env("VTCODE_ACP_CWD_TEST", workspace.agent_root())
            .output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    #[test]
    fn ranges_preserve_line_endings_and_empty_limits() {
        assert_eq!(
            line_range("one\r\ntwo\r\nthree", Some(2), Some(1)).unwrap(),
            "two\r\n"
        );
        assert_eq!(line_range("one\ntwo", None, Some(0)).unwrap(), "");
        assert!(line_range("one", Some(0), None).is_err());
        assert_eq!(line_range("one", Some(3), None).unwrap(), "");
    }
    #[test]
    fn workspace_writes_are_atomic_and_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let path = dir.path().join("new/sub/file.txt");
        let request = p::WriteTextFileRequest::new("test", &path, "hello\n");
        let edit = workspace.write(&request).unwrap();
        assert_eq!(edit.before, "");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "hello\n");
        assert!(workspace.resolve(&dir.path().join("../outside")).is_err());
        assert!(workspace.resolve(Path::new("relative")).is_err());
        let outside = tempfile::tempdir().unwrap();
        assert!(workspace.resolve(&outside.path().join("no.txt")).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn symlink_ancestors_cannot_escape_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        assert!(
            workspace
                .resolve(&dir.path().join("escape/new/file"))
                .is_err()
        );
    }
}
