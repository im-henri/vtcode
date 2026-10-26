use crate::background::CancellationToken;
use grep_matcher::Matcher;
use std::ops::Range;

#[path = "search_disk.rs"]
mod disk;

// Behavioral reference: microsoft/vscode 959031245ebb1fe077e0d512e1397c0ae82006e4,
// textModelSearch.ts, strings.ts and ripgrepTextSearchEngine.ts (MIT).
pub(crate) const WORD_SEPARATORS: &str = "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?";

pub(crate) fn error_summary(error: &str) -> String {
    let detail = error.lines().last().unwrap_or(error).trim();
    format!(
        "Invalid regex: {}",
        detail.strip_prefix("error: ").unwrap_or(detail)
    )
}

pub(crate) fn is_multiline_regex(query: &str) -> bool {
    let mut chars = query.chars();
    while let Some(ch) = chars.next() {
        if ch == '\n' || ch == '\\' && matches!(chars.next(), Some('n' | 'r' | 'W')) {
            return true;
        }
    }
    false
}

pub(crate) fn is_whole_word_match(text: &str, start: usize, end: usize) -> bool {
    let separator = |ch: char| WORD_SEPARATORS.contains(ch) || matches!(ch, ' ' | '\t');
    let outside = |ch: char| separator(ch) || matches!(ch, '\r' | '\n');
    let left = start == 0
        || text[..start].chars().next_back().is_some_and(outside)
        || start < end && text[start..end].chars().next().is_some_and(separator);
    let right = end == text.len()
        || text[end..].chars().next().is_some_and(outside)
        || start < end && text[start..end].chars().next_back().is_some_and(separator);
    left && right
}

#[derive(Clone)]
enum Engine {
    JavaScript(std::sync::Arc<regress::Regex>),
    Ripgrep(grep_regex::RegexMatcher),
    Pcre2(grep_pcre2::RegexMatcher),
}

#[derive(Clone)]
pub(crate) struct SearchRegex {
    engine: Engine,
    pub needs_document: bool,
    whole_word: bool,
    capture_regex: Option<std::sync::Arc<regress::Regex>>,
    editor_regex: Option<std::sync::Arc<regress::Regex>>,
}

pub(crate) struct SearchCaptures {
    pub range: Range<usize>,
    pub groups: Vec<Option<Range<usize>>>,
    pub names: Vec<(String, Option<Range<usize>>)>,
}

/// VS Code searches LF text in editors, then maps results back to CRLF.
pub(crate) struct SearchText {
    pub text: String,
    removed_crs: Vec<usize>,
}

struct CachedEditorPattern {
    query: String,
    case_sensitive: bool,
    whole_word: bool,
    is_regex: bool,
    regex: Result<std::sync::Arc<SearchRegex>, String>,
}

thread_local! {
    static EDITOR_PATTERNS: std::cell::RefCell<std::collections::VecDeque<CachedEditorPattern>> =
        std::cell::RefCell::new(std::collections::VecDeque::new());
}

impl SearchText {
    pub(crate) fn from_rope(slice: ropey::RopeSlice<'_>, normalize: bool) -> Self {
        if let Some(text) = slice.as_str() {
            return Self::new(text, normalize);
        }
        let text = slice.to_string();
        if !normalize || !text.contains('\r') {
            Self {
                text,
                removed_crs: Vec::new(),
            }
        } else {
            Self::new(&text, normalize)
        }
    }

    pub(crate) fn new(text: &str, normalize: bool) -> Self {
        if !normalize || !text.contains('\r') {
            return Self {
                text: text.to_owned(),
                removed_crs: Vec::new(),
            };
        }
        let mut result = Self {
            text: String::with_capacity(text.len()),
            removed_crs: Vec::new(),
        };
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\r' {
                if chars.peek() == Some(&'\n') {
                    result.removed_crs.push(result.text.len());
                    chars.next();
                }
                result.text.push('\n');
            } else {
                result.text.push(ch);
            }
        }
        result
    }

    pub(crate) fn original_offset(&self, byte: usize) -> usize {
        byte + self.removed_crs.partition_point(|&cr| cr < byte)
    }

    pub(crate) fn normalized_offset(&self, byte: usize) -> usize {
        // The original CR offsets are monotone too. Replacement may convert
        // thousands of positions in a CRLF document; do not rescan every
        // preceding CR for each match.
        let mut low = 0;
        let mut high = self.removed_crs.len();
        while low < high {
            let mid = low + (high - low) / 2;
            if self.removed_crs[mid] + mid < byte {
                low = mid + 1;
            } else {
                high = mid;
            }
        }
        byte - low
    }
}

impl SearchRegex {
    /// Incremental Find and repeated document scans normally reuse a handful
    /// of patterns. Retain bounded per-worker compiled engines, including
    /// invalid-pattern diagnostics, instead of compiling again for each edit.
    pub(crate) fn cached_editor(
        query: &str,
        case_sensitive: bool,
        whole_word: bool,
        is_regex: bool,
    ) -> Result<std::sync::Arc<Self>, String> {
        EDITOR_PATTERNS.with(|patterns| {
            let mut patterns = patterns.borrow_mut();
            if let Some(index) = patterns.iter().position(|pattern| {
                pattern.query == query
                    && pattern.case_sensitive == case_sensitive
                    && pattern.whole_word == whole_word
                    && pattern.is_regex == is_regex
            }) {
                let pattern = patterns.remove(index).unwrap();
                let regex = pattern.regex.clone();
                patterns.push_back(pattern);
                return regex;
            }
            let regex =
                Self::editor(query, case_sensitive, whole_word, is_regex).map(std::sync::Arc::new);
            if patterns.len() == 8 {
                patterns.pop_front();
            }
            patterns.push_back(CachedEditorPattern {
                query: query.to_owned(),
                case_sensitive,
                whole_word,
                is_regex,
                regex: regex.clone(),
            });
            regex
        })
    }

    pub(crate) fn new(query: &str, case_sensitive: bool, whole_word: bool) -> Result<Self, String> {
        Self::editor(query, case_sensitive, whole_word, true)
    }

    pub(crate) fn editor(
        query: &str,
        case_sensitive: bool,
        whole_word: bool,
        is_regex: bool,
    ) -> Result<Self, String> {
        let needs_document = if is_regex {
            is_multiline_regex(query)
        } else {
            query.contains('\n')
        };
        let source = if is_regex {
            query.to_owned()
        } else {
            escape_literal(query)
        };
        let regex = regress::Regex::with_flags(
            &source,
            regress::Flags {
                icase: !case_sensitive,
                multiline: needs_document,
                unicode: true,
                ..Default::default()
            },
        )
        .map_err(|error| error.to_string())?;
        Ok(Self {
            engine: Engine::JavaScript(std::sync::Arc::new(regex)),
            needs_document,
            whole_word,
            capture_regex: None,
            editor_regex: None,
        })
    }

    pub(crate) fn workspace(
        query: &str,
        case_sensitive: bool,
        whole_word: bool,
        is_regex: bool,
    ) -> Result<Self, String> {
        // Validate the editor syntax once and reuse that immutable program.
        // Captures need a separate program only if word boundaries alter it;
        // opening another buffer must not clone the compiled instruction Vec.
        let editor = if is_regex { Self::new(query, case_sensitive, whole_word)? }
            else { Self::editor(query, case_sensitive, whole_word, false)? };
        let needs_document = editor.needs_document;
        let Engine::JavaScript(editor_regex) = editor.engine else { unreachable!() };
        let original = if is_regex {
            query.to_owned()
        } else {
            escape_literal(query)
        };
        let add_boundaries = |mut source: String| {
            let word = |ch: char| ch.is_ascii_alphanumeric() || ch == '_';
            if source.chars().next().is_some_and(word) {
                source.insert_str(0, r"\b");
            }
            if source.chars().next_back().is_some_and(word) {
                source.push_str(r"\b");
            }
            source
        };
        let capture_source = if whole_word {
            add_boundaries(original.clone())
        } else {
            original.clone()
        };
        let capture_regex = if capture_source == original { editor_regex.clone() } else {
            std::sync::Arc::new(regress::Regex::with_flags(
            &capture_source,
            regress::Flags {
                icase: !case_sensitive,
                multiline: needs_document,
                unicode: true,
                ..Default::default()
            },
        )
        .map_err(|error| error.to_string())?)
        };
        // VS Code translates Unicode escapes before applying whole-word rules.
        static UNICODE: std::sync::OnceLock<[regex::Regex; 2]> = std::sync::OnceLock::new();
        let escapes = UNICODE.get_or_init(|| {
            [
                regex::Regex::new(r"(?i)((?:[^\\]|^)(?:\\\\)*)\\u([a-z0-9]{4})").unwrap(),
                regex::Regex::new(r"(?i)((?:[^\\]|^)(?:\\\\)*)\\u\{([a-z0-9]{4})\}").unwrap(),
            ]
        });
        let mut source = original;
        if is_regex {
            for escape in escapes {
                while escape.is_match(&source) {
                    source = escape
                        .replace_all(&source, |caps: &regex::Captures<'_>| {
                            format!(r"{}\x{{{}}}", &caps[1], &caps[2])
                        })
                        .into_owned();
                }
            }
        }
        if whole_word {
            source = add_boundaries(source);
        } else if needs_document {
            source = fix_regex_newlines(&source);
        }
        let mut builder = grep_regex::RegexMatcherBuilder::new();
        builder
            .case_insensitive(!case_sensitive)
            .multi_line(true)
            .unicode(true)
            .crlf(true);
        if needs_document {
            builder.line_terminator(None);
        }
        let engine = match builder.build(&source) {
            Ok(regex) => Engine::Ripgrep(regex),
            Err(_) => Engine::Pcre2(
                grep_pcre2::RegexMatcherBuilder::new()
                    .caseless(!case_sensitive)
                    .multi_line(true)
                    .crlf(true)
                    .utf(true)
                    .ucp(true)
                    .jit_if_available(cfg!(target_pointer_width = "64"))
                    .max_jit_stack_size(Some(10 * (1 << 20)))
                    .build(&source)
                    .map_err(|error| error.to_string())?,
            ),
        };
        Ok(Self {
            engine,
            needs_document,
            whole_word,
            capture_regex: Some(capture_regex),
            editor_regex: Some(editor_regex),
        })
    }
    pub(crate) fn for_open_buffer(&self) -> Self {
        if let Some(regex) = &self.editor_regex {
            Self {
                engine: Engine::JavaScript(regex.clone()),
                needs_document: self.needs_document,
                whole_word: self.whole_word,
                capture_regex: None,
                editor_regex: None,
            }
        } else {
            self.clone()
        }
    }

    pub(crate) fn find_at(&self, text: &str, start: usize) -> Option<Range<usize>> {
        match &self.engine {
            Engine::JavaScript(regex) => regex
                .find_from(text, start)
                .next()
                .map(|found| found.range()),
            Engine::Ripgrep(regex) => regex
                .find_at(text.as_bytes(), start)
                .ok()
                .flatten()
                .map(|found| found.start()..found.end()),
            Engine::Pcre2(regex) => regex
                .find_at(text.as_bytes(), start)
                .ok()
                .flatten()
                .map(|found| found.start()..found.end()),
        }
    }

    pub(crate) fn captures_at(&self, text: &str, start: usize) -> Option<SearchCaptures> {
        let regex = match &self.engine {
            Engine::JavaScript(regex) => regex,
            _ => self.capture_regex.as_ref()?,
        };
        regex
            .find_from(text, start)
            .next()
            .map(|found| SearchCaptures {
                names: found
                    .named_groups()
                    .map(|(name, range)| (name.to_owned(), range))
                    .collect(),
                range: found.range(),
                groups: std::iter::once(Some(found.range()))
                    .chain(found.captures)
                    .collect(),
            })
    }
    pub(crate) fn scan(
        &self,
        rope: &ropey::Rope,
        selection: Option<(usize, usize, usize, usize)>,
        limit: usize,
        cancel: &CancellationToken,
    ) -> (Vec<(usize, usize, usize)>, bool) {
        self.scan_lines(rope, selection, 0, rope.len_lines(), limit, cancel)
    }

    pub(crate) fn scan_lines(
        &self,
        rope: &ropey::Rope,
        selection: Option<(usize, usize, usize, usize)>,
        first_line: usize,
        last_line: usize,
        limit: usize,
        cancel: &CancellationToken,
    ) -> (Vec<(usize, usize, usize)>, bool) {
        match &self.engine {
            Engine::Ripgrep(matcher) => {
                return scan_workspace(matcher, rope, self.needs_document, limit, cancel);
            }
            Engine::Pcre2(matcher) => {
                return scan_workspace(matcher, rope, self.needs_document, limit, cancel);
            }
            Engine::JavaScript(_) => {}
        }
        let mut matches = Vec::new();
        let bounds = selection.map(|(sl, sc, el, ec)| {
            let position = |line: usize, col: usize| {
                let line = line.min(rope.len_lines() - 1);
                (rope.line_to_char(line) + col.min(rope.line(line).len_chars()))
                    .min(rope.len_chars())
            };
            let start = position(sl, sc);
            (start, position(el, ec).max(start))
        });
        let Engine::JavaScript(regex) = &self.engine else {
            unreachable!()
        };
        let mut scan = |text: &str, byte_offset: usize, line_context: Option<(usize, usize)>| {
            // Most lines are already LF text. Borrow them instead of making
            // another complete copy after assembling a chunked rope line.
            let normalized = text.contains('\r').then(|| SearchText::new(text, true));
            let input = normalized
                .as_ref()
                .map_or(text, |input| input.text.as_str());
            let mut column_byte = 0;
            let mut column_char = line_context.map_or(0, |(_, col)| col);
            // Reuse the regex executor across matches. Recreating find_from
            // for each dense hit repeatedly allocates its capture/backtrack state.
            for found in regex.find_iter(input) {
                if cancel.is_cancelled() {
                    return false;
                }
                let found = found.range();
                let original_start = normalized
                    .as_ref()
                    .map_or(found.start, |input| input.original_offset(found.start));
                let original_end = normalized
                    .as_ref()
                    .map_or(found.end, |input| input.original_offset(found.end));
                let coordinates = if let Some((row, _)) = line_context {
                    // Match positions advance monotonically. Decode each UTF-8
                    // prefix once, avoiding several rope tree seeks per hit.
                    let start = column_char + text[column_byte..original_start].chars().count();
                    let end = start + text[original_start..original_end].chars().count();
                    column_byte = original_end;
                    column_char = end;
                    (row, start, end)
                } else {
                    let start = rope.byte_to_char(byte_offset + original_start);
                    let end = rope.byte_to_char(byte_offset + original_end);
                    let row = rope.char_to_line(start);
                    let line_start = rope.line_to_char(row);
                    (row, start - line_start, end - line_start)
                };
                if !self.whole_word || is_whole_word_match(input, found.start, found.end) {
                    if matches.len() == limit {
                        return true;
                    }
                    matches.push(coordinates);
                }
                // Preserve Find's rule suppressing a trailing empty match
                // after a nonempty match that already reached input end.
                if found.end == input.len() {
                    return false;
                }
            }
            false
        };
        let truncated = if self.needs_document {
            let (start, end) = bounds.unwrap_or((0, rope.len_chars()));
            let mut text = String::new();
            for chunk in rope.slice(start..end).chunks() {
                if cancel.is_cancelled() {
                    return (Vec::new(), false);
                }
                text.push_str(chunk);
            }
            scan(&text, rope.char_to_byte(start), None)
        } else {
            let mut limited = false;
            let mut line_buffer = String::new();
            let first_line = first_line.min(rope.len_lines());
            let last_line = last_line.min(rope.len_lines());
            for (row, line) in (first_line..last_line).zip(rope.lines_at(first_line)) {
                if cancel.is_cancelled() {
                    return (Vec::new(), false);
                }
                let line_start = rope.line_to_char(row);
                let text = if let Some(text) = line.as_str() {
                    text
                } else {
                    line_buffer.clear();
                    for chunk in line.chunks() {
                        if cancel.is_cancelled() {
                            return (Vec::new(), false);
                        }
                        line_buffer.push_str(chunk);
                    }
                    &line_buffer
                };
                let clean = text.trim_end_matches(['\r', '\n']);
                let line_end = line_start + line.len_chars() - (text.len() - clean.len());
                let (start, end) = bounds.map_or((line_start, line_end), |(low, high)| {
                    (low.max(line_start), high.min(line_end))
                });
                if start > end {
                    continue;
                }
                let start_col = start - line_start;
                let start_byte = if start_col == 0 {
                    0
                } else {
                    line.char_to_byte(start_col)
                };
                let end_byte = if end == line_end {
                    clean.len()
                } else {
                    line.char_to_byte(end - line_start)
                };
                if scan(&text[start_byte..end_byte], 0, Some((row, start_col))) {
                    limited = true;
                    break;
                }
            }
            limited
        };
        if cancel.is_cancelled() {
            (Vec::new(), false)
        } else {
            (matches, truncated)
        }
    }
}

/// Use the same searcher and match enumeration as ripgrep's JSON output.
/// VS Code consumes that output rather than running its editor matcher on disk.
fn scan_workspace<M: Matcher>(
    matcher: &M,
    rope: &ropey::Rope,
    multiline: bool,
    limit: usize,
    cancel: &CancellationToken,
) -> (Vec<(usize, usize, usize)>, bool) {
    scan_workspace_input(
        matcher,
        rope,
        multiline,
        limit,
        cancel,
        #[cfg(test)]
        true,
    )
}

/// Feed immutable Rope chunks directly to ripgrep's rolling input buffer. Most
/// workspace searches need no complete contiguous copy of the source at all;
/// multiline queries let the searcher assemble its required document once.
struct RopeSearchReader<'a> {
    chunks: ropey::iter::Chunks<'a>,
    current: &'a [u8],
    cancel: &'a CancellationToken,
}

impl std::io::Read for RopeSearchReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Ok(0);
        }
        let mut written = 0;
        while written < output.len() {
            if self.current.is_empty() {
                self.current = self.chunks.next().map(str::as_bytes).unwrap_or_default();
                if self.current.is_empty() {
                    break;
                }
            }
            let count = self.current.len().min(output.len() - written);
            output[written..written + count].copy_from_slice(&self.current[..count]);
            self.current = &self.current[count..];
            written += count;
        }
        Ok(written)
    }
}

fn scan_workspace_input<M: Matcher>(
    matcher: &M,
    rope: &ropey::Rope,
    multiline: bool,
    limit: usize,
    cancel: &CancellationToken,
    #[cfg(test)] streamed: bool,
) -> (Vec<(usize, usize, usize)>, bool) {
    struct Sink<'a, M> {
        matcher: &'a M,
        rope: &'a ropey::Rope,
        cancel: &'a CancellationToken,
        matches: Vec<(usize, usize, usize)>,
        limit: usize,
        truncated: bool,
        column_row: usize,
        column_utf16: usize,
        column_scalar: usize,
        last_column: usize,
    }
    impl<M: Matcher> Sink<'_, M> {
        fn column(&mut self, row: usize, utf16: usize) -> usize {
            if self.column_row != row || utf16 < self.last_column {
                self.column_row = row;
                self.column_utf16 = 0;
                self.column_scalar = 0;
            }
            self.last_column = utf16;
            let line = self.rope.line(row);
            let mut chars = line.chars_at(self.column_scalar);
            while self.column_utf16 < utf16 {
                let Some(ch) = chars.next() else {
                    break;
                };
                self.column_utf16 += ch.len_utf16();
                self.column_scalar += 1;
            }
            self.column_scalar + utf16.saturating_sub(self.column_utf16)
        }
        fn push(&mut self, row: usize, col: usize, end_row: usize, end_col: usize) -> bool {
            if self.cancel.is_cancelled() {
                return false;
            }
            if self.matches.len() == self.limit {
                self.truncated = true;
                return false;
            }
            if row >= self.rope.len_lines() || end_row >= self.rope.len_lines() {
                return true;
            }
            let col = self.column(row, col);
            let end_col = self.column(end_row, end_col);
            self.matches.push((
                row,
                col,
                self.rope.line_to_char(end_row) + end_col - self.rope.line_to_char(row),
            ));
            true
        }
    }
    fn advance(text: &str, row: &mut usize, column: &mut usize) {
        if let Some(last) = text.rfind('\n') {
            *row += text.bytes().filter(|&ch| ch == b'\n').count();
            *column = text[last + 1..].encode_utf16().count();
        } else {
            *column += text.encode_utf16().count();
        }
    }
    impl<M: Matcher> grep_searcher::Sink for Sink<'_, M> {
        type Error = std::io::Error;
        fn matched(
            &mut self,
            searcher: &grep_searcher::Searcher,
            mat: &grep_searcher::SinkMatch<'_>,
        ) -> Result<bool, Self::Error> {
            if self.cancel.is_cancelled() {
                return Ok(false);
            }
            let range = mat.bytes_range_in_buffer();
            let mut bytes = mat.buffer();
            if searcher.multi_line_with_matcher(self.matcher) {
                // grep-printer's MAX_LOOK_AHEAD.
                bytes = &bytes[..bytes.len().min(range.end.saturating_add(128))];
            } else {
                let mut end = range.end;
                if searcher.line_terminator().is_suffix(&bytes[..end]) {
                    end -= 1;
                    if searcher.line_terminator().is_crlf() && end > 0 && bytes[end - 1] == b'\r' {
                        end -= 1;
                    }
                }
                bytes = &bytes[..end];
            }
            let mut previous_end = range.start;
            let mut row = mat.line_number().unwrap_or(1) as usize - 1;
            let mut col = 0;
            let mut found_any = false;
            let mut keep_going = true;
            self.matcher
                .find_iter_at(bytes, range.start, |found| {
                    if found.start() >= range.end {
                        return false;
                    }
                    found_any = true;
                    // Decode each byte segment separately, as createTextSearchMatch does.
                    advance(
                        &String::from_utf8_lossy(&bytes[previous_end..found.start()]),
                        &mut row,
                        &mut col,
                    );
                    let (start_row, start_col) = (row, col);
                    advance(
                        &String::from_utf8_lossy(&bytes[found.start()..found.end()]),
                        &mut row,
                        &mut col,
                    );
                    previous_end = found.end();
                    keep_going = self.push(start_row, start_col, row, col);
                    keep_going
                })
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            if !found_any {
                // VS Code supplies a first-character match when rg found a
                // matching line but emitted no submatches (e.g. /\r/ on CRLF).
                let width = String::from_utf8_lossy(mat.bytes())
                    .chars()
                    .next()
                    .map_or(0, |_| 1);
                keep_going = self.push(row, 0, row, width);
            }
            Ok(keep_going)
        }
    }
    let mut sink = Sink {
        matcher,
        rope,
        cancel,
        matches: Vec::new(),
        limit,
        truncated: false,
        column_row: usize::MAX,
        column_utf16: 0,
        column_scalar: 0,
        last_column: 0,
    };
    let mut searcher = grep_searcher::SearcherBuilder::new()
        .line_terminator(grep_matcher::LineTerminator::crlf())
        .line_number(true)
        .multi_line(multiline)
        .build();
    #[cfg(test)]
    if !streamed {
        // Retained contiguous oracle for comparative regression tests.
        let mut text = String::with_capacity(rope.len_bytes());
        for chunk in rope.chunks() {
            if cancel.is_cancelled() {
                return (Vec::new(), false);
            }
            text.push_str(chunk);
        }
        let _ = searcher.search_slice(matcher, text.as_bytes(), &mut sink);
        return if cancel.is_cancelled() {
            (Vec::new(), false)
        } else {
            (sink.matches, sink.truncated)
        };
    }
    let reader = RopeSearchReader {
        chunks: rope.chunks(),
        current: &[],
        cancel,
    };
    let _ = searcher.search_reader(matcher, reader, &mut sink);
    if cancel.is_cancelled() {
        (Vec::new(), false)
    } else {
        (sink.matches, sink.truncated)
    }
}

// regex::escape escapes #, &, ~ and - as well; those identity escapes are
// invalid in a JavaScript Unicode regex. Use VS Code's literal escape set.
pub(crate) fn escape_literal(text: &str) -> String {
    let mut source = String::new();
    for ch in text.chars() {
        if "\\{}[]()+*?.^$|".contains(ch) {
            source.push('\\');
        }
        source.push(ch);
    }
    source
}

fn fix_regex_newlines(source: &str) -> String {
    let mut result = String::new();
    let mut chars = source.chars().peekable();
    let mut groups = Vec::new();
    while let Some(ch) = chars.next() {
        match ch {
            '(' => {
                let rest: String = chars.clone().take(3).collect();
                groups.push(rest.starts_with("?<=") || rest.starts_with("?<!"));
                result.push(ch);
            }
            ')' => {
                groups.pop();
                result.push(ch);
            }
            '\\' => match chars.next() {
                Some('n') if !groups.contains(&true) => {
                    if chars
                        .peek()
                        .is_some_and(|c| matches!(c, '*' | '+' | '?' | '{'))
                    {
                        result.push_str(r"(?:\r?\n)");
                    } else {
                        result.push_str(r"\r?\n");
                    }
                }
                Some(next) => {
                    result.push('\\');
                    result.push(next);
                }
                None => result.push('\\'),
            },
            '[' => {
                let mut class = String::new();
                while let Some(next) = chars.next() {
                    if next == ']' {
                        break;
                    }
                    class.push(next);
                    if next == '\\' {
                        if let Some(escaped) = chars.next() {
                            class.push(escaped);
                        }
                    }
                }
                let mut escaped = class.char_indices();
                let mut newlines = Vec::new();
                while let Some((index, ch)) = escaped.next() {
                    if ch == '\\'
                        && escaped.next().is_some_and(|(_, next)| next == 'n')
                        && !class[..index].ends_with('-')
                        && !class[index + 2..].starts_with('-')
                    {
                        newlines.push(index);
                    }
                }
                if !groups.contains(&true) && !newlines.is_empty() {
                    for index in newlines {
                        let negative = class.starts_with('^');
                        let other = format!(
                            "{}{}",
                            &class[usize::from(negative)..index],
                            &class[index + 2..]
                        );
                        if negative
                            && chars
                                .peek()
                                .is_some_and(|c| matches!(c, '*' | '+' | '?' | '{'))
                        {
                            if other.is_empty() {
                                result.push('.');
                            } else {
                                result.push_str(&format!("[^{other}]"));
                            }
                        } else if negative {
                            result.push_str(&format!(
                                r"(?!\r?\n{})",
                                if other.is_empty() {
                                    String::new()
                                } else {
                                    format!("|[{other}]")
                                }
                            ));
                        } else if other.is_empty() {
                            result.push_str(r"\r?\n");
                        } else {
                            result.push_str(&format!(r"(?:[{other}]|\r?\n)"));
                        }
                    }
                } else {
                    result.push('[');
                    result.push_str(&class);
                    result.push(']');
                }
            }
            '\n' => result.push_str(r"\r?\n"),
            _ => result.push(ch),
        }
    }
    result.replace('\n', r"\r?\n")
}

#[cfg(test)]
mod performance_tests {
    use super::*;

    #[test]
    fn streamed_workspace_search_matches_contiguous_oracle_across_buffers_and_limits() {
        let source = format!(
            "{}é needle NEEDLE\r\n{}\r\nend needle\n",
            "ordinary row\r\n".repeat(8_000),
            "é needle ".repeat(12_000)
        );
        let rope = ropey::Rope::from_str(&source);
        let cancel = CancellationToken::new();
        for (query, sensitive, regex) in [
            ("needle", true, false),
            ("needle", false, false),
            ("é n\\w+", true, true),
            ("needle\\r?\\n", true, true),
            ("(?:)", true, true),
            ("(?<=é )needle", true, true),
            ("absent", true, false),
        ] {
            let pattern = SearchRegex::workspace(query, sensitive, false, regex).unwrap();
            for limit in [0, 1, 17, 20_000] {
                let (actual, expected) = match &pattern.engine {
                    Engine::Ripgrep(matcher) => (
                        scan_workspace_input(
                            matcher,
                            &rope,
                            pattern.needs_document,
                            limit,
                            &cancel,
                            true,
                        ),
                        scan_workspace_input(
                            matcher,
                            &rope,
                            pattern.needs_document,
                            limit,
                            &cancel,
                            false,
                        ),
                    ),
                    Engine::Pcre2(matcher) => (
                        scan_workspace_input(
                            matcher,
                            &rope,
                            pattern.needs_document,
                            limit,
                            &cancel,
                            true,
                        ),
                        scan_workspace_input(
                            matcher,
                            &rope,
                            pattern.needs_document,
                            limit,
                            &cancel,
                            false,
                        ),
                    ),
                    Engine::JavaScript(_) => unreachable!("disk search uses ripgrep"),
                };
                assert_eq!(actual, expected, "query={query:?}, limit={limit}");
            }
        }
        cancel.cancel();
        let mut reader = RopeSearchReader {
            chunks: rope.chunks(),
            current: &[],
            cancel: &cancel,
        };
        let mut bytes = [0u8; 16];
        assert_eq!(std::io::Read::read(&mut reader, &mut bytes).unwrap(), 0);
    }

    #[test]
    fn streamed_regex_matches_keep_unicode_selection_empty_match_and_crlf_semantics() {
        let cancel = CancellationToken::new();
        for source in [
            "α😀 foo FOO foo\r\nline\r\n",
            "aaaa\nä界 ä界\n",
            "",
            "x\r\n\r\ny",
            "foo",
        ] {
            let rope = ropey::Rope::from_str(source);
            for pattern in [
                "foo",
                "a*",
                "(?=.)",
                "$",
                "^",
                "(?<=f)oo",
                "[ä界]+",
                "\\n",
                "[\\s\\S]*",
                "(?<word>foo)|.",
            ] {
                for whole_word in [false, true] {
                    let regex = SearchRegex::editor(pattern, false, whole_word, true).unwrap();
                    for selection in [
                        None,
                        Some((0, 1, 0, 9)),
                        Some((0, 0, 2, 1)),
                        Some((0, 999, 0, 999)),
                    ] {
                        for limit in [0, 2, 100] {
                            assert_eq!(
                                regex.scan_lines(
                                    &rope,
                                    selection,
                                    0,
                                    rope.len_lines(),
                                    limit,
                                    &cancel
                                ),
                                regex.legacy_scan_lines(
                                    &rope,
                                    selection,
                                    0,
                                    rope.len_lines(),
                                    limit,
                                    &cancel
                                ),
                                "source={source:?} pattern={pattern:?} selection={selection:?} whole={whole_word} limit={limit}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn cached_editor_patterns_reuse_engines_and_preserve_options_and_errors() {
        let first = SearchRegex::cached_editor("cache-[0-9]+", true, false, true).unwrap();
        let second = SearchRegex::cached_editor("cache-[0-9]+", true, false, true).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first, &second));
        let other = SearchRegex::cached_editor("cache-[0-9]+", false, false, true).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&first, &other));
        assert_eq!(
            SearchRegex::cached_editor("[", true, false, true).err(),
            SearchRegex::editor("[", true, false, true).err()
        );
        for query in 0..20 {
            SearchRegex::cached_editor(&format!("cache-{query}"), true, false, false).unwrap();
        }
        EDITOR_PATTERNS.with(|patterns| assert_eq!(patterns.borrow().len(), 8));
    }

    #[test]
    fn workspace_patterns_share_programs_without_changing_capture_or_editor_word_rules() {
        for (query, whole_word, shared) in [("(?<item>foo)", false, true), ("foo", true, false), ("(foo)", true, true)] {
            let workspace = SearchRegex::workspace(query, true, whole_word, true).unwrap();
            let capture = workspace.capture_regex.as_ref().unwrap();
            let editor = workspace.editor_regex.as_ref().unwrap();
            assert_eq!(std::sync::Arc::ptr_eq(capture, editor), shared);
            let open = workspace.for_open_buffer();
            let Engine::JavaScript(open_regex) = &open.engine else { panic!() };
            assert!(std::sync::Arc::ptr_eq(open_regex, editor));
            let expected = SearchRegex::editor(query, true, whole_word, true).unwrap();
            let rope = ropey::Rope::from_str("foobar foo\r\nfoo🦀 foo\n");
            let cancel = CancellationToken::new();
            assert_eq!(open.scan(&rope, None, 100, &cancel), expected.scan(&rope, None, 100, &cancel));
            let captures = workspace.captures_at("foobar foo", 0).unwrap();
            assert_eq!(captures.range, if query == "foo" && whole_word { 7..10 } else { 0..3 });
        }
        for query in ["(", r"\p{Invalid}", "[abc"] {
            assert_eq!(SearchRegex::workspace(query, true, false, true).err(), SearchRegex::editor(query, true, false, true).err());
        }
    }

    #[test]
    fn crlf_normalized_offsets_match_linear_reference_at_all_boundaries() {
        let source = format!("{}tail", "界\r\n😀\r\n".repeat(1_000));
        let input = SearchText::new(&source, true);
        for byte in 0..=source.len() {
            let expected = byte
                - input
                    .removed_crs
                    .iter()
                    .enumerate()
                    .take_while(|&(index, cr)| cr + index < byte)
                    .count();
            assert_eq!(input.normalized_offset(byte), expected);
        }
    }

    #[test]
    #[ignore = "manual comparative release benchmark"]
    fn benchmark_regex_find_dense_unicode_and_many_lines() {
        let cancel = CancellationToken::new();
        for (name, source, query) in [
            ("dense-unicode", "界😀 needle ".repeat(10_000), "needle"),
            (
                "many-short-lines",
                "let value = ordinary_source();\r\n".repeat(50_000),
                "absent-[0-9]+",
            ),
        ] {
            let rope = ropey::Rope::from_str(&source);
            let regex = SearchRegex::editor(query, false, false, true).unwrap();
            let expected =
                regex.legacy_scan_lines(&rope, None, 0, rope.len_lines(), 19_999, &cancel);
            let mut before = Vec::new();
            let mut after = Vec::new();
            for sample in 0..7 {
                for optimized in [sample % 2 == 0, sample % 2 != 0] {
                    let started = std::time::Instant::now();
                    let results = if optimized {
                        regex.scan_lines(
                            std::hint::black_box(&rope),
                            None,
                            0,
                            rope.len_lines(),
                            19_999,
                            &cancel,
                        )
                    } else {
                        regex.legacy_scan_lines(
                            std::hint::black_box(&rope),
                            None,
                            0,
                            rope.len_lines(),
                            19_999,
                            &cancel,
                        )
                    };
                    let elapsed = started.elapsed();
                    assert_eq!(std::hint::black_box(results), expected);
                    if optimized {
                        after.push(elapsed);
                    } else {
                        before.push(elapsed);
                    }
                }
            }
            before.sort();
            after.sort();
            eprintln!(
                "regex Find {name}: before={:?} after={:?}",
                before[3], after[3]
            );
        }
    }

    // Previous scan implementation retained as a semantic and timing reference.
    impl SearchRegex {
        fn legacy_scan_lines(
            &self,
            rope: &ropey::Rope,
            selection: Option<(usize, usize, usize, usize)>,
            first_line: usize,
            last_line: usize,
            limit: usize,
            cancel: &CancellationToken,
        ) -> (Vec<(usize, usize, usize)>, bool) {
            match &self.engine {
                Engine::Ripgrep(matcher) => {
                    return scan_workspace(matcher, rope, self.needs_document, limit, cancel);
                }
                Engine::Pcre2(matcher) => {
                    return scan_workspace(matcher, rope, self.needs_document, limit, cancel);
                }
                Engine::JavaScript(_) => {}
            }
            let mut matches = Vec::new();
            let bounds = selection.map(|(sl, sc, el, ec)| {
                let position = |line: usize, col: usize| {
                    let line = line.min(rope.len_lines() - 1);
                    (rope.line_to_char(line) + col.min(rope.line(line).len_chars()))
                        .min(rope.len_chars())
                };
                let start = position(sl, sc);
                (start, position(el, ec).max(start))
            });
            let mut scan = |text: &str, byte_offset: usize| {
                let input = SearchText::new(text, true);
                let mut pos = 0;
                let mut previous_end = None;
                loop {
                    if cancel.is_cancelled() {
                        return false;
                    }
                    // Searcher allows adjacent empty matches except after a match
                    // that already reached the end of its input.
                    if previous_end == Some(input.text.len()) {
                        return false;
                    }
                    let Some(found) = self.find_at(&input.text, pos) else {
                        return false;
                    };
                    previous_end = Some(found.end);
                    let start = rope.byte_to_char(byte_offset + input.original_offset(found.start));
                    let end = rope.byte_to_char(byte_offset + input.original_offset(found.end));
                    if bounds.is_none_or(|(low, high)| start >= low && end <= high)
                        && (!self.whole_word
                            || is_whole_word_match(&input.text, found.start, found.end))
                    {
                        if matches.len() == limit {
                            return true;
                        }
                        let row = rope.char_to_line(start);
                        let line_start = rope.line_to_char(row);
                        matches.push((row, start - line_start, end - line_start));
                    }
                    pos = if found.start == found.end {
                        found.end
                            + input.text[found.end..]
                                .chars()
                                .next()
                                .map_or(1, char::len_utf8)
                    } else {
                        found.end
                    };
                    if pos > input.text.len() {
                        return false;
                    }
                }
            };
            let truncated = if self.needs_document {
                let (start, end) = bounds.unwrap_or((0, rope.len_chars()));
                let mut text = String::new();
                for chunk in rope.slice(start..end).chunks() {
                    if cancel.is_cancelled() {
                        return (Vec::new(), false);
                    }
                    text.push_str(chunk);
                }
                scan(&text, rope.char_to_byte(start))
            } else {
                let mut limited = false;
                for row in first_line.min(rope.len_lines())..last_line.min(rope.len_lines()) {
                    if cancel.is_cancelled() {
                        return (Vec::new(), false);
                    }
                    let line_start = rope.line_to_char(row);
                    let line = rope.line(row);
                    let text = line.to_string();
                    let clean = text.trim_end_matches(['\r', '\n']);
                    let line_end = line_start + clean.chars().count();
                    let (start, end) = bounds.map_or((line_start, line_end), |(low, high)| {
                        (low.max(line_start), high.min(line_end))
                    });
                    if start > end {
                        continue;
                    }
                    let text = rope.slice(start..end).to_string();
                    if scan(&text, rope.char_to_byte(start)) {
                        limited = true;
                        break;
                    }
                }
                limited
            };
            if cancel.is_cancelled() {
                (Vec::new(), false)
            } else {
                (matches, truncated)
            }
        }
    }
}
