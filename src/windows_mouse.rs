//! Windows Terminal drops XBUTTON and Shift+mouse events before console/VT
//! input. Capture those gestures while this terminal pane has focus.
//! The hook consumes the original gesture, preventing duplicate navigation if
//! a future terminal version starts forwarding it. Other windows, inactive
//! panes and terminals that already report these buttons use normal input.
//! Ordinary left clicks and drags retain the terminal's own coordinates.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering};
use crate::terminal_input_batch::Sender;

use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GA_ROOT, GetAncestor, GetClassNameW, GetForegroundWindow,
    GetMessageW, MSG, MSLLHOOKSTRUCT, PM_NOREMOVE, PeekMessageW, PostThreadMessageW,
    SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, WH_MOUSE_LL, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN, WM_XBUTTONUP, WindowFromPoint,
};

#[path = "windows_mouse_geometry.rs"]
mod geometry;

#[derive(Default)]
pub(crate) struct MouseFocus {
    enabled: AtomicBool,
    host: AtomicIsize,
    geometry: geometry::Tracker,
    suppressed_buttons: AtomicU8,
    native_pointer: Mutex<NativePointer>,
    native_left_down: AtomicBool,
    native_reference_pending: AtomicBool,
}

#[derive(Clone, Copy)]
struct NativePress {
    id: u64,
    observed_at: std::time::Instant,
    point: windows_sys::Win32::Foundation::POINT,
    grid: Option<geometry::TextGrid>,
    modifiers: KeyModifiers,
}

struct NativePointer {
    next_id: u64,
    pressed: Option<u64>,
    pending: VecDeque<NativePress>,
    discarded: usize,
    reference: Option<(NativePress, (u16, u16))>,
    reference_ordered: bool,
    decoded_press_at: Option<std::time::Instant>,
}

impl Default for NativePointer {
    fn default() -> Self {
        Self {
            next_id: 0,
            pressed: None,
            pending: VecDeque::with_capacity(16),
            discarded: 0,
            reference: None,
            reference_ordered: false,
            decoded_press_at: None,
        }
    }
}

impl MouseFocus {
    pub(crate) fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
    }

    pub(crate) fn set_focused(&self, focused: bool) {
        let host = if focused {
            // Every routed key/pointer sample confirms pane ownership. The
            // usual case needs no window-class query or UTF-16 allocation.
            let foreground = unsafe { GetForegroundWindow() as isize };
            if foreground != 0 && self.host.load(Ordering::Acquire) == foreground {
                return;
            }
            terminal_window(foreground)
        } else { 0 };
        if self.host.swap(host, Ordering::AcqRel) != host {
            self.geometry.invalidate();
            self.reset_native_pointer();
        }
    }

    pub(crate) fn resized(&self) {
        self.geometry.invalidate();
        self.reset_native_pointer();
    }

    fn reset_native_pointer(&self) {
        self.native_left_down.store(false, Ordering::Release);
        self.native_reference_pending.store(false, Ordering::Release);
        if let Ok(mut pointer) = self.native_pointer.lock() {
            pointer.pressed = None;
            pointer.pending.clear();
            pointer.discarded = 0;
            pointer.reference = None;
            pointer.reference_ordered = false;
            pointer.decoded_press_at = None;
        }
    }

    fn native_press(
        &self,
        event: &MSLLHOOKSTRUCT,
        grid: Option<geometry::TextGrid>,
        modifiers: KeyModifiers,
    ) {
        // The hook never waits for the input reader. Keep physical presses in
        // order so delayed console reports cannot calibrate a newer gesture.
        self.native_left_down.store(false, Ordering::Release);
        let Ok(mut pointer) = self.native_pointer.try_lock() else {
            return;
        };
        pointer.next_id = pointer.next_id.wrapping_add(1);
        let id = pointer.next_id;
        pointer.pressed = Some(id);
        if pointer.discarded > 0 || pointer.pending.len() == 16 {
            pointer.discarded = pointer.discarded.saturating_add(1);
        } else {
            pointer.pending.push_back(NativePress {
                id,
                observed_at: std::time::Instant::now(),
                point: event.pt,
                grid,
                modifiers,
            });
        }
        self.native_left_down.store(true, Ordering::Release);
    }

    fn native_release(&self) {
        self.native_left_down.store(false, Ordering::Release);
        if let Ok(mut pointer) = self.native_pointer.try_lock() {
            pointer.pressed = None;
        }
    }

    fn pointer_grid(
        &self,
        grid: Option<geometry::TextGrid>,
        continuing_native: bool,
    ) -> Option<geometry::TextGrid> {
        if continuing_native && !self.native_left_down.load(Ordering::Acquire) {
            return None;
        }
        let grid = grid?;
        let Ok(pointer) = self.native_pointer.try_lock() else {
            return (!continuing_native).then_some(grid);
        };
        let Some((press, position)) = pointer.reference else {
            return (!continuing_native).then_some(grid);
        };
        if continuing_native && pointer.pressed != Some(press.id) {
            return None;
        }
        if continuing_native && !pointer.reference_ordered { return None; }
        press
            .grid
            .and_then(|reference| grid.with_native_reference(reference, press.point, position))
            .or_else(|| (!continuing_native).then_some(grid))
    }

    /// Terminal may still emit drags after a swallowed press (especially when
    /// Shift is released first). Suppress that stream through its next press,
    /// including packets decoded after the native release reached our hook.
    pub(crate) fn stage_native_mouse(&self, event: &MouseEvent) -> bool {
        let (button, press) = match event.kind {
            MouseEventKind::Down(button) => (button, true),
            MouseEventKind::Drag(button) | MouseEventKind::Up(button) => (button, false),
            _ => return false,
        };
        let mask = pointer_mask(button);
        if press {
            self.suppressed_buttons.fetch_and(!mask, Ordering::AcqRel);
            if button == MouseButton::Left
                && let Ok(mut pointer) = self.native_pointer.lock()
            {
                // Terminal may consume Ctrl+click on its own hyperlinks.
                // Such a press must not displace a later ordinary report.
                while pointer
                    .pending
                    .front()
                    .is_some_and(|physical| physical.modifiers != event.modifiers)
                {
                    pointer.pending.pop_front();
                }
                if let Some(physical) = pointer.pending.pop_front() {
                    // Retain the physical-to-terminal delay even if the
                    // physical release arrived before this decoded press.
                    // An earlier click on Terminal's own chrome may never
                    // produce a console press. Do not label that stale FIFO
                    // entry as the current press's delivery latency.
                    pointer.decoded_press_at = (physical.id == pointer.next_id)
                        .then_some(physical.observed_at);
                    self.native_reference_pending.store(true, Ordering::Release);
                    if pointer.pressed == Some(physical.id) && physical.modifiers == event.modifiers
                    {
                        pointer.reference = Some((physical, (event.column, event.row)));
                        pointer.reference_ordered = false;
                    }
                } else {
                    pointer.discarded = pointer.discarded.saturating_sub(1);
                }
            }
            false
        } else {
            if button == MouseButton::Left
                && matches!(event.kind, MouseEventKind::Up(_))
                && let Ok(mut pointer) = self.native_pointer.lock()
                && pointer
                    .reference
                    .is_some_and(|(physical, _)| pointer.pressed == Some(physical.id))
            {
                self.native_left_down.store(false, Ordering::Release);
                pointer.pressed = None;
            }
            self.suppressed_buttons.load(Ordering::Acquire) & mask != 0
        }
    }

    pub(crate) fn order_native_mouse(&self) -> Option<std::time::Instant> {
        if !self.native_reference_pending.swap(false, Ordering::AcqRel) { return None; }
        let mut pointer = self.native_pointer.lock().ok()?;
        pointer.reference_ordered = true;
        pointer.decoded_press_at.take()
    }

    #[cfg(test)]
    fn filter_native_mouse(&self, event: &MouseEvent) -> bool {
        // Existing hook fixtures publish one terminal event at a time.
        let filtered = self.stage_native_mouse(event);
        let _ = self.order_native_mouse();
        filtered
    }
}

fn foreground_terminal() -> isize {
    terminal_window(unsafe { GetForegroundWindow() as isize })
}

fn terminal_window(window: isize) -> isize {
    // SAFETY: this reads the foreground HWND and copies its class name into a
    // fixed writable buffer; no window is modified or owned by this function.
    unsafe {
        let mut class = [0u16; 128];
        let len = GetClassNameW(window as _, class.as_mut_ptr(), class.len() as i32);
        let class = &class[..len.max(0) as usize];
        if class.iter().copied().eq("CASCADIA_HOSTING_WINDOW_CLASS".encode_utf16())
            || class.iter().copied().eq("WindowsTerminalDesktopWindow".encode_utf16())
        {
            window
        } else {
            0
        }
    }
}

struct HookContext {
    sender: Sender,
    wake: crate::background::UiWake,
    focus: Arc<MouseFocus>,
    captured_buttons: u8,
    pointer: PointerCapture,
    wheel_remainder: [i32; 2],
}

#[derive(Default)]
struct PointerCapture {
    buttons: u8,
    grid: Option<geometry::TextGrid>,
    position: (u16, u16),
    active: bool,
    native_press: bool,
}

fn pointer_mask(button: MouseButton) -> u8 {
    match button {
        MouseButton::Left => 1,
        MouseButton::Middle => 2,
        MouseButton::Right => 4,
        _ => 0,
    }
}

fn pointer_button(message: u32) -> Option<(MouseButton, bool)> {
    match message {
        WM_LBUTTONDOWN => Some((MouseButton::Left, true)),
        WM_LBUTTONUP => Some((MouseButton::Left, false)),
        WM_MBUTTONDOWN => Some((MouseButton::Middle, true)),
        WM_MBUTTONUP => Some((MouseButton::Middle, false)),
        WM_RBUTTONDOWN => Some((MouseButton::Right, true)),
        WM_RBUTTONUP => Some((MouseButton::Right, false)),
        _ => None,
    }
}

fn current_modifiers() -> KeyModifiers {
    let mut modifiers = KeyModifiers::NONE;
    for (key, modifier) in [
        (VK_SHIFT, KeyModifiers::SHIFT),
        (VK_CONTROL, KeyModifiers::CONTROL),
        (VK_MENU, KeyModifiers::ALT),
    ] {
        // SAFETY: query only the current down bit, not the global pressed bit.
        if unsafe { GetAsyncKeyState(i32::from(key)) } < 0 {
            modifiers |= modifier;
        }
    }
    modifiers
}

thread_local! {
    static CONTEXT: RefCell<Option<HookContext>> = const { RefCell::new(None) };
}

fn may_capture(enabled: bool, host: isize, foreground: isize) -> bool {
    enabled && host != 0 && host == foreground
}

impl HookContext {
    fn send_mouse(
        &self,
        kind: MouseEventKind,
        position: (u16, u16),
        modifiers: KeyModifiers,
    ) -> bool {
        if self
            .sender
            .send(Ok(Event::Mouse(MouseEvent {
                kind,
                column: position.0,
                row: position.1,
                modifiers,
            })))
            .is_err()
        {
            return false;
        }
        self.wake.notify();
        true
    }

    fn handle_pointer(
        &mut self,
        message: u32,
        event: &MSLLHOOKSTRUCT,
        foreground: isize,
        modifiers: KeyModifiers,
        grid: Option<geometry::TextGrid>,
    ) -> bool {
        let focused = may_capture(true, self.focus.host.load(Ordering::Acquire), foreground)
            && self.focus.geometry.permits_capture();
        if self.pointer.active && !focused {
            for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
                if self.pointer.buttons & pointer_mask(button) != 0 {
                    self.send_mouse(MouseEventKind::Up(button), self.pointer.position, modifiers);
                }
            }
            self.pointer.active = false;
        }
        if message == WM_LBUTTONDOWN && !modifiers.contains(KeyModifiers::SHIFT) {
            // Terminal reports the actual viewport coordinates. UIA can have a
            // different vertical origin; never replace ordinary clicks/drags
            // with that estimate, including Ctrl/Alt clicks and double clicks.
            if self.pointer.buttons & pointer_mask(MouseButton::Left) != 0 {
                if self.pointer.active {
                    self.send_mouse(
                        MouseEventKind::Up(MouseButton::Left),
                        self.pointer.position,
                        modifiers,
                    );
                }
                self.pointer.buttons &= !pointer_mask(MouseButton::Left);
                self.pointer.active &= self.pointer.buttons != 0;
                self.pointer.native_press = false;
            }
            if focused {
                self.focus.native_press(event, grid, modifiers);
            }
            return false;
        }
        if message == WM_LBUTTONUP {
            self.focus.native_release();
        }
        let native_grid = (message == WM_MOUSEMOVE
            && modifiers.contains(KeyModifiers::SHIFT)
            && self.pointer.buttons & pointer_mask(MouseButton::Left) == 0
            && focused)
            .then(|| self.focus.pointer_grid(grid, true))
            .flatten();
        let grid = self.focus.pointer_grid(grid, false);
        if let Some(grid) = native_grid
            && let Some(position) = grid.position(event.pt, true)
        {
            // A normal press already established the editor's selection anchor.
            // Only supply the Shift motion Terminal omits; do not replay Down.
            self.focus
                .suppressed_buttons
                .fetch_or(pointer_mask(MouseButton::Left), Ordering::AcqRel);
            if self.send_mouse(MouseEventKind::Drag(MouseButton::Left), position, modifiers) {
                self.pointer.buttons |= pointer_mask(MouseButton::Left);
                self.pointer.grid = Some(grid);
                self.pointer.position = position;
                self.pointer.active = true;
                self.pointer.native_press = true;
            } else {
                self.focus
                    .suppressed_buttons
                    .fetch_and(!pointer_mask(MouseButton::Left), Ordering::AcqRel);
            }
        }
        if let Some((button, down)) = pointer_button(message) {
            let mask = pointer_mask(button);
            if !down && self.pointer.buttons & mask != 0 {
                let native_release = button == MouseButton::Left && self.pointer.native_press;
                if self.pointer.active {
                    if let Some(position) = grid
                        .or(self.pointer.grid)
                        .and_then(|grid| grid.position(event.pt, true))
                    {
                        self.pointer.position = position;
                    }
                    self.send_mouse(MouseEventKind::Up(button), self.pointer.position, modifiers);
                }
                self.pointer.buttons &= !mask;
                if button == MouseButton::Left {
                    self.pointer.native_press = false;
                }
                if self.pointer.buttons == 0 {
                    self.pointer.active = false;
                }
                // Terminal received a delegated press and must receive its
                // physical release too. Its duplicate console report is filtered.
                return !native_release;
            }
            // Only intercept gestures that Windows Terminal does not forward.
            if down
                && focused
                && modifiers.contains(KeyModifiers::SHIFT)
                && let Some(grid) = grid
                && let Some(position) = grid.position(event.pt, false)
            {
                self.focus
                    .suppressed_buttons
                    .fetch_or(mask, Ordering::AcqRel);
                if !self.send_mouse(MouseEventKind::Down(button), position, modifiers) {
                    self.focus
                        .suppressed_buttons
                        .fetch_and(!mask, Ordering::AcqRel);
                    return false;
                }
                self.pointer.buttons |= mask;
                self.pointer.grid = Some(grid);
                self.pointer.position = position;
                self.pointer.active = true;
                return true;
            }
        }
        if message == WM_MOUSEMOVE && self.pointer.active {
            let position = grid
                .or(self.pointer.grid)
                .and_then(|grid| grid.position(event.pt, true));
            if let Some(position) = position
                && position != self.pointer.position
            {
                for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
                    if self.pointer.buttons & pointer_mask(button) != 0 {
                        self.send_mouse(MouseEventKind::Drag(button), position, modifiers);
                    }
                }
                self.pointer.position = position;
            }
            // Keep physical pointer motion. Shift presses are swallowed;
            // a delegated press has no native selection anchor. Duplicate VT
            // drags after releasing Shift are discarded by filter_native_mouse.
            return false;
        }
        if matches!(message, WM_MOUSEWHEEL | WM_MOUSEHWHEEL) {
            if !focused || !modifiers.contains(KeyModifiers::SHIFT) {
                self.wheel_remainder = [0; 2];
                return false;
            }
            if let Some(position) = grid.and_then(|grid| grid.position(event.pt, false)) {
                let axis = usize::from(message == WM_MOUSEHWHEEL);
                let delta = i32::from((event.mouseData >> 16) as i16);
                let total = self.wheel_remainder[axis] + delta;
                self.wheel_remainder[axis] = total % 120;
                let kind = match (axis, total > 0) {
                    (0, true) => MouseEventKind::ScrollUp,
                    (0, false) => MouseEventKind::ScrollDown,
                    (_, true) => MouseEventKind::ScrollRight,
                    (_, false) => MouseEventKind::ScrollLeft,
                };
                for _ in 0..(total / 120).unsigned_abs() {
                    if !self.send_mouse(kind, position, modifiers) {
                        return false;
                    }
                }
                return true;
            }
        }
        false
    }

    fn handle_event(&mut self, message: u32, event: &MSLLHOOKSTRUCT, foreground: isize) -> bool {
        let (button, mask) = match event.mouseData >> 16 {
            1 => (MouseButton::Back, 1),
            2 => (MouseButton::Forward, 2),
            _ => return false,
        };
        // Navigation belongs to the focused pane, regardless of the pointer
        // position. In particular, do not hit-test event.pt: it is in physical
        // pixels while a console process's window APIs can use scaled units.
        self.handle_button(message, button, mask, foreground)
    }

    fn handle_button(
        &mut self,
        message: u32,
        button: MouseButton,
        mask: u8,
        foreground: isize,
    ) -> bool {
        if message == WM_XBUTTONUP {
            let captured = self.captured_buttons & mask != 0;
            self.captured_buttons &= !mask;
            return captured;
        }
        if !may_capture(
            self.focus.enabled.load(Ordering::Acquire),
            self.focus.host.load(Ordering::Acquire),
            foreground,
        ) || !self.focus.geometry.permits_capture() {
            return false;
        }
        if self.captured_buttons & mask == 0 {
            if !self.send_mouse(MouseEventKind::Down(button), (0, 0), KeyModifiers::empty()) {
                return false;
            }
            self.captured_buttons |= mask;
        }
        true
    }
}

fn with_physical_coordinates<T>(read: impl FnOnce() -> T) -> T {
    struct RestoreDpi(DPI_AWARENESS_CONTEXT);
    impl Drop for RestoreDpi {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: restore this thread's context returned by the setter.
                unsafe { SetThreadDpiAwarenessContext(self.0) };
            }
        }
    }
    // Low-level callbacks can enter with the process's DPI context despite
    // setting the hook thread's context at startup. GetWindowRect and hit tests
    // must use physical pixels, like MSLLHOOKSTRUCT and UIA, on every callback.
    // Restore before calling the next hook so its coordinate behavior is intact.
    let _restore = RestoreDpi(unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
    });
    read()
}

unsafe extern "system" fn mouse_hook(code: i32, message: usize, data: isize) -> isize {
    if code >= 0 {
        // SAFETY: for a nonnegative WH_MOUSE_LL notification, Windows supplies
        // a valid MSLLHOOKSTRUCT for the lifetime of this callback.
        let event = unsafe { &*(data as *const MSLLHOOKSTRUCT) };
        let handled = with_physical_coordinates(|| {
            CONTEXT.with_borrow_mut(|context| {
                let Some(context) = context else { return false };
                // SAFETY: this only reads the foreground window's handle.
                let foreground = unsafe { GetForegroundWindow() as isize };
                if matches!(message as u32, WM_XBUTTONDOWN | WM_XBUTTONUP) {
                    context.handle_event(message as u32, event, foreground)
                } else {
                    let modifiers = current_modifiers();
                    let needs_grid = context.pointer.active
                        || modifiers.contains(KeyModifiers::SHIFT)
                        || message as u32 == WM_LBUTTONDOWN;
                    let mut grid = if needs_grid
                        && context.focus.host.load(Ordering::Acquire) == foreground
                        && foreground != 0
                    {
                        if message as u32 == WM_LBUTTONDOWN && !modifiers.contains(KeyModifiers::SHIFT) {
                            context.focus.geometry.cached(foreground)
                        } else {
                            context.focus.geometry.current(foreground)
                        }
                    } else {
                        None
                    };
                    // Do not intercept clicks on an overlapping window, menu, tab
                    // strip or a different Terminal pane.
                    if grid.is_some()
                        && unsafe { GetAncestor(WindowFromPoint(event.pt), GA_ROOT) as isize }
                            != foreground
                    {
                        grid = None;
                    }
                    context.handle_pointer(message as u32, event, foreground, modifiers, grid)
                }
            })
        });
        if handled {
            return 1;
        }
    }
    // SAFETY: unhandled notifications must continue down the system hook chain.
    unsafe { CallNextHookEx(std::ptr::null_mut(), code, message, data) }
}

pub(crate) struct WindowsMouseInput {
    pub(crate) focus: Arc<MouseFocus>,
    thread_id: Arc<Mutex<Option<u32>>>,
    stopping: Arc<AtomicBool>,
    started: Option<std::sync::mpsc::Receiver<io::Result<u32>>>,
    thread: Option<std::thread::JoinHandle<()>>,
    _geometry_worker: geometry::Worker,
}

impl WindowsMouseInput {
    pub(crate) fn start(
        sender: impl Into<Sender>,
        wake: crate::background::UiWake,
    ) -> io::Result<Self> {
        Self::start_with_setup(sender, wake, || {})
    }

    fn start_with_setup(
        sender: impl Into<Sender>,
        wake: crate::background::UiWake,
        setup: impl FnOnce() + Send + 'static,
    ) -> io::Result<Self> {
        let sender = sender.into();
        // Windows' default-terminal handoff can host PowerShell or a directly
        // launched executable in Terminal without setting WT_SESSION. Detect
        // the actual focused host in MouseFocus instead of treating an
        // inherited environment variable as proof that capture is available.
        let focus = Arc::new(MouseFocus::default());
        focus.geometry.pin_current_pane();
        let geometry_worker = geometry::Worker::start(&focus)?;
        // Wait for the caller's setting before intercepting any gesture.
        // A background launch must not assume that a foreground terminal is
        // its own pane. Focus reports or routed user input establish ownership.
        let thread_focus = focus.clone();
        let thread_id = Arc::new(Mutex::new(None));
        let hook_thread_id = thread_id.clone();
        let stopping = Arc::new(AtomicBool::new(false));
        let hook_stopping = stopping.clone();
        let (ready, started) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("terminal-mouse-input".into())
            .spawn(move || {
                struct ThreadId(Arc<Mutex<Option<u32>>>);
                impl Drop for ThreadId {
                    fn drop(&mut self) { *self.0.lock().unwrap_or_else(|error| error.into_inner()) = None; }
                }
                // Serialize ID retirement with shutdown posting: Windows may
                // reuse a thread ID immediately after that thread exits.
                let _id = ThreadId(hook_thread_id.clone());
                setup();
                if hook_stopping.load(Ordering::Acquire) { return; }
                CONTEXT.set(Some(HookContext {
                    sender,
                    wake,
                    focus: thread_focus,
                    captured_buttons: 0,
                    pointer: PointerCapture::default(),
                    wheel_remainder: [0; 2],
                }));
                // SAFETY: the callback and module remain loaded until the hook is
                // removed on this same thread. PeekMessage creates its message queue
                // before the owner is allowed to send the shutdown message.
                unsafe {
                    SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
                    let mut message: MSG = std::mem::zeroed();
                    PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
                    *hook_thread_id.lock().unwrap_or_else(|error| error.into_inner()) = Some(GetCurrentThreadId());
                    let hook = SetWindowsHookExW(
                        WH_MOUSE_LL,
                        Some(mouse_hook),
                        GetModuleHandleW(std::ptr::null()),
                        0,
                    );
                    if hook.is_null() {
                        let _ = ready.send(Err(io::Error::last_os_error()));
                        CONTEXT.set(None);
                        return;
                    }
                    if ready.send(Ok(GetCurrentThreadId())).is_ok()
                        && !hook_stopping.load(Ordering::Acquire)
                    {
                        while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                            TranslateMessage(&message);
                            DispatchMessageW(&message);
                        }
                    }
                    UnhookWindowsHookEx(hook);
                }
                CONTEXT.set(None);
            })?;
        // Installing a global hook can wait on Windows while the system is
        // busy. Keyboard/native mouse input and the first frame do not need
        // this optional bridge to have finished its handshake.
        Ok(Self {
            focus, thread_id, stopping, started: Some(started), thread: Some(thread),
            _geometry_worker: geometry_worker,
        })
    }

    pub(crate) fn poll_ready(&mut self) {
        let Some(started) = &self.started else { return };
        match started.try_recv() {
            Ok(Ok(_)) => { self.started = None; }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            result => {
                log_warn!("Windows Terminal mouse bridge unavailable: {result:?}");
                self.started = None;
            }
        }
    }
}

impl Drop for WindowsMouseInput {
    fn drop(&mut self) {
        self.focus.set_enabled(false);
        self.focus.set_focused(false);
        self.stopping.store(true, Ordering::Release);
        // The queue is published before hook installation. If shutdown wins
        // that race, the worker checks `stopping` before entering its loop.
        {
            let id = self.thread_id.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(id) = *id { unsafe { PostThreadMessageW(id, WM_QUIT, 0, 0); } }
        }
        if let Some(thread) = self.thread.take().filter(|thread| thread.is_finished()) {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_and_cancellation_do_not_wait_for_windows_hook_initialization() {
        let (sender, _events) = std::sync::mpsc::channel();
        let (started, running) = std::sync::mpsc::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        struct Release(std::sync::mpsc::Sender<()>);
        impl Drop for Release { fn drop(&mut self) { let _ = self.0.send(()); } }
        let _release = Release(release);
        let mut input = WindowsMouseInput::start_with_setup(sender, Default::default(), move || {
            started.send(()).unwrap();
            blocked.recv_timeout(std::time::Duration::from_secs(15)).unwrap();
        }).unwrap();
        running.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        input.poll_ready();
        assert!(input.started.is_some(), "initialization is deliberately still blocked");
        let (closed, done) = std::sync::mpsc::channel();
        std::thread::spawn(move || { drop(input); closed.send(()).unwrap(); });
        done.recv_timeout(std::time::Duration::from_secs(5))
            .expect("closing startup must not wait for the optional hook");
    }

    #[test]
    fn native_side_button_events_ignore_pointer_position() {
        use windows_sys::Win32::Foundation::POINT;
        let (sender, receiver) = std::sync::mpsc::channel();
        let focus = Arc::new(MouseFocus::default());
        focus.set_enabled(true);
        focus.host.store(12, Ordering::Release);
        let mut context = HookContext {
            sender: sender.into(),
            wake: crate::background::UiWake::default(),
            focus,
            captured_buttons: 0,
            pointer: PointerCapture::default(),
            wheel_remainder: [0; 2],
        };
        // Sidebar, editor, window edges, scaled-display coordinates and other
        // monitors all route to the focused terminal, including outside it.
        for (x, y) in [
            (0, 0),
            (10, 400),
            (750, 500),
            (1919, 1079),
            (-1920, 300),
            (3840, 2160),
            (i32::MIN, i32::MAX),
        ] {
            for (button, code) in [(MouseButton::Back, 1), (MouseButton::Forward, 2)] {
                let event = MSLLHOOKSTRUCT {
                    pt: POINT { x, y },
                    mouseData: code << 16,
                    flags: 0,
                    time: 0,
                    dwExtraInfo: 0,
                };
                assert!(context.handle_event(WM_XBUTTONDOWN, &event, 12));
                assert!(
                    matches!(receiver.try_recv().unwrap().unwrap(), Event::Mouse(mouse)
                    if mouse.kind == MouseEventKind::Down(button))
                );
                assert!(context.handle_event(WM_XBUTTONUP, &event, 12));
                assert!(receiver.try_recv().is_err());
                assert!(!context.handle_event(WM_XBUTTONDOWN, &event, 34));
                assert!(receiver.try_recv().is_err());
            }
        }
    }

    #[test]
    fn side_button_input_starts_without_windows_terminal_environment() {
        const PROBE: &str = "VTCODE_SIDE_BUTTON_STARTUP_TEST";
        if std::env::var_os(PROBE).is_none() {
            use std::os::windows::process::CommandExt;
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "windows_mouse::tests::side_button_input_starts_without_windows_terminal_environment",
                    "--nocapture",
                ])
                .env(PROBE, "1")
                .env_remove("WT_SESSION")
                .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        assert!(std::env::var_os("WT_SESSION").is_none());
        let (sender, receiver) = std::sync::mpsc::channel();
        let input = WindowsMouseInput::start(sender, crate::background::UiWake::default())
            .expect("default-terminal launches also need side-button input");
        assert!(!input.focus.enabled.load(Ordering::Acquire));
        assert_eq!(input.focus.host.load(Ordering::Acquire), 0);
        drop(input);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn side_buttons_are_captured_only_in_the_focused_terminal_window() {
        assert!(may_capture(true, 12, 12));
        assert!(!may_capture(false, 12, 12));
        assert!(!may_capture(true, 0, 12)); // another terminal pane owns focus
        assert!(!may_capture(true, 12, 34)); // another app is foreground
        assert!(!may_capture(true, 0, 0));
    }

    #[test]
    fn one_event_per_press_and_captured_release_is_consumed_after_focus_loss() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let focus = Arc::new(MouseFocus::default());
        focus.set_enabled(true);
        focus.host.store(12, Ordering::Release);
        let wake = crate::background::UiWake::default();
        let mut context = HookContext {
            sender: sender.into(),
            wake: wake.clone(),
            focus: focus.clone(),
            captured_buttons: 0,
            pointer: PointerCapture::default(),
            wheel_remainder: [0; 2],
        };
        for (button, mask) in [(MouseButton::Back, 1), (MouseButton::Forward, 2)] {
            assert!(context.handle_button(WM_XBUTTONDOWN, button, mask, 12));
            assert!(context.handle_button(WM_XBUTTONDOWN, button, mask, 12));
            assert!(
                matches!(receiver.try_recv().unwrap().unwrap(), Event::Mouse(event)
                if event.kind == MouseEventKind::Down(button))
            );
            assert!(receiver.try_recv().is_err());
        }
        assert_eq!(wake.generation(), 2);
        focus.set_focused(false);
        for (button, mask) in [(MouseButton::Back, 1), (MouseButton::Forward, 2)] {
            assert!(context.handle_button(WM_XBUTTONUP, button, mask, 34));
            assert!(!context.handle_button(WM_XBUTTONUP, button, mask, 34));
            assert!(!context.handle_button(WM_XBUTTONDOWN, button, mask, 34));
        }
        assert!(receiver.try_recv().is_err());
        assert_eq!(context.captured_buttons, 0);
    }

    #[test]
    fn disabled_or_disconnected_input_does_not_consume_mouse_gestures() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let focus = Arc::new(MouseFocus::default());
        focus.host.store(12, Ordering::Release);
        let mut context = HookContext {
            sender: sender.into(),
            wake: crate::background::UiWake::default(),
            focus: focus.clone(),
            captured_buttons: 0,
            pointer: PointerCapture::default(),
            wheel_remainder: [0; 2],
        };
        assert!(!context.handle_button(WM_XBUTTONDOWN, MouseButton::Back, 1, 12));
        assert!(receiver.try_recv().is_err());
        focus.set_enabled(true);
        drop(receiver);
        assert!(!context.handle_button(WM_XBUTTONDOWN, MouseButton::Back, 1, 12));
        assert!(!context.handle_button(WM_XBUTTONUP, MouseButton::Back, 1, 12));
        assert_eq!(context.wake.generation(), 0);
    }

    fn pointer_context() -> (HookContext, std::sync::mpsc::Receiver<io::Result<Event>>) {
        let (sender, receiver) = std::sync::mpsc::channel();
        let focus = Arc::new(MouseFocus::default());
        focus.host.store(12, Ordering::Release);
        (
            HookContext {
                sender: sender.into(),
                    focus,
                wake: crate::background::UiWake::default(),
                captured_buttons: 0,
                pointer: PointerCapture::default(),
                wheel_remainder: [0; 2],
            },
            receiver,
        )
    }

    fn native_event(x: i32, y: i32, delta: i16) -> MSLLHOOKSTRUCT {
        MSLLHOOKSTRUCT {
            pt: windows_sys::Win32::Foundation::POINT { x, y },
            mouseData: u32::from(delta as u16) << 16,
            flags: 0,
            time: 0,
            dwExtraInfo: 0,
        }
    }

    fn receive_mouse(receiver: &std::sync::mpsc::Receiver<io::Result<Event>>) -> MouseEvent {
        let Event::Mouse(event) = receiver.try_recv().unwrap().unwrap() else {
            panic!("expected mouse")
        };
        event
    }

    #[test]
    fn press_ingress_timestamp_survives_release_and_is_attached_only_once() {
        for modifiers in [KeyModifiers::NONE, KeyModifiers::CONTROL, KeyModifiers::ALT] {
            let (mut context, _) = pointer_context();
            assert!(!context.handle_pointer(WM_LBUTTONDOWN, &native_event(55, 91, 0), 12, modifiers, None));
            let observed = context.focus.native_pointer.lock().unwrap().pending.front().unwrap().observed_at;
            // A slow Terminal may deliver Down after the physical Up. That
            // must still be measurable, without rearming a released gesture.
            assert!(!context.handle_pointer(WM_LBUTTONUP, &native_event(55, 91, 0), 12, modifiers, None));
            let press = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 3, row: 0, modifiers };
            assert!(!context.focus.stage_native_mouse(&press));
            assert_eq!(context.focus.order_native_mouse(), Some(observed));
            assert_eq!(context.focus.order_native_mouse(), None);
            assert!(context.focus.native_pointer.lock().unwrap().reference.is_none());
            assert!(!context.focus.native_left_down.load(Ordering::Acquire));

            assert!(!context.handle_pointer(WM_LBUTTONDOWN, &native_event(55, 91, 0), 12, modifiers, None));
            assert!(!context.focus.stage_native_mouse(&press));
            context.focus.reset_native_pointer();
            assert_eq!(context.focus.order_native_mouse(), None, "focus/resize invalidation discards pending metadata too");
        }
        let (mut context, _) = pointer_context();
        for _ in 0..2 {
            assert!(!context.handle_pointer(WM_LBUTTONDOWN, &native_event(55, 91, 0), 12, KeyModifiers::NONE, None));
            assert!(!context.handle_pointer(WM_LBUTTONUP, &native_event(55, 91, 0), 12, KeyModifiers::NONE, None));
        }
        let press = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 3, row: 0, modifiers: KeyModifiers::NONE };
        assert!(!context.focus.stage_native_mouse(&press));
        assert_eq!(context.focus.order_native_mouse(), None, "an older unmatched physical press is not an ingress sample");
        assert!(!context.focus.stage_native_mouse(&press));
        assert!(context.focus.order_native_mouse().is_some());
    }

    #[test]
    fn rapid_shift_drag_and_release_keep_order_with_an_unpublished_terminal_press() {
        let (mut context, _) = pointer_context();
        let (sender, receiver) = crate::terminal_input_batch::channel();
        context.sender = sender.clone();
        let grid = Some(geometry::test_grid());
        assert!(!context.handle_pointer(WM_LBUTTONDOWN, &native_event(55, 91, 0), 12, KeyModifiers::NONE, grid));
        let press = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: 3, row: 0, modifiers: KeyModifiers::NONE };
        let focus = context.focus.clone();
        assert!(crate::terminal_input_batch::forward_with_budget(Ok(Event::Mouse(press)), &sender, &Default::default(),
            |event| matches!(event, Ok(Event::Mouse(mouse)) if focus.stage_native_mouse(mouse)),
            || {
                assert!(!context.handle_pointer(WM_MOUSEMOVE, &native_event(85, 131, 0), 12, KeyModifiers::SHIFT, grid));
                assert!(!context.handle_pointer(WM_LBUTTONUP, &native_event(85, 131, 0), 12, KeyModifiers::SHIFT, grid));
                assert!(receiver.try_recv().is_err(), "native motion and release cannot overtake an unpublished terminal Down");
                Ok(None)
            }, std::time::Duration::MAX, || focus.order_native_mouse()));
        assert_eq!(receiver.try_recv().unwrap().unwrap(), Event::Mouse(press));
        assert!(matches!(receiver.try_recv().unwrap().unwrap(), Event::Mouse(MouseEvent { kind: MouseEventKind::Drag(MouseButton::Left), .. })));
        assert!(matches!(receiver.try_recv().unwrap().unwrap(), Event::Mouse(MouseEvent { kind: MouseEventKind::Up(MouseButton::Left), .. })));
    }

    #[test]
    fn ordinary_clicks_and_drags_keep_terminal_coordinates_when_uia_has_the_wrong_origin() {
        for grid in [Some(geometry::test_grid()), None] {
            for modifiers in [KeyModifiers::NONE, KeyModifiers::CONTROL, KeyModifiers::ALT] {
                let (mut context, receiver) = pointer_context();
                for _ in 0..3 {
                    assert!(!context.handle_pointer(
                        WM_LBUTTONDOWN,
                        &native_event(55, 91, 0),
                        12,
                        modifiers,
                        grid
                    ));
                    // UIA estimates row 2 here, but the terminal reports row 0.
                    let mut reported = MouseEvent {
                        kind: MouseEventKind::Down(MouseButton::Left),
                        column: 3,
                        row: 0,
                        modifiers,
                    };
                    assert!(!context.focus.filter_native_mouse(&reported));
                    assert_eq!((reported.column, reported.row), (3, 0));
                    assert!(!context.handle_pointer(
                        WM_MOUSEMOVE,
                        &native_event(85, 131, 0),
                        12,
                        modifiers,
                        grid
                    ));
                    reported.kind = MouseEventKind::Drag(MouseButton::Left);
                    reported.column = 6;
                    reported.row = 2;
                    assert!(!context.focus.filter_native_mouse(&reported));
                    assert!(!context.handle_pointer(
                        WM_LBUTTONUP,
                        &native_event(85, 131, 0),
                        12,
                        modifiers,
                        grid
                    ));
                    reported.kind = MouseEventKind::Up(MouseButton::Left);
                    assert!(!context.focus.filter_native_mouse(&reported));
                    assert!(
                        receiver.try_recv().is_err(),
                        "ordinary input must not be synthesized twice"
                    );
                    assert!(!context.pointer.active);
                }
            }
        }
    }

    #[test]
    fn shift_takeover_uses_the_native_press_origin_and_releases_the_terminal_press() {
        let (mut context, receiver) = pointer_context();
        let grid = Some(geometry::test_grid());
        assert!(!context.handle_pointer(
            WM_LBUTTONDOWN,
            &native_event(55, 91, 0),
            12,
            KeyModifiers::NONE,
            grid
        ));
        assert!(!context.focus.filter_native_mouse(&MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(!context.handle_pointer(
            WM_MOUSEMOVE,
            &native_event(85, 131, 0),
            12,
            KeyModifiers::SHIFT,
            grid
        ));
        let drag = receive_mouse(&receiver);
        assert_eq!(drag.kind, MouseEventKind::Drag(MouseButton::Left));
        assert_eq!((drag.column, drag.row), (6, 2));
        assert!(context.focus.filter_native_mouse(&drag));
        assert!(
            receiver.try_recv().is_err(),
            "takeover must not replay Down or duplicate Drag"
        );
        // Releasing Shift mid-drag must keep the same coordinates and owner.
        assert!(!context.handle_pointer(
            WM_MOUSEMOVE,
            &native_event(115, 151, 0),
            12,
            KeyModifiers::NONE,
            grid
        ));
        let drag = receive_mouse(&receiver);
        assert_eq!((drag.column, drag.row), (9, 3));
        assert!(!context.handle_pointer(
            WM_LBUTTONUP,
            &native_event(115, 151, 0),
            12,
            KeyModifiers::NONE,
            grid
        ));
        let release = receive_mouse(&receiver);
        assert_eq!(release.kind, MouseEventKind::Up(MouseButton::Left));
        assert_eq!((release.column, release.row), (9, 3));
        assert!(context.focus.filter_native_mouse(&release));
        assert!(!context.handle_pointer(
            WM_MOUSEMOVE,
            &native_event(145, 171, 0),
            12,
            KeyModifiers::SHIFT,
            grid
        ));
        assert!(
            receiver.try_recv().is_err(),
            "release must prevent a phantom new drag"
        );

        // Subsequent Shift+clicks/wheels use the confirmed terminal origin too.
        assert!(context.handle_pointer(
            WM_LBUTTONDOWN,
            &native_event(85, 131, 0),
            12,
            KeyModifiers::SHIFT,
            grid
        ));
        let press = receive_mouse(&receiver);
        assert_eq!((press.column, press.row), (6, 2));
        assert!(context.handle_pointer(
            WM_LBUTTONUP,
            &native_event(85, 131, 0),
            12,
            KeyModifiers::NONE,
            grid
        ));
        receive_mouse(&receiver);
        assert!(context.handle_pointer(
            WM_MOUSEWHEEL,
            &native_event(85, 131, 120),
            12,
            KeyModifiers::SHIFT,
            grid
        ));
        let wheel = receive_mouse(&receiver);
        assert_eq!((wheel.column, wheel.row), (6, 2));
    }

    #[test]
    fn delayed_native_reports_cannot_take_over_a_newer_physical_drag() {
        let (mut context, receiver) = pointer_context();
        let grid = Some(geometry::test_grid());
        context.handle_pointer(
            WM_LBUTTONDOWN,
            &native_event(55, 91, 0),
            12,
            KeyModifiers::NONE,
            grid,
        );
        context.handle_pointer(
            WM_LBUTTONUP,
            &native_event(55, 91, 0),
            12,
            KeyModifiers::NONE,
            grid,
        );
        context.handle_pointer(
            WM_LBUTTONDOWN,
            &native_event(85, 131, 0),
            12,
            KeyModifiers::NONE,
            grid,
        );
        context.focus.filter_native_mouse(&MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        context.handle_pointer(
            WM_MOUSEMOVE,
            &native_event(85, 151, 0),
            12,
            KeyModifiers::SHIFT,
            grid,
        );
        assert!(receiver.try_recv().is_err());
        context.focus.filter_native_mouse(&MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 6,
            row: 2,
            modifiers: KeyModifiers::NONE,
        });
        context.handle_pointer(
            WM_MOUSEMOVE,
            &native_event(85, 151, 0),
            12,
            KeyModifiers::SHIFT,
            grid,
        );
        assert_eq!(
            (receive_mouse(&receiver).column, context.pointer.position.1),
            (6, 3)
        );
    }

    #[test]
    fn native_press_tracking_is_bounded_when_console_input_is_delayed() {
        let (mut context, receiver) = pointer_context();
        for _ in 0..80 {
            context.handle_pointer(
                WM_LBUTTONDOWN,
                &native_event(55, 91, 0),
                12,
                KeyModifiers::NONE,
                Some(geometry::test_grid()),
            );
            context.handle_pointer(
                WM_LBUTTONUP,
                &native_event(55, 91, 0),
                12,
                KeyModifiers::NONE,
                Some(geometry::test_grid()),
            );
        }
        assert_eq!(
            context.focus.native_pointer.lock().unwrap().pending.len(),
            16
        );
        for _ in 0..80 {
            assert!(!context.focus.filter_native_mouse(&MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 3,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }));
        }
        let pointer = context.focus.native_pointer.lock().unwrap();
        assert!(pointer.pending.is_empty());
        assert_eq!(pointer.discarded, 0);
        assert!(pointer.reference.is_none());
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn resizing_clears_native_drag_references_and_releases_cannot_restart_them() {
        let (mut context, receiver) = pointer_context();
        let grid = Some(geometry::test_grid());
        context.handle_pointer(
            WM_LBUTTONDOWN,
            &native_event(55, 91, 0),
            12,
            KeyModifiers::NONE,
            grid,
        );
        context.focus.filter_native_mouse(&MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        context.focus.resized();
        context.handle_pointer(
            WM_MOUSEMOVE,
            &native_event(85, 131, 0),
            12,
            KeyModifiers::SHIFT,
            grid,
        );
        assert!(receiver.try_recv().is_err());
        assert!(!context.pointer.active);
        assert!(
            context
                .focus
                .native_pointer
                .lock()
                .unwrap()
                .reference
                .is_none()
        );
    }

    #[test]
    fn next_ordinary_press_ends_a_capture_whose_release_was_lost() {
        let (mut context, receiver) = pointer_context();
        let grid = Some(geometry::test_grid());
        context.handle_pointer(
            WM_LBUTTONDOWN,
            &native_event(55, 91, 0),
            12,
            KeyModifiers::SHIFT,
            grid,
        );
        receive_mouse(&receiver);
        assert!(!context.handle_pointer(
            WM_LBUTTONDOWN,
            &native_event(85, 131, 0),
            12,
            KeyModifiers::NONE,
            grid
        ));
        assert_eq!(
            receive_mouse(&receiver).kind,
            MouseEventKind::Up(MouseButton::Left)
        );
        assert!(!context.focus.filter_native_mouse(&MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 6,
            row: 2,
            modifiers: KeyModifiers::NONE,
        }));
        context.handle_pointer(
            WM_MOUSEMOVE,
            &native_event(115, 151, 0),
            12,
            KeyModifiers::NONE,
            grid,
        );
        assert!(receiver.try_recv().is_err());
        assert!(!context.pointer.active);
    }

    #[test]
    fn native_release_report_prevents_takeover_when_the_hook_missed_release() {
        let (mut context, receiver) = pointer_context();
        let grid = Some(geometry::test_grid());
        context.handle_pointer(
            WM_LBUTTONDOWN,
            &native_event(55, 91, 0),
            12,
            KeyModifiers::NONE,
            grid,
        );
        let mut reported = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        context.focus.filter_native_mouse(&reported);
        reported.kind = MouseEventKind::Up(MouseButton::Left);
        assert!(!context.focus.filter_native_mouse(&reported));
        context.handle_pointer(
            WM_MOUSEMOVE,
            &native_event(85, 131, 0),
            12,
            KeyModifiers::SHIFT,
            grid,
        );
        assert!(receiver.try_recv().is_err());
        assert!(!context.pointer.active);
    }

    #[test]
    fn ordinary_press_after_focus_loss_cannot_revive_a_captured_button_chord() {
        let (mut context, receiver) = pointer_context();
        let grid = Some(geometry::test_grid());
        for message in [WM_LBUTTONDOWN, WM_RBUTTONDOWN] {
            context.handle_pointer(
                message,
                &native_event(55, 91, 0),
                12,
                KeyModifiers::SHIFT,
                grid,
            );
            receive_mouse(&receiver);
        }
        context.focus.set_focused(false);
        context.handle_pointer(
            WM_MOUSEMOVE,
            &native_event(55, 91, 0),
            34,
            KeyModifiers::NONE,
            None,
        );
        receive_mouse(&receiver);
        receive_mouse(&receiver);
        context.focus.host.store(12, Ordering::Release);
        assert!(!context.handle_pointer(
            WM_LBUTTONDOWN,
            &native_event(55, 91, 0),
            12,
            KeyModifiers::NONE,
            grid
        ));
        context.handle_pointer(
            WM_MOUSEMOVE,
            &native_event(85, 131, 0),
            12,
            KeyModifiers::NONE,
            grid,
        );
        assert!(receiver.try_recv().is_err());
        assert!(!context.pointer.active);
    }

    #[test]
    fn shift_gesture_is_captured_through_shift_release_and_native_duplicates_are_filtered() {
        let (mut context, receiver) = pointer_context();
        let grid = Some(geometry::test_grid());
        assert!(context.handle_pointer(
            WM_LBUTTONDOWN,
            &native_event(55, 91, 0),
            12,
            KeyModifiers::SHIFT,
            grid
        ));
        let down = receive_mouse(&receiver);
        assert_eq!(
            down,
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 3,
                row: 2,
                modifiers: KeyModifiers::SHIFT
            }
        );
        // No side-button setting is required for selection or horizontal scroll.
        assert!(!context.focus.enabled.load(Ordering::Acquire));
        assert!(!context.handle_pointer(
            WM_MOUSEMOVE,
            &native_event(85, 131, 0),
            12,
            KeyModifiers::NONE,
            grid
        ));
        let drag = receive_mouse(&receiver);
        assert_eq!(drag.kind, MouseEventKind::Drag(MouseButton::Left));
        assert_eq!((drag.column, drag.row), (6, 4));
        assert!(context.focus.filter_native_mouse(&drag));
        assert!(context.handle_pointer(
            WM_LBUTTONUP,
            &native_event(85, 131, 0),
            12,
            KeyModifiers::NONE,
            grid
        ));
        let up = receive_mouse(&receiver);
        assert_eq!(up.kind, MouseEventKind::Up(MouseButton::Left));
        assert!(context.focus.filter_native_mouse(&drag)); // late VT packet
        assert!(context.focus.filter_native_mouse(&up));
        assert!(!context.focus.filter_native_mouse(&down)); // next native gesture
        assert!(!context.focus.filter_native_mouse(&drag));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn pressing_shift_during_an_existing_drag_keeps_editor_ownership() {
        let (mut context, receiver) = pointer_context();
        let grid = Some(geometry::test_grid());
        assert!(!context.handle_pointer(
            WM_LBUTTONDOWN,
            &native_event(55, 91, 0),
            12,
            KeyModifiers::NONE,
            grid
        ));
        assert!(receiver.try_recv().is_err());
        assert!(!context.focus.filter_native_mouse(&MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left), column: 3, row: 2,
            modifiers: KeyModifiers::NONE,
        }));
        assert!(!context.handle_pointer(
            WM_MOUSEMOVE,
            &native_event(-10, 900, 0),
            12,
            KeyModifiers::SHIFT,
            grid
        ));
        let drag = receive_mouse(&receiver);
        assert_eq!((drag.column, drag.row), (0, 19));
        assert_eq!(drag.modifiers, KeyModifiers::SHIFT);
        assert!(!context.handle_pointer(
            WM_LBUTTONUP,
            &native_event(-10, 900, 0),
            12,
            KeyModifiers::SHIFT,
            grid
        ));
        assert_eq!(
            receive_mouse(&receiver).kind,
            MouseEventKind::Up(MouseButton::Left)
        );
    }

    #[test]
    fn pointer_release_keeps_its_actual_position_without_a_final_move() {
        let (mut context, receiver) = pointer_context();
        let grid = Some(geometry::test_grid());
        context.handle_pointer(
            WM_LBUTTONDOWN,
            &native_event(55, 91, 0),
            12,
            KeyModifiers::SHIFT,
            grid,
        );
        receive_mouse(&receiver);
        assert!(context.handle_pointer(
            WM_LBUTTONUP,
            &native_event(85, 131, 0),
            12,
            KeyModifiers::NONE,
            grid
        ));
        let release = receive_mouse(&receiver);
        assert_eq!(release.kind, MouseEventKind::Up(MouseButton::Left));
        assert_eq!((release.column, release.row), (6, 4));
    }

    #[test]
    fn shift_clicks_and_wheels_ignore_other_windows_chrome_and_missing_geometry() {
        let (mut context, receiver) = pointer_context();
        for message in [
            WM_LBUTTONDOWN,
            WM_RBUTTONDOWN,
            WM_MBUTTONDOWN,
            WM_MOUSEWHEEL,
        ] {
            for (x, y, host, grid) in [
                (55, 91, 34, Some(geometry::test_grid())),
                (55, 20, 12, Some(geometry::test_grid())),
                (900, 91, 12, Some(geometry::test_grid())),
                (55, 91, 12, None),
            ] {
                assert!(!context.handle_pointer(
                    message,
                    &native_event(x, y, 120),
                    host,
                    KeyModifiers::SHIFT,
                    grid
                ));
            }
        }
        assert!(receiver.try_recv().is_err());
        drop(receiver);
        assert!(!context.handle_pointer(
            WM_LBUTTONDOWN,
            &native_event(55, 91, 0),
            12,
            KeyModifiers::SHIFT,
            Some(geometry::test_grid())
        ));
        assert_eq!(context.pointer.buttons, 0);
    }

    #[test]
    fn focus_loss_ends_drag_once_and_swallows_the_original_release() {
        let (mut context, receiver) = pointer_context();
        let grid = Some(geometry::test_grid());
        context.handle_pointer(
            WM_LBUTTONDOWN,
            &native_event(55, 91, 0),
            12,
            KeyModifiers::SHIFT,
            grid,
        );
        receive_mouse(&receiver);
        context.focus.set_focused(false);
        context.handle_pointer(
            WM_MOUSEMOVE,
            &native_event(85, 131, 0),
            34,
            KeyModifiers::SHIFT,
            None,
        );
        let release = receive_mouse(&receiver);
        assert_eq!(release.kind, MouseEventKind::Up(MouseButton::Left));
        assert_eq!((release.column, release.row), (3, 2));
        assert!(context.handle_pointer(
            WM_LBUTTONUP,
            &native_event(85, 131, 0),
            34,
            KeyModifiers::NONE,
            None
        ));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn switching_terminal_panes_cannot_continue_a_cached_drag_or_capture_side_buttons() {
        let (mut context, receiver) = pointer_context();
        context.handle_pointer(WM_LBUTTONDOWN, &native_event(55, 91, 0), 12,
            KeyModifiers::SHIFT, Some(geometry::test_grid()));
        receive_mouse(&receiver);
        // The outer window is unchanged. UIA has invalidated ownership before
        // the remote focus report can make a round trip through SSH.
        context.focus.geometry.pin_current_pane();
        context.handle_pointer(WM_MOUSEMOVE, &native_event(85, 131, 0), 12,
            KeyModifiers::SHIFT, Some(geometry::test_grid()));
        let release = receive_mouse(&receiver);
        assert_eq!(release.kind, MouseEventKind::Up(MouseButton::Left));
        assert_eq!((release.column, release.row), (3, 2));
        assert!(!context.handle_button(WM_XBUTTONDOWN, MouseButton::Back, 1, 12));
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn shift_wheel_preserves_direction_delta_and_modifiers() {
        let (mut context, receiver) = pointer_context();
        let grid = Some(geometry::test_grid());
        assert!(context.handle_pointer(
            WM_MOUSEWHEEL,
            &native_event(55, 91, 60),
            12,
            KeyModifiers::SHIFT,
            grid
        ));
        assert!(receiver.try_recv().is_err());
        assert!(context.handle_pointer(
            WM_MOUSEWHEEL,
            &native_event(55, 91, 60),
            12,
            KeyModifiers::SHIFT,
            grid
        ));
        let up = receive_mouse(&receiver);
        assert_eq!(up.kind, MouseEventKind::ScrollUp);
        assert_eq!(up.modifiers, KeyModifiers::SHIFT);
        assert_eq!((up.column, up.row), (3, 2));
        for (message, delta, kind) in [
            (WM_MOUSEWHEEL, -240, MouseEventKind::ScrollDown),
            (WM_MOUSEHWHEEL, 240, MouseEventKind::ScrollRight),
            (WM_MOUSEHWHEEL, -240, MouseEventKind::ScrollLeft),
        ] {
            assert!(context.handle_pointer(
                message,
                &native_event(55, 91, delta),
                12,
                KeyModifiers::SHIFT | KeyModifiers::ALT,
                grid
            ));
            for _ in 0..2 {
                let event = receive_mouse(&receiver);
                assert_eq!(event.kind, kind);
                assert_eq!(event.modifiers, KeyModifiers::SHIFT | KeyModifiers::ALT);
            }
        }
        assert!(!context.handle_pointer(
            WM_MOUSEWHEEL,
            &native_event(55, 91, 120),
            12,
            KeyModifiers::NONE,
            grid
        ));
        assert!(receiver.try_recv().is_err());
    }
}
