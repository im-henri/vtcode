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
        if !path.is_absolute()
            || path.components().any(|part| {
                part == Component::ParentDir
                    || matches!(part, Component::Normal(name) if has_windows_stream_separator(name))
            })
        {
            return Err(p::Error::new(
                p::ErrorCode::InvalidParams.into(),
                "ACP paths must be absolute and use ordinary file and directory names without parent traversals",
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
        self.read_inner(request, buffer, || {})
    }

    fn read_inner(
        &self,
        request: &p::ReadTextFileRequest,
        buffer: Option<&str>,
        after_resolve: impl FnOnce(),
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
            after_resolve();
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
        let before = match crate::file_source::open_canonical_regular_file(&path) {
            Ok((mut file, metadata)) => {
                if metadata.permissions().readonly() {
                    return Err(failure("The file is read-only"));
                }
                let text = read_text_contents(&mut file)?;
                Some(WriteBaseline { file, metadata, text })
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(io_error(error)),
        };
        let parent = path
            .parent()
            .ok_or_else(|| failure("Cannot write the workspace root"))?;
        if before.is_none() {
            std::fs::create_dir_all(parent).map_err(io_error)?;
        }
        let validate_destination = || {
            if self.resolve(&request.path)? == path {
                Ok(())
            } else {
                Err(failure("The destination changed during the ACP write"))
            }
        };
        validate_destination()?;
        let temporary = if before
            .as_ref()
            .is_none_or(|baseline| baseline.text != request.content)
        {
            let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
            if let Some(before) = &before {
                temporary
                    .as_file()
                    .set_permissions(before.metadata.permissions())
                    .map_err(io_error)?;
            }
            temporary
                .write_all(request.content.as_bytes())
                .map_err(io_error)?;
            temporary.as_file().sync_all().map_err(io_error)?;
            Some(temporary)
        } else {
            None
        };
        if cancelled() {
            return Err(p::Error::request_cancelled());
        }
        validate_destination()?;
        let before = match before {
            Some(baseline) => {
                verify_write_baseline(&path, &baseline)?;
                // Windows publication needs the original entry's handle closed.
                drop(baseline.file);
                Some(baseline.text)
            }
            None => None,
        };
        if let Some(temporary) = temporary {
            if before.is_some() {
                temporary.persist(&path)
            } else {
                temporary.persist_noclobber(&path)
            }
            .map_err(|error| io_error(error.error))?;
        }
        Ok(FileEdit {
            session_id: request.session_id.clone(),
            path,
            before: before.unwrap_or_default(),
            after: request.content.clone(),
        })
    }
}

/// A colon in a Windows name can select a stream or replace the base when
/// that component is appended on its own.
pub(super) fn has_windows_stream_separator(name: &std::ffi::OsStr) -> bool {
    cfg!(windows) && name.as_encoded_bytes().contains(&b':')
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
    let (file, _) = crate::file_source::open_canonical_regular_file(path).map_err(io_error)?;
    read_text_contents(file)
}

fn read_text_contents(file: impl Read) -> Result<String, p::Error> {
    let mut bytes = Vec::new();
    file.take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err(failure("ACP text files are limited to 8 MiB"));
    }
    String::from_utf8(bytes).map_err(|_| failure("ACP text files must be UTF-8"))
}

struct WriteBaseline {
    // The original entry stays open so its identifier cannot be reused.
    file: std::fs::File,
    metadata: std::fs::Metadata,
    text: String,
}

fn verify_write_baseline(path: &Path, before: &WriteBaseline) -> Result<(), p::Error> {
    use crate::file_source::file_identity;

    let (mut file, metadata) = crate::file_source::open_canonical_regular_file(path)
        .map_err(io_error)?;
    let changed = || failure("The file changed during the ACP write");
    if metadata.len() != before.text.len() as u64
        || metadata.permissions() != before.metadata.permissions()
        || file_identity(&file, &metadata) != file_identity(&before.file, &before.metadata)
    {
        return Err(changed());
    }
    // Compare exact bytes in bounded chunks. A timestamp or file-size check
    // alone misses edits that preserve both.
    let mut buffer = [0; 16 * 1024];
    for expected in before.text.as_bytes().chunks(buffer.len()) {
        let actual = &mut buffer[..expected.len()];
        file.read_exact(actual).map_err(io_error)?;
        if actual != expected {
            return Err(changed());
        }
    }
    if file.read(&mut buffer[..1]).map_err(io_error)? != 0 {
        return Err(changed());
    }
    Ok(())
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
    p::Error::new(p::ErrorCode::InternalError.into(), message)
}
pub(super) fn io_error(error: std::io::Error) -> p::Error {
    let code = match error.kind() {
        std::io::ErrorKind::NotFound => p::ErrorCode::ResourceNotFound,
        _ => p::ErrorCode::InternalError,
    };
    p::Error::new(code.into(), error.to_string())
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
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello\n");
        let update = p::WriteTextFileRequest::new("test", &path, "updated\n");
        let edit = workspace.write(&update).unwrap();
        assert_eq!(edit.before, "hello\n");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "updated\n");
        assert!(workspace.resolve(&dir.path().join("../outside")).is_err());
        assert!(workspace.resolve(Path::new("relative")).is_err());
        let outside = tempfile::tempdir().unwrap();
        assert!(workspace.resolve(&outside.path().join("no.txt")).is_err());
    }

    #[test]
    fn an_identical_agent_write_preserves_the_file_and_its_hard_links() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(directory.path()).unwrap();
        let path = directory.path().join("file.txt");
        let alias = directory.path().join("alias.txt");
        let content = "unchanged\r\n";
        std::fs::write(&path, content).unwrap();
        std::fs::hard_link(&path, &alias).unwrap();
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(time))
            .unwrap();

        let edit = workspace
            .write(&p::WriteTextFileRequest::new("test", &path, content))
            .unwrap();

        assert_eq!(edit.before, content);
        assert_eq!(edit.after, content);
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), time);
        std::fs::write(alias, "updated through the other name").unwrap();
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "updated through the other name"
        );
    }

    #[test]
    fn workspace_file_errors_report_their_protocol_codes() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let request = p::ReadTextFileRequest::new("test", dir.path().join("missing.txt"));
        let error = workspace.read(&request, None).unwrap_err();
        assert_eq!(error.code, p::ErrorCode::ResourceNotFound);

        let error = workspace.resolve(Path::new("relative")).unwrap_err();
        assert_eq!(error.code, p::ErrorCode::InvalidParams);

        let error = workspace.resolve(dir.path().parent().unwrap()).unwrap_err();
        assert_eq!(error.code, p::ErrorCode::InternalError);
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn agent_read_rejects_an_ancestor_redirected_after_resolution() {
        let directory = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(directory.path()).unwrap();
        let parent = directory.path().join("sub");
        let moved = directory.path().join("moved");
        let path = parent.join("file.txt");
        std::fs::create_dir(&parent).unwrap();
        std::fs::write(&path, "workspace text").unwrap();
        std::fs::write(outside.path().join("file.txt"), "external text").unwrap();
        let request = p::ReadTextFileRequest::new("test", &path);

        let point_parent_at = |target: &Path| {
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &parent).unwrap();
            #[cfg(windows)]
            assert!(
                std::process::Command::new("cmd.exe")
                    .args(["/d", "/c", "mklink", "/J"])
                    .arg(&parent)
                    .arg(target)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        let remove_link = || {
            #[cfg(unix)]
            std::fs::remove_file(&parent).unwrap();
            #[cfg(windows)]
            std::fs::remove_dir(&parent).unwrap();
        };
        let result = workspace.read_inner(&request, None, || {
            std::fs::rename(&parent, &moved).unwrap();
            point_parent_at(outside.path());
        });
        remove_link();

        assert!(
            result.is_err(),
            "read accessed a substituted ancestor: {result:?}"
        );
        point_parent_at(&moved);
        let response = workspace.read(&request, None);
        remove_link();
        assert_eq!(response.unwrap().content, "workspace text");
    }

    fn preserve_external_write(original: Option<&str>) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let path = dir.path().join("file.txt");
        if let Some(original) = original {
            std::fs::write(&path, original).unwrap();
        }
        let request = p::WriteTextFileRequest::new("test", &path, "agent replacement\n");
        let checked = std::cell::Cell::new(false);
        let result = workspace.write_cancellable(&request, &|| {
            if checked.replace(true) {
                std::fs::write(&path, "external\n").unwrap();
            }
            false
        });
        assert_eq!(std::fs::read_to_string(path).unwrap(), "external\n");
        assert!(result.is_err());
    }

    #[test]
    fn agent_write_preserves_files_created_during_publication() {
        preserve_external_write(None);
    }

    #[test]
    fn agent_write_preserves_same_size_external_edits_during_publication() {
        preserve_external_write(Some("original\n"));
    }

    #[test]
    fn agent_write_refuses_a_replaced_file_with_identical_contents() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let path = dir.path().join("file.txt");
        let retained = dir.path().join("original.txt");
        std::fs::write(&path, "original\n").unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let request = p::WriteTextFileRequest::new("test", &path, "agent replacement\n");
        let staged = std::cell::Cell::new(false);

        let result = workspace.write_cancellable(&request, &|| {
            if staged.replace(true) {
                std::fs::rename(&path, &retained).unwrap();
                std::fs::write(&path, "original\n").unwrap();
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_modified(modified))
                    .unwrap();
            }
            false
        });

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original\n");
        assert_eq!(std::fs::read_to_string(&retained).unwrap(), "original\n");
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn agent_write_preserves_both_files_after_link_retargeting() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let original = dir.path().join("original.txt");
        let other = dir.path().join("other.txt");
        let link = dir.path().join("link.txt");
        let new_link = dir.path().join("new-link.txt");
        std::fs::write(&original, "original\n").unwrap();
        std::fs::write(&other, "other\n").unwrap();
        std::os::unix::fs::symlink(&original, &link).unwrap();
        std::os::unix::fs::symlink(&other, &new_link).unwrap();
        let request = p::WriteTextFileRequest::new("test", &link, "agent replacement\n");
        let checked = std::cell::Cell::new(false);
        let result = workspace.write_cancellable(&request, &|| {
            if checked.replace(true) {
                std::fs::rename(&new_link, &link).unwrap();
            }
            false
        });
        assert_eq!(std::fs::read_to_string(original).unwrap(), "original\n");
        assert_eq!(std::fs::read_to_string(other).unwrap(), "other\n");
        assert!(result.is_err());
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
