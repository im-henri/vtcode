//! Ex commands: ranges, `:substitute`, `:global`, `:normal`, line
//! commands, options, mappings and workbench commands.

use super::exec::{self, cursor, push_jump, set_cursor, update_curswant, Tx};
use super::keys::Key;
use super::map::{self, MapMode, Mapping};
use super::register::{Reg, RegKind};
use super::search;
use super::text::{Pos, Text};
use super::{Act, Host, HostClip, Mode, VimState};

type R<T> = Result<T, String>;

#[derive(Clone, Debug)]
pub(crate) struct Substitute {
    pub(crate) pattern: String,
    pub(crate) replacement: String,
    pub(crate) flags: String,
}

/// Interactive `:s///c` state.
#[derive(Clone, Debug)]
pub(crate) struct Confirm {
    /// Remaining matches as (start, end) char offsets in the current text.
    matches: Vec<(usize, usize)>,
    pattern: String,
    replacement: String,
    boundary: Option<u64>,
    done: usize,
    lines: std::collections::BTreeSet<usize>,
    all: bool,
}

/// One Ex command line (possibly several commands separated by `|`).
pub(crate) fn run(vim: &mut VimState, host: &mut dyn Host, line: &str) -> R<()> {
    vim.last_ex = Some(line.to_string());
    for part in split_bar(line) {
        run_one(vim, host, &part)?;
        if vim.ex_abort { vim.ex_abort = false; break; }
    }
    Ok(())
}

/// Split at `|` unless the command takes `|` as an argument.
fn split_bar(line: &str) -> Vec<String> {
    let trimmed = line.trim_start_matches([' ', ':']);
    let name: String = trimmed.chars().skip_while(|c| c.is_ascii_digit() || ",.;$%'+-".contains(*c)).take_while(|c| c.is_ascii_alphabetic()).collect();
    if ["g", "gl", "glo", "glob", "globa", "global", "v", "vg", "vgl", "vglobal", "norm", "norma", "normal", "map", "nmap", "vmap", "xmap", "imap", "omap", "cmap",
        "noremap", "nnoremap", "vnoremap", "xnoremap", "inoremap", "onoremap", "cnoremap", "nno", "vno", "xno", "ino", "ono", "cno", "no", "nm", "vm", "xm", "im", "om", "cm"]
        .contains(&name.as_str()) || trimmed.contains('!') && name.is_empty()
    {
        return vec![line.to_string()];
    }
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'|') {
            cur.push('|');
            chars.next();
        } else if c == '|' {
            parts.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    parts.push(cur);
    parts
}

// ---------------------------------------------------------------------------
// Ranges

struct Range {
    start: usize,
    end: usize,
    given: usize,
}

fn parse_number(s: &[char], i: &mut usize) -> Option<usize> {
    let st = *i;
    while *i < s.len() && s[*i].is_ascii_digit() { *i += 1; }
    (st < *i).then(|| s[st..*i].iter().collect::<String>().parse().ok()).flatten()
}

fn address(vim: &mut VimState, host: &mut dyn Host, s: &[char], i: &mut usize, cur_line: usize) -> R<Option<isize>> {
    let ed = host.ed_ref().ok_or("E32: No file name")?;
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let last = t.last_line() as isize;
    while *i < s.len() && s[*i] == ' ' { *i += 1; }
    let mut base: Option<isize> = None;
    if *i < s.len() {
        match s[*i] {
            '.' => { *i += 1; base = Some(cur_line as isize); }
            '$' => { *i += 1; base = Some(last); }
            '0'..='9' => { base = Some(parse_number(s, i).unwrap_or(0) as isize - 1); }
            '\'' => {
                *i += 1;
                let m = *s.get(*i).ok_or("E20: Mark not set")?;
                *i += 1;
                let p = exec::get_mark(vim, host, m).map_err(|e| if e.starts_with('\0') { "E20: Mark not set".to_string() } else { e })?;
                base = Some(p.line as isize);
            }
            '/' | '?' => {
                let delim = s[*i];
                *i += 1;
                let rest: String = s[*i..].iter().collect();
                let (pat, after) = super::cmdline::split_pattern(&rest, delim);
                *i += pat.chars().count() + usize::from(after.is_some());
                let pat = if pat.is_empty() { vim.last_search.as_ref().map(|(p, _)| p.clone()).ok_or("E35: No previous regular expression")? } else { pat };
                vim.last_search = Some((pat.clone(), delim == '/'));
                vim.regs.last_search = pat.clone();
                let re = search::compile(&pat, &vim.search_options())?;
                let ed = host.ed_ref().ok_or("E32")?;
                let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                let hay = exec::haystack(ed);
                let from = if delim == '/' { t.line_end(cur_line) } else { t.line_start(cur_line) };
                let found = search::search(&ed.rope, &hay, &re, from, delim == '/', vim.opts.wrapscan, false)
                    .ok_or_else(|| format!("E486: Pattern not found: {pat}"))?;
                base = Some(t.pos(found.0).line as isize);
            }
            '\\' => {
                *i += 1;
                let c = s.get(*i).copied();
                *i += 1;
                let (pat, fwd) = match c {
                    Some('/') => (vim.last_search.as_ref().map(|s| s.0.clone()), true),
                    Some('?') => (vim.last_search.as_ref().map(|s| s.0.clone()), false),
                    Some('&') => (vim.last_sub.as_ref().map(|s| s.pattern.clone()), true),
                    _ => return Err("E10: \\ should be followed by /, ? or &".into()),
                };
                let pat = pat.ok_or("E35: No previous regular expression")?;
                let re = search::compile(&pat, &vim.search_options())?;
                let ed = host.ed_ref().ok_or("E32")?;
                let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                let hay = exec::haystack(ed);
                let from = if fwd { t.line_end(cur_line) } else { t.line_start(cur_line) };
                let found = search::search(&ed.rope, &hay, &re, from, fwd, vim.opts.wrapscan, false).ok_or("E486: Pattern not found")?;
                base = Some(t.pos(found.0).line as isize);
            }
            _ => {}
        }
    }
    // Offsets: +N, -N, +, -.
    loop {
        while *i < s.len() && s[*i] == ' ' && s.get(*i + 1).is_some_and(|c| *c == '+' || *c == '-') { *i += 1; }
        let Some(&c) = s.get(*i) else { break; };
        if c != '+' && c != '-' { break; }
        *i += 1;
        let n = parse_number(s, i).unwrap_or(1) as isize;
        let b = base.unwrap_or(cur_line as isize);
        base = Some(if c == '+' { b + n } else { b - n });
    }
    Ok(base)
}

fn parse_range(vim: &mut VimState, host: &mut dyn Host, s: &[char], i: &mut usize) -> R<Range> {
    let ed = host.ed_ref().ok_or("E32: No file name")?;
    let cur = ed.cursor_line;
    let last = Text::of(&ed.rope, ed.vim_buf.trailing_empty).last_line();
    while *i < s.len() && (s[*i] == ' ' || s[*i] == ':') { *i += 1; }
    if s.get(*i) == Some(&'%') {
        *i += 1;
        return Ok(Range { start: 0, end: last, given: 2 });
    }
    if s.get(*i) == Some(&'*') {
        *i += 1;
        let a = exec::get_mark(vim, host, '<')?;
        let b = exec::get_mark(vim, host, '>')?;
        return Ok(Range { start: a.line, end: b.line, given: 2 });
    }
    let mut lines: Vec<isize> = Vec::new();
    let mut cur_line = cur;
    loop {
        let a = address(vim, host, s, i, cur_line)?;
        while *i < s.len() && s[*i] == ' ' { *i += 1; }
        match s.get(*i) {
            Some(',') => {
                lines.push(a.unwrap_or(cur_line as isize));
                *i += 1;
            }
            Some(';') => {
                let v = a.unwrap_or(cur_line as isize);
                lines.push(v);
                cur_line = v.clamp(0, last as isize) as usize;
                *i += 1;
            }
            _ => {
                if let Some(a) = a { lines.push(a); } else if !lines.is_empty() { lines.push(cur_line as isize); }
                break;
            }
        }
    }
    let given = lines.len().min(2);
    let (a, b) = match lines.len() {
        0 => (cur as isize, cur as isize),
        1 => (lines[0], lines[0]),
        n => (lines[n - 2], lines[n - 1]),
    };
    if a < -1 || b < -1 {
        return Err("E16: Invalid range".into());
    }
    let (a, b) = (a.min(last as isize + 1), b.min(last as isize + 1));
    let (a, b) = (a.max(0) as usize, b.max(0) as usize);
    let (a, b) = if a > b { (b, a) } else { (a, b) };
    Ok(Range { start: a, end: b, given })
}

// ---------------------------------------------------------------------------
// Command table

const COMMANDS: &[(&str, usize)] = &[
    ("append", 1), ("buffer", 1), ("bNext", 2), ("bdelete", 2), ("bfirst", 2), ("blast", 2), ("bnext", 2), ("bprevious", 2),
    ("brewind", 2), ("buffers", 7), ("bwipeout", 2), ("cd", 2), ("center", 2), ("changes", 7), ("close", 3), ("copy", 2),
    ("delete", 1), ("delmarks", 4), ("display", 2), ("echo", 2), ("edit", 1), ("enew", 3), ("exit", 3), ("file", 1), ("files", 5),
    ("global", 1), ("help", 1), ("join", 1), ("jumps", 2), ("k", 1), ("left", 2), ("let", 3), ("ls", 2), ("lcd", 2), ("mark", 2),
    ("marks", 5), ("move", 1), ("new", 3), ("nohlsearch", 3), ("normal", 4), ("only", 2), ("print", 1), ("put", 2), ("pwd", 2),
    ("quit", 1), ("qall", 2), ("quitall", 5), ("read", 1), ("redo", 3), ("registers", 3), ("retab", 3), ("right", 2),
    ("saveas", 3), ("set", 2), ("setglobal", 4), ("setlocal", 4), ("silent", 3), ("source", 2), ("sort", 3), ("split", 2),
    ("startinsert", 4), ("startreplace", 6), ("stopinsert", 5), ("substitute", 1), ("t", 1), ("tabclose", 4), ("tabedit", 4),
    ("tabfirst", 6), ("tablast", 4), ("tabnew", 6), ("tabnext", 4), ("tabNext", 4), ("tabonly", 4), ("tabprevious", 4),
    ("tabrewind", 4), ("tcd", 3), ("terminal", 3), ("undo", 1), ("update", 2), ("vglobal", 1), ("version", 2), ("vnew", 3),
    ("vsplit", 2), ("wall", 2), ("wincmd", 4), ("wq", 2), ("wqall", 3), ("write", 1), ("xit", 1), ("xall", 2), ("yank", 1),
    ("Explore", 1), ("Sexplore", 2), ("Vexplore", 2), ("Lexplore", 2), ("vtcode", 6), ("VT", 2), ("execute", 3), ("call", 3),
    ("colorscheme", 4), ("syntax", 2), ("filetype", 5), ("autocmd", 2), ("augroup", 3), ("highlight", 2), ("command", 3),
    ("noremap", 2), ("nnoremap", 2), ("vnoremap", 2), ("xnoremap", 2), ("onoremap", 3), ("inoremap", 3), ("cnoremap", 3),
    ("snoremap", 4), ("map", 3), ("nmap", 2), ("vmap", 2), ("xmap", 2), ("omap", 2), ("imap", 2), ("cmap", 2), ("smap", 4),
    ("unmap", 3), ("nunmap", 3), ("vunmap", 2), ("xunmap", 2), ("ounmap", 2), ("iunmap", 2), ("cunmap", 2), ("sunmap", 4),
    ("mapclear", 4), ("nmapclear", 5), ("vmapclear", 5), ("xmapclear", 5), ("omapclear", 5), ("imapclear", 5), ("cmapclear", 5),
    ("copen", 4), ("cwindow", 2), ("clist", 2), ("cclose", 3), ("cnext", 2), ("cNext", 2), ("cprevious", 2),
    ("smagic", 2), ("snomagic", 3), ("wnext", 2), ("windo", 5), ("bufdo", 5), ("tabdo", 4),
    ("messages", 3), ("checktime", 6), ("redraw", 4), ("echomsg", 5), ("echoerr", 5), ("unlet", 3), ("language", 3),
    ("runtime", 2), ("packadd", 2), ("Plug", 4), ("lua", 3), ("abbreviate", 2), ("iabbrev", 2), ("cabbrev", 2),
];

fn resolve(name: &str) -> Option<&'static str> {
    if let Some(&(full, _)) = COMMANDS.iter().find(|(full, _)| *full == name) { return Some(full); }
    COMMANDS.iter().find(|(full, min)| name.len() >= *min && full.starts_with(name)).map(|(f, _)| *f)
}

fn run_one(vim: &mut VimState, host: &mut dyn Host, text: &str) -> R<()> {
    let s: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < s.len() && (s[i] == ' ' || s[i] == ':' || s[i] == '\t') { i += 1; }
    if i >= s.len() { return Ok(()); }
    if s[i] == '"' { return Ok(()); }
    if host.ed_ref().is_none() {
        return run_without_buffer(vim, host, &s[i..].iter().collect::<String>());
    }
    let range = parse_range(vim, host, &s, &mut i)?;
    while i < s.len() && s[i] == ' ' { i += 1; }
    // Command name.
    let name: String = if i < s.len() && s[i].is_ascii_alphabetic() {
        let st = i;
        while i < s.len() && s[i].is_ascii_alphabetic() { i += 1; }
        // `:s` variants like `:s#a#b#`, `:sno`, `:k{a}`: keep only known names.
        let n: String = s[st..i].iter().collect();
        if n.len() > 1 && n.starts_with('k') && resolve(&n).is_none() && n.len() == 2 {
            i = st + 1;
            "k".into()
        } else if n.starts_with('s') && resolve(&n).is_none() && !n.starts_with("se") {
            // `:sg`, `:si`... are :s with flags; treat as substitute.
            i = st + 1;
            "s".into()
        } else { n }
    } else if i < s.len() {
        let c = s[i];
        i += 1;
        if c == '&' && s.get(i) == Some(&'&') { i += 1; "&&".into() } else { c.to_string() }
    } else {
        String::new()
    };
    let bang = s.get(i) == Some(&'!') && !matches!(name.as_str(), "!" | "=");
    if bang { i += 1; }
    let arg: String = s[i..].iter().collect::<String>();
    let arg_trim = arg.trim();
    if !name.is_empty() && range.end > host.ed_ref().map_or(0, |e| Text::of(&e.rope, e.vim_buf.trailing_empty).last_line()) && full_needs_range(&name) {
        return Err("E16: Invalid range".into());
    }
    if name.is_empty() {
        // `:N` or a range alone: go to its last line.
        if range.given == 0 { return Ok(()); }
        let Some(ed) = host.ed() else { return Ok(()); };
        push_jump(vim, ed);
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        let line = range.end.min(t.last_line());
        set_cursor(ed, Pos::new(line, t.first_nonblank(line)));
        update_curswant(vim, ed);
        return Ok(());
    }
    let full = match name.as_str() {
        "!" | "&" | "&&" | "~" | "<" | ">" | "=" | "#" | "@" | "*" => name.as_str(),
        n => resolve(n).ok_or_else(|| format!("E492: Not an editor command: {}", text.trim()))?,
    };
    // Map commands.
    if let Some((modes, noremap)) = map::modes_for(full) {
        return map_cmd(vim, &modes, noremap, bang, arg_trim);
    }
    if full.ends_with("unmap") {
        let modes = map::modes_for(&full.replace("unmap", "map")).map(|m| m.0).unwrap_or_default();
        let lhs = map::map_keys(arg_trim, &vim.leader.clone());
        if !vim.maps.remove(&modes, &lhs) { return Err(format!("E31: No such mapping: {arg_trim}")); }
        return Ok(());
    }
    if full.ends_with("mapclear") {
        let modes = map::modes_for(&full.replace("mapclear", "map")).map(|m| m.0).unwrap_or_default();
        vim.maps.clear(&modes);
        return Ok(());
    }
    match full {
        "write" | "update" | "wq" | "xit" | "exit" => {
            let path = (!arg_trim.is_empty()).then(|| arg_trim.to_string());
            let write = match full {
                "update" | "xit" | "exit" => host.ed_ref().is_some_and(|e| e.modified || e.file_path.is_none()) || path.is_some(),
                _ => true,
            };
            if let Some(cmd) = arg_trim.strip_prefix('!').filter(|_| full == "write") {
                // :w !cmd pipes lines to a command.
                let text = range_text(host, &range);
                let out = shell(host, cmd.trim(), Some(&text))?;
                show_output(vim, &out);
                return Ok(());
            }
            let quit = full != "write" && full != "update";
            if write && quit && path.is_none() {
                // Saved and closed together: the save is synchronous, so a
                // failed save keeps the window open.
                return host.act(Act::WriteQuit { force: bang });
            }
            if write {
                if range.given > 0 && path.is_some() && !(range.start == 0 && range.end == Text::of(&host.ed_ref().unwrap().rope, host.ed_ref().unwrap().vim_buf.trailing_empty).last_line()) {
                    return Err("E140: Writing part of a file is not supported; use :'<,'>w !cat > file".into());
                }
                host.act(Act::Write { path, force: bang })?;
            }
            if quit {
                host.act(Act::Quit { force: bang })?;
            }
            Ok(())
        }
        "saveas" => {
            if arg_trim.is_empty() { return Err("E471: Argument required".into()); }
            host.act(Act::Write { path: Some(arg_trim.into()), force: bang })
        }
        "wall" => host.act(Act::WriteAll),
        "wqall" | "xall" => host.act(Act::WriteQuitAll),
        "quit" | "close" => host.act(Act::Quit { force: bang }),
        "qall" | "quitall" => host.act(Act::QuitAll { force: bang }),
        "edit" => {
            if arg_trim == "#" { return host.act(Act::Alternate); }
            host.act(Act::Edit { path: (!arg_trim.is_empty()).then(|| arg_trim.to_string()), force: bang })
        }
        "enew" => host.act(Act::NewBuffer),
        "new" | "split" => {
            if !arg_trim.is_empty() || full == "new" {
                host.act(Act::Window(super::parse::Window::Split, 1))?;
                if full == "new" && arg_trim.is_empty() { return host.act(Act::NewBuffer); }
                return host.act(Act::Edit { path: Some(arg_trim.into()), force: false });
            }
            host.act(Act::Window(super::parse::Window::Split, 1))
        }
        "vnew" | "vsplit" => {
            host.act(Act::Window(super::parse::Window::VSplit, 1))?;
            if !arg_trim.is_empty() { return host.act(Act::Edit { path: Some(arg_trim.into()), force: false }); }
            if full == "vnew" { return host.act(Act::NewBuffer); }
            Ok(())
        }
        "only" => host.act(Act::Window(super::parse::Window::Only, 1)),
        "wincmd" => {
            let keys = super::keys::parse(&format!("<C-w>{arg_trim}"), &[]);
            vim.feed_keys(host, &keys, false, false);
            Ok(())
        }
        "tabnext" => host.act(Act::TabNext(arg_trim.parse().ok())),
        "tabprevious" | "tabNext" => host.act(Act::TabPrev(arg_trim.parse().unwrap_or(1))),
        "tabfirst" | "tabrewind" => host.act(Act::TabNext(Some(1))),
        "tablast" => host.act(Act::TabNext(Some(usize::MAX))),
        "tabnew" | "tabedit" => {
            if arg_trim.is_empty() { host.act(Act::NewBuffer) } else { host.act(Act::Edit { path: Some(arg_trim.into()), force: false }) }
        }
        "tabclose" => host.act(Act::TabClose { force: bang }),
        "tabonly" => host.act(Act::TabOnly),
        "bnext" => host.act(Act::BufferNext(arg_trim.parse().unwrap_or(1))),
        "bprevious" | "bNext" => host.act(Act::BufferPrev(arg_trim.parse().unwrap_or(1))),
        "bfirst" | "brewind" => host.act(Act::Buffer("1".into())),
        "blast" => host.act(Act::Buffer("$".into())),
        "buffer" => {
            if arg_trim.is_empty() && range.given > 0 { return host.act(Act::Buffer((range.end + 1).to_string())); }
            if arg_trim.is_empty() { return Ok(()); }
            if arg_trim == "#" { return host.act(Act::Alternate); }
            host.act(Act::Buffer(arg_trim.into()))
        }
        "bdelete" | "bwipeout" => host.act(Act::BufferDelete { force: bang }),
        "buffers" | "files" | "ls" => {
            let lines: Vec<String> = host.buffers().into_iter().map(|(n, name, modified)| {
                format!("{:>3} {} \"{}\"", n, if modified { "+" } else { " " }, name)
            }).collect();
            vim.set_lines(lines);
            Ok(())
        }
        "copen" | "cwindow" | "clist" => host.act(Act::Quickfix(super::Quickfix::Open)),
        "cclose" => host.act(Act::Quickfix(super::Quickfix::Close)),
        "cnext" => host.act(Act::Quickfix(super::Quickfix::Next)),
        "cprevious" | "cNext" => host.act(Act::Quickfix(super::Quickfix::Prev)),
        "Explore" | "Sexplore" | "Vexplore" | "Lexplore" => host.act(Act::Explore),
        "terminal" => host.act(Act::Terminal((!arg_trim.is_empty()).then(|| arg_trim.to_string()))),
        "cd" | "lcd" | "tcd" => host.act(Act::Cd((!arg_trim.is_empty()).then(|| arg_trim.to_string()))),
        "pwd" => {
            let d = host.cwd().map(|p| p.display().to_string()).unwrap_or_default();
            vim.set_message(d);
            Ok(())
        }
        "vtcode" | "VT" => {
            if arg_trim.is_empty() { return Err("E471: Argument required".into()); }
            host.act(Act::Command(arg_trim.into()))
        }
        "file" => {
            let keys = [Key::ctrl('g')];
            vim.feed_keys(host, &keys, false, false);
            Ok(())
        }
        "nohlsearch" | "noh" | "nohl" => { vim.hl_hidden = true; Ok(()) }
        "substitute" | "&" | "&&" | "~" | "smagic" | "snomagic" => substitute(vim, host, &range, full, &arg),
        "global" | "vglobal" => global(vim, host, &range, full == "vglobal" || bang, &arg),
        "normal" => normal(vim, host, &range, &arg, !bang),
        "delete" | "yank" => {
            let (reg, count) = reg_count(arg_trim);
            let range = with_count(host, range, count);
            let region = exec::Region { start: Pos::new(range.start, 0), end: Pos::new(range.end, 0), kind: exec::RKind::Line, nl: false };
            let saved = host.ed_ref().map(cursor);
            if let Some(ed) = host.ed() { ed.cursor_line = range.start; }
            let op = if full == "delete" { super::parse::Op::Delete } else { super::parse::Op::Yank };
            if op == super::parse::Op::Delete && read_only(host) { return Err("E21: Cannot make changes, 'modifiable' is off".into()); }
            super::ops::apply(vim, host, reg, op, region, 1, false, false);
            if op == super::parse::Op::Yank {
                if let (Some(ed), Some(p)) = (host.ed(), saved) { set_cursor(ed, p); }
            }
            Ok(())
        }
        "put" => {
            let reg = arg_trim.chars().next();
            let Some(value) = super::ops::read_reg(vim, host, reg) else { return Err(format!("E353: Nothing in register {}", reg.unwrap_or('"'))); };
            let mut value = value;
            value.kind = RegKind::Line;
            if !value.text.ends_with('\n') { value.text.push('\n'); }
            let Some(ed) = host.ed() else { return Ok(()); };
            // `:0put` puts above the first line.
            let line = if range.given > 0 { range.end } else { ed.cursor_line };
            let above = bang || (range.given > 0 && s_starts_zero(text));
            ed.cursor_line = line;
            super::ops::put_value(vim, ed, &value, above, false, false, 1);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let last = ed.vim_buf.marks.get(&']').map_or(ed.cursor_line, |p| p.line);
            set_cursor(ed, Pos::new(last, t.first_nonblank(last)));
            Ok(())
        }
        "move" | "copy" | "t" => {
            let mut j = 0;
            let a: Vec<char> = arg_trim.chars().collect();
            let dest = if a.first() == Some(&'0') && a.len() == 1 { -1 } else {
                let cur = host.ed_ref().map_or(0, |e| e.cursor_line);
                address(vim, host, &a, &mut j, cur)?.ok_or("E14: Invalid address")?
            };
            move_copy(vim, host, &range, dest, full == "move")
        }
        "join" => {
            let (_, count) = reg_count(arg_trim);
            let Some(ed) = host.ed() else { return Ok(()); };
            let (s, mut e) = (range.start, range.end);
            if let Some(c) = count { e = range.end + c - 1; } else if range.given < 2 || s == e { e = s + 1; }
            if read_only_ed(ed) { return Err("E21".into()); }
            super::ops::join(vim, ed, s, e, !bang);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let line = ed.cursor_line;
            set_cursor(ed, Pos::new(line, t.first_nonblank(line)));
            Ok(())
        }
        "<" | ">" => {
            let extra = arg.chars().take_while(|&c| c.to_string() == full).count();
            let rest: String = arg.chars().skip(extra).collect();
            let (_, count) = reg_count(rest.trim());
            let range = with_count(host, range, count);
            let Some(ed) = host.ed() else { return Ok(()); };
            if read_only_ed(ed) { return Err("E21".into()); }
            let mut tx = Tx::default();
            super::ops::shift_lines(vim, ed, &mut tx, range.start, range.end, full == ">", extra + 1);
            tx.finish(ed);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            set_cursor(ed, Pos::new(range.end, t.first_nonblank(range.end)));
            Ok(())
        }
        "k" | "mark" => {
            let m = arg_trim.chars().next().ok_or("E471: Argument required")?;
            let Some(ed) = host.ed() else { return Ok(()); };
            let p = Pos::new(range.end, 0);
            if !exec::set_mark(vim, ed, m, p) { return Err("E191: Argument must be a letter or forward/backward quote".into()); }
            Ok(())
        }
        "delmarks" => {
            let Some(ed) = host.ed() else { return Ok(()); };
            if bang { ed.vim_buf.marks.retain(|c, _| !c.is_ascii_lowercase()); return Ok(()); }
            let chars: Vec<char> = arg_trim.chars().filter(|c| *c != ' ').collect();
            let mut k = 0;
            while k < chars.len() {
                if k + 2 < chars.len() && chars[k + 1] == '-' {
                    for c in chars[k]..=chars[k + 2] { ed.vim_buf.marks.remove(&c); vim.global_marks.remove(&c); }
                    k += 3;
                } else {
                    ed.vim_buf.marks.remove(&chars[k]);
                    vim.global_marks.remove(&chars[k]);
                    k += 1;
                }
            }
            Ok(())
        }
        "marks" => {
            let Some(ed) = host.ed_ref() else { return Ok(()); };
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let mut lines = vec!["mark line  col file/text".to_string()];
            for (c, p) in &ed.vim_buf.marks {
                if *c == '"' { continue; }
                if p.line > t.last_line() { continue; }
                lines.push(format!(" {c} {:>6} {:>4} {}", p.line + 1, p.col, t.line_string(p.line).trim()));
            }
            for (c, j) in &vim.global_marks {
                let name = j.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                lines.push(format!(" {c} {:>6} {:>4} {name}", j.pos.line + 1, j.pos.col));
            }
            vim.set_lines(lines);
            Ok(())
        }
        "registers" | "display" => {
            let file = host.file_name();
            let filter: Vec<char> = arg_trim.chars().filter(|c| *c != ' ').collect();
            let mut clip = HostClip(host);
            let list = vim.regs.list(&mut clip, file.as_deref());
            let mut lines = vec!["Type Name Content".to_string()];
            for (r, reg) in list {
                if !filter.is_empty() && !filter.contains(&r) { continue; }
                let kind = match reg.kind { RegKind::Char => 'c', RegKind::Line => 'l', RegKind::Block(_) => 'b' };
                let shown: String = super::ops::keys_to_text(&super::ops::text_to_keys(&reg.text)).chars().map(|c| {
                    if c == '\n' { "^J".to_string() } else if (c as u32) < 0x20 { format!("^{}", ((c as u8) ^ 0x40) as char) } else { c.to_string() }
                }).collect();
                let shown: String = shown.chars().take(120).collect();
                lines.push(format!("  {kind}  \"{r}   {shown}"));
            }
            vim.set_lines(lines);
            Ok(())
        }
        "jumps" => {
            let mut lines = vec![" jump line  col file/text".to_string()];
            for (n, j) in vim.jumps.iter().enumerate() {
                let rel = (n as isize - vim.jump_idx as isize).unsigned_abs();
                let name = j.path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                lines.push(format!("{}{rel:>4} {:>5} {:>4} {name}", if n == vim.jump_idx { '>' } else { ' ' }, j.pos.line + 1, j.pos.col));
            }
            if vim.jump_idx >= vim.jumps.len() { lines.push(">".into()); }
            vim.set_lines(lines);
            Ok(())
        }
        "changes" => {
            let Some(ed) = host.ed_ref() else { return Ok(()); };
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let mut lines = vec!["change line  col text".to_string()];
            let idx = ed.vim_buf.change_idx;
            for (n, p) in ed.vim_buf.changes.iter().enumerate() {
                let rel = (n as isize - idx as isize).unsigned_abs();
                lines.push(format!("{}{rel:>5} {:>5} {:>4} {}", if n == idx { '>' } else { ' ' }, p.line + 1, p.col, t.line_string(p.line.min(t.last_line())).trim()));
            }
            vim.set_lines(lines);
            Ok(())
        }
        "undo" => {
            let keys = [Key::ch('u')];
            vim.feed_keys(host, &keys, false, false);
            Ok(())
        }
        "redo" => {
            let keys = [Key::ctrl('r')];
            vim.feed_keys(host, &keys, false, false);
            Ok(())
        }
        "set" | "setlocal" | "setglobal" => set(vim, host, arg_trim),
        "let" => let_cmd(vim, host, arg_trim),
        "source" => {
            let path = expand_path(host, arg_trim);
            let text = std::fs::read_to_string(&path).map_err(|e| format!("E484: Can't open file {arg_trim}: {e}"))?;
            let errors = source(vim, host, &text);
            if !errors.is_empty() { vim.set_lines(errors); }
            Ok(())
        }
        "sort" => sort(vim, host, &range, bang, arg_trim),
        "!" => {
            if range.given > 0 {
                filter(vim, host, &range, arg.trim())
            } else {
                let out = shell(host, arg.trim(), None)?;
                show_output(vim, &out);
                Ok(())
            }
        }
        "read" => {
            let Some(ed) = host.ed() else { return Ok(()); };
            let line = if range.given > 0 { range.end } else { ed.cursor_line };
            let _ = ed;
            let text = if let Some(cmd) = arg_trim.strip_prefix('!') {
                shell(host, cmd.trim(), None)?
            } else {
                let path = expand_path(host, arg_trim);
                std::fs::read_to_string(&path).map_err(|e| format!("E484: Can't open file {arg_trim}: {e}"))?
            };
            let Some(ed) = host.ed() else { return Ok(()); };
            ed.cursor_line = line;
            let mut text = exec::plain(&text);
            if !text.ends_with('\n') { text.push('\n'); }
            super::ops::put_value(vim, ed, &Reg::new(text, RegKind::Line), false, false, false, 1);
            Ok(())
        }
        "=" => {
            let Some(ed) = host.ed_ref() else { return Ok(()); };
            let n = if range.given > 0 { range.end + 1 } else { Text::of(&ed.rope, ed.vim_buf.trailing_empty).lines() };
            vim.set_message(n.to_string());
            Ok(())
        }
        "print" | "#" => {
            let Some(ed) = host.ed_ref() else { return Ok(()); };
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let lines: Vec<String> = (range.start..=range.end).map(|l| if full == "#" { format!("{:>3} {}", l + 1, t.line_string(l)) } else { t.line_string(l) }).collect();
            if lines.len() == 1 { vim.set_message(lines[0].clone()); } else { vim.set_lines(lines); }
            Ok(())
        }
        "echo" | "echomsg" | "echoerr" => {
            let m = arg_trim.trim_matches(|c| c == '"' || c == '\'').to_string();
            if full == "echoerr" { vim.set_error(m); } else { vim.set_message(m); }
            Ok(())
        }
        "messages" | "checktime" | "redraw" | "syntax" | "filetype" | "autocmd" | "augroup" | "highlight" | "runtime"
        | "packadd" | "Plug" | "language" | "unlet" | "call" | "lua" | "abbreviate" | "iabbrev" | "cabbrev" => Ok(()),
        "colorscheme" => {
            if arg_trim.is_empty() { return Ok(()); }
            host.act(Act::SetOption { name: "colorscheme".into(), value: arg_trim.into() })
        }
        "startinsert" | "startreplace" => {
            let kind = if bang { super::parse::InsertKind::LineEnd } else { super::parse::InsertKind::Before };
            if vim.is_visual() { exec::exit_visual(vim, host); }
            super::insert::begin(vim, host, kind, 1, false, None);
            if full == "startreplace" { vim.mode = Mode::Replace; }
            Ok(())
        }
        "stopinsert" => {
            if vim.is_insert() { super::insert::finish(vim, host, true); }
            Ok(())
        }
        "silent" => {
            let rest = arg.trim_start_matches('!').trim();
            let r = run(vim, host, rest);
            if r.is_err() && bang { return Ok(()); }
            vim.message = None;
            r
        }
        "execute" => {
            // :execute "normal! ..." with simple string literals.
            let mut out = String::new();
            for part in split_strings(arg_trim) { out.push_str(&part); }
            run(vim, host, &out)
        }
        "help" => {
            vim.set_message(format!("Vim help is not bundled; see :h {} at vimhelp.org", if arg_trim.is_empty() { "index" } else { arg_trim }));
            Ok(())
        }
        "version" => { vim.set_message("VTCode Vim mode (Vim 9 compatible normal, visual, insert and ex commands)"); Ok(()) }
        "left" | "right" | "center" => align(vim, host, &range, full, arg_trim),
        "retab" => retab(vim, host, &range, bang, arg_trim),
        "windo" | "bufdo" | "tabdo" => run(vim, host, arg_trim),
        "append" => Err("E319: :append is not supported; use o".into()),
        "command" => Err("E319: user commands are not supported; use mappings".into()),
        "wnext" => Err("E163: There is only one file to edit".into()),
        "@" | "*" => {
            let r = arg_trim.chars().next().unwrap_or('@');
            if let Some(ed) = host.ed() { ed.cursor_line = range.end; }
            let keys = [Key::ch('@'), Key::ch(if r == '@' && full == "*" { '@' } else { r })];
            vim.feed_keys(host, &keys, false, false);
            Ok(())
        }
        _ => Err(format!("E492: Not an editor command: {}", text.trim())),
    }
}

/// Commands that act on buffer lines (a range past the end is an error).
fn full_needs_range(name: &str) -> bool {
    !matches!(resolve(name), Some("write" | "update" | "wq" | "xit" | "exit" | "quit" | "qall" | "quitall" | "edit" | "split" | "vsplit" | "buffer"
        | "tabnext" | "tabprevious" | "set" | "map" | "nnoremap" | "let" | "echo" | "normal"))
}

fn s_starts_zero(text: &str) -> bool {
    let t = text.trim_start_matches([' ', ':']);
    t.starts_with('0') && !t[1..].starts_with(|c: char| c.is_ascii_digit())
}

fn split_strings(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '"' || c == '\'' {
            let mut lit = String::new();
            while let Some(n) = chars.next() {
                if n == c { break; }
                if n == '\\' && c == '"' {
                    match chars.next() {
                        Some('<') => {
                            let mut name = String::from("<");
                            for k in chars.by_ref() { name.push(k); if k == '>' { break; } }
                            lit.push_str(&name);
                        }
                        Some('e') => lit.push_str("<Esc>"),
                        Some('r') => lit.push_str("<CR>"),
                        Some('t') => lit.push('\t'),
                        Some(o) => lit.push(o),
                        None => {}
                    }
                } else {
                    lit.push(n);
                }
            }
            out.push(lit);
        } else if c == '.' || c == ' ' {
        } else {
            out.push(c.to_string());
        }
    }
    out
}

fn run_without_buffer(vim: &mut VimState, host: &mut dyn Host, text: &str) -> R<()> {
    let name: String = text.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let bang = text[name.len()..].starts_with('!');
    let arg = text[name.len() + usize::from(bang)..].trim();
    let full = resolve(&name).ok_or_else(|| format!("E492: Not an editor command: {text}"))?;
    if let Some((modes, noremap)) = map::modes_for(full) { return map_cmd(vim, &modes, noremap, bang, arg); }
    match full {
        "quit" | "qall" | "quitall" => host.act(Act::QuitAll { force: bang }),
        "edit" | "tabedit" | "tabnew" => host.act(Act::Edit { path: (!arg.is_empty()).then(|| arg.to_string()), force: bang }),
        "enew" => host.act(Act::NewBuffer),
        "set" | "setlocal" | "setglobal" => set(vim, host, arg),
        "let" => let_cmd(vim, host, arg),
        "vtcode" | "VT" => host.act(Act::Command(arg.into())),
        "copen" | "cwindow" | "clist" => host.act(Act::Quickfix(super::Quickfix::Open)),
        "cclose" => host.act(Act::Quickfix(super::Quickfix::Close)),
        "cnext" => host.act(Act::Quickfix(super::Quickfix::Next)),
        "cprevious" | "cNext" => host.act(Act::Quickfix(super::Quickfix::Prev)),
        "Explore" | "Sexplore" | "Vexplore" | "Lexplore" => host.act(Act::Explore),
        "terminal" => host.act(Act::Terminal((!arg.is_empty()).then(|| arg.to_string()))),
        "wall" => host.act(Act::WriteAll),
        _ => Err("E32: No file name".into()),
    }
}

fn read_only(host: &dyn Host) -> bool { host.ed_ref().is_some_and(|e| e.read_only) }
fn read_only_ed(ed: &crate::editor::Editor) -> bool { ed.read_only }

/// `[x] [count]` arguments.
fn reg_count(arg: &str) -> (Option<char>, Option<usize>) {
    let mut reg = None;
    let mut rest = arg.trim();
    if let Some(c) = rest.chars().next()
        && !c.is_ascii_digit()
        && super::register::Registers::valid(c)
    {
        reg = Some(c);
        rest = rest[c.len_utf8()..].trim();
    }
    (reg, rest.parse().ok())
}

fn with_count(host: &dyn Host, range: Range, count: Option<usize>) -> Range {
    let last = host.ed_ref().map_or(0, |e| Text::of(&e.rope, e.vim_buf.trailing_empty).last_line());
    match count {
        Some(n) if n > 0 => Range { start: range.end, end: (range.end + n - 1).min(last), given: 2 },
        _ => range,
    }
}

fn range_text(host: &dyn Host, range: &Range) -> String {
    let Some(ed) = host.ed_ref() else { return String::new(); };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let mut s = String::new();
    for l in range.start..=range.end.min(t.last_line()) {
        s.push_str(&t.line_string(l));
        s.push('\n');
    }
    s
}

fn show_output(vim: &mut VimState, out: &str) {
    let lines: Vec<String> = out.lines().map(str::to_string).collect();
    match lines.len() {
        0 => vim.set_message(""),
        1 => vim.set_message(lines[0].clone()),
        _ => vim.set_lines(lines),
    }
}

fn expand_path(host: &dyn Host, p: &str) -> std::path::PathBuf {
    let p = p.trim();
    let p = if p == "%" { host.file_name().unwrap_or_default() } else { p.to_string() };
    let expanded = if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")).map(|h| std::path::PathBuf::from(h).join(rest)).unwrap_or_else(|| p.clone().into())
    } else {
        std::path::PathBuf::from(&p)
    };
    if expanded.is_relative() {
        if let Some(cwd) = host.cwd() { return cwd.join(expanded); }
    }
    expanded
}

/// Run a shell command, optionally feeding stdin.
fn shell(host: &dyn Host, cmd: &str, input: Option<&str>) -> R<String> {
    if cmd.is_empty() { return Err("E471: Argument required".into()); }
    let mut command = if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", cmd]);
        c
    } else {
        let sh = std::env::var("SHELL").unwrap_or_else(|_| "sh".into());
        let mut c = std::process::Command::new(sh);
        c.args(["-c", cmd]);
        c
    };
    if let Some(dir) = host.cwd() { command.current_dir(dir); }
    command.stdin(if input.is_some() { std::process::Stdio::piped() } else { std::process::Stdio::null() });
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let mut child = command.spawn().map_err(|e| format!("E282: Cannot execute shell: {e}"))?;
    if let Some(text) = input {
        use std::io::Write;
        if let Some(mut stdin) = child.stdin.take() {
            let text = text.to_string();
            std::thread::spawn(move || { let _ = stdin.write_all(text.as_bytes()); });
        }
    }
    let out = child.wait_with_output().map_err(|e| format!("E282: {e}"))?;
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() && s.is_empty() {
        s = String::from_utf8_lossy(&out.stderr).to_string();
        if s.trim().is_empty() { s = format!("shell returned {}", out.status.code().unwrap_or(-1)); }
    }
    Ok(s)
}

fn filter(vim: &mut VimState, host: &mut dyn Host, range: &Range, cmd: &str) -> R<()> {
    if read_only(host) { return Err("E21: Cannot make changes, 'modifiable' is off".into()); }
    let input = range_text(host, range);
    let out = exec::plain(&shell(host, cmd, Some(&input))?);
    let Some(ed) = host.ed() else { return Ok(()); };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let s = t.line_start(range.start);
    let e = t.line_end(range.end);
    let mut out = out;
    if out.ends_with('\n') { out.pop(); }
    let mut tx = Tx::default();
    tx.edit(ed, s, e, &out);
    tx.finish(ed);
    let n = range.end - range.start + 1;
    set_cursor(ed, Pos::new(range.start, Text::of(&ed.rope, ed.vim_buf.trailing_empty).first_nonblank(range.start)));
    if n > vim.opts.report { vim.set_message(format!("{n} lines filtered")); }
    Ok(())
}

fn move_copy(vim: &mut VimState, host: &mut dyn Host, range: &Range, dest: isize, is_move: bool) -> R<()> {
    if read_only(host) { return Err("E21".into()); }
    let Some(ed) = host.ed() else { return Ok(()); };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    if dest > t.last_line() as isize { return Err("E16: Invalid range".into()); }
    if is_move && dest >= range.start as isize && dest < range.end as isize {
        return Err("E134: Cannot move a range of lines into itself".into());
    }
    if is_move && (dest == range.start as isize - 1 || dest == range.end as isize) {
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        let line = range.end.min(t.last_line());
        set_cursor(ed, Pos::new(line, t.first_nonblank(line)));
        return Ok(());
    }
    let mut text = String::new();
    for l in range.start..=range.end {
        text.push_str(&t.line_string(l));
        text.push('\n');
    }
    let n = range.end - range.start + 1;
    let mut tx = Tx::default();
    let ends_eol = ed.rope.len_chars() > 0 && matches!(ed.rope.char(ed.rope.len_chars() - 1), '\n' | '\r');
    let insert_at = |ed: &crate::editor::Editor, after: isize| -> (usize, bool) {
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        if after < 0 { return (0, false); }
        let line = after as usize;
        if line >= t.last_line() && !ends_eol { (ed.rope.len_chars(), true) } else { (t.line_next_start(line), false) }
    };
    let new_last;
    if is_move {
        if dest < range.start as isize {
            // Delete first (below), then insert above.
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let (s, e) = line_span_for_delete(&t, range.start, range.end);
            tx.edit(ed, s, e, "");
            let (at, prepend) = insert_at(ed, dest);
            let body = if prepend { format!("\n{}", text.trim_end_matches('\n')) } else { text.clone() };
            tx.edit(ed, at, at, &body);
            new_last = (dest + n as isize) as usize;
        } else {
            let (at, prepend) = insert_at(ed, dest);
            let body = if prepend { format!("\n{}", text.trim_end_matches('\n')) } else { text.clone() };
            tx.edit(ed, at, at, &body);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let (s, e) = line_span_for_delete(&t, range.start, range.end);
            tx.edit(ed, s, e, "");
            new_last = dest as usize;
        }
    } else {
        let (at, prepend) = insert_at(ed, dest);
        let body = if prepend { format!("\n{}", text.trim_end_matches('\n')) } else { text.clone() };
        tx.edit(ed, at, at, &body);
        new_last = (dest + n as isize) as usize;
    }
    tx.finish(ed);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let line = new_last.min(t.last_line());
    set_cursor(ed, Pos::new(line, t.first_nonblank(line)));
    update_curswant(vim, ed);
    if n > vim.opts.report { vim.set_message(format!("{n} lines {}", if is_move { "moved" } else { "copied" })); }
    Ok(())
}

fn line_span_for_delete(t: &Text<'_>, s: usize, e: usize) -> (usize, usize) {
    let rope_len = t.rope.len_chars();
    let end = t.line_next_start(e);
    let ends_with_eol = rope_len > 0 && matches!(t.rope.char(rope_len - 1), '\n' | '\r');
    if end >= rope_len && !ends_with_eol && s > 0 {
        return (t.line_end(s - 1), rope_len);
    }
    (t.line_start(s), end)
}

// ---------------------------------------------------------------------------
// :substitute

fn parse_sub_args(arg: &str) -> Option<(String, Option<String>, String, Option<usize>)> {
    let arg = arg.trim_start();
    let delim = arg.chars().next()?;
    if delim.is_alphanumeric() || delim == '\\' || delim == '"' || delim == '|' || delim == ' ' {
        return None;
    }
    let rest = &arg[delim.len_utf8()..];
    let (pat, after) = super::cmdline::split_pattern(rest, delim);
    let Some(after) = after else { return Some((pat, None, String::new(), None)); };
    // Replacement ends at the next unescaped delimiter.
    let mut rep = String::new();
    let mut chars = after.chars();
    let mut tail = String::new();
    let mut closed = false;
    while let Some(c) = chars.next() {
        if c == '\\' {
            rep.push(c);
            if let Some(n) = chars.next() { rep.push(n); }
            continue;
        }
        if c == delim { closed = true; tail = chars.collect(); break; }
        rep.push(c);
    }
    let _ = closed;
    let tail = tail.trim();
    let flags: String = tail.chars().take_while(|c| "&cegiInp#lr".contains(*c)).collect();
    let count = tail[flags.len()..].trim().parse().ok();
    Some((pat, Some(rep), flags, count))
}

fn substitute(vim: &mut VimState, host: &mut dyn Host, range: &Range, cmd: &str, arg: &str) -> R<()> {
    if read_only(host) { return Err("E21: Cannot make changes, 'modifiable' is off".into()); }
    let magic = cmd != "snomagic" && vim.opts.magic;
    let last = vim.last_sub.clone();
    let (pattern, replacement, mut flags, count) = match parse_sub_args(arg) {
        Some((pat, rep, flags, count)) if cmd == "substitute" || cmd == "smagic" || cmd == "snomagic" => {
            let pat = if pat.is_empty() {
                vim.last_search.as_ref().map(|s| s.0.clone()).or_else(|| last.as_ref().map(|s| s.pattern.clone())).ok_or("E35: No previous regular expression")?
            } else { pat };
            let rep = match rep {
                Some(r) => search::resolve_tilde(&r, last.as_ref().map_or("", |s| s.replacement.as_str()), magic),
                None => String::new(),
            };
            (pat, rep, flags, count)
        }
        _ => {
            // :s [flags] [count], :&, :&&, :~
            let l = last.clone().ok_or("E35: No previous regular expression")?;
            let tail = arg.trim();
            let mut flags: String = tail.chars().take_while(|c| "&cegiInp#lr".contains(*c)).collect();
            let count = tail[flags.len()..].trim().parse().ok();
            if cmd == "&&" || flags.starts_with('&') { flags = format!("{}{}", l.flags, flags.trim_start_matches('&')); }
            let pat = if cmd == "~" || flags.contains('r') {
                vim.last_search.as_ref().map(|s| s.0.clone()).unwrap_or(l.pattern.clone())
            } else { l.pattern.clone() };
            (pat, l.replacement.clone(), flags, count)
        }
    };
    if flags.starts_with('&') {
        if let Some(l) = &last { flags = format!("{}{}", l.flags, &flags[1..]); }
    }
    vim.last_sub = Some(Substitute { pattern: pattern.clone(), replacement: replacement.clone(), flags: flags.replace('&', "") });
    vim.last_search = Some((pattern.clone(), true));
    vim.regs.last_search = pattern.clone();
    vim.hl_hidden = false;
    let mut global = flags.contains('g');
    if vim.opts.gdefault { global = !global; }
    let mut opts = vim.search_options();
    opts.magic = magic;
    if flags.contains('i') { opts.ignorecase = true; opts.smartcase = false; }
    if flags.contains('I') { opts.ignorecase = false; }
    let re = search::compile(&pattern, &opts)?;
    let (start_line, end_line) = match count {
        Some(n) => (range.end, range.end + n.max(1) - 1),
        None => (range.start, range.end),
    };
    let Some(ed) = host.ed() else { return Ok(()); };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let end_line = end_line.min(t.last_line());
    let block_start = t.line_start(start_line);
    let block_end = t.line_next_start(end_line);
    let hay = exec::haystack(ed);
    let bs = ed.rope.char_to_byte(block_start);
    let be = ed.rope.char_to_byte(block_end);
    // Matches starting on lines in range: first per line unless 'g'.
    let mut found: Vec<(usize, usize, String)> = Vec::new();
    let mut last_line_hit: Option<usize> = None;
    let mut lines = std::collections::BTreeSet::new();
    let at_end = be >= hay.len();
    let in_block = |s: usize| s >= bs && (s < be || (at_end && s == be));
    for caps in re.re.captures_iter(&hay).filter(|c| c.get(0).is_some_and(|m| in_block(m.start()))) {
        let whole = caps.get(0).unwrap();
        let m = if re.zs { caps.name("vzs").unwrap_or(whole) } else { whole };
        if !in_block(m.start()) { continue; }
        let line = ed.rope.char_to_line(ed.rope.byte_to_char(m.start()));
        if line > end_line { break; }
        if !global && last_line_hit == Some(line) { continue; }
        // Empty match right after a previous match on the same spot.
        last_line_hit = Some(line);
        lines.insert(line);
        let rep = search::expand_replacement(&replacement, &caps, &re, magic);
        found.push((ed.rope.byte_to_char(m.start()), ed.rope.byte_to_char(m.end()), rep));
    }
    if found.is_empty() {
        if flags.contains('e') { return Ok(()); }
        return Err(format!("E486: Pattern not found: {pattern}"));
    }
    if flags.contains('n') {
        vim.set_message(format!("{} match{} on {} line{}", found.len(), if found.len() == 1 { "" } else { "es" }, lines.len(), if lines.len() == 1 { "" } else { "s" }));
        return Ok(());
    }
    if flags.contains('c') {
        let boundary = super::insert::undo_back_id(ed);
        vim.confirm = Some(Confirm {
            matches: found.iter().map(|(s, e, _)| (*s, *e)).collect(),
            pattern, replacement, boundary, done: 0, lines: Default::default(), all: false,
        });
        confirm_show(vim, host);
        return Ok(());
    }
    if let Some((s, _, _)) = found.first() {
        ed.cursor_line = ed.rope.char_to_line(*s);
        ed.cursor_col = 0;
    }
    let mut tx = Tx::default();
    // The cursor goes to the line of the last substitution, counting lines
    // added or removed by the earlier ones.
    let mut last_line = start_line;
    if let Some((s, _, _)) = found.last() {
        let mut delta = 0isize;
        for (ms, me, rep) in &found[..found.len() - 1] {
            let removed = ed.rope.slice(*ms..*me).chars().filter(|&c| c == '\n').count() as isize;
            delta += rep.matches('\n').count() as isize - removed;
        }
        last_line = (ed.rope.char_to_line(*s) as isize + delta).max(0) as usize;
    }
    for (s, e, rep) in found.iter().rev() {
        tx.edit(ed, *s, *e, rep);
    }
    tx.finish(ed);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let line = last_line.min(t.last_line());
    let target = Pos::new(line, t.first_nonblank(line));
    push_jump(vim, ed);
    set_cursor(ed, target);
    update_curswant(vim, ed);
    let n = found.len();
    if n > vim.opts.report || lines.len() > vim.opts.report {
        vim.set_message(format!("{n} substitution{} on {} line{}", if n == 1 { "" } else { "s" }, lines.len(), if lines.len() == 1 { "" } else { "s" }));
    }
    Ok(())
}

fn confirm_show(vim: &mut VimState, host: &mut dyn Host) {
    let Some(c) = &vim.confirm else { return; };
    let Some(&(s, _)) = c.matches.first() else {
        confirm_end(vim, host);
        return;
    };
    let rep = c.replacement.clone();
    if let Some(ed) = host.ed() {
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        set_cursor(ed, t.pos(s));
    }
    vim.set_message(format!("replace with {rep} (y/n/a/q/l/^E/^Y)?"));
}

fn confirm_end(vim: &mut VimState, host: &mut dyn Host) {
    let Some(c) = vim.confirm.take() else { return; };
    if let Some(ed) = host.ed() {
        // One undo step for the whole :s///c.
        let mut deque = ed.undo_stack.write();
        let idx = c.boundary.and_then(|id| deque.iter().rposition(|u| u.snapshot_id == id));
        let keep = idx.map_or(1, |i| i + 2);
        if deque.len() > keep { deque.truncate(keep); }
        drop(deque);
        exec::clamp_normal(ed);
    }
    if c.done > 0 {
        vim.set_message(format!("{} substitution{} on {} line{}", c.done, if c.done == 1 { "" } else { "s" }, c.lines.len(), if c.lines.len() == 1 { "" } else { "s" }));
    } else {
        vim.message = None;
    }
}

pub(crate) fn confirm_key(vim: &mut VimState, host: &mut dyn Host, key: Key) {
    let Some(c) = vim.confirm.as_mut() else { return; };
    if key.is_ctrl('e') || key.is_ctrl('y') {
        if let Some(ed) = host.ed() {
            if key.is_ctrl('e') { ed.scroll_offset += 1; } else { ed.scroll_offset = ed.scroll_offset.saturating_sub(1); }
        }
        return;
    }
    let choice = if key.is_escape() { 'q' } else { key.char().unwrap_or('?') };
    let replace_one = |vim: &mut VimState, host: &mut dyn Host| -> R<()> {
        let c = vim.confirm.as_mut().ok_or("")?;
        let (s, e) = c.matches.remove(0);
        let pattern = c.pattern.clone();
        let replacement = c.replacement.clone();
        let opts = vim.search_options();
        let re = search::compile(&pattern, &opts)?;
        let Some(ed) = host.ed() else { return Ok(()); };
        let matched = ed.rope.slice(s..e).to_string();
        let caps = re.re.captures(&matched);
        let rep = match caps { Some(caps) => search::expand_replacement(&replacement, &caps, &re, true), None => replacement };
        let mut tx = Tx::default();
        tx.edit(ed, s, e, &rep);
        tx.finish(ed);
        let line = ed.rope.char_to_line(s);
        let delta = rep.chars().count() as isize - (e - s) as isize;
        let c = vim.confirm.as_mut().unwrap();
        for m in &mut c.matches {
            m.0 = (m.0 as isize + delta) as usize;
            m.1 = (m.1 as isize + delta) as usize;
        }
        c.done += 1;
        c.lines.insert(line);
        Ok(())
    };
    match choice {
        'y' => { let _ = replace_one(vim, host); }
        'l' => { let _ = replace_one(vim, host); if let Some(c) = vim.confirm.as_mut() { c.matches.clear(); } }
        'n' => { c.matches.remove(0); }
        'a' => {
            c.all = true;
            while vim.confirm.as_ref().is_some_and(|c| !c.matches.is_empty()) {
                if replace_one(vim, host).is_err() { break; }
            }
        }
        'q' => { c.matches.clear(); }
        _ => return,
    }
    confirm_show(vim, host);
}

// ---------------------------------------------------------------------------
// :global and :normal

fn global(vim: &mut VimState, host: &mut dyn Host, range: &Range, invert: bool, arg: &str) -> R<()> {
    let arg = arg.trim_start();
    let delim = arg.chars().next().ok_or("E476: Invalid command")?;
    let rest = &arg[delim.len_utf8()..];
    let (pat, after) = super::cmdline::split_pattern(rest, delim);
    let cmd = after.unwrap_or_default();
    let cmd = if cmd.trim().is_empty() { "p".to_string() } else { cmd };
    let pat = if pat.is_empty() { vim.last_search.as_ref().map(|s| s.0.clone()).ok_or("E35: No previous regular expression")? } else { pat };
    vim.last_search = Some((pat.clone(), true));
    vim.regs.last_search = pat.clone();
    let re = search::compile(&pat, &vim.search_options())?;
    let Some(ed) = host.ed() else { return Ok(()); };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let (s, e) = if range.given == 0 { (0, t.last_line()) } else { (range.start, range.end) };
    let lines: Vec<usize> = (s..=e).filter(|&l| re.re.is_match(&t.line_string(l)) != invert).collect();
    if lines.is_empty() {
        vim.set_message(format!("Pattern {}found: {pat}", if invert { "found in every line" } else { "not " }));
        return Ok(());
    }
    ed.vim_buf.global_lines = lines;
    let boundary = super::insert::undo_back_id(ed);
    let mut printed = Vec::new();
    loop {
        let Some(ed) = host.ed() else { break; };
        if ed.vim_buf.global_lines.is_empty() { break; }
        let line = ed.vim_buf.global_lines.remove(0);
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        if line > t.last_line() { continue; }
        ed.cursor_line = line;
        ed.cursor_col = 0;
        let before_lines = t.lines();
        let rev = ed.vim_buf.shift_rev;
        if cmd.trim() == "p" || cmd.trim() == "print" {
            printed.push(t.line_string(line));
            continue;
        }
        vim.replaying += 1;
        let r = run(vim, host, &cmd);
        vim.replaying -= 1;
        if let Err(e) = r { vim.set_error(e); break; }
        if let Some(ed) = host.ed() {
            let after = Text::of(&ed.rope, ed.vim_buf.trailing_empty).lines();
            if ed.vim_buf.shift_rev == rev && after != before_lines {
                let delta = after as isize - before_lines as isize;
                for l in &mut ed.vim_buf.global_lines {
                    if *l > line { *l = (*l as isize + delta).max(0) as usize; }
                }
            }
        }
    }
    if let Some(ed) = host.ed() {
        ed.vim_buf.global_lines.clear();
        let mut deque = ed.undo_stack.write();
        let idx = boundary.and_then(|id| deque.iter().rposition(|u| u.snapshot_id == id));
        let keep = idx.map_or(1, |i| i + 2);
        if deque.len() > keep { deque.truncate(keep); }
        drop(deque);
        exec::clamp_normal(ed);
    }
    if !printed.is_empty() { vim.set_lines(printed); }
    Ok(())
}

fn normal(vim: &mut VimState, host: &mut dyn Host, range: &Range, arg: &str, remap: bool) -> R<()> {
    let arg = arg.strip_prefix(' ').unwrap_or(arg);
    if arg.is_empty() { return Err("E471: Argument required".into()); }
    let keys = super::keys::parse(arg, &vim.leader.clone());
    let lines: Vec<usize> = if range.given > 0 { (range.start..=range.end).collect() } else { vec![usize::MAX] };
    let multi = range.given > 0;
    for line in lines {
        if line != usize::MAX {
            let Some(ed) = host.ed() else { break; };
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            if line > t.last_line() { continue; }
            ed.cursor_line = line;
            ed.cursor_col = 0;
        }
        vim.replaying += 1;
        vim.feed_keys(host, &keys, remap, true);
        // An incomplete command is aborted, Insert mode ends.
        vim.pending.clear();
        if vim.is_insert() { super::insert::finish(vim, host, true); }
        if vim.mode == Mode::Cmdline { super::cmdline::cancel(vim, host); }
        vim.replaying -= 1;
        if !multi { break; }
    }
    if vim.is_visual() { exec::exit_visual(vim, host); }
    Ok(())
}

// ---------------------------------------------------------------------------
// :sort, :retab, alignment

fn sort(vim: &mut VimState, host: &mut dyn Host, range: &Range, reverse: bool, arg: &str) -> R<()> {
    if read_only(host) { return Err("E21".into()); }
    let Some(ed) = host.ed() else { return Ok(()); };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let (s, e) = if range.given == 0 { (0, t.last_line()) } else { (range.start, range.end) };
    let mut flags = String::new();
    let mut pat: Option<String> = None;
    let mut chars = arg.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' {
            let rest: String = chars.by_ref().collect();
            let (p, _) = super::cmdline::split_pattern(&rest, '/');
            pat = Some(p);
            break;
        }
        if c != ' ' { flags.push(c); }
    }
    let re = match &pat {
        Some(p) if !p.is_empty() => Some(search::compile(p, &vim.search_options())?),
        Some(_) => vim.last_search.as_ref().and_then(|(p, _)| search::compile(p, &vim.search_options()).ok()),
        None => None,
    };
    let icase = flags.contains('i');
    let numeric = flags.contains('n');
    let float = flags.contains('f');
    let hex = flags.contains('x');
    let unique = flags.contains('u');
    let use_match = flags.contains('r');
    let mut lines: Vec<String> = (s..=e).map(|l| t.line_string(l)).collect();
    let key = |l: &str| -> String {
        match &re {
            Some(re) => {
                let m = re.find_iter(l).next();
                match m {
                    Some((ms, me)) => if use_match { l[ms..me].to_string() } else { l[me..].to_string() },
                    None => if use_match { String::new() } else { l.to_string() },
                }
            }
            None => l.to_string(),
        }
    };
    let num = |k: &str| -> f64 {
        if hex {
            let h: String = k.chars().skip_while(|c| !c.is_ascii_hexdigit()).take_while(|c| c.is_ascii_hexdigit() || *c == 'x').collect();
            i64::from_str_radix(h.trim_start_matches("0x"), 16).map(|v| v as f64).unwrap_or(f64::MIN)
        } else {
            let start = k.find(|c: char| c.is_ascii_digit() || (c == '-' && float)).unwrap_or(k.len());
            let mut digits = String::new();
            let mut neg = false;
            if start > 0 && k[..start].ends_with('-') { neg = true; }
            for c in k[start..].chars() {
                if c.is_ascii_digit() || (float && (c == '.' || c == 'e' || c == '-' && digits.is_empty())) { digits.push(c); } else { break; }
            }
            if digits.is_empty() { return f64::MIN; }
            let v: f64 = digits.parse().unwrap_or(f64::MIN);
            if neg { -v } else { v }
        }
    };
    if numeric || float || hex {
        lines.sort_by(|a, b| num(&key(a)).partial_cmp(&num(&key(b))).unwrap_or(std::cmp::Ordering::Equal));
    } else if icase {
        lines.sort_by_key(|l| key(l).to_lowercase());
    } else {
        lines.sort_by_key(|l| key(l));
    }
    if reverse { lines.reverse(); }
    if unique {
        lines.dedup_by(|a, b| if icase { a.to_lowercase() == b.to_lowercase() } else { a == b });
    }
    let start = t.line_start(s);
    let end = t.line_end(e);
    let new = lines.join("\n");
    let mut tx = Tx::default();
    if t.slice(start, end) != new { tx.edit(ed, start, end, &new); }
    tx.finish(ed);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    set_cursor(ed, Pos::new(s, t.first_nonblank(s)));
    Ok(())
}

fn retab(vim: &mut VimState, host: &mut dyn Host, range: &Range, bang: bool, arg: &str) -> R<()> {
    let Some(ed) = host.ed() else { return Ok(()); };
    if let Ok(n) = arg.parse::<usize>() { if n > 0 { ed.tab_size = n; } }
    let ts = ed.tab_size.max(1);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let (s, e) = if range.given == 0 { (0, t.last_line()) } else { (range.start, range.end) };
    let mut tx = Tx::default();
    for line in (s..=e).rev() {
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        let chars = t.line_chars(line);
        let ws = chars.iter().take_while(|&&c| c == ' ' || c == '\t').count();
        if !(chars[..ws].contains(&'\t') || bang && ws > 0) { continue; }
        let width = t.vcol(line, ws, ts);
        let new = if ed.use_tabs { format!("{}{}", "\t".repeat(width / ts), " ".repeat(width % ts)) } else { " ".repeat(width) };
        let base = t.line_start(line);
        if chars[..ws].iter().collect::<String>() != new { tx.edit(ed, base, base + ws, &new); }
    }
    tx.finish(ed);
    let _ = vim;
    Ok(())
}

fn align(vim: &mut VimState, host: &mut dyn Host, range: &Range, how: &str, arg: &str) -> R<()> {
    let Some(ed) = host.ed() else { return Ok(()); };
    let width: usize = arg.parse().unwrap_or(if vim.opts.textwidth > 0 { vim.opts.textwidth } else { 80 });
    let mut tx = Tx::default();
    for line in range.start..=range.end {
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        let text = t.line_string(line);
        let body = text.trim();
        let new = match how {
            "left" => format!("{}{}", " ".repeat(arg.parse().unwrap_or(0)), body),
            "right" => format!("{}{}", " ".repeat(width.saturating_sub(body.chars().count())), body),
            _ => format!("{}{}", " ".repeat(width.saturating_sub(body.chars().count()) / 2), body),
        };
        let (s, e) = (t.line_start(line), t.line_end(line));
        if text != new { tx.edit(ed, s, e, &new); }
    }
    tx.finish(ed);
    Ok(())
}

// ---------------------------------------------------------------------------
// :set, :let, mappings, vimrc

fn map_cmd(vim: &mut VimState, modes: &[MapMode], noremap: bool, bang: bool, arg: &str) -> R<()> {
    let modes: Vec<MapMode> = if bang { vec![MapMode::Insert, MapMode::Cmdline] } else { modes.to_vec() };
    let (silent, nowait, lhs, rhs) = map::parse_args(arg);
    let leader = vim.leader.clone();
    if lhs.is_empty() {
        let lines = vim.maps.list(&modes, &[]);
        if lines.is_empty() { vim.set_message("No mapping found"); } else { vim.set_lines(lines); }
        return Ok(());
    }
    let lhs_keys = map::map_keys(lhs, &leader);
    if rhs.is_empty() {
        let lines = vim.maps.list(&modes, &lhs_keys);
        if lines.is_empty() { vim.set_message("No mapping found"); } else { vim.set_lines(lines); }
        return Ok(());
    }
    if lhs_keys.is_empty() { return Err("E474: Invalid argument".into()); }
    let rhs_keys = map::map_keys(rhs, &leader);
    vim.maps.define(&modes, lhs_keys, Mapping { rhs: rhs_keys, noremap, nowait, silent, source: rhs.to_string() });
    Ok(())
}

fn let_cmd(vim: &mut VimState, host: &mut dyn Host, arg: &str) -> R<()> {
    let (name, value) = arg.split_once('=').ok_or("E15: Invalid expression")?;
    let name = name.trim();
    let raw = value.trim();
    let value = if (raw.starts_with('"') && raw.ends_with('"') || raw.starts_with('\'') && raw.ends_with('\'')) && raw.len() >= 2 {
        let inner = &raw[1..raw.len() - 1];
        if raw.starts_with('"') { inner.replace("\\\\", "\\").replace("\\<", "<").replace("\\\"", "\"") } else { inner.to_string() }
    } else { raw.to_string() };
    match name {
        "mapleader" | "g:mapleader" | "maplocalleader" | "g:maplocalleader" => {
            let keys = super::keys::parse(&value, &[]);
            if name.contains("local") { return Ok(()); }
            vim.leader = if keys.is_empty() { vec![Key::ch('\\')] } else { keys };
            Ok(())
        }
        n if n.starts_with('@') && n.len() == 2 => {
            let r = n.chars().nth(1).unwrap();
            let mut clip = HostClip(host);
            vim.regs.set(r, Reg::new(value, RegKind::Char), &mut clip);
            Ok(())
        }
        n if n.starts_with('&') => set(vim, host, &format!("{}={}", &n[1..], value)),
        _ => Ok(()),
    }
}

/// Option names and aliases.
const OPTIONS: &[(&str, &str)] = &[
    ("ignorecase", "ic"), ("smartcase", "scs"), ("hlsearch", "hls"), ("incsearch", "is"), ("wrapscan", "ws"), ("magic", "magic"),
    ("gdefault", "gd"), ("timeout", "to"), ("timeoutlen", "tm"), ("ttimeoutlen", "ttm"), ("clipboard", "cb"), ("startofline", "sol"),
    ("joinspaces", "js"), ("tildeop", "top"), ("whichwrap", "ww"), ("iskeyword", "isk"), ("shiftround", "sr"), ("autoindent", "ai"),
    ("textwidth", "tw"), ("nrformats", "nf"), ("showcmd", "sc"), ("showmode", "smd"), ("scrolloff", "so"), ("report", "report"),
    ("splitright", "spr"), ("splitbelow", "sb"), ("number", "nu"), ("relativenumber", "rnu"), ("wrap", "wrap"), ("tabstop", "ts"),
    ("shiftwidth", "sw"), ("softtabstop", "sts"), ("expandtab", "et"), ("list", "list"), ("readonly", "ro"), ("modifiable", "ma"),
    ("fileformat", "ff"), ("filetype", "ft"), ("cursorline", "cul"), ("spell", "spell"), ("mouse", "mouse"), ("encoding", "enc"),
    ("fileencoding", "fenc"), ("syntax", "syn"), ("smartindent", "si"), ("cindent", "cin"), ("hidden", "hid"), ("backup", "bk"),
    ("swapfile", "swf"), ("undofile", "udf"), ("compatible", "cp"), ("ruler", "ru"), ("laststatus", "ls"), ("cmdheight", "ch"),
    ("signcolumn", "scl"), ("colorcolumn", "cc"), ("termguicolors", "tgc"), ("background", "bg"), ("belloff", "bo"),
    ("errorbells", "eb"), ("visualbell", "vb"), ("updatetime", "ut"), ("wildmenu", "wmnu"), ("wildmode", "wim"),
    ("backspace", "bs"), ("history", "hi"), ("lazyredraw", "lz"), ("showmatch", "sm"), ("foldmethod", "fdm"), ("foldenable", "fen"),
    ("sidescroll", "ss"), ("sidescrolloff", "siso"), ("undolevels", "ul"), ("virtualedit", "ve"), ("selection", "sel"),
    ("matchpairs", "mps"), ("wildignorecase", "wic"), ("autoread", "ar"), ("autowrite", "aw"), ("formatoptions", "fo"),
    ("conceallevel", "cole"), ("linebreak", "lbr"), ("breakindent", "bri"), ("cursorcolumn", "cuc"), ("ttyfast", "tf"),
    ("highlightyank", "hly"), ("vimctrlkeys", "vck"),
];

fn option_name(n: &str) -> Option<&'static str> {
    OPTIONS.iter().find(|(full, short)| *full == n || *short == n).map(|(f, _)| *f)
}

fn set(vim: &mut VimState, host: &mut dyn Host, arg: &str) -> R<()> {
    if arg.is_empty() || arg == "all" {
        let o = &vim.opts;
        vim.set_lines(vec![format!(
            "{}hlsearch {}ignorecase {}smartcase {}incsearch {}wrapscan  timeoutlen={}  clipboard={}  scrolloff={}  textwidth={}",
            if o.hlsearch { "" } else { "no" }, if o.ignorecase { "" } else { "no" }, if o.smartcase { "" } else { "no" },
            if o.incsearch { "" } else { "no" }, if o.wrapscan { "" } else { "no" }, o.timeoutlen,
            if o.clipboard_unnamed { "unnamedplus" } else { "" }, o.scrolloff.map_or("-1".into(), |s| s.to_string()), o.textwidth,
        )]);
        return Ok(());
    }
    let mut errors = Vec::new();
    for item in split_set_args(arg) {
        if let Err(e) = set_one(vim, host, &item) { errors.push(e); }
    }
    errors.into_iter().next().map_or(Ok(()), Err)
}

fn split_set_args(arg: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = arg.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' { if let Some(n) = chars.next() { cur.push(n); } continue; }
        if c == ' ' {
            if !cur.is_empty() { out.push(std::mem::take(&mut cur)); }
            continue;
        }
        if c == '"' && cur.is_empty() { break; }
        cur.push(c);
    }
    if !cur.is_empty() { out.push(cur); }
    out
}

fn bool_str(b: bool) -> &'static str { if b { "true" } else { "false" } }

fn set_one(vim: &mut VimState, host: &mut dyn Host, item: &str) -> R<()> {
    // Forms: name, noname, invname, name!, name?, name&, name=val, name+=val, name-=val, name^=val, name:val
    let (raw, op, value) = if let Some(i) = item.find(['=', ':']) {
        let (n, v) = item.split_at(i);
        let (n, op) = if let Some(n) = n.strip_suffix('+') { (n, "+=") } else if let Some(n) = n.strip_suffix('-') { (n, "-=") } else if let Some(n) = n.strip_suffix('^') { (n, "^=") } else { (n, "=") };
        (n.to_string(), op, Some(v[1..].to_string()))
    } else {
        (item.to_string(), "", None)
    };
    let (raw, query, reset, toggle_bang) = {
        let mut r = raw.as_str();
        let q = r.ends_with('?'); if q { r = &r[..r.len() - 1]; }
        let a = r.ends_with('&'); if a { r = &r[..r.len() - 1]; }
        let b = r.ends_with('!'); if b { r = &r[..r.len() - 1]; }
        (r.to_string(), q, a, b)
    };
    let (name, negate, invert) = if let Some(n) = option_name(&raw) {
        (n, false, false)
    } else if let Some(n) = raw.strip_prefix("no").and_then(option_name) {
        (n, true, false)
    } else if let Some(n) = raw.strip_prefix("inv").and_then(option_name) {
        (n, false, true)
    } else {
        return Err(format!("E518: Unknown option: {raw}"));
    };
    let invert = invert || toggle_bang;
    let defaults = super::Options::default();
    macro_rules! flag {
        ($field:ident) => {{
            if query { vim.set_message(format!("{}{}", if vim.opts.$field { "  " } else { "no" }, name)); return Ok(()); }
            vim.opts.$field = if reset { defaults.$field } else if invert { !vim.opts.$field } else { !negate };
            Ok(())
        }};
    }
    macro_rules! number {
        ($field:ident) => {{
            if query || value.is_none() { vim.set_message(format!("  {}={}", name, vim.opts.$field)); return Ok(()); }
            let v = value.as_deref().unwrap_or("").parse().map_err(|_| format!("E521: Number required after =: {item}"))?;
            vim.opts.$field = match op { "+=" => vim.opts.$field + v, "-=" => vim.opts.$field.saturating_sub(v), "^=" => vim.opts.$field * v, _ => v };
            Ok(())
        }};
    }
    match name {
        "ignorecase" => flag!(ignorecase),
        "smartcase" => flag!(smartcase),
        "hlsearch" => { let r = flag!(hlsearch); vim.hl_hidden = false; r }
        "incsearch" => flag!(incsearch),
        "wrapscan" => flag!(wrapscan),
        "magic" => flag!(magic),
        "gdefault" => flag!(gdefault),
        "timeout" => flag!(timeout),
        "startofline" => flag!(startofline),
        "joinspaces" => flag!(joinspaces),
        "tildeop" => flag!(tildeop),
        "shiftround" => flag!(shiftround),
        "autoindent" => flag!(autoindent),
        "showcmd" => flag!(showcmd),
        "showmode" => flag!(showmode),
        "splitright" => flag!(splitright),
        "splitbelow" => flag!(splitbelow),
        "wildignorecase" => flag!(wildignorecase),
        "highlightyank" => flag!(highlight_yank),
        "vimctrlkeys" => flag!(ctrl_keys),
        "timeoutlen" => number!(timeoutlen),
        "ttimeoutlen" => number!(ttimeoutlen),
        "textwidth" => number!(textwidth),
        "report" => number!(report),
        "sidescroll" => number!(sidescroll),
        "undolevels" => number!(undolevels),
        "scrolloff" => {
            if query || value.is_none() { vim.set_message(format!("  scrolloff={}", vim.opts.scrolloff.map_or(-1, |v| v as i64))); return Ok(()); }
            let v: i64 = value.as_deref().unwrap_or("0").parse().map_err(|_| "E521: Number required".to_string())?;
            vim.opts.scrolloff = (v >= 0).then_some(v as usize);
            host.act(Act::SetOption { name: "scrolloff".into(), value: v.max(0).to_string() })
        }
        "clipboard" => {
            if query || value.is_none() { vim.set_message(format!("  clipboard={}", if vim.opts.clipboard_unnamed { "unnamedplus" } else { "" })); return Ok(()); }
            let v = value.unwrap_or_default();
            vim.opts.clipboard_unnamed = match op {
                "-=" => if v.contains("unnamed") { false } else { vim.opts.clipboard_unnamed },
                _ => v.contains("unnamed"),
            };
            Ok(())
        }
        "whichwrap" => {
            if query || value.is_none() { vim.set_message(format!("  whichwrap={}", vim.opts.whichwrap)); return Ok(()); }
            let v = value.unwrap_or_default();
            vim.opts.whichwrap = match op { "+=" => format!("{},{v}", vim.opts.whichwrap), _ => v };
            Ok(())
        }
        "iskeyword" => {
            let v = value.unwrap_or_default();
            let chars = parse_isk(&v);
            match op {
                "+=" => vim.opts.iskeyword.extra.extend(chars),
                "-=" => { vim.opts.iskeyword.extra.retain(|c| !chars.contains(c)); vim.opts.iskeyword.removed.extend(chars); }
                _ => { vim.opts.iskeyword = super::text::Keyword { extra: chars, removed: Vec::new() }; }
            }
            Ok(())
        }
        "nrformats" => {
            let v = value.unwrap_or_default();
            let has = |s: &str| v.split(',').any(|p| p == s);
            match op {
                "+=" => { if has("alpha") { vim.opts.nrformats_alpha = true; } if has("octal") { vim.opts.nrformats_octal = true; } if has("hex") { vim.opts.nrformats_hex = true; } if has("bin") { vim.opts.nrformats_bin = true; } }
                "-=" => { if has("alpha") { vim.opts.nrformats_alpha = false; } if has("octal") { vim.opts.nrformats_octal = false; } if has("hex") { vim.opts.nrformats_hex = false; } if has("bin") { vim.opts.nrformats_bin = false; } }
                _ => { vim.opts.nrformats_alpha = has("alpha"); vim.opts.nrformats_octal = has("octal"); vim.opts.nrformats_hex = has("hex"); vim.opts.nrformats_bin = has("bin"); }
            }
            Ok(())
        }
        "matchpairs" => Ok(()),
        // Editor/workbench options.
        "number" | "relativenumber" | "wrap" | "list" | "expandtab" | "readonly" | "modifiable" | "cursorline" | "spell" | "linebreak" => {
            let v = if query { "?".to_string() } else if reset { "&".into() } else if invert { "!".into() } else { bool_str(!negate).into() };
            host.act(Act::SetOption { name: name.into(), value: v })
        }
        "tabstop" | "shiftwidth" | "softtabstop" | "fileformat" | "filetype" | "fileencoding" | "encoding" | "colorcolumn" | "syntax" => {
            let v = if query || value.is_none() { "?".to_string() } else { value.unwrap_or_default() };
            host.act(Act::SetOption { name: name.into(), value: v })
        }
        // Accepted for vimrc compatibility; nothing to configure.
        _ => Ok(()),
    }
}

fn parse_isk(v: &str) -> Vec<char> {
    let mut out = Vec::new();
    for part in v.split(',') {
        if part.is_empty() { out.push(','); continue; }
        if let Some((a, b)) = part.split_once('-').filter(|(a, b)| !a.is_empty() && !b.is_empty()) {
            let (a, b) = (a.parse::<u32>().ok().and_then(char::from_u32).or(a.chars().next()), b.parse::<u32>().ok().and_then(char::from_u32).or(b.chars().next()));
            if let (Some(a), Some(b)) = (a, b) { for c in a..=b { out.push(c); } }
        } else if part == "@" {
        } else if let Ok(n) = part.parse::<u32>() {
            if let Some(c) = char::from_u32(n) { out.push(c); }
        } else {
            out.extend(part.chars().take(1));
        }
    }
    out
}

/// Source vimrc text. Returns error lines.
pub(crate) fn source(vim: &mut VimState, host: &mut dyn Host, text: &str) -> Vec<String> {
    let mut errors = Vec::new();
    let mut lines = text.lines().enumerate().peekable();
    let mut skip_depth = 0usize;
    while let Some((n, line)) = lines.next() {
        let mut line = line.trim().to_string();
        // Line continuation.
        while let Some((_, next)) = lines.peek() {
            let next = next.trim_start();
            if let Some(rest) = next.strip_prefix('\\') { line.push_str(rest); lines.next(); } else { break; }
        }
        if line.is_empty() || line.starts_with('"') { continue; }
        let word: String = line.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        // Skip if/function/augroup blocks we cannot evaluate.
        if matches!(word.as_str(), "if" | "function" | "fun" | "func" | "for" | "while" | "try") { skip_depth += 1; continue; }
        if skip_depth > 0 {
            if matches!(word.as_str(), "endif" | "endfunction" | "endfun" | "endfunc" | "endfor" | "endwhile" | "endtry") { skip_depth -= 1; }
            continue;
        }
        if matches!(word.as_str(), "else" | "elseif" | "endif") { continue; }
        if let Err(e) = run(vim, host, &line) {
            errors.push(format!("line {:>3}: {e}", n + 1));
        }
    }
    vim.message = None;
    errors
}

// ---------------------------------------------------------------------------
// Completion

pub(crate) fn completions(vim: &VimState, host: &mut dyn Host, before: &str) -> (usize, Vec<String>) {
    let chars: Vec<char> = before.chars().collect();
    // Skip a leading range.
    let mut i = 0;
    while i < chars.len() && (chars[i] == ' ' || chars[i] == ':') { i += 1; }
    while i < chars.len() && (chars[i].is_ascii_digit() || ",.;$%'<>+-".contains(chars[i])) { i += 1; }
    let name_start = i;
    while i < chars.len() && chars[i].is_ascii_alphabetic() { i += 1; }
    let name: String = chars[name_start..i].iter().collect();
    if i >= chars.len() {
        let mut items: Vec<String> = COMMANDS.iter().map(|(n, _)| n.to_string()).filter(|n| n.starts_with(&name)).collect();
        items.sort();
        items.dedup();
        return (name_start, items);
    }
    if chars.get(i) == Some(&'!') { i += 1; }
    while i < chars.len() && chars[i] == ' ' { i += 1; }
    let full = resolve(&name).unwrap_or("");
    let arg_start = i;
    let arg: String = chars[arg_start..].iter().collect();
    match full {
        "set" | "setlocal" | "setglobal" => {
            let word_start = arg.rfind(' ').map_or(arg_start, |p| arg_start + arg[..p + 1].chars().count());
            let word = &arg[arg.rfind(' ').map_or(0, |p| p + 1)..];
            let (prefix, w) = if let Some(w) = word.strip_prefix("no") { ("no", w) } else if let Some(w) = word.strip_prefix("inv") { ("inv", w) } else { ("", word) };
            let mut items: Vec<String> = OPTIONS.iter().map(|(n, _)| format!("{prefix}{n}")).filter(|n| n[prefix.len()..].starts_with(w)).collect();
            items.sort();
            (word_start, items)
        }
        "buffer" | "bdelete" => {
            let items = host.buffers().into_iter().map(|(_, n, _)| n).filter(|n| n.to_lowercase().contains(&arg.to_lowercase())).collect();
            (arg_start, items)
        }
        "vtcode" | "VT" => {
            let mut items: Vec<String> = host.commands().into_iter().filter(|c| c.starts_with(&arg)).collect();
            items.sort();
            (arg_start, items)
        }
        "edit" | "write" | "saveas" | "split" | "vsplit" | "new" | "vnew" | "tabedit" | "tabnew" | "read" | "source" | "cd" | "lcd" | "tcd" => {
            let base = host.cwd().unwrap_or_default();
            let (dir_part, file_part) = match arg.rfind(['/', '\\']) { Some(p) => (&arg[..p + 1], &arg[p + 1..]), None => ("", arg.as_str()) };
            let dir = if std::path::Path::new(dir_part).is_absolute() { std::path::PathBuf::from(dir_part) } else { base.join(dir_part) };
            let mut items: Vec<String> = std::fs::read_dir(&dir).map(|rd| rd.filter_map(|e| e.ok()).filter_map(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                let ok = if vim.opts.wildignorecase || cfg!(windows) { n.to_lowercase().starts_with(&file_part.to_lowercase()) } else { n.starts_with(file_part) };
                if !ok || (n.starts_with('.') && !file_part.starts_with('.')) { return None; }
                let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
                if full == "cd" && !is_dir { return None; }
                Some(format!("{dir_part}{n}{}", if is_dir { "/" } else { "" }))
            }).collect()).unwrap_or_default();
            items.sort();
            (arg_start, items)
        }
        _ => (arg_start, Vec::new()),
    }
}

// ---------------------------------------------------------------------------
// Digraphs (a common subset of RFC 1345, as in Vim's `:digraphs`).

pub(crate) fn digraph(a: char, b: char) -> Option<char> {
    const TABLE: &[(&str, char)] = &[
        ("a:", 'ä'), ("o:", 'ö'), ("u:", 'ü'), ("A:", 'Ä'), ("O:", 'Ö'), ("U:", 'Ü'), ("ss", 'ß'), ("e:", 'ë'), ("i:", 'ï'),
        ("a'", 'á'), ("e'", 'é'), ("i'", 'í'), ("o'", 'ó'), ("u'", 'ú'), ("y'", 'ý'), ("A'", 'Á'), ("E'", 'É'), ("I'", 'Í'), ("O'", 'Ó'), ("U'", 'Ú'),
        ("a!", 'à'), ("e!", 'è'), ("i!", 'ì'), ("o!", 'ò'), ("u!", 'ù'), ("A!", 'À'), ("E!", 'È'), ("I!", 'Ì'), ("O!", 'Ò'), ("U!", 'Ù'),
        ("a>", 'â'), ("e>", 'ê'), ("i>", 'î'), ("o>", 'ô'), ("u>", 'û'), ("A>", 'Â'), ("E>", 'Ê'), ("I>", 'Î'), ("O>", 'Ô'), ("U>", 'Û'),
        ("a?", 'ã'), ("o?", 'õ'), ("n?", 'ñ'), ("A?", 'Ã'), ("O?", 'Õ'), ("N?", 'Ñ'), ("c,", 'ç'), ("C,", 'Ç'), ("aa", 'å'), ("AA", 'Å'),
        ("ae", 'æ'), ("AE", 'Æ'), ("o/", 'ø'), ("O/", 'Ø'), ("s<", 'š'), ("S<", 'Š'), ("z<", 'ž'), ("Z<", 'Ž'), ("c<", 'č'), ("C<", 'Č'),
        ("a*", 'α'), ("b*", 'β'), ("g*", 'γ'), ("d*", 'δ'), ("e*", 'ε'), ("z*", 'ζ'), ("y*", 'η'), ("h*", 'θ'), ("i*", 'ι'), ("k*", 'κ'),
        ("l*", 'λ'), ("m*", 'μ'), ("n*", 'ν'), ("c*", 'ξ'), ("o*", 'ο'), ("p*", 'π'), ("r*", 'ρ'), ("s*", 'σ'), ("t*", 'τ'), ("u*", 'υ'),
        ("f*", 'φ'), ("x*", 'χ'), ("q*", 'ψ'), ("w*", 'ω'), ("D*", 'Δ'), ("G*", 'Γ'), ("L*", 'Λ'), ("P*", 'Π'), ("S*", 'Σ'), ("W*", 'Ω'),
        ("->", '→'), ("<-", '←'), ("-!", '↑'), ("-v", '↓'), ("=>", '⇒'), ("==", '⇔'), ("!=", '≠'), ("=<", '≤'), (">=", '≥'), ("?=", '≅'),
        ("+-", '±'), ("*X", '×'), ("-:", '÷'), ("OK", '✓'), ("XX", '✗'), ("Co", '©'), ("Rg", '®'), ("TM", '™'), ("Eu", '€'), ("Pd", '£'),
        ("Ye", '¥'), ("Ct", '¢'), ("SE", '§'), ("PI", '¶'), ("DG", '°'), ("My", 'µ'), ("<<", '«'), (">>", '»'), ("..", '…'), ("-N", '–'),
        ("-M", '—'), ("'6", '‘'), ("'9", '’'), ("\"6", '“'), ("\"9", '”'), ("1S", '¹'), ("2S", '²'), ("3S", '³'), ("12", '½'), ("14", '¼'),
        ("34", '¾'), ("00", '∞'), ("RT", '√'), ("FA", '∀'), ("TE", '∃'), ("(-", '∈'), ("AN", '∧'), ("OR", '∨'), ("NO", '¬'), ("Sb", '∙'),
        ("NS", '\u{a0}'), ("!I", '¡'), ("?I", '¿'),
    ];
    let key: String = [a, b].iter().collect();
    let rev: String = [b, a].iter().collect();
    TABLE.iter().find(|(k, _)| *k == key).or_else(|| TABLE.iter().find(|(k, _)| *k == rev)).map(|(_, c)| *c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_arguments_parse_delimiters_flags_and_counts() {
        assert_eq!(parse_sub_args("/a/b/g"), Some(("a".into(), Some("b".into()), "g".into(), None)));
        assert_eq!(parse_sub_args("#a/b#c\\#d#gi 3"), Some(("a/b".into(), Some("c\\#d".into()), "gi".into(), Some(3))));
        assert_eq!(parse_sub_args("/x"), Some(("x".into(), None, String::new(), None)));
        assert_eq!(parse_sub_args("/x/"), Some(("x".into(), Some(String::new()), String::new(), None)));
        assert!(parse_sub_args("abc").is_none());
    }

    #[test]
    fn command_abbreviations_resolve_like_vim() {
        assert_eq!(resolve("w"), Some("write"));
        assert_eq!(resolve("wq"), Some("wq"));
        assert_eq!(resolve("q"), Some("quit"));
        assert_eq!(resolve("s"), Some("substitute"));
        assert_eq!(resolve("norm"), Some("normal"));
        assert_eq!(resolve("nor"), Some("noremap"));
        assert_eq!(resolve("sp"), Some("split"));
        assert_eq!(resolve("vs"), Some("vsplit"));
        assert_eq!(resolve("noh"), Some("nohlsearch"));
        assert_eq!(resolve("tabn"), Some("tabnext"));
        assert_eq!(resolve("bd"), Some("bdelete"));
        assert_eq!(resolve("g"), Some("global"));
        assert_eq!(resolve("sor"), Some("sort"));
        assert_eq!(resolve("e"), Some("edit"));
        assert_eq!(resolve("x"), Some("xit"));
        assert_eq!(resolve("nnoremap"), Some("nnoremap"));
        assert_eq!(resolve("ino"), Some("inoremap"));
    }

    #[test]
    fn digraphs_and_isk() {
        assert_eq!(digraph('a', ':'), Some('ä'));
        assert_eq!(digraph(':', 'a'), Some('ä'));
        assert_eq!(digraph('-', '>'), Some('→'));
        assert_eq!(parse_isk("45,@-@"), vec!['-', '@']);
        assert_eq!(parse_isk("-"), vec!['-']);
    }
}
