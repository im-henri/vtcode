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
}

type Caret = (u64, usize, usize, Option<(usize, usize)>, usize);

struct Origin {
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
}
impl Origin {
    fn capture(app: &App, source: Source) -> Self {
        Self {
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
    fn matches(&self, app: &App) -> bool {
        self.desk == app.desks.active
            && self.group == app.active_group
            && self.pane == app.active_pane
            && self.editor
                == app
                    .tabs
                    .get(app.active_tab)
                    .map(|tab| tab.editor.navigation_id)
            && self.caret == Self::caret(app)
            && Self::overlays(app)
                .iter()
                .zip(self.overlays)
                .all(|(now, before)| !*now || before)
            && (self.pane != ActivePane::Explorer
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
    workspace: Workspace,
    path: PathBuf,
    line: Option<u32>,
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
    pub(crate) fn cancel_remote_location(&mut self) {
        if let Some((_, cancel)) = &self.remote_navigation.running {
            cancel.cancel();
        }
        if let Some((_, cancel)) = &self.remote_navigation.retiring {
            cancel.cancel();
        }
        if let Some(request) = self.remote_navigation.pending.take()
            && self.status_message == request.status
        {
            self.status_message = request.previous_status;
        }
    }

    /// Borrowed comparisons only; recording cursor motion must not clone draft
    /// text, decode URIs, or issue a cancellation RPC on every frame.
    pub(crate) fn cancel_stale_remote_location(&mut self) {
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
        self.cancel_remote_location();
        let Some(workspace) = self.remote_workspace.clone() else {
            return;
        };
        if workspace.remote_path(&path).is_err() {
            self.remote_location_error(
                source,
                "This path belongs to a different remote workspace.".into(),
            );
            return;
        }
        if path == self.explorer.root {
            if source == Source::FileCommand {
                self.remote_location_error(
                    source,
                    "Choose a file, not the workspace folder".into(),
                );
                return;
            }
            self.reveal_remote_directory(&path);
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
                    && tab
                        .editor
                        .file_path
                        .as_deref()
                        .is_some_and(|other| Self::same_open_path(other, &path))
            });
        if live {
            self.open_file(&path);
            if let Some(line) = line
                && let Some(editor) = self.editor_mut()
            {
                editor.set_cursor(line as usize, 0);
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
            path,
            line,
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
        let (id, workspace, path) = (request.id, request.workspace.clone(), request.path.clone());
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
                let result = workspace
                    .open_location(&path, &cancel, progress)
                    .map_err(|error| error.to_string());
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
            && self.remote_workspace.as_ref().is_some_and(|workspace| {
                workspace.view_root == request.workspace.view_root
                    && workspace.client.same_connection(&request.workspace.client)
            });
        if !valid {
            self.discard_remote_location(result);
            return true;
        }
        match result {
            Ok(Location::Directory(path)) => {
                if request.source == Source::FileCommand {
                    self.remote_location_error(
                        request.source,
                        format!("{} is a folder, not a file", self.display_path(&path)),
                    );
                } else {
                    self.reveal_remote_directory(&path);
                }
            }
            Ok(Location::File(loaded)) => {
                self.push_nav_history();
                self.open_file_with_preloaded(&request.path, loaded);
                if let Some(line) = request.line
                    && let Some(editor) = self.editor_mut()
                {
                    editor.set_cursor(line as usize, 0);
                }
            }
            Err(error) => self.remote_location_error(request.source, error),
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
