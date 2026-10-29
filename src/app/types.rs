use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::editor::Editor;

#[cfg(test)]
#[path = "../diff_text_index_performance_tests.rs"]
mod diff_text_index_performance_tests;

/// Flatten the provider's label once, retaining the kind needed for the
/// generic, type, and parameter theme roles.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InlayHintDisplay {
    pub line: u32,
    pub column: u32,
    pub label: String,
    pub kind: Option<lsp_types::InlayHintKind>,
}

impl InlayHintDisplay {
    pub(crate) fn new(line: u32, column: u32, label: impl Into<String>, kind: Option<lsp_types::InlayHintKind>) -> Self {
        Self { line, column, label: label.into(), kind }
    }

    pub(crate) fn color_kind_index(&self) -> usize {
        match self.kind {
            Some(lsp_types::InlayHintKind::TYPE) => 1,
            Some(lsp_types::InlayHintKind::PARAMETER) => 2,
            _ => 0,
        }
    }
}

/// Compute a unified diff between two file contents using the `similar` crate.
/// Returns a Vec of (kind, text) where kind is ' ' (equal), '+' (added in target), '-' (removed from base).
pub(crate) fn compute_file_diff(base: &str, target: &str) -> Vec<(char, String)> {
    use similar::{ChangeTag, TextDiff};
    // Normalize line endings so CRLF/LF differences don't mark every line as changed.
    let base_normalized = base.replace("\r\n", "\n").replace('\r', "\n");
    let target_normalized = target.replace("\r\n", "\n").replace('\r', "\n");
    let diff = TextDiff::from_lines(&base_normalized, &target_normalized);
    let mut result = Vec::new();
    for change in diff.iter_all_changes() {
        let kind = match change.tag() {
            ChangeTag::Equal => ' ',
            ChangeTag::Delete => '-',
            ChangeTag::Insert => '+',
        };
        let text = change.value().trim_end_matches(['\n', '\r']).to_string();
        result.push((kind, text));
    }
    result
}

/// Compute per-character diff highlights for a pair of old/new lines.
/// Returns (old_highlights, new_highlights) where `true` = this char changed.
pub(crate) fn compute_char_diff(old: &str, new: &str) -> (Vec<bool>, Vec<bool>) {
    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_chars(old, new);
    let mut old_highlights = Vec::with_capacity(old.len());
    let mut new_highlights = Vec::with_capacity(new.len());
    for change in diff.iter_all_changes() {
        let char_count = change.value().chars().count();
        match change.tag() {
            ChangeTag::Equal => {
                old_highlights.extend(std::iter::repeat_n(false, char_count));
                new_highlights.extend(std::iter::repeat_n(false, char_count));
            }
            ChangeTag::Delete => {
                old_highlights.extend(std::iter::repeat_n(true, char_count));
            }
            ChangeTag::Insert => {
                new_highlights.extend(std::iter::repeat_n(true, char_count));
            }
        }
    }
    (old_highlights, new_highlights)
}

#[derive(Clone, Debug)]
pub(crate) struct AlignedDiffRow {
    pub hidden_lines: usize,
    pub left_kind: char,
    pub left_line: Option<usize>,
    pub left_text: String,
    /// Per-char highlight flags for left text (true = changed char, use highlight color)
    pub left_char_highlights: Option<Vec<bool>>,
    pub right_kind: char,
    pub right_line: Option<usize>,
    pub right_text: String,
    /// Per-char highlight flags for right text (true = changed char, use highlight color)
    pub right_char_highlights: Option<Vec<bool>>,
    left_text_index: DiffTextIndex,
    right_text_index: DiffTextIndex,
    left_highlight_hash: u64,
    right_highlight_hash: u64,
}

const DIFF_TEXT_INDEX_STRIDE: usize = 256;

/// Source attachment facts prepared alongside the immutable character index.
/// Two bits per delimiter retain only zero, one, or repeated occurrences.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DiffScopeLineSummary {
    first: usize,
    branch: usize,
    first_char: char,
    delimiters: u16,
}

impl Default for DiffScopeLineSummary {
    fn default() -> Self {
        Self {
            first: usize::MAX,
            branch: usize::MAX,
            first_char: '\0',
            delimiters: 0,
        }
    }
}

impl DiffScopeLineSummary {
    pub(crate) fn from_text(text: &str) -> Self {
        let mut summary = Self::default();
        for (column, ch) in text.chars().enumerate() {
            summary.observe(column, ch);
        }
        summary
    }

    fn slot(ch: char) -> Option<u32> {
        match ch {
            '{' => Some(0),
            '}' => Some(1),
            '[' => Some(2),
            ']' => Some(3),
            '(' => Some(4),
            ')' => Some(5),
            '<' => Some(6),
            '>' => Some(7),
            _ => None,
        }
    }

    fn observe(&mut self, column: usize, ch: char) {
        if !matches!(ch, ' ' | '\t') && !ch.is_control() {
            if self.first == usize::MAX {
                self.first = column;
                self.first_char = ch;
            } else if self.branch == usize::MAX
                && matches!(self.first_char, '}' | ']' | ')' | '>')
                && ch != self.first_char
            {
                // Only the leading closer run determines branch attachment;
                // later closers may belong to strings or comments.
                self.branch = column;
            }
        }
        if let Some(slot) = Self::slot(ch) {
            let shift = slot * 2;
            if (self.delimiters >> shift) & 3 < 2 {
                self.delimiters += 1 << shift;
            }
        }
    }

    pub(crate) fn first(self) -> Option<(usize, char)> {
        (self.first != usize::MAX).then_some((self.first, self.first_char))
    }

    pub(crate) fn repeated(self, delimiter: char) -> bool {
        Self::slot(delimiter).is_some_and(|slot| (self.delimiters >> (slot * 2)) & 3 > 1)
    }

    pub(crate) fn branch(self) -> Option<(char, usize)> {
        (self.branch != usize::MAX).then_some((self.first_char, self.branch))
    }
}

#[derive(Clone, Debug)]
struct DiffCellCheckpoint {
    byte: usize,
    character: usize,
    cell: usize,
}

#[derive(Clone, Debug, Default)]
struct DiffTextIndex {
    char_len: usize,
    cell_len: usize,
    scope_summary: DiffScopeLineSummary,
    /// UTF-8 byte offset for every `DIFF_TEXT_INDEX_STRIDE`th character.
    byte_checkpoints: Vec<usize>,
    /// Grapheme boundaries prepared with the immutable diff, independently of
    /// pane width. Horizontal redraws can seek without decoding from byte zero.
    cell_checkpoints: Vec<DiffCellCheckpoint>,
}

impl DiffTextIndex {
    fn new(text: &str) -> Self {
        if text.is_ascii() {
            let mut scope_summary = DiffScopeLineSummary::default();
            let mut has_controls = false;
            for (column, byte) in text.bytes().enumerate() {
                if byte.is_ascii_control() {
                    has_controls = true;
                    break;
                }
                scope_summary.observe(column, char::from(byte));
            }
            if !has_controls {
                return Self {
                    char_len: text.len(),
                    cell_len: text.len(),
                    scope_summary,
                    byte_checkpoints: Vec::new(),
                    cell_checkpoints: Vec::new(),
                };
            }
        }
        let mut byte_checkpoints = Vec::new();
        let mut cell_checkpoints = Vec::new();
        let mut char_len = 0usize;
        let mut cell = 0usize;
        let mut scope_summary = DiffScopeLineSummary::default();
        let mut next_checkpoint = 0usize;
        for (byte, grapheme) in text.grapheme_indices(true) {
            if char_len >= next_checkpoint {
                cell_checkpoints.push(DiffCellCheckpoint {
                    byte,
                    character: char_len,
                    cell,
                });
                next_checkpoint = char_len.saturating_add(DIFF_TEXT_INDEX_STRIDE);
            }
            for (offset, ch) in grapheme.char_indices() {
                scope_summary.observe(char_len, ch);
                if char_len.is_multiple_of(DIFF_TEXT_INDEX_STRIDE) {
                    byte_checkpoints.push(byte + offset);
                }
                char_len += 1;
            }
            if !grapheme.contains(char::is_control) {
                cell += grapheme.width();
            }
        }
        Self {
            char_len,
            cell_len: cell,
            scope_summary,
            byte_checkpoints,
            cell_checkpoints,
        }
    }

    fn character_at_byte(&self, text: &str, byte: usize) -> usize {
        if self.char_len == text.len() {
            return byte;
        }
        let checkpoint = self
            .byte_checkpoints
            .partition_point(|&offset| offset <= byte)
            .saturating_sub(1);
        let start = self.byte_checkpoints.get(checkpoint).copied().unwrap_or(0);
        checkpoint * DIFF_TEXT_INDEX_STRIDE + text[start..byte].chars().count()
    }

    fn inline_window<'a>(
        &self,
        text: &'a str,
        column: usize,
        width: usize,
    ) -> (&'a str, usize, usize) {
        if self.char_len == text.len() && self.cell_len == self.char_len {
            let start = column.min(text.len());
            return (
                &text[start..start.saturating_add(width).min(text.len())],
                start,
                0,
            );
        }
        // Strict comparison retains leading zero-width graphemes at an exact
        // column boundary, matching a scan of the complete source line.
        let checkpoint = self
            .cell_checkpoints
            .partition_point(|entry| entry.cell < column)
            .saturating_sub(1);
        let (byte, character, mut cell) = self
            .cell_checkpoints
            .get(checkpoint)
            .map_or((0, 0, 0), |entry| (entry.byte, entry.character, entry.cell));
        let mut padding = 0usize;
        for (offset, grapheme) in text[byte..].grapheme_indices(true) {
            let cells = if grapheme.contains(char::is_control) {
                0
            } else {
                grapheme.width()
            };
            if cell < column {
                padding = cell.saturating_add(cells).saturating_sub(column).min(width);
                cell += cells;
                continue;
            }
            let start_byte = byte + offset;
            let tail = &text[start_byte..];
            let mut used = padding;
            let mut end = tail.len();
            for (offset, grapheme) in tail.grapheme_indices(true) {
                if !grapheme.contains(char::is_control) {
                    used += grapheme.width();
                }
                if used > width {
                    end = offset;
                    break;
                }
            }
            let character = if start_byte - byte <= DIFF_TEXT_INDEX_STRIDE * 4 {
                character + text[byte..start_byte].chars().count()
            } else {
                self.character_at_byte(text, start_byte)
            };
            return (&tail[..end], character, padding);
        }
        ("", self.char_len, padding)
    }

    fn column_at_cell(&self, text: &str, target: usize) -> usize {
        if target >= self.cell_len { return self.char_len; }
        if self.char_len == text.len() && self.cell_len == self.char_len { return target; }
        let checkpoint = self.cell_checkpoints
            .partition_point(|entry| entry.cell <= target).saturating_sub(1);
        let (byte, mut character, mut cell) = self.cell_checkpoints.get(checkpoint)
            .map_or((0, 0, 0), |entry| (entry.byte, entry.character, entry.cell));
        for grapheme in text[byte..].graphemes(true) {
            let width = if grapheme.contains(char::is_control) { 0 } else { grapheme.width() };
            if target < cell + width { return character; }
            cell += width;
            character += grapheme.chars().count();
        }
        self.char_len
    }

    fn cell_column(&self, text: &str, column: usize) -> usize {
        if column >= self.char_len {
            return self.cell_len;
        }
        if self.char_len == text.len() && self.cell_len == self.char_len {
            return column;
        }
        let checkpoint = self
            .cell_checkpoints
            .partition_point(|entry| entry.character <= column)
            .saturating_sub(1);
        let (byte, mut character, mut cell) = self
            .cell_checkpoints
            .get(checkpoint)
            .map_or((0, 0, 0), |entry| (entry.byte, entry.character, entry.cell));
        for grapheme in text[byte..].graphemes(true) {
            let end = character + grapheme.chars().count();
            if column < end {
                break;
            }
            if !grapheme.contains(char::is_control) {
                cell += grapheme.width();
            }
            character = end;
            if character == column {
                break;
            }
        }
        cell
    }

    fn window<'a>(&self, text: &'a str, start: usize, width: usize) -> &'a str {
        if width == 0 || start >= self.char_len {
            return "";
        }
        if self.char_len == text.len() {
            let end = start.saturating_add(width).min(text.len());
            return &text[start..end];
        }

        let bucket = start / DIFF_TEXT_INDEX_STRIDE;
        let checkpoint_char = bucket * DIFF_TEXT_INDEX_STRIDE;
        let checkpoint_byte = self.byte_checkpoints.get(bucket).copied().unwrap_or(0);
        let start_byte = text[checkpoint_byte..]
            .char_indices()
            .nth(start.saturating_sub(checkpoint_char))
            .map_or(text.len(), |(offset, _)| checkpoint_byte + offset);
        let tail = &text[start_byte..];
        let end_byte = tail
            .char_indices()
            .nth(width)
            .map_or(tail.len(), |(offset, _)| offset);
        &tail[..end_byte]
    }
}
impl AlignedDiffRow {
    #[allow(clippy::too_many_arguments)]
    fn new(
        left_kind: char,
        left_line: Option<usize>,
        left_text: String,
        left_char_highlights: Option<Vec<bool>>,
        right_kind: char,
        right_line: Option<usize>,
        right_text: String,
        right_char_highlights: Option<Vec<bool>>,
    ) -> Self {
        let left_text_index = DiffTextIndex::new(&left_text);
        let right_text_index = DiffTextIndex::new(&right_text);
        let left_highlight_hash = crate::highlight::Highlighter::line_hash(
            left_text_index.window(
                &left_text,
                0,
                crate::highlight::MAX_SYNTAX_LINE_CHARS,
            ),
        );
        let right_highlight_hash = crate::highlight::Highlighter::line_hash(
            right_text_index.window(
                &right_text,
                0,
                crate::highlight::MAX_SYNTAX_LINE_CHARS,
            ),
        );
        Self {
            hidden_lines: 0,
            left_kind,
            left_line,
            left_text,
            left_char_highlights,
            right_kind,
            right_line,
            right_text,
            right_char_highlights,
            left_text_index,
            right_text_index,
            left_highlight_hash,
            right_highlight_hash,
        }
    }

    pub(crate) fn left_char_len(&self) -> usize {
        self.left_text_index.char_len
    }

    pub(crate) fn right_char_len(&self) -> usize {
        self.right_text_index.char_len
    }

    pub(crate) fn left_scope_summary(&self) -> DiffScopeLineSummary {
        self.left_text_index.scope_summary
    }

    pub(crate) fn right_scope_summary(&self) -> DiffScopeLineSummary {
        self.right_text_index.scope_summary
    }

    pub(crate) fn left_cell_len(&self) -> usize {
        self.left_text_index.cell_len
    }

    pub(crate) fn right_cell_len(&self) -> usize {
        self.right_text_index.cell_len
    }

    pub(crate) fn left_cell_column(&self, column: usize) -> usize {
        self.left_text_index.cell_column(&self.left_text, column)
    }

    pub(crate) fn right_cell_column(&self, column: usize) -> usize {
        self.right_text_index.cell_column(&self.right_text, column)
    }

    pub(crate) fn left_column_at_cell(&self, cell: usize) -> usize {
        self.left_text_index.column_at_cell(&self.left_text, cell)
    }

    pub(crate) fn right_column_at_cell(&self, cell: usize) -> usize {
        self.right_text_index.column_at_cell(&self.right_text, cell)
    }

    pub(crate) fn left_text_window(&self, start: usize, width: usize) -> &str {
        self.left_text_index.window(&self.left_text, start, width)
    }

    pub(crate) fn right_text_window(&self, start: usize, width: usize) -> &str {
        self.right_text_index.window(&self.right_text, start, width)
    }

    pub(crate) fn left_inline_text_window(&self, column: usize, width: usize) -> (&str, usize, usize) {
        self.left_text_index.inline_window(&self.left_text, column, width)
    }

    pub(crate) fn right_inline_text_window(&self, column: usize, width: usize) -> (&str, usize, usize) {
        self.right_text_index.inline_window(&self.right_text, column, width)
    }

    pub(crate) fn left_highlight_hash(&self) -> u64 {
        self.left_highlight_hash
    }

    pub(crate) fn right_highlight_hash(&self) -> u64 {
        self.right_highlight_hash
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CachedInlineDiffRow {
    pub aligned_row: usize,
    pub kind: char,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CachedDiffHunk {
    pub target_start: usize,
    pub target_end: usize,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct DiffLabels {
    pub base: String,
    pub target: String,
}

impl DiffLabels {
    pub(crate) fn new(base: impl Into<String>, target: impl Into<String>) -> Self {
        Self {
            base: base.into(),
            target: target.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DiffBaseRevision {
    Snapshot,
    OpenEditor {
        content_version: u64,
        line_count: usize,
        last_disk_modified: Option<std::time::SystemTime>,
    },
    DiskFile {
        len: u64,
        modified: Option<std::time::SystemTime>,
    },
    Missing,
}

/// Precomputed row layout used by both diff panes.
///
/// Building this includes cloning line text and calculating character-level
/// replacements, so it must happen when the diff changes rather than once per
/// rendered frame.
#[derive(Clone, Debug)]
pub(crate) struct DiffRenderCache {
    /// Bounded original/target headers survive scrolling and compact views.
    pub syntax_first_lines: [String; 2],
    pub context_lines: Option<usize>,
    pub visible_target_lines: Vec<usize>,
    aligned_row_targets: Vec<usize>,
    inline_row_targets: Vec<usize>,
    pub rows: Vec<AlignedDiffRow>,
    pub inline_rows: Vec<CachedInlineDiffRow>,
    pub hunks: Vec<CachedDiffHunk>,
    pub target_changes: Vec<(usize, char)>,
    pub max_left_line: usize,
    pub max_left_text_width: usize,
    pub max_inline_text_width: usize,
    target_content_lines: usize,
    aligned_target_rows: Vec<usize>,
    inline_target_rows: Vec<usize>,
    selectable_rows: [Vec<usize>; 3],
    overview_rows: [[Vec<usize>; 2]; 2],
}

impl Default for DiffRenderCache {
    fn default() -> Self {
        Self {
            syntax_first_lines: Default::default(),
            context_lines: None,
            visible_target_lines: Vec::new(),
            aligned_row_targets: Vec::new(),
            inline_row_targets: Vec::new(),
            rows: Vec::new(),
            inline_rows: Vec::new(),
            hunks: Vec::new(),
            target_changes: Vec::new(),
            max_left_line: 1,
            max_left_text_width: 0,
            max_inline_text_width: 0,
            target_content_lines: 0,
            aligned_target_rows: vec![0],
            inline_target_rows: vec![0],
            selectable_rows: Default::default(),
            overview_rows: Default::default(),
        }
    }
}

impl DiffRenderCache {
    pub(crate) fn new(diff_lines: &[(char, String)]) -> Self {
        let syntax_first_lines = ['-', '+'].map(|side| {
            let mut line = diff_lines.iter()
                .find(|(kind, _)| *kind == ' ' || *kind == side)
                .map(|(_, text)| text.chars().take(crate::highlight::MAX_SYNTAX_DETECTION_CHARS).collect())
                .unwrap_or_default();
            crate::highlight::Highlighter::normalize_line_for_highlighting(&mut line);
            line
        });
        let syntax_hashes = syntax_first_lines.each_ref().map(|line| crate::highlight::Highlighter::line_hash(line));
        let mut rows = align_diff_rows(diff_lines);
        let max_left_line = rows
            .iter()
            .filter_map(|row| row.left_line)
            .max()
            .map_or(1, |line| line + 1);
        let target_content_lines = rows
            .iter()
            .filter_map(|row| row.right_line)
            .max()
            .map_or(0, |line| line + 1);
        let max_right_line = target_content_lines.max(1);
        let max_left_text_width = rows
            .iter()
            .map(|row| UnicodeWidthStr::width(row.left_text.as_str()))
            .max()
            .unwrap_or(0);

        let last_target_line = max_right_line.saturating_sub(1);
        let max_inline_text_width = rows.iter()
            .map(|row| UnicodeWidthStr::width(row.right_text.as_str()))
            .max().unwrap_or(0).max(max_left_text_width);
        let mut hunks = Vec::new();
        let mut target_statuses = vec![' '; max_right_line];
        let mut target_line = 0usize;
        let mut hunk_start: Option<usize> = None;
        for (kind, _) in diff_lines {
            match *kind {
                ' ' => {
                    if let Some(start) = hunk_start.take() {
                        let target_start = start.min(last_target_line);
                        let target_end = target_line
                            .saturating_sub(1)
                            .min(last_target_line)
                            .max(target_start);
                        hunks.push(CachedDiffHunk {
                            target_start,
                            target_end,
                        });
                    }
                    target_line = target_line.saturating_add(1);
                }
                '+' => {
                    hunk_start.get_or_insert(target_line);
                    if let Some(status) = target_statuses.get_mut(target_line) {
                        *status = 'U';
                    }
                    target_line = target_line.saturating_add(1);
                }
                '-' => {
                    hunk_start.get_or_insert(target_line);
                    let marker_line = target_line.min(last_target_line);
                    if let Some(status) = target_statuses.get_mut(marker_line) {
                        if *status == ' ' {
                            *status = 'D';
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(start) = hunk_start {
            let target_start = start.min(last_target_line);
            let target_end = target_line
                .saturating_sub(1)
                .min(last_target_line)
                .max(target_start);
            hunks.push(CachedDiffHunk {
                target_start,
                target_end,
            });
        }
        let target_changes = target_statuses
            .into_iter()
            .enumerate()
            .filter_map(|(line, status)| (status != ' ').then_some((line, status)))
            .collect();

        let mut aligned_target_rows = vec![0; max_right_line];
        for (row_idx, row) in rows.iter_mut().enumerate() {
            // Identical rows in revisions with different shebangs/modelines
            // must not share colors, including late worker replies.
            use std::hash::Hasher;
            for (hash, syntax_hash) in [&mut row.left_highlight_hash, &mut row.right_highlight_hash]
                .into_iter().zip(syntax_hashes)
            {
                let mut hasher = rustc_hash::FxHasher::default();
                hasher.write_u64(syntax_hash);
                hasher.write_u64(*hash);
                *hash = hasher.finish();
            }
            if let Some(line) = row.right_line {
                aligned_target_rows[line] = row_idx;
            }
        }

        let mut inline_rows = Vec::with_capacity(rows.len());
        for (aligned_row, row) in rows.iter().enumerate() {
            match (row.left_line, row.right_line) {
                (Some(_), Some(_)) if row.left_kind == ' ' && row.right_kind == ' ' => {
                    inline_rows.push(CachedInlineDiffRow {
                        aligned_row,
                        kind: ' ',
                    });
                }
                (left_line, right_line) => {
                    if left_line.is_some() {
                        inline_rows.push(CachedInlineDiffRow {
                            aligned_row,
                            kind: '-',
                        });
                    }
                    if right_line.is_some() {
                        inline_rows.push(CachedInlineDiffRow {
                            aligned_row,
                            kind: '+',
                        });
                    }
                }
            }
        }

        let mut inline_target_rows = vec![0; max_right_line];
        let mut removed_start = None;
        for (inline_row, cached_row) in inline_rows.iter().enumerate() {
            if cached_row.kind == '-' {
                removed_start.get_or_insert(inline_row);
                continue;
            }
            let row = &rows[cached_row.aligned_row];
            let target_line = match cached_row.kind {
                ' ' | '+' => row.right_line,
                _ => None,
            };
            if let Some(line) = target_line {
                // Include the removed text when its replacement (or the next
                // unchanged line) is at the top of the inline viewport.
                inline_target_rows[line] = removed_start.take().unwrap_or(inline_row);
            }
        }

        let selectable_rows = Self::index_selectable_rows(&rows, &inline_rows);
        let overview_rows = Self::index_overview_rows(&rows, &inline_rows);
        Self {
            syntax_first_lines,
            context_lines: None,
            visible_target_lines: Vec::new(),
            aligned_row_targets: Vec::new(),
            inline_row_targets: Vec::new(),
            rows,
            inline_rows,
            hunks,
            target_changes,
            max_left_line,
            max_left_text_width,
            max_inline_text_width,
            target_content_lines,
            aligned_target_rows,
            inline_target_rows,
            selectable_rows,
            overview_rows,
        }
    }

    pub(crate) fn aligned_row_for_target_line(&self, target_line: usize) -> usize {
        self.aligned_target_rows
            .get(target_line)
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn target_content_lines(&self) -> usize {
        self.target_content_lines
    }

    pub(crate) fn inline_row_for_target_line(&self, target_line: usize) -> usize {
        self.inline_target_rows
            .get(target_line)
            .copied()
            .unwrap_or(0)
    }

    pub(crate) fn inline_target_rows(&self) -> &[usize] {
        &self.inline_target_rows
    }

    pub(crate) fn aligned_target_rows(&self) -> &[usize] {
        &self.aligned_target_rows
    }

    pub(crate) fn target_line_for_inline_row(&self, row: usize) -> usize {
        if self.context_lines.is_some() {
            return self.inline_row_targets.get(row).copied().unwrap_or(0);
        }
        self.inline_target_rows.partition_point(|target_row| *target_row <= row).saturating_sub(1)
    }

    pub(crate) fn target_line_for_aligned_row(&self, row: usize) -> usize {
        if self.context_lines.is_some() {
            return self.aligned_row_targets.get(row).copied().unwrap_or(0);
        }
        self.aligned_target_rows
            .partition_point(|target_row| *target_row <= row)
            .saturating_sub(1)
    }

    fn index_selectable_rows(
        rows: &[AlignedDiffRow],
        inline: &[CachedInlineDiffRow],
    ) -> [Vec<usize>; 3] {
        [
            rows.iter()
                .enumerate()
                .filter_map(|(index, row)| {
                    (row.hidden_lines == 0 && row.left_line.is_some()).then_some(index)
                })
                .collect(),
            rows.iter()
                .enumerate()
                .filter_map(|(index, row)| {
                    (row.hidden_lines == 0 && row.right_line.is_some()).then_some(index)
                })
                .collect(),
            inline
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    (rows[entry.aligned_row].hidden_lines == 0).then_some(index)
                })
                .collect(),
        ]
    }

    pub(crate) fn selectable_rows(&self, aligned: bool, original: bool) -> &[usize] {
        &self.selectable_rows[if !aligned {
            2
        } else if original {
            0
        } else {
            1
        }]
    }

    fn index_overview_rows(
        rows: &[AlignedDiffRow],
        inline: &[CachedInlineDiffRow],
    ) -> [[Vec<usize>; 2]; 2] {
        let mut result: [[Vec<usize>; 2]; 2] = Default::default();
        for (index, row) in rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.hidden_lines == 0)
        {
            let kind = if row.right_kind == '+' {
                '+'
            } else {
                row.left_kind
            };
            if kind != ' ' {
                result[0][usize::from(kind == '-')].push(index);
            }
        }
        for (index, entry) in inline.iter().enumerate() {
            if entry.kind != ' ' && rows[entry.aligned_row].hidden_lines == 0 {
                result[1][usize::from(entry.kind == '-')].push(index);
            }
        }
        result
    }

    /// Query indexed changes without visiting unchanged diff display rows.
    pub(crate) fn overview_status_in_range(
        &self,
        aligned: bool,
        start: usize,
        end: usize,
    ) -> Option<char> {
        if start >= end {
            return None;
        }
        let rows = &self.overview_rows[usize::from(!aligned)];
        for (indices, status) in rows.iter().zip(['U', 'D']) {
            let first = indices.partition_point(|row| *row < start);
            if indices.get(first).is_some_and(|row| *row < end) {
                return Some(status);
            }
        }
        None
    }

    /// Project complete revisions into hunks without renumbering source lines.
    /// This runs only when the context setting or the underlying diff changes.
    pub(crate) fn with_context(&self, context: usize) -> Self {
        self.with_context_around(context, None)
    }

    pub(crate) fn with_context_around(&self, context: usize, reveal: Option<usize>) -> Self {
        let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
        for (index, row) in self.rows.iter().enumerate() {
            if row.left_kind != '-' && row.right_kind != '+' { continue; }
            let start = index.saturating_sub(context);
            let end = index.saturating_add(context).saturating_add(1).min(self.rows.len());
            if let Some(last) = ranges.last_mut().filter(|last| start <= last.end) {
                last.end = last.end.max(end);
            } else {
                ranges.push(start..end);
            }
        }
        if let Some(line) = reveal {
            let index = self.aligned_row_for_target_line(line);
            ranges.push(index.saturating_sub(context)..index.saturating_add(context).saturating_add(1).min(self.rows.len()));
            ranges.sort_unstable_by_key(|range| range.start);
            let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
            for range in ranges {
                if let Some(last) = merged.last_mut().filter(|last| range.start <= last.end) { last.end = last.end.max(range.end); }
                else { merged.push(range); }
            }
            ranges = merged;
        }
        let mut rows = Vec::new();
        let mut previous = 0;
        for range in &ranges {
            if range.start > previous {
                let count = range.start - previous;
                let text = format!("… {count} unchanged lines hidden …");
                let mut gap = AlignedDiffRow::new(' ', None, text.clone(), None, ' ', None, text, None);
                gap.hidden_lines = count;
                rows.push(gap);
            }
            rows.extend_from_slice(&self.rows[range.clone()]);
            previous = range.end;
        }
        if !ranges.is_empty() && previous < self.rows.len() {
            let count = self.rows.len() - previous;
            let text = format!("… {count} unchanged lines hidden …");
            let mut gap = AlignedDiffRow::new(' ', None, text.clone(), None, ' ', None, text, None);
            gap.hidden_lines = count;
            rows.push(gap);
        }
        let mut inline_rows = Vec::new();
        for (aligned_row, row) in rows.iter().enumerate() {
            if row.hidden_lines > 0 || row.left_kind == ' ' && row.right_kind == ' ' {
                inline_rows.push(CachedInlineDiffRow { aligned_row, kind: ' ' });
            } else {
                if row.left_line.is_some() { inline_rows.push(CachedInlineDiffRow { aligned_row, kind: '-' }); }
                if row.right_line.is_some() { inline_rows.push(CachedInlineDiffRow { aligned_row, kind: '+' }); }
            }
        }
        let last_target = self.aligned_target_rows.len().saturating_sub(1);
        let targets = |lines: Vec<Option<usize>>| {
            let mut next = last_target;
            let mut result = vec![0; lines.len()];
            for index in (0..lines.len()).rev() {
                next = lines[index].unwrap_or(next);
                result[index] = next;
            }
            result
        };
        let aligned_row_targets = targets(rows.iter().map(|row| row.right_line).collect());
        let inline_row_targets = targets(inline_rows.iter().map(|entry| {
            (entry.kind != '-').then_some(rows[entry.aligned_row].right_line).flatten()
        }).collect());
        let map_targets = |row_targets: &[usize]| {
            let mut row = 0;
            (0..self.aligned_target_rows.len()).map(|line| {
                while row + 1 < row_targets.len() && row_targets[row] < line { row += 1; }
                row
            }).collect()
        };
        let max_left_text_width = rows.iter().filter(|row| row.hidden_lines == 0)
            .map(|row| UnicodeWidthStr::width(row.left_text.as_str())).max().unwrap_or(0);
        let max_inline_text_width = rows.iter().filter(|row| row.hidden_lines == 0)
            .map(|row| UnicodeWidthStr::width(row.right_text.as_str())).max().unwrap_or(0).max(max_left_text_width);
        let selectable_rows = Self::index_selectable_rows(&rows, &inline_rows);
        let overview_rows = Self::index_overview_rows(&rows, &inline_rows);
        Self {
            syntax_first_lines: self.syntax_first_lines.clone(),
            context_lines: Some(context),
            visible_target_lines: rows.iter().filter_map(|row| row.right_line).collect(),
            aligned_target_rows: map_targets(&aligned_row_targets),
            inline_target_rows: map_targets(&inline_row_targets),
            aligned_row_targets,
            inline_row_targets,
            rows,
            inline_rows,
            hunks: self.hunks.clone(),
            target_changes: self.target_changes.clone(),
            max_left_line: self.max_left_line,
            max_left_text_width,
            max_inline_text_width,
            target_content_lines: self.target_content_lines,
            selectable_rows,
            overview_rows,
        }
    }

    pub(crate) fn next_hunk_target(&self, cursor_line: usize) -> Option<usize> {
        if let Some(current_hunk) = self
            .hunks
            .iter()
            .position(|hunk| hunk.target_start <= cursor_line && cursor_line <= hunk.target_end)
        {
            return self
                .hunks
                .get(current_hunk.saturating_add(1))
                .map(|hunk| hunk.target_start);
        }

        let next_hunk = self
            .hunks
            .partition_point(|hunk| hunk.target_start <= cursor_line);
        self.hunks.get(next_hunk).map(|hunk| hunk.target_start)
    }

    pub(crate) fn previous_hunk_target(&self, cursor_line: usize) -> Option<usize> {
        if let Some(current_hunk) = self
            .hunks
            .iter()
            .position(|hunk| hunk.target_start <= cursor_line && cursor_line <= hunk.target_end)
        {
            return current_hunk
                .checked_sub(1)
                .and_then(|idx| self.hunks.get(idx))
                .map(|hunk| hunk.target_start);
        }

        let previous_hunk = self
            .hunks
            .partition_point(|hunk| hunk.target_start < cursor_line)
            .checked_sub(1)?;
        self.hunks
            .get(previous_hunk)
            .map(|hunk| hunk.target_start)
    }
}

/// Align unified diff lines into side-by-side rows with spacer lines.
/// This keeps matching lines at the same visual height in both panes.
pub(crate) fn align_diff_rows(diff_lines: &[(char, String)]) -> Vec<AlignedDiffRow> {
    let mut rows: Vec<AlignedDiffRow> = Vec::new();
    let mut left_line = 0usize;
    let mut right_line = 0usize;
    let mut i = 0usize;

    while i < diff_lines.len() {
        let (kind, text) = &diff_lines[i];
        match *kind {
            ' ' => {
                rows.push(AlignedDiffRow::new(
                    ' ',
                    Some(left_line),
                    text.clone(),
                    None,
                    ' ',
                    Some(right_line),
                    text.clone(),
                    None,
                ));
                left_line += 1;
                right_line += 1;
                i += 1;
            }
            '-' => {
                let mut deletes: Vec<String> = Vec::new();
                while i < diff_lines.len() && diff_lines[i].0 == '-' {
                    deletes.push(diff_lines[i].1.clone());
                    i += 1;
                }

                let mut inserts: Vec<String> = Vec::new();
                while i < diff_lines.len() && diff_lines[i].0 == '+' {
                    inserts.push(diff_lines[i].1.clone());
                    i += 1;
                }

                let max_len = deletes.len().max(inserts.len());
                for idx in 0..max_len {
                    let (left_kind, left_idx, left_text) = if idx < deletes.len() {
                        ('-', Some(left_line + idx), deletes[idx].clone())
                    } else {
                        (' ', None, String::new())
                    };
                    let (right_kind, right_idx, right_text) = if idx < inserts.len() {
                        ('+', Some(right_line + idx), inserts[idx].clone())
                    } else {
                        (' ', None, String::new())
                    };
                    // Compute char-level diff for paired delete/insert lines
                    let (left_char_highlights, right_char_highlights) =
                        if idx < deletes.len() && idx < inserts.len() {
                            let (lh, rh) = compute_char_diff(&deletes[idx], &inserts[idx]);
                            (Some(lh), Some(rh))
                        } else {
                            (None, None)
                        };
                    rows.push(AlignedDiffRow::new(
                        left_kind,
                        left_idx,
                        left_text,
                        left_char_highlights,
                        right_kind,
                        right_idx,
                        right_text,
                        right_char_highlights,
                    ));
                }
                left_line += deletes.len();
                right_line += inserts.len();
            }
            '+' => {
                rows.push(AlignedDiffRow::new(
                    ' ',
                    None,
                    String::new(),
                    None,
                    '+',
                    Some(right_line),
                    text.clone(),
                    None,
                ));
                right_line += 1;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    rows
}

#[derive(Clone, Copy, PartialEq)]
pub enum DiffMode {
    Off,
    SideBySide,
    Inline,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GitFileChange<P = String> {
    pub path: P,
    pub original_path: Option<P>,
    pub status: char,   // 'M' modified, 'A' added, 'D' deleted, 'R' renamed/moved, '?' untracked
    pub staged: bool,
    /// A collapsed untracked directory; index changes remain individual entries.
    pub directory: bool,
}

impl<P> GitFileChange<P> {
    pub(crate) fn try_map_paths<Q, E>(self, mut convert: impl FnMut(P) -> Result<Q, E>) -> Result<GitFileChange<Q>, E> {
        Ok(GitFileChange { path: convert(self.path)?, original_path: self.original_path.map(convert).transpose()?, status: self.status, staged: self.staged, directory: self.directory })
    }
}
impl<P: AsRef<Path>> GitFileChange<P> {
    pub(crate) fn native_paths(&self) -> impl Iterator<Item = &Path> {
        std::iter::once(self.path.as_ref()).chain(self.original_path.as_ref().map(AsRef::as_ref))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceControlRepositoryState {
    Disabled,
    Checking,
    Loading,
    Available,
    Missing,
    Failed,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct GitCommitInfo {
    pub graph: String,     // topology prepared by the history worker
    pub hash: String,       // short hash
    pub full_hash: String,  // unambiguous identity, even in very large histories
    pub message: String,    // first line of commit message
    pub author: String,
    pub time_ago: String,   // e.g. "2 hours ago"
    pub branches: Vec<GitCommitReference>, // references pointing at this commit
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitCommitReference {
    pub name: String,
    pub remote: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum GitBranchPickerItem {
    Local {
        name: String,
    },
    Remote {
        remote_ref: String,
        local_name: String,
    },
}

impl GitBranchPickerItem {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Local { name } => format!("local: {name}"),
            Self::Remote { remote_ref, .. } => format!("remote: {remote_ref}"),
        }
    }

    pub(crate) fn target_branch_name(&self) -> &str {
        match self {
            Self::Local { name } => name,
            Self::Remote { local_name, .. } => local_name,
        }
    }

    pub(crate) fn remote_ref(&self) -> Option<&str> {
        match self {
            Self::Local { .. } => None,
            Self::Remote { remote_ref, .. } => Some(remote_ref),
        }
    }
}


#[derive(Clone)]
pub struct KeyBinding {
    pub id: &'static str,
    pub label: &'static str,
    pub modifiers: KeyModifiers,
    pub code: KeyCode,
    pub default_modifiers: KeyModifiers,
    pub default_code: KeyCode,
}

impl KeyBinding {
    pub(crate) fn display(&self) -> String {
        if self.code == KeyCode::Null {
            return "Unassigned".into();
        }
        let mut parts = Vec::new();
        if self.modifiers.contains(KeyModifiers::CONTROL) { parts.push("Ctrl"); }
        if self.modifiers.contains(KeyModifiers::ALT) { parts.push("Alt"); }
        if self.modifiers.contains(KeyModifiers::SHIFT) { parts.push("Shift"); }
        let key_name = match self.code {
            KeyCode::Char(c) => c.to_uppercase().to_string(),
            KeyCode::F(n) => format!("F{n}"),
            KeyCode::Enter => "Enter".into(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Tab => "Tab".into(),
            KeyCode::BackTab => "BackTab".into(),
            KeyCode::Backspace => "Backspace".into(),
            KeyCode::Delete => "Delete".into(),
            KeyCode::Up => "Up".into(),
            KeyCode::Down => "Down".into(),
            KeyCode::Left => "Left".into(),
            KeyCode::Right => "Right".into(),
            KeyCode::Home => "Home".into(),
            KeyCode::End => "End".into(),
            KeyCode::PageUp => "PageUp".into(),
            KeyCode::PageDown => "PageDown".into(),
            _ => "?".into(),
        };
        parts.push(&key_name);
        parts.join("+")
    }

    pub(crate) fn is_modified(&self) -> bool {
        self.modifiers != self.default_modifiers || self.code != self.default_code
    }

    pub(crate) fn reset(&mut self) {
        self.modifiers = self.default_modifiers;
        self.code = self.default_code;
    }
}

pub(crate) fn default_keybindings() -> Vec<KeyBinding> {
    let make = |id, label, modifiers: KeyModifiers, code: KeyCode| KeyBinding {
        id, label, modifiers, code,
        default_modifiers: modifiers,
        default_code: code,
    };
    vec![
        make("save", "Save File", KeyModifiers::CONTROL, KeyCode::Char('s')),
        make("find", "Find", KeyModifiers::CONTROL, KeyCode::Char('f')),
        make("find_replace", "Find & Replace", KeyModifiers::CONTROL, KeyCode::Char('h')),
        make("toggle_explorer", "Toggle Explorer", KeyModifiers::CONTROL, KeyCode::Char('b')),
        make("acp_chat", "Agent Chat", KeyModifiers::CONTROL | KeyModifiers::SHIFT, KeyCode::Char('i')),
        make(
            "toggle_terminal",
            "Toggle Right Terminal",
            KeyModifiers::CONTROL,
            KeyCode::Char('`'),
        ),
        make(
            "new_terminal",
            "New Right Terminal",
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            KeyCode::Char('`'),
        ),
        make("close_tab", "Close Tab", KeyModifiers::CONTROL, KeyCode::Char('w')),
        make("new_file", "New File", KeyModifiers::CONTROL, KeyCode::Char('n')),
        make("undo", "Undo", KeyModifiers::CONTROL, KeyCode::Char('z')),
        make("redo", "Redo", KeyModifiers::CONTROL, KeyCode::Char('y')),
        make("redo", "Redo", KeyModifiers::CONTROL | KeyModifiers::SHIFT, KeyCode::Char('Z')),
        make("copy", "Copy", KeyModifiers::CONTROL, KeyCode::Char('c')),
        make("cut", "Cut", KeyModifiers::CONTROL, KeyCode::Char('x')),
        make("paste", "Paste", KeyModifiers::CONTROL, KeyCode::Char('v')),
        make("select_all", "Select All", KeyModifiers::CONTROL, KeyCode::Char('a')),
        make("go_to_line", "Go to Line", KeyModifiers::CONTROL, KeyCode::Char('g')),
        make("navigate_back", "Go Back", KeyModifiers::ALT, KeyCode::Left),
        make("navigate_forward", "Go Forward", KeyModifiers::ALT, KeyCode::Right),
        make("duplicate_line", "Duplicate Line", KeyModifiers::CONTROL, KeyCode::Char('d')),
        make("palette", "Command Palette", KeyModifiers::empty(), KeyCode::F(1)),
        make("palette", "Command Palette", KeyModifiers::CONTROL | KeyModifiers::SHIFT, KeyCode::Char('P')),
        make("source_control", "Source Control", KeyModifiers::CONTROL | KeyModifiers::SHIFT, KeyCode::Char('G')),
        make("extensions", "Extensions", KeyModifiers::CONTROL | KeyModifiers::SHIFT, KeyCode::Char('X')),
        make("global_search", "Search in Files", KeyModifiers::CONTROL | KeyModifiers::SHIFT, KeyCode::Char('F')),
        make("delete_line", "Delete Line", KeyModifiers::CONTROL | KeyModifiers::SHIFT, KeyCode::Char('K')),
        make("toggle_comment", "Toggle Comment", KeyModifiers::CONTROL, KeyCode::Char('/')),
        make("cursor_undo", "Cursor Undo", KeyModifiers::CONTROL, KeyCode::Char('u')),
        make("clipboard_history", "Clipboard History", KeyModifiers::CONTROL | KeyModifiers::SHIFT, KeyCode::Char('V')),
        make("paste_plain", "Paste Without Formatting", KeyModifiers::CONTROL | KeyModifiers::ALT, KeyCode::Char('v')),
        make("toggle_word_wrap", "Toggle Word Wrap", KeyModifiers::ALT, KeyCode::Char('z')),
        make("join_lines", "Join Lines", KeyModifiers::CONTROL, KeyCode::Char('j')),
        make("select_line", "Select Line", KeyModifiers::CONTROL, KeyCode::Char('l')),
        make("rename_symbol", "Rename Symbol", KeyModifiers::empty(), KeyCode::F(2)),
        make("find_next", "Find Next", KeyModifiers::empty(), KeyCode::F(3)),
        make("find_prev", "Find Previous", KeyModifiers::SHIFT, KeyCode::F(3)),
        make("next_diagnostic", "Next Diagnostic", KeyModifiers::empty(), KeyCode::F(8)),
        make("prev_diagnostic", "Previous Diagnostic", KeyModifiers::SHIFT, KeyCode::F(8)),
        make("toggle_menu_bar", "Toggle Menu Bar", KeyModifiers::empty(), KeyCode::F(10)),
        make("go_to_definition", "Go to Definition", KeyModifiers::empty(), KeyCode::F(12)),
        make("go_to_implementation", "Go to Implementation", KeyModifiers::CONTROL, KeyCode::F(12)),
        make("find_references", "Find References", KeyModifiers::SHIFT, KeyCode::F(12)),
        make("format_document", "Format Document", KeyModifiers::SHIFT | KeyModifiers::ALT, KeyCode::Char('F')),
        make("toggle_problems", "Toggle Problems Panel", KeyModifiers::CONTROL | KeyModifiers::SHIFT, KeyCode::Char('M')),
        make("show_shortcuts", "Show Keyboard Shortcuts", KeyModifiers::CONTROL | KeyModifiers::SHIFT, KeyCode::Char('/')),
        make("next_tab", "Next Tab", KeyModifiers::CONTROL, KeyCode::Tab),
        make("prev_tab", "Previous Tab", KeyModifiers::CONTROL | KeyModifiers::SHIFT, KeyCode::Tab),
        make("next_tab_page", "Next Tab", KeyModifiers::CONTROL, KeyCode::PageDown),
        make("prev_tab_page", "Previous Tab", KeyModifiers::CONTROL, KeyCode::PageUp),
    ]
}

use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;

thread_local! {
    static FUZZY_MATCHER: SkimMatcherV2 = SkimMatcherV2::default();
}

/// Simple fuzzy match: all query chars must appear in order in the target
pub(crate) fn fuzzy_match(target: &str, query: &str) -> bool {
    // Membership needs only a subsequence scan. Building the scoring matrix
    // for every symbol wastes both CPU and scratch memory when no rank is used.
    let sensitive = query.bytes().any(|byte| byte.is_ascii_uppercase());
    if query.is_ascii() {
        let mut remaining = target.bytes();
        query.bytes().all(|expected| {
            remaining.any(|actual| {
                if sensitive { actual == expected } else { actual.eq_ignore_ascii_case(&expected) }
            })
        })
    } else {
        let mut remaining = target.chars();
        query.chars().all(|expected| {
            remaining.any(|actual| {
                if sensitive { actual == expected } else { actual.eq_ignore_ascii_case(&expected) }
            })
        })
    }
}

/// Fuzzy match with score — higher is better. Returns None if no match.
pub(crate) fn fuzzy_match_score(target: &str, query: &str) -> Option<i64> {
    FUZZY_MATCHER.with(|m| m.fuzzy_match(target, query))
}

/// Fuzzy match returning matched character indices (for highlighting)
pub(crate) fn fuzzy_match_indices(target: &str, query: &str) -> Option<(i64, Vec<usize>)> {
    FUZZY_MATCHER.with(|m| m.fuzzy_indices(target, query))
}

/// Case-insensitive substring containment without allocating a lowercase copy.
pub(crate) fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() { return true; }
    if needle.len() > haystack.len() { return false; }
    let hb = haystack.as_bytes();
    let nb = needle.as_bytes();
    'outer: for start in 0..=(hb.len() - nb.len()) {
        for (j, &b) in nb.iter().enumerate() {
            if !hb[start + j].eq_ignore_ascii_case(&b) {
                continue 'outer;
            }
        }
        return true;
    }
    false
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    Line,
    Block,
    Underline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoSaveMode {
    Off,
    AfterDelay,
    OnFocusChange,
    OnWindowChange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhitespaceMode {
    None,
    Boundary,
    Selection,
    Trailing,
    All,
}

impl WhitespaceMode {
    pub(crate) fn name(&self) -> &'static str {
        match self {
            WhitespaceMode::None => "None",
            WhitespaceMode::Boundary => "Boundary",
            WhitespaceMode::Selection => "Selection",
            WhitespaceMode::Trailing => "Trailing",
            WhitespaceMode::All => "All",
        }
    }
    pub(crate) fn next(&self) -> WhitespaceMode {
        match self {
            WhitespaceMode::None => WhitespaceMode::Boundary,
            WhitespaceMode::Boundary => WhitespaceMode::Selection,
            WhitespaceMode::Selection => WhitespaceMode::Trailing,
            WhitespaceMode::Trailing => WhitespaceMode::All,
            WhitespaceMode::All => WhitespaceMode::None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePane {
    Explorer,
    Editor,
    Chat,
    Panel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarView {
    Explorer,
    Search,
    SourceControl,
    Settings,
    Extensions,
    Outline,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TabDropZone {
    Left,
    Right,
    Top,
    Bottom,
}

/// Workbench surface used by a terminal when it is not attached to an editor tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalDock {
    Right,
}

/// Type of content shown in a tab
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TabKind {
    File,
    GitRevision,
    CommitDetails,
    Diff(u64),
    MarkdownPreview,
    Settings,
    KeyboardShortcuts,
    ExtensionView,
    ExtensionInfo,
    Terminal,
    Chat(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionViewTabRef {
    pub extension_id: String,
    pub view_id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionPaletteAction {
    Command {
        extension_id: String,
        command_id: String,
    },
    View {
        extension_id: String,
        view_id: String,
    },
}

impl ExtensionPaletteAction {
    pub(crate) fn recent_key(&self) -> String {
        match self {
            ExtensionPaletteAction::Command {
                extension_id,
                command_id,
            } => format!("ext_cmd:{extension_id}:{command_id}"),
            ExtensionPaletteAction::View {
                extension_id,
                view_id,
            } => format!("ext_view:{extension_id}:{view_id}"),
        }
    }
}

/// A single open file tab
pub struct Tab {
    pub editor: Editor,
    pub pinned: bool,
    pub preview: bool,
    pub kind: TabKind,
    pub extension_view: Option<ExtensionViewTabRef>,
    pub(crate) file_search: crate::file_search::State,
    pub terminal_idx: Option<usize>,
}

impl Tab {
    /// Only live clean views can consume an external file snapshot. Use this
    /// before scheduling I/O as well as when applying its asynchronous result.
    pub(crate) fn reloadable_file_path(&mut self) -> Option<&std::path::Path> {
        if self.editor.file_path.is_none()
            || matches!(self.kind, TabKind::Diff(_) | TabKind::GitRevision | TabKind::CommitDetails)
        { return None; }
        // Another pane can publish an edit before the next frame refreshes this
        // view. A cached clean flag must never authorize overwriting that edit.
        self.editor.refresh_document();
        if self.editor.modified || self.editor.bg_loading {
            None
        } else {
            self.editor.file_path.as_deref()
        }
    }

    /// Custom file editors participate in the same unsaved-change protection.
    pub(crate) fn has_unsaved_changes(&self) -> bool {
        matches!(self.kind, TabKind::File | TabKind::ExtensionView) && self.editor.modified
    }

    /// Create a new file tab with default settings.
    pub(crate) fn new_file(editor: Editor) -> Self {
        Self {
            editor,
            pinned: false,
            preview: false,
            kind: TabKind::File,
            extension_view: None,
            file_search: Default::default(),
            terminal_idx: None,
        }
    }
}

/// An editor group (pane) — contains its own set of tabs with independent state.
/// Replaces the old flat tabs/split system. Each group is like a VS Code "editor group".
pub struct EditorGroup {
    pub(crate) desk: u64,
    pub(crate) inactive_chrome_cache: Option<crate::ui_helpers::PaneChromeCache>,
    pub(crate) visible_highlight_key: Option<super::VisibleHighlightKey>,
    pub(crate) visible_highlight_cancel_token: Option<crate::background::CancellationToken>,
    pub(crate) provisional_highlight_key: Option<super::VisibleHighlightKey>,
    pub(crate) provisional_highlight_cancel_token: Option<crate::background::CancellationToken>,
    pub tabs: Vec<Tab>,
    pub tab_mru_order: Vec<usize>,
    pub active_tab: usize,
    pub tab_scroll_offset: usize,
    pub(crate) tab_strip_scroll: Option<crate::ui_chrome_tabs::StripScroll>,
    pub code_area: Option<ratatui::layout::Rect>,
    pub tab_bar_area: Option<ratatui::layout::Rect>,
    pub editor_area: Option<ratatui::layout::Rect>,
    pub minimap_area: Option<ratatui::layout::Rect>,
    pub gutter_area: Option<ratatui::layout::Rect>,
    pub gutter_width: u16,
    pub scrollbar_area: Option<ratatui::layout::Rect>,
    pub h_scrollbar_area: Option<ratatui::layout::Rect>,
    pub hovered_tab: Option<usize>,
    pub hovered_close_btn: bool,
    pub breadcrumb_area: Option<ratatui::layout::Rect>,
    pub breadcrumb_segments: Vec<(u16, u16, usize)>,
}

impl Default for EditorGroup {
    fn default() -> Self { Self::new() }
}

impl EditorGroup {
    pub(crate) fn new() -> Self {
        Self {
            desk: 0,
            inactive_chrome_cache: None,
            visible_highlight_key: None,
            visible_highlight_cancel_token: None,
            provisional_highlight_key: None,
            provisional_highlight_cancel_token: None,
            tabs: Vec::new(),
            tab_mru_order: Vec::new(),
            active_tab: 0,
            tab_scroll_offset: 0,
            tab_strip_scroll: None,
            code_area: None,
            tab_bar_area: None,
            editor_area: None,
            minimap_area: None,
            gutter_area: None,
            gutter_width: 4,
            scrollbar_area: None,
            h_scrollbar_area: None,
            hovered_tab: None,
            hovered_close_btn: false,
            breadcrumb_area: None,
            breadcrumb_segments: Vec::new(),
        }
    }
}

/// Direction for a split.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SplitDirection {
    Horizontal, // panes side by side (left | right)
    Vertical,   // panes stacked (top / bottom)
}

/// A tree node for nested split layouts.
/// Each leaf holds a group index into `App::groups`.
/// Each branch splits its area into children with given direction and ratios.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SplitNode {
    Leaf(usize), // group index
    Split {
        direction: SplitDirection,
        children: Vec<SplitNode>,
        ratios: Vec<f32>, // relative sizes, should sum to ~1.0
    },
}

impl SplitNode {
    /// Collect all leaf group indices.
    pub(crate) fn leaves(&self) -> Vec<usize> {
        let mut leaves = Vec::new();
        self.visit_leaves(&mut |group| leaves.push(group));
        leaves
    }

    /// Visit in display order without allocating a vector at every tree node.
    pub(crate) fn visit_leaves(&self, visit: &mut impl FnMut(usize)) {
        match self {
            SplitNode::Leaf(g) => visit(*g),
            SplitNode::Split { children, .. } => {
                for child in children { child.visit_leaves(visit); }
            }
        }
    }

    /// Remove a leaf and simplify the tree. Returns true if found and removed.
    pub(crate) fn remove_leaf(&mut self, group_id: usize) -> bool {
        match self {
            SplitNode::Leaf(_) => false,
            SplitNode::Split { children, ratios, .. } => {
                // Check if any direct child is the target leaf
                if let Some(idx) = children.iter().position(|c| matches!(c, SplitNode::Leaf(g) if *g == group_id)) {
                    children.remove(idx);
                    if idx < ratios.len() { ratios.remove(idx); }
                    // Normalize ratios
                    let sum: f32 = ratios.iter().sum();
                    if sum > 0.0 {
                        for r in ratios.iter_mut() { *r /= sum; }
                    }
                    return true;
                }
                // Recurse into children
                for c in children.iter_mut() {
                    if c.remove_leaf(group_id) {
                        return true;
                    }
                }
                false
            }
        }
    }

    /// Simplify the tree: unwrap single-child splits, collapse same-direction nesting.
    pub(crate) fn simplify(&mut self) {
        if let SplitNode::Split { direction, children, ratios } = self {
            // Recursively simplify children first
            for c in children.iter_mut() { c.simplify(); }

            // Single child → replace self with that child
            if children.len() == 1 {
                *self = children.remove(0);
                return;
            }

            // Flatten same-direction nested splits
            let dir = *direction;
            let mut new_children = Vec::new();
            let mut new_ratios = Vec::new();
            for (i, child) in children.drain(..).enumerate() {
                let r = ratios.get(i).copied().unwrap_or(1.0 / (i + 1) as f32);
                if let SplitNode::Split { direction: d, children: gc, ratios: gr, .. } = &child {
                    if *d == dir {
                        // Flatten: incorporate grandchildren with scaled ratios
                        for (j, gc_child) in gc.iter().enumerate() {
                            new_children.push(gc_child.clone());
                            let gr_ratio = gr.get(j).copied().unwrap_or(1.0 / gc.len() as f32);
                            new_ratios.push(r * gr_ratio);
                        }
                        continue;
                    }
                }
                new_children.push(child);
                new_ratios.push(r);
            }
            *children = new_children;
            *ratios = new_ratios;
        }
    }

    /// Insert a new leaf next to an existing group, splitting in the given direction.
    /// `before` places the new group on the left or top instead of right or bottom.
    pub(crate) fn split_leaf(
        &mut self,
        existing_group: usize,
        new_group: usize,
        direction: SplitDirection,
        before: bool,
    ) -> bool {
        match self {
            SplitNode::Leaf(g) if *g == existing_group => {
                // Replace this leaf with a split containing both
                *self = SplitNode::Split {
                    direction,
                    children: if before {
                        vec![SplitNode::Leaf(new_group), SplitNode::Leaf(existing_group)]
                    } else {
                        vec![SplitNode::Leaf(existing_group), SplitNode::Leaf(new_group)]
                    },
                    ratios: vec![0.5, 0.5],
                };
                true
            }
            SplitNode::Split { children, .. } => {
                for c in children.iter_mut() {
                    if c.split_leaf(existing_group, new_group, direction, before) {
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// Remap group indices after groups are compacted.
    pub(crate) fn remap_indices(&mut self, map: &rustc_hash::FxHashMap<usize, usize>) {
        match self {
            SplitNode::Leaf(g) => {
                if let Some(&new_idx) = map.get(g) { *g = new_idx; }
            }
            SplitNode::Split { children, .. } => {
                for c in children { c.remap_indices(map); }
            }
        }
    }

    /// Adjust the ratio between child[idx] and child[idx+1] at the given path.
    /// `delta` is the change in ratio (positive = grow child[idx], shrink child[idx+1]).
    pub(crate) fn adjust_ratio(&mut self, path: &[usize], child_idx: usize, delta: f32) {
        if path.is_empty() {
            if let SplitNode::Split { ratios, .. } = self {
                if child_idx + 1 < ratios.len() {
                    let min_ratio = 0.05;
                    let new_a = (ratios[child_idx] + delta).clamp(min_ratio, 1.0 - min_ratio);
                    let new_b = (ratios[child_idx + 1] - delta).clamp(min_ratio, 1.0 - min_ratio);
                    let old_sum = ratios[child_idx] + ratios[child_idx + 1];
                    let new_sum = new_a + new_b;
                    if new_sum > 0.0 {
                        ratios[child_idx] = new_a * old_sum / new_sum;
                        ratios[child_idx + 1] = new_b * old_sum / new_sum;
                    }
                }
            }
        } else if let SplitNode::Split { children, .. } = self {
            if let Some(child) = children.get_mut(path[0]) {
                child.adjust_ratio(&path[1..], child_idx, delta);
            }
        }
    }

    /// Reset ratios to equal for the split node at the given path.
    pub(crate) fn reset_ratios_at(&mut self, path: &[usize]) {
        if path.is_empty() {
            if let SplitNode::Split { ratios, .. } = self {
                let n = ratios.len();
                if n > 0 {
                    let equal = 1.0 / n as f32;
                    for r in ratios.iter_mut() {
                        *r = equal;
                    }
                }
            }
        } else if let SplitNode::Split { children, .. } = self {
            if let Some(child) = children.get_mut(path[0]) {
                child.reset_ratios_at(&path[1..]);
            }
        }
    }
}

#[derive(Clone)]
pub struct SymbolItem {
    pub name: String,
    pub kind: &'static str,
    pub line: usize,
    pub tab_index: Option<usize>,
    pub file_name: Option<String>,
    /// Full file path for workspace symbols (to open files not yet in tabs)
    pub file_path: Option<PathBuf>,
}

#[derive(Clone)]
pub struct GlobalSearchResult {
    pub file_path: PathBuf,
    pub line_num: usize,
    /// Matches share their preview; embedded line breaks are displayed as ↵.
    pub line_text: std::sync::Arc<str>,
    pub col_start: usize,
    /// Character offset from the start line, possibly extending into later lines.
    pub col_end: usize,
    pub context_before: Option<String>,
    pub context_after: Option<String>,
}

pub struct PeekDefinition {
    pub lines: Vec<String>,
    pub file_path: String,
    pub start_line: usize,
}

pub struct PaletteCommand {
    pub name: String,
    pub shortcut: String,
    pub id: std::borrow::Cow<'static, str>,
}

pub struct ContextMenuItem {
    pub label: String,
    pub shortcut: String,
    pub id: &'static str,
    pub separator_after: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub enum CallHierarchyMode {
    Incoming,
    Outgoing,
}

#[derive(Clone)]
pub struct CallHierarchyEntry {
    pub name: String,
    pub detail: String,
    pub file_path: PathBuf,
    pub line: usize,
    pub col: usize,
}

#[derive(Clone, Copy, PartialEq)]
pub enum TypeHierarchyMode {
    Supertypes,
    Subtypes,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct TaskDefinition {
    pub label: String,
    pub command: String,
    pub source: String, // "vscode", "cargo", "npm", "make", etc.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_scope_summary(text: &str) {
        let index = DiffTextIndex::new(text);
        let summary = index.scope_summary;
        let first = text
            .chars()
            .enumerate()
            .find(|&(_, ch)| !matches!(ch, ' ' | '\t') && !ch.is_control());
        assert_eq!(summary.first(), first, "first token: {text:?}");
        let branch = first
            .filter(|&(_, ch)| matches!(ch, '}' | ']' | ')' | '>'))
            .and_then(|(column, closing)| {
                text.chars()
                    .enumerate()
                    .skip(column + 1)
                    .find(|&(_, ch)| ch != closing && !matches!(ch, ' ' | '\t') && !ch.is_control())
                    .map(|(column, _)| (closing, column))
            });
        assert_eq!(summary.branch(), branch, "branch token: {text:?}");
        for delimiter in ['{', '}', '[', ']', '(', ')', '<', '>', 'x', '\0'] {
            let repeated = "{}[]()<>".contains(delimiter)
                && text.chars().filter(|&ch| ch == delimiter).take(2).count() > 1;
            assert_eq!(
                summary.repeated(delimiter),
                repeated,
                "delimiter {delimiter:?}: {text:?}"
            );
        }
        assert_eq!(summary, DiffScopeLineSummary::from_text(text));
    }

    #[test]
    fn diff_scope_summary_matches_scalar_reference_for_ascii_unicode_and_controls() {
        for text in [
            "",
            " \t\r\n\u{7}",
            "    if ready {",
            "{ { } } [ [ ] ] ( ( ) ) < < > >",
            "    }}  else {",
            "    } else if label == \"}\" {",
            "    } /* } comment */ else {",
            "if label == \"}\" {",
            " \t界e\u{301} 😀 { {",
            "\u{85}\t} \u{1b} else if label == \"}\" {",
            "\u{a0}} else {",
            "\u{301} } else {",
            "\t} \t\u{7}} \r\nelse {",
        ] {
            assert_scope_summary(text);
        }
        let ascii = DiffTextIndex::new("    } else if label == \"}\" {");
        assert!(ascii.byte_checkpoints.is_empty() && ascii.cell_checkpoints.is_empty());
        assert_eq!(ascii.scope_summary.first(), Some((4, '}')));
        assert_eq!(ascii.scope_summary.branch(), Some(('}', 6)));
        assert!(ascii.scope_summary.repeated('}'));
        assert!(!ascii.scope_summary.repeated('{'));
        assert!(std::mem::size_of::<DiffScopeLineSummary>() <= 24);
    }

    #[test]
    fn diff_scope_summary_keeps_scalar_columns_and_distinct_revision_sides() {
        let unicode = format!("    }} {} else {{", "界e\u{301}".repeat(1_024));
        assert_scope_summary(&unicode);
        let long_ascii = format!("    }} {}else {{", " ".repeat(128 * 1_024));
        let index = DiffTextIndex::new(&long_ascii);
        assert!(index.byte_checkpoints.is_empty() && index.cell_checkpoints.is_empty());
        assert_eq!(index.scope_summary.branch(), Some(('}', 128 * 1_024 + 6)));
        let row = AlignedDiffRow::new(
            '-',
            Some(0),
            "\t界 { {".into(),
            None,
            '+',
            Some(0),
            "    } else if s == \"}\" {".into(),
            None,
        );
        assert_eq!(row.left_scope_summary().first(), Some((1, '界')));
        assert!(row.left_scope_summary().repeated('{'));
        assert!(!row.left_scope_summary().repeated('}'));
        assert_eq!(row.right_scope_summary().branch(), Some(('}', 6)));
        assert!(row.right_scope_summary().repeated('}'));
        assert!(!row.right_scope_summary().repeated('{'));
    }

    #[test]
    fn compute_file_diff_equal() {
        let result = compute_file_diff("hello\nworld\n", "hello\nworld\n");
        assert!(result.iter().all(|(k, _)| *k == ' '));
    }

    #[test]
    fn compute_file_diff_added() {
        let result = compute_file_diff("hello\n", "hello\nworld\n");
        assert!(result.iter().any(|(k, _)| *k == '+'));
    }

    #[test]
    fn compute_file_diff_removed() {
        let result = compute_file_diff("hello\nworld\n", "hello\n");
        assert!(result.iter().any(|(k, _)| *k == '-'));
    }

    #[test]
    fn compute_file_diff_crlf_normalization() {
        let result = compute_file_diff("hello\r\nworld\r\n", "hello\nworld\n");
        // After normalization, these should be equal
        assert!(result.iter().all(|(k, _)| *k == ' '));
    }

    #[test]
    fn fuzzy_match_score_basic() {
        let score = fuzzy_match_score("hello_world", "hw");
        assert!(score.is_some());
    }

    #[test]
    fn fuzzy_match_score_no_match() {
        let score = fuzzy_match_score("hello", "xyz");
        assert!(score.is_none());
    }

    #[test]
    fn fuzzy_match_indices_basic() {
        let result = fuzzy_match_indices("hello_world", "hw");
        assert!(result.is_some());
        let (score, indices) = result.unwrap();
        assert!(score > 0);
        assert!(!indices.is_empty());
    }

    #[test]
    fn whitespace_mode_next_cycle() {
        let mode = WhitespaceMode::None;
        let mode = mode.next();
        assert_eq!(mode, WhitespaceMode::Boundary);
        let mode = mode.next();
        assert_eq!(mode, WhitespaceMode::Selection);
        let mode = mode.next();
        assert_eq!(mode, WhitespaceMode::Trailing);
        let mode = mode.next();
        assert_eq!(mode, WhitespaceMode::All);
        let mode = mode.next();
        assert_eq!(mode, WhitespaceMode::None);
    }

    #[test]
    fn split_node_leaves() {
        let node = SplitNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![SplitNode::Leaf(0), SplitNode::Leaf(1)],
            ratios: vec![0.5, 0.5],
        };
        assert_eq!(node.leaves(), vec![0, 1]);
    }

    #[test]
    fn split_node_simplify_single_child() {
        let mut node = SplitNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![SplitNode::Leaf(42)],
            ratios: vec![1.0],
        };
        node.simplify();
        match node {
            SplitNode::Leaf(g) => assert_eq!(g, 42),
            _ => panic!("expected leaf after simplify"),
        }
    }

    #[test]
    fn split_node_split_leaf() {
        let mut node = SplitNode::Leaf(0);
        assert!(node.split_leaf(0, 1, SplitDirection::Horizontal, false));
        assert_eq!(node.leaves(), vec![0, 1]);
    }

    #[test]
    fn split_node_adjust_ratio() {
        let mut node = SplitNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![SplitNode::Leaf(0), SplitNode::Leaf(1)],
            ratios: vec![0.5, 0.5],
        };
        node.adjust_ratio(&[], 0, 0.1);
        if let SplitNode::Split { ratios, .. } = &node {
            assert!(ratios[0] > 0.5);
            assert!(ratios[1] < 0.5);
        }
    }

    #[test]
    fn extension_palette_action_recent_key() {
        let action = ExtensionPaletteAction::Command {
            extension_id: "my_ext".to_string(),
            command_id: "do_thing".to_string(),
        };
        assert_eq!(action.recent_key(), "ext_cmd:my_ext:do_thing");
    }

    #[test]
    fn split_node_remove_leaf() {
        let mut node = SplitNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![SplitNode::Leaf(0), SplitNode::Leaf(1), SplitNode::Leaf(2)],
            ratios: vec![0.33, 0.34, 0.33],
        };
        assert!(node.remove_leaf(1));
        assert_eq!(node.leaves(), vec![0, 2]);
    }

    #[test]
    fn split_node_remove_leaf_not_found() {
        let mut node = SplitNode::Leaf(0);
        assert!(!node.remove_leaf(99));
    }

    #[test]
    fn split_node_remap_indices() {
        let mut node = SplitNode::Split {
            direction: SplitDirection::Horizontal,
            children: vec![SplitNode::Leaf(0), SplitNode::Leaf(2)],
            ratios: vec![0.5, 0.5],
        };
        let mut map = rustc_hash::FxHashMap::default();
        map.insert(2, 1);
        node.remap_indices(&map);
        assert_eq!(node.leaves(), vec![0, 1]);
    }

    #[test]
    fn whitespace_mode_name() {
        assert_eq!(WhitespaceMode::None.name(), "None");
        assert_eq!(WhitespaceMode::All.name(), "All");
        assert_eq!(WhitespaceMode::Boundary.name(), "Boundary");
        assert_eq!(WhitespaceMode::Selection.name(), "Selection");
    }

    #[test]
    fn align_diff_rows_equal() {
        let diff = vec![(' ', "hello".to_string()), (' ', "world".to_string())];
        let rows = align_diff_rows(&diff);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].left_kind, ' ');
        assert_eq!(rows[0].right_kind, ' ');
    }

    #[test]
    fn aligned_diff_rows_index_long_unicode_windows_and_hash_prefixes() {
        let text = format!(
            "{}TAIL",
            "λ".repeat(crate::highlight::MAX_SYNTAX_LINE_CHARS + 300)
        );
        let rows = align_diff_rows(&[(' ', text)]);
        let row = &rows[0];

        assert_eq!(
            row.left_char_len(),
            crate::highlight::MAX_SYNTAX_LINE_CHARS + 304
        );
        assert_eq!(
            row.left_text_window(crate::highlight::MAX_SYNTAX_LINE_CHARS + 300, 4),
            "TAIL"
        );
        assert_eq!(
            row.left_highlight_hash(),
            crate::highlight::Highlighter::line_hash(
                row.left_text_window(0, crate::highlight::MAX_SYNTAX_LINE_CHARS)
            )
        );
    }

    #[test]
    fn align_diff_rows_add_remove() {
        let diff = vec![('-', "old".to_string()), ('+', "new".to_string())];
        let rows = align_diff_rows(&diff);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].left_kind, '-');
        assert_eq!(rows[0].right_kind, '+');
    }

    #[test]
    fn align_diff_rows_addition_marks_only_right_side() {
        let diff = vec![('+', "new".to_string())];
        let rows = align_diff_rows(&diff);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].left_kind, ' ');
        assert_eq!(rows[0].left_line, None);
        assert_eq!(rows[0].right_kind, '+');
        assert_eq!(rows[0].right_line, Some(0));
    }

    #[test]
    fn align_diff_rows_deletion_marks_only_left_side() {
        let diff = vec![('-', "old".to_string())];
        let rows = align_diff_rows(&diff);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].left_kind, '-');
        assert_eq!(rows[0].left_line, Some(0));
        assert_eq!(rows[0].right_kind, ' ');
        assert_eq!(rows[0].right_line, None);
    }

    #[test]
    fn align_diff_rows_unpaired_replacement_lines_are_neutral_on_empty_side() {
        let extra_delete = vec![
            ('-', "old one".to_string()),
            ('-', "old two".to_string()),
            ('+', "new one".to_string()),
        ];
        let rows = align_diff_rows(&extra_delete);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].left_kind, '-');
        assert_eq!(rows[0].right_kind, '+');
        assert_eq!(rows[1].left_kind, '-');
        assert_eq!(rows[1].right_kind, ' ');
        assert_eq!(rows[1].right_line, None);

        let extra_insert = vec![
            ('-', "old one".to_string()),
            ('+', "new one".to_string()),
            ('+', "new two".to_string()),
        ];
        let rows = align_diff_rows(&extra_insert);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].left_kind, ' ');
        assert_eq!(rows[1].left_line, None);
        assert_eq!(rows[1].right_kind, '+');
        assert_eq!(rows[1].right_line, Some(1));
    }

    #[test]
    fn diff_render_cache_maps_target_lines_to_rows() {
        let diff = vec![(' ', "a".to_string()), (' ', "b".to_string())];
        let cache = DiffRenderCache::new(&diff);
        assert_eq!(cache.aligned_row_for_target_line(1), 1);
        assert_eq!(cache.inline_row_for_target_line(1), 1);
    }

    #[test]
    fn diff_render_cache_navigates_target_hunks() {
        let diff = vec![
            (' ', "zero".to_string()),
            ('-', "old one".to_string()),
            ('+', "new one".to_string()),
            ('+', "new two".to_string()),
            (' ', "three".to_string()),
            ('-', "deleted before four".to_string()),
            (' ', "four".to_string()),
        ];
        let cache = DiffRenderCache::new(&diff);

        assert_eq!(
            cache.hunks,
            vec![
                CachedDiffHunk {
                    target_start: 1,
                    target_end: 2,
                },
                CachedDiffHunk {
                    target_start: 4,
                    target_end: 4,
                },
            ]
        );
        assert_eq!(cache.target_changes, vec![(1, 'U'), (2, 'U'), (4, 'D')]);
        assert_eq!(cache.next_hunk_target(0), Some(1));
        assert_eq!(cache.next_hunk_target(1), Some(4));
        assert_eq!(cache.next_hunk_target(2), Some(4));
        assert_eq!(cache.previous_hunk_target(4), Some(1));
        assert_eq!(cache.previous_hunk_target(3), Some(1));
        assert_eq!(cache.previous_hunk_target(1), None);
    }

    #[test]
    fn compute_char_diff_basic() {
        let (old_hl, new_hl) = compute_char_diff("hello world", "hello rust!");
        // "hello " is shared (6 chars false), "world" vs "rust!" differ
        assert_eq!(old_hl.len(), 11);
        assert_eq!(new_hl.len(), 11);
        // First 6 chars ("hello ") should be unchanged
        assert!(old_hl[..6].iter().all(|&b| !b));
        assert!(new_hl[..6].iter().all(|&b| !b));
        // Remaining chars should have at least some highlights
        assert!(old_hl[6..].iter().any(|&b| b));
        assert!(new_hl[6..].iter().any(|&b| b));
    }

    #[test]
    fn compute_char_diff_identical() {
        let (old_hl, new_hl) = compute_char_diff("same", "same");
        assert!(old_hl.iter().all(|&b| !b));
        assert!(new_hl.iter().all(|&b| !b));
    }

    #[test]
    fn compute_char_diff_completely_different() {
        let (old_hl, new_hl) = compute_char_diff("abc", "xyz");
        assert!(old_hl.iter().all(|&b| b));
        assert!(new_hl.iter().all(|&b| b));
    }

    #[test]
    fn align_diff_rows_char_highlights_present() {
        let diff = vec![('-', "old line".to_string()), ('+', "new line".to_string())];
        let rows = align_diff_rows(&diff);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].left_char_highlights.is_some());
        assert!(rows[0].right_char_highlights.is_some());
    }

    #[test]
    fn align_diff_rows_no_char_highlights_for_context() {
        let diff = vec![(' ', "context".to_string())];
        let rows = align_diff_rows(&diff);
        assert!(rows[0].left_char_highlights.is_none());
        assert!(rows[0].right_char_highlights.is_none());
    }
}

// ── Terminal / Palette / Rename sub-types ──────────────────────────────

pub(crate) struct TerminalTab {
    pub name: String,
    pub dock: TerminalDock,
    /// The unopened slot retained when there are no terminal sessions.
    pub is_placeholder: bool,
    pub terminal_cwd: PathBuf,
    pub terminal_launch_error: Option<String>,
    pub pending_command: Option<String>,
    pub terminal_pty: Option<crate::terminal::TerminalEmulator>,
}

impl TerminalTab {
    pub(crate) fn new(name: String, cwd: PathBuf) -> Self {
        Self::new_in(name, cwd, TerminalDock::Right)
    }

    pub(crate) fn new_in(name: String, cwd: PathBuf, dock: TerminalDock) -> Self {
        Self {
            name,
            dock,
            is_placeholder: false,
            terminal_cwd: cwd,
            terminal_launch_error: None,
            pending_command: None,
            terminal_pty: None,
        }
    }

    pub(crate) fn placeholder(cwd: PathBuf, dock: TerminalDock) -> Self {
        let mut tab = Self::new_in("1: Terminal".into(), cwd, dock);
        tab.is_placeholder = true;
        tab
    }
}

/// Command palette state (Ctrl+Shift+P).
#[derive(Default)]
pub(crate) struct PaletteState {
    pub theme_preview_original: Option<String>,
    pub install_theme_mode: bool,
    pub install_extension_mode: bool,
    pub install_syntax_mode: bool,
    pub syntax_install_running: bool,
    pub syntax_install_generation: u64,
    pub extension_versions: Option<crate::app_extension_install::VersionPicker>,
    pub extension_install_request: u64,
    pub extension_installing: bool,
    pub theme_install_running: bool,
    pub theme_install_error: Option<String>,
    pub theme_vsix_url: Option<String>,
    pub active: bool,
    pub query: String,
    pub cursor: usize,
    pub selection_anchor: Option<usize>,
    pub commands: Vec<PaletteCommand>,
    pub filtered: Vec<usize>,
    pub selected: usize,
    pub scroll_offset: usize,
    pub recent: Vec<String>, // recently used command IDs
}

/// Rename / rename-symbol dialog state.
#[derive(Default)]
pub(crate) struct RenameState {
    // Rename dialog (file/folder)
    pub active: bool,
    pub query: String,
    pub cursor: usize, // byte cursor position in rename input
    pub selection_anchor: Option<usize>, // byte anchor for Shift+Arrow selection
    pub placeholder_active: bool, // true when renaming a temporary new file/folder placeholder
    pub target: Option<PathBuf>,
    // Rename symbol dialog
    pub symbol_active: bool,
    pub symbol_old: String,
    pub symbol_new: String,
    pub symbol_cursor: usize,
    pub symbol_selection_anchor: Option<usize>,
}
