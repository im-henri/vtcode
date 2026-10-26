//! One-shot maintenance on the existing worker. Erased paths are not retained
//! as rules, preferences or tombstones after the queued operation completes.
use super::*;

impl Worker {
    pub(super) fn erase_pending(&mut self) {
        let (targets, mut settings) = {
            let mut control = self
                .shared
                .control
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let targets: Vec<_> = control
                .pending
                .iter()
                .filter(|(_, request)| matches!(request, Pending::Erase))
                .take(MAX_PENDING_FILES)
                .map(|(path, _)| path.clone())
                .collect();
            if targets.is_empty() {
                return;
            }
            for path in &targets {
                control.pending.remove(path);
            }
            // A scan requested before this erase must not immediately put the
            // file back. A later explicit scan is a new indexing operation.
            control.rescan = false;
            (targets, control.settings.clone())
        };
        let matches = |path: &Path| {
            targets
                .iter()
                .any(|target| same_index_file_path(target, path))
        };
        self.walk = None;
        self.scans.retain(|scan| !matches(&scan.path));
        if self
            .admission
            .as_ref()
            .is_some_and(|record| matches(&record.path))
        {
            self.admission = None;
        }
        self.validation.retain(|path| !matches(path));
        // Close a partial restore before replacing its checkpoint on Windows.
        let restoring = self.restore.take().is_some();
        let result = erase_checkpoint(
            &self.shared.cache_dir.join("completion-index.jsonl"),
            &self.shared.root,
            self.remote_root_uri.as_deref(),
            &targets,
        );
        if result.is_ok() {
            self.shared
                .status
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .error = None;
            let paths: Vec<_> = self
                .files
                .keys()
                .filter(|path| matches(path))
                .cloned()
                .collect();
            for path in paths {
                self.remove(&path);
            }
            self.recent.retain(|path| !matches(path));
            self.shared
                .known
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|path| !matches(path));
        }
        if restoring {
            // Reopen the filtered (or unchanged on failure) checkpoint without
            // losing records that had not yet been loaded. Opening the reader
            // does not resume processing when indexing is paused/disabled.
            settings.enabled = true;
            self.restore_cache(&settings);
        }
        {
            let mut control = self
                .shared
                .control
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            // Observations queued during this operation belong to the work
            // being erased. Future events remain free to index the file anew.
            control.pending.retain(|path, _| !matches(path));
            control.pending_bytes = control.pending.values().map(Pending::retained_bytes).sum();
        }
        if let Err(error) = result {
            self.error(error);
        }
        let control = self
            .shared
            .control
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let phase = if !control.settings.enabled {
            IndexPhase::Disabled
        } else if control.settings.paused {
            IndexPhase::Paused
        } else if control.cancelled {
            IndexPhase::Cancelled
        } else if !control.pending.is_empty()
            || !self.scans.is_empty()
            || self.restore.is_some()
            || self.admission.is_some()
            || self.oversized_after.is_some()
        {
            IndexPhase::Indexing
        } else if self.limited {
            IndexPhase::Limited
        } else {
            IndexPhase::Idle
        };
        drop(control);
        self.publish(phase, None, true);
    }
}

/// Preserve other records, including records not yet restored into memory.
/// Only a bounded line and the record's path are decoded; terms/examples are
/// copied verbatim. The original checkpoint survives any parse/write failure.
fn erase_checkpoint(
    path: &Path,
    root: &Path,
    remote_root_uri: Option<&str>,
    targets: &[PathBuf],
) -> std::io::Result<()> {
    use std::io::{BufReader, BufWriter, Write};
    #[derive(Deserialize)]
    struct RecordPath<'a> {
        #[serde(borrow)]
        path: &'a serde_json::value::RawValue,
    }
    let input = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let temporary = temporary_path(path);
    let result = (|| -> std::io::Result<()> {
        let mut reader = BufReader::new(input);
        let header = read_cache_line(&mut reader, 64 * 1024)?
            .ok_or_else(|| std::io::Error::other("Completion checkpoint has no header"))?;
        let parsed: CacheHeader = serde_json::from_slice(&header).map_err(std::io::Error::other)?;
        if parsed.version != CACHE_VERSION
            || parsed.root != root
            || parsed.remote_root_uri.as_deref() != remote_root_uri
        {
            return Err(std::io::Error::other(
                "Completion checkpoint does not match this index",
            ));
        }
        let mut changed = false;
        {
            let mut writer = BufWriter::new(std::fs::File::create(&temporary)?);
            writer.write_all(&header)?;
            while let Some(line) = read_cache_line(&mut reader, 2 * 1024 * 1024)? {
                let record: RecordPath<'_> =
                    serde_json::from_slice(&line).map_err(std::io::Error::other)?;
                // Check the borrowed JSON before allocating/unescaping a path.
                if record.path.get().len() > MAX_CACHE_PATH_BYTES * 6 + 2 {
                    return Err(std::io::Error::other(
                        "Completion checkpoint path exceeds limit",
                    ));
                }
                let record: PathBuf =
                    serde_json::from_str(record.path.get()).map_err(std::io::Error::other)?;
                if record.as_os_str().len() > MAX_CACHE_PATH_BYTES {
                    return Err(std::io::Error::other(
                        "Completion checkpoint path exceeds limit",
                    ));
                }
                if targets
                    .iter()
                    .any(|target| same_index_file_path(target, &record))
                {
                    changed = true;
                } else {
                    writer.write_all(&line)?;
                }
            }
            writer.flush()?;
            if changed {
                writer.get_ref().sync_all()?;
            }
        }
        drop(reader);
        if changed {
            atomic_replace_file(&temporary, path)?;
        }
        Ok(())
    })();
    // The rename consumes it on success; all other outcomes discard scratch.
    let _ = std::fs::remove_file(&temporary);
    result
}
