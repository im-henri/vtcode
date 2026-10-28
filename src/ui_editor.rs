use crate::ui_colors::syntax_font_modifiers;
use std::fmt::Write as FmtWrite;
use unicode_width::UnicodeWidthStr;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::Span,
    widgets::Paragraph,
    Frame,
};
use rustc_hash::FxHashMap;

use crate::app::App;
#[cfg(test)]
use crate::app::WhitespaceMode;
use crate::helpers::len_as_u16;
use crate::ui_dialogs::get_cached_blame_line;
use crate::ui_welcome::draw_welcome_screen;

use crate::ui_editor_highlight::*;
#[path = "ui_editor_text.rs"]
mod prepared_text;
#[path = "ui_editor_plain_runs.rs"]
mod plain_runs;
#[path = "ui_editor_source_lines.rs"]
mod source_lines;
use crate::ui_editor_minimap::{
    ChangeLineIndex, DiagnosticLineIndex, MinimapParams, ScrollbarParams, draw_minimap,
    draw_scrollbar, scrollbar_change_rows_indexed,
};

/// Lines beyond cursor distance before focus-mode dimming begins.
const FOCUS_MODE_DIM_START: usize = 5;
/// Divisor controlling how fast dimming reaches maximum.
const FOCUS_MODE_DIM_RANGE: f32 = 10.0;
/// Per-character decorations are intentionally bypassed for pathological
/// single-line content (minified bundles, generated JSON, data URIs). The
/// bounded renderer still shows cached syntax colors, selections, and cursors
/// while slicing only the terminal-width viewport.
const MAX_RICH_RENDER_LINE_CHARS: usize = crate::highlight::MAX_SYNTAX_LINE_CHARS;

#[cfg(test)]
#[path = "ui_editor_render_performance_tests.rs"]
mod render_performance_tests;

#[cfg(test)]
#[path = "ui_editor_responsiveness_tests.rs"]
mod responsiveness_tests;
#[cfg(test)]
#[path = "ui_editor_viewport_tests.rs"]
mod viewport_tests;
#[cfg(test)]
#[path = "ui_editor_whitespace_tests.rs"]
mod whitespace_tests;

#[cfg(test)]
#[path = "ui_editor_decoration_tests.rs"]
mod decoration_tests;
#[cfg(test)]
#[path = "ui_editor_hotpath_tests.rs"]
mod hotpath_tests;
#[cfg(test)]
#[path = "ui_editor_pending_wrap_tests.rs"]
mod pending_wrap_tests;
#[cfg(test)]
#[path = "ui_editor_cell_batch_tests.rs"]
mod cell_batch_tests;
#[cfg(test)]
#[path = "ui_editor_row_work_tests.rs"]
mod row_work_tests;
#[cfg(test)]
#[path = "ui_editor_plain_run_tests.rs"]
mod plain_run_tests;

#[cfg(test)]
thread_local! {
    static REFERENCE_CELL_PAINT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static REFERENCE_ROW_PREPARATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn reference_row_preparation() -> bool {
    #[cfg(test)]
    { return REFERENCE_ROW_PREPARATION.get(); }
    #[cfg(not(test))]
    { false }
}

fn reference_cell_paint() -> bool {
    #[cfg(test)]
    { return REFERENCE_CELL_PAINT.get(); }
    #[cfg(not(test))]
    { false }
}

/// Terminal-width tab and virtual-space runs borrow a fixed pool. Larger
/// configured tab widths remain owned instead of leaking per-frame storage.
fn cell_spaces(width: usize) -> std::borrow::Cow<'static, str> {
    if !reference_cell_paint() {
        crate::helpers::spaces(width)
    } else {
        std::borrow::Cow::Owned(" ".repeat(width))
    }
}

fn marked_tab(width: usize) -> std::borrow::Cow<'static, str> {
    static MARKED_TAB: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
        let mut text = String::with_capacity(514);
        text.push('→');
        text.push_str(&crate::helpers::spaces(511));
        text
    });
    if !reference_cell_paint() && (1..=512).contains(&width) {
        std::borrow::Cow::Borrowed(&MARKED_TAB[..width + 2])
    } else {
        std::borrow::Cow::Owned(format!("→{}", " ".repeat(width.saturating_sub(1))))
    }
}

fn indent_guide_depth(visual_col: usize, indent_columns: usize, tab_size: usize) -> Option<usize> {
    (visual_col < indent_columns && tab_size > 0 && visual_col.is_multiple_of(tab_size)
        && indent_columns - visual_col >= tab_size)
        .then(|| visual_col / tab_size)
}

/// One terminal cell represents both decorations without displacing the code.
fn guide_glyph(ch: char, whitespace_visible: bool) -> &'static str {
    match (ch, whitespace_visible) {
        (_, false) => "│",
        ('\t', true) => "↦",
        _ => "┆",
    }
}

fn guide_style(style: Style, color: Color, whitespace_color: Color, whitespace_visible: bool) -> Style {
    let color = if whitespace_visible && style.bg == Some(color) {
        if style.bg == Some(whitespace_color) { style.fg.unwrap_or(color) } else { whitespace_color }
    } else { color };
    style.fg(color).remove_modifier(Modifier::DIM)
}

fn push_cell_padding(spans: &mut Vec<Span<'_>>, count: usize, style: Style) {
    if reference_cell_paint() {
        spans.extend((0..count).map(|_| Span::styled(" ", style)));
    } else if count > 0 {
        spans.push(Span::styled(cell_spaces(count), style));
    }
}

fn visible_search_runs(
    matches: &[(usize, usize, bool)],
    start: usize,
    end: usize,
    scratch: &mut Vec<(usize, usize, bool)>,
    output: &mut Vec<(usize, usize, bool)>,
) {
    scratch.clear();
    scratch.extend(matches.iter().copied().filter(|&(from, to, _)| from < end && to > start));
    flatten_priority_ranges(scratch, output);
}

#[cfg(test)]
thread_local! {
    static REFERENCE_PENDING_WRAP_PAINT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static PENDING_WRAP_WORK: std::cell::Cell<PendingWrapWork> = const { std::cell::Cell::new(PendingWrapWork {
        scanned: 0, requested: 0, styled: 0, padding: 0, spans: 0, pending_rows: 0,
    }) };
}

#[cfg(test)]
#[derive(Clone, Copy, Default, Debug)]
struct PendingWrapWork {
    scanned: usize,
    requested: usize,
    styled: usize,
    padding: usize,
    spans: usize,
    pending_rows: usize,
}

fn reference_pending_wrap_paint() -> bool {
    #[cfg(test)]
    {
        return REFERENCE_PENDING_WRAP_PAINT.get();
    }
    #[cfg(not(test))]
    {
        false
    }
}

/// A pending layout keeps one logical line per row. Only prepare the source
/// prefix that can contribute cells to Paragraph's truncator. Style boundaries
/// and injected color swatches can consume additional cells, so this source
/// width is deliberately conservative. Keep the first overflowing grapheme:
/// truncation must see it, and combining sequences must remain whole.
fn pending_wrap_paint_end(text: &str, width: usize, tab_size: usize) -> usize {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    if width == 0 {
        return 0;
    }
    let mut column = 0usize;
    let mut source_visual = 0usize;
    let mut cells = 0usize;
    let tab_size = tab_size.max(1);
    for grapheme in text.graphemes(true) {
        let characters = grapheme.chars().count();
        let grapheme_width = if grapheme == "\t" {
            let tab = tab_size - source_visual % tab_size;
            source_visual += tab;
            tab
        } else {
            source_visual += characters;
            if grapheme.contains(char::is_control)
                // A style boundary can split a text-presentation sequence.
                // At width one its wide base may then be skipped completely,
                // so do not count that sequence toward a proven paint limit.
                || (width == 1 && grapheme.chars().any(|ch| unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) > width))
            {
                0
            } else {
                grapheme.width()
            }
        };
        column += characters;
        // Expanded tabs contain individually fitting spaces. Other graphemes
        // wider than the entire area are skipped by Paragraph, not a stop.
        if grapheme_width <= width || grapheme == "\t" {
            cells = cells.saturating_add(grapheme_width);
            if cells > width {
                break;
            }
        }
    }
    column
}
/// Index ordinary multi-line diagnostics directly, while keeping provider
/// mistakes such as a range spanning the entire document sparse.
const MAX_INDEXED_DIAGNOSTIC_SPAN_LINES: usize = 64;
const MAX_INDEXED_MULTILINE_DIAGNOSTIC_REFS: usize = 65_536;
const MAX_CACHED_EDITOR_PANES: usize = 16;

pub(crate) struct EditorDiagnosticCacheEntry {
    key: (Option<std::path::PathBuf>, u64, u64),
    lines: std::sync::Arc<Vec<(usize, u8)>>,
    diagnostics: std::sync::Arc<Vec<crate::lsp::LspDiagnostic>>,
    overview: std::sync::Arc<DiagnosticLineIndex>,
    by_line: std::sync::Arc<FxHashMap<usize, Vec<usize>>>,
    wide: std::sync::Arc<Vec<usize>>,
}

type ChangeIndexKey = (u64, usize, usize, bool, bool, u64, u64);

pub(crate) struct EditorChangeIndexEntry {
    key: ChangeIndexKey,
    index: std::sync::Arc<ChangeLineIndex>,
    // Retain immutable sources so allocation addresses cannot be reused while
    // their index is cached. Each editor keeps only its latest source.
    _git_source: Option<crate::editor::PreparedGitLineStatus>,
    _compare_source: Option<std::sync::Arc<crate::app::DiffRenderCache>>,
}

pub(crate) fn refresh_editor_change_index(app: &mut App, diff_changes: Option<&[(usize, char)]>) {
    let Some(editor) = app.tabs.get(app.active_tab).map(|tab| &tab.editor) else {
        return;
    };
    let source = diff_changes.unwrap_or_else(|| editor.git_change_lines());
    let compare = diff_changes.is_some();
    let key = (
        editor.navigation_id,
        source.as_ptr() as usize,
        source.len(),
        compare,
        editor.large_file_mode,
        if compare {
            app.git_compare_generation
        } else {
            0
        },
        if compare {
            app.diff_refresh_generation
        } else {
            0
        },
    );
    if let Some(index) = app
        .editor_change_index_cache
        .iter()
        .position(|entry| entry.key == key)
    {
        let entry = app.editor_change_index_cache.remove(index).unwrap();
        app.cached_editor_change_index = std::sync::Arc::clone(&entry.index);
        app.editor_change_index_cache.push_back(entry);
        return;
    }
    let entry = EditorChangeIndexEntry {
        key,
        index: std::sync::Arc::new(if editor.large_file_mode && !compare {
            ChangeLineIndex::default()
        } else {
            ChangeLineIndex::from_changes(source)
        }),
        _git_source: (!compare).then(|| editor.prepared_git_line_status()),
        _compare_source: compare.then(|| std::sync::Arc::clone(&app.diff_render_cache)),
    };
    app.editor_change_index_cache
        .retain(|cached| cached.key.0 != key.0);
    if app.editor_change_index_cache.len() >= MAX_CACHED_EDITOR_PANES {
        app.editor_change_index_cache.pop_front();
    }
    app.cached_editor_change_index = std::sync::Arc::clone(&entry.index);
    app.editor_change_index_cache.push_back(entry);
}

#[path = "ui_editor_gutter.rs"]
mod gutter;

pub(crate) struct CursorWordEntry {
    key: crate::app::CursorWordKey,
    source: ropey::Rope,
    range: Option<(usize, usize)>,
    pub(crate) word: Option<std::sync::Arc<str>>,
}

fn refresh_cursor_word(
    editor: &crate::editor::Editor,
    current_key: &mut crate::app::CursorWordKey,
    current_word: &mut Option<std::sync::Arc<str>>,
    cache: &mut std::collections::VecDeque<CursorWordEntry>,
) {
    *current_word = cached_cursor_word(editor, cache);
    *current_key = (editor.navigation_id, editor.cursor_line, editor.cursor_col, editor.content_version);
}

/// Share the immutable word between painting and background-analysis targets.
/// Moving within a token or scrolling must not rescan/copy it for each consumer.
pub(crate) fn cached_cursor_word(
    editor: &crate::editor::Editor,
    cache: &mut std::collections::VecDeque<CursorWordEntry>,
) -> Option<std::sync::Arc<str>> {
    let key = (
        editor.navigation_id,
        editor.cursor_line,
        editor.cursor_col,
        editor.content_version,
    );
    if let Some(index) = cache.iter().rposition(|entry| {
        entry.key.0 == key.0 && entry.key.1 == key.1 && entry.key.3 == key.3
            && entry.source.is_instance(&editor.rope)
            && entry.range.map_or(entry.key.2 == key.2, |(start, end)| {
                (start..end).contains(&key.2)
            })
    }) {
        let word = cache[index].word.clone();
        if index + 1 != cache.len() {
            let entry = cache.remove(index).unwrap();
            cache.push_back(entry);
        }
        word
    } else {
        let range = editor.word_range_at_cursor();
        let word = range.map(|(start, end)| {
            std::sync::Arc::from(editor.rope.line(editor.cursor_line).slice(start..end).to_string())
        });
        cache.retain(|entry| entry.key.0 != editor.navigation_id);
        if cache.len() >= MAX_CACHED_EDITOR_PANES {
            cache.pop_front();
        }
        cache.push_back(CursorWordEntry {
            key, source: editor.rope.clone(), range, word: word.clone(),
        });
        word
    }
}

fn paint_editor_row(f: &mut Frame, rows: &mut prepared_text::Rows, spans: &mut Vec<Span<'_>>, marker: Option<Style>, colors: &crate::app_theme::ThemeColors) {
    if let Some(marker) = marker {
        // Only decorated marker lines need a contrast check. Ordinary syntax
        // stays exactly as authored, and controls retain their own fills.
        for span in spans.iter_mut() {
            let background = span.style.bg.or(marker.bg).unwrap_or(colors.bg_editor);
            let foreground = span.style.fg.or(marker.fg).unwrap_or(colors.fg_text);
            span.style = span.style.fg(colors.ui.readable_foreground(foreground, background, 4.5));
        }
    }
    rows.push(f.buffer_mut(), spans, marker.unwrap_or_default());
}

fn plain_theme_style(color: Color) -> crate::textmate::Style {
    let mut style = PLAIN_STYLE;
    if let Color::Rgb(r,g,b) = color { style.foreground = crate::textmate::Color {r,g,b,a:255}; }
    style
}


/// Occurrence highlighting only accepts short single-line selections. Check
/// the rope range before copying it so dragging over a generated/minified line
/// cannot allocate megabytes of text merely to reject it afterwards.
fn selection_occurrence_text(editor: &crate::editor::Editor) -> Option<String> {
    let (sl, _, el, _) = editor.selection_range()?;
    if sl != el {
        return None;
    }
    let (start, end, _, _) = editor.clamped_selection_indices()?;
    let slice = editor.rope.slice(start..end);
    if slice.len_bytes() > 200 {
        // Preserve the existing trimmed-length rule, including selections
        // padded with whitespace, without allocating rejected selections.
        let leading = slice.chars().take_while(|c| c.is_whitespace()).count();
        let trailing = slice.chars_at(slice.len_chars()).reversed()
            .take(slice.len_chars() - leading)
            .take_while(|c| c.is_whitespace())
            .count();
        let trimmed = slice.slice(leading..slice.len_chars() - trailing);
        if trimmed.len_bytes() > 200 || trimmed.len_chars() == 0 {
            return None;
        }
    }
    let text = slice.to_string();
    (!text.trim().is_empty()).then_some(text)
}

pub(crate) struct SelectionWordEntry {
    editor: u64,
    range: (usize, usize, usize, usize),
    source: ropey::Rope,
    text: Option<std::sync::Arc<str>>,
}

fn cached_selection_occurrence_text(
    editor: &crate::editor::Editor,
    cache: &mut std::collections::VecDeque<SelectionWordEntry>,
) -> Option<std::sync::Arc<str>> {
    let range = editor.selection_range()?;
    if let Some(index) = cache.iter().rposition(|entry| {
        entry.editor == editor.navigation_id && entry.range == range
            && entry.source.is_instance(&editor.rope)
    }) {
        let entry = cache.remove(index).unwrap();
        let text = entry.text.clone();
        cache.push_back(entry);
        return text;
    }
    let text = selection_occurrence_text(editor).map(std::sync::Arc::<str>::from);
    cache.retain(|entry| entry.editor != editor.navigation_id);
    // Preserve matching of whitespace-padded selections without retaining
    // arbitrary amounts of text from a generated line in each visited tab.
    if text.as_ref().is_none_or(|text| text.len() <= 4096) {
        if cache.len() >= MAX_CACHED_EDITOR_PANES { cache.pop_front(); }
        cache.push_back(SelectionWordEntry {
            editor: editor.navigation_id, range, source: editor.rope.clone(), text: text.clone(),
        });
    }
    text
}

/// Render a large document without ever materializing or scanning an entire
/// visible line. Rope line lengths and slices are logarithmic, so even a
/// hundreds-of-megabytes single-line file only contributes the terminal-width
/// window currently on screen.
#[derive(Clone, Copy)]
struct LargeFileEditorLayout {
    gutter_area: Rect,
    scope_rail_area: Rect,
    code_area: Rect,
    minimap_sep_area: Rect,
    minimap_area: Rect,
    scrollbar_area: Rect,
    h_scrollbar_area: Rect,
    render_line_numbers: bool,
    gutter_width: usize,
    max_line_width: usize,
    show_h_scrollbar: bool,
}

pub(crate) fn cached_overview_change_rows(
    app: &mut App,
    height: usize,
    diff_changes: Option<&[(usize, char)]>,
) -> std::sync::Arc<crate::ui_editor_minimap::ChangeRows> {
    let Some(editor) = app.tabs.get(app.active_tab).map(|tab| &tab.editor) else {
        return std::sync::Arc::default();
    };
    let (diff_ptr, diff_len) =
        diff_changes.map_or((0, 0), |changes| (changes.as_ptr() as usize, changes.len()));
    let key = (
        editor.navigation_id,
        editor.file_path.clone(),
        editor.modified,
        editor.large_file_mode,
        editor.overview_change_revision,
        editor.git_line_status_len(),
        editor.line_count(),
        height,
        diff_ptr,
        diff_len,
        app.git_compare_generation,
        app.diff_refresh_generation,
    );
    refresh_editor_change_index(app, diff_changes);
    if app.cached_overview_change_rows_key.as_ref() != Some(&key) {
        if let Some(index) = app
            .editor_overview_cache
            .iter()
            .position(|(cached, _)| cached == &key)
        {
            let entry = app.editor_overview_cache.remove(index).unwrap();
            app.cached_overview_change_rows = std::sync::Arc::clone(&entry.1);
            app.editor_overview_cache.push_back(entry);
        } else {
            app.cached_overview_change_rows = std::sync::Arc::new(scrollbar_change_rows_indexed(
                &app.tabs[app.active_tab].editor,
                height,
                diff_changes.is_some(),
                &app.cached_editor_change_index,
            ));
            if app.editor_overview_cache.len() >= MAX_CACHED_EDITOR_PANES {
                app.editor_overview_cache.pop_front();
            }
            app.editor_overview_cache.push_back((
                key.clone(),
                std::sync::Arc::clone(&app.cached_overview_change_rows),
            ));
        }
        app.cached_overview_change_rows_key = Some(key);
    }
    std::sync::Arc::clone(&app.cached_overview_change_rows)
}

pub(crate) fn refresh_cached_editor_diagnostics(app: &mut App) {
    let file_path = app
        .editor()
        .and_then(crate::editor::Editor::syntax_file_path);
    let lsp_version = app.lsp_manager.diagnostics_version;
    let extension_version = app.extension_manager.diagnostics_version();
    if app.cached_diag_key.0.as_deref() == file_path
        && app.cached_diag_key.1 == lsp_version
        && app.cached_diag_key.2 == extension_version
    {
        return;
    }
    if let Some(index) = app.editor_diagnostic_cache.iter().position(|entry| {
        entry.key.0.as_deref() == file_path
            && entry.key.1 == lsp_version
            && entry.key.2 == extension_version
    }) {
        let entry = app.editor_diagnostic_cache.remove(index).unwrap();
        app.cached_diag_lines = std::sync::Arc::clone(&entry.lines);
        app.cached_editor_diagnostics = std::sync::Arc::clone(&entry.diagnostics);
        app.cached_diagnostic_index = std::sync::Arc::clone(&entry.overview);
        app.cached_diag_indices_by_line = std::sync::Arc::clone(&entry.by_line);
        app.cached_wide_diag_indices = std::sync::Arc::clone(&entry.wide);
        app.cached_diag_key = entry.key.clone();
        app.editor_diagnostic_cache.push_back(entry);
        return;
    }
    let diag_key = (
        file_path.map(std::path::Path::to_path_buf),
        lsp_version,
        extension_version,
    );

    let diagnostics = merged_diagnostics_from(&app.lsp_manager, &app.extension_manager, file_path, &app.editor().unwrap().rope);
    app.cached_diag_lines = std::sync::Arc::new(
        diagnostics
            .iter()
            .map(|diagnostic| {
                let severity = match diagnostic.severity {
                    lsp_types::DiagnosticSeverity::ERROR => 1,
                    lsp_types::DiagnosticSeverity::WARNING => 2,
                    lsp_types::DiagnosticSeverity::INFORMATION => 3,
                    _ => 4,
                };
                (diagnostic.line, severity)
            })
            .collect(),
    );
    let (indices_by_line, wide_indices) = build_diagnostic_line_index(&diagnostics);
    app.cached_diag_indices_by_line = std::sync::Arc::new(indices_by_line);
    app.cached_wide_diag_indices = std::sync::Arc::new(wide_indices);
    app.cached_editor_diagnostics = std::sync::Arc::new(diagnostics);
    app.cached_diagnostic_index = std::sync::Arc::new(DiagnosticLineIndex::from_diagnostics(
        &app.cached_diag_lines,
    ));
    // Old provider revisions cannot be used by any pane. Drop them together
    // rather than retaining multiple full diagnostic snapshots per file.
    app.editor_diagnostic_cache
        .retain(|entry| entry.key.1 == lsp_version && entry.key.2 == extension_version);
    if app.editor_diagnostic_cache.len() >= MAX_CACHED_EDITOR_PANES {
        app.editor_diagnostic_cache.pop_front();
    }
    app.editor_diagnostic_cache
        .push_back(EditorDiagnosticCacheEntry {
            key: diag_key.clone(),
            lines: std::sync::Arc::clone(&app.cached_diag_lines),
            diagnostics: std::sync::Arc::clone(&app.cached_editor_diagnostics),
            overview: std::sync::Arc::clone(&app.cached_diagnostic_index),
            by_line: std::sync::Arc::clone(&app.cached_diag_indices_by_line),
            wide: std::sync::Arc::clone(&app.cached_wide_diag_indices),
        });
    app.cached_diag_key = diag_key;
}

fn build_diagnostic_line_index(
    diagnostics: &[crate::lsp::LspDiagnostic],
) -> (FxHashMap<usize, Vec<usize>>, Vec<usize>) {
    let mut by_line: FxHashMap<usize, Vec<usize>> = FxHashMap::default();
    let mut wide = Vec::new();
    let mut multiline_refs = 0usize;

    for (index, diagnostic) in diagnostics.iter().enumerate() {
        if diagnostic.end_line < diagnostic.line {
            continue;
        }
        if diagnostic.end_line == diagnostic.line {
            by_line.entry(diagnostic.line).or_default().push(index);
            continue;
        }

        let span = diagnostic
            .end_line
            .saturating_sub(diagnostic.line)
            .saturating_add(1);
        if span <= MAX_INDEXED_DIAGNOSTIC_SPAN_LINES
            && multiline_refs.saturating_add(span) <= MAX_INDEXED_MULTILINE_DIAGNOSTIC_REFS
        {
            for line in diagnostic.line..=diagnostic.end_line {
                by_line.entry(line).or_default().push(index);
            }
            multiline_refs += span;
        } else {
            wide.push(index);
        }
    }

    (by_line, wide)
}

fn diagnostic_indices_for_line(
    diagnostics: &[crate::lsp::LspDiagnostic],
    by_line: &FxHashMap<usize, Vec<usize>>,
    wide_indices: &[usize],
    line: usize,
    output: &mut Vec<usize>,
) {
    output.clear();
    if let Some(indices) = by_line.get(&line) {
        output.extend_from_slice(indices);
    }
    let indexed_len = output.len();
    output.extend(wide_indices.iter().copied().filter(|&index| {
        diagnostics
            .get(index)
            .is_some_and(|diagnostic| line >= diagnostic.line && line <= diagnostic.end_line)
    }));
    if indexed_len > 0 && output.len() > indexed_len {
        output.sort_unstable();
    }
}

fn document_highlights_for_line(
    highlights: &[(u32, u32, u32, u32)],
    multiline: &[(u32, u32, u32, u32)],
    line: u32,
    output: &mut Vec<(u32, u32, u32, u32)>,
) {
    output.clear();
    let start = highlights.partition_point(|highlight| highlight.0 < line);
    output.extend(
        highlights[start..]
            .iter()
            .copied()
            .take_while(|highlight| highlight.0 == line),
    );
    output.extend(
        multiline
            .iter()
            .copied()
            .filter(|highlight| highlight.0 < line && highlight.2 >= line),
    );
}

fn inlay_hints_for_line(hints: &[crate::app::InlayHintDisplay], line: u32) -> &[crate::app::InlayHintDisplay] {
    let start = hints.partition_point(|hint| hint.line < line);
    let end = start + hints[start..].partition_point(|hint| hint.line == line);
    &hints[start..end]
}

fn highlight_column_ranges(
    highlights: &[(u32, u32, u32, u32)],
    line: u32,
    output: &mut Vec<(usize, usize)>,
) {
    output.clear();
    output.extend(highlights.iter().map(|&(sl, sc, el, ec)| (
        if line == sl { sc as usize } else { 0 },
        if line == el { ec as usize } else { usize::MAX },
    )));
    merge_column_ranges(output);
}

fn diagnostic_column_ranges(
    diagnostics: &[crate::lsp::LspDiagnostic],
    indices: &[usize],
    line: usize,
    colors: &crate::app_theme::ThemeColors,
    scratch: &mut Vec<(usize, usize, Color)>,
    output: &mut Vec<(usize, usize, Color)>,
) {
    scratch.clear();
    scratch.extend(indices.iter().map(|&index| {
        let diagnostic = &diagnostics[index];
        (
            if line == diagnostic.line { diagnostic.col } else { 0 },
            if line == diagnostic.end_line { diagnostic.end_col } else { usize::MAX },
            colors.ui.diagnostic(diagnostic.severity),
        )
    }));
    flatten_priority_ranges(scratch, output);
}

#[cfg(test)]
fn semantic_color_for_token(
    token: &crate::lsp::DecodedSemanticToken,
    legend: Option<&lsp_types::SemanticTokensLegend>,
    language: &str,
    theme: Option<&crate::semantic_theme::SemanticTheme>,
    cache: &mut FxHashMap<(u32, u32), Option<Style>>,
) -> Option<Style> {
    let legend = legend?;
    let theme = theme?;
    // A file usually has just a handful of token type/modifier combinations.
    // Resolve theme selectors once per combination, never once per character.
    *cache.entry((token.token_type, token._token_modifiers)).or_insert_with(|| {
        crate::lsp::themed_semantic_token_style(token, legend, language, Some(theme))
    })
}

#[cfg(test)]
#[path = "ui_semantic_cache_tests.rs"]
mod semantic_cache_tests;

fn editor_semantic_color_for_token(
    token: &crate::lsp::DecodedSemanticToken,
    manager: &crate::lsp::LspManager,
    editor: &crate::editor::Editor,
    theme: Option<&crate::semantic_theme::SemanticTheme>,
    cache: &mut FxHashMap<(u32, u32), Option<Style>>,
) -> Option<Style> {
    let theme = theme?;
    // The cache lives for one editor draw. Resolve the server and language
    // only for a new style, including caching unsupported token styles.
    *cache.entry((token.token_type, token._token_modifiers)).or_insert_with(|| {
        let legend = manager.semantic_legend_for_file(editor.file_path.as_deref())?;
        crate::lsp::themed_semantic_token_style(token, legend, editor.language_name(), Some(theme))
    })
}

#[cfg(test)]
fn syntax_foregrounds_for_range(
    highlighted: &[(crate::textmate::Style, String)],
    span_char_lengths: &[usize],
    background: Color,
    start: usize,
    end: usize,
) -> Vec<Option<Color>> {
    let mut foregrounds = vec![None; end.saturating_sub(start)];
    let mut span_start = 0usize;
    for ((style, _), &span_len) in highlighted.iter().zip(span_char_lengths) {
        let span_end = span_start.saturating_add(span_len);
        if span_end > start && span_start < end {
            let from = span_start.max(start) - start;
            let to = span_end.min(end) - start;
            foregrounds[from..to].fill(Some(crate::ui_colors::syntax_color(style.foreground, background)));
        }
        span_start = span_end;
        if span_start >= end {
            break;
        }
    }
    foregrounds
}

/// Walk the worker's ordered syntax runs directly. Preparing an intermediate
/// foreground vector duplicated the visible window and allocated per row,
/// even though the bounded renderer consumes those colors only once.
struct SyntaxForegroundCursor<'a> {
    spans: &'a [(crate::textmate::Style, String)],
    lengths: &'a [usize],
    background: Color,
    next: usize,
    end: usize,
    color: Option<Color>,
    #[cfg(test)]
    reference: Option<(usize, Vec<Option<Color>>)>,
}

impl<'a> SyntaxForegroundCursor<'a> {
    fn new(
        source: Option<(&'a [(crate::textmate::Style, String)], &'a [usize])>,
        background: Color,
        _start: usize,
        _end: usize,
    ) -> Self {
        let (spans, lengths) = source.unwrap_or((&[], &[]));
        Self {
            spans, lengths, background, next: 0, end: 0, color: None,
            #[cfg(test)]
            reference: reference_cell_paint().then(|| (
                _start, syntax_foregrounds_for_range(spans, lengths, background, _start, _end),
            )),
        }
    }

    fn at(&mut self, column: usize) -> Option<Color> {
        #[cfg(test)]
        if let Some((start, colors)) = &self.reference {
            return colors.get(column.saturating_sub(*start)).copied().flatten();
        }
        while self.end <= column {
            let (Some((style, _)), Some(&length)) =
                (self.spans.get(self.next), self.lengths.get(self.next))
            else {
                self.color = None;
                break;
            };
            self.next += 1;
            self.end = self.end.saturating_add(length);
            // No color conversion for spans wholly left of the viewport.
            if self.end > column {
                self.color = Some(crate::ui_colors::syntax_color(style.foreground, self.background));
            }
        }
        self.color
    }
}

/// Keep the first overflowing grapheme so Paragraph stops at exactly the
/// same boundary as the complete annotation, including a wide final glyph.
fn error_lens_prefix(text: &str, cells: usize, whole_width: usize) -> &str {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    let mut width = 0usize;
    for (byte, grapheme) in text.grapheme_indices(true) {
        if grapheme.chars().any(char::is_control) {
            continue;
        }
        let grapheme_width = grapheme.width();
        if grapheme_width > whole_width {
            continue;
        }
        width += grapheme_width;
        if width > cells {
            return &text[..byte + grapheme.len()];
        }
    }
    text
}

/// Match Paragraph's truncator: an over-area grapheme is ignored, while the
/// first ordinary grapheme that exceeds remaining space stops the line.
fn advance_error_lens_width(occupied: &mut usize, text: &str, whole_width: usize) -> bool {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    for grapheme in text.graphemes(true) {
        if grapheme.chars().any(char::is_control) {
            continue;
        }
        let width = grapheme.width();
        if width > whole_width {
            continue;
        }
        if occupied.saturating_add(width) > whole_width {
            *occupied = whole_width;
            return true;
        }
        *occupied += width;
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn append_error_lens(
    rows: &mut prepared_text::Rows,
    spans: &mut Vec<Span<'_>>,
    buffer: &mut String,
    occupied: &mut usize,
    visible_width: usize,
    icon: &str,
    text: &str,
    style: Style,
    reference: bool,
) -> usize {
    use unicode_width::UnicodeWidthStr;
    let remaining = visible_width.saturating_sub(*occupied);
    if !reference && remaining == 0 {
        return 0;
    }
    buffer.clear();
    let _ = write!(buffer, "  {icon} ");
    let mut prefix_end = *occupied;
    let prefix_stops = advance_error_lens_width(&mut prefix_end, buffer, visible_width);
    if reference {
        buffer.push_str(text);
    } else if !prefix_stops {
        buffer.push_str(error_lens_prefix(
            text,
            visible_width.saturating_sub(prefix_end),
            visible_width,
        ));
    }
    advance_error_lens_width(occupied, buffer, visible_width);
    let displayed = buffer.as_str().width();
    rows.flush(spans, buffer, style);
    displayed
}

fn draw_large_file_editor(f: &mut Frame, app: &mut App, layout: LargeFileEditorLayout) {
    let LargeFileEditorLayout {
        gutter_area,
        scope_rail_area,
        code_area,
        minimap_sep_area,
        minimap_area,
        scrollbar_area,
        h_scrollbar_area,
        render_line_numbers,
        gutter_width,
        max_line_width,
        show_h_scrollbar,
    } = layout;
    let editor = &app.tabs[app.active_tab].editor;
    let visible_lines = code_area.height as usize;
    let visible_width = code_area.width as usize;
    let scroll = editor.scroll_offset;
    let h_scroll = editor.horizontal_scroll;
    let cursor_line = editor.cursor_line;
    let cursor_col = editor.cursor_col;
    let selection = editor.selection_range();
    let file_path = editor.syntax_file_path().map(std::path::Path::to_path_buf);
    let syntax_language = editor.syntax_language_token().map(str::to_string);
    let content_version = editor.content_version;
    let show_cursor = (app.cursor_visible
        || (app.terminal_has_focus
            && app.animations.cursor_smear_active(
                editor.navigation_id,
                &app.animation_settings,
                std::time::Instant::now(),
            )))
        && !app.rendering_inactive_editor
        && !app.status_details.open;
    let colors = app.colors;
    let mut selection_style = app.copy_animation_selection_style();
    let extra_cursor_viewport = ExtraCursorViewport::new(
        editor,
        scroll
            ..scroll
                .saturating_add(visible_lines)
                .min(editor.rope.len_lines()),
    );
    let mut extra_cursor_scratch = ExtraCursorLine::default();

    let mut gutter_lines = gutter::Rows::new(f, gutter_area, colors.bg_dark);
    let mut code_lines = prepared_text::Rows::new(code_area, Style::default().bg(app.colors.bg_editor));
    let mut gutter_num_buf = String::with_capacity(gutter_width + 2);
    let mut line_matches = Vec::with_capacity(8);
    let mut visible_matches = Vec::new();
    let mut line_search_runs = Vec::new();
    let animate = app.animation_settings.editor_effects_enabled() && app.terminal_has_focus && !app.rendering_inactive_editor
            && editor.extra_cursors.is_empty()
            && !app.should_render_diff_for_active_editor();
    let mut animation_geometry = app.animation_frame_cache.geometry(code_area, animate);

    animation_geometry.defer_gradients(app.editor_gradient_area);
    animation_geometry.set_bounded_source();

    let mut spans = Vec::with_capacity(32);
    let mut cell_batch = String::with_capacity(visible_width);
    for screen_row in 0..visible_lines {
        let line_idx = scroll.saturating_add(screen_row);
        if line_idx >= editor.rope.len_lines() {
            format_empty_gutter_row(&mut gutter_num_buf, render_line_numbers, gutter_width);
            gutter_lines.push(f, [Span::styled(
                gutter_num_buf.as_str(),
                Style::default().fg(colors.fg_tilde).bg(colors.bg_dark),
            )]);
            spans.push(Span::styled(crate::helpers::spaces(visible_width), Style::default().bg(colors.bg_editor)));
            code_lines.push(f.buffer_mut(), &mut spans, Style::default());
            continue;
        }

        animation_geometry.map_scope_row(line_idx, 0, screen_row);

        let is_current_line = line_idx == cursor_line;
        let gutter_bg = if is_current_line {
            colors.bg_line_highlight
        } else {
            colors.bg_dark
        };
        format_gutter_line_number(
            &mut gutter_num_buf,
            render_line_numbers,
            app.relative_line_numbers,
            line_idx,
            cursor_line,
            gutter_width,
        );
        let bookmark = if editor.bookmarks.contains(&line_idx) {
            Span::styled("●", Style::default().fg(colors.fg_bookmark).bg(gutter_bg))
        } else {
            Span::styled(" ", Style::default().bg(gutter_bg))
        };
        let number_style = if is_current_line {
            Style::default()
                .fg(colors.fg_text)
                .bg(gutter_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(colors.ui.fg_line_number).bg(gutter_bg)
        };
        gutter_lines.push(f, [
            bookmark,
            Span::styled(gutter_num_buf.as_str(), number_style),
            Span::styled(" ", Style::default().bg(gutter_bg)),
        ]);

        let line_bg = if is_current_line {
            colors.bg_line_highlight
        } else {
            colors.bg_editor
        };
        let line = editor.rope.line(line_idx);
        let line_bg = crate::merge_conflict_ui::content_background(editor, line_idx, &colors, line_bg);
        let merge_style = colors.ui.merge_marker_style(line.chars(), line_bg);
        let line_bg = merge_style.and_then(|style| style.bg).unwrap_or(line_bg);
        let content_len = editor.line_len(line_idx);
        let slice_start = h_scroll.min(content_len);
        let slice_end = slice_start.saturating_add(visible_width).min(content_len);
        let slice = line.slice(slice_start..slice_end);
        let mut cached_highlight = app.highlighter.highlighted_line_by_index_if_cached(
            line_idx,
            file_path.as_deref(),
            syntax_language.as_deref(),
            app.syntax_theme.as_str(),
            content_version,
        );
        if cached_highlight.is_none() {
            // Only materialize a bounded prefix on a cache miss. Normal frames
            // remain index-only, while an edit can keep exact stale colors on
            // unchanged lines until the worker result arrives.
            let stale_text = line
                .slice(
                    ..line
                        .len_chars()
                        .min(crate::highlight::MAX_SYNTAX_LINE_CHARS),
                )
                .to_string();
            cached_highlight = app.highlighter.reconcile_stale_line_for_render(
                &stale_text,
                line_idx,
                file_path.as_deref(),
                syntax_language.as_deref(),
                app.syntax_theme.as_str(),
                content_version,
            );
        }
        let mut syntax_foregrounds = SyntaxForegroundCursor::new(
            cached_highlight.as_ref().map(|(spans, lengths)| (spans.as_slice(), lengths.as_slice())),
            line_bg, slice_start, slice_end,
        );
        matches_for_line(
            &app.tabs[app.active_tab].file_search.search_matches,
            &editor.rope,
            line_idx,
            app.tabs[app.active_tab].file_search.search_current,
            &mut line_matches,
        );
        if !reference_cell_paint() {
            visible_search_runs(&line_matches, slice_start, slice_end, &mut visible_matches, &mut line_search_runs);
        }
        let mut search_runs = ValueRangeCursor::new(&line_search_runs, slice_start);

        let mut cell_batch_style = Style::default();
        let mut rendered_width = 0usize;
        let mut animation_col = 0usize;
        let mut tab_column = None;
        let extra_cursor_line =
            extra_cursor_viewport.line(editor, line_idx, &mut extra_cursor_scratch);
        let mut extra_cursor_cols =
            CursorColumnCursor::new(&extra_cursor_line.columns, slice_start);
        for (offset, ch) in slice.chars().enumerate() {
            if rendered_width >= visible_width {
                break;
            }
            let absolute_col = slice_start + offset;
            let cell_width = if ch == '\t' {
                let width = tab_column.map_or_else(
                    || crate::editor_columns::tab_width_at(line, absolute_col, editor.tab_size),
                    |previous: usize| editor.tab_size.max(1) - previous % editor.tab_size.max(1),
                );
                tab_column = Some(0);
                width
            } else {
                if let Some(previous) = &mut tab_column { *previous += 1; }
                unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0)
            };
            let selected = selection.is_some_and(|(sl, sc, el, ec)| {
                line_idx >= sl
                    && line_idx <= el
                    && (line_idx != sl || absolute_col >= sc)
                    && (line_idx != el || absolute_col < ec)
            });
            let extra_cursor = if extra_cursor_viewport.reference() {
                editor
                    .extra_cursors
                    .iter()
                    .any(|&(line, col)| line == line_idx && col == absolute_col)
            } else {
                extra_cursor_cols.contains(absolute_col)
            };
            let cursor = (is_current_line && absolute_col == cursor_col && show_cursor)
                || (extra_cursor && !app.rendering_inactive_editor);
            let search_style = if reference_cell_paint() {
                line_matches.iter().find_map(|&(start, end, current)| {
                    (absolute_col >= start && absolute_col < end).then_some(current)
                })
            } else {
                search_runs.at(absolute_col)
            };
            let foreground = syntax_foregrounds
                .at(absolute_col)
                .unwrap_or(colors.fg_text);
            let mut style = Style::default().fg(foreground).bg(line_bg);
            if let Some(current) = search_style {
                style = colors.ui.find_style(style, current, colors.bg_search_match);
            }
            if selected {
                style = selection_style.apply(style);
            }
            if cursor {
                if is_current_line && absolute_col == cursor_col && show_cursor {
                    let cursor_width = if ch == '\t' {
                        cell_width.min(visible_width.saturating_sub(rendered_width))
                    } else if animation_geometry.enabled() {
                        unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0)
                    } else {
                        0
                    };
                    animation_geometry.record_primary_cursor(screen_row, animation_col, cursor_width, style);
                }
                style = style.patch(crate::ui_colors::block_cursor_style(
                    colors.bg_cursor, colors.fg_cursor, style.bg.unwrap_or(line_bg), colors.ui.contrast_colors()));
            }

            if ch == '\t' {
                code_lines.flush(&mut spans, &mut cell_batch, cell_batch_style);
                let tab_width = cell_width.min(visible_width.saturating_sub(rendered_width));
                spans.push(Span::styled(crate::helpers::spaces(tab_width), style));
                animation_geometry.push(line_idx, absolute_col, ch, screen_row, animation_col, tab_width);
                animation_col += tab_width;
                rendered_width += tab_width;
            } else {
                // The bounded renderer historically puts each Unicode scalar
                // in its own span. Preserve those grapheme boundaries exactly;
                // ASCII runs can share one allocation and Paragraph span.
                if ch.is_ascii() && !reference_cell_paint() {
                    if style != cell_batch_style {
                        code_lines.flush(&mut spans, &mut cell_batch, cell_batch_style);
                        cell_batch_style = style;
                    }
                    cell_batch.push(ch);
                } else {
                    code_lines.flush(&mut spans, &mut cell_batch, cell_batch_style);
                    cell_batch.push(ch);
                    code_lines.flush(&mut spans, &mut cell_batch, style);
                }
                if animation_geometry.enabled() {
                    let width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                    animation_geometry.push(line_idx, absolute_col, ch, screen_row, animation_col, width);
                    animation_col += width;
                }
                rendered_width += cell_width;
            }
        }

        code_lines.flush(&mut spans, &mut cell_batch, cell_batch_style);

        // EOL is a stable source position even after the caret leaves it.
        // Otherwise the first move back into text looks like a layout remap.
        if slice_end == content_len && content_len >= h_scroll {
            animation_geometry.push(line_idx, content_len, '\0', screen_row, animation_col, 1);
        }
        let search_eol = line_matches.iter().find_map(|&(start, end, current)|
            (start <= content_len && end > content_len).then_some(current));
        if ((is_current_line && cursor_col == content_len && show_cursor) || search_eol.is_some())
            && content_len >= h_scroll
            && content_len < h_scroll.saturating_add(visible_width)
            && rendered_width < visible_width
        {
            let style = if is_current_line && cursor_col == content_len && show_cursor {
                animation_geometry.record_primary_cursor(screen_row, animation_col, 1, Style::default().bg(line_bg));
                crate::ui_colors::block_cursor_style(colors.bg_cursor, colors.fg_cursor, line_bg, colors.ui.contrast_colors())
            } else {
                colors.ui.find_style(Style::default(), search_eol == Some(true), colors.bg_search_match)
            };
            spans.push(Span::styled(
                " ",
                style,
            ));
            rendered_width += 1;
        }
        if rendered_width < visible_width {
            spans.push(Span::styled(
                crate::helpers::spaces(visible_width - rendered_width),
                Style::default().bg(line_bg),
            ));
        }
        paint_editor_row(f, &mut code_lines, &mut spans, merge_style, &colors);
    }

    gutter_lines.finish(f);
    code_lines.finish(f.buffer_mut());

    let change_rows = crate::ui_editor_minimap::ChangeRows::default();
    if minimap_area.width > 0 {
        draw_minimap_separator(f, minimap_sep_area, colors.ui.fg_overview_border, colors.bg_editor);
        draw_minimap(
            f,
            MinimapParams {
                area: minimap_area,
                editor,
                scroll,
                visible_lines,
                search_matches: &app.tabs[app.active_tab].file_search.search_matches,
                diagnostic_lines: &[],
                diagnostic_index: None,
                change_index: Some(&app.cached_editor_change_index),
                diff_changes: None,
                colors,
                hovered: app.hovered_minimap,
                active: app.dragging_minimap,
            },
        );
    }

    draw_scrollbar(
        f,
        ScrollbarParams {
            area: scrollbar_area,
            total_lines: editor.line_count(),
            visible_lines,
            scroll,
            search_matches: &app.tabs[app.active_tab].file_search.search_matches,
            diagnostic_lines: &[],
            diagnostic_index: None,
            change_rows: &change_rows,
            cursor_line,
            colors,
            hovered: app.hovered_scrollbar,
            active: app.dragging_v_scrollbar,
        },
    );

    if show_h_scrollbar {
        let h_area = Rect::new(code_area.x, h_scrollbar_area.y, code_area.width, 1);
        app.h_scrollbar_area = Some(h_area);
        draw_horizontal_scrollbar(
            f,
            HScrollbarParams {
                area: h_area,
                total_width: max_line_width,
                visible_width,
                scroll: h_scroll,
                bg_editor: colors.bg_editor,
                hovered: app.hovered_h_scrollbar,
                active: app.dragging_h_scrollbar,
                colors: &colors.ui,
            },
        );
    } else {
        app.h_scrollbar_area = None;
    }
    app.copy_animation.observe(&selection_style);
    crate::ui_animation_frame::finish_editor(f, app, animation_geometry, scope_rail_area, false);
}

pub(crate) fn draw_editor(f: &mut Frame, app: &mut App, area: Rect) {
    crate::editor_blame::draw(f, app, area);
}

pub(crate) fn draw_editor_content(f: &mut Frame, app: &mut App, area: Rect) {
    if crate::ui_animation_frame::begin_editor(f, app, area) {
        return;
    }
    if let Some(editor) = app.editor_mut() {
        editor.blame.inline_candidate = None;
        editor.blame.inline_annotation = None;
        editor.fold_buttons.clear();
    }
    // No file open — show welcome
    if app.tabs.is_empty() {
        let colors = app.colors;
        draw_welcome_screen(f, app, area, |f, rect| {
            apply_editor_gradients(
                f,
                rect,
                colors.bg_editor,
                colors.bg_line_highlight,
                colors.gradients.bg_editor,
                colors.gradients.bg_line_highlight,
            );
        });
        return;
    }

    if crate::extension_views::draw_active_view(f, app, area) {
        return;
    }

    let render_line_numbers = app.show_line_numbers
        || (app.diff_mode == crate::app::DiffMode::SideBySide
            && app.should_render_diff_for_active_editor());

    // Rebuild diagnostics only when a provider publishes a new revision. The
    // previous renderer cloned and sorted every diagnostic on every frame.
    refresh_cached_editor_diagnostics(app);
    let link_hover = app.lsp_link_hover_range();
    let editor = &app.tabs[app.active_tab].editor;
    // Gutter width
    let total_lines = editor.rope.len_lines();
    let gutter_width = if render_line_numbers {
        (digit_count(total_lines).max(GUTTER_MIN_DIGITS) + 1).min(GUTTER_MAX_WIDTH)
    } else {
        0
    };
    // A wrapped continuation can occupy every code column. Keep a blank
    // connector lane after fold/diff markers so its fallback rail stays whole.
    let scope_rail_width = u16::from(
        app.animation_settings
            .scope_enabled_for(editor),
    );

    // Whole-document width analysis is intentionally disabled in large-file
    // mode. Rope line lengths are constant-time, so derive an exact width for
    // only the visible rows (plus the cursor row) without scanning text.
    let first = editor.scroll_offset.min(editor.rope.len_lines());
    let end = first
        .saturating_add(area.height as usize)
        .min(editor.rope.len_lines());
    let visible_max_line_width = editor.render_line_cache.borrow_mut().visible_line_width(
        (editor.content_version, editor.wrap_revision, editor.tab_size),
        &editor.rope, first, end, editor.cursor_line,
    );
    // Whole-document analysis belongs to the active editor. Inactive panes
    // have their own visible-line cache, not the active file's width estimate.
    let max_line_width = if editor.large_file_mode || app.rendering_inactive_editor {
        visible_max_line_width
    } else {
        app.cached_max_line_width.max(visible_max_line_width)
    };
    // A minified/generated line should use the bounded renderer only while it
    // is actually visible. Keeping the whole document in that mode because of
    // one distant long line would unnecessarily disable rich editing features
    // on every ordinary line.
    let bounded_render_mode =
        editor.large_file_mode || visible_max_line_width > MAX_RICH_RENDER_LINE_CHARS;
    let rich_features_enabled = !bounded_render_mode;

    // Decide whether to show horizontal scrollbar
    let check_named_colors = rich_features_enabled
        && matches!(
            editor.language_name(),
            "CSS"
                | "SCSS"
                | "LESS"
                | "HTML"
                | "JavaScript"
                | "TypeScript"
                | "JSX"
                | "TSX"
                | "Vue"
                | "Svelte"
        );
    let effective_word_wrap = editor.word_wrap && rich_features_enabled;
    let minimap_est: u16 = if app.show_minimap && area.width > 80 {
        app.minimap_width + 1
    } else {
        0
    };
    let code_width_estimate = area
        .width
        .saturating_sub(gutter_width as u16 + 2 + scope_rail_width + minimap_est + 1);
    let show_h_scrollbar = max_line_width > code_width_estimate as usize && !effective_word_wrap;

    // Split vertically: editor content | optional horizontal scrollbar
    let v_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if show_h_scrollbar {
            vec![Constraint::Min(1), Constraint::Length(1)]
        } else {
            vec![Constraint::Min(1), Constraint::Length(0)]
        })
        .split(area);

    let editor_row_area = v_layout[0];
    let h_scrollbar_area = v_layout[1];

    // Split: gutter | code | minimap_separator | minimap | scrollbar
    let minimap_width: u16 = if app.show_minimap && editor_row_area.width > 80 {
        app.minimap_width
    } else {
        0
    };
    let minimap_sep_width: u16 = u16::from(minimap_width > 0);
    let scrollbar_width: u16 = if minimap_width > 0 { 1 } else { 2 };
    let editor_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(gutter_width as u16 + 2 + scope_rail_width),
            Constraint::Min(1),
            Constraint::Length(minimap_sep_width),
            Constraint::Length(minimap_width),
            Constraint::Length(scrollbar_width),
        ])
        .split(editor_row_area);

    let full_gutter_area = editor_layout[0];
    let gutter_area = Rect {
        width: full_gutter_area.width.saturating_sub(scope_rail_width),
        ..full_gutter_area
    };
    let scope_rail_area = Rect::new(
        gutter_area.right(),
        gutter_area.y,
        full_gutter_area.width.saturating_sub(gutter_area.width),
        gutter_area.height,
    );
    f.buffer_mut()
        .set_style(scope_rail_area, Style::default().bg(app.colors.bg_editor));
    let code_area = editor_layout[1];
    let minimap_sep_area = editor_layout[2];
    let minimap_area = editor_layout[3];
    let scrollbar_area = editor_layout[4];
    let animate = app.animation_settings.editor_effects_enabled() && app.terminal_has_focus && !app.rendering_inactive_editor
            && app.tabs[app.active_tab].editor.extra_cursors.is_empty()
            && !app.should_render_diff_for_active_editor();
    let mut animation_geometry = app.animation_frame_cache.geometry(code_area, animate);

    if rich_features_enabled && app.show_color_preview {
        animation_geometry.set_color_preview(check_named_colors);
    }

    app.gutter_width = gutter_width as u16 + 2 + scope_rail_width;
    app.gutter_area = Some(gutter_area);
    app.code_area = Some(code_area);
    app.minimap_area = if minimap_width > 0 {
        Some(minimap_area)
    } else {
        None
    };
    app.scrollbar_area = Some(scrollbar_area);

    // The interactive horizontal scrollbar only spans the code viewport, but
    // its layout row spans the full editor width. Paint the reserved gutter
    // and overview regions as well so the terminal's default background does
    // not show through as dark end caps.
    if show_h_scrollbar && h_scrollbar_area.height > 0 {
        let gutter_cap = Rect::new(
            gutter_area.x,
            h_scrollbar_area.y,
            gutter_area.width,
            h_scrollbar_area.height,
        );
        let buffer = f.buffer_mut();
        buffer.set_style(h_scrollbar_area, Style::default().bg(app.colors.bg_editor));
        buffer.set_style(gutter_cap, Style::default().bg(app.colors.bg_dark));
    }

    let active_diff_cache = app
        .should_render_diff_for_active_editor()
        .then(|| std::sync::Arc::clone(&app.diff_render_cache));
    let diff_changes = active_diff_cache
        .as_ref()
        // The bounded renderer's minimap uses the editor's Git source, so its
        // immutable index must be prepared from that same source.
        .filter(|_| !bounded_render_mode)
        .map(|cache| cache.target_changes.as_slice());
    let change_rows =
        cached_overview_change_rows(app, scrollbar_area.height as usize, diff_changes);
    let diag_lines = &app.cached_diag_lines;

    let cursor_match_target = app.cursor_match_target();
    let (matching_bracket_cached, matching_tag_cached) = if app
        .cached_cursor_match_target
        .as_ref()
        .zip(cursor_match_target.as_ref())
        .is_some_and(|(cached, current)| cached.same_render_anchor(current))
    {
        (app.cached_matching_bracket, app.cached_matching_tag)
    } else {
        (None, None)
    };

    // Update viewport dimensions
    let mut selection_style = app.copy_animation_selection_style();
    let crate::app::Tab { editor, file_search, .. } = &mut app.tabs[app.active_tab];
    editor.viewport_height = code_area.height as usize;
    editor.viewport_width = code_area.width as usize;
    editor.restore_reopen_wrap_scroll();
    editor.scroll_past_end = app.scroll_past_end;
    // Defensive clamp: ensure cursor is within rope bounds before rendering
    editor.clamp_cursor();

    if bounded_render_mode {
        if editor.blame.enabled {
            editor.blame.rows = (0..code_area.height as usize).map(|row| {
                let line_idx = editor.scroll_offset.saturating_add(row);
                (line_idx < editor.rope.len_lines()).then_some(crate::editor::WrappedRowInfo {
                    line_idx, start_col: 0, end_col: 0, line_len: 0, is_first: true,
                })
            }).collect();
        }
        draw_large_file_editor(
            f,
            app,
            LargeFileEditorLayout {
                gutter_area,
                scope_rail_area,
                code_area,
                minimap_sep_area,
                minimap_area,
                scrollbar_area,
                h_scrollbar_area,
                render_line_numbers,
                gutter_width,
                max_line_width,
                show_h_scrollbar,
            },
        );
        return;
    }

    let scroll = editor.scroll_offset;
    let cursor_line = editor.cursor_line;
    let cursor_col = editor.cursor_col;
    let h_scroll = editor.horizontal_scroll;
    let visible_lines = code_area.height as usize;
    let file_path = editor.syntax_file_path().map(std::path::Path::to_path_buf);
    let syntax_language = editor.syntax_language_token().map(str::to_string);
    let tab_size = editor.tab_size;

    // The closest guide sits in the final indentation block, including tabs.
    let cursor_indent = if app.show_indent_guides && tab_size > 0 {
        let line = editor.rope.line(cursor_line);
        // Rich decorations are skipped on pathological lines; do not scan a
        // megabyte of indentation merely to choose an offscreen active guide.
        if line.len_chars() > MAX_RICH_RENDER_LINE_CHARS {
            None
        } else {
            let mut width = 0usize;
            for ch in line.chars().take_while(|ch| matches!(ch, ' ' | '\t')) {
                width += if ch == '\t' { tab_size - width % tab_size } else { 1 };
            }
            (width / tab_size).checked_sub(1).map(|level| level * tab_size)
        }
    } else { None };

    // Cursor color: brighter for overwrite mode
    let overwrite = editor.overwrite_mode;
    let cursor_bg = if overwrite {
        app.colors.fg_overwrite_cursor
    } else {
        app.colors.bg_cursor
    };
    let cursor_fg = app.colors.fg_cursor;
    let cursor_style = |surface| crate::ui_colors::block_cursor_style(cursor_bg, cursor_fg, surface, app.colors.ui.contrast_colors());
    let show_cursors = (app.cursor_visible
        || (app.terminal_has_focus
            && app.animations.cursor_smear_active(
                editor.navigation_id,
                &app.animation_settings,
                std::time::Instant::now(),
            )))
        && !app.rendering_inactive_editor
        && !app.status_details.open;
    let show_inactive_cursor = app.rendering_inactive_editor;
    let mut semantic_color_cache = FxHashMap::default();
    let mut folded_rows = Vec::new();

    if effective_word_wrap {
        editor.horizontal_scroll = 0;
        editor.set_wrap_scroll_offset(editor.wrap_scroll_offset);
        let wrap_width = editor.effective_wrap_width();
        let wrapped_rows =
            editor.wrapped_rows(editor.wrap_scroll_offset, visible_lines, wrap_width);
        let exact_wrap_layout = editor.wrap_layout_ready(wrap_width);
        if exact_wrap_layout {
            animation_geometry.set_wrapped(wrap_width);
        }
        if editor.blame.enabled { editor.blame.rows = wrapped_rows.iter().copied().map(Some).collect(); }
        let cursor_wrap_row = editor.wrapped_row_for_position(cursor_line, cursor_col, wrap_width);

        let gutter_bg = app.colors.bg_dark;
        let gutter_hl_bg = app.colors.bg_line_highlight;
        let gutter_fg = app.colors.fg_text;
        let gutter_dim = app.colors.ui.fg_line_number;
        let file_diagnostics = std::sync::Arc::clone(&app.cached_editor_diagnostics);
        let diag_indices_by_line = std::sync::Arc::clone(&app.cached_diag_indices_by_line);
        let wide_diag_indices = std::sync::Arc::clone(&app.cached_wide_diag_indices);
        let mut gutter_lines = gutter::Rows::new(f, gutter_area, gutter_bg);
        let mut gutter_num_buf = String::with_capacity(gutter_width + 2);

        for row in &wrapped_rows {
            let line_idx = row.line_idx;
            let cursor_in_row = line_idx == cursor_line
                && cursor_col >= row.start_col
                && (cursor_col < row.end_col
                    || (cursor_col == row.end_col && row.end_col == row.line_len));
            let bg = if cursor_in_row {
                gutter_hl_bg
            } else {
                gutter_bg
            };

            let fold_char = if row.is_first && editor.is_fold_start(line_idx) {
                "+"
            } else if row.is_first && editor.can_fold_line(line_idx) {
                "▾"
            } else {
                " "
            };

            let diff_status = if row.is_first {
                editor.line_diff_status(line_idx)
            } else {
                ' '
            };
            let gutter_hovered = app.hovered_gutter_line == Some(line_idx);

            // Check for diagnostics on this line (binary search on sorted diagnostics)
            let lsp_diag = if row.is_first {
                let idx = file_diagnostics.partition_point(|d| d.line < line_idx);
                file_diagnostics.get(idx).filter(|d| d.line == line_idx)
            } else {
                None
            };

            let (diff_char, diff_style) = if let Some(diag) = lsp_diag {
                let icon = crate::lsp::severity_icon(diag.severity);
                let color = app.colors.ui.diagnostic(diag.severity);
                (icon, Style::default().fg(color).bg(bg))
            } else {
                gutter_diff_indicator(diff_status, bg, gutter_hovered, &app.colors)
            };

            let line_num_display = if render_line_numbers {
                if row.is_first {
                    format_gutter_line_number(
                        &mut gutter_num_buf,
                        render_line_numbers,
                        app.relative_line_numbers,
                        line_idx,
                        cursor_line,
                        gutter_width,
                    );
                    gutter_num_buf.as_str()
                } else {
                    // Show wrap continuation indicator
                    gutter_num_buf.clear();
                    if gutter_width > 1 {
                        for _ in 0..gutter_width - 1 {
                            gutter_num_buf.push(' ');
                        }
                    }
                    gutter_num_buf.push('↩');
                    gutter_num_buf.as_str()
                }
            } else {
                ""
            };
            let num_style = if cursor_in_row {
                Style::default()
                    .fg(gutter_fg)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(gutter_dim).bg(bg)
            };
            let fold_col = gutter_area.x + gutter_area.width.saturating_sub(1);
            let gutter_row = gutter_area.y + (len_as_u16(gutter_lines.len()));
            let is_fold_hovered =
                fold_char != " " && app.mouse_row == gutter_row && app.mouse_col == fold_col;
            let fold_style = crate::ui_editor_folds::gutter_style(&app.colors, bg, fold_char == "+", is_fold_hovered);

            gutter_lines.push(f, [
                Span::styled(diff_char, diff_style),
                Span::styled(line_num_display, num_style),
                Span::styled(fold_char, fold_style),
            ]);
        }

        format_empty_gutter_row(&mut gutter_num_buf, render_line_numbers, gutter_width);
        let tilde_style = Style::default().fg(app.colors.fg_tilde).bg(gutter_bg);
        let tilde_count = visible_lines.saturating_sub(wrapped_rows.len());
        for _ in 0..tilde_count {
            gutter_lines.push(f, [Span::styled(gutter_num_buf.as_str(), tilde_style)]);
        }
        gutter_lines.finish(f);

        let editor_bg = app.colors.bg_editor;
        let has_selection = editor.has_selection();
        let selection_range = editor.selection_range();
        let matching_bracket = matching_bracket_cached;
        let matching_tag = matching_tag_cached;
        let search_matches = &file_search.search_matches;
        let search_current = file_search.search_current;
        let cursor_word_owned: Option<std::sync::Arc<str>>;
        let cursor_word: Option<&str> = if file_search.search_active {
            None
        } else if has_selection {
            cursor_word_owned = cached_selection_occurrence_text(editor, &mut app.editor_selection_word_cache);
            cursor_word_owned.as_deref()
        } else {
            // Use cached word_at_cursor to avoid recomputing every frame
            refresh_cursor_word(
                editor,
                &mut app.cached_cursor_word_key,
                &mut app.cached_cursor_word,
                app.editor_cursor_word_cache.get_mut(),
            );
            app.cached_cursor_word.as_deref()
        };

        let mut code_lines = prepared_text::Rows::new(code_area, Style::default().bg(app.colors.bg_editor).remove_modifier(Modifier::DIM));
        let wrap_spaces_pool: &str = &crate::helpers::spaces(wrap_width);
        let mut line_text_buf = RenderLineText::Borrowed("");
        let mut word_occurrences = None;
        let cursor_word_char_len = cursor_word.map_or(0, |w| {
            if w.is_ascii() {
                w.len()
            } else {
                w.chars().count()
            }
        });
        if let Some(word) = cursor_word {
            editor.render_line_cache.borrow_mut().prepare_occurrences(
                (
                    editor.content_version,
                    editor.wrap_revision,
                    editor.tab_size,
                ),
                &editor.rope,
                word,
                cursor_word_char_len,
                !has_selection,
            );
        }
        let first_line = wrapped_rows.first().map_or(0, |row| row.line_idx);
        // Keep the previous same-editor viewport analysis visible while a new
        // content version is being prepared. Rejecting it solely by version
        // made bracket colors/guides blink off for one frame on every edit.
        let bracket_cache_matches =
            app.cached_bracket_analysis_target
                .as_ref()
                .is_some_and(|target| {
                    target.first_line == first_line
                        && target.navigation_id == editor.navigation_id
                        && target.editor.active_group == app.active_group
                        && target.editor.active_tab == app.active_tab
                        && target.editor.file_path == editor.file_path
                });
        let mut bracket_depth = if bracket_cache_matches {
            app.cached_bracket_starting_depth
        } else {
            0
        };
        let bracket_depth_index = app
            .cached_bracket_depth_index
            .as_ref()
            .filter(|index| index.matches(editor))
            .cloned();
        let colorize_brackets = app.bracket_pair_colorization
            && bracket_depth_index
                .as_ref()
                .is_none_or(|index| index.enabled());
        let mut bracket_depth_line: Option<usize> = None;
        let mut bracket_colors = None;
        let mut line_spans: Vec<Span> = Vec::with_capacity(32);
        let mut wrap_batch_buf = String::with_capacity(64);
        let mut line_matches_buf: Vec<(usize, usize, bool)> = Vec::with_capacity(8);
        let mut line_match_runs = Vec::new();
        let mut line_highlight_columns = Vec::new();
        let mut line_diagnostic_columns = Vec::new();
        let mut line_diagnostic_runs = Vec::new();
        let mut line_highlights_buf: Vec<(u32, u32, u32, u32)> = Vec::with_capacity(16);
        let mut line_diags_buf: Vec<usize> = Vec::with_capacity(4);
        let extra_cursor_viewport =
            ExtraCursorViewport::new(editor, wrapped_rows.iter().map(|row| row.line_idx));
        let mut extra_cursor_scratch = ExtraCursorLine::default();
        let mut cached_hl_wrap = None;
        let mut wrapped_glyphs = None;
        let mut comment_ranges = None;
        let mut inline_colors = None;
        let mut logical_line_char_count = 0;
        let mut logical_paint_end = 0;
        let mut line_semantic_wrap: &[crate::lsp::DecodedSemanticToken] = &[];
        let mut sem_tok_idx = 0;
        let mut sem_color_idx = usize::MAX;
        let mut sem_color = None;
        let mut source_lines = source_lines::SourceLines::new(&editor.rope, scroll);
        for (row_index, row) in wrapped_rows.iter().enumerate() {
            let line_idx = row.line_idx;
            animation_geometry.map_scope_row(line_idx, row.start_col, code_lines.len());
            let new_logical_line = bracket_depth_line != Some(line_idx);
            if new_logical_line {
                let rope_line = source_lines.get(line_idx);
                logical_line_char_count = rope_line.len_chars();
                line_text_buf = editor.render_line_cache.borrow_mut().line_text(
                    line_idx,
                    (
                        editor.content_version,
                        editor.wrap_revision,
                        editor.tab_size,
                    ),
                    &editor.rope,
                    rope_line,
                );
            }
            let clean_line = line_text_buf.trim_end_matches(['\n', '\r']);
            let clean_line_char_count = logical_line_char_count - (line_text_buf.len() - clean_line.len());
            if new_logical_line {
                bracket_depth_line = Some(line_idx);
                logical_paint_end = if !exact_wrap_layout && !reference_pending_wrap_paint() {
                    pending_wrap_paint_end(clean_line, code_area.width as usize, tab_size)
                } else {
                    clean_line_char_count
                };
                #[cfg(test)]
                if !exact_wrap_layout {
                    PENDING_WRAP_WORK.with(|slot| {
                        let mut work = slot.get();
                        work.scanned += logical_paint_end;
                        slot.set(work);
                    });
                }
            }
            let paint_end = row.end_col.min(logical_paint_end);
            let source_end_prepared = paint_end == row.end_col;

            if new_logical_line {
                cached_hl_wrap = app.highlighter.render_line(
                    &line_text_buf, line_idx, file_path.as_deref(), syntax_language.as_deref(),
                    app.syntax_theme.as_str(), editor.content_version,
                );
                comment_ranges = app.comment_whitespace_mode.and_then(|_| {
                    cached_hl_wrap.as_ref().map(|line| std::rc::Rc::clone(&line.comments))
                });
            }
            let highlighted = HighlightedLine::new(
                cached_hl_wrap
                    .as_ref()
                    .map(|line| (line.spans.as_slice(), line.char_len)),
                clean_line,
                clean_line_char_count,
                plain_theme_style(app.colors.fg_text),
            )
            .with_text_snapshot(&line_text_buf);
            if new_logical_line {
                let decode_end = if editor.render_line_cache.borrow().viewport_start(1) == 0 {
                    clean_line_char_count
                } else {
                    let last_visible = if reference_row_preparation() {
                        wrapped_rows.iter().rev().find(|visible| visible.line_idx == line_idx).unwrap()
                    } else {
                        // Continuations are adjacent: locate their end without
                        // rescanning every later logical line in this viewport.
                        let count = wrapped_rows[row_index..]
                            .partition_point(|visible| visible.line_idx == line_idx);
                        &wrapped_rows[row_index + count - 1]
                    };
                    last_visible.end_col.min(logical_paint_end).saturating_add(1)
                };
                #[cfg(test)]
                if !exact_wrap_layout {
                    PENDING_WRAP_WORK.with(|slot| {
                        let mut work = slot.get();
                        work.requested += decode_end.min(clean_line_char_count);
                        slot.set(work);
                    });
                }
                wrapped_glyphs = Some(
                    editor.render_line_cache.borrow_mut().line(
                        line_idx,
                        (
                            editor.content_version,
                            editor.wrap_revision,
                            editor.tab_size,
                        ),
                        &editor.rope,
                        &highlighted,
                        cached_hl_wrap
                            .as_ref()
                            .map(|line| (&line.spans, line.lengths.as_slice())),
                        decode_end,
                        LineWhitespace::needs_boundary(editor.whitespace_mode, app.comment_whitespace_mode),
                    ),
                );
                let line_start = bracket_depth_index
                    .as_ref()
                    .and_then(|index| index.line_starting_depth(line_idx))
                    .unwrap_or(bracket_depth);
                bracket_colors = (colorize_brackets
                    && logical_line_char_count <= crate::background::MAX_BRACKET_LINE_CHARS)
                    .then(|| {
                        editor
                            .render_line_cache
                            .borrow_mut()
                            .brackets(line_idx, clean_line, line_start)
                    });
                bracket_depth = bracket_colors
                    .as_ref()
                    .map_or(line_start, BracketLineColors::end_depth);
            }
            let glyphs = wrapped_glyphs.as_ref().unwrap();
            let wrapped_chars = &glyphs.chars;
            let wrapped_span_ends = &glyphs.span_ends;
            let _cursor_in_row = line_idx == cursor_line
                && cursor_col >= row.start_col
                && (cursor_col < row.end_col
                    || (cursor_col == row.end_col && row.end_col == row.line_len));
            let is_current_line = line_idx == editor.cursor_line;
            let extra_cursor_line =
                extra_cursor_viewport.line(editor, line_idx, &mut extra_cursor_scratch);
            let is_extra_cursor_line = !extra_cursor_line.columns.is_empty();
            let show_line_hl = (is_current_line || is_extra_cursor_line)
                && !{
                    if let Some((sl, _, el, _)) = editor.selection_range() {
                        sl != el
                    } else {
                        false
                    }
                };
            let line_bg = if show_line_hl {
                app.colors.bg_line_highlight
            } else if editor.is_fold_start(line_idx) {
                app.colors.bg_fold_indicator
            } else {
                editor_bg
            };
            let line_bg = crate::merge_conflict_ui::content_background(editor, line_idx, &app.colors, line_bg);
            let merge_style = app
                .colors
                .ui
                .merge_marker_style(clean_line.chars(), line_bg);
            let line_bg = merge_style.and_then(|style| style.bg).unwrap_or(line_bg);
            if new_logical_line {
                matches_for_line(
                    search_matches,
                    &editor.rope,
                    line_idx,
                    search_current,
                    &mut line_matches_buf,
                );
                flatten_priority_ranges(&line_matches_buf, &mut line_match_runs);
                word_occurrences = cursor_word.map(|_| {
                    editor
                        .render_line_cache
                        .borrow_mut()
                        .occurrences(line_idx, clean_line)
                });
            }

            // Pre-compute main selection column range for this line
            let main_sel_cols: Option<(usize, usize)> = if has_selection {
                if let Some((sl, sc, el, ec)) = selection_range {
                    if line_idx >= sl && line_idx <= el {
                        let start_col = if line_idx == sl { sc } else { 0 };
                        let end_col = if line_idx == el { ec } else { usize::MAX };
                        Some((start_col, end_col))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };

            let line_u32_hl = line_idx as u32;
            if new_logical_line {
                // Pre-filter document highlights for this line (reuse scratch buffer)
                document_highlights_for_line(
                    &app.lsp_document_highlights,
                    &app.lsp_document_multiline_highlights,
                    line_u32_hl,
                    &mut line_highlights_buf,
                );
                highlight_column_ranges(&line_highlights_buf, line_u32_hl, &mut line_highlight_columns);

                // Diagnostics are indexed when providers publish a new revision;
                // rendering only visits matches for this visible line.
                diagnostic_indices_for_line(
                    &file_diagnostics,
                    &diag_indices_by_line,
                    &wide_diag_indices,
                    line_idx,
                    &mut line_diags_buf,
                );
                diagnostic_column_ranges(&file_diagnostics, &line_diags_buf, line_idx, &app.colors,
                    &mut line_diagnostic_columns, &mut line_diagnostic_runs);

                // Detect inline color values (#RGB, #RRGGBB)
                inline_colors = app.show_color_preview.then(|| {
                    editor.render_line_cache.borrow_mut().inline_colors(
                        line_idx,
                        clean_line,
                        check_named_colors,
                    )
                });
            }

            // Continuations visit increasing columns of the same source line.
            // Retain its provider slice and token cursor instead of restarting
            // the scan from column zero on each terminal row.
            if new_logical_line || reference_row_preparation() {
                line_semantic_wrap = if editor.blame.revision.is_none() && app.show_semantic_tokens && !app.lsp_semantic_tokens.is_empty() {
                    let line_u32 = line_idx as u32;
                    let start = app
                        .lsp_semantic_tokens
                        .partition_point(|t| t.line < line_u32);
                    let end = app
                        .lsp_semantic_tokens
                        .partition_point(|t| t.line <= line_u32);
                    &app.lsp_semantic_tokens[start..end]
                } else {
                    &[]
                };
                sem_tok_idx = 0;
                sem_color_idx = usize::MAX;
                sem_color = None;
            }

            line_spans.clear();
            let mut abs_col = row.start_col.min(wrapped_chars.len());
            let mut animation_col = 0usize;
            let mut wrap_visual_col = glyphs.visual_column(abs_col);
            let indent_columns = glyphs.visual_column(glyphs.leading_whitespace);
            let ts = editor.tab_size;
            let mut prev_ch = abs_col.checked_sub(1).map(|col| wrapped_chars[col]);
            let mut whitespace = LineWhitespace::new(
                editor.whitespace_mode, app.comment_whitespace_mode,
                comment_ranges.as_deref().map_or(&[], Vec::as_slice),
                glyphs.whitespace, abs_col, line_idx == cursor_line,
            );
            wrap_batch_buf.clear();
            let mut wrap_batch_style = Style::default();
            let mut occurrences = word_occurrences
                .as_deref()
                .map(|ranges| WordOccurrenceCursor::new(ranges, abs_col));
            let mut search_runs = ValueRangeCursor::new(&line_match_runs, abs_col);
            let mut document_occurrences = OccurrenceCursor::new(&line_highlight_columns, abs_col);
            let mut extra_selections =
                OccurrenceCursor::new(&extra_cursor_line.selections, abs_col);
            let mut extra_cursor_cols =
                CursorColumnCursor::new(&extra_cursor_line.columns, abs_col);
            let mut bracket_depths = bracket_colors.as_ref().map(|line| line.cursor(abs_col));
            let mut diagnostic_runs = ValueRangeCursor::new(&line_diagnostic_runs, abs_col);
            let mut color_previews = InlineColorCursor::new(
                inline_colors.as_deref().map_or(&[], Vec::as_slice),
                abs_col,
            );
            let first_span = wrapped_span_ends.partition_point(|&end| end <= abs_col);
            for ((style, _), &span_end) in highlighted
                .iter_from_span(first_span)
                .zip(&wrapped_span_ends[first_span..])
            {
                if abs_col >= paint_end {
                    break;
                }
                let syntax_style = Style::default()
                    .fg(crate::ui_colors::syntax_color(style.foreground, line_bg))
                    .bg(line_bg)
                    .add_modifier(syntax_font_modifiers(style.font_style));
                for ch in wrapped_chars[abs_col..span_end.min(paint_end)]
                    .iter()
                    .copied()
                {
                    // Inject inline color swatch before #color value
                    if let Some(color) = color_previews.at(abs_col) {
                        if !wrap_batch_buf.is_empty() {
                            code_lines.flush(&mut line_spans, &mut wrap_batch_buf, wrap_batch_style);
                        }
                        line_spans.push(Span::styled("▉", Style::default().fg(color).bg(line_bg)));
                        animation_col += 1;
                        wrap_batch_style = Style::default();
                    }
                    let mut s = syntax_style;
                    // Semantic token color override (LSP semantic highlighting) - wrap mode
                    if !line_semantic_wrap.is_empty() {
                        let col_u32 = abs_col as u32;
                        // Advance past tokens that end before current column
                        while sem_tok_idx < line_semantic_wrap.len() {
                            let st = &line_semantic_wrap[sem_tok_idx];
                            if st.start_char + st.length <= col_u32 {
                                sem_tok_idx += 1;
                            } else {
                                break;
                            }
                        }
                        if sem_tok_idx < line_semantic_wrap.len() {
                            let st = &line_semantic_wrap[sem_tok_idx];
                            if col_u32 >= st.start_char && col_u32 < st.start_char + st.length {
                                if sem_color_idx != sem_tok_idx {
                                    sem_color = editor_semantic_color_for_token(st,
                                        &app.lsp_manager, editor, app.semantic_theme_colors.as_ref(),
                                        &mut semantic_color_cache);
                                    sem_color_idx = sem_tok_idx;
                                }
                                if let Some(color) = sem_color {
                                    s = s.patch(color);
                                }
                            }
                        }
                    }
                    if app.bracket_pair_colorization
                        && matches!(ch, '(' | ')' | '[' | ']' | '{' | '}')
                    {
                        if let Some(depth) = bracket_depths
                            .as_mut()
                            .and_then(|cursor| cursor.at(abs_col))
                            && let Some(color) = app.colors.ui.bracket_color(depth as usize)
                        {
                            s = s.fg(color);
                        }
                    }
                    if let Some(is_current) = search_runs.at(abs_col) {
                        s = app.colors.ui.find_style(s, is_current, app.colors.bg_search_match);
                    }
                    // LSP document highlight (semantic symbol occurrences)
                    if document_occurrences.contains(abs_col) {
                        s = app.colors.ui.occurrence_style(s, app.colors.bg_word_highlight, false);
                    }
                    if occurrences
                        .as_mut()
                        .is_some_and(|cursor| cursor.contains(abs_col))
                    {
                        s = app.colors.ui.occurrence_style(
                            s,
                            app.colors.bg_word_highlight,
                            has_selection,
                        );
                    }
                    let cell_selected = main_sel_cols
                        .is_some_and(|(sel_sc, sel_ec)| abs_col >= sel_sc && abs_col < sel_ec)
                        || extra_selections.contains(abs_col);
                    if let Some((ml, mc)) = matching_bracket {
                        if line_idx == ml && abs_col == mc {
                            s = app.colors.ui.bracket_style(s, app.colors.bg_bracket_match);
                        }
                    }
                    if line_idx == cursor_line
                        && abs_col == cursor_col
                        && matching_bracket.is_some()
                    {
                        s = app.colors.ui.bracket_style(s, app.colors.bg_bracket_match);
                    }
                    // Matching tag highlight
                    if let Some((t1l, t1cs, t1ce, t2l, t2cs, t2ce)) = matching_tag {
                        if (line_idx == t1l && abs_col >= t1cs && abs_col < t1ce)
                            || (line_idx == t2l && abs_col >= t2cs && abs_col < t2ce)
                        {
                            s = app.colors.ui.bracket_style(s, app.colors.bg_bracket_match);
                        }
                    }
                    // Diagnostic underlines (pre-filtered per line)
                    if let Some(color) = diagnostic_runs.at(abs_col) {
                        s = s.add_modifier(Modifier::UNDERLINED).underline_color(color);
                    }
                    if link_hover.is_some_and(|(line, start, end)| line_idx == line && abs_col >= start && abs_col < end) {
                        s = s.add_modifier(Modifier::UNDERLINED);
                    }
                    let indent_depth = app.show_indent_guides.then(||
                        indent_guide_depth(wrap_visual_col, indent_columns, ts)).flatten();
                    let guide_color = if let Some(depth) = indent_depth {
                        Some(app.colors.ui.indent_guide_color(depth, cursor_indent == Some(wrap_visual_col)))
                    } else if ch == ' ' {
                        bracket_cache_matches.then(|| app.cached_bracket_guides.at(line_idx, abs_col)).flatten().map(|g| {
                            let index = (g.2 as usize).wrapping_sub(1) % 6;
                            if editor.cursor_line >= g.0 && editor.cursor_line < g.1 { app.colors.ui.bracket_guides_active[index] }
                            else { app.colors.ui.bracket_guides[index] }
                        })
                    } else { None };
                    let show_ws = whitespace.shows(ch, abs_col,
                        ch == ' ' && (prev_ch == Some(' ') || wrapped_chars.get(abs_col + 1) == Some(&' ')), cell_selected);
                    if let Some(color) = guide_color {
                        s = guide_style(s, color, app.colors.ui.fg_whitespace, show_ws);
                    }
                    // Selection must remain visible over text decorations.
                    if cell_selected {
                        s = selection_style.apply(s);
                    }
                    if show_cursors
                        && ((line_idx == cursor_line && abs_col == cursor_col)
                            || extra_cursor_cols.contains(abs_col))
                    {
                        if line_idx == cursor_line && abs_col == cursor_col {
                            let cursor_width = if ch == '\t' {
                                ts.saturating_sub(wrap_visual_col % ts.max(1)).max(1)
                            } else if animation_geometry.enabled() {
                                unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0)
                            } else {
                                0
                            };
                            animation_geometry.record_primary_cursor(code_lines.len(), animation_col, cursor_width, s);
                        }
                        s = s.patch(cursor_style(s.bg.unwrap_or(line_bg)));
                    } else if show_inactive_cursor
                        && line_idx == cursor_line
                        && abs_col == cursor_col
                    {
                        s = s.add_modifier(ratatui::style::Modifier::UNDERLINED);
                    }
                    if animation_geometry.enabled() {
                        let animation_width = if ch == '\t' {
                            ts.saturating_sub(wrap_visual_col % ts.max(1)).max(1)
                        } else { unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) };
                        animation_geometry.push(line_idx, abs_col, ch, code_lines.len(), animation_col, animation_width);
                        animation_col += animation_width;
                    }
                    if guide_color.is_some() {
                        if !wrap_batch_buf.is_empty() {
                            code_lines.flush(&mut line_spans, &mut wrap_batch_buf, wrap_batch_style);
                        }
                        line_spans.push(Span::styled(guide_glyph(ch, show_ws), s));
                        if ch == '\t' {
                            push_cell_padding(&mut line_spans, ts.saturating_sub(wrap_visual_col % ts.max(1)).max(1) - 1, s);
                        }
                        wrap_batch_style = s;
                    } else if ch == '\t' {
                        // Tab: expand to tab_size-aligned spaces
                        let tab_w = ts.saturating_sub(wrap_visual_col % ts).max(1);
                        if !wrap_batch_buf.is_empty() {
                            code_lines.flush(&mut line_spans, &mut wrap_batch_buf, wrap_batch_style);
                        }
                        if show_ws {
                            let ns = s.fg(app.colors.ui.fg_whitespace).remove_modifier(Modifier::DIM);
                            line_spans.push(Span::styled(marked_tab(tab_w), ns));
                            wrap_batch_style = ns;
                        } else {
                            line_spans.push(Span::styled(cell_spaces(tab_w), s));
                            wrap_batch_style = s;
                        }
                    } else {
                        if show_ws {
                            let ws_ch = "·";
                            if !wrap_batch_buf.is_empty() {
                                code_lines.flush(&mut line_spans, &mut wrap_batch_buf, wrap_batch_style);
                            }
                            line_spans.push(Span::styled(
                                ws_ch,
                                s.fg(app.colors.ui.fg_whitespace).remove_modifier(Modifier::DIM),
                            ));
                            wrap_batch_style =
                                s.fg(app.colors.ui.fg_whitespace).remove_modifier(Modifier::DIM);
                        } else if s == wrap_batch_style && !wrap_batch_buf.is_empty() {
                            wrap_batch_buf.push(ch);
                        } else {
                            if !wrap_batch_buf.is_empty() {
                                code_lines.flush(&mut line_spans, &mut wrap_batch_buf, wrap_batch_style);
                            }
                            wrap_batch_buf.push(ch);
                            wrap_batch_style = s;
                        }
                    }
                    prev_ch = Some(ch);
                    if ch == '\t' {
                        wrap_visual_col += ts.saturating_sub(wrap_visual_col % ts).max(1);
                    } else {
                        wrap_visual_col += 1;
                    }
                    abs_col += 1;
                }
                if abs_col >= paint_end {
                    break;
                }
            }
            // Flush remaining wrap batch buffer
            if !wrap_batch_buf.is_empty() {
                code_lines.flush(&mut line_spans, &mut wrap_batch_buf, wrap_batch_style);
            }

            // Track rendered column count for cursor/indicator positioning
            let mut rendered_cols: usize = abs_col.saturating_sub(row.start_col);
            #[cfg(test)]
            if !exact_wrap_layout {
                PENDING_WRAP_WORK.with(|slot| {
                    let mut work = slot.get();
                    work.styled += rendered_cols;
                    work.spans += line_spans.len();
                    work.pending_rows += 1;
                    slot.set(work);
                });
            }

            if source_end_prepared && row.end_col == row.line_len {
                animation_geometry.push(
                    line_idx,
                    row.line_len,
                    '\0',
                    code_lines.len(),
                    animation_col,
                    1,
                );
            }
            let paints_line_end = source_end_prepared && row.end_col == row.line_len;
            let search_eol = (paints_line_end || reference_row_preparation()).then(||
                line_matches_buf.iter().find_map(|&(start, end, current)|
                    (start <= row.line_len && end > row.line_len).then_some(current))
            ).flatten();
            let cursor_at_end = line_idx == cursor_line && cursor_col == row.end_col;
            if ((show_cursors && cursor_at_end) || search_eol.is_some())
                && source_end_prepared
                && row.end_col == row.line_len
                && row.end_col >= row.start_col
            {
                while rendered_cols < row.end_col.saturating_sub(row.start_col) {
                    line_spans.push(Span::styled(" ", Style::default().bg(line_bg)));
                    rendered_cols += 1;
                }
                if show_cursors && cursor_at_end {
                    animation_geometry.record_primary_cursor(code_lines.len(), animation_col.max(rendered_cols), 1, Style::default().bg(line_bg));
                }
                line_spans.push(Span::styled(
                    " ",
                    if show_cursors && cursor_at_end { cursor_style(line_bg) }
                    else { app.colors.ui.find_style(Style::default(), search_eol == Some(true), app.colors.bg_search_match) },
                ));
                animation_col = animation_col.max(rendered_cols).saturating_add(1);
            } else if show_inactive_cursor
                && source_end_prepared
                && line_idx == cursor_line
                && cursor_col == row.end_col
                && row.end_col == row.line_len
                && row.end_col >= row.start_col
            {
                while rendered_cols < cursor_col.saturating_sub(row.start_col) {
                    line_spans.push(Span::styled(" ", Style::default().bg(line_bg)));
                    rendered_cols += 1;
                }
                line_spans.push(Span::styled(
                    " ",
                    Style::default()
                        .bg(line_bg)
                        .add_modifier(ratatui::style::Modifier::UNDERLINED),
                ));
            }
            if show_cursors && source_end_prepared && (paints_line_end || reference_row_preparation()) {
                for &ec_col in &extra_cursor_line.columns {
                    if ec_col == row.end_col
                        && row.end_col == row.line_len
                        && row.end_col >= row.start_col
                    {
                        while rendered_cols < ec_col.saturating_sub(row.start_col) {
                            line_spans.push(Span::styled(" ", Style::default().bg(line_bg)));
                            rendered_cols += 1;
                        }
                        line_spans.push(Span::styled(
                            " ",
                            cursor_style(line_bg),
                        ));
                        animation_geometry.push(line_idx, ec_col, '\0', code_lines.len(), animation_col.max(rendered_cols), 1);
                        animation_col = animation_col.max(rendered_cols).saturating_add(1);
                    }
                }
            }
            if (row.is_first || row_index == 0) && editor.is_fold_start(line_idx) {
                folded_rows.push(crate::ui_editor_folds::Row {
                    line: line_idx, screen_row: row_index,
                    content_width: line_spans.iter().map(|span| span.content.width()).sum(),
                    start_col: row.start_col, end_col: row.end_col,
                });
            }
            // Show wrap indicator "↩" at the end of non-last wrapped segments
            if row.end_col < row.line_len && rendered_cols < wrap_width {
                let gap = wrap_width - rendered_cols - 1;
                if gap > 0 {
                    line_spans.push(Span::styled(
                        &wrap_spaces_pool[..gap],
                        Style::default().bg(line_bg),
                    ));
                }
                line_spans.push(Span::styled(
                    "↩",
                    Style::default().fg(app.colors.fg_dim).bg(line_bg),
                ));
            }
            if line_spans.len() < wrap_width {
                let pad = wrap_width - line_spans.len();
                #[cfg(test)]
                if !exact_wrap_layout {
                    PENDING_WRAP_WORK.with(|slot| {
                        let mut work = slot.get();
                        work.padding += pad;
                        slot.set(work);
                    });
                }
                line_spans.push(Span::styled(
                    &wrap_spaces_pool[..pad],
                    Style::default().bg(line_bg),
                ));
            } else if line_spans.len() > wrap_width {
                line_spans.truncate(wrap_width);
            }
            paint_editor_row(f, &mut code_lines, &mut line_spans, merge_style, &app.colors);
        }
        code_lines.finish(f.buffer_mut());
        apply_editor_gradients(
            f,
            code_area,
            editor_bg,
            app.colors.bg_line_highlight,
            app.colors.gradients.bg_editor,
            app.colors.gradients.bg_line_highlight,
        );

        // Text drag-and-drop: show insertion indicator at mouse position (wrap mode)
        if app.dragging_text {
            draw_text_drag_indicator(
                f,
                code_area,
                app.mouse_row,
                app.mouse_col,
                app.colors.fg_accent,
            );
        }

        let wrap_scroll = editor.wrap_scroll_offset;
        let wrap_total_rows = editor.wrapped_total_rows(wrap_width);
        draw_scrollbar(
            f,
            ScrollbarParams {
                area: scrollbar_area,
                total_lines: wrap_total_rows.max(1),
                visible_lines,
                scroll: wrap_scroll,
                search_matches: &file_search.search_matches,
                diagnostic_lines: diag_lines,
                diagnostic_index: Some(&app.cached_diagnostic_index),
                change_rows: &change_rows,
                cursor_line: cursor_wrap_row,
                colors: app.colors,
                hovered: app.hovered_scrollbar,
                active: app.dragging_v_scrollbar,
            },
        );
        if minimap_sep_width > 0 {
            draw_minimap_separator(f, minimap_sep_area, app.colors.ui.fg_overview_border, app.colors.bg_editor);
        }
        if minimap_width > 0 {
            draw_minimap(
                f,
                MinimapParams {
                    area: minimap_area,
                    editor,
                    scroll: editor.scroll_offset,
                    visible_lines,
                    search_matches: &file_search.search_matches,
                    diagnostic_lines: diag_lines,
                    diagnostic_index: Some(&app.cached_diagnostic_index),
                    change_index: Some(&app.cached_editor_change_index),
                    diff_changes,
                    colors: app.colors,
                    hovered: app.hovered_minimap,
                    active: app.dragging_minimap,
                },
            );
        }
        app.h_scrollbar_area = None;
        crate::ui_editor_folds::draw(f, app, code_area, &folded_rows, 0, &mut animation_geometry);
        app.copy_animation.observe(&selection_style);
        crate::ui_animation_frame::finish_editor(
            f,
            app,
            animation_geometry,
            scope_rail_area,
            false,
        );
        return;
    }

    // Get visible line indices (respecting folds)
    let display_lines = editor.visible_lines(scroll, visible_lines);
    if editor.blame.enabled {
        editor.blame.rows = display_lines.iter().map(|&line_idx| Some(crate::editor::WrappedRowInfo {
            line_idx, start_col: 0, end_col: 0, line_len: 0, is_first: true,
        })).collect();
    }

    // Draw gutter
    let gutter_bg = app.colors.bg_dark;
    let gutter_hl_bg = app.colors.bg_line_highlight;
    let gutter_fg = app.colors.fg_text;
    let gutter_dim = app.colors.ui.fg_line_number;
    let mut gutter_lines = gutter::Rows::new(f, gutter_area, gutter_bg);
    let mut gutter_num_buf = String::with_capacity(gutter_width + 2);
    for &i in &display_lines {
        let is_cursor = i == cursor_line;
        let bg = if is_cursor { gutter_hl_bg } else { gutter_bg };

        // Fold indicator
        let fold_char = if editor.is_fold_start(i) {
            "+"
        } else if editor.can_fold_line(i) {
            "▾"
        } else {
            " "
        };

        // Git diff indicator / bookmark indicator
        let is_bookmarked = editor.bookmarks.contains(&i);
        let diff_status = editor.line_diff_status(i);
        let (diff_char, diff_style) = if is_bookmarked {
            ("●", Style::default().fg(app.colors.fg_bookmark).bg(bg)) // Orange bookmark
        } else {
            gutter_diff_indicator(
                diff_status,
                bg,
                app.hovered_gutter_line == Some(i),
                &app.colors,
            )
        };

        format_gutter_line_number(
            &mut gutter_num_buf,
            render_line_numbers,
            app.relative_line_numbers,
            i,
            cursor_line,
            gutter_width,
        );
        let line_num_display = gutter_num_buf.as_str();
        let num_style = if is_cursor {
            Style::default()
                .fg(gutter_fg)
                .bg(bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(gutter_dim).bg(bg)
        };
        // Fold indicator hover detection
        let fold_col = gutter_area.x + gutter_area.width.saturating_sub(1);
        let gutter_row = gutter_area.y + (len_as_u16(gutter_lines.len()));
        let is_fold_hovered =
            fold_char != " " && app.mouse_row == gutter_row && app.mouse_col == fold_col;
        let fold_style = crate::ui_editor_folds::gutter_style(&app.colors, bg, fold_char == "+", is_fold_hovered);

        gutter_lines.push(f, [
            Span::styled(diff_char, diff_style),
            Span::styled(line_num_display, num_style),
            Span::styled(fold_char, fold_style),
        ]);
    }
    // Fill remaining gutter lines
    format_empty_gutter_row(&mut gutter_num_buf, render_line_numbers, gutter_width);
    let tilde_style = Style::default().fg(app.colors.fg_tilde).bg(gutter_bg);
    for _ in 0..visible_lines.saturating_sub(display_lines.len()) {
        gutter_lines.push(f, [Span::styled(gutter_num_buf.as_str(), tilde_style)]);
    }
    gutter_lines.finish(f);

    // Draw code with syntax highlighting
    let editor = &app.tabs[app.active_tab].editor;
    let has_selection = editor.has_selection();
    let selection_range = editor.selection_range();
    let matching_bracket = matching_bracket_cached;
    let matching_tag = matching_tag_cached;
    let search_matches = &app.tabs[app.active_tab].file_search.search_matches;
    let search_current = app.tabs[app.active_tab].file_search.search_current;
    // Word/selection occurrence highlighting
    let cursor_word_owned_nw: Option<std::sync::Arc<str>>;
    let cursor_word: Option<&str> = if app.tabs[app.active_tab].file_search.search_active {
        None
    } else if has_selection {
        cursor_word_owned_nw = cached_selection_occurrence_text(editor, &mut app.editor_selection_word_cache);
        cursor_word_owned_nw.as_deref()
    } else {
        // Use cached word_at_cursor to avoid recomputing every frame
        refresh_cursor_word(
            editor,
            &mut app.cached_cursor_word_key,
            &mut app.cached_cursor_word,
            app.editor_cursor_word_cache.get_mut(),
        );
        app.cached_cursor_word.as_deref()
    };
    let first_displayed = display_lines.first().copied().unwrap_or(0);
    // Stale-while-revalidate: absolute guide coordinates above an edit remain
    // useful until the exact same-editor viewport result replaces them.
    let bracket_cache_matches = app
        .cached_bracket_analysis_target
        .as_ref()
        .is_some_and(|target| {
            target.first_line == first_displayed
                && target.navigation_id == editor.navigation_id
                && target.editor.active_group == app.active_group
                && target.editor.active_tab == app.active_tab
                && target.editor.file_path == editor.file_path
        });
    let mut bracket_depth = if bracket_cache_matches {
        app.cached_bracket_starting_depth
    } else {
        0
    };
    let bracket_depth_index = app
        .cached_bracket_depth_index
        .as_ref()
        .filter(|index| index.matches(editor))
        .cloned();
    let colorize_brackets = app.bracket_pair_colorization
        && bracket_depth_index
            .as_ref()
            .is_none_or(|index| index.enabled());
    let mut code_lines = prepared_text::Rows::new(code_area, Style::default().bg(app.colors.bg_editor).remove_modifier(Modifier::DIM));
    let spaces_pool: &str = &crate::helpers::spaces(code_area.width as usize);
    let file_diags_non_wrap = std::sync::Arc::clone(&app.cached_editor_diagnostics);
    let mut inline_annotation = None;
    let mut inline_candidate = None;
    let diag_indices_by_line = std::sync::Arc::clone(&app.cached_diag_indices_by_line);
    let wide_diag_indices = std::sync::Arc::clone(&app.cached_wide_diag_indices);
    let cursor_word_char_len = cursor_word.map_or(0, |w| {
        if w.is_ascii() {
            w.len()
        } else {
            w.chars().count()
        }
    });
    if let Some(word) = cursor_word {
        editor.render_line_cache.borrow_mut().prepare_occurrences(
            (
                editor.content_version,
                editor.wrap_revision,
                editor.tab_size,
            ),
            &editor.rope,
            word,
            cursor_word_char_len,
            !has_selection,
        );
    }
    let mut trailing_selection_ranges: Vec<(usize, usize)> = Vec::with_capacity(8);
    let mut merged_trailing_ranges: Vec<(usize, usize)> = Vec::with_capacity(8);
    let visible_width = code_area.width as usize;
    let mut batch_buf = String::with_capacity(visible_width);
    let mut fmt_buf = String::with_capacity(128); // reusable buffer for per-line format!() avoidance
    let mut line_matches_buf: Vec<(usize, usize, bool)> = Vec::with_capacity(8);
    let mut line_match_runs = Vec::new();
    let mut line_highlight_columns = Vec::new();
    let mut line_diagnostic_columns = Vec::new();
    let mut line_diagnostic_runs = Vec::new();
    let mut line_spans: Vec<Span> = Vec::with_capacity(32);
    let mut line_highlights_buf: Vec<(u32, u32, u32, u32)> = Vec::with_capacity(16);
    let mut line_diags_buf: Vec<usize> = Vec::with_capacity(4);
    let extra_cursor_viewport = ExtraCursorViewport::new(editor, display_lines.iter().copied());
    let mut extra_cursor_scratch = ExtraCursorLine::default();
    let focus_mode_active = app.focus_mode && display_lines.binary_search(&cursor_line).is_ok();
    let mut source_lines = source_lines::SourceLines::new(&editor.rope, scroll);
    for &i in &display_lines {
        animation_geometry.map_scope_row(i, 0, code_lines.len());
        let rope_line = source_lines.get(i);
        let line_text_buf = editor.render_line_cache.borrow_mut().line_text(
            i,
            (
                editor.content_version,
                editor.wrap_revision,
                editor.tab_size,
            ),
            &editor.rope,
            rope_line,
        );
        let clean_line = line_text_buf.trim_end_matches(['\n', '\r']);
        let clean_line_char_count = rope_line.len_chars() - (line_text_buf.len() - clean_line.len());

        let cached_hl = app.highlighter.render_line(
            &line_text_buf, i, file_path.as_deref(), syntax_language.as_deref(),
            app.syntax_theme.as_str(), editor.content_version,
        );
        let highlighted = HighlightedLine::new(
            cached_hl
                .as_ref()
                .map(|line| (line.spans.as_slice(), line.char_len)),
            clean_line,
            clean_line_char_count,
            plain_theme_style(app.colors.fg_text),
        )
        .with_text_snapshot(&line_text_buf);

        let glyphs = editor.render_line_cache.borrow_mut().line(
            i,
            (
                editor.content_version,
                editor.wrap_revision,
                editor.tab_size,
            ),
            &editor.rope,
            &highlighted,
            cached_hl
                .as_ref()
                .map(|line| (&line.spans, line.lengths.as_slice())),
            h_scroll.saturating_add(visible_width),
            LineWhitespace::needs_boundary(editor.whitespace_mode, app.comment_whitespace_mode),
        );

        let is_current_line = i == cursor_line;
        let extra_cursor_line = extra_cursor_viewport.line(editor, i, &mut extra_cursor_scratch);
        let is_extra_cursor_line = !extra_cursor_line.columns.is_empty();
        // VS Code hides the current-line highlight when there is a multi-line selection
        let show_line_hl = (is_current_line || is_extra_cursor_line)
            && !{
                if let Some((sl, _, el, _)) = editor.selection_range() {
                    sl != el
                } else {
                    false
                }
            };
        let line_bg = if show_line_hl {
            app.colors.bg_line_highlight
        } else if editor.is_fold_start(i) {
            app.colors.bg_fold_indicator
        } else {
            app.colors.bg_editor
        };

        // Collect search matches on this line
        matches_for_line(search_matches, &editor.rope, i, search_current, &mut line_matches_buf);
        flatten_priority_ranges(&line_matches_buf, &mut line_match_runs);

        // Match positions are independent of the horizontal viewport.
        let word_occurrences = cursor_word.map(|_| {
            editor
                .render_line_cache
                .borrow_mut()
                .occurrences(i, clean_line)
        });

        let line_bracket_depth = bracket_depth_index
            .as_ref()
            .and_then(|index| index.line_starting_depth(i))
            .unwrap_or(bracket_depth);
        let bracket_colors = (colorize_brackets
            && rope_line.len_chars() <= crate::background::MAX_BRACKET_LINE_CHARS)
            .then(|| {
                editor
                    .render_line_cache
                    .borrow_mut()
                    .brackets(i, clean_line, line_bracket_depth)
            });
        bracket_depth = bracket_colors
            .as_ref()
            .map_or(line_bracket_depth, BracketLineColors::end_depth);

        // Pre-filter document highlights for this line (reuse scratch buffer)
        let line_u32_hl = i as u32;
        document_highlights_for_line(
            &app.lsp_document_highlights,
            &app.lsp_document_multiline_highlights,
            line_u32_hl,
            &mut line_highlights_buf,
        );
        highlight_column_ranges(&line_highlights_buf, line_u32_hl, &mut line_highlight_columns);

        // Diagnostics are indexed when providers publish a new revision;
        // rendering only visits matches for this visible line.
        diagnostic_indices_for_line(
            &file_diags_non_wrap,
            &diag_indices_by_line,
            &wide_diag_indices,
            i,
            &mut line_diags_buf,
        );
        diagnostic_column_ranges(&file_diags_non_wrap, &line_diags_buf, i, &app.colors,
            &mut line_diagnostic_columns, &mut line_diagnostic_runs);

        // Pre-compute main selection column range for this line
        let main_sel_cols: Option<(usize, usize)> = if has_selection {
            if let Some((sl, sc, el, ec)) = selection_range {
                if i >= sl && i <= el {
                    let start_col = if i == sl { sc } else { 0 };
                    let end_col = if i == el { ec } else { usize::MAX };
                    Some((start_col, end_col))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let comment_ranges = app.comment_whitespace_mode.and_then(|_| {
            cached_hl.as_ref().map(|line| std::rc::Rc::clone(&line.comments))
        });

        // Build character by character for cursor, selection, and search highlighting
        // Pre-compute semantic token ranges for this line
        let line_semantic: &[crate::lsp::DecodedSemanticToken] =
            if editor.blame.revision.is_none() && app.show_semantic_tokens && !app.lsp_semantic_tokens.is_empty() {
                let line_u32 = i as u32;
                let start = app
                    .lsp_semantic_tokens
                    .partition_point(|t| t.line < line_u32);
                let end = app
                    .lsp_semantic_tokens
                    .partition_point(|t| t.line <= line_u32);
                &app.lsp_semantic_tokens[start..end]
            } else {
                &[]
            };
        line_spans.clear();
        let mut col = editor
            .render_line_cache
            .borrow()
            .viewport_start(h_scroll)
            .min(glyphs.chars.len());
        let mut animation_col = 0usize;
        let mut visual_col = glyphs.visual_column(col);
        let ts = editor.tab_size;
        let mut prev_ch = col.checked_sub(1).map(|previous| glyphs.chars[previous]);
        let mut whitespace = LineWhitespace::new(
            editor.whitespace_mode, app.comment_whitespace_mode,
            comment_ranges.as_deref().map_or(&[], Vec::as_slice),
            glyphs.whitespace, col, i == cursor_line,
        );
        let mut sem_tok_idx: usize = 0; // track current semantic token for O(1) lookup
        let mut sem_color_idx = usize::MAX;
        let mut sem_color = None;
                                        // Span batching: accumulate chars with same style to reduce allocations
        batch_buf.clear();
        // Calculate leading whitespace for indent guides
        let leading_whitespace = glyphs.leading_whitespace;
        let indent_columns = glyphs.visual_column(leading_whitespace);
        let todo_markers = editor.render_line_cache.borrow_mut().todos(i, clean_line);

        // Detect inline color values (#RGB, #RRGGBB)
        let inline_colors = app.show_color_preview.then(|| {
            editor
                .render_line_cache
                .borrow_mut()
                .inline_colors(i, clean_line, check_named_colors)
        });

        let line_bg = crate::merge_conflict_ui::content_background(editor, i, &app.colors, line_bg);
        let merge_style = app.colors.ui.merge_marker_style(clean_line.chars(), line_bg);
        let line_bg = merge_style.and_then(|style| style.bg).unwrap_or(line_bg);

        // File diff highlight: green background for added lines in compare mode
        let line_bg = if app.diff_mode == crate::app::DiffMode::SideBySide
            && app.active_editor_matches_diff_target()
            && !app.compare_diff_lines.is_empty()
        {
            let row_idx = app.diff_render_cache.aligned_row_for_target_line(i);
            app.diff_render_cache
                .rows
                .get(row_idx)
                .filter(|row| row.right_line == Some(i) && row.right_kind == '+')
                .map_or(line_bg, |_| app.colors.bg_diff_added)
        } else {
            line_bg
        };

        let mut batch_style = Style::default();
        let visible_end = h_scroll.saturating_add(visible_width);
        let mut occurrences = word_occurrences
            .as_deref()
            .map(|ranges| WordOccurrenceCursor::new(ranges, h_scroll));
        let mut search_runs = ValueRangeCursor::new(&line_match_runs, h_scroll);
        let mut document_occurrences = OccurrenceCursor::new(&line_highlight_columns, h_scroll);
        let mut extra_selections = OccurrenceCursor::new(&extra_cursor_line.selections, h_scroll);
        let mut extra_cursor_cols = CursorColumnCursor::new(&extra_cursor_line.columns, h_scroll);
        let mut bracket_depths = bracket_colors.as_ref().map(|line| line.cursor(h_scroll));
        let mut diagnostic_runs = ValueRangeCursor::new(&line_diagnostic_runs, h_scroll);
        let mut todo_cursor = TodoCursor::new(&todo_markers, h_scroll);
        let mut color_previews = InlineColorCursor::new(
            inline_colors.as_deref().map_or(&[], Vec::as_slice),
            h_scroll,
        );
        let first_span = glyphs.span_ends.partition_point(|&end| end <= col);
        // Range decorations share a style up to their next boundary. A single
        // diagnostic or search hit must not force the entire row into cell work.
        let plain_line = plain_runs::enabled()
            && extra_cursor_line.columns.is_empty() && extra_cursor_line.selections.is_empty()
            && inline_colors.as_ref().is_none_or(|colors| colors.is_empty())
            && !matching_bracket.is_some_and(|(line, _)| i == line || is_current_line)
            && !matching_tag.is_some_and(|(a, _, _, b, _, _)| i == a || i == b)
            && !link_hover.is_some_and(|(line, _, _)| i == line);
        let decorated_indent = if app.show_indent_guides || app.indent_rainbow { leading_whitespace } else { 0 };
        let has_bracket_guides = bracket_cache_matches && !app.cached_bracket_guides.as_slice().is_empty();
        'syntax_spans: for (span_index, ((style, syntax_text), &span_end)) in highlighted
            .iter_from_span(first_span)
            .zip(&glyphs.span_ends[first_span..])
            .enumerate()
        {
            let span_start = (first_span + span_index).checked_sub(1)
                .map_or(0, |previous| glyphs.span_ends[previous]);
            if col >= visible_end {
                break;
            }
            let syntax_style = Style::default()
                .fg(crate::ui_colors::syntax_color(style.foreground, line_bg))
                .bg(line_bg)
                .add_modifier(syntax_font_modifiers(style.font_style));
            while col < span_end.min(glyphs.chars.len()) {
                if col >= visible_end {
                    break 'syntax_spans;
                }
                let mut text_style = syntax_style;
                let mut style_end = span_end.min(glyphs.chars.len()).min(visible_end);
                // Semantic tokens are runs too. Resolve their color only at a
                // token boundary, and never let a plain run cross that boundary.
                if !line_semantic.is_empty() && col >= h_scroll {
                    let col_u32 = col as u32;
                    while sem_tok_idx < line_semantic.len() {
                        let token = &line_semantic[sem_tok_idx];
                        if token.start_char + token.length > col_u32 { break; }
                        sem_tok_idx += 1;
                    }
                    if let Some(token) = line_semantic.get(sem_tok_idx) {
                        if col_u32 >= token.start_char {
                            if sem_color_idx != sem_tok_idx {
                                sem_color = editor_semantic_color_for_token(token, &app.lsp_manager,
                                    editor, app.semantic_theme_colors.as_ref(), &mut semantic_color_cache);
                                sem_color_idx = sem_tok_idx;
                            }
                            if let Some(color) = sem_color { text_style = text_style.patch(color); }
                            style_end = style_end.min((token.start_char + token.length) as usize);
                        } else {
                            style_end = style_end.min(token.start_char as usize);
                        }
                    }
                }
                if plain_line && col >= h_scroll {
                    let mut end = style_end;
                    let mut style = text_style;
                    let (search, boundary) = search_runs.run(col);
                    end = end.min(boundary);
                    if let Some(current) = search {
                        style = app.colors.ui.find_style(style, current, app.colors.bg_search_match);
                    }
                    let (document, boundary) = document_occurrences.run(col);
                    end = end.min(boundary);
                    if document {
                        style = app.colors.ui.occurrence_style(style, app.colors.bg_word_highlight, false);
                    }
                    if let Some(occurrences) = &mut occurrences {
                        let (word, boundary) = occurrences.run(col);
                        end = end.min(boundary);
                        if word {
                            style = app.colors.ui.occurrence_style(style, app.colors.bg_word_highlight, has_selection);
                        }
                    }
                    let (diagnostic, boundary) = diagnostic_runs.run(col);
                    end = end.min(boundary);
                    if let Some(color) = diagnostic {
                        style = style.add_modifier(Modifier::UNDERLINED).underline_color(color);
                    }
                    let (todo, boundary) = todo_cursor.run(col);
                    end = end.min(boundary);
                    if let Some(kind) = todo {
                        style = style.fg(kind.color(&app.colors)).add_modifier(Modifier::BOLD);
                    }
                    let selected = main_sel_cols.is_some_and(|(start, end)| col >= start && col < end);
                    if let Some((start, stop)) = main_sel_cols {
                        if col < start { end = end.min(start); }
                        else if col < stop { end = end.min(stop); }
                    }
                    if is_current_line && (show_cursors || show_inactive_cursor) && col <= cursor_col {
                        end = end.min(cursor_col);
                    }
                    let end = plain_runs::end(&glyphs.chars, col, end, app.bracket_pair_colorization,
                        decorated_indent, has_bracket_guides, selected, &mut whitespace, &glyphs.source_spaces);
                    if end > col {
                        let style = if selected { selection_style.apply(style) } else { style };
                        if style != batch_style {
                            code_lines.flush(&mut line_spans, &mut batch_buf, batch_style);
                            batch_style = style;
                        }
                        if end <= glyphs.ascii_prefix {
                            batch_buf.push_str(&syntax_text[col - span_start..end - span_start]);
                        } else {
                            batch_buf.extend(glyphs.chars[col..end].iter().copied());
                        }
                        if animation_geometry.enabled() {
                            animation_geometry.push_unit_width_run(i, col, &glyphs.chars[col..end], code_lines.len(), animation_col);
                            animation_col += end - col;
                        }
                        visual_col += end - col;
                        prev_ch = Some(glyphs.chars[end - 1]);
                        col = end;
                        continue;
                    }
                }
                let ch = glyphs.chars[col];
                // Only render characters in the visible horizontal range
                if col >= h_scroll && col < h_scroll + visible_width {
                    // Inject inline color swatch before #color value
                    if let Some(color) = color_previews.at(col) {
                        if !batch_buf.is_empty() {
                            code_lines.flush(&mut line_spans, &mut batch_buf, batch_style);
                        }
                        line_spans.push(Span::styled("▉", Style::default().fg(color).bg(line_bg)));
                        animation_col += 1;
                        batch_style = Style::default();
                    }
                    let mut s = text_style;
                    let cursor_animation_col = animation_col;
                    if animation_geometry.enabled() {
                        let animation_width = if ch == '\t' {
                            ts.saturating_sub(visual_col % ts.max(1)).max(1)
                        } else { unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0) };
                        animation_geometry.push(i, col, ch, code_lines.len(), animation_col, animation_width);
                        animation_col += animation_width;
                    }
                    // Bracket pair colorization
                    if app.bracket_pair_colorization
                        && matches!(ch, '(' | ')' | '[' | ']' | '{' | '}')
                    {
                        if let Some(depth) =
                            bracket_depths.as_mut().and_then(|cursor| cursor.at(col))
                            && let Some(color) = app.colors.ui.bracket_color(depth as usize)
                        {
                            s = s.fg(color);
                        }
                    }
                    let indent_depth = app.show_indent_guides.then(||
                        indent_guide_depth(visual_col, indent_columns, ts)).flatten();
                    let guide_color = if let Some(depth) = indent_depth {
                        Some(app.colors.ui.indent_guide_color(depth, cursor_indent == Some(visual_col)))
                    } else if ch == ' ' {
                        bracket_cache_matches.then(|| app.cached_bracket_guides.at(i, col)).flatten().map(|g| {
                            let index = (g.2 as usize).wrapping_sub(1) % 6;
                            if editor.cursor_line >= g.0 && editor.cursor_line < g.1 { app.colors.ui.bracket_guides_active[index] }
                            else { app.colors.ui.bracket_guides[index] }
                        })
                    } else { None };
                    // Search match highlight
                    if let Some(is_current) = search_runs.at(col) {
                        s = app.colors.ui.find_style(s, is_current, app.colors.bg_search_match);
                    }
                    // LSP document highlight (semantic symbol occurrences)
                    if document_occurrences.contains(col) {
                        s = app.colors.ui.occurrence_style(s, app.colors.bg_word_highlight, false);
                    }
                    // Word occurrence highlight
                    if occurrences
                        .as_mut()
                        .is_some_and(|cursor| cursor.contains(col))
                    {
                        s = app.colors.ui.occurrence_style(
                            s,
                            app.colors.bg_word_highlight,
                            has_selection,
                        );
                    }
                    let cell_selected = main_sel_cols
                        .is_some_and(|(sel_sc, sel_ec)| col >= sel_sc && col < sel_ec)
                        || extra_selections.contains(col);
                    // Matching bracket highlight
                    if let Some((ml, mc)) = matching_bracket {
                        if i == ml && col == mc {
                            s = app.colors.ui.bracket_style(s, app.colors.bg_bracket_match);
                        }
                    }
                    if is_current_line && col == cursor_col && matching_bracket.is_some() {
                        s = app.colors.ui.bracket_style(s, app.colors.bg_bracket_match);
                    }
                    // Matching tag highlight
                    if let Some((t1l, t1cs, t1ce, t2l, t2cs, t2ce)) = matching_tag {
                        if (i == t1l && col >= t1cs && col < t1ce)
                            || (i == t2l && col >= t2cs && col < t2ce)
                        {
                            s = app.colors.ui.bracket_style(s, app.colors.bg_bracket_match);
                        }
                    }
                    // Diagnostic underlines (pre-filtered per line)
                    if let Some(color) = diagnostic_runs.at(col) {
                        s = s.add_modifier(Modifier::UNDERLINED).underline_color(color);
                    }
                    if link_hover.is_some_and(|(line, start, end)| i == line && col >= start && col < end) {
                        s = s.add_modifier(Modifier::UNDERLINED);
                    }
                    // TODO/FIXME/HACK/NOTE marker highlight
                    if let Some(kind) = todo_cursor.at(col) {
                        s = s
                            .fg(kind.color(&app.colors))
                            .add_modifier(ratatui::style::Modifier::BOLD);
                    }
                    // Indent rainbow background
                    if app.indent_rainbow && col < leading_whitespace && ts > 0 {
                        let indent_level = visual_col / ts;
                        let rainbow_bg = match indent_level % 6 {
                            0 => bg_tint(app.colors.fg_error),
                            1 => bg_tint(app.colors.fg_warning),
                            2 => bg_tint(app.colors.fg_success),
                            3 => bg_tint(app.colors.fg_accent_blue),
                            4 => bg_tint(app.colors.fg_accent),
                            _ => bg_tint(app.colors.fg_git_modified),
                        };
                        s = s.bg(rainbow_bg);
                    }
                    let show_ws = whitespace.shows(ch, col,
                        ch == ' ' && (prev_ch == Some(' ') || glyphs.source_spaces.get(col + 1) == Some(&true)), cell_selected);
                    if let Some(color) = guide_color {
                        s = guide_style(s, color, app.colors.ui.fg_whitespace, show_ws);
                    }
                    // Selection takes priority over colored whitespace blocks.
                    if cell_selected {
                        s = selection_style.apply(s);
                    }
                    // Main cursor and extra cursors remain the top visual layer.
                    if show_cursors
                        && ((is_current_line && col == cursor_col)
                            || extra_cursor_cols.contains(col))
                    {
                        if is_current_line && col == cursor_col {
                            let cursor_width = if ch == '\t' {
                                ts.saturating_sub(visual_col % ts.max(1)).max(1)
                            } else if animation_geometry.enabled() {
                                unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0)
                            } else {
                                0
                            };
                            animation_geometry.record_primary_cursor(code_lines.len(), cursor_animation_col, cursor_width, s);
                        }
                        s = s.patch(cursor_style(s.bg.unwrap_or(line_bg)));
                    } else if show_inactive_cursor && is_current_line && col == cursor_col {
                        s = s.add_modifier(ratatui::style::Modifier::UNDERLINED);
                    }
                    if guide_color.is_some() {
                        if !batch_buf.is_empty() {
                            code_lines.flush(&mut line_spans, &mut batch_buf, batch_style);
                        }
                        line_spans.push(Span::styled(guide_glyph(ch, show_ws), s));
                        if ch == '\t' {
                            push_cell_padding(&mut line_spans, ts.saturating_sub(visual_col % ts.max(1)).max(1) - 1, s);
                        }
                        batch_style = s;
                    } else if ch == ' ' {
                        if show_ws {
                            let ns = s.fg(app.colors.ui.fg_whitespace).remove_modifier(Modifier::DIM);
                            if !batch_buf.is_empty() {
                                code_lines.flush(&mut line_spans, &mut batch_buf, batch_style);
                            }
                            line_spans.push(Span::styled("·", ns));
                            batch_style = ns;
                        } else if s == batch_style && !batch_buf.is_empty() {
                            batch_buf.push(ch);
                        } else {
                            if !batch_buf.is_empty() {
                                code_lines.flush(&mut line_spans, &mut batch_buf, batch_style);
                            }
                            batch_buf.push(ch);
                            batch_style = s;
                        }
                    } else if ch == '\t' {
                        // Tab: expand to tab_size-aligned spaces
                        let tab_w = ts.saturating_sub(visual_col % ts).max(1);
                        if !batch_buf.is_empty() {
                            code_lines.flush(&mut line_spans, &mut batch_buf, batch_style);
                        }
                        if show_ws {
                            let ns = s.fg(app.colors.ui.fg_whitespace).remove_modifier(Modifier::DIM);
                            line_spans.push(Span::styled(marked_tab(tab_w), ns));
                            batch_style = ns;
                        } else {
                            line_spans.push(Span::styled(cell_spaces(tab_w), s));
                            batch_style = s;
                        }
                    } else {
                        // Regular non-space characters
                        if show_ws {
                            let ws_ch = "·";
                            let ns = s.fg(app.colors.ui.fg_whitespace).remove_modifier(Modifier::DIM);
                            if !batch_buf.is_empty() {
                                code_lines.flush(&mut line_spans, &mut batch_buf, batch_style);
                            }
                            line_spans.push(Span::styled(ws_ch, ns));
                            batch_style = ns;
                        } else if s == batch_style && !batch_buf.is_empty() {
                            batch_buf.push(ch);
                        } else {
                            if !batch_buf.is_empty() {
                                code_lines.flush(&mut line_spans, &mut batch_buf, batch_style);
                            }
                            batch_buf.push(ch);
                            batch_style = s;
                        }
                    }
                }
                prev_ch = Some(ch);
                if ch == '\t' {
                    visual_col += ts.saturating_sub(visual_col % ts).max(1);
                } else {
                    visual_col += 1;
                }
                col += 1;
            }
        }
        // Flush remaining batch buffer
        if !batch_buf.is_empty() {
            code_lines.flush(&mut line_spans, &mut batch_buf, batch_style);
        }
        let line_len = clean_line_char_count;
        if col >= line_len && line_len >= h_scroll {
            animation_geometry.push(i, line_len, '\0', code_lines.len(), animation_col, 1);
        }

        // Render selection highlight in virtual space past EOL (main + extra cursor selections).
        trailing_selection_ranges.clear();
        if let Some((sl, sc, el, ec)) = selection_range {
            if i >= sl && i <= el {
                let (sel_start, sel_end) = if sl == el {
                    (sc, ec)
                } else if i == sl {
                    (sc, line_len)
                } else if i == el {
                    (0, ec)
                } else {
                    (0, line_len)
                };
                if sel_end > line_len {
                    trailing_selection_ranges.push((sel_start.max(line_len), sel_end));
                }
            }
        }
        for &(sel_start, sel_end) in &extra_cursor_line.virtual_selections {
            if sel_end > line_len {
                trailing_selection_ranges.push((sel_start.max(line_len), sel_end));
            }
        }
        trailing_selection_ranges.sort_unstable_by_key(|(start, end)| (*start, *end));
        merged_trailing_ranges.clear();
        for &(start, end) in &trailing_selection_ranges {
            if let Some((_, last_end)) = merged_trailing_ranges.last_mut() {
                if start <= *last_end {
                    *last_end = (*last_end).max(end);
                    continue;
                }
            }
            merged_trailing_ranges.push((start, end));
        }
        for &(sel_start, sel_end) in &merged_trailing_ranges {
            let draw_start = sel_start.max(h_scroll);
            let draw_end = sel_end.min(h_scroll + visible_width);
            if draw_start >= draw_end {
                continue;
            }
            let mut rendered_cols: usize = line_spans
                .iter()
                .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()))
                .sum();
            let padding = draw_start.saturating_sub(h_scroll).saturating_sub(rendered_cols);
            push_cell_padding(&mut line_spans, padding, Style::default().bg(line_bg));
            rendered_cols += padding;
            let selected = draw_end - draw_start;
            push_cell_padding(&mut line_spans, selected, selection_style.apply(Style::default()));
            if animation_geometry.enabled() {
                for (offset, virtual_col) in (draw_start..draw_end).enumerate() {
                    animation_geometry.push(i, virtual_col, ' ', code_lines.len(), rendered_cols + offset, 1);
                }
            }
        }

        // Cursor at end of line (adjusted for h_scroll)
        let search_eol = line_matches_buf.iter().find_map(|&(start, end, current)|
            (start <= line_len && end > line_len).then_some(current));
        let cursor_at_end = is_current_line && cursor_col >= line_len;
        let main_draw_col = if search_eol.is_some() { line_len } else { cursor_col.min(line_len) };
        if (is_current_line || search_eol.is_some())
            && main_draw_col >= col
            && main_draw_col >= h_scroll
            && main_draw_col < h_scroll + visible_width
        {
            let mut rendered_cols: usize = line_spans
                .iter()
                .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()))
                .sum();
            let padding = main_draw_col.saturating_sub(h_scroll).saturating_sub(rendered_cols);
            push_cell_padding(&mut line_spans, padding, Style::default().bg(line_bg));
            rendered_cols += padding;
            // Always reserve the cursor column to keep blame text stable
            let eol_style = if show_cursors && cursor_at_end {
                animation_geometry.record_primary_cursor(code_lines.len(), rendered_cols, 1, Style::default().bg(line_bg));
                cursor_style(line_bg)
            } else if let Some(current) = search_eol {
                app.colors.ui.find_style(Style::default(), current, app.colors.bg_search_match)
            } else if show_inactive_cursor && cursor_at_end {
                Style::default()
                    .bg(line_bg)
                    .add_modifier(ratatui::style::Modifier::UNDERLINED)
            } else {
                Style::default().bg(line_bg)
            };
            line_spans.push(Span::styled(" ", eol_style));
        }

        // Extra cursors at or past end of line.
        let extra_eol_cols = std::iter::once(line_len).filter(|_| {
            extra_cursor_line
                .columns
                .last()
                .is_some_and(|&column| column >= line_len)
        });
        if show_cursors {
            for ec_col in extra_eol_cols {
                if ec_col < h_scroll || ec_col >= h_scroll + visible_width {
                    continue;
                }
                let mut rendered_cols: usize = line_spans
                    .iter()
                    .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()))
                    .sum();
                let padding = ec_col.saturating_sub(h_scroll).saturating_sub(rendered_cols);
                push_cell_padding(&mut line_spans, padding, Style::default().bg(line_bg));
                rendered_cols += padding;
                let (eol_str, eol_style) = (" ", cursor_style(line_bg));
                line_spans.push(Span::styled(eol_str, eol_style));
                animation_geometry.push(i, ec_col, '\0', code_lines.len(), rendered_cols, 1);
            }
        }
        if editor.is_fold_start(i) {
            folded_rows.push(crate::ui_editor_folds::Row {
                line: i, screen_row: code_lines.len(),
                content_width: line_spans.iter().map(|span| span.content.width()).sum(),
                start_col: h_scroll, end_col: h_scroll.saturating_add(visible_width),
            });
        }
        // LSP inlay hints inline
        if editor.blame.revision.is_none() && app.show_inlay_hints && !app.lsp_inlay_hints.is_empty() {
            for hint in inlay_hints_for_line(&app.lsp_inlay_hints, i as u32) {
                fmt_buf.clear();
                let _ = write!(fmt_buf, " {} ", hint.label);
                code_lines.flush(&mut line_spans, &mut fmt_buf, app.colors.ui.inlay_hint_style(hint.color_kind_index(), line_bg));
                fmt_buf.clear();
            }
        }
        // Code lens annotations (e.g. "3 references | Run Test")
        if app.show_code_lenses && !app.lsp_code_lenses.is_empty() {
            if let Ok(idx) = app
                .lsp_code_lenses
                .binary_search_by_key(&i, |&(line, _)| line)
            {
                let label = &app.lsp_code_lenses[idx].1;
                fmt_buf.clear();
                let _ = write!(fmt_buf, "  ▷ {label}");
                code_lines.flush(&mut line_spans, &mut fmt_buf, app.colors.ui.codelens_style(line_bg));
                fmt_buf.clear();
            }
        }
        // Git blame inline (GitLens-style) on current line
        if app.source_control_enabled && is_current_line && !editor.blame.enabled {
            let blame_column: usize = line_spans
                .iter()
                .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()))
                .sum();
            let start = blame_column.saturating_add(2);
            if start < visible_width {
                inline_candidate = Some((Rect::new(
                    code_area.x + start as u16,
                    code_area.y + len_as_u16(code_lines.len()),
                    (visible_width - start) as u16,
                    1,
                ), i));
            }
            if inline_candidate.is_some()
                && let Some(blame_text) = get_cached_blame_line(app, editor, i)
            {
                fmt_buf.clear();
                let _ = write!(fmt_buf, "  {blame_text}");
                // Blame is drawn over the bubble, including its spaces. Its
                // rendered extent must not alter the source contour.
                let hit = Rect::new(
                    code_area.x.saturating_add(len_as_u16(blame_column.saturating_add(2))),
                    code_area.y + len_as_u16(code_lines.len()),
                    len_as_u16(unicode_width::UnicodeWidthStr::width(blame_text.as_str())),
                    1,
                ).intersection(code_area);
                if !hit.is_empty() {
                    inline_annotation = Some((hit, i));
                }
                animation_geometry.overlay(
                    code_lines.len(),
                    blame_column,
                    unicode_width::UnicodeWidthStr::width(fmt_buf.as_str()),
                );
                code_lines.flush(&mut line_spans, &mut fmt_buf, Style::default().fg(app.colors.fg_dim));
                fmt_buf.clear();
            }
        }
        let mut displayed: usize = line_spans
            .iter()
            .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()))
            .sum();
        let mut annotation_occupied = visible_width;
        if app.error_lens {
            annotation_occupied = 0;
            for span in &line_spans {
                if advance_error_lens_width(
                    &mut annotation_occupied,
                    span.content.as_ref(),
                    visible_width,
                ) {
                    break;
                }
            }
        }
        let reference_decorations = editor.render_line_cache.borrow().reference_decorations();
        // Once the source fills the viewport, end-of-line annotations cannot
        // contribute cells. Do not materialize the offscreen comment suffix.
        if app.error_lens
            && (reference_decorations || annotation_occupied < visible_width)
            && let Some(first_marker) = todo_markers.0.first()
        {
            let trimmed = clean_line[first_marker.byte as usize..].trim();
            if !trimmed.is_empty() {
                let foreground = first_marker.kind.color(&app.colors);
                displayed += append_error_lens(
                    &mut code_lines,
                    &mut line_spans,
                    &mut fmt_buf,
                    &mut annotation_occupied,
                    visible_width,
                    "◆",
                    trimmed,
                    Style::default()
                        .fg(foreground)
                        .bg(bg_tint(foreground))
                        .add_modifier(Modifier::ITALIC),
                    reference_decorations,
                );
            }
        }
        // Error lens: show LSP diagnostics inline at end of line
        if app.error_lens && (reference_decorations || annotation_occupied < visible_width) {
            let diag_idx = file_diags_non_wrap.partition_point(|d| d.line < i);
            if let Some(diag) = file_diags_non_wrap.get(diag_idx).filter(|d| d.line == i) {
                let fg = app.colors.ui.diagnostic(diag.severity);
                let bg_tint_color = if diag.severity == lsp_types::DiagnosticSeverity::ERROR {
                    bg_tint(app.colors.fg_error)
                } else if diag.severity == lsp_types::DiagnosticSeverity::WARNING {
                    bg_tint(app.colors.fg_warning)
                } else {
                    bg_tint(app.colors.fg_info_label)
                };
                let icon = crate::lsp::severity_icon(diag.severity);
                let msg = &diag.message;
                let first_line = msg.lines().next().unwrap_or(msg);
                displayed += append_error_lens(
                    &mut code_lines,
                    &mut line_spans,
                    &mut fmt_buf,
                    &mut annotation_occupied,
                    visible_width,
                    icon,
                    first_line,
                    Style::default()
                        .fg(fg)
                        .bg(bg_tint_color)
                        .add_modifier(Modifier::ITALIC),
                    reference_decorations,
                );
            }
        }

        // Fill rest of line with background
        let remaining = visible_width.saturating_sub(displayed);
        if remaining > 0 {
            line_spans.push(Span::styled(
                &spaces_pool[..remaining.min(spaces_pool.len())],
                Style::default().bg(line_bg),
            ));
        }
        // Apply focus mode dimming (only when cursor is visible on screen)
        if focus_mode_active && !is_current_line {
            let distance = i.abs_diff(cursor_line);
            if distance > FOCUS_MODE_DIM_START {
                let dim_factor =
                    ((distance - FOCUS_MODE_DIM_START) as f32 / FOCUS_MODE_DIM_RANGE).min(0.7);
                for span in &mut line_spans {
                        let dimmed_fg = match span.style.fg {
                            Some(Color::Rgb(r, g, b)) => {
                                let f = 1.0 - dim_factor;
                                Color::Rgb(
                                    (f32::from(r) * f) as u8,
                                    (f32::from(g) * f) as u8,
                                    (f32::from(b) * f) as u8,
                                )
                            }
                            _ => span.style.fg.unwrap_or(app.colors.fg_dim),
                        };
                        span.style = span.style.fg(dimmed_fg);
                }
            }
        }
        paint_editor_row(f, &mut code_lines, &mut line_spans,
            merge_style.map(|style| style.bg(line_bg)), &app.colors);
    }

    // Fill empty lines below content
    code_lines.finish(f.buffer_mut());

    app.tabs[app.active_tab].editor.blame.inline_annotation = inline_annotation;
    app.tabs[app.active_tab].editor.blame.inline_candidate = inline_candidate;

    // Text drag-and-drop: show insertion indicator at mouse position (large file mode)
    if app.dragging_text {
        draw_text_drag_indicator(
            f,
            code_area,
            app.mouse_row,
            app.mouse_col,
            app.colors.fg_accent,
        );
    }

    // Sticky scroll: overlay scope-opening lines at top of editor
    if app.sticky_scroll && scroll > 0 {
        let editor = &app.tabs[app.active_tab].editor;
        let max_sticky = app.sticky_scroll_max;

        // Keep the last complete same-document header through edits. The
        // analysis result explicitly invalidates this cache when updated scope
        // coordinates arrive; including content_version here made an edit
        // rebuild from stale absolute coordinates and blink the header away.
        let sticky_key = (scroll as u64)
            .wrapping_mul(31)
            .wrapping_add(max_sticky as u64);

        if sticky_key != app.cached_sticky_key {
            app.cached_sticky_lines.clear();

            // Scope parents are computed by the editor-analysis worker. Follow
            // only that short chain instead of rescanning every line above the
            // viewport during rendering.
            let mut scope_index = app
                .cached_sticky_scopes
                .partition_point(|scope| scope.line < scroll)
                .checked_sub(1);
            while let Some(index) = scope_index {
                let Some(scope) = app.cached_sticky_scopes.get(index) else {
                    break;
                };
                if scope.line < editor.rope.len_lines() {
                    let text = editor.rope.line(scope.line).to_string();
                    app.cached_sticky_lines
                        .push((scope.line, text.trim_end_matches(['\n', '\r']).to_string()));
                }
                if app.cached_sticky_lines.len() >= max_sticky {
                    break;
                }
                scope_index = scope.parent;
            }
            app.cached_sticky_lines.reverse();
            app.cached_sticky_key = sticky_key;
        }

        let sticky_lines = &app.cached_sticky_lines;

        // Render sticky lines overlaying the top of the code area
        let gw = gutter_width;
        for (si, (_line_idx, text)) in sticky_lines.iter().enumerate() {
            if si >= code_area.height as usize {
                break;
            }
            let y = code_area.y + si as u16;
            if app.tabs[app.active_tab].editor.blame.inline_annotation.is_some_and(|(area, _)| area.y == y) {
                app.tabs[app.active_tab].editor.blame.inline_annotation = None;
            }
            if app.tabs[app.active_tab].editor.blame.inline_candidate.is_some_and(|(area, _)| area.y == y) {
                app.tabs[app.active_tab].editor.blame.inline_candidate = None;
            }
            if app.tabs[app.active_tab].editor.blame.enabled {
                if let Some(row) = app.tabs[app.active_tab].editor.blame.rows.get_mut(si) {
                    *row = Some(crate::editor::WrappedRowInfo {
                        line_idx: *_line_idx, start_col: 0, end_col: 0, line_len: 0, is_first: true,
                    });
                }
            }
            let row_area = Rect::new(gutter_area.x, y, code_area.right().saturating_sub(gutter_area.x), 1);
            let hovered = row_area.contains((app.mouse_col, app.mouse_row).into());
            let mut sticky_style = app.colors.ui.sticky_scroll_style(app.colors.bg_editor, hovered);
            let sticky_bg = sticky_style.bg.unwrap_or(app.colors.bg_editor);
            if si + 1 == sticky_lines.len().min(code_area.height as usize)
                && let Some(border) = app.colors.ui.sticky_border.border(sticky_bg)
            {
                sticky_style = sticky_style.add_modifier(Modifier::UNDERLINED).underline_color(border);
            }
            // Build padded/truncated text to fill code area width
            let mut display: String = {
                let mut s = String::new();
                let mut w = 0usize;
                for ch in text.chars().skip(h_scroll) {
                    let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                    if w + cw > code_area.width as usize {
                        break;
                    }
                    s.push(ch);
                    w += cw;
                }
                s
            };
            let disp_w = unicode_width::UnicodeWidthStr::width(display.as_str());
            let pad = (code_area.width as usize).saturating_sub(disp_w);
            for _ in 0..pad {
                display.push(' ');
            }
            f.render_widget(
                Paragraph::new(display).style(sticky_style),
                Rect::new(code_area.x, y, code_area.width, 1),
            );
            // Also overlay the gutter for sticky lines
            let mut gutter_buf = String::with_capacity(gw + 2);
            if render_line_numbers {
                let _ = write!(gutter_buf, " {:>width$} ", _line_idx + 1, width = gw);
            } else {
                gutter_buf.push_str("  ");
            }
            gutter_buf.truncate(gw + 2);
            f.render_widget(
                Paragraph::new(gutter_buf)
                    .style(sticky_style.fg(app.colors.ui.readable_foreground(
                        app.colors.ui.line_number_foreground.over(sticky_bg)
                            .unwrap_or_else(|| app.colors.ui.description_color(sticky_bg)), sticky_bg, 4.5))),
                Rect::new(gutter_area.x, y, gutter_area.width, 1),
            );
        }
    }

    // Draw column rulers (semi-transparent - preserve text underneath)
    let h_scroll = app.tabs[app.active_tab].editor.horizontal_scroll;
    draw_column_rulers(
        f,
        code_area,
        &app.rulers,
        h_scroll,
        app.colors.ui.ruler_foreground,
    );

    // Draw minimap separator
    if minimap_sep_width > 0 {
        draw_minimap_separator(f, minimap_sep_area, app.colors.ui.fg_overview_border, app.colors.bg_editor);
    }

    // Draw minimap
    if minimap_width > 0 {
        draw_minimap(
            f,
            MinimapParams {
                area: minimap_area,
                editor: &app.tabs[app.active_tab].editor,
                scroll,
                visible_lines,
                search_matches: &app.tabs[app.active_tab].file_search.search_matches,
                diagnostic_lines: diag_lines,
                diagnostic_index: Some(&app.cached_diagnostic_index),
                change_index: Some(&app.cached_editor_change_index),
                diff_changes,
                colors: app.colors,
                hovered: app.hovered_minimap,
                active: app.dragging_minimap,
            },
        );
    }

    // Draw scrollbar (use visible line count for folds)
    let effective_lines = app.tabs[app.active_tab].editor.visible_line_count();
    draw_scrollbar(
        f,
        ScrollbarParams {
            area: scrollbar_area,
            total_lines: effective_lines,
            visible_lines,
            scroll,
            search_matches: &app.tabs[app.active_tab].file_search.search_matches,
            diagnostic_lines: diag_lines,
            diagnostic_index: Some(&app.cached_diagnostic_index),
            change_rows: &change_rows,
            cursor_line,
            colors: app.colors,
            hovered: app.hovered_scrollbar,
            active: app.dragging_v_scrollbar,
        },
    );

    // Draw horizontal scrollbar
    if show_h_scrollbar && h_scrollbar_area.height > 0 {
        let h_area = Rect::new(
            code_area.x,
            h_scrollbar_area.y,
            code_area.width,
            1,
        );
        app.h_scrollbar_area = Some(h_area);
        draw_horizontal_scrollbar(
            f,
            HScrollbarParams {
                area: h_area,
                total_width: max_line_width,
                visible_width: code_area.width as usize,
                scroll: h_scroll,
                bg_editor: app.colors.bg_editor,
                hovered: app.hovered_h_scrollbar,
                active: app.dragging_h_scrollbar,
                colors: &app.colors.ui,
            },
        );
    } else {
        app.h_scrollbar_area = None;
    }

    apply_editor_gradients(
        f,
        code_area,
        app.colors.bg_editor,
        app.colors.bg_line_highlight,
        app.colors.gradients.bg_editor,
        app.colors.gradients.bg_line_highlight,
    );

    // Sticky headers own their rows. Cosmetic overlays are clipped below them
    // and run before either editor tooltip or the outer dialog layer is drawn.
    let sticky_rows = if app.sticky_scroll && scroll > 0 { app.cached_sticky_lines.len().min(code_area.height as usize) as u16 } else { 0 };
    crate::ui_editor_folds::draw(f, app, code_area, &folded_rows, sticky_rows as usize, &mut animation_geometry);
    animation_geometry.occlude_top(sticky_rows);
    let animation_gutter = Rect::new(
        scope_rail_area.x,
        scope_rail_area.y.saturating_add(sticky_rows),
        scope_rail_area.width,
        scope_rail_area.height.saturating_sub(sticky_rows),
    );
    app.copy_animation.observe(&selection_style);
    crate::ui_animation_frame::finish_editor(f, app, animation_geometry, animation_gutter, true);

    // Gutter blame tooltip on hover
    draw_gutter_blame_tooltip(f, app, gutter_area);

    // Diagnostic marker tooltip on hover (TODO/FIXME/HACK/BUG/NOTE)
    draw_diagnostic_marker_tooltip(f, app, code_area);
}

pub(crate) fn draw_animation_frame_tooltips(f: &mut Frame, app: &mut App) {
    if let Some(gutter) = app.gutter_area {
        draw_gutter_blame_tooltip(f, app, gutter);
    }
    if let Some(code) = app.code_area {
        draw_diagnostic_marker_tooltip(f, app, code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{app::Tab, editor::Editor};
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn selection_occurrences_reject_long_text_before_copying_and_preserve_trim_rules() {
        for (text, accepted) in [
            ("token".to_string(), true),
            ("  token  ".to_string(), true),
            ("界".repeat(60), true),
            ("界".repeat(100), false),
            ("x".repeat(500_000), false),
            (format!("{}token{}", " ".repeat(300), "\t".repeat(300)), true),
            (" ".repeat(1_000), false),
        ] {
            let mut editor = Editor::test_with_text(&text);
            editor.select_all();
            assert_eq!(selection_occurrence_text(&editor), accepted.then_some(text));
        }
        let mut editor = Editor::test_with_text("first\nsecond");
        editor.select_all();
        assert!(selection_occurrence_text(&editor).is_none());
    }

    fn diagnostic(line: usize, end_line: usize) -> crate::lsp::LspDiagnostic {
        crate::lsp::LspDiagnostic {
            line,
            col: 0,
            end_line,
            end_col: 1,
            message: "diagnostic".to_string(),
            severity: lsp_types::DiagnosticSeverity::WARNING,
        }
    }

    #[test]
    fn diagnostic_line_index_keeps_frame_queries_sparse() {
        let diagnostics = vec![diagnostic(0, 100), diagnostic(10, 10), diagnostic(20, 22)];
        let (by_line, wide) = build_diagnostic_line_index(&diagnostics);
        assert_eq!(wide, vec![0]);

        let mut matches = Vec::new();
        diagnostic_indices_for_line(&diagnostics, &by_line, &wide, 10, &mut matches);
        assert_eq!(matches, vec![0, 1]);
        diagnostic_indices_for_line(&diagnostics, &by_line, &wide, 21, &mut matches);
        assert_eq!(matches, vec![0, 2]);
        diagnostic_indices_for_line(&diagnostics, &by_line, &wide, 101, &mut matches);
        assert!(matches.is_empty());
    }

    #[test]
    fn document_highlight_queries_are_bounded_to_the_visible_line() {
        let highlights: Vec<_> = (0..50_000).map(|line| (line, 1, line, 4)).collect();
        let multiline = [(20, 0, 22, 3)];
        let mut visible = Vec::new();

        document_highlights_for_line(&highlights, &multiline, 21, &mut visible);

        assert_eq!(visible, vec![(21, 1, 21, 4), (20, 0, 22, 3)]);
    }

    #[test]
    fn inlay_hint_queries_return_only_the_requested_line() {
        let hints: Vec<_> = (0..1_000)
            .map(|line| crate::app::InlayHintDisplay::new(line, 3, format!(": type_{line}"), None))
            .collect();

        let visible = inlay_hints_for_line(&hints, 750);

        assert_eq!(visible, &[crate::app::InlayHintDisplay::new(750, 3, ": type_750", None)]);
    }

    #[test]
    fn document_links_keep_editor_background_and_no_idle_underline() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("#include <vector>\nint main() {}");
        editor.file_path = Some(std::path::PathBuf::from("fixture.cpp"));
        editor.cursor_line = 1;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.show_line_numbers = false;
        app.show_minimap = false;
        app.sticky_scroll = false;
        app.lsp_document_links
            .push((0, 9, 17, "file:///usr/include/vector".to_string()));

        let mut terminal = Terminal::new(TestBackend::new(50, 4)).expect("test terminal");
        terminal
            .draw(|frame| draw_editor(frame, &mut app, Rect::new(0, 0, 50, 4)))
            .expect("editor draw");

        let buffer = terminal.backend().buffer();
        let (start_x, y) = (0..4)
            .find_map(|y| {
                (0..50)
                    .find(|&x| buffer.cell((x, y)).is_some_and(|cell| cell.symbol() == "<"))
                    .map(|x| (x, y))
            })
            .expect("rendered include link");

        for x in start_x..start_x + 8 {
            let cell = buffer.cell((x, y)).expect("link cell");
            assert_eq!(cell.bg, app.colors.bg_editor);
            assert!(!cell.modifier.contains(Modifier::UNDERLINED));
        }

        assert_eq!(
            app.lsp_document_links.len(),
            1,
            "Ctrl+click target remains registered"
        );
    }

    #[test]
    fn whitespace_markers_use_whitespace_role_without_terminal_dim_modifier() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("  value\n");
        editor.file_path = Some(std::path::PathBuf::from("fixture.cpp"));
        editor.cursor_line = 1;
        editor.whitespace_mode = WhitespaceMode::All;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.show_line_numbers = false;
        app.show_minimap = false;
        app.sticky_scroll = false;

        let area = Rect::new(0, 0, 30, 3);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| {
                frame
                    .buffer_mut()
                    .set_style(area, Style::default().add_modifier(Modifier::DIM));
                draw_editor(frame, &mut app, area);
            })
            .expect("editor draw");

        let buffer = terminal.backend().buffer();
        let code_area = app.code_area.expect("code area");
        let mut marker_count = 0;
        for y in code_area.top()..code_area.bottom() {
            for x in code_area.left()..code_area.right() {
                let cell = buffer.cell((x, y)).expect("code cell");
                assert!(!cell.modifier.contains(Modifier::DIM));
                if cell.symbol() == "·" {
                    marker_count += 1;
                    assert_eq!(cell.fg, app.colors.ui.fg_whitespace);
                }
            }
        }
        assert_eq!(marker_count, 2);
    }

    #[test]
    fn trailing_whitespace_uses_configured_markers_without_background_blocks() {
        for word_wrap in [false, true] {
            for mode in [
                WhitespaceMode::None,
                WhitespaceMode::All,
                WhitespaceMode::Boundary,
                WhitespaceMode::Selection,
                WhitespaceMode::Trailing,
            ] {
                let mut app = App::test_new();
                let mut editor = Editor::test_with_text("value  \t\nnext\n");
                editor.whitespace_mode = mode;
                editor.word_wrap = word_wrap;
                editor.cursor_line = 1;
                app.tabs.push(Tab::new_file(editor));
                app.active_tab = 0;
                app.cursor_visible = false;
                app.show_line_numbers = false;
                app.show_minimap = false;
                app.sticky_scroll = false;

                let area = Rect::new(0, 0, 30, 4);
                let mut terminal =
                    Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
                terminal
                    .draw(|frame| draw_editor(frame, &mut app, area))
                    .expect("editor draw");

                let code_area = app.code_area.expect("code area");
                let buffer = terminal.backend().buffer();
                let show_markers = matches!(
                    mode,
                    WhitespaceMode::All | WhitespaceMode::Boundary | WhitespaceMode::Trailing
                );
                for col in 5..8 {
                    let cell = buffer
                        .cell((code_area.x + col, code_area.y))
                        .expect("trailing whitespace cell");
                    let symbol = if !show_markers {
                        " "
                    } else if col == 7 {
                        "→"
                    } else {
                        "·"
                    };
                    assert_eq!(cell.symbol(), symbol, "word_wrap={word_wrap}, mode={mode:?}");
                    assert_eq!(
                        cell.bg, app.colors.bg_editor,
                        "word_wrap={word_wrap}, mode={mode:?}"
                    );
                    if show_markers {
                        assert_eq!(cell.fg, app.colors.ui.fg_whitespace);
                    }
                }
            }
        }
    }

    #[test]
    fn selection_background_is_preserved_on_trailing_whitespace() {
        for word_wrap in [false, true] {
            let mut app = App::test_new();
            let mut editor = Editor::test_with_text("value  \nnext\n");
            editor.file_path = Some(std::path::PathBuf::from("fixture.txt"));
            editor.whitespace_mode = WhitespaceMode::Trailing;
            editor.word_wrap = word_wrap;
            editor.selection_anchor = Some((0, 5));
            editor.cursor_line = 1;
            editor.cursor_col = 0;
            app.tabs.push(Tab::new_file(editor));
            app.active_tab = 0;
            app.cursor_visible = false;
            app.show_line_numbers = false;
            app.show_minimap = false;
            app.sticky_scroll = false;

            let area = Rect::new(0, 0, 30, 4);
            let mut terminal =
                Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
            terminal
                .draw(|frame| draw_editor(frame, &mut app, area))
                .expect("editor draw");

            let code_area = app.code_area.expect("code area");
            let buffer = terminal.backend().buffer();
            for col in 5..7 {
                let cell = buffer
                    .cell((code_area.x + col, code_area.y))
                    .expect("selected trailing-whitespace cell");
                assert_eq!(cell.symbol(), "·");
                assert_eq!(cell.bg, app.colors.bg_selection, "word_wrap={word_wrap}");
            }
        }
    }

    #[test]
    fn selection_background_overrides_indent_rainbow_blocks() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("  value\n  next\n");
        editor.file_path = Some(std::path::PathBuf::from("fixture.txt"));
        editor.selection_anchor = Some((0, 0));
        editor.cursor_line = 0;
        editor.cursor_col = 2;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.cursor_visible = false;
        app.indent_rainbow = true;
        app.show_indent_guides = false;
        app.show_line_numbers = false;
        app.show_minimap = false;
        app.sticky_scroll = false;

        let area = Rect::new(0, 0, 30, 4);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| draw_editor(frame, &mut app, area))
            .expect("editor draw");

        let code_area = app.code_area.expect("code area");
        let buffer = terminal.backend().buffer();
        for col in 0..2 {
            let selected = buffer
                .cell((code_area.x + col, code_area.y))
                .expect("selected indent cell");
            let unselected = buffer
                .cell((code_area.x + col, code_area.y + 1))
                .expect("unselected indent cell");
            assert_eq!(selected.bg, app.colors.bg_selection);
            assert_ne!(unselected.bg, app.colors.bg_selection);
        }
    }

    #[test]
    fn horizontal_scrollbar_paints_gutter_and_overview_end_caps() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(&format!("{}\n", "x".repeat(500)));
        editor.file_path = Some(std::path::PathBuf::from("wide.txt"));
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.show_line_numbers = true;
        app.show_minimap = true;
        app.sticky_scroll = false;

        let area = Rect::new(0, 0, 120, 10);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| {
                frame
                    .buffer_mut()
                    .set_style(area, Style::default().bg(Color::Black));
                draw_editor(frame, &mut app, area);
            })
            .expect("editor draw");

        let buffer = terminal.backend().buffer();
        let gutter = app.gutter_area.expect("gutter area");
        let scrollbar = app.h_scrollbar_area.expect("horizontal scrollbar");
        let y = scrollbar.y;

        for x in gutter.left()..gutter.right() {
            assert_eq!(
                buffer.cell((x, y)).expect("gutter cap cell").bg,
                app.colors.bg_dark,
                "gutter cap at column {x}"
            );
        }
        for x in scrollbar.right()..area.right() {
            assert_eq!(
                buffer.cell((x, y)).expect("overview cap cell").bg,
                app.colors.bg_editor,
                "overview cap at column {x}"
            );
        }
    }

    #[test]
    fn stale_unsaved_diff_never_paints_the_shifted_line_suffix() {
        let saved = ropey::Rope::from_str(
            "line 0\nline 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\n",
        );
        let mut editor = Editor::test_with_text(
            "line 0\ninserted\nline 1\nline 2\nline 3\nline 4\nline 5\nline 6\nline 7\nline 8\n",
        );
        editor.file_path = Some(std::path::PathBuf::from("fixture.txt"));
        editor.saved_rope = Some(saved);
        editor.modified = true;
        editor.content_version = 1;
        let diff = crate::editor::compute_unsaved_diff_markers(
            editor.saved_rope.as_ref().expect("saved baseline"),
            &editor.rope,
        );
        editor.apply_unsaved_diff_cache(editor.content_version, diff);

        // Make the cache stale without changing the line count. The earlier
        // inserted line means a naive same-index comparison paints every later
        // gutter row yellow until the background diff completes.
        editor.cursor_line = 7;
        editor.cursor_col = 0;
        editor.insert_char('x');

        let mut app = App::test_new();
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.show_line_numbers = true;
        app.show_minimap = false;
        app.sticky_scroll = false;

        let area = Rect::new(0, 0, 60, 12);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| draw_editor(frame, &mut app, area))
            .expect("editor draw");

        let buffer = terminal.backend().buffer();
        let gutter = app.gutter_area.expect("gutter area");
        let marker_rows: Vec<u16> = (gutter.top()..gutter.bottom())
            .filter(|&y| {
                buffer
                    .cell((gutter.x, y))
                    .is_some_and(|cell| cell.symbol() == "┃")
            })
            .collect();

        assert_eq!(marker_rows, vec![gutter.y + 1, gutter.y + 7]);
    }

    #[test]
    fn folded_and_wrapped_rendering_seed_each_logical_line_from_source_depth() {
        for word_wrap in [false, true] {
            let mut app = App::test_new();
            let mut editor = Editor::test_with_text(
                "outer {\n  fn hidden() {\n    let marker = \"{\";\n  }\n  visible();\n}\n",
            );
            editor.word_wrap = word_wrap;
            editor.cursor_line = 4;
            let index = std::sync::Arc::new(crate::background::BracketDepthIndex::build(
                editor.rope.clone(),
                editor.navigation_id,
                editor.content_version,
                editor.wrap_revision,
            ));
            app.cached_bracket_depth_index = Some(index);
            app.tabs.push(Tab::new_file(editor));
            app.active_tab = 0;
            app.cursor_visible = false;
            app.show_line_numbers = false;
            app.show_minimap = false;
            app.show_indent_guides = false;
            app.sticky_scroll = false;
            app.bracket_pair_colorization = true;
            let expected = app.colors.ui.bracket_color(1).expect("palette color");
            for folded in [false, true] {
                if folded {
                    app.editor_mut().unwrap().folded_ranges.insert(1, 3);
                }
                let area = Rect::new(0, 0, 20, 10);
                let mut terminal =
                    Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
                terminal
                    .draw(|frame| draw_editor(frame, &mut app, area))
                    .unwrap();
                let closing = terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .filter(|cell| cell.symbol() == "}")
                    .last();
                assert_eq!(
                    closing.map(|cell| cell.fg),
                    Some(expected),
                    "word_wrap={word_wrap}, folded={folded}"
                );
            }
        }
    }

    #[test]
    fn bracket_guides_remain_visible_while_same_editor_analysis_is_stale() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text("root {\n    first\n    second\n}\n");
        editor.file_path = Some(std::path::PathBuf::from("fixture.rs"));
        editor.content_version = 8;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.show_line_numbers = false;
        app.show_minimap = false;
        app.show_indent_guides = false;
        app.sticky_scroll = false;
        app.bracket_pair_colorization = true;
        app.cached_bracket_analysis_target = Some(crate::background::BracketAnalysisTarget {
            navigation_id: app.editor().unwrap().navigation_id,
            editor: crate::background::EditorAnalysisTarget {
                active_group: app.active_group,
                active_tab: app.active_tab,
                file_path: Some(std::path::PathBuf::from("fixture.rs")),
                content_version: 7,
                len_bytes: 29,
                modified: true,
                disk_modified: None,
            },
            first_line: 0,
            visible_lines: 50,
        });
        app.cached_bracket_guides = vec![(0, 1, 3, 1)].into();

        let area = Rect::new(0, 0, 30, 5);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| draw_editor(frame, &mut app, area))
            .expect("editor draw");

        let code = app.code_area.expect("code area");
        let buffer = terminal.backend().buffer();
        assert_eq!(
            buffer
                .cell((code.x, code.y + 1))
                .expect("first guide cell")
                .symbol(),
            "│"
        );
        assert_eq!(
            buffer
                .cell((code.x, code.y + 2))
                .expect("second guide cell")
                .symbol(),
            "│"
        );
    }

    #[test]
    fn sticky_headers_do_not_highlight_the_following_editor_row() {
        for scope_count in [1, 3] {
            for cursor_after_headers in [0, 2] {
                let mut app = App::test_new();
                app.mouse_col = u16::MAX;
                app.mouse_row = u16::MAX;
                let source = format!(
                    "fn outer() {{\n    if ready {{\n        while active {{\n{}",
                    "            work();\n".repeat(20),
                );
                let mut editor = Editor::test_with_text(&source);
                editor.file_path = Some(std::path::PathBuf::from("sticky.rs"));
                editor.scroll_offset = 5;
                editor.cursor_line = 5 + scope_count + cursor_after_headers;
                app.tabs.push(Tab::new_file(editor));
                app.active_tab = 0;
                app.show_line_numbers = true;
                app.show_minimap = false;
                app.rulers.clear();
                let theme = crate::vscode_theme::customized_theme(&crate::app_theme::load_builtin_themes().remove(0),
                    &std::collections::BTreeMap::from([
                        ("editor.background".into(), serde_json::json!("#0a141e")),
                        ("editorStickyScroll.background".into(), serde_json::json!("#28323c")),
                        ("editor.lineHighlightBackground".into(), serde_json::json!("#46505a")),
                    ]));
                app.colors = theme.to_theme_colors();
                app.cached_sticky_scopes = (0..scope_count)
                    .map(|line| crate::background::StickyScopeLine {
                        line,
                        parent: line.checked_sub(1),
                    })
                    .collect();

                let area = Rect::new(0, 0, 70, 12);
                let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
                app.sticky_scroll = false;
                terminal.draw(|frame| draw_editor(frame, &mut app, area)).unwrap();
                let ordinary = terminal.backend().buffer().clone();
                app.sticky_scroll = true;
                terminal.draw(|frame| draw_editor(frame, &mut app, area)).unwrap();
                let buffer = terminal.backend().buffer();
                let code = app.code_area.unwrap();
                let gutter = app.gutter_area.unwrap();
                assert_eq!(app.cached_sticky_lines.len(), scope_count);
                for y in code.y..code.y + scope_count as u16 {
                    assert_eq!(buffer[(code.x, y)].bg, app.colors.bg_sticky_header);
                }
                // Pinned headers must not repaint any ordinary editor row,
                // including the real cursor row directly below the headers.
                for y in code.y + scope_count as u16..code.bottom() {
                    for x in gutter.x..code.right() {
                        assert_eq!(buffer[(x, y)], ordinary[(x, y)], "cell {x},{y}");
                    }
                }
                let cursor_y = code.y + (scope_count + cursor_after_headers) as u16;
                assert_eq!(buffer[(code.right() - 1, cursor_y)].bg, app.colors.bg_line_highlight);
            }
        }
    }

    #[test]
    fn sticky_header_stays_stable_until_new_scope_analysis_arrives() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(
            "fn outer() {\n    first();\n    second();\n    third();\n    fourth();\n}\n",
        );
        editor.file_path = Some(std::path::PathBuf::from("fixture.rs"));
        editor.cursor_line = 4;
        editor.scroll_offset = 3;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.show_line_numbers = false;
        app.show_minimap = false;
        app.sticky_scroll = true;
        app.cached_sticky_scopes = vec![crate::background::StickyScopeLine {
            line: 0,
            parent: None,
        }].into();

        let area = Rect::new(0, 0, 50, 6);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| draw_editor(frame, &mut app, area))
            .expect("initial editor draw");
        assert_eq!(
            app.cached_sticky_lines,
            vec![(0, "fn outer() {".to_string())]
        );

        // Model a line insertion above the viewport while the coalesced scope
        // worker still holds the previous absolute line coordinates.
        let editor = app.editor_mut().unwrap();
        editor.rope.insert(0, "\n");
        editor.content_version = editor.content_version.wrapping_add(1);
        editor.mark_syntax_dirty_range(0, 1);
        terminal
            .draw(|frame| draw_editor(frame, &mut app, area))
            .expect("post-edit editor draw");

        assert_eq!(
            app.cached_sticky_lines,
            vec![(0, "fn outer() {".to_string())],
            "the previous header should remain visible until new scope coordinates are ready"
        );
    }

    #[test]
    fn split_line_keeps_its_rendered_colors_in_the_immediate_edit_frame() {
        let mut app = App::test_new();
        let path = std::path::PathBuf::from("immediate-frame.rs");
        let source: String = (0..10)
            .map(|line| format!("let value_{line} = {line};\n"))
            .collect();
        let mut editor = Editor::test_with_text(&source);
        editor.file_path = Some(path.clone());
        editor.cursor_line = 5;
        editor.cursor_col = 13;
        let rope = editor.rope.clone();
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.cursor_visible = false;
        app.show_line_numbers = false;
        app.show_minimap = false;
        app.sticky_scroll = false;
        app.bracket_pair_colorization = false;

        let job = app
            .highlighter
            .prepare_job(
                &rope,
                &path,
                0,
                None,
                0..10,
                None,
                app.syntax_theme.as_str(),
            )
            .unwrap();
        let (syntax_set, theme_set) = app.highlighter.shared_sets();
        let batch = job.run(&rope, &syntax_set, &theme_set, || false);
        assert!(app.highlighter.apply_batch(batch));

        let area = Rect::new(0, 0, 50, 12);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| draw_editor(frame, &mut app, area))
            .expect("initial editor draw");
        let code = app.code_area.expect("code area");
        let before: Vec<Color> = (0..12)
            .map(|col| {
                terminal
                    .backend()
                    .buffer()
                    .cell((code.x + col, code.y + 5))
                    .unwrap()
                    .fg
            })
            .collect();

        let editor = app.editor_mut().unwrap();
        editor.insert_newline();
        editor.mark_syntax_dirty_range(5, 6);
        let version = editor.content_version;
        let edited = editor.rope.clone();
        let pending = app
            .highlighter
            .prepare_job(
                &edited,
                &path,
                version,
                Some((5, 6)),
                0..11,
                None,
                app.syntax_theme.as_str(),
            )
            .unwrap();
        drop(pending);
        terminal
            .draw(|frame| draw_editor(frame, &mut app, area))
            .expect("immediate post-edit draw");
        // Splitting the line can scroll to keep the new caret visible. Compare
        // the same source line, rather than a different row in the viewport.
        let source_row_y = code.y + 5 - app.editor().unwrap().scroll_offset as u16;
        let after: Vec<Color> = (0..12)
            .map(|col| {
                terminal
                    .backend()
                    .buffer()
                    .cell((code.x + col, source_row_y))
                    .unwrap()
                    .fg
            })
            .collect();

        assert_eq!(after, before);
    }

    #[test]
    fn editor_and_diff_layouts_render_fractional_change_markers_with_and_without_minimaps() {
        for (mode, unsaved, show_minimap) in [
            (crate::app::DiffMode::Off, false, false),
            (crate::app::DiffMode::Off, true, false),
            (crate::app::DiffMode::Inline, false, false),
            (crate::app::DiffMode::SideBySide, false, false),
            (crate::app::DiffMode::Inline, false, true),
            (crate::app::DiffMode::SideBySide, false, true),
        ] {
            let base: String = (0..200)
                .map(|line| format!("original line {line}\n"))
                .collect();
            let target: String = (0..200)
                .map(|line| {
                    if [70, 78].contains(&line) {
                        format!("changed line {line}\n")
                    } else {
                        format!("original line {line}\n")
                    }
                })
                .collect();
            let mut app = App::test_new();
            let mut editor = Editor::test_with_text(&target);
            editor.file_path = Some(std::path::PathBuf::from("fractional.rs"));
            editor.saved_rope = Some(editor.rope.clone());
            editor.set_git_line_status(
                (0..editor.line_count())
                    .map(|line| if [70, 78].contains(&line) { 'U' } else { ' ' })
                    .collect(),
            );
            if unsaved {
                editor.cursor_line = 70;
                editor.insert_char('X');
                let diff = crate::editor::compute_unsaved_diff_markers(
                    editor.saved_rope.as_ref().unwrap(), &editor.rope);
                editor.apply_unsaved_diff_cache(
                    editor.content_version, diff);
                editor.cursor_line = 0;
                editor.cursor_col = 0;
            }
            app.tabs.push(Tab::new_file(editor));
            app.active_tab = 0;
            app.diff_mode = mode;
            app.show_minimap = show_minimap;
            app.sticky_scroll = false;
            if mode != crate::app::DiffMode::Off {
                app.compare_base_content = Some(base.clone().into());
                app.set_compare_diff_lines(crate::app::compute_file_diff(&base, &target));
            }
            let mut terminal = Terminal::new(TestBackend::new(120, 20)).unwrap();
            terminal
                .draw(|frame| match mode {
                    crate::app::DiffMode::Inline => crate::ui_diff_display::draw_inline_diff(
                        frame,
                        &mut app,
                        Rect::new(0, 0, 120, 20),
                    ),
                    crate::app::DiffMode::SideBySide => {
                        crate::ui_diff_display::draw_side_by_side_diff(
                            frame,
                            &mut app,
                            Rect::new(0, 0, 120, 20),
                        )
                    }
                    _ => draw_editor(frame, &mut app, Rect::new(0, 0, 120, 20)),
                })
                .unwrap();
            let bar = app.scrollbar_area.expect("change scrollbar");
            let color = if unsaved {
                app.colors.ui.fg_overview_added
            } else {
                app.colors.ui.fg_overview_modified
            };
            assert!(
                (bar.y..bar.bottom()).any(|y| {
                    (bar.x..bar.right()).any(|x| {
                        let cell = &terminal.backend().buffer()[(x, y)];
                        cell.fg == color && cell.symbol() != "█" && cell.symbol() != " "
                    })
                }),
                "layout={}, unsaved={unsaved}, minimap={show_minimap}",
                mode as u8
            );
            if !show_minimap { assert!(app.minimap_area.is_none()); }
        }
    }

    #[test]
    fn side_by_side_diff_marks_minimap_and_scrollbar_from_cached_changes() {
        let mut base = String::new();
        let mut target = String::new();
        for line in 0..100 {
            if line == 70 {
                base.push_str("old value\n");
                target.push_str("new value\n");
            } else {
                let text = format!("line {line}\n");
                base.push_str(&text);
                target.push_str(&text);
            }
        }

        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(&target);
        editor.file_path = Some(std::path::PathBuf::from("fixture.cpp"));
        editor.cursor_line = 65;
        editor.scroll_offset = 65;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.diff_mode = crate::app::DiffMode::SideBySide;
        app.compare_base_content = Some(base.clone().into());
        app.set_compare_diff_lines(crate::app::compute_file_diff(&base, &target));
        app.editor_mut()
            .expect("active editor")
            .clear_git_line_status();
        app.show_minimap = true;
        app.sticky_scroll = false;

        let mut terminal = Terminal::new(TestBackend::new(120, 20)).expect("test terminal");
        terminal
            .draw(|frame| draw_editor(frame, &mut app, Rect::new(0, 0, 120, 20)))
            .expect("editor draw");

        let buffer = terminal.backend().buffer();
        let minimap_area = app.minimap_area.expect("minimap area");
        let scrollbar_area = app.scrollbar_area.expect("scrollbar area");
        let has_modified_marker = |area: Rect, foreground: Color| {
            (area.top()..area.bottom()).any(|y| {
                (area.left()..area.right()).any(|x| {
                    buffer
                        .cell((x, y))
                        .is_some_and(|cell| cell.fg == foreground)
                })
            })
        };

        assert!(has_modified_marker(minimap_area, app.colors.ui.fg_minimap_modified));
        assert!(has_modified_marker(scrollbar_area, app.colors.ui.fg_overview_modified));
    }

    #[test]
    fn large_file_renderer_keeps_line_numbers_and_minimap() {
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(&"short line\n".repeat(200));
        editor.file_path = Some(std::path::PathBuf::from("large.txt"));
        editor.file_size = 100 * 1024 * 1024;
        editor.large_file_mode = true;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.show_line_numbers = true;
        app.show_minimap = true;

        let area = Rect::new(0, 0, 120, 20);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| draw_editor(frame, &mut app, area))
            .expect("large editor draw");

        assert!(app.minimap_area.is_some());
        let gutter = app.gutter_area.expect("gutter");
        assert!((gutter.left()..gutter.right()).any(|x| {
            terminal
                .backend()
                .buffer()
                .cell((x, gutter.y))
                .is_some_and(|cell| cell.symbol() == "1")
        }));
    }

    #[test]
    fn large_file_renderer_slices_a_long_line_at_horizontal_scroll() {
        let mut text = "x".repeat(20_000);
        text.push_str("TAIL\n");
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(&text);
        editor.file_path = Some(std::path::PathBuf::from("large-line.txt"));
        editor.file_size = 100 * 1024 * 1024;
        editor.large_file_mode = true;
        editor.horizontal_scroll = 20_000;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.show_line_numbers = false;
        app.show_minimap = false;

        let area = Rect::new(0, 0, 40, 3);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| draw_editor(frame, &mut app, area))
            .expect("large editor draw");

        let code = app.code_area.expect("code area");
        let rendered: String = (code.left()..code.right())
            .filter_map(|x| terminal.backend().buffer().cell((x, code.y)))
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.starts_with("TAIL"));
    }

    #[test]
    fn pathological_line_uses_bounded_renderer_below_large_file_threshold() {
        let mut text = "x".repeat(MAX_RICH_RENDER_LINE_CHARS + 32);
        text.push_str("TAIL\n");
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(&text);
        editor.file_path = Some(std::path::PathBuf::from("minified.js"));
        editor.horizontal_scroll = MAX_RICH_RENDER_LINE_CHARS + 32;
        assert!(!editor.large_file_mode);
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.show_line_numbers = false;
        app.show_minimap = false;

        let area = Rect::new(0, 0, 40, 3);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| draw_editor(frame, &mut app, area))
            .expect("bounded editor draw");

        let code = app.code_area.expect("code area");
        let rendered: String = (code.left()..code.right())
            .filter_map(|x| terminal.backend().buffer().cell((x, code.y)))
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.starts_with("TAIL"));
    }

    #[test]
    fn overview_change_rows_are_reused_until_markers_change() {
        let mut app = App::test_new();
        let text = format!("{}line", "line\n".repeat(99));
        let mut editor = Editor::test_with_text(&text);
        editor.file_path = Some(std::path::PathBuf::from("changes.txt"));
        editor.set_git_line_status(
            (0..editor.line_count())
                .map(|line| if line == 50 { 'U' } else { ' ' })
                .collect(),
        );
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;

        let first = cached_overview_change_rows(&mut app, 10, None);
        let second = cached_overview_change_rows(&mut app, 10, None);
        assert!(std::sync::Arc::ptr_eq(&first, &second));
        assert_eq!(first.row(5).status(), Some('U'));

        app.editor_mut()
            .unwrap()
            .set_git_line_status(vec![' '; 101]);
        let refreshed = cached_overview_change_rows(&mut app, 10, None);
        assert!(!std::sync::Arc::ptr_eq(&first, &refreshed));
        assert!(refreshed.is_empty());
    }

    #[test]
    fn dense_unsaved_overview_is_reused_across_large_document_edits() {
        let mut app = App::test_new();
        let text = format!("{}line", "line\n".repeat(49_999));
        let mut editor = Editor::test_with_text(&text);
        editor.file_path = Some(std::path::PathBuf::from("dense-changes.txt"));
        editor.saved_rope = Some(editor.rope.clone());
        editor.modified = true;
        let line_count = editor.line_count();
        editor.apply_unsaved_diff_cache(
            editor.content_version,
            crate::editor::UnsavedDiff { line_count, changed_ranges: vec![0..line_count] },
        );
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;

        let before_edit = cached_overview_change_rows(&mut app, 100, None);
        assert!((0..100).all(|row| before_edit.row(row).status().is_some()));

        app.editor_mut().unwrap().insert_char('x');
        let after_edit = cached_overview_change_rows(&mut app, 100, None);

        // Typing changes the content version, but not the applied marker
        // payload. Reuse the already bucketed rows until the worker publishes
        // a replacement instead of scanning all 50k markers on the UI thread.
        assert!(std::sync::Arc::ptr_eq(&before_edit, &after_edit));
    }

    #[test]
    fn large_file_renderer_uses_cached_textmate_foregrounds() {
        let mut app = App::test_new();
        let path = std::path::PathBuf::from("large.rs");
        let mut editor = Editor::test_with_text("let answer = 42;\n");
        editor.file_path = Some(path.clone());
        editor.file_size = 100 * 1024 * 1024;
        editor.large_file_mode = true;
        let rope = editor.rope.clone();
        let version = editor.content_version;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.show_line_numbers = false;
        app.show_minimap = false;
        app.cursor_visible = false;

        let job = app
            .highlighter
            .prepare_job(
                &rope,
                &path,
                version,
                None,
                0..1,
                None,
                app.syntax_theme.as_str(),
            )
            .unwrap();
        let (syntax_set, theme_set) = app.highlighter.shared_sets();
        let batch = job.run(&rope, &syntax_set, &theme_set, || false);
        assert!(app.highlighter.apply_batch(batch));
        let (cached, _) = app
            .highlighter
            .highlighted_line_by_index_if_cached(
                0,
                Some(&path),
                None,
                app.syntax_theme.as_str(),
                version,
            )
            .unwrap();
        let expected = cached
            .iter()
            .find(|(_, text)| text.contains("let"))
            .map(|(style, _)| {
                Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b)
            })
            .unwrap();
        assert_ne!(expected, app.colors.fg_text);

        let area = Rect::new(0, 0, 40, 3);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| draw_editor(frame, &mut app, area))
            .expect("large editor draw");

        let code = app.code_area.expect("code area");
        assert_eq!(
            terminal
                .backend()
                .buffer()
                .cell((code.x, code.y))
                .unwrap()
                .fg,
            expected
        );
    }

    /// Reproducible local probe for the release-critical 50k-line interaction
    /// path. It is intentionally ignored in the ordinary suite because it
    /// reports timings rather than enforcing machine-dependent thresholds.
    #[test]
    #[ignore = "manual large-file performance probe"]
    fn profile_50k_line_editor_hot_paths() {
        let source = "def calculate(value): return value * 2  # representative source line\n"
            .repeat(50_000);
        let mut app = App::test_new();
        let mut editor = Editor::test_with_text(&source);
        editor.file_path = Some(std::path::PathBuf::from("profile.py"));
        editor.cursor_line = 49_999;
        editor.cursor_col = 12;
        editor.scroll_offset = 49_940;
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;

        let area = Rect::new(0, 0, 180, 60);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| draw_editor(frame, &mut app, area))
            .expect("warm editor draw");

        let cursor_started = std::time::Instant::now();
        for _ in 0..10_000 {
            let editor = app.editor_mut().expect("editor");
            editor.move_up();
            editor.move_down();
        }
        let cursor_elapsed = cursor_started.elapsed();

        let draw_started = std::time::Instant::now();
        for _ in 0..20 {
            terminal
                .draw(|frame| draw_editor(frame, &mut app, area))
                .expect("editor draw");
        }
        let draw_elapsed = draw_started.elapsed();

        let edit_started = std::time::Instant::now();
        for _ in 0..20 {
            let editor = app.editor_mut().expect("editor");
            editor.insert_char('x');
            editor.backspace();
            terminal
                .draw(|frame| draw_editor(frame, &mut app, area))
                .expect("post-edit draw");
        }
        let edit_elapsed = edit_started.elapsed();

        let mut full_terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("full test terminal");
        full_terminal
            .draw(|frame| crate::ui::draw(frame, &mut app))
            .expect("warm full UI draw");
        let full_draw_started = std::time::Instant::now();
        for _ in 0..20 {
            full_terminal
                .draw(|frame| crate::ui::draw(frame, &mut app))
                .expect("full UI draw");
        }
        let full_draw_elapsed = full_draw_started.elapsed();

        eprintln!(
            "50k probe: cursor pair {:.3} us, editor draw {:.3} ms, edit+draw {:.3} ms, full UI draw {:.3} ms",
            cursor_elapsed.as_secs_f64() * 1_000_000.0 / 10_000.0,
            draw_elapsed.as_secs_f64() * 1_000.0 / 20.0,
            edit_elapsed.as_secs_f64() * 1_000.0 / 20.0,
            full_draw_elapsed.as_secs_f64() * 1_000.0 / 20.0,
        );
        assert_eq!(app.editor().expect("editor").line_count(), 50_001);
    }
}
