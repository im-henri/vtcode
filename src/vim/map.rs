//! Key mappings (`:map`, `:noremap` and their mode variants).

use super::keys::{self, Key};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum MapMode {
    Normal,
    Visual,
    /// Select mode is not emulated; `:smap` entries are accepted and ignored.
    Select,
    OpPending,
    Insert,
    Cmdline,
}

impl MapMode {
    fn letter(self) -> char {
        match self {
            MapMode::Normal => 'n',
            MapMode::Visual => 'x',
            MapMode::Select => 's',
            MapMode::OpPending => 'o',
            MapMode::Insert => 'i',
            MapMode::Cmdline => 'c',
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Mapping {
    pub(crate) rhs: Vec<Key>,
    pub(crate) noremap: bool,
    pub(crate) nowait: bool,
    pub(crate) silent: bool,
    /// The right-hand side as written, for listings.
    pub(crate) source: String,
    /// `<expr>`: `source` is evaluated each time to get the keys.
    pub(crate) expr: bool,
}

#[derive(Default, Clone, Debug)]
pub(crate) struct Maps {
    maps: BTreeMap<(MapMode, Vec<Key>), Mapping>,
}

pub(crate) enum Lookup {
    None,
    /// `seq` is a strict prefix of a mapping: wait for more keys.
    Prefix,
    Match {
        len: usize,
        rhs: Vec<Key>,
        noremap: bool,
        expr: Option<String>,
    },
}

/// The modes a `:map` command name applies to.
pub(crate) fn modes_for(cmd: &str) -> Option<(Vec<MapMode>, bool)> {
    use MapMode::*;
    let (base, noremap) = if let Some(b) = cmd.strip_suffix("noremap") {
        (b, true)
    } else if let Some(b) = cmd.strip_suffix("nore") {
        (b, true)
    } else if let Some(b) = cmd.strip_suffix("map") {
        (b, false)
    } else {
        return None;
    };
    let modes = match base {
        "" => vec![Normal, Visual, Select, OpPending],
        "n" => vec![Normal],
        "v" => vec![Visual, Select],
        "x" => vec![Visual],
        "s" => vec![Select],
        "o" => vec![OpPending],
        "i" => vec![Insert],
        "c" => vec![Cmdline],
        "l" => vec![Insert, Cmdline],
        "!" => vec![Insert, Cmdline],
        _ => return None,
    };
    Some((modes, noremap))
}

impl Maps {
    pub(crate) fn define(&mut self, modes: &[MapMode], lhs: Vec<Key>, m: Mapping) {
        for &mode in modes {
            self.maps.insert((mode, lhs.clone()), m.clone());
        }
    }

    pub(crate) fn remove(&mut self, modes: &[MapMode], lhs: &[Key]) -> bool {
        let mut any = false;
        for &mode in modes {
            any |= self.maps.remove(&(mode, lhs.to_vec())).is_some();
        }
        any
    }

    pub(crate) fn clear(&mut self, modes: &[MapMode]) {
        self.maps.retain(|(mode, _), _| !modes.contains(mode));
    }

    pub(crate) fn lookup(&self, mode: MapMode, seq: &[Key]) -> Lookup {
        if seq.is_empty() {
            return Lookup::None;
        }
        let mut exact: Option<(usize, &Mapping)> = None;
        let mut longer = false;
        // Candidates share the first key.
        let lo = (mode, vec![seq[0]]);
        for ((m, lhs), mapping) in self.maps.range(lo..) {
            if *m != mode || lhs.first() != Some(&seq[0]) {
                break;
            }
            if lhs.len() <= seq.len() {
                if seq.starts_with(lhs) && exact.is_none_or(|(l, _)| lhs.len() > l) {
                    exact = Some((lhs.len(), mapping));
                }
            } else if lhs.starts_with(seq) {
                longer = true;
            }
        }
        // A longer mapping may still match: wait, unless the complete one
        // is `<nowait>`.
        match exact {
            Some((len, m)) if !longer || m.nowait => Lookup::Match {
                len,
                rhs: m.rhs.clone(),
                noremap: m.noremap,
                expr: m.expr.then(|| m.source.clone()),
            },
            _ if longer => Lookup::Prefix,
            _ => Lookup::None,
        }
    }

    /// The longest mapping that matches a prefix of `seq` (after a timeout).
    pub(crate) fn longest(
        &self,
        mode: MapMode,
        seq: &[Key],
    ) -> Option<(usize, Vec<Key>, bool, Option<String>)> {
        (1..=seq.len()).rev().find_map(|n| {
            self.maps.get(&(mode, seq[..n].to_vec())).map(|m| {
                (
                    n,
                    m.rhs.clone(),
                    m.noremap,
                    m.expr.then(|| m.source.clone()),
                )
            })
        })
    }

    /// `:map` listing lines.
    pub(crate) fn list(&self, modes: &[MapMode], prefix: &[Key]) -> Vec<String> {
        let mut rows: BTreeMap<(Vec<Key>, String), Vec<char>> = BTreeMap::new();
        for ((mode, lhs), m) in &self.maps {
            if !modes.contains(mode) || !lhs.starts_with(prefix) {
                continue;
            }
            let rhs = format!("{}{}", if m.noremap { "*" } else { " " }, m.source);
            rows.entry((lhs.clone(), rhs))
                .or_default()
                .push(mode.letter());
        }
        rows.into_iter()
            .map(|((lhs, rhs), letters)| {
                let mode = if letters.len() >= 4 {
                    " ".to_string()
                } else {
                    letters.iter().collect()
                };
                format!("{mode:<3}{:<12} {rhs}", keys::format(&lhs))
            })
            .collect()
    }
}

/// Parse `:map` arguments: `[<silent>] [<buffer>] {lhs} {rhs}`.
pub(crate) fn parse_args(args: &str) -> (bool, bool, bool, &str, &str) {
    let mut rest = args.trim_start();
    let mut silent = false;
    let mut nowait = false;
    let mut expr = false;
    loop {
        let lower = rest.get(..10).unwrap_or(rest).to_ascii_lowercase();
        let special = [
            "<silent>",
            "<buffer>",
            "<nowait>",
            "<unique>",
            "<special>",
            "<script>",
            "<expr>",
        ]
        .into_iter()
        .find(|s| lower.starts_with(s));
        match special {
            Some(s) => {
                if s == "<silent>" {
                    silent = true;
                }
                if s == "<nowait>" {
                    nowait = true;
                }
                if s == "<expr>" {
                    expr = true;
                }
                rest = rest[s.len()..].trim_start();
            }
            None => break,
        }
    }
    // The lhs ends at the first unescaped blank; `\ ` or `<Space>` keep it.
    let mut end = rest.len();
    let mut prev_bs = false;
    for (i, c) in rest.char_indices() {
        if (c == ' ' || c == '\t') && !prev_bs {
            end = i;
            break;
        }
        prev_bs = c == '\\' && !prev_bs;
    }
    let lhs = &rest[..end];
    let rhs = rest[end..].trim_start();
    (silent, nowait, expr, lhs, rhs)
}

/// Convert map text to keys: `\|` is `|`, a trailing `<Bar>` is `|`.
pub(crate) fn map_keys(text: &str, leader: &[Key]) -> Vec<Key> {
    let text = text.replace("\\|", "<Bar>").replace("\\ ", "<Space>");
    keys::parse(&text, leader)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(rhs: &str, noremap: bool) -> Mapping {
        Mapping {
            rhs: keys::parse(rhs, &[]),
            noremap,
            nowait: false,
            silent: false,
            source: rhs.into(),
            expr: false,
        }
    }

    #[test]
    fn lookup_waits_for_longer_mappings_and_matches_exactly() {
        let mut maps = Maps::default();
        let lhs = |s| keys::parse(s, &[]);
        maps.define(&[MapMode::Insert], lhs("jk"), m("<Esc>", true));
        maps.define(&[MapMode::Normal], lhs("<Space>f"), m(":find ", true));
        maps.define(&[MapMode::Normal], lhs("<Space>"), m("l", false));
        assert!(matches!(
            maps.lookup(MapMode::Insert, &lhs("j")),
            Lookup::Prefix
        ));
        assert!(matches!(
            maps.lookup(MapMode::Insert, &lhs("jk")),
            Lookup::Match { len: 2, .. }
        ));
        assert!(matches!(
            maps.lookup(MapMode::Insert, &lhs("x")),
            Lookup::None
        ));
        assert!(matches!(
            maps.lookup(MapMode::Normal, &lhs("<Space>")),
            Lookup::Prefix
        ));
        assert!(matches!(
            maps.lookup(MapMode::Normal, &lhs("<Space>x")),
            Lookup::Match { len: 1, .. }
        ));
        assert_eq!(maps.longest(MapMode::Normal, &lhs("<Space>")).unwrap().0, 1);
        assert!(matches!(
            maps.lookup(MapMode::Visual, &lhs("jk")),
            Lookup::None
        ));
    }

    #[test]
    fn map_command_names_and_arguments() {
        assert_eq!(modes_for("nnoremap"), Some((vec![MapMode::Normal], true)));
        assert_eq!(modes_for("xmap"), Some((vec![MapMode::Visual], false)));
        assert_eq!(modes_for("inoremap"), Some((vec![MapMode::Insert], true)));
        assert!(modes_for("nmapx").is_none());
        let (silent, _, _, lhs, rhs) = parse_args("<silent> <leader>w :w<CR>");
        assert!(silent);
        assert_eq!((lhs, rhs), ("<leader>w", ":w<CR>"));
    }
}
