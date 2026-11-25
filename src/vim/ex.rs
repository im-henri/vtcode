//! Ex commands: ranges, `:substitute`, `:global`, `:normal`, line
//! commands, options, mappings and workbench commands.

use super::exec::{self, Tx, cursor, push_jump, set_cursor, update_curswant};
use super::keys::Key;
use super::map::{self, MapMode, Mapping};
use super::paths::Matches;
use super::register::{Reg, RegKind};
use super::search;
use super::text::{Pos, Text};
use super::{Act, Host, HostClip, Mode, OpenTarget, VimState};
use crate::app::TabDropZone;

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
    let parts = split_bar(line);
    if parts.len() > 1 && starts_block(&parts[0]) {
        // `:if x | ... | endif` and friends on one line.
        let lines: Vec<super::script::Line> = parts
            .iter()
            .map(|p| super::script::Line {
                n: 1,
                text: p.trim().to_string(),
            })
            .collect();
        let mut errors = Vec::new();
        super::script::run(vim, host, &lines, &mut errors, true);
        return match errors.into_iter().next() {
            Some((_, e)) => Err(e),
            None => Ok(()),
        };
    }
    for part in parts {
        run_one(vim, host, &part)?;
        if vim.ex_abort {
            vim.ex_abort = false;
            break;
        }
    }
    Ok(())
}

/// Split at `|` unless the command takes `|` as an argument.
fn split_bar(line: &str) -> Vec<String> {
    let trimmed = line.trim_start_matches([' ', ':']);
    let name: String = trimmed
        .chars()
        .skip_while(|c| c.is_ascii_digit() || ",.;$%'+-".contains(*c))
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    if [
        "g", "gl", "glo", "glob", "globa", "global", "v", "vg", "vgl", "vglobal", "norm", "norma",
        "normal", "map", "nmap", "vmap", "xmap", "imap", "omap", "cmap", "noremap", "nnoremap",
        "vnoremap", "xnoremap", "inoremap", "onoremap", "cnoremap", "nno", "vno", "xno", "ino",
        "ono", "cno", "no", "nm", "vm", "xm", "im", "om", "cm",
    ]
    .contains(&name.as_str())
        || trimmed.contains('!') && name.is_empty()
    {
        return vec![line.to_string()];
    }
    // `:s/pat/rep/`: the pattern and replacement are taken as they are (a
    // `|` there is part of them, and `\|` is alternation), as Vim's do_sub
    // skips them before looking for a `|`.
    let is_sub = !name.is_empty() && "substitute".starts_with(name.as_str());
    let mut skip_to = 0;
    if is_sub {
        let at = line.len() - trimmed.len() + trimmed.find(name.as_str()).unwrap_or(0) + name.len();
        if let Some(delim) = line[at..]
            .chars()
            .next()
            .filter(|d| !d.is_alphanumeric() && !matches!(d, ' ' | '"' | '|' | '\\'))
        {
            let start = at + delim.len_utf8();
            // Without a closing delimiter the rest of the line is the argument.
            skip_to = line.len();
            let mut seen = 0;
            let mut it = line[start..].char_indices();
            while let Some((off, ch)) = it.next() {
                if ch == '\\' {
                    it.next();
                    continue;
                }
                if ch == delim {
                    seen += 1;
                    if seen == 2 {
                        skip_to = start + off + ch.len_utf8();
                        break;
                    }
                }
            }
        }
    }
    let mut cur = line[..skip_to].to_string();
    let mut chars = line[skip_to..].char_indices().peekable();
    while let Some((off, c)) = chars.next() {
        if c == '\\' && chars.peek().map(|p| p.1) == Some('|') {
            cur.push('|');
            chars.next();
        } else if c == '|' {
            // The next command decides for itself how it takes a `|`.
            let mut parts = vec![cur];
            parts.extend(split_bar(&line[skip_to + off + 1..]));
            return parts;
        } else {
            cur.push(c);
        }
    }
    vec![cur]
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
    while *i < s.len() && s[*i].is_ascii_digit() {
        *i += 1;
    }
    (st < *i)
        .then(|| s[st..*i].iter().collect::<String>().parse().ok())
        .flatten()
}

fn address(
    vim: &mut VimState,
    host: &mut dyn Host,
    s: &[char],
    i: &mut usize,
    cur_line: usize,
) -> R<Option<isize>> {
    let ed = host.ed_ref().ok_or("E32: No file name")?;
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let last = t.last_line() as isize;
    while *i < s.len() && s[*i] == ' ' {
        *i += 1;
    }
    let mut base: Option<isize> = None;
    if *i < s.len() {
        match s[*i] {
            '.' => {
                *i += 1;
                base = Some(cur_line as isize);
            }
            '$' => {
                *i += 1;
                base = Some(last);
            }
            '0'..='9' => {
                base = Some(parse_number(s, i).unwrap_or(0) as isize - 1);
            }
            '\'' => {
                *i += 1;
                let m = *s.get(*i).ok_or("E20: Mark not set")?;
                *i += 1;
                let p = exec::get_mark(vim, host, m).map_err(|e| {
                    if e.starts_with('\0') {
                        "E20: Mark not set".to_string()
                    } else {
                        e
                    }
                })?;
                base = Some(p.line as isize);
            }
            '/' | '?' => {
                let delim = s[*i];
                *i += 1;
                let rest: String = s[*i..].iter().collect();
                let (pat, after) = super::cmdline::split_pattern(&rest, delim);
                *i += pat.chars().count() + usize::from(after.is_some());
                let pat = if pat.is_empty() {
                    vim.last_search
                        .as_ref()
                        .map(|(p, _)| p.clone())
                        .ok_or("E35: No previous regular expression")?
                } else {
                    pat
                };
                vim.last_search = Some((pat.clone(), delim == '/'));
                vim.regs.last_search = pat.clone();
                let re = search::compile(&pat, &vim.search_options())?;
                let ed = host.ed_ref().ok_or("E32")?;
                let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                let hay = exec::haystack(ed);
                let from = if delim == '/' {
                    t.line_end(cur_line)
                } else {
                    t.line_start(cur_line)
                };
                let found = search::search(
                    &ed.rope,
                    &hay,
                    &re,
                    from,
                    delim == '/',
                    vim.opts.wrapscan,
                    false,
                )
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
                let from = if fwd {
                    t.line_end(cur_line)
                } else {
                    t.line_start(cur_line)
                };
                let found =
                    search::search(&ed.rope, &hay, &re, from, fwd, vim.opts.wrapscan, false)
                        .ok_or("E486: Pattern not found")?;
                base = Some(t.pos(found.0).line as isize);
            }
            _ => {}
        }
    }
    // Offsets: +N, -N, +, -.
    loop {
        while *i < s.len() && s[*i] == ' ' && s.get(*i + 1).is_some_and(|c| *c == '+' || *c == '-')
        {
            *i += 1;
        }
        let Some(&c) = s.get(*i) else {
            break;
        };
        if c != '+' && c != '-' {
            break;
        }
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
    while *i < s.len() && (s[*i] == ' ' || s[*i] == ':') {
        *i += 1;
    }
    if s.get(*i) == Some(&'%') {
        *i += 1;
        return Ok(Range {
            start: 0,
            end: last,
            given: 2,
        });
    }
    if s.get(*i) == Some(&'*') {
        *i += 1;
        let a = exec::get_mark(vim, host, '<')?;
        let b = exec::get_mark(vim, host, '>')?;
        return Ok(Range {
            start: a.line,
            end: b.line,
            given: 2,
        });
    }
    let mut lines: Vec<isize> = Vec::new();
    let mut cur_line = cur;
    loop {
        let a = address(vim, host, s, i, cur_line)?;
        while *i < s.len() && s[*i] == ' ' {
            *i += 1;
        }
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
                if let Some(a) = a {
                    lines.push(a);
                } else if !lines.is_empty() {
                    lines.push(cur_line as isize);
                }
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
    Ok(Range {
        start: a,
        end: b,
        given,
    })
}

// ---------------------------------------------------------------------------
// Command table

const COMMANDS: &[(&str, usize)] = &[
    ("append", 1),
    ("buffer", 1),
    ("bNext", 2),
    ("bdelete", 2),
    ("bfirst", 2),
    ("blast", 2),
    ("bnext", 2),
    ("bprevious", 2),
    ("brewind", 2),
    ("buffers", 7),
    ("bwipeout", 2),
    ("cd", 2),
    ("center", 2),
    ("changes", 7),
    ("close", 3),
    ("copy", 2),
    ("delete", 1),
    ("delmarks", 4),
    ("display", 2),
    ("echo", 2),
    ("edit", 1),
    ("enew", 3),
    ("exit", 3),
    ("file", 1),
    ("files", 5),
    ("global", 1),
    ("help", 1),
    ("join", 1),
    ("jumps", 2),
    ("k", 1),
    ("left", 2),
    ("let", 3),
    ("ls", 2),
    ("lcd", 2),
    ("mark", 2),
    ("marks", 5),
    ("move", 1),
    ("new", 3),
    ("nohlsearch", 3),
    ("normal", 4),
    ("only", 2),
    ("print", 1),
    ("put", 2),
    ("pwd", 2),
    ("quit", 1),
    ("qall", 2),
    ("quitall", 5),
    ("read", 1),
    ("redo", 3),
    ("registers", 3),
    ("retab", 3),
    ("right", 2),
    ("saveas", 3),
    ("set", 2),
    ("setglobal", 4),
    ("setlocal", 4),
    ("silent", 3),
    ("source", 2),
    ("sort", 3),
    ("split", 2),
    ("startinsert", 4),
    ("startreplace", 6),
    ("stopinsert", 5),
    ("substitute", 1),
    ("t", 1),
    ("tabclose", 4),
    ("tabedit", 4),
    ("tabfirst", 6),
    ("tablast", 4),
    ("tabnew", 6),
    ("tabnext", 4),
    ("tabNext", 4),
    ("tabonly", 4),
    ("tabprevious", 4),
    ("tabrewind", 4),
    ("tcd", 3),
    ("terminal", 3),
    ("undo", 1),
    ("update", 2),
    ("vglobal", 1),
    ("version", 2),
    ("vnew", 3),
    ("vsplit", 2),
    ("wall", 2),
    ("wincmd", 4),
    ("wq", 2),
    ("wqall", 3),
    ("write", 1),
    ("xit", 1),
    ("xall", 2),
    ("yank", 1),
    ("Explore", 1),
    ("Sexplore", 2),
    ("Vexplore", 2),
    ("Lexplore", 2),
    ("vtcode", 6),
    ("VT", 2),
    ("execute", 3),
    ("call", 3),
    ("colorscheme", 4),
    ("syntax", 2),
    ("filetype", 5),
    ("autocmd", 2),
    ("augroup", 3),
    ("highlight", 2),
    ("command", 3),
    ("noremap", 2),
    ("nnoremap", 2),
    ("vnoremap", 2),
    ("xnoremap", 2),
    ("onoremap", 3),
    ("inoremap", 3),
    ("cnoremap", 3),
    ("snoremap", 4),
    ("map", 3),
    ("nmap", 2),
    ("vmap", 2),
    ("xmap", 2),
    ("omap", 2),
    ("imap", 2),
    ("cmap", 2),
    ("smap", 4),
    ("unmap", 3),
    ("nunmap", 3),
    ("vunmap", 2),
    ("xunmap", 2),
    ("ounmap", 2),
    ("iunmap", 2),
    ("cunmap", 2),
    ("sunmap", 4),
    ("mapclear", 4),
    ("nmapclear", 5),
    ("vmapclear", 5),
    ("xmapclear", 5),
    ("omapclear", 5),
    ("imapclear", 5),
    ("cmapclear", 5),
    ("copen", 4),
    ("cwindow", 2),
    ("clist", 2),
    ("cclose", 3),
    ("cnext", 2),
    ("cNext", 2),
    ("cprevious", 2),
    ("smagic", 2),
    ("snomagic", 3),
    ("windo", 5),
    ("bufdo", 5),
    ("tabdo", 4),
    ("messages", 3),
    ("checktime", 6),
    ("redraw", 4),
    ("echomsg", 5),
    ("echoerr", 5),
    ("unlet", 3),
    ("language", 3),
    ("runtime", 2),
    ("packadd", 2),
    ("Plug", 4),
    ("lua", 3),
    ("abbreviate", 2),
    ("iabbrev", 2),
    ("cabbrev", 2),
    ("noreabbrev", 7),
    ("inoreabbrev", 6),
    ("cnoreabbrev", 6),
    ("unabbreviate", 3),
    ("iunabbrev", 5),
    ("cunabbrev", 4),
    ("abclear", 3),
    ("iabclear", 4),
    ("cabclear", 4),
    ("echon", 5),
    ("function", 2),
    ("delfunction", 4),
    ("return", 4),
    ("if", 2),
    ("for", 3),
    ("while", 2),
    ("try", 3),
    ("earlier", 2),
    ("later", 3),
    ("grep", 2),
    ("grepadd", 5),
    ("vimgrep", 3),
    ("vimgrepadd", 8),
    ("lgrep", 2),
    ("lvimgrep", 2),
    ("make", 3),
    ("lmake", 4),
    ("cc", 2),
    ("cfirst", 3),
    ("crewind", 2),
    ("clast", 3),
    ("cnfile", 3),
    ("cpfile", 3),
    ("cexpr", 3),
    ("cgetexpr", 5),
    ("lnext", 2),
    ("lprevious", 2),
    ("lNext", 2),
    ("lfirst", 3),
    ("llast", 3),
    ("lopen", 3),
    ("lclose", 3),
    ("llist", 3),
    ("ll", 2),
    ("cdo", 3),
    ("cfdo", 3),
    ("find", 3),
    ("sfind", 2),
    ("tabfind", 4),
    ("resize", 3),
    ("vertical", 4),
    ("horizontal", 3),
    ("aboveleft", 3),
    ("belowright", 3),
    ("topleft", 2),
    ("botright", 2),
    ("keepjumps", 5),
    ("keepmarks", 3),
    ("keeppatterns", 5),
    ("lockmarks", 3),
    ("noautocmd", 3),
    ("sbuffer", 2),
    ("sbnext", 3),
    ("sbprevious", 3),
    ("badd", 3),
    ("args", 2),
    ("argadd", 4),
    ("next", 1),
    ("previous", 4),
    ("Next", 1),
    ("first", 3),
    ("last", 3),
    ("argdo", 5),
    ("wnext", 2),
];

fn resolve(name: &str) -> Option<&'static str> {
    if let Some(&(full, _)) = COMMANDS.iter().find(|(full, _)| *full == name) {
        return Some(full);
    }
    COMMANDS
        .iter()
        .find(|(full, min)| name.len() >= *min && full.starts_with(name))
        .map(|(f, _)| *f)
}

fn run_one(vim: &mut VimState, host: &mut dyn Host, text: &str) -> R<()> {
    let s: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < s.len() && (s[i] == ' ' || s[i] == ':' || s[i] == '\t') {
        i += 1;
    }
    if i >= s.len() {
        return Ok(());
    }
    if s[i] == '"' {
        return Ok(());
    }
    let view_command = host.ed_ref().is_none() && {
        // A tab with no Vim buffer behind it (Commit Details, Settings, a
        // preview or diff): window, tab and buffer commands still act on it.
        let name: String = s[i..]
            .iter()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        resolve(&name).is_some_and(view_safe)
    };
    if host.ed_ref().is_none() && !view_command {
        return run_without_buffer(vim, host, &s[i..].iter().collect::<String>());
    }
    let range = if view_command {
        Range {
            start: 0,
            end: 0,
            given: 0,
        }
    } else {
        parse_range(vim, host, &s, &mut i)?
    };
    while i < s.len() && s[i] == ' ' {
        i += 1;
    }
    // Command name.
    let name: String = if i < s.len() && s[i].is_ascii_alphabetic() {
        let st = i;
        while i < s.len() && s[i].is_ascii_alphabetic() {
            i += 1;
        }
        // `:s` variants like `:s#a#b#`, `:sno`, `:k{a}`: keep only known names.
        let n: String = s[st..i].iter().collect();
        if n.len() > 1 && n.starts_with('k') && resolve(&n).is_none() && n.len() == 2 {
            i = st + 1;
            "k".into()
        } else if n.starts_with('s') && resolve(&n).is_none() && !n.starts_with("se") {
            // `:sg`, `:si`... are :s with flags; treat as substitute.
            i = st + 1;
            "s".into()
        } else {
            n
        }
    } else if i < s.len() {
        let c = s[i];
        i += 1;
        if c == '&' && s.get(i) == Some(&'&') {
            i += 1;
            "&&".into()
        } else {
            c.to_string()
        }
    } else {
        String::new()
    };
    let bang = s.get(i) == Some(&'!') && !matches!(name.as_str(), "!" | "=");
    if bang {
        i += 1;
    }
    let arg: String = s[i..].iter().collect::<String>();
    let arg = match resolve(&name) {
        Some(full) if expands_files(full) => expand_specials(host, &arg),
        Some("vimgrep" | "vimgrepadd" | "lvimgrep") => {
            // Only the file names after the pattern.
            let (pat, files) = split_vimgrep(&arg);
            format!("{pat}{}", expand_specials(host, &files))
        }
        _ => arg,
    };
    let arg_trim = arg.trim();
    let path = || {
        let file = super::paths::argument(&arg);
        (!file.is_empty()).then(|| file.to_owned())
    };
    if !name.is_empty()
        && range.end
            > host.ed_ref().map_or(0, |e| {
                Text::of(&e.rope, e.vim_buf.trailing_empty).last_line()
            })
        && full_needs_range(&name)
    {
        return Err("E16: Invalid range".into());
    }
    if name.is_empty() {
        // `:N` or a range alone: go to its last line.
        if range.given == 0 {
            return Ok(());
        }
        let Some(ed) = host.ed() else {
            return Ok(());
        };
        push_jump(vim, ed);
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        let line = range.end.min(t.last_line());
        set_cursor(ed, Pos::new(line, t.first_nonblank(line)));
        update_curswant(vim, ed);
        return Ok(());
    }
    if name.starts_with(|c: char| c.is_ascii_uppercase())
        && let Some(r) = run_user_command(vim, host, &range, &name, bang, &arg)
    {
        return r;
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
        let modes = map::modes_for(&full.replace("unmap", "map"))
            .map(|m| m.0)
            .unwrap_or_default();
        let lhs = map::map_keys(arg_trim, &vim.leader.clone());
        if !vim.maps.remove(&modes, &lhs) {
            return Err(format!("E31: No such mapping: {arg_trim}"));
        }
        return Ok(());
    }
    if full.ends_with("mapclear") {
        let modes = map::modes_for(&full.replace("mapclear", "map"))
            .map(|m| m.0)
            .unwrap_or_default();
        vim.maps.clear(&modes);
        return Ok(());
    }
    match full {
        "write" | "update" | "wq" | "xit" | "exit" => {
            let path = path();
            let write = match full {
                // As in Vim, only a changed buffer is written: `:x` and `ZZ`
                // on an unchanged one just close it, named or not.
                "update" | "xit" | "exit" => {
                    host.ed_ref().is_some_and(|e| e.modified) || path.is_some()
                }
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
                if range.given > 0
                    && path.is_some()
                    && !(range.start == 0
                        && range.end
                            == Text::of(
                                &host.ed_ref().unwrap().rope,
                                host.ed_ref().unwrap().vim_buf.trailing_empty,
                            )
                            .last_line())
                {
                    return Err(
                        "E140: Writing part of a file is not supported; use :'<,'>w !cat > file"
                            .into(),
                    );
                }
                host.act(Act::Write { path, force: bang })?;
            }
            if quit {
                host.act(Act::Quit { force: bang })?;
            }
            Ok(())
        }
        "saveas" => {
            let path = path();
            if path.is_none() {
                return Err("E471: Argument required".into());
            }
            host.act(Act::Write { path, force: bang })
        }
        "wall" => host.act(Act::WriteAll),
        "wqall" | "xall" => host.act(Act::WriteQuitAll),
        "quit" | "close" => host.act(Act::Quit { force: bang }),
        "qall" | "quitall" => host.act(Act::QuitAll { force: bang }),
        "edit" => match path() {
            Some(path) if path == "#" => host.act(Act::Alternate),
            None => host.act(Act::Reload { force: bang }),
            Some(path) => host.act(Act::Open {
                target: OpenTarget::Path(path),
                split: None,
            }),
        },
        "enew" => host.act(Act::Open {
            target: OpenTarget::Empty,
            split: None,
        }),
        "new" | "split" | "vnew" | "vsplit" => {
            use super::parse::Window;
            let vertical = full.starts_with('v');
            if arg_trim.is_empty() && matches!(full, "split" | "vsplit") {
                let window = if vertical {
                    Window::VSplit
                } else {
                    Window::Split
                };
                return host.act(Act::Window(window, 1));
            }
            host.act(Act::Open {
                target: path().map_or(OpenTarget::Empty, OpenTarget::Path),
                split: Some(if vertical {
                    TabDropZone::Right
                } else {
                    TabDropZone::Bottom
                }),
            })
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
        "tabnew" | "tabedit" => host.act(Act::Open {
            target: path().map_or(OpenTarget::Empty, OpenTarget::Path),
            split: None,
        }),
        "tabclose" => host.act(Act::TabClose { force: bang }),
        "tabonly" => host.act(Act::TabOnly),
        "bnext" => host.act(Act::BufferNext(arg_trim.parse().unwrap_or(1))),
        "bprevious" | "bNext" => host.act(Act::BufferPrev(arg_trim.parse().unwrap_or(1))),
        "bfirst" | "brewind" => host.act(Act::Buffer("1".into())),
        "blast" => host.act(Act::Buffer("$".into())),
        "buffer" => {
            if arg_trim.is_empty() && range.given > 0 {
                return host.act(Act::Buffer((range.end + 1).to_string()));
            }
            if arg_trim.is_empty() {
                return Ok(());
            }
            if arg_trim == "#" {
                return host.act(Act::Alternate);
            }
            host.act(Act::Buffer(arg_trim.into()))
        }
        "bdelete" | "bwipeout" => host.act(Act::BufferDelete { force: bang }),
        "buffers" | "files" | "ls" => {
            let lines: Vec<String> = host
                .buffers()
                .into_iter()
                .map(|(n, name, modified)| {
                    format!("{:>3} {} \"{}\"", n, if modified { "+" } else { " " }, name)
                })
                .collect();
            vim.set_lines(lines);
            Ok(())
        }
        "copen" | "cwindow" | "clist" if !vim.qf.is_empty() => {
            qf_list(vim, full == "clist" && bang);
            Ok(())
        }
        "cnext" | "cprevious" | "cNext" if !vim.qf.is_empty() => {
            let n = arg_trim.parse().unwrap_or(1);
            qf_jump(
                vim,
                host,
                if full == "cnext" {
                    QfMove::Next(n)
                } else {
                    QfMove::Prev(n)
                },
            )
        }
        "cclose" if !vim.qf.is_empty() => {
            vim.message = None;
            host.act(Act::Quickfix(super::Quickfix::Close))
        }
        "copen" | "cwindow" | "clist" => host.act(Act::Quickfix(super::Quickfix::Open)),
        "cclose" => host.act(Act::Quickfix(super::Quickfix::Close)),
        "cnext" => host.act(Act::Quickfix(super::Quickfix::Next)),
        "cprevious" | "cNext" => host.act(Act::Quickfix(super::Quickfix::Prev)),
        "Explore" | "Lexplore" if arg_trim.is_empty() => host.act(Act::Explore),
        "Explore" | "Lexplore" | "Sexplore" | "Vexplore" => host.act(Act::Open {
            target: OpenTarget::Directory(path()),
            split: match full {
                "Sexplore" => Some(TabDropZone::Bottom),
                "Vexplore" => Some(TabDropZone::Right),
                _ => None,
            },
        }),
        "terminal" => host.act(Act::Terminal(
            (!arg_trim.is_empty()).then(|| arg_trim.to_string()),
        )),
        "cd" | "lcd" | "tcd" => host.act(Act::Cd(path())),
        "pwd" => {
            vim.set_message(host.cwd_label());
            Ok(())
        }
        "vtcode" | "VT" => {
            if arg_trim.is_empty() {
                return Err("E471: Argument required".into());
            }
            host.act(Act::Command(arg_trim.into()))
        }
        "file" => {
            let keys = [Key::ctrl('g')];
            vim.feed_keys(host, &keys, false, false);
            Ok(())
        }
        "nohlsearch" | "noh" | "nohl" => {
            vim.hl_hidden = true;
            Ok(())
        }
        "substitute" | "&" | "&&" | "~" | "smagic" | "snomagic" => {
            substitute(vim, host, &range, full, &arg)
        }
        "global" | "vglobal" => global(vim, host, &range, full == "vglobal" || bang, &arg),
        "normal" => normal(vim, host, &range, &arg, !bang),
        "delete" | "yank" => {
            let (reg, count) = reg_count(arg_trim);
            let range = with_count(host, range, count);
            let region = exec::Region {
                start: Pos::new(range.start, 0),
                end: Pos::new(range.end, 0),
                kind: exec::RKind::Line,
                nl: false,
            };
            let saved = host.ed_ref().map(cursor);
            if let Some(ed) = host.ed() {
                ed.cursor_line = range.start;
            }
            let op = if full == "delete" {
                super::parse::Op::Delete
            } else {
                super::parse::Op::Yank
            };
            if op == super::parse::Op::Delete && read_only(host) {
                return Err("E21: Cannot make changes, 'modifiable' is off".into());
            }
            super::ops::apply(vim, host, reg, op, region, 1, false, false);
            if op == super::parse::Op::Yank {
                if let (Some(ed), Some(p)) = (host.ed(), saved) {
                    set_cursor(ed, p);
                }
            }
            Ok(())
        }
        "put" => {
            let reg = arg_trim.chars().next();
            let Some(value) = super::ops::read_reg(vim, host, reg) else {
                return Err(format!("E353: Nothing in register {}", reg.unwrap_or('"')));
            };
            let mut value = value;
            value.kind = RegKind::Line;
            if !value.text.ends_with('\n') {
                value.text.push('\n');
            }
            let Some(ed) = host.ed() else {
                return Ok(());
            };
            // `:0put` puts above the first line.
            let line = if range.given > 0 {
                range.end
            } else {
                ed.cursor_line
            };
            let above = bang || (range.given > 0 && s_starts_zero(text));
            ed.cursor_line = line;
            super::ops::put_value(vim, ed, &value, above, false, false, 1);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let last = ed
                .vim_buf
                .marks
                .get(&']')
                .map_or(ed.cursor_line, |p| p.line);
            set_cursor(ed, Pos::new(last, t.first_nonblank(last)));
            Ok(())
        }
        "move" | "copy" | "t" => {
            let mut j = 0;
            let a: Vec<char> = arg_trim.chars().collect();
            let dest = if a.first() == Some(&'0') && a.len() == 1 {
                -1
            } else {
                let cur = host.ed_ref().map_or(0, |e| e.cursor_line);
                address(vim, host, &a, &mut j, cur)?.ok_or("E14: Invalid address")?
            };
            move_copy(vim, host, &range, dest, full == "move")
        }
        "join" => {
            let (_, count) = reg_count(arg_trim);
            let Some(ed) = host.ed() else {
                return Ok(());
            };
            let (s, mut e) = (range.start, range.end);
            if let Some(c) = count {
                e = range.end + c - 1;
            } else if range.given < 2 || s == e {
                e = s + 1;
            }
            if read_only_ed(ed) {
                return Err("E21".into());
            }
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
            let Some(ed) = host.ed() else {
                return Ok(());
            };
            if read_only_ed(ed) {
                return Err("E21".into());
            }
            let mut tx = Tx::default();
            super::ops::shift_lines(
                vim,
                ed,
                &mut tx,
                range.start,
                range.end,
                full == ">",
                extra + 1,
            );
            tx.finish(ed);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            set_cursor(ed, Pos::new(range.end, t.first_nonblank(range.end)));
            Ok(())
        }
        "k" | "mark" => {
            let m = arg_trim.chars().next().ok_or("E471: Argument required")?;
            let Some(ed) = host.ed() else {
                return Ok(());
            };
            let p = Pos::new(range.end, 0);
            if !exec::set_mark(vim, ed, m, p) {
                return Err("E191: Argument must be a letter or forward/backward quote".into());
            }
            Ok(())
        }
        "delmarks" => {
            let Some(ed) = host.ed() else {
                return Ok(());
            };
            if bang {
                ed.vim_buf.marks.retain(|c, _| !c.is_ascii_lowercase());
                return Ok(());
            }
            let chars: Vec<char> = arg_trim.chars().filter(|c| *c != ' ').collect();
            let mut k = 0;
            while k < chars.len() {
                if k + 2 < chars.len() && chars[k + 1] == '-' {
                    for c in chars[k]..=chars[k + 2] {
                        ed.vim_buf.marks.remove(&c);
                        vim.global_marks.remove(&c);
                    }
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
            let Some(ed) = host.ed_ref() else {
                return Ok(());
            };
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let mut lines = vec!["mark line  col file/text".to_string()];
            for (c, p) in &ed.vim_buf.marks {
                if *c == '"' {
                    continue;
                }
                if p.line > t.last_line() {
                    continue;
                }
                lines.push(format!(
                    " {c} {:>6} {:>4} {}",
                    p.line + 1,
                    p.col,
                    t.line_string(p.line).trim()
                ));
            }
            for (c, j) in &vim.global_marks {
                let name = j
                    .path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                lines.push(format!(
                    " {c} {:>6} {:>4} {name}",
                    j.pos.line + 1,
                    j.pos.col
                ));
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
                if !filter.is_empty() && !filter.contains(&r) {
                    continue;
                }
                let kind = match reg.kind {
                    RegKind::Char => 'c',
                    RegKind::Line => 'l',
                    RegKind::Block(_) => 'b',
                };
                let shown: String = super::ops::keys_to_text(&super::ops::text_to_keys(&reg.text))
                    .chars()
                    .map(|c| {
                        if c == '\n' {
                            "^J".to_string()
                        } else if (c as u32) < 0x20 {
                            format!("^{}", ((c as u8) ^ 0x40) as char)
                        } else {
                            c.to_string()
                        }
                    })
                    .collect();
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
                let name = j
                    .path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                lines.push(format!(
                    "{}{rel:>4} {:>5} {:>4} {name}",
                    if n == vim.jump_idx { '>' } else { ' ' },
                    j.pos.line + 1,
                    j.pos.col
                ));
            }
            if vim.jump_idx >= vim.jumps.len() {
                lines.push(">".into());
            }
            vim.set_lines(lines);
            Ok(())
        }
        "changes" => {
            let Some(ed) = host.ed_ref() else {
                return Ok(());
            };
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let mut lines = vec!["change line  col text".to_string()];
            let idx = ed.vim_buf.change_idx;
            for (n, p) in ed.vim_buf.changes.iter().enumerate() {
                let rel = (n as isize - idx as isize).unsigned_abs();
                lines.push(format!(
                    "{}{rel:>5} {:>5} {:>4} {}",
                    if n == idx { '>' } else { ' ' },
                    p.line + 1,
                    p.col,
                    t.line_string(p.line.min(t.last_line())).trim()
                ));
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
            let path = expand_path(host, &arg);
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("E484: Can't open file {arg_trim}: {e}"))?;
            let errors = source(vim, host, &text);
            if !errors.is_empty() {
                vim.set_lines(errors);
            }
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
            let Some(ed) = host.ed() else {
                return Ok(());
            };
            let line = if range.given > 0 {
                range.end
            } else {
                ed.cursor_line
            };
            let _ = ed;
            let text = if let Some(cmd) = arg_trim.strip_prefix('!') {
                shell(host, cmd.trim(), None)?
            } else {
                let path = expand_path(host, &arg);
                std::fs::read_to_string(&path)
                    .map_err(|e| format!("E484: Can't open file {arg_trim}: {e}"))?
            };
            let Some(ed) = host.ed() else {
                return Ok(());
            };
            ed.cursor_line = line;
            let mut text = exec::plain(&text);
            if !text.ends_with('\n') {
                text.push('\n');
            }
            super::ops::put_value(
                vim,
                ed,
                &Reg::new(text, RegKind::Line),
                false,
                false,
                false,
                1,
            );
            Ok(())
        }
        "=" => {
            let Some(ed) = host.ed_ref() else {
                return Ok(());
            };
            let n = if range.given > 0 {
                range.end + 1
            } else {
                Text::of(&ed.rope, ed.vim_buf.trailing_empty).lines()
            };
            vim.set_message(n.to_string());
            Ok(())
        }
        "print" | "#" => {
            let Some(ed) = host.ed_ref() else {
                return Ok(());
            };
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let lines: Vec<String> = (range.start..=range.end)
                .map(|l| {
                    if full == "#" {
                        format!("{:>3} {}", l + 1, t.line_string(l))
                    } else {
                        t.line_string(l)
                    }
                })
                .collect();
            if lines.len() == 1 {
                vim.set_message(lines[0].clone());
            } else {
                vim.set_lines(lines);
            }
            Ok(())
        }
        "echo" | "echon" | "echomsg" | "echoerr" => echo(vim, host, full, arg_trim),
        "unlet" => {
            for name in arg_trim.split_whitespace() {
                if !super::eval::unset_var(vim, name) && !bang {
                    return Err(format!("E108: No such variable: \"{name}\""));
                }
            }
            Ok(())
        }
        "call" => call_cmd(vim, host, &range, arg_trim),
        "function" => {
            if arg_trim.is_empty() {
                let lines: Vec<String> = vim
                    .funcs
                    .iter()
                    .map(|(n, f)| format!("function {n}({})", f.params.join(", ")))
                    .collect();
                vim.set_lines(lines);
                return Ok(());
            }
            Err("E126: Missing :endfunction".into())
        }
        "delfunction" => {
            let n = super::script::normalize_name(arg_trim.trim_end_matches("()"));
            if vim.funcs.remove(&n).is_none() && !bang {
                return Err(format!("E130: Unknown function: {arg_trim}"));
            }
            Ok(())
        }
        "if" | "for" | "while" | "try" => Err(format!(
            "E171: Missing :end{full} (use '|' to put the block on one line)"
        )),
        "return" => Ok(()),
        "abbreviate" | "iabbrev" | "cabbrev" | "noreabbrev" | "inoreabbrev" | "cnoreabbrev" => {
            abbrev_cmd(vim, full, arg_trim)
        }
        "unabbreviate" | "iunabbrev" | "cunabbrev" => {
            let insert = full != "cunabbrev";
            let cmdline = full != "iunabbrev";
            let before = vim.abbrevs.len();
            vim.abbrevs.retain(|a| {
                !(a.lhs == arg_trim && ((insert && a.insert) || (cmdline && a.cmdline)))
            });
            if vim.abbrevs.len() == before {
                return Err(format!("E24: No such abbreviation: {arg_trim}"));
            }
            Ok(())
        }
        "abclear" | "iabclear" | "cabclear" => {
            let insert = full != "cabclear";
            let cmdline = full != "iabclear";
            vim.abbrevs
                .retain(|a| !((insert && a.insert) || (cmdline && a.cmdline)));
            Ok(())
        }
        "earlier" | "later" => earlier_later(vim, host, full == "earlier", arg_trim),
        "grep" | "grepadd" | "lgrep" | "make" | "lmake" => {
            let add = full == "grepadd";
            let is_make = full.ends_with("make");
            quickfix_shell(vim, host, is_make, add, bang, arg_trim)
        }
        "vimgrep" | "vimgrepadd" | "lvimgrep" => {
            vimgrep(vim, host, full == "vimgrepadd", bang, arg_trim)
        }
        "cexpr" | "cgetexpr" => {
            let v = {
                let mut cx = super::eval::Ctx::new(vim, host);
                super::eval::eval(arg_trim, &mut cx)?
            };
            let items = parse_errors(&v.to_text(), host.cwd().unwrap_or_default());
            set_quickfix(vim, items, false);
            if full == "cexpr" {
                qf_jump(vim, host, QfMove::Nth(1))?;
            }
            Ok(())
        }
        "cc" | "ll" => {
            let n = arg_trim
                .parse()
                .ok()
                .or(range.given.gt(&0).then_some(range.end + 1));
            qf_jump(
                vim,
                host,
                match n {
                    Some(n) => QfMove::Nth(n),
                    None => QfMove::Current,
                },
            )
        }
        "cfirst" | "crewind" | "lfirst" => {
            qf_jump(vim, host, QfMove::Nth(arg_trim.parse().unwrap_or(1)))
        }
        "clast" | "llast" => qf_jump(
            vim,
            host,
            match arg_trim.parse() {
                Ok(n) => QfMove::Nth(n),
                Err(_) => QfMove::Last,
            },
        ),
        "cnfile" | "cpfile" => qf_jump(vim, host, QfMove::File(full == "cnfile")),
        "cdo" | "cfdo" => qf_do(vim, host, full == "cfdo", arg_trim),
        "find" | "sfind" | "tabfind" => {
            let found = find_file(vim, host, arg_trim)?;
            host.act(Act::Open {
                target: OpenTarget::Path(found),
                split: match full {
                    "sfind" => Some(TabDropZone::Bottom),
                    _ => None,
                },
            })
        }
        "resize" => resize(vim, host, arg_trim, false),
        "vertical" | "horizontal" | "aboveleft" | "belowright" | "topleft" | "botright" => {
            let rest = arg_trim;
            let word: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect();
            match (full, resolve(&word)) {
                ("vertical", Some("resize")) => resize(vim, host, rest[word.len()..].trim(), true),
                ("vertical", Some("split")) => {
                    run(vim, host, &format!("vsplit{}", &rest[word.len()..]))
                }
                ("vertical", Some("new")) => {
                    run(vim, host, &format!("vnew{}", &rest[word.len()..]))
                }
                ("vertical", Some("sbuffer")) => {
                    host.act(Act::Window(super::parse::Window::VSplit, 1))?;
                    run(vim, host, &format!("buffer{}", &rest[word.len()..]))
                }
                ("vertical", Some("help" | "terminal")) => run(vim, host, rest),
                _ => run(vim, host, rest),
            }
        }
        "keepjumps" | "keepmarks" | "keeppatterns" | "lockmarks" | "noautocmd" => {
            let saved = (vim.last_search.clone(), vim.jumps.len());
            let r = run(vim, host, arg_trim);
            if full == "keeppatterns" {
                vim.last_search = saved.0;
            }
            if full == "keepjumps" {
                vim.jumps.truncate(saved.1);
                vim.jump_idx = vim.jump_idx.min(vim.jumps.len());
            }
            r
        }
        "sbuffer" | "sbnext" | "sbprevious" => {
            host.act(Act::Window(super::parse::Window::Split, 1))?;
            match full {
                "sbnext" => host.act(Act::BufferNext(1)),
                "sbprevious" => host.act(Act::BufferPrev(1)),
                _ if arg_trim.is_empty() => Ok(()),
                _ => host.act(Act::Buffer(arg_trim.into())),
            }
        }
        "badd" | "argadd" => Ok(()),
        "args" => {
            let lines: Vec<String> = host
                .buffers()
                .into_iter()
                .map(|(_, name, _)| name)
                .collect();
            vim.set_message(lines.join(" "));
            Ok(())
        }
        "next" | "wnext" => {
            if full == "wnext" {
                host.act(Act::Write {
                    path: None,
                    force: bang,
                })?;
            }
            host.act(Act::BufferNext(arg_trim.parse().unwrap_or(1)))
        }
        "previous" | "Next" => host.act(Act::BufferPrev(arg_trim.parse().unwrap_or(1))),
        "first" => host.act(Act::Buffer("1".into())),
        "last" => host.act(Act::Buffer("$".into())),
        "argdo" => run(vim, host, &format!("bufdo {arg_trim}")),
        "lnext" | "lprevious" | "lNext" | "lopen" | "lclose" | "llist" => match full {
            "lnext" => run(vim, host, "cnext"),
            "lopen" => run(vim, host, "copen"),
            "lclose" => run(vim, host, "cclose"),
            "llist" => run(vim, host, "clist"),
            _ => run(vim, host, "cprevious"),
        },
        "messages" | "checktime" | "redraw" | "syntax" | "filetype" | "autocmd" | "augroup"
        | "highlight" | "runtime" | "packadd" | "Plug" | "language" | "lua" => Ok(()),
        "colorscheme" => {
            if arg_trim.is_empty() {
                return Ok(());
            }
            host.act(Act::SetOption {
                name: "colorscheme".into(),
                value: arg_trim.into(),
            })
        }
        "startinsert" | "startreplace" => {
            let kind = if bang {
                super::parse::InsertKind::LineEnd
            } else {
                super::parse::InsertKind::Before
            };
            if vim.is_visual() {
                exec::exit_visual(vim, host);
            }
            super::insert::begin(vim, host, kind, 1, false, None);
            if full == "startreplace" {
                vim.mode = Mode::Replace;
            }
            Ok(())
        }
        "stopinsert" => {
            if vim.is_insert() {
                super::insert::finish(vim, host, true);
            }
            Ok(())
        }
        "silent" => {
            let rest = arg.trim_start_matches('!').trim();
            let r = run(vim, host, rest);
            if r.is_err() && bang {
                return Ok(());
            }
            vim.message = None;
            r
        }
        "execute" => {
            let mut parts = Vec::new();
            let mut rest = arg_trim.to_string();
            while !rest.trim().is_empty() {
                let mut cx = super::eval::Ctx::new(vim, host);
                let (v, r) = super::eval::eval_prefix(&rest, &mut cx)?;
                parts.push(v.to_str());
                if r.len() == rest.len() {
                    break;
                }
                rest = r;
            }
            run(vim, host, &parts.join(" "))
        }
        "help" => {
            vim.set_message(format!(
                "Vim help is not bundled; see :h {} at vimhelp.org",
                if arg_trim.is_empty() {
                    "index"
                } else {
                    arg_trim
                }
            ));
            Ok(())
        }
        "version" => {
            vim.set_message(
                "VTCode Vim mode (Vim 9 compatible normal, visual, insert and ex commands)",
            );
            Ok(())
        }
        "left" | "right" | "center" => align(vim, host, &range, full, arg_trim),
        "retab" => retab(vim, host, &range, bang, arg_trim),
        "windo" | "bufdo" | "tabdo" => run(vim, host, arg_trim),
        "append" => Err("E319: :append is not supported; use o".into()),
        "command" => user_command(vim, bang, arg_trim),
        "@" | "*" => {
            let r = arg_trim.chars().next().unwrap_or('@');
            if let Some(ed) = host.ed() {
                ed.cursor_line = range.end;
            }
            let keys = [
                Key::ch('@'),
                Key::ch(if r == '@' && full == "*" { '@' } else { r }),
            ];
            vim.feed_keys(host, &keys, false, false);
            Ok(())
        }
        _ => Err(format!("E492: Not an editor command: {}", text.trim())),
    }
}

/// Commands that act on buffer lines (a range past the end is an error).
fn full_needs_range(name: &str) -> bool {
    !matches!(
        resolve(name),
        Some(
            "write"
                | "update"
                | "wq"
                | "xit"
                | "exit"
                | "quit"
                | "qall"
                | "quitall"
                | "edit"
                | "split"
                | "vsplit"
                | "buffer"
                | "tabnext"
                | "tabprevious"
                | "set"
                | "map"
                | "nnoremap"
                | "let"
                | "echo"
                | "normal"
        )
    )
}

fn s_starts_zero(text: &str) -> bool {
    let t = text.trim_start_matches([' ', ':']);
    t.starts_with('0') && !t[1..].starts_with(|c: char| c.is_ascii_digit())
}

/// Commands that work on a tab that has no text buffer: they act on tabs,
/// windows and editor-wide state, never on lines.
fn view_safe(full: &str) -> bool {
    matches!(
        full,
        "quit"
            | "close"
            | "xit"
            | "exit"
            | "edit"
            | "enew"
            | "Explore"
            | "Lexplore"
            | "Sexplore"
            | "Vexplore"
            | "tabnext"
            | "tabprevious"
            | "tabNext"
            | "tabfirst"
            | "tabrewind"
            | "tablast"
            | "tabclose"
            | "tabonly"
            | "tabnew"
            | "tabedit"
            | "bnext"
            | "bprevious"
            | "bNext"
            | "bfirst"
            | "brewind"
            | "blast"
            | "buffer"
            | "bdelete"
            | "bwipeout"
            | "buffers"
            | "files"
            | "ls"
            | "only"
            | "split"
            | "vsplit"
            | "new"
            | "vnew"
            | "wincmd"
            | "cd"
            | "lcd"
            | "tcd"
            | "pwd"
            | "nohlsearch"
            | "registers"
            | "display"
            | "messages"
    )
}

fn run_without_buffer(vim: &mut VimState, host: &mut dyn Host, text: &str) -> R<()> {
    let name: String = text
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    let bang = text[name.len()..].starts_with('!');
    let arg = text[name.len() + usize::from(bang)..].trim();
    if name.starts_with(|c: char| c.is_ascii_uppercase()) {
        let range = Range {
            start: 0,
            end: 0,
            given: 0,
        };
        if let Some(r) = run_user_command(vim, host, &range, &name, bang, arg) {
            return r;
        }
    }
    let full = resolve(&name).ok_or_else(|| format!("E492: Not an editor command: {text}"))?;
    if let Some((modes, noremap)) = map::modes_for(full) {
        return map_cmd(vim, &modes, noremap, bang, arg);
    }
    match full {
        "quit" | "qall" | "quitall" => host.act(Act::QuitAll { force: bang }),
        "set" | "setlocal" | "setglobal" => set(vim, host, arg),
        "let" => let_cmd(vim, host, arg),
        "echo" | "echon" | "echomsg" | "echoerr" => echo(vim, host, full, arg),
        "unlet" => {
            for name in arg.split_whitespace() {
                super::eval::unset_var(vim, name);
            }
            Ok(())
        }
        "call" => call_cmd(
            vim,
            host,
            &Range {
                start: 0,
                end: 0,
                given: 0,
            },
            arg,
        ),
        "execute" | "silent" | "keepjumps" | "keeppatterns" | "noautocmd" | "vertical"
        | "aboveleft" | "belowright" | "topleft" | "botright" => {
            let rest = if full == "execute" {
                let mut cx = super::eval::Ctx::new(vim, host);
                let mut parts = Vec::new();
                let mut rest = arg.to_string();
                while !rest.trim().is_empty() {
                    let (v, r) = super::eval::eval_prefix(&rest, &mut cx)?;
                    parts.push(v.to_str());
                    if r.len() == rest.len() {
                        break;
                    }
                    rest = r;
                }
                parts.join(" ")
            } else {
                arg.trim_start_matches('!').trim().to_string()
            };
            let r = run(vim, host, &rest);
            if full == "silent" && bang {
                return Ok(());
            }
            r
        }
        "abbreviate" | "iabbrev" | "cabbrev" | "noreabbrev" | "inoreabbrev" | "cnoreabbrev" => {
            abbrev_cmd(vim, full, arg)
        }
        "command" => user_command(vim, bang, arg),
        "function" | "delfunction" | "return" => Ok(()),
        "messages" | "checktime" | "redraw" | "syntax" | "filetype" | "autocmd" | "augroup"
        | "highlight" | "runtime" | "packadd" | "Plug" | "language" | "lua" | "nohlsearch"
        | "badd" | "argadd" | "unabbreviate" | "iunabbrev" | "cunabbrev" | "abclear"
        | "iabclear" | "cabclear" => {
            if full == "nohlsearch" {
                vim.hl_hidden = true;
            }
            Ok(())
        }
        "colorscheme" => {
            if arg.is_empty() {
                return Ok(());
            }
            host.act(Act::SetOption {
                name: "colorscheme".into(),
                value: arg.into(),
            })
        }
        "source" => {
            let path = expand_path(host, arg);
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("E484: Can't open file {arg}: {e}"))?;
            let errors = source(vim, host, &text);
            if !errors.is_empty() {
                vim.set_lines(errors);
            }
            Ok(())
        }
        "cd" | "lcd" | "tcd" => host.act(Act::Cd((!arg.is_empty()).then(|| arg.to_string()))),
        "pwd" => {
            vim.set_message(host.cwd_label());
            Ok(())
        }
        "grep" | "grepadd" | "lgrep" | "make" | "lmake" => quickfix_shell(
            vim,
            host,
            full.ends_with("make"),
            full == "grepadd",
            bang,
            arg,
        ),
        "vimgrep" | "vimgrepadd" | "lvimgrep" => {
            vimgrep(vim, host, full == "vimgrepadd", bang, arg)
        }
        "cc" | "cfirst" | "crewind" | "clast" if !vim.qf.is_empty() => qf_jump(
            vim,
            host,
            match full {
                "clast" => QfMove::Last,
                "cc" => arg.parse().map_or(QfMove::Current, QfMove::Nth),
                _ => QfMove::Nth(1),
            },
        ),
        "find" | "sfind" | "tabfind" => {
            let found = find_file(vim, host, arg)?;
            host.act(Act::Open {
                target: OpenTarget::Path(found),
                split: None,
            })
        }
        "help" | "version" | "map" => Ok(()),
        "vtcode" | "VT" => host.act(Act::Command(arg.into())),
        "copen" | "cwindow" | "clist" if !vim.qf.is_empty() => {
            qf_list(vim, false);
            Ok(())
        }
        "cnext" | "cprevious" | "cNext" if !vim.qf.is_empty() => qf_jump(
            vim,
            host,
            if full == "cnext" {
                QfMove::Next(1)
            } else {
                QfMove::Prev(1)
            },
        ),
        "copen" | "cwindow" | "clist" => host.act(Act::Quickfix(super::Quickfix::Open)),
        "cclose" => host.act(Act::Quickfix(super::Quickfix::Close)),
        "cnext" => host.act(Act::Quickfix(super::Quickfix::Next)),
        "cprevious" | "cNext" => host.act(Act::Quickfix(super::Quickfix::Prev)),
        "terminal" => host.act(Act::Terminal((!arg.is_empty()).then(|| arg.to_string()))),
        "wall" => host.act(Act::WriteAll),
        _ => Err("E32: No file name".into()),
    }
}

fn read_only(host: &dyn Host) -> bool {
    host.ed_ref().is_some_and(|e| e.read_only)
}
fn read_only_ed(ed: &crate::editor::Editor) -> bool {
    ed.read_only
}

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
    let last = host.ed_ref().map_or(0, |e| {
        Text::of(&e.rope, e.vim_buf.trailing_empty).last_line()
    });
    match count {
        Some(n) if n > 0 => Range {
            start: range.end,
            end: (range.end + n - 1).min(last),
            given: 2,
        },
        _ => range,
    }
}

fn range_text(host: &dyn Host, range: &Range) -> String {
    let Some(ed) = host.ed_ref() else {
        return String::new();
    };
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
    let p = super::paths::argument(p);
    let base = host.cwd().unwrap_or_default();
    if p == "%" {
        if let Some(path) = host.ed_ref().and_then(|ed| ed.file_path.as_ref()) {
            return path.clone();
        }
        return base.join(host.file_name().unwrap_or_default());
    }
    super::paths::resolve_local(&base, p)
}

/// Run a shell command, optionally feeding stdin.
pub(crate) fn shell(host: &dyn Host, cmd: &str, input: Option<&str>) -> R<String> {
    if cmd.is_empty() {
        return Err("E471: Argument required".into());
    }
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
    if let Some(dir) = host.cwd() {
        command.current_dir(dir);
    }
    command.stdin(if input.is_some() {
        std::process::Stdio::piped()
    } else {
        std::process::Stdio::null()
    });
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|e| format!("E282: Cannot execute shell: {e}"))?;
    if let Some(text) = input {
        use std::io::Write;
        if let Some(mut stdin) = child.stdin.take() {
            let text = text.to_string();
            std::thread::spawn(move || {
                let _ = stdin.write_all(text.as_bytes());
            });
        }
    }
    let out = child.wait_with_output().map_err(|e| format!("E282: {e}"))?;
    let mut s = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() && s.is_empty() {
        s = String::from_utf8_lossy(&out.stderr).to_string();
        if s.trim().is_empty() {
            s = format!("shell returned {}", out.status.code().unwrap_or(-1));
        }
    }
    Ok(s)
}

fn filter(vim: &mut VimState, host: &mut dyn Host, range: &Range, cmd: &str) -> R<()> {
    if read_only(host) {
        return Err("E21: Cannot make changes, 'modifiable' is off".into());
    }
    let input = range_text(host, range);
    let out = exec::plain(&shell(host, cmd, Some(&input))?);
    let Some(ed) = host.ed() else {
        return Ok(());
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let s = t.line_start(range.start);
    let e = t.line_end(range.end);
    let mut out = out;
    if out.ends_with('\n') {
        out.pop();
    }
    let mut tx = Tx::default();
    tx.edit(ed, s, e, &out);
    tx.finish(ed);
    let n = range.end - range.start + 1;
    set_cursor(
        ed,
        Pos::new(
            range.start,
            Text::of(&ed.rope, ed.vim_buf.trailing_empty).first_nonblank(range.start),
        ),
    );
    if n > vim.opts.report {
        vim.set_message(format!("{n} lines filtered"));
    }
    Ok(())
}

fn move_copy(
    vim: &mut VimState,
    host: &mut dyn Host,
    range: &Range,
    dest: isize,
    is_move: bool,
) -> R<()> {
    if read_only(host) {
        return Err("E21".into());
    }
    let Some(ed) = host.ed() else {
        return Ok(());
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    if dest > t.last_line() as isize {
        return Err("E16: Invalid range".into());
    }
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
    let ends_eol =
        ed.rope.len_chars() > 0 && matches!(ed.rope.char(ed.rope.len_chars() - 1), '\n' | '\r');
    let insert_at = |ed: &crate::editor::Editor, after: isize| -> (usize, bool) {
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        if after < 0 {
            return (0, false);
        }
        let line = after as usize;
        if line >= t.last_line() && !ends_eol {
            (ed.rope.len_chars(), true)
        } else {
            (t.line_next_start(line), false)
        }
    };
    let new_last;
    if is_move {
        if dest < range.start as isize {
            // Delete first (below), then insert above.
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let (s, e) = line_span_for_delete(&t, range.start, range.end);
            tx.edit(ed, s, e, "");
            let (at, prepend) = insert_at(ed, dest);
            let body = if prepend {
                format!("\n{}", text.trim_end_matches('\n'))
            } else {
                text.clone()
            };
            tx.edit(ed, at, at, &body);
            new_last = (dest + n as isize) as usize;
        } else {
            let (at, prepend) = insert_at(ed, dest);
            let body = if prepend {
                format!("\n{}", text.trim_end_matches('\n'))
            } else {
                text.clone()
            };
            tx.edit(ed, at, at, &body);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let (s, e) = line_span_for_delete(&t, range.start, range.end);
            tx.edit(ed, s, e, "");
            new_last = dest as usize;
        }
    } else {
        let (at, prepend) = insert_at(ed, dest);
        let body = if prepend {
            format!("\n{}", text.trim_end_matches('\n'))
        } else {
            text.clone()
        };
        tx.edit(ed, at, at, &body);
        new_last = (dest + n as isize) as usize;
    }
    tx.finish(ed);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let line = new_last.min(t.last_line());
    set_cursor(ed, Pos::new(line, t.first_nonblank(line)));
    update_curswant(vim, ed);
    if n > vim.opts.report {
        vim.set_message(format!(
            "{n} lines {}",
            if is_move { "moved" } else { "copied" }
        ));
    }
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
    let Some(after) = after else {
        return Some((pat, None, String::new(), None));
    };
    // Replacement ends at the next unescaped delimiter.
    let mut rep = String::new();
    let mut chars = after.chars();
    let mut tail = String::new();
    let mut closed = false;
    while let Some(c) = chars.next() {
        if c == '\\' {
            rep.push(c);
            if let Some(n) = chars.next() {
                rep.push(n);
            }
            continue;
        }
        if c == delim {
            closed = true;
            tail = chars.collect();
            break;
        }
        rep.push(c);
    }
    let _ = closed;
    let tail = tail.trim();
    let flags: String = tail
        .chars()
        .take_while(|c| "&cegiInp#lr".contains(*c))
        .collect();
    let count = tail[flags.len()..].trim().parse().ok();
    Some((pat, Some(rep), flags, count))
}

fn substitute(
    vim: &mut VimState,
    host: &mut dyn Host,
    range: &Range,
    cmd: &str,
    arg: &str,
) -> R<()> {
    if read_only(host) {
        return Err("E21: Cannot make changes, 'modifiable' is off".into());
    }
    let magic = cmd != "snomagic" && vim.opts.magic;
    let last = vim.last_sub.clone();
    let (pattern, replacement, mut flags, count) = match parse_sub_args(arg) {
        Some((pat, rep, flags, count))
            if cmd == "substitute" || cmd == "smagic" || cmd == "snomagic" =>
        {
            let pat = if pat.is_empty() {
                vim.last_search
                    .as_ref()
                    .map(|s| s.0.clone())
                    .or_else(|| last.as_ref().map(|s| s.pattern.clone()))
                    .ok_or("E35: No previous regular expression")?
            } else {
                pat
            };
            let rep = match rep {
                Some(r) => search::resolve_tilde(
                    &r,
                    last.as_ref().map_or("", |s| s.replacement.as_str()),
                    magic,
                ),
                None => String::new(),
            };
            (pat, rep, flags, count)
        }
        _ => {
            // :s [flags] [count], :&, :&&, :~
            let l = last.clone().ok_or("E35: No previous regular expression")?;
            let tail = arg.trim();
            let mut flags: String = tail
                .chars()
                .take_while(|c| "&cegiInp#lr".contains(*c))
                .collect();
            let count = tail[flags.len()..].trim().parse().ok();
            if cmd == "&&" || flags.starts_with('&') {
                flags = format!("{}{}", l.flags, flags.trim_start_matches('&'));
            }
            let pat = if cmd == "~" || flags.contains('r') {
                vim.last_search
                    .as_ref()
                    .map(|s| s.0.clone())
                    .unwrap_or(l.pattern.clone())
            } else {
                l.pattern.clone()
            };
            (pat, l.replacement.clone(), flags, count)
        }
    };
    if flags.starts_with('&') {
        if let Some(l) = &last {
            flags = format!("{}{}", l.flags, &flags[1..]);
        }
    }
    vim.last_sub = Some(Substitute {
        pattern: pattern.clone(),
        replacement: replacement.clone(),
        flags: flags.replace('&', ""),
    });
    vim.last_search = Some((pattern.clone(), true));
    vim.regs.last_search = pattern.clone();
    vim.hl_hidden = false;
    let mut global = flags.contains('g');
    if vim.opts.gdefault {
        global = !global;
    }
    let mut opts = vim.search_options();
    opts.magic = magic;
    if flags.contains('i') {
        opts.ignorecase = true;
        opts.smartcase = false;
    }
    if flags.contains('I') {
        opts.ignorecase = false;
    }
    let re = search::compile(&pattern, &opts)?;
    let (start_line, end_line) = match count {
        Some(n) => (range.end, range.end + n.max(1) - 1),
        None => (range.start, range.end),
    };
    let Some(ed) = host.ed() else {
        return Ok(());
    };
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
    // Search from the first line of the range, not the top of the buffer: a
    // match starting on the line above (`\n\n` at its end) must not swallow
    // one that starts in the range.
    let mut all = Vec::new();
    let mut pos = bs;
    while pos <= hay.len() {
        let Some(caps) = re.re.captures_at(&hay, pos) else {
            break;
        };
        let whole = caps.get(0).unwrap();
        if !in_block(whole.start()) {
            break;
        }
        pos = if whole.end() > whole.start() {
            whole.end()
        } else {
            whole.end() + hay[whole.end()..].chars().next().map_or(1, char::len_utf8)
        };
        all.push(caps);
    }
    for caps in all {
        let whole = caps.get(0).unwrap();
        let m = if re.zs {
            caps.name("vzs").unwrap_or(whole)
        } else {
            whole
        };
        if !in_block(m.start()) {
            continue;
        }
        let line = ed.rope.char_to_line(ed.rope.byte_to_char(m.start()));
        if line > end_line {
            break;
        }
        if !global && last_line_hit == Some(line) {
            continue;
        }
        // Empty match right after a previous match on the same spot.
        last_line_hit = Some(line);
        lines.insert(line);
        let rep = search::expand_replacement(&replacement, &caps, &re, magic);
        found.push((
            ed.rope.byte_to_char(m.start()),
            ed.rope.byte_to_char(m.end()),
            rep,
        ));
    }
    if found.is_empty() {
        if flags.contains('e') {
            return Ok(());
        }
        return Err(format!("E486: Pattern not found: {pattern}"));
    }
    if flags.contains('n') {
        vim.set_message(format!(
            "{} match{} on {} line{}",
            found.len(),
            if found.len() == 1 { "" } else { "es" },
            lines.len(),
            if lines.len() == 1 { "" } else { "s" }
        ));
        return Ok(());
    }
    if flags.contains('c') {
        let boundary = super::insert::undo_back_id(ed);
        vim.confirm = Some(Confirm {
            matches: found.iter().map(|(s, e, _)| (*s, *e)).collect(),
            pattern,
            replacement,
            boundary,
            done: 0,
            lines: Default::default(),
            all: false,
        });
        confirm_show(vim, host);
        return Ok(());
    }
    if let Some((s, _, _)) = found.first() {
        ed.cursor_line = ed.rope.char_to_line(*s);
        ed.cursor_col = 0;
    }
    let mut tx = Tx::default();
    // The cursor goes to the line where the last substitution's text ends,
    // counting lines added or removed by the earlier ones.
    let mut last_line = start_line;
    if let Some((s, _, last_rep)) = found.last() {
        let mut delta = last_rep.matches('\n').count() as isize;
        for (ms, me, rep) in &found[..found.len() - 1] {
            let removed = ed
                .rope
                .slice(*ms..*me)
                .chars()
                .filter(|&c| c == '\n')
                .count() as isize;
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
        vim.set_message(format!(
            "{n} substitution{} on {} line{}",
            if n == 1 { "" } else { "s" },
            lines.len(),
            if lines.len() == 1 { "" } else { "s" }
        ));
    }
    Ok(())
}

fn confirm_show(vim: &mut VimState, host: &mut dyn Host) {
    let Some(c) = &vim.confirm else {
        return;
    };
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
    let Some(c) = vim.confirm.take() else {
        return;
    };
    if let Some(ed) = host.ed() {
        // One undo step for the whole :s///c.
        let mut deque = ed.undo_stack.write();
        let idx = c
            .boundary
            .and_then(|id| deque.iter().rposition(|u| u.snapshot_id == id));
        let keep = idx.map_or(1, |i| i + 2);
        let merged = deque.len() > keep;
        if merged {
            deque.truncate(keep);
        }
        drop(deque);
        exec::clamp_normal(ed);
        if merged {
            // The merged step now ends where the whole command left the caret.
            let caret = ed.caret_state();
            if let Some(mut top) = ed.undo_stack.back_mut() {
                top.other_caret = Some(caret);
            }
        }
    }
    if c.done > 0 {
        vim.set_message(format!(
            "{} substitution{} on {} line{}",
            c.done,
            if c.done == 1 { "" } else { "s" },
            c.lines.len(),
            if c.lines.len() == 1 { "" } else { "s" }
        ));
    } else {
        vim.message = None;
    }
}

pub(crate) fn confirm_key(vim: &mut VimState, host: &mut dyn Host, key: Key) {
    let Some(c) = vim.confirm.as_mut() else {
        return;
    };
    if key.is_ctrl('e') || key.is_ctrl('y') {
        if let Some(ed) = host.ed() {
            if key.is_ctrl('e') {
                ed.scroll_offset += 1;
            } else {
                ed.scroll_offset = ed.scroll_offset.saturating_sub(1);
            }
        }
        return;
    }
    let choice = if key.is_escape() {
        'q'
    } else {
        key.char().unwrap_or('?')
    };
    let replace_one = |vim: &mut VimState, host: &mut dyn Host| -> R<()> {
        let c = vim.confirm.as_mut().ok_or("")?;
        let (s, e) = c.matches.remove(0);
        let pattern = c.pattern.clone();
        let replacement = c.replacement.clone();
        let opts = vim.search_options();
        let re = search::compile(&pattern, &opts)?;
        let Some(ed) = host.ed() else {
            return Ok(());
        };
        let matched = ed.rope.slice(s..e).to_string();
        let caps = re.re.captures(&matched);
        let rep = match caps {
            Some(caps) => search::expand_replacement(&replacement, &caps, &re, true),
            None => replacement,
        };
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
        'y' => {
            let _ = replace_one(vim, host);
        }
        'l' => {
            let _ = replace_one(vim, host);
            if let Some(c) = vim.confirm.as_mut() {
                c.matches.clear();
            }
        }
        'n' => {
            c.matches.remove(0);
        }
        'a' => {
            c.all = true;
            while vim.confirm.as_ref().is_some_and(|c| !c.matches.is_empty()) {
                if replace_one(vim, host).is_err() {
                    break;
                }
            }
        }
        'q' => {
            c.matches.clear();
        }
        _ => return,
    }
    confirm_show(vim, host);
}

// ---------------------------------------------------------------------------
// :global and :normal

fn global(
    vim: &mut VimState,
    host: &mut dyn Host,
    range: &Range,
    invert: bool,
    arg: &str,
) -> R<()> {
    let arg = arg.trim_start();
    let delim = arg.chars().next().ok_or("E476: Invalid command")?;
    let rest = &arg[delim.len_utf8()..];
    let (pat, after) = super::cmdline::split_pattern(rest, delim);
    let cmd = after.unwrap_or_default();
    let cmd = if cmd.trim().is_empty() {
        "p".to_string()
    } else {
        cmd
    };
    let pat = if pat.is_empty() {
        vim.last_search
            .as_ref()
            .map(|s| s.0.clone())
            .ok_or("E35: No previous regular expression")?
    } else {
        pat
    };
    vim.last_search = Some((pat.clone(), true));
    vim.regs.last_search = pat.clone();
    let re = search::compile(&pat, &vim.search_options())?;
    let Some(ed) = host.ed() else {
        return Ok(());
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let (s, e) = if range.given == 0 {
        (0, t.last_line())
    } else {
        (range.start, range.end)
    };
    let lines: Vec<usize> = (s..=e)
        .filter(|&l| re.re.is_match(&t.line_string(l)) != invert)
        .collect();
    if lines.is_empty() {
        vim.set_message(format!(
            "Pattern {}found: {pat}",
            if invert {
                "found in every line"
            } else {
                "not "
            }
        ));
        return Ok(());
    }
    ed.vim_buf.global_lines = lines;
    let boundary = super::insert::undo_back_id(ed);
    let mut printed = Vec::new();
    loop {
        let Some(ed) = host.ed() else {
            break;
        };
        if ed.vim_buf.global_lines.is_empty() {
            break;
        }
        let line = ed.vim_buf.global_lines.remove(0);
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        if line > t.last_line() {
            continue;
        }
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
        if let Err(e) = r {
            vim.set_error(e);
            break;
        }
        if let Some(ed) = host.ed() {
            let after = Text::of(&ed.rope, ed.vim_buf.trailing_empty).lines();
            if ed.vim_buf.shift_rev == rev && after != before_lines {
                let delta = after as isize - before_lines as isize;
                for l in &mut ed.vim_buf.global_lines {
                    if *l > line {
                        *l = (*l as isize + delta).max(0) as usize;
                    }
                }
            }
        }
    }
    if let Some(ed) = host.ed() {
        ed.vim_buf.global_lines.clear();
        let mut deque = ed.undo_stack.write();
        let idx = boundary.and_then(|id| deque.iter().rposition(|u| u.snapshot_id == id));
        let keep = idx.map_or(1, |i| i + 2);
        let merged = deque.len() > keep;
        if merged {
            deque.truncate(keep);
        }
        drop(deque);
        exec::clamp_normal(ed);
        if merged {
            // The merged step now ends where the whole command left the caret.
            let caret = ed.caret_state();
            if let Some(mut top) = ed.undo_stack.back_mut() {
                top.other_caret = Some(caret);
            }
        }
    }
    if !printed.is_empty() {
        vim.set_lines(printed);
    }
    Ok(())
}

fn normal(vim: &mut VimState, host: &mut dyn Host, range: &Range, arg: &str, remap: bool) -> R<()> {
    let arg = arg.strip_prefix(' ').unwrap_or(arg);
    if arg.is_empty() {
        return Err("E471: Argument required".into());
    }
    // Each level runs synchronously; Vim stops runaway recursion the same way.
    if vim.normal_depth >= 20 {
        return Err("E192: Recursive use of :normal too deep".into());
    }
    vim.normal_depth += 1;
    let r = normal_lines(vim, host, range, arg, remap);
    vim.normal_depth -= 1;
    r
}

fn normal_lines(
    vim: &mut VimState,
    host: &mut dyn Host,
    range: &Range,
    arg: &str,
    remap: bool,
) -> R<()> {
    // Raw control characters come from `:execute "normal! A\<Esc>"`: take
    // them literally, as Vim does.
    let keys = if arg.chars().any(|c| (c as u32) < 0x20 || c == '\u{E000}') {
        super::ops::text_to_keys(arg)
    } else {
        super::keys::parse(arg, &vim.leader.clone())
    };
    let lines: Vec<usize> = if range.given > 0 {
        (range.start..=range.end).collect()
    } else {
        vec![usize::MAX]
    };
    let multi = range.given > 0;
    for line in lines {
        if line != usize::MAX {
            let Some(ed) = host.ed() else {
                break;
            };
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            if line > t.last_line() {
                continue;
            }
            ed.cursor_line = line;
            ed.cursor_col = 0;
        }
        vim.replaying += 1;
        vim.feed_keys(host, &keys, remap, true);
        // An incomplete command is aborted, Insert mode ends.
        vim.pending.clear();
        if vim.is_insert() {
            super::insert::finish(vim, host, true);
        }
        if vim.mode == Mode::Cmdline {
            super::cmdline::cancel(vim, host);
        }
        vim.replaying -= 1;
        if !multi {
            break;
        }
    }
    if vim.is_visual() {
        exec::exit_visual(vim, host);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// :sort, :retab, alignment

fn sort(vim: &mut VimState, host: &mut dyn Host, range: &Range, reverse: bool, arg: &str) -> R<()> {
    if read_only(host) {
        return Err("E21".into());
    }
    let Some(ed) = host.ed() else {
        return Ok(());
    };
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let (s, e) = if range.given == 0 {
        (0, t.last_line())
    } else {
        (range.start, range.end)
    };
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
        if c != ' ' {
            flags.push(c);
        }
    }
    let re = match &pat {
        Some(p) if !p.is_empty() => Some(search::compile(p, &vim.search_options())?),
        Some(_) => vim
            .last_search
            .as_ref()
            .and_then(|(p, _)| search::compile(p, &vim.search_options()).ok()),
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
                    Some((ms, me)) => {
                        if use_match {
                            l[ms..me].to_string()
                        } else {
                            l[me..].to_string()
                        }
                    }
                    None => {
                        if use_match {
                            String::new()
                        } else {
                            l.to_string()
                        }
                    }
                }
            }
            None => l.to_string(),
        }
    };
    let num = |k: &str| -> f64 {
        if hex {
            let h: String = k
                .chars()
                .skip_while(|c| !c.is_ascii_hexdigit())
                .take_while(|c| c.is_ascii_hexdigit() || *c == 'x')
                .collect();
            i64::from_str_radix(h.trim_start_matches("0x"), 16)
                .map(|v| v as f64)
                .unwrap_or(f64::MIN)
        } else {
            let start = k
                .find(|c: char| c.is_ascii_digit() || (c == '-' && float))
                .unwrap_or(k.len());
            let mut digits = String::new();
            let mut neg = false;
            if start > 0 && k[..start].ends_with('-') {
                neg = true;
            }
            for c in k[start..].chars() {
                if c.is_ascii_digit()
                    || (float && (c == '.' || c == 'e' || c == '-' && digits.is_empty()))
                {
                    digits.push(c);
                } else {
                    break;
                }
            }
            if digits.is_empty() {
                return f64::MIN;
            }
            let v: f64 = digits.parse().unwrap_or(f64::MIN);
            if neg { -v } else { v }
        }
    };
    if numeric || float || hex {
        lines.sort_by(|a, b| {
            num(&key(a))
                .partial_cmp(&num(&key(b)))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    } else if icase {
        lines.sort_by_key(|l| key(l).to_lowercase());
    } else {
        lines.sort_by_key(|l| key(l));
    }
    if reverse {
        lines.reverse();
    }
    if unique {
        lines.dedup_by(|a, b| {
            if icase {
                a.to_lowercase() == b.to_lowercase()
            } else {
                a == b
            }
        });
    }
    let start = t.line_start(s);
    let end = t.line_end(e);
    let new = lines.join("\n");
    let mut tx = Tx::default();
    if t.slice(start, end) != new {
        tx.edit(ed, start, end, &new);
    }
    tx.finish(ed);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    set_cursor(ed, Pos::new(s, t.first_nonblank(s)));
    Ok(())
}

fn retab(vim: &mut VimState, host: &mut dyn Host, range: &Range, bang: bool, arg: &str) -> R<()> {
    let Some(ed) = host.ed() else {
        return Ok(());
    };
    if let Ok(n) = arg.parse::<usize>() {
        if n > 0 {
            ed.tab_size = n;
        }
    }
    let ts = ed.tab_size.max(1);
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let (s, e) = if range.given == 0 {
        (0, t.last_line())
    } else {
        (range.start, range.end)
    };
    let mut tx = Tx::default();
    for line in (s..=e).rev() {
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        let chars = t.line_chars(line);
        let ws = chars.iter().take_while(|&&c| c == ' ' || c == '\t').count();
        if !(chars[..ws].contains(&'\t') || bang && ws > 0) {
            continue;
        }
        let width = t.vcol(line, ws, ts);
        let new = if ed.use_tabs {
            format!("{}{}", "\t".repeat(width / ts), " ".repeat(width % ts))
        } else {
            " ".repeat(width)
        };
        let base = t.line_start(line);
        if chars[..ws].iter().collect::<String>() != new {
            tx.edit(ed, base, base + ws, &new);
        }
    }
    tx.finish(ed);
    let _ = vim;
    Ok(())
}

fn align(vim: &mut VimState, host: &mut dyn Host, range: &Range, how: &str, arg: &str) -> R<()> {
    let Some(ed) = host.ed() else {
        return Ok(());
    };
    let width: usize = arg.parse().unwrap_or(if vim.opts.textwidth > 0 {
        vim.opts.textwidth
    } else {
        80
    });
    let mut tx = Tx::default();
    for line in range.start..=range.end {
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        let text = t.line_string(line);
        let body = text.trim();
        let new = match how {
            "left" => format!("{}{}", " ".repeat(arg.parse().unwrap_or(0)), body),
            "right" => format!(
                "{}{}",
                " ".repeat(width.saturating_sub(body.chars().count())),
                body
            ),
            _ => format!(
                "{}{}",
                " ".repeat(width.saturating_sub(body.chars().count()) / 2),
                body
            ),
        };
        let (s, e) = (t.line_start(line), t.line_end(line));
        if text != new {
            tx.edit(ed, s, e, &new);
        }
    }
    tx.finish(ed);
    Ok(())
}

// ---------------------------------------------------------------------------
// :set, :let, mappings, vimrc

fn map_cmd(vim: &mut VimState, modes: &[MapMode], noremap: bool, bang: bool, arg: &str) -> R<()> {
    let modes: Vec<MapMode> = if bang {
        vec![MapMode::Insert, MapMode::Cmdline]
    } else {
        modes.to_vec()
    };
    let (silent, nowait, lhs, rhs) = map::parse_args(arg);
    let leader = vim.leader.clone();
    if lhs.is_empty() {
        let lines = vim.maps.list(&modes, &[]);
        if lines.is_empty() {
            vim.set_message("No mapping found");
        } else {
            vim.set_lines(lines);
        }
        return Ok(());
    }
    let lhs_keys = map::map_keys(lhs, &leader);
    if rhs.is_empty() {
        let lines = vim.maps.list(&modes, &lhs_keys);
        if lines.is_empty() {
            vim.set_message("No mapping found");
        } else {
            vim.set_lines(lines);
        }
        return Ok(());
    }
    if lhs_keys.is_empty() {
        return Err("E474: Invalid argument".into());
    }
    let rhs_keys = map::map_keys(rhs, &leader);
    vim.maps.define(
        &modes,
        lhs_keys,
        Mapping {
            rhs: rhs_keys,
            noremap,
            nowait,
            silent,
            source: rhs.to_string(),
        },
    );
    Ok(())
}

fn let_cmd(vim: &mut VimState, host: &mut dyn Host, arg: &str) -> R<()> {
    let arg = arg.trim();
    if arg.is_empty() {
        let lines: Vec<String> = vim
            .vars
            .iter()
            .filter(|(k, _)| !k.starts_with("v:"))
            .map(|(k, v)| format!("{k:<20} {}", v.repr()))
            .collect();
        if lines.is_empty() {
            vim.set_message("No variables");
        } else {
            vim.set_lines(lines);
        }
        return Ok(());
    }
    let Some((target, op, rhs)) = split_assign(arg) else {
        // `:let name`: show it.
        let mut cx = super::eval::Ctx::new(vim, host);
        let v = super::eval::eval(arg, &mut cx)?;
        vim.set_message(format!("{arg:<20} {}", v.repr()));
        return Ok(());
    };
    let target = target.trim();
    let value = {
        let mut cx = super::eval::Ctx::new(vim, host);
        super::eval::eval(rhs.trim(), &mut cx)?
    };
    // `:let [a, b] = [1, 2]`
    if let Some(inner) = target.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
        let names: Vec<&str> = inner
            .split(',')
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .collect();
        let super::eval::Val::List(items) = value else {
            return Err("E714: List required".into());
        };
        if items.len() != names.len() && !names.iter().any(|n| n.starts_with(';')) {
            return Err("E687: Less targets than List items".into());
        }
        for (n, v) in names.iter().zip(items) {
            let_one(vim, host, n, op, v)?;
        }
        return Ok(());
    }
    let_one(vim, host, target, op, value)
}

/// Split `target op= value` at the assignment outside strings and brackets.
fn split_assign(arg: &str) -> Option<(&str, &'static str, &str)> {
    let b = arg.as_bytes();
    let mut depth = 0i32;
    let mut quote: Option<u8> = None;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if let Some(q) = quote {
            if c == b'\\' && q == b'"' {
                i += 2;
                continue;
            }
            if c == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' | b'\'' => quote = Some(c),
            b'[' | b'(' | b'{' => depth += 1,
            b']' | b')' | b'}' => depth -= 1,
            b'=' if depth == 0 => {
                if b.get(i + 1).is_some_and(|n| *n == b'=' || *n == b'~') {
                    return None;
                }
                let (op, start) = if i >= 2 && &b[i - 2..i] == b".." {
                    ("..=", i - 2)
                } else {
                    match i.checked_sub(1).map(|p| b[p]) {
                        Some(b'+') => ("+=", i - 1),
                        Some(b'-') => ("-=", i - 1),
                        Some(b'*') => ("*=", i - 1),
                        Some(b'/') => ("/=", i - 1),
                        Some(b'%') => ("%=", i - 1),
                        Some(b'.') => (".=", i - 1),
                        Some(b'!' | b'<' | b'>') => return None,
                        _ => ("=", i),
                    }
                };
                return Some((&arg[..start], op, &arg[i + 1..]));
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn combine(old: super::eval::Val, op: &str, v: super::eval::Val) -> R<super::eval::Val> {
    use super::eval::Val;
    Ok(match op {
        "=" => v,
        ".=" | "..=" => Val::Str(old.to_str() + &v.to_str()),
        "+=" => match (old, v) {
            (Val::List(mut a), Val::List(b)) => {
                a.extend(b);
                Val::List(a)
            }
            (Val::Float(a), b) => Val::Float(a + b.to_num() as f64),
            (a, Val::Float(b)) => Val::Float(a.to_num() as f64 + b),
            (a, b) => Val::Num(a.to_num() + b.to_num()),
        },
        "-=" => Val::Num(old.to_num() - v.to_num()),
        "*=" => Val::Num(old.to_num() * v.to_num()),
        "/=" => {
            let d = v.to_num();
            Val::Num(if d == 0 { 0 } else { old.to_num() / d })
        }
        "%=" => {
            let d = v.to_num();
            Val::Num(if d == 0 { 0 } else { old.to_num() % d })
        }
        _ => v,
    })
}

fn let_one(
    vim: &mut VimState,
    host: &mut dyn Host,
    target: &str,
    op: &str,
    value: super::eval::Val,
) -> R<()> {
    use super::eval::Val;
    let current = |vim: &mut VimState, host: &mut dyn Host| -> Val {
        let mut cx = super::eval::Ctx::new(vim, host);
        super::eval::eval(target, &mut cx).unwrap_or(Val::Str(String::new()))
    };
    let value = if op == "=" {
        value
    } else {
        let old = current(vim, host);
        combine(old, op, value)?
    };
    match target {
        "mapleader" | "g:mapleader" => {
            let keys = super::keys::parse(&value.to_str(), &[]);
            vim.leader = if keys.is_empty() {
                vec![Key::ch('\\')]
            } else {
                keys
            };
            super::eval::set_var(vim, "mapleader", value);
            Ok(())
        }
        "maplocalleader" | "g:maplocalleader" => {
            super::eval::set_var(vim, "maplocalleader", value);
            Ok(())
        }
        t if t.starts_with('@') && t.chars().count() == 2 => {
            let r = t.chars().nth(1).unwrap();
            let r = if r == '@' { '"' } else { r };
            let text = value.to_text();
            let (text, kind) = match &value {
                Val::List(_) => (format!("{text}\n"), RegKind::Line),
                _ if text.ends_with('\n') => (text, RegKind::Line),
                _ => (text, RegKind::Char),
            };
            let mut clip = HostClip(host);
            vim.regs.set(r, Reg::new(text, kind), &mut clip);
            Ok(())
        }
        t if t.starts_with('&') => {
            let name = t[1..]
                .trim_start_matches("l:")
                .trim_start_matches("g:")
                .to_string();
            let v = value.to_str();
            let escaped = v.replace('\\', "\\\\").replace(' ', "\\ ");
            match set(vim, host, &format!("{name}={escaped}")) {
                Ok(()) => Ok(()),
                Err(_) if matches!(value, Val::Num(_)) => {
                    let flag = if value.truthy() {
                        name.clone()
                    } else {
                        format!("no{name}")
                    };
                    set(vim, host, &flag)
                }
                Err(e) => Err(e),
            }
        }
        // Environment variables are left alone: other threads read them.
        t if t.starts_with('$') => Ok(()),
        t if t.ends_with(']') && t.contains('[') => {
            let open = t.find('[').unwrap();
            let base = &t[..open];
            let key_src = &t[open + 1..t.len() - 1];
            let key = {
                let mut cx = super::eval::Ctx::new(vim, host);
                super::eval::eval(key_src, &mut cx)?
            };
            let mut container = {
                let mut cx = super::eval::Ctx::new(vim, host);
                super::eval::eval(base, &mut cx)?
            };
            match &mut container {
                Val::List(l) => {
                    let n = key.to_num();
                    let i = if n < 0 { l.len() as i64 + n } else { n };
                    let slot = l
                        .get_mut(i as usize)
                        .filter(|_| i >= 0)
                        .ok_or_else(|| format!("E684: List index out of range: {n}"))?;
                    *slot = value;
                }
                Val::Dict(d) => {
                    d.insert(key.to_str(), value);
                }
                _ => return Err("E689: Can only index a List or Dictionary".into()),
            }
            super::eval::set_var(vim, base, container);
            Ok(())
        }
        t if t
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == ':' || c == '#' || c == '.') =>
        {
            if let Some((base, key)) = t.split_once('.')
                && !base.is_empty()
                && !key.is_empty()
                && !base.ends_with(':')
            {
                let mut cx = super::eval::Ctx::new(vim, host);
                if let Ok(Val::Dict(mut d)) = super::eval::eval(base, &mut cx) {
                    d.insert(key.to_string(), value);
                    super::eval::set_var(vim, base, Val::Dict(d));
                    return Ok(());
                }
            }
            super::eval::set_var(vim, t, value);
            Ok(())
        }
        t => Err(format!("E461: Illegal variable name: {t}")),
    }
}

fn echo(vim: &mut VimState, host: &mut dyn Host, full: &str, arg: &str) -> R<()> {
    let mut parts = Vec::new();
    let mut rest = arg.to_string();
    while !rest.trim().is_empty() {
        let mut cx = super::eval::Ctx::new(vim, host);
        let (v, r) = super::eval::eval_prefix(&rest, &mut cx)?;
        parts.push(v.to_str());
        if r.len() == rest.len() {
            break;
        }
        rest = r;
    }
    let text = parts.join(if full == "echon" { "" } else { " " });
    if full == "echoerr" {
        vim.set_error(text);
        return Ok(());
    }
    if text.contains('\n') {
        vim.set_lines(text.lines().map(str::to_string).collect());
    } else {
        vim.set_message(text);
    }
    Ok(())
}

fn call_cmd(vim: &mut VimState, host: &mut dyn Host, range: &Range, arg: &str) -> R<()> {
    let arg = arg.trim();
    if arg.is_empty() {
        return Err("E471: Argument required".into());
    }
    if range.given > 0
        && let Some(open) = arg.find('(')
    {
        let name = arg[..open].trim();
        if vim.funcs.contains_key(&super::script::normalize_name(name)) {
            let inner = arg[open + 1..].trim_end().trim_end_matches(')');
            let args = {
                let mut cx = super::eval::Ctx::new(vim, host);
                super::eval::eval(&format!("[{inner}]"), &mut cx)?
            };
            let super::eval::Val::List(args) = args else {
                return Ok(());
            };
            return super::script::call(vim, host, name, args, Some((range.start, range.end)))
                .unwrap_or(Ok(super::eval::Val::Num(0)))
                .map(|_| ());
        }
    }
    let mut cx = super::eval::Ctx::new(vim, host);
    super::eval::eval(arg, &mut cx).map(|_| ())
}

#[derive(Clone, Debug)]
pub(crate) struct Abbrev {
    pub(crate) lhs: String,
    pub(crate) rhs: String,
    pub(crate) insert: bool,
    pub(crate) cmdline: bool,
    pub(crate) noremap: bool,
    /// `<expr>`: read once abbreviations expand while typing.
    #[allow(dead_code)]
    pub(crate) expr: bool,
}

fn abbrev_cmd(vim: &mut VimState, full: &str, arg: &str) -> R<()> {
    let insert = !full.starts_with('c');
    let cmdline = !full.starts_with('i');
    let noremap = full.contains("nore");
    let mut rest = arg.trim_start();
    let mut expr = false;
    loop {
        let lower = rest.get(..9).unwrap_or(rest).to_ascii_lowercase();
        let Some(s) = ["<expr>", "<buffer>", "<silent>", "<script>", "<special>"]
            .into_iter()
            .find(|s| lower.starts_with(s))
        else {
            break;
        };
        expr |= s == "<expr>";
        rest = rest[s.len()..].trim_start();
    }
    let (lhs, rhs) = match rest.find([' ', '\t']) {
        Some(i) => (&rest[..i], rest[i..].trim()),
        None => (rest, ""),
    };
    if rhs.is_empty() {
        let lines: Vec<String> = vim
            .abbrevs
            .iter()
            .filter(|a| lhs.is_empty() || a.lhs.starts_with(lhs))
            .filter(|a| (insert && a.insert) || (cmdline && a.cmdline))
            .map(|a| {
                let m = match (a.insert, a.cmdline) {
                    (true, true) => '!',
                    (true, false) => 'i',
                    _ => 'c',
                };
                format!(
                    "{m}  {:<10} {}{}",
                    a.lhs,
                    if a.noremap { "* " } else { "  " },
                    a.rhs
                )
            })
            .collect();
        if lines.is_empty() {
            vim.set_message("No abbreviation found");
        } else {
            vim.set_lines(lines);
        }
        return Ok(());
    }
    vim.abbrevs
        .retain(|a| !(a.lhs == lhs && a.insert == insert && a.cmdline == cmdline));
    vim.abbrevs.push(Abbrev {
        lhs: lhs.to_string(),
        rhs: rhs.to_string(),
        insert,
        cmdline,
        noremap,
        expr,
    });
    Ok(())
}

#[derive(Clone, Debug)]
pub(crate) struct UserCommand {
    pub(crate) rhs: String,
    pub(crate) nargs: String,
    pub(crate) range: bool,
}

/// `:command[!] [-nargs=..] [-range] [-bang] ... Name {rhs}`
fn user_command(vim: &mut VimState, _bang: bool, arg: &str) -> R<()> {
    let mut rest = arg.trim();
    let mut nargs = "0".to_string();
    let mut range = false;
    while let Some(r) = rest.strip_prefix('-') {
        let end = r.find([' ', '\t']).unwrap_or(r.len());
        let attr = &r[..end];
        if let Some(n) = attr.strip_prefix("nargs=") {
            nargs = n.to_string();
        } else if attr.starts_with("range") || attr.starts_with("count") {
            range = true;
        }
        rest = r[end..].trim_start();
    }
    let (name, rhs) = match rest.find([' ', '\t']) {
        Some(i) => (&rest[..i], rest[i..].trim()),
        None => (rest, ""),
    };
    if name.is_empty() {
        let lines: Vec<String> = vim
            .user_commands
            .iter()
            .map(|(n, c)| format!("    {n:<12} {:<5} {}", c.nargs, c.rhs))
            .collect();
        if lines.is_empty() {
            vim.set_message("No user-defined commands found");
        } else {
            vim.set_lines(lines);
        }
        return Ok(());
    }
    if !name.starts_with(|c: char| c.is_ascii_uppercase()) {
        return Err("E183: User defined commands must start with an uppercase letter".into());
    }
    if rhs.is_empty() {
        return Err("E471: Argument required".into());
    }
    vim.user_commands.insert(
        name.to_string(),
        UserCommand {
            rhs: rhs.to_string(),
            nargs,
            range,
        },
    );
    Ok(())
}

/// Run a user command; `None` when `name` is not one.
fn run_user_command(
    vim: &mut VimState,
    host: &mut dyn Host,
    range: &Range,
    name: &str,
    bang: bool,
    arg: &str,
) -> Option<R<()>> {
    let cmd = vim.user_commands.get(name).cloned().or_else(|| {
        let mut it = vim
            .user_commands
            .iter()
            .filter(|(n, _)| n.starts_with(name));
        let first = it.next()?;
        it.next().is_none().then(|| first.1.clone())
    })?;
    let arg = arg.trim();
    if cmd.nargs == "0" && !arg.is_empty() {
        return Some(Err("E488: Trailing characters".into()));
    }
    if cmd.nargs == "1" && arg.is_empty() {
        return Some(Err("E471: Argument required".into()));
    }
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let f_args: Vec<String> = arg.split_whitespace().map(q).collect();
    let mut out = String::new();
    let mut i = 0;
    let rhs = cmd.rhs.as_str();
    while i < rhs.len() {
        let rest = &rhs[i..];
        let lower = rest.to_ascii_lowercase();
        let rep: Option<(usize, String)> = [
            ("<args>", arg.to_string()),
            ("<q-args>", q(arg)),
            ("<f-args>", f_args.join(", ")),
            ("<bang>", if bang { "!".into() } else { String::new() }),
            ("<line1>", (range.start + 1).to_string()),
            ("<line2>", (range.end + 1).to_string()),
            (
                "<count>",
                if range.given > 0 {
                    (range.end + 1).to_string()
                } else {
                    "0".into()
                },
            ),
            ("<range>", range.given.to_string()),
            ("<lt>", "<".into()),
            ("<reg>", String::new()),
            ("<mods>", String::new()),
        ]
        .into_iter()
        .find(|(k, _)| lower.starts_with(k))
        .map(|(k, v)| (k.len(), v));
        match rep {
            Some((n, v)) => {
                out.push_str(&v);
                i += n;
            }
            None => {
                let c = rest.chars().next().unwrap();
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    let line = if cmd.range && range.given > 0 {
        format!("{},{}{out}", range.start + 1, range.end + 1)
    } else {
        out
    };
    Some(run(vim, host, &line))
}

fn earlier_later(vim: &mut VimState, host: &mut dyn Host, earlier: bool, arg: &str) -> R<()> {
    let arg = arg.trim();
    let digits: String = arg.chars().take_while(char::is_ascii_digit).collect();
    let n: u64 = if digits.is_empty() {
        1
    } else {
        digits.parse().unwrap_or(1)
    };
    let unit = arg[digits.len()..].trim();
    let undo_key = if earlier {
        Key::ch('u')
    } else {
        Key::ctrl('r')
    };
    match unit {
        "" => {
            let keys: Vec<Key> = std::iter::once(n.to_string())
                .flat_map(|s| s.chars().map(Key::ch).collect::<Vec<_>>())
                .chain(std::iter::once(undo_key))
                .collect();
            vim.feed_keys(host, &keys, false, false);
            Ok(())
        }
        "f" => {
            for k in 0..n {
                let Some(ed) = host.ed() else {
                    return Ok(());
                };
                let at_saved = |ed: &crate::editor::Editor| {
                    ed.saved_rope.as_ref().is_some_and(|r| *r == ed.rope)
                };
                let mut steps = 0;
                // The first file write back is the saved text itself.
                if at_saved(ed) || k > 0 {
                    if !step_history(ed, earlier) {
                        break;
                    }
                    steps += 1;
                }
                while !at_saved(ed) && step_history(ed, earlier) {
                    steps += 1;
                }
                let _ = steps;
            }
            if let Some(ed) = host.ed() {
                exec::clamp_normal(ed);
            }
            Ok(())
        }
        "s" | "m" | "h" | "d" => {
            let secs = n * match unit {
                "s" => 1,
                "m" => 60,
                "h" => 3600,
                _ => 86400,
            };
            let span = std::time::Duration::from_secs(secs);
            let Some(ed) = host.ed() else {
                return Ok(());
            };
            let time_of =
                |ed: &crate::editor::Editor, id: u64| ed.vim_buf.undo_times.get(&id).copied();
            if earlier {
                let target = std::time::Instant::now().checked_sub(span);
                loop {
                    let Some(id) = ed.undo_stack.back().map(|s| s.change_id) else {
                        break;
                    };
                    let newer = match (time_of(ed, id), target) {
                        (Some(t), Some(target)) => t > target,
                        (Some(_), None) => true,
                        (None, _) => false,
                    };
                    if !newer || !step_history(ed, true) {
                        break;
                    }
                }
            } else {
                let now_state = ed
                    .undo_stack
                    .back()
                    .and_then(|s| time_of(ed, s.change_id))
                    .unwrap_or_else(std::time::Instant::now);
                let target = now_state + span;
                loop {
                    let Some(id) = ed.redo_stack.back().map(|s| s.change_id) else {
                        break;
                    };
                    let within = time_of(ed, id).is_none_or(|t| t <= target);
                    if !within || !step_history(ed, false) {
                        break;
                    }
                }
            }
            exec::clamp_normal(ed);
            Ok(())
        }
        _ => Err(format!("E475: Invalid argument: {arg}")),
    }
}

fn step_history(ed: &mut crate::editor::Editor, back: bool) -> bool {
    if back {
        if !ed.can_undo() {
            return false;
        }
        ed.undo();
    } else {
        if !ed.can_redo() {
            return false;
        }
        ed.redo();
    }
    true
}

// ---------------------------------------------------------------------------
// Quickfix

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QfItem {
    pub(crate) path: std::path::PathBuf,
    /// 0-based.
    pub(crate) line: usize,
    pub(crate) col: usize,
    pub(crate) text: String,
}

#[derive(Clone, Copy)]
enum QfMove {
    Next(usize),
    Prev(usize),
    /// 1-based.
    Nth(usize),
    Last,
    Current,
    File(bool),
}

fn set_quickfix(vim: &mut VimState, items: Vec<QfItem>, add: bool) {
    if add {
        vim.qf.extend(items);
    } else {
        vim.qf = items;
        vim.qf_idx = 0;
    }
}

fn qf_jump(vim: &mut VimState, host: &mut dyn Host, mv: QfMove) -> R<()> {
    if vim.qf.is_empty() {
        return Err("E42: No Errors".into());
    }
    let len = vim.qf.len();
    let idx = match mv {
        QfMove::Next(n) => {
            if vim.qf_idx + 1 >= len {
                return Err("E553: No more items".into());
            }
            (vim.qf_idx + n).min(len - 1)
        }
        QfMove::Prev(n) => {
            if vim.qf_idx == 0 {
                return Err("E553: No more items".into());
            }
            vim.qf_idx.saturating_sub(n)
        }
        QfMove::Nth(n) => n.clamp(1, len) - 1,
        QfMove::Last => len - 1,
        QfMove::Current => vim.qf_idx.min(len - 1),
        QfMove::File(next) => {
            let cur = vim.qf[vim.qf_idx].path.clone();
            let found = if next {
                (vim.qf_idx + 1..len).find(|&i| vim.qf[i].path != cur)
            } else {
                // The last entry of the previous file.
                (0..vim.qf_idx).rev().find(|&i| vim.qf[i].path != cur)
            };
            found.ok_or("E553: No more items")?
        }
    };
    vim.qf_idx = idx;
    let item = vim.qf[idx].clone();
    if let Some(ed) = host.ed() {
        push_jump(vim, ed);
    }
    host.act(Act::JumpTo {
        nav_id: 0,
        path: Some(item.path.clone()),
        pos: Pos::new(item.line, item.col),
    })?;
    if let Some(ed) = host.ed() {
        update_curswant(vim, ed);
    }
    vim.set_message(format!("({} of {len}): {}", idx + 1, item.text.trim()));
    Ok(())
}

fn qf_list(vim: &mut VimState, _all: bool) {
    let lines: Vec<String> = vim
        .qf
        .iter()
        .enumerate()
        .map(|(i, q)| {
            format!(
                "{}{:>3} {}:{} col {}: {}",
                if i == vim.qf_idx { ">" } else { " " },
                i + 1,
                q.path.display().to_string().replace('\\', "/"),
                q.line + 1,
                q.col + 1,
                q.text.trim()
            )
        })
        .collect();
    vim.set_lines(lines);
}

fn qf_do(vim: &mut VimState, host: &mut dyn Host, per_file: bool, cmd: &str) -> R<()> {
    if vim.qf.is_empty() {
        return Err("E42: No Errors".into());
    }
    let mut targets = Vec::new();
    for (i, q) in vim.qf.iter().enumerate() {
        if per_file && i > 0 && vim.qf[i - 1].path == q.path {
            continue;
        }
        targets.push(i);
    }
    for i in targets {
        qf_jump(vim, host, QfMove::Nth(i + 1))?;
        run(vim, host, cmd)?;
    }
    Ok(())
}

/// Which program runs `:grep`: 'grepprg' when set, else ripgrep or git grep
/// when available, else the platform default.
fn grep_program(vim: &VimState, host: &dyn Host) -> String {
    if let Some(p) = vim.other_opts.get("grepprg") {
        return p.clone();
    }
    if super::eval::which("rg") {
        return "rg --vimgrep --smart-case".into();
    }
    if host.cwd().is_some_and(|d| d.join(".git").exists()) && super::eval::which("git") {
        return "git grep -n --column".into();
    }
    default_other("grepprg").into()
}

fn quickfix_shell(
    vim: &mut VimState,
    host: &mut dyn Host,
    make: bool,
    add: bool,
    bang: bool,
    arg: &str,
) -> R<()> {
    let prg = if make {
        vim.other_opts
            .get("makeprg")
            .cloned()
            .unwrap_or_else(|| default_other("makeprg").into())
    } else {
        if arg.trim().is_empty() {
            return Err("E471: Argument required".into());
        }
        grep_program(vim, host)
    };
    let cmd = if prg.contains("$*") {
        prg.replace("$*", arg)
    } else {
        format!("{prg} {arg}")
    };
    let out = match shell(host, cmd.trim(), None) {
        Ok(o) => o,
        Err(e) => return Err(e),
    };
    let cwd = host.cwd().unwrap_or_default();
    let items = parse_errors(&out, cwd);
    let n = items.len();
    set_quickfix(vim, items, add);
    if n == 0 {
        if make {
            show_output(vim, &out);
            return Ok(());
        }
        return Err(format!("E480: No match: {}", arg.trim()));
    }
    if !bang {
        qf_jump(
            vim,
            host,
            QfMove::Nth(if add { vim.qf.len() - n + 1 } else { 1 }),
        )?;
    } else {
        vim.set_message(format!("{n} item{}", if n == 1 { "" } else { "s" }));
    }
    Ok(())
}

/// Read `file:line:col: text`, `file:line: text`, `file(line,col): text`
/// and rustc's ` --> file:line:col` lines into quickfix entries.
pub(crate) fn parse_errors(out: &str, cwd: std::path::PathBuf) -> Vec<QfItem> {
    use std::sync::LazyLock;
    static COLON: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^(.+?):(\d+)(?::(\d+))?:?\s?(.*)$").unwrap());
    static PAREN: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^(.+?)\((\d+)(?:,(\d+))?\)\s*:\s*(.*)$").unwrap());
    static ARROW: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"^\s*--> (.+?):(\d+):(\d+)\s*$").unwrap());
    let resolve = |f: &str| -> Option<std::path::PathBuf> {
        let f = f.trim();
        if f.is_empty() || f.len() > 400 {
            return None;
        }
        let p = std::path::Path::new(f);
        let full = if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        };
        full.is_file().then_some(full)
    };
    let mut items = Vec::new();
    let mut last_message = String::new();
    for line in out.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(c) = ARROW.captures(line) {
            if let Some(path) = resolve(&c[1]) {
                items.push(QfItem {
                    path,
                    line: c[2].parse::<usize>().unwrap_or(1).saturating_sub(1),
                    col: c[3].parse::<usize>().unwrap_or(1).saturating_sub(1),
                    text: std::mem::take(&mut last_message),
                });
            }
            continue;
        }
        let caps = PAREN.captures(line).or_else(|| COLON.captures(line));
        if let Some(c) = caps
            && let Some(path) = resolve(&c[1])
        {
            let text = c.get(4).map_or("", |m| m.as_str()).to_string();
            items.push(QfItem {
                path,
                line: c[2].parse::<usize>().unwrap_or(1).saturating_sub(1),
                col: c
                    .get(3)
                    .and_then(|m| m.as_str().parse::<usize>().ok())
                    .unwrap_or(1)
                    .saturating_sub(1),
                text,
            });
            continue;
        }
        let t = line.trim_start();
        if t.starts_with("error") || t.starts_with("warning") {
            last_message = t.to_string();
        }
    }
    items
}

/// `:vimgrep /pat/[gj] file...`: split after the pattern and its flags.
fn split_vimgrep(arg: &str) -> (String, String) {
    let t = arg.trim_start();
    let lead = &arg[..arg.len() - t.len()];
    let Some(d) = t.chars().next() else {
        return (arg.to_string(), String::new());
    };
    if d.is_alphanumeric() || d == '_' || d == '"' || d == '|' {
        // `:vimgrep word files`: the pattern is the first word.
        let end = t.find([' ', '\t']).unwrap_or(t.len());
        return (format!("{lead}{}", &t[..end]), t[end..].to_string());
    }
    let mut it = t.char_indices().skip(1);
    let mut close = None;
    while let Some((i, c)) = it.next() {
        if c == '\\' {
            it.next();
            continue;
        }
        if c == d {
            close = Some(i);
            break;
        }
    }
    let Some(close) = close else {
        return (arg.to_string(), String::new());
    };
    let mut end = close + d.len_utf8();
    while t[end..].starts_with(['g', 'j', 'f']) {
        end += 1;
    }
    (format!("{lead}{}", &t[..end]), t[end..].to_string())
}

fn vimgrep(vim: &mut VimState, host: &mut dyn Host, add: bool, bang: bool, arg: &str) -> R<()> {
    let (pat_part, files) = split_vimgrep(arg);
    let p = pat_part.trim();
    let (pattern, flags) = match p.chars().next() {
        Some(d) if !(d.is_alphanumeric() || d == '_') => {
            let body = &p[d.len_utf8()..];
            let close = body.rfind(d).unwrap_or(body.len());
            (
                body[..close].to_string(),
                body[(close + d.len_utf8()).min(body.len())..].to_string(),
            )
        }
        _ => (p.to_string(), String::new()),
    };
    let pattern = if pattern.is_empty() {
        vim.last_search
            .as_ref()
            .map(|(p, _)| p.clone())
            .ok_or("E35: No previous regular expression")?
    } else {
        pattern
    };
    let files = files.trim();
    if files.is_empty() {
        return Err("E683: File name missing or invalid pattern".into());
    }
    let re = search::compile(
        &pattern,
        &search::Options {
            ignorecase: vim.opts.ignorecase,
            smartcase: vim.opts.smartcase,
            magic: vim.opts.magic,
            last_sub: None,
        },
    )?;
    let cwd = host.cwd().unwrap_or_default();
    let paths = glob_files(&cwd, files);
    let all = flags.contains('g');
    let mut items = Vec::new();
    for path in paths {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.len() > 8 * 1024 * 1024 {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            for (s, _) in re.find_iter(line) {
                items.push(QfItem {
                    path: path.clone(),
                    line: n,
                    col: line[..s].chars().count(),
                    text: line.to_string(),
                });
                if !all {
                    break;
                }
            }
        }
        if items.len() > 100_000 {
            break;
        }
    }
    if items.is_empty() {
        return Err(format!("E480: No match: {pattern}"));
    }
    let n = items.len();
    set_quickfix(vim, items, add);
    vim.last_search = Some((pattern.clone(), true));
    vim.regs.last_search = pattern;
    if flags.contains('j') || bang {
        vim.set_message(format!("{n} match{}", if n == 1 { "" } else { "es" }));
        return Ok(());
    }
    qf_jump(
        vim,
        host,
        QfMove::Nth(if add { vim.qf.len() - n + 1 } else { 1 }),
    )
}

/// Directories never searched by `:vimgrep **` and `:find`.
fn skip_dir(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "target" | "node_modules" | "__pycache__" | "dist" | "build"
        )
}

/// Walk `root` (bounded) for files.
fn walk(root: &std::path::Path, mut visit: impl FnMut(&std::path::Path, &str) -> bool) {
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries.into_iter().rev() {
            seen += 1;
            if seen > 300_000 {
                return;
            }
            let name = e.file_name().to_string_lossy().to_string();
            let Ok(ft) = e.file_type() else {
                continue;
            };
            let path = e.path();
            if ft.is_dir() {
                if !skip_dir(&name) {
                    stack.push(path);
                }
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                if !visit(&path, &rel) {
                    return;
                }
            }
        }
    }
}

fn glob_regex(glob: &str) -> Option<regex::Regex> {
    let g = glob.replace('\\', "/");
    let mut re = String::from("(?i)^");
    if !cfg!(windows) {
        re = "^".into();
    }
    let chars: Vec<char> = g.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '*' && chars.get(i + 1) == Some(&'*') {
            if chars.get(i + 2) == Some(&'/') {
                re.push_str("(?:.*/)?");
                i += 3;
            } else {
                re.push_str(".*");
                i += 2;
            }
            continue;
        }
        match c {
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            '{' => re.push_str("(?:"),
            '}' => re.push(')'),
            ',' => re.push('|'),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
        i += 1;
    }
    re.push('$');
    regex::Regex::new(&re).ok()
}

/// File arguments with `*`, `**` and `?` expanded under `cwd`.
fn glob_files(cwd: &std::path::Path, files: &str) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for f in files.split_whitespace() {
        let f = f.replace("\\ ", " ");
        if !f.contains(['*', '?', '{']) {
            let p = std::path::Path::new(&f);
            let full = if p.is_absolute() {
                p.to_path_buf()
            } else {
                cwd.join(p)
            };
            if full.is_file() {
                out.push(full);
            } else if full.is_dir() {
                walk(&full, |path, _| {
                    out.push(path.to_path_buf());
                    true
                });
            }
            continue;
        }
        // A literal directory prefix narrows the walk.
        let norm = f.replace('\\', "/");
        let (root, pat) = match norm.find(['*', '?', '{']) {
            Some(i) => match norm[..i].rfind('/') {
                Some(slash) => (cwd.join(&norm[..slash]), norm[slash + 1..].to_string()),
                None => (cwd.to_path_buf(), norm.clone()),
            },
            None => (cwd.to_path_buf(), norm.clone()),
        };
        let Some(re) = glob_regex(&pat) else {
            continue;
        };
        walk(&root, |path, rel| {
            if re.is_match(rel) {
                out.push(path.to_path_buf());
            }
            true
        });
    }
    out.sort();
    out.dedup();
    out
}

/// `:find name`: a file under the working directory (as with `path=**`).
fn find_file(vim: &VimState, host: &dyn Host, name: &str) -> R<String> {
    let name = super::paths::argument(name).replace("\\ ", " ");
    if name.is_empty() {
        return Err("E471: Argument required".into());
    }
    let cwd = host.cwd().unwrap_or_default();
    let suffixes: Vec<String> = std::iter::once(String::new())
        .chain(
            vim.other_opts
                .get("suffixesadd")
                .map(|s| s.split(',').map(str::to_string).collect::<Vec<_>>())
                .unwrap_or_default(),
        )
        .collect();
    // Relative to the working directory or the current file first.
    let here = host
        .ed_ref()
        .and_then(|ed| ed.file_path.as_ref())
        .and_then(|p| p.parent().map(|p| p.to_path_buf()));
    for base in std::iter::once(cwd.clone()).chain(here) {
        for s in &suffixes {
            let p = base.join(format!("{name}{s}"));
            if p.is_file() {
                return Ok(p.display().to_string());
            }
        }
    }
    let norm = name.replace('\\', "/");
    let re = if norm.contains(['*', '?']) {
        glob_regex(&format!("**/{norm}"))
    } else {
        None
    };
    let mut best: Option<(usize, std::path::PathBuf)> = None;
    walk(&cwd, |path, rel| {
        let hit = match &re {
            Some(re) => re.is_match(rel),
            None => suffixes.iter().any(|s| {
                let want = format!("{norm}{s}");
                rel == want || rel.ends_with(&format!("/{want}"))
            }),
        };
        if hit && best.as_ref().is_none_or(|(d, _)| rel.len() < *d) {
            best = Some((rel.len(), path.to_path_buf()));
        }
        true
    });
    best.map(|(_, p)| p.display().to_string())
        .ok_or_else(|| format!("E345: Can't find file \"{name}\" in path"))
}

fn resize(vim: &mut VimState, host: &mut dyn Host, arg: &str, width: bool) -> R<()> {
    let _ = vim;
    let arg = arg.trim();
    let (delta, absolute) = if let Some(n) = arg.strip_prefix('+') {
        (n.parse::<isize>().unwrap_or(1), None)
    } else if let Some(n) = arg.strip_prefix('-') {
        (-n.parse::<isize>().unwrap_or(1), None)
    } else if arg.is_empty() {
        (0, Some(usize::MAX))
    } else {
        (
            0,
            Some(
                arg.parse::<usize>()
                    .map_err(|_| format!("E475: Invalid argument: {arg}"))?,
            ),
        )
    };
    host.act(Act::Resize {
        width,
        delta,
        absolute,
    })
}

/// Ex commands whose arguments are file names: `%`, `#`, `<cword>`...
/// expand in them (Vim's cmdline-special).
fn expands_files(full: &str) -> bool {
    matches!(
        full,
        "edit"
            | "write"
            | "update"
            | "wq"
            | "xit"
            | "exit"
            | "saveas"
            | "split"
            | "vsplit"
            | "new"
            | "vnew"
            | "tabedit"
            | "tabnew"
            | "read"
            | "source"
            | "cd"
            | "lcd"
            | "tcd"
            | "!"
            | "terminal"
            | "find"
            | "sfind"
            | "tabfind"
            | "grep"
            | "grepadd"
            | "lgrep"
            | "make"
            | "lmake"
            | "badd"
            | "argadd"
            | "Explore"
            | "Sexplore"
            | "Vexplore"
            | "Lexplore"
    )
}

/// Expand unescaped `%`, `#`, `<cword>`, `<cWORD>`, `<cfile>` (with
/// `:p :h :t :r :e :~ :.` modifiers); `\%` stays a literal `%`.
fn expand_specials(host: &dyn Host, text: &str) -> String {
    let file = || -> Option<String> {
        let ed = host.ed_ref()?;
        let path = ed.file_path.as_ref()?;
        let cwd = host.cwd().unwrap_or_default();
        Some(
            path.strip_prefix(&cwd)
                .unwrap_or(path)
                .display()
                .to_string(),
        )
    };
    let word = |big: bool| -> String {
        let Some(ed) = host.ed_ref() else {
            return String::new();
        };
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        let chars = t.line_chars(ed.cursor_line);
        let is = |c: char| {
            if big {
                !c.is_whitespace()
            } else {
                c.is_alphanumeric() || c == '_'
            }
        };
        let mut st = ed.cursor_col.min(chars.len());
        while st < chars.len() && !is(chars[st]) {
            st += 1;
        }
        let mut b = st;
        while b > 0 && is(chars[b - 1]) {
            b -= 1;
        }
        let mut e = st;
        while e < chars.len() && is(chars[e]) {
            e += 1;
        }
        chars[b..e].iter().collect()
    };
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let take_mods = |i: &mut usize| -> String {
        let mut mods = String::new();
        while *i + 1 < chars.len() && chars[*i] == ':' && "p~.htre".contains(chars[*i + 1]) {
            mods.push(':');
            mods.push(chars[*i + 1]);
            *i += 2;
        }
        mods
    };
    while i < chars.len() {
        let c = chars[i];
        if c == '\\' && matches!(chars.get(i + 1), Some('%' | '#' | '<')) {
            out.push(chars[i + 1]);
            i += 2;
            continue;
        }
        if c == '%' {
            i += 1;
            let mods = take_mods(&mut i);
            match file() {
                Some(f) => out.push_str(&super::paths::modify(&f, &mods)),
                None => {
                    out.push('%');
                    out.push_str(&mods);
                }
            }
            continue;
        }
        if c == '<' {
            let rest: String = chars[i..].iter().take(8).collect();
            let special = ["<cword>", "<cWORD>", "<cfile>"]
                .into_iter()
                .find(|s| rest.starts_with(s));
            if let Some(s) = special {
                i += s.len();
                let mods = take_mods(&mut i);
                let w = match s {
                    "<cword>" => word(false),
                    "<cWORD>" => word(true),
                    _ => word(true)
                        .trim_matches(|c: char| "\"'()[]{}<>,;".contains(c))
                        .to_string(),
                };
                out.push_str(&if mods.is_empty() {
                    w
                } else {
                    super::paths::modify(&w, &mods)
                });
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// The first `|` part opens a script block (`if`, `for`, `while`, `try`).
fn starts_block(part: &str) -> bool {
    let t = part.trim_start_matches([' ', ':']);
    let w: String = t.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    matches!(
        w.as_str(),
        "if" | "for" | "while" | "wh" | "whi" | "whil" | "try"
    )
}

/// Option names and aliases.
const OPTIONS: &[(&str, &str)] = &[
    ("ignorecase", "ic"),
    ("smartcase", "scs"),
    ("hlsearch", "hls"),
    ("incsearch", "is"),
    ("wrapscan", "ws"),
    ("magic", "magic"),
    ("gdefault", "gd"),
    ("timeout", "to"),
    ("timeoutlen", "tm"),
    ("ttimeoutlen", "ttm"),
    ("clipboard", "cb"),
    ("startofline", "sol"),
    ("joinspaces", "js"),
    ("tildeop", "top"),
    ("whichwrap", "ww"),
    ("iskeyword", "isk"),
    ("shiftround", "sr"),
    ("autoindent", "ai"),
    ("textwidth", "tw"),
    ("nrformats", "nf"),
    ("showcmd", "sc"),
    ("whichkey", "wk"),
    ("showmode", "smd"),
    ("scrolloff", "so"),
    ("report", "report"),
    ("splitright", "spr"),
    ("splitbelow", "sb"),
    ("number", "nu"),
    ("relativenumber", "rnu"),
    ("wrap", "wrap"),
    ("tabstop", "ts"),
    ("shiftwidth", "sw"),
    ("softtabstop", "sts"),
    ("expandtab", "et"),
    ("list", "list"),
    ("readonly", "ro"),
    ("modifiable", "ma"),
    ("fileformat", "ff"),
    ("filetype", "ft"),
    ("cursorline", "cul"),
    ("spell", "spell"),
    ("mouse", "mouse"),
    ("encoding", "enc"),
    ("fileencoding", "fenc"),
    ("syntax", "syn"),
    ("smartindent", "si"),
    ("cindent", "cin"),
    ("hidden", "hid"),
    ("backup", "bk"),
    ("swapfile", "swf"),
    ("undofile", "udf"),
    ("compatible", "cp"),
    ("ruler", "ru"),
    ("laststatus", "ls"),
    ("cmdheight", "ch"),
    ("signcolumn", "scl"),
    ("colorcolumn", "cc"),
    ("termguicolors", "tgc"),
    ("background", "bg"),
    ("belloff", "bo"),
    ("errorbells", "eb"),
    ("visualbell", "vb"),
    ("updatetime", "ut"),
    ("wildmenu", "wmnu"),
    ("wildmode", "wim"),
    ("backspace", "bs"),
    ("history", "hi"),
    ("lazyredraw", "lz"),
    ("showmatch", "sm"),
    ("foldmethod", "fdm"),
    ("foldenable", "fen"),
    ("sidescroll", "ss"),
    ("sidescrolloff", "siso"),
    ("undolevels", "ul"),
    ("virtualedit", "ve"),
    ("selection", "sel"),
    ("matchpairs", "mps"),
    ("wildignorecase", "wic"),
    ("autoread", "ar"),
    ("autowrite", "aw"),
    ("formatoptions", "fo"),
    ("conceallevel", "cole"),
    ("linebreak", "lbr"),
    ("breakindent", "bri"),
    ("cursorcolumn", "cuc"),
    ("ttyfast", "tf"),
    ("highlightyank", "hly"),
    ("vimctrlkeys", "vck"),
    ("grepprg", "gp"),
    ("grepformat", "gfm"),
    ("makeprg", "mp"),
    ("errorformat", "efm"),
    ("path", "pa"),
    ("wildignore", "wig"),
    ("suffixesadd", "sua"),
    ("showtabline", "stal"),
    ("title", "title"),
    ("shortmess", "shm"),
    ("completeopt", "cot"),
    ("undodir", "udir"),
    ("backupdir", "bdir"),
    ("directory", "dir"),
    ("writebackup", "wb"),
    ("spelllang", "spl"),
    ("fileformats", "ffs"),
    ("listchars", "lcs"),
    ("fillchars", "fcs"),
    ("cursorlineopt", "culopt"),
    ("numberwidth", "nuw"),
    ("ttimeout", "ttimeout"),
    ("smarttab", "sta"),
    ("infercase", "inf"),
    ("pumheight", "ph"),
    ("confirm", "cf"),
    ("modeline", "ml"),
    ("modelines", "mls"),
    ("secure", "secure"),
    ("exrc", "ex"),
    ("scrolljump", "sj"),
    ("display", "dy"),
    ("guifont", "gfn"),
    ("guioptions", "go"),
    ("foldlevel", "fdl"),
    ("foldlevelstart", "fdls"),
    ("foldcolumn", "fdc"),
    ("foldnestmax", "fdn"),
    ("wildoptions", "wop"),
    ("shell", "sh"),
    ("shellcmdflag", "shcf"),
    ("statusline", "stl"),
    ("tabline", "tal"),
    ("rulerformat", "ruf"),
    ("keywordprg", "kp"),
    ("formatprg", "fp"),
    ("equalprg", "ep"),
    ("tags", "tag"),
    ("cpoptions", "cpo"),
    ("whichwrap", "ww"),
    ("winminheight", "wmh"),
    ("winminwidth", "wmw"),
    ("winheight", "wh"),
    ("winwidth", "wiw"),
    ("equalalways", "ea"),
    ("synmaxcol", "smc"),
    ("redrawtime", "rdt"),
    ("regexpengine", "re"),
    ("maxmempattern", "mmp"),
    ("termencoding", "tenc"),
    ("ttymouse", "ttym"),
    ("esckeys", "ek"),
    ("hlsearch", "hls"),
    ("showbreak", "sbr"),
    ("emoji", "emo"),
    ("ambiwidth", "ambw"),
    ("autochdir", "acd"),
    ("cmdwinheight", "cwh"),
    ("previewheight", "pvh"),
    ("helpheight", "hh"),
    ("sessionoptions", "ssop"),
    ("viewoptions", "vop"),
    ("viminfo", "vi"),
    ("viminfofile", "vif"),
    ("shada", "sd"),
    ("inccommand", "icm"),
    ("textwidth", "tw"),
    ("wrapmargin", "wm"),
    ("comments", "com"),
    ("commentstring", "cms"),
    ("indentexpr", "inde"),
    ("indentkeys", "indk"),
    ("cinoptions", "cino"),
    ("cinkeys", "cink"),
    ("copyindent", "ci"),
    ("preserveindent", "pi"),
    ("omnifunc", "ofu"),
    ("completefunc", "cfu"),
    ("dictionary", "dict"),
    ("thesaurus", "tsr"),
    ("complete", "cpt"),
    ("spellfile", "spf"),
    ("fixendofline", "fixeol"),
    ("endofline", "eol"),
    ("binary", "bin"),
    ("bomb", "bomb"),
    ("lisp", "lisp"),
    ("termwinsize", "tws"),
    ("ttyscroll", "tsl"),
    ("guicursor", "gcr"),
    ("mousemodel", "mousem"),
    ("mousehide", "mh"),
    ("keymodel", "km"),
    ("selectmode", "slm"),
    ("clipboard", "cb"),
    ("splitkeep", "spk"),
    ("jumpoptions", "jop"),
    ("switchbuf", "swb"),
    ("tagcase", "tc"),
    ("timeout", "to"),
];

pub(crate) fn command_known(n: &str) -> bool {
    resolve(n).is_some()
}

pub(crate) fn option_known(n: &str) -> bool {
    option_name(n).is_some()
}

fn option_name(n: &str) -> Option<&'static str> {
    OPTIONS
        .iter()
        .find(|(full, short)| *full == n || *short == n)
        .map(|(f, _)| *f)
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
        if let Err(e) = set_one(vim, host, &item) {
            errors.push(e);
        }
    }
    errors.into_iter().next().map_or(Ok(()), Err)
}

fn split_set_args(arg: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = arg.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                cur.push(n);
            }
            continue;
        }
        if c == ' ' {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if c == '"' && cur.is_empty() {
            break;
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn bool_str(b: bool) -> &'static str {
    if b { "true" } else { "false" }
}

fn set_one(vim: &mut VimState, host: &mut dyn Host, item: &str) -> R<()> {
    // Forms: name, noname, invname, name!, name?, name&, name=val, name+=val, name-=val, name^=val, name:val
    let (raw, op, value) = if let Some(i) = item.find(['=', ':']) {
        let (n, v) = item.split_at(i);
        let (n, op) = if let Some(n) = n.strip_suffix('+') {
            (n, "+=")
        } else if let Some(n) = n.strip_suffix('-') {
            (n, "-=")
        } else if let Some(n) = n.strip_suffix('^') {
            (n, "^=")
        } else {
            (n, "=")
        };
        (n.to_string(), op, Some(v[1..].to_string()))
    } else {
        (item.to_string(), "", None)
    };
    let (raw, query, reset, toggle_bang) = {
        let mut r = raw.as_str();
        let q = r.ends_with('?');
        if q {
            r = &r[..r.len() - 1];
        }
        let a = r.ends_with('&');
        if a {
            r = &r[..r.len() - 1];
        }
        let b = r.ends_with('!');
        if b {
            r = &r[..r.len() - 1];
        }
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
            if query {
                vim.set_message(format!(
                    "{}{}",
                    if vim.opts.$field { "  " } else { "no" },
                    name
                ));
                return Ok(());
            }
            vim.opts.$field = if reset {
                defaults.$field
            } else if invert {
                !vim.opts.$field
            } else {
                !negate
            };
            Ok(())
        }};
    }
    macro_rules! number {
        ($field:ident) => {{
            if query || value.is_none() {
                vim.set_message(format!("  {}={}", name, vim.opts.$field));
                return Ok(());
            }
            let v = value
                .as_deref()
                .unwrap_or("")
                .parse()
                .map_err(|_| format!("E521: Number required after =: {item}"))?;
            vim.opts.$field = match op {
                "+=" => vim.opts.$field + v,
                "-=" => vim.opts.$field.saturating_sub(v),
                "^=" => vim.opts.$field * v,
                _ => v,
            };
            Ok(())
        }};
    }
    match name {
        "ignorecase" => flag!(ignorecase),
        "smartcase" => flag!(smartcase),
        "hlsearch" => {
            let r = flag!(hlsearch);
            vim.hl_hidden = false;
            r
        }
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
        "whichkey" => flag!(which_key),
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
            if query || value.is_none() {
                vim.set_message(format!(
                    "  scrolloff={}",
                    vim.opts.scrolloff.map_or(-1, |v| v as i64)
                ));
                return Ok(());
            }
            let v: i64 = value
                .as_deref()
                .unwrap_or("0")
                .parse()
                .map_err(|_| "E521: Number required".to_string())?;
            vim.opts.scrolloff = (v >= 0).then_some(v as usize);
            host.act(Act::SetOption {
                name: "scrolloff".into(),
                value: v.max(0).to_string(),
            })
        }
        "clipboard" => {
            if query || value.is_none() {
                vim.set_message(format!(
                    "  clipboard={}",
                    if vim.opts.clipboard_unnamed {
                        "unnamedplus"
                    } else {
                        ""
                    }
                ));
                return Ok(());
            }
            let v = value.unwrap_or_default();
            vim.opts.clipboard_unnamed = match op {
                "-=" => {
                    if v.contains("unnamed") {
                        false
                    } else {
                        vim.opts.clipboard_unnamed
                    }
                }
                _ => v.contains("unnamed"),
            };
            Ok(())
        }
        "whichwrap" => {
            if query || value.is_none() {
                vim.set_message(format!("  whichwrap={}", vim.opts.whichwrap));
                return Ok(());
            }
            let v = value.unwrap_or_default();
            vim.opts.whichwrap = match op {
                "+=" => format!("{},{v}", vim.opts.whichwrap),
                _ => v,
            };
            Ok(())
        }
        "iskeyword" => {
            let v = value.unwrap_or_default();
            let chars = parse_isk(&v);
            match op {
                "+=" => vim.opts.iskeyword.extra.extend(chars),
                "-=" => {
                    vim.opts.iskeyword.extra.retain(|c| !chars.contains(c));
                    vim.opts.iskeyword.removed.extend(chars);
                }
                _ => {
                    vim.opts.iskeyword = super::text::Keyword {
                        extra: chars,
                        removed: Vec::new(),
                    };
                }
            }
            Ok(())
        }
        "nrformats" => {
            let v = value.unwrap_or_default();
            let has = |s: &str| v.split(',').any(|p| p == s);
            match op {
                "+=" => {
                    if has("alpha") {
                        vim.opts.nrformats_alpha = true;
                    }
                    if has("octal") {
                        vim.opts.nrformats_octal = true;
                    }
                    if has("hex") {
                        vim.opts.nrformats_hex = true;
                    }
                    if has("bin") {
                        vim.opts.nrformats_bin = true;
                    }
                }
                "-=" => {
                    if has("alpha") {
                        vim.opts.nrformats_alpha = false;
                    }
                    if has("octal") {
                        vim.opts.nrformats_octal = false;
                    }
                    if has("hex") {
                        vim.opts.nrformats_hex = false;
                    }
                    if has("bin") {
                        vim.opts.nrformats_bin = false;
                    }
                }
                _ => {
                    vim.opts.nrformats_alpha = has("alpha");
                    vim.opts.nrformats_octal = has("octal");
                    vim.opts.nrformats_hex = has("hex");
                    vim.opts.nrformats_bin = has("bin");
                }
            }
            Ok(())
        }
        "matchpairs" => Ok(()),
        // Editor/workbench options.
        "number" | "relativenumber" | "wrap" | "list" | "expandtab" | "readonly" | "modifiable"
        | "cursorline" | "spell" | "linebreak" => {
            let v = if query {
                "?".to_string()
            } else if reset {
                "&".into()
            } else if invert {
                "!".into()
            } else {
                bool_str(!negate).into()
            };
            host.act(Act::SetOption {
                name: name.into(),
                value: v,
            })
        }
        "tabstop" | "shiftwidth" | "softtabstop" | "fileformat" | "filetype" | "fileencoding"
        | "encoding" | "colorcolumn" | "syntax" => {
            let v = if query || value.is_none() {
                "?".to_string()
            } else {
                value.unwrap_or_default()
            };
            host.act(Act::SetOption {
                name: name.into(),
                value: v,
            })
        }
        // Accepted for vimrc compatibility; the value is kept for `:set x?`.
        _ => {
            let cur = vim.other_opts.get(name).cloned();
            if query
                || (value.is_none()
                    && !negate
                    && !invert
                    && !reset
                    && cur.as_deref().is_some_and(|v| v != "0" && v != "1"))
            {
                vim.set_message(match cur {
                    Some(v) if v == "0" => format!("no{name}"),
                    Some(v) if v == "1" => format!("  {name}"),
                    Some(v) => format!("  {name}={v}"),
                    None => format!("  {name}={}", default_other(name)),
                });
                return Ok(());
            }
            let new = match (value, op) {
                (Some(v), "+=") => {
                    let c = cur.unwrap_or_else(|| default_other(name).to_string());
                    if c.is_empty() { v } else { format!("{c},{v}") }
                }
                (Some(v), "-=") => {
                    let c = cur.unwrap_or_default();
                    c.split(',')
                        .filter(|p| *p != v)
                        .collect::<Vec<_>>()
                        .join(",")
                }
                (Some(v), "^=") => {
                    let c = cur.unwrap_or_default();
                    if c.is_empty() { v } else { format!("{v},{c}") }
                }
                (Some(v), _) => v,
                (None, _) if reset => default_other(name).to_string(),
                (None, _) if invert => {
                    if cur.as_deref() == Some("1") {
                        "0".into()
                    } else {
                        "1".into()
                    }
                }
                (None, _) => {
                    if negate {
                        "0".into()
                    } else {
                        "1".into()
                    }
                }
            };
            vim.other_opts.insert(name.to_string(), new);
            Ok(())
        }
    }
}

/// Defaults of options kept only for queries (and `:grep`/`:make`/`:find`).
fn default_other(name: &str) -> &'static str {
    match name {
        "grepprg" => {
            if cfg!(windows) {
                "findstr /n /s"
            } else {
                "grep -n $* /dev/null"
            }
        }
        "grepformat" => "%f:%l:%m,%f:%l%m,%f  %l%m",
        "makeprg" => "make",
        "path" => ".,,",
        "shell" => {
            if cfg!(windows) {
                "cmd.exe"
            } else {
                "sh"
            }
        }
        "keywordprg" => ":help",
        "commentstring" => "/* %s */",
        _ => "",
    }
}

fn parse_isk(v: &str) -> Vec<char> {
    let mut out = Vec::new();
    for part in v.split(',') {
        if part.is_empty() {
            out.push(',');
            continue;
        }
        if let Some((a, b)) = part
            .split_once('-')
            .filter(|(a, b)| !a.is_empty() && !b.is_empty())
        {
            let (a, b) = (
                a.parse::<u32>()
                    .ok()
                    .and_then(char::from_u32)
                    .or(a.chars().next()),
                b.parse::<u32>()
                    .ok()
                    .and_then(char::from_u32)
                    .or(b.chars().next()),
            );
            if let (Some(a), Some(b)) = (a, b) {
                for c in a..=b {
                    out.push(c);
                }
            }
        } else if part == "@" {
        } else if let Ok(n) = part.parse::<u32>() {
            if let Some(c) = char::from_u32(n) {
                out.push(c);
            }
        } else {
            out.extend(part.chars().take(1));
        }
    }
    out
}

/// Source vimrc text. Returns error lines.
pub(crate) fn source(vim: &mut VimState, host: &mut dyn Host, text: &str) -> Vec<String> {
    let errors = super::script::source(vim, host, text);
    vim.message = None;
    errors
}

// ---------------------------------------------------------------------------
// Completion

pub(crate) fn completions(vim: &VimState, host: &mut dyn Host, before: &str) -> (usize, Matches) {
    let chars: Vec<char> = before.chars().collect();
    // Skip a leading range.
    let mut i = 0;
    while i < chars.len() && (chars[i] == ' ' || chars[i] == ':') {
        i += 1;
    }
    while i < chars.len() && (chars[i].is_ascii_digit() || ",.;$%'<>+-".contains(chars[i])) {
        i += 1;
    }
    let name_start = i;
    while i < chars.len() && chars[i].is_ascii_alphabetic() {
        i += 1;
    }
    let name: String = chars[name_start..i].iter().collect();
    if i >= chars.len() {
        let mut items: Vec<String> = COMMANDS
            .iter()
            .map(|(n, _)| n.to_string())
            .filter(|n| n.starts_with(&name))
            .collect();
        items.sort();
        items.dedup();
        return (name_start, Matches::Ready(items));
    }
    if chars.get(i) == Some(&'!') {
        i += 1;
    }
    while i < chars.len() && matches!(chars[i], ' ' | '\t') {
        i += 1;
    }
    let full = resolve(&name).unwrap_or("");
    let arg_start = i;
    let arg: String = chars[arg_start..].iter().collect();
    match full {
        "set" | "setlocal" | "setglobal" => {
            let word_start = arg
                .rfind(' ')
                .map_or(arg_start, |p| arg_start + arg[..p + 1].chars().count());
            let word = &arg[arg.rfind(' ').map_or(0, |p| p + 1)..];
            let (prefix, w) = if let Some(w) = word.strip_prefix("no") {
                ("no", w)
            } else if let Some(w) = word.strip_prefix("inv") {
                ("inv", w)
            } else {
                ("", word)
            };
            let mut items: Vec<String> = OPTIONS
                .iter()
                .map(|(n, _)| format!("{prefix}{n}"))
                .filter(|n| n[prefix.len()..].starts_with(w))
                .collect();
            items.sort();
            (word_start, Matches::Ready(items))
        }
        "buffer" | "bdelete" => {
            let items = host
                .buffers()
                .into_iter()
                .map(|(_, n, _)| n)
                .filter(|n| n.to_lowercase().contains(&arg.to_lowercase()))
                .collect();
            (arg_start, Matches::Ready(items))
        }
        "vtcode" | "VT" => {
            let mut items: Vec<String> = host
                .commands()
                .into_iter()
                .filter(|c| c.starts_with(&arg))
                .collect();
            items.sort();
            (arg_start, Matches::Ready(items))
        }
        "edit" | "write" | "update" | "wq" | "xit" | "exit" | "saveas" | "split" | "vsplit"
        | "new" | "vnew" | "tabedit" | "tabnew" | "read" | "source"
            if !arg.starts_with('!') =>
        {
            (
                arg_start,
                host.complete_path(&arg, false, vim.opts.wildignorecase),
            )
        }
        "cd" | "lcd" | "tcd" | "Explore" | "Lexplore" | "Sexplore" | "Vexplore" => (
            arg_start,
            host.complete_path(&arg, true, vim.opts.wildignorecase),
        ),
        _ => (arg_start, Matches::Ready(Vec::new())),
    }
}

// ---------------------------------------------------------------------------
// Digraphs (a common subset of RFC 1345, as in Vim's `:digraphs`).

pub(crate) fn digraph(a: char, b: char) -> Option<char> {
    const TABLE: &[(&str, char)] = &[
        ("a:", 'ä'),
        ("o:", 'ö'),
        ("u:", 'ü'),
        ("A:", 'Ä'),
        ("O:", 'Ö'),
        ("U:", 'Ü'),
        ("ss", 'ß'),
        ("e:", 'ë'),
        ("i:", 'ï'),
        ("a'", 'á'),
        ("e'", 'é'),
        ("i'", 'í'),
        ("o'", 'ó'),
        ("u'", 'ú'),
        ("y'", 'ý'),
        ("A'", 'Á'),
        ("E'", 'É'),
        ("I'", 'Í'),
        ("O'", 'Ó'),
        ("U'", 'Ú'),
        ("a!", 'à'),
        ("e!", 'è'),
        ("i!", 'ì'),
        ("o!", 'ò'),
        ("u!", 'ù'),
        ("A!", 'À'),
        ("E!", 'È'),
        ("I!", 'Ì'),
        ("O!", 'Ò'),
        ("U!", 'Ù'),
        ("a>", 'â'),
        ("e>", 'ê'),
        ("i>", 'î'),
        ("o>", 'ô'),
        ("u>", 'û'),
        ("A>", 'Â'),
        ("E>", 'Ê'),
        ("I>", 'Î'),
        ("O>", 'Ô'),
        ("U>", 'Û'),
        ("a?", 'ã'),
        ("o?", 'õ'),
        ("n?", 'ñ'),
        ("A?", 'Ã'),
        ("O?", 'Õ'),
        ("N?", 'Ñ'),
        ("c,", 'ç'),
        ("C,", 'Ç'),
        ("aa", 'å'),
        ("AA", 'Å'),
        ("ae", 'æ'),
        ("AE", 'Æ'),
        ("o/", 'ø'),
        ("O/", 'Ø'),
        ("s<", 'š'),
        ("S<", 'Š'),
        ("z<", 'ž'),
        ("Z<", 'Ž'),
        ("c<", 'č'),
        ("C<", 'Č'),
        ("a*", 'α'),
        ("b*", 'β'),
        ("g*", 'γ'),
        ("d*", 'δ'),
        ("e*", 'ε'),
        ("z*", 'ζ'),
        ("y*", 'η'),
        ("h*", 'θ'),
        ("i*", 'ι'),
        ("k*", 'κ'),
        ("l*", 'λ'),
        ("m*", 'μ'),
        ("n*", 'ν'),
        ("c*", 'ξ'),
        ("o*", 'ο'),
        ("p*", 'π'),
        ("r*", 'ρ'),
        ("s*", 'σ'),
        ("t*", 'τ'),
        ("u*", 'υ'),
        ("f*", 'φ'),
        ("x*", 'χ'),
        ("q*", 'ψ'),
        ("w*", 'ω'),
        ("D*", 'Δ'),
        ("G*", 'Γ'),
        ("L*", 'Λ'),
        ("P*", 'Π'),
        ("S*", 'Σ'),
        ("W*", 'Ω'),
        ("->", '→'),
        ("<-", '←'),
        ("-!", '↑'),
        ("-v", '↓'),
        ("=>", '⇒'),
        ("==", '⇔'),
        ("!=", '≠'),
        ("=<", '≤'),
        (">=", '≥'),
        ("?=", '≅'),
        ("+-", '±'),
        ("*X", '×'),
        ("-:", '÷'),
        ("OK", '✓'),
        ("XX", '✗'),
        ("Co", '©'),
        ("Rg", '®'),
        ("TM", '™'),
        ("Eu", '€'),
        ("Pd", '£'),
        ("Ye", '¥'),
        ("Ct", '¢'),
        ("SE", '§'),
        ("PI", '¶'),
        ("DG", '°'),
        ("My", 'µ'),
        ("<<", '«'),
        (">>", '»'),
        ("..", '…'),
        ("-N", '–'),
        ("-M", '—'),
        ("'6", '‘'),
        ("'9", '’'),
        ("\"6", '“'),
        ("\"9", '”'),
        ("1S", '¹'),
        ("2S", '²'),
        ("3S", '³'),
        ("12", '½'),
        ("14", '¼'),
        ("34", '¾'),
        ("00", '∞'),
        ("RT", '√'),
        ("FA", '∀'),
        ("TE", '∃'),
        ("(-", '∈'),
        ("AN", '∧'),
        ("OR", '∨'),
        ("NO", '¬'),
        ("Sb", '∙'),
        ("NS", '\u{a0}'),
        ("!I", '¡'),
        ("?I", '¿'),
    ];
    let key: String = [a, b].iter().collect();
    let rev: String = [b, a].iter().collect();
    TABLE
        .iter()
        .find(|(k, _)| *k == key)
        .or_else(|| TABLE.iter().find(|(k, _)| *k == rev))
        .map(|(_, c)| *c)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_arguments_parse_delimiters_flags_and_counts() {
        assert_eq!(
            parse_sub_args("/a/b/g"),
            Some(("a".into(), Some("b".into()), "g".into(), None))
        );
        assert_eq!(
            parse_sub_args("#a/b#c\\#d#gi 3"),
            Some(("a/b".into(), Some("c\\#d".into()), "gi".into(), Some(3)))
        );
        assert_eq!(
            parse_sub_args("/x"),
            Some(("x".into(), None, String::new(), None))
        );
        assert_eq!(
            parse_sub_args("/x/"),
            Some(("x".into(), Some(String::new()), String::new(), None))
        );
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
