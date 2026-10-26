use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::Span,
    widgets::Paragraph,
    Frame,
};
#[cfg(test)]
use ratatui::text::Line;
use std::fmt::Write as FmtWrite;

use crate::app::App;
use crate::app::WhitespaceMode;
use crate::ui_dialogs::get_cached_blame_line;

#[path = "ui_render_line_lru.rs"]
pub(crate) mod line_lru;
#[path = "ui_editor_widths.rs"]
mod viewport_widths;

/// Minimum digits shown in the line-number gutter.
pub(crate) const GUTTER_MIN_DIGITS: usize = 2;
/// Maximum gutter width (digits + padding) to prevent extreme widths.
pub(crate) const GUTTER_MAX_WIDTH: usize = 12;
/// Minimum thumb width (in cells) for horizontal scrollbars.
const HSCROLLBAR_MIN_THUMB: f64 = 2.0;

/// Count decimal digits without allocating a String.
pub(crate) fn digit_count(n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    let mut count = 0;
    let mut v = n;
    while v > 0 {
        count += 1;
        v /= 10;
    }
    count
}

/// Format only the numeric portion of an editor gutter row.
///
/// A hidden line-number column has width zero, but the surrounding diff and
/// fold columns still exist. Keeping the visibility check here prevents a
/// decimal value from overflowing into those two cells and being clipped to
/// its first digit.
pub(crate) fn format_gutter_line_number(
    buffer: &mut String,
    show_line_numbers: bool,
    relative_line_numbers: bool,
    line_idx: usize,
    cursor_line: usize,
    gutter_width: usize,
) {
    buffer.clear();
    if !show_line_numbers {
        return;
    }

    if relative_line_numbers && line_idx != cursor_line {
        let relative = line_idx.abs_diff(cursor_line);
        let _ = write!(buffer, "{relative:>gutter_width$}");
    } else {
        let _ = write!(buffer, "{:>width$}", line_idx + 1, width = gutter_width);
    }
}

/// Format a complete empty gutter row, including the diff and fold cells.
pub(crate) fn format_empty_gutter_row(
    buffer: &mut String,
    show_line_numbers: bool,
    gutter_width: usize,
) {
    buffer.clear();
    if show_line_numbers {
        buffer.push_str(&crate::helpers::spaces(gutter_width + 2));
    } else {
        buffer.push_str("  ");
    }
}

/// Compute a very dim background tint from a themed foreground color (~20% intensity).
pub(crate) fn bg_tint(c: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => Color::Rgb(
            (r as u16 / 5) as u8,
            (g as u16 / 5) as u8,
            (b as u16 / 5) as u8,
        ),
        _ => c,
    }
}

/// Convert an ExtensionDiagnostic to an LspDiagnostic for unified rendering.
pub(crate) fn extension_diag_to_lsp(
    d: &crate::extensions::ExtensionDiagnostic,
) -> crate::lsp::LspDiagnostic {
    crate::lsp::LspDiagnostic {
        line: d.line,
        col: d.col,
        end_line: d.end_line,
        end_col: d.end_col,
        message: d.message.clone(),
        severity: match d.severity {
            1 => lsp_types::DiagnosticSeverity::ERROR,
            2 => lsp_types::DiagnosticSeverity::WARNING,
            3 => lsp_types::DiagnosticSeverity::INFORMATION,
            _ => lsp_types::DiagnosticSeverity::HINT,
        },
    }
}

/// Merge LSP diagnostics with extension diagnostics for a file into a single owned Vec.
pub(crate) fn merged_diagnostics_from(
    lsp_manager: &crate::lsp::LspManager,
    extension_manager: &crate::extensions::ExtensionManager,
    file_path: Option<&std::path::Path>,
    rope: &ropey::Rope,
) -> Vec<crate::lsp::LspDiagnostic> {
    let mut result: Vec<crate::lsp::LspDiagnostic> = Vec::with_capacity(8);
    if let Some(fp) = file_path {
        if let Some(lsp_diags) = lsp_manager.get_diagnostics(fp) {
            result.extend(lsp_diags.iter().cloned().map(|mut diagnostic| {
                diagnostic.col = crate::lsp_positions::from_utf16(rope, diagnostic.line, diagnostic.col as u32);
                diagnostic.end_col = crate::lsp_positions::from_utf16(rope, diagnostic.end_line, diagnostic.end_col as u32);
                diagnostic
            }));
        }
        let ext_diags = extension_manager.extension_diagnostics_for_file(&fp.to_string_lossy());
        for d in &ext_diags {
            result.push(extension_diag_to_lsp(d));
        }
    }
    result.sort_unstable_by_key(|d| d.line);
    result
}

/// Pre-computed whitespace boundary info for a line (used in Boundary mode).
#[derive(Clone, Copy)]
pub(crate) struct WhitespaceBoundary {
    pub leading_end: usize,
    pub trailing_start: usize,
}

#[cfg(test)]
impl WhitespaceBoundary {
    pub(crate) fn compute(line_text: &str) -> Self {
        if line_text.is_ascii() {
            let bytes = line_text.as_bytes();
            let leading_end = bytes
                .iter()
                .take_while(|&&b| b == b' ' || b == b'\t')
                .count();
            let trailing_ws = bytes
                .iter()
                .rev()
                .take_while(|&&b| b == b' ' || b == b'\t')
                .count();
            let trailing_start = bytes.len().saturating_sub(trailing_ws);
            Self {
                leading_end,
                trailing_start,
            }
        } else {
            let leading_end = line_text.chars().take_while(|c| c.is_whitespace()).count();
            let line_len = line_text.chars().count();
            let trailing_ws = line_text
                .chars()
                .rev()
                .take_while(|c| c.is_whitespace())
                .count();
            let trailing_start = line_len.saturating_sub(trailing_ws);
            Self {
                leading_end,
                trailing_start,
            }
        }
    }
}

/// Determine whether to render a whitespace character as visible, based on mode.
/// `_col` is the column index, `_line_text` is the full line text, `in_selection` indicates
/// whether this position is within a selection.
pub(crate) fn should_show_whitespace(
    mode: WhitespaceMode,
    ch: char,
    _col: usize,
    _line_text: &str,
    in_selection: bool,
) -> bool {
    if ch != ' ' && ch != '\t' {
        return false;
    }
    match mode {
        WhitespaceMode::None => false,
        WhitespaceMode::All => true,
        WhitespaceMode::Selection => in_selection,
        WhitespaceMode::Trailing => false, // handled separately per-line using WhitespaceBoundary
        WhitespaceMode::Boundary => {
            // Should not be called in Boundary mode; use should_show_whitespace_boundary instead.
            // Fallback: show all whitespace.
            true
        }
    }
}

/// Shared by wrapped and unwrapped rows, including mixed code/comment lines.
pub(crate) struct LineWhitespace<'a> {
    mode: WhitespaceMode,
    comment_mode: Option<WhitespaceMode>,
    comments: OccurrenceCursor<'a>,
    boundary: Option<WhitespaceBoundary>,
    is_cursor_line: bool,
}

impl<'a> LineWhitespace<'a> {
    pub(crate) fn new(
        mode: WhitespaceMode,
        comment_mode: Option<WhitespaceMode>,
        comments: &'a [(usize, usize)],
        boundary: Option<WhitespaceBoundary>,
        start: usize,
        is_cursor_line: bool,
    ) -> Self {
        Self {
            mode, comment_mode, comments: OccurrenceCursor::new(comments, start),
            boundary, is_cursor_line,
        }
    }

    pub(crate) fn needs_boundary(mode: WhitespaceMode, comment_mode: Option<WhitespaceMode>) -> bool {
        matches!(mode, WhitespaceMode::Boundary | WhitespaceMode::Trailing)
            || matches!(comment_mode, Some(WhitespaceMode::Boundary | WhitespaceMode::Trailing))
    }

    pub(crate) fn shows(&mut self, ch: char, col: usize, adjacent_space: bool, selected: bool) -> bool {
        if !matches!(ch, ' ' | '\t') {
            return false;
        }
        let mode = if self.comment_mode.is_some() && self.comments.contains(col) {
            self.comment_mode.unwrap_or(self.mode)
        } else {
            self.mode
        };
        match mode {
            WhitespaceMode::Boundary => ch == '\t' || adjacent_space || self.boundary
                .is_some_and(|b| col < b.leading_end || col >= b.trailing_start),
            WhitespaceMode::Trailing => !self.is_cursor_line && self.boundary
                .is_some_and(|b| col >= b.trailing_start),
            _ => should_show_whitespace(mode, ch, col, "", selected),
        }
    }
}

pub(crate) fn gutter_diff_indicator(
    diff_status: char,
    bg: Color,
    hovered: bool,
    colors: &crate::app_theme::ThemeColors,
) -> (&'static str, Style) {
    let hover_lighten = |c: Color| -> Color {
        if hovered && !colors.ui.preserve_theme_colors {
            crate::ui_colors::lighten(c, 30)
        } else {
            c
        }
    };
    match crate::ui_colors::change_marker_color(diff_status, colors) {
        Some(base) => {
            let fg = hover_lighten(base);
            ("┃", Style::default().fg(fg).bg(bg))
        }
        None => (" ", Style::default().fg(bg).bg(bg)),
    }
}

/// Binary search for all matches on a given line in the sorted search_matches vec.
/// Returns (col_start, col_end, is_current) tuples.
pub(crate) fn matches_for_line(
    search_matches: &[(usize, usize, usize)],
    rope: &ropey::Rope,
    line: usize,
    search_current: usize,
    result: &mut Vec<(usize, usize, bool)>,
) {
    result.clear();
    if search_matches.is_empty() || line >= rope.len_lines() { return; }
    let line_start = rope.line_to_char(line);
    let line_text = rope.line(line);
    let mut line_len = line_text.len_chars();
    while line_len > 0 && matches!(line_text.char(line_len - 1), '\r' | '\n') { line_len -= 1; }
    let first = search_matches.partition_point(|&(l, _, _)| l < line);
    // Regex hits do not overlap, so only the preceding hit can continue from
    // an earlier line. Literal hits are confined to their starting line.
    let start = first.saturating_sub(1);
    for (idx, &(row, cs, ce)) in search_matches[start..].iter().enumerate() {
        if row > line { break; }
        let origin = if row == line { line_start } else { rope.line_to_char(row) };
        let begin = origin + cs;
        let end = origin + ce;
        if row < line && end <= line_start { continue; }
        let col = begin.saturating_sub(line_start).min(line_len);
        let end_col = end.saturating_sub(line_start).min(line_len);
        // A one-cell marker makes an empty match or a matched newline visible.
        result.push((col, end_col.max(col + 1), start + idx == search_current));
    }
}

/// A borrowed view over either worker spans or the current plain line. Plain
/// redraws do not need a Vec and a second owned copy of the line's text.
pub(crate) struct HighlightedLine<'a> {
    cached: Option<&'a [(syntect::highlighting::Style, String)]>,
    plain: &'a str,
    plain_style: syntect::highlighting::Style,
    char_len: usize,
    owned_text: Option<std::rc::Rc<String>>,
}

impl<'a> HighlightedLine<'a> {
    pub(crate) fn new(
        cached: Option<(&'a [(syntect::highlighting::Style, String)], usize)>,
        clean_line: &'a str,
        clean_char_len: usize,
        plain_style: syntect::highlighting::Style,
    ) -> Self {
        let valid = cached.filter(|&(_, length)| length == clean_char_len);
        Self {
            cached: valid.map(|(spans, _)| spans),
            plain: clean_line,
            char_len: clean_char_len,
            owned_text: None,
            // Preserve the established neutral fallback for malformed spans.
            plain_style: if cached.is_some() && valid.is_none() {
                PLAIN_STYLE
            } else {
                plain_style
            },
        }
    }

    pub(crate) fn with_text_snapshot(mut self, text: &RenderLineText<'_>) -> Self {
        self.owned_text = match text {
            RenderLineText::Borrowed(_) => None,
            RenderLineText::Shared(text) => Some(std::rc::Rc::clone(text)),
        };
        self
    }

    pub(crate) fn iter(
        &self,
    ) -> impl Iterator<Item = (syntect::highlighting::Style, &'a str)> + '_ {
        self.iter_from_span(0)
    }

    pub(crate) fn iter_from_span(
        &self,
        start: usize,
    ) -> impl Iterator<Item = (syntect::highlighting::Style, &'a str)> + '_ {
        self.cached
            .map(|spans| &spans[start.min(spans.len())..])
            .into_iter()
            .flatten()
            .map(|(style, text)| (*style, text.as_str()))
            .chain((self.cached.is_none() && start == 0).then_some((self.plain_style, self.plain)))
    }
}

pub(crate) enum RenderLineText<'a> {
    Borrowed(&'a str),
    Shared(std::rc::Rc<String>),
}

impl std::ops::Deref for RenderLineText<'_> {
    type Target = str;

    fn deref(&self) -> &str {
        match self {
            Self::Borrowed(text) => text,
            Self::Shared(text) => text.as_str(),
        }
    }
}

/// Decoded text belongs to an editor revision, while syntax colors can arrive
/// independently. Retain the exact span snapshot so replacing worker output
/// cannot reuse an allocation address and accidentally hit an older layout.
pub(crate) struct RenderLineGlyphs {
    source: Option<std::rc::Rc<crate::highlight::HighlightSpans>>,
    owned_text: Option<std::rc::Rc<String>>,
    pub(crate) chars: Vec<char>,
    pub(crate) line_chars: usize,
    /// Character and byte offsets coincide up to the first non-ASCII glyph.
    pub(crate) ascii_prefix: usize,
    // Only expanded tabs change the renderer's source-based tab accumulator.
    // Each entry stores (source column after tab, visual column after tab).
    tab_columns: Vec<(usize, usize)>,
    // Empty for unit-width text (including tabs). Wide/zero-width scalars get
    // an exact prefix map; ordinary lines need no integer per character.
    cell_columns: Vec<usize>,
    pub(crate) span_ends: Vec<usize>,
    pub(crate) source_spaces: Vec<bool>,
    pub(crate) whitespace: Option<WhitespaceBoundary>,
    pub(crate) leading_whitespace: usize,
    storage_bytes: usize,
}

impl RenderLineGlyphs {
    pub(crate) fn visual_column(&self, column: usize) -> usize {
        let before = self.tab_columns.partition_point(|&(source, _)| source <= column);
        column + before.checked_sub(1).map_or(0, |index| {
            let (source, visual) = self.tab_columns[index];
            visual - source
        })
    }

    pub(crate) fn cell_column(&self, column: usize) -> usize {
        if self.cell_columns.is_empty() { self.visual_column(column) } else { self.cell_columns[column] }
    }

    pub(crate) fn source_at_cell(&self, start: usize, end: usize, cell: usize) -> (usize, bool) {
        if self.cell_columns.is_empty() {
            if self.tab_columns.is_empty() {
                return (start + cell.min(end - start), cell < end - start);
            }
            let target = self.visual_column(start).saturating_add(cell);
            if target >= self.visual_column(end) { return (end, false); }
            let before = self.tab_columns.partition_point(|&(_, visual)| visual <= target);
            let excess = before.checked_sub(1).map_or(0, |index| {
                let (source, visual) = self.tab_columns[index];
                visual - source
            });
            let column = target - excess;
            // Until the next tab has ended, every cell in its expansion still
            // belongs to that tab's source character.
            return (self.tab_columns.get(before).map_or(column, |&(source, _)| column.min(source - 1)), true);
        }
        let columns = &self.cell_columns;
        let target = columns[start].saturating_add(cell);
        if target >= columns[end] { return (end, false); }
        // Equal offsets skip combining/zero-width scalars, matching the cold
        // Rope walker. A cell inside a tab or wide glyph selects that glyph.
        let offset = columns[start..=end].partition_point(|&column| column <= target).saturating_sub(1);
        (start + offset, true)
    }
}

#[cfg(test)]
#[path = "ui_editor_column_tests.rs"]
mod column_tests;

#[cfg(test)]
impl RenderLineGlyphs {
    pub(crate) fn owned_text_snapshot(&self) -> Option<std::rc::Rc<String>> {
        self.owned_text.as_ref().map(std::rc::Rc::clone)
    }
}

struct RenderLineDirty {
    from: usize,
    through: usize,
    version: u64,
    wrap_revision: u64,
    invalid: bool,
    line_count_changed: bool,
}

#[derive(Default)]
pub(crate) struct RenderLineCache {
    revision: Option<(u64, u64, usize)>,
    snapshot: Option<ropey::Rope>,
    entries: line_lru::LineLru<std::rc::Rc<RenderLineGlyphs>>,
    storage_bytes: usize,
    dirty: Option<RenderLineDirty>,
    decorations: rustc_hash::FxHashMap<usize, LineDecorations>,
    decoration_bytes: usize,
    occurrence_query: Option<(String, usize, bool)>,
    occurrence_generation: u64,
    visible_width: viewport_widths::VisibleWidths,
    #[cfg(test)]
    width_builds: usize,
    #[cfg(test)]
    pub(crate) reference_scan: bool,
    #[cfg(test)]
    pub(crate) builds: usize,
    #[cfg(test)]
    pub(crate) full_invalidation: bool,
    #[cfg(test)]
    pub(crate) reference_decorations: bool,
    #[cfg(test)]
    pub(crate) occurrence_builds: usize,
    #[cfg(test)]
    pub(crate) reference_query_invalidation: bool,
    #[cfg(test)]
    pub(crate) todo_builds: usize,
    #[cfg(test)]
    pub(crate) inline_color_builds: usize,
    #[cfg(test)]
    pub(crate) reference_brackets: bool,
    #[cfg(test)]
    pub(crate) bracket_builds: usize,
}

#[cfg(test)]
#[path = "ui_editor_frame_work_tests.rs"]
mod frame_work_tests;

/// Prepared once when bracket analysis arrives, shared by every subsequent
/// cursor/selection frame. Preserve provider order for overlapping guides.
#[derive(Default)]
pub(crate) struct BracketGuides {
    source: Vec<(usize, usize, usize, i32)>,
    columns: rustc_hash::FxHashMap<usize, Vec<(usize, usize, i32)>>,
}

impl From<Vec<(usize, usize, usize, i32)>> for BracketGuides {
    fn from(source: Vec<(usize, usize, usize, i32)>) -> Self {
        let mut columns: rustc_hash::FxHashMap<_, Vec<_>> = Default::default();
        for &(column, start, end, depth) in &source {
            columns.entry(column).or_default().push((start, end, depth));
        }
        Self { source, columns }
    }
}

impl BracketGuides {
    pub(crate) fn as_slice(&self) -> &[(usize, usize, usize, i32)] { &self.source }
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool { self.source.is_empty() }
    pub(crate) fn clear(&mut self) { self.source.clear(); self.columns.clear(); }
    pub(crate) fn at(&self, line: usize, column: usize) -> Option<&(usize, usize, i32)> {
        self.columns.get(&column)?.iter().find(|&&(start, end, _)| line >= start && line < end)
    }
}

#[derive(Default)]
struct LineDecorations {
    occurrences: Option<(u64, std::rc::Rc<LineOccurrences>)>,
    todos: Option<std::rc::Rc<TodoMarkers>>,
    inline_colors: Option<(bool, InlineColors)>,
    brackets: Option<std::rc::Rc<BracketEvents>>,
}

type InlineColors = std::rc::Rc<Vec<(usize, u8, u8, u8)>>;

/// Composition of saturating opens and zero-clamped closes. Keeping the
/// transform rather than an absolute depth lets unchanged lines reuse their
/// events when an earlier edit changes their starting bracket depth.
#[derive(Clone, Copy)]
struct BracketDepthTransform {
    shift: i16,
    floor: u16,
    ceiling_drop: u16,
}

impl BracketDepthTransform {
    const IDENTITY: Self = Self {
        shift: 0,
        floor: 0,
        ceiling_drop: 0,
    };

    fn apply(self, depth: i32) -> i32 {
        (i64::from(depth) + i64::from(self.shift)).clamp(
            i64::from(self.floor),
            i64::from(i32::MAX) - i64::from(self.ceiling_drop),
        ) as i32
    }

    fn open(&mut self) {
        self.shift += 1;
        self.floor += 1;
        self.ceiling_drop = self.ceiling_drop.saturating_sub(1);
    }

    fn close(&mut self) {
        self.shift -= 1;
        self.floor = self.floor.saturating_sub(1);
        self.ceiling_drop += 1;
    }
}

pub(crate) struct BracketEvents {
    // The renderer admits at most 5,000 characters here, so columns and
    // relative clamp offsets fit in 16 bits. Eight bytes per event keeps a
    // dense bracket viewport inside the shared metadata budget.
    events: Vec<(u16, BracketDepthTransform)>,
    end: BracketDepthTransform,
}

const _: () = assert!(crate::background::MAX_BRACKET_LINE_CHARS <= i16::MAX as usize);

impl BracketEvents {
    fn build(text: &str) -> Self {
        let mut end = BracketDepthTransform::IDENTITY;
        let mut events = Vec::new();
        for (column, ch) in text.chars().enumerate() {
            match ch {
                '(' | '[' | '{' => {
                    events.push((column as u16, end));
                    end.open();
                }
                ')' | ']' | '}' => {
                    end.close();
                    events.push((column as u16, end));
                }
                _ => {}
            }
        }
        if events.capacity() > events.len().saturating_add(events.len() / 4) {
            events.shrink_to_fit();
        }
        Self { events, end }
    }

    fn bytes(&self) -> usize {
        self.events.capacity() * std::mem::size_of::<(u16, BracketDepthTransform)>()
    }
}

pub(crate) struct BracketLineColors {
    events: std::rc::Rc<BracketEvents>,
    starting_depth: i32,
    #[cfg(test)]
    reference: Option<(rustc_hash::FxHashMap<usize, i32>, i32)>,
}

#[cfg(test)]
thread_local! {
    // Reuse the largest original-map allocation even across frames. The
    // original renderer cleared one map between logical lines but allocated
    // it anew each frame; this pool conservatively favors that reference.
    static REFERENCE_BRACKET_MAP_POOL: std::cell::RefCell<rustc_hash::FxHashMap<usize, i32>> = Default::default();
    static REFERENCE_BRACKET_EMPTY_EVENTS: std::rc::Rc<BracketEvents> = std::rc::Rc::new(BracketEvents {
        events: Vec::new(), end: BracketDepthTransform::IDENTITY,
    });
}

#[cfg(test)]
pub(crate) fn reset_reference_bracket_pool() {
    REFERENCE_BRACKET_MAP_POOL.with(|slot| {
        *slot.borrow_mut() = rustc_hash::FxHashMap::default();
    });
}

#[cfg(test)]
impl Drop for BracketLineColors {
    fn drop(&mut self) {
        if let Some((map, _)) = self.reference.take() {
            REFERENCE_BRACKET_MAP_POOL.with(|slot| {
                let mut retained = slot.borrow_mut();
                if map.capacity() > retained.capacity() {
                    *retained = map;
                }
            });
        }
    }
}

impl BracketLineColors {
    pub(crate) fn end_depth(&self) -> i32 {
        #[cfg(test)]
        if let Some((_, depth)) = &self.reference {
            return *depth;
        }
        self.events.end.apply(self.starting_depth)
    }

    pub(crate) fn cursor(&self, column: usize) -> BracketColorCursor<'_> {
        BracketColorCursor {
            line: self,
            next: self
                .events
                .events
                .partition_point(|&(col, _)| usize::from(col) < column),
        }
    }
}

pub(crate) struct BracketColorCursor<'a> {
    line: &'a BracketLineColors,
    next: usize,
}

#[cfg(test)]
thread_local! {
    pub(crate) static REFERENCE_EXTRA_CURSOR_PROJECTION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Default)]
pub(crate) struct ExtraCursorLine {
    pub(crate) columns: Vec<usize>,
    pub(crate) selections: Vec<(usize, usize)>,
    // Only a selection's final logical line can extend past EOL. Interior
    // full-line coverage must never paint virtual space.
    pub(crate) virtual_selections: Vec<(usize, usize)>,
}

#[derive(Default)]
pub(crate) struct ExtraCursorViewport {
    lines: Vec<(usize, ExtraCursorLine)>,
    direct: bool,
    #[cfg(test)]
    reference: bool,
}

impl ExtraCursorViewport {
    pub(crate) fn reference(&self) -> bool {
        #[cfg(test)]
        {
            return self.reference;
        }
        #[cfg(not(test))]
        {
            false
        }
    }

    pub(crate) fn new(
        editor: &crate::editor::Editor,
        visible: impl Iterator<Item = usize>,
    ) -> Self {
        #[cfg(test)]
        if REFERENCE_EXTRA_CURSOR_PROJECTION.with(std::cell::Cell::get) {
            return Self {
                reference: true,
                ..Default::default()
            };
        }
        if editor.extra_cursors.is_empty() {
            return Self::default();
        }
        // A handful of cursors is cheaper to project directly into reusable
        // line scratch than to allocate a viewport index. This scan stays
        // bounded independently of the document or total cursor count.
        if editor.extra_cursors.len() <= 4 {
            return Self {
                direct: true,
                ..Default::default()
            };
        }
        let mut visible: Vec<_> = visible.collect();
        visible.sort_unstable();
        visible.dedup();
        let mut lines: Vec<_> = visible
            .iter()
            .map(|&line| (line, ExtraCursorLine::default()))
            .collect();
        let mut full_cover = vec![0isize; visible.len() + 1];
        for (index, &(line, column)) in editor.extra_cursors.iter().enumerate() {
            if let Ok(row) = visible.binary_search(&line) {
                lines[row].1.columns.push(column);
            }
            let Some(&(anchor_line, anchor_column)) = editor
                .extra_cursor_anchors
                .get(index)
                .and_then(Option::as_ref)
            else {
                continue;
            };
            let (start, end) = if (anchor_line, anchor_column) <= (line, column) {
                ((anchor_line, anchor_column), (line, column))
            } else {
                ((line, column), (anchor_line, anchor_column))
            };
            if start.0 == end.0 {
                if let Ok(row) = visible.binary_search(&start.0) {
                    lines[row].1.selections.push((start.1, end.1));
                    lines[row].1.virtual_selections.push((start.1, end.1));
                }
            } else {
                if let Ok(row) = visible.binary_search(&start.0) {
                    lines[row].1.selections.push((start.1, usize::MAX));
                }
                if let Ok(row) = visible.binary_search(&end.0) {
                    lines[row].1.selections.push((0, end.1));
                    lines[row].1.virtual_selections.push((0, end.1));
                }
                let first = visible.partition_point(|&line| line <= start.0);
                let after = visible.partition_point(|&line| line < end.0);
                if first < after {
                    full_cover[first] += 1;
                    full_cover[after] -= 1;
                }
            }
        }
        let mut covering = 0isize;
        for (row, (_, line)) in lines.iter_mut().enumerate() {
            covering += full_cover[row];
            if covering > 0 {
                line.selections.clear();
                line.selections.push((0, usize::MAX));
            } else {
                merge_column_ranges(&mut line.selections);
            }
            merge_column_ranges(&mut line.virtual_selections);
            // Preserve duplicate EOL cursors in wrapped rendering, while a
            // monotonic cursor below handles duplicates in text without a set.
            line.columns.sort_unstable();
        }
        Self {
            lines,
            direct: false,
            #[cfg(test)]
            reference: false,
        }
    }

    pub(crate) fn line<'a>(
        &'a self,
        editor: &crate::editor::Editor,
        line: usize,
        scratch: &'a mut ExtraCursorLine,
    ) -> &'a ExtraCursorLine {
        if self.direct || self.reference() {
            // Original per-line scans, retained as an independent projection
            // oracle and conservative comparative timing reference.
            scratch.columns.clear();
            scratch.selections.clear();
            scratch.virtual_selections.clear();
            for (index, &(cursor_line, cursor_column)) in editor.extra_cursors.iter().enumerate() {
                if cursor_line == line {
                    scratch.columns.push(cursor_column);
                }
                let Some(&(anchor_line, anchor_column)) = editor
                    .extra_cursor_anchors
                    .get(index)
                    .and_then(Option::as_ref)
                else {
                    continue;
                };
                let (sl, sc, el, ec) =
                    if (anchor_line, anchor_column) <= (cursor_line, cursor_column) {
                        (anchor_line, anchor_column, cursor_line, cursor_column)
                    } else {
                        (cursor_line, cursor_column, anchor_line, anchor_column)
                    };
                if sl <= line && line <= el {
                    let start = if line == sl { sc } else { 0 };
                    let end = if line == el { ec } else { usize::MAX };
                    scratch.selections.push((start, end));
                    if line == el {
                        scratch.virtual_selections.push((start, end));
                    }
                }
            }
            scratch.columns.sort_unstable();
            merge_column_ranges(&mut scratch.selections);
            merge_column_ranges(&mut scratch.virtual_selections);
            return scratch;
        }
        self.lines
            .binary_search_by_key(&line, |&(index, _)| index)
            .map_or(&EMPTY_EXTRA_CURSOR_LINE, |index| &self.lines[index].1)
    }
}

static EMPTY_EXTRA_CURSOR_LINE: ExtraCursorLine = ExtraCursorLine {
    columns: Vec::new(),
    selections: Vec::new(),
    virtual_selections: Vec::new(),
};

pub(crate) struct CursorColumnCursor<'a> {
    columns: &'a [usize],
    next: usize,
}

impl<'a> CursorColumnCursor<'a> {
    pub(crate) fn new(columns: &'a [usize], start: usize) -> Self {
        Self {
            columns,
            next: columns.partition_point(|&column| column < start),
        }
    }

    pub(crate) fn contains(&mut self, column: usize) -> bool {
        while self
            .columns
            .get(self.next)
            .is_some_and(|&value| value < column)
        {
            self.next += 1;
        }
        self.columns.get(self.next) == Some(&column)
    }
}

impl BracketColorCursor<'_> {
    pub(crate) fn at(&mut self, column: usize) -> Option<i32> {
        #[cfg(test)]
        if let Some(reference) = &self.line.reference {
            return reference.0.get(&column).copied();
        }
        while self
            .line
            .events
            .events
            .get(self.next)
            .is_some_and(|&(col, _)| usize::from(col) < column)
        {
            self.next += 1;
        }
        self.line
            .events
            .events
            .get(self.next)
            .filter(|&&(col, _)| usize::from(col) == column)
            .map(|&(_, depth)| depth.apply(self.line.starting_depth))
    }
}

impl LineDecorations {
    fn bytes(&self) -> usize {
        self.occurrences.as_ref().map_or(0, |(_, value)| value.bytes())
            + self.todos.as_ref().map_or(0, |value| {
                value.0.capacity() * std::mem::size_of::<TodoMarker>()
            })
            + self.inline_colors.as_ref().map_or(0, |(_, value)| {
                value.capacity() * std::mem::size_of::<(usize, u8, u8, u8)>()
            })
            + self.brackets.as_ref().map_or(0, |value| value.bytes())
    }
}

impl RenderLineCache {
    const MAX_LINES: usize = 256;
    const MAX_BYTES: usize = 2 * 1024 * 1024;
    const MAX_DIRTY_LINES: usize = 64;

    /// Call after line() has synchronized this line with the current source.
    pub(crate) fn brackets(
        &mut self,
        line: usize,
        text: &str,
        starting_depth: i32,
    ) -> BracketLineColors {
        #[cfg(test)]
        if self.reference_brackets {
            let mut reference =
                REFERENCE_BRACKET_MAP_POOL.with(|slot| std::mem::take(&mut *slot.borrow_mut()));
            reference.clear();
            let mut depth = starting_depth;
            for (col, ch) in text.chars().enumerate() {
                match ch {
                    '(' | '[' | '{' => {
                        reference.insert(col, depth);
                        depth = depth.saturating_add(1);
                    }
                    ')' | ']' | '}' => {
                        depth = (depth - 1).max(0);
                        reference.insert(col, depth);
                    }
                    _ => {}
                }
            }
            return BracketLineColors {
                events: REFERENCE_BRACKET_EMPTY_EVENTS.with(std::rc::Rc::clone),
                starting_depth,
                reference: Some((reference, depth)),
            };
        }
        let events = if let Some(events) = self
            .decorations
            .get(&line)
            .and_then(|entry| entry.brackets.as_ref())
        {
            std::rc::Rc::clone(events)
        } else {
            #[cfg(test)]
            {
                self.bracket_builds += 1;
            }
            let events = std::rc::Rc::new(BracketEvents::build(text));
            let mut entry = self.remove_decorations(line);
            entry.brackets = Some(std::rc::Rc::clone(&events));
            self.store_decorations(line, entry);
            events
        };
        BracketLineColors {
            events,
            starting_depth,
            #[cfg(test)]
            reference: None,
        }
    }

    pub(crate) fn reference_decorations(&self) -> bool {
        #[cfg(test)]
        {
            return self.reference_decorations;
        }
        #[cfg(not(test))]
        {
            false
        }
    }

    fn remove_decorations(&mut self, line: usize) -> LineDecorations {
        if let Some(entry) = self.decorations.remove(&line) {
            self.decoration_bytes -= entry.bytes();
            entry
        } else {
            LineDecorations::default()
        }
    }

    fn evict_first_line(&mut self) {
        let (line, entry) = self.entries.pop_front().unwrap();
        self.storage_bytes -= entry.storage_bytes;
        self.remove_decorations(line);
    }

    fn store_decorations(&mut self, line: usize, entry: LineDecorations) {
        let bytes = entry.bytes();
        while !self.entries.is_empty()
            && self.storage_bytes + self.decoration_bytes + bytes > Self::MAX_BYTES
        {
            self.evict_first_line();
        }
        // Oversized glyph data remains uncached; do not retain decoration
        // data for lines absent from the same bounded text LRU.
        if self.entries.get(line).is_some() {
            self.decoration_bytes += bytes;
            self.decorations.insert(line, entry);
        }
    }

    pub(crate) fn prepare_occurrences(
        &mut self,
        revision: (u64, u64, usize),
        rope: &ropey::Rope,
        query: &str,
        query_chars: usize,
        whole_word: bool,
    ) {
        self.synchronize(revision, rope);
        // A selection or cursor word can come from a huge hidden line while
        // the viewport still uses rich rendering. It cannot match any rich
        // line; represent that state without retaining its raw text.
        let (query, query_chars, whole_word) =
            if query_chars > crate::highlight::MAX_SYNTAX_LINE_CHARS {
                ("", 0, false)
            } else {
                (query, query_chars, whole_word)
            };
        if self
            .occurrence_query
            .as_ref()
            .is_none_or(|(word, chars, whole)| {
                word != query || *chars != query_chars || *whole != whole_word
            })
        {
            // A caret crossing a word boundary must not walk/drop every
            // previously visited line. Stale occurrences stay byte-accounted
            // in the bounded LRU and are replaced only when that row is used.
            self.occurrence_generation = self.occurrence_generation.wrapping_add(1);
            #[cfg(test)]
            let reset = self.reference_query_invalidation || self.occurrence_generation == 0;
            #[cfg(not(test))]
            let reset = self.occurrence_generation == 0;
            if reset {
                for entry in self.decorations.values_mut() {
                    if let Some((_, value)) = entry.occurrences.take() {
                        self.decoration_bytes -= value.bytes();
                    }
                }
            }
            self.decoration_bytes -= self.occurrence_query.as_ref()
                .map_or(0, |(word, _, _)| word.capacity());
            let word = query.to_owned();
            self.decoration_bytes += word.capacity();
            self.occurrence_query = Some((word, query_chars, whole_word));
            while !self.entries.is_empty()
                && self.storage_bytes + self.decoration_bytes > Self::MAX_BYTES
            {
                self.evict_first_line();
            }
        }
    }

    // Call after line()/line_text() synchronize this source line with the
    // current rope. Syntax snapshot changes do not invalidate text metadata.
    pub(crate) fn occurrences(&mut self, line: usize, text: &str) -> std::rc::Rc<LineOccurrences> {
        let reference = self.reference_decorations();
        if !reference
            && let Some((_, value)) = self
                .decorations
                .get(&line)
                .and_then(|entry| entry.occurrences.as_ref())
                .filter(|(generation, _)| *generation == self.occurrence_generation)
        {
            return std::rc::Rc::clone(value);
        }
        #[cfg(test)]
        {
            self.occurrence_builds += 1;
        }
        let (query, query_chars, whole_word) = self
            .occurrence_query
            .as_ref()
            .expect("occurrence query must be prepared before drawing");
        let (query_chars, whole_word) = (*query_chars, *whole_word);
        #[cfg(test)]
        if reference {
            return std::rc::Rc::new(LineOccurrences::reference(
                text,
                query,
                query_chars,
                whole_word,
            ));
        }
        let result = std::rc::Rc::new(LineOccurrences::build(text, query, query_chars, whole_word));
        let mut entry = self.remove_decorations(line);
        entry.occurrences = Some((self.occurrence_generation, std::rc::Rc::clone(&result)));
        self.store_decorations(line, entry);
        result
    }

    pub(crate) fn todos(&mut self, line: usize, text: &str) -> std::rc::Rc<TodoMarkers> {
        let reference = self.reference_decorations();
        if !reference
            && let Some(value) = self
                .decorations
                .get(&line)
                .and_then(|entry| entry.todos.as_ref())
        {
            return std::rc::Rc::clone(value);
        }
        #[cfg(test)]
        {
            self.todo_builds += 1;
        }
        #[cfg(test)]
        if reference {
            return std::rc::Rc::new(TodoMarkers::reference(text));
        }
        let result = std::rc::Rc::new(TodoMarkers::build(text));
        let mut entry = self.remove_decorations(line);
        entry.todos = Some(std::rc::Rc::clone(&result));
        self.store_decorations(line, entry);
        result
    }

    /// Source colors are independent of theme, cursor and horizontal panning.
    /// Keep the parsed positions beside the same revision's bounded glyph LRU.
    pub(crate) fn inline_colors(
        &mut self,
        line: usize,
        text: &str,
        named: bool,
    ) -> std::rc::Rc<Vec<(usize, u8, u8, u8)>> {
        let reference = self.reference_decorations();
        if !reference
            && let Some((_, colors)) = self
                .decorations
                .get(&line)
                .and_then(|entry| entry.inline_colors.as_ref())
                .filter(|(cached_named, _)| *cached_named == named)
        {
            return std::rc::Rc::clone(colors);
        }
        #[cfg(test)]
        {
            self.inline_color_builds += 1;
        }
        let mut colors = crate::ui_colors::find_inline_colors(text, named);
        colors.sort_by_key(|&(column, _, _, _)| column);
        let result = std::rc::Rc::new(colors);
        if !reference {
            let mut entry = self.remove_decorations(line);
            entry.inline_colors = Some((named, std::rc::Rc::clone(&result)));
            self.store_decorations(line, entry);
        }
        result
    }

    #[cfg(test)]
    pub(crate) fn cached_line(&self, line: usize) -> Option<std::rc::Rc<RenderLineGlyphs>> {
        self.entries.get(line).map(std::rc::Rc::clone)
    }

    #[cfg(test)]
    pub(crate) fn decoration_storage(&self) -> (usize, usize, usize) {
        (
            self.entries.len(),
            self.storage_bytes + self.decoration_bytes,
            self.occurrence_query
                .as_ref()
                .map_or(0, |(word, _, _)| word.capacity()),
        )
    }

    #[cfg(test)]
    pub(crate) fn cached_occurrences(&self, line: usize) -> Option<std::rc::Rc<LineOccurrences>> {
        self.decorations
            .get(&line)
            .and_then(|entry| entry.occurrences.as_ref())
            .filter(|(generation, _)| *generation == self.occurrence_generation)
            .map(|(_, value)| std::rc::Rc::clone(value))
    }

    #[cfg(test)]
    pub(crate) fn cached_todos(&self, line: usize) -> Option<std::rc::Rc<TodoMarkers>> {
        self.decorations
            .get(&line)
            .and_then(|entry| entry.todos.as_ref())
            .map(std::rc::Rc::clone)
    }

    /// Marks may precede the rope mutation and version increment, or follow
    /// them. Preserve their complete range until the next render reconciles
    /// the final snapshot; missing revision steps remain a full invalidation.
    pub(crate) fn mark_dirty(
        &mut self,
        start: usize,
        end: usize,
        version: u64,
        wrap_revision: u64,
        rope: &ropey::Rope,
    ) {
        let Some((cached_version, cached_wrap, _)) = self.revision else {
            return;
        };
        let start = start.min(rope.len_lines().saturating_sub(1));
        let end = end.max(start).min(rope.len_lines().saturating_sub(1));
        let previous_version = self
            .dirty
            .as_ref()
            .map_or(cached_version, |dirty| dirty.version);
        let previous_wrap = self
            .dirty
            .as_ref()
            .map_or(cached_wrap, |dirty| dirty.wrap_revision);
        let invalid = !(version == previous_version || version == previous_version.wrapping_add(1))
            || wrap_revision != previous_wrap.wrapping_add(1);
        let line_count_changed = self.snapshot.as_ref()
            .is_none_or(|snapshot| snapshot.len_lines() != rope.len_lines());
        let dirty = self.dirty.get_or_insert(RenderLineDirty {
            from: start,
            through: end.max(start),
            version,
            wrap_revision,
            invalid: false,
            line_count_changed: false,
        });
        dirty.from = dirty.from.min(start);
        dirty.through = dirty.through.max(end.max(start));
        dirty.version = version;
        dirty.wrap_revision = wrap_revision;
        dirty.line_count_changed |= line_count_changed;
        dirty.invalid |=
            invalid || dirty.through.saturating_sub(dirty.from) >= Self::MAX_DIRTY_LINES;
    }

    pub(crate) fn viewport_start(&self, requested: usize) -> usize {
        #[cfg(test)]
        if self.reference_scan {
            return 0;
        }
        requested
    }

    /// Pointer hit testing uses the same bounded glyph snapshot as painting.
    pub(crate) fn pointer_glyphs(&self, line: usize, revision: (u64, u64, usize), rope: &ropey::Rope)
        -> Option<std::rc::Rc<RenderLineGlyphs>>
    {
        if self.revision != Some(revision) || self.dirty.is_some()
            || !self.snapshot.as_ref().is_some_and(|snapshot| snapshot.is_instance(rope))
        {
            return None;
        }
        self.entries.get(line).map(std::rc::Rc::clone)
    }

    fn synchronize(&mut self, revision: (u64, u64, usize), rope: &ropey::Rope) {
        if self.revision != Some(revision)
            || !self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.is_instance(rope))
        {
            let dirty = self.dirty.take().filter(|dirty| {
                !dirty.invalid
                    && self
                        .revision
                        .is_some_and(|(_, _, tab_size)| tab_size == revision.2)
                    && dirty.wrap_revision == revision.1
                    && (dirty.version == revision.0 || dirty.version.wrapping_add(1) == revision.0)
                    && !(dirty.from == 0 && dirty.through >= rope.len_lines().saturating_sub(1))
            });
            #[cfg(test)]
            let dirty = dirty.filter(|_| !self.full_invalidation);
            if let Some(dirty) = dirty {
                let from = dirty.from.saturating_sub(1);
                // Newlines can renumber every suffix entry, but the prefix
                // above the earliest edit is still valid. Track intermediate
                // line-count changes too (insert then delete before a draw).
                let renumbered = dirty.line_count_changed || self.snapshot.as_ref()
                    .is_none_or(|snapshot| snapshot.len_lines() != rope.len_lines());
                if renumbered {
                    self.visible_width.clear();
                    self.entries.retain(|(line, entry)| {
                        if *line < from { return true; }
                        self.storage_bytes -= entry.storage_bytes;
                        false
                    });
                    self.decorations.retain(|line, entry| {
                        if *line < from { return true; }
                        self.decoration_bytes -= entry.bytes();
                        false
                    });
                } else {
                    self.visible_width.invalidate(from, dirty.through.saturating_add(1));
                    // A local edit invalidates at most 66 adjacent rows. Use
                    // the index and update byte counts only for removed data;
                    // don't walk and recount the whole LRU for each keystroke.
                    for line in from..=dirty.through.saturating_add(1) {
                        if let Some(entry) = self.entries.remove(line) {
                            self.storage_bytes -= entry.storage_bytes;
                        }
                        self.remove_decorations(line);
                    }
                }
            } else {
                self.visible_width.clear();
                self.entries.clear();
                self.storage_bytes = 0;
                self.decorations.clear();
                self.decoration_bytes = 0;
                self.occurrence_query = None;
            }
            self.revision = Some(revision);
            self.snapshot = Some(rope.clone());
        }
    }

    /// Width metadata depends on the viewport and text, not the cursor or its
    /// selection. Warm navigation frames need no per-row Rope tree searches.
    pub(crate) fn visible_line_width(
        &mut self, revision: (u64, u64, usize), rope: &ropey::Rope,
        first: usize, end: usize, cursor: usize,
    ) -> usize {
        self.synchronize(revision, rope);
        let first = first.min(rope.len_lines());
        let end = end.max(first).min(rope.len_lines());
        let (width, _updated) = self.visible_width.get(rope, first, end);
        #[cfg(test)]
        { self.width_builds += usize::from(_updated); }
        if (first..end).contains(&cursor) { width }
        else { width.max(rope.get_line(cursor).map_or(0, viewport_widths::content_len)) }
    }

    pub(crate) fn line_text<'a>(
        &mut self,
        line_idx: usize,
        revision: (u64, u64, usize),
        rope: &ropey::Rope,
        line: ropey::RopeSlice<'a>,
    ) -> RenderLineText<'a> {
        self.synchronize(revision, rope);
        if let Some(text) = line.as_str() {
            return RenderLineText::Borrowed(text);
        }
        if self.viewport_start(1) != 0
            && let Some(text) = self
                .entries
                .get(line_idx)
                .and_then(|entry| entry.owned_text.as_ref())
        {
            return RenderLineText::Shared(std::rc::Rc::clone(text));
        }
        RenderLineText::Shared(std::rc::Rc::new(line.to_string()))
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn line(
        &mut self,
        line_idx: usize,
        revision: (u64, u64, usize),
        rope: &ropey::Rope,
        highlighted: &HighlightedLine<'_>,
        source: Option<(&std::rc::Rc<crate::highlight::HighlightSpans>, &[usize])>,
        requested_end: usize,
        whitespace_metadata: bool,
    ) -> std::rc::Rc<RenderLineGlyphs> {
        self.synchronize(revision, rope);
        let source = source.filter(|_| highlighted.cached.is_some());
        let mut requested_end = requested_end.min(highlighted.char_len);
        let reference = self.viewport_start(1) == 0;
        if !reference
            && let Some(entry) = self.entries.get(line_idx).filter(|entry| {
                entry.chars.len() >= requested_end
                    && (!whitespace_metadata || entry.whitespace.is_some())
                    && match (&entry.source, source) {
                        (Some(old), Some((new, _))) => std::rc::Rc::ptr_eq(old, new),
                        (None, None) => true,
                        _ => false,
                    }
            })
        {
            let result = std::rc::Rc::clone(entry);
            self.entries.touch(line_idx);
            return result;
        }
        // At most one text snapshot per source line survives syntax updates.
        if let Some(old) = self.entries.remove(line_idx) {
            // Horizontal movement grows a decoded prefix geometrically rather
            // than rebuilding a long prefix for every newly exposed column.
            let same_source = match (&old.source, source) {
                (Some(old), Some((new, _))) => std::rc::Rc::ptr_eq(old, new),
                (None, None) => true,
                _ => false,
            };
            if !reference && same_source && requested_end > old.chars.len() {
                requested_end = requested_end
                    .max(old.chars.len().saturating_mul(2))
                    .min(highlighted.char_len);
            }
            self.storage_bytes -= old.storage_bytes;
        }
        #[cfg(test)]
        {
            self.builds += 1;
        }
        let mut chars = Vec::with_capacity(requested_end);
        let mut ascii_prefix = 0;
        let mut tab_columns = Vec::new();
        let mut cell_columns = Vec::new();
        let mut span_ends = Vec::with_capacity(highlighted.cached.map_or(1, |spans| spans.len()));
        let mut visual_col = 0usize;
        let mut cell_col = 0usize;
        let tab_size = revision.2.max(1);
        let plain_lengths = [highlighted.char_len];
        for ((_, text), span_len) in highlighted.iter().zip(
            source
                .map_or(&plain_lengths[..], |(_, lengths)| lengths)
                .iter()
                .copied(),
        ) {
            let span_start = span_ends.last().copied().unwrap_or(0);
            span_ends.push(span_start + span_len);
            if chars.len() >= requested_end {
                continue;
            }
            for ch in text
                .trim_end_matches('\n')
                .chars()
                .take(requested_end - chars.len())
            {
                if ascii_prefix == chars.len() && ch.is_ascii() { ascii_prefix += 1; }
                let column = chars.len();
                chars.push(ch);
                let visual_width = if ch == '\t' {
                    tab_size - visual_col % tab_size
                } else {
                    1
                };
                let cell_width = if ch == '\t' { visual_width }
                    else { unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) };
                if cell_width != visual_width && cell_columns.is_empty() {
                    cell_columns.reserve(requested_end.saturating_add(1));
                    if tab_columns.is_empty() {
                        cell_columns.extend(0..column);
                    } else {
                        let mut tab = 0;
                        let mut excess = 0;
                        for source in 0..column {
                            if let Some(&(end, visual)) = tab_columns.get(tab)
                                && end == source
                            {
                                excess = visual - end;
                                tab += 1;
                            }
                            cell_columns.push(source + excess);
                        }
                    }
                }
                if !cell_columns.is_empty() || cell_width != visual_width {
                    cell_columns.push(cell_col);
                }
                visual_col += visual_width;
                cell_col += cell_width;
                if visual_width != 1 { tab_columns.push((column + 1, visual_col)); }
            }
        }
        if !cell_columns.is_empty() { cell_columns.push(cell_col); }
        let mut source_spaces = Vec::new();
        let mut leading_end = 0usize;
        let mut trailing_start = 0usize;
        if whitespace_metadata {
            source_spaces.reserve(requested_end.saturating_add(1));
            let ascii = highlighted.plain.is_ascii();
            source_spaces.extend(
                highlighted
                    .plain
                    .chars()
                    .take(requested_end.saturating_add(1))
                    .map(|ch| ch == ' '),
            );
            let whitespace = |ch: &char| {
                if ascii {
                    matches!(ch, ' ' | '\t')
                } else {
                    ch.is_whitespace()
                }
            };
            leading_end = highlighted.plain.chars().take_while(whitespace).count();
            trailing_start = highlighted.char_len
                - highlighted
                    .plain
                    .chars()
                    .rev()
                    .take_while(whitespace)
                    .count();
        }
        let storage_bytes = chars.capacity() * std::mem::size_of::<char>()
            + (tab_columns.capacity() * 2 + cell_columns.capacity() + span_ends.capacity()) * std::mem::size_of::<usize>()
            + source_spaces.capacity()
            + highlighted
                .owned_text
                .as_ref()
                .map_or(0, |text| text.capacity())
            + source.map_or(0, |(spans, _)| {
                spans.iter().map(|(_, text)| text.capacity()).sum::<usize>()
                    + spans.capacity()
                        * std::mem::size_of::<(syntect::highlighting::Style, String)>()
            });
        let entry = std::rc::Rc::new(RenderLineGlyphs {
            source: source.map(|(spans, _)| std::rc::Rc::clone(spans)),
            owned_text: highlighted.owned_text.as_ref().map(std::rc::Rc::clone),
            chars,
            line_chars: highlighted.char_len,
            ascii_prefix,
            tab_columns,
            cell_columns,
            span_ends,
            source_spaces,
            whitespace: whitespace_metadata.then_some(WhitespaceBoundary {
                leading_end,
                trailing_start,
            }),
            leading_whitespace: highlighted
                .plain
                .as_bytes()
                .iter()
                .take_while(|&&byte| matches!(byte, b' ' | b'\t'))
                .count(),
            storage_bytes,
        });
        if !reference && storage_bytes <= Self::MAX_BYTES {
            while self.entries.len() >= Self::MAX_LINES
                || self.storage_bytes + self.decoration_bytes + storage_bytes > Self::MAX_BYTES
            {
                if self.entries.is_empty() {
                    break;
                }
                self.evict_first_line();
            }
            if self.storage_bytes + self.decoration_bytes + storage_bytes <= Self::MAX_BYTES {
                self.storage_bytes += storage_bytes;
                self.entries
                    .push_back((line_idx, std::rc::Rc::clone(&entry)));
            } else {
                self.remove_decorations(line_idx);
            }
        } else {
            self.remove_decorations(line_idx);
        }
        entry
    }
}

pub(crate) const PLAIN_STYLE: syntect::highlighting::Style = syntect::highlighting::Style {
    foreground: syntect::highlighting::Color {
        r: 212,
        g: 212,
        b: 212,
        a: 255,
    },
    background: syntect::highlighting::Color {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    },
    font_style: syntect::highlighting::FontStyle::empty(),
};

/// Find non-overlapping occurrences while converting each UTF-8 prefix only
/// once. Counting from the start of the line for every hit is quadratic on
/// lines containing many Unicode occurrences.
fn visit_word_occurrences(
    text: &str,
    word: &str,
    word_char_len: usize,
    whole_word: bool,
    mut found: impl FnMut(usize, usize),
) {
    if word.is_empty() {
        return;
    }
    let ascii = text.is_ascii();
    let mut previous_byte = 0;
    let mut previous_col = 0;
    for (start, _) in text.match_indices(word) {
        let end = start + word.len();
        let col = if ascii {
            start
        } else {
            previous_col + text[previous_byte..start].chars().count()
        };
        previous_byte = end;
        previous_col = col + word_char_len;
        if !whole_word
            || ((start == 0 || !text[..start].ends_with(|c: char| c.is_alphanumeric() || c == '_'))
                && (end == text.len()
                    || !text[end..].starts_with(|c: char| c.is_alphanumeric() || c == '_')))
        {
            found(col, col + word_char_len);
        }
    }
}

#[cfg(test)]
pub(crate) fn word_occurrences_for_line(
    text: &str,
    word: &str,
    word_char_len: usize,
    whole_word: bool,
    output: &mut Vec<(usize, usize)>,
) {
    output.clear();
    visit_word_occurrences(text, word, word_char_len, whole_word, |start, end| {
        output.push((start, end))
    });
}

enum OccurrenceStarts {
    Compact(Vec<u16>),
    Wide(Vec<usize>),
    #[cfg(test)]
    Reference(Vec<(usize, usize)>),
}

pub(crate) struct LineOccurrences {
    starts: OccurrenceStarts,
    word_len: usize,
}

impl LineOccurrences {
    fn build(text: &str, query: &str, word_len: usize, whole_word: bool) -> Self {
        let mut starts = OccurrenceStarts::Compact(Vec::new());
        visit_word_occurrences(
            text,
            query,
            word_len,
            whole_word,
            |start, _| match &mut starts {
                OccurrenceStarts::Compact(values) => {
                    if let Ok(start) = u16::try_from(start) {
                        values.push(start);
                    } else {
                        let mut wide: Vec<_> =
                            values.iter().map(|&value| usize::from(value)).collect();
                        wide.push(start);
                        starts = OccurrenceStarts::Wide(wide);
                    }
                }
                OccurrenceStarts::Wide(values) => values.push(start),
                #[cfg(test)]
                OccurrenceStarts::Reference(_) => unreachable!(),
            },
        );
        Self { starts, word_len }
    }

    #[cfg(test)]
    fn reference(text: &str, query: &str, word_len: usize, whole_word: bool) -> Self {
        let mut ranges = Vec::new();
        word_occurrences_for_line(text, query, word_len, whole_word, &mut ranges);
        Self {
            starts: OccurrenceStarts::Reference(ranges),
            word_len,
        }
    }

    fn bytes(&self) -> usize {
        match &self.starts {
            OccurrenceStarts::Compact(values) => values.capacity() * std::mem::size_of::<u16>(),
            OccurrenceStarts::Wide(values) => values.capacity() * std::mem::size_of::<usize>(),
            #[cfg(test)]
            OccurrenceStarts::Reference(values) => {
                values.capacity() * std::mem::size_of::<(usize, usize)>()
            }
        }
    }
}

pub(crate) struct WordOccurrenceCursor<'a> {
    ranges: &'a LineOccurrences,
    next: usize,
}

impl<'a> WordOccurrenceCursor<'a> {
    pub(crate) fn new(ranges: &'a LineOccurrences, start: usize) -> Self {
        let word_len = ranges.word_len;
        let next = match &ranges.starts {
            OccurrenceStarts::Compact(values) => {
                values.partition_point(|&value| usize::from(value) + word_len <= start)
            }
            OccurrenceStarts::Wide(values) => {
                values.partition_point(|&value| value + word_len <= start)
            }
            #[cfg(test)]
            OccurrenceStarts::Reference(values) => values.partition_point(|&(_, end)| end <= start),
        };
        Self { ranges, next }
    }

    pub(crate) fn contains(&mut self, column: usize) -> bool {
        self.run(column).0
    }

    /// Coverage and its next boundary, so a whole styled run shares one lookup.
    pub(crate) fn run(&mut self, column: usize) -> (bool, usize) {
        loop {
            let range = match &self.ranges.starts {
                OccurrenceStarts::Compact(values) => values.get(self.next).map(|&value| {
                    let start = usize::from(value);
                    (start, start + self.ranges.word_len)
                }),
                OccurrenceStarts::Wide(values) => values
                    .get(self.next)
                    .map(|&value| (value, value + self.ranges.word_len)),
                #[cfg(test)]
                OccurrenceStarts::Reference(values) => values.get(self.next).copied(),
            };
            let Some((start, end)) = range else {
                return (false, usize::MAX);
            };
            if end > column {
                return (start <= column, if start > column { start } else { end });
            }
            self.next += 1;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TodoKind {
    Todo,
    Fixme,
    Hack,
    Note,
    Bug,
}

impl TodoKind {
    fn word(self) -> &'static str {
        match self {
            Self::Todo => "TODO",
            Self::Fixme => "FIXME",
            Self::Hack => "HACK",
            Self::Note => "NOTE",
            Self::Bug => "BUG",
        }
    }
    pub(crate) fn color(self, colors: &crate::app_theme::ThemeColors) -> Color {
        match self {
            Self::Todo => colors.fg_todo,
            Self::Fixme => colors.fg_fixme,
            Self::Hack => colors.fg_hack,
            Self::Note => colors.fg_info_label,
            Self::Bug => colors.fg_error,
        }
    }
}

pub(crate) struct TodoMarker {
    column: u32,
    pub(crate) byte: u32,
    pub(crate) kind: TodoKind,
}

pub(crate) struct TodoMarkers(pub(crate) Vec<TodoMarker>);

impl TodoMarkers {
    fn build(text: &str) -> Self {
        let mut output = Vec::new();
        let bytes = text.as_bytes();
        for (column, (byte, ch)) in text.char_indices().enumerate() {
            let kind = match ch {
                'T' => TodoKind::Todo,
                'F' => TodoKind::Fixme,
                'H' => TodoKind::Hack,
                'N' => TodoKind::Note,
                'B' => TodoKind::Bug,
                _ => continue,
            };
            let end = byte + kind.word().len();
            if bytes.get(byte..end) == Some(kind.word().as_bytes())
                && (byte == 0 || !bytes[byte - 1].is_ascii_alphanumeric())
                && (end == bytes.len() || !bytes[end].is_ascii_alphanumeric())
            {
                // Only <=16k-character rich lines request this metadata.
                output.push(TodoMarker {
                    column: column as u32,
                    byte: byte as u32,
                    kind,
                });
            }
        }
        Self(output)
    }

    #[cfg(test)]
    fn reference(text: &str) -> Self {
        let kinds = [
            TodoKind::Todo,
            TodoKind::Fixme,
            TodoKind::Hack,
            TodoKind::Note,
            TodoKind::Bug,
        ];
        let markers: Vec<_> = kinds
            .iter()
            .enumerate()
            .map(|(index, &kind)| (kind.word(), Color::Indexed(index as u8)))
            .collect();
        let mut ranges = Vec::new();
        todo_markers_for_line(text, &markers, &mut ranges);
        ranges.sort_by_key(|&(start, _, _)| start);
        let mut output: Vec<_> = ranges
            .into_iter()
            .map(|(column, _, color)| {
                let Color::Indexed(index) = color else {
                    unreachable!();
                };
                TodoMarker {
                    column: column as u32,
                    byte: 0,
                    kind: kinds[index as usize],
                }
            })
            .collect();
        if let Some(first) = output.first_mut() {
            first.byte = text
                .char_indices()
                .nth(first.column as usize)
                .map_or(text.len(), |(byte, _)| byte) as u32;
        }
        Self(output)
    }
}

pub(crate) struct TodoCursor<'a> {
    markers: &'a TodoMarkers,
    next: usize,
}

impl<'a> TodoCursor<'a> {
    pub(crate) fn new(markers: &'a TodoMarkers, start: usize) -> Self {
        Self {
            markers,
            next: markers.0.partition_point(|marker| {
                marker.column as usize + marker.kind.word().len() <= start
            }),
        }
    }
    pub(crate) fn at(&mut self, column: usize) -> Option<TodoKind> {
        self.run(column).0
    }

    pub(crate) fn run(&mut self, column: usize) -> (Option<TodoKind>, usize) {
        while self
            .markers
            .0
            .get(self.next)
            .is_some_and(|marker| marker.column as usize + marker.kind.word().len() <= column)
        {
            self.next += 1;
        }
        self.markers.0.get(self.next).map_or((None, usize::MAX), |marker| {
            let start = marker.column as usize;
            if start > column { (None, start) }
            else { (Some(marker.kind), start + marker.kind.word().len()) }
        })
    }
}

/// A rendered row visits columns in order. Start at its visible range and
/// advance through occurrence intervals instead of searching every interval
/// again for every character, especially near the end of a wrapped line.
pub(crate) struct OccurrenceCursor<'a> {
    ranges: &'a [(usize, usize)],
    next: usize,
}

/// Resolve overlapping ranges once, preserving the first provider's value.
/// The resulting disjoint runs support a monotone lookup while painting cells.
/// This replaces a scan of every diagnostic/search hit for every character.
pub(crate) fn flatten_priority_ranges<T: Copy + PartialEq>(
    ranges: &[(usize, usize, T)],
    output: &mut Vec<(usize, usize, T)>,
) {
    output.clear();
    if ranges.len() <= 1 {
        output.extend(ranges.iter().copied().filter(|&(start, end, _)| start < end));
        return;
    }
    // Most providers already publish disjoint, ordered intervals.
    if ranges.iter().all(|&(start, end, _)| start < end)
        && ranges.windows(2).all(|pair| pair[0].1 <= pair[1].0)
    {
        output.extend_from_slice(ranges);
        return;
    }
    let mut starts: Vec<_> = ranges.iter().enumerate()
        .filter(|(_, (start, end, _))| start < end)
        .map(|(index, &(start, _, _))| (start, index))
        .collect();
    starts.sort_unstable();
    let mut active = std::collections::BinaryHeap::new();
    let mut next = 0;
    let mut col = starts.first().map_or(0, |&(start, _)| start);
    while next < starts.len() || !active.is_empty() {
        while next < starts.len() && starts[next].0 <= col {
            active.push(std::cmp::Reverse(starts[next].1));
            next += 1;
        }
        while active.peek().is_some_and(|&std::cmp::Reverse(index)| ranges[index].1 <= col) {
            active.pop();
        }
        let Some(&std::cmp::Reverse(index)) = active.peek() else {
            if let Some(&(start, _)) = starts.get(next) { col = start; }
            continue;
        };
        let (_, end, value) = ranges[index];
        let next_col = starts.get(next).map_or(end, |&(start, _)| end.min(start));
        if let Some((_, previous_end, previous_value)) = output.last_mut()
            && *previous_end == col && *previous_value == value
        {
            *previous_end = next_col;
        } else {
            output.push((col, next_col, value));
        }
        col = next_col;
    }
}

/// Merge selection and symbol-highlight coverage before drawing. Unlike
/// provider ranges, these intervals all have the same visual meaning.
pub(crate) fn merge_column_ranges(ranges: &mut Vec<(usize, usize)>) {
    ranges.retain(|&(start, end)| start < end);
    ranges.sort_unstable();
    let mut count = 0;
    for read in 0..ranges.len() {
        let (start, end) = ranges[read];
        if count > 0 && ranges[count - 1].1 >= start {
            ranges[count - 1].1 = ranges[count - 1].1.max(end);
        } else {
            ranges[count] = (start, end);
            count += 1;
        }
    }
    ranges.truncate(count);
}

pub(crate) struct ValueRangeCursor<'a, T> {
    ranges: &'a [(usize, usize, T)],
    next: usize,
}

impl<'a, T: Copy> ValueRangeCursor<'a, T> {
    pub(crate) fn new(ranges: &'a [(usize, usize, T)], start: usize) -> Self {
        Self { ranges, next: ranges.partition_point(|&(_, end, _)| end <= start) }
    }

    pub(crate) fn at(&mut self, col: usize) -> Option<T> {
        self.run(col).0
    }

    pub(crate) fn run(&mut self, col: usize) -> (Option<T>, usize) {
        while self.ranges.get(self.next).is_some_and(|&(_, end, _)| end <= col) {
            self.next += 1;
        }
        self.ranges.get(self.next)
            .map_or((None, usize::MAX), |&(start, end, value)| {
                if start > col { (None, start) } else { (Some(value), end) }
            })
    }
}

/// Color previews are points rather than ranges. Detection puts named colors
/// after literal colors, so callers sort once before using this cursor.
pub(crate) struct InlineColorCursor<'a> {
    colors: &'a [(usize, u8, u8, u8)],
    next: usize,
}

impl<'a> InlineColorCursor<'a> {
    pub(crate) fn new(colors: &'a [(usize, u8, u8, u8)], start: usize) -> Self {
        Self { colors, next: colors.partition_point(|&(col, _, _, _)| col < start) }
    }

    pub(crate) fn at(&mut self, col: usize) -> Option<Color> {
        while self.colors.get(self.next).is_some_and(|&(start, _, _, _)| start < col) {
            self.next += 1;
        }
        let &(start, r, g, b) = self.colors.get(self.next)?;
        (start == col).then_some(Color::Rgb(r, g, b))
    }
}

impl<'a> OccurrenceCursor<'a> {
    pub(crate) fn new(ranges: &'a [(usize, usize)], start: usize) -> Self {
        Self {
            ranges,
            next: ranges.partition_point(|&(_, end)| end <= start),
        }
    }

    pub(crate) fn contains(&mut self, col: usize) -> bool {
        self.run(col).0
    }

    pub(crate) fn run(&mut self, col: usize) -> (bool, usize) {
        while self
            .ranges
            .get(self.next)
            .is_some_and(|&(_, end)| end <= col)
        {
            self.next += 1;
        }
        self.ranges
            .get(self.next)
            .map_or((false, usize::MAX), |&(start, end)| {
                (start <= col, if start > col { start } else { end })
            })
    }
}

#[cfg(test)]
pub(crate) fn todo_markers_for_line(
    text: &str,
    markers: &[(&str, Color)],
    output: &mut Vec<(usize, usize, Color)>,
) {
    output.clear();
    let bytes = text.as_bytes();
    let ascii = text.is_ascii();
    for &(marker, color) in markers {
        let mut previous_byte = 0;
        let mut previous_col = 0;
        for (start, _) in text.match_indices(marker) {
            let end = start + marker.len();
            let col = if ascii {
                start
            } else {
                previous_col + text[previous_byte..start].chars().count()
            };
            previous_byte = end;
            previous_col = col + marker.len();
            // Retain the existing ASCII boundary rule for comment markers.
            if (start == 0 || !bytes[start - 1].is_ascii_alphanumeric())
                && (end == bytes.len() || !bytes[end].is_ascii_alphanumeric())
            {
                output.push((col, col + marker.len(), color));
            }
        }
    }
}

#[path = "ui_editor_gradient_cache.rs"]
mod gradient_cache;

pub(crate) fn apply_editor_gradients(
    f: &mut Frame,
    code_area: Rect,
    editor_bg: Color,
    line_hl_bg: Color,
    editor_grad: crate::app_theme::ThemeGradientSpec,
    line_hl_grad: crate::app_theme::ThemeGradientSpec,
) {
    gradient_cache::draw(
        f,
        code_area,
        editor_bg,
        line_hl_bg,
        editor_grad,
        line_hl_grad,
    );
}

#[cfg(test)]
pub(crate) fn apply_editor_gradients_reference(
    f: &mut Frame,
    code_area: Rect,
    editor_bg: Color,
    line_hl_bg: Color,
    editor_grad: crate::app_theme::ThemeGradientSpec,
    line_hl_grad: crate::app_theme::ThemeGradientSpec,
) {
    if code_area.width == 0 || code_area.height == 0 {
        return;
    }
    if editor_grad.start == editor_grad.end && line_hl_grad.start == line_hl_grad.end {
        return;
    }

    let buf = f.buffer_mut();
    let has_editor = editor_grad.start != editor_grad.end;
    let has_linehl = line_hl_grad.start != line_hl_grad.end;
    let eg = crate::app_theme::GradientPrecomputed::new(
        code_area.width,
        code_area.height,
        editor_grad.angle_deg,
    );
    let lg = crate::app_theme::GradientPrecomputed::new(
        code_area.width,
        code_area.height,
        line_hl_grad.angle_deg,
    );
    for y in code_area.y..code_area.y + code_area.height {
        let ly = y - code_area.y;
        for x in code_area.x..code_area.x + code_area.width {
            if let Some(cell) = buf.cell_mut((x, y)) {
                let lx = x - code_area.x;
                if cell.bg == editor_bg && has_editor {
                    let t = eg.t_at(lx, ly);
                    cell.set_bg(crate::app_theme::lerp_color(
                        editor_grad.start,
                        editor_grad.end,
                        t,
                    ));
                } else if cell.bg == line_hl_bg && has_linehl {
                    let t = lg.t_at(lx, ly);
                    cell.set_bg(crate::app_theme::lerp_color(
                        line_hl_grad.start,
                        line_hl_grad.end,
                        t,
                    ));
                }
            }
        }
    }
}

pub(crate) struct HScrollbarParams<'a> {
    pub(crate) area: Rect,
    pub(crate) total_width: usize,
    pub(crate) visible_width: usize,
    pub(crate) scroll: usize,
    pub(crate) bg_editor: Color,
    pub(crate) hovered: bool,
    pub(crate) active: bool,
    pub(crate) colors: &'a crate::theme_workbench::WorkbenchColors,
}

pub(crate) fn draw_horizontal_scrollbar(f: &mut Frame, p: HScrollbarParams) {
    #[cfg(test)]
    if REFERENCE_EDITOR_CHROME.get() {
        draw_horizontal_scrollbar_reference(f, p);
        return;
    }
    let HScrollbarParams {
        area, total_width, visible_width, scroll, bg_editor, hovered, active, colors,
    } = p;
    let bar_width = area.width as usize;
    if bar_width == 0 || total_width <= visible_width {
        f.buffer_mut().set_style(area, Style::default().bg(bg_editor));
        return;
    }
    let thumb_size = (((visible_width as f64 / total_width as f64) * bar_width as f64)
        .max(HSCROLLBAR_MIN_THUMB) as usize).min(bar_width);
    let max_scroll = total_width.saturating_sub(visible_width);
    let thumb_pos = if max_scroll > 0 {
        ((scroll as f64 / max_scroll as f64) * (bar_width - thumb_size) as f64) as usize
    } else { 0 };
    let (track_color, thumb_color) = colors.scrollbar_colors(bg_editor, hovered, active);
    let track_style = Style::default().fg(track_color).bg(track_color);
    let thumb_style = Style::default().fg(thumb_color).bg(thumb_color);
    let buffer = f.buffer_mut();
    let clipped = area.intersection(buffer.area);
    // Paragraph first intersects its area with the buffer, then paints the
    // first text row there. Preserve that origin for offset pane buffers too.
    // Existing modifiers/underline colors are patched, not reset.
    if clipped.height == 0 { return; }
    for x in clipped.x..clipped.right() {
        let col = usize::from(x - clipped.x);
        let style = if col >= thumb_pos && col < thumb_pos + thumb_size {
            thumb_style
        } else { track_style };
        buffer[(x, clipped.y)].set_symbol("▁").set_style(style);
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static REFERENCE_EDITOR_CHROME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(crate) fn draw_horizontal_scrollbar_reference(f: &mut Frame, p: HScrollbarParams) {
    let HScrollbarParams {
        area,
        total_width,
        visible_width,
        scroll,
        bg_editor,
        hovered,
        active,
        colors,
    } = p;
    let bar_width = area.width as usize;
    if bar_width == 0 || total_width <= visible_width {
        let bg = Paragraph::new("").style(Style::default().bg(bg_editor));
        f.render_widget(bg, area);
        return;
    }

    let thumb_size = ((visible_width as f64 / total_width as f64) * bar_width as f64)
        .max(HSCROLLBAR_MIN_THUMB) as usize;
    let thumb_size = thumb_size.min(bar_width);
    let max_scroll = total_width.saturating_sub(visible_width);
    let thumb_pos = if max_scroll > 0 {
        ((scroll as f64 / max_scroll as f64) * (bar_width - thumb_size) as f64) as usize
    } else {
        0
    };

    let (track_color, thumb_color) = colors.scrollbar_colors(bg_editor, hovered, active);
    let mut spans: Vec<Span> = Vec::with_capacity(bar_width);
    for i in 0..bar_width {
        let in_thumb = i >= thumb_pos && i < thumb_pos + thumb_size;
        let style = if in_thumb {
            Style::default().fg(thumb_color).bg(thumb_color)
        } else {
            Style::default().fg(track_color).bg(track_color)
        };
        spans.push(Span::styled("▁", style));
    }
    let line = Line::from(spans);
    let para = Paragraph::new(line);
    f.render_widget(para, area);
}

/// Draw column ruler lines at configured positions.
pub(crate) fn draw_column_rulers(
    f: &mut Frame,
    code_area: Rect,
    rulers: &[u16],
    h_scroll: usize,
    foreground: crate::theme_workbench::ColorLayer,
) {
    for &ruler_col in rulers {
        let Some(screen_col) = usize::from(ruler_col).checked_sub(h_scroll) else { continue; };
        if screen_col < usize::from(code_area.width) {
            let x = code_area.x + screen_col as u16;
            let buf = f.buffer_mut();
            for y in code_area.y..code_area.y + code_area.height {
                let Some(cell) = buf.cell((x, y)) else { continue; };
                if cell.symbol() != " " { continue; }
                let Some(color) = foreground.border(cell.bg) else { continue; };
                // Blank continuation cells belong to a wide grapheme. Do not
                // put a ruler through them, or overwrite any existing text.
                let continuation = (code_area.x..x).rev()
                    .find(|&left| buf.cell((left, y)).is_some_and(|cell| cell.symbol() != " "))
                    .is_some_and(|left| buf.cell((left, y)).is_some_and(|cell|
                        unicode_width::UnicodeWidthStr::width(cell.symbol()) > usize::from(x - left)));
                if continuation { continue; }
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_char('│');
                    cell.set_fg(color);
                }
            }
        }
    }
}

/// Draw the minimap separator line.
pub(crate) fn draw_minimap_separator(f: &mut Frame, area: Rect, fg: Color, bg: Color) {
    if area.width == 0 {
        return;
    }
    let style = Style::default().fg(fg).bg(bg);
    #[cfg(test)]
    if REFERENCE_EDITOR_CHROME.get() {
        for y in area.y..area.y + area.height {
            f.render_widget(Paragraph::new("│").style(style), Rect::new(area.x, y, 1, 1));
        }
        return;
    }
    let buffer = f.buffer_mut();
    let clipped = Rect::new(area.x, area.y, 1, area.height).intersection(buffer.area);
    if clipped.width == 0 { return; }
    for y in clipped.y..clipped.bottom() {
        buffer[(area.x, y)].set_symbol("│").set_style(style);
    }
}

/// Draw the text drag-and-drop insertion indicator at the mouse position.
pub(crate) fn draw_text_drag_indicator(
    f: &mut Frame,
    code_area: Rect,
    mouse_row: u16,
    mouse_col: u16,
    fg_accent: Color,
) {
    if mouse_row >= code_area.y
        && mouse_row < code_area.y + code_area.height
        && mouse_col >= code_area.x
        && mouse_col < code_area.x + code_area.width
    {
        if let Some(cell) = f.buffer_mut().cell_mut((mouse_col, mouse_row)) {
            cell.set_fg(fg_accent);
            cell.set_symbol("▏");
        }
    }
}


/// Draw a git blame tooltip when hovering over the gutter.
pub(crate) fn draw_gutter_blame_tooltip(f: &mut Frame, app: &App, gutter_area: Rect) {
    if let Some(hover_line) = app.hovered_gutter_line {
        if let Some(editor) = app.editor() {
            if let Some(blame_text) = get_cached_blame_line(app, editor, hover_line) {
                let tooltip_text = format!(" {blame_text} ");
                let tw = unicode_width::UnicodeWidthStr::width(tooltip_text.as_str()) as u16;
                let tx = gutter_area.x + gutter_area.width;
                let ty = {
                    let rel = hover_line.saturating_sub(editor.scroll_offset);
                    gutter_area.y + (rel as u16).min(gutter_area.height.saturating_sub(1))
                };
                let tooltip_area = Rect::new(
                    tx.min(f.area().width.saturating_sub(tw)),
                    ty,
                    tw.min(f.area().width),
                    1,
                );
                let tooltip = Paragraph::new(Span::styled(
                    tooltip_text,
                    Style::default()
                        .fg(app.colors.fg_text)
                        .bg(app.colors.bg_dark),
                ));
                f.render_widget(tooltip, tooltip_area);
            }
        }
    }
}

#[path = "ui_marker_tooltip.rs"]
mod marker_tooltip;
pub(crate) use marker_tooltip::draw as draw_diagnostic_marker_tooltip;
pub(crate) use marker_tooltip::has_tooltip as has_diagnostic_marker_tooltip;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bracket_reference_reuses_capacity_without_per_line_event_allocation() {
        reset_reference_bracket_pool();
        let mut cache = RenderLineCache::default();
        cache.reference_brackets = true;
        let first = cache.brackets(0, &"({[]})".repeat(300), 0);
        let capacity = first.reference.as_ref().unwrap().0.capacity();
        let dummy = std::rc::Rc::clone(&first.events);
        drop(first);
        let second = cache.brackets(1, "()", 0);
        assert_eq!(second.reference.as_ref().unwrap().0.capacity(), capacity);
        assert!(std::rc::Rc::ptr_eq(&dummy, &second.events));
        drop(second);
        reset_reference_bracket_pool();
    }

    fn cached_plain_line(
        cache: &mut RenderLineCache,
        rope: &ropey::Rope,
        line: usize,
        revision: (u64, u64, usize),
    ) -> std::rc::Rc<RenderLineGlyphs> {
        let text = rope.line(line).to_string();
        let clean = text.trim_end_matches(['\r', '\n']);
        let view = HighlightedLine::new(None, clean, clean.chars().count(), PLAIN_STYLE);
        cache.line(line, revision, rope, &view, None, usize::MAX, false)
    }

    #[test]
    fn glyph_dirty_ranges_bridge_before_and_after_edit_marks_without_hiding_unknown_changes() {
        let mut rope = ropey::Rope::from_str(&"let value = ä界;\n".repeat(100));
        let mut cache = RenderLineCache::default();
        let distant = cached_plain_line(&mut cache, &rope, 90, (0, 0, 4));
        cache.mark_dirty(3, 3, 0, 1, &rope); // save_undo marks before mutation/version.
        rope.insert_char(rope.line_to_char(3) + 2, 'x');
        assert!(std::rc::Rc::ptr_eq(
            &distant,
            &cached_plain_line(&mut cache, &rope, 90, (1, 1, 4))
        ));
        rope.insert_char(rope.line_to_char(5) + 2, 'y');
        cache.mark_dirty(5, 5, 2, 2, &rope); // after mutation/version is also supported.
        assert!(std::rc::Rc::ptr_eq(
            &distant,
            &cached_plain_line(&mut cache, &rope, 90, (2, 2, 4))
        ));
        cache.mark_dirty(6, 6, 2, 3, &rope);
        rope.insert_char(rope.line_to_char(6) + 2, 'z');
        cache.mark_dirty(7, 7, 3, 4, &rope);
        rope.insert_char(rope.line_to_char(7) + 2, 'z');
        assert!(std::rc::Rc::ptr_eq(
            &distant,
            &cached_plain_line(&mut cache, &rope, 90, (4, 4, 4))
        ));
        let version_only = cached_plain_line(&mut cache, &rope, 90, (5, 4, 4));
        assert!(
            !std::rc::Rc::ptr_eq(&distant, &version_only),
            "unmarked version change must clear"
        );
        cache.mark_dirty(3, 3, 5, 5, &rope);
        let before_mutation = cached_plain_line(&mut cache, &rope, 90, (6, 5, 4));
        assert!(std::rc::Rc::ptr_eq(&version_only, &before_mutation));
        rope.insert_char(rope.line_to_char(3) + 2, 'a');
        let after_consumed_mark = cached_plain_line(&mut cache, &rope, 90, (6, 5, 4));
        assert!(
            !std::rc::Rc::ptr_eq(&before_mutation, &after_consumed_mark),
            "a consumed before-edit mark cannot authorize a later root change"
        );
        cache.mark_dirty(3, 3, 6, 6, &rope);
        rope.insert_char(rope.line_to_char(3) + 2, '\n');
        let structural = cached_plain_line(&mut cache, &rope, 90, (7, 6, 4));
        assert!(!std::rc::Rc::ptr_eq(&after_consumed_mark, &structural));
        cache.mark_dirty(0, 80, 7, 7, &rope);
        let wide = cached_plain_line(&mut cache, &rope, 90, (8, 7, 4));
        assert!(
            !std::rc::Rc::ptr_eq(&structural, &wide),
            "wide range must conservatively clear distant entries"
        );
        cache.mark_dirty(3, 3, 8, 8, &rope);
        let missing_mark = cached_plain_line(&mut cache, &rope, 90, (10, 8, 4));
        assert!(
            !std::rc::Rc::ptr_eq(&wide, &missing_mark),
            "unmarked version steps must clear"
        );
    }

    #[test]
    fn glyph_dirty_ranges_evict_neighbors_and_full_document_marks() {
        let mut rope = ropey::Rope::from_str(&"alpha ä界\n".repeat(20));
        let mut cache = RenderLineCache::default();
        let before = (0..20)
            .map(|line| cached_plain_line(&mut cache, &rope, line, (0, 0, 4)))
            .collect::<Vec<_>>();
        cache.mark_dirty(5, 5, 0, 1, &rope);
        rope.insert_char(rope.line_to_char(5), 'x');
        for (line, old) in before.iter().enumerate() {
            let after = cached_plain_line(&mut cache, &rope, line, (1, 1, 4));
            assert_eq!(
                std::rc::Rc::ptr_eq(old, &after),
                !(4..=6).contains(&line),
                "line={line}"
            );
        }
        let distant = cached_plain_line(&mut cache, &rope, 19, (1, 1, 4));
        cache.mark_dirty(0, rope.len_lines(), 1, 2, &rope);
        assert!(!std::rc::Rc::ptr_eq(
            &distant,
            &cached_plain_line(&mut cache, &rope, 19, (2, 2, 4))
        ));
    }

    #[test]
    fn render_line_cache_reuses_snapshots_and_invalidates_text_tabs_and_worker_spans() {
        use std::rc::Rc;
        let text = " \t界e\u{301}\t😀  ";
        let rope = ropey::Rope::from_str(text);
        let spans = Rc::new(vec![
            (PLAIN_STYLE, " \t界".into()),
            (PLAIN_STYLE, "e\u{301}\t😀  \n".into()),
        ]);
        let view = HighlightedLine::new(
            Some((&spans, text.chars().count())),
            text,
            text.chars().count(),
            PLAIN_STYLE,
        );
        let mut cache = RenderLineCache::default();
        let first = cache.line(
            0,
            (0, 0, 4),
            &rope,
            &view,
            Some((&spans, &[3, 6])),
            usize::MAX,
            true,
        );
        assert_eq!(first.chars, text.chars().collect::<Vec<_>>());
        assert_eq!((0..=first.chars.len()).map(|column| first.visual_column(column)).collect::<Vec<_>>(), [0, 1, 4, 5, 6, 7, 8, 9, 10, 11]);
        assert_eq!(first.span_ends, [3, 9]);
        assert!(Rc::ptr_eq(
            &first,
            &cache.line(
                0,
                (0, 0, 4),
                &rope,
                &view,
                Some((&spans, &[3, 6])),
                usize::MAX,
                true
            )
        ));
        assert_eq!(cache.builds, 1);
        let tabs = cache.line(
            0,
            (0, 0, 7),
            &rope,
            &view,
            Some((&spans, &[3, 6])),
            usize::MAX,
            true,
        );
        assert_eq!(tabs.visual_column(2), 7);
        let worker_spans = Rc::new(spans.as_ref().clone());
        let updated = HighlightedLine::new(Some((&worker_spans, 9)), text, 9, PLAIN_STYLE);
        assert!(!Rc::ptr_eq(
            &tabs,
            &cache.line(
                0,
                (0, 0, 7),
                &rope,
                &updated,
                Some((&worker_spans, &[3, 6])),
                usize::MAX,
                true
            )
        ));
        assert!(!Rc::ptr_eq(
            &tabs,
            &cache.line(
                0,
                (1, 1, 7),
                &rope,
                &updated,
                Some((&worker_spans, &[3, 6])),
                usize::MAX,
                true
            )
        ));
        for text in [
            "",
            "    ",
            " \thi \t",
            "ä\t \u{2003}",
            "\u{2003}ä \t",
            "\u{a0} \t ",
        ] {
            let rope = ropey::Rope::from_str(text);
            let view = HighlightedLine::new(None, text, text.chars().count(), PLAIN_STYLE);
            let glyphs = cache.line(0, (0, 0, 4), &rope, &view, None, usize::MAX, true);
            let reference = WhitespaceBoundary::compute(text);
            assert_eq!(
                (
                    glyphs.whitespace.unwrap().leading_end,
                    glyphs.whitespace.unwrap().trailing_start
                ),
                (reference.leading_end, reference.trailing_start)
            );
        }
    }

    #[test]
    fn render_line_cache_bounds_retained_lines_and_memory() {
        let text = "value ä界\t".repeat(128);
        let rope = ropey::Rope::from_str(&(text.clone() + "\n").repeat(512));
        let view = HighlightedLine::new(None, &text, text.chars().count(), PLAIN_STYLE);
        let mut cache = RenderLineCache::default();
        for line in 0..512 {
            cache.line(line, (0, 0, 4), &rope, &view, None, usize::MAX, true);
        }
        assert!(cache.entries.len() <= RenderLineCache::MAX_LINES);
        assert!(cache.storage_bytes <= RenderLineCache::MAX_BYTES);
        assert!(
            *cache.entries.front().unwrap().0 > 0,
            "old lines were not evicted"
        );
        let huge = "界".repeat(RenderLineCache::MAX_BYTES / 4);
        let view = HighlightedLine::new(None, &huge, huge.chars().count(), PLAIN_STYLE);
        let huge_rope = ropey::Rope::from_str(&huge);
        assert_eq!(
            cache
                .line(0, (1, 1, 4), &huge_rope, &view, None, usize::MAX, true)
                .chars
                .len(),
            huge.chars().count()
        );
        assert!(
            cache.entries.is_empty(),
            "oversized layout must not be retained"
        );
        assert_eq!(cache.storage_bytes, 0);
    }

    #[test]
    fn unwrapped_glyph_cache_decodes_only_the_requested_prefix_and_skips_disabled_metadata() {
        let text = "界\tvalue ".repeat(1_000);
        let rope = ropey::Rope::from_str(&text);
        let view = HighlightedLine::new(None, &text, text.chars().count(), PLAIN_STYLE);
        let mut cache = RenderLineCache::default();
        let first = cache.line(0, (0, 0, 4), &rope, &view, None, 80, false);
        assert_eq!(first.chars.len(), 80);
        assert_eq!(first.cell_columns.len(), 81);
        assert!(first.whitespace.is_none());
        assert!(first.source_spaces.is_empty());
        assert!(std::rc::Rc::ptr_eq(
            &first,
            &cache.line(0, (0, 0, 4), &rope, &view, None, 60, false)
        ));
        let extended = cache.line(0, (0, 0, 4), &rope, &view, None, 8_000, true);
        assert_eq!(extended.chars.len(), 8_000);
        assert!(extended.whitespace.is_some());
        assert_eq!(extended.span_ends, [text.chars().count()]);
    }

    #[test]
    fn render_line_cache_reuses_owned_text_for_noncontiguous_rope_lines() {
        let text = "let value = \"ä界\";\t".repeat(800);
        let rope = ropey::Rope::from_str(&text);
        let mut cache = RenderLineCache::default();
        let first = cache.line_text(0, (0, 0, 4), &rope, rope.line(0));
        let RenderLineText::Shared(first_source) = &first else {
            panic!("fixture must cross rope chunks");
        };
        let highlighted = HighlightedLine::new(None, &first, rope.len_chars(), PLAIN_STYLE)
            .with_text_snapshot(&first);
        cache.line(0, (0, 0, 4), &rope, &highlighted, None, 80, false);
        let reused = cache.line_text(0, (0, 0, 4), &rope, rope.line(0));
        let RenderLineText::Shared(reused_source) = reused else {
            panic!("cached owned text missing");
        };
        assert!(std::rc::Rc::ptr_eq(first_source, &reused_source));
        let replaced_rope = ropey::Rope::from_str(&text);
        let replaced = cache.line_text(0, (0, 0, 4), &replaced_rope, replaced_rope.line(0));
        let RenderLineText::Shared(replaced_source) = replaced else {
            panic!("replacement text missing");
        };
        assert!(!std::rc::Rc::ptr_eq(first_source, &replaced_source));
    }

    #[test]
    fn whitespace_mode_none_hides_all() {
        assert!(!should_show_whitespace(
            WhitespaceMode::None,
            ' ',
            0,
            "  hello",
            false
        ));
        assert!(!should_show_whitespace(
            WhitespaceMode::None,
            '\t',
            0,
            "\thello",
            false
        ));
    }

    #[test]
    fn whitespace_mode_all_shows_all() {
        assert!(should_show_whitespace(
            WhitespaceMode::All,
            ' ',
            2,
            "  hello  ",
            false
        ));
        assert!(should_show_whitespace(
            WhitespaceMode::All,
            '\t',
            0,
            "\thello",
            false
        ));
    }

    #[test]
    fn whitespace_mode_selection_only_in_selection() {
        assert!(should_show_whitespace(
            WhitespaceMode::Selection,
            ' ',
            0,
            " hi",
            true
        ));
        assert!(!should_show_whitespace(
            WhitespaceMode::Selection,
            ' ',
            0,
            " hi",
            false
        ));
    }

    #[test]
    fn whitespace_mode_boundary_leading_trailing() {
        let boundary = WhitespaceBoundary::compute("  hi  ");
        // Leading whitespace (col 0) should show
        assert_eq!(boundary.leading_end, 2);
        assert_eq!(boundary.trailing_start, 4);
        // col 0 < leading_end(2) => show
        assert!(0 < boundary.leading_end);
        // col 5 >= trailing_start(4) => show
        assert!(5 >= boundary.trailing_start);
    }

    #[test]
    fn whitespace_non_ws_char_always_false() {
        assert!(!should_show_whitespace(
            WhitespaceMode::All,
            'a',
            0,
            "abc",
            false
        ));
    }

    #[test]
    fn matches_for_line_empty() {
        let mut buf = Vec::new();
        matches_for_line(&[], &ropey::Rope::from_str(&"0123456789\n".repeat(8)), 5, 0, &mut buf);
        assert!(buf.is_empty());
    }

    #[test]
    fn matches_for_line_finds_matches() {
        let matches = vec![(2, 0, 3), (2, 5, 8), (5, 0, 2)];
        let mut buf = Vec::new();
        matches_for_line(&matches, &ropey::Rope::from_str(&"0123456789\n".repeat(8)), 2, 1, &mut buf);
        assert_eq!(buf.len(), 2);
        assert_eq!(buf[0], (0, 3, false));
        assert_eq!(buf[1], (5, 8, true));
    }

    #[test]
    fn matches_for_line_no_match_on_line() {
        let matches = vec![(1, 0, 3), (3, 0, 2)];
        let mut buf = Vec::new();
        matches_for_line(&matches, &ropey::Rope::from_str(&"0123456789\n".repeat(8)), 2, 0, &mut buf);
        assert!(buf.is_empty());
    }

    #[test]
    fn gutter_diff_new_line() {
        let colors = crate::app_theme::EditorTheme::Dark.colors();
        let (symbol, _style) = gutter_diff_indicator('N', Color::Black, false, &colors);
        assert_eq!(symbol, "┃");
    }

    #[test]
    fn gutter_diff_unknown() {
        let colors = crate::app_theme::EditorTheme::Dark.colors();
        let (symbol, _style) = gutter_diff_indicator('?', Color::Black, false, &colors);
        assert_eq!(symbol, " ");
    }

    #[test]
    fn hidden_line_numbers_do_not_overflow_into_the_gutter() {
        let mut buffer = String::new();
        format_gutter_line_number(&mut buffer, false, false, 1234, 0, 0);
        assert!(buffer.is_empty());

        format_empty_gutter_row(&mut buffer, false, 0);
        assert_eq!(buffer, "  ");
    }

    #[test]
    fn rulers_preserve_text_selections_and_wide_graphemes_and_blend_on_each_cell() {
        use ratatui::{Terminal, backend::TestBackend, widgets::Block};
        use crate::theme_workbench::ColorLayer;
        let area = Rect::new(3, 2, 16, 3);
        let surface = Color::Rgb(20, 30, 40);
        let selection = Color::Rgb(60, 20, 40);
        let foreground = ColorLayer::rgba(153, 187, 221, 128);
        let mut terminal = Terminal::new(TestBackend::new(24, 8)).unwrap();
        let mut original_text = None;
        let mut original_wide = None;
        terminal.draw(|frame| {
            frame.render_widget(Block::default().style(Style::default().fg(Color::Rgb(220, 230, 240)).bg(surface)), area);
            frame.render_widget(Span::styled("X", Style::default().fg(Color::Rgb(240, 200, 220)).bg(selection)
                .add_modifier(ratatui::style::Modifier::UNDERLINED).underline_color(Color::Rgb(200, 150, 180))), Rect::new(7, 2, 1, 1));
            frame.render_widget(Span::styled("界", Style::default().fg(Color::Rgb(230, 220, 240)).bg(selection)), Rect::new(6, 3, 2, 1));
            frame.buffer_mut()[(7, 4)].set_bg(selection);
            original_text = Some(frame.buffer_mut()[(7, 2)].clone());
            original_wide = Some(frame.buffer_mut()[(7, 3)].clone());
            draw_column_rulers(frame, area, &[4, 12], 0, foreground);
        }).unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(7, 2)], original_text.unwrap());
        assert_eq!(buffer[(7, 3)], original_wide.unwrap());
        assert_eq!(buffer[(7, 4)].symbol(), "│");
        assert_eq!(buffer[(7, 4)].bg, selection);
        assert_eq!(buffer[(7, 4)].fg, foreground.over(selection).unwrap());
        assert_eq!(buffer[(15, 4)].bg, surface);
        assert_eq!(buffer[(15, 4)].fg, foreground.over(surface).unwrap());
        crate::ui_control_visibility_tests::snapshot(buffer, "ruler selection and wide grapheme roles");
    }

    #[test]
    fn transparent_and_offscreen_rulers_leave_the_buffer_unchanged() {
        use ratatui::{Terminal, backend::TestBackend, widgets::Block};
        use crate::theme_workbench::ColorLayer;
        for (scroll, foreground) in [(0, ColorLayer::rgba(153, 187, 221, 0)), (100_000, ColorLayer::rgba(153, 187, 221, 255))] {
            let area = Rect::new(3, 2, 16, 3);
            let mut terminal = Terminal::new(TestBackend::new(24, 8)).unwrap();
            let mut original = None;
            terminal.draw(|frame| {
                frame.render_widget(Block::default().style(Style::default().fg(Color::Rgb(220, 230, 240)).bg(Color::Rgb(20, 30, 40))), area);
                original = Some(frame.buffer_mut().clone());
                draw_column_rulers(frame, area, &[4, 12], scroll, foreground);
            }).unwrap();
            assert_eq!(terminal.backend().buffer(), &original.unwrap());
        }
    }

    #[test]
    fn visible_multi_digit_line_numbers_use_the_full_width() {
        let mut buffer = String::new();
        format_gutter_line_number(&mut buffer, true, false, 1233, 0, 5);
        assert_eq!(buffer, " 1234");

        format_empty_gutter_row(&mut buffer, true, 5);
        assert_eq!(buffer, "       ");
    }
}
