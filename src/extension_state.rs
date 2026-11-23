//! Which extensions the user disabled, kept across sessions.
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write;
use std::path::Path;

#[derive(Default, Serialize, Deserialize)]
struct StateFile {
    #[serde(default)]
    disabled: Vec<String>,
}

/// A missing or unreadable file means nothing is disabled.
pub(crate) fn read_disabled(path: &Path) -> HashSet<String> {
    if path.as_os_str().is_empty() {
        return HashSet::new();
    }
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<StateFile>(&bytes).ok())
        .map(|state| state.disabled.into_iter().collect())
        .unwrap_or_default()
}

pub(crate) fn write_disabled(path: &Path, disabled: &HashSet<String>) -> Result<(), String> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    let mut disabled: Vec<String> = disabled.iter().cloned().collect();
    disabled.sort();
    let bytes =
        serde_json::to_vec_pretty(&StateFile { disabled }).map_err(|error| error.to_string())?;
    let parent = path.parent().ok_or("Invalid extension state path")?;
    std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    temporary.write_all(&bytes).map_err(|e| e.to_string())?;
    temporary.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_ids_round_trip_and_missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("extension-state.json");
        assert!(read_disabled(&path).is_empty());
        let ids: HashSet<String> = ["b.ext", "a.ext"].map(String::from).into();
        write_disabled(&path, &ids).unwrap();
        assert_eq!(read_disabled(&path), ids);
        std::fs::write(&path, "not json").unwrap();
        assert!(read_disabled(&path).is_empty());
    }
}
