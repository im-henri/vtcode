//! Resolve unclassified links off the input thread. One active request, one
//! retiring request and one replaceable destination bound rapid clicks. The
//! replacement can start without waiting another SSH round trip for cancellation.
use crate::app::{ActivePane, App, SidebarView, TabKind};
use crate::background::{BackgroundResult, CancellationToken};
use crate::remote::workspace::{Location, Workspace};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    Chat,
    Markdown,
    FileCommand,
    PathDialog,
    Terminal,
}

type Caret = (u64, usize, usize, Option<(usize, usize)>, usize);

pub(crate) struct Origin {
    path_dialog: bool,
    desk: u64,
    group: usize,
    pane: ActivePane,
    editor: Option<u64>,
    caret: Option<Caret>,
    explorer: Option<PathBuf>,
    chat: Option<(
        u64,
        Option<vtcode::acp::protocol::SessionId>,
        u64,
        usize,
        usize,
        Option<usize>,
    )>,
    overlays: [bool; 5],
    terminal: Option<u64>,
}
impl Origin {
    pub(crate) fn capture(app: &App, source: Source) -> Self {
        Self {
            path_dialog: source == Source::PathDialog,
            desk: app.desks.active,
            group: app.active_group,
            pane: app.active_pane,
            editor: app
                .tabs
                .get(app.active_tab)
                .map(|tab| tab.editor.navigation_id),
            caret: Self::caret(app),
            explorer: (app.active_pane == ActivePane::Explorer)
                .then(|| {
                    app.explorer
                        .entries
                        .get(app.explorer.selected)
                        .map(|entry| entry.path.clone())
                })
                .flatten(),
            chat: (source == Source::Chat).then(|| {
                (
                    app.chat.active_agent,
                    app.chat.selected.clone(),
                    app.chat.input_history.revision(),
                    app.chat.input.len(),
                    app.chat.cursor,
                    app.chat.anchor,
                )
            }),
            overlays: Self::overlays(app),
            terminal: (source == Source::Terminal).then(|| app.focused_terminal_index()
                .map(|index| app.terminals[index].find.id)).flatten(),
        }
    }
    fn caret(app: &App) -> Option<Caret> {
        (app.active_pane == ActivePane::Editor)
            .then(|| app.editor())
            .flatten()
            .map(|editor| {
                (
                    editor.content_version,
                    editor.cursor_line,
                    editor.cursor_col,
                    editor.selection_anchor,
                    editor.scroll_offset,
                )
            })
    }
    fn overlays(app: &App) -> [bool; 5] {
        [
            app.palette.active,
            app.quick_open_active,
            app.goto_line_active,
            app.save_as_active,
            app.menu_active,
        ]
    }
    pub(crate) fn matches(&self, app: &App) -> bool {
        self.desk == app.desks.active
            && self.group == app.active_group
            && self.pane == app.active_pane
            && self.editor
                == app
                    .tabs
                    .get(app.active_tab)
                    .map(|tab| tab.editor.navigation_id)
            // A path prompt owns the input while background loads or formatting
            // can still update the editor and Explorer behind it.
            && (self.path_dialog || self.caret == Self::caret(app))
            && self.terminal.is_none_or(|id| app.focused_terminal_index()
                .is_some_and(|index| app.terminals[index].find.id == id))
            && Self::overlays(app)
                .iter()
                .zip(self.overlays)
                .all(|(now, before)| !*now || before)
            && (self.path_dialog || self.pane != ActivePane::Explorer
                || self.explorer.as_ref()
                    == app
                        .explorer
                        .entries
                        .get(app.explorer.selected)
                        .map(|entry| &entry.path))
            && self
                .chat
                .as_ref()
                .is_none_or(|(agent, session, revision, len, cursor, anchor)| {
                    *agent == app.chat.active_agent
                        && *session == app.chat.selected
                        && *revision == app.chat.input_history.revision()
                        && *len == app.chat.input.len()
                        && *cursor == app.chat.cursor
                        && *anchor == app.chat.anchor
                })
    }
}

struct Request {
    id: u64,
    generation: u64,
    // Local terminal links share cancellation and focus validation with SSH.
    workspace: Option<Workspace>,
    root: PathBuf,
    path: PathBuf,
    line: Option<u32>,
    column: u32,
    source: Source,
    origin: Origin,
    status: String,
    previous_status: String,
}

#[derive(Default)]
pub(crate) struct State {
    next: u64,
    pending: Option<Request>,
    running: Option<(u64, CancellationToken)>,
    retiring: Option<(u64, CancellationToken)>,
}
impl Drop for State {
    fn drop(&mut self) {
        if let Some((_, cancel)) = &self.running {
            cancel.cancel();
        }
        if let Some((_, cancel)) = &self.retiring {
            cancel.cancel();
        }
    }
}

impl App {
    pub(crate) fn cancel_remote_terminal_location(&mut self) {
        self.cancel_terminal_directory_reveal();
        if self.remote_navigation.pending.as_ref().is_some_and(|request| request.source == Source::Terminal) {
            self.cancel_remote_location();
        }
    }

    pub(crate) fn cancel_remote_location(&mut self) {
        self.cancel_terminal_directory_reveal();
        if let Some((_, cancel)) = &self.remote_navigation.running {
            cancel.cancel();
        }
        if let Some((_, cancel)) = &self.remote_navigation.retiring {
            cancel.cancel();
        }
        if let Some(request) = self.remote_navigation.pending.take()
        {
            if request.source == Source::PathDialog { self.file_open.pending = false; }
            if self.status_message == request.status { self.status_message = request.previous_status; }
        }
    }

    /// Borrowed comparisons only; recording cursor motion must not clone draft
    /// text, decode URIs, or issue a cancellation RPC on every frame.
    pub(crate) fn cancel_stale_remote_location(&mut self) {
        self.cancel_stale_terminal_directory_reveal();
        if self
            .remote_navigation
            .pending
            .as_ref()
            .is_some_and(|request| !request.origin.matches(self))
        {
            self.cancel_remote_location();
        }
    }

    pub(crate) fn open_remote_location(
        &mut self,
        path: PathBuf,
        line: Option<u32>,
        source: Source,
    ) {
        self.open_remote_location_at(path, line, 0, source);
    }

    pub(crate) fn open_remote_location_at(
        &mut self, path: PathBuf, line: Option<u32>, column: u32, source: Source,
    ) {
        if self.remote_workspace.is_some() { self.open_location_at(path, line, column, source); }
    }

    pub(crate) fn open_terminal_location_at(&mut self, path: PathBuf, line: Option<u32>, column: u32) {
        self.open_location_at(path, line, column, Source::Terminal);
    }

    pub(crate) fn open_path_dialog_location(&mut self, path: PathBuf) {
        self.open_location_at(path, None, 0, Source::PathDialog);
    }

    fn open_location_at(&mut self, path: PathBuf, line: Option<u32>, column: u32, source: Source) {
        self.cancel_remote_location();
        let workspace = self.remote_workspace.clone();
        let remote_key = crate::remote::workspace::is_view_key(&path);
        let local_file_dialog = source == Source::PathDialog && !remote_key;
        if !local_file_dialog && ((remote_key && workspace.is_none())
            || workspace.as_ref().is_some_and(|workspace| workspace.remote_path(&path).is_err())) {
            self.remote_location_error(
                source,
                "This path belongs to a different remote workspace.".into(),
            );
            return;
        }
        if path == self.explorer.root {
            if matches!(source, Source::FileCommand | Source::PathDialog) {
                self.remote_location_error(
                    source,
                    "Choose a file, not the workspace folder".into(),
                );
                return;
            }
            if source == Source::Terminal { self.reveal_terminal_directory(&path); }
            else { self.reveal_remote_directory(&path); }
            self.hide_chat_after_remote_location(source);
            return;
        }
        // Live buffers take precedence over stale explorer entries, including
        // unsaved buffers and views in inactive desks. Do not reload their text.
        let live = self
            .tabs
            .iter()
            .chain(self.groups.iter().flat_map(|group| &group.tabs))
            .any(|tab| {
                tab.kind == TabKind::File
                    && !tab.editor.load_failed
                    && tab
                        .editor
                        .file_path
                        .as_deref()
                        .is_some_and(|other| Self::same_open_path(other, &path))
            });
        if live {
            if source == Source::PathDialog {
                self.file_open.active = false;
                self.file_open.pending = false;
                self.modal = Default::default();
            }
            self.open_file(&path);
            if source == Source::Terminal { self.terminal_panel_focused = false; }
            if let Some(line) = line
                && let Some(editor) = self.editor_mut()
            {
                if source == Source::Terminal {
                    if editor.file_path.as_deref().is_some_and(|other| Self::same_open_path(other, &path)) {
                        editor.set_centered_cursor(line as usize, column as usize);
                    }
                }
                else { editor.set_cursor(line as usize, column as usize); }
            }
            self.hide_chat_after_remote_location(source);
            return;
        }
        self.remote_navigation.next = self.remote_navigation.next.wrapping_add(1);
        let status = format!("Opening: {}...", self.display_path(&path));
        let request = Request {
            id: self.remote_navigation.next,
            generation: self.workspace_generation,
            workspace,
            root: self.explorer.root.clone(),
            path,
            line,
            column,
            source,
            origin: Origin::capture(self, source),
            status: status.clone(),
            previous_status: std::mem::replace(&mut self.status_message, status),
        };
        self.remote_navigation.pending = Some(request);
        self.start_remote_location();
    }

    fn start_remote_location(&mut self) {
        self.cancel_stale_remote_location();
        let Some(request) = &self.remote_navigation.pending else {
            return;
        };
        if let Some((running, _)) = &self.remote_navigation.running {
            if *running == request.id || self.remote_navigation.retiring.is_some() {
                return;
            }
            self.remote_navigation.retiring = self.remote_navigation.running.take();
        }
        let request = self.remote_navigation.pending.as_ref().unwrap();
        let (id, workspace, path, root, source) = (request.id,
            request.workspace.clone().filter(|_| crate::remote::workspace::is_view_key(&request.path)),
            request.path.clone(), request.root.clone(), request.source);
        let sender = self.background.sender.clone();
        let cancel = self
            .background
            .spawn_cancellable("remote-location", move |cancel| {
                let progress_sender = sender.clone();
                let progress = crate::remote::text::throttled_progress(move |completed, total| {
                    let _ = progress_sender.send(BackgroundResult::RemoteLocationProgress {
                        id,
                        completed,
                        total,
                    });
                });
                let result = match workspace {
                    Some(workspace) => workspace.open_location(&path, &cancel, progress),
                    None => crate::app_terminal_links::open_local_location(&path, &root, &cancel,
                        matches!(source, Source::FileCommand | Source::PathDialog)),
                }.map_err(|error| error.to_string());
                // A cancelled large result is freed here, never on the input thread.
                let result = if cancel.is_cancelled() {
                    drop(result);
                    Err("File open cancelled".into())
                } else {
                    result
                };
                let _ = sender.send(BackgroundResult::RemoteLocationResolved { id, result });
            });
        self.remote_navigation.running = Some((id, cancel));
    }

    pub(crate) fn apply_remote_location(
        &mut self,
        id: u64,
        result: Result<Location, String>,
    ) -> bool {
        if self
            .remote_navigation
            .retiring
            .as_ref()
            .is_some_and(|(retiring, _)| *retiring == id)
        {
            self.remote_navigation.retiring = None;
            self.discard_remote_location(result);
            self.start_remote_location();
            return false;
        }
        if !self
            .remote_navigation
            .running
            .as_ref()
            .is_some_and(|(running, _)| *running == id)
        {
            self.discard_remote_location(result);
            return false;
        }
        self.remote_navigation.running = None;
        if !self
            .remote_navigation
            .pending
            .as_ref()
            .is_some_and(|request| request.id == id)
        {
            self.discard_remote_location(result);
            self.start_remote_location();
            return false;
        }
        let request = self.remote_navigation.pending.take().unwrap();
        if self.status_message == request.status {
            self.status_message = request.previous_status.clone();
        }
        let valid = request.generation == self.workspace_generation
            && request.origin.matches(self)
            && (request.source != Source::PathDialog || (self.file_open.active && self.file_open.pending
                && crate::ui_modal::active(self) == Some(crate::ui_modal::Kind::OpenFilePath)))
            && request.root == self.explorer.root
            && match (&self.remote_workspace, &request.workspace) {
                (Some(workspace), Some(original)) => workspace.view_root == original.view_root
                    && workspace.client.same_connection(&original.client),
                (None, None) => true,
                _ => false,
            };
        if !valid {
            if request.source == Source::PathDialog { self.file_open.pending = false; }
            self.discard_remote_location(result);
            return true;
        }
        match result {
            Ok(Location::Directory(path)) => {
                if matches!(request.source, Source::FileCommand | Source::PathDialog) {
                    self.remote_location_error(
                        request.source,
                        format!("{} is a folder, not a file", self.display_path(&path)),
                    );
                } else if request.source == Source::Terminal {
                    self.reveal_terminal_directory(&path);
                } else {
                    self.reveal_remote_directory(&path);
                }
            }
            Ok(Location::File(loaded)) => {
                if request.source == Source::PathDialog {
                    self.file_open.active = false;
                    self.file_open.pending = false;
                    self.modal = Default::default();
                }
                self.push_nav_history();
                self.open_file_with_preloaded(&request.path, loaded);
                if request.source == Source::Terminal { self.terminal_panel_focused = false; }
                if let Some(line) = request.line
                    && let Some(editor) = self.editor_mut()
                {
                    if request.source == Source::Terminal {
                        if editor.file_path.as_deref().is_some_and(|other| Self::same_open_path(other, &request.path)) {
                            editor.set_centered_cursor(line as usize, request.column as usize);
                        }
                    }
                    else { editor.set_cursor(line as usize, request.column as usize); }
                }
            }
            Err(error) => {
                if request.source == Source::Terminal {
                    crate::app_terminal_links::show_path_error(self, &request.path, error);
                } else { self.remote_location_error(request.source, error); }
            }
        }
        self.hide_chat_after_remote_location(request.source);
        true
    }

    pub(crate) fn apply_remote_location_progress(
        &mut self,
        id: u64,
        completed: u64,
        total: u64,
    ) -> bool {
        let Some(request) = self
            .remote_navigation
            .pending
            .as_ref()
            .filter(|request| request.id == id)
        else {
            return false;
        };
        if request.generation != self.workspace_generation || !request.origin.matches(self) {
            return false;
        }
        let percent = if total == 0 {
            100
        } else {
            ((completed as u128 * 100) / total as u128).min(100)
        };
        let status = format!(
            "Opening {} · {:.1}/{:.1} MiB ({percent}%)",
            self.display_path(&request.path),
            completed as f64 / 1_048_576.0,
            total as f64 / 1_048_576.0
        );
        let request = self.remote_navigation.pending.as_mut().unwrap();
        if self.status_message != request.status {
            return false;
        }
        if status == request.status {
            return false;
        }
        request.status = status.clone();
        self.status_message = status;
        true
    }

    fn discard_remote_location(&mut self, result: Result<Location, String>) {
        if let Ok(Location::File(loaded)) = result {
            self.background
                .spawn("remote-location-drop", move || drop(loaded));
        }
    }

    fn reveal_remote_directory(&mut self, path: &Path) {
        self.show_explorer = true;
        self.sidebar_view = SidebarView::Explorer;
        self.active_pane = ActivePane::Explorer;
        if path == self.explorer.root {
            self.explorer.selected = 0;
            self.explorer.scroll_offset = 0;
        } else if self.explorer.reveal_file_deferred(path) {
            self.refresh_explorer_tree_async();
        }
    }
    fn remote_location_error(&mut self, source: Source, error: String) {
        match source {
            Source::Chat => self.chat.error = Some(error),
            Source::Markdown | Source::FileCommand => self.notify_warn(error),
            Source::PathDialog => {
                self.file_open.pending = false;
                self.modal.error = Some(error);
            },
            Source::Terminal => crate::app_terminal_links::show_error(self, error),
        }
    }
    fn hide_chat_after_remote_location(&mut self, source: Source) {
        if source == Source::Chat && self.workbench_body_area.is_some_and(|area| area.width == 0) {
            self.chat.visible = false;
        }
    }
}

#[cfg(test)]
#[path = "app_remote_navigation_tests.rs"]
mod tests;
