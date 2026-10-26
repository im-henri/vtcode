//! Durable recovery for file-backed extension editors, independent of guest memory.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecoveryRecord {
    pub extension_id: String,
    pub command_id: String,
    pub original_path: PathBuf,
    pub stamp: crate::extension_documents::DiskStamp,
    #[serde(skip)]
    pub record_path: PathBuf,
}

pub(crate) fn directory(settings_file: &Path) -> PathBuf {
    settings_file
        .parent()
        .unwrap_or(Path::new("."))
        .join("custom-documents")
}

pub(crate) fn read(directory: &Path) -> Vec<RecoveryRecord> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().is_none_or(|extension| extension != "json") {
                return None;
            }
            let metadata = std::fs::symlink_metadata(&path).ok()?;
            if !metadata.is_file() || metadata.len() > 1024 * 1024 {
                return None;
            }
            let mut record: RecoveryRecord =
                serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
            if !record.original_path.is_absolute()
                || record.extension_id.is_empty()
                || record.command_id.is_empty()
            {
                return None;
            }
            let content = std::fs::symlink_metadata(path.with_extension("bin")).ok()?;
            if !content.is_file() || content.file_type().is_symlink() {
                return None;
            }
            record.record_path = path;
            Some(record)
        })
        .collect()
}
