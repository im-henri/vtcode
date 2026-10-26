//! Read the focused Terminal pane's physical text bounds off the hook thread.
//! Console font/window APIs describe ConPTY's hidden console, not Terminal's
//! actual font, padding, DPI or split panes. UIA's visible text range does.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak, mpsc};
use std::time::{Duration, Instant};

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayDestroy, SafeArrayGetElement, SafeArrayGetLBound, SafeArrayGetUBound,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation8, IUIAutomation, IUIAutomation2, IUIAutomationElement,
    IUIAutomationFocusChangedEventHandler, IUIAutomationFocusChangedEventHandler_Impl,
    IUIAutomationTextPattern, UIA_TextPatternId,
};
use windows::core::Interface;
use windows_sys::Win32::Foundation::{POINT, RECT};
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetThreadDpiAwarenessContext,
};
use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowRect;

#[derive(Clone, Copy, Debug)]
pub(super) struct TextGrid {
    pub host: isize,
    window: [i32; 4],
    bounds: [f64; 4],
    columns: u16,
    rows: u16,
    updated: Instant,
}

impl TextGrid {
    // Retain UIA's cell pitch, but align its cell coordinates with a matched
    // terminal report. A resize, pane move or font change invalidates the match.
    pub(super) fn with_native_reference(
        mut self,
        reference: Self,
        point: POINT,
        position: (u16, u16),
    ) -> Option<Self> {
        if self.host != reference.host
            || self.window != reference.window
            || self.bounds != reference.bounds
            || self.columns != reference.columns
            || self.rows != reference.rows
            || position.0 >= self.columns
            || position.1 >= self.rows
        {
            return None;
        }
        let [left, top, right, bottom] = self.bounds;
        let width = (right - left) / f64::from(self.columns);
        let height = (bottom - top) / f64::from(self.rows);
        let dx = (((f64::from(point.x) - left) / width).floor() - f64::from(position.0)) * width;
        let dy = (((f64::from(point.y) - top) / height).floor() - f64::from(position.1)) * height;
        self.bounds = [left + dx, top + dy, right + dx, bottom + dy];
        Some(self)
    }

    fn new(
        host: isize,
        window: [i32; 4],
        bounds: [f64; 4],
        columns: u16,
        rows: u16,
    ) -> Option<Self> {
        let [left, top, right, bottom] = bounds;
        if columns == 0
            || rows == 0
            || !bounds.iter().all(|n| n.is_finite())
            || right - left < f64::from(columns)
            || bottom - top < f64::from(rows)
        {
            return None;
        }
        Some(Self {
            host,
            window,
            bounds,
            columns,
            rows,
            updated: Instant::now(),
        })
    }

    pub(super) fn position(&self, point: POINT, clamp: bool) -> Option<(u16, u16)> {
        let [left, top, right, bottom] = self.bounds;
        let (x, y) = (f64::from(point.x), f64::from(point.y));
        if !clamp && (x < left || x >= right || y < top || y >= bottom) {
            return None;
        }
        let col = ((x - left) * f64::from(self.columns) / (right - left)).floor();
        let row = ((y - top) * f64::from(self.rows) / (bottom - top)).floor();
        Some((
            col.clamp(0.0, f64::from(self.columns - 1)) as u16,
            row.clamp(0.0, f64::from(self.rows - 1)) as u16,
        ))
    }
}

#[derive(Default)]
pub(super) struct Tracker {
    pin_pane: AtomicBool,
    pane_valid: AtomicBool,
    grid: Mutex<Option<TextGrid>>,
    epoch: AtomicU64,
    pending: AtomicBool,
    sender: OnceLock<mpsc::SyncSender<bool>>,
}

#[windows::core::implement(IUIAutomationFocusChangedEventHandler)]
struct FocusChanged(Weak<super::MouseFocus>);

impl IUIAutomationFocusChangedEventHandler_Impl for FocusChanged_Impl {
    fn HandleFocusChangedEvent(
        &self,
        _sender: Option<&IUIAutomationElement>,
    ) -> windows::core::Result<()> {
        if let Some(focus) = self.0.upgrade() {
            focus.geometry.invalidate();
        }
        Ok(())
    }
}

impl Tracker {
    pub(super) fn pin_current_pane(&self) {
        self.pin_pane.store(true, Ordering::Release);
        self.invalidate();
    }
    pub(super) fn invalidate(&self) {
        self.pane_valid.store(false, Ordering::Release);
        self.epoch.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut grid) = self.grid.lock() {
            *grid = None;
        }
        self.refresh();
    }

    pub(super) fn permits_capture(&self) -> bool {
        !self.pin_pane.load(Ordering::Acquire) || self.pane_valid.load(Ordering::Acquire)
    }

    pub(super) fn refresh(&self) {
        let Some(sender) = self.sender.get() else {
            return;
        };
        if !self.pending.swap(true, Ordering::AcqRel) && sender.try_send(true).is_err() {
            self.pending.store(false, Ordering::Release);
        }
    }

    pub(super) fn current(&self, host: isize) -> Option<TextGrid> {
        self.lookup(host, true)
    }

    /// Ordinary presses only retain an optional calibration reference. Their
    /// real coordinates come from Terminal, so they must not initiate UIA
    /// cross-process calls on Terminal's input/rendering path.
    pub(super) fn cached(&self, host: isize) -> Option<TextGrid> {
        self.lookup(host, false)
    }

    fn lookup(&self, host: isize, refresh: bool) -> Option<TextGrid> {
        // Never wait on another thread in a low-level mouse hook.
        let grid = *self.grid.try_lock().ok()?;
        if refresh && grid.is_none_or(|grid| grid.updated.elapsed() > Duration::from_millis(250)) {
            self.refresh();
        }
        let grid = grid.filter(|grid| grid.host == host)?;
        if window_bounds(host) != Some(grid.window) {
            if refresh { self.refresh(); }
            return None;
        }
        Some(grid)
    }
}

pub(super) struct Worker {
    sender: mpsc::SyncSender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Worker {
    pub(super) fn start(focus: &Arc<super::MouseFocus>) -> std::io::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let weak = Arc::downgrade(focus);
        let thread = std::thread::Builder::new()
            .name("terminal-mouse-geometry".into())
            .spawn(move || run(weak, receiver))?;
        let _ = focus.geometry.sender.set(sender.clone());
        Ok(Self {
            sender,
            thread: Some(thread),
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // The COM provider can still be inside a slow native call. Focus is
        // weakly held and disabled by the owner; shutdown must not wait for it.
        let _ = self.sender.try_send(false);
        if let Some(thread) = self.thread.take().filter(|thread| thread.is_finished()) {
            let _ = thread.join();
        }
    }
}

fn run(focus: Weak<super::MouseFocus>, receiver: mpsc::Receiver<bool>) {
    crate::worker_priority::lower_current_thread();
    // SAFETY: COM and DPI context are confined to this dedicated worker. No
    // COM object or provider call enters the low-level mouse hook.
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        if CoInitializeEx(None, COINIT_MULTITHREADED).is_err() {
            return;
        }
        if let Ok(automation) =
            CoCreateInstance::<_, IUIAutomation2>(&CUIAutomation8, None, CLSCTX_INPROC_SERVER)
        {
            let _ = automation.SetConnectionTimeout(200);
            let _ = automation.SetTransactionTimeout(200);
            if let Ok(automation) = automation.cast::<IUIAutomation>() {
                // Invalidate local
                // geometry directly when Windows changes Terminal panes, then
                // revalidate the pinned UIA identity off the hook thread.
                let handler: IUIAutomationFocusChangedEventHandler =
                    FocusChanged(focus.clone()).into();
                let watching_focus = focus
                    .upgrade()
                    .is_some_and(|focus| focus.geometry.pin_pane.load(Ordering::Acquire))
                    && automation
                        .AddFocusChangedEventHandler(None, &handler)
                        .is_ok();
                let mut owner = None;
                while receiver.recv() == Ok(true) {
                    let Some(focus) = focus.upgrade() else { break };
                    let epoch = focus.geometry.epoch.load(Ordering::Acquire);
                    let host = focus.host.load(Ordering::Acquire);
                    // A provider can finish after a pane/size change. Commit
                    // identity together with valid geometry, never from a
                    // discarded or partially failed asynchronous read.
                    let mut candidate_owner = owner.clone();
                    let grid = if host != 0 && super::foreground_terminal() == host {
                        read_grid(
                            &automation,
                            host,
                            if focus.geometry.pin_pane.load(Ordering::Acquire) {
                                Some(&mut candidate_owner)
                            } else {
                                None
                            },
                        )
                    } else {
                        None
                    };
                    if let Ok(mut cached) = focus.geometry.grid.lock()
                        && epoch == focus.geometry.epoch.load(Ordering::Acquire)
                    {
                        if grid.is_some() {
                            owner = candidate_owner;
                        }
                        focus.geometry.pane_valid.store(grid.is_some(), Ordering::Release);
                        *cached = grid;
                    }
                    focus.geometry.pending.store(false, Ordering::Release);
                    // Focus or size may have changed while the provider was busy.
                    if epoch != focus.geometry.epoch.load(Ordering::Acquire) {
                        focus.geometry.refresh();
                    }
                }
                if watching_focus {
                    let _ = automation.RemoveFocusChangedEventHandler(&handler);
                }
            }
        }
        CoUninitialize();
    }
}

fn window_bounds(host: isize) -> Option<[i32; 4]> {
    let mut rect = RECT::default();
    // SAFETY: caller supplies a foreground HWND; rect is writable storage.
    (host != 0 && unsafe { GetWindowRect(host as _, &mut rect) } != 0).then_some([
        rect.left,
        rect.top,
        rect.right,
        rect.bottom,
    ])
}

struct Rectangles(*mut SAFEARRAY);
impl Drop for Rectangles {
    fn drop(&mut self) {
        // SAFETY: GetBoundingRectangles transfers this SAFEARRAY to the caller.
        unsafe {
            let _ = SafeArrayDestroy(self.0);
        }
    }
}

fn read_grid(
    automation: &IUIAutomation,
    host: isize,
    owner: Option<&mut Option<Vec<i32>>>,
) -> Option<TextGrid> {
    // SAFETY: these read-only UIA calls run in an initialized COM apartment.
    unsafe {
        let window = window_bounds(host)?;
        let element = automation.GetFocusedElement().ok()?;
        if element.CurrentClassName().ok()? != "TermControl"
            || !element.CurrentHasKeyboardFocus().ok()?.as_bool()
        {
            return None;
        }
        if let Some(owner) = owner {
            // Bind native input capture's
            // geometry to the original UIA pane, so switching to another pane
            // in the same Terminal window can never redirect captured input.
            let ids = Rectangles(element.GetRuntimeId().ok()?);
            if ids.0.is_null() {
                return None;
            }
            let lower = SafeArrayGetLBound(ids.0, 1).ok()?;
            let upper = SafeArrayGetUBound(ids.0, 1).ok()?;
            if upper < lower || upper - lower > 256 {
                return None;
            }
            let mut identity = Vec::with_capacity((upper - lower + 1) as usize);
            for index in lower..=upper {
                let mut value = 0i32;
                SafeArrayGetElement(ids.0, &index, (&mut value as *mut i32).cast()).ok()?;
                identity.push(value);
            }
            if let Some(pinned) = owner.as_ref() {
                if pinned != &identity {
                    return None;
                }
            } else {
                *owner = Some(identity);
            }
        }
        let pattern: IUIAutomationTextPattern =
            element.GetCurrentPatternAs(UIA_TextPatternId).ok()?;
        let range = pattern.GetVisibleRanges().ok()?.GetElement(0).ok()?;
        let rectangles = Rectangles(range.GetBoundingRectangles().ok()?);
        if rectangles.0.is_null() {
            return None;
        }
        let lower = SafeArrayGetLBound(rectangles.0, 1).ok()?;
        let upper = SafeArrayGetUBound(rectangles.0, 1).ok()?;
        if upper < lower || (upper - lower + 1) % 4 != 0 {
            return None;
        }
        let mut bounds = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for offset in (lower..=upper).step_by(4) {
            let mut rect = [0.0f64; 4];
            for (index, value) in rect.iter_mut().enumerate() {
                SafeArrayGetElement(
                    rectangles.0,
                    &(offset + index as i32),
                    (value as *mut f64).cast(),
                )
                .ok()?;
            }
            if !rect.iter().all(|n| n.is_finite()) || rect[2] <= 0.0 || rect[3] <= 0.0 {
                return None;
            }
            bounds[0] = bounds[0].min(rect[0]);
            bounds[1] = bounds[1].min(rect[1]);
            bounds[2] = bounds[2].max(rect[0] + rect[2]);
            bounds[3] = bounds[3].max(rect[1] + rect[3]);
        }
        let (columns, rows) = crossterm::terminal::size().ok()?;
        if super::foreground_terminal() != host || window_bounds(host)? != window {
            return None;
        }
        TextGrid::new(host, window, bounds, columns, rows)
    }
}

#[cfg(test)]
pub(super) fn test_grid() -> TextGrid {
    TextGrid::new(12, [0, 0, 1000, 600], [20.0, 50.0, 820.0, 450.0], 80, 20).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_reference_corrects_cell_offsets_without_hardcoded_terminal_padding() {
        let raw = test_grid();
        let point = POINT { x: 55, y: 91 };
        let corrected = raw.with_native_reference(raw, point, (3, 0)).unwrap();
        assert_eq!(corrected.position(point, false), Some((3, 0)));
        for row in 0..18 {
            for column in 0..80 {
                assert_eq!(
                    corrected.position(
                        POINT {
                            x: 20 + column * 10,
                            y: 90 + row * 20
                        },
                        false
                    ),
                    Some((column as u16, row as u16))
                );
            }
        }
        let resized = TextGrid::new(12, raw.window, raw.bounds, 80, 21).unwrap();
        assert!(resized.with_native_reference(raw, point, (3, 0)).is_none());
        let moved = TextGrid::new(12, [1, 0, 1001, 600], raw.bounds, 80, 20).unwrap();
        assert!(moved.with_native_reference(raw, point, (3, 0)).is_none());
        assert!(raw.with_native_reference(raw, point, (80, 0)).is_none());
        assert!(raw.with_native_reference(raw, point, (0, 20)).is_none());
    }

    #[test]
    fn mouse_callback_keeps_physical_geometry_from_an_unaware_dpi_context() {
        use windows_sys::Win32::UI::HiDpi::{
            AreDpiAwarenessContextsEqual, DPI_AWARENESS_CONTEXT_UNAWARE,
            GetThreadDpiAwarenessContext,
        };
        use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow};

        // Confine DPI changes and this invisible test window to a fresh thread.
        // No foreground window, user input or terminal geometry is modified.
        std::thread::spawn(|| unsafe {
            struct TestWindow(windows_sys::Win32::Foundation::HWND);
            impl Drop for TestWindow {
                fn drop(&mut self) {
                    unsafe { DestroyWindow(self.0) };
                }
            }
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_UNAWARE);
            let caller_dpi = GetThreadDpiAwarenessContext();
            let (window, tracker, point) = super::super::with_physical_coordinates(|| {
                let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
                let window = TestWindow(CreateWindowExW(
                    0,
                    class.as_ptr(),
                    std::ptr::null(),
                    0,
                    100,
                    100,
                    1000,
                    600,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                ));
                assert!(!window.0.is_null());
                let bounds = window_bounds(window.0 as isize).unwrap();
                let tracker = Tracker::default();
                *tracker.grid.lock().unwrap() =
                    TextGrid::new(window.0 as isize, bounds, bounds.map(f64::from), 100, 30);
                let point = POINT {
                    x: bounds[0] + 55,
                    y: bounds[1] + 91,
                };
                (window, tracker, point)
            });

            for _ in 0..2 {
                assert_ne!(
                    AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), caller_dpi),
                    0
                );
                let grid = super::super::with_physical_coordinates(|| {
                    assert_ne!(
                        AreDpiAwarenessContextsEqual(
                            GetThreadDpiAwarenessContext(),
                            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
                        ),
                        0
                    );
                    // Without the callback override, a scaled display rejects
                    // this cache as if the window had moved on every event.
                    tracker
                        .current(window.0 as isize)
                        .expect("physical geometry must remain valid")
                });
                assert_eq!(grid.position(point, false), Some((5, 4)));
                assert_ne!(
                    AreDpiAwarenessContextsEqual(GetThreadDpiAwarenessContext(), caller_dpi),
                    0
                );
            }

            let (refresh, requests) = mpsc::sync_channel(1);
            tracker.sender.set(refresh).unwrap();
            tracker.grid.lock().unwrap().as_mut().unwrap().updated = Instant::now() - Duration::from_secs(1);
            assert!(super::super::with_physical_coordinates(|| tracker.cached(window.0 as isize)).is_some());
            assert!(requests.try_recv().is_err(), "an ordinary press never requests stale-geometry UIA work");
            assert!(super::super::with_physical_coordinates(|| tracker.current(window.0 as isize)).is_some());
            assert_eq!(requests.try_recv().unwrap(), true, "special gestures retain explicit geometry refresh");
        })
        .join()
        .unwrap();
    }

    #[test]
    fn physical_cells_exclude_terminal_chrome_and_clamp_drags() {
        let grid = test_grid();
        assert_eq!(grid.position(POINT { x: 55, y: 91 }, false), Some((3, 2)));
        for (x, y) in [(19, 50), (20, 49), (820, 100), (100, 450)] {
            assert_eq!(grid.position(POINT { x, y }, false), None);
        }
        assert_eq!(
            grid.position(POINT { x: -200, y: 1000 }, true),
            Some((0, 19))
        );
    }

    #[test]
    fn scaled_split_panes_and_negative_monitor_coordinates() {
        let grid = TextGrid::new(
            12,
            [-2000, 0, 0, 1000],
            [-1800.0, 90.0, -600.0, 690.0],
            80,
            20,
        )
        .unwrap();
        assert_eq!(
            grid.position(POINT { x: -1748, y: 153 }, false),
            Some((3, 2))
        );
        assert!(grid.position(POINT { x: -500, y: 153 }, false).is_none());
        assert!(TextGrid::new(12, [0; 4], [0.0; 4], 0, 20).is_none());
    }
}
