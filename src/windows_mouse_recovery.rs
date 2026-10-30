//! Recover hooks Windows silently removes after a timeout. Raw mouse activity
//! remains available independently of WH_MOUSE_LL, including Shift+wheel that
//! Terminal never forwards. There is no timer or work while the mouse is idle.

use std::cell::Cell;
use std::io;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Input::{
    RAWINPUTDEVICE, RIDEV_INPUTSINK, RIDEV_REMOVE, RegisterRawInputDevices,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, HHOOK, HWND_MESSAGE, MSG, SetWindowsHookExW,
    UnhookWindowsHookEx, WH_MOUSE_LL, WM_INPUT,
};

const RECOVERY_DELAY: Duration = Duration::from_millis(250);

thread_local! {
    static LAST_CALLBACK: Cell<Option<Instant>> = const { Cell::new(None) };
}

pub(super) fn note_callback() {
    LAST_CALLBACK.set(Some(Instant::now()));
}

pub(super) struct Hook {
    handle: HHOOK,
    last_attempt: Instant,
}

impl Hook {
    pub(super) fn install() -> io::Result<Self> {
        // SAFETY: this module is used only on the hook's message-loop thread;
        // the callback and executable module outlive the hook.
        let handle = unsafe {
            SetWindowsHookExW(
                WH_MOUSE_LL,
                Some(super::mouse_hook),
                GetModuleHandleW(std::ptr::null()),
                0,
            )
        };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            handle,
            last_attempt: Instant::now(),
        })
    }

    pub(super) fn recover_if_stalled(&mut self, now: Instant) -> bool {
        let last_seen = LAST_CALLBACK
            .get()
            .unwrap_or(self.last_attempt)
            .max(self.last_attempt);
        if now.saturating_duration_since(last_seen) < RECOVERY_DELAY {
            return false;
        }
        // Rate-limit retries if Windows temporarily refuses a new hook. Keep
        // the old handle until replacement succeeds, on this same thread.
        self.last_attempt = now;
        let Ok(replacement) = Self::install() else {
            return false;
        };
        *self = replacement;
        true
    }
}

impl Drop for Hook {
    fn drop(&mut self) {
        // An already-removed hook can legitimately fail to unhook.
        if !self.handle.is_null() {
            unsafe {
                UnhookWindowsHookEx(self.handle);
            }
        }
    }
}

pub(super) struct ActivityWindow(HWND);

impl ActivityWindow {
    pub(super) fn start() -> io::Result<Self> {
        // A message-only built-in window needs no class registration or UI.
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let window = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                std::ptr::null(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        if window.is_null() {
            return Err(io::Error::last_os_error());
        }
        let device = RAWINPUTDEVICE {
            usUsagePage: 1,
            usUsage: 2,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: window,
        };
        // No NOLEGACY flag: ordinary terminal input is unchanged. Raw events
        // only check liveness; the hook remains the sole gesture producer.
        if unsafe { RegisterRawInputDevices(&device, 1, std::mem::size_of_val(&device) as u32) }
            == 0
        {
            let error = io::Error::last_os_error();
            unsafe {
                DestroyWindow(window);
            }
            return Err(error);
        }
        Ok(Self(window))
    }

    pub(super) fn owns(&self, message: &MSG) -> bool {
        message.hwnd == self.0 && message.message == WM_INPUT
    }
}

impl Drop for ActivityWindow {
    fn drop(&mut self) {
        let device = RAWINPUTDEVICE {
            usUsagePage: 1,
            usUsage: 2,
            dwFlags: RIDEV_REMOVE,
            hwndTarget: std::ptr::null_mut(),
        };
        // The registration and window are created and retired on one thread.
        unsafe {
            RegisterRawInputDevices(&device, 1, std::mem::size_of_val(&device) as u32);
            DestroyWindow(self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_activity_rearms_a_silently_removed_hook_without_terminal_input() {
        // No mouse input is generated or intercepted: the callback has no
        // focused pane. Remove our own real hook to model Windows' timeout.
        std::thread::spawn(|| {
            use windows_sys::Win32::UI::Input::GetRegisteredRawInputDevices;
            use windows_sys::Win32::UI::WindowsAndMessaging::{IsWindow, IsWindowVisible};
            let activity = ActivityWindow::start().unwrap();
            assert_eq!(unsafe { IsWindowVisible(activity.0) }, 0);
            let mut count = 0;
            let size = std::mem::size_of::<RAWINPUTDEVICE>() as u32;
            assert_eq!(
                unsafe { GetRegisteredRawInputDevices(std::ptr::null_mut(), &mut count, size) },
                0
            );
            let mut devices = vec![RAWINPUTDEVICE::default(); count as usize];
            assert_ne!(
                unsafe { GetRegisteredRawInputDevices(devices.as_mut_ptr(), &mut count, size) },
                u32::MAX
            );
            assert!(devices.iter().any(|device| device.usUsagePage == 1
                && device.usUsage == 2
                && device.hwndTarget == activity.0
                && device.dwFlags == RIDEV_INPUTSINK));
            assert!(activity.owns(&MSG {
                hwnd: activity.0,
                message: WM_INPUT,
                ..Default::default()
            }));
            let mut hook = Hook::install().unwrap();
            assert_ne!(unsafe { UnhookWindowsHookEx(hook.handle) }, 0);
            let now = hook.last_attempt + Duration::from_secs(1);
            LAST_CALLBACK.set(Some(now - Duration::from_secs(1)));
            assert!(hook.recover_if_stalled(now));
            assert_ne!(
                unsafe {
                    UnhookWindowsHookEx(std::mem::replace(&mut hook.handle, std::ptr::null_mut()))
                },
                0,
                "raw activity must install a live replacement after removal"
            );
            let window = activity.0;
            drop(activity);
            assert_eq!(
                unsafe { IsWindow(window) },
                0,
                "shutdown must retire the activity window"
            );
        })
        .join()
        .unwrap();
    }

    #[test]
    fn healthy_mouse_callbacks_and_recent_attempts_do_not_reinstall_the_hook() {
        std::thread::spawn(|| {
            let mut hook = Hook::install().unwrap();
            let now = hook.last_attempt + Duration::from_secs(1);
            LAST_CALLBACK.set(Some(now));
            assert!(!hook.recover_if_stalled(now));
            LAST_CALLBACK.set(None);
            assert!(!hook.recover_if_stalled(hook.last_attempt + RECOVERY_DELAY / 2));
            assert_ne!(
                unsafe {
                    UnhookWindowsHookEx(std::mem::replace(&mut hook.handle, std::ptr::null_mut()))
                },
                0,
                "healthy callbacks must retain the existing hook"
            );
        })
        .join()
        .unwrap();
    }
}
