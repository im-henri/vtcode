//! Remember the open folders of each scope so a launch without a path can
//! reopen them. Only settled states are recorded: never while a folder opens,
//! a saved workspace switches, or folders close for quitting.
use super::*;
use crate::last_workspaces::{self, Entry};
use crate::workspace_sets::{Location, SavedSet};

pub(crate) struct State {
    pub(crate) path: PathBuf,
    /// The open folders by workspace ID, in order, and the active ID.
    key: Option<(Vec<(u64, PathBuf)>, u64)>,
    /// What this window last wrote for each scope it has had folders in.
    recorded: Vec<Entry>,
    /// The newest entries not yet written, at most one per scope.
    pending: Vec<Entry>,
    writing: Option<mpsc::Receiver<io::Result<()>>>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            path: last_workspaces::path(),
            key: None,
            recorded: vec![],
            pending: vec![],
            writing: None,
        }
    }
}

fn merge(entries: &mut Vec<Entry>, update: Entry) {
    entries.retain(|entry| entry.scope != update.scope);
    entries.push(update);
}

impl App {
    fn last_workspaces_snapshot(&self) -> Vec<Entry> {
        let base = crate::portable::base_dir();
        let mut entries: Vec<Entry> = vec![];
        for (id, app) in self.ordered_workspaces() {
            if !app.has_open_folder() {
                continue;
            }
            let scope = app.workspace_sets_scope();
            let location = app.remote_workspace.as_ref().map_or_else(
                || Location::local(&app.explorer.root, &base),
                |remote| Location::Remote {
                    uri: remote.client.hello().root_uri.clone(),
                },
            );
            let index = match entries.iter().position(|entry| entry.scope == scope) {
                Some(index) => index,
                None => {
                    entries.push(Entry {
                        scope,
                        roots: vec![],
                        active: 0,
                    });
                    entries.len() - 1
                }
            };
            let entry = &mut entries[index];
            if id == self.workspaces.active {
                entry.active = entry.roots.len();
            }
            entry.roots.push(location);
        }
        entries
    }

    pub(crate) fn poll_last_workspaces(&mut self) {
        if !self.settings_persistence_enabled {
            return;
        }
        if let Some(writing) = &self.workspaces.last.writing {
            match writing.try_recv() {
                Ok(result) => {
                    if let Err(error) = result {
                        log_error!("failed to record the open folders: {error}");
                    }
                    self.workspaces.last.writing = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => self.workspaces.last.writing = None,
            }
        }
        let settled = self.workspaces.exit.is_none()
            && self.workspaces.opening.is_none()
            && !self.workspace_set_switching()
            && !self.shutdown_anim_active;
        if settled {
            let key = (
                self.ordered_workspaces()
                    .into_iter()
                    .filter(|(_, app)| app.has_open_folder())
                    .map(|(id, app)| (id, app.explorer.root.clone()))
                    .collect(),
                self.workspaces.active,
            );
            if self.workspaces.last.key.as_ref() != Some(&key) {
                self.workspaces.last.key = Some(key);
                let mut snapshot = self.last_workspaces_snapshot();
                // A scope whose folders all closed is recorded as empty.
                for recorded in &self.workspaces.last.recorded {
                    if !snapshot.iter().any(|entry| entry.scope == recorded.scope) {
                        snapshot.push(Entry {
                            scope: recorded.scope.clone(),
                            roots: vec![],
                            active: 0,
                        });
                    }
                }
                for entry in snapshot {
                    let last = &self.workspaces.last;
                    let queued = last
                        .pending
                        .iter()
                        .any(|queued| queued.scope == entry.scope);
                    if queued || !last.recorded.contains(&entry) {
                        merge(&mut self.workspaces.last.pending, entry);
                    }
                }
            }
        }
        if self.workspaces.last.writing.is_some() || self.workspaces.last.pending.is_empty() {
            return;
        }
        let updates = std::mem::take(&mut self.workspaces.last.pending);
        for update in &updates {
            merge(&mut self.workspaces.last.recorded, update.clone());
        }
        let path = self.workspaces.last.path.clone();
        let (sender, receiver) = mpsc::channel();
        self.workspaces.last.writing = Some(receiver);
        let wake = self.background.wake_handle();
        self.background.spawn("record-open-folders", move || {
            let _ = sender.send(last_workspaces::record(&path, &updates));
            wake.notify();
        });
    }

    /// Write what the event loop has not yet written before the process exits.
    pub(crate) fn flush_last_workspaces(&mut self) {
        if !self.settings_persistence_enabled {
            return;
        }
        if let Some(writing) = self.workspaces.last.writing.take() {
            let _ = writing.recv_timeout(Duration::from_secs(2));
        }
        let updates = std::mem::take(&mut self.workspaces.last.pending);
        if !updates.is_empty()
            && let Err(error) = last_workspaces::record(&self.workspaces.last.path, &updates)
        {
            log_error!("failed to record the open folders: {error}");
        }
    }

    /// Reopen the folders recorded for this window's scope. Folders that no
    /// longer exist are skipped; the launch window stays when none remain.
    pub(crate) fn restore_last_workspaces(&mut self, entry: Entry) {
        let set = SavedSet {
            id: 0,
            name: "last session".into(),
            scope: entry.scope,
            roots: entry.roots,
            active: entry.active,
        };
        self.stage_workspace_set(set, None, true);
    }
}
