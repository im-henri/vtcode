//! Complete command transcripts, with bounded memory and indexed disk pages.
//! The ACP outputByteLimit applies to protocol replies, never to this UI log.
use super::terminal_style::{Decoder, MAX_LINE_CHARS, TerminalLine};
use std::{
    fs::{self, File},
    io::{self, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::Mutex,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub const TERMINAL_ROWS: usize = 10;

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TerminalPage {
    pub version: u64,
    pub first: usize,
    pub total: usize,
    pub max_columns: usize,
    pub lines: Vec<String>,
    pub styled: Vec<TerminalLine>,
}

pub struct TerminalHistory {
    backend: Backend,
}

pub trait HistorySource: Send + Sync {
    fn page(&self, first: Option<usize>, column: usize, width: usize, height: usize) -> io::Result<TerminalPage>;
}
enum Backend {
    Local(Mutex<State>),
    Hosted(std::sync::Arc<dyn HistorySource>),
}

impl std::fmt::Debug for TerminalHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalHistory").finish_non_exhaustive()
    }
}

struct State {
    text: BufWriter<tempfile::NamedTempFile>,
    index: BufWriter<tempfile::NamedTempFile>,
    bytes: u64,
    lines: usize,
    version: u64,
    max_columns: usize,
    decoder: Decoder,
    error: Option<String>,
}

impl TerminalHistory {
    pub fn hosted(source: std::sync::Arc<dyn HistorySource>) -> Self {
        Self { backend: Backend::Hosted(source) }
    }
    fn local(&self) -> io::Result<&Mutex<State>> {
        match &self.backend {
            Backend::Local(state) => Ok(state),
            Backend::Hosted(_) => Err(io::Error::new(io::ErrorKind::Unsupported, "Hosted terminal history is read-only")),
        }
    }
    pub fn new(directory: &Path) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        Ok(Self {
            backend: Backend::Local(Mutex::new(State {
                text: BufWriter::new(
                    tempfile::Builder::new()
                        .prefix("agent-terminal-")
                        .suffix(".log")
                        .tempfile_in(directory)?,
                ),
                index: BufWriter::new(
                    tempfile::Builder::new()
                        .prefix("agent-terminal-")
                        .suffix(".idx")
                        .tempfile_in(directory)?,
                ),
                bytes: 0,
                lines: 0,
                version: 0,
                max_columns: 0,
                decoder: Decoder::default(),
                error: None,
            })),
        })
    }

    pub fn append(&self, bytes: &[u8]) -> io::Result<u64> {
        let mut state = self
            .local()?
            .lock()
            .map_err(|_| io::Error::other("Terminal log lock failed"))?;
        state.decoder.append(bytes);
        state.flush_completed();
        if let Err(error) = state.text.flush().and_then(|_| state.index.flush()) {
            state.error = Some(error.to_string());
        }
        state.version += 1;
        if let Some(error) = &state.error {
            return Err(io::Error::other(error.clone()));
        }
        Ok(state.version)
    }

    /// Input is a distinct record. It is never mixed into the agent's stdout.
    pub fn input(&self, text: &str) -> io::Result<u64> {
        let mut state = self
            .local()?
            .lock()
            .map_err(|_| io::Error::other("Terminal log lock failed"))?;
        state.decoder.input(text);
        state.flush_completed();
        if let Err(error) = state.text.flush().and_then(|_| state.index.flush()) {
            state.error = Some(error.to_string());
        }
        state.version += 1;
        if let Some(error) = &state.error {
            return Err(io::Error::other(error.clone()));
        }
        Ok(state.version)
    }

    /// Called only from a worker. Completed lines are immutable; only the short
    /// unfinished line is copied under the producer lock. Reads never seek the
    /// writer's file handles and never scan earlier output to find a page.
    pub fn page(
        &self,
        first: Option<usize>,
        column: usize,
        width: usize,
        height: usize,
    ) -> io::Result<TerminalPage> {
        if let Backend::Hosted(source) = &self.backend {
            return source.page(first, column, width, height.clamp(1, TERMINAL_ROWS));
        }
        let (mut text, mut index, bytes, complete, tail, version, max_columns) = {
            let state = self
                .local()?
                .lock()
                .map_err(|_| io::Error::other("Terminal log lock failed"))?;
            if let Some(error) = &state.error {
                return Err(io::Error::other(error.clone()));
            }
            (
                File::open(state.text.get_ref().path())?,
                File::open(state.index.get_ref().path())?,
                state.bytes,
                state.lines,
                state.decoder.current_line(),
                state.version,
                state.max_columns,
            )
        };
        let total = complete + usize::from(!tail.text.is_empty());
        let height = height.clamp(1, TERMINAL_ROWS);
        let first = first
            .unwrap_or(total.saturating_sub(height))
            .min(total.saturating_sub(height));
        let end = (first + height).min(total);
        let mut styled = Vec::new();
        for line in first..end {
            let value = if line == complete {
                tail.clone()
            } else {
                index.seek(SeekFrom::Start((line as u64) * 8))?;
                let mut offset = [0; 8];
                index.read_exact(&mut offset)?;
                let start = u64::from_le_bytes(offset);
                let end = if line + 1 == complete {
                    bytes
                } else {
                    index.read_exact(&mut offset)?;
                    u64::from_le_bytes(offset)
                };
                let length = end
                    .checked_sub(start)
                    .filter(|n| *n <= (MAX_LINE_CHARS * 256 + 256) as u64)
                    .ok_or_else(|| io::Error::other("Invalid terminal log index"))?;
                text.seek(SeekFrom::Start(start))?;
                let mut data = vec![0; length as usize];
                text.read_exact(&mut data)?;
                serde_json::from_slice::<TerminalLine>(&data).map_err(io::Error::other)?
            };
            styled.push(value.window(column, width));
        }
        Ok(TerminalPage {
            version,
            first,
            total,
            max_columns: max_columns.max(tail.text.width()),
            lines: styled.iter().map(|line| line.text.clone()).collect(),
            styled,
        })
    }
}

pub fn window(text: &str, column: usize, width: usize) -> String {
    let mut result = String::new();
    let mut x = 0;
    for grapheme in text.graphemes(true) {
        let next = x + grapheme.width();
        if next > column.saturating_add(width) {
            break;
        }
        if x >= column {
            result.push_str(grapheme);
        } else if next > column {
            result.extend(std::iter::repeat_n(' ', next - column));
        }
        x = next;
    }
    result
}

impl State {
    fn flush_completed(&mut self) {
        for line in std::mem::take(&mut self.decoder.lines) {
            if self.error.is_some() {
                return;
            }
            self.max_columns = self.max_columns.max(line.text.width());
            let data = match serde_json::to_vec(&line) {
                Ok(data) => data,
                Err(error) => {
                    self.error = Some(error.to_string());
                    return;
                }
            };
            let result = self
                .text
                .write_all(&data)
                .and_then(|_| self.index.write_all(&self.bytes.to_le_bytes()));
            if let Err(error) = result {
                self.error = Some(error.to_string());
                return;
            }
            self.bytes += data.len() as u64;
            self.lines += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_history_preserves_color_across_chunks_pages_and_input() {
        use super::super::terminal_style::AnsiColor;
        let root = tempfile::tempdir().unwrap();
        let log = TerminalHistory::new(root.path()).unwrap();
        for byte in
            "\x1b[1;31mred\n世界\x1b[38;2;10;20;30mRGB\x1b[0m plain\n\x1b]52;c;ignored\x07safe\n"
                .as_bytes()
        {
            log.append(&[*byte]).unwrap();
        }
        let page = log.page(Some(1), 2, 80, 1).unwrap();
        assert_eq!(page.lines, ["界RGB plain"]);
        let runs = &page.styled[0].runs;
        assert_eq!(runs[0].style.fg, Some(AnsiColor::Indexed(1)));
        assert_eq!(runs[0].style.attributes, 1);
        assert_eq!(runs[1].style.fg, Some(AnsiColor::Rgb(10, 20, 30)));
        assert_eq!(runs[2].style, Default::default());
        assert_eq!(log.page(Some(2), 0, 80, 1).unwrap().lines, ["safe"]);
        log.append(b"\x1b[32mcontinued").unwrap();
        log.input("user text").unwrap();
        log.append(b"still green\n").unwrap();
        let page = log.page(None, 0, 80, 3).unwrap();
        assert_eq!(
            page.lines,
            ["continued", "[user input] user text", "still green"]
        );
        assert_eq!(page.styled[0].runs[0].style.fg, Some(AnsiColor::Indexed(2)));
        assert_eq!(page.styled[1].runs[0].style, Default::default());
        assert_eq!(page.styled[2].runs[0].style.fg, Some(AnsiColor::Indexed(2)));
    }
    #[test]
    fn terminal_history_retains_every_page_and_input_without_protocol_limits() {
        use std::fmt::Write as _;

        let root = tempfile::tempdir().unwrap();
        let log = TerminalHistory::new(root.path()).unwrap();
        // Keep the full transcript size without turning fixture construction
        // into 40,000 file flushes. Split-byte decoding is covered separately.
        for first in (0..20_000).step_by(256) {
            let mut chunk = String::new();
            for i in first..(first + 256).min(20_000) {
                writeln!(chunk, "line-{i:05} 世界").unwrap();
            }
            log.append(chunk.as_bytes()).unwrap();
        }
        log.input("hello").unwrap();
        let first = log.page(Some(0), 0, 80, 10).unwrap();
        assert_eq!(first.lines[0], "line-00000 世界");
        assert_eq!(first.lines.len(), 10);
        let middle = log.page(Some(10_000), 0, 80, 10).unwrap();
        assert_eq!(middle.lines[0], "line-10000 世界");
        let last = log.page(None, 0, 80, 10).unwrap();
        assert_eq!(last.lines.last().unwrap(), "[user input] hello");
        assert_eq!(first.total, 20_001);
        drop(log);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }
    #[test]
    fn terminal_history_handles_split_escapes_progress_utf8_and_horizontal_scroll() {
        let root = tempfile::tempdir().unwrap();
        let log = TerminalHistory::new(root.path()).unwrap();
        for byte in "\u{1b}[31mprogress 5%\r\u{1b}[2Kdone 世界\u{1b}[0m\n".as_bytes() {
            log.append(&[*byte]).unwrap();
        }
        assert_eq!(log.page(Some(0), 0, 80, 10).unwrap().lines, ["done 世界"]);
        assert_eq!(log.page(Some(0), 5, 4, 10).unwrap().lines, ["世界"]);
        log.append("x".repeat(MAX_LINE_CHARS + 7).as_bytes())
            .unwrap();
        assert_eq!(
            log.page(Some(0), MAX_LINE_CHARS - 3, 10, 10).unwrap().lines[1],
            "xxx"
        );
        assert_eq!(log.page(None, 0, 10, 1).unwrap().lines, ["xxxxxxx"]);
    }
}
