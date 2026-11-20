//! Behaviour checked against real Vim 9: `scripts/vim_parity.py` records the
//! buffer, cursor and unnamed register after each case of a generated
//! corpus; this test replays the corpus through the engine.

use std::io::Read;

use super::keys;
use super::register::RegKind;
use super::tests::{parity_vim, vim_lines, TestHost};
use super::Mode;

#[derive(serde::Deserialize)]
struct Expect {
    lines: Vec<String>,
    cursor: [usize; 2],
    reg: String,
    regtype: String,
}

#[derive(serde::Deserialize)]
struct Case {
    name: String,
    lines: Vec<String>,
    eol: bool,
    cursor: [usize; 2],
    keys: String,
    expect: Expect,
}

fn corpus() -> Vec<Case> {
    let gz = include_bytes!("parity_expected.json.gz");
    let mut json = String::new();
    flate2::read::GzDecoder::new(&gz[..]).read_to_string(&mut json).unwrap();
    serde_json::from_str(&json).unwrap()
}

pub(crate) struct Actual {
    pub(crate) lines: Vec<String>,
    pub(crate) cursor: [usize; 2],
    pub(crate) reg: String,
    pub(crate) regtype: String,
}

/// Run keys the way `:normal` does: as typeahead that an error flushes, with
/// an unfinished command or Insert mode ended at the end.
pub(crate) fn replay(lines: &[String], eol: bool, cursor: [usize; 2], keys_text: &str) -> Actual {
    replay_with(lines, eol, cursor, keys_text, "\n").0
}

/// `replay` on text whose lines end in `newline`; also returns the buffer.
fn replay_with(lines: &[String], eol: bool, cursor: [usize; 2], keys_text: &str, newline: &str) -> (Actual, String) {
    let mut text = lines.join(newline);
    if eol { text.push_str(newline); }
    let mut host = TestHost::new(&text);
    host.ed.detect_line_ending();
    host.ed.cursor_line = cursor[0];
    host.ed.cursor_col = cursor[1];
    let mut vim = parity_vim();
    // Keys are typed: undo steps and error recovery behave interactively.
    for key in keys::parse(keys_text, &[]) {
        vim.feed(&mut host, key);
    }
    vim.pending.clear();
    if vim.is_insert() { super::insert::finish(&mut vim, &mut host, true); }
    if vim.mode == Mode::Cmdline { super::cmdline::cancel(&mut vim, &mut host); }
    // Vim stays in Visual mode after feedkeys(); report its cursor.
    let visual_cursor = vim.is_visual().then(|| [host.ed.cursor_line, host.ed.cursor_col]);
    if vim.is_visual() { super::exec::exit_visual(&mut vim, &mut host); }
    let mut clip = super::register::NoClip(None);
    let reg = vim.regs.get('"', &mut clip, None).unwrap_or_default();
    let regtype = match reg.kind {
        RegKind::Char => "v".to_string(),
        RegKind::Line => "V".to_string(),
        RegKind::Block(w) => format!("\u{16}{w}"),
    };
    let actual = Actual {
        lines: vim_lines(&host.ed),
        cursor: visual_cursor.unwrap_or([host.ed.cursor_line, host.ed.cursor_col]),
        reg: reg.text,
        regtype,
    };
    (actual, host.ed.rope.to_string())
}

/// The same cases on a CRLF buffer, as files from Windows usually are. Vim
/// keeps a "dos" file's lines without the "\r", so the results must match
/// with "\r\n" endings. A bare "\n" is allowed (saving writes "\r\n"), but
/// a "\r" anywhere else would reach the file as a stray line break.
#[test]
fn engine_matches_real_vim_on_crlf_text() {
    let cases = corpus();
    let filter = std::env::var("VIM_PARITY_FILTER").unwrap_or_default();
    let mut failures = Vec::new();
    let mut ran = 0;
    for case in &cases {
        if !filter.is_empty() && !case.name.contains(&filter) { continue; }
        // Text containing a carriage return of its own cannot be told apart.
        if case.lines.iter().any(|l| l.contains('\r')) || case.keys.contains("<C-v><C-m>") || case.keys.contains("<C-m>") { continue; }
        ran += 1;
        let got = std::panic::catch_unwind(|| replay_with(&case.lines, case.eol, case.cursor, &case.keys, "\r\n"));
        let (got, raw) = match got {
            Ok(g) => g,
            Err(_) => { failures.push(format!("{}: PANIC", case.name)); continue; }
        };
        let e = &case.expect;
        let mut diffs = Vec::new();
        let plain = raw.replace("\r\n", "\n");
        if plain.contains('\r') { diffs.push(format!("stray \\r in {raw:?}")); }
        let lines: Vec<String> = got.lines.iter().map(|l| l.trim_end_matches('\r').to_string()).collect();
        if lines != e.lines { diffs.push(format!("lines {:?} != vim {:?}", lines, e.lines)); }
        if got.cursor != e.cursor { diffs.push(format!("cursor {:?} != vim {:?}", got.cursor, e.cursor)); }
        let reg = got.reg.replace("\r\n", "\n");
        if reg != e.reg || (!e.reg.is_empty() && got.regtype != e.regtype) {
            diffs.push(format!("reg {:?}/{:?} != vim {:?}/{:?}", got.reg, got.regtype, e.reg, e.regtype));
        }
        if !diffs.is_empty() {
            failures.push(format!("{}: {}", case.name, diffs.join("; ")));
        }
    }
    if !failures.is_empty() {
        let shown: Vec<&String> = failures.iter().take(400).collect();
        if let Ok(path) = std::env::var("VIM_PARITY_REPORT") {
            let _ = std::fs::write(path, failures.join("\n"));
        }
        panic!("{} of {} CRLF cases differ from Vim:\n{}", failures.len(), ran, shown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"));
    }
}

#[test]
fn engine_matches_real_vim() {
    let cases = corpus();
    let filter = std::env::var("VIM_PARITY_FILTER").unwrap_or_default();
    let mut failures = Vec::new();
    let mut ran = 0;
    for case in &cases {
        if !filter.is_empty() && !case.name.contains(&filter) { continue; }
        ran += 1;
        let got = std::panic::catch_unwind(|| replay(&case.lines, case.eol, case.cursor, &case.keys));
        let got = match got {
            Ok(g) => g,
            Err(_) => { failures.push(format!("{}: PANIC", case.name)); continue; }
        };
        let e = &case.expect;
        let mut diffs = Vec::new();
        // Text, cursor and the unnamed register must all match Vim exactly.
        if got.lines != e.lines { diffs.push(format!("lines {:?} != vim {:?}", got.lines, e.lines)); }
        if got.cursor != e.cursor { diffs.push(format!("cursor {:?} != vim {:?}", got.cursor, e.cursor)); }
        if got.reg != e.reg || (!e.reg.is_empty() && got.regtype != e.regtype) {
            diffs.push(format!("reg {:?}/{:?} != vim {:?}/{:?}", got.reg, got.regtype, e.reg, e.regtype));
        }
        if !diffs.is_empty() {
            failures.push(format!("{}: {}", case.name, diffs.join("; ")));
        }
    }
    if !failures.is_empty() {
        let shown: Vec<&String> = failures.iter().take(400).collect();
        if let Ok(path) = std::env::var("VIM_PARITY_REPORT") {
            let _ = std::fs::write(path, failures.join("\n"));
        }
        panic!("{} of {} cases differ from Vim:\n{}", failures.len(), ran, shown.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n"));
    }
}
