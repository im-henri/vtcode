//! Remember the open folders of each scope so a launch without a path can
//! reopen them. Only settled states are recorded: never while a folder opens,
//! a saved workspace switches, or folders close for quitting.
use super::*;
use crate::last_workspaces::{self, Entry};
use crate::workspace_sets::{Location, SavedSet, Scope};
use rustc_hash::FxHashMap;

struct Write {
    updates: Vec<Entry>,
    finished: mpsc::Receiver<io::Result<()>>,
}

type Layout = (Vec<(u64, PathBuf)>, u64);

pub(crate) struct State {
    pub(crate) path: PathBuf,
    /// The last selected folder in each connection, including parked ones.
    pub(crate) preferred: FxHashMap<Scope, u64>,
    /// The open folders by workspace ID, in order, and the active ID.
    key: Option<Layout>,
    /// The most recent snapshot of each scope this window has opened.
    observed: Vec<Entry>,
    /// The newest entries not yet written, at most one per scope.
    pending: Vec<Entry>,
    writing: Option<Write>,
    retry_on_change: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            path: last_workspaces::path(),
            preferred: FxHashMap::default(),
            key: None,
            observed: vec![],
            pending: vec![],
            writing: None,
            retry_on_change: false,
        }
    }
}

impl State {
    pub(super) fn restored(&mut self) {
        self.key = None;
    }

    fn finish_write(&mut self, write: Write, result: io::Result<()>) {
        if let Err(error) = result {
            log_error!("failed to record the open folders: {error}");
            // Retry when a newer snapshot is waiting, or at the next change
            // or shutdown. A store that stays unwritable should not spin.
            self.retry_on_change = self.pending.is_empty();
            for update in write.updates {
                if !self.pending.iter().any(|newer| newer.scope == update.scope) {
                    self.pending.push(update);
                }
            }
        }
    }
}

fn merge(entries: &mut Vec<Entry>, update: Entry) {
    entries.retain(|entry| entry.scope != update.scope);
    entries.push(update);
}

impl App {
    fn last_workspaces_layout(&self) -> Layout {
        (
            self.ordered_workspaces()
                .into_iter()
                .filter(|(_, app)| app.has_open_folder())
                .map(|(id, app)| (id, app.explorer.root.clone()))
                .collect(),
            self.workspaces.active,
        )
    }

    fn last_workspaces_snapshot(&self, preferred: &mut FxHashMap<Scope, u64>) -> Vec<Entry> {
        let base = crate::portable::base_dir();
        let mut entries: Vec<Entry> = vec![];
        for (id, app) in self.ordered_workspaces() {
            if !app.has_open_folder() {
                continue;
            }
            let scope = app.workspace_sets_scope();
            let active = id == *preferred.entry(scope.clone()).or_insert(id);
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
            if active {
                entry.active = entry.roots.len();
            }
            entry.roots.push(location);
        }
        entries
    }

    fn queue_last_workspaces(&mut self) -> bool {
        let settled = self.workspaces.exit.is_none()
            && self.workspaces.opening.is_none()
            && !self.workspace_set_switching()
            && !self.shutdown_anim_active;
        if !settled {
            return false;
        }
        let key = self.last_workspaces_layout();
        if self.workspaces.last.key.as_ref() == Some(&key) {
            return false;
        }
        let mut preferred = std::mem::take(&mut self.workspaces.last.preferred);
        preferred.retain(|_, id| key.0.iter().any(|(open, _)| open == id));
        if self.has_open_folder() {
            preferred.insert(self.workspace_sets_scope(), self.workspaces.active);
        }
        let mut snapshot = self.last_workspaces_snapshot(&mut preferred);
        let last = &mut self.workspaces.last;
        last.preferred = preferred;
        last.key = Some(key);
        // A scope whose folders all closed is recorded as empty.
        for observed in &last.observed {
            if !observed.roots.is_empty()
                && !snapshot.iter().any(|entry| entry.scope == observed.scope)
            {
                snapshot.push(Entry {
                    scope: observed.scope.clone(),
                    roots: vec![],
                    active: 0,
                });
            }
        }
        for entry in snapshot {
            if !last.observed.contains(&entry) {
                merge(&mut last.observed, entry.clone());
                merge(&mut last.pending, entry);
            }
        }
        true
    }

    pub(crate) fn poll_last_workspaces(&mut self) {
        if !self.settings_persistence_enabled {
            return;
        }
        let last = &mut self.workspaces.last;
        if let Some(writing) = &last.writing {
            let result = match writing.finished.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err(io::Error::other("Open folders writer stopped")))
                }
            };
            if let Some(result) = result {
                let write = last.writing.take().unwrap();
                last.finish_write(write, result);
            }
        }
        let changed = self.queue_last_workspaces();
        let last = &mut self.workspaces.last;
        if last.writing.is_some() || last.pending.is_empty() || (last.retry_on_change && !changed) {
            return;
        }
        let updates = std::mem::take(&mut last.pending);
        let path = last.path.clone();
        let (sender, finished) = mpsc::channel();
        last.writing = Some(Write {
            updates: updates.clone(),
            finished,
        });
        last.retry_on_change = false;
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
        self.queue_last_workspaces();
        let last = &mut self.workspaces.last;
        if let Some(write) = last.writing.take() {
            let result = match write
                .finished
                .recv_timeout(last_workspaces::LOCK_WAIT + Duration::from_secs(1))
            {
                Ok(result) => result,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    Err(io::Error::other("Open folders writer stopped"))
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    log_error!("timed out recording the open folders");
                    return;
                }
            };
            last.finish_write(write, result);
        }
        if !last.pending.is_empty()
            && let Err(error) = last_workspaces::record(&last.path, &last.pending)
        {
            log_error!("failed to record the open folders: {error}");
        } else {
            last.pending.clear();
        }
    }

    /// Reopen the folders recorded for this window's scope. Folders that no
    /// longer exist are skipped; the launch window stays when none remain.
    pub(crate) fn restore_last_workspaces(&mut self, entry: Entry) {
        // Keep the existing record if restoration fails or the user cancels
        // while the launch folder is still the only one open.
        self.workspaces.last.key = Some(self.last_workspaces_layout());
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_last_workspace_write_is_retried_at_shutdown() {
        let temp = tempfile::tempdir().unwrap();
        let blocked = temp.path().join("blocked");
        std::fs::write(&blocked, "").unwrap();
        let mut app = App::new_startup(temp.path().into(), Default::default());
        app.settings_persistence_enabled = true;
        app.workspaces.last.path = blocked.join("last.json");
        app.poll_last_workspaces();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.workspaces.last.writing.is_some() {
            assert!(Instant::now() < deadline);
            app.poll_last_workspaces();
            std::thread::sleep(Duration::from_millis(1));
        }
        std::fs::remove_file(&blocked).unwrap();
        app.flush_last_workspaces();
        assert_eq!(
            last_workspaces::load(&app.workspaces.last.path, &Scope::Local)
                .unwrap()
                .roots,
            [Location::local(temp.path(), &crate::portable::base_dir())],
        );
    }
}
