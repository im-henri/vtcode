use super::{Blob, FileState, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{self, Read, Seek, Write},
    path::{Component, Path, PathBuf},
};

pub fn digest(bytes: &[u8]) -> String {
    git2::Oid::hash_object(git2::ObjectType::Blob, bytes)
        .expect("Git blob hash")
        .to_string()
}

pub(super) fn path_key(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(path.to_string_lossy().replace('/', "\\").to_lowercase())
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// Use the real spelling of existing targets. For a missing suffix, consult
/// the first existing parent's mode before treating distinct spellings as
/// separate destinations. The mode of uncreated directories is unknown.
#[cfg(windows)]
pub(super) fn restore_target_key(path: &Path) -> Result<PathBuf> {
    let mut ancestor = path;
    let mut missing = Vec::new();
    loop {
        match ancestor.canonicalize() {
            Ok(mut actual) => {
                if let Some(first) = missing.pop() {
                    if case_sensitive_directory(&actual) {
                        actual.push(first);
                    } else {
                        actual.push(path_key(Path::new(first)));
                    }
                    for part in missing.into_iter().rev() {
                        actual.push(path_key(Path::new(part)));
                    }
                }
                return Ok(actual);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", path.display())),
        }
        missing.push(ancestor.file_name().ok_or("Missing rollback target parent")?);
        ancestor = ancestor.parent().ok_or("Missing rollback target parent")?;
    }
}

#[cfg(windows)]
fn case_sensitive_directory(path: &Path) -> bool {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_CASE_SENSITIVE_INFO, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, FileCaseSensitiveInfo, GetFileInformationByHandleEx,
    };
    const FILE_CS_FLAG_CASE_SENSITIVE_DIR: u32 = 1;
    let Ok(file) = File::options()
        .access_mode(FILE_READ_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    else {
        return false;
    };
    let mut info = FILE_CASE_SENSITIVE_INFO { Flags: 0 };
    // SAFETY: file owns the handle and info is the output structure for this class.
    let queried = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileCaseSensitiveInfo,
            (&mut info as *mut FILE_CASE_SENSITIVE_INFO).cast(),
            std::mem::size_of::<FILE_CASE_SENSITIVE_INFO>() as u32,
        )
    };
    queried != 0 && info.Flags & FILE_CS_FLAG_CASE_SENSITIVE_DIR != 0
}

pub fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|e| e.to_string())?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(super) struct BackupIdentity {
    volume: u64,
    file: [u8; 16],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    created: Option<(u64, u32)>,
}

impl BackupIdentity {
    fn of(file: &File, _metadata: &fs::Metadata) -> Result<Self> {
        let (volume, file) = crate::file_source::file_identity(file, _metadata)
            .ok_or("Cannot identify the file to retain its rollback backup safely")?;
        #[cfg(unix)]
        let created = _metadata
            .created()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|time| (time.as_secs(), time.subsec_nanos()));
        // NTFS can carry a deleted destination's creation time onto the
        // renamed file. The identifier from the handle survives that rename.
        #[cfg(not(unix))]
        let created = None;
        Ok(Self {
            volume,
            file: file.to_le_bytes(),
            created,
        })
    }
}

fn open_checkpoint_file(path: &Path) -> Result<Option<(File, fs::Metadata)>> {
    match crate::file_source::open_canonical_regular_file(path) {
        Ok((file, metadata)) => Ok(Some((file, metadata))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

pub(super) fn backup_identity(path: &Path) -> Result<Option<BackupIdentity>> {
    open_checkpoint_file(path)?
        .map(|(file, metadata)| BackupIdentity::of(&file, &metadata))
        .transpose()
}

pub(super) fn fresh_backup_path(parent: &Path, prefix: &str) -> Result<PathBuf> {
    let temporary = tempfile::Builder::new()
        .prefix(prefix)
        .tempfile_in(parent)
        .map_err(|e| e.to_string())?;
    let path = temporary.path().to_path_buf();
    temporary.close().map_err(|e| e.to_string())?;
    Ok(path)
}

pub(super) fn displace_to_backup(source: &Path, backup: &Path) -> Result<()> {
    crate::file_source::rename_no_replace(source, backup)
        .map_err(|e| format!("Cannot retain {} at {}: {e}", source.display(), backup.display()))?;
    sync_directory(source.parent().ok_or("Missing rollback backup directory")?)
}

pub(super) fn capture_displaced(
    repository: &Path,
    path: &Path,
    expected: Option<&BackupIdentity>,
) -> Result<Option<FileState>> {
    let Some(expected) = expected else {
        // Older write-ahead plans recorded only the backup's name.
        return capture(repository, path);
    };
    let Some((file, metadata)) = open_checkpoint_file(path)? else {
        return Ok(None);
    };
    if BackupIdentity::of(&file, &metadata)? != *expected {
        return Err(format!(
            "Rollback backup was replaced: {}. The file was retained.",
            path.display()
        ));
    }
    // Recover the bytes from the verified handle, even when the original
    // received a final write after it was moved to the backup name.
    capture_opened(&self::repository(repository)?, path, file, metadata, || {}).map(Some)
}

pub fn atomic_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let parent = path.parent().ok_or("Missing storage directory")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    serde_json::to_writer(temporary.as_file_mut(), value).map_err(|e| e.to_string())?;
    temporary.as_file().sync_all().map_err(|e| e.to_string())?;
    temporary.persist(path).map_err(|e| e.to_string())?;
    sync_directory(parent)
}

pub(super) fn backup_history(directory: &Path, bytes: &[u8]) -> Result<()> {
    let mut backup = tempfile::Builder::new()
        .prefix("state-before-chunk-repair-")
        .suffix(".json")
        .tempfile_in(directory)
        .map_err(|e| e.to_string())?;
    backup.write_all(bytes).map_err(|e| e.to_string())?;
    backup.as_file().sync_all().map_err(|e| e.to_string())?;
    backup.keep().map_err(|e| e.to_string())?;
    sync_directory(directory)
}

pub(super) fn checked_root(root: &Path) -> Result<()> {
    // `root` is the canonical directory bound when the journal opens. Checking
    // only relative components misses a replaced root or one of its ancestors.
    if root.canonicalize().map_err(|e| e.to_string())? != root {
        return Err("Checkpoint workspace location changed; no file operation is safe".into());
    }
    Ok(())
}

/// Refuse links/junctions and escaping paths, including replaced ancestors.
pub fn checked_path(root: &Path, relative: &Path) -> Result<PathBuf> {
    checked_root(root)?;
    if relative.as_os_str().is_empty()
        || relative.components().any(|part| match part {
            Component::Normal(name) => metadata_name(name) || crate::acp::files::has_windows_stream_separator(name),
            _ => true,
        })
    {
        return Err("Unsafe path in rollback history".into());
    }
    let mut result = root.to_path_buf();
    for part in relative.components() {
        result.push(part);
        match fs::symlink_metadata(&result) {
            Ok(meta) if is_link(&meta) => {
                return Err(format!(
                    "Rollback does not follow links: {}",
                    result.display()
                ));
            }
            Ok(meta) if meta.is_dir() && result.join(".git").exists() => {
                return Err(format!(
                    "Rollback does not cross nested repositories: {}", result.display()
                ));
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(result)
}

fn metadata_name(name: &std::ffi::OsStr) -> bool {
    let name = name.to_string_lossy();
    if cfg!(windows) {
        name.eq_ignore_ascii_case(".git") || name.eq_ignore_ascii_case(".vtcode")
    } else {
        name == ".git" || name == ".vtcode"
    }
}

pub fn excluded(path: &Path) -> bool {
    path.components().any(|c| {
        metadata_name(c.as_os_str())
            || c.as_os_str()
                .to_string_lossy()
                .starts_with(".vtcode-rollback-")
    })
}

pub fn is_link(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

pub fn mode(meta: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        u32::from(meta.permissions().readonly())
    }
}

pub fn set_mode(file: &File, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(mode))
            .map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    {
        let mut p = file.metadata().map_err(|e| e.to_string())?.permissions();
        p.set_readonly(mode != 0);
        file.set_permissions(p).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn oid(blob: &Blob) -> Result<git2::Oid> {
    git2::Oid::from_str(&blob.hash).map_err(|e| e.to_string())
}

pub(super) fn repository(path: &Path) -> Result<git2::Repository> {
    let repo = git2::Repository::open(path).map_err(|e| e.to_string())?;
    // This backend is attached only to this repository handle; it never changes
    // .git/config or the user's settings. libgit2 fsyncs before making objects
    // read-only (Windows cannot FlushFileBuffers a read-only object handle).
    let config = repo.path().join("vtcode-rollback.config");
    if !config.exists() {
        let mut tmp = tempfile::NamedTempFile::new_in(repo.path()).map_err(|e| e.to_string())?;
        tmp.write_all(b"[core]\nfsyncObjectFiles = true\n")
            .map_err(|e| e.to_string())?;
        tmp.as_file().sync_all().map_err(|e| e.to_string())?;
        tmp.persist_noclobber(&config).map_err(|e| e.to_string())?;
    }
    repo.config()
        .and_then(|mut c| c.add_file(&config, git2::ConfigLevel::App, true))
        .map_err(|e| e.to_string())?;
    Ok(repo)
}

fn sync_object(repo: &git2::Repository, oid: git2::Oid) -> Result<()> {
    let hex = oid.to_string();
    let directory = repo.commondir().join("objects").join(&hex[..2]);
    let path = directory.join(&hex[2..]);
    if cfg!(unix) && path.exists() {
        File::open(path)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        sync_directory(&directory)?;
    }
    Ok(())
}

// Git reuses existing objects, including clean HEAD/index blobs. Hashing rather
// than trusting size/mtime also detects writes with identical timestamps.
pub fn capture(repository: &Path, path: &Path) -> Result<Option<FileState>> {
    capture_with_repo(&self::repository(repository)?, path)
}

pub(super) fn capture_with_repo(repo: &git2::Repository, path: &Path) -> Result<Option<FileState>> {
    capture_with_repo_inner(repo, path, || {})
}

fn capture_with_repo_inner(
    repo: &git2::Repository,
    path: &Path,
    before_read: impl FnOnce(),
) -> Result<Option<FileState>> {
    let Some((file, metadata)) = open_checkpoint_file(path)? else {
        return Ok(None);
    };
    capture_opened(repo, path, file, metadata, before_read).map(Some)
}

#[derive(PartialEq, Eq)]
struct CaptureVersion {
    identity: (u64, u128),
    length: u64,
    mode: u32,
    modified: Option<std::time::SystemTime>,
    changed: Option<(i64, i64)>,
}

impl CaptureVersion {
    fn of(file: &File, metadata: &fs::Metadata) -> Option<Self> {
        Some(Self {
            identity: crate::file_source::file_identity(file, metadata)?,
            length: metadata.len(),
            mode: mode(metadata),
            modified: metadata.modified().ok(),
            changed: change_time(file, metadata),
        })
    }
}

fn change_time(_file: &File, _metadata: &fs::Metadata) -> Option<(i64, i64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((_metadata.ctime(), _metadata.ctime_nsec()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx,
        };
        let mut info = FILE_BASIC_INFO::default();
        // SAFETY: the file owns a live handle and info matches the information class.
        let queried = unsafe {
            GetFileInformationByHandleEx(
                _file.as_raw_handle(),
                FileBasicInfo,
                (&mut info as *mut FILE_BASIC_INFO).cast(),
                std::mem::size_of::<FILE_BASIC_INFO>() as u32,
            )
        };
        (queried != 0).then_some((info.ChangeTime, 0))
    }
    #[cfg(not(any(unix, windows)))]
    None
}

fn capture_opened(
    repo: &git2::Repository,
    path: &Path,
    mut file: File,
    metadata: fs::Metadata,
    before_read: impl FnOnce(),
) -> Result<FileState> {
    let changed = || format!("File changed during checkpoint capture: {}", path.display());
    let before = CaptureVersion::of(&file, &metadata)
        .ok_or_else(|| format!("Cannot identify checkpoint file: {}", path.display()))?;
    before_read();
    let hash = hash_file_handle(&mut file, before.length)
        .map_err(|e| format!("Cannot hash {}: {e}", path.display()))?;
    let odb = repo.odb().map_err(|e| e.to_string())?;
    let pending = if odb.exists(hash) {
        None
    } else {
        // Write raw bytes without Git's worktree clean filters. A known size
        // lets the ODB stream compress directly without a second temporary file.
        let size = usize::try_from(before.length).map_err(|e| e.to_string())?;
        let mut writer = odb
            .writer(size, git2::ObjectType::Blob)
            .map_err(|e| e.to_string())?;
        file.rewind().map_err(|e| e.to_string())?;
        let length = io::copy(&mut (&mut file).take(before.length), &mut writer)
            .map_err(|e| e.to_string())?;
        if length != before.length {
            return Err(changed());
        }
        Some(writer)
    };

    // Only inspect the name to confirm that it still denotes the opened entry.
    // Keep the original handle alive until this comparison to prevent ID reuse.
    #[cfg(unix)]
    let after = fs::symlink_metadata(path)
        .ok()
        .filter(|metadata| metadata.is_file() && !is_link(metadata))
        .and_then(|metadata| CaptureVersion::of(&file, &metadata));
    #[cfg(not(unix))]
    let after = open_checkpoint_file(path)?
        .and_then(|(file, metadata)| CaptureVersion::of(&file, &metadata));
    if after.as_ref() != Some(&before) {
        return Err(changed());
    }

    if let Some(mut writer) = pending {
        let stored = writer.finalize().map_err(|e| e.to_string())?;
        if stored != hash {
            return Err(changed());
        }
        sync_object(repo, stored)?;
    }
    Ok(FileState {
        blob: Blob {
            hash: hash.to_string(),
            length: before.length,
        },
        mode: before.mode,
    })
}

fn hash_file_handle(file: &mut File, length: u64) -> Result<git2::Oid> {
    use std::ffi::{c_int, c_void};

    struct Source<'a> {
        file: &'a mut File,
        error: Option<io::Error>,
    }

    unsafe extern "C" fn read_chunk(data: *mut c_void, bytes: *mut c_void, len: usize) -> c_int {
        // SAFETY: the native call passes our live Source and a writable,
        // initialized slice of its own buffer. Calls are synchronous and serial.
        let source = unsafe { &mut *data.cast::<Source<'_>>() };
        let bytes = unsafe { std::slice::from_raw_parts_mut(bytes.cast::<u8>(), len) };
        match source.file.read_exact(bytes) {
            Ok(()) => 0,
            Err(error) => {
                source.error = Some(error);
                -1
            }
        }
    }

    let mut source = Source { file, error: None };
    let mut oid = libgit2_sys::git_oid {
        id: [0; libgit2_sys::GIT_OID_RAWSZ],
    };
    // SAFETY: both pointers stay valid throughout the call. Libgit2 only calls
    // read_chunk with its own buffer and never retains the source pointer.
    let status = unsafe {
        libgit2_sys::vtcode_git_hash_blob_stream(
            &mut oid,
            length,
            read_chunk,
            (&mut source as *mut Source<'_>).cast(),
        )
    };
    if let Some(error) = source.error {
        return Err(error.to_string());
    }
    if status < 0 {
        return Err(git2::Error::last_error(status).to_string());
    }
    git2::Oid::from_bytes(&oid.id).map_err(|e| e.to_string())
}

pub fn store_bytes(repository: &Path, bytes: &[u8], mode: u32) -> Result<FileState> {
    let repo = self::repository(repository)?;
    let hash = repo.blob(bytes).map_err(|e| e.to_string())?;
    sync_object(&repo, hash)?;
    Ok(FileState {
        blob: Blob {
            hash: hash.to_string(),
            length: bytes.len() as u64,
        },
        mode,
    })
}

pub fn bytes(repository: &Path, blob: &Blob) -> Result<Vec<u8>> {
    if blob.length > super::TEXT_LIMIT {
        return Err("Snapshot is too large for a text merge".into());
    }
    let repo = git2::Repository::open(repository).map_err(|e| e.to_string())?;
    let value = repo
        .find_blob(oid(blob)?)
        .map_err(|e| format!("Missing rollback snapshot: {e}"))?;
    if value.size() as u64 != blob.length || digest(value.content()) != blob.hash {
        return Err("Damaged rollback snapshot; original files have been preserved".into());
    }
    Ok(value.content().to_vec())
}

pub fn copy_verified(repository: &Path, blob: &Blob, output: &mut impl Write) -> Result<()> {
    let repo = git2::Repository::open(repository).map_err(|e| e.to_string())?;
    let value = repo
        .find_blob(oid(blob)?)
        .map_err(|e| format!("Missing rollback snapshot: {e}"))?;
    if value.size() as u64 != blob.length || digest(value.content()) != blob.hash {
        return Err("Damaged rollback snapshot; original files have been preserved".into());
    }
    output.write_all(value.content()).map_err(|e| e.to_string())
}

/// Private refs prevent Git GC from discarding snapshots. The index here is
/// exclusively in memory: the user's real index, HEAD and branches stay intact.
pub fn pin(repository: &Path, reference: &str, files: &BTreeMap<PathBuf, FileState>) -> Result<()> {
    let repo = self::repository(repository)?;
    let mut index = git2::Index::new().map_err(|e| e.to_string())?;
    for (path, state) in files {
        let path = path
            .to_str()
            .ok_or("Checkpoint paths must be UTF-8")?
            .replace('\\', "/");
        let id = oid(&state.blob)?;
        if !repo.odb().map_err(|e| e.to_string())?.exists(id) {
            return Err(format!("Missing rollback snapshot: {id}"));
        }
        index
            .add(&git2::IndexEntry {
                ctime: git2::IndexTime::new(0, 0),
                mtime: git2::IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: if cfg!(unix) && state.mode & 0o111 != 0 {
                    0o100755
                } else {
                    0o100644
                },
                uid: 0,
                gid: 0,
                file_size: state.blob.length.min(u32::MAX as u64) as u32,
                id,
                flags: 0,
                flags_extended: 0,
                path: path.into_bytes(),
            })
            .map_err(|e| e.to_string())?;
    }
    let tree = index.write_tree_to(&repo).map_err(|e| e.to_string())?;
    let tree = repo.find_tree(tree).map_err(|e| e.to_string())?;
    // Retrying after an interrupted journal save may pin the same snapshot or
    // a newer observed snapshot. Never overwrite an earlier checkpoint ref.
    let reference = format!("{reference}/{}", tree.id());
    if let Ok(existing) = repo.find_reference(&reference) {
        if existing.peel_to_tree().map_err(|e| e.to_string())?.id() == tree.id() {
            return Ok(());
        }
        return Err("Checkpoint reference has been modified; history was preserved".into());
    }
    let signature = git2::Signature::now("vtcode checkpoints", "checkpoints@vtcode.local")
        .map_err(|e| e.to_string())?;
    let id = repo
        .commit(
            Some(&reference),
            &signature,
            &signature,
            "vtcode rollback snapshot",
            &tree,
            &[],
        )
        .map_err(|e| e.to_string())?;
    let mut trees = vec![tree.id()];
    tree.walk(git2::TreeWalkMode::PreOrder, |_, entry| {
        if entry.kind() == Some(git2::ObjectType::Tree) {
            trees.push(entry.id());
        }
        git2::TreeWalkResult::Ok
    })
    .map_err(|e| e.to_string())?;
    for tree in trees {
        sync_object(&repo, tree)?;
    }
    sync_object(&repo, id)?;
    let path = repo.commondir().join(&reference);
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .and_then(|f| f.sync_all())
        .map_err(|e| e.to_string())?;
    sync_directory(path.parent().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoint_capture_preserves_binary_bytes_across_hash_chunks() {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init_bare(directory.path().join("git")).unwrap();
        let path = directory.path().join("file.bin");
        let bytes: Vec<_> = (0..(128 * 1024 + 3)).map(|index| (index % 251) as u8).collect();
        fs::write(&path, &bytes).unwrap();

        let expected = git2::Oid::hash_object(git2::ObjectType::Blob, &bytes).unwrap();
        for _ in 0..2 {
            let captured = capture_with_repo(&repo, &path).unwrap().unwrap();
            assert_eq!(captured.blob.hash, expected.to_string());
            assert_eq!(captured.blob.length, bytes.len() as u64);
            assert_eq!(repo.find_blob(expected).unwrap().content(), bytes);
        }
    }

    #[test]
    fn checkpoint_capture_rejects_a_same_size_replacement_before_reading() {
        let directory = tempfile::tempdir().unwrap();
        let repo = git2::Repository::init_bare(directory.path().join("git")).unwrap();
        let path = directory.path().join("file.txt");
        let displaced = directory.path().join("displaced.txt");
        fs::write(&path, "original").unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let replacement = b"external";

        let result = capture_with_repo_inner(&repo, &path, || {
            fs::rename(&path, &displaced).unwrap();
            fs::write(&path, replacement).unwrap();
            File::options()
                .write(true)
                .open(&path)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(modified))
                .unwrap();
        });

        assert!(result.is_err(), "a different file cannot enter the checkpoint");
        let unexpected = git2::Oid::hash_object(git2::ObjectType::Blob, replacement).unwrap();
        assert!(!repo.odb().unwrap().exists(unexpected));
        assert_eq!(fs::read(&path).unwrap(), replacement);
        assert_eq!(fs::read(displaced).unwrap(), b"original");
    }
}
