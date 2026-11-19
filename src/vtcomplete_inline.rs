//! Multi-token inline prediction for GPTComplete.
//!
//! The predictor is a variable-order sequence model over lexical tokens of
//! the text the editor already holds. It retrieves earlier positions whose
//! preceding tokens match the caret's context, either literally or under a
//! consistent renaming of identifiers, and lets those witnesses vote on the
//! continuation one token at a time. Renamings learned from the matched
//! context are applied to the copied continuation, including sub-word
//! renamings (`left_margin` -> `right_margin` implies `Left` -> `Right`).
//!
//! No model weights, no network and no subprocess are involved; every
//! request is a bounded scan over at most a few megabytes of token ids.
use rustc_hash::FxHashMap;

#[path = "vtcomplete_inline_model.rs"]
pub(crate) mod model;
#[path = "vtcomplete_inline_subwords.rs"]
pub(crate) mod subwords;

pub(crate) const MAX_DOCUMENT_BYTES: usize = 768 * 1024;
const MAX_CORPUS_BYTES: usize = 4 * 1024 * 1024;
const MAX_CORPUS_DOCUMENTS: usize = 24;
const MAX_SYMBOLS: usize = 400_000;
const MAX_INTERNED_BYTES: usize = 96;

const KIND_SHIFT: u32 = 28;
const SYMBOL_MASK: u32 = (1 << KIND_SHIFT) - 1;
const IDENT: u32 = 0;
const NUMBER: u32 = 1;
const STRING: u32 = 2;
const KEYWORD: u32 = 3;
const PUNCT: u32 = 4;
const NEWLINE: u32 = 5;
const NEWLINE_ID: u32 = NEWLINE << KIND_SHIFT;

const MAX_CONTEXT: usize = 40;
const MAX_EXACT_CANDIDATES: usize = 768;
const MAX_ABSTRACT_CANDIDATES: usize = 768;
const MAX_VOTERS: usize = 96;
const MAX_OUTPUT_TOKENS: usize = 64;
const MAX_OUTPUT_BYTES: usize = 240;
const MAX_OUTPUT_LINES: usize = 8;

/// Whether the neural model writes on into blocks the prediction opens or
/// is inside, with its own indentation (`VT_MULTI=0` turns it off, for
/// measuring).
fn multiline_model() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("VT_MULTI").map_or(true, |value| value != "0"))
}

/// Whether the passage from elsewhere is the analogous place the pattern
/// predictor finds rather than the place sharing most names
/// (`VT_ANALOG=0` turns it off, for measuring).
fn analog_passages() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("VT_ANALOG").map_or(true, |value| value != "0"))
}

fn kind(id: u32) -> u32 {
    id >> KIND_SHIFT
}

/// Identifiers, numbers and strings are parameters of a pattern; every other
/// token is literal structure.
fn class(id: u32) -> u32 {
    if kind(id) <= STRING {
        id & !SYMBOL_MASK
    } else {
        id
    }
}

fn punct(ch: char) -> u32 {
    (PUNCT << KIND_SHIFT) | (ch as u32 & SYMBOL_MASK)
}

/// Whether `text` begins with what was typed. Typing without capitals
/// matches any capitalisation: `layoutr` begins `LayoutRenderers`.
fn begins(text: &str, typed: &str) -> bool {
    text.starts_with(typed)
        || (!typed.chars().any(char::is_uppercase)
            && text.len() >= typed.len()
            && text.is_char_boundary(typed.len())
            && text[..typed.len()].eq_ignore_ascii_case(typed))
}

/// The text before the caret as subword tokens.
#[derive(Default)]
struct Stream {
    key: u64,
    text: String,
    /// The document-start token, then the text's tokens.
    tokens: Vec<u16>,
    /// Byte offset in `text` where each token starts.
    starts: Vec<u32>,
    last_piece: usize,
    cache: subwords::Cache,
}

/// The neural model's choice for the next lexical token.
struct Proposal {
    id: u32,
    text: String,
    /// Whether space stands before it.
    spaced: bool,
    /// At the start of a line, the indentation the model wrote before it.
    indent: Option<String>,
    probability: f32,
}

/// What the neural model wrote ahead from some point of a prediction.
struct Plan {
    /// Length of the predicted text when it was written.
    made_at: usize,
    /// Bytes of the unfinished piece the text ended in then.
    typed: usize,
    /// The typed piece as the model spells it, then what it wrote.
    text: String,
    /// For each token written: where it starts and ends in `text`, and its
    /// probability.
    steps: Vec<(usize, usize, f32)>,
}

impl Plan {
    fn new(made_at: usize, typed: Vec<u8>, written: model::Written, _mark: usize) -> Self {
        // Only whole characters are kept.
        let valid = match std::str::from_utf8(&written.bytes) {
            Ok(text) => text.len(),
            Err(error) => error.valid_up_to(),
        };
        let mut text = String::from_utf8_lossy(&written.bytes[..valid]).into_owned();
        if text.len() < typed.len() {
            text = String::from_utf8_lossy(&typed).into_owned();
        }
        let mut start = 0;
        let steps = written
            .steps
            .iter()
            .map(|&(end, probability)| {
                let step = (start, end, probability);
                start = end;
                step
            })
            .collect();
        Self {
            made_at,
            typed: typed.len(),
            text,
            steps,
        }
    }

    /// What is still to come of the written text when the prediction has
    /// reached `output`, with its place in `text`; None when the prediction
    /// left it.
    fn rest(&self, output: &str) -> Option<(usize, &str)> {
        let followed = output.get(self.made_at..)?;
        let written = self.text.get(self.typed..)?;
        let rest = written.strip_prefix(followed)?;
        (!rest.is_empty()).then_some((self.typed + followed.len(), rest))
    }

    /// Probability of the tokens that write `text[at..end]`. A token is
    /// counted where it starts; the tokens spelling the typed piece count
    /// for the first text after it.
    fn chance(&self, at: usize, end: usize) -> f32 {
        let from = if at <= self.typed { 0 } else { at };
        self.steps
            .iter()
            .filter(|&&(start, ..)| start >= from && start < end)
            .map(|&(.., probability)| probability)
            .product()
    }

    /// Probability of `addition` coming next, when the written text goes on
    /// that way.
    fn chance_of(&self, output: &str, addition: &str) -> Option<f32> {
        let (at, rest) = self.rest(output)?;
        rest.starts_with(addition).then(|| self.chance(at, at + addition.len()))
    }

    /// The next lexical token of the written text. `typed_word` is the
    /// start of a word typed before the caret, which the token continues.
    fn next(&self, output: &str, typed_word: &str, syntax: Syntax, interner: &mut Interner) -> Option<Proposal> {
        let (at, rest) = self.rest(output)?;
        // The typed word as the model spelled it.
        let word = self.text.get(at.checked_sub(typed_word.len())?..at)?;
        if !word.eq_ignore_ascii_case(typed_word) {
            return None;
        }
        // The line ends here.
        let blank = rest.len() - rest.trim_start_matches([' ', '\t', '\r']).len();
        if rest[blank..].starts_with('\n') {
            return typed_word.is_empty().then(|| Proposal {
                id: NEWLINE_ID,
                text: String::new(),
                spaced: false,
                indent: None,
                probability: self.chance(at, at + blank + 1),
            });
        }
        let mut document = Document {
            text: format!("{word}{rest}"),
            ..Document::default()
        };
        lex(&mut document, 0, syntax, interner);
        let id = *document.ids.first()?;
        let (start, end) = (document.starts[0] as usize, document.ends[0] as usize);
        if id != NEWLINE_ID && end <= word.len() {
            return None; // The model ended the typed word where it stands.
        }
        // A token the text stops in the middle of may not be whole.
        if id != NEWLINE_ID && end == document.text.len() && !document.text.ends_with('\n') && kind(id) != PUNCT {
            let whole = self.steps.last().is_some_and(|&(_, end, _)| end > self.text.len());
            if !whole && self.steps.len() >= 40 {
                return None;
            }
        }
        if !typed_word.is_empty() && start != 0 {
            return None;
        }
        let end = at + end.max(word.len()) - word.len();
        let indent = (output.ends_with('\n') && typed_word.is_empty()).then(|| document.text[..start].to_owned());
        Some(Proposal {
            id,
            text: document.token(0).to_owned(),
            spaced: start > 0 && id != NEWLINE_ID,
            indent,
            probability: self.chance(at, end.max(at + 1)),
        })
    }
}

/// The start of the text below the caret line that the neural model reads.
fn passage_start(below: &str) -> &str {
    const BYTES: usize = 3072;
    let mut end = below.len().min(BYTES);
    while !below.is_char_boundary(end) {
        end -= 1;
    }
    &below[..end]
}

fn identifier_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$'
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Syntax {
    slash_comments: bool,
    hash_comments: bool,
    dash_comments: bool,
    plain: bool,
}

impl Syntax {
    /// No comments and no strings: every word and sign is a token, in any
    /// language. This is how the neural model reads a file, so that it can
    /// continue comments, strings and prose, and needs to know nothing about
    /// a language to read it.
    pub(crate) fn plain() -> Self {
        Self {
            plain: true,
            ..Self::default()
        }
    }

    pub(crate) fn for_language(language: &str) -> Self {
        let lower = language.to_ascii_lowercase();
        let hash = [
            "python", "shell", "bash", "ruby", "perl", "yaml", "toml", "makefile", "dockerfile",
            "r", "powershell", "elixir", "nim", "julia", "ini", "properties", "cmake", "tcl",
            "coffeescript", "crystal", "gdscript", "nix", "fish", "zsh", "sh", "awk", "sed",
            "terraform", "hcl", "graphql", "starlark", "bazel", "meson", "just",
        ]
        .contains(&lower.as_str())
            || lower.contains("shell");
        let dash = ["sql", "lua", "haskell", "ada", "elm", "vhdl", "plsql", "purescript"]
            .contains(&lower.as_str());
        Self {
            slash_comments: !hash && !dash,
            hash_comments: hash,
            dash_comments: dash,
            plain: false,
        }
    }
}

/// The language families the neural model tells apart; zero is any other.
pub(crate) fn language_id(language: &str) -> u8 {
    let lower = language.to_ascii_lowercase();
    let is = |names: &[&str]| names.contains(&lower.as_str());
    if is(&["rust"]) {
        1
    } else if is(&["python"]) {
        2
    } else if is(&["c", "objective-c"]) {
        3
    } else if is(&["c++", "cpp"]) {
        4
    } else if lower.starts_with("javascript") || is(&["jsx"]) {
        5
    } else if lower.starts_with("typescript") || is(&["tsx"]) {
        6
    } else if is(&["go"]) {
        7
    } else if is(&["java"]) {
        8
    } else if is(&["kotlin"]) {
        9
    } else if is(&["c#", "csharp"]) {
        10
    } else if is(&["ruby"]) {
        11
    } else if is(&["php"]) {
        12
    } else if is(&["swift"]) {
        13
    } else if is(&["elixir"]) {
        14
    } else if is(&["scala"]) {
        15
    } else if is(&["lua"]) {
        16
    } else if lower.contains("shell") || is(&["bash", "sh", "zsh"]) {
        17
    } else if is(&["dart"]) {
        18
    } else if lower.contains("markdown") || is(&["plain text", "text", "restructuredtext", "asciidoc"]) {
        19
    } else if is(&["html", "xml", "vue", "svelte"]) {
        20
    } else if is(&["css", "scss", "less", "sass"]) {
        21
    } else if lower.contains("sql") {
        22
    } else if lower.starts_with("json") || is(&["yaml", "toml", "ini"]) {
        23
    } else {
        0
    }
}

const KEYWORDS: &[&str] = &[
    "abstract", "and", "as", "assert", "async", "await", "begin", "break", "case", "catch",
    "class", "const", "continue", "crate", "def", "default", "defer", "del", "delete", "do",
    "dyn", "elif", "else", "end", "enum", "except", "export", "extends", "extern", "final",
    "finally", "fn", "for", "foreach", "from", "func", "function", "global", "go", "goto", "if",
    "impl", "implements", "import", "in", "include", "instanceof", "interface", "is", "lambda",
    "let", "local", "loop", "match", "mod", "module", "move", "mut", "namespace", "new", "not",
    "of", "or", "override", "package", "pass", "private", "protected", "pub", "public", "raise",
    "ref", "require", "return", "select", "static", "struct", "super", "switch", "template",
    "then", "throw", "throws", "trait", "try", "type", "typedef", "typeof", "union", "unless",
    "unsafe", "until", "use", "using", "var", "virtual", "void", "when", "where", "while",
    "with", "yield",
];

#[derive(Default)]
struct Interner {
    ids: FxHashMap<Box<str>, u32>,
    names: Vec<Box<str>>,
    keyword: Vec<bool>,
}

impl Interner {
    fn intern(&mut self, text: &str) -> u32 {
        if let Some(&id) = self.ids.get(text) {
            return id;
        }
        let id = self.names.len() as u32 + 1;
        self.names.push(text.into());
        self.keyword.push(KEYWORDS.binary_search(&text).is_ok());
        self.ids.insert(text.into(), id);
        id
    }

    fn name(&self, id: u32) -> &str {
        let symbol = id & SYMBOL_MASK;
        if symbol == 0 {
            ""
        } else {
            &self.names[symbol as usize - 1]
        }
    }

    fn word(&mut self, text: &str) -> u32 {
        let symbol = self.intern(text);
        let keyword = self.keyword[symbol as usize - 1];
        (if keyword { KEYWORD } else { IDENT } << KIND_SHIFT) | symbol
    }
}

/// One lexed text. Offsets address `text`; the gap before a token is the
/// source between the previous token's end and this token's start.
#[derive(Default)]
struct Document {
    key: u64,
    text: String,
    ids: Vec<u32>,
    starts: Vec<u32>,
    ends: Vec<u32>,
    /// Where the lexer entered or left a comment or string spanning lines:
    /// byte offset and the mode from there on. Lexing restarts from these.
    modes: Vec<(u32, u8)>,
    used: u64,
}

impl Document {
    fn token(&self, at: usize) -> &str {
        &self.text[self.starts[at] as usize..self.ends[at] as usize]
    }

    fn gap(&self, at: usize) -> &str {
        let from = if at == 0 { 0 } else { self.ends[at - 1] as usize };
        &self.text[from..self.starts[at] as usize]
    }

    fn truncate_tokens(&mut self, byte: usize) {
        let keep = self.ends.partition_point(|&end| end as usize <= byte);
        self.ids.truncate(keep);
        self.starts.truncate(keep);
        self.ends.truncate(keep);
    }

    /// Indentation of the line holding token `at`.
    fn indent(&self, at: usize) -> &str {
        let position = if at < self.starts.len() {
            self.starts[at] as usize
        } else {
            self.text.len()
        };
        let line = self.text[..position].rfind('\n').map_or(0, |i| i + 1);
        let end = self.text[line..]
            .find(|ch: char| ch != ' ' && ch != '\t')
            .map_or(self.text.len(), |i| line + i);
        &self.text[line..end]
    }
}

/// Line-local lexer: strings and comments never span a line, so lexing can
/// restart at any line boundary after an edit.
fn lex(document: &mut Document, from: usize, syntax: Syntax, interner: &mut Interner) {
    let Document {
        text,
        ids,
        starts,
        ends,
        modes,
        ..
    } = document;
    let bytes = text.as_bytes();
    let mut at = from;
    let mut line_start = true;
    // 0 code, 1 block comment, 2 and 3 triple-quoted strings.
    modes.retain(|&(offset, _)| (offset as usize) <= from);
    let mut mode = modes.last().map_or(0, |&(_, mode)| mode);
    while at < bytes.len() {
        let byte = bytes[at];
        if byte == b'\n' || byte == b'\r' {
            let start = at;
            while at < bytes.len() && (bytes[at] as char).is_ascii_whitespace() {
                // Blank lines collapse into one separator token.
                if bytes[at] == b'\n' || bytes[at] == b'\r' {
                    at += 1;
                } else {
                    let mut probe = at;
                    while probe < bytes.len() && (bytes[probe] == b' ' || bytes[probe] == b'\t') {
                        probe += 1;
                    }
                    if probe < bytes.len() && (bytes[probe] == b'\n' || bytes[probe] == b'\r') {
                        at = probe;
                    } else {
                        break;
                    }
                }
            }
            if ids.last() != Some(&NEWLINE_ID) && !ids.is_empty() {
                ids.push(NEWLINE_ID);
                starts.push(start as u32);
                ends.push((start + 1) as u32);
            }
            line_start = true;
            continue;
        }
        if byte == b' ' || byte == b'\t' {
            at += 1;
            continue;
        }
        let rest = &bytes[at..];
        if mode != 0 {
            let close: &[u8] = match mode {
                1 => b"*/",
                2 => b"\"\"\"",
                _ => b"'''",
            };
            match find_bytes(&rest[..line_length(rest)], close) {
                Some(end) => {
                    at += end + close.len();
                    if mode != 1 {
                        // The whole string is one opaque token.
                        ids.push(STRING << KIND_SHIFT);
                        starts.push((at - close.len()) as u32);
                        ends.push(at as u32);
                        line_start = false;
                    }
                    mode = 0;
                    modes.push((at as u32, 0));
                }
                None => at += line_length(rest),
            }
            continue;
        }
        if syntax.hash_comments && (rest.starts_with(b"\"\"\"") || rest.starts_with(b"'''")) {
            let close = &rest[..3];
            let line = &rest[3..line_length(rest)];
            if let Some(end) = find_bytes(line, close) {
                ids.push(STRING << KIND_SHIFT);
                starts.push(at as u32);
                at += 3 + end + 3;
                ends.push(at as u32);
                line_start = false;
            } else {
                mode = if byte == b'"' { 2 } else { 3 };
                at += 3;
                modes.push((at as u32, mode));
            }
            continue;
        }
        let comment = (syntax.slash_comments && rest.starts_with(b"//"))
            || (syntax.hash_comments && byte == b'#')
            || (syntax.dash_comments && rest.starts_with(b"--"))
            || (line_start
                && !syntax.plain
                && !syntax.hash_comments
                && byte == b'#'
                && rest.get(1).is_some_and(|&next| next == b' ' || next == b'!'));
        if comment {
            at += line_length(rest);
            continue;
        }
        if syntax.slash_comments && rest.starts_with(b"/*") {
            mode = 1;
            at += 2;
            modes.push((at as u32, 1));
            continue;
        }
        line_start = false;
        let ch = text[at..].chars().next().unwrap();
        let start = at;
        let id = if identifier_char(ch) {
            let length = text[at..]
                .find(|ch: char| !identifier_char(ch))
                .unwrap_or(text.len() - at);
            at += length;
            let word = &text[start..at];
            if length > MAX_INTERNED_BYTES || interner.names.len() >= MAX_SYMBOLS {
                IDENT << KIND_SHIFT
            } else if ch.is_ascii_digit() {
                (NUMBER << KIND_SHIFT) | interner.intern(word)
            } else {
                interner.word(word)
            }
        } else if !syntax.plain && matches!(byte, b'"' | b'\'' | b'`') {
            // A quote without a partner on this line is ordinary punctuation
            // (lifetimes, apostrophes, unfinished typing).
            let mut end = None;
            let mut probe = at + 1;
            while probe < bytes.len() && bytes[probe] != b'\n' && bytes[probe] != b'\r' {
                if bytes[probe] == b'\\' {
                    probe += 2;
                    continue;
                }
                if bytes[probe] == byte {
                    end = Some(probe + 1);
                    break;
                }
                probe += 1;
            }
            match end {
                Some(end) if byte != b'\'' || end - at <= 64 => {
                    at = end;
                    let literal = &text[start..at];
                    if literal.len() > MAX_INTERNED_BYTES || interner.names.len() >= MAX_SYMBOLS {
                        STRING << KIND_SHIFT
                    } else {
                        (STRING << KIND_SHIFT) | interner.intern(literal)
                    }
                }
                _ => {
                    at += 1;
                    punct(ch)
                }
            }
        } else if let Some(&operator) = OPERATORS
            .iter()
            .chain(if syntax.plain { PLAIN_OPERATORS } else { &[] })
            .find(|operator| rest.starts_with(operator.as_bytes()))
        {
            at += 2;
            let bytes = operator.as_bytes();
            (PUNCT << KIND_SHIFT) | 0x0800_0000 | (bytes[0] as u32) << 8 | bytes[1] as u32
        } else {
            at += ch.len_utf8();
            punct(ch)
        };
        ids.push(id);
        starts.push(start as u32);
        ends.push(at as u32);
    }
}

/// Operators lexed as one token. Shifts are left apart: `>>` also closes
/// nested generics.
const OPERATORS: &[&str] = &[
    "::", "->", "=>", "==", "!=", "<=", ">=", "&&", "||", "+=", "-=", "*=", "/=", "..", "?.",
    ":=", "|>", "??",
];

/// Further pairs lexed as one token when reading plainly, where comment
/// signs are tokens too.
const PLAIN_OPERATORS: &[&str] = &["//", "/*", "*/", "--", "++", "**", "<<", ">>", "</", "/>", "<-"];

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn line_length(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .position(|&byte| byte == b'\n' || byte == b'\r')
        .unwrap_or(bytes.len())
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Prediction {
    /// Text to insert at the caret.
    pub(crate) text: String,
    /// Estimated probability that the first predicted token is right.
    pub(crate) confidence: f32,
    /// Number of lexical tokens in `text`, counting a completed prefix.
    pub(crate) tokens: usize,
    /// The typed beginning of the first token as the prediction spells it,
    /// when that differs from the typing in capitalisation.
    pub(crate) retype: Option<String>,
    pub(crate) trace: Vec<Step>,
}

/// Evidence behind one predicted token.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Step {
    /// Byte length of the prediction through this token.
    pub(crate) end: usize,
    pub(crate) share: f32,
    pub(crate) supporters: usize,
    /// Best aligned-context score among the supporting witnesses.
    pub(crate) score: f32,
    pub(crate) kind: u32,
    pub(crate) line_start: bool,
    pub(crate) lines: usize,
    pub(crate) renamed: bool,
    /// Characters of this token the user had already typed.
    pub(crate) typed: usize,
    /// Position of the token in the prediction.
    pub(crate) index: usize,
    /// Distinct proposals among the witnesses.
    pub(crate) groups: usize,
    /// The best supporting witness is in the text above the caret.
    pub(crate) local: bool,
    /// An identifier the active document never mentions.
    pub(crate) foreign: bool,
    pub(crate) nested: bool,
    /// How probable the neural model finds this token: 0 when there is no
    /// model, otherwise 1 (under 5%) to 13 (99% and more).
    pub(crate) neural: u8,
    /// Inside a comment or a string, where only the neural model predicts.
    pub(crate) prose: bool,
    /// A name that exists nowhere yet, put together from parts.
    pub(crate) coined: bool,
    /// The next step of a series the lines above count through.
    pub(crate) series: bool,
    /// Proposed by the neural model alone, without a witness in the text.
    pub(crate) model: bool,
    /// Estimated probability that this token is right, given the ones before.
    pub(crate) estimate: f32,
}

pub(crate) const FEATURES: usize = 158;

fn neural_level(probability: f32) -> u8 {
    1 + [0.05, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 0.95, 0.99]
        .iter()
        .filter(|&&edge| probability >= edge)
        .count() as u8
}

impl Step {
    /// Active one-hot features of the calibration model.
    pub(crate) fn features(&self) -> [usize; 17] {
        let kind = (self.kind as usize).min(5);
        if self.model {
            let mut features = [94; 17];
            features[0] = 108 + kind;
            features[1] = 116 + (self.neural as usize).clamp(1, 13);
            features[2] = match self.typed {
                0 => 94,
                1 | 2 => 114,
                _ => 115,
            };
            if self.line_start {
                features[3] = 53;
            }
            if self.nested {
                features[4] = 57;
            }
            if self.prose {
                features[5] = 116;
            }
            if self.coined {
                features[6] = 144;
            }
            return features;
        }
        let supporters = match self.supporters {
            0 | 1 => 0,
            2 => 1,
            3 | 4 => 2,
            _ => 3,
        };
        let share = [0.3, 0.5, 0.7, 0.85, 0.95]
            .iter()
            .filter(|&&edge| self.share >= edge)
            .count();
        let score = [4., 7., 10., 14., 20., 28.]
            .iter()
            .filter(|&&edge| self.score >= edge)
            .count();
        let step = match self.index {
            0..=2 => self.index,
            3 | 4 => 3,
            5..=8 => 4,
            _ => 5,
        };
        let groups = match self.groups {
            0 | 1 => 0,
            2 => 1,
            3 | 4 => 2,
            _ => 3,
        };
        // How firmly the text itself points at this token, crossed with what
        // the model makes of it: a long exact match that nothing else in
        // the text contradicts stands even when the model would not have
        // thought of it, while a loose match needs the model's support.
        let firm = match (self.share, self.score) {
            (share, score) if share >= 0.95 && score >= 28. => 2,
            (share, score) if share >= 0.85 && score >= 10. => 1,
            _ => 0,
        };
        let view = match self.neural {
            0 => 0,
            1 | 2 => 1,
            3..=6 => 2,
            _ => 3,
        };
        // Absent flags share the always-zero slot 94.
        let flag = |on: bool, slot: usize| if on { 52 + slot } else { 94 };
        [
            kind * 4 + supporters,
            24 + share,
            30 + score,
            // Four or more typed characters count as three: the replay types three.
            37 + self.typed.min(3),
            42 + step,
            48 + groups,
            flag(self.renamed, 0),
            flag(self.line_start, 1),
            flag(self.lines > 0, 2),
            flag(self.local, 3),
            flag(self.foreign, 4),
            flag(self.nested, 5),
            flag(self.typed > 0 && self.supporters == 1, 6),
            59 + kind * 6 + share.min(5),
            130 + (self.neural as usize).min(13),
            if self.series { 145 } else { 94 },
            146 + firm * 4 + view,
        ]
    }
}

// BEGIN FITTED WEIGHTS
#[rustfmt::skip]
#[allow(clippy::approx_constant)] // Fitted numbers; any likeness to a constant is chance.
const WEIGHTS: [f32; FEATURES + 1] = [-0.2374, -0.0883, 0.0278, 0.0683, -1.0462, 0.3448, -0.7906, -0.1451, -0.6083, -0.0275, 0.0153, -0.0122, -0.4600, -0.6525, 0.0083, -0.0576, -0.0761, -0.0335, 0.5029, 0.0768, 0.7187, 0.4981, 0.9508, 1.9120, -0.2784, -0.0279, -0.1479, 0.2162, 0.2363, 0.8904, 0.0238, 0.1509, 0.0127, 0.3441, 0.0697, 0.2617, 0.0257, -0.0811, 0.2996, -0.0140, 0.6841, 0.0000, 0.2990, 0.0355, -0.0043, 0.1580, -0.0635, 0.4639, 1.2194, -0.1794, -0.0920, -0.0595, -0.9567, -0.1137, 0.3798, -0.6475, -0.3000, -0.0953, -0.5488, -0.3364, 0.0432, 0.2077, 0.0943, -0.0241, -0.2143, 0.0637, -0.5500, -0.4471, 0.0435, 0.0145, -0.7616, -0.5685, 0.1599, -0.0854, 0.1349, 0.1201, -0.3938, -0.1286, -0.1670, -0.1466, -0.5197, -0.3249, 0.1251, -0.4682, -0.3062, 0.1510, 0.3710, 0.2316, 0.4907, 1.1596, 0.7920, 0.1725, 0.0921, 0.2193, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, -0.2501, -0.4929, -0.0626, -0.0763, -0.0065, 0.0000, -0.0219, 0.3053, 0.2659, -3.2907, -2.4767, -2.1018, -1.5146, -0.9757, -0.5652, -0.2353, 0.0954, 1.1295, 1.5716, 2.1960, 2.7277, 2.5513, 0.3302, -1.4076, -0.5989, -0.7985, -0.5823, 0.0474, 0.3958, -0.9653, -0.3218, 0.0599, 0.4622, 1.2238, 2.0540, 0.9897, 0.0000, -0.3099, 0.5757, -1.5407, -0.4400, 1.1063, 0.3424, -0.2791, -0.6593, 1.0935, -0.5879, -0.1867, 0.1618, 1.3028, 0.8969];
// END FITTED WEIGHTS

/// The same, fitted to the pattern predictor working without the language
/// model (`--table WEIGHTS_ALONE`).
// BEGIN FITTED WEIGHTS ALONE
#[rustfmt::skip]
#[allow(clippy::approx_constant)] // Fitted numbers; any likeness to a constant is chance.
const WEIGHTS_ALONE: [f32; FEATURES + 1] = [-1.0291, -0.2133, -0.0662, 0.4974, -0.9979, -0.7900, -0.1144, 0.1047, -1.4361, -0.5951, -0.3441, -0.1189, -0.4679, -0.0874, 0.0109, 0.4601, -0.1739, 0.0257, 0.0759, 0.3308, 0.7023, 0.6205, 1.1621, 2.4438, -1.1293, -0.5208, -0.1495, 0.3811, 0.4102, 1.0084, -0.5088, -0.1223, 0.2446, 0.2922, 0.0857, 0.0278, -0.0192, -0.9036, -0.5695, 0.6282, 0.8449, 0.0000, -0.0258, -0.1782, -0.0707, 0.1482, 0.0141, 0.1125, 1.1834, -0.2292, -0.2425, -0.7117, -1.8286, -0.3427, 0.0596, 0.0274, -0.7961, 0.1803, 0.1939, -0.5318, -0.3109, 0.0134, 0.0599, 0.1276, -0.1693, -0.1607, -0.1769, -0.3593, -0.1332, -0.2576, -0.7098, -0.6555, -0.2850, -0.4325, 0.0729, -0.4580, -0.7362, -0.0148, -0.1036, -0.0010, -0.2010, 0.2421, -0.0059, -0.5801, -0.3362, -0.1083, 0.2315, 0.2992, 0.7524, 0.8136, 0.6918, 0.7383, 0.3510, 0.4568, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 0.0000, 1.6977, -0.3864, 0.0000, 0.0000, 0.0000, -0.1470, 0.0000, 0.0000, 0.0000, 0.5334, 0.0000, 0.0000, 0.0000, 1.9434];
// END FITTED WEIGHTS ALONE

/// Probability that a voted token is right, from a logistic model fitted to
/// replayed source files (`scripts/fit_vtcomplete_inline.py`). Patterns
/// `alone`, without the language model to check them, are trusted less.
fn calibrated(step: &Step, alone: bool) -> f32 {
    let weights = if alone { &WEIGHTS_ALONE } else { &WEIGHTS };
    let mut sum = weights[FEATURES];
    for feature in step.features() {
        sum += weights[feature];
    }
    1. / (1. + (-sum).exp())
}

/// The active document around the caret.
#[derive(Clone, Copy)]
pub(crate) struct Caret<'a> {
    /// Identifies the document, so that typing only lexes changed lines.
    pub(crate) key: u64,
    /// The document up to the caret.
    pub(crate) before: &'a str,
    /// The remainder of the caret line.
    pub(crate) rest: &'a str,
    /// Everything after the caret line.
    pub(crate) below: &'a str,
    pub(crate) syntax: Syntax,
    /// `language_id` of the document.
    pub(crate) language: u8,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Options {
    /// Minimum estimated probability for the first token.
    pub(crate) first: f32,
    /// Minimum estimated probability for each following token.
    pub(crate) next: f32,
    /// Minimum estimated probability to continue past a line end.
    pub(crate) block: f32,
    /// Minimum estimated probability that the whole prediction is right.
    pub(crate) floor: f32,
    pub(crate) multiline: bool,
    /// Longest prediction in tokens.
    pub(crate) tokens: usize,
    /// Time after which no further evidence is retrieved.
    pub(crate) budget: std::time::Duration,
}

/// Confidence needed to show a prediction, from most to least cautious:
/// first token, later tokens, a further line, and the whole prediction.
/// Measured on held-out projects at 3 typed characters, these show a
/// suggestion on about 62-72% / 75-83% / 80-88% / 85-95% of lines, and the
/// suggestions are fully right about 82% / 71% / 62% / 55% of the time.
const PRESETS: [(f32, f32, f32, f32); 4] = [
    (0.6, 0.6, 0.8, 0.45),
    // The model's memory and the text below make its tokens more often
    // right; Balanced spends that on showing more and longer suggestions.
    (0.3, 0.4, 0.65, 0.15),
    (0.25, 0.35, 0.6, 0.12),
    (0.1, 0.2, 0.5, 0.04),
];

impl Options {
    /// Thresholds for eagerness `level`: 0 cautious, 1 balanced, 2 eager,
    /// 3 always (anything above is the same as 3).
    pub(crate) fn preset(level: usize) -> Self {
        let (first, next, block, floor) = PRESETS[level.min(PRESETS.len() - 1)];
        Self {
            first,
            next,
            block,
            floor,
            multiline: true,
            tokens: MAX_OUTPUT_TOKENS,
            budget: std::time::Duration::from_millis(160),
        }
    }
}

impl Default for Options {
    fn default() -> Self {
        Self::preset(1)
    }
}

impl Options {
    /// For a prediction the user asked for: show the best guess even when
    /// it is far from certain, and never be more cautious than `usual`.
    pub(crate) fn requested(usual: Self) -> Self {
        Self {
            // The first token is shown whatever its chance: the user asked.
            first: 0.,
            next: usual.next.min(0.35),
            block: usual.block.min(0.6),
            floor: 0.,
            ..usual
        }
    }
}

#[derive(Clone)]
struct Candidate {
    document: u32,
    position: u32,
    score: f32,
    /// Source identifier -> query identifier, for identifiers that differ.
    renames: Vec<(u32, u32)>,
    /// Token whose line corresponds to the line being predicted when this
    /// witness joined, and that line's indentation relative to the caret line.
    base: u32,
    relative: String,
}

pub(crate) struct Engine<'a> {
    interner: Interner,
    /// `documents[0]` is the text before the caret, `documents[1]` the text
    /// below the caret line. Other entries are reference documents.
    documents: Vec<Document>,
    active_key: u64,
    clock: u64,
    counts: FxHashMap<u32, u32>,
    subwords: Subwords,
    network: Option<&'a model::Network<'a>>,
    session: model::Session,
    /// The text before the caret as the neural model reads it.
    stream: Stream,
    /// What the neural model reads before its window: a passage from
    /// elsewhere and the text below the caret. They are chosen when a window
    /// is started and kept while it is typed in.
    passages: model::Passages,
    /// The start of the text below the caret that `passages` was made from.
    passage_below: String,
    /// Tokens before the caret when `passages` was chosen.
    passage_at: usize,
    /// The line being predicted has already taken a step of a named series
    /// (`item3` after `item1`, `item2`), so bare numbers and letters on it
    /// may count on too.
    counting: bool,
    /// The token the latest prediction stopped before for being too
    /// uncertain, with the evidence for it. For the benchmark's reports.
    pub(crate) stopped: Option<(String, Step)>,
    /// While recording: what the model first wrote at the caret in the
    /// latest prediction, after this many bytes of the typed piece.
    pub(crate) first_written: Option<(usize, model::Written)>,
    #[cfg(test)]
    pub(crate) lexed_bytes: usize,
}

impl<'a> Engine<'a> {
    pub(crate) fn new() -> Self {
        Self {
            interner: Interner::default(),
            documents: vec![Document::default(), Document::default()],
            active_key: 0,
            clock: 0,
            counts: FxHashMap::default(),
            subwords: Subwords::default(),
            // Test builds are unoptimized, where running the network in every
            // typing test would starve the shared workers; its own tests use it.
            network: if cfg!(test) { None } else { model::embedded() },
            session: model::Session::default(),
            stream: Stream::default(),
            passages: model::Passages::default(),
            passage_below: String::new(),
            passage_at: 0,
            counting: false,
            stopped: None,
            first_written: None,
            #[cfg(test)]
            lexed_bytes: 0,
        }
    }

    /// Replace the neural model; None leaves only the pattern predictor.
    /// Used by tests and by the replay benchmark.
    #[allow(dead_code)]
    pub(crate) fn set_network(&mut self, network: Option<&'a model::Network<'a>>) {
        self.network = network;
    }

    fn reset_if_full(&mut self) {
        if self.interner.names.len() >= MAX_SYMBOLS {
            // Symbols are renumbered: nothing cached about them survives.
            let network = self.network;
            *self = Self::new();
            self.network = network;
        }
    }

    #[allow(dead_code)] // Used by the standalone replay benchmark.
    pub(crate) fn has_reference(&self, key: u64) -> bool {
        self.documents[2..].iter().any(|document| document.key == key)
    }

    /// Add or replace the reference document identified by `key`.
    #[allow(dead_code)] // Used by the standalone replay benchmark.
    pub(crate) fn set_reference(&mut self, key: u64, text: &str, syntax: Syntax) {
        self.remove_reference(key);
        if text.len() > MAX_DOCUMENT_BYTES {
            return;
        }
        self.reset_if_full();
        self.clock += 1;
        let mut document = Document {
            key,
            text: text.to_owned(),
            used: self.clock,
            ..Document::default()
        };
        lex(&mut document, 0, syntax, &mut self.interner);
        self.documents.push(document);
        self.evict();
    }

    #[allow(dead_code)] // Used by the standalone replay benchmark.
    pub(crate) fn touch_reference(&mut self, key: u64) {
        self.clock += 1;
        let clock = self.clock;
        if let Some(existing) = self.documents[2..]
            .iter_mut()
            .find(|document| document.key == key)
        {
            existing.used = clock;
        }
    }

    pub(crate) fn remove_reference(&mut self, key: u64) {
        if let Some(index) = self.documents[2..]
            .iter()
            .position(|document| document.key == key)
        {
            self.documents.remove(index + 2);
        }
    }

    fn evict(&mut self) {
        loop {
            let references = &self.documents[2..];
            let bytes: usize = references.iter().map(|document| document.text.len()).sum();
            if references.len() <= MAX_CORPUS_DOCUMENTS && bytes <= MAX_CORPUS_BYTES {
                return;
            }
            let oldest = references
                .iter()
                .enumerate()
                .min_by_key(|(_, document)| document.used)
                .map(|(index, _)| index + 2)
                .unwrap();
            self.documents.remove(oldest);
        }
    }

    /// Refresh the active document. Only the changed tail is lexed again.
    fn set_active(&mut self, key: u64, before: &str, below: &str, syntax: Syntax) {
        self.reset_if_full();
        if self.active_key != key && self.documents.len() == 2 {
            self.interner = Interner::default();
            self.documents.fill_with(Document::default);
            self.counts = FxHashMap::default();
        }
        let Self {
            documents,
            interner,
            counts,
            ..
        } = self;
        let head = &mut documents[0];
        let common = if self.active_key == key {
            head.text
                .as_bytes()
                .iter()
                .zip(before.as_bytes())
                .take_while(|(a, b)| a == b)
                .count()
        } else {
            0
        };
        // Restart at the start of the first changed line.
        let restart = before.as_bytes()[..common]
            .iter()
            .rposition(|&byte| byte == b'\n')
            .unwrap_or(0);
        let keep = head.ends.partition_point(|&end| end as usize <= restart);
        for &id in &head.ids[keep..] {
            if kind(id) == IDENT
                && let Some(count) = counts.get_mut(&id)
            {
                *count = count.saturating_sub(1);
            }
        }
        if keep == 0 {
            counts.retain(|_, count| *count != 0);
        }
        head.truncate_tokens(restart);
        head.text.truncate(restart);
        head.text.push_str(&before[restart..]);
        #[cfg(test)]
        {
            self.lexed_bytes += before.len() - restart;
        }
        lex(head, restart, syntax, interner);
        for &id in &head.ids[keep..] {
            if kind(id) == IDENT {
                *counts.entry(id).or_default() += 1;
            }
        }
        head.key = key;
        self.active_key = key;
        let tail = &mut documents[1];
        if tail.key != key || tail.text != below {
            for &id in &tail.ids {
                if kind(id) == IDENT
                    && let Some(count) = counts.get_mut(&id)
                {
                    *count = count.saturating_sub(1);
                }
            }
            *tail = Document {
                key,
                text: below.to_owned(),
                ..Document::default()
            };
            lex(tail, 0, syntax, interner);
            for &id in &tail.ids {
                if kind(id) == IDENT {
                    *counts.entry(id).or_default() += 1;
                }
            }
        }
    }

    /// Refresh the neural model's reading of the text before the caret.
    /// Only the changed tail is read again.
    fn set_stream(&mut self, network: &model::Network<'_>, key: u64, before: &str) {
        let stream = &mut self.stream;
        let common = if stream.key == key {
            stream
                .text
                .as_bytes()
                .iter()
                .zip(before.as_bytes())
                .take_while(|(a, b)| a == b)
                .count()
        } else {
            stream.cache = subwords::Cache::default();
            network.reset_context(&mut self.session);
            0
        };
        // A line end starts a piece: reading restarts at the last one before
        // the change.
        let restart = before.as_bytes()[..common]
            .iter()
            .rposition(|&byte| byte == b'\n')
            .unwrap_or(0);
        if stream.tokens.is_empty() || restart == 0 {
            stream.tokens.clear();
            stream.starts.clear();
            stream.tokens.push(network.subwords().document());
            stream.starts.push(0);
        } else {
            let keep = stream.starts.partition_point(|&start| (start as usize) < restart).max(1);
            stream.tokens.truncate(keep);
            stream.starts.truncate(keep);
        }
        stream.text.truncate(restart);
        stream.text.push_str(&before[restart..]);
        let held = stream.tokens.len();
        stream.last_piece = restart + network
            .subwords()
            .encode(&before[restart..], &mut stream.cache, &mut stream.tokens, &mut stream.starts);
        for start in &mut stream.starts[held..] {
            *start += restart as u32;
        }
        stream.key = key;
    }

    /// The token of the document where the neural model's window starts for
    /// the caret, with the passages chosen when a new window is started.
    /// With `renew`, reading ahead while the caret rests, the passage from
    /// elsewhere is chosen again once the caret has gone on some way: the
    /// place most like the caret's context changes as code is written.
    fn open_window(&mut self, network: &model::Network<'_>, below: &str, line_ends: bool, renew: bool) -> usize {
        const RENEW: usize = 96;
        let count = self.model_count();
        // Text below that differs from what the window was started above
        // (the caret moved, or the lines below were edited) starts a new
        // window, like a caret outside the old one.
        let below_start = passage_start(below);
        let fresh = !network.continues(&self.session, count) || self.passage_below != below_start;
        if !fresh {
            let window = network.start(&self.session, count);
            if renew && analog_passages() && count >= self.passage_at + RENEW {
                // Read again only if the passage changes.
                self.passage_at = count;
                self.passages = self.read_passages(network, window, line_ends);
            }
            return window;
        }
        let window = network.fresh_start(count);
        self.passage_below = below_start.to_owned();
        self.passage_at = count;
        self.passages = self.read_passages(network, window, line_ends);
        window
    }

    /// Let the neural model read the surroundings of the caret ahead of a
    /// request, so that the first prediction there need not wait for it.
    pub(crate) fn warm(&mut self, caret: Caret<'_>, cancelled: &dyn Fn() -> bool) {
        let Caret {
            key,
            before,
            rest,
            below,
            syntax,
            language,
        } = caret;
        let Some(network) = self.network else { return };
        if before.len() > MAX_DOCUMENT_BYTES || below.len() > MAX_DOCUMENT_BYTES {
            return;
        }
        let partial_start = before
            .char_indices()
            .rev()
            .take_while(|&(_, ch)| identifier_char(ch))
            .last()
            .map_or(before.len(), |(at, _)| at);
        self.set_active(key, &before[..partial_start], below, syntax);
        self.set_stream(network, key, before);
        let window = self.open_window(network, below, rest.is_empty(), true);
        let (tokens, _) = self.model_state(network, window, "");
        network.read(&mut self.session, &self.passages, &tokens, window, language, cancelled);
    }

    /// What the neural model reads to continue `output` after the text
    /// before the caret: the tokens from token `from` of the document up to
    /// the last, unfinished piece, and that piece's bytes.
    fn model_state(&mut self, network: &model::Network<'_>, from: usize, output: &str) -> (Vec<u16>, Vec<u8>) {
        if output.is_empty() {
            let end = self.model_count();
            return (
                self.stream.tokens[from.min(end)..end].to_vec(),
                self.stream.text.as_bytes()[self.stream.last_piece..].to_vec(),
            );
        }
        let stream = &mut self.stream;
        let line = stream.text.rfind('\n').unwrap_or(0);
        let kept = stream.starts.partition_point(|&start| (start as usize) < line).max(1);
        let tail = format!("{}{output}", &stream.text[line..]);
        let last = subwords::pieces(&tail).last().map_or(0, |&(start, _)| start);
        let mut tokens = stream.tokens[from.min(kept)..kept].to_vec();
        network
            .subwords()
            .encode(&tail[..last], &mut stream.cache, &mut tokens, &mut Vec::new());
        // The model window can begin partway through the retokenized line.
        let skip = from.saturating_sub(kept).min(tokens.len());
        tokens.drain(..skip);
        (tokens, tail.as_bytes()[last..].to_vec())
    }

    /// Tokens before the caret's last, unfinished piece.
    fn model_count(&self) -> usize {
        let stream = &self.stream;
        stream.starts.partition_point(|&start| (start as usize) < stream.last_piece).max(1)
    }

    /// The place outside the model's window most like the caret's context:
    /// the pattern predictor's best witness above the window, further below
    /// than the text below that is read, or in a reference document. As
    /// document index and the bytes of a passage that starts a little
    /// before it and holds what followed it.
    fn analog_place(&mut self, window_byte: usize) -> Option<(usize, usize, usize)> {
        const BEFORE: usize = 10;
        const BYTES: usize = 1200;
        let ids = &self.documents[0].ids;
        if ids.len() < 4 {
            return None;
        }
        let context = ids[ids.len().saturating_sub(MAX_CONTEXT)..].to_vec();
        let candidates = self.retrieve(&context, &|| false);
        // Renamings learned on the way belong to a prediction, not here.
        self.subwords.clear();
        for candidate in candidates? {
            // A few tokens matched, at least.
            if candidate.score < 6. {
                break;
            }
            let index = candidate.document as usize;
            let document = &self.documents[index];
            let position = candidate.position as usize;
            if position >= document.ids.len() {
                continue;
            }
            let byte = document.starts[position] as usize;
            let outside = match index {
                0 => byte < window_byte,
                1 => byte > self.passage_below.len(),
                _ => true,
            };
            if !outside {
                continue;
            }
            let first = document.starts[position.saturating_sub(BEFORE)] as usize;
            let start = document.text[..first].rfind('\n').map_or(0, |at| at + 1);
            let mut end = (start + BYTES).min(document.text.len());
            if index == 0 {
                end = end.min(window_byte);
            }
            while !document.text.is_char_boundary(end) {
                end -= 1;
            }
            if end > byte {
                return Some((index, start, end));
            }
        }
        None
    }

    /// The passages for a window starting at token `from`: the text below
    /// the caret, and the place elsewhere (above the window, further below,
    /// or in a reference document) most like the caret's surroundings.
    fn read_passages(&mut self, network: &model::Network<'_>, from: usize, line_ends: bool) -> model::Passages {
        let (most_elsewhere, most_below) = network.passage_sizes();
        let subwords = network.subwords();
        let cache = &mut self.stream.cache;
        // Below: the line end first when the caret line has nothing after
        // the caret, then the following lines.
        let below_text = if line_ends {
            format!("\n{}", self.passage_below)
        } else {
            self.passage_below.clone()
        };
        let mut below = Vec::new();
        subwords.encode(&below_text, cache, &mut below, &mut Vec::new());
        below.truncate(most_below);

        // Names around the caret, rare ones counting for more.
        let mut wanted: FxHashMap<u32, f32> = FxHashMap::default();
        let head = &self.documents[0];
        let near = head.ids.iter().rev().take(384).chain(self.documents[1].ids.iter().take(96));
        for &id in near {
            if kind(id) == IDENT {
                let uses = self.counts.get(&id).copied().unwrap_or(1).max(1) as f32;
                wanted.insert(id, 1. / uses.sqrt());
            }
        }
        let window_byte = self.stream.starts.get(from).map_or(0, |&start| start as usize);
        // Best of all: the place the pattern predictor finds most like the
        // caret's context, with what followed it there.
        if analog_passages()
            && let Some((index, start, end)) = self.analog_place(window_byte)
        {
            let mut elsewhere = Vec::new();
            let text = &self.documents[index].text[start..end];
            subwords.encode(text, &mut self.stream.cache, &mut elsewhere, &mut Vec::new());
            elsewhere.truncate(most_elsewhere);
            if elsewhere.len() >= 16 {
                return model::Passages { elsewhere, below };
            }
        }
        // Otherwise the best run of tokens outside the window and the text
        // below, by the names it shares with the caret's surroundings.
        let cache = &mut self.stream.cache;
        let span = most_elsewhere * 2 / 3;
        let mut best: Option<(f32, usize, usize, usize)> = None;
        for (index, document) in self.documents.iter().enumerate() {
            let range = match index {
                0 => 0..document.starts.partition_point(|&start| (start as usize) < window_byte),
                1 => document.starts.partition_point(|&start| (start as usize) < self.passage_below.len())..document.ids.len(),
                _ => 0..document.ids.len(),
            };
            if range.len() < 8 {
                continue;
            }
            let ids = &document.ids[range.clone()];
            let weight = |id: &u32| wanted.get(id).copied().unwrap_or(0.);
            let mut sum: f32 = ids.iter().take(span).map(weight).sum();
            let mut at = 0;
            loop {
                if best.is_none_or(|(score, ..)| sum > score + 1e-3) && sum > 0. {
                    best = Some((sum, index, range.start + at, range.start + (at + span).min(ids.len())));
                }
                // Step by lines' worth of tokens.
                let step = 16;
                if at + span + step > ids.len() {
                    break;
                }
                sum += ids[at + span..at + span + step].iter().map(weight).sum::<f32>();
                sum -= ids[at..at + step].iter().map(weight).sum::<f32>();
                at += step;
            }
        }
        let mut elsewhere = Vec::new();
        // A passage that shares next to nothing tells nothing.
        if let Some((score, index, first, end)) = best
            && score >= 1.
        {
            let document = &self.documents[index];
            let start = document.starts[first] as usize;
            let start = document.text[..start].rfind('\n').map_or(0, |at| at + 1);
            let text = &document.text[start..document.ends[end - 1] as usize];
            subwords.encode(text, cache, &mut elsewhere, &mut Vec::new());
            elsewhere.truncate(most_elsewhere);
        }
        model::Passages { elsewhere, below }
    }

    /// Whether the active document uses the identifier spelled `name`.
    fn names(&self, name: &str) -> bool {
        self.interner
            .ids
            .get(name)
            .is_some_and(|&symbol| self.visible((IDENT << KIND_SHIFT) | symbol))
    }

    fn visible(&self, id: u32) -> bool {
        self.counts.get(&id).is_some_and(|&count| count > 0)
    }

    /// Predict the text following the caret.
    pub(crate) fn predict(
        &mut self,
        caret: Caret<'_>,
        options: Options,
        cancelled: &dyn Fn() -> bool,
    ) -> Option<Prediction> {
        let Caret {
            key,
            before,
            rest,
            below,
            syntax,
            language,
        } = caret;
        if before.len() > MAX_DOCUMENT_BYTES || rest.len() > MAX_DOCUMENT_BYTES || below.len() > MAX_DOCUMENT_BYTES {
            return None;
        }
        // The typed prefix of an unfinished word is matched by spelling.
        let partial_start = before
            .char_indices()
            .rev()
            .take_while(|&(_, ch)| identifier_char(ch))
            .last()
            .map_or(before.len(), |(at, _)| at);
        let partial = &before[partial_start..];
        if partial.len() > MAX_INTERNED_BYTES {
            return None;
        }
        self.set_active(key, &before[..partial_start], below, syntax);
        if self.documents[0].text.len() != partial_start {
            return None;
        }
        // The lexer drops comment text, so a trailing gap holding anything
        // but spaces is a comment or a string. There the text holds no
        // patterns to follow and only the neural model predicts.
        let network = self.network;
        let head = &self.documents[0];
        let consumed = head.ends.last().map_or(0, |&end| end as usize);
        let mut prose = head.text[consumed..]
            .chars()
            .any(|ch| !ch.is_whitespace());
        {
            // An odd number of unpaired quotes on this line: inside a string.
            let line = head.text.rfind('\n').map_or(0, |at| at + 1);
            let mut open = false;
            for at in (0..head.ids.len()).rev() {
                if (head.starts[at] as usize) < line {
                    break;
                }
                if matches!(head.ids[at], id if id == punct('"') || id == punct('`')) {
                    open = !open;
                }
            }
            prose |= open;
        }
        if prose && network.is_none() {
            return None;
        }
        if let Some(network) = network {
            self.set_stream(network, key, before);
        }
        let mut suffix = Document {
            text: rest.trim_end().to_owned(),
            ..Document::default()
        };
        lex(&mut suffix, 0, if prose { Syntax::plain() } else { syntax }, &mut self.interner);
        if suffix.ids.is_empty() && !rest.trim().is_empty() {
            return None; // The rest of the line is a comment.
        }
        // A word continuing directly after the caret is being edited in place.
        if rest.chars().next().is_some_and(identifier_char) {
            return None;
        }
        let space_before = partial.is_empty()
            && before
                .chars()
                .next_back()
                .is_none_or(|ch| ch.is_whitespace());
        let line_start = partial.is_empty()
            && before[before.rfind('\n').map_or(0, |at| at + 1)..]
                .chars()
                .all(|ch| ch == ' ' || ch == '\t');
        let base_indent = {
            let line = &before[before.rfind('\n').map_or(0, |at| at + 1)..];
            let end = line
                .find(|ch: char| ch != ' ' && ch != '\t')
                .unwrap_or(line.len());
            line[..end].to_owned()
        };
        let mut context: Vec<u32> = {
            let ids = &self.documents[0].ids;
            let mut tail = ids[ids.len().saturating_sub(MAX_CONTEXT)..].to_vec();
            if line_start && tail.last().is_some_and(|&id| id != NEWLINE_ID) {
                tail.push(NEWLINE_ID);
            }
            tail
        };
        if context.is_empty() {
            if !prose {
                return None;
            }
            // A comment opening the file: nothing lexical precedes it.
            context.push(0);
        }
        self.subwords.clear();
        // The neural model reads a window of the text before the caret,
        // starting at token `window` of the document.
        let window = network.map_or(0, |network| self.open_window(network, below, rest.is_empty(), false));
        // What the model wrote ahead, kept while the prediction follows it.
        let mut plan: Option<Plan> = None;
        // Whether the caret is inside a double-quoted text on its line.
        let mut quoted = {
            let head = &self.documents[0];
            let line = head.text.rfind('\n').map_or(0, |at| at + 1);
            let first = head.starts.partition_point(|&start| (start as usize) < line);
            head.ids[first..].iter().filter(|&&id| id == punct('"')).count() % 2 == 1
        };

        let mut output = String::new();
        let mut produced = 0usize;
        let mut lines = 0usize;
        // `depth` counts brackets the prediction opened itself; `outer` the
        // ones left open on the caret line, which a block may run on to close.
        let mut depth = 0i32;
        let mut outer = {
            let head = &self.documents[0];
            let line = head.text.rfind('\n').map_or(0, |at| at + 1);
            let first = head.starts.partition_point(|&start| (start as usize) < line);
            let mut open = 0i32;
            for &id in &head.ids[first..] {
                if ['(', '[', '{'].iter().any(|&ch| id == punct(ch)) {
                    open += 1;
                } else if [')', ']', '}'].iter().any(|&ch| id == punct(ch)) {
                    open = (open - 1).max(0);
                }
            }
            open
        };
        self.counting = false;
        self.stopped = None;
        self.first_written = None;
        let mut confidence = 0f32;
        let mut retype = None;
        let mut likelihood = 1f32;
        let mut trace: Vec<Step> = Vec::new();
        let mut voters: Vec<Candidate> = Vec::new();
        let mut relative = String::new();
        let started = std::cell::Cell::new(std::time::Instant::now());
        let mut committed = (0usize, 0usize, 0i32); // output bytes, tokens, depth
        let suffix_first = suffix.ids.first().copied();
        let mut met_suffix = false;
        // Where the caret line ended in the output: bytes and tokens.
        let mut caret_line: Option<(usize, usize)> = None;
        // Line ends reached while every bracket opened by the prediction is
        // closed; the text is cut back to the last such end.
        loop {
            if cancelled() {
                return None;
            }
            let first = produced == 0;
            let mut votes = if prose {
                None
            } else {
                self.vote(&voters, &context, first.then_some(partial), None)
            };
            let threshold = if first { options.first } else { options.next };
            let weak = votes
                .as_ref()
                .is_none_or(|votes| votes.share < 0.8 || votes.supporters < 3);
            if !prose && weak && started.get().elapsed() < options.budget {
                // Fresh evidence for the extended context; established
                // witnesses keep their bindings and accumulated score.
                let mut fresh = self.retrieve(&context, cancelled)?;
                let after_newline = context.last() == Some(&NEWLINE_ID);
                for candidate in &mut fresh {
                    candidate.base = match (after_newline, first) {
                        (true, true) => candidate.position,
                        (true, false) => candidate.position.saturating_sub(2),
                        (false, _) => candidate.position.saturating_sub(1),
                    };
                    candidate.relative.clone_from(&relative);
                }
                fresh.retain(|candidate| {
                    !voters.iter().any(|voter| {
                        voter.document == candidate.document && voter.position == candidate.position
                    })
                });
                voters.extend(fresh);
                voters.sort_unstable_by(|a, b| b.score.total_cmp(&a.score));
                voters.truncate(MAX_VOTERS);
                votes = self.vote(&voters, &context, first.then_some(partial), None);
            }
            let after_newline = context.last() == Some(&NEWLINE_ID);
            let typed = if first { partial.chars().count() } else { 0 };
            // The neural model scores the witnesses' choice and may propose
            // its own where the text offers none. It stays on one line.
            // Model work stops at a hard limit: a long first pass is cut short
            // (and resumed by the next request) rather than delaying the answer.
            let limited = || cancelled() || started.get().elapsed() > options.budget * 3 / 2;
            // What the model wrote ahead is kept while the prediction follows
            // it; otherwise it writes anew from here.
            // The model writes on into a block the prediction is inside, with
            // its own indentation.
            let blocks = multiline_model() && !prose;
            let model_allowed = network.filter(|_| lines == 0 || !after_newline || blocks);
            if let Some(network) = model_allowed
                && plan.as_ref().is_none_or(|plan| plan.rest(&output).is_none())
                && started.get().elapsed() < options.budget
            {
                let (tokens, typed) = self.model_state(network, window, &output);
                plan = None;
                if network.read(&mut self.session, &self.passages, &tokens, window, language, &limited) {
                    let mark = self.session.mark();
                    let open = if blocks && options.multiline { depth.max(0) + outer.max(0) } else { 0 };
                    // The text below the caret line judges the line written:
                    // it must lead into it.
                    let below = if lines == 0 && rest.is_empty() && open == 0 {
                        self.passages.below.clone()
                    } else {
                        Vec::new()
                    };
                    let mut written = network.write_ranked(&mut self.session, &typed, false, open, &below, &limited);
                    // Weighing a doubtful line is worth its time; the time
                    // for checking the rest of the prediction is kept.
                    started.set(started.get() + written.ranking);
                    // Asked for outright with a word begun: when the model
                    // would leave the word as it stands, it writes again
                    // taking the word to be unfinished.
                    if first && !partial.is_empty() && options.first == 0. {
                        let ended = written.bytes.get(typed.len()).is_none_or(|&byte| !identifier_char(byte as char));
                        if ended {
                            network.cut(&mut self.session, mark);
                            written = network.write_block(&mut self.session, &typed, true, open, &limited);
                        }
                    }
                    if output.is_empty() && model::recording() {
                        self.first_written = Some((typed.len(), written.clone()));
                    }
                    plan = Some(Plan::new(output.len(), typed, written, mark));
                }
            }
            let model_syntax = if prose { Syntax::plain() } else { syntax };
            let typed_word = if first { partial } else { "" };
            let proposal = model_allowed
                .and(plan.as_ref())
                .and_then(|plan| plan.next(&output, typed_word, model_syntax, &mut self.interner));
            // Candidates for the next token: the witnesses' favourite, and
            // the model's favourite, backed by whichever witnesses agree with
            // it. The one most likely to be right is taken.
            let witness_step = |votes: &Vote, neural: u8| {
                let mut step = Step {
                    end: 0,
                    share: votes.share,
                    supporters: votes.supporters,
                    score: votes.score,
                    kind: kind(votes.id),
                    line_start: after_newline,
                    lines,
                    renamed: votes.renamed,
                    typed,
                    index: produced,
                    groups: votes.groups,
                    local: votes.local,
                    foreign: votes.foreign,
                    nested: depth > 0,
                    neural,
                    prose: false,
                    coined: false,
                    series: votes.series,
                    model: false,
                    estimate: 0.,
                };
                step.estimate = calibrated(&step, network.is_none());
                step
            };
            // How likely the model finds a witness's choice: read off what
            // it wrote ahead where that starts the same way, and otherwise
            // asked for.
            let mut level = |votes: &Vote, engine: &mut Self| -> u8 {
                let (Some(network), Some(plan)) = (network, plan.as_mut()) else { return 0 };
                let addition = if votes.id == NEWLINE_ID {
                    "\n".to_owned()
                } else if first && !partial.is_empty() {
                    match votes.text.get(partial.len()..) {
                        Some(tail) => tail.to_owned(),
                        None => return 0,
                    }
                } else if first {
                    let space = !space_before && !votes.gap.is_empty() && !votes.gap.contains('\n');
                    format!("{}{}", if space { " " } else { "" }, votes.text)
                } else if after_newline {
                    format!("{base_indent}{}{}", votes.gap, votes.text)
                } else {
                    format!("{}{}", if votes.gap.is_empty() { "" } else { " " }, votes.text)
                };
                if addition.is_empty() {
                    return 0;
                }
                if let Some(chance) = plan.chance_of(&output, &addition) {
                    return neural_level(chance);
                }
                if started.get().elapsed() >= options.budget {
                    return 0;
                }
                // Asked from where the prediction stands now.
                let (tokens, mut typed) = engine.model_state(network, window, &output);
                if !network.read(&mut engine.session, &engine.passages, &tokens, window, language, &limited) {
                    return 0;
                }
                if first && !partial.is_empty() {
                    // The typed start as this choice spells it.
                    let shared = partial.len().min(typed.len());
                    let from = typed.len() - shared;
                    typed[from..].copy_from_slice(&votes.text.as_bytes()[partial.len() - shared..partial.len()]);
                }
                let mark = engine.session.mark();
                network
                    .chance(&mut engine.session, mark, &typed, addition.as_bytes(), &limited)
                    .map_or(0, neural_level)
            };
            let mut chosen: Option<(Step, Vote)> = match votes {
                Some(votes) => {
                    let level = level(&votes, self);
                    Some((witness_step(&votes, level), votes))
                }
                None => None,
            };
            if let Some(proposal) = proposal
                && !chosen.as_ref().is_some_and(|(_, votes)| {
                    votes.text == proposal.text && kind(votes.id) == kind(proposal.id)
                })
            {
                let wanted = Some((proposal.text.as_str(), kind(proposal.id)));
                let rival = match self.vote(&voters, &context, first.then_some(partial), wanted) {
                    Some(backed) => {
                        let level = level(&backed, self);
                        Some((witness_step(&backed, level), backed))
                    }
                    // A line end proposed inside an open bracket starts a
                    // block, which the model indents only when it writes on.
                    None if proposal.id == NEWLINE_ID && (depth > 0 || outer > 0) && !blocks => None,
                    None => {
                        let mut step = Step {
                            end: 0,
                            share: 0.,
                            supporters: 0,
                            score: 0.,
                            kind: kind(proposal.id),
                            line_start: after_newline,
                            lines,
                            renamed: false,
                            typed,
                            index: produced,
                            groups: 0,
                            local: false,
                            foreign: false,
                            nested: depth > 0,
                            neural: neural_level(proposal.probability),
                            prose,
                            coined: false,
                            series: false,
                            model: true,
                            estimate: 0.,
                        };
                        step.estimate = calibrated(&step, false);
                        let gap = if proposal.spaced { " " } else { "" };
                        Some((
                            step,
                            Vote {
                                groups: 0,
                                local: false,
                                foreign: false,
                                score: 0.,
                                renamed: false,
                                series: false,
                                id: proposal.id,
                                text: proposal.text,
                                gap: gap.to_owned(),
                                indent: proposal.indent,
                                share: 0.,
                                supporters: 0,
                                agreeing: Vec::new(),
                            },
                        ))
                    }
                };
                if let Some(rival) = rival
                    && chosen.as_ref().is_none_or(|(step, _)| rival.0.estimate > step.estimate)
                {
                    chosen = Some(rival);
                }
            }
            let Some((mut step, votes)) = chosen else { break };
            // A pattern the model had no time left to check is taken only
            // when the text points at it firmly: under load, unchecked
            // patterns run on into confident wrong text.
            let unchecked = network.is_some()
                && !step.model
                && step.neural == 0
                && started.get().elapsed() >= options.budget
                && !(step.share >= 0.95 && step.score >= 28.);
            if unchecked {
                self.stopped = Some((votes.text, step));
                break;
            }
            let probability = step.estimate;
            if probability < threshold || likelihood * probability < options.floor {
                self.stopped = Some((votes.text, step));
                break;
            }
            likelihood *= probability;
            let Vote { id, text, gap, indent, .. } = votes;
            if first {
                confidence = probability;
            }
            if id == NEWLINE_ID {
                if prose {
                    break; // A comment is continued to the end of its line.
                }
                if depth <= 0 {
                    committed = (output.len(), produced, depth);
                    if outer <= 0 {
                        break;
                    }
                }
                // Leaving the caret line needs only ordinary confidence;
                // every further line must be nearly certain.
                let needed = if first { options.first } else { options.block };
                if !options.multiline || probability < needed || lines + 1 >= MAX_OUTPUT_LINES {
                    break;
                }
            }
            if id == 0 {
                break; // End of a witness document.
            }
            let closes = [')', ']', '}'].iter().any(|&ch| id == punct(ch));
            let opens = ['(', '[', '{'].iter().any(|&ch| id == punct(ch));
            if Some(id) == suffix_first && depth - i32::from(closes) <= 0 && !(first && !partial.is_empty())
            {
                // The text right of the caret already supplies this token.
                committed = (output.len(), produced, depth);
                met_suffix = true;
                break;
            }
            if id == NEWLINE_ID {
                if lines == 0 {
                    caret_line = Some((output.len(), produced));
                }
                lines += 1;
                output.push('\n');
            } else if first {
                if !partial.is_empty() {
                    if !text.starts_with(partial) {
                        retype = Some(text[..partial.len()].to_owned());
                    }
                    output.push_str(&text[partial.len()..]);
                } else {
                    if !space_before && !gap.is_empty() && !gap.contains('\n') {
                        output.push(' ');
                    }
                    output.push_str(&text);
                }
            } else {
                if context.last() == Some(&NEWLINE_ID) {
                    if let Some(indent) = &indent {
                        output.push_str(indent);
                        relative = indent.strip_prefix(base_indent.as_str()).unwrap_or("").to_owned();
                    } else {
                        output.push_str(&base_indent);
                        output.push_str(&gap);
                        relative.clone_from(&gap);
                    }
                } else if !gap.is_empty() {
                    output.push(' ');
                }
                output.push_str(&text);
            }
            if closes && depth == 0 {
                outer -= 1;
            } else {
                depth += i32::from(opens) - i32::from(closes);
            }
            produced += 1;
            step.end = output.len();
            self.counting |= step.series;
            trace.push(step);
            let query_id = self.query_id(id, &text);
            if id == punct('"') {
                quoted = !quoted;
            }
            context.push(query_id);
            if context.len() > MAX_CONTEXT {
                context.remove(0);
            }
            // Advance agreeing witnesses; the others no longer describe
            // this continuation.
            let mut next = Vec::with_capacity(voters.len());
            for (index, mut voter) in std::mem::take(&mut voters).into_iter().enumerate() {
                if !votes.agreeing.contains(&(index as u32)) {
                    continue;
                }
                let source = self.documents[voter.document as usize].ids[voter.position as usize];
                if kind(source) <= STRING
                    && source != query_id
                    && kind(source) == kind(query_id)
                    && !voter.renames.iter().any(|&(from, _)| from == source)
                {
                    voter.renames.push((source, query_id));
                }
                voter.score += token_weight(source, source == query_id);
                voter.position += 1;
                next.push(voter);
            }
            voters = next;
            if depth <= 0 && id != NEWLINE_ID {
                committed = (output.len(), produced, depth);
            }
            if produced >= options.tokens.min(MAX_OUTPUT_TOKENS) || output.len() >= MAX_OUTPUT_BYTES {
                break;
            }
        }
        if lines > 0 && depth > 0 {
            // Further lines are offered only up to the last complete one. The
            // caret line is kept whole even when it leaves a bracket open:
            // a line that opens a block is a complete thing to offer.
            let (bytes, tokens) = match caret_line {
                Some(line) if line.0 > committed.0 => line,
                _ => (committed.0, committed.1),
            };
            output.truncate(bytes);
            produced = tokens;
            trace.truncate(produced);
        }
        // Text right of the caret must be accounted for: either the
        // prediction runs into it, or it only closes what is already open.
        if !suffix.ids.is_empty() && !met_suffix {
            let closers = suffix.ids.iter().all(|&id| {
                [')', ']', '}', ';', ',', '"', '\'', '>', ':'].iter().any(|&ch| id == punct(ch))
            });
            if !closers {
                return None;
            }
            // Keep only the part inside the brackets the suffix closes.
            let mut level = 0i32;
            let mut cut = None;
            let mut quote = None;
            for (at, ch) in output.char_indices() {
                if let Some(open) = quote {
                    if ch == open {
                        quote = None;
                    }
                    continue;
                }
                match ch {
                    '"' | '`' => quote = Some(ch),
                    '(' | '[' | '{' => level += 1,
                    ')' | ']' | '}' => {
                        level -= 1;
                        if level < 0 {
                            cut = Some(at);
                            break;
                        }
                    }
                    ';' if level == 0 => {
                        cut = Some(at);
                        break;
                    }
                    _ => {}
                }
            }
            if let Some(cut) = cut {
                output.truncate(cut);
            }
        }
        // A quote opened last, with nothing to put in it, is not shown.
        if quoted && output.ends_with('"') && produced > 1 {
            output.pop();
            produced -= 1;
            trace.truncate(produced);
        }
        let text = output.trim_end().to_owned();
        if text.is_empty() {
            return None;
        }
        Some(Prediction {
            text,
            confidence,
            tokens: produced,
            retype,
            trace,
        })
    }

    /// Whether a token proposed by the neural model is set off by a space.
    /// The active document's own spacing of the same pair decides first.
    fn query_id(&mut self, id: u32, text: &str) -> u32 {
        match kind(id) {
            IDENT | KEYWORD => self.interner.word(text),
            NUMBER | STRING if id & SYMBOL_MASK != 0 => {
                (kind(id) << KIND_SHIFT) | self.interner.intern(text)
            }
            _ => id,
        }
    }

    /// Collect positions whose preceding tokens match the end of `context`.
    fn retrieve(&mut self, context: &[u32], cancelled: &dyn Fn() -> bool) -> Option<Vec<Candidate>> {
        let length = context.len();
        let last = context[length - 1];
        let previous = if length >= 2 { context[length - 2] } else { 0 };
        let (last_class, previous_class) = (class(last), class(previous));
        let mut exact = Vec::new();
        let mut loose = Vec::new();
        let order: Vec<usize> = (0..self.documents.len()).collect();
        for &index in &order {
            if cancelled() {
                return None;
            }
            let ids = &self.documents[index].ids;
            if ids.len() < 2 {
                continue;
            }
            // Nearest first in the text above the caret.
            let mut visit = |position: usize| {
                let a = ids[position - 1];
                if class(a) != last_class {
                    return;
                }
                let b = if position >= 2 { ids[position - 2] } else { 0 };
                if length >= 2 && class(b) != previous_class {
                    return;
                }
                if a == last && (length < 2 || b == previous) {
                    if exact.len() < MAX_EXACT_CANDIDATES {
                        exact.push((index as u32, position as u32));
                    }
                } else if loose.len() < MAX_ABSTRACT_CANDIDATES {
                    loose.push((index as u32, position as u32));
                }
            };
            if index == 0 {
                for position in (1..ids.len()).rev() {
                    visit(position);
                }
            } else {
                for position in 1..ids.len() {
                    visit(position);
                }
            }
        }
        let mut candidates = Vec::with_capacity(exact.len() + loose.len());
        for (document, position) in exact.into_iter().chain(loose) {
            if let Some(candidate) = self.align(document, position, context) {
                candidates.push(candidate);
            }
        }
        candidates.sort_unstable_by(|a, b| b.score.total_cmp(&a.score));
        candidates.truncate(MAX_VOTERS);
        for candidate in &candidates {
            for &(from, to) in &candidate.renames {
                if kind(from) == IDENT {
                    let (from, to) = (self.interner.name(from), self.interner.name(to));
                    self.subwords.learn(from, to);
                }
            }
        }
        Some(candidates)
    }

    /// Walk backwards from a candidate position, requiring literal structure
    /// to agree and identifiers to rename one-to-one.
    fn align(&self, document: u32, position: u32, context: &[u32]) -> Option<Candidate> {
        let ids = &self.documents[document as usize].ids;
        let position = position as usize;
        let mut renames: Vec<(u32, u32)> = Vec::new();
        let mut score = 0f32;
        let mut matched = 0u16;
        for back in 1..=context.len().min(position) {
            let source = ids[position - back];
            let query = context[context.len() - back];
            if source == query {
                if kind(source) <= STRING
                    && renames
                        .iter()
                        .any(|&(from, to)| (from == source) != (to == query))
                {
                    break;
                }
                if kind(source) <= STRING && !renames.iter().any(|&(from, _)| from == source) {
                    renames.push((source, query));
                }
                score += token_weight(source, true);
            } else if kind(source) == kind(query) && kind(source) <= STRING {
                if kind(source) == IDENT {
                    let forward = renames.iter().find(|&&(from, _)| from == source);
                    let reverse = renames.iter().find(|&&(_, to)| to == query);
                    match (forward, reverse) {
                        (Some(&(_, to)), _) if to != query => break,
                        (_, Some(&(from, _))) if from != source => break,
                        (None, None) => renames.push((source, query)),
                        _ => {}
                    }
                }
                score += token_weight(source, false);
            } else {
                break;
            }
            matched += 1;
        }
        if matched < 2 && context.len() >= 2 {
            return None;
        }
        // A context made only of renamed parameters carries little evidence.
        if score < 2. {
            return None;
        }
        // The text above the caret is the most relevant, recent text most.
        if document == 0 {
            let distance = (ids.len() - position) as f32;
            score += 1.5 / (1. + distance / 400.);
        } else if document == 1 {
            score += 0.5;
        }
        renames.retain(|&(from, to)| from != to);
        Some(Candidate {
            document,
            position: position as u32,
            score,
            renames,
            base: position as u32,
            relative: String::new(),
        })
    }

    /// Let every witness propose its next token and pick the heaviest.
    /// With `wanted`, the witnesses backing that token are reported instead
    /// of the heaviest group.
    fn vote(
        &self,
        voters: &[Candidate],
        context: &[u32],
        partial: Option<&str>,
        wanted: Option<(&str, u32)>,
    ) -> Option<Vote> {
        if voters.is_empty() {
            return None;
        }
        struct Group {
            id: u32,
            text: String,
            weight: f32,
            best: f32,
            score: f32,
            renamed: bool,
            local: bool,
            foreign: bool,
            gap: String,
            members: Vec<u32>,
            /// Where the nearest witness above the caret stands.
            at: Option<u32>,
        }
        let mut groups: Vec<Group> = Vec::new();
        let mut total = 0f32;
        let after_newline = context.last() == Some(&NEWLINE_ID);
        for (index, voter) in voters.iter().enumerate() {
            let document = &self.documents[voter.document as usize];
            let position = voter.position as usize;
            let mut weight = (voter.score / 1.5).min(40.).exp2();
            let mut foreign = false;
            let (id, text) = if position >= document.ids.len() {
                (0, String::new())
            } else {
                let source = document.ids[position];
                match kind(source) {
                    IDENT => {
                        let name = document.token(position);
                        if let Some(&(_, to)) =
                            voter.renames.iter().find(|&&(from, _)| from == source)
                        {
                            (to, self.interner.name(to).to_owned())
                        } else if let Some(derived) = (!voter.renames.is_empty())
                            .then(|| self.subwords.apply(name))
                            .flatten()
                        {
                            let known = self
                                .interner
                                .ids
                                .get(derived.as_str())
                                .is_some_and(|&symbol| self.visible((IDENT << KIND_SHIFT) | symbol));
                            if !known {
                                weight *= 0.6;
                            }
                            (source, derived)
                        } else {
                            if !self.visible(source) && source & SYMBOL_MASK != 0 {
                                // A name the active document never mentions:
                                // plausible as an API, unlikely as a variable.
                                let previous = position
                                    .checked_sub(1)
                                    .map(|at| document.ids[at])
                                    .unwrap_or(0);
                                let next = document.ids.get(position + 1).copied().unwrap_or(0);
                                let api = previous == punct('.')
                                    || previous == punct(':')
                                    || previous == punct('>')
                                    || next == punct('(')
                                    || next == punct(':')
                                    || next == punct('!')
                                    || name.chars().next().is_some_and(char::is_uppercase);
                                weight *= if api { 0.7 } else { 0.25 };
                                foreign = true;
                            }
                            (source, name.to_owned())
                        }
                    }
                    NEWLINE => (source, String::new()),
                    _ => (source, document.token(position).to_owned()),
                }
            };
            if let Some(partial) = partial
                && !partial.is_empty()
                && (!begins(&text, partial) || kind(id) > KEYWORD)
            {
                continue;
            }
            total += weight;
            let gap = if id == 0 {
                String::new()
            } else if after_newline {
                // Indentation relative to the line this witness aligned with.
                let indent = document.indent(position);
                let base = document.indent(voter.base as usize);
                if let Some(extra) = indent.strip_prefix(base) {
                    format!("{}{extra}", voter.relative)
                } else if base.starts_with(indent) {
                    let keep = voter
                        .relative
                        .len()
                        .saturating_sub(base.len() - indent.len());
                    voter.relative[..keep].to_owned()
                } else {
                    voter.relative.clone()
                }
            } else {
                document.gap(position).to_owned()
            };
            let renamed = kind(id) == IDENT
                && position < document.ids.len()
                && document.token(position) != text;
            let at = (voter.document == 0).then_some(voter.position);
            match groups
                .iter_mut()
                .find(|group| group.text == text && kind(group.id) == kind(id))
            {
                Some(group) => {
                    group.weight += weight;
                    group.score = group.score.max(voter.score);
                    if weight > group.best {
                        group.best = weight;
                        group.gap = gap;
                        group.renamed = renamed;
                        group.local = voter.document == 0;
                        group.foreign = foreign;
                    }
                    group.members.push(index as u32);
                    group.at = group.at.max(at);
                }
                None => groups.push(Group {
                    id,
                    text,
                    weight,
                    best: weight,
                    score: voter.score,
                    renamed,
                    local: voter.document == 0,
                    foreign,
                    gap,
                    members: vec![index as u32],
                    at,
                }),
            }
        }
        // Witnesses above the caret that propose the steps of a series
        // (`item1`, then `item2`) together propose its next step (`item3`).
        let mut steps: Vec<usize> = (0..groups.len())
            .filter(|&index| groups[index].at.is_some() && kind(groups[index].id) <= STRING)
            .collect();
        steps.sort_by_key(|&index| groups[index].at);
        // A series is counted through in neighbouring lines just above the
        // caret; steps found far apart are more likely separate things.
        const NEAR: u32 = 40;
        let end = self.documents[0].ids.len() as u32;
        let near = |later: u32, earlier: Option<u32>| earlier.is_some_and(|at| later.saturating_sub(at) <= NEAR);
        let mut series = None;
        if let [.., earlier, latest] = steps[..]
            && kind(groups[earlier].id) == kind(groups[latest].id)
            && near(end, groups[latest].at)
            && near(groups[latest].at.unwrap_or(0), groups[earlier].at)
            && let Some(next) = series_next(&groups[earlier].text, &groups[latest].text)
            && !(kind(groups[latest].id) == IDENT && self.names(&next))
            // A bare number or letter counts on only with a third step
            // behind it, or on a line that is already counting.
            && (groups[latest].text.trim_end_matches(|ch: char| ch.is_ascii_digit()).len() > 1
                || steps.len() >= 3
                || self.counting)
            && steps.len().checked_sub(3).is_none_or(|third| {
                series_next(&groups[steps[third]].text, &groups[earlier].text).as_deref()
                    == Some(groups[latest].text.as_str())
            })
            && !groups.iter().any(|group| group.text == next)
        {
            // The whole series stands behind the next step.
            let mut parts = steps[steps.len().saturating_sub(3)..].to_vec();
            parts.sort_unstable_by(|a, b| b.cmp(a));
            let mut merged: Option<Group> = None;
            for index in parts {
                let group = groups.remove(index);
                match merged.as_mut() {
                    // The nearest step lends its spacing and standing.
                    Some(merged) if merged.at > group.at => {
                        merged.weight += group.weight;
                        merged.score = merged.score.max(group.score);
                        merged.members.extend(&group.members);
                    }
                    Some(merged) => {
                        let mut group = group;
                        group.weight += merged.weight;
                        group.score = group.score.max(merged.score);
                        group.members.extend(&merged.members);
                        *merged = group;
                    }
                    None => merged = Some(group),
                }
            }
            let mut merged = merged?;
            series = Some(next.clone());
            merged.text = next;
            // Judged as a series, which is not a renaming.
            merged.renamed = false;
            groups.push(merged);
        } else if let Some(&latest) = steps.last()
            && kind(groups[latest].id) == IDENT
            && !groups[latest].renamed
            && let Some(at) = groups[latest].at
            && near(end, Some(at))
            && let Some((previous, next)) = series_around(&groups[latest].text)
            && self.names(&previous)
            && !self.names(&next)
            && !self.documents[0].ids[..at as usize].contains(&groups[latest].id)
        {
            // One witness, at the place where it introduces a name that
            // follows another (`row_b` after `row_a`): this place introduces
            // the one after it.
            series = Some(next.clone());
            groups[latest].text = next;
        }
        let count = groups.len();
        let best = match wanted {
            Some((text, wanted_kind)) => groups
                .into_iter()
                .find(|group| group.text == text && kind(group.id) == wanted_kind)?,
            None => groups
                .into_iter()
                .max_by(|a, b| a.weight.total_cmp(&b.weight))?,
        };
        // A constant prior keeps a lone weak witness from looking certain.
        let prior = (6f32 / 1.5).exp2();
        Some(Vote {
            groups: count,
            local: best.local,
            foreign: best.foreign,
            score: best.score,
            renamed: best.renamed,
            series: series.as_deref() == Some(best.text.as_str()),
            id: best.id,
            share: best.weight / (total + prior),
            supporters: best.members.len(),
            text: best.text,
            gap: best.gap,
            indent: None,
            agreeing: best.members,
        })
    }
}

/// The step after `earlier` and `latest` in a series: a number at the end
/// counting on (`item1`, `item2` give `item3`; `10`, `20` give `30`), or a
/// letter (`row_a`, `row_b` give `row_c`; `a`, `b` give `c`).
fn series_next(earlier: &str, latest: &str) -> Option<String> {
    // A quoted text counts by what is inside the quotes.
    if let (Some(quote), true) = (latest.chars().next_back(), latest.len() > 2)
        && matches!(quote, '"' | '\'' | '`')
        && earlier.len() > 2
        && earlier.ends_with(quote)
    {
        let next = series_next(&earlier[..earlier.len() - 1], &latest[..latest.len() - 1])?;
        return Some(format!("{next}{quote}"));
    }
    let digits = |text: &str| text.len() - text.trim_end_matches(|ch: char| ch.is_ascii_digit()).len();
    let (a, b) = (digits(earlier), digits(latest));
    if a > 0 && b > 0 && a <= 9 && b <= 9 {
        let (stem, number) = latest.split_at(latest.len() - b);
        if earlier[..earlier.len() - a] != *stem {
            return None;
        }
        let (from, to): (i64, i64) = (earlier[earlier.len() - a..].parse().ok()?, number.parse().ok()?);
        let next = to + (to - from);
        if to == from || next < 0 || (to - from).abs() > 1000 {
            return None;
        }
        // Keep leading zeros: `file_01`, `file_02` give `file_03`.
        return Some(if number.starts_with('0') && b > 1 {
            format!("{stem}{next:0b$}")
        } else {
            format!("{stem}{next}")
        });
    }
    let (from, to) = (earlier.chars().next_back()?, latest.chars().next_back()?);
    let stem = &latest[..latest.len() - to.len_utf8()];
    let apart = stem.chars().next_back().is_none_or(|ch| !ch.is_alphabetic());
    if from.is_ascii_alphabetic()
        && to.is_ascii_alphabetic()
        && apart
        && earlier[..earlier.len() - 1] == *stem
        && from.is_ascii_uppercase() == to.is_ascii_uppercase()
        && to as u32 == from as u32 + 1
        && !matches!(to, 'z' | 'Z')
    {
        return Some(format!("{stem}{}", (to as u8 + 1) as char));
    }
    None
}

/// The names before and after `name` in a series counting by one, when its
/// end can be counted: `item2` stands between `item1` and `item3`, `row_b`
/// between `row_a` and `row_c`.
fn series_around(name: &str) -> Option<(String, String)> {
    let digits = name.len() - name.trim_end_matches(|ch: char| ch.is_ascii_digit()).len();
    let previous = if digits > 0 {
        let (stem, number) = name.split_at(name.len() - digits);
        let value: u64 = number.parse().ok()?;
        let before = value.checked_sub(1)?;
        if number.starts_with('0') && digits > 1 {
            format!("{stem}{before:0digits$}")
        } else {
            format!("{stem}{before}")
        }
    } else {
        let last = name.chars().next_back().filter(char::is_ascii_alphabetic)?;
        let stem = &name[..name.len() - 1];
        if stem.is_empty() || matches!(last, 'a' | 'A') {
            return None;
        }
        format!("{stem}{}", (last as u8 - 1) as char)
    };
    let next = series_next(&previous, name)?;
    Some((previous, next))
}

fn token_weight(id: u32, exact: bool) -> f32 {
    match (kind(id), exact) {
        (IDENT, true) => 2.,
        (IDENT, false) => 0.75,
        (KEYWORD, _) => 1.5,
        (NUMBER | STRING, true) => 1.5,
        (NUMBER | STRING, false) => 0.5,
        (NEWLINE, _) => 0.5,
        _ => 1.,
    }
}

struct Vote {
    groups: usize,
    local: bool,
    foreign: bool,
    score: f32,
    renamed: bool,
    /// The next step of a series the witnesses count through.
    series: bool,
    id: u32,
    text: String,
    gap: String,
    /// The whole indentation of a line the token starts, when the neural
    /// model wrote it; otherwise the caret line's plus `gap`.
    indent: Option<String>,
    share: f32,
    supporters: usize,
    agreeing: Vec<u32>,
}

/// Sub-word renamings implied by the matched identifier renamings.
#[derive(Default)]
struct Subwords {
    pairs: Vec<(String, String)>,
}

fn split_subwords(name: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let bytes = name.as_bytes();
    let mut index = 0usize;
    for (at, ch) in name.char_indices() {
        let previous = index.checked_sub(1).map(|i| bytes[i] as char);
        index = at + ch.len_utf8();
        if at == start {
            if ch == '_' {
                start = at + 1;
            }
            continue;
        }
        let boundary = ch == '_'
            || (ch.is_uppercase() && previous.is_some_and(|p| p.is_lowercase() || p.is_ascii_digit()))
            || (ch.is_ascii_digit() != previous.is_some_and(|p| p.is_ascii_digit()));
        if boundary {
            if at > start {
                parts.push(&name[start..at]);
            }
            start = if ch == '_' { at + 1 } else { at };
        }
    }
    if start < name.len() {
        parts.push(&name[start..]);
    }
    parts
}

fn restyle(template: &str, word: &str) -> String {
    let lower = word.to_lowercase();
    if template.len() > 1 && template.chars().all(|ch| !ch.is_lowercase()) {
        lower.to_uppercase()
    } else if template.chars().next().is_some_and(char::is_uppercase) {
        let mut chars = lower.chars();
        chars
            .next()
            .map(|first| first.to_uppercase().chain(chars).collect())
            .unwrap_or_default()
    } else {
        lower
    }
}

impl Subwords {
    fn clear(&mut self) {
        self.pairs.clear();
    }

    fn learn(&mut self, from: &str, to: &str) {
        let (a, b) = (split_subwords(from), split_subwords(to));
        let pairs: Vec<(String, String)> = if a.len() == b.len() && a.len() > 1 {
            a.iter()
                .zip(&b)
                .filter(|(x, y)| !x.eq_ignore_ascii_case(y))
                .map(|(x, y)| (x.to_lowercase(), y.to_lowercase()))
                .collect()
        } else if a.len() == 1 && b.len() == 1 {
            vec![(from.to_lowercase(), to.to_lowercase())]
        } else {
            Vec::new()
        };
        for (x, y) in pairs {
            if x.len() < 2 || y.len() < 2 || x.chars().all(|ch| ch.is_ascii_digit()) {
                continue;
            }
            // Conflicting evidence for the same sub-word withdraws it.
            if let Some(existing) = self.pairs.iter().position(|(known, _)| *known == x) {
                if self.pairs[existing].1 != y {
                    self.pairs[existing].1.clear();
                }
            } else if self.pairs.len() < 32 {
                self.pairs.push((x, y));
            }
        }
    }

    /// Rename the sub-words of `name`, or None when none of them is known.
    fn apply(&self, name: &str) -> Option<String> {
        if self.pairs.is_empty() {
            return None;
        }
        let parts = split_subwords(name);
        if parts.len() < 2 {
            return None;
        }
        let mut result = String::with_capacity(name.len() + 8);
        let mut changed = false;
        let mut cursor = 0;
        for part in parts {
            let start = part.as_ptr() as usize - name.as_ptr() as usize;
            result.push_str(&name[cursor..start]);
            cursor = start + part.len();
            let lower = part.to_lowercase();
            match self
                .pairs
                .iter()
                .find(|(from, to)| *from == lower && !to.is_empty())
            {
                Some((_, to)) => {
                    changed = true;
                    result.push_str(&restyle(part, to));
                }
                None => result.push_str(part),
            }
        }
        result.push_str(&name[cursor..]);
        changed.then_some(result)
    }
}

#[cfg(test)]
#[path = "vtcomplete_inline_tests.rs"]
mod tests;
