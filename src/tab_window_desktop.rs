//! Screen-space tab drops on terminals that expose their top-level window.
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

pub(super) type Host = u64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ScreenPoint {
    pub x: i32,
    pub y: i32,
}

#[derive(Clone, Copy)]
pub(crate) struct Drop {
    pub host: Option<Host>,
    pub point: ScreenPoint,
}

#[derive(Clone, Copy)]
pub(crate) struct Press {
    pub host: Host,
    point: ScreenPoint,
    time: Instant,
}

struct Gesture {
    press: Press,
    release: Option<Drop>,
    armed: bool,
}

#[derive(Default)]
pub(crate) struct Tracker(Mutex<Option<Gesture>>);

pub(crate) enum Release {
    Waiting,
    Cancelled,
    Dropped(Drop),
}

impl Tracker {
    pub(super) fn observe(&self, pressed: bool, sample: Drop, wake: &crate::background::UiWake) {
        let Ok(mut gesture) = self.0.try_lock() else {
            return;
        };
        if pressed {
            let previously_armed = gesture.as_ref().is_some_and(|value| value.armed);
            *gesture = sample.host.map(|host| Gesture {
                press: Press {
                    host,
                    point: sample.point,
                    time: Instant::now(),
                },
                release: None,
                armed: false,
            });
            if previously_armed {
                wake.notify();
            }
        } else if let Some(gesture) = gesture.as_mut() {
            gesture.release = Some(sample);
            if gesture.armed {
                wake.notify();
            }
        }
    }

    pub(super) fn arm(&self) -> Option<Press> {
        let mut gesture = self.0.lock().ok()?;
        let gesture = gesture.as_mut()?;
        if gesture.armed || gesture.press.time.elapsed() > Duration::from_secs(2) {
            return None;
        }
        gesture.armed = true;
        Some(gesture.press)
    }

    pub(super) fn release(&self, press: Press) -> Release {
        let Ok(mut gesture) = self.0.lock() else {
            return Release::Cancelled;
        };
        let Some(value) = gesture
            .as_ref()
            .filter(|value| value.press.time == press.time)
        else {
            return Release::Cancelled;
        };
        let Some(drop) = value.release else {
            return Release::Waiting;
        };
        *gesture = None;
        let moved = press
            .point
            .x
            .abs_diff(drop.point.x)
            .max(press.point.y.abs_diff(drop.point.y));
        if drop.host == Some(press.host) || moved < 5 {
            Release::Cancelled
        } else {
            Release::Dropped(drop)
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{POINT, RECT},
        System::Console::GetConsoleWindow,
        UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON},
        UI::WindowsAndMessaging::{
            GA_ROOT, GetAncestor, GetCursorPos, GetForegroundWindow, GetWindowRect,
            IsWindowVisible, WM_LBUTTONDOWN, WM_LBUTTONUP, WindowFromPoint,
        },
    };

    pub(crate) fn foreground_host() -> Option<Host> {
        // SAFETY: read-only queries for the visible console or Terminal host.
        unsafe {
            let console = GetConsoleWindow();
            if !console.is_null() && IsWindowVisible(console) != 0 {
                return Some(console as Host);
            }
            let window = crate::windows_mouse::terminal_window(GetForegroundWindow() as isize);
            (window != 0).then_some(window as Host)
        }
    }

    fn host_at(point: POINT) -> Option<Host> {
        // SAFETY: the hook passes physical coordinates. Use the visible root
        // under the pointer so overlapping and snapped windows work correctly.
        let host = unsafe { GetAncestor(WindowFromPoint(point), GA_ROOT) };
        (!host.is_null()).then_some(host as Host)
    }

    pub(crate) fn released_pointer() -> Option<Drop> {
        // A console may report Up before the hook's matching button event
        // reaches the workbench. Sample the desktop only at that boundary.
        crate::windows_mouse::with_physical_coordinates(|| {
            let mut point = POINT::default();
            if unsafe {
                GetAsyncKeyState(i32::from(VK_LBUTTON)) < 0 || GetCursorPos(&mut point) == 0
            } {
                return None;
            }
            Some(Drop {
                host: host_at(point),
                point: ScreenPoint {
                    x: point.x,
                    y: point.y,
                },
            })
        })
    }

    impl Tracker {
        pub(crate) fn observe_windows(
            &self,
            message: u32,
            point: POINT,
            wake: &crate::background::UiWake,
        ) {
            if matches!(message, WM_LBUTTONDOWN | WM_LBUTTONUP) {
                self.observe(
                    message == WM_LBUTTONDOWN,
                    Drop {
                        host: host_at(point),
                        point: ScreenPoint {
                            x: point.x,
                            y: point.y,
                        },
                    },
                    wake,
                );
            }
        }
    }

    pub(crate) fn launch_position(point: ScreenPoint) -> (i32, i32) {
        use windows_sys::Win32::Graphics::Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
        };
        let mut monitor = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let raw = POINT {
            x: point.x,
            y: point.y,
        };
        let work = unsafe {
            let handle = MonitorFromPoint(raw, MONITOR_DEFAULTTONEAREST);
            (GetMonitorInfoW(handle, &mut monitor) != 0).then_some(monitor.rcWork)
        };
        work.map_or((point.x, point.y), |area| {
            (
                point.x.clamp(area.left, (area.right - 240).max(area.left)),
                point.y.clamp(area.top, (area.bottom - 160).max(area.top)),
            )
        })
    }

    pub(crate) fn valid_host(host: Host) -> bool {
        let mut area = RECT::default();
        // SAFETY: a stale HWND is rejected by the OS without dereferencing it.
        unsafe { GetWindowRect(host as _, &mut area) != 0 && IsWindowVisible(host as _) != 0 }
    }

    pub(crate) fn focus(host: Host) {
        // Best effort. Windows owns activation across foreground processes.
        unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::SetForegroundWindow(host as _);
        }
    }
}

#[cfg(windows)]
pub(crate) use windows::{focus, foreground_host, launch_position, released_pointer, valid_host};

#[cfg(target_os = "linux")]
#[path = "tab_window_x11.rs"]
mod x11;

#[cfg(target_os = "linux")]
pub(super) use x11::{foreground_host, released_pointer, start_tracker, valid_host};

#[cfg(not(any(windows, target_os = "linux")))]
pub(super) fn foreground_host() -> Option<Host> {
    None
}

#[cfg(not(any(windows, target_os = "linux")))]
pub(super) fn valid_host(_host: Host) -> bool {
    false
}

#[cfg(not(any(windows, target_os = "linux")))]
pub(super) fn released_pointer() -> Option<Drop> {
    None
}

#[cfg(not(windows))]
pub(super) fn focus(_host: Host) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(host: Option<Host>, x: i32) -> Drop {
        Drop {
            host,
            point: ScreenPoint { x, y: 150 },
        }
    }

    #[test]
    fn tracks_a_cross_window_drop_even_when_up_precedes_terminal_input() {
        let wake = crate::background::UiWake::default();
        let tracker = Tracker::default();
        tracker.observe(true, sample(Some(10), 100), &wake);
        tracker.observe(false, sample(Some(20), 500), &wake);
        let press = tracker.arm().unwrap();
        assert_eq!(press.host, 10);
        assert!(matches!(
            tracker.release(press),
            Release::Dropped(Drop { host: Some(20), .. })
        ));
        assert!(matches!(tracker.release(press), Release::Cancelled));
    }

    #[test]
    fn same_window_and_sub_threshold_releases_keep_local_drag_handling() {
        let wake = crate::background::UiWake::default();
        let tracker = Tracker::default();
        for end in [sample(Some(10), 500), sample(None, 102)] {
            tracker.observe(true, sample(Some(10), 100), &wake);
            let press = tracker.arm().unwrap();
            assert!(matches!(tracker.release(press), Release::Waiting));
            tracker.observe(false, end, &wake);
            assert!(matches!(tracker.release(press), Release::Cancelled));
        }
    }
}
