//! Native, debounced remote filesystem notifications. There is no idle polling
//! or stat-per-frame traffic. Bursts are bounded and coalesced at both ends.
use super::*;
use crate::workspace_watch::{Burst, NoiseFilter};
#[cfg(all(test, target_os = "linux"))]
#[path = "remote_watch_scope_tests.rs"]
mod scope_tests;
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
pub(super) struct Observation {
    stamp: Mutex<Stamp>,
    git: Mutex<Vec<PathBuf>>,
    wake: Mutex<Option<mpsc::SyncSender<()>>>,
}
impl Observation {
    pub fn stamp(&self) -> Stamp {
        *self.stamp.lock().unwrap()
    }
    pub fn active(&self, active: bool) {
        let mut stamp = self.stamp.lock().unwrap();
        stamp.active = active;
        stamp.epoch = stamp.epoch.wrapping_add(1);
    }
    pub fn invalidate(&self) {
        let mut stamp = self.stamp.lock().unwrap();
        stamp.epoch = stamp.epoch.wrapping_add(1);
    }
    // Repository discovery supplies explicit dependencies; arbitrary directory
    // symlinks are not recursive workspace subscriptions.
    pub(super) fn repositories(&self, root: &Path, discovery: &git::Discovery, parent_open: bool, closed: &[RemotePath]) {
        let mut paths = Vec::new();
        for path in discovery.roots.iter().chain(discovery.parent.iter().filter(|_| parent_open))
            .filter(|path| !closed.contains(path)) {
            let Ok(native) = path.resolve(root) else { continue };
            let Ok(repo) = git2::Repository::open(native) else { continue };
            for directory in [repo.path(), repo.commondir()] {
                if let Ok(directory) = std::fs::canonicalize(directory)
                    && !directory.starts_with(root) { paths.push(directory); }
            }
        }
        paths.sort();
        paths.dedup();
        let mut previous = self.git.lock().unwrap();
        if *previous != paths {
            *previous = paths;
            if let Some(wake) = &*self.wake.lock().unwrap() { let _ = wake.try_send(()); }
        }
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
        let _ = wake.try_send(());
        let callback_wake = wake.clone();
        let worker_observation = observation.clone();
        *observation.wake.lock().unwrap() = Some(wake.clone());
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
                        self.0.wake.lock().unwrap().take();
                    }
                }
                let _inactive = InactiveOnExit(worker_observation.clone());
                let mut noise = NoiseFilter::default();
                let callback_observation = worker_observation.clone();
                crate::worker_priority::lower_current_thread();
                let callback_root = root.clone();
                let callback_burst = burst.clone();
                let coverage_root = root.clone();
                let coverage_observation = worker_observation.clone();
                let coverage_stop = worker_stop.clone();
                let watcher = crate::workspace_watch::native_watcher(
                    move |event| {
                        let mut burst = callback_burst.lock().unwrap();
                        burst.push_with_dependencies(&callback_root, event, &callback_observation.git.lock().unwrap());
                        if !burst.event.is_empty() {
                            let _ = callback_wake.try_send(());
                        }
                    },
                    native_watch_config(),
                    move |path| !coverage_stop.load(Ordering::Acquire) && crate::workspace_watch::directory_is_relevant(
                        &coverage_root, path, &coverage_observation.git.lock().unwrap()),
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
                let mut git_watches = Vec::new();
                let mut git_dependencies = Vec::new();
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
                    if worker_stop.load(Ordering::Acquire) { break; }
                    // Only discovery changes alter subscriptions. This is not
                    // a recurring directory scan or a background Git query.
                    let changed_dependencies = {
                        let paths = worker_observation.git.lock().unwrap();
                        (*paths != git_dependencies).then(|| paths.clone())
                    };
                    let mut failed = false;
                    if let Some(paths) = changed_dependencies {
                        let mut dependencies: Vec<PathBuf> = Vec::new();
                        // Discovery supplies sorted paths. A retained parent
                        // covers adjacent descendants in that sorted inventory.
                        for path in &paths {
                            if !dependencies.last().is_some_and(|parent| path.starts_with(parent)) {
                                dependencies.push(path.clone());
                            }
                        }
                        // A newly explicit Git root may be below a pruned
                        // objects/logs directory of an unchanged parent watch.
                        let filter_changed = cfg!(target_os = "linux") && paths.iter().chain(&git_dependencies)
                            .filter(|path| paths.binary_search(path).is_err()
                                || git_dependencies.binary_search(path).is_err())
                            .any(|path| !crate::workspace_watch::directory_is_relevant(&root, path, &dependencies));
                        git_dependencies = paths;
                        if git_watches != dependencies || filter_changed {
                            // Remove first: unwatching a parent also removes its
                            // children, including a newly narrowed subscription.
                            let mut rearm_workspace = false;
                            for path in &git_watches {
                                if filter_changed || dependencies.binary_search(path).is_err() {
                                    rearm_workspace |= root.starts_with(path);
                                    let _ = watcher.unwatch(path);
                                }
                            }
                            if rearm_workspace && watcher.watch(&root, RecursiveMode::Recursive).is_err() {
                                failed = true;
                            }
                            for path in &dependencies {
                                if (filter_changed || git_watches.binary_search(path).is_err())
                                    && watcher.watch(path, RecursiveMode::Recursive).is_err() {
                                    failed = true;
                                }
                            }
                            let mut pending = burst.lock().unwrap();
                            pending.event.git_metadata_changed = true;
                            pending.event.rescan |= rearm_workspace;
                            git_watches = dependencies;
                        }
                    }
                    if failed {
                        worker_observation.active(false);
                        let mut pending = burst.lock().unwrap();
                        pending.event.rescan = true;
                        pending.event.watcher_failed = true;
                    }
                    let pending = std::mem::take(&mut *burst.lock().unwrap());
                    let event = noise.filter(pending);
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
                    if event.watcher_failed { break; }
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

#[cfg(all(test, target_os = "linux"))]
fn native_watcher(event_handler: impl notify::EventHandler) -> notify::Result<notify::RecommendedWatcher> {
    crate::workspace_watch::native_watcher(event_handler, native_watch_config(), |_| true)
}

fn native_watch_config() -> notify::Config {
    // VTCode consumes modifications, including close-after-write, but ignores
    // read-only opens. Filter those in the kernel, before allocating events or
    // waking a thread for other tools' reads of the workspace and its caches.
    notify::Config::default().with_open_events(false).with_follow_symlinks(false)
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
        let mut watcher = native_watcher(events).unwrap();
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

    #[cfg(target_os = "linux")]
    #[test]
    fn native_headless_batches_hot_files_without_dropping_paths_renames_or_replacements() {
        use notify::{RecursiveMode, Watcher};
        use std::time::{Duration, Instant};
        let workspace = tempfile::tempdir().unwrap();
        let root = workspace.path();
        let files: Vec<_> = ["build/source.rs", "target/generated.rs", "node_modules/cache", "dist/source.js",
            "__pycache__/source.py", ".venv/source.py", "venv/source.py", ".tox/source.py",
            ".mypy_cache/source.py", ".pytest_cache/source.py"].into_iter().map(|path| root.join(path)).collect();
        for path in &files {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "before").unwrap();
        }
        let (events, received) = mpsc::channel();
        let mut watcher = native_watcher(events).unwrap();
        watcher.watch(root, RecursiveMode::Recursive).unwrap();
        for path in &files { std::fs::write(path, "after").unwrap(); }
        let hot = &files[2];
        for _ in 0..1000 { std::fs::write(hot, "hot file").unwrap(); }
        let mut missing: std::collections::HashSet<_> = files.iter().cloned().collect();
        let deadline = Instant::now() + Duration::from_secs(3);
        while !missing.is_empty() {
            let event = received.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap().unwrap();
            for path in event.paths { missing.remove(&path); }
        }
        let drain = || loop {
            match received.recv_timeout(Duration::from_millis(50)) {
                Ok(event) => { event.unwrap(); }
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(error) => panic!("{error}"),
            }
        };
        drain();

        // Coalescing must reset at structural barriers, including replacement
        // of a path that already emitted a modification in the same batch.
        let renamed = root.join("build/renamed");
        std::fs::write(hot, "before rename").unwrap();
        std::fs::rename(hot, &renamed).unwrap();
        std::fs::write(hot, "replacement").unwrap();
        let mut modifications = 0;
        let mut saw_rename = false;
        let deadline = Instant::now() + Duration::from_secs(3);
        while modifications < 2 || !saw_rename {
            let event = received.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap().unwrap();
            if matches!(event.kind, EventKind::Modify(ModifyKind::Data(_))) && event.paths.contains(hot) {
                modifications += 1;
            }
            saw_rename |= matches!(event.kind, EventKind::Modify(ModifyKind::Name(_))) && event.paths.contains(&renamed);
        }
        drain();
        std::fs::remove_file(&renamed).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let event = received.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap().unwrap();
            if matches!(event.kind, EventKind::Remove(_)) && event.paths.contains(&renamed) { break; }
        }
        drain();

        // Newly created subdirectories retain recursive coverage as well.
        let nested = root.join("build/new-subdirectory");
        std::fs::create_dir(&nested).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let event = received.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap().unwrap();
            if event.paths.contains(&nested) { break; }
        }
        drain();
        let child = nested.join("new.rs");
        std::fs::write(&child, "new source").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let event = received.recv_timeout(deadline.saturating_duration_since(Instant::now())).unwrap().unwrap();
            if event.paths.contains(&child) { break; }
        }
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
