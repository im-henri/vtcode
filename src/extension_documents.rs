//! Binary buffers for custom editors. Files are read in ranges, edits use a
//! piece table, and saves replace the original only after conflict checks.
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

const HISTORY_LIMIT: usize = 1_000;
const IO_CHUNK: usize = 64 * 1024;

#[derive(Debug)]
struct SourceFile {
    file: Option<File>,
    remote: Option<Arc<crate::remote::binary::Source>>,
    stamp: DiskStamp,
    cursor: Option<u64>,
}

impl SourceFile {
    fn open(path: &Path) -> Result<Arc<Mutex<Self>>, String> {
        Self::open_metadata(path).map(|(source, _)| source).map_err(io_error)
    }
    fn open_metadata(path: &Path) -> std::io::Result<(Arc<Mutex<Self>>, std::fs::Metadata)> {
        let (file, metadata) = crate::fs_ops::open_document_source(path, false)?;
        let stamp = DiskStamp::from_metadata(&metadata);
        Ok((Arc::new(Mutex::new(Self { file: Some(file), remote: None, stamp, cursor: Some(0) })), metadata))
    }

    fn read(&mut self, at: u64, out: &mut [u8]) -> Result<(), String> {
        if let Some(remote) = &self.remote { return remote.read(at, out); }
        let file = self.file.as_mut().ok_or("Binary source unavailable")?;
        if DiskStamp::from_metadata(&file.metadata().map_err(io_error)?) != self.stamp {
            return Err("The file changed on disk. Reopen it before reading or saving this binary buffer.".into());
        }
        let seek = self.cursor != Some(at);
        self.cursor = None;
        if seek { file.seek(SeekFrom::Start(at)).map_err(io_error)?; }
        file.read_exact(out).map_err(io_error)?;
        self.cursor = Some(at + out.len() as u64);
        Ok(())
    }
}

#[derive(Clone, Debug)]
enum Piece {
    File {
        source: Arc<Mutex<SourceFile>>,
        offset: u64,
        len: u64,
    },
    Bytes {
        source: Arc<[u8]>,
        offset: u64,
        len: u64,
    },
}

impl Piece {
    fn read_cached(&self, offset: u64, out: &mut [u8]) -> bool {
        match self {
            Self::Bytes { .. } => self.read(offset, out).is_ok(),
            Self::File { source, offset: begin, .. } => source.lock().ok().is_some_and(|source|
                source.remote.as_ref().is_some_and(|remote| remote.read_cached(begin + offset, out))),
        }
    }
    fn len(&self) -> u64 {
        match self {
            Self::File { len, .. } | Self::Bytes { len, .. } => *len,
        }
    }

    fn slice(&self, start: u64, len: u64) -> Self {
        match self {
            Self::File { source, offset, .. } => Self::File {
                source: source.clone(),
                offset: offset + start,
                len,
            },
            Self::Bytes { source, offset, .. } => Self::Bytes {
                source: source.clone(),
                offset: offset + start,
                len,
            },
        }
    }

    fn read(&self, start: u64, out: &mut [u8]) -> Result<(), String> {
        match self {
            Self::File { source, offset, .. } => {
                let mut file = source.lock().map_err(|_| "Binary buffer lock failed")?;
                file.read(offset + start, out)
            }
            Self::Bytes { source, offset, .. } => {
                let begin =
                    usize::try_from(offset + start).map_err(|_| "Byte offset is too large")?;
                out.copy_from_slice(&source[begin..begin + out.len()]);
                Ok(())
            }
        }
    }
}

fn io_error(error: std::io::Error) -> String {
    error.to_string()
}

fn publish_no_replace(source: &Path, destination: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        // MoveFileW never replaces an existing target and also supports FAT/exFAT.
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileW(
                source.as_ptr(),
                destination.as_ptr(),
            )
        } == 0
        {
            return Err(io_error(std::io::Error::last_os_error()));
        }
        Ok(())
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let from = std::ffi::CString::new(source.as_os_str().as_bytes())
            .map_err(|error| error.to_string())?;
        let to = std::ffi::CString::new(destination.as_os_str().as_bytes())
            .map_err(|error| error.to_string())?;
        if unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        } == 0
        {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ENOSYS) && error.raw_os_error() != Some(libc::EINVAL)
        {
            return Err(io_error(error));
        }
        std::fs::hard_link(source, destination).map_err(io_error)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        std::fs::hard_link(source, destination).map_err(io_error)
    }
}

pub(crate) fn normalized_file_path(path: &Path) -> Result<PathBuf, String> {
    if crate::remote::workspace::is_view_key(path) {
        return Ok(path.to_path_buf());
    }
    let absolute = std::path::absolute(path).map_err(io_error)?;
    let resolved = match std::fs::canonicalize(&absolute) {
        Ok(path) => path,
        Err(error)
            if std::fs::symlink_metadata(&absolute)
                .is_ok_and(|metadata| metadata.file_type().is_symlink()) =>
        {
            return Err(format!(
                "Cannot resolve the selected file's symlink: {error}"
            ));
        }
        Err(_) => absolute,
    };
    #[cfg(windows)]
    {
        let text = resolved.to_string_lossy();
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{unc}")));
        }
        if let Some(disk) = text.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(disk));
        }
    }
    Ok(resolved)
}

#[derive(Clone, Debug)]
struct Snapshot {
    pieces: Vec<Piece>,
    len: u64,
    revision: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DiskStamp {
    pub len: u64,
    pub modified: Option<SystemTime>,
}

impl DiskStamp {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
        }
    }
    fn read(path: &Path) -> Result<Self, String> {
        let metadata = std::fs::metadata(path).map_err(io_error)?;
        if !metadata.is_file() {
            return Err("Choose a regular file to open in a custom editor".into());
        }
        Ok(Self::from_metadata(&metadata))
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DocumentInfo {
    pub path: String,
    pub size: u64,
    pub dirty: bool,
    pub read_only: bool,
    pub can_undo: bool,
    pub can_redo: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct BinaryDocument {
    path: PathBuf,
    stamp: DiskStamp,
    current: Snapshot,
    saved: Snapshot,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    saved_revision: u64,
    next_revision: u64,
    read_only: bool,
    amend_next: bool,
    recovery: Option<PathBuf>,
    transaction: Option<Snapshot>,
    remote: Option<RemoteDocument>,
}

#[derive(Clone, Debug)]
struct RemoteDocument {
    source: Option<Arc<crate::remote::binary::Source>>,
    workspace: crate::remote::workspace::Workspace,
    conflicted: bool,
    recent: Arc<Mutex<std::collections::VecDeque<u64>>>,
}

pub(crate) struct BinaryFragment(Vec<Piece>, u64);

impl BinaryDocument {
    pub(crate) fn unsaved() -> Self {
        let snapshot = Snapshot { pieces: Vec::new(), len: 0, revision: 0 };
        Self {
            path: PathBuf::new(), stamp: DiskStamp { len: 0, modified: None },
            current: snapshot.clone(), saved: snapshot,
            undo: Vec::new(), redo: Vec::new(), saved_revision: 0, next_revision: 1,
            read_only: false, amend_next: false, recovery: None, transaction: None, remote: None,
        }
    }
    pub(crate) fn open(path: &Path) -> Result<Self, String> {
        Self::open_io(path).map_err(io_error)
    }
    pub(crate) fn open_io(path: &Path) -> std::io::Result<Self> {
        let path = normalized_file_path(path).map_err(std::io::Error::other)?;
        let (source, metadata) = SourceFile::open_metadata(&path)?;
        let stamp = DiskStamp::from_metadata(&metadata);
        let read_only = metadata.permissions().readonly();
        let pieces = if stamp.len == 0 {
            Vec::new()
        } else {
            vec![Piece::File {
                source,
                offset: 0,
                len: stamp.len,
            }]
        };
        Ok(Self {
            path,
            current: Snapshot {
                pieces: pieces.clone(),
                len: stamp.len,
                revision: 0,
            },
            saved: Snapshot {
                pieces,
                len: stamp.len,
                revision: 0,
            },
            stamp,
            undo: Vec::new(),
            redo: Vec::new(),
            saved_revision: 0,
            next_revision: 1,
            read_only,
            amend_next: false,
            recovery: None,
            transaction: None,
            remote: None,
        })
    }

    pub(crate) fn stamp(&self) -> &DiskStamp { &self.stamp }

    pub(crate) fn fragment(&self, offset: u64, len: u64) -> Result<BinaryFragment, String> {
        let end = offset.checked_add(len).filter(|end| *end <= self.current.len).ok_or("Invalid binary source range")?;
        let mut pieces = Vec::new();
        let mut logical = 0;
        for piece in &self.current.pieces {
            let stop = logical + piece.len();
            if stop > offset && logical < end {
                let start = offset.saturating_sub(logical);
                pieces.push(piece.slice(start, (end.min(stop) - logical).saturating_sub(start)));
            }
            logical = stop;
            if logical >= end { break; }
        }
        Ok(BinaryFragment(pieces, len))
    }
    pub(crate) fn bytes_fragment(bytes: Vec<u8>) -> BinaryFragment {
        let len = bytes.len() as u64;
        BinaryFragment(if len == 0 { Vec::new() } else { vec![Piece::Bytes { source: bytes.into(), offset: 0, len }] }, len)
    }
    pub(crate) fn with_fragments(&self, fragments: Vec<BinaryFragment>) -> Result<Self, String> {
        let mut result = self.clone();
        let mut pieces = Vec::new();
        let mut len = 0u64;
        for BinaryFragment(part, size) in fragments {
            len = len.checked_add(size).ok_or("Binary file size overflow")?;
            pieces.extend(part);
        }
        result.current = Snapshot { pieces, len, revision: self.next_revision };
        result.next_revision = self.next_revision.checked_add(1).ok_or("Too many binary saves")?;
        Ok(result)
    }

    pub(crate) fn open_remote(path: &Path, workspace: &crate::remote::workspace::Workspace) -> Result<Self, String> {
        let source = crate::remote::binary::Source::open(workspace, path).map_err(io_error)?;
        Ok(Self::from_remote_source(path, source, workspace))
    }
    fn from_remote_source(path: &Path, source: Arc<crate::remote::binary::Source>, workspace: &crate::remote::workspace::Workspace) -> Self {
        let stamp = source.stamp.clone();
        let file = Arc::new(Mutex::new(SourceFile { file: None, remote: Some(source.clone()), stamp: stamp.clone(), cursor: None }));
        let pieces = if stamp.len == 0 { Vec::new() } else { vec![Piece::File { source: file, offset: 0, len: stamp.len }] };
        let snapshot = Snapshot { pieces, len: stamp.len, revision: 0 };
        Self {
            path: path.to_path_buf(), stamp,
            current: snapshot.clone(), saved: snapshot,
            undo: Vec::new(), redo: Vec::new(), saved_revision: 0, next_revision: 1,
            read_only: source.read_only, amend_next: false, recovery: None, transaction: None,
            remote: Some(RemoteDocument { source: Some(source), workspace: workspace.clone(), conflicted: false, recent: Arc::default() }),
        }
    }

    fn save_remote(&mut self, destination: Option<(&Path, bool)>) -> Result<(), String> {
        let remote = self.remote.as_ref().ok_or("Remote binary source unavailable")?;
        if destination.is_none() && remote.conflicted {
            return Err("Cannot validate the original remote file. Save a copy or reopen it before overwriting it.".into());
        }
        let mut plan = crate::remote::binary::Plan::new();
        let mut buffer = Vec::new();
        for piece in &self.current.pieces {
            if let Piece::File { source, offset, len } = piece {
                let source = source.lock().map_err(|_| "Binary source lock failed")?;
                if let Some(source) = &source.remote {
                    plan.source(source.id, *offset, *len)?;
                    continue;
                }
            }
            if let Piece::Bytes { source, offset, len } = piece {
                let start = *offset as usize;
                plan.bytes(&source[start..start + *len as usize])?;
                continue;
            }
            if buffer.is_empty() { buffer.resize(IO_CHUNK, 0); }
            let mut offset = 0u64;
            while offset < piece.len() {
                let count = (piece.len() - offset).min(IO_CHUNK as u64) as usize;
                piece.read(offset, &mut buffer[..count])?;
                offset += count as u64;
                plan.bytes(&buffer[..count])?;
            }
        }
        let plan = plan.finish()?;
        let path = destination.map_or_else(|| self.path.clone(), |(path, _)| path.to_path_buf());
        let workspace = remote.workspace.clone();
        let source = crate::remote::binary::Source::save(&workspace, remote.source.as_ref().map(|source| source.id), plan,
            destination.map(|(_, replace)| (path.as_path(), replace)))?;
        if source.stamp.len != self.current.len { return Err("Remote binary save acknowledged an unexpected length".into()); }
        let recent = remote.recent.clone();
        let compact = self.current.pieces.len() > 1024;
        let pages = if compact { recent.lock().unwrap().clone() } else { Default::default() };
        // Reuse already resident viewport pages. A successful save must not
        // force an extra round trip just to render the same bytes again.
        for page in pages {
            let offset = page * crate::remote::binary::PAGE as u64;
            if offset >= self.current.len { continue; }
            let mut bytes = vec![0; (self.current.len - offset).min(crate::remote::binary::PAGE as u64) as usize];
            if Self::read_snapshot_cached(&self.current, offset, &mut bytes) { source.seed(page, bytes); }
        }
        self.path = path;
        self.stamp = source.stamp.clone();
        let file = Arc::new(Mutex::new(SourceFile { file: None, remote: Some(source.clone()), stamp: self.stamp.clone(), cursor: None }));
        // Keep resident pieces through normal saves. Periodic compaction bounds
        // fragmentation without discarding the viewport cache on every save.
        if compact {
            self.current.pieces = if self.current.len == 0 { Vec::new() } else { vec![Piece::File { source: file, offset: 0, len: self.current.len }] };
        }
        self.saved = self.current.clone();
        self.saved_revision = self.current.revision;
        self.read_only = source.read_only;
        self.remote = Some(RemoteDocument { source: Some(source), workspace, conflicted: false, recent });
        self.clear_recovery()
    }

    pub(crate) fn info(&self) -> DocumentInfo {
        DocumentInfo {
            path: self.path.to_string_lossy().into_owned(),
            size: self.current.len,
            dirty: self.current.revision != self.saved_revision,
            read_only: self.read_only,
            can_undo: !self.undo.is_empty(),
            can_redo: !self.redo.is_empty(),
        }
    }

    pub(crate) fn read(&self, offset: u64, out: &mut [u8]) -> Result<usize, String> {
        let count = Self::read_snapshot(&self.current, offset, out)?;
        if count > 0 && let Some(remote) = &self.remote {
            let mut recent = remote.recent.lock().unwrap();
            let first = offset / crate::remote::binary::PAGE as u64;
            let last = (offset + count as u64 - 1) / crate::remote::binary::PAGE as u64;
            for page in first.max(last.saturating_sub(7))..=last {
                recent.retain(|previous| *previous != page);
                recent.push_back(page);
                if recent.len() > 8 { recent.pop_front(); }
            }
        }
        Ok(count)
    }

    fn read_snapshot_cached(snapshot: &Snapshot, offset: u64, out: &mut [u8]) -> bool {
        let mut logical = 0u64;
        let mut written = 0;
        for piece in &snapshot.pieces {
            let end = logical + piece.len();
            if end > offset && written < out.len() {
                let start = offset.saturating_sub(logical);
                let count = (piece.len() - start).min((out.len() - written) as u64) as usize;
                if !piece.read_cached(start, &mut out[written..written + count]) { return false; }
                written += count;
            }
            logical = end;
            if written == out.len() { return true; }
        }
        written == out.len()
    }

    fn read_snapshot(snapshot: &Snapshot, offset: u64, out: &mut [u8]) -> Result<usize, String> {
        if offset > snapshot.len {
            return Err("Byte offset is past the end of the file".into());
        }
        let count = out
            .len()
            .min(usize::try_from(snapshot.len - offset).unwrap_or(usize::MAX));
        let mut logical = 0u64;
        let mut written = 0usize;
        for piece in &snapshot.pieces {
            let end = logical + piece.len();
            if end > offset && written < count {
                let start = offset.saturating_sub(logical);
                let available = usize::try_from(piece.len() - start).unwrap_or(usize::MAX);
                let take = available.min(count - written);
                piece.read(start, &mut out[written..written + take])?;
                written += take;
            }
            logical = end;
            if written == count {
                break;
            }
        }
        Ok(written)
    }

    pub(crate) fn changed(&self, offset: u64, count: usize) -> Result<Vec<u8>, String> {
        let mut current = vec![0; count];
        let actual = self.read(offset, &mut current)?;
        let mut flags = vec![0; actual];
        if !self.info().dirty {
            return Ok(flags);
        }
        let mut saved = vec![0; actual];
        let original = if offset <= self.saved.len {
            Self::read_snapshot(&self.saved, offset, &mut saved)?
        } else {
            0
        };
        for index in 0..actual {
            flags[index] = u8::from(index >= original || current[index] != saved[index]);
        }
        Ok(flags)
    }

    pub(crate) fn edit(&mut self, offset: u64, deleted: u64, bytes: &[u8]) -> Result<(), String> {
        if self.read_only {
            return Err("This file is read-only".into());
        }
        let end = offset
            .checked_add(deleted)
            .ok_or("Byte range is too large")?;
        if end > self.current.len {
            return Err("Edit range is past the end of the file".into());
        }
        if deleted == 0 && bytes.is_empty() {
            return Ok(());
        }
        let len = self
            .current
            .len
            .checked_sub(deleted)
            .and_then(|n| n.checked_add(bytes.len() as u64))
            .ok_or("The edited file would be too large")?;
        let mut pieces = Vec::with_capacity(self.current.pieces.len() + 2);
        let mut logical = 0u64;
        // Retain the prefix without reading or copying file contents.
        for piece in &self.current.pieces {
            if logical < offset {
                let take = (offset - logical).min(piece.len());
                if take > 0 {
                    pieces.push(piece.slice(0, take));
                }
            }
            logical += piece.len();
            if logical >= offset {
                break;
            }
        }
        if !bytes.is_empty() {
            pieces.push(Piece::Bytes {
                source: Arc::from(bytes),
                offset: 0,
                len: bytes.len() as u64,
            });
        }
        logical = 0;
        for piece in &self.current.pieces {
            let piece_end = logical + piece.len();
            if piece_end > end {
                let start = end.saturating_sub(logical);
                pieces.push(piece.slice(start, piece.len() - start));
            }
            logical = piece_end;
        }
        if !std::mem::take(&mut self.amend_next) && self.transaction.is_none() {
            self.undo.push(self.current.clone());
        }
        if self.undo.len() > HISTORY_LIMIT {
            self.undo.remove(0);
        }
        if self.transaction.is_none() {
            self.redo.clear();
        }
        self.current = Snapshot {
            pieces,
            len,
            revision: self.next_revision,
        };
        self.next_revision = self.next_revision.checked_add(1).ok_or("Too many edits")?;
        Ok(())
    }

    pub(crate) fn undo(&mut self) -> bool {
        if self.transaction.is_some() {
            return false;
        }
        self.amend_next = false;
        let Some(previous) = self.undo.pop() else {
            return false;
        };
        self.redo
            .push(std::mem::replace(&mut self.current, previous));
        true
    }

    pub(crate) fn redo(&mut self) -> bool {
        if self.transaction.is_some() {
            return false;
        }
        self.amend_next = false;
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(std::mem::replace(&mut self.current, next));
        true
    }

    fn check_disk(&self) -> Result<(), String> {
        if DiskStamp::read(&self.path)? != self.stamp {
            return Err("The file changed on disk. Reopen it before saving to avoid overwriting external changes.".into());
        }
        Ok(())
    }

    pub(crate) fn save(&mut self) -> Result<(), String> {
        if self.transaction.is_some() {
            return Err("Finish or cancel the current document operation before saving".into());
        }
        self.amend_next = false;
        if !self.info().dirty {
            return Ok(());
        }
        if self.read_only {
            return Err("This file is read-only".into());
        }
        if self.remote.is_some() { return self.save_remote(None); }
        self.check_disk()?;
        let parent = self.path.parent().ok_or("The file has no parent folder")?;
        let permissions = std::fs::metadata(&self.path)
            .map_err(io_error)?
            .permissions();
        // Use a normal file: Windows' temporary attribute prevents replacement
        // on some filesystems and under restricted application accounts.
        let mut temporary = tempfile::Builder::new()
            .prefix(".vtcode-save-")
            .make_in(parent, |path| {
                std::fs::OpenOptions::new()
                    .create_new(true)
                    .read(true)
                    .write(true)
                    .open(path)
            })
            .map_err(io_error)?;
        let mut buffer = vec![0u8; IO_CHUNK];
        let mut offset = 0u64;
        while offset < self.current.len {
            let count = self.read(offset, &mut buffer)?;
            if count == 0 {
                return Err("Could not read the complete binary buffer".into());
            }
            temporary.write_all(&buffer[..count]).map_err(io_error)?;
            offset += count as u64;
        }
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(|error| format!("Could not preserve file permissions: {error}"))?;
        temporary.as_file().sync_all().map_err(|error| format!("Could not flush saved bytes: {error}"))?;
        self.check_disk()?;
        std::fs::rename(temporary.path(), &self.path).map_err(|error| format!("Could not atomically replace {}: {error}", self.path.display()))?;
        self.stamp = DiskStamp::read(&self.path)?;
        // Undo snapshots keep their original file handles, even across atomic saves.
        let source = SourceFile::open(&self.path)?;
        self.current.pieces = if self.current.len == 0 {
            Vec::new()
        } else {
            vec![Piece::File {
                source,
                offset: 0,
                len: self.current.len,
            }]
        };
        self.saved_revision = self.current.revision;
        self.saved = self.current.clone();
        self.clear_recovery()?;
        Ok(())
    }

    pub(crate) fn amend_next_edit(&mut self) {
        self.amend_next = !self.undo.is_empty() && self.info().dirty;
    }

    pub(crate) fn begin_transaction(&mut self) -> Result<(), String> {
        if self.transaction.is_some() {
            return Err("A document operation is already running".into());
        }
        if self.read_only {
            return Err("This file is read-only".into());
        }
        self.amend_next = false;
        self.transaction = Some(self.current.clone());
        Ok(())
    }

    pub(crate) fn end_transaction(&mut self, commit: bool) -> Result<(), String> {
        let Some(original) = self.transaction.take() else {
            return Err("No document operation is running".into());
        };
        if commit {
            if original.revision != self.current.revision {
                self.undo.push(original);
                if self.undo.len() > HISTORY_LIMIT {
                    self.undo.remove(0);
                }
                self.redo.clear();
            }
        } else {
            self.current = original;
        }
        self.amend_next = false;
        Ok(())
    }

    pub(crate) fn abort_transaction(&mut self) {
        if self.transaction.is_some() {
            let _ = self.end_transaction(false);
        }
    }

    pub(crate) fn reload_if_clean(&mut self) -> Result<(), String> {
        if self.transaction.is_some() {
            return Err("Finish or cancel the current document operation before reloading".into());
        }
        if self.info().dirty {
            return Err("Unsaved document changes must be saved or discarded before reloading".into());
        }
        self.reload()
    }

    pub(crate) fn reload(&mut self) -> Result<(), String> {
        let fresh = match self.remote.as_ref().map_or_else(|| Self::open(&self.path), |remote| Self::open_remote(&self.path, &remote.workspace)) {
            Ok(document) => document,
            Err(_)
                if std::fs::metadata(&self.path)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                self.clear_recovery()?;
                self.abort_transaction();
                self.current = Snapshot {
                    pieces: Vec::new(),
                    len: 0,
                    revision: self.next_revision,
                };
                self.saved = self.current.clone();
                self.saved_revision = self.current.revision;
                self.undo.clear();
                self.redo.clear();
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        self.clear_recovery()?;
        *self = fresh;
        Ok(())
    }

    pub(crate) fn save_as(&mut self, path: &Path, replace: bool) -> Result<(), String> {
        if self.transaction.is_some() {
            return Err("Finish or cancel the current document operation before saving".into());
        }
        let path = normalized_file_path(path)?;
        if path == self.path {
            return self.save();
        }
        if self.remote.is_some() { return self.save_remote(Some((&path, replace))); }
        let parent = path.parent().ok_or("The file has no parent folder")?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".vtcode-save-")
            .make_in(parent, |path| {
                std::fs::OpenOptions::new()
                    .create_new(true)
                    .read(true)
                    .write(true)
                    .open(path)
            })
            .map_err(io_error)?;
        let mut buffer = vec![0; IO_CHUNK];
        let mut offset = 0;
        while offset < self.current.len {
            let count = self.read(offset, &mut buffer)?;
            if count == 0 {
                return Err("Could not read the complete binary buffer".into());
            }
            temporary.write_all(&buffer[..count]).map_err(io_error)?;
            offset += count as u64;
        }
        temporary.as_file().sync_all().map_err(io_error)?;
        if replace {
            std::fs::rename(temporary.path(), &path).map_err(io_error)?;
        } else {
            // Never replace a file which appeared after dialog validation.
            publish_no_replace(temporary.path(), &path)?;
        }
        self.path = path;
        self.stamp = DiskStamp::read(&self.path)?;
        self.read_only = false;
        let source = SourceFile::open(&self.path)?;
        self.current.pieces = if self.current.len == 0 {
            Vec::new()
        } else {
            vec![Piece::File {
                source,
                offset: 0,
                len: self.current.len,
            }]
        };
        self.saved_revision = self.current.revision;
        self.saved = self.current.clone();
        self.clear_recovery()?;
        Ok(())
    }

    fn clear_recovery(&mut self) -> Result<(), String> {
        if let Some(path) = self.recovery.as_ref() {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(format!(
                        "File saved, but its recovery record could not be removed: {error}"
                    ));
                }
            }
            let _ = std::fs::remove_file(path.with_extension("bin"));
        }
        self.recovery = None;
        Ok(())
    }

    pub(crate) fn discard_recovery(&mut self) -> Result<(), String> {
        self.clear_recovery()
    }

    pub(crate) fn keep_for_session(
        &mut self,
        directory: &Path,
        extension_id: &str,
        command_id: &str,
        _id: u32,
    ) -> Result<(), String> {
        if self.transaction.is_some() {
            return Err("Finish or cancel the current document operation before keeping it for the next session".into());
        }
        if !self.info().dirty {
            return self.clear_recovery();
        }
        std::fs::create_dir_all(directory).map_err(io_error)?;
        let mut hash = 0x6c62272e07bb014262b821756295c58du128;
        for byte in self.path.as_os_str().as_encoded_bytes() {
            hash ^= u128::from(*byte);
            hash = hash.wrapping_mul(0x0000000001000000000000000000013b);
        }
        let path = self
            .recovery
            .clone()
            .unwrap_or_else(|| directory.join(format!("{extension_id}-{hash:032x}.json")));
        let mut temporary = tempfile::Builder::new()
            .prefix(".recovery-")
            .make_in(directory, |path| {
                std::fs::OpenOptions::new()
                    .create_new(true)
                    .read(true)
                    .write(true)
                    .open(path)
            })
            .map_err(io_error)?;
        let mut buffer = vec![0; IO_CHUNK];
        let mut offset = 0;
        while offset < self.current.len {
            let count = self.read(offset, &mut buffer)?;
            if count == 0 {
                return Err("Could not preserve the complete custom document".into());
            }
            temporary.write_all(&buffer[..count]).map_err(io_error)?;
            offset += count as u64;
        }
        temporary.as_file().sync_all().map_err(io_error)?;
        std::fs::rename(temporary.path(), path.with_extension("bin")).map_err(io_error)?;
        let record = crate::extension_recovery::RecoveryRecord {
            extension_id: extension_id.into(),
            command_id: command_id.into(),
            original_path: self.path.clone(),
            stamp: self.stamp.clone(),
            record_path: path.clone(),
        };
        let mut manifest = tempfile::Builder::new()
            .prefix(".recovery-")
            .make_in(directory, |path| {
                std::fs::OpenOptions::new()
                    .create_new(true)
                    .read(true)
                    .write(true)
                    .open(path)
            })
            .map_err(io_error)?;
        manifest
            .write_all(&serde_json::to_vec(&record).map_err(|error| error.to_string())?)
            .map_err(io_error)?;
        manifest.as_file().sync_all().map_err(io_error)?;
        std::fs::rename(manifest.path(), &path).map_err(io_error)?;
        self.recovery = Some(path);
        Ok(())
    }

    pub(crate) fn restore(
        record: &crate::extension_recovery::RecoveryRecord,
    ) -> Result<Self, String> {
        let content = record.record_path.with_extension("bin");
        let stamp = DiskStamp::read(&content)?;
        let file = SourceFile::open(&content)?;
        let pieces = if stamp.len == 0 {
            Vec::new()
        } else {
            vec![Piece::File {
                source: file,
                offset: 0,
                len: stamp.len,
            }]
        };
        Ok(Self {
            path: record.original_path.clone(),
            stamp: record.stamp.clone(),
            current: Snapshot {
                pieces,
                len: stamp.len,
                revision: 1,
            },
            saved: Self::open(&record.original_path)
                .map(|document| document.current)
                .unwrap_or(Snapshot {
                    pieces: Vec::new(),
                    len: 0,
                    revision: 0,
                }),
            undo: Vec::new(),
            redo: Vec::new(),
            saved_revision: 0,
            next_revision: 2,
            read_only: std::fs::metadata(&record.original_path)
                .is_ok_and(|metadata| metadata.permissions().readonly()),
            amend_next: false,
            recovery: Some(record.record_path.clone()),
            transaction: None,
            remote: None,
        })
    }

    pub(crate) fn restore_remote(record: &crate::extension_recovery::RecoveryRecord, workspace: &crate::remote::workspace::Workspace) -> Result<Self, String> {
        let mut restored = Self::restore(record)?;
        // An unavailable/replaced original must not hide durable local recovery.
        // A copy needs no original baseline; ordinary Save still refuses it.
        let source = match crate::remote::binary::Source::open(workspace, &record.original_path) {
            Err(_) => {
                restored.remote = Some(RemoteDocument { source: None, workspace: workspace.clone(), conflicted: true, recent: Arc::default() });
                return Ok(restored);
            }
            Ok(source) => source,
        };
        let current = Self::from_remote_source(&record.original_path, source, workspace);
        restored.read_only = current.read_only;
        restored.saved = current.saved;
        restored.remote = current.remote.map(|mut remote| {
            remote.conflicted = record.stamp != current.stamp;
            remote
        });
        Ok(restored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(document: &BinaryDocument) -> Vec<u8> {
        let mut result = vec![0; document.info().size as usize];
        assert_eq!(document.read(0, &mut result).unwrap(), result.len());
        result
    }

    #[test]
    fn edits_across_pieces_and_save_preserve_undo_and_redo() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("sample.bin");
        std::fs::write(&path, [0, 1, 2, 3, 4, 255]).unwrap();
        let mut document = BinaryDocument::open(&path).unwrap();
        document.edit(2, 2, &[10, 11, 12]).unwrap();
        document.edit(1, 4, &[42]).unwrap();
        assert_eq!(bytes(&document), [0, 42, 4, 255]);
        document.save().unwrap();
        assert!(!document.info().dirty);
        assert_eq!(std::fs::read(&path).unwrap(), [0, 42, 4, 255]);
        assert!(document.undo());
        assert_eq!(bytes(&document), [0, 1, 10, 11, 12, 4, 255]);
        assert!(document.info().dirty);
        assert!(document.undo());
        assert_eq!(bytes(&document), [0, 1, 2, 3, 4, 255]);
        assert!(document.redo());
        assert!(document.redo());
        assert_eq!(bytes(&document), [0, 42, 4, 255]);
        assert!(!document.info().dirty);
        document.edit(4, 0, &[13]).unwrap();
        document.save().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), [0, 42, 4, 255, 13]);
    }

    #[test]
    fn empty_files_and_bounds() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut document = BinaryDocument::open(file.path()).unwrap();
        assert_eq!(document.read(0, &mut [0; 10]).unwrap(), 0);
        assert!(document.edit(1, 0, &[1]).is_err());
        assert!(document.edit(0, u64::MAX, &[]).is_err());
        document.edit(0, 0, &[0, 255]).unwrap();
        document.edit(0, 2, &[]).unwrap();
        assert!(bytes(&document).is_empty());
        document.save().unwrap();
        assert!(std::fs::read(file.path()).unwrap().is_empty());
    }

    #[test]
    fn clean_reload_reads_external_replacements_and_preserves_edits_on_refusal() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("dump.bin");
        std::fs::write(&path, b"first").unwrap();
        let mut document = BinaryDocument::open(&path).unwrap();
        document.begin_transaction().unwrap();
        assert!(document.reload_if_clean().unwrap_err().contains("cancel"));
        document.end_transaction(false).unwrap();
        document.edit(0, 1, b"F").unwrap();
        assert!(document.reload_if_clean().unwrap_err().contains("Unsaved"));
        assert_eq!(bytes(&document), b"First");
        assert_eq!(std::fs::read(&path).unwrap(), b"first");
        document.undo();
        std::fs::write(&path, b"another simulation").unwrap();
        document.reload_if_clean().unwrap();
        assert_eq!(bytes(&document), b"another simulation");
        assert!(!document.info().dirty);
    }

    #[test]
    fn changed_ranges_follow_saved_snapshots_and_save_as_detaches_the_source() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.bin");
        let destination = directory.path().join("saved.bin");
        std::fs::write(&source, [1, 2, 3]).unwrap();
        let mut document = BinaryDocument::open(&source).unwrap();
        assert_eq!(document.changed(0, 10).unwrap(), [0, 0, 0]);
        document.edit(1, 1, &[9]).unwrap();
        assert_eq!(document.changed(0, 10).unwrap(), [0, 1, 0]);
        document.save_as(&destination, false).unwrap();
        assert_eq!(document.changed(0, 10).unwrap(), [0, 0, 0]);
        // A size change detects the external edit even when rapid writes share
        // a filesystem timestamp (for example on a Linux tmpfs).
        std::fs::write(&source, [20, 30, 40, 50]).unwrap();
        assert_eq!(bytes(&document), [1, 9, 3]);
        document.undo();
        assert!(
            document
                .changed(0, 10)
                .unwrap_err()
                .contains("changed on disk")
        );
        document.redo();
        assert!(!document.info().dirty);
        assert_eq!(bytes(&document), [1, 9, 3]);
    }

    #[test]
    fn transactions_are_atomic_for_undo_cancel_and_save_guards() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.bin");
        std::fs::write(&source, [1, 2, 3]).unwrap();
        let mut document = BinaryDocument::open(&source).unwrap();
        document.begin_transaction().unwrap();
        document.edit(0, 1, &[8]).unwrap();
        document.edit(1, 1, &[9]).unwrap();
        assert!(document.save().unwrap_err().contains("cancel"));
        assert!(!document.undo());
        document.end_transaction(false).unwrap();
        assert_eq!(bytes(&document), [1, 2, 3]);
        assert!(!document.info().can_undo);
        document.begin_transaction().unwrap();
        document.edit(0, 1, &[8]).unwrap();
        document.edit(1, 1, &[9]).unwrap();
        document.end_transaction(true).unwrap();
        assert_eq!(bytes(&document), [8, 9, 3]);
        assert!(document.undo());
        assert_eq!(bytes(&document), [1, 2, 3]);
        assert!(!document.undo());
    }

    #[test]
    fn save_refuses_external_changes_without_touching_them() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"original").unwrap();
        let mut document = BinaryDocument::open(file.path()).unwrap();
        document.edit(0, 1, b"O").unwrap();
        std::fs::write(file.path(), b"externally changed").unwrap();
        assert!(document.save().unwrap_err().contains("changed on disk"));
        assert_eq!(std::fs::read(file.path()).unwrap(), b"externally changed");
        assert!(document.info().dirty);
    }

    #[test]
    fn large_sparse_files_are_opened_and_edited_without_loading_them() {
        let file = tempfile::NamedTempFile::new().unwrap();
        file.as_file().set_len(5 * 1024 * 1024 * 1024).unwrap();
        let mut document = BinaryDocument::open(file.path()).unwrap();
        let end = document.info().size - 2;
        document.edit(end, 2, &[0xCA, 0xFE]).unwrap();
        let mut tail = [0; 4];
        assert_eq!(document.read(end - 2, &mut tail).unwrap(), 4);
        assert_eq!(tail, [0, 0, 0xCA, 0xFE]);
        assert!(document.undo());
        assert_eq!(document.info().size, 5 * 1024 * 1024 * 1024);
    }

    #[test]
    fn randomized_edits_match_a_flat_buffer() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut document = BinaryDocument::open(file.path()).unwrap();
        let mut expected = Vec::new();
        let mut random = 1u64;
        for _ in 0..600 {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
            let offset = random as usize % (expected.len() + 1);
            let delete = (random >> 21) as usize % (expected.len() - offset + 1);
            let replacement = random.to_le_bytes();
            let take = (random >> 40) as usize % 9;
            document
                .edit(offset as u64, delete as u64, &replacement[..take])
                .unwrap();
            expected.splice(offset..offset + delete, replacement[..take].iter().copied());
            assert_eq!(bytes(&document), expected);
        }
    }

    #[test]
    fn recovery_preserves_binary_edits_and_removes_sidecars_after_save() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file.bin");
        std::fs::write(&path, [0, 1, 255]).unwrap();
        let mut document = BinaryDocument::open(&path).unwrap();
        document.edit(1, 1, &[0xFE, 0xED]).unwrap();
        let recovery = root.path().join("recovery");
        document
            .keep_for_session(&recovery, "test.hex", "hex.open", 1)
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), [0, 1, 255]);
        let records = crate::extension_recovery::read(&recovery);
        assert_eq!(records.len(), 1);
        drop(document);
        let mut recovered = BinaryDocument::restore(&records[0]).unwrap();
        assert_eq!(bytes(&recovered), [0, 0xFE, 0xED, 255]);
        assert!(recovered.info().dirty);
        recovered.save().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), [0, 0xFE, 0xED, 255]);
        assert!(crate::extension_recovery::read(&recovery).is_empty());
    }

    #[test]
    fn recovery_refuses_external_changes_and_can_be_saved_under_a_new_name() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("file.bin");
        std::fs::write(&path, b"original").unwrap();
        let mut document = BinaryDocument::open(&path).unwrap();
        document.edit(0, 8, &[0xCA, 0xFE]).unwrap();
        let recovery = root.path().join("recovery");
        document
            .keep_for_session(&recovery, "test.hex", "hex.open", 1)
            .unwrap();
        drop(document);
        std::fs::write(&path, b"externally updated").unwrap();
        let record = crate::extension_recovery::read(&recovery).remove(0);
        let mut recovered = BinaryDocument::restore(&record).unwrap();
        assert!(recovered.save().is_err());
        assert_eq!(bytes(&recovered), [0xCA, 0xFE]);
        let copy = root.path().join("copy.bin");
        recovered.save_as(&copy, false).unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), [0xCA, 0xFE]);
        assert_eq!(std::fs::read(&path).unwrap(), b"externally updated");
        assert!(crate::extension_recovery::read(&recovery).is_empty());
        recovered.edit(0, 1, &[0]).unwrap();
        assert!(recovered.save_as(&path, false).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"externally updated");
    }
}
