//! Owns terminal connections and routes their input into one workbench.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
};

use crossterm::{
    event::{Event, KeyEventKind, MouseButton, MouseEvent, MouseEventKind},
    terminal::SetTitle,
};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport, layout::Rect};

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
    Open {
        view: u64,
        point: Option<ScreenPoint>,
    },
    Move {
        view: u64,
        window: WindowId,
    },
}

pub(crate) fn run_frontend(endpoint: OsString) -> io::Result<()> {
    frontend::run(endpoint)
}

type WindowTerminal = Terminal<CursorBackend<AnsiBackend<Vec<u8>>>>;

struct Surface {
    terminal: WindowTerminal,
    desired_area: Rect,
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

    fn background_ready(&self, focused: bool) -> bool {
        // The first frame must reach a new terminal even if its focus report
        // arrives after registration.
        self.dirty && self.ready() && (focused || self.title.is_empty())
    }

    fn resize(&mut self, width: u16, height: u16) {
        if let Ok(area) = screen_area((width, height)) {
            self.desired_area = area;
            self.dirty = true;
        }
    }

    fn paint(&mut self, title: String, draw: impl FnOnce(&mut Frame)) -> io::Result<()> {
        if !self.ready() {
            self.dirty = true;
            return Ok(());
        }
        if self.terminal.get_frame().area() != self.desired_area {
            self.terminal.resize(self.desired_area)?;
        }
        if self.title != title {
            queue_ansi(self.terminal.backend_mut(), SetTitle(&title))?;
            self.title = title;
        }
        self.terminal.draw(draw)?;
        let bytes = self.terminal.backend_mut().take_output();
        if !bytes.is_empty() {
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
        (value != 0 && desktop::valid_host(value)).then_some(value)
    }
}

struct Window {
    workspace: u64,
    source_window: WindowId,
    source_view: u64,
    surface: Option<Surface>,
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
    next_window: WindowId,
    last_background_window: WindowId,
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
            next_window: 1,
            last_background_window: MAIN_WINDOW,
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

    pub(crate) fn background_draw_ready(&self, focused: bool) -> bool {
        (!self.is_main() && self.main_dirty && focused)
            || self.windows.iter().any(|(&id, window)| {
                id != self.id
                    && window
                        .surface
                        .as_ref()
                        .is_some_and(|surface| surface.background_ready(focused))
            })
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
        if self.is_main() || !self.main_dirty || !app.terminal_has_focus {
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
        let focused = app.terminal_has_focus;
        let mut ready: Vec<_> = self
            .windows
            .iter()
            .filter_map(|(&id, window)| {
                (id != self.id
                    && window
                        .surface
                        .as_ref()
                        .is_some_and(|surface| surface.background_ready(focused)))
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
        if let Some(event) = main.try_recv()? {
            return Ok(Some((MAIN_WINDOW, event)));
        }
        for (&id, window) in &mut self.windows {
            let Some(surface) = &mut window.surface else {
                continue;
            };
            match surface.events.try_recv() {
                Ok(Some(event @ Event::Resize(width, height))) if id != self.id => {
                    surface.resize(width, height);
                    return Ok(Some((id, event)));
                }
                Ok(Some(event)) => return Ok(Some((id, event))),
                Ok(None) => {}
                Err(_) => {
                    // The transport may close between the reader's last packet
                    // and its close notice. Wake an idle workbench for cleanup.
                    self.wake.notify();
                }
            }
        }
        Ok(None)
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
            return true;
        }
        if !claims_focus || transfer_blocked(app) {
            return false;
        }
        self.activate(app, source)
    }

    fn activate(&mut self, app: &mut App, source: WindowId) -> bool {
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
        self.id = source;
        self.sync_main_observation(app);
        self.invalidate_background();
        app.terminal_has_focus = true;
        app.forward_terminal_focus(true);
        app.idle_quiescent = false;
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
        if let Some(owner) = workspace_mut(app, window.workspace) {
            owner.return_detached_tabs(id);
        }
        if id == self.id {
            self.activate(app, MAIN_WINDOW);
        }
        true
    }

    fn attach(&mut self, app: &mut App, id: WindowId, ready: frontend::Ready) -> io::Result<()> {
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
        let Some(window) = self.windows.get_mut(&id) else {
            return Ok(());
        };
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
        if owner.tab_window(window.source_view) != Some(window.source_window)
            || !owner.open_detached_window(id, window.source_view)
        {
            return Err(io::Error::other(
                "the tab was closed or moved before the terminal opened",
            ));
        }
        window.surface = Some(Surface {
            terminal,
            desired_area: area,
            _connection: ready.connection,
            output: ready.output,
            events: TerminalEventPump::from_receiver(ready.inputs),
            host: ready.host,
            dirty: true,
            title: String::new(),
        });
        Ok(())
    }

    fn request(&mut self, app: &mut App, request: Request) -> bool {
        if transfer_blocked(app) {
            return false;
        }
        match request {
            Request::Open { view, point } => {
                let Some(source_window) = app.tab_window(view) else {
                    return false;
                };
                if self.windows.values().any(|window| {
                    window.surface.is_none()
                        && window.workspace == app.workspaces.active
                        && window.source_view == view
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
                    Ok(()) => {
                        self.windows.insert(
                            id,
                            Window {
                                workspace: app.workspaces.active,
                                source_window,
                                source_view: view,
                                surface: None,
                            },
                        );
                    }
                    Err(error) => {
                        app.notify_warn(format!("Could not open a detached terminal: {error}"))
                    }
                }
                true
            }
            Request::Move { view, window } => {
                if app.move_tab_to_window(view, window) {
                    if window == MAIN_WINDOW {
                        self.main_workspace = app.workspaces.active;
                        self.activate(app, MAIN_WINDOW);
                    }
                    self.focus(window);
                    self.invalidate_background();
                    true
                } else {
                    false
                }
            }
        }
    }

    pub(crate) fn poll(&mut self, app: &mut App) -> bool {
        if self.is_main() {
            self.main_workspace = app.workspaces.active;
        }
        if std::mem::take(&mut app.tab_window_document_changed) {
            self.invalidate_background();
        }
        let mut changed = app
            .tab_window_request
            .take()
            .is_some_and(|request| self.request(app, request));
        while let Ok((id, notice)) = self.incoming.try_recv() {
            match notice {
                frontend::Notice::Ready(ready) => {
                    if let Err(error) = self.attach(app, id, ready) {
                        self.close(app, id);
                        app.notify_warn(format!("Could not detach tab: {error}"));
                    }
                    changed = true;
                }
                frontend::Notice::Closed(result) => {
                    let pending = self
                        .windows
                        .get(&id)
                        .is_some_and(|window| window.surface.is_none());
                    if self.close(app, id) {
                        if pending {
                            let error = result.err().map_or_else(
                                || "the terminal closed".to_owned(),
                                |e| e.to_string(),
                            );
                            app.notify_warn(format!("Could not detach tab: {error}"));
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
            let retired: Vec<_> = self
                .windows
                .iter()
                .filter_map(|(&id, window)| {
                    let owner = workspace(app, window.workspace);
                    (owner.is_none_or(|owner| {
                        window.surface.is_some() && owner.window_tabs(id).next().is_none()
                    }))
                    .then_some(id)
                })
                .collect();
            for id in retired {
                changed |= self.close(app, id);
            }
        }

        let actual = app.detached_window_id().unwrap_or(MAIN_WINDOW);
        if actual != self.id {
            // Opening or navigating to a view can select its desk directly.
            // Route the resulting frame to the terminal that owns that desk.
            if actual == MAIN_WINDOW
                || self.windows.get(&actual).is_some_and(|window| {
                    window.workspace == app.workspaces.active && window.surface.is_some()
                })
            {
                self.id = actual;
                self.focus(actual);
                changed = true;
            }
        }
        if changed {
            self.sync_main_observation(app);
            self.invalidate_background();
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
