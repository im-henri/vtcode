//! Git-backed user-message checkpoints, pending edits and conservative rollback.
//! Working files stay real files. Decisions never alter the user's Git index.
mod inline;
mod merge;
mod storage;
mod view;
pub use inline::{FileReview, InlineChunk};
pub use view::View;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};
pub type Result<T> = std::result::Result<T, String>;
const TEXT_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct Blob {
    hash: String,
    length: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct FileState {
    blob: Blob,
    mode: u32,
}
type Snapshot = BTreeMap<PathBuf, FileState>;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum Decision {
    Pending,
    Accepted,
    Declined,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chunk {
    pub hunk: merge::Hunk,
    pub decision: Decision,
    pub line: usize,
    pub preview: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Edit {
    pub id: u64,
    pub checkpoint: u64,
    pub path: PathBuf,
    pub chunks: Vec<Chunk>,
    pub conflict: Option<String>,
    before: Option<FileState>,
    after: Option<FileState>,
    whole_file: bool,
}

impl Edit {
    pub fn pending(&self) -> usize {
        self.chunks
            .iter()
            .filter(|c| c.decision == Decision::Pending)
            .count()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Checkpoint {
    pub id: u64,
    pub owner: String,
    pub session: String,
    pub label: String,
    pub message_hash: String,
}

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub edits: Vec<Edit>,
    pub checkpoints: Vec<Checkpoint>,
    pub interrupted: bool,
    pub active: Option<u64>,
    pub can_undo: bool,
    root: PathBuf,
}

impl Summary {
    /// Bind an accept action to the pending chunks currently shown in a chat.
    /// Later checkpoint captures must not expand an already queued action.
    pub fn pending_chunks(&self, selection: &Selection) -> Result<Vec<(u64, usize)>> {
        if matches!(selection, Selection::Checkpoint(_)) {
            return Err("Use restore for a checkpoint".into());
        }
        let mut selected = Vec::new();
        for edit in &self.edits {
            let matches = match selection {
                Selection::All => true,
                Selection::File(path) => &edit.path == path,
                Selection::Edit(id) | Selection::Chunk(id, _) => edit.id == *id,
                Selection::Checkpoint(_) => unreachable!(),
            };
            if matches {
                for (index, chunk) in edit.chunks.iter().enumerate() {
                    if chunk.decision == Decision::Pending
                        && !matches!(selection, Selection::Chunk(_, i) if *i != index)
                    {
                        selected.push((edit.id, index));
                    }
                }
            }
        }
        if selected.is_empty() {
            return Err("There are no matching changes to resolve".into());
        }
        Ok(selected)
    }

    /// Match canonical absolute paths from Workspace::write without filesystem I/O.
    /// Exact workspace-relative matching; a/b.rs must never match b.rs.
    pub fn pending_file(&self, path: &Path) -> Option<&Edit> {
        let normalize = |path: &Path| {
            #[cfg(windows)]
            {
                // Hosted files use opaque, case-sensitive view keys. Their
                // spelling must not acquire the client's Windows semantics.
                if self.root.as_os_str().as_encoded_bytes().contains(&0) {
                    return path.to_path_buf();
                }
                let text = path.to_string_lossy().replace('/', "\\");
                PathBuf::from(text.strip_prefix("\\\\?\\").unwrap_or(&text).to_lowercase())
            }
            #[cfg(not(windows))]
            {
                path.to_path_buf()
            }
        };
        let path = normalize(path);
        let root = normalize(&self.root);
        let relative = path.strip_prefix(root).ok()?;
        self.edits
            .iter()
            .find(|e| e.pending() > 0 && normalize(&e.path) == relative)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    All,
    File(PathBuf),
    Edit(u64),
    Chunk(u64, usize),
    Checkpoint(u64),
}

#[derive(Clone, Serialize, Deserialize)]
struct ActiveTurn {
    checkpoint: u64,
    before: Snapshot,
    protected: BTreeSet<PathBuf>,
    #[serde(default)]
    shared: BTreeSet<PathBuf>,
}

// Version 1 stored just one active turn. Accept it on load, but write version 2
// so older builds cannot silently discard concurrent checkpoints.
fn deserialize_active<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Vec<ActiveTurn>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Active {
        Many(Vec<ActiveTurn>),
        Legacy(Option<ActiveTurn>),
    }
    Ok(match Active::deserialize(d)? {
        Active::Many(turns) => turns,
        Active::Legacy(turn) => turn.into_iter().collect(),
    })
}

#[derive(Clone, Serialize, Deserialize)]
struct Replacement {
    path: PathBuf,
    before: Option<FileState>,
    after: Option<FileState>,
    backup: Option<PathBuf>,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Plan {
    version: u64,
    operation: u64,
    owner: String,
    session: String,
    undo_of: Option<u64>,
    replacements: Vec<Replacement>,
    decisions: Vec<(u64, usize)>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Completed {
    plan: Plan,
    previous: Vec<(u64, usize, Decision)>,
    undone: bool,
}

impl Plan {
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.replacements.iter().map(|r| r.path.as_path())
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct State {
    schema: u32,
    namespace: String,
    version: u64,
    next: u64,
    root: PathBuf,
    checkpoints: Vec<Checkpoint>,
    edits: Vec<Edit>,
    #[serde(deserialize_with = "deserialize_active")]
    active: Vec<ActiveTurn>,
    #[serde(default)]
    boundary: Option<Snapshot>,
    transaction: Option<Plan>,
    completed: Vec<Completed>,
}

// Closing a descriptor alone does not release a Unix flock while a concurrent
// fork/dup still holds the same open file description. End this owner's lease
// explicitly, including error paths before the Journal is fully initialized.
struct JournalLock(File);
impl Drop for JournalLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

pub struct Journal {
    root: PathBuf,
    repository: PathBuf,
    directory: PathBuf,
    namespace: String,
    state: State,
    persistence_failed: bool,
    // OS lock is released on crash. A second editor cannot race this journal.
    _lock: JournalLock,
}

impl Journal {
    /// Canonical workspace root used by checkpoint paths, including when the
    /// editor opened this workspace through a directory alias.
    pub fn workspace_root(&self) -> &Path {
        &self.root
    }

    pub fn open(root: &Path, directory: &Path) -> Result<Self> {
        let root = root.canonicalize().map_err(|e| e.to_string())?;
        let repo = git2::Repository::discover(&root)
            .map_err(|_| "Agent checkpoints require a Git working tree. Initialize Git in this workspace first.".to_string())?;
        let workdir = repo
            .workdir()
            .ok_or("Agent checkpoints require a Git working tree")?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        if !root.starts_with(&workdir) {
            return Err("Workspace is outside its Git working tree".into());
        }
        if root.starts_with(repo.path().canonicalize().map_err(|e| e.to_string())?) {
            return Err("Git metadata cannot be used as a checkpoint workspace".into());
        }
        fs::create_dir_all(directory).map_err(|e| e.to_string())?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(repo.path().join("vtcode-rollback.lock"))
            .map_err(|e| e.to_string())?;
        lock.try_lock().map_err(|_| {
            "Another vtcode window is using this workspace's rollback history".to_string()
        })?;
        let lock = JournalLock(lock);
        let history = match fs::read(directory.join("state.json")) {
            Ok(bytes) => Some(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.to_string()),
        };
        let mut state = match &history {
            Some(bytes) => serde_json::from_slice::<State>(bytes)
                .map_err(|e| format!("Cannot read rollback history: {e}"))?,
            None => State {
                schema: 2,
                namespace: storage::digest(
                    format!(
                        "{}:{}:{}:{}",
                        root.display(),
                        directory.display(),
                        std::process::id(),
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map_err(|e| e.to_string())?
                            .as_nanos()
                    )
                    .as_bytes(),
                ),
                version: 0,
                next: 1,
                root: root.clone(),
                checkpoints: vec![],
                edits: vec![],
                active: vec![],
                boundary: None,
                transaction: None,
                completed: vec![],
            },
        };
        if state.schema == 1 {
            state.schema = 2;
            state.boundary = state.active.first().map(|a| a.before.clone());
        }
        if state.schema != 2 || state.root != root {
            return Err("Rollback history belongs to a different workspace or version".into());
        }
        let namespace = state.namespace.clone();
        let mut result = Self {
            root,
            repository: repo.path().to_path_buf(),
            directory: directory.into(),
            namespace,
            state,
            persistence_failed: false,
            _lock: lock,
        };
        let repaired = result.repair_empty_chunk_anchors()?;
        result.validate()?;
        if repaired {
            // Validate the entire journal before saving. Keep the exact original
            // metadata; chunk IDs, decisions and transaction references stay put.
            storage::backup_history(&result.directory, history.as_ref().unwrap())?;
            result.save(result.state.clone())?;
        }
        Ok(result)
    }

    fn repair_empty_chunk_anchors(&mut self) -> Result<bool> {
        let mut repaired = false;
        for edit in &mut self.state.edits {
            if edit.whole_file {
                continue;
            }
            let mut hunks: Vec<_> = edit.chunks.iter().map(|chunk| chunk.hunk.clone()).collect();
            if !merge::normalize_empty_anchors(&mut hunks)? {
                continue;
            }
            let (Some(before), Some(after)) = (&edit.before, &edit.after) else {
                return Err("Invalid chunk sequence in rollback history".into());
            };
            let before = storage::bytes(&self.repository, &before.blob)?;
            let after = storage::bytes(&self.repository, &after.blob)?;
            let (Ok(before), Ok(after)) =
                (std::str::from_utf8(&before), std::str::from_utf8(&after))
            else {
                return Err("Invalid text snapshots in rollback history".into());
            };
            if !merge::exact_hunks(before, after, &hunks) {
                return Err("Invalid chunk sequence in rollback history".into());
            }
            let mut line_starts = vec![0];
            line_starts.extend(after.match_indices('\n').map(|(i, _)| i + 1));
            for (chunk, hunk) in edit.chunks.iter_mut().zip(hunks) {
                if chunk.hunk != hunk {
                    chunk.line = line_starts
                        .partition_point(|start| *start <= hunk.new.start)
                        .max(1);
                    chunk.preview = after[hunk.new.clone()]
                        .lines()
                        .next()
                        .unwrap_or("Deleted lines")
                        .trim()
                        .chars()
                        .take(60)
                        .collect();
                    chunk.hunk = hunk;
                }
            }
            repaired = true;
        }
        Ok(repaired)
    }

    fn validate(&self) -> Result<()> {
        if self.state.namespace.len() != 40
            || !self.state.namespace.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("Invalid rollback namespace".into());
        }
        let mut ids = BTreeSet::new();
        for cp in &self.state.checkpoints {
            if !ids.insert(cp.id) || cp.id >= self.state.next {
                return Err("Invalid checkpoint IDs".into());
            }
        }
        for edit in &self.state.edits {
            if !ids.insert(edit.id)
                || edit.id >= self.state.next
                || !self
                    .state
                    .checkpoints
                    .iter()
                    .any(|c| c.id == edit.checkpoint)
            {
                return Err("Invalid edit IDs in rollback history".into());
            }
            storage::checked_path(&self.root, &edit.path)?;
            if edit.chunks.is_empty()
                || (edit.whole_file && edit.chunks.len() != 1)
                || (!edit.whole_file && (edit.before.is_none() || edit.after.is_none()))
                || edit.chunks.windows(2).any(|pair| {
                    pair[0].hunk.old.end > pair[1].hunk.old.start
                        || pair[0].hunk.new.end > pair[1].hunk.new.start
                })
            {
                return Err("Invalid chunk sequence in rollback history".into());
            }
            for chunk in &edit.chunks {
                if chunk.hunk.old.start > chunk.hunk.old.end
                    || chunk.hunk.new.start > chunk.hunk.new.end
                    || chunk.hunk.old.end as u64 > edit.before.as_ref().map_or(0, |f| f.blob.length)
                    || chunk.hunk.new.end as u64 > edit.after.as_ref().map_or(0, |f| f.blob.length)
                {
                    return Err("Invalid chunk in rollback history".into());
                }
            }
        }
        let mut active_ids = BTreeSet::new();
        let mut conversations = BTreeSet::new();
        for active in &self.state.active {
            let cp = self
                .state
                .checkpoints
                .iter()
                .find(|cp| cp.id == active.checkpoint)
                .ok_or("Invalid active checkpoint")?;
            if !active_ids.insert(cp.id) || !conversations.insert((&cp.owner, &cp.session)) {
                return Err("Duplicate active conversation checkpoint".into());
            }
            for path in active
                .before
                .keys()
                .chain(&active.shared)
                .chain(&active.protected)
            {
                storage::checked_path(&self.root, path)?;
            }
        }
        if self.state.active.is_empty() != self.state.boundary.is_none() {
            return Err("Invalid concurrent checkpoint boundary".into());
        }
        if let Some(plan) = &self.state.transaction {
            self.validate_plan(plan)?;
        }
        for completed in &self.state.completed {
            self.validate_plan(&completed.plan)?;
            for (id, index, _) in &completed.previous {
                self.validate_decision(*id, *index)?;
            }
        }
        Ok(())
    }

    fn validate_decision(&self, id: u64, index: usize) -> Result<()> {
        if self
            .state
            .edits
            .iter()
            .find(|e| e.id == id)
            .and_then(|e| e.chunks.get(index))
            .is_none()
        {
            return Err("Invalid review decision in rollback history".into());
        }
        Ok(())
    }

    fn validate_plan(&self, plan: &Plan) -> Result<()> {
        let mut paths = BTreeSet::new();
        for item in &plan.replacements {
            storage::checked_path(&self.root, &item.path)?;
            let key = storage::path_key(&item.path);
            if !paths.insert(key) {
                return Err(
                    "Two rollback paths refer to the same file; no automatic restore is safe"
                        .into(),
                );
            }
            if let Some(backup) = &item.backup {
                if backup.parent() != item.path.parent()
                    || !backup
                        .file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with(".vtcode-rollback-"))
                {
                    return Err("Invalid rollback backup path".into());
                }
                storage::checked_path(&self.root, backup)?;
            }
            for state in [&item.before, &item.after].into_iter().flatten() {
                if state.blob.hash.len() != 40 || git2::Oid::from_str(&state.blob.hash).is_err() {
                    return Err("Invalid snapshot identifier".into());
                }
            }
        }
        for (id, index) in &plan.decisions {
            self.validate_decision(*id, *index)?;
        }
        if let Some(id) = plan.undo_of
            && !self.state.completed.iter().any(|c| {
                c.plan.operation == id
                    && c.plan.owner == plan.owner
                    && c.plan.session == plan.session
            })
        {
            return Err("Missing prior rollback record".into());
        }
        Ok(())
    }

    fn save(&mut self, mut state: State) -> Result<()> {
        state.version = self.state.version + 1;
        if let Err(error) = storage::atomic_json(&self.directory.join("state.json"), &state) {
            // A directory flush can fail after the metadata rename succeeded.
            // Do not continue using a possibly stale in-memory journal.
            self.persistence_failed = true;
            return Err(format!(
                "{error}. Restart vtcode to reload the retained rollback/recovery history."
            ));
        }
        self.state = state;
        Ok(())
    }

    fn ensure_idle(&self) -> Result<()> {
        self.ensure_no_transaction()?;
        if !self.state.active.is_empty() {
            return Err("Wait for the current response and checkpoint capture to finish".into());
        }
        Ok(())
    }

    fn ensure_no_transaction(&self) -> Result<()> {
        self.ensure_storage()?;
        if self.state.transaction.is_some() {
            return Err("An interrupted rollback needs recovery before more changes".into());
        }
        Ok(())
    }

    fn ensure_storage(&self) -> Result<()> {
        if self.persistence_failed {
            return Err(
                "Restart vtcode to reload rollback history after its storage failure".into(),
            );
        }
        Ok(())
    }

    fn pin(&self, id: u64, kind: &str, files: &Snapshot) -> Result<()> {
        storage::pin(
            &self.repository,
            &format!("refs/vtcode/checkpoints/{}/{id}/{kind}", self.namespace),
            files,
        )
    }

    /// Include tracked files even if ignored, plus nonignored untracked files.
    /// Never traverse Git metadata, nested repositories, symlinks or submodules.
    #[cfg(test)]
    fn snapshot(&self, previous: Option<&Snapshot>) -> Result<Snapshot> {
        self.snapshot_paths(previous, None)
    }

    fn snapshot_paths(
        &self,
        previous: Option<&Snapshot>,
        selection: Option<&BTreeSet<PathBuf>>,
    ) -> Result<Snapshot> {
        storage::checked_root(&self.root)?;
        let repo = storage::repository(&self.repository)?;
        let workdir = repo
            .workdir()
            .ok_or("Git working tree unavailable")?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let prefix = self
            .root
            .strip_prefix(&workdir)
            .map_err(|e| e.to_string())?;
        let index = repo.index().map_err(|e| e.to_string())?;
        let mut paths = BTreeSet::new();
        let mut submodules = BTreeSet::new();
        for entry in index.iter() {
            let path = PathBuf::from(
                String::from_utf8(entry.path).map_err(|_| "Checkpoint paths must be UTF-8")?,
            );
            if let Ok(relative) = path.strip_prefix(prefix) {
                if storage::excluded(relative) {
                    continue;
                } else if entry.mode == 0o160000 {
                    submodules.insert(self.root.join(relative));
                } else {
                    paths.insert(relative.to_path_buf());
                }
            }
        }
        let mut walker = walkdir::WalkDir::new(&self.root)
            .follow_links(false)
            .into_iter();
        while let Some(entry) = walker.next() {
            let entry = entry.map_err(|e| format!("Cannot capture workspace: {e}"))?;
            if entry.depth() == 0 {
                continue;
            }
            let name = entry.file_name().to_string_lossy();
            let relative = entry
                .path()
                .strip_prefix(&self.root)
                .map_err(|e| e.to_string())?;
            if selection.is_some_and(|paths| {
                !paths.iter().any(|path| {
                    let path = storage::path_key(path);
                    let relative = storage::path_key(relative);
                    path.starts_with(&relative) || relative.starts_with(&path)
                })
            }) {
                if entry.file_type().is_dir() {
                    walker.skip_current_dir();
                }
                continue;
            }
            let metadata = fs::symlink_metadata(entry.path()).map_err(|e| e.to_string())?;
            let excluded = storage::excluded(Path::new(name.as_ref()))
                || submodules.contains(entry.path())
                || storage::is_link(&metadata)
                || (entry.file_type().is_dir() && entry.path().join(".git").exists())
                || repo
                    .is_path_ignored(prefix.join(relative))
                    .map_err(|e| e.to_string())?;
            if excluded {
                if entry.file_type().is_dir() {
                    walker.skip_current_dir();
                }
                continue;
            }
            if entry.file_type().is_file() {
                paths.insert(relative.to_path_buf());
            }
        }
        // An agent may add a new ignore rule. Already captured files must remain
        // covered, including detecting their deletion, for the entire turn.
        if let Some(previous) = previous {
            paths.extend(previous.keys().cloned());
        }
        if let Some(selection) = selection {
            paths.retain(|path| Self::capture_matches(path, selection));
        }
        let paths: Vec<_> = paths.into_iter().collect();
        let capture = |repo: &git2::Repository, paths: &[PathBuf]| -> Result<Snapshot> {
            let mut files = BTreeMap::new();
            for path in paths {
                if let Some(state) =
                    storage::capture_with_repo(repo, &storage::checked_path(&self.root, path)?)?
                {
                    files.insert(path.clone(), state);
                }
            }
            Ok(files)
        };
        if paths.len() < 64 {
            return capture(&repo, &paths);
        }
        // Bound I/O concurrency. Each worker owns its libgit2 handle; object
        // installation remains content-addressed and each new object is durable.
        // Parallel hashing/fsync avoids serial disk latency on a first capture.
        let workers = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(8);
        std::thread::scope(|scope| {
            let jobs: Vec<_> = paths
                .chunks(paths.len().div_ceil(workers))
                .map(|paths| {
                    let capture = &capture;
                    let repository = &self.repository;
                    scope.spawn(move || capture(&storage::repository(repository)?, paths))
                })
                .collect();
            let mut files = Snapshot::new();
            for job in jobs {
                files.extend(job.join().map_err(|_| "Checkpoint worker failed")??);
            }
            Ok(files)
        })
    }

    pub fn begin(
        &mut self,
        owner: &str,
        session: &str,
        message: &str,
        protected: BTreeSet<PathBuf>,
    ) -> Result<u64> {
        self.ensure_storage()?;
        if self.state.transaction.is_some() {
            return Err("An interrupted rollback needs recovery before more changes".into());
        }
        if self.summary(owner, session).active.is_some() {
            return Err("This conversation already has an active checkpoint".into());
        }
        let before = self.turn_snapshot()?;
        let id = self.state.next;
        self.pin(id, "before", &before)?;
        let mut state = self.state.clone();
        state.next += 1;
        state.checkpoints.push(Checkpoint {
            id,
            owner: owner.into(),
            session: session.into(),
            label: message
                .lines()
                .next()
                .unwrap_or("Message")
                .chars()
                .take(90)
                .collect(),
            message_hash: storage::digest(message.as_bytes()),
        });
        Self::observe_overlap(&mut state, &before);
        state.boundary = Some(before.clone());
        state.active.push(ActiveTurn {
            checkpoint: id,
            before,
            protected,
            shared: BTreeSet::new(),
        });
        self.save(state)?;
        Ok(id)
    }

    /// Captures both ACP writes and direct shell edits. Files edited by the user
    /// during this turn stay recoverable but require manual conflict resolution.
    pub fn finish(&mut self, protected: BTreeSet<PathBuf>) -> Result<()> {
        if self.state.active.len() != 1 {
            return Err("Specify the checkpoint when multiple conversations are active".into());
        }
        self.finish_checkpoint(self.state.active[0].checkpoint, protected)
    }

    // Keep formerly tracked/unignored files covered throughout every active turn.
    fn turn_snapshot(&self) -> Result<Snapshot> {
        self.turn_snapshot_paths(None)
    }

    fn capture_matches(path: &Path, selection: &BTreeSet<PathBuf>) -> bool {
        let path = storage::path_key(path);
        selection
            .iter()
            .any(|prefix| path.starts_with(storage::path_key(prefix)))
    }

    fn turn_snapshot_paths(&self, selection: Option<&BTreeSet<PathBuf>>) -> Result<Snapshot> {
        let active: BTreeSet<_> = self.state.active.iter().map(|a| a.checkpoint).collect();
        // Live captures advance each turn's baseline. Retain historical paths
        // too, so a deleted file recreated under a new ignore rule stays covered.
        let mut previous: Snapshot = self
            .state
            .edits
            .iter()
            .filter(|edit| active.contains(&edit.checkpoint))
            .filter_map(|edit| {
                edit.after
                    .as_ref()
                    .or(edit.before.as_ref())
                    .map(|file| (edit.path.clone(), file.clone()))
            })
            .collect();
        previous.extend(self.state.active.iter().flat_map(|a| a.before.clone()));
        self.snapshot_paths(Some(&previous), selection)
    }

    // Shell writes cannot be attributed to a process using workspace snapshots.
    // At each observed boundary, protect only files that changed while two or
    // more turns overlapped. Outside those intervals, normal rollback still works.
    fn observe_overlap(state: &mut State, current: &Snapshot) {
        if state.active.len() < 2 {
            return;
        }
        if let Some(previous) = &state.boundary {
            let changed: BTreeSet<_> = previous
                .keys()
                .chain(current.keys())
                .filter(|path| previous.get(*path) != current.get(*path))
                .cloned()
                .collect();
            for active in &mut state.active {
                active.shared.extend(changed.iter().cloned());
            }
        }
    }

    pub fn finish_checkpoint(
        &mut self,
        checkpoint: u64,
        protected: BTreeSet<PathBuf>,
    ) -> Result<()> {
        self.capture_checkpoint_inner(checkpoint, protected, None, true)
    }

    /// Persist completed edits while the response continues. Chunk identities
    /// and decisions stay immutable; later writes create later pending edits.
    /// An empty relative path selects the whole workspace for a fallback scan.
    pub fn capture_checkpoint(
        &mut self,
        checkpoint: u64,
        protected: BTreeSet<PathBuf>,
        paths: &BTreeSet<PathBuf>,
    ) -> Result<()> {
        for path in paths {
            // Selectors only filter the safe snapshot traversal. Do not inspect
            // a newly observed link or nested repository here: those are outside
            // capture coverage and should not keep a watcher burst retrying.
            if path
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Err("Unsafe path in checkpoint capture selection".into());
            }
        }
        self.capture_checkpoint_inner(checkpoint, protected, Some(paths), false)
    }

    fn capture_checkpoint_inner(
        &mut self,
        checkpoint: u64,
        protected: BTreeSet<PathBuf>,
        selection: Option<&BTreeSet<PathBuf>>,
        finish: bool,
    ) -> Result<()> {
        self.ensure_storage()?;
        if !self.state.active.iter().any(|a| a.checkpoint == checkpoint) {
            return Err("No active checkpoint for this conversation".into());
        }
        let after = self.turn_snapshot_paths(selection)?;
        let mut state = self.state.clone();
        let mut boundary = state.boundary.clone().unwrap_or_default();
        if let Some(selection) = selection {
            boundary.retain(|path, _| !Self::capture_matches(path, selection));
            boundary.extend(after.clone());
        } else {
            boundary = after.clone();
        }
        Self::observe_overlap(&mut state, &boundary);
        let active = state
            .active
            .iter()
            .find(|a| a.checkpoint == checkpoint)
            .unwrap()
            .clone();
        let protected: BTreeSet<_> = active
            .protected
            .iter()
            .chain(&protected)
            .map(|path| storage::path_key(path))
            .collect();
        let mut paths: BTreeSet<_> = active.before.keys().chain(after.keys()).cloned().collect();
        if let Some(selection) = selection {
            paths.retain(|path| Self::capture_matches(path, selection));
        }
        if finish
            || paths
                .iter()
                .any(|path| active.before.get(path) != after.get(path))
        {
            self.pin(
                checkpoint,
                if finish { "after" } else { "progress" },
                &after,
            )?;
        }
        for path in &paths {
            let before = active.before.get(path).cloned();
            let after = after.get(path).cloned();
            if before == after {
                continue;
            }
            let mut whole_file = true;
            let mut hunks = Vec::new();
            let mut text_after = None;
            if let (Some(a), Some(b)) = (&before, &after)
                && a.mode == b.mode
                && a.blob.length + b.blob.length <= TEXT_LIMIT
            {
                let a = storage::bytes(&self.repository, &a.blob)?;
                let b = storage::bytes(&self.repository, &b.blob)?;
                if let (Ok(a), Ok(b)) = (std::str::from_utf8(&a), std::str::from_utf8(&b))
                    && !a.contains('\0')
                    && !b.contains('\0')
                {
                    hunks = merge::hunks(a, b);
                    text_after = Some(b.to_string());
                    whole_file = false;
                }
            }
            if whole_file {
                hunks.push(merge::Hunk {
                    old: 0..before.as_ref().map_or(0, |s| s.blob.length as usize),
                    new: 0..after.as_ref().map_or(0, |s| s.blob.length as usize),
                });
            }
            let mut line_starts = vec![0];
            if let Some(text) = &text_after {
                line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
            }
            state.edits.push(Edit {
                id: state.next, checkpoint: active.checkpoint,
                conflict: if active.shared.contains(path) {
                    Some("This file changed while conversations ran concurrently. Its saved snapshots are retained, but the edits cannot be safely attributed to one chat. Review and resolve the shared changes manually.".into())
                } else {
                    protected.contains(&storage::path_key(path))
                        .then(|| "This file was also edited in vtcode during the response. Review its saved snapshots before resolving it manually.".into())
                },
                path: path.clone(), before, after, whole_file,
                chunks: hunks.into_iter().map(|hunk| {
                    let line = line_starts.partition_point(|start| *start <= hunk.new.start).max(1);
                    let preview = text_after.as_ref().and_then(|text| text.get(hunk.new.clone()))
                        .map(|text| text.lines().next().unwrap_or("Deleted lines").trim().chars().take(60).collect())
                        .unwrap_or_else(|| "Whole file".into());
                    Chunk { hunk, decision: Decision::Pending, line, preview }
                }).collect(),
            });
            state.next += 1;
        }
        // A later user edit or overlapping conversation also protects chunks
        // already exposed by a live capture, including previously accepted ones.
        let mut conflicts_changed = false;
        for edit in &mut state.edits {
            if let Some(turn) = state
                .active
                .iter()
                .find(|a| a.checkpoint == edit.checkpoint)
            {
                let user_edited = if turn.checkpoint == checkpoint {
                    protected.contains(&storage::path_key(&edit.path))
                } else {
                    turn.protected
                        .iter()
                        .any(|p| storage::path_key(p) == storage::path_key(&edit.path))
                };
                let reason = if turn.shared.contains(&edit.path) {
                    Some(
                        "This file changed while conversations ran concurrently. Its saved snapshots are retained, but the edits cannot be safely attributed to one chat. Review and resolve the shared changes manually.",
                    )
                } else if user_edited {
                    Some(
                        "This file was also edited in vtcode during the response. Review its saved snapshots before resolving it manually.",
                    )
                } else {
                    None
                };
                if let Some(reason) = reason
                    && edit.conflict.as_deref() != Some(reason)
                {
                    edit.conflict = Some(reason.into());
                    conflicts_changed = true;
                }
            }
        }
        if finish {
            state.active.retain(|a| a.checkpoint != checkpoint);
        } else {
            let turn = state
                .active
                .iter_mut()
                .find(|a| a.checkpoint == checkpoint)
                .unwrap();
            for path in &paths {
                if let Some(file) = after.get(path) {
                    turn.before.insert(path.clone(), file.clone());
                } else {
                    turn.before.remove(path);
                }
            }
            turn.protected.extend(protected);
        }
        state.boundary = (!state.active.is_empty()).then_some(boundary);
        if !finish
            && state.next == self.state.next
            && state.boundary == self.state.boundary
            && state.active.iter().zip(&self.state.active).all(|(a, b)| {
                a.before == b.before && a.protected == b.protected && a.shared == b.shared
            })
            && !conflicts_changed
        {
            return Ok(());
        }
        self.save(state)
    }

    pub fn summary(&self, owner: &str, session: &str) -> Summary {
        let checkpoints: Vec<_> = self
            .state
            .checkpoints
            .iter()
            .filter(|c| c.owner == owner && c.session == session)
            .cloned()
            .collect();
        let ids: BTreeSet<_> = checkpoints.iter().map(|c| c.id).collect();
        Summary {
            root: self.root.clone(),
            edits: self
                .state
                .edits
                .iter()
                .filter(|e| ids.contains(&e.checkpoint))
                .cloned()
                .collect(),
            checkpoints,
            interrupted: self.state.transaction.is_some(),
            active: self
                .state
                .active
                .iter()
                .find(|a| ids.contains(&a.checkpoint))
                .map(|a| a.checkpoint),
            can_undo: self
                .state
                .completed
                .iter()
                .any(|c| !c.undone && c.plan.owner == owner && c.plan.session == session),
        }
    }

    pub fn active(&self) -> Option<u64> {
        self.state.active.first().map(|a| a.checkpoint)
    }
    pub fn interrupted(&self) -> bool {
        self.state.transaction.is_some()
    }

    pub fn finish_uncertain(&mut self) -> Result<()> {
        while let Some(checkpoint) = self.active() {
            let paths = self
                .turn_snapshot()?
                .into_keys()
                .chain(
                    self.state
                        .active
                        .iter()
                        .flat_map(|a| a.before.keys().cloned()),
                )
                .chain(
                    self.state
                        .edits
                        .iter()
                        .filter(|edit| edit.checkpoint == checkpoint)
                        .map(|edit| edit.path.clone()),
                )
                .collect();
            self.finish_checkpoint(checkpoint, paths)?;
        }
        Ok(())
    }

    fn selected(
        &self,
        owner: &str,
        session: &str,
        selection: &Selection,
    ) -> Result<Vec<(u64, usize)>> {
        let summary = self.summary(owner, session);
        let Selection::Checkpoint(id) = selection else {
            return summary.pending_chunks(selection);
        };
        if !summary.checkpoints.iter().any(|c| c.id == *id) {
            return Err("Checkpoint does not belong to this chat".into());
        }
        let mut result = Vec::new();
        for edit in &summary.edits {
            if edit.checkpoint < *id {
                continue;
            }
            for (index, chunk) in edit.chunks.iter().enumerate() {
                if matches!(chunk.decision, Decision::Pending | Decision::Accepted) {
                    result.push((edit.id, index));
                }
            }
        }
        if result.is_empty() {
            return Err("There are no matching changes to resolve".into());
        }
        Ok(result)
    }

    pub fn accept(&mut self, owner: &str, session: &str, selection: &Selection) -> Result<()> {
        self.ensure_no_transaction()?;
        let selected = self.summary(owner, session).pending_chunks(selection)?;
        self.accept_chunks(&selected)
    }

    /// Accept exactly the chunks visible when the user clicked, without
    /// touching files or requiring running agent checkpoints to finish.
    pub fn accept_chunks(&mut self, selected: &[(u64, usize)]) -> Result<()> {
        self.ensure_no_transaction()?;
        if selected.is_empty() {
            return Err("There are no matching changes to resolve".into());
        }
        let mut pending = BTreeSet::new();
        for (id, index) in selected {
            let decision = self
                .state
                .edits
                .iter()
                .find(|edit| edit.id == *id)
                .and_then(|edit| edit.chunks.get(*index))
                .map(|chunk| chunk.decision)
                .ok_or(
                    "A selected change is no longer available. Review the current changes again.",
                )?;
            match decision {
                Decision::Pending => {
                    pending.insert((*id, *index));
                }
                Decision::Accepted => {}
                Decision::Declined => {
                    return Err(
                        "A selected change was already declined. Review the current changes again."
                            .into(),
                    );
                }
            }
        }
        if pending.is_empty() {
            return Ok(());
        }
        let mut state = self.state.clone();
        let pending = pending.into_iter().collect::<Vec<_>>();
        Self::decide(&mut state, &pending, Decision::Accepted);
        self.save(state)
    }

    /// Editor controls address the visible file/chunk, independently of the
    /// conversation currently selected in the chat panel.
    pub fn accept_editor(&mut self, selection: &Selection) -> Result<()> {
        self.ensure_no_transaction()?;
        let selected = self.editor_selected(selection)?;
        self.accept_chunks(&selected)
    }

    fn editor_selected(&self, selection: &Selection) -> Result<Vec<(u64, usize)>> {
        let mut selected = Vec::new();
        for edit in &self.state.edits {
            let matches = match selection {
                Selection::File(path) => storage::path_key(&edit.path) == storage::path_key(path),
                Selection::Chunk(id, _) => edit.id == *id,
                _ => return Err("Editor review requires a file or chunk".into()),
            };
            if matches {
                for (index, chunk) in edit.chunks.iter().enumerate() {
                    if chunk.decision == Decision::Pending
                        && !matches!(selection, Selection::Chunk(_, i) if *i != index)
                    {
                        selected.push((edit.id, index));
                    }
                }
            }
        }
        if selected.is_empty() {
            return Err("There are no matching changes to resolve".into());
        }
        Ok(selected)
    }

    fn decide(state: &mut State, selected: &[(u64, usize)], decision: Decision) {
        for (id, chunk) in selected {
            if let Some(edit) = state.edits.iter_mut().find(|e| e.id == *id) {
                edit.chunks[*chunk].decision = decision;
            }
        }
    }

    fn materialize(&self, edit: &Edit, remove: &BTreeSet<usize>) -> Result<Option<FileState>> {
        if edit.whole_file {
            return Ok(
                if edit.chunks[0].decision == Decision::Declined || remove.contains(&0) {
                    &edit.before
                } else {
                    &edit.after
                }
                .clone(),
            );
        }
        let before = edit.before.as_ref().ok_or("Invalid text edit")?;
        let after = edit.after.as_ref().ok_or("Invalid text edit")?;
        let mut bytes = storage::bytes(&self.repository, &before.blob)?;
        let new = storage::bytes(&self.repository, &after.blob)?;
        for (index, chunk) in edit.chunks.iter().enumerate().rev() {
            if chunk.decision != Decision::Declined && !remove.contains(&index) {
                let replacement = new.get(chunk.hunk.new.clone()).ok_or("Invalid chunk")?;
                if chunk.hunk.old.end > bytes.len() {
                    return Err("Invalid chunk".into());
                }
                bytes.splice(chunk.hunk.old.clone(), replacement.iter().copied());
            }
        }
        Ok(Some(storage::store_bytes(
            &self.repository,
            &bytes,
            before.mode,
        )?))
    }

    fn reverse(
        &self,
        base: Option<FileState>,
        wanted: Option<FileState>,
        current: Option<FileState>,
    ) -> Result<Option<FileState>> {
        if current == base {
            return Ok(wanted);
        }
        if current == wanted {
            return Ok(current);
        }
        if base == wanted {
            return Ok(current);
        }
        let (Some(base), Some(wanted), Some(current)) = (base, wanted, current) else {
            return Err("File creation or deletion conflicts with its current state".into());
        };
        if base.mode != current.mode || base.mode != wanted.mode {
            return Err("File permissions have changed".into());
        }
        let base = storage::bytes(&self.repository, &base.blob)?;
        let wanted = storage::bytes(&self.repository, &wanted.blob)?;
        let current_bytes = storage::bytes(&self.repository, &current.blob)?;
        let utf8 = |bytes| {
            std::str::from_utf8(bytes)
                .map_err(|_| "Binary file changed since the agent edit".to_string())
        };
        let (base, wanted, current_text) = (utf8(&base)?, utf8(&wanted)?, utf8(&current_bytes)?);
        if base.contains('\0') || wanted.contains('\0') || current_text.contains('\0') {
            return Err("Binary file changed since the agent edit".into());
        }
        let merged = merge::merge(base, wanted, current_text)?;
        Ok(Some(storage::store_bytes(
            &self.repository,
            merged.as_bytes(),
            current.mode,
        )?))
    }

    pub fn plan(&self, owner: &str, session: &str, selection: &Selection) -> Result<Plan> {
        self.ensure_idle()?;
        let decisions = self.selected(owner, session, selection)?;
        self.plan_decisions(owner, session, decisions)
    }

    pub fn plan_editor(&self, selection: &Selection) -> Result<Plan> {
        self.ensure_idle()?;
        let decisions = self.editor_selected(selection)?;
        let edit = self
            .state
            .edits
            .iter()
            .find(|e| e.id == decisions.last().unwrap().0)
            .ok_or("Unknown change")?;
        let checkpoint = self
            .state
            .checkpoints
            .iter()
            .find(|c| c.id == edit.checkpoint)
            .ok_or("Unknown checkpoint")?;
        self.plan_decisions(&checkpoint.owner, &checkpoint.session, decisions)
    }

    fn plan_decisions(
        &self,
        owner: &str,
        session: &str,
        decisions: Vec<(u64, usize)>,
    ) -> Result<Plan> {
        let mut replacements = BTreeMap::<PathBuf, Replacement>::new();
        for edit in self.state.edits.iter().rev() {
            let remove: BTreeSet<_> = decisions
                .iter()
                .filter(|(id, _)| *id == edit.id)
                .map(|(_, i)| *i)
                .collect();
            if remove.is_empty() {
                continue;
            }
            if let Some(reason) = &edit.conflict {
                return Err(format!("{}: {reason}", edit.path.display()));
            }
            if !replacements.contains_key(&edit.path) {
                let path = storage::checked_path(&self.root, &edit.path)?;
                if path.metadata().is_ok_and(|m| m.permissions().readonly()) {
                    return Err(format!("{} is read-only", edit.path.display()));
                }
                let current = storage::capture(&self.repository, &path)?;
                replacements.insert(
                    edit.path.clone(),
                    Replacement {
                        path: edit.path.clone(),
                        before: current.clone(),
                        after: current,
                        backup: None,
                    },
                );
            }
            let replacement = replacements.get_mut(&edit.path).unwrap();
            replacement.after = self
                .reverse(
                    self.materialize(edit, &BTreeSet::new())?,
                    self.materialize(edit, &remove)?,
                    replacement.after.clone(),
                )
                .map_err(|e| format!("{}: {e}. No files were changed.", edit.path.display()))?;
        }
        Ok(Plan {
            version: self.state.version,
            operation: 0,
            owner: owner.into(),
            session: session.into(),
            undo_of: None,
            replacements: replacements
                .into_values()
                .filter(|r| r.before != r.after)
                .collect(),
            decisions,
        })
    }

    /// A mistaken decline is recoverable even after a successful operation or
    /// restart. Preserve later disjoint work using the same conflict checks.
    pub fn undo_plan(&self, owner: &str, session: &str) -> Result<Plan> {
        self.ensure_idle()?;
        let completed = self
            .state
            .completed
            .iter()
            .rev()
            .find(|c| !c.undone && c.plan.owner == owner && c.plan.session == session)
            .ok_or("No completed rollback to undo")?;
        let mut replacements = Vec::new();
        for item in &completed.plan.replacements {
            let path = storage::checked_path(&self.root, &item.path)?;
            let current = storage::capture(&self.repository, &path)?;
            let wanted = self
                .reverse(item.after.clone(), item.before.clone(), current.clone())
                .map_err(|e| format!("{}: {e}. No files were changed.", item.path.display()))?;
            replacements.push(Replacement {
                path: item.path.clone(),
                before: current,
                after: wanted,
                backup: None,
            });
        }
        Ok(Plan {
            version: self.state.version,
            operation: 0,
            owner: owner.into(),
            session: session.into(),
            undo_of: Some(completed.plan.operation),
            replacements,
            decisions: vec![],
        })
    }

    pub fn review(&self, id: u64) -> Result<(PathBuf, String, String)> {
        let edit = self
            .state
            .edits
            .iter()
            .find(|e| e.id == id)
            .ok_or("Unknown change")?;
        let text = |state: &Option<FileState>| -> Result<String> {
            match state {
                None => Ok(String::new()),
                Some(state) => String::from_utf8(storage::bytes(&self.repository, &state.blob)?)
                    .map_err(|_| "Binary snapshot; resolve at file level".into()),
            }
        };
        Ok((
            self.root.join(&edit.path),
            text(&edit.before)?,
            text(&self.materialize(edit, &BTreeSet::new())?)?,
        ))
    }

    /// All preconditions and all replacement data are checked before moving a
    /// user file. The write-ahead plan and displaced originals survive failure.
    pub fn apply(&mut self, mut plan: Plan) -> Result<Vec<PathBuf>> {
        self.ensure_idle()?;
        self.validate_plan(&plan)?;
        if plan.version != self.state.version {
            return Err("Changes were updated; review them again".into());
        }
        for item in &plan.replacements {
            let path = storage::checked_path(&self.root, &item.path)?;
            if storage::capture(&self.repository, &path)? != item.before {
                return Err(format!(
                    "{} changed after review. No files were changed.",
                    item.path.display()
                ));
            }
            if path.metadata().is_ok_and(|m| m.permissions().readonly()) {
                return Err(format!("{} is read-only", item.path.display()));
            }
            if let Some(after) = &item.after {
                storage::copy_verified(&self.repository, &after.blob, &mut std::io::sink())?;
            }
        }
        let mut pins = Snapshot::new();
        for (i, item) in plan.replacements.iter_mut().enumerate() {
            if let Some(before) = &item.before {
                pins.insert(PathBuf::from(format!("{i}/before")), before.clone());
            }
            if let Some(after) = &item.after {
                pins.insert(PathBuf::from(format!("{i}/after")), after.clone());
            }
            let path = storage::checked_path(&self.root, &item.path)?;
            if item.before.is_some() {
                let tmp = tempfile::Builder::new()
                    .prefix(".vtcode-rollback-")
                    .tempfile_in(path.parent().unwrap())
                    .map_err(|e| e.to_string())?;
                let backup = tmp.path().to_path_buf();
                tmp.close().map_err(|e| e.to_string())?;
                item.backup = Some(
                    backup
                        .strip_prefix(&self.root)
                        .map_err(|e| e.to_string())?
                        .into(),
                );
            }
        }
        self.pin(self.state.next, "transaction", &pins)?;
        plan.operation = self.state.next;
        let mut state = self.state.clone();
        state.next += 1;
        state.transaction = Some(plan.clone());
        self.save(state)?;
        for item in &plan.replacements {
            if let Err(error) = self.replace(item) {
                return Err(format!(
                    "Rollback interrupted: {error}. Saved originals and the recovery journal were retained; use Recover rollback."
                ));
            }
        }
        self.commit_transaction(true)
    }

    fn replace(&self, item: &Replacement) -> Result<()> {
        let path = storage::checked_path(&self.root, &item.path)?;
        if storage::capture(&self.repository, &path)? != item.before {
            return Err(format!("{} changed during rollback", item.path.display()));
        }
        // Keep the actual displaced file, not just an earlier read. This closes
        // the check/replace data-loss window even with noncooperating writers.
        if let Some(backup) = &item.backup {
            let backup = storage::checked_path(&self.root, backup)?;
            if backup.exists() {
                return Err("Rollback backup path is already occupied".into());
            }
            fs::rename(&path, &backup).map_err(|e| e.to_string())?;
            storage::sync_directory(path.parent().unwrap())?;
            if storage::capture(&self.repository, &backup)? != item.before {
                return Err(format!(
                    "{} changed during replacement; its actual contents remain in {}",
                    item.path.display(),
                    backup.display()
                ));
            }
        }
        if let Some(after) = &item.after {
            self.install(&item.path, after)?;
        }
        Ok(())
    }

    fn install(&self, relative: &Path, state: &FileState) -> Result<()> {
        let path = storage::checked_path(&self.root, relative)?;
        let parent = path.parent().ok_or("Invalid target")?;
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        storage::checked_path(&self.root, relative)?;
        let mut tmp = tempfile::Builder::new()
            .prefix(".vtcode-rollback-")
            .tempfile_in(parent)
            .map_err(|e| e.to_string())?;
        storage::copy_verified(&self.repository, &state.blob, tmp.as_file_mut())?;
        storage::set_mode(tmp.as_file(), state.mode)?;
        tmp.as_file().sync_all().map_err(|e| e.to_string())?;
        // Never clobber a file created by another writer during the rename gap.
        tmp.persist_noclobber(&path).map_err(|e| e.to_string())?;
        storage::sync_directory(parent)
    }

    fn commit_transaction(&mut self, declined: bool) -> Result<Vec<PathBuf>> {
        let plan = self
            .state
            .transaction
            .as_ref()
            .ok_or("No interrupted rollback")?
            .clone();
        let mut state = self.state.clone();
        if declined {
            if let Some(undo_of) = plan.undo_of {
                let completed = state
                    .completed
                    .iter_mut()
                    .find(|c| c.plan.operation == undo_of)
                    .ok_or("Missing rollback recovery record")?;
                completed.undone = true;
                let previous = completed.previous.clone();
                for (id, index, decision) in previous {
                    Self::decide(&mut state, &[(id, index)], decision);
                }
            } else {
                let previous = plan
                    .decisions
                    .iter()
                    .map(|(id, index)| {
                        let edit = state
                            .edits
                            .iter()
                            .find(|e| e.id == *id)
                            .expect("validated plan edit");
                        (*id, *index, edit.chunks[*index].decision)
                    })
                    .collect();
                Self::decide(&mut state, &plan.decisions, Decision::Declined);
                state.completed.push(Completed {
                    plan: plan.clone(),
                    previous,
                    undone: false,
                });
            }
        }
        state.transaction = None;
        self.save(state)?;
        // Never unlink displaced originals automatically. Another process can
        // still write through an open handle after any content check, even after
        // this transaction commits. Git only preserves the bytes already read.
        Ok(plan
            .replacements
            .iter()
            .map(|r| self.root.join(&r.path))
            .collect())
    }

    /// Recovery restores the pre-rollback state. It does not guess which side
    /// of a partially completed multi-file operation the user wanted to keep.
    pub fn recover(&mut self) -> Result<Vec<PathBuf>> {
        self.ensure_storage()?;
        let plan = self
            .state
            .transaction
            .clone()
            .ok_or("No interrupted rollback")?;
        let mut restore = Vec::new();
        for item in &plan.replacements {
            let path = storage::checked_path(&self.root, &item.path)?;
            let current = storage::capture(&self.repository, &path)?;
            if current == item.before {
                continue;
            }
            let mut before = item.before.clone();
            if let Some(backup) = &item.backup {
                let saved = storage::capture(
                    &self.repository,
                    &storage::checked_path(&self.root, backup)?,
                )?;
                if let Some(saved) = saved {
                    before = Some(saved);
                }
            }
            // A previous recovery attempt may already have restored the actual
            // displaced original, whose late writes differ from the planned
            // snapshot. Recognize that state after a crash without replacing it.
            if current == before {
                continue;
            }
            if current != item.after && current.is_some() {
                return Err(format!(
                    "{} has newer edits; recovery stopped and backups were retained",
                    item.path.display()
                ));
            }
            // Recovery must validate the entire batch before displacing even
            // the first file, just like a normal restore. A missing/corrupt Git
            // object must not strand a later file outside its original path.
            if let Some(before) = &before {
                storage::copy_verified(&self.repository, &before.blob, &mut std::io::sink())?;
            }
            restore.push((item.path.clone(), current, before));
        }
        for (relative, expected, before) in restore {
            let path = storage::checked_path(&self.root, &relative)?;
            if storage::capture(&self.repository, &path)? != expected {
                return Err("File changed during recovery; backups retained".into());
            }
            if expected.is_some() {
                // Retain any intervening file as well, until manually inspected.
                let tmp = tempfile::Builder::new()
                    .prefix(".vtcode-rollback-recovery-")
                    .tempfile_in(path.parent().unwrap())
                    .map_err(|e| e.to_string())?;
                let (_, saved) = tmp.keep().map_err(|e| e.to_string())?;
                fs::rename(&path, &saved).map_err(|e| e.to_string())?;
                storage::sync_directory(path.parent().unwrap())?;
                if storage::capture(&self.repository, &saved)? != expected {
                    return Err(format!(
                        "File changed during recovery; retained in {}",
                        saved.display()
                    ));
                }
            }
            if let Some(before) = before {
                self.install(&relative, &before)?;
            }
        }
        self.commit_transaction(false)
    }
}
