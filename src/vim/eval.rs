//! Vim script expressions: the expression register (`<C-r>=`, `"=`),
//! `\=` in `:s`, `:echo`, `:let`, `:execute`, `:call` and `:if`.
//!
//! Supports numbers, floats, strings, lists and dictionaries; the usual
//! operators with Vim's precedence (`?:` `||` `&&` comparisons with `#`/`?`,
//! `=~` `!~` `is`, `+ - . ..`, `* / %`, unary `! - +`, indexing and slices);
//! variables (`g:` `b:` `w:` `t:` `s:` `l:` `v:`), `&options`, `@registers`,
//! `$ENV`; and the built-in functions people put in mappings and vimrcs.

use std::collections::BTreeMap;

use super::exec::Tx;
use super::register::{Reg, RegKind};
use super::text::{Pos, Text};
use super::{Host, VimState};
use crate::editor::Editor;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Val {
    Num(i64),
    Float(f64),
    Str(String),
    List(Vec<Val>),
    Dict(BTreeMap<String, Val>),
}

type R<T> = Result<T, String>;

impl Val {
    pub(crate) fn str(s: impl Into<String>) -> Self {
        Val::Str(s.into())
    }

    /// The value as a String, as `:echo` and `.` see it.
    pub(crate) fn to_str(&self) -> String {
        match self {
            Val::Num(n) => n.to_string(),
            Val::Float(f) => fmt_float(*f),
            Val::Str(s) => s.clone(),
            Val::List(_) | Val::Dict(_) => self.repr(),
        }
    }

    /// `string()`: how Vim shows a value inside a List.
    pub(crate) fn repr(&self) -> String {
        match self {
            Val::Str(s) => format!("'{}'", s.replace('\'', "''")),
            Val::List(l) => {
                let items: Vec<String> = l.iter().map(Val::repr).collect();
                format!("[{}]", items.join(", "))
            }
            Val::Dict(d) => {
                let items: Vec<String> = d
                    .iter()
                    .map(|(k, v)| format!("'{}': {}", k, v.repr()))
                    .collect();
                format!("{{{}}}", items.join(", "))
            }
            v => v.to_str(),
        }
    }

    pub(crate) fn to_num(&self) -> i64 {
        match self {
            Val::Num(n) => *n,
            Val::Float(f) => *f as i64,
            Val::Str(s) => str2nr(s, 10),
            Val::List(_) | Val::Dict(_) => 0,
        }
    }

    fn to_f(&self) -> f64 {
        match self {
            Val::Float(f) => *f,
            v => v.to_num() as f64,
        }
    }

    pub(crate) fn truthy(&self) -> bool {
        match self {
            Val::Float(f) => *f != 0.0,
            Val::List(l) => !l.is_empty(),
            Val::Dict(d) => !d.is_empty(),
            v => v.to_num() != 0,
        }
    }

    /// Text for a register, `\=` or `<C-r>=`: Lists become lines.
    pub(crate) fn to_text(&self) -> String {
        match self {
            Val::List(l) => {
                let lines: Vec<String> = l.iter().map(Val::to_str).collect();
                lines.join("\n")
            }
            v => v.to_str(),
        }
    }
}

fn fmt_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    // Vim prints Floats with "%g" and 6 significant digits.
    if f != 0.0 && (f.abs() >= 1e15 || f.abs() < 1e-4) {
        let s = format!("{f:.5e}");
        let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
        let m = trim_zeros(m);
        let exp: i32 = e.parse().unwrap_or(0);
        return format!("{m}e{}{:02}", if exp < 0 { "-" } else { "" }, exp.abs());
    }
    let digits = 6 - (f.abs().log10().floor() as i32 + 1).clamp(-10, 6);
    let s = format!("{:.*}", digits.max(1) as usize, f);
    trim_zeros(&s)
}

fn trim_zeros(s: &str) -> String {
    if !s.contains('.') {
        return format!("{s}.0");
    }
    let t = s.trim_end_matches('0');
    if t.ends_with('.') {
        format!("{t}0")
    } else {
        t.to_string()
    }
}

/// Vim's string-to-number: leading digits, with `0x`/`0b`/`0o` prefixes.
pub(crate) fn str2nr(s: &str, base: u32) -> i64 {
    let t = s.trim_start();
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let lower = t.to_ascii_lowercase();
    let (base, digits) = if (base == 16 || base == 10) && lower.starts_with("0x") {
        (16, &t[2..])
    } else if (base == 2 || base == 10) && lower.starts_with("0b") {
        (2, &t[2..])
    } else if (base == 8 || base == 10) && lower.starts_with("0o") {
        (8, &t[2..])
    } else {
        (base, t)
    };
    let mut n: i64 = 0;
    for c in digits.chars() {
        let Some(d) = c.to_digit(base) else {
            break;
        };
        n = n.saturating_mul(base as i64).saturating_add(d as i64);
    }
    if neg { -n } else { n }
}

/// What an expression can see.
pub(crate) struct Ctx<'a, 'h> {
    pub(crate) vim: &'a mut VimState,
    pub(crate) host: Option<&'a mut (dyn Host + 'h)>,
    /// A read-only buffer when the host is busy (`:s` with `\=`).
    pub(crate) ed: Option<&'a Editor>,
    /// `submatch()` groups for `\=`.
    pub(crate) submatch: Vec<String>,
    /// `line('.')` override (the line being substituted).
    pub(crate) lnum: Option<usize>,
}

impl<'a, 'h> Ctx<'a, 'h> {
    pub(crate) fn new(vim: &'a mut VimState, host: &'a mut (dyn Host + 'h)) -> Self {
        Self {
            vim,
            host: Some(host),
            ed: None,
            submatch: Vec::new(),
            lnum: None,
        }
    }

    fn editor(&self) -> Option<&Editor> {
        if let Some(ed) = self.ed {
            return Some(ed);
        }
        self.host.as_ref().and_then(|h| h.ed_ref())
    }
}

/// Evaluate a whole expression.
pub(crate) fn eval(src: &str, cx: &mut Ctx<'_, '_>) -> R<Val> {
    let chars: Vec<char> = src.chars().collect();
    let mut p = Parser {
        s: &chars,
        i: 0,
        skip: 0,
    };
    let v = p.expr1(cx)?;
    p.ws();
    if p.i < chars.len() {
        return Err(format!(
            "E15: Invalid expression: \"{}\"",
            chars[p.i..].iter().collect::<String>()
        ));
    }
    Ok(v)
}

/// Evaluate one expression at the start of `src`; returns it and the rest
/// (`:echo a b`, `:call f(1) | ...`).
pub(crate) fn eval_prefix(src: &str, cx: &mut Ctx<'_, '_>) -> R<(Val, String)> {
    let chars: Vec<char> = src.chars().collect();
    let mut p = Parser {
        s: &chars,
        i: 0,
        skip: 0,
    };
    let v = p.expr1(cx)?;
    p.ws();
    Ok((v, chars[p.i..].iter().collect()))
}

struct Parser<'s> {
    s: &'s [char],
    i: usize,
    /// Inside the branch `?:`/`&&`/`||` does not take: parse, don't run.
    skip: u32,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && matches!(self.s[self.i], ' ' | '\t') {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }

    fn at(&self, t: &str) -> bool {
        let n = t.chars().count();
        self.i + n <= self.s.len() && self.s[self.i..self.i + n].iter().copied().eq(t.chars())
    }

    fn eat(&mut self, t: &str) -> bool {
        self.ws();
        if self.at(t) {
            self.i += t.chars().count();
            true
        } else {
            false
        }
    }

    fn expect(&mut self, t: &str) -> R<()> {
        if self.eat(t) {
            Ok(())
        } else {
            Err(format!("E15: Missing '{t}'"))
        }
    }

    // expr1: a ? b : c, a ?? b
    fn expr1(&mut self, cx: &mut Ctx<'_, '_>) -> R<Val> {
        let cond = self.expr2(cx)?;
        self.ws();
        if self.at("??") {
            self.i += 2;
            let take = cond.truthy();
            if take {
                self.skip += 1;
            }
            let other = self.expr1(cx);
            if take {
                self.skip -= 1;
            }
            let other = other?;
            return Ok(if take { cond } else { other });
        }
        if self.peek() == Some('?') {
            self.i += 1;
            let t = cond.truthy();
            if !t {
                self.skip += 1;
            }
            let a = self.expr1(cx);
            if !t {
                self.skip -= 1;
            }
            let a = a?;
            self.expect(":")?;
            if t {
                self.skip += 1;
            }
            let b = self.expr1(cx);
            if t {
                self.skip -= 1;
            }
            let b = b?;
            return Ok(if t { a } else { b });
        }
        Ok(cond)
    }

    fn expr2(&mut self, cx: &mut Ctx<'_, '_>) -> R<Val> {
        let mut v = self.expr3(cx)?;
        while self.eat("||") {
            let done = v.truthy();
            if done {
                self.skip += 1;
            }
            let r = self.expr3(cx);
            if done {
                self.skip -= 1;
            }
            let r = r?;
            v = Val::Num(i64::from(done || r.truthy()));
        }
        Ok(v)
    }

    fn expr3(&mut self, cx: &mut Ctx<'_, '_>) -> R<Val> {
        let mut v = self.expr4(cx)?;
        while self.eat("&&") {
            let fail = !v.truthy();
            if fail {
                self.skip += 1;
            }
            let r = self.expr4(cx);
            if fail {
                self.skip -= 1;
            }
            let r = r?;
            v = Val::Num(i64::from(!fail && r.truthy()));
        }
        Ok(v)
    }

    fn expr4(&mut self, cx: &mut Ctx<'_, '_>) -> R<Val> {
        let a = self.expr5(cx)?;
        self.ws();
        const OPS: &[&str] = &["==", "!=", ">=", "<=", "=~", "!~", ">", "<", "isnot", "is"];
        let Some(op) = OPS.iter().find(|o| self.at(o)).copied() else {
            return Ok(a);
        };
        if op.starts_with("is")
            && self
                .s
                .get(self.i + op.len())
                .is_some_and(|c| c.is_alphanumeric() && *c != '#' && *c != '?')
        {
            return Ok(a);
        }
        self.i += op.len();
        let case = match self.peek() {
            Some('#') => {
                self.i += 1;
                Some(false)
            }
            Some('?') => {
                self.i += 1;
                Some(true)
            }
            _ => None,
        };
        let b = self.expr5(cx)?;
        if self.skip > 0 {
            return Ok(Val::Num(0));
        }
        let ic = case.unwrap_or(cx.vim.opts.ignorecase);
        let r = match op {
            "=~" | "!~" => {
                let opts = super::search::Options {
                    ignorecase: ic,
                    smartcase: false,
                    magic: true,
                    last_sub: None,
                };
                let re = super::search::compile(&b.to_str(), &opts)?;
                let hit = re.find_iter(&a.to_str()).next().is_some();
                hit == (op == "=~")
            }
            "is" | "isnot" => (a == b) == (op == "is"),
            _ => {
                let ord = compare(&a, &b, ic);
                match op {
                    "==" => ord == std::cmp::Ordering::Equal,
                    "!=" => ord != std::cmp::Ordering::Equal,
                    ">" => ord == std::cmp::Ordering::Greater,
                    ">=" => ord != std::cmp::Ordering::Less,
                    "<" => ord == std::cmp::Ordering::Less,
                    _ => ord != std::cmp::Ordering::Greater,
                }
            }
        };
        Ok(Val::Num(i64::from(r)))
    }

    fn expr5(&mut self, cx: &mut Ctx<'_, '_>) -> R<Val> {
        let mut v = self.expr6(cx)?;
        loop {
            self.ws();
            let op = if self.at("..") {
                self.i += 2;
                '.'
            } else {
                match self.peek() {
                    Some('.') if !self.s.get(self.i + 1).is_some_and(|c| *c == '=') => {
                        self.i += 1;
                        '.'
                    }
                    Some(c @ ('+' | '-')) if !self.s.get(self.i + 1).is_some_and(|n| *n == '=') => {
                        self.i += 1;
                        c
                    }
                    _ => return Ok(v),
                }
            };
            let b = self.expr6(cx)?;
            if self.skip > 0 {
                continue;
            }
            v = match op {
                '.' => Val::Str(v.to_str() + &b.to_str()),
                '+' => match (v, b) {
                    (Val::List(mut a), Val::List(b)) => {
                        a.extend(b);
                        Val::List(a)
                    }
                    (a, b) => arith(&a, &b, '+')?,
                },
                _ => arith(&v, &b, '-')?,
            };
        }
    }

    fn expr6(&mut self, cx: &mut Ctx<'_, '_>) -> R<Val> {
        let mut v = self.expr7(cx)?;
        loop {
            self.ws();
            let op = match self.peek() {
                Some(c @ ('*' | '/' | '%')) if self.s.get(self.i + 1) != Some(&'=') => c,
                _ => return Ok(v),
            };
            self.i += 1;
            let b = self.expr7(cx)?;
            if self.skip > 0 {
                continue;
            }
            v = arith(&v, &b, op)?;
        }
    }

    fn expr7(&mut self, cx: &mut Ctx<'_, '_>) -> R<Val> {
        self.ws();
        match self.peek() {
            Some('!') => {
                self.i += 1;
                let v = self.expr7(cx)?;
                Ok(Val::Num(i64::from(!v.truthy())))
            }
            Some('-') => {
                self.i += 1;
                let v = self.expr7(cx)?;
                Ok(match v {
                    Val::Float(f) => Val::Float(-f),
                    v => Val::Num(-v.to_num()),
                })
            }
            Some('+') => {
                self.i += 1;
                let v = self.expr7(cx)?;
                Ok(match v {
                    Val::Float(f) => Val::Float(f),
                    v => Val::Num(v.to_num()),
                })
            }
            _ => self.expr8(cx),
        }
    }

    // Indexing, slices and `->method()`.
    fn expr8(&mut self, cx: &mut Ctx<'_, '_>) -> R<Val> {
        let mut v = self.expr9(cx)?;
        loop {
            if self.peek() == Some('[') {
                self.i += 1;
                self.ws();
                let a = if self.peek() == Some(':') {
                    None
                } else {
                    Some(self.expr1(cx)?)
                };
                self.ws();
                if self.peek() == Some(':') {
                    self.i += 1;
                    self.ws();
                    let b = if self.peek() == Some(']') {
                        None
                    } else {
                        Some(self.expr1(cx)?)
                    };
                    self.expect("]")?;
                    v = slice(&v, a.map(|a| a.to_num()), b.map(|b| b.to_num()));
                } else {
                    self.expect("]")?;
                    let a = a.ok_or("E15: Invalid expression")?;
                    v = index(&v, &a)?;
                }
            } else if self.peek() == Some('.')
                && matches!(v, Val::Dict(_))
                && self
                    .s
                    .get(self.i + 1)
                    .is_some_and(|c| c.is_alphanumeric() || *c == '_')
            {
                self.i += 1;
                let key = self.name();
                v = index(&v, &Val::Str(key))?;
            } else if self.at("->") {
                self.i += 2;
                let name = self.name();
                self.expect("(")?;
                let mut args = vec![v];
                args.extend(self.args(cx)?);
                v = if self.skip > 0 {
                    Val::Num(0)
                } else {
                    call(cx, &name, args)?
                };
            } else {
                return Ok(v);
            }
        }
    }

    fn name(&mut self) -> String {
        let st = self.i;
        while self.i < self.s.len() {
            let c = self.s[self.i];
            if c.is_alphanumeric() || c == '_' || c == '#' {
                self.i += 1;
            } else if c == ':'
                && self.i == st + 1
                && "gbwtslva".contains(self.s[st])
                && self
                    .s
                    .get(self.i + 1)
                    .is_some_and(|c| c.is_alphanumeric() || *c == '_')
            {
                self.i += 1;
            } else {
                break;
            }
        }
        self.s[st..self.i].iter().collect()
    }

    fn args(&mut self, cx: &mut Ctx<'_, '_>) -> R<Vec<Val>> {
        let mut out = Vec::new();
        self.ws();
        if self.eat(")") {
            return Ok(out);
        }
        loop {
            out.push(self.expr1(cx)?);
            if self.eat(")") {
                return Ok(out);
            }
            self.expect(",")?;
            if self.eat(")") {
                return Ok(out);
            }
        }
    }

    fn expr9(&mut self, cx: &mut Ctx<'_, '_>) -> R<Val> {
        self.ws();
        let Some(c) = self.peek() else {
            return Err("E15: Invalid expression: \"\"".into());
        };
        match c {
            '0'..='9' => Ok(self.number()),
            '"' => {
                self.i += 1;
                let mut out = String::new();
                loop {
                    let Some(c) = self.peek() else {
                        return Err("E114: Missing double quote".into());
                    };
                    self.i += 1;
                    match c {
                        '"' => break,
                        '\\' => {
                            let Some(n) = self.peek() else {
                                break;
                            };
                            self.i += 1;
                            match n {
                                'n' => out.push('\n'),
                                'r' => out.push('\r'),
                                't' => out.push('\t'),
                                'e' => out.push('\x1b'),
                                'b' => out.push('\x08'),
                                '\\' => out.push('\\'),
                                '"' => out.push('"'),
                                'x' | 'X' | 'u' | 'U' => {
                                    let max = match n {
                                        'x' | 'X' => 2,
                                        'u' => 4,
                                        _ => 8,
                                    };
                                    let st = self.i;
                                    while self.i < self.s.len()
                                        && self.i - st < max
                                        && self.s[self.i].is_ascii_hexdigit()
                                    {
                                        self.i += 1;
                                    }
                                    let hex: String = self.s[st..self.i].iter().collect();
                                    match u32::from_str_radix(&hex, 16)
                                        .ok()
                                        .and_then(char::from_u32)
                                    {
                                        Some(ch) => out.push(ch),
                                        None => out.push(n),
                                    }
                                }
                                '<' => {
                                    // "\<Esc>" and friends: Vim key notation.
                                    let st = self.i;
                                    while self.i < self.s.len() && self.s[self.i] != '>' {
                                        self.i += 1;
                                    }
                                    let inner: String = self.s[st..self.i].iter().collect();
                                    self.i = (self.i + 1).min(self.s.len());
                                    out.push_str(&key_notation(&inner));
                                }
                                d @ '0'..='7' => {
                                    let st = self.i - 1;
                                    while self.i < self.s.len()
                                        && self.i - st < 3
                                        && ('0'..='7').contains(&self.s[self.i])
                                    {
                                        self.i += 1;
                                    }
                                    let oct: String = self.s[st..self.i].iter().collect();
                                    let _ = d;
                                    if let Some(ch) =
                                        u32::from_str_radix(&oct, 8).ok().and_then(char::from_u32)
                                    {
                                        out.push(ch);
                                    }
                                }
                                other => out.push(other),
                            }
                        }
                        c => out.push(c),
                    }
                }
                Ok(Val::Str(out))
            }
            '\'' => {
                self.i += 1;
                let mut out = String::new();
                loop {
                    let Some(c) = self.peek() else {
                        return Err("E115: Missing single quote".into());
                    };
                    self.i += 1;
                    if c == '\'' {
                        if self.peek() == Some('\'') {
                            self.i += 1;
                            out.push('\'');
                            continue;
                        }
                        break;
                    }
                    out.push(c);
                }
                Ok(Val::Str(out))
            }
            '[' => {
                self.i += 1;
                let mut out = Vec::new();
                loop {
                    if self.eat("]") {
                        break;
                    }
                    out.push(self.expr1(cx)?);
                    if self.eat("]") {
                        break;
                    }
                    self.expect(",")?;
                }
                Ok(Val::List(out))
            }
            '{' => {
                self.i += 1;
                let mut out = BTreeMap::new();
                loop {
                    if self.eat("}") {
                        break;
                    }
                    let k = self.expr1(cx)?.to_str();
                    self.expect(":")?;
                    let v = self.expr1(cx)?;
                    out.insert(k, v);
                    if self.eat("}") {
                        break;
                    }
                    self.expect(",")?;
                }
                Ok(Val::Dict(out))
            }
            '(' => {
                self.i += 1;
                let v = self.expr1(cx)?;
                self.expect(")")?;
                Ok(v)
            }
            '&' => {
                self.i += 1;
                let mut name = self.name();
                for scope in ["l:", "g:"] {
                    if let Some(n) = name.strip_prefix(scope) {
                        name = n.to_string();
                    }
                }
                if self.skip > 0 {
                    return Ok(Val::Num(0));
                }
                Ok(option_value(cx, &name))
            }
            '@' => {
                self.i += 1;
                let r = self.peek().unwrap_or('"');
                self.i += 1;
                if self.skip > 0 {
                    return Ok(Val::Num(0));
                }
                let r = if r == '@' { '"' } else { r };
                Ok(Val::Str(read_register(cx, r)))
            }
            '$' => {
                self.i += 1;
                let name = self.name();
                Ok(Val::Str(std::env::var(&name).unwrap_or_default()))
            }
            c if c.is_alphabetic() || c == '_' => {
                let name = self.name();
                if self.peek() == Some('(') {
                    self.i += 1;
                    let args = self.args(cx)?;
                    if self.skip > 0 {
                        return Ok(Val::Num(0));
                    }
                    return call(cx, &name, args);
                }
                if self.skip > 0 {
                    return Ok(Val::Num(0));
                }
                variable(cx, &name)
            }
            _ => Err(format!(
                "E15: Invalid expression: \"{}\"",
                self.s[self.i..].iter().collect::<String>()
            )),
        }
    }

    fn number(&mut self) -> Val {
        let st = self.i;
        let lower = |i: usize| self.s.get(i).map(|c| c.to_ascii_lowercase());
        if self.s[st] == '0' && matches!(lower(st + 1), Some('x' | 'b' | 'o')) {
            let base = match lower(st + 1) {
                Some('x') => 16,
                Some('b') => 2,
                _ => 8,
            };
            self.i += 2;
            let ds = self.i;
            while self.i < self.s.len() && self.s[self.i].is_digit(base) {
                self.i += 1;
            }
            let digits: String = self.s[ds..self.i].iter().collect();
            return Val::Num(i64::from_str_radix(&digits, base).unwrap_or(0));
        }
        while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
            self.i += 1;
        }
        if self.peek() == Some('.') && self.s.get(self.i + 1).is_some_and(char::is_ascii_digit) {
            self.i += 1;
            while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                self.i += 1;
            }
            if matches!(self.peek(), Some('e' | 'E')) {
                let save = self.i;
                self.i += 1;
                if matches!(self.peek(), Some('+' | '-')) {
                    self.i += 1;
                }
                if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                        self.i += 1;
                    }
                } else {
                    self.i = save;
                }
            }
            let s: String = self.s[st..self.i].iter().collect();
            return Val::Float(s.parse().unwrap_or(0.0));
        }
        let s: String = self.s[st..self.i].iter().collect();
        // A leading zero means octal in legacy Vim script.
        if s.len() > 1 && s.starts_with('0') && s.chars().all(|c| ('0'..='7').contains(&c)) {
            return Val::Num(i64::from_str_radix(&s, 8).unwrap_or(0));
        }
        Val::Num(s.parse().unwrap_or(i64::MAX))
    }
}

/// `"\<Esc>"`-style key notation inside a double-quoted string.
fn key_notation(inner: &str) -> String {
    let keys = super::keys::parse(&format!("<{inner}>"), &[]);
    super::ops::keys_to_text(&keys)
}

fn compare(a: &Val, b: &Val, ic: bool) -> std::cmp::Ordering {
    match (a, b) {
        (Val::Str(x), Val::Str(y)) => {
            if ic {
                x.to_lowercase().cmp(&y.to_lowercase())
            } else {
                x.cmp(y)
            }
        }
        (Val::Float(_), _) | (_, Val::Float(_)) => a
            .to_f()
            .partial_cmp(&b.to_f())
            .unwrap_or(std::cmp::Ordering::Equal),
        (Val::List(x), Val::List(y)) => {
            if x.len() != y.len() {
                return std::cmp::Ordering::Less;
            }
            for (p, q) in x.iter().zip(y) {
                let o = compare(p, q, ic);
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
            }
            std::cmp::Ordering::Equal
        }
        _ => a.to_num().cmp(&b.to_num()),
    }
}

fn arith(a: &Val, b: &Val, op: char) -> R<Val> {
    if matches!(a, Val::Float(_)) || matches!(b, Val::Float(_)) {
        let (x, y) = (a.to_f(), b.to_f());
        return Ok(Val::Float(match op {
            '+' => x + y,
            '-' => x - y,
            '*' => x * y,
            '/' => x / y,
            _ => return Err("E804: Cannot use '%' with Float".into()),
        }));
    }
    let (x, y) = (a.to_num(), b.to_num());
    Ok(Val::Num(match op {
        '+' => x.wrapping_add(y),
        '-' => x.wrapping_sub(y),
        '*' => x.wrapping_mul(y),
        // Vim: dividing by zero gives a large number, not an error.
        '/' => {
            if y == 0 {
                if x == 0 {
                    i64::MIN
                } else if x > 0 {
                    i64::MAX
                } else {
                    -i64::MAX
                }
            } else {
                x.wrapping_div(y)
            }
        }
        _ => {
            if y == 0 {
                0
            } else {
                x.wrapping_rem(y)
            }
        }
    }))
}

fn index(v: &Val, i: &Val) -> R<Val> {
    match v {
        Val::List(l) => {
            let n = i.to_num();
            let idx = if n < 0 { l.len() as i64 + n } else { n };
            l.get(idx as usize)
                .cloned()
                .filter(|_| idx >= 0)
                .ok_or_else(|| format!("E684: List index out of range: {n}"))
        }
        Val::Dict(d) => d
            .get(&i.to_str())
            .cloned()
            .ok_or_else(|| format!("E716: Key not present in Dictionary: \"{}\"", i.to_str())),
        v => {
            let s: Vec<char> = v.to_str().chars().collect();
            let n = i.to_num();
            Ok(Val::Str(if n < 0 {
                String::new()
            } else {
                s.get(n as usize).map(|c| c.to_string()).unwrap_or_default()
            }))
        }
    }
}

fn slice(v: &Val, a: Option<i64>, b: Option<i64>) -> Val {
    let fix = |n: i64, len: usize| -> i64 { if n < 0 { len as i64 + n } else { n } };
    match v {
        Val::List(l) => {
            let len = l.len();
            let s = fix(a.unwrap_or(0), len).max(0) as usize;
            let e = b.map_or(len as i64 - 1, |b| fix(b, len));
            if e < s as i64 {
                return Val::List(Vec::new());
            }
            Val::List(l.iter().skip(s).take(e as usize + 1 - s).cloned().collect())
        }
        v => {
            let chars: Vec<char> = v.to_str().chars().collect();
            let len = chars.len();
            let s = fix(a.unwrap_or(0), len).max(0) as usize;
            let e = b.map_or(len as i64 - 1, |b| fix(b, len));
            if e < s as i64 || s >= len {
                return Val::Str(String::new());
            }
            let e = (e as usize).min(len - 1);
            Val::Str(chars[s..=e].iter().collect())
        }
    }
}

fn var_key(name: &str) -> String {
    for p in ["g:", "s:", "l:"] {
        if let Some(n) = name.strip_prefix(p) {
            return n.to_string();
        }
    }
    name.to_string()
}

fn variable(cx: &mut Ctx<'_, '_>, name: &str) -> R<Val> {
    match name {
        "v:true" | "true" => return Ok(Val::Num(1)),
        "v:false" | "false" | "v:null" | "null" | "v:none" => return Ok(Val::Num(0)),
        "v:version" => return Ok(Val::Num(901)),
        "v:count" | "v:count1" => {
            let fallback = i64::from(name == "v:count1");
            return Ok(cx.vim.vars.get(name).cloned().unwrap_or(Val::Num(fallback)));
        }
        "v:lnum" => {
            return Ok(Val::Num(
                cx.lnum
                    .map_or_else(|| cur_line(cx) as i64 + 1, |l| l as i64 + 1),
            ));
        }
        "v:register" => return Ok(Val::str("\"")),
        "v:errmsg" => return Ok(Val::Str(cx.vim.last_error.clone())),
        "v:shell_error" => return Ok(Val::Num(cx.vim.shell_error)),
        "v:servername" | "v:progname" => return Ok(Val::str("vtcode")),
        _ => {}
    }
    if let Some(frame) = cx.vim.frames.last() {
        if name.starts_with("a:") {
            return frame
                .get(name)
                .cloned()
                .ok_or_else(|| format!("E121: Undefined variable: {name}"));
        }
        if let Some(v) = local_key(name).and_then(|k| frame.get(&k)) {
            return Ok(v.clone());
        }
    }
    if name == "mapleader" || name == "g:mapleader" {
        if let Some(v) = cx.vim.vars.get("mapleader") {
            return Ok(v.clone());
        }
        return Ok(Val::Str(super::keys::showcmd(&cx.vim.leader)));
    }
    cx.vim
        .vars
        .get(&var_key(name))
        .cloned()
        .ok_or_else(|| format!("E121: Undefined variable: {name}"))
}

/// Inside a function, bare names and `l:` names are locals.
fn local_key(name: &str) -> Option<String> {
    if let Some(n) = name.strip_prefix("l:") {
        return Some(format!("l:{n}"));
    }
    if name.len() > 1 && name.as_bytes()[1] == b':' {
        return None;
    }
    Some(format!("l:{name}"))
}

/// Store a variable (`:let`).
pub(crate) fn set_var(vim: &mut VimState, name: &str, v: Val) {
    if let Some(frame) = vim.frames.last_mut()
        && let Some(k) = local_key(name)
    {
        frame.insert(k, v);
        return;
    }
    vim.vars.insert(var_key(name), v);
}

pub(crate) fn unset_var(vim: &mut VimState, name: &str) -> bool {
    if let Some(frame) = vim.frames.last_mut()
        && let Some(k) = local_key(name)
        && frame.remove(&k).is_some()
    {
        return true;
    }
    vim.vars.remove(&var_key(name)).is_some()
}

pub(crate) fn var_exists(vim: &VimState, name: &str) -> bool {
    if let Some(frame) = vim.frames.last()
        && let Some(k) = local_key(name)
        && frame.contains_key(&k)
    {
        return true;
    }
    if name.starts_with("a:") {
        return vim.frames.last().is_some_and(|f| f.contains_key(name));
    }
    vim.vars.contains_key(&var_key(name))
}

fn option_value(cx: &mut Ctx<'_, '_>, name: &str) -> Val {
    // Read through `:set name?` so every option the engine knows works.
    let Some(host) = cx.host.as_deref_mut() else {
        return match name {
            "ignorecase" | "ic" => Val::Num(i64::from(cx.vim.opts.ignorecase)),
            "tabstop" | "ts" | "shiftwidth" | "sw" => {
                Val::Num(cx.editor().map_or(8, |ed| super::exec::tabstop(ed) as i64))
            }
            _ => Val::Num(0),
        };
    };
    let saved = cx.vim.message.take();
    let r = super::ex::run(cx.vim, host, &format!("set {name}?"));
    let msg = std::mem::replace(&mut cx.vim.message, saved);
    let text = match r {
        // Host options answer through an "error" marked with U+0001.
        Err(e) => match e.strip_prefix('\u{1}') {
            Some(info) => info.to_string(),
            None => return Val::Num(0),
        },
        Ok(()) => msg.map(|m| m.text).unwrap_or_default(),
    };
    let text = text.trim();
    if let Some((_, v)) = text.split_once('=') {
        return if !v.is_empty() && v.chars().all(|c| c.is_ascii_digit()) {
            Val::Num(v.parse().unwrap_or(0))
        } else {
            Val::str(v)
        };
    }
    Val::Num(i64::from(!text.starts_with("no")))
}

fn read_register(cx: &mut Ctx<'_, '_>, r: char) -> String {
    match cx.host.as_deref_mut() {
        Some(host) => super::ops::read_reg(cx.vim, host, Some(r))
            .map(|reg| reg.text)
            .unwrap_or_default(),
        None => {
            let mut clip = NoClip;
            cx.vim
                .regs
                .get(r, &mut clip, None)
                .map(|reg| reg.text)
                .unwrap_or_default()
        }
    }
}

struct NoClip;
impl super::register::Clipboard for NoClip {
    fn get(&mut self) -> Option<String> {
        None
    }
    fn set(&mut self, _text: &str, _linewise: bool) {}
}

fn cur_line(cx: &Ctx<'_, '_>) -> usize {
    cx.editor().map_or(0, |ed| ed.cursor_line)
}

/// A line number argument: `.`, `$`, `'x`, `w0`, `w$`, `v` or a number.
fn lnum_arg(cx: &Ctx<'_, '_>, v: &Val) -> Option<usize> {
    let ed = cx.editor()?;
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    match v {
        Val::Str(s) if !s.is_empty() && !s.chars().all(|c| c.is_ascii_digit()) => {
            let line = match s.as_str() {
                "." => cx.lnum.unwrap_or(ed.cursor_line),
                "$" => t.last_line(),
                "w0" => ed.scroll_offset,
                "w$" => (ed.scroll_offset + ed.viewport_height.max(1) - 1).min(t.last_line()),
                "v" => {
                    if cx.vim.is_visual() {
                        cx.vim.visual_anchor.line
                    } else {
                        ed.cursor_line
                    }
                }
                s if s.starts_with('\'') => {
                    let m = s.chars().nth(1)?;
                    ed.vim_buf.marks.get(&m)?.line
                }
                _ => return None,
            };
            Some(line + 1)
        }
        v => {
            let n = v.to_num();
            (n >= 0).then_some(n as usize)
        }
    }
}

fn line_text(cx: &Ctx<'_, '_>, lnum: usize) -> Option<String> {
    let ed = cx.editor()?;
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    (lnum >= 1 && lnum - 1 < t.lines()).then(|| t.line_string(lnum - 1))
}

fn text_lines(v: &Val) -> Vec<String> {
    match v {
        Val::List(l) => l.iter().map(Val::to_str).collect(),
        v => vec![v.to_str()],
    }
}

fn arg(args: &[Val], i: usize) -> Val {
    args.get(i).cloned().unwrap_or(Val::Str(String::new()))
}

fn need(args: &[Val], n: usize, name: &str) -> R<()> {
    if args.len() < n {
        Err(format!("E119: Not enough arguments for function: {name}"))
    } else {
        Ok(())
    }
}

fn opts(cx: &Ctx<'_, '_>) -> super::search::Options<'static> {
    super::search::Options {
        ignorecase: cx.vim.opts.ignorecase,
        smartcase: false,
        magic: true,
        last_sub: None,
    }
}

fn list_of(v: &Val) -> Vec<Val> {
    match v {
        Val::List(l) => l.clone(),
        Val::Dict(d) => d.values().cloned().collect(),
        _ => Vec::new(),
    }
}

/// Built-in functions.
pub(crate) fn call(cx: &mut Ctx<'_, '_>, name: &str, args: Vec<Val>) -> R<Val> {
    let a0 = || arg(&args, 0);
    let a1 = || arg(&args, 1);
    Ok(match name {
        "line" => {
            need(&args, 1, name)?;
            Val::Num(lnum_arg(cx, &a0()).unwrap_or(0) as i64)
        }
        "col" | "virtcol" | "charcol" => {
            need(&args, 1, name)?;
            let Some(ed) = cx.editor() else {
                return Ok(Val::Num(0));
            };
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let spec = a0().to_str();
            let (line, col) = match spec.as_str() {
                "." => (ed.cursor_line, ed.cursor_col),
                "$" => (ed.cursor_line, t.len(ed.cursor_line)),
                "v" if cx.vim.is_visual() => (cx.vim.visual_anchor.line, cx.vim.visual_anchor.col),
                s if s.starts_with('\'') => {
                    match s.chars().nth(1).and_then(|m| ed.vim_buf.marks.get(&m)) {
                        Some(p) => (p.line, p.col),
                        None => return Ok(Val::Num(0)),
                    }
                }
                _ => (ed.cursor_line, ed.cursor_col),
            };
            let chars = t.line_chars(line);
            let col = col.min(chars.len());
            Val::Num(match name {
                "col" => chars[..col].iter().map(|c| c.len_utf8()).sum::<usize>() as i64 + 1,
                "charcol" => col as i64 + 1,
                _ => t.vcol(line, col, super::exec::tabstop(ed)) as i64 + 1,
            })
        }
        "getline" => {
            need(&args, 1, name)?;
            let a = lnum_arg(cx, &a0()).unwrap_or(0);
            if args.len() > 1 {
                let b = lnum_arg(cx, &a1()).unwrap_or(0);
                Val::List(
                    (a..=b)
                        .filter_map(|l| line_text(cx, l))
                        .map(Val::Str)
                        .collect(),
                )
            } else {
                Val::Str(line_text(cx, a).unwrap_or_default())
            }
        }
        "setline" | "append" => {
            need(&args, 2, name)?;
            let lnum = lnum_arg(cx, &a0()).unwrap_or(0);
            let lines = text_lines(&a1());
            let Some(host) = cx.host.as_deref_mut() else {
                return Err(format!("E523: Not allowed here: {name}()"));
            };
            let Some(ed) = host.ed() else {
                return Ok(Val::Num(1));
            };
            if ed.read_only {
                return Err("E21: Cannot make changes, 'modifiable' is off".into());
            }
            Val::Num(i64::from(!edit_lines(ed, lnum, &lines, name == "append")))
        }
        "indent" => {
            need(&args, 1, name)?;
            let l = lnum_arg(cx, &a0()).unwrap_or(0);
            let ts = cx.editor().map_or(8, super::exec::tabstop);
            let Some(text) = line_text(cx, l) else {
                return Ok(Val::Num(-1));
            };
            let mut w = 0;
            for c in text.chars() {
                match c {
                    ' ' => w += 1,
                    '\t' => w += ts - w % ts,
                    _ => break,
                }
            }
            Val::Num(w as i64)
        }
        "cursor" => {
            need(&args, 2, name)?;
            let l = lnum_arg(cx, &a0()).unwrap_or(1).max(1);
            let c = a1().to_num().max(1) as usize;
            if let Some(ed) = cx.host.as_deref_mut().and_then(|h| h.ed()) {
                let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                let line = (l - 1).min(t.last_line());
                // The column is a byte index.
                let chars = t.line_chars(line);
                let mut bytes = 0;
                let mut col = 0;
                while col < chars.len() && bytes + chars[col].len_utf8() < c {
                    bytes += chars[col].len_utf8();
                    col += 1;
                }
                let p = t.clamp_normal(Pos::new(line, col));
                ed.cursor_line = p.line;
                ed.cursor_col = p.col;
            }
            Val::Num(0)
        }
        "len" => match a0() {
            Val::List(l) => Val::Num(l.len() as i64),
            Val::Dict(d) => Val::Num(d.len() as i64),
            v => Val::Num(v.to_str().len() as i64),
        },
        "strlen" => Val::Num(a0().to_str().len() as i64),
        "strchars" | "strcharlen" => Val::Num(a0().to_str().chars().count() as i64),
        "strwidth" | "strdisplaywidth" => Val::Num(
            a0().to_str()
                .chars()
                .map(|c| super::text::char_width(c, 0, 8))
                .sum::<usize>() as i64,
        ),
        "toupper" => Val::Str(a0().to_str().to_uppercase()),
        "tolower" => Val::Str(a0().to_str().to_lowercase()),
        "repeat" => match a0() {
            Val::List(l) => {
                let n = a1().to_num().max(0) as usize;
                Val::List(l.iter().cloned().cycle().take(l.len() * n).collect())
            }
            v => Val::Str(v.to_str().repeat(a1().to_num().max(0) as usize)),
        },
        "string" => Val::Str(a0().repr()),
        "str2nr" => {
            let base = if args.len() > 1 {
                a1().to_num() as u32
            } else {
                10
            };
            Val::Num(str2nr(&a0().to_str(), base))
        }
        "str2float" => Val::Float(
            a0().to_str()
                .trim()
                .split(|c: char| !(c.is_ascii_digit() || ".-+eE".contains(c)))
                .next()
                .unwrap_or("")
                .parse()
                .unwrap_or(0.0),
        ),
        "float2nr" | "trunc" => match name {
            "trunc" => Val::Float(a0().to_f().trunc()),
            _ => Val::Num(a0().to_f() as i64),
        },
        "round" => Val::Float(a0().to_f().round()),
        "floor" => Val::Float(a0().to_f().floor()),
        "ceil" => Val::Float(a0().to_f().ceil()),
        "sqrt" => Val::Float(a0().to_f().sqrt()),
        "pow" => Val::Float(a0().to_f().powf(a1().to_f())),
        "abs" => match a0() {
            Val::Float(f) => Val::Float(f.abs()),
            v => Val::Num(v.to_num().abs()),
        },
        "max" | "min" => {
            let items = list_of(&a0());
            let nums = items.iter().map(Val::to_num);
            Val::Num(if name == "max" {
                nums.max().unwrap_or(0)
            } else {
                nums.min().unwrap_or(0)
            })
        }
        "range" => {
            need(&args, 1, name)?;
            let (start, end) = if args.len() == 1 {
                (0, a0().to_num() - 1)
            } else {
                (a0().to_num(), a1().to_num())
            };
            let step = args.get(2).map_or(1, Val::to_num);
            if step == 0 {
                return Err("E726: Stride is zero".into());
            }
            let mut out = Vec::new();
            let mut i = start;
            while (step > 0 && i <= end) || (step < 0 && i >= end) {
                out.push(Val::Num(i));
                if out.len() > 1_000_000 {
                    break;
                }
                i += step;
            }
            Val::List(out)
        }
        "join" => {
            let sep = if args.len() > 1 {
                a1().to_str()
            } else {
                " ".into()
            };
            let parts: Vec<String> = list_of(&a0()).iter().map(Val::to_str).collect();
            Val::Str(parts.join(&sep))
        }
        "split" => {
            let s = a0().to_str();
            let keep = args.get(2).is_some_and(Val::truthy);
            let parts: Vec<String> = if args.len() > 1 && !a1().to_str().is_empty() {
                let re = super::search::compile(&a1().to_str(), &opts(cx))?;
                let mut out = Vec::new();
                let mut last = 0;
                for (s0, e0) in re.find_iter(&s) {
                    if e0 == s0 && s0 == last && s0 != 0 {
                        continue;
                    }
                    out.push(s[last..s0].to_string());
                    last = e0;
                }
                out.push(s[last..].to_string());
                out
            } else {
                s.split_whitespace().map(str::to_string).collect()
            };
            Val::List(
                parts
                    .into_iter()
                    .filter(|p| keep || !p.is_empty())
                    .map(Val::Str)
                    .collect(),
            )
        }
        "trim" => {
            let s = a0().to_str();
            let mask = args.get(1).map(Val::to_str);
            let is = |c: char| match &mask {
                Some(m) if !m.is_empty() => m.contains(c),
                _ => c.is_whitespace() || c == '\u{a0}',
            };
            Val::Str(match args.get(2).map_or(0, Val::to_num) {
                1 => s.trim_start_matches(is).to_string(),
                2 => s.trim_end_matches(is).to_string(),
                _ => s.trim_matches(is).to_string(),
            })
        }
        "substitute" => {
            need(&args, 4, name)?;
            let s = a0().to_str();
            let re = super::search::compile(&a1().to_str(), &opts(cx))?;
            let rep = arg(&args, 2).to_str();
            let global = arg(&args, 3).to_str().contains('g');
            let mut out = String::new();
            let mut last = 0;
            let mut pos = 0;
            while pos <= s.len() {
                let Some(caps) = re.re.captures_at(&s, pos) else {
                    break;
                };
                let whole = caps.get(0).unwrap();
                let m = if re.zs {
                    caps.name("vzs").unwrap_or(whole)
                } else {
                    whole
                };
                out.push_str(&s[last..m.start()]);
                out.push_str(&super::search::expand_replacement(&rep, &caps, &re, true));
                last = m.end();
                pos = if whole.end() > whole.start() {
                    whole.end()
                } else {
                    out.push_str(
                        s[whole.end()..]
                            .chars()
                            .next()
                            .map_or("", |c| &s[whole.end()..whole.end() + c.len_utf8()]),
                    );
                    last = (whole.end()
                        + s[whole.end()..].chars().next().map_or(1, char::len_utf8))
                    .min(s.len());
                    last
                };
                if !global {
                    break;
                }
            }
            if last <= s.len() {
                out.push_str(&s[last..]);
            }
            Val::Str(out)
        }
        "matchstr" | "match" | "matchend" => {
            need(&args, 2, name)?;
            let s = a0().to_str();
            let re = super::search::compile(&a1().to_str(), &opts(cx))?;
            let hit = re.find_iter(&s).next();
            match name {
                "matchstr" => Val::Str(hit.map(|(a, b)| s[a..b].to_string()).unwrap_or_default()),
                "match" => Val::Num(hit.map_or(-1, |(a, _)| a as i64)),
                _ => Val::Num(hit.map_or(-1, |(_, b)| b as i64)),
            }
        }
        "escape" => {
            let chars = a1().to_str();
            let mut out = String::new();
            for c in a0().to_str().chars() {
                if chars.contains(c) {
                    out.push('\\');
                }
                out.push(c);
            }
            Val::Str(out)
        }
        "tr" => {
            need(&args, 3, name)?;
            let from: Vec<char> = a1().to_str().chars().collect();
            let to: Vec<char> = arg(&args, 2).to_str().chars().collect();
            Val::Str(
                a0().to_str()
                    .chars()
                    .map(|c| match from.iter().position(|&f| f == c) {
                        Some(i) => to.get(i).copied().unwrap_or(c),
                        None => c,
                    })
                    .collect(),
            )
        }
        "submatch" => {
            let n = a0().to_num().max(0) as usize;
            Val::Str(cx.submatch.get(n).cloned().unwrap_or_default())
        }
        "getreg" => {
            let r = a0().to_str().chars().next().unwrap_or('"');
            Val::Str(read_register(cx, r))
        }
        "setreg" => {
            need(&args, 2, name)?;
            let r = a0().to_str().chars().next().unwrap_or('"');
            let opt = arg(&args, 2).to_str();
            let (text, kind) = match a1() {
                Val::List(l) => (
                    l.iter().map(Val::to_str).collect::<Vec<_>>().join("\n") + "\n",
                    RegKind::Line,
                ),
                v => {
                    let t = v.to_str();
                    if opt.contains('l') || opt.contains('V') {
                        (format!("{}\n", t.trim_end_matches('\n')), RegKind::Line)
                    } else {
                        (t, RegKind::Char)
                    }
                }
            };
            match cx.host.as_deref_mut() {
                Some(host) => {
                    let mut clip = super::HostClip(host);
                    cx.vim.regs.set(r, Reg::new(text, kind), &mut clip);
                }
                None => cx.vim.regs.set(r, Reg::new(text, kind), &mut NoClip),
            }
            Val::Num(0)
        }
        "exists" => {
            let s = a0().to_str();
            Val::Num(i64::from(if let Some(o) = s.strip_prefix('&') {
                super::ex::option_known(o)
            } else if let Some(f) = s.strip_prefix('*') {
                cx.vim.funcs.contains_key(&super::script::normalize_name(f))
            } else if let Some(c) = s.strip_prefix(':') {
                cx.vim.user_commands.contains_key(c) || super::ex::command_known(c)
            } else if let Some(v) = s.strip_prefix('$') {
                std::env::var(v).is_ok()
            } else {
                var_exists(cx.vim, &s)
            }))
        }
        "has" => {
            let f = a0().to_str();
            Val::Num(i64::from(match f.as_str() {
                "win32" | "win64" | "win95" | "win16" => cfg!(windows),
                "unix" | "linux" => cfg!(unix) && !cfg!(target_os = "macos"),
                "mac" | "macunix" | "osx" | "osxdarwin" => cfg!(target_os = "macos"),
                "nvim" | "gui_running" | "gui" | "python" | "python3" | "lua" | "ruby" | "perl"
                | "conceal" | "gui_win32" | "gui_gtk" | "gui_macvim" => false,
                f if f.starts_with("patch") => true,
                _ => true,
            }))
        }
        "empty" => Val::Num(i64::from(match a0() {
            Val::Num(n) => n == 0,
            Val::Float(f) => f == 0.0,
            Val::Str(s) => s.is_empty(),
            Val::List(l) => l.is_empty(),
            Val::Dict(d) => d.is_empty(),
        })),
        "type" => Val::Num(match a0() {
            Val::Num(_) => 0,
            Val::Str(_) => 1,
            Val::List(_) => 3,
            Val::Dict(_) => 4,
            Val::Float(_) => 5,
        }),
        "nr2char" => Val::Str(
            char::from_u32(a0().to_num() as u32)
                .map(|c| c.to_string())
                .unwrap_or_default(),
        ),
        "char2nr" => Val::Num(a0().to_str().chars().next().map_or(0, |c| c as i64)),
        "reverse" => match a0() {
            Val::List(mut l) => {
                l.reverse();
                Val::List(l)
            }
            v => Val::Str(v.to_str().chars().rev().collect()),
        },
        "sort" => {
            let mut l = list_of(&a0());
            let how = a1().to_str();
            if how == "n" || how == "N" {
                l.sort_by_key(Val::to_num);
            } else if how == "f" {
                l.sort_by(|a, b| {
                    a.to_f()
                        .partial_cmp(&b.to_f())
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            } else if how == "i" || how == "1" {
                l.sort_by_key(|v| v.to_str().to_lowercase());
            } else {
                l.sort_by_key(Val::to_str);
            }
            Val::List(l)
        }
        "uniq" => {
            let mut l = list_of(&a0());
            l.dedup();
            Val::List(l)
        }
        "count" => {
            let needle = a1();
            Val::Num(match a0() {
                Val::Str(s) => {
                    let n = needle.to_str();
                    if n.is_empty() {
                        0
                    } else {
                        s.matches(&n).count() as i64
                    }
                }
                v => list_of(&v).iter().filter(|x| **x == needle).count() as i64,
            })
        }
        "index" => {
            let needle = a1();
            Val::Num(
                list_of(&a0())
                    .iter()
                    .position(|x| *x == needle)
                    .map_or(-1, |i| i as i64),
            )
        }
        "get" => match a0() {
            Val::List(l) => {
                let n = a1().to_num();
                let i = if n < 0 { l.len() as i64 + n } else { n };
                l.get(i as usize)
                    .cloned()
                    .filter(|_| i >= 0)
                    .unwrap_or_else(|| args.get(2).cloned().unwrap_or(Val::Num(0)))
            }
            Val::Dict(d) => d
                .get(&a1().to_str())
                .cloned()
                .unwrap_or_else(|| args.get(2).cloned().unwrap_or(Val::Num(0))),
            _ => args.get(2).cloned().unwrap_or(Val::Num(0)),
        },
        "keys" => Val::List(match a0() {
            Val::Dict(d) => d.keys().cloned().map(Val::Str).collect(),
            _ => Vec::new(),
        }),
        "values" => Val::List(list_of(&a0())),
        "has_key" => Val::Num(i64::from(match a0() {
            Val::Dict(d) => d.contains_key(&a1().to_str()),
            _ => false,
        })),
        "add" => match a0() {
            Val::List(mut l) => {
                l.push(a1());
                Val::List(l)
            }
            _ => return Err("E897: List or Blob required".into()),
        },
        "extend" => match (a0(), a1()) {
            (Val::List(mut a), Val::List(b)) => {
                a.extend(b);
                Val::List(a)
            }
            (Val::Dict(mut a), Val::Dict(b)) => {
                a.extend(b);
                Val::Dict(a)
            }
            _ => return Err("E712: Argument of extend() must be a List or Dictionary".into()),
        },
        "printf" => Val::Str(printf(&a0().to_str(), &args[1.min(args.len())..])?),
        "expand" => Val::Str(expand(cx, &a0().to_str())),
        "fnamemodify" => Val::Str(super::paths::modify(&a0().to_str(), &a1().to_str())),
        "getcwd" => Val::Str(
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        ),
        "bufname" | "expand_file" => Val::Str(expand(cx, "%")),
        "localtime" => Val::Num(chrono::Local::now().timestamp()),
        "strftime" => {
            let fmt = a0().to_str();
            let when = if args.len() > 1 {
                chrono::DateTime::from_timestamp(a1().to_num(), 0)
                    .map(|d| d.with_timezone(&chrono::Local))
                    .unwrap_or_else(chrono::Local::now)
            } else {
                chrono::Local::now()
            };
            let mut out = String::new();
            use std::fmt::Write as _;
            if write!(out, "{}", when.format(&fmt)).is_err() {
                return Err("E15: Invalid strftime() format".into());
            }
            Val::Str(out)
        }
        "reltime" | "reltimestr" | "reltimefloat" => Val::Num(0),
        "mode" => Val::str(match cx.vim.mode {
            super::Mode::Normal => "n",
            super::Mode::Insert => "i",
            super::Mode::Replace => "R",
            super::Mode::Visual(super::VisualKind::Char) => "v",
            super::Mode::Visual(super::VisualKind::Line) => "V",
            super::Mode::Visual(super::VisualKind::Block) => "\u{16}",
            super::Mode::Cmdline => "c",
        }),
        "system" | "systemlist" => {
            let Some(host) = cx.host.as_deref() else {
                return Err(format!("E523: Not allowed here: {name}()"));
            };
            let input = args.get(1).map(Val::to_text);
            let out = match super::ex::shell(host, &a0().to_str(), input.as_deref()) {
                Ok(o) => {
                    cx.vim.shell_error = 0;
                    o
                }
                Err(e) => {
                    cx.vim.shell_error = 1;
                    e
                }
            };
            if name == "systemlist" {
                Val::List(out.lines().map(|l| Val::str(l)).collect())
            } else {
                Val::Str(out)
            }
        }
        "executable" => Val::Num(i64::from(which(&a0().to_str()))),
        "filereadable" => Val::Num(i64::from(
            std::path::Path::new(&expand(cx, &a0().to_str())).is_file(),
        )),
        "isdirectory" => Val::Num(i64::from(
            std::path::Path::new(&expand(cx, &a0().to_str())).is_dir(),
        )),
        "and" => Val::Num(a0().to_num() & a1().to_num()),
        "or" => Val::Num(a0().to_num() | a1().to_num()),
        "xor" => Val::Num(a0().to_num() ^ a1().to_num()),
        "invert" => Val::Num(!a0().to_num()),
        "eval" => eval(&a0().to_str(), cx)?,
        "execute" => {
            let cmds = text_lines(&a0());
            let Some(host) = cx.host.as_deref_mut() else {
                return Err(format!("E523: Not allowed here: {name}()"));
            };
            let saved = cx.vim.message.take();
            for c in cmds {
                super::ex::run(cx.vim, host, &c)?;
            }
            let msg = std::mem::replace(&mut cx.vim.message, saved);
            Val::Str(msg.map_or(String::new(), |m| {
                let mut out = String::from("\n");
                if m.lines.is_empty() {
                    out.push_str(&m.text);
                } else {
                    out.push_str(&m.lines.join("\n"));
                }
                out
            }))
        }
        "feedkeys" => {
            let keys = super::ops::text_to_keys(&a0().to_str());
            let remap = !a1().to_str().contains('n');
            cx.vim.queue_keys(&keys, remap, false);
            Val::Num(0)
        }
        "search" => {
            need(&args, 1, name)?;
            let flags = a1().to_str();
            let re = super::search::compile(&a0().to_str(), &opts(cx))?;
            let wrapscan = cx.vim.opts.wrapscan;
            let Some(host) = cx.host.as_deref_mut() else {
                return Err(format!("E523: Not allowed here: {name}()"));
            };
            let Some(ed) = host.ed() else {
                return Ok(Val::Num(0));
            };
            let hay = super::exec::haystack(ed);
            let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
            let from = t.idx(Pos::new(ed.cursor_line, ed.cursor_col));
            let wrap = !flags.contains('W') && (flags.contains('w') || wrapscan);
            match super::search::search(
                &ed.rope,
                &hay,
                &re,
                from,
                !flags.contains('b'),
                wrap,
                flags.contains('c'),
            ) {
                Some((s, e, _)) => {
                    let at = if flags.contains('e') {
                        e.saturating_sub(1)
                    } else {
                        s
                    };
                    let line = ed.rope.char_to_line(at);
                    if !flags.contains('n') {
                        let p = t.pos(at);
                        ed.cursor_line = p.line;
                        ed.cursor_col = p.col;
                    }
                    Val::Num(line as i64 + 1)
                }
                None => Val::Num(0),
            }
        }
        "line2byte" => {
            let l = lnum_arg(cx, &a0()).unwrap_or(0);
            let Some(ed) = cx.editor() else {
                return Ok(Val::Num(-1));
            };
            if l == 0 || l > ed.rope.len_lines() {
                Val::Num(-1)
            } else {
                Val::Num(ed.rope.char_to_byte(ed.rope.line_to_char(l - 1)) as i64 + 1)
            }
        }
        "wordcount" => {
            let Some(ed) = cx.editor() else {
                return Ok(Val::Dict(BTreeMap::new()));
            };
            let text = ed.rope.to_string();
            let mut d = BTreeMap::new();
            d.insert(
                "words".into(),
                Val::Num(text.split_whitespace().count() as i64),
            );
            d.insert("chars".into(), Val::Num(text.chars().count() as i64));
            d.insert("bytes".into(), Val::Num(text.len() as i64));
            Val::Dict(d)
        }
        "map" | "filter" => {
            // String expressions only: v:val and v:key.
            let src = a1().to_str();
            let items = list_of(&a0());
            let mut out = Vec::new();
            for (i, item) in items.into_iter().enumerate() {
                let saved_val = cx.vim.vars.insert("v:val".into(), item.clone());
                let saved_key = cx.vim.vars.insert("v:key".into(), Val::Num(i as i64));
                let r = eval(&src, cx);
                restore(cx.vim, "v:val", saved_val);
                restore(cx.vim, "v:key", saved_key);
                let r = r?;
                if name == "map" {
                    out.push(r);
                } else if r.truthy() {
                    out.push(item);
                }
            }
            Val::List(out)
        }
        "pumvisible" => Val::Num(i64::from(
            cx.host.as_ref().is_some_and(|h| h.completion_visible()),
        )),
        "getcmdtype" => Val::Str(
            cx.vim
                .cmdline
                .as_ref()
                .map(|c| c.kind.to_string())
                .unwrap_or_default(),
        ),
        "getcmdline" => Val::Str(
            cx.vim
                .cmdline
                .as_ref()
                .map(|c| c.string())
                .unwrap_or_default(),
        ),
        "getcmdpos" => Val::Num(cx.vim.cmdline.as_ref().map_or(0, |c| c.cursor as i64 + 1)),
        "visualmode" => Val::str(
            match cx
                .editor()
                .and_then(|e| e.vim_buf.last_visual)
                .map(|l| l.kind)
            {
                Some(super::VisualKind::Char) => "v",
                Some(super::VisualKind::Line) => "V",
                Some(super::VisualKind::Block) => "\u{16}",
                None => "",
            },
        ),
        "getpos" | "getcurpos" | "getcharpos" => {
            let spec = if name == "getcurpos" {
                ".".into()
            } else {
                a0().to_str()
            };
            let Some(ed) = cx.editor() else {
                return Ok(Val::List(vec![Val::Num(0); 4]));
            };
            let p = match spec.as_str() {
                "." => Some(Pos::new(ed.cursor_line, ed.cursor_col)),
                "v" if cx.vim.is_visual() => Some(cx.vim.visual_anchor),
                s if s.starts_with('\'') => s
                    .chars()
                    .nth(1)
                    .and_then(|m| ed.vim_buf.marks.get(&m).copied()),
                _ => None,
            };
            Val::List(match p {
                Some(p) => vec![
                    Val::Num(0),
                    Val::Num(p.line as i64 + 1),
                    Val::Num(p.col as i64 + 1),
                    Val::Num(0),
                ],
                None => vec![Val::Num(0); 4],
            })
        }
        "setpos" => {
            need(&args, 2, name)?;
            let spec = a0().to_str();
            let items = list_of(&a1());
            let line = items.get(1).map_or(1, Val::to_num).max(1) as usize - 1;
            let col = items.get(2).map_or(1, Val::to_num).max(1) as usize - 1;
            if let Some(ed) = cx.host.as_deref_mut().and_then(|h| h.ed()) {
                let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
                let p = t.clamp_normal(Pos::new(line.min(t.last_line()), col));
                match spec.as_str() {
                    "." => {
                        ed.cursor_line = p.line;
                        ed.cursor_col = p.col;
                    }
                    s if s.starts_with('\'') => {
                        if let Some(m) = s.chars().nth(1) {
                            ed.vim_buf.marks.insert(m, p);
                        }
                    }
                    _ => return Ok(Val::Num(-1)),
                }
            }
            Val::Num(0)
        }
        "winheight" => Val::Num(cx.editor().map_or(0, |e| e.viewport_height as i64)),
        "winwidth" => Val::Num(cx.editor().map_or(0, |e| e.viewport_width as i64)),
        "winline" => Val::Num(cx.editor().map_or(0, |e| {
            e.cursor_line.saturating_sub(e.scroll_offset) as i64 + 1
        })),
        "bufnr" => Val::Num(1),
        "winnr" | "tabpagenr" => Val::Num(1),
        "exists_compiled" => Val::Num(0),
        "function" | "funcref" => Val::Str(a0().to_str()),
        "call" => {
            let f = a0().to_str();
            let fargs = list_of(&a1());
            call(cx, &f, fargs)?
        }
        "abs_path" => Val::Num(0),
        _ => {
            if let Some(host) = cx.host.as_deref_mut()
                && let Some(r) = super::script::call(cx.vim, host, name, args, None)
            {
                return r;
            }
            return Err(format!("E117: Unknown function: {name}"));
        }
    })
}

fn restore(vim: &mut VimState, key: &str, old: Option<Val>) {
    match old {
        Some(v) => {
            vim.vars.insert(key.into(), v);
        }
        None => {
            vim.vars.remove(key);
        }
    }
}

pub(crate) fn which(cmd: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or(".EXE;.CMD;.BAT;.COM".into())
            .split(';')
            .map(|s| s.to_lowercase())
            .chain(std::iter::once(String::new()))
            .collect()
    } else {
        vec![String::new()]
    };
    std::env::split_paths(&path)
        .any(|dir| exts.iter().any(|e| dir.join(format!("{cmd}{e}")).is_file()))
}

/// Replace lines from `lnum` (`setline`) or insert after it (`append`).
/// Returns false when the line number is out of range.
fn edit_lines(ed: &mut Editor, lnum: usize, lines: &[String], append: bool) -> bool {
    let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
    let count = t.lines();
    let eol = super::exec::doc_eol(ed).to_string();
    let mut tx = Tx::default();
    if append {
        if lnum > count {
            return false;
        }
        let body: String = lines.iter().map(|l| format!("{l}{eol}")).collect();
        let at = if lnum == 0 {
            0
        } else if lnum >= count && !super::ends_with_eol(&ed.rope) {
            let end = ed.rope.len_chars();
            let body: String = lines.iter().map(|l| format!("{eol}{l}")).collect();
            tx.edit(ed, end, end, &body);
            tx.finish(ed);
            ed.vim_buf.shift_lines(lnum, lines.len() as isize);
            return true;
        } else {
            t.line_next_start(lnum - 1)
        };
        tx.edit(ed, at, at, &body);
        tx.finish(ed);
        ed.vim_buf.shift_lines(lnum, lines.len() as isize);
        return true;
    }
    if lnum == 0 || lnum > count + 1 {
        return false;
    }
    for (i, text) in lines.iter().enumerate() {
        let l = lnum - 1 + i;
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        if l < t.lines() {
            let (s, e) = (t.line_start(l), t.line_end(l));
            tx.edit(ed, s, e, text);
        } else {
            let end = ed.rope.len_chars();
            let pre = if super::ends_with_eol(&ed.rope) {
                String::new()
            } else {
                eol.clone()
            };
            tx.edit(ed, end, end, &format!("{pre}{text}"));
        }
    }
    tx.finish(ed);
    true
}

/// `expand()`: `%` `#` with modifiers, `<cword>`, `<cWORD>`, `<cfile>`,
/// `~` and `$VAR`.
pub(crate) fn expand(cx: &Ctx<'_, '_>, s: &str) -> String {
    let file = || -> String {
        cx.editor()
            .and_then(|ed| ed.file_path.as_ref())
            .map(|p| p.display().to_string())
            .unwrap_or_default()
    };
    let word = |big: bool| -> String {
        let Some(ed) = cx.editor() else {
            return String::new();
        };
        let t = Text::of(&ed.rope, ed.vim_buf.trailing_empty);
        let chars = t.line_chars(ed.cursor_line);
        let is = |c: char| {
            if big {
                !c.is_whitespace()
            } else {
                cx.vim.opts.iskeyword.is_word(c)
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
    let (head, mods) = match s.find(':') {
        Some(i) if s.starts_with('%') || s.starts_with('#') || s.starts_with('<') => {
            (&s[..i], &s[i..])
        }
        _ => (s, ""),
    };
    let base = match head {
        "%" => file(),
        "#" => String::new(),
        "<cword>" => word(false),
        "<cWORD>" => word(true),
        "<cfile>" => {
            let w = word(true);
            w.trim_matches(|c: char| "\"'()[]{}<>,;".contains(c))
                .to_string()
        }
        "<sfile>" | "<script>" => String::new(),
        "<afile>" | "<abuf>" | "<amatch>" => String::new(),
        _ => {
            let mut out = s.to_string();
            if let Some(rest) = out.strip_prefix('~')
                && let Some(home) = dirs_home()
            {
                out = format!("{home}{rest}");
            }
            return expand_env(&out);
        }
    };
    if mods.is_empty() {
        base
    } else {
        super::paths::modify(&base, mods)
    }
}

fn dirs_home() -> Option<String> {
    std::env::var("HOME")
        .ok()
        .or_else(|| std::env::var("USERPROFILE").ok())
}

fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' {
            let st = i + 1;
            let mut e = st;
            while e < chars.len() && (chars[e].is_alphanumeric() || chars[e] == '_') {
                e += 1;
            }
            if e > st {
                let name: String = chars[st..e].iter().collect();
                if let Ok(v) = std::env::var(&name) {
                    out.push_str(&v);
                    i = e;
                    continue;
                }
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// A useful subset of `printf()`: `%d %i %s %S %x %X %o %c %f %e %g %%`
/// with flags, width and precision (`*` too).
pub(crate) fn printf(fmt: &str, args: &[Val]) -> R<String> {
    let mut out = String::new();
    let mut it = fmt.chars().peekable();
    let mut ai = 0;
    let next = |ai: &mut usize| -> R<Val> {
        let v = args
            .get(*ai)
            .cloned()
            .ok_or_else(|| "E766: Insufficient arguments for printf()".to_string())?;
        *ai += 1;
        Ok(v)
    };
    while let Some(c) = it.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let mut flags = String::new();
        while let Some(&f) = it.peek() {
            if "-+ 0#".contains(f) {
                flags.push(f);
                it.next();
            } else {
                break;
            }
        }
        let mut width = String::new();
        if it.peek() == Some(&'*') {
            it.next();
            width = next(&mut ai)?.to_num().to_string();
        }
        while let Some(&d) = it.peek() {
            if d.is_ascii_digit() {
                width.push(d);
                it.next();
            } else {
                break;
            }
        }
        let mut prec: Option<usize> = None;
        if it.peek() == Some(&'.') {
            it.next();
            let mut p = String::new();
            if it.peek() == Some(&'*') {
                it.next();
                p = next(&mut ai)?.to_num().to_string();
            }
            while let Some(&d) = it.peek() {
                if d.is_ascii_digit() {
                    p.push(d);
                    it.next();
                } else {
                    break;
                }
            }
            prec = Some(p.parse().unwrap_or(0));
        }
        // Length modifiers are ignored.
        while matches!(it.peek(), Some('l' | 'h')) {
            it.next();
        }
        let Some(conv) = it.next() else {
            out.push('%');
            break;
        };
        let body = match conv {
            '%' => {
                out.push('%');
                continue;
            }
            'd' | 'i' => {
                let n = next(&mut ai)?.to_num();
                let mut s = n.abs().to_string();
                if let Some(p) = prec {
                    while s.len() < p {
                        s.insert(0, '0');
                    }
                }
                if n < 0 {
                    s.insert(0, '-');
                } else if flags.contains('+') {
                    s.insert(0, '+');
                } else if flags.contains(' ') {
                    s.insert(0, ' ');
                }
                s
            }
            'x' | 'X' | 'o' | 'b' | 'B' => {
                let n = next(&mut ai)?.to_num();
                let s = match conv {
                    'x' => format!("{n:x}"),
                    'X' => format!("{n:X}"),
                    'o' => format!("{n:o}"),
                    _ => format!("{n:b}"),
                };
                if flags.contains('#') && n != 0 {
                    let p = match conv {
                        'x' => "0x",
                        'X' => "0X",
                        'o' => "0",
                        'b' => "0b",
                        _ => "0B",
                    };
                    format!("{p}{s}")
                } else {
                    s
                }
            }
            'c' => char::from_u32(next(&mut ai)?.to_num() as u32)
                .map(|c| c.to_string())
                .unwrap_or_default(),
            's' | 'S' => {
                let v = next(&mut ai)?;
                let s = match v {
                    Val::Str(s) => s,
                    v => v.repr(),
                };
                match prec {
                    Some(p) => s.chars().take(p).collect(),
                    None => s,
                }
            }
            'f' | 'F' => {
                let f = next(&mut ai)?.to_f();
                let s = format!("{:.*}", prec.unwrap_or(6), f);
                if flags.contains('+') && f >= 0.0 {
                    format!("+{s}")
                } else {
                    s
                }
            }
            'e' | 'E' => {
                let f = next(&mut ai)?.to_f();
                let s = format!("{:.*e}", prec.unwrap_or(6), f);
                let (m, e) = s.split_once('e').unwrap_or((&s, "0"));
                let exp: i32 = e.parse().unwrap_or(0);
                let s = format!("{m}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
                if conv == 'E' { s.to_uppercase() } else { s }
            }
            'g' | 'G' => fmt_float(next(&mut ai)?.to_f()),
            other => return Err(format!("E767: Unsupported printf() conversion: %{other}")),
        };
        let w: usize = width.parse().unwrap_or(0);
        let len = body.chars().count();
        if len >= w {
            out.push_str(&body);
        } else if flags.contains('-') {
            out.push_str(&body);
            out.push_str(&" ".repeat(w - len));
        } else if flags.contains('0') && !matches!(conv, 's' | 'S' | 'c') {
            let (sign, digits) = match body.chars().next() {
                Some(c @ ('-' | '+' | ' ')) => (c.to_string(), body[1..].to_string()),
                _ => (String::new(), body.clone()),
            };
            out.push_str(&sign);
            out.push_str(&"0".repeat(w - len));
            out.push_str(&digits);
        } else {
            out.push_str(&" ".repeat(w - len));
            out.push_str(&body);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vim::tests::TestHost;

    fn ev(src: &str) -> String {
        let mut host = TestHost::new("hello world\nsecond line\n");
        let mut vim = crate::vim::tests::parity_vim();
        let mut cx = Ctx::new(&mut vim, &mut host);
        match eval(src, &mut cx) {
            Ok(v) => v.to_str(),
            Err(e) => format!("ERR {e}"),
        }
    }

    #[test]
    fn arithmetic_strings_and_precedence() {
        assert_eq!(ev("2*21"), "42");
        assert_eq!(ev("1 + 2 * 3"), "7");
        assert_eq!(ev("(1 + 2) * 3"), "9");
        assert_eq!(ev("7 / 2"), "3");
        assert_eq!(ev("7 % 3"), "1");
        assert_eq!(ev("-7 / 2"), "-3");
        assert_eq!(ev("7.0 / 2"), "3.5");
        assert_eq!(ev("0.1 + 0.2"), "0.3");
        assert_eq!(ev("3.0"), "3.0");
        assert_eq!(ev("0x1f + 0b11"), "34");
        assert_eq!(ev("'a' . 'b' .. 1"), "ab1");
        assert_eq!(ev("\"tab\\there\""), "tab\there");
        assert_eq!(ev("'it''s'"), "it's");
        assert_eq!(ev("'10' + 5"), "15");
        assert_eq!(ev("1 ? 'y' : 'n'"), "y");
        assert_eq!(ev("0 ? 'y' : 'n'"), "n");
        assert_eq!(ev("'abc' == 'abc' && 2 > 1"), "1");
        assert_eq!(ev("'abc' ==? 'ABC'"), "1");
        assert_eq!(ev("'foo123' =~ '\\d\\+'"), "1");
        assert_eq!(ev("!0"), "1");
        assert_eq!(ev("[1, 2, 3][1]"), "2");
        assert_eq!(ev("[1, 2, 3][-1]"), "3");
        assert_eq!(ev("'hello'[1:3]"), "ell");
        assert_eq!(ev("{'a': 1}.a"), "1");
        assert_eq!(ev("0 && undefined_var"), "0");
    }

    #[test]
    fn buffer_and_string_functions() {
        assert_eq!(ev("line('.')"), "1");
        assert_eq!(ev("line('$')"), "2");
        assert_eq!(ev("getline(2)"), "second line");
        assert_eq!(ev("col('$')"), "12");
        assert_eq!(ev("toupper('ab') . tolower('CD')"), "ABcd");
        assert_eq!(
            ev("printf('%05.1f|%-3s|%3d|%x', 3.14159, 'a', 7, 255)"),
            "003.1|a  |  7|ff"
        );
        assert_eq!(ev("repeat('-', 3)"), "---");
        assert_eq!(ev("join(split('a b  c'), ',')"), "a,b,c");
        assert_eq!(ev("substitute('foo bar', 'o', '0', 'g')"), "f00 bar");
        assert_eq!(ev("matchstr('abc123', '\\d\\+')"), "123");
        assert_eq!(ev("len([1,2]) + strlen('abc')"), "5");
        assert_eq!(ev("range(3)"), "[0, 1, 2]");
        assert_eq!(ev("map([1, 2], 'v:val * 2')"), "[2, 4]");
        assert_eq!(ev("[3, 1, 2]->sort()->join('')"), "123");
        assert_eq!(ev("expand('<cword>')"), "hello");
        assert_eq!(ev("str2nr('0x10', 16)"), "16");
        assert_eq!(ev("trim('  x  ')"), "x");
        assert!(ev("strftime('%Y')").len() == 4);
        assert!(ev("nosuchfn()").starts_with("ERR E117"));
    }
}
