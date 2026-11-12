use rustc_hash::FxHashMap;
use crate::line_changes::LineChanges;
use crate::ui_colors::{ChangeSurface, ensure_contrast};
use std::ops::Range;

#[path = "ui_scrollbar_changes.rs"]
mod scrollbar_changes;
pub(crate) use scrollbar_changes::ChangeRows;

#[path = "ui_minimap_cache.rs"]
mod cache;
pub(crate) use cache::MinimapCache;
#[cfg(test)]
use cache::Glyph;

#[cfg(test)]
use ratatui::{
    text::{Line, Span},
    widgets::Paragraph,
};

#[cfg(test)]
#[path = "ui_editor_minimap_reference.rs"]
mod reference;

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
};

/// Immutable sorted line buckets built when a provider/source changes. Range
/// queries inspect only the small number of priorities, never every marker in
/// a compressed overview row.
#[derive(Default)]
pub(crate) struct LinePriorityIndex<T> {
    buckets: Vec<(T, Vec<usize>)>,
}

pub(crate) type DiagnosticLineIndex = LinePriorityIndex<u8>;

impl<T: Copy> LinePriorityIndex<T> {
    fn build(lines: impl IntoIterator<Item = (usize, T)>, priority: impl Fn(T) -> u8) -> Self {
        let mut buckets: FxHashMap<u8, (T, Vec<usize>)> = FxHashMap::default();
        for (line, value) in lines {
            buckets
                .entry(priority(value))
                .or_insert_with(|| (value, Vec::new()))
                .1
                .push(line);
        }
        let mut buckets: Vec<_> = buckets.into_values().collect();
        buckets.sort_unstable_by_key(|&(value, _)| priority(value));
        for (_, lines) in &mut buckets {
            lines.sort_unstable();
            lines.dedup();
        }
        Self { buckets }
    }

    pub(crate) fn best_in_range(&self, start: usize, end: usize) -> Option<T> {
        self.best_in_range_inner(start, end, false)
    }

    fn best_in_range_inner(&self, start: usize, end: usize, include_tail: bool) -> Option<T> {
        if start >= end {
            return None;
        }
        self.buckets.iter().find_map(|&(value, ref lines)| {
            let index = lines.partition_point(|&line| line < start);
            lines
                .get(index)
                .filter(|&&line| include_tail || line < end)
                .map(|_| value)
        })
    }
}

impl DiagnosticLineIndex {
    pub(crate) fn from_diagnostics(lines: &[(usize, u8)]) -> Self {
        Self::build(lines.iter().copied(), |severity| severity)
    }
}

/// Separate buckets retain every kind in a compressed row while keeping
/// queries bounded by the number of kinds, independent of the file size.
#[derive(Default)]
pub(crate) struct ChangeLineIndex([Vec<usize>; LineChanges::BIT_COUNT]);

impl ChangeLineIndex {
    pub(crate) fn from_changes<T: Copy + Into<LineChanges>>(changes: &[(usize, T)]) -> Self {
        let mut index = Self::default();
        for &(line, change) in changes {
            let bits = change.into().bits();
            for (bit, lines) in index.0.iter_mut().enumerate() {
                if bits & (1 << bit) != 0 {
                    lines.push(line);
                }
            }
        }
        for lines in &mut index.0 {
            lines.sort_unstable();
            lines.dedup();
        }
        index
    }

    fn in_range(&self, start: usize, end: usize, include_tail: bool) -> LineChanges {
        if start >= end {
            return LineChanges::default();
        }
        let mut bits = 0;
        for (bit, lines) in self.0.iter().enumerate() {
            let index = lines.partition_point(|&line| line < start);
            if lines.get(index).is_some_and(|&line| include_tail || line < end) {
                bits |= 1 << bit;
            }
        }
        LineChanges::from_bits(bits)
    }
}

fn has_search_match_in_range(
    sorted_matches: &[(usize, usize, usize)],
    start: usize,
    end: usize,
) -> bool {
    if start >= end || sorted_matches.is_empty() {
        return false;
    }
    let index = sorted_matches.partition_point(|&(line, _, _)| line < start);
    sorted_matches
        .get(index)
        .is_some_and(|&(line, _, _)| line < end)
}

/// Returns the worst (lowest number = highest priority) diagnostic severity in a line range.
/// severity: 1=error, 2=warning, 3=info, 4=hint
fn worst_diagnostic_in_range(diag_lines: &[(usize, u8)], start: usize, end: usize) -> Option<u8> {
    if start >= end || diag_lines.is_empty() {
        return None;
    }
    let idx = diag_lines.partition_point(|&(line, _)| line < start);
    let mut worst: Option<u8> = None;
    for &(line, sev) in &diag_lines[idx..] {
        if line >= end {
            break;
        }
        worst = Some(worst.map_or(sev, |w: u8| w.min(sev)));
        if worst == Some(1) {
            return worst; // can't get worse than error
        }
    }
    worst
}

fn diagnostic_marker_color(severity: u8, colors: &crate::app_theme::ThemeColors) -> Color {
    match severity {
        1 => colors.ui.fg_minimap_error,
        2 => colors.ui.fg_minimap_warning,
        3 => colors.ui.fg_minimap_info,
        _ => colors.fg_dim,
    }
}

fn selection_overlaps_range(
    selection_lines: Option<(usize, usize)>,
    start: usize,
    end: usize,
) -> bool {
    if start >= end {
        return false;
    }
    selection_lines.is_some_and(|(sl, el)| sl < end && el >= start)
}

fn dim_color(c: Color, scale: u8, surface: Color) -> Color {
    match c {
        Color::Rgb(r, g, b) => crate::theme_workbench::ColorLayer::rgba(r, g, b, scale)
            .over(surface)
            .unwrap_or(c),
        other => other,
    }
}

fn minimap_row_changes(p: MinimapParams<'_>, start: usize, end: usize) -> LineChanges {
    if p.diff_changes.is_none() && p.editor.large_file_mode {
        return LineChanges::default();
    }
    let changes = p.change_index.map_or_else(
        || match p.diff_changes {
            Some(diff) => minimap_change_in_range(diff.lines, start, end),
            None => minimap_change_in_range(p.editor.git_change_lines(), start, end),
        },
        |index| index.in_range(start, end, false),
    );
    match p.diff_changes {
        Some(diff) if diff.staged => changes.as_staged_diff(),
        None if p.editor.has_unsaved_change_in_range(start, end) => {
            changes.merge(LineChanges::UNSAVED)
        }
        _ => changes,
    }
}

struct MinimapRowMarkers {
    background: Color,
    cells: [(&'static str, Style); 3],
    width: usize,
}

fn minimap_row_markers(
    changes: LineChanges,
    diagnostic: Option<u8>,
    has_search_match: bool,
    width: usize,
    bg: Color,
    colors: &crate::app_theme::ThemeColors,
) -> MinimapRowMarkers {
    let working = changes
        .working()
        .map(|kind| ChangeSurface::Gutter { staged: false }.color(kind, colors));
    let staged = changes
        .staged()
        .map(|kind| ChangeSurface::Gutter { staged: true }.color(kind, colors));
    let change_color = working.or(staged);
    let background = if has_search_match {
        dim_color(colors.ui.fg_minimap_find, 100, bg)
    } else {
        change_color.map_or(bg, |color| dim_color(color, 56, bg))
    };
    // The viewport border skips bold cells to keep markers visible.
    let marker_style = |bg| Style::default().bg(bg).add_modifier(Modifier::BOLD);
    let mut cells = [(" ", Style::default().bg(background)); 3];
    let mut count = 0;
    if let Some(severity) = diagnostic {
        let style = marker_style(background).fg(diagnostic_marker_color(severity, colors));
        count = if change_color.is_some() { 1 } else { cells.len() };
        cells[..count].fill(("█", style));
    }
    if let Some(color) = staged {
        let ink = ensure_contrast(
            background,
            color,
            4.5,
            &[colors.fg_text, Color::Rgb(0, 0, 0), Color::Rgb(255, 255, 255)],
        );
        cells[count] = ("•", marker_style(color).fg(ink));
        count += 1;
    }
    if let Some(color) = working {
        cells[count] = (" ", marker_style(color));
        count += 1;
    }
    MinimapRowMarkers {
        background,
        cells,
        width: width.min(count),
    }
}

#[cfg(test)]
pub(crate) fn scrollbar_change_rows(
    editor: &crate::editor::Editor,
    height: usize,
    diff_changes: Option<&[(usize, char)]>,
) -> ChangeRows {
    if editor.large_file_mode && diff_changes.is_none() {
        return ChangeRows::default();
    }
    let total_lines = editor.line_count().max(1);
    if let Some(changes) = diff_changes {
        return ChangeRows::from_lines(height, total_lines, changes.iter().copied());
    }
    let changes = editor.git_change_lines().iter().copied().chain(
        (0..total_lines)
            .filter(|&line| editor.has_unsaved_change_in_range(line, line + 1))
            .map(|line| (line, LineChanges::UNSAVED)),
    );
    ChangeRows::from_lines(height, total_lines, changes)
}

#[cfg(test)]
#[path = "ui_editor_minimap_performance_tests.rs"]
mod performance_tests;

#[cfg(test)]
#[path = "ui_minimap_ux_tests.rs"]
mod ux_tests;

pub(crate) fn scrollbar_change_rows_indexed(
    editor: &crate::editor::Editor,
    height: usize,
    compare: bool,
    changes: &ChangeLineIndex,
) -> ChangeRows {
    if height == 0 || editor.large_file_mode && !compare {
        return ChangeRows::default();
    }
    let total_lines = editor.line_count().max(1);
    ChangeRows::project(height, editor.overview_total_rows(), |start, end| {
        let start = editor.overview_source_boundary(start);
        let end = editor.overview_source_boundary(end);
        // Include stale markers beyond EOF in the last source row.
        let change = changes.in_range(start, end, end == total_lines);
        if !compare && editor.has_unsaved_change_in_range(start, end) {
            change.merge(LineChanges::UNSAVED)
        } else { change }
    })
}

#[derive(Clone, Copy)]
pub(crate) struct MinimapDiffChanges<'a> {
    pub(crate) lines: &'a [(usize, char)],
    pub(crate) staged: bool,
}

#[derive(Clone, Copy)]
pub(crate) struct MinimapParams<'a> {
    pub(crate) area: Rect,
    pub(crate) editor: &'a crate::editor::Editor,
    pub(crate) scroll: usize,
    pub(crate) visible_lines: usize,
    pub(crate) search_matches: &'a [(usize, usize, usize)],
    pub(crate) diagnostic_lines: &'a [(usize, u8)],
    pub(crate) diagnostic_index: Option<&'a DiagnosticLineIndex>,
    pub(crate) change_index: Option<&'a ChangeLineIndex>,
    pub(crate) diff_changes: Option<MinimapDiffChanges<'a>>,
    pub(crate) colors: crate::app_theme::ThemeColors,
    pub(crate) hovered: bool,
    pub(crate) active: bool,
}

/// The document slice rendered by the minimap, including blank space below EOF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MinimapWindow {
    start: usize,
    end: usize,
}

impl MinimapWindow {
    fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }
}

fn minimap_window(
    total_lines: usize,
    scroll: usize,
    visible_lines: usize,
    height: usize,
) -> MinimapWindow {
    if height == 0 || total_lines == 0 {
        return MinimapWindow { start: 0, end: 0 };
    }
    let visible = visible_lines.max(1).min(total_lines);
    let scale = visible.div_ceil(height.saturating_sub(1).max(1)).max(4);
    let bands = height.max(2);
    let len = bands.saturating_mul(scale);
    if total_lines <= len {
        return MinimapWindow { start: 0, end: total_lines };
    }
    let max_scroll = total_lines - visible;
    let top_band = scroll.min(max_scroll) / scale;
    let last_top_band = max_scroll / scale;
    // Scroll intact bands and pad the final one. Choose the viewport's local
    // top band before positioning the window so both move monotonically.
    let last_window_band = total_lines.div_ceil(scale) - bands;
    let travel = last_top_band.saturating_sub(last_window_band);
    let local_top = (top_band as u128 * travel as u128 / last_top_band.max(1) as u128) as usize;
    let start = (top_band - local_top) * scale;
    MinimapWindow {
        start,
        end: start.saturating_add(len),
    }
}

/// Shared geometry for painting, hit testing, and dragging the viewport.
#[derive(Clone, Copy)]
pub(crate) struct MinimapLayout {
    window: MinimapWindow,
    height: usize,
    extent: usize,
    visible: usize,
    scroll: usize,
    max_scroll: usize,
    pub(crate) view_start: usize,
    pub(crate) view_end: usize,
}

impl MinimapLayout {
    fn for_editor(editor: &crate::editor::Editor, height: usize) -> Self {
        Self::new(
            editor.visible_line_count(),
            editor.visible_scroll_offset(),
            editor.minimap_source_rows(editor.viewport_height),
            height,
            editor.scroll_past_end,
        )
    }

    pub(crate) fn new(
        total_lines: usize,
        scroll: usize,
        visible_lines: usize,
        height: usize,
        scroll_past_end: bool,
    ) -> Self {
        // Match the editor's range: the last line can reach the top of the
        // viewport. Include the remaining blank rows in both the content map
        // and slider geometry so painting, clicks, and drags use one extent.
        let scrollable_lines = if scroll_past_end && total_lines > 0 {
            total_lines.saturating_add(visible_lines.saturating_sub(1))
        } else {
            total_lines
        };
        let visible = visible_lines.max(1).min(scrollable_lines);
        let max_scroll = scrollable_lines.saturating_sub(visible);
        let scroll = scroll.min(max_scroll);
        let window = minimap_window(scrollable_lines, scroll, visible, height);
        // The first and last visible lines may share bands with offscreen
        // text. Include those bands using the same boundaries as the markers.
        let view_start = minimap_row_after(scroll.saturating_add(1), window, height).saturating_sub(1);
        let view_end = minimap_row_after(scroll.saturating_add(visible), window, height);
        Self {
            window,
            height,
            extent: scrollable_lines,
            visible,
            scroll,
            max_scroll,
            view_start,
            view_end,
        }
    }

    /// Clicking the slider grabs it; clicking content centers that local line.
    pub(crate) fn click_scroll(&self, row: usize) -> usize {
        if (self.view_start..self.view_end).contains(&row) || self.height == 0 {
            return self.scroll;
        }
        let (start, end) = minimap_source_range(self.window, row.min(self.height - 1), self.height);
        let line = start + end.saturating_sub(start) / 2;
        line.saturating_sub(self.visible / 2).min(self.max_scroll)
    }

    /// Anchor the drag to mouse-down so rounding and the moving content cannot
    /// accumulate drift. The slider can traverse the entire document.
    pub(crate) fn drag_scroll(&self, start_row: usize, row: usize) -> usize {
        let view_start = |scroll| {
            let window = minimap_window(self.extent, scroll, self.visible, self.height);
            minimap_row_after(scroll.saturating_add(1), window, self.height).saturating_sub(1)
        };
        let last = view_start(self.max_scroll);
        if row == start_row || last == 0 {
            return self.scroll;
        }
        let target = self.view_start.saturating_add(row).saturating_sub(start_row);
        if target == 0 {
            return 0;
        }
        if target >= last {
            return self.max_scroll;
        }
        // Invert the painted position, including quantized window shifts.
        let mut first = 0;
        let mut end = self.max_scroll;
        while first < end {
            let middle = first + (end - first) / 2;
            if view_start(middle) < target {
                first = middle + 1;
            } else {
                end = middle;
            }
        }
        first
    }
}

#[derive(Clone, Copy)]
pub(crate) struct MinimapDrag {
    row: usize,
    layout: MinimapLayout,
    wrap_scroll: usize,
}

impl MinimapDrag {
    pub(crate) fn new(editor: &crate::editor::Editor, row: usize, height: usize) -> Self {
        Self {
            row,
            layout: MinimapLayout::for_editor(editor, height),
            wrap_scroll: editor.wrap_scroll_offset,
        }
    }

    pub(crate) fn begin(editor: &mut crate::editor::Editor, row: usize, height: usize) -> Self {
        let target = MinimapLayout::for_editor(editor, height).click_scroll(row);
        Self::scroll(editor, target);
        Self::new(editor, row, height)
    }

    pub(crate) fn drag(self, editor: &mut crate::editor::Editor, row: usize) {
        let target = self.layout.drag_scroll(self.row, row);
        if editor.word_wrap {
            // The final screenful may contain a different number of source
            // lines from the one where the drag started.
            if row > self.row && target == self.layout.max_scroll {
                let end = editor.max_wrap_scroll_offset();
                editor.set_wrap_scroll_offset(end);
                return;
            }
            if target == self.layout.scroll {
                editor.set_wrap_scroll_offset(self.wrap_scroll);
                return;
            }
        }
        Self::scroll(editor, target);
    }

    fn scroll(editor: &mut crate::editor::Editor, row: usize) {
        if editor.visible_scroll_offset() == row {
            return;
        }
        if editor.word_wrap {
            let line = editor.line_for_visible_row(row);
            let wrap_row = editor.wrapped_row_for_position(line, 0, editor.effective_wrap_width());
            editor.set_wrap_scroll_offset(wrap_row);
        } else {
            editor.set_visible_scroll_offset(row);
        }
    }
}

fn minimap_change_in_range<T: Copy + Into<LineChanges>>(
    changes: &[(usize, T)], start: usize, end: usize,
) -> LineChanges {
    let index = changes.partition_point(|&(line, _)| line < start);
    changes[index..]
        .iter()
        .take_while(|&&(line, _)| line < end)
        .fold(LineChanges::default(), |combined, &(_, change)| combined.merge(change.into()))
}

fn minimap_source_range(window: MinimapWindow, row: usize, height: usize) -> (usize, usize) {
    let len = window.len();
    if height == 0 || len == 0 || row >= height {
        return (window.end, window.end);
    }
    if len <= height {
        let start = window.start.saturating_add(row).min(window.end);
        return (start, start.saturating_add(1).min(window.end));
    }

    let start = window
        .start
        .saturating_add((row as u128 * len as u128 / height as u128) as usize)
        .min(window.end);
    let end = window
        .start
        .saturating_add(((row + 1) as u128 * len as u128 / height as u128) as usize)
        .max(start.saturating_add(1))
        .min(window.end);
    (start, end)
}

/// Pack source lines into the four pixel rows of a 2×4 braille cell.
fn minimap_samples(start: usize, end: usize) -> impl Iterator<Item = (usize, u8)> {
    let len = end.saturating_sub(start);
    let count = len.min(4);
    (0..count).map(move |sample| {
        let pixel = if count == 1 { 1 } else { sample * 3 / (count - 1) };
        (start + sample * len / count, [0x09, 0x12, 0x24, 0xc0][pixel])
    })
}

fn minimap_row_after(line: usize, window: MinimapWindow, height: usize) -> usize {
    let len = window.len();
    if height == 0 || len == 0 {
        return 0;
    }
    let relative = line.saturating_sub(window.start).min(len);
    if len <= height {
        return relative.min(height);
    }
    (relative as u128 * height as u128).div_ceil(len as u128) as usize
}

fn draw_minimap_viewport_border(
    f: &mut Frame,
    area: Rect,
    view_start_row: usize,
    view_end_row: usize,
    hovered: bool,
    colors: &crate::app_theme::ThemeColors,
) {
    let height = area.height as usize;
    if height == 0 || area.width == 0 {
        return;
    }
    let buf = f.buffer_mut();
    let border_fg = if hovered {
        colors.fg_text
    } else {
        colors.fg_dim
    };
    let top_y = area.y + view_start_row.min(height.saturating_sub(1)) as u16;
    let bot_row = view_end_row.min(height).saturating_sub(1);
    let bot_y = area.y + bot_row as u16;
    for x in area.x..area.x + area.width {
        for (y, symbol) in [(top_y, "▔"), (bot_y, "▁")] {
            if symbol == "▁" && bot_y <= top_y { continue; }
            if let Some(cell) = buf.cell_mut((x, y))
                && !cell.modifier.contains(Modifier::BOLD)
            {
                if cell.fg != colors.fg_bookmark {
                    cell.set_fg(colors.ui.readable_foreground(border_fg, cell.bg, 3.0));
                }
                cell.set_symbol(symbol);
            }
        }
    }
}

pub(crate) fn draw_minimap(f: &mut Frame, p: MinimapParams) {
    let MinimapParams {
        area,
        editor,
        scroll,
        visible_lines,
        search_matches,
        diagnostic_lines,
        diagnostic_index,
        change_index: _,
        diff_changes: _,
        colors,
        hovered,
        active,
    } = p;
    let height = area.height as usize;
    let width = area.width as usize;
    let total_lines = editor.line_count();
    let visible_total = editor.visible_line_count();
    if height == 0 || width == 0 || total_lines == 0 {
        return;
    }

    let layout = MinimapLayout::new(
        visible_total,
        editor.visible_row_for_line(scroll),
        visible_lines,
        height,
        editor.scroll_past_end,
    );
    let window = layout.window;
    let view_start_row = layout.view_start;
    let view_end_row = layout.view_end;
    let selection_lines = editor.selection_range().map(|(sl, _, el, _)| (sl, el));
    let mut cache = editor.minimap_cache.borrow_mut();
    cache.synchronize(&editor.rope, width, editor.tab_size);

    let buffer = f.buffer_mut();
    let track_bg = colors.ui.bg_minimap;
    let slider_bg = if active {
        colors.ui.bg_minimap_slider_active
    } else if hovered {
        colors.ui.bg_minimap_slider_hover
    } else {
        colors.ui.bg_minimap_slider
    };
    for row in 0..height {
        let (source_row, end_row) = minimap_source_range(window, row, height);
        let source_line = editor.line_for_visible_row(source_row.min(visible_total));
        let row_end_line = editor.line_for_visible_row(end_row.min(visible_total));
        let in_viewport = row >= view_start_row && row < view_end_row;
        let has_search_match = has_search_match_in_range(search_matches, source_line, row_end_line);
        let has_bookmark = editor
            .bookmarks
            .range(source_line..row_end_line)
            .next()
            .is_some();
        let changes = minimap_row_changes(p, source_line, row_end_line);
        let has_selection = selection_overlaps_range(selection_lines, source_line, row_end_line);
        let diag_severity = diagnostic_index.map_or_else(
            || worst_diagnostic_in_range(diagnostic_lines, source_line, row_end_line),
            |index| index.best_in_range(source_line, row_end_line),
        );

        let bg = if has_selection {
            colors.ui.bg_minimap_selection
        } else if in_viewport {
            slider_bg
        } else {
            track_bg
        };

        if source_line < total_lines {
            let markers = minimap_row_markers(changes, diag_severity, has_search_match, width, bg, &colors);
            let row_bg = markers.background;
            let content_width = width - markers.width;
            let highlight = if has_search_match {
                Some(colors.ui.fg_minimap_find)
            } else if has_bookmark {
                Some(colors.fg_bookmark)
            } else {
                None
            };
            let blank = Style::default().bg(row_bg);
            let styles = [
                blank,
                blank.fg(highlight.unwrap_or(if in_viewport {
                    colors.fg_text
                } else {
                    colors.fg_dim
                })),
                blank.fg(highlight.unwrap_or_else(|| dim_color(colors.fg_text, 150, row_bg))),
                blank.fg(highlight.unwrap_or_else(|| dim_color(colors.fg_text, 160, row_bg))),
                blank.fg(highlight.unwrap_or_else(|| dim_color(colors.fg_text, 140, row_bg))),
            ];
            let samples = minimap_samples(source_row, end_row)
                .filter(|&(row, _)| row < visible_total)
                .map(|(row, dots)| (editor.line_for_visible_row(row), dots));
            for (column, cell) in cache.band(&editor.rope, samples)[..content_width]
                .iter()
                .enumerate()
            {
                let ch = cell.symbol();
                let style = styles[cell.glyph as usize];
                if let Some(cell) = buffer.cell_mut((area.x + column as u16, area.y + row as u16)) {
                    cell.set_char(ch).set_style(style);
                }
            }
            for (offset, &(symbol, style)) in markers.cells[..markers.width].iter().enumerate() {
                if let Some(cell) = buffer.cell_mut((area.x + (content_width + offset) as u16, area.y + row as u16)) {
                    cell.set_symbol(symbol).set_style(style);
                }
            }
        } else {
            for column in 0..width {
                if let Some(cell) = buffer.cell_mut((area.x + column as u16, area.y + row as u16)) {
                    cell.set_char(' ').set_bg(bg);
                }
            }
        }
    }
    draw_minimap_viewport_border(f, area, view_start_row, view_end_row, hovered, &colors);
}

#[derive(Clone, Copy)]
pub(crate) struct ScrollbarParams<'a> {
    pub(crate) area: Rect,
    pub(crate) total_lines: usize,
    pub(crate) visible_lines: usize,
    pub(crate) scroll: usize,
    pub(crate) search_matches: &'a [(usize, usize, usize)],
    pub(crate) diagnostic_lines: &'a [(usize, u8)],
    pub(crate) diagnostic_index: Option<&'a DiagnosticLineIndex>,
    pub(crate) change_rows: &'a ChangeRows,
    pub(crate) cursor_line: usize,
    pub(crate) colors: crate::app_theme::ThemeColors,
    pub(crate) hovered: bool,
    pub(crate) active: bool,
}

/// Project the visible interval onto the same track as the change markers.
/// Include any terminal row touched by the viewport, even at a partial edge.
pub(crate) fn scrollbar_viewport(
    total: usize,
    visible: usize,
    scroll: usize,
    height: usize,
    scroll_past_end: bool,
) -> Range<usize> {
    let total = total.max(1);
    let visible = visible.max(1);
    let start = scroll.min(total.saturating_sub(if scroll_past_end { 1 } else { visible }));
    let end = start.saturating_add(visible).min(total);
    (start as u128 * height as u128 / total as u128) as usize
        ..(end as u128 * height as u128).div_ceil(total as u128) as usize
}

#[cfg(test)]
pub(crate) fn draw_scrollbar(f: &mut Frame, p: ScrollbarParams) {
    draw_scrollbar_with_mapping(f, p, None, None);
}

pub(crate) fn draw_editor_scrollbar(f: &mut Frame, p: ScrollbarParams, editor: &crate::editor::Editor) {
    draw_scrollbar_with_mapping(f, p, None, Some(editor));
}

/// Query original source indexes without projecting every provider item into
/// immutable diff display rows again. The source-to-display map is
/// nondecreasing. Entries beyond it retain the diff renderer's fallback to
/// display row zero.
pub(crate) fn draw_mapped_scrollbar(
    f: &mut Frame,
    p: ScrollbarParams,
    source_to_display: &[usize],
    logical_boundary: impl Fn(usize) -> usize,
) {
    draw_scrollbar_with_mapping(f, p, Some((source_to_display, &logical_boundary)), None);
}

/// Navigation and overview markers share the existing scrollbar width. Wide
/// bars use separate lanes; narrow bars use the two halves of a terminal cell.
fn scrollbar_row_cells(
    width: usize,
    in_thumb: bool,
    is_cursor: bool,
    marker: Option<Color>,
    change: scrollbar_changes::ChangeRow,
    colors: &crate::app_theme::ThemeColors,
    track: Color,
    thumb: Color,
) -> [(char, Style); 2] {
    let surface = if in_thumb { thumb } else { track };
    let navigation = if is_cursor {
        ('━', Style::default().bg(surface)
            .fg(colors.ui.readable_foreground(colors.bg_cursor, surface, 3.0)))
    } else {
        (if in_thumb { '█' } else { ' ' }, Style::default().fg(surface).bg(surface))
    };
    let navigation_bg = if is_cursor { navigation.1.fg.unwrap_or(colors.bg_cursor) } else { surface };
    let blank = (' ', Style::default().fg(track).bg(track));
    if let Some(color) = marker {
        return if width == 1 {
            [('▐', Style::default().fg(color).bg(navigation_bg)); 2]
        } else {
            [navigation, ('█', Style::default().fg(color).bg(track))]
        };
    }
    let combined = change.changes();
    let palette = ChangeSurface::Scrollbar;
    let color = |kind| palette.color(kind, colors);
    if width == 1 {
        let cell = combined.summary().map_or(navigation, |kind| {
            let mut style = Style::default().fg(color(kind)).bg(navigation_bg);
            if let Some(staged) = combined.staged() {
                style = style.add_modifier(Modifier::UNDERLINED).underline_color(color(staged));
            }
            (change.narrow_symbol(), style)
        });
        return [cell; 2];
    }
    if combined.working().is_none() {
        let staged = combined.staged().map_or(blank, |kind| {
            (change.symbol(), Style::default().fg(color(kind)).bg(track).add_modifier(Modifier::UNDERLINED))
        });
        return [navigation, staged];
    }
    let [upper, lower] = change.halves();
    let staged_lane = combined.staged().map_or(navigation, |kind| {
        let symbol = scrollbar_changes::coverage_symbol(upper.staged().is_some(), lower.staged().is_some(), true);
        (symbol, Style::default().fg(color(kind)).bg(navigation_bg).add_modifier(Modifier::UNDERLINED))
    });
    let [top, bottom] = [upper.working(), lower.working()];
    let working_lane = match (top, bottom) {
        (Some(a), Some(b)) if color(a) != color(b) => ('▀', Style::default().fg(color(a)).bg(color(b))),
        (None, None) => blank,
        _ => {
            let symbol = scrollbar_changes::coverage_symbol(top.is_some(), bottom.is_some(), false);
            let kind = combined.working().unwrap();
            (symbol, Style::default().fg(color(kind)).bg(track))
        }
    };
    [staged_lane, working_lane]
}

fn draw_scrollbar_with_mapping(
    f: &mut Frame,
    p: ScrollbarParams,
    source_to_display: Option<(&[usize], &dyn Fn(usize) -> usize)>,
    editor: Option<&crate::editor::Editor>,
) {
    let ScrollbarParams {
        area,
        total_lines,
        visible_lines,
        scroll,
        search_matches,
        diagnostic_lines,
        diagnostic_index,
        change_rows,
        cursor_line,
        colors,
        hovered,
        active,
    } = p;
    let height = area.height as usize;
    let width = area.width as usize;
    if height == 0 || total_lines <= visible_lines {
        f.buffer_mut()
            .set_style(area, Style::default().bg(colors.ui.bg_overview));
        return;
    }

    // Search and diagnostic vectors are sorted by line. Query one source range
    // per terminal row instead of rebuilding full-document row maps on every
    // scroll/click frame.
    let cursor_row = cursor_line
        .min(total_lines.saturating_sub(1))
        .saturating_mul(height)
        / total_lines;
    let viewport = scrollbar_viewport(
        total_lines,
        visible_lines,
        scroll,
        height,
        editor.is_some_and(|editor| editor.scroll_past_end),
    );

    let (track, thumb) = colors
        .ui
        .scrollbar_colors(colors.ui.bg_overview, hovered, active);
    let buffer = f.buffer_mut();
    for i in 0..height {
        let source_start = i.saturating_mul(total_lines).div_ceil(height);
        let source_end = (i + 1)
            .saturating_mul(total_lines)
            .div_ceil(height)
            .min(total_lines);
        let in_thumb = viewport.contains(&i);
        let (query_start, query_end) =
            source_to_display.map_or_else(|| editor.map_or((source_start, source_end), |editor| {
                (editor.overview_source_boundary(source_start), editor.overview_source_boundary(source_end))
            }), |(mapping, logical_boundary)| {
                let start = logical_boundary(source_start);
                let end = logical_boundary(source_end);
                (
                    mapping.partition_point(|&row| row < start),
                    mapping.partition_point(|&row| row < end),
                )
            });
        let includes_fallback = source_start == 0 && source_end > 0;
        let fallback_start = source_to_display
            .filter(|_| includes_fallback)
            .map(|(mapping, _)| mapping.len());
        let has_match = has_search_match_in_range(search_matches, query_start, query_end)
            || fallback_start.is_some_and(|start| {
                search_matches
                    .last()
                    .is_some_and(|&(line, _, _)| line >= start)
            });
        let is_cursor = i == cursor_row;
        let change = change_rows.row(i);
        let diag_sev = diagnostic_index.map_or_else(
            || worst_diagnostic_in_range(diagnostic_lines, query_start, query_end),
            |index| index.best_in_range(query_start, query_end),
        );
        let fallback_severity = fallback_start.and_then(|start| {
            diagnostic_index.map_or_else(
                || {
                    let first = diagnostic_lines.partition_point(|&(line, _)| line < start);
                    diagnostic_lines[first..]
                        .iter()
                        .map(|&(_, severity)| severity)
                        .min()
                },
                |index| index.best_in_range_inner(start, usize::MAX, true),
            )
        });
        let diag_sev = match (diag_sev, fallback_severity) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let marker = if let Some(sev) = diag_sev {
            let c = match sev {
                1 => colors.ui.fg_overview_error,
                2 => colors.ui.fg_overview_warning,
                3 => colors.ui.fg_overview_info,
                _ => colors.fg_dim,
            };
            Some(c)
        } else if has_match {
            let c = colors.ui.fg_overview_find;
            Some(c)
        } else {
            None
        };
        let cells = scrollbar_row_cells(width, in_thumb, is_cursor, marker, change, &colors, track, thumb);
        for column in 0..width {
            let (symbol, style) = cells[usize::from(column >= width / 2)];
            if let Some(cell) = buffer.cell_mut((area.x + column as u16, area.y + i as u16)) {
                cell.set_char(symbol).set_style(style);
            }
        }
    }
}
#[cfg(test)]
#[path = "ui_editor_mapped_scrollbar_tests.rs"]
mod mapped_scrollbar_tests;

#[cfg(test)]
#[path = "ui_scrollbar_visibility_tests.rs"]
mod scrollbar_visibility_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;

    fn test_colors() -> crate::app_theme::ThemeColors {
        static COLORS: std::sync::OnceLock<crate::app_theme::ThemeColors> =
            std::sync::OnceLock::new();
        *COLORS.get_or_init(|| crate::app_theme::EditorTheme::Dark.colors())
    }

    #[test]
    fn compressed_rows_keep_additions_and_removals_in_each_git_source() {
        let changes = [
            (0, LineChanges::git('D', ' ')),
            (1, LineChanges::git(' ', 'D')),
            (2, LineChanges::git('A', 'A')),
        ];
        let index = ChangeLineIndex::from_changes(&changes);
        let compressed = index.in_range(0, 3, false);
        assert_eq!(compressed, LineChanges::git('M', 'M'));
        assert_eq!(compressed, minimap_change_in_range(&changes, 0, 3));
        assert_eq!(index.in_range(0, 1, false), changes[0].1);
        assert_eq!(index.in_range(1, 2, false), changes[1].1);
    }

    #[test]
    fn overview_marks_keep_staged_removals_distinct_from_working_additions() {
        for theme in [crate::app_theme::EditorTheme::Dark, crate::app_theme::EditorTheme::Light] {
            let colors = theme.colors();
            let overlap = LineChanges::git('D', 'A');
            let markers = minimap_row_markers(overlap, Some(2), false, 12, colors.ui.bg_minimap, &colors);
            assert_eq!(markers.width, 3);
            assert_eq!(markers.cells.map(|(symbol, _)| symbol), ["█", "•", " "]);
            assert_eq!(markers.cells[0].1.fg, Some(colors.ui.fg_minimap_warning));
            assert_eq!(markers.cells[1].1.bg, Some(colors.ui.fg_gutter_deleted_secondary));
            assert_eq!(markers.cells[2].1.bg, Some(colors.ui.fg_gutter_added));

            let replacement = minimap_row_markers(LineChanges::git('M', ' '), None, false, 12, colors.ui.bg_minimap, &colors);
            assert_eq!(replacement.width, 1);
            assert_eq!(replacement.cells[0].0, "•");
            assert_eq!(replacement.cells[0].1.bg, Some(colors.ui.fg_gutter_modified_secondary));

            let rows = ChangeRows::from_lines(10, 100, [(40, overlap), (45, 'D'.into())]);
            let (track, thumb) = colors.ui.scrollbar_colors(colors.ui.bg_overview, false, false);
            let [staged, working] = scrollbar_row_cells(2, true, false, None, rows.row(4), &colors, track, thumb);
            assert_eq!(staged.0, '▝');
            assert_eq!(staged.1.fg, Some(colors.ui.fg_overview_deleted));
            assert_eq!(staged.1.bg, Some(thumb));
            assert_eq!(working.0, '▀');
            assert_eq!(working.1.fg, Some(colors.ui.fg_overview_added));
            assert_eq!(working.1.bg, Some(colors.ui.fg_overview_deleted));

            let [marker, _] = scrollbar_row_cells(1, true, false, None, rows.row(4), &colors, track, thumb);
            assert_eq!(marker.0, '▐');
            assert_eq!(marker.1.bg, Some(thumb));
            assert_eq!(marker.1.fg, Some(colors.ui.fg_overview_modified));
            assert_eq!(marker.1.underline_color, Some(colors.ui.fg_overview_deleted));
        }
    }

    // -- has_search_match_in_range --

    #[test]
    fn search_ranges_are_half_open_and_empty_or_reversed_ranges_have_no_matches() {
        for (matches, start, end, expected) in [
            (
                &[(2, 0, 1), (5, 0, 1), (10, 0, 1), (15, 0, 1)][..],
                4,
                8,
                true,
            ),
            (
                &[(2, 0, 1), (5, 0, 1), (10, 0, 1), (15, 0, 1)][..],
                6,
                8,
                false,
            ),
            (&[][..], 0, 10, false),
            (&[(5, 0, 1)][..], 5, 5, false),
            (&[(5, 0, 1)][..], 6, 5, false),
            (&[(5, 0, 1)][..], 5, 6, true),
        ] {
            assert_eq!(
                has_search_match_in_range(matches, start, end),
                expected,
                "{matches:?} in {start}..{end}"
            );
        }
    }

    // -- worst_diagnostic_in_range --

    #[test]
    fn diagnostic_ranges_choose_the_worst_severity_and_ignore_outside_or_empty_ranges() {
        for (diagnostics, start, end, expected) in [
            (&[][..], 0, 10, None),
            (&[(5, 1)][..], 0, 10, Some(1)),
            (&[(3, 3), (5, 2), (7, 4)][..], 0, 10, Some(2)),
            (&[(3, 2), (5, 1), (7, 4)][..], 0, 10, Some(1)),
            (&[(1, 2), (20, 1)][..], 5, 15, None),
            (&[(5, 1)][..], 5, 5, None),
        ] {
            assert_eq!(
                worst_diagnostic_in_range(diagnostics, start, end),
                expected,
                "{diagnostics:?} in {start}..{end}"
            );
        }
    }

    // -- selection_overlaps_range --

    #[test]
    fn selection_ranges_preserve_endpoint_and_empty_selection_semantics() {
        for (selection, start, end, expected) in [
            (Some((3, 7)), 5, 10, true),
            (Some((0, 3)), 5, 10, false),
            (None, 5, 10, false),
            (Some((3, 7)), 5, 5, false),
            (Some((5, 5)), 5, 10, true),
            (Some((10, 12)), 5, 10, false),
        ] {
            assert_eq!(
                selection_overlaps_range(selection, start, end),
                expected,
                "{selection:?} in {start}..{end}"
            );
        }
    }

    // -- dim_color --

    #[test]
    fn dim_color_blends_rgb_against_the_surface_and_preserves_named_colors() {
        for (foreground, amount, surface, expected) in [
            (
                Color::Rgb(100, 200, 50),
                255,
                Color::Rgb(20, 30, 40),
                Color::Rgb(100, 200, 50),
            ),
            (
                Color::Rgb(200, 100, 50),
                128,
                Color::Rgb(0, 0, 0),
                Color::Rgb(100, 50, 25),
            ),
            (
                Color::Rgb(10, 20, 30),
                0,
                Color::Rgb(250, 245, 240),
                Color::Rgb(250, 245, 240),
            ),
            (Color::Red, 128, Color::Rgb(250, 245, 240), Color::Red),
        ] {
            assert_eq!(
                dim_color(foreground, amount, surface),
                expected,
                "{foreground:?}, amount={amount}, surface={surface:?}"
            );
        }
    }

    #[test]
    fn minimap_tints_use_the_actual_light_surface_and_preserve_marker_hues() {
        let surface = Color::Rgb(240, 245, 250);
        assert_eq!(
            dim_color(Color::Rgb(20, 40, 60), 128, surface),
            Color::Rgb(130, 142, 155)
        );
        let mut colors = test_colors();
        colors.ui.fg_gutter_deleted = Color::Rgb(20, 40, 60);
        let markers = minimap_row_markers('D'.into(), None, false, 3, surface, &colors);
        assert_eq!(markers.background, Color::Rgb(192, 200, 208));
        assert_eq!(markers.width, 1);
        assert_eq!(markers.cells[0].1.bg, Some(Color::Rgb(20, 40, 60)));
    }

    #[test]
    fn minimap_diff_rows_composite_on_the_light_track_without_darkening_the_hue() {
        let mut editor = crate::editor::Editor::new();
        editor.rope = ropey::Rope::from_str(&format!("{}line", "line\n".repeat(99)));
        editor.clear_git_line_status();
        let mut colors = test_colors();
        colors.ui.bg_minimap = Color::Rgb(240, 245, 250);
        colors.ui.fg_gutter_deleted = Color::Rgb(20, 40, 60);
        let mut terminal = Terminal::new(TestBackend::new(12, 10)).unwrap();
        terminal
            .draw(|frame| {
                draw_minimap(
                    frame,
                    MinimapParams {
                        area: frame.area(),
                        editor: &editor,
                        scroll: 45,
                        visible_lines: 10,
                        search_matches: &[],
                        diagnostic_lines: &[],
                        diagnostic_index: None,
                        change_index: None,
                        diff_changes: Some(MinimapDiffChanges { lines: &[(40, 'D')], staged: false }),
                        colors,
                        hovered: false,
                        active: false,
                    },
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        for x in 0..11 {
            assert_eq!(buffer[(x, 1)].bg, Color::Rgb(192, 200, 208));
        }
        assert_eq!(buffer[(11, 1)].bg, Color::Rgb(20, 40, 60));
    }

    #[test]
    fn diff_marker_colors_exist_only_for_changed_or_saved_statuses() {
        let colors = test_colors();
        for (status, expected) in [
            ('N', true),
            ('U', true),
            ('D', true),
            ('S', true),
            (' ', false),
            ('X', false),
        ] {
            assert_eq!(
                minimap_row_markers(status.into(), None, false, 3, Color::Black, &colors).width > 0,
                expected,
                "{status:?}"
            );
        }
    }

    #[test]
    fn cached_diff_changes_populate_scrollbar_without_git_markers() {
        let mut editor = crate::editor::Editor::new();
        editor.rope = ropey::Rope::from_str(&format!("{}line", "line\n".repeat(99)));
        editor.large_file_mode = true;
        editor.modified = false;
        editor.clear_git_line_status();
        let diff_changes = [(50, 'U'), (90, 'D')];

        let without_compare = scrollbar_change_rows(&editor, 10, None);
        let with_compare = scrollbar_change_rows(&editor, 10, Some(&diff_changes));

        assert!(without_compare.is_empty());
        assert_eq!(with_compare.row(5).changes(), 'U'.into());
        assert_eq!(with_compare.row(9).changes(), 'D'.into());
    }

    #[test]
    fn sparse_change_rows_use_the_same_proportional_coordinates_as_other_markers() {
        let mut editor = crate::editor::Editor::new();
        editor.rope = ropey::Rope::from_str(&format!("{}line", "line\n".repeat(99)));
        let changes = [(1, 'U'), (99, 'D')];

        let rows = scrollbar_change_rows(&editor, 10, Some(&changes));

        assert_eq!(rows.row(0).changes(), 'U'.into());
        assert_eq!(rows.row(9).changes(), 'D'.into());
    }

    #[test]
    fn minimap_renders_cached_diff_marker_without_git_markers() {
        let mut editor = crate::editor::Editor::new();
        editor.rope = ropey::Rope::from_str(&format!("{}line", "line\n".repeat(99)));
        editor.cursor_line = 0;
        editor.clear_git_line_status();
        let colors = test_colors();
        let diff_changes = [(0, 'D'), (40, 'U'), (99, 'D')];
        let mut terminal = Terminal::new(TestBackend::new(12, 10)).expect("terminal");

        terminal
            .draw(|frame| {
                draw_minimap(
                    frame,
                    MinimapParams {
                        area: Rect::new(0, 0, 12, 10),
                        editor: &editor,
                        scroll: 45,
                        visible_lines: 10,
                        search_matches: &[],
                        diagnostic_lines: &[],
                        diagnostic_index: None,
                        change_index: None,
                        diff_changes: Some(MinimapDiffChanges { lines: &diff_changes, staged: false }),
                        colors,
                        hovered: false,
                        active: false,
                    },
                );
            })
            .expect("draw minimap");

        let buffer = terminal.backend().buffer();
        // The local window starts at line 36 and each row covers four lines.
        assert_eq!(buffer[(11, 1)].bg, colors.ui.fg_gutter_modified);
        assert_eq!(buffer[(11, 1)].symbol(), " ");
        assert!((0..10).all(|y| buffer[(11, y)].bg != colors.ui.fg_gutter_deleted));
    }

    #[test]
    fn scrollbar_renders_cached_diff_marker_without_git_markers() {
        let mut editor = crate::editor::Editor::new();
        editor.rope = ropey::Rope::from_str(&format!("{}line", "line\n".repeat(99)));
        editor.clear_git_line_status();
        let colors = test_colors();
        let diff_changes = [(50, 'U')];
        let change_rows = scrollbar_change_rows(&editor, 10, Some(&diff_changes));
        let mut terminal = Terminal::new(TestBackend::new(2, 10)).expect("terminal");

        terminal
            .draw(|frame| {
                draw_scrollbar(
                    frame,
                    ScrollbarParams {
                        area: Rect::new(0, 0, 2, 10),
                        total_lines: editor.line_count(),
                        visible_lines: 10,
                        scroll: 45,
                        search_matches: &[],
                        diagnostic_lines: &[],
                        diagnostic_index: None,
                        change_rows: &change_rows,
                        cursor_line: 0,
                        colors,
                        hovered: false,
                        active: false,
                    },
                );
            })
            .expect("draw scrollbar");

        assert!((0..2).any(|x| {
            terminal
                .backend()
                .buffer()
                .cell((x, 5))
                .is_some_and(|cell| cell.fg == colors.ui.fg_overview_modified)
        }));
    }

    // -- diagnostic_marker_color --

    #[test]
    fn minimap_uses_gutter_colors_and_only_staged_changes_get_a_symbol_at_viewport_border() {
        let mut editor = crate::editor::Editor::new();
        editor.rope = ropey::Rope::from_str(&format!("{}line", "line\n".repeat(99)));
        let mut colors = test_colors();
        // Distinct theme values catch palette mix-ups at the viewport border.
        colors.ui.fg_gutter_added = Color::Rgb(10, 20, 30);
        colors.ui.fg_gutter_modified = Color::Rgb(40, 50, 60);
        colors.ui.fg_gutter_deleted = Color::Rgb(70, 80, 90);
        colors.ui.fg_gutter_modified_secondary = Color::Rgb(41, 51, 61);
        colors.ui.fg_overview_added = Color::Rgb(12, 22, 32);
        colors.ui.fg_overview_modified = Color::Rgb(42, 52, 62);
        colors.ui.fg_overview_deleted = Color::Rgb(72, 82, 92);
        for (status, minimap_expected, symbol) in [
            ('N', colors.ui.fg_gutter_modified, " "),
            ('U', colors.ui.fg_gutter_modified, " "),
            ('A', colors.ui.fg_gutter_added, " "),
            ('M', colors.ui.fg_gutter_modified, " "),
            ('D', colors.ui.fg_gutter_deleted, " "),
            ('S', colors.ui.fg_gutter_modified_secondary, "•"),
        ] {
            let overview_expected =
                crate::ui_colors::overview_marker_color(status, &colors).unwrap();
            let changes = [(45, status)];
            let rows = scrollbar_change_rows(&editor, 10, Some(&changes));
            let mut terminal = Terminal::new(TestBackend::new(14, 10)).unwrap();
            terminal
                .draw(|frame| {
                    draw_minimap(
                        frame,
                        MinimapParams {
                            area: Rect::new(0, 0, 12, 10),
                            editor: &editor,
                            scroll: 45,
                            visible_lines: 10,
                            search_matches: &[],
                            diagnostic_lines: &[],
                            diagnostic_index: None,
                            change_index: None,
                            diff_changes: Some(MinimapDiffChanges { lines: &changes, staged: false }),
                            colors,
                            hovered: false,
                            active: false,
                        },
                    );
                    draw_scrollbar(
                        frame,
                        ScrollbarParams {
                            area: Rect::new(12, 0, 2, 10),
                            total_lines: 100,
                            visible_lines: 10,
                            scroll: 45,
                            search_matches: &[],
                            diagnostic_lines: &[],
                            diagnostic_index: None,
                            change_rows: &rows,
                            cursor_line: 0,
                            colors,
                            hovered: false,
                            active: false,
                        },
                    );
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(11, 2)].bg, minimap_expected, "minimap {status}");
            assert_eq!(buffer[(11, 2)].symbol(), symbol, "minimap {status}");
            assert_eq!(
                buffer.cell((13, 4)).unwrap().fg,
                overview_expected,
                "scrollbar {status}"
            );
        }
    }

    #[test]
    fn diagnostic_marker_colors_use_the_severity_role_or_dim_fallback() {
        let colors = test_colors();
        for (severity, expected) in [
            (1, colors.ui.fg_minimap_error),
            (2, colors.ui.fg_minimap_warning),
            (3, colors.ui.fg_minimap_info),
            (4, colors.fg_dim),
            (99, colors.fg_dim),
        ] {
            assert_eq!(
                diagnostic_marker_color(severity, &colors),
                expected,
                "severity={severity}"
            );
        }
    }

    #[test]
    fn minimap_short_files_fit_and_long_files_show_four_viewports() {
        assert_eq!(
            minimap_window(100, 0, 40, 40),
            MinimapWindow { start: 0, end: 100 }
        );
        for scroll in [0, 2500, 5000, 7500, 9960] {
            let window = minimap_window(10_000, scroll, 40, 40);
            assert_eq!(window.len(), 160);
            assert!(window.start <= scroll);
            assert!(window.end >= scroll + 40);
        }
    }

    #[test]
    fn minimap_slider_covers_each_visible_band_and_traverses_entire_track() {
        for height in [2, 3, 10, 40, 50, 60] {
            let visible = height;
            let total = 10_000;
            let max_scroll = total - visible;
            let mut previous_row = 0;
            let mut previous_window = 0;
            for scroll in 0..=max_scroll {
                let layout = MinimapLayout::new(total, scroll, visible, height, false);
                assert_eq!(layout.view_end - layout.view_start, (scroll % 4 + visible).div_ceil(4));
                assert!(layout.view_start >= previous_row);
                assert!(layout.window.start >= previous_window);
                assert!(layout.view_end <= height);
                if scroll == 0 {
                    assert_eq!(layout.view_start, 0);
                }
                if scroll == max_scroll {
                    assert_eq!(layout.view_end, height);
                }
                previous_row = layout.view_start;
                previous_window = layout.window.start;
            }
            let quarter = MinimapLayout::new(total, max_scroll / 4, visible, height, false);
            let middle = MinimapLayout::new(total, max_scroll / 2, visible, height, false);
            let later = MinimapLayout::new(total, max_scroll * 3 / 4, visible, height, false);
            if height >= 10 {
                assert!(quarter.view_start < middle.view_start);
                assert!(middle.view_start < later.view_start);
            }
        }
    }

    #[test]
    fn minimap_past_end_layout_includes_the_entire_viewport() {
        for total in [1_usize, 10, 60, 10_000] {
            let visible = 40;
            let mut previous_window = 0;
            let mut previous_row = 0;
            for scroll in 0..total {
                let layout = MinimapLayout::new(total, scroll, visible, 40, true);
                assert_eq!(layout.scroll, scroll);
                assert_eq!(layout.max_scroll, total - 1);
                assert!(layout.window.start <= scroll);
                assert!(layout.window.end >= scroll + visible);
                assert!(layout.window.start >= previous_window);
                assert!(layout.view_start >= previous_row);
                assert!(layout.view_end <= 40);
                assert_eq!(layout.click_scroll(layout.view_end - 1), scroll);
                assert_eq!(
                    layout.drag_scroll(layout.view_end - 1, layout.view_end - 1),
                    scroll,
                );
                if scroll == total - 1 {
                    assert!(layout.window.end >= total + visible - 1);
                    assert!(layout.window.end - (total + visible - 1) < 4);
                    assert_eq!(layout.view_end, 40);
                }
                previous_window = layout.window.start;
                previous_row = layout.view_start;
            }
        }
    }

    #[test]
    fn minimap_source_rows_cover_only_the_moving_window() {
        let window = minimap_window(50_000, 25_000, 50, 50);
        assert!(window.start > 0);
        let mut end = window.start;
        for row in 0..50 {
            let range = minimap_source_range(window, row, 50);
            assert_eq!(range.0, end);
            assert!(range.1 > range.0);
            end = range.1;
        }
        assert_eq!(end, window.end);
        assert!(end < 50_000);
    }

    #[test]
    fn minimap_click_grabs_slider_without_jumping_and_targets_local_content() {
        let layout = MinimapLayout::new(10_000, 5000, 40, 40, false);
        for row in layout.view_start..layout.view_end {
            assert_eq!(layout.click_scroll(row), 5000);
        }
        let click = layout.click_scroll(39);
        assert!(click > 5000 && click < 5100);
        let range = minimap_source_range(layout.window, 39, 40);
        assert!((range.0..range.1).contains(&(click + 20)));
    }

    #[test]
    fn minimap_drag_preserves_grab_position_and_reaches_both_ends() {
        let layout = MinimapLayout::new(10_000, 4980, 40, 40, false);
        for grab_row in [
            layout.view_start,
            layout.view_start + 6,
            layout.view_end - 1,
        ] {
            assert_eq!(layout.drag_scroll(grab_row, grab_row), 4980);
            let down = layout.drag_scroll(grab_row, grab_row + 5);
            assert!(down > 4980);
            let moved = MinimapLayout::new(10_000, down, 40, 40, false);
            assert_eq!(moved.view_start, layout.view_start + 5);
            assert_eq!(layout.drag_scroll(grab_row, 0), 0);
            assert_eq!(layout.drag_scroll(grab_row, 39), 9960);
        }
    }

    #[test]
    fn minimap_degenerate_dimensions_and_scroll_are_clamped() {
        for scroll_past_end in [false, true] {
            for (total, visible, height) in [
                (0, 40, 40),
                (10, 40, 40),
                (100, 40, 0),
                (100, 40, 1),
                (100, 0, 40),
            ] {
                let layout =
                    MinimapLayout::new(total, usize::MAX, visible, height, scroll_past_end);
                let max_scroll =
                    total.saturating_sub(if scroll_past_end { 1 } else { visible.max(1) });
                assert!(layout.view_end <= height);
                assert!(layout.click_scroll(usize::MAX) <= max_scroll);
                assert!(layout.drag_scroll(0, 1) <= max_scroll);
            }
        }
    }

    #[test]
    fn minimap_changes_are_clipped_to_the_rendered_source_range() {
        let changes = [(0, 'N'), (4990, 'U'), (5000, 'D'), (5010, 'S'), (9999, 'N')];
        assert!(minimap_change_in_range(&changes, 4980, 4990).is_empty());
        assert_eq!(minimap_change_in_range(&changes, 4990, 5001), LineChanges::from('U').merge('D'.into()));
        assert_eq!(minimap_change_in_range(&changes, 5000, 5001), 'D'.into());
        assert!(minimap_change_in_range(&changes, 5010, 5010).is_empty());
    }

    #[test]
    fn minimap_renders_blank_viewport_space_when_scrolled_past_end() {
        for total_lines in [10, 60, 10_000] {
            let mut editor = crate::editor::Editor::new();
            editor.rope =
                ropey::Rope::from_str(&vec!["abcdefghijklmnopqrstuvwx"; total_lines].join("\n"));
            editor.viewport_height = 12;
            editor.scroll_past_end = true;
            editor.scroll_offset = total_lines.saturating_sub(editor.viewport_height);
            let mut terminal = Terminal::new(TestBackend::new(12, 12)).expect("terminal");
            let mut before = None;
            for past_end in [false, true] {
                if past_end {
                    editor.scroll_down(editor.viewport_height);
                    assert_eq!(editor.scroll_offset, total_lines - 1);
                }
                terminal
                    .draw(|frame| {
                        draw_minimap(
                            frame,
                            MinimapParams {
                                area: Rect::new(0, 0, 12, 12),
                                editor: &editor,
                                scroll: editor.scroll_offset,
                                visible_lines: editor.viewport_height,
                                search_matches: &[],
                                diagnostic_lines: &[],
                                diagnostic_index: None,
                                change_index: None,
                                diff_changes: None,
                                colors: test_colors(),
                                hovered: false,
                                active: false,
                            },
                        );
                    })
                    .expect("draw minimap");
                let buffer = terminal.backend().buffer();
                if past_end {
                    assert_ne!(buffer, before.as_ref().unwrap(), "{total_lines} lines");
                    let blank = &buffer[(6, 10)];
                    assert_eq!(blank.symbol(), " ", "blank space below EOF");
                    assert_ne!(blank.bg, test_colors().ui.bg_minimap);
                    assert_eq!(buffer[(6, 11)].symbol(), "▁");
                } else {
                    before = Some(buffer.clone());
                }
            }
        }
    }

    #[test]
    fn minimap_renders_viewport_borders_at_top_middle_and_bottom() {
        let mut editor = crate::editor::Editor::new();
        editor.scroll_past_end = false;
        editor.rope = ropey::Rope::from_str(&"line\n".repeat(9989));
        let mut terminal = Terminal::new(TestBackend::new(12, 30)).expect("terminal");
        for (scroll, top, bottom) in [(0, 0, 7), (4980, 11, 18), (9960, 22, 29)] {
            terminal
                .draw(|frame| {
                    draw_minimap(
                        frame,
                        MinimapParams {
                            area: Rect::new(0, 0, 12, 30),
                            editor: &editor,
                            scroll,
                            visible_lines: 30,
                            search_matches: &[],
                            diagnostic_lines: &[],
                            diagnostic_index: None,
                            change_index: None,
                            diff_changes: None,
                            colors: test_colors(),
                            hovered: false,
                            active: false,
                        },
                    );
                })
                .expect("draw minimap");
            let buffer = terminal.backend().buffer();
            for x in 0..12 {
                assert_eq!(buffer.cell((x, top)).unwrap().symbol(), "▔");
                assert_eq!(buffer.cell((x, bottom)).unwrap().symbol(), "▁");
            }
        }
    }

    #[test]
    fn minimap_preserves_slider_fills_across_themes_and_interaction_states() {
        let mut editor = crate::editor::Editor::new();
        editor.scroll_past_end = false;
        editor.rope = ropey::Rope::from_str(&"line\n".repeat(9989));
        let mut terminal = Terminal::new(TestBackend::new(12, 30)).expect("terminal");
        let mut palettes: Vec<_> = crate::ui_control_visibility_tests::audit_themes()
            .iter()
            .map(|theme| (theme.name.clone(), theme.to_theme_colors()))
            .collect();
        // A theme may intentionally make the slider transparent. Its outline
        // can remain visible without replacing the fill with a text color.
        let mut faint = test_colors();
        faint.ui.bg_minimap_slider = faint.ui.bg_minimap;
        faint.ui.bg_minimap_slider_hover = faint.ui.bg_minimap;
        faint.ui.bg_minimap_slider_active = faint.ui.bg_minimap;
        faint.fg_dim = faint.ui.bg_minimap;
        palettes.push(("Transparent slider".into(), faint));
        for (name, colors) in palettes {
            for (hovered, active, expected) in [
                (false, false, colors.ui.bg_minimap_slider),
                (true, false, colors.ui.bg_minimap_slider_hover),
                (false, true, colors.ui.bg_minimap_slider_active),
                (true, true, colors.ui.bg_minimap_slider_active),
            ] {
                for scroll in [0, 4980, 9960] {
                    editor.cursor_line = scroll + 15;
                    let layout = MinimapLayout::new(editor.rope.len_lines(), scroll, 30, 30, false);
                    terminal
                        .draw(|frame| {
                            draw_minimap(
                                frame,
                                MinimapParams {
                                    area: Rect::new(0, 0, 12, 30),
                                    editor: &editor,
                                    scroll,
                                    visible_lines: 30,
                                    search_matches: &[],
                                    diagnostic_lines: &[],
                                    diagnostic_index: None,
                                    change_index: None,
                                    diff_changes: None,
                                    colors,
                                    hovered,
                                    active,
                                },
                            )
                        })
                        .unwrap();
                    let buffer = terminal.backend().buffer();
                    if scroll == 4980 && !hovered && !active {
                        crate::ui_control_visibility_tests::snapshot(
                            buffer,
                            &format!("minimap {name}"),
                        );
                    }
                    for row in 0..30 {
                        let cell = &buffer[(6, row as u16)];
                        let in_viewport = (layout.view_start..layout.view_end).contains(&row);
                        assert_eq!(
                            cell.bg,
                            if in_viewport {
                                expected
                            } else {
                                colors.ui.bg_minimap
                            },
                            "{name}, hover={hovered}, active={active}, scroll={scroll}, row={row}"
                        );
                        if row == layout.view_start || row == layout.view_end - 1 {
                            if colors.ui.preserve_theme_colors {
                                assert_eq!(
                                    cell.fg,
                                    if hovered {
                                        colors.fg_text
                                    } else {
                                        colors.fg_dim
                                    },
                                    "{name}: viewport boundary retains the theme foreground"
                                );
                            } else {
                                assert!(
                                    crate::ui_colors::contrast_ratio(cell.fg, cell.bg).unwrap()
                                        >= 3.0
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn minimap_and_scrollbar_keep_subtle_translucent_theme_fills() {
        let mut editor = crate::editor::Editor::new();
        editor.scroll_past_end = false;
        editor.rope = ropey::Rope::from_str(&vec!["some code here"; 90].join("\n"));
        editor.cursor_line = 15;
        let mut colors = test_colors();
        let values = serde_json::json!({
            "minimap.background": "#101010",
            "editorOverviewRuler.background": "#101010",
            "scrollbarSlider.background": "#40404055",
            "scrollbarSlider.hoverBackground": "#60606055",
            "scrollbarSlider.activeBackground": "#90909055"
        });
        colors.ui = crate::theme_workbench::WorkbenchColors::resolve(
            values.as_object().unwrap(),
            &colors,
            false,
            false,
        );
        let mut terminal = Terminal::new(TestBackend::new(14, 30)).unwrap();
        let change_rows = ChangeRows::default();
        for (hovered, active, minimap_gray, scrollbar_gray) in [
            (false, false, 24, 32),
            (true, false, 29, 43),
            (false, true, 38, 59),
            (true, true, 38, 59),
        ] {
            terminal
                .draw(|frame| {
                    draw_minimap(
                        frame,
                        MinimapParams {
                            area: Rect::new(0, 0, 12, 30),
                            editor: &editor,
                            scroll: 0,
                            visible_lines: 30,
                            search_matches: &[],
                            diagnostic_lines: &[],
                            diagnostic_index: None,
                            change_index: None,
                            diff_changes: None,
                            colors,
                            hovered,
                            active,
                        },
                    );
                    draw_scrollbar(
                        frame,
                        ScrollbarParams {
                            area: Rect::new(12, 0, 2, 30),
                            total_lines: 90,
                            visible_lines: 30,
                            scroll: 0,
                            search_matches: &[],
                            diagnostic_lines: &[],
                            diagnostic_index: None,
                            change_rows: &change_rows,
                            cursor_line: 89,
                            colors,
                            hovered,
                            active,
                        },
                    );
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            for y in 0..10 {
                for x in 0..12 {
                    assert_eq!(
                        buffer[(x, y)].bg,
                        Color::Rgb(minimap_gray, minimap_gray, minimap_gray)
                    );
                }
            }
            for y in 0..10 {
                assert_eq!(
                    buffer[(12, y)].bg,
                    Color::Rgb(scrollbar_gray, scrollbar_gray, scrollbar_gray)
                );
            }
            // The editor's cursor line must not introduce a bright row inside
            // the slider. Selection and diagnostic markers remain separate.
            editor.select_all();
            terminal
                .draw(|frame| {
                    draw_minimap(
                        frame,
                        MinimapParams {
                            area: Rect::new(0, 0, 12, 30),
                            editor: &editor,
                            scroll: 0,
                            visible_lines: 30,
                            search_matches: &[],
                            diagnostic_lines: &[],
                            diagnostic_index: None,
                            change_index: None,
                            diff_changes: None,
                            colors,
                            hovered,
                            active,
                        },
                    )
                })
                .unwrap();
            assert_eq!(
                terminal.backend().buffer()[(6, 1)].bg,
                colors.ui.bg_minimap_selection
            );
            editor.clear_selection();
        }
    }
}
