//! Reconnection is a workbench lifecycle, never a replay of failed mutations.
//! Buffers, document identities, views and undo stay local throughout recovery.
use crate::{
    app::{App, TabKind},
    background::{BackgroundResult, CancellationToken},
    remote::{launch, workspace},
};
use ratatui::layout::{Rect, Size};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) const NOTICE_ID: &str = "remote-connection";
const LOG_ACTIONS: &[crate::ui_side_notice::Action] = &[crate::ui_side_notice::Action {
    label: "Log", command: "remote_connection_diagnostics",
}];
const LOGIN_ACTIONS: &[crate::ui_side_notice::Action] = &[
    crate::ui_side_notice::Action { label: "Log", command: "remote_connection_diagnostics" },
    crate::ui_side_notice::Action { label: "Sign in", command: "remote_connection_sign_in" },
];
const STOPPED_ACTIONS: &[crate::ui_side_notice::Action] = &[
    crate::ui_side_notice::Action { label: "Log", command: "remote_connection_diagnostics" },
    crate::ui_side_notice::Action { label: "Retry", command: "remote_reconnect" },
];

#[derive(Default, PartialEq, Eq)]
pub(crate) enum Phase {
    #[default]
    Connected,
    Waiting,
    Starting,
    Connecting,
    Resolving,
    Restoring,
    Stopped,
    Restored,
}

pub(crate) struct State {
    pub phase: Phase,
    pub attempt: u32,
    pub serial: u64,
    pub message: String,
    pub error: String,
    pub details: bool,
    trace: Option<crate::remote::diagnostics::Log>,
    pub area: Option<Rect>,
    pub buttons: Vec<(Rect, &'static str)>,
    pub login: Option<LoginOutput>,
    retry: Option<Instant>,
    expires: Option<Instant>,
    setup: Option<launch::Setup>,
    candidate: Option<workspace::Workspace>,
    cancel: Option<CancellationToken>,
    drain_pending: bool,
    pub checking: rustc_hash::FxHashSet<PathBuf>,
    pub conflicts: usize,
    restart_history: bool,
}

pub(crate) struct LoginOutput {
    pub grid: crate::terminal::TermGrid,
    parser: vte::Parser,
}

#[cfg(test)]
#[path = "app_remote_connection_tests.rs"]
mod tests;
impl Default for State {
    fn default() -> Self {
        Self {
            phase: Phase::Connected,
            attempt: 0,
            serial: 0,
            message: String::new(),
            error: String::new(),
            details: false,
            trace: None,
            area: None,
            buttons: Vec::new(),
            login: None,
            retry: None,
            expires: None,
            setup: None,
            candidate: None,
            cancel: None,
            drain_pending: false,
            checking: Default::default(),
            conflicts: 0,
            restart_history: false,
        }
    }
}
impl State {
    pub(crate) fn diagnostic_log(&self) -> Option<crate::remote::diagnostics::Log> { self.trace.clone() }
    pub(crate) fn send_key(&mut self, key: crossterm::event::KeyEvent) {
        if let Some(setup) = &self.setup
            && let Some(login) = &self.login
            && let Err(error) = setup.send_key(key, &login.grid)
        {
            self.error = error.to_string();
        }
    }
    pub(crate) fn send_paste(&mut self, text: &str) {
        if let Some(setup) = &self.setup
            && let Err(error) = setup.send_paste(
                text,
                self.login
                    .as_ref()
                    .is_some_and(|login| login.grid.bracketed_paste),
            )
        {
            self.error = error.to_string();
        }
    }
    pub(crate) fn next_wait(&self) -> Option<Duration> {
        if self.drain_pending {
            return Some(Duration::ZERO);
        }
        self.retry
            .into_iter()
            .chain(self.expires)
            .min()
            .map(|due| due.saturating_duration_since(Instant::now()))
    }
    fn cancel_attempt(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        if let Some(setup) = self.setup.take() {
            setup.terminate();
        }
        if let Some(candidate) = self.candidate.take() {
            candidate.disconnect();
        }
        self.serial = self.serial.wrapping_add(1);
        self.retry = None;
        self.expires = None;
        self.drain_pending = false;
    }
    fn failed(&mut self, error: String) {
        if let Some(trace) = &self.trace { trace.event("Reconnect failed", &error); }
        self.cancel_attempt();
        self.error = error;
        if self.error.contains("VTCode version mismatch:") {
            self.phase = Phase::Stopped;
            self.message = "Update both VTCode builds before reconnecting.".into();
        } else {
            let delay =
                Duration::from_secs((1u64 << self.attempt.saturating_sub(1).min(5)).min(30));
            self.retry = Some(Instant::now() + delay);
            self.phase = Phase::Waiting;
            self.message = format!(
                "Retrying in {} seconds (attempt {}).",
                delay.as_secs(),
                self.attempt.saturating_add(1)
            );
            if let Some(trace) = &self.trace { trace.event("Retry", &self.message); }
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.cancel_attempt();
    }
}

pub(crate) struct FileCheck {
    pub path: PathBuf,
    pub document: u64,
    pub version: u64,
    pub source: ropey::Rope,
    pub baseline: Option<workspace::Baseline>,
    pub loaded: Result<crate::editor::LoadedTextFile, String>,
    pub matches_source: bool,
    pub matches_baseline: bool,
}

impl App {
    /// Refresh only at lifecycle transitions, never on rendering or idle polls.
    /// The shared notice preserves dismissal through retries of this outage.
    pub(crate) fn publish_remote_connection_notice(&mut self, reveal: bool) {
        use crate::{app::NotifyLevel, ui_side_notice::Notice};
        let state = &self.remote_connection;
        let (title, message, level) = match state.phase {
            Phase::Connected => ("SSH connected", "Editing and undo run locally.".into(), NotifyLevel::Info),
            Phase::Restored if state.conflicts > 0 => ("SSH reconnected", format!("{} files need attention. Local edits are safe.", state.conflicts), NotifyLevel::Warning),
            Phase::Restored => ("SSH reconnected", "Connection restored. Local edits are safe.".into(), NotifyLevel::Info),
            Phase::Restoring => ("SSH reconnected", "Checking open files…".into(), NotifyLevel::Info),
            Phase::Resolving => ("SSH reconnected", "Restoring the workspace…".into(), NotifyLevel::Info),
            Phase::Stopped => ("SSH disconnected", state.message.clone(), NotifyLevel::Warning),
            Phase::Waiting => ("SSH connection lost", format!("Retrying connection · attempt {} next", state.attempt.saturating_add(1)), NotifyLevel::Warning),
            Phase::Starting | Phase::Connecting => ("SSH connection lost", format!("Reconnecting · attempt {}", state.attempt), NotifyLevel::Warning),
        };
        let actions = if state.phase == Phase::Connecting { LOGIN_ACTIONS }
            else if state.phase == Phase::Stopped && !state.error.contains("VTCode version mismatch:")
                && self.remote_workspace.as_ref().is_some_and(|remote| remote.recipe.is_some()) { STOPPED_ACTIONS }
            else { LOG_ACTIONS };
        let notice = Notice { title, message, level, actions };
        if reveal { self.side_notice.show(NOTICE_ID, notice); }
        else { self.side_notice.publish(NOTICE_ID, notice); }
    }

    fn remote_reconnect_failed(&mut self, error: String) {
        self.remote_connection.failed(error);
        self.remote_connection.details = false;
        self.publish_remote_connection_notice(false);
    }

    pub(crate) fn stop_remote_reconnect(&mut self) {
        if self
            .remote_workspace
            .as_ref()
            .is_some_and(|remote| remote.client.connected())
        {
            return;
        }
        self.remote_connection.cancel_attempt();
        self.remote_connection.phase = Phase::Stopped;
        self.remote_connection.message =
            "Automatic reconnect stopped. Local edits are retained.".into();
        if let Some(trace) = &self.remote_connection.trace { trace.event("Reconnect", "Automatic retries stopped locally"); }
        self.remote_connection.details = false;
        self.publish_remote_connection_notice(true);
    }
    pub(crate) fn retry_remote_connection(&mut self) {
        if self
            .remote_workspace
            .as_ref()
            .is_none_or(|remote| remote.recipe.is_none())
        {
            return;
        }
        if self
            .remote_workspace
            .as_ref()
            .is_some_and(|remote| remote.client.connected())
        {
            self.notify("Remote workspace is already connected");
            return;
        }
        self.remote_connection.cancel_attempt();
        self.remote_connection.phase = Phase::Waiting;
        self.remote_connection.retry = Some(Instant::now());
        self.remote_connection.message = "Reconnecting…".into();
        if let Some(trace) = &self.remote_connection.trace { trace.event("Reconnect", "Retry requested locally"); }
        self.publish_remote_connection_notice(true);
    }
    pub(crate) fn poll_remote_connection(&mut self, deadline: Instant) -> bool {
        let exiting = self.exit_keep_unsaved_quit
            || self.exit_discard_quit
            || self.exit_save_and_quit
            || self.shutdown_anim_active;
        if exiting {
            if matches!(
                self.remote_connection.phase,
                Phase::Waiting | Phase::Starting | Phase::Connecting | Phase::Resolving
            ) {
                self.stop_remote_reconnect();
            }
            return false;
        }
        let lost = self
            .remote_workspace
            .as_ref()
            .is_some_and(|remote| remote.take_disconnect());
        if lost {
            self.begin_remote_disconnect();
        }
        let mut changed = lost | self.logs.take_changed();
        let now = Instant::now();
        if self.remote_connection.expires.is_some_and(|due| now >= due) {
            self.remote_connection.expires = None;
            self.side_notice.remove(NOTICE_ID);
            self.remote_connection.details = false;
            changed = true;
        }
        if self.remote_connection.phase == Phase::Connecting {
            let state = &mut self.remote_connection;
            state.drain_pending = Instant::now() >= deadline;
            let mut result = None;
            if let Some(setup) = &state.setup {
                while Instant::now() < deadline {
                    let Some(bytes) = setup.take_output() else {
                        state.drain_pending = false;
                        break;
                    };
                    if let Some(login) = &mut state.login {
                        for byte in bytes {
                            login.parser.advance(&mut login.grid, byte);
                        }
                    }
                    state.drain_pending = true;
                    changed |= state.details;
                }
                result = setup.poll_client();
            }
            if let Some(result) = result {
                self.remote_connection.drain_pending = false;
                changed = true;
                match result {
                    Err(error) => self.remote_reconnect_failed(error.to_string()),
                    Ok(client) => {
                        let setup = self.remote_connection.setup.take().unwrap();
                        let candidate = setup.into_workspace(client);
                        candidate.client.set_wake(self.background.wake_handle());
                        self.remote_connection.candidate = Some(candidate.clone());
                        self.remote_connection.phase = Phase::Resolving;
                        self.remote_connection.message = "Restoring the remote workspace…".into();
                        if let Some(trace) = &self.remote_connection.trace { trace.event("Workspace", "Restoring the current remote folder"); }
                        let old = self.remote_workspace.as_ref().unwrap().clone();
                        let serial = self.remote_connection.serial;
                        let generation = self.workspace_generation;
                        let sender = self.background.sender.clone();
                        self.background.spawn("remote-reconnect-folder", move || {
                            let result =
                                old.resume_on(candidate).map_err(|error| error.to_string());
                            let _ = sender.send(BackgroundResult::RemoteReconnected {
                                generation,
                                serial,
                                result,
                            });
                        });
                        self.publish_remote_connection_notice(false);
                    }
                }
            }
        }
        if self.remote_connection.phase == Phase::Waiting
            && self.remote_connection.retry.is_some_and(|due| now >= due)
        {
            self.start_remote_reconnect();
            changed = true;
        }
        changed
    }
    fn begin_remote_disconnect(&mut self) {
        self.remote_connection.trace = self.remote_workspace.as_ref().and_then(|remote| remote.diagnostics());
        let auto = self
            .remote_workspace
            .as_ref()
            .is_some_and(|remote| remote.reconnect_allowed());
        self.remote_connection.cancel_attempt();
        self.remote_connection.attempt = 0;
        self.remote_connection.error = self.remote_workspace.as_ref().and_then(|remote| remote.client.disconnect_reason()).unwrap_or_default();
        self.remote_connection.conflicts = 0;
        self.remote_connection.details = false;
        self.remote_connection.checking.clear();
        self.remote_connection.phase = if auto { Phase::Waiting } else { Phase::Stopped };
        self.remote_connection.retry = auto.then(Instant::now);
        self.remote_connection.message = if auto {
            "Connection lost. Reconnecting…"
        } else {
            "Connection lost. Local edits and undo are retained."
        }
        .into();
        self.notify_warn(self.remote_connection.message.clone());
        self.workspace_generation = self.workspace_generation.wrapping_add(1);
        self.workspace_open_generation = self.workspace_open_generation.wrapping_add(1);
        self.cancel_remote_location();
        self.cancel_workspace_replace();
        self.cancel_remote_disk_checks();
        self.cancel_source_control_reads();
        self.remote_connection.restart_history |= self.suspend_remote_git_history();
        self.blame_cache = None;
        self.blame_documents.clear();
        self.blame_cache_key = None;
        self.blame_requested_path = None;
        self.inline_blame_hover = None;
        self.clear_workspace_lsp_syncs();
        self.pending_file_saves
            .retain(|path, _| !crate::remote::workspace::is_view_key(path));
        self.deferred_file_saves
            .retain(|path, _| !crate::remote::workspace::is_view_key(path));
        self.pending_save_as
            .retain(|_, (_, target)| !crate::remote::workspace::is_view_key(target));
        self.pending_file_reloads.clear();
        self.pending_file_reload_again.clear();
        self.close_hunk_popup();
        self.quick_open_generation = self.quick_open_generation.wrapping_add(1);
        if let Some(cancel) = self.quick_open_cancel_token.take() {
            cancel.cancel();
        }
        self.bg_quick_open_running = false;
        self.bg_explorer_refresh_running = false;
        self.explorer_refresh_active_generation = None;
        self.bg_explorer_refresh_pending = false;
        self.remote_explorer_error = Some("Connection lost. Showing cached files.".into());
        self.explorer.cancel_unloaded_expansions(std::mem::take(&mut self.explorer_expanding));
        self.explorer.find.loading = false;
        self.explorer_refresh_generation = self.explorer_refresh_generation.wrapping_add(1);
        if let Some(cancel) = self.explorer.find.cancel.take() {
            cancel.cancel();
        }
        self.workspace_watcher_active = false;
        self.bg_filesystem_action_running = false;
        self.publish_remote_connection_notice(true);
    }
    fn start_remote_reconnect(&mut self) {
        let Some(recipe) = self
            .remote_workspace
            .as_ref()
            .and_then(|remote| remote.recipe.clone())
        else {
            return;
        };
        let state = &mut self.remote_connection;
        state.phase = Phase::Starting;
        state.retry = None;
        state.attempt = state.attempt.saturating_add(1);
        state.message = format!("Reconnecting… (attempt {})", state.attempt);
        state.serial = state.serial.wrapping_add(1);
        state.login = Some(LoginOutput {
            grid: crate::terminal::TermGrid::new(72, 16),
            parser: vte::Parser::new(),
        });
        let serial = state.serial;
        let generation = self.workspace_generation;
        let cancel = CancellationToken::new();
        state.cancel = Some(cancel.clone());
        self.publish_remote_connection_notice(false);
        let wake = self.background.wake_handle();
        let sender = self.background.sender.clone();
        self.background.spawn("remote-reconnect-launch", move || {
            if cancel.is_cancelled() { return; }
            let result = recipe
                .start(Size::new(72, 18), wake)
                .map_err(|error| error.to_string());
            if cancel.is_cancelled() {
                if let Ok(setup) = result {
                    setup.terminate();
                }
                return;
            }
            let _ = sender.send(BackgroundResult::RemoteReconnectStarted {
                generation,
                serial,
                result,
            });
        });
    }
    pub(crate) fn remote_reconnect_started(
        &mut self,
        generation: u64,
        serial: u64,
        result: Result<launch::Setup, String>,
    ) {
        if generation != self.workspace_generation
            || serial != self.remote_connection.serial
            || self.remote_connection.phase != Phase::Starting
        {
            if let Ok(setup) = result {
                setup.terminate();
            }
            return;
        }
        match result {
            Ok(setup) => {
                self.remote_connection.setup = Some(setup);
                self.remote_connection.phase = Phase::Connecting;
            }
            Err(error) => self.remote_reconnect_failed(error),
        }
        if self.remote_connection.phase == Phase::Connecting {
            self.publish_remote_connection_notice(false);
        }
    }
    pub(crate) fn finish_remote_reconnect(
        &mut self,
        generation: u64,
        serial: u64,
        result: Result<workspace::Workspace, String>,
    ) {
        if generation != self.workspace_generation
            || serial != self.remote_connection.serial
            || self.remote_connection.phase != Phase::Resolving
        {
            if let Ok(remote) = result {
                remote.disconnect();
            }
            return;
        }
        self.remote_connection.candidate = None;
        match result {
            Err(error) => self.remote_reconnect_failed(error),
            Ok(remote) => self.resume_remote_services(remote),
        }
    }
    pub(crate) fn resume_remote_services(&mut self, remote: workspace::Workspace) {
        if self
            .remote_workspace
            .as_ref()
            .is_none_or(|old| old.view_root != remote.view_root)
        {
            remote.disconnect();
            self.remote_reconnect_failed("Remote workspace changed; buffers were retained.".into());
            return;
        }
        remote.client.set_wake(self.background.wake_handle());
        remote.client.set_git_output(&self.logs.git_log());
        self.remote_workspace = Some(remote.clone());
        self.remote_explorer_error = None;
        self.remote_connection.phase = Phase::Restoring;
        self.remote_connection.error.clear();
        self.remote_connection.message = "Connected. Checking open files…".into();
        self.remote_connection.details = false;
        self.publish_remote_connection_notice(false);
        let mut files = rustc_hash::FxHashMap::default();
        for tab in self
            .tabs
            .iter_mut()
            .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
        {
            if tab.kind != TabKind::File {
                continue;
            }
            tab.editor.refresh_document();
            let editor = &tab.editor;
            let Some(path) = editor
                .file_path
                .clone()
                .filter(|path| crate::remote::workspace::is_view_key(path))
            else {
                continue;
            };
            if files.contains_key(&path) {
                continue;
            }
            let baseline = editor
                .remote_checkpoint
                .as_ref()
                .zip(editor.saved_rope.as_ref())
                .map(|(checkpoint, rope)| workspace::Baseline {
                    checkpoint: checkpoint.clone(),
                    rope: rope.clone(),
                    encoding: editor.saved_encoding,
                });
            files.entry(path.clone()).or_insert(FileCheck {
                path,
                document: editor.save_identity,
                version: editor.content_version,
                source: editor.rope.clone(),
                baseline,
                loaded: Err(String::new()),
                matches_source: false,
                matches_baseline: false,
            });
        }
        self.remote_connection.checking = files.keys().cloned().collect();
        if let Some(trace) = &self.remote_connection.trace { trace.event("Revalidation", &format!("Checking {} unique open text files", files.len())); }
        let generation = self.workspace_generation;
        let focused_path = self.editor().and_then(|editor| editor.file_path.clone());
        let focused = focused_path.as_ref().and_then(|path| files.remove(path));
        for mut check in focused.into_iter().chain(files.into_values()) {
            let path = check.path.clone();
            let foreground = focused_path.as_ref() == Some(&path);
            let sender = self.background.sender.clone();
            let remote = remote.clone();
            self.background
                .read_for_file("remote-reconnect-check", &path, foreground, move || {
                    check.loaded = remote
                        .reload(&check.path, check.baseline.as_ref())
                        .map_err(|error| error.to_string());
                    if let Ok(loaded) = &check.loaded {
                        check.matches_source =
                            crate::rope_changes::equal(&check.source, &loaded.rope);
                        check.matches_baseline = check.baseline.as_ref().is_some_and(|old| {
                            old.encoding == loaded.encoding
                                && crate::rope_changes::equal(&old.rope, &loaded.rope)
                        });
                    }
                    let _ =
                        sender.send(BackgroundResult::RemoteReconnectFile { generation, check });
                });
        }
        self.refresh_language_servers(true);
        self.bg_git_branch_pending = false;
        self.bg_git_status_running = false;
        self.source_control_last_refresh = None;
        self.refresh_explorer_async();
        self.refresh_source_control_async();
        self.refresh_git_branch_async();
        if self.remote_connection.restart_history {
            self.remote_connection.restart_history = false;
            self.refresh_git_log_async();
        }
        let _ = remote.client.notify(crate::remote::Command::Watch);
        self.complete_remote_reconnect_if_ready();
    }
    pub(crate) fn finish_remote_reconnect_file(&mut self, generation: u64, check: FileCheck) {
        if generation != self.workspace_generation
            || !self.remote_connection.checking.remove(&check.path)
        {
            return;
        }
        let threshold = self.large_file_threshold_mb.saturating_mul(1024 * 1024);
        let mut conflict = false;
        for tab in self
            .tabs
            .iter_mut()
            .chain(self.groups.iter_mut().flat_map(|group| &mut group.tabs))
        {
            let editor = &mut tab.editor;
            editor.refresh_document();
            if tab.kind != TabKind::File
                || editor.file_path.as_ref() != Some(&check.path)
                || editor.save_identity != check.document
            {
                continue;
            }
            match &check.loaded {
                Ok(loaded) if editor.bg_loading || editor.save_content_error().is_some() => {
                    editor.apply_reloaded_file(loaded.clone());
                    editor.read_only = self.default_read_only || loaded.encoding == "Binary";
                    editor.publish_document();
                }
                Ok(loaded) if check.matches_source && editor.saved_encoding == loaded.encoding => {
                    editor.apply_background_save(
                        check.version,
                        &check.source,
                        loaded.rope.clone(),
                        loaded.encoding,
                        loaded.last_disk_modified,
                        Some(loaded.file_size),
                        None,
                        threshold,
                    );
                    editor.set_remote_checkpoint(loaded.remote_checkpoint.clone());
                }
                Ok(loaded) if check.matches_baseline => {
                    editor.set_remote_checkpoint(loaded.remote_checkpoint.clone());
                    editor.last_disk_modified = loaded.last_disk_modified;
                    editor.file_size = loaded.file_size;
                    editor.disk_unavailable = false;
                    editor.publish_document();
                }
                Ok(loaded) if !editor.modified => {
                    editor.apply_reloaded_file(loaded.clone());
                    editor.read_only = self.default_read_only || loaded.encoding == "Binary";
                    editor.publish_document();
                }
                Ok(_) => {
                    if let Some(old) = &editor.remote_checkpoint {
                        let mut checkpoint = (**old).clone();
                        checkpoint.conflicted = true;
                        editor.set_remote_checkpoint(Some(Arc::new(checkpoint)));
                    }
                    conflict = true;
                }
                Err(_) => {
                    conflict = true;
                }
            }
            // One shared document can have many views and inactive desks.
            // Publishing the update once preserves each view and avoids
            // interpreting our own reload as another concurrent edit.
            break;
        }
        if conflict {
            self.remote_connection.conflicts += 1;
            self.remote_change_warnings.insert(check.path.clone());
            self.notify_warn(format!("{} changed or is unavailable after reconnecting. Your local edits are preserved; compare before saving.", self.display_path(&check.path)));
        }
        self.recheck_deferred_remote_file(&check.path);
        self.complete_remote_reconnect_if_ready();
    }
    fn complete_remote_reconnect_if_ready(&mut self) {
        if self.remote_connection.phase == Phase::Restoring
            && self.remote_connection.checking.is_empty()
        {
            self.remote_connection.phase = Phase::Restored;
            if self.remote_connection.conflicts > 0 {
                self.remote_connection.message = format!(
                    "{} {} changed or could not be checked. Local edits are intact. Compare before saving.",
                    self.remote_connection.conflicts,
                    if self.remote_connection.conflicts == 1 {
                        "file"
                    } else {
                        "files"
                    }
                );
                self.remote_connection.expires = None;
            } else {
                self.remote_connection.message =
                    "Connection restored. Your local edits and undo are intact.".into();
                self.remote_connection.expires = Some(Instant::now() + Duration::from_secs(5));
            }
            self.notify(self.remote_connection.message.clone());
            if let Some(trace) = &self.remote_connection.trace { trace.event("Restored", &self.remote_connection.message); }
            self.publish_remote_connection_notice(false);
        }
    }
}
