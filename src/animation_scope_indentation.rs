//! Python suites, discovered from logical statements rather than physical lines.
//!
//! The worker retains only its Rope chunk, open suites, and completed intervals.
//! Every character is visited a bounded number of times, including long strings
//! and comments; no line-sized strings or document-sized token arrays are built.

use super::{CharInput, ScanControl};

struct Suite {
    indent: usize,
    start: usize,
    line: usize,
}

struct OpenSuite {
    indent: usize,
    node: usize,
}

struct Node {
    start: usize,
    line: usize,
    end: Option<usize>,
}

struct Statement {
    indent: usize,
    start: usize,
    line: usize,
    end: usize,
    end_line: usize,
    complete: bool,
    block: bool,
    inline_suite: bool,
}

/// Return complete multiline suites, in source order. A bounded prefix never
/// invents EOF dedents: only a later statement can close an open prefix suite.
pub(super) fn scan(
    source: &ropey::Rope,
    limit: usize,
    cancelled: &dyn Fn() -> bool,
) -> Option<Vec<(usize, usize)>> {
    if cancelled() {
        return None;
    }
    let control = ScanControl::new(cancelled);
    let input = CharInput::prefix(source, limit);
    let mut lexer = Statements {
        input: &input,
        control: &control,
        at: 0,
        line: 0,
        string_frames: Vec::new(),
    };
    let mut nodes: Vec<Node> = Vec::new();
    let mut open: Vec<OpenSuite> = Vec::new();
    let mut pending: Option<Suite> = None;
    let mut last_end = None;
    let mut complete = true;
    while let Some(statement) = lexer.next().ok()? {
        // The first code character proves the previous suite has ended even
        // when the new statement itself extends beyond a bounded prefix.
        while open
            .last()
            .is_some_and(|suite| statement.indent <= suite.indent)
        {
            control.tick().ok()?;
            let suite = open.pop().unwrap();
            close(&mut nodes[suite.node], last_end);
        }
        if let Some(suite) = pending
            .take()
            .filter(|suite| statement.indent > suite.indent)
        {
            open.push(OpenSuite {
                indent: suite.indent,
                node: nodes.len(),
            });
            nodes.push(Node {
                start: suite.start,
                line: suite.line,
                end: None,
            });
        }
        if !statement.complete {
            complete = false;
            break;
        }
        last_end = Some((statement.end, statement.end_line));
        if statement.block {
            pending = Some(Suite {
                indent: statement.indent,
                start: statement.start,
                line: statement.line,
            });
        } else if statement.inline_suite && statement.end_line > statement.line {
            nodes.push(Node {
                start: statement.start,
                line: statement.line,
                end: Some(statement.end),
            });
        }
    }
    if input.len() == source.len_chars() && complete {
        while let Some(suite) = open.pop() {
            control.tick().ok()?;
            close(&mut nodes[suite.node], last_end);
        }
    }
    let mut result = Vec::with_capacity(nodes.len());
    for node in nodes {
        control.tick().ok()?;
        if let Some(end) = node.end {
            result.push((node.start, end));
        }
    }
    (!cancelled()).then_some(result)
}

fn close(node: &mut Node, last_end: Option<(usize, usize)>) {
    if let Some((end, _)) = last_end.filter(|(_, line)| *line > node.line) {
        node.end = Some(end);
    }
}

/// Only the initial keyword (and the word after `async`) needs to be retained.
/// Long Unicode identifiers therefore use the same bounded scratch storage.
#[derive(Default)]
struct Word {
    bytes: [u8; 8],
    len: usize,
}

impl Word {
    fn push(&mut self, ch: char) {
        if self.len < self.bytes.len() && ch.is_ascii() {
            self.bytes[self.len] = ch as u8;
            self.len += 1;
        } else {
            self.len = self.bytes.len() + 1;
        }
    }

    fn is(&self, word: &[u8]) -> bool {
        self.len == word.len() && self.bytes.get(..self.len) == Some(word)
    }

    fn header(&self) -> bool {
        matches!(
            self.bytes.get(..self.len),
            Some(
                b"def"
                    | b"class"
                    | b"if"
                    | b"elif"
                    | b"else"
                    | b"for"
                    | b"while"
                    | b"try"
                    | b"except"
                    | b"finally"
                    | b"with"
                    | b"match"
                    | b"case"
            )
        )
    }

    fn string_prefix(&self) -> Option<bool> {
        let bytes = self.bytes.get(..self.len)?;
        match bytes {
            [b'r' | b'R' | b'u' | b'U' | b'b' | b'B'] => Some(false),
            [b'f' | b'F' | b't' | b'T'] => Some(true),
            [b'r' | b'R', b'b' | b'B'] | [b'b' | b'B', b'r' | b'R'] => Some(false),
            [b'r' | b'R', b'f' | b'F' | b't' | b'T'] | [b'f' | b'F' | b't' | b'T', b'r' | b'R'] => {
                Some(true)
            }
            _ => None,
        }
    }
}

#[derive(Clone, Copy)]
enum StringFrame {
    Text {
        quote: char,
        triple: bool,
        formatted: bool,
    },
    Expression {
        depth: usize,
    },
    Format,
}

struct Statements<'a> {
    input: &'a CharInput<'a>,
    control: &'a ScanControl<'a>,
    at: usize,
    line: usize,
    string_frames: Vec<StringFrame>,
}

/// Share the Python f-string scanner with configurable delimiter scopes.
/// It is iterative and uses the caller's cancellation/work budget.
pub(super) fn formatted_string_end(input: &CharInput<'_>, at: usize, control: Option<&ScanControl<'_>>) -> Result<usize, ()> {
    let fallback = ScanControl::new(&|| false);
    let mut lexer = Statements { input, control: control.unwrap_or(&fallback), at, line: 0, string_frames: Vec::new() };
    if lexer.string(true)? { Ok(lexer.at) } else { Err(()) }
}

impl Statements<'_> {
    fn get(&self, ahead: usize) -> Option<char> {
        self.input.get(self.at + ahead)
    }

    fn newline(&mut self) {
        self.at += if self.get(0) == Some('\r') && self.get(1) == Some('\n') {
            2
        } else {
            1
        };
        self.line += 1;
    }

    fn comment(&mut self) -> Result<(), ()> {
        while self.get(0).is_some_and(|ch| !matches!(ch, '\n' | '\r')) {
            self.control.tick()?;
            self.at += 1;
        }
        Ok(())
    }

    fn word(&mut self) -> Result<Word, ()> {
        let mut word = Word::default();
        while let Some(ch) = self.get(0).filter(|ch| ch.is_alphanumeric() || *ch == '_') {
            self.control.tick()?;
            word.push(ch);
            self.at += 1;
        }
        Ok(word)
    }

    fn string_frame(&mut self, formatted: bool) -> StringFrame {
        let quote = self.get(0).unwrap();
        let triple = self.get(1) == Some(quote) && self.get(2) == Some(quote);
        self.at += if triple { 3 } else { 1 };
        StringFrame::Text {
            quote,
            triple,
            formatted,
        }
    }

    /// F-string expressions have their own strings, comments, and delimiters.
    /// An explicit stack also handles same-quote nested strings in modern
    /// Python without recursion or mistaking their contents for Python suites.
    fn string(&mut self, formatted: bool) -> Result<bool, ()> {
        // Reuse the frame allocation across literals; documents with many short
        // strings should not allocate once for every token.
        let mut frames = std::mem::take(&mut self.string_frames);
        frames.clear();
        frames.push(self.string_frame(formatted));
        while let Some(ch) = self.get(0) {
            self.control.tick()?;
            match *frames.last().unwrap() {
                StringFrame::Text {
                    quote,
                    triple,
                    formatted,
                } => {
                    if ch == quote
                        && (!triple || (self.get(1) == Some(quote) && self.get(2) == Some(quote)))
                    {
                        self.at += if triple { 3 } else { 1 };
                        frames.pop();
                        if frames.is_empty() {
                            self.string_frames = frames;
                            return Ok(true);
                        }
                    } else if ch == '\\' {
                        self.at += 1;
                        if matches!(self.get(0), Some('\r' | '\n')) {
                            self.newline();
                        } else if self.get(0).is_some()
                            && !(formatted && matches!(self.get(0), Some('{' | '}')))
                        {
                            self.at += 1;
                        }
                    } else if matches!(ch, '\r' | '\n') {
                        if !triple {
                            self.string_frames = frames;
                            return Ok(false);
                        }
                        self.newline();
                    } else if formatted && ch == '{' {
                        if self.get(1) == Some('{') {
                            self.at += 2;
                        } else {
                            self.at += 1;
                            frames.push(StringFrame::Expression { depth: 0 });
                        }
                    } else {
                        self.at += if formatted && ch == '}' && self.get(1) == Some('}') {
                            2
                        } else {
                            1
                        };
                    }
                }
                StringFrame::Expression { depth } => match ch {
                    '\'' | '"' => frames.push(self.string_frame(false)),
                    '#' => self.comment()?,
                    '\r' | '\n' => self.newline(),
                    '(' | '[' | '{' => {
                        self.at += 1;
                        *frames.last_mut().unwrap() = StringFrame::Expression {
                            depth: depth.saturating_add(1),
                        };
                    }
                    '}' if depth == 0 => {
                        self.at += 1;
                        frames.pop();
                    }
                    ')' | ']' | '}' => {
                        self.at += 1;
                        *frames.last_mut().unwrap() = StringFrame::Expression {
                            depth: depth.saturating_sub(1),
                        };
                    }
                    ':' if depth == 0 => {
                        self.at += 1;
                        *frames.last_mut().unwrap() = StringFrame::Format;
                    }
                    ch if ch.is_alphabetic() || ch == '_' => {
                        let word = self.word()?;
                        if let Some(formatted) = word.string_prefix()
                            && matches!(self.get(0), Some('\'' | '"'))
                        {
                            frames.push(self.string_frame(formatted));
                        }
                    }
                    _ => self.at += 1,
                },
                StringFrame::Format => match ch {
                    '{' => {
                        self.at += 1;
                        frames.push(StringFrame::Expression { depth: 0 });
                    }
                    '}' => {
                        self.at += 1;
                        frames.pop();
                    }
                    '\r' | '\n' => self.newline(),
                    _ => self.at += 1,
                },
            }
        }
        self.string_frames = frames;
        Ok(false)
    }

    fn next(&mut self) -> Result<Option<Statement>, ()> {
        let mut indent = 0usize;
        // Comments and blank lines never produce INDENT or DEDENT, including
        // comments positioned to the left of an otherwise indented body.
        loop {
            self.control.tick()?;
            match self.get(0) {
                Some(' ') => {
                    indent = indent.saturating_add(1);
                    self.at += 1;
                }
                Some('\t') => {
                    indent = (indent / 8).saturating_add(1).saturating_mul(8);
                    self.at += 1;
                }
                Some('\x0c') => {
                    indent = 0;
                    self.at += 1;
                }
                Some('\u{feff}') if self.at == 0 => self.at += 1,
                Some('#') => self.comment()?,
                Some('\r' | '\n') => {
                    self.newline();
                    indent = 0;
                }
                None => return Ok(None),
                _ => break,
            }
        }
        let mut statement = Statement {
            indent,
            start: self.at,
            line: self.line,
            end: self.at,
            end_line: self.line,
            complete: false,
            block: false,
            inline_suite: false,
        };
        let mut first = true;
        let mut after_async = false;
        let mut header = false;
        let mut empty_header = false;
        let mut match_header = false;
        let mut soft_keyword = false;
        let mut payload = false;
        let mut colon = None;
        let mut lambdas = 0usize;
        let mut depth = 0usize;
        let mut continuation = false;
        while let Some(ch) = self.get(0) {
            self.control.tick()?;
            match ch {
                ' ' | '\t' | '\x0c' => {
                    self.at += 1;
                    continue;
                }
                '#' => {
                    self.comment()?;
                    continue;
                }
                '\r' | '\n' => {
                    self.newline();
                    if depth == 0 {
                        statement.complete = true;
                        break;
                    }
                    continue;
                }
                '\\' if matches!(self.get(1), Some('\r' | '\n')) => {
                    self.at += 1;
                    self.newline();
                    continuation = true;
                    continue;
                }
                _ => {}
            }
            continuation = false;
            if ch.is_alphabetic() || ch == '_' {
                let word = self.word()?;
                if first {
                    header = word.header();
                    after_async = word.is(b"async");
                    empty_header = word.is(b"else")
                        || word.is(b"try")
                        || word.is(b"except")
                        || word.is(b"finally");
                    match_header = word.is(b"match");
                    soft_keyword = match_header || word.is(b"case");
                } else if after_async {
                    header = word.is(b"def") || word.is(b"for") || word.is(b"with");
                    after_async = false;
                } else {
                    payload = true;
                }
                if header && depth == 0 && colon.is_none() && word.is(b"lambda") {
                    lambdas = lambdas.saturating_add(1);
                }
                if let Some(formatted) = word.string_prefix()
                    && matches!(self.get(0), Some('\'' | '"'))
                    && !self.string(formatted)?
                {
                    return Ok(Some(statement));
                }
            } else {
                after_async = false;
                match ch {
                    '\'' | '"' => {
                        if !self.string(false)? {
                            return Ok(Some(statement));
                        }
                    }
                    '(' | '[' | '{' => {
                        depth = depth.saturating_add(1);
                        self.at += 1;
                    }
                    ')' | ']' | '}' => {
                        depth = depth.saturating_sub(1);
                        self.at += 1;
                    }
                    ':' if depth == 0 && header && colon.is_none() => {
                        if lambdas > 0 {
                            lambdas -= 1;
                        } else if payload || empty_header {
                            colon = Some(self.at);
                        } else {
                            header = false;
                        }
                        self.at += 1;
                    }
                    '=' if depth == 0 && soft_keyword && colon.is_none() => {
                        header = false;
                        self.at += 1;
                    }
                    _ => self.at += 1,
                }
                payload = true;
            }
            first = false;
            statement.end = self.at - 1;
            statement.end_line = self.line;
        }
        // A physical line at true EOF is also a complete logical statement.
        // At a prefix boundary it may continue, even if its last character
        // happens to be an identifier or a colon.
        if self.at == self.input.len()
            && self.input.len() == self.input.source.len_chars()
            && depth == 0
            && !continuation
        {
            statement.complete = true;
        }
        statement.block = colon == Some(statement.end);
        statement.inline_suite = !match_header && colon.is_some_and(|colon| statement.end > colon);
        Ok(Some(statement))
    }
}

#[cfg(test)]
mod tests {
    use super::scan;
    use std::cell::Cell;

    fn scopes(source: &str) -> Vec<String> {
        let rope = ropey::Rope::from_str(source);
        scan(&rope, rope.len_chars(), &|| false)
            .unwrap()
            .into_iter()
            .map(|(start, end)| rope.slice(start..=end).to_string())
            .collect()
    }

    #[test]
    fn discovers_nested_suites_and_sibling_branches() {
        let source = "class Example:\n    async def compute(self):\n        if ready:\n            run()\n        elif waiting:\n            wait()\n        else:\n            stop()\n    finish()\nnext()\n";
        let found = scopes(source);
        assert_eq!(found.len(), 5);
        assert_eq!(found[0], &source[..source.find("\nnext()").unwrap()]);
        assert_eq!(
            found[1],
            "async def compute(self):\n        if ready:\n            run()\n        elif waiting:\n            wait()\n        else:\n            stop()"
        );
        assert_eq!(found[2], "if ready:\n            run()");
        assert_eq!(found[3], "elif waiting:\n            wait()");
        assert_eq!(found[4], "else:\n            stop()");
    }

    #[test]
    fn supports_all_compound_statement_keywords() {
        for header in [
            "def f()",
            "class C",
            "if x",
            "elif x",
            "else",
            "for x in xs",
            "while x",
            "try",
            "except Error",
            "except* Error",
            "finally",
            "with resource() as r",
            "match value",
            "case [head, *tail]",
            "async def f()",
            "async for x in xs",
            "async with resource() as r",
        ] {
            let source = format!("{header}:\n    pass\n");
            assert_eq!(
                scopes(&source),
                [format!("{header}:\n    pass")],
                "{header}"
            );
        }
    }

    #[test]
    fn multiline_headers_and_bodies_use_logical_indentation() {
        let source = "def choose(\n    first: dict[str, int],\n    second=None,\n):  # a header\n    if (\n        first\n        and second\n    ):\n        result = {\n  'value': first,  # continuation may be indented less\n}\n    return result\nnext()\n";
        let found = scopes(source);
        assert_eq!(found.len(), 2);
        assert!(found[0].ends_with("return result"));
        assert_eq!(
            found[1],
            "if (\n        first\n        and second\n    ):\n        result = {\n  'value': first,  # continuation may be indented less\n}"
        );
    }

    #[test]
    fn explicit_continuations_and_crlf_do_not_dedent() {
        let source =
            "if first and \\\r\n   second:\r\n    value = first + \\\r\nsecond\r\nnext()\r\n";
        assert_eq!(
            scopes(source),
            [source[..source.find("\r\nnext()").unwrap()].to_owned()]
        );
    }

    #[test]
    fn comments_blank_lines_and_trailing_whitespace_do_not_extend_suites() {
        let source = "if ready: # header\n    work()   # trailing\n# left comment\n\n    finish()\t # end\n\n# next comment\nnext()\n";
        assert_eq!(
            scopes(source),
            ["if ready: # header\n    work()   # trailing\n# left comment\n\n    finish()"]
        );
    }

    #[test]
    fn strings_and_dictionaries_do_not_create_suites() {
        let source = "values = {\n 'key': {'nested': 1},\n}\ntext = '''\nif fake:\n    class AlsoFake:\n        pass\n'''\nother = \"if fake:\\n    pass\"\n# if fake:\n#     pass\n";
        assert!(scopes(source).is_empty());
    }

    #[test]
    fn multiline_docstrings_are_body_code_even_at_column_zero() {
        let source = "def doc():\n    r'''text\nif fake:\n   pass\n'''\nnext()\n";
        assert_eq!(
            scopes(source),
            [source[..source.find("\nnext()").unwrap()].to_owned()]
        );
    }

    #[test]
    fn string_escapes_and_fstring_expressions_preserve_outer_scope() {
        let source = "def render():\n    escaped = 'don\\'t: }'\n    result = f\"{values[\"key\"]:{width}.{precision}f}\"\n    nested = f'''{f\"{value}\"}\nif fake:\n    pass\n'''\n    return result\nnext()\n";
        let found = scopes(source);
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("return result"));
    }

    #[test]
    fn multiline_fstring_expression_can_contain_comments_and_strings() {
        let source = "if ready:\n    result = f\"{\n        mapping[\"key\"]  # } isn't a delimiter here\n    }\"\n    finish()\nnext()\n";
        assert_eq!(scopes(source).len(), 1);
        assert!(scopes(source)[0].ends_with("finish()"));
    }

    #[test]
    fn tabs_expand_to_eight_columns_and_formfeed_resets_indent() {
        let source = "if first:\n\tif second:\n\t\twork()\n        finish()\n \x0cnext()\n";
        let found = scopes(source);
        assert_eq!(found.len(), 2);
        assert!(found[0].ends_with("        finish()"));
        assert_eq!(found[1], "if second:\n\t\twork()");
    }

    #[test]
    fn unicode_offsets_and_bom_are_character_based() {
        let source = "\u{feff}if café:\n    print('🦀')\nnext()";
        assert_eq!(scopes(source), ["if café:\n    print('🦀')"]);
    }

    #[test]
    fn inline_suites_are_not_mistaken_for_indented_blocks() {
        assert!(scopes("if ready: work()\n    unrelated()\nnext()\n").is_empty());
        assert_eq!(
            scopes("if (\n    ready\n): work()\nnext()\n"),
            ["if (\n    ready\n): work()"]
        );
        assert_eq!(
            scopes("if lambda: True:\n    work()\n"),
            ["if lambda: True:\n    work()"]
        );
    }

    #[test]
    fn identifiers_soft_keywords_and_annotations_are_not_headers() {
        let source = "classify = 1\n    unrelated()\nmatch: dict[str, int] = {\n    'x': 1,\n}\ncase = 3\n    unrelated()\nasync_value = 4\n    unrelated()\n";
        assert!(scopes(source).is_empty());
    }

    #[test]
    fn true_eof_closes_suites_with_or_without_final_newline() {
        for source in [
            "if x:\n    pass",
            "if x:\n    pass\n",
            "if x:\n    pass\n# comment\n",
        ] {
            assert_eq!(scopes(source), ["if x:\n    pass"]);
        }
        assert!(scopes("if x:\n# no body\n").is_empty());
        assert!(scopes("if x:\n    unfinished(\n").is_empty());
        assert!(scopes("if x:\n    value = '''unfinished\n").is_empty());
        assert!(scopes("if x:\n    value = 1 + \\\n").is_empty());
    }

    #[test]
    fn prefix_truncation_does_not_invent_an_eof_dedent() {
        let rope = ropey::Rope::from_str(
            "if first:\n    if second:\n        work()\n    finish()\nnext()\n",
        );
        let finish = rope.to_string().find("finish()").unwrap();
        let prefix = scan(&rope, finish, &|| false).unwrap();
        assert!(prefix.is_empty());
        let prefix = scan(&rope, finish + 1, &|| false).unwrap();
        assert_eq!(prefix.len(), 1);
        assert_eq!(
            rope.slice(prefix[0].0..=prefix[0].1).to_string(),
            "if second:\n        work()"
        );
        let next = rope.to_string().find("next()").unwrap();
        assert_eq!(scan(&rope, next, &|| false).unwrap().len(), 1);
        assert_eq!(scan(&rope, next + 1, &|| false).unwrap().len(), 2);
    }

    #[test]
    fn cancellation_is_checked_inside_long_tokens_comments_and_strings() {
        for source in [
            format!("if x:\n    {}\n", "a".repeat(200_000)),
            format!("# {}\n", "x".repeat(200_000)),
            format!("if x:\n    '''{}'''\n", "x".repeat(200_000)),
            format!("{}if x:\n    pass\n", " ".repeat(200_000)),
        ] {
            let rope = ropey::Rope::from_str(&source);
            let checks = Cell::new(0);
            assert!(
                scan(&rope, rope.len_chars(), &|| {
                    checks.set(checks.get() + 1);
                    checks.get() > 4
                })
                .is_none()
            );
            assert_eq!(checks.get(), 5);
        }
    }

    #[test]
    fn large_document_retains_sorted_scopes_without_recursion() {
        let source = "if ready:\n    work()\nnext()\n".repeat(10_000);
        let rope = ropey::Rope::from_str(&source);
        let checks = Cell::new(0);
        let found = scan(&rope, rope.len_chars(), &|| {
            checks.set(checks.get() + 1);
            false
        })
        .unwrap();
        assert_eq!(found.len(), 10_000);
        assert!(found.windows(2).all(|nodes| nodes[0].1 < nodes[1].0));
        // The callback runs once per fixed work interval, not once per byte or
        // suite ancestor. This also guards against accidental quadratic work.
        assert!(checks.get() < source.len() / 1_000);
    }
}
