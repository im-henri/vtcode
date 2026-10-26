//! Agent checkpoints execute beside the real files. The workbench retains only
//! immutable review data; drawing and inspecting changes never issue an RPC.
use super::*;
use std::collections::{BTreeSet, HashMap};
use vtcode::acp::rollback as native;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Command {
    Open,
    Run {
        journal: u64,
        known: u64,
        action: Action,
    },
    Close {
        journal: u64,
    },
}
impl Command {
    pub(super) fn retained_bytes(&self) -> usize {
        fn paths(paths: &[RemotePath]) -> usize {
            paths
                .iter()
                .fold(0usize, |bytes, path| bytes.saturating_add(path.0.len()))
        }
        let Self::Run { action, .. } = self else {
            return text::INLINE_BYTES * 4;
        };
        let response = match action {
            Action::Review(_) => 16 * 1024 * 1024,
            Action::Plan { .. } | Action::Undo { .. } => MAX_PAYLOAD_BYTES,
            _ => text::INLINE_BYTES * 4,
        };
        let input = match action {
            Action::Begin {
                owner,
                session,
                message,
                protected,
            } => owner
                .len()
                .saturating_add(session.len())
                .saturating_add(message.len())
                .saturating_add(paths(protected)),
            Action::Finish { protected, .. } => paths(protected),
            Action::Capture {
                protected,
                paths: selected,
                ..
            } => paths(protected).saturating_add(paths(selected)),
            Action::Apply(token) => token.len(),
            Action::Accept(chunks) => chunks
                .len()
                .saturating_mul(std::mem::size_of::<(u64, usize)>()),
            Action::Plan { owner, selection } => owner
                .as_ref()
                .map_or(0, |(o, s)| o.len().saturating_add(s.len()))
                .saturating_add(match selection {
                    Selection::File(path) => path.0.len(),
                    _ => 0,
                }),
            Action::Undo { owner, session } => owner.len().saturating_add(session.len()),
            _ => 0,
        };
        response.saturating_add(input)
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Action {
    Begin {
        owner: String,
        session: String,
        message: String,
        protected: Vec<RemotePath>,
    },
    Finish {
        checkpoint: u64,
        protected: Vec<RemotePath>,
    },
    Capture {
        checkpoint: u64,
        protected: Vec<RemotePath>,
        paths: Vec<RemotePath>,
    },
    FinishUncertain,
    Accept(Vec<(u64, usize)>),
    Plan {
        owner: Option<(String, String)>,
        selection: Selection,
    },
    Undo {
        owner: String,
        session: String,
    },
    Review(u64),
    Apply(String),
    Recover,
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Selection {
    All,
    File(RemotePath),
    Edit(u64),
    Chunk(u64, usize),
    Checkpoint(u64),
}
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Reply {
    pub journal: u64,
    pub opened: bool,
    version: u64,
    pub(super) state: Option<text::Data>,
    outcome: Result<Outcome, String>,
}
#[derive(Debug, Serialize, Deserialize)]
enum Outcome {
    Disabled,
    Done,
    Begun(u64),
    Planned {
        token: String,
        paths: Vec<RemotePath>,
    },
    Applied(Vec<(RemotePath, bool)>),
    Review {
        path: RemotePath,
        before: String,
        after: String,
    },
}
#[derive(Serialize, Deserialize)]
struct Snapshot {
    metadata: native::View,
    paths: Vec<RemotePath>,
    reviews: Vec<Review>,
}
#[derive(Serialize, Deserialize)]
struct Review {
    path: RemotePath,
    text: Option<std::sync::Arc<str>>,
    chunks: Vec<native::InlineChunk>,
}
#[derive(Serialize, Deserialize)]
struct PlanEnvelope {
    root: String,
    plan: native::Plan,
}
struct Entry {
    root: PathBuf,
    journal: native::Journal,
    published: Option<(u64, HashMap<RemotePath, std::sync::Arc<str>>)>,
}
#[derive(Default)]
pub(super) struct Manager {
    next: u64,
    entries: HashMap<u64, Entry>,
}
impl Manager {
    pub(super) fn close(&mut self, journal: u64) {
        self.entries.remove(&journal);
    }
    pub(super) fn execute(
        &mut self,
        root: &Path,
        command: Command,
        store: &std::sync::Mutex<text::Store>,
    ) -> io::Result<super::Reply> {
        let (id, opened, known, action) = match command {
            Command::Close { journal } => {
                self.close(journal);
                return Ok(super::Reply::Done);
            }
            Command::Open => {
                match git2::Repository::discover(root) {
                    Err(e) if e.code() == git2::ErrorCode::NotFound => {
                        return Ok(super::Reply::Rollback(Reply {
                            journal: 0,
                            opened: false,
                            version: 0,
                            state: None,
                            outcome: Ok(Outcome::Disabled),
                        }));
                    }
                    Err(e) => return Err(io::Error::other(e)),
                    Ok(repo) if repo.is_bare() => {
                        return Ok(super::Reply::Rollback(Reply {
                            journal: 0,
                            opened: false,
                            version: 0,
                            state: None,
                            outcome: Ok(Outcome::Disabled),
                        }));
                    }
                    _ => {}
                }
                if self.entries.len() >= 8 {
                    return Err(io::Error::other("Remote checkpoint journals are busy"));
                }
                let directory = crate::portable::workspace_dir(root).join("agent-rollback");
                let mut journal =
                    native::Journal::open(root, &directory).map_err(io::Error::other)?;
                if journal.active().is_some() {
                    journal.finish_uncertain().map_err(io::Error::other)?;
                }
                self.next = self
                    .next
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("Journal ID overflow"))?;
                let id = self.next;
                self.entries.insert(
                    id,
                    Entry {
                        root: root.into(),
                        journal,
                        published: None,
                    },
                );
                (id, true, None, None)
            }
            Command::Run {
                journal,
                known,
                action,
            } => (journal, false, Some(known), Some(action)),
        };
        let response = (|| {
            let entry = self
                .entries
                .get_mut(&id)
                .ok_or_else(|| io::Error::other("Remote checkpoint journal is closed"))?;
            if entry.root != root {
                return Err(io::Error::other("Checkpoint belongs to another workspace"));
            }
            let outcome = action.map_or(Ok(Outcome::Done), |action| execute(entry, action));
            // A no-change live capture neither scans preview files again nor
            // serializes/transfers the cached journal history.
            let state = if known != Some(entry.journal.version()) {
                Some(text::Data::encode(store, &snapshot(entry, known)?)?)
            } else {
                None
            };
            Ok(super::Reply::Rollback(Reply {
                journal: id,
                opened,
                version: entry.journal.version(),
                state,
                outcome,
            }))
        })();
        if opened && response.is_err() {
            self.entries.remove(&id);
        }
        response
    }
}

fn relative(root: &Path, path: &Path) -> io::Result<RemotePath> {
    let path = git::from_native(path, root)?;
    if path.1 {
        return Err(io::Error::other("Checkpoint path is outside its workspace"));
    }
    Ok(path)
}
fn native_paths(paths: Vec<RemotePath>) -> Result<BTreeSet<PathBuf>, String> {
    paths
        .into_iter()
        .map(|path| {
            if path.1 {
                return Err("Checkpoint selectors must be workspace-relative".into());
            }
            path.resolve(Path::new("")).map_err(|e| e.to_string())
        })
        .collect()
}
fn selection(value: Selection) -> Result<native::Selection, String> {
    Ok(match value {
        Selection::All => native::Selection::All,
        Selection::File(path) => {
            native::Selection::File(native_paths(vec![path])?.into_iter().next().unwrap())
        }
        Selection::Edit(id) => native::Selection::Edit(id),
        Selection::Chunk(id, chunk) => native::Selection::Chunk(id, chunk),
        Selection::Checkpoint(id) => native::Selection::Checkpoint(id),
    })
}
fn plan(entry: &Entry, plan: native::Plan) -> Result<Outcome, String> {
    let paths = plan
        .paths()
        .map(|path| relative(&entry.root, &entry.root.join(path)).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    let envelope = PlanEnvelope {
        root: url::Url::from_directory_path(&entry.root)
            .map_err(|_| "Invalid checkpoint root")?
            .into(),
        plan,
    };
    let token = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&envelope).map_err(|e| e.to_string())?);
    Ok(Outcome::Planned { token, paths })
}
fn applied(entry: &Entry, paths: Vec<PathBuf>) -> Result<Outcome, String> {
    Ok(Outcome::Applied(
        paths
            .into_iter()
            .map(|path| {
                let exists = !std::fs::metadata(&path).is_err_and(|error| {
                    matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    )
                });
                relative(&entry.root, &path)
                    .map(|path| (path, exists))
                    .map_err(|e| e.to_string())
            })
            .collect::<Result<_, _>>()?,
    ))
}
fn execute(entry: &mut Entry, action: Action) -> Result<Outcome, String> {
    match action {
        Action::Begin {
            owner,
            session,
            message,
            protected,
        } => entry
            .journal
            .begin(&owner, &session, &message, native_paths(protected)?)
            .map(Outcome::Begun),
        Action::Finish {
            checkpoint,
            protected,
        } => entry
            .journal
            .finish_checkpoint(checkpoint, native_paths(protected)?)
            .map(|_| Outcome::Done),
        Action::Capture {
            checkpoint,
            protected,
            paths,
        } => entry
            .journal
            .capture_checkpoint(checkpoint, native_paths(protected)?, &native_paths(paths)?)
            .map(|_| Outcome::Done),
        Action::FinishUncertain => entry.journal.finish_uncertain().map(|_| Outcome::Done),
        Action::Accept(chunks) => entry.journal.accept_chunks(&chunks).map(|_| Outcome::Done),
        Action::Plan {
            owner,
            selection: selected,
        } => {
            let selected = selection(selected)?;
            let planned = match owner {
                Some((owner, session)) => entry.journal.plan(&owner, &session, &selected),
                None => entry.journal.plan_editor(&selected),
            }?;
            plan(entry, planned)
        }
        Action::Undo { owner, session } => plan(entry, entry.journal.undo_plan(&owner, &session)?),
        Action::Review(id) => {
            let (path, before, after) = entry.journal.review(id)?;
            Ok(Outcome::Review {
                path: relative(&entry.root, &path).map_err(|e| e.to_string())?,
                before,
                after,
            })
        }
        Action::Apply(token) => {
            let bytes = URL_SAFE_NO_PAD.decode(token).map_err(|e| e.to_string())?;
            let envelope: PlanEnvelope =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            let root = url::Url::from_directory_path(&entry.root)
                .map_err(|_| "Invalid checkpoint root")?
                .to_string();
            if envelope.root != root {
                return Err("Restore plan belongs to another workspace".into());
            }
            let paths = entry.journal.apply(envelope.plan)?;
            applied(entry, paths)
        }
        Action::Recover => {
            let paths = entry.journal.recover()?;
            applied(entry, paths)
        }
    }
}
fn snapshot(entry: &mut Entry, known: Option<u64>) -> io::Result<Snapshot> {
    let mut metadata = entry.journal.view();
    metadata.root.clear();
    let paths = metadata
        .edits
        .iter_mut()
        .map(|edit| {
            let path = std::mem::take(&mut edit.path);
            relative(&entry.root, &entry.root.join(path))
        })
        .collect::<io::Result<_>>()?;
    let previous = entry
        .published
        .as_ref()
        .filter(|(version, _)| Some(*version) == known);
    let mut published = HashMap::new();
    let reviews = entry
        .journal
        .inline_reviews()
        .into_iter()
        .map(|file| {
            let path = relative(&entry.root, &file.path)?;
            let reused = previous
                .and_then(|(_, texts)| texts.get(&path))
                .is_some_and(|text| text == &file.text);
            published.insert(path.clone(), file.text.clone());
            Ok(Review {
                path,
                text: (!reused).then_some(file.text),
                chunks: file.chunks,
            })
        })
        .collect::<io::Result<_>>()?;
    entry.published = Some((entry.journal.version(), published));
    Ok(Snapshot {
        metadata,
        paths,
        reviews,
    })
}

pub(crate) struct Journal {
    workspace: workspace::Workspace,
    id: u64,
    version: u64,
    view: native::View,
    reviews: Vec<native::FileReview>,
}
pub(crate) struct Plan {
    pub token: String,
    pub paths: Vec<PathBuf>,
    root: PathBuf,
}
impl Drop for Journal {
    fn drop(&mut self) {
        self.workspace.client.close_rollback(self.id);
    }
}
impl Journal {
    pub(crate) fn open(workspace: workspace::Workspace) -> Result<Option<Self>, String> {
        let super::Reply::Rollback(reply) = workspace
            .client
            .call_file(super::Command::Rollback(Command::Open))
            .map_err(|e| e.to_string())?
        else {
            return Err("Invalid checkpoint open reply".into());
        };
        if matches!(reply.outcome, Ok(Outcome::Disabled)) {
            return Ok(None);
        }
        if !reply.opened || reply.journal == 0 {
            return Err("Invalid remote checkpoint identity".into());
        }
        let mut journal = Self {
            workspace,
            id: reply.journal,
            version: 0,
            view: Default::default(),
            reviews: Vec::new(),
        };
        journal.update(reply)?;
        Ok(Some(journal))
    }
    fn update(&mut self, reply: Reply) -> Result<Outcome, String> {
        if reply.journal != self.id {
            return Err("Checkpoint response identity mismatch".into());
        }
        if let Some(data) = reply.state {
            let mut snapshot: Snapshot = data.decode(&self.workspace).map_err(|e| e.to_string())?;
            if snapshot.metadata.edits.len() != snapshot.paths.len() {
                return Err("Invalid checkpoint path metadata".into());
            }
            snapshot.metadata.root = self.workspace.view_root.clone();
            for (edit, path) in snapshot.metadata.edits.iter_mut().zip(snapshot.paths) {
                edit.path = self.relative_key(&path)?;
            }
            let cached: HashMap<_, _> = self
                .reviews
                .iter()
                .map(|file| (file.path.as_path(), &file.text))
                .collect();
            let reviews = snapshot
                .reviews
                .into_iter()
                .map(|review| {
                    let path = self
                        .workspace
                        .view_root
                        .join(self.relative_key(&review.path)?);
                    let text = review
                        .text
                        .or_else(|| cached.get(path.as_path()).map(|text| (*text).clone()))
                        .ok_or("Missing cached checkpoint preview")?;
                    Ok(native::FileReview {
                        path,
                        text,
                        chunks: review.chunks,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            self.view = snapshot.metadata;
            self.reviews = reviews;
        }
        self.version = reply.version;
        reply.outcome
    }
    fn run(&mut self, action: Action) -> Result<Outcome, String> {
        let super::Reply::Rollback(reply) = self
            .workspace
            .client
            .call_file(super::Command::Rollback(Command::Run {
                journal: self.id,
                known: self.version,
                action,
            }))
            .map_err(|e| e.to_string())?
        else {
            return Err("Invalid checkpoint reply".into());
        };
        self.update(reply)
    }
    fn relative_key(&self, path: &RemotePath) -> Result<PathBuf, String> {
        if path.1 {
            return Err("Checkpoint returned an absolute selector".into());
        }
        self.workspace
            .view_key(path)
            .strip_prefix(&self.workspace.view_root)
            .map(Path::to_path_buf)
            .map_err(|e| e.to_string())
    }
    fn paths(&self, paths: &BTreeSet<PathBuf>) -> Result<Vec<RemotePath>, String> {
        paths
            .iter()
            .map(|path| {
                let path = self
                    .workspace
                    .remote_path(&self.workspace.view_root.join(path))
                    .map_err(|e| e.to_string())?;
                if path.1 {
                    return Err("Checkpoint selector is outside its workspace".into());
                }
                Ok(path)
            })
            .collect()
    }
    fn selected(&self, selected: &native::Selection) -> Result<Selection, String> {
        Ok(match selected {
            native::Selection::All => Selection::All,
            native::Selection::File(path) => {
                Selection::File(self.paths(&BTreeSet::from([path.clone()]))?.remove(0))
            }
            native::Selection::Edit(id) => Selection::Edit(*id),
            native::Selection::Chunk(id, index) => Selection::Chunk(*id, *index),
            native::Selection::Checkpoint(id) => Selection::Checkpoint(*id),
        })
    }
    pub(crate) fn workspace_root(&self) -> &Path {
        &self.workspace.view_root
    }
    pub(crate) fn active(&self) -> Option<u64> {
        self.view.active.first().copied()
    }
    pub(crate) fn summary(&self, owner: &str, session: &str) -> native::Summary {
        self.view.summary(owner, session)
    }
    pub(crate) fn inline_reviews(&self) -> Vec<native::FileReview> {
        self.reviews.clone()
    }
    pub(crate) fn begin(
        &mut self,
        owner: &str,
        session: &str,
        message: &str,
        protected: BTreeSet<PathBuf>,
    ) -> Result<u64, String> {
        let action = Action::Begin {
            owner: owner.into(),
            session: session.into(),
            message: message.into(),
            protected: self.paths(&protected)?,
        };
        match self.run(action)? {
            Outcome::Begun(id) => Ok(id),
            _ => Err("Invalid checkpoint begin reply".into()),
        }
    }
    pub(crate) fn finish_checkpoint(
        &mut self,
        checkpoint: u64,
        protected: BTreeSet<PathBuf>,
    ) -> Result<(), String> {
        let action = Action::Finish {
            checkpoint,
            protected: self.paths(&protected)?,
        };
        self.run(action).map(|_| ())
    }
    pub(crate) fn capture_checkpoint(
        &mut self,
        checkpoint: u64,
        protected: BTreeSet<PathBuf>,
        paths: &BTreeSet<PathBuf>,
    ) -> Result<(), String> {
        let action = Action::Capture {
            checkpoint,
            protected: self.paths(&protected)?,
            paths: self.paths(paths)?,
        };
        self.run(action).map(|_| ())
    }
    pub(crate) fn finish_uncertain(&mut self) -> Result<(), String> {
        self.run(Action::FinishUncertain).map(|_| ())
    }
    pub(crate) fn accept_chunks(&mut self, chunks: &[(u64, usize)]) -> Result<(), String> {
        self.run(Action::Accept(chunks.to_vec())).map(|_| ())
    }
    pub(crate) fn plan(
        &mut self,
        owner: Option<(&str, &str)>,
        selection: &native::Selection,
    ) -> Result<Plan, String> {
        let action = Action::Plan {
            owner: owner.map(|(o, s)| (o.into(), s.into())),
            selection: self.selected(selection)?,
        };
        let outcome = self.run(action)?;
        self.planned(outcome)
    }
    pub(crate) fn undo_plan(&mut self, owner: &str, session: &str) -> Result<Plan, String> {
        let outcome = self.run(Action::Undo {
            owner: owner.into(),
            session: session.into(),
        })?;
        self.planned(outcome)
    }
    fn planned(&self, outcome: Outcome) -> Result<Plan, String> {
        let Outcome::Planned { token, paths } = outcome else {
            return Err("Invalid restore plan reply".into());
        };
        Ok(Plan {
            token,
            paths: paths
                .iter()
                .map(|path| self.relative_key(path))
                .collect::<Result<_, _>>()?,
            root: self.workspace.view_root.clone(),
        })
    }
    pub(crate) fn review(&mut self, id: u64) -> Result<(PathBuf, String, String), String> {
        let Outcome::Review {
            path,
            before,
            after,
        } = self.run(Action::Review(id))?
        else {
            return Err("Invalid change review reply".into());
        };
        Ok((self.workspace.view_key(&path), before, after))
    }
    fn applied(&self, outcome: Outcome) -> Result<Vec<(PathBuf, bool)>, String> {
        let Outcome::Applied(paths) = outcome else {
            return Err("Invalid restore result".into());
        };
        Ok(paths
            .into_iter()
            .map(|(path, exists)| (self.workspace.view_key(&path), exists))
            .collect())
    }
    pub(crate) fn apply(&mut self, plan: Plan) -> Result<Vec<(PathBuf, bool)>, String> {
        if plan.root != self.workspace.view_root {
            return Err("Restore plan belongs to another workspace".into());
        }
        let outcome = self.run(Action::Apply(plan.token))?;
        self.applied(outcome)
    }
    pub(crate) fn recover(&mut self) -> Result<Vec<(PathBuf, bool)>, String> {
        let outcome = self.run(Action::Recover)?;
        self.applied(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_retain_journal_root_and_reject_absolute_capture_selectors() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        git2::Repository::init(first.path()).unwrap();
        git2::Repository::init(second.path()).unwrap();
        let first = first.path().canonicalize().unwrap();
        let second = second.path().canonicalize().unwrap();
        let mut manager = Manager::default();
        let store = std::sync::Mutex::new(text::Store::default());
        let super::super::Reply::Rollback(opened) =
            manager.execute(&first, Command::Open, &store).unwrap()
        else {
            panic!()
        };
        let id = opened.journal;
        let action = || Action::Begin {
            owner: "agent".into(),
            session: "chat".into(),
            message: "test".into(),
            protected: Vec::new(),
        };
        assert!(
            manager
                .execute(
                    &second,
                    Command::Run {
                        journal: id,
                        known: opened.version,
                        action: action()
                    },
                    &store
                )
                .is_err()
        );
        assert!(manager.entries[&id].journal.active().is_none());
        let invalid = Action::Begin {
            owner: "agent".into(),
            session: "chat".into(),
            message: "test".into(),
            protected: vec![RemotePath::absolute(b"/outside/file").unwrap()],
        };
        let super::super::Reply::Rollback(reply) = manager
            .execute(
                &first,
                Command::Run {
                    journal: id,
                    known: opened.version,
                    action: invalid,
                },
                &store,
            )
            .unwrap()
        else {
            panic!()
        };
        assert!(reply.outcome.unwrap_err().contains("relative"));
        assert!(manager.entries[&id].journal.active().is_none());
        let super::super::Reply::Rollback(reply) = manager
            .execute(
                &first,
                Command::Run {
                    journal: id,
                    known: opened.version,
                    action: action(),
                },
                &store,
            )
            .unwrap()
        else {
            panic!()
        };
        assert!(matches!(reply.outcome, Ok(Outcome::Begun(_))));
        manager
            .execute(&second, Command::Close { journal: id }, &store)
            .unwrap();
        assert!(
            manager.entries.is_empty(),
            "connection-wide cleanup must work after a folder switch"
        );
    }
}
