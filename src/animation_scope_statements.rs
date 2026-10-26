//! Complete C-family control statements with an unbraced body. This is a
//! deliberately small statement recognizer, not a semantic language parser.
//! It shares the structural lexer's literal/comment handling and runs once per
//! source revision. Frames replace recursion, including for dangling `else`.

use super::{CharInput, Dialect, Lexer, Literal, ScanControl};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    If,
    Else,
    Loop,
    Switch,
    While,
    Do,
    Constexpr,
    Consteval,
    Case,
    Default,
    Word,
    OpenParen,
    CloseParen,
    OpenBracket,
    CloseBracket,
    OpenBrace,
    CloseBrace,
    Semicolon,
    Colon,
    Question,
    Not,
    Other,
    Barrier,
}

impl Kind {
    fn control(self) -> bool {
        matches!(
            self,
            Self::If | Self::Loop | Self::Switch | Self::While | Self::Do
        )
    }

    fn closing(self) -> Option<Self> {
        match self {
            Self::OpenParen => Some(Self::CloseParen),
            Self::OpenBracket => Some(Self::CloseBracket),
            Self::OpenBrace => Some(Self::CloseBrace),
            _ => None,
        }
    }

    fn is_close(self) -> bool {
        matches!(
            self,
            Self::CloseParen | Self::CloseBracket | Self::CloseBrace
        )
    }
}

#[derive(Clone, Copy)]
struct Token {
    kind: Kind,
    start: usize,
    end: usize,
}

struct Tokens<'a> {
    lexer: Lexer<'a>,
    pending: Option<Token>,
}

impl<'a> Tokens<'a> {
    fn peek(&mut self) -> Result<Option<Token>, ()> {
        if self.pending.is_none() {
            self.pending = self.next()?;
        }
        Ok(self.pending)
    }

    fn consume(&mut self) {
        self.pending = None;
    }

    fn next(&mut self) -> Result<Option<Token>, ()> {
        let lexer = &mut self.lexer;
        let chars = lexer.chars;
        while let Some(ch) = chars.get(lexer.index) {
            super::check_work(lexer.control)?;
            let start = lexer.index;
            if ch.is_whitespace() {
                lexer.index += 1;
                continue;
            }
            if super::starts(chars, start, &['/', '/']) {
                lexer.index =
                    super::skip_line_comment(chars, start + 2, lexer.dialect, lexer.control)?;
                continue;
            }
            if super::starts(chars, start, &['/', '*']) {
                lexer.index =
                    super::skip_block_comment(chars, start + 2, lexer.dialect, lexer.control)?;
                continue;
            }
            // A preprocessor branch or macro is not an ordinary statement.
            // Never connect an unfinished control statement across one.
            if ch == '#' {
                lexer.index += 1;
                skip_directive(lexer)?;
                return Ok(Some(Token {
                    kind: Kind::Barrier,
                    start,
                    end: lexer.index,
                }));
            }
            if let Some(literal) = lexer.literal_end()? {
                lexer.index = match literal {
                    Literal::End(end) => end,
                    Literal::Template(state) => lexer.interpolated_end(state)?,
                };
                return Ok(Some(Token {
                    kind: Kind::Other,
                    start,
                    end: lexer.index,
                }));
            }
            let escaped = lexer.dialect == Dialect::CSharp && ch == '@';
            let kind = if ch.is_alphabetic() || ch == '_' || ch == '$' || escaped {
                // Keywords fit on the stack; ordinary identifiers allocate no
                // strings, regardless of their length or Unicode contents.
                let mut word = [0_u8; 16];
                let mut length = 0;
                if escaped {
                    lexer.index += 1;
                }
                while let Some(ch) = chars.get(lexer.index) {
                    if !(ch.is_alphanumeric() || matches!(ch, '_' | '$')) {
                        break;
                    }
                    super::check_work(lexer.control)?;
                    if length < word.len() {
                        word[length] = if ch.is_ascii() { ch as u8 } else { 0 };
                    }
                    length += 1;
                    lexer.index += 1;
                }
                if lexer.index == chars.len() && chars.len() < chars.source.len_chars() {
                    // A prefix ending in `el` cannot prove that the next token
                    // is not `else`, even though `el` alone is an identifier.
                    return Err(());
                }
                if escaped || length > word.len() {
                    Kind::Word
                } else {
                    match &word[..length] {
                        b"if" => Kind::If,
                        b"else" => Kind::Else,
                        b"for" => Kind::Loop,
                        // C# switch statements require braces; its expression
                        // form follows a value, without a parenthesized header.
                        b"switch" if lexer.dialect == Dialect::CSharp => Kind::Word,
                        b"switch" => Kind::Switch,
                        b"while" => Kind::While,
                        b"do" => Kind::Do,
                        b"constexpr" if lexer.dialect == Dialect::Cpp => Kind::Constexpr,
                        b"consteval" if lexer.dialect == Dialect::Cpp => Kind::Consteval,
                        b"case" => Kind::Case,
                        b"default" => Kind::Default,
                        b"foreach" | b"lock" | b"fixed" if lexer.dialect == Dialect::CSharp => {
                            Kind::Loop
                        }
                        b"synchronized" if lexer.dialect == Dialect::Java => Kind::Loop,
                        _ => Kind::Word,
                    }
                }
            } else {
                lexer.index += 1;
                match ch {
                    '(' => Kind::OpenParen,
                    ')' => Kind::CloseParen,
                    '[' => Kind::OpenBracket,
                    ']' => Kind::CloseBracket,
                    '{' => Kind::OpenBrace,
                    '}' => Kind::CloseBrace,
                    ';' => Kind::Semicolon,
                    ':' if chars.get(lexer.index) == Some(':') => {
                        lexer.index += 1;
                        Kind::Other
                    }
                    ':' => Kind::Colon,
                    '?' => Kind::Question,
                    '!' => Kind::Not,
                    _ => Kind::Other,
                }
            };
            return Ok(Some(Token {
                kind,
                start,
                end: lexer.index,
            }));
        }
        Ok(None)
    }
}

pub(super) fn skip_directive(lexer: &mut Lexer<'_>) -> Result<(), ()> {
    // A comment is replaced by whitespace before preprocessing, so a block
    // comment's physical newlines do not end the directive. Raw strings may
    // also contain newlines. A plain line skipper would expose macro bodies.
    while let Some(ch) = lexer.chars.get(lexer.index) {
        super::check_work(lexer.control)?;
        if ch == '\n' {
            lexer.index += 1;
            break;
        }
        if super::starts(lexer.chars, lexer.index, &['/', '/']) {
            lexer.index = super::skip_line_comment(
                lexer.chars,
                lexer.index + 2,
                Dialect::Cpp,
                lexer.control,
            )?;
            break;
        }
        if super::starts(lexer.chars, lexer.index, &['/', '*']) {
            lexer.index = super::skip_block_comment(
                lexer.chars,
                lexer.index + 2,
                lexer.dialect,
                lexer.control,
            )?;
            continue;
        }
        if ch == '\\' {
            let next = lexer.index + 1;
            if lexer.chars.get(next) == Some('\n') {
                lexer.index += 2;
                continue;
            }
            if super::starts(lexer.chars, next, &['\r', '\n']) {
                lexer.index += 3;
                continue;
            }
        }
        if let Some(literal) = lexer.literal_end()? {
            lexer.index = match literal {
                Literal::End(end) => end,
                Literal::Template(state) => lexer.interpolated_end(state)?,
            };
        } else {
            lexer.index += 1;
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Arm {
    Then,
    Else,
}

enum Stage {
    HeaderStart { tail: bool },
    NegatedConsteval,
    Header { closes: Vec<Kind>, tail: bool },
    Body(Arm),
    Attributes { closes: Vec<Kind>, arm: Arm },
    Waiting { arm: Arm, braced: bool },
    AfterThen(usize),
    AfterDo,
    DoSemicolon,
    Complete(usize),
}

struct Control {
    kind: Kind,
    start: usize,
    slot: usize,
    unbraced: bool,
    block_only: bool,
    else_scope: Option<(usize, usize)>,
    stage: Stage,
}

impl Control {
    fn new(token: Token, slots: &mut Vec<Option<(usize, usize)>>) -> Self {
        let slot = slots.len();
        slots.push(None);
        Self {
            kind: token.kind,
            start: token.start,
            slot,
            unbraced: false,
            block_only: false,
            else_scope: None,
            stage: if token.kind == Kind::Do {
                Stage::Body(Arm::Then)
            } else {
                Stage::HeaderStart { tail: false }
            },
        }
    }
}

enum Frame {
    Root,
    Block,
    Control(Control),
    Simple { closes: Vec<Kind>, label: Label },
}

enum Label {
    Initial,
    Name,
    Case(usize),
    Disabled,
}

/// Notify the owner after a nested statement/block reaches its genuine end.
fn completed(frames: &mut [Frame], end: usize) {
    if let Some(Frame::Control(control)) = frames.last_mut()
        && let Stage::Waiting { arm, braced } = control.stage
    {
        control.unbraced |= !braced;
        control.stage = match (control.kind, arm) {
            (Kind::If, Arm::Then) => Stage::AfterThen(end),
            (Kind::Do, _) => Stage::AfterDo,
            _ => Stage::Complete(end),
        };
    }
}

fn record(
    source: &ropey::Rope,
    slots: &mut [Option<(usize, usize)>],
    slot: usize,
    start: usize,
    end: usize,
) {
    if source.char_to_line(start) != source.char_to_line(end) {
        slots[slot] = Some((start, end));
    }
}

/// Inclusive character offsets, ordered by start. A truncated prefix never
/// acts as an implicit terminator (or evidence that an `if` has no `else`).
pub(super) fn scan(
    source: &ropey::Rope,
    limit: usize,
    dialect: Dialect,
    cancelled: &dyn Fn() -> bool,
) -> Option<Vec<(usize, usize)>> {
    if !matches!(
        dialect,
        Dialect::C | Dialect::Cpp | Dialect::Java | Dialect::CSharp
    ) {
        return Some(Vec::new());
    }
    let chars = CharInput::prefix(source, limit);
    let control = ScanControl::new(cancelled);
    let mut tokens = Tokens {
        lexer: Lexer::controlled(&chars, dialect, 0, Some(&control)),
        pending: None,
    };
    let mut frames = vec![Frame::Root];
    // Reserving slots at the opening keyword gives sorted output without an
    // uncancellable sort or a second token buffer the size of the document.
    let mut slots = Vec::new();
    loop {
        control.tick().ok()?;
        if let Some(Frame::Control(statement)) = frames.last()
            && let Stage::Complete(end) = statement.stage
        {
            if statement.unbraced {
                record(source, &mut slots, statement.slot, statement.start, end);
            }
            if let Some((slot, start)) = statement.else_scope {
                record(source, &mut slots, slot, start, end);
            }
            frames.pop();
            completed(&mut frames, end);
            continue;
        }
        let token = match tokens.peek() {
            Ok(Some(token)) => token,
            Ok(None) => {
                // At true EOF a completed then arm cannot gain an else. At a
                // prefix boundary that decision belongs to the worker scan.
                if chars.len() == source.len_chars()
                    && let Some(Frame::Control(statement)) = frames.last_mut()
                    && let Stage::AfterThen(end) = statement.stage
                {
                    statement.stage = Stage::Complete(end);
                    continue;
                }
                break;
            }
            Err(()) => break,
        };
        if token.kind == Kind::Barrier {
            // A terminated then arm is already a complete local scope. Keep
            // it before #elif/#else/#endif; only unfinished bodies must be
            // discarded, and an else in another branch cannot extend it.
            if let Some(Frame::Control(statement)) = frames.last_mut()
                && let Stage::AfterThen(end) = statement.stage
            {
                statement.stage = Stage::Complete(end);
                continue;
            }
            // Drop pending scopes and block context; subsequent complete
            // statements remain discoverable even in another #if branch.
            frames.clear();
            frames.push(Frame::Root);
            tokens.consume();
            continue;
        }
        match frames.last_mut().unwrap() {
            Frame::Root | Frame::Block => {
                if token.kind.control() {
                    frames.push(Frame::Control(Control::new(token, &mut slots)));
                } else if token.kind == Kind::OpenBrace {
                    frames.push(Frame::Block);
                } else if token.kind == Kind::CloseBrace && frames.len() > 1 {
                    frames.pop();
                    completed(&mut frames, token.end - 1);
                }
                tokens.consume();
            }
            Frame::Control(statement) => match &mut statement.stage {
                Stage::HeaderStart { tail } => {
                    if !*tail && statement.kind == Kind::If && token.kind == Kind::Constexpr {
                        tokens.consume();
                        continue;
                    }
                    if !*tail && statement.kind == Kind::If {
                        if token.kind == Kind::Consteval {
                            statement.block_only = true;
                            statement.stage = Stage::Body(Arm::Then);
                            tokens.consume();
                            continue;
                        }
                        if token.kind == Kind::Not && dialect == Dialect::Cpp {
                            statement.stage = Stage::NegatedConsteval;
                            tokens.consume();
                            continue;
                        }
                    }
                    if token.kind != Kind::OpenParen {
                        break;
                    }
                    statement.stage = Stage::Header {
                        closes: vec![Kind::CloseParen],
                        tail: *tail,
                    };
                    tokens.consume();
                }
                Stage::NegatedConsteval => {
                    if token.kind != Kind::Consteval {
                        break;
                    }
                    statement.block_only = true;
                    statement.stage = Stage::Body(Arm::Then);
                    tokens.consume();
                }
                Stage::Header { closes, tail } => {
                    if let Some(close) = token.kind.closing() {
                        closes.push(close);
                    } else if token.kind.is_close() {
                        if closes.pop() != Some(token.kind) {
                            break;
                        }
                        if closes.is_empty() {
                            statement.stage = if *tail {
                                Stage::DoSemicolon
                            } else {
                                Stage::Body(Arm::Then)
                            };
                        }
                    }
                    tokens.consume();
                }
                Stage::Body(arm) => {
                    if dialect == Dialect::Cpp
                        && token.kind == Kind::OpenBracket
                        && chars.get(token.end) == Some('[')
                    {
                        statement.stage = Stage::Attributes {
                            closes: vec![Kind::CloseBracket],
                            arm: *arm,
                        };
                        tokens.consume();
                        continue;
                    }
                    if token.kind.is_close() || token.kind == Kind::Else {
                        break;
                    }
                    let braced = token.kind == Kind::OpenBrace;
                    if statement.block_only && !braced {
                        break;
                    }
                    if matches!(arm, Arm::Else) && braced {
                        statement.else_scope = None;
                    }
                    statement.stage = Stage::Waiting { arm: *arm, braced };
                    if braced {
                        frames.push(Frame::Block);
                        tokens.consume();
                    } else if token.kind.control() {
                        frames.push(Frame::Control(Control::new(token, &mut slots)));
                        tokens.consume();
                    } else {
                        frames.push(Frame::Simple {
                            closes: Vec::new(),
                            label: Label::Initial,
                        });
                    }
                }
                Stage::Attributes { closes, arm } => {
                    if let Some(close) = token.kind.closing() {
                        closes.push(close);
                    } else if token.kind.is_close() {
                        if closes.pop() != Some(token.kind) {
                            break;
                        }
                        if closes.is_empty() {
                            statement.stage = Stage::Body(*arm);
                        }
                    }
                    tokens.consume();
                }
                Stage::AfterThen(end) => {
                    if token.kind == Kind::Else {
                        let slot = slots.len();
                        slots.push(None);
                        statement.else_scope = Some((slot, token.start));
                        statement.stage = Stage::Body(Arm::Else);
                        tokens.consume();
                    } else {
                        statement.stage = Stage::Complete(*end);
                    }
                }
                Stage::AfterDo => {
                    if token.kind != Kind::While {
                        break;
                    }
                    statement.stage = Stage::HeaderStart { tail: true };
                    tokens.consume();
                }
                Stage::DoSemicolon => {
                    if token.kind != Kind::Semicolon {
                        break;
                    }
                    statement.stage = Stage::Complete(token.end - 1);
                    tokens.consume();
                }
                Stage::Waiting { .. } | Stage::Complete(_) => unreachable!("active child"),
            },
            Frame::Simple { closes, label } => {
                if (token.kind.control()
                    && !(token.kind == Kind::Switch && dialect == Dialect::Java))
                    || token.kind == Kind::Else
                {
                    // A missing terminator must not borrow the semicolon
                    // from a subsequent branch/control statement.
                    break;
                }
                if closes.is_empty()
                    && token.kind == Kind::Colon
                    && matches!(label, Label::Name | Label::Case(0))
                {
                    // An ordinary label prefixes a statement without changing
                    // where that statement ends, including a nested if/else.
                    frames.pop();
                    if let Some(Frame::Control(statement)) = frames.last_mut()
                        && let Stage::Waiting { arm, .. } = statement.stage
                    {
                        statement.stage = Stage::Body(arm);
                    }
                    tokens.consume();
                    continue;
                }
                match label {
                    Label::Initial => {
                        *label = match token.kind {
                            Kind::Word | Kind::Default => Label::Name,
                            Kind::Case => Label::Case(0),
                            _ => Label::Disabled,
                        };
                    }
                    Label::Case(ternaries) if closes.is_empty() => match token.kind {
                        Kind::Question => *ternaries += 1,
                        Kind::Colon if *ternaries > 0 => *ternaries -= 1,
                        Kind::Semicolon => break,
                        _ => {}
                    },
                    Label::Name => *label = Label::Disabled,
                    _ => {}
                }
                if token.kind == Kind::OpenBrace {
                    // Lambda bodies and braced initializers do not terminate
                    // the containing expression statement at their `}`.
                    frames.push(Frame::Block);
                } else if let Some(close) = token.kind.closing() {
                    closes.push(close);
                } else if token.kind.is_close() {
                    if closes.pop() != Some(token.kind) {
                        break;
                    }
                } else if token.kind == Kind::Semicolon && closes.is_empty() {
                    frames.pop();
                    completed(&mut frames, token.end - 1);
                }
                tokens.consume();
            }
        }
    }
    if control.was_cancelled.get() || cancelled() {
        return None;
    }
    let mut pairs = Vec::new();
    for pair in slots {
        control.tick().ok()?;
        if let Some(pair) = pair {
            pairs.push(pair);
        }
    }
    Some(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn spans(text: &str, dialect: Dialect) -> Vec<String> {
        let source = ropey::Rope::from_str(text);
        scan(&source, source.len_chars(), dialect, &|| false)
            .unwrap()
            .into_iter()
            .map(|(start, end)| source.slice(start..=end).to_string())
            .collect()
    }

    #[test]
    fn unbraced_headers_bodies_and_for_semicolons() {
        let source = "void f() {\nfor (int i = 0;\n     i < 3; ++i)\n    call(i);\nwhile (ready)\n    next();\n}\n";
        assert_eq!(
            spans(source, Dialect::Cpp),
            [
                "for (int i = 0;\n     i < 3; ++i)\n    call(i);",
                "while (ready)\n    next();",
            ]
        );
    }

    #[test]
    fn dangling_else_belongs_to_innermost_if() {
        let source =
            "if (outer)\n    if (inner)\n        yes();\n    else\n        no();\nafter();";
        let scopes = spans(source, Dialect::C);
        assert_eq!(
            scopes,
            [
                "if (outer)\n    if (inner)\n        yes();\n    else\n        no();",
                "if (inner)\n        yes();\n    else\n        no();",
                "else\n        no();",
            ]
        );
    }

    #[test]
    fn mixed_braced_arms_and_else_if_chain() {
        let source = "if (a) {\n    first();\n} else if (b)\n    second();\nelse {\n    last();\n}";
        assert_eq!(
            spans(source, Dialect::Cpp),
            [
                source,
                "else if (b)\n    second();\nelse {\n    last();\n}",
                "if (b)\n    second();\nelse {\n    last();\n}",
            ]
        );
        assert!(spans("if (a) {\n  b();\n} else {\n  c();\n}", Dialect::C).is_empty());
    }

    #[test]
    fn do_while_closes_only_after_its_tail() {
        let source =
            "do\n    if (x)\n        one();\n    else\n        two();\nwhile (again);\nafter();";
        assert_eq!(
            spans(source, Dialect::C),
            [
                "do\n    if (x)\n        one();\n    else\n        two();\nwhile (again);",
                "if (x)\n        one();\n    else\n        two();",
                "else\n        two();",
            ]
        );
        assert!(spans("do {\n  call();\n} while (again);\nafter();", Dialect::C).is_empty());
        assert!(spans("do\n  call();\nwhile (again)", Dialect::C).is_empty());
    }

    #[test]
    fn comments_strings_and_cpp_raw_strings_are_opaque() {
        let source = "if (ok /* ) ; else */)\n    call(R\"tag(; else { if (x)\n )tag\", \"; }\");\n// if (fake)\nnext();";
        assert_eq!(
            spans(source, Dialect::Cpp),
            ["if (ok /* ) ; else */)\n    call(R\"tag(; else { if (x)\n )tag\", \"; }\");",]
        );
    }

    #[test]
    fn braced_initializer_and_lambda_do_not_end_expression_early() {
        let source = "if (ok)\n    call(Value{1, 2}, [] {\n        if (nested)\n            work();\n    });\nafter();";
        assert_eq!(
            spans(source, Dialect::Cpp),
            [
                "if (ok)\n    call(Value{1, 2}, [] {\n        if (nested)\n            work();\n    });",
                "if (nested)\n            work();",
            ]
        );
    }

    #[test]
    fn labels_keep_nested_statement_ownership() {
        let source = "if (outer)\nlabel: if (inner)\n    yes();\nelse\n    no();";
        assert_eq!(spans(source, Dialect::C).first().unwrap(), source);
    }

    #[test]
    fn switch_labels_and_cpp_attributes_keep_body_ownership() {
        let source = "switch (x)\ncase A::value ? 1 : 2:\n    if (enabled)\n        work();";
        assert_eq!(
            spans(source, Dialect::Cpp),
            [source, "if (enabled)\n        work();"]
        );
        let source =
            "if (outer) [[likely]]\n    if (inner)\n        yes();\n    else\n        no();";
        assert_eq!(spans(source, Dialect::Cpp).first().unwrap(), source);
        assert!(spans("if (ok) [[likely]] {\n    work();\n}", Dialect::Cpp).is_empty());
        let source = "switch (x)\ndefault:\n    while (ready)\n        work();";
        assert_eq!(
            spans(source, Dialect::C),
            [source, "while (ready)\n        work();"]
        );
    }

    #[test]
    fn consteval_blocks_do_not_hide_nested_or_subsequent_statements() {
        let source = "if !consteval {\n    if (a)\n        work();\n} else {\n    other();\n}\nif (b)\n    done();";
        assert_eq!(
            spans(source, Dialect::Cpp),
            ["if (a)\n        work();", "if (b)\n    done();"]
        );
    }

    #[test]
    fn java_and_csharp_reuse_language_literals() {
        let java = "for (String x : values)\n    call(\"\"\"\nif (fake);\n\"\"\");";
        assert_eq!(spans(java, Dialect::Java), [java]);
        let csharp = "foreach (var x in values)\n    call($\"{Nested(\";\")}\");\n@if();";
        assert_eq!(
            spans(csharp, Dialect::CSharp),
            ["foreach (var x in values)\n    call($\"{Nested(\";\")}\");",]
        );
        let java = "if (ok)\n    return switch (value) {\n        case 0 -> 1;\n        default -> 2;\n    };";
        assert_eq!(spans(java, Dialect::Java), [java]);
        let csharp = "if (ok)\n    return value switch {\n        0 => 1,\n        _ => 2,\n    };";
        assert_eq!(spans(csharp, Dialect::CSharp), [csharp]);
    }

    #[test]
    fn multiline_header_and_unicode_offsets() {
        let source = "// õ🙂\nif constexpr (\n    enabled) act();\nafter();";
        assert_eq!(
            spans(source, Dialect::Cpp),
            ["if constexpr (\n    enabled) act();"]
        );
        assert!(spans("if (x) act();", Dialect::Cpp).is_empty());
    }

    #[test]
    fn incomplete_bodies_and_prefix_boundaries_are_not_closed() {
        for source in [
            "if (x)\n    call()",
            "if (x)\n",
            "for (;;\n",
            "while(x)\ncall(;",
            "if(x)\ncall()\nelse\nother();",
            "if(x)\ncall()\nwhile(y)\nother();",
        ] {
            assert!(spans(source, Dialect::C).is_empty(), "{source}");
        }
        let source = ropey::Rope::from_str("if (x)\n    first();\nelse\n    second();");
        let limit = source.to_string().find("else").unwrap();
        assert!(
            scan(&source, limit + 2, Dialect::C, &|| false)
                .unwrap()
                .is_empty()
        );
        assert!(
            scan(&source, limit, Dialect::C, &|| false)
                .unwrap()
                .is_empty()
        );
        let source = ropey::Rope::from_str("while (x)\n    call(); after();");
        let limit = source.to_string().find(';').unwrap();
        assert!(
            scan(&source, limit, Dialect::C, &|| false)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn preprocessor_macros_and_branches_do_not_form_statements() {
        let source = "#define RUN(x) if (x) \\\n    do_it();\nif (first)\n#if OPTION\n    a();\n#else\n    b();\n#endif\nif (last)\n    done();";
        assert_eq!(spans(source, Dialect::Cpp), ["if (last)\n    done();"]);
        let source = "#define RUN(x) if (x) \\\r\n    do_it();\r\nif (last)\r\n    done();";
        assert_eq!(spans(source, Dialect::Cpp), ["if (last)\r\n    done();"]);
        let source =
            "#define RUN(x) /* long\ncomment */ if(x) \\\n    do_it();\nif (last)\n    done();";
        assert_eq!(spans(source, Dialect::Cpp), ["if (last)\n    done();"]);
    }

    #[test]
    fn nesting_is_iterative_and_work_is_cancellable() {
        let source = format!("{}\ncall();", "if(x) ".repeat(20_000));
        let source = ropey::Rope::from_str(&source);
        assert_eq!(
            scan(&source, source.len_chars(), Dialect::C, &|| false)
                .unwrap()
                .len(),
            20_000
        );
        let source = ropey::Rope::from_str(&format!("if (x)\ncall(\"{}\");", "a".repeat(50_000)));
        let checks = Cell::new(0);
        let cancelled = || {
            checks.set(checks.get() + 1);
            checks.get() == 3
        };
        assert!(scan(&source, source.len_chars(), Dialect::C, &cancelled).is_none());
        assert_eq!(checks.get(), 3);
    }
}
