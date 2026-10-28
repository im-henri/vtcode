use std::fmt::Write as FmtWrite;

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::ui_editor_highlight::{draw_horizontal_scrollbar, draw_minimap_separator, HScrollbarParams};
use crate::ui_editor::refresh_cached_editor_diagnostics;
use crate::ui_editor_minimap::{
    MinimapParams, ScrollbarParams, draw_mapped_scrollbar, draw_minimap,
};

#[cfg(test)]
#[path = "ui_diff_cursor_tests.rs"]
mod cursor_tests;

#[cfg(test)]
#[path = "ui_diff_selection_tests.rs"]
mod selection_tests;

#[cfg(test)]
#[path = "ui_diff_scope_tests.rs"]
mod scope_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SideBySideDiffLayout {
    pub(crate) left_header: Rect,
    pub(crate) left: Rect,
    pub(crate) divider: Rect,
    pub(crate) right_header: Rect,
    pub(crate) right: Rect,
}

pub(crate) fn side_by_side_diff_layout(area: Rect, ratio: f32) -> SideBySideDiffLayout {
    let divider_width = u16::from(area.width >= 3);
    let available = area.width.saturating_sub(divider_width);
    let min_pane = if available >= 24 {
        12
    } else {
        u16::from(available >= 2)
    };
    let desired = (f32::from(available) * ratio.clamp(0.0, 1.0)).round() as u16;
    let max_left = available.saturating_sub(min_pane);
    let left_width = desired.clamp(min_pane.min(max_left), max_left.max(min_pane));
    let right_width = available.saturating_sub(left_width);
    let header_height = area.height.min(1);
    let content_y = area.y.saturating_add(header_height);
    let content_height = area.height.saturating_sub(header_height);
    let left_header = Rect::new(area.x, area.y, left_width, header_height);
    let left = Rect::new(area.x, content_y, left_width, content_height);
    let divider = Rect::new(
        area.x.saturating_add(left_width),
        area.y,
        divider_width,
        area.height,
    );
    let right_header = Rect::new(
        divider.x.saturating_add(divider_width),
        area.y,
        right_width,
        header_height,
    );
    let right = Rect::new(right_header.x, content_y, right_width, content_height);
    SideBySideDiffLayout {
        left_header,
        left,
        divider,
        right_header,
        right,
    }
}

fn diff_labels(app: &App) -> (&str, &str) {
    app.diff_labels
        .as_ref()
        .map_or(("Original", "Modified"), |labels| {
            (labels.base.as_str(), labels.target.as_str())
        })
}

fn draw_diff_header(f: &mut Frame, app: &App, area: Rect, label: &str, unsaved: bool) {
    let style = Style::default()
        .fg(app.colors.fg_text)
        .bg(app.colors.bg_dark)
        .add_modifier(Modifier::BOLD)
        .remove_modifier(Modifier::DIM);
    let mut spans = vec![Span::raw(" "), Span::raw(label)];
    if unsaved {
        spans.push(Span::raw(" (unsaved)"));
    }
    f.render_widget(Paragraph::new(Line::from(spans)).style(style), area);
}

pub(crate) fn draw_side_by_side_diff(f: &mut Frame, app: &mut App, area: Rect) {
    if crate::ui_animation_frame::begin_diff(f, app, area, true) {
        return;
    }
    let layout = side_by_side_diff_layout(area, app.diff_split_ratio);
    app.diff_split_area = Some(area);
    app.diff_divider_area = (layout.divider.width > 0).then_some(layout.divider);
    app.editor_area = Some(layout.right);

    let (base, target) = diff_labels(app);
    let unsaved = app
        .editor()
        .is_some_and(|editor| editor.modified && !editor.read_only);
    draw_diff_header(f, app, layout.left_header, base, false);
    draw_diff_header(f, app, layout.right_header, target, unsaved);

    if app.context_diff_active() || crate::commit_review::painting_row(app).is_some() {
        crate::diff_context::draw_compact_side_by_side(f, app, area);
    } else {
        let word_wrap = app.editor().is_some_and(|editor| editor.word_wrap);
        let cursor_visible = app.cursor_visible;
        if app.diff_selection_owns_cursor() {
            app.cursor_visible = false;
        }
        if let Some(editor) = app.editor_mut() {
            editor.word_wrap = false;
            editor.diff_display_rows = None;
        }
        crate::ui_editor::draw_editor(f, app, layout.right);
        app.cursor_visible = cursor_visible;
        if let Some(editor) = app.editor_mut() {
            editor.word_wrap = word_wrap;
        }
        layout_aligned_diff_scroll(app, area);
        draw_diff_original(f, app, layout.left);
        draw_diff_target_overlay(f, app);
        crate::diff_context::map_visible_rows(app, true);
        crate::diff_context::draw_scrollbar(f, app);
        crate::diff_context::draw_search_matches(f, app);
    }

    // Draw the divider last. Besides being the resize affordance, this is a
    // hard visual boundary that prevents either pane's diff style leaking.
    if layout.divider.width > 0 {
        let border = if app.resizing_diff_split {
            Some(app.colors.fg_accent)
        } else {
            app.colors.ui.border_diff
        };
        let buffer = f.buffer_mut();
        for y in layout.divider.top()..layout.divider.bottom() {
            if let Some(cell) = buffer.cell_mut((layout.divider.x, y)) {
                cell.set_symbol(if border.is_some() { "│" } else { " " });
                let color = border.unwrap_or(app.colors.bg_editor);
                cell.set_fg(color);
                cell.set_bg(app.colors.bg_editor);
                cell.set_style(Style::default().fg(color).bg(app.colors.bg_editor));
            }
        }
    }
    crate::ui_animation_frame::finish_diff(f, app, area, true);
}

/// Keep an exact aligned row anchor even when many original rows map to the
/// same target line. Both full and compact comparisons use this scroll state.
pub(crate) fn layout_aligned_diff_scroll(app: &mut App, area: Rect) {
    let layout = side_by_side_diff_layout(area, app.diff_split_ratio);
    let mut scroll = std::mem::take(&mut app.diff_inline_scroll);
    if !scroll.aligned {
        scroll = Default::default();
        scroll.aligned = true;
    }
    scroll.area = area;
    scroll.text_area = app.code_area.unwrap_or_default();
    // Either side can reserve a horizontal scrollbar independently. Retain
    // enough vertical travel to expose the last source row on the shorter side.
    let left_gutter = digit_count(app.diff_render_cache.max_left_line).max(2) + 1;
    let left_width = (layout.left.width as usize).saturating_sub(left_gutter + 3);
    let left_height = layout.left.height.saturating_sub(u16::from(
        app.diff_render_cache.max_left_text_width > left_width && layout.left.height > 1,
    ));
    scroll.text_area.height = scroll.text_area.height.min(left_height);
    scroll.vertical_area = app.scrollbar_area.unwrap_or_default();
    scroll.horizontal_area = app.h_scrollbar_area;
    scroll.original_terminal = app.diff_render_cache.context_lines.is_none()
        && app
            .compare_base_content
            .as_ref()
            .is_some_and(|base| base.is_empty() || base.ends_with(['\n', '\r']));
    let cache = std::sync::Arc::clone(&app.diff_render_cache);
    let review_row = crate::commit_review::painting_row(app);
    if let Some(editor) = app.editor_mut() {
        if let Some(row) = review_row { scroll.layout_review(cache, editor, row); }
        else { scroll.layout(cache, editor); }
    }
    app.diff_inline_scroll = scroll;
}

/// All comparisons finish with the same caret, selection, and animation layers.
pub(crate) fn draw_diff_layers(frame: &mut Frame, app: &mut App, aligned: bool) {
    crate::ui_editor_animations::suspend_for_multiple_cursors(app);
    let id = crate::diff_scope_source::animation_view_id(app);
    let show_cursor = crate::ui_diff_animations::cursor_effects_allowed(app)
        && (app.cursor_visible
            || (app.terminal_has_focus
                && id.is_some_and(|id| {
                    app.animations.cursor_smear_active(
                        id,
                        &app.animation_settings,
                        std::time::Instant::now(),
                    )
                })));
    let primary = crate::diff_selection::draw(frame, app, aligned, show_cursor);
    let primary = draw_diff_cursor(frame, app, aligned, show_cursor).or(primary);
    crate::ui_diff_animations::draw(frame, app, aligned, primary);
}

/// Project only the visible window of the target row. Inline diffs scroll in
/// terminal cells; aligned diffs scroll in source characters. Their immutable
/// text indexes already provide the matching window without scanning a prefix.
fn draw_diff_cursor(
    f: &mut Frame,
    app: &App,
    aligned: bool,
    show_cursor: bool,
) -> Option<crate::ui_editor_animations::CursorUnderlay> {
    if !show_cursor
        || app.rendering_inactive_editor
        || app.status_details.open
        || app.diff_selection_owns_cursor()
        || (aligned && app.compare_diff_lines.is_empty())
    {
        return None;
    }
    let (Some(editor), Some(area)) = (app.editor(), app.code_area) else {
        return None;
    };
    let cache = &app.diff_render_cache;
    let inset = if aligned { 2 } else { 0 };
    let width = (area.width as usize).saturating_sub(inset);
    if width == 0 {
        return None;
    }
    let row_scroll = if aligned {
        app.aligned_diff_scroll_row(editor.scroll_offset)
    } else {
        app.diff_inline_scroll.row
    };
    // Scroll anchors can include deletions or hidden-context markers. Locate
    // the actual target among only painted rows, without scanning the diff.
    let Some((screen_row, row)) = (0..area.height as usize).find_map(|offset| {
        let index = row_scroll.checked_add(offset)?;
        let row = if aligned {
            cache.rows.get(index)?
        } else {
            let entry = cache.inline_rows.get(index).filter(|row| row.kind != '-')?;
            cache.rows.get(entry.aligned_row)?
        };
        (row.hidden_lines == 0 && row.right_line == Some(editor.cursor_line))
            .then_some((offset, row))
    }) else {
        // Diff lines omit the terminal empty Rope line. Keep that real source
        // position navigable without inventing a diff hunk or changing text.
        if cache.context_lines.is_none()
            && editor.cursor_line == cache.target_content_lines()
            && editor.cursor_line + 1 == editor.line_count()
            && editor.rope.line(editor.cursor_line).len_chars() == 0
            && editor.horizontal_scroll == 0
        {
            let terminal_row = if aligned {
                cache.rows.len()
            } else {
                cache.inline_rows.len()
            };
            if let Some(offset) = terminal_row
                .checked_sub(row_scroll)
                .filter(|offset| *offset < area.height as usize)
            {
                return Some(paint_diff_cursor(
                    f,
                    app,
                    area.x + inset as u16,
                    area.y + offset as u16,
                    1,
                ));
            }
        }
        return None;
    };
    let (text, char_start, padding) = if aligned {
        (
            row.right_text_window(editor.horizontal_scroll, width),
            editor.horizontal_scroll,
            0,
        )
    } else {
        if app.diff_inline_scroll.column > row.right_cell_len() {
            return None;
        }
        row.right_inline_text_window(app.diff_inline_scroll.column, width)
    };
    let cursor = editor.cursor_col.min(row.right_char_len());
    let mut source_col = char_start;
    let mut cell_col = padding;
    if cursor < source_col {
        return None;
    }
    let mut caret = None;
    for grapheme in text.graphemes(true) {
        let end = source_col + grapheme.chars().count();
        if grapheme.contains(char::is_control) {
            source_col = end;
            continue;
        }
        if cursor < end {
            caret = Some((cell_col, grapheme.width().max(1)));
            break;
        }
        source_col = end;
        cell_col += grapheme.width();
    }
    if caret.is_none() && cursor <= source_col && source_col == row.right_char_len() {
        caret = Some((cell_col, 1));
    }
    let (column, cells) = caret.filter(|(column, cells)| column + cells <= width)?;
    let y = area.y + screen_row as u16;
    let x = area.x + (inset + column) as u16;
    Some(paint_diff_cursor(f, app, x, y, cells))
}

pub(crate) fn paint_diff_cursor(
    f: &mut Frame,
    app: &App,
    x: u16,
    y: u16,
    cells: usize,
) -> crate::ui_editor_animations::CursorUnderlay {
    let buffer = f.buffer_mut();
    let underlay = (Rect::new(x, y, cells as u16, 1), buffer[(x, y)].style());
    let style = crate::ui_colors::block_cursor_style(
        app.colors.bg_cursor,
        app.colors.fg_cursor,
        buffer[(x, y)].bg,
        app.colors.ui.contrast_colors(),
    );
    for offset in 0..cells as u16 {
        buffer[(x + offset, y)].set_style(style);
    }
    underlay
}

fn digit_count(n: usize) -> usize {
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

/// Build a muted, outlined diagonal hatch for the side of a side-by-side diff
/// that has no corresponding source line. Contiguous spacer rows share one
/// border, and the phase changes per row to keep the interior visually shaded.
fn diff_spacer_pattern(width: usize, row: usize, starts_block: bool, ends_block: bool) -> String {
    const STRIDE: usize = 4;
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        let edge = match (starts_block, ends_block) {
            (true, true) => '╬',
            (true, false) => '┬',
            (false, true) => '┴',
            (false, false) => '│',
        };
        return edge.to_string();
    }

    let phase = row % STRIDE;
    let mut pattern = String::with_capacity(width.saturating_mul('╱'.len_utf8()));
    if starts_block && ends_block {
        pattern.push('╞');
        pattern.extend(std::iter::repeat_n('═', width - 2));
        pattern.push('╡');
    } else if starts_block {
        pattern.push('┌');
        pattern.extend(std::iter::repeat_n('─', width - 2));
        pattern.push('┐');
    } else if ends_block {
        pattern.push('└');
        pattern.extend(std::iter::repeat_n('─', width - 2));
        pattern.push('┘');
    } else {
        pattern.push('│');
        for col in 1..width - 1 {
            pattern.push(if (col + phase).is_multiple_of(STRIDE) {
                '╱'
            } else {
                ' '
            });
        }
        pattern.push('│');
    }
    pattern
}

#[cfg(test)]
fn plain_diff_highlight_line(
    text: &str,
    foreground: Color,
) -> Vec<(crate::textmate::Style, String)> {
    let (r, g, b) = match foreground {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (212, 212, 212),
    };
    vec![(
        crate::textmate::Style {
            foreground: crate::textmate::Color { r, g, b, a: 255 },
            background: crate::textmate::Color {
                r: 0,
                g: 0,
                b: 0,
                a: 0,
            },
            font_style: crate::textmate::FontStyle::empty(),
        },
        text.to_string(),
    )]
}

const DIFF_HIGHLIGHT_SEEK_STRIDE: usize = 64;

struct DiffSpanIndex {
    char_end: usize,
    checkpoint_start: usize, // usize::MAX identifies an ASCII segment.
}

struct DiffHighlightIndex {
    spans: Vec<DiffSpanIndex>,
    bytes: Vec<usize>,
}

impl DiffHighlightIndex {
    fn new(highlighted: &crate::highlight::HighlightSpans) -> Self {
        let mut spans = Vec::with_capacity(highlighted.len());
        let mut bytes = Vec::new();
        let mut char_end = 0;
        for (_, text) in highlighted {
            let text = text.trim_end_matches('\n');
            let checkpoint_start = if text.is_ascii() {
                char_end += text.len();
                usize::MAX
            } else {
                let start = bytes.len();
                for (character, (byte, _)) in text.char_indices().enumerate() {
                    if character.is_multiple_of(DIFF_HIGHLIGHT_SEEK_STRIDE) {
                        bytes.push(byte);
                    }
                    char_end += 1;
                }
                start
            };
            spans.push(DiffSpanIndex {
                char_end,
                checkpoint_start,
            });
        }
        Self { spans, bytes }
    }

    fn char_len(&self) -> usize {
        self.spans.last().map_or(0, |span| span.char_end)
    }

    fn segment_tail<'a>(&self, segment: usize, text: &'a str, character: usize) -> &'a str {
        let text = text.trim_end_matches('\n');
        if character == 0 {
            return text;
        }
        let checkpoints = self.spans[segment].checkpoint_start;
        if checkpoints == usize::MAX {
            return &text[character.min(text.len())..];
        }
        let bucket = character / DIFF_HIGHLIGHT_SEEK_STRIDE;
        let checkpoint = self.bytes[checkpoints + bucket];
        let byte = text[checkpoint..]
            .char_indices()
            .nth(character % DIFF_HIGHLIGHT_SEEK_STRIDE)
            .map_or(text.len(), |(byte, _)| checkpoint + byte);
        &text[byte..]
    }

    fn storage_bytes(&self) -> usize {
        self.spans.capacity() * std::mem::size_of::<DiffSpanIndex>()
            + self.bytes.capacity() * std::mem::size_of::<usize>()
    }
}

struct DiffHighlightCacheEntry {
    source: std::rc::Weak<crate::highlight::HighlightSpans>,
    index: std::rc::Rc<DiffHighlightIndex>,
    used: u64,
}

#[derive(Default)]
struct DiffHighlightCache {
    entries: rustc_hash::FxHashMap<usize, DiffHighlightCacheEntry>,
    bytes: usize,
    used: u64,
}

impl DiffHighlightCache {
    const MAX_ENTRIES: usize = 256;
    const MAX_BYTES: usize = 2 * 1024 * 1024;

    fn get(
        &mut self,
        spans: &std::rc::Rc<crate::highlight::HighlightSpans>,
    ) -> std::rc::Rc<DiffHighlightIndex> {
        self.used = self.used.wrapping_add(1);
        let key = std::rc::Rc::as_ptr(spans) as usize;
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.used = self.used;
            return std::rc::Rc::clone(&entry.index);
        }
        let index = std::rc::Rc::new(DiffHighlightIndex::new(spans));
        if index.storage_bytes() > Self::MAX_BYTES {
            return index;
        }
        self.entries
            .retain(|_, entry| entry.source.strong_count() > 0);
        self.bytes = self
            .entries
            .values()
            .map(|entry| entry.index.storage_bytes())
            .sum();
        while self.entries.len() >= Self::MAX_ENTRIES
            || self.bytes + index.storage_bytes() > Self::MAX_BYTES
        {
            let key = *self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.used)
                .unwrap()
                .0;
            self.bytes -= self.entries.remove(&key).unwrap().index.storage_bytes();
        }
        self.bytes += index.storage_bytes();
        self.entries.insert(
            key,
            DiffHighlightCacheEntry {
                source: std::rc::Rc::downgrade(spans),
                index: std::rc::Rc::clone(&index),
                used: self.used,
            },
        );
        index
    }
}

thread_local! {
    static DIFF_HIGHLIGHT_CACHE: std::cell::RefCell<DiffHighlightCache> = Default::default();
    #[cfg(test)]
    static REFERENCE_DIFF_RENDER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn diff_highlight_index(
    spans: &std::rc::Rc<crate::highlight::HighlightSpans>,
) -> std::rc::Rc<DiffHighlightIndex> {
    DIFF_HIGHLIGHT_CACHE.with(|cache| cache.borrow_mut().get(spans))
}

#[cfg(test)]
fn normalize_diff_highlight_line(
    highlighted: &[(crate::textmate::Style, String)],
    text: &str,
    foreground: Color,
) -> Option<Vec<(crate::textmate::Style, String)>> {
    let expected_len = text.chars().count();
    let highlighted_len: usize = highlighted
        .iter()
        .map(|(_, segment)| segment.trim_end_matches('\n').chars().count())
        .sum();
    if highlighted_len < expected_len {
        Some(plain_diff_highlight_line(text, foreground))
    } else {
        None
    }
}

fn render_diff_highlighted_spans(
    highlighted: &[(crate::textmate::Style, String)],
    index: &DiffHighlightIndex,
    visible_width: usize,
    horizontal_scroll: usize,
    bg: Color,
    bg_highlight: Option<Color>,
    char_highlights: Option<&[bool]>,
    fallback_fg: Color,
) -> Vec<Span<'static>> {
    if visible_width == 0 {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let first_span = index
        .spans
        .partition_point(|span| span.char_end <= horizontal_scroll);
    let mut absolute_col = horizontal_scroll;
    let mut rendered = 0usize;
    let mut batch_buf = String::with_capacity(128);
    let mut batch_style = Style::default();
    'outer: for (relative, (style, segment)) in highlighted[first_span..].iter().enumerate() {
        let span = first_span + relative;
        let span_start = if span == 0 {
            0
        } else {
            index.spans[span - 1].char_end
        };
        let skipped = horizontal_scroll.saturating_sub(span_start);
        let segment = index.segment_tail(span, segment, skipped);
        let fg = crate::ui_colors::syntax_color(style.foreground, bg);
        let token_style = Style::default().fg(fg).add_modifier(crate::ui_colors::syntax_font_modifiers(style.font_style));
        let highlighted_style = token_style.fg(crate::ui_colors::syntax_color(
            style.foreground,
            bg_highlight.unwrap_or(bg),
        ));
        for ch in segment.chars() {
            if absolute_col >= horizontal_scroll {
                let is_highlighted = char_highlights
                    .and_then(|h| h.get(absolute_col).copied())
                    .unwrap_or(false);
                let char_bg = if is_highlighted {
                    bg_highlight.unwrap_or(bg)
                } else {
                    bg
                };
                let char_style = if is_highlighted {
                    highlighted_style
                } else {
                    token_style
                }
                .bg(char_bg);
                if char_style == batch_style && !batch_buf.is_empty() {
                    batch_buf.push(ch);
                } else {
                    if !batch_buf.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut batch_buf), batch_style));
                    }
                    batch_buf.push(ch);
                    batch_style = char_style;
                }
                rendered += 1;
                if rendered >= visible_width {
                    break 'outer;
                }
            }
            absolute_col += 1;
        }
    }
    if !batch_buf.is_empty() {
        spans.push(Span::styled(std::mem::take(&mut batch_buf), batch_style));
    }
    if rendered < visible_width {
        spans.push(Span::styled(
            crate::helpers::spaces(visible_width - rendered),
            Style::default().fg(fallback_fg).bg(bg),
        ));
    }
    spans
}

#[cfg(test)]
fn render_diff_highlighted_spans_reference(
    highlighted: &[(crate::textmate::Style, String)],
    visible_width: usize,
    horizontal_scroll: usize,
    bg: Color,
    bg_highlight: Option<Color>,
    char_highlights: Option<&[bool]>,
    fallback_fg: Color,
) -> Vec<Span<'static>> {
    if visible_width == 0 {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut absolute_col = 0usize;
    let mut rendered = 0usize;
    let mut batch_buf = String::with_capacity(128);
    let mut batch_style = Style::default();
    'outer: for (style, segment) in highlighted {
        let fg = crate::ui_colors::syntax_color(style.foreground, bg);
        let token_style = Style::default().fg(fg).add_modifier(crate::ui_colors::syntax_font_modifiers(style.font_style));
        let highlighted_style = token_style.fg(crate::ui_colors::syntax_color(
            style.foreground, bg_highlight.unwrap_or(bg),
        ));
        for ch in segment.trim_end_matches('\n').chars() {
            if absolute_col >= horizontal_scroll {
                let is_highlighted = char_highlights
                    .and_then(|h| h.get(absolute_col).copied())
                    .unwrap_or(false);
                let char_bg = if is_highlighted {
                    bg_highlight.unwrap_or(bg)
                } else {
                    bg
                };
                let char_style = if is_highlighted { highlighted_style } else { token_style }.bg(char_bg);
                if char_style == batch_style && !batch_buf.is_empty() {
                    batch_buf.push(ch);
                } else {
                    if !batch_buf.is_empty() {
                        spans.push(Span::styled(std::mem::take(&mut batch_buf), batch_style));
                    }
                    batch_buf.push(ch);
                    batch_style = char_style;
                }
                rendered += 1;
                if rendered >= visible_width {
                    break 'outer;
                }
            }
            absolute_col += 1;
        }
    }
    if !batch_buf.is_empty() {
        spans.push(Span::styled(std::mem::take(&mut batch_buf), batch_style));
    }
    if rendered < visible_width {
        spans.push(Span::styled(
            crate::helpers::spaces(visible_width - rendered),
            Style::default().fg(fallback_fg).bg(bg),
        ));
    }
    spans
}

fn render_plain_diff_window(
    text: &str,
    absolute_start: usize,
    visible_width: usize,
    bg: Color,
    bg_highlight: Option<Color>,
    char_highlights: Option<&[bool]>,
    fg: Color,
) -> Vec<Span<'static>> {
    if visible_width == 0 {
        return Vec::new();
    }
    let mut spans = Vec::new();
    let mut batch = String::with_capacity(visible_width.min(128));
    let mut batch_style = Style::default().fg(fg).bg(bg);
    let mut rendered = 0usize;
    for (offset, ch) in text.chars().take(visible_width).enumerate() {
        let highlighted = char_highlights
            .and_then(|flags| flags.get(absolute_start + offset).copied())
            .unwrap_or(false);
        let style = Style::default().fg(fg).bg(if highlighted {
            bg_highlight.unwrap_or(bg)
        } else {
            bg
        });
        if style != batch_style && !batch.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut batch), batch_style));
        }
        batch_style = style;
        batch.push(ch);
        rendered += 1;
    }
    if !batch.is_empty() {
        spans.push(Span::styled(batch, batch_style));
    }
    if rendered < visible_width {
        spans.push(Span::styled(
            crate::helpers::spaces(visible_width - rendered),
            Style::default().fg(fg).bg(bg),
        ));
    }
    spans
}

fn render_diff_line<'a>(
    cached: Option<&std::rc::Rc<crate::highlight::HighlightSpans>>,
    _text: &str,
    char_len: usize,
    plain_window: impl FnOnce() -> &'a str,
    visible_width: usize,
    horizontal_scroll: usize,
    bg: Color,
    bg_highlight: Option<Color>,
    char_highlights: Option<&[bool]>,
    fg: Color,
) -> Vec<Span<'static>> {
    #[cfg(test)]
    if REFERENCE_DIFF_RENDER.with(std::cell::Cell::get) {
        let fallback = match cached {
            Some(spans) => normalize_diff_highlight_line(spans, _text, fg),
            None => Some(plain_diff_highlight_line(_text, fg)),
        };
        let highlighted = fallback
            .as_ref()
            .or(cached.map(std::rc::Rc::as_ref))
            .unwrap();
        return render_diff_highlighted_spans_reference(
            highlighted,
            visible_width,
            horizontal_scroll,
            bg,
            bg_highlight,
            char_highlights,
            fg,
        );
    }
    if let Some(cached) = cached {
        let index = diff_highlight_index(cached);
        if index.char_len() >= char_len {
            return render_diff_highlighted_spans(
                cached,
                &index,
                visible_width,
                horizontal_scroll,
                bg,
                bg_highlight,
                char_highlights,
                fg,
            );
        }
    }
    // Keep the previous neutral TextMate fallback color, while borrowing just
    // the visible source window instead of copying the complete logical line.
    let content_fg = match fg {
        Color::Rgb(..) => fg,
        _ => Color::Rgb(212, 212, 212),
    };
    let window = plain_window().trim_end_matches('\n');
    let mut spans = render_plain_diff_window(
        window,
        horizontal_scroll,
        visible_width,
        bg,
        bg_highlight,
        char_highlights,
        content_fg,
    );
    if content_fg != fg
        && window.chars().count() < visible_width
        && let Some(padding) = spans.last_mut()
    {
        padding.style = padding.style.fg(fg);
    }
    spans
}

fn style_diff_spans(spans: &mut [Span<'_>], colors: &crate::app_theme::ThemeColors, kind: char, contrast: &mut crate::ui_colors::TextContrastCache) {
    let (highlight, border) = match kind {
        '+' => (colors.bg_diff_added_highlight, colors.ui.border_diff_added),
        '-' => (colors.bg_diff_removed_highlight, colors.ui.border_diff_removed),
        _ => (colors.bg_editor, None),
    };
    for span in spans {
        let background = span.style.bg.unwrap_or(colors.bg_editor);
        let foreground = span.style.fg.unwrap_or(colors.fg_text);
        span.style = span.style.fg(contrast.foreground(foreground, background))
            .remove_modifier(Modifier::DIM);
        if background == highlight && let Some(border) = border {
            span.style = span.style.add_modifier(Modifier::UNDERLINED).underline_color(border);
        }
    }
}

pub(crate) fn draw_diff_original(f: &mut Frame, app: &mut App, area: Rect) {
    let (
        editor_scroll_offset,
        editor_cursor_line,
        editor_file_path,
        editor_syntax_language,
    ) = match app.editor() {
        Some(e) => (
            e.scroll_offset,
            e.cursor_line,
            e.syntax_file_path().map(std::path::Path::to_path_buf),
            e.syntax_language_token().map(str::to_string),
        ),
        None => return,
    };

    if app.compare_base_content.is_none() {
        return;
    }
    let diff_cache = std::sync::Arc::clone(&app.diff_render_cache);
    let rows = &diff_cache.rows;
    let row_scroll = app.aligned_diff_scroll_row(editor_scroll_offset);
    let gutter_width = digit_count(diff_cache.max_left_line).max(2) + 1;
    let display_width = (area.width as usize).saturating_sub(gutter_width + 3);
    let show_h_scrollbar = diff_cache.max_left_text_width > display_width && area.height > 1;
    let content_area = Rect::new(
        area.x,
        area.y,
        area.width,
        area.height.saturating_sub(u16::from(show_h_scrollbar)),
    );
    let left_code_area = Rect::new(
        area.x.saturating_add((gutter_width + 3) as u16).min(area.right()),
        content_area.y,
        display_width.min(u16::MAX as usize) as u16,
        content_area.height,
    );
    app.diff_left_code_area = Some(left_code_area);
    let max_scroll = diff_cache.max_left_text_width.saturating_sub(display_width);
    app.diff_left_horizontal_scroll = app.diff_left_horizontal_scroll.min(max_scroll);
    let left_horizontal_scroll = app.diff_left_horizontal_scroll;
    let visible_lines = content_area.height as usize;

    let mut contrast = crate::ui_colors::TextContrastCache::new(&app.colors.ui);
    let mut lines_out: Vec<Line<'_>> = Vec::with_capacity(visible_lines);
    let mut left_num_pad = String::with_capacity(gutter_width + 2);
    let _ = write!(left_num_pad, "{:>width$} ", " ", width = gutter_width);
    let mut left_num_buf = String::with_capacity(gutter_width + 2);
    for row in 0..visible_lines {
        let row_idx = row_scroll + row;
        let Some(diff_row) = rows.get(row_idx) else {
            let filler = crate::helpers::spaces(content_area.width as usize);
            lines_out.push(Line::from(Span::styled(
                filler,
                Style::default()
                    .fg(app.colors.fg_dim)
                    .bg(app.colors.bg_dark),
            )));
            continue;
        };
        if diff_row.hidden_lines > 0 {
            let style = Style::default()
                .fg(app.colors.ui.description_color(app.colors.bg_dark)).bg(app.colors.bg_dark);
            lines_out.push(Line::styled(diff_row.left_text.as_str(), style));
            continue;
        }
        let is_cursor_row = diff_row.right_line == Some(editor_cursor_line);
        let base_bg = if is_cursor_row {
            app.colors.bg_line_highlight
        } else {
            app.colors.bg_editor
        };
        let status = diff_row.left_kind;

        if diff_row.left_line.is_none() {
            let spacer_bg = app.colors.bg_dark;
            let starts_block = row_idx == 0
                || rows
                    .get(row_idx - 1)
                    .is_none_or(|row| row.left_line.is_some());
            let ends_block = rows
                .get(row_idx.saturating_add(1))
                .is_none_or(|row| row.left_line.is_some());
            lines_out.push(Line::from(vec![
                Span::styled(
                    left_num_pad.as_str(),
                    Style::default().fg(app.colors.fg_dim).bg(spacer_bg),
                ),
                Span::styled("  ", Style::default().fg(app.colors.fg_dim).bg(spacer_bg)),
                Span::styled(
                    diff_spacer_pattern(display_width, row_idx, starts_block, ends_block),
                    Style::default().fg(app.colors.ui.fg_diff_diagonal).bg(spacer_bg),
                ),
            ]));
            continue;
        }

        let bg = match status {
            '-' => app.colors.bg_diff_removed,
            '+' => app.colors.bg_diff_added,
            _ => base_bg,
        };

        let line_num_span = if diff_row.left_line.is_some() {
            left_num_buf.clear();
            let _ = write!(
                left_num_buf,
                "{:>width$} ",
                diff_row.left_line.unwrap() + 1,
                width = gutter_width
            );
            let s = std::mem::take(&mut left_num_buf);
            left_num_buf = String::with_capacity(gutter_width + 2);
            Span::styled(s, Style::default().fg(app.colors.fg_dim).bg(bg))
        } else {
            Span::styled(
                left_num_pad.as_str(),
                Style::default().fg(app.colors.fg_dim).bg(bg),
            )
        };

        let indicator = match status {
            '-' => Span::styled("- ", Style::default().fg(app.colors.fg_diff_removed).bg(bg)),
            '+' => Span::styled("+ ", Style::default().fg(app.colors.fg_diff_added).bg(bg)),
            _ => Span::styled("  ", Style::default().fg(app.colors.fg_dim).bg(bg)),
        };
        let bg_hl = match status {
            '-' => Some(app.colors.bg_diff_removed_highlight),
            '+' => Some(app.colors.bg_diff_added_highlight),
            _ => None,
        };
        let mut row_spans = vec![line_num_span, indicator];
        if diff_row.left_char_len() > crate::highlight::MAX_SYNTAX_LINE_CHARS {
            row_spans.extend(render_plain_diff_window(
                diff_row.left_text_window(left_horizontal_scroll, display_width),
                left_horizontal_scroll,
                display_width,
                bg,
                bg_hl,
                diff_row.left_char_highlights.as_deref(),
                app.colors.fg_text,
            ));
        } else {
            let cached = app.highlighter.highlighted_fragment_by_hash_if_cached(
                diff_row.left_highlight_hash(),
                diff_row.left_line.unwrap_or(row_idx),
                editor_file_path.as_deref(),
                editor_syntax_language.as_deref(),
                app.syntax_theme.as_str(),
            );
            row_spans.extend(render_diff_line(
                cached.as_ref(),
                &diff_row.left_text,
                diff_row.left_char_len(),
                || diff_row.left_text_window(left_horizontal_scroll, display_width),
                display_width,
                left_horizontal_scroll,
                bg,
                bg_hl,
                diff_row.left_char_highlights.as_deref(),
                app.colors.fg_text,
            ));
        }
        style_diff_spans(&mut row_spans, &app.colors, status, &mut contrast);
        lines_out.push(Line::from(row_spans));
    }

    f.render_widget(
        Paragraph::new(lines_out).style(Style::default().remove_modifier(Modifier::DIM)),
        content_area,
    );

    if show_h_scrollbar {
        let track_row = area.bottom().saturating_sub(1);
        f.render_widget(
            Paragraph::new("").style(Style::default().bg(app.colors.bg_dark)),
            Rect::new(area.x, track_row, area.width, 1),
        );
        let h_area = Rect::new(left_code_area.x, track_row, left_code_area.width, 1);
        app.diff_left_h_scrollbar_area = Some(h_area);
        draw_horizontal_scrollbar(
            f,
            HScrollbarParams {
                area: h_area,
                total_width: diff_cache.max_left_text_width,
                visible_width: display_width,
                scroll: left_horizontal_scroll,
                bg_editor: app.colors.bg_dark,
                hovered: false,
                active: app.dragging_diff_left_h_scrollbar,
                colors: &app.colors.ui,
            },
        );
    } else {
        app.diff_left_h_scrollbar_area = None;
    }
}

/// Overlay aligned diff rows on top of the regular editor pane in side-by-side mode.
pub(crate) fn draw_diff_target_overlay(f: &mut Frame, app: &mut App) {
    if app.diff_mode != crate::app::DiffMode::SideBySide || app.compare_base_content.is_none() {
        return;
    }
    let (
        editor_scroll_offset,
        editor_cursor_line,
        editor_h_scroll,
        editor_file_path,
        editor_syntax_language,
    ) = match app.editor() {
        Some(e) => (
            e.scroll_offset,
            e.cursor_line,
            e.horizontal_scroll,
            e.syntax_file_path().map(std::path::Path::to_path_buf),
            e.syntax_language_token().map(str::to_string),
        ),
        None => return,
    };
    let (Some(gutter_area), Some(code_area)) = (app.gutter_area, app.code_area) else {
        return;
    };
    if app.compare_diff_lines.is_empty() {
        return;
    }
    let diff_cache = std::sync::Arc::clone(&app.diff_render_cache);
    let rows = &diff_cache.rows;
    let row_scroll = app.aligned_diff_scroll_row(editor_scroll_offset);
    let visible_rows = code_area.height as usize;
    let h_scroll = editor_h_scroll;
    let visible_width = code_area.width as usize;
    // The side-by-side layout always reserves a numeric gutter on both panes.
    // Use the actual target gutter width so a transient editor/diff line-count
    // mismatch cannot push the digits beyond the clipped render area.
    let show_line_numbers = gutter_area.width > 2;
    let num_width = if show_line_numbers {
        gutter_area.width.saturating_sub(2) as usize
    } else {
        0
    };
    let mut gutter_lines: Vec<Line> = Vec::with_capacity(visible_rows);
    let mut contrast = crate::ui_colors::TextContrastCache::new(&app.colors.ui);
    let mut code_lines: Vec<Line> = Vec::with_capacity(visible_rows);
    let mut has_hidden_rows = false;
    let tilde_gutter: std::borrow::Cow<str> = if show_line_numbers {
        let mut buf = String::with_capacity(num_width + 3);
        buf.push_str(&crate::helpers::spaces(num_width + 2));
        std::borrow::Cow::Owned(buf)
    } else {
        std::borrow::Cow::Borrowed("  ")
    };
    let blank_pad: &str = &crate::helpers::spaces(visible_width);
    let num_pad: &str = &crate::helpers::spaces(num_width);
    let mut num_buf = String::with_capacity(num_width + 2);
    let tilde_line = Line::from(Span::styled(
        tilde_gutter,
        Style::default()
            .fg(app.colors.fg_tilde)
            .bg(app.colors.bg_dark),
    ));
    let blank_line = Line::from(Span::styled(
        blank_pad,
        Style::default().bg(app.colors.bg_editor),
    ));

    for row in 0..visible_rows {
        let row_idx = row_scroll + row;
        let Some(diff_row) = rows.get(row_idx) else {
            gutter_lines.push(tilde_line.clone());
            code_lines.push(blank_line.clone());
            continue;
        };

        if diff_row.hidden_lines > 0 {
            has_hidden_rows = true;
            gutter_lines.push(Line::default());
            code_lines.push(Line::default());
            continue;
        }
        if diff_row.right_line.is_none() {
            let gutter_bg = app.colors.bg_dark;
            let spacer_bg = app.colors.bg_editor;
            let starts_block = row_idx == 0
                || rows
                    .get(row_idx - 1)
                    .is_none_or(|row| row.right_line.is_some());
            let ends_block = rows
                .get(row_idx.saturating_add(1))
                .is_none_or(|row| row.right_line.is_some());
            gutter_lines.push(Line::from(vec![
                Span::styled(" ", Style::default().fg(app.colors.fg_dim).bg(gutter_bg)),
                Span::styled(
                    if show_line_numbers { num_pad } else { "" },
                    Style::default().fg(app.colors.fg_dim).bg(gutter_bg),
                ),
                Span::styled(" ", Style::default().fg(app.colors.fg_dim).bg(gutter_bg)),
            ]));
            let style = Style::default().fg(app.colors.ui.fg_diff_diagonal).bg(spacer_bg);
            code_lines.push(Line::from(vec![
                Span::styled("  ", style),
                Span::styled(
                    diff_spacer_pattern(visible_width.saturating_sub(2), row_idx, starts_block, ends_block),
                    style,
                ),
            ]));
            continue;
        }

        let is_cursor_row = diff_row.right_line == Some(editor_cursor_line);
        let base_bg = if is_cursor_row {
            app.colors.bg_line_highlight
        } else {
            app.colors.bg_editor
        };
        let row_bg = match diff_row.right_kind {
            '+' => app.colors.bg_diff_added,
            '-' => app.colors.bg_diff_removed,
            _ => base_bg,
        };

        let (diff_char, diff_style) = match diff_row.right_kind {
            '+' => (
                "+",
                Style::default()
                    .fg(app.colors.fg_diff_added)
                    .bg(row_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            '-' => (
                "-",
                Style::default()
                    .fg(app.colors.fg_diff_removed)
                    .bg(row_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            _ => (" ", Style::default().fg(row_bg).bg(row_bg)),
        };
        let line_num = if show_line_numbers {
            if let Some(line) = diff_row.right_line {
                num_buf.clear();
                let _ = write!(num_buf, "{:>width$}", line + 1, width = num_width);
                let s = std::mem::take(&mut num_buf);
                num_buf = String::with_capacity(num_width + 1);
                s
            } else {
                num_pad.to_string()
            }
        } else {
            String::new()
        };
        let fold_char = " ";
        gutter_lines.push(Line::from(vec![
            Span::styled(diff_char, diff_style),
            Span::styled(
                line_num,
                Style::default()
                    .fg(if is_cursor_row {
                        app.colors.fg_text
                    } else {
                        app.colors.fg_dim
                    })
                    .bg(row_bg)
                    .add_modifier(if is_cursor_row {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(fold_char, Style::default().fg(app.colors.fg_dim).bg(row_bg)),
        ]));

        let fg = match diff_row.right_kind {
            '+' => app.colors.fg_diff_added,
            '-' => app.colors.fg_diff_removed,
            _ => app.colors.fg_text,
        };
        let marker = match diff_row.right_kind {
            '+' => Span::styled(
                "+ ",
                Style::default().fg(app.colors.fg_diff_added).bg(row_bg),
            ),
            '-' => Span::styled(
                "- ",
                Style::default().fg(app.colors.fg_diff_removed).bg(row_bg),
            ),
            _ => Span::styled("  ", Style::default().fg(app.colors.fg_dim).bg(row_bg)),
        };
        let bg_hl = match diff_row.right_kind {
            '+' => Some(app.colors.bg_diff_added_highlight),
            '-' => Some(app.colors.bg_diff_removed_highlight),
            _ => None,
        };
        let text_width = visible_width.saturating_sub(2);
        let mut row_spans = vec![marker];
        if diff_row.right_char_len() > crate::highlight::MAX_SYNTAX_LINE_CHARS {
            row_spans.extend(render_plain_diff_window(
                diff_row.right_text_window(h_scroll, text_width),
                h_scroll,
                text_width,
                row_bg,
                bg_hl,
                diff_row.right_char_highlights.as_deref(),
                fg,
            ));
        } else {
            let cached = app.highlighter.highlighted_fragment_by_hash_if_cached(
                diff_row.right_highlight_hash(),
                diff_row.right_line.unwrap_or(row_idx),
                editor_file_path.as_deref(),
                editor_syntax_language.as_deref(),
                app.syntax_theme.as_str(),
            );
            row_spans.extend(render_diff_line(
                cached.as_ref(),
                &diff_row.right_text,
                diff_row.right_char_len(),
                || diff_row.right_text_window(h_scroll, text_width),
                text_width,
                h_scroll,
                row_bg,
                bg_hl,
                diff_row.right_char_highlights.as_deref(),
                fg,
            ));
        }
        style_diff_spans(&mut row_spans, &app.colors, diff_row.right_kind, &mut contrast);
        code_lines.push(Line::from(row_spans));
    }

    f.render_widget(
        Paragraph::new(gutter_lines).style(
            Style::default()
                .bg(app.colors.bg_dark)
                .remove_modifier(Modifier::DIM),
        ),
        gutter_area,
    );
    f.render_widget(
        Paragraph::new(code_lines).style(
            Style::default()
                .bg(app.colors.bg_editor)
                .remove_modifier(Modifier::DIM),
        ),
        code_area,
    );

    // The gutter, scope rail and code are separate regions. Hidden-context
    // separators span all three, independently of horizontal text scrolling.
    if has_hidden_rows {
        let style = Style::default()
            .fg(app.colors.ui.description_color(app.colors.bg_dark)).bg(app.colors.bg_dark);
        for (offset, row) in rows.iter().skip(row_scroll).take(visible_rows).enumerate() {
            if row.hidden_lines > 0 {
                let area = Rect::new(
                    gutter_area.x,
                    code_area.y + offset as u16,
                    code_area.right().saturating_sub(gutter_area.x),
                    1,
                );
                // The regular editor was painted underneath this overlay.
                // A background style alone leaves its trailing text visible.
                f.render_widget(Clear, area);
                f.render_widget(
                    Paragraph::new(row.right_text.as_str()).style(style),
                    area,
                );
            }
        }
    }
}

/// Inline diff view rendered from library-produced diff lines.
#[derive(Clone, Copy)]
struct InlineDiffLayout {
    title_area: Rect,
    code_area: Rect,
    minimap_sep_area: Option<Rect>,
    minimap_area: Option<Rect>,
    scrollbar_area: Rect,
}

fn inline_diff_layout(area: Rect, show_minimap: bool, minimap_width: u16) -> InlineDiffLayout {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    let content_area = layout[1];
    let minimap_width = if show_minimap && content_area.width > 80 {
        minimap_width
    } else {
        0
    };
    let minimap_sep_width = u16::from(minimap_width > 0);
    let scrollbar_width = if minimap_width > 0 { 1 } else { 2 };
    let content_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(minimap_sep_width),
            Constraint::Length(minimap_width),
            Constraint::Length(scrollbar_width),
        ])
        .split(content_area);

    InlineDiffLayout {
        title_area: layout[0],
        code_area: content_layout[0],
        minimap_sep_area: (minimap_sep_width > 0).then_some(content_layout[1]),
        minimap_area: (minimap_width > 0).then_some(content_layout[2]),
        scrollbar_area: content_layout[3],
    }
}

#[cfg(test)]
fn inline_diff_text_window(text: &str, column: usize, width: usize) -> (&str, usize, usize) {
    if text.is_ascii() && !text.bytes().any(|byte| byte.is_ascii_control()) {
        let start = column.min(text.len());
        return (
            &text[start..start.saturating_add(width).min(text.len())],
            start,
            0,
        );
    }
    let mut cell = 0usize;
    let mut character = 0usize;
    let mut padding = 0usize;
    for (byte, grapheme) in text.grapheme_indices(true) {
        let cells = if grapheme.contains(char::is_control) {
            0
        } else {
            grapheme.width()
        };
        if cell < column {
            padding = cell.saturating_add(cells).saturating_sub(column).min(width);
            cell += cells;
            character += grapheme.chars().count();
            continue;
        }
        let tail = &text[byte..];
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
        return (&tail[..end], character, padding);
    }
    ("", character, padding)
}

fn draw_inline_scrollbar(
    f: &mut Frame,
    params: ScrollbarParams,
    cache: &crate::app::DiffRenderCache,
) {
    #[cfg(test)]
    if REFERENCE_DIFF_RENDER.with(std::cell::Cell::get) {
        let matches: Vec<_> = params
            .search_matches
            .iter()
            .map(|&(line, start, end)| (cache.inline_row_for_target_line(line), start, end))
            .collect();
        let diagnostics: Vec<_> = params
            .diagnostic_lines
            .iter()
            .map(|&(line, severity)| (cache.inline_row_for_target_line(line), severity))
            .collect();
        crate::ui_editor_minimap::draw_scrollbar(
            f,
            ScrollbarParams {
                search_matches: &matches,
                diagnostic_lines: &diagnostics,
                diagnostic_index: None,
                ..params
            },
        );
        return;
    }
    draw_mapped_scrollbar(f, params, cache.inline_target_rows());
}

pub(crate) fn draw_inline_diff(f: &mut Frame, app: &mut App, area: Rect) {
    if crate::ui_animation_frame::begin_diff(f, app, area, false) {
        return;
    }
    if app.compare_base_content.is_none() {
        return;
    }
    app.gutter_width = 0;
    app.gutter_area = None;
    app.h_scrollbar_area = None;
    if let Some(editor) = app.editor_mut() { editor.diff_display_rows = None; }

    let diff_cache = std::sync::Arc::clone(&app.diff_render_cache);
    let review_row = crate::commit_review::painting_row(app);
    let mut layout = inline_diff_layout(area, app.show_minimap && !app.context_diff_active() && review_row.is_none(), app.minimap_width);
    let number_width = digit_count(diff_cache.max_left_line.max(
        app.editor().map_or(1, |editor| editor.line_count()),
    )).max(4);
    let gutter_width = (number_width * 2 + 4).min(layout.code_area.width as usize);
    let text_width = layout.code_area.width.saturating_sub(gutter_width as u16);
    let horizontal_area = if diff_cache.max_inline_text_width > text_width as usize
        && layout.code_area.height > 1 && text_width > 0
    {
        layout.code_area.height -= 1;
        layout.scrollbar_area.height = layout.code_area.height;
        if let Some(minimap) = &mut layout.minimap_area { minimap.height = layout.code_area.height; }
        if let Some(separator) = &mut layout.minimap_sep_area { separator.height = layout.code_area.height; }
        Some(Rect::new(
            layout.code_area.x + gutter_width as u16,
            layout.code_area.bottom(),
            text_width,
            1,
        ))
    } else {
        None
    };
    let text_area = Rect::new(
        layout.code_area.x + gutter_width as u16,
        layout.code_area.y,
        text_width,
        layout.code_area.height,
    );
    app.code_area = Some(text_area);
    app.gutter_width = gutter_width as u16;
    app.h_scrollbar_area = horizontal_area;
    app.minimap_area = layout.minimap_area;
    app.scrollbar_area = Some(layout.scrollbar_area);

    let mut scroll = std::mem::take(&mut app.diff_inline_scroll);
    if scroll.aligned { scroll = Default::default(); }
    scroll.area = Rect::new(area.x, layout.code_area.y, area.width, area.bottom().saturating_sub(layout.code_area.y));
    scroll.text_area = text_area;
    scroll.vertical_area = layout.scrollbar_area;
    scroll.horizontal_area = horizontal_area;
    if let Some(editor) = app.editor_mut() {
        if let Some(row) = review_row { scroll.layout_review(diff_cache.clone(), editor, row); }
        else { scroll.layout(std::sync::Arc::clone(&diff_cache), editor); }
    }
    app.diff_inline_scroll = scroll;

    refresh_cached_editor_diagnostics(app);
    let Some(editor) = app.editor() else {
        return;
    };

    let (base, target) = diff_labels(app);
    let unsaved = if editor.modified && !editor.read_only {
        " (unsaved)"
    } else {
        ""
    };
    let title = format!(
        "{base} → {target}{unsaved}  • Inline Diff: {}",
        editor.file_name()
    );
    draw_diff_header(f, app, layout.title_area, &title, false);

    let visible = layout.code_area.height as usize;
    let scroll = app.diff_inline_scroll.row;
    let horizontal_scroll = app.diff_inline_scroll.column;
    let width = layout.code_area.width as usize;

    let mut contrast = crate::ui_colors::TextContrastCache::new(&app.colors.ui);
    let mut render_lines: Vec<Line<'_>> = Vec::new();
    for row in 0..visible {
        let idx = scroll + row;
        if idx >= diff_cache.inline_rows.len() {
            render_lines.push(Line::from(Span::styled(
                " ",
                Style::default().fg(app.colors.fg_dim),
            )));
            continue;
        }
        let cached_row = &diff_cache.inline_rows[idx];
        let aligned_row = &diff_cache.rows[cached_row.aligned_row];
        if aligned_row.hidden_lines > 0 {
            let style = Style::default()
                .fg(app.colors.ui.description_color(app.colors.bg_dark)).bg(app.colors.bg_dark);
            render_lines.push(Line::styled(aligned_row.right_text.as_str(), style));
            continue;
        }
        let (old_ln, new_ln, _source_text, char_highlights, highlight_hash, char_len) =
            match cached_row.kind {
                '-' => (
                    aligned_row.left_line,
                    None,
                    aligned_row.left_text.as_str(),
                    aligned_row.left_char_highlights.as_deref(),
                    aligned_row.left_highlight_hash(),
                    aligned_row.left_char_len(),
                ),
                '+' => (
                    None,
                    aligned_row.right_line,
                    aligned_row.right_text.as_str(),
                    aligned_row.right_char_highlights.as_deref(),
                    aligned_row.right_highlight_hash(),
                    aligned_row.right_char_len(),
                ),
                _ => (
                    aligned_row.left_line,
                    aligned_row.right_line,
                    aligned_row.right_text.as_str(),
                    None,
                    aligned_row.right_highlight_hash(),
                    aligned_row.right_char_len(),
                ),
            };
        let highlighted = if char_len <= crate::highlight::MAX_SYNTAX_LINE_CHARS {
            app.highlighter
                .highlighted_fragment_by_hash_if_cached(
                    highlight_hash,
                    new_ln.or(old_ln).unwrap_or(cached_row.aligned_row),
                    editor.syntax_file_path(),
                    editor.syntax_language_token(),
                    &app.syntax_theme,
                )
                .map(|spans| {
                    let index = diff_highlight_index(&spans);
                    (spans, index)
                })
                .filter(|(_, index)| index.char_len() >= char_len)
        } else {
            None
        };
        let (prefix, bg, bg_hl, fg) = match cached_row.kind {
            '-' => (
                "- ",
                app.colors.bg_diff_removed,
                app.colors.bg_diff_removed_highlight,
                app.colors.fg_diff_removed,
            ),
            '+' => (
                "+ ",
                app.colors.bg_diff_added,
                app.colors.bg_diff_added_highlight,
                app.colors.fg_diff_added,
            ),
            _ => (
                "  ",
                app.colors.bg_editor,
                app.colors.bg_editor,
                app.colors.fg_text,
            ),
        };
        let old_disp = old_ln.map(|n| n + 1);
        let new_disp = new_ln.map(|n| n + 1);
        let mut line_head = String::with_capacity(16);
        if let Some(n) = old_disp {
            let _ = write!(line_head, "{n:>number_width$}");
        } else {
            line_head.push_str(&crate::helpers::spaces(number_width));
        }
        line_head.push(' ');
        if let Some(n) = new_disp {
            let _ = write!(line_head, "{n:>number_width$}");
        } else {
            line_head.push_str(&crate::helpers::spaces(number_width));
        }
        line_head.push(' ');
        line_head.push_str(prefix);
        let available = width.saturating_sub(UnicodeWidthStr::width(line_head.as_str()));
        let mut row_spans = vec![Span::styled(line_head, Style::default().fg(fg).bg(bg))];
        let window = || {
            if cached_row.kind == '-' {
                aligned_row.left_inline_text_window(horizontal_scroll, available)
            } else {
                aligned_row.right_inline_text_window(horizontal_scroll, available)
            }
        };
        #[cfg(test)]
        let (text, char_start, padding) = if REFERENCE_DIFF_RENDER.with(std::cell::Cell::get) {
            inline_diff_text_window(_source_text, horizontal_scroll, available)
        } else {
            window()
        };
        #[cfg(not(test))]
        let (text, char_start, padding) = window();
        row_spans.push(Span::styled(crate::helpers::spaces(padding), Style::default().fg(fg).bg(bg)));
        if let Some((highlighted, index)) = &highlighted {
            // The window is clipped in terminal cells, while syntax spans and
            // character-diff flags use character offsets. Keep both aligned,
            // including when scrolling past wide or combining graphemes.
            #[cfg(test)]
            let reference = REFERENCE_DIFF_RENDER.with(std::cell::Cell::get);
            #[cfg(not(test))]
            let reference = false;
            let spans = if reference {
                #[cfg(test)]
                {
                    render_diff_highlighted_spans_reference(
                        highlighted,
                        text.chars().count(),
                        char_start,
                        bg,
                        Some(bg_hl),
                        char_highlights,
                        fg,
                    )
                }
                #[cfg(not(test))]
                {
                    unreachable!()
                }
            } else {
                render_diff_highlighted_spans(
                    highlighted,
                    index,
                    text.chars().count(),
                    char_start,
                    bg,
                    Some(bg_hl),
                    char_highlights,
                    fg,
                )
            };
            row_spans.extend(spans);
        } else {
            row_spans.extend(render_plain_diff_window(
                text, char_start, text.chars().count(), bg, Some(bg_hl), char_highlights, fg,
            ));
        }
        row_spans.push(Span::styled(
            crate::helpers::spaces(available.saturating_sub(padding + text.width())),
            Style::default().fg(fg).bg(bg),
        ));
        style_diff_spans(&mut row_spans, &app.colors, cached_row.kind, &mut contrast);
        render_lines.push(Line::from(row_spans));
    }

    f.render_widget(
        Paragraph::new(render_lines).style(
            Style::default()
                .bg(app.colors.bg_editor)
                .remove_modifier(Modifier::DIM),
        ),
        layout.code_area,
    );

    if app.context_diff_active() {
        crate::diff_context::map_visible_rows(app, false);
        crate::diff_context::draw_search_matches(f, app);
    }
    if layout.minimap_area.is_some() {
        crate::ui_editor::refresh_editor_change_index(
            app,
            Some(diff_cache.target_changes.as_slice()),
        );
    }
    let Some(editor) = app.editor() else { return; };
    let diag_lines = &app.cached_diag_lines;
    draw_inline_scrollbar(
        f,
        ScrollbarParams {
            area: layout.scrollbar_area,
            total_lines: app.diff_inline_scroll.row_count().max(1),
            visible_lines: visible,
            scroll,
            search_matches: &app.search_matches,
            diagnostic_lines: diag_lines,
            diagnostic_index: Some(&app.cached_diagnostic_index),
            change_rows: &app.diff_inline_scroll.change_rows,
            cursor_line: app
                .diff_inline_scroll
                .row_for_line(&diff_cache, editor.cursor_line),
            colors: app.colors,
            hovered: app.hovered_scrollbar,
            active: app.dragging_v_scrollbar,
        },
        &diff_cache,
    );
    // A two-column bar reserves its first column for the thumb. A single-column
    // bar (beside a minimap) must retain the shared renderer's change markers;
    // repainting a thumb there would erase every overview decoration.
    if review_row.is_none() && app.diff_inline_scroll.row_count() > visible && layout.scrollbar_area.width > 1 {
        let height = layout.scrollbar_area.height as usize;
        let (max_scroll, thumb) = crate::inline_diff::scrollbar_thumb(
            app.diff_inline_scroll.row_count(),
            visible,
            height,
            1,
        );
        let top = scroll.saturating_mul(height.saturating_sub(thumb)) / max_scroll.max(1);
        let (track, slider) = app.colors.ui.scrollbar_colors(app.colors.ui.bg_overview, app.hovered_scrollbar, app.dragging_v_scrollbar);
        for row in 0..height {
            let in_thumb = (top..top + thumb).contains(&row);
            let color = if in_thumb {
                slider
            } else {
                track
            };
            if let Some(cell) = f.buffer_mut().cell_mut((layout.scrollbar_area.x, layout.scrollbar_area.y + row as u16)) {
                cell.set_symbol(if in_thumb { "█" } else { " " });
                cell.set_style(Style::default().fg(color).bg(color));
            }
        }
    }
    if let Some(horizontal_area) = horizontal_area {
        // The scrollbar covers only the text viewport. Paint the reserved
        // gutter and overview end caps just as the regular editor does.
        let buffer = f.buffer_mut();
        buffer.set_style(
            Rect::new(area.x, horizontal_area.y, area.width, 1),
            Style::default().bg(app.colors.bg_editor),
        );
        buffer.set_style(
            Rect::new(layout.code_area.x, horizontal_area.y, gutter_width as u16, 1),
            Style::default().bg(app.colors.bg_dark),
        );
        draw_horizontal_scrollbar(f, HScrollbarParams {
            area: horizontal_area,
            total_width: diff_cache.max_inline_text_width,
            visible_width: text_width as usize,
            scroll: horizontal_scroll,
            bg_editor: app.colors.bg_editor,
            hovered: app.hovered_h_scrollbar,
            active: app.dragging_h_scrollbar,
            colors: &app.colors.ui,
        });
    }
    if let Some(minimap_sep_area) = layout.minimap_sep_area {
        draw_minimap_separator(
            f, minimap_sep_area, app.colors.ui.fg_overview_border, app.colors.bg_editor,
        );
    }
    if let Some(minimap_area) = layout.minimap_area {
        draw_minimap(
            f,
            MinimapParams {
                area: minimap_area,
                editor,
                scroll: editor.scroll_offset,
                visible_lines: visible,
                search_matches: &app.search_matches,
                diagnostic_lines: diag_lines,
                diagnostic_index: Some(&app.cached_diagnostic_index),
                change_index: Some(&app.cached_editor_change_index),
                diff_changes: Some(diff_cache.target_changes.as_slice()),
                colors: app.colors,
                hovered: app.hovered_minimap,
                active: app.dragging_minimap,
            },
        );
    }
    if app.context_diff_active() {
        crate::diff_context::draw_scrollbar(f, app);
    }
    // The retained scene includes the overview and both scrollbar tracks.
    // Cosmetic source overlays are clipped to the text viewport.
    crate::ui_animation_frame::finish_diff(f, app, area, false);
}

#[cfg(test)]
#[path = "ui_inline_diff_performance_tests.rs"]
mod performance_tests;

#[cfg(test)]
mod tests {
    use super::*;

    use crate::app::{App, DiffLabels, DiffMode, Tab};
    use crate::editor::Editor;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;
    use std::path::PathBuf;

    #[test]
    fn diff_token_alpha_and_font_styles_follow_character_highlight_surfaces() {
        let highlighted = vec![(
            crate::textmate::Style {
                foreground: crate::textmate::Color {
                    r: 255,
                    g: 255,
                    b: 255,
                    a: 128,
                },
                background: crate::textmate::Color {
                    r: 0,
                    g: 0,
                    b: 0,
                    a: 0,
                },
                font_style: crate::textmate::FontStyle::ITALIC,
            },
            "ab".to_string(),
        )];
        let spans = render_diff_highlighted_spans(
            &highlighted,
            &DiffHighlightIndex::new(&highlighted),
            2,
            0,
            Color::Rgb(32, 32, 32),
            Some(Color::Rgb(224, 224, 224)),
            Some(&[false, true]),
            Color::Rgb(255, 255, 255),
        );
        assert_eq!(spans[0].style.fg, Some(Color::Rgb(144, 144, 144)));
        assert_eq!(spans[1].style.fg, Some(Color::Rgb(240, 240, 240)));
        assert!(
            spans
                .iter()
                .all(|span| span.style.add_modifier.contains(Modifier::ITALIC))
        );
    }

    fn app_with_diff(base: &str, target: &str) -> App {
        let mut app = App::test_new();
        let mut editor = Editor::new();
        editor.file_path = Some(PathBuf::from("file.txt"));
        editor.rope = ropey::Rope::from_str(target);
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.compare_base_content = Some(base.to_string().into());
        app.set_compare_diff_lines(crate::app::compute_file_diff(base, target));
        app
    }

    fn buffer_row(buffer: &Buffer, y: u16, width: u16) -> String {
        (0..width)
            .filter_map(|x| buffer.cell((x, y)))
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn missing_original_line_renders_neutral_shaded_spacer() {
        let mut app = app_with_diff("one\nfive\n", "one\ntwo\nthree\nfour\nfive\n");
        let added_bg = app.colors.bg_diff_added;
        let width = 40;
        let height = 5;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");

        terminal
            .draw(|f| draw_diff_original(f, &mut app, Rect::new(0, 0, width, height)))
            .expect("draw original diff");

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((6, 1)).map(|cell| cell.symbol()), Some("┌"));
        assert_eq!(buffer.cell((39, 1)).map(|cell| cell.symbol()), Some("┐"));
        assert_eq!(buffer.cell((6, 2)).map(|cell| cell.symbol()), Some("│"));
        assert_eq!(buffer.cell((39, 2)).map(|cell| cell.symbol()), Some("│"));
        assert_eq!(buffer.cell((6, 3)).map(|cell| cell.symbol()), Some("└"));
        assert_eq!(buffer.cell((39, 3)).map(|cell| cell.symbol()), Some("┘"));
        assert!(buffer_row(buffer, 2, width).contains('╱'));
        for y in 1..=3 {
            assert!(!buffer_row(buffer, y, width).contains('+'));
            assert!((0..width)
                .filter_map(|x| buffer.cell((x, y)))
                .all(|cell| cell.bg != added_bg));
        }
    }

    #[test]
    fn missing_target_line_renders_neutral_shaded_spacer() {
        let mut app = app_with_diff("one\ntwo\nthree\nfour\nfive\n", "one\nfive\n");
        app.diff_mode = DiffMode::SideBySide;
        app.gutter_area = Some(Rect::new(0, 0, 5, 5));
        app.code_area = Some(Rect::new(5, 0, 35, 5));
        let removed_bg = app.colors.bg_diff_removed;
        let width = 40;
        let mut terminal = Terminal::new(TestBackend::new(width, 5)).expect("terminal");

        terminal
            .draw(|f| draw_diff_target_overlay(f, &mut app))
            .expect("draw target diff");

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((7, 1)).map(|cell| cell.symbol()), Some("┌"));
        assert_eq!(buffer.cell((39, 1)).map(|cell| cell.symbol()), Some("┐"));
        assert_eq!(buffer.cell((7, 2)).map(|cell| cell.symbol()), Some("│"));
        assert_eq!(buffer.cell((39, 2)).map(|cell| cell.symbol()), Some("│"));
        assert_eq!(buffer.cell((7, 3)).map(|cell| cell.symbol()), Some("└"));
        assert_eq!(buffer.cell((39, 3)).map(|cell| cell.symbol()), Some("┘"));
        assert!(buffer_row(buffer, 2, width).contains('╱'));
        for y in 1..=3 {
            // The diff marker prefix stays blank through alignment spacers,
            // keeping the scope connector's fallback column passable.
            for x in 5..7 {
                assert_eq!(buffer[(x, y)].symbol(), " ");
            }
            assert!(!buffer_row(buffer, y, width).contains('-'));
            assert!((0..width)
                .filter_map(|x| buffer.cell((x, y)))
                .all(|cell| cell.bg != removed_bg));
        }
    }

    #[test]
    fn side_by_side_diff_clears_underlying_dim_modifiers() {
        let mut app = app_with_diff("one\nold\nthree\n", "one\nnew\nthree\n");
        app.diff_mode = DiffMode::SideBySide;
        app.gutter_area = Some(Rect::new(40, 0, 5, 3));
        app.code_area = Some(Rect::new(45, 0, 35, 3));
        let full_area = Rect::new(0, 0, 80, 3);
        let mut terminal =
            Terminal::new(TestBackend::new(full_area.width, full_area.height)).expect("terminal");

        terminal
            .draw(|f| {
                f.buffer_mut()
                    .set_style(full_area, Style::default().add_modifier(Modifier::DIM));
                draw_diff_original(f, &mut app, Rect::new(0, 0, 40, 3));
                draw_diff_target_overlay(f, &mut app);
            })
            .expect("draw side-by-side diff");

        let buffer = terminal.backend().buffer();
        for y in full_area.top()..full_area.bottom() {
            for x in full_area.left()..full_area.right() {
                let cell = buffer.cell((x, y)).expect("diff cell");
                assert!(!cell.modifier.contains(Modifier::DIM));
            }
        }
    }

    #[test]
    fn side_by_side_diff_renders_target_line_numbers_on_both_panes() {
        let mut app = app_with_diff("one\nold\nthree\n", "one\nnew\nthree\n");
        app.diff_mode = DiffMode::SideBySide;
        app.show_line_numbers = false;
        app.show_minimap = false;
        app.sticky_scroll = false;
        let mut terminal = Terminal::new(TestBackend::new(80, 3)).expect("terminal");

        terminal
            .draw(|f| {
                draw_diff_original(f, &mut app, Rect::new(0, 0, 40, 3));
                crate::ui_editor::draw_editor(f, &mut app, Rect::new(40, 0, 40, 3));
                draw_diff_target_overlay(f, &mut app);
            })
            .expect("draw side-by-side diff");

        let gutter = app.gutter_area.expect("target gutter");
        assert!(gutter.width > 2, "diff target must reserve line numbers");
        let number_x = gutter.x + gutter.width - 2;
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((number_x, 0)).map(|cell| cell.symbol()), Some("1"));
        assert_eq!(buffer.cell((number_x, 1)).map(|cell| cell.symbol()), Some("2"));
        assert_eq!(buffer.cell((number_x, 2)).map(|cell| cell.symbol()), Some("3"));
    }

    #[test]
    fn side_by_side_layout_tracks_configured_ratio_and_reserves_divider() {
        let area = Rect::new(10, 2, 101, 20);
        let narrow_left = side_by_side_diff_layout(area, 0.25);
        let wide_left = side_by_side_diff_layout(area, 0.75);

        assert_eq!(narrow_left.divider.width, 1);
        assert_eq!(narrow_left.left.width + 1 + narrow_left.right.width, area.width);
        assert!(narrow_left.left.width < narrow_left.right.width);
        assert!(wide_left.left.width > wide_left.right.width);
        assert_eq!(narrow_left.right.right(), area.right());
        assert_eq!(wide_left.right.right(), area.right());
        assert_eq!(narrow_left.left.y, narrow_left.left_header.bottom());
        assert_eq!(narrow_left.right.y, narrow_left.right_header.bottom());
        assert_eq!(narrow_left.left.y, narrow_left.right.y);
        assert_eq!(narrow_left.left.bottom(), area.bottom());
    }

    #[test]
    fn diff_headers_keep_sources_visible_above_scrolled_content() {
        for mode in [DiffMode::SideBySide, DiffMode::Inline] {
            for (base, target) in [("one\nold\nlast\n", "one\nnew\nlast\n"), ("", "")] {
                let mut app = app_with_diff(base, target);
                app.diff_mode = mode;
                app.diff_labels = Some(DiffLabels::new("old/file.txt", "new/file.txt"));
                app.show_minimap = false;
                app.sticky_scroll = false;
                let editor = app.editor_mut().unwrap();
                editor.scroll_offset = usize::from(!target.is_empty());
                editor.modified = true;
                let area = Rect::new(2, 1, 100, 5);
                let mut terminal = Terminal::new(TestBackend::new(104, 7)).expect("terminal");

                terminal
                    .draw(|frame| match mode {
                        DiffMode::SideBySide => draw_side_by_side_diff(frame, &mut app, area),
                        DiffMode::Inline => draw_inline_diff(frame, &mut app, area),
                        DiffMode::Off => unreachable!(),
                    })
                    .expect("draw labeled diff");

                let header = buffer_row(terminal.backend().buffer(), area.y, 104);
                assert!(header.contains("old/file.txt"), "{header:?}");
                assert!(header.contains("new/file.txt (unsaved)"), "{header:?}");
                let code = app.code_area.expect("code area");
                assert!(code.y > area.y);
                if mode == DiffMode::SideBySide {
                    let layout = side_by_side_diff_layout(area, app.diff_split_ratio);
                    let buffer = terminal.backend().buffer();
                    assert_eq!(
                        buffer.cell((layout.left_header.x + 1, area.y)).unwrap().symbol(),
                        "o"
                    );
                    assert_eq!(
                        buffer.cell((layout.right_header.x + 1, area.y)).unwrap().symbol(),
                        "n"
                    );
                    assert_eq!(app.diff_left_code_area.unwrap().y, code.y);
                    assert_eq!(app.editor_area, Some(layout.right));
                } else {
                    assert!(header.contains("old/file.txt → new/file.txt"), "{header:?}");
                }
            }
        }
    }

    #[test]
    fn full_side_by_side_addition_never_colors_or_marks_left_pane() {
        let mut app = app_with_diff("one\nlast\n", "one\nadded\nlast\n");
        // Request a visible divider explicitly; the VS Code normal-theme
        // diffEditor.border default is null.
        app.colors.ui.border_diff = Some(Color::Rgb(80, 90, 100));
        app.diff_mode = DiffMode::SideBySide;
        app.show_minimap = false;
        app.sticky_scroll = false;
        let area = Rect::new(0, 0, 81, 4);
        let layout = side_by_side_diff_layout(area, app.diff_split_ratio);
        let added_bg = app.colors.bg_diff_added;
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("terminal");

        terminal
            .draw(|frame| draw_side_by_side_diff(frame, &mut app, area))
            .expect("draw side-by-side addition");

        let buffer = terminal.backend().buffer();
        for y in layout.left.top()..layout.left.bottom() {
            for x in layout.left.left()..layout.left.right() {
                let cell = buffer.cell((x, y)).expect("left pane cell");
                assert_ne!(cell.symbol(), "+");
                assert_ne!(cell.bg, added_bg);
            }
        }
        assert!((layout.right.left()..layout.right.right()).any(|x| {
            buffer
                .cell((x, layout.right.y + 1))
                .is_some_and(|cell| cell.symbol() == "+" && cell.bg == added_bg)
        }));
        assert_eq!(
            buffer
                .cell((layout.divider.x, layout.divider.y))
                .map(|cell| cell.symbol()),
            Some("│")
        );
    }

    #[test]
    fn full_side_by_side_deletion_never_colors_or_marks_right_pane() {
        let mut app = app_with_diff("one\nremoved\nlast\n", "one\nlast\n");
        app.diff_mode = DiffMode::SideBySide;
        app.show_minimap = false;
        app.sticky_scroll = false;
        let area = Rect::new(0, 0, 81, 4);
        let layout = side_by_side_diff_layout(area, app.diff_split_ratio);
        let removed_bg = app.colors.bg_diff_removed;
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("terminal");

        terminal
            .draw(|frame| draw_side_by_side_diff(frame, &mut app, area))
            .expect("draw side-by-side deletion");

        let buffer = terminal.backend().buffer();
        for y in layout.right.top()..layout.right.bottom() {
            for x in layout.right.left()..layout.right.right() {
                let cell = buffer.cell((x, y)).expect("right pane cell");
                assert_ne!(cell.symbol(), "-");
                assert_ne!(cell.bg, removed_bg);
            }
        }
        assert!((layout.left.left()..layout.left.right()).any(|x| {
            buffer
                .cell((x, layout.left.y + 1))
                .is_some_and(|cell| cell.symbol() == "-" && cell.bg == removed_bg)
        }));
    }

    #[test]
    fn diff_panes_expose_independent_horizontal_scrollbars() {
        let long_base = format!("{}\n", "left".repeat(30));
        let long_target = format!("{}\n", "right".repeat(30));
        let mut app = app_with_diff(&long_base, &long_target);
        app.diff_mode = DiffMode::SideBySide;
        app.show_minimap = false;
        app.sticky_scroll = false;
        app.cached_max_line_width = 150;
        app.diff_left_horizontal_scroll = 7;
        app.editor_mut().expect("editor").horizontal_scroll = 13;
        let area = Rect::new(0, 0, 81, 4);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height))
            .expect("terminal");

        terminal
            .draw(|frame| draw_side_by_side_diff(frame, &mut app, area))
            .expect("draw side-by-side scrollbars");

        let left = app
            .diff_left_h_scrollbar_area
            .expect("left diff scrollbar");
        let right = app.h_scrollbar_area.expect("right diff scrollbar");
        assert!(left.right() < right.x);
        assert_eq!(app.diff_left_horizontal_scroll, 7);
        assert_eq!(app.editor().expect("editor").horizontal_scroll, 13);
    }

    fn diff_wheel(
        app: &mut App,
        point: (u16, u16),
        kind: crossterm::event::MouseEventKind,
        modifiers: crossterm::event::KeyModifiers,
    ) {
        crate::mouse::handle_mouse(
            app,
            crossterm::event::MouseEvent {
                kind,
                column: point.0,
                row: point.1,
                modifiers,
            },
        );
    }

    #[test]
    fn side_by_side_diff_horizontal_wheel_follows_hovered_pane() {
        use crossterm::event::{KeyModifiers, MouseEventKind};

        for (forward, backward, modifiers) in [
            (
                MouseEventKind::ScrollDown,
                MouseEventKind::ScrollUp,
                KeyModifiers::SHIFT,
            ),
            (
                MouseEventKind::ScrollRight,
                MouseEventKind::ScrollLeft,
                KeyModifiers::NONE,
            ),
        ] {
            for ratio in [0.3, 0.7] {
                for word_wrap in [false, true] {
                    let base = format!("{}LEFT_END\n", "abcdefgh".repeat(20));
                    let target = format!("{}RIGHT_END\n", "ijklmnop".repeat(20));
                    let mut app = app_with_diff(&base, &target);
                    app.diff_mode = DiffMode::SideBySide;
                    app.diff_split_ratio = ratio;
                    app.show_minimap = false;
                    app.sticky_scroll = false;
                    app.cached_max_line_width = 169;
                    app.editor_mut().unwrap().word_wrap = word_wrap;
                    let area = Rect::new(2, 1, 81, 6);
                    let layout = side_by_side_diff_layout(area, ratio);
                    let mut terminal = Terminal::new(TestBackend::new(85, 8)).unwrap();
                    terminal
                        .draw(|f| draw_side_by_side_diff(f, &mut app, area))
                        .unwrap();
                    let left = app.diff_left_code_area.unwrap();
                    let right = app.code_area.unwrap();
                    let bar = app.diff_left_h_scrollbar_area.unwrap();

                    // Text, gutter, header and scrollbar all belong to the original pane.
                    for point in [
                        (left.x, left.y),
                        (layout.left.x, left.y),
                        (layout.left_header.x, layout.left_header.y),
                        (bar.x, bar.y),
                    ] {
                        diff_wheel(&mut app, point, forward, modifiers);
                        assert_eq!(app.diff_left_horizontal_scroll, 6);
                        assert_eq!(app.editor().unwrap().horizontal_scroll, 0);
                        terminal
                            .draw(|f| draw_side_by_side_diff(f, &mut app, area))
                            .unwrap();
                        assert_eq!(
                            terminal
                                .backend()
                                .buffer()
                                .cell((left.x, left.y))
                                .unwrap()
                                .symbol(),
                            "g"
                        );
                        diff_wheel(&mut app, point, backward, modifiers);
                        assert_eq!(app.diff_left_horizontal_scroll, 0);
                    }

                    if !word_wrap {
                        diff_wheel(&mut app, (right.x, right.y), forward, modifiers);
                        assert_eq!(app.editor().unwrap().horizontal_scroll, 6);
                        assert_eq!(app.diff_left_horizontal_scroll, 0);
                        diff_wheel(&mut app, (right.x, right.y), backward, modifiers);
                    }
                    for _ in 0..40 {
                        diff_wheel(&mut app, (left.x, left.y), forward, modifiers);
                    }
                    assert_eq!(
                        app.diff_left_horizontal_scroll,
                        app.diff_render_cache
                            .max_left_text_width
                            .saturating_sub(left.width as usize)
                    );
                    terminal
                        .draw(|f| draw_side_by_side_diff(f, &mut app, area))
                        .unwrap();
                    assert!(
                        buffer_row(terminal.backend().buffer(), left.y, 85).contains("LEFT_END")
                    );
                    for _ in 0..40 {
                        diff_wheel(&mut app, (left.x, left.y), backward, modifiers);
                    }
                    assert_eq!(app.diff_left_horizontal_scroll, 0);
                    let editor = app.editor().unwrap();
                    assert_eq!(
                        (
                            editor.horizontal_scroll,
                            editor.scroll_offset,
                            editor.cursor_line,
                            editor.cursor_col
                        ),
                        (0, 0, 0, 0)
                    );
                }
            }
        }
    }

    #[test]
    fn side_by_side_diff_horizontal_wheel_preserves_inactive_split_focus() {
        use crate::app::{SplitDirection, TabKind};
        use crossterm::event::{KeyModifiers, MouseEventKind};

        let mut app = app_with_diff(&format!("{}\n", "left".repeat(40)), "right\n");
        app.diff_mode = DiffMode::SideBySide;
        app.show_minimap = false;
        app.sticky_scroll = false;
        app.tabs[0].kind = TabKind::Diff(1);
        app.diff_tabs.owner = Some(1);
        app.tabs.insert(0, Tab::new_file(Editor::new()));
        app.active_tab = 1;
        app.create_split_group_dir(1, SplitDirection::Horizontal);
        let diff_group = app.active_group;
        let mut terminal = Terminal::new(TestBackend::new(160, 15)).unwrap();
        let render = |app: &mut App, terminal: &mut Terminal<TestBackend>| {
            terminal
                .draw(|frame| crate::ui_chrome::draw_editor_section(frame, app, frame.area()))
                .unwrap();
        };
        render(&mut app, &mut terminal);
        let left = app.diff_left_code_area.unwrap();
        app.switch_group(0);
        render(&mut app, &mut terminal);
        diff_wheel(
            &mut app,
            (left.x, left.y),
            MouseEventKind::ScrollDown,
            KeyModifiers::SHIFT,
        );
        render(&mut app, &mut terminal);
        diff_wheel(
            &mut app,
            (left.x, left.y),
            MouseEventKind::ScrollRight,
            KeyModifiers::NONE,
        );
        assert_eq!(app.active_group, 0);
        assert_eq!(app.editor().unwrap().horizontal_scroll, 0);
        render(&mut app, &mut terminal);
        app.switch_group(diff_group);
        render(&mut app, &mut terminal);
        assert_eq!(app.diff_left_horizontal_scroll, 12);
        assert_eq!(app.editor().unwrap().horizontal_scroll, 0);
    }

    #[test]
    fn original_diff_slices_pathological_lines_at_horizontal_scroll() {
        let width = 40;
        let prefix = crate::highlight::MAX_SYNTAX_LINE_CHARS + 256;
        let long = format!("{}TAIL{}\n", "x".repeat(prefix), "y".repeat(width as usize));
        let mut app = app_with_diff(&long, &long);
        app.diff_left_horizontal_scroll = prefix;
        let mut terminal = Terminal::new(TestBackend::new(width, 3)).expect("terminal");

        terminal
            .draw(|frame| draw_diff_original(frame, &mut app, Rect::new(0, 0, width, 3)))
            .expect("bounded original diff draw");

        let code = app.diff_left_code_area.expect("left code area");
        let rendered: String = (code.left()..code.right())
            .filter_map(|x| terminal.backend().buffer().cell((x, code.y)))
            .map(|cell| cell.symbol())
            .collect();
        assert!(rendered.starts_with("TAIL"), "rendered row: {rendered:?}");
    }

    #[test]
    fn target_line_numbers_fit_the_allocated_gutter() {
        let mut app = app_with_diff("one\nold\n", "one\nnew\n");
        app.diff_mode = DiffMode::SideBySide;
        app.gutter_area = Some(Rect::new(0, 0, 5, 2));
        app.code_area = Some(Rect::new(5, 0, 35, 2));
        std::sync::Arc::make_mut(&mut app.diff_render_cache).rows[0].right_line = Some(99_999);
        let mut terminal = Terminal::new(TestBackend::new(40, 2)).expect("terminal");

        terminal
            .draw(|f| draw_diff_target_overlay(f, &mut app))
            .expect("draw target diff");

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer.cell((1, 0)).map(|cell| cell.symbol()), Some("1"));
        assert_eq!(buffer.cell((3, 1)).map(|cell| cell.symbol()), Some("2"));
    }

    #[test]
    fn inline_diff_layout_reserves_minimap_and_scrollbar_when_wide_enough() {
        let layout = inline_diff_layout(Rect::new(0, 0, 120, 20), true, 10);

        assert!(layout.minimap_area.is_some());
        assert_eq!(layout.scrollbar_area.width, 1);
        assert!(layout.code_area.width < 120);
    }

    #[test]
    fn inline_diff_minimap_separator_paints_the_theme_background() {
        for background in [Color::Rgb(45, 42, 46), Color::Rgb(248, 246, 250)] {
            for wide_line in [false, true] {
                let target = if wide_line { "x".repeat(300) } else { "short".into() };
                let mut app = app_with_diff("original\n", &target);
                app.diff_mode = DiffMode::Inline;
                app.show_minimap = true;
                app.minimap_width = 10;
                app.colors.bg_editor = background;
                let area = Rect::new(3, 2, 120, 12);
                let mut terminal = Terminal::new(TestBackend::new(128, 16)).unwrap();
                terminal.draw(|frame| {
                    let screen = frame.area();
                    frame.buffer_mut().set_style(screen, Style::default().bg(Color::Black));
                    draw_inline_diff(frame, &mut app, area);
                }).unwrap();

                let minimap = app.minimap_area.expect("visible minimap");
                assert_eq!(app.h_scrollbar_area.is_some(), wide_line);
                for y in minimap.y..minimap.bottom() {
                    let cell = &terminal.backend().buffer()[(minimap.x - 1, y)];
                    assert_eq!(cell.symbol(), "│");
                    assert_eq!(cell.bg, background, "separator at row {y}");
                    assert_eq!(cell.fg, app.colors.ui.fg_overview_border);
                }
                assert_eq!(terminal.backend().buffer()[(area.x - 1, area.y)].bg, Color::Black);
            }
        }
    }

    #[test]
    fn inline_diff_scrollbar_end_caps_keep_theme_colors_across_layouts_and_replay() {
        let base = format!("{}\n{}", "original".repeat(60), "same\n".repeat(30));
        let target = format!("{}\n{}", "modified".repeat(60), "same\n".repeat(30));
        for (background, gutter) in [
            (Color::Rgb(45, 42, 46), Color::Rgb(53, 50, 54)),
            (Color::Rgb(248, 246, 250), Color::Rgb(230, 228, 232)),
        ] {
            for compact in [false, true] {
                let mut app = app_with_diff(&base, &target);
                app.diff_mode = DiffMode::Inline;
                app.minimap_width = 10;
                app.colors.bg_editor = background;
                app.colors.bg_dark = gutter;
                app.animation_settings.set(crate::animation_config::AnimationEffect::CursorSmear, true);
                if compact {
                    app.diff_render_cache = std::sync::Arc::new(app.diff_render_cache.with_context(2));
                }
                // Reuse the view while resizing and toggling the minimap. The
                // offset viewport also models an editor inside a split pane.
                for (width, minimap) in [(120, true), (60, true), (120, false)] {
                    app.show_minimap = minimap;
                    let area = Rect::new(3, 2, width, 12);
                    let mut terminal = Terminal::new(TestBackend::new(128, 16)).unwrap();
                    for cosmetic in [false, true] {
                        app.rendering_animation_frame = cosmetic;
                        let replays = app.animation_frame_cache.replays;
                        terminal.draw(|frame| {
                            let screen = frame.area();
                            frame.buffer_mut().set_style(screen, Style::default().bg(Color::Black));
                            draw_inline_diff(frame, &mut app, area);
                        }).unwrap();
                        let bar = app.h_scrollbar_area.expect("long lines need a horizontal scrollbar");
                        let buffer = terminal.backend().buffer();
                        for x in area.x..bar.x {
                            assert_eq!(buffer[(x, bar.y)].bg, gutter, "gutter cap {x}, cosmetic={cosmetic}");
                        }
                        for x in bar.right()..area.right() {
                            assert_eq!(buffer[(x, bar.y)].bg, background, "overview cap {x}, cosmetic={cosmetic}");
                        }
                        assert_eq!(buffer[(area.x - 1, bar.y)].bg, Color::Black);
                        assert_eq!(buffer[(area.right(), bar.y)].bg, Color::Black);
                        if cosmetic {
                            assert_eq!(app.animation_frame_cache.replays, replays + 1);
                        }
                    }
                }
            }
        }
    }

    fn draw_inline(app: &mut App, terminal: &mut Terminal<TestBackend>) {
        terminal.draw(|frame| draw_inline_diff(frame, app, frame.area())).unwrap();
    }

    fn wait_for_visible_inline_highlights(app: &mut App) {
        app.refresh_visible_highlights_async();
        app.wait_for_background(std::time::Duration::from_secs(5), |app| {
            let editor = app.editor().unwrap();
            app.diff_render_cache.inline_rows.iter()
                .skip(app.diff_inline_scroll.row)
                .take(app.diff_inline_scroll.text_area.height as usize)
                .all(|inline| {
                    let row = &app.diff_render_cache.rows[inline.aligned_row];
                    let (line, hash) = if inline.kind == '-' {
                        (row.left_line.unwrap(), row.left_highlight_hash())
                    } else {
                        (row.right_line.unwrap(), row.right_highlight_hash())
                    };
                    app.highlighter.highlighted_fragment_by_hash_if_cached(
                        hash, line, editor.syntax_file_path(),
                        editor.syntax_language_token(), &app.syntax_theme,
                    ).is_some()
                })
        });
    }

    #[test]
    fn inline_diff_syntax_colors_match_both_side_by_side_panes() {
        let mut app = app_with_diff(
            "fn demo() {\n    let message = \"before\";\n    // unchanged comment\n}\n",
            "fn demo() {\n    let message = \"after\";\n    // unchanged comment\n}\n",
        );
        app.show_minimap = false;
        app.sticky_scroll = false;
        app.cursor_visible = false;
        let mut terminal = Terminal::new(TestBackend::new(140, 9)).unwrap();
        let mut keyword_colors = Vec::new();
        for (theme, path, language) in [
            ("dark-plus", "example.rs", None),
            ("monokai", "example.txt", Some("Rust")),
        ] {
            app.syntax_theme = theme.into();
            app.editor_mut().unwrap().file_path = Some(path.into());
            app.editor_mut().unwrap().language_override = language.map(str::to_string);
            app.diff_mode = DiffMode::Inline;
            draw_inline(&mut app, &mut terminal);
            wait_for_visible_inline_highlights(&mut app);

            app.diff_mode = DiffMode::SideBySide;
            terminal.draw(|frame| draw_side_by_side_diff(frame, &mut app, frame.area())).unwrap();
            let left = app.diff_left_code_area.unwrap();
            let right = app.code_area.unwrap();
            let reference = terminal.backend().buffer().clone();

            app.diff_mode = DiffMode::Inline;
            draw_inline(&mut app, &mut terminal);
            let code = app.code_area.unwrap();
            let buffer = terminal.backend().buffer();
            let mut colors = Vec::new();
            for (line, inline) in app.diff_render_cache.inline_rows.iter().enumerate() {
                let row = &app.diff_render_cache.rows[inline.aligned_row];
                let (source, x, y) = if inline.kind == '-' {
                    (&row.left_text, left.x, left.y)
                } else {
                    (&row.right_text, right.x + 2, right.y)
                };
                for (column, character) in source.chars().enumerate() {
                    let actual = buffer.cell((code.x + column as u16, code.y + line as u16)).unwrap();
                    let expected = reference.cell((x + column as u16, y + inline.aligned_row as u16)).unwrap();
                    assert_eq!(actual.symbol(), character.to_string());
                    assert_eq!(actual.fg, expected.fg, "{theme}: {source} at {column}");
                    if inline.kind != ' ' {
                        assert_eq!(actual.bg, expected.bg, "character diff backgrounds must survive");
                    }
                    if !character.is_whitespace() && !colors.contains(&actual.fg) {
                        colors.push(actual.fg);
                    }
                }
            }
            assert!(colors.len() >= 3, "keywords, strings and comments need syntax colors");
            keyword_colors.push(buffer.cell((code.x, code.y)).unwrap().fg);
        }
        assert_ne!(keyword_colors[0], keyword_colors[1], "theme changes must recolor inline diffs");
    }

    #[test]
    fn inline_diff_syntax_colors_follow_unicode_horizontal_scrolling() {
        let mut app = app_with_diff(
            "let label = \"界e\u{301}old\"; let count = 123; // trailing comment\n",
            "let label = \"界e\u{301}new\"; let count = 456; // trailing comment\n",
        );
        app.editor_mut().unwrap().file_path = Some("example.rs".into());
        app.diff_mode = DiffMode::Inline;
        app.show_minimap = false;
        let mut full = Terminal::new(TestBackend::new(100, 5)).unwrap();
        draw_inline(&mut app, &mut full);
        wait_for_visible_inline_highlights(&mut app);
        draw_inline(&mut app, &mut full);
        let reference_area = app.code_area.unwrap();
        let reference = full.backend().buffer().clone();
        let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
        for column in [13, 14, 15, 16, 18, 23] {
            app.editor_mut().unwrap().horizontal_scroll = column;
            draw_inline(&mut app, &mut terminal);
            let code = app.code_area.unwrap();
            assert_eq!(app.diff_inline_scroll.column, column);
            for row in 0..2 {
                for offset in 0..code.width {
                    let actual = terminal.backend().buffer().cell((code.x + offset, code.y + row)).unwrap();
                    let expected = reference.cell((reference_area.x + column as u16 + offset, reference_area.y + row)).unwrap();
                    assert_eq!(actual.symbol(), expected.symbol(), "scroll {column}, row {row}, offset {offset}");
                    if !actual.symbol().trim().is_empty() {
                        assert_eq!(actual.fg, expected.fg, "syntax colors must scroll with their graphemes");
                        assert_eq!(actual.bg, expected.bg, "character diff offsets must scroll with their graphemes");
                    }
                }
            }
        }
    }

    #[test]
    fn inline_diff_highlights_deleted_viewports_when_target_scroll_stays_fixed() {
        let base: String = (0..250).map(|line| format!("let removed_{line} = {line};\n")).collect();
        let mut app = app_with_diff(&base, "");
        app.editor_mut().unwrap().file_path = Some("example.rs".into());
        app.diff_mode = DiffMode::Inline;
        app.show_minimap = false;
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        draw_inline(&mut app, &mut terminal);
        wait_for_visible_inline_highlights(&mut app);
        let target_scroll = app.editor().unwrap().scroll_offset;
        for _ in 0..15 {
            assert!(app.inline_diff_navigation(crossterm::event::KeyCode::PageDown));
        }
        draw_inline(&mut app, &mut terminal);
        assert!(app.diff_inline_scroll.row > 100);
        assert_eq!(app.editor().unwrap().scroll_offset, target_scroll);
        wait_for_visible_inline_highlights(&mut app);
        draw_inline(&mut app, &mut terminal);
        let code = app.code_area.unwrap();
        let buffer = terminal.backend().buffer();
        let keyword = buffer.cell((code.x, code.y)).unwrap();
        let number = buffer.cell((code.x + 18, code.y)).unwrap();
        assert_eq!(keyword.symbol(), "l");
        assert_ne!(keyword.fg, number.fg, "deleted code must keep syntax colors after scrolling");
        assert_eq!(keyword.bg, app.colors.bg_diff_removed);
    }

    #[test]
    fn inline_diff_keeps_removed_lines_at_the_top_of_the_viewport() {
        for (base, target, scroll, expected) in [
            ("before\n", "after\n", 0, vec!["- before", "+ after"]),
            ("gone\nkept\n", "kept\n", 0, vec!["- gone", "kept"]),
            (
                "kept\nbefore\n",
                "kept\nafter\n",
                1,
                vec!["- before", "+ after"],
            ),
        ] {
            let mut app = app_with_diff(base, target);
            app.show_minimap = false;
            app.editor_mut().unwrap().scroll_offset = scroll;
            let mut terminal = Terminal::new(TestBackend::new(60, 3)).unwrap();
            terminal
                .draw(|frame| draw_inline_diff(frame, &mut app, Rect::new(0, 0, 60, 3)))
                .unwrap();
            for (row, text) in expected.iter().enumerate() {
                assert!(buffer_row(terminal.backend().buffer(), row as u16 + 1, 60).contains(text));
            }
        }
    }

    #[test]
    fn draw_inline_diff_sets_minimap_and_scrollbar_areas() {
        let mut app = App::test_new();
        let mut editor = Editor::new();
        editor.file_path = Some(PathBuf::from("file.txt"));
        editor.rope = ropey::Rope::from_str("after\nline2\n");
        app.tabs.push(Tab::new_file(editor));
        app.active_tab = 0;
        app.compare_base_content = Some("before\nline2\n".to_string().into());
        app.set_compare_diff_lines(crate::app::compute_file_diff(
            "before\nline2\n",
            "after\nline2\n",
        ));

        let backend = TestBackend::new(120, 20);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| {
                draw_inline_diff(f, &mut app, Rect::new(0, 0, 120, 20));
            })
            .expect("draw");

        assert!(app.minimap_area.is_some());
        assert!(app.scrollbar_area.is_some());
    }
}
