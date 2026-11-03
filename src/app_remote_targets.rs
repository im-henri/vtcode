//! Saved-target editing and cancellable first-time SSH connections.
use crate::{
    app::{ActivePane, App, SidebarView, Tab, TabKind},
    remote::launch,
    remote_targets::{Store, Target, UpdateError},
};
use ratatui::layout::{Rect, Size};
use std::{path::PathBuf, sync::mpsc, time::Instant};

#[derive(Default)]
pub(crate) struct Field {
    pub text: String,
    pub cursor: usize,
    pub anchor: Option<usize>,
}
impl Field {
    fn new(text: String) -> Self {
        Self {
            cursor: text.len(),
            text,
            anchor: None,
        }
    }
}

pub(crate) struct Form {
    pub fields: [Field; 5],
    pub original: Option<Target>,
    pub focus: usize,
    pub advanced: bool,
    pub error: Option<String>,
    pub scroll: usize,
    pub reveal: bool,
    pub conflict: bool,
}
impl Form {
    pub(crate) fn target(&self) -> Target {
        Target {
            name: self.fields[0].text.trim().into(),
            host: self.fields[1].text.trim().into(),
            folder: self.fields[2].text.clone(),
            remote_command: self.fields[3].text.clone(),
            ssh_command: self.fields[4].text.clone(),
        }
    }
    pub(crate) fn order(&self) -> Vec<usize> {
        let mut order = vec![0, 1, 2, 5];
        if self.advanced {
            order.extend([3, 4]);
        }
        order.extend(if self.conflict {
            vec![9, 10, 8]
        } else {
            vec![7, 6, 8]
        });
        order
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Add,
    Refresh,
    ClearFilter,
    OpenConfig,
    Filter,
    Select(usize),
    ScrollTo(usize),
    Connect(usize),
    Edit(usize),
    Duplicate(usize),
    Remove(usize),
    Menu(usize),
    Focus(usize),
    Advanced,
    Save(bool),
    Cancel,
    ConfirmRemove,
    Retry,
    EditAttempt,
    CopyLog,
    ReloadForm,
    SaveCopy,
    SaveCurrent,
    Status,
    Disconnect,
}

pub(crate) struct Attempt {
    pub id: u64,
    pub target: Target,
    starting: Option<mpsc::Receiver<Result<launch::Setup, String>>>,
    pub setup: Option<launch::Setup>,
    pub grid: crate::terminal::TermGrid,
    pub rows: crate::ui_terminal_rows::RowCache,
    parser: vte::Parser,
    pub error: Option<String>,
    pub input_sent: bool,
    pub input_error: Option<String>,
    pub focus: usize,
    pending_output: Option<Vec<u8>>,
}
impl Drop for Attempt {
    fn drop(&mut self) {
        if let Some(setup) = &self.setup {
            setup.terminate();
        }
    }
}

#[derive(Default)]
pub(crate) struct State {
    pub store: Option<Store>,
    pub loaded: bool,
    pub error: Option<String>,
    pub path: Option<PathBuf>,
    pub filter: Field,
    pub filter_focused: bool,
    pub selected: usize,
    pub scroll: usize,
    pub rows: usize,
    pub form: Option<Form>,
    pub remove: Option<Target>,
    pub remove_error: Option<String>,
    pub attempt: Option<Attempt>,
    next_connection_id: u64,
    pub connection_area: Rect,
    pub connection_hits: Vec<(Rect, Action)>,
    pub menu: Option<(usize, usize, (u16, u16))>,
    pub hits: Vec<(Rect, Action)>,
    pub dialog_hits: Vec<(Rect, Action)>,
    pub dialog_body: Rect,
    pub dialog_max_scroll: usize,
    pub last_click: Option<(usize, Instant)>,
    pub list_area: Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ConnectionStatus {
    Current,
    Connected,
    Reconnecting,
    Disconnected,
    SigningIn,
    Failed,
}
impl ConnectionStatus {
    pub(crate) fn action(self) -> &'static str {
        match self {
            Self::Current => "Open",
            Self::Connected => "Switch",
            Self::Reconnecting => "Show",
            Self::SigningIn | Self::Failed => "Show",
            Self::Disconnected => "Reconnect",
        }
    }
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Current => "Connected here",
            Self::Connected => "Open workspace",
            Self::Reconnecting => "Reconnecting",
            Self::Disconnected => "Disconnected",
            Self::SigningIn => "Signing in",
            Self::Failed => "Connection failed",
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Connection {
    pub id: u64,
    pub status: ConnectionStatus,
}
impl State {
    pub(crate) fn can_edit(&self) -> bool {
        self.loaded && self.store.is_some() && self.error.is_none()
    }
    pub(crate) fn modal(&self) -> bool {
        self.form.is_some() || self.remove.is_some() || self.menu.is_some()
    }
    pub(crate) fn indices(&self) -> Vec<usize> {
        let query = self.filter.text.to_lowercase();
        self.store.as_ref().map_or_else(Vec::new, |store| {
            store
                .targets
                .iter()
                .enumerate()
                .filter(|(_, target)| {
                    [&target.name, &target.host, &target.folder]
                        .iter()
                        .any(|text| text.to_lowercase().contains(&query))
                })
                .map(|(index, _)| index)
                .collect()
        })
    }
    pub(crate) fn selected_index(&self) -> Option<usize> {
        self.indices().get(self.selected).copied()
    }
    pub(crate) fn target(&self, index: usize) -> Option<Target> {
        self.store.as_ref()?.targets.get(index).cloned()
    }
    pub(crate) fn clamp(&mut self) {
        let count = self.indices().len();
        self.selected = self.selected.min(count.saturating_sub(1));
        let rows = self.rows.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll.saturating_add(rows) {
            self.scroll = self.selected + 1 - rows;
        }
        self.scroll = self.scroll.min(count.saturating_sub(rows));
    }
}

impl App {
    pub(crate) fn open_remote_sidebar(&mut self) {
        self.close_palette();
        self.global_search_active = false;
        self.sidebar_view = SidebarView::Remote;
        self.activity_bar_selected = 3;
        self.active_pane = ActivePane::Explorer;
        self.terminal_panel_focused = false;
        self.reveal_sidebar();
        self.reload_remote_targets();
    }

    pub(crate) fn reload_remote_targets(&mut self) {
        let name = self
            .remote_targets
            .selected_index()
            .and_then(|index| self.remote_targets.target(index))
            .map(|target| target.name);
        let path = self
            .remote_targets
            .path
            .clone()
            .unwrap_or_else(crate::remote_targets::path);
        self.remote_targets.loaded = true;
        match Store::load(&path) {
            Ok(store) => {
                self.remote_targets.store = Some(store);
                self.remote_targets.error = None;
                if let Some(name) = name {
                    self.select_remote_target(&name);
                }
                self.remote_targets.clamp();
            }
            Err(error) => {
                self.remote_targets.error = Some(error);
            }
        }
        self.remote_targets.last_click = None;
        self.remote_targets.hits.clear();
    }

    fn select_remote_target(&mut self, name: &str) {
        if let Some(index) = self.remote_targets.indices().iter().position(|&index| {
            self.remote_targets
                .target(index)
                .is_some_and(|target| target.name == name)
        }) {
            self.remote_targets.selected = index;
        }
        self.remote_targets.clamp();
    }

    fn clear_remote_target_inputs(&mut self) {
        self.text_input_histories
            .retain(|id, _| !matches!(id, crate::text_input::TextInputId::RemoteTarget(_)));
        self.remove_text_input_regions(|id| {
            matches!(id, crate::text_input::TextInputId::RemoteTarget(_))
        });
        self.text_input_drag = None;
        self.text_input_click_id = None;
    }

    pub(crate) fn clear_remote_target_filter(&mut self) {
        self.remote_targets.filter = Field::default();
        self.remote_targets.filter_focused = false;
        self.remote_targets.last_click = None;
        self.remote_targets.selected = 0;
        self.remote_targets.scroll = 0;
        self.text_input_histories
            .remove(&crate::text_input::TextInputId::RemoteFilter);
        self.remove_text_input_regions(|id| id == crate::text_input::TextInputId::RemoteFilter);
    }

    pub(crate) fn open_remote_targets_config(&mut self) {
        let path = self
            .remote_targets
            .path
            .clone()
            .unwrap_or_else(crate::remote_targets::path);
        if !path.exists() {
            let result = Store::load(&path).and_then(|mut store| store.save(Vec::new()));
            if let Err(error) = result {
                self.notify_error(error);
                return;
            }
        }
        // Like User Settings, this is a local file even in a remote workspace.
        self.open_file(&path);
        self.active_pane = ActivePane::Editor;
    }

    pub(crate) fn edit_remote_target(&mut self, index: Option<usize>, duplicate: bool) {
        if !self.remote_targets.loaded {
            self.reload_remote_targets();
        }
        if !self.remote_targets.can_edit() {
            self.notify_warn("Refresh or repair the remote targets configuration before editing.");
            return;
        }
        let mut target = index
            .and_then(|index| self.remote_targets.target(index))
            .unwrap_or_default();
        let original = (index.is_some() && !duplicate).then(|| target.clone());
        if duplicate {
            let stem = format!("{} copy", target.name);
            target.name = stem.clone();
            let mut suffix = 2;
            while self
                .remote_targets
                .store
                .as_ref()
                .unwrap()
                .find(&target.name)
                .is_ok()
            {
                target.name = format!("{stem} {suffix}");
                suffix += 1;
            }
        }
        self.begin_remote_target_form(target, original);
    }

    pub(crate) fn save_current_remote_target(&mut self) {
        if !self.remote_targets.loaded {
            self.reload_remote_targets();
        }
        if !self.remote_targets.can_edit() {
            self.notify_warn("Refresh or repair the remote targets configuration before saving.");
            return;
        }
        let Some(target) = self
            .remote_workspace
            .as_ref()
            .and_then(|remote| remote.recipe.as_ref()?.target())
        else {
            return;
        };
        self.begin_remote_target_form(target, None);
    }

    pub(crate) fn begin_remote_target_form(&mut self, target: Target, original: Option<Target>) {
        self.clear_remote_target_inputs();
        self.clear_drag_states();
        self.chord_pending = false;
        self.remote_targets.dialog_hits.clear();
        self.remote_targets.form = Some(Form {
            advanced: !target.remote_command.is_empty() || !target.ssh_command.is_empty(),
            fields: [
                target.name,
                target.host,
                target.folder,
                target.remote_command,
                target.ssh_command,
            ]
            .map(Field::new),
            original,
            focus: 0,
            error: None,
            scroll: 0,
            reveal: true,
            conflict: false,
        });
    }

    pub(crate) fn cancel_remote_target_dialog(&mut self) {
        self.remote_targets.form = None;
        self.remote_targets.remove = None;
        self.remote_targets.remove_error = None;
        self.remote_targets.menu = None;
        self.remote_targets.dialog_hits.clear();
        self.clear_remote_target_inputs();
        self.modal.kind = None;
    }

    pub(crate) fn cancel_remote_target_connection(&mut self) {
        if let Some(attempt) = self.remote_targets.attempt.take() {
            let id = attempt.id;
            drop(attempt);
            self.close_remote_connection_tabs(id);
        }
        self.remote_targets.connection_hits.clear();
        self.remote_targets.connection_area = Rect::default();
    }

    /// Authentication uses the same editor tabs, MRU and split groups as files.
    pub(crate) fn open_remote_connection_tab(&mut self, id: u64) {
        let kind = TabKind::RemoteConnection(id);
        self.record_navigation();
        self.clear_transient_editor_state();
        let existing = self
            .tabs
            .iter()
            .position(|tab| tab.kind == kind)
            .map(|index| (self.active_group, index))
            .or_else(|| {
                self.groups
                    .iter()
                    .enumerate()
                    .filter(|(group, _)| {
                        *group != self.active_group && self.group_in_current_desk(*group)
                    })
                    .find_map(|(group, state)| {
                        state
                            .tabs
                            .iter()
                            .position(|tab| tab.kind == kind)
                            .map(|index| (group, index))
                    })
            });
        if let Some((group, index)) = existing {
            if group != self.active_group {
                self.switch_group(group);
            }
            self.active_tab = index;
        } else {
            let mut editor = self.new_editor();
            editor.read_only = true;
            let mut tab = Tab::new_file(editor);
            tab.kind = kind;
            self.tabs.push(tab);
            self.active_tab = self.tabs.len() - 1;
        }
        self.active_pane = ActivePane::Editor;
        self.terminal_panel_focused = false;
        self.remote_targets.filter_focused = false;
        self.touch_tab_mru(self.active_tab);
        self.validate_diff_state_for_active_tab();
        self.tab_scroll_manual_anchor = None;
        self.ensure_active_tab_visible();
    }

    pub(crate) fn reload_remote_target_form(&mut self) {
        let name = self
            .remote_targets
            .form
            .as_ref()
            .and_then(|form| form.original.as_ref())
            .map(|target| target.name.clone());
        let Some(name) = name else {
            return;
        };
        self.reload_remote_targets();
        if let Some(error) = self.remote_targets.error.clone() {
            if let Some(form) = &mut self.remote_targets.form {
                form.error = Some(error);
            }
            return;
        }
        if let Some(target) = self
            .remote_targets
            .store
            .as_ref()
            .and_then(|store| store.find(&name).ok())
            .cloned()
        {
            self.begin_remote_target_form(target.clone(), Some(target));
        } else if let Some(form) = &mut self.remote_targets.form {
            form.error = Some("This target was removed. Save a copy to keep your draft.".into());
        }
    }

    pub(crate) fn save_remote_target_copy(&mut self) {
        let Some(form) = &self.remote_targets.form else {
            return;
        };
        let mut target = form.target();
        let path = self
            .remote_targets
            .path
            .clone()
            .unwrap_or_else(crate::remote_targets::path);
        let latest = match Store::load(&path) {
            Ok(store) => store,
            Err(error) => {
                self.remote_targets.form.as_mut().unwrap().error = Some(error);
                return;
            }
        };
        let stem = format!("{} copy", target.name);
        target.name = stem.clone();
        let mut suffix = 2;
        while latest.find(&target.name).is_ok() {
            target.name = format!("{stem} {suffix}");
            suffix += 1;
        }
        self.remote_targets.store = Some(latest);
        self.begin_remote_target_form(target, None);
        self.save_remote_target(false);
    }

    pub(crate) fn save_remote_target(&mut self, connect: bool) {
        let Some(form) = &self.remote_targets.form else {
            return;
        };
        let target = form.target();
        let original = form.original.clone();
        let validation = target.validate_fields().and_then(|_| {
            let duplicate = self.remote_targets.store.as_ref().is_some_and(|store| {
                store.targets.iter().any(|existing| {
                    original.as_ref().map(|target| target.name.as_str())
                        != Some(existing.name.as_str())
                        && existing.name.eq_ignore_ascii_case(&target.name)
                })
            });
            if duplicate {
                Err((
                    0,
                    format!(
                        "A remote target named {:?} already exists. Choose a unique name.",
                        target.name
                    ),
                ))
            } else {
                Ok(())
            }
        });
        if let Err((field, error)) = validation {
            if let Some(form) = &mut self.remote_targets.form {
                form.error = Some(error);
                form.focus = field;
                form.advanced |= field >= 3;
                form.reveal = true;
            }
            return;
        }
        let result = self
            .remote_targets
            .store
            .as_mut()
            .ok_or_else(|| UpdateError::Storage("Refresh remote targets before saving".into()))
            .and_then(|store| store.upsert(original.as_ref(), target.clone()));
        match result {
            Ok(()) => {
                self.cancel_remote_target_dialog();
                self.clear_remote_target_filter();
                self.remote_targets.error = None;
                self.select_remote_target(&target.name);
                self.notify(format!("Saved remote target '{}'", target.name));
                if connect {
                    self.connect_remote_target(target);
                }
            }
            Err(error) => {
                if let Some(form) = &mut self.remote_targets.form {
                    form.conflict = error == UpdateError::Conflict;
                    if error == UpdateError::NameInUse {
                        form.focus = 0;
                        form.reveal = true;
                    }
                    form.error = Some(error.to_string());
                }
            }
        }
    }

    pub(crate) fn remove_remote_target(&mut self) {
        let Some(target) = &self.remote_targets.remove else {
            return;
        };
        let target = target.clone();
        let result = self
            .remote_targets
            .store
            .as_mut()
            .ok_or_else(|| UpdateError::Storage("Refresh remote targets before removing".into()))
            .and_then(|store| store.remove(&target));
        match result {
            Ok(()) => {
                self.cancel_remote_target_dialog();
                self.remote_targets.clamp();
                self.notify(format!("Removed remote target '{}'", target.name));
            }
            Err(error) => {
                self.remote_targets.remove_error = Some(if error == UpdateError::Conflict {
                    "This target changed in another window. Cancel and refresh before removing it."
                        .into()
                } else {
                    error.to_string()
                })
            }
        }
    }

    pub(crate) fn remote_target_connection(&self, target: &Target) -> Option<Connection> {
        use crate::app_remote_connection::Phase;
        let launch = target.launch().ok()?;
        std::iter::once((self.workspaces.active, &*self))
            .chain(
                self.workspaces
                    .parked
                    .iter()
                    .map(|parked| (parked.id, &*parked.app)),
            )
            .filter_map(|(id, app)| {
                let signing_in = app.remote_targets.attempt.as_ref().filter(|attempt| {
                    attempt
                        .target
                        .launch()
                        .is_ok_and(|candidate| candidate == launch)
                });
                if let Some(attempt) = signing_in {
                    let status = if attempt.error.is_some() {
                        ConnectionStatus::Failed
                    } else {
                        ConnectionStatus::SigningIn
                    };
                    return Some((
                        if attempt.error.is_some() { 4 } else { 2 },
                        Connection { id, status },
                    ));
                }
                let remote = app.remote_workspace.as_ref()?;
                if !remote
                    .recipe
                    .as_ref()
                    .is_some_and(|recipe| recipe.matches(&launch))
                {
                    return None;
                }
                let status = if remote.client.connected() {
                    if id == self.workspaces.active {
                        ConnectionStatus::Current
                    } else {
                        ConnectionStatus::Connected
                    }
                } else if matches!(
                    app.remote_connection.phase,
                    Phase::Waiting
                        | Phase::Starting
                        | Phase::Connecting
                        | Phase::Resolving
                        | Phase::Restoring
                ) {
                    ConnectionStatus::Reconnecting
                } else {
                    ConnectionStatus::Disconnected
                };
                let rank = match status {
                    ConnectionStatus::Current => 0,
                    ConnectionStatus::Connected => 1,
                    ConnectionStatus::Reconnecting => 2,
                    ConnectionStatus::Disconnected => 3,
                    ConnectionStatus::SigningIn | ConnectionStatus::Failed => unreachable!(),
                };
                Some((rank, Connection { id, status }))
            })
            .min_by_key(|(rank, _)| *rank)
            .map(|(_, connection)| connection)
    }

    pub(crate) fn connect_remote_target(&mut self, target: Target) {
        self.select_remote_target(&target.name);
        if self.workspace_opening() {
            self.notify("A remote target or folder is already opening");
            return;
        }
        let launch = match target.launch() {
            Ok(launch) => launch,
            Err(error) => {
                self.notify_error(error);
                return;
            }
        };
        if let Some(connection) = self.remote_target_connection(&target) {
            let destination = if connection.id == self.workspaces.active {
                &mut *self
            } else {
                &mut *self
                    .workspaces
                    .parked
                    .iter_mut()
                    .find(|parked| parked.id == connection.id)
                    .unwrap()
                    .app
            };
            if matches!(
                connection.status,
                ConnectionStatus::SigningIn | ConnectionStatus::Failed
            ) {
                let id = destination.remote_targets.attempt.as_ref().unwrap().id;
                destination.open_remote_connection_tab(id);
                self.workspaces.request =
                    Some(crate::app_workspaces::Request::Switch(connection.id));
                return;
            }
            if connection.status == ConnectionStatus::Disconnected {
                destination.retry_remote_connection();
            }
            if destination.sidebar_view == SidebarView::Remote {
                destination.sidebar_view = SidebarView::Explorer;
                destination.sync_activity_bar_selection();
                destination.reveal_sidebar();
            }
            if destination.remote_connection.phase
                == crate::app_remote_connection::Phase::Connecting
            {
                destination.open_remote_reconnect_tab();
            }
            self.workspaces.request = Some(crate::app_workspaces::Request::Switch(connection.id));
            return;
        }
        if let Some(attempt) = &self.remote_targets.attempt {
            if attempt.error.is_none() {
                let id = attempt.id;
                self.open_remote_connection_tab(id);
                self.notify("Finish or cancel this SSH sign-in before connecting another target");
                return;
            }
            self.cancel_remote_target_connection();
        }
        let size = Size::new(80, 24);
        let wake = self.background.wake_handle();
        let (sender, receiver) = mpsc::channel();
        self.background.spawn("connect-remote-target", move || {
            let result = launch
                .start(size, wake.clone())
                .map_err(|error| error.to_string());
            if let Err(mpsc::SendError(Ok(setup))) = sender.send(result) {
                setup.terminate();
            }
            wake.notify();
        });
        self.remote_targets.connection_hits.clear();
        self.remote_targets.next_connection_id = self
            .remote_targets
            .next_connection_id
            .wrapping_add(1)
            .max(1);
        let id = self.remote_targets.next_connection_id;
        self.remote_targets.attempt = Some(Attempt {
            id,
            target,
            starting: Some(receiver),
            setup: None,
            grid: crate::terminal::TermGrid::new(
                size.width.max(2) as usize,
                size.height.saturating_sub(2).max(2) as usize,
            ),
            parser: vte::Parser::new(),
            rows: Default::default(),
            error: None,
            input_sent: false,
            input_error: None,
            focus: 0,
            pending_output: None,
        });
        self.open_remote_connection_tab(id);
    }

    pub(crate) fn paste_remote_target_login(&mut self, text: &str) -> bool {
        let Some(attempt) = &mut self.remote_targets.attempt else {
            return false;
        };
        if attempt.error.is_some() {
            return true;
        }
        let result = attempt
            .setup
            .as_ref()
            .ok_or_else(|| "Wait for the SSH sign-in prompt before pasting.".to_string())
            .and_then(|setup| {
                setup
                    .send_paste(text, attempt.grid.bracketed_paste)
                    .map_err(|error| error.to_string())
            });
        match result {
            Ok(()) => {
                attempt.input_sent = true;
                attempt.input_error = None;
            }
            Err(error) => attempt.input_error = Some(format!("Could not send input: {error}")),
        }
        true
    }

    pub(crate) fn poll_remote_target(&mut self, deadline: Instant) -> bool {
        if !self.remote_targets.loaded && self.sidebar_view == SidebarView::Remote {
            self.reload_remote_targets();
            return true;
        }
        if self.exit_keep_unsaved_quit
            || self.exit_discard_quit
            || self.exit_save_and_quit
            || self.shutdown_anim_active
        {
            self.remote_targets.attempt = None;
            return false;
        }
        let Some(attempt) = &mut self.remote_targets.attempt else {
            return false;
        };
        if attempt.error.is_some() {
            return false;
        }
        let mut changed = false;
        if let Some(receiver) = &attempt.starting {
            let result = match receiver.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err("SSH setup worker stopped".into()))
                }
                Err(mpsc::TryRecvError::Empty) => None,
            };
            if let Some(result) = result {
                attempt.starting = None;
                changed = true;
                match result {
                    Ok(setup) => {
                        let _ = setup.resize(attempt.grid.width as u16, attempt.grid.height as u16);
                        attempt.setup = Some(setup);
                    }
                    Err(error) => attempt.error = Some(error),
                }
            }
        }
        if let Some(setup) = &attempt.setup {
            // The output queue is bounded; draining it must not delay input.
            for chunk in 0..32 {
                let Some(bytes) = attempt
                    .pending_output
                    .take()
                    .or_else(|| setup.take_output())
                else {
                    break;
                };
                if Instant::now() >= deadline {
                    attempt.pending_output = Some(bytes);
                    self.background.wake_handle().notify();
                    break;
                }
                for byte in bytes {
                    attempt.parser.advance(&mut attempt.grid, byte);
                }
                changed = true;
                if chunk == 31 {
                    attempt.pending_output = setup.take_output();
                    if attempt.pending_output.is_some() {
                        self.background.wake_handle().notify();
                    }
                }
            }
            if let Some(result) = setup.poll_client() {
                changed = true;
                match result {
                    Ok(client) => {
                        let id = attempt.id;
                        let setup = attempt.setup.take().unwrap();
                        let remote = setup.into_workspace(client);
                        let foreground = self.workspace_visible
                            && self.active_pane == ActivePane::Editor
                            && self
                                .tabs
                                .get(self.active_tab)
                                .is_some_and(|tab| tab.kind == TabKind::RemoteConnection(id));
                        self.remote_targets.attempt = None;
                        self.close_remote_connection_tabs(id);
                        self.workspaces.remote_ready.push_back((remote, foreground));
                        self.background.wake_handle().notify();
                    }
                    Err(error) => {
                        setup.terminate();
                        attempt.error = Some(error.to_string());
                    }
                }
            }
        }
        changed
    }
}

#[cfg(test)]
#[path = "app_remote_targets_tests.rs"]
mod tests;
