//! Cache paths by their projected attachments and actual free cells. Changes
//! to text elsewhere, colors, selections and carets do not restart routing.

use super::*;
use std::sync::Arc;

#[derive(PartialEq, Eq)]
enum Hint {
    Missing,
    Unsupported,
    Projected(u16, bool, Option<u16>),
}

#[derive(PartialEq, Eq)]
struct Key {
    first: Option<Endpoint>,
    second: Option<Endpoint>,
    anchors: [Option<(u16, u16, Option<u8>)>; 2],
    hints: [Hint; 2],
    outline: Vec<(u16, u16, u16)>,
    obstacles: Vec<(u16, u16)>,
    boundary_gaps: Vec<(u16, u16)>,
    shape: Option<(
        u64,
        Arc<BTreeMap<usize, (i64, u16)>>,
        Arc<BTreeMap<usize, i64>>,
    )>,
}

impl Key {
    fn new(
        painter: &Painter<'_>,
        geometry: &Geometry,
        rows: &BTreeMap<usize, (u16, u16)>,
        mut pair: Pair,
    ) -> Self {
        if pair.1 < pair.0 {
            std::mem::swap(&mut pair.0, &mut pair.1);
        }
        let first = source_endpoint(painter, geometry, rows, pair.0);
        let second = source_endpoint(painter, geometry, rows, pair.1);
        let shape = painter.scope_shapes.get(&pair);
        let span = first.zip(second).map(|(first, second)| {
            let top = first
                .y
                .min(second.y)
                .min(rows.get(&pair.0.0).map_or(first.y, |row| row.0));
            let bottom = first
                .y
                .max(second.y)
                .max(rows.get(&pair.1.0).map_or(second.y, |row| row.1));
            (
                top.saturating_sub(ROUTE_ROW_MARGIN),
                bottom.saturating_add(ROUTE_ROW_MARGIN),
            )
        });
        let mut obstacles: Vec<_> = geometry
            .obstacles
            .iter()
            .copied()
            .filter(|&(_, y)| {
                shape.is_none() && span.is_some_and(|(top, bottom)| y >= top && y <= bottom)
            })
            .collect();
        obstacles.sort_unstable();
        let mut outline = BTreeMap::<u16, (u16, u16)>::new();
        for anchor in geometry.anchors.iter().take(if shape.is_none() {
            geometry.anchors.len()
        } else {
            0
        }) {
            if !bubble::contains(pair, anchor.line, anchor.col)
                || matches!(anchor.ch, ' ' | '\t' | '\0')
            {
                continue;
            }
            let row = outline
                .entry(anchor.y)
                .or_insert((anchor.x, anchor.x + anchor.width));
            row.0 = row.0.min(anchor.x);
            row.1 = row.1.max(anchor.x + anchor.width);
        }
        Self {
            shape: shape.map(|shape| {
                (
                    shape.key,
                    Arc::clone(&shape.rows),
                    Arc::clone(&shape.origins),
                )
            }),
            obstacles,
            boundary_gaps: if shape.is_some() {
                Vec::new()
            } else {
                bubble::boundary_gaps(geometry, pair)
            },
            outline: outline
                .into_iter()
                .map(|(y, (left, right))| (y, left, right))
                .collect(),
            first,
            second,
            anchors: [pair.0, pair.1].map(|position| {
                geometry
                    .at(position)
                    .map(|a| (a.x, a.y, super::super::delimiter_family(a.ch)))
            }),
            hints: [pair.0, pair.1].map(|position| match painter.scope_endpoints.get(&position) {
                None => Hint::Missing,
                Some(None) => Hint::Unsupported,
                Some(Some(hint)) => Hint::Projected(hint.x, hint.precise, hint.delimiter_x),
            }),
        }
    }
}

struct Entry {
    key: Key,
    projection: Option<(u64, Pair)>,
    hints: [Option<Option<super::super::ScopeEndpointHint>>; 2],
    generation: u64,
    route: Option<Route>,
}

#[derive(Default)]
pub(crate) struct RouteCache {
    loose: bool,
    area: Rect,
    gutter: Rect,
    bounds: Rect,
    walls: Vec<u64>,
    free: Vec<u64>,
    scratch: Vec<u64>,
    entries: Vec<Entry>,
    generation: u64,
    ready: bool,
    grid_ready: bool,
    wall_projection: Option<u64>,
    #[cfg(test)]
    pub(crate) builds: usize,
    #[cfg(test)]
    grid_builds: usize,
    #[cfg(test)]
    wall_builds: usize,
    #[cfg(test)]
    key_builds: usize,
}

impl RouteCache {
    pub(super) fn set_tight(&mut self, tight: bool) {
        if self.loose == tight {
            self.loose = !tight;
            self.entries.clear();
        }
    }
    #[cfg(test)]
    pub(super) fn prepare(&mut self, painter: &Painter<'_>, geometry: &Geometry, gutter: Rect) {
        self.prepare_rows(
            painter,
            geometry,
            gutter,
            geometry.area.y,
            geometry.area.bottom().saturating_sub(1),
        );
    }

    pub(super) fn prepare_rows(
        &mut self,
        _painter: &Painter<'_>,
        geometry: &Geometry,
        gutter: Rect,
        top: u16,
        bottom: u16,
    ) {
        const MAX_CACHED_CELLS: usize = 65_536;
        let mut bounds = if gutter.is_empty() {
            geometry.area
        } else {
            geometry.area.union(gutter)
        };
        // Include the projected source rows and their cap margin, without
        // scanning unrelated viewport rows on every animation frame.
        let top = top.max(bounds.y);
        let bottom = bottom.saturating_add(1).min(bounds.bottom());
        bounds.y = top;
        bounds.height = bottom.saturating_sub(top);
        let count = usize::from(bounds.width) * usize::from(bounds.height);
        self.grid_ready = false;
        self.ready = count != 0 && count <= MAX_CACHED_CELLS;
        if !self.ready {
            self.entries.clear();
            self.wall_projection = None;
            return;
        }
        let projection = geometry.is_retained().then_some(geometry.projection_id);
        if projection.is_some() && self.wall_projection == projection
            && self.area == geometry.area && self.gutter == gutter && self.bounds == bounds
        {
            return;
        }
        self.wall_projection = projection;
        #[cfg(test)]
        { self.wall_builds += 1; }
        self.walls.clear();
        self.walls.resize(count.div_ceil(64), 0);
        let mut block = |x: u16, y: u16| {
            if bounds.contains((x, y).into()) {
                let index = usize::from(y - bounds.y) * usize::from(bounds.width)
                    + usize::from(x - bounds.x);
                self.walls[index / 64] |= 1 << (index % 64);
            }
        };
        // Literal box glyphs and every cell of wide source glyphs are walls,
        // even if their terminal symbols resemble decorative guides.
        for anchor in &geometry.anchors {
            if anchor.y >= bounds.y
                && anchor.y < bounds.bottom()
                && !matches!(anchor.ch, ' ' | '\t' | '\0')
            {
                for dx in 0..anchor.width {
                    block(anchor.x + dx, anchor.y);
                }
            }
        }
        for &(x, y) in &geometry.obstacles {
            block(x, y);
        }
        if self.area != geometry.area || self.gutter != gutter || self.bounds != bounds {
            self.entries.clear();
        }
        self.area = geometry.area;
        self.gutter = gutter;
        self.bounds = bounds;
    }

    fn available(&self, painter: &Painter<'_>, x: u16, y: u16) -> bool {
        if !self.bounds.contains((x, y).into())
            || !(self.area.contains((x, y).into()) || self.gutter.contains((x, y).into()))
        {
            return false;
        }
        let index = usize::from(y - self.bounds.y) * usize::from(self.bounds.width)
            + usize::from(x - self.bounds.x);
        self.walls[index / 64] & (1 << (index % 64)) == 0 && route_cell(painter, x, y)
    }

    fn ensure_grid(&mut self, painter: &Painter<'_>) {
        if self.grid_ready {
            return;
        }
        #[cfg(test)]
        {
            self.grid_builds += 1;
        }
        self.scratch.clear();
        self.scratch.resize(self.walls.len(), 0);
        for y in self.bounds.y..self.bounds.bottom() {
            if painter.blocked_rows.contains(&y) { continue; }
            for x in self.bounds.x..self.bounds.right() {
                if self.available(painter, x, y) {
                    let index = usize::from(y - self.bounds.y) * usize::from(self.bounds.width)
                        + usize::from(x - self.bounds.x);
                    self.scratch[index / 64] |= 1 << (index % 64);
                }
            }
        }
        if self.free != self.scratch {
            self.generation = self.generation.wrapping_add(1);
        }
        std::mem::swap(&mut self.free, &mut self.scratch);
        self.grid_ready = true;
    }

    pub(super) fn route(
        &mut self,
        painter: &Painter<'_>,
        geometry: &Geometry,
        gutter: Rect,
        rows: &BTreeMap<usize, (u16, u16)>,
        pair: Pair,
    ) -> Option<&Route> {
        let normalized = (pair.0.min(pair.1), pair.0.max(pair.1));
        let projection = geometry.is_retained().then_some((geometry.projection_id, normalized));
        let hints = [normalized.0, normalized.1].map(|position| painter.scope_endpoints.get(&position).copied());
        let shape = painter.scope_shapes.get(&normalized);
        // Retained geometry certifies unchanged anchors and source rows. Keep
        // hints and source-shape identity in this check: offscreen edits can
        // move attachments even while the visible projection stays unchanged.
        let projected = projection.and_then(|projection| self.entries.iter().position(|entry| {
            entry.projection == Some(projection) && entry.hints == hints
                && match (&entry.key.shape, shape) {
                    (None, None) => true,
                    (Some((key, rows, origins)), Some(shape)) => *key == shape.key
                        && Arc::ptr_eq(rows, &shape.rows) && Arc::ptr_eq(origins, &shape.origins),
                    _ => false,
                }
        }));
        let mut key = None;
        let existing = projected.or_else(|| {
            #[cfg(test)]
            { self.key_builds += 1; }
            key = Some(Key::new(painter, geometry, rows, pair));
            self.entries.iter().position(|entry| Some(&entry.key) == key.as_ref())
        });
        if let Some(index) = existing {
            let exact = self.entries[index]
                .route
                .as_ref()
                .is_some_and(|route| route.exact);
            let valid = self.ready
                && if exact {
                    self.entries[index]
                        .route
                        .as_ref()
                        .unwrap()
                        .cells
                        .iter()
                        .all(|cell| self.available(painter, cell.x, cell.y))
                } else {
                    // A failed/fallback path must notice newly opened whitespace.
                    // Exact paths only need their own cells to remain unoccupied.
                    self.ensure_grid(painter);
                    self.entries[index].generation == self.generation
                };
            if valid {
                self.entries[index].projection = projection;
                self.entries[index].hints = hints;
                return self.entries[index].route.as_ref();
            }
            self.entries.remove(index);
        }
        #[cfg(test)]
        {
            self.builds += 1;
        }
        let route = scope_route_with_mode(painter, geometry, gutter, rows, pair, !self.loose);
        if self.ready && !route.as_ref().is_some_and(|route| route.exact) {
            self.ensure_grid(painter);
        }
        // An active scope and four fading predecessors are the usual maximum.
        // Keep one spare for rapid transitions; memory never grows with edits.
        if self.entries.len() >= 6 {
            self.entries.remove(0);
        }
        self.entries.push(Entry {
            key: key.unwrap_or_else(|| Key::new(painter, geometry, rows, pair)),
            projection,
            hints,
            generation: self.generation,
            route,
        });
        self.entries.last().unwrap().route.as_ref()
    }
}

#[cfg(test)]
#[path = "ui_scope_route_cache_tests.rs"]
mod tests;
