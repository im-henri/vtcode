//! Vim script blocks: `if`/`elseif`/`else`, `for`, `while`, `try`, user
//! functions (`function!`, `a:` arguments, `l:` locals, `return`), so the
//! logic in a vimrc and the functions its mappings call work.

use std::collections::BTreeMap;

use super::eval::{self, Ctx, Val};
use super::{Host, VimState};

type R<T> = Result<T, String>;

#[derive(Clone, Debug)]
pub(crate) struct UserFn {
    pub(crate) params: Vec<String>,
    pub(crate) varargs: bool,
    pub(crate) range: bool,
    pub(crate) abort: bool,
    pub(crate) body: Vec<String>,
}

pub(crate) enum Flow {
    Normal,
    Return(Val),
    Break,
    Continue,
}

/// One logical line: continuation lines (`\`) are already joined.
#[derive(Clone)]
pub(crate) struct Line {
    pub(crate) n: usize,
    pub(crate) text: String,
}

/// Join continuation lines and drop comments.
pub(crate) fn logical_lines(text: &str) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    for (n, raw) in text.lines().enumerate() {
        let t = raw.trim();
        if let Some(rest) = t.strip_prefix('\\') {
            if let Some(last) = out.last_mut() {
                last.text.push_str(rest);
                continue;
            }
        }
        // `"\ ` comments inside a continued line.
        if t.starts_with("\"\\ ") {
            continue;
        }
        out.push(Line {
            n: n + 1,
            text: t.to_string(),
        });
    }
    out.retain(|l| !l.text.is_empty() && !l.text.starts_with('"') && !l.text.starts_with('#'));
    out
}

fn word(line: &str) -> &str {
    let end = line
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(line.len());
    &line[..end]
}

fn is_word(line: &str, names: &[&str]) -> bool {
    let w = word(line);
    names.contains(&w)
}

const IF: &[&str] = &["if"];
const ENDIF: &[&str] = &["endif", "en", "endi"];
const ELSEIF: &[&str] = &["elseif", "elsei"];
const ELSE: &[&str] = &["else", "el", "els"];
const FOR: &[&str] = &["for"];
const ENDFOR: &[&str] = &["endfor", "endfo"];
const WHILE: &[&str] = &["while", "wh", "whi", "whil"];
const ENDWHILE: &[&str] = &["endwhile", "endw", "endwh", "endwhi", "endwhil"];
const TRY: &[&str] = &["try"];
const CATCH: &[&str] = &["catch", "cat", "catc"];
const FINALLY: &[&str] = &["finally", "fina", "final", "finall"];
const ENDTRY: &[&str] = &["endtry", "endt", "endtr"];
const FUNCTION: &[&str] = &[
    "function", "fu", "fun", "func", "funct", "functi", "functio", "def",
];
const ENDFUNCTION: &[&str] = &[
    "endfunction",
    "endf",
    "endfu",
    "endfun",
    "endfunc",
    "endfunct",
    "endfuncti",
    "endfunctio",
    "enddef",
];
const AUGROUP: &[&str] = &["augroup", "aug", "augr", "augro", "augrou"];

/// Index of the line closing the block opened at `start`.
fn block_end(lines: &[Line], start: usize, open: &[&str], close: &[&str]) -> usize {
    let mut depth = 0;
    for (i, l) in lines.iter().enumerate().skip(start) {
        if is_word(&l.text, open) {
            depth += 1;
        } else if is_word(&l.text, close) {
            depth -= 1;
            if depth == 0 {
                return i;
            }
        }
    }
    lines.len()
}

/// Run lines. Errors are collected (with their line numbers) and the
/// script continues, as Vim does when sourcing; `abort` stops at the first.
pub(crate) fn run(
    vim: &mut VimState,
    host: &mut dyn Host,
    lines: &[Line],
    errors: &mut Vec<(usize, String)>,
    abort: bool,
) -> Flow {
    let mut i = 0;
    while i < lines.len() {
        if vim.ex_abort {
            return Flow::Normal;
        }
        let line = &lines[i];
        let text = line.text.trim_start_matches(':').trim();
        let fail = |errors: &mut Vec<(usize, String)>, e: String| {
            errors.push((line.n, e));
        };
        macro_rules! check_abort {
            () => {
                if abort && !errors.is_empty() {
                    return Flow::Normal;
                }
            };
        }
        if is_word(text, IF) {
            let end = block_end(lines, i, IF, ENDIF);
            // Split into branches at this depth.
            let mut branches: Vec<(Option<String>, usize, usize)> = Vec::new();
            let mut depth = 0;
            let mut cond = Some(text[word(text).len()..].trim().to_string());
            let mut from = i + 1;
            for j in i + 1..end {
                let t = lines[j].text.trim_start_matches(':').trim();
                if is_word(t, IF) {
                    depth += 1;
                } else if is_word(t, ENDIF) {
                    depth -= 1;
                } else if depth == 0 && (is_word(t, ELSEIF) || is_word(t, ELSE)) {
                    branches.push((cond.take(), from, j));
                    cond = if is_word(t, ELSEIF) {
                        Some(t[word(t).len()..].trim().to_string())
                    } else {
                        None
                    };
                    from = j + 1;
                }
            }
            branches.push((cond, from, end.min(lines.len())));
            for (c, s, e) in branches {
                let take = match c {
                    None => true,
                    Some(c) => match eval_str(vim, host, &c) {
                        Ok(v) => v.truthy(),
                        Err(err) => {
                            fail(errors, err);
                            false
                        }
                    },
                };
                if take {
                    match run(vim, host, &lines[s..e], errors, abort) {
                        Flow::Normal => {}
                        f => return f,
                    }
                    break;
                }
            }
            check_abort!();
            i = end + 1;
            continue;
        }
        if is_word(text, FOR) {
            let end = block_end(lines, i, FOR, ENDFOR);
            let spec = text[word(text).len()..].trim();
            let body = &lines[i + 1..end.min(lines.len())];
            let Some((vars, list)) = spec.split_once(" in ") else {
                fail(errors, "E690: Missing \"in\" after :for".into());
                i = end + 1;
                continue;
            };
            let items = match eval_str(vim, host, list) {
                Ok(Val::List(l)) => l,
                Ok(Val::Dict(d)) => d.into_values().collect(),
                Ok(Val::Str(s)) => s.chars().map(|c| Val::Str(c.to_string())).collect(),
                Ok(_) => {
                    fail(errors, "E1098: String, List or Blob required".into());
                    Vec::new()
                }
                Err(e) => {
                    fail(errors, e);
                    Vec::new()
                }
            };
            let vars = vars.trim();
            for item in items {
                if let Some(inner) = vars.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
                    let names: Vec<&str> = inner.split(',').map(str::trim).collect();
                    let vals = match &item {
                        Val::List(l) => l.clone(),
                        v => vec![v.clone()],
                    };
                    for (n, v) in names.iter().zip(vals) {
                        set_var(vim, n, v);
                    }
                } else {
                    set_var(vim, vars, item);
                }
                match run(vim, host, body, errors, abort) {
                    Flow::Break => break,
                    Flow::Return(v) => return Flow::Return(v),
                    _ => {}
                }
                if abort && !errors.is_empty() {
                    return Flow::Normal;
                }
            }
            i = end + 1;
            continue;
        }
        if is_word(text, WHILE) {
            let end = block_end(lines, i, WHILE, ENDWHILE);
            let cond = text[word(text).len()..].trim().to_string();
            let body = &lines[i + 1..end.min(lines.len())];
            let mut guard = 0;
            loop {
                guard += 1;
                if guard > 1_000_000 {
                    fail(errors, "E169: :while loop ran too long".into());
                    break;
                }
                match eval_str(vim, host, &cond) {
                    Ok(v) if v.truthy() => {}
                    Ok(_) => break,
                    Err(e) => {
                        fail(errors, e);
                        break;
                    }
                }
                match run(vim, host, body, errors, abort) {
                    Flow::Break => break,
                    Flow::Return(v) => return Flow::Return(v),
                    _ => {}
                }
                if abort && !errors.is_empty() {
                    return Flow::Normal;
                }
            }
            i = end + 1;
            continue;
        }
        if is_word(text, TRY) {
            let end = block_end(lines, i, TRY, ENDTRY);
            let mut depth = 0;
            let mut catch_at = None;
            let mut finally_at = None;
            for j in i + 1..end.min(lines.len()) {
                let t = lines[j].text.trim();
                if is_word(t, TRY) {
                    depth += 1;
                } else if is_word(t, ENDTRY) {
                    depth -= 1;
                } else if depth == 0 && is_word(t, CATCH) && catch_at.is_none() {
                    catch_at = Some(j);
                } else if depth == 0 && is_word(t, FINALLY) {
                    finally_at = Some(j);
                }
            }
            let try_end = catch_at.or(finally_at).unwrap_or(end);
            let mut errs = Vec::new();
            let flow = run(
                vim,
                host,
                &lines[i + 1..try_end.min(lines.len())],
                &mut errs,
                true,
            );
            let mut result = flow;
            if !errs.is_empty() {
                if let Some(c) = catch_at {
                    let stop = finally_at.unwrap_or(end);
                    // Later `catch` clauses at this depth are skipped.
                    let mut catch_end = stop;
                    for j in c + 1..stop {
                        if is_word(lines[j].text.trim(), CATCH) {
                            catch_end = j;
                            break;
                        }
                    }
                    let msg = errs[0].1.clone();
                    vim.vars.insert("v:exception".into(), Val::Str(msg));
                    result = run(
                        vim,
                        host,
                        &lines[c + 1..catch_end.min(lines.len())],
                        errors,
                        abort,
                    );
                } else {
                    errors.extend(errs);
                }
            }
            if let Some(f) = finally_at {
                run(
                    vim,
                    host,
                    &lines[f + 1..end.min(lines.len())],
                    errors,
                    abort,
                );
            }
            match result {
                Flow::Normal => {}
                f => return f,
            }
            i = end + 1;
            continue;
        }
        if is_word(text, FUNCTION)
            && text[word(text).len()..]
                .trim_start_matches('!')
                .trim()
                .contains('(')
        {
            let end = block_end(lines, i, FUNCTION, ENDFUNCTION);
            if let Err(e) = define(vim, text, &lines[i + 1..end.min(lines.len())]) {
                fail(errors, e);
            }
            i = end + 1;
            continue;
        }
        if is_word(text, AUGROUP) || is_word(text, ENDIF) || is_word(text, ENDFOR) {
            i += 1;
            continue;
        }
        match word(text) {
            "return" | "retu" | "retur" => {
                let e = text[word(text).len()..].trim();
                if e.is_empty() {
                    return Flow::Return(Val::Num(0));
                }
                return match eval_str(vim, host, e) {
                    Ok(v) => Flow::Return(v),
                    Err(err) => {
                        fail(errors, err);
                        Flow::Return(Val::Num(0))
                    }
                };
            }
            "break" | "brea" => return Flow::Break,
            "continue" | "con" | "cont" | "conti" | "contin" | "continu" => {
                return Flow::Continue;
            }
            "finish" | "fini" | "finis" => return Flow::Return(Val::Num(0)),
            "throw" => {
                let e = text[5..].trim();
                let msg = eval_str(vim, host, e)
                    .map(|v| v.to_str())
                    .unwrap_or_else(|e| e);
                fail(errors, msg);
                check_abort!();
                i += 1;
                continue;
            }
            _ => {}
        }
        if let Err(e) = super::ex::run(vim, host, text) {
            fail(errors, e);
            check_abort!();
        }
        i += 1;
    }
    Flow::Normal
}

fn eval_str(vim: &mut VimState, host: &mut dyn Host, src: &str) -> R<Val> {
    let mut cx = Ctx::new(vim, host);
    eval::eval(src, &mut cx)
}

/// Store a variable, respecting function scope.
pub(crate) fn set_var(vim: &mut VimState, name: &str, v: Val) {
    eval::set_var(vim, name, v);
}

/// `function[!] Name(a, b, ...) [range] [abort] [dict] [closure]`
fn define(vim: &mut VimState, header: &str, body: &[Line]) -> R<()> {
    let rest = header[word(header).len()..].trim_start();
    let bang = rest.starts_with('!');
    let rest = rest.trim_start_matches('!').trim();
    let open = rest.find('(').ok_or("E124: Missing '('")?;
    let name = normalize_name(rest[..open].trim());
    let close = rest.rfind(')').ok_or("E124: Missing ')'")?;
    let params_src = &rest[open + 1..close];
    let attrs = &rest[close + 1..];
    let mut params = Vec::new();
    let mut varargs = false;
    for p in params_src
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        if p == "..." {
            varargs = true;
        } else {
            // `def` style `name: type` and defaults `name = 1`.
            let p = p.split([':', '=']).next().unwrap_or(p).trim();
            params.push(p.to_string());
        }
    }
    if !bang && vim.funcs.contains_key(&name) {
        // Re-sourcing a vimrc redefines its functions; accept it.
    }
    vim.funcs.insert(
        name,
        UserFn {
            params,
            varargs,
            range: attrs.contains("range"),
            abort: attrs.contains("abort"),
            body: body.iter().map(|l| l.text.clone()).collect(),
        },
    );
    Ok(())
}

/// `s:Fn`, `<SID>Fn` and `g:Fn` name the same function here.
pub(crate) fn normalize_name(name: &str) -> String {
    let n = name
        .strip_prefix("<SID>")
        .or_else(|| name.strip_prefix("<sid>"))
        .or_else(|| name.strip_prefix("s:"))
        .or_else(|| name.strip_prefix("g:"))
        .unwrap_or(name);
    n.to_string()
}

/// Call a user function.
pub(crate) fn call(
    vim: &mut VimState,
    host: &mut dyn Host,
    name: &str,
    args: Vec<Val>,
    range: Option<(usize, usize)>,
) -> Option<R<Val>> {
    let f = vim.funcs.get(&normalize_name(name))?.clone();
    if vim.frames.len() >= 100 {
        return Some(Err(format!(
            "E132: Function call depth is higher than 'maxfuncdepth': {name}"
        )));
    }
    if args.len() < f.params.len() {
        return Some(Err(format!(
            "E119: Not enough arguments for function: {name}"
        )));
    }
    if args.len() > f.params.len() && !f.varargs {
        return Some(Err(format!(
            "E118: Too many arguments for function: {name}"
        )));
    }
    let mut frame = BTreeMap::new();
    for (p, v) in f.params.iter().zip(args.iter()) {
        frame.insert(format!("a:{p}"), v.clone());
    }
    let extra: Vec<Val> = args.iter().skip(f.params.len()).cloned().collect();
    frame.insert("a:0".into(), Val::Num(extra.len() as i64));
    for (i, v) in extra.iter().enumerate() {
        frame.insert(format!("a:{}", i + 1), v.clone());
    }
    frame.insert("a:000".into(), Val::List(extra));
    let (first, last) = range.unwrap_or_else(|| {
        let l = host.ed_ref().map_or(0, |ed| ed.cursor_line);
        (l, l)
    });
    frame.insert("a:firstline".into(), Val::Num(first as i64 + 1));
    frame.insert("a:lastline".into(), Val::Num(last as i64 + 1));
    let lines: Vec<Line> = f
        .body
        .iter()
        .enumerate()
        .map(|(n, t)| Line {
            n: n + 1,
            text: t.clone(),
        })
        .collect();
    let mut result = Val::Num(0);
    let mut errors = Vec::new();
    // Without the `range` attribute the function runs once per line.
    let passes: Vec<usize> = if range.is_some() && !f.range {
        (first..=last).collect()
    } else {
        vec![usize::MAX]
    };
    for line in passes {
        if line != usize::MAX
            && let Some(ed) = host.ed()
        {
            ed.cursor_line = line;
            ed.cursor_col = 0;
        }
        vim.frames.push(frame.clone());
        let flow = run(vim, host, &lines, &mut errors, f.abort);
        vim.frames.pop();
        if let Flow::Return(v) = flow {
            result = v;
        }
        if !errors.is_empty() {
            break;
        }
    }
    if let Some((n, e)) = errors.into_iter().next() {
        return Some(Err(format!("{e} (in function {name}, line {n})")));
    }
    Some(Ok(result))
}

/// Source a whole script (vimrc). Returns error lines.
pub(crate) fn source(vim: &mut VimState, host: &mut dyn Host, text: &str) -> Vec<String> {
    let lines = logical_lines(text);
    let mut errors = Vec::new();
    run(vim, host, &lines, &mut errors, false);
    errors
        .into_iter()
        .map(|(n, e)| format!("line {n:>3}: {e}"))
        .collect()
}
