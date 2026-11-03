use super::*;
use crate::workspace_sets::{self, Catalog, Location, Mutation, SavedSet, Scope};

type CatalogReply = mpsc::Receiver<Result<Catalog, String>>;
pub(super) struct StageReply {
    receiver: mpsc::Receiver<Result<Staged, String>>,
    cancel: crate::background::CancellationToken,
}
impl Drop for StageReply {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

pub(crate) struct State {
    pub catalog: Catalog,
    pub loaded: bool,
    pub error: Option<String>,
    pub path: PathBuf,
    pub(super) preserving: bool,
    read: Option<CatalogReply>,
    write: Option<CatalogReply>,
    pub(super) plan: Option<Plan>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            catalog: Catalog::default(),
            loaded: false,
            error: None,
            path: crate::portable::data_dir().join("workspace-sets.json"),
            preserving: false,
            read: None,
            write: None,
            plan: None,
        }
    }
}

pub(super) enum Plan {
    Loading(StageReply),
    Ready(Staged),
    Closing(Transition),
}
pub(super) struct Staged {
    set: SavedSet,
    roots: Vec<StagedRoot>,
    active: usize,
    expected: Vec<u64>,
    confirmed: bool,
}
struct StagedRoot {
    root: PathBuf,
    canonical: Option<PathBuf>,
    existing: Option<u64>,
    prepared: Option<Prepared>,
}
pub(super) struct Transition {
    name: String,
    target: Vec<u64>,
    active: u64,
    close: Vec<u64>,
}

fn reply<T>(receiver: &mpsc::Receiver<Result<T, String>>) -> Option<Result<T, String>> {
    match receiver.try_recv() {
        Ok(result) => Some(result),
        Err(mpsc::TryRecvError::Empty) => None,
        Err(mpsc::TryRecvError::Disconnected) => Some(Err("Workspace worker stopped".into())),
    }
}

impl App {
    pub(crate) fn workspace_sets_scope(&self) -> Scope {
        self.remote_workspace
            .as_ref()
            .map_or(Scope::Local, |remote| {
                Scope::Remote(remote.workspace_sets_scope())
            })
    }
    pub(crate) fn saved_workspace_sets(&self) -> Vec<SavedSet> {
        let scope = self.workspace_sets_scope();
        let mut sets: Vec<_> = self
            .workspaces
            .sets
            .catalog
            .sets
            .iter()
            .filter(|set| set.scope == scope)
            .cloned()
            .collect();
        sets.sort_by_key(|set| (set.name.to_lowercase(), set.id));
        sets
    }
    pub(crate) fn workspace_sets_busy(&self) -> bool {
        self.workspaces.sets.read.is_some() || self.workspaces.sets.write.is_some()
    }
    pub(crate) fn workspace_sets_saving(&self) -> bool {
        self.workspaces.sets.write.is_some()
    }
    pub(crate) fn workspace_set_loading(&self) -> bool {
        matches!(self.workspaces.sets.plan, Some(Plan::Loading(_)))
    }
    pub(crate) fn workspace_set_switching(&self) -> bool {
        self.workspaces.sets.plan.is_some()
    }
    pub(crate) fn workspace_set_preserving(&self) -> bool {
        self.workspaces.sets.preserving
    }
    pub(crate) fn load_workspace_sets(&mut self, refresh: bool) {
        if self.workspace_sets_busy() || (self.workspaces.sets.loaded && !refresh) {
            return;
        }
        let path = self.workspaces.sets.path.clone();
        let (sender, receiver) = mpsc::channel();
        self.workspaces.sets.read = Some(receiver);
        self.workspaces.sets.error = None;
        let wake = self.background.wake_handle();
        self.background.spawn("workspace-set-catalog", move || {
            let _ = sender.send(workspace_sets::load(&path).map_err(|error| error.to_string()));
            wake.notify();
        });
    }
    fn write_workspace_sets(&mut self, mutation: Mutation) {
        if self.workspace_set_switching() {
            self.workspaces.sets.error =
                Some("Finish or cancel opening the workspace first".into());
            return;
        }
        if self.workspace_sets_busy() {
            self.workspaces.sets.error =
                Some("Wait for saved workspaces to finish saving or loading".into());
            return;
        }
        let path = self.workspaces.sets.path.clone();
        let (sender, receiver) = mpsc::channel();
        self.workspaces.sets.write = Some(receiver);
        self.workspaces.sets.error = None;
        let wake = self.background.wake_handle();
        self.background.spawn("save-workspace-sets", move || {
            let _ = sender
                .send(workspace_sets::mutate(&path, mutation).map_err(|error| error.to_string()));
            wake.notify();
        });
    }
    pub(crate) fn save_workspace_set(&mut self, name: String, replace: Option<u64>) {
        let result = (|| -> Result<Mutation, String> {
            workspace_sets::valid_name(&name).map_err(|error| error.to_string())?;
            if self.workspace_opening()
                || self.workspace_set_switching()
                || self.workspaces.exit.is_some()
            {
                return Err("Finish opening or closing folders before saving a workspace".into());
            }
            let base = crate::portable::base_dir();
            let scope = self.workspace_sets_scope();
            let mut roots: Vec<_> = std::iter::once((self.workspaces.active, &*self))
                .chain(
                    self.workspaces
                        .parked
                        .iter()
                        .map(|parked| (parked.id, &*parked.app)),
                )
                .filter(|(_, app)| app.has_open_folder())
                .collect();
            if roots.is_empty() { return Err("Open a folder before saving a workspace".into()); }
            roots.sort_by_key(|(id, _)| *id);
            let active = roots
                .iter()
                .position(|(id, _)| *id == self.workspaces.active)
                .unwrap_or(0);
            let roots = roots
                .into_iter()
                .map(|(_, app)| {
                    if app.workspace_sets_scope() != scope {
                        return Err("A workspace cannot mix different connections".into());
                    }
                    Ok(if let Some(remote) = &app.remote_workspace {
                        Location::Remote {
                            uri: remote.client.hello().root_uri.clone(),
                        }
                    } else {
                        Location::local(&app.explorer.root, &base)
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(Mutation::Save {
                name,
                scope,
                roots,
                active,
                replace,
            })
        })();
        match result {
            Ok(mutation) => self.write_workspace_sets(mutation),
            Err(error) => self.workspaces.sets.error = Some(error),
        }
    }
    pub(crate) fn rename_workspace_set(&mut self, id: u64, name: String) {
        self.write_workspace_sets(Mutation::Rename {
            id,
            scope: self.workspace_sets_scope(),
            name,
        });
    }
    pub(crate) fn delete_workspace_set(&mut self, id: u64) {
        self.write_workspace_sets(Mutation::Delete {
            id,
            scope: self.workspace_sets_scope(),
        });
    }
    pub(crate) fn cancel_workspace_set_switch(&mut self) {
        self.workspaces.sets.plan = None;
        self.workspaces.sets.preserving = false;
    }
    pub(crate) fn workspace_set_confirmation(&self) -> Option<(String, usize, Vec<Entry>)> {
        let Some(Plan::Ready(staged)) = &self.workspaces.sets.plan else {
            return None;
        };
        let keep: Vec<_> = staged
            .roots
            .iter()
            .filter_map(|root| root.existing)
            .collect();
        let close = self
            .workspace_entries()
            .into_iter()
            .filter(|entry| !entry.path.is_empty() && !keep.contains(&entry.id))
            .collect();
        Some((staged.set.name.clone(), staged.roots.len(), close))
    }
    pub(crate) fn confirm_workspace_set(&mut self) {
        let Some(Plan::Ready(staged)) = self.workspaces.sets.plan.take() else {
            return;
        };
        let (sender, receiver) = mpsc::channel();
        self.workspaces.sets.preserving = true;
        let cancel = crate::background::CancellationToken::new();
        self.workspaces.sets.plan = Some(Plan::Loading(StageReply {
            receiver,
            cancel: cancel.clone(),
        }));
        self.workspaces.popup = Default::default();
        let wake = self.background.wake_handle();
        self.background.spawn("validate-workspace-set", move || {
            let result = (|| -> Result<Staged, String> {
                for root in &staged.roots {
                    if cancel.is_cancelled() {
                        return Err("Opening the workspace was cancelled".into());
                    }
                    if root.existing.is_some() {
                        continue;
                    }
                    if let Some(remote) = root
                        .prepared
                        .as_ref()
                        .and_then(|prepared| prepared.remote.as_ref())
                    {
                        let checked = remote
                            .open_workspace(&root.root)
                            .map_err(|error| error.to_string())?;
                        if checked.view_root != root.root || !checked.client.connected() {
                            return Err("A remote folder changed while confirming the workspace".into());
                        }
                    } else if std::fs::canonicalize(&root.root).ok().as_ref()
                        != root.canonical.as_ref()
                        || !root.root.is_dir()
                    {
                        return Err(format!(
                            "{} is no longer the saved folder; current folders were kept open",
                            root.root.display()
                        ));
                    }
                }
                Ok(Staged {
                    confirmed: true,
                    ..staged
                })
            })();
            let _ = sender.send(result);
            wake.notify();
        });
    }
    pub(super) fn prepare_workspace_set(&mut self, id: u64) {
        if self.workspaces.opening.is_some() || self.workspace_sets_busy() {
            self.notify_warn("Wait for folders or saved workspaces to finish loading");
            return;
        }
        let Some(set) = self
            .saved_workspace_sets()
            .into_iter()
            .find(|set| set.id == id)
        else {
            self.notify_error("Saved workspace was not found for this connection");
            return;
        };
        let opened: Vec<_> = std::iter::once((self.workspaces.active, self.explorer.root.clone()))
            .chain(
                self.workspaces
                    .parked
                    .iter()
                    .map(|parked| (parked.id, parked.app.explorer.root.clone())),
            )
            .filter(|(_, root)| !root.as_os_str().is_empty())
            .collect();
        let remote = self.remote_workspace.clone();
        let base = crate::portable::base_dir();
        let remote_roots: Vec<_> = std::iter::once((self.workspaces.active, &*self))
            .chain(
                self.workspaces
                    .parked
                    .iter()
                    .map(|parked| (parked.id, &*parked.app)),
            )
            .filter_map(|(id, app)| {
                app.remote_workspace.as_ref().map(|remote| {
                    (
                        id,
                        remote.client.hello().root_uri.clone(),
                        app.explorer.root.clone(),
                    )
                })
            })
            .collect();
        let (sender, receiver) = mpsc::channel();
        let cancel = crate::background::CancellationToken::new();
        self.workspaces.sets.plan = Some(Plan::Loading(StageReply {
            receiver,
            cancel: cancel.clone(),
        }));
        let wake = self.background.wake_handle();
        self.background.spawn("prepare-workspace-set", move || {
            let result = (|| -> Result<Staged, String> {
                let mut roots: Vec<StagedRoot> = vec![];
                let mut active = 0;
                for (index, location) in set.roots.iter().enumerate() {
                    if cancel.is_cancelled() { return Err("Opening the workspace was cancelled".into()); }
                    let known = match location { Location::Remote { uri } => remote_roots.iter().find(|(_, opened, _)| opened == uri), _ => None };
                    let root = if let Some((_, _, root)) = known { root.clone() } else { match (location, &remote) {
                        (Location::Local { .. }, None) => location.local_path(&base).map_err(|error| error.to_string())?,
                        (Location::Remote { uri }, Some(remote)) => remote.target_key(uri).map_err(|error| error.to_string())?,
                        _ => return Err("Workspace uses a different connection".into()),
                    }};
                    let canonical = if remote.is_none() { std::fs::canonicalize(&root).ok() } else { None };
                    let mut existing = opened.iter().find(|(_, path)| same_workspace(path, &root)
                        || canonical.as_ref().is_some_and(|canonical| std::fs::canonicalize(path).is_ok_and(|path| path == *canonical))).map(|(id, _)| *id);
                    let (root, prepared) = if existing.is_some() { (root, None) }
                    else if let Some(remote) = &remote {
                        let workspace = remote.open_workspace(&root).map_err(|error| error.to_string())?;
                        existing = opened.iter().find(|(_, path)| same_workspace(path, &workspace.view_root)).map(|(id, _)| *id);
                        let prepared = existing.is_none().then(|| Prepared { startup: crate::startup::Prepared::load_remote(workspace.view_root.clone()), remote: Some(workspace.clone()), equivalent_roots: vec![] });
                        (workspace.view_root.clone(), prepared)
                    } else {
                        if canonical.is_none() || !root.is_dir() { return Err(format!("{} is not an available directory; current folders were kept open", root.display())); }
                        let startup = crate::startup::Prepared::load(Some(root.clone().into_os_string()));
                        if !same_workspace(&startup.target.root, &root) { return Err("A folder changed during loading".into()); }
                        (root, Some(Prepared { startup, remote: None, equivalent_roots: vec![] }))
                    };
                    if let Some(duplicate) = roots.iter().position(|previous| same_workspace(&previous.root, &root)
                        || canonical.is_some() && previous.canonical == canonical) {
                        if index == set.active { active = duplicate; }
                        continue;
                    }
                    if index == set.active { active = roots.len(); }
                    roots.push(StagedRoot { root, canonical, existing, prepared });
                }
                Ok(Staged { set, roots, active, expected: opened.into_iter().map(|(id, _)| id).collect(), confirmed: false })
            })();
            let _ = sender.send(result); wake.notify();
        });
        self.notify("Preparing workspace…");
    }

    pub(super) fn poll_workspace_sets(&mut self) -> bool {
        let mut changed = false;
        let read = self.workspaces.sets.read.as_ref().and_then(reply);
        let write = self.workspaces.sets.write.as_ref().and_then(reply);
        for (is_write, result) in [(false, read), (true, write)] {
            let Some(result) = result else {
                continue;
            };
            if is_write {
                self.workspaces.sets.write = None;
            } else {
                self.workspaces.sets.read = None;
            }
            match result {
                Ok(catalog) => {
                    self.workspaces.sets.catalog = catalog;
                    self.workspaces.sets.loaded = true;
                    self.workspaces.sets.error = None;
                    if is_write {
                        if self.workspaces.popup.open
                            && matches!(
                                self.workspaces.popup.sets.mode,
                                Some(
                                    crate::ui_workspace_sets::Mode::Save(_)
                                        | crate::ui_workspace_sets::Mode::Rename(_)
                                        | crate::ui_workspace_sets::Mode::Delete(_)
                                )
                            )
                        {
                            crate::ui_workspace_sets::show_list(self);
                        }
                        self.notify("Workspaces saved");
                    }
                }
                Err(error) => {
                    self.workspaces.sets.error = Some(error.clone());
                    self.notify_error(error);
                }
            }
            changed = true;
        }
        let result = match &self.workspaces.sets.plan {
            Some(Plan::Loading(pending)) => reply(&pending.receiver),
            _ => None,
        };
        if let Some(result) = result {
            self.workspaces.sets.plan = match result {
                Ok(staged) => Some(Plan::Ready(staged)),
                Err(error) => {
                    self.notify_error(format!("Could not open workspace: {error}"));
                    None
                }
            };
            changed = true;
        }
        if self.workspaces.exit.is_some() || self.workspace_transition_blocked() {
            return changed;
        }
        let Some(plan) = self.workspaces.sets.plan.take() else {
            return changed;
        };
        match plan {
            Plan::Ready(mut staged) => {
                let mut current: Vec<_> = self
                    .workspace_entries()
                    .iter()
                    .filter(|entry| !entry.path.is_empty())
                    .map(|entry| entry.id)
                    .collect();
                current.sort_unstable();
                let mut expected = staged.expected.clone();
                expected.sort_unstable();
                if current != expected {
                    self.notify_warn(
                        "Open folders changed while preparing the workspace. Try opening it again.",
                    );
                    return true;
                }
                let keep: Vec<_> = staged
                    .roots
                    .iter()
                    .filter_map(|root| root.existing)
                    .collect();
                let close: Vec<_> = current
                    .into_iter()
                    .filter(|id| !keep.contains(id))
                    .collect();
                if !close.is_empty() && !staged.confirmed {
                    self.workspaces.sets.plan = Some(Plan::Ready(staged));
                    crate::ui_workspace_sets::show_confirmation(self);
                    return true;
                }
                if !staged.confirmed && staged.roots.iter().any(|root| root.prepared.is_some()) {
                    // Adding folders still has a preparation/application gap,
                    // even when there is nothing to ask permission to close.
                    self.workspaces.sets.plan = Some(Plan::Ready(staged));
                    self.confirm_workspace_set();
                    return true;
                }
                if staged.roots.iter().any(|root| {
                    root.prepared
                        .as_ref()
                        .and_then(|prepared| prepared.remote.as_ref())
                        .is_some_and(|remote| !remote.client.connected())
                }) {
                    self.notify_error("Remote connection was lost while opening the workspace. Current folders were kept open.");
                    return true;
                }
                let mut target = vec![];
                for root in &mut staged.roots {
                    let id = if let Some(id) = root.existing {
                        id
                    } else {
                        let prepared = root.prepared.take().unwrap();
                        let mut next = prepared
                            .startup
                            .into_app_with_remote(self.background.wake_handle(), prepared.remote);
                        next.workspace_visible = false;
                        next.start_workspace_services();
                        self.park_new_workspace(next);
                        self.workspaces.parked.last().unwrap().id
                    };
                    target.push(id);
                }
                let active = target[staged.active];
                self.workspaces.sets.plan = Some(Plan::Closing(Transition {
                    name: staged.set.name,
                    target,
                    active,
                    close,
                }));
                self.background.wake_handle().notify();
                true
            }
            Plan::Closing(mut transition) => {
                if let Some(id) = transition.close.pop() {
                    self.switch_workspace(id);
                    self.workspaces.sets.plan = Some(Plan::Closing(transition));
                    self.begin_workspace_exit('k', true);
                    if self.workspaces.exit.is_none() {
                        self.cancel_workspace_set_switch();
                        self.notify_warn(
                            "Opening the workspace stopped; remaining folders were kept open",
                        );
                    }
                } else {
                    if self
                        .workspace_entries()
                        .iter()
                        .any(|entry| !entry.path.is_empty() && !transition.target.contains(&entry.id))
                    {
                        self.notify_warn(
                            "Opening the workspace stopped because the open folders changed",
                        );
                        return true;
                    }
                    self.switch_workspace(transition.active);
                    self.notify(format!("Opened workspace: {}", transition.name));
                }
                true
            }
            other => {
                self.workspaces.sets.plan = Some(other);
                changed
            }
        }
    }
}
