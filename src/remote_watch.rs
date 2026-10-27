//! Native, debounced remote filesystem notifications. There is no idle polling
//! or stat-per-frame traffic. Bursts are bounded and coalesced at both ends.
use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Stamp {
    pub active: bool,
    epoch: u64,
}
#[derive(Default)]
pub(super) struct Observation(Mutex<Stamp>);
impl Observation {
    pub fn stamp(&self) -> Stamp {
        *self.0.lock().unwrap()
    }
    pub fn active(&self, active: bool) {
        let mut stamp = self.0.lock().unwrap();
        stamp.active = active;
        stamp.epoch = stamp.epoch.wrapping_add(1);
    }
    pub fn invalidate(&self) {
        let mut stamp = self.0.lock().unwrap();
        stamp.epoch = stamp.epoch.wrapping_add(1);
    }
}

#[derive(Clone, Default)]
pub(super) struct Registry(
    Arc<Mutex<std::collections::HashMap<PathBuf, std::sync::Weak<Observation>>>>,
);
impl Registry {
    pub fn get(&self, root: &Path) -> Arc<Observation> {
        let mut entries = self.0.lock().unwrap();
        if let Some(observation) = entries.get(root).and_then(std::sync::Weak::upgrade) {
            return observation;
        }
        entries.retain(|_, entry| entry.strong_count() != 0);
        let observation = Arc::new(Observation::default());
        entries.insert(root.to_owned(), Arc::downgrade(&observation));
        observation
    }
}

/// Repeated notifications for one hot file are one change, not an overflow
/// requiring a workspace rescan. Once overflowed, retain flags, not stat work.
#[derive(Default)]
struct Burst {
    event: crate::background::WorkspaceEventBurst,
    paths: std::collections::HashSet<PathBuf>,
}
impl Burst {
    fn push(&mut self, root: &Path, event: notify::Result<notify::Event>) {
        let mut index = self.event.paths.len();
        self.event.push(root, event);
        if self.event.rescan {
            self.event.paths.clear();
            self.paths.clear();
            return;
        }
        while index < self.event.paths.len() {
            let path = &self.event.paths[index];
            if self.paths.contains(path) {
                self.event.paths.swap_remove(index);
            } else if self.paths.len() == 1024 {
                self.event.rescan = true;
                self.event.paths.clear();
                self.paths.clear();
                break;
            } else {
                self.paths.insert(path.clone());
                index += 1;
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct State {
    pub path: RemotePath,
    pub exists: bool,
    pub modified: Option<std::time::SystemTime>,
    pub size: Option<u64>,
}
impl State {
    pub(crate) fn read(root: &Path, path: RemotePath) -> io::Result<Self> {
        let native = path.resolve(root)?;
        let state = crate::background::inspect_workspace_path(native);
        Ok(Self {
            path,
            exists: state.exists,
            modified: state.modified,
            size: state.len,
        })
    }
    fn local(self, workspace: &workspace::Workspace) -> crate::background::WorkspacePathChange {
        crate::background::WorkspacePathChange {
            observed_at: std::time::Instant::now(),
            path: workspace.view_key(&self.path),
            exists: self.exists,
            modified: self.modified,
            len: self.size,
        }
    }
}

pub(super) fn inspect(
    root: &Path,
    paths: Vec<RemotePath>,
    cancel: &crate::background::CancellationToken,
) -> io::Result<Reply> {
    if paths.len() > 1024 {
        return Err(io::Error::other("File inspection batch exceeds limit"));
    }
    let mut states = Vec::with_capacity(paths.len());
    for path in paths {
        if cancel.is_cancelled() {
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        states.push(State::read(root, path)?);
    }
    if cancel.is_cancelled() {
        return Err(io::Error::from(io::ErrorKind::Interrupted));
    }
    Ok(Reply::FileStates(states))
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub(crate) struct Batch {
    pub root_uri: String,
    pub states: Vec<State>,
    pub structural: bool,
    pub git: bool,
    pub rescan: bool,
    pub active: Option<bool>,
}
impl Batch {
    pub(crate) fn merge(&mut self, other: Self) {
        self.root_uri = other.root_uri.clone();
        self.structural |= other.structural;
        self.git |= other.git;
        self.rescan |= other.rescan;
        self.active = other.active.or(self.active);
        if self.rescan {
            self.states.clear();
            return;
        }
        let mut states: std::collections::HashMap<_, _> = self
            .states
            .drain(..)
            .map(|state| (state.path.clone(), state))
            .collect();
        for state in other.states {
            states.insert(state.path.clone(), state);
        }
        if states.len() > 1024 {
            self.rescan = true;
        } else {
            self.states = states.into_values().collect();
        }
    }
}

pub(crate) struct Watch {
    stop: Arc<AtomicBool>,
    wake: mpsc::SyncSender<()>,
    observation: Arc<Observation>,
}
impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.observation.active(false);
        let _ = self.wake.try_send(());
    }
}
impl Watch {
    pub(super) fn start(
        root: PathBuf,
        output: mpsc::SyncSender<Response>,
        observation: Arc<Observation>,
    ) -> io::Result<Self> {
        use notify::{RecursiveMode, Watcher};
        let burst = Arc::new(Mutex::new(Burst::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (wake, requests) = mpsc::sync_channel(1);
        let worker_stop = stop.clone();
        let callback_wake = wake.clone();
        let worker_observation = observation.clone();
        let root_uri: String = url::Url::from_directory_path(&root)
            .map_err(|_| io::Error::other("Invalid watcher root"))?
            .into();
        std::thread::Builder::new()
            .name("headless-watch".into())
            .spawn(move || {
                struct InactiveOnExit(Arc<Observation>);
                impl Drop for InactiveOnExit {
                    fn drop(&mut self) {
                        self.0.active(false);
                    }
                }
                let _inactive = InactiveOnExit(worker_observation.clone());
                crate::worker_priority::lower_current_thread();
                let callback_root = root.clone();
                let callback_burst = burst.clone();
                let watcher = notify::RecommendedWatcher::new(
                    move |event| {
                        let mut burst = callback_burst.lock().unwrap();
                        burst.push(&callback_root, event);
                        if !burst.event.is_empty() {
                            let _ = callback_wake.try_send(());
                        }
                    },
                    native_watch_config(),
                );
                let mut watcher = match watcher.and_then(|mut watcher| {
                    watcher.watch(&root, RecursiveMode::Recursive)?;
                    Ok(watcher)
                }) {
                    Ok(watcher) => watcher,
                    Err(_) => {
                        worker_observation.active(false);
                        let _ = output.send(Response {
                            id: 0,
                            result: Ok(Reply::WorkspaceChanges(Batch {
                                root_uri,
                                active: Some(false),
                                ..Default::default()
                            })),
                        });
                        return;
                    }
                };
                worker_observation.active(true);
                if output
                    .send(Response {
                        id: 0,
                        result: Ok(Reply::WorkspaceChanges(Batch {
                            root_uri: root_uri.clone(),
                            active: Some(true),
                            ..Default::default()
                        })),
                    })
                    .is_err()
                {
                    return;
                }
                while requests.recv().is_ok() && !worker_stop.load(Ordering::Acquire) {
                    let deadline =
                        std::time::Instant::now() + std::time::Duration::from_millis(300);
                    while !worker_stop.load(Ordering::Acquire)
                        && requests
                            .recv_timeout(
                                deadline.saturating_duration_since(std::time::Instant::now()),
                            )
                            .is_ok()
                    {}
                    if worker_stop.load(Ordering::Acquire) {
                        break;
                    }
                    let event = std::mem::take(&mut *burst.lock().unwrap()).event;
                    if event.is_empty() {
                        continue;
                    }
                    if event.structure_changed || event.rescan {
                        worker_observation.invalidate();
                    }
                    if event.watcher_failed {
                        worker_observation.active(false);
                    }
                    let states =
                        crate::background::coalesce_workspace_event_paths(&root, event.paths)
                            .into_iter()
                            .filter_map(|path| git::from_native(&path, &root).ok())
                            .filter_map(|path| State::read(&root, path).ok())
                            .collect();
                    let batch = Batch {
                        root_uri: root_uri.clone(),
                        states,
                        structural: event.structure_changed,
                        git: event.git_metadata_changed,
                        rescan: event.rescan,
                        active: event.watcher_failed.then_some(false),
                    };
                    if output
                        .send(Response {
                            id: 0,
                            result: Ok(Reply::WorkspaceChanges(batch)),
                        })
                        .is_err()
                    {
                        break;
                    }
                }
                let _ = watcher.unwatch(&root);
            })?;
        Ok(Self {
            stop,
            wake,
            observation,
        })
    }
}

fn native_watch_config() -> notify::Config {
    // VTCode consumes modifications, including close-after-write, but ignores
    // read-only opens. Filter those in the kernel, before allocating events or
    // waking a thread for other tools' reads of the workspace and its caches.
    notify::Config::default().with_open_events(false)
}

impl workspace::Workspace {
    pub(crate) fn save_is_watched(&self, key: &Path, checkpoint: Option<&Checkpoint>) -> bool {
        let Ok(path) = self.remote_path(key) else {
            return false;
        };
        // A symlink can publish outside the watched tree. Its canonical
        // checkpoint identity, rather than the tab's spelling, decides coverage.
        !path.1
            && checkpoint.and_then(|checkpoint| checkpoint.identity.as_ref()) == Some(&path)
            && crate::background::workspace_path_is_watched(&self.view_root, key)
    }

    pub(crate) fn apply_notifications(&self, sender: &crate::background::BackgroundResultSender, generation: u64) {
        let Some(batch) = self.client.take_workspace_changes() else {
            return;
        };
        let send = |result| {
            let _ = sender.send(crate::background::BackgroundResult::WorkspaceResult {
                generation, result: Box::new(result),
            });
        };
        if let Some(active) = batch.active {
            send(crate::background::BackgroundResult::FileWatcherState {
                root: self.view_root.clone(),
                active,
            });
        }
        if batch.structural || batch.rescan || batch.git {
            self.invalidate_directories();
        }
        if batch.states.is_empty() && !batch.git && !batch.rescan {
            return;
        }
        send(crate::background::BackgroundResult::FilesChanged {
            root: self.view_root.clone(),
            changes: batch
                .states
                .into_iter()
                .map(|state| state.local(self))
                .collect(),
            structure_changed: batch.structural,
            rescan: batch.rescan,
            git_metadata_changed: batch.git,
        });
    }
    pub(crate) fn inspect(
        &self,
        keys: &[PathBuf],
        cancel: &crate::background::CancellationToken,
    ) -> io::Result<Vec<crate::background::WorkspacePathChange>> {
        // Receipt time is too new for save/checkpoint barriers: this reply may
        // have waited behind a slow disk or network while a save completed.
        let observed_at = std::time::Instant::now();
        let paths = keys
            .iter()
            .map(|key| self.remote_path(key))
            .collect::<io::Result<_>>()?;
        let Reply::FileStates(states) = self
            .client
            .call_cancellable(Command::Inspect { paths }, cancel)?
        else {
            return Err(io::Error::other("Invalid remote file-state response"));
        };
        Ok(states
            .into_iter()
            .map(|state| {
                let mut change = state.local(self);
                change.observed_at = observed_at;
                change
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::{
        Event, EventKind,
        event::{DataChange, ModifyKind},
    };

    #[cfg(target_os = "linux")]
    #[test]
    fn native_headless_watch_stays_asleep_for_reads_but_reports_writes_and_renames() {
        use notify::{RecursiveMode, Watcher, event::{AccessKind, AccessMode}};
        use std::time::{Duration, Instant};
        let root = tempfile::tempdir().unwrap();
        let nested = root.path().join("node_modules/cache");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("entry.txt");
        std::fs::write(&file, "before").unwrap();
        let (events, received) = mpsc::channel();
        let mut watcher = notify::RecommendedWatcher::new(events, native_watch_config()).unwrap();
        watcher.watch(root.path(), RecursiveMode::Recursive).unwrap();
        for _ in 0..1000 { assert_eq!(std::fs::read(&file).unwrap(), b"before"); }
        assert!(matches!(received.recv_timeout(Duration::from_millis(50)), Err(mpsc::RecvTimeoutError::Timeout)),
            "read-only opens must not wake the native event handler");
        std::fs::write(&file, "after").unwrap();
        let changed = received.recv_timeout(Duration::from_secs(3)).unwrap().unwrap();
        assert!(changed.paths.contains(&file));
        assert!(matches!(changed.kind, EventKind::Modify(_) | EventKind::Access(AccessKind::Close(AccessMode::Write))));
        let renamed = nested.join("renamed.txt");
        std::fs::rename(&file, &renamed).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let event = received.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap().unwrap();
            if matches!(event.kind, EventKind::Modify(ModifyKind::Name(_))) && event.paths.contains(&renamed) { break; }
        }
        std::fs::remove_file(&renamed).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let event = received.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap().unwrap();
            if matches!(event.kind, EventKind::Remove(_)) && event.paths.contains(&renamed) { break; }
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn native_watcher_default_still_supports_open_notifications() {
        use notify::{RecursiveMode, Watcher, event::AccessKind};
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("entry.txt");
        std::fs::write(&file, "content").unwrap();
        let (events, received) = mpsc::channel();
        let mut watcher = notify::RecommendedWatcher::new(events, notify::Config::default()).unwrap();
        watcher.watch(&file, RecursiveMode::NonRecursive).unwrap();
        std::fs::read(&file).unwrap();
        let event = received.recv_timeout(std::time::Duration::from_secs(3)).unwrap().unwrap();
        assert!(matches!(event.kind, EventKind::Access(AccessKind::Open(_))));
    }

    #[test]
    fn metadata_batches_are_bounded_and_cancel_before_resolving_paths() {
        let root = tempfile::tempdir().unwrap();
        let cancel = crate::background::CancellationToken::new();
        assert!(inspect(root.path(), vec![RemotePath::default(); 1025], &cancel).is_err());
        let Reply::FileStates(states) =
            inspect(root.path(), vec![RemotePath::default()], &cancel).unwrap()
        else {
            panic!()
        };
        assert_eq!(states.len(), 1);
        assert!(states[0].exists);
        cancel.cancel();
        assert_eq!(
            inspect(root.path(), vec![RemotePath::default()], &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(
            inspect(root.path(), Vec::new(), &cancel)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn hot_file_notifications_are_bounded_by_unique_paths() {
        let root = Path::new("workspace");
        let file = root.join("hot.rs");
        let mut burst = Burst::default();
        for _ in 0..10_000 {
            burst.push(
                root,
                Ok(
                    Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any)))
                        .add_path(file.clone()),
                ),
            );
        }
        assert_eq!(burst.event.paths, [file]);
        assert!(!burst.event.rescan);
        assert_eq!(burst.paths.len(), 1);
        for index in 0..1024 {
            burst.push(
                root,
                Ok(
                    Event::new(EventKind::Create(notify::event::CreateKind::File))
                        .add_path(root.join(format!("file-{index}"))),
                ),
            );
        }
        assert!(burst.event.rescan && burst.event.structure_changed);
        assert!(burst.event.paths.is_empty() && burst.paths.is_empty());
        burst.push(
            root,
            Ok(
                Event::new(EventKind::Modify(ModifyKind::Data(DataChange::Any)))
                    .add_path(root.join(".git/index")),
            ),
        );
        burst.push(root, Err(notify::Error::generic("watch failed")));
        assert!(burst.event.git_metadata_changed && burst.event.watcher_failed);
        assert!(burst.event.paths.is_empty());
    }

    #[test]
    fn temporary_save_notifications_do_not_invalidate_the_tree_but_the_published_file_does() {
        let root = Path::new("workspace");
        let target = root.join("file.rs");
        let temporary = crate::editor::save_temp_path(&target);
        let mut burst = Burst::default();
        for kind in [
            EventKind::Create(notify::event::CreateKind::File),
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            EventKind::Remove(notify::event::RemoveKind::File),
        ] {
            burst.push(root, Ok(Event::new(kind).add_path(temporary.clone())));
        }
        assert!(burst.event.is_empty());
        burst.push(
            root,
            Ok(Event::new(EventKind::Modify(ModifyKind::Name(
                notify::event::RenameMode::Both,
            )))
            .add_path(temporary)
            .add_path(target.clone())),
        );
        assert_eq!(burst.event.paths, [target]);
        assert!(burst.event.structure_changed);
        for real in [
            "file.vtcode-tmp-not-a-save",
            "file.vtcode-tmp-12-",
            "file.vtcode-tmp--3",
            "file.vtcode-tmp-1-2.rs",
        ] {
            assert!(
                !crate::editor::is_save_temporary_path(&root.join(real)),
                "{real}"
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            let raw = root.join(std::ffi::OsStr::from_bytes(b"file-\xff.rs"));
            assert!(crate::editor::is_save_temporary_path(
                &crate::editor::save_temp_path(&raw)
            ));
        }
    }
}
