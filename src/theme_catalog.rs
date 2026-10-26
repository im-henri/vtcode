//! Persistent removal of individual installed variants. Packs may share theme
//! files and includes, so pruning a variant must never delete their dependencies.
use std::{collections::BTreeSet, io::Write, path::Path, sync::Mutex};

static REGISTRY_LOCK: Mutex<()> = Mutex::new(());
const FILE_NAME: &str = ".removed-themes.json";
const MAX_BYTES: u64 = 4 * 1024 * 1024;

pub(crate) fn removed(root: &Path) -> Result<BTreeSet<String>, String> {
    let _guard = REGISTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    read(root)
}

fn read(root: &Path) -> Result<BTreeSet<String>, String> {
    use std::io::Read;
    let file = match std::fs::File::open(root.join(FILE_NAME)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
        Err(error) => return Err(format!("Could not read removed themes: {error}")),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("Removed theme list is too large".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("Could not read removed themes: {e}"))
}

fn update(root: &Path, change: impl FnOnce(&mut BTreeSet<String>)) -> Result<(), String> {
    let _guard = REGISTRY_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut keys = read(root)?;
    let before = keys.clone();
    change(&mut keys);
    if keys == before {
        return Ok(());
    }
    std::fs::create_dir_all(root).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(root).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut file, &keys).map_err(|e| e.to_string())?;
    file.write_all(b"\n")
        .and_then(|()| file.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    file.persist(root.join(FILE_NAME))
        .map_err(|e| format!("Could not save removed themes: {}", e.error))?;
    Ok(())
}

pub(crate) fn remove(root: &Path, key: &str) -> Result<(), String> {
    update(root, |keys| {
        keys.insert(key.to_string());
    })
}

/// Restore only variants already removed when this explicit reinstall started.
/// A removal confirmed while an installer was running must remain in effect.
pub(crate) fn restore(root: &Path, keys: &BTreeSet<String>) -> Result<(), String> {
    if keys.is_empty() {
        return Ok(());
    }
    update(root, |removed| removed.retain(|key| !keys.contains(key)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removal_survives_reload_and_reinstall_restores_only_its_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        remove(dir.path(), "light").unwrap();
        let snapshot = removed(dir.path()).unwrap();
        remove(dir.path(), "other").unwrap();
        remove(dir.path(), "light").unwrap();
        assert_eq!(removed(dir.path()).unwrap().len(), 2);
        restore(dir.path(), &snapshot).unwrap();
        assert_eq!(
            removed(dir.path()).unwrap(),
            BTreeSet::from(["other".into()])
        );
    }

    #[test]
    fn invalid_registry_is_preserved_on_failed_removal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        std::fs::write(&path, "broken").unwrap();
        assert!(remove(dir.path(), "theme").is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "broken");
    }
}
