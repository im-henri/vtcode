use serde::{Deserialize, Serialize};
use crate::fs_ops::open_lock_file;
use std::fs::TryLockError;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use vtcode::file_source::FileLock;

type UndoStack = Vec<crate::editor::UndoSnapshot>;
pub(crate) const MAX_UNDO_ENTRIES: usize = 50;
const UNDO_SEPARATOR: &[u8] = b"\n\x00\x01UNDO_SEP\x01\x00\n";
const RECOVERY_FILE_PREFIX: &str = "tab_";
const RECOVERY_FILE_RANDOM_LEN: usize = 6;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;
const SESSION_WRITER_LOCK: &str = "session-write.lock";
const RECOVERY_READER_LOCK: &str = "session-recovery.lock";

#[path = "session_history.rs"]
mod history;

/// Serialized undo state entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedUndoEntry {
    /// Byte lengths allow bounded streaming reads, including empty snapshots
    /// and text containing the legacy separator. Older manifests omit this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_bytes: Option<usize>,
    pub cursor_line: usize,
    pub cursor_col: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_cursors: Vec<(usize, usize)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_cursor_anchors: Vec<Option<(usize, usize)>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection_anchor: Option<(usize, usize)>,
    pub modified: bool,
}

/// Saved state for a single editor tab.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedTab {
    #[serde(default, with = "crate::native_paths::optional_serde")]
    pub file_path: Option<PathBuf>,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub scroll_offset: usize,
    pub horizontal_scroll: usize,
    pub pinned: bool,
    #[serde(default)]
    pub preview: bool,
    /// Reference to unsaved content file in the portable workspace unsaved/ directory (if any)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unsaved_file: Option<String>,
    /// Transient unsaved text passed between the editor and recovery sidecar.
    #[serde(skip)]
    pub pending_unsaved_content: Option<ropey::Rope>,
    /// Reference to undo history file in the portable workspace unsaved/ directory (if any)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_history_file: Option<String>,
    /// Language override for syntax highlighting
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language_override: Option<String>,
    /// Transient: undo entries loaded from file (not serialized to session.json)
    #[serde(skip)]
    pub undo_entries: Option<UndoStack>,
    /// Transient: redo entries loaded from file (not serialized to session.json)
    #[serde(skip)]
    pub redo_entries: Option<UndoStack>,
    /// Bookmarks (line numbers)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bookmarks: Vec<usize>,
    /// Folded ranges: Vec of (start_line, end_line)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folded_ranges: Vec<(usize, usize)>,
}

/// One uniquely identified tab being restored on a background worker.
#[derive(Debug, Clone)]
pub(crate) struct SessionRestore {
    pub id: u64,
    pub tab: SavedTab,
    pub recovery_lease: RecoveryLease,
}

/// Keeps an older manifest's sidecars readable while its restore is pending.
#[derive(Debug, Clone, Default)]
pub(crate) struct RecoveryLease(Option<Arc<FileLock>>);

impl RecoveryLease {
    pub(crate) fn is_held(&self) -> bool {
        self.0.is_some()
    }
}

impl PartialEq for RecoveryLease {
    fn eq(&self, _other: &Self) -> bool {
        // The lease carries no session data and must not dirty the snapshot.
        true
    }
}

/// A saved editor group (split pane)
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SavedGroup {
    pub tabs: Vec<SavedTab>,
    pub active_tab: usize,
}

/// Full session state for a directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    #[serde(skip)]
    pub(crate) recovery_lease: RecoveryLease,
    #[serde(skip)]
    pub(crate) recovery_incomplete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) remote_checkpoints: Option<crate::remote::RecoveryCheckpoints>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) desks: Option<crate::app_desks::SavedDesks>,
    pub tabs: Vec<SavedTab>,
    pub active_tab: usize,
    pub explorer_width: u16,
    pub show_explorer: bool,
    pub show_minimap: bool,
    // Full layout state
    #[serde(default)]
    pub show_panel: bool,
    #[serde(default = "default_panel_height")]
    pub panel_height: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_sidebar_width: Option<u16>,
    #[serde(default)]
    pub panel_tab: u8,
    #[serde(default)]
    pub show_line_numbers: Option<bool>,
    #[serde(default)]
    pub relative_line_numbers: bool,
    #[serde(default)]
    pub show_indent_guides: Option<bool>,
    #[serde(default)]
    pub show_open_editors: Option<bool>,
    #[serde(default)]
    pub minimap_width: Option<u16>,
    #[serde(default)]
    pub sidebar_right: Option<bool>,
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub sidebar_view: Option<String>,
    #[serde(default)]
    pub sc_graph_collapsed: bool,
    #[serde(default)]
    pub(crate) sc_visibility: crate::source_control_rows::Visibility,
    #[serde(default)]
    pub(crate) repositories: crate::git_repositories::SavedRepositories,
    #[serde(default = "default_sc_graph_height")]
    pub sc_graph_height: u16,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search_history: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub global_search_history: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub global_search_replace_history: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search_include_history: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search_exclude_history: Vec<String>,
    #[serde(default)]
    pub global_search_query: String,
    #[serde(default)]
    pub global_search_replace_query: String,
    #[serde(default)]
    pub search_include_pattern: String,
    #[serde(default)]
    pub search_exclude_pattern: String,
    #[serde(default)]
    pub search_focus_field: u8,
    // Split pane state
    #[serde(default)]
    pub split_active: bool,
    #[serde(default)]
    pub split_tab: usize,
    #[serde(default = "default_true")]
    pub split_vertical: bool,
    #[serde(default = "default_split_ratio")]
    pub split_ratio: f32,
    /// Preserve nested pane directions, ordering, and divider positions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_tree: Option<crate::app::SplitNode>,
    /// Editor groups for multi-pane sessions
    #[serde(default)]
    pub groups: Vec<SavedGroup>,
    /// Which group was focused
    #[serde(default)]
    pub active_group: usize,
    #[serde(default)]
    pub zen_mode: bool,
    #[serde(default = "default_true")]
    pub show_activity_bar: bool,
    #[serde(default = "default_true")]
    pub show_tab_bar: bool,
    #[serde(default = "default_true")]
    pub show_status_bar: bool,
}

fn default_panel_height() -> u16 { 10 }
fn default_sc_graph_height() -> u16 { 10 }
fn default_true() -> bool { true }
fn default_split_ratio() -> f32 { 0.5 }

/// Path to the session file for a given directory.
pub(crate) fn session_path(project_root: &Path) -> PathBuf {
    crate::portable::workspace_dir(project_root).join("session.json")
}

/// Path to the unsaved content directory.
pub(crate) fn unsaved_dir(project_root: &Path) -> PathBuf {
    crate::portable::workspace_dir(project_root).join("unsaved")
}

fn is_recovery_filename(name: &str) -> bool {
    // Both the current and legacy writers use the tab_ prefix and portable
    // names. Reject paths, alternate data streams and device names on load.
    name.starts_with(RECOVERY_FILE_PREFIX)
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        && matches!(name.rsplit_once('.'), Some((_, "txt" | "json" | "bin")))
}

fn is_generated_recovery_filename(name: &str) -> bool {
    let Some(name) = name.strip_prefix(RECOVERY_FILE_PREFIX) else {
        return false;
    };
    [".txt", "_history.json", "_history.bin"]
        .into_iter()
        .any(|suffix| {
            name.strip_suffix(suffix).is_some_and(|random| {
                random.len() == RECOVERY_FILE_RANDOM_LEN
                    && random.bytes().all(|byte| byte.is_ascii_alphanumeric())
            })
        })
}

fn recovery_filename(name: &str) -> io::Result<&str> {
    if is_recovery_filename(name) {
        Ok(name)
    } else {
        Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid recovery filename"))
    }
}

fn require_recovery_directory(path: &Path) -> io::Result<()> {
    if path.symlink_metadata()?.is_dir() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotADirectory,
            "Invalid session recovery directory",
        ))
    }
}

fn recovery_path(directory: &Path, filename: &str) -> io::Result<PathBuf> {
    let name = recovery_filename(filename)?;
    require_recovery_directory(directory)?;
    Ok(directory.join(name))
}

fn open_recovery_file(path: &Path) -> io::Result<std::fs::File> {
    vtcode::file_source::open_regular_file_no_follow(path).map(|(file, _)| file)
}

fn acquire_recovery_lease(manifest: &Path) -> io::Result<Arc<FileLock>> {
    let path = manifest
        .parent()
        .expect("session storage directory")
        .join(RECOVERY_READER_LOCK);
    let file = match open_recovery_file(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound
            && manifest.symlink_metadata().is_ok_and(|metadata| metadata.is_file()) => {
            // Older manifests predate the lock. Create it only when there is
            // an existing session to migrate, including on startup.
            open_lock_file(&path)?
        }
        Err(error) => return Err(error),
    };
    FileLock::acquire_shared(file).map(Arc::new)
}

/// Write a new recovery file without overwriting one referenced by the current
/// manifest or a background restore. The manifest is committed only after all
/// of its recovery files have been written successfully.
fn save_recovery_file(
    project_root: &Path,
    suffix: &str,
    write: impl FnOnce(&mut std::io::BufWriter<&mut std::fs::File>) -> std::io::Result<()>,
) -> std::io::Result<String> {
    let dir = unsaved_dir(project_root);
    std::fs::create_dir_all(&dir)?;
    require_recovery_directory(&dir)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(RECOVERY_FILE_PREFIX)
        .rand_bytes(RECOVERY_FILE_RANDOM_LEN)
        .suffix(suffix)
        .tempfile_in(&dir)?;
    {
        let mut writer = std::io::BufWriter::with_capacity(64 * 1024, temporary.as_file_mut());
        write(&mut writer)?;
        writer.flush()?;
    }
    temporary.as_file().sync_all()?;
    let (_, path) = temporary.keep().map_err(|error| error.error)?;
    Ok(path.file_name().expect("temporary recovery filename").to_string_lossy().into_owned())
}

/// Roll back only this transaction's new sidecars when any later write fails.
/// Existing committed recovery (and files created by other instances) is kept.
struct RecoveryWrites<'a> {
    root: &'a Path,
    created: Vec<String>,
    committed: bool,
}

impl<'a> RecoveryWrites<'a> {
    fn new(root: &'a Path) -> Self { Self { root, created: Vec::new(), committed: false } }

    fn write(&mut self, suffix: &str,
        write: impl FnOnce(&mut std::io::BufWriter<&mut std::fs::File>) -> std::io::Result<()>,
    ) -> std::io::Result<String> {
        let name = save_recovery_file(self.root, suffix, write)?;
        self.created.push(name.clone());
        Ok(name)
    }
}

impl Drop for RecoveryWrites<'_> {
    fn drop(&mut self) {
        if !self.committed && !self.created.is_empty() {
            let dir = unsaved_dir(self.root);
            if require_recovery_directory(&dir).is_err() {
                return;
            }
            for filename in &self.created {
                let _ = std::fs::remove_file(dir.join(filename));
            }
        }
    }
}

/// Text and checksum are read together, once per distinct recovery sidecar.
#[derive(Clone)]
struct RecoveredText {
    content: ropey::Rope,
    sha256: [u8; 32],
}

fn load_unsaved_content(project_root: &Path, filename: &str) -> Option<RecoveredText> {
    let load = || -> io::Result<_> {
        let path = recovery_path(&unsaved_dir(project_root), filename)?;
        let file = open_recovery_file(&path)?;
        let (content, sha256) = history::read_base(std::io::BufReader::with_capacity(64 * 1024, file))?;
        Ok(RecoveredText { content, sha256 })
    };
    load().map_err(|e| {
        if e.kind() != std::io::ErrorKind::NotFound {
            log_warn!("failed to read unsaved content {filename:?}: {e}");
        }
    }).ok()
}

/// Remove obsolete sidecars only after the new manifest is committed. Pending
/// restores retain their original filenames, including undo content files.
fn cleanup_unsaved_dir(project_root: &Path, retained: &std::collections::HashSet<String>) {
    let dir = unsaved_dir(project_root);
    if require_recovery_directory(&dir).is_ok()
        && let Ok(entries) = std::fs::read_dir(&dir)
    {
        for entry in entries.flatten() {
            if entry.file_name().to_str().is_some_and(|name| {
                is_generated_recovery_filename(name) && !retained.contains(name)
            }) && entry.file_type().is_ok_and(|kind| kind.is_file())
                && let Err(error) = std::fs::remove_file(entry.path())
            {
                log_warn!("failed to remove obsolete session recovery file '{}': {error}", entry.path().display());
            }
        }
    }
}

/// The durable unsaved text can serve as history's initial snapshot.
struct RecoveryBase<'a> {
    filename: &'a str,
    content: &'a ropey::Rope,
    sha256: [u8; 32],
}

/// Save undo history to a separate file. Returns the filename.
fn save_undo_history(
    writes: &mut RecoveryWrites<'_>,
    undo_entries: &[crate::editor::UndoSnapshot],
    redo_entries: &[crate::editor::UndoSnapshot],
    retained: &mut std::collections::HashSet<String>,
    base: Option<RecoveryBase<'_>>,
) -> std::io::Result<String> {
    let undo = &undo_entries[undo_entries.len().saturating_sub(MAX_UNDO_ENTRIES)..];
    let redo = &redo_entries[redo_entries.len().saturating_sub(MAX_UNDO_ENTRIES)..];
    let metadata = |entries: &[crate::editor::UndoSnapshot]| entries.iter()
        .map(|(content, cl, cc, ec, eca, sa, modified)| SavedUndoEntry {
            content_bytes: Some(content.len_bytes()),
            cursor_line: *cl, cursor_col: *cc,
            extra_cursors: ec.clone(), extra_cursor_anchors: eca.clone(), selection_anchor: *sa,
            modified: *modified,
        }).collect();
    let mut digest = [0; 32];
    let contents = writes.write("_history.bin", |writer| {
        digest = if let Some(base) = &base {
            history::write_with_base(writer, undo, redo, Some(base.content))?
        } else {
            history::write(writer, undo, redo)?
        };
        Ok(())
    })?;
    let history = SavedUndoHistory {
        version: if base.is_some() { 3 } else { 2 },
        undo: metadata(undo),
        redo: metadata(redo),
        contents_file: Some(contents.clone()),
        contents_sha256: Some(digest),
        base_file: base.as_ref().map(|base| base.filename.to_owned()),
        base_sha256: base.as_ref().map(|base| base.sha256),
        undo_contents_file: String::new(),
        redo_contents_file: String::new(),
    };
    let filename = writes.write("_history.json", |writer| {
        serde_json::to_writer(&mut *writer, &history)?;
        writer.flush()?;
        check_manifest_size(writer.get_ref().metadata()?.len())
    })?;
    // These sidecars were just durably written. Do not reopen and parse them
    // to rediscover their names (particularly costly on shared storage).
    retained.extend([filename.clone(), contents]);
    if let Some(base) = base { retained.insert(base.filename.to_owned()); }
    Ok(filename)
}

#[cfg(test)]
fn save_test_undo_history(
    root: &Path, undo: &[crate::editor::UndoSnapshot], redo: &[crate::editor::UndoSnapshot],
    retained: &mut std::collections::HashSet<String>,
) -> std::io::Result<String> {
    let mut writes = RecoveryWrites::new(root);
    let name = save_undo_history(&mut writes, undo, redo, retained, None)?;
    writes.committed = true;
    Ok(name)
}

#[derive(Serialize, Deserialize)]
struct SavedUndoHistory {
    #[serde(default)]
    version: u32,
    undo: Vec<SavedUndoEntry>,
    redo: Vec<SavedUndoEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    contents_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    contents_sha256: Option<[u8; 32]>,
    /// Version 3 uses the unsaved text sidecar as its immutable initial state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    base_sha256: Option<[u8; 32]>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    undo_contents_file: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    redo_contents_file: String,
}

impl SavedUndoHistory {
    fn content_files(&self) -> std::io::Result<Vec<&str>> {
        let invalid = || std::io::Error::new(
            std::io::ErrorKind::InvalidData, "invalid session undo recovery manifest");
        let files = match self.version {
            0 => vec![self.undo_contents_file.as_str(), self.redo_contents_file.as_str()],
            2 | 3 if self.undo.len() <= MAX_UNDO_ENTRIES
                && self.redo.len() <= MAX_UNDO_ENTRIES
                && self.contents_sha256.is_some()
                && self.undo.iter().chain(&self.redo)
                    .all(|entry| entry.content_bytes.is_some()) => {
                let mut files = vec![self.contents_file.as_deref().ok_or_else(invalid)?];
                if self.version == 3 {
                    if self.base_sha256.is_none() { return Err(invalid()); }
                    files.push(self.base_file.as_deref().ok_or_else(invalid)?);
                }
                files
            }
            _ => return Err(invalid()),
        };
        for file in &files {
            recovery_filename(file)?;
        }
        Ok(files)
    }
}

fn load_undo_stack(path: &Path, metadata: Vec<SavedUndoEntry>) -> std::io::Result<UndoStack> {
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, open_recovery_file(path)?);
    let invalid = || std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid undo recovery content");
    let mut contents = Vec::with_capacity(metadata.len());
    if metadata.iter().all(|entry| entry.content_bytes.is_some()) {
        for (index, entry) in metadata.iter().enumerate() {
            if index > 0 {
                let mut separator = [0; UNDO_SEPARATOR.len()];
                reader.read_exact(&mut separator)?;
                if separator != UNDO_SEPARATOR { return Err(invalid()); }
            }
            let len = entry.content_bytes.unwrap();
            let content = ropey::Rope::from_reader(reader.by_ref().take(len as u64))?;
            if content.len_bytes() != len { return Err(invalid()); }
            contents.push(content);
        }
        if reader.read(&mut [0])? != 0 { return Err(invalid()); }
    } else {
        // Existing installations have separator-delimited history without
        // lengths. Keep reading it, but never turn a missing file into empty
        // undo states that would erase recovered text.
        if metadata.iter().any(|entry| entry.content_bytes.is_some()) { return Err(invalid()); }
        let mut text = String::new();
        reader.read_to_string(&mut text)?;
        contents.extend(text.split(std::str::from_utf8(UNDO_SEPARATOR).unwrap()).map(ropey::Rope::from_str));
        if contents.len() != metadata.len() { return Err(invalid()); }
    }
    Ok(contents.into_iter().zip(metadata).map(|(content, entry)| (
        content, entry.cursor_line, entry.cursor_col, entry.extra_cursors,
        entry.extra_cursor_anchors, entry.selection_anchor, entry.modified,
    )).collect())
}

/// Load undo history from files. Returns (undo_entries, redo_entries).
#[cfg(test)]
fn load_undo_history(
    project_root: &Path,
    filename: &str,
) -> Option<(UndoStack, UndoStack)> {
    load_undo_history_cached(project_root, filename, &mut std::collections::HashMap::new())
}

fn load_undo_history_cached(
    project_root: &Path,
    filename: &str,
    contents: &mut std::collections::HashMap<String, Option<RecoveredText>>,
) -> Option<(UndoStack, UndoStack)> {
    let dir = unsaved_dir(project_root);
    let mut load = || -> std::io::Result<_> {
        let path = recovery_path(&dir, filename)?;
        let parsed: SavedUndoHistory = read_manifest(&path)?;
        parsed.content_files()?;
        if parsed.version == 2 || parsed.version == 3 {
            let base = if parsed.version == 3 {
                let name = parsed.base_file.as_ref().unwrap();
                let content = contents.entry(name.clone())
                    .or_insert_with(|| load_unsaved_content(project_root, name)).clone()
                    .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing undo recovery base"))?;
                if content.sha256 != parsed.base_sha256.unwrap() {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "undo recovery base checksum mismatch"));
                }
                Some(content.content)
            } else { None };
            let file = open_recovery_file(&dir.join(parsed.contents_file.unwrap()))?;
            let reader = std::io::BufReader::with_capacity(64 * 1024, file);
            if base.is_some() {
                history::read_with_base(reader, parsed.undo, parsed.redo, parsed.contents_sha256.unwrap(), base)
            } else {
                history::read(reader, parsed.undo, parsed.redo, parsed.contents_sha256.unwrap())
            }
        } else {
            Ok((load_undo_stack(&dir.join(parsed.undo_contents_file), parsed.undo)?,
                load_undo_stack(&dir.join(parsed.redo_contents_file), parsed.redo)?))
        }
    };
    load().map_err(|error| {
        if error.kind() != io::ErrorKind::NotFound {
            log_warn!("failed to read undo history {filename:?}: {error}");
        }
    }).ok()
}

fn check_manifest_size(size: u64) -> io::Result<()> {
    if size > MAX_MANIFEST_BYTES {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Session manifest exceeds 16 MiB",
        ))
    } else {
        Ok(())
    }
}

fn read_manifest<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<T> {
    let (file, metadata) = vtcode::file_source::open_regular_file_no_follow(path)?;
    check_manifest_size(metadata.len())?;
    let mut data = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_MANIFEST_BYTES + 1).read_to_end(&mut data)?;
    check_manifest_size(data.len() as u64)?;
    serde_json::from_slice(&data).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::File::options()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(path)?
        .sync_all()
}

/// Save session state to disk.
pub(crate) fn save_session(project_root: &Path, session: &Session) -> std::io::Result<()> {
    if session.recovery_incomplete {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Some recovery files could not be read. The previous session backup was preserved.",
        ));
    }
    let path = session_path(project_root);
    let parent = path.parent().expect("session storage directory");
    let recovery_dir = parent.join("unsaved");
    std::fs::create_dir_all(parent)?;
    let writer = open_lock_file(&parent.join(SESSION_WRITER_LOCK))?;
    let _writer = FileLock::try_acquire(writer).map_err(|error| match error {
        TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "Another VTCode window is saving this workspace session",
        ),
        TryLockError::Error(error) => error,
    })?;
    // Restore can fail while the manifest still contains the only references
    // to unsaved work. Preserve that state even if the window opened empty.
    if let Err(error) = read_manifest::<Session>(&path)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(io::Error::new(
            error.kind(),
            format!("Cannot replace session manifest '{}': {error}", path.display()),
        ));
    }
    let mut writes = RecoveryWrites::new(project_root);
    let mut session_out = session.clone();
    // Keep every referenced file until the new manifest is durable. This also
    // handles a folder switch while recovery hydration is still in progress.
    let mut retained = std::collections::HashSet::new();
    let document_ids:std::collections::HashMap<_,_>=session_out.desks.as_ref().map(|d|d.documents.iter().map(|&(pane,tab,id)|((pane,tab),id)).collect()).unwrap_or_default();
    let mut documents:std::collections::HashMap<u64,(Option<String>,Option<String>)>=std::collections::HashMap::new();
    let active_group=session_out.active_group;
    for (pane,index,tab) in session_out.tabs.iter_mut().enumerate().map(|(i,t)|(active_group,i,t)).chain(
        session_out.groups.iter_mut().enumerate().flat_map(|(pane,group)|group.tabs.iter_mut().enumerate().map(move |(i,t)|(pane,i,t))),
    ) {
        let document=document_ids.get(&(pane,index)).copied();
        if let Some((text,history))=document.and_then(|id|documents.get(&id)) {
            tab.unsaved_file=text.clone();tab.undo_history_file=history.clone();
            tab.pending_unsaved_content=None;tab.undo_entries=None;tab.redo_entries=None;
            continue;
        }
        let content = tab.pending_unsaved_content.take();
        let mut base_digest = [0; 32];
        let wrote_unsaved = if let Some(content) = &content {
            tab.unsaved_file = Some(writes.write(".txt", |writer| {
                base_digest = history::write_base(writer, content)?;
                Ok(())
            })?);
            true
        } else { false };
        let mut wrote_history = false;
        if let (Some(undo), Some(redo)) = (&tab.undo_entries, &tab.redo_entries) {
            if !undo.is_empty() || !redo.is_empty() {
                let base = content.as_ref().map(|content| RecoveryBase {
                    filename: tab.unsaved_file.as_deref().unwrap(), content, sha256: base_digest,
                });
                tab.undo_history_file = Some(save_undo_history(&mut writes, undo, redo, &mut retained, base)?);
                wrote_history = true;
            } else {
                tab.undo_history_file = None;
            }
        }
        tab.undo_entries = None;
        tab.redo_entries = None;
        if let Some(id)=document {documents.insert(id,(tab.unsaved_file.clone(),tab.undo_history_file.clone()));}
        if let Some(filename) = &tab.unsaved_file {
            if !wrote_unsaved { open_recovery_file(&recovery_path(&recovery_dir, filename)?)?; }
            retained.insert(filename.clone());
        }
        if let Some(filename) = &tab.undo_history_file {
            if wrote_history { continue; }
            let history: SavedUndoHistory = read_manifest(&recovery_path(&recovery_dir, filename)?)?;
            retained.insert(filename.clone());
            for content_file in history.content_files()? {
                open_recovery_file(&recovery_dir.join(content_file))?;
                retained.insert(content_file.to_string());
            }
        }
    }
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    {
        let mut writer = io::BufWriter::new(temporary.as_file_mut());
        serde_json::to_writer_pretty(&mut writer, &session_out)?;
        writer.flush()?;
    }
    check_manifest_size(temporary.as_file().metadata()?.len())?;
    temporary.as_file().sync_all()?;
    // Make new sidecar names durable before publishing their references.
    #[cfg(unix)]
    if !writes.created.is_empty() {
        sync_directory(&recovery_dir)?;
    }
    temporary.persist(&path).map_err(|error| error.error)?;
    writes.committed = true;
    // A crash can otherwise restore the old manifest after its sidecars have
    // been pruned. Keep both generations if syncing the rename fails.
    #[cfg(unix)]
    sync_directory(parent)?;
    // A reader may hold an older manifest while its file load is queued.
    // Publish the new manifest immediately and defer pruning if needed.
    if let Ok(readers) = open_lock_file(&parent.join(RECOVERY_READER_LOCK))
        && let Ok(_cleanup_lock) = FileLock::try_acquire(readers)
    {
        cleanup_unsaved_dir(project_root, &retained);
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn load_session(project_root: &Path) -> Option<Session> {
    load_session_with_recovery_status(project_root).0
}

/// Load the manifest and retain its lease when it cannot be read. Recovery
/// payloads are hydrated separately by file-load workers.
pub(crate) fn load_session_with_recovery_status(
    project_root: &Path,
) -> (Option<Session>, Option<RecoveryLease>) {
    load_session_inner(project_root, || {})
}

fn load_session_inner(
    project_root: &Path,
    before_manifest: impl FnOnce(),
) -> (Option<Session>, Option<RecoveryLease>) {
    let path = session_path(project_root);
    // Pin before opening the manifest so cleanup cannot retire its files
    // between the atomic read and the eventual background restore.
    let mut lease = RecoveryLease(acquire_recovery_lease(&path).ok());
    before_manifest();
    let manifest = read_manifest::<Session>(&path).and_then(|session| {
        if lease.is_held() || !session.has_recovery_sidecars() {
            return Ok(session);
        }
        // Another window can publish the first manifest after the initial
        // lease attempt. Reload under the lease to pin the current sidecars.
        lease.0 = Some(acquire_recovery_lease(&path).map_err(|error| {
            io::Error::other(format!("Cannot lease session recovery files: {error}"))
        })?);
        read_manifest(&path)
    });
    let mut session = match manifest {
        Ok(session) => session,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return (None, None),
        Err(error) => {
            log_warn!("failed to read session file '{}': {error}", path.display());
            return (None, Some(lease));
        }
    };
    if session.has_recovery_sidecars() {
        session.recovery_lease = lease;
    }
    (Some(session), None)
}

impl Session {
    pub(crate) fn has_recovery_sidecars(&self) -> bool {
        self.tabs.iter().chain(self.groups.iter().flat_map(|group| &group.tabs))
            .any(SavedTab::has_recovery_sidecars)
    }
}

impl SavedTab {
    pub(crate) fn has_recovery_sidecars(&self) -> bool {
        self.unsaved_file.is_some() || self.undo_history_file.is_some()
    }

    /// Check after a restore worker has attempted to hydrate the sidecars.
    pub(crate) fn recovery_read_failed(&self) -> bool {
        (self.unsaved_file.is_some() && self.pending_unsaved_content.is_none())
            || (self.undo_history_file.is_some()
                && (self.undo_entries.is_none() || self.redo_entries.is_none()))
    }
}

/// Resolve a tab's unsaved buffer and undo/redo data. Call only from a worker.
pub(crate) fn hydrate_saved_tab(project_root: &Path, tab: &mut SavedTab) {
    hydrate_saved_tabs(project_root, std::iter::once(tab));
}

/// One restore job owns this short-lived cache. Duplicate views of a saved
/// document reference the same sidecars: decode them once and clone only Rope
/// handles/history metadata. No global cache keeps closed recovery files alive.
pub(crate) fn hydrate_saved_tabs<'a>(
    project_root: &Path,
    tabs: impl IntoIterator<Item = &'a mut SavedTab>,
) {
    let mut contents = std::collections::HashMap::new();
    let mut histories = std::collections::HashMap::new();
    for tab in tabs {
        if tab.pending_unsaved_content.is_none() && let Some(filename) = &tab.unsaved_file {
            tab.pending_unsaved_content = contents.entry(filename.clone())
                .or_insert_with(|| load_unsaved_content(project_root, filename))
                .as_ref().map(|text| text.content.clone());
        }
        if tab.undo_entries.is_none() && tab.redo_entries.is_none()
            && let Some(filename) = &tab.undo_history_file
            && let Some((undo, redo)) = histories.entry(filename.clone())
                .or_insert_with(|| load_undo_history_cached(project_root, filename, &mut contents))
        {
            tab.undo_entries = Some(undo.clone());
            tab.redo_entries = Some(redo.clone());
        }
    }
}

/// Recovery views can share a decoded sidecar. Compare each distinct Rope
/// against disk once, then give every equal view the live disk checkpoint.
pub(crate) fn share_recovered_disk_snapshot<'a>(
    tabs: impl IntoIterator<Item = &'a mut SavedTab>,
    disk: &ropey::Rope,
) {
    let mut checked: Vec<(ropey::Rope, bool)> = Vec::new();
    for tab in tabs {
        let Some(content) = &mut tab.pending_unsaved_content else { continue; };
        let equal = if let Some((_, equal)) = checked.iter().find(|(source, _)| source.is_instance(content)) {
            *equal
        } else {
            let equal = crate::rope_changes::equal(content, disk);
            checked.push((content.clone(), equal));
            equal
        };
        if equal { *content = disk.clone(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
    use std::path::Path;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_project_root() -> std::path::PathBuf {
        // Windows can return the same timestamp to concurrent tests. Keep
        // recovery/permission fixtures in distinct directories regardless.
        static NEXT_ROOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = NEXT_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        path.push(format!("vtcode-session-test-{}-{stamp}-{sequence}", std::process::id()));
        path
    }

    #[test]
    fn session_path_constructs_correctly() {
        let root = Path::new("/project");
        let path = session_path(root);
        assert!(path.ends_with("session.json"));
        assert!(path.to_string_lossy().contains(".vtcode"));
    }

    #[test]
    fn unsaved_dir_constructs_correctly() {
        let root = Path::new("/project");
        let dir = unsaved_dir(root);
        assert!(dir.ends_with("unsaved"));
        assert!(dir.to_string_lossy().contains(".vtcode"));
    }

    #[test]
    fn save_session_reports_recovery_write_failure_without_replacing_manifest() {
        let project_root = unique_temp_project_root();
        let vtcode_dir = crate::portable::workspace_dir(&project_root);
        std::fs::create_dir_all(&vtcode_dir).unwrap();
        save_session(&project_root, &Session::default()).unwrap();
        let previous_manifest = std::fs::read(session_path(&project_root)).unwrap();
        std::fs::write(vtcode_dir.join("unsaved"), "blocking-file").unwrap();

        let session = Session {
            tabs: vec![SavedTab {
                file_path: Some("dummy.txt".into()),
                cursor_line: 0,
                cursor_col: 0,
                scroll_offset: 0,
                horizontal_scroll: 0,
                pinned: false,
                preview: false,
                unsaved_file: None,
                pending_unsaved_content: Some("unsaved edits".into()),
                undo_history_file: None,
                language_override: None,
                undo_entries: None,
                redo_entries: None,
                bookmarks: Vec::new(),
                folded_ranges: Vec::new(),
            }],
            ..Session::default()
        };

        assert!(save_session(&project_root, &session).is_err());
        assert_eq!(std::fs::read(session_path(&project_root)).unwrap(), previous_manifest);
        assert_eq!(session.tabs[0].pending_unsaved_content.as_ref().unwrap(), "unsaved edits");

        std::fs::remove_dir_all(vtcode_dir).unwrap();
        let _ = std::fs::remove_dir_all(&project_root); // Error OK: dir may not exist
    }

    pub(super) fn recovery_test_tab(content: &str) -> SavedTab {
        SavedTab {
            file_path: Some("test.txt".into()),
            cursor_line: 0, cursor_col: 0, scroll_offset: 0,
            horizontal_scroll: 0, pinned: false, preview: false,
            unsaved_file: None,
            pending_unsaved_content: Some(content.into()),
            undo_history_file: None, language_override: None,
            undo_entries: Some(vec![("before".into(), 0, 0, vec![], vec![], None, false)]),
            redo_entries: Some(vec![("after".into(), 0, 0, vec![], vec![], None, true)]),
            bookmarks: Vec::new(), folded_ranges: Vec::new(),
        }
    }

    #[test]
    fn recovery_manifest_rejects_paths_outside_its_sidecar_directory() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path();
        save_session(project, &Session {
            tabs: vec![recovery_test_tab("original")], ..Session::default()
        }).unwrap();
        let manifest = fs::read(session_path(project)).unwrap();
        let mut session = load_session(project).unwrap();
        let dir = unsaved_dir(project);
        let parent = dir.parent().unwrap();
        let tab = &mut session.tabs[0];
        let text = tab.unsaved_file.clone().unwrap();
        let history = tab.undo_history_file.clone().unwrap();
        fs::rename(dir.join(&text), parent.join(&text)).unwrap();
        fs::rename(dir.join(&history), parent.join(&history)).unwrap();
        tab.unsaved_file = Some(parent.join(&text).to_str().unwrap().to_owned());
        tab.undo_history_file = Some(format!("../{history}"));
        hydrate_saved_tab(project, tab);
        assert!(tab.pending_unsaved_content.is_none());
        assert!(tab.undo_entries.is_none());
        assert_eq!(save_session(project, &session).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(session_path(project)).unwrap(), manifest);
        fs::remove_dir_all(parent).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn recovery_ignores_links_and_special_files_without_blocking() {
        use std::os::unix::{ffi::OsStrExt, fs::symlink};

        let root = tempfile::tempdir().unwrap();
        let project = root.path();
        let dir = unsaved_dir(project);
        fs::create_dir_all(&dir).unwrap();
        let outside = root.path().join("external.txt");
        fs::write(&outside, "outside state").unwrap();
        symlink(outside, dir.join("tab_link.txt")).unwrap();
        let fifo = std::ffi::CString::new(dir.join("tab_fifo.txt").as_os_str().as_bytes()).unwrap();
        // SAFETY: The path is NUL-terminated and inside the test recovery directory.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        for name in ["tab_link.txt", "tab_fifo.txt"] {
            assert!(load_unsaved_content(project, name).is_none());
            assert!(load_undo_history(project, name).is_none());
        }
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn recovery_cleanup_preserves_unrelated_files() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path();
        let dir = unsaved_dir(project);
        save_session(project, &Session {
            tabs: vec![recovery_test_tab("old")], ..Session::default()
        }).unwrap();
        let unrelated = ["keep.txt", "tab_project_notes.txt", "tab_aBc123_report.bin"];
        for name in unrelated {
            fs::write(dir.join(name), "unrelated file").unwrap();
        }
        save_session(project, &Session::default()).unwrap();
        assert_eq!(fs::read_dir(&dir).unwrap().count(), unrelated.len());
        for name in unrelated {
            assert_eq!(fs::read_to_string(dir.join(name)).unwrap(), "unrelated file");
        }
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn linked_recovery_directory_cannot_redirect_reads_writes_or_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let project = root.path();
        let dir = unsaved_dir(project);
        fs::create_dir_all(dir.parent().unwrap()).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), &dir).unwrap();
        #[cfg(windows)]
        if let Err(error) = std::os::windows::fs::symlink_dir(outside.path(), &dir) {
            if error.raw_os_error() == Some(1314) {
                return;
            }
            panic!("could not create a recovery directory link: {error}");
        }
        let name = "tab_aBc123.txt";
        let external_file = outside.path().join(name);
        fs::write(&external_file, "outside recovery storage").unwrap();

        assert!(load_unsaved_content(project, name).is_none());
        save_session(project, &Session::default()).unwrap();
        let manifest = fs::read(session_path(project)).unwrap();
        let dirty = Session {
            tabs: vec![recovery_test_tab("private draft")],
            ..Session::default()
        };
        assert_eq!(
            save_session(project, &dirty).unwrap_err().kind(),
            io::ErrorKind::NotADirectory
        );
        assert_eq!(fs::read(session_path(project)).unwrap(), manifest);
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
        assert_eq!(
            fs::read_to_string(external_file).unwrap(),
            "outside recovery storage"
        );

        #[cfg(unix)]
        fs::remove_file(&dir).unwrap();
        #[cfg(windows)]
        fs::remove_dir(&dir).unwrap();
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn oversized_manifest_cannot_replace_the_saved_session() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path();
        save_session(project, &Session::default()).unwrap();
        let path = session_path(project);
        let previous = fs::read(&path).unwrap();
        let oversized = Session {
            global_search_query: "a".repeat(MAX_MANIFEST_BYTES as usize),
            ..Session::default()
        };
        assert_eq!(save_session(project, &oversized).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&path).unwrap(), previous);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn restore_rejects_oversized_session_and_undo_manifests() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path();
        let dir = unsaved_dir(project);
        fs::create_dir_all(&dir).unwrap();
        let filename = "tab_123456_history.json";
        for path in [session_path(project), dir.join(filename)] {
            File::create(path).unwrap().set_len(MAX_MANIFEST_BYTES + 1).unwrap();
        }
        assert!(load_session(project).is_none());
        assert!(load_undo_history(project, filename).is_none());
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn save_preserves_unreadable_manifest_and_its_recovery_files() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path();
        save_session(project, &Session {
            tabs: vec![recovery_test_tab("recover this")], ..Session::default()
        }).unwrap();
        let mut saved = load_session(project).unwrap();
        let text = unsaved_dir(project).join(saved.tabs.remove(0).unsaved_file.unwrap());
        drop(saved);

        let manifest = session_path(project);
        fs::write(&manifest, b"{truncated").unwrap();
        assert!(load_session(project).is_none());
        assert_eq!(save_session(project, &Session::default()).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&manifest).unwrap(), b"{truncated");
        assert_eq!(fs::read_to_string(&text).unwrap(), "recover this");
        fs::remove_dir_all(manifest.parent().unwrap()).unwrap();
    }

    #[test]
    fn busy_session_writer_rejects_another_save_before_creating_sidecars() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path();
        let manifest = session_path(project);
        let directory = manifest.parent().unwrap();
        save_session(project, &Session::default()).unwrap();
        let original = fs::read(&manifest).unwrap();
        let writer = open_lock_file(&directory.join(SESSION_WRITER_LOCK)).unwrap();
        writer.try_lock().unwrap();
        let new = Session {
            tabs: vec![recovery_test_tab("another window")],
            ..Session::default()
        };
        assert_eq!(save_session(project, &new).unwrap_err().kind(), io::ErrorKind::WouldBlock);
        assert_eq!(fs::read(&manifest).unwrap(), original);
        assert!(!unsaved_dir(project).exists());

        drop(writer);
        save_session(project, &new).unwrap();
        assert_eq!(load_session(project).unwrap().tabs.len(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn queued_restore_keeps_older_sidecars_after_another_window_saves() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path();
        let mut tab = recovery_test_tab("older unsaved buffer");
        tab.file_path = None;
        save_session(project, &Session { tabs: vec![tab], ..Session::default() }).unwrap();
        let original = load_session(project).unwrap();
        let dir = unsaved_dir(project);
        let content = dir.join(original.tabs[0].unsaved_file.as_deref().unwrap());
        let mut app = crate::app::App::new(project.to_owned());
        app.restore_session(&original);
        drop(original);

        save_session(project, &Session::default()).unwrap();
        assert!(load_session(project).unwrap().tabs.is_empty());
        assert!(content.is_file());
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            app.editor().is_some_and(|editor| !editor.bg_loading)
        });
        let editor = app.editor().unwrap();
        assert_eq!(editor.rope, "older unsaved buffer");
        assert_eq!(editor.export_undo_stack()[0].0, "before");

        save_session(project, &Session::default()).unwrap();
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    #[test]
    fn first_manifest_published_during_startup_gets_a_recovery_lease() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path();
        let dirty = Session {
            tabs: vec![recovery_test_tab("draft from another window")],
            ..Session::default()
        };
        assert!(!session_path(project).exists());
        let (session, incomplete) = load_session_inner(project, || {
            save_session(project, &dirty).unwrap();
        });
        assert!(incomplete.is_none());
        let session = session.unwrap();
        let filename = session.tabs[0].unsaved_file.as_deref().unwrap();
        let path = unsaved_dir(project).join(filename);

        save_session(project, &Session::default()).unwrap();
        assert!(
            path.is_file(),
            "the first manifest's recovery file was pruned during startup"
        );
        assert!(session.recovery_lease.is_held());
        drop(session);

        save_session(project, &Session::default()).unwrap();
        assert!(!path.exists());
        fs::remove_dir_all(session_path(project).parent().unwrap()).unwrap();
    }

    #[test]
    fn duplicate_recovery_views_share_one_decoded_buffer_and_history() {
        let root = tempfile::tempdir().unwrap();
        let source = recovery_test_tab(&"λ log output\n".repeat(20_000));
        save_session(root.path(), &Session { tabs: vec![source], ..Default::default() }).unwrap();
        let saved = load_session(root.path()).unwrap().tabs.remove(0);
        let mut views: Vec<_> = (0..12).map(|index| {
            let mut tab = saved.clone();
            tab.cursor_line = index;
            tab
        }).collect();
        hydrate_saved_tabs(root.path(), views.iter_mut().enumerate().map(|(index, tab)| {
            if index == 1 {
                // Later views must use this job's decoded objects, even if
                // another session write has removed the old sidecars.
                fs::remove_file(unsaved_dir(root.path()).join(saved.unsaved_file.as_ref().unwrap())).unwrap();
                fs::remove_file(unsaved_dir(root.path()).join(saved.undo_history_file.as_ref().unwrap())).unwrap();
            }
            tab
        }));
        for (index, tab) in views.iter().enumerate() {
            assert_eq!(tab.cursor_line, index);
            assert!(tab.pending_unsaved_content.as_ref().unwrap()
                .is_instance(views[0].pending_unsaved_content.as_ref().unwrap()));
            assert!(tab.undo_entries.as_ref().unwrap()[0].0
                .is_instance(&views[0].undo_entries.as_ref().unwrap()[0].0));
            assert!(tab.redo_entries.as_ref().unwrap()[0].0
                .is_instance(&views[0].redo_entries.as_ref().unwrap()[0].0));
        }
    }

    #[test]
    fn recovery_sharing_preserves_transient_edits_and_expires_after_each_job() {
        let root = tempfile::tempdir().unwrap();
        save_session(root.path(), &Session {
            tabs: vec![recovery_test_tab("original")], ..Default::default()
        }).unwrap();
        let saved = load_session(root.path()).unwrap().tabs.remove(0);
        let mut views = vec![saved.clone(); 3];
        views[0].pending_unsaved_content = Some("newer edit".into());
        views[0].undo_entries = Some(Vec::new());
        views[0].redo_entries = Some(Vec::new());
        hydrate_saved_tabs(root.path(), &mut views);
        assert_eq!(views[0].pending_unsaved_content.as_ref().unwrap(), "newer edit");
        assert!(views[0].undo_entries.as_ref().unwrap().is_empty());
        assert_eq!(views[1].pending_unsaved_content.as_ref().unwrap(), "original");
        fs::write(unsaved_dir(root.path()).join(saved.unsaved_file.as_ref().unwrap()), "later disk recovery").unwrap();
        let mut next_job = saved;
        hydrate_saved_tab(root.path(), &mut next_job);
        assert_eq!(next_job.pending_unsaved_content.as_ref().unwrap(), "later disk recovery");
    }

    #[test]
    fn recovery_disk_reconciliation_shares_equal_text_and_preserves_distinct_edits() {
        let disk = ropey::Rope::from_str(&"λ log output\n".repeat(20_000));
        let recovered = ropey::Rope::from_str(&disk.to_string());
        let mut tabs = vec![recovery_test_tab(""); 6];
        for tab in &mut tabs[..4] { tab.pending_unsaved_content = Some(recovered.clone()); }
        let mut edited = recovered.clone();
        edited.insert_char(edited.len_chars() - 3, '!');
        tabs[4].pending_unsaved_content = Some(edited.clone());
        tabs[5].pending_unsaved_content = None;
        share_recovered_disk_snapshot(&mut tabs, &disk);
        assert!(tabs[..4].iter().all(|tab| tab.pending_unsaved_content.as_ref().unwrap().is_instance(&disk)));
        assert!(tabs[4].pending_unsaved_content.as_ref().unwrap().is_instance(&edited));
        assert!(tabs[5].pending_unsaved_content.is_none());
    }

    #[test]
    fn saving_pending_restore_retains_recovery_and_undo_after_tab_reordering() {
        let project_root = unique_temp_project_root();
        let session = Session {
            tabs: vec![recovery_test_tab("pending tab")],
            groups: vec![SavedGroup { tabs: vec![recovery_test_tab("pending group")], active_tab: 0 }],
            ..Session::default()
        };
        save_session(&project_root, &session).unwrap();
        let mut pending = load_session(&project_root).unwrap();
        assert!(pending.tabs[0].pending_unsaved_content.is_none());
        let retained_name = pending.tabs[0].unsaved_file.clone().unwrap();
        // A freshly edited tab taking the same index must not overwrite the
        // sidecar belonging to the still-loading tab or inactive split group.
        pending.tabs.insert(0, recovery_test_tab("new tab"));
        save_session(&project_root, &pending).unwrap();
        let mut loaded = load_session(&project_root).unwrap();
        assert_eq!(loaded.tabs[1].unsaved_file.as_deref(), Some(retained_name.as_str()));
        for (tab, expected) in loaded.tabs.iter_mut().zip(["new tab", "pending tab"]) {
            hydrate_saved_tab(&project_root, tab);
            assert_eq!(tab.pending_unsaved_content.as_ref().unwrap(), expected);
            assert_eq!(tab.undo_entries.as_ref().unwrap()[0].0, "before");
            assert_eq!(tab.redo_entries.as_ref().unwrap()[0].0, "after");
        }
        hydrate_saved_tab(&project_root, &mut loaded.groups[0].tabs[0]);
        assert_eq!(loaded.groups[0].tabs[0].pending_unsaved_content.as_ref().unwrap(), "pending group");
        assert_eq!(loaded.groups[0].tabs[0].undo_entries.as_ref().unwrap()[0].0, "before");
        let manifest = std::fs::read_to_string(session_path(&project_root)).unwrap();
        assert!(!manifest.contains("pending tab"));
        drop(loaded);
        drop(pending);
        save_session(&project_root, &Session::default()).unwrap();
        assert_eq!(std::fs::read_dir(unsaved_dir(&project_root)).unwrap().count(), 0);
        std::fs::remove_dir_all(crate::portable::workspace_dir(&project_root)).unwrap();
    }

    #[test]
    fn failed_manifest_commit_preserves_previous_recovery() {
        let project_root = unique_temp_project_root();
        let mut session = Session { tabs: vec![recovery_test_tab("original")], ..Session::default() };
        save_session(&project_root, &session).unwrap();
        let mut pending = load_session(&project_root).unwrap();
        let files = || std::fs::read_dir(unsaved_dir(&project_root)).unwrap()
            .map(|entry| entry.unwrap().file_name()).collect::<std::collections::BTreeSet<_>>();
        let committed_files = files();
        // Make the final rename fail after the new sidecars were written.
        std::fs::remove_file(session_path(&project_root)).unwrap();
        std::fs::create_dir(session_path(&project_root)).unwrap();
        session.tabs[0].pending_unsaved_content = Some("replacement".into());
        assert!(save_session(&project_root, &session).is_err());
        assert_eq!(files(), committed_files, "a failed backup must not leave large orphan sidecars");
        hydrate_saved_tab(&project_root, &mut pending.tabs[0]);
        assert_eq!(pending.tabs[0].pending_unsaved_content.as_ref().unwrap(), "original");
        assert_eq!(pending.tabs[0].undo_entries.as_ref().unwrap()[0].0, "before");
        std::fs::remove_dir_all(crate::portable::workspace_dir(&project_root)).unwrap();
    }

    #[test]
    fn hydration_preserves_existing_transient_recovery() {
        let project_root = unique_temp_project_root();
        let mut tab = recovery_test_tab("current edits");
        let original = tab.clone();
        hydrate_saved_tab(&project_root, &mut tab);
        assert_eq!(tab, original);
    }

    #[test]
    fn streaming_history_round_trips_empty_unicode_and_separator_content() {
        let root = unique_temp_project_root();
        let texts = ["".to_owned(), "é😀\r\n".repeat(12_000),
            format!("before{}after", std::str::from_utf8(UNDO_SEPARATOR).unwrap()), "".to_owned()];
        let snapshots: UndoStack = texts.iter().enumerate().map(|(index, text)| (
            ropey::Rope::from_str(text), index, 2, vec![(1, 3)], vec![Some((1, 1))], Some((0, 0)), index != 0,
        )).collect();
        let mut retained = std::collections::HashSet::new();
        let name = save_test_undo_history(&root, &snapshots, &snapshots, &mut retained).unwrap();
        assert_eq!(retained.len(), 2);
        let (undo, redo) = load_undo_history(&root, &name).unwrap();
        assert_eq!(undo, snapshots);
        assert_eq!(redo, snapshots);
        std::fs::remove_dir_all(crate::portable::workspace_dir(&root)).unwrap();
    }

    #[test]
    fn streaming_history_rejects_missing_truncated_or_extra_content() {
        let root = unique_temp_project_root();
        let entries = vec![("é😀\n".into(), 0, 0, vec![], vec![], None, false)];
        let name = save_test_undo_history(&root, &entries, &[], &mut Default::default()).unwrap();
        let history: SavedUndoHistory = serde_json::from_slice(&std::fs::read(unsaved_dir(&root).join(&name)).unwrap()).unwrap();
        let path = unsaved_dir(&root).join(history.contents_file.unwrap());
        for contents in ["é", "", "é😀\nextra"] {
            std::fs::write(&path, contents).unwrap();
            assert!(load_undo_history(&root, &name).is_none());
        }
        std::fs::remove_file(&path).unwrap();
        assert!(load_undo_history(&root, &name).is_none());
        std::fs::remove_dir_all(crate::portable::workspace_dir(&root)).unwrap();
    }

    #[test]
    fn streaming_history_accepts_legacy_snapshots_and_empty_stacks() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("history.txt");
        let entry = SavedUndoEntry { content_bytes: None, cursor_line: 1, cursor_col: 3,
            extra_cursors: vec![], extra_cursor_anchors: vec![], selection_anchor: None, modified: true };
        let text = ["", "old é😀 text", ""].join(std::str::from_utf8(UNDO_SEPARATOR).unwrap());
        std::fs::write(&path, text).unwrap();
        let entries = load_undo_stack(&path, vec![entry.clone(); 3]).unwrap();
        assert_eq!(entries.iter().map(|entry| entry.0.to_string()).collect::<Vec<_>>(), ["", "old é😀 text", ""]);
        std::fs::write(&path, "").unwrap();
        assert!(load_undo_stack(&path, vec![]).unwrap().is_empty());
        assert_eq!(load_undo_stack(&path, vec![entry.clone()]).unwrap()[0].0, "");
        assert!(load_undo_stack(&path, vec![entry; 2]).is_err());
    }

    #[test]
    fn session_round_trip_preserves_cursor_and_scroll() {
        let project_root = unique_temp_project_root();
        std::fs::create_dir_all(crate::portable::workspace_dir(&project_root)).unwrap();

        let session = Session {
            tabs: vec![SavedTab {
                file_path: Some("test.rs".into()),
                cursor_line: 42,
                cursor_col: 15,
                scroll_offset: 30,
                horizontal_scroll: 10,
                pinned: true,
                preview: false,
                unsaved_file: None,
                pending_unsaved_content: None,
                undo_history_file: None,
                language_override: Some("rust".to_string()),
                undo_entries: None,
                redo_entries: None,
                bookmarks: vec![5, 10, 42],
                folded_ranges: vec![(3, 8), (20, 25)],
            }],
            ..Session::default()
        };

        save_session(&project_root, &session).unwrap();
        let loaded = load_session(&project_root).expect("session should load");
        assert_eq!(loaded.tabs.len(), 1);
        let tab = &loaded.tabs[0];
        assert_eq!(tab.file_path.as_deref(), Some(Path::new("test.rs")));
        assert_eq!(tab.cursor_line, 42);
        assert_eq!(tab.cursor_col, 15);
        assert_eq!(tab.scroll_offset, 30);
        assert_eq!(tab.horizontal_scroll, 10);
        assert!(tab.pinned);
        assert_eq!(tab.language_override.as_deref(), Some("rust"));
        assert_eq!(tab.bookmarks, vec![5, 10, 42]);
        assert_eq!(tab.folded_ranges, vec![(3, 8), (20, 25)]);

        let _ = std::fs::remove_dir_all(&project_root); // Error OK: dir may not exist
    }

    #[test]
    fn session_round_trip_multiple_tabs() {
        let project_root = unique_temp_project_root();
        std::fs::create_dir_all(crate::portable::workspace_dir(&project_root)).unwrap();

        let session = Session {
            tabs: vec![
                SavedTab {
                    file_path: Some("a.rs".into()),
                    cursor_line: 0, cursor_col: 0, scroll_offset: 0,
                    horizontal_scroll: 0, pinned: false, preview: false,
                    unsaved_file: None, pending_unsaved_content: None,
                    undo_history_file: None, language_override: None,
                    undo_entries: None, redo_entries: None,
                    bookmarks: Vec::new(), folded_ranges: Vec::new(),
                },
                SavedTab {
                    file_path: Some("b.rs".into()),
                    cursor_line: 100, cursor_col: 50, scroll_offset: 80,
                    horizontal_scroll: 20, pinned: true, preview: false,
                    unsaved_file: None, pending_unsaved_content: None,
                    undo_history_file: None, language_override: None,
                    undo_entries: None, redo_entries: None,
                    bookmarks: vec![1], folded_ranges: Vec::new(),
                },
            ],
            active_tab: 1,
            ..Session::default()
        };

        save_session(&project_root, &session).unwrap();
        let loaded = load_session(&project_root).expect("session should load");
        assert_eq!(loaded.tabs.len(), 2);
        assert_eq!(loaded.active_tab, 1);
        assert_eq!(loaded.tabs[1].cursor_line, 100);
        assert!(loaded.tabs[1].pinned);

        let _ = std::fs::remove_dir_all(&project_root); // Error OK: dir may not exist
    }

    #[test]
    fn session_round_trip_with_unsaved_content() {
        let project_root = unique_temp_project_root();
        std::fs::create_dir_all(crate::portable::workspace_dir(&project_root)).unwrap();

        let content = "fn main() {\n    println!(\"hello\");\n}\n";
        let session = Session {
            tabs: vec![SavedTab {
                file_path: Some("main.rs".into()),
                cursor_line: 1, cursor_col: 4, scroll_offset: 0,
                horizontal_scroll: 0, pinned: false, preview: false,
                unsaved_file: None,
                pending_unsaved_content: Some(content.into()),
                undo_history_file: None, language_override: None,
                undo_entries: None, redo_entries: None,
                bookmarks: Vec::new(), folded_ranges: Vec::new(),
            }],
            ..Session::default()
        };

        save_session(&project_root, &session).unwrap();
        let mut loaded = load_session(&project_root).expect("session should load");
        assert!(loaded.tabs[0].pending_unsaved_content.is_none());
        assert!(loaded.tabs[0].unsaved_file.is_some());

        hydrate_saved_tab(&project_root, &mut loaded.tabs[0]);
        assert_eq!(
            loaded.tabs[0].pending_unsaved_content.as_ref().unwrap(),
            content
        );

        let _ = std::fs::remove_dir_all(&project_root); // Error OK: dir may not exist
    }

    #[test]
    fn session_load_missing_file_returns_none() {
        let project_root = unique_temp_project_root();
        std::fs::create_dir_all(&project_root).unwrap();
        // No session.json exists
        let loaded = load_session(&project_root);
        assert!(loaded.is_none());

        let _ = std::fs::remove_dir_all(&project_root); // Error OK: dir may not exist
    }

    #[test]
    fn session_round_trip_empty_tabs() {
        let project_root = unique_temp_project_root();
        std::fs::create_dir_all(crate::portable::workspace_dir(&project_root)).unwrap();

        let session = Session {
            tabs: Vec::new(),
            ..Session::default()
        };

        save_session(&project_root, &session).unwrap();
        let loaded = load_session(&project_root).expect("session should load");
        assert!(loaded.tabs.is_empty());

        let _ = std::fs::remove_dir_all(&project_root); // Error OK: dir may not exist
    }

    #[test]
    fn session_round_trip_split_groups() {
        let project_root = unique_temp_project_root();
        std::fs::create_dir_all(crate::portable::workspace_dir(&project_root)).unwrap();

        let session = Session {
            tabs: vec![SavedTab {
                file_path: Some("main.rs".into()),
                cursor_line: 0, cursor_col: 0, scroll_offset: 0,
                horizontal_scroll: 0, pinned: false, preview: false,
                unsaved_file: None, pending_unsaved_content: None,
                undo_history_file: None, language_override: None,
                undo_entries: None, redo_entries: None,
                bookmarks: Vec::new(), folded_ranges: Vec::new(),
            }],
            groups: vec![SavedGroup {
                tabs: vec![SavedTab {
                    file_path: Some("lib.rs".into()),
                    cursor_line: 10, cursor_col: 5, scroll_offset: 0,
                    horizontal_scroll: 0, pinned: false, preview: false,
                    unsaved_file: None, pending_unsaved_content: None,
                    undo_history_file: None, language_override: None,
                    undo_entries: None, redo_entries: None,
                    bookmarks: Vec::new(), folded_ranges: Vec::new(),
                }],
                active_tab: 0,
            }],
            active_tab: 0,
            active_group: 0,
            ..Session::default()
        };

        save_session(&project_root, &session).unwrap();
        let loaded = load_session(&project_root).expect("session should load");
        assert_eq!(loaded.groups.len(), 1);
        assert_eq!(loaded.groups[0].tabs[0].file_path.as_deref(), Some(Path::new("lib.rs")));
        assert_eq!(loaded.groups[0].tabs[0].cursor_line, 10);

        let _ = std::fs::remove_dir_all(&project_root); // Error OK: dir may not exist
    }

    #[test]
    fn session_default_values() {
        let s = Session::default();
        assert!(s.tabs.is_empty());
        assert_eq!(s.active_tab, 0);
        assert!(!s.show_explorer);
        assert!(!s.show_minimap);
        assert!(!s.split_active);
        assert!(s.groups.is_empty());
        assert!(!s.zen_mode);
    }

    #[test]
    fn session_serde_round_trip_in_memory() {
        let session = Session {
            tabs: vec![SavedTab {
                file_path: Some("hello.txt".into()),
                cursor_line: 5,
                cursor_col: 10,
                scroll_offset: 3,
                horizontal_scroll: 2,
                pinned: true,
                preview: false,
                unsaved_file: None,
                pending_unsaved_content: None,
                undo_history_file: None,
                language_override: Some("Rust".to_string()),
                undo_entries: None,
                redo_entries: None,
                bookmarks: vec![1, 5, 10],
                folded_ranges: vec![(2, 4)],
            }],
            active_tab: 0,
            show_explorer: true,
            show_minimap: true,
            show_panel: true,
            panel_height: 15,
            zen_mode: true,
            ..Session::default()
        };
        let json = serde_json::to_string(&session).unwrap();
        let loaded: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.tabs.len(), 1);
        assert_eq!(loaded.tabs[0].cursor_line, 5);
        assert_eq!(loaded.tabs[0].cursor_col, 10);
        assert!(loaded.tabs[0].pinned);
        assert_eq!(loaded.tabs[0].language_override.as_deref(), Some("Rust"));
        assert_eq!(loaded.tabs[0].bookmarks, vec![1, 5, 10]);
        assert_eq!(loaded.tabs[0].folded_ranges, vec![(2, 4)]);
        assert!(loaded.show_explorer);
        assert!(loaded.show_minimap);
        assert!(loaded.zen_mode);
        assert_eq!(loaded.panel_height, 15);
    }

    #[test]
    fn session_round_trip_zen_mode_and_layout() {
        let project_root = unique_temp_project_root();
        std::fs::create_dir_all(crate::portable::workspace_dir(&project_root)).unwrap();

        let session = Session {
            tabs: Vec::new(),
            active_tab: 0,
            zen_mode: true,
            show_panel: true,
            panel_height: 20,
            panel_tab: 2,
            relative_line_numbers: true,
            sidebar_right: Some(true),
            theme: Some("gruvbox".to_string()),
            sc_graph_collapsed: true,
            sc_graph_height: 7,
            global_search_query: "todo".to_string(),
            global_search_replace_query: "done".to_string(),
            search_include_pattern: "src\\**\\*.rs".to_string(),
            search_exclude_pattern: "target".to_string(),
            search_focus_field: 3,
            ..Session::default()
        };

        save_session(&project_root, &session).unwrap();
        let loaded = load_session(&project_root).expect("session should load");
        assert!(loaded.zen_mode);
        assert!(loaded.show_panel);
        assert_eq!(loaded.panel_height, 20);
        assert_eq!(loaded.panel_tab, 2);
        assert!(loaded.relative_line_numbers);
        assert_eq!(loaded.sidebar_right, Some(true));
        assert_eq!(loaded.theme.as_deref(), Some("gruvbox"));
        assert!(loaded.sc_graph_collapsed);
        assert_eq!(loaded.sc_graph_height, 7);
        assert_eq!(loaded.global_search_query, "todo");
        assert_eq!(loaded.global_search_replace_query, "done");
        assert_eq!(loaded.search_include_pattern, "src\\**\\*.rs");
        assert_eq!(loaded.search_exclude_pattern, "target");
        assert_eq!(loaded.search_focus_field, 3);

        let _ = std::fs::remove_dir_all(&project_root); // Error OK: dir may not exist
    }

    #[test]
    fn saved_tab_serde_skips_transient_fields() {
        let tab = SavedTab {
            file_path: Some("test.rs".into()),
            cursor_line: 0, cursor_col: 0,
            scroll_offset: 0, horizontal_scroll: 0,
            pinned: false,
            preview: false,
            unsaved_file: None, pending_unsaved_content: None,
            undo_history_file: None, language_override: None,
            undo_entries: Some(vec![("content".into(), 0, 0, vec![], vec![], None, true)]),
            redo_entries: Some(vec![("redo".into(), 1, 1, vec![], vec![], None, true)]),
            bookmarks: Vec::new(), folded_ranges: Vec::new(),
        };
        let json = serde_json::to_string(&tab).unwrap();
        let loaded: SavedTab = serde_json::from_str(&json).unwrap();
        // Transient fields should not be serialized
        assert!(loaded.undo_entries.is_none());
        assert!(loaded.redo_entries.is_none());
    }

    #[test]
    fn saved_tab_paths_read_legacy_strings_and_preserve_non_unicode_names() {
        let mut tab = recovery_test_tab("recovery text");
        let legacy = serde_json::to_value(&tab).unwrap();
        assert_eq!(legacy["filePath"], "test.txt");
        let restored: SavedTab = serde_json::from_value(legacy).unwrap();
        assert_eq!(restored.file_path, tab.file_path);

        #[cfg(unix)]
        let opaque = {
            use std::os::unix::ffi::OsStrExt;
            PathBuf::from(std::ffi::OsStr::from_bytes(b"tab-\xff.txt"))
        };
        #[cfg(windows)]
        let opaque = {
            use std::os::windows::ffi::OsStringExt;
            PathBuf::from(std::ffi::OsString::from_wide(&[0xd800, b'a' as u16]))
        };
        tab.file_path = Some(opaque);
        let bytes = serde_json::to_vec(&tab).unwrap();
        let restored: SavedTab = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(restored.file_path, tab.file_path);
    }

    #[test]
    fn session_round_trip_preserves_all_layout_flags() {
        let project_root = unique_temp_project_root();
        std::fs::create_dir_all(crate::portable::workspace_dir(&project_root)).unwrap();

        let session = Session {
            tabs: Vec::new(),
            show_explorer: true,
            show_minimap: true,
            show_panel: true,
            panel_height: 25,
            panel_tab: 3,
            zen_mode: true,
            relative_line_numbers: true,
            show_line_numbers: Some(false),
            show_indent_guides: Some(true),
            sidebar_right: Some(true),
            ..Session::default()
        };

        save_session(&project_root, &session).unwrap();
        let loaded = load_session(&project_root).expect("session should load");
        assert!(loaded.show_explorer);
        assert!(loaded.show_minimap);
        assert!(loaded.show_panel);
        assert_eq!(loaded.panel_height, 25);
        assert_eq!(loaded.panel_tab, 3);
        assert!(loaded.zen_mode);
        assert!(loaded.relative_line_numbers);
        assert_eq!(loaded.show_line_numbers, Some(false));
        assert_eq!(loaded.show_indent_guides, Some(true));
        assert_eq!(loaded.sidebar_right, Some(true));

        let _ = std::fs::remove_dir_all(&project_root); // Error OK: dir may not exist
    }

    #[test]
    fn session_serde_rejects_unknown_fields() {
        let json = r#"{
            "tabs": [],
            "activeTab": 0,
            "showExplorer": false,
            "showMinimap": false,
            "unknownFutureField": 42
        }"#;
        let loaded: Result<Session, _> = serde_json::from_str(json);
        assert!(loaded.is_err(), "unknown fields should be rejected");
    }
}
