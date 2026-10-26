//! Pure terminal rasterization for a filled cursor ribbon.
//!
//! Inputs use fractional terminal-cell coordinates. A cell is twice as tall as
//! it is wide in the physical metric used for longitudinal shading and subcell
//! sampling. Text and terminal timing are deliberately outside this module.
use ratatui::layout::Rect;

use crate::animations::MotionPoint;

const SAMPLE_COLUMNS: usize = 8;
const SAMPLE_ROWS: usize = 16;
const SAMPLE_COUNT: f64 = (SAMPLE_COLUMNS * SAMPLE_ROWS) as f64;
/// Motion normally limits a ribbon to 25 physical cell widths. This independent
/// cap also bounds accidental oversized or synthetic polygons passed by callers.
const MAX_RASTER_CELLS: usize = 4_096;
const MIN_COVERAGE: f64 = 0.000_001;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SmearCell {
    pub x: u16,
    pub y: u16,
    /// True polygon area inside this terminal cell, including fractional edges.
    pub coverage: f32,
    /// A solid Unicode block; never a dotted shading or braille character.
    pub symbol: &'static str,
    /// Swap the blank cell's ordinary background and ribbon foreground. This
    /// represents upper/right fractions whose direct Unicode glyph is missing.
    pub inverse: bool,
    /// Area occupied by the effective colored block after applying `inverse`.
    /// Blank-cell opacity can use min(coverage / glyph_coverage, 1) to avoid
    /// applying the fractional edge coverage twice.
    pub glyph_coverage: f32,
    /// Longitudinal brightness from the trailing end (0.35) to the head (1.0).
    pub shade: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Point {
    x: f64,
    y: f64,
}

impl From<MotionPoint> for Point {
    fn from(point: MotionPoint) -> Self {
        Self {
            x: f64::from(point.x),
            y: f64::from(point.y),
        }
    }
}

fn cross(a: Point, b: Point, c: Point) -> f64 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

fn convex_hull(corners: [MotionPoint; 4]) -> Polygon {
    let mut points = corners.map(Point::from);
    points.sort_by(|a, b| a.x.total_cmp(&b.x).then_with(|| a.y.total_cmp(&b.y)));
    let mut lower = Polygon::empty();
    for &point in &points {
        while lower.len >= 2
            && cross(
                lower.points[lower.len - 2],
                lower.points[lower.len - 1],
                point,
            ) <= 0.0
        {
            lower.len -= 1;
        }
        if lower.len == 0 || lower.points[lower.len - 1] != point {
            lower.push(point);
        }
    }
    let mut upper = Polygon::empty();
    for &point in points.iter().rev() {
        while upper.len >= 2
            && cross(
                upper.points[upper.len - 2],
                upper.points[upper.len - 1],
                point,
            ) <= 0.0
        {
            upper.len -= 1;
        }
        if upper.len == 0 || upper.points[upper.len - 1] != point {
            upper.push(point);
        }
    }
    lower.len = lower.len.saturating_sub(1);
    upper.len = upper.len.saturating_sub(1);
    for &point in &upper.points[..upper.len] {
        lower.push(point);
    }
    lower
}

/// A convex quadrilateral clipped against a rectangle has at most eight points.
/// Two small stack buffers avoid allocations for each candidate terminal cell.
#[derive(Clone, Copy)]
struct Polygon {
    points: [Point; 12],
    len: usize,
}

impl Polygon {
    fn empty() -> Self {
        Self {
            points: [Point::default(); 12],
            len: 0,
        }
    }

    fn push(&mut self, point: Point) {
        if self.len < self.points.len() {
            self.points[self.len] = point;
            self.len += 1;
        }
    }

    fn clipped(self, axis_x: bool, boundary: f64, greater: bool) -> Self {
        let mut result = Self {
            points: [Point::default(); 12],
            len: 0,
        };
        if self.len == 0 {
            return result;
        }
        let coordinate = |point: Point| if axis_x { point.x } else { point.y };
        let inside = |point: Point| {
            if greater {
                coordinate(point) >= boundary
            } else {
                coordinate(point) <= boundary
            }
        };
        let mut previous = self.points[self.len - 1];
        let mut previous_inside = inside(previous);
        for &current in &self.points[..self.len] {
            let current_inside = inside(current);
            if current_inside != previous_inside {
                let difference = coordinate(current) - coordinate(previous);
                if difference != 0.0 {
                    let t = ((boundary - coordinate(previous)) / difference).clamp(0.0, 1.0);
                    let mut intersection = Point {
                        x: previous.x + (current.x - previous.x) * t,
                        y: previous.y + (current.y - previous.y) * t,
                    };
                    if axis_x {
                        intersection.x = boundary;
                    } else {
                        intersection.y = boundary;
                    }
                    result.push(intersection);
                }
            }
            if current_inside {
                result.push(current);
            }
            previous = current;
            previous_inside = current_inside;
        }
        result
    }

    fn area(&self) -> f64 {
        if self.len < 3 {
            return 0.0;
        }
        // Subtract an origin before multiplying to preserve small coverage at
        // coordinates near u16::MAX (a global shoelace sum loses precision).
        let origin = self.points[0];
        (1..self.len - 1)
            .map(|index| cross(origin, self.points[index], self.points[index + 1]))
            .sum::<f64>()
            .abs()
            * 0.5
    }

    #[cfg(test)]
    fn contains(&self, point: Point) -> bool {
        (0..self.len).all(|index| {
            cross(
                self.points[index],
                self.points[(index + 1) % self.len],
                point,
            ) >= -1e-10
        })
    }

    fn centroid(&self) -> Point {
        let origin = self.points[0];
        let mut weight = 0.0;
        let mut x = 0.0;
        let mut y = 0.0;
        for index in 1..self.len.saturating_sub(1) {
            let a = self.points[index];
            let b = self.points[index + 1];
            let triangle = cross(origin, a, b).abs();
            x += (origin.x + a.x + b.x) * triangle / 3.0;
            y += (origin.y + a.y + b.y) * triangle / 3.0;
            weight += triangle;
        }
        if weight > 0.0 {
            Point {
                x: x / weight,
                y: y / weight,
            }
        } else {
            origin
        }
    }
}

/// A convex polygon intersects each sample row in one contiguous interval.
/// Solve the edge inequalities once per row rather than testing all edges at
/// each of the 128 subcell sample points.
fn occupancy(polygon: &Polygon, x: f64, y: f64) -> u128 {
    let mut result = 0;
    for row in 0..SAMPLE_ROWS {
        let sample_y = y + (row as f64 + 0.5) / SAMPLE_ROWS as f64;
        let mut left = x;
        let mut right = x + 1.0;
        for index in 0..polygon.len {
            let first = polygon.points[index];
            let last = polygon.points[(index + 1) % polygon.len];
            let dx = last.x - first.x;
            let dy = last.y - first.y;
            if dy == 0.0 {
                if dx * (sample_y - first.y) < -1e-10 {
                    right = left - 1.0;
                    break;
                }
            } else {
                let crossing = first.x + (dx * (sample_y - first.y) + 1e-10) / dy;
                if dy > 0.0 {
                    right = right.min(crossing);
                } else {
                    left = left.max(crossing);
                }
            }
        }
        let first = (((left - x) * SAMPLE_COLUMNS as f64 - 0.5).ceil() as i32)
            .clamp(0, SAMPLE_COLUMNS as i32) as u32;
        let end = ((((right - x) * SAMPLE_COLUMNS as f64 - 0.5).floor() as i32) + 1)
            .clamp(0, SAMPLE_COLUMNS as i32) as u32;
        if end > first {
            result |= (((1u128 << (end - first)) - 1) << first) << (row * SAMPLE_COLUMNS);
        }
    }
    result
}

const fn rectangle_mask(left: usize, top: usize, right: usize, bottom: usize) -> u128 {
    let mut mask = 0;
    let mut row = top;
    while row < bottom {
        let mut column = left;
        while column < right {
            mask |= 1u128 << (row * SAMPLE_COLUMNS + column);
            column += 1;
        }
        row += 1;
    }
    mask
}

#[derive(Clone, Copy)]
struct Glyph {
    symbol: &'static str,
    inverse: bool,
    mask: u128,
    coverage: f64,
    centroid: Point,
}

const fn glyph(symbol: &'static str, inverse: bool, mask: u128) -> Glyph {
    let mut x = 0.0;
    let mut y = 0.0;
    let mut count = 0;
    let mut index = 0;
    while index < SAMPLE_COLUMNS * SAMPLE_ROWS {
        if mask & (1u128 << index) != 0 {
            x += ((index % SAMPLE_COLUMNS) as f64 + 0.5) / SAMPLE_COLUMNS as f64;
            y += ((index / SAMPLE_COLUMNS) as f64 + 0.5) / SAMPLE_ROWS as f64;
            count += 1;
        }
        index += 1;
    }
    Glyph {
        symbol,
        inverse,
        mask,
        coverage: count as f64 / SAMPLE_COUNT,
        centroid: Point {
            x: x / count as f64,
            y: y / count as f64,
        },
    }
}

const TOP_LEFT: u128 = rectangle_mask(0, 0, 4, 8);
const TOP_RIGHT: u128 = rectangle_mask(4, 0, 8, 8);
const BOTTOM_LEFT: u128 = rectangle_mask(0, 8, 4, 16);
const BOTTOM_RIGHT: u128 = rectangle_mask(4, 8, 8, 16);
const FULL: Glyph = glyph("█", false, u128::MAX);

const GLYPHS: [Glyph; 39] = [
    glyph("▁", false, rectangle_mask(0, 14, 8, 16)),
    glyph("▂", false, rectangle_mask(0, 12, 8, 16)),
    glyph("▃", false, rectangle_mask(0, 10, 8, 16)),
    glyph("▄", false, rectangle_mask(0, 8, 8, 16)),
    glyph("▅", false, rectangle_mask(0, 6, 8, 16)),
    glyph("▆", false, rectangle_mask(0, 4, 8, 16)),
    glyph("▇", false, rectangle_mask(0, 2, 8, 16)),
    FULL,
    glyph("▔", false, rectangle_mask(0, 0, 8, 2)),
    glyph("▆", true, rectangle_mask(0, 0, 8, 4)),
    glyph("▅", true, rectangle_mask(0, 0, 8, 6)),
    glyph("▀", false, rectangle_mask(0, 0, 8, 8)),
    glyph("▃", true, rectangle_mask(0, 0, 8, 10)),
    glyph("▂", true, rectangle_mask(0, 0, 8, 12)),
    glyph("▁", true, rectangle_mask(0, 0, 8, 14)),
    glyph("▏", false, rectangle_mask(0, 0, 1, 16)),
    glyph("▎", false, rectangle_mask(0, 0, 2, 16)),
    glyph("▍", false, rectangle_mask(0, 0, 3, 16)),
    glyph("▌", false, rectangle_mask(0, 0, 4, 16)),
    glyph("▋", false, rectangle_mask(0, 0, 5, 16)),
    glyph("▊", false, rectangle_mask(0, 0, 6, 16)),
    glyph("▉", false, rectangle_mask(0, 0, 7, 16)),
    glyph("▕", false, rectangle_mask(7, 0, 8, 16)),
    glyph("▊", true, rectangle_mask(6, 0, 8, 16)),
    glyph("▋", true, rectangle_mask(5, 0, 8, 16)),
    glyph("▐", false, rectangle_mask(4, 0, 8, 16)),
    glyph("▍", true, rectangle_mask(3, 0, 8, 16)),
    glyph("▎", true, rectangle_mask(2, 0, 8, 16)),
    glyph("▏", true, rectangle_mask(1, 0, 8, 16)),
    glyph("▘", false, TOP_LEFT),
    glyph("▝", false, TOP_RIGHT),
    glyph("▖", false, BOTTOM_LEFT),
    glyph("▗", false, BOTTOM_RIGHT),
    glyph("▚", false, TOP_LEFT | BOTTOM_RIGHT),
    glyph("▞", false, TOP_RIGHT | BOTTOM_LEFT),
    glyph("▛", false, TOP_LEFT | TOP_RIGHT | BOTTOM_LEFT),
    glyph("▜", false, TOP_LEFT | TOP_RIGHT | BOTTOM_RIGHT),
    glyph("▙", false, TOP_LEFT | BOTTOM_LEFT | BOTTOM_RIGHT),
    glyph("▟", false, TOP_RIGHT | BOTTOM_LEFT | BOTTOM_RIGHT),
];

fn edge_glyph(polygon: &Polygon, x: f64, y: f64, coverage: f64, centroid: Point) -> Glyph {
    let centroid = Point {
        x: centroid.x - x,
        y: centroid.y - y,
    };
    // An interior strip has no corresponding solid Unicode block. A softly
    // blended full cell preserves its position instead of snapping it to an
    // unrelated top/bottom or left/right edge (or drawing a dotted substitute).
    let min_x = polygon.points[..polygon.len]
        .iter()
        .map(|point| point.x - x)
        .fold(1.0, f64::min);
    let max_x = polygon.points[..polygon.len]
        .iter()
        .map(|point| point.x - x)
        .fold(0.0, f64::max);
    let min_y = polygon.points[..polygon.len]
        .iter()
        .map(|point| point.y - y)
        .fold(1.0, f64::min);
    let max_y = polygon.points[..polygon.len]
        .iter()
        .map(|point| point.y - y)
        .fold(0.0, f64::max);
    if (min_y > 1e-9 && max_y < 1.0 - 1e-9 && min_x <= 1e-9 && max_x >= 1.0 - 1e-9)
        || (min_x > 1e-9 && max_x < 1.0 - 1e-9 && min_y <= 1e-9 && max_y >= 1.0 - 1e-9)
        || (min_x > 1e-9 && max_x < 1.0 - 1e-9 && min_y > 1e-9 && max_y < 1.0 - 1e-9)
    {
        return FULL;
    }
    let occupancy = occupancy(polygon, x, y);
    let score = |candidate: &Glyph| {
        let disagreement = (candidate.mask ^ occupancy).count_ones() as f64 / SAMPLE_COUNT;
        let area_error = (candidate.coverage - coverage).abs();
        let dx = candidate.centroid.x - centroid.x;
        let dy = (candidate.centroid.y - centroid.y) * 2.0;
        disagreement + area_error * 0.15 + (dx * dx + dy * dy) * 0.25
    };
    let mut best = FULL;
    let mut best_score = f64::INFINITY;
    for candidate in GLYPHS {
        let candidate_score = score(&candidate);
        if candidate_score < best_score {
            best = candidate;
            best_score = candidate_score;
        }
    }
    best
}

fn longitudinal_shade(point: Point, tail: Point, head: Point) -> f32 {
    let dx = head.x - tail.x;
    let dy = (head.y - tail.y) * 2.0;
    let denominator = dx * dx + dy * dy;
    if denominator <= f64::EPSILON {
        return 1.0;
    }
    let t =
        (((point.x - tail.x) * dx + (point.y - tail.y) * 2.0 * dy) / denominator).clamp(0.0, 1.0);
    (0.35 + 0.65 * t) as f32
}

/// Rasterize the convex hull of four ribbon corners into clipped terminal cells.
/// Corner order may be reversed, duplicated, or crossed during a direction
/// change. Degenerate and non-finite geometry produces no cells.
#[cfg(test)]
pub(crate) fn rasterize(
    area: Rect,
    corners: [MotionPoint; 4],
    tail: MotionPoint,
    head: MotionPoint,
) -> Vec<SmearCell> {
    let mut cells = Vec::new();
    rasterize_with(area, corners, tail, head, |cell| cells.push(cell));
    cells
}

/// Emit cells directly without allocating a frame-sized result or hull.
pub(crate) fn rasterize_with(
    area: Rect,
    corners: [MotionPoint; 4],
    tail: MotionPoint,
    head: MotionPoint,
    mut emit: impl FnMut(SmearCell),
) {
    if area.is_empty()
        || corners
            .iter()
            .chain([&tail, &head])
            .any(|point| !point.x.is_finite() || !point.y.is_finite())
    {
        return;
    }
    let hull = convex_hull(corners);
    if hull.len < 3 {
        return;
    }
    let polygon = hull
        .clipped(true, f64::from(area.x), true)
        .clipped(true, f64::from(area.right()), false)
        .clipped(false, f64::from(area.y), true)
        .clipped(false, f64::from(area.bottom()), false);
    if polygon.len < 3 || polygon.area() <= MIN_COVERAGE {
        return;
    }
    let min_x = polygon.points[..polygon.len]
        .iter()
        .map(|point| point.x)
        .fold(f64::INFINITY, f64::min)
        .floor()
        .max(f64::from(area.x)) as u32;
    let max_x = polygon.points[..polygon.len]
        .iter()
        .map(|point| point.x)
        .fold(f64::NEG_INFINITY, f64::max)
        .ceil()
        .min(f64::from(area.right())) as u32;
    let min_y = polygon.points[..polygon.len]
        .iter()
        .map(|point| point.y)
        .fold(f64::INFINITY, f64::min)
        .floor()
        .max(f64::from(area.y)) as u32;
    let max_y = polygon.points[..polygon.len]
        .iter()
        .map(|point| point.y)
        .fold(f64::NEG_INFINITY, f64::max)
        .ceil()
        .min(f64::from(area.bottom())) as u32;
    let tail = Point::from(tail);
    let head = Point::from(head);
    let mut examined = 0;
    'rows: for y in min_y..max_y {
        let row_polygon =
            polygon
                .clipped(false, f64::from(y), true)
                .clipped(false, f64::from(y) + 1.0, false);
        if row_polygon.len < 3 {
            continue;
        }
        let row_min = row_polygon.points[..row_polygon.len]
            .iter()
            .map(|point| point.x)
            .fold(f64::INFINITY, f64::min)
            .floor()
            .max(f64::from(min_x)) as u32;
        let row_max = row_polygon.points[..row_polygon.len]
            .iter()
            .map(|point| point.x)
            .fold(f64::NEG_INFINITY, f64::max)
            .ceil()
            .min(f64::from(max_x)) as u32;
        for x in row_min..row_max {
            if examined == MAX_RASTER_CELLS {
                break 'rows;
            }
            examined += 1;
            let clipped = row_polygon.clipped(true, f64::from(x), true).clipped(
                true,
                f64::from(x) + 1.0,
                false,
            );
            let coverage = clipped.area().clamp(0.0, 1.0);
            if coverage <= MIN_COVERAGE {
                continue;
            }
            let centroid = clipped.centroid();
            let block = if coverage >= 1.0 - MIN_COVERAGE {
                FULL
            } else {
                edge_glyph(&clipped, f64::from(x), f64::from(y), coverage, centroid)
            };
            emit(SmearCell {
                x: x as u16,
                y: y as u16,
                coverage: coverage as f32,
                symbol: block.symbol,
                inverse: block.inverse,
                glyph_coverage: block.coverage as f32,
                shade: longitudinal_shade(centroid, tail, head),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(x: f32, y: f32) -> MotionPoint {
        MotionPoint { x, y }
    }

    fn rectangle(left: f32, top: f32, right: f32, bottom: f32) -> [MotionPoint; 4] {
        [
            point(left, top),
            point(right, top),
            point(right, bottom),
            point(left, bottom),
        ]
    }

    fn one_cell(corners: [MotionPoint; 4]) -> SmearCell {
        let cells = rasterize(
            Rect::new(0, 0, 1, 1),
            corners,
            point(0.0, 0.5),
            point(1.0, 0.5),
        );
        assert_eq!(cells.len(), 1);
        cells[0]
    }

    #[test]
    fn full_cell_has_a_solid_block_and_full_coverage() {
        let cell = one_cell(rectangle(0.0, 0.0, 1.0, 1.0));
        assert_eq!(
            (
                cell.symbol,
                cell.inverse,
                cell.coverage,
                cell.glyph_coverage
            ),
            ("█", false, 1.0, 1.0)
        );
    }

    #[test]
    fn halves_and_quadrants_preserve_fractional_area() {
        for (bounds, expected, area) in [
            ((0.0, 0.5, 1.0, 1.0), "▄", 0.5),
            ((0.0, 0.0, 1.0, 0.5), "▀", 0.5),
            ((0.0, 0.0, 0.5, 1.0), "▌", 0.5),
            ((0.0, 0.0, 0.5, 0.5), "▘", 0.25),
            ((0.5, 0.5, 1.0, 1.0), "▗", 0.25),
        ] {
            let cell = one_cell(rectangle(bounds.0, bounds.1, bounds.2, bounds.3));
            assert_eq!(cell.symbol, expected);
            assert!((cell.coverage - area).abs() < 1e-6);
            assert!((cell.glyph_coverage - area).abs() < 1e-6);
        }
    }

    #[test]
    fn eighth_edges_and_inverse_upper_right_edges_are_crisp() {
        for (corners, symbol, inverse, coverage) in [
            (rectangle(0.0, 0.875, 1.0, 1.0), "▁", false, 0.125),
            (rectangle(0.0, 0.0, 0.125, 1.0), "▏", false, 0.125),
            (rectangle(0.0, 0.0, 1.0, 0.25), "▆", true, 0.25),
            (rectangle(0.75, 0.0, 1.0, 1.0), "▊", true, 0.25),
        ] {
            let cell = one_cell(corners);
            assert_eq!((cell.symbol, cell.inverse), (symbol, inverse));
            assert!((cell.coverage - coverage).abs() < 1e-6);
        }
    }

    #[test]
    fn thin_diagonal_ribbon_has_continuous_finite_coverage() {
        let corners = [
            point(0.0, 0.0),
            point(0.3, 0.0),
            point(5.0, 3.0),
            point(4.7, 3.0),
        ];
        let cells = rasterize(
            Rect::new(0, 0, 8, 5),
            corners,
            point(0.15, 0.0),
            point(4.85, 3.0),
        );
        assert!(cells.len() >= 7);
        let area: f32 = cells.iter().map(|cell| cell.coverage).sum();
        assert!((area - 0.9).abs() < 0.0001);
        for y in 0..3 {
            assert!(cells.iter().any(|cell| cell.y == y));
        }
        assert!(
            cells.iter().all(|cell| cell.coverage.is_finite()
                && cell.coverage > 0.0
                && cell.coverage <= 1.0)
        );
        assert!(
            cells
                .iter()
                .all(|cell| cell.shade.is_finite() && (0.35..=1.0).contains(&cell.shade))
        );
    }

    #[test]
    fn clipping_discards_outside_cells_and_preserves_boundary_fractions() {
        let cells = rasterize(
            Rect::new(5, 4, 3, 2),
            rectangle(3.0, 3.0, 8.5, 5.5),
            point(3.0, 3.0),
            point(8.5, 5.5),
        );
        assert_eq!(cells.len(), 6);
        assert!(
            cells
                .iter()
                .all(|cell| (5..8).contains(&cell.x) && (4..6).contains(&cell.y))
        );
        assert!(
            cells
                .iter()
                .filter(|cell| cell.y == 4)
                .all(|cell| cell.coverage == 1.0)
        );
        assert!(
            cells
                .iter()
                .filter(|cell| cell.y == 5)
                .all(|cell| cell.coverage == 0.5)
        );
    }

    #[test]
    fn reversing_or_crossing_corners_has_the_same_filled_hull() {
        let corners = [
            point(1.0, 0.0),
            point(4.0, 1.0),
            point(3.0, 3.0),
            point(0.0, 2.0),
        ];
        let area = Rect::new(0, 0, 6, 4);
        let expected = rasterize(area, corners, point(0.0, 1.0), point(4.0, 2.0));
        for order in [[3, 2, 1, 0], [0, 2, 1, 3], [1, 3, 0, 2]] {
            assert_eq!(
                rasterize(
                    area,
                    order.map(|index| corners[index]),
                    point(0.0, 1.0),
                    point(4.0, 2.0)
                ),
                expected
            );
        }
    }

    #[test]
    fn wide_terminal_coordinates_keep_quarter_coverage() {
        let cells = rasterize(
            Rect::new(65_530, 65_530, 5, 5),
            rectangle(65_533.0, 65_533.0, 65_533.5, 65_533.5),
            point(65_530.0, 65_530.0),
            point(65_534.0, 65_534.0),
        );
        assert_eq!(cells.len(), 1);
        assert_eq!(
            (cells[0].x, cells[0].y, cells[0].symbol),
            (65_533, 65_533, "▘")
        );
        assert_eq!(cells[0].coverage, 0.25);
    }

    #[test]
    fn degenerate_nonfinite_and_empty_inputs_produce_no_cells() {
        let area = Rect::new(0, 0, 10, 10);
        let tail = point(0.0, 0.0);
        for corners in [
            [tail; 4],
            rectangle(0.0, 0.0, 0.0, 5.0),
            [point(f32::NAN, 0.0), tail, point(1.0, 1.0), point(0.0, 1.0)],
            rectangle(0.0, 0.0, f32::INFINITY, 1.0),
        ] {
            assert!(rasterize(area, corners, tail, tail).is_empty());
        }
        assert!(
            rasterize(
                Rect::new(0, 0, 0, 1),
                rectangle(0.0, 0.0, 1.0, 1.0),
                tail,
                tail
            )
            .is_empty()
        );
        assert!(
            rasterize(
                area,
                rectangle(0.0, 0.0, 1.0, 1.0),
                tail,
                point(f32::NAN, 0.0)
            )
            .is_empty()
        );
    }

    #[test]
    fn oversized_input_remains_bounded_and_zero_motion_has_finite_shade() {
        let cells = rasterize(
            Rect::new(0, 0, 1_000, 1_000),
            rectangle(-10.0, -10.0, 2_000.0, 2_000.0),
            point(0.0, 0.0),
            point(0.0, 0.0),
        );
        assert_eq!(cells.len(), MAX_RASTER_CELLS);
        assert!(cells.iter().all(|cell| cell.shade == 1.0));
    }

    #[test]
    fn scanline_occupancy_matches_independent_point_tests() {
        let mut seed = 0x73ab_49c1_u32;
        for offset in [0.0, 65_530.0] {
            for _ in 0..2_000 {
                let corners = std::array::from_fn(|_| {
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let x = (seed >> 8) as f32 / 16_777_216.0;
                    seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    let y = (seed >> 8) as f32 / 16_777_216.0;
                    point(offset + x, offset + y)
                });
                let polygon = convex_hull(corners);
                if polygon.len < 3 {
                    continue;
                }
                let mut expected = 0;
                for row in 0..SAMPLE_ROWS {
                    for column in 0..SAMPLE_COLUMNS {
                        let sample = Point {
                            x: f64::from(offset) + (column as f64 + 0.5) / SAMPLE_COLUMNS as f64,
                            y: f64::from(offset) + (row as f64 + 0.5) / SAMPLE_ROWS as f64,
                        };
                        if polygon.contains(sample) {
                            expected |= 1u128 << (row * SAMPLE_COLUMNS + column);
                        }
                    }
                }
                assert_eq!(
                    occupancy(&polygon, f64::from(offset), f64::from(offset)),
                    expected
                );
            }
        }
    }
}
