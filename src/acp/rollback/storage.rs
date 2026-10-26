use super::{Blob, FileState, Result};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
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

pub fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|e| e.to_string())?;
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
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
        || relative.components().any(|c| metadata_name(c.as_os_str()))
        || relative
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
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
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if !meta.is_file() || is_link(&meta) {
        return Err(format!("Not a regular file: {}", path.display()));
    }
    let hash = git2::Oid::hash_file(git2::ObjectType::Blob, path).map_err(|e| e.to_string())?;
    if !repo.odb().map_err(|e| e.to_string())?.exists(hash) {
        // blob_path applies .gitattributes/CRLF clean filters; rollback needs
        // the exact disk bytes and must never execute user-provided filters.
        let mut writer = repo.blob_writer(None).map_err(|e| e.to_string())?;
        std::io::copy(
            &mut File::open(path).map_err(|e| e.to_string())?,
            &mut writer,
        )
        .map_err(|e| e.to_string())?;
        let stored = writer.commit().map_err(|e| e.to_string())?;
        if stored != hash {
            return Err(format!(
                "File changed during checkpoint capture: {}",
                path.display()
            ));
        }
        sync_object(repo, stored)?;
    }
    let after = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !after.is_file()
        || is_link(&after)
        || mode(&meta) != mode(&after)
        || meta.len() != after.len()
        || meta.modified().ok() != after.modified().ok()
    {
        return Err(format!(
            "File changed during checkpoint capture: {}",
            path.display()
        ));
    }
    Ok(Some(FileState {
        blob: Blob {
            hash: hash.to_string(),
            length: meta.len(),
        },
        mode: mode(&meta),
    }))
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
