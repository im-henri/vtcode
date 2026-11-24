//! Vim key values and `<C-x>`-style key notation.
//!
//! Keys are normalized so that a shifted letter is the uppercase character
//! without a Shift modifier, exactly as Vim sees it. Mappings, macros, the
//! dot-repeat log and `:normal` all use this one representation.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum K {
    Char(char),
    Esc,
    Enter,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    F(u8),
    /// The mapleader placeholder; resolved when a mapping is defined.
    Nop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Key {
    pub(crate) k: K,
    pub(crate) ctrl: bool,
    pub(crate) alt: bool,
    /// Shift is only kept for keys without a shifted character (`<S-Tab>`).
    pub(crate) shift: bool,
}

impl Key {
    pub(crate) const fn ch(c: char) -> Self {
        Self {
            k: K::Char(c),
            ctrl: false,
            alt: false,
            shift: false,
        }
    }
    pub(crate) const fn plain(k: K) -> Self {
        Self {
            k,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }
    pub(crate) const fn ctrl(c: char) -> Self {
        Self {
            k: K::Char(c),
            ctrl: true,
            alt: false,
            shift: false,
        }
    }
    pub(crate) const ESC: Self = Self::plain(K::Esc);
    pub(crate) const ENTER: Self = Self::plain(K::Enter);

    /// A printable character typed without Ctrl/Alt.
    pub(crate) fn char(self) -> Option<char> {
        match self.k {
            K::Char(c) if !self.ctrl && !self.alt => Some(c),
            _ => None,
        }
    }

    /// `<C-c>` exactly. Ctrl+Shift+letter is never a Vim key: Vim cannot
    /// tell it from `<C-c>`, and the workbench binds those strokes (Ctrl+Shift+P
    /// is the Command Palette), so it must not move a list like `<C-p>`.
    pub(crate) fn is_ctrl(self, c: char) -> bool {
        self.ctrl && !self.alt && !self.shift && self.k == K::Char(c)
    }

    /// Vim treats `<C-[>` as `<Esc>` and `<C-c>` as an interrupt that also
    /// leaves Insert mode.
    pub(crate) fn is_escape(self) -> bool {
        self.k == K::Esc && !self.ctrl && !self.alt || self.is_ctrl('[')
    }

    pub(crate) fn from_event(event: KeyEvent) -> Option<Self> {
        let mods = event.modifiers;
        let mut ctrl = mods.contains(KeyModifiers::CONTROL);
        let mut alt = mods.contains(KeyModifiers::ALT);
        let shift = mods.contains(KeyModifiers::SHIFT);
        // Windows reports AltGr as Ctrl+Alt with the composed character.
        if ctrl && alt {
            if let KeyCode::Char(c) = event.code
                && !c.is_ascii_alphabetic()
            {
                ctrl = false;
                alt = false;
            }
        }
        // Terminals send Ctrl-\ and Ctrl-] as the bytes for Ctrl+4 / Ctrl+5.
        let event = match event.code {
            KeyCode::Char('4') if ctrl && !alt => KeyEvent::new(KeyCode::Char('\\'), mods),
            KeyCode::Char('5') if ctrl && !alt => KeyEvent::new(KeyCode::Char(']'), mods),
            // A raw control character (0x1c for Ctrl-\) is Ctrl plus its key.
            KeyCode::Char(c) if (c as u32) < 0x20 && !matches!(c, '\t' | '\r' | '\n' | '\x1b') => {
                let base = if (c as u32) <= 0x1a {
                    (c as u8 | 0x60) as char
                } else {
                    (c as u8 | 0x40) as char
                };
                ctrl = true;
                KeyEvent::new(KeyCode::Char(base), mods | KeyModifiers::CONTROL)
            }
            _ => event,
        };
        let k = match event.code {
            KeyCode::Char(c) => {
                // Vim cannot tell <C-a> from <C-A>; keep Shift so that
                // workbench shortcuts like Ctrl+Shift+P stay distinct.
                let ctrl_shift =
                    ctrl && c.is_ascii_alphabetic() && (shift || c.is_ascii_uppercase());
                let c = if ctrl && c.is_ascii_alphabetic() {
                    c.to_ascii_lowercase()
                } else if shift && c.is_ascii_lowercase() {
                    c.to_ascii_uppercase()
                } else {
                    c
                };
                return Some(Self {
                    k: K::Char(c),
                    ctrl,
                    alt,
                    shift: ctrl_shift,
                });
            }
            KeyCode::Esc => K::Esc,
            KeyCode::Enter => K::Enter,
            KeyCode::Tab if shift => K::BackTab,
            KeyCode::Tab => K::Tab,
            KeyCode::BackTab => K::BackTab,
            KeyCode::Backspace => K::Backspace,
            KeyCode::Delete => K::Delete,
            KeyCode::Insert => K::Insert,
            KeyCode::Up => K::Up,
            KeyCode::Down => K::Down,
            KeyCode::Left => K::Left,
            KeyCode::Right => K::Right,
            KeyCode::Home => K::Home,
            KeyCode::End => K::End,
            KeyCode::PageUp => K::PageUp,
            KeyCode::PageDown => K::PageDown,
            KeyCode::F(n) => K::F(n),
            _ => return None,
        };
        let shift = shift && k != K::BackTab;
        Some(Self {
            k,
            ctrl,
            alt,
            shift,
        })
    }

    /// The crossterm event that the workbench would have seen for this key.
    pub(crate) fn to_event(self) -> KeyEvent {
        let mut mods = KeyModifiers::empty();
        if self.ctrl {
            mods |= KeyModifiers::CONTROL;
        }
        if self.alt {
            mods |= KeyModifiers::ALT;
        }
        if self.shift {
            mods |= KeyModifiers::SHIFT;
        }
        let code = match self.k {
            K::Char(c) => {
                if c.is_ascii_uppercase() {
                    mods |= KeyModifiers::SHIFT;
                }
                KeyCode::Char(c)
            }
            K::Esc => KeyCode::Esc,
            K::Enter => KeyCode::Enter,
            K::Tab => KeyCode::Tab,
            K::BackTab => {
                mods |= KeyModifiers::SHIFT;
                KeyCode::BackTab
            }
            K::Backspace => KeyCode::Backspace,
            K::Delete => KeyCode::Delete,
            K::Insert => KeyCode::Insert,
            K::Up => KeyCode::Up,
            K::Down => KeyCode::Down,
            K::Left => KeyCode::Left,
            K::Right => KeyCode::Right,
            K::Home => KeyCode::Home,
            K::End => KeyCode::End,
            K::PageUp => KeyCode::PageUp,
            K::PageDown => KeyCode::PageDown,
            K::F(n) => KeyCode::F(n),
            K::Nop => KeyCode::Null,
        };
        KeyEvent::new(code, mods)
    }
}

fn named(name: &str) -> Option<K> {
    Some(match name.to_ascii_lowercase().as_str() {
        "esc" => K::Esc,
        "cr" | "enter" | "return" => K::Enter,
        "tab" => K::Tab,
        "bs" | "backspace" => K::Backspace,
        "del" | "delete" => K::Delete,
        "insert" | "ins" => K::Insert,
        "up" => K::Up,
        "down" => K::Down,
        "left" => K::Left,
        "right" => K::Right,
        "home" => K::Home,
        "end" => K::End,
        "pageup" => K::PageUp,
        "pagedown" => K::PageDown,
        "space" => K::Char(' '),
        "lt" => K::Char('<'),
        "bar" => K::Char('|'),
        "bslash" => K::Char('\\'),
        "nop" => K::Nop,
        n if n.len() > 1 && n.starts_with('f') => K::F(n[1..].parse().ok()?),
        _ => return None,
    })
}

/// Parse Vim key notation such as `ciw<C-r>"<Esc>` or `<leader>ff`.
/// `<leader>` and `<localleader>` expand to `leader`. Unknown `<...>` groups
/// are taken literally, as Vim does.
pub(crate) fn parse(text: &str, leader: &[Key]) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some(close) = rest.find('>')
            && close > 1
        {
            let inner = &rest[1..close];
            if let Some(parsed) = parse_group(inner, leader) {
                keys.extend(parsed);
                rest = &rest[close + 1..];
                continue;
            }
        }
        keys.push(Key::ch(c));
        rest = &rest[c.len_utf8()..];
    }
    keys
}

fn parse_group(inner: &str, leader: &[Key]) -> Option<Vec<Key>> {
    let lower = inner.to_ascii_lowercase();
    if lower == "leader" || lower == "localleader" {
        return Some(leader.to_vec());
    }
    let mut key = Key::plain(K::Nop);
    let mut name = inner;
    loop {
        let bytes = name.as_bytes();
        if bytes.len() > 2 && bytes[1] == b'-' {
            match bytes[0].to_ascii_lowercase() {
                b'c' => key.ctrl = true,
                b'a' | b'm' => key.alt = true,
                b's' => key.shift = true,
                b'd' => key.ctrl = true, // <D-x>: treat Command as Control
                _ => return None,
            }
            name = &name[2..];
        } else {
            break;
        }
    }
    let mut chars = name.chars();
    key.k = match (chars.next(), chars.next()) {
        (Some(c), None) => K::Char(c),
        _ => named(name)?,
    };
    if let K::Char(c) = key.k {
        if key.ctrl && c.is_ascii_alphabetic() {
            key.k = K::Char(c.to_ascii_lowercase());
        } else if key.shift && c.is_alphabetic() {
            key.k = K::Char(c.to_uppercase().next().unwrap_or(c));
            key.shift = false;
        }
        if key.ctrl && c == '[' && !key.alt {
            // <C-[> is <Esc>.
        }
    }
    if key.k == K::Tab && key.shift {
        key.k = K::BackTab;
        key.shift = false;
    }
    if key.k == K::Nop && (key.ctrl || key.alt || key.shift) {
        return None;
    }
    if key.k == K::Nop {
        return Some(Vec::new());
    }
    Some(vec![key])
}

/// Render keys in Vim notation, e.g. for `:map` listings and `showcmd`.
pub(crate) fn format(keys: &[Key]) -> String {
    let mut out = String::new();
    for key in keys {
        out.push_str(&format_key(*key));
    }
    out
}

pub(crate) fn format_key(key: Key) -> String {
    let base = match key.k {
        K::Char(' ') if key.ctrl || key.alt => "Space".to_string(),
        K::Char('<') if !key.ctrl && !key.alt => return "<lt>".into(),
        K::Char(c) => {
            if !key.ctrl && !key.alt && !key.shift {
                return c.to_string();
            }
            c.to_string()
        }
        K::Esc => "Esc".into(),
        K::Enter => "CR".into(),
        K::Tab => "Tab".into(),
        K::BackTab => "S-Tab".into(),
        K::Backspace => "BS".into(),
        K::Delete => "Del".into(),
        K::Insert => "Insert".into(),
        K::Up => "Up".into(),
        K::Down => "Down".into(),
        K::Left => "Left".into(),
        K::Right => "Right".into(),
        K::Home => "Home".into(),
        K::End => "End".into(),
        K::PageUp => "PageUp".into(),
        K::PageDown => "PageDown".into(),
        K::F(n) => format!("F{n}"),
        K::Nop => "Nop".into(),
    };
    let mut out = String::from("<");
    if key.ctrl {
        out.push_str("C-");
    }
    if key.alt {
        out.push_str("M-");
    }
    if key.shift {
        out.push_str("S-");
    }
    out.push_str(&base);
    out.push('>');
    out
}

/// `showcmd` rendering: control characters display as `^X`.
pub(crate) fn showcmd(keys: &[Key]) -> String {
    let mut out = String::new();
    for key in keys {
        match key.k {
            K::Char(c) if key.ctrl && !key.alt => {
                out.push('^');
                out.push(c.to_ascii_uppercase());
            }
            K::Char(c) if !key.ctrl && !key.alt => out.push(c),
            _ => out.push_str(&format_key(*key)),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notation_round_trips_common_mappings() {
        let leader = [Key::ch(' ')];
        let keys = parse("<leader>ff<C-w>j<Esc>:w<CR><lt>", &leader);
        assert_eq!(
            keys,
            vec![
                Key::ch(' '),
                Key::ch('f'),
                Key::ch('f'),
                Key::ctrl('w'),
                Key::ch('j'),
                Key::ESC,
                Key::ch(':'),
                Key::ch('w'),
                Key::ENTER,
                Key::ch('<'),
            ]
        );
        assert_eq!(format(&keys[3..]), "<C-w>j<Esc>:w<CR><lt>");
        assert_eq!(
            parse("<C-S-x>", &leader),
            vec![Key {
                k: K::Char('x'),
                ctrl: true,
                alt: false,
                shift: true
            }]
        );
        assert_eq!(parse("<S-a>", &leader), vec![Key::ch('A')]);
        assert_eq!(parse("<S-Tab>", &leader), vec![Key::plain(K::BackTab)]);
        assert_eq!(parse("<Nop>", &leader), Vec::<Key>::new());
        assert_eq!(parse("<notakey>", &leader).len(), "<notakey>".len());
        assert_eq!(parse("<Space>", &leader), vec![Key::ch(' ')]);
    }

    #[test]
    fn events_normalize_shift_and_ctrl_like_vim() {
        let shifted =
            Key::from_event(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::SHIFT)).unwrap();
        assert_eq!(shifted, Key::ch('A'));
        let ctrl =
            Key::from_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)).unwrap();
        assert_eq!(ctrl, Key::ctrl('r'));
        // Ctrl+Shift+P must not turn into Ctrl+P.
        let palette = Key::from_event(KeyEvent::new(
            KeyCode::Char('P'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ))
        .unwrap();
        assert_ne!(palette, Key::ctrl('p'));
        assert_eq!(
            palette.to_event(),
            KeyEvent::new(
                KeyCode::Char('p'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            )
        );
        let altgr = Key::from_event(KeyEvent::new(
            KeyCode::Char('{'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        ))
        .unwrap();
        assert_eq!(altgr, Key::ch('{'));
        let backtab =
            Key::from_event(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)).unwrap();
        assert_eq!(backtab, Key::plain(K::BackTab));
        assert!(Key::ctrl('[').is_escape());
        assert_eq!(
            showcmd(&[Key::ch('"'), Key::ch('a'), Key::ctrl('w')]),
            "\"a^W"
        );
    }
}
