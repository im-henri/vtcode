use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::App;
use crate::app_event_loop::write_csi_mod;

#[path = "terminal_keyboard.rs"]
mod keyboard;
#[cfg(windows)]
pub(crate) use keyboard::for_each_win32_key;

impl App {
    pub(crate) fn focused_terminal_is_fullscreen(&self) -> bool {
        self.focused_terminal_index()
            .and_then(|index| self.terminals[index].terminal_pty.as_ref())
            .is_some_and(|pty| pty.grid.is_alt_screen())
    }

    /// Handle keyboard input for a terminal editor tab.
    /// Returns `Some(false)` when the key was consumed, `None` to fall through.
    pub(crate) fn handle_terminal_tab_key(&mut self, key: KeyEvent) -> Option<bool> {
        if self.active_pane != crate::app::ActivePane::Editor || self.docked_terminal_has_focus() {
            return None;
        }
        let index = self
            .tabs
            .get(self.active_tab)
            .filter(|tab| tab.kind == crate::app::TabKind::Terminal)
            .and_then(|tab| tab.terminal_idx)?;
        self.handle_terminal_key_at(index, key)
    }

    /// Handle keyboard input for a focused terminal in either workbench dock.
    /// Returns `Some(false)` when the key was consumed, `None` to fall through.
    pub(crate) fn handle_panel_terminal_key(&mut self, key: KeyEvent) -> Option<bool> {
        if !self.docked_terminal_has_focus() {
            return None;
        }
        self.handle_terminal_key_at(self.active_terminal, key)
    }

    fn handle_terminal_key_at(&mut self, index: usize, key: KeyEvent) -> Option<bool> {
        self.handle_terminal_key_at_with_copy(index, key, |text| {
            crate::clipboard::set_text(text).is_ok()
        })
    }

    fn handle_terminal_key_at_with_copy(
        &mut self,
        index: usize,
        key: KeyEvent,
        copy: impl FnOnce(&str) -> bool,
    ) -> Option<bool> {
        if index >= self.terminals.len() {
            return Some(false);
        }

        // This is a workbench shortcut even while the terminal has focus.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('`') {
            return None;
        }

        if self.terminals[index].terminal_pty.is_none() {
            if key.code == KeyCode::Enter && self.terminals[index].terminal_launch_error.is_some() {
                self.retry_terminal_pty_at(index, 80, 24);
            } else {
                self.ensure_terminal_pty_at(index, 80, 24);
            }
            if self.terminals[index].terminal_pty.is_none() {
                // A failed PTY is an error state, never a homemade text prompt.
                return Some(false);
            }
        }

        let app_cursor = self.terminals[index]
            .terminal_pty
            .as_ref()
            .is_some_and(|pty| pty.grid.application_cursor_keys);
        let mods = key.modifiers;
        let fullscreen = self.terminals[index]
            .terminal_pty
            .as_ref()
            .is_some_and(|pty| pty.grid.is_alt_screen());
        let ctrl = mods.contains(KeyModifiers::CONTROL) && !mods.contains(KeyModifiers::ALT);
        let shift = mods.contains(KeyModifiers::SHIFT);
        let selected = self.terminals[index]
            .terminal_pty
            .as_ref()
            .is_some_and(|pty| pty.grid.has_selection());
        let copy_key = ctrl && matches!(key.code, KeyCode::Char('c' | 'C') | KeyCode::Insert);
        if copy_key && (selected || (!fullscreen && (shift || key.code == KeyCode::Insert))) {
            self.copy_terminal_selection_with(index, copy);
            return Some(false);
        }
        if !fullscreen
            && ((ctrl && matches!(key.code, KeyCode::Char('v' | 'V')))
                || (shift
                    && !mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && key.code == KeyCode::Insert))
        {
            self.paste_terminal_clipboard(index);
            return Some(false);
        }
        if selected && key.code == KeyCode::Esc {
            if let Some(pty) = self.terminals[index].terminal_pty.as_mut() {
                pty.grid.clear_selection();
            }
            return Some(false);
        }

        if !fullscreen && mods.contains(KeyModifiers::SHIFT) {
            match key.code {
                KeyCode::PageUp => {
                    if let Some(pty) = &mut self.terminals[index].terminal_pty {
                        pty.scroll_up(5);
                    }
                    return Some(false);
                }
                KeyCode::PageDown => {
                    if let Some(pty) = &mut self.terminals[index].terminal_pty {
                        pty.scroll_down(5);
                    }
                    return Some(false);
                }
                _ => {}
            }
        }

        if let Some(pty) = &mut self.terminals[index].terminal_pty
            && pty.grid.win32_input_mode
        {
            if let Some(bytes) = keyboard::win32_key_sequence(key) {
                pty.grid.clear_selection();
                pty.grid.scroll_view = 0;
                pty.write(&bytes);
            }
            return Some(false);
        }

        write_key(key, app_cursor, false, |bytes| {
            if let Some(pty) = &mut self.terminals[index].terminal_pty {
                pty.grid.clear_selection();
                pty.grid.scroll_view = 0;
                pty.write(bytes);
            }
        });
        // Unsupported terminal keys must not become commands in the parent.
        Some(false)
    }
}

/// Shared by integrated terminals and the remote connection's login shell.
/// Encoding stays independent of App and does not invent editor commands.
pub(crate) fn write_key(key: KeyEvent, app_cursor: bool, win32: bool, mut write: impl FnMut(&[u8])) {
        if win32 {
            if let Some(bytes) = keyboard::win32_key_sequence(key) { write(&bytes); }
            return;
        }
        if key.kind == crossterm::event::KeyEventKind::Release { return; }
        let mods = key.modifiers;
        let mod_param = xterm_modifier_parameter(mods);
        let mut key_buf = [0u8; 16];
        let data: Option<&[u8]> = match key.code {
            KeyCode::Char(c) => {
                // On Windows, Ctrl+Alt is commonly AltGr. Preserve the printable
                // character instead of turning it into an unrelated control byte.
                if mods.contains(KeyModifiers::CONTROL) && mods.contains(KeyModifiers::ALT) {
                    let encoded = c.encode_utf8(&mut key_buf);
                    Some(encoded.as_bytes())
                } else if mods.contains(KeyModifiers::CONTROL) {
                    control_byte(c).map(|byte| {
                        key_buf[0] = byte;
                        &key_buf[..1]
                    })
                } else if mods.contains(KeyModifiers::ALT) {
                    key_buf[0] = 0x1b;
                    let encoded = c.encode_utf8(&mut key_buf[1..]);
                    let len = 1 + encoded.len();
                    Some(&key_buf[..len])
                } else {
                    let encoded = c.encode_utf8(&mut key_buf);
                    Some(encoded.as_bytes())
                }
            }
            KeyCode::Enter => Some(b"\r"),
            KeyCode::Backspace => {
                if mods.contains(KeyModifiers::ALT) {
                    key_buf[0] = 0x1b;
                    key_buf[1] = if mods.contains(KeyModifiers::CONTROL) { 0x08 } else { 0x7f };
                    Some(&key_buf[..2])
                } else if mods.contains(KeyModifiers::CONTROL) {
                    Some(b"\x08")
                } else {
                    Some(b"\x7f")
                }
            }
            KeyCode::Tab => {
                if mods.contains(KeyModifiers::SHIFT) {
                    Some(b"\x1b[Z")
                } else {
                    // The real shell receives Tab and performs its own completion.
                    Some(b"\t")
                }
            }
            KeyCode::BackTab => Some(b"\x1b[Z"),
            KeyCode::Esc => Some(b"\x1b"),
            KeyCode::Up => modified_cursor_key(&mut key_buf, b'A', mod_param, app_cursor),
            KeyCode::Down => modified_cursor_key(&mut key_buf, b'B', mod_param, app_cursor),
            KeyCode::Right => modified_cursor_key(&mut key_buf, b'C', mod_param, app_cursor),
            KeyCode::Left => modified_cursor_key(&mut key_buf, b'D', mod_param, app_cursor),
            KeyCode::Home => modified_cursor_key(&mut key_buf, b'H', mod_param, app_cursor),
            KeyCode::End => modified_cursor_key(&mut key_buf, b'F', mod_param, app_cursor),
            KeyCode::PageUp => Some(modified_tilde_key(&mut key_buf, 5, mod_param)),
            KeyCode::PageDown => Some(modified_tilde_key(&mut key_buf, 6, mod_param)),
            KeyCode::Delete => Some(modified_tilde_key(&mut key_buf, 3, mod_param)),
            KeyCode::Insert => Some(modified_tilde_key(&mut key_buf, 2, mod_param)),
            KeyCode::F(number) => function_key_sequence(&mut key_buf, number, mod_param),
            _ => None,
        };

        if let Some(bytes) = data { write(bytes); }
}

fn xterm_modifier_parameter(modifiers: KeyModifiers) -> u8 {
    let mut value = 0u8;
    if modifiers.contains(KeyModifiers::SHIFT) {
        value += 1;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        value += 2;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        value += 4;
    }
    if value == 0 { 0 } else { value + 1 }
}

fn control_byte(character: char) -> Option<u8> {
    match character {
        ' ' | '@' => Some(0x00),
        'a'..='z' | 'A'..='Z' => Some(character.to_ascii_uppercase() as u8 - b'A' + 1),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        '?' => Some(0x7f),
        _ => None,
    }
}

fn modified_cursor_key(
    buffer: &mut [u8; 16],
    final_byte: u8,
    modifier: u8,
    application_mode: bool,
) -> Option<&[u8]> {
    if modifier > 0 {
        let len = write_csi_mod(buffer, final_byte, modifier);
        Some(&buffer[..len])
    } else {
        buffer[0] = 0x1b;
        buffer[1] = if application_mode { b'O' } else { b'[' };
        buffer[2] = final_byte;
        Some(&buffer[..3])
    }
}

fn modified_tilde_key(buffer: &mut [u8; 16], code: u8, modifier: u8) -> &[u8] {
    buffer[..2].copy_from_slice(b"\x1b[");
    let mut len = 2;
    if code >= 10 {
        buffer[len] = b'0' + code / 10;
        len += 1;
    }
    buffer[len] = b'0' + code % 10;
    len += 1;
    if modifier > 0 {
        buffer[len] = b';';
        buffer[len + 1] = b'0' + modifier;
        len += 2;
    }
    buffer[len] = b'~';
    &buffer[..len + 1]
}

fn function_key_sequence(buffer: &mut [u8; 16], number: u8, modifier: u8) -> Option<&[u8]> {
    let code = match number {
        1..=4 => return modified_cursor_key(buffer, b'P' + number - 1, modifier, true),
        5 => 15,
        6..=10 => number + 11,
        11..=12 => number + 12,
        _ => return None,
    };
    Some(modified_tilde_key(buffer, code, modifier))
}

#[cfg(all(test, windows))]
#[path = "input_terminal_windows_tests.rs"]
mod windows_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_letters_map_to_ascii_controls() {
        assert_eq!(control_byte('a'), Some(0x01));
        assert_eq!(control_byte('C'), Some(0x03));
        assert_eq!(control_byte('z'), Some(0x1a));
    }

    #[test]
    fn control_punctuation_maps_to_terminal_controls() {
        assert_eq!(control_byte(' '), Some(0x00));
        assert_eq!(control_byte('['), Some(0x1b));
        assert_eq!(control_byte('?'), Some(0x7f));
    }

    #[test]
    fn xterm_modifier_values_match_protocol() {
        assert_eq!(xterm_modifier_parameter(KeyModifiers::NONE), 0);
        assert_eq!(xterm_modifier_parameter(KeyModifiers::SHIFT), 2);
        assert_eq!(xterm_modifier_parameter(KeyModifiers::CONTROL), 5);
        assert_eq!(
            xterm_modifier_parameter(KeyModifiers::CONTROL | KeyModifiers::SHIFT),
            6
        );
    }

    #[test]
    fn fullscreen_shortcuts_belong_to_child_and_return_to_workbench_after_exit() {
        let mut app = App::test_new();
        app.show_terminal_sidebar = true;
        app.terminal_panel_focused = true;
        let mut pty =
            crate::terminal::TerminalEmulator::test_with_output(20, 3, "\x1b[?1049;9001h");
        let captured = pty.test_capture_input();
        app.terminals[0].terminal_pty = Some(pty);
        let palette = KeyEvent::new(
            KeyCode::Char('P'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        crate::input_keyboard::handle_keyboard_event(&mut app, palette);
        assert!(!app.palette.active);
        assert!(!captured.lock().unwrap().is_empty());

        app.terminals[0].terminal_pty = Some(crate::terminal::TerminalEmulator::test_with_output(
            20,
            3,
            "\x1b[?1049;9001h\x1b[?1049l",
        ));
        crate::input_keyboard::handle_keyboard_event(&mut app, palette);
        assert!(app.palette.active);
    }
    fn terminal_fixture() -> (App, std::sync::Arc<std::sync::Mutex<Vec<u8>>>) {
        let mut app = App::test_new();
        let mut pty = crate::terminal::TerminalEmulator::test_with_output(20, 3, "hello");
        let captured = pty.test_capture_input();
        app.terminals[0].terminal_pty = Some(pty);
        (app, captured)
    }

    #[test]
    fn ssh_terminal_shortcuts_preserve_modifiers() {
        let ctrl = KeyModifiers::CONTROL;
        let shift = KeyModifiers::SHIFT;
        let alt = KeyModifiers::ALT;
        // A remote fullscreen editor owns these keys; no local tab switching
        // or scrollback shortcut should steal or strip the modifiers.
        let mut app = App::test_new();
        let mut pty = crate::terminal::TerminalEmulator::test_with_output(20, 3, "\x1b[?1049h");
        let captured = pty.test_capture_input();
        app.terminals[0].terminal_pty = Some(pty);
        app.show_terminal_sidebar = true;
        app.terminal_panel_focused = true;
        for (code, mods, expected) in [
            (KeyCode::Backspace, KeyModifiers::NONE, "\x7f"),
            (KeyCode::Backspace, ctrl, "\x08"),
            (KeyCode::Backspace, ctrl | alt, "\x1b\x08"),
            (KeyCode::Left, ctrl | shift, "\x1b[1;6D"),
            (KeyCode::Delete, KeyModifiers::NONE, "\x1b[3~"),
            (KeyCode::Delete, ctrl, "\x1b[3;5~"),
            (KeyCode::Delete, ctrl | shift, "\x1b[3;6~"),
            (KeyCode::Insert, alt, "\x1b[2;3~"),
            (KeyCode::PageUp, ctrl, "\x1b[5;5~"),
            (KeyCode::PageDown, ctrl | shift, "\x1b[6;6~"),
            (KeyCode::F(1), KeyModifiers::NONE, "\x1bOP"),
            (KeyCode::F(3), shift, "\x1b[1;2R"),
            (KeyCode::F(5), KeyModifiers::NONE, "\x1b[15~"),
            (KeyCode::F(5), shift, "\x1b[15;2~"),
            (KeyCode::F(12), ctrl | alt | shift, "\x1b[24;8~"),
        ] {
            captured.lock().unwrap().clear();
            crate::input_keyboard::handle_keyboard_event(&mut app, KeyEvent::new(code, mods));
            assert_eq!(&*captured.lock().unwrap(), expected.as_bytes(), "{code:?}, {mods:?}");
        }
    }

    #[test]
    fn copy_shortcuts_copy_text_instead_of_sending_interrupts_to_the_shell() {
        let (mut app, captured) = terminal_fixture();
        let grid = &mut app.terminals[0].terminal_pty.as_mut().unwrap().grid;
        grid.begin_selection(0, 0);
        grid.extend_selection(0, 4);
        for key in [
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            KeyEvent::new(
                KeyCode::Char('C'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
            KeyEvent::new(KeyCode::Insert, KeyModifiers::CONTROL),
        ] {
            let mut copied = String::new();
            assert_eq!(
                app.handle_terminal_key_at_with_copy(0, key, |text| {
                    copied = text.to_owned();
                    true
                }),
                Some(false)
            );
            assert_eq!(copied, "hello");
        }
        assert!(captured.lock().unwrap().is_empty());
        assert!(
            app.terminals[0]
                .terminal_pty
                .as_ref()
                .unwrap()
                .grid
                .has_selection()
        );
    }

    #[test]
    fn ctrl_c_interrupts_without_selection_and_ctrl_shift_c_never_interrupts() {
        let (mut app, captured) = terminal_fixture();
        let no_clipboard = |_: &str| -> bool { panic!("no selected text to copy") };
        assert_eq!(
            app.handle_terminal_key_at_with_copy(
                0,
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                no_clipboard
            ),
            Some(false)
        );
        assert_eq!(&*captured.lock().unwrap(), b"\x03");
        captured.lock().unwrap().clear();
        assert_eq!(
            app.handle_terminal_key_at_with_copy(
                0,
                KeyEvent::new(
                    KeyCode::Char('C'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT
                ),
                no_clipboard
            ),
            Some(false)
        );
        assert!(captured.lock().unwrap().is_empty());
    }

    #[test]
    fn escape_clears_selection_and_altgr_remains_shell_input() {
        let (mut app, captured) = terminal_fixture();
        let no_clipboard = |_: &str| -> bool { panic!("not a copy shortcut") };
        let grid = &mut app.terminals[0].terminal_pty.as_mut().unwrap().grid;
        grid.begin_selection(0, 0);
        grid.extend_selection(0, 4);
        app.handle_terminal_key_at_with_copy(
            0,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            no_clipboard,
        );
        assert!(
            !app.terminals[0]
                .terminal_pty
                .as_ref()
                .unwrap()
                .grid
                .has_selection()
        );
        assert!(captured.lock().unwrap().is_empty());
        let grid = &mut app.terminals[0].terminal_pty.as_mut().unwrap().grid;
        grid.begin_selection(0, 0);
        grid.extend_selection(0, 4);
        app.handle_terminal_key_at_with_copy(
            0,
            KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            ),
            no_clipboard,
        );
        assert_eq!(&*captured.lock().unwrap(), b"c");
        assert!(
            !app.terminals[0]
                .terminal_pty
                .as_ref()
                .unwrap()
                .grid
                .has_selection()
        );
    }

    #[test]
    fn dock_keyboard_input_wins_over_an_open_editor_terminal() {
        let (mut app, dock_input) = terminal_fixture();
        app.terminals.push(crate::app::TerminalTab::new(
            "Editor terminal".into(),
            app.explorer.root.clone(),
        ));
        let mut editor_pty = crate::terminal::TerminalEmulator::test_with_output(20, 3, "other");
        let editor_input = editor_pty.test_capture_input();
        app.terminals[1].terminal_pty = Some(editor_pty);
        app.detach_terminal_to_group(1, app.active_group);
        app.active_terminal = 0;
        app.show_terminal_sidebar = true;
        app.terminal_panel_focused = true;
        crate::input_keyboard::handle_keyboard_event(
            &mut app,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        );
        assert_eq!(&*dock_input.lock().unwrap(), b"\x03");
        assert!(editor_input.lock().unwrap().is_empty());
    }
}
