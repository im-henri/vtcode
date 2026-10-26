//! Stream ordinary disk searches without constructing an editor Rope. Unusual
//! encodings and multiline patterns retain the existing decoded search path.
use super::{Engine, SearchRegex};
use crate::background::CancellationToken;
use grep_matcher::Matcher;
use std::{
    io::{self, Read},
    path::Path,
    sync::Arc,
};

thread_local! {
    // A workspace worker scans many small files with the same configuration.
    // Retain the searcher's rolling buffers for its lifetime instead of
    // allocating and discarding them for every path.
    static SEARCHER: std::cell::RefCell<grep_searcher::Searcher> = std::cell::RefCell::new(
        grep_searcher::SearcherBuilder::new()
            .line_terminator(grep_matcher::LineTerminator::crlf())
            .line_number(true).bom_sniffing(false).build()
    );
}

pub(crate) struct DiskMatch {
    pub row: usize,
    pub start: usize,
    pub end: usize,
    pub preview: Arc<str>,
}

impl SearchRegex {
    /// `None` requests the decoded Rope fallback; no provisional results escape
    /// when invalid UTF-8, bare CR line endings or binary bytes are discovered.
    pub(crate) fn scan_disk(
        &self,
        path: &Path,
        limit: usize,
        cancel: &CancellationToken,
    ) -> Option<(Vec<DiskMatch>, bool)> {
        if self.needs_document || matches!(self.engine, Engine::JavaScript(_)) {
            return None;
        }
        if cancel.is_cancelled() {
            return Some((Vec::new(), false));
        }
        let mut file = std::fs::File::open(path).ok()?;
        let mut header = Vec::with_capacity(8192);
        file.by_ref().take(8192).read_to_end(&mut header).ok()?;
        if header.starts_with(&[0xff, 0xfe]) || header.starts_with(&[0xfe, 0xff]) {
            return None;
        }
        let start = usize::from(header.starts_with(&[0xef, 0xbb, 0xbf])) * 3;
        let mut input = CheckedReader::new(header[start..].chain(file), cancel);
        match &self.engine {
            Engine::Ripgrep(matcher) => scan(matcher, &mut input, limit, cancel),
            Engine::Pcre2(matcher) => scan(matcher, &mut input, limit, cancel),
            Engine::JavaScript(_) => unreachable!(),
        }
    }
}

/// Validate incrementally, including code points and CRLF split across reads.
/// Detecting NUL anywhere in a file must discard even its earlier matches.
struct CheckedReader<'a, R> {
    input: R,
    cancel: &'a CancellationToken,
    utf8_tail: [u8; 4],
    tail_len: usize,
    pending_cr: bool,
    fallback: bool,
    binary: bool,
}

impl<'a, R> CheckedReader<'a, R> {
    fn new(input: R, cancel: &'a CancellationToken) -> Self {
        Self {
            input,
            cancel,
            utf8_tail: [0; 4],
            tail_len: 0,
            pending_cr: false,
            fallback: false,
            binary: false,
        }
    }

    fn validate(&mut self, mut bytes: &[u8]) {
        self.binary |= memchr::memchr(0, bytes).is_some();
        if self.pending_cr && bytes.first() != Some(&b'\n') {
            self.fallback = true;
        }
        self.pending_cr = bytes.last() == Some(&b'\r');
        for index in memchr::memchr_iter(b'\r', bytes) {
            if index + 1 < bytes.len() && bytes[index + 1] != b'\n' {
                self.fallback = true;
            }
        }
        while self.tail_len > 0 && !bytes.is_empty() && !self.fallback {
            self.utf8_tail[self.tail_len] = bytes[0];
            self.tail_len += 1;
            bytes = &bytes[1..];
            match std::str::from_utf8(&self.utf8_tail[..self.tail_len]) {
                Ok(_) => self.tail_len = 0,
                Err(error) if error.error_len().is_some() => self.fallback = true,
                Err(_) => {}
            }
        }
        if !bytes.is_empty() && !self.fallback {
            if let Err(error) = std::str::from_utf8(bytes) {
                if error.error_len().is_some() {
                    self.fallback = true;
                } else {
                    let tail = &bytes[error.valid_up_to()..];
                    self.utf8_tail[..tail.len()].copy_from_slice(tail);
                    self.tail_len = tail.len();
                }
            }
        }
    }
}

impl<R: Read> Read for CheckedReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() || self.cancel.is_cancelled() || self.binary || self.fallback {
            return Ok(0);
        }
        let count = self.input.read(bytes)?;
        if count == 0 {
            self.fallback |= self.tail_len != 0 || self.pending_cr;
        } else {
            self.validate(&bytes[..count]);
        }
        Ok(if self.fallback || self.binary {
            0
        } else {
            count
        })
    }
}

struct MatchSink<'a, M> {
    matcher: &'a M,
    cancel: &'a CancellationToken,
    matches: Vec<DiskMatch>,
    limit: usize,
    truncated: bool,
    fallback: bool,
}

impl<M: Matcher> grep_searcher::Sink for MatchSink<'_, M> {
    type Error = io::Error;

    fn matched(
        &mut self,
        searcher: &grep_searcher::Searcher,
        mat: &grep_searcher::SinkMatch<'_>,
    ) -> io::Result<bool> {
        if self.cancel.is_cancelled() {
            return Ok(false);
        }
        let range = mat.bytes_range_in_buffer();
        let buffer = mat.buffer();
        let mut end = range.end;
        if searcher.line_terminator().is_suffix(&buffer[..end]) {
            end -= 1;
            if end > 0 && buffer[end - 1] == b'\r' {
                end -= 1;
            }
        }
        let Ok(line) = std::str::from_utf8(&buffer[range.clone()]) else {
            self.fallback = true;
            return Ok(false);
        };
        let row = mat.line_number().unwrap_or(1) as usize - 1;
        let mut preview: Option<Arc<str>> = None;
        let mut previous = 0;
        let mut column = 0;
        let mut found_any = false;
        let mut keep_going = true;
        self.matcher
            .find_iter_at(&buffer[..end], range.start, |found| {
                if found.start() >= range.end {
                    return false;
                }
                found_any = true;
                if self.cancel.is_cancelled() {
                    keep_going = false;
                    return false;
                }
                if self.matches.len() == self.limit {
                    self.truncated = true;
                    keep_going = false;
                    return false;
                }
                let start = found.start() - range.start;
                let end = found.end() - range.start;
                if !line.is_char_boundary(start) || !line.is_char_boundary(end) || end > line.len()
                {
                    self.fallback = true;
                    keep_going = false;
                    return false;
                }
                column += line[previous..start].chars().count();
                let start_col = column;
                column += line[start..end].chars().count();
                previous = end;
                let preview =
                    preview.get_or_insert_with(|| Arc::from(line.trim_end_matches(['\r', '\n'])));
                self.matches.push(DiskMatch {
                    row,
                    start: start_col,
                    end: column,
                    preview: preview.clone(),
                });
                true
            })
            .map_err(|error| io::Error::other(error.to_string()))?;
        if !found_any {
            // Preserve the ripgrep/VS Code first-character fallback (e.g. /\r/).
            if self.matches.len() == self.limit {
                self.truncated = true;
                return Ok(false);
            }
            let width = usize::from(!line.is_empty());
            let keep = line
                .trim_end_matches(['\r', '\n'])
                .len()
                .max(line.chars().next().map_or(0, char::len_utf8));
            self.matches.push(DiskMatch {
                row,
                start: 0,
                end: width,
                preview: Arc::from(line[..keep].replace('\r', " ").replace('\n', "↵")),
            });
        }
        Ok(keep_going)
    }
}

fn scan<M: Matcher, R: Read>(
    matcher: &M,
    input: &mut CheckedReader<'_, R>,
    limit: usize,
    cancel: &CancellationToken,
) -> Option<(Vec<DiskMatch>, bool)> {
    let mut sink = MatchSink {
        matcher,
        cancel,
        matches: Vec::new(),
        limit,
        truncated: false,
        fallback: false,
    };
    SEARCHER
        .with(|searcher| {
            searcher
                .borrow_mut()
                .search_reader(matcher, &mut *input, &mut sink)
        })
        .ok()?;
    // The matcher can stop at the result limit before the file ends. Finish
    // validating without running the regex again, so late binary data cannot
    // leak text matches or a false "results truncated" warning.
    io::copy(input, &mut io::sink()).ok()?;
    if cancel.is_cancelled() || input.binary {
        return Some((Vec::new(), false));
    }
    if input.fallback || sink.fallback {
        return None;
    }
    Some((sink.matches, sink.truncated))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ShortReads<'a> {
        bytes: &'a [u8],
        max: usize,
    }
    impl Read for ShortReads<'_> {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let len = output.len().min(self.max);
            self.bytes.read(&mut output[..len])
        }
    }

    #[test]
    fn streaming_validation_handles_split_unicode_crlf_invalid_and_binary_bytes() {
        let cancel = CancellationToken::new();
        for (bytes, fallback, binary) in [
            ("é😀\r\nplain\n".as_bytes(), false, false),
            (b"\xf0\x9f\x98".as_slice(), true, false),
            (b"\xe2\x82\xff".as_slice(), true, false),
            (b"x\rnext".as_slice(), true, false),
            (b"last\r".as_slice(), true, false),
            (b"text\0more".as_slice(), false, true),
        ] {
            for max in 1..=9 {
                let mut input = CheckedReader::new(ShortReads { bytes, max }, &cancel);
                let mut actual = Vec::new();
                input.read_to_end(&mut actual).unwrap();
                assert_eq!(
                    (input.fallback, input.binary),
                    (fallback, binary),
                    "bytes={bytes:?} max={max}"
                );
                if !fallback && !binary {
                    assert_eq!(actual, bytes);
                }
            }
        }
    }

    #[test]
    fn direct_search_uses_streaming_for_normal_text_and_falls_back_for_unusual_text() {
        let workspace = tempfile::tempdir().unwrap();
        let path = workspace.path().join("fixture");
        let cancel = CancellationToken::new();
        let pattern = SearchRegex::workspace("needle", true, false, false).unwrap();
        for text in ["é😀 needle\r\nneedle", "\u{feff}é😀 needle\r\nneedle"] {
            std::fs::write(&path, text).unwrap();
            let (matches, truncated) = pattern.scan_disk(&path, 10, &cancel).unwrap();
            assert_eq!(matches.len(), 2);
            assert_eq!(
                (matches[0].row, matches[0].start, matches[0].end),
                (0, 3, 9)
            );
            assert!(!truncated);
        }
        for bytes in [b"\xffneedle".as_slice(), b"needle\rneedle", b"\xff\xfen\0"] {
            std::fs::write(&path, bytes).unwrap();
            assert!(pattern.scan_disk(&path, 10, &cancel).is_none());
        }
        cancel.cancel();
        assert!(pattern.scan_disk(&path, 10, &cancel).unwrap().0.is_empty());
    }
}
