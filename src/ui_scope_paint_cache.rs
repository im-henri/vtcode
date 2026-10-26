//! Projection and routing caches belong to a view, rather than a paint frame.
//! Text revisions invalidate endpoint lines only when those lines change.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use super::{Geometry, ScopeEndpointHint, context, source_scope_endpoint};
use context::canonical::{Contour, Profile};
use ratatui::layout::Rect;
use unicode_width::UnicodeWidthChar;

const MAX_SOURCE_CHARS: usize = 32 * 1024;
const MAX_SOURCE_ROWS: usize = 1_024;

#[derive(Default)]
struct ShapeWork {
    #[cfg(test)]
    decoded_rows: usize,
}

#[derive(Clone)]
struct SourceRow {
    line: usize,
    start: usize,
    end: usize,
    columns: Arc<[i64]>,
    origin: i64,
}

struct SourceShape {
    rows: Vec<SourceRow>,
    profiles: Vec<Profile>,
}

impl SourceShape {
    fn row(&self, position: (usize, usize)) -> Option<usize> {
        let index = self
            .rows
            .partition_point(|row| (row.line, row.start) <= position)
            .checked_sub(1)?;
        (self.rows[index].line == position.0).then_some(index)
    }

    fn new(
        editor: &crate::editor::Editor,
        geometry: &Geometry,
        pair: super::ScopePair,
        previous: Option<&CachedShape>,
        work: &mut ShapeWork,
    ) -> Option<Self> {
        #[cfg(not(test))]
        let _ = &work;
        // Source-wide contour work is cosmetic and runs on input frames. Reject
        // oversized scopes from Rope metadata before decoding or allocating a
        // single row; the viewport route remains available for these scopes.
        let source_rows = pair.1.0.checked_sub(pair.0.0)?.checked_add(1)?;
        if source_rows > MAX_SOURCE_ROWS || pair.1.0 >= editor.rope.len_lines() {
            return None;
        }
        let first = editor.rope.line_to_char(pair.0.0);
        let end = pair.1.0.checked_add(1)?;
        let end = if end < editor.rope.len_lines() {
            editor.rope.line_to_char(end)
        } else {
            editor.rope.len_chars()
        };
        if end.checked_sub(first)? > MAX_SOURCE_CHARS {
            return None;
        }
        let mut rows = Vec::new();
        let mut profiles = Vec::new();
        // Seek once in each Rope, then walk their chunks together. Looking up
        // two lines and binary-searching cached rows per iteration makes even
        // unchanged large scopes unnecessarily expensive.
        let mut previous_lines = previous.map(|entry| entry.source.lines_at(pair.0.0));
        let mut previous_row = 0;
        for (offset, line) in editor.rope.lines_at(pair.0.0).take(source_rows).enumerate() {
            let line_index = pair.0.0 + offset;
            let old_line = previous_lines.as_mut().and_then(Iterator::next);
            let start = previous_row;
            if let Some(previous) = previous {
                while previous.shape.rows.get(previous_row).is_some_and(|row| row.line <= line_index) {
                    previous_row += 1;
                }
            }
            // Edits usually change one row, while a scope may span hundreds.
            // Compare Rope slices before decoding text or allocating columns;
            // this also handles undo and replacements without edit metadata.
            if let Some(previous) = previous
                && old_line.is_some_and(|old| old == line)
            {
                let end = previous_row;
                if start < end {
                    if rows.len() + end - start > MAX_SOURCE_ROWS { return None; }
                    rows.extend_from_slice(&previous.shape.rows[start..end]);
                    profiles.extend_from_slice(&previous.shape.profiles[start..end]);
                    continue;
                }
            }
            #[cfg(test)]
            {
                work.decoded_rows += 1;
            }
            let text = line.to_string();
            let text = text.trim_end_matches(['\r', '\n']);
            let chars: Vec<_> = text.chars().collect();
            if chars.iter().any(|&ch| {
                ch != '\t'
                    && UnicodeWidthChar::width(ch).unwrap_or(0) == 0
                    && !(geometry.diff_projection.is_some() && ch.is_control())
                    || ('\u{1f1e6}'..='\u{1f1ff}').contains(&ch)
            }) {
                return None;
            }
            let mut previews: Vec<_> = geometry
                .color_preview
                .map(|named| {
                    crate::ui_colors::find_inline_colors(text, named)
                        .into_iter()
                        .map(|entry| entry.0)
                        .collect()
                })
                .unwrap_or_default();
            previews.sort_unstable();
            previews.dedup();
            let mut visual = vec![0i64];
            let mut widths = Vec::with_capacity(chars.len());
            let mut tab_column = 0usize;
            for &ch in &chars {
                let width = if geometry.diff_projection.is_some() && ch.is_control() {
                    0
                } else if ch == '\t' {
                    editor.tab_size.max(1) - tab_column % editor.tab_size.max(1)
                } else {
                    UnicodeWidthChar::width(ch).unwrap_or(0)
                };
                tab_column += if ch == '\t' { width } else { 1 };
                widths.push(width as i64);
                visual.push(visual.last().copied().unwrap() + width as i64);
            }
            // Add preview cells in place; otherwise the same columns were
            // allocated and copied a second time for every decoded line.
            if !previews.is_empty() {
                let mut count = 0;
                for (col, x) in visual.iter_mut().enumerate() {
                    while previews.get(count).is_some_and(|&preview| preview <= col) { count += 1; }
                    *x += count as i64;
                }
            }
            let columns: Arc<[i64]> = visual.into();
            let ranges = geometry
                .wrap_width
                .map(|width| {
                    crate::editor::Editor::wrap_ranges_for_line_text_tabbed(
                        text,
                        width,
                        editor.tab_size,
                    )
                })
                .unwrap_or_else(|| vec![(0, chars.len())]);
            for (start, end) in ranges {
                if line_index == pair.1.0 && start > pair.1.1 {
                    break;
                }
                let from = if pair.0.0 == pair.1.0 {
                    start.max(pair.0.1)
                } else {
                    start
                };
                if from >= end && start != end {
                    continue;
                }
                if rows.len() == MAX_SOURCE_ROWS {
                    return None;
                }
                let limit = if line_index == pair.1.0 {
                    end.min(pair.1.1.saturating_add(1))
                } else {
                    end
                };
                let origin = columns[start] - i64::from(previews.binary_search(&start).is_ok());
                let source = (from..limit)
                    .filter(|&col| !matches!(chars[col], ' ' | '\t') && widths[col] > 0);
                let left = source.clone().map(|col| columns[col] - origin).min();
                let right = source
                    .map(|col| columns[col] - origin + widths[col] - 1)
                    .max();
                let mut gaps: Vec<(i64, i64)> = Vec::new();
                if line_index == pair.0.0 || line_index == pair.1.0 {
                    for col in from..limit {
                        if matches!(chars[col], ' ' | '\t') && widths[col] > 0 {
                            let x = columns[col] - origin;
                            if left.zip(right).is_some_and(|(left, right)| {
                                x > left && x + widths[col] - 1 < right
                            }) {
                                if let Some((_, last)) =
                                    gaps.last_mut().filter(|(_, last)| *last + 1 == x)
                                {
                                    *last = x + widths[col] - 1;
                                } else {
                                    gaps.push((x, x + widths[col] - 1));
                                }
                            }
                        }
                    }
                }
                rows.push(SourceRow {
                    line: line_index,
                    start,
                    end,
                    columns: Arc::clone(&columns),
                    origin,
                });
                profiles.push(Profile { left, right, gaps });
            }
        }
        let last = profiles.len().saturating_sub(1);
        for (index, profile) in profiles.iter_mut().enumerate() {
            if index != 0 && index != last {
                profile.gaps.clear();
            }
        }
        Some(Self { rows, profiles })
    }
}

struct CachedShape {
    key: u64,
    pair: super::ScopePair,
    source: ropey::Rope,
    wrap: Option<usize>,
    bounded: bool,
    diff: bool,
    preview: Option<bool>,
    tab_size: usize,
    tight: bool,
    shape: Arc<SourceShape>,
    contour: Arc<Contour>,
    projection: Option<(ProjectionKey, SourceProjection)>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ProjectionKey {
    geometry: u64,
    area: Rect,
    pan: usize,
    diff: Option<(usize, bool)>,
    anchors: usize,
    rows: usize,
}

#[derive(Clone)]
pub(super) struct SourceProjection {
    pub(super) key: u64,
    pub(super) contour: Arc<Contour>,
    pub(super) rows: Arc<BTreeMap<usize, (i64, u16)>>,
    pub(super) origins: Arc<BTreeMap<usize, i64>>,
    source: Arc<SourceShape>,
}

impl SourceProjection {
    pub(super) fn origin_cost(&self, position: (usize, usize)) -> f32 {
        let row = self.source.row(position).unwrap_or(0);
        let source = &self.source.rows[row];
        let x = source.columns[position.1.min(source.columns.len() - 1)] - source.origin;
        self.contour.origin_cost(row, x)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Projection {
    x: u16,
    wrap_width: Option<usize>,
    bounded: bool,
    preview: Option<bool>,
    pan: usize,
    tab_size: usize,
}

struct CachedEndpoint {
    position: (usize, usize),
    source: ropey::Rope,
    projection: Projection,
    hint: Option<ScopeEndpointHint>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ShapeMode {
    wrap: Option<usize>,
    bounded: bool,
    diff: bool,
    preview: Option<bool>,
    tab_size: usize,
    tight: bool,
}

struct FailedShape {
    pair: super::ScopePair,
    source: ropey::Rope,
    mode: ShapeMode,
}

#[derive(Default)]
pub(super) struct View {
    shape_clock: u64,
    endpoints: Vec<CachedEndpoint>,
    shapes: Vec<CachedShape>,
    failed_shapes: Vec<FailedShape>,
    shape_work: ShapeWork,
    pub(super) routes: context::RouteCache,
    seen: u64,
    #[cfg(test)]
    endpoint_builds: usize,
    #[cfg(test)]
    pub(super) shape_builds: usize,
}

impl View {
    #[cfg(test)]
    pub(super) fn decoded_rows(&self) -> usize {
        self.shape_work.decoded_rows
    }

    pub(super) fn shape(
        &mut self,
        editor: &crate::editor::Editor,
        geometry: &Geometry,
        pair: super::ScopePair,
        tight: bool,
    ) -> Option<SourceProjection> {
        let pair = (pair.0.min(pair.1), pair.0.max(pair.1));
        let mode = ShapeMode {
            wrap: geometry.wrap_width,
            bounded: geometry.bounded_source,
            diff: geometry.diff_projection.is_some(),
            preview: geometry.color_preview,
            tab_size: editor.tab_size,
            tight,
        };
        if self.failed_shapes.iter().any(|failed| {
            failed.pair == pair && failed.mode == mode && failed.source.is_instance(&editor.rope)
        }) {
            return None;
        }
        let existing = self.shapes.iter().position(|entry| {
            entry.pair == pair
                && entry.source.is_instance(&editor.rope)
                && entry.wrap == geometry.wrap_width
                && entry.bounded == geometry.bounded_source
                && entry.diff == geometry.diff_projection.is_some()
                && entry.preview == geometry.color_preview
                && entry.tab_size == editor.tab_size
                && entry.tight == tight
        });
        let index = if let Some(index) = existing {
            index
        } else {
            #[cfg(test)]
            {
                self.shape_builds += 1;
            }
            let previous = self.shapes.iter().find(|entry| {
                entry.pair == pair && entry.wrap == mode.wrap && entry.bounded == mode.bounded
                    && entry.diff == mode.diff && entry.preview == mode.preview
                    && entry.tab_size == mode.tab_size && entry.tight == mode.tight
            });
            let Some(shape) = SourceShape::new(editor, geometry, pair, previous, &mut self.shape_work) else {
                self.remember_failed_shape(editor, pair, mode);
                return None;
            };
            let shape = Arc::new(shape);
            let (contour, key) = if let Some(previous) = self.shapes.iter().find(|entry| {
                entry.pair == pair
                    && entry.wrap == geometry.wrap_width
                    && entry.bounded == geometry.bounded_source
                    && entry.diff == geometry.diff_projection.is_some()
                    && entry.preview == geometry.color_preview
                    && entry.tab_size == editor.tab_size
                    && entry.tight == tight
                    && entry.shape.profiles.len() == shape.profiles.len()
                    && entry
                        .shape
                        .profiles
                        .iter()
                        .zip(&shape.profiles)
                        .all(|(old, new)| {
                            old.left == new.left && old.right == new.right && old.gaps == new.gaps
                        })
            }) {
                (Arc::clone(&previous.contour), previous.key)
            } else {
                self.shape_clock = self.shape_clock.wrapping_add(1);
                let Some(contour) = Contour::new(&shape.profiles, tight) else {
                    self.remember_failed_shape(editor, pair, mode);
                    return None;
                };
                (Arc::new(contour), self.shape_clock)
            };
            self.shapes.retain(|entry| entry.pair != pair);
            if self.shapes.len() >= 6 {
                self.shapes.remove(0);
            }
            self.shapes.push(CachedShape {
                key,
                pair,
                source: editor.rope.clone(),
                wrap: geometry.wrap_width,
                bounded: geometry.bounded_source,
                diff: geometry.diff_projection.is_some(),
                preview: geometry.color_preview,
                tab_size: editor.tab_size,
                tight,
                shape,
                contour,
                projection: None,
            });
            self.shapes.len() - 1
        };
        let entry = &mut self.shapes[index];
        let projection_key = ProjectionKey {
            geometry: geometry.projection_id,
            area: geometry.area,
            pan: editor.horizontal_scroll,
            diff: geometry.diff_projection,
            anchors: geometry.anchors.len(),
            rows: geometry.scope_rows.len(),
        };
        if let Some((key, projection)) = &entry.projection
            && *key == projection_key
        {
            return Some(projection.clone());
        }
        let mut projected = BTreeMap::new();
        let row_origin = |row: &SourceRow| {
            let column = |pan: usize| {
                let end = row.columns.len() - 1;
                row.columns[pan.min(end)] + pan.saturating_sub(end) as i64
            };
            let offset = if geometry.wrap_width.is_some() {
                row.origin
            } else if let Some((pan, inline)) = geometry.diff_projection {
                if inline { pan as i64 } else { column(pan) }
            } else {
                column(editor.horizontal_scroll)
            };
            i64::from(geometry.area.x) - (offset - row.origin)
        };
        let mut project = |line, start, y| {
            let Some(index) = entry.shape.row((line, start)) else {
                return;
            };
            let row = &entry.shape.rows[index];
            // Later wrapped segments of the same source line are outside the
            // scope. They must not replace its last projected row.
            if start >= row.end && start != row.start {
                return;
            }
            let anchor = geometry.line_anchors(line).iter().find(|a| {
                a.y == y
                    && a.line == line
                    && a.col >= row.start
                    && a.col <= row.end
                    && a.col < row.columns.len()
            });
            let x = if let Some(anchor) = anchor {
                i64::from(anchor.x) - (row.columns[anchor.col] - row.origin)
            } else {
                row_origin(row)
            };
            projected.insert(index, (x, y));
        };
        if geometry.scope_rows.is_empty() {
            let mut seen = std::collections::HashSet::new();
            for anchor in &geometry.anchors {
                if seen.insert(anchor.y) {
                    project(anchor.line, anchor.col, anchor.y);
                }
            }
        } else {
            for &(line, start, y) in &geometry.scope_rows {
                project(line, start, y);
            }
        }
        // Neighbors outside the viewport still determine the same corner.
        // Character-based panning can shift tabbed rows by different cell
        // widths, so retain their source origins even while their ink is clipped.
        let mut origins = BTreeMap::new();
        for (&index, &(x, _)) in &projected {
            origins.insert(index, x);
            for neighbor in [index.checked_sub(1), index.checked_add(1)]
                .into_iter()
                .flatten()
            {
                if let Some(row) = entry.shape.rows.get(neighbor) {
                    origins.entry(neighbor).or_insert_with(|| row_origin(row));
                }
            }
        }
        let projection = SourceProjection {
            key: entry.key,
            contour: Arc::clone(&entry.contour),
            rows: Arc::new(projected),
            origins: Arc::new(origins),
            source: Arc::clone(&entry.shape),
        };
        entry.projection = Some((projection_key, projection.clone()));
        Some(projection)
    }
    fn remember_failed_shape(
        &mut self,
        editor: &crate::editor::Editor,
        pair: super::ScopePair,
        mode: ShapeMode,
    ) {
        self.failed_shapes
            .retain(|failed| failed.pair != pair || failed.mode != mode);
        if self.failed_shapes.len() >= 6 {
            self.failed_shapes.remove(0);
        }
        self.failed_shapes.push(FailedShape {
            pair,
            source: editor.rope.clone(),
            mode,
        });
    }
    pub(super) fn retain_endpoints(&mut self, positions: &[(usize, usize)]) {
        self.endpoints
            .retain(|entry| positions.contains(&entry.position));
    }

    pub(super) fn endpoint(
        &mut self,
        editor: &crate::editor::Editor,
        geometry: &Geometry,
        position: (usize, usize),
    ) -> Option<ScopeEndpointHint> {
        let projection = Projection {
            x: geometry.area.x,
            wrap_width: geometry.wrap_width,
            bounded: geometry.bounded_source,
            preview: geometry.color_preview,
            pan: editor.horizontal_scroll,
            tab_size: editor.tab_size,
        };
        if let Some(entry) = self
            .endpoints
            .iter_mut()
            .find(|entry| entry.position == position)
            && entry.projection == projection
            && (entry.source.is_instance(&editor.rope)
                || entry
                    .source
                    .get_line(position.0)
                    .zip(editor.rope.get_line(position.0))
                    .is_some_and(|(old, new)| {
                        old.len_chars() <= crate::background::MAX_BRACKET_LINE_CHARS
                            && new.len_chars() <= crate::background::MAX_BRACKET_LINE_CHARS
                            && old == new
                    }))
        {
            entry.source = editor.rope.clone();
            return entry.hint;
        }
        #[cfg(test)]
        {
            self.endpoint_builds += 1;
        }
        let hint = source_scope_endpoint(editor, geometry, position);
        self.endpoints.retain(|entry| entry.position != position);
        self.endpoints.push(CachedEndpoint {
            position,
            source: editor.rope.clone(),
            projection,
            hint,
        });
        hint
    }
}

#[cfg(test)]
#[path = "ui_scope_layout_tests.rs"]
mod source_budget_tests;

#[derive(Default)]
pub(crate) struct ScopePaintCache {
    views: HashMap<u64, View>,
    clock: u64,
}

impl ScopePaintCache {
    pub(super) fn view(&mut self, id: u64) -> &mut View {
        const MAX_VIEWS: usize = 16;
        self.clock = self.clock.wrapping_add(1);
        if self.views.len() >= MAX_VIEWS
            && !self.views.contains_key(&id)
            && let Some(oldest) = self
                .views
                .iter()
                .min_by_key(|(_, view)| view.seen)
                .map(|(&id, _)| id)
        {
            self.views.remove(&oldest);
        }
        let view = self.views.entry(id).or_default();
        view.seen = self.clock;
        view
    }

    pub(crate) fn clear(&mut self) {
        self.views.clear();
    }

    pub(crate) fn remove(&mut self, id: u64) {
        self.views.remove(&id);
    }

    #[cfg(test)]
    pub(crate) fn stats(&self, id: u64) -> (usize, usize) {
        self.views
            .get(&id)
            .map_or((0, 0), |view| (view.routes.builds, view.endpoint_builds))
    }
}
