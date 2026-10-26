use super::TermGrid;
use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

impl TermGrid {
    /// Encode a mouse event whose coordinates are relative to this terminal.
    pub(crate) fn mouse_sequence(&self, event: MouseEvent) -> Option<Vec<u8>> {
        if self.mouse_tracking == 0 {
            return None;
        }
        let button_code = |button| match button {
            MouseButton::Left => 0u8,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            MouseButton::Back => 128,
            MouseButton::Forward => 129,
        };
        let (mut code, release) = match event.kind {
            MouseEventKind::Down(button) => (button_code(button), false),
            MouseEventKind::Up(button) => (
                if self.sgr_mouse {
                    button_code(button)
                } else {
                    3
                },
                true,
            ),
            MouseEventKind::Drag(button) if self.mouse_tracking >= 1002 => {
                (32 + button_code(button), false)
            }
            MouseEventKind::Moved if self.mouse_tracking == 1003 => (35, false),
            MouseEventKind::ScrollUp => (64, false),
            MouseEventKind::ScrollDown => (65, false),
            MouseEventKind::ScrollLeft => (66, false),
            MouseEventKind::ScrollRight => (67, false),
            _ => return None,
        };
        if event.modifiers.contains(KeyModifiers::SHIFT) {
            code |= 4;
        }
        if event.modifiers.contains(KeyModifiers::ALT) {
            code |= 8;
        }
        if event.modifiers.contains(KeyModifiers::CONTROL) {
            code |= 16;
        }
        let x = event.column as usize + 1;
        let y = event.row as usize + 1;
        if self.sgr_mouse {
            Some(format!("\x1b[<{code};{x};{y}{}", if release { 'm' } else { 'M' }).into_bytes())
        } else if x <= 223 && y <= 223 {
            Some(vec![
                0x1b,
                b'[',
                b'M',
                code + 32,
                x as u8 + 32,
                y as u8 + 32,
            ])
        } else {
            None
        }
    }
}
