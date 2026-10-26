//! Small, query-local programs over visible text. No source cache, language
//! vocabulary, filesystem, timer, or learned weights are required.

#[path = "completion_transformations.rs"]
mod relations;

const ROWS: usize = 16;
const TOKENS: usize = 96;
const WORD_BYTES: usize = 160;
const LINE_BYTES: usize = 512;
const CANDIDATES: usize = 8;
const WORK: usize = 32_768;
const _: () = assert!(TOKENS < u8::MAX as usize);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Sequence,
    Transform,
    Stem,
    Relation,
}

struct Prediction {
    word: String,
    count: f64,
    kind: Kind,
}

#[derive(Default)]
pub(crate) struct Predictions {
    rows: Vec<Prediction>,
    conflicts: f64,
    pub(crate) work: usize,
    pub(crate) limited: bool,
}
impl Predictions {
    pub(crate) fn candidates(&self) -> impl Iterator<Item = &str> {
        self.rows.iter().map(|row| row.word.as_str())
    }
    /// Two pairs can rank a spelling already observed in the buffer/index.
    /// A new spelling from the richer program language needs a third distinct
    /// pair: an independent witness beyond the pair used to infer the program.
    pub(crate) fn novel_candidates(&self) -> impl Iterator<Item = &str> {
        self.rows
            .iter()
            .filter(|row| row.kind != Kind::Relation || row.count >= 3.)
            .map(|row| row.word.as_str())
    }
    pub(crate) fn ground_relations(&mut self, observed: impl Fn(&str) -> bool) {
        self.rows
            .retain(|row| row.kind != Kind::Relation || row.count >= 3. || observed(&row.word));
    }
    pub(crate) fn kind(&self, word: &str) -> Option<Kind> {
        self.rows
            .iter()
            .find(|row| row.word == word)
            .map(|row| row.kind)
    }
    pub(crate) fn active(&self) -> bool {
        !self.rows.is_empty()
    }
    /// A separate local evidence channel, with one unit of backoff mass. No
    /// generated evidence is the exact identity, including work exhaustion.
    pub(crate) fn probability(&self, word: &str, prior: f64) -> f64 {
        if !self.active() {
            return prior;
        }
        let count = self
            .rows
            .iter()
            .find(|row| row.word == word)
            .map_or(0., |row| row.count);
        (count + prior) / (self.rows.iter().map(|row| row.count).sum::<f64>() + self.conflicts + 1.)
    }
    fn add(&mut self, word: String, prefix: &str, count: f64, kind: Kind) {
        if word.len() > WORD_BYTES || word.len() <= prefix.len() || !word.starts_with(prefix) {
            return;
        }
        if let Some(row) = self.rows.iter_mut().find(|row| row.word == word) {
            row.count = row.count.max(count);
            if kind != Kind::Stem && (row.kind == Kind::Stem || kind != Kind::Relation) {
                row.kind = kind;
            }
        } else if self.rows.len() < CANDIDATES {
            self.rows.push(Prediction { word, count, kind });
        } else {
            self.limited = true;
        }
    }
}

struct Budget<'a> {
    used: usize,
    limited: bool,
    cancel: &'a dyn Fn() -> bool,
}
impl Budget<'_> {
    fn spend(&mut self, amount: usize) -> bool {
        if amount > WORK.saturating_sub(self.used) || (self.cancel)() {
            self.limited = true;
            return false;
        }
        self.used += amount;
        true
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TokenKind {
    Word,
    Literal,
    Punctuation,
}
#[derive(Clone, Copy)]
struct Token<'a> {
    text: &'a str,
    kind: TokenKind,
    // First occurrence in this row. Equality of these partitions proves a
    // reversible identifier mapping without comparing every token pair.
    binding: u8,
}
struct Row<'a> {
    indent: &'a str,
    tokens: Vec<Token<'a>>,
}

fn word_char(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '_' | '$')
}
fn lex<'a>(text: &'a str, budget: &mut Budget<'_>) -> Option<Row<'a>> {
    if text.len() > LINE_BYTES || !budget.spend(text.len() + 1) {
        return None;
    }
    let indent_end = text
        .char_indices()
        .find(|(_, ch)| !ch.is_whitespace())
        .map_or(text.len(), |(at, _)| at);
    let mut chars = text.char_indices().peekable();
    let mut tokens: Vec<Token<'a>> = Vec::new();
    let mut bindings = [u8::MAX; 256];
    while let Some((start, ch)) = chars.next() {
        if ch.is_whitespace() {
            continue;
        }
        if tokens.len() == TOKENS {
            return None;
        }
        let kind;
        let mut end = start + ch.len_utf8();
        if word_char(ch) {
            kind = TokenKind::Word;
            while let Some(&(at, next)) = chars.peek() {
                if !word_char(next) {
                    break;
                }
                chars.next();
                end = at + next.len_utf8();
            }
            if end - start > WORD_BYTES {
                return None;
            }
        } else if matches!(ch, '\'' | '"' | '`') {
            kind = TokenKind::Literal;
            let mut escaped = false;
            let mut closed = false;
            for (at, next) in chars.by_ref() {
                end = at + next.len_utf8();
                if !escaped && next == ch {
                    closed = true;
                    break;
                }
                escaped = !escaped && next == '\\';
            }
            if !closed {
                return None;
            }
        } else {
            kind = TokenKind::Punctuation;
        }
        let value = &text[start..end];
        let mut binding = u8::MAX;
        if kind != TokenKind::Punctuation {
            if !budget.spend(value.len()) {
                return None;
            }
            let hash = value.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
            });
            let mut slot = (hash as usize) & (bindings.len() - 1);
            loop {
                if !budget.spend(1) {
                    return None;
                }
                let previous = bindings[slot];
                if previous == u8::MAX {
                    binding = tokens.len() as u8;
                    bindings[slot] = binding;
                    break;
                }
                if tokens[usize::from(previous)].text == value {
                    binding = previous;
                    break;
                }
                slot = (slot + 1) & (bindings.len() - 1);
            }
        }
        tokens.push(Token {
            text: value,
            kind,
            binding,
        });
    }
    Some(Row {
        indent: &text[..indent_end],
        tokens,
    })
}

fn scope_boundary(row: &Row<'_>) -> bool {
    let mut stack = Vec::new();
    for token in &row.tokens {
        if token.kind != TokenKind::Punctuation {
            continue;
        }
        match token.text {
            "{" => stack.push("}"),
            "(" => stack.push(")"),
            "[" => stack.push("]"),
            "}" | ")" | "]" => {
                if stack.pop() != Some(token.text) {
                    return true;
                }
            }
            _ => {}
        }
    }
    !stack.is_empty()
}

#[derive(Clone)]
struct Numbers<'a> {
    parts: Vec<&'a str>,
    values: Vec<u64>,
    widths: Vec<usize>,
}
fn numbers(word: &str) -> Option<Numbers<'_>> {
    let mut parts = Vec::new();
    let mut values = Vec::new();
    let mut widths = Vec::new();
    let mut start = 0;
    let mut at = 0;
    let bytes = word.as_bytes();
    while at < bytes.len() {
        if !bytes[at].is_ascii_digit() {
            at += 1;
            continue;
        }
        if values.len() == 4 {
            return None;
        }
        parts.push(&word[start..at]);
        start = at;
        while at < bytes.len() && bytes[at].is_ascii_digit() {
            at += 1;
        }
        values.push(word[start..at].parse().ok()?);
        widths.push(if bytes[start] == b'0' && at - start > 1 {
            at - start
        } else {
            0
        });
        start = at;
    }
    if values.is_empty() {
        return None;
    }
    parts.push(&word[start..]);
    Some(Numbers {
        parts,
        values,
        widths,
    })
}
fn render(parts: &[&str], values: &[u64], widths: &[usize]) -> Option<String> {
    let mut word = String::new();
    for (index, &value) in values.iter().enumerate() {
        word.push_str(parts[index]);
        use std::fmt::Write;
        write!(word, "{value:0width$}", width = widths[index]).ok()?;
        if word.len() > WORD_BYTES {
            return None;
        }
    }
    word.push_str(parts[values.len()]);
    (word.len() <= WORD_BYTES).then_some(word)
}

struct Mapping<'a> {
    pairs: Vec<(&'a str, &'a str)>,
}
impl<'a> Mapping<'a> {
    fn new() -> Self {
        Self { pairs: Vec::new() }
    }
    fn bind(&mut self, from: &'a str, to: &'a str) -> bool {
        if let Some((_, mapped)) = self.pairs.iter().find(|(word, _)| *word == from) {
            return *mapped == to;
        }
        if self.pairs.iter().any(|(_, mapped)| *mapped == to) {
            return false;
        }
        self.pairs.push((from, to));
        true
    }
    fn get(&self, from: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(word, _)| *word == from)
            .map(|(_, to)| *to)
    }
}
fn compatible<'a>(a: &'a Row<'a>, b: &'a Row<'a>, budget: &mut Budget<'_>) -> Option<Mapping<'a>> {
    if a.tokens.len() != b.tokens.len() || !budget.spend(a.tokens.len() + 1) {
        return None;
    }
    let mut map = Mapping::new();
    for (index, (a, b)) in a.tokens.iter().zip(&b.tokens).enumerate() {
        if a.kind != b.kind {
            return None;
        }
        if a.kind == TokenKind::Punctuation {
            if a.text != b.text {
                return None;
            }
        } else {
            if a.binding != b.binding {
                return None;
            }
            if usize::from(a.binding) == index {
                map.pairs.push((a.text, b.text));
            }
        }
    }
    Some(map)
}
fn visible<'a>(
    left: &'a Row<'a>,
    right: &'a Row<'a>,
    at: usize,
) -> impl Iterator<Item = (usize, Token<'a>)> {
    left.tokens.iter().copied().enumerate().chain(
        right
            .tokens
            .iter()
            .copied()
            .enumerate()
            .map(move |(index, token)| (at + 1 + index, token)),
    )
}

fn verify_sequence(
    rows: &[&Row<'_>],
    left: &Row<'_>,
    right: &Row<'_>,
    at: usize,
    candidate: &str,
    budget: &mut Budget<'_>,
) -> bool {
    let last = rows[0];
    let mut map = Mapping::new();
    if !map.bind(last.tokens[at].text, candidate) {
        return false;
    }
    for (index, current) in visible(left, right, at) {
        if !budget.spend(rows.len() + TOKENS) {
            return false;
        }
        let Some(previous) = last.tokens.get(index) else {
            return false;
        };
        if previous.kind != current.kind {
            return false;
        }
        if previous.kind == TokenKind::Punctuation {
            if previous.text != current.text {
                return false;
            }
            continue;
        }
        if let Some(shape) = numbers(previous.text) {
            let observations: Option<Vec<_>> = rows
                .iter()
                .map(|row| numbers(row.tokens[index].text).filter(|name| name.parts == shape.parts))
                .collect();
            if let Some(observations) = observations {
                let Some(current) = numbers(current.text).filter(|name| name.parts == shape.parts)
                else {
                    return false;
                };
                for field in 0..shape.values.len() {
                    if observations
                        .iter()
                        .all(|name| name.values[field] == shape.values[field])
                        && current.values[field] != shape.values[field]
                    {
                        return false;
                    }
                }
            }
        }
        if let Some(expected) = map.get(previous.text) {
            if expected != current.text {
                return false;
            }
        } else {
            let constant = rows.iter().all(|row| {
                row.tokens
                    .get(index)
                    .is_some_and(|token| token.text == previous.text)
            });
            if constant && previous.text != current.text {
                return false;
            }
            if !map.bind(previous.text, current.text) {
                return false;
            }
        }
    }
    true
}

/// Use visible numeric fields as exact affine constraints on the missing
/// number. Every witness must fit, and the solution must be an in-range integer.
fn constrained_numbers(
    rows: &[&Row<'_>],
    values: &[Vec<u64>],
    left: &Row<'_>,
    right: &Row<'_>,
    at: usize,
    next: &mut [u64],
    budget: &mut Budget<'_>,
) -> bool {
    let mut bound = [None; 4];
    for (column, current) in visible(left, right, at) {
        let Some(source) = rows[0].tokens.get(column) else {
            return false;
        };
        let Some(shape) = numbers(source.text) else {
            continue;
        };
        let observations: Option<Vec<_>> = rows
            .iter()
            .map(|row| {
                row.tokens
                    .get(column)
                    .and_then(|token| numbers(token.text))
                    .filter(|name| name.parts == shape.parts)
            })
            .collect();
        let Some(observations) = observations else {
            continue;
        };
        let Some(current) = numbers(current.text).filter(|name| name.parts == shape.parts) else {
            return false;
        };
        for target in 0..next.len() {
            let t0 = i128::from(values[0][target]);
            let t1 = i128::from(values[1][target]);
            let dt = t0 - t1;
            if dt == 0 {
                continue;
            }
            for other in 0..shape.values.len() {
                if !budget.spend(rows.len() * 4 + 1) {
                    return false;
                }
                let o0 = i128::from(observations[0].values[other]);
                let o1 = i128::from(observations[1].values[other]);
                let change = o0 - o1;
                if change == 0 {
                    continue;
                }
                let fits = observations.iter().zip(values).all(|(o, t)| {
                    (i128::from(o.values[other]) - o1)
                        .checked_mul(dt)
                        .zip((i128::from(t[target]) - t1).checked_mul(change))
                        .is_some_and(|(a, b)| a == b)
                });
                if !fits {
                    return false;
                }
                let Some(numerator) = (i128::from(current.values[other]) - o1).checked_mul(dt)
                else {
                    return false;
                };
                if numerator % change != 0 {
                    return false;
                }
                let Some(solved) = t1
                    .checked_add(numerator / change)
                    .and_then(|n| u64::try_from(n).ok())
                else {
                    return false;
                };
                if bound[target].is_some_and(|previous| previous != solved) {
                    return false;
                }
                bound[target] = Some(solved);
            }
        }
    }
    for (next, bound) in next.iter_mut().zip(bound) {
        if let Some(bound) = bound {
            *next = bound;
        }
    }
    true
}

fn sequences(
    rows: &[Row<'_>],
    left: &Row<'_>,
    right: &Row<'_>,
    prefix: &str,
    out: &mut Predictions,
    budget: &mut Budget<'_>,
) {
    let at = left.tokens.len();
    for period in 1..=4 {
        if rows.len() < period * 2 {
            continue;
        }
        let last = &rows[rows.len() - period];
        let Some(token) = last
            .tokens
            .get(at)
            .filter(|token| token.kind == TokenKind::Word)
        else {
            continue;
        };
        let Some(shape) = numbers(token.text) else {
            continue;
        };
        if !shape.parts[0].starts_with(prefix) && !token.text.starts_with(prefix) {
            continue;
        }
        let mut aligned = Vec::new();
        let mut values = Vec::new();
        let mut widths = shape.widths.clone();
        for index in (0..=rows.len() - period).rev().step_by(period) {
            let row = &rows[index];
            let Some(name) = row.tokens.get(at).and_then(|token| numbers(token.text)) else {
                break;
            };
            if name.parts != shape.parts || compatible(last, row, budget).is_none() {
                break;
            }
            for (width, other) in widths.iter_mut().zip(&name.widths) {
                *width = (*width).max(*other);
            }
            aligned.push(row);
            values.push(name.values);
        }
        if aligned.len() < 2 {
            continue;
        }
        let mut next = Vec::new();
        let mut valid = true;
        let mut changed = false;
        for field in 0..shape.values.len() {
            let step = i128::from(values[0][field]) - i128::from(values[1][field]);
            let fits = values
                .windows(2)
                .all(|pair| i128::from(pair[0][field]) - i128::from(pair[1][field]) == step);
            changed |= step != 0;
            // Two ordered values establish the simplest arithmetic rule. More
            // visible examples must all confirm it; no stride-size exception.
            if !fits {
                valid = false;
                break;
            }
            let Some(value) = i128::from(values[0][field])
                .checked_add(step)
                .and_then(|n| u64::try_from(n).ok())
            else {
                valid = false;
                break;
            };
            next.push(value);
        }
        valid &= changed;
        if valid && !constrained_numbers(&aligned, &values, left, right, at, &mut next, budget) {
            out.conflicts = out.conflicts.max(aligned.len() as f64);
            break;
        }
        if valid {
            if let Some(word) = render(&shape.parts, &next, &widths) {
                if verify_sequence(&aligned, left, right, at, &word, budget) {
                    out.add(word, prefix, aligned.len().min(4) as f64, Kind::Sequence);
                } else {
                    out.conflicts = out.conflicts.max(aligned.len() as f64);
                }
            }
        } else if shape.values.len() == 1 && shape.parts.last() == Some(&"") {
            let stem = shape.parts[0].to_owned();
            if verify_sequence(&aligned, left, right, at, &stem, budget) {
                out.add(stem, prefix, 1., Kind::Stem);
            }
        }
        // A shorter period explains this role even when its numbers conflict.
        // Never rescue an irregular series by cherry-picking a wider period.
        break;
    }
}

fn boundary(word: &str, at: usize) -> bool {
    if at == 0 || at == word.len() {
        return true;
    }
    let Some(before) = word[..at].chars().next_back() else {
        return true;
    };
    let Some(after) = word[at..].chars().next() else {
        return true;
    };
    matches!(before, '_' | '$')
        || matches!(after, '_' | '$')
        || before.is_lowercase() && after.is_uppercase()
        || before.is_ascii_digit() != after.is_ascii_digit()
}
#[derive(Clone, Copy)]
enum Operation<'a> {
    Prefix(&'a str, &'a str),
    Suffix(&'a str, &'a str),
}
impl Operation<'_> {
    fn apply(&self, word: &str) -> Option<String> {
        let result = match self {
            Self::Prefix(from, to) => {
                let rest = word.strip_prefix(from)?;
                if !boundary(word, from.len()) {
                    return None;
                }
                format!("{to}{rest}")
            }
            Self::Suffix(from, to) => {
                let rest = word.strip_suffix(from)?;
                if !from.is_empty() && !boundary(word, rest.len()) {
                    return None;
                }
                format!("{rest}{to}")
            }
        };
        (result.len() <= WORD_BYTES).then_some(result)
    }
    fn affix_addition(&self) -> bool {
        match self {
            Self::Prefix(from, _) | Self::Suffix(from, _) => from.is_empty(),
        }
    }
}
fn operations<'a>(a: &'a str, b: &'a str, budget: &mut Budget<'_>) -> Vec<Operation<'a>> {
    let mut result = Vec::new();
    if a == b || !budget.spend(a.len() + b.len()) {
        return result;
    }
    let mut suffix = 0;
    for (x, y) in a.chars().rev().zip(b.chars().rev()) {
        if x != y {
            break;
        }
        suffix += x.len_utf8();
    }
    while suffix > 0 && (!boundary(a, a.len() - suffix) || !boundary(b, b.len() - suffix)) {
        suffix -= a[a.len() - suffix..].chars().next().unwrap().len_utf8();
    }
    if suffix >= 2 {
        result.push(Operation::Prefix(
            &a[..a.len() - suffix],
            &b[..b.len() - suffix],
        ));
    }
    let mut shared = 0;
    for (x, y) in a.chars().zip(b.chars()) {
        if x != y {
            break;
        }
        shared += x.len_utf8();
    }
    if shared != a.len() && shared != b.len() {
        while shared > 0 && (!boundary(a, shared) || !boundary(b, shared)) {
            shared -= a[..shared].chars().next_back().unwrap().len_utf8();
        }
    }
    if shared >= 2 {
        result.push(Operation::Suffix(&a[shared..], &b[shared..]));
    }
    result
}
fn verify_analogy(
    a: &Row<'_>,
    b: &Row<'_>,
    c: &Row<'_>,
    left: &Row<'_>,
    right: &Row<'_>,
    at: usize,
    candidate: &str,
    budget: &mut Budget<'_>,
) -> bool {
    let Some(mut map) = compatible(a, c, budget) else {
        return false;
    };
    if !map.bind(b.tokens[at].text, candidate) {
        return false;
    }
    for (index, current) in visible(left, right, at) {
        if !budget.spend(TOKENS) {
            return false;
        }
        let Some(source) = b.tokens.get(index) else {
            return false;
        };
        if source.kind != current.kind {
            return false;
        }
        let expected = if source.kind == TokenKind::Punctuation {
            source.text
        } else {
            map.get(source.text).unwrap_or(source.text)
        };
        if expected != current.text {
            return false;
        }
    }
    true
}
fn transformations(
    rows: &[Row<'_>],
    left: &Row<'_>,
    right: &Row<'_>,
    prefix: &str,
    out: &mut Predictions,
    budget: &mut Budget<'_>,
) {
    let at = left.tokens.len();
    for period in 2..=4 {
        for gap in 1..period {
            let Some(anchor) = rows.len().checked_sub(gap + period) else {
                continue;
            };
            let a = &rows[anchor];
            let b = &rows[rows.len() - period];
            let c = &rows[rows.len() - gap];
            let Some((a_name, b_name, c_name)) = a
                .tokens
                .get(at)
                .zip(b.tokens.get(at))
                .zip(c.tokens.get(at))
                .map(|((a, b), c)| (a, b, c))
            else {
                continue;
            };
            if [a_name, b_name, c_name]
                .iter()
                .any(|token| token.kind != TokenKind::Word)
                || a_name.text == c_name.text
            {
                continue;
            }
            if compatible(a, c, budget).is_none() {
                continue;
            }
            for operation in operations(a_name.text, b_name.text, budget) {
                let Some(word) = operation.apply(c_name.text) else {
                    continue;
                };
                if !word.starts_with(prefix) || word == c_name.text {
                    continue;
                }
                let mut witnesses = Vec::new();
                let mut pair = anchor;
                loop {
                    let a = &rows[pair];
                    let b = &rows[pair + gap];
                    if let Some((a, b)) = a.tokens.get(at).zip(b.tokens.get(at)) {
                        if operation.apply(a.text).as_deref() == Some(b.text)
                            && !witnesses.contains(&a.text)
                        {
                            witnesses.push(a.text);
                        }
                    }
                    let Some(previous) = pair.checked_sub(period) else {
                        break;
                    };
                    pair = previous;
                }
                if witnesses.is_empty()
                    || witnesses.len() < 2
                        && (operation.affix_addition() || c_name.text.starts_with(prefix))
                {
                    continue;
                }
                if verify_analogy(a, b, c, left, right, at, &word, budget) {
                    out.add(
                        word,
                        prefix,
                        (witnesses.len() + 1).min(4) as f64,
                        Kind::Transform,
                    );
                } else {
                    out.conflicts = out.conflicts.max(witnesses.len() as f64);
                }
            }
        }
    }
}

pub(crate) fn predict(
    left: &str,
    right: &str,
    prefix: &str,
    complete_start: bool,
    cancelled: &impl Fn() -> bool,
) -> Option<Predictions> {
    if cancelled() {
        return None;
    }
    let mut out = Predictions::default();
    if prefix.is_empty()
        || !prefix
            .chars()
            .any(|ch| ch.is_alphabetic() || matches!(ch, '_' | '$'))
        || prefix.len() > 64
        || left.len() > 4096
        || right.len() > 512
    {
        return Some(out);
    }
    let Some((before, current)) = left.rsplit_once('\n') else {
        return Some(out);
    };
    let mut budget = Budget {
        used: 0,
        limited: false,
        cancel: cancelled,
    };
    let Some(left) = lex(current, &mut budget) else {
        return Some(out);
    };
    let Some(right) = lex(right, &mut budget) else {
        return Some(out);
    };
    let mut lines: Vec<_> = before.lines().rev().take(ROWS).collect();
    lines.reverse();
    if !complete_start && before.lines().count() <= ROWS && !lines.is_empty() {
        lines.remove(0);
    }
    let mut rows = Vec::new();
    for line in lines {
        let Some(row) = lex(line, &mut budget) else {
            rows.clear();
            continue;
        };
        if row.tokens.is_empty() || row.indent != left.indent || scope_boundary(&row) {
            rows.clear();
            continue;
        }
        rows.push(row);
    }
    #[cfg(test)]
    thread_local! { static MODE:String=std::env::var("VTCODE_GENERATION_MODE").unwrap_or_else(|_|"full".into()); }
    #[cfg(test)]
    let (numeric, transform, relational, cases) = MODE.with(|mode| {
        (
            matches!(mode.as_str(), "full" | "numeric" | "layout" | "affix"),
            matches!(mode.as_str(), "full" | "transform" | "layout" | "affix"),
            matches!(mode.as_str(), "full" | "affix"),
            mode == "full",
        )
    });
    #[cfg(not(test))]
    let (numeric, transform, relational, cases) = (true, true, true, true);
    if numeric {
        sequences(&rows, &left, &right, prefix, &mut out, &mut budget);
    }
    // Numeric names have an invariant stem and independently verified row
    // shape, so they can help after one letter. Richer string programs still
    // need a longer prefix to avoid broad speculative generation.
    let long_prefix = prefix.chars().count() >= 2;
    if transform && long_prefix {
        transformations(&rows, &left, &right, prefix, &mut out, &mut budget);
    }
    out.limited |= budget.limited;
    if out.limited {
        out.rows.clear();
        out.conflicts = 0.;
    } else if relational && long_prefix {
        let mut related = Predictions::default();
        relations::learn(
            &rows,
            &left,
            &right,
            prefix,
            cases,
            &mut related,
            &mut budget,
        );
        let added = related
            .rows
            .iter()
            .filter(|row| !out.rows.iter().any(|old| old.word == row.word))
            .count();
        if budget.limited || related.limited || out.rows.len() + added > CANDIDATES {
            // An optional richer search cannot erase already verified counters
            // or analogies. Its own partial/ambiguous results are discarded.
            out.limited = true;
        } else {
            for row in related.rows {
                out.add(row.word, prefix, row.count, row.kind);
            }
        }
    }
    out.work = budget.used;
    if cancelled() {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn words(source: &str, prefix: &str) -> Vec<String> {
        let left = source.strip_suffix(prefix).unwrap();
        predict(left, "", prefix, true, &|| false)
            .unwrap()
            .candidates()
            .map(str::to_owned)
            .collect()
    }
    #[test]
    fn numeric_names_are_programs_rather_than_observed_vocabulary() {
        for (source, prefix, expected) in [
            ("my_var1 = a\nmy_var2 = b\nmy", "my", "my_var3"),
            ("v_07 = a\nv_08 = b\nv_", "v_", "v_09"),
            ("slot9 = a\nslot10 = b\nsl", "sl", "slot11"),
            ("slot3 = a\nslot2 = b\nsl", "sl", "slot1"),
            ("slot1 = a\nslot3 = b\nslot5 = c\nsl", "sl", "slot7"),
            ("slot10 = a\nslot20 = b\nsl", "sl", "slot30"),
            ("slot20 = a\nslot10 = b\ns", "s", "slot0"),
            ("slot_005 = a\nslot_010 = b\ns", "s", "slot_015"),
            ("cell1_10 = a\ncell3_20 = b\nc", "c", "cell5_30"),
            ("cell1_2 = a\ncell2_3 = b\nce", "ce", "cell3_4"),
            ("slot1 = a\nslot4 = b\nslot9 = c\nsl", "sl", "slot"),
        ] {
            assert!(
                words(source, prefix).iter().any(|word| word == expected),
                "{source}"
            );
        }
    }

    #[test]
    fn early_sequences_still_require_consistent_rows_and_all_visible_evidence() {
        assert!(words("slot10 = a\ns", "s").is_empty());
        for source in [
            "slot10 = a\nslot20 = b\nslot35 = c\ns",
            "slot10 = a\nslot20 = b\nslot20 = c\ns",
            "slot10 = a\nother20 = b\ns",
            "slot20 = a\nslot5 = b\ns",
        ] {
            assert!(words(source, "s").iter().all(|word| {
                !word.chars().any(|ch| ch.is_ascii_digit())
            }), "{source}");
        }
        let conflict = predict("slot10 = fixed\nslot20 = fixed\n", " = wrong", "s", true, &|| false).unwrap();
        assert!(!conflict.active());
        assert!(words("load_user(one)\nstore_user(one)\nload_order(two)\ns", "s").is_empty());
    }
    #[test]
    fn four_substantial_statements_predict_identifiers_at_each_position() {
        let before = (1..=4).map(|n| format!(
            "let record{n} = build_entry(source{n}, options, theme, bounds).with_action(action{n}).with_border(style{n}, color{n}).with_padding(Insets::new(1, 2, 3, 4)).finish();\n"
        )).collect::<String>();
        for (current, prefix, expected) in [
            ("let ", "record", "record5"),
            ("let record5 = build_entry(", "source", "source5"),
            (
                "let record5 = build_entry(source5, options, theme, bounds).with_action(",
                "action",
                "action5",
            ),
        ] {
            let input = format!("{before}{current}{prefix}");
            assert!(
                words(&input, prefix).iter().any(|word| word == expected),
                "{input}"
            );
        }
    }
    #[test]
    fn row_partitions_preserve_both_directions_of_identifier_bindings() {
        let mut budget = Budget {
            used: 0,
            limited: false,
            cancel: &|| false,
        };
        for (a, b, expected) in [
            ("call(a, a, b)", "emit(c, c, d)", true),
            ("call(a, a, b)", "emit(c, d, d)", false),
            ("call(a, b, c)", "emit(x, x, z)", false),
            ("获取(值, 值)", "释放(新, 新)", true),
            ("value = 'x'", "result = 'y'", true),
            ("value = other", "result = 'y'", false),
        ] {
            let a = lex(a, &mut budget).unwrap();
            let b = lex(b, &mut budget).unwrap();
            assert_eq!(compatible(&a, &b, &mut budget).is_some(), expected);
        }
        // The extra binding fits existing token padding on supported targets.
        assert_eq!(
            std::mem::size_of::<Token<'_>>(),
            3 * std::mem::size_of::<usize>()
        );
    }
    #[test]
    fn transformations_copy_identifier_components_with_verified_bindings() {
        for (source, prefix, expected) in [
            (
                "load_user(one)\nstore_user(one)\nload_order(two)\nst",
                "st",
                "store_order",
            ),
            (
                "user_input = a\nuser_output = a\norder_input = b\norder_o",
                "order_o",
                "order_output",
            ),
            (
                "take_item1()\nreturn_item1()\ntake_item2()\nret",
                "ret",
                "return_item2",
            ),
            (
                "root_item = a\nroot_items = a\nroot_entry = b\nroot_entrys = b\nroot_object = c\nro",
                "ro",
                "root_objects",
            ),
            (
                "getUser(one)\nsetUser(one)\ngetOrder(two)\nse",
                "se",
                "setOrder",
            ),
            (
                "fetch$User(one)\nstore$User(one)\nfetch$Order(two)\nsto",
                "sto",
                "store$Order",
            ),
            (
                "获取_旧(one)\n释放_旧(one)\n获取_新(two)\n释放",
                "释放",
                "释放_新",
            ),
        ] {
            assert!(
                words(source, prefix).iter().any(|word| word == expected),
                "{source}"
            );
        }
    }
    #[test]
    fn missing_evidence_scope_breaks_and_suffix_conflicts_back_off() {
        assert!(words("slot1 = a\nsl", "sl").is_empty());
        assert!(words("{\nslot1 = a\nslot2 = b\n}\nsl", "sl").is_empty());
        assert!(words("    slot1 = a\n    slot2 = b\nsl", "sl").is_empty());
        let result = predict(
            "slot1 = fixed\nslot2 = fixed\n",
            " = wrong",
            "sl",
            true,
            &|| false,
        )
        .unwrap();
        assert!(!result.active());
        assert_eq!(result.probability("slot1", 0.25), 0.25);
        assert!(predict("slot1\nslot2\n", "", "sl", true, &|| true).is_none());
    }

    #[test]
    fn visible_numeric_relationships_and_fixed_fields_constrain_sequences() {
        for (left, right, prefix, expected) in [
            (
                "slot1 = source1\nslot2 = source2\n",
                " = source9",
                "sl",
                "slot9",
            ),
            ("slot1 = 10\nslot2 = 20\n", " = 90", "sl", "slot9"),
            ("IPv4_slot1 = a\nIPv4_slot2 = b\n", "", "IP", "IPv4_slot3"),
        ] {
            let result = predict(left, right, prefix, true, &|| false).unwrap();
            assert!(
                result.candidates().any(|word| word == expected),
                "{left}{prefix}{right}"
            );
        }
        let bad = predict(
            "slot1 = source1_2\nslot2 = source2_3\n",
            " = source9_20",
            "sl",
            true,
            &|| false,
        )
        .unwrap();
        assert!(
            !bad.active(),
            "contradictory suffix equations must not invent a counter"
        );
    }

    #[test]
    fn bounds_utf8_overflow_and_duplicate_patterns_are_safe() {
        assert!(
            words(
                "slot18446744073709551614\nslot18446744073709551615\nsl",
                "sl"
            )
            .iter()
            .all(|word| word == "slot")
        );
        assert!(words("slot999999999999999999999999999999999999\nslot999999999999999999999999999999999999\nsl","sl").is_empty());
        assert!(words("slot_界1 = a\nslot_界2 = b\nsl", "sl").contains(&"slot_界3".into()));
        let text = format!("slot1 = a\n{}", "slot2 = b\n".repeat(2000));
        let result = predict(&text[text.len() - 1000..], "", "sl", false, &|| false).unwrap();
        assert!(result.candidates().all(|word| word == "slot"));
        assert!(result.work <= WORK);
        let mut budget = Budget {
            used: 0,
            limited: false,
            cancel: &|| false,
        };
        assert!(!budget.spend(WORK + 1));
        assert!(budget.limited);
    }

    #[test]
    fn exhausted_matching_is_an_exact_backoff_and_cancelled_work_is_discarded() {
        let left = "x1_2_3_4 ".repeat(47);
        let mut text = (1..=8)
            .map(|n| format!("{left}root{n}_{}_{}_{}\n", n + 1, n + 2, n + 3))
            .collect::<String>();
        text.push_str(&left);
        let result = predict(&text, "", "ro", true, &|| false).unwrap();
        assert!(result.limited);
        assert!(result.candidates().next().is_none());
        assert_eq!(result.probability("root", 0.4), 0.4);
        assert!(result.work <= WORK);
        let checks = std::cell::Cell::new(0);
        let cancel = || {
            let n = checks.get() + 1;
            checks.set(n);
            n > 4
        };
        assert!(predict("slot1 = a\nslot2 = b\n", "", "sl", true, &cancel).is_none());
    }

    #[test]
    fn cloned_name_pairs_do_not_increase_confidence() {
        let pair = "load_user(one)\nstore_user(one)\n";
        let small = format!("{pair}load_order(two)\n");
        let repeated = format!("{}load_order(two)\n", pair.repeat(2000));
        let before = predict(&small, "", "sto", true, &|| false).unwrap();
        let after = predict(
            &repeated[repeated.len() - 1000..],
            "",
            "sto",
            false,
            &|| false,
        )
        .unwrap();
        assert_eq!(
            before.probability("store_order", 0.),
            after.probability("store_order", 0.)
        );
        assert!(before.probability("store_order", 0.) > 0.);
    }

    #[test]
    fn arbitrary_unicode_context_is_bounded_and_never_fabricates_invalid_identifiers() {
        let alphabet = [
            'a', 'b', '1', '2', '_', '$', '界', 'é', '\u{2003}', ' ', '\n', '\t', '(', ')', '{',
            '}', '[', ']', '\'', '"', '`', '\\',
        ];
        let mut random = 0x24e8c739b1d50a6fu64;
        for _ in 0..2000 {
            let mut text = String::new();
            for _ in 0..256 {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                text.push(alphabet[(random as usize) % alphabet.len()]);
            }
            text.push('\n');
            let result = predict(&text, "", "ab", false, &|| false).unwrap();
            assert!(result.work <= WORK);
            assert!(result.rows.len() <= CANDIDATES);
            for word in result.candidates() {
                assert!(
                    word.starts_with("ab")
                        && word.len() <= WORD_BYTES
                        && word.chars().all(word_char)
                );
                assert!(result.kind(word).is_some());
            }
        }
        assert!(words("\u{2003}slot1 = a\n\u{2003}slot2 = b\nsl", "sl").is_empty());
        assert!(
            words("\u{2003}slot1 = a\n\u{2003}slot2 = b\n\u{2003}sl", "sl")
                .contains(&"slot3".into())
        );
    }
}
