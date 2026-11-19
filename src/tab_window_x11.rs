//! XInput2 delivers button releases even when the terminal owns the pointer grab.
use std::{
    cell::OnceCell,
    ffi::OsStr,
    sync::{Arc, Weak},
};
use x11rb::{
    connection::Connection,
    protocol::{
        Event,
        xinput::{ConnectionExt as _, Device, EventMask, XIEventMask},
        xproto::{ConnectionExt as _, KeyButMask, MapState, Window},
    },
    rust_connection::RustConnection,
};

use super::{Drop, Host, ScreenPoint, Tracker};
use crate::background::UiWake;

struct Display {
    connection: RustConnection,
    root: Window,
}

impl Display {
    fn connect() -> Option<Self> {
        if !has_x11_display() {
            return None;
        }
        let (connection, screen) = x11rb::connect(None).ok()?;
        let root = connection.setup().roots.get(screen)?.root;
        Some(Self { connection, root })
    }

    fn pointer(&self, only_released: bool) -> Option<Drop> {
        // Query the root rather than the grabbed terminal. Its child is the
        // topmost window under the pointer even during an implicit grab.
        let point = self
            .connection
            .query_pointer(self.root)
            .ok()?
            .reply()
            .ok()?;
        (point.same_screen && (!only_released || !point.mask.contains(KeyButMask::BUTTON1)))
            .then_some(Drop {
                host: (point.child != 0).then_some(u64::from(point.child)),
                point: ScreenPoint {
                    x: i32::from(point.root_x),
                    y: i32::from(point.root_y),
                },
            })
    }

    fn foreground(&self) -> Option<Host> {
        let mut window = self.connection.get_input_focus().ok()?.reply().ok()?.focus;
        if window == 1 {
            return self.pointer(false)?.host;
        } // PointerRoot
        while window != 0 && window != self.root {
            let tree = self.connection.query_tree(window).ok()?.reply().ok()?;
            if tree.parent == self.root {
                return Some(u64::from(window));
            }
            window = tree.parent;
        }
        None
    }
}

fn has_x11_display() -> bool {
    std::env::var_os("DISPLAY").is_some()
        && (std::env::var_os("WAYLAND_DISPLAY").is_none()
            || std::env::var_os("XDG_SESSION_TYPE").as_deref() == Some(OsStr::new("x11")))
}

thread_local! {
    static DISPLAY: OnceCell<Option<Display>> = const { OnceCell::new() };
}

pub(crate) fn foreground_host() -> Option<Host> {
    DISPLAY.with(|display| display.get_or_init(Display::connect).as_ref()?.foreground())
}

pub(crate) fn released_pointer() -> Option<Drop> {
    DISPLAY.with(|display| {
        display
            .get_or_init(Display::connect)
            .as_ref()?
            .pointer(true)
    })
}

pub(crate) fn valid_host(host: Host) -> bool {
    DISPLAY.with(|display| {
        let Some(display) = display.get_or_init(Display::connect).as_ref() else {
            return false;
        };
        display
            .connection
            .get_window_attributes(host as Window)
            .ok()
            .and_then(|cookie| cookie.reply().ok())
            .is_some_and(|attributes| attributes.map_state == MapState::VIEWABLE)
    })
}

pub(crate) fn start_tracker(wake: UiWake) -> Option<Arc<Tracker>> {
    if !has_x11_display()
        || std::env::var_os("TERM_PROGRAM").as_deref() == Some(OsStr::new("vtcode"))
    {
        return None;
    }
    let tracker = Arc::new(Tracker::default());
    let weak = Arc::downgrade(&tracker);
    std::thread::Builder::new()
        .name("window-x11-pointer".into())
        .spawn(move || track(weak, wake))
        .ok()?;
    Some(tracker)
}

fn track(tracker: Weak<Tracker>, wake: UiWake) -> Option<()> {
    let display = Display::connect()?;
    let version = display
        .connection
        .xinput_xi_query_version(2, 1)
        .ok()?
        .reply()
        .ok()?;
    if (version.major_version, version.minor_version) < (2, 1) {
        return None;
    }
    display
        .connection
        .xinput_xi_select_events(
            display.root,
            &[EventMask {
                deviceid: u16::from(Device::ALL_MASTER),
                mask: vec![XIEventMask::RAW_BUTTON_PRESS | XIEventMask::RAW_BUTTON_RELEASE],
            }],
        )
        .ok()?
        .check()
        .ok()?;
    while let Ok(event) = display.connection.wait_for_event() {
        let pressed = match event {
            Event::XinputRawButtonPress(event) if event.detail == 1 => true,
            Event::XinputRawButtonRelease(event) if event.detail == 1 => false,
            _ => continue,
        };
        let tracker = tracker.upgrade()?;
        if let Some(sample) = display.pointer(false) {
            tracker.observe(pressed, sample, &wake);
        }
    }
    None
}
