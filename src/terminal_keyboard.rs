//! Encode keys for a terminal that requests Microsoft's Win32 input protocol.
//! https://github.com/microsoft/terminal/blob/main/doc/specs/%234999%20-%20Improved%20keyboard%20handling%20in%20Conpty.md

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::fmt::Write;

pub(super) fn win32_key_sequence(key: KeyEvent) -> Option<Vec<u8>> {
    let mut output = String::new();
    for_each_win32_key(key, |vk, unit, down, state| { write!(&mut output, "\x1b[{vk};0;{unit};{down};{state};1_").unwrap(); })?;
    Some(output.into_bytes())
}

pub(crate) fn for_each_win32_key(key: KeyEvent, mut emit: impl FnMut(u16, u16, u8, u32)) -> Option<()> {
    let mut modifiers = key.modifiers;
    let (vk, character) = match key.code {
        KeyCode::Char(character) => {
            let vk = virtual_key(character);
            let character = if modifiers.contains(KeyModifiers::CONTROL)
                && !modifiers.contains(KeyModifiers::ALT)
            {
                super::control_byte(character)
                    .map(char::from)
                    .unwrap_or(character)
            } else {
                character
            };
            (vk, character)
        }
        KeyCode::Backspace => (0x08, '\x08'),
        KeyCode::Tab => (0x09, '\t'),
        KeyCode::BackTab => {
            modifiers |= KeyModifiers::SHIFT;
            (0x09, '\t')
        }
        KeyCode::Enter => (0x0d, '\r'),
        KeyCode::Esc => (0x1b, '\x1b'),
        KeyCode::PageUp => (0x21, '\0'),
        KeyCode::PageDown => (0x22, '\0'),
        KeyCode::End => (0x23, '\0'),
        KeyCode::Home => (0x24, '\0'),
        KeyCode::Left => (0x25, '\0'),
        KeyCode::Up => (0x26, '\0'),
        KeyCode::Right => (0x27, '\0'),
        KeyCode::Down => (0x28, '\0'),
        KeyCode::Insert => (0x2d, '\0'),
        KeyCode::Delete => (0x2e, '\0'),
        KeyCode::F(number @ 1..=24) => (0x6f + u16::from(number), '\0'),
        _ => return None,
    };
    let mut state = 0;
    if modifiers.contains(KeyModifiers::SHIFT) {
        state |= 0x10;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        state |= 0x08;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        // The terminal's existing Ctrl+Alt text policy treats this as AltGr.
        // Windows identifies AltGr by Right Alt together with Control.
        state |=
            if modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char(_)) {
                0x01
            } else {
                0x02
            };
    }
    let down = u8::from(key.kind != KeyEventKind::Release);
    for unit in character.encode_utf16(&mut [0; 2]) {
        emit(vk, *unit, down, state);
    }
    Some(())
}

fn virtual_key(character: char) -> u16 {
    match character {
        'a'..='z' | 'A'..='Z' => return character.to_ascii_uppercase() as u16,
        '0'..='9' | ' ' => return character as u16,
        _ => {}
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayout, VkKeyScanExW};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetForegroundWindow, GetWindowThreadProcessId,
        };
        if character as u32 <= u16::MAX as u32 {
            // Match Crossterm's choice of the foreground keyboard layout.
            let vk = unsafe {
                let thread = GetWindowThreadProcessId(GetForegroundWindow(), std::ptr::null_mut());
                VkKeyScanExW(character as u16, GetKeyboardLayout(thread))
            };
            if vk != -1 {
                return (vk as u16) & 0xff;
            }
        }
    }
    // Unmapped Unicode travels as UTF-16 text, without inventing a physical key.
    0
}
