//! A workspace owns a complete live workbench. Only the visible workbench owns
//! this registry; parked workbenches have empty registries and share its wake.
//! Moving a workbench never recreates documents, PTYs, agents or remote clients.
use crate::{app::App, ui_work::Service};
use std::{
    collections::VecDeque,
    io,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

#[path = "app_workspaces_remote.rs"]
mod remote;
#[path = "app_workspaces_sets.rs"]
pub(crate) mod sets;
#[path = "app_workspaces_idle.rs"]
mod idle;
pub(crate) use idle::GitWork;

#[derive(Default)]
pub(crate) struct Observation {
    started: bool,
    assets_started: bool,
    document_services_started: bool,
    extension_services_started: bool,
    check_all_files: bool,
    pub(crate) pending: PendingChanges,
    checks: rustc_hash::FxHashSet<PathBuf>,
    work: idle::DeferredWork,
    file_checks: crate::background::CancellationToken,
}

/// Replies already in flight can arrive after hiding. Keep their exact hints
/// bounded, just like the watcher; overflow is the exceptional full rescan.
#[derive(Default)]
pub(crate) struct PendingChanges {
    paths: rustc_hash::FxHashMap<PathBuf, crate::background::WorkspacePathChange>,
    structure: bool,
    git: bool,
    rescan: bool,
}
impl PendingChanges {
    pub(crate) fn merge(&mut self, changes: Vec<crate::background::WorkspacePathChange>, structure: bool, rescan: bool, git: bool) {
        self.structure |= structure;
        self.git |= git;
        self.rescan |= rescan;
        if !self.rescan {
            for change in changes {
                self.paths.insert(change.path.clone(), change);
                if self.paths.len() > 1024 { self.rescan = true; break; }
            }
        }
        if self.rescan { self.paths.clear(); }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    Done,
    Attention,
}

#[derive(Default)]
pub(crate) struct Activity {
    pub unread: usize,
    pub attention: bool,
    pub last: String,
}

pub(crate) struct Parked {
    pub id: u64,
    pub app: Box<App>,
}

struct Prepared {
    startup: crate::startup::Prepared,
    remote: Option<crate::remote::workspace::Workspace>,
    equivalent_roots: Vec<PathBuf>,
}

struct Opening {
    receiver: mpsc::Receiver<Result<Prepared, String>>,
    foreground: bool,
    close_after: Option<u64>,
}

#[derive(Default)]
pub(crate) struct Workspaces {
    pub active: u64,
    pub parked: Vec<Parked>,
    pub popup: crate::ui_workspaces::Popup,
    next_id: u64,
    cursors: [usize; 5],
    service_remaining: [usize; 5],
    opening: Option<Opening>,
    /// Completed authentication can arrive while its editor is parked or a
    /// different folder is loading. Retain the transport until it can open.
    pub(crate) remote_ready: VecDeque<(crate::remote::workspace::Workspace, bool)>,
    /// Set by the UI, consumed between event-loop turns, never while draining
    /// a workbench's queue or rendering with its borrowed geometry.
    pub request: Option<Request>,
    pub(crate) exit: Option<Exit>,
    pub sets: sets::State,
}

pub(crate) enum Request {
    Switch(u64),
    // Exercise the shared loader in the foreground in lifecycle tests.
    #[cfg(test)]
    Open(PathBuf),
    Add(PathBuf),
    Replace(PathBuf),
    CloseRemote,
    #[cfg(test)]
    Close,
    CloseFolder(u64),
    Exit(char),
    OpenSet(u64),
}

pub(crate) struct Exit {
    remaining: Vec<u64>,
    choice: char,
    close: bool,
    completed: Vec<(u64, ExitStamp)>,
    ready: bool,
    return_to: Option<u64>,
}

#[derive(PartialEq)]
struct ExitStamp {
    documents: Vec<(u64, u64, u64, u64, bool, Option<PathBuf>)>,
    custom: Vec<(String, Vec<(PathBuf, u64, u64)>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Entry {
    pub id: u64,
    pub name: String,
    pub path: String,
    pub active: bool,
    pub unread: usize,
    pub attention: bool,
    pub running: bool,
    pub last: String,
    pub tabs: usize,
    pub unsaved: usize,
    pub terminals: usize,
}

fn same_workspace(a: &Path, b: &Path) -> bool {
    crate::portable::workspace_dir(a) == crate::portable::workspace_dir(b)
}

impl App {
    /// An empty path represents an editor window with no attached folder.
    pub(crate) fn has_open_folder(&self) -> bool {
        !self.explorer.root.as_os_str().is_empty()
    }

    pub(crate) fn open_folder_count(&self) -> usize {
        usize::from(self.has_open_folder())
            + self.workspaces.parked.iter().filter(|parked| parked.app.has_open_folder()).count()
    }

    fn discardable_empty_window(&self) -> bool {
        !self.has_open_folder() && self.tabs.is_empty()
            && self.groups.iter().all(|group| group.tabs.is_empty())
            && self.terminals.iter().all(|terminal| terminal.is_placeholder && terminal.terminal_pty.is_none())
            && self.chat.agent_order.is_empty() && !self.chat.visible
            && self.pending_custom_recoveries.is_empty()
            && self.desks.items.len() <= 1 && !self.extension_manager.has_busy_runtimes()
    }

    fn discard_empty_window(&mut self) -> bool {
        if !self.discardable_empty_window() { return false; }
        match self.save_finished_workspace() {
            Ok(()) => true,
            Err(error) => { self.notify_warn(format!("Could not save empty window state: {error}")); false }
        }
    }

    pub(crate) fn closing_workspace_only(&self) -> bool {
        !self.workspaces.parked.is_empty()
            && self.workspaces.exit.as_ref().is_some_and(|exit| exit.close)
    }
    pub(crate) fn exit_unsaved_file_count(&self) -> usize {
        if self.closing_workspace_only() {
            self.unsaved_file_count()
        } else {
            self.workspace_unsaved_file_count()
        }
    }
    fn workspace_transition_blocked(&self) -> bool {
        crate::ui_modal::active(self).is_some()
            || self.change_dir_active
            || self.workspaces.popup.open
            || self.paste_receiving()
    }

    pub(crate) fn workspace_entries(&self) -> Vec<Entry> {
        let entry = |id, app: &App, active| Entry {
            id,
            name: if !app.has_open_folder() { "No folder".into() } else { crate::ui_workspaces::escaped(
                &crate::remote::workspace::display_name(&app.explorer.root).unwrap_or_else(|| {
                    app.explorer
                        .root
                        .file_name()
                        .unwrap_or(app.explorer.root.as_os_str())
                        .to_string_lossy()
                        .into_owned()
                }),
            )
            .into_owned() },
            path: crate::ui_workspaces::escaped(&app.display_path(&app.explorer.root)).into_owned(),
            active,
            unread: app.workspace_activity.unread,
            attention: app.workspace_activity.attention
                || app
                    .chat
                    .agents()
                    .any(|(_, agent)| !agent.permissions.is_empty()),
            running: app.chat.any_agent_running()
                || app.terminals.iter().any(|terminal| {
                    terminal
                        .terminal_pty
                        .as_ref()
                        .is_some_and(|pty| pty.grid.commands.running())
                }),
            last: app.workspace_activity.last.clone(),
            tabs: app.tabs.len() + app.groups.iter().map(|group| group.tabs.len()).sum::<usize>(),
            unsaved: app.unsaved_file_count(),
            terminals: (0..app.terminals.len()).filter(|&index| app.terminal_is_open(index)).count(),
        };
        let mut entries = vec![entry(self.workspaces.active, self, true)];
        entries.extend(
            self.workspaces
                .parked
                .iter()
                .map(|parked| entry(parked.id, &parked.app, false)),
        );
        entries.sort_by_key(|entry| entry.id);
        entries
    }

    pub(crate) fn workspace_unread(&self) -> usize {
        self.workspaces
            .parked
            .iter()
            .map(|parked| parked.app.workspace_activity.unread)
            .fold(0usize, usize::saturating_add)
    }

    pub(crate) fn workspace_needs_attention(&self) -> bool {
        self.workspaces.parked.iter().any(|parked| {
            parked.app.workspace_activity.attention
                || parked
                    .app
                    .chat
                    .agents()
                    .any(|(_, agent)| !agent.permissions.is_empty())
        })
    }

    pub(crate) fn workspace_unsaved_file_count(&self) -> usize {
        self.unsaved_file_count()
            + self
                .workspaces
                .parked
                .iter()
                .map(|parked| parked.app.unsaved_file_count())
                .sum::<usize>()
    }

    pub(crate) fn record_workspace_event(&mut self, message: impl Into<String>, level: Level) {
        self.record_workspace_events(message, level, 1);
    }

    pub(crate) fn record_workspace_events(
        &mut self,
        message: impl Into<String>,
        level: Level,
        count: usize,
    ) {
        if self.workspace_visible {
            return;
        }
        let message = message.into();
        self.workspace_activity.unread = self.workspace_activity.unread.saturating_add(count);
        self.workspace_activity.attention |= level == Level::Attention;
        self.workspace_activity.last = message.clone();
        match level {
            Level::Done => self.notify(message),
            Level::Attention => self.notify_error(message),
        }
    }

    /// Called only at an event-loop boundary. Returns true if the visible
    /// workbench changed, so viewport-dependent loop caches can be reset.
    pub(crate) fn poll_workspace_requests(&mut self) -> bool {
        for parked in &mut self.workspaces.parked {
            self.workspaces.remote_ready.extend(parked.app.workspaces.remote_ready.drain(..)
                .map(|(remote, _)| (remote, false)));
        }
        if matches!(self.workspaces.request, Some(Request::Exit(_))) {
            self.cancel_workspace_set_switch();
        }
        let mut changed = self.poll_workspace_sets();
        if !self.workspace_set_switching() {
            self.workspaces.sets.preserving = false;
        }
        if let Some(request) = self.workspaces.request.take() {
            if self.workspaces.exit.is_some() {
                self.notify("Finish or cancel closing folders before switching or opening another folder");
                return false;
            }
            if !matches!(request, Request::Exit(_)) && self.workspace_transition_blocked() {
                self.workspaces.request = Some(request);
                return false;
            }
            if self.workspaces.sets.plan.is_some()
                && !matches!(request, Request::Switch(_) | Request::Exit(_))
            {
                self.notify_warn("Finish or cancel opening the workspace first");
                return changed;
            }
            match request {
                Request::Switch(id) => {
                    for (_, foreground) in &mut self.workspaces.remote_ready { *foreground = false; }
                    if let Some(opening) = &mut self.workspaces.opening {
                        opening.foreground = false;
                    }
                    changed |= self.switch_workspace(id);
                }
                #[cfg(test)]
                Request::Open(path) => {
                    for (_, foreground) in &mut self.workspaces.remote_ready { *foreground = false; }
                    changed |= self.begin_open_workspace(path);
                }
                Request::Add(path) => {
                    for (_, foreground) in &mut self.workspaces.remote_ready { *foreground = false; }
                    changed |= self.begin_open_workspace_in(path, false);
                }
                Request::Replace(path) => self.prepare_folder_replacement(path),
                Request::CloseRemote => {
                    if self.remote_workspace.is_some() {
                        if self.workspaces.parked.is_empty() { changed |= self.prepare_empty_window_for_close(); }
                        else { self.begin_workspace_exit('k', true); }
                    }
                }
                #[cfg(test)]
                Request::Close => self.begin_close_folder(self.workspaces.active),
                Request::CloseFolder(id) => {
                    let previous = self.workspaces.active;
                    self.begin_close_folder(id);
                    changed |= previous != self.workspaces.active;
                }
                Request::Exit(choice) => self.begin_workspace_exit(choice, false),
                Request::OpenSet(id) => self.prepare_workspace_set(id),
            }
        }
        if self.workspaces.opening.is_none() && self.workspaces.exit.is_none()
            && !self.workspace_set_switching() && !self.workspace_transition_blocked()
            && let Some((remote, foreground)) = self.workspaces.remote_ready.pop_front()
        {
            changed |= self.begin_open_remote_target(remote, foreground);
        }
        let adding_in_background = self.workspaces.opening.as_ref()
            .is_some_and(|opening| !opening.foreground && opening.close_after.is_none());
        let blocked = self.workspace_transition_blocked()
            && !(adding_in_background && self.workspaces.popup.open
                && crate::ui_modal::active(self).is_none() && !self.change_dir_active && !self.paste_receiving());
        if !blocked && let Some(opening) = &self.workspaces.opening {
            let foreground = opening.foreground;
            let close_after = opening.close_after;
            let result = match opening.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("Folder loader stopped".into()))
                }
                Err(mpsc::TryRecvError::Empty) => None,
            };
            if let Some(result) = result {
                changed = true;
                self.workspaces.opening = None;
                match result {
                    Ok(prepared) => {
                        if prepared
                            .remote
                            .as_ref()
                            .is_some_and(|remote| !remote.client.connected())
                        {
                            self.notify_error("The remote connection was lost while opening the folder. The current folder was kept open; reconnect before trying again.");
                            self.background.wake_handle().notify();
                            return true;
                        }
                        let root = &prepared.startup.target.root;
                        let existing = self.find_open_workspace(root).or_else(|| {
                            prepared
                                .equivalent_roots
                                .iter()
                                .find_map(|root| self.find_open_workspace(root))
                        });
                        if let Some(id) = existing {
                            if foreground {
                                changed |= self.switch_workspace(id);
                            }
                        } else {
                            let mut next = prepared.startup.into_app_with_remote(
                                self.background.wake_handle(),
                                prepared.remote,
                            );
                            next.workspace_visible = foreground;
                            next.start_workspace_services();
                            if foreground {
                                self.insert_workspace(next);
                            } else {
                                self.park_new_workspace(next);
                                if close_after.is_none() { self.notify("Folder added"); }
                            }
                            changed = true;
                        }
                        if close_after == Some(self.workspaces.active) {
                            self.begin_workspace_exit('k', true);
                        }
                    }
                    Err(error) => self.notify_error(format!("Could not open folder: {error}")),
                }
                // A failed load also needs its final chrome frame.
                self.background.wake_handle().notify();
            }
        }
        changed
    }

    fn find_open_workspace(&self, root: &Path) -> Option<u64> {
        if same_workspace(root, &self.explorer.root) {
            return Some(self.workspaces.active);
        }
        self.workspaces
            .parked
            .iter()
            .find(|parked| same_workspace(root, &parked.app.explorer.root))
            .map(|parked| parked.id)
    }

    pub(crate) fn workspace_opening(&self) -> bool {
        self.workspaces.opening.is_some() || self.workspace_set_loading()
    }

    #[cfg(test)]
    fn begin_open_workspace(&mut self, path: PathBuf) -> bool {
        self.begin_open_workspace_in(path, true)
    }

    fn begin_open_workspace_in(&mut self, path: PathBuf, foreground: bool) -> bool {
        if let Some(id) = self.find_open_workspace(&path) {
            if let Some(opening) = &mut self.workspaces.opening {
                opening.foreground = false;
            }
            return foreground && self.switch_workspace(id);
        }
        if self.workspaces.opening.is_some() {
            self.notify("A folder is already opening");
            return false;
        }
        let open_roots: Vec<_> = std::iter::once(self.explorer.root.clone())
            .chain(
                self.workspaces
                    .parked
                    .iter()
                    .map(|parked| parked.app.explorer.root.clone()),
            )
            .filter(|root| !root.as_os_str().is_empty())
            .collect();
        let remote = self.remote_workspace.clone();
        let (sender, receiver) = mpsc::channel();
        let wake = self.background.wake_handle();
        self.workspaces.opening = Some(Opening {
            receiver,
            foreground,
            close_after: None,
        });
        self.background.spawn("open-live-workspace", move || {
            let result = (|| -> io::Result<Prepared> {
                if let Some(remote) = remote {
                    let remote = remote.open_workspace(&path)?;
                    Ok(Prepared {
                        startup: crate::startup::Prepared::load_remote(remote.view_root.clone()),
                        remote: Some(remote),
                        equivalent_roots: Vec::new(),
                    })
                } else {
                    if !path.is_dir() {
                        return Err(io::Error::other("Choose a folder"));
                    }
                    // Keep the launch/session path spelling stable; lexical
                    // aliases are already normalized by the folder picker.
                    let root = crate::folder_browser::normalized_path(&path);
                    let canonical = std::fs::canonicalize(&root)?;
                    let equivalent_roots = open_roots
                        .into_iter()
                        .filter(|opened| {
                            std::fs::canonicalize(opened).is_ok_and(|other| canonical == other)
                        })
                        .collect();
                    Ok(Prepared {
                        startup: crate::startup::Prepared::load(Some(root.into_os_string())),
                        remote: None,
                        equivalent_roots,
                    })
                }
            })()
            .map_err(|error| error.to_string());
            let _ = sender.send(result);
            wake.notify();
        });
        true
    }

    fn begin_open_remote_target(&mut self, remote: crate::remote::workspace::Workspace, foreground: bool) -> bool {
        if let Some(id) = self.find_open_workspace(&remote.view_root) { return foreground && self.switch_workspace(id); }
        let (sender, receiver) = mpsc::channel();
        let wake = self.background.wake_handle();
        self.workspaces.opening = Some(Opening { receiver, foreground, close_after: None });
        self.background.spawn("open-remote-target", move || {
            let prepared = Prepared { startup: crate::startup::Prepared::load_remote(remote.view_root.clone()),
                remote: Some(remote), equivalent_roots: Vec::new() };
            let _ = sender.send(Ok(prepared));
            wake.notify();
        });
        true
    }

    fn prepare_empty_window_for_close(&mut self) -> bool {
        if self.workspaces.opening.is_some() { self.notify("Wait for the folder to finish opening before disconnecting"); return false; }
        let (sender, receiver) = mpsc::channel();
        let wake = self.background.wake_handle();
        self.workspaces.opening = Some(Opening { receiver, foreground: false, close_after: Some(self.workspaces.active) });
        self.background.spawn("prepare-local-window", move || {
            let _ = sender.send(Ok(Prepared { startup: crate::startup::Prepared::load(None), remote: None, equivalent_roots: Vec::new() }));
            wake.notify();
        });
        true
    }

    fn park_new_workspace(&mut self, mut next: App) {
        if next.settings_global != self.settings_global {
            next.sync_workspace_user_settings(self.settings_global.clone());
        }
        next.prepare_workspace_hide(false);
        self.workspaces.next_id = self
            .workspaces
            .next_id
            .max(self.workspaces.active)
            .saturating_add(1);
        self.workspaces.parked.push(Parked {
            id: self.workspaces.next_id,
            app: Box::new(next),
        });
        self.notify("Folder opened in the background");
    }

    pub(crate) fn start_workspace_services(&mut self) {
        self.workspace_observation.started = true;
        self.refresh_explorer_async();
        self.refresh_source_control_async();
        self.refresh_git_branch_async();
        if self.has_open_folder() { self.initialize_extensions(); }
        self.start_visible_workspace_services();
    }

    pub(crate) fn insert_workspace(&mut self, next: App) {
        let global = (self.settings_global != next.settings_global).then(|| self.settings_global.clone());
        let auto_save = !self.workspace_set_preserving();
        let mut registry = std::mem::take(&mut self.workspaces);
        registry.next_id = registry.next_id.max(registry.active).saturating_add(1);
        let old_id = registry.active;
        registry.active = registry.next_id;
        registry.popup = Default::default();
        let focus = self.terminal_has_focus;
        self.prepare_workspace_hide(auto_save);
        let mut previous = std::mem::replace(self, next);
        if !previous.discard_empty_window() {
            registry.parked.push(Parked {
                id: old_id,
                app: Box::new(previous),
            });
        }
        self.workspaces = registry;
        if let Some(global) = global { self.sync_workspace_user_settings(global); }
        self.prepare_workspace_show(focus);
    }

    pub(crate) fn switch_workspace(&mut self, id: u64) -> bool {
        if self.paste_receiving() {
            self.notify_warn("Finish or cancel the incoming paste before switching folders");
            return false;
        }
        if id == self.workspaces.active {
            return false;
        }
        let Some(index) = self
            .workspaces
            .parked
            .iter()
            .position(|parked| parked.id == id)
        else {
            return false;
        };
        let focus = self.terminal_has_focus;
        if let Some(opening) = &mut self.workspaces.opening {
            opening.foreground = false;
        }
        let global = (self.settings_global != self.workspaces.parked[index].app.settings_global)
            .then(|| self.settings_global.clone());
        let auto_save = !self.workspace_set_preserving();
        let mut registry = std::mem::take(&mut self.workspaces);
        self.prepare_workspace_hide(auto_save);
        std::mem::swap(self, &mut registry.parked[index].app);
        std::mem::swap(&mut registry.active, &mut registry.parked[index].id);
        if registry.parked[index].app.discard_empty_window() {
            registry.parked.remove(index);
            registry.cursors = [0; 5];
            registry.service_remaining = [0; 5];
        }
        registry.popup = Default::default();
        self.workspaces = registry;
        if let Some(global) = global { self.sync_workspace_user_settings(global); }
        self.prepare_workspace_show(focus);
        true
    }

    fn sync_workspace_user_settings(&mut self, settings: crate::settings::Settings) {
        if self.settings_global != settings {
            self.settings_global = settings;
            self.apply_effective_settings();
        }
    }

    fn prepare_workspace_hide(&mut self, auto_save: bool) {
        self.sync_duplicate_file_tabs();
        if auto_save
            && !self.exit_keep_unsaved_quit
            && !self.exit_discard_quit
            && !self.shutdown_anim_active
        {
            self.auto_save_on_focus_change();
        }
        self.forward_terminal_focus(false);
        self.clear_drag_states();
        self.clear_tab_tooltip();
        self.menu_active = false;
        self.menu_open = false;
        self.palette.active = false;
        self.chord_pending = false;
        self.shortcut_chord_prefix = None;
        self.workspace_visible = false;
        self.terminal_has_focus = false;
        self.chat.background = true;
        self.idle_quiescent = true;
        self.suspend_workspace_observation();
    }

    fn prepare_workspace_show(&mut self, focus: bool) {
        let was_hidden = !self.workspace_visible;
        self.workspace_visible = true;
        self.terminal_has_focus = focus;
        self.forward_terminal_focus(focus);
        self.chat.background = false;
        self.idle_quiescent = false;
        self.last_user_activity = Instant::now();
        if self.workspace_activity.unread > 0 {
            self.status_message = self.workspace_activity.last.clone();
        }
        self.workspace_activity.unread = 0;
        self.workspace_activity.attention = false;
        if was_hidden { self.resume_workspace_observation(); }
        self.background.wake_handle().notify();
    }

    /// Rotate each producer independently across workspaces, within the UI's
    /// existing shared budget. A noisy PTY cannot starve another workspace's
    /// permission request, saves, or language server. Idle work has no tick.
    pub(crate) fn service_workspaces_until(
        &mut self,
        service: Service,
        deadline: Instant,
        mut can_continue: impl FnMut() -> bool,
    ) -> bool {
        if service == Service::Workers {
            // User preferences also govern hidden saves and services. Apply
            // changes before any parked workbench can act on stale defaults;
            // each workbench still resolves its own Workspace overrides.
            for parked in &mut self.workspaces.parked {
                if parked.app.settings_global != self.settings_global {
                    parked.app.sync_workspace_user_settings(self.settings_global.clone());
                }
            }
            self.coordinate_workspace_connections();
        }
        let mut registry = std::mem::take(&mut self.workspaces);
        let slot = service.index();
        let count = registry.parked.len() + 1;
        let remaining = if registry.service_remaining[slot] == 0 { count }
            else { registry.service_remaining[slot].min(count) };
        registry.service_remaining[slot] = remaining;
        let mut redraw = false;
        for visited in 0..remaining {
            let index = registry.cursors[slot] % count;
            registry.cursors[slot] = (index + 1) % count;
            registry.service_remaining[slot] -= 1;
            let id = if index == 0 {
                registry.active
            } else {
                registry.parked[index - 1].id
            };
            if registry
                .exit
                .as_ref()
                .is_some_and(|exit| exit.completed.iter().any(|(completed, _)| *completed == id))
                && service != Service::Workers
            {
                continue;
            }
            let app = if index == 0 {
                &mut *self
            } else {
                &mut registry.parked[index - 1].app
            };
            let unread = app.workspace_activity.unread;
            let mut changed = false;
            match service {
                Service::LanguageServers => {
                    crate::app_lsp_poll::poll_lsp_until(app, &mut changed, deadline)
                }
                Service::Workers => {
                    changed |= app.poll_background_results_until(deadline);
                    if app.workspace_visible { changed |= crate::diff_scope_source::poll(app); }
                    if index != 0 {
                        if registry.exit.is_none() && !registry.sets.preserving {
                            crate::app_event_loop::queue_background_workspace_auto_saves(app);
                        }
                    }
                }
                Service::Terminals => {
                    changed |= app.process_terminals_until(deadline, &mut can_continue)
                }
                Service::Agents => changed |= app.poll_acp_until(deadline),
                Service::Extensions => {
                    changed |= app.apply_extension_text_edits();
                    crate::extension_scheduler::dispatch(app);
                }
            }
            redraw |= if index == 0 {
                changed
            } else {
                unread != app.workspace_activity.unread || (changed && registry.popup.open)
            };
            if visited + 1 < remaining && (Instant::now() >= deadline || !can_continue()) {
                break;
            }
        }
        registry.remote_ready.append(&mut self.workspaces.remote_ready);
        self.workspaces = registry;
        redraw
    }

    pub(crate) fn workspace_service_pending(&self, service: Service) -> bool {
        self.workspaces.service_remaining[service.index()] != 0
    }

    pub(crate) fn background_workspace_wait(&self) -> Option<Duration> {
        self.workspaces
            .parked
            .iter()
            .filter(|parked| !self.workspaces.exit.as_ref().is_some_and(|exit|
                exit.completed.iter().any(|(id, _)| *id == parked.id)))
            .filter_map(|parked| {
                crate::app_event_loop::background_workspace_wait(
                    &parked.app,
                    self.workspaces.exit.is_some() || self.workspace_set_preserving(),
                )
            })
            .min()
    }

    pub(crate) fn workspaces_have_pending_results(&self) -> bool {
        self.background.has_pending_results()
            || self
                .workspaces
                .parked
                .iter()
                .any(|parked| parked.app.background.has_pending_results())
    }

    // Exit sequencing is shared by Close Workspace and application quit. Each
    // workbench uses its existing asynchronous custom-document/save/recovery
    // path, so no hidden unsaved document can disappear at shutdown.
    fn begin_close_folder(&mut self, id: u64) {
        let Some(entry) = self.workspace_entries().into_iter().find(|entry| entry.id == id && !entry.path.is_empty()) else { return; };
        let target = if id == self.workspaces.active { Some(&*self) }
            else { self.workspaces.parked.iter().find(|parked| parked.id == id).map(|parked| &*parked.app) };
        if target.is_some_and(|app| app.paste_in_progress() || !app.pending_file_saves.is_empty() || !app.deferred_file_saves.is_empty()) {
            self.notify_error("Wait for pending saves or paste to finish before closing this folder");
            return;
        }
        if self.workspaces.opening.is_some() {
            self.notify_warn("Wait for the folder to finish opening before closing a folder");
            return;
        }
        let previous = self.workspaces.active;
        if self.workspaces.parked.is_empty() {
            self.prepare_empty_window_for_close();
            self.notify(format!("Closing folder: {}", entry.name));
            return;
        }
        if id != previous && !self.switch_workspace(id) { return; }
        self.begin_workspace_exit('k', true);
        if let Some(exit) = &mut self.workspaces.exit {
            exit.return_to = (previous != id).then_some(previous);
        }
    }

    fn begin_workspace_exit(&mut self, choice: char, close: bool) {
        if self.paste_in_progress()
            || (!close
                && self
                    .workspaces
                    .parked
                    .iter()
                    .any(|parked| parked.app.paste_in_progress()))
        {
            self.exit_confirm_active = !close;
            self.modal.error =
                Some("Finish or cancel pending pastes before closing folders".into());
            self.notify_warn("Finish or cancel pending pastes before closing folders");
            return;
        }
        // A Keep & Quit may disconnect an uncertain remote save. Closing one
        // folder must never disconnect the shared transport of its siblings.
        if close
            && (!self.pending_file_saves.is_empty()
                || !self.deferred_file_saves.is_empty()
                || self.paste_in_progress())
        {
            self.notify_error(
                "Wait for pending saves or paste to finish before closing this folder",
            );
            return;
        }
        self.workspaces.opening = None;
        let remaining = if close {
            vec![]
        } else {
            self.workspaces
                .parked
                .iter()
                .map(|parked| parked.id)
                .collect()
        };
        self.workspaces.exit = Some(Exit {
            remaining,
            choice,
            close,
            completed: Vec::new(),
            ready: false,
            return_to: None,
        });
        self.apply_workspace_exit_choice(choice);
    }

    fn apply_workspace_exit_choice(&mut self, choice: char) {
        self.exit_confirm_active = true;
        if crate::input_dialogs::handle_exit_confirm_key(
            self,
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char(choice),
                crossterm::event::KeyModifiers::NONE,
            ),
        )
        .unwrap_or(false)
        {
            self.shutdown_anim_active = true;
        }
    }

    /// Called before the shutdown animation. Returns true while the registry
    /// is handling shutdown, including transitions to another live workbench.
    pub(crate) fn advance_workspace_exit(&mut self) -> bool {
        if self.workspaces.exit.is_none() {
            return false;
        }
        if self.save_error.is_some() || self.custom_exit_failed {
            self.reset_workspace_exit();
            return false;
        }
        if !self.shutdown_anim_active {
            return false;
        }
        let mut exit = self.workspaces.exit.take().unwrap();
        if let Some(id) = exit.completed.iter().find_map(|(id, stamp)| {
            let app = if *id == self.workspaces.active {
                Some(&*self)
            } else {
                self.workspaces
                    .parked
                    .iter()
                    .find(|parked| parked.id == *id)
                    .map(|parked| &*parked.app)
            };
            app.is_none_or(|app| app.workspace_exit_stamp().as_ref() != Some(stamp))
                .then_some(*id)
        }) {
            self.reset_workspace_exit();
            self.switch_workspace(id);
            self.show_save_error("A folder changed while closing. Its newest edits remain open; review them and try again.");
            return true;
        }
        if exit.ready {
            self.workspaces.exit = Some(exit);
            return false;
        }
        let Some(stamp) = self.workspace_exit_stamp() else {
            self.reset_workspace_exit();
            self.show_save_error("Background document work is still pending. Wait for it to finish before closing the folder.");
            return true;
        };
        if exit.close {
            self.shutdown_anim_active = false;
            if let Err(error) = self.save_finished_workspace() {
                self.reset_workspace_exit();
                self.show_save_error(error.to_string());
                return true;
            }
            if let Some(id) = exit.return_to.filter(|id| self.workspaces.parked.iter().any(|parked| parked.id == *id))
                .or_else(|| self.workspaces.parked.first().map(|parked| parked.id)) {
                let old = self.workspaces.active;
                self.switch_workspace(id);
                let Some(index) = self
                    .workspaces
                    .parked
                    .iter()
                    .position(|parked| parked.id == old)
                else {
                    // Switching already saved and discarded an empty window.
                    return true;
                };
                let mut parked = self.workspaces.parked.remove(index);
                if let Some(remote) = &parked.app.remote_workspace {
                    let _ = remote.client.notify(crate::remote::Command::Unwatch);
                    remote.client.release_git_output();
                }
                let lsp = std::mem::replace(
                    &mut parked.app.lsp_manager,
                    crate::lsp::LspManager::new_with_wake(
                        parked.app.explorer.root.clone(),
                        self.background.wake_handle(),
                    ),
                );
                self.background
                    .spawn("closed-workspace-lsp-shutdown", move || drop(lsp));
                drop(parked);
                if self.open_folder_count() == 0 { self.workspaces.sets.active = None; }
            } else {
                self.shutdown_anim_active = true;
                self.shutdown_anim_start = Instant::now();
            }
            return true;
        }
        if let Some(id) = exit.remaining.pop() {
            self.shutdown_anim_active = false;
            if let Err(error) = self.save_finished_workspace() {
                self.reset_workspace_exit();
                self.show_save_error(error.to_string());
                return true;
            }
            exit.completed.push((self.workspaces.active, stamp));
            self.switch_workspace(id);
            let choice = exit.choice;
            self.workspaces.exit = Some(exit);
            self.apply_workspace_exit_choice(choice);
            return true;
        }
        if let Err(error) = self.save_finished_workspace() {
            self.reset_workspace_exit();
            self.show_save_error(error.to_string());
            return true;
        }
        exit.completed.push((self.workspaces.active, stamp));
        exit.ready = true;
        self.workspaces.exit = Some(exit);
        self.shutdown_anim_start = Instant::now();
        false
    }

    pub(crate) fn reset_workspace_exit(&mut self) {
        self.cancel_workspace_set_switch();
        self.workspaces.exit = None;
        self.exit_save_and_quit = false;
        self.exit_keep_unsaved_quit = false;
        self.exit_discard_quit = false;
        self.custom_exit_started = false;
        self.shutdown_anim_active = false;
        self.exit_recovery.saved = false;
        self.exit_recovery.approved = None;
        self.exit_backup_warning = None;
        self.exit_confirm_active = false;
        for parked in &mut self.workspaces.parked {
            parked.app.reset_workspace_exit();
        }
    }

    fn workspace_exit_stamp(&self) -> Option<ExitStamp> {
        if self.paste_in_progress()
            || !self.pending_file_saves.is_empty()
            || !self.deferred_file_saves.is_empty()
            || self.is_formatting()
            || self.lsp_rename_pending
            || self.lsp_on_type_format_target.is_some()
            || !self.pending_lsp_workspace_edits.is_empty()
            || self.lsp_manager.has_pending_workspace_changes()
            || self.extension_manager.records.iter().any(|record| !record.text_edit_requests.is_empty())
            || self.extension_manager.has_busy_runtimes()
            || !self.extension_manager.pending_view_events.is_empty()
        {
            return None;
        }
        let mut custom: Vec<_> = self
            .extension_manager
            .records
            .iter()
            .filter_map(|record| {
                let runtime = record.runtime.as_ref()?;
                Some((
                    record.manifest.id.clone(),
                    runtime
                        .store
                        .data()
                        .documents
                        .iter()
                        .map(|document| document.exit_revision())
                        .collect(),
                ))
            })
            .collect();
        custom.sort_by(|a, b| a.0.cmp(&b.0));
        let mut documents: Vec<_> = self
            .tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .filter(|tab| tab.kind == crate::app::TabKind::File)
            .map(|tab| {
                (
                    tab.editor.navigation_id,
                    tab.editor.save_identity,
                    tab.editor.document_revision(),
                    tab.editor.content_version,
                    tab.editor.modified,
                    tab.editor.file_path.clone(),
                )
            })
            .collect();
        documents.sort_by_key(|document| document.0);
        Some(ExitStamp { documents, custom })
    }

    pub(crate) fn workspace_exit_choice(&mut self, choice: char) {
        if let Some(exit) = &mut self.workspaces.exit {
            exit.choice = choice;
        }
    }

    pub(crate) fn save_finished_workspace(&mut self) -> io::Result<()> {
        let mut session = self.build_session();
        if self.exit_discard_quit {
            for tab in session
                .tabs
                .iter_mut()
                .chain(session.groups.iter_mut().flat_map(|group| &mut group.tabs))
            {
                tab.pending_unsaved_content = None;
                tab.unsaved_file = None;
                tab.undo_history_file = None;
                tab.undo_entries = None;
                tab.redo_entries = None;
            }
        }
        if self.session_baseline.as_ref() != Some(&session) {
            crate::session::save_session(&self.explorer.root, &session)?;
            self.session_baseline = Some(session);
        }
        Ok(())
    }

    pub(crate) fn finish_workspaces_on_shutdown(&mut self) {
        if let Err(error) = self.save_finished_workspace() {
            log_error!("failed to save workspace session: {error}");
        }
        self.lsp_manager.shutdown_all();
        for parked in &mut self.workspaces.parked {
            parked.app.finish_workspaces_on_shutdown();
        }
    }
}

#[cfg(test)]
#[path = "app_workspaces_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "app_empty_window_tests.rs"]
mod empty_tests;
