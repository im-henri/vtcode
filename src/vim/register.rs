//! Registers: unnamed, named (a-z, A-Z appends), numbered 0-9, small
//! delete, black hole, read-only (`.` `:` `/` `%`) and clipboard (`*` `+`).

use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum RegKind {
    #[default]
    Char,
    Line,
    /// Blockwise with the block's display width.
    Block(usize),
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct Reg {
    pub(crate) text: String,
    pub(crate) kind: RegKind,
}

impl Reg {
    pub(crate) fn new(text: impl Into<String>, kind: RegKind) -> Self {
        Self {
            text: text.into(),
            kind,
        }
    }
    /// Lines of a block register.
    pub(crate) fn block_lines(&self) -> Vec<&str> {
        self.text.split('\n').collect()
    }
}

#[derive(Default)]
pub(crate) struct Registers {
    named: HashMap<char, Reg>,
    numbered: [Reg; 10],
    small_delete: Reg,
    /// Which register `""` points at.
    unnamed_target: Option<char>,
    unnamed: Reg,
    pub(crate) last_inserted: String,
    pub(crate) last_cmdline: String,
    pub(crate) last_search: String,
    pub(crate) expr: String,
}

pub(crate) enum Store {
    Yank,
    /// Delete or change. `small` is true for deletes within one line that
    /// did not use one of the "big" motions (`%`, `(`, `)`, `` ` ``, `/`,
    /// `?`, `n`, `N`, `{`, `}`).
    Delete {
        small: bool,
    },
}

/// Clipboard access needed for `*` and `+`.
pub(crate) trait Clipboard {
    fn get(&mut self) -> Option<String>;
    fn set(&mut self, text: &str, linewise: bool);
}

impl Registers {
    pub(crate) fn valid(r: char) -> bool {
        r.is_ascii_alphanumeric() || "\"-_*+.:%#/=".contains(r)
    }

    /// Read a register. `file` provides `%`.
    pub(crate) fn get(&self, r: char, clip: &mut dyn Clipboard, file: Option<&str>) -> Option<Reg> {
        let r = r.to_ascii_lowercase();
        Some(match r {
            '"' => {
                if let Some(t) = self.unnamed_target {
                    if t == '*' || t == '+' {
                        return self.get(t, clip, file);
                    }
                }
                self.unnamed.clone()
            }
            '0'..='9' => self.numbered[r as usize - '0' as usize].clone(),
            'a'..='z' => self.named.get(&r)?.clone(),
            '-' => self.small_delete.clone(),
            '.' => Reg::new(self.last_inserted.clone(), RegKind::Char),
            ':' => Reg::new(self.last_cmdline.clone(), RegKind::Char),
            '/' => Reg::new(self.last_search.clone(), RegKind::Char),
            '=' => Reg::new(self.expr.clone(), RegKind::Char),
            '%' => Reg::new(file?.to_string(), RegKind::Char),
            '#' => return None,
            '*' | '+' => {
                let text = clip.get()?;
                let line = crate::clipboard::line_copy_status(&text).unwrap_or(false)
                    || (text.ends_with('\n')
                        && self.unnamed.kind == RegKind::Line
                        && self.unnamed.text == text);
                let kind = if line { RegKind::Line } else { RegKind::Char };
                Reg::new(text.replace("\r\n", "\n"), kind)
            }
            '_' => Reg::default(),
            _ => return None,
        })
    }

    /// Store text from a yank or delete. `clip_default` is set when
    /// `clipboard=unnamed[plus]` sends unnamed writes to the clipboard.
    pub(crate) fn store(
        &mut self,
        reg: Option<char>,
        value: Reg,
        how: Store,
        clip: &mut dyn Clipboard,
        clip_default: bool,
    ) {
        let target = reg.unwrap_or(if clip_default { '+' } else { '"' });
        if target == '_' {
            return;
        }
        let lower = target.to_ascii_lowercase();
        let mut value = value;
        match target {
            'A'..='Z' => {
                let entry = self.named.entry(lower).or_default();
                if entry.kind == RegKind::Line && value.kind != RegKind::Line {
                    entry.text.push_str(&value.text);
                } else if value.kind == RegKind::Line
                    && entry.kind != RegKind::Line
                    && !entry.text.is_empty()
                {
                    entry.text.push('\n');
                    entry.text.push_str(&value.text);
                    entry.kind = RegKind::Line;
                } else {
                    entry.text.push_str(&value.text);
                    if value.kind == RegKind::Line {
                        entry.kind = RegKind::Line;
                    }
                }
                value = entry.clone();
            }
            'a'..='z' => {
                self.named.insert(lower, value.clone());
            }
            '*' | '+' => clip.set(&value.text, value.kind == RegKind::Line),
            '"' | '-' | '0'..='9' => {}
            _ => {}
        }
        // The unnamed register always points at the last written one.
        self.unnamed = value.clone();
        self.unnamed_target = Some(lower);
        match how {
            Store::Yank => {
                // Only unnamed yanks (also through clipboard=unnamed) fill "0.
                if reg.is_none() || reg == Some('"') {
                    self.numbered[0] = value;
                }
            }
            Store::Delete { small } => {
                let named = matches!(target, 'a'..='z' | 'A'..='Z');
                if small && !named && value.kind != RegKind::Line && !value.text.contains('\n') {
                    self.small_delete = value;
                } else {
                    // Numbered registers shift on every multi-line or
                    // non-small delete, even into a named register.
                    for i in (2..10).rev() {
                        self.numbered[i] = std::mem::take(&mut self.numbered[i - 1]);
                    }
                    self.numbered[1] = value;
                }
            }
        }
    }

    /// Set a register directly (`:let @a = ...`, macro recording).
    pub(crate) fn set(&mut self, r: char, value: Reg, clip: &mut dyn Clipboard) {
        match r {
            'a'..='z' => {
                self.named.insert(r, value);
            }
            'A'..='Z' => {
                let entry = self.named.entry(r.to_ascii_lowercase()).or_default();
                entry.text.push_str(&value.text);
            }
            '"' => self.unnamed = value,
            '*' | '+' => clip.set(&value.text, value.kind == RegKind::Line),
            '0'..='9' => self.numbered[r as usize - '0' as usize] = value,
            '-' => self.small_delete = value,
            '/' => self.last_search = value.text,
            _ => {}
        }
    }

    /// `:registers` listing: (name, register).
    pub(crate) fn list(&self, clip: &mut dyn Clipboard, file: Option<&str>) -> Vec<(char, Reg)> {
        let mut out = Vec::new();
        for r in "\"0123456789abcdefghijklmnopqrstuvwxyz-.:%/+".chars() {
            if let Some(reg) = self.get(r, clip, file)
                && !reg.text.is_empty()
            {
                out.push((r, reg));
            }
        }
        out
    }
}

#[cfg(test)]
pub(crate) struct NoClip(pub(crate) Option<String>);

#[cfg(test)]
impl Clipboard for NoClip {
    fn get(&mut self) -> Option<String> {
        self.0.clone()
    }
    fn set(&mut self, text: &str, _: bool) {
        self.0 = Some(text.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deletes_shift_numbered_and_small_deletes_use_minus() {
        let mut r = Registers::default();
        let mut c = NoClip(None);
        r.store(
            None,
            Reg::new("one\n", RegKind::Line),
            Store::Delete { small: false },
            &mut c,
            false,
        );
        r.store(
            None,
            Reg::new("two\n", RegKind::Line),
            Store::Delete { small: false },
            &mut c,
            false,
        );
        r.store(
            None,
            Reg::new("w", RegKind::Char),
            Store::Delete { small: true },
            &mut c,
            false,
        );
        assert_eq!(r.get('1', &mut c, None).unwrap().text, "two\n");
        assert_eq!(r.get('2', &mut c, None).unwrap().text, "one\n");
        assert_eq!(r.get('-', &mut c, None).unwrap().text, "w");
        assert_eq!(r.get('"', &mut c, None).unwrap().text, "w");
        r.store(
            None,
            Reg::new("y", RegKind::Char),
            Store::Yank,
            &mut c,
            false,
        );
        assert_eq!(r.get('0', &mut c, None).unwrap().text, "y");
        r.store(
            Some('a'),
            Reg::new("a", RegKind::Char),
            Store::Yank,
            &mut c,
            false,
        );
        r.store(
            Some('A'),
            Reg::new("b", RegKind::Char),
            Store::Yank,
            &mut c,
            false,
        );
        assert_eq!(r.get('a', &mut c, None).unwrap().text, "ab");
        assert_eq!(
            r.get('0', &mut c, None).unwrap().text,
            "y",
            "named yanks leave \"0 alone"
        );
        r.store(
            Some('_'),
            Reg::new("gone", RegKind::Char),
            Store::Delete { small: true },
            &mut c,
            false,
        );
        assert_eq!(r.get('"', &mut c, None).unwrap().text, "ab");
        r.store(
            None,
            Reg::new("clip", RegKind::Char),
            Store::Yank,
            &mut c,
            true,
        );
        assert_eq!(c.0.as_deref(), Some("clip"));
    }
}
