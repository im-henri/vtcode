//! Finite cosmetic overlays. Geometry comes from the real editor renderer;
//! effects never move the editing caret, mutate text, or delay input.
use crate::{
    animation_config::AnimationEffect as Effect,
    animations::{Anchor, Snapshot, Trigger},
    app::App,
    app_theme::{ThemeColors, lerp_color},
};
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
};
#[cfg(test)]
use std::time::Duration;
use std::{
    borrow::Cow,
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::OnceLock,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthChar;
use unicode_width::UnicodeWidthStr;

#[path = "ui_animation_layers.rs"]
mod animation_layers;

const MAX_ANCHORS: usize = 32_768;
type ScopePair = ((usize, usize), (usize, usize));
pub(crate) type CursorUnderlay = (Rect, Style);

fn delimiter_family(ch: char) -> Option<u8> {
    match ch {
        '{' | '}' => Some(0),
        '[' | ']' => Some(1),
        '(' | ')' => Some(2),
        '<' | '>' => Some(3),
        _ => None,
    }
}

fn closing_delimiter(ch: char) -> Option<char> {
    match ch {
        '{' => Some('}'),
        '[' => Some(']'),
        '(' => Some(')'),
        '<' => Some('>'),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ScopeEndpointHint {
    // Stable indentation/branch attachment for the fallback rail.
    attachment: (usize, usize),
    x: u16,
    precise: bool,
    // Actual delimiter column, independent of the first token on its row.
    delimiter_x: Option<u16>,
}

/// Resolve only the two bounded source lines used by a live scope. The
/// attachment is independent of viewport clipping, including siblings that
/// are currently beyond its horizontal edge.
fn source_scope_endpoint(
    editor: &crate::editor::Editor,
    geometry: &Geometry,
    position: (usize, usize),
) -> Option<ScopeEndpointHint> {
    let line = editor.rope.get_line(position.0)?;
    if line.len_chars() > crate::background::MAX_BRACKET_LINE_CHARS {
        return None;
    }
    let text = line.to_string();
    let text = text.trim_end_matches(['\r', '\n']);
    let chars: Vec<_> = text.chars().collect();
    let &delimiter = chars.get(position.1)?;
    let (start, end) = if let Some(width) = geometry.wrap_width {
        let ranges =
            crate::editor::Editor::wrap_ranges_for_line_text_tabbed(text, width, editor.tab_size);
        ranges
            .into_iter()
            .find(|&(start, end)| position.1 >= start && position.1 < end)?
    } else {
        (0, chars.len())
    };
    // The renderer rejects rows whose scalar widths cannot identify terminal
    // cells reliably. An offscreen source hint must follow that same rule.
    if chars[start..end].iter().any(|&ch| {
        UnicodeWidthChar::width(ch).unwrap_or(0) == 0 && ch != '\t'
            || ('\u{1f1e6}'..='\u{1f1ff}').contains(&ch)
    }) {
        return None;
    }
    let precise = delimiter_family(delimiter).is_some()
        && chars[start..end]
            .iter()
            .filter(|&&ch| ch == delimiter)
            .take(2)
            .count()
            > 1;
    let branch = (!precise)
        .then(|| closing_delimiter(delimiter))
        .flatten()
        .filter(|closing| {
            chars[start..end]
                .iter()
                .copied()
                .find(|ch| !matches!(ch, ' ' | '\t'))
                == Some(*closing)
        })
        .and_then(|closing| {
            (start..=position.1)
                .find(|&col| chars[col] != closing && !matches!(chars[col], ' ' | '\t'))
        });
    let attachment = if let Some(branch) = branch {
        branch
    } else if precise {
        position.1
    } else {
        (start..=position.1)
            .find(|&col| !matches!(chars[col], ' ' | '\t'))
            .unwrap_or(position.1)
    };
    let mut visual_columns = Vec::with_capacity(chars.len() + 1);
    visual_columns.push(0usize);
    let mut tab_column = 0usize;
    for &ch in &chars {
        let visual = *visual_columns.last().unwrap();
        let width = if ch == '\t' {
            let width = editor.tab_size.max(1) - tab_column % editor.tab_size.max(1);
            tab_column = tab_column.saturating_add(width);
            width
        } else {
            // Match the editor's tab-stop accumulator separately from its
            // terminal glyph widths (a wide scalar advances it by one).
            tab_column = tab_column.saturating_add(1);
            UnicodeWidthChar::width(ch).unwrap_or(0)
        };
        visual_columns.push(visual.saturating_add(width));
    }
    // Horizontal scroll is a source-character offset. Tabs keep their global
    // tab stops while the renderer starts its first visible character at x=0.
    let origin = if geometry.wrap_width.is_some() {
        start
    } else {
        editor.horizontal_scroll.min(chars.len())
    };
    let mut column = visual_columns[attachment];
    let mut delimiter_column = visual_columns[position.1];
    let mut offset = visual_columns[origin];
    if let Some(named) = geometry.color_preview {
        let mut previews: Vec<_> = crate::ui_colors::find_inline_colors(text, named)
            .into_iter()
            .map(|(col, _, _, _)| col)
            .collect();
        previews.sort_unstable();
        previews.dedup();
        column += previews.partition_point(|&col| col <= attachment);
        delimiter_column += previews.partition_point(|&col| col <= position.1);
        offset += previews.partition_point(|&col| col < origin);
    }
    let project = |column: usize| {
        if column >= offset {
            geometry
                .area
                .x
                .saturating_add(u16::try_from(column - offset).unwrap_or(u16::MAX))
        } else {
            geometry
                .area
                .x
                .saturating_sub(u16::try_from(offset - column).unwrap_or(u16::MAX))
        }
    };
    Some(ScopeEndpointHint {
        attachment: (position.0, attachment),
        x: project(column),
        precise,
        delimiter_x: delimiter_family(delimiter).map(|_| project(delimiter_column)),
    })
}

pub(crate) struct DiffScopeOptions<'a> {
    pub(crate) source: &'a crate::editor::Editor,
    pub(crate) index: Option<&'a crate::animation_structural_scope::StructuralScopeIndex>,
    pub(crate) bracket_index: Option<&'a crate::background::BracketDepthIndex>,
    pub(crate) colorize: bool,
    pub(crate) selection: Option<(usize, usize, usize, usize)>,
    pub(crate) cell_column: Option<&'a dyn Fn((usize, usize)) -> Option<usize>>,
    pub(crate) line_summary: Option<&'a dyn Fn(usize) -> Option<crate::app::DiffScopeLineSummary>>,
}

struct DiffEndpointCells {
    text: std::sync::Arc<str>,
    checkpoints: Vec<(usize, usize, usize)>,
    len: usize,
    cells: usize,
}

fn diff_grapheme_width(grapheme: &str) -> usize {
    if grapheme.contains(char::is_control) {
        0
    } else {
        grapheme.width()
    }
}

impl DiffEndpointCells {
    fn new(text: &str) -> Self {
        let mut checkpoints = Vec::new();
        let mut len = 0usize;
        let mut cells = 0usize;
        let mut next = 0usize;
        for (byte, grapheme) in text.grapheme_indices(true) {
            if len >= next {
                checkpoints.push((len, byte, cells));
                next = len.saturating_add(256);
            }
            len += grapheme.chars().count();
            cells += diff_grapheme_width(grapheme);
        }
        Self {
            text: text.into(),
            checkpoints,
            len,
            cells,
        }
    }

    fn column(&self, column: usize) -> usize {
        if column >= self.len {
            return self.cells;
        }
        let index = self
            .checkpoints
            .partition_point(|&(character, _, _)| character <= column)
            .saturating_sub(1);
        let (mut character, byte, mut cell) =
            self.checkpoints.get(index).copied().unwrap_or((0, 0, 0));
        for grapheme in self.text[byte..].graphemes(true) {
            let end = character + grapheme.chars().count();
            if column < end {
                break;
            }
            cell += diff_grapheme_width(grapheme);
            character = end;
            if character == column {
                break;
            }
        }
        cell
    }
}

struct DiffEndpointLine {
    summary: crate::app::DiffScopeLineSummary,
    fallback: Option<DiffEndpointCells>,
}

#[derive(Clone, Copy)]
struct CanonicalDiffEndpoint {
    attachment: (usize, usize),
    cell: usize,
    precise: bool,
    delimiter_cell: Option<usize>,
}

/// Only live source endpoints are retained. Available diff rows reuse their
/// worker-built cell indexes; omitted rows acquire one sparse, cached index.
#[derive(Default)]
pub(crate) struct DiffScopePaintState {
    source: Option<ropey::Rope>,
    navigation_id: u64,
    content_version: u64,
    wrap_revision: u64,
    path: Option<std::path::PathBuf>,
    language: Option<String>,
    lines: HashMap<usize, DiffEndpointLine>,
    endpoints: HashMap<(usize, usize), Option<CanonicalDiffEndpoint>>,
}

impl DiffScopePaintState {
    fn synchronize(&mut self, source: &crate::editor::Editor) {
        let same = self.navigation_id == source.navigation_id
            && self.content_version == source.content_version
            && self.wrap_revision == source.wrap_revision
            && self
                .source
                .as_ref()
                .is_some_and(|old| old.is_instance(&source.rope))
            && self.path.as_deref() == source.syntax_file_path()
            && self.language.as_deref() == source.syntax_language_token();
        if same {
            return;
        }
        *self = Self {
            source: Some(source.rope.clone()),
            navigation_id: source.navigation_id,
            content_version: source.content_version,
            wrap_revision: source.wrap_revision,
            path: source.syntax_file_path().map(std::path::Path::to_path_buf),
            language: source.syntax_language_token().map(str::to_owned),
            ..Self::default()
        };
    }

    fn line(
        &mut self,
        options: &DiffScopeOptions<'_>,
        line: usize,
    ) -> Option<&mut DiffEndpointLine> {
        if let std::collections::hash_map::Entry::Vacant(entry) = self.lines.entry(line) {
            if let Some(summary) = options.line_summary.and_then(|summary| summary(line)) {
                entry.insert(DiffEndpointLine {
                    summary,
                    fallback: None,
                });
                return self.lines.get_mut(&line);
            }
            let slice = options.source.rope.get_line(line)?;
            let owned;
            let text = if let Some(text) = slice.as_str() {
                text
            } else {
                owned = slice.to_string();
                &owned
            };
            let text = text.trim_end_matches(['\n', '\r']);
            entry.insert(DiffEndpointLine {
                summary: crate::app::DiffScopeLineSummary::from_text(text),
                fallback: None,
            });
        }
        self.lines.get_mut(&line)
    }

    fn cell_column(
        &mut self,
        options: &DiffScopeOptions<'_>,
        position: (usize, usize),
    ) -> Option<usize> {
        if let Some(cell) = options.cell_column.and_then(|column| column(position)) {
            return Some(cell);
        }
        let line = self.line(options, position.0)?;
        if line.fallback.is_none() {
            let slice = options.source.rope.get_line(position.0)?;
            let owned;
            let text = if let Some(text) = slice.as_str() {
                text
            } else {
                owned = slice.to_string();
                &owned
            };
            line.fallback = Some(DiffEndpointCells::new(text.trim_end_matches(['\n', '\r'])));
        }
        Some(line.fallback.as_ref()?.column(position.1))
    }

    fn endpoint(
        &mut self,
        options: &DiffScopeOptions<'_>,
        geometry: &Geometry,
        position: (usize, usize),
    ) -> Option<ScopeEndpointHint> {
        if !self.endpoints.contains_key(&position) {
            let result = (|| {
                let delimiter = options
                    .source
                    .rope
                    .get_line(position.0)?
                    .get_char(position.1)?;
                let line = self.line(options, position.0)?;
                let precise = line.summary.repeated(delimiter);
                let branch = (!precise)
                    .then(|| closing_delimiter(delimiter))
                    .flatten()
                    .filter(|closing| line.summary.first().map(|(_, ch)| ch) == Some(*closing))
                    .and_then(|closing| {
                        line.summary
                            .branch()
                            .filter(|&(first, column)| first == closing && column <= position.1)
                    })
                    .map(|(_, column)| column);
                let column = branch.unwrap_or_else(|| {
                    if precise {
                        position.1
                    } else {
                        line.summary
                            .first()
                            .map_or(position.1, |(column, _)| column)
                            .min(position.1)
                    }
                });
                let attachment = (position.0, column);
                let cell = self.cell_column(options, attachment)?;
                let delimiter_cell =
                    delimiter_family(delimiter).and_then(|_| self.cell_column(options, position));
                Some(CanonicalDiffEndpoint {
                    attachment,
                    cell,
                    precise,
                    delimiter_cell,
                })
            })();
            self.endpoints.insert(position, result);
        }
        let endpoint = self.endpoints.get(&position).copied().flatten()?;
        let (pan, inline) = geometry
            .diff_projection
            .unwrap_or((options.source.horizontal_scroll, false));
        let origin = if inline {
            pan
        } else {
            self.cell_column(options, (position.0, pan))?
        };
        let project = |cell: usize| {
            if cell >= origin {
                geometry
                    .area
                    .x
                    .saturating_add(u16::try_from(cell - origin).unwrap_or(u16::MAX))
            } else {
                geometry
                    .area
                    .x
                    .saturating_sub(u16::try_from(origin - cell).unwrap_or(u16::MAX))
            }
        };
        Some(ScopeEndpointHint {
            attachment: endpoint.attachment,
            x: project(endpoint.cell),
            precise: endpoint.precise,
            delimiter_x: endpoint.delimiter_cell.map(project),
        })
    }
}

#[cfg(test)]
thread_local! {
    pub(crate) static REFERENCE_RETAINED_GEOMETRY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static REFERENCE_PAINT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Clone)]
pub(crate) struct Geometry {
    pub(crate) area: Rect,
    gradient_area: Option<Rect>,
    deferred_gradient_area: Option<Rect>,
    enabled: bool,
    pub(crate) anchors: Vec<Anchor>,
    ordered: bool,
    projection_id: u64,
    retained: bool,
    offsets: RefCell<Option<(ropey::Rope, Rc<Vec<(usize, Anchor)>>)>>,
    paint_scratch: RefCell<PaintScratch>,
    source_rows: OnceLock<std::collections::BTreeMap<usize, (u16, u16)>>,
    source_cells: OnceLock<animation_layers::SourceCells>,
    fill_rows: RefCell<Vec<(ScopePair, Rc<context::fill::Ownership>)>>,
    wide_cells: OnceLock<rustc_hash::FxHashSet<(u16, u16)>>,
    protection: RefCell<Option<Rc<OverlayProtection>>>,
    unsafe_rows: HashSet<u16>,
    primary_cursor: Option<(Rect, Style)>,
    wrap_width: Option<usize>,
    bounded_source: bool,
    color_preview: Option<bool>,
    diff_projection: Option<(usize, bool)>,
    protected: HashSet<(u16, u16)>,
    obstacles: HashSet<(u16, u16)>,
    overlays: HashSet<(u16, u16)>,
    omitted_lines: Vec<(usize, usize, u16)>,
    scope_rows: Vec<(usize, usize, u16)>,
}

impl Geometry {
    fn next_projection() -> u64 {
        static NEXT_PROJECTION: AtomicU64 = AtomicU64::new(1);
        NEXT_PROJECTION.fetch_add(1, Ordering::Relaxed)
    }

    pub(crate) fn new(area: Rect, enabled: bool) -> Self {
        Self {
            area,
            gradient_area: Some(area),
            deferred_gradient_area: None,
            enabled,
            anchors: Vec::new(),
            ordered: true,
            projection_id: Self::next_projection(),
            retained: false,
            offsets: RefCell::new(None),
            paint_scratch: RefCell::new(PaintScratch::default()),
            source_rows: OnceLock::new(),
            source_cells: OnceLock::new(),
            fill_rows: RefCell::new(Vec::new()),
            wide_cells: OnceLock::new(),
            protection: RefCell::new(None),
            unsafe_rows: HashSet::new(),
            primary_cursor: None,
            wrap_width: None,
            bounded_source: false,
            color_preview: None,
            diff_projection: None,
            protected: HashSet::new(),
            obstacles: HashSet::new(),
            overlays: HashSet::new(),
            omitted_lines: Vec::new(),
            scope_rows: Vec::new(),
        }
    }

    pub(crate) fn configure_recycled(&mut self, area: Rect, enabled: bool) {
        debug_assert!(self.anchors.is_empty() && !self.retained);
        self.area = area;
        self.gradient_area = Some(area);
        self.enabled = enabled;
    }

    /// Keep only scratch capacity between input frames. Source projections,
    /// masks and cached colors must be rebuilt for the new document/viewport.
    pub(crate) fn reset(&mut self, area: Rect, enabled: bool) {
        let mut fresh = Self::new(area, enabled);
        std::mem::swap(&mut fresh.anchors, &mut self.anchors);
        std::mem::swap(&mut fresh.unsafe_rows, &mut self.unsafe_rows);
        std::mem::swap(&mut fresh.protected, &mut self.protected);
        std::mem::swap(&mut fresh.obstacles, &mut self.obstacles);
        std::mem::swap(&mut fresh.overlays, &mut self.overlays);
        std::mem::swap(&mut fresh.omitted_lines, &mut self.omitted_lines);
        std::mem::swap(&mut fresh.scope_rows, &mut self.scope_rows);
        std::mem::swap(&mut fresh.paint_scratch, &mut self.paint_scratch);
        fresh.anchors.clear();
        fresh.unsafe_rows.clear();
        fresh.protected.clear();
        fresh.obstacles.clear();
        fresh.overlays.clear();
        fresh.omitted_lines.clear();
        fresh.scope_rows.clear();
        let scratch = fresh.paint_scratch.get_mut();
        scratch.layers.clear();
        scratch.scope_colors.clear();
        scratch.scope_endpoints.clear();
        scratch.scope_shapes.clear();
        scratch.contrast.clear();
        scratch.fire_core.clear();
        scratch.pairs.clear();
        scratch.positions.clear();
        *self = fresh;
    }
    #[inline]
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }
    pub(crate) fn set_wrapped(&mut self, width: usize) {
        self.wrap_width = Some(width.max(1));
    }
    pub(crate) fn map_scope_row(&mut self, line: usize, start_col: usize, screen_row: usize) {
        if self.enabled && screen_row < self.area.height as usize {
            self.invalidate_rows();
            self.scope_rows
                .push((line, start_col, self.area.y + screen_row as u16));
        }
    }
    pub(crate) fn set_bounded_source(&mut self) {
        self.bounded_source = true;
    }
    pub(crate) fn set_color_preview(&mut self, named: bool) {
        self.color_preview = Some(named);
    }
    pub(crate) fn set_diff_projection(&mut self, pan: usize, inline: bool) {
        self.diff_projection = Some((pan, inline));
        // Diff cells already have their final source/selection background.
        self.gradient_area = None;
    }
    pub(crate) fn record_diff_cursor(
        &mut self,
        position: (usize, usize),
        underlay: CursorUnderlay,
        ch: char,
    ) {
        let (area, _) = underlay;
        if !self.area.contains((area.x, area.y).into()) {
            return;
        }
        self.protection.get_mut().take();
        self.primary_cursor = Some(underlay);
        // Carets inside a grapheme or on an omitted control character share
        // its rendered cell, even when no exact source anchor exists.
        if self.at(position).is_none() {
            self.invalidate_rows();
            if !self.ordered {
                self.anchors.push(Anchor {
                    line: position.0,
                    col: position.1,
                    ch,
                    x: area.x,
                    y: area.y,
                    width: area.width,
                });
                return;
            }
            let index = self
                .anchors
                .partition_point(|anchor| (anchor.line, anchor.col) < position);
            self.anchors.insert(
                index,
                Anchor {
                    line: position.0,
                    col: position.1,
                    ch,
                    x: area.x,
                    y: area.y,
                    width: area.width,
                },
            );
        }
    }
    pub(crate) fn protect(&mut self, row: usize, column: usize, width: usize) {
        if row >= self.area.height as usize {
            return;
        }
        let y = self.area.y + row as u16;
        self.protection.get_mut().take();
        for column in column..column.saturating_add(width).min(self.area.width as usize) {
            self.protected.insert((self.area.x + column as u16, y));
        }
    }
    pub(crate) fn obstacle(&mut self, row: usize, column: usize, width: usize) {
        if row >= self.area.height as usize {
            return;
        }
        let y = self.area.y + row as u16;
        self.invalidate_retained();
        self.protection.get_mut().take();
        for column in column..column.saturating_add(width).min(self.area.width as usize) {
            self.obstacles.insert((self.area.x + column as u16, y));
        }
    }
    /// Rendered annotations cover tracing ink without changing source routes.
    /// Include their spaces so a bubble never leaks through an annotation.
    pub(crate) fn overlay(&mut self, row: usize, column: usize, width: usize) {
        if row >= self.area.height as usize {
            return;
        }
        let y = self.area.y + row as u16;
        self.protection.get_mut().take();
        for column in column..column.saturating_add(width).min(self.area.width as usize) {
            self.overlays.insert((self.area.x + column as u16, y));
        }
    }
    pub(crate) fn omit_lines(&mut self, start: usize, end: usize, row: usize) {
        if start < end && row < self.area.height as usize {
            self.omitted_lines
                .push((start, end, self.area.y + row as u16));
        }
    }
    /// A visible diff row can have no anchors when horizontal panning clips
    /// every glyph. Preserve its exact screen row instead of treating that
    /// source line as folded out or clipping it to a viewport boundary.
    pub(crate) fn map_source_row(&mut self, line: usize, row: usize) {
        self.map_scope_row(line, 0, row);
        self.omit_lines(line, line.saturating_add(1), row);
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn push_diff(
        &mut self,
        line: usize,
        col: usize,
        ch: char,
        row: usize,
        column: usize,
        width: usize,
    ) {
        if !self.enabled
            || row >= self.area.height as usize
            || column >= self.area.width as usize
            || width == 0
            || width > self.area.width as usize - column
        {
            return;
        }
        self.invalidate_rows();
        self.ordered &= self
            .anchors
            .last()
            .is_none_or(|last| (last.line, last.col) <= (line, col));
        self.anchors.push(Anchor {
            line,
            col,
            ch,
            x: self.area.x + column as u16,
            y: self.area.y + row as u16,
            width: width as u16,
        });
    }
    /// Large-file backgrounds receive their gradient in the outer pane pass.
    /// Restore the base style there so that pass uses its original projection.
    pub(crate) fn defer_gradients(&mut self, area: Option<Rect>) {
        self.gradient_area = None;
        self.deferred_gradient_area = area;
    }
    #[inline]
    pub(crate) fn push(
        &mut self,
        line: usize,
        col: usize,
        ch: char,
        row: usize,
        column: usize,
        width: usize,
    ) {
        if !self.enabled || row >= self.area.height as usize {
            return;
        }
        let y = self.area.y.saturating_add(row as u16);
        // Clustering across spans can change the width of combining sequences.
        // Skip ambiguous rows instead of guessing which source cell owns them.
        if (!matches!(ch, '\0' | '\t') && UnicodeWidthChar::width(ch).unwrap_or(0) == 0)
            || ('\u{1f1e6}'..='\u{1f1ff}').contains(&ch)
        {
            // One ambiguous glyph invalidates the row. Combining sequences
            // can contain thousands of scalars; later ones must not repeatedly
            // scan all anchors from the earlier viewport rows.
            if self.unsafe_rows.insert(y) {
                self.invalidate_rows();
                self.anchors.retain(|a| a.y != y);
            }
            return;
        }
        if self.unsafe_rows.contains(&y)
            || self.anchors.len() >= MAX_ANCHORS
            || column >= self.area.width as usize
            || width == 0
            || width > self.area.width as usize - column
        {
            return;
        }
        self.invalidate_rows();
        self.ordered &= self
            .anchors
            .last()
            .is_none_or(|last| (last.line, last.col) <= (line, col));
        self.anchors.push(Anchor {
            line,
            col,
            ch,
            x: self.area.x.saturating_add(column as u16),
            y,
            width: width as u16,
        });
    }
    /// The text renderer has already checked this run's one-column
    /// glyphs. Validate clipping, row safety and derived caches once per run.
    pub(crate) fn push_unit_width_run(&mut self, line: usize, col: usize, chars: &[char], row: usize, column: usize) {
        if !self.enabled || row >= self.area.height as usize || column >= self.area.width as usize {
            return;
        }
        let y = self.area.y.saturating_add(row as u16);
        if self.unsafe_rows.contains(&y) { return; }
        let count = chars.len().min(self.area.width as usize - column)
            .min(MAX_ANCHORS.saturating_sub(self.anchors.len()));
        if count == 0 { return; }
        self.invalidate_rows();
        self.ordered &= self.anchors.last().is_none_or(|last| (last.line, last.col) <= (line, col));
        self.anchors.extend(chars[..count].iter().enumerate().map(|(offset, &ch)| {
            debug_assert!(unicode_width::UnicodeWidthChar::width(ch) == Some(1)
                && !('\u{1f1e6}'..='\u{1f1ff}').contains(&ch));
            Anchor {
                line, col: col + offset, ch,
                x: self.area.x.saturating_add((column + offset) as u16), y, width: 1,
            }
        }));
    }

    fn at(&self, position: (usize, usize)) -> Option<Anchor> {
        if self.ordered {
            let index = self.anchors.partition_point(|a| (a.line, a.col) < position);
            return self
                .anchors
                .get(index)
                .filter(|a| (a.line, a.col) == position)
                .copied();
        }
        self.anchors
            .iter()
            .find(|a| (a.line, a.col) == position)
            .copied()
    }
    /// Renderer anchors normally arrive in source order. Limit endpoint work to
    /// its line, while retaining the safe scan for unusual diff projections.
    fn line_anchors(&self, line: usize) -> &[Anchor] {
        if !self.ordered {
            return &self.anchors;
        }
        let first = self.anchors.partition_point(|a| a.line < line);
        let last = first + self.anchors[first..].partition_point(|a| a.line == line);
        &self.anchors[first..last]
    }
    fn invalidate_rows(&mut self) {
        self.invalidate_retained();
        self.offsets.get_mut().take();
        self.source_rows.take();
        self.source_cells.take();
        self.fill_rows.get_mut().clear();
        self.wide_cells.take();
        self.protection.get_mut().take();
    }

    fn source_cells(&self) -> &animation_layers::SourceCells {
        self.source_cells
            .get_or_init(|| animation_layers::SourceCells::new(self.area, &self.anchors))
    }

    fn invalidate_retained(&mut self) {
        if self.retained {
            self.projection_id = Self::next_projection();
            self.retained = false;
        }
    }

    fn is_retained(&self) -> bool {
        #[cfg(test)]
        if REFERENCE_RETAINED_GEOMETRY.get() { return false; }
        self.retained
    }

    /// Only a captured scene may bypass validation: cosmetic replay restores
    /// exactly these cells before painting effects. Ordinary draws still check
    /// annotations and overlays against the actual terminal buffer.
    pub(crate) fn retain_validated(&mut self, buffer: &Buffer) {
        if !self.is_retained() {
            // This caller owns the geometry. Filter it in place instead of
            // cloning all anchors, masks, and paint scratch when one glyph was
            // clipped or covered by an annotation. Keep recycled allocations.
            let previous = (self.anchors.len(), self.scope_rows.len());
            self.anchors.retain(|anchor| Self::anchor_is_visible(anchor, buffer));
            let area = self.area;
            self.scope_rows.retain(|row| row.2 >= area.y && row.2 < area.bottom());
            if previous != (self.anchors.len(), self.scope_rows.len()) {
                self.invalidate_rows();
            }
        }
        self.retained = true;
    }

    /// Reuse only source geometry, after validating the newly painted cells.
    /// Carets, selections, annotations and styles always belong to this frame.
    /// Exact comparisons avoid a hash collision or a missed edit reusing routes.
    pub(crate) fn reuse_projection(&mut self, old: &mut Self) -> bool {
        #[cfg(test)]
        if REFERENCE_RETAINED_GEOMETRY.get() { return false; }
        if !self.retained || !old.retained || !self.enabled || !old.enabled
            || self.area != old.area || self.ordered != old.ordered
            || self.wrap_width != old.wrap_width || self.bounded_source != old.bounded_source
            || self.color_preview != old.color_preview || self.diff_projection != old.diff_projection
            || self.anchors != old.anchors || self.scope_rows != old.scope_rows
            || self.omitted_lines != old.omitted_lines || self.unsafe_rows != old.unsafe_rows
            || self.obstacles != old.obstacles
        {
            return false;
        }
        self.projection_id = old.projection_id;
        std::mem::swap(&mut self.source_rows, &mut old.source_rows);
        std::mem::swap(&mut self.source_cells, &mut old.source_cells);
        std::mem::swap(&mut self.fill_rows, &mut old.fill_rows);
        std::mem::swap(&mut self.wide_cells, &mut old.wide_cells);
        std::mem::swap(&mut self.offsets, &mut old.offsets);
        true
    }

    fn source_offsets(&self, rope: &ropey::Rope) -> Rc<Vec<(usize, Anchor)>> {
        #[cfg(test)]
        let reuse = !REFERENCE_RETAINED_GEOMETRY.get();
        #[cfg(not(test))]
        let reuse = true;
        if reuse && let Some((old, offsets)) = self.offsets.borrow().as_ref()
            && old.is_instance(rope)
        {
            return Rc::clone(offsets);
        }
        let mut cached_line = usize::MAX;
        let mut line_start = 0;
        let offsets = Rc::new(self.anchors.iter().map(|anchor| {
            if anchor.line != cached_line {
                cached_line = anchor.line;
                line_start = rope.line_to_char(cached_line);
            }
            (line_start.saturating_add(anchor.col), *anchor)
        }).collect());
        *self.offsets.borrow_mut() = Some((rope.clone(), Rc::clone(&offsets)));
        offsets
    }
    /// Save the real rendered style below the primary caret. Only a live smear
    /// removes that caret; the document position and extra carets stay immediate.
    pub(crate) fn record_primary_cursor(
        &mut self,
        row: usize,
        column: usize,
        width: usize,
        style: Style,
    ) {
        if !self.enabled || row >= self.area.height as usize || column >= self.area.width as usize {
            return;
        }
        self.protection.get_mut().take();
        self.primary_cursor = Some((
            Rect::new(
                self.area.x.saturating_add(column as u16),
                self.area.y.saturating_add(row as u16),
                width.min(self.area.width as usize - column) as u16,
                1,
            ),
            style,
        ));
    }
    pub(crate) fn occlude_top(&mut self, rows: u16) {
        self.invalidate_rows();
        let rows = rows.min(self.area.height);
        self.area.y = self.area.y.saturating_add(rows);
        self.area.height -= rows;
        self.anchors.retain(|a| a.y >= self.area.y);
        if self
            .primary_cursor
            .is_some_and(|(area, _)| area.y < self.area.y)
        {
            self.primary_cursor = None;
        }
    }
    fn anchor_is_visible(a: &Anchor, buffer: &Buffer) -> bool {
        buffer.cell((a.x, a.y)).is_some_and(|cell| match a.ch {
            ' ' | '\t' => matches!(cell.symbol(), " " | "·" | "→" | "│" | "┊" | "┆" | "┃"),
            // A column ruler may decorate the blank end-of-line caret.
            '\0' => matches!(cell.symbol(), " " | "│" | "┊" | "┆" | "┃"),
            ch => cell.symbol().starts_with(ch),
        })
    }

    fn validated<'a>(&'a self, buffer: &Buffer) -> Cow<'a, Self> {
        if self.is_retained() { return Cow::Borrowed(self); }
        let valid = |a: &Anchor| Self::anchor_is_visible(a, buffer);
        let row_valid =
            |row: &(usize, usize, u16)| row.2 >= self.area.y && row.2 < self.area.bottom();
        if self.anchors.iter().all(valid) && self.scope_rows.iter().all(row_valid) {
            return Cow::Borrowed(self);
        }
        let mut validated = self.clone();
        validated.invalidate_rows();
        validated.anchors.retain(valid);
        validated.scope_rows.retain(row_valid);
        Cow::Owned(validated)
    }
}

fn selected(anchor: &Anchor, range: Option<(usize, usize, usize, usize)>) -> bool {
    range.is_some_and(|(sl, sc, el, ec)| {
        (anchor.line, anchor.col) >= (sl, sc) && (anchor.line, anchor.col) < (el, ec)
    })
}

/// Selection and caret ownership is immutable between cosmetic frames. Keep
/// one bounded entry with the geometry, and borrow its mask while painting.
#[derive(Clone)]
struct OverlayProtection {
    cursor: Option<(u16, u16)>,
    selection: Option<(usize, usize, usize, usize)>,
    extra_cursors: Vec<(usize, usize)>,
    extra_cursor_anchors: Vec<Option<(usize, usize)>>,
    protected: HashSet<(u16, u16)>,
    carets: HashSet<(u16, u16)>,
    mask: Option<animation_layers::CellMask>,
}

#[cfg(test)]
impl OverlayProtection {
    fn protected_cells(&self) -> HashSet<(u16, u16)> {
        self.protected.iter().copied().chain(self.mask.iter()
            .flat_map(|mask| mask.cells(animation_layers::PROTECTED))).collect()
    }

    fn caret_cells(&self) -> HashSet<(u16, u16)> {
        self.carets.iter().copied().chain(self.mask.iter()
            .flat_map(|mask| mask.cells(animation_layers::CARET))).collect()
    }
}

impl Geometry {
    #[cfg(test)]
    pub(crate) fn clear_protection_cache(&self) -> bool {
        self.protection.borrow_mut().take().is_some()
    }

    fn protection(
        &self,
        cursor: Option<Anchor>,
        selection: Option<(usize, usize, usize, usize)>,
        extra_cursors: &[(usize, usize)],
        extra_cursor_anchors: &[Option<(usize, usize)>],
    ) -> Rc<OverlayProtection> {
        let cursor_line = cursor.map(|anchor| anchor.line);
        let cursor = cursor.map(|anchor| (anchor.x, anchor.y));
        if let Some(cached) = self.protection.borrow().as_ref()
            && cached.cursor == cursor
            && cached.selection == selection
            && cached.extra_cursors == extra_cursors
            && cached.extra_cursor_anchors == extra_cursor_anchors
        {
            return Rc::clone(cached);
        }
        let extra: HashSet<_> = extra_cursors.iter().copied().collect();
        let mut ranges: Vec<_> = extra_cursors
            .iter()
            .enumerate()
            .filter_map(|(i, &end)| {
                extra_cursor_anchors.get(i).and_then(|a| *a).map(|start| {
                    if start <= end {
                        (start, end)
                    } else {
                        (end, start)
                    }
                })
            })
            .collect();
        ranges.sort_unstable();
        let mut merged: Vec<((usize, usize), (usize, usize))> = Vec::new();
        for (start, end) in ranges {
            if let Some((_, prev)) = merged.last_mut()
                && *prev >= start
            {
                *prev = (*prev).max(end);
            } else {
                merged.push((start, end));
            }
        }
        // Write viewport cells once. Hash sets only cover the sparse fallback
        // (oversized rectangles or source projections outside the viewport).
        let mut mask = animation_layers::CellMask::new(self.area);
        let mut protected = HashSet::new();
        let mut carets = HashSet::new();
        let mut protect = |position, flags| {
            if mask.as_mut().is_some_and(|mask| mask.insert(position, flags)) {
                return;
            }
            protected.insert(position);
            if flags & animation_layers::CARET != 0 { carets.insert(position); }
        };
        for &position in self.protected.iter().chain(&self.obstacles) {
            protect(position, animation_layers::PROTECTED);
        }
        if let Some((area, _)) = self.primary_cursor {
            for x in area.x..area.right() {
                protect((x, area.y), animation_layers::PROTECTED | animation_layers::CARET);
            }
        }
        // Ordinary editor projections follow source order. A caret or a small
        // selection only needs its source lines, not every visible character.
        // Diff projections may repeat screen positions, so retain their scan.
        let anchors = if self.ordered && self.diff_projection.is_none() {
            let mut lines = None::<(usize, usize)>;
            let mut include = |line: usize| {
                lines = Some(lines.map_or((line, line), |(first, last)| (first.min(line), last.max(line))));
            };
            if self.primary_cursor.is_some() {
                if let Some(line) = cursor_line { include(line); }
            }
            if let Some((sl, _, el, _)) = selection { include(sl); include(el); }
            for &(line, _) in extra_cursors { include(line); }
            for &((start, _), (end, _)) in &merged { include(start); include(end); }
            lines.map_or(&self.anchors[..0], |(first, last)| {
                let start = self.anchors.partition_point(|anchor| anchor.line < first);
                let end = self.anchors.partition_point(|anchor| anchor.line <= last);
                &self.anchors[start..end]
            })
        } else {
            &self.anchors
        };
        for anchor in anchors {
            let position = (anchor.line, anchor.col);
            let caret = extra.contains(&position)
                || self.primary_cursor.is_some() && cursor == Some((anchor.x, anchor.y));
            let extra_selected = merged
                .get(merged.partition_point(|(_, end)| *end <= position))
                .is_some_and(|(start, end)| position >= *start && position < *end);
            if !caret && !selected(anchor, selection) && !extra_selected {
                continue;
            }
            for dx in 0..anchor.width {
                let cell = (anchor.x.saturating_add(dx), anchor.y);
                protect(cell, animation_layers::PROTECTED | if caret { animation_layers::CARET } else { 0 });
            }
        }
        if let Some(mask) = &mut mask {
            for &position in &self.overlays { mask.insert(position, animation_layers::EXCLUDED); }
            for &row in &self.unsafe_rows {
                for x in self.area.left()..self.area.right() {
                    mask.insert((x, row), animation_layers::EXCLUDED | animation_layers::BLOCKED_ROW);
                }
            }
        }
        let cached = Rc::new(OverlayProtection {
            cursor,
            selection,
            extra_cursors: extra_cursors.to_vec(),
            extra_cursor_anchors: extra_cursor_anchors.to_vec(),
            mask,
            protected,
            carets,
        });
        *self.protection.borrow_mut() = Some(Rc::clone(&cached));
        cached
    }
}

#[derive(Clone, Default)]
struct Layer {
    background: Option<(Color, f32)>,
    symbol: Option<(&'static str, Color, f32)>,
    scope_connections: u8,
    active_scope: bool,
}

const NORTH: u8 = 1;
const SOUTH: u8 = 2;
const EAST: u8 = 4;
const WEST: u8 = 8;

fn scope_connections(symbol: &str) -> u8 {
    match symbol {
        "╭" => SOUTH | EAST,
        "╮" => SOUTH | WEST,
        "╰" => NORTH | EAST,
        "╯" => NORTH | WEST,
        "─" => EAST | WEST,
        "│" => NORTH | SOUTH,
        "╷" => SOUTH,
        "╵" => NORTH,
        "╶" => EAST,
        "╴" => WEST,
        _ => 0,
    }
}

fn scope_symbol(connections: u8) -> &'static str {
    match connections {
        n if n == SOUTH | EAST => "╭",
        n if n == SOUTH | WEST => "╮",
        n if n == NORTH | EAST => "╰",
        n if n == NORTH | WEST => "╯",
        n if n == EAST | WEST => "─",
        SOUTH => "╷",
        NORTH => "╵",
        EAST => "╶",
        WEST => "╴",
        n if n == NORTH | SOUTH | EAST => "├",
        n if n == NORTH | SOUTH | WEST => "┤",
        n if n == SOUTH | EAST | WEST => "┬",
        n if n == NORTH | EAST | WEST => "┴",
        n if n == NORTH | SOUTH | EAST | WEST => "┼",
        _ => "│",
    }
}

fn scope_color_key(color: Color) -> u32 {
    match color {
        Color::Reset => 0,
        Color::Black => 1,
        Color::Red => 2,
        Color::Green => 3,
        Color::Yellow => 4,
        Color::Blue => 5,
        Color::Magenta => 6,
        Color::Cyan => 7,
        Color::Gray => 8,
        Color::DarkGray => 9,
        Color::LightRed => 10,
        Color::LightGreen => 11,
        Color::LightYellow => 12,
        Color::LightBlue => 13,
        Color::LightMagenta => 14,
        Color::LightCyan => 15,
        Color::White => 16,
        Color::Indexed(index) => 0x100 + u32::from(index),
        Color::Rgb(r, g, b) => {
            0x100_0000 + (u32::from(r) << 16) + (u32::from(g) << 8) + u32::from(b)
        }
    }
}

/// Reuse allocation capacity only. Values are cleared after each paint so
/// source edits, selection changes and theme changes cannot reuse old colors.
#[derive(Clone, Default)]
struct PaintScratch {
    layers: animation_layers::Layers,
    scope_colors: rustc_hash::FxHashMap<ScopePair, Color>,
    scope_endpoints: rustc_hash::FxHashMap<(usize, usize), Option<ScopeEndpointHint>>,
    scope_shapes: rustc_hash::FxHashMap<ScopePair, scope_paint_cache::SourceProjection>,
    contrast: rustc_hash::FxHashMap<(Color, Color, Color), Color>,
    fire_core: rustc_hash::FxHashSet<(u16, u16)>,
    pairs: Vec<ScopePair>,
    positions: Vec<(usize, usize)>,
}

struct Painter<'a> {
    buffer: &'a mut Buffer,
    area: Rect,
    clip: Rect,
    mask: Option<&'a animation_layers::CellMask>,
    colors: ThemeColors,
    protected: Cow<'a, HashSet<(u16, u16)>>,
    carets: Cow<'a, HashSet<(u16, u16)>>,
    blocked_rows: Cow<'a, HashSet<u16>>,
    overlays: Cow<'a, HashSet<(u16, u16)>>,
    layers: animation_layers::Layers,
    scope_colors: rustc_hash::FxHashMap<ScopePair, Color>,
    scope_endpoints: rustc_hash::FxHashMap<(usize, usize), Option<ScopeEndpointHint>>,
    scope_shapes: rustc_hash::FxHashMap<ScopePair, scope_paint_cache::SourceProjection>,
    contrast: rustc_hash::FxHashMap<(Color, Color, Color), Color>,
    fire_core: rustc_hash::FxHashSet<(u16, u16)>,
}

impl<'a> Painter<'a> {
    fn new(
        buffer: &'a mut Buffer,
        area: Rect,
        colors: ThemeColors,
        protected: HashSet<(u16, u16)>,
        carets: HashSet<(u16, u16)>,
        blocked_rows: HashSet<u16>,
    ) -> Self {
        Self {
            clip: area.intersection(buffer.area),
            mask: None,
            buffer,
            area,
            colors,
            protected: Cow::Owned(protected),
            carets: Cow::Owned(carets),
            blocked_rows: Cow::Owned(blocked_rows),
            overlays: Cow::Owned(HashSet::new()),
            layers: animation_layers::Layers::default(),
            scope_colors: rustc_hash::FxHashMap::default(),
            scope_endpoints: rustc_hash::FxHashMap::default(),
            scope_shapes: rustc_hash::FxHashMap::default(),
            contrast: rustc_hash::FxHashMap::default(),
            fire_core: rustc_hash::FxHashSet::default(),
        }
    }

    fn swap_scratch(&mut self, scratch: &mut PaintScratch) {
        std::mem::swap(&mut self.layers, &mut scratch.layers);
        std::mem::swap(&mut self.scope_colors, &mut scratch.scope_colors);
        std::mem::swap(&mut self.scope_endpoints, &mut scratch.scope_endpoints);
        std::mem::swap(&mut self.scope_shapes, &mut scratch.scope_shapes);
        std::mem::swap(&mut self.contrast, &mut scratch.contrast);
        std::mem::swap(&mut self.fire_core, &mut scratch.fire_core);
    }

    fn take_scratch(&mut self, scratch: &mut PaintScratch) {
        self.swap_scratch(scratch);
        self.layers.configure(self.clip);
    }

    fn recycle(&mut self, scratch: &mut PaintScratch) {
        self.layers.clear();
        self.scope_colors.clear();
        self.scope_endpoints.clear();
        self.scope_shapes.clear();
        self.contrast.clear();
        self.fire_core.clear();
        self.swap_scratch(scratch);
    }
    fn allowed(&self, x: u16, y: u16) -> bool {
        self.clip.contains((x, y).into())
            && self.mask.and_then(|mask| mask.get((x, y))).map_or_else(
                || !self.blocked_rows.contains(&y) && !self.overlays.contains(&(x, y)),
                |flags| flags & animation_layers::EXCLUDED == 0,
            )
            && (self.layers.len() < MAX_ANCHORS || self.layers.contains_key(&(x, y)))
    }
    fn is_protected(&self, position: (u16, u16)) -> bool {
        self.mask.and_then(|mask| mask.get(position)).map_or_else(|| self.protected.contains(&position),
            |flags| flags & animation_layers::PROTECTED != 0)
    }
    fn is_caret(&self, position: (u16, u16)) -> bool {
        self.mask.and_then(|mask| mask.get(position)).map_or_else(|| self.carets.contains(&position),
            |flags| flags & animation_layers::CARET != 0)
    }
    fn set_area(&mut self, area: Rect) {
        // Scope connectors briefly paint in the gutter outside the code
        // viewport. Keep clipping current and use sparse slots for that ink.
        if self.area != area {
            self.area = area;
            self.clip = area.intersection(self.buffer.area);
        }
    }
    fn peak(slot: &mut Option<(Color, f32)>, color: Color, amount: f32) {
        if amount.is_finite() && amount > 0.002 && slot.is_none_or(|(_, old)| amount > old) {
            *slot = Some((color, amount));
        }
    }
    #[cfg(test)]
    fn tint(&mut self, x: u16, y: u16, amount: f32) {
        self.tint_to(x, y, self.colors.fg_accent, amount);
    }
    fn tint_to(&mut self, x: u16, y: u16, color: Color, amount: f32) -> bool {
        if !amount.is_finite()
            || amount <= 0.002
            || !self.allowed(x, y)
            || self.is_protected((x, y))
        {
            return false;
        }
        Self::peak(
            &mut self.layers.get_or_insert((x, y)).background,
            color,
            amount.clamp(0.0, 0.60),
        );
        true
    }
    #[cfg(test)]
    fn symbol(&mut self, x: u16, y: u16, symbol: &'static str, amount: f32) {
        self.symbol_to(x, y, symbol, self.colors.fg_accent, amount);
    }
    fn symbol_to(&mut self, x: u16, y: u16, symbol: &'static str, color: Color, amount: f32) {
        if !amount.is_finite() || amount <= 0.015 || !self.decorative_cell(x, y, false) {
            return;
        }
        let layer = self.layers.get_or_insert((x, y));
        if layer
            .symbol
            .as_ref()
            .is_none_or(|(_, _, old)| amount > *old)
        {
            layer.symbol = Some((symbol, color, amount.clamp(0.0, 1.0)));
        }
    }
    /// Keep each scope contour intact when transitions overlap. The active
    /// bubble owns its cells; equally active paths use the stronger stroke,
    /// with stable shape and color ties independent of drawing order.
    /// Their layout has already excluded literal document glyphs.
    fn scope_symbol_to(
        &mut self,
        position: (u16, u16),
        symbol: &str,
        color: Color,
        amount: f32,
        active: bool,
    ) {
        if !amount.is_finite()
            || amount <= 0.015
            || !self.decorative_cell(position.0, position.1, true)
        {
            return;
        }
        let connections = scope_connections(symbol);
        if connections == 0 {
            return;
        }
        let amount = amount.clamp(0.0, 1.0);
        let layer = self.layers.get_or_insert(position);
        if let Some((_, old_color, old_amount)) = &layer.symbol
            && layer.scope_connections != 0
        {
            if layer.active_scope && !active {
                return;
            }
            if active == layer.active_scope
                && (amount < *old_amount
                    || amount == *old_amount
                        && (connections, scope_color_key(color))
                            <= (layer.scope_connections, scope_color_key(*old_color)))
            {
                return;
            }
        }
        layer.scope_connections = connections;
        layer.active_scope = active;
        layer.symbol = Some((scope_symbol(connections), color, amount));
    }
    fn decorative_cell(&self, x: u16, y: u16, replace_guide: bool) -> bool {
        if !self.allowed(x, y) || self.is_protected((x, y)) {
            return false;
        }
        for offset in 1..=2u16 {
            if let Some(previous_x) = x.checked_sub(offset)
                && self.buffer.cell((previous_x, y)).is_some_and(|c| {
                    unicode_width::UnicodeWidthStr::width(c.symbol()) > offset as usize
                })
            {
                return false;
            }
        }
        let cell = &self.buffer[(x, y)];
        let guide = replace_guide && matches!(cell.symbol(), "│" | "┊" | "┆" | "┃" | "·" | "→");
        (cell.symbol() == " " || guide) && !cell.skip
    }
    // Fractional coverage transfers light between adjacent terminal cells.
    #[cfg(test)]
    #[cfg(test)]
    fn spot(&mut self, x: f32, y: f32, amount: f32) {
        self.spot_to(x, y, self.colors.fg_accent, amount);
    }
    #[cfg(test)]
    fn spot_to(&mut self, x: f32, y: f32, color: Color, amount: f32) {
        if !x.is_finite() || !y.is_finite() {
            return;
        }
        let ix = x.floor();
        let iy = y.floor();
        let fx = x - ix;
        let fy = y - iy;
        for (dx, wx) in [(0, 1.0 - fx), (1, fx)] {
            for (dy, wy) in [(0, 1.0 - fy), (1, fy)] {
                let px = ix as i32 + dx;
                let py = iy as i32 + dy;
                if px >= 0 && py >= 0 && px <= u16::MAX as i32 && py <= u16::MAX as i32 {
                    self.tint_to(px as u16, py as u16, color, amount * wx * wy);
                }
            }
        }
    }
    fn finish(&mut self) {
        // Composite once against the ordinary rendered frame. Repeated events
        // never multiply brightness or progressively wash syntax colors out.
        // Adjacent fill/rail cells usually have identical inputs. Reuse their
        // exact colors before doing interpolation or a contrast-map lookup.
        let mut previous = None;
        #[cfg(test)]
        let reuse = !REFERENCE_PAINT.get();
        #[cfg(not(test))]
        let reuse = true;
        for (&position, layer) in self.layers.iter() {
            let Some(cell) = self.buffer.cell_mut(position) else {
                continue;
            };
            let original_fg = cell.fg;
            let original_bg = cell.bg;
            let key = (original_fg, original_bg,
                layer.background.map(|(color, amount)| (color, amount.to_bits())),
                layer.symbol.as_ref().map(|(_, color, amount)| (*color, amount.to_bits())));
            if reuse && let Some((old_key, fg, bg)) = previous && key == old_key {
                cell.set_fg(fg).set_bg(bg);
                if let Some((symbol, _, _)) = &layer.symbol { cell.set_symbol(symbol); }
                continue;
            }
            if let Some((color, amount)) = layer.background {
                cell.set_bg(lerp_color(cell.bg, color, amount));
            }
            if let Some((symbol, color, amount)) = &layer.symbol {
                let foreground = guide_blend(cell.bg, *color, *amount);
                cell.set_symbol(symbol).set_fg(foreground);
            } else if cell.bg != original_bg {
                let key = (original_fg, original_bg, cell.bg);
                let foreground = if reuse && let Some(&color) = self.contrast.get(&key) {
                    color
                } else {
                    let color = preserve_contrast(&self.colors, original_fg, original_bg, cell.bg);
                    // Gradients can generate a unique color at every cell;
                    // bound memory and hashing cost for those frames.
                    if reuse && self.contrast.len() < 256 { self.contrast.insert(key, color); }
                    color
                };
                cell.set_fg(foreground);
            }
            previous = Some((key, cell.fg, cell.bg));
        }
        self.layers.clear();
        self.contrast.clear();
    }
}

fn preserve_contrast(colors: &ThemeColors, foreground: Color, old_bg: Color, background: Color) -> Color {
    // Keep the source theme's contrast without a sudden jump to a palette
    // color when a faint highlight crosses a threshold.
    let floor = crate::ui_colors::contrast_ratio(foreground, old_bg)
        .unwrap_or(4.5).clamp(1.0, 4.5);
    if !crate::ui_colors::contrast_ratio(foreground, background).is_some_and(|r| r < floor) {
        return foreground;
    }
    let target = colors.ui.readable_foreground(foreground, background, floor);
    let mut low = 0.0;
    let mut high = 1.0;
    for _ in 0..7 {
        let middle = (low + high) * 0.5;
        let candidate = lerp_color(foreground, target, middle);
        if crate::ui_colors::contrast_ratio(candidate, background).is_some_and(|r| r >= floor) {
            high = middle;
        } else {
            low = middle;
        }
    }
    lerp_color(foreground, target, high)
}

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
fn linear_channel(channel: u8) -> f32 {
    crate::ui_colors::srgb_linear_channel(channel)
}

/// Grow contrast at the same rate on light and dark surfaces. Ordinary
/// linear-light opacity washes dark guides out on white; remapping that
/// opacity keeps them legible without a contrast floor that snaps during fades.
fn guide_blend(surface: Color, color: Color, amount: f32) -> Color {
    if amount >= 1.0 {
        return color;
    }
    if amount <= 0.0 {
        return surface;
    }
    let mut amount = amount.clamp(0.0, 1.0);
    if let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (surface, color) {
        let luminance = |r, g, b| {
            0.2126 * linear_channel(r) + 0.7152 * linear_channel(g) + 0.0722 * linear_channel(b)
        };
        let surface_light = luminance(ar, ag, ab);
        let ink_light = luminance(br, bg, bb);
        if surface_light > ink_light {
            let contrast = (surface_light + 0.05) / (ink_light + 0.05);
            amount = amount * contrast / (1.0 + amount * (contrast - 1.0));
        }
    }
    cursor_blend(surface, color, amount)
}

fn cursor_blend(surface: Color, color: Color, amount: f32) -> Color {
    if amount >= 1.0 {
        return color;
    }
    if amount <= 0.0 {
        return surface;
    }
    let amount = amount.clamp(0.0, 1.0);
    let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (surface, color) else {
        return lerp_color(surface, color, amount);
    };
    let channel = |a, b| {
        let value = linear_channel(a) * (1.0 - amount) + linear_channel(b) * amount;
        crate::ui_colors::srgb_from_linear(value)
    };
    Color::Rgb(channel(ar, br), channel(ag, bg), channel(ab, bb))
}

fn fire_color(heat: f32) -> Color {
    const COLORS: [Color; 5] = [
        Color::Rgb(90, 12, 4),
        Color::Rgb(205, 35, 5),
        Color::Rgb(255, 95, 12),
        Color::Rgb(255, 190, 48),
        Color::Rgb(255, 248, 200),
    ];
    let t = heat.clamp(0.0, 1.0) * (COLORS.len() - 1) as f32;
    let index = (t.floor() as usize).min(COLORS.len() - 1);
    lerp_color(
        COLORS[index],
        COLORS[(index + 1).min(COLORS.len() - 1)],
        t.fract(),
    )
}

fn wide_cells(geometry: &Geometry) -> &rustc_hash::FxHashSet<(u16, u16)> {
    geometry.wide_cells.get_or_init(|| {
        geometry
            .anchors
            .iter()
            .filter(|anchor| anchor.width > 1 && anchor.ch != '\t')
            .flat_map(|anchor| (0..anchor.width).map(move |dx| (anchor.x + dx, anchor.y)))
            .collect()
    })
}

fn draw_fire(
    painter: &mut Painter<'_>,
    geometry: &Geometry,
    particles: &[crate::cursor_fire::FireParticle],
    core: Option<crate::cursor_smear::SmearSample>,
) {
    let wide = wide_cells(geometry);
    // Keep the solid leading ribbon above the particle field. Flame glyphs
    // must never be mistaken for source text by the cursor compositor.
    let mut core_cells = std::mem::take(&mut painter.fire_core);
    core_cells.clear();
    if let Some(sample) = core {
        crate::cursor_smear_raster::rasterize_with(
            painter.area,
            sample.corners,
            sample.tail,
            sample.head,
            |pixel| {
                core_cells.insert((pixel.x, pixel.y));
            },
        );
    }
    let gradient = painter.colors.gradients.bg_editor;
    let deferred = geometry
        .deferred_gradient_area
        .filter(|_| gradient.start != gradient.end)
        .map(|area| {
            (
                area,
                crate::app_theme::GradientPrecomputed::new(
                    area.width,
                    area.height,
                    gradient.angle_deg,
                ),
            )
        });
    crate::fire_raster::rasterize_with(painter.area, particles, |pixel| {
        let position = (pixel.x, pixel.y);
        if !painter.allowed(pixel.x, pixel.y)
            || painter.is_caret(position)
            || core_cells.contains(&position)
        {
            return;
        }
        let protected = painter.is_protected(position);
        let cell = &mut painter.buffer[position];
        if cell.skip {
            return;
        }
        if let Some((area, projection)) = &deferred
            && cell.bg == painter.colors.bg_editor
            && area.contains(position.into())
        {
            cell.set_bg(lerp_color(
                gradient.start,
                gradient.end,
                projection.t_at(pixel.x - area.x, pixel.y - area.y),
            ));
        }
        let color = fire_color(pixel.heat);
        // Selections are a surface to blend over, rather than a hole in the
        // particle field. Keep their highlight visible beneath the same flame
        // silhouette; only carets and the solid cursor core block particles.
        let strength = if protected {
            0.6
        } else {
            1.0
        };
        let opacity = pixel.opacity.clamp(0.0, 0.96) * strength;
        let glow = pixel.glow * strength;
        if cell.symbol() == " " && !wide.contains(&position) {
            let surface = cell.bg;
            let ink = cursor_blend(surface, color, opacity);
            cell.set_char(pixel.symbol);
            if pixel.inverse {
                cell.set_fg(surface).set_bg(ink);
            } else {
                cell.set_fg(ink).set_bg(cursor_blend(surface, color, glow));
            }
        } else {
            let original_fg = cell.fg;
            let original_bg = cell.bg;
            let background =
                cursor_blend(original_bg, color, opacity * pixel.coverage * 0.72 + glow);
            let contrast = crate::ui_colors::contrast_ratio(original_fg, original_bg)
                .unwrap_or(4.5)
                .clamp(1.0, 4.5);
            cell.set_bg(background)
                .set_fg(
                    painter
                        .colors
                        .ui
                        .readable_foreground(original_fg, background, contrast),
                );
        }
    });
    core_cells.clear();
    painter.fire_core = core_cells;
}

fn restore_primary_cursor(buffer: &mut Buffer, geometry: &Geometry, colors: &ThemeColors) {
    let Some((area, style)) = geometry.primary_cursor else {
        return;
    };
    buffer.set_style(area, style);
    let gradient = if style.bg == Some(colors.bg_editor)
        && colors.gradients.bg_editor.start != colors.gradients.bg_editor.end
    {
        Some(colors.gradients.bg_editor)
    } else if style.bg == Some(colors.bg_line_highlight)
        && colors.gradients.bg_line_highlight.start != colors.gradients.bg_line_highlight.end
    {
        Some(colors.gradients.bg_line_highlight)
    } else {
        None
    };
    if let (Some(gradient), Some(layout)) = (gradient, geometry.gradient_area) {
        let projection = crate::app_theme::GradientPrecomputed::new(
            layout.width,
            layout.height,
            gradient.angle_deg,
        );
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                if let Some(cell) = buffer.cell_mut((x, y)) {
                    cell.set_bg(lerp_color(
                        gradient.start,
                        gradient.end,
                        projection.t_at(x - layout.x, y - layout.y),
                    ));
                }
            }
        }
    }
}

fn draw_smear(
    painter: &mut Painter<'_>,
    geometry: &Geometry,
    sample: crate::cursor_smear::SmearSample,
    color: Color,
    strength: f32,
    fire_heat: f32,
) {
    let tail = sample.tail;
    let head = sample.head;
    let extent = sample
        .corners
        .iter()
        .enumerate()
        .flat_map(|(index, a)| {
            sample.corners[index + 1..]
                .iter()
                .map(move |b| (a.x - b.x).powi(2) + ((a.y - b.y) * 2.0).powi(2))
        })
        .fold(0.0, f32::max)
        .sqrt();
    let stretched = smoothstep((extent - 5.0_f32.sqrt()) / 4.0);
    let wide_cells = wide_cells(geometry);
    // The large-file path receives its ordinary background gradient later in
    // the outer pane pass. Resolve only ribbon cells now: fractional and inverse
    // blocks need the actual surface beneath their uncovered portions.
    let gradient = painter.colors.gradients.bg_editor;
    let deferred_gradient = geometry
        .deferred_gradient_area
        .filter(|_| gradient.start != gradient.end)
        .map(|area| {
            (
                area,
                crate::app_theme::GradientPrecomputed::new(
                    area.width,
                    area.height,
                    gradient.angle_deg,
                ),
            )
        });
    let tail_opacity = 0.16 + strength * 0.64;
    crate::cursor_smear_raster::rasterize_with(painter.area, sample.corners, tail, head, |pixel| {
        let color = if fire_heat > 0.0 {
            lerp_color(color, fire_color(0.28 + 0.72 * pixel.shade), fire_heat)
        } else {
            color
        };
        let position = (pixel.x, pixel.y);
        if !painter.allowed(pixel.x, pixel.y) {
            return;
        }
        let primary = geometry
            .primary_cursor
            .is_some_and(|(area, _)| area.contains(position.into()));
        if painter.is_caret(position) && !primary {
            return;
        }
        // The leading edge remains a clear caret at any intensity. The setting
        // controls its tail, rather than making the editing position disappear.
        let shade = 1.0 - (1.0 - pixel.shade) * stretched;
        let mut opacity = tail_opacity + (1.0 - tail_opacity) * shade.powf(0.75);
        if painter.is_protected(position) && !primary {
            // Preserve selected text while letting the moving head remain legible.
            opacity *= 0.45 + pixel.shade * 0.55;
        }
        let cell = &mut painter.buffer[position];
        if cell.skip {
            return;
        }
        if let Some((area, projection)) = &deferred_gradient
            && cell.bg == painter.colors.bg_editor
            && area.contains(position.into())
        {
            cell.set_bg(lerp_color(
                gradient.start,
                gradient.end,
                projection.t_at(pixel.x - area.x, pixel.y - area.y),
            ));
        }
        if cell.symbol() == " " && !wide_cells.contains(&position) {
            let shade = cursor_blend(
                cell.bg,
                color,
                opacity * (pixel.coverage / pixel.glyph_coverage.max(0.001)).min(1.0),
            );
            let surface = cell.bg;
            cell.set_symbol(pixel.symbol);
            if pixel.inverse {
                cell.set_fg(surface).set_bg(shade);
            } else {
                cell.set_fg(shade);
            }
        } else {
            // Text is never replaced by a block glyph. Its background carries
            // the same fractional silhouette and its foreground follows contrast.
            let background = cursor_blend(cell.bg, color, opacity * pixel.coverage);
            let foreground =
                cursor_blend(cell.fg, painter.colors.fg_cursor, opacity * pixel.coverage);
            cell.set_bg(background).set_fg(
                painter
                    .colors
                    .ui
                    .readable_foreground(foreground, background, 4.5),
            );
        }
    });
}

#[path = "ui_animation_context.rs"]
mod context;

#[path = "ui_scope_paint_cache.rs"]
mod scope_paint_cache;
pub(crate) use scope_paint_cache::ScopePaintCache;

#[cfg(test)]
#[path = "ui_scope_efficiency_tests.rs"]
mod efficiency_tests;

#[cfg(test)]
#[path = "ui_scope_projection_tests.rs"]
mod projection_tests;

#[cfg(test)]
#[path = "ui_animation_responsiveness_tests.rs"]
mod responsiveness_tests;

#[path = "ui_whitespace_reveal.rs"]
mod ui_whitespace_reveal;

enum ScopeProjection<'a> {
    Editor(&'a crate::editor::Editor),
    Diff(&'a mut DiffScopePaintState, DiffScopeOptions<'a>),
}

struct OverlayOptions<'a> {
    snapshot: Snapshot,
    scope: Option<ScopePair>,
    projection: Option<ScopeProjection<'a>>,
    bracket_index: Option<&'a crate::background::BracketDepthIndex>,
    colorize: bool,
    extra_cursors: &'a [(usize, usize)],
    extra_cursor_anchors: &'a [Option<(usize, usize)>],
    allow_cursor_effects: bool,
    suppressed: bool,
}

/// Diff views supply final revision-specific geometry. Effects and their
/// lifecycle are shared with the editor, even before scope analysis is ready.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_diff(
    frame: &mut Frame,
    geometry: &Geometry,
    gutter: Rect,
    animations: &mut crate::animations::AnimationState,
    settings: &crate::animation_config::AnimationSettings,
    colors: ThemeColors,
    mut snapshot: Snapshot,
    allow_cursor_effects: bool,
    state: &mut DiffScopePaintState,
    options: Option<DiffScopeOptions<'_>>,
) {
    let mut scoped_settings;
    let settings = if options
        .as_ref()
        .is_some_and(|options| !settings.scope_enabled_for(options.source))
        && settings.enabled(Effect::ScopeConnector)
    {
        scoped_settings = settings.clone();
        scoped_settings.set(Effect::ScopeConnector, false);
        &scoped_settings
    } else {
        settings
    };
    let scope = options.as_ref().and_then(|options| {
        settings
            .enabled(Effect::ScopeConnector)
            .then(|| animations.diff_scope(options.source, options.index))
            .flatten()
    });
    let bracket_index = options.as_ref().and_then(|options| options.bracket_index);
    let colorize = options.as_ref().is_some_and(|options| options.colorize);
    if let Some(options) = &options {
        state.synchronize(options.source);
        snapshot.selection = options.selection.or(snapshot.selection);
    }
    draw_overlays_at(
        frame,
        geometry,
        gutter,
        animations,
        settings,
        colors,
        OverlayOptions {
            snapshot,
            scope,
            projection: options.map(|options| ScopeProjection::Diff(state, options)),
            bracket_index,
            colorize,
            extra_cursors: &[],
            extra_cursor_anchors: &[],
            allow_cursor_effects,
            suppressed: false,
        },
        Instant::now(),
    );
}

pub(crate) fn draw(frame: &mut Frame, app: &mut App, geometry: &Geometry, gutter: Rect) {
    draw_at(frame, app, geometry, gutter, Instant::now());
}

/// Multiple carets have no single motion or scope origin. Drop the old
/// snapshot as well as its effects so returning to one caret cannot replay
/// edits or motion from the multiple-cursor session.
pub(crate) fn suspend_for_multiple_cursors(app: &mut App) -> bool {
    let Some(id) = app
        .editor()
        .filter(|editor| !editor.extra_cursors.is_empty())
        .map(|editor| editor.navigation_id)
    else {
        return false;
    };
    app.animations.forget_view(id);
    if let Some(diff_id) = crate::diff_scope_source::animation_view_id(app)
        && diff_id != id
    {
        app.animations.forget_view(diff_id);
    }
    true
}

fn draw_at(frame: &mut Frame, app: &mut App, geometry: &Geometry, gutter: Rect, now: Instant) {
    if suspend_for_multiple_cursors(app)
        || !app.animation_settings.editor_effects_enabled()
        || !app.terminal_has_focus
        || app.rendering_inactive_editor
        || app.should_render_diff_for_active_editor()
        || geometry.area.is_empty()
    {
        return;
    }
    let Some(editor) = app.tabs.get(app.active_tab).map(|tab| &tab.editor) else {
        return;
    };
    let snapshot = Snapshot {
        id: editor.navigation_id,
        rope: editor.rope.clone(),
        cursor: (editor.cursor_line, editor.cursor_col),
        offset: editor
            .rope
            .line_to_char(editor.cursor_line)
            .saturating_add(editor.cursor_col)
            .min(editor.rope.len_chars()),
        content_version: editor.content_version,
        undo_revision: editor.animation_undo_revision,
        scroll: (
            editor.scroll_offset,
            editor.horizontal_scroll,
            editor.wrap_scroll_offset,
        ),
        selection: editor.selection_range(),
        area: geometry.area,
    };
    let scope_enabled = app.animation_settings.scope_enabled_for(editor);
    let mut scoped_settings;
    let settings = if !scope_enabled && app.animation_settings.enabled(Effect::ScopeConnector) {
        scoped_settings = app.animation_settings.clone();
        scoped_settings.set(Effect::ScopeConnector, false);
        &scoped_settings
    } else {
        &app.animation_settings
    };
    let scope = if scope_enabled {
        app.animations
            .scope(editor, app.cached_structural_scope_index.as_deref())
    } else {
        None
    };
    let pair = scope
        .or_else(|| {
            if !scope_enabled || editor.scope_bubble_rules.for_editor(editor).is_some() {
                // Explicit profiles use their lexer exclusively. Raw bracket/tag
                // matches can re-enable unselected pairs or pair string contents.
                return None;
            }
            // Most frames already have an indexed scope. Construct cursor
            // match keys and inspect fallback pairs only when they are needed.
            let matches_fresh = app
                .cached_cursor_match_target
                .as_ref()
                .zip(app.cursor_match_target().as_ref())
                .is_some_and(|(cached, current)| {
                    cached.same_render_anchor(current)
                        && cached.editor.content_version == editor.content_version
                        && cached.editor.len_bytes == editor.rope.len_bytes()
                });
            if !matches_fresh {
                return None;
            }
            app.cached_matching_bracket
                .filter(|other| {
                    // The lexical index is authoritative for curly scopes.
                    // Raw matching can also pair braces in comments/strings.
                    !app.cached_structural_scope_index
                        .as_ref()
                        .is_some_and(|index| index.matches(editor))
                        || !matches!(
                            editor
                                .rope
                                .get_line(other.0)
                                .and_then(|line| line.get_char(other.1)),
                            Some('{' | '}')
                        )
                })
                .map(|other| ((editor.cursor_line, editor.cursor_col), other))
                .or_else(|| {
                    app.cached_matching_tag
                        .map(|(sl, sc, _, el, ec, _)| ((sl, sc), (el, ec)))
                })
        })
        .filter(|_| scope_enabled);
    draw_overlays_at(
        frame,
        geometry,
        gutter,
        &mut app.animations,
        settings,
        app.colors,
        OverlayOptions {
            snapshot,
            scope: pair,
            projection: Some(ScopeProjection::Editor(editor)),
            bracket_index: app.cached_bracket_depth_index.as_deref(),
            colorize: app.bracket_pair_colorization,
            extra_cursors: &editor.extra_cursors,
            extra_cursor_anchors: &editor.extra_cursor_anchors,
            allow_cursor_effects: true,
            suppressed: app.status_details.open,
        },
        now,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_overlays_at(
    frame: &mut Frame,
    geometry: &Geometry,
    gutter: Rect,
    animations: &mut crate::animations::AnimationState,
    settings: &crate::animation_config::AnimationSettings,
    colors: ThemeColors,
    options: OverlayOptions<'_>,
    now: Instant,
) {
    if !geometry.enabled || geometry.area.is_empty() {
        return;
    }
    // A tool view can retain its source guides while focus moves to another
    // control. Retire cursor/typing motion without disturbing those guides.
    let mut focused_settings;
    let settings = if options.allow_cursor_effects {
        settings
    } else {
        focused_settings = settings.clone();
        for effect in [
            Effect::CursorSmear,
            Effect::CursorFire,
            Effect::WhitespaceReveal,
        ] {
            focused_settings.set(effect, false);
        }
        &focused_settings
    };
    let geometry = geometry.validated(frame.buffer_mut());
    let geometry = &geometry;
    let id = options.snapshot.id;
    let cursor = geometry.at(options.snapshot.cursor);
    let selection = options.snapshot.selection;
    let rope = options.snapshot.rope.clone();
    animations.observe_geometry(
        options.snapshot,
        &geometry.anchors,
        geometry.ordered,
        settings,
        now,
    );
    let pair = options.scope;
    animations.update_scope(id, pair, settings, now);
    // Status details suppress every overlay. Keep observing edits and scope
    // membership, but do not prepare masks, source shapes or particle samples
    // for a frame whose effect pixels cannot be displayed.
    if options.suppressed {
        return;
    }
    let events = animations.take_events(id);
    let fire_particles = animations.take_fire_particles(id, settings, now);
    if events.is_empty() && fire_particles.is_empty() && pair.is_none() {
        animations.restore_fire_particles(fire_particles);
        animations.restore_events(id, events);
        return;
    }
    let fire_heat = if settings.enabled(Effect::CursorFire) {
        events
            .iter()
            .rev()
            .find(|event| matches!(event.trigger, Trigger::Cursor { .. }))
            .map_or(0.0, |event| {
                (1.0 - smoothstep(
                    (event.effect_progress(settings, Effect::CursorFire, now) - 0.6) / 0.4,
                )) * settings.intensity(Effect::CursorFire).min(100) as f32
                    / 100.0
            })
    } else {
        0.0
    };
    let smear = ((settings.enabled(Effect::CursorSmear) || settings.enabled(Effect::CursorFire))
        && cursor.is_some()
        && geometry.primary_cursor.is_some())
    .then(|| {
        events
            .iter()
            .rev()
            .find_map(|event| event.cursor_sample(settings, Effect::CursorSmear, now))
            .or_else(|| animations.fire_core(id, settings, now))
    })
    .flatten();
    let smear_color = cursor
        .and_then(|cursor| {
            frame
                .buffer_mut()
                .cell((cursor.x, cursor.y))
                .map(|cell| cell.bg)
        })
        .unwrap_or(colors.bg_cursor);
    if smear.is_some() {
        restore_primary_cursor(frame.buffer_mut(), geometry, &colors);
    }

    let protection = geometry.protection(
        cursor,
        selection,
        options.extra_cursors,
        options.extra_cursor_anchors,
    );
    let mut painter = Painter::new(
        frame.buffer_mut(),
        geometry.area,
        colors,
        HashSet::new(),
        HashSet::new(),
        HashSet::new(),
    );
    painter.mask = protection.mask.as_ref();
    painter.protected = Cow::Borrowed(&protection.protected);
    painter.carets = Cow::Borrowed(&protection.carets);
    painter.blocked_rows = Cow::Borrowed(&geometry.unsafe_rows);
    painter.overlays = Cow::Borrowed(&geometry.overlays);
    let mut scratch = geometry.paint_scratch.borrow_mut();
    #[cfg(test)]
    let reuse_scratch = !REFERENCE_PAINT.get();
    #[cfg(not(test))]
    let reuse_scratch = true;
    if reuse_scratch { painter.take_scratch(&mut scratch); }
    let mut pairs = std::mem::take(&mut scratch.pairs);
    pairs.clear();
    pairs.extend(events
        .iter()
        .filter_map(|event| match event.trigger {
            Trigger::Scope { pair, .. } => Some(pair),
            _ => None,
        })
        .chain(pair));
    pairs.sort_unstable();
    pairs.dedup();
    let source = match &options.projection {
        Some(ScopeProjection::Editor(editor)) => Some(*editor),
        Some(ScopeProjection::Diff(_, options)) => Some(options.source),
        None => None,
    };
    // Keep semantic nesting depth while workers catch up with harmless edits.
    // Resolve it through the current theme on every draw, never a stale color.
    if options.colorize
        && let Some(editor) = source
    {
        for scope in &pairs {
            if painter.scope_colors.contains_key(scope) {
                continue;
            }
            if let Some(color) = animations
                .scope_bracket_depth(editor, *scope, options.bracket_index)
                .and_then(|depth| colors.ui.bracket_color(depth))
            {
                painter.scope_colors.insert(*scope, color);
            }
        }
    }
    let paint_state = animations.scope_paint.view(id);
    let mut positions = std::mem::take(&mut scratch.positions);
    positions.clear();
    positions.extend(pairs.iter().flat_map(|pair| [pair.0, pair.1]));
    positions.sort_unstable();
    positions.dedup();
    paint_state.retain_endpoints(&positions);
    match options.projection {
        Some(ScopeProjection::Editor(editor)) => {
            for scope in &pairs {
                if let Some(shape) =
                    paint_state.shape(editor, geometry, *scope, settings.scope_tight)
                {
                    painter.scope_shapes.insert(*scope, shape);
                }
                for position in [scope.0, scope.1] {
                    painter
                        .scope_endpoints
                        .entry(position)
                        .or_insert_with(|| paint_state.endpoint(editor, geometry, position));
                }
            }
        }
        Some(ScopeProjection::Diff(state, options)) => {
            for scope in &pairs {
                if let Some(shape) =
                    paint_state.shape(options.source, geometry, *scope, settings.scope_tight)
                {
                    painter.scope_shapes.insert(*scope, shape);
                }
            }
            state
                .endpoints
                .retain(|position, _| positions.binary_search(position).is_ok());
            state
                .lines
                .retain(|line, _| positions.iter().any(|position| position.0 == *line));
            for &position in &positions {
                painter
                    .scope_endpoints
                    .insert(position, state.endpoint(&options, geometry, position));
            }
        }
        None => {}
    }
    scratch.pairs = pairs;
    scratch.positions = positions;
    let whitespace_strength = settings.intensity(Effect::WhitespaceReveal).min(100) as f32 / 100.0;
    // Index source offsets only when a whitespace ribbon needs them. One Rope
    // lookup per source line is shared by every live insertion event.
    let has_insert = events
        .iter()
        .any(|event| matches!(event.trigger, Trigger::Insert { .. }));
    let indexed = (has_insert && settings.enabled(Effect::WhitespaceReveal))
        .then(|| geometry.source_offsets(&rope));
    for event in &events {
        let p = event.progress(settings, now);
        if p >= 1.0 {
            continue;
        }
        if let Trigger::Insert { start, end, paste } = &event.trigger
            && let Some(indexed) = indexed.as_ref()
        {
            let first = indexed.partition_point(|(offset, _)| offset < start);
            let last = indexed.partition_point(|(offset, _)| offset < end);
            if settings.enabled(Effect::WhitespaceReveal) {
                ui_whitespace_reveal::draw(
                    &mut painter,
                    &indexed[first..last],
                    p,
                    whitespace_strength,
                    *paste,
                );
            }
        }
    }
    context::draw_cached(
        &mut painter,
        geometry,
        settings,
        &events,
        pair,
        gutter,
        now,
        &mut paint_state.routes,
    );
    painter.finish();
    if let Some(smear) = smear {
        draw_smear(
            &mut painter,
            geometry,
            smear,
            smear_color,
            if events.iter().any(|event| {
                event
                    .cursor_sample(settings, Effect::CursorSmear, now)
                    .is_some()
            }) {
                settings.intensity(Effect::CursorSmear).min(100) as f32 / 100.0
            } else {
                settings.intensity(Effect::CursorFire).min(100) as f32 / 100.0
            },
            fire_heat,
        );
    }
    if !fire_particles.is_empty() {
        draw_fire(&mut painter, geometry, &fire_particles, smear);
    }
    if reuse_scratch { painter.recycle(&mut scratch); }
    animations.restore_fire_particles(fire_particles);
    animations.restore_events(id, events);
}

#[cfg(test)]
#[path = "ui_animation_mask_tests.rs"]
mod mask_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend, style::Style};

    #[test]
    fn unit_width_geometry_runs_match_single_glyphs_with_clipping_and_unsafe_rows() {
        let chars: Vec<_> = "éλЯ ordinary code with spaces + numbers_123".chars().collect();
        let anchors = |geometry: &Geometry| geometry.anchors.iter().map(|a|
            (a.line, a.col, a.ch, a.x, a.y, a.width)).collect::<Vec<_>>();
        for enabled in [false, true] {
            for width in [0, 1, 7, 60] {
                for column in [0, 1, 5, 60] {
                    for row in [0, 1, 2] {
                        for unsafe_row in [false, true] {
                            let area = Rect::new(3, 4, width, 2);
                            let mut actual = Geometry::new(area, enabled);
                            let mut expected = Geometry::new(area, enabled);
                            for geometry in [&mut actual, &mut expected] {
                                geometry.push(8, 2, 'x', 0, 0, 1);
                                if unsafe_row { geometry.push(3, 0, '\u{301}', row, 0, 0); }
                            }
                            actual.push_unit_width_run(3, 4, &chars, row, column);
                            for (offset, &ch) in chars.iter().enumerate() {
                                expected.push(3, 4 + offset, ch, row, column + offset, 1);
                            }
                            assert_eq!(anchors(&actual), anchors(&expected));
                            assert_eq!(actual.ordered, expected.ordered);
                            assert_eq!(actual.unsafe_rows, expected.unsafe_rows);
                        }
                    }
                }
            }
        }
        let mut actual = Geometry::new(Rect::new(0, 0, u16::MAX, 1), true);
        for col in 0..MAX_ANCHORS - 2 { actual.push(0, col, 'x', 0, col, 1); }
        actual.push_unit_width_run(0, MAX_ANCHORS - 2, &chars, 0, MAX_ANCHORS - 2);
        assert_eq!(actual.anchors.len(), MAX_ANCHORS);
        assert_eq!(actual.anchors.last().unwrap().ch, chars[1]);
    }

    #[test]
    fn validation_borrows_unchanged_geometry_and_filters_occluded_source() {
        let mut geometry = Geometry::new(Rect::new(0, 0, 8, 2), true);
        geometry.push(0, 0, 'a', 0, 0, 1);
        geometry.push(0, 1, 'b', 0, 1, 1);
        geometry.protect(0, 2, 1);
        let mut buffer = Buffer::empty(geometry.area);
        buffer[(0, 0)].set_char('a');
        buffer[(1, 0)].set_char('b');
        assert!(matches!(geometry.validated(&buffer), Cow::Borrowed(_)));
        buffer[(1, 0)].set_char('!');
        let validated = geometry.validated(&buffer);
        assert!(matches!(validated, Cow::Owned(_)));
        assert_eq!(validated.anchors.len(), 1);
        assert!(validated.protected.contains(&(2, 0)));
        assert_eq!(
            geometry.anchors.len(),
            2,
            "validation leaves retained source intact"
        );
    }

    #[test]
    fn anchor_lookup_handles_ordered_and_unordered_diff_projection() {
        for positions in [[(0, 2), (1, 0), (1, 4)], [(1, 4), (0, 2), (1, 0)]] {
            let mut geometry = Geometry::new(Rect::new(0, 0, 8, 3), true);
            for (row, &(line, col)) in positions.iter().enumerate() {
                geometry.push_diff(line, col, 'x', row, 0, 1);
            }
            for &(line, col) in &positions {
                assert_eq!(
                    geometry.at((line, col)).map(|a| (a.line, a.col)),
                    Some((line, col))
                );
            }
            assert!(geometry.at((1, 2)).is_none());
        }
    }

    #[test]
    fn shared_linear_channel_table_preserves_every_original_float() {
        for channel in 0..=255u8 {
            let value = channel as f32 / 255.0;
            let expected = if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            };
            assert_eq!(linear_channel(channel).to_bits(), expected.to_bits());
        }
    }

    #[test]
    fn linear_light_encoder_preserves_original_rounding_across_blends() {
        let original = |value: f32| {
            let srgb = if value <= 0.0031308 {
                value * 12.92
            } else {
                1.055 * value.powf(1.0 / 2.4) - 0.055
            };
            (srgb * 255.0).round().clamp(0.0, 255.0) as u8
        };
        for step in 0..=1_000_000 {
            let value = step as f32 / 1_000_000.0;
            assert_eq!(crate::ui_colors::srgb_from_linear(value), original(value));
        }
        for a in 0..=255u8 {
            for b in 0..=255u8 {
                for amount in [0.001, 0.01, 0.1, 0.25, 0.5, 0.75, 0.9, 0.99, 0.999] {
                    let value = linear_channel(a) * (1.0 - amount) + linear_channel(b) * amount;
                    assert_eq!(crate::ui_colors::srgb_from_linear(value), original(value));
                }
            }
        }
        for value in [f32::NAN, f32::NEG_INFINITY, f32::INFINITY, -1.0, 2.0] {
            assert_eq!(crate::ui_colors::srgb_from_linear(value), original(value));
        }
    }

    #[test]
    fn retained_geometry_reuses_projection_maps_and_pan_invalidates_them() {
        let text = "fn scope() {\n    body();\n}\n";
        let mut editor = crate::editor::Editor::test_with_text(text);
        let mut geometry = Geometry::new(Rect::new(4, 1, 80, 4), true);
        for (row, line) in text.lines().enumerate() {
            geometry.map_source_row(row, row);
            for (col, ch) in line.chars().enumerate() {
                geometry.push(row, col, ch, row, col, 1);
            }
        }
        let mut view = scope_paint_cache::View::default();
        let pair = ((0, 11), (2, 0));
        let first = view.shape(&editor, &geometry, pair, true).unwrap();
        let retained = view.shape(&editor, &geometry.clone(), pair, true).unwrap();
        assert!(std::sync::Arc::ptr_eq(&first.rows, &retained.rows));
        assert!(std::sync::Arc::ptr_eq(&first.origins, &retained.origins));
        editor.horizontal_scroll = 1;
        let panned = view.shape(&editor, &geometry, pair, true).unwrap();
        assert!(!std::sync::Arc::ptr_eq(&first.rows, &panned.rows));
    }

    #[test]
    fn unsupported_source_shapes_are_cached_until_the_source_changes() {
        let mut editor = crate::editor::Editor::test_with_text("{\n    e\u{301}\n}\n");
        let geometry = Geometry::new(Rect::new(4, 1, 80, 4), true);
        let pair = ((0, 0), (2, 0));
        let mut view = scope_paint_cache::View::default();
        for _ in 0..240 {
            assert!(view.shape(&editor, &geometry, pair, true).is_none());
        }
        assert_eq!(
            view.shape_builds, 1,
            "ambiguous Unicode must not be rescanned every animation frame"
        );
        editor.rope = ropey::Rope::from_str("{\n    e\n}\n");
        assert!(view.shape(&editor, &geometry, pair, true).is_some());
        assert_eq!(view.shape_builds, 2, "a changed source retries the contour");
    }

    #[test]
    fn branch_endpoint_attaches_after_the_previous_closer_without_guessing_string_braces() {
        let mut editor = crate::editor::Editor::test_with_text(
            "            cache.aligned_row_for_target_line(line);\n            } else {\n            return self.column_for_code_body();\n        }\n",
        );
        let geometry = Geometry::new(Rect::new(4, 1, 70, 4), true);
        let hint = source_scope_endpoint(&editor, &geometry, (1, 19)).unwrap();
        assert_eq!(hint.attachment, (1, 14));
        assert!(
            !hint.precise,
            "mixed branch attachment must not use a boxed precise route"
        );
        editor.rope = ropey::Rope::from_str("if label == \"}\" {");
        let hint =
            source_scope_endpoint(&editor, &geometry, (0, editor.rope.len_chars() - 1)).unwrap();
        assert_eq!(hint.attachment, (0, 0));
        editor.rope = ropey::Rope::from_str("if ok { other {");
        let position = (0, editor.rope.len_chars() - 1);
        let hint = source_scope_endpoint(&editor, &geometry, position).unwrap();
        assert_eq!(hint.attachment, position);
        assert!(
            hint.precise,
            "identical shared delimiters retain precise attachment"
        );
    }

    #[test]
    fn branch_attachment_ignores_later_quoted_and_commented_closing_braces() {
        for (text, expected) in [
            ("} else if label == \"}\" {", 2),
            ("} else /* } */ {", 2),
            ("}} else if label == \"}\" {", 3),
        ] {
            let editor = crate::editor::Editor::test_with_text(text);
            let position = (0, text.chars().count() - 1);
            let mut geometry = Geometry::new(Rect::new(4, 1, 50, 3), true);
            let normal = source_scope_endpoint(&editor, &geometry, position).unwrap();
            assert_eq!(normal.attachment, (0, expected), "{text}");
            let options = DiffScopeOptions {
                source: &editor,
                index: None,
                bracket_index: None,
                colorize: false,
                selection: None,
                cell_column: None,
                line_summary: None,
            };
            let mut state = DiffScopePaintState::default();
            geometry.set_diff_projection(0, false);
            let diff = state.endpoint(&options, &geometry, position).unwrap();
            assert_eq!(diff.attachment, (0, expected), "{text}");
            assert!(!normal.precise && !diff.precise);
        }
    }

    #[test]
    fn exact_delimiter_hints_match_normal_and_diff_character_pan_projection() {
        for text in [
            "    ) -> bool {",
            "    bool {",
            "    } else {",
            "界 ) -> bool {",
        ] {
            let mut editor = crate::editor::Editor::test_with_text(text);
            let position = (0, text.chars().count() - 1);
            let cache = crate::app::DiffRenderCache::new(&[(' ', text.to_owned())]);
            let column = |position: (usize, usize)| {
                (position.0 == 0).then(|| cache.rows[0].right_cell_column(position.1))
            };
            let summary = |line| (line == 0).then(|| cache.rows[0].right_scope_summary());
            for indexed in [false, true] {
                let mut state = DiffScopePaintState::default();
                let mut geometry = Geometry::new(Rect::new(20, 1, 30, 4), true);
                for pan in [0, 1, 5] {
                    editor.horizontal_scroll = pan;
                    let normal = source_scope_endpoint(&editor, &geometry, position).unwrap();
                    let options = DiffScopeOptions {
                        source: &editor,
                        index: None,
                        bracket_index: None,
                        colorize: false,
                        selection: None,
                        cell_column: if indexed { Some(&column) } else { None },
                        line_summary: if indexed { Some(&summary) } else { None },
                    };
                    geometry.set_diff_projection(pan, false);
                    let diff = state.endpoint(&options, &geometry, position).unwrap();
                    let prefix: String = text.chars().take(position.1).collect();
                    let origin: String = text.chars().take(pan).collect();
                    let expected =
                        geometry.area.x + u16::try_from(prefix.width() - origin.width()).unwrap();
                    assert_eq!(normal.delimiter_x, Some(expected), "{text}, pan={pan}");
                    assert_eq!(diff.delimiter_x, normal.delimiter_x, "{text}, pan={pan}");
                    assert_ne!(
                        diff.delimiter_x,
                        Some(diff.x),
                        "exact attachment stays separate from its fallback token"
                    );
                }
            }
        }
        let editor = crate::editor::Editor::test_with_text("    if ready");
        let geometry = Geometry::new(Rect::new(20, 1, 30, 4), true);
        let normal = source_scope_endpoint(&editor, &geometry, (0, 4)).unwrap();
        let options = DiffScopeOptions {
            source: &editor,
            index: None,
            bracket_index: None,
            colorize: false,
            selection: None,
            cell_column: None,
            line_summary: None,
        };
        let diff = DiffScopePaintState::default()
            .endpoint(&options, &geometry, (0, 4))
            .unwrap();
        assert!(normal.delimiter_x.is_none() && diff.delimiter_x.is_none());
    }

    #[test]
    fn diff_endpoint_metadata_reuses_unicode_cells_across_pan_and_resets_for_replaced_source() {
        let text = "\t界e\u{301} { {";
        let mut editor = crate::editor::Editor::test_with_text(text);
        let mut state = DiffScopePaintState::default();
        state.synchronize(&editor);
        let mut geometry = Geometry::new(Rect::new(20, 0, 30, 4), true);
        {
            let cache = crate::app::DiffRenderCache::new(&[(' ', text.to_owned())]);
            let column = |position: (usize, usize)| {
                (position.0 == 0).then(|| cache.rows[0].right_cell_column(position.1))
            };
            let options = DiffScopeOptions {
                source: &editor,
                index: None,
                bracket_index: None,
                colorize: false,
                selection: None,
                cell_column: Some(&column),
                line_summary: None,
            };
            geometry.set_diff_projection(0, false);
            assert_eq!(state.endpoint(&options, &geometry, (0, 7)).unwrap().x, 26);
            assert!(state.lines[&0].fallback.is_none());
            geometry.set_diff_projection(1, false);
            assert_eq!(state.endpoint(&options, &geometry, (0, 7)).unwrap().x, 26);
            geometry.set_diff_projection(1, true);
            assert_eq!(state.endpoint(&options, &geometry, (0, 7)).unwrap().x, 25);
            let options = DiffScopeOptions {
                cell_column: None,
                ..options
            };
            geometry.set_diff_projection(2, false);
            assert_eq!(state.endpoint(&options, &geometry, (0, 7)).unwrap().x, 24);
            let cached = state.lines[&0].fallback.as_ref().unwrap().text.as_ptr();
            geometry.set_diff_projection(4, false);
            state.endpoint(&options, &geometry, (0, 7)).unwrap();
            assert_eq!(
                state.lines[&0].fallback.as_ref().unwrap().text.as_ptr(),
                cached
            );
            assert_eq!(state.endpoints.len(), 1);
        }
        let version = editor.content_version;
        editor.rope = ropey::Rope::from_str("\t界e\u{301} [ [");
        state.synchronize(&editor);
        assert_eq!(editor.content_version, version);
        assert!(state.lines.is_empty() && state.endpoints.is_empty());
    }

    #[test]
    fn available_giant_endpoint_uses_worker_summary_without_cold_line_decode() {
        let prefix = 256 * 1024;
        let text = format!("{}{{ {{", " ".repeat(prefix));
        let editor = crate::editor::Editor::test_with_text(&text);
        let cache = crate::app::DiffRenderCache::new(&[(' ', text)]);
        let reads = std::cell::Cell::new(0);
        let summary = |line| {
            reads.set(reads.get() + 1);
            (line == 0).then(|| cache.rows[0].right_scope_summary())
        };
        let column = |position: (usize, usize)| {
            (position.0 == 0).then(|| cache.rows[0].right_cell_column(position.1))
        };
        let options = DiffScopeOptions {
            source: &editor,
            index: None,
            bracket_index: None,
            colorize: false,
            selection: None,
            cell_column: Some(&column),
            line_summary: Some(&summary),
        };
        let mut state = DiffScopePaintState::default();
        let mut geometry = Geometry::new(Rect::new(4, 1, 30, 4), true);
        geometry.set_diff_projection(prefix, false);
        let position = (0, prefix + 2);
        assert_eq!(state.endpoint(&options, &geometry, position).unwrap().x, 6);
        assert!(state.lines[&0].fallback.is_none());
        geometry.area.width = 40;
        geometry.set_diff_projection(prefix - 2, false);
        assert_eq!(state.endpoint(&options, &geometry, position).unwrap().x, 8);
        assert_eq!(
            reads.get(),
            1,
            "worker summary is installed once and reused across pan/resize"
        );
        assert!(state.lines[&0].fallback.is_none());
    }

    #[test]
    fn source_endpoint_hints_match_tab_unicode_and_character_scroll_projection() {
        let mut editor = crate::editor::Editor::test_with_text("界\t{ { {");
        let geometry = Geometry::new(Rect::new(20, 0, 30, 4), true);
        for (pan, expected) in [(0, 29), (1, 27), (2, 24)] {
            editor.horizontal_scroll = pan;
            let hint = source_scope_endpoint(&editor, &geometry, (0, 6)).unwrap();
            assert!(hint.precise);
            assert_eq!(hint.attachment, (0, 6));
            assert_eq!(hint.x, expected, "pan={pan}");
        }
        editor.rope = ropey::Rope::from_str("\tif ready {");
        editor.horizontal_scroll = 1;
        let hint = source_scope_endpoint(&editor, &geometry, (0, 10)).unwrap();
        assert_eq!(hint.attachment, (0, 1));
        assert_eq!(hint.x, geometry.area.x);
        assert!(!hint.precise);

        editor.rope = ropey::Rope::from_str("\t\t{ { {");
        editor.horizontal_scroll = 1;
        let hint = source_scope_endpoint(&editor, &geometry, (0, 6)).unwrap();
        assert_eq!(hint.x, geometry.area.x + 8);
        let mut bounded = geometry;
        bounded.set_bounded_source();
        let hint = source_scope_endpoint(&editor, &bounded, (0, 6)).unwrap();
        assert_eq!(hint.x, bounded.area.x + 8);
    }

    #[test]
    fn source_endpoint_hints_follow_exact_wrapped_segments_and_reject_ambiguous_rows() {
        let mut editor = crate::editor::Editor::test_with_text("界\t{ { {");
        let mut geometry = Geometry::new(Rect::new(20, 0, 30, 4), true);
        geometry.set_wrapped(4);
        let hint = source_scope_endpoint(&editor, &geometry, (0, 6)).unwrap();
        assert!(hint.precise);
        assert_eq!(hint.x, geometry.area.x + 2);
        editor.rope = ropey::Rope::from_str("e\u{301} {");
        assert!(source_scope_endpoint(&editor, &geometry, (0, 3)).is_none());
        editor.rope = ropey::Rope::from_str(&format!(
            "{}{{",
            "x".repeat(crate::background::MAX_BRACKET_LINE_CHARS)
        ));
        assert!(
            source_scope_endpoint(
                &editor,
                &geometry,
                (0, crate::background::MAX_BRACKET_LINE_CHARS)
            )
            .is_none()
        );
    }

    #[test]
    fn source_endpoint_hints_include_only_previews_on_the_rendered_segment() {
        let mut editor = crate::editor::Editor::test_with_text("#fff { { {");
        let mut geometry = Geometry::new(Rect::new(20, 0, 30, 4), true);
        let column = editor.rope.len_chars() - 1;
        let plain = source_scope_endpoint(&editor, &geometry, (0, column)).unwrap();
        geometry.set_color_preview(false);
        let preview = source_scope_endpoint(&editor, &geometry, (0, column)).unwrap();
        assert_eq!(preview.x, plain.x + 1);
        editor.horizontal_scroll = 5;
        let preview = source_scope_endpoint(&editor, &geometry, (0, column)).unwrap();
        geometry.color_preview = None;
        let plain = source_scope_endpoint(&editor, &geometry, (0, column)).unwrap();
        assert_eq!(
            preview.x, plain.x,
            "a clipped swatch does not consume a visible cell"
        );
        geometry.set_color_preview(false);
        geometry.set_wrapped(5);
        let preview = source_scope_endpoint(&editor, &geometry, (0, column)).unwrap();
        geometry.color_preview = None;
        let plain = source_scope_endpoint(&editor, &geometry, (0, column)).unwrap();
        assert_eq!(
            preview.x, plain.x,
            "a swatch on a prior wrapped row does not shift the endpoint"
        );
    }

    fn fixture(text: &str) -> App {
        let mut app = App::test_new();
        app.tabs.push(crate::app::Tab::new_file(
            crate::editor::Editor::test_with_text(text),
        ));
        app
    }

    fn animation_frame(app: &mut App, now: Instant) -> Buffer {
        animation_frame_with_height(app, now, 5)
    }

    fn animation_frame_with_height(app: &mut App, now: Instant, height: u16) -> Buffer {
        animation_frame_with_size(app, now, 30, height)
    }

    fn animation_frame_with_size(app: &mut App, now: Instant, width: u16, height: u16) -> Buffer {
        let area = Rect::new(2, 1, width - 6, height);
        let editor = &app.tabs[0].editor;
        let mut geometry = Geometry::new(area, true);
        for line in 0..editor.rope.len_lines().min(height as usize) {
            let mut column = 0;
            let text = editor.rope.line(line).to_string();
            let text = text.trim_end_matches(['\n', '\r']);
            for (col, ch) in text.chars().enumerate() {
                let width = if ch == '\t' {
                    editor.tab_size.max(1) - column % editor.tab_size.max(1)
                } else {
                    UnicodeWidthChar::width(ch).unwrap_or(0)
                };
                geometry.push(line, col, ch, line, column, width);
                column += width;
            }
            geometry.push(line, text.chars().count(), '\0', line, column, 1);
        }
        let cursor = (editor.cursor_line, editor.cursor_col);
        let selection = editor.selection_range();
        let colors = app.colors;
        let mut selection_style = crate::ui_colors::EditorSelectionStyle::new(&colors);
        if let Some(anchor) = geometry.at(cursor) {
            let style = Style::default().fg(colors.fg_text).bg(colors.bg_editor);
            let style = if selected(&anchor, selection) {
                selection_style.apply(style)
            } else {
                style
            };
            geometry.record_primary_cursor(
                (anchor.y - area.y) as usize,
                (anchor.x - area.x) as usize,
                anchor.width as usize,
                style,
            );
        }
        app.animations.begin_frame();
        app.animations.set_active_view(Some(editor.navigation_id));
        let mut terminal = Terminal::new(TestBackend::new(width, height + 3)).unwrap();
        terminal
            .draw(|frame| {
                frame.buffer_mut().set_style(
                    Rect::new(0, 0, width, height + 3),
                    Style::default().fg(colors.fg_text).bg(colors.bg_editor),
                );
                for anchor in &geometry.anchors {
                    if selected(anchor, selection) {
                        let style = selection_style
                            .apply(Style::default().fg(colors.fg_text).bg(colors.bg_editor));
                        for dx in 0..anchor.width {
                            frame.buffer_mut()[(anchor.x + dx, anchor.y)].set_style(style);
                        }
                    }
                    if !matches!(anchor.ch, '\t' | '\0') {
                        let mut text = [0u8; 4];
                        frame.buffer_mut()[(anchor.x, anchor.y)]
                            .set_symbol(anchor.ch.encode_utf8(&mut text));
                    }
                    if (anchor.line, anchor.col) == cursor
                        || app.tabs[0]
                            .editor
                            .extra_cursors
                            .contains(&(anchor.line, anchor.col))
                    {
                        frame.buffer_mut()[(anchor.x, anchor.y)].set_bg(colors.bg_cursor);
                    }
                }
                draw_at(frame, app, &geometry, Rect::new(0, 1, 2, height), now);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    #[ignore = "exports deterministic Fire Hazard frames for visual review"]
    fn export_fire_hazard_preview() {
        export_fire_hazard_frames(false);
    }

    #[test]
    #[ignore = "exports deterministic selection frames for visual review"]
    fn export_fire_hazard_selection_preview() {
        export_fire_hazard_frames(true);
    }

    fn export_fire_hazard_frames(selecting: bool) {
        fn rgb(color: Color) -> String {
            match color {
                Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
                _ => "#aaaaaa".into(),
            }
        }
        let source = concat!(
            "\n\n\n\n\n\n\n\n",
            "    require(\"smear_cursor\").setup({\n",
            "        cursor_color = \"#ff4000\",\n",
            "        particles_enabled = true,\n",
            "        particle_max_num = 200,\n",
            "        stiffness = 0.5,\n",
            "        trailing_stiffness = 0.2,\n",
            "        damping = 0.6,\n",
            "    })\n",
        );
        let mut app = fixture(source);
        app.animation_settings.set(Effect::CursorFire, true);
        app.tabs[0].editor.cursor_line = 8;
        app.tabs[0].editor.cursor_col = 4;
        let started = Instant::now();
        animation_frame_with_size(&mut app, started, 58, 18);
        let mut frames = Vec::new();
        for frame in 0..180 {
            if selecting && matches!(frame, 0 | 24 | 48 | 72 | 96 | 120) {
                let editor = &mut app.tabs[0].editor;
                editor.selection_anchor = Some((editor.cursor_line, editor.cursor_col));
            }
            match frame {
                0 => app.tabs[0].editor.cursor_col = 32,
                24 => app.tabs[0].editor.cursor_col = 4,
                48 => app.tabs[0].editor.cursor_col = 24,
                72 => {
                    app.tabs[0].editor.cursor_line = 10;
                    app.tabs[0].editor.cursor_col = 12;
                }
                96 => {
                    app.tabs[0].editor.cursor_line = 9;
                    app.tabs[0].editor.cursor_col = 4;
                }
                120 => {
                    app.tabs[0].editor.cursor_line = 8;
                    app.tabs[0].editor.cursor_col = 32;
                }
                _ => {}
            }
            let now = started + Duration::from_millis(frame * 16);
            let buffer = animation_frame_with_size(&mut app, now, 58, 18);
            let cells: Vec<_> = buffer
                .content
                .iter()
                .map(|cell| (cell.symbol().to_owned(), rgb(cell.fg), rgb(cell.bg)))
                .collect();
            frames.push(cells);
        }
        let path = if selecting {
            "target/fire-hazard-selection-preview.json"
        } else {
            "target/fire-hazard-preview.json"
        };
        std::fs::write(
            path,
            serde_json::to_vec(&serde_json::json!({
                "width": 58, "height": 21, "duration_ms": 16, "frames": frames,
            }))
            .unwrap(),
        )
        .unwrap();
    }
    #[test]
    fn inserted_tab_reveals_cells_in_sequence_then_restores_blank_text() {
        let mut app = fixture("");
        app.animation_settings.set(Effect::WhitespaceReveal, true);
        let now = Instant::now();
        animation_frame(&mut app, now);
        app.tabs[0].editor.insert_char('\t');
        let first = animation_frame(&mut app, now);
        assert_eq!(
            first[(2, 1)].symbol(),
            " ",
            "ignition starts without a first-frame flash"
        );
        let middle = animation_frame(&mut app, now + Duration::from_millis(100));
        assert!(
            middle.content.iter().any(|cell| matches!(
                cell.symbol(),
                "█" | "▁"
                    | "▂"
                    | "▃"
                    | "▄"
                    | "▅"
                    | "▆"
                    | "▇"
                    | "▏"
                    | "▎"
                    | "▍"
                    | "▌"
                    | "▋"
                    | "▊"
                    | "▉"
            )),
            "the inserted tab receives a filled fractional ribbon"
        );
        let finished = animation_frame(&mut app, now + Duration::from_secs(2));
        assert_eq!(finished[(2, 1)].symbol(), " ");
        assert_eq!(app.tabs[0].editor.rope.to_string(), "\t");
    }

    #[test]
    fn smear_moves_the_visible_caret_and_restores_the_exact_plain_frame() {
        let mut app = fixture(
            "start                  end\n                         \nfinish                  x",
        );
        app.animation_settings.set(Effect::CursorSmear, true);
        let now = Instant::now();
        animation_frame(&mut app, now);
        app.tabs[0].editor.cursor_line = 2;
        app.tabs[0].editor.cursor_col = 20;
        let source = app.tabs[0].editor.rope.to_string();
        let first = animation_frame(&mut app, now);
        assert_eq!(
            first[(2, 1)].bg,
            app.colors.bg_cursor,
            "the visible caret starts at its old position"
        );
        assert_ne!(
            first[(22, 3)].bg,
            app.colors.bg_cursor,
            "the destination must not show a duplicate caret"
        );
        let middle = animation_frame(&mut app, now + Duration::from_millis(50));
        assert!(
            middle.content.iter().any(|cell| matches!(
                cell.symbol(),
                "█" | "▏"
                    | "▎"
                    | "▍"
                    | "▌"
                    | "▋"
                    | "▊"
                    | "▉"
                    | "▁"
                    | "▂"
                    | "▃"
                    | "▄"
                    | "▅"
                    | "▆"
                    | "▇"
                    | "▘"
                    | "▝"
                    | "▖"
                    | "▗"
                    | "▞"
                    | "▚"
                    | "▙"
                    | "▛"
                    | "▜"
                    | "▟"
            )),
            "the moving body has filled fractional edges"
        );
        let finished = animation_frame(&mut app, now + Duration::from_secs(2));
        app.animation_settings.set(Effect::CursorSmear, false);
        assert_eq!(
            finished,
            animation_frame(&mut app, now + Duration::from_secs(2))
        );
        assert_eq!(app.tabs[0].editor.rope.to_string(), source);
        assert_eq!(
            (
                app.tabs[0].editor.cursor_line,
                app.tabs[0].editor.cursor_col
            ),
            (2, 20)
        );
    }

    #[test]
    fn fire_particles_keep_the_solid_cursor_core_above_the_flame_field() {
        use crate::{animations::MotionPoint, cursor_smear::SmearSample};
        let area = Rect::new(0, 0, 20, 10);
        let colors = crate::app_theme::EditorTheme::Dark.colors();
        let sample = SmearSample {
            corners: [
                MotionPoint { x: 4.0, y: 4.0 },
                MotionPoint { x: 8.0, y: 4.0 },
                MotionPoint { x: 8.0, y: 5.0 },
                MotionPoint { x: 4.0, y: 5.0 },
            ],
            velocities: [MotionPoint::default(); 4],
            head: MotionPoint { x: 8.0, y: 4.5 },
            tail: MotionPoint { x: 4.0, y: 4.5 },
            shade_direction: MotionPoint { x: 1.0, y: 0.0 },
        };
        let geometry = Geometry::new(area, true);
        let mut buffer = Buffer::empty(area);
        buffer.set_style(
            area,
            Style::default().bg(colors.bg_editor).fg(colors.fg_text),
        );
        let plain = buffer.clone();
        let mut painter = Painter::new(
            &mut buffer,
            area,
            colors,
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
        );
        draw_smear(&mut painter, &geometry, sample, colors.bg_cursor, 0.75, 1.0);
        let core = painter.buffer.clone();
        let flame = crate::cursor_fire::FireParticle {
            kind: crate::cursor_fire::FireParticleKind::Flame,
            position: MotionPoint { x: 6.5, y: 4.5 },
            velocity: MotionPoint { x: 0.0, y: -5.0 },
            heat: 1.0,
            opacity: 1.0,
            radius: 1.2,
            seed: 1,
        };
        let ember = crate::cursor_fire::FireParticle {
            kind: crate::cursor_fire::FireParticleKind::Ember,
            radius: 0.14,
            ..flame
        };
        draw_fire(
            &mut painter,
            &geometry,
            &[
                flame,
                ember,
                crate::cursor_fire::FireParticle {
                    position: MotionPoint { x: 9.25, y: 2.125 },
                    ..ember
                },
            ],
            Some(sample),
        );
        let core_pixels =
            crate::cursor_smear_raster::rasterize(area, sample.corners, sample.tail, sample.head);
        assert!(!core_pixels.is_empty());
        for pixel in core_pixels {
            assert_eq!(
                painter.buffer[(pixel.x, pixel.y)],
                core[(pixel.x, pixel.y)],
                "particles cannot punch dark glyphs through the hot ribbon"
            );
        }
        assert!(
            painter
                .buffer
                .content
                .iter()
                .zip(&core.content)
                .zip(&plain.content)
                .any(|((actual, ribbon), ordinary)| actual != ribbon && ribbon == ordinary),
            "the flame field still draws outside the core"
        );
    }

    #[test]
    fn fine_fire_sparks_preserve_source_unicode_selections_and_extra_carets() {
        use crate::{
            animations::MotionPoint,
            cursor_fire::{FireParticle, FireParticleKind},
        };
        for theme in [
            crate::app_theme::EditorTheme::Dark,
            crate::app_theme::EditorTheme::Light,
        ] {
            let area = Rect::new(0, 0, 20, 10);
            let colors = theme.colors();
            let mut geometry = Geometry::new(area, true);
            let mut buffer = Buffer::empty(area);
            buffer.set_style(
                area,
                Style::default().fg(colors.fg_text).bg(colors.bg_editor),
            );
            for (column, ch, width) in [(4, 'X', 1), (10, '界', 2), (14, '⣿', 1)] {
                geometry.push(0, column, ch, 4, column, width);
                buffer[(column as u16, 4)].set_char(ch);
            }
            let selection_style = crate::ui_colors::EditorSelectionStyle::new(&colors)
                .apply(Style::default().fg(colors.fg_text).bg(colors.bg_editor));
            for x in [4, 6, 10, 11, 14] {
                buffer[(x, 4)].set_style(selection_style);
            }
            buffer[(8, 4)].set_bg(colors.bg_cursor);
            let original = buffer.clone();
            let mut painter = Painter::new(
                &mut buffer,
                area,
                colors,
                HashSet::from([(4, 4), (6, 4), (8, 4), (10, 4), (11, 4), (14, 4)]),
                HashSet::from([(8, 4)]),
                HashSet::new(),
            );
            let particles: Vec<_> = [2, 4, 6, 8, 10, 11, 14]
                .into_iter()
                .map(|x| FireParticle {
                    kind: FireParticleKind::Ember,
                    position: MotionPoint {
                        x: x as f32 + 0.25,
                        y: 4.125,
                    },
                    velocity: MotionPoint::default(),
                    radius: 0.14,
                    heat: 0.9,
                    opacity: 1.0,
                    seed: x,
                })
                .collect();
            draw_fire(&mut painter, &geometry, &particles, None);
            assert_eq!(buffer[(2, 4)].symbol(), "⠁");
            assert_eq!(
                buffer[(6, 4)].symbol(),
                "⠁",
                "selection must not cut out sparks"
            );
            assert_ne!(
                buffer[(6, 4)].bg,
                colors.bg_editor,
                "the selection remains visible"
            );
            assert_eq!(
                buffer[(8, 4)],
                original[(8, 4)],
                "extra carets remain steady"
            );
            for x in [4, 10, 11, 14] {
                assert_eq!(buffer[(x, 4)].symbol(), original[(x, 4)].symbol());
            }
            for x in [4, 10, 14] {
                assert!(
                    crate::ui_colors::contrast_ratio(buffer[(x, 4)].fg, buffer[(x, 4)].bg)
                        .is_some_and(|ratio| ratio >= 4.5)
                );
            }
        }
    }

    #[test]
    fn fire_keeps_its_shape_across_forward_and_backward_selections() {
        let text = "                       \n    selected source    \n                       \n    target text        \n                       ";
        for theme in [
            crate::app_theme::EditorTheme::Dark,
            crate::app_theme::EditorTheme::Light,
        ] {
            let mut app = fixture(text);
            app.colors = theme.colors();
            app.animation_settings.set(Effect::CursorFire, true);
            app.tabs[0].editor.cursor_line = 3;
            let now = Instant::now();
            animation_frame(&mut app, now);
            for (step, cursor, anchor) in [(0, (3, 20), (0, 0)), (1, (1, 4), (3, 20))] {
                let started = now + Duration::from_millis(step * 500);
                app.tabs[0].editor.cursor_line = cursor.0;
                app.tabs[0].editor.cursor_col = cursor.1;
                app.tabs[0].editor.selection_anchor = Some(anchor);
                animation_frame(&mut app, started);
                let sampled = started + Duration::from_millis(150);
                let selected_frame = animation_frame(&mut app, sampled);
                app.tabs[0].editor.selection_anchor = None;
                let ordinary = animation_frame(&mut app, sampled);
                assert!(
                    ordinary.content.iter().any(|cell| {
                        cell.symbol()
                            .chars()
                            .any(|ch| ('\u{2801}'..='\u{28ff}').contains(&ch))
                    }),
                    "the comparison must include aged fire particles"
                );
                for (selected_cell, ordinary_cell) in
                    selected_frame.content.iter().zip(&ordinary.content)
                {
                    assert_eq!(
                        selected_cell.symbol(),
                        ordinary_cell.symbol(),
                        "selecting text must not cut holes in the flame (step {step})"
                    );
                }
                app.tabs[0].editor.selection_anchor = Some(anchor);
                assert_eq!(
                    selected_frame,
                    animation_frame(&mut app, sampled),
                    "selection changes do not restart fire"
                );
            }
            let finished = now + Duration::from_secs(4);
            let selected_plain = animation_frame(&mut app, finished);
            app.animation_settings.set(Effect::CursorFire, false);
            assert_eq!(selected_plain, animation_frame(&mut app, finished));
            assert_eq!(app.tabs[0].editor.rope.to_string(), text);
            assert_eq!(app.tabs[0].editor.selection_anchor, Some((3, 20)));
        }
    }
    #[test]
    fn deferred_pane_gradient_matches_composing_smear_after_the_gradient() {
        use crate::{animations::MotionPoint, cursor_smear::SmearSample};
        let pane = Rect::new(1, 1, 20, 10);
        let code = Rect::new(5, 4, 12, 4);
        let mut colors = crate::app_theme::EditorTheme::Dark.colors();
        colors.gradients.bg_editor = crate::app_theme::ThemeGradientSpec {
            start: Color::Rgb(10, 20, 120),
            end: Color::Rgb(220, 90, 15),
            angle_deg: 35.0,
        };
        let sample = SmearSample {
            corners: [
                MotionPoint { x: 7.0, y: 5.0 },
                MotionPoint { x: 10.0, y: 5.0 },
                MotionPoint { x: 10.0, y: 5.25 },
                MotionPoint { x: 7.0, y: 5.25 },
            ],
            velocities: [MotionPoint::default(); 4],
            head: MotionPoint { x: 10.0, y: 5.125 },
            tail: MotionPoint { x: 7.0, y: 5.125 },
            shade_direction: MotionPoint { x: 1.0, y: 0.0 },
        };
        // This models the existing outer pane pass, which replaces only cells
        // still carrying the ungraded editor background.
        let apply_outer_gradient = |buffer: &mut Buffer| {
            let gradient = colors.gradients.bg_editor;
            let projection = crate::app_theme::GradientPrecomputed::new(
                pane.width,
                pane.height,
                gradient.angle_deg,
            );
            for y in pane.top()..pane.bottom() {
                for x in pane.left()..pane.right() {
                    if buffer[(x, y)].bg == colors.bg_editor {
                        buffer[(x, y)].set_bg(lerp_color(
                            gradient.start,
                            gradient.end,
                            projection.t_at(x - pane.x, y - pane.y),
                        ));
                    }
                }
            }
        };
        let mut plain = Buffer::empty(pane);
        plain.set_style(
            pane,
            Style::default().fg(colors.fg_text).bg(colors.bg_editor),
        );
        plain[(8, 5)].set_symbol("x");
        let render = |buffer: &mut Buffer, geometry: &Geometry| {
            let mut painter = Painter::new(
                buffer,
                code,
                colors,
                HashSet::new(),
                HashSet::new(),
                HashSet::new(),
            );
            draw_smear(&mut painter, geometry, sample, colors.bg_cursor, 0.55, 0.0);
        };
        let mut expected = plain.clone();
        apply_outer_gradient(&mut expected);
        let uncovered_surface = expected[(7, 5)].bg;
        render(&mut expected, &Geometry::new(code, true));
        let mut geometry = Geometry::new(code, true);
        geometry.defer_gradients(Some(pane));
        let mut actual = plain.clone();
        render(&mut actual, &geometry);
        assert_eq!(
            actual[(7, 5)].symbol(),
            "▆",
            "upper quarter uses an inverse block"
        );
        assert_eq!(actual[(7, 5)].fg, uncovered_surface);
        assert_eq!(actual[(8, 5)].symbol(), "x", "source text is retained");
        assert_eq!(
            actual[(5, 4)],
            plain[(5, 4)],
            "only ribbon cells resolve early"
        );
        apply_outer_gradient(&mut actual);
        assert_eq!(
            actual, expected,
            "pane projection and fractional text tint agree"
        );

        geometry.defer_gradients(None);
        let mut standalone = plain.clone();
        render(&mut standalone, &geometry);
        assert_eq!(
            standalone[(7, 5)].fg,
            colors.bg_editor,
            "direct renderer calls have no outer gradient"
        );
    }

    #[test]
    fn smear_retarget_preserves_the_rendered_gradient_at_the_same_instant() {
        use crate::{animations::MotionPoint, cursor_smear::SmearMotion};
        let settings = crate::animation_config::AnimationSettings::default();
        let before = SmearMotion::retarget(
            MotionPoint { x: 2.0, y: 2.0 },
            MotionPoint { x: 28.0, y: 7.0 },
            None,
            false,
        )
        .sample(&settings, Duration::from_millis(32));
        let after = SmearMotion::retarget(
            MotionPoint::default(),
            MotionPoint { x: 1.0, y: 1.0 },
            Some(before),
            false,
        )
        .sample(&settings, Duration::ZERO);
        let colors = crate::app_theme::EditorTheme::Dark.colors();
        let geometry = Geometry::new(Rect::new(0, 0, 40, 16), true);
        let render = |sample| {
            let mut buffer = Buffer::empty(geometry.area);
            buffer.set_style(
                geometry.area,
                Style::default().bg(colors.bg_editor).fg(colors.fg_text),
            );
            let mut painter = Painter::new(
                &mut buffer,
                geometry.area,
                colors,
                HashSet::new(),
                HashSet::new(),
                HashSet::new(),
            );
            draw_smear(&mut painter, &geometry, sample, colors.bg_cursor, 0.55, 0.0);
            buffer
        };
        let before = render(before);
        let after = render(after);
        for (a, b) in before.content.iter().zip(&after.content) {
            assert_eq!(a.symbol(), b.symbol());
            for (a, b) in [(a.fg, b.fg), (a.bg, b.bg)] {
                if let (Color::Rgb(ar, ag, ab), Color::Rgb(br, bg, bb)) = (a, b) {
                    assert!(
                        ar.abs_diff(br) <= 1 && ag.abs_diff(bg) <= 1 && ab.abs_diff(bb) <= 1,
                        "retarget changed brightness: {a:?} -> {b:?}"
                    );
                } else {
                    assert_eq!(a, b);
                }
            }
        }
    }

    #[test]
    fn keyword_bubble_uses_gutter_rail_without_replacing_code() {
        let mut app = fixture("if ready\n    echo yes\nendif\n");
        app.tabs[0].editor.file_path = Some("fixture.vim".into());
        app.animation_settings.set(Effect::ScopeConnector, true);
        let now = Instant::now();
        animation_frame(&mut app, now);
        app.tabs[0].editor.cursor_line = 1;
        app.tabs[0].editor.cursor_col = 4;
        animation_frame(&mut app, now);
        let settled = animation_frame(&mut app, now + Duration::from_millis(300));
        assert_eq!(settled[(6, 2)].symbol(), "e");
        assert!(
            (0..=2).any(|x| settled[(x, 2)].symbol() == "│"),
            "the completed bubble has a visible gutter rail"
        );
        assert_eq!(
            app.tabs[0].editor.rope.to_string(),
            "if ready\n    echo yes\nendif\n"
        );
    }

    #[test]
    fn geometry_clips_tabs_wide_glyphs_sticky_and_complex_rows() {
        let mut geometry = Geometry::new(Rect::new(10, 4, 6, 3), true);
        geometry.push(7, 2, '\t', 0, 1, 4);
        geometry.push(7, 3, '界', 0, 5, 2);
        geometry.push(8, 0, 'x', 3, 0, 1);
        assert_eq!(geometry.anchors.len(), 1);
        assert_eq!((geometry.anchors[0].x, geometry.anchors[0].width), (11, 4));
        geometry.push(8, 0, 'a', 1, 0, 1);
        geometry.push(8, 1, '\u{301}', 1, 1, 0);
        geometry.push(8, 2, 'b', 1, 1, 1);
        assert_eq!(geometry.anchors.len(), 1);
        geometry.push(9, 0, 'z', 2, 0, 1);
        geometry.occlude_top(2);
        assert_eq!(geometry.area, Rect::new(10, 6, 6, 1));
        assert_eq!(geometry.anchors[0].line, 9);
        let mut disabled = Geometry::new(geometry.area, false);
        disabled.push(1, 0, 'x', 0, 0, 1);
        assert!(disabled.anchors.is_empty());
    }

    #[test]
    fn guide_fades_remain_gradual_on_light_and_dark_surfaces() {
        for theme in [
            crate::app_theme::EditorTheme::Dark,
            crate::app_theme::EditorTheme::Light,
        ] {
            let colors = theme.colors();
            let surface = colors.bg_editor;
            let ink = lerp_color(colors.fg_accent_blue, colors.fg_text, 0.20);
            assert_eq!(guide_blend(surface, ink, 0.0), surface);
            assert_eq!(guide_blend(surface, ink, 1.0), ink);
            let mut previous = 1.0;
            for frame in 1..=128 {
                let foreground = guide_blend(surface, ink, frame as f32 / 128.0);
                let contrast = crate::ui_colors::contrast_ratio(foreground, surface).unwrap();
                assert!(contrast >= previous, "fades must not reverse brightness");
                assert!(
                    contrast - previous < 0.20,
                    "fades must not snap to a contrast floor"
                );
                previous = contrast;
            }
        }
    }

    #[test]
    fn compositor_preserves_text_cursor_selection_wide_and_complex_cells() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 12, 4));
        let colors = crate::app_theme::EditorTheme::Dark.colors();
        buffer[(3, 1)].set_symbol("界");
        buffer[(5, 1)].set_bg(colors.bg_cursor).set_symbol("x");
        buffer[(6, 1)].set_bg(colors.bg_selection).set_symbol("y");
        let original = buffer.clone();
        let mut painter = Painter::new(
            &mut buffer,
            Rect::new(3, 1, 5, 2),
            colors,
            HashSet::from([(5, 1), (6, 1)]),
            HashSet::from([(5, 1)]),
            HashSet::from([2]),
        );
        for (x, y) in [(3, 1), (4, 1), (5, 1), (6, 1), (2, 1), (4, 2)] {
            painter.symbol(x, y, "·", 1.0);
        }
        for (x, y) in [(5, 1), (6, 1), (2, 1), (4, 2)] {
            painter.tint(x, y, 0.5);
        }
        painter.finish();
        assert_eq!(buffer, original);
    }

    #[test]
    fn repeated_effects_composite_once_instead_of_burning_out_syntax() {
        let colors = crate::app_theme::EditorTheme::Dark.colors();
        let render = |repeats| {
            let mut buffer = Buffer::empty(Rect::new(0, 0, 2, 1));
            buffer[(0, 0)]
                .set_bg(colors.bg_editor)
                .set_fg(colors.fg_text)
                .set_symbol("x");
            let mut painter = Painter::new(
                &mut buffer,
                Rect::new(0, 0, 2, 1),
                colors,
                HashSet::new(),
                HashSet::new(),
                HashSet::new(),
            );
            for _ in 0..repeats {
                painter.tint(0, 0, 0.4);
            }
            painter.finish();
            buffer
        };
        assert_eq!(render(1), render(40));
    }

    #[test]
    fn moving_subcell_head_transfers_energy_continuously() {
        let colors = crate::app_theme::EditorTheme::Dark.colors();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 2));
        let mut painter = Painter::new(
            &mut buffer,
            Rect::new(0, 0, 4, 2),
            colors,
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
        );
        painter.spot(1.25, 0.0, 0.5);
        assert!((painter.layers[&(1, 0)].background.unwrap().1 - 0.375).abs() < 0.001);
        assert!((painter.layers[&(2, 0)].background.unwrap().1 - 0.125).abs() < 0.001);
        assert_eq!(painter.layers.len(), 2);
    }

    #[test]
    fn retained_effects_keep_actual_text_and_restore_baseline_when_disabled() {
        let mut app = fixture("abcd");
        for effect in Effect::ALL {
            app.animation_settings.set(effect, true);
        }
        let now = Instant::now();
        animation_frame(&mut app, now);
        for i in 0..8 {
            app.tabs[0].editor.insert_char('x');
            animation_frame(&mut app, now + Duration::from_millis(i * 18));
        }
        let finished = animation_frame(&mut app, now + Duration::from_secs(2));
        let text = app.tabs[0].editor.rope.to_string();
        let cursor = app.tabs[0].editor.cursor_col;
        app.animation_settings = Default::default();
        assert_eq!(
            finished,
            animation_frame(&mut app, now + Duration::from_secs(2))
        );
        assert_eq!(app.tabs[0].editor.rope.to_string(), text);
        assert_eq!(app.tabs[0].editor.cursor_col, cursor);
    }

    #[test]
    #[ignore = "exports deterministic terminal frames for visual motion review"]
    fn export_animation_preview() {
        fn color(c: Color) -> String {
            match c {
                Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
                _ => "#b0b5c0".into(),
            }
        }
        let mut scenes = Vec::new();
        for (name, text, effects) in [
            (
                "Cursor · reversal",
                "let result = source;\n    process(result);\n    return value;",
                vec![Effect::CursorSmear],
            ),
            (
                "Cursor · open space",
                "                       \n                       \n                       ",
                vec![Effect::CursorSmear],
            ),
            (
                "Fire · hazard",
                "                       \n                       \n    let value = source; \n    process(value);    \n    return result;     ",
                vec![Effect::CursorFire],
            ),
            ("Tab · expanding cells", "", vec![Effect::WhitespaceReveal]),
            (
                "Scope · routed energy",
                "if ready\n    begin work\n    if nested\n        stage one\n        stage two\n    endif\n    finish work\nendif\n\noutside",
                vec![Effect::ScopeConnector],
            ),
        ] {
            let mut app = fixture(text);
            app.tabs[0].editor.file_path = Some("preview.vim".into());
            app.animation_settings.duration_ms = 420;
            app.animation_settings.intensity = 75;
            if name.starts_with("Fire") {
                app.tabs[0].editor.cursor_line = 3;
                app.tabs[0].editor.cursor_col = 2;
            }
            let height = if name.starts_with("Scope") { 10 } else { 5 };
            if name.starts_with("Scope") {
                app.tabs[0].editor.cursor_line = 9;
            }
            for effect in effects {
                app.animation_settings.set(effect, true);
            }
            let now = Instant::now();
            animation_frame_with_height(&mut app, now, height);
            let mut frames = Vec::new();
            let frame_count = if name.starts_with("Fire") || name.starts_with("Scope") {
                160
            } else {
                100
            };
            for i in 0..frame_count {
                if name.starts_with("Fire") {
                    match i {
                        5 => app.tabs[0].editor.cursor_col = 18,
                        13 => app.tabs[0].editor.cursor_col = 3,
                        21 => {
                            app.tabs[0].editor.cursor_line = 2;
                            app.tabs[0].editor.cursor_col = 12;
                        }
                        33 => {
                            app.tabs[0].editor.cursor_line = 4;
                            app.tabs[0].editor.cursor_col = 19;
                        }
                        45 => app.tabs[0].editor.cursor_col = 7,
                        57 => {
                            app.tabs[0].editor.cursor_line = 3;
                            app.tabs[0].editor.cursor_col = 2;
                        }
                        _ => {}
                    }
                } else if name.starts_with("Cursor") {
                    if i == 5 {
                        app.tabs[0].editor.cursor_col = 19;
                    }
                    if i == 9 {
                        app.tabs[0].editor.cursor_col = 5;
                    }
                    if i == 13 {
                        app.tabs[0].editor.cursor_line = 2;
                        app.tabs[0].editor.cursor_col = 12;
                    }
                } else if name.starts_with("Tab") {
                    if i == 5 {
                        app.tabs[0].editor.insert_char('\t');
                    }
                    if i == 17 {
                        app.tabs[0].editor.insert_char('\t');
                    }
                } else if name.starts_with("Scope") {
                    match i {
                        5 => {
                            app.tabs[0].editor.cursor_line = 1;
                            app.tabs[0].editor.cursor_col = 4;
                        }
                        25 => {
                            app.tabs[0].editor.cursor_line = 3;
                            app.tabs[0].editor.cursor_col = 8;
                        }
                        50 => {
                            app.tabs[0].editor.cursor_line = 6;
                            app.tabs[0].editor.cursor_col = 4;
                        }
                        80 => {
                            app.tabs[0].editor.cursor_line = 9;
                            app.tabs[0].editor.cursor_col = 0;
                        }
                        _ => {}
                    }
                }
                let buffer = animation_frame_with_height(
                    &mut app,
                    now + Duration::from_millis(i * 16),
                    height,
                );
                let cells: Vec<_> = buffer
                    .content
                    .iter()
                    .map(|cell| (cell.symbol().to_owned(), color(cell.fg), color(cell.bg)))
                    .collect();
                frames.push(cells);
            }
            scenes.push(
                serde_json::json!({"name":name,"width":30,"height":height + 3,"frames":frames}),
            );
        }
        let path = std::path::Path::new("target/animation-preview.json");
        std::fs::write(path, serde_json::to_vec(&scenes).unwrap()).unwrap();
    }

    #[test]
    fn fire_alone_draws_warm_core_and_cooling_embers_then_exactly_restores_plain_frame() {
        let text = "                       \n                       \n                       \n    source      value  \n                       ";
        let mut app = fixture(text);
        app.tabs[0].editor.cursor_line = 3;
        app.animation_settings.set(Effect::CursorFire, true);
        let now = Instant::now();
        animation_frame(&mut app, now);
        app.tabs[0].editor.cursor_col = 18;
        animation_frame(&mut app, now);
        let hot = animation_frame(&mut app, now + Duration::from_millis(70));
        assert!(hot.content.iter().any(|cell| matches!(cell.fg, Color::Rgb(r,g,b) if r > 160 && r > b.saturating_add(70) && g > b)));
        let cooling_time = now + Duration::from_millis(400);
        let cooling = animation_frame(&mut app, cooling_time);
        assert!(
            !app.animations
                .fire_particles(
                    app.tabs[0].editor.navigation_id,
                    &app.animation_settings,
                    cooling_time
                )
                .is_empty()
        );
        let finished_time = now + Duration::from_secs(3);
        let finished = animation_frame(&mut app, finished_time);
        assert_ne!(
            cooling, finished,
            "embers keep rendering after the cursor event ends"
        );
        assert_eq!(
            app.animations
                .next_frame_wait(&app.animation_settings, finished_time),
            None
        );
        app.animation_settings.set(Effect::CursorFire, false);
        assert_eq!(finished, animation_frame(&mut app, finished_time));
        assert_eq!(app.tabs[0].editor.rope.to_string(), text);
    }

    #[test]
    fn fire_and_smear_share_one_core_without_a_duplicate_caret() {
        let text = "                       \n                       \n                       \n                       ";
        let now = Instant::now();
        let capture = |with_smear| {
            let mut app = fixture(text);
            app.tabs[0].editor.cursor_line = 3;
            for effect in [Effect::CursorFire, Effect::CursorSmear] {
                app.animation_settings.set_duration(effect, 420);
            }
            app.animation_settings.set(Effect::CursorFire, true);
            app.animation_settings.set(Effect::CursorSmear, with_smear);
            animation_frame(&mut app, now);
            app.tabs[0].editor.cursor_col = 18;
            animation_frame(&mut app, now);
            animation_frame(&mut app, now + Duration::from_millis(60))
        };
        assert_eq!(capture(false), capture(true));
    }
}
