use std::io;
use std::path::PathBuf;
use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
};
use ratatui::{backend::Backend, Terminal};
#[cfg(test)]
use ratatui::backend::CrosstermBackend;

use crate::app::{ActivePane, App, TabKind};
use crate::input_keyboard;
use crate::mouse;
use crate::mouse_overlay;
use crate::ui;

#[path = "editor_content_analysis.rs"]
pub(crate) mod content_analysis;

#[cfg(test)]
#[path = "app_blame_tests.rs"]
mod blame_tests;

#[cfg(test)]
#[path = "app_input_performance_tests.rs"]
mod input_performance_tests;

#[cfg(test)]
#[path = "app_input_load_tests.rs"]
mod input_load_tests;

#[cfg(test)]
#[path = "mouse_selection_latency_tests.rs"]
mod mouse_selection_latency_tests;

#[cfg(test)]
#[path = "mouse_block_selection_latency_tests.rs"]
mod mouse_block_selection_latency_tests;

#[cfg(test)]
#[path = "app_responsiveness_tests.rs"]
mod responsiveness_tests;

#[cfg(all(test, windows))]
#[path = "windows_pointer_pipeline_tests.rs"]
mod windows_pointer_pipeline_tests;

#[cfg(all(test, windows))]
#[path = "windows_pointer_stream_tests.rs"]
mod windows_pointer_stream_tests;

#[cfg(test)]
#[path = "pointer_event_loop_latency_tests.rs"]
mod pointer_event_loop_latency_tests;

#[cfg(test)]
#[path = "word_occurrence_demand_tests.rs"]
mod word_occurrence_demand_tests;

#[cfg(test)]
#[path = "app_pair_work_tests.rs"]
mod pair_work_tests;

#[cfg(test)]
#[path = "app_single_core_tests.rs"]
mod single_core_tests;

#[cfg(test)]
#[path = "app_architecture_tests.rs"]
mod architecture_tests;

#[cfg(test)]
thread_local! {
    static REFERENCE_INPUT_BATCHES: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn reference_input_batches() -> bool {
    #[cfg(test)]
    { return REFERENCE_INPUT_BATCHES.get(); }
    #[cfg(not(test))]
    { false }
}

#[cfg(test)]
#[path = "app_polling_performance_tests.rs"]
mod polling_performance_tests;

/// Whole-document metrics and unsaved diffs are useful after an edit settles,
/// but restarting them between keystrokes only competes with the UI and syntax
/// workers. Viewport-scoped feedback continues updating immediately.
const EDITOR_ANALYSIS_EDIT_QUIET_PERIOD: Duration = Duration::from_millis(200);
const UNWATCHED_DISK_CHECK_INTERVAL: Duration = Duration::from_secs(2);
const CURSOR_BLINK_INTERVAL: Duration = Duration::from_millis(500);
/// Stop purely cosmetic timers after the user has genuinely walked away. One
/// deadline transitions the workbench into quiescence; there is no idle tick.
const FOREGROUND_IDLE_QUIET_PERIOD: Duration = Duration::from_secs(5);
const WORD_OCCURRENCE_QUIET_PERIOD: Duration = Duration::from_millis(150);
const CHORD_TIMEOUT: Duration = Duration::from_secs(2);
const SCREENCAST_LIFETIME: Duration = Duration::from_secs(3);
const TAB_TOOLTIP_DELAY: Duration = Duration::from_millis(500);
const SAVING_FLASH_LIFETIME: Duration = Duration::from_millis(1_500);

fn background_chat_redraw(app: &App, was_running: bool, changed: bool) -> bool {
    app.terminal_has_focus || was_running || changed
        || app.chat.any_agent_running()
        || app.chat.rollback.busy()
}

fn open_disk_sources(app: &App) -> impl Iterator<Item = (&std::path::Path, Option<&std::sync::Arc<crate::remote::Checkpoint>>)> {
    app.tabs
        .iter()
        .chain(app.groups.iter().flat_map(|group| group.tabs.iter()))
        .filter(|tab| !matches!(tab.kind, TabKind::Diff(_) | TabKind::GitRevision | TabKind::CommitDetails))
        .filter_map(|tab| Some((tab.editor.file_path.as_deref()?, tab.editor.remote_checkpoint.as_ref())))
}

#[derive(Default)]
struct PeriodicDiskPaths {
    root: Option<PathBuf>,
    watcher_active: bool,
    remote: bool,
    sources: Vec<(PathBuf, Option<std::sync::Arc<crate::remote::Checkpoint>>)>,
    paths: Vec<PathBuf>,
    #[cfg(test)]
    rebuilds: usize,
}

impl PeriodicDiskPaths {
    fn get(&mut self, app: &App) -> &[PathBuf] {
        // Check borrowed path spellings without cloning, hashing or rechecking
        // watcher exclusions. Editing, scrolling and animations do not change
        // this inventory; renames, tab/group changes and watcher coverage do.
        if self.root.as_ref().map(|root| root.as_os_str()) != Some(app.explorer.root.as_os_str())
            || self.watcher_active != app.workspace_watcher_active
            || self.remote != app.remote_workspace.is_some()
            || !open_disk_sources(app)
                .map(|(path, checkpoint)| (path.as_os_str(), checkpoint.filter(|_| self.remote).map(std::sync::Arc::as_ptr)))
                .eq(self.sources.iter().map(|(path, checkpoint)| (path.as_os_str(), checkpoint.as_ref().map(std::sync::Arc::as_ptr))))
        {
            self.root = Some(app.explorer.root.clone());
            self.watcher_active = app.workspace_watcher_active;
            self.remote = app.remote_workspace.is_some();
            self.sources = open_disk_sources(app).map(|(path, checkpoint)| (path.to_owned(), checkpoint.filter(|_| self.remote).cloned())).collect();
            self.paths.clear();
            let mut seen = rustc_hash::FxHashSet::default();
            for (path, checkpoint) in &self.sources {
                if !seen.insert(path) { continue; }
                let watched = self.watcher_active && app.remote_workspace.as_ref().map_or_else(
                    || crate::background::workspace_path_is_watched(&app.explorer.root, path),
                    |remote| remote.save_is_watched(path, checkpoint.as_deref()),
                );
                if !watched {
                    self.paths.push(path.clone());
                }
            }
            #[cfg(test)]
            {
                self.rebuilds += 1;
            }
        }
        &self.paths
    }
}

/// Crossterm owns a blocking console read. Keep it on one dedicated thread so
/// the UI can wait on the same condition variable as background and LSP work
/// instead of waking at a fixed rate merely to ask every queue whether it has
/// changed.
pub(crate) struct TerminalEventPump {
    #[cfg(windows)]
    receiver: crate::terminal_input_batch::Receiver,
    #[cfg(not(windows))]
    receiver: std::sync::mpsc::Receiver<io::Result<Event>>,
    pending: Option<io::Result<Event>>,
    startup_input: std::collections::VecDeque<Event>,
    #[cfg(windows)]
    windows_mouse: Option<crate::windows_mouse::WindowsMouseInput>,
}

impl TerminalEventPump {
    // Bound queue draining so a continuous input stream cannot starve drawing
    // or background work. This is an event budget, not a frame-rate limit.
    const MAX_MOVE_BATCH: usize = 1024;

    pub(crate) fn start(wake: crate::background::UiWake) -> io::Result<Self> {
        #[cfg(windows)]
        let (sender, receiver) = crate::terminal_input_batch::channel();
        #[cfg(windows)]
        crate::terminal_input_batch::reset_delivery_timings();
        #[cfg(not(windows))]
        let (sender, receiver) = std::sync::mpsc::channel();
        #[cfg(windows)]
        // Nested terminals receive coordinates from their parent. A native
        // hook would capture the outer Windows Terminal window a second time.
        let windows_mouse = if std::env::var_os("TERM_PROGRAM").as_deref()
            == Some(std::ffi::OsStr::new("vtcode"))
        {
            None
        } else {
            crate::windows_mouse::WindowsMouseInput::start(sender.clone(), wake.clone())
                .map(Some)
                .unwrap_or_else(|error| {
                    log_warn!("Windows Terminal mouse input unavailable: {error}");
                    None
                })
        };
        #[cfg(windows)]
        let mouse_focus = windows_mouse.as_ref().map(|input| input.focus.clone());
        std::thread::Builder::new()
            .name("terminal-input".to_string())
            .spawn(move || {
                // This thread only decodes input and sleeps in the OS when
                // idle. Let it deliver fresh events ahead of CPU-bound work;
                // boosting only the renderer cannot repair a starved reader.
                let _input_priority = crate::worker_priority::InputPriority::new(true);
                loop {
                    let event = event::read();
                    #[cfg(windows)]
                    let running = crate::terminal_input_batch::forward(event, &sender, &wake, |event| {
                        let Some(focus) = &mouse_focus else { return false; };
                        match event {
                            Ok(Event::FocusGained) => focus.set_focused(true),
                            Ok(Event::FocusLost) => focus.set_focused(false),
                            Ok(Event::Resize(..)) => focus.resized(),
                            Ok(Event::Key(_) | Event::Paste(_)) => focus.set_focused(true),
                            // Routed hover/wheel input also establishes pane
                            // ownership before the first Shift gesture.
                            Ok(Event::Mouse(_)) => focus.set_focused(true),
                            _ => {}
                        }
                        matches!(event, Ok(Event::Mouse(mouse)) if focus.stage_native_mouse(mouse))
                    }, || {
                        if event::poll(Duration::ZERO)? { event::read().map(Some) } else { Ok(None) }
                    }, || {
                        // Native Shift gestures may follow a stamped terminal
                        // press immediately; the queue still delivers its Down
                        // before those gestures, even before batch publication.
                        mouse_focus.as_ref().and_then(|focus| focus.order_native_mouse())
                    });
                    #[cfg(not(windows))]
                    let running = {
                        let failed = event.is_err();
                        if sender.send(event).is_err() {
                            break;
                        }
                        wake.notify();
                        !failed
                    };
                    if !running { break; }
                }
            })
            .map_err(io::Error::other)?;
        Ok(Self {
            receiver, pending: None, startup_input: Default::default(),
            #[cfg(windows)]
            windows_mouse,
        })
    }

    #[cfg(test)]
    pub(crate) fn from_receiver(receiver: std::sync::mpsc::Receiver<io::Result<Event>>) -> Self {
        #[cfg(windows)]
        let receiver = receiver.into();
        Self {
            receiver, pending: None, startup_input: Default::default(),
            #[cfg(windows)]
            windows_mouse: None,
        }
    }

    /// Deliver keys typed during startup before input received afterward.
    pub(crate) fn replay_startup_input(&mut self, events: std::collections::VecDeque<Event>) {
        debug_assert!(self.startup_input.is_empty());
        self.startup_input = events;
    }

    /// A cosmetic deadline yields to already queued input. Keep the event in
    /// the same slot used by batching, preserving order and input errors.
    pub(crate) fn has_pending_event(&mut self) -> bool {
        if !self.startup_input.is_empty() { return true; }
        if self.pending.is_none() {
            self.pending = self.receiver.try_recv().ok();
        }
        self.pending.is_some()
    }

    pub(crate) fn try_recv(&mut self) -> io::Result<Option<Event>> {
        let Some(mut event) = self.take_next()? else {
            return Ok(None);
        };
        if let Event::Mouse(mouse) = event
            && mouse.kind == crossterm::event::MouseEventKind::Moved
        {
            // Only the newest queued hover position needs a frame. Never wait
            // for another move, or merge across clicks, drags, scroll, keyboard,
            // focus/resize events, modifier changes, or input errors.
            for _ in 1..Self::MAX_MOVE_BATCH {
                match self.receiver.try_recv() {
                    Ok(Ok(Event::Mouse(next)))
                        if next.kind == crossterm::event::MouseEventKind::Moved
                            && next.modifiers == mouse.modifiers =>
                    {
                        event = Event::Mouse(next);
                    }
                    Ok(next) => {
                        self.pending = Some(next);
                        break;
                    }
                    Err(_) => break,
                }
            }
        }
        if matches!(event, Event::Resize(..)) {
            // A terminal/window resize burst has one final geometry. Preserve
            // input boundaries while avoiding full layouts for stale sizes.
            for _ in 1..Self::MAX_MOVE_BATCH {
                match self.receiver.try_recv() {
                    Ok(Ok(next @ Event::Resize(..))) => event = next,
                    Ok(next) => { self.pending = Some(next); break; }
                    Err(_) => break,
                }
            }
        }
        Ok(Some(event))
    }

    fn take_next(&mut self) -> io::Result<Option<Event>> {
        if let Some(event) = self.startup_input.pop_front() { return Ok(Some(event)); }
        if let Some(event) = self.pending.take() {
            return event.map(Some);
        }
        match self.receiver.try_recv() {
            Ok(result) => result.map(Some),
            Err(std::sync::mpsc::TryRecvError::Empty) => Ok(None),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "terminal input reader stopped",
            )),
        }
    }

    /// Consume only an already queued, identical wheel event. Keep every other
    /// event (including errors) in order for the next event-loop iteration.
    fn next_matching_scroll(&mut self, mouse: crossterm::event::MouseEvent) -> bool {
        let next = self.pending.take().or_else(|| self.receiver.try_recv().ok());
        match next {
            Some(Ok(Event::Mouse(next))) if next == mouse => true,
            other => {
                self.pending = other;
                false
            }
        }
    }

    fn next_matching_drag(
        &mut self,
        mouse: crossterm::event::MouseEvent,
    ) -> Option<crossterm::event::MouseEvent> {
        self.next_matching_mouse(|next| next.kind == mouse.kind && next.modifiers == mouse.modifiers)
    }

    fn next_matching_mouse(
        &mut self,
        matches: impl FnOnce(crossterm::event::MouseEvent) -> bool,
    ) -> Option<crossterm::event::MouseEvent> {
        // Startup replay precedes both a previously inspected event and new
        // terminal input. Peek at its boundary instead of moving it past either.
        if let Some(event) = self.startup_input.front() {
            let Event::Mouse(mouse) = event else { return None; };
            let mouse = *mouse;
            if !matches(mouse) { return None; }
            self.startup_input.pop_front();
            return Some(mouse);
        }
        let next = self.pending.take().or_else(|| self.receiver.try_recv().ok());
        match next {
            Some(Ok(Event::Mouse(next))) if matches(next) => Some(next),
            other => {
                self.pending = other;
                None
            }
        }
    }

    /// Interior character selection depends on the latest endpoint within the
    /// viewport. Consume only queued samples; outside rows retain every
    /// auto-scroll step, and all other input keeps its original ordering.
    fn latest_selection_drag(
        &mut self,
        mut mouse: crossterm::event::MouseEvent,
        area: ratatui::layout::Rect,
        limit: usize,
    ) -> (crossterm::event::MouseEvent, usize) {
        let mut consumed = 1;
        while consumed < limit {
            let Some(next) = self.next_matching_mouse(|next|
                next.kind == mouse.kind && next.modifiers == mouse.modifiers
                    && area.contains((next.column, next.row).into()))
            else { break; };
            mouse = next;
            consumed += 1;
        }
        (mouse, consumed)
    }

    fn next_matching_key(&mut self, key: crossterm::event::KeyEvent) -> bool {
        let next = self.pending.take().or_else(|| self.receiver.try_recv().ok());
        match next {
            Some(Ok(Event::Key(next))) if next == key => true,
            other => {
                self.pending = other;
                false
            }
        }
    }

    fn next_navigation_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> Option<crossterm::event::KeyEvent> {
        let next = self.pending.take().or_else(|| self.receiver.try_recv().ok());
        match next {
            Some(Ok(Event::Key(next)))
                if is_navigation_key(next)
                    && next.modifiers == key.modifiers
                    && next.state == key.state => Some(next),
            other => { self.pending = other; None }
        }
    }

    fn next_editor_scroll(
        &mut self,
        mouse: crossterm::event::MouseEvent,
        area: ratatui::layout::Rect,
    ) -> Option<crossterm::event::MouseEvent> {
        let next = self.pending.take().or_else(|| self.receiver.try_recv().ok());
        match next {
            Some(Ok(Event::Mouse(next)))
                if next.kind == mouse.kind && next.modifiers == mouse.modifiers
                    && area.contains((next.column, next.row).into()) => Some(next),
            other => { self.pending = other; None }
        }
    }

    fn next_editing_key(
        &mut self,
        key: crossterm::event::KeyEvent,
    ) -> Option<crossterm::event::KeyEvent> {
        let next = self
            .pending
            .take()
            .or_else(|| self.receiver.try_recv().ok());
        match next {
            Some(Ok(Event::Key(next)))
                if next.modifiers == key.modifiers
                    && next.kind == key.kind
                    && next.state == key.state
                    && is_plain_editing_key(next) =>
            {
                Some(next)
            }
            other => {
                self.pending = other;
                None
            }
        }
    }

    /// Layout dragging needs the newest already queued position, not a frame
    /// for every historical pointer sample. Keep gesture and input boundaries.
    fn latest_split_drag(&mut self, mut mouse: crossterm::event::MouseEvent) -> crossterm::event::MouseEvent {
        use crossterm::event::{MouseButton, MouseEventKind};
        if mouse.kind != MouseEventKind::Drag(MouseButton::Left) {
            return mouse;
        }
        for _ in 1..Self::MAX_MOVE_BATCH {
            let next = self.pending.take().or_else(|| self.receiver.try_recv().ok());
            match next {
                Some(Ok(Event::Mouse(next))) if next.kind == mouse.kind && next.modifiers == mouse.modifiers => {
                    mouse = next;
                }
                other => {
                    self.pending = other;
                    break;
                }
            }
        }
        mouse
    }
}

const MAX_SCROLL_BATCH: usize = 64;
const SCROLL_INPUT_BUDGET: Duration = Duration::from_millis(4);
const MAX_NAVIGATION_BATCH: usize = 64;
const NAVIGATION_INPUT_BUDGET: Duration = Duration::from_millis(4);

fn is_navigation_key(key: crossterm::event::KeyEvent) -> bool {
    matches!(key.code,
        KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down
            | KeyCode::Home | KeyCode::End | KeyCode::PageUp | KeyCode::PageDown
    ) && key.kind != KeyEventKind::Release
        && !key.modifiers.intersects(
            KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META,
        )
}

fn is_plain_editing_key(key: crossterm::event::KeyEvent) -> bool {
    if key.kind == KeyEventKind::Release
        || key.modifiers.intersects(
            KeyModifiers::CONTROL
                | KeyModifiers::ALT
                | KeyModifiers::SUPER
                | KeyModifiers::HYPER
                | KeyModifiers::META,
        )
    {
        return false;
    }
    match key.code {
        KeyCode::Char(ch) => !ch.is_control(),
        KeyCode::Backspace | KeyCode::Delete => key.modifiers.is_empty(),
        KeyCode::Enter | KeyCode::Tab => key.modifiers.is_empty() && !reference_input_batches(),
        _ => false,
    }
}

fn editor_batch_target(app: &App) -> Option<(usize, usize, u64)> {
    if app.active_pane != ActivePane::Editor
        || app.docked_terminal_has_focus()
        || app.change_dir_active
        || app.menu_open
        || app.chord_pending
        || app.tab_switcher_active
        || app.tab_context_active
        || app.context_menu_active
        || app.status_details.open
        || app.completion_index_ui.open
        || crate::ui_performance_details::captures_keyboard(app)
        || app.lsp_code_action_active
        || app.call_hierarchy_active
        || app.type_hierarchy_active
        || app.custom_exit_remaining > 0
        || app.focused_text_input_id().is_some()
        || crate::ui_modal::active(app).is_some()
        || crate::ui_quick_picker::active(app).is_some()
    {
        return None;
    }
    let tab = app.tabs.get(app.active_tab)?;
    (tab.kind == crate::app::TabKind::File && tab.extension_view.is_none() && !tab.editor.bg_loading)
        .then_some((app.active_group, app.active_tab, tab.editor.navigation_id))
}

fn editing_batch_target(app: &App) -> Option<(usize, usize, u64)> {
    editor_batch_target(app).filter(|_| app.editor().is_some_and(|editor| !editor.read_only))
}

fn navigation_batch_target(app: &App) -> Option<(usize, usize, u64)> {
    editor_batch_target(app).filter(|_| !app.vtcomplete_active && !app.lsp_completion_active)
}

/// Route a paste through the focused surface before considering the code
/// editor. A tool panel or its context menu never implies editor text focus.
#[cfg(test)]
pub(crate) fn handle_paste_input(app: &mut App, text: &str) {
    handle_paste_input_value(app, std::borrow::Cow::Borrowed(text));
}

fn handle_paste_input_value(app: &mut App, value: std::borrow::Cow<'_, str>) {
    let _foreground = app.vtcomplete_worker.activity().enter();
    if app.completion_index_ui.open {
        crate::ui_vtcomplete_index::handle_paste(app, value.as_ref());
        return;
    }
    if !app.accept_terminal_paste() { return; }
    let text = value.as_ref();
    if crate::ui_remote_connection::handle_paste(app, text) { return; }
    crate::clipboard::receive_terminal_paste(text);
    if crate::ui_desks::paste(app, text) || crate::ui_logs::paste(app, text) { return; }
    app.dismiss_lsp_hover();
    if crate::ui_commit_details::focused(app) {
        if app.commit_details.focus == crate::commit_details::Focus::Filter {
            app.commit_details.edit_filter(crossterm::event::KeyCode::Null, crossterm::event::KeyModifiers::NONE, Some(text));
        } else { crate::ui_commit_details::paste_context_count(app, text); }
        return;
    }
    if app.change_dir_active || crate::ui_modal::active(app).is_some() || crate::ui_quick_picker::active(app).is_some() {
        app.paste_into_focused_text_input(text);
        return;
    }
    if let Some(index) = app.focused_terminal_index() {
        app.paste_terminal_text(index, text);
        return;
    }
    relight_cursor(app);
    if app.paste_into_focused_text_input(text) || app.paste_explorer_clipboard_text(text)
        || app.active_pane != crate::app::ActivePane::Editor { return; }
    if app.paste_into_extension_editor(text) || app.diff_selection_owns_cursor() { return; }
    app.paste_editor_text(value.into_owned(), true);
}

/// Dispatch queued navigation repeats or ordinary editor edits before painting.
/// Every key still takes the normal command/undo/completion/macro path. Never
/// wait for more input or cross a modifier, control, pane, or document boundary.
/// The event and time budgets keep painting and workers responsive under load.
pub(crate) fn handle_keyboard_input(
    app: &mut App,
    pump: &mut TerminalEventPump,
    mut key: crossterm::event::KeyEvent,
) -> bool {
    let _foreground = app.vtcomplete_worker.activity().enter();
    let _input_priority = crate::worker_priority::InputPriority::new(
        app.terminal_has_focus && key.kind != crossterm::event::KeyEventKind::Release,
    );
    let batchable = is_navigation_key(key);
    let navigation_target = (batchable && !reference_input_batches())
        .then(|| navigation_batch_target(app)).flatten();
    let editing_target = is_plain_editing_key(key)
        .then(|| editing_batch_target(app))
        .flatten();
    let started = std::time::Instant::now();
    if !reference_input_batches() { app.begin_vtcomplete_batch(); }
    let mut quit = false;
    for processed in 1..=MAX_NAVIGATION_BATCH {
        note_user_activity(app);
        if app.macro_recording {
            let is_macro_toggle = key.modifiers.contains(KeyModifiers::CONTROL)
                && key.modifiers.contains(KeyModifiers::SHIFT)
                && matches!(key.code, KeyCode::Char('R' | 'r'));
            if !is_macro_toggle {
                app.macro_buffer.push(key);
            }
        }
        if app.screencast_mode {
            app.screencast_keys.push_back((key, std::time::Instant::now()));
            while app.screencast_keys.len() > 5 {
                app.screencast_keys.pop_front();
            }
        }
        if input_keyboard::handle_keyboard_event(app, key) {
            quit = true;
            break;
        }
        if !app.macro_replay_queue.is_empty()
            || processed == MAX_NAVIGATION_BATCH
            || started.elapsed() >= NAVIGATION_INPUT_BUDGET
        {
            break;
        }
        if navigation_target.is_some() {
            if navigation_target != navigation_batch_target(app) {
                break;
            }
            let Some(next) = pump.next_navigation_key(key) else { break; };
            key = next;
        } else if batchable {
            if !pump.next_matching_key(key) {
                break;
            }
        } else if editing_target.is_some() && editing_target == editing_batch_target(app) {
            let Some(next) = pump.next_editing_key(key) else {
                break;
            };
            key = next;
        } else {
            break;
        }
    }
    if !reference_input_batches() { app.finish_vtcomplete_batch(); }
    quit
}

/// Apply wheel deltas and editor selection before rendering their combined
/// result. Interior character selection can use the latest queued endpoint in
/// the viewport; stateful gestures retain their ordered samples.
/// Both limits guarantee drawing and other work get a turn under continuous input.
pub(crate) fn handle_mouse_input(
    app: &mut App,
    pump: &mut TerminalEventPump,
    mouse: crossterm::event::MouseEvent,
) -> bool {
    let _foreground = app.vtcomplete_worker.activity().enter();
    use crossterm::event::MouseEventKind;
    let _input_priority = crate::worker_priority::InputPriority::new(
        app.terminal_has_focus && mouse.kind != MouseEventKind::Moved,
    );
    let resizing_layout = app.resizing_split || app.resizing_explorer || app.resizing_panel
        || app.resizing_diff_split || app.resizing_sc_graph || app.chat.resizing;
    let mut mouse = if resizing_layout { pump.latest_split_drag(mouse) } else { mouse };
    let is_move = mouse.kind == MouseEventKind::Moved;
    let is_scroll = matches!(
        mouse.kind,
        MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight
    );
    // The full mouse route still runs for every sample. Only share painting
    // across position jitter within one ordinary editor; never cross chrome,
    // popup, terminal, extension, direction or modifier boundaries.
    let scroll_area = (is_scroll && !reference_input_batches())
        .then(|| editor_scroll_batch_area(app, mouse)).flatten();
    // Out-of-viewport samples each contribute an auto-scroll step, and column,
    // word and line selections have gesture state that must survive direction
    // changes. Their samples, extension streams and terminal streams stay intact.
    let is_editor_drag = (mouse.kind == MouseEventKind::Drag(crossterm::event::MouseButton::Left)
            || (mouse.kind == MouseEventKind::Drag(crossterm::event::MouseButton::Middle)
                && app.middle_drag_start.is_some()))
        && app.active_pane == ActivePane::Editor
        && !app.resizing_split
        && !app.resizing_diff_split
        && !app.resizing_explorer
        && !app.resizing_panel
        && !app.resizing_sc_graph
        && app.dragging_tab.is_none()
        && app.dragging_terminal_tab.is_none()
        && app.dragging_explorer_path.is_none()
        && !app.dragging_text
        && !app.context_menu_active
        && crate::ui_modal::active(app).is_none()
        && app.tabs.get(app.active_tab).is_some_and(|tab| {
            tab.kind == crate::app::TabKind::File && tab.extension_view.is_none()
        });
    let batch_selection = is_editor_drag && !reference_input_batches();
    let selection_before = batch_selection
        .then(|| editor_selection_drag_state(app, mouse)).flatten();
    let mut selection_area = selection_before.as_ref().map(|state| state.area);
    let mut selection_budget = TerminalEventPump::MAX_MOVE_BATCH;
    let pos_changed = mouse.column != app.mouse_col || mouse.row != app.mouse_row;
    let modifiers_changed = app.lsp_link_hover.modifiers != mouse.modifiers;
    let layout_before = (resizing_layout && mouse.kind == MouseEventKind::Drag(crossterm::event::MouseButton::Left))
        .then(|| layout_drag_state(app));
    if app.active_pane == ActivePane::Editor && !is_move {
        relight_cursor(app);
    }
    let started = std::time::Instant::now();
    for processed in 1..=MAX_SCROLL_BATCH {
        if let Some(area) = selection_area {
            let (latest, consumed) = pump.latest_selection_drag(mouse, area, selection_budget);
            mouse = latest;
            selection_budget -= consumed;
        } else {
            selection_budget -= 1;
        }
        app.mouse_hover_changed = true;
        if app.completion_index_ui.open {
            mouse::handle_mouse(app, mouse);
            break;
        } else if mouse::handle_navigation_mouse(app, mouse) {
            break;
        } else if app.context_menu_active {
            // A panel press keeps its release even if a context menu opens.
            if !crate::ui_panels::captured_mouse(app, mouse) {
                mouse_overlay::handle_context_menu_mouse(app, mouse);
            }
        } else {
            mouse::handle_mouse(app, mouse);
        }
        if processed == MAX_SCROLL_BATCH || selection_budget == 0
            || started.elapsed() >= SCROLL_INPUT_BUDGET {
            break;
        }
        if is_scroll {
            if let Some(area) = scroll_area {
                if editor_scroll_batch_area(app, mouse) != Some(area) { break; }
                let Some(next) = pump.next_editor_scroll(mouse, area) else { break; };
                mouse = next;
            } else if !pump.next_matching_scroll(mouse) {
                break;
            }
        } else if is_editor_drag {
            if let Some(next) = pump.next_matching_drag(mouse) {
                mouse = next;
                selection_area = batch_selection
                    .then(|| editor_selection_drag_state(app, mouse)).flatten().map(|state| state.area);
            } else {
                break;
            }
        } else {
            break;
        }
    }
    if !modifiers_changed && let Some(before) = layout_before
        && layout_drag_state(app) == before
    {
        return false;
    }
    if !modifiers_changed && let Some(before) = selection_before
        && editor_selection_drag_state(app, mouse) == Some(before)
    {
        // Terminal reports can repeat within one cell. The normal route still
        // applies undo boundaries and validates the current editor position;
        // repaint only when its visible result (including caret blink) changed.
        return false;
    }
    !is_move || modifiers_changed || (pos_changed && app.mouse_hover_changed)
}

/// Duplicate terminal-cell samples still validate the layout through the
/// normal route, but do not repaint an unchanged pane. A split ratio is
/// delta-based: an unchanged pointer cannot adjust it. All absolute resize
/// values and modes are sampled, including maximized/collapsed transitions.
#[derive(PartialEq, Eq)]
struct LayoutDragState {
    pointer: (u16, u16),
    cursor_visible: bool,
    explorer: u16,
    panel: (u16, bool),
    diff: u32,
    graph: (u16, bool),
    chat: u16,
}

fn layout_drag_state(app: &App) -> LayoutDragState {
    LayoutDragState {
        pointer: (app.mouse_col, app.mouse_row),
        cursor_visible: app.cursor_visible,
        explorer: app.explorer_width,
        panel: (app.panel_height, app.panel_chrome.maximized),
        diff: app.diff_split_ratio.to_bits(),
        graph: (app.sc_graph_height, app.sc_graph_collapsed),
        chat: app.chat.width,
    }
}

#[derive(PartialEq, Eq)]
struct SelectionDragState {
    area: ratatui::layout::Rect,
    target: (usize, usize, u64),
    content_version: u64,
    cursor: (usize, usize),
    anchor: Option<(usize, usize)>,
    scroll: (usize, usize, usize),
    pointer: (u16, u16),
    cursor_visible: bool,
}

/// Only ordinary, unmodified character selection owns this fast path. The
/// press must have begun in the same code area; captured panel/terminal and
/// overlay gestures must never inherit an old editor selection.
fn editor_selection_drag_state(
    app: &App,
    mouse: crossterm::event::MouseEvent,
) -> Option<SelectionDragState> {
    use crossterm::event::{MouseButton, MouseEventKind};
    if mouse.kind != MouseEventKind::Drag(MouseButton::Left) || !mouse.modifiers.is_empty()
        || app.multi_click_anchor.is_some() || app.middle_drag_start.is_some()
        || app.dragging_gutter || app.dragging_v_scrollbar || app.dragging_h_scrollbar
        || app.dragging_diff_left_h_scrollbar || app.dragging_minimap
        || app.alt_cursor_mouse_down_active || app.desks.popup.mode.is_some()
        || app.hunk_peek.is_some() || app.hunk_popup_active
        || app.side_notice.visible() || app.remote_connection.details || app.inline_blame_hover.is_some()
        || app.background.activity.details_open || app.file_deletion.is_some()
        || app.completion_index_ui.open
        || app.terminal_mouse_capture.is_some() || app.terminal_selection_drag.is_some()
        || app.extension_pointer_capture.is_some() || app.explorer_scrollbar_drag.is_some()
        || app.settings_scrollbar_drag.is_some() || app.dragging_settings_categories
    {
        return None;
    }
    navigation_batch_target(app)?;
    if app.lsp_hover_active || app.should_render_diff_for_active_editor() { return None; }
    let area = app.code_area.filter(|area| area.contains((mouse.column, mouse.row).into()))?;
    let press = app.drag_start_pos?;
    if !area.contains(press.into()) { return None; }
    let editor = app.editor()?;
    if editor.selection_anchor.is_none() || !editor.extra_cursors.is_empty()
        || editor.ai_review.active || editor.diff_display_rows.is_some()
    {
        return None;
    }
    Some(SelectionDragState {
        area,
        target: (app.active_group, app.active_tab, editor.navigation_id),
        content_version: editor.content_version,
        cursor: (editor.cursor_line, editor.cursor_col),
        anchor: editor.selection_anchor,
        scroll: (editor.scroll_offset, editor.wrap_scroll_offset, editor.horizontal_scroll),
        pointer: (app.mouse_col, app.mouse_row),
        cursor_visible: app.cursor_visible,
    })
}

/// An input may arrive while a frame is being prepared (or this thread is
/// descheduled). Refresh only the same ordinary selection before painting;
/// never present a known-old endpoint merely because preparation started first.
fn refresh_selection_before_paint(
    app: &mut App,
    pump: &mut TerminalEventPump,
    drag: &mut Option<(crossterm::event::MouseEvent, SelectionDragState)>,
) -> bool {
    let Some((mouse, state)) = drag.as_ref() else { return false; };
    if editor_selection_drag_state(app, *mouse).as_ref() != Some(state) {
        *drag = None;
        return false;
    }
    let Some(next) = pump.next_matching_mouse(|next| {
        next.kind == mouse.kind && next.modifiers == mouse.modifiers
            && state.area.contains((next.column, next.row).into())
    }) else { return false; };
    let changed = handle_mouse_input(app, pump, next);
    let mouse = crossterm::event::MouseEvent {
        column: app.mouse_col, row: app.mouse_row, ..next
    };
    *drag = editor_selection_drag_state(app, mouse).map(|state| (mouse, state));
    changed
}

fn editor_scroll_batch_area(app: &App, mouse: crossterm::event::MouseEvent) -> Option<ratatui::layout::Rect> {
    navigation_batch_target(app)?;
    if app.search_active || app.lsp_hover_active || app.should_render_diff_for_active_editor() {
        return None;
    }
    app.code_area.filter(|area| area.contains((mouse.column, mouse.row).into()))
}

fn is_marker_word_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Write xterm-style CSI sequence with modifier: ESC[1;{mod}{key}
/// Returns the number of bytes written.
pub(crate) fn write_csi_mod(buf: &mut [u8; 16], key: u8, modifier: u8) -> usize {
    buf[0] = 0x1b;
    buf[1] = b'[';
    buf[2] = b'1';
    buf[3] = b';';
    // modifier is 2-8, single digit
    buf[4] = b'0' + modifier;
    buf[5] = key;
    6
}

fn count_word_occurrences(haystack: &str, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    let hay_bytes = haystack.as_bytes();
    let mut count = 0;
    let mut pos = 0;
    while let Some(found) = haystack[pos..].find(needle) {
        let start = pos + found;
        let end = start + needle.len();
        let before_ok = start == 0 || !is_marker_word_char(hay_bytes[start - 1]);
        let after_ok = end >= hay_bytes.len() || !is_marker_word_char(hay_bytes[end]);
        if before_ok && after_ok {
            count += 1;
        }
        pos = start + haystack[start..].chars().next().map_or(1, char::len_utf8);
    }
    count
}

#[cfg(test)]
fn count_word_occurrences_in_rope(rope: &ropey::Rope, needle: &str) -> usize {
    count_word_occurrences_in_rope_cancellable(rope, needle, || false).unwrap_or_default()
}

fn count_word_occurrences_in_rope_cancellable(
    rope: &ropey::Rope,
    needle: &str,
    is_cancelled: impl Fn() -> bool,
) -> Option<usize> {
    use std::borrow::Cow;

    let mut count = 0usize;
    for line in rope.lines() {
        // This is cancellation polling, not a viewport or document limit.
        // Every line is scanned unless a newer request supersedes this one.
        if is_cancelled() {
            return None;
        }
        let text: Cow<'_, str> = match line.as_str() {
            Some(text) => Cow::Borrowed(text),
            None => Cow::Owned(line.to_string()),
        };
        count += count_word_occurrences(&text, needle);
    }
    (!is_cancelled()).then_some(count)
}

/// Single-pass function that counts all 4 diagnostic markers at once.
/// Returns (errors, warnings) where errors = BUG + FIXME, warnings = TODO + HACK.
fn count_diagnostic_markers(haystack: &str) -> (usize, usize) {
    let hay = haystack.as_bytes();
    let len = hay.len();
    let mut errors = 0usize;
    let mut warnings = 0usize;
    let mut i = 0;
    while i < len {
        let b = hay[i];
        // Quick filter: only check at word boundary starts
        let before_ok = i == 0 || !is_marker_word_char(hay[i - 1]);
        if !before_ok {
            i += 1;
            continue;
        }
        match b {
            b'B' => {
                // BUG (3 bytes)
                if i + 3 <= len && &hay[i..i + 3] == b"BUG" {
                    let after_ok = i + 3 >= len || !is_marker_word_char(hay[i + 3]);
                    if after_ok {
                        errors += 1;
                        i += 3;
                        continue;
                    }
                }
            }
            b'F' => {
                // FIXME (5 bytes)
                if i + 5 <= len && &hay[i..i + 5] == b"FIXME" {
                    let after_ok = i + 5 >= len || !is_marker_word_char(hay[i + 5]);
                    if after_ok {
                        errors += 1;
                        i += 5;
                        continue;
                    }
                }
            }
            b'T' => {
                // TODO (4 bytes)
                if i + 4 <= len && &hay[i..i + 4] == b"TODO" {
                    let after_ok = i + 4 >= len || !is_marker_word_char(hay[i + 4]);
                    if after_ok {
                        warnings += 1;
                        i += 4;
                        continue;
                    }
                }
            }
            b'H' => {
                // HACK (4 bytes)
                if i + 4 <= len && &hay[i..i + 4] == b"HACK" {
                    let after_ok = i + 4 >= len || !is_marker_word_char(hay[i + 4]);
                    if after_ok {
                        warnings += 1;
                        i += 4;
                        continue;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    (errors, warnings)
}

/// Previous full scan retained as an exact test and benchmark reference for
/// the incremental content-analysis worker.
#[cfg(test)]
fn analyze_editor_content_until(
    rope: &ropey::Rope,
    deadline: std::time::Instant,
    cancelled: &impl Fn() -> bool,
) -> Option<(
    usize,
    usize,
    usize,
    usize,
    std::sync::Arc<[crate::background::StickyScopeLine]>,
)> {
    use std::borrow::Cow;

    let mut errors = 0usize;
    let mut warnings = 0usize;
    let mut max_line_width = 0usize;
    let mut word_count = 0usize;
    let mut sticky_scopes = Vec::new();
    let mut scope_stack: Vec<(usize, usize)> = Vec::new();

    for (line_idx, line) in rope.lines().enumerate() {
        if cancelled() || (line_idx.is_multiple_of(128) && std::time::Instant::now() >= deadline) {
            return None;
        }
        let text: Cow<'_, str> = match line.as_str() {
            Some(text) => Cow::Borrowed(text),
            None => Cow::Owned(line.to_string()),
        };
        let (line_errors, line_warnings) = count_diagnostic_markers(&text);
        errors += line_errors;
        warnings += line_warnings;
        word_count += text.split_whitespace().count();

        let without_eol = text.trim_end_matches(['\n', '\r']);
        let width = if without_eol.is_ascii() {
            without_eol.len()
        } else {
            without_eol.chars().count()
        };
        max_line_width = max_line_width.max(width);

        if !without_eol.is_empty()
            && (without_eol.ends_with('{')
                || without_eol.ends_with('(')
                || without_eol.ends_with('[')
                || without_eol.ends_with(':'))
        {
            let indent = without_eol
                .chars()
                .take_while(|character| *character == ' ' || *character == '\t')
                .count();
            while scope_stack
                .last()
                .is_some_and(|(parent_indent, _)| *parent_indent >= indent)
            {
                scope_stack.pop();
            }
            let parent = scope_stack.last().map(|(_, index)| *index);
            let index = sticky_scopes.len();
            sticky_scopes.push(crate::background::StickyScopeLine {
                line: line_idx,
                parent,
            });
            scope_stack.push((indent, index));
        }
    }

    if cancelled() || std::time::Instant::now() >= deadline {
        None
    } else {
        Some((errors, warnings, max_line_width, word_count, sticky_scopes.into()))
    }
}

#[cfg(test)]
fn analyze_editor_content(
    rope: &ropey::Rope,
) -> (
    usize,
    usize,
    usize,
    usize,
    std::sync::Arc<[crate::background::StickyScopeLine]>,
) {
    analyze_editor_content_until(
        rope,
        std::time::Instant::now() + std::time::Duration::from_secs(60),
        &|| false,
    )
    .expect("test analysis deadline")
}

fn analyze_viewport_brackets(
    rope: &ropey::Rope,
    depth_index: &crate::background::BracketDepthIndex,
    first_line: usize,
    visible_lines: usize,
) -> (i32, Vec<(usize, usize, usize, i32)>) {
    use crate::background::MAX_BRACKET_LINE_CHARS;
    let starting_depth = depth_index.line_starting_depth(first_line).unwrap_or(0);
    if !depth_index.enabled() {
        return (0, Vec::new());
    }
    let scan_start = first_line.saturating_sub(200).min(rope.len_lines());
    let scan_end = first_line.saturating_add(visible_lines).saturating_add(200).min(rope.len_lines());
    let mut depth = depth_index.line_starting_depth(scan_start).unwrap_or(0);

    let mut guides = Vec::with_capacity(64);
    let mut stack: Vec<(char, usize, usize, i32)> = Vec::with_capacity(32);
    let mut line_buffer = String::new();
    for (offset, line) in rope.lines_at(scan_start).take(scan_end - scan_start).enumerate() {
        let line_idx = scan_start + offset;
        if line.len_chars() > MAX_BRACKET_LINE_CHARS {
            continue;
        }
        let text = line.as_str().unwrap_or_else(|| {
            line_buffer.clear();
            for chunk in line.chunks() { line_buffer.push_str(chunk); }
            &line_buffer
        });
        for (column, character) in text.chars().enumerate()
        {
            match character {
                '(' | '[' | '{' => {
                    depth = depth.saturating_add(1);
                    stack.push((character, line_idx, column, depth));
                }
                ')' | ']' | '}' => {
                    if let Some((open, open_line, open_column, guide_depth)) = stack.pop() {
                        let expected = match character {
                            ')' => '(',
                            ']' => '[',
                            '}' => '{',
                            _ => unreachable!(),
                        };
                        if open == expected && open_line != line_idx {
                            guides.push((open_column, open_line + 1, line_idx, guide_depth));
                        }
                    }
                    depth = (depth - 1).max(0);
                }
                _ => {}
            }
        }
    }
    (starting_depth, guides)
}

#[cfg(test)]
mod tests {
    use super::{
        analyze_editor_content, analyze_viewport_brackets,
        count_diagnostic_markers,
        count_word_occurrences, count_word_occurrences_in_rope,
        count_word_occurrences_in_rope_cancellable, deferred_editor_analysis_is_due,
        deferred_word_occurrence_is_due, has_delayed_auto_save_candidate, next_event_wait,
        reconcile_sticky_cache_document, refresh_caches, refresh_structural_scopes,
        CURSOR_BLINK_INTERVAL, EDITOR_ANALYSIS_EDIT_QUIET_PERIOD,
        FOREGROUND_IDLE_QUIET_PERIOD,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn pointer_event(column: u16) -> crossterm::event::Event {
        crossterm::event::Event::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column,
            row: 4,
            modifiers: crossterm::event::KeyModifiers::NONE,
        })
    }

    fn scroll_event() -> crossterm::event::MouseEvent {
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::ScrollDown,
            column: 80,
            row: 10,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }
    }

    fn scrolling_app() -> crate::app::App {
        let mut app = crate::app::App::test_new();
        let mut editor = crate::editor::Editor::new();
        editor.rope = ropey::Rope::from_str(&"fn sample() { let value = 123; }\n".repeat(2000));
        app.tabs = vec![crate::app::Tab::new_file(editor)];
        app.active_tab = 0;
        app.scroll_speed = 3;
        app
    }

    #[test]
    fn scroll_batch_works_in_explorer_without_an_open_file() {
        use crossterm::event::Event;
        let mut app = scrolling_app();
        app.tabs.clear();
        app.explorer_area = Some(ratatui::layout::Rect::new(0, 0, 40, 50));
        app.explorer.visible_height = 45;
        app.explorer.entries = (0..1000).map(|index| crate::explorer::FileEntry {
            path: format!("file-{index}.rs").into(),
            name: format!("file-{index}.rs"),
            is_dir: false,
            depth: 0,
            expanded: false,
            git_status: crate::explorer::GitStatus::Unmodified,
            git_status_summary: Default::default(),
        }).collect();
        let mouse = crossterm::event::MouseEvent { column: 20, ..scroll_event() };
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = super::TerminalEventPump::from_receiver(receiver);
        for _ in 0..100 {
            sender.send(Ok(Event::Mouse(mouse))).unwrap();
        }
        while let Some(Event::Mouse(next)) = pump.try_recv().unwrap() {
            super::handle_mouse_input(&mut app, &mut pump, next);
        }
        assert!(app.editor().is_none());
        assert_eq!(app.explorer.scroll_offset, 300);
    }

    #[test]
    fn scroll_batch_preserves_deltas_and_yields_after_event_budget() {
        use crossterm::event::Event;
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = super::TerminalEventPump::from_receiver(receiver);
        let mouse = scroll_event();
        for _ in 0..100 {
            sender.send(Ok(Event::Mouse(mouse))).unwrap();
        }
        let mut app = scrolling_app();
        let mut frames = 0;
        while let Some(Event::Mouse(next)) = pump.try_recv().unwrap() {
            assert!(super::handle_mouse_input(&mut app, &mut pump, next));
            frames += 1;
            assert!(app.editor().unwrap().scroll_offset <= frames * super::MAX_SCROLL_BATCH * 3);
        }
        assert_eq!(app.editor().unwrap().scroll_offset, 300);
        assert!(frames >= 2);
    }

    #[test]
    fn scroll_batch_stops_at_position_direction_modifier_and_input_boundaries() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEventKind};
        let mouse = scroll_event();
        for boundary in [
            Event::Mouse(crossterm::event::MouseEvent { column: 81, ..mouse }),
            Event::Mouse(crossterm::event::MouseEvent { row: 11, ..mouse }),
            Event::Mouse(crossterm::event::MouseEvent { kind: MouseEventKind::ScrollUp, ..mouse }),
            Event::Mouse(crossterm::event::MouseEvent { modifiers: KeyModifiers::SHIFT, ..mouse }),
            pointer_event(80),
            Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            Event::Resize(80, 24),
            Event::FocusLost,
        ] {
            let (sender, receiver) = std::sync::mpsc::channel();
            let mut pump = super::TerminalEventPump::from_receiver(receiver);
            sender.send(Ok(Event::Mouse(mouse))).unwrap();
            sender.send(Ok(boundary.clone())).unwrap();
            sender.send(Ok(Event::Mouse(mouse))).unwrap();
            assert!(pump.next_matching_scroll(mouse));
            assert!(!pump.next_matching_scroll(mouse));
            assert_eq!(pump.try_recv().unwrap(), Some(boundary));
            assert_eq!(pump.try_recv().unwrap(), Some(Event::Mouse(mouse)));
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = super::TerminalEventPump::from_receiver(receiver);
        sender.send(Err(std::io::Error::other("wheel error"))).unwrap();
        assert!(!pump.next_matching_scroll(mouse));
        assert_eq!(pump.try_recv().unwrap_err().to_string(), "wheel error");
        drop(sender);
        assert!(!pump.next_matching_scroll(mouse));
        assert_eq!(pump.try_recv().unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[test]
    #[ignore = "manual release benchmark"]
    fn benchmark_scroll_backlog() {
        use crossterm::event::Event;
        use ratatui::{backend::TestBackend, Terminal};
        let mut previous_samples = Vec::new();
        let mut batched_samples = Vec::new();
        for sample in 0..7 {
            let mut outputs = Vec::new();
            // Alternate order to reduce warm-up and clock bias.
            for batched in [sample % 2 == 0, sample % 2 != 0] {
                let mut app = scrolling_app();
                let mut terminal = Terminal::new(TestBackend::new(160, 50)).unwrap();
                terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                let (sender, receiver) = std::sync::mpsc::channel();
                let mut pump = super::TerminalEventPump::from_receiver(receiver);
                for _ in 0..120 {
                    sender.send(Ok(Event::Mouse(scroll_event()))).unwrap();
                }
                let mut frames = 0;
                let started = std::time::Instant::now();
                while let Some(Event::Mouse(mouse)) = pump.try_recv().unwrap() {
                    if batched {
                        super::handle_mouse_input(&mut app, &mut pump, mouse);
                    } else {
                        crate::mouse::handle_mouse(&mut app, mouse);
                    }
                    terminal.draw(|f| crate::ui::draw(f, &mut app)).unwrap();
                    frames += 1;
                }
                let elapsed = started.elapsed();
                assert_eq!(app.editor().unwrap().scroll_offset, 360);
                outputs.push(terminal.backend().buffer().clone());
                if batched { batched_samples.push(elapsed); } else { previous_samples.push(elapsed); }
                eprintln!("scroll backlog batched={batched} frames={frames} elapsed={elapsed:?}");
            }
            assert_eq!(outputs[0], outputs[1]);
        }
        previous_samples.sort();
        batched_samples.sort();
        eprintln!("scroll backlog median before={:?} after={:?}", previous_samples[3], batched_samples[3]);
    }

    #[test]
    fn input_pump_coalesces_queued_hover_without_waiting_for_more_input() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = super::TerminalEventPump::from_receiver(receiver);
        for column in 0..100 {
            sender.send(Ok(pointer_event(column))).unwrap();
        }
        assert_eq!(pump.try_recv().unwrap(), Some(pointer_event(99)));
        assert_eq!(pump.try_recv().unwrap(), None);
        sender.send(Ok(pointer_event(100))).unwrap();
        assert_eq!(pump.try_recv().unwrap(), Some(pointer_event(100)));
    }

    #[test]
    fn cosmetic_deadline_input_peek_preserves_batching_order_errors_and_disconnect() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = super::TerminalEventPump::from_receiver(receiver);
        assert!(!pump.has_pending_event());
        sender.send(Ok(pointer_event(1))).unwrap();
        sender.send(Ok(pointer_event(2))).unwrap();
        sender
            .send(Err(std::io::Error::other("input failed")))
            .unwrap();
        assert!(pump.has_pending_event());
        assert!(pump.has_pending_event());
        assert_eq!(pump.try_recv().unwrap(), Some(pointer_event(2)));
        assert!(pump.has_pending_event());
        assert_eq!(pump.try_recv().unwrap_err().to_string(), "input failed");
        assert!(!pump.has_pending_event());
        drop(sender);
        assert!(!pump.has_pending_event());
        assert_eq!(
            pump.try_recv().unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn input_pump_preserves_event_and_modifier_boundaries() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let mouse = |kind, modifiers| Event::Mouse(MouseEvent {
            kind, column: 2, row: 4, modifiers,
        });
        let boundaries = [
            mouse(MouseEventKind::Down(MouseButton::Left), KeyModifiers::NONE),
            mouse(MouseEventKind::Up(MouseButton::Left), KeyModifiers::NONE),
            mouse(MouseEventKind::Drag(MouseButton::Left), KeyModifiers::NONE),
            mouse(MouseEventKind::ScrollDown, KeyModifiers::NONE),
            mouse(MouseEventKind::Moved, KeyModifiers::ALT),
            Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            Event::Resize(120, 40),
            Event::FocusLost,
            Event::FocusGained,
            Event::Paste("text".to_string()),
        ];
        for boundary in boundaries {
            let (sender, receiver) = std::sync::mpsc::channel();
            let mut pump = super::TerminalEventPump::from_receiver(receiver);
            for event in [pointer_event(0), pointer_event(1), boundary.clone(), pointer_event(3), pointer_event(4)] {
                sender.send(Ok(event)).unwrap();
            }
            assert_eq!(pump.try_recv().unwrap(), Some(pointer_event(1)));
            assert_eq!(pump.try_recv().unwrap(), Some(boundary));
            assert_eq!(pump.try_recv().unwrap(), Some(pointer_event(4)));
            assert_eq!(pump.try_recv().unwrap(), None);
        }
    }

    #[test]
    fn input_pump_delivers_hover_before_input_error_or_disconnect() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = super::TerminalEventPump::from_receiver(receiver);
        sender.send(Ok(pointer_event(1))).unwrap();
        sender.send(Err(std::io::Error::other("input failed"))).unwrap();
        assert_eq!(pump.try_recv().unwrap(), Some(pointer_event(1)));
        assert_eq!(pump.try_recv().unwrap_err().to_string(), "input failed");
        sender.send(Ok(pointer_event(2))).unwrap();
        drop(sender);
        assert_eq!(pump.try_recv().unwrap(), Some(pointer_event(2)));
        assert_eq!(pump.try_recv().unwrap_err().kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn input_pump_bounds_hover_work_per_frame() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = super::TerminalEventPump::from_receiver(receiver);
        let batch = super::TerminalEventPump::MAX_MOVE_BATCH as u16;
        for column in 0..batch + 10 {
            sender.send(Ok(pointer_event(column))).unwrap();
        }
        assert_eq!(pump.try_recv().unwrap(), Some(pointer_event(batch - 1)));
        assert_eq!(pump.try_recv().unwrap(), Some(pointer_event(batch + 9)));
    }

    #[test]
    fn split_drag_coalesces_positions_but_keeps_release_modifiers_and_other_input_in_order() {
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let drag = |row| MouseEvent { kind: MouseEventKind::Drag(MouseButton::Left), column: 20, row, modifiers: KeyModifiers::NONE };
        let boundaries = [
            Event::Mouse(MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), ..drag(4) }),
            Event::Mouse(MouseEvent { modifiers: KeyModifiers::SHIFT, ..drag(4) }),
            Event::Mouse(MouseEvent { kind: MouseEventKind::Drag(MouseButton::Right), ..drag(4) }),
            Event::Mouse(MouseEvent { kind: MouseEventKind::ScrollDown, ..drag(4) }),
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            Event::Resize(180, 60), Event::FocusLost,
        ];
        for boundary in boundaries {
            let (sender, receiver) = std::sync::mpsc::channel();
            let mut pump = super::TerminalEventPump::from_receiver(receiver);
            for event in [Event::Mouse(drag(2)), Event::Mouse(drag(3)), boundary.clone(), Event::Mouse(drag(5))] {
                sender.send(Ok(event)).unwrap();
            }
            assert_eq!(pump.latest_split_drag(drag(1)), drag(3));
            assert_eq!(pump.try_recv().unwrap(), Some(boundary));
            assert_eq!(pump.try_recv().unwrap(), Some(Event::Mouse(drag(5))));
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let mut pump = super::TerminalEventPump::from_receiver(receiver);
        for row in 1..=super::TerminalEventPump::MAX_MOVE_BATCH + 10 {
            sender.send(Ok(Event::Mouse(drag(row as u16)))).unwrap();
        }
        assert_eq!(pump.latest_split_drag(drag(0)).row, super::TerminalEventPump::MAX_MOVE_BATCH as u16 - 1);
        assert_eq!(pump.latest_split_drag(drag(super::TerminalEventPump::MAX_MOVE_BATCH as u16)).row, super::TerminalEventPump::MAX_MOVE_BATCH as u16 + 10);
        sender.send(Err(std::io::Error::other("input failed"))).unwrap();
        assert_eq!(pump.latest_split_drag(drag(0)), drag(0));
        assert_eq!(pump.try_recv().unwrap_err().to_string(), "input failed");
    }

    #[test]
    fn count_word_treats_underscore_as_word_char() {
        assert_eq!(count_word_occurrences("TODO_ITEM", "TODO"), 0);
    }

    #[test]
    fn diagnostic_markers_basic() {
        let (e, w) = count_diagnostic_markers("TODO: fix this BUG");
        assert_eq!(e, 1); // BUG
        assert_eq!(w, 1); // TODO
    }

    #[test]
    fn diagnostic_markers_all_four() {
        let (e, w) = count_diagnostic_markers("BUG FIXME TODO HACK");
        assert_eq!(e, 2); // BUG + FIXME
        assert_eq!(w, 2); // TODO + HACK
    }

    #[test]
    fn diagnostic_markers_word_boundaries() {
        let (e, w) = count_diagnostic_markers("TODO_ITEM FIXME_IT BUGGY HACKED");
        assert_eq!(e, 0);
        assert_eq!(w, 0);
    }

    #[test]
    fn diagnostic_markers_empty() {
        let (e, w) = count_diagnostic_markers("");
        assert_eq!(e, 0);
        assert_eq!(w, 0);
    }

    #[test]
    fn diagnostic_markers_at_boundaries() {
        let (e, w) = count_diagnostic_markers("BUG\nTODO\nFIXME\nHACK");
        assert_eq!(e, 2);
        assert_eq!(w, 2);
    }

    #[test]
    fn editor_analysis_is_exact_past_old_sampling_boundaries() {
        let mut text = "ordinary line\n".repeat(6_000);
        text.push_str("TODO final-marker\nthis-is-the-longest-line\n");
        let rope = ropey::Rope::from_str(&text);

        let (errors, warnings, max_width, words, _) = analyze_editor_content(&rope);

        assert_eq!(errors, 0);
        assert_eq!(warnings, 1);
        assert_eq!(max_width, "this-is-the-longest-line".len());
        assert_eq!(words, 12_003);
    }

    #[test]
    fn word_occurrences_cover_the_entire_rope() {
        let mut text = "unrelated\n".repeat(5_001);
        text.push_str("needle\n");
        let rope = ropey::Rope::from_str(&text);

        assert_eq!(count_word_occurrences_in_rope(&rope, "needle"), 1);
    }

    #[test]
    fn superseded_word_occurrence_scan_stops_early() {
        let rope = ropey::Rope::from_str(&"needle on a large line\n".repeat(50_000));
        let cancellation_checks = AtomicUsize::new(0);

        let count = count_word_occurrences_in_rope_cancellable(&rope, "needle", || {
            cancellation_checks.fetch_add(1, Ordering::Relaxed) >= 1
        });

        assert_eq!(count, None);
        assert_eq!(cancellation_checks.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn deferred_editor_analysis_runs_after_quiet_time_without_a_redraw() {
        let mut app = crate::app::App::test_new();
        let mut editor = crate::editor::Editor::test_with_text("");
        editor.modified = true;
        app.tabs.push(crate::app::Tab::new_file(editor));
        app.last_edit_time = std::time::Instant::now();

        refresh_caches(&mut app);
        assert!(app.editor_analysis_deferred);
        assert!(!deferred_editor_analysis_is_due(&app));

        app.last_edit_time = std::time::Instant::now() - EDITOR_ANALYSIS_EDIT_QUIET_PERIOD;
        assert!(deferred_editor_analysis_is_due(&app));
        refresh_caches(&mut app);
        assert!(!app.editor_analysis_deferred);
        assert!(app.bg_editor_analysis_running);
    }

    #[test]
    fn missing_bracket_index_uses_edit_quiet_timer_but_warm_viewports_queue_immediately() {
        let mut app = crate::app::App::test_new();
        let mut editor = crate::editor::Editor::test_with_text("{\n}\n");
        editor.modified = true;
        app.tabs.push(crate::app::Tab::new_file(editor));
        app.active_tab = 0;
        app.cached_editor_analysis_target = app.editor_analysis_target();
        app.bracket_pair_colorization = true;
        app.last_edit_time = std::time::Instant::now();
        refresh_caches(&mut app);
        assert!(app.editor_analysis_deferred);
        assert!(!app.bg_bracket_analysis_running);
        assert!(
            next_event_wait(&app, false, false)
                .is_some_and(|wait| wait <= EDITOR_ANALYSIS_EDIT_QUIET_PERIOD)
        );
        app.last_edit_time = std::time::Instant::now() - EDITOR_ANALYSIS_EDIT_QUIET_PERIOD;
        assert!(deferred_editor_analysis_is_due(&app));
        refresh_caches(&mut app);
        assert!(!app.editor_analysis_deferred);
        assert!(app.bg_bracket_analysis_running);

        // A current snapshot makes scrolling cheap even if an unrelated recent
        // edit timestamp remains. No document rebuild needs the quiet gate.
        app.wait_for_background(Duration::from_secs(3), |app| !app.bg_bracket_analysis_running);
        let source = app.editor().unwrap();
        app.cached_bracket_depth_index = Some(std::sync::Arc::new(
            crate::background::BracketDepthIndex::build(
                source.rope.clone(),
                source.navigation_id,
                source.content_version,
                source.wrap_revision,
            ),
        ));
        app.cached_bracket_analysis_target = None;
        app.last_edit_time = std::time::Instant::now();
        refresh_caches(&mut app);
        assert!(!app.editor_analysis_deferred);
        assert!(app.bg_bracket_analysis_running);
        app.tabs.clear();
        refresh_caches(&mut app);
        assert!(app.cached_bracket_depth_index.is_none());
        assert!(app.cached_bracket_analysis_target.is_none());
        assert!(app.cached_bracket_guides.is_empty());
    }

    fn structural_scope_app() -> crate::app::App {
        let mut app = crate::app::App::test_new();
        let mut editor = crate::editor::Editor::test_with_text("fn sample() {\n    work();\n}\n");
        editor.file_path = Some("sample.rs".into());
        editor.large_file_mode = true;
        app.tabs = vec![crate::app::Tab::new_file(editor)];
        app.active_tab = 0;
        app.bracket_pair_colorization = false;
        app.animation_settings.set(
            crate::animation_config::AnimationEffect::ScopeConnector,
            true,
        );
        app
    }

    #[test]
    fn structural_scope_jobs_use_quiet_time_and_reuse_warm_navigation() {
        let mut app = structural_scope_app();
        app.editor_mut().unwrap().modified = true;
        app.last_edit_time = std::time::Instant::now();
        refresh_caches(&mut app);
        assert!(app.editor_analysis_deferred);
        assert!(app.pending_structural_scope.is_none());
        assert!(
            next_event_wait(&app, false, false)
                .is_some_and(|wait| wait <= EDITOR_ANALYSIS_EDIT_QUIET_PERIOD)
        );

        app.last_edit_time = std::time::Instant::now() - EDITOR_ANALYSIS_EDIT_QUIET_PERIOD;
        refresh_caches(&mut app);
        assert!(!app.editor_analysis_deferred);
        assert!(app.pending_structural_scope.is_some());
        let cancel = app.structural_scope_cancel.as_ref().unwrap().clone();
        let editor = app.editor_mut().unwrap();
        editor.cursor_line = 1;
        editor.cursor_col = 5;
        editor.scroll_offset = 1;
        editor.horizontal_scroll = 2;
        refresh_structural_scopes(&mut app);
        assert!(!cancel.is_cancelled());
        app.wait_for_background(Duration::from_secs(5), |app| {
            app.pending_structural_scope.is_none() && app.cached_structural_scope_index.is_some()
        });
        let cached = app.cached_structural_scope_index.as_ref().unwrap().clone();
        assert!(cached.enclosing_scope(app.editor().unwrap()).is_some());

        // A current index bypasses edit timing and schedules no further work
        // when browsing, even in large-file mode with bracket colors disabled.
        app.last_edit_time = std::time::Instant::now();
        refresh_caches(&mut app);
        assert!(!app.editor_analysis_deferred);
        assert!(app.pending_structural_scope.is_none());
        assert!(std::sync::Arc::ptr_eq(
            &cached,
            app.cached_structural_scope_index.as_ref().unwrap(),
        ));

        app.editor_mut().unwrap().rope.insert(20, "extra(); ");
        refresh_structural_scopes(&mut app);
        assert!(app.pending_structural_scope.is_none());
        assert!(std::sync::Arc::ptr_eq(
            &cached,
            app.cached_structural_scope_index.as_ref().unwrap(),
        ));
        assert!(cached.enclosing_scope(app.editor().unwrap()).is_none());
        app.last_edit_time = std::time::Instant::now() - EDITOR_ANALYSIS_EDIT_QUIET_PERIOD;
        refresh_structural_scopes(&mut app);
        app.wait_for_background(Duration::from_secs(5), |app| {
            app.pending_structural_scope.is_none()
                && app
                    .cached_structural_scope_index
                    .as_ref()
                    .is_some_and(|index| app.editor().is_some_and(|editor| index.matches(editor)))
        });
        assert!(!std::sync::Arc::ptr_eq(
            &cached,
            app.cached_structural_scope_index.as_ref().unwrap(),
        ));
        app.tabs.clear();
        refresh_structural_scopes(&mut app);
        assert!(app.cached_structural_scope_index.is_none());
    }

    #[test]
    fn scope_language_disable_prevents_jobs_and_cancels_pending_work() {
        let mut app = structural_scope_app();
        app.animation_settings.scope_disabled_languages = vec!["rust".into()];
        refresh_structural_scopes(&mut app);
        assert!(app.pending_structural_scope.is_none());
        app.animation_settings.scope_disabled_languages.clear();
        refresh_structural_scopes(&mut app);
        assert!(app.pending_structural_scope.is_some());
        let cancel = app.structural_scope_cancel.as_ref().unwrap().clone();
        app.animation_settings.scope_disabled_languages.push("rust".into());
        refresh_structural_scopes(&mut app);
        assert!(cancel.is_cancelled());
        app.wait_for_background(Duration::from_secs(5), |app| app.pending_structural_scope.is_none());
        assert!(app.cached_structural_scope_index.is_none());
        refresh_structural_scopes(&mut app);
        assert!(app.pending_structural_scope.is_none());
    }

    #[test]
    fn structural_scope_jobs_cancel_stale_sources_and_release_the_pending_slot() {
        use crate::animation_structural_scope::StructuralScopeRequest;
        let mut app = structural_scope_app();
        let request = StructuralScopeRequest::from_editor(app.editor().unwrap()).unwrap();
        let cancel = crate::background::CancellationToken::new();
        app.pending_structural_scope = Some(request.clone());
        app.structural_scope_cancel = Some(cancel.clone());

        app.editor_mut().unwrap().rope.insert(0, "// changed\n");
        refresh_structural_scopes(&mut app);
        assert!(cancel.is_cancelled());
        assert!(app.pending_structural_scope.is_some());
        app.background
            .sender
            .send(crate::background::BackgroundResult::StructuralScopes {
                request,
                index: None,
            })
            .unwrap();
        assert!(app.poll_background_results());
        assert!(app.pending_structural_scope.is_none());
        refresh_structural_scopes(&mut app);
        assert!(app.pending_structural_scope.is_some());
        let cancel = app.structural_scope_cancel.as_ref().unwrap().clone();
        app.animation_settings.set(
            crate::animation_config::AnimationEffect::ScopeConnector,
            false,
        );
        refresh_structural_scopes(&mut app);
        assert!(cancel.is_cancelled());
        assert!(app.cached_structural_scope_index.is_none());
        app.wait_for_background(Duration::from_secs(5), |app| {
            app.pending_structural_scope.is_none()
        });
        assert!(app.cached_structural_scope_index.is_none());
    }

    #[test]
    fn commit_copy_feedback_schedules_one_visible_expiry_then_returns_to_idle() {
        let mut app = crate::app::App::test_new();
        app.workspace_watcher_active = true;
        app.idle_quiescent = true;
        let expires_at = std::time::Instant::now() + Duration::from_secs(3);
        app.commit_details.copy_feedback = Some(crate::commit_details::CopyFeedback {
            action: 2, copied: true, expires_at,
        });
        assert_eq!(next_event_wait(&app, false, false), None, "hidden feedback must not wake the UI");
        app.commit_details.visible = true;
        assert!(next_event_wait(&app, false, false)
            .is_some_and(|wait| wait > Duration::ZERO && wait <= Duration::from_secs(3)));
        app.terminal_has_focus = false;
        assert_eq!(next_event_wait(&app, false, false), None);
        app.terminal_has_focus = true;
        assert!(app.commit_details.expire_copy_feedback(expires_at));
        assert_eq!(next_event_wait(&app, false, false), None);
        assert!(!app.commit_details.expire_copy_feedback(expires_at));
    }

    #[test]
    fn stable_workbench_has_no_periodic_poll_deadline() {
        let mut app = crate::app::App::test_new();
        app.workspace_watcher_active = true;
        app.idle_quiescent = true;
        assert_eq!(next_event_wait(&app, false, false), None);

        app.idle_quiescent = false;
        app.last_user_activity = std::time::Instant::now();
        assert!(next_event_wait(&app, false, false)
            .is_some_and(|wait| wait <= FOREGROUND_IDLE_QUIET_PERIOD));

        let mut editor = crate::editor::Editor::test_with_text("idle\n");
        editor.file_path = Some(std::path::PathBuf::from("idle.txt"));
        app.tabs.push(crate::app::Tab::new_file(editor));
        app.active_pane = crate::app::ActivePane::Editor;
        app.last_blink = std::time::Instant::now();
        assert!(next_event_wait(&app, false, false).is_some_and(|wait| {
            wait <= CURSOR_BLINK_INTERVAL
        }));

        app.terminal_has_focus = false;
        app.idle_quiescent = false;
        assert_eq!(next_event_wait(&app, false, false), None);
    }

    #[test]
    fn workspace_search_debounce_arms_one_wait_even_without_terminal_focus() {
        let root = tempfile::tempdir().unwrap();
        let mut app = crate::app::App::new_deferred(root.path().to_owned());
        app.workspace_watcher_active = true;
        app.idle_quiescent = true;
        app.terminal_has_focus = false;
        assert_eq!(next_event_wait(&app, false, false), None);
        app.global_search_query = "needle".into();
        app.schedule_global_search();
        assert!(next_event_wait(&app, false, false).is_some_and(|wait| wait <= Duration::from_millis(300)));
        assert!(app.search_cancel_token.is_none());
        app.search_debounce_ms = 0;
        assert_eq!(next_event_wait(&app, false, false), Some(Duration::ZERO));
        crate::input::handle_global_search_key(&mut app, crossterm::event::KeyCode::Esc, crossterm::event::KeyModifiers::NONE);
        assert_eq!(next_event_wait(&app, false, false), None);
        assert!(!app.bg_global_search_running);
    }

    #[test]
    fn loading_feedback_wakes_only_a_visible_focused_pending_view() {
        let mut app = crate::app::App::new_deferred("loading-workspace".into());
        app.idle_quiescent = true;
        app.workspace_watcher_active = true;
        app.cursor_blinking = false;
        app.show_status_bar = false;
        app.bg_explorer_refresh_running = true;
        draw_animation_fixture(&mut app);
        assert!(next_event_wait(&app, false, false).is_some_and(|wait|
            wait <= Duration::from_millis(80)));
        app.terminal_has_focus = false;
        assert_eq!(next_event_wait(&app, false, false), None);
        app.terminal_has_focus = true;
        app.show_explorer = false;
        draw_animation_fixture(&mut app);
        assert_eq!(next_event_wait(&app, false, false), None);
        app.show_explorer = true;
        app.bg_explorer_refresh_running = false;
        draw_animation_fixture(&mut app);
        assert_eq!(next_event_wait(&app, false, false), None);
    }

    fn draw_animation_fixture(app: &mut crate::app::App) {
        let backend = ratatui::backend::TestBackend::new(100, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
    }

    fn animation_fixture() -> crate::app::App {
        let mut app = crate::app::App::test_new();
        app.tabs.push(crate::app::Tab::new_file(
            crate::editor::Editor::test_with_text("abcdef\nsecond line\n"),
        ));
        app.active_pane = crate::app::ActivePane::Editor;
        app.workspace_watcher_active = true;
        app.idle_quiescent = true;
        app.cursor_blinking = false;
        app.show_status_bar = false;
        app.show_explorer = false;
        app
    }

    #[test]
    fn copy_animation_only_wakes_for_visible_feedback_and_returns_to_idle() {
        let mut app = animation_fixture();
        draw_animation_fixture(&mut app);
        assert_eq!(next_event_wait(&app, false, false), None);
        app.tabs[0].editor.selection_anchor = Some((0, 0));
        app.tabs[0].editor.cursor_col = 4;
        app.start_copy_animation(std::time::Instant::now());
        draw_animation_fixture(&mut app);
        assert!(next_event_wait(&app, false, false).is_some());
        assert!(app.advance_copy_animation(std::time::Instant::now() + Duration::from_secs(6)));
        draw_animation_fixture(&mut app);
        assert_eq!(next_event_wait(&app, false, false), None);
    }

    #[test]
    fn editor_animations_request_frames_only_during_visible_effects() {
        use crate::animation_config::AnimationEffect;
        let mut app = animation_fixture();
        draw_animation_fixture(&mut app);
        app.tabs[0].editor.cursor_col = 4;
        draw_animation_fixture(&mut app);
        assert_eq!(next_event_wait(&app, false, false), None, "default-off editing stays idle");

        app.animation_settings.set(AnimationEffect::CursorSmear, true);
        draw_animation_fixture(&mut app);
        app.tabs[0].editor.cursor_col = 1;
        draw_animation_fixture(&mut app);
        assert!(next_event_wait(&app, false, false)
            .is_some_and(|wait| wait <= Duration::from_millis(17)));

        let after_effect = std::time::Instant::now() + Duration::from_secs(2);
        assert!(app.animations.advance_frame(&app.animation_settings, after_effect),
            "expiry requests a final frame to erase the overlay");
        draw_animation_fixture(&mut app);
        assert_eq!(next_event_wait(&app, false, false), None, "finished effects stop waking the UI");
    }

    #[test]
    fn slow_animation_pacing_preserves_immediate_input_and_worker_redraws() {
        use crate::animation_config::AnimationEffect;
        let mut app = animation_fixture();
        app.animation_settings.fps = 60;
        app.animation_settings.set(AnimationEffect::CursorSmear, true);
        draw_animation_fixture(&mut app);
        app.tabs[0].editor.cursor_col = 4;
        draw_animation_fixture(&mut app);
        let now = std::time::Instant::now();
        app.animations.record_render(now, Duration::from_millis(40));
        assert_eq!(app.animations.next_frame_wait(&app.animation_settings, now), Some(Duration::from_millis(40)));
        assert!(!app.animations.advance_frame(&app.animation_settings, now + Duration::from_millis(20)));
        assert_eq!(next_event_wait(&app, true, false), Some(Duration::ZERO), "input must never wait on cosmetic pacing");
        app.background.sender.send(crate::background::BackgroundResult::GitBranch("main".into())).unwrap();
        assert_eq!(next_event_wait(&app, false, false), Some(Duration::ZERO), "queued work remains ready");
        app.poll_background_results();
        assert_eq!(app.cached_git_branch, "main");
        assert!(app.animations.advance_frame(&app.animation_settings, now + Duration::from_secs(2)));
        draw_animation_fixture(&mut app);
        assert_eq!(next_event_wait(&app, false, false), None, "adaptation must not manufacture idle frames");
        assert_eq!(app.animation_settings.fps, 60, "keep the user's configured rate");
    }

    #[test]
    fn editor_animation_deadlines_stop_for_settings_and_unfocused_windows() {
        use crate::animation_config::AnimationEffect;
        let mut app = animation_fixture();
        app.animation_settings.set(AnimationEffect::CursorSmear, true);
        draw_animation_fixture(&mut app);
        app.tabs[0].editor.cursor_col = 5;
        draw_animation_fixture(&mut app);
        assert!(next_event_wait(&app, false, false).is_some());

        app.terminal_has_focus = false;
        assert_eq!(next_event_wait(&app, false, false), None);
        app.terminal_has_focus = true;
        app.open_settings_tab();
        draw_animation_fixture(&mut app);
        assert_eq!(next_event_wait(&app, false, false), None, "a hidden editor has no animation deadline");
    }

    #[test]
    fn background_frames_coalesce_but_input_work_and_final_frame_stay_ready() {
        let mut app = animation_fixture();
        let now = std::time::Instant::now();
        app.animations.record_render(now, Duration::from_millis(80));
        // Disabled effects and multicursor cleanup may clear animation state
        // every frame; they must not erase the renderer's load measurement.
        app.animations.clear();
        assert_eq!(app.animations.background_frame_wait(now), Duration::from_millis(80));
        assert_eq!(super::next_event_wait_for_redraw(&app, true, true, false), Some(Duration::ZERO));
        assert!(super::next_event_wait_for_redraw(&app, true, false, false)
            .is_some_and(|wait| wait > Duration::ZERO && wait <= Duration::from_millis(80)));
        app.background.sender.send(crate::background::BackgroundResult::GitBranch("main".into())).unwrap();
        assert_eq!(super::next_event_wait_for_redraw(&app, true, false, false), Some(Duration::ZERO));
        app.poll_background_results();
        app.editor_analysis_deferred = true;
        app.last_edit_time = now - Duration::from_secs(1);
        assert!(super::next_event_wait_for_redraw(&app, true, false, false)
            .is_some_and(|wait| wait > Duration::ZERO), "already-dirty timed work must not busy-spin while waiting to paint");
        app.editor_analysis_deferred = false;
        app.terminal_has_focus = false;
        assert!(super::next_event_wait_for_redraw(&app, true, false, false).is_some(), "final background frame retains a deadline after the last producer stops");
        assert_eq!(super::next_event_wait_for_redraw(&app, false, false, false), None, "no redraw request means no pacing timer");
    }

    #[test]
    fn multicursor_stops_animation_deadlines_until_new_single_cursor_motion() {
        use crate::animation_config::AnimationEffect;
        for effect in [AnimationEffect::CursorSmear, AnimationEffect::CursorFire] {
            let mut app = animation_fixture();
            app.animation_settings.set(effect, true);
            draw_animation_fixture(&mut app);
            app.tabs[0].editor.cursor_col = 5;
            draw_animation_fixture(&mut app);
            assert!(next_event_wait(&app, false, false).is_some());

            app.tabs[0].editor.extra_cursors.push((0, 1));
            draw_animation_fixture(&mut app);
            assert_eq!(next_event_wait(&app, false, false), None);
            app.tabs[0].editor.cursor_col = 3;
            draw_animation_fixture(&mut app);
            assert_eq!(next_event_wait(&app, false, false), None);

            app.tabs[0].editor.extra_cursors.clear();
            draw_animation_fixture(&mut app);
            assert_eq!(next_event_wait(&app, false, false), None, "old cursor motion must not resume");
            app.tabs[0].editor.cursor_col = 4;
            draw_animation_fixture(&mut app);
            assert!(next_event_wait(&app, false, false).is_some());
        }
    }

    #[test]
    fn performance_deadlines_stop_when_hidden_or_unfocused() {
        let mut app = crate::app::App::test_new();
        app.workspace_watcher_active = true;
        app.idle_quiescent = true;
        let ticket = app.background.activity.start("Opening", None);
        app.background.refresh_activity(std::time::Instant::now());
        assert!(next_event_wait(&app, false, false).is_some_and(|wait| wait <= crate::performance::ACTIVITY_DELAY));
        app.show_status_bar = false;
        assert_eq!(next_event_wait(&app, false, false), None);
        app.background.activity.details_open = true;
        assert!(next_event_wait(&app, false, false).is_some());
        app.terminal_has_focus = false;
        assert_eq!(next_event_wait(&app, false, false), None);
        drop(ticket);
        app.background.refresh_activity(std::time::Instant::now());
        app.terminal_has_focus = true;
        app.show_status_bar = true;
        assert_eq!(next_event_wait(&app, false, false), None);
    }

    #[test]
    fn process_resource_view_adds_only_a_ten_second_idle_deadline() {
        use crate::process_usage::{Counters, SAMPLE_INTERVAL};
        let mut app = crate::app::App::test_new();
        app.workspace_watcher_active = true;
        app.idle_quiescent = true;
        app.terminal_has_focus = true;
        assert_eq!(next_event_wait(&app, false, false), None);
        crate::ui_performance_details::toggle(&mut app);
        app.performance_view.resources.refresh_with(true, std::time::Instant::now(), || Counters {
            cpu_time: Some(Duration::from_secs(1)),
            resident_bytes: Some(64 * 1024 * 1024),
        });
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 28)).unwrap();
        terminal.draw(|frame| crate::ui_performance_details::draw(frame, &mut app)).unwrap();
        assert!(next_event_wait(&app, false, false)
            .is_some_and(|wait| wait > Duration::from_secs(9) && wait <= SAMPLE_INTERVAL));
        app.terminal_has_focus = false;
        assert_eq!(next_event_wait(&app, false, false), None);
        app.terminal_has_focus = true;
        crate::ui_performance_details::dismiss(&mut app);
        assert_eq!(next_event_wait(&app, false, false), None);
        assert_eq!(app.performance_view.resources.samples, 1);
    }

    #[test]
    fn responsiveness_diagnostics_and_task_totals_do_not_create_idle_polling() {
        use crate::performance::diagnostics::UiPhase;
        let mut app = crate::app::App::test_new();
        app.workspace_watcher_active = true;
        app.idle_quiescent = true;
        let now = std::time::Instant::now();
        app.background.activity.ui.record(UiPhase::Preparation, Duration::from_millis(80), now);
        crate::ui_performance_details::toggle(&mut app);
        app.performance_view.section = crate::ui_performance_details::Section::Timings;
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 28)).unwrap();
        terminal.draw(|frame| crate::ui_performance_details::draw(frame, &mut app)).unwrap();
        assert_eq!(next_event_wait(&app, false, false), None);
        app.background.activity.totals.record("Short task", Duration::from_millis(2), None, false);
        assert_eq!(next_event_wait(&app, false, false), Some(Duration::ZERO));
        terminal.draw(|frame| crate::ui_performance_details::draw(frame, &mut app)).unwrap();
        assert_eq!(next_event_wait(&app, false, false), None);
        app.performance_view.section = crate::ui_performance_details::Section::Workspace;
        app.background.activity.totals.record("Short task", Duration::from_millis(2), None, false);
        assert_eq!(next_event_wait(&app, false, false), None);
    }

    #[test]
    fn pinned_monitor_preserves_editor_input_batching_until_explicitly_focused() {
        use super::{editing_batch_target, navigation_batch_target};
        let mut app = crate::app::App::test_new();
        app.tabs = vec![crate::app::Tab::new_file(crate::editor::Editor::test_with_text("hello\n"))];
        app.active_tab = 0;
        app.active_pane = crate::app::ActivePane::Editor;
        let target = editing_batch_target(&app);
        assert!(target.is_some());
        app.background.activity.details_open = true;
        app.performance_view.pinned = true;
        assert_eq!(editing_batch_target(&app), target);
        assert_eq!(navigation_batch_target(&app), target);
        app.execute_palette_command("performance");
        assert!(app.background.activity.details_open);
        assert_eq!(editing_batch_target(&app), None);
        crate::ui_performance_details::release_focus(&mut app);
        assert_eq!(editing_batch_target(&app), target);
    }

    #[test]
    fn acp_chat_activity_wakes_an_idle_workbench_only_while_visible() {
        use ratatui::{backend::TestBackend, layout::Rect, Terminal};
        use std::time::Duration;
        use vtcode::acp::{protocol as p, Session};
        let mut app = crate::app::App::test_new();
        app.idle_quiescent = true;
        app.focus_chat();
        let id = p::SessionId::new("waiting");
        let mut session = Session::new(id.clone());
        session.begin_prompt("Wait for an answer".into());
        app.chat.sessions.insert(id.clone(), session);
        app.chat.selected = Some(id);
        let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
        terminal
            .draw(|f| crate::ui_chat::draw(f, &mut app, Rect::new(0, 0, 60, 24)))
            .unwrap();
        assert!(next_event_wait(&app, false, false)
            .is_some_and(|wait| wait <= Duration::from_millis(120)));
        app.chat.visible = false;
        assert_eq!(next_event_wait(&app, false, false), None);
        app.chat.visible = true;
        app.terminal_has_focus = false;
        assert!(next_event_wait(&app, false, false).is_some_and(|wait| wait <= Duration::from_millis(120)));
        assert!(super::background_chat_redraw(&app, false, false));
        app.terminal_has_focus = true;
        app.chat
            .session_mut()
            .unwrap()
            .finish(Some(p::StopReason::EndTurn));
        assert_eq!(next_event_wait(&app, false, false), None);
        app.terminal_has_focus = false;
        assert!(super::background_chat_redraw(&app, true, true));
        assert!(super::background_chat_redraw(&app, false, true));
        assert!(!super::background_chat_redraw(&app, false, false));
    }

    #[test]
    fn inline_blame_adds_one_idle_deadline_only_while_waiting_on_the_annotation() {
        use crate::{app::{App, Tab}, editor::Editor, git_blame};
        use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
        use ratatui::layout::Rect;
        use std::sync::Arc;
        let mut app = App::test_new();
        app.terminal_has_focus = true;
        app.idle_quiescent = true;
        app.auto_save = crate::app::AutoSaveMode::Off;
        let mut editor = Editor::test_with_text("value\n");
        editor.file_path = Some(app.explorer.root.join("hover.rs"));
        let anchor = Rect::new(10, 4, 20, 1);
        editor.blame.inline_annotation = Some((anchor, 0));
        app.blame_documents.insert(editor.navigation_id, Arc::new(git_blame::Document {
            path: editor.file_path.clone().unwrap(), revision: None, rope: editor.rope.clone(), key: None,
            lines: vec![git_blame::Line::Uncommitted; editor.rope.len_lines()],
            commit_lines: vec![None; editor.rope.len_lines()], notice: None,
        }));
        app.tabs = vec![Tab::new_file(editor)];
        app.code_area = Some(Rect::new(0, 0, 90, 20));
        app.mouse_col = anchor.x;
        app.mouse_row = anchor.y;
        assert_eq!(next_event_wait(&app, false, false), None);
        assert!(crate::ui_inline_blame::handle_mouse(&mut app, MouseEvent {
            kind: MouseEventKind::Moved, column: anchor.x, row: anchor.y, modifiers: KeyModifiers::NONE,
        }));
        assert!(next_event_wait(&app, false, false).is_some_and(|wait| !wait.is_zero() && wait <= Duration::from_secs(2)));
        app.terminal_has_focus = false;
        assert_eq!(next_event_wait(&app, false, false), None);
        app.terminal_has_focus = true;
        app.context_menu_active = true;
        assert_eq!(next_event_wait(&app, false, false), None);
        app.context_menu_active = false;
        crate::ui_inline_blame::leave(&mut app, 0, 0);
        assert_eq!(next_event_wait(&app, false, false), None);
    }

    #[test]
    fn lsp_hover_wakes_after_startup_without_more_mouse_input_or_idle_polling() {
        use crate::lsp::{LspClient, LspEvent};
        use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
        use ratatui::{Terminal, backend::TestBackend, layout::Rect};
        use serde_json::json;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hover.py");
        let mut app = crate::app::App::test_new();
        // Isolate LSP wakeups from the independent lazy inline-blame deadline.
        app.source_control_enabled = false;
        app.auto_save = crate::app::AutoSaveMode::Off;
        app.terminal_has_focus = true;
        app.idle_quiescent = true;
        app.show_inlay_hints = false;
        app.show_semantic_tokens = false;
        let mut editor = crate::editor::Editor::test_with_text("items = {}\n");
        editor.file_path = Some(path.clone());
        app.tabs = vec![crate::app::Tab::new_file(editor)];
        let (mut client, server) = LspClient::test_pair("python", &["*.py"]);
        client.install_capabilities(serde_json::from_value(json!({"hoverProvider":true})).unwrap());
        client.initialized = false;
        app.lsp_manager.clients.push(client);
        app.lsp_manager.sync_document_owned(&path, 0, "items = {}\n".into());
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        terminal.draw(|frame| crate::ui_editor::draw_editor(frame, &mut app, Rect::new(0, 0, 80, 12))).unwrap();
        let code = app.code_area.unwrap();
        let (_sender, receiver) = std::sync::mpsc::channel();
        let mut pump = super::TerminalEventPump::from_receiver(receiver);
        super::handle_mouse_input(&mut app, &mut pump, MouseEvent {
            kind: MouseEventKind::Moved, column: code.x + 1, row: code.y,
            modifiers: KeyModifiers::NONE,
        });
        crate::app_lsp_poll::poll_lsp(&mut app, &mut false);
        assert_eq!(next_event_wait(&app, false, false), None, "initialization wakes us; do not poll for it");
        server.queue_event(LspEvent::Initialized { capabilities: Box::new(None) });
        crate::app_lsp_poll::poll_lsp(&mut app, &mut false);
        assert!(next_event_wait(&app, false, false).is_some_and(|wait| !wait.is_zero() && wait <= Duration::from_millis(500)));
        app.lsp_hover_request_time = Some(std::time::Instant::now() - Duration::from_millis(501));
        assert_eq!(next_event_wait(&app, false, false), Some(Duration::ZERO));
        crate::app_lsp_poll::poll_lsp(&mut app, &mut false);
        let request = server.take_messages().into_iter().find(|m| m["method"] == "textDocument/hover").unwrap();
        assert_eq!(next_event_wait(&app, false, false), None, "the server reply wakes us");
        server.respond(json!({"id":request["id"],"result":{"contents":"ready hover"}}));
        let mut redraw = false;
        crate::app_lsp_poll::poll_lsp(&mut app, &mut redraw);
        assert!(redraw);
        assert_eq!(app.lsp_manager.hover_content.as_deref(), Some("ready hover"));
        assert_eq!(next_event_wait(&app, false, false), None, "visible hover needs no periodic work");
    }

    #[test]
    fn delayed_diagnostics_wake_the_idle_editor_and_request_one_redraw() {
        let mut app = crate::app::App::test_new();
        app.auto_save = crate::app::AutoSaveMode::Off;
        app.terminal_has_focus = false;
        app.idle_quiescent = true;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("typing.py");
        let (client, _server) = crate::lsp::LspClient::test_pair("python", &["*.py"]);
        app.lsp_manager.clients.push(client);
        app.lsp_manager.sync_document_owned(&path, 1, "digest = 1\n".into());
        assert_eq!(next_event_wait(&app, false, false), None);
        app.lsp_manager.diagnostic_publication.set_delay_ms(10_000);
        app.lsp_manager.invalidate_document_feedback(&path);
        app.lsp_manager.sync_document_owned(&path, 2, "digest = 1\ndige\n".into());
        app.lsp_manager.receive_diagnostics(path.clone(), Some(1), vec![lsp_types::Diagnostic {
            message: "dige is not defined".into(),
            ..Default::default()
        }], std::time::Instant::now());
        assert!(next_event_wait(&app, false, false).is_some_and(|wait| {
            !wait.is_zero() && wait <= std::time::Duration::from_secs(10)
        }));
        let mut redraw = false;
        crate::app_lsp_poll::poll_lsp(&mut app, &mut redraw);
        assert!(!redraw);
        assert!(app.lsp_manager.get_diagnostics(&path).is_none());

        // Make the deadline due without sleeping or requiring another input event.
        app.lsp_manager.diagnostic_publication.set_delay_ms(0);
        assert_eq!(next_event_wait(&app, false, false), Some(std::time::Duration::ZERO));
        crate::app_lsp_poll::poll_lsp(&mut app, &mut redraw);
        assert!(redraw);
        assert_eq!(app.lsp_manager.get_diagnostics(&path).unwrap()[0].message, "dige is not defined");
        assert_eq!(next_event_wait(&app, false, false), None);
        redraw = false;
        crate::app_lsp_poll::poll_lsp(&mut app, &mut redraw);
        assert!(!redraw, "completed publication must not keep waking or redrawing");
    }

    #[test]
    fn delayed_auto_save_does_not_poll_an_in_flight_snapshot() {
        let mut app = crate::app::App::test_new();
        app.auto_save = crate::app::AutoSaveMode::AfterDelay;
        app.auto_save_delay = 100;
        app.last_edit_time = std::time::Instant::now() - std::time::Duration::from_secs(1);
        app.terminal_has_focus = false;
        app.idle_quiescent = true;

        let path = std::path::PathBuf::from("pending-auto-save.txt");
        let mut editor = crate::editor::Editor::test_with_text("changed\n");
        editor.file_path = Some(path.clone());
        editor.modified = true;
        editor.content_version = 7;
        app.tabs.push(crate::app::Tab::new_file(editor));

        assert!(has_delayed_auto_save_candidate(&app));
        assert_eq!(
            next_event_wait(&app, false, false),
            Some(std::time::Duration::ZERO)
        );

        app.pending_file_saves.insert(path, (app.workspace_generation, 7.into()));
        assert!(!has_delayed_auto_save_candidate(&app));
        assert_eq!(next_event_wait(&app, false, false), None);
    }

    #[test]
    fn failed_auto_save_snapshot_waits_for_a_new_edit_before_retrying() {
        let mut app = crate::app::App::test_new();
        app.auto_save = crate::app::AutoSaveMode::AfterDelay;
        app.auto_save_delay = 100;
        app.last_edit_time = std::time::Instant::now() - std::time::Duration::from_secs(1);
        app.terminal_has_focus = false;
        app.idle_quiescent = true;

        let path = std::path::PathBuf::from("failed-auto-save.txt");
        let mut editor = crate::editor::Editor::test_with_text("changed\n");
        editor.file_path = Some(path.clone());
        editor.modified = true;
        editor.content_version = 11;
        app.tabs.push(crate::app::Tab::new_file(editor));
        app.auto_save_attempted_versions.insert(path, 11);

        assert!(!has_delayed_auto_save_candidate(&app));
        assert_eq!(next_event_wait(&app, false, false), None);

        app.tabs[0].editor.content_version = 12;
        assert!(has_delayed_auto_save_candidate(&app));
        assert_eq!(
            next_event_wait(&app, false, false),
            Some(std::time::Duration::ZERO)
        );
    }

    #[test]
    fn word_occurrence_scan_is_coalesced_until_cursor_input_settles() {
        let mut app = crate::app::App::test_new();
        app.status_bar_pins.insert("word_occ".into(), true);
        let mut editor = crate::editor::Editor::test_with_text("needle needle\n");
        editor.cursor_col = 1;
        app.tabs.push(crate::app::Tab::new_file(editor));
        app.last_user_activity = std::time::Instant::now();

        refresh_caches(&mut app);
        assert!(app.word_occurrence_deferred);
        assert!(app.pending_word_occurrence.is_none());
        assert!(!deferred_word_occurrence_is_due(&app));

        app.last_user_activity =
            std::time::Instant::now() - super::WORD_OCCURRENCE_QUIET_PERIOD;
        assert!(deferred_word_occurrence_is_due(&app));
        refresh_caches(&mut app);
        assert!(!app.word_occurrence_deferred);
        assert!(app.pending_word_occurrence.is_some());
    }

    #[test]
    fn viewport_brackets_find_multiline_guides() {
        let rope = ropey::Rope::from_str("fn main() {\n  call(\n    value\n  );\n}\n");
        let index = crate::background::BracketDepthIndex::build(rope.clone(), 0, 0, 0);
        let (_, guides) = analyze_viewport_brackets(&rope, &index, 0, 5);
        assert!(
            guides
                .iter()
                .any(|&(column, start, end, _)| { column == 6 && start == 2 && end == 3 })
        );
    }

    #[test]
    fn viewport_bracket_seeds_and_guide_depths_use_the_document_prefix() {
        let rope = ropey::Rope::from_str(&format!(
            "{{\n{}[\n{}]\n}}\n",
            "plain\n".repeat(330),
            "plain\n".repeat(17)
        ));
        let index = crate::background::BracketDepthIndex::build(rope.clone(), 0, 0, 0);
        for first_line in [150, 199, 200, 201, 250, 320, 330, 331] {
            let (depth, guides) = analyze_viewport_brackets(&rope, &index, first_line, 50);
            assert_eq!(depth, 1, "first_line={first_line}");
            let guide = guides
                .iter()
                .find(|&&(_, start, end, _)| start == 332 && end == 349);
            assert_eq!(
                guide.map(|guide| guide.3),
                Some(2),
                "first_line={first_line}"
            );
        }
    }

    #[test]
    fn sticky_scope_cache_survives_same_editor_content_versions() {
        let mut app = crate::app::App::test_new();
        let cached = crate::background::EditorAnalysisTarget {
            active_group: 0,
            active_tab: 0,
            file_path: Some(std::path::PathBuf::from("fixture.py")),
            content_version: 4,
            len_bytes: 100,
            modified: false,
            disk_modified: None,
        };
        app.cached_editor_analysis_target = Some(cached.clone());
        app.cached_sticky_scopes = vec![crate::background::StickyScopeLine {
            line: 2,
            parent: None,
        }].into();
        app.cached_sticky_lines = vec![(2, "def outer():".to_string())];
        app.cached_sticky_key = 42;

        let edited = crate::background::EditorAnalysisTarget {
            content_version: 5,
            len_bytes: 101,
            modified: true,
            ..cached
        };

        assert!(reconcile_sticky_cache_document(&mut app, &edited));
        assert_eq!(app.cached_sticky_scopes.len(), 1);
        assert_eq!(app.cached_sticky_lines.len(), 1);
        assert_eq!(app.cached_sticky_key, 42);
    }

    #[test]
    fn sticky_scope_cache_is_cleared_for_another_editor() {
        let mut app = crate::app::App::test_new();
        let cached = crate::background::EditorAnalysisTarget {
            active_group: 0,
            active_tab: 0,
            file_path: Some(std::path::PathBuf::from("first.py")),
            content_version: 4,
            len_bytes: 100,
            modified: false,
            disk_modified: None,
        };
        app.cached_editor_analysis_target = Some(cached.clone());
        app.cached_sticky_scopes = vec![crate::background::StickyScopeLine {
            line: 2,
            parent: None,
        }].into();
        app.cached_sticky_lines = vec![(2, "def outer():".to_string())];
        app.cached_sticky_key = 42;

        let other = crate::background::EditorAnalysisTarget {
            active_tab: 1,
            file_path: Some(std::path::PathBuf::from("second.py")),
            ..cached
        };

        assert!(!reconcile_sticky_cache_document(&mut app, &other));
        assert!(app.cached_sticky_scopes.is_empty());
        assert!(app.cached_sticky_lines.is_empty());
        assert_eq!(app.cached_sticky_key, u64::MAX);
    }

    #[test]
    fn render_anchors_survive_only_same_editor_content_versions() {
        let editor = crate::background::EditorAnalysisTarget {
            active_group: 0,
            active_tab: 0,
            file_path: Some(std::path::PathBuf::from("fixture.rs")),
            content_version: 4,
            len_bytes: 100,
            modified: false,
            disk_modified: None,
        };
        let cached_cursor = crate::background::CursorMatchTarget {
            editor: editor.clone(),
            cursor_line: 8,
            cursor_col: 4,
            language_name: "Rust".to_string(),
        };
        let edited_cursor = crate::background::CursorMatchTarget {
            editor: crate::background::EditorAnalysisTarget {
                content_version: 5,
                len_bytes: 101,
                modified: true,
                ..editor.clone()
            },
            ..cached_cursor.clone()
        };
        assert!(cached_cursor.same_render_anchor(&edited_cursor));

        let moved_cursor = crate::background::CursorMatchTarget {
            cursor_col: 5,
            ..edited_cursor
        };
        assert!(!cached_cursor.same_render_anchor(&moved_cursor));

        let cached_word = crate::background::WordOccurrenceTarget {
            editor: editor.clone(),
            word: "value".into(),
        };
        let edited_word = crate::background::WordOccurrenceTarget {
            editor: crate::background::EditorAnalysisTarget {
                content_version: 5,
                ..editor
            },
            word: "value".into(),
        };
        assert!(cached_word.same_render_anchor(&edited_word));
    }
}

/// Keep sticky-scroll data across edits in the same editor while its updated
/// scope tree is prepared on the worker. Clearing these rows on every content
/// version made the pinned headers disappear for one frame while typing.
fn reconcile_sticky_cache_document(
    app: &mut App,
    target: &crate::background::EditorAnalysisTarget,
) -> bool {
    let same_document = app
        .cached_editor_analysis_target
        .as_ref()
        .is_some_and(|cached| cached.same_editor(target));
    if !same_document {
        app.cached_sticky_scopes = Default::default();
        app.cached_sticky_lines.clear();
        app.cached_sticky_key = u64::MAX;
    }
    same_document
}

fn deferred_editor_analysis_is_due(app: &App) -> bool {
    app.editor_analysis_deferred
        && app.last_edit_time.elapsed() >= EDITOR_ANALYSIS_EDIT_QUIET_PERIOD
}

fn deferred_word_occurrence_is_due(app: &App) -> bool {
    app.terminal_has_focus
        && !app.idle_quiescent
        && app.word_occurrence_deferred
        && app.last_user_activity.elapsed() >= WORD_OCCURRENCE_QUIET_PERIOD
}

fn remaining(start: std::time::Instant, duration: Duration) -> Duration {
    duration.saturating_sub(start.elapsed())
}

fn editor_needs_delayed_auto_save(app: &App, editor: &crate::editor::Editor) -> bool {
    if !editor.modified || app.pending_save_as.contains_key(&editor.save_identity)
        || app.pending_paste_document() == Some(editor.save_identity) {
        return false;
    }
    let Some(path) = editor.file_path.as_ref() else {
        return false;
    };
    if crate::remote::workspace::is_view_key(path)
        && (app.remote_workspace.as_ref().is_none_or(|remote| !remote.client.connected())
            || app.remote_connection.checking.contains(path))
    {
        return false;
    }
    let version = editor.content_version;
    if app
        .auto_save_attempted_versions
        .get(path)
        .is_some_and(|attempted| *attempted == version)
    {
        return false;
    }
    if app
        .pending_file_saves
        .get(path)
        .is_some_and(|(generation, pending)| *generation == app.workspace_generation && pending.version == version)
    {
        return false;
    }
    !app
        .deferred_file_saves
        .get(path)
        .is_some_and(|(generation, pending)| *generation == app.workspace_generation && pending.source_content_version == version)
}

fn has_delayed_auto_save_candidate(app: &App) -> bool {
    app.tabs
        .iter()
        .chain(app.groups.iter().flat_map(|group| group.tabs.iter()))
        .any(|tab| editor_needs_delayed_auto_save(app, &tab.editor))
}

/// Submit at most one delayed save for each path/content snapshot. Once a
/// request is pending (or has failed), the event loop can block until another
/// edit or a worker result instead of repeatedly revisiting an expired timer.
fn queue_due_delayed_auto_saves(app: &mut App) -> usize {
    let trim = app.trim_trailing_on_save;
    let newline = app.insert_final_newline;
    let mut newest_by_path = rustc_hash::FxHashMap::default();
    for tab in app
        .tabs
        .iter()
        .chain(app.groups.iter().flat_map(|group| group.tabs.iter()))
    {
        if !editor_needs_delayed_auto_save(app, &tab.editor) {
            continue;
        }
        let Some(request) = tab.editor.background_save_request(trim, newline, false) else {
            continue;
        };
        let replace = newest_by_path
            .get(&request.path)
            .is_none_or(|current: &crate::editor::BackgroundSaveRequest| {
                request.source_content_version > current.source_content_version
            });
        if replace {
            newest_by_path.insert(request.path.clone(), request);
        }
    }

    let count = newest_by_path.len();
    for request in newest_by_path.into_values() {
        app.auto_save_attempted_versions
            .insert(request.path.clone(), request.source_content_version);
        app.queue_background_save_job(request);
    }
    count
}

pub(crate) fn next_visible_extension_tick_wait(app: &App) -> Option<Duration> {
    crate::extension_scheduler::next_wait(app)
}

fn next_event_wait(
    app: &App,
    needs_redraw: bool,
    has_periodic_disk_paths: bool,
) -> Option<Duration> {
    if needs_redraw || app.background.has_pending_results() {
        return Some(Duration::ZERO);
    }

    let mut wait: Option<Duration> = None;
    let mut include = |candidate: Duration| {
        wait = Some(wait.map_or(candidate, |current| current.min(candidate)));
    };
    if let Some(wait) = app.completion_index_next_wait() {
        include(wait);
    }
    if let Some(wait) = crate::app_explorer_hover::next_wait(app, std::time::Instant::now()) {
        include(wait);
    }
    if let Some(wait) = app.copy_animation_next_wait(std::time::Instant::now()) { include(wait); }
    if let Some(reconnect_wait) = app.remote_connection.next_wait() { include(reconnect_wait); }
    if let Some(format_wait) = app.formatting.next_wait() {
        include(format_wait);
    }
    if let Some(wait) = app.loading.next_wait(app.terminal_has_focus, std::time::Instant::now()) {
        include(wait);
    }
    if app.terminal_has_focus && let Some(wait) = app.file_deletion.as_ref().and_then(|deletion| deletion.next_wait()) {
        include(wait);
    }

    if app.terminal_has_focus
        && let Some(animation_wait) = app.animations.next_frame_wait(
            &app.animation_settings,
            std::time::Instant::now(),
        )
    {
        include(animation_wait);
    }

    if app.shutdown_anim_active {
        include(Duration::from_millis(16));
    }
    if let Some(activity_wait) = crate::ui_chat::next_activity_frame(app) {
        include(activity_wait);
    }
    if let Some(resource_wait) = crate::ui_performance_details::resource_next_wait(app, std::time::Instant::now()) {
        include(resource_wait);
    }
    if app.terminal_has_focus && crate::ui_performance_details::timings_need_redraw(app) {
        include(Duration::ZERO);
    }
    if app.terminal_has_focus && app.commit_details.visible
        && let Some(feedback) = &app.commit_details.copy_feedback
    {
        include(feedback.expires_at.saturating_duration_since(std::time::Instant::now()));
    }
    if app.terminal_has_focus
        && (app.background.activity.details_open || (app.show_status_bar && !app.zen_mode))
        && let Some(activity_wait) = app.background.activity.next_wait(std::time::Instant::now())
    {
        include(activity_wait);
    }
    if let Some(deadline) = app.chat.rollback.next_capture_deadline() {
        include(deadline.saturating_duration_since(std::time::Instant::now()));
    }

    if app.editor_analysis_deferred {
        include(remaining(
            app.last_edit_time,
            EDITOR_ANALYSIS_EDIT_QUIET_PERIOD,
        ));
    }
    if app.terminal_has_focus && !app.idle_quiescent && app.word_occurrence_deferred {
        include(remaining(
            app.last_user_activity,
            WORD_OCCURRENCE_QUIET_PERIOD,
        ));
    }
    if app.file_search_refresh_pending {
        include(remaining(
            app.last_edit_time,
            crate::app_search_algo::FILE_SEARCH_EDIT_QUIET_PERIOD,
        ));
    }
    if let Some(wait) = app.global_search_debounce_wait(std::time::Instant::now()) {
        include(wait);
    }
    if !app.lsp_manager.configs.is_empty()
        && app.last_edit_time.elapsed() < crate::app_lsp_poll::LSP_EDIT_QUIET_PERIOD
    {
        include(remaining(
            app.last_edit_time,
            crate::app_lsp_poll::LSP_EDIT_QUIET_PERIOD,
        ));
    }
    if let Some(start) = app.lsp_hover_request_time {
        include(remaining(start, Duration::from_millis(500)));
    }
    if let Some(diagnostic_wait) = app.lsp_manager.diagnostic_publication.next_wait(std::time::Instant::now()) {
        include(diagnostic_wait);
    }

    if matches!(app.auto_save, crate::app::AutoSaveMode::AfterDelay)
        && has_delayed_auto_save_candidate(app)
    {
        include(remaining(
            app.last_edit_time,
            Duration::from_millis(app.auto_save_delay),
        ));
    }

    if let Some(extension_wait) = next_visible_extension_tick_wait(app) { include(extension_wait); }
    if app.terminal_has_focus {
        if !app.idle_quiescent {
            include(remaining(
                app.last_user_activity,
                FOREGROUND_IDLE_QUIET_PERIOD,
            ));
        }
        if !app.idle_quiescent && app.cursor_blinking && app.has_text_input_focus() {
            include(remaining(app.last_blink, CURSOR_BLINK_INTERVAL));
        }
        if app.chord_pending {
            include(remaining(app.chord_start, CHORD_TIMEOUT));
        }
        if !app.idle_quiescent && has_periodic_disk_paths {
            include(remaining(
                app.disk_check_last,
                UNWATCHED_DISK_CHECK_INTERVAL,
            ));
        }
        if !app.tab_tooltip_shown {
            if let (Some(_), Some(start)) =
                (app.tab_tooltip_tab, app.tab_tooltip_hover_start)
            {
                include(remaining(start, TAB_TOOLTIP_DELAY));
            }
        }
        if !app.chrome_hover.shown && let Some(start) = app.chrome_hover.started {
            include(remaining(start, crate::ui_status::HOVER_DELAY));
        }
        if let Some(blame_wait) = crate::ui_inline_blame::next_wait(app, std::time::Instant::now()) {
            include(blame_wait);
        }
        if let Some(blame_wait) = crate::git_blame::next_wait(app, std::time::Instant::now()) {
            include(blame_wait);
        }
        if let Some(start) = app.saving_flash {
            include(remaining(start, SAVING_FLASH_LIFETIME));
        }
        if app.screencast_mode {
            for (_, created) in &app.screencast_keys {
                include(remaining(*created, SCREENCAST_LIFETIME));
            }
        }
    }

    wait
}

fn next_event_wait_for_redraw(app: &App, needs_redraw: bool, input_redraw: bool, has_periodic_disk_paths: bool) -> Option<Duration> {
    let redraw_wait = if input_redraw { Duration::ZERO } else {
        app.animations.background_frame_wait(std::time::Instant::now())
    };
    let wait = next_event_wait(app, needs_redraw && redraw_wait.is_zero(), has_periodic_disk_paths);
    if needs_redraw {
        // A due visual/analysis timer may remain due until the dirty frame is
        // painted. Do not spin on its zero deadline while that frame is paced.
        // Producers still wake us, incomplete service cycles never park, and
        // ready worker results are consumed immediately.
        Some(wait.map_or(redraw_wait, |current| {
            if current.is_zero() && !input_redraw && !app.background.has_pending_results() {
                redraw_wait
            } else {
                current.min(redraw_wait)
            }
        }))
    } else {
        wait
    }
}

fn refresh_blame_cache(app: &mut App) {
    crate::git_blame::refresh(app);
}

fn refresh_structural_scopes(app: &mut App) {
    use crate::animation_structural_scope::StructuralScopeRequest;
    let editor = app.editor().filter(|editor| !editor.bg_loading);
    let enabled = editor.is_some_and(|editor| app.animation_settings.scope_enabled_for(editor));
    if let Some(pending) = &app.pending_structural_scope
        && (!enabled || editor.is_none_or(|editor| !pending.matches(editor)))
        && let Some(cancel) = &app.structural_scope_cancel
    {
        cancel.cancel();
    }
    if !enabled || editor.is_none() {
        app.cached_structural_scope_index = None;
        return;
    }
    let editor = editor.unwrap();
    if app
        .cached_structural_scope_index
        .as_ref()
        .is_some_and(|index| index.matches(editor))
    {
        return;
    }
    // One worker owns the pending snapshot until it acknowledges cancellation.
    // Cursor and viewport changes neither cancel it nor clone another request.
    if app.pending_structural_scope.is_some() {
        return;
    }
    let Some(request) = StructuralScopeRequest::from_editor(editor) else {
        app.cached_structural_scope_index = None;
        return;
    };
    if editor.modified && app.last_edit_time.elapsed() < EDITOR_ANALYSIS_EDIT_QUIET_PERIOD {
        app.editor_analysis_deferred = true;
        return;
    }
    // Retain the old Rope/index for worker-side prefix reuse. Render lookups
    // still require an exact source match, so an edited index is never drawn.
    let previous = app.cached_structural_scope_index.clone();
    let cancel = crate::background::CancellationToken::new();
    let worker_cancel = cancel.clone();
    let worker_request = request.clone();
    let sender = app.background.sender.clone();
    app.pending_structural_scope = Some(request);
    app.structural_scope_cancel = Some(cancel);
    let queued = app.background.pool_sender().send(Box::new(move || {
        let index = worker_request
            .build(previous.as_deref(), || worker_cancel.is_cancelled())
            .map(std::sync::Arc::new);
        let _ = sender.send(crate::background::BackgroundResult::StructuralScopes {
            request: worker_request,
            index,
        });
    }));
    if queued.is_err() {
        app.pending_structural_scope = None;
        app.structural_scope_cancel = None;
    }
}

fn refresh_caches(app: &mut App) {
    // Pasting can grow a previously small document beyond the configured
    // threshold without reopening it. Detect that from Rope metadata before
    // scheduling any optional analysis; this is constant-time.
    let large_file_threshold_bytes = app.large_file_threshold_mb.saturating_mul(1024 * 1024);
    if app.editor().is_some_and(|editor| {
        !editor.large_file_mode && editor.rope.len_bytes() as u64 > large_file_threshold_bytes
    }) {
        if let Some(editor) = app.editor_mut() {
            editor.update_large_file_mode(large_file_threshold_bytes);
        }
    }
    app.refresh_wrap_layout_async();
    app.refresh_visible_highlights_async();

    // Content-derived caches are computed on a worker while the UI keeps
    // rendering the previous cache. Large-file mode skips this pass, and a
    // deadline pauses rich features if an unexpectedly costly file slips
    // below the configured size threshold.
    let editor_target = app.editor_analysis_target();
    if let Some((pending, token)) = &app.pending_editor_analysis
        && editor_target.as_ref() != Some(pending)
    { token.cancel(); }
    if let Some(target) = editor_target {
        if app.cached_editor_analysis_target.as_ref() != Some(&target) {
            let same_document = reconcile_sticky_cache_document(app, &target);
            let current_line_width = app.editor().map_or(0, |editor| editor.current_line_len());
            if same_document {
                app.cached_max_line_width = app.cached_max_line_width.max(current_line_width);
            } else {
                app.cached_max_line_width = current_line_width;
                app.cached_error_count = 0;
                app.cached_warning_count = 0;
                app.cached_editor_word_count = 0;
                app.cached_word_count = None;
            }

            let edit_has_settled = !target.modified
                || app.last_edit_time.elapsed() >= EDITOR_ANALYSIS_EDIT_QUIET_PERIOD;
            if !app.bg_editor_analysis_running && edit_has_settled {
                app.editor_analysis_deferred = false;
                let (rope, saved_rope) = app.editor().map_or_else(
                    || (ropey::Rope::new(), None),
                    |editor| (editor.rope.clone(), editor.saved_rope.clone()),
                );
                let sender = app.background.sender.clone();
                let pool_sender = app.background.pool_sender();
                let content_cache = std::sync::Arc::clone(&app.content_analysis_cache);
                let document = app.editor().map_or(0, |editor| editor.navigation_id);
                let request_target = target.clone();
                let timeout =
                    std::time::Duration::from_millis(app.large_file_analysis_timeout_ms.max(100));
                app.bg_editor_analysis_running = true;
                let cancel = crate::background::CancellationToken::new();
                app.pending_editor_analysis = Some((target.clone(), cancel.clone()));
                let activity = app.background.activity.start("Analyzing", target.file_path.as_deref());
                let queued = pool_sender.send(Box::new(move || {
                    let _activity = activity.running();
                    let deadline = std::time::Instant::now() + timeout;
                    let analysis = (|| {
                        // Only this worker locks the cache. The UI transfers an
                        // Arc, never scans or clones the retained summaries.
                        let metrics = content_cache.lock().unwrap_or_else(|error| error.into_inner())
                            .analyze(document, &rope, deadline, &|| cancel.is_cancelled())?;
                        let unsaved_diff = if request_target.modified && let Some(saved) = &saved_rope {
                            crate::editor::compute_unsaved_diff_markers_cancellable(
                                saved, &rope, Some(deadline), &|| cancel.is_cancelled(),
                            )?
                        } else { crate::editor::UnsavedDiff::default() };
                        Some((metrics, unsaved_diff))
                    })();
                    let result = if cancel.is_cancelled() {
                        crate::background::BackgroundResult::EditorWorkCancelled {
                            kind: crate::background::EditorWorkKind::Metrics, token: cancel,
                        }
                    } else if let Some(((errors, warnings, max_line_width, word_count, sticky_scopes), unsaved_diff)) = analysis
                        && std::time::Instant::now() < deadline
                    {
                        crate::background::BackgroundResult::EditorAnalysis {
                            target: request_target, errors, warnings, max_line_width, word_count,
                            unsaved_diff, sticky_scopes,
                        }
                    } else {
                        crate::background::BackgroundResult::RichFeaturesTimedOut {
                            target: request_target, feature: "whole-file analysis",
                        }
                    };
                    let _ = sender.send(result);
                }));
                if queued.is_err() {
                    app.bg_editor_analysis_running = false;
                    app.pending_editor_analysis = None;
                }
            } else {
                // An in-flight analysis will trigger a redraw when it returns,
                // so only arm the timer when quiet time is the sole blocker.
                app.editor_analysis_deferred =
                    !app.bg_editor_analysis_running && !edit_has_settled;
            }
        } else {
            app.editor_analysis_deferred = false;
        }
    } else {
        app.editor_analysis_deferred = false;
        app.cached_editor_analysis_target = None;
        app.cached_error_count = 0;
        app.cached_warning_count = 0;
        app.cached_max_line_width = 0;
        app.cached_editor_word_count = 0;
        app.cached_word_count = None;
        app.cached_sticky_scopes = Default::default();
        app.cached_sticky_lines.clear();
        app.cached_sticky_key = u64::MAX;
    }

    refresh_structural_scopes(app);
    crate::diff_scope_source::refresh(app);

    let bracket_target = app.editor_analysis_target().map(|editor| {
        let (first_line, visible_lines) = app.editor().map_or((0, 50), |current| {
            (current.scroll_offset, current.viewport_height.max(50))
        });
        crate::background::BracketAnalysisTarget {
            editor,
            navigation_id: app.editor().map_or(0, |editor| editor.navigation_id),
            first_line,
            visible_lines,
        }
    });
    if let Some((pending, token)) = &app.pending_bracket_analysis
        && (!app.bracket_pair_colorization || bracket_target.as_ref().is_none_or(|target| {
            // Scrolling still needs the same document-wide index. Do not
            // cancel it merely because a different viewport is requested.
            target.editor != pending.editor || target.navigation_id != pending.navigation_id
        }))
    { token.cancel(); }
    if app.bracket_pair_colorization
        && let Some(target) = bracket_target
    {
        let current_index = app
            .cached_bracket_depth_index
            .as_ref()
            .filter(|index| app.editor().is_some_and(|editor| index.matches(editor)))
            .cloned();
        if (app.cached_bracket_analysis_target.as_ref() != Some(&target) || current_index.is_none())
            && !app.bg_bracket_analysis_running
        {
            if current_index.is_none()
                && target.editor.modified
                && app.last_edit_time.elapsed() < EDITOR_ANALYSIS_EDIT_QUIET_PERIOD
            {
                // Reuse the content-analysis timer; its due path refreshes
                // all caches without manufacturing a redraw while typing.
                app.editor_analysis_deferred = true;
            } else if let Some((rope, navigation_id, content_version, wrap_revision)) =
                app.editor().map(|editor| {
                    (
                        editor.rope.clone(),
                        editor.navigation_id,
                        editor.content_version,
                        editor.wrap_revision,
                    )
                })
            {
                let sender = app.background.sender.clone();
                let pool_sender = app.background.pool_sender();
                let request_target = target.clone();
                let timeout = Duration::from_millis(app.large_file_analysis_timeout_ms.max(100));
                let previous_index = app.cached_bracket_depth_index.clone();
                app.bg_bracket_analysis_running = true;
                let cancel = crate::background::CancellationToken::new();
                app.pending_bracket_analysis = Some((target.clone(), cancel.clone()));
                let queued = pool_sender.send(Box::new(move || {
                    let depth_index = current_index.or_else(|| {
                        crate::background::BracketDepthIndex::update_cancellable(
                            previous_index.as_deref(),
                            rope.clone(),
                            navigation_id,
                            content_version,
                            wrap_revision,
                            std::time::Instant::now() + timeout,
                            &|| cancel.is_cancelled(),
                        ).map(std::sync::Arc::new)
                    });
                    let Some(depth_index) = depth_index.filter(|_| !cancel.is_cancelled()) else {
                        let _ = sender.send(crate::background::BackgroundResult::EditorWorkCancelled {
                            kind: crate::background::EditorWorkKind::Brackets, token: cancel,
                        });
                        return;
                    };
                    let (starting_depth, guides) = analyze_viewport_brackets(
                        &rope,
                        &depth_index,
                        request_target.first_line,
                        request_target.visible_lines,
                    );
                    if cancel.is_cancelled() {
                        let _ = sender.send(crate::background::BackgroundResult::EditorWorkCancelled {
                            kind: crate::background::EditorWorkKind::Brackets, token: cancel,
                        });
                        return;
                    }
                    let _ = sender.send(crate::background::BackgroundResult::BracketAnalysis {
                        target: request_target,
                        depth_index,
                        starting_depth,
                        guides,
                    });
                }));
                if queued.is_err() {
                    app.bg_bracket_analysis_running = false;
                    app.pending_bracket_analysis = None;
                }
            }
        }
    } else {
        app.cached_bracket_analysis_target = None;
        app.cached_bracket_depth_index = None;
        app.cached_bracket_guides.clear();
        app.cached_bracket_starting_depth = 0;
    }

    // Matching pairs can be arbitrarily far apart. Compute them from a cheap
    // Rope snapshot on the worker pool and coalesce rapid cursor movement to
    // one in-flight search, instead of scanning during every rendered frame.
    let cursor_target = app.cursor_match_target();
    if let Some((pending, token)) = &app.pending_cursor_match
        && cursor_target.as_ref() != Some(pending)
    { token.cancel(); }
    match cursor_target {
        Some(target) if app.cached_cursor_match_target.as_ref() != Some(&target) => {
            // A content-version change must not make a matching pair blink out
            // for a frame. Keep the old positions only for the same editor,
            // caret and language; switching tabs or moving the caret still
            // clears them immediately in the `None`/render mismatch paths.
            let same_anchor = app
                .cached_cursor_match_target
                .as_ref()
                .is_some_and(|cached| cached.same_render_anchor(&target));
            if !same_anchor {
                app.cached_matching_bracket = None;
                app.cached_matching_tag = None;
            }
            if app.editor().is_none_or(|editor| !editor.may_have_cursor_pair()) {
                app.cached_cursor_match_target = Some(target);
                app.cached_matching_bracket = None;
                app.cached_matching_tag = None;
            } else if !app.bg_cursor_match_running {
                if let Some(rope) = app.editor().map(|editor| editor.rope.clone()) {
                    let sender = app.background.sender.clone();
                    let pool_sender = app.background.interactive_sender();
                    let request_target = target.clone();
                    app.bg_cursor_match_running = true;
                    let cancel = crate::background::CancellationToken::new();
                    app.pending_cursor_match = Some((target.clone(), cancel.clone()));
                    let queued = pool_sender.send(Box::new(move || {
                        let mut editor = crate::editor::Editor::new();
                        editor.rope = rope;
                        editor.cursor_line = request_target.cursor_line;
                        editor.cursor_col = request_target.cursor_col;
                        editor.file_path = request_target.editor.file_path.clone();
                        editor.language_override = Some(request_target.language_name.clone());
                        let bracket = editor.find_matching_bracket_cancellable(&|| cancel.is_cancelled());
                        let tag = editor.find_matching_tag_cancellable(&|| cancel.is_cancelled());
                        if cancel.is_cancelled() {
                            let _ = sender.send(crate::background::BackgroundResult::EditorWorkCancelled {
                                kind: crate::background::EditorWorkKind::Cursor, token: cancel,
                            });
                            return;
                        }
                        let _ = sender.send(crate::background::BackgroundResult::CursorMatches {
                            target: request_target,
                            bracket,
                            tag,
                        });
                    }));
                    if queued.is_err() {
                        app.bg_cursor_match_running = false;
                        app.pending_cursor_match = None;
                    }
                }
            }
        }
        None => {
            app.cached_cursor_match_target = None;
            app.cached_matching_bracket = None;
            app.cached_matching_tag = None;
        }
        Some(_) => {}
    }

    // Count only for a visible consumer. A hidden count also takes the None
    // path below to cancel obsolete scans and remove their deferred wakeup.
    // Visible counts still scan on the worker pool after cursor input settles.
    match app.word_occurrence_target() {
        Some(target) if app.cached_word_occ_target.as_ref() != Some(&target) => {
            // Preserve the status text when only the content version changed
            // and the word under the caret is still identical.
            let same_anchor = app
                .cached_word_occ_target
                .as_ref()
                .is_some_and(|cached| cached.same_render_anchor(&target));
            if !same_anchor {
                app.cached_word_occ = None;
            }
            let pending_matches = app
                .pending_word_occurrence
                .as_ref()
                .is_some_and(|(pending, _)| pending == &target);
            if !pending_matches {
                if let Some((_, token)) = app.pending_word_occurrence.take() {
                    token.cancel();
                }
                app.bg_word_occ_running = false;
                if !app.terminal_has_focus
                    || app.idle_quiescent
                    || app.last_user_activity.elapsed() < WORD_OCCURRENCE_QUIET_PERIOD
                {
                    app.word_occurrence_deferred = app.terminal_has_focus && !app.idle_quiescent;
                    return;
                }
                app.word_occurrence_deferred = false;
                let rope = app.editor().map(|editor| editor.rope.clone());
                if let Some(rope) = rope {
                    let sender = app.background.sender.clone();
                    let request_target = target.clone();
                    app.bg_word_occ_running = true;
                    let worker_target = target.clone();
                    let token = app.background.submit_cancellable(move |cancel| {
                        let Some(count) = count_word_occurrences_in_rope_cancellable(
                            &rope,
                            &worker_target.word,
                            || cancel.is_cancelled(),
                        ) else {
                            return;
                        };
                        if !cancel.is_cancelled() {
                            let _ = sender.send(
                                crate::background::BackgroundResult::WordOccurrences {
                                    target: worker_target,
                                    count,
                                },
                            );
                        }
                    });
                    app.pending_word_occurrence = Some((request_target, token));
                }
            }
        }
        None => {
            if let Some((_, token)) = app.pending_word_occurrence.take() {
                token.cancel();
            }
            app.bg_word_occ_running = false;
            app.word_occurrence_deferred = false;
            app.cached_word_occ = None;
            app.cached_word_occ_target = None;
        }
        Some(_) => {
            app.word_occurrence_deferred = false;
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
struct CaretState {
    active_tab: usize,
    cursor_line: usize,
    cursor_col: usize,
    selection_anchor: Option<(usize, usize)>,
    extra_cursor_count: usize,
    content_version: u64,
}

fn capture_caret_state(app: &App) -> Option<CaretState> {
    let editor = app.editor()?;
    Some(CaretState {
        active_tab: app.active_tab,
        cursor_line: editor.cursor_line,
        cursor_col: editor.cursor_col,
        selection_anchor: editor.selection_anchor,
        extra_cursor_count: editor.extra_cursors.len(),
        content_version: editor.content_version,
    })
}

fn relight_cursor(app: &mut App) {
    app.cursor_visible = true;
    app.last_blink = std::time::Instant::now();
}

fn note_user_activity(app: &mut App) {
    app.vtcomplete_worker.activity().note_input();
    app.last_user_activity = std::time::Instant::now();
    app.idle_quiescent = false;
    relight_cursor(app);
}

pub(crate) fn run_app<B: Backend<Error = io::Error> + io::Write>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    mut event_pump: TerminalEventPump,
    mut startup_timings: Option<crate::startup::Timings>,
) -> io::Result<()> {
    use crate::performance::diagnostics::UiPhase;
    let ui_wake = app.background.wake_handle();
    let mut last_revealed_tab: Option<usize> = None;
    let mut last_revealed_path: Option<PathBuf> = None;
    let mut last_caret_state = capture_caret_state(app);
    let mut periodic_disk_paths_cache = PeriodicDiskPaths::default();
    let mut last_terminal_title = String::new();
    let mut last_title_key: (u64, bool) = (u64::MAX, false); // (view identity, modified)
    let mut needs_redraw = true;
    let mut input_redraw = true;
    let mut chat_redraw_pending = false;
    let mut service_scheduler = crate::ui_work::Scheduler::default();
    let mut startup_assets_requested = false;
    let mut input_started: Option<std::time::Instant> = None;
    // Keep dispatch and its first presentation in one scheduling scope. A
    // temporary boost ending after mouse handling used to surrender the CPU
    // between updating the selection and drawing it, precisely under load.
    let mut input_priority: Option<crate::worker_priority::InputPriority> = None;
    let mut selection_drag = None;
    // Explorer decorations are populated by the Git worker; constructing or
    // refreshing the tree never runs a repository status scan on the UI.
    app.refresh_explorer_async();
    app.refresh_source_control_async();
    app.refresh_git_branch_async();
    loop {
        if let Some(started) = input_started.take() {
            app.background.activity.ui.input_handled(started, std::time::Instant::now(), needs_redraw && app.terminal_has_focus);
        }
        // Observe before polling *any* producer. A PTY/LSP notification can
        // arrive after its queue was polled but before we reach the wait below.
        // Taking a fresh snapshot there would forget that work and sleep until
        // an unrelated event or cosmetic timer wakes us again.
        let observed_wake = service_scheduler.observe_wake(ui_wake.generation());
        if (input_redraw && needs_redraw) || event_pump.has_pending_event() {
            app.vtcomplete_worker.activity().note_input();
        } else {
            needs_redraw |= app.poll_completion_index();
        }
        #[cfg(windows)]
        if let Some(input) = &mut event_pump.windows_mouse {
            input.poll_ready();
            input.focus.set_enabled(app.mouse_back_forward_to_navigate);
        }
        let current_caret_state = capture_caret_state(app);
        crate::app_explorer_hover::poll(app, std::time::Instant::now());
        if current_caret_state != last_caret_state {
            relight_cursor(app);
            needs_redraw = true;
            last_caret_state = current_caret_state;
        }

        // Ordinary edits update Find incrementally. Pathological or very wide
        // edits are coalesced into one background scan after typing settles.
        if app.refresh_pending_file_search() {
            needs_redraw = true;
        }
        if app.poll_pending_global_search(std::time::Instant::now()) {
            needs_redraw = true;
        }

        // One shared wall-clock budget, interrupted by queued input. Giving
        // every producer its own full budget used to add up before a key or
        // mouse event could be handled, especially when the OS was busy.
        let agent_was_running = app.chat.any_agent_running();
        let mut agent_changed = false;
        let mut service_turn = crate::ui_work::Turn::new(std::time::Instant::now());
        while let Some(service) = service_scheduler.next(
            &mut service_turn,
            std::time::Instant::now(),
            (input_redraw && needs_redraw) || !app.macro_replay_queue.is_empty() || event_pump.has_pending_event(),
        ) {
            use crate::ui_work::Service;
            let service_started = std::time::Instant::now();
            match service {
                Service::LanguageServers => crate::app_lsp_poll::poll_lsp_until(app, &mut needs_redraw, service_turn.deadline),
                Service::Workers => {
                    needs_redraw |= app.poll_background_results_until(service_turn.deadline);
                    needs_redraw |= crate::diff_scope_source::poll(app);
                }
                Service::Terminals => {
                    needs_redraw |= app.process_terminals_until(service_turn.deadline, || {
                        !event_pump.has_pending_event()
                    });
                }
                Service::Agents => {
                    agent_changed = app.poll_acp_until(service_turn.deadline);
                    needs_redraw |= agent_changed;
                }
                Service::Extensions => {
                    needs_redraw |= app.apply_extension_text_edits();
                    crate::extension_scheduler::dispatch(app);
                }
            }
            let finished = std::time::Instant::now();
            let phase = match service {
                Service::LanguageServers => UiPhase::LanguageServers,
                Service::Workers => UiPhase::Workers,
                Service::Terminals => UiPhase::Terminals,
                Service::Agents => UiPhase::Agents,
                Service::Extensions => UiPhase::Extensions,
            };
            app.background.activity.ui.record(phase, finished.saturating_duration_since(service_started), finished);
        }
        if crate::ui_chat::advance_activity_frame(app) {
            needs_redraw = true;
        }
        if app.commit_details.expire_copy_feedback(std::time::Instant::now())
            && app.commit_details.visible
        {
            needs_redraw = true;
        }
        app.finish_navigation();

        if app.promote_modified_preview_tabs() {
            needs_redraw = true;
        }

        // Cosmetic ticks do not invalidate text/layout/provider caches. Keep
        // this separate from real changes through the draw decision below.
        let cosmetic_allowed = app.terminal_has_focus
            && (needs_redraw
                || !app.animation_settings.any_enabled()
                || (app.macro_replay_queue.is_empty() && !event_pump.has_pending_event()));
        let animation_redraw = cosmetic_allowed && app
                .animations
                .advance_frame(&app.animation_settings, std::time::Instant::now());
        let loading_allowed = app.loading.active() && app.terminal_has_focus && (needs_redraw
            || (app.macro_replay_queue.is_empty() && !event_pump.has_pending_event()));
        let loading_redraw = loading_allowed && app.loading.advance(
            app.terminal_has_focus, std::time::Instant::now(),
        );
        let copy_redraw = app.terminal_has_focus
            && (needs_redraw || (app.macro_replay_queue.is_empty() && !event_pump.has_pending_event()))
            && app.advance_copy_animation(std::time::Instant::now());
        let animation_redraw = animation_redraw || loading_redraw || copy_redraw;
        if app.sync_markdown_preview_state() {
            needs_redraw = true;
        }

        // Whole-file metrics are intentionally delayed while typing. Queue
        // them when their timer expires without repainting an unchanged UI.
        if !needs_redraw
            && (deferred_editor_analysis_is_due(app)
                || deferred_word_occurrence_is_due(app))
        {
            // Long cosmetic effects must not postpone due analysis work.
            // A cache refresh can alter render inputs, so this one frame
            // follows the ordinary rendering path.
            if animation_redraw {
                needs_redraw = true;
            } else {
                refresh_caches(app);
            }
        }

        // Keep agent output live in an unfocused terminal, including its final
        // completion frame. Idle workbenches still suspend background redraws.
        chat_redraw_pending |= agent_changed;
        if !chat_redraw_pending && !background_chat_redraw(app, agent_was_running, agent_changed) {
            needs_redraw = false;
        }

        let activity_now = std::time::Instant::now();
        app.background.refresh_activity(activity_now);
        needs_redraw |= crate::ui_performance_details::poll_resources(app, activity_now);
        if app.terminal_has_focus
            && (app.background.activity.details_open || (app.show_status_bar && !app.zen_mode))
        {
            needs_redraw |= app.background.activity.needs_redraw(activity_now)
                || crate::ui_performance_details::timings_need_redraw(app);
        }

        let background_frame_wait = app.animations.background_frame_wait(std::time::Instant::now());
        let draw_changed = needs_redraw && (input_redraw
            || (!event_pump.has_pending_event() && background_frame_wait.is_zero()));
        if draw_changed || (!needs_redraw && animation_redraw) {
            let _foreground = app.vtcomplete_worker.activity().present();
            let render_priority = crate::worker_priority::InputPriority::new(input_redraw && app.terminal_has_focus);
            let frame_started = std::time::Instant::now();
            app.rendering_animation_frame = !needs_redraw;
            if needs_redraw {
                app.sync_duplicate_file_tabs();
                app.ensure_active_tab_visible();
                app.prepare_visible_merge_conflicts();
                refresh_caches(app);
            }

            // Update terminal title with current file name (only when changed)
            {
                let title_key = app
                    .tabs.get(app.active_tab)
                    .map_or((u64::MAX, false), |tab| (tab.editor.navigation_id, tab.has_unsaved_changes()));
                if title_key != last_title_key {
                    last_title_key = title_key;
                    let title = if let Some(tab) = app.tabs.get(app.active_tab) {
                        let name = tab.editor.file_name();
                        let modified = if tab.has_unsaved_changes() { "● " } else { "" };
                        format!("{modified}{name} — vtcode")
                    } else {
                        "vtcode".to_string()
                    };
                    if title != last_terminal_title {
                        let _ = execute!(
                            terminal.backend_mut(),
                            crossterm::terminal::SetTitle(&title)
                        ); // Error OK: terminal title is best-effort
                        last_terminal_title = title;
                    }
                }
            }

            // Auto-reveal active file in explorer when tab changes
            if app.show_explorer && app.sidebar_view == crate::app::SidebarView::Explorer {
                let tab_changed = Some(app.active_tab) != last_revealed_tab;
                let path_changed = tab_changed || {
                    let current = app.editor().and_then(|e| e.file_path.as_ref());
                    current != last_revealed_path.as_ref()
                };
                if path_changed {
                    let current_path = app.editor().and_then(|e| e.file_path.clone());
                    last_revealed_tab = Some(app.active_tab);
                    if let Some(ref path) = current_path {
                        if app.explorer.reveal_file_centered_deferred(path) {
                            app.refresh_explorer_tree_async();
                        }
                    }
                    last_revealed_path = current_path;
                }
            }

            #[cfg(test)]
            pointer_event_loop_latency_tests::before_paint(app);
            if input_redraw && selection_drag.is_some() && !reference_input_batches() {
                let started = std::time::Instant::now();
                if refresh_selection_before_paint(app, &mut event_pump, &mut selection_drag) {
                    last_caret_state = capture_caret_state(app);
                    // Cursor-dependent analysis may have been queued for the
                    // old position. Reconcile it using the normal cache path.
                    refresh_caches(app);
                    let finished = std::time::Instant::now();
                    app.background.activity.ui.input_handled(started, finished, true);
                    #[cfg(all(test, windows))]
                    pointer_event_loop_latency_tests::pointer_updated();
                }
            }
            let render_started = std::time::Instant::now();
            app.background.activity.ui.record(UiPhase::Preparation, render_started.saturating_duration_since(frame_started), render_started);
            let mut painting_elapsed = Duration::ZERO;
            terminal.draw(|f| {
                let painting_started = std::time::Instant::now();
                ui::draw(f, app);
                #[cfg(all(test, windows))]
                pointer_event_loop_latency_tests::paint_marker(f, app);
                painting_elapsed = painting_started.elapsed();
            })?;
            let render_finished = std::time::Instant::now();
            #[cfg(test)]
            if pointer_event_loop_latency_tests::presented(app) { return Ok(()); }
            drop(render_priority);
            input_priority.take();
            let render_elapsed = render_finished.saturating_duration_since(render_started);
            app.background.activity.ui.record(UiPhase::Painting, painting_elapsed, render_finished);
            app.background.activity.ui.record(UiPhase::TerminalOutput,
                render_elapsed.saturating_sub(painting_elapsed), render_finished);
            // Preparation competes with input too; include it in pacing, but
            // keep the render performance display measuring the draw itself.
            let frame_elapsed = render_finished.saturating_duration_since(frame_started);
            app.animations.record_render(render_finished, frame_elapsed);
            app.loading.record_render(render_finished, frame_elapsed);
            app.rendering_animation_frame = false;
            app.background
                .activity
                .render
                .record(render_elapsed);
            app.background.activity.ui.draw_completed(render_elapsed, render_finished);
            app.background.activity.mark_drawn(render_started);
            if !startup_assets_requested {
                if let Some(mut timings) = startup_timings.take() {
                    timings.mark("workbench_frame");
                    timings.finish();
                }
                // Only start optional services once the editor is visible and
                // input is running. File/recovery loads use a separate I/O queue.
                app.request_external_themes_async();
                app.request_syntax_assets_async();
                app.initialize_extensions();
                if let Some(remote) = &app.remote_workspace { let _ = remote.client.notify(crate::remote::Command::Watch); }
                else { app.background.start_file_watcher(app.explorer.root.clone()); }
                startup_assets_requested = true;
            }
            // Drawing establishes the exact current viewport width. Queueing
            // here avoids waiting for another input event after a resize or
            // the first frame, while row-map construction stays off-thread.
            if needs_redraw {
                app.refresh_wrap_layout_async();
            }

            needs_redraw = false;
            input_redraw = false;
            chat_redraw_pending = false;
        }

        // Handle mouse-triggered exit (from clicking exit confirm buttons)
        if (app.exit_save_and_quit || app.exit_keep_unsaved_quit) && !app.custom_exit_started {
            app.start_custom_exit(app.exit_keep_unsaved_quit);
        }
        if app.exit_discard_quit && !app.custom_exit_started { app.start_custom_discard_exit(); }
        if app.custom_exit_started && app.custom_exit_remaining == 0 && app.custom_exit_failed {
            app.exit_save_and_quit = false;
            app.exit_keep_unsaved_quit = false;
            app.exit_discard_quit = false;
            app.custom_exit_started = false;
            app.exit_confirm_active = true;
            app.notify_error("Could not save or preserve a custom document. Your edits remain open; resolve the error before quitting.");
        }
        let text_saves_done = app.pending_file_saves.is_empty() && app.deferred_file_saves.is_empty();
        if app.exit_save_and_quit && app.custom_exit_remaining == 0 && text_saves_done && !app.shutdown_anim_active {
            match crate::input_dialogs::save_text_documents_before_exit(app) {
                Ok(()) => {
                    app.shutdown_anim_active = true;
                    app.shutdown_anim_start = std::time::Instant::now();
                }
                Err(error) => {
                    if app.remote_workspace.is_some() && !app.pending_file_saves.is_empty() {
                        needs_redraw = true;
                        continue;
                    }
                    app.exit_save_and_quit = false;
                    app.custom_exit_started = false;
                    app.exit_confirm_active = true;
                    if app.save_error.is_none() { app.show_save_error(error); }
                    needs_redraw = true;
                }
            }
        }
        if app.exit_discard_quit && app.custom_exit_remaining == 0 && text_saves_done && !app.shutdown_anim_active {
            let unsaved_path = crate::session::unsaved_dir(&app.explorer.root);
            let _ = std::fs::remove_dir_all(&unsaved_path); // Error OK: dir may not exist
            app.shutdown_anim_active = true;
            app.shutdown_anim_start = std::time::Instant::now();
        }
        if app.exit_keep_unsaved_quit && app.custom_exit_remaining == 0 && text_saves_done && !app.shutdown_anim_active {
            if app.exit_recovery.saved {
                app.shutdown_anim_active = true;
                app.shutdown_anim_start = std::time::Instant::now();
            } else if !app.exit_recovery.pending() {
                app.start_exit_recovery();
                needs_redraw = true;
            }
        }

        // Shutdown fade-to-black animation
        if app.shutdown_anim_active {
            let elapsed = app.shutdown_anim_start.elapsed().as_millis() as u64;
            if elapsed >= 500 {
                return Ok(());
            }
            let progress = (elapsed as f64 / 500.0).min(1.0);
            terminal.draw(|f| {
                let area = f.area();
                // Draw normal UI first
                crate::ui::draw(f, app);
                // Fade: darken all cells toward black
                let inv = 1.0 - progress;
                for y in area.y..area.y + area.height {
                    for x in area.x..area.x + area.width {
                        if let Some(cell) = f
                            .buffer_mut()
                            .cell_mut(ratatui::layout::Position::new(x, y))
                        {
                            let fade_color = |c: ratatui::style::Color| -> ratatui::style::Color {
                                let (r, g, b) = match c {
                                    ratatui::style::Color::Rgb(r, g, b) => (r, g, b),
                                    ratatui::style::Color::Black => (0, 0, 0),
                                    ratatui::style::Color::Red
                                    | ratatui::style::Color::LightRed => (200, 50, 50),
                                    ratatui::style::Color::Green
                                    | ratatui::style::Color::LightGreen => (50, 200, 50),
                                    ratatui::style::Color::Yellow
                                    | ratatui::style::Color::LightYellow => (200, 200, 50),
                                    ratatui::style::Color::Blue
                                    | ratatui::style::Color::LightBlue => (50, 50, 200),
                                    ratatui::style::Color::Magenta
                                    | ratatui::style::Color::LightMagenta => (200, 50, 200),
                                    ratatui::style::Color::Cyan
                                    | ratatui::style::Color::LightCyan => (50, 200, 200),
                                    ratatui::style::Color::White
                                    | ratatui::style::Color::Gray
                                    | ratatui::style::Color::DarkGray => (180, 180, 180),
                                    _ => (0, 0, 0),
                                };
                                ratatui::style::Color::Rgb(
                                    (f64::from(r) * inv) as u8,
                                    (f64::from(g) * inv) as u8,
                                    (f64::from(b) * inv) as u8,
                                )
                            };
                            let new_bg = fade_color(cell.bg);
                            let new_fg = fade_color(cell.fg);
                            cell.set_bg(new_bg);
                            cell.set_fg(new_fg);
                        }
                    }
                }
                // Center "Goodbye!" text after 30%
                if progress > 0.3 {
                    let text_bright = ((progress - 0.3) / 0.7 * 200.0).min(200.0) as u8;
                    let msg = "Goodbye!";
                    let msg_w = unicode_width::UnicodeWidthStr::width(msg) as u16;
                    let msg_x = area.width / 2 - msg_w / 2;
                    let msg_y = area.height / 2;
                    let p = ratatui::widgets::Paragraph::new(msg).style(
                        ratatui::style::Style::default()
                            .fg(ratatui::style::Color::Rgb(
                                text_bright,
                                text_bright,
                                text_bright,
                            ))
                            .bg(ratatui::style::Color::Rgb(0, 0, 0)),
                    );
                    f.render_widget(p, ratatui::layout::Rect::new(msg_x, msg_y, msg_w, 1));
                }
            })?;
            std::thread::sleep(std::time::Duration::from_millis(16));
            continue;
        }

        // A focused terminal can remain untouched for hours. Converge all
        // cosmetic/maintenance timers after one quiet-period deadline, then
        // leave the shared wake condition blocked until real input or work.
        if app.terminal_has_focus
            && !app.idle_quiescent
            && app.last_user_activity.elapsed() >= FOREGROUND_IDLE_QUIET_PERIOD
        {
            app.idle_quiescent = true;
            if !app.cursor_visible {
                app.cursor_visible = true;
                needs_redraw = true;
            }
        }

        // Load only for visible blame consumers: an explicit rail immediately,
        // or an inline annotation after input settles. Cached snapshots have
        // no timer and need no probe unless their inputs change.
        refresh_blame_cache(app);

        // Native watcher coverage needs no metadata polling. Keep a low-rate
        // fallback only for files outside the workspace, private Git metadata,
        // or platforms where watcher startup failed.
        let periodic_disk_paths = periodic_disk_paths_cache.get(app);
        if app.terminal_has_focus
            && !app.idle_quiescent
            && app.disk_check_last.elapsed() >= UNWATCHED_DISK_CHECK_INTERVAL
            && !periodic_disk_paths.is_empty()
        {
            let idx = app.disk_check_index % periodic_disk_paths.len();
            app.disk_check_index = idx + 1;
            app.queue_file_disk_checks(std::iter::once(periodic_disk_paths[idx].clone()));
            app.disk_check_last = std::time::Instant::now();
        }

        // Cursor blink should be time-based and independent of mouse movement/events.
        // Only blink when a text input context is focused (editor file tab, palette, search, etc.)
        let should_blink = app.terminal_has_focus
            && !app.idle_quiescent
            && app.cursor_blinking
            && app.has_text_input_focus();
        if should_blink && app.last_blink.elapsed().as_millis() >= 500 {
            app.cursor_visible = !app.cursor_visible;
            app.last_blink = std::time::Instant::now();
            needs_redraw = true;
        } else if !should_blink && !app.cursor_visible {
            app.cursor_visible = true;
            needs_redraw = true;
        }

        // Ctrl+K chord timeout (2 seconds)
        if app.chord_pending && app.chord_start.elapsed().as_secs() >= 2 {
            app.chord_pending = false;
            app.notify("Chord timed out");
            needs_redraw = true;
        }

        // Auto-save: save modified files after configured delay of inactivity
        if matches!(app.auto_save, crate::app::AutoSaveMode::AfterDelay)
            && app.last_edit_time.elapsed().as_millis() >= u128::from(app.auto_save_delay)
        {
            queue_due_delayed_auto_saves(app);
        }

        // Process macro replay queue or poll for new events
        needs_redraw |= app.update_file_deletion_timer();
        app.background.refresh_activity(std::time::Instant::now());
        if !needs_redraw {
            input_priority.take();
        }
        let replaying_macro = !app.macro_replay_queue.is_empty();
        let key_event: Option<crossterm::event::KeyEvent> = if !replaying_macro {
            // Every asynchronous producer signals `ui_wake`. With no queued
            // result, input, animation, or genuine deadline, this wait blocks
            // rather than periodically walking the application state.
            let wait = next_event_wait_for_redraw(app, needs_redraw, input_redraw, !periodic_disk_paths.is_empty());
            let mut next_event = event_pump.try_recv()?;
            if next_event.is_none() && !service_turn.deferred() && !app.background.has_pending_results() {
                ui_wake.wait_for_change(observed_wake, wait);
                next_event = event_pump.try_recv()?;
            }
            if next_event.is_none() {
                // Redraw only when a timed overlay actually reaches its expiry.
                let screencast_key_expired = app.screencast_mode
                    && app
                        .screencast_keys
                        .iter()
                        .any(|(_, created)| created.elapsed().as_millis() >= 3_000);
                let tab_tooltip_due = !app.tab_tooltip_shown
                    && app.tab_tooltip_tab.is_some()
                    && app
                        .tab_tooltip_hover_start
                        .is_some_and(|start| start.elapsed() >= TAB_TOOLTIP_DELAY);
                let saving_flash_expired = app
                    .saving_flash
                    .is_some_and(|start| start.elapsed() >= SAVING_FLASH_LIFETIME);
                if app.terminal_has_focus
                    && (screencast_key_expired
                        || tab_tooltip_due
                        || crate::ui_status::hover_due(app)
                        || crate::ui_inline_blame::hover_due(app)
                        || saving_flash_expired)
                {
                    needs_redraw = true;
                }
                continue;
            }

            let next_event = next_event.expect("terminal event checked above");
            selection_drag = None;
            crate::terminal_output::apply_resize(terminal, &next_event)?;
            if matches!(&next_event, Event::Key(key) if key.kind != KeyEventKind::Release && !matches!(key.code, KeyCode::Modifier(_)))
                || matches!(&next_event, Event::Mouse(_) | Event::Paste(_))
            {
                input_started = Some(std::time::Instant::now());
                if !matches!(&next_event, Event::Mouse(mouse) if mouse.kind == crossterm::event::MouseEventKind::Moved) {
                    input_priority.get_or_insert_with(|| {
                        crate::worker_priority::InputPriority::new(app.terminal_has_focus)
                    });
                }
            }
            input_redraw = true;
            if !matches!(&next_event, Event::FocusLost) {
                note_user_activity(app);
            }
            match next_event {
                Event::Key(key) => {
                    needs_redraw |= app.update_link_hover_key(key);
                    if key.kind == KeyEventKind::Release || matches!(key.code, KeyCode::Modifier(_)) {
                        continue;
                    }
                    Some(key)
                }
                Event::Mouse(mouse) => {
                    let changed = handle_mouse_input(app, &mut event_pump, mouse);
                    let mouse = crossterm::event::MouseEvent {
                        column: app.mouse_col, row: app.mouse_row, ..mouse
                    };
                    selection_drag = editor_selection_drag_state(app, mouse)
                        .map(|state| (mouse, state));
                    #[cfg(all(test, windows))]
                    pointer_event_loop_latency_tests::pointer_updated();
                    needs_redraw |= changed;
                    if changed {
                        input_priority.get_or_insert_with(|| {
                            crate::worker_priority::InputPriority::new(app.terminal_has_focus)
                        });
                    } else if !needs_redraw {
                        input_priority.take();
                    }
                    continue;
                }
                Event::FocusGained => {
                    app.terminal_has_focus = true;
                    app.forward_terminal_focus(true);
                    // Slow/network filesystem metadata must not delay the first
                    // frame after returning to the terminal.
                    let mut paths = rustc_hash::FxHashSet::default();
                    for tab in &app.tabs {
                        if let Some(path) = tab.editor.file_path.as_ref() {
                            paths.insert(path.clone());
                        }
                    }
                    for group in &app.groups {
                        for tab in &group.tabs {
                            if let Some(path) = tab.editor.file_path.as_ref() {
                                paths.insert(path.clone());
                            }
                        }
                    }
                    app.queue_file_disk_checks(paths);
                    needs_redraw = true;
                    continue;
                }
                Event::FocusLost => {
                    app.dismiss_vtcomplete();
                    app.background.activity.ui.cancel_input();
                    app.terminal_has_focus = false;
                    app.inline_blame_hover = None;
                    app.update_link_hover_modifiers(KeyModifiers::NONE);
                    app.animations.clear();
                    app.copy_animation.clear();
                    app.forward_terminal_focus(false);
                    app.idle_quiescent = true;
                    app.cursor_visible = true;
                    if let Some((_, token)) = app.pending_word_occurrence.take() {
                        token.cancel();
                    }
                    app.bg_word_occ_running = false;
                    app.word_occurrence_deferred = false;
                    app.auto_save_on_window_change();
                    needs_redraw = true;
                    continue;
                }
                Event::Paste(text) => {
                    handle_paste_input_value(app, std::borrow::Cow::Owned(text));
                    needs_redraw = true;
                    continue;
                }
                Event::PasteStatus(status) => {
                    app.terminal_paste_status(status);
                    needs_redraw = true;
                    continue;
                }
                _ => {
                    needs_redraw = true;
                    continue;
                }
            }
        } else {
            selection_drag = None;
            input_started = Some(std::time::Instant::now());
            input_priority.get_or_insert_with(|| {
                crate::worker_priority::InputPriority::new(app.terminal_has_focus)
            });
            input_redraw = true;
            note_user_activity(app);
            app.macro_replay_queue.pop_front()
        };

        // Any event processed below needs a redraw
        needs_redraw = true;

        let Some(key) = key_event else {
            continue;
        };

        let quit = if replaying_macro {
            input_keyboard::handle_keyboard_event(app, key)
        } else {
            handle_keyboard_input(app, &mut event_pump, key)
        };
        if quit {
            return Ok(());
        }
    }
}
