//! Background checkpoint/review adapter. No filesystem work in the render path.
use super::{ChatAction, Choice, Picker};
use crate::app::{App, TabKind};
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use vtcode::acp::{
    AgentConfig, ChatEntry, Command, protocol as p,
    rollback::{FileReview, Selection},
};
#[path = "app_acp_journal.rs"]
mod journal;
use journal::{Journal, Plan};
#[cfg(test)]
#[path = "app_acp_rollback_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "app_acp_remote_rollback_tests.rs"]
mod remote_tests;

enum Reply {
    Loaded(Option<Box<Journal>>),
    Begun(u64, p::SessionId, Vec<p::ContentBlock>, Option<Arc<crate::remote::context::Source>>),
    Finished(p::SessionId),
    Captured(p::SessionId),
    Accepted,
    Planned(Plan),
    Validated(Plan, Vec<(PathBuf, Option<PathBuf>)>),
    Applied(Vec<(PathBuf, bool)>),
    Review(PathBuf, String, String),
}

struct ReviewReply {
    result: Result<Reply, String>,
    reviews: Option<ReviewSnapshot>,
}

struct ReviewSnapshot {
    root: PathBuf,
    files: Vec<FileReview>,
}

impl ReviewSnapshot {
    fn new(journal: &Journal) -> Self {
        Self {
            root: normalize(journal.workspace_root()),
            files: journal.inline_reviews(),
        }
    }
}

#[derive(Default)]
pub(crate) struct ReviewState {
    pub enabled: bool,
    pub initialized: bool,
    initialization_error: Option<String>,
    pub active: bool,
    pub mutating: bool,
    pub file_writes: Arc<AtomicUsize>,
    session_writes: HashMap<(super::AgentId, p::SessionId), Arc<AtomicUsize>>,
    root: PathBuf,
    journal: Option<Arc<Mutex<Journal>>>,
    receiver: Option<mpsc::Receiver<ReviewReply>>,
    job: Option<JobContext>,
    jobs: VecDeque<CheckpointJob>,
    turns: HashMap<(super::AgentId, p::SessionId), TurnCapture>,
    write_paths: Vec<PathBuf>,
    disk_barriers: HashMap<PathBuf, Instant>,
    inline_root: PathBuf,
    pub(crate) inline_reviews: Arc<Vec<FileReview>>,
    pub(crate) inline_revision: u64,
}

#[derive(Default, Debug)]
struct TurnCapture {
    checkpoint: Option<u64>,
    protected: BTreeSet<PathBuf>,
    finishing: bool,
    failed: bool,
    changed_paths: BTreeSet<PathBuf>,
    capture_paths: BTreeSet<PathBuf>,
    capturing: bool,
    retry_at: Option<Instant>,
    next_scan: Option<Instant>,
    capture_error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Initialize,
    Review,
    Accept,
    Begin,
    Finish,
    Capture,
}

#[derive(Clone)]
struct JobContext {
    agent: super::AgentId,
    session: Option<p::SessionId>,
    kind: JobKind,
    editor: bool,
}

struct CheckpointJob {
    context: JobContext,
    mutating: bool,
    run: Box<dyn FnOnce() -> Result<Reply, String> + Send>,
}

fn normalize(path: &Path) -> PathBuf {
    if crate::remote::workspace::is_view_key(path) { return path.to_path_buf(); }
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.into());
    let text = absolute.to_string_lossy().replace('/', "\\");
    #[cfg(windows)]
    {
        let text = if let Some(unc) = text.strip_prefix("\\\\?\\UNC\\") {
            format!("\\\\{unc}")
        } else {
            text.strip_prefix("\\\\?\\").unwrap_or(&text).to_owned()
        };
        PathBuf::from(text.to_lowercase())
    }
    #[cfg(not(windows))]
    {
        let _ = text;
        absolute
    }
}

/// Resolve directory aliases even for a new/deleted file. This is used only
/// when collecting user edits or preflighting a restore, never while rendering.
pub(super) fn resolve_review_path(path: &Path) -> PathBuf {
    if crate::remote::workspace::is_view_key(path) { return path.to_path_buf(); }
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut ancestor = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match ancestor.canonicalize() {
            Ok(mut resolved) => {
                for part in missing.into_iter().rev() {
                    resolved.push(part);
                }
                return resolved;
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    && ancestor.symlink_metadata().is_err() => {}
            Err(_) => return absolute.clone(),
        }
        let (Some(name), Some(parent)) = (ancestor.file_name(), ancestor.parent()) else {
            return absolute.clone();
        };
        missing.push(name);
        ancestor = parent;
    }
}

impl ReviewState {
    pub fn busy(&self) -> bool {
        self.receiver.is_some() || !self.jobs.is_empty()
    }
    pub(crate) fn initializing(&self) -> bool {
        self.job
            .as_ref()
            .is_some_and(|job| job.kind == JobKind::Initialize)
            || self
                .jobs
                .iter()
                .any(|job| job.context.kind == JobKind::Initialize)
    }
    pub(crate) fn initialization_error(&self) -> Option<&str> {
        self.initialization_error.as_deref()
    }
    pub(super) fn history_delete_blocked(
        &self,
        owner: &str,
        id: &p::SessionId,
    ) -> Option<&'static str> {
        if self.initialization_error.is_some() {
            return Some("Retry checkpoint storage before deleting this chat.");
        }
        if self.initializing() {
            return Some("Wait for checkpoint storage before deleting this chat.");
        }
        if !self.enabled {
            return None;
        }
        let Some(journal) = &self.journal else {
            return Some("Wait for checkpoint storage before deleting this chat.");
        };
        let Ok(journal) = journal.try_lock() else {
            return Some("Wait for checkpoint capture before deleting this chat.");
        };
        journal
            .summary(owner, &id.to_string())
            .edits
            .iter()
            .any(|edit| edit.pending() > 0)
            .then_some("Accept or decline this chat's pending changes before deleting it.")
    }
    pub(super) fn changing_workspace(&self, root: &Path) -> bool {
        self.root != root && (self.busy() || self.active)
    }
    pub(super) fn cancel_preparing(
        &self,
        agent: super::AgentId,
        id: Option<&p::SessionId>,
    ) -> bool {
        id.and_then(|id| self.turns.get(&(agent, id.clone())))
            .is_some_and(|turn| turn.checkpoint.is_none())
    }
    pub(crate) fn conversation_busy(
        &self,
        agent: super::AgentId,
        id: Option<&p::SessionId>,
    ) -> bool {
        id.is_some_and(|id| {
            self.turns.contains_key(&(agent, id.clone())) || self.write_busy(agent, id)
        })
    }
    pub(super) fn agent_busy(&self, agent: super::AgentId) -> bool {
        self.turns.keys().any(|(owner, _)| *owner == agent)
            || self
                .session_writes
                .iter()
                .any(|((owner, _), count)| *owner == agent && count.load(Ordering::Acquire) != 0)
            || self.job.as_ref().is_some_and(|job| job.agent == agent)
            || self.jobs.iter().any(|job| job.context.agent == agent)
    }
    pub(super) fn agent_capture_failed(&self, agent: super::AgentId) -> bool {
        self.turns.iter().any(|((owner, _), turn)| *owner == agent && turn.failed)
    }
    pub(crate) fn reviewing(&self) -> bool {
        self.job
            .as_ref()
            .is_some_and(|job| job.kind == JobKind::Review)
            || self
                .jobs
                .iter()
                .any(|job| job.context.kind == JobKind::Review)
    }
    pub(crate) fn capture_failed(&self, agent: super::AgentId, id: Option<&p::SessionId>) -> bool {
        id.and_then(|id| self.turns.get(&(agent, id.clone())))
            .is_some_and(|turn| turn.failed)
    }
    pub(super) fn retry_checkpoint(&mut self, agent: super::AgentId, id: Option<&p::SessionId>) {
        if let Some(turn) = id.and_then(|id| self.turns.get_mut(&(agent, id.clone()))) {
            turn.failed = false;
        }
    }
    fn write_busy(&self, agent: super::AgentId, id: &p::SessionId) -> bool {
        self.session_writes
            .get(&(agent, id.clone()))
            .is_some_and(|count| count.load(Ordering::Acquire) != 0)
    }
    pub(super) fn file_request_guard(
        &mut self,
        agent: super::AgentId,
        id: &p::SessionId,
        write: bool,
    ) -> FileWriteGuard {
        let session = write.then(|| {
            self.session_writes
                .entry((agent, id.clone()))
                .or_default()
                .clone()
        });
        if let Some(count) = &session {
            count.fetch_add(1, Ordering::AcqRel);
            self.file_writes.fetch_add(1, Ordering::AcqRel);
        }
        FileWriteGuard {
            pending: self.file_writes.clone(),
            session,
        }
    }
    pub(crate) fn file_busy(&self, path: &Path) -> bool {
        let path = self.path_key(path);
        self.write_paths
            .iter()
            .any(|p| p == &path || p.starts_with(&path) || path.starts_with(p))
    }
    pub(crate) fn defer_disk_change(
        &self,
        change: &crate::background::WorkspacePathChange,
    ) -> bool {
        let path = self.path_key(&change.path);
        self.file_busy(&path)
            || self.disk_barriers.iter().any(|(p, time)| {
                (p == &path || path.starts_with(p) || (!change.exists && p.starts_with(&path)))
                    && change.observed_at <= *time
            })
    }
    pub(crate) fn note_disk_write_finished(&mut self, path: &Path) {
        self.disk_barriers.insert(self.path_key(path), Instant::now());
    }
    fn relative(&self, path: &Path) -> Option<PathBuf> {
        let path = normalize(path);
        [&self.root, &self.inline_root]
            .into_iter()
            .filter(|root| !root.as_os_str().is_empty())
            .find_map(|root| {
                path.strip_prefix(normalize(root))
                    .ok()
                    .map(Path::to_path_buf)
            })
    }

    fn path_key(&self, path: &Path) -> PathBuf {
        let path = normalize(path);
        // Journal paths are canonical; editor paths retain the caller's
        // spelling. Translate the workspace alias using the worker's cached
        // root, never by resolving paths or locking the journal while drawing.
        if !self.inline_root.as_os_str().is_empty()
            && let Some(relative) = self.relative(&path)
        {
            return self.inline_root.join(relative);
        }
        path
    }

    fn file_review(&self, path: &Path) -> Option<&FileReview> {
        let path = self.path_key(path);
        self.inline_reviews
            .iter()
            .find(|file| normalize(&file.path) == path)
    }
    pub fn note_user_save(&mut self, path: &Path) {
        if self.turns.is_empty() {
            return;
        }
        // Editors can open a workspace through a directory alias. Resolve it
        // here, outside rendering, so the protected path matches the snapshot.
        let path = resolve_review_path(path);
        let root = resolve_review_path(&self.root);
        if let Ok(relative) = normalize(&path).strip_prefix(normalize(&root)) {
            for turn in self.turns.values_mut() {
                turn.protected.insert(relative.to_path_buf());
            }
        }
    }

    fn note_turn_path(turn: &mut TurnCapture, path: PathBuf) {
        if turn.changed_paths.contains(Path::new("")) {
            return;
        }
        turn.changed_paths.insert(path);
        // A large watcher burst becomes one bounded worker scan, rather than
        // retaining an unbounded path queue or creating a job per file.
        if turn.changed_paths.len() > 128 {
            turn.changed_paths.clear();
            turn.changed_paths.insert(PathBuf::new());
        }
    }

    pub(crate) fn note_workspace_change(&mut self, path: &Path) {
        let Some(path) = self.relative(path) else {
            return;
        };
        for turn in self.turns.values_mut().filter(|turn| !turn.finishing) {
            Self::note_turn_path(turn, path.clone());
        }
    }

    pub(super) fn note_agent_write(
        &mut self,
        agent: super::AgentId,
        id: &p::SessionId,
        path: &Path,
    ) {
        let Some(path) = self.relative(path) else {
            return;
        };
        if let Some(turn) = self
            .turns
            .get_mut(&(agent, id.clone()))
            .filter(|turn| !turn.finishing)
        {
            Self::note_turn_path(turn, path);
        }
    }

    pub(crate) fn next_capture_deadline(&self) -> Option<Instant> {
        if !self.enabled || self.busy() || self.file_writes.load(Ordering::Acquire) != 0 {
            return None;
        }
        self.turns
            .values()
            .filter(|turn| {
                turn.checkpoint.is_some() && !turn.finishing && !turn.failed && !turn.capturing
            })
            .filter_map(|turn| {
                if !turn.changed_paths.is_empty() {
                    Some(turn.retry_at.unwrap_or_else(Instant::now))
                } else {
                    turn.next_scan
                }
            })
            .min()
    }
}

pub(super) struct FileWriteGuard {
    pending: Arc<AtomicUsize>,
    session: Option<Arc<AtomicUsize>>,
}
impl Drop for FileWriteGuard {
    fn drop(&mut self) {
        if let Some(count) = &self.session {
            count.fetch_sub(1, Ordering::AcqRel);
            self.pending.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl App {
    pub(crate) fn editor_file_review(&self, path: &Path) -> Option<FileReview> {
        self.chat.rollback.file_review(path).cloned()
    }

    pub(crate) fn pending_agent_change(&self, path: &Path) -> bool {
        // Pending edits belong to the workspace journal, even if their agent
        // was closed or its originating conversation has not been loaded.
        self.chat.rollback.file_review(path).is_some()
    }

    fn rollback_job(
        &mut self,
        mutating: bool,
        job: impl FnOnce() -> Result<Reply, String> + Send + 'static,
    ) {
        self.checkpoint_job(JobKind::Review, self.chat.selected.clone(), mutating, job);
    }

    fn checkpoint_job(
        &mut self,
        kind: JobKind,
        session: Option<p::SessionId>,
        mutating: bool,
        job: impl FnOnce() -> Result<Reply, String> + Send + 'static,
    ) {
        // Only snapshot I/O is serialized. Agent responses are never serialized.
        self.chat.rollback.jobs.push_back(CheckpointJob {
            context: JobContext {
                agent: self.chat.active_agent,
                session,
                kind,
                editor: false,
            },
            mutating,
            run: Box::new(job),
        });
        self.start_checkpoint_job();
    }

    fn start_checkpoint_job(&mut self) {
        if self.chat.rollback.receiver.is_some() {
            return;
        }
        let Some(job) = self.chat.rollback.jobs.pop_front() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.chat.rollback.receiver = Some(rx);
        self.chat.rollback.mutating = job.mutating;
        self.chat.rollback.job = Some(job.context);
        let wake = self.background.wake_handle();
        let journal = self.chat.rollback.journal.clone();
        self.background.spawn("chat-checkpoint", move || {
            let result = (job.run)();
            let reviews = if let Ok(Reply::Loaded(Some(journal))) = &result {
                Some(ReviewSnapshot::new(journal))
            } else {
                journal.and_then(|journal| journal.lock().ok().map(|j| ReviewSnapshot::new(&j)))
            };
            let _ = tx.send(ReviewReply { result, reviews });
            wake.notify();
        });
    }

    pub(super) fn initialize_chat_rollback(&mut self, _config: &AgentConfig) {
        if ((self.chat.rollback.initialized
            && (self.chat.rollback.enabled || self.chat.any_agent_running()))
            || self.chat.rollback.busy())
            && self.chat.rollback.root == self.explorer.root
        {
            self.refresh_chat_rollback();
            return;
        }
        if self.chat.rollback.root != self.explorer.root {
            self.chat.rollback = ReviewState {
                root: self.explorer.root.clone(),
                ..Default::default()
            };
        }
        if let Some(error) = self.chat.rollback.initialization_error.take() {
            let chat = &mut self.chat;
            for state in std::iter::once(&mut chat.connection).chain(chat.connections.values_mut())
            {
                if state.error.as_deref() == Some(error.as_str()) {
                    state.error = None;
                }
                for view in state.views.values_mut() {
                    if view.error.as_deref() == Some(error.as_str()) {
                        view.error = None;
                    }
                }
            }
        }
        self.chat.rollback.initialized = false;
        let root = self.explorer.root.clone();
        let remote = self.remote_workspace.clone();
        let directory = crate::portable::workspace_dir(&root).join("agent-rollback");
        self.checkpoint_job(JobKind::Initialize, None, false, move || {
            if let Some(remote) = remote {
                return Journal::open_remote(remote).map(|journal| Reply::Loaded(journal.map(Box::new)));
            }
            match git2::Repository::discover(&root) {
                Err(error) if error.code() == git2::ErrorCode::NotFound => {
                    return Ok(Reply::Loaded(None));
                }
                Err(error) => return Err(format!("Cannot inspect Git for checkpoints: {error}")),
                Ok(repo) if repo.is_bare() => return Ok(Reply::Loaded(None)),
                Ok(_) => {}
            }
            let mut journal = Journal::open(&root, &directory)?;
            if journal.active().is_some() {
                journal.finish_uncertain()?;
            }
            Ok(Reply::Loaded(Some(Box::new(journal))))
        });
    }

    pub(super) fn retry_chat_checkpoint_storage(&mut self) {
        if self.chat.rollback.busy() || self.chat.rollback.initialization_error().is_none() {
            return;
        }
        let Some(config) = self.chat.agent.clone() else {
            return;
        };
        self.initialize_chat_rollback(&config);
        self.chat.status = "Opening checkpoints…".into();
    }

    /// A submitted prompt can wait for storage without being sent, lost, or
    /// paused. Keep the actual initialization failure after new-session events.
    pub(super) fn checkpoint_storage_blocked(&mut self) -> Option<bool> {
        if self.chat.rollback.initialized || self.chat.rollback.root.as_os_str().is_empty() {
            return None;
        }
        if let Some(error) = self.chat.rollback.initialization_error() {
            let changed = self.chat.error.as_deref() != Some(error);
            self.chat.error = Some(error.to_owned());
            Some(changed)
        } else {
            let changed = self.chat.status != "Opening checkpoints…";
            self.chat.status = "Opening checkpoints…".into();
            Some(changed)
        }
    }

    fn protect_chat_buffers(&mut self) {
        if self.chat.rollback.turns.is_empty() {
            return;
        }
        let paths: Vec<_> = self
            .tabs
            .iter()
            .chain(self.groups.iter().flat_map(|g| &g.tabs))
            .filter(|t| t.kind == TabKind::File && t.editor.modified)
            .filter_map(|t| self.acp_editor_identity(&t.editor))
            .chain(self.pending_file_saves.keys().cloned())
            .collect();
        for path in paths {
            self.chat.rollback.note_user_save(&path);
        }
    }

    pub(super) fn refresh_chat_rollback(&mut self) {
        let Some(journal) = self.chat.rollback.journal.clone() else {
            return;
        };
        let Ok(journal) = journal.try_lock() else {
            return;
        };
        self.chat.rollback.inline_root = normalize(journal.workspace_root());
        self.chat.rollback.active = journal.active().is_some();
        let chat = &mut self.chat;
        for state in std::iter::once(&mut chat.connection).chain(chat.connections.values_mut()) {
            let owner = state.checkpoint_owner();
            for (id, session) in &mut state.sessions {
                let summary = journal.summary(&owner, &id.to_string());
                // Full exact replay can restore inline links. Partial/reformatted
                // replay uses the durable checkpoint picker instead of guessing.
                let users: Vec<_> = session
                    .entries
                    .iter()
                    .enumerate()
                    .filter_map(|(i, e)| {
                        if let ChatEntry::User(text) = e {
                            Some((i, text))
                        } else {
                            None
                        }
                    })
                    .collect();
                if users.len() == summary.checkpoints.len()
                    && users
                        .iter()
                        .zip(&summary.checkpoints)
                        .all(|((_, text), cp)| {
                            git2::Oid::hash_object(git2::ObjectType::Blob, text.as_bytes())
                                .is_ok_and(|oid| oid.to_string() == cp.message_hash)
                        })
                {
                    let links: Vec<_> = users
                        .iter()
                        .zip(&summary.checkpoints)
                        .map(|((i, _), cp)| (*i, cp.id))
                        .collect();
                    for (i, cp) in links {
                        session.set_checkpoint(i, cp);
                    }
                }
                session.set_rollback(summary);
            }
        }
    }

    pub(super) fn send_chat_with_checkpoint(
        &mut self,
        session_id: p::SessionId,
        content: Vec<p::ContentBlock>,
        text: &str,
    ) -> bool {
        self.send_chat_with_checkpoint_context(session_id, content, text, None)
    }

    pub(super) fn send_chat_with_checkpoint_context(
        &mut self,
        session_id: p::SessionId,
        content: Vec<p::ContentBlock>,
        text: &str,
        source: Option<Arc<crate::remote::context::Source>>,
    ) -> bool {
        if self.checkpoint_storage_blocked().is_some() {
            return false;
        }
        if !self.chat.rollback.enabled {
            return self.send_acp(Command::Prompt {
                session_id,
                content,
            });
        }
        if self.chat.rollback.reviewing() {
            self.chat.error = Some("Wait for the rollback operation to finish.".into());
            return false;
        }
        if self
            .chat
            .rollback
            .conversation_busy(self.chat.active_agent, Some(&session_id))
        {
            return false;
        }
        let Some(journal) = self.chat.rollback.journal.clone() else {
            return false;
        };
        let owner = self.chat.checkpoint_owner();
        let text = text.to_string();
        let key = (self.chat.active_agent, session_id.clone());
        self.chat
            .rollback
            .turns
            .insert(key.clone(), TurnCapture::default());
        self.protect_chat_buffers();
        let protected = self.chat.rollback.turns[&key].protected.clone();
        self.chat.status = "Capturing message checkpoint…".into();
        self.checkpoint_job(JobKind::Begin, Some(session_id.clone()), true, move || {
            let id = journal
                .lock()
                .map_err(|_| "Checkpoint worker failed")?
                .begin(&owner, &session_id.to_string(), &text, protected)?;
            Ok(Reply::Begun(id, session_id, content, source))
        });
        true
    }

    pub(super) fn poll_chat_rollback(&mut self) -> bool {
        self.protect_chat_buffers();
        self.chat
            .rollback
            .session_writes
            .retain(|_, count| count.load(Ordering::Acquire) != 0);
        let mut changed = false;
        if let Some(context) = self.chat.rollback.job.clone() {
            let background = self.chat.background || context.agent != self.chat.active_agent;
            changed |= self
                .with_agent_context(context.agent, background, |app| {
                    app.poll_chat_rollback_reply(&context)
                })
                .unwrap_or(false);
        }
        self.start_checkpoint_job();
        // Finish each response independently, even if another chat is still
        // waiting on the model, a permission, or a long-running terminal.
        let finished: Vec<_> = self
            .chat
            .rollback
            .turns
            .iter()
            .filter_map(|((agent, id), turn)| {
                let checkpoint = turn.checkpoint?;
                if turn.finishing || turn.failed || self.chat.rollback.write_busy(*agent, id) {
                    return None;
                }
                let session = self
                    .chat
                    .agent_state(*agent)
                    .and_then(|state| state.sessions.get(id));
                session
                    .is_none_or(|s| {
                        !s.busy
                            && s.terminals
                                .values()
                                .all(|t| t.released || t.exit_status.is_some())
                    })
                    .then(|| (*agent, id.clone(), checkpoint, turn.protected.clone()))
            })
            .collect();
        for (agent, id, checkpoint, protected) in finished {
            let Some(journal) = self.chat.rollback.journal.clone() else {
                continue;
            };
            self.chat
                .rollback
                .turns
                .get_mut(&(agent, id.clone()))
                .unwrap()
                .finishing = true;
            let background = self.chat.background || agent != self.chat.active_agent;
            self.with_agent_context(agent, background, |app| {
                app.checkpoint_job(JobKind::Finish, Some(id.clone()), true, move || {
                    journal
                        .lock()
                        .map_err(|_| "Checkpoint worker failed")?
                        .finish_checkpoint(checkpoint, protected)?;
                    Ok(Reply::Finished(id))
                });
            });
            changed = true;
        }
        self.capture_running_chat_changes();
        changed
    }

    fn capture_running_chat_changes(&mut self) {
        if !self.chat.rollback.enabled
            || self.chat.rollback.busy()
            || self.chat.rollback.file_writes.load(Ordering::Acquire) != 0
        {
            return;
        }
        let now = Instant::now();
        let candidate = self
            .chat
            .rollback
            .turns
            .iter()
            .filter_map(|((agent, id), turn)| {
                let checkpoint = turn.checkpoint?;
                if turn.finishing
                    || turn.failed
                    || turn.capturing
                    || turn.retry_at.is_some_and(|retry| retry > now)
                {
                    return None;
                }
                let scan = turn.next_scan.is_some_and(|deadline| deadline <= now);
                (!turn.changed_paths.is_empty() || scan).then(|| {
                    (
                        *agent,
                        id.clone(),
                        checkpoint,
                        scan,
                        turn.next_scan.unwrap_or(now),
                    )
                })
            })
            // Prefer the least recently captured turn. A chat with continuous
            // writes must not starve another chat's pending notifications.
            .min_by_key(|(_, _, _, _, deadline)| *deadline);
        let Some((agent, id, checkpoint, scan, _)) = candidate else {
            return;
        };
        let Some(journal) = self.chat.rollback.journal.clone() else {
            return;
        };
        let turn = self
            .chat
            .rollback
            .turns
            .get_mut(&(agent, id.clone()))
            .unwrap();
        let mut paths = std::mem::take(&mut turn.changed_paths);
        if scan {
            paths.clear();
            paths.insert(PathBuf::new());
        }
        turn.capture_paths = paths.clone();
        turn.capturing = true;
        turn.retry_at = None;
        // A sparse fallback catches dropped watcher events and shell edits in
        // files excluded from native watcher coverage, even with chat hidden.
        turn.next_scan = Some(now + Duration::from_secs(2));
        let protected = turn.protected.clone();
        let background = self.chat.background || agent != self.chat.active_agent;
        self.with_agent_context(agent, background, |app| {
            app.checkpoint_job(JobKind::Capture, Some(id.clone()), false, move || {
                journal
                    .lock()
                    .map_err(|_| "Checkpoint worker failed")?
                    .capture_checkpoint(checkpoint, protected, &paths)?;
                Ok(Reply::Captured(id))
            });
        });
    }

    fn poll_chat_rollback_reply(&mut self, context: &JobContext) -> bool {
        let result = self
            .chat
            .rollback
            .receiver
            .as_ref()
            .and_then(|rx| match rx.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Disconnected) => Some(ReviewReply {
                    result: Err("Checkpoint worker stopped unexpectedly".into()),
                    reviews: None,
                }),
                Err(mpsc::TryRecvError::Empty) => None,
            });
        let changed = result.is_some();
        if let Some(ReviewReply { result, reviews }) = result {
            if let Some(reviews) = reviews {
                self.chat.rollback.inline_root = reviews.root;
                self.chat.rollback.inline_reviews = Arc::new(reviews.files);
                self.chat.rollback.inline_revision =
                    self.chat.rollback.inline_revision.wrapping_add(1);
            }
            self.chat.rollback.receiver = None;
            self.chat.rollback.job = None;
            self.chat.rollback.mutating = false;
            let completed_paths = std::mem::take(&mut self.chat.rollback.write_paths);
            for path in &completed_paths {
                self.chat
                    .rollback
                    .disk_barriers
                    .insert(path.clone(), Instant::now());
            }
            // Always get fresh metadata, including after a partial failure.
            let reload_paths: Vec<_> = self
                .tabs
                .iter()
                .chain(self.groups.iter().flat_map(|g| &g.tabs))
                .filter_map(|t| t.editor.file_path.as_ref())
                .filter(|path| {
                    let path = self.chat.rollback.path_key(path);
                    completed_paths.iter().any(|p| path.starts_with(p))
                })
                .cloned()
                .collect();
            self.queue_file_disk_checks(reload_paths.clone());
            // An older reload may already be running: request a second read so
            // that its result cannot install an intermediate file version.
            for path in reload_paths {
                self.refresh_pending_file_reload(path);
            }
            match result {
                Ok(Reply::Loaded(journal)) => {
                    self.chat.rollback.initialized = true;
                    self.chat.rollback.initialization_error = None;
                    self.chat.rollback.enabled = journal.is_some();
                    self.chat.rollback.journal = journal.map(|j| Arc::new(Mutex::new(*j)));
                }
                Ok(Reply::Begun(id, session_id, content, source)) => {
                    if let Some(turn) = self
                        .chat
                        .rollback
                        .turns
                        .get_mut(&(context.agent, session_id.clone()))
                    {
                        turn.checkpoint = Some(id);
                        turn.next_scan = Some(Instant::now() + Duration::from_secs(2));
                    }
                    self.chat.rollback.active = true;
                    if let Some(session) = self.chat.sessions.get_mut(&session_id)
                        && let Some(index) = session
                            .entries
                            .iter()
                            .rposition(|e| matches!(e, ChatEntry::User(_)))
                    {
                        session.set_checkpoint(index, id);
                    }
                    let cancelled = self
                        .chat
                        .sessions
                        .get(&session_id)
                        .is_none_or(|s| s.cancelling || !s.busy);
                    let sent = !cancelled
                        && self.send_acp(Command::Prompt {
                            session_id: session_id.clone(),
                            content,
                        });
                    drop(source); // The hosted command queue has accepted ownership.
                    self.queued_checkpoint_finished(&session_id, sent, cancelled);
                    if !sent && let Some(session) = self.chat.sessions.get_mut(&session_id) {
                        session.finish(None);
                    }
                }
                Ok(Reply::Finished(id)) => {
                    self.chat.rollback.turns.remove(&(context.agent, id));
                    if !self.chat.control_busy {
                        self.chat.status = "Changes ready for review".into();
                    }
                }
                Ok(Reply::Captured(id)) => {
                    if let Some(turn) = self.chat.rollback.turns.get_mut(&(context.agent, id)) {
                        turn.capturing = false;
                        turn.capture_paths.clear();
                        turn.capture_error = None;
                        turn.next_scan = Some(Instant::now() + Duration::from_secs(2));
                    }
                }
                Ok(Reply::Accepted) => {
                    if !self.chat.control_busy {
                        self.chat.status = "Changes accepted".into();
                    }
                    if context.editor {
                        self.notify("AI changes accepted");
                    }
                }
                Ok(Reply::Planned(plan)) => {
                    if let Some(workspace) = self.remote_workspace.clone() {
                        let paths = self.agent_open_paths();
                        self.review_job(JobKind::Review, context.editor, false, move || {
                            workspace.resolve_aliases(&paths).map(|aliases| Reply::Validated(plan, aliases)).map_err(|error| error.to_string())
                        });
                    } else { self.apply_review_plan(plan, context.editor); }
                }
                Ok(Reply::Validated(plan, aliases)) => {
                    let checked: HashMap<_, _> = aliases.iter().map(|(path, identity)| (path, identity)).collect();
                    if self.agent_open_paths().iter().any(|path| !checked.contains_key(path)) {
                        self.chat.error = Some("Open files changed while checking this restore. Retry after file operations finish. No files were changed.".into());
                    } else if self.tabs.iter().chain(self.groups.iter().flat_map(|group| &group.tabs)).any(|tab| {
                        (tab.editor.modified || tab.editor.read_only || tab.editor.bg_loading)
                            && tab.editor.file_path.as_ref().is_some_and(|path| checked.get(path).is_some_and(|identity| identity.is_none()))
                    }) {
                        self.chat.error = Some("An open file could not be checked for aliases. Retry after file operations finish. No files were changed.".into());
                    } else {
                        self.record_agent_aliases(&aliases);
                        self.apply_review_plan(plan, context.editor);
                    }
                }
                Ok(Reply::Applied(paths)) => {
                    for (path, exists) in paths {
                        let key = self.chat.rollback.path_key(&path);
                        let open: Vec<_> = self
                            .tabs
                            .iter()
                            .chain(self.groups.iter().flat_map(|g| &g.tabs))
                            .filter(|tab| self.acp_editor_identity(&tab.editor).is_some_and(|path| self.chat.rollback.path_key(&path) == key))
                            .filter_map(|tab| tab.editor.file_path.clone())
                            .collect();
                        if !exists {
                            // Closing only the canonical spelling would leave
                            // an alias tab able to recreate a rejected AI file.
                            for alias in open {
                                self.close_deleted_file_tabs(&alias, true);
                            }
                            self.close_deleted_file_tabs(&path, true);
                            continue;
                        }
                        for path in open {
                            self.queue_file_reload(path);
                        }
                    }
                    self.refresh_explorer_async();
                    self.refresh_source_control_async();
                    if !self.chat.control_busy {
                        self.chat.status = "Rollback completed; chat history is retained".into();
                    }
                    if context.editor {
                        self.notify("AI changes rejected");
                    }
                }
                Ok(Reply::Review(path, before, after)) => {
                    self.open_acp_diff(&p::Diff::new(path, after).old_text(before));
                }
                Err(error) if context.kind == JobKind::Capture => {
                    let diagnostic = if let Some(turn) = context.session.as_ref().and_then(|id| {
                        self.chat
                            .rollback
                            .turns
                            .get_mut(&(context.agent, id.clone()))
                    }) {
                        turn.capturing = false;
                        turn.changed_paths.append(&mut turn.capture_paths);
                        turn.retry_at = Some(Instant::now() + Duration::from_millis(500));
                        let changed = turn.capture_error.as_deref() != Some(error.as_str());
                        turn.capture_error = Some(error.clone());
                        changed
                    } else {
                        false
                    };
                    if diagnostic {
                        self.chat
                            .diagnostics
                            .push_back(format!("Live change capture will retry: {error}"));
                        while self.chat.diagnostics.len() > 32 {
                            self.chat.diagnostics.pop_front();
                        }
                    }
                    // A shell file may still be changing. Keep recorded edits
                    // and the baseline and retry off the UI. Final capture
                    // remains mandatory and reports any failure.
                }
                Err(error) => {
                    let error = if context.kind == JobKind::Initialize {
                        let error = format!("Checkpoints unavailable: {error}");
                        self.chat.rollback.initialization_error = Some(error.clone());
                        error
                    } else {
                        error
                    };
                    if context.editor {
                        self.notify(&error);
                    }
                    if let Some(id) = &context.session {
                        let key = (context.agent, id.clone());
                        if context.kind == JobKind::Begin {
                            self.chat.rollback.turns.remove(&key);
                            self.queued_checkpoint_finished(id, false, false);
                            if let Some(session) = self.chat.sessions.get_mut(id) {
                                session.finish(None);
                                session.push(ChatEntry::Error(format!(
                                    "Message was not sent: {error}"
                                )));
                            }
                        } else if context.kind == JobKind::Finish
                            && let Some(turn) = self.chat.rollback.turns.get_mut(&key)
                        {
                            turn.failed = true;
                            turn.finishing = false;
                        }
                    }
                    if context.session.is_some() && context.session != self.chat.selected {
                        self.chat
                            .views
                            .entry(context.session.clone())
                            .or_default()
                            .error = Some(error);
                    } else {
                        self.chat.error = Some(error);
                    }
                }
            }
            self.refresh_chat_rollback();
            if let Some(picker) = self.chat.picker.clone() {
                self.build_chat_choices(picker);
            }
        }
        changed
    }

    fn apply_review_plan(&mut self, plan: Plan, editor: bool) {
        let paths: Vec<_> = plan.paths().map(|path| self.chat.rollback.root.join(path)).collect();
        if let Some(reason) = self.rollback_buffer_conflict(&paths) {
            if editor { self.notify(&reason); }
            self.chat.error = Some(reason);
        } else if let Some(journal) = self.chat.rollback.journal.clone() {
            self.chat.rollback.write_paths = paths.iter().map(|path| self.chat.rollback.path_key(path)).collect();
            self.review_job(JobKind::Review, editor, true, move || {
                journal.lock().map_err(|_| "Checkpoint worker failed")?.apply(plan).map(Reply::Applied)
            });
        }
    }

    fn rollback_buffer_conflict(&self, paths: &[PathBuf]) -> Option<String> {
        if self.settings_effective.read_only.unwrap_or(false) {
            return Some("The editor is read-only".into());
        }
        if self.unresolved_agent_files() || (self.remote_workspace.is_some() && self.pending_file_saves.keys().any(|path| crate::remote::workspace::is_view_key(path))) {
            return Some("Wait for remote files to finish opening or saving. No files were changed.".into());
        }
        for path in paths {
            let canonical = normalize(&resolve_review_path(path));
            let same = |other: &Path| {
                Self::same_open_path(&normalize(other), &normalize(path))
                    || Self::same_open_path(&normalize(&resolve_review_path(other)), &canonical)
            };
            if self.pending_file_saves.keys().any(|p| same(p))
                || self
                    .tabs
                    .iter()
                    .chain(self.groups.iter().flat_map(|g| &g.tabs))
                    .any(|t| {
                        t.kind == TabKind::File
                            && (t.editor.file_path.as_ref().is_some_and(|p| same(p))
                                || self.acp_editor_identity(&t.editor).as_deref().is_some_and(same))
                            && (t.editor.modified || t.editor.read_only || t.editor.bg_loading)
                    })
            {
                return Some(format!(
                    "{} has unsaved edits, is read-only, or has a pending save. No files were changed.",
                    path.display()
                ));
            }
        }
        None
    }

    fn review_available(&mut self) -> bool {
        if self.checkpoint_storage_blocked().is_some() {
            return false;
        }
        if !self.chat.rollback.enabled {
            self.chat.error = Some("Rollback is available only in a Git working tree. Agent chat and tools remain available.".into());
            return false;
        }
        if self.chat.rollback.journal.is_none() {
            self.chat.error =
                Some("Change review is still initializing. Try again when it is ready.".into());
            return false;
        }
        true
    }

    fn rollback_ready(&mut self) -> bool {
        if !self.review_available() {
            return false;
        }
        if self.chat.rollback.busy()
            || self.chat.rollback.active
            || self.chat.all_sessions().any(|s| {
                s.busy
                    || s.terminals
                        .values()
                        .any(|t| !t.released && t.exit_status.is_none())
            })
            || self.chat.rollback.file_writes.load(Ordering::Acquire) != 0
        {
            self.chat.error =
                Some("Wait for responses, file writes, and checkpoint capture to finish.".into());
            return false;
        }
        true
    }

    pub(super) fn resolve_chat_changes(&mut self, selection: Selection, accept: bool) {
        if accept {
            if !self.review_available() {
                return;
            }
            let Some(session) = self.chat.session() else {
                return;
            };
            match session.rollback.pending_chunks(&selection) {
                Ok(chunks) => self.accept_changes(false, chunks),
                Err(error) => self.chat.error = Some(error),
            }
            return;
        }
        if !self.rollback_ready() {
            return;
        }
        let Some(session) = self.chat.selected.clone() else {
            return;
        };
        let Some(journal) = self.chat.rollback.journal.clone() else {
            return;
        };
        let owner = self.chat.checkpoint_owner();
        self.chat.error = None;
        self.rollback_job(false, move || {
            journal
                .lock()
                .map_err(|_| "Checkpoint worker failed")?
                .plan(&owner, &session.to_string(), &selection)
                .map(Reply::Planned)
        });
    }

    fn accept_changes(&mut self, editor: bool, chunks: Vec<(u64, usize)>) {
        let Some(journal) = self.chat.rollback.journal.clone() else {
            return;
        };
        self.chat.error = None;
        self.review_job(JobKind::Accept, editor, false, move || {
            journal
                .lock()
                .map_err(|_| "Checkpoint worker failed")?
                .accept_chunks(&chunks)?;
            Ok(Reply::Accepted)
        });
    }

    fn review_job(
        &mut self,
        kind: JobKind,
        editor: bool,
        mutating: bool,
        job: impl FnOnce() -> Result<Reply, String> + Send + 'static,
    ) {
        self.chat.rollback.jobs.push_back(CheckpointJob {
            context: JobContext {
                agent: self.chat.active_agent,
                session: self.chat.selected.clone(),
                kind,
                editor,
            },
            mutating,
            run: Box::new(job),
        });
        self.start_checkpoint_job();
    }

    pub(crate) fn resolve_editor_changes(
        &mut self,
        path: &Path,
        chunk: Option<(u64, usize)>,
        accept: bool,
    ) {
        let ready = if accept {
            self.review_available()
        } else {
            self.rollback_ready()
        };
        if !ready {
            if let Some(error) = self.chat.error.clone() {
                self.notify(&error);
            }
            return;
        }
        let Some(relative) = self.chat.rollback.relative(path) else {
            return;
        };
        // Check the captured identity again: a stale click must not act on a
        // different tab, file, or a chunk that was resolved in the meantime.
        let Some(file) = self.chat.rollback.file_review(path) else {
            return;
        };
        if chunk.is_some_and(|(id, index)| {
            !file.chunks.iter().any(|c| c.edit == id && c.index == index)
        }) {
            return;
        }
        if accept {
            // Bind to the preview now: a queued checkpoint may record more
            // chunks before this metadata-only job reaches the worker.
            let chunks = file
                .chunks
                .iter()
                .map(|c| (c.edit, c.index))
                .filter(|id| chunk.is_none_or(|selected| selected == *id))
                .collect();
            self.accept_changes(true, chunks);
            return;
        }
        let selection = chunk.map_or(Selection::File(relative), |(id, index)| {
            Selection::Chunk(id, index)
        });
        let Some(journal) = self.chat.rollback.journal.clone() else {
            return;
        };
        self.chat.error = None;
        self.review_job(JobKind::Review, true, false, move || {
            journal
                .lock()
                .map_err(|_| "Checkpoint worker failed")?
                .plan_editor(&selection)
                .map(Reply::Planned)
        });
    }

    pub(super) fn review_chat_change(&mut self, id: u64) {
        if self.chat.rollback.busy() {
            return;
        }
        let Some(journal) = self.chat.rollback.journal.clone() else {
            return;
        };
        self.rollback_job(false, move || {
            let (path, before, after) = journal
                .lock()
                .map_err(|_| "Checkpoint worker failed")?
                .review(id)?;
            Ok(Reply::Review(path, before, after))
        });
    }

    pub(super) fn undo_chat_rollback(&mut self) {
        if !self.rollback_ready() {
            return;
        }
        let Some(session) = self.chat.selected.clone() else {
            return;
        };
        let Some(journal) = self.chat.rollback.journal.clone() else {
            return;
        };
        let owner = self.chat.checkpoint_owner();
        self.chat.error = None;
        self.rollback_job(false, move || {
            journal
                .lock()
                .map_err(|_| "Checkpoint worker failed")?
                .undo_plan(&owner, &session.to_string())
                .map(Reply::Planned)
        });
    }

    pub(super) fn recover_chat_rollback(&mut self) {
        if !self.rollback_ready() {
            return;
        }
        // Recovery can involve any file in the interrupted transaction. Require
        // clean buffers throughout this workspace before starting it.
        let paths: Vec<_> = self
            .tabs
            .iter()
            .chain(self.groups.iter().flat_map(|g| &g.tabs))
            .filter_map(|t| t.editor.file_path.clone())
            .chain(self.pending_file_saves.keys().cloned())
            .collect();
        if let Some(error) = self.rollback_buffer_conflict(&paths) {
            self.chat.error = Some(error);
            return;
        }
        let Some(journal) = self.chat.rollback.journal.clone() else {
            return;
        };
        self.chat.rollback.write_paths = vec![self.chat.rollback.path_key(&self.chat.rollback.root)];
        self.rollback_job(true, move || {
            journal
                .lock()
                .map_err(|_| "Checkpoint worker failed")?
                .recover()
                .map(Reply::Applied)
        });
    }

    pub(super) fn build_rollback_choices(&mut self, picker: &Picker) -> bool {
        if !matches!(
            picker,
            Picker::Changes
                | Picker::ChangedFile(_)
                | Picker::Change(_)
                | Picker::ChangeChunk(_, _)
                | Picker::Checkpoints
                | Picker::Restore(_)
        ) {
            return false;
        }
        let summary = self
            .chat
            .session()
            .map(|s| s.rollback.clone())
            .unwrap_or_default();
        let mut choices = Vec::new();
        let mut add = |label: String, detail: String, action| {
            choices.push(Choice {
                label,
                detail,
                action,
            })
        };
        let resolve = |selection, accept| ChatAction::Resolve(selection, accept);
        if !self.chat.rollback.enabled {
            if let Some(error) = self.chat.rollback.initialization_error() {
                add(
                    "Retry checkpoints".into(),
                    error.into(),
                    ChatAction::RetryCheckpointStorage,
                );
            } else if self.chat.rollback.initializing() {
                add(
                    "Opening checkpoints…".into(),
                    "Submitted messages wait until storage is ready.".into(),
                    ChatAction::DismissPicker,
                );
            } else {
                add(
                    "Rollback unavailable".into(),
                    "A Git working tree is required; agent chat and tools still work.".into(),
                    ChatAction::DismissPicker,
                );
            }
        } else {
            match picker {
                Picker::Changes => {
                    if summary.can_undo {
                        add(
                            "Undo last rollback".into(),
                            "Recover declined changes and their previous review state".into(),
                            ChatAction::UndoRollback,
                        );
                    }
                    if summary.interrupted {
                        add(
                            "Recover rollback".into(),
                            "Restore the state before the interrupted rollback".into(),
                            ChatAction::RecoverRollback,
                        );
                    }
                    if self
                        .chat
                        .rollback
                        .capture_failed(self.chat.active_agent, self.chat.selected.as_ref())
                    {
                        add(
                            "Retry checkpoint capture".into(),
                            "Original snapshots are still retained".into(),
                            ChatAction::RetryCheckpoint,
                        );
                    }
                    let pending: usize = summary.edits.iter().map(|e| e.pending()).sum();
                    if pending > 0 {
                        add(
                            format!("Accept all {pending} chunks"),
                            "Keep files as they are and clear pending marks".into(),
                            resolve(Selection::All, true),
                        );
                        add(
                            format!("Decline all {pending} chunks"),
                            "Reverse pending changes; stop on any conflict".into(),
                            resolve(Selection::All, false),
                        );
                    }
                    add(
                        "Message checkpoints".into(),
                        "Restore files to before a user message".into(),
                        ChatAction::Picker(Picker::Checkpoints),
                    );
                    let paths: BTreeSet<_> = summary
                        .edits
                        .iter()
                        .filter(|e| e.pending() > 0)
                        .map(|e| e.path.clone())
                        .collect();
                    for path in paths {
                        let count: usize = summary
                            .edits
                            .iter()
                            .filter(|e| e.path == path)
                            .map(|e| e.pending())
                            .sum();
                        add(
                            format!("● {}", path.display()),
                            format!("{count} pending chunks"),
                            ChatAction::Picker(Picker::ChangedFile(path)),
                        );
                    }
                    if pending == 0 {
                        add(
                            "No pending changes".into(),
                            "Accepted changes remain in checkpoint history".into(),
                            ChatAction::DismissPicker,
                        );
                    }
                }
                Picker::ChangedFile(path) => {
                    add(
                        "Accept file".into(),
                        path.display().to_string(),
                        resolve(Selection::File(path.clone()), true),
                    );
                    add(
                        "Decline file".into(),
                        "Reverse all pending chunks in this file".into(),
                        resolve(Selection::File(path.clone()), false),
                    );
                    for edit in summary
                        .edits
                        .iter()
                        .filter(|e| e.path == *path && e.pending() > 0)
                    {
                        add(
                            format!("Change {} · {} chunks", edit.id, edit.pending()),
                            edit.conflict.clone().unwrap_or_else(|| {
                                format!("Message checkpoint {}", edit.checkpoint)
                            }),
                            ChatAction::Picker(Picker::Change(edit.id)),
                        );
                    }
                }
                Picker::Change(id) => {
                    add(
                        "Review diff".into(),
                        "Open the complete saved change".into(),
                        ChatAction::ReviewChange(*id),
                    );
                    add(
                        "Accept change".into(),
                        String::new(),
                        resolve(Selection::Edit(*id), true),
                    );
                    add(
                        "Decline change".into(),
                        String::new(),
                        resolve(Selection::Edit(*id), false),
                    );
                    if let Some(edit) = summary.edits.iter().find(|e| e.id == *id) {
                        for (i, chunk) in
                            edit.chunks.iter().enumerate().filter(|(_, c)| {
                                c.decision == vtcode::acp::rollback::Decision::Pending
                            })
                        {
                            add(
                                format!(
                                    "Chunk {} · line {} · {}",
                                    i + 1,
                                    chunk.line,
                                    chunk.preview
                                ),
                                edit.path.display().to_string(),
                                ChatAction::Picker(Picker::ChangeChunk(*id, i)),
                            );
                        }
                    }
                }
                Picker::ChangeChunk(id, i) => {
                    add(
                        format!("Review change · chunk {}", i + 1),
                        "Chunk order follows the saved diff".into(),
                        ChatAction::ReviewChange(*id),
                    );
                    add(
                        "Accept chunk".into(),
                        String::new(),
                        resolve(Selection::Chunk(*id, *i), true),
                    );
                    add(
                        "Decline chunk".into(),
                        String::new(),
                        resolve(Selection::Chunk(*id, *i), false),
                    );
                }
                Picker::Checkpoints => {
                    for cp in &summary.checkpoints {
                        add(
                            format!("Before message {} · {}", cp.id, cp.label),
                            "Restore this chat's files; retain the conversation".into(),
                            ChatAction::Picker(Picker::Restore(cp.id)),
                        );
                    }
                    if summary.checkpoints.is_empty() {
                        add(
                            "No message checkpoints yet".into(),
                            "A checkpoint is made before sending each message in Git workspaces"
                                .into(),
                            ChatAction::DismissPicker,
                        );
                    }
                }
                Picker::Restore(id) => {
                    add(
                        "Restore files before this message".into(),
                        "Includes accepted changes. Later user edits are checked for conflicts."
                            .into(),
                        resolve(Selection::Checkpoint(*id), false),
                    );
                    let paths: BTreeSet<_> = summary
                        .edits
                        .iter()
                        .filter(|e| {
                            e.checkpoint >= *id
                                && e.chunks.iter().any(|c| {
                                    c.decision != vtcode::acp::rollback::Decision::Declined
                                })
                        })
                        .map(|e| e.path.clone())
                        .collect();
                    for path in paths {
                        add(
                            path.display().to_string(),
                            "Affected file".into(),
                            ChatAction::Picker(Picker::ChangedFile(path)),
                        );
                    }
                }
                _ => {}
            }
        }
        self.chat.choices = choices;
        self.chat.picker = Some(picker.clone());
        true
    }
}
