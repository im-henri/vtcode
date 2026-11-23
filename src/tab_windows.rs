//! Owns terminal connections and routes their input into one workbench.
use std::{
    collections::{BTreeMap, VecDeque},
    ffi::OsString,
    io,
    ops::Bound::{Excluded, Unbounded},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use crossterm::{
    event::{Event, KeyEventKind, MouseButton, MouseEvent, MouseEventKind},
    terminal::SetTitle,
};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport, layout::Rect};
use rustc_hash::FxHashSet;

use crate::{
    app::App,
    app_event_loop::TerminalEventPump,
    background::UiWake,
    terminal_output::{AnsiBackend, CursorBackend, queue_ansi},
};

#[path = "tab_window_desktop.rs"]
mod desktop;
#[path = "tab_window_frontend.rs"]
mod frontend;
#[cfg(test)]
#[path = "tab_window_tests.rs"]
mod tests;
#[path = "tab_window_transport.rs"]
mod transport;

pub(crate) use desktop::ScreenPoint;
pub(crate) use desktop::Tracker as DesktopTracker;

pub(crate) type WindowId = u64;
pub(crate) const MAIN_WINDOW: WindowId = 0;

pub(crate) enum Request {
    FocusSourceControl {
        view: u64,
        window: WindowId,
    },
    Open {
        view: u64,
        point: Option<ScreenPoint>,
    },
    Move {
        view: u64,
        window: WindowId,
    },
    OpenFile {
        path: PathBuf,
        window: Option<WindowId>,
    },
}

pub(crate) fn run_frontend(endpoint: OsString) -> io::Result<()> {
    frontend::run(endpoint)
}

type WindowTerminal = Terminal<CursorBackend<AnsiBackend<Vec<u8>>>>;

struct Surface {
    terminal: WindowTerminal,
    desired_area: Rect,
    resized: bool,
    input_closed: bool,
    _connection: frontend::Connection,
    output: frontend::Output,
    events: TerminalEventPump,
    host: Arc<AtomicU64>,
    dirty: bool,
    title: String,
}

fn screen_area((width, height): (u16, u16)) -> io::Result<Rect> {
    if width == 0 || height == 0 || u32::from(width) * u32::from(height) > 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid detached terminal size",
        ));
    }
    Ok(Rect::new(0, 0, width, height))
}

impl Surface {
    fn ready(&self) -> bool {
        self.output.ready.load(Ordering::Acquire)
    }

    /// Visible windows repaint their changes whether or not any vtcode
    /// window has focus; pacing keeps background bursts cheap.
    fn background_ready(&self) -> bool {
        self.dirty && self.ready()
    }

    fn resize(&mut self, width: u16, height: u16) {
        if let Ok(area) = screen_area((width, height)) {
            self.desired_area = area;
            self.resized = true;
            self.dirty = true;
        }
    }

    fn paint(&mut self, title: String, draw: impl FnOnce(&mut Frame)) -> io::Result<()> {
        if !self.ready() {
            self.dirty = true;
            return Ok(());
        }
        if let Ok(buffer) = self.output.buffers.try_recv() {
            self.terminal.backend_mut().reuse_output(buffer);
        }
        let resized = std::mem::take(&mut self.resized);
        if self.terminal.get_frame().area() != self.desired_area {
            self.terminal.resize(self.desired_area)?;
        } else if resized {
            // The host can reflow through other sizes before settling here.
            self.terminal.clear()?;
        }
        if self.title != title {
            queue_ansi(self.terminal.backend_mut(), SetTitle(&title))?;
            self.title = title;
        }
        self.terminal.draw(draw)?;
        if let Some(bytes) = self.terminal.backend_mut().take_output() {
            self.output.ready.store(false, Ordering::Release);
            self.output
                .frames
                .try_send(bytes)
                .map_err(io::Error::other)?;
        }
        self.dirty = false;
        Ok(())
    }

    fn host(&self) -> Option<desktop::Host> {
        let value = self.host.load(Ordering::Acquire);
        (value != 0).then_some(value)
    }
}

struct Window {
    workspace: u64,
    source: WindowSource,
    surface: Option<Surface>,
    launch: Option<frontend::Launch>,
}

#[derive(PartialEq, Eq)]
enum WindowSource {
    Tab { view: u64, window: WindowId },
    File { path: PathBuf, generation: u64 },
}

impl WindowSource {
    fn is_valid(&self, app: &App) -> bool {
        match self {
            Self::Tab { view, window } => app.tab_window(*view) == Some(*window),
            Self::File { generation, .. } => app.workspace_generation == *generation,
        }
    }

    fn operation(&self) -> &'static str {
        match self {
            Self::Tab { .. } => "detach tab",
            Self::File { .. } => "open file in a new window",
        }
    }
}

struct DesktopDrag {
    source: WindowId,
    workspace: u64,
    view: u64,
    press: desktop::Press,
}

pub(crate) struct Windows {
    id: WindowId,
    main_workspace: u64,
    observed_main: Option<u64>,
    main_dirty: bool,
    main_size: Option<(u16, u16)>,
    main_host: Option<desktop::Host>,
    windows: BTreeMap<WindowId, Window>,
    populated: FxHashSet<WindowId>,
    populated_desks: FxHashSet<u64>,
    retired: Vec<WindowId>,
    last_input_window: WindowId,
    focused_frame_needed: bool,
    deferred_input: BTreeMap<WindowId, VecDeque<Event>>,
    next_window: WindowId,
    last_background_window: WindowId,
    next_background_paint: Instant,
    notices: mpsc::Sender<(WindowId, frontend::Notice)>,
    incoming: mpsc::Receiver<(WindowId, frontend::Notice)>,
    wake: UiWake,
    drag: Option<DesktopDrag>,
    #[cfg(target_os = "linux")]
    tracker: Option<Arc<DesktopTracker>>,
}

fn workspace(app: &App, id: u64) -> Option<&App> {
    if app.workspaces.active == id {
        Some(app)
    } else {
        app.workspaces
            .parked
            .iter()
            .find(|workspace| workspace.id == id)
            .map(|workspace| workspace.app.as_ref())
    }
}

fn workspace_mut(app: &mut App, id: u64) -> Option<&mut App> {
    if app.workspaces.active == id {
        Some(app)
    } else {
        app.workspaces
            .parked
            .iter_mut()
            .find(|workspace| workspace.id == id)
            .map(|workspace| workspace.app.as_mut())
    }
}

fn transfer_blocked(app: &App) -> bool {
    crate::ui_modal::active(app).is_some() || app.change_dir_active || app.paste_receiving()
}

impl Windows {
    pub(crate) fn new(app: &App) -> Self {
        let (notices, incoming) = mpsc::channel();
        let wake = app.background.wake_handle();
        Self {
            id: MAIN_WINDOW,
            main_workspace: app.workspaces.active,
            observed_main: None,
            main_dirty: false,
            main_size: None,
            main_host: None,
            windows: BTreeMap::new(),
            populated: FxHashSet::default(),
            populated_desks: FxHashSet::default(),
            retired: Vec::new(),
            last_input_window: MAIN_WINDOW,
            focused_frame_needed: false,
            deferred_input: BTreeMap::new(),
            next_window: 1,
            last_background_window: MAIN_WINDOW,
            next_background_paint: Instant::now(),
            notices,
            incoming,
            #[cfg(target_os = "linux")]
            tracker: if cfg!(test) {
                None
            } else {
                desktop::start_tracker(wake.clone())
            },
            wake,
            drag: None,
        }
    }

    pub(crate) fn is_main(&self) -> bool {
        self.id == MAIN_WINDOW
    }

    pub(crate) fn active(&self) -> WindowId {
        self.id
    }

    fn surface_mut(&mut self, id: WindowId) -> Option<&mut Surface> {
        self.windows.get_mut(&id)?.surface.as_mut()
    }

    pub(crate) fn active_pump<'a>(
        &'a mut self,
        main: &'a mut TerminalEventPump,
    ) -> &'a mut TerminalEventPump {
        self.surface_mut(self.id)
            .map_or(main, |surface| &mut surface.events)
    }

    pub(crate) fn ready_to_draw(&self) -> bool {
        self.windows
            .get(&self.id)
            .and_then(|window| window.surface.as_ref())
            .is_none_or(Surface::ready)
    }

    pub(crate) fn input_waits_for_frame(&self) -> bool {
        self.focused_frame_needed
    }

    pub(crate) fn has_foreground_input(&mut self, main: &mut TerminalEventPump) -> bool {
        !self.focused_frame_needed
            && (self.deferred_input.contains_key(&self.id)
                || self.active_pump(main).has_pending_event())
    }

    #[cfg(test)]
    pub(crate) fn invalidate_background(&mut self) {
        self.main_dirty |= !self.is_main();
        for (&id, window) in &mut self.windows {
            if id != self.id
                && let Some(surface) = &mut window.surface
            {
                surface.dirty = true;
            }
        }
    }

    pub(crate) fn invalidate_workspace(&mut self, workspace: u64) {
        self.main_dirty |= !self.is_main() && self.main_workspace == workspace;
        for (&id, window) in &mut self.windows {
            if id != self.id
                && window.workspace == workspace
                && let Some(surface) = &mut window.surface
            {
                surface.dirty = true;
            }
        }
    }

    pub(crate) fn invalidate_pending_workspace_redraw(&mut self, app: &mut App) {
        if std::mem::take(&mut app.tab_window_redraw_pending) {
            self.invalidate_workspace(app.workspaces.active);
        }
    }

    fn invalidate_window(&mut self, id: WindowId) {
        if id == MAIN_WINDOW {
            self.main_dirty = !self.is_main();
        } else if let Some(surface) = self.surface_mut(id) {
            surface.dirty = true;
        }
    }

    pub(crate) fn background_draw_ready(&self) -> bool {
        (!self.is_main() && self.main_dirty)
            || self.windows.iter().any(|(&id, window)| {
                id != self.id
                    && window
                        .surface
                        .as_ref()
                        .is_some_and(|surface| surface.background_ready())
            })
    }

    pub(crate) fn background_draw_wait(&self, now: Instant) -> Option<Duration> {
        self.background_draw_ready()
            .then(|| self.next_background_paint.saturating_duration_since(now))
    }

    pub(crate) fn pace_background_draws(&mut self, finished: Instant) {
        // Coalesce bursts while leaving the focused editor's frame rate free.
        self.next_background_paint = finished + Duration::from_millis(24);
    }

    pub(crate) fn note_main_resize(&mut self, width: u16, height: u16) {
        if width > 0 && height > 0 {
            self.main_size = Some((width, height));
            self.main_dirty = true;
        }
    }

    pub(crate) fn apply_main_resize<B: ratatui::backend::Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
    ) -> Result<(), B::Error> {
        if let Some((width, height)) = self.main_size.take()
            && crate::terminal_output::apply_resize(terminal, &Event::Resize(width, height))?
                .is_none()
        {
            // A sequence of resizes may have returned to the old dimensions.
            terminal.clear()?;
        }
        Ok(())
    }

    pub(crate) fn paint_main_background<B: ratatui::backend::Backend>(
        &mut self,
        terminal: &mut Terminal<B>,
        app: &mut App,
    ) -> Result<(), B::Error> {
        if self.is_main() || !self.main_dirty {
            return Ok(());
        }
        if let Some(owner) = workspace_mut(app, self.main_workspace) {
            self.apply_main_resize(terminal)?;
            terminal.draw(|frame| crate::ui::draw_background_window(frame, owner, MAIN_WINDOW))?;
        }
        self.main_dirty = false;
        Ok(())
    }

    pub(crate) fn paint_active(&mut self, app: &mut App) {
        let id = self.id;
        self.paint(id, app, |frame, app| crate::ui::draw(frame, app));
    }

    pub(crate) fn paint_background(&mut self, app: &mut App) {
        let budget = std::time::Instant::now() + std::time::Duration::from_millis(5);
        let mut ready: Vec<_> = self
            .windows
            .iter()
            .filter_map(|(&id, window)| {
                (id != self.id
                    && window
                        .surface
                        .as_ref()
                        .is_some_and(|surface| surface.background_ready()))
                .then_some(id)
            })
            .collect();
        let next = ready.partition_point(|&id| id <= self.last_background_window);
        ready.rotate_left(next);
        let mut failed = Vec::new();
        for id in ready {
            let Some(window) = self.windows.get_mut(&id) else {
                continue;
            };
            let Some(surface) = &mut window.surface else {
                continue;
            };
            let Some(owner) = workspace_mut(app, window.workspace) else {
                continue;
            };
            let title = terminal_title(owner, id);
            if surface
                .paint(title, |frame| {
                    crate::ui::draw_background_window(frame, owner, id)
                })
                .is_err()
            {
                failed.push(id);
            }
            self.last_background_window = id;
            if std::time::Instant::now() >= budget {
                break;
            }
        }
        for id in failed {
            self.close(app, id);
        }
    }

    fn paint(&mut self, id: WindowId, app: &mut App, draw: impl FnOnce(&mut Frame, &mut App)) {
        let title = terminal_title(app, id);
        if let Some(surface) = self.surface_mut(id)
            && let Err(error) = surface.paint(title, |frame| draw(frame, app))
        {
            log_warn!("Detached terminal output failed: {error}");
            self.close(app, id);
        }
    }

    pub(crate) fn resize_active(&mut self, event: &Event) {
        if let Event::Resize(width, height) = *event
            && let Some(surface) = self.surface_mut(self.id)
        {
            surface.resize(width, height);
        }
    }

    pub(crate) fn next_event(
        &mut self,
        main: &mut TerminalEventPump,
    ) -> io::Result<Option<(WindowId, Event)>> {
        if !self.focused_frame_needed
            && let Some(events) = self.deferred_input.get_mut(&self.id)
        {
            let event = events.pop_front();
            if events.is_empty() {
                self.deferred_input.remove(&self.id);
            }
            if let Some(event) = event {
                return Ok(Some((self.id, event)));
            }
        }
        let blocked = self.focused_frame_needed.then_some(self.id);
        let read_detached = |(&id, window): (&WindowId, &mut Window)| {
            let Some(surface) = &mut window.surface else {
                return None;
            };
            if Some(id) == blocked || surface.input_closed {
                return None;
            }
            match surface.events.try_recv() {
                Ok(Some(event @ Event::Resize(width, height))) if id != self.id => {
                    surface.resize(width, height);
                    Some((id, event))
                }
                Ok(event) => event.map(|event| (id, event)),
                Err(error) => {
                    // Report a disconnected queue once, even if the reader's
                    // final notice hasn't reached the UI yet.
                    surface.input_closed = true;
                    let _ = self
                        .notices
                        .send((id, frontend::Notice::Closed(Err(error))));
                    self.wake.notify();
                    None
                }
            }
        };
        let mut event = self
            .windows
            .range_mut((Excluded(self.last_input_window), Unbounded))
            .find_map(read_detached);
        if event.is_none() && blocked != Some(MAIN_WINDOW) {
            event = main.try_recv()?.map(|event| (MAIN_WINDOW, event));
        }
        if event.is_none() {
            event = self
                .windows
                .range_mut(..=self.last_input_window)
                .find_map(read_detached);
        }
        if let Some((source, _)) = &event {
            self.last_input_window = *source;
        }
        Ok(event)
    }

    pub(crate) fn defer_input_until_focused_frame(&mut self, event: Event) {
        self.deferred_input
            .entry(self.id)
            .or_default()
            .push_back(event);
    }

    pub(crate) fn focused_frame_drawn(&mut self) {
        self.focused_frame_needed = false;
    }

    fn select_window(&mut self, source: WindowId) {
        let previous = std::mem::replace(&mut self.id, source);
        if previous != source {
            if let Some(events) = self.deferred_input.get_mut(&previous) {
                // Only text and keys stay meaningful if focus moves again
                // before this window has a layout for its mouse coordinates.
                events.retain(|event| matches!(event, Event::Key(_) | Event::Paste(_)));
                if events.is_empty() {
                    self.deferred_input.remove(&previous);
                }
            }
            self.focused_frame_needed = true;
            self.invalidate_window(previous);
        }
    }

    pub(crate) fn accept_focus(&mut self, app: &mut App, source: WindowId, event: &Event) -> bool {
        let claims_focus = matches!(
            event,
            Event::FocusGained
                | Event::Paste(_)
                | Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(_),
                    ..
                })
        ) || matches!(event, Event::Key(key) if key.kind != KeyEventKind::Release);
        if source == MAIN_WINDOW
            && (matches!(event, Event::FocusGained) || (self.main_host.is_none() && claims_focus))
        {
            if let Some(host) = desktop::foreground_host()
                && !self
                    .windows
                    .values()
                    .any(|window| window.surface.as_ref().and_then(Surface::host) == Some(host))
            {
                self.main_host = Some(host);
            }
        }
        if source == self.id {
            if claims_focus && !app.terminal_has_focus {
                app.terminal_has_focus = true;
                app.forward_terminal_focus(true);
                app.idle_quiescent = false;
            }
            return true;
        }
        if !claims_focus || transfer_blocked(app) {
            return false;
        }
        self.activate(app, source, true)
    }

    fn activate(&mut self, app: &mut App, source: WindowId, focused: bool) -> bool {
        let owner = if source == MAIN_WINDOW {
            self.main_workspace
        } else if let Some(window) = self
            .windows
            .get(&source)
            .filter(|window| window.surface.is_some())
        {
            window.workspace
        } else {
            return false;
        };
        // Keep the main window's workspace watched before parking it.
        if source != MAIN_WINDOW {
            self.observe_main(app, Some(self.main_workspace));
        }
        if app.workspaces.active != owner {
            app.switch_workspace(owner);
        }
        if (source != MAIN_WINDOW && app.workspaces.active != owner) || !app.activate_window(source)
        {
            self.sync_main_observation(app);
            return false;
        }
        self.select_window(source);
        if self.is_main() {
            self.main_workspace = app.workspaces.active;
        }
        self.sync_main_observation(app);
        app.terminal_has_focus = focused;
        app.forward_terminal_focus(focused);
        app.idle_quiescent = !focused;
        true
    }

    fn sync_main_observation(&mut self, app: &mut App) {
        self.observe_main(app, (!self.is_main()).then_some(self.main_workspace));
    }

    fn observe_main(&mut self, app: &mut App, next: Option<u64>) {
        if self.observed_main == next {
            return;
        }
        if let Some(owner) = self.observed_main.and_then(|id| workspace_mut(app, id)) {
            owner.set_main_window_in_background(false);
        }
        self.observed_main = next;
        if let Some(owner) = next.and_then(|id| workspace_mut(app, id)) {
            owner.set_main_window_in_background(true);
        }
    }

    fn focus(&self, id: WindowId) {
        let target = if id == MAIN_WINDOW {
            self.main_host
        } else {
            self.windows
                .get(&id)
                .and_then(|window| window.surface.as_ref())
                .and_then(Surface::host)
        };
        if let Some(host) = target.filter(|&host| desktop::valid_host(host)) {
            desktop::focus(host);
        }
    }

    fn close(&mut self, app: &mut App, id: WindowId) -> bool {
        let Some(window) = self.windows.remove(&id) else {
            return false;
        };
        let workspace_id = window.workspace;
        let attached = window.surface.is_some();
        self.deferred_input.remove(&id);
        // Close the pipe or the pending launch before any tab/layout cleanup.
        drop(window);
        if attached {
            if let Some(owner) = workspace_mut(app, workspace_id) {
                if owner.detached_window_id() == Some(id) && owner.paste_receiving() {
                    owner.terminal_paste_status(crossterm::event::PasteStatus::Failed {
                        bytes: 0,
                        reason: crossterm::event::PasteFailure::Interrupted,
                    });
                }
                owner.return_detached_tabs(id);
            }
            self.invalidate_workspace(workspace_id);
        }
        if id == self.id {
            self.activate(app, MAIN_WINDOW, false);
        }
        true
    }

    fn attach(&mut self, app: &mut App, id: WindowId, ready: frontend::Ready) -> io::Result<bool> {
        if !self.windows.contains_key(&id) {
            return Ok(false);
        }
        let initial_host = ready.host.load(Ordering::Acquire);
        if initial_host != 0
            && (self.main_host == Some(initial_host)
                || self.windows.values().any(|window| {
                    window
                        .surface
                        .as_ref()
                        .is_some_and(|surface| surface.host.load(Ordering::Acquire) == initial_host)
                }))
        {
            // The helper can start while another terminal is still foreground.
            // Wait for its own focus or mouse packet to establish ownership.
            let _ =
                ready
                    .host
                    .compare_exchange(initial_host, 0, Ordering::AcqRel, Ordering::Acquire);
        }
        let window = self.windows.get_mut(&id).unwrap();
        let area = screen_area(ready.size)?;
        let terminal = Terminal::with_options(
            CursorBackend::for_window(),
            TerminalOptions {
                viewport: Viewport::Fixed(area),
            },
        )?;
        let Some(owner) = workspace_mut(app, window.workspace) else {
            return Err(io::Error::other("the tab's workspace was closed"));
        };
        if transfer_blocked(owner) {
            return Err(io::Error::other(
                "finish the open dialog or paste and try again",
            ));
        }
        if !window.source.is_valid(owner) {
            return Err(io::Error::other(
                "the source changed before the terminal opened",
            ));
        }
        let opened = match &window.source {
            WindowSource::Tab { view, .. } => owner.open_detached_window(id, *view),
            WindowSource::File { path, .. } => owner.open_detached_file_window(id, path),
        };
        if !opened {
            return Err(io::Error::other("the destination could not be opened"));
        }
        window.surface = Some(Surface {
            terminal,
            desired_area: area,
            resized: false,
            input_closed: false,
            _connection: ready.connection,
            output: ready.output,
            events: TerminalEventPump::from_receiver(ready.inputs),
            host: ready.host,
            dirty: true,
            title: String::new(),
        });
        window.launch = None;
        let workspace_id = window.workspace;
        self.invalidate_workspace(workspace_id);
        Ok(true)
    }

    fn launch_window(
        &mut self,
        app: &mut App,
        source: WindowSource,
        point: Option<ScreenPoint>,
    ) -> bool {
        let workspace = app.workspaces.active;
        if self.windows.values().any(|window| {
            window.surface.is_none() && window.workspace == workspace && window.source == source
        }) {
            return false;
        }
        if self.is_main() && self.main_host.is_none() {
            self.main_host = desktop::foreground_host();
        }
        let id = self.next_window;
        self.next_window += 1;
        let notices = self.notices.clone();
        let wake = self.wake.clone();
        let notify = move |event| {
            notices.send((id, event)).map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "the owning workbench has closed")
            })?;
            wake.notify();
            Ok(())
        };
        match frontend::start(self.wake.clone(), notify, point) {
            Ok(launch) => {
                self.windows.insert(
                    id,
                    Window {
                        workspace,
                        source,
                        surface: None,
                        launch: Some(launch),
                    },
                );
            }
            Err(error) => app.notify_warn(format!("Could not {}: {error}", source.operation())),
        }
        true
    }

    fn request(&mut self, app: &mut App, request: Request) -> bool {
        if transfer_blocked(app) {
            return false;
        }
        match request {
            Request::FocusSourceControl { view, window } => {
                if app.tab_window(view) != Some(window) || !self.activate(app, window, false) {
                    return false;
                }
                app.open_source_control_tab(false);
                self.focus(window);
                self.invalidate_workspace(app.workspaces.active);
                true
            }
            Request::Open { view, point } => {
                let Some(window) = app.tab_window(view) else {
                    return false;
                };
                self.launch_window(app, WindowSource::Tab { view, window }, point)
            }
            Request::Move { view, window } => {
                if app.move_tab_to_window(view, window) {
                    let workspace = app.workspaces.active;
                    if window == MAIN_WINDOW {
                        self.main_workspace = workspace;
                        self.activate(app, MAIN_WINDOW, self.is_main() && app.terminal_has_focus);
                    }
                    self.focus(window);
                    self.invalidate_workspace(workspace);
                    true
                } else {
                    false
                }
            }
            Request::OpenFile { path, window: None } => {
                let generation = app.workspace_generation;
                self.launch_window(app, WindowSource::File { path, generation }, None)
            }
            Request::OpenFile {
                path,
                window: Some(window),
            } => {
                if app.open_file_in_window(window, &path) {
                    self.invalidate_workspace(app.workspaces.active);
                } else {
                    app.notify_warn("The selected window has closed");
                }
                true
            }
        }
    }

    pub(crate) fn poll(&mut self, app: &mut App) -> bool {
        if self.is_main() {
            if self.main_workspace != app.workspaces.active {
                self.deferred_input.remove(&MAIN_WINDOW);
            }
            self.main_workspace = app.workspaces.active;
        }
        self.invalidate_pending_workspace_redraw(app);
        let mut changed = app
            .tab_window_request
            .take()
            .is_some_and(|request| self.request(app, request));
        while let Ok((id, notice)) = self.incoming.try_recv() {
            match notice {
                frontend::Notice::Ready(ready) => match self.attach(app, id, ready) {
                    Ok(attached) => changed |= attached,
                    Err(error) => {
                        let operation = self
                            .windows
                            .get(&id)
                            .map_or("open window", |window| window.source.operation());
                        self.close(app, id);
                        app.notify_warn(format!("Could not {operation}: {error}"));
                        changed = true;
                    }
                },
                frontend::Notice::Closed(result) => {
                    let pending_operation = self
                        .windows
                        .get(&id)
                        .filter(|window| window.surface.is_none())
                        .map(|window| window.source.operation());
                    if self.close(app, id) {
                        if let Some(operation) = pending_operation {
                            let error = result.err().map_or_else(
                                || "the terminal closed".to_owned(),
                                |e| e.to_string(),
                            );
                            app.notify_warn(format!("Could not {operation}: {error}"));
                        }
                        changed = true;
                    }
                }
            }
        }

        if !self.windows.is_empty() {
            if !self.is_main() && app.exit_confirm_active {
                app.exit_confirm_active = false;
                changed |= self.close(app, self.id);
            }
            self.populated.clear();
            app.collect_populated_windows(&mut self.populated, &mut self.populated_desks);
            for owner in &mut app.workspaces.parked {
                if std::mem::take(&mut owner.app.tab_window_redraw_pending) {
                    self.invalidate_workspace(owner.id);
                }
                if owner.app.has_detached_windows() {
                    owner
                        .app
                        .collect_populated_windows(&mut self.populated, &mut self.populated_desks);
                }
            }
            let mut retired = std::mem::take(&mut self.retired);
            retired.extend(self.windows.iter().filter_map(|(&id, window)| {
                let still_open = if window.surface.is_some() {
                    self.populated.contains(&id)
                } else {
                    workspace(app, window.workspace)
                        .is_some_and(|owner| window.source.is_valid(owner))
                };
                (!still_open).then_some(id)
            }));
            for id in retired.drain(..) {
                changed |= self.close(app, id);
            }
            self.retired = retired;
        }

        let actual = if self.windows.is_empty() {
            MAIN_WINDOW
        } else {
            app.detached_window_id().unwrap_or(MAIN_WINDOW)
        };
        if actual != self.id {
            // Opening or navigating to a view can select its desk directly.
            // Route the resulting frame to the terminal that owns that desk.
            if actual == MAIN_WINDOW
                || self.windows.get(&actual).is_some_and(|window| {
                    window.workspace == app.workspaces.active && window.surface.is_some()
                })
            {
                self.select_window(actual);
                if self.is_main() {
                    self.main_workspace = app.workspaces.active;
                }
                self.focus(actual);
                changed = true;
            }
        }
        if changed {
            self.sync_main_observation(app);
        }
        changed
    }

    pub(crate) fn after_mouse(
        &mut self,
        app: &mut App,
        main: &TerminalEventPump,
        mouse: MouseEvent,
    ) {
        if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            self.drag = None;
        } else if mouse.kind != MouseEventKind::Drag(MouseButton::Left) || self.drag.is_some() {
            if mouse.kind == MouseEventKind::Up(MouseButton::Left) {
                self.drag = None;
            }
            return;
        }
        if let Some(tab) = crate::mouse_drag::dragged_editor_tab(app) {
            let known_host = if self.is_main() {
                self.main_host
            } else {
                self.windows
                    .get(&self.id)
                    .and_then(|window| window.surface.as_ref())
                    .and_then(Surface::host)
            };
            if let Some(press) = self.tracker(main).and_then(DesktopTracker::arm)
                && (known_host.is_none()
                    || known_host == Some(press.host)
                    || desktop::foreground_host() == Some(press.host))
            {
                // A real tab Down is proof this terminal routed the native
                // press, even if its top-level window was just moved/rehosted.
                if self.is_main() {
                    self.main_host = Some(press.host);
                } else if let Some(surface) = self.surface_mut(self.id) {
                    surface.host.store(press.host, Ordering::Release);
                }
                self.drag = Some(DesktopDrag {
                    source: self.id,
                    workspace: app.workspaces.active,
                    view: tab.editor.navigation_id,
                    press,
                });
            }
        }
    }

    fn tracker<'a>(&'a self, _main: &'a TerminalEventPump) -> Option<&'a DesktopTracker> {
        #[cfg(windows)]
        {
            _main.desktop_tracker()
        }
        #[cfg(target_os = "linux")]
        {
            self.tracker.as_deref()
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            None
        }
    }

    pub(crate) fn poll_drag(
        &mut self,
        app: &mut App,
        main: &TerminalEventPump,
        terminal_release: bool,
    ) -> bool {
        let Some(drag) = self.drag.as_ref() else {
            return false;
        };
        if drag.source != self.id
            || drag.workspace != app.workspaces.active
            || crate::mouse_drag::dragged_editor_tab(app).map(|tab| tab.editor.navigation_id)
                != Some(drag.view)
        {
            self.drag = None;
            return false;
        }
        let Some(tracker) = self.tracker(main) else {
            return false;
        };
        let mut release = tracker.release(drag.press);
        if terminal_release
            && matches!(release, desktop::Release::Waiting)
            && let Some(sample) = desktop::released_pointer()
        {
            tracker.observe(false, sample, &self.wake);
            release = tracker.release(drag.press);
        }
        if matches!(release, desktop::Release::Waiting) {
            return false;
        }
        let drag = self.drag.take().unwrap();
        let desktop::Release::Dropped(drop) = release else {
            return false;
        };
        app.clear_drag_states();
        app.drag_start_pos = None;
        app.drag_has_moved = false;
        let target = drop.host.and_then(|host| {
            if self.main_host == Some(host) {
                return Some(MAIN_WINDOW);
            }
            self.windows.iter().find_map(|(&id, window)| {
                window
                    .surface
                    .as_ref()
                    .is_some_and(|surface| surface.host() == Some(host))
                    .then_some(id)
            })
        });
        match target {
            Some(window)
                if window != MAIN_WINDOW
                    && self
                        .windows
                        .get(&window)
                        .is_some_and(|target| target.workspace != drag.workspace) =>
            {
                app.notify_warn("This terminal belongs to a different workspace");
                true
            }
            Some(window) => self.request(
                app,
                Request::Move {
                    view: drag.view,
                    window,
                },
            ),
            None => self.request(
                app,
                Request::Open {
                    view: drag.view,
                    point: Some(drop.point),
                },
            ),
        }
    }

    pub(crate) fn cancel_drag(&mut self, app: &mut App, key: crossterm::event::KeyEvent) {
        if key.code == crossterm::event::KeyCode::Esc
            && key.kind != KeyEventKind::Release
            && app.dragging_tab.is_some()
        {
            self.drag = None;
            app.clear_drag_states();
            app.drag_start_pos = None;
            app.drag_has_moved = false;
        }
    }
}

fn terminal_title(app: &App, id: WindowId) -> String {
    let name: String = app
        .window_name(id)
        .chars()
        .filter(|ch| !ch.is_control())
        .collect();
    format!("{name} — VTCode")
}
