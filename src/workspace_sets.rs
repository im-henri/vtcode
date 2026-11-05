//! Portable named folder sets. Writes merge under a file lock and replace the
//! catalog atomically; a malformed or newer catalog is never overwritten.
use crate::native_paths::NativePath;
use serde::{Deserialize, Serialize};
use std::{
    fs::TryLockError,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const VERSION: u32 = 1;
const MAX_BYTES: u64 = 2 * 1024 * 1024;
pub(crate) const MAX_ROOTS: usize = 128;

#[cfg(test)]
#[path = "workspace_sets_tests.rs"]
mod tests;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "connection", rename_all = "camelCase")]
pub(crate) enum Scope {
    Local,
    Remote(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub(crate) enum Location {
    Local {
        path: NativePath,
        relative: bool,
        windows: bool,
    },
    Remote {
        uri: String,
    },
}

impl Location {
    pub(crate) fn local(path: &Path, base: &Path) -> Self {
        let path = crate::folder_browser::normalized_path(path);
        let relative = path
            .strip_prefix(base)
            .ok()
            .filter(|p| !p.as_os_str().is_empty());
        let relative_text = relative
            .and_then(|path| {
                path.components()
                    .map(|part| part.as_os_str().to_str())
                    .collect::<Option<Vec<_>>>()
            })
            .map(|parts| parts.join("/"));
        if let Some(text) = relative_text {
            return Self::Local {
                path: NativePath::Utf8(text),
                relative: true,
                windows: cfg!(windows),
            };
        }
        let native = relative.unwrap_or(&path);
        Self::Local {
            path: NativePath::from_path(native),
            relative: relative.is_some(),
            windows: cfg!(windows),
        }
    }

    pub(crate) fn local_path(&self, base: &Path) -> io::Result<PathBuf> {
        let Self::Local {
            path,
            relative,
            windows,
        } = self
        else {
            return Err(invalid("This is a remote workspace"));
        };
        if *windows != cfg!(windows) && !(*relative && matches!(path, NativePath::Utf8(_))) {
            return Err(invalid(
                "This workspace contains folders saved on a different operating system",
            ));
        }
        let path = path.to_path_buf().ok_or_else(|| {
            invalid("This folder name uses a different operating system's path encoding")
        })?;
        if *relative {
            if path
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Err(invalid("Invalid portable workspace folder"));
            }
            Ok(base.join(path))
        } else if path.is_absolute() {
            Ok(path)
        } else {
            Err(invalid("Workspace folders must be absolute"))
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SavedSet {
    pub id: u64,
    pub name: String,
    pub scope: Scope,
    pub roots: Vec<Location>,
    pub active: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Catalog {
    version: u32,
    #[serde(default)]
    next_id: u64,
    pub sets: Vec<SavedSet>,
}
impl Default for Catalog {
    fn default() -> Self {
        Self {
            version: VERSION,
            next_id: 1,
            sets: vec![],
        }
    }
}

pub(crate) enum Mutation {
    Save {
        name: String,
        scope: Scope,
        roots: Vec<Location>,
        active: usize,
        replace: Option<u64>,
    },
    Rename {
        id: u64,
        scope: Scope,
        name: String,
    },
    Delete {
        id: u64,
        scope: Scope,
    },
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(crate) fn valid_name(name: &str) -> io::Result<String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(invalid("Enter a workspace name"));
    }
    if name.chars().any(char::is_control) || name.chars().count() > 80 {
        return Err(invalid(
            "Use a name of up to 80 characters without control characters",
        ));
    }
    Ok(name.into())
}

pub(crate) fn load(path: &Path) -> io::Result<Catalog> {
    let (file, metadata) = match crate::fs_ops::open_document_source(path, false) {
        Ok(source) => source,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Catalog::default()),
        Err(error) => return Err(error),
    };
    if metadata.len() > MAX_BYTES {
        return Err(invalid("Saved workspace catalog is too large"));
    }
    let mut data = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_BYTES + 1).read_to_end(&mut data)?;
    if data.len() as u64 > MAX_BYTES {
        return Err(invalid("Saved workspace catalog is too large"));
    }
    let catalog: Catalog = serde_json::from_slice(&data)
        .map_err(|error| invalid(format!("Could not read saved workspaces: {error}")))?;
    validate(&catalog)?;
    Ok(catalog)
}

fn validate(catalog: &Catalog) -> io::Result<()> {
    if catalog.version != VERSION {
        return Err(invalid(
            "These saved workspaces require a different VTCode version",
        ));
    }
    if catalog.sets.len() > 512 {
        return Err(invalid("Too many saved workspaces"));
    }
    let mut ids = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    for set in &catalog.sets {
        if !ids.insert(set.id)
            || set.roots.is_empty()
            || set.roots.len() > MAX_ROOTS
            || set.active >= set.roots.len()
        {
            return Err(invalid("Invalid saved workspace"));
        }
        valid_name(&set.name)?;
        let scope = serde_json::to_string(&set.scope).map_err(io::Error::other)?;
        if !names.insert((scope, set.name.to_lowercase())) {
            return Err(invalid("Duplicate workspace names"));
        }
        if set.roots.iter().any(|root| {
            !matches!(
                (&set.scope, root),
                (Scope::Local, Location::Local { .. })
                    | (Scope::Remote(_), Location::Remote { .. })
            )
        }) {
            return Err(invalid(
                "A workspace cannot mix local and remote folders",
            ));
        }
    }
    Ok(())
}

pub(crate) fn mutate(path: &Path, mutation: Mutation) -> io::Result<Catalog> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("Invalid saved workspace catalog path"))?;
    std::fs::create_dir_all(parent)?;
    let lock = crate::fs_ops::open_lock_file(&path.with_extension("lock"))?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let _lock = vtcode::file_source::FileLock::try_acquire_before(lock, deadline)
        .map_err(|error| match error {
            TryLockError::WouldBlock => {
                io::Error::new(io::ErrorKind::WouldBlock, "Saved workspaces are busy")
            }
            TryLockError::Error(error) => error,
        })?;
    let mut catalog = load(path)?;
    match mutation {
        Mutation::Save {
            name,
            scope,
            roots,
            active,
            replace,
        } => {
            let name = valid_name(&name)?;
            if roots.is_empty() || roots.len() > MAX_ROOTS || active >= roots.len() {
                return Err(invalid("A workspace must contain between 1 and 128 folders"));
            }
            if catalog.sets.iter().any(|set| {
                set.scope == scope
                    && set.name.to_lowercase() == name.to_lowercase()
                    && Some(set.id) != replace
            }) {
                return Err(invalid(
                    "A workspace already has this name. Choose another name or update the existing workspace.",
                ));
            }
            if let Some(id) = replace {
                let set = catalog
                    .sets
                    .iter_mut()
                    .find(|set| set.id == id && set.scope == scope)
                    .ok_or_else(|| invalid("This workspace was removed; refresh the list"))?;
                *set = SavedSet {
                    id,
                    name,
                    scope,
                    roots,
                    active,
                };
            } else {
                if catalog.sets.len() >= 512 {
                    return Err(invalid("Too many saved workspaces"));
                }
                let id = catalog
                    .sets
                    .iter()
                    .map(|set| set.id)
                    .max()
                    .unwrap_or(0)
                    .checked_add(1)
                    .ok_or_else(|| invalid("Saved workspace IDs exhausted"))?
                    .max(catalog.next_id);
                catalog.next_id = id
                    .checked_add(1)
                    .ok_or_else(|| invalid("Saved workspace IDs exhausted"))?;
                catalog.sets.push(SavedSet {
                    id,
                    name,
                    scope,
                    roots,
                    active,
                });
            }
        }
        Mutation::Rename { id, scope, name } => {
            let name = valid_name(&name)?;
            if catalog.sets.iter().any(|set| {
                set.id != id && set.scope == scope && set.name.to_lowercase() == name.to_lowercase()
            }) {
                return Err(invalid("A workspace already has this name"));
            }
            catalog
                .sets
                .iter_mut()
                .find(|set| set.id == id && set.scope == scope)
                .ok_or_else(|| invalid("This workspace was removed; refresh the list"))?
                .name = name;
        }
        Mutation::Delete { id, scope } => catalog
            .sets
            .retain(|set| set.id != id || set.scope != scope),
    }
    validate(&catalog)?;
    let data = serde_json::to_vec_pretty(&catalog).map_err(io::Error::other)?;
    if data.len() as u64 > MAX_BYTES {
        return Err(invalid("Saved workspace catalog is too large"));
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(&data)?;
    temporary.write_all(b"\n")?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(catalog)
}
