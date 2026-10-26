//! Preprocessor branches are alternatives, not consecutive blocks of code.
//! Keep their brace stacks separate and join only compatible outcomes. We do
//! not evaluate build flags; ambiguous brace structure stays undecorated.

use super::{CharInput, Dialect, LexEvent, Lexer, NO_NODE, Pair, ScanControl, ScopeNode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Directive {
    If,
    Elif,
    Else,
    Endif,
}

pub(super) fn directive(lexer: &mut Lexer<'_>) -> Result<Option<Directive>, ()> {
    lexer.index += 1; // '#'
    loop {
        super::check_work(lexer.control)?;
        match lexer.chars.get(lexer.index) {
            Some(' ' | '\t' | '\r') => lexer.index += 1,
            Some('/') if super::starts(lexer.chars, lexer.index, &['/', '*']) => {
                lexer.index = super::skip_block_comment(
                    lexer.chars,
                    lexer.index + 2,
                    lexer.dialect,
                    lexer.control,
                )?;
            }
            Some('\\') if super::starts(lexer.chars, lexer.index + 1, &['\r', '\n']) => {
                lexer.index += 3;
            }
            Some('\\') if lexer.chars.get(lexer.index + 1) == Some('\n') => {
                lexer.index += 2;
            }
            _ => break,
        }
    }
    let mut word = [0_u8; 8];
    let mut length = 0;
    while let Some(ch) = lexer.chars.get(lexer.index) {
        super::check_work(lexer.control)?;
        if ch == '\\' && lexer.chars.get(lexer.index + 1) == Some('\n') {
            lexer.index += 2;
            continue;
        }
        if ch == '\\' && super::starts(lexer.chars, lexer.index + 1, &['\r', '\n']) {
            lexer.index += 3;
            continue;
        }
        if !(ch.is_ascii_alphanumeric() || ch == '_') {
            break;
        }
        if length < word.len() {
            word[length] = ch as u8;
        }
        length += 1;
        lexer.index += 1;
    }
    let kind = match word.get(..length) {
        Some(b"if" | b"ifdef" | b"ifndef") => Some(Directive::If),
        Some(b"elif" | b"elifdef" | b"elifndef") => Some(Directive::Elif),
        Some(b"else") => Some(Directive::Else),
        Some(b"endif") => Some(Directive::Endif),
        _ => None,
    };
    super::statements::skip_directive(lexer)?;
    Ok(kind)
}

pub(crate) struct KeywordRegions {
    pub(crate) pairs: Vec<Pair>,
    pub(crate) uncertain_before: Option<usize>,
    pub(crate) uncertain_after: Option<usize>,
}

struct KeywordFrame {
    opener: super::Position,
    branch: super::Position,
    branches: Vec<Pair>,
    has_else: bool,
    malformed: bool,
}

pub(crate) fn keyword_regions(
    editor: &crate::editor::Editor,
    first: usize,
    end: usize,
) -> KeywordRegions {
    let source = &editor.rope;
    let limit = source.try_line_to_char(end).unwrap_or(source.len_chars());
    let input = CharInput::prefix(source, limit);
    let dialect = super::dialect(editor, editor.scope_bubble_rules.for_editor(editor)).unwrap_or(Dialect::Cpp);
    let mut lexer = Lexer::new(&input, dialect, source.line_to_char(first));
    let mut stack: Vec<KeywordFrame> = Vec::new();
    let mut result = KeywordRegions {
        pairs: Vec::new(),
        uncertain_before: None,
        uncertain_after: None,
    };
    while let Ok(Some(event)) = lexer.next_index_event() {
        let LexEvent::Directive(offset, kind) = event else {
            continue;
        };
        let line = source.char_to_line(offset);
        let position = (line, offset - source.line_to_char(line));
        // The next directive belongs to its own branch. Include blank body
        // rows, but anchor the old branch at its last row's last visible char.
        let previous_end = || {
            let row = line.saturating_sub(1);
            let col = source
                .line(row)
                .chars()
                .enumerate()
                .filter(|(_, ch)| !ch.is_whitespace())
                .map(|(col, _)| col)
                .last()
                .unwrap_or(0);
            (row, col)
        };
        match kind {
            Directive::If => stack.push(KeywordFrame {
                opener: position,
                branch: position,
                branches: Vec::new(),
                has_else: false,
                malformed: false,
            }),
            Directive::Elif | Directive::Else => {
                if let Some(frame) = stack.last_mut() {
                    frame.malformed |= frame.has_else;
                    frame.has_else |= kind == Directive::Else;
                    frame.branches.push((frame.branch, previous_end()));
                    frame.branch = position;
                } else {
                    result.uncertain_before = Some(line);
                }
            }
            Directive::Endif => {
                if let Some(mut frame) = stack.pop() {
                    if frame.malformed {
                        result.uncertain_after.get_or_insert(frame.opener.0);
                        continue;
                    }
                    // On #endif show the complete conditional. Inside a chain,
                    // prefer the current branch over that enclosing region.
                    result.pairs.push((frame.opener, position));
                    if !frame.branches.is_empty() {
                        frame.branches.push((frame.branch, previous_end()));
                        result.pairs.extend(frame.branches);
                    }
                } else {
                    result.uncertain_before = Some(line);
                }
            }
        }
    }
    if let Some(frame) = stack.first() {
        result.uncertain_after = Some(
            result
                .uncertain_after
                .map_or(frame.opener.0, |line| line.min(frame.opener.0)),
        );
    }
    result
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Outcome {
    base: usize,
    depth: usize,
}

struct Frame {
    offset: usize,
    first_node: usize,
    entry: usize,
    first_head: usize,
    deepest_head: usize,
    outcome: Option<Outcome>,
    compatible: bool,
    has_else: bool,
    undo: Vec<(usize, usize)>,
}

impl Frame {
    fn finish_branch(
        &mut self,
        nodes: &mut [ScopeNode],
        head: usize,
        control: Option<&ScanControl<'_>>,
    ) -> Result<(), ()> {
        let mut base = head;
        let mut depth = 0;
        let mut expected = self.first_head;
        while base != NO_NODE && base >= self.first_node {
            super::check_work(control)?;
            self.compatible &= nodes[base].uncertain_from == NO_NODE;
            if self.outcome.is_some() {
                self.compatible &= expected != NO_NODE && nodes[expected].delimiter == nodes[base].delimiter;
                if expected != NO_NODE { expected = nodes[expected].parent; }
            }
            base = nodes[base].parent;
            depth += 1;
        }
        let outcome = Outcome { base, depth };
        if let Some(first) = self.outcome {
            self.compatible &= first == outcome;
        } else {
            self.outcome = Some(outcome);
            self.first_head = head;
        }
        let depth = |head: usize| {
            if head == NO_NODE {
                0
            } else {
                nodes[head].depth
            }
        };
        if depth(head) > depth(self.deepest_head) {
            self.deepest_head = head;
        }
        for (node, end) in self.undo.drain(..).rev() {
            super::check_work(control)?;
            nodes[node].end = end;
        }
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct Branches {
    frames: Vec<Frame>,
}

impl Branches {
    pub(super) fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub(super) fn close(&mut self, nodes: &mut [ScopeNode], node: usize, end: usize) {
        if let Some(frame) = self.frames.last_mut()
            && node < frame.first_node
        {
            frame.undo.push((node, nodes[node].end));
        }
        nodes[node].end = end;
    }

    pub(super) fn unmatched_close(&mut self) -> bool {
        if let Some(frame) = self.frames.last_mut() {
            frame.compatible = false;
            true
        } else {
            false
        }
    }

    pub(super) fn directive(
        &mut self,
        nodes: &mut [ScopeNode],
        head: &mut usize,
        offset: usize,
        kind: Directive,
        control: Option<&ScanControl<'_>>,
    ) -> Result<(), ()> {
        match kind {
            Directive::If => self.frames.push(Frame {
                offset,
                first_node: nodes.len(),
                entry: *head,
                first_head: NO_NODE,
                deepest_head: *head,
                outcome: None,
                compatible: true,
                has_else: false,
                undo: Vec::new(),
            }),
            Directive::Elif | Directive::Else => {
                if let Some(frame) = self.frames.last_mut() {
                    frame.finish_branch(nodes, *head, control)?;
                    frame.compatible &= !frame.has_else;
                    frame.has_else |= kind == Directive::Else;
                    *head = frame.entry;
                } else {
                    invalidate(nodes, *head, offset, control)?;
                }
            }
            Directive::Endif => {
                let Some(mut frame) = self.frames.pop() else {
                    return invalidate(nodes, *head, offset, control);
                };
                frame.finish_branch(nodes, *head, control)?;
                if !frame.has_else {
                    // An omitted #else still has a path: none of this code.
                    frame.finish_branch(nodes, frame.entry, control)?;
                }
                if frame.compatible {
                    let outcome = frame.outcome.unwrap();
                    let mut node = frame.entry;
                    while node != outcome.base {
                        super::check_work(control)?;
                        self.close(nodes, node, offset);
                        node = nodes[node].parent;
                    }
                    *head = frame.first_head;
                    let mut node = *head;
                    while node != outcome.base {
                        super::check_work(control)?;
                        // Equivalent opening braces in alternatives share a
                        // closer after #endif. Anchor that shared scope at #if,
                        // rather than arbitrarily selecting one alternative.
                        nodes[node].anchor = Some(frame.offset);
                        node = nodes[node].parent;
                    }
                } else {
                    // Different stack shapes cannot be resolved without build
                    // flags. Consume possible closers conservatively; retain
                    // complete local blocks, never invent a cross-branch pair.
                    invalidate(nodes, frame.entry, offset, control)?;
                    *head = frame.deepest_head;
                    invalidate(nodes, *head, offset, control)?;
                    if let Some(parent) = self.frames.last_mut() {
                        parent.compatible = false;
                    }
                }
            }
        }
        Ok(())
    }

    pub(super) fn abandon(
        &mut self,
        nodes: &mut [ScopeNode],
        control: Option<&ScanControl<'_>>,
    ) -> Result<(), ()> {
        while let Some(frame) = self.frames.pop() {
            for (node, end) in frame.undo.into_iter().rev() {
                super::check_work(control)?;
                nodes[node].end = end;
            }
        }
        Ok(())
    }
}

fn invalidate(
    nodes: &mut [ScopeNode],
    mut head: usize,
    offset: usize,
    control: Option<&ScanControl<'_>>,
) -> Result<(), ()> {
    while head != NO_NODE {
        super::check_work(control)?;
        if nodes[head].uncertain_from <= offset {
            break; // This node and all its ancestors were already invalidated.
        }
        nodes[head].uncertain_from = nodes[head].uncertain_from.min(offset);
        head = nodes[head].parent;
    }
    Ok(())
}
