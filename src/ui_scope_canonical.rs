//! Source-space scope contours, projected and clipped only after construction.
use super::*;

#[derive(Clone, Debug, Default)]
pub(crate) struct Profile {
    pub(crate) left: Option<i64>,
    pub(crate) right: Option<i64>,
    pub(crate) gaps: Vec<(i64, i64)>,
}

#[derive(Clone, Copy)]
struct Stroke {
    from: i64,
    to: i64,
    before: u8,
    after: u8,
    distance: u64,
}

struct Row {
    left: Stroke,
    right: Stroke,
    gaps: Vec<(i64, i64)>,
}

pub(crate) struct Contour {
    rows: Vec<Option<Row>>,
    first: usize,
    last: usize,
    length: u64,
    heading_left: i64,
    closing_right: i64,
    closing_cost: u64,
}

impl Contour {
    pub(crate) fn new(profiles: &[Profile], tight: bool) -> Option<Self> {
        let original: Vec<_> = profiles
            .iter()
            .map(|row| match (row.left, row.right) {
                (Some(left), Some(right)) if left <= right => Some((left, right)),
                _ => None,
            })
            .collect();
        let first = original.iter().position(Option::is_some)?;
        let last = original.iter().rposition(Option::is_some)?;
        // Record nearest real rows with two linear passes. Empty lines retain
        // the surrounding silhouette without a quadratic nearest-row search.
        let mut previous = vec![None; original.len()];
        let mut seen = None;
        for (index, row) in original.iter().enumerate() {
            if row.is_some() {
                seen = Some(index);
            }
            previous[index] = seen;
        }
        let mut next = vec![None; original.len()];
        seen = None;
        for (index, row) in original.iter().enumerate().rev() {
            if row.is_some() {
                seen = Some(index);
            }
            next[index] = seen;
        }
        let mut envelopes: Vec<_> = (first..=last)
            .map(|index| {
                let nearest = match (previous[index], next[index]) {
                    (Some(before), Some(after)) => {
                        if index - before <= after - index {
                            before
                        } else {
                            after
                        }
                    }
                    (Some(row), None) | (None, Some(row)) => row,
                    _ => unreachable!(),
                };
                original[nearest].unwrap()
            })
            .collect();
        if !tight {
            let left = envelopes.iter().map(|row| row.0).min()?;
            let old = envelopes.clone();
            let right = |index: isize| old[index.clamp(0, old.len() as isize - 1) as usize].1;
            for (index, row) in envelopes.iter_mut().enumerate() {
                row.0 = left;
                row.1 = (-1..=1)
                    .map(|offset| {
                        (-1..=1)
                            .map(|neighbor| right(index as isize + offset + neighbor))
                            .max()
                            .unwrap()
                    })
                    .min()
                    .unwrap();
            }
        }
        let heading_left = original[first]?.0.checked_sub(1)?;
        let heading_right = original[first]?.1.checked_add(1)?;
        let closing_left = original[last]?.0.checked_sub(1)?;
        let closing_right = original[last]?.1.checked_add(1)?;
        let mut right_distance = heading_left.abs_diff(heading_right);
        let mut left_distance = 0u64;
        let mut rows: Vec<Option<Row>> = (0..profiles.len()).map(|_| None).collect();
        for index in first..=last {
            let local = index - first;
            let (left, right) = envelopes[local];
            let left_from = if index == first {
                heading_left
            } else {
                left.min(envelopes[local - 1].0).checked_sub(1)?
            };
            let left_to = if index == last {
                closing_left
            } else {
                left.min(envelopes[local + 1].0).checked_sub(1)?
            };
            let right_from = if index == first {
                heading_right
            } else {
                right.max(envelopes[local - 1].1).checked_add(1)?
            };
            let right_to = if index == last {
                closing_right
            } else {
                right.max(envelopes[local + 1].1).checked_add(1)?
            };
            rows[index] = Some(Row {
                left: Stroke {
                    from: left_from,
                    to: left_to,
                    before: if index == first {
                        super::super::EAST
                    } else {
                        super::super::NORTH
                    },
                    after: if index == last {
                        super::super::EAST
                    } else {
                        super::super::SOUTH
                    },
                    distance: left_distance,
                },
                right: Stroke {
                    from: right_from,
                    to: right_to,
                    before: if index == first {
                        super::super::WEST
                    } else {
                        super::super::NORTH
                    },
                    after: if index == last {
                        super::super::WEST
                    } else {
                        super::super::SOUTH
                    },
                    distance: right_distance,
                },
                gaps: if index == first || index == last {
                    profiles[index].gaps.clone()
                } else {
                    Vec::new()
                },
            });
            left_distance = left_distance.checked_add(left_from.abs_diff(left_to))?;
            right_distance = right_distance.checked_add(right_from.abs_diff(right_to))?;
            if index < last {
                left_distance = left_distance.checked_add(1)?;
                right_distance = right_distance.checked_add(1)?;
            }
        }
        let closing_cost = right_distance;
        let length = right_distance
            .checked_add(closing_right.abs_diff(closing_left))?
            .checked_add(left_distance)?
            .max(1);
        Some(Self {
            rows,
            first,
            last,
            length,
            heading_left,
            closing_right,
            closing_cost,
        })
    }

    fn stroke_cost(&self, stroke: Stroke, x: i64, left: bool) -> f32 {
        let distance = stroke.distance + stroke.from.abs_diff(x);
        if left {
            ((self.length - distance) % self.length) as f32
        } else {
            distance as f32
        }
    }

    fn gap_cost(&self, row: usize, x: i64) -> f32 {
        if row == self.first {
            self.heading_left.abs_diff(x) as f32
        } else {
            (self.closing_cost + self.closing_right.abs_diff(x)) as f32
        }
    }

    pub(crate) fn origin_cost(&self, row_index: usize, x: i64) -> f32 {
        let index = row_index.clamp(self.first, self.last);
        let row = self.rows[index].as_ref().unwrap();
        let mut best = (u64::MAX, 0.0);
        for (stroke, left) in [(row.left, true), (row.right, false)] {
            let nearest = x.clamp(stroke.from.min(stroke.to), stroke.from.max(stroke.to));
            let distance = x.abs_diff(nearest);
            if distance < best.0 {
                best = (distance, self.stroke_cost(stroke, nearest, left));
            }
        }
        for &(from, to) in &row.gaps {
            let nearest = x.clamp(from.min(to), from.max(to));
            let distance = x.abs_diff(nearest);
            if distance < best.0 {
                best = (distance, self.gap_cost(index, nearest));
            }
        }
        best.1
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn project(
        &self,
        painter: &Painter<'_>,
        geometry: &Geometry,
        gutter: Rect,
        rows: &BTreeMap<usize, (u16, u16)>,
        pair: Pair,
        projection: &BTreeMap<usize, (i64, u16)>,
        origins: &BTreeMap<usize, i64>,
    ) -> Option<Route> {
        let first = source_endpoint(painter, geometry, rows, pair.0)?;
        let second = source_endpoint(painter, geometry, rows, pair.1)?;
        let bounds = if gutter.is_empty() {
            geometry.area
        } else {
            geometry.area.union(gutter)
        };
        if bounds.is_empty() {
            return None;
        }
        let source = geometry.source_cells();
        let mut cells = BTreeMap::<(u16, u16), (u8, f32, bool)>::new();
        let mut fill = Vec::new();
        let mut budget = MAX_ROUTE_CELLS;
        let mut masked = false;
        let mut emit = |x: i64, y: u16, ports: u8, cost: f32, terminal: bool| -> Option<()> {
            if x < i64::from(bounds.x) || x >= i64::from(bounds.right()) {
                return Some(());
            }
            let x = x as u16;
            if !(geometry.area.contains((x, y).into()) || gutter.contains((x, y).into())) {
                return Some(());
            }
            if budget == 0 {
                return None;
            }
            budget -= 1;
            if source.contains((x, y))
                || geometry.obstacles.contains(&(x, y))
                || !route_cell(painter, x, y)
            {
                masked = true;
                return Some(());
            }
            let entry = cells.entry((x, y)).or_insert((ports, cost, terminal));
            let joined = entry.0 | ports;
            if joined.count_ones() <= 2 {
                entry.0 = joined;
            }
            entry.1 = entry.1.min(cost);
            entry.2 |= terminal;
            Some(())
        };
        for (&index, &(origin, y)) in projection {
            let Some(row) = self.rows.get(index).and_then(Option::as_ref) else {
                continue;
            };
            let mut fill_left = 0;
            let mut fill_right = 0;
            for (stroke, left) in [(row.left, true), (row.right, false)] {
                let exterior = |a: i64, b: i64| if left { a.min(b) } else { a.max(b) };
                let mut from = origin.checked_add(stroke.from)?;
                let mut to = origin.checked_add(stroke.to)?;
                if index > self.first
                    && let Some(&previous_origin) = origins.get(&(index - 1))
                    && let Some(previous) = self.rows[index - 1].as_ref()
                {
                    let previous = if left { previous.left } else { previous.right };
                    from = exterior(from, previous_origin.checked_add(previous.to)?);
                }
                if index < self.last
                    && let Some(&next_origin) = origins.get(&(index + 1))
                    && let Some(next) = self.rows[index + 1].as_ref()
                {
                    let next = if left { next.left } else { next.right };
                    to = exterior(to, next_origin.checked_add(next.from)?);
                }
                let start = from.min(to).max(i64::from(bounds.x));
                let end = from.max(to).min(i64::from(bounds.right()) - 1);
                if left {
                    fill_left = from.max(to);
                } else {
                    fill_right = from.min(to);
                }
                for x in start..=end {
                    let mut ports = if from == to {
                        stroke.before | stroke.after
                    } else if x == from {
                        stroke.before
                            | if to > from {
                                super::super::EAST
                            } else {
                                super::super::WEST
                            }
                    } else if x == to {
                        stroke.after
                            | if from > to {
                                super::super::EAST
                            } else {
                                super::super::WEST
                            }
                    } else {
                        super::super::EAST | super::super::WEST
                    };
                    if ports == 0 {
                        ports = super::super::NORTH | super::super::SOUTH;
                    }
                    let canonical_x =
                        (x - origin).clamp(stroke.from.min(stroke.to), stroke.from.max(stroke.to));
                    emit(
                        x,
                        y,
                        ports,
                        self.stroke_cost(stroke, canonical_x, left),
                        index == self.first || index == self.last,
                    )?;
                }
                // Rows belonging to the other diff revision can separate two
                // source rows. Continue the same edge when either neighboring
                // source row is offscreen, bounded by projected scope markers.
                // These display-only rows share the canonical edge midpoint;
                // clipping cannot change their animation distance.
                if index > self.first && !projection.contains_key(&(index - 1)) {
                    let cost = self.stroke_cost(stroke, stroke.from, left);
                    let bridge_cost = if left {
                        (cost + 0.5).rem_euclid(self.length as f32)
                    } else {
                        cost - 0.5
                    };
                    let preceding = projection
                        .range(..index)
                        .next_back()
                        .map_or(bounds.y, |(_, &(_, row))| row.saturating_add(1));
                    for bridge_y in bounds.y.max(first.y).max(preceding)..y.min(bounds.bottom()) {
                        emit(
                            from,
                            bridge_y,
                            super::super::NORTH | super::super::SOUTH,
                            bridge_cost,
                            false,
                        )?;
                    }
                }
                if index < self.last {
                    let next_y = projection
                        .get(&(index + 1))
                        .map(|&(_, y)| y)
                        .unwrap_or_else(|| {
                            second
                                .y
                                .min(bounds.bottom().saturating_sub(1))
                                .saturating_add(1)
                        });
                    let next_y = projection
                        .range(index + 1..)
                        .next()
                        .map_or(next_y, |(_, &(_, row))| next_y.min(row));
                    let cost = self.stroke_cost(stroke, stroke.to, left);
                    let bridge_cost = if left {
                        (cost - 0.5).rem_euclid(self.length as f32)
                    } else {
                        cost + 0.5
                    };
                    for bridge_y in y.saturating_add(1).max(bounds.y)..next_y.min(bounds.bottom()) {
                        emit(
                            to,
                            bridge_y,
                            super::super::NORTH | super::super::SOUTH,
                            bridge_cost,
                            false,
                        )?;
                    }
                }
            }
            fill.push(fill::Span {
                y,
                left: fill_left,
                right: fill_right,
            });
            for &(from, to) in &row.gaps {
                let start = origin.checked_add(from.min(to))?.max(i64::from(bounds.x));
                let end = origin
                    .checked_add(from.max(to))?
                    .min(i64::from(bounds.right()) - 1);
                for x in start..=end {
                    emit(
                        x,
                        y,
                        super::super::EAST | super::super::WEST,
                        self.gap_cost(index, x - origin),
                        true,
                    )?;
                }
            }
        }
        drop(emit);
        let cells: Vec<_> = cells
            .into_iter()
            .map(|((x, y), (ports, cost, terminal))| RouteCell {
                x,
                y,
                symbol: super::super::scope_symbol(ports),
                cost,
                terminal,
            })
            .collect();
        if cells.is_empty() && fill.is_empty() && pair.0.0 != pair.1.0 {
            return None;
        }
        Some(Route {
            cells,
            fill,
            first,
            second,
            length: self.length as f32,
            exact: !masked,
        })
    }
}
