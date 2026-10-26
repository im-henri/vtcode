//! Request unambiguous modified keys on supporting terminals, including SSH.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::event::{
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};

static KEYBOARD_PROTOCOL_ENABLED: AtomicBool = AtomicBool::new(false);

pub(crate) struct KeyboardProtocol;

impl KeyboardProtocol {
    pub(crate) fn enable(output: &mut impl Write) -> io::Result<Self> {
        let guard = Self;
        KEYBOARD_PROTOCOL_ENABLED.store(true, Ordering::Relaxed);
        // Unsupported terminals ignore this request; do not wait for a probe
        // response on startup. Ordinary text keeps its compact byte encoding.
        crossterm::execute!(
            output,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
        Ok(guard)
    }
}

pub(crate) fn restore_keyboard_protocol() {
    if KEYBOARD_PROTOCOL_ENABLED.swap(false, Ordering::Relaxed) {
        let _ = crossterm::execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
}

impl Drop for KeyboardProtocol {
    fn drop(&mut self) {
        // Also used by the panic hook, before leaving the alternate screen;
        // the atomic flag prevents a second pop during stack unwinding.
        restore_keyboard_protocol();
    }
}

#[cfg(test)]
#[path = "unix_input_tests.rs"]
mod tests;
