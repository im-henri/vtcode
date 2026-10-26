//! Language-aware block discovery from known lexical state. A worker builds a reusable
//! document index directly from Rope chunks. The small prefix helper remains
//! an immediate UI fallback while that index is being prepared.

use crate::editor::Editor;
use crate::scope_bubble_config::{Delimiter, Rules, Syntax};
use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
};

#[path = "animation_scope_checkpoints.rs"]
mod checkpoints;
pub(crate) use checkpoints::MaxEndTree;
use checkpoints::{checkpoint_before, shared_prefix_chars};

#[path = "animation_scope_indentation.rs"]
mod indentation;
#[cfg(test)]
#[path = "animation_language_scope_tests.rs"]
mod language_tests;
#[path = "animation_scope_preprocessor.rs"]
mod preprocessor;
#[path = "animation_scope_statements.rs"]
mod statements;
pub(crate) use preprocessor::keyword_regions as preprocessor_keyword_regions;
#[cfg(test)]
#[path = "animation_inline_scope_tests.rs"]
mod inline_tests;
#[cfg(test)]
#[path = "animation_preprocessor_scope_tests.rs"]
mod preprocessor_tests;
#[cfg(test)]
#[path = "scope_bubble_profile_tests.rs"]
mod profile_tests;

// These budgets apply only to the immediate draw fallback, not worker analysis.
const MAX_LINES: usize = 512;
const MAX_CHARS: usize = 32 * 1024;
const MAX_DEPTH: usize = 128;
type Position = (usize, usize);
type Pair = (Position, Position);
const CONTROL_INTERVAL: usize = 4_096;
const NO_NODE: usize = usize::MAX;

fn checkpoint_boundary(ch: char) -> bool {
    ch.is_whitespace() || matches!(ch, '{' | '}' | '(' | ')' | '[' | ']')
}

#[derive(Clone)]
pub struct StructuralScopeRequest {
    source: ropey::Rope,
    navigation_id: u64,
    version: u64,
    wrap_revision: u64,
    language: Option<String>,
    path: Option<PathBuf>,
    dialect: Dialect,
    delimiters: u8,
    language_blocks: bool,
    rules: std::sync::Arc<Rules>,
}

#[derive(Clone)]
struct ScopeNode {
    delimiter: Delimiter,
    start: usize,
    end: usize,
    parent: usize,
    depth: usize,
    anchor: Option<usize>,
    uncertain_from: usize,
}

#[derive(Clone)]
struct ParenNode {
    control: bool,
    parent: usize,
}

struct ParenStack {
    entries: Vec<ParenNode>,
    head: usize,
    retained: Cell<usize>,
}

impl ParenStack {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            head: NO_NODE,
            retained: Cell::new(0),
        }
    }
    fn push(&mut self, control: bool) {
        self.entries.push(ParenNode {
            control,
            parent: self.head,
        });
        self.head = self.entries.len() - 1;
    }
    fn pop(&mut self) -> Option<bool> {
        if self.head == NO_NODE {
            return None;
        }
        let index = self.head;
        let node = &self.entries[index];
        let control = node.control;
        self.head = node.parent;
        if index + 1 == self.entries.len() && index >= self.retained.get() {
            self.entries.pop();
        }
        Some(control)
    }
}

#[derive(Clone)]
struct ScopeCheckpoint {
    offset: usize,
    node_len: usize,
    open_node: usize,
    closed: usize,
    paren_len: usize,
    paren_head: usize,
    regex_allowed: bool,
    control_word: bool,
    line_start: bool,
}

pub struct StructuralScopeIndex {
    request: StructuralScopeRequest,
    nodes: Vec<ScopeNode>,
    coverage: usize,
    has_scopes: bool,
    checkpoints: Vec<ScopeCheckpoint>,
    parens: Vec<ParenNode>,
    lookup: Option<MaxEndTree>,
    language_scopes: Option<std::sync::Arc<LanguageScopes>>,
    #[cfg(test)]
    work: usize,
    #[cfg(test)]
    restart: usize,
}

impl StructuralScopeRequest {
    pub(crate) fn from_editor(editor: &Editor) -> Option<Self> {
        let rule = editor.scope_bubble_rules.for_editor(editor);
        let dialect = dialect(editor, rule)?;
        Some(Self {
            source: editor.rope.clone(),
            navigation_id: editor.navigation_id,
            version: editor.content_version,
            wrap_revision: editor.wrap_revision,
            language: editor.syntax_language_token().map(str::to_owned),
            path: editor.syntax_file_path().map(PathBuf::from),
            dialect,
            delimiters: rule.and_then(|rule| rule.delimiters)
                .unwrap_or(if dialect == Dialect::Python { 0 } else { Delimiter::Braces.bit() }),
            language_blocks: rule.is_none_or(|rule| rule.language_blocks),
            rules: editor.scope_bubble_rules.clone(),
        })
    }

    pub(crate) fn matches(&self, editor: &Editor) -> bool {
        self.navigation_id == editor.navigation_id
            && self.version == editor.content_version
            && self.wrap_revision == editor.wrap_revision
            && self.source.is_instance(&editor.rope)
            && self.language.as_deref() == editor.syntax_language_token()
            && self.path.as_deref() == editor.syntax_file_path()
            && std::sync::Arc::ptr_eq(&self.rules, &editor.scope_bubble_rules)
    }

    /// Read Rope chunks on a cancellable worker. Previous immutable source
    /// chunks allow a changed document to restart at a known-safe checkpoint.
    pub(crate) fn build(
        &self,
        previous: Option<&StructuralScopeIndex>,
        cancelled: impl Fn() -> bool,
    ) -> Option<StructuralScopeIndex> {
        if cancelled() {
            return None;
        }
        let mut result = StructuralScopeIndex {
            request: self.clone(),
            nodes: Vec::new(),
            coverage: 0,
            has_scopes: false,
            checkpoints: Vec::new(),
            parens: Vec::new(),
            lookup: None,
            language_scopes: None,
            #[cfg(test)]
            work: 0,
            #[cfg(test)]
            restart: 0,
        };
        if self.language_blocks && self.dialect.has_language_scopes() {
            // Navigation and Save As reuse the immutable result. Changed
            // statement/indentation structure is analyzed only on the worker;
            // the UI has a separate strict prefix budget while it catches up.
            result.language_scopes = previous
                .filter(|previous| {
                    previous.request.dialect == self.dialect
                        && previous.request.source.is_instance(&self.source)
                })
                .and_then(|previous| previous.language_scopes.clone());
            if result.language_scopes.is_none() {
                result.language_scopes = Some(std::sync::Arc::new(LanguageScopes::build(
                    &self.source,
                    self.source.len_chars(),
                    self.dialect,
                    &cancelled,
                    false,
                )?));
            }
        }
        if self.delimiters == 0 {
            result.coverage = self.source.len_chars();
            result.has_scopes = result
                .language_scopes
                .as_ref()
                .is_some_and(|s| !s.pairs.is_empty());
            return (!cancelled()).then_some(result);
        }
        let control = ScanControl::new(&cancelled);
        let chars = CharInput::rope(&self.source);
        let mut lexer = Lexer::controlled(&chars, self.dialect, 0, Some(&control));
        lexer.delimiters = self.delimiters;
        let mut head = NO_NODE;
        let mut closed = 0;
        let mut branches = preprocessor::Branches::default();
        result.record_checkpoint(&lexer, head, closed);
        // Parsing depends on the dialect and shared source, so Save As or a
        // language alias can reuse lexical work while receiving a new exact key.
        if let Some(previous) = previous.filter(|previous| {
            previous.request.navigation_id == self.navigation_id
                && previous.request.dialect == self.dialect
                && previous.request.delimiters == self.delimiters
        }) {
            let prefix = shared_prefix_chars(&previous.request.source, &self.source, &cancelled)?;
            if let Some(index) =
                checkpoint_before(&previous.checkpoints, prefix, |point| point.offset)
            {
                let point = &previous.checkpoints[index];
                result.nodes = copy_prefix(&previous.nodes[..point.node_len], &cancelled)?;
                result.checkpoints = copy_prefix(&previous.checkpoints[..=index], &cancelled)?;
                lexer.index = point.offset;
                lexer.checkpoint_at = point.offset.saturating_add(CONTROL_INTERVAL);
                lexer.regex_allowed = point.regex_allowed;
                lexer.control_word = point.control_word;
                lexer.line_start = point.line_start;
                lexer.control_parens.entries =
                    copy_prefix(&previous.parens[..point.paren_len], &cancelled)?;
                lexer.control_parens.head = point.paren_head;
                lexer.control_parens.retained.set(point.paren_len);
                head = point.open_node;
                closed = point.closed;
                let mut ancestor = head;
                let mut steps = 0usize;
                while ancestor != NO_NODE {
                    if steps.is_multiple_of(CONTROL_INTERVAL) && cancelled() {
                        return None;
                    }
                    result.nodes[ancestor].end = NO_NODE;
                    // Later conditional divergence may have invalidated an
                    // ancestor after this checkpoint. Re-evaluate that suffix.
                    if result.nodes[ancestor].uncertain_from >= point.offset {
                        result.nodes[ancestor].uncertain_from = NO_NODE;
                    }
                    ancestor = result.nodes[ancestor].parent;
                    steps += 1;
                }
            }
        }
        #[cfg(test)]
        {
            result.restart = lexer.index;
        }
        loop {
            let next = match lexer.next_index_event() {
                Ok(next) => next,
                Err(()) => {
                    result.coverage = lexer.index;
                    break;
                }
            };
            let (offset, delimiter, opening) = match next {
                Some(LexEvent::Brace(offset, opening)) => (offset, Delimiter::Braces, opening),
                Some(LexEvent::Delimiter(offset, delimiter, opening)) => (offset, delimiter, opening),
                Some(LexEvent::Directive(offset, kind)) => {
                    branches
                        .directive(&mut result.nodes, &mut head, offset, kind, Some(&control))
                        .ok()?;
                    continue;
                }
                Some(LexEvent::Checkpoint) => {
                    // Alternatives may revise ancestor endpoints and anchors.
                    // Resume before the conditional, with a known brace stack.
                    if branches.is_empty() {
                        result.record_checkpoint(&lexer, head, closed);
                    }
                    continue;
                }
                Some(LexEvent::Template(_)) => unreachable!("resolved template"),
                None => {
                    result.coverage = chars.len();
                    if branches.is_empty() {
                        result.record_checkpoint(&lexer, head, closed);
                    }
                    break;
                }
            };
            result.coverage = offset;
            if opening {
                result.nodes.push(ScopeNode {
                    delimiter,
                    start: offset,
                    end: NO_NODE,
                    parent: head,
                    depth: if head == NO_NODE {
                        1
                    } else {
                        result.nodes[head].depth + 1
                    },
                    anchor: None,
                    uncertain_from: NO_NODE,
                });
                head = result.nodes.len() - 1;
            } else {
                if head == NO_NODE {
                    if branches.unmatched_close() {
                        continue;
                    }
                    break;
                }
                let node = head;
                if result.nodes[node].delimiter != delimiter { break; }
                head = result.nodes[node].parent;
                branches.close(&mut result.nodes, node, offset);
                closed += 1;
            }
        }
        branches.abandon(&mut result.nodes, Some(&control)).ok()?;
        if control.was_cancelled.get() || cancelled() {
            return None;
        }
        result.has_scopes = result
            .nodes
            .iter()
            .any(|node| node.end != NO_NODE && node.uncertain_from == NO_NODE)
            || result
                .language_scopes
                .as_ref()
                .is_some_and(|s| !s.pairs.is_empty());
        result.parens = std::mem::take(&mut lexer.control_parens.entries);
        #[cfg(test)]
        {
            result.work = control.work.get();
        }
        result.lookup = Some(MaxEndTree::build(
            result.nodes.iter().map(|node| {
                if node.end == NO_NODE || node.uncertain_from != NO_NODE {
                    0
                } else {
                    node.end.saturating_add(1)
                }
            }),
            &cancelled,
        )?);
        Some(result)
    }
}

impl StructuralScopeIndex {
    pub(crate) fn matches(&self, editor: &Editor) -> bool {
        self.request.matches(editor)
    }
    pub(crate) fn has_scopes(&self) -> bool {
        self.has_scopes
    }
    pub(crate) fn keyword_scopes_first(&self) -> bool {
        self.request.language_blocks && matches!(
            self.request.dialect,
            Dialect::C | Dialect::Cpp | Dialect::CSharp
        )
    }

    /// The interval tree finds the rightmost enclosing pair in
    /// O(log scopes), including after arbitrarily deep closed sibling blocks.
    pub(crate) fn enclosing_scope(&self, editor: &Editor) -> Option<Pair> {
        if !self.matches(editor) || editor.cursor_line >= self.request.source.len_lines() {
            return None;
        }
        innermost(
            self.brace_scope(editor),
            self.language_scopes
                .as_ref()
                .and_then(|scopes| scopes.enclosing_scope(editor)),
        )
    }

    fn brace_scope(&self, editor: &Editor) -> Option<Pair> {
        let source = &self.request.source;
        let cursor = source.line_to_char(editor.cursor_line)
            + editor
                .cursor_col
                .min(source.line(editor.cursor_line).len_chars());
        if cursor > self.coverage {
            return None;
        }
        let last = self
            .nodes
            .partition_point(|node| node.start <= cursor)
            .checked_sub(1)?;
        let index = self
            .lookup
            .as_ref()?
            .rightmost_before(last, cursor.saturating_add(1))?;
        let node = &self.nodes[index];
        let start = node.anchor.unwrap_or(node.start);
        let first = source.char_to_line(start);
        let last = source.char_to_line(node.end);
        Some((
            (first, start - source.line_to_char(first)),
            (last, node.end - source.line_to_char(last)),
        ))
    }

    fn record_checkpoint(&mut self, lexer: &Lexer<'_>, head: usize, closed: usize) {
        if self
            .checkpoints
            .last()
            .is_some_and(|point| point.offset == lexer.index)
        {
            return;
        }
        if lexer.index > 0
            && !self
                .request
                .source
                .get_char(lexer.index - 1)
            .is_some_and(checkpoint_boundary)
        {
            return;
        }
        self.checkpoints.push(ScopeCheckpoint {
            offset: lexer.index,
            node_len: self.nodes.len(),
            open_node: head,
            closed,
            paren_len: lexer.control_parens.entries.len(),
            paren_head: lexer.control_parens.head,
            regex_allowed: lexer.regex_allowed,
            control_word: lexer.control_word,
            line_start: lexer.line_start,
        });
        lexer
            .control_parens
            .retained
            .set(lexer.control_parens.entries.len());
    }
}

fn copy_prefix<T: Clone>(source: &[T], cancelled: &dyn Fn() -> bool) -> Option<Vec<T>> {
    let mut copy = Vec::with_capacity(source.len());
    for chunk in source.chunks(CONTROL_INTERVAL) {
        if cancelled() {
            return None;
        }
        std::thread::yield_now();
        copy.extend_from_slice(chunk);
    }
    Some(copy)
}

struct ScanControl<'a> {
    cancelled: &'a dyn Fn() -> bool,
    remaining: Cell<usize>,
    was_cancelled: Cell<bool>,
    #[cfg(test)]
    work: Cell<usize>,
}

impl<'a> ScanControl<'a> {
    fn new(cancelled: &'a dyn Fn() -> bool) -> Self {
        Self {
            cancelled,
            remaining: Cell::new(0),
            was_cancelled: Cell::new(false),
            #[cfg(test)]
            work: Cell::new(0),
        }
    }
    fn tick(&self) -> Result<(), ()> {
        #[cfg(test)]
        self.work.set(self.work.get() + 1);
        if self.remaining.get() == 0 {
            if (self.cancelled)() {
                self.was_cancelled.set(true);
                return Err(());
            }
            std::thread::yield_now();
            self.remaining.set(CONTROL_INTERVAL);
        }
        self.remaining.set(self.remaining.get() - 1);
        Ok(())
    }
}

fn check_work(control: Option<&ScanControl<'_>>) -> Result<(), ()> {
    control.map_or(Ok(()), ScanControl::tick)
}

#[derive(Default)]
struct DecodedChunk {
    start: usize,
    chars: Vec<char>,
}

/// Keep only one decoded Rope chunk. Lexer lookahead returns copied scalars,
/// so nested literals can safely move this cache without retaining references.
struct CharInput<'a> {
    source: &'a ropey::Rope,
    len: usize,
    cache: RefCell<DecodedChunk>,
}

impl<'a> CharInput<'a> {
    fn rope(source: &'a ropey::Rope) -> Self {
        Self {
            source,
            len: source.len_chars(),
            cache: RefCell::default(),
        }
    }
    fn prefix(source: &'a ropey::Rope, len: usize) -> Self {
        Self {
            source,
            len: len.min(source.len_chars()),
            cache: RefCell::default(),
        }
    }
    fn len(&self) -> usize {
        self.len
    }
    fn get(&self, index: usize) -> Option<char> {
        let source = self.source;
        if index >= self.len {
            return None;
        }
        let mut cache = self.cache.borrow_mut();
        if index < cache.start || index - cache.start >= cache.chars.len() {
            let (chunk, _, start, _) = source.chunk_at_char(index);
            cache.start = start;
            cache.chars.clear();
            cache.chars.extend(chunk.chars());
        }
        cache.chars.get(index - cache.start).copied()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dialect {
    Python,
    Rust,
    C,
    Cpp,
    CSharp,
    Java,
    JavaScript,
    Go,
    Json,
    Css,
}

impl Dialect {
    fn has_language_scopes(self) -> bool {
        matches!(
            self,
            Self::Python | Self::C | Self::Cpp | Self::Java | Self::CSharp
        )
    }
}

fn dialect(editor: &Editor, rule: Option<crate::scope_bubble_config::Rule>) -> Option<Dialect> {
    if rule.is_some_and(|rule| rule.syntax == Syntax::Disabled) { return None; }
    let language = rule.and_then(|rule| rule.syntax_for(editor).language()).or_else(|| editor.syntax_language_token());
    if let Some(language) = language {
        return match language.to_ascii_lowercase().as_str() {
            "python" | "python 3" | "python3" | "py" => Some(Dialect::Python),
            "rust" => Some(Dialect::Rust),
            "c" | "objective-c" | "glsl" | "hlsl" => Some(Dialect::C),
            "c++" | "cpp" | "c/c++ header" | "objective-c++" => Some(Dialect::Cpp),
            "c#" | "csharp" => Some(Dialect::CSharp),
            "java" => Some(Dialect::Java),
            "javascript" | "javascript (babel)" | "javascriptreact" | "jsx" | "typescript"
            | "typescriptreact" | "tsx" | "js" | "ts" => Some(Dialect::JavaScript),
            "go" => Some(Dialect::Go),
            "json" | "jsonc" | "json with comments" => Some(Dialect::Json),
            "css" | "scss" | "less" => Some(Dialect::Css),
            _ => None,
        };
    }
    let extension = editor
        .syntax_file_path()
        .and_then(|path| path.extension())
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "py" | "pyw" | "pyi" => Some(Dialect::Python),
        "rs" => Some(Dialect::Rust),
        "c" | "m" | "glsl" | "hlsl" | "vert" | "frag" => Some(Dialect::C),
        "h" | "cc" | "hh" | "cpp" | "hpp" | "cxx" | "hxx" | "mm" => Some(Dialect::Cpp),
        "cs" => Some(Dialect::CSharp),
        "java" => Some(Dialect::Java),
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "tsx" => Some(Dialect::JavaScript),
        "go" => Some(Dialect::Go),
        "json" | "jsonc" => Some(Dialect::Json),
        "css" | "scss" | "less" => Some(Dialect::Css),
        "" => {
            let first: String = editor.rope.line(0).chars().take(256).collect();
            (first.starts_with("#!") && first.contains("python")).then_some(Dialect::Python)
        }
        _ => None,
    }
}

/// Return the innermost complete block containing the cursor.
#[cfg(test)]
pub(crate) fn enclosing_scope(editor: &Editor) -> Option<Pair> {
    ImmediateScopeIndex::build(editor)?.enclosing_scope(editor)
}

/// A bounded fallback is parsed once for a source revision. Cursor steps then
/// use the same interval lookup as the worker index, rather than re-decoding
/// and re-lexing the first 32 Ki characters on every navigation event.
pub(crate) struct ImmediateScopeIndex {
    nodes: Vec<ScopeNode>,
    lookup: MaxEndTree,
    limit: usize,
    language_scopes: Option<LanguageScopes>,
}

impl ImmediateScopeIndex {
    pub(crate) fn build(editor: &Editor) -> Option<Self> {
        if editor.cursor_line >= MAX_LINES
            || editor.cursor_line >= editor.rope.len_lines()
            || editor
                .rope
                .line_to_char(editor.cursor_line)
                .saturating_add(editor.cursor_col)
                >= MAX_CHARS
        {
            return None;
        }
        let request = StructuralScopeRequest::from_editor(editor)?;
        let dialect = request.dialect;
        let source = &editor.rope;
        let limit = if source.len_lines() > MAX_LINES {
            source.line_to_char(MAX_LINES)
        } else {
            source.len_chars()
        }
        .min(MAX_CHARS);
        let language_scopes = if request.language_blocks && dialect.has_language_scopes() {
            Some(LanguageScopes::build(
                source,
                limit,
                dialect,
                &|| false,
                true,
            )?)
        } else {
            None
        };
        let input = CharInput::prefix(source, limit);
        let mut lexer = Lexer::new(&input, dialect, 0);
        lexer.delimiters = request.delimiters;
        let mut nodes: Vec<ScopeNode> = Vec::new();
        let mut head = NO_NODE;
        let mut branches = preprocessor::Branches::default();
        while request.delimiters != 0
            && let Ok(Some(event)) = lexer.next_index_event()
        {
            let (index, delimiter, opening) = match event {
                LexEvent::Brace(index, opening) => (index, Delimiter::Braces, opening),
                LexEvent::Delimiter(index, delimiter, opening) => (index, delimiter, opening),
                LexEvent::Directive(offset, kind) => {
                    branches
                        .directive(&mut nodes, &mut head, offset, kind, None)
                        .ok()?;
                    continue;
                }
                LexEvent::Checkpoint => continue,
                LexEvent::Template(_) => unreachable!("resolved template"),
            };
            if opening {
                let depth = if head == NO_NODE {
                    0
                } else {
                    nodes[head].depth
                };
                if depth >= MAX_DEPTH {
                    break;
                }
                nodes.push(ScopeNode {
                    delimiter,
                    start: index,
                    end: NO_NODE,
                    parent: head,
                    depth: depth + 1,
                    anchor: None,
                    uncertain_from: NO_NODE,
                });
                head = nodes.len() - 1;
            } else {
                if head == NO_NODE {
                    if branches.unmatched_close() {
                        continue;
                    }
                    break;
                }
                let node = head;
                if nodes[node].delimiter != delimiter { break; }
                head = nodes[node].parent;
                branches.close(&mut nodes, node, index);
            }
        }
        branches.abandon(&mut nodes, None).ok()?;
        let lookup = MaxEndTree::build_bounded(nodes.iter().map(|node| {
            if node.end == NO_NODE || node.uncertain_from != NO_NODE {
                0
            } else {
                node.end.saturating_add(1)
            }
        }))?;
        Some(Self {
            nodes,
            lookup,
            limit,
            language_scopes,
        })
    }

    pub(crate) fn enclosing_scope(&self, editor: &Editor) -> Option<Pair> {
        innermost(
            self.brace_scope(editor),
            self.language_scopes
                .as_ref()
                .and_then(|scopes| scopes.enclosing_scope(editor)),
        )
    }

    fn brace_scope(&self, editor: &Editor) -> Option<Pair> {
        let source = &editor.rope;
        let line = source.get_line(editor.cursor_line)?;
        let cursor =
            source.line_to_char(editor.cursor_line) + editor.cursor_col.min(line.len_chars());
        if cursor >= self.limit {
            return None;
        }
        let last = self
            .nodes
            .partition_point(|node| node.start <= cursor)
            .checked_sub(1)?;
        let node = &self.nodes[self
            .lookup
            .rightmost_before(last, cursor.saturating_add(1))?];
        let position = |offset| {
            let line = source.char_to_line(offset);
            (line, offset - source.line_to_char(line))
        };
        Some((
            position(node.anchor.unwrap_or(node.start)),
            position(node.end),
        ))
    }
}

/// Statement and indentation pairs share the same logarithmic interval lookup
/// as braces, but retain their actual source endpoints for painting. Python
/// membership covers whole rows, including indentation and the insertion point
/// after the last token. It never includes the following dedented row.
struct LanguageScopes {
    pairs: Vec<(usize, usize)>,
    starts: Vec<usize>,
    lookup: MaxEndTree,
}

impl LanguageScopes {
    fn build(
        source: &ropey::Rope,
        limit: usize,
        dialect: Dialect,
        cancelled: &dyn Fn() -> bool,
        bounded: bool,
    ) -> Option<Self> {
        let pairs = if dialect == Dialect::Python {
            indentation::scan(source, limit, cancelled)?
        } else {
            statements::scan(source, limit, dialect, cancelled)?
        };
        let row_membership = dialect == Dialect::Python;
        let mut starts = Vec::with_capacity(pairs.len());
        for (index, &(start, _)) in pairs.iter().enumerate() {
            if index.is_multiple_of(CONTROL_INTERVAL) && cancelled() {
                return None;
            }
            starts.push(if row_membership {
                source.line_to_char(source.char_to_line(start))
            } else {
                start
            });
        }
        let ends = pairs.iter().map(|&(_, end)| {
            if row_membership {
                let line = source.char_to_line(end);
                // Encoded inclusive end is the next row's starting offset.
                source
                    .try_line_to_char(line + 1)
                    .unwrap_or(source.len_chars() + 1)
            } else {
                // Include the caret immediately after a statement terminator.
                end.saturating_add(2)
            }
        });
        let lookup = if bounded {
            MaxEndTree::build_bounded(ends)?
        } else {
            MaxEndTree::build(ends, cancelled)?
        };
        Some(Self {
            pairs,
            starts,
            lookup,
        })
    }

    fn enclosing_scope(&self, editor: &Editor) -> Option<Pair> {
        let source = &editor.rope;
        let line = source.get_line(editor.cursor_line)?;
        let cursor =
            source.line_to_char(editor.cursor_line) + editor.cursor_col.min(line.len_chars());
        let last = self
            .starts
            .partition_point(|&start| start <= cursor)
            .checked_sub(1)?;
        let (start, end) = self.pairs[self
            .lookup
            .rightmost_before(last, cursor.saturating_add(1))?];
        let position = |offset| {
            let line = source.char_to_line(offset);
            (line, offset - source.line_to_char(line))
        };
        Some((position(start), position(end)))
    }
}

fn innermost(left: Option<Pair>, right: Option<Pair>) -> Option<Pair> {
    match (left, right) {
        (Some(left), Some(right)) => Some(
            if right.0 > left.0 || (right.0 == left.0 && right.1 < left.1) {
                right
            } else {
                left
            },
        ),
        (left, right) => left.or(right),
    }
}

struct Lexer<'a> {
    chars: &'a CharInput<'a>,
    dialect: Dialect,
    delimiters: u8,
    index: usize,
    regex_allowed: bool,
    control_word: bool,
    control_parens: ParenStack,
    control: Option<&'a ScanControl<'a>>,
    checkpoint_at: usize,
    line_start: bool,
}

struct TemplateState {
    at: usize,
    quote: char,
    verbatim: bool,
    javascript: bool,
}

enum Literal {
    End(usize),
    Template(TemplateState),
}

enum LexEvent {
    Brace(usize, bool),
    Delimiter(usize, Delimiter, bool),
    Directive(usize, preprocessor::Directive),
    Template(TemplateState),
    Checkpoint,
}

enum TemplateFrame<'a> {
    Text(TemplateState),
    Code { lexer: Lexer<'a>, braces: usize },
}

impl<'a> Lexer<'a> {
    fn new(chars: &'a CharInput<'a>, dialect: Dialect, index: usize) -> Self {
        Self::controlled(chars, dialect, index, None)
    }

    fn controlled(
        chars: &'a CharInput<'a>,
        dialect: Dialect,
        index: usize,
        control: Option<&'a ScanControl<'a>>,
    ) -> Self {
        Self {
            chars,
            dialect,
            delimiters: Delimiter::Braces.bit(),
            index,
            regex_allowed: true,
            control_word: false,
            control_parens: ParenStack::new(),
            control,
            checkpoint_at: index.saturating_add(CONTROL_INTERVAL),
            line_start: index == 0 || chars.get(index - 1) == Some('\n'),
        }
    }

    fn next_index_event(&mut self) -> Result<Option<LexEvent>, ()> {
        loop {
            match self.next_event()? {
                Some(LexEvent::Template(state)) => {
                    self.index = self.interpolated_end(state)?;
                    self.regex_allowed = false;
                    self.control_word = false;
                }
                other => return Ok(other),
            }
        }
    }

    fn next_event(&mut self) -> Result<Option<LexEvent>, ()> {
        while let Some(ch) = self.chars.get(self.index) {
            if self.control.is_some() && self.index >= self.checkpoint_at
                && self.chars.get(self.index - 1).is_some_and(checkpoint_boundary) {
                // Keep the deadline pending until a safe token boundary. Otherwise
                // delimiter-dense code can repeatedly miss checkpoints and reparse
                // a large unchanged prefix after a tiny edit at the end.
                self.checkpoint_at = self.index.saturating_add(CONTROL_INTERVAL);
                return Ok(Some(LexEvent::Checkpoint));
            }
            check_work(self.control)?;
            let at = self.index;
            if ch.is_whitespace() {
                self.line_start |= ch == '\n';
                self.index += 1;
                continue;
            }
            if self.dialect == Dialect::Python && ch == '#' {
                self.index = skip_line_comment(self.chars, at + 1, self.dialect, self.control)?;
                self.line_start = true;
                continue;
            }
            if self.dialect != Dialect::Python && starts(self.chars, at, &['/', '/']) {
                self.index = skip_line_comment(self.chars, at + 2, self.dialect, self.control)?;
                self.line_start = true;
                continue;
            }
            if self.dialect != Dialect::Python && starts(self.chars, at, &['/', '*']) {
                self.index = skip_block_comment(self.chars, at + 2, self.dialect, self.control)?;
                continue;
            }
            if matches!(self.dialect, Dialect::C | Dialect::Cpp | Dialect::CSharp) {
                // Spliced lines cannot introduce a new directive. Directive
                // bodies (including multiline macros) never contribute braces.
                if ch == '\\' {
                    let length = if starts(self.chars, at + 1, &['\r', '\n']) {
                        3
                    } else if self.chars.get(at + 1) == Some('\n') {
                        2
                    } else {
                        0
                    };
                    if length > 0 {
                        self.index += length;
                        continue;
                    }
                }
                if ch == '#' && self.line_start {
                    let kind = preprocessor::directive(self)?;
                    self.line_start = true;
                    if let Some(kind) = kind {
                        return Ok(Some(LexEvent::Directive(at, kind)));
                    }
                    continue;
                }
            }
            self.line_start = false;
            if let Some(literal) = self.literal_end()? {
                match literal {
                    Literal::End(end) => {
                        self.index = end;
                        self.regex_allowed = false;
                        self.control_word = false;
                        continue;
                    }
                    Literal::Template(state) => return Ok(Some(LexEvent::Template(state))),
                }
            }
            if self.dialect == Dialect::JavaScript && ch == '/' {
                if self.regex_allowed {
                    self.index = skip_regex(self.chars, at + 1, self.control)?;
                    self.regex_allowed = false;
                } else {
                    self.index += 1 + usize::from(self.chars.get(at + 1) == Some('='));
                    self.regex_allowed = true;
                }
                self.control_word = false;
                continue;
            }
            if ch.is_alphabetic() || ch == '_' || ch == '$' {
                self.index += 1;
                while self
                    .chars
                    .get(self.index)
                    .is_some_and(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '$'))
                {
                    check_work(self.control)?;
                    self.index += 1;
                }
                if self.dialect == Dialect::JavaScript {
                    if self.index - at > 10 {
                        self.control_word = false;
                        self.regex_allowed = false;
                        continue;
                    }
                    let word: String = (at..self.index)
                        .filter_map(|index| self.chars.get(index))
                        .collect();
                    self.control_word = matches!(
                        word.as_str(),
                        "if" | "while" | "for" | "with" | "switch" | "catch"
                    );
                    self.regex_allowed = matches!(
                        word.as_str(),
                        "return"
                            | "throw"
                            | "case"
                            | "yield"
                            | "await"
                            | "typeof"
                            | "void"
                            | "delete"
                            | "in"
                            | "instanceof"
                            | "else"
                            | "do"
                    );
                }
                continue;
            }
            self.index += 1;
            match ch {
                '{' => {
                    self.regex_allowed = true;
                    self.control_word = false;
                    if self.delimiters & Delimiter::Braces.bit() != 0 { return Ok(Some(LexEvent::Brace(at, true))); }
                }
                '}' => {
                    self.regex_allowed = true;
                    self.control_word = false;
                    if self.delimiters & Delimiter::Braces.bit() != 0 { return Ok(Some(LexEvent::Brace(at, false))); }
                }
                '(' if self.dialect == Dialect::JavaScript => {
                    self.control_parens.push(self.control_word);
                    self.regex_allowed = true;
                }
                ')' if self.dialect == Dialect::JavaScript => {
                    self.regex_allowed = self.control_parens.pop().unwrap_or(false);
                }
                ']' => self.regex_allowed = false,
                '.' => self.regex_allowed = false,
                '\\' if self.dialect == Dialect::Css => {
                    self.index += usize::from(self.index < self.chars.len())
                }
                _ => self.regex_allowed = !ch.is_ascii_digit(),
            }
            self.control_word = false;
            if let Some((delimiter, opening)) = Delimiter::token(ch)
                && delimiter != Delimiter::Braces && self.delimiters & delimiter.bit() != 0 {
                return Ok(Some(LexEvent::Delimiter(at, delimiter, opening)));
            }
        }
        Ok(None)
    }

    fn literal_end(&self) -> Result<Option<Literal>, ()> {
        let at = self.index;
        let ch = self.chars.get(at).unwrap();
        if self.dialect == Dialect::Python {
            let quote = match ch {
                'f' | 'F' | 't' | 'T' => at + 1 + usize::from(matches!(self.chars.get(at + 1), Some('r' | 'R'))),
                'r' | 'R' if matches!(self.chars.get(at + 1), Some('f' | 'F' | 't' | 'T')) => at + 2,
                _ => at,
            };
            if quote > at && matches!(self.chars.get(quote), Some('\'' | '"')) {
                return indentation::formatted_string_end(self.chars, quote, self.control).map(|end| Some(Literal::End(end)));
            }
        }
        if self.dialect == Dialect::Python && matches!(ch, '\'' | '"')
            && starts(self.chars, at, &[ch, ch, ch]) {
            let mut end = at + 3;
            while let Some(next) = self.chars.get(end) {
                check_work(self.control)?;
                if starts(self.chars, end, &[ch, ch, ch]) { return Ok(Some(Literal::End(end + 3))); }
                end += if next == '\\' { 2 } else { 1 };
            }
            return Err(());
        }
        if self.dialect == Dialect::Rust {
            let prefix = if ch == 'r' {
                Some(at + 1)
            } else if matches!(ch, 'b' | 'c') && self.chars.get(at + 1) == Some('r') {
                Some(at + 2)
            } else {
                None
            };
            if let Some(mut quote) = prefix {
                while self.chars.get(quote) == Some('#') {
                    check_work(self.control)?;
                    quote += 1;
                }
                if self.chars.get(quote) == Some('"') {
                    let hashes = quote - prefix.unwrap();
                    return skip_rust_raw(self.chars, quote + 1, hashes, self.control)
                        .map(Literal::End)
                        .map(Some);
                }
            }
        }
        if self.dialect == Dialect::Cpp {
            const PREFIXES: &[&[char]] = &[
                &['R', '"'],
                &['u', 'R', '"'],
                &['U', 'R', '"'],
                &['L', 'R', '"'],
                &['u', '8', 'R', '"'],
            ];
            for &prefix in PREFIXES {
                if starts(self.chars, at, prefix) {
                    let from = at + prefix.len();
                    let mut end = from;
                    while let Some(ch) = self.chars.get(end) {
                        if ch == '(' {
                            break;
                        }
                        if end - from >= 16 || ch.is_whitespace() || matches!(ch, ')' | '\\') {
                            return Err(());
                        }
                        end += 1;
                    }
                    if self.chars.get(end) != Some('(') {
                        return Err(());
                    }
                    let mut delimiter = vec![')'];
                    delimiter.extend((from..end).filter_map(|index| self.chars.get(index)));
                    delimiter.push('"');
                    return skip_raw(self.chars, end + 1, &delimiter, self.control)
                        .map(Literal::End)
                        .map(Some);
                }
            }
        }
        if self.dialect == Dialect::CSharp && matches!(ch, '$' | '@' | '"') {
            let mut quote = at;
            let mut dollars = 0;
            let mut verbatim = false;
            while let Some(prefix) = self.chars.get(quote) {
                check_work(self.control)?;
                if prefix == '$' {
                    dollars += 1;
                } else if prefix == '@' && !verbatim {
                    verbatim = true;
                } else {
                    break;
                }
                quote += 1;
            }
            if self.chars.get(quote) == Some('"') {
                let mut quotes = 0;
                while self.chars.get(quote + quotes) == Some('"') {
                    check_work(self.control)?;
                    quotes += 1;
                }
                if quotes >= 3 {
                    // Raw interpolated strings can contain arbitrary raw
                    // strings in holes. Without a full C# grammar, abstain.
                    if dollars > 0 || verbatim {
                        return Err(());
                    }
                    return skip_quote_run(self.chars, quote + quotes, quotes, self.control)
                        .map(Literal::End)
                        .map(Some);
                }
                if dollars > 0 {
                    return Ok(Some(Literal::Template(TemplateState {
                        at: quote + 1,
                        quote: '"',
                        verbatim,
                        javascript: false,
                    })));
                }
                return skip_quoted(self.chars, quote + 1, '"', verbatim, verbatim, self.control)
                    .map(Literal::End)
                    .map(Some);
            }
        }
        if self.dialect == Dialect::Java && starts(self.chars, at, &['"', '"', '"']) {
            return skip_java_text_block(self.chars, at + 3, self.control)
                .map(Literal::End)
                .map(Some);
        }
        if ch == '`' {
            return match self.dialect {
                Dialect::Go => skip_raw(self.chars, at + 1, &['`'], self.control)
                    .map(Literal::End)
                    .map(Some),
                Dialect::JavaScript => Ok(Some(Literal::Template(TemplateState {
                    at: at + 1,
                    quote: '`',
                    verbatim: false,
                    javascript: true,
                }))),
                _ => Ok(None),
            };
        }
        if ch == '\'' && self.dialect == Dialect::Rust {
            if let Some(end) = rust_char_end(self.chars, at, self.control) {
                return Ok(Some(Literal::End(end)));
            }
            // A lifetime/label is an identifier, not an unclosed quote. An
            // unterminated character beginning with punctuation is ambiguous.
            return if self
                .chars
                .get(at + 1)
                .is_some_and(|ch| ch.is_alphabetic() || ch == '_')
            {
                Ok(None)
            } else {
                Err(())
            };
        }
        if matches!(ch, '\'' | '"') {
            return skip_quoted(
                self.chars,
                at + 1,
                ch,
                false,
                self.dialect == Dialect::Rust,
                self.control,
            )
            .map(Literal::End)
            .map(Some);
        }
        Ok(None)
    }

    fn interpolated_end(&self, state: TemplateState) -> Result<usize, ()> {
        enum Action {
            Code(usize),
            Text(TemplateState),
            Complete(usize),
        }
        let mut frames = vec![TemplateFrame::Text(state)];
        loop {
            let action = match frames.last_mut().unwrap() {
                TemplateFrame::Text(state) => loop {
                    check_work(self.control)?;
                    let ch = self.chars.get(state.at).ok_or(())?;
                    if ch == state.quote {
                        if state.verbatim && self.chars.get(state.at + 1) == Some(state.quote) {
                            state.at += 2;
                            continue;
                        }
                        break Action::Complete(state.at + 1);
                    }
                    if ch == '\\' && !state.verbatim {
                        state.at += 2;
                        continue;
                    }
                    let hole = if state.javascript {
                        starts(self.chars, state.at, &['$', '{'])
                    } else {
                        ch == '{' && self.chars.get(state.at + 1) != Some('{')
                    };
                    if hole {
                        break Action::Code(state.at + if state.javascript { 2 } else { 1 });
                    }
                    if !state.javascript
                        && matches!(ch, '{' | '}')
                        && self.chars.get(state.at + 1) == Some(ch)
                    {
                        state.at += 2;
                    } else {
                        state.at += 1;
                    }
                },
                TemplateFrame::Code { lexer, braces } => match lexer.next_event()? {
                    Some(LexEvent::Brace(_, opening)) => {
                        if opening {
                            *braces += 1;
                        } else {
                            *braces -= 1;
                        }
                        if *braces == 0 {
                            Action::Complete(lexer.index)
                        } else {
                            continue;
                        }
                    }
                    Some(LexEvent::Template(state)) => Action::Text(state),
                    Some(LexEvent::Checkpoint) => continue,
                    Some(LexEvent::Directive(_, _)) => continue,
                    Some(LexEvent::Delimiter(_, _, _)) => continue,
                    None => return Err(()),
                },
            };
            match action {
                Action::Code(index) => frames.push(TemplateFrame::Code {
                    lexer: Lexer::controlled(self.chars, self.dialect, index, self.control),
                    braces: 1,
                }),
                Action::Text(state) => frames.push(TemplateFrame::Text(state)),
                Action::Complete(end) => {
                    frames.pop();
                    match frames.last_mut() {
                        Some(TemplateFrame::Text(state)) => state.at = end,
                        Some(TemplateFrame::Code { lexer, .. }) => {
                            lexer.index = end;
                            lexer.regex_allowed = false;
                            lexer.control_word = false;
                        }
                        None => return Ok(end),
                    }
                }
            }
        }
    }
}

fn starts(chars: &CharInput<'_>, at: usize, needle: &[char]) -> bool {
    needle
        .iter()
        .enumerate()
        .all(|(offset, &ch)| chars.get(at.saturating_add(offset)) == Some(ch))
}

fn skip_line_comment(
    chars: &CharInput<'_>,
    mut at: usize,
    dialect: Dialect,
    control: Option<&ScanControl<'_>>,
) -> Result<usize, ()> {
    while let Some(ch) = chars.get(at) {
        check_work(control)?;
        at += 1;
        if ch == '\n' {
            let previous =
                at.saturating_sub(2 + usize::from(at >= 2 && chars.get(at - 2).unwrap() == '\r'));
            if !matches!(dialect, Dialect::C | Dialect::Cpp) || chars.get(previous) != Some('\\') {
                break;
            }
        }
    }
    Ok(at)
}

fn skip_block_comment(
    chars: &CharInput<'_>,
    mut at: usize,
    dialect: Dialect,
    control: Option<&ScanControl<'_>>,
) -> Result<usize, ()> {
    let mut depth = 1;
    while at < chars.len() {
        check_work(control)?;
        if starts(chars, at, &['*', '/']) {
            depth -= 1;
            at += 2;
            if depth == 0 {
                return Ok(at);
            }
        } else if dialect == Dialect::Rust && starts(chars, at, &['/', '*']) {
            depth += 1;
            at += 2;
        } else {
            at += 1;
        }
    }
    Err(())
}

fn skip_quoted(
    chars: &CharInput<'_>,
    mut at: usize,
    quote: char,
    verbatim: bool,
    multiline: bool,
    control: Option<&ScanControl<'_>>,
) -> Result<usize, ()> {
    while let Some(ch) = chars.get(at) {
        check_work(control)?;
        if ch == quote {
            if verbatim && chars.get(at + 1) == Some(quote) {
                at += 2;
            } else {
                return Ok(at + 1);
            }
        } else if ch == '\\' && !verbatim {
            at += 2;
        } else if ch == '\n' && !multiline {
            return Err(());
        } else {
            at += 1;
        }
    }
    Err(())
}

fn skip_rust_raw(
    chars: &CharInput<'_>,
    mut at: usize,
    hashes: usize,
    control: Option<&ScanControl<'_>>,
) -> Result<usize, ()> {
    while let Some(ch) = chars.get(at) {
        check_work(control)?;
        at += 1;
        if ch == '"' {
            let mut count = 0;
            while count < hashes && chars.get(at) == Some('#') {
                check_work(control)?;
                count += 1;
                at += 1;
            }
            if count == hashes {
                return Ok(at);
            }
        }
    }
    Err(())
}

fn skip_raw(
    chars: &CharInput<'_>,
    mut at: usize,
    delimiter: &[char],
    control: Option<&ScanControl<'_>>,
) -> Result<usize, ()> {
    while at < chars.len() {
        check_work(control)?;
        if starts(chars, at, delimiter) {
            return Ok(at + delimiter.len());
        }
        at += 1;
    }
    Err(())
}

fn skip_java_text_block(
    chars: &CharInput<'_>,
    mut at: usize,
    control: Option<&ScanControl<'_>>,
) -> Result<usize, ()> {
    while at < chars.len() {
        check_work(control)?;
        if starts(chars, at, &['"', '"', '"']) {
            return Ok(at + 3);
        }
        at += if chars.get(at).unwrap() == '\\' { 2 } else { 1 };
    }
    Err(())
}

fn skip_quote_run(
    chars: &CharInput<'_>,
    mut at: usize,
    quotes: usize,
    control: Option<&ScanControl<'_>>,
) -> Result<usize, ()> {
    let mut run = 0;
    while let Some(ch) = chars.get(at) {
        check_work(control)?;
        at += 1;
        run = if ch == '"' { run + 1 } else { 0 };
        if run == quotes {
            return Ok(at);
        }
    }
    Err(())
}

fn rust_char_end(
    chars: &CharInput<'_>,
    at: usize,
    control: Option<&ScanControl<'_>>,
) -> Option<usize> {
    let mut end = at + 1;
    if chars.get(end) == Some('\\') {
        end += 1;
        match chars.get(end)? {
            'u' if chars.get(end + 1) == Some('{') => {
                end += 2;
                let mut digits = 0;
                while chars
                    .get(end)
                    .is_some_and(|ch| ch.is_ascii_hexdigit() || ch == '_')
                {
                    check_work(control).ok()?;
                    if chars.get(end)?.is_ascii_hexdigit() {
                        digits += 1;
                        if digits > 6 {
                            return None;
                        }
                    }
                    end += 1;
                }
                if digits == 0 || chars.get(end) != Some('}') {
                    return None;
                }
            }
            'x' => end += 2,
            _ => {}
        }
    } else if chars
        .get(end)
        .is_none_or(|ch| matches!(ch, '\n' | '\r' | '\''))
    {
        return None;
    }
    end += 1;
    (chars.get(end) == Some('\'')).then_some(end + 1)
}

fn skip_regex(
    chars: &CharInput<'_>,
    mut at: usize,
    control: Option<&ScanControl<'_>>,
) -> Result<usize, ()> {
    let mut class = false;
    while let Some(ch) = chars.get(at) {
        check_work(control)?;
        match ch {
            '\\' => at += 2,
            '[' => {
                class = true;
                at += 1;
            }
            ']' => {
                class = false;
                at += 1;
            }
            '/' if !class => {
                at += 1;
                while chars.get(at).is_some_and(|ch| ch.is_ascii_alphabetic()) {
                    check_work(control)?;
                    at += 1;
                }
                return Ok(at);
            }
            '\n' | '\r' => return Err(()),
            _ => at += 1,
        }
    }
    Err(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(editor: &Editor) -> StructuralScopeIndex {
        StructuralScopeRequest::from_editor(editor)
            .unwrap()
            .build(None, || false)
            .unwrap()
    }

    fn assert_same_index(first: &StructuralScopeIndex, second: &StructuralScopeIndex) {
        let nodes = |index: &StructuralScopeIndex| {
            index
                .nodes
                .iter()
                .map(|node| (node.start, node.end, node.parent))
                .collect::<Vec<_>>()
        };
        assert_eq!(nodes(first), nodes(second));
        assert_eq!(first.coverage, second.coverage);
    }

    #[test]
    fn document_index_queries_deep_positions_and_arbitrary_scope_depth() {
        let prefix = "// known source prefix with enough padding\n".repeat(2_000);
        let source = format!("{prefix}fn run() {{\n  if ready {{\n    work();\n  }}\n}}\n");
        let sample = editor(&source, "Rust", 2_002, 5);
        assert_eq!(enclosing_scope(&sample), None);
        let cached = index(&sample);
        assert_eq!(
            cached.enclosing_scope(&sample),
            Some(((2_001, 11), (2_003, 2)))
        );

        let source = format!(
            "{{\n{}work();\n{}outer();\n}}\n",
            "{\n".repeat(1_000),
            "}\n".repeat(1_000)
        );
        let mut sample = editor(&source, "Rust", 1_001, 2);
        let cached = index(&sample);
        assert_eq!(
            cached.enclosing_scope(&sample),
            Some(((1_000, 0), (1_002, 0)))
        );
        sample.cursor_line = 2_002;
        assert_eq!(cached.enclosing_scope(&sample), Some(((0, 0), (2_003, 0))));
    }

    #[test]
    fn worker_index_matches_prefix_lexer_for_each_supported_literal_dialect() {
        for (language, source) in [
            (
                "Rust",
                "fn run<'a>() {\n /* } /* { */ } */\n let r = r#\" } \" { \"#;\n let c = '\\u{7b}';\n if yes {\n work();\n }\n}\n",
            ),
            (
                "C++",
                "void run() {\n auto raw = R\"tag( } \" { )tag\";\n // { } \\\n } {\n work();\n}\n",
            ),
            (
                "TypeScript",
                "function run() {\n const re = /[{}]/g;\n const t = ` } ${ { value: `nested ${ { x: 1 } }` } } { `;\n if (ok) /[}]/.test(t);\n work();\n}\n",
            ),
            (
                "C#",
                "class Run {\n string s = @\" } \"\" {\n }\";\n string t = $\" {new { X = \"}\" }} {{ \";\n void Work() {}\n}\n",
            ),
            (
                "Java",
                "class Run {\n String t = \"\"\" } {\n } \"\"\";\n void work() {}\n}\n",
            ),
            ("Go", "func run() {\n s := ` } {\n } `\n run()\n}\n"),
            ("JSON", "{\n \"界\": {\n \"text\": \"\\\" } {\"\n }\n}\n"),
            (
                "CSS",
                "body {\n content: \" } {\";\n .nested {\n color: red;\n }\n}\n",
            ),
        ] {
            let mut sample = editor(source, language, 0, 0);
            let cached = index(&sample);
            for line in 0..sample.line_count() {
                for col in 0..sample.rope.line(line).len_chars() {
                    sample.cursor_line = line;
                    sample.cursor_col = col;
                    assert_eq!(
                        cached.enclosing_scope(&sample),
                        enclosing_scope(&sample),
                        "{language} {line}:{col}"
                    );
                }
            }
        }
    }

    #[test]
    fn bottom_edit_reuses_safe_source_prefix_and_clears_old_ancestor_ends() {
        let mut sample = editor(
            &"fn item() {\n  work();\n}\n".repeat(5_000),
            "Rust",
            14_998,
            3,
        );
        let previous = index(&sample);
        let offset = sample.rope.line_to_char(14_999);
        sample
            .rope
            .insert(offset, "  if ready {\n    nested();\n  }\n");
        sample.content_version += 1;
        sample.wrap_revision += 1;
        let request = StructuralScopeRequest::from_editor(&sample).unwrap();
        let reused = request.build(Some(&previous), || false).unwrap();
        let fresh = request.build(None, || false).unwrap();
        assert_same_index(&reused, &fresh);
        assert!(reused.restart > sample.rope.len_chars() * 9 / 10);
        assert!(
            reused.work * 10 < fresh.work,
            "reused={}, fresh={}, restart={}",
            reused.work,
            fresh.work,
            reused.restart
        );
        for line in [0, 100, 8_000, 14_998, 14_999, 15_000, 15_002] {
            sample.cursor_line = line;
            assert_eq!(
                reused.enclosing_scope(&sample),
                fresh.enclosing_scope(&sample)
            );
        }
    }

    #[test]
    fn unchanged_source_reuses_lexical_work_after_renaming_or_a_language_alias_change() {
        let source = format!("fn run() {{\n{}}}\n", "    work();\n".repeat(1_000));
        let mut sample = editor(&source, "Rust", 1, 4);
        let previous = index(&sample);
        sample.file_path = Some("renamed.rs".into());
        sample.language_override = Some("rust".into());
        assert!(!previous.matches(&sample));

        let request = StructuralScopeRequest::from_editor(&sample).unwrap();
        let reused = request.build(Some(&previous), || false).unwrap();
        let fresh = request.build(None, || false).unwrap();
        assert_same_index(&reused, &fresh);
        assert_eq!(reused.restart, sample.rope.len_chars());
        assert_eq!(reused.work, 0);
        assert!(reused.matches(&sample));
        assert_eq!(
            reused.enclosing_scope(&sample),
            fresh.enclosing_scope(&sample)
        );
    }

    #[test]
    fn append_relexes_partial_identifiers_and_raw_string_prefixes() {
        for (language, prefix, suffix) in [
            (
                "JavaScript",
                format!("function run() {{\n{}retur", "x();\n".repeat(2_000)),
                "n /[{]/;\nwork();\n}\n",
            ),
            (
                "Rust",
                format!("fn run() {{\n{}let raw = r#", "let x = 0;\n".repeat(1_000)),
                "\"a \" } {\n b\"#;\nwork();\n}\n",
            ),
        ] {
            let mut sample = editor(&prefix, language, 0, 0);
            let previous = index(&sample);
            sample.rope.insert(sample.rope.len_chars(), suffix);
            sample.content_version += 1;
            let request = StructuralScopeRequest::from_editor(&sample).unwrap();
            let reused = request.build(Some(&previous), || false).unwrap();
            let fresh = request.build(None, || false).unwrap();
            assert_same_index(&reused, &fresh);
            sample.cursor_line = sample.line_count() - 3;
            assert!(reused.enclosing_scope(&sample).is_some(), "{language}");
        }
    }

    #[test]
    fn reused_indexes_match_fresh_analysis_through_delimiter_and_newline_edits() {
        for language in ["Rust", "JavaScript"] {
            let source = format!(
                "{}\n{}\n}}\n",
                if language == "Rust" {
                    "fn run() {"
                } else {
                    "function run() {"
                },
                "  work(); // padding\n".repeat(1_000)
            );
            let mut sample = editor(&source, language, 0, 0);
            let mut previous = index(&sample);
            for step in 0..24 {
                let count = sample.rope.len_chars();
                let at = (count / 2 + step * 977) % count;
                match step % 6 {
                    0 => sample.rope.insert(at, "{\n nested();\n}\n"),
                    1 => sample.rope.remove(at..at + 1),
                    2 => sample.rope.insert(at, "/* } { */"),
                    3 => sample.rope.insert(at, "\" { } \""),
                    4 => sample.rope.insert(at, "\n"),
                    _ => sample.rope.insert(
                        at,
                        if language == "Rust" {
                            "r#\" } {\"#"
                        } else {
                            "`${ { x: 1 } }`"
                        },
                    ),
                }
                sample.content_version += 1;
                sample.wrap_revision += 1;
                let request = StructuralScopeRequest::from_editor(&sample).unwrap();
                let reused = request.build(Some(&previous), || false).unwrap();
                let fresh = request.build(None, || false).unwrap();
                assert_same_index(&reused, &fresh);
                for line in [
                    0,
                    sample.line_count() / 4,
                    sample.line_count() / 2,
                    sample.line_count() - 2,
                ] {
                    sample.cursor_line = line;
                    sample.cursor_col = 2;
                    assert_eq!(
                        reused.enclosing_scope(&sample),
                        fresh.enclosing_scope(&sample),
                        "{language} edit{step} line{line}"
                    );
                }
                previous = reused;
            }
        }
    }

    #[test]
    fn complete_prefix_scopes_survive_uncertain_suffixes_and_source_identity_is_strong() {
        let mut sample = editor(
            "fn run() {\n work();\n}\n let raw = r#\" unfinished {\n",
            "Rust",
            1,
            2,
        );
        let request = StructuralScopeRequest::from_editor(&sample).unwrap();
        let cached = request.build(None, || false).unwrap();
        assert_eq!(cached.enclosing_scope(&sample), Some(((0, 9), (2, 0))));
        let old = sample.rope.clone();
        sample.rope = ropey::Rope::from_str(&old.to_string());
        assert!(!request.matches(&sample));
        assert!(!cached.matches(&sample));
        sample.rope = old;
        sample.wrap_revision += 1;
        assert!(!request.matches(&sample));
        sample.wrap_revision -= 1;
        sample.language_override = Some("Plain Text".into());
        assert!(!request.matches(&sample));
        assert!(StructuralScopeRequest::from_editor(&sample).is_none());
    }

    #[test]
    fn iterative_literal_frames_and_chunk_reads_have_no_nesting_or_size_cutoff() {
        let nested = format!(
            "function run() {{\n let s = {}1{};\n work();\n}}\n",
            "`${".repeat(1_000),
            "}`".repeat(1_000)
        );
        let sample = editor(&nested, "JavaScript", 2, 3);
        assert_eq!(
            index(&sample).enclosing_scope(&sample),
            Some(((0, 15), (3, 0)))
        );
        let sample = editor("fn run() {\r\n 界();\u{2028} next();\r}\n", "Rust", 1, 2);
        let closing = sample.rope.len_chars() - 2;
        let line = sample.rope.char_to_line(closing);
        assert_eq!(
            index(&sample).enclosing_scope(&sample),
            Some(((0, 9), (line, closing - sample.rope.line_to_char(line))))
        );
        let sample = editor(
            "fn run() {\n let c = '\\u{7____________________b}';\n work();\n}\n",
            "Rust",
            2,
            2,
        );
        assert_eq!(
            index(&sample).enclosing_scope(&sample),
            Some(((0, 9), (3, 0)))
        );
    }

    #[test]
    fn cancellation_is_polled_inside_brace_free_tokens_and_nested_literals() {
        let payload = "x".repeat(40_000);
        for (language, source) in [
            ("Rust", " ".repeat(40_000)),
            ("Rust", payload.clone()),
            ("Rust", format!("//{payload}")),
            ("Rust", format!("/*{payload}*/")),
            ("Rust", format!("r#\"{payload}\"#")),
            ("Rust", format!("\"{payload}\"")),
            ("Java", format!("\"\"\"{payload}\"\"\"")),
            ("JavaScript", format!("/[{{}}]/{};", "g".repeat(40_000))),
            ("JavaScript", format!("`${{ `{payload}` }}`")),
            ("C#", "\"".repeat(40_000)),
        ] {
            let sample = editor(&source, language, 0, 0);
            let calls = Cell::new(0);
            let result = StructuralScopeRequest::from_editor(&sample)
                .unwrap()
                .build(None, || {
                    calls.set(calls.get() + 1);
                    calls.get() >= 3
                });
            assert!(
                result.is_none(),
                "cancellation was missed in {language}: {}",
                source.len()
            );
            assert_eq!(calls.get(), 3);
        }
    }

    fn editor(text: &str, language: &str, line: usize, column: usize) -> Editor {
        let mut editor = Editor::new();
        editor.rope = ropey::Rope::from_str(text);
        editor.language_override = Some(language.into());
        editor.cursor_line = line;
        editor.cursor_col = column;
        editor
    }

    #[test]
    fn structural_scope_finds_innermost_block_including_single_line_children() {
        let mut editor = editor(
            "fn main() {\n  if ready {\n    let one = { 1 };\n    use_value();\n  }\n}\n",
            "Rust",
            2,
            18,
        );
        assert_eq!(enclosing_scope(&editor), Some(((2, 14), (2, 18))));
        editor.cursor_line = 0;
        editor.cursor_col = 10;
        assert_eq!(enclosing_scope(&editor), Some(((0, 10), (5, 0))));
        editor.cursor_line = 3;
        assert_eq!(enclosing_scope(&editor), Some(((1, 11), (4, 2))));
    }

    #[test]
    fn structural_scope_skips_rust_lifetimes_nested_comments_raw_and_escaped_literals() {
        let source = "fn borrow<'a>(value: &'a str) {\n  /* } /* { */ } */\n  let raw = br###\" } { \\\"\n  } {{{ \"###;\n  let ch = '\\u{7b}'; let close = '}';\n  let normal = \"\\\" } {\";\n  work(); // }\n}\n";
        let editor = editor(source, "Rust", 6, 3);
        assert_eq!(enclosing_scope(&editor), Some(((0, 30), (7, 0))));
    }

    #[test]
    fn structural_scope_skips_cpp_raw_strings_and_line_comment_continuations() {
        let source = "void main() {\n  auto s = u8R\"tag( } {\n  \" } )tag\";\n  // { } \\\n  } {\n  work();\n}\n";
        let editor = editor(source, "C++", 5, 3);
        assert_eq!(enclosing_scope(&editor), Some(((0, 12), (6, 0))));
    }

    #[test]
    fn structural_scope_skips_js_regex_division_and_nested_template_interpolations() {
        let source = "function run() {\n  const re = /[{}]\\/x{2}/g;\n  const ratio = 10 / 2;\n  const text = ` } ${ { value: `nested ${ { x: 1 } }` } } { `;\n  if (ok) /[}]/.test(text);\n  work();\n}\n";
        let editor = editor(source, "TypeScript", 5, 3);
        assert_eq!(enclosing_scope(&editor), Some(((0, 15), (6, 0))));
    }

    #[test]
    fn structural_scope_skips_csharp_verbatim_interpolation_and_raw_literals() {
        let source = "class Test {\n  string a = @\" } \"\" {\n  }\";\n  string b = $\" {new { X = \"}\" }} {{ \";\n  string c = \"\"\" } \" {\n  } \"\"\";\n  void Work() {}\n}\n";
        let sample = editor(source, "C#", 6, 4);
        assert_eq!(enclosing_scope(&sample), Some(((0, 11), (7, 0))));
        let uncertain = editor(
            "class Test {\n var x = $$\"\"\" {{other}} \"\"\";\n}\n",
            "C#",
            1,
            3,
        );
        assert_eq!(enclosing_scope(&uncertain), None);
    }

    #[test]
    fn structural_scope_handles_go_raw_json_strings_and_unicode_columns() {
        let go = editor(
            "func main() {\n  s := ` } {\n  } `\n  run()\n}\n",
            "Go",
            3,
            3,
        );
        assert_eq!(enclosing_scope(&go), Some(((0, 12), (4, 0))));
        let json = editor(
            "{\n  \"é\": {\n    \"text\": \"\\\" } {\"\n  }\n}\n",
            "JSON",
            2,
            10,
        );
        assert_eq!(enclosing_scope(&json), Some(((1, 7), (3, 2))));
    }

    #[test]
    fn structural_scope_abstains_for_unmatched_truncated_or_uncertain_prefixes() {
        for source in [
            "fn main() {\n work();\n",
            "} fn main() {\n work();\n}\n",
            "fn main() {\n let s = r#\" }\n}\n",
            "fn main() {\n /* }\n}\n",
        ] {
            assert_eq!(enclosing_scope(&editor(source, "Rust", 1, 2)), None);
        }
        let huge_line = format!("fn main() {{\n{}\n}}\n", " ".repeat(MAX_CHARS));
        assert_eq!(enclosing_scope(&editor(&huge_line, "Rust", 1, 2)), None);
        let late = format!(
            "/*\n{}fn main() {{\n work();\n}}\n*/",
            " } {\n".repeat(MAX_LINES)
        );
        assert_eq!(
            enclosing_scope(&editor(&late, "Rust", MAX_LINES + 1, 2)),
            None
        );
        let distant_close = format!("fn main() {{\n{} }}", "\n".repeat(MAX_LINES));
        assert_eq!(enclosing_scope(&editor(&distant_close, "Rust", 1, 0)), None);
    }

    #[test]
    fn structural_scope_honors_language_override_and_declines_unknown_modes() {
        let mut editor = editor("fn main() {\n work();\n}\n", "Rust", 1, 2);
        editor.file_path = Some("code.py".into());
        assert!(enclosing_scope(&editor).is_some());
        editor.language_override = Some("Plain Text".into());
        editor.file_path = Some("code.rs".into());
        assert_eq!(enclosing_scope(&editor), None);
        editor.language_override = None;
        assert!(enclosing_scope(&editor).is_some());
    }
}
