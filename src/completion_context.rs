//! Small streaming lexical and structural contexts shared by learning and queries.
//! No grammar, AST, file reads, or unbounded source history is required.
use std::collections::{BTreeMap, VecDeque};
use std::{borrow::Cow, sync::Arc};

const TOKEN_BYTES: usize = 160;
const STACK_DEPTH: usize = 32;
const ALIASES: usize = 96;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Domain {
    #[default]
    Code,
    String,
    Comment,
}

impl Domain {
    pub(crate) fn slot(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Context {
    // Full receiver, receiver family, call/argument, call family/argument,
    // assignment target, preceding token, preceding pair, and structural shape.
    pub(crate) keys: [u64; 8],
    pub(crate) source_family: u64,
    pub(crate) source_scope: u64,
    pub(crate) domain: Domain,
    pub(crate) scope: [u64; 4],
    pub(crate) declaration: bool,
    pub(crate) type_position: bool,
}

impl Context {
    pub(crate) fn affinity(self, other: Self, local: bool) -> i64 {
        let mut score = 0;
        for (slot, weight) in [230, 155, 210, 150, 175, 32, 48, 14]
            .into_iter()
            .enumerate()
        {
            if self.keys[slot] != 0 && self.keys[slot] == other.keys[slot] {
                score += weight;
            }
        }
        if self.keys[0] != 0
            && other.keys[0] != 0
            && self.keys[0] != other.keys[0]
            && (self.keys[1] == 0 || self.keys[1] != other.keys[1])
        {
            score -= 45;
        }
        if self.domain != other.domain {
            score -= if other.domain == Domain::Comment {
                145
            } else {
                65
            };
        } else if self.domain != Domain::Code {
            score += 65;
        }
        if local && self.domain == Domain::Code {
            let own = self
                .scope
                .iter()
                .rev()
                .find(|&&key| key != 0)
                .copied()
                .unwrap_or(0);
            let theirs = other
                .scope
                .iter()
                .rev()
                .find(|&&key| key != 0)
                .copied()
                .unwrap_or(0);
            score += if own == theirs {
                42
            } else if theirs == 0 || self.scope.contains(&theirs) {
                16
            } else if self.keys[..2]
                .iter()
                .zip(other.keys)
                .any(|(&a, b)| a != 0 && a == b)
            {
                // Members transfer between sibling scopes and factory instances.
                0
            } else {
                -100
            };
        }
        score.clamp(-220, 430)
    }

    pub(crate) fn member(self) -> bool {
        self.keys[0] != 0
    }
    pub(crate) fn structural(self) -> bool {
        self.keys[..5].iter().any(|&key| key != 0)
    }
}

pub(crate) fn hash_parts(kind: u8, parts: &[&str]) -> u64 {
    let mut value = 0xcbf29ce484222325u64 ^ u64::from(kind);
    for part in parts {
        for byte in part.bytes().chain(std::iter::once(0xff)) {
            value = (value ^ u64::from(byte)).wrapping_mul(0x100000001b3);
        }
    }
    value.max(1)
}

pub(crate) fn initials(word: &str) -> String {
    initial_chars(word).collect()
}

pub(crate) fn initials_start_with(word: &str, prefix: &str) -> bool {
    let mut initials = initial_chars(word);
    prefix.chars().all(|ch| initials.next() == Some(ch))
}

fn initial_chars(word: &str) -> impl Iterator<Item = char> + '_ {
    let mut previous = None;
    let mut chars = word.chars().peekable();
    std::iter::from_fn(move || {
        loop {
            let ch = chars.next()?;
            let boundary = previous.is_none_or(|before: char| {
                !before.is_alphanumeric()
                    || (before.is_alphabetic() && ch.is_numeric())
                    || (before.is_numeric() && ch.is_alphabetic())
                    || (before.is_lowercase() && ch.is_uppercase())
                    || (before.is_uppercase()
                        && ch.is_uppercase()
                        && chars.peek().is_some_and(|next| next.is_lowercase()))
            });
            previous = Some(ch);
            if boundary && ch.is_alphanumeric() {
                return Some(ch);
            }
        }
    })
    .flat_map(char::to_lowercase)
}

#[derive(Clone)]
struct Frame {
    delimiter: char,
    head: String,
    argument: u16,
    scope: [u64; 4],
}

#[derive(Clone, Default)]
struct Tracker {
    previous: String,
    before: String,
    chain: String,
    operator: String,
    assignment: String,
    assignment_value: bool,
    literal: String,
    scope: [u64; 4],
    frames: Vec<Frame>,
    indents: Vec<(usize, [u64; 4])>,
    indent: usize,
    line_start: bool,
    new_statement: bool,
    overflow: usize,
    aliases: Arc<BTreeMap<String, (u64, String)>>,
}

impl Tracker {
    fn canonical<'a>(&'a self, receiver: &'a str) -> Cow<'a, str> {
        let split = receiver
            .find(['.', ':', '-', '['])
            .unwrap_or(receiver.len());
        let first = &receiver[..split];
        if let Some((owner, alias)) = self.aliases.get(first)
            && (*owner == 0 || self.scope.contains(owner))
        {
            return Cow::Owned(bounded(&format!("{alias}{}", &receiver[split..])));
        }
        Cow::Borrowed(receiver)
    }

    fn family(receiver: &str) -> &str {
        receiver
            .rsplit(['.', ':', '>'])
            .find(|part| !part.is_empty())
            .unwrap_or(receiver)
    }

    fn context(&self, domain: Domain) -> Context {
        let mut keys = [0; 8];
        if matches!(self.operator.as_str(), "." | "::" | "->" | "?.") && !self.chain.is_empty() {
            keys[0] = hash_parts(1, &[&self.chain]);
            keys[1] = hash_parts(2, &[&self.canonical(&self.chain)]);
        }
        if let Some(frame) = self
            .frames
            .iter()
            .rev()
            .find(|frame| frame.delimiter == '(' && !frame.head.is_empty())
        {
            let argument = frame.argument.to_string();
            keys[2] = hash_parts(3, &[&frame.head, &argument]);
            keys[3] = hash_parts(4, &[Self::family(&frame.head), &argument]);
        }
        if !self.assignment.is_empty() {
            keys[4] = hash_parts(5, &[&self.assignment]);
        }
        if !self.previous.is_empty() {
            keys[5] = hash_parts(6, &[&self.previous, &self.operator]);
            if !self.before.is_empty() {
                keys[6] = hash_parts(7, &[&self.before, &self.previous, &self.operator]);
            }
        }
        if matches!(self.operator.as_str(), "&" | "*" | "=" | ":") {
            keys[7] = hash_parts(8, &[&self.operator]);
        }
        Context {
            keys,
            source_family: 0,
            source_scope: 0,
            domain,
            scope: self.scope,
            declaration: matches!(
                self.previous.as_str(),
                "let"
                    | "var"
                    | "const"
                    | "mut"
                    | "fn"
                    | "def"
                    | "func"
                    | "function"
                    | "class"
                    | "struct"
                    | "enum"
                    | "trait"
                    | "interface"
                    | "type"
            ),
            type_position: matches!(
                self.previous.as_str(),
                "class"
                    | "struct"
                    | "enum"
                    | "trait"
                    | "interface"
                    | "type"
                    | "new"
                    | "extends"
                    | "implements"
            ),
        }
    }

    fn word(&mut self, word: &str) {
        let member = matches!(self.operator.as_str(), "." | "::" | "->" | "?.");
        if member && !self.chain.is_empty() {
            self.chain.push_str(&self.operator);
            self.chain.push_str(word);
            let mut end = self.chain.len().min(TOKEN_BYTES);
            while !self.chain.is_char_boundary(end) {
                end -= 1;
            }
            self.chain.truncate(end);
        } else {
            self.chain.clear();
            self.chain.push_str(word);
        }
        if !self.assignment.is_empty() && (!self.assignment_value || member) {
            if self.aliases.len() >= ALIASES
                && !self.aliases.contains_key(&self.assignment)
                && let Some(key) = self.aliases.keys().next().cloned()
            {
                Arc::make_mut(&mut self.aliases).remove(&key);
            }
            let owner = self
                .scope
                .iter()
                .rev()
                .find(|&&key| key != 0)
                .copied()
                .unwrap_or(0);
            let alias = self.canonical(&self.chain).into_owned();
            Arc::make_mut(&mut self.aliases).insert(self.assignment.clone(), (owner, alias));
            self.assignment_value = true;
        }
        std::mem::swap(&mut self.before, &mut self.previous);
        self.previous.clear();
        self.previous.push_str(word);
        self.operator.clear();
        self.literal.clear();
    }

    fn enter_scope(&mut self, offset: usize) {
        let key = hash_parts(9, &[&offset.to_string()]);
        self.scope.rotate_left(1);
        self.scope[3] = key;
    }

    fn start_token(&mut self, offset: usize) {
        if !self.line_start {
            return;
        }
        self.line_start = false;
        if self
            .frames
            .iter()
            .any(|frame| matches!(frame.delimiter, '(' | '[' | '{'))
        {
            return;
        }
        while self
            .indents
            .last()
            .is_some_and(|&(indent, _)| self.indent < indent)
        {
            self.scope = self.indents.pop().unwrap().1;
        }
        if self.indent > self.indents.last().map_or(0, |&(indent, _)| indent)
            && self.indents.len() < STACK_DEPTH
        {
            self.indents.push((self.indent, self.scope));
            self.enter_scope(offset);
        }
    }

    fn start_word(&mut self, offset: usize) {
        self.start_token(offset);
        if std::mem::take(&mut self.new_statement) {
            self.previous.clear();
            self.before.clear();
            self.chain.clear();
            self.operator.clear();
        }
    }

    fn symbol(&mut self, ch: char, offset: usize) {
        if ch == '\n' {
            self.line_start = true;
            self.indent = 0;
            let continuation = self
                .frames
                .iter()
                .any(|frame| matches!(frame.delimiter, '(' | '['))
                || self.overflow > 0
                || (!self.assignment.is_empty() && !self.assignment_value)
                || matches!(self.operator.as_str(), "." | "::" | "->" | "?.");
            if !continuation {
                // Keep the chain until the next token: a leading dot on the
                // following line still belongs to the preceding receiver.
                self.new_statement = true;
                self.assignment.clear();
                self.assignment_value = false;
            }
            return;
        }
        if ch.is_whitespace() {
            if self.line_start {
                self.indent = self.indent.saturating_add(if ch == '\t' { 4 } else { 1 });
            }
            return;
        }
        self.start_token(offset);
        match ch {
            '(' | '[' | '{' => {
                if self.frames.len() < STACK_DEPTH && self.overflow == 0 {
                    let head = if ch == '{' {
                        String::new()
                    } else {
                        self.canonical(&self.chain).into_owned()
                    };
                    self.frames.push(Frame {
                        delimiter: ch,
                        head: head.clone(),
                        argument: 0,
                        scope: self.scope,
                    });
                    if ch == '('
                        && self.assignment_value
                        && let Some((_, alias)) =
                            Arc::make_mut(&mut self.aliases).get_mut(&self.assignment)
                    {
                        *alias = bounded(&format!("@{head}"));
                    }
                    if ch == '{' {
                        self.enter_scope(offset);
                    }
                } else {
                    self.overflow = self.overflow.saturating_add(1);
                }
                self.chain.clear();
                self.operator.clear();
            }
            ')' | ']' | '}' => {
                if self.overflow > 0 {
                    self.overflow -= 1;
                    return;
                }
                let opening = match ch {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                if let Some(index) = self
                    .frames
                    .iter()
                    .rposition(|frame| frame.delimiter == opening)
                {
                    let frame = self.frames.remove(index);
                    self.frames.truncate(index);
                    if ch == '}' {
                        self.scope = frame.scope;
                    }
                    self.chain = if ch == ')' {
                        bounded(&format!("@{}", frame.head))
                    } else {
                        frame.head
                    };
                }
                self.assignment.clear();
                self.assignment_value = false;
                self.operator.clear();
            }
            ',' => {
                if let Some(frame) = self.frames.last_mut()
                    && frame.delimiter == '('
                {
                    frame.argument = frame.argument.saturating_add(1);
                }
                self.assignment.clear();
                self.assignment_value = false;
                self.chain.clear();
                self.operator.clear();
            }
            ';' => {
                self.assignment.clear();
                self.assignment_value = false;
                self.chain.clear();
                self.operator.clear();
            }
            '=' => {
                if matches!(self.operator.as_str(), "=" | "!" | "<" | ">") {
                    self.assignment.clear();
                } else {
                    self.assignment = self.chain.clone();
                    self.assignment_value = false;
                }
                self.operator = "=".into();
            }
            ':' => {
                if self.operator == ":" {
                    self.operator = "::".into();
                    self.assignment.clear();
                } else {
                    self.assignment = if self.literal.is_empty() {
                        self.chain.clone()
                    } else {
                        self.literal.clone()
                    };
                    self.assignment_value = false;
                    self.operator = ":".into();
                }
            }
            '.' => {
                self.new_statement = false;
                self.operator = if self.operator == "?" {
                    "?.".into()
                } else {
                    ".".into()
                };
            }
            '>' if self.operator == "-" => {
                self.new_statement = false;
                self.operator = "->".into();
            }
            _ => {
                self.operator.clear();
                self.operator.push(ch);
            }
        }
    }
}

fn bounded(text: &str) -> String {
    let mut end = text.len().min(TOKEN_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

#[derive(Clone, Copy, Debug, Default)]
enum Mode {
    #[default]
    Code,
    LineComment,
    BlockComment(u8),
    Quote(char, bool),
}

pub(crate) enum Event<'a> {
    Word {
        word: &'a str,
        context: Context,
        offset: usize,
    },
    Call {
        offset: usize,
    },
}

#[derive(Clone, Default)]
pub(crate) struct Analyzer {
    tracker: Tracker,
    mode: Mode,
    pending: VecDeque<char>,
    word: String,
    oversized: bool,
    word_context: Context,
    word_offset: usize,
    offset: usize,
    skip: usize,
    escaped: bool,
    last_code_word: Option<usize>,
    literal_word: String,
    literal_words: usize,
    previous: char,
}

impl Analyzer {
    pub(crate) fn at(offset: usize) -> Self {
        Self {
            offset,
            ..Self::default()
        }
    }

    pub(crate) fn feed(&mut self, text: &str, mut emit: impl FnMut(Event<'_>)) {
        for ch in text.chars() {
            self.pending.push_back(ch);
            if self.pending.len() >= 4 && !self.awaiting_apostrophe() {
                self.step(&mut emit);
            }
        }
    }

    fn awaiting_apostrophe(&self) -> bool {
        if !matches!(self.mode, Mode::Code)
            || self.pending.front() != Some(&'\'')
            || self.pending.len() > TOKEN_BYTES
            || matches!(self.previous, '&' | '<')
        {
            return false;
        }
        let mut tail = self.pending.iter().skip(1).copied();
        tail.clone().next().is_some_and(char::is_alphabetic)
            && tail.all(|ch| ch.is_alphanumeric() || ch == '_' || ch.is_whitespace())
    }

    pub(crate) fn flush(&mut self, mut emit: impl FnMut(Event<'_>)) {
        while !self.pending.is_empty() {
            self.step(&mut emit);
        }
        self.finish_word(&mut emit);
    }

    /// An unfinished identifier is a query, not a new binding or observation.
    pub(crate) fn query(
        &mut self,
        offset: usize,
        prefix_empty: bool,
        mut emit: impl FnMut(Event<'_>),
    ) -> Context {
        while !self.pending.is_empty() {
            self.step(&mut emit);
        }
        if prefix_empty {
            self.context()
        } else {
            self.token_context(offset).unwrap_or_else(|| self.context())
        }
    }

    pub(crate) fn context(&self) -> Context {
        self.tracker.context(self.domain())
    }

    fn domain(&self) -> Domain {
        match self.mode {
            Mode::Code => Domain::Code,
            Mode::LineComment | Mode::BlockComment(_) => Domain::Comment,
            Mode::Quote(..) => Domain::String,
        }
    }

    pub(crate) fn token_context(&self, offset: usize) -> Option<Context> {
        (self.word_offset == offset).then_some(self.word_context)
    }

    fn finish_word(&mut self, emit: &mut impl FnMut(Event<'_>)) {
        if self.word.is_empty() && !self.oversized {
            return;
        }
        if !self.oversized {
            let mut word = std::mem::take(&mut self.word);
            if self.word_context.domain == Domain::Code {
                self.tracker.word(&word);
                self.last_code_word = Some(self.word_offset);
            } else if self.word_context.domain == Domain::String {
                self.literal_words += 1;
                self.literal_word.clone_from(&word);
            }
            if word.len() >= 2 && !word.chars().all(char::is_numeric) {
                emit(Event::Word {
                    word: &word,
                    context: self.word_context,
                    offset: self.word_offset,
                });
            }
            word.clear();
            self.word = word;
        } else {
            self.word.clear();
            self.last_code_word = None;
        }
        self.oversized = false;
    }

    fn step(&mut self, emit: &mut impl FnMut(Event<'_>)) {
        let ch = self.pending.pop_front().unwrap();
        let next = self.pending.front().copied();
        let third = self.pending.get(1).copied();
        let offset = self.offset;
        self.offset += 1;
        if self.skip > 0 {
            self.skip -= 1;
            self.previous = ch;
            return;
        }
        let escaped = std::mem::take(&mut self.escaped);
        let domain = self.domain();
        if ch.is_alphanumeric() || matches!(ch, '_' | '$') {
            if self.word.is_empty() && !self.oversized {
                if domain == Domain::Code {
                    self.tracker.start_word(offset);
                }
                self.word_context = self.context();
                self.word_offset = offset;
            }
            if !self.oversized && self.word.len() + ch.len_utf8() <= TOKEN_BYTES {
                self.word.push(ch);
            } else {
                self.word.clear();
                self.oversized = true;
            }
            self.previous = ch;
            return;
        }
        self.finish_word(emit);
        match self.mode {
            Mode::LineComment => {
                if ch == '\n' {
                    self.mode = Mode::Code;
                    self.tracker.symbol(ch, offset);
                }
            }
            Mode::BlockComment(depth) => {
                if ch == '/' && next == Some('*') {
                    self.mode = Mode::BlockComment(depth.saturating_add(1));
                    self.skip = 1;
                } else if ch == '*' && next == Some('/') {
                    self.mode = if depth <= 1 {
                        Mode::Code
                    } else {
                        Mode::BlockComment(depth - 1)
                    };
                    self.skip = 1;
                }
            }
            Mode::Quote(delimiter, triple) => {
                if escaped {
                } else if ch == '\\' {
                    self.escaped = true;
                } else if ch == delimiter && (!triple || (next == Some(ch) && third == Some(ch))) {
                    self.mode = Mode::Code;
                    self.skip = if triple { 2 } else { 0 };
                    if self.literal_words == 1 {
                        self.tracker.literal.clone_from(&self.literal_word);
                    }
                    if !self.tracker.assignment.is_empty() {
                        self.tracker.assignment_value = true;
                    }
                }
            }
            Mode::Code => {
                if ch == '/' && matches!(next, Some('/' | '*')) {
                    self.mode = if next == Some('/') {
                        Mode::LineComment
                    } else {
                        Mode::BlockComment(1)
                    };
                    self.skip = 1;
                    self.last_code_word = None;
                } else if (ch == '#'
                    && !matches!(next, Some('[' | '!'))
                    && !self.directive()
                    && (offset == 0 || self.previous.is_whitespace()))
                    || (ch == '-'
                        && next == Some('-')
                        && (offset == 0 || self.previous.is_whitespace()))
                {
                    self.mode = Mode::LineComment;
                    self.last_code_word = None;
                } else if matches!(ch, '"' | '\'' | '`') {
                    let end = self
                        .pending
                        .iter()
                        .position(|ch| !ch.is_alphanumeric() && *ch != '_');
                    let lifetime = ch == '\''
                        && (matches!(self.previous, '&' | '<')
                            || (next.is_some_and(char::is_alphabetic)
                                && end
                                    .and_then(|end| {
                                        self.pending.iter().skip(end).find(|ch| !ch.is_whitespace())
                                    })
                                    .is_some_and(|c| {
                                        matches!(c, ':' | ',' | '>' | '+' | ')' | ']')
                                    })));
                    if !lifetime {
                        self.tracker.start_word(offset);
                        let triple = next == Some(ch) && third == Some(ch);
                        self.mode = Mode::Quote(ch, triple);
                        self.skip = if triple { 2 } else { 0 };
                        self.literal_word.clear();
                        self.literal_words = 0;
                    }
                    self.last_code_word = None;
                } else {
                    if ch == '('
                        && let Some(offset) = self.last_code_word.take()
                        && !matches!(
                            self.tracker.previous.as_str(),
                            "if" | "for" | "while" | "switch" | "match" | "catch" | "with"
                        )
                    {
                        emit(Event::Call { offset });
                    } else if !ch.is_whitespace() {
                        self.last_code_word = None;
                    }
                    self.tracker.symbol(ch, offset);
                }
            }
        }
        self.previous = ch;
    }

    fn directive(&self) -> bool {
        let head: String = self.pending.iter().take(3).collect();
        [
            "inc", "def", "if", "els", "end", "pra", "eli", "err", "lin", "und", "war",
        ]
        .iter()
        .any(|prefix| head.starts_with(prefix))
    }
}

#[cfg(test)]
#[path = "completion_context_tests.rs"]
mod tests;
