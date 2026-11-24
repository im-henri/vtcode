//! The folders each scope (this machine, or one remote connection) had open
//! when vtcode last ran. A launch without a path reopens them. Writes merge
//! under a file lock, so windows recording other scopes keep their entries.
use crate::workspace_sets::{Location, MAX_ROOTS, Scope};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const VERSION: u32 = 1;
const MAX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ENTRIES: usize = 64;

#[cfg(test)]
#[path = "last_workspaces_tests.rs"]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Entry {
    pub scope: Scope,
    /// Empty when the last window of this scope closed every folder.
    pub roots: Vec<Location>,
    pub active: usize,
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    /// Most recently recorded first.
    entries: Vec<Entry>,
}

pub(crate) fn path() -> PathBuf {
    crate::portable::data_dir().join("last-workspaces.json")
}

fn read(path: &Path) -> io::Result<Vec<Entry>> {
    let (file, metadata) = match crate::fs_ops::open_document_source(path, false) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error),
    };
    if metadata.len() > MAX_BYTES {
        return Err(io::Error::other("Last workspace record is too large"));
    }
    let mut data = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut data)?;
    let file: File = serde_json::from_slice(&data).map_err(io::Error::other)?;
    if file.version != VERSION {
        return Err(io::Error::other(
            "Last workspace record has another version",
        ));
    }
    Ok(file.entries)
}

/// The folders to reopen for `scope`, if any were open and the record is valid.
pub(crate) fn load(path: &Path, scope: &Scope) -> Option<Entry> {
    read(path)
        .ok()?
        .into_iter()
        .find(|entry| entry.scope == *scope)
        .filter(|entry| {
            !entry.roots.is_empty()
                && entry.roots.len() <= MAX_ROOTS
                && entry.active < entry.roots.len()
                && entry.roots.iter().all(|root| {
                    matches!(
                        (&entry.scope, root),
                        (Scope::Local, Location::Local { .. })
                            | (Scope::Remote(_), Location::Remote { .. })
                    )
                })
        })
}

/// Replace the entries of the given scopes, keeping every other scope.
pub(crate) fn record(path: &Path, updates: &[Entry]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Invalid last workspace record path"))?;
    std::fs::create_dir_all(parent)?;
    let lock = crate::fs_ops::open_lock_file(&path.with_extension("lock"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let _lock =
        vtcode::file_source::FileLock::try_acquire_before(lock, deadline).map_err(|error| {
            match error {
                std::fs::TryLockError::WouldBlock => {
                    io::Error::new(io::ErrorKind::WouldBlock, "Last workspace record is busy")
                }
                std::fs::TryLockError::Error(error) => error,
            }
        })?;
    // An unreadable record only holds folders to reopen; start over.
    let mut entries = read(path).unwrap_or_default();
    entries.retain(|entry| !updates.iter().any(|update| update.scope == entry.scope));
    entries.splice(0..0, updates.iter().cloned());
    entries.truncate(MAX_ENTRIES);
    let data = serde_json::to_vec_pretty(&File {
        version: VERSION,
        entries,
    })
    .map_err(io::Error::other)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&data)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}
